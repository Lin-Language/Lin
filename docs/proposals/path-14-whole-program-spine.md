# Path 14 — The whole-program spine: the runtime joins the program, shapes monomorphize to saturation

**Status:** Open proposal, **deliberately sequenced LAST** — this is the multiplier you apply after
[Path 10](path-10-layout-as-a-type-system-fact.md)/[11](path-11-lambda-set-specialization.md)/
[12](path-12-eight-byte-tagged-value.md) thin the seams, not a lever to lead with. Its leading move
was already spiked and measured **<2% in today's architecture**
([[project_bitcode_runtime_spike]], Path 8 Tier 1) — that result is *expected* to invert once the
consumers are direct and packed, and this path exists to re-collect it then. **No userland language
change.**

**Direction in one line:** compile the hot runtime core as LLVM bitcode **into the program module**
(the Julia/Swift model) so the now-thin helpers inline and paired box/unbox + retain/release fold to
nothing; extend Lin's existing monomorphization from type parameters to **row shapes**, so every
structurally-typed function specializes to constant field offsets; make ThinLTO the default release
pipeline. Lin compiles whole programs already — this is its natural endgame, and it is the one
advantage separate-compilation languages (Go included) structurally cannot match.

---

## 1. Why last (the measured sequencing fact)

The Tier-1 spike did exactly what it promised mechanically — **91 helper calls → 0 inlined** — and
bought **<2%**, because the *consumers* (`lin_tagged_arith`, `lin_object_get`, indirect closure
calls) stayed opaque, so box/unbox pairs never met in one function and never cancelled. The
cross-language-LTO literature agrees with the mechanism: inlining across the runtime boundary pays
when it lets the optimizer **cancel paired operations**, not when it shaves call overhead
([LLVM cross-language LTO](https://blog.llvm.org/2019/09/closing-gap-cross-language-lto-between.html),
[rustc linker-plugin-LTO](https://doc.rust-lang.org/rustc/linker-plugin-lto.html)). After Path 11
makes the calls direct and Path 10/12 make values flat, the pairs *do* meet:
`box(x); …; unbox(x)` in one visible function is dead code to LLVM, and `retain(p); …; release(p)`
folds under its TBAA/noalias facts. The <2% number is stale the day those land; treat it as a
baseline to re-measure, not a verdict.

## 2. Mechanism

### 2a. Runtime-as-bitcode, done right this time
- Compile a **curated hot core** of `lin-runtime` (tagged ops, object/array/map accessors, RC
  helpers, string key-eq) to LLVM bitcode at build time and link it into the program module before
  the optimization pipeline, `alwaysinline` on the leaf helpers. The cold 90% (fs, net, process,
  compress…) stays in the static archive — this sidesteps the spike's fragility finding
  (hash-mangled internal statics in a full-Rust-bc link) by never linking the full runtime as
  bitcode.
- Pin the Rust/LLVM version pairing in CI (the known operational cost of bitcode runtimes).
- Internal-linkage games are **not** part of this path — the spike proved they buy nothing (the
  program is already one module).

### 2b. Row-shape monomorphization (the structural-subtyping endgame)
Lin already monomorphizes generics per type argument. Extend the same machinery to **record shapes**:
a function taking `{ x: Int, ... }` (width-subtyped) specializes per concrete caller shape, so the
field offset is a **compile-time constant** in every specialization — no evidence-passing, no
runtime offset lookup. This is the literature's first-choice answer for row-polymorphic access
([osa1's survey](https://osa1.net/posts/2023-01-23-fast-polymorphic-record-access.html)) and the
MLton precedent (whole-program monomorphization + defunctorization, historically 2-6×,
[Weeks 2006](http://www.mlton.org/References.attachments/060916-mlton.pdf)). It is also
[Path 10](path-10-layout-as-a-type-system-fact.md)'s named answer to its width-subtyping risk — the
two paths share this stage.

### 2c. The pipeline
ThinLTO + `--gc-sections` (already in place) as the default `lin build` release pipeline; the
existing `LIN_NO_OPT=1` stays the fast-build escape hatch. Compile-time cost is the budget to watch
(§4).

## 3. Staged plan

1. **Re-baseline:** after Paths 10 stage 4 + 11 stage 3 land, re-run the Tier-1 spike branch
   (`spike/bitcode-runtime` rebased) on interp + RAPTOR. **Gate: proceed only if >5%** — otherwise
   park again with the new number recorded.
2. **Curated-core bitcode** (2a) productionized: build-system work in `lin-compile` (emit bc, link
   order, version pinning), `alwaysinline` annotations in `lin-runtime` behind a cargo feature.
3. **Row-shape monomorphization** (2b), shared with Path 10: shape-keyed specialization in the
   existing monomorphizer, dedup by layout (two width-compatible shapes with identical
   offsets-for-used-fields share a specialization — keeps the explosion bounded).
4. **Measure the canonical pair** (interp + RAPTOR) plus compile-time on the biggest example; tune
   the inline threshold and the specialization dedup until both budgets hold.

## 4. Risks
- **Compile time and code size:** monomorphization breadth is the classic MLton cost. Mitigations:
  layout-dedup (stage 3), specialization only on *used-field sets* rather than full shapes, and the
  module cache already amortizing unchanged imports.
- **Toolchain coupling:** bitcode ties `lin-runtime`'s rustc to the inkwell LLVM major. CI-pinned;
  the static-archive fallback (today's path) remains one cargo feature away.
- **The gate may say no:** if stage 1 re-measures small, this path stays parked — it is cheap to
  hold, because stages 2-3 are independently useful engineering (row monomorphization serves Path 10
  regardless).

## 5. Relationship to other paths
- **Multiplies** Paths 10/11/12; **shares** row monomorphization with Path 10; **replaces** Path 8
  Tier 1 with its correctly-sequenced version.
- Closes the loop on the project's two standing "re-measure later" notes: the bitcode <2% and the
  6c box/unbox-cancellation dead end — both were waiting on exactly the consumer-side work that
  precedes this path.

---

## Appendix — the five-path map (10-14) and what stays dead

Two measured bottlenecks drive everything: RAPTOR is **representation-bound** (631 M linear-scan
record reads + 3.5 B box ops), interp is **call-bound**; neither is allocation-bound (the ceiling
test recovered ~0%).

| Path | Bottleneck owned | Sequencing |
|------|------------------|------------|
| [10 — layout as a type-system fact](path-10-layout-as-a-type-system-fact.md) | RAPTOR's record reads; the repr-mismatch bug class | Start now (stage 1 = the two pinned 9-E/TCO fixes) |
| [11 — lambda-set specialization](path-11-lambda-set-specialization.md) | interp's indirect calls; the combinator frontier | Parallel with 10 |
| [12 — 8-byte tagged value](path-12-eight-byte-tagged-value.md) | the residual dynamic seams (`Json`, unions) | After 10; go/no-go on a re-profile |
| [13 — ownership conventions](path-13-ownership-parameter-conventions.md) | the RC bug class; defensive RC/clone churn | Start early; gates 12's borrowed reads |
| [14 — whole-program spine](path-14-whole-program-spine.md) | cross-boundary cancellation; width subtyping | Last; gated on a >5% re-measure |

**Closed-negative, do not revisit for perf** (all measured in this repo): tracing GC
([Path 7](path-7-tracing-gc-foundation.md)), arenas/regions
([Path 3](path-3-arena-allocation-construction-cost.md)/[4](path-4-whole-program-region-inference.md)),
box pooling/mimalloc, inline caches ([Path 2](path-2-fast-dynamic-inline-caches.md) — built, 99.56%
hit rate, still a wash), value-semantics records ([Path 5](path-5-value-records.md) — premise
falsified, breaking), Tier-1 bitcode *as a leading move*, leaf-helper inlining alone, and
incremental/partial RAPTOR typing.

The external existence proof for the destination: **Roc** — structurally typed, refcounted,
LLVM-backed, the closest architectural cousin to Lin — reaches near-C++ benchmarks with exactly this
recipe (default-unboxed layouts ≈ Path 10, lambda sets ≈ Path 11, monomorphization ≈ Path 14).
Nothing in Lin's semantics prevents the same endpoint; the gap is representation and calling
convention, both still freely changeable at this stage of the language.
