# Execution plan — Path 8 (make functions free) and Path 7 (tracing GC)

**Date:** 2026-06-09. **Baseline:** master after the `perf/integrate-inplace-fusion` merge (`acf35a83`)
— Path-1 Steps 1+2, Path-6 6a chain-fusion (reduce+for), and 6b length/push dispatch all shipped. This
plan covers only what is **left** on Paths 7 and 8, sequenced by measured leverage.

---

## TL;DR — the two paths are NOT equals

- **Path 8 is the live, measured-positive direction.** Its mechanism (remove non-inlined calls) is the
  one thing that has *ever* moved the call-bound benchmarks (Stage-1 ~3.2×, 6b ~2.7×, both shipped). The
  plan below is "finish the tiers that aren't done," sequenced by the spike + cost-map evidence.
- **Path 7 (tracing GC) is on HOLD, gated, and probably dead.** The interp ceiling test (removing ALL
  alloc+RC = no speedup) falsifies it for interp; the RAPTOR profile shows RAPTOR is **read-bound, not
  alloc-bound** (631 M linear-scan reads, not allocation). **Neither headline benchmark is alloc-bound, so
  a GC has no measured cost to attack.** The plan for Path 7 is therefore a *single decisive measurement*
  that either revives it or formally retires it — not an implementation.

Do Path 8. Run Path 7's one measurement in parallel (cheap). Only build Path 7 if that measurement
surprises us.

---

# PATH 8 — execution plan (the live direction)

## What already shipped (do NOT redo — verify, then build on)
- **Tier 2 partial:** in-place packed `for`/`length`/`map`/`reduce` (no `sealed_array_to_tagged`
  materialize); combinator-chain fusion for **reduce + for terminals** (`8b845cb7`/`51db384b`/`9eada284`);
  inline-lambda splicing for literal closures.
- **Tier 3 partial:** 6b specialized dispatch — concrete-typed `length`/`push` skip the Json box
  (`c6bbc089`, measured ~2.7× on a length-bound loop).
- **Correctness substrate:** the boxed-combinator-result-into-packed-annotation coercion
  (`45923841`), the inline-for element-box leak fix (`5f6592f4`), the `Trip|Null` tail-param UAF fix.

## The remaining gaps (verified in `crates/lin-ir/src/lower.rs` on current master)
1. **Fusion bails on sealed/heap-element sources** (`5a64f7f9` gated it to inline-scalar flow) and only
   covers **reduce + for** terminals. A `xs.map(f).filter(g).map(h)` chain producing an array, or any
   chain over a record array, still materializes per stage.
2. **Closures still use the boxed indirect ABI for any non-literal / non-inlinable callee.** The inline
   path fires only when the combinator's callback is a literal lambda at the call site
   (`inline_lambda_body`). A named function value, a passed-in callback, or a cross-module callee still
   boxes every arg and `indirect_call`s. **No devirtualization of statically-known targets.**
3. **6b dispatch covers only `length`/`push`** — `get`/`at`/index/`slice`/`sort` over a concrete element
   type still route through the boxed type-erased generic.
4. **Tier 1 (bitcode runtime) is untouched** — confirmed `.a`-only on master (the spike, `bf72f308`,
   measured <2% alone; parked).

## Sequenced plan (each step shippable + benchmark-gated; ASan `=1`-scaling mandatory per the Stage-1 leak lesson)

### Step 8.1 — Generalize fusion + inlining to record-element sources and full chains *(highest leverage, lowest new risk — START HERE)*
The shipped fusion is the proven mechanism but artificially narrow (scalar-only, reduce/for-only).
Widen it:
- **Extend the inline-lambda + fusion path to sealed-record element arrays** (the `5a64f7f9` bail), reusing
  the in-place packed-element read that already exists (`try_lower_packed_elem_field`,
  `inline_lambda_body_packed_view`). This makes `trips.filter(...).map(...)` over a packed `Trip[]` a
  single loop with const-offset reads — directly helps the RAPTOR scan *and* any record-combinator code.
- **Add array-producing terminals to the chain fuser** (`map`/`filter` producing an array, not just
  reduce/for) so a multi-stage pipeline is one pass with no intermediate array.
- *Gate:* the IR-mechanism assertion (no `sealed_array_to_tagged` / per-element box / indirect call in the
  fused hot path), digest byte-identical, cross-lang non-regression, ASan `=1`-scaling.
- *Why first:* it is the **already-validated mechanism** (Stage-1 ~3.2×) applied to the cases it currently
  refuses; no new ABI, no oracle reconciliation, immediately helps both interp-style and record code.

