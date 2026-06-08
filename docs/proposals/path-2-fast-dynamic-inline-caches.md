# Path 2 — Make the dynamic representation fast (inline caches / hidden classes)

**Status:** Open proposal, one of five independent paths. Self-contained.
**Direction in one line:** instead of escaping `Json`/objects onto a packed type, make the *single
existing* object representation fast via hidden classes + inline caches — the way V8/SpiderMonkey/LuaJIT
give dynamic objects struct-speed field access — so there is no packed type, no gate, and no boxing
boundary at all.

---

## Background (shared context — the problem, the framing, the full history)

### The problem
Reading a field of a known record type — `point["x"]`, `trip["stopTimes"]`, `token["kind"]` — and
operating over arrays of such records (`length`, `for`, `map`, `filter`) is dramatically slower in Lin
than in Go/Rust/Zig/Nim, where these are constant-offset loads and const-stride walks.

### The framing correction (the root misconception)
**Lin's type system is not JSON.** It is syntactically JSON-like and shares JSON's primitives, but a
named `type` is a known, closed shape — not a dynamic bag. The conflation of "looks like JSON" with "is
represented like dynamic JSON" is the root of the performance problem. *(This path takes a different
lesson from the framing than the others: rather than make the type system stop being JSON-like, it makes
the JSON-like representation fast — see thesis.)*

### How Lin represents values today
- **Boxed (default, dynamic):** a record is a heap `LinObject` — refcounted, string-keyed, hash-indexed
  when large. `obj["k"]` is a non-inlinable `lin_object_get` (intern-pointer compare + scan/probe + box
  result as a `TaggedVal`), **opaque to LLVM**. This is the representation of `Json`, anonymous/inferred
  literals, structurally-subtyped params, **and every value flowing through a polymorphic stdlib op**.
- **Packed / "sealed" (fast, opt-in):** a named `type T` as a packed struct (const-offset fields); an
  array of them a header-less contiguous `0xFE` buffer with a per-field RC descriptor. A scalar packed
  field read is `getelementptr + load`, verified.
- **Flat scalar arrays:** `Int32[]` already contiguous + specialized (`lin_flat_array_*`).
- Machinery: a `Repr` lattice + oracle/verifier (ADR-062); the gate
  `Type::is_sealed_array_field_packable`, currently scalar+Bool only.

### The three costs
1. **Field reads through the dynamic ABI** — ~72× (typed vs `Json`); fixable, now largely solved by a
   spike for the packed case.
2. **Operations at the boxing boundary** — the dominant, unanticipated cost: `length(packed Token[])`
   emits `lin_sealed_array_to_tagged`, materializing the whole array to boxed `Object[]` to read a `u64`
   count; same for all combinators — they re-box a packed array on entry.
3. **Construction refcounting** — per-element-per-field retain on build + drop-walk on free.

### The full history (what was tried, learned, failed)
- **H1 — Profile (valid):** typed vs `Json` field read ~72×; LLVM elided a dead typed object. Measured
  *already-packed* reads → looked like reads were the whole story.
- **H2 — Leaks drained (independent win):** RAPTOR ~190 MB/scan → ~97% reduced; bench completes.
- **H3 — Sealed machinery + harness built (sound):** per-field RC, descriptors, keep-packed ops,
  mechanism (i) materialize-on-read, 3-point ASan harness (found a `sort` leak manual probing missed).
- **H4 — Gate widenings net-negative:** scalar→String→Array→Map→nested each found+fixed a real bug
  (silent data loss, a compiler panic, a broad leak, two crashes, missing KIND_MAP) but packing heap
  fields **regressed interp ~3×, crashed the TLV codec, helped RAPTOR nothing**. Gate narrowed back to
  scalar+Bool; plumbing dormant.
- **H5 — RAPTOR retype: correct, >5× regression** (killed ~45 min vs ~510 s); sub-blockers `get<T,D>`
  monomorphization + `Trip|Null`/`Conn` re-boxing.
- **H6 — The pivotal spike:** cheap packed heap-field reads (sound, 1.7× isolated) recovered **only ~6%**
  of interp's regression; IR showed `length`/combinators materialize the whole array on entry.
  Reads were *not* the bottleneck.
- **H7 — Ruled out:** boxed inline-slot (unsound); shape-ratio gate (3.6× blind spot); cheap-reads-alone
  (~6%); round-key churn (neutral); NaN-box/slab/GC/box-pool (prior negatives).

### The central finding
The bottleneck is that the packed representation is **not integrated with the runtime's polymorphic
operations**. Every "make the packed type win" path must integrate the verbs (Path 1) or change the type
model (Paths 3/4). **This path questions the premise**: why have a packed type at all if you can make the
*one* representation fast?

---

## This path's thesis

Every other "make it fast" path introduces or privileges a **second, packed representation** and then
fights the consequences (a gate, a boxing boundary, a packed/boxed-mismatch bug class — the entire
§H4/H5 saga). Path 2 takes the opposite approach, the one the fastest *dynamic* languages take: keep a
**single uniform object representation** and make field access on it fast via **hidden classes (shapes)
+ inline caches**.

