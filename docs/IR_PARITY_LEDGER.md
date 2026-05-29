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

## Remaining failures after Phase 1 foundation (74)

Bucketed against the plan's later phases — confirms the remaining work maps onto
the scheduled phases rather than more hidden foundational gaps:

- **Loops / iteration (Phase 4):** for_and_range, map_filter_reduce, iter_builtin,
  iterator_restart, chaining, multiline_chain, stdlib_array_* — control-flow
  intrinsics still lower to no-ops (`_ => null`).
- **Pattern matching / match (Phase 4):** pattern_matching_is/has,
  array_pattern_matching_*, tagged_unions, string_literal_pattern,
  non_exhaustive_match_error, tco_in_match, match_with_block_body.
- **Partial application (Phase 6):** functions_and_partial_application,
  dot_partial_application, partial_application_chain, higher_order_functions.
- **TCO (Phase 7):** tail_call_optimization, recursion, recursive_fibonacci,
  mutual_recursion_via_forward_decl.
- **Async (Phase 5):** async_await_basic, async_val_capture, parallel_three_thunks,
  thread_pool_async, worker_request_reply.
- **Closures/var:** closures_and_var, multiple_closures_share_var,
  forward_reference_in_closure.
- **fs/http/server (need investigation — may share a common cause):** fs_*, http_*,
  server_*, io_lines/io_read.
- **Misc to investigate:** equality, not_equal, numeric_comparison, string_comparison,
  if_expressions, if_block_branches, chained_if_else, continuation_*,
  index_assign (IndexSet — Phase 2), keys_values_entries, push_and_concat,
  nested_objects_access, object_equality_deep, object/array_rest_destructuring,
  is_has_as_boolean_expressions, speculative_reads_typed_union, multi_param_lambda,
  multi_statement_*_lambda/paren, interp_with_expressions, array_oob_error,
  mixed_numeric_operations.

## Progress log

