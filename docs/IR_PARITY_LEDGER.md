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

## Phase 1 discovery — import resolution is a hidden foundational gap

The earlier exploration reported lowering "covers all 7 TypedStmt variants except
IndexSet." That is misleading: `lower_stmt`'s `Import`/`ForeignImport` arms
(`lower.rs:303-317`) only allocate **placeholder temps** — they do not connect an
import binding to the compiled target function. Consequently `lower_call`
(`lower.rs:594-618`) finds the import slot is neither an `intrinsic_slot` nor a
`global_fn_slot`, falls through, and emits an **indirect call on a dead placeholder
temp** — which the IR codegen drops to null. So **every call to an imported symbol
silently vanishes**, which is why `print` (and therefore essentially every real
program) produces no output. This is the dominant cause of the 121 failures, and it
sits *below* the intrinsic-catalogue work the plan scheduled for Phase 1.

`std/io`'s `print` is a normal exported `val` lowered to LLVM `std_io_print`
(`{module_key}_{name}` mangling, codegen `register_import`). The IR `Call`
handler's `CallTarget::Named(name)` arm already resolves via
`self.module.get_function(name)` (codegen.rs:7190) — so the fix is to make lowering
resolve import slots to `Named("<module_key>_<name>")` (and imported non-fn vals to
their `{module_key}_{name}__val` wrappers), rather than to placeholder temps.
`lower_module` will need the import map / module-key info that today only codegen has.

**Revised Phase 1 scope:** (a) thread import resolution into `lower_module` and emit
real `Named` targets for import/foreign-import slots; (b) THEN the intrinsic-name
catalogue; (c) CI scaffolding. Item (a) was not in the approved plan's Phase 1 and is
a prerequisite for any program to run on the IR path.

## Progress log

| Phase | IR-leg failures | Notes |
|-------|-----------------|-------|
| 0 (baseline) | 121 / 128 | starting point |
| 1a (import resolution) | 83 / 128 | resolve import/foreign slots to `Named` targets + box concrete args to Json params; 45 passing. AST leg still 128/128. |