- Every `LinObject` carries a **shape id** — an interned descriptor of its field set + layout. Objects
  constructed the same way share a shape.
- A field-access site holds an **inline cache**: the last shape seen + the offset it resolved to. A hit
  is `load shape; cmp cached; br; load field at cached_offset` — a handful of instructions, visible to
  LLVM. A miss falls to `lin_object_get` and updates the cache.
- A **monomorphic** site — the common hot-loop case, reading `token["kind"]` over uniformly-shaped
  tokens — caches once and then runs at const-offset speed *on the dynamic representation*.
- `length`/`map`/`for` need **no packed ABI**: `length` is an array-header field; iteration reads
  uniform-shaped elements whose per-field reads inline-cache. **There is no representation to convert to,
  so nothing re-boxes** — cost #2 is dissolved by construction.

## What this path fixes

- **Field reads:** yes, for monomorphic / polymorphic-stable sites (the overwhelming majority of hot
  loops) — the dynamic read becomes a guarded const-offset load.
- **Combinator/`length` boundary (cost #2):** **dissolved** — there is no second representation to
  materialize to/from.
- **Construction RC (cost #3):** no (objects stay heap + refcounted; compose with Path 3 if needed).
- **Bonus no other path gives:** it speeds up genuinely-`Json` code too (untyped wire data with stable
  shapes) — the packed paths only help statically-known types.

## Rationale / why pursue this path

- **It removes the entire packed/boxed-mismatch bug class** — the one that consumed this session
  (§H4/H5: silent data loss, panics, crashes, UAFs all from packed-vs-boxed representation disagreement).
  There is no second representation, so there is no mismatch. That is a large risk *removed*, not added.
- **No userland language change.** No `struct` keyword (Path 4), no `packed` annotation, no
  default-inversion (Path 1's risky sub-variant), no semantics change (Path 3). Existing object/`Json`
  code just gets faster.
- **It is the proven answer** for "dynamic-shaped values with fast field access" — every major JS engine.
- It makes the framing true *in practice* without changing the language: a known-shape object's field
  read *is* a const-offset load after the cache warms.

## Cons / risks

- **Designed for JITs, not AOT/LLVM.** Inline caches are normally mutable, runtime-warmed,
  self-modifying. In an AOT model the cache is a static per-site slot updated at runtime — workable
  (load+cmp+branch + a mutable global), but a **novel pattern for this backend; least-charted, highest
  unknown** of all paths.
- **Guarded, not guaranteed.** A hit is fast; a *megamorphic* site (many shapes) degrades to the slow
  path and the guard is pure overhead. Lin's typed code is mostly monomorphic so this should be rare —
  but it is a *speculative* speedup, not the *by-construction* static guarantee a packed/struct layout
  gives. The framing ("a struct field read **is** const-offset") is met in practice but always behind a
  shape guard.
- **Shape management cost:** interning shapes, shape transitions on construct/mutation, memory for shape
  ids. `var`/index-set/field-add cause transitions — Lin's records are mostly immutable-after-construct
  (helps), but the model must handle mutation correctly.
- **No layout/bandwidth win.** It gives fast *reads* on a pointer-chasing representation; it does **not**
  give the contiguous cache-locality of a packed `0xFE` buffer. For array-of-struct *iteration
  bandwidth*, a packed layout (Path 1/3/4) can still beat inline-cached-but-scattered objects.
- **Doesn't fix construction RC** (cost #3) on its own.

## Relationship to the other paths

- **Opposite philosophy to Paths 1/3/4** (which privilege a packed/value representation). Path 2 makes
  the dynamic one fast instead. In spirit mutually exclusive as a *first* move — but not technically
  exclusive long-term: a mature system could have hidden classes *and* a packed-elements array (V8 has
  both). As a first move, Path 2 is attractive precisely because it needs no new type, no gate, and
  removes the dominant bug class.
- **Composable with Path 3** (arenas/value semantics) for construction RC — Path 2 fixes reads, Path 3
  fixes construction.
- **Strongest no-language-change alternative** to Path 1's packed-by-default and Path 4's `struct` kind.

## Acceptance gates

Full `cargo test --workspace` green; a benchmark showing monomorphic-site reads hit const-offset speed
(interp is the ideal test — Token reads are monomorphic); a megamorphic-site fallback that never
crashes/mis-reads; shape-transition correctness under `var`/index-set/field-add (ASan-clean); RAPTOR
digest byte-identical; cross-language benchmark non-regression (interp, RAPTOR, records, dijkstra).

## Verdict

The "stop escaping the dynamic representation; make it fast" path. It uniquely **removes the
packed/boxed bug class entirely**, needs **no language change**, and **speeds up `Json` too** — but it is
a *speculative* (guarded) speedup, **novel for an AOT/LLVM backend** (highest implementation unknown),
and does not deliver contiguous-layout iteration bandwidth. Best if avoiding a new
type/representation/language-change is paramount and a pervasive-but-speculative speedup is acceptable
over a static guarantee.