| Phase | IR-leg failures | Notes |
|-------|-----------------|-------|
| 0 (baseline) | 121 / 128 | starting point |
| 1a (import resolution) | 83 / 128 | resolve import/foreign slots to `Named` targets + box concrete args to Json params; 45 passing. AST leg still 128/128. |
| 1b (top-level fn FuncId) | 77 / 128 | top-level fn vals reused a fresh FuncId instead of the pre-assigned `global_fn_slots` id (Direct calls panicked); also main must be FuncId(0). 51 passing. AST leg 128/128. |
| 1c (ToString by input type) | 74 / 128 | `compile_to_string_value` dispatched on LLVM value kind, treating `Str` as a tagged ptr (broke string interp). Thread arg types into `compile_ir_intrinsic`; delegate to type-driven `value_to_string_simple`. 54 passing. AST leg 128/128. |
| 1d (boxed-object index) | 61 / 128 | `Index` carried no object/key types, so indexing a Json-boxed object (e.g. `stat(p)["size"]`) called `lin_object_get` on the `TaggedVal*` directly → null. Added `obj_ty`/`key_ty` to the `Index` instruction; unbox boxed containers/keys before runtime accessors. Unblocked the fs/http/server cluster. 67 passing. AST leg 128/128. |
| 2 (IndexSet + flat read) | 59 / 128 | added `IndexSet` IR instruction + `compile_ir_index_set` (object/array/dynamic dispatch); fixed `compile_ir_index` to use `flat_array_get`/`lin_array_get_tagged` per result type (was always tagged → garbage for flat scalar arrays). 69 passing. AST leg 128/128. |
| 2b (if-branch RC scope) | 59 / 128 | **heap-corruption fix:** `lower_if` registered branch-local heap temps in the enclosing scope, so the merge block released both branches' temps (only one branch ran) → SIGABRT. Each branch now gets its own ownership scope. No count change but fixes latent UB; validates the ASan plan. AST leg 128/128. |
| 2c (Binary operand type) | 53 / 128 | `Binary` carried only the result type, so `==`/comparisons got `lty=Bool` and did scalar compares instead of object/array deep equality (`{..}=={..}` → false). Added `operand_ty` to `Binary`; pass it to `compile_eq`/`compile_cmp`. 75 passing. AST leg 128/128. |
| 4a (Phi / SSA merge) | 45 / 128 | the IR codegen had **no SSA merge** — `lower_if` copied both branches into a shared temp, and the single-pass codegen let the last-compiled branch win for both runtime paths (if-expressions crashed/wrong). Added a `Phi` IR instruction (records actual predecessor block per branch) + LLVM phi codegen with deferred backpatch (so loop back-edge values resolve). 83 passing. AST leg 128/128. |
| 4b (named-fn closure wrap) | 41 / 128 | passing a named (capture-less) function as a `Function` param stored the raw fn ptr in a closure struct, but call sites invoke `fn_ptr(env, args...)` → env shifted all args (`apply(add,7)`→7 not 14). Wrap capture-less `MakeClosure` targets via `wrap_named_fn_as_closure`. 87 passing. AST leg 128/128. |
| 4c (loops as IR blocks) | 36 / 128 | lowered for/while/map/filter/reduce/range to explicit LinIR CFG (header phi + back-edge); added ArrayAlloc/FlatArray intrinsic codegen; callback ABI coercion (box args to Json params, coerce fn returns); **heap-box escaping values** (compile_ir_box/coerce used a stack alloca → dangling boxed returns, the `[object]` bug); union-operand arithmetic re-boxes when result is Json. 92 passing. AST leg 128/128. |
| 4d (Function-arg RC) | 34 / 128 | AST-compiled callees release their Function-typed params at return, so a closure passed from IR-main was freed by the callee then released again at module exit → double-free segfault. Retain Function-typed args before Named/Direct calls. 94 passing. AST leg 128/128. |
| 4e (match Phi + arg/field boxing) | 33 / 128 | `lower_match` used the same broken shared-Copy merge as if (→ `[object]`); now emits a Phi and boxes the scrutinee for `is`/`has` tag tests. Box concrete args to Json params for global (Direct) calls too. Add `obj_ty` to `FieldGet` and unbox boxed-object containers (fixes destructure of a boxed param). 95 passing. AST leg 128/128. |
| 4f (literal patterns + panic term) | 31 / 128 | `is "lit"` patterns were lowered as type-only checks (matched any same-typed value); now emit a value equality (`lin_tagged_eq`, fixed to i8 return + literal boxed to Json). Removed the double-terminator from `Panic` codegen (it emitted `unreachable` AND the IR terminator did → verifier error). 97 passing. AST leg 128/128. |
| 7 (TCO TailCall) | 30 / 128 | implemented the `TailCall` terminator (was a `build_unreachable` stub → deep self-recursion stack-overflowed). Functions with a tail call get a param-alloca prologue + loop-header that reloads params each iteration; `TailCall` stores new args and branches back. Marked post-tail-call blocks "diverged" so they don't become phi predecessors. `count(1_000_000,0)` now returns without overflow. 98 passing. AST leg 128/128. |
| 6 (partial application) | 28 / 128 | under-applied Direct/Named calls (args < arity, Function result) now build a partial-application closure via a value-input port of `build_partial_application`. `add(5)(10)`→15. 100 passing. AST leg 128/128. |
| 6b (narrowed LocalGet unbox) | 27 / 128 | a Json slot narrowed to a concrete type in a match arm (e.g. `is String => x`) read the boxed value without unboxing → null/garbage. `LocalGet` now emits a `Coerce` when the stored slot type is union but the use wants concrete. 101 passing. AST leg 128/128. |
| 6c (closure env + uniform return) | 27 / 128 | closures capturing values crashed: env reads used `lin_object_get` on the raw env struct, and a closure returning a concrete scalar was called via the opaque-Function ABI expecting a ptr return. Added an `EnvCapture` IR instruction (raw struct load at offset 8+i*8) and made closures use a uniform boxed (Json) return ABI. Capturing-param closures work (`adder(10)(5)`→15); mutable var-capture-by-ref still pending. 101 passing. AST leg 128/128. |

## Status checkpoint (Phases 1–7 substantially complete)

