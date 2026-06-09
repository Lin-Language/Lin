# Path 8 — Make functions free: demolish the call/closure/dispatch abstraction before *and* with LLVM

**Status:** Open proposal, **now spike-tested and reconciled with the full cost map (see the dated
sections at the bottom — read them first).** The architectural framing + concrete current-ABI findings
behind the call/dispatch lever. **Complements [Path 6 (eliminate-call-dispatch-cost)](path-6-eliminate-call-dispatch-cost.md)**
(the IR-level inlining/fusion/monomorphization mechanisms = Tiers 2–3 here) and **hands off the
representation half to [Path 9 (end-to-end packed records)](path-9-end-to-end-packed-records.md)**. This
path adds **Tier 1** (the LLVM never gets to *see* the helpers — a linked `.a`, not bitcode) + **Tier 4**
(loop forms) + the compounding model. **No userland language change.** **Key measured updates since first
draft:** Tier-1-alone buys <2% (sequencing reversed — Tier 2 leads); and the cost is **two bottlenecks in
two programs** (interp = calls → this path; RAPTOR = `Json`-record reads → Path 9) — see "THE COMPLETED
COST MAP".

**Direction in one line:** Lin is a functional language where *everything is a function* — `for`, every
combinator, every loop — and it is slow because those abstractions are still **standing as runtime
artifacts** (heap closures, boxed args, indirect calls, opaque helper calls) at the moment LLVM tries to
optimize. Fast languages (Rust, Zig, Go, C++) **demolish the abstraction before the optimizer runs**; Lin
hands LLVM the abstraction intact. The fix is to demolish it — in two places that *compound*.

---

## Why this path exists — the ceiling test says the cost is the *code*, not the data

Path 6's anchor measurement (H12, the `LIN_NO_RC`/arena ceiling test) is decisive: removing the **entire**
allocation + refcounting subsystem from interp gave **no speedup** (0.48 / 0.42 s vs 0.408 s baseline). So
the recoverable cost is not the data (alloc/RC/layout — Paths 1–5/7's targets) and not reads (already
const-offset). **It is the non-inlined calls.** And the one change that *did* move interp — Stage 1's
in-place `for`, ~3.2× — won by *removing per-element calls*, not by touching data. The code is the cost.

This path explains **why** the calls are so expensive (the current ABI, read from the codegen below) and
lays out the **full menu** of how to make them free — of which Path 6's IR-inlining is the middle tier.

> ## ⚠️ Reconciliation with the RAPTOR profile — "reads are already const-offset" is true ONLY for SEALED records
> The RAPTOR cost-attribution profile (`investigate/raptor-typed-profile`, measured by env-gated hot-call
> counters; `perf`/`valgrind` unavailable) found the query-phase bottleneck is **field reads after all** —
> but specifically reads of **`Json`-typed** records: **756 M `lin_object_get`, of which 631 M (83%) are
> LINEAR SCANS** over small (<16-key) objects, plus **~3.5 B value-box ops** (TAGGED_RELEASE 1.37 B /
> CLONE 1.12 B / ALLOC 1.02 B). This is **not** a contradiction of the interp ceiling test — it is the
> resolution of both: a field read is a const-offset `FieldGet` **only when the value's static type is a
> sealed record**; a **`Json`-typed** value's read is *always* `lin_object_get` (verified in codegen:
> boxed objects have no const-offset path; runtime objects with <16 keys *always* linear-scan, no hash
> index — verified in `lin-runtime/src/object.rs`). **interp's records are typed/sealed → its reads are
> already cheap → its residual cost is the calls (ceiling test). RAPTOR's hot records are `Json` → its
> reads are 631 M linear scans → its bottleneck is the boxed-`Json` representation.** Two different
> programs, two different dominant costs, both measured — and the lever differs accordingly (RAPTOR needs
> de-`Json`-ing to sealed *with* the consuming ops able to read packed; interp needs the calls inlined).
> **The crucial RAPTOR finding (and the trap):** *partial* typing is a **measured ~13% REGRESSION** —
> typing a value read back from a still-`Json` source *materializes a fresh sealed struct per access*
> (SEALED_ALLOC 468→65 M, RC_RETAIN 778 M→2.09 B) on top of the unchanged `lin_object_get`. So RAPTOR's
> fix is **all-or-nothing end-to-end** packed records (loader → `createRaptor` → map store → read), not
> incremental typing — and it is blocked today by (1) packed records with a heap-array field
> (`stopTimes: StopTime[]`) still boxing the inner array, and (2) the loader needing a real `Json→Trip[]`
> decode. That is a **representation** effort (Path 1 / Path 5 territory), *distinct from this path's
> call-cost lever* — RAPTOR and interp need different primary fixes. See `benchmarks/compare/raptor/lin/NOTES.md`.

