# Compiler coherence: structural consolidation proposal

Status: **in progress** (2026-06-20). Not yet an ADR — this is the rationale + plan that one or
more ADRs should be cut from as the work lands.

**Landed so far (Phase 0, the safety net):**
- RC-balance verifier over LinIR — `LIN_VERIFY_RC=1`, off by default (`34584403`). Clean over the
  whole corpus; ready to promote to a CI gate (needs the quiet-on-success + nonzero-exit tweak).
- RAPTOR-shaped unit corpus + CI gate — TS spec gaps ported, RAPTOR unit suite now blocks CI
  (`dfd750a5`).
- Cluster-3 down payment — the dot-call capture loop, the exact drift that caused bug #4, is now a
  single shared `record_capture_in_enclosing_fns` (`e93e4da9`).

**Still open:** ASan-as-a-dedicated-CI-job (Phase 0); the full dot→prefix desugar (Phase 1); the
`Repr` lattice (Phase 2, on `reset/main`); the offside helper (Phase 3). This doc stays until those
land — it is the roadmap for them, not a record of finished work.

## Why this doc exists

The RAPTOR-port campaign surfaced a run of compiler bugs that, fixed one at a time, look
unrelated. Looked at together they are not random — they share one meta-pattern and fall into a
small number of structural clusters. This doc names the pattern, classifies the bugs as evidence,
and proposes a *verifier-first, incremental* consolidation. It deliberately does **not** propose a
big-bang rewrite: master stays green, each step is independently shippable, and the verifiers are
added *before* the refactors they protect.

## The meta-pattern

> **The same semantic decision is computed in several places that must agree, and they drift.**

Almost every bug below is two parts of the compiler disagreeing about one fact — "what is the
physical representation of this value", "who owns this allocation", "is this name a capture",
"how many days does this loop run". The decision has no single home, so each site re-derives it and
the derivations fall out of sync.

## Evidence (bugs from this campaign)

| Bug | One-line | Cluster |
|-----|----------|---------|
| reduce→sealed-array double-free (`2e9f1891`) | boxed `reduce` result returned through `:T[]` used a *borrow* coercion, double-released | 1 repr/coercion |
| sealed→Named over-release UAF (`0f8ac060`) | `sealed_record_arg_materialized` predicate disagreed with what `lower_coerce_arg` actually does for `Named` params → spurious post-call full-release freed a live value | 1 repr/coercion + 2 RC |
| nested-map header corruption | downstream of the same materialize↔project coercion disagreement | 1 repr/coercion |
| dot-call capture drop (`0fc257a3`) | `infer_dot_call` builds the method callee as a `LocalGet` directly, bypassing `infer_ident` — the only place captures are recorded → call silently dropped, evaluated to null | 3 dual paths |
| parser else-attach (`911cae53`) | nested `if/else if` inside `()` mis-attached `else` because INDENT/DEDENT are suppressed inside delimiters (ADR-003) | 4 offside |
| (port) searchDay off-by-one (`ebae2e21`) | not a compiler bug, but the same shape: a loop-bound decision with no shared definition between port and reference | — |

The recurring UAF / double-free / leak class across the wider project (see
`MEMORY_MANAGEMENT.md` and the ownership notes) is the long tail of cluster 2.

## The clusters

### Cluster 1 — Representation & coercion coherence  *(highest value, deepest)*

"What physical representation does a value of type `T` have, and what conversion does
`repr(A) → repr(B)` require?" is spread across `compile_ir_coerce` (codegen `match.rs`),
`lower_coerce_arg` (`lin-ir` `coerce.rs`), `func.rs` return handling, and ~8 mutually-dependent
predicates (`sealed_record_arg_materialized`, `sealed_array_arg_materialized`,
`arg_box_is_caller_owned_shell`, `arg_box_is_caller_owned_scalar_shell`, …). These must agree by
hand. Bug #2 and #3 are literally two of them disagreeing.

**Target:** one `Repr` lattice + one `coerce(from_repr, to_repr) -> ConversionPlan` function that
**both** the lowerer and codegen consult. `repr.rs` is the seed; today the decision is re-derived
ad hoc at each site instead of owned there. (This is the same direction as the
"representation-reset" design notes: records = value types, dissolve `LinObject`, one `Repr`.)

### Cluster 2 — RC ownership as heuristics, not a proven discipline  *(cheapest high-value win)*

`own_for_read` / `register_owned` / `retain_call_arg` / `full_release_boxes` / `return_keep` /
`FreeBoxShell` — each rule carries a comment naming the specific ASan-found bug it was bolted on to
fix. That is the signature of a discipline maintained by accretion. Each new representation change
risks re-opening the class, and each instance currently costs a multi-minute ASan/lldb bisect to
find.

**Target:** a **static RC-balance verifier** over `LinIR` — for every value, retains == releases on
every control-flow path (and no use-after-release) — run in CI. Plus an ownership pass that is
actually *verified* rather than heuristic. This turns the entire UAF/leak class from "found late by
a 6-minute bisect" into a compile error, and de-risks every future repr change (Cluster 1).

