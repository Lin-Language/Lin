# Path 2 — Make the dynamic representation fast (inline caches / hidden classes)

**Status:** Open proposal, one of three independent paths. Self-contained.
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
- **No userland language change.** No semantics change, no
  representation-default inversion (cf. Path 1's packed-by-default). Existing object/`Json` code just
  gets faster. (All three surviving paths are now no-surface-change; this one is also no-*representation*-
  change and no behavior change — it just makes the one existing dynamic representation fast.)
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
- **Composable with Path 3** (inferred arenas) for construction RC — Path 2 fixes reads, Path 3
  fixes construction.
- **The no-*representation*-change alternative** to Path 1's packed-by-default (which adds a second
  representation and its boundary) — Path 2 makes the single existing representation fast instead.

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

---

## IMPLEMENTATION FINDINGS (2026-06-09, NOT merged)

> **Work reference (durable handles — the worktree branches are ephemeral and will be GC'd; cite the commits):** Path 2 was built across commits `4806324` (shape ids) · `8750517` (inline cache) · `85746a6` (constant-key threading) · `0e0a764` (gate off by default) · `436a86c`/`568d730` (hit-rate instrumentation + diagnosis). `git log <hash>` to recover.

**This path was implemented end-to-end, measured on a quiet box, and the result is a clean NEGATIVE: the
inline-cache mechanism works exactly as designed, is sound, and buys essentially nothing on the real
workloads. It is committed but GATED OFF by default. The diagnosis below is the valuable output — it
pinpoints where the dynamic-read cost actually lives, and it is *not* the cache.**

### The AOT feasibility unknown (the proposal's "highest unknown") — RESOLVED POSITIVE
A standalone LLVM-22 spike (hand-written IR, separate from the Lin compiler) confirmed the per-site
**static-mutable-global** inline cache is feasible on an AOT/LLVM backend: it compiles and runs correctly
at `-O2`/`-O3`/LTO, mono- and megamorphic, with **no `volatile` and no special aliasing flags**. The hot
path is exactly `load shape; cmp cached(%rip); jne miss; load offset; load field@offset` — no call. LLVM
register-promotes the cached shape across the loop and writes it back on the miss path, and crucially does
*not* constant-fold the cache away because the resolved offset comes from the opaque `lin_object_get`.
Synthetic win was ~1.25–1.4× non-LTO, ~4× LTO (bounded by a cheap synthetic fallback). **So the "novel for
AOT" risk is retired — the pattern is sound on this backend.** It just doesn't help the real programs (see
below), for reasons that have nothing to do with AOT.

### What was built (sound, committed, gated off)
- **S2.0 shape ids (always-on, transparent):** `LinObject.shape_id@40` + a process-global interned
  ordered-key-sequence table; lazy compute/cache/invalidate. Header growth is transparent to the inline
  `MakeObject` path (objstress digest-identical, ASan-clean).
