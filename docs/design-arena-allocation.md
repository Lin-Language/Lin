# Arena / Region Allocation for Program-Lifetime Data

> Design investigation — read-only, no code changes. Every claim is grounded in a
> specific file:line. Numbers are from the WAVE M attribution measurements cited in
> `docs/TODO.md` and `docs/PERFORMANCE.md §5`.

---

## 1. How allocation works today

### 1.1 LinMap

`crates/lin-runtime/src/map.rs`

Every `lin_map_alloc` (`map.rs:341`) performs **three separate `std::alloc::alloc` calls**:

| allocation | size | notes |
|---|---|---|
| LinMap header | 32 B (`size_of::<LinMap>()`) | `map_header_layout()` at `map.rs:128` |
| slot table | `cap × 32 B` (hash u64 + key u64 + TaggedVal 16 B) | `alloc_slots` / `alloc_zeroed` at `map.rs:143` |
| order array | `cap × 8 B` (`*mut u64` insertion-order list) | `alloc_order` at `map.rs:155` |

At `INITIAL_CAP = 4` (lowered from 8 in `c8119174`): header 32 B + slots 128 B + order 32 B = **192 B
minimum** — plus glibc's per-allocation overhead (~16 B each × 3 = ~48 B hidden overhead).

`lin_map_release` (`map.rs:593`) calls `dealloc` three times (slots, order, header) and recurses
into `release_tagged_payload_pub` for every occupied value slot.

RC lives at `LinMap.refcount: u32` (offset 0). `lin_map_retain`/`lin_map_release` gate on
`refcount >= IMMORTAL_RC` (`map.rs:630`, `map.rs:597`) — the immortal sentinel.

**RAPTOR peak (TODO.md WAVE M):** 51.5M live LinMaps = **15.25 GB (76% of peak RSS)**.

### 1.2 Sealed records

`crates/lin-runtime/src/sealed.rs`

`lin_sealed_alloc` (`sealed.rs:103`) performs **one zeroed `alloc`** of `size` bytes (24 B header +
packed field payload), then writes:
- refcount 1 at offset 0 (`u32`)
- total byte-size at offset 4 (`u32`) — needed for `lin_sealed_release_self` (`sealed.rs:272`)
- heap-field descriptor pointer at offset 8 (`*const u8`) — drives RC drop/retain walks
- named-field descriptor pointer at offset 16 (`*const u8`) — drives dynamic access by name

`lin_sealed_release` (`sealed.rs:168`) walks the heap-field descriptor via `release_heap_fields`
(`sealed.rs:144`) and calls `dealloc` with `Layout::from_size_align_unchecked(size, 8)`.

RC lives at `*(ptr as *mut u32)` (offset 0). The immortal guard (`*rc >= IMMORTAL_RC → return`)
appears at `sealed.rs:186`, mirroring the string/array/map guards.

**RAPTOR peak:** 69.8M live sealed records = **4.47 GB** (sealed Trip + StopTime structs).

### 1.3 LinArray

`crates/lin-runtime/src/array.rs`

`lin_array_alloc` (`array.rs:103`): two allocs — header (`array_layout()`) plus data buffer
(`array_elem_layout(cap)`). Flat scalar arrays use the element's natural width; tagged arrays use
16 B elements.

### 1.4 LinString

`crates/lin-runtime/src/string.rs`

`lin_string_alloc` (behind `lin_string_from_bytes`): one alloc, variable size (`size_of::<LinString>()
+ len` bytes). String **literals** are immortal via `lin_string_literal` (`string.rs:80`): the first
call allocates once with `refcount = IMMORTAL_RC` and caches in a `thread_local! RefCell<HashMap>`.
Literal strings are individually malloc'd but RC-immortal and never freed — a precursor to the
arena concept.

### 1.5 RC discipline summary

