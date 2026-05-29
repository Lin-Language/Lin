# IR Parity Burn-Down Ledger

Tracks the failure set of the integration suite run on the **IR path**
(`LIN_USE_IR=1 cargo test -p lin --test integration`) as we bring it to parity
with the TypedAST path. Per the plan, this set must shrink monotonically to zero
by the Phase 8 parity gate.

Working branch: `ir-sole-path` (worktree at `/tmp/lin-ir-sole-path`, off `master` @ `fa2943e`).

## Baseline — Phase 0 (start)

- **AST leg:** 128 integration + 33 type_check + 6 snapshots + 6 lin_ir unit = **green, 0 failures**.
- **IR leg (integration):** **7 passed / 121 failed** of 128.

The 7 "passes" are all compile-time-error or formatter tests that never reach IR
codegen (`test_cannot_assign_immutable_error`, `test_division_by_zero_error`,
`test_fmt_idempotent`, `test_if_old_syntax_error`, `test_modulo_by_zero_error`,
`test_object_spread_null_error`, `test_undefined_variable_error`). **No program
that actually executes IR-generated code currently works.**

### Root-cause confirmed at baseline
`test_hello_world` builds and exits 0 but prints nothing. The emitted `.ll` shows
`main()` creating the `"hello world"` string and immediately releasing it — **the
`print` call was dropped entirely during lowering**, even though `print`/`lin_print`
is one of the 5 "mapped" intrinsics. So the gap is broader than missing intrinsic
variants: imported-function call emission and intrinsic dispatch through imported
wrappers are not wired in the IR path. Phase 1 must address call lowering, not just
the intrinsic name table.

## Progress log

| Phase | IR-leg failures | Notes |
|-------|-----------------|-------|
| 0 (baseline) | 121 / 128 | starting point |