### Cluster 3 — Dual name/call-resolution paths that hand-mirror  *(bounded, mechanical)*

`infer_dot_call` re-implements `infer_call`; the file is full of `// mirror of the infer_call rule`
comments and notes that the dot path "bypassed" several of them. Bug #4 was one more rule the dot
path forgot (capture recording). Two paths that are supposed to be identical post-desugar but are
maintained separately will keep drifting.

**Target:** desugar `x.f(a)` → `f(x, a)` **once, early** (parser or a dedicated desugar pass) so
there is a single callee-resolution path. The `TupleArgs` branch already does exactly this
(`Expr::Call { func: Ident(method) }` → `infer_expr`); generalizing it makes capture-recording and
every future "mirror" rule free. The blocker is ADR-085 expected-type/receiver-push ordering — the
receiver currently has to be inferred mid-resolution — so this needs the receiver-push to be
expressible on the desugared form first.

### Cluster 4 — ADR-003 indentation suppression inside delimiters  *(lowest priority)*

Suppressing INDENT/DEDENT inside `()[]{}` means every indentation-sensitive construct
(else-attach, blocks, match arms) needs its own hand-rolled offside-column rule. Bug #1 is one
instance; it is a recurring small-bug generator rather than a single defect. No rewrite proposed —
just awareness, and a shared offside-column helper if a third instance appears.

## Non-architectural, equally load-bearing: verification coverage

The reason these bugs cost 6-minute lldb bisects is that the **240k-trip RAPTOR bench was the only
integration test exercising the affected paths.** Each fix this campaign shipped with a fast,
targeted test (sealed-Named transfer test, dot-call capture test, reduce-coerce test, and now the
`searchDay` two-service-day boundary tests). A small **RAPTOR-shaped unit corpus** that hits
multi-round scans, multi-day search, materialize↔project round-trips, and captured closures catches
this whole family in milliseconds regardless of how the compiler is organized. This is the cheapest
leverage available and should land alongside (not after) the structural work.

## To-do list

Ordered by leverage-per-risk. Each item is independently shippable; master never regresses.

### Phase 0 — Verifiers first (the safety net)
- [x] **RC-balance verifier over `LinIR`** (Cluster 2) — `LIN_VERIFY_RC=1`, off by default,
      `34584403`. Per-value retain/release balance + use-after-release on every path; clean over
      stdlib/examples/RAPTOR. **Open follow-up:** quiet-on-success + nonzero-exit, then promote to a
      CI gate.
- [x] **RAPTOR-shaped unit corpus** (verification coverage) — TS query/results spec gaps ported
      (DepartAfterQuery/RangeQuery/MultipleCriteriaFilter), plus the multi-day `searchDay` boundary,
      calendar_dates loader fixture, and captured-closure-via-dot-call tests. `dfd750a5`.
- [x] Wire the RAPTOR unit suite into `ci.yml` (`dfd750a5`). **Open follow-up:** run it (and the
      stdlib suite) under ASan as a dedicated job — gate on `0 failed` + no ASan reports.

### Phase 1 — Cluster 3 (bounded, mechanical, do early)
- [x] Down payment: the dot-call capture rule is now one shared `record_capture_in_enclosing_fns`
      instead of a hand-mirrored copy (`e93e4da9`).
- [ ] Make ADR-085 receiver-push expressible on a desugared `f(receiver, …)` call.
- [ ] Desugar all dot-calls to prefix calls in one place; delete the parallel resolution body in
      `infer_dot_call`, keeping only method-specific routing (stream ops, packed-array intrinsics).
- [ ] Audit every `// mirror of the infer_call rule` comment — each should become dead code or a
      shared helper. Track the count down to zero.

### Phase 2 — Cluster 1 (the deep one, incremental on a branch)
- [ ] Define the `Repr` lattice explicitly (boxed / sealed-inline / sealed-ptr / packed-scalar /
      named-struct / …) as the single source of truth.
- [ ] Replace the ~8 ad-hoc materialize/borrow/own predicates with queries against `Repr` +
      one `coerce(from_repr, to_repr) -> ConversionPlan`.
- [ ] Make codegen `compile_ir_coerce` and lowering `lower_coerce_arg` consume the *same*
      `ConversionPlan` (no independently-derived decisions).
- [ ] Continue the representation-reset stages (records = value types, dissolve `LinObject`) on
      `reset/main`, gated by the Phase-0 RC verifier; merge to master only when net-positive.

### Phase 3 — Cluster 4 (opportunistic)
- [ ] If a third offside-inside-delimiters bug appears, extract a shared offside-column helper and
      route else-attach / block / match-arm boundary decisions through it.

## Explicit non-goals
- No big-bang rewrite. The existing patches are correct and master is green.
- No change that regresses master perf or correctness to land a cluster — perf-degrading
      intermediate states live on `reset/main` until net-positive (existing branch policy).
- Verifiers land *before* the refactors they protect, never after.