- **S2.1 inline cache:** `lin_object_get_cached(obj, key, *LinIcSlot)` with a single packed-`u64` slot
  (`shape<<32 | offset`, atomically read/written → no torn read → **no key-reconfirm needed** → thread-safe
  by construction, answering the proposal's S2.3 concern). Codegen `emit_ic_object_get` emits the guarded
  fast path inline. `Index.key_const` was threaded through the IR so the IC fires on `Json["lit"]` and the
  `arr[i]["lit"]` RAPTOR shape (runtime `obj[k]` correctly stays uncached).
- **Correctness verified FIRST (the H8 lesson):** mono/megamorphic/absent-key/chained-runtime-key fixtures
  all produce identical values IC-on vs IC-off; the **full 683-test integration suite passes with the IC
  both ON and OFF**; RAPTOR digest byte-identical both ways (`group=26203913 range=773022892 journeys=139`).

### The measurement — marginal and MIXED, not a win (quiet box, load ≈ 0.4, digest-identical both ways)
| workload | IC off | IC on | Δ |
|---|---|---|---|
| RAPTOR GROUP | 99 510 ms | 96 240 ms | ~3.3% faster |
| RAPTOR RANGE | 290 965 ms | 301 898 ms | ~3.8% **slower** |
| interp | baseline | — | ~2.5% **slower** |

An earlier same-session A/B had reported ~7.6% GROUP; the quiet-box re-measure shows that was noise. Net
across phases the IC is a **wash-to-slight-loss**, which is why it ships gated off (`LIN_INLINE_CACHE=1`
to enable; default build is byte-identical to pre-Path-2 codegen).

### WHY it doesn't win — the decisive diagnosis (this is the real finding)
LIN_COUNT-gated instrumentation on the IC fast path over the full RAPTOR bench measured a **99.56% hit
rate** (656.6M inline hits / 659.5M sites; only 0.44% reached the runtime — essentially all lazy
first-miss-per-object shape warming). **So the cache is near-perfect; low hit rate is NOT the problem**
(this also kills the "eager shape assignment at construction" idea — it could only convert that 0.44%).
Inspecting the emitted IR/asm at an IC site confirms the hit path is the clean spike design (`load
shape@40; icmp; br; const-offset load`, no call) — but the **per-read machinery the IC does not touch
dominates it**: every `trips[i]["field"]` read still pays out-of-line `lin_string_literal` (key intern),
`lin_unbox_ptr`, `lin_get_tag`, and `lin_tagged_clone` (the owning +1). Those four calls dwarf the
~10-instruction inline hit. **The inline cache optimizes the cheapest part of a dynamic read.**

### Reconciliation with the path's premise, and the corrected next lever
The thesis "make the dynamic read a const-offset load" was achieved literally (99.56% of reads *are* now a
guarded const-offset load) — and it still didn't move the needle, because a Lin dynamic field read is not
just the offset resolution; it is offset-resolution **wrapped in** key-interning + unboxing + tag-dispatch
+ an owning clone. **The real Json-read lever is therefore NOT the inline cache** but: (1) hoist
key-literal interning out of loops (intern once, reuse the `LinString*`), (2) fold unbox + tag-dispatch
into the read site, and (3) drop the owning `lin_tagged_clone` where the result is consumed borrowed. Each
is independent of the IC and worth more than it. This redraws the path's "field reads: yes" claim: the IC
delivers the offset half cheaply, but the surrounding ABI is the actual cost — a finding that **also
explains §H6** (cheap packed reads recovered only ~6%: same shape, the read *wrapper* dominates, not the
offset).

### ⚠️ The cheaper alternative this path's measurement implies (2026-06-09): type the DICTIONARIES, don't cache them
This path's own finding — "99.56% of reads are now a guarded const-offset load and it still didn't move
the needle, because the cost is the read *wrapper* (key-intern + unbox + tag-dispatch + owning clone),
not the offset" — has a direct, cheaper corollary the path didn't draw: **where the `Json` is a
DICTIONARY** (string→value, the RAPTOR `routeStopIndex`/`bestArrivals`/`kConnections` shape), typing it
`{ String: T }` routes `m[k]` to `lin_map_get`, which **deletes the entire wrapper** (no key-intern, no
tag-dispatch, no per-lookup box) rather than caching the offset inside it. Measured: **~5.6× on RAPTOR
PREP** (`8859f713`, on master) — orders of magnitude more than this IC's ±3%. The IC only earns its keep
on genuinely *record-shaped* `Json` that is neither a typed map nor a packable struct. Before any further
IC investment, run path-0's per-call-site-CLASS profile to confirm the residual hot `lin_object_get`s are
record-shaped, not dictionary-shaped — the latter are removable by a type annotation. This *reinforces*
the "IC not worth enabling" verdict below. (See path-0 RETROSPECTIVE 2026-06-09 for why this dict-vs-record
distinction was invisible to the aggregate `lin_object_get` profile that launched these paths.)

### Cross-references to the other paths' findings
- **path-0 (`Trip|Null` tail-recursive leak):** this branch independently found and fixed the *same*
  universal RC/codegen leak the `path1-packed-records` agent fixed (their `04bec70`; this branch's
  `f0d02bf`) — two independent fixes of one real bug, mutually corroborating. Both are merge-worthy on
  their own; pick one.
- **path-1 (in-place packed ABI):** path-1's measured 4.5× on packed iteration is a *real* win where the
  value packs — orthogonal to and larger than this path's. The two are not in competition: path-1 helps
  statically-packable types; this path's residual lever (the read-wrapper above) helps the `Json`/boxed
  reads path-1 can't pack (e.g. RAPTOR's heap-field `Trip`).
- **path-3 (escape-based RC elision):** the owning `lin_tagged_clone` identified above as a dominant
  per-read cost is exactly the construction/RC-churn family path-3 attacks; the B2 RC-suppression
  (`a7366c4`, see path-3 findings) is a first concrete bite of it.

### Net verdict (revised)
The inline cache is **sound, correct, near-perfect hit rate, and not worth enabling.** Keep it gated off
(or drop it). The path's *real* contribution turned out to be diagnostic: it proved the AOT IC pattern
works, then proved the dynamic-read bottleneck is the read **wrapper** (key-intern + unbox + tag + clone),
not the offset resolution the IC accelerates. Spend the next effort on the wrapper, not the cache. The
~2.5% interp regression (interp's hot reads are packed records that bypass `lin_object_get` entirely, so
the IC sites that fire are pure guard overhead there) independently confirms the IC is not the interp
lever either.