## The architectural finding — what's actually happening (read from `crates/lin-codegen`)

Three facts about how Lin compiles today explain the entire call cost:

1. **The runtime is a linked static archive (`liblin_runtime.a`), NOT bitcode.** `lin_object_get`,
   `lin_box_int32`, `lin_unbox_int64`, `lin_tagged_arith`, `lin_string_concat`, … are **opaque external
   symbols** at optimization time. The codegen runs `module.run_passes("default<O2>", …)` over *user code
   only*; LLVM **cannot see into a single runtime helper** — cannot inline it, cannot cancel a
   `box(x)`→`unbox` round-trip (which today goes through *memory*), cannot fold arithmetic through
   `lin_tagged_arith`. Each helper is a hard call boundary wrapping a few instructions of real work.
   *(This is why the prior whole-program-LTO investigation found "no win" — the codegen-level inlining
   that LTO needed to act on was never there. This path is the targeted version.)*
2. **The closure ABI is uniform-boxed + indirect.** A closure is a 48-byte heap struct
   `{rc, _pad, fn_ptr@8, env_ptr@16, env_size@24, dflt_desc@32, cap_desc@40}` (`codegen/call.rs`). Every
   call is `builder.indirect_call(fn_ptr, …)` with **every argument boxed to a `TaggedVal*`** (the uniform
   ABI). LLVM **cannot devirtualize or inline an indirect call** — it does not know the target. So
   `range(0,n).for(f)` is, per element: box the index → indirect-call through `fn_ptr` → inside the
   callee, unbox → do the work → box the result → release. None of it visible to the optimizer.
3. **User functions are emitted with EXTERNAL linkage** (`module.add_function(name, ty, None)`). External
   functions must be preserved as callable symbols, which **throttles LLVM's own inliner** across user
   functions even when the target *is* statically known.

**So the problem is not "LLVM isn't good enough."** It is that the abstraction (heap closure, boxed args,
opaque helpers, indirect call, external linkage) is **erected at runtime and never torn down** before the
optimizer looks. Rust monomorphizes + inlines an entire `.map().filter().sum()` into one loop *before*
LLVM; Lin presents LLVM a tree of opaque indirect calls and asks it to optimize around them. It can't.

## The model — two demolition sites, and they *compound*

There are exactly two places to make "everything is a function" fast:

- **(A) Demolish it at the IR level, before LLVM.** Turn the call into straight-line code in `lin-ir`
  yourself, so LLVM receives a flat loop with no closure, no boxing, no indirect call — code it is
  brilliant at. (Stage 1's in-place `for`; Path 6's 6a/6b; Rust iterators.)
- **(B) Let LLVM demolish it.** Give LLVM what it needs to inline *itself*: the runtime as **bitcode**
  (see into the helpers), user functions as **internal linkage**, indirect calls turned into **direct**
  calls where the target is known.

**The critical insight: (A) and (B) multiply.** Today even Stage 1's perfectly-inlined loop *still* calls
opaque `.a` helpers, so half its potential win is left on the table — LLVM inlines the closure body but
then hits a wall of `lin_box`/`lin_unbox`/`lin_object_get` external calls it cannot cancel. Do (B) and the
box/unbox pairs an inlined loop produces finally **cancel**, the closure env **SROAs into registers**,
invariants **hoist**, arithmetic **folds**. (B) is what turns (A)'s output from "fewer calls" into "C-speed
straight-line code." Neither alone reaches the ceiling; together they do.

---

## The 4-tier menu (ranked by leverage × inverse-effort; all zero userland change)

