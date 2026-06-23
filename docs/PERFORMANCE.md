# Lin performance

> Engineering documentation, not marketing. Every quantitative claim below is
> traced to a measured source — a benchmark in `benchmarks/`, a cross-language
> cell in `benchmarks/compare/`, an ADR in `docs/DECISIONS.md`, or one of the
> `path-*` perf proposals (which this document is the surviving distillation of).
> Where a number is *inferred* rather than measured it says so.

---

## 1. TL;DR

Lin compiles to native code via LLVM, so **type-bound, statically-shaped code
runs at or near systems-language parity**: the `records` cross-language workload
(constant-offset field access on a sealed all-scalar struct) lands at **Lin 200 ms
vs Rust 224 ms vs Go 624 ms** — Lin beats both. The eager combinator `pipeline`
workload (`map`/`filter`/`reduce`) **beats Rust ~4×** (25 ms vs 100 ms) because the
chain fuses to a single zero-allocation loop.

Lin is **slow exactly where the program leans on its dynamic escape hatch**:
`AnyVal`-typed field access lowers to a **runtime tag-dispatch + an O(1) hashed
`lin_map_get`** (or, for a `TAG_RECORD` value, a descriptor field read) and is an LLVM
**optimization barrier** — the type is unknown at compile time, so the read can't fold
to a constant-offset load, hoist, or elide — measured **~4× slower** than the
equivalent typed record on the `records` workload, and ~70× slower on heavily
`AnyVal`-read-bound code (RAPTOR). (The cost is the per-access dispatch + barrier,
**not** an O(n) association-list scan — that `LinObject` representation was deleted in
the reset, §5.6.) The other structural cost is the **call boundary**: a polymorphic
combinator calling a non-devirtualizable closure boxes each element and unboxes
the result across an opaque indirect call. The one-line story: **Lin is a
fast native language when you give it types, and a slow dynamic one when you
hand it `AnyVal`.**

---

## 2. The benchmark picture

There are two harnesses, deliberately separate:

- **`benchmarks/run.sh` + `benchmarks/*.lin`** — Lin-only. Times compiled Lin
  binaries against each other across code changes. This is the regression gate
  for codegen/runtime work. Targets per file:

  | File | Hot path exercised |
  |------|--------------------|
  | `recursion` | call/return overhead, TCO loop transform, non-tail self-recursion (`fib`); mostly unboxed `Int32` so isolates call + branch |
  | `array_pipeline` | `map`/`filter`/`reduce` over a range: indirect closure calls, Int32 box/unbox through the `AnyVal` element slot, RC on intermediates |
  | `object_access` | object construction + dynamic field lookup (tag-dispatch + O(1) hashed `map_get`); chained reads multiply the per-access dispatch + optimization barrier |
  | `string_build` | string allocation (historically no SSO), interpolation/concat, string RC |
  | `map_flat_scalar` | packed scalar array + `{String:T}` map store in a hot loop |
  | `async_await` / `worker_roundtrip` | **latency** — per-op round-trip (thread spawn, env deep-copy, mailbox) |
  | `parallel_speedup` | **throughput** — 8 CPU-bound chunks; wall-clock should approach one chunk, `user` ~8× |
  | `thread_pool` / `shared_lock` | dispatch + lock/queue contention |

- **`benchmarks/compare/`** — cross-language. Seven identical workloads against
  **Rust, Go, Python, Node**, plus the larger **RAPTOR** GTFS journey-planner
  port (`benchmarks/compare/raptor/`, five-language). Whole-process min
  wall-clock; startup is deliberately included. **Indicative, not authoritative**
  (one machine, one timer). The seven workloads: `dijkstra` (graph build +
  shortest path + parsing), `interp` (tokenize → recursive-descent parse →
  tree-walk eval — the call-bound generalized workload), `parallel` (CPU fan-out),
  `recursion` (`fib` + iterative sum), `pipeline` (eager `map`/`filter`/`reduce`),
  `records` (constant-offset struct field access — the typed-layout workload), and
  `async_io` (latency-bound bounded concurrency).

> **⚠ Measure perf in RELEASE, never debug.** `compare.sh` builds `target/release/lin` +
> `cargo build --release -p lin-runtime -p lin` (it forces a fresh *release* runtime archive). A/B
> testing with `target/debug/lin` links the *unoptimized* debug `liblin_runtime.a`, where every
> runtime-call (the bounds `_oob` accessor, `lin_map_get`, RC ops, string ops) is ~8× costlier than
> release. That inverts the signal for any change that inlines/elides runtime-calls: it looks like a
> large win in debug and is neutral-or-negative in release. The "take-five" campaign (§5.9) learned
> this the hard way — a debug-measured **−18 % on RAPTOR turned into a +2.5 % release regression**
> once re-measured correctly. Always: release build, release runtime, same-batch interleaved, min of
> ≥3 (`compare.sh` is the reference harness).

### Cross-language results

Measured on master (`benchmarks/compare/compare.sh`, RUNS=5, min wall-clock ms,
lower = faster; every language computed the same result per workload — a
correctness gate):

| workload | lin | rust | go | python | node |
|----------|----:|-----:|---:|-------:|-----:|
| records   | 200 | 224 | 624 | 23581 | 1135 |
| pipeline  | 25  | 100 | 116 | 1402  | 500  |
| recursion | 479 | 476 | 883 | 20178 | 2193 |
| async_io  | 202 | 202 | 202 | 230   | 216  |
| parallel  | 168 | 104 | 172 | 18904 | 4265 |
| dijkstra  | 35  | 8   | 13  | 205   | 52   |
| interp    | 363 | 12  | 50  | 216   | 42   |

**The shape you can state confidently** (mechanism-grounded, not noise):

- **`records` beats Rust, ≫ Go/Python/Node.** Sealed all-scalar records lay
  out as packed heap structs; each field read is a constant-offset load LLVM can
  hoist/fold. This is Lin's headline strength.
- **`pipeline` beats Rust ~4×.** The eager `map`/`filter`/`reduce` chain fuses into
  a single loop with zero per-element boxing (ADR-044, Path 6).
- **`recursion` ≈ Rust** (TCO loop transform + unboxed Int32).
- **`interp` is Lin's weakest cell** (~30× Rust): it is call- and allocation-bound
  — per-iteration token arrays, AST nodes, recursion, tagged-union dispatch —
  exactly the non-devirtualizable-call cost of §4.
- **`dijkstra` trails Rust/Go** but beats the interpreters; the gap is the
  linear-scan PQ and object field access in the graph representation.
- **`parallel`/`async_io` ≈ Rust** (native threads; async_io is latency-floored,
  every language pins to the sleep floor).

### Typed vs `AnyVal` (RAPTOR)

RAPTOR is the load-bearing real-world workload — a GTFS journey planner threaded
through many generic boundaries. We measured a **fully-typed** RAPTOR (trips as
`Trip{tripId, stopTimes: StopTime[], service}` records, the route map as
`{String: Trip[]}`) against the `AnyVal` baseline on the same compiler, full feed,
O2, single-run min, digest byte-identical (`group=26203913 range=773022892
journeys=139`). The "naive typed" column is the straightforward typed port; the
"typed" column is after the **de-materialization** pass described below:

| phase | `AnyVal` (ms) | naive typed | typed (de-mat) | typed / `AnyVal` |
|-------|----:|----:|----:|----:|
| LOAD  | 15779 | 16209 | 17353 | 1.10× |
| PREP  | 28365 | 119567 | 104264 | **3.67×** |
| GROUP | 62040 | 140221 | 114583 | 1.85× |
| RANGE | 184475 | 401310 | 334286 | 1.81× |
| total | 290759 | 677807 | 571086 | **1.96×** |

**Two distinct costs, with opposite outlooks — this is the key finding.**

A typed heap-field record (`Trip` holds a `String` and a `StopTime[]`) does **not**
reach the packed flat layout — it is boxed, like the `AnyVal` form — and a *typed*
read **materializes the record per access** (a `lin_sealed_alloc` re-projection as
it is read out of a `{String: Trip[]}` map, narrowed from a `Trip | Null`, or bound
out of a boxed `Trip[]`). The naive typed port paid this everywhere, for the ~2×
query penalty.

1. **Query READ materialization is de-materializable — and we fixed it.** Wherever
   the hot path only needs a *field*, read it directly by **fused const-offset
   index** off the packed array (`trip["stopTimes"][i]["arrivalTime"]`) instead of
   binding the whole record (`val st = trip["stopTimes"][i]`); and thread loop state
   as an **`Int32` index**, not a materialized `Trip | Null`, materializing the one
   chosen record at the end. Applied to `scanRouteAt`/`scanBack` (query) and the
   `getRouteId`/`buildRoutes` per-stop loops (PREP), this cut `lin_sealed_alloc`
   **299.6M → 184.5M** and pulled the query phases from ~2.2–2.3× to **~1.8×**,
   digest-exact. The earlier "`Trip | Null` union in `scanRouteAt` is the dominant
   seam" read (from an IR-op *count*) was a partial red herring: the dominant
   *time* was per-step whole-record binds, which this idiom removes.

2. **PREP construction/regroup materialization is largely INHERENT.** PREP stays
   **3.67×** because it does not *read* records, it *builds and copies* them:
   `tripsByRoute[routeId]` is grown by `push(arr, sorted[i])`, and a packed value
   array **copies each `Trip` record in** (a required `lin_sealed_alloc`), where the
   `AnyVal` form copies a pointer. The stable `sort` likewise binds whole `Trip`s per
   comparison. De-materialization removed only the incidental per-stop `StopTime`
   binds (the −13% PREP delta); the regroup/sort copies are the price of value
   semantics for a construct-heavy workload and are not addressable without storing
   indices in place of records. **Typing reaches ~1.8× on read-heavy query work, but
   record construction/regrouping carries an inherent copy cost `AnyVal`'s pointer
   sharing avoids.**

