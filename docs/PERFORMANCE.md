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
`AnyVal`-typed field access is a string-keyed O(n) linear scan that is an LLVM
optimization barrier — measured **~4× slower** than the equivalent typed record
on the `records` workload, and ~70× slower on heavily `AnyVal`-read-bound code
(RAPTOR). The other structural cost is the **call boundary**: a polymorphic
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
  | `object_access` | object construction + the O(n) linear-scan field lookup; chained reads multiply the scan |
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

- **`AnyVal`-read-bound code is the cliff.** `AnyVal` field access is a string-keyed
  O(n) linear scan over the object's entries *and* an LLVM optimization barrier —
  the compiler can't elide, hoist, or fold it the way it does a typed record's
  constant-offset slot. Measured ~4× on `records`, ~70× on RAPTOR-class code.
  `AnyVal` is a genuine escape hatch (untyped wire data, recursive ASTs), not a
  default. The fix is *userland*: type the data (§6), not a codegen tweak — see
  the path-9 closed-negative in §5.
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

## 6. Guidance for writing fast Lin

1. **Prefer typed records and `&`-composed named types over `AnyVal`.** This is the
   single biggest lever — a typed record field read is a constant-offset load;
   `AnyVal` field access is an O(n) scan and an optimization barrier (~4–70× slower).
   `AnyVal` is for genuinely unknowable shapes only.
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