### Tier 1 — Unblock LLVM (SPIKED — confirmed it pays off ONLY after Tier 2/3, not alone)
- **Ship `lin-runtime` as embedded LLVM bitcode**, `llvm-link` it into the module *before* `run_passes`,
  and mark the hot leaf helpers (`lin_box_*`, `lin_unbox_*`, `lin_object_get`, `lin_tagged_arith`,
  small string/array accessors) `alwaysinline`/`inlinehint`. Intended effect: LLVM inlines the helpers and
  **cancels the box/unbox round-trips that today go through memory**, folds tag-known arithmetic, SROA's
  the boxed values.
- **Internal linkage for all non-exported user functions** (`set_linkage(Internal)`).

> **⚠️ SPIKE RESULT (`spike/bitcode-runtime`, `bf72f308`, measured — do NOT lead with this tier).** The
> visibility mechanism **fires exactly as hypothesized** (box/unbox helper calls 91→0, helpers inlined),
> but the box/unbox round-trips **do NOT cancel**, and the wall-clock win on realistic benchmarks is
> **under 2%** (interp −0.6%, object_access −1.8% — within noise). The one real win (a synthetic tight box
> loop, **−16%**) was *call/branch-overhead removal, not cancellation*. **Why no cancellation:** the
> operation that *consumes* the boxed value — `lin_tagged_arith`, `lin_object_get`, or the **indirect
> closure call** — is still an opaque boundary, so LLVM cannot prove the box doesn't escape and cannot
> elide it. *Inlining the producer (`box`) is worthless while the consumer stays opaque.* This is the
> "tiers compound" claim **confirmed empirically as a negative**: Tier 1 in isolation has little payoff;
> it only pays after Tier 2 (inline the closure → kill the indirect call) and Tier 3 (inline/devirtualize
> the consuming helper) put `box` and its consumer in the *same* function where the pair can cancel.
> Also measured: **internal linkage (B1) bought nothing** — the whole program is already one LLVM module,
> so external linkage was not actually throttling the inliner in practice.
> **Feasibility:** LLVM versions match (rustc LLVM 22 / inkwell llvm22), so `parse_bitcode` +
> `link_in_module` pre-O2 works. BUT the *full Rust-runtime* bc link is **genuinely fragile** — the
> helpers reference per-build hash-mangled `internal` statics (int/bool caches, panic-location, async/fault
> vtables) not linkable from the `.a` and not isolable (`llvm-extract` drags in 4600+ globals). A sound
> productionized version needs the bc **and** the `.a` built from *one* compilation with stable C-ABI
> exported symbols for those statics — a real build-system change, not a flip. The spike's working route
> was *hand-authored LLVM IR* for the box/unbox/tag helpers (links cleanly, byte-identical) — fine for a
> spike, not maintainable (duplicates runtime semantics, drops the small-int cache).
> **Verdict: Tier 1 is necessary plumbing for the *end* state, but it is NOT a lead move and NOT a cheap
> win — it is sequenced AFTER Tier 2/3 remove the consuming boundary, and its integration cost is real.**

### Tier 2 — Demolish the closure/combinator abstraction at the IR level (Path 6's 6a; the headline win)
- **Generalize the inlined-callback lowering** from `for`/`length` (shipped, Stage 1) to
  `map`/`filter`/`reduce`/`sortBy`: a literal/capturing closure becomes the loop body; the element is read
  in place; captures are read from the env by `load`, not via an indirect call. No per-element arg box, no
  indirect call, no per-element release.
- **Fuse combinator chains** (`xs.map(f).filter(g).reduce(h)`) into one pass — no intermediate arrays, no
  per-stage closure call (the Rust-iterator win).
- *Risk:* CFG/SSA correctness at the inline site (the latch-wiring hazard from prior fusion work) and the
  per-element element-box RC (Stage 1's leak — fixed by giving the fused read the concrete field type, not
  `Json`). ASan `detect_leaks=1`-with-scaling is mandatory (the Stage-1 lesson).

### Tier 3 — Kill the boxed ABI and the indirect call (deepest, highest ceiling)
- **Devirtualize statically-known call targets.** The overwhelmingly common case — `xs.map(literalLambda)`,
  a call to a named `val` function — has a *known* callee. Emit a **direct call**, not `indirect_call`, so
  LLVM can inline it. (Today everything goes through the boxed `{fn_ptr,env_ptr}` indirect path.)