Two honesty notes: (a) the comparison is kept **algorithmically faithful to the
reference** (node/go/rust) — a `runsOn`-by-`serviceId` memo that would have shaved
another ~20s was *dropped*, not added, because the reference does not memoize and
mirroring it into the `AnyVal` port would have let Lin cheat cross-language; (b) the
typed RAPTOR is **leak-free** (RSS bounded/flat, ASan-clean), so the residual gap is
materialization *time*, not allocation churn.

The orthogonal win that *did* pay independently of all this: typing the
**dictionaries** — the `{String: Int32}`/`{String: Trip[]}` index and scan-state
maps that were `AnyVal` objects — replaced O(n) association-list scans with O(1)
hashed `LinMap` lookups. So the guidance is nuanced: typing your *dictionaries* and
*scalar records* is a clear win; typing *heap-field record graphs* is a clear win
for the **read** path (use the const-offset-index idiom) but carries an inherent
copy cost on the **construct/regroup** path. The one remaining *representation*
lever (unshipped) is teaching `match` on a union to narrow the **value** to a packed
repr, not just the type — the boxed-record-in-a-union seam (`Conn = Boarding |
Transfer`) that still materializes per read (see §5).

---

## 3. Strengths 

Each of these is why a typed Lin program is fast; the mechanism is the thing to
preserve.

- **Closed/sealed records → flat structs, constant-offset reads.** A named
  all-scalar `type State { a..f: Int64 }` is laid out as a packed heap struct;
  `state["a"]` is a constant-offset load, not a string-keyed hash probe. This is
  what puts `records` at Rust parity. (`records` bench; measured ~4× over the
  same code typed `AnyVal`.)
- **Combinator fusion.** `range().map().filter().reduce()` lowers to one counted
  loop with **zero per-element boxing** at `-O2` (ADR-044). Measured ~3.3× over
  the unfused path (200 M elements: 0.817 s → 0.247 s, Path 6); the `pipeline`
  cell beats Rust as a result. The capture-less *and* (since this session)
  capturing literal-lambda callbacks both inline at the Layer-1 gate.
- **Packed unboxed scalar arrays.** `Int8..Int64`/`UInt*`/`Float32/64` arrays are
  contiguous element-width buffers, no per-element tag (ADR-040). A `UInt8[]` is a
  literal byte buffer. In-place packed-array iteration (Path 1 steps 1+2) measured
  **~4.5×** over materializing to boxed `Object[]` (2.84 s → 0.63 s).
- **Unboxed primitives.** `Int32→i32`, `Float64→double`, `Bool→i1`; no boxing for
  arithmetic/comparison/calls on scalars (ADR-015) — the 50–200× gap over a
  tree-walking interpreter.
- **TCO via alloca/loop transform** (ADR-016): tail self-recursion runs in
  constant stack as a native loop `mem2reg` promotes to phi nodes — no trampoline,
  no per-call heap.
- **Unboxed recursive sum types** (ADR / sumtype build): tagged sum nodes are
  unboxed with a tag-switch and child drop-walk — the lever for interpreter ASTs.
- **`{String:T}` index-signature maps → real hashed `LinMap`** (ADR-055):
  open-addressing FNV-1a, **O(1) average** lookup, not the O(n) association-list
  `LinObject`. Choosing `{String:T}` over an open object for dictionary data is a
  direct algorithmic win (digest-identical, measured on 783k-read RAPTOR data).
- **Singleton string-literal types** (ADR-034) let tagged unions discriminate at
  compile time rather than via a runtime string compare.
- **Deterministic refcounting** (ADR-024) with Perceus-style compile-time elision
  (`rc_elide.rs`): no GC pauses, predictable memory. Escape analysis suppresses RC
  entirely for non-escaping records (an all-scalar sealed record in a 200 M-iter
  loop SROAs to registers, ~0 runtime RC — Path 3).
- **Small-value caches.** Immortal interned `LinString` per small int in
  `[-128,1024)` for `toString` (measured map_flat_scalar allocs 38.9 M→26.1 M,
  ~33%; wall ~5.8 s→5.5 s), and the scalar-box cache routes flat-scalar gets
  through the same window. Float/int stringification writes into a stack buffer
  rather than `format!`.
- **Native concurrency at Rust parity** (`parallel`/`async_io`): OS threads with
  copy-by-default transfer (ADR-028), atomic-RC `Shared<T>` box + `RwLock`, and
  immortal zero-copy `Frozen<T>` reads — the single-threaded hot path keeps
  non-atomic RC.

---

## 4. Where it struggles

- **`AnyVal`-read-bound code is the cliff.** `AnyVal` field access lowers to a
  **runtime tag-dispatch** (unbox → load tag → branch, `codegen/data/index.rs`) that
  routes to an **O(1) hashed `lin_map_get`** for a `TAG_MAP` value or a
  descriptor-driven field read (`lin_record_get_field`) for a `TAG_RECORD`, *and* it is
  an LLVM optimization barrier — the type is unknown at compile time, so the compiler
  can't elide, hoist, or fold the read the way it does a typed record's constant-offset
  slot. Measured ~4× on `records`, ~70× on RAPTOR-class code. **NB — not an O(n)
  scan:** the per-access cost is the tag-dispatch + barrier (+ the hashed probe / a
  small descriptor walk), **not** an association-list scan over the object's entries.
  That O(n) `LinObject`/`TAG_OBJECT` representation was *deleted* in the representation
  reset (§5.6); dynamic string-keyed data is now an O(1) `LinMap`. Don't re-describe
  `AnyVal` access as "O(n) field lookup." `AnyVal` is a genuine escape hatch (untyped
  wire data, recursive ASTs), not a default. The fix is *userland*: type the data
  (§6), not a codegen tweak — see the path-9 closed-negative in §5.
- **The non-devirtualizable call boundary.** A polymorphic combinator (or any
  stored closure) calls its callback through a uniform all-ptr boxed-closure ABI:
  it boxes each element argument and unboxes the boxed result across an opaque
  indirect call (`CallTarget::Indirect`), which LLVM cannot see through, so the
  box/unbox pair never cancels. This is the dominant cost in `interp` (call-bound)
  and is why Tier-1 "make the runtime inlinable" bought <2% alone (§5, path-8):
  the *consumer* stays opaque. Devirtualization (this session: `find`/`some`/`every`
  with a named no-capture callback) attacks it directly and measured **2.54×** on
  the microbench, but the general case is unsolved.
- **String building had no SSO historically.** Every `LinString` is a heap
  allocation; hot interpolation/concat loops allocate per step. Partially
  addressed this session (immortal small-int `toString` cache, one-pass
  `utf8Bytes`/`fromCodePoints`/`tryParse` intrinsics) but there is still no true
  short-string optimization (`string_build` bench).
- **RAPTOR's structural cap.** Functional code threads records through many
  generic boundaries; each is a representation-boundary materialize-or-leak site.
  This caps how fast RAPTOR-shaped programs can get *without* a whole-program
  representation change — and that change (end-to-end packing) measured *slower*
  (§5). Honest status: the biggest real-world lever (typed RAPTOR end-to-end) is
  not yet a net win.
- **Peak memory on typed RAPTOR is high (~25 GB vs Node ~2–4 GB).** This is a
  *separate axis* from the throughput finding that "no workload is alloc-bound"
  (§5, path-7): that's about allocation *churn*; this is about *live* peak RSS.
  Per-kind attribution of the 132M-live peak (2026-06): **maps = 15.25 GB / 76 %**
  (51.5M live maps, ~296 B each), sealed records = 4.47 GB, transient
  tag-boxes/array/string < 1 GB. **Crucially, those maps are NOT dictionaries — a
  per-first-insert-tag census (`LIN_VKIND_STATS`) found 99.99 % of them are
  *materialized records*** (a `StopTime{stopId:String, arrivalTime:Int64, …}`
  flattened field-by-field into a `{String:…}` map: first key tag String 204.8 M,
  then a differently-typed field → heterogeneous). So the lever is **stop
  materializing records into maps** (keep them packed — the §5.6/§5.7
  de-materialization direction), *not* a per-map slot tweak. The slot *is*
  structurally floored (`hash 8 + key 8 + value:TaggedVal 16 = 32 B`), and
  **value-unbox** (24 B homogeneous slots / 32 B MIXED, ABI preserved via a
  per-thread scratch ring + record-materializers born MIXED) *shipped* this
  session — but it's **neutral on RAPTOR** precisely because those dominant maps
  are heterogeneous; it wins on genuinely homogeneous scalar maps (`{_:UInt32}`).
  The cheap structural win (`INITIAL_CAP` 8→4, −1.4 GB, byte-identical IR) shipped.
  See §5.7 for the full memory deep-dive.
- **No reference-cycle collection** (ADR-024). Cycles between long-lived heap
  objects leak; documented, the fix is to null a field to break the cycle.
- **`Number` (boxed numeric union) is ~3.6× slower than a concrete family**
  (ADR-014) — prefer a concrete `Int32`/`Float64`.

---

## ★ 5. Path-n learnings — the consolidated record

This is the heart of the document: one row per perf-investigation path, distilled
so the `path-*` proposal docs can be deleted without losing the conclusion. The
recurring theme: **two bottlenecks in two programs** — `interp` is *call-bound*,
RAPTOR is *`AnyVal`-read-bound* — and the cost is always *work per operation*
(reads, calls, materialization), never allocation/reclamation itself.

### The big closed-negatives (do not re-try these)

