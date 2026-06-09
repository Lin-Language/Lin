# Path 6 — Eliminate the non-inlined call / dispatch cost (inlining, fusion, monomorphic dispatch)

**Status:** Open proposal, written *after* Paths 0–5 and after the **ceiling measurement** (the
`LIN_NO_RC`/arena spike) falsified the construction-RC premise. Self-contained. **No userland language
change.**

**Direction in one line:** the measured dominant cost in interp — and a large share in RAPTOR — is the
**non-inlined runtime call and its internal work** (the per-element boxed closure call, the polymorphic
stdlib dispatch, `lin_object_get`, the box/unbox + tagged-arith helpers), **not** allocation, **not**
refcounting, **not** field reads. Removing *all* malloc+RC+free from interp gives **no speedup** (the
ceiling test below). The lever the data points at is therefore: **make the hot calls disappear** —
inline/fuse combinator and closure calls into straight-line LLVM, and specialise polymorphic dispatch —
so the per-element/per-op work LLVM cannot currently see across becomes register-resident code it can.

---

## Why this path exists — the measurement that redirects here

Paths 1–5 argued over **representation** (packed records vs inline caches vs value records) and **memory
model** (frame arenas, whole-program regions). Every one of them was written against a cost model the
measurements progressively dismantled. The decisive, falsifying result:

> **The ceiling test (Path-3 spike, verified):** a throwaway build with `lin_rc_retain`/`release`/`free`
> and `lin_*_alloc` all turned into no-ops (`LIN_NO_RC`) — i.e. *removing the entire allocation +
> refcounting subsystem* — ran interp at **0.48 s vs the 0.408 s baseline. No speedup. It got slightly
> slower.** A bump-arena ceiling was 0.42 s — also no win. **If construction + RC were the dominant cost,
> deleting them would speed interp up. It does not.** So construction-RC (the target of Paths 3/4) is
> *not* the recoverable cost, and value records (Path 5) cannot help interp either (same ceiling — you
> cannot recover a cost that vanishing the entire heap+RC subsystem does not recover).

What *is* left when alloc+RC are free? The **calls.** interp's hot path is a tree of non-inlined runtime
calls — the per-element boxed closure call at every `.for`, the polymorphic stdlib op dispatch, the
`lin_object_get`/box/unbox/`lin_tagged_arith` helpers — each an opaque function call LLVM cannot inline,
hoist, fold, or SROA across. The cost is the **call overhead + the helper's internal work**, repeated
millions of times. That is this path's target, and it is the one lever consistent with *every*
measurement in the history (it is why packed reads recovered only ~6%, why inline caches were modest,
why the arena ceiling was flat, and why the one thing that *did* move — Stage 1's in-place `for`,
~3.2×, shipped on the perf branch — worked by *removing per-element calls*, not by changing layout).

This is not "the prior work was wasted." Path 0 ran the measurement that grounds this redirect; Stage 1
already *is* an instance of this path (it killed the per-element materialize **call**); and two real
RC-soundness bugs + a general leak were fixed along the way. But the *direction* — make data cheaper —
is now measured to be the wrong axis. **Make the code cheaper (fewer, inlinable calls)** is the right one.

---

## Background (shared context — the measured history)