- **Monomorphize the polymorphic stdlib per concrete element type** (`length$Int32`, `for$Trip`,
  `push$StopTime`) so there is **no shared boxed ABI to box into** — the boundary is *dissolved*, not
  special-cased. Lin already monomorphizes generics partially; the `get<T,D>` record-array link error is
  literally a missing instance.
- **A specialized unboxed calling convention** for monomorphic sites: pass scalars/records in registers or
  by `sret`, not as `TaggedVal*`. The biggest structural change and the highest ceiling — it attacks the
  boxing *at the call boundary itself*, which Tiers 1–2 only mitigate.
- *Risk:* monomorphization compile-time/binary-size cost and the machinery's history of biting
  (`mangle_type` collisions, the `get<T,D>` failure); the unboxed-convention ABI must interoperate
  correctly with the boxed fallback at every boundary (the recurring repr-mismatch class). Incremental,
  measured, ASan-gated.

### Tier 4 — The loop forms specifically ("a for loop is a function; a while loop is a function")
- `range(a,b).for(f)` already fuses to a counted loop (shipped, `23c3e1f`). Extend the same counted-loop
  recognition to the other loop-shaped combinators and to **`while`-as-recursion**.
- Make **self-tail-recursion** (already TCO'd to a loop) **compose with closure inlining**, so a recursive
  functional loop whose body calls an inlined closure becomes a flat LLVM loop with the body inlined — the
  functional-loop equivalent of Tier 2.

---

## Sequencing (REVISED after the Tier-1 spike — Tier 2 leads, not Tier 1)

The Tier-1 spike falsified the original "Tier 1 first" plan: making the producer helpers visible buys
nothing while the *consumer* (the indirect closure call, `lin_tagged_arith`, `lin_object_get`) stays
opaque. The boundary that must fall first is the **consumer**, which is Tier 2/3. Revised order:

1. **Tier 2 LEADS** (generalize combinator/closure inlining) — the proven mechanism (Stage 1's ~3.2×), the
   headline per-element win. Inlining the closure **removes the indirect call** that was blocking box
   elision, so it *also* unlocks Tier 1's cancellation as a side effect (producer + consumer land in one
   function).
2. **Tier 3** (devirtualize known targets; monomorphize the polymorphic stdlib; unboxed convention) — the
   deep structural fix that makes the *consuming* helper (`lin_tagged_arith`/`lin_object_get`) direct/typed
   so the box it consumes can finally fold. Highest ceiling, highest risk; incremental and measured.
3. **Tier 1 LAST** (bitcode runtime + alwaysinline) — once Tier 2/3 have put producers and consumers in the
   same function, *then* making the leaf helpers inlinable lets the pairs cancel. Spike-measured to be
   worthless before that point, and its integration cost is real (the stable-ABI bc/`.a` co-build) — so it
   is the *finishing* pass, not the opening move.
4. **Tier 4** woven in where the loop shape is the bottleneck.

The in-flight RAPTOR profile tells us the *mix* — how much of the hot path is per-element closure calls
(favors Tier 2) vs polymorphic dispatch (favors Tier 3) vs the separate string-allocation frontier Path 6
flags — and therefore where within Tier 2/3 to concentrate for *that* workload.

## What this fixes / does not fix

- **Non-inlined call/dispatch + opaque-helper cost (#4):** yes — the measured recoverable cost. Tier 1
  unblocks the helpers; Tier 2 removes the per-element closure call; Tier 3 dissolves the boxed dispatch
  ABI; Tier 4 the loop forms.
- **Reads (#1):** already fast.
- **Boxing boundary (#2):** dissolved per-op by Tier 3's monomorphization; mitigated by Tier 1's box/unbox
  cancellation.
- **Construction/RC (#3):** not the target — the ceiling test says it is not recoverable for these
  workloads; Tiers 1–2 incidentally remove per-element arg-box alloc/RC as a side win.
- **The RAPTOR query-phase string-allocation frontier** (Path 6's companion note, ~3.5 B string allocs):
  **not touched** by this path — a separate frontier (string interning / avoiding per-access key
  construction).

## Relationship to the other paths

- **Path 6 is Tiers 2–3 of this menu** stated as standalone mechanisms; this path adds Tier 1 (the
  LLVM-can't-see-the-helpers finding) + Tier 4 + the compounding model. They should be read together; Path
  6 is the recommended primary direction and this is its architectural foundation and missing first tier.
- **Supersedes Paths 1/3/4/5/7 as the primary lever** for the headline benchmarks (the ceiling test shows
  their data/memory-model targets are not the recoverable cost). Those remain for niche cases (Path 5 for
  array-of-struct *bandwidth*; Path 7 only if RAPTOR is measured allocation-bound unlike interp).
- **Composable with Path 2 (inline caches)** for residual genuinely-`Json` reads.

## Acceptance gates

Full `cargo test --workspace` green; the combinator/iterator/stream corpus green (the call-heavy suites are
the guard); RAPTOR digest byte-identical (`group=26203913 range=773022892 journeys=139`); **ASan
`detect_leaks=0` AND `=1`-with-scaling** on any fused/inlined combinator (Stage-1-leak lesson); the
**mechanism in IR** (Tier 1: box/unbox pairs cancelled, helper bodies inlined; Tier 2/3: no per-element
indirect closure call / no shared-ABI box in a typed hot path) **and** the wall-clock (median ≥5 at low
load — the box is contended). Tier 1 specifically must show the runtime-bitcode link is debug-info-clean
and does not regress build time unacceptably.

## Verdict

Lin is slow because "everything is a function" is compiled as "everything is a *runtime artifact LLVM
can't see through*" — a linked-`.a` opaque helper, a boxed indirect closure call, an external-linkage
function. The fix is to demolish the abstraction in two compounding places: **let LLVM demolish it** (Tier
1: bitcode runtime + alwaysinline + internal linkage — cheap, never tried, multiplies everything else) and
**demolish it yourself at the IR level** (Tiers 2–4: inline/fuse combinator+closure calls, devirtualize +
monomorphize the dispatch, flatten the loop forms — Path 6's mechanisms, proven once at ~3.2×). This is
exactly how Rust/Zig/Go reach C speed: the functional design is not the enemy; *leaving the abstraction
standing at runtime* is. Zero userland change. **Lead with Tier 2** (combinator/closure inlining — kills
the indirect call that blocks everything), then Tier 3 (devirtualize/monomorphize the consuming helper),
then **Tier 1 LAST** as the finishing pass (the spike proved it buys <2% until the consumer boundary
falls). For interp and call-bound code this is *the* lever; for `Json`-read-bound code (RAPTOR) see the
two-bottleneck map below.

---

### ✅ Corroborating note 2026-06-09 — the biggest shipped RAPTOR win already validates this thesis (and refines the target list)
The single largest RAPTOR speedup on master, which no path claimed, is a direct instance of "the cost is
an opaque non-inlined call": typing the `Json` **dictionaries** (`routeStopIndex`/`bestArrivals`/
`kConnections`) as `{ String: T }` maps measured **PREP 144 s → 25.7 s (~5.6×)** (`8859f713`, on master).
Mechanism: `m[k]` on a `Json` object is fact-#1's opaque `lin_object_get` (+ box the result into a
`TaggedVal`); typing the value routes it to a lean `lin_map_get`. So it **removed a hot opaque helper
call** — Tier 1's exact target — but *without* Tier 1's bitcode-runtime machinery: a type annotation made
the compiler stop emitting the call at all.

The refinement for this path's target list: when enumerating the opaque helpers to demolish, separate
`lin_object_get` calls by the **kind** of value they read — a `Json` **dictionary** (string→value) is
removable *for free today* by typing it `{ String: T }`; a `Json` **record** read needs Tier 1/2/3 (or
packing) because there is no cheaper typed form for it. Cheapest-first: type the dictionaries, then
demolish the helpers that remain. This is why path-0's per-call-site-**class** profile (split
`lin_object_get` into dict-lookup vs record-read vs small-map, by source line) should run **before** the
Tier-1 spike — it tells you how much of the opaque-call cost is a type annotation away vs genuinely needs
the bitcode/inlining work. See **path-0 RETROSPECTIVE 2026-06-09** for the full root-cause of why this win
was invisible to the aggregate-`lin_object_get` profile that originally framed Paths 0–7.

---

### 🗺️ THE COMPLETED COST MAP 2026-06-09 — two bottlenecks, two programs, two levers (all measured)
The investigations are now reconciled into one picture. **There is no single bottleneck — there are two,
in different programs, needing different primary fixes.** Both are measured, not assumed.

| | **interp** | **RAPTOR (query phase)** |
|---|---|---|
| Hot records are… | typed / sealed | **`Json`** |
| So a field read is… | const-offset `FieldGet` (cheap) | **`lin_object_get` — 631 M of 756 M are LINEAR SCANS** (<16-key objects, no index) |
| Dominant cost | **the non-inlined calls** (H12 ceiling: deleting ALL alloc+RC = *no* speedup) | **the boxed-`Json` representation** (~3.5 B box ops: RELEASE 1.37 B / CLONE 1.12 B / ALLOC 1.02 B) |
| Primary fix | **this path** (Tier 2/3: inline the calls) | **end-to-end packed records** (Path 1 packed-by-default / Path 5's target) |
| Map lookups (`{String:T}`) | n/a | 269 M, **O(1), NOT the problem** (dead lever) |

**Why it's not a contradiction:** a const-offset read exists *only* for statically-sealed records; a
`Json` value's read is *always* `lin_object_get` (verified in codegen + `object.rs` HASH_INDEX_THRESHOLD=16).
interp's records are sealed → reads cheap → residual cost is the calls. RAPTOR's records are `Json` →
631 M linear scans → cost is the representation. **Measured trap:** *partial* RAPTOR typing **REGRESSES
~13%** — reading a `Json`-built value back as a typed record *materializes a fresh sealed struct per
access* (SEALED_ALLOC 65 M, RC_RETAIN 778 M→2.09 B) on top of the unchanged scan. So RAPTOR's repr fix is
**all-or-nothing end-to-end**, not incremental. See [[project_raptor_json_read_bottleneck]] and
`benchmarks/compare/raptor/lin/NOTES.md`.

**Dead levers, now measured-negative (stop proposing):** map-lookup cost (O(1)); RC/alloc in isolation
(interp ceiling test, [Path 7](path-7-tracing-gc-foundation.md) demoted); internal linkage (already one
module); Tier-1 bitcode *alone* (<2%, consumer stays opaque — [[project_bitcode_runtime_spike]]);
incremental RAPTOR typing (regresses 13%).

**Live levers, each targeting a *measured* dominant cost:**
1. **This path's Tier 2** (combinator/closure inlining) → interp + all call-bound functional code. Lower
   risk, proven mechanism (Stage 1 ~3.2×), ships incrementally. **The recommended first implementation.**
2. **Path 1 packed-by-default + the heap-field-array + loader-decode gaps** → RAPTOR + all `Json`-read-bound
   code. The *only* thing the profile shows will help it; the non-breaking form of Path 5. Bigger, riskier
   (the repr-oracle reconciliation Path 1 Step 3 names). **Scoped as its own continuation — see
   [Path 9](path-9-end-to-end-packed-records.md).**
3. **Secondary RAPTOR frontier:** 203 M string allocs from round-key `"${k}"` interpolation (string
   interning / avoid per-access key construction). Separate, smaller.

These two primary levers (1 and 2) **do not compete — they fix different programs** and can proceed in
parallel. This path owns lever 1; Path 9 owns lever 2.

### Reconciliation 2026-06-09 (path-6 implementation + the path1-packed-records head-to-head) — Tier 2/3 now have verified numbers + a built base
The path-6 implementation work measured Tier 2 and Tier 3 head-to-head against the OTHER 0/1/3 agent (`path1-packed-records`), tight interleaved low-load A/B, both correct:
- **Tier 2 (chain fusion) is REAL and the headline: ~3.4×.** `map().filter().reduce()` 200M: chain-fusion 0.27s vs per-combinator-in-place 0.94s. Fusing the whole chain (no intermediate arrays) beats per-combinator in-place by ~3.4× — so Tier 2's "fuse combinator chains into one pass" is the differentiating win, not just per-combinator inlining.
- **Tier 3 (the single-combinator in-place / no-per-element-materialize half) is ALSO real and ALREADY BUILT on `path1-packed-records`: ~55× on a lone packed `reduce`** (Pt[] 50M: in-place 0.013s vs routes-to-generic-std_iter_reduce 0.756s). i.e. the "reduce/map stop materializing per element" lever — which path-9 names as Step-3's prerequisite (cheap typed reads) and path-6 named as "the convergent next lever" — is DONE for the single-combinator case on that branch (`4de04817` in-place packed map/reduce, `470ea0ad` for/length).
- **6b length-dispatch: ~1.35×** (length-heavy loops; `std/array.length` is non-generic `(Json):Int32` → concrete call boxes; redirect to the `Length` intrinsic. `push`/`get` already monomorphized).
- **Tier-1 (linked-`.a` LLVM-blind helpers): NOT attempted by path-6 (its 6c); path-8's own spike already showed it's not a lead move.**
**MERGE ORDER (the two 0/1/3 branches are COMPLEMENTARY, merge both):** `path1-packed-records`'s in-place packed reduce/map/for/length (Tier 3 single-combinator, the bigger/more-fundamental win + the Trip|Null UAF fix 04bec701 + per-iter box-release 568756b0) is the BASE; rebase path-6's chain-fusion (Tier 2, ~3.4× on chains) + 6b length-dispatch on top. The combinator-chain fusion and the single-combinator in-place are the two halves of "demolish the combinator abstraction" — neither subsumes the other (fusion kills intermediates across stages; in-place kills per-element materialize within a stage). NEITHER branch should ship its RAPTOR full-typing `.lin` (verified ~2× regression — see path-6/path-9; all-or-nothing end-to-end packing is path-9's domain, and even then gated on Step-3 heap-field packing).

### ✅ MERGED 2026-06-09 (master `acf35a83`): Tier 2 (chain fusion) + Tier 3 single-combinator in-place + length-dispatch
The Tier-2 chain fusion (path-6 6a) and the Tier-3 single-combinator in-place reduce/map/for/length (no per-element materialize) — the two halves of "demolish the combinator abstraction" — both shipped, coexisting. Cross-lang pipeline 75ms→27ms (2.8×); ~34–55× single packed reduce. Tier 1 (linked-`.a` LLVM-blind helpers) and the deeper Tier-3 devirtualization/unboxed-ABI remain unbuilt; path-6's 6c (leaf-helper inlining) is the next increment.

### ✅ Step 8.1 — combinator fusion WIDENED to record sources + array terminals (VERIFIED by me; branch `perf/path8-step1-record-fusion`)
Extends the merged Tier-2 chain fusion (which was scalar-source + reduce/for-only) to **sealed-RECORD element arrays** and **array-producing terminals** (`map`/`filter`-ending chains build one result array, no intermediate per-stage arrays). A `trips.filter(...).map(...).reduce(...)` over a packed record array is now a single const-offset pass — `sealed_array_to_tagged` 2→0 (materialize boundary eliminated, IR-verified).

- **MEASURED: ~2.28×** (typed-record `filter.map.reduce` over 2M recs: master 6.509s → branch 2.860s, median of 5 at load <4.5, identical output 653067456, spreads <2%). I ran this myself.
- **Two PRE-EXISTING bugs found + fixed along the way** (both worse on master, so this is a strict improvement): (1) a per-element ~88 B/elem leak in any 3+-stage fused chain with a `map` stage — the map-stage produced value wasn't reclaimed (I caught this under ASan scaling; the agent's harness missed it because it only tests single-combinator cells, not chains); (2) a `map.filter.reduce` double-free UAF when `map` is the first stage. Fix commit `4f37b2aa` (reclaim map-produced/source values; `src_alive` flag). After fix: filter.map.reduce / map.filter.reduce / String-field chains all leak=42 CONSTANT across N=200/2000/20000 (string-intern residual, not scaling), UAF-clean, values correct.
- **Gates (I verified):** 694 integration + all crates green, 72/72 lin test, fmt clean, RAPTOR digest byte-identical `group=26203913 range=773022892 journeys=139`. **Status: verified-mergeable, UNMERGED (parallel exploration).**
- **Caveat (the cost-map reality):** this is a *secondary* lever — the Phase-0 call-site profile shows both interp and RAPTOR bottleneck on boxed heap-field RECORDS (Path 9), so Step 8.1 is a real ~2.3× for record-combinator-chain code but won't move the headline benchmarks alone. It IS shared groundwork (fused loops are where packed reads pay off).