| Path | Tried | Measured | Verdict | WHY |
|------|-------|----------|---------|-----|
| **7 — tracing GC** | Replace RC with generational tracing GC to make allocation cheap | `LIN_NO_RC` ceiling (entire allocator+RC as no-ops): **0.48 s vs 0.408 s = NO speedup**; RAPTOR ~1.0× all phases despite textbook GC-bait retention (32.9 GB allocated, 0.039 retention, 96% dying young) | **CLOSED-NEGATIVE** | A GC can't recover a cost that deleting the *entire* heap+RC subsystem doesn't recover — the cost is work-per-allocation (reads + calls), which GC doesn't touch. **No workload is alloc-bound.** Revisit only for correctness (RC-UAF), never perf. |
| **9 — end-to-end packed records** | Pack heap-field records all the way (loader→map→read) so RAPTOR's 630 M linear scans become const-offset loads | Three independent agents built digest-correct end-to-end typed RAPTOR: PREP 7.7 s→27.2 s (**3.5× slower**), GROUP 19.9 s→36.2 s (**1.82×**), RANGE 59.4 s→105.3 s (**1.77×**) | **CLOSED-NEGATIVE as a flow-sensitive oracle → re-approached and RESOLVED by the representation reset (§5.6)** | The cost is **representation-boundary materialization, not field reads.** Each packing fix repaired a bug a prior packing fix introduced — "fix-for-a-fix all the way down" — *because* it tried to reconcile packed-vs-boxed at compile time. The **reset (§5.6)** deletes that reconciliation: representation is type-determined (a record is *always* packed; the dynamic case is runtime-tagged `TAG_RECORD`/`AnyVal`). That landed the architecture — but the honest re-measure (§5.6) shows it lands at **parity**, because representation is ≤4% of RAPTOR; the real lever is the call/value axis. |
| **5 — value records** | Make fixed-key records inline values (no header/shell RC), claimed semantics-preserving | Falsifying test on master: `val b=a; a["state"]=99; b["state"]` → **99** | **CLOSED-NEGATIVE (premise falsified)** | Records are observably-mutable **reference** types; value semantics is a *breaking* change, not a free representation swap. Cost diagnosis was right; the "non-breaking" framing was wrong. The live form is Path 1 (packed *representation*, not value *semantics*). |
| **2 — inline caches / hidden classes** | Shape ids + per-site inline cache for `AnyVal` field offset resolution | **99.56% cache hit rate** (656.6 M/659.5 M); but RAPTOR GROUP −3.3%, RANGE +3.8% slower, interp +2.5% slower — **net wash-to-loss** | **CLOSED-NEGATIVE (built, sound, gated off)** | The IC mechanism works perfectly, but it optimizes the *cheapest* part. The real per-read cost is the wrapper (key-intern + unbox + tag-dispatch + owning clone), not offset resolution. The cheap corollary is to use `{String:T}` → `lin_map_get` instead. |
| **8 Tier-1 — bitcode runtime** | Compile runtime to bitcode + `alwaysinline` so box/unbox can cancel | Spike: **<2%** (interp −0.6%, object_access −1.8%); box/unbox pairs do **not** cancel | **Dead-end alone** | The *consumer* (indirect closure call / `lin_tagged_arith` / `lin_object_get`) stays opaque, so the box never meets its unbox. Inline the consumer (Tier 2/3) first; Tier 1 last. Reversed the path-8 sequencing. |
| **8 Tier-3 — named-call devirt** | Devirtualize known/named calls | Named calls are **already** direct; interp hot path 100% direct; devirtualizable population ~0 | **Dead-end** | The lever is *lambda-set*-shaped (callback sites *inside* stdlib combinator bodies), not named-call devirt. |
| **3 — inferred arenas (full)** | Whole region inference: bump-allocate scoped graphs, suppress RC, bulk-free | Foundation (escape analysis) proven live; full arena **not attempted**; borrow prototype returned **wrong values** | **Deferred (soundness risk)** | Multi-week with high region-drop-UAF risk. The shippable sub-piece (B2 escape-RC elision for non-escaping heap-field records) is sound and landed; full arena isn't worth the UAF surface. |
| **6c — leaf-helper inlining** | Inline leaf helpers LLVM can't see | ~2–3% interp, **0% on fused chains** | No-win standalone | Box/unbox can't cancel while the consumer (`tagged_arith`/`object_get`) stays opaque — same opacity wall as Tier-1. |

### The wins (shipped, with the mechanism that paid)

| Path / work | What shipped | Measured | WHY it worked |
|-------------|-------------|----------|---------------|
| **1 (steps 1+2) — in-place packed-array ABI** | `length`/index/`for`/`map`/`reduce` operate on `0xFE` packed arrays without materializing to boxed `Object[]` | **~4.5×** (2.84 s→0.63 s); 0 `sealed_array_to_tagged`, 0 `lin_object_get` in IR | Killed cost #2 (whole-array materialization at combinator entry); the in-place ABI thesis validated. |
| **6 / 8 Tier-2 — combinator fusion** | `range().map().filter().reduce()` fuses to one loop; record-combinator fusion (Step 8.1) | ~3.3× (200 M: 0.817 s→0.247 s); pipeline 75 ms→27 ms (2.8×); Step 8.1 ~2.28× over 2 M records | Removes the per-element call **and** the intermediate arrays — the only lever consistent with the call-bound ceiling test. Merged `acf35a83`. |
| **6b — monomorphic dispatch** | Monomorphic dispatch for polymorphic stdlib ops | ~1.35× on length-bound loops | Direct dispatch removes one indirect hop. |
| **Wave C — lambda-set devirt (this session)** | `find`/`some`/`every` with a *named no-capture* callback: per-callback spec axis substitutes the callback param with the named fn `L`, turning the per-element boxed indirect call into a direct `@isEven(i32)` call | **2.54×** (find+some over 2 M Int32[], 200 iters: 32.6 s→12.85 s); RAPTOR IR byte-identical | Attacks the call boundary (§4) directly for the devirtualizable subset. Capturing-lambda callbacks correctly stay on the indirect path. This is the realized, narrow form of path-11's lambda-set thesis. |
| **flatMap fusion (Wave D, this session)** | flatMap fuses as a push-model loop-nest stage; empty-inner (`x=>[]`) case fuses + reclaims | byte-identical corpus | flatMap is fusable as a nested push loop, not a pull-fusion barrier. |
| **dict→Map fidelity (path-9 salvage)** | `AnyVal`-as-dict → real `{String:T}` `LinMap` | 783k reads, digest-identical | O(1) hashed lookup vs O(n) assoc-list — the cheap corollary of the path-2 finding. |
| **capturing-lambda inline + stack-overflow fix (this session)** | Admit capturing literal lambdas at the Layer-1 inline gate | ~3.9× on local-capture map/reduce microbench | Earlier revert was a *stack* overflow (per-iteration `alloca` in the loop body), not a heap leak; fixed by hoisting the scratch alloca to the entry block (`entry_block_alloca`). |
| **9C salvage — seal-propagation** | Producer/consumer seal agreement in the checker | fixed live data corruption (nested all-scalar sealed-record-array read: garbage `7 0` → correct `33 44`) | A correctness fix surfaced by the packing work; merged independently of the (negative) packing chain. |
| **Runtime alloc wins (this session)** | toString small-int cache (~33% allocs); one-pass `utf8Bytes`/`fromCodePoints`/`tryParse*` intrinsics; byte-key `lin_object_get_bytes` (no temp LinString); display stringifier into one buffer; float/decode via `write!` not `format!`; union-probe save-len/truncate not clone | per-commit allocation reductions; map_flat_scalar wall ~5.8 s→5.5 s | Eliminate per-call transient allocation in hot runtime paths. |
| **stdlib algorithmic wins (this session)** | csv scanners/trim tail-recursive (was `range().while`); `buildQuery` `string.join` not O(n²) `joinAmp`; `array.chunk` inner copy via `slice`; `object.pick` bind-once; `lin_object_eq` O(n·m)→hash-index | csv ~60× on large input; object-eq ~2.0× on 24-key records | Big-O fixes beat micro-tuning; the csv O(N²)→O(N) is the largest single win this session. |
| **loop-emitter unification (this session, cleanup)** | `emit_combinator_loop`: one counted-loop scaffold for index + packed views; `lower_while` re-expressed through it | for/map/filter/fusion byte-identical | Not a perf win itself — removes ~95% duplication so future loop work lands once. |
| **ownership-as-a-fact verifier (this session)** | `Convention{Borrow,Own,Inout}` on `LinFunction` + `LIN_OWNERSHIP_SHADOW` report-only RC-balance verifier; first per-site heuristic consumed (Index-result lifetime) | zero behaviour change; shadow CLEAN | Foundation for sound RC elision and path-10's borrowed-reads; ships as inert metadata first. |

### 5.6 The representation reset — path-9 re-approached and resolved (2026-06)

Path-9 (above) closed *negative* because it tried to recover struct speed for dynamic-context records
with a **flow-sensitive, compile-time packed-vs-boxed oracle** (`repr.rs`, ADR-062) — every generic
boundary became a materialize-or-leak seam, and each packing fix repaired a bug a prior packing fix
introduced. The **representation reset** re-approached the same goal from the opposite side and is the
project that finally closed it — not by a better oracle, but by **deleting the oracle**.

**The thesis.** A *record* and a *JSON object* had been welded into one boxed, refcounted,
**string-keyed** representation (`LinObject`), so field access was an association-list lookup and an LLVM
optimization barrier even when the field set was statically known. The fix draws three clean lines:

- **Records are flat packed sealed structs** with constant-offset field access — and keep **reference
  semantics** (`val b = a` shares; mutation through a parameter is visible). Representation is now
  **type-determined**, not inferred: a record is *always* a packed struct; there is no boxed shadow.
- **The dynamic value is `AnyVal`** (née `AnyVal`) — a **JSON-shaped** tagged union (`Null | Bool | Int* |
  Float* | String | AnyVal[] | {String:AnyVal} | <record>`) that is **runtime-tagged** (`TAG_RECORD`
  carries a sealed pointer + descriptor, modelled on `SumNode`/ADR-064) rather than reconciled by a
  compile-time oracle. It deliberately **cannot hold an opaque handle** (`Function`/`Iterator`/`Stream`/
  `Shared`/`Promise`/`TarEntry`); handle-carrying code stays statically typed. There is intentionally
  no true `Any` top type — generics `<T>` and unions cover the parametric cases.