Every heap object carries a `u32 refcount` at byte offset 0. Retain/release is emitted by the
lowerer (`lin-ir/src/lower/`) and partially elided by `rc_elide.rs`. On the hot RAPTOR query path,
all index objects are live for the entire program run — so every `Retain`/`Release` touching index
data is pure overhead. `LIN_NO_RC` (the `docs/PERFORMANCE.md §5` closed-negative experiment)
confirmed that deleting RC entirely recovers ≈0% on RAPTOR because RC is not on the critical path
for RAPTOR's wall-clock, but the 265M live objects do carry 265M × 4 B = ~1 GB of refcount fields,
and the `lin_map_alloc` burst of 3 `malloc` calls × 51.5M maps = 154.5M `malloc` calls is a real
allocation-path cost.

---

## 2. What `frozen()` does and does NOT do

`crates/lin-runtime/src/frozen.rs`

`lin_freeze` (`frozen.rs:125`) implements the ADR-030 deep immortal seal:
1. Walks the value graph recursively via `freeze_payload` (`frozen.rs:65`).
2. Sets every node's `refcount = IMMORTAL_RC` (0x8000_0000).
3. All subsequent `Retain`/`Release` on those nodes become **guarded no-ops** (the `>= IMMORTAL_RC`
   check in every `*_release` / `lin_rc_retain`).

**What `frozen` IS:** a way to make a graph permanently live with zero RC overhead from that
point on, safe to share across threads without atomic RC (`frozen.rs:1-13` rationale).

**What `frozen` is NOT:**
- It does NOT change how memory was allocated. Each node was individually malloc'd and retains its
  glibc per-object header and alignment padding.
- It does NOT consolidate the objects into a contiguous block. The 51.5M maps + 69.8M sealed records
  remain scattered across glibc arenas with pointer-chasing access patterns.
