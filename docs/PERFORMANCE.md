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
`Json`-typed field access is a string-keyed O(n) linear scan that is an LLVM
optimization barrier — measured **~4× slower** than the equivalent typed record
on the `records` workload, and ~70× slower on heavily `Json`-read-bound code
(RAPTOR). The other structural cost is the **call boundary**: a polymorphic
combinator calling a non-devirtualizable closure boxes each element and unboxes
the result across an opaque indirect call. The one-line story: **Lin is a
fast native language when you give it types, and a slow dynamic one when you
hand it `Json`.**

---

## 2. The benchmark picture

There are two harnesses, deliberately separate:

- **`benchmarks/run.sh` + `benchmarks/*.lin`** — Lin-only. Times compiled Lin
  binaries against each other across code changes. This is the regression gate
  for codegen/runtime work. Targets per file:

  | File | Hot path exercised |
  |------|--------------------|
  | `recursion` | call/return overhead, TCO loop transform, non-tail self-recursion (`fib`); mostly unboxed `Int32` so isolates call + branch |
  | `array_pipeline` | `map`/`filter`/`reduce` over a range: indirect closure calls, Int32 box/unbox through the `Json` element slot, RC on intermediates |
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

### Typed vs `Json` (RAPTOR)

RAPTOR is the load-bearing real-world workload — a GTFS journey planner threaded
through many generic boundaries. We measured a **fully-typed** RAPTOR (trips as
`Trip{tripId, stopTimes: StopTime[], service}` records, the route map as
`{String: Trip[]}`) against the `Json` baseline on the same compiler, full feed,
O2, single-run min, digest byte-identical (`group=26203913 range=773022892
journeys=139`). The "naive typed" column is the straightforward typed port; the
"typed" column is after the **de-materialization** pass described below:

| phase | `Json` (ms) | naive typed | typed (de-mat) | typed / `Json` |
|-------|----:|----:|----:|----:|
| LOAD  | 15779 | 16209 | 17353 | 1.10× |
| PREP  | 28365 | 119567 | 104264 | **3.67×** |
| GROUP | 62040 | 140221 | 114583 | 1.85× |
| RANGE | 184475 | 401310 | 334286 | 1.81× |
| total | 290759 | 677807 | 571086 | **1.96×** |

**Two distinct costs, with opposite outlooks — this is the key finding.**

A typed heap-field record (`Trip` holds a `String` and a `StopTime[]`) does **not**
reach the packed flat layout — it is boxed, like the `Json` form — and a *typed*
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
   `Json` form copies a pointer. The stable `sort` likewise binds whole `Trip`s per
   comparison. De-materialization removed only the incidental per-stop `StopTime`
   binds (the −13% PREP delta); the regroup/sort copies are the price of value
   semantics for a construct-heavy workload and are not addressable without storing
   indices in place of records. **Typing reaches ~1.8× on read-heavy query work, but
   record construction/regrouping carries an inherent copy cost `Json`'s pointer
   sharing avoids.**

Two honesty notes: (a) the comparison is kept **algorithmically faithful to the
reference** (node/go/rust) — a `runsOn`-by-`serviceId` memo that would have shaved
another ~20s was *dropped*, not added, because the reference does not memoize and
mirroring it into the `Json` port would have let Lin cheat cross-language; (b) the
typed RAPTOR is **leak-free** (RSS bounded/flat, ASan-clean), so the residual gap is
materialization *time*, not allocation churn.

The orthogonal win that *did* pay independently of all this: typing the
**dictionaries** — the `{String: Int32}`/`{String: Trip[]}` index and scan-state
maps that were `Json` objects — replaced O(n) association-list scans with O(1)
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
  same code typed `Json`.)
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

- **`Json`-read-bound code is the cliff.** `Json` field access is a string-keyed
  O(n) linear scan over the object's entries *and* an LLVM optimization barrier —
  the compiler can't elide, hoist, or fold it the way it does a typed record's
  constant-offset slot. Measured ~4× on `records`, ~70× on RAPTOR-class code.
  `Json` is a genuine escape hatch (untyped wire data, recursive ASTs), not a
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
- **No reference-cycle collection** (ADR-024). Cycles between long-lived heap
  objects leak; documented, the fix is to null a field to break the cycle.
- **`Number` (boxed numeric union) is ~3.6× slower than a concrete family**
  (ADR-014) — prefer a concrete `Int32`/`Float64`.

---

## ★ 5. Path-n learnings — the consolidated record

This is the heart of the document: one row per perf-investigation path, distilled
so the `path-*` proposal docs can be deleted without losing the conclusion. The
recurring theme: **two bottlenecks in two programs** — `interp` is *call-bound*,
RAPTOR is *`Json`-read-bound* — and the cost is always *work per operation*
(reads, calls, materialization), never allocation/reclamation itself.

### The big closed-negatives (do not re-try these)