**IR-leg integration: 103 / 128 passing** (from a 7/128 baseline). AST leg: **128/128
throughout**. ~21 commits, each verified.

Architecture delivered (Option B):
- Import/foreign call resolution; top-level FuncId fix; ToString-by-type.
- Index/IndexSet/FieldGet carry object/key types; box/unbox at boundaries; flat vs
  tagged array reads.
- **Phi-based SSA merge** (if-exprs, match) with deferred back-edge backpatch.
- **Loops as explicit IR blocks** (for/while/map/filter/reduce/range) + ArrayAlloc/
  FlatArray intrinsics; callback ABI (box args to Json params, uniform boxed closure
  return, re-box union arithmetic).
- **Closures**: EnvCapture instruction (raw env-struct load); capture-less wrapping;
  uniform boxed return ABI.
- **TCO**: structural TailCall loop transform (param allocas + header reload).
- **Partial application** (value-input port).
- Heap-boxing of escaping values; Function-arg retain to balance callee consume;
  narrowed-slot unbox; Val/Var + array-element coercion to declared representation;
  mixed int/float widening; literal-pattern value equality.

### Remaining failures (25), by area
- **Closures / curry / mutable var-capture (≈5):** closures_and_var,
  multiple_closures_share_var, higher_order_functions, partial_application_chain,
  forward_reference_in_closure. Curried-closure return ABI when the inner fn type is
  inferred; mutable `var` captured by reference (heap cell) not yet modelled in IR.
- **Array pattern / rest destructuring (≈4):** array_pattern_matching_is/has,
  array_rest_destructuring, object_rest_destructuring. Rest slicing + array-pattern
  binding need IR support.
- **Async / concurrency (≈5):** async_await_basic, async_val_capture,
  parallel_three_thunks, thread_pool_async, worker_request_reply. IR codegen for
  async/await/exit exists; the stdlib wrappers are still AST-compiled, so full parity
  needs the IR path to compile imports too (or the wrapper RC interaction resolved).
- **Iterators (2):** iter_builtin, iterator_restart — `lin_iter` not yet lowered.
- **Heterogeneous-literal boxing (≈3):** tagged_unions, tostring_objects_and_arrays,
  speculative_reads_typed_union — boolean (and null) boxing inside mixed Json arrays.
- **stdlib array fns (2):** stdlib_array_find_some_every, flatmap_indexof_reverse.
- **FFI / fs (2):** ffi_end_to_end_c_library, fs_read_lines.
- **TCO (1):** tail_call_optimization — a specific shape still failing (basic + deep TCO work).
- **pattern_matching_has (1).**

## Checkpoint 2 — 113/128 (Phases 1–7 + most edge cases)

Since the last checkpoint (103): uniform closure return ABI completed (curried/HOF
calls), array rest + array pattern matching, object rest, `iter()` as IR blocks,
mixed int/float widening, has-pattern value constraints, if-branch result coercion.
AST leg green throughout.

### Remaining (15) — root causes identified
1. **Mutable `var` captured by closure (heap cell)** — `closures_and_var`,
   `multiple_closures_share_var`, and (via stdlib `some`/`every`, which mutate a
   captured `var` inside a `lin_while` body) `stdlib_array_find_some_every`. The IR
   models `var` as a plain SSA temp; captured-and-mutated `var` needs a heap cell shared
   by reference (ADR-015). Largest remaining piece; comparable in size to the loop work.
2. **RC of an object returned from a function then matched** — `tagged_unions`,
   `tostring_objects_and_arrays`, `speculative_reads_typed_union`. A `match`/`has` on a
   freshly-returned object reads a dangling entry key — an RC-ordering bug in how the
   object's interior is owned across the function-return + match boundary.
3. **Async/concurrency** — `async_val_capture`, `parallel_three_thunks`,
   `thread_pool_async`, `worker_request_reply`. IR async/await codegen exists, but the
   stdlib async wrappers are AST-compiled; full parity needs the IR path to compile
   imports too (Phase 9 territory) or the wrapper RC interaction resolved.
