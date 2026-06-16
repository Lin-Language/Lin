# Investigation: Arena / bump allocation for program-lifetime RAPTOR data

**Branch:** `experiment/arena-explore` · **Status:** investigation + measured prototype, NOT merged.
**Date:** 2026-06-16

## TL;DR

An arena for program-lifetime data is **feasible, sound (in the right form), and a real
~15–18 % memory win (~3.5–4.2 GB of the 23 GB), but a ~0 % speed win** — and it is **NOT the
single biggest RAPTOR lever**. The biggest lever remains killing the **100× allocation
amplification** (265 M objects for 2.6 M logical records) via representation change
(`0xFE` inline / columnar). The arena removes the *per-object tax* (16 B malloc header); only
representation removes the *objects themselves* (Node holds the same data in 2–4 GB = a 6–10×
gap, far larger than the arena's 17 %). The two are complementary; representation comes first.

The cheap, available, sound subset of "arena benefits" — **eliminating RC churn + free cost on
program-lifetime data** — is **already shippable today via `frozen()`** with zero new machinery.
The arena adds, on top of `frozen`, only the malloc-header removal and locality.

---

## (1) What fraction of the 23 GB is program-lifetime?

**Nearly all of it.** The WAVE-M measurement (`docs/TODO.md` §182) reports 265 M live
allocations / ~21–23 GB **at peak**. Peak occurs during PREP/scan, *after* the streaming
loader has already freed its transients. RAPTOR's allocation profile is 32.9 GB *allocated*
total at **0.039 retention** — 96 % dies young and is already gone by peak. So the 23 GB that
*survives to peak* is, by definition, the durable index:

- `tripsByRoute` : RouteID → `Trip[]`  (240 K trips, each owning a `StopTime[]`)
- `routePath` / `routesAtStop` / `routeStopIndex` : the ~16 K-route topology
- `transfers` / `interchange` / `stops`
- the 2.37 M `StopTime` records reachable through the trips

These are built once in PREP (`raptor.lin::createRaptor` → `buildRoutes` → `indexRoute`) and
the loader's output (`gtfsLoader.lin`), and **never freed until exit**. The size-class
histogram (≤48 B = 89 M, ≤64 B = 85 M = sealed `StopTime`/`Trip` records) confirms the bulk is
the typed records themselves, not scaffolding. **Addressable surface for an arena ≈ the full
265 M objects.**

## (3) Malloc-header + RC-word overhead — the measured win

Measured glibc (default allocator) per-object RSS overhead by size class, matching the WAVE-M
live histogram (prototype `/tmp/arena-probe/probe4.rs`, keep-vec subtracted):

| requested | actual RSS/obj | overhead |
|-----------|----------------|----------|
| 16 B (TaggedVal box) | 32 B | **16 B** |
| 48 B (sealed record) | 64 B | **16 B** |
| 56 B | 64 B | 8 B (rounding only) |
| 64 B (sealed record) | 80 B | **16 B** |
| 128 B | 144 B | 16 B |
| 256 B | 272 B | 16 B |

glibc imposes an **8 B chunk header + 16 B-granularity rounding ⇒ ~16 B/object** for these
small sizes (the dominant classes). Applying to the WAVE-M live histogram:

| class | count | ×16 B header |
|-------|-------|--------------|
| ≤16 B | 38 M | 0.58 GB |
| ≤48 B | 89 M | 1.36 GB |
| ≤64 B | 85 M | 1.30 GB |
| ≤256 B | 51.7 M | 0.79 GB |
| **total** | **265 M** | **≈ 4.0 GB** |

**Memory win from header elimination ≈ 4.0 GB / 23 GB ≈ 17 %** (conservative band 3.2–4.2 GB
depending on how much is rounding vs header). A bump arena with 8 B alignment removes both the
header and the rounding waste.

The **RC word** (the u32 refcount embedded in every object) is *not* removed by an arena —
it's part of the object layout, the arena only changes where the bytes come from. Removing it
needs a separate arena-specialized layout (see §5, "stretch"). The **free cost** for this data
is already ~0 at runtime: it is never individually freed (program-lifetime), so there is
nothing to save.

### Speed win ≈ 0 (rigorously)

The `LIN_NO_RC` ceiling (`docs/PERFORMANCE.md` §5, Path 7) deletes the **entire** allocator + RC
subsystem as no-ops and measured **0.48 s vs 0.408 s = no speedup**; RAPTOR ~1.0× all phases.
An arena is a strict *subset* of that (it keeps the reads, kills only the alloc/header/free), so
it **cannot** beat the no-op ceiling. Steady-state glibc alloc+free is 1.1 ns/pair (tcache-hot,
`probe3.rs`) — negligible against RAPTOR's read-bound scan. **Treat the arena as a memory lever,
not a speed lever.**

## (4) Does `frozen` already give RC-elimination? Can an arena layer under it?

**Yes and yes.** `frozen()` (`crates/lin-runtime/src/frozen.rs`) deep-walks a transferable graph
and saturates every node's refcount to `IMMORTAL_RC`. The existing immortal guards in
`lin_rc_retain`/`*_release`/`lin_sealed_release` then make all retain/release **guarded no-ops**.
So **calling `frozen(raptorIndex)` after PREP already eliminates RC churn and free cost on the
durable index** — today, sound, zero new machinery. This is the "RC elimination for
program-lifetime graphs" item in TODO §282, and it's the cheap available subset of the arena.

`frozen` and an arena are **orthogonal and composable**:
- `frozen` = the **RC strategy** (immortal ⇒ no retain/release/free).
- arena = the **allocation strategy** (bump ⇒ no malloc header, contiguous, no free walk).

`frozen` leaves objects malloc-scattered with their 16 B headers (just never freed). An arena
additionally compacts them and drops the headers. The clean fusion (§5) is **"frozen that
allocates its copies into a bump region"**: one graph walk that bump-allocates immortal copies.

## (2) `region { … }` scope vs escape-inferred vs promote-copy

Three ways to route a value graph's allocations into the arena:

**(a) Ambient `region { … }` scope (thread-local current-arena).** A `region` block sets a TLS
"current arena" pointer; `lin_alloc`/`lin_sealed_alloc`/`lin_string_alloc`/`lin_map_alloc`/
`lin_array_alloc` check it and bump instead of `malloc`, stamping `IMMORTAL_RC` at alloc time
(so release is automatically a no-op via existing guards — no separate freeze walk needed).
*Minimal codegen surface (call sites unchanged).* **But fatal for RAPTOR as written:** wrapping
`loadGTFS()`+`createRaptor()` in a region would make the loader's **transient** parse
strings/line-arrays immortal and arena-resident — and 96 % of RAPTOR's allocation is transient
(32.9 GB allocated at 0.039 retention). The streaming loader exists *specifically* to free those
transients; a region defeats it and **blows memory up to ~tens of GB**. An ambient region is
only safe when the region's transient garbage is bounded — not true here.

**(b) Escape/lifetime-inferred arena.** Use `crates/lin-ir/src/escape.rs` to prove which
allocations are durable-and-non-escaping and bump-allocate only those. This is the deferred
Path-3 "inferred arenas (full)" (`docs/PERFORMANCE.md` §5) — **the borrow prototype returned
wrong values**, multi-week, high region-drop-UAF risk. Not recommended.

**(c) Promote-copy (`region.intern(graph)`) — RECOMMENDED.** Build the index normally (transients
die normally, streaming intact). Then deep-copy *only the final index graph* into the arena,
exactly like `frozen`'s descriptor walk but allocating immortal copies into bump space, returning
the arena root. Transients are unaffected; only the ~265 M durable objects land in the arena,
header-free and contiguous. **This is the only form that is both sound and avoids the
transient-retention blowup**, and it degrades gracefully to plain `frozen()` if the arena is
disabled.

## (5) Minimal design

### Runtime (`crates/lin-runtime/src/arena.rs`, new)

```rust
// A growable bump region: a linked list of large mmap/malloc'd chunks. Objects are never
// individually freed; the whole arena is dropped at once (or leaked at exit).
pub struct Arena { chunks: Vec<*mut u8>, cur: *mut u8, end: *mut u8 }

#[no_mangle] pub extern "C" fn lin_arena_new() -> *mut Arena;
#[no_mangle] pub extern "C" fn lin_arena_bump(a: *mut Arena, size: usize, align: usize) -> *mut u8;
#[no_mangle] pub extern "C" fn lin_arena_free(a: *mut Arena);   // drop all chunks at once
```

`lin_arena_bump` rounds `cur` up to `align`, returns it, advances; allocates a fresh
(geometrically larger) chunk when the request doesn't fit. ~30 lines.

### The promote walk (`lin_arena_intern`)

Mirror `frozen.rs::freeze_payload` / `freeze_sealed` / `freeze_array` / `freeze_map` exactly,
but instead of `*rc = IMMORTAL_RC` in place, **bump-allocate a copy into the arena, stamp its
refcount `IMMORTAL_RC`, and rewrite child pointers to the interned copies** (post-order, with a
`HashMap<old_ptr, new_ptr>` to preserve sharing/DAG structure). Because every interned node is
immortal, the existing `lin_*_release` guards make all subsequent release a no-op — no drop walk,
no free, no per-object RC. The descriptor machinery (`heap_desc`/`named_desc`) is reused verbatim
to know which fields are heap pointers to rewrite.

```
lin_arena_intern(arena, root_taggedval) -> root'   // deep immortal copy into the arena
```

### Surface in `.lin`

A stdlib intrinsic (e.g. `std/region`):

```lin
val idx = region(createRaptor(trips, transfers, interchange, date))
```

`region(v)` = `lin_arena_intern(<process arena>, v)`. Returns `v`'s type unchanged (like
`frozen`). For RAPTOR: one call site wrapping `createRaptor`'s result in `run.lin`/`bench.lin`.

