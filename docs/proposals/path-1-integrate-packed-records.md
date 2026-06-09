# Path 1 — Finish the packed-record approach: integrate packing with the operations

**Status:** Open proposal, one of three independent paths. Self-contained.
**Direction in one line:** keep one record construct; make the packed/sealed representation we already
built actually pay off by making the stdlib *operations* (`length`/`map`/`for`/index) work on packed
data without re-boxing, and pack fixed-key record types by default (no userland change — the grammar already says struct vs map).

---

## Background (shared context — the problem, the framing, the full history)

### The problem
Reading a field of a known record type — `point["x"]`, `trip["stopTimes"]`, `token["kind"]` — and
operating over arrays of such records (`length`, `for`, `map`, `filter`) is dramatically slower in Lin
than in Go/Rust/Zig/Nim, where these are constant-offset loads and const-stride walks.

### The framing correction (the root misconception)
**Lin's type system is not JSON.** It is syntactically JSON-like and shares JSON's primitives, but
`type Point = { "x": Int32, "y": Int32 }` is a named, closed, statically-known record type — not a
dynamic bag. The historical conflation of "looks like JSON" with "is represented like dynamic JSON" is
the root of the performance problem. Slow field access on a *known* type is not excused by the JSON
resemblance.

### How Lin represents values today
- **Boxed (default, dynamic):** a record is a heap `LinObject` — refcounted, string-keyed, hash-indexed
  when large. `obj["k"]` is a non-inlinable `lin_object_get` call (intern-pointer compare + scan/probe +
  box result as a 16-byte `TaggedVal`), and is **opaque to LLVM** (no hoist/fold/SROA/elim across it).
  This is the representation of `Json`, anonymous/inferred literals, structurally-subtyped params, **and
  every value flowing through a polymorphic stdlib op** (they are typed against this one dynamic ABI).
- **Packed / "sealed" (fast, opt-in):** a named `type T` laid out as a packed struct
  `[u32 rc | u32 size | u64 desc | fields...]`, fields at const offsets; an array of them is a
  header-less contiguous `0xFE` buffer, `elem_stride` bytes/element, with a per-field RC descriptor. A
  scalar field read of a packed element is `getelementptr + load` — verified, the Go/Rust-class behavior,
  where it's reached.
- **Flat scalar arrays:** `Int32[]` is already a contiguous unboxed buffer with specialized lowerings
  (`lin_flat_array_*`) — precedent that "specialize the op per representation" already exists.
- Supporting machinery (kept by every path): a `Repr` lattice + debug oracle/verifier
  (`crates/lin-ir/src/repr.rs`, ADR-062); the single-source gate
  `Type::is_sealed_array_field_packable` (`crates/lin-check/src/types.rs`), currently **scalar+Bool only**.

### The three costs
1. **Field reads through the dynamic ABI** — the original motivation, ~72× (typed vs `Json`, 50M reads).
   **Now understood to be fixable / largely solved** (a spike made even heap-field packed reads a
   const-offset load).
2. **Operations at the boxing boundary** — the cost that dominates real programs and was *not*
   anticipated. Verified at the IR level: `length(tokens)` on a packed `Token[]` emits
   `lin_sealed_array_to_tagged` — it **materializes the entire array into a boxed `Object[]`** (full copy
   of every element + field) just to read a `u64` length at offset 8. Same for `map`/`filter`/`for`/
   `reduce`/`sort`. Every polymorphic op re-boxes a packed array **on entry**.
3. **Construction refcounting** — per-element-per-field retain on build + a descriptor drop-walk on free.
   A memory-model cost, not a representation one.

### The full history (what was tried, learned, failed)
- **H1 — Decisive profile (valid motivation):** typed-record vs `Json` field read ~72×; LLVM even
  elided a dead typed object. Real, but measured reads of an *already-packed/in-register* value — which
  is why it looked like reads were the whole story.
- **H2 — Leak drain (succeeded, independent):** RAPTOR leaked ~190 MB/scan; a class of RC/ownership
  bugs fixed → ~97% reduction (RSS 6 GB→2.2 GB), bench completes.
- **H3 — Sealed machinery + harness (built, sound):** per-field RC, descriptor walks, keep-packed ops,
  **mechanism (i)** (materialize-on-read via a named full-field descriptor), a 3-point ASan harness over
  {op × position × field-shape}. The harness found a general `sort` leak manual probing had missed.