### The problem
interp ~10–25× slower than Node/Go; RAPTOR query phase ~25–30× slower. The framing instinct ("a
language shouldn't be slow reading a property of a known type") is correct *in general* but **measured
not to be these programs' bottleneck** — Lin already lays fixed-key records out as const-offset packed
structs; the read is already a `getelementptr + load`.

### How Lin executes a hot loop today
Lin is functional: every loop is `range(a,b).for(f)` / `arr.map(g)` / a recursive function. Lowered,
that is, per element: a **boxed closure call** through a `{fn_ptr, env_ptr}` ABI (box the element arg as
a `TaggedVal`, indirect-call, release), plus whatever the body does — which for a polymorphic stdlib op
or a `Json`/union value is *more non-inlined calls* (`lin_object_get`, `lin_box_*`, `lin_unbox_*`,
`lin_tagged_arith`). None of these are visible to LLVM: it cannot hoist an invariant out, fold a
constant through, SROA a value, or dead-eliminate. The "everything is a function" design means the hot
path is **call-bound**, and the calls are opaque.

### The three costs, and what the data says (now including the ceiling test)
1. **Field reads through the dynamic ABI** — assumed dominant; **measured not** (typed RAPTOR retype
   left `lin_object_get` 247→250 and was 2× *slower*; packed-read spike recovered ~6%).
2. **The boxing boundary** — real, but a cost a *second representation introduces*; the fix is to not
   introduce one (Stage 1 dissolves it in place for the cases it covers).
3. **Construction + RC** — **measured NOT recoverable** (the `LIN_NO_RC`/arena ceiling: no speedup).
4. **(this path) Non-inlined call + dispatch cost** — the residual when 1–3 are removed/free; the
   per-element closure ABI, polymorphic stdlib dispatch, and the box/unbox/`lin_object_get`/tagged-arith
   helper calls. **The measured remainder, and the one Stage 1 already dented (~3.2×).**

### The measured history (H1–H12)
- **H1** typed-vs-`Json` read ~72× — but measured an *already-packed in-register* value; overstated reads.
- **H2** RAPTOR leak drain ~97% (shipped, independent).
- **H3** sealed packed-array machinery + ASan harness (built, sound).
- **H4** heap-field gate widening net-negative (interp ~3× regression, TLV crash); narrowed back. First
  signal layout isn't the lever.
- **H5** RAPTOR retype >5× regression (first attempt).
- **H6** cheap-packed-reads spike recovered **only ~6%** of interp. Second signal reads aren't it.
- **H7** ruled out: boxed inline-slot (unsound), shape-ratio gate (3.6× blind spot), NaN-box/slab/GC/
  box-pool (allocator-swap negatives).
- **H8** function-boundary RC cliff measured (record free inline, ~13× passed to a fn because `escape.rs`
  treats every call-arg as escape); the read-only-arg *borrow* fix was UNSOUND (wrong values).
- **H9** the Path-0 decisive measurement: typing RAPTOR trips off `Json` made GROUP/RANGE **~2× slower**,
  `lin_object_get` unchanged — reads were never the bottleneck. (LOAD −38% from fewer transient boxes —
  but LOAD is ~2% of wall, so ~0.8% of total: a red herring for "construction is the lever.")
- **H10** inline caches (Path 2): interp neutral, RAPTOR −5–6% — modest because concrete records already
  seal; the IC only reaches genuinely-`Json` reads, which are call/box-dominated.
- **H11** construction census: the dominant *allocated* graphs escape their frame and live program-long.
  Read as "build region inference" by Path 4 — but see H12.
- **H12 — THE CEILING TEST (this path's anchor):** `LIN_NO_RC`/arena removing ALL alloc+RC+free from
  interp → **no speedup** (0.48 / 0.42 vs 0.408 baseline). Construction-RC is *not* the recoverable cost.
  The remainder is the non-inlined calls. **Also (Stage 1, shipped on the perf branch): the in-place
  `for` ABI — which removes the per-element materialize *call* — gave ~3.2× on packed-array iteration
  (7.3 s → 2.3 s).** That is this path's mechanism already proven once.

### The central finding (measured, not assumed)
interp and RAPTOR are slow because their hot loops are **trees of non-inlined runtime calls** — the
per-element boxed closure call, the polymorphic stdlib dispatch, and the box/unbox/`lin_object_get`/
tagged-arith helpers — none of which LLVM can optimise across. Data is already cheap (reads are
const-offset; the ceiling test shows alloc+RC are not recoverable). **The code is the cost.** Make the
calls inlinable / disappear and the per-element work collapses into register-resident straight-line code
the optimiser can fold — which is exactly how Go/Rust/Zig (and Stage 1's in-place `for`) reach C speed.

---

## This path's thesis

Attack the **call/dispatch axis**, not the data axis. Three composable mechanisms, no userland change:

### 6a — Combinator + closure inlining / fusion (the primary lever; Stage 1 is its first instance)
- **Generalise the in-place / inlined-callback lowering** (Stage 1 did `for`/`length`; extend to
  `map`/`filter`/`reduce`/`sortBy`) so a literal closure passed to a combinator is **inlined into the
  loop body**, not called per element through the boxed `{fn,env}` ABI. The element is read in place; the
  body's field reads are const-offset; no per-element arg box, no indirect call, no per-element release.
- **Fuse combinator chains** (`xs.map(f).filter(g).reduce(h)`) into a **single loop** with no
  intermediate array materialisation and no per-stage closure call — the Rust-iterator win.
- **Capturing closures:** the win must survive a closure that captures (`acc`, config) — the inlined
  body reads captures from the env by load, not via an indirect call. (The H8 read-only-arg *borrow* was
  unsound; inlining is the sound route to the same end — when the call is inlined there is no boundary to
  borrow across.)
- The risk surface is CFG/SSA correctness at the inline site (a body that emits its own blocks must wire
  the loop back-edge to the true latch — a known hazard from the prior fusion work) and the per-element
  element-box RC (Stage 1's leak — fixed by giving the fused field read the concrete field type, not
  `Json`; the same discipline applies). ASan `detect_leaks=1` with scaling is mandatory.

### 6b — Monomorphic / specialised dispatch for the polymorphic stdlib ops
- The polymorphic stdlib ops (`push`/`length`/`get`/iteration) are typed against the one dynamic ABI, so
  a concrete-typed call boxes into it. **Specialise the hot ones per concrete element type** (the
  Path-1-Step-2b monomorphization, generalised) so there is no shared dynamic ABI to box into — the
  boundary is *dissolved*, not special-cased. Lin already monomorphizes generics partially; the
  `get<T,D>` record-array link error (a Stage-0 fix on the perf branch) is literally a missing instance.
- Lower-risk subset: an internal direct-call fast path for a concrete-typed combinator over a known
  representation, falling back to the dynamic ABI otherwise (exactly what Stage 1's `for` redirect does).

### 6c — Inline the leaf helpers LLVM is currently blind to
- `lin_object_get` on a *concrete* `Json`-but-stable-shape site, `lin_box_int32`/`lin_unbox_int64` pairs
  that cancel, `lin_tagged_arith` on operands whose runtime tags are statically knowable — these are
  non-inlined calls that, made inlinable (LLVM `internal`/`alwaysinline` on the runtime bitcode, or
  emitting the body at the call site for the monomorphic case), let LLVM cancel box/unbox pairs and fold
  arithmetic. (Prior LTO investigation found whole-program LTO gave no win *because* the codegen-level
  inlining wasn't there — this is the targeted version of that.)

## What this path fixes
- **Non-inlined call/dispatch cost (#4):** yes — the measured remainder; interp's per-element closure +
  dispatch, RAPTOR's combinator loops. This is the lever the ceiling test points at.
- **Reads (#1):** already fast; not this path's job.
- **Boxing boundary (#2):** dissolved per-op by 6b's specialisation (no shared dynamic ABI to box into).
- **Construction/RC (#3):** not directly — but the ceiling test says that's not recoverable anyway; and
  inlining *incidentally* removes per-element arg-box alloc/RC (a side win, not the goal).

## Rationale / why pursue this path
- **It is anchored on the ceiling measurement (H12)** — the only test that isolates what is *recoverable*,
  and it points here, not at data/memory-model.
- **Its mechanism is already proven once:** Stage 1's in-place `for` (a call-elimination) shipped ~3.2×
  on packed-array iteration. 6a generalises exactly that.
- **Zero userland change**; reuses the existing inline-lambda / combinator lowering + the monomorphizer.
- **It is the model the fast functional/iterator languages use** (Rust iterators monomorphize+inline to
  one loop; this is "be like them" on the *call* axis, where the data axis was already fine).

## Cons / risks
- **CFG/SSA correctness at inline boundaries** is the primary risk (the latch-wiring hazard; block-emitting
  bodies). Each combinator extended is an ASan + correctness gate.
- **Per-element element-box RC** (Stage 1's leak class) — the fused read must carry the concrete type so
  transient boxes are reclaimed; `detect_leaks=1`-scaling is mandatory (the prior agent shipped a 7.8 MB
  leak by checking `=0` only).
- **Monomorphization cost** (6b): compile time + binary size; the machinery has bitten before
  (`mangle_type` collisions, the `get<T,D>` failure) — extend carefully.
- **Helper inlining (6c)** is the least-charted (runtime-bitcode inlining for AOT); do it last, measured.
- **It does not help a genuinely-`Json`-read-bound or string-alloc-bound phase** — see the companion note.

### Companion frontier (not this path, but the other measured cost): RAPTOR query-phase string allocation
The Path-3 spike also measured RAPTOR's query phase (~94% of wall) is **string-allocation-bound (~3.5 B
string allocs)** — the tokenizer `charCode`/`substring` and the `kConnections`/round-key string churn —
which neither this path nor any representation path touches. That is a *separate* frontier (string
interning / avoiding per-access key construction; the `byteAt` O(1) work in memory is a start). Worth its
own proposal; flagged here so it is not conflated with the call-cost lever.

## Relationship to the other paths
- **Generalises Stage 1 / the perf branch** — Stage 1's in-place `for` is 6a's first, shipped instance.
- **Supersedes Paths 3/4 (arenas/regions) and Path 5 (value records) as the *primary* lever** — H12's
  ceiling test shows their target (alloc/RC) is not recoverable for these benchmarks. Those paths are
  retired-as-measured-negative for the headline workloads (kept for the record + the array-of-struct
  *bandwidth* niche Path 5 still wins).
- **Composable with Path 2 (inline caches)** for the residual genuinely-`Json` reads, and with the
  string-allocation frontier above.

## Acceptance gates
Full `cargo test --workspace` green; sealed/iterator/stream corpus green (the combinator-heavy suites are
the guard); RAPTOR digest byte-identical (`group=26203913 range=773022892 journeys=139`); **ASan
`detect_leaks=0` AND `=1`-with-scaling** on every fused combinator (the Stage-1-leak lesson — `=0` alone
is insufficient); the **mechanism in IR** (no per-element indirect closure call / `lin_object_get` /
`sealed_array_to_tagged` in a fused typed combinator hot path) **and** the wall-clock (median ≥5,
low-load — the 128-core box is contended; only trust timings at low load), on interp + RAPTOR + the
combinator-pipeline bench.

## Verdict
The path the ceiling measurement actually points at: interp and RAPTOR are **call-bound**, not data-bound
— removing all alloc+RC gives no speedup, while removing per-element *calls* (Stage 1) gave ~3.2×. Attack
the call/dispatch axis: inline and fuse combinator/closure calls (6a, already proven once), specialise
polymorphic dispatch (6b), inline the leaf helpers LLVM is blind to (6c). Zero userland change, reuses
the existing inlining + monomorphization machinery, and it is the only proposal grounded in the one
measurement that isolates the recoverable cost. The companion string-allocation frontier (RAPTOR query)
is real and separate. **This is the recommended primary direction.**

### ✅ Corroborating note 2026-06-09 — the biggest shipped RAPTOR win is a "kill the hot call" win, by type change
This path's "call-bound, not data-bound" thesis is *reinforced* by the single largest RAPTOR speedup on
master, which no path claimed: typing the `Json` *dictionaries* (`routeStopIndex`/`bestArrivals`/
`kConnections`) as `{ String: T }` maps measured **PREP 144 s → 25.7 s (~5.6×)** (`8859f713`). Mechanism:
`m[k]` on a `Json` object is a non-inlined `lin_object_get` call (+ box the result); typing it routes to
a lean `lin_map_get` — i.e. it **removes a hot non-inlined call**, exactly the axis this path targets,
but via a type annotation rather than an inliner. The strategic lesson for this path: when enumerating
"the hot calls to make disappear" (6a–6c), include `lin_object_get` on `Json` **dictionaries** — the
cheapest way to kill that call is often to type the value as a `Map` (no compiler change needed), not to
inline the call. Run path-0's per-call-site-CLASS profile FIRST to find which hot `lin_object_get`s are
dictionary-shaped (delete by typing) vs record-shaped (need 6a–6c or packing). See path-0 RETROSPECTIVE
2026-06-09 for why this win was invisible to the aggregate-`lin_object_get` profile that framed the paths.
