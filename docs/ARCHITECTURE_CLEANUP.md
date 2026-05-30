# Compiler Architecture & Cleanup Plan

Status: **proposed** (2026-05-29). This document captures a structural review of the
Rust codebase and a phased plan to make the compiler coherent. It complements
`docs/DECISIONS.md` (which records *why* individual mechanisms work the way they do);
this file is about *where code lives* and *how the pipeline is shaped*.

## 1. Findings

A full read of all ~25.5k lines of Rust. The decomposition is mostly healthy — the
problem is concentrated, not pervasive.

### Healthy (leave alone)
- `lin-runtime` — 17 focused modules, clean topic split.
- `lin-check` submodules (`compat`, `env`, `resolve`, `widen`, `zonk`, `signature`,
  `exhaustiveness`, `typed_ir`, `types`) — single-responsibility each.
- `lin` CLI — 7 small command files under `cmd/`.
- `lin-common`, `lin-lex` — small and cohesive.
- Internal documentation throughout is excellent (explains *why*, cites ADRs).
- Debt markers are near-zero: 0 `TODO`/`FIXME`, 2 `#[allow(dead_code)]`.

### Problems (ranked)

1. **`lin-codegen/src/codegen.rs` is 7,685 lines in a single `impl Codegen` block**
   — 30% of the whole codebase in one file with ~100 methods spanning every codegen
   concern. The methods are individually well-named and well-documented; they are
   simply undifferentiated. This is the single thing that makes the codebase feel
   unmanageable.

2. **Two compilation pipelines coexist.** The live path is
   `TypedModule → compile_module` (TypedAST → LLVM). A second path,
   `TypedModule → lin-ir → compile_module_from_ir` (~2,500 lines across `lin-ir` and
   the `compile_ir_*` methods), is gated behind `LIN_USE_IR=1` and exercised by **zero
   tests, zero CI, zero examples**. It is also incomplete — `compile_ir_intrinsic`
   handles ~5 intrinsics and falls through to `null` for the rest, versus 600+ lines in
   the live `compile_intrinsic_call`.
   **Decision (2026-05-29): `lin-ir` is the intended future backend and will be
   promoted to the default; the legacy TypedAST path will be deleted once parity is
   reached.** This reframes the whole plan (see §3).

3. **857 `.unwrap()` in codegen, 836 of them inkwell `build_*().unwrap()` boilerplate.**
   Not a safety issue (codegen failure is a compiler bug; panic is fine) but enormous
   visual noise that buries the logic in every method.

4. **The `Codegen` struct conflates two lifetimes:** ~40 process-wide `rt_*`
   `FunctionValue` runtime declarations interleaved with per-module compilation state
   (slot maps, closure counter, import maps, coverage).

5. **Lower priority:** `checker.rs` (2,231) and `parser.rs` (1,487) are large but
   cohesive. Address only after codegen.

## 2. Target end-state

A **single** lowering pipeline, no env-var fork:

```
source → lex → parse → check (TypedModule)
       → lin-ir: lower → liveness → rc_elide (LinModule)
       → lin-codegen: LinModule → LLVM IR
       → opt → object → link
```

`lin-codegen` becomes a module tree under `codegen/` whose only input is `LinModule`.
All `TypedExpr`-shaped compilation (`compile_expr`, `compile_intrinsic_call`,
`compile_match`, the call family, the loop family) is either deleted (if the IR carries
the structure) or rewritten to consume IR nodes.

## 3. Phased plan

Because the legacy TypedAST path is slated for deletion, we must **not** spend effort
prettifying code that's going away. Sequence matters: reach IR parity *first*, delete
the legacy path, *then* split what remains.

### Phase 0 — IR parity (prerequisite, the real work)
Goal: `compile_module_from_ir` produces correct binaries for the entire existing test
suite and all `examples/*.lin`.
- [ ] Inventory every intrinsic/feature handled by `compile_intrinsic_call` and the
      AST-path expression/loop/match methods; enumerate gaps in the IR path.
- [ ] Extend `lin-ir` lowering (`lower.rs`) so every `TypedExpr` construct has a faithful
      `LinIR` representation (intrinsics, closures, partial application, async, pattern
      match, string interp, flat scalar arrays).
- [ ] Complete `compile_ir_intrinsic` to full parity (remove the `_ => null` fallback).
- [ ] Add a CI matrix axis (or a test runner flag) that runs the full suite with the IR
      path, so it can never silently rot again.
- [ ] Benchmark: confirm IR path output is at least at parity with the AST path.

### Phase 1 — Flip the default & delete the legacy path
- [ ] Make the IR path unconditional in `lin-compile`; remove the `LIN_USE_IR` branch.
- [ ] Delete `compile_module` and every method only reachable from the TypedAST path.
- [ ] Confirm `cargo build --workspace && cargo test --workspace` is green.
  Expected result: codegen.rs shrinks substantially before any cosmetic refactor.

### Phase 2 — Split `codegen.rs` into a module tree
One `impl Codegen` may span many files. No logic changes — pure code movement.
```
codegen/
  mod.rs        Codegen struct, new(), compile_module_from_ir, opt/emit
  runtime.rs    rt_* declarations (see Phase 4)
  types.rs      llvm_type, box/unbox, type_tag, coercion
  expr.rs       IR expression dispatch + literals/locals
  call.rs       call family + TCO
  control.rs    if / match / pattern-match / loops
  data.rs       arrays, objects, strings, interpolation
  intrinsics.rs intrinsic emission (+ async)
```
Verify tests after each file is extracted.

### Phase 3 — Builder façade (kill the 836 unwraps)
- [ ] Introduce a thin wrapper (e.g. `self.b.int_add(a, b, "name")`) over the inkwell
      `build_*().unwrap()` calls. Mechanical, dramatically reduces line count and noise.

### Phase 4 — Split runtime declarations off the `Codegen` struct
- [ ] Move the ~40 `rt_*` fields into a `RuntimeFns` struct constructed once; hold it as a
      single field on `Codegen`. Separates process-wide decls from per-module state.

### Phase 5 (optional, later) — checker.rs / parser.rs
- [ ] Only if still warranted after codegen is clean. Split `checker.rs` by concern
      (inference / narrowing / capture analysis / statement checking).

## 4. Working rules for this refactor
- Per `CLAUDE.md`: do the work in a git worktree off latest `master`, run
  `cargo build --workspace && cargo test --workspace`, and ask before merging.
- One phase per branch/PR; each phase must leave the suite green.
- No behavioural changes inside a "move" phase — refactor and feature work stay separate.