- It does NOT reclaim the memory. Frozen data lives until process exit (`frozen.rs:14`: "Cost: a
  frozen graph is **never freed**"). The OS reclaims the pages at process exit.
- It does NOT change allocation speed. All allocs during construction still go through `malloc`,
  even if the caller will immediately `freeze` the result.

`frozen` is a **partial arena** in one narrow sense: its lifetime contract ("never freed, load-once,
program-lifetime data") is exactly the arena contract. But it delivers only the RC-suppression half.
The allocation-efficiency, locality, and header-elimination halves are absent.

The `TODO.md WAVE R` note — "arena ~17%/4GB (frozen() free half)" — means that if we bumped-arena
the frozen objects we could recover roughly half the overhead (the 4GB of sealed records are the
candidate; the maps are at 15GB but their internal structure is more complex).

---

## 3. Escape / region analysis: what exists and its limits

### 3.1 The current escape pass (`lin-ir/src/escape.rs`)

`escape::analyze` (`escape.rs:77`) runs per-function, over the flat LinIR of one function. It:
- Finds `MakeObject` instructions constructing **all-scalar** sealed records with no spreads.
- Builds union-find carry classes over representation-preserving edges (Copy, Phi, Bind, no-op
  Coerce, self-TailCall args).
- Marks a carry class as **escaping** if any member reaches a `Return`, container store, closure
  capture, or any unknown consumer.
- Marks non-escaping candidates with `stack = true`, enabling the alloca-based stack allocation
  in codegen and RC suppression.

**Scope limit:** the analysis is strictly **intra-function**, over one `LinFunction`. It has no
concept of "escapes to a program-lifetime binding" vs "escapes to a per-query-scoped binding". A
value that flows into the return value of a function escapes at `escape.rs:135` (`Return(t) =>
mark(escaping, t)`) — from the pass's perspective, it is gone and the frame cannot track it further.

**What it cannot infer:**
- Cross-function program-lifetime analysis: "these records are constructed in `buildIndex`, returned
  into a `val index = buildIndex(...)` that lives until process exit" — this requires a whole-program
  or interprocedural analysis over the call graph that does not exist.
- A value that escapes one function's frame via `Return` is conservatively treated as possibly
  short-lived — the analysis has no way to distinguish "escapes to program lifetime" from "escapes
  to a per-iteration loop variable".
- The `frozen` call site is invisible to the escape pass; it operates on LinIR before codegen and
  `frozen` is a stdlib function call that the pass treats as an ordinary `Call` (interference).

### 3.2 The RC-elide pass (`lin-ir/src/rc_elide.rs`)

`elide_rc` (`rc_elide.rs:59`) eliminates balanced Retain/Release pairs within a function using
post-dominator-chain walking and a liveness gate. It can elide cross-block pairs when the Release
post-dominates the Retain and the path is clean.

This pass is even more narrowly scoped than escape analysis: it operates on IR-level temp variables,
not on the runtime memory of the objects they point to. It cannot declare an object "program-
lifetime" and exempt it from all future RC.

### 3.3 Could escape.rs be extended to infer a region?

A "region inference" extension would need to determine that a value graph built in some function
escapes only to a binding that is (a) at module-global scope, or (b) captured only by program-
lifetime closures, or (c) explicitly annotated. The analysis would need:
1. An interprocedural call-graph walk or whole-module analysis pass.
2. A representation of "escapes to program-lifetime" as a distinct outcome (today the pass only
   distinguishes stack-resident vs heap-resident, not short-lifetime-heap vs program-lifetime-heap).
3. A way to communicate the decision to the allocator at codegen time (a new `MakeObject { region:
   Option<RegionId>, .. }` field, or a separate lowering path).

The escape.rs analysis IS the right structural foundation (union-find, carry classes, per-consumer
classification) and could be extended to track region membership. But such an extension is
non-trivial and can only safely handle the "explicit annotation" case for a first slice — automated
inference for program-lifetime graphs would need the interprocedural analysis noted above.

---

## 4. Design options

### 4.A Explicit user-facing `region {}` / arena scope

**Mechanism:** A new keyword (or stdlib wrapper) that marks a construction block as region-scoped.
All allocations inside the block go into a bump arena associated with the block's lifetime. At the
end of the block, the arena is either:
- (i) freed all-at-once (scope-exit semantics: all allocated values die together), or
- (ii) promoted to program-lifetime (never freed: equivalent to `frozen` but for the allocation
  strategy).

Variant (ii) is the relevant one for RAPTOR's PREP phase.

Example user code:
```lin
import std/arena

val index = arena.region {  // or a first-class `region {}` keyword
    buildRaptorIndex(rawData)  // allocations here → bump arena, zero per-object RC
}
// index now lives until exit; arena is marked immortal
```

**Runtime changes needed:**
- New `crates/lin-runtime/src/arena.rs`: bump arena with page-list backing, thread-local current-
  arena slot.
- Hook into `lin_sealed_alloc` / `lin_map_alloc` / `lin_array_alloc` / `lin_string_alloc`: check
  the thread-local arena; if active, bump-allocate from it instead of `malloc`. RC field is written
  as `IMMORTAL_RC` immediately (zero ongoing RC cost).
- New runtime functions: `lin_arena_enter()` / `lin_arena_exit_immortal()` (variant ii) or
  `lin_arena_exit_free()` (variant i).
- Codegen or stdlib wraps the body of `region {}` with `lin_arena_enter` / `lin_arena_exit_*`.

**Estimated win (RAPTOR):**
- 51.5M LinMap allocs × (3 malloc calls saved × ~50 ns + allocation locality win) → several
  seconds of PREP wall-clock.
- 69.8M sealed record allocs × ~50 ns → ~3.5 s PREP wall-clock saved.
- RSS: glibc per-object overhead ~16 B × 265M = ~4.2 GB recovered. Fragmentation reduction.
  Measured mimalloc delta was ~10% (~2.5 GB); a proper arena would beat that by 2–4×.
- RC: all operations on arena-allocated data become no-ops immediately (zero ongoing retain/release
  cost on the 20 GB live index for all RANGE/GROUP queries).

**Estimated combined win:** 4–8 GB RSS, 2–5 s PREP wall-clock, measurable RANGE/GROUP speedup
from eliminated RC traffic. The word "potentially the single biggest RAPTOR memory+speed lever"
in TODO.md appears well-founded.

**Risks:**
- UAF surface (§5) — the dominant risk.
- Spec change: `region` / `arena` is a new language-level concept; ADR needed.
- Hook complexity: touching four hot allocators (map, sealed, array, string) is a lot of
  surface; each must correctly fall back to normal malloc when no arena is active.
- Thread-safety: the thread-local arena slot is safe per-thread, but a `region` passed across
  threads (e.g. RAPTOR building the index in a worker) requires the arena to be worker-local
  and transferred to the main thread — which requires deep-copy semantics (the existing transfer
  mechanism) or reference transfer with ownership.

**Tradeoffs:** Maximum memory and speed win; highest implementation and spec cost; permanent UAF
surface once the feature exists.

### 4.B Escape-inferred arena for PREP-built indexes

**Mechanism:** The compiler automatically detects that a value graph built in a function escapes
to a program-lifetime binding and emits arena-allocation code without user annotation.

**Feasibility:** Very low for a first slice. The escape pass is intra-function (§3.1); recognizing
"program-lifetime escape" requires interprocedural analysis across the call graph. The `frozen()`
call site is not even seen by the IR escape pass. Even if the analysis existed, the boundary
between PREP (build) and RANGE (query) is a runtime phase distinction, not a structural static
property the compiler can detect without user annotation.

A narrower inference is feasible: if a `MakeObject` result flows into a `Call { callee:
"lin_freeze", .. }` directly in the same block (a pattern the IR expresses), the escape pass could
mark it as "arena-eligible". But this does not cover the general case (the call chain may be
`buildIndex → buildRoute → buildMap → lin_map_alloc`; `frozen` is called on the top-level result
only).

**Verdict:** Not a sound first slice. Deferred to after explicit `region {}` is proved out.
Potentially useful as a "region-inference from explicit annotation" layer later (i.e., the
compiler infers which sub-allocations inside a user-annotated region block belong to the arena).

### 4.C Generalize `frozen` to also bump-allocate (the "copy-into-arena" approach)

**Mechanism:** When `lin_freeze(v)` is called on an already-built value graph, ALSO relocate the
entire graph into a bump arena: walk every node, copy it into arena storage, update all internal
pointers, free the originals.

**Problems:**
- This is a **copying garbage collection pass** over a live heap. It requires walking every
  pointer in every node, which needs the same descriptor infrastructure as `freeze_sealed` /
  `freeze_array` / `freeze_map` (already present in `frozen.rs`) — but also writing new values
  back and patching pointers.
- `lin_map_get` returns a **borrowed interior pointer** (`*const TaggedVal` into the slot array;
  `map.rs:386`). If the map is moved, that pointer is invalidated. Any live borrowed pointer to
  the pre-copy location would be a UAF. At the `lin_freeze` call site the pointer is generally
  "just constructed" with no outstanding borrows — but the runtime has no way to verify this.
- `lin_freeze` is called from user Lin code; there is no guarantee the graph is not simultaneously
  pointed to by a live stack variable whose address we cannot patch.
- Complexity: the relocation logic would be as complex as a full copying GC. This is not a simple
  change.

**Verdict:** Too complex and too risky for a first (or second) slice. The clean version of this
idea is option A (allocate in the arena from the start), not option C (move after the fact).

### 4.D Hybrid: `frozen`-aware arena, no language change

**Mechanism:** A middle ground between A and C that avoids a new keyword: a new stdlib function
`arena.build { ... }` (not a new keyword, just a function call) that activates a thread-local
bump arena for the duration of its body, then seals it immortal and returns the result. This is
option A variant (ii) implemented through the stdlib, with no compiler support needed for the
first slice — the arena is always active; there is no static region analysis; UAF is the user's
responsibility (same as `frozen`).

This is the most pragmatic path to a first proof-of-concept.

---

## 5. UAF / soundness surface

The arena UAF surface is the hardest part. It falls into two categories:

### 5.1 Variant (i) — scope-exit arena (freed at scope end)

If the arena is freed at the end of the `region {}` block, any reference that escapes the block
is a UAF:
- A returned value from `region {}` that is stored in a longer-lived binding → UAF after the
  region is freed.
- A closure that captures a region-allocated string and outlives the region → UAF.
- A `Shared<T>` holding a region-allocated value → UAF when a worker thread outlives the region.

**Mitigation:**
- Static checker: region-allocated values have a "region type" that the checker marks as non-
  transferable out of the region scope (similar to lifetime analysis in Rust or the `Stream<T>`
  affine rule in ADR-049).
- But this is non-trivial to implement in `lin-check` and has not been done.
- For a sound first slice: **forbid variant (i) entirely**. Only implement variant (ii) (program-
  lifetime / never-freed arenas). A program-lifetime arena has no scope exit and therefore no
  UAF surface from region-drop.

### 5.2 Variant (ii) — program-lifetime arena (never freed)

If the arena is never freed (leaked to the OS), there is **no region-drop UAF**: the objects
live forever, just like `frozen` objects today. The only UAF risk is the same one `frozen` has:
a frozen/immortal object that is mutated after being shared (Lin's type system prevents this for
non-`var` bindings; `var` objects should not be `frozen`, same as today's rule).

**This is safe by construction for program-lifetime arenas.** The rule is the same as
`frozen`'s existing contract (`frozen.rs:14`): "Cost: a frozen graph is **never freed** —
`frozen` is for load-once, program-lifetime data."

An arena used this way is strictly more conservative than `frozen` (never-freed AND bump-allocated),
not less.

### 5.3 The `lin_map_get` borrowed-interior-pointer ABI concern

`lin_map_get` (`map.rs:386`) returns `*const TaggedVal` — a borrowed pointer INTO the map's slot
array. If the slot array is in a bump arena and the arena is reallocated (its page list grows),
the slot array moves → the borrowed pointer is stale.

**Mitigation for bump arenas:** A bump arena must NOT relocate existing allocations. Each page in
the page-list is a fixed contiguous block; once a slot array is written into a page, it stays at
that address for the arena's lifetime. This is the standard property of a bump allocator (vs a
compacting arena), and it preserves the borrowed-pointer ABI.

The `TODO.md` note — "feasibility proven: no codegen consumer holds two live borrowed results
across a second get → a thread-local scratch return is safe" — applies to the value-unbox lane
(Lane V) and is independent of arenas. An arena-backed slot array at a fixed address is fully
compatible with the existing borrowed-pointer ABI.

### 5.4 Mixed-arena / normal-heap graphs

If a RAPTOR query (RANGE/GROUP) allocates a `Trip | Null` result that POINTS INTO the
arena-backed index (e.g., holds a borrowed `*const Trip`), but the result itself is heap-allocated
(not in the arena), the result's `Release` must not decrement the arena-backed `Trip`'s refcount —
because it is `IMMORTAL_RC` (the arena objects have `IMMORTAL_RC` from construction). The
existing immortal guard in `lin_sealed_release` (`sealed.rs:186`) already handles this correctly:
`if *rc >= IMMORTAL_RC { return; }`. **No special-casing needed for mixed graphs** — the existing
IMMORTAL_RC discipline already makes arena-held (immortal) objects safe to point to from normal-
heap (mortal) objects.

---

## 6. Recommended first slice

### Summary

**Implement a never-freed thread-local bump arena for sealed-record allocations in `sealed.rs`,
activated by a new `lin_arena_enter` / `lin_arena_exit` runtime API, callable from a new
`crates/lin-runtime/src/arena.rs` module. Expose it to Lin code via a thin `std/arena` wrapper.
Start with sealed records only (4.47 GB, no map.rs conflict); validate the approach before
extending to LinMap.**

This is sound (variant-ii only, never freed), does not require a new language keyword, and is
file-disjoint from all currently-active lanes.

---

### 6.1 File footprint

| File | Change | Notes |
|------|--------|-------|
| `crates/lin-runtime/src/arena.rs` | **NEW** | Bump arena implementation |
| `crates/lin-runtime/src/sealed.rs` | Modify `lin_sealed_alloc` | Check thread-local arena |
| `crates/lin-runtime/src/lib.rs` | `pub mod arena;` | Expose the new module |
| `stdlib/arena.lin` | **NEW** | Thin Lin wrapper for `lin_arena_enter`/`lin_arena_exit` |

Not touched: `map.rs` (Lane V), `tagged.rs` (Lane S), `lin-ir/{repr,escape}.rs` (Lane F),
`codegen/{types,boxing,literals,arith,data/array,intrinsics}.rs` (all active lanes).
`array.rs` is not touched (sealed records only in the first slice; `lin_sealed_array_alloc` in
`array.rs` would be a subsequent extension once the arena pattern is validated).

**Unavoidable overlap with Lane F:** Lane F owns `sealed.rs` and `array.rs` for the `0xFE`
inline-record migration. The change to `lin_sealed_alloc` in `sealed.rs` is a small, localized
hook (a thread-local arena check before the existing `alloc` call). It is mechanically disjoint
from the 0xFE work (which changes the sealed-array construction paths, not `lin_sealed_alloc`
itself), but coordination with the Lane F agent is required to avoid a merge conflict on `sealed.rs`.

---

### 6.2 `arena.rs` design

```
// arena.rs — bump arena for program-lifetime (never-freed) allocations.
//
// A page-list allocator: each page is a fixed-size contiguous block (default 4 MB).
// Allocation = bump the offset pointer within the current page; if it overflows, add
// a new page. Pages are never reallocated, so interior pointers remain stable —
// preserving the lin_map_get borrowed-pointer ABI (§5.3).
//
// Thread-local: one arena per thread. Not Sync; not shared across threads.
// Program-lifetime: arena pages are leaked (never freed individually); the OS reclaims at exit.
// All arena-allocated objects are written with IMMORTAL_RC at construction.

struct Arena { pages: Vec<Box<[u8]>>, offset: usize }
const PAGE_SIZE: usize = 4 * 1024 * 1024; // 4 MB

thread_local! {
    static CURRENT_ARENA: RefCell<Option<Arena>> = RefCell::new(None);
}

// lin_arena_enter: activate a fresh bump arena on this thread.
// lin_arena_exit_immortal: seal + keep the arena alive forever; return the arena pointer
//   (for diagnostic use; the arena is owned by the thread-local and never dropped).
// lin_arena_alloc(size, align): allocate from the active arena, or null if no arena active.
//   Called by lin_sealed_alloc as a fast-path check before falling back to malloc.
```

The `lin_sealed_alloc` change is a **four-line patch**:

```rust
// sealed.rs:103  lin_sealed_alloc(size, heap_desc, named_desc)
pub extern "C" fn lin_sealed_alloc(size: usize, heap_desc: *const u8, named_desc: *const u8) -> *mut u8 {
    let size = size.max(SEALED_HEADER);
    // ── arena fast-path ──────────────────────────────────────────────────────
    if let Some(ptr) = crate::arena::arena_alloc(size, 8) {  // NEW: 4 lines
        unsafe { std::ptr::write_bytes(ptr, 0, size); }
        let words = ptr as *mut u32;
        unsafe { *words = IMMORTAL_RC; *words.add(1) = size as u32; }  // immortal immediately
        // write heap_desc + named_desc same as before...
        return ptr;
    }
    // ── existing malloc path (unchanged) ─────────────────────────────────────
    unsafe { ... }  // existing body
}
```

### 6.3 Estimated win

| metric | before | after (first slice, sealed only) | notes |
|--------|--------|----------------------------------|-------|
| RAPTOR PREP sealed alloc | 69.8M × `malloc` + RC init | 69.8M × bump pointer | ~3.5 GB/s × ~50ns/alloc → ~1.7 s PREP saved |
| RAPTOR peak RSS (sealed) | 4.47 GB live + ~1.1 GB glibc overhead | ~4.47 GB live, ~0 overhead | glibc per-obj ~16 B × 70M |
| RANGE/GROUP RC traffic | retain/release per Trip/StopTime read | zero (IMMORTAL_RC) | measured total RC eliminated |
| PREP wall-clock | ~104 s typed | estimated −1–3 s | 50 ns × 70M allocs = 3.5 s ceiling |
| peak RSS | ~23 GB | estimated −1–2 GB (sealed only) | maps remain normal |

Extending the arena to `lin_map_alloc` (after the Lane V map-value-unbox work lands) would
multiply all wins by ~4× (maps are 4× larger than sealed records at peak).

### 6.4 Risk assessment

| risk | severity | mitigation |
|------|----------|------------|
| UAF from arena-drop | NONE (program-lifetime arena, never freed) | Variant-ii only: no `arena_free`; pages leak to OS |
| Borrowed interior pointer invalidation | NONE | Bump allocator: pages never move; existing immortal-RC guard prevents double-free |
| `sealed.rs` merge conflict with Lane F | LOW | Small localized patch at function entry; coordinate with Lane F |
| Arena growth to >4 GB: multiple pages | LOW | Page-list design handles it; each page is a separate Box<[u8]> |
| Thread-local arena vs multi-threaded RAPTOR builds | MEDIUM | Each thread has its own arena; inter-thread references are safe (IMMORTAL_RC). A `region {}` that spawns workers needs the build phase to stay on one thread (same as `frozen`'s contract) |
| Wrong-result if arena objects are mutated | NONE for first slice | Lin types + IMMORTAL_RC: a mutation on an immortal sealed record via `lin_sealed_release` is a no-op; typed records don't expose mutable fields through the `AnyVal` path once sealed |

### 6.5 Recommended sequence

1. **Spike** (`arena.rs` + `lin_sealed_alloc` hook, 3 files): implement the arena, wire
   `lin_sealed_alloc` to it. Test with a hardcoded `lin_arena_enter()` call before RAPTOR PREP.
   Gate: `cargo test -p lin` + RAPTOR digest exact (both variants). Measure RSS + PREP wall-clock.

2. **`std/arena` wrapper**: expose `arena.enter()` / `arena.build { ... }` to Lin code as a
   stdlib module. Add a `.test.lin` test suite (`stdlib/arena.test.lin`).

3. **Gate on digest + RSS delta**: if PREP RSS drops ≥1 GB and wall-clock improves without
   digest regression, merge. The risk profile is the same as shipping `frozen()` — it is strictly
   a specialisation of the existing immortal-RC discipline.

4. **Extend to `lin_map_alloc`** (requires coordination with Lane V after it merges). This
   targets the 15.25 GB (76%) lever and is the high-impact second step.

---

## Appendix: file locations for key cited code

| symbol | file | line |
|--------|------|------|
| `lin_map_alloc` | `crates/lin-runtime/src/map.rs` | 341 |
| `lin_map_release` | `crates/lin-runtime/src/map.rs` | 593 |
| `INITIAL_CAP` | `crates/lin-runtime/src/map.rs` | 122 |
| `lin_sealed_alloc` | `crates/lin-runtime/src/sealed.rs` | 103 |
| `lin_sealed_release` | `crates/lin-runtime/src/sealed.rs` | 168 |
| `SEALED_HEADER` | `crates/lin-runtime/src/sealed.rs` | 63 |
| immortal guard in `lin_sealed_release` | `crates/lin-runtime/src/sealed.rs` | 186 |
| `lin_freeze` | `crates/lin-runtime/src/frozen.rs` | 125 |
| `freeze_sealed` | `crates/lin-runtime/src/frozen.rs` | 86 |
| `lin_string_literal` (immortal interning) | `crates/lin-runtime/src/string.rs` | 80 |
| `IMMORTAL_RC` | `crates/lin-runtime/src/string.rs` | 35 |
| `lin_rc_retain` | `crates/lin-runtime/src/memory.rs` | 163 |
| `escape::analyze` | `crates/lin-ir/src/escape.rs` | 77 |
| `is_stack_eligible_type` | `crates/lin-ir/src/escape.rs` | 90 |
| `elide_rc` | `crates/lin-ir/src/rc_elide.rs` | 59 |
| RAPTOR peak attribution | `docs/TODO.md` | WAVE M / WAVE R |
| LIN_NO_RC closed-negative | `docs/PERFORMANCE.md` | §5 (Path 7) |
| Arena "single biggest lever" | `docs/TODO.md` | ADDITIONAL AREAS |