### Why this is the minimal sound design
- **No codegen changes** (no per-site allocation routing, no new layout, no offset shifts).
- **Reuses the entire `frozen` descriptor walk + immortal-guard infrastructure** — the soundness
  argument is identical to `frozen`'s (immortal ⇒ never freed ⇒ no UAF, no data race on RC).
- **No transient blowup** (build runs normally; only the durable graph is interned).
- **Graceful fallback**: `region(v)` ≡ `frozen(v)` if the arena chunk is replaced by in-place
  sealing — so it can ship as "frozen, but compacted".

### Stretch (separate, higher-risk): arena-specialized header
Interned objects are always immortal and never individually freed, so the `refcount` (4 B),
`size` (4 B), and `heap_desc` (8 B) header words are dead for them — only `named_desc` (8 B, for
dynamic field reads) is needed. A 24 B → 8 B header would save another **~16 B/record**
(another ~3–4 GB), but requires the read path to recognize arena records (offset shift) =
the high-risk multi-site migration. **Out of scope for the minimal lever.**

## Risk

| Risk | Severity | Mitigation |
|------|----------|-----------|
| Ambient `region {}` retains transients → memory blowup | **Fatal** | Use promote-copy (c), never ambient scope (a) for RAPTOR. |
| Interned graph must be acyclic/transferable | Medium | Same constraint as `frozen`/`shared`; enforced identically. Use the `old→new` map to handle DAG sharing; reject/handle cycles like `frozen` does. |
| Cross-thread reads of arena data | Low | Immortal + read-only = race-free by the same argument `frozen` already relies on (frozen.rs docstring). |
| Mutation after interning | Medium | Interned copy is immortal/read-only; the index is read-only after PREP in RAPTOR, so this matches usage. Mutating an interned value is a logic error (document like `frozen`). |
| Arena never returned to OS | Low | It IS the 23 GB you wanted resident anyway; one `lin_arena_free` at/near exit (or just leak — process teardown reclaims). |
| Escape-inferred variant returns wrong values | High | Don't build (b); the borrow prototype already failed (Path-3). |

## Verdict & recommended sequencing

1. **Ship `frozen(raptorIndex)` first** (zero new code) — captures the RC-churn + free-cost
   subset, sound today. Measure RSS/wall-clock; this is the free baseline.
2. **Then the promote-copy arena** (`lin_arena_*` + `lin_arena_intern`, ~150 lines, reuses the
   `frozen` walk) for the **~17 % / ~4 GB** header-elimination win. Low risk because it's
   structurally `frozen` + bump.
3. **Representation (`0xFE` inline / columnar) remains the dominant lever** — it removes the
   objects, not just their headers, and is what closes the 6–10× gap to Node. Arena is
   *complementary* memory polish, not the headline.

Bottom line: the prompt's hypothesis that this is "the single biggest RAPTOR lever" is **not
borne out** — it's a solid ~17 % memory win with ~0 % speed (the `LIN_NO_RC` ceiling forbids a
speed win), and `frozen` already delivers its cheap half for free.