- **H4 — Gate widenings (each found one bug; collectively net-negative):** widening
  scalar→String→Array→Map→nested surfaced + fixed, one per step, real defects — push-into-Json-field
  **silent data loss**, a `sealed_construct` width-subtyping **panic**, a for/while/reduce per-element
  **leak**, a nested-record-array push **crash**, a repr-pass nested-array-field **crash**, a missing
  **KIND_MAP** heap-field kind. But once heap fields packed: **interp ~3× regression**
  (`Token={kind,text:String}`), **TLV codec crash** (`{tag:Int32,bytes:Int32[]}`), **zero RAPTOR
  benefit**. Gate **narrowed back to scalar+Bool** (current merged state); plumbing kept dormant.
- **H5 — RAPTOR retype (first attempt: correct, >5× regression — but the conclusion was refined, see
  H5b):** typing trips end-to-end type-checked, passed units, correct digest — but `run.lin` regressed
  >5× (killed ~45 min vs ~510 s). It clarified two real sub-blockers: `get<T,D>` monomorphization for
  record-array `T` (a link error), and threading a packed `Trip` through the `Conn` tuple / `Trip|Null`
  tail-recursive scan param without re-boxing (a UAF/repr-demotion).
- **H5b — Reframing (the typed-RAPTOR-FIRST argument):** an exact-hot-read micro (the RAPTOR
  `stopTime["arrivalTime"]` read, `Json` vs typed `StopTime[]`, 2M iters, hand-verified) measured
  **566 ms → 148 ms (~3.8×)** with **object_get 6M→0, probe-steps 20M→0** — i.e. ~72% of RAPTOR's
  hot-read cost is *just that it was left `Json`*. Projection: typing the trips moves RAPTOR's ~110 s
  query toward ~30 s (**~3.5×**), digest byte-identical, **no language change** — just fixing the two
  H5 crash/RC bugs + the `get<T,D>` monomorphization + retyping `.lin`. **So typed-RAPTOR is the
  cheapest large win and a candidate FIRST move** — *with this caveat (the reconciliation with H5/H6):*
  the micro measures the now-shipped **packed const-offset read**; the >5× full-retype regression was
  the **`length`/combinator materialization** (cost #2) dominating. Whether typed-RAPTOR nets ~3.5× or
  regresses is therefore **empirical and path-dependent**: it wins iff RAPTOR's hot path is dominated by
  the (now cheap) field reads rather than by `length`/`for`/combinator calls that still materialize. The
  honest sequencing: fix the two crash bugs + `get<T,D>`, type the trips, **measure**; if the
  combinators dominate, Step 1's in-place ABI is the prerequisite that turns the regression into the
  win. (This is the difference between H5's pessimism and H5b's optimism — both are right about
  different sub-costs.)
- **H6 — The cheap-packed-read spike:** made packed heap-field reads cheap (const-offset `load ptr` +
  retain-if-escapes; sound, ASan-clean; 1.7× on a read-only microbench) — but recovered **only ~6%** of
  interp's regression. IR showed why: `length`/combinators materialize the whole array on entry
  (cost #2). **Necessary but not sufficient — reads were not interp's bottleneck.** (Reconciles with
  H5b: interp is combinator/`length`-heavy so reads don't dominate; RAPTOR's scan may differ — measure.)
- **H7 — Ruled out:** boxed inline-slot (unsound under structural subtyping); field-shape ratio gate
  (proxy for the wrong variable, 3.6× blind spot); cheap-reads-alone (~6%); `"${k}"` round-key churn
  (GROUP-neutral); NaN-box / slab / GC / box-pool (prior negatives).
- **H8 — Inlining / fusion (a SEPARATE, orthogonal, already-merged lever — see "the orphaned axis"
  below):** independent of representation, making the hot function-call boundaries *vanish* recovered
  real time on loop-bound code. Merged: `range(a,b).for(f)` → a counted loop (`23c3e1f`); capturing
  closures inlined at literal `.for`/combinator call sites (`e98a09a`/`3d3d425`, ~2× on loop-bound
  code); flat-scalar array push/set inlined; the boxed-`Object[]` `arr[i].field` fusion (`71c89e3`).
  This axis is **not** about packed vs boxed at all — it is about Lin being a functional language where
  *everything is a call*, so eliminating call overhead at hot sites is a lever orthogonal to (and
  composable with) every representation choice.

### The central finding
The bottleneck was never field reads (fixed). It is that **the packed representation is not integrated
with the runtime's polymorphic operations** — the layout got fixed; the *verbs over the layout* still
box. Go/Rust have no equivalent penalty because `len()`/iteration are defined on the contiguous
representation; there is no second dynamic ABI to convert to. Plus the separate construction-RC cost (#3)
that no dispatch-axis fix touches.

---

## This path's thesis

The packed/sealed machinery (layout, repr lattice, per-field RC, mechanism (i)) is **built and sound** —
it just doesn't pay off because cost #2 (the boxing boundary) was never addressed. So *finish the
approach we started*: make the operations operate on packed data in place, then re-widen / re-default the
packing decision behind a benchmark gate. No new type, no surface change, no new memory model — make the
existing typed-record packing actually fast end-to-end.

This is the **continuation** path. It contains three coupled decisions and one sub-choice.

### Already on master (do NOT re-do — this path *continues* from here)
A meaningful slice of this path is already merged; the work below builds on it:
- **Scalar field-read fusion is shipped.** `arr[i]["scalarField"]` over a packed sealed-scalar array
  already lowers to a single const-offset `SealedArrayFieldGet` (`lower.rs::try_lower_sealed_array_field`
  + codegen `compile_ir_sealed_array_field_get`) — no element materialization. Verified Go/Rust-class.
- **Fused boxed-Object[] field read is shipped** (`BoxedArrayFieldGet`) — `arr[i].field` over a boxed
  record array is a single borrowed `lin_array_get` + `lin_object_get`, not a per-access materialize.
- **The repr-pass nested-sealed-array-field-read classification is fixed** (`f378f2f`) — `t["stopTimes"]`
  off a packed record is `Packed`, no oracle crash. (This was a live blocker earlier in the effort; it
  is done.)
- **The gate is consolidated to one predicate** (`Type::is_sealed_array_field_packable`), and the
  runtime plumbing (per-field RC descriptors, KIND_MAP, `clone_sealed_array`, mechanism (i)
  materialize-on-read) is all merged and dormant. The gate is currently **scalar+Bool**.
So Step 1 below is **only the parts NOT yet done**: the *heap-field* read fusion (scalar is shipped;
the String/Array/Map/nested extension is the spike on `spike/cheap-typed-reads`, unmerged), and —
the actual unsolved core — making `length`/iteration/the combinators operate on a packed array **in
place** instead of materializing it (cost #2, which nothing on master addresses).

### Step 0 — see [Path 0 (Prerequisites)](path-0-prerequisites.md), do it FIRST
The cheapest large RAPTOR win — type the trips off `Json` + fix `get<T,D>` monomorphization + the
`Trip|Null` tail-param UAF, then **measure** — has been hoisted into its own
[Path 0](path-0-prerequisites.md) because it is **path-independent**: those fixes and the de-`Json`-ing
are needed under Path 2 just as much as Path 1, so they are not part of *this* path's strategy. Path 0's
measurement is also what decides whether this path's Step 1 (the in-place ABI) is even required: if
RAPTOR's hot path is read-dominated, typing alone wins (~110 s → ~30 s) and Step 1 isn't needed for
RAPTOR; if the `length`/`for`/combinator calls still dominate (they materialize the packed array — cost
#2), that is the concrete proof Step 1 is the prerequisite. **One sub-fix IS this path's, not Path 0's:**
the `sort$Object` comparator reading packed `Trip[]` elements as boxed — it only bites once the trips are
*packed* (this path's representation), so it lives here, not in Path 0.

### Step 1 (mandatory if Step 0 shows combinators dominate; the core) — an in-place packed-array ABI
Make `length`/index/`for`/`map`/`filter`/`reduce`/`sort`/`push`/`set` operate on a `0xFE` packed array
**in place**, dispatched on the operand's `Repr` (codegen already knows it — exactly as flat `Int32[]`
is specialized):
- `length` → load `u64` at offset 8 (no materialize).
- `arr[i]` → const-offset interior pointer (borrowed; materialize-to-owned only on genuine escape).
- `arr[i]["field"]` → const-offset load. **Scalar field: already shipped** (see "Already on master").
  **Heap field** (String/Array/Map/nested): borrowed pointer + retain-if-escapes — the spike on
  `spike/cheap-typed-reads`, built + ASan-clean, ready to re-land **on top of** the in-place iteration
  ABI (re-landing it before the combinators are in-place reproduces the §H6 ~6% dead end).
- iteration → const-stride loop, callback gets a borrowed element pointer; its typed-record param makes
  field reads const-offset. No per-element box, no `sealed_array_to_tagged`. **This is the unsolved core**
  — nothing on master makes `length`/`for`/`map`/`reduce` operate on a packed array in place; they all
  materialize it (cost #2).
This directly kills cost #2. It is the necessary, not-yet-done core of this entire path.

### Step 2 (the dispatch sub-choice) — how is the in-place op generated?
Two mechanisms, not exclusive:
- **(a) Representation-dispatched lowering** (the "Option A" mechanism): one generic op, branches on
  `Repr` at lowering — packed path or boxed path. Smaller to add, but a **dual lowering to maintain
  forever**, and a field read is fast only if the value packed.
- **(b) Monomorphize the stdlib** (the "Option F" mechanism): compile a separate instance per concrete
  element type (`length$Token`, `for$Trip`) so there is **no shared dynamic ABI to box into** — the
  boundary is *dissolved*, not special-cased. The Rust/C++/Zig approach; Lin already monomorphizes
  generics partially (and §H5's `get<T,D>` sub-blocker is literally a missing monomorphization). Cost:
  compile time + binary-size bloat + extending the monomorphization machinery (which has bitten us:
  `mangle_type` collisions, `get<T,D>` failure). A pragmatic system does **both**: monomorphize hot
  statically-typed sites (b), keep a repr-dispatched boxed instance (a) as the shared fallback.

### Step 3 (the default) — pack fixed-key record types by default. NO userland change.
**Key realization: the surface grammar ALREADY distinguishes a struct from a map — the compiler does not
have to infer or annotate anything.**
- `type Person = { "age": UInt8, "name": String }` — **fixed keys** → a sealed struct → pack it.
- `type Counts = { String: UInt8 }` — **index signature** → a hashmap → boxed `LinMap`, dynamic.
- `Json` and inferred/anonymous-shape values → boxed, dynamic.

So packing a fixed-key record type **by default** is not a heuristic (rejected: §H7's usage/shape-ratio
gate had a 3.6× blind spot) and not a new keyword (rejected: a `packed` annotation asks the programmer to
re-state what `type Person = {fixed keys}` already declares — pure ceremony). It is simply **honoring the
declaration the programmer already wrote.** A fixed-key `type` is the struct; it gets the struct
representation. This absorbs the entire value of a distinct-`struct`-kind idea **with zero surface
change**, because Lin's grammar already carries the distinction.

The gate (`is_sealed_array_field_packable`) therefore stops being a perf heuristic and becomes a pure
**soundness predicate**: "can this fixed-key shape be laid out packed?" — yes unless it reaches a genuine
dynamic boundary (`Json` slot, undiscriminated union, structurally-subtyped param that reinterprets the
layout, FFI, cross-thread transfer), where it materializes to boxed.

**The one real risk this still carries (not a userland change — an implementation-soundness obligation):**
structural/width subtyping. `{a,b,c} <: {a}` means a value can be read through a narrower type than it was
built with; a packed struct has a fixed declaration-order layout, so the gate must refuse to pack (or
must box at) any site where width/structural subtyping can reinterpret the value, and `Json` round-trips
must route through the boxed fallback. This is exactly the packed/boxed-mismatch class §H4/H5 produced, so
it is only safe *after* Step 1's in-place ABI proves out and the repr oracle/verifier + the boxed-boundary
set are hardened. But note: it is a *correctness* problem in the compiler, **not** a question the
programmer is asked to answer — the programmer already answered it by writing fixed keys vs an index
signature.

### The orphaned axis — inlining / fusion (orthogonal to everything above; partly shipped)
Distinct from representation entirely, and **composable with every path**, is the lever of making Lin's
hot **function-call boundaries vanish**. Lin is functional — *everything is a call*, including `for`
(`range().for(f)`) and every combinator — so call overhead is itself a major cost, independent of
packed-vs-boxed. Already merged and proven (§H8): `range(a,b).for(f)` fused to a counted loop;
capturing closures inlined at literal `.for`/combinator sites (~2× on loop-bound code); flat-scalar
push/set inlined; the boxed-`Object[]` `arr[i].field` fusion. **Remaining reach** (the orphaned
follow-on): extend closure inlining to `arr.for`/`map`/`filter`/`reduce`/`sortBy` with literal/capturing
closures (the latch-relative-CFG back-edge wiring is the known hazard), and to indirect/cross-module
callees. This is a separate, lower-risk, no-model-decision lever that helps *every* representation choice
— it should be tracked as its own workstream, not folded into the packed-vs-boxed decision. (It is the
core of the parallel `perf-1-inlining-fusion.md` proposal; named here so this path doesn't pretend
representation is the only axis.)

## What this path fixes

- **Field reads:** yes — scalar shipped; heap via the spike re-land on Step 1.
- **Combinator/`length` boundary (cost #2):** yes — Step 1, the core of this path.
- **Construction RC (cost #3):** **no.** Build-heavy/read-once workloads are not helped; compose with
  Path 3's arenas if that cost matters.
- **Call-boundary overhead:** **not by this path** — that is the orthogonal inlining/fusion lever above,
  partly shipped, composable with this path and all others.

## Rationale / why pursue this path

- It fixes the *measured* bottleneck (§H6) at its source, and is the minimum that turns the
  interp/RAPTOR regressions into wins.
- **It reuses everything already built and verified** — layout, repr lattice + oracle, per-field RC,
  mechanism (i), the spike's read fusion. No surface language change, no new memory model. Lowest
  conceptual disruption of the "make it fast" paths.
- It is **incremental and each step is shippable + benchmarkable** (ABI on scalar shapes → spike re-land
  → widen gate per shape → type RAPTOR), encoding the hard-won §H4 lesson: *never widen the
  representation before the operations underneath are cheap.*
- Step 1 is **decision-free and the necessary core of Paths 1, 3-composed, and 4** — so this path's first
  move is valuable no matter which direction is ultimately chosen.

## Cons / risks

- **The structural-subtyping soundness obligation (Step 3)** is the highest-risk part — packing a
  fixed-key type by default is sound only where width/structural subtyping cannot reinterpret the layout
  and where `Json` round-trips route through the boxed fallback. This is the exact packed/boxed-mismatch
  bug class §H4/H5 produced. It is a *compiler-correctness* obligation, **not** a userland change (the
  programmer already declared struct-vs-map by writing fixed keys vs an index signature) — but it is real
  and must be hardened (repr oracle/verifier coverage) before packed-by-default is safe.
- **Touches every eager combinator's lowering** (Step 1) — multi-week; the monomorphization mechanism (b)
  adds compile-time/bloat costs and extends machinery that has bitten us.
- **Construction RC untouched** — needs composition with Path 3's inferred arenas for build-heavy
  workloads.
- **The boxed boundary is the risk surface.** Every genuine dynamic boundary (`Json` slot,
  undiscriminated union, FFI, transfer, `toString`/serialize) must still materialize; a missed site is a
  UAF/mis-read. The repr oracle/verifier is the structural guard and must cover every new in-place
  lowering — and must be *hardened* before packed-by-default.

## Relationship to the other paths

- **Path 2 (inline caches)** is the opposite philosophy — make the *dynamic* representation fast so no
  packed type is needed. Mutually exclusive in spirit (though a mature system could have both layers).
- **Path 3 (inferred arenas)** is orthogonal and composable — it fixes cost #3 (construction) which this
  path leaves untouched. Path 1 (in-place reads/ops) + Path 3 (cheap construction) covers all three costs
  with **no userland change**.

## Acceptance gates (apply to every step)

Full `cargo test --workspace` green; the sealed harness green **plus a new IR-mechanism assertion** (no
`sealed_array_to_tagged` / per-element box in a typed combinator/`length` hot path — because §H4 proved
correctness-green is insufficient, the harness missed the perf regression by not benchmarking); RAPTOR
digest byte-identical (`group=26203913 range=773022892 journeys=139`); ASan-clean (no UAF/double-free/
scaling leak — the boxed-boundary contract is the risk); **cross-language benchmark non-regression**
(interp, RAPTOR GROUP/RANGE, records, dijkstra) — prove the mechanism in IR **and** the wall-clock.

## Verdict

The lowest-disruption continuation, and **entirely within the compiler — no userland change.** It makes
the work already done pay off by fixing the one cost that was never addressed (the in-place ABI), reuses
all existing machinery, and packs fixed-key record types by default simply by honoring the struct-vs-map
distinction the grammar already carries (`{"age":UInt8}` = struct; `{String:UInt8}` = map). It fully
answers "a struct field read is cheap, full stop" *once Step 1 + packed-by-default land* (the latter
gated on the structural-subtyping soundness hardening). It does not touch construction RC — compose with
Path 3's inferred arenas for that. Best if the goal is the win with zero language-surface change.

---

## IMPLEMENTATION FINDINGS (2026-06-09, branch `path1-packed-records`, NOT merged)

> **Work reference (the code is all here):** branch **`path1-packed-records`** (tip `39f8329c`), checked out at worktree `.claude/worktrees/path1-packed`. The worktree is ephemeral; the **branch is the durable handle** — `git log path1-packed-records`. Commit chain from master base `29d39237`:
> - `04bec701` fix(ir): Trip|Null tail-recursive self-tail-call UAF (Path 0)
> - `568756b0` fix(ir): per-iteration index/element box leak in non-inline for loops
> - `470ea0ad` perf(ir): **Path-1 Step 1+2** — in-place packed-array `for`/`length`, no materialize
> - `4de04817` perf(ir): **Path-1 Step 1+2** — in-place packed `map`/`reduce` field reads
> - `39f8329c` perf(ir): **Path-1 Step 3** — in-place String field read capability; gate stays scalar+Bool (oracle blocker)
>
> Mergeable subset (the 4.5× win, scalar scope): `470ea0ad` + `4de04817` on top of the two fixes.

**Steps 1+2 (the in-place packed-array op ABI — "the unsolved core") are SOLVED and measured-winning. Step 3 (pack heap-field records by default) hit the predicted repr-oracle wall and is built-but-dormant.** All numbers below independently re-verified, not just agent-reported.

### Steps 1+2 — DONE (commits `470ea0ad`, `4de04817`)
The whole-array materialization on combinator entry — the dominant cost #2 — is **eliminated** for packed sealed-scalar arrays in `for`/`length`/`map`/`reduce`.
- **Mechanism (a) chosen** (representation-dispatched lowering, not monomorphization): a `std_iter_for` / `std_array_length` call over a packed array is redirected at the import-fn dispatch site to the `lin_for`/`lin_length` intrinsic at the **concrete** element type, dissolving the `Json`-param boundary that forced `lin_sealed_array_to_tagged`. The element binds to a borrowed `(array, index)` **view** (`ctx.packed_elem_slots`); `p["field"]` lowers to a const-offset `SealedArrayFieldGet` straight off the `0xFE` buffer. `length` loads the u64 at offset 8 directly.
- **Soundness guard** (`elem_used_only_for_scalar_fields`): the in-place path fires ONLY when the body uses the element solely for scalar field reads; whole-value uses (`push(out, p)`, comparisons, `filter`'s keep) fall back to the existing materialize path — byte-identical to master, **no correctness regression**.
- **IR criterion proven (by me):** `Pt[].for(p => … p["x"] …)` emits **0** `sealed_array_to_tagged` (was 2) and **0** `lin_object_get` for the element — only const-offset `getelementptr`+`load`. Same for `map`/`reduce`.
- **Measured (by me, low load): 2.84s → 0.63s ≈ 4.5×** on a packed-`Pt[]` iteration microbench, output identical (29970000000). interp **neutral** (0.44s, RESULT 10460000), records **neutral**, RAPTOR digest **byte-identical**, 682 integration tests green, ASan-clean (constant 72 B leak at N=1k and N=100k — no scaling). This is the first time across the whole effort that cost #2 was both eliminated AND shown a wall-clock win — two prior agents stopped short here (one reverted a fixable operand-box leak; this run fixed it via a concrete-type read coerced to the param's `Json` type with the box owned by the body scope).

  **The exact microbench (reproducible from this doc alone):**
  ```lin
  import { print } from "std/io"
  import { toString } from "std/string"
  import { range, for } from "std/iter"
  import { push } from "std/array"
  type Pt = { "x": Int32, "y": Int32 }
  val main = () =>
    var ps: Pt[] = []
    range(0, 1000).for(i => push(ps, { "x": i, "y": i * 2 }))   // build a 1000-elem packed Pt[]
    var acc = 0i64
    range(0, 20000).for(_ =>                                    // 20000 passes × 1000 elems = 20M reads
      ps.for(p => acc = acc + p["x"] + p["y"])                  // the hot in-place packed iteration
    )
    print(toString(acc))                                       // 29970000000
  main()
  ```
  Recipe: build the compiler release (`cargo build -p lin --release`) on master vs branch `path1-packed-records`, for EACH `cp target/release/deps/liblin_runtime-*.a target/release/liblin_runtime.a`, then `LIN_EMIT_IR=1 ./target/release/lin build mb.lin -o mb` and time `mb` (median of ≥5, `TIMEFORMAT='%3R'`, **at 1-min load < ~4** — other agents contend). IR check: `grep -c sealed_array_to_tagged mb.ll` → **2 on master, 0 on branch** (the materialize eliminated = the win). Master baseline ≈ **2.84s**, branch ≈ **0.63s**, both print `29970000000`. (The whole-program `lin_object_get` count drops 10→8; the residual 8 are the `range().for()` driver + `push`, not the packed element read — which is pure const-offset.)

### Step 3 — pack fixed-key records by default: PARTIAL, blocked at the repr oracle (the §H4/H5 seam, now precisely located)
- **Scalar fixed-key records already pack by default** (verified) and now iterate in place — that IS the win above.
- **String-field widening is built but DORMANT.** The in-place String field read (borrowed `load ptr` + retain-if-escapes) is implemented and harness-leak-clean for build/push/index/map/for/reduce (commit `39f8329c`). But flipping `Type::is_sealed_array_field_packable` to admit String **trips the repr Stage-2 oracle**: an `Index` on a packed String-field array threaded through a `T|Null` tail-recursive param has the *old type predicate* saying `Packed` while the *dataflow repr analysis* correctly demotes to `Boxed(Opaque)` at the union boundary. This is the §H4/H5 packed/boxed classification divergence — now pinned to a specific contradiction between the type-predicate gate and the repr pass at the union/tail-recursive boundary, rather than vaguely "hard." Gate reverted to scalar+Bool; the capability stays present, dormant.
- **The remaining work to finish Step 3 is precise:** reconcile the repr oracle/verifier with the widened gate — make the type-predicate gate agree with the dataflow repr at the `T|Null`/union/Index boundary (or drive packability purely off the repr pass, the ADR-062 single-owner direction) — *before* the gate widens. This is the named "highest-risk step," and it is genuinely the multi-day repr-pass reconciliation, not a one-liner.

### Net Path-1 verdict
The in-place ABI thesis is **validated**: Steps 1+2 make packed-scalar-array iteration ~4.5× faster with no regression, proving the materialization boundary (cost #2) was the right target and is fixable soundly. The lever for RAPTOR specifically, however, needs Step 3 (heap-field default-pack) because `Trip` has String/nested fields — and Step 3 is gated on the repr-oracle reconciliation above. **Mergeable now: Steps 1+2 (conservative scalar-only scope, 4.5× win, all gates green).** Deferred: Step 3 heap-field widening, blocked on the precisely-located repr-oracle divergence.

### ⚠️ Note 2026-06-09 — packing records and typing DICTIONARIES are different, non-competing levers
This path packs `Json`/record *values* into structs. A **distinct, cheaper** lever — typing `Json`
*dictionaries* as `{ String: T }` maps — is what actually delivered the biggest RAPTOR speedup on master:
`8859f713` (`routeStopIndex` etc.) measured **PREP 144 s → 25.7 s (~5.6×)**, by routing `m[k]` to
`lin_map_get` instead of `lin_object_get`+box. It needs **none** of this path's machinery (no gate, no
packing, no repr oracle) — it's a type annotation. Keep the two separate when reasoning about RAPTOR: the
dictionary win is **already banked** (it's why master PREP is ~26 s now, not ~144 s); this path's
remaining target is the *record* values (`Trip`/`StopTime`) the query phase iterates, which a typed map
does not help. See path-0's RETROSPECTIVE 2026-06-09 — the two were conflated under one "de-`Json`-ing"
heading, which is part of why the dictionary win went uncredited to any path and the profiling pointed
five agents at the harder record/packing half.