- **Dynamic string-keyed data is a real `LinMap` hashmap** (`{String:T}`), O(1) — not a string-keyed
  object. Genuinely-dynamic runtime objects (HTTP/URL/env/error) now build `LinMap`; known-shape
  results reconstruct typed records.

**What landed (on `master`):** Stages 0–4 + 6a + the 6b runtime-producer migration. `T | Null` over a
record is a **nullable sealed pointer** (`Layout::NullableRecord`, no per-access materialize); `A | B`
is a tag + sealed payload; `match … is T` narrows to a typed pointer. `repr.rs` collapsed to a **pure
layout calculator**; the entire "path-9" problem space is deleted. **ADR-069 supersedes ADR-062.** The
compiler got *smaller*. Also shipped on this foundation: numeric **`{Int:T}` map keys** (raw-`i64`
inline keys — faster *and* smaller than string keys; SPECIFICATION.md §5.1.1).

**The honest verdict (measured, not asserted).** Typed RAPTOR is at **~parity** with the pre-reset
baseline — *not* faster. A cycle profile (rdtsc bucket counters) explains why:

| bucket | share |
|---|---|
| `lin_map_get` (string-keyed) | 9.5% |
| field lookups (`record_get_field`/`object_get`) | 3.7% |
| `tagged_eq` / alloc / box-unbox / ptr-chase | ~2% combined |
| **`tagged_arith`** | **0.0%** (typed arithmetic fully inlines) |
| **everything else (~85%)** | **the call/value axis — closure + loop dispatch, control flow** |

Representation touches **≤ ~4%** of RAPTOR, so the reset is an **architecture + simplification win at
parity**, *not* a RAPTOR speedup — and that is the correct, honest outcome. It retires `§5.6`-style
inline-array layout (the pointer-chase it targets is 0.2%) and the older "inherent PREP copy" framing
(§2.4): under pointer-backed reference arrays the regroup shares a pointer.

**Therefore the real Go-gap lever is the call/value axis** — confirmed by direct interp profiling: ~82%
of the `interp` benchmark is **object allocation + RC at call/value boundaries** (per-frame `Cursor`/
`Token` allocs, ~1000 retain/release sites), not representation, dispatch, or strings (a measured
"string slice-copy" worry proved false at 0.01%). The tractable levers there are **alloc elimination**
(stack-alloc non-escaping frames, multi-value returns) and **RC elision on hot borrows** — a separate
project from the reset, and the one that actually narrows the 80–113× interp gap to Go/Rust. (A
sealed-struct version of the `Cursor` was tried and *regressed* 9%: un-boxing is not the same as not
allocating — the alloc count is what matters.)

---

### 5.7 Memory + interp — the 2026-06 deep-dive (what worked, what was sound-but-0%, and why)

This session attacked the two open gaps from §5.6 — typed-RAPTOR peak memory (~25 GB) and the interp
call/value axis — with a fleet of file-disjoint lanes. The headline reinforces §5/§5.6: **both gaps are
allocation/materialization-bound, and that lives in *how the program is written*, not in the representation.**
Nearly every "clean" compiler-side repr/RC optimization came back **sound but ~0 %**; the wins came from
changing what the program *allocates*.

**Map attribution, corrected.** §4's 76 %-of-peak LinMap is **99.99 % materialized records**, not dictionaries
(`LIN_VKIND_STATS` census). Confirms the de-materialization direction, not a per-map tweak.

**What worked (merged):**
- **`Cursor.node` union-field sealing (lane U).** The interp's per-node `Cursor{node:<sum>, pos}` was boxed as
  a hashmap because its single-pointer union field disqualified sealing. Admitting single-pointer (`*SumNode`)
  union fields into the packed layout (`NKIND_SUMNODE`) cut the parser hot loop **1.66 M `lin_map_get`/run → 0**
  and dropped the interp leak 10×. (Distinct from §5.6's "sealed-struct Cursor regressed 9 %" — that made the
  whole record a value struct; this seals one *field* and keeps reference semantics + removes the map.)
- **Interp leak fix.** Interp leaked ~34 MB / 1.49 M allocs *per run* (a String-TCO under-release in
  `lin_string_slice`/`char_at`) → **424 B / 27 allocs** (residual = intentional string-interning).
- **LinMap `INITIAL_CAP` 8→4** (−1.4 GB, byte-identical) — the one cheap structural memory win.

**Sound but ~0 % (the pattern is the point):**

| Lever | State | Why ~0 % |
|---|---|---|
| **value-unbox** (24 B homogeneous slots / 32 B MIXED; ABI kept via a scratch ring; materializers born MIXED) | merged, neutral | dominant RAPTOR maps are heterogeneous materialized records → can't shrink. *Wins on homogeneous scalar maps* (`{_:UInt32}`/`{_:Boolean}` — the fully-typed port). |
| **0xFE inline record arrays** | merged, sound | RAPTOR builds arrays **store-then-push** (`g[k]=[]; push(g[k],…)`), incompatible with inline (push corrupts the headerless buffer). Fires only for local read-only arrays. *Unblocks once the port build-then-stores / `freeze`s.* |
| **RC-elision at Borrow calls** (Option C) | merged, sound | interp is alloc-bound, not RC-bound — 28 elided RC ops are dwarfed by `lin_map_alloc`/node. |
| **stack-alloc heap-field records** (interp-D) | branch, not merged | the interp's `Cursor`/`Token` are *returned up the parse chain* → escape the frame → 0 stack allocas in interp IR. The lever is **arena/region**, not stack. |

The recurring shape: an optimization is sound and passes every gate, but moves ~0 % because the program's data
doesn't meet its precondition (heterogeneous maps; store-then-push arrays; escaping frames). The wins need to
attack the *allocation* directly — de-materialize records (port-side), eliminate the per-node alloc (arena),
and the convergent idea, **`freeze`** (below).

**SMI — inert → enabled → dropped (a full round-trip, worth recording).** Pointer-tagged small-int inlining
(`(n<<1)|1`) was carried as "what dates-as-ints needs." Three findings killed it: (1) the merged `smi` feature
was **inert** — `lin_box_int*` never emitted immediates, so "it passes all gates" was verifying a no-op;
(2) when actually enabled it was a **whack-a-mole of consumer-guard bugs** — every `*const u8` deref must check
`is_smi_ptr`, and **"tests pass" is necessary but NOT sufficient** (an unguarded path with no test is invisible:
array-slice and regex both shipped green and segfaulted later); (3) decisively, the fully-typed RAPTOR port
stores dates as **typed `UInt32`** — unboxed record fields, raw integer map keys, scalar map values — which
**never call `lin_box_int`**, SMI's only target. SMI fires *zero times* on the real workload. Stripped from
master (−811 lines), preserved on `reference/smi`. The general lesson — a tagged-pointer scheme with scattered
consumer guards is fragile by construction — also gates **inline SSO** (its design agent independently flagged
the same surface).

**Process lessons (cost real time):**
- **Agent self-reports are not trustworthy.** Lanes self-reported "820/0, mergeable" with real regressions (a
  4-RAPTOR-query break; an all-scalar stack-residence regression; the SMI guard bugs). Re-run every gate.
- **Stale / feature-mixed incremental builds give *flaky* failures.** Interleaving `--features`/ASan builds in
  a worktree produced phantom "2 failed" that vanished after `cargo clean` — which led to chasing non-bugs
  *and* nearly dismissing a real one as "stale." Rule: `cargo build --workspace` first, then test; `cargo
  clean` between feature states.

