# Path 11 — The call adventure: every call is direct, unboxed, and visible to LLVM

**Status:** Open proposal. **A complete, self-contained journey** — this document bundles what were
briefly three separate proposals (lambda-set specialization, the whole-program spine, and ownership
conventions) into one path walkable end-to-end without reading anything else. Its sibling is
[Path 10 — the data adventure](path-10-the-data-adventure.md); the two are **independent
adventures, not stages of each other** — they attack different measured bottlenecks (this one owns
interp; Path 10 owns RAPTOR), can run in parallel or alone, and each duplicates the shared
groundwork it needs (Leg 1 here = Leg 1 there; row-shape monomorphization appears in both). If both
run, build the duplicated legs once — they are identical. **No userland language change.** This is
the successor to [Path 6](path-6-eliminate-call-dispatch-cost.md) /
[Path 8](path-8-make-functions-free.md): it keeps their shipped wins (6a fusion ~3.3×, 6b dispatch)
and replaces their per-combinator mechanism with the general pass their own findings said was
missing.

**Direction in one line:** make Lin's *calls* cost-free — every closure call whose callee set the
checker can see becomes a direct (or switch-dispatched) call with an unboxed environment and unboxed
arguments (lambda-set specialization, PLDI'23 / the Roc model), then compile the hot runtime core
into the program module as LLVM bitcode so the now-direct call edges let box/unbox and
retain/release pairs cancel — the exact condition the <2% bitcode spike was waiting on.

**Finish line (measurable):** interp benchmark — target the fused-chain ratio (~3.3×) extended
across the whole combinator surface (`find`/`some`/`every`/`flatMap`/`partition`/`groupBy`/
`sortBy`/binary-search currently all indirect); % of closure call sites compiled direct (counted, not
estimated); the bitcode re-measure beating its recorded <2% baseline.

---

## 1. The measured target (why calls, why these three moves)

- interp is **call-bound, not data-bound**: the H12 ceiling test (all alloc+RC no-ops) recovered
  ~0%, while removing *calls* paid every time it was tried — in-place `for` ~3.2×, chain fusion
  ~3.3×, packed reduce ~34-55× on the microbench.
- The current mechanism is **ad-hoc singleton lambda-set specialization, one combinator at a time**
  (eta-expansion into `try_inline_combinator_wrapper` + `combinator_intrinsic`, covering exactly
  `map`/`filter`/`reduce`/`for`/`while`). The frontier finding
  ([[project_combinator_inline_frontier]]) says the trick cannot extend — the remaining combinators
  call the user callback *from inside a nested closure* — and names the missing piece: "a general
  no-capture-closure devirt pass (doesn't exist)."
- Path 8's Tier-3 devirt spike closed-negative for *named* calls (already direct). The residual
  indirection lives exactly at callback positions inside stdlib combinator bodies — i.e. it is 100%
  lambda-set-shaped.
- Every closure call pays the uniform boxed ABI (`codegen/call.rs`): `(ptr env, ptr boxedArgs...) ->
  ptr` — box each scalar arg, indirect call, unbox inside the wrapper, box the result, unbox at the
  caller. 2-3 boxings and two representation round-trips per element where Go does one add.
- The bitcode-runtime spike ([[project_bitcode_runtime_spike]]) inlined 91 helper calls → 0 and
  bought **<2%**, because the consumers (indirect calls, `lin_tagged_arith`, `lin_object_get`)
  stayed opaque so box/unbox pairs never met in one function. The mechanism is proven; only the
  sequencing was wrong. This adventure is that sequencing.

## 2. Why one adventure (the dependency story)

Leg 2 (lambda sets) is what makes Leg 3 (bitcode LTO) pay — pairs can only cancel across *direct*
edges — and Leg 1 (ownership conventions) is what lets codegen stop emitting the defensive
retain/release and defensive boxes that would otherwise survive inlining as real work. Walked in
order, each leg raises the next leg's ceiling; walked alone, each is the measured-small version of
itself.

---

## Leg 1 — Ownership as IR fact: borrow/own/inout conventions + a call-edge verifier
*(duplicated verbatim as Path 10's Leg 1 — build once if both adventures run)*

### The diagnosis
The recurring RC bug class is one structural gap: ownership is an emergent property of scattered
retain/release emission, documented only in prose. The greatest hits — TCO param-slot overwrites
(`9a1a735`), releases in unreachable `tco_post` blocks (`b2e6d35`), the pinned sealed-record
param-slot leak (`codegen/types.rs:131`), union-boundary double-frees, the `Trip|Null` TCO UAF
(`object.rs:218`), projection re-CloneBox leaks. And because callee intent is unknown at every call
edge, every boundary pays *defensive* RC/cloning (the measured ~13× call-boundary cliff; Lobster's
equivalent compile-time ownership analysis removes ~95% of RC ops —
[Lobster](https://aardappel.github.io/lobster/memory_management.html)). For *this* adventure the
defensive-RC half is the active ingredient: inlining a call edge is only a win if what gets inlined
isn't padded with clones taken to satisfy an unknown callee.

### The move (Swift SIL / Lean "Counting Immutable Beans" model, kept internal)
1. Per-param/per-return conventions on every `LinIR` signature — `borrow` / `own` / `inout` —
   inferred by lowering (doubt → `own`, today's behaviour), with one hand-audited table for the
   runtime intrinsics in `RuntimeFns` (`codegen/runtime.rs`).
2. A call-edge/block verifier (owned args have owned-then-dead sources; borrows outlive the call;
   TCO back-edges release the old slot before the store; scope exits release exactly the owned-live
   set), run in **shadow mode** over the full corpus first — every violation is a latent bug or a
   wrong inference; zero behaviour change until clean.
3. Codegen consumes conventions; the per-site heuristics (`own_for_read`, `record_escape_alias`,
   `index_result_is_fresh_owned_box`, `tco_owns`) are deleted one at a time, ASan-gated.

**Allowed strictness tweak:** escaping borrows get a defined retain-on-escape; the
escaping-`var`-capture class (obj-literal closure-var segfault, worker captured-var garbage) gets a
checked capture convention instead of a latent miscompile.

---

## Leg 2 — Lambda-set specialization: the general closure-devirt pass

### The technique (and why it fits Lin specifically)
**"Better Defunctionalization through Lambda Set Specialization" (PLDI'23,
[doi 10.1145/3591260](https://dl.acm.org/doi/10.1145/3591260))**, the design Roc ships as "lambda
sets" ([roc#5969](https://github.com/roc-lang/roc/issues/5969)). The type system annotates each
function type with the *set* of syntactic lambdas that can inhabit it; higher-order functions
specialize per set; a closure value becomes an unboxed tagged union of the set's environments; the
call becomes direct (singleton) or a jump table (small set). Reported: up to 6.85× under MLton,
3.45× under OCaml. It is **type-directed** — no whole-program 0CFA — so it rides Lin's existing
checker propagation and monomorphization machinery (a set component on `Type::Function`, unified
like any other component, carried on the monomorphization key). And it **handles captures**: each
lambda's environment is a known struct, the per-set union is unboxed and stack-allocatable — the
thing the `combinator_intrinsic` trick could never do.

### The move
1. **Shadow inference:** compute lambda sets in the checker; emit `LIN_COUNT`-style statistics (% of
   call sites singleton / small-set / ⊤) on interp + RAPTOR + stdlib tests. Expectation from the
   frontier memo: interp's hot callback sites are overwhelmingly singleton. Payoff validated before
   any lowering work.
2. **Singleton, no-capture:** direct-call lowering (supersedes eta-expansion;
   `try_inline_combinator_wrapper` becomes a deleted special case). Gate: suite + ASan +
   no-scaling-leak + interp bench.
3. **Singleton, with-capture:** unboxed concrete env, stack-allocated when non-escaping (composes
   with the existing escape analysis). **This is the step that unlocks
   `find`/`some`/`every`/`flatMap`/`partition`/`groupBy`/`sortBy`.**
4. **Small-set switch dispatch** (threshold ~8) + per-set specialization of generic stdlib
   higher-order functions. ⊤ sets (FFI, `Json`-stored, worker-transferred closures) keep today's
   boxed ABI byte-identically — the demotion wrapper is the wrapper codegen already emits.
5. **Unboxed calling convention** for specialized calls: the callback parameter stops being
   `(ptr env, ptr boxed...) -> ptr` and takes concrete unboxed types. The per-element box/unbox
   round-trip disappears at the source instead of waiting for LLVM to cancel it (it can't across an
   indirect edge — the spike proved it).
6. Serialize the set component through the `.lin-cache` signature (cache-format bump — coordinate
   with Path 10's stamp bump if both adventures run).

Risks: set explosion (mitigated by the small-set threshold + the dedup machinery monomorphization
already uses; Roc ships this in production), recursion through function-typed record fields (widen
to ⊤ at the knot, as Roc does).

---

## Leg 3 — The whole-program spine: the runtime joins the program

### Gate first
**Re-run the rebased bitcode spike** (`spike/bitcode-runtime`) after Leg 2 stage 3 lands, on interp
+ RAPTOR. **Proceed only if >5%** — otherwise park with the new number recorded. The <2% baseline
is *expected* to invert once call edges are direct (the cross-language-LTO literature is explicit
that the win is paired-operation cancellation, not call-overhead shaving —
[LLVM blog](https://blog.llvm.org/2019/09/closing-gap-cross-language-lto-between.html)).

### The move (if the gate says go)
a. **Curated-core bitcode:** compile the hot runtime core (tagged ops, object/array/map accessors,
   RC helpers, string key-eq) to LLVM bitcode and link it into the program module before
   optimization, `alwaysinline` on the leaves. The cold 90% (fs/net/process/compress…) stays in the
   static archive — sidestepping the spike's fragility finding (hash-mangled internal statics in a
   full-Rust-bc link). Pin the rustc/LLVM pairing in CI; keep the archive fallback one cargo
   feature away.
b. **No internal-linkage games** — the spike proved they buy nothing (the program is already one
   module).
c. **Row-shape monomorphization:** extend the monomorphizer from type parameters to record shapes,
   so structurally-typed functions specialize to constant field offsets (MLton precedent,
   historically 2-6× whole-program; first-choice per
   [osa1's survey](https://osa1.net/posts/2023-01-23-fast-polymorphic-record-access.html)), deduped
   by layout (shapes with identical offsets-for-used-fields share a specialization). *(Identical to
   Path 10 Leg 2 step 6 — build once if both adventures run.)*
d. **Pipeline:** ThinLTO + the existing `--gc-sections` as the default release pipeline;
   `LIN_NO_OPT=1` remains the fast-build escape hatch. Watch compile time + code size on the
   largest example; tune inline threshold and specialization dedup until both budgets hold.

---

## Risks (whole-adventure)
- **Leg 1's hand-audited intrinsic table is load-bearing** — shadow mode cross-checks it against
  the corpus before anything changes behaviour.
- **Leg 2's ⊤ seams** must stay byte-identical (FFI, workers, `Json`); the demotion wrapper is
  existing codegen, but it is the place to concentrate review.
- **Leg 3 may be cancelled by its own gate.** That is the gate working; the adventure's finish line
  is the interp number and the direct-call-site percentage, not leg count.
- Compile-time growth (monomorphization breadth + LTO) is the classic MLton cost — budgeted
  explicitly in Leg 3d.

## Shared/duplicated work with Path 10
Leg 1 is identical to Path 10's Leg 1 — build once. Leg 3c (row-shape monomorphization) is
identical to Path 10's Leg 2 step 6 — build once. Both adventures bump the `.lin-cache` format —
coordinate as one bump. Everything else is disjoint: this adventure lives in the checker's function
types, the closure lowering, and the build pipeline; Path 10 lives in record layout and the value
representation. Different hot paths, different benchmark gates, freely parallel.

## What stays dead on this route (measured, do not revisit)
Leaf-helper inlining alone (Path 6c, ~2-3%), Tier-1 bitcode *as a leading move* (<2%,
[Path 8](path-8-make-functions-free.md)), internal linkage, Tier-3 named-call devirt (already
direct), box pooling, and — for call-bound code — allocation-side work generally
([Path 7](path-7-tracing-gc-foundation.md) closed-negative;
[Path 3](path-3-arena-allocation-construction-cost.md)/[4](path-4-whole-program-region-inference.md)
superseded).