4. **FFI / fs** — `ffi_end_to_end_c_library`, `fs_read_lines`.
5. **Misc** — `partial_application_chain`, `pattern_matching_has`,
   `stdlib_array_flatmap_indexof_reverse`, `tail_call_optimization` (a specific shape).

## Checkpoint 3 — 114/128 (stable stopping point)

Net since checkpoint 2: object-return RC fix (function boxing its result kept the raw
object too), array/object rest, iter, has-value-constraints, mixed numeric widening.

**14 remaining**, dominated by two hard themes — both surfaced as real heap corruption
(`stdlib/` run reports `malloc(): unaligned tcache chunk`, validating the planned ASan leg):

1. **Mutable `var` captured by a closure** (heap-cell semantics, ADR-015) — not yet
   modelled in the IR (var is a plain SSA temp). Blocks closures_and_var,
   multiple_closures_share_var, and stdlib some/every (→ stdlib_array_find_some_every).
   This is the single largest remaining feature (comparable to the loop/TCO work):
   needs cell alloc + load/store-through-cell + capture-cell-pointer across lowering,
   the IR model, and codegen.
2. **RC ordering for boxed objects flowing through match value-constraints** —
   tagged_unions, speculative_reads_typed_union (and contributes to the var/async
   crashes). The has-pattern value-constraint path (Index read + boxed literal compare)
   leaks/aliases; flaky crash vs empty output indicates a missing release or aliasing read.

Plus: async/concurrency (async_val_capture, worker_request_reply — stdlib async wrappers
are AST-compiled), FFI/fs (ffi_end_to_end_c_library, fs_read_lines), and one-offs
(partial_application_chain, pattern_matching_has, tail_call_optimization,
tostring_objects_and_arrays).

### Recommended next steps (in order)
- Add the **ASan/LSan CI leg + RC-stress fixtures** (deferred Phase 1 infra) BEFORE
  further RC work — the long-tail is now clearly memory-safety-bound.
- Implement mutable-var heap cells (biggest unlock).
- Resolve the has-value-constraint RC aliasing.
- Then the parity gate (Phase 8) and milestone-1 merge ask.

## Checkpoint 4 — 124/128 (strong stopping point)

Since checkpoint 3 (120): tagged_unions (multi-match + has value-constraints, via per-arm
ownership scopes + scoped value-constraint temps + branchless has/arraylen helpers),
stdlib some/every (box `==` rhs by value kind), fs_read_lines (any-width int array index),
closures-stored-in-arrays + multiple_closures_share_var (retain Function array elements +
unbox boxed callee), mutable-var-capture heap cells, array/object rest, iter, partial
application, TCO (incl. Int64), guards. **AST leg green (128/128) throughout; ~30 commits.**

### Final remaining (4) — each needs a distinct, non-trivial feature
1. **async_val_capture** — root cause found: a closure that references a top-level
   (module-level) `val` reads a placeholder instead of the value. The IR path lowers
   module vals into `main`'s slots; closures can't see them. The AST path emits them as
   LLVM globals (`global_val_slots`) and loads from there. Fix: lower top-level non-fn
   vals as globals (or capture-by-value into closures). Also blocks any closure over a
   module val.
2. **partial_application_chain** — `add3(1)(2)(3)`: a partial-application wrapper called
   with fewer args than its remaining params must itself partially apply. Indirect closure
   calls carry no arity, so under-application isn't detected. Needs arity metadata on
   closures or a curried-wrapper representation.
3. **ffi_end_to_end_c_library** — FFI: `import foreign` C symbol linkage on the IR path.
4. **worker_request_reply** — worker-thread concurrency primitives (thread pool / channels).

### Recommended next steps
- Add the **ASan/LSan CI leg + RC-stress fixtures** (deferred Phase 1 infra). The many
  RC fixes in checkpoints 2–4 (heap-box escaping values, per-branch/arm scopes,
  function-arg/array-element retains, null handling) are exactly what a sanitizer leg
  guards; it should land before the parity gate.
- Module-global vals in closures (unblocks async_val_capture).
- Then the Phase 8 parity gate and the Milestone-1 merge **ask** (still off by default).