**The convergent direction — `freeze` as a repack primitive (MERGED `46cc61f7`).** RAPTOR's `Trip[]` can't go
0xFE at compile time because it's store-then-push — but `frozen(v)` is the missing *signal*: build naturally
with `push`, then `freeze` when done. `frozen()` already deep-immortalizes (RC-suppresses) the graph; it now
also **repacks 0xFD pointer-spine record arrays → 0xFE inline** (allocate a headerless buffer, copy payloads,
swap `elem_tag→0xFE`, free the old spine — sound because the frozen contract forbids post-freeze mutation).
One user-driven primitive unifies inline-layout + RC-elimination, sidestepping the store-then-push hazard by
construction. **This is the lever for the typed-port memory gap** — `frozen(...)` the loadGTFS return and the
`Trip[]`/`StopTime[]` arrays go inline (~48 B/record saved). A `std/arena` bump-allocator (`arena.build(thunk)`
→ thread-local bump-alloc with immortal-RC) was also prototyped but **not merged**. The measurement: a full
arena would save **~15–18 % / 3.5–4.2 GB** of the 23 GB by removing the 16 B malloc header per object — but
**representation is the bigger lever, not the arena**: 0xFE/columnar/freeze-repack remove the *objects*, the
arena only removes their per-object *tax* (Node holds the same data in 2–4 GB, a 6–10× gap that dwarfs the
arena's 17 %). And `frozen` already delivers the arena's RC-churn-elimination subset for free, with zero new
machinery. So `frozen` covers the shippable program-lifetime case; the bump-arena spike is parked on
`explore/arena` as a complementary follow-up *after* representation, not before.

**Columnar (struct-of-arrays) record arrays (MERGED `20876032`).** Beyond 0xFE's array-of-structs: a `0xFC`
columnar array stores each field in its own contiguous buffer (`dep[]`, `arr[]`, `stop[]`) instead of
interleaved records. The win is **field-at-a-time scans** — RAPTOR's hot loop scans `trip.dep` across all
trips of a route, and on AoS (0xFE, stride 24 B) each cache line loads ~2–3 elements with the unused `arr`/
`stop` fields, ≈3× wasted bandwidth; SoA loads only `dep`. Escape-analysis-gated like 0xFE (read-only,
non-aliased), field-get = two-ptr-load + GEP + scalar-load. Verified sound + RAPTOR-digest-exact. **But it
fires on nothing today** (RAPTOR's arrays are store-then-push → 0xFD; no benchmark opts in) — Phase-2
(push-scatter fusion) + Phase-3 (`@columnar` on the port's `Trip` type) + a field-scan measurement remain.

**Honest scoreboard for the memory work.** value-unbox, 0xFE, freeze-repack, and columnar are all **merged,
verified, and neutral** — but they **fire on nothing in the current benchmarks** (RAPTOR calls `frozen()` 0
times; produces 0 columnar arrays). They are *enabling infrastructure*; their measured impact is **0 until the
typed port uses `frozen()` + build-then-store**. The only *measured* perf wins this session are on interp
(Cursor-sealing 1.66 M map_gets→0; leak fix 34 MB→424 B). The decisive next experiment is getting
`lin-manually-typed` compiling and measured with `frozen()` applied — that tells us whether this infrastructure
actually closes the 25 GB gap, and it's the natural test because that one change exercises value-unbox +
freeze-repack + RC-suppression at once.

**Parked / decided (2026-06):**
- **interp-D (stack-alloc heap-field records)** — sound but 0%: the interp's `Cursor`/`Token` are returned up
  the parse chain (escape the frame), so 0 stack allocas fire. The interp alloc lever is arena/region (those
  records are parse-lifetime), i.e. the `freeze`/region direction — not stack. On `explore/interpd`, not pursuing.
- **inline SSO** (`explore/sso`, design + spike only) — ≤15 B strings inline in the value word, eliminating the
  alloc for the ≤7 B strings that are 100 % of interp/dijkstra hot strings. **Deferred behind the value-record
  repr reset**: same "guard every consumer" fragility as SMI (every `LinString` consumer must branch
  inline-vs-heap), and it tangles with the string-field layout inside sealed structs.
- **mimalloc default allocator** — left **opt-in (default-off)**. It's ~10 % RSS but **−3–5 % wall-clock** and a
  build dependency, and Wave M proved it does NOT fix the RAPTOR peak (glibc ≈ arena-max ≈ mimalloc at peak —
  the 25 GB is genuinely live, not fragmentation). Not worth flipping for a memory-for-speed trade that doesn't
  even address the real problem.
- **SMI** — dropped/stripped (above); on `reference/smi`. **Header compaction 24→16 B**, **B2 tag-walker
  unification** — won't-do (subsumed / low-value). **#8 Float32 sealed size divergence** — fixed (`NKIND_FLOAT32`).

**Still-open ideas (unscoped):** multi-core parallel RAPTOR queries (the 24 GROUP + 5 RANGE queries are
independent — fan out via the existing worker/async; speed not memory); broaden the benchmark suite beyond
RAPTOR (dijkstra/pipeline/parallel cells) with CI regression tracking.

---

### 5.8 The 2026-06-21 session — the data layer is EXHAUSTED; the gap is the call/value axis (DEFINITIVE)

> **⚠ SUPERSEDED IN PART by §5.9 (2026-06-22).** This section's headline — "data layer exhausted,
> ≤16 %, do not propose data-layer tweaks again" — and its cycle attribution were based on a **debug
> rdtsc profile** and **debug-runtime A/B**, which (per the §2 banner) inflate runtime-call costs and
> mis-rank wall-time. Re-measured in **release**, a *data-layer* change — caching the string hash so
> string-keyed `map_get`/`map_set` skip re-hashing — cut RAPTOR **−25 %** (the single biggest lever
> found), and a combined fix landed **−29 %**. The "9.5 % map_get" / "85 % call-value" split below is
> a debug artifact; the string-key map work was the dominant *wall* cost. The §5.8 *mechanisms*
> (representation reset, keep-packed, etc.) remain correct and merged; only the "exhausted /
> data-layer-is-hopeless" *conclusion* is wrong. See §5.9.

This session removed, one at a time, **every** remaining data/representation-layer cost on RAPTOR — and
*also* copied the Go port's data design — and **none of it moved the wall clock** (~82–91 s throughout,
all digest-exact `group=26203913 range=773022892 journeys=139`). That is not a string of failures; taken
together it is **proof**, and it closes the question of where the Go gap lives.

**Why removing real costs changes nothing — the authoritative attribution.** The §5.6 rdtsc *cycle*
profile is the wall-time truth; callgrind *instruction* counts mislead here (cheap pipelined arithmetic
shows large instruction share but tiny cycle share). Cycle shares: `lin_map_get` 9.5%, field lookups 3.7%,
alloc/box-unbox/ptr-chase/`tagged_eq` ~2%, typed arithmetic 0% — **≤ ~16% total.** The other **~85% is the
call/value axis: closure + loop dispatch, control flow** (plus Lin's per-iteration safe-access null/bounds/
union checks that Go does not pay). Map probes are 9.5%, representation ≤4%. So no data-layer lever can
exceed single digits, and each one we shipped lands exactly there.

**The wins that shipped this session (all merged, digest-exact, `cargo test --workspace` green — and all
≤ single-digit %, confirming the ≤16% ceiling):**

| Commit | Change | Measured |
|--------|--------|----------|
| `d2bf74c6` | **fix(codegen): NullableRecord vs null compared its refcount byte as a tag.** A `T \| Null` record (raw struct ptr or null, e.g. `getTrip`'s `Trip \| Null`) compared via `lin_tagged_eq`, which reads the first byte as a `TaggedVal` tag — but a `NullableRecord` is a raw struct pointer, so it read the struct's **refcount**. A `frozen()` record's rc is `IMMORTAL_RC = 0x80000000`, whose low byte `0x00 == TAG_NULL`, so `trip != null` wrongly returned false → the scan never boarded a frozen trip → 0 journeys. **Latent soundness bug for ANY `T\|Null` record whose rc low-byte collides with a tag.** Fix: lower `NullableRecord` Eq/NotEq to a pointer-null check / null-guarded boxing, not `lin_tagged_eq`. | correctness keystone |
| `8bd68e34` | `lin_rc_release` was missing the `IMMORTAL_RC` guard that `lin_rc_retain`/`lin_sealed_release` have — a frozen array/object released via that path drifted out of immortal range → eventual UAF. | latent UAF closed |
| `ae9b0775` | The `LITERAL_CACHE` (string-literal intern) used SipHash; swapped to an FxHash-style mix. callgrind put `lin_string_literal` at ~13–19% of *instructions* (~453 M calls/run). | **PREP ~12%, total ~5%** |
| `531ec933` | Emit each string literal as a constant immortal `LinString` global in rodata (matching `#[repr(C)] LinString`); use a pointer to it — eliminates the `lin_string_literal` runtime call entirely. Needs a `freeze_string` guard (`rc < IMMORTAL_RC` before writing — rodata is read-only). Supersedes the fast-hasher path. | **PREP ~13%, total ~4%** |
| `9b722eb1` | **keep records PACKED into mixed-union value slots (`TAG_RECORD`).** RAPTOR's `setTrip` (~623 M iters) stored `[trip,…]` into a `Connection \| Transfer` union value via the `is_union_type` coerce arm, which O(n) `sealed_materialize_to_map`-ed the **whole Trip graph per iteration** (~3 `lin_map_alloc` + ~12 `lin_box_*` + ~12 `lin_map_set`). Route it through O(1) `lin_box_record` (`TAG_RECORD`), gated by `union_keep_packed_record_safe` (fires only when every object-shaped variant is a sealed record and no read-back reads it as a raw `LinMap*`). `setTrip` materialization 42→0 refs. | **WALL-NEUTRAL + RSS-NEUTRAL** |

The last row is the clincher: deleting the single biggest per-iteration instruction *and* allocation cost
in the hottest loop in the program moved **neither the clock nor peak RSS** (~82 s and ~6.4 GB either way).
That is only possible if the CPU was stalled elsewhere — the call/value axis — the whole time.

**Closed experiments (do not re-run):**
- **"Copy the Go data approach."** Go does NOT int-index — it uses string-keyed maps too
  (`Interchange = map[StopID]Time`, inline scalar values), and Lin already matches that model
  (value-unbox makes Lin's `{StopId:Time}` slots inline; `bestArrival` reads with no unbox). The one
  structural difference is that Go's `Connection` is a single struct with a nullable `Transfer` field
  (`IsTransfer() = Transfer != nil`), not a `Connection | Transfer` union. Collapsing the Lin union into
  one nullable-field record (the faithful Go shape) was digest-exact and tests-green but **wall-NEUTRAL
  (82 s)** and re-introduced the materialization keep-packed had removed → **discarded.** Data layout is
  not the differentiator.
- **Multi-core parallel queries** (the §5.7 "still-open idea"). The 24 GROUP + 5 RANGE queries are
  independent; fanning them across `async`/workers (128 cores) over the `frozen()`-shared index gave
  **GROUP 19 s→0.87 s (22×), total ~91 s→~31 s (~3×), digest-exact** — the ONLY large speedup found.
  BUT: it needs the whole query graph `frozen()` (else cross-thread non-atomic RC corrupts the heap), and
  a 20-run hardening pass showed **15/20** — a real intermittent cross-thread race, not production-ready.
  And it parallelizes only the Lin bench while the other ports run sequentially, so it breaks the
  like-for-like cross-language comparison. **Declined for benchmark fairness + race-robustness**; recorded
  as the genuine but off-table lever.
- **`frozen()` on the RAPTOR index.** Perf-neutral (§5.7 said so; re-confirmed). Surfaced + fixed two real
  `frozen()` crash bugs along the way (int-keyed map keys deref'd as `LinString*` → segfault; the 0xFD→0xFE
  repack freed *shared* element shells → heap corruption) and the `d2bf74c6` keystone above.

**What it would actually take to close the Go gap (the lever is the EXECUTION MODEL, not the data layer):**
1. **The combinator loop-flattening is ALREADY done** (re-measured 2026-06-22 — see the update below). The
   RAPTOR hot loop `range(a,b).for(pi => …)` — a *capturing* literal lambda over mutable `var`s, outer `val`s,
   and params — already inlines its whole body into a flat native loop (`for_header`/`for_body`/`for_latch`,
   no closure alloc, no per-element indirect dispatch), verified by reading the emitted IR. The per-stop
   helpers (`previousArrival`/`bestArrival`/`setTrip`/`getTrip`) are *direct* monomorphic calls, not indirect
   dispatch. So the loop is **already structurally flat**; what remains un-folded is the **boxed value
   representation crossing those direct call boundaries** (e.g. a `Trip | Null` return materializing a record)
   + RC churn + the per-iteration safe-access null/bounds/union checks. That is exactly why LTO/cross-module
   inlining showed **no speedup** — the calls were never the overhead. The genuinely-remaining execution-model
   lever is *not* "flatten the scan" (done) but **eliminating the per-iteration boxed-value/RC/safe-access work
   inside the already-flat loop** — value-flowing across call boundaries unboxed, and proving away the safe
   checks — which is a harder, narrower frontier than "monomorphize + flatten" implied.
2. **A JIT** (the V8 route) — keep full dynamism, specialize map/field/closure access at runtime. A
   different backend, multi-quarter.
3. Int-indexed contiguous arrays (cache-friendly, no hashing) — but per the cycle profile this only attacks
   the 9.5% map bucket, and it changes the data model the other ports share (off-table for fairness).

**Update (2026-06-22) — the loop-flattening is already implemented; one narrow detector gap remains.**
Acting on lever #1 above, the first step was to *verify* the premise rather than assume it. Reproducing the
exact hot-loop shape (a `range().for` whose capturing lambda mutates `var`s and calls direct helpers) and
reading the unoptimized IR showed the lambda body is **already spliced inline into a flat counted loop** — the
"whole-program monomorphization + loop-flattening" frontier as stated is, for this shape, **already done**
(mature combinator inline/devirt/fusion machinery: ADR-044 literal-lambda inline, the capturing-lambda inline
gate, Wave C/D devirt+fusion). This *confirms* the §5.8 thesis from the other direction: with the loop already
flat and the calls already direct, the residual gap can only be the value-representation/RC/safe-access work
inside it — not loop structure. The honest correction to lever #1: the flattening is shipped; do not scope a
project to "make the scan a flat loop."

The IR did expose **one real, un-flattened thing** — a narrow detector gap, not a general frontier:
`range(a,b).for(f)` is *supposed* to fuse into a native `i32` register-counter loop (`lower_range_for`), but
the detector `range_for_bounds` only recognises the `range` callee via `intrinsic_slots`/`import_fn_slots` —
and after monomorphization the 2-arg `range` (a thin `lin_range` wrapper) is a **rehomed `global_fn_slots`
spec** (`std_iter_range$Int32_Int32_NN`) that neither bucket holds. So fusion **silently never fires in real
code** (even trivial `range(0,10).for(print)`): every range loop iterates a heap-materialised *tagged* array
with per-element `lin_array_get_tagged` + `lin_unbox_int32` instead of a flat counter — including RAPTOR's
hottest inner loop. The *same* `global_fn_slots` gap exists in a second detector, `is_provably_flat_producer`
(an explicit `// not a flat producer we trust here` no-op), which de-optimises the element reads of **every
lambda-free producer spec** — empirically `range`, `range`-with-step, `arrayAllocate`, `arrayAllocateFilled`
all iterate via tagged-get+unbox; only literal-array + lambda combinators (`[…].map(f)`, which inline to
intrinsics) stay flat. The fix is mechanical and sound — consult the existing `spec_origins`-gated
`combinator_spec_slots` tag (already used for `flatMap`/`some`/`every`/`find`) for the `global_fn_slots` case
in both detectors. **In flight on `perf/range-for-fusion-fix`; RAPTOR wall + digest measurement pending.**
Honest expectation, set by this whole campaign: counter-fusion deletes a heap alloc and the per-iteration
index box/unbox and lets LLVM see a real induction variable — but the *value* work in the loop body is
untouched, so per the ≤16% data-layer ceiling and the keep-packed wall-neutral result it **may well land
wall-neutral on RAPTOR too**. It is worth shipping regardless (it changes the loop's fundamental shape and is
a clean general win for all range loops), and its RAPTOR delta is one more direct probe of the call/value
ceiling.

**Bottom line.** The data-layer optimization well is **dry**: representation, allocation, RC, GC,
materialization, literals, hashing, map-value-boxing, and Go-style data layout are all measured ≤ ~16% and
individually wall-neutral. Closing the 4–5× gap is an **execution-model project** — and (per the 2026-06-22
update) *not* "flatten the scan", which is already done: it is **unboxing values across the already-direct
call boundaries + eliding the per-iteration safe-access checks** in the already-flat hot loop, or a JIT.
Not another patch. Do not propose data-layer tweaks as the RAPTOR speedup again.

---

### 5.9 The "take-five" campaign (2026-06-22) — debug fooled us, release found the real lever (RAPTOR −29 %)

This is the consolidated record of the `project-performance-take-five.md` campaign (that doc is now
deleted; everything load-bearing is here). It set out to close the "call/value axis" §5.8 named, ran
14 lanes via parallel agents, **failed on a measurement mistake, then succeeded once measured
correctly** — and in doing so overturned §5.8's "data layer is exhausted" verdict.

**Act 1 — the debug-measurement mistake.** 14 lanes (internalization, bounds-check elision + IRCE,
RC-op inlining, `map_get` inlining, `alwaysinline`, devirt generalization, LSS-v1, scalar-CPR,
bitcode-runtime, PGO wiring) were built, verified, and A/B'd — all with `target/debug/lin`. The debug
runtime (unoptimized `liblin_runtime.a`) makes runtime-calls ~8× costlier, so the "inline/elide a
runtime-call" lanes looked huge: "wave-2" measured a stable **−18 % on RAPTOR**. Re-measured in
**release** (release compiler + release runtime, the `compare.sh` configuration), that −18 % was a
**+2.5 % regression** — the debug A/B had measured a cost profile that does not exist in production.
The release per-phase chain (min, same-batch): pre-campaign 80 s → batch-1 (internalization/devirt/
entries) **77 s (−4 %, a genuine win)** → +wave-2 **81 s (+5 %, wipes it)** → +phase-2 **82 s**. The
"inline a cheap release-runtime op" lanes (`alwaysinline`, RC-inline, `map_get`-inline) removed the
*call*, not the *work*, and their added code slightly regressed release.

**Act 2 — profile in release, find the truth.** `perf` is unavailable in this container (no
`CAP_PERFMON`) and `valgrind` SIGILLs on the AVX-512 codegen, so the release binary was profiled with
a **sudo-gdb stack-sampler** (300 samples). Self-time, with the IR confirming each:

| release self-time | bucket | mechanism |
|---|--:|---|
| **~30 %** | string-keyed map work | `lin_map_get`/`lin_map_set` + `lin_string_eq` + `memcmp` (every probe hashes a `StopId`/`RouteId` string and compares string keys) |
| **~40 %** | algorithm in **boxed-closure wrappers** | `std_iter_whileLoop` (the dominant loop) calls its body via an **indirect `call ptr %fnp`** from a heap closure, boxed-`bool` return unboxed per iteration, per-iter `lin_closure_release` |
| **~11 %** | RC | `retain_sealed_payload_fields`, releases, `tagged_clone` |
| **~10 %** | allocation + `getTrip` record materialization | `_int_malloc`, `lin_sealed_alloc` |

**Act 3 — fix it, measured in release from the start.** Three lanes, each targeting a profiled
hotspot, baseline `c46e2379`, same-batch, min of 3, digest-exact:

| lever | what | release RAPTOR | Δ |
|---|---|--:|--:|
| baseline | — | 76 s | — |
| **STR-KEY** | cache the FNV-1a hash in the `LinString` header; codegen loads it, skips re-hash + hash-gates `memcmp` | **57 s** | **−25 %** |
| CLOS-DEVIRT | inline the boxed `while(() => …)` loop into a direct loop (no `std_iter_whileLoop`, no indirect call) | 75 s | −1 % |
| REC-CPR | `is T` on a `Record \| Null` → pointer-null check, not box + `lin_matches_schema` | 75 s | −1 % |
| **COMBO** | all three | **54 s** | **−29 %** |

Cross-language (release, same machine, digest-exact `26203913/773022892/139`): **Go ~18.7 s, Node
~29.4 s, Lin 76 s → 54 s** — the gap closes **4.1×→2.9× vs Go and 2.6×→1.8× vs Node.** (Go/Node use
string-keyed maps too, so the cached-hash win is like-for-like.)

**The two findings that matter:**

1. **Self-time ≠ wall-time lever — and it inverted the priority.** The boxed-closure dispatch was
   **40 % of self-time** but removing it bought **−1 % wall** (the indirect call is well-predicted and
   overlaps memory stalls). String-key map work was **30 % of self-time** but cutting it bought
   **−25 % wall** (the hash + `string_eq` *serializes* the hot loop). Where the PC sits is not where
   the wall-time is. This is also why §5.8's debug "inline `map_get`" (RT.2c) was neutral — it removed
   the call; STR-KEY removed the **work**.

2. **§5.8's "data layer is exhausted" was wrong, for the §2 reason.** A data-layer change (string-key
   hashing) was the **single biggest releasable lever (−25 %)**. §5.8's debug rdtsc profile ranked
   `map_get` at 9.5 % and declared the data layer dead; release ranks it first. Do **not** trust the
   "≤16 % / call-value-axis-only" framing — it was a debug artifact.

**Durable process lessons (the campaign's real ROI):**
- **All perf A/B must be release** (release compiler **and** release runtime). Debug inverts the
  signal for anything touching runtime-call overhead. (§2 banner.)
- **Differential stdout probes** (compile+run the same program on master vs the change, diff output)
  caught **three soundness bugs** that green test suites + RC-verify + ASan all missed — all in `is`/
  null fast-path lowering: (a) `is`-elision used `is_compatible`, treating `Int32`→`Int64`/`Float64`
  as widening-compatible, so `(x:Int32) is Int64` folded to `true`; (b) `is T` on `R | Null` lowered
  to a pure non-null check, so `(Trip|Null) is Other` reported a non-null `Trip` as `Other`. `is`/
  narrowing fast paths are a soundness minefield — keep a permanent `is`-on-`T|Null` differential.
- **Build the integration; never trust a clean cherry-pick.** Two lanes with zero textual conflict
  failed to compile together (one added an IR struct field the other's new sites didn't set).
- **Same-batch interleaved, min of ≥3.** Cross-batch single-pair comparisons at RAPTOR's noise floor
  (~7 % debug / ~1 % release) produced two false-regression alarms (machine drift, e.g. the recorded
  a537b8c8 interp 94 ms measured 107 ms on a busier day for the *identical* commit).
- **Agent self-reports are not trustworthy** (cf. §5.7): every lane self-reported green; the orchestrator
  re-ran every gate and the differential, which is what caught the soundness bugs.

---

### 5.10 The 2026-06-23 session — records aren't maps; the wall is reference-record memory latency

Continuing §5.9's RAPTOR work (which ended at 54 s), this session merged one more real lever
(RC-ELIDE, −9 %), a batch of correctness + general-quality changes, and — driven by the question
"*why* is Lin still ~2.5× Go" — produced the **definitive structural diagnosis** of the residual gap.

**Merged (master, release-verified, digest-exact `26203913/773022892/139`):**

| change | effect | mechanism |
|---|--:|---|
| **RC-ELIDE** | **−9 %** (54→49 s) | de-materialize *view-only* sealed-array element binds (`val s = arr[i]` used only for field reads → borrowed const-offset reads, no `lin_sealed_alloc`/`retain_sealed_payload_fields`). The one real perf win — removes *serializing work*, like §5.9's STR-KEY. |
| SwissTable | general; RAPTOR-neutral | control-byte (h2) probe for `LinMap`, Go-grade map quality. ~17 % on a hot small-map microbench; **0 % on RAPTOR** (cold random-slot miss, not probe length). |
| utf8-fast | ~2 % LOAD | drop the per-CSV-field `from_utf8_lossy` double-alloc (2→1 alloc/field). |
| SEAL-FIELD | **correctness** | `m[k]["field"]` on `{String:Record}` returned **null** (NullableRecord map-value field read mis-lowered) → offset `FieldGet`. |
| coerce-fin | **correctness** | `coerce_if_branch` concrete-concrete arm leaked a ref on narrowed-union `if`-branch results (`owned=false` → orphaned retain → u32 wrap → UAF at scale). |
| BOUND-ELEM | neutral, cleaner | escaping sealed-array element field reads → offset (getTrip `map_get` 48→28). |
| FROZEN-RC | neutral, sound | immortal early-out in `retain_sealed_payload_fields` — skip the deep RC walk for frozen graphs. |
| stride-spec | general | fused `arr[i]["heapField"]` → inlined `SealedArrayFieldGet` instead of materialize. |

**The structural diagnosis (the session's real output).** The recurring question — *are records secretly
maps?* — was **answered no, by direct IR probing.** Typed-record field access compiles to a constant-offset
load for **direct, param, fused, and view-only** binds (all 0 `map_get`). The `map_get` fallbacks were two
**narrow boundary cases** — a record retrieved from a map (`m[k]["f"]`) and an *escaping* bound element
(`val e=arr[i]; …; saved=e`) — both now fixed. Records and maps share `Type::Object{sealed}` at the type
level, but the *representation* is already right: sealed records are packed structs with offset access; the
remaining `map_get`s in RAPTOR are *genuine collection lookups* (`{StopId:V}` maps).

**So the Go gap is the DATA LAYOUT, not the access mechanism.** Records are **reference types stored as
0xFD pointer-spines** — a `Trip[]` is an array of pointers to separately-heap-allocated `Trip`s scattered
across the heap; Go uses inline value structs (`[]Trip`, contiguous). getTrip's backward scan pointer-chases
a cache-missing struct per element. `frozen()` **does** de-scatter at runtime (measured:
`repacked_0xfd_arrays=242744`, `freed_struct_shells=2372709` — the trip data physically becomes 0xFE
contiguous) — **but codegen can't exploit it**: `repr.rs:599/638/654` stamp every map-/field-read array
`inline:false`, so element access is a **per-element runtime call** (`lin_sealed_array_elem_ptr` /
`lin_array_get_tagged`), not an inlined `base + i*elem_stride` loop the CPU can stride/prefetch.

**Every access-mechanism lever is wall-neutral — proof, from ~8 angles.** SwissTable, INTERN, typed-get,
SEAL-FIELD, BOUND-ELEM (getTrip 48→28 `map_get`), reduce-devirt, getTrip-hoist, FROZEN-RC, stride-spec are
**all wall-neutral on RAPTOR.** Eliminating 20 `map_get`s from the hottest function moved the clock **0 %**.
The `map_get`/RC machinery is *overlapped* with the cache-miss load of the scattered data; the load is the
cost. The only levers that ever moved the wall (RC-ELIDE here, STR-KEY/CLOS combo in §5.9) removed
*serializing work + allocations*. This **refines §5.8/§5.9**: not "data layer exhausted" (§5.8, wrong) nor
"string-key hashing" alone (§5.9), but specifically **memory latency on heap-scattered reference records** —
the layout, the one axis left untouched.

**The lever that hits it — and why it's hard.** `lazy-materialize-at-escape`: keep an escaping bound element
a *borrowed view* (offset reads), materialize the copy only at the escape site (N reads + 1 copy, not N
copies). A microbench isolates it: an escaping scan = 4 s, the same scan view-only = ~0 s; a port proxy
(store the *index*, re-read once) gave RAPTOR **GROUP −4.3 %, digest-exact** — RC-ELIDE-shaped, the first
non-neutral signal since RC-ELIDE. **But the sound compiler realization is a UAF minefield** — two agent
attempts (a `bench.lin` build-panic, then a runtime segfault in PREP): borrowing array elements across a
loop with conditional escape is exactly the use-after-free class. **The safe form needs
records-as-value-types-stored-inline** (the §5.6 representation-reset direction), not a point
borrow-across-escape pass. Shelved on `take5/lazymat*`.

**Conclusion / the fork.** The per-op levers are exhausted (all neutral); the next real RAPTOR gain is the
**value-struct inline layout** — `frozen()` already produces the contiguous runtime buffer; what's missing
is codegen *striding* it (a static `frozen`/0xFE type so the scan emits an inlined `base+i*stride` loop, not
per-element calls). That is the reset campaign (records = value types), a deliberate larger effort — or
accept the **~2.5× Go floor** as the cost of reference-record semantics. Either way the point-lever well for
RAPTOR is dry. Arc across §5.9+§5.10: **~80 s → ~46–49 s; 4.1×→~2.5× Go, 2.6×→~1.6× Node.**

**Process lesson (now a gate).** **Seven** lanes this session passed their full unit gate (`cargo test` +
`LIN_VERIFY_RC` + ASan on small repros) but **failed on RAPTOR** — neutral-ineffective, build-panic, or
runtime-segfault. The cheap catch: **`lin build <raptor>/bench.lin` (build-only, no run)** is now a
mandatory gate for any codegen/lowering lane — it caught the lazy-mat codegen panic the unit suite missed.
(Reinforces §5.7/§5.9: "tests pass" is necessary, not sufficient; RAPTOR is the oracle.)

### 5.11 Stage 5 — locking the record-never-a-map invariant (2026-06-23)

**What was fixed.** The representation-reset campaign (Stages 1–4c, see §5.6) made it *structurally impossible* to represent a sealed record as a string-keyed `LinMap` for most call sites — but one leaf remained: `lin_sealed_any_to_tagged`'s 0xFE inline arm still built a `LinMap` per element via descriptor-walk, then stored it as `TAG_MAP`. Stage 5 fixed this last site (delegate to `sealed_elem_payload_to_record_box`, same as `lin_array_get_tagged`'s 0xFE arm) and added enforcement so no future change can silently regress.

**What changed in measured terms.** On the manually-typed RAPTOR bench (release, digest-exact `26203913/773022892/139`), `LIN_VERIFY_REPR` conversion counts show:

| site | before | after |
|---|--:|--:|
| `lin_sealed_ptr_array_to_tagged` | ~28 M | ~0 |
| `lin_sealed_any_to_tagged` | ~23 M | ~0 |
| `dynamic_to_map` + `lin_union_force_to_map` | ~0.4 M | ~2.4 M |
| **TOTAL** | **~51.9 M** | **~2.4 M (~95% reduction)** |

Wall-clock impact (release, medians):

| phase | before | after | change |
|---|--:|--:|--:|
| PREP | ~6.9 s | ~6.0 s | **−13 %** |
| GROUP | ~9.9 s | ~9.0 s | **−9 %** |
| RANGE | ~27.5 s | ~26.3 s | **−4 %** |

**Honest reconciliation with §5.10.** Section 5.10 predicted that access-mechanism fixes would be wall-neutral. This *was* a real, if modest, lever — but it belongs to a different cost class. The cost removed is *materialization allocation*: per-record `LinMap` builds (alloc + key-intern + field-insert per element). This is the same alloc-elimination class as RC-ELIDE (§5.10) and STR-KEY (§5.9), not pure access-mechanism. PREP (the construction-heavy phase) benefits most because it iterates the entire dataset and builds collections. RANGE benefits least because its bottleneck is the pointer-chasing scatter load (§5.10), not allocations.

The remaining 2.4 M conversions are legitimate: `dynamic_to_map` and `lin_union_force_to_map` are called on genuine `{String:T}` map values and `AnyVal` blobs (union display/json paths), not on records.

**The design win, independent of perf.** "A record is sometimes a map at runtime" is now impossible by construction. The type-system invariant (sealed record ↔ TAG_RECORD, dictionary ↔ TAG_MAP) is enforced at three layers: the structural fix (the last TAG_MAP-producing leaf eliminated), `debug_assert` guards in every array-converter arm (fires in debug/ASan on any regression), and the `repr_invariant_tests` CI gate (three tests covering 0xFD, 0xFE, and nested-record paths — verified live by sabotage). See ADR-088.

---

### 5.12 The 2026-06-23 (pm) session — string keys aren't the problem, *our strings* were (RAPTOR −33%)

This session reopened the RAPTOR gap with one question: **Go and Node both key their maps by string and are
fast — so "string maps are slow" is wrong. Why are *their* string lookups fast and ours weren't?** The
answer, and the three levers that followed, took the manually-typed RAPTOR bench from ~46 s → ~33 s
(**~1.76× Go, ~1.1× Node**, from 2.5×/1.6× at session start; ~80 s → ~33 s ≈ **−59%** cumulative across the
whole effort). All numbers are release, digest-exact `26203913/773022892/139`, same-batch interleaved A/B.

**The keystone — data-string interning (−22%).** Per-lookup, the three runtimes do very different work on
the *key*:
- **Go**: `string` is a weightless value `{ptr,len}` — no header, no refcount; the map hashes bytes + `memcmp`s, but carrying the key costs nothing.
- **V8**: strings are **interned** — equal strings are the *same object*, so a lookup is cached-hash + **pointer-identity** key compare (no `memcmp`), and no per-use allocation.
- **Lin (before)**: `LinString` is a heap-allocated, **refcounted, non-interned** object (literals were interned; CSV-parsed feed strings each got a fresh `lin_string_alloc`). So the map's stored key and the lookup key were *different objects with the same bytes* → `lin_string_eq`'s `a == b` fast-path missed → a **`memcmp` on every probe** (`LIN_MAP_PROFILE`: `key_eq_calls ≈ 273 M`, all matching), plus a header cache-miss and refcount traffic per use.

The fix interns feed strings at CSV-parse time (a lock-free `thread_local` table; interned entries are
immortal) so equal `StopId`/`RouteId`s become *the same object*. Then `lin_string_eq` short-circuits on
pointer identity → the 273 M `memcmp`s vanish; `string_eq`/`memcmp` left the profile entirely (was ~17% of
RANGE). **Faithful — string keys stay string keys.** GROUP 9302→7077 ms.

> **Lesson — a parked "negative" can be an implementation artifact.** A prior `experiment/string-intern`
> measured only ~3-4% and was shelved; I nearly trusted that and skipped the spike. It used a **global
> Mutex** (a lock per intern lookup) that ate the win. The lock-free `thread_local` version on the live
> baseline pays **−22%**. Re-measure shelved ideas on the current head, with a clean implementation.

**Read-only RC elision (−8-12%).** With the string cost gone, RC became the biggest RANGE category (~25%).
Extending RC-ELIDE's borrow analysis to **map-get / call results consumed read-only** (compare/arith,
non-escaping) elides the owning `CloneBox`/`Release` — keep the borrow (`lin_map_get` already returns a
borrowed `*TaggedVal`). RAPTOR clones 211→99 (−53%). GROUP 6971→6124 ms. Same UAF discipline as RC-ELIDE
(digest-exact + ASan + `LIN_VERIFY_RC`); elide only when provably read-before-mutation + non-escaping.

**Closure-devirt of reduce-into-map (−3.5%).** `getQueue`'s `markedStops.reduce({}, …)` dispatched its body
through a boxed closure per element. Inlining it (raw-ptr accumulator through a phi loop, identity-aware
`ReleaseRawIfDistinct` for the per-iteration RC) removed the indirect dispatch. **This is the same shape
that heap-corrupted RAPTOR in an earlier reduce-devirt attempt** — done soundly this time by handling the
accumulator's ownership across iterations; bench-RUN + valgrind clean.

**The grind convergence — serializing vs overlapped (the definitive cut line).** After the three wins, a
fan-out of spikes on every remaining residual returned **five straight neutrals**: per-query map seeding
(`fromKeys` fusion), LOAD (column-skip — already merged by another agent), PREP (sort-by-key — phase already
~2.8 s), box/unbox at null-check sites (§5.8), and the scan's map-writes / `map_keys` iteration. The rule is
now sharp and predictive: **a lever moves the wall iff it removes *serializing* work on the hot path**
(string `memcmp`, owning clones, indirect dispatch — all merged). **Overlapped work is wall-neutral no
matter its self-time** — allocation (~17% self-time), boxing, per-query setup, map-iteration all profile
high but sit off the critical path. This is §5.10's self-time≠wall-time, now with a clean mechanism test.

> **Measurement gotchas banked this session.** (1) The `sudo gdb` phase sampler *inflates* per-phase
> wall-ms ~2-3× (it pauses the process per sample) — its phase *ms* are unusable for phase-share claims;
> only its sample *distribution* is valid. Use un-sampled clean runs for phase shares. (2) Master moves
> under you (many concurrent agents) — a profile goes stale in minutes; re-profile on the true current head
> before scoping. My "LOAD+PREP are 57% of the wall" was wrong on both counts (stale *and* gdb-inflated);
> the truth is RANGE ≈ 60% (query-bound), as it always was.

**Two structural lanes — and both paid off.** getTrip was ~32% of RANGE, dominated by record-field reads
still lowering to `lin_map_get` instead of offset loads (keys: `RouteScanner`/`StopTime`/`Service` field
names). A diagnosis lane (`recoffset`) split the cause cleanly:

- **`RouteScanner` wouldn't seal** because its `dow: DayOfWeek` field is a **pure-IntLit union** (`0|…|6`),
  and union fields are stored as heap `TaggedVal*`, which forces the whole record unsealed → ~24 of
  getTrip's `map_get`s were scanner fields. The fix (`Type::IntUnion` — represent a small pure-IntLit union
  as an **i32 sealed scalar**, boxing to `TaggedVal*` only at generic-Union call boundaries) makes
  `RouteScanner` seal (`dow@36` becomes a 4-byte i32 slot) and turns those reads into const-offset GEPs.
  **−12% GROUP** (6139→5414, digest-exact). Contained to the type/codegen layers; the one excluded-file
  touch (`boxing.rs`) is a clean *additive* IntLit-union→`TAG_INT32` guard (flagged for repr-agent merge).
  *Map-typed fields like `tripsByRoute` were never the blocker — they seal fine as a heap ptr slot.* The
  `StopTime`/`Service` field reads are a **separate** cause (frozen-array element materialization) and
  remain the repr-agent's domain (§5.11).
- **The inner-array pointer-chase** (§5.10): `routeTrips` is a 0xFD pointer-spine array (cache-miss per
  element via a `lin_sealed_ptr_array_get_ptr` *call*). Full 0xFE inline repack is **blocked — by shared
  ownership, not heterogeneity** (`Trip` is uniform 56 B): after `frozen(tripsByRoute)`, `trips[]` +
  `sortedTrips` (from `.sort()`) still hold refs, so the rc==1 exclusivity check fails and it stays 0xFD;
  repacking would need cross-call liveness-driven RC elision. *And* a 0xFE `val trip = arr[i]` would flip a
  1-bump retain-ptr into alloc+memcpy+4-field-retain — possibly slower for a heap-heavy struct. But the
  cheap part paid: **inlining the 0xFD pointer-spine load** (kill the per-element *call*) is **−3.3% GROUP**
  (digest-exact) — the call overhead was serializing on the hot path.

So the record-offset path is partly unlocked (scanner fields now offset; the rest awaits the repr-agent),
and the full value-layout (fixed-length `T[N]` / contiguous inline) remains the deeper open lever, gated on
the ownership-at-`frozen()` problem above. End-of-session RAPTOR ≈ **30-31 s** (~1.6× Go, ≈ Node parity).

---

## 6. Guidance for writing fast Lin

1. **Prefer typed records and `&`-composed named types over `AnyVal`.** This is the
   single biggest lever — a typed record field read is a constant-offset load;
   `AnyVal` field access is a runtime tag-dispatch + an O(1) hashed `map_get` and an
   optimization barrier the compiler can't fold to a constant-offset load (~4–70×
   slower) — not an O(n) scan (§4). `AnyVal` is for genuinely unknowable shapes only.
2. **Use `{String:T}` for dictionary data**, not an open object. It's an O(1)
   hashed `LinMap`, not an O(n) association list.
3. **Use the fusable eager combinators** (`map`/`filter`/`reduce`/`for`) in chains
   — they fuse to a single zero-allocation loop. A literal lambda (capturing or
   not) inlines; a named no-capture callback to `find`/`some`/`every`
   devirtualizes.
4. **Pass named no-capture functions** to the combinators that devirtualize them,
   rather than capturing closures, where you have the choice.
5. **Use concrete scalar arrays** (`Int32[]`, `UInt8[]`, …) — they're packed,
   unboxed, contiguous buffers; avoid the boxed `Number` union (~3.6× slower).
6. **Keep tail recursion in tail position** — it compiles to a constant-stack loop.
7. **For shared state, use `Frozen<T>`** for load-once read-only reference data
   (immortal, zero-copy, lock-free reads) and `Shared<T>` only for genuine shared
   *mutable* state.
8. **Break reference cycles manually** (null a field) — there is no cycle collector.
9. **`freeze` load-once, never-mutated data** (e.g. the return of a `loadData`/index-build function).
   `frozen(v)` deep-immortalizes the graph (retain/release become no-ops for the rest of the program — the
   LIN_NO_RC ceiling showed RC is pure overhead for program-lifetime retention) **and** repacks its 0xFD
   pointer-spine record arrays to compact 0xFE inline (~48 B/record saved). Build naturally with `push`, then
   `freeze` once you're done mutating. Only freeze genuinely read-after-this data; a frozen value is never
   reclaimed until process exit.

---

## Provenance

Synthesized from the `docs/DECISIONS.md` perf ADRs
(014/015/016/024/028/034/040/044/045/055), the `benchmarks/` + `benchmarks/compare/`
suites, and the perf/cleanup work merged through this codebase's history. All
cross-language and typed-vs-`AnyVal` numbers are measured (`compare.sh` and the RAPTOR
A/B harness); every quantitative claim is cited to its measured source. The path-n
learnings table (§5) is the consolidated record of the perf-investigation paths.