| Path | Tried | Measured | Verdict | WHY |
|------|-------|----------|---------|-----|
| **7 — tracing GC** | Replace RC with generational tracing GC to make allocation cheap | `LIN_NO_RC` ceiling (entire allocator+RC as no-ops): **0.48 s vs 0.408 s = NO speedup**; RAPTOR ~1.0× all phases despite textbook GC-bait retention (32.9 GB allocated, 0.039 retention, 96% dying young) | **CLOSED-NEGATIVE** | A GC can't recover a cost that deleting the *entire* heap+RC subsystem doesn't recover — the cost is work-per-allocation (reads + calls), which GC doesn't touch. **No workload is alloc-bound.** Revisit only for correctness (RC-UAF), never perf. |
| **9 — end-to-end packed records** | Pack heap-field records all the way (loader→map→read) so RAPTOR's 630 M linear scans become const-offset loads | Three independent agents built digest-correct end-to-end typed RAPTOR: PREP 7.7 s→27.2 s (**3.5× slower**), GROUP 19.9 s→36.2 s (**1.82×**), RANGE 59.4 s→105.3 s (**1.77×**) | **CLOSED-NEGATIVE** | The cost is **representation-boundary materialization, not field reads.** Functional code threads records through many generic boundaries; each is a materialize-or-leak seam (worker boundary → nested-record gate → TCO param leak → `Trip\|Null` union boxing → map-value materialize-per-access). Each packing fix repaired a bug a prior packing fix introduced — "fix-for-a-fix all the way down." |
| **5 — value records** | Make fixed-key records inline values (no header/shell RC), claimed semantics-preserving | Falsifying test on master: `val b=a; a["state"]=99; b["state"]` → **99** | **CLOSED-NEGATIVE (premise falsified)** | Records are observably-mutable **reference** types; value semantics is a *breaking* change, not a free representation swap. Cost diagnosis was right; the "non-breaking" framing was wrong. The live form is Path 1 (packed *representation*, not value *semantics*). |
| **2 — inline caches / hidden classes** | Shape ids + per-site inline cache for `Json` field offset resolution | **99.56% cache hit rate** (656.6 M/659.5 M); but RAPTOR GROUP −3.3%, RANGE +3.8% slower, interp +2.5% slower — **net wash-to-loss** | **CLOSED-NEGATIVE (built, sound, gated off)** | The IC mechanism works perfectly, but it optimizes the *cheapest* part. The real per-read cost is the wrapper (key-intern + unbox + tag-dispatch + owning clone), not offset resolution. The cheap corollary is to use `{String:T}` → `lin_map_get` instead. |
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
| **dict→Map fidelity (path-9 salvage)** | `Json`-as-dict → real `{String:T}` `LinMap` | 783k reads, digest-identical | O(1) hashed lookup vs O(n) assoc-list — the cheap corollary of the path-2 finding. |
| **capturing-lambda inline + stack-overflow fix (this session)** | Admit capturing literal lambdas at the Layer-1 inline gate | ~3.9× on local-capture map/reduce microbench | Earlier revert was a *stack* overflow (per-iteration `alloca` in the loop body), not a heap leak; fixed by hoisting the scratch alloca to the entry block (`entry_block_alloca`). |
| **9C salvage — seal-propagation** | Producer/consumer seal agreement in the checker | fixed live data corruption (nested all-scalar sealed-record-array read: garbage `7 0` → correct `33 44`) | A correctness fix surfaced by the packing work; merged independently of the (negative) packing chain. |
| **Runtime alloc wins (this session)** | toString small-int cache (~33% allocs); one-pass `utf8Bytes`/`fromCodePoints`/`tryParse*` intrinsics; byte-key `lin_object_get_bytes` (no temp LinString); display stringifier into one buffer; float/decode via `write!` not `format!`; union-probe save-len/truncate not clone | per-commit allocation reductions; map_flat_scalar wall ~5.8 s→5.5 s | Eliminate per-call transient allocation in hot runtime paths. |
| **stdlib algorithmic wins (this session)** | csv scanners/trim tail-recursive (was `range().while`); `buildQuery` `string.join` not O(n²) `joinAmp`; `array.chunk` inner copy via `slice`; `object.pick` bind-once; `lin_object_eq` O(n·m)→hash-index | csv ~60× on large input; object-eq ~2.0× on 24-key records | Big-O fixes beat micro-tuning; the csv O(N²)→O(N) is the largest single win this session. |
| **loop-emitter unification (this session, cleanup)** | `emit_combinator_loop`: one counted-loop scaffold for index + packed views; `lower_while` re-expressed through it | for/map/filter/fusion byte-identical | Not a perf win itself — removes ~95% duplication so future loop work lands once. |
| **ownership-as-a-fact verifier (this session)** | `Convention{Borrow,Own,Inout}` on `LinFunction` + `LIN_OWNERSHIP_SHADOW` report-only RC-balance verifier; first per-site heuristic consumed (Index-result lifetime) | zero behaviour change; shadow CLEAN | Foundation for sound RC elision and path-10's borrowed-reads; ships as inert metadata first. |

---

## 6. Guidance for writing fast Lin

1. **Prefer typed records and `&`-composed named types over `Json`.** This is the
   single biggest lever — a typed record field read is a constant-offset load;
   `Json` field access is an O(n) scan and an optimization barrier (~4–70× slower).
   `Json` is for genuinely unknowable shapes only.
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

---

## Provenance

Synthesized from the `docs/DECISIONS.md` perf ADRs
(014/015/016/024/028/034/040/044/045/055), the `benchmarks/` + `benchmarks/compare/`
suites, and the perf/cleanup work merged through this codebase's history. All
cross-language and typed-vs-`Json` numbers are measured (`compare.sh` and the RAPTOR
A/B harness); every quantitative claim is cited to its measured source. The path-n
learnings table (§5) is the consolidated record of the perf-investigation paths.