### Step 8.2 — Devirtualize statically-known closure calls *(the Tier-3 core; the big structural win)*
When the callee is statically known — a literal lambda passed to a non-inlinable position, a named
top-level `val` function, an imported export — emit a **direct call** instead of the boxed
`{fn_ptr,env_ptr}` `indirect_call`, and pass args in their natural (unboxed) types where the signature is
concrete. This is what lets LLVM inline across user functions at all.
- Start narrow: direct-call the known-callee case, keep the boxed indirect path as the fallback for
  genuinely-dynamic callees (first-class function values whose target isn't known).
- Compose with **internal linkage** for non-exported functions (the spike found internal-linkage *alone*
  bought nothing because the program is one module — but it is a prerequisite for the inliner to act on the
  direct calls this step creates; re-measure *with* devirtualization, not alone).
- *Risk:* this is the deepest change; the unboxed-arg convention must interoperate with the boxed fallback
  at every boundary (the repr-mismatch class). Incremental, one call-shape at a time, ASan-gated.

### Step 8.3 — Extend 6b dispatch to the rest of the hot stdlib ops
`get`/`at`/index/`slice`/`sort`/`keys` over a concrete element type → specialized intrinsic, skip the Json
box (the `length`/`push` pattern, generalized). Mechanical once 8.2's unboxed-arg plumbing exists.

### Step 8.4 — Tier 1 (bitcode runtime) LAST, as the finishing pass
Only now does it pay: once 8.1+8.2 have put the closure body and its consuming helpers in one function
(no indirect call, direct helper calls), making the leaf helpers inlinable bitcode lets the box/unbox
pairs finally **cancel** (the spike proved they don't while the consumer is opaque). Productionize the
stable-ABI bc/`.a` co-build (the real integration cost the spike flagged), `alwaysinline` the hot leaf
helpers. *Re-measure the spike's benchmarks — the <2% should become meaningful once the consumer boundary
is gone.*

## Path-8 expected payoff & gate
The mechanism is proven; the question is breadth. After 8.1+8.2, a combinator-heavy or closure-heavy hot
loop should approach the Stage-1 floor (register-resident straight-line code). **Gate every step on the
cross-language scoreboard** (interp vs Node/Python; the combinator-pipeline bench; dijkstra) — the goal is
to close the gap to Node/Python on the *call-bound* benchmarks. If a step shows the IR mechanism but no
wall-clock, stop and diagnose (the Tier-1 lesson).

---

# PATH 7 — execution plan (one measurement, then revive-or-retire)

## Current standing: gated and probably dead
- interp: H12 ceiling test — deleting ALL alloc+RC gave **no speedup**. A GC cannot beat that limit.
- RAPTOR: the profile shows **read-bound** (631 M linear scans), with ~3.5 B box ops that are a symptom of
  the `Json` representation (cloning boxed values on read), not of allocation pressure a GC would relieve —
  Path 9 (packed records) removes them at the source; a GC would still allocate+collect them.
- So **no measured benchmark is allocation-bound**, which is the only thing a tracing GC fixes.

## The single decisive measurement (the ONLY Path-7 work to do now)
Before any GC design, answer one question with one cheap experiment: **is there ANY workload where Lin is
allocation-bound — i.e. where allocation *rate* (not RC instruction count, not read cost) dominates?**
- **Instrument allocation rate + live-set + collection-cost-proxy**, env-gated (reuse the spike's
  `LIN_COUNT` counters): bytes-allocated/sec and peak-live-bytes on interp, RAPTOR (all phases — LOAD/PREP
  may differ from query), and an allocation-heavy synthetic (deep recursive build, large transient graphs).
- **The discriminating test:** a workload where bump-allocation + batch-collection would beat
  malloc+per-object-RC is one with a **high allocation rate AND a low live-set** (lots of short-lived
  garbage). If RAPTOR's LOAD/PREP (build-once, big graphs) shows this — and the profile's LOAD −38%
  from-fewer-transient-boxes hint suggests it might — then a GC has a real target *there* even though the
  query phase doesn't.
- *Cost:* counters + one analysis pass. Days, not weeks. No GC built.

## The decision gate
- **If the measurement finds an allocation-bound phase** (high alloc rate, low live-set, and RC/free is a
  measurable fraction of it) → revive Path 7, but **scoped to that phase** (e.g. a build-phase region/nursery),
  and stage it exactly as the proposal lays out (non-moving mark-sweep behind a flag first, to prove the
  trace-map, before any moving collector). The disjoint-per-thread-heap design (ADR-043) keeps it tractable.
- **If nothing is allocation-bound** (the likely outcome given the ceiling test) → **formally retire Path
  7.** Record the measurement, mark the proposal CLOSED-NEGATIVE, and put the effort into Path 8 + Path 9.
  The non-perf wins a GC would bring (cycle collection for `Shared<T>`, killing the UAF bug class) are real
  but do NOT justify a multi-month GC on their own — revisit only if the UAF bug class becomes a recurring
  production problem.

## Why not build the GC speculatively
It is the largest single change proposed (allocator, collector, precise/conservative root finding, removing
RC threaded through lower.rs/codegen) and **every measurement to date says its target cost is not where the
benchmarks spend time.** Building it on the architectural argument alone — against the ceiling test — would
repeat exactly the mistake that sent five agents at the wrong lever. Measure first.

---

# Combined sequencing & resourcing

| Workstream | Status | Next action | Risk | Blocks on |
|---|---|---|---|---|
| **Path 8 Step 8.1** (widen fusion/inline to records + full chains) | proven mechanism, narrow scope | **build now** | low | nothing — START HERE |
| **Path 8 Step 8.2** (devirtualize known calls) | not started | after 8.1 | high (deep ABI) | 8.1 patterns |
| **Path 8 Step 8.3** (extend 6b ops) | length/push done | after 8.2 | low | 8.2 unboxed plumbing |
| **Path 8 Step 8.4** (bitcode runtime) | spiked, parked | LAST | med (build-system) | 8.1+8.2 (else <2%) |
| **Path 7 measurement** (alloc-bound hunt) | — | **run in parallel, now** | low | nothing |
| **Path 7 implementation** | HOLD | only if measurement says alloc-bound | very high | the measurement |

**Recommended immediate moves (parallel, non-overlapping):**
1. Path 8 Step 8.1 — one agent, fresh worktree off current master. The highest-leverage unblocked work.
2. Path 7 measurement — one agent, instrumentation-only, answers the revive-or-retire question cheaply.

These don't touch the same code (8.1 = `lower.rs` fusion/inline; Path-7 measurement = runtime counters +
analysis), so they run truly in parallel. Step 8.2 waits for 8.1 (same `lower.rs` call-lowering region —
sequence, don't race, per the lower.rs-collision lesson). Path 9 (packed records, the RAPTOR/`Json`-read
lever) is the *other* half of the story and proceeds on its own track; 8.1's record-fusion widening is
shared groundwork that helps both.
