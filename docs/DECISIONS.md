# Architecture Decision Records

> **Numbering note (2026 consolidation):** ADR-003 was never used, and several
> earlier ADRs were absorbed into the survivor that owns their topic (each survivor
> carries a "Supersedes/absorbs" note). As a result the ADR numbers are intentionally
> non-contiguous — gaps are expected and do not indicate a missing record.

## ADR-001: Static typing via lin-check

**Decision**: All Lin programs are type-checked before codegen. The `lin-check` crate performs bidirectional inference, structural typing, union narrowing, and exhaustiveness checking. Runtime type tags still exist in the runtime for `is`/`has` pattern dispatch, but no program should reach codegen with unresolved type errors.

**Rationale**: A full bidirectional type system with generics, variance, and numeric widening allows LLVM to emit unboxed primitives and enables helpful compile-time error messages.

**Consequence**: Type annotations are parsed, checked, and emitted into `TypedModule`. The `lin-check` crate owns all type inference logic.

## ADR-002: Minimal built-ins, stdlib for iteration

**Decision**: Only `lin_for` and `lin_iter` are compiler intrinsics with special codegen. Higher-level functions (`map`, `filter`, `reduce`, `range`, `iterOf`) are implemented in `.lin` stdlib files (`std/array`) and must be explicitly imported by user code.

**Rationale**: `lin_for` and `lin_iter` require special compiler treatment (inline loop emission, iterator struct construction). All other iteration functions can be expressed in Lin itself. Keeping intrinsics minimal means the runtime stays small and the stdlib is readable Lin code.

**Consequence**: `range()` returns a lazy `Iterator`. User code imports `map`, `filter`, `reduce`, etc. from `std/array`. The compiler recognises `lin_for`/`lin_iter` by name and emits them as native loops rather than function calls.

## ADR-004: Objects suppress indentation tracking

**Decision**: When inside `{ }` (brace depth > 0), the lexer suppresses newline tokens and indentation tracking (no INDENT/DEDENT emitted). The same suppression applies inside `( )` and `[ ]`.

**Rationale**: Multi-line JSON object literals must not trigger block parsing.

**Consequence**: You cannot have indentation-significant syntax inside object literals (which is fine — object values are expressions, not statements).

**Supersedes/absorbs ADR-017.** The `at_line_start` flag is reset to false unconditionally at the top of `next_token()`, not only when entering `handle_indentation()`. Otherwise a newline inside braces (e.g. a multi-line `import { ... } from "path"`) left the flag stale, and when the closing brace returned depth to 0 it triggered a spurious INDENT on the next token. Always clearing the flag eliminates that whole class of bug with no behavioural change elsewhere.

## ADR-005: String interpolation as compound token

**Decision**: The lexer produces a single `InterpString(Vec<InterpPart>)` token for interpolated strings. Each `InterpPart::Expr` contains its own sub-token-stream that the parser processes independently.

**Rationale**: The initial approach of inlining interpolation tokens into the main token stream caused ordering issues with the pending-token queue. A compound token with embedded sub-streams is self-contained and avoids interaction with indentation tracking.

**Consequence**: Interpolation expressions are parsed in isolation (no access to outer indentation context), which is fine since they're always single expressions.

## ADR-006: Dot-chaining across newlines via lookahead

**Decision**: The parser's postfix expression loop checks for `.` across newline boundaries using a save/restore pattern. If a newline is followed by `.`, parsing continues the dot chain. Otherwise, position is restored.

**Rationale**: The spec requires `x\n  .f()` to chain. But aggressively skipping all indentation tokens breaks block structure. The save/restore pattern is conservative — it only consumes whitespace tokens when followed by a dot.

**Consequence**: Dot-chaining works across lines without breaking function bodies or if-then-else blocks.

**Supersedes/absorbs ADR-013.** The same save/restore lookahead is used for `&&`/`||` continuation lines: `parse_and_expr`/`parse_or_expr` use a `skip_continuation_newline` helper that looks past a Newline token for the operator (the lexer suppresses INDENT/DEDENT for lines starting with `&&`/`||` per spec §2.2 but still emits a trailing Newline). Without it, `x >= 5\n  && active` would parse as just `x >= 5`; with it, multi-line boolean expressions and `if` conditions with continuation lines work as specified.

## ADR-007: Bare identifier lambdas

**Decision**: The parser recognizes `name => body` (without parentheses) as a single-parameter lambda when used as a function argument.

**Rationale**: The spec's examples use this form extensively (`x => x * 2`, `n => print(n)`). Without this, every callback would need `(x) => x * 2`.

**Consequence**: `is_bare_lambda()` check applies only in argument position. A standalone `name => ...` at statement level would be ambiguous (could be assignment with `=>`?), but this doesn't arise in practice.

## ADR-008: Module-level environment isolation in the compiler

**Decision**: Each module is type-checked in its own scope. Imports from other modules are resolved before the importing module is checked, with each module's public exports available as a `ModuleSignature`.

**Rationale**: Modules must not pollute each other's namespaces. The compiler uses a module cache keyed by source hash so unchanged modules are not re-checked.

**Consequence**: Circular imports within a single init chain are detected at compile time. Each module's checked `TypedModule` is cached and reused by all importers.

## ADR-009: Stdlib functions as thin Lin wrappers over runtime intrinsics

**Decision**: String, array, object, and IO operations are implemented as C-ABI functions in `lin-runtime` (e.g. `lin_string_trim`, `lin_fs_read_file`) and declared in stdlib `.lin` files via `import foreign "lin-runtime"`. The `.lin` files provide the public API surface.

**Rationale**: String/IO manipulation requires Rust code. The .lin wrapper layer keeps the user-facing API in Lin, making stdlib readable and testable in the same language. The compiler recognises `"lin-runtime"` as a reserved path and always links the runtime archive.

**Consequence**: The stdlib is a mix of pure-Lin logic and thin `lin-runtime` wrappers. Adding a new runtime function requires both a `#[no_mangle] pub unsafe extern "C" fn lin_xxx` in `lin-runtime/src/` and an exported wrapper in the appropriate `stdlib/*.lin` file.

**Supersedes/absorbs ADR-030 and ADR-031.** This pattern extends to all IO: filesystem, HTTP client, and server operations are likewise implemented as `#[no_mangle] pub unsafe extern "C"` functions in `lin-runtime` (`lin_io_read_line`, `lin_fs_read_file`, `lin_http_fetch`, …) and exposed through thin `.lin` modules (`std/io`, `std/fs`, `std/http`, `std/server`) registered via `include_str!`. All IO is synchronous on the calling thread (run in the background via `async`/`threadPool`); the `lin_*` symbols stay implementation details behind the clean wrapper API, and higher-level helpers like `fetchJson`/`postJson`/`parseBody` are written in Lin.

## ADR-010: Multi-line if/then/else syntax

**Decision**: `then` always appears on the condition line (or the last continuation line of the condition). The body follows on an indented block (INDENT … DEDENT). `else` appears at the same indent level as `if`. The parser does not consume any INDENT before `then` — it simply expects `then` after the condition expression.

**Rationale**: Placing `then` at the end of the condition line is clearer and more consistent with how block-opening keywords work in other languages. The old approach of allowing `then` on its own indented line required the parser to tentatively consume an INDENT token before `then`, then emit a corresponding DEDENT, making the grammar more complex with three special-case DEDENT guards. The new rule is simpler: condition, `then`, body block, `else` at original indent, else body.

**Consequence**: All spec-defined if layouts (single-line, multi-line with block body, multi-line with inline body) parse correctly. Condition continuation lines with `&&`/`||` end with `then` on the last continuation line. The `then_indented` tracking variable and its three associated DEDENT guards have been removed from `parse_if_expr`.

## ADR-011: Postfix suppression after DEDENT, and line-leading `[`/`(` as a new statement

**Decision**: The parser's postfix expression loop (`[` and `(`) is suppressed when the immediately preceding consumed token was a DEDENT. Dot-chaining (`.`) is still allowed (it handles cross-line chaining via a separate lookahead mechanism).

**Rationale**: After a block-bodied function expression like `() => \n  42`, the lexer produces `... IntLit(42) Newline Dedent LBracket ...` — the inner block's `skip_newlines` consumes the Newline, so after the Dedent is consumed, no Newline separates the function from the next line's `[`. Without this guard, `[x]` at the outer block level is incorrectly parsed as index access on the function expression.

**Consequence**: Array/object literals at block level after indented function definitions parse correctly as separate expressions. Same-line index access (`f()[0]`) still works because no DEDENT intervenes.

**Supersedes/absorbs the duplicate ADR-043 ("line-leading `[`/`(` is a new statement").** The same intent applies where the boundary is a *suppressed newline* rather than a DEDENT: inside an inline lambda body (`() => ...` with no INDENT, parsed by `parse_inline_block`), `()`/`[]`/`{}` suppress newline tokens (ADR-004), so the postfix loop would otherwise read `expr \n [ ... ]` as `expr[...]`. The lexer records a per-token `newline_before` flag (set by scanning the gap between token spans), and `parse_postfix_expr` suppresses the `LBracket`/`LParen` arms when `at_line_start()` is true. This lets a multi-statement inline body return an array literal on its own line (`val xs = f(); push(xs, y); [ expect(...).toBe(...) ]`) instead of gluing it as an index. The postfix `.` arm is not gated, so same-line indexing and continuation dot-chains are unaffected.

## ADR-014: Inline block parsing for lambda bodies inside parentheses

**Decision**: `parse_function_body` always delegates to `parse_inline_block` when there is no `Indent` token ahead. `parse_inline_block` collects statements until it sees `Newline`, `)`, `]`, `}`, `,`, `Dedent`, or EOF, then returns either the single expression or an `Expr::Block` wrapping all collected statements.

**Rationale**: Inside parentheses, brackets, or braces, the lexer suppresses all INDENT/DEDENT and Newline tokens (ADR-004), so `parse_expr_or_block` cannot detect a multi-statement body. At top level, Newline tokens are present and `parse_inline_block` breaks on them, making it behave identically to `parse_expr` for the single-expression case. The break conditions `]` and `}` prevent over-consuming array and object literal contents. `Comma` ensures argument-list lambdas (e.g. `iter(() => 0, i => i + 1)`) parse correctly.

The earlier version used `val`/`var` as the trigger for multi-statement inline bodies. That was too narrow — bare expression side-effects (calls to `print`, `writeFile`, etc.) were silently dropped, leaving only the first expression evaluated.

**Consequence**: Bare side-effect sequences work in both inline and indented lambda bodies:

```txt
[1, 2, 3].for(x =>
  print("before")    // executed
  print(toString(x)) // executed
)

val myFunc = () =>
  print("first")     // executed
  print("second")    // executed
  42                 // return value
```

## ADR-015: Forward references between top-level functions via mutable cells

**Decision**: Before evaluating a module's statements, a pre-scan registers all `val name = (...) => ...` bindings (function expressions with named pattern) as mutable cells holding `Null`. During evaluation, each function's closure captures the environment containing these cells. When the actual definition is reached, the cell is updated with the real function value.

**Rationale**: The spec (§6.3) expects mutual recursion between top-level functions. Without forward declaration, functions must be defined before use, which prevents mutual recursion and requires careful ordering. The mutable-cell approach solves this without changing evaluation semantics — a function that calls another function reads the cell at call time, by which point the definition has been evaluated.

**Consequence**: Forward references work between functions (e.g., `isEven` calling `isOdd` and vice versa). However, eager top-level evaluation that *immediately* calls a forward-referenced function (before its definition is evaluated) will still fail with "Cannot call value of type Null". This is inherent to sequential evaluation and matches the behavior of languages like JavaScript (`let` before initialization).

**Supersedes/absorbs ADR-022 and ADR-024.** The same forward-reference mechanism is realised in codegen, where it also establishes *function identity*. (1) Before compiling any top-level function body, `compile_module` pre-scans all `Val` statements and LLVM-declares any function whose value is a named `Function`, storing them in `global_fn_slots`; bodies are compiled in a second pass so sibling functions can call each other. (2) The binding name is propagated onto `TypedExpr::Function { name }` by the checker when a `val` pattern is a plain identifier, so the codegen emits a named LLVM function (`@factorial`, not `@__closure_0`), enables `global_fn_slots` lookup for direct calls, and sets `current_function` for tail-call detection (which feeds the TCO loop transform, ADR-021).

## ADR-016: User module loading from filesystem

**Decision**: When an import path does not match a `std/` prefix, the interpreter resolves it relative to the importing file's directory by appending `.lin` to the path.

**Rationale**: Multi-file programs need to import user-defined modules. The resolution strategy mirrors Node.js-style relative imports without requiring a leading `./` — the `std/` prefix is the only special case, everything else is relative.

**Consequence**: `import { x } from "lib/math"` in `examples/main.lin` loads `examples/lib/math.lin`. Absolute paths and `..` traversal work naturally via the filesystem.

## ADR-018: `Number` as a built-in union alias

**Decision**: Add `Number` to the built-in types as a union alias for every numeric family (`Int8 | … | Float64`), and use it in the definition of `Json`. `Number` does not introduce a new runtime kind, a new subtype relation, or any new narrowing rule — it is exactly the union it expands to.

**Rationale**: Without a name for "any numeric," the `Json` type has to enumerate all sixteen numeric families to be accurate, and signatures that accept any numeric have no concise spelling. A true supertype with subtype assignability would introduce a third kind of type relation alongside structural typing and unions, and would force decisions about `is Number` narrowing, arithmetic on a `Number`-typed operand, and how widening (§21) interacts with the supertype. A union alias avoids all of that: `is Int32`, widening, and operator dispatch keep working exactly as they did, because under the hood there is still only a concrete numeric family at every site.

**Consequence**: Spec-only change in v0 (no type checker exists yet — `Number` already parses as a `TypeExpr::Named`). The future type checker treats `Number` as a union alias when resolving assignability and exhaustiveness. Runtime is unchanged: §28.4 still says every numeric value carries its specific family tag and there is no single `Number` representation.

## ADR-019: LLVM 22 via inkwell with dynamic linking

**Decision**: The compiler backend uses LLVM 22 (the latest stable release) via the `inkwell` 0.9.0 Rust wrapper, with the `llvm22-1-prefer-dynamic` feature flag for dynamic linking.

**Rationale**: LLVM 22 is the latest release with the best optimizations and codegen quality. The `prefer-dynamic` flag is required because Debian/Ubuntu package `LLVMPolly.so` as a dynamic library only — no `.a` static archive is provided. Without dynamic linking, the linker fails with "could not find native static library 'Polly'". The `inkwell` wrapper provides a safe, idiomatic Rust API over the LLVM C API and supports LLVM 22.

**Consequence**: The devcontainer installs LLVM 22 from `apt.llvm.org/bookworm` and sets `LLVM_SYS_221_PREFIX=/usr/lib/llvm-22`. The compiled binary dynamically links against `libLLVM-22.so` at runtime, which is available on the devcontainer but would need to be present on deployment targets.

## ADR-020: Unboxed primitive value representation in LLVM IR

**Decision**: Numeric and boolean types are represented as bare LLVM primitives: `Int32` → `i32`, `Float64` → `double`, `Bool` → `i1`. Strings are represented as `ptr` to a heap-allocated `LinString` struct (refcount + len + bytes). Closures are represented as `ptr` to a `{ fn_ptr, env_ptr }` struct. Union types use a heap-allocated tagged representation.

**Rationale**: The type checker produces `TypedIR` with a concrete `Type` for every expression. This means we know at compile time whether a value is `i32` or `f64`, enabling LLVM to treat them as first-class register-width values rather than tagged `Value` boxes. The performance difference versus a tree-walking interpreter (which boxes everything in a `Value` enum) is typically 50–200×. Strings cannot be unboxed (variable-length), so they remain as pointers.

**Consequence**: No boxing for arithmetic, comparisons, boolean operations, or function calls on primitive types. LLVM's optimizer can treat these as register values and apply standard scalar optimizations. Union types and unknown-typed values (TypeVar) fall back to pointer representation.

## ADR-021: TCO via alloca/loop transform (not trampoline)

**Decision**: Tail-recursive functions are compiled using the "loop transform": parameters are stored in `alloca` slots, the function body is wrapped in a `tco_loop` basic block, and tail self-calls store updated argument values into the alloca slots and branch back to `tco_loop` rather than making a recursive call.

**Rationale**: The alloca/loop approach produces standard LLVM IR that LLVM's optimizer understands — it can apply `mem2reg` to promote the alloca slots to phi nodes, yielding optimal machine code. A trampoline approach (returning a thunk and looping externally) requires a heap allocation per tail call and more complex call-site machinery. The loop transform produces a native loop with no allocation overhead.

**Consequence**: Tail self-calls are identified by `is_tail: bool` in `TypedExpr::Call`, set by the checker when the call is in tail position and the callee is the current function. Non-tail recursive calls and mutual recursion still use normal stack frames. `mem2reg` (run as part of `default<O2>`) eliminates all alloca slots from the final machine code. The spec (§28.3) requires direct self-recursive tail calls to run in constant stack space; mutual tail recursion is not optimised.

**Supersedes/absorbs ADR-012.** ADR-012 described an earlier `TailResult`/`eval_tail_expr` trampoline in a tree-walking interpreter. That interpreter no longer exists in the codebase — TCO is now realised entirely in codegen by the alloca/loop transform above, which supersedes the obsolete ADR-012.

## ADR-023: Runtime library as a static archive linked into every binary

**Decision**: `lin-runtime` is compiled as a Rust `staticlib` (`crate-type = ["staticlib", "rlib"]`) that provides C-ABI functions (`lin_print`, `lin_string_concat`, `lin_int_to_string`, `lin_array_alloc`, `lin_panic`, etc.). The compile pipeline locates the `.a` file and passes it to the system linker (`cc`) alongside the LLVM-emitted `.o` file.

**Rationale**: LLVM IR cannot express Rust-level operations like `write!` or `alloc::alloc`. The runtime provides these as well-known C symbols that LLVM IR can `declare` and call. A static archive avoids a runtime shared-library dependency on deployed binaries. Using the Rust `staticlib` crate type ensures `rustc` links in all needed Rust stdlib code (allocator, panic handler, etc.).

**Consequence**: Compiled Lin binaries are self-contained: they link against `libc` (via `cc`) plus the runtime `.a`, with no Lin-specific shared libraries required. The runtime is small (~10KB stripped) since it only contains the functions LLVM IR references.

## ADR-025: Closure capture analysis via scope depth tracking

**Decision**: Capture analysis is performed inline during type-checking. When `infer_function` is entered, the current scope depth is pushed onto `function_scope_depths`. During `LocalGet` inference, if the variable's scope depth is less than the innermost function's entry depth, it is recorded as a capture in `capture_stack`. The captures are sorted by `outer_slot` for deterministic codegen.

**Rationale**: A separate capture-analysis pass would need to traverse the typed IR a second time. Doing it inline avoids this while the scope information is naturally available. Scope depth (not slot number) is the right discriminant: variables from the current function's scope are parameters/locals; variables from outer scopes are captures. Stable sorting by slot ensures codegen produces deterministic env struct layouts.

**Consequence**: Closures that capture variables now correctly carry a `captures: Vec<Capture>` list in `TypedExpr::Function`. The codegen heap-allocates environment structs for captured variables and packs `{fn_ptr, env_ptr}` closure values on the heap (not the stack) to support closures that outlive their creating scope.

## ADR-026: Iterator representation as heap-allocated struct; inline for-loop codegen

**Decision**: `range(a, b)` returns a heap-allocated `{i32 start, i32 end}` struct. `for(iterable, body)` is compiled to an inline LLVM loop: for arrays, an i64 index loop with `lin_array_get` element access; for `Iterator<Int32>` (range result), a counted `i32` loop. The `body` closure is inlined — the codegen recognizes `TypedExpr::Function` and `TypedExpr::LocalGet` to avoid creating/calling a closure struct when the body is a literal lambda.

**Rationale**: General iterators need function-pointer dispatch. For the common `range(...).for(i => ...)` pattern, generating a direct counted loop is equivalent to a C `for` loop with no overhead. Array iteration avoids boxing by loading `LinArrayElem.payload` directly. `TypeVar` substitution was added to `infer_call` and `infer_dot_call` to propagate the element type into the body lambda's parameter when the `for` intrinsic's parameter types use `TypeVar`.

**Consequence**: `range(0, n).for(i => ...)` and `arr.for(x => ...)` compile to native loops. The `iter` intrinsic is supported but `map`/`filter`/`reduce` are not yet compiled (runtime panic). Bidirectional type checking was extended (`check_expr` now guides function argument inference using expected parameter types from the call site).

## ADR-027: Concurrency via OS threads

**Decision**: `async(thunk)` spawns a real OS thread. Results are communicated back via `Arc<Mutex<PromiseState>>`. `await` blocks the caller thread until the promise resolves. `ThreadPool` uses `mpsc::channel` with a fixed set of worker threads. `Worker` uses `mpsc::sync_channel` for backpressure.

**Rationale**: OS threads are heavyweight but correct: each thread runs independently with no shared mutable state between concurrent thunks. A true async executor (tokio) would require pervasive `async/await` in the runtime.

**Consequence**: `async` thunks run on true OS threads. `await` blocks the caller thread (not a coroutine yield). Values must be JSON-serializable to cross thread boundaries (spec §24.4).

**Supersedes/absorbs ADR-028, ADR-029 and ADR-036.** (1) Cross-thread value transfer uses a JSON bridge: values crossing a thread boundary are serialized to a `JsonValue` enum (`Clone + Send`, no `Rc`/`RefCell`, mirroring Lin's data shapes) and deserialized on the receiving thread, because `Value`'s `Rc<RefCell<…>>` arrays/objects cannot cross threads. `Value::to_json_value()` returns `Err` for non-serializable types, enforcing spec §24.4: functions, iterators, promises, workers, and thread pools cannot cross a boundary. Transfer is O(size) deep copy, the correct cost given the `Rc`-based representation. (2) `async` also accepts an array of thunks `(() => T)[]` — spawning one thread per element and returning an array of promises in input order — so `await(async([t1, t2, …]))` is the natural fork-join idiom.

## ADR-033: FFI via `import foreign` and LLVM `declare`

**Decision**: Foreign function imports use `import foreign "<path>"` followed by an indented block of `val name: Type` declarations. The `foreign` keyword is added to the lexer; the parser reuses the existing indented-block machinery; the AST node is `Stmt::ForeignImport { path, bindings }`. The compiler emits an LLVM `declare` for each binding using the C-ABI type mapping, and library paths collected from `ForeignImport` nodes are passed to the linker step in `lin-compile`. `import foreign "lin-runtime"` is a special reserved path that is always linked and skips normal FFI type validation.

**Rationale**: Reusing `import` as the outer keyword makes foreign imports visually consistent with regular imports; the `foreign` keyword distinguishes them without a separate statement form, and the indented block mirrors function-body parsing (ADR-014). LLVM IR's `declare` is the correct mechanism for external C symbol resolution, and keeping library-path collection in the AST means `lin-compile` can drive the linker without a separate manifest.

**Consequence**: FFI requires `lin build`. The token `foreign` is now a reserved keyword. End-to-end FFI tests compile a C library to `.a` and call it from Lin via `lin build`. The type checker validates that foreign binding types are legal FFI types (numeric, Boolean, Null, or String).

**Supersedes/absorbs ADR-032 and ADR-037.** ADR-032 recorded the `import foreign "<path>"` syntax + indented-type-block AST shape (now folded into the Decision above); ADR-037 recorded that the checker validates *arity and types* at every foreign call site against the declared `(params) => ret` signature, so `add(1)` against a 2-param `add` is a compile-time arity error rather than a link- or run-time failure.

## ADR-034: `async` var-capture check via global slot tracking

**Decision**: The type checker rejects `async(f)` and `pool.async(f)` calls where the thunk `f` directly references any mutable `var` binding (either captured from a non-global outer scope, or referencing a global `var` from within the thunk body).

Implementation:
- `Checker` gains a `mutable_global_slots: HashMap<usize, String>` field, populated whenever a `Stmt::Var` is processed at global scope (when `function_scope_depths` is empty).
- `first_mutable_capture(expr, mutable_globals)` checks a `TypedExpr::Function` for: (a) any `Capture` where `is_mutable == true`; (b) any `LocalGet` in the body that references a slot in `mutable_global_slots`. Body scanning does not recurse into nested `Function` nodes (inner lambdas have their own capture check when their own `async` call is analysed).
- In `infer_call`, after building `typed_args`, if `func == Ident("async")`, every thunk argument is checked. Same check on the thunk args of `infer_dot_call` when `method == "async"`.
- The check also registers the concurrency builtins (`async`, `await`, `parallel`, `race`, `timeout`, `retry`, `threadPool`, `worker`) as intrinsics in `register_intrinsics()` using `TypeVar`-based signatures, so they resolve instead of producing "Undefined variable" errors.

**Rationale**: Sharing mutable state across OS threads without synchronisation leads to data races. Lin's `var` is captured by `Rc<RefCell<Value>>` in the interpreter and by pointer in the compiler — neither is `Send`. The spec (§24.2) requires a compile-time error. Global vars are not recorded as "captures" (they're accessed directly via `LocalGet` with slot from global env), so a two-pronged check is needed.

**Consequence**: `async(() => counter = counter + 1)` where `counter` is a `var` produces a compile-time error with a help message suggesting snapshot capture. `async(() => message)` where `message` is a `val` is allowed.

## ADR-035: Match arm narrowing via scope-shadowing

**Decision**: In `check_match_arm`, when the pattern is `Is(TypeCheck(narrowed_ty))` and the scrutinee is a simple `Ident(name)`, the checker defines a new binding `name: narrowed_ty` in the arm's scope. This shadows the outer binding for the duration of the arm body, giving the narrowed type to all references to the scrutinee within that arm.

**Rationale**: Using scope-shadowing instead of mutating the original binding avoids any state-management burden when leaving the arm scope: `pop_scope()` automatically removes the shadow. It also correctly handles nested scopes and captures within the arm body.

**Consequence**: `match x is Int32 => x + 1` correctly type-checks because `x` inside the arm body resolves to the arm-scope `x: Int32`, not the outer `x: Int32 | String`.

## ADR-038: Optional `else` in `if` expressions — implicit `else null`

**Decision**: The `else` branch of an `if` expression is optional. When omitted, the parser synthesizes `Expr::NullLit` at the `if` expression's span as the implicit else branch. The type checker then unions the then-branch type with `Null`, yielding `T | Null` as the expression's type.

**Rationale**: Side-effect-only patterns like `if cond then push(arr, item)` are idiomatic and common in the stdlib. Requiring `else null` is pure noise in these cases — the intent is clear and the result is always discarded. The `else null` pattern also appeared in predicate-style code (`if found == null && f(item) then found = item else null`) where the explicit null was a placeholder with no meaning. Synthesizing `NullLit` at parse time means the AST shape is unchanged — no `Option<Box<Expr>>` needed in `Expr::If` or anywhere downstream.

**Consequence**: The result type widens to `T | Null` when `else` is absent. Code that uses the result of an `else`-less `if` without handling the `Null` case will pass type-checking silently (the union just grows). This is an acceptable tradeoff: the common case (result discarded) gets cleaner syntax, and the footgun (accidentally using a `T | Null` result as `T`) is the same class of error already present whenever any function returns `Null`.

## ADR-039: Memory management — deterministic reference counting, cycles are user responsibility

**Decision**: Lin uses deterministic reference counting (RC) for all heap-allocated values (strings, arrays, objects, closures). RC operations are inserted by the compiler; the runtime provides `lin_string_release`, `lin_array_release`, `lin_object_release`, and `lin_closure_release`. Release functions recurse into heap-typed elements/values so that nested structures are freed correctly. Reference cycles between heap objects are **not** detected and will leak — this is a documented limitation.

**Rationale**: RC is deterministic (no GC pauses), predictable, and systems-friendly. The Perceus approach (Reinking et al., PLDI 2021, used in Koka and Lean 4) shows that compile-time linearity analysis can elide most RC operations, making the overhead negligible for common functional-style code. Cycle detection requires either programmer annotations (`Weak<T>`, as in Swift/Rust) or a runtime trial-deletion pass (as in Nim ORC). Both add complexity. Cycles are uncommon in the data pipeline / request handler patterns Lin targets. The tradeoff is acceptable: correctness for acyclic data (the common case), documentation for the cycle edge case.

**Consequence**: Programs must not create reference cycles between long-lived heap objects if they care about memory usage. The typical fix is to break cycles by setting a field to `Null` before the data becomes unreachable. Future work: `Weak<T>` type (Option B) or ORC-style trial deletion (Option C) can be layered on top without changing the base RC contract.

## ADR-040: Formatter preserves comments (reverses the original "drops comments" decision)

**Decision**: The `lin fmt` formatter (`lin-parse/src/formatter.rs`) preserves `//` line comments (Lin's only comment form). This reverses the original ADR-040 decision, which dropped them. The lexer no longer discards comments: `skip_line_comment` records each one as a `Comment { span, text, own_line }` on a side channel exposed by `Lexer::comments()`, **without** changing the token stream (same kinds, `newline_before` flags, and span lengths — proven by `lin-lex` unit tests). The CLI (`lin-parse::Formatter::with_comments(source, comments)`) reattaches each comment to an AST anchor and re-emits it.

**Rationale**: A `fmt --check` CI gate is unusable if formatting erases the ~1040 comment lines across the stdlib. The chosen design is option (b) from the original ADR — a span-based reattachment pass — but constrained to be idempotent and lossless rather than "heuristic and fragile": comments attach only to a fixed, well-defined set of anchors (every statement in a statement-list slot, every block tail, and each single-expression function body), and the attachment rule is deterministic. The token stream is left untouched (side channel), so no parser/checker/LSP consumer is affected.

**Mechanics**: Own-line comments attach as **leading** lines to the first anchor that starts after them (or dangle at EOF). Trailing comments (code precedes them on the line) attach to the closest anchor lying **entirely on the same source line**; a single-expression function body that is itself control flow (`if`/`match`) is excluded from trailing attachment because its recorded span is a single token and it renders multi-line. A trailing comment with no single-line anchor is demoted to a leading own-line comment of the next anchor (lossless, deterministic). Text is right-trimmed at capture; leading comments sit at the anchor's indentation; trailing comments sit exactly one space after the code.

**Consequence**: `lin fmt` round-trips comments and is idempotent over the whole `stdlib/` + `examples/` corpus with zero comment loss. The one documented imprecision: comments that trail expressions deep inside a multi-line `if`/`match` chain (which have no anchor of their own — only statements, tails, and function bodies are anchors) are hoisted to own-line comments after the enclosing construct rather than staying inline. They are preserved, just repositioned. Example: the per-branch `// q: left +` comments in `examples/raspberry-controller/main.lin` become a block of own-line comments after `applyKey`.

## ADR-041: Default argument values — trailing-comma inversion + per-arity adapters

**Decision**: A parameter may carry a default value (`(a: Int32, b: Int32 = a + 1)`). Optional parameters must be last. Because Lin already gives "supply fewer arguments than declared" a meaning — left-to-right partial application (spec §15.2) — and default values want the *same* call shape to mean "call now, fill the rest from defaults", the two are disambiguated at the call site by an **explicit trailing comma**: `f(x,)` partially applies; `f(x)` is a complete call that fills any omitted trailing defaults (and is an error if an omitted parameter has no default). This inverts the previous rule, where bare under-application curried. `Type::Function` gains a `required: usize` field (count of non-defaulted leading params), excluded from structural compatibility but serialized into module signatures so importers can check arity. Defaults are filled by the **defining** module, not the caller: for a function with optional params, lowering synthesizes one **adapter** per shortfall arity (`f$default{k}`) that binds the omitted parameters to their default expressions and calls the real function. Static calls (direct, dot, imported-by-symbol) route to the adapter by name/id. For the first-class-value path (`val g = f; g(x)`), each default-bearing function gets a static **descriptor** (`{ total, required, entries[] }` of boxed-ABI wrappers) stored at closure offset 32; an indirect under-arity call dispatches through it. The closure struct grew from 32 to 40 bytes (all closures, uniformly, so the runtime frees a single fixed layout); the descriptor is a never-freed static global.

**Rationale**: Synthesizing adapters as `TypedExpr::Function` and lowering them through the normal function path means RC, coercion, and earlier-parameter/chained default references (`(a, b = a + 1, c = b + 1)`) all work for free — defaults are just ordinary expressions evaluated in a scope where the preceding parameters are bound. Filling defaults in the defining module (rather than serializing default *expressions* into `.sig` files for callers to inline) keeps signatures small and makes cross-module defaults work by symbol reference. The trailing-comma marker resolves the currying/default-fill ambiguity at the exact site where intent lives, with zero new tokens. Putting `required` in `Type::Function` but excluding it from compatibility means default-ness never blocks an assignment or argument match — a `(Int32, Int32) => Int32` value is interchangeable whether or not its second parameter had a default.

**Consequence**: Existing code that relied on bare under-application to curry (e.g. `add(10)`) must add a trailing comma (`add(10,)`); within this repo only one example needed migration. The closure ABI change (32→40 bytes) touches every closure allocation site and `lin_closure_release`; all are updated together. A self-recursive *default-fill* tail call cannot use the TCO fast path (it targets a different-arity adapter), so it lowers as an ordinary call. Implementing the indirect path surfaced and fixed a pre-existing bug in the boxed-ABI wrapper: it inferred the Lin return type from the LLVM return kind and treated every pointer return as already-boxed Json, so a function value returning a raw `String`/`Array`/`Object` crashed the indirect caller (which unboxes); the wrapper now takes the real Lin return type and boxes correctly.

## ADR-042: All call paths must coerce arguments to parameter types

**Decision**: Every call-lowering path in `lower_call` (`lin-ir/src/lower.rs`) coerces each argument to the callee's declared parameter type via `lower_call_arg` (which boxes a concrete value to `Json`/`TaggedVal*` when the parameter is union/Json) and retains heap arguments via `retain_call_arg`. This includes the fallback **indirect-call path** — a call through a closure *value* (`val f = ...; f(x)`, a closure passed as a parameter, or any non-statically-resolved callee) — which previously lowered its arguments with a bare `lower_expr` and no coercion.

**Rationale**: Lin's uniform closure ABI passes `Json` parameters as boxed `TaggedVal*`. The named-function and imported-function paths already box concrete arguments (an `Array`, `Object`, or scalar) to match a `Json` parameter; the indirect path is just another way to reach the same ABI and must follow the same rule. The callee's parameter types are read from the callee expression's `Type::Function` signature, identically to the other paths.

**Consequence**: Fixes silent data corruption — before this, an `Array` (or any heap value) passed to a `Json`-typed closure parameter reached the callee as a raw `LinArray*` instead of a boxed `TaggedVal*`. The callee read its tag/payload from garbage, so the value behaved as a different (or empty) object and *mutations through it were lost* (e.g. `push` into an accumulator passed to a stored closure left the original array empty). This is the argument-side analog of the return-side boxing bug noted in ADR-041; together they make the first-class-function/closure path representation-correct for all heap types. Regression: `test_array_passed_to_closure_value_mutates` in `crates/lin/tests/integration.rs`.

## ADR-043: Async concurrency — copy-by-default RC, catchable faults at the thread boundary

**Decision**: Turning the synchronous async stub into real OS-thread concurrency (spec §24) is gated on three model decisions, locked in here (see `docs/ASYNC_DESIGN.md` for the full plan):

1. **RC under threads = Option C (transfer by deep copy) by default, plus two opt-in shared types `Shared<T>` and `Frozen<T>`.** Refcounts stay non-atomic on the single-threaded hot path. Values crossing a thread boundary (a thunk's captured env, and the transferable result returned through a promise) are **deep-copied** so each thread owns a private, disjoint object graph — nothing is shared, so non-atomic RC is sound. The set of boundary-crossing values is exactly the transferable types (JSON-shaped, acyclic, no `Function`/`Iterator`/cycles — already enforced by the checker), so a deep copy is total and bounded. `Shared<T>` (atomic-RC box + `RwLock`, accessor-only, copy in/out) is the escape hatch for shared *mutable* state; `Frozen<T>` (immortal deep-frozen graph, zero-copy lock-free reads via mutation-inference coercion) for shared *read-only* state. Atomic-RC-everywhere (Option A) and dynamic shared-flag RC (Option D) and COW are rejected (§2.3, §2.3.3) — they tax the non-threaded hot path we just optimised.

2. **Catchable faults via a thread-local async-boundary flag.** A runtime fault (`lin_panic`, array OOB, division by zero, non-exhaustive match, null-spread) historically called `std::process::exit(1)` — uncatchable, correct at the top level (spec §20.1). All such sites now route through `crate::fault::runtime_fault(msg)`: inside an async boundary (thread-local depth > 0) it `panic!`s and unwinds to the boundary's `catch_unwind` (becoming an `Error` at `await`, spec §24.2.2); outside, it keeps the `process::exit(1)` behaviour. The spawned thunk runs inside `fault::with_async_boundary`. `lin_exit` (user `exit()`) is unaffected — intentional termination stays a real exit.

3. **`nounwind` is dropped program-wide when the program uses async.** User-emitted Lin functions are marked `nounwind` (sound: value-based errors, frames never unwind) — but a fault inside a thunk now unwinds *through* Lin frames to the boundary, so `nounwind` is unsound for any function reachable from a thunk. We cannot cheaply prove a given function is unreachable from a thunk, so codegen conservatively drops `nounwind` from all user functions whenever the program references any concurrency intrinsic (detected in `lin-compile` by scanning every module's intrinsic map for the `lin_async`/`lin_parallel`/`lin_worker`/… family, which is reachable only through `std/async`). The overwhelmingly common non-async program keeps `nounwind` and its optimisation value (doc §2.4.3 option a).

**Rationale**: The spec's correctness-by-construction guards (`var`-capture ban, transferable-only returns) were designed anticipating threads — they guarantee a thunk shares only immutable, JSON-shaped, acyclic data with its parent, which is exactly what makes Option C's deep copy total and keeps the single-threaded path atomic-free. Catchable faults are the entire point of `async` being Lin's fault-isolation boundary; routing every fault through one helper that branches on a thread-local keeps the top-level `exit` semantics intact while making thunk faults recoverable. The runtime is `panic = "unwind"` (unchanged), so `catch_unwind` works and unwinding crosses the LLVM/Rust boundary; the only requirement is that the Lin frames in between are not `nounwind`, hence decision 3.

**Consequence**: Programs that use async pay a small code-size/optimisation cost (no `nounwind` on user functions) — measured negligible, and zero for non-async programs. Deep-copying large transferable results at a boundary is the cost of Option C; `Shared<T>`/`Frozen<T>` are the escape hatches so we are never forced into all-atomic RC. `Shared<T>` reintroduces deadlock and RC-cycle hazards (documented); `Frozen<T>`'s immortal graphs are never freed (load-once data only). A genuine (non-fault) panic inside a thunk is also caught and surfaced as an `Error` — acceptable, since a runtime bug in a worker should isolate to that worker rather than abort the process. (Implementation note, post-merge with Rust 1.81+: a panic must not unwind out of a plain `extern "C"` runtime fn — the faulting runtime functions and the thunk-call transmutes are `extern "C-unwind"`, and async-reachable Lin frames get `uwtable` so the unwinder can walk through them.)

## ADR-044: `Shared<T>` — opt-in shared mutable state (runtime box; type enforcement deferred)

**Decision**: `Shared<T>` (ADR-043 §2.3.1) is implemented as a runtime box: an **atomic**-refcounted `SharedBox` wrapping an `RwLock` over the inner value (stored as a boxed `TaggedVal*`). Four built-ins, exported by `std/async`: `shared(v)` (deep-copy-in, atomic rc=1), `get(s)` (read lock, deep-copy a snapshot out), `set(s, v)` (write lock, deep-copy in), `withLock(s, f)` (write lock held across `f`, which mutates the inner value in place; `f`'s result is deep-copied out). The box is boxed as `TaggedVal*(TAG_SHARED)`; its retain/release route to atomic `lin_shared_retain_box`/`lin_shared_release_box`, and the thread-transfer copy path **shares** it by an atomic bump rather than copying through (the nesting rule). The inner object graph keeps ordinary non-atomic RC — it is only reachable while a lock is held, so all access is serialized.

**Rationale**: This delivers the load-bearing guarantee — real, race-free shared *mutable* state without taxing the single-threaded hot path (only the box's refcount is atomic; only `Shared` operations take a lock). Copy-in/copy-out at every boundary means no live reference into the inner graph escapes the lock, so the inner non-atomic RC is sound. Validated under ASan and a multi-threaded `#[test]` (8 threads × concurrent get/set) plus a Lin-level concurrent-`withLock`-push test (no lost updates).

**Consequence**: The compile-time **accessor-only enforcement** (rejecting `push(s, 7)`, indexing, auto-unwrap on a `Shared<T>` as a type error) is **now wired** (follow-up landed): a dedicated `Type::Shared(Box<Type>)` variant is threaded through the checker, IR, and codegen. `shared`/`get`/`set`/`withLock` are typed against it (`shared: <T>(T) => Shared<T>`, `get: <T>(Shared<T>) => T`, `set: <T>(Shared<T>, T) => Null`, `withLock: <T,R>(Shared<T>, (T)=>R) => R`); the stdlib wrappers annotate `Shared` (resolvable by name, like `Iterator`); and compat makes `Shared<T>` **invariant** — compatible only with another `Shared<U>` (inner types recursed) and explicitly NOT widening to `Json`/`TypeVar`, so it can't silently flow into a `Json` parameter and lose the guard. Any non-accessor op on a `Shared` value is therefore a type error (`Argument 1 has type Shared<…>, expected …`). At runtime it is still a boxed `TaggedVal*(TAG_SHARED)` (`is_union_type` / `is_union_ty` include it; RC dispatches through the tag-aware path; `capture_kind` → `CAP_TAGGED` so the transfer copy path shares it by atomic bump — the nesting rule). NOTE: `lin check` does not resolve imports, so this enforcement is visible under `lin build`/`lin run` (which do); a bare `lin check` still sees imported names as `Json`. Remaining caveats unchanged: `withLock` mutates in place, so a scalar accumulator (`n => n + 1`) does not persist (use a one-element array or `get`/`set`); `set` collides by name with `std/array`'s `set` when both are imported (alias one); `Shared<T>` makes reference cycles reachable and Lin has no cycle collector (ADR-039); `withLock` reintroduces deadlock potential (no reentrancy, keep critical sections short).

## ADR-045: `Frozen<T>` — opt-in shared read-only state via deep immortal seal (coercion deferred)

**Decision**: `Frozen<T>` (ADR-043 §2.3.2) is implemented as a deep, transitive **immortal seal**. `frozen(v)` (runtime `lin_freeze`, exported by `std/async`) walks the transferable graph rooted at `v` and saturates every heap node's refcount to `IMMORTAL_RC` (string/array/object, recursively). The existing immortal guard on strings is extended to arrays and objects: `lin_array_release`/`lin_object_release` and the array/object arms of `retain_tagged_payload` (and `lin_rc_retain`, already guarded) become **no-ops** when a node's refcount is `>= IMMORTAL_RC`. The thread-transfer copy path shares an immortal array/object by reference (zero-copy), never deep-copies through it. `frozen(v)` returns `v` (now frozen) — the value keeps its plain type, so readers use it transparently.

**Rationale**: The trap with shared read-only data is that a read-only function compiled once against `T` does **non-atomic** `retain`/`release` on its parameter; run on N threads sharing one value, those refcount writes race even though the contents are never written. Making the graph immortal turns retain/release into guarded no-ops that only *read* the sentinel — and a race needs a writer, so concurrent reads of the count are race-free. Therefore the read-only function's existing non-atomic RC runs correctly on a shared frozen value **with no recompilation, no lock, and no atomics**. This is the interned-string immortality trick (already shipped) generalized from one string to a whole graph. Validated by a multi-threaded test (a frozen array read concurrently by N threads) under ASan.

**Consequence**: **Immortal ⇒ never freed.** `frozen` is for load-once, program-lifetime reference data (one O(size) seal at startup); a `frozen()` value created-and-discarded in a loop **leaks** — documented in STDLIB.md. The **mutation-inference read-only coercion** (the §2.3.2 rule that lets a `Frozen<T>` be passed to a `T` parameter *iff the callee doesn't mutate it*, rejecting mutating callees at compile time) is **deferred** — it needs a dedicated `Type::Frozen` variant plus an interprocedural per-parameter mutation-inference pass cached in `ModuleSignature`. Today `frozen(v): T` returns the plain type, so reads "just work", but *mutating* a frozen value is not a compile error — the mutation is silently a no-op on the immortal node (and lost) rather than diagnosed. The runtime immortality/zero-copy-share semantics are fully enforced and safe. A frozen graph is acyclic and immutable, so unlike `Shared<T>` it adds no deadlock and no new cycle hazard.

## ADR-046: `Error` built-in type + `is Error`; `await`'s `T | Error` wrapping deferred

**Decision**: `Error` is a built-in type resolving to the structural shape
`{ "type": String, "message": String }` (`resolve.rs::error_type`) — the conventional error
value (spec §20) and the exact object the async runtime builds when a thunk faults
(`{ "type": "error", "message": <msg> }`). `is Error` (and any `is <ObjectShape>`) lowers to a
**field-presence** check (`HasPattern` on the object's keys) rather than a bare tag check, so it
matches error-shaped objects specifically instead of every object. This makes the spec's §24.2.2
pattern work:

```txt
match await(p)
  is Error => print("failed: ${result}")
  else     => use(result)
```

**Rationale**: `Error` has no special control-flow behaviour (§20), so a structural object type is
the faithful model — it composes in unions and narrows by shape. Routing object-shaped `is`
checks through the existing `HasPattern` machinery reuses the same field-presence test as
`is { .. }`, with no new runtime support.

**Consequence (deferred)**: The other half of §24.2.2 — `async` wrapping its result as
`Promise<T | Error>` so the checker **rejects using an uninspected `Error` as a plain `T`** — is
**not implemented at the time of this ADR**, and is not a localized change. The entire async surface
is `Json`-typed through the stdlib wrappers (`async = (f: Json): Json`, `await = (p: Json): Json`);
there is no parametric `Promise<T>` tracking (there never was — the synchronous stub was `Json`-typed
too). `await(p)` therefore returns `Json`, which coerces freely to any type, so the "reject uninspected
Error" rule cannot be enforced without first making `await` generic over the thunk's return type — a
feature spanning the checker's inference, the intrinsic signatures, and module signatures. That was
its own project; until it landed `is Error` gives users the *runtime* discrimination the spec intends,
and a fault is always a well-formed `Error` object. Likewise §24.2.3 nested-promise auto-flatten IS
now implemented (runtime: `await` recurses through a `TAG_PROMISE` result). (The deferred enforcement
later shipped at `await`, not `async` — see ADR-070.)

## ADR-047: Two unary operators — bitwise `~` and logical `!`; unary minus is sugar for `0 - x`

**Decision**: Lin has exactly **two** prefix unary *operators*: bitwise complement `~` (§27.2) and logical not `!` (§8.1). There is **no unary-minus operator** — but a leading `-` on a non-literal is parse-time **sugar for subtraction from zero**: `parse_primary` desugars `-expr` into the binary `0 - expr` (`Expr::BinaryOp { left: IntLit(0), op: Sub, right: expr }`, `lin-parse/src/parser/expr.rs`), so there is no `UnaryOp::Neg` in the AST and no negation typing rule — `-x` *is* `0 - x` and obeys ordinary numeric typing. (A leading `-` directly before digits is instead absorbed into a numeric literal by the lexer, §2.7.) This desugaring predates the `!` work and is relied upon by checked-in code (e.g. `examples/matrix/matrix.lin`'s `-s`). Both genuine unary operators are right-associative and bind tighter than `*` but looser than postfix call/index/dot (§8.2), so `!!x` parses as `!(!x)` and `!a == b` parses as `(!a) == b`. Negated *patterns* (e.g. `is !true`) are explicitly out of scope.

**Rationale**: This supersedes the original v1 design of `~` as the *single* sanctioned unary operator. `!` was added because boolean negation otherwise had to be spelled `x == false` — pervasive boilerplate in stdlib (`std/array`) and user code. It reuses the existing unary pipeline end-to-end: the lexer emits a new `Bang` token, the AST gains `UnaryOp::Not`, IR lowering maps it to the same `crate::ir::UnaryOp::Not` as `~`, and for an `i1` a bitwise-not *is* a logical-not, so codegen's existing `build_not` arm needs no change. When the operand is not statically `Bool` (e.g. a boxed `TypeVar` through a generic lambda), lowering routes it through `lower_cond_as_bool` first to unbox/coerce to a raw `i1`.

**Consequence**: Typing rules differ by operator — `~x` requires an integer and yields that integer type; `!x` requires `Bool` and yields `Bool`; a float operand to `~` (or a non-`Bool` to `!`) is a compile-time error. This supersedes the spec's older "the only unary operator is `~`" / "no unary operators in v1" statements (§2.7, §8.1, §27.2, decision-list #9), now updated to "exactly two unary operators (`~`, `!`); leading `-` is a numeric literal or sugar for `0 - x`". An earlier revision of this ADR (and the matching spec text) overstated the rule as a flat "no unary minus", which read as though `-x` on a computed value was rejected; in fact it has always parsed as `0 - x`, so the wording now distinguishes "no unary-minus *operator*" from "`-x` is sugar".

**Supersedes/absorbs ADR-059**, which restated the same two-unary-operator rule and spec-text updates; both records are now this one.

## ADR-048: `Json` is a covariant sink — closing the Json→concrete cast hole

**Decision**: `Json` (modelled as `Type::TypeVar(u32::MAX)`, see `types.rs::is_json`) is made a **covariant sink**: anything is assignable *into* `Json` (concrete `T → Json` stays allowed, so `writeJson(value: Json)` and the pervasive "store anything as Json" patterns keep working), but a `Json` *value* is **not** assignable *out* to a fully-concrete **structured object** target — one that (after unfolding `Named` types) is an `Object` with at least one required, non-nullable field. So `val p: Person = readJson(...)` (where `Person = {name: String, age: Int32}`) is now a type error; the value must be decoded via `fromJson` (ADR-049) or narrowed via `is`/`has`. The fix splits the old blanket `(_, TypeVar(_)) | (TypeVar(_), _) => true` arm in `compat.rs` into: (1) `(_, TypeVar(MAX)) => true` (sink), (2) `(TypeVar(MAX), target) => lenient_json || !requires_structured_decode(target)`, (3) the existing permissive arm for all *other* (non-MAX) TypeVars — genuine inference vars, the `9000+` generic slots, intrinsic vars — so inference is unchanged. `requires_structured_decode` deliberately treats only required-field objects as the hazard: `Json` flowing into scalars (`Int32`/`Int64`), buffers (`UInt8[]`), opaque handles, open objects (`{}`), arrays, functions, iterators, or anything still containing a TypeVar stays permissive — those are the language's handle/buffer/polymorphic-return patterns, which have no `fromJson` remedy and predate this change.

**Rationale**: The old rule made `Json` bidirectionally compatible with *everything*, so a value read from a `Json` source could be silently bound to a richly-typed annotation with **zero validation** — the annotation was a lie. Drawing the line at required-field objects catches the real "I claimed this JSON is a `Person`" hazard (the one a decoder can fix) while not breaking the thousands of existing scalar/handle/buffer flows. The leniency is scoped: a per-`Checker` `lenient_json` flag is set **only** for the trusted embedded stdlib (whose wrappers forward `Json` handles into concrete intrinsic/foreign params by design, e.g. `lin_parse_json`, `pathMatch`); user modules and user-defined imported modules always check strictly. After this change the only sound `Json → T` conversions are (a) `fromJson` (validated decode) and (b) `is`/`has` narrowing (a separate `checker/pattern.rs` path that branches on `ty.is_json()` directly and is backed by runtime tag checks, so it stays sound and is unaffected).

**Consequence**: The predicted blast-radius migration sites (`pathMatch`'s `String` params, `lin_parse_json`'s `String` param) did **not** in fact break: their targets are scalars, not required-field objects, so the narrower `requires_structured_decode` rule leaves them permissive — no stdlib widening was needed. The whole stdlib, example suite, and test suite compile unchanged. The scalar/handle escape hatch is a deliberate soundness gap (`Json → Int32` is still unchecked) accepted to avoid a disruptive migration; tightening it later (e.g. a `fromJson` for scalars) is additive. `lin build` of `val p: Person = readJson(...)` (and the direct `val p: Person = jsonReturningCall()` form) now surfaces `Expected type {...}, got ?T4294967295`; the remedy is `Person.fromJson(...)`. (Note: `lin check` of a *single* file leaves imported functions' return types as fresh inference vars rather than `Json`, so the gate cannot fire there for an imported call — the full `lin build` pipeline, which resolves import signatures, is the authority. A bug where a zero-param or all-`Json`-param `Json`-returning function was misclassified as the opaque `Function` annotation — and its `Json` return freshened into a permissive inference var, slipping the gate — was fixed in `infer_call` by requiring the opaque shortcut to have a *non-empty* all-`TypeVar(MAX)` param list.)

**Scope decision — total vs structured (empirically locked)**: a *total* gate (rejecting ANY `Json → concrete T`, including scalars and arrays) was implemented and run against the full suite. It broke (a) the stdlib's pervasive **polymorphic-return idiom** — `slice`/`concat`/`accept`/`wait`/etc. return `Json` and are routinely assigned to concrete `val`s (`val sub: UInt8[] = slice(bytes, 1, 4)`, `val code: Int64 = wait(pid)`, socket `accept(): Int32`) — and (b) **`is`-narrowing into a concrete branch** (`if j is String then j else ""`, whose narrowed value is still statically `Json`). Empirically: `test_is_narrowing_still_works`, `test_slice_preserves_element_type`, `test_net_tcp_loopback_echo`, `test_net_udp_loopback_roundtrip`, `test_proc_spawn_read_wait`, `test_proc_wait_exit_code` all failed under the total gate. These patterns have **no `fromJson` remedy** and forcing one is hostile, so the total scope was rejected and the gate is **scoped to required-field structured objects** — the genuine "unchecked object decode" hazard. This matches the structured-object-only conclusion anticipated in the plan.

## ADR-049: `fromJson` — type-directed JSON decode (descriptor-driven runtime interpreter)

**Decision**: `T.fromJson(json)` (and the equivalent `fromJson(T, json)`) is a **checker special form** that validates a `Json` value against the target type `T` and yields `T | Error`. It is recognised by the surface name `fromJson` at the call site (intercepted in both `infer_call` and `infer_dot_call` *before* arg0/receiver is inferred as a value, since arg0 is a *type*, not a runtime value — so unlike `print`/`for` no `lin_*` wrapper can express it). `std/json` exports a `fromJson` stub purely so the import resolves and `lin check` sees the name; the stub body is never used for real call sites. Validation is implemented as a single generic runtime interpreter `lin_from_json(value, descriptor)` driven by a compact, position-relative byte **descriptor** emitted per call site by codegen (`DescEncoder`), so emitted code is O(1) per site and recursion/cycles are finite back-edges (memoised by named type). The interpreter walks value+descriptor in lockstep, returns the input **cloned** (`lin_tagged_clone`, +1 independently owned) on success or a fresh `Error` on the first structural mismatch, building a JSONPath-ish `path` (e.g. `$.address.city`) during the walk. `Error` is a structural object alias `{ "type": "error", "message": String }` (resolved by `resolve_named_cycle`, not a new `Type` variant — cf. ADR-044), and the runtime error value also carries a `"path"` field, which width subtyping permits.

This ADR records three load-bearing semantic choices and their trade-offs:

- **(a) Union variant selection is FIRST-MATCH-WINS.** A `KIND_UNION` node tries each variant in declaration order and accepts the first that validates. **Trade-off**: for overlapping, non-discriminated object variants the most-permissive / first-listed variant *shadows* more-specific ones — e.g. with `{ "k": String } | { "k": String, "w": Int32 }`, an input that has both `k` and `w` matches the first variant. The *runtime data is fully preserved* (the same value is returned, no fields are dropped), but the *static type* the program reasons about is the matched variant, which may be the wider one. **Recommendation**: give union variants a discriminant field (e.g. a literal `"type"` tag) so exactly one variant matches; list more-specific variants first when overlap is unavoidable. First-match-wins was chosen for v1 because it is predictable, order-explicit, and matches the spec's first-error policy spirit; a "best/most-specific match" rule would be ambiguous and costly.

- **(b) Number policy is target-type-driven.** An **integer** target requires the JSON number to be integral and within the target's width/signedness range (a float like `3.14` is rejected; an integral float like `5000000000.0` against `Int32` is rejected as out of range). A **float** target (`Float32`/`Float64`) accepts any JSON number. An **unconstrained** target (`Json`/a `TypeVar`, encoded as `KIND_JSON`) accepts any number as-is with no narrowing — number-range validation is intentionally skipped there by design. (Note: a bare suffixless integer *literal* in Lin source is typed `Int32` and truncated by the lexer per spec §21 *before* it can reach `fromJson`, so genuine out-of-range integers arriving from real JSON parsing are the cases the range check guards.)

- **(c) `Error` is a structural object alias, but `is Error` IS made to discriminate.** `Error` is `{ "type": String, "message": String }` (open, resolved by `resolve_named_cycle`; the runtime value also carries `"path"`). A *bare* tag check would match any object, so to make the agreed idiom `match result | is Error => .. | is Person => ..` work, **`is Error` is desugared in the checker (`check_pattern`, `Pattern::TypeName == "Error"`) into the value-constrained object pattern `{ "type": "error", "message": _ }`.** This reuses the existing object-pattern lowering (`lower_object_pattern_test`) which checks field presence AND `scrut["type"] == "error"` at runtime — exactly what distinguishes a decode failure (always `"type": "error"`) from a decoded value (any other shape). Standalone `Expr::Is` was routed through the same object-pattern path (its old `IsType` lowering mapped an object pattern to `Type::Never`/tag `0xFF`, which never matched). Exhaustiveness was taught to count this desugared pattern as covering the `Error` union variant. Chosen over adding a dedicated `Type::StrLit` literal-type (which would touch ~20 exhaustive `Type` matches across codegen/boxing/representation — too invasive, cf. ADR-044) and over a new `Type` variant. **Former residual trade-off, now RESOLVED by ADR-054:** when this ADR was written, the standalone expression form `result is Person` compiled to a bare `TAG_OBJECT` check, so it *also* matched the Error object and the `is Error` arm had to come first or a decode failure would route into the `Person` arm and fault on `result["name"]`. ADR-054 makes `is <ObjectType>` check the target's required fields (and types) in **both** the match-arm and expression paths, so `is Person` no longer matches an Error object and the arm order is no longer load-bearing. The `result["type"] == "error"` discriminant still works and remains valid for code that prefers it.

**Rationale**: A descriptor + one generic interpreter (validator strategy C) beats inlining per-site LLVM (code bloat, recursion needs emitted helpers) and per-type generated functions (still heavy IR, forward-decl cycle handling): it keeps generated code tiny, reuses the existing tag/unbox runtime primitives, makes recursion trivial (table indices), and makes the walker ordinary, unit-testable Rust. Returning a **clone** rather than the same retained pointer (a deviation from the original plan note) keeps ownership symmetric: the `fromJson` result is registered `+1 owned` in lin-ir, and the input value temp is independently owned and released by normal liveness — returning the *same* pointer would alias two owners and double-free. Verified under AddressSanitizer: no use-after-free / double-free; the decode-`Error` builder (`make_decode_error`) releases its locally-created key/value strings after `lin_object_set` retains them, so error values are leak-clean (only the program-lifetime interned string-literal cache leaks, as elsewhere).

**Consequence**: The input `Json` is **borrowed** (never consumed); the result is unconditionally a fresh `+1`-owned value (clone on success, fresh `Error` on failure); the descriptor is a static const global (never freed). Array/fixed-array/object/union targets can only be named via a `type` alias or built-in name because the special form requires arg0 to be a bare identifier (`Int32[].fromJson(...)` does not parse; use `type IntArr = Int32[]`). A user who shadows `fromJson` with a *local* binding defers to the normal call path; a global user `fromJson` called with a real type-name arg0 would still be intercepted (an accepted, low-risk corner). `Iterator`/`Function`/`Never`/`TypeVar` target fields encode as `KIND_JSON` (accept-any), since they are not JSON-shaped and have no meaningful structural check.

**Supersedes/absorbs ADR-052.** A `Type::StrLit(s)` target field encodes as a dedicated descriptor node `KIND_STRLIT = 10` carrying `{ u16 lit_len, lit_bytes }`, and the runtime interpreter (`lin_from_json`, `lin-runtime/src/decode.rs`) validates the JSON value is a string **and** equals that exact literal (e.g. `expected "alpha" at $.kind, got "beta"`). This replaces the original v1 `KIND_STRING` placeholder, which accepted any string in a discriminant slot and let first-match-wins (a) silently mis-decode a wrong-tagged union (e.g. `Result.fromJson({ "type": "bogus", … })` succeeded). With `KIND_STRLIT`, each union variant's discriminant literal is checked during the probe, so `{ "type": "failure", … }` fails the `"success"` variant and correctly falls through. A plain `Type::Str` field still encodes as `KIND_STRING` (any string), so ordinary string decoding is unchanged; encoder (`codegen/intrinsics.rs`) and decoder are kept in sync.

## ADR-051: Singleton string-literal types

**Decision**: A string literal in **type** position is a singleton type. `Type::StrLit(String)` is a
new `Type` variant (mirrored by `TypeExpr::StringLit` in the surface AST and parsed in
`parser/types.rs` before the `_` fallback). It admits only the one string value. This makes the
spec §19 tagged union discriminate at compile time:

```txt
type Result<T, E> = { "type": "success", "value": T } | { "type": "failure", "error": E }
val r:   Result<Int32, String> = { "type": "success", "value": 1 }   // OK
val bad: Result<Int32, String> = { "type": "nope",    "value": 1 }   // compile error
```

The guiding principle is **"`StrLit` is `Str` at runtime, a singleton at check-time."** A
`StrLit("x")` value is represented at runtime *identically* to a `String` — same `TAG_STR`/6 tag,
same `string_ptr_type` llvm type, same boxing/unboxing, same refcounting, same `toString`. So
nearly every exhaustive `Type` match in `lin-ir`/`lin-codegen` simply grew a new arm grouped with
`Type::Str` (`Type::Str | Type::StrLit(_)`), and a `StrLit` lowers to an owned `Str` temp. The only
genuinely new logic is at check-time.

**This REVERSES the avoidance of a new `Type` variant that ADR-044/049 chose.** ADR-049 explicitly
rejected "adding a dedicated `Type::StrLit` literal-type (which would touch ~20 exhaustive `Type`
matches across codegen/boxing/representation — too invasive)". That cost was paid here, but the
"`StrLit` = `Str` at runtime" mitigation made it mechanical: each of those ~20 sites does *exactly*
what the `Str` arm does, so there is no new representation, no new runtime primitive, and no new RC
class. The three RC classifiers that must stay in lockstep — `lin-ir::lower::is_rc_type`,
`lin-ir::rc_elide::is_rc_type`, and `lin-codegen::types::ty_is_concrete_rc` — all treat `StrLit`
as a refcounted string, and release routes through `string_release` (retain uses the tag-aware
`lin_rc_retain`, identical to `Str`). Validated under AddressSanitizer: a `String`-typed and a
`StrLit`-typed loop (1000 iterations each, build-and-discard) produce an *identical* leak profile
(4140 bytes / 3 allocations — the program-lifetime interned literal cache, leaked by design as
elsewhere), with **no** use-after-free, double-free, or refcount underflow. The §19 divide/Result
example runs and discriminates both branches cleanly under ASan too.

**Compat rules (`compat.rs`, after the `Shared`/`TypeVar(MAX)` arms, before numeric/union/object)**:
1. `(StrLit a, StrLit b) => a == b` — two singletons compatible iff equal.
2. `(StrLit, Str) => true` — a literal widens to the open `String` type.
3. `(Str, StrLit) => false` — load-bearing rejection: an arbitrary string is not statically known
   to equal the singleton, so `val t: Tag = someString` is an error.
4. The `Json`-sink arms are unchanged; an object with a (non-null) `StrLit` field is already treated
   as "structured" by `requires_structured_decode`, so a `Json` value still cannot be silently
   bound to a literal-discriminated object — it must go through `fromJson` or `is`/`has` narrowing.

**Bidirectional refinement (`checker/expr.rs`)**: a bare string-literal *value* still infers to
`String` (`infer_expr` is unchanged — §25). Narrowing happens only in `check_expr` against an
expected type: (a) `Expr::StringLit` against an expected `StrLit("t")` is accepted iff equal and
yields a `StrLit("t")`-typed node (`TypedExpr::StringLit` gained a `Type` field, normally `Str`);
(b) `Expr::Object` against an expected object/union/named type pushes the expected field types down
per-field, and for a union *selects the variant by matching the discriminant literal*, erroring with
the list of valid tags if none match. To make this reach the §19 `divide` body — an
`if/then/else` returning object literals — the expected return type is now pushed into the function
body (and through `if`/block tail positions), but **only when the declared return type mentions a
`StrLit`**, so all other inference and error messages (e.g. "Function body has type …") are
unchanged. This mirrors the existing array-literal refinement pattern.

**`fromJson` validates the exact literal value (ADR-049).** A `StrLit` field encodes as a
`KIND_STRLIT` descriptor node carrying the expected bytes; the runtime interpreter (`lin_from_json`)
checks the JSON value is a string AND equals the singleton, reporting e.g.
`expected "alpha" at $.kind, got "beta"`. This makes `Result.fromJson(...)` reject a wrong
discriminant tag, so the union's first-match-wins probe discriminates variants by their literal tag
(a `{ "type": "failure", ... }` value fails the `"success"` variant's `KIND_STRLIT` check and falls
through to the `"failure"` variant). Superseded the original v1 `KIND_STRING` placeholder.

**Limitations / scope (deliberate, v1)**:
- **`Json → StrLit` stays permissive (unchecked) in user code.** A `Json` value IS assignable to a
  `StrLit` target (`requires_structured_decode(StrLit)` is false — a literal is scalar-like), exactly
  as `Json → Int32` is unchecked (ADR-048 scalar gap). `fromJson` is the validated path. Tightening
  this would diverge from the scalar-gap policy, so it is left consistent by design.
- **Exhaustiveness (Step F) was NOT implemented.** Recognising a literal-discriminated `has`/`is`
  arm as covering a specific union variant is *not* done. This is safe: the existing exhaustiveness
  checker already requires an `else` (or a covering arm) for *any* object-union `has`-match — literal
  or not — and emits a diagnostic when absent; that behaviour is unchanged and consistent. Adding
  partial literal-coverage recognition risked inconsistency for marginal benefit, so it was skipped
  (the §19 examples use `else`, which always satisfies exhaustiveness).
- **Numeric and boolean literal types are out of scope** — only string literals are singletons.

**Consequence**: `lin build`/`lin run` of a wrong-tag object now reports e.g. *"Object does not
match any variant of …; expected a discriminant tag in [\"failure\", \"success\"]"* at the object's
span; a `String → literal` assignment reports *"Expected type \"ok\", got String"*. Literal tags
survive generic substitution (`Result<Int32, String>` and `Result<String, Int32>` both
discriminate), since `substitute` passes `StrLit` through its `_ => ty.clone()` tail. As with
`lin-check` generally, a single-file `lin check` leaves an imported function's return type as a
fresh inference var, so the strictest checking is via the full `lin build` pipeline. Verified: full
suite green (288 integration + 6 + 33 + 7; stdlib 19 files; examples 22 files), plus the new
`examples/result/main.lin` fixture.

## ADR-053: Imported types usable in type position

**Decision**: An `export type Foo = ...` declaration can be imported (`import { Foo } from "m"`,
including `Foo as Bar` aliases) and used in a type annotation in the importing module. Spec §22.3
always promised this ("Types may be imported with the same syntax"), but it was never implemented:
type exports were dropped at the module boundary, so a use-site hit *"Unknown type 'Foo'"*.

**Why it was missing**: a `type` decl produces no runtime code, so the checker resolved it into its
local `TypeEnv::type_decls` and then returned `TypedStmt::Expr(NullLit)` — it left no trace in the
`TypedModule`. `ModuleSignature::from_module` only scanned `TypedStmt::Val`, so the dependent
module's checker (which is seeded from the signature via the `import_types` map) never learned the
name. Value imports worked; type imports silently didn't.

**Mechanism**: mirror the value path one level up, as *module metadata* rather than a statement.
- `TypedModule` gains `exported_types: HashMap<String, (Vec<String>, Type)>` (params + resolved
  body), populated in `check_module` from each `export type` via `env.lookup_type` — alongside the
  existing `intrinsics` metadata map. It is **not** a `TypedStmt`, so `lin-ir` lowering, codegen,
  liveness, and rc_elide are entirely unaffected (they never see it).
- `ModuleSignature` gains `type_exports` (copied from `exported_types`), so dependents that only
  load the `.sig` still get types. Both new fields are `#[serde(default)]`, so stale `.typed`/`.sig`
  caches deserialize as empty and trigger a graceful re-check rather than an error.
- `Checker` gains an `import_type_decls: (module, name) -> (params, body)` input (the type-level
  analogue of `import_types`). `lin-compile` populates it from each import's `type_exports`.
- A new `register_imported_types` pre-pass (run before `forward_declare_types`, since
  forward-declared signatures may annotate with imported types) walks the `Import` stmts and, for
  each binding matching `import_type_decls`, calls `env.define_type(local_name, params, body)`
  honouring `as` aliases.

The body stored is the **fully resolved** type (Named cycle points preserved), so no cross-module
type-env lookup is needed at the use site — it resolves like a local `type` alias. Generic exported
types work because the stored `params` flow through the same `substitute` path as local generics.

**Scope/limits**: registration is scoped to what is imported — referencing `Foo` without importing
it is still *"Unknown type"* (verified by `test_imported_type_unknown_without_import`). Verified:
the web-server example now does `import { HttpRequest as Request, HttpResponse as Response } from
"std/http"` (its local `Request`/`Response` aliases deleted); full suite green; imported types
round-trip through both cold and warm module caches.

## ADR-054: `is <ObjectType>` deep type validation

**Decision**: `x is <Name>` (where `Name` resolves to a non-empty object type, e.g.
`Person = { "name": String, "age": Int32 }`) validates field **types recursively** at runtime, not
merely that the fields are present. The match succeeds only when `x` genuinely conforms to
the target type — so the binding narrowing the matched arm performs is **sound**. The deep walk is
the *same* operation as `fromJson`'s structural validator (ADR-049): it is **reused, not
duplicated**. A new runtime entry point `lin_matches_schema(value, descriptor) -> u8` runs the
existing `validate` walker (`lin-runtime/src/decode.rs`) and returns a bool (`{ let d = Desc { base:
desc }; let mut p = String::new(); validate(value, &d, 0, &mut p).is_ok() as u8 }`); the mismatch
error string is discarded on the cold path. This inherits fromJson's number policy verbatim
(`KIND_INT` integral + width/sign range, `KIND_FLOAT` any number, `KIND_STRLIT` exact value,
`KIND_OBJECT` recursive, `KIND_ARRAY`/`KIND_FIXED`/`KIND_UNION` as in ADR-049) — exactly the
consistent, sound semantics wanted.

**Wiring** (mirrors `Intrinsic::FromJson { target, named_defs }`):
- **Checker** (`checker/pattern.rs`): `check_pattern` for `Pattern::TypeName` whose resolved type is
  a non-empty `Type::Object` now produces a **new typed-pattern variant**
  `TypedPattern::TypeCheckDeep(Type, Vec<(String, Type)>, Span)` carrying the object type plus the
  resolved bodies of every reachable `Named` type (via the existing `collect_named_defs` helper,
  promoted to `pub(crate)`) so IR lowering — which has no type environment — can build the
  (possibly recursive) schema descriptor. Plain primitives, unions, and non-object named types keep
  the bare `TypeCheck` (and its `IsType` tag check); `is Error` keeps its value-constrained object
  pattern (`error_discriminant_pattern`); empty object types `{}` keep `TypeCheck` (bare tag check).
- **A new variant, not a field on `TypeCheck`, was chosen.** `TypeCheck(Type, Span)` is matched in
  ~8 places; adding a field touches all of them invasively. The variant localizes the change: the
  sites that needed updating just treat `TypeCheckDeep` like `TypeCheck` — narrowing
  (`checker/pattern.rs`, narrow to the carried `Type`), zonking (`zonk.rs`, zonk the `Type` and the
  `named_defs` bodies), exhaustiveness (`exhaustiveness.rs`, count it as covering its variant),
  and the IR pattern helpers `pattern_type_check` / `pattern_elem_type` / the no-binding arm in
  `lower.rs`. Only the two `is <ObjectType>` emit sites diverge.
- **IR** (`lin-ir/src/ir.rs`, `lower.rs`, `liveness.rs`): a new instruction `MatchesSchema { dst,
  val, target, named_defs }` (payload mirrors `FromJson`). The two former `is <ObjectType>` sites —
  the standalone `TypedExpr::Is` arm and `lower_match_pattern`'s
  `Is(TypeCheckDeep(..))` — now emit `MatchesSchema` instead of `HasPattern`. `val` is the
  already-boxed-to-Json scrutinee, exactly as the `HasPattern` path boxed it (`box_to_json`);
  liveness treats it as `(uses val, defs dst)`.
- **Codegen** (`codegen/match.rs`, `mod.rs`): `MatchesSchema` reuses `emit_from_json_descriptor`
  (promoted to `pub(crate)`) to emit the same static descriptor global the `FromJson` path builds,
  then calls `lin_matches_schema(val, desc)` and truncates the returned `i8` to a bool. Branchless,
  single basic block — composes inside match-arm test blocks like `compile_ir_has_pattern`.

**RC/memory**: `lin_matches_schema` **borrows** `value` (no clone, no release) and reads a static
const descriptor — no ownership change, low risk. The input value temp's ownership is unchanged from
the `HasPattern` path; the `box_to_json` boxing is still done before `MatchesSchema`.

**Unchanged behavior** (verified): `has { .. }` and inline `is { .. }` object patterns stay
presence + value-constraint (`lower_object_pattern_test`); `is Error` stays the value-constrained
object pattern and still discriminates a decode failure from a decoded value in either arm order;
empty object types `{}` keep the bare tag check; all `fromJson`, async/await `Error`, and
literal-type/union tests stay green.

**Consequence**: `is <ObjectType>` narrowing is now sound — the runtime enforces what the type
narrowing claims. Recursive types (`type Tree = { "value": Int32, "children": Tree[] }`) terminate
because `collect_named_defs` is bounded by a `seen` set and the descriptor encoder memoises Named
nodes as finite back-edges. Number policy is consistent with `fromJson`: `is { "n": Int32 }` rejects
`3.14` (non-integral) and accepts `5.0` (integral). Verified end-to-end via `lin build`: deep
rejection of a wrong (incl. nested) field type, a valid value matching with sound narrowed field
access (`v["age"] + 1` = correct number), `is Error` still discriminating, and recursive `Tree`
validation. Full suite green (302 integration + 6 + 33 + 29 + 7; 42 stdlib/examples test files).

**Supersedes/absorbs ADR-050.** ADR-050 first made `is <ObjectType>` check required-field
**presence** (closing the unsoundness where `is Person` matched *any* object — including a `fromJson`
decode error or `{ "foo": "bar" }` — and then narrowed the binding to `Person`, so a subsequent
`x["name"]` could null-deref). It was explicitly presence-only: `is Person` on `{ "name": 1, "age":
"x" }` (keys present, wrong types) still matched, deferring field-*type* validation. This ADR closes
that residual gap — a present-but-wrong-typed field now fails too (so `x["age"] + 1` never operates
on a string), while a missing field still fails via the `KIND_OBJECT` walk. Both the match-arm and
standalone-expression forms go through `MatchesSchema`; width subtyping (extra fields) is preserved.

## ADR-055: `std/proc` consolidated into `std/process` (batch + streaming)

**Decision**: There were two documented subprocess modules — a working low-level `std/proc`
(`spawn(argv)` → `Int64`, `readStdout`, `wait` → exit code) and an unimplemented high-level
`std/process` (`exec`/`shell`/`cwd`/`chdir`, `spawn(command,args)`, `wait` → `ExecResult`). They
are merged into a single `std/process` exposing **both** styles:
- **Batch**: `exec(command, args)` / `shell(command)` run to completion and return an
  `ExecResult { status, stdout, stderr }`; `cwd()` / `chdir(path)`.
- **Streaming**: `spawn(command, args)` → opaque `ProcessHandle` (`Int64`); `readStdout(handle, buf)`
  reads the piped stdout incrementally; `kill`; `wait` → exit code (`Int32`).

**Why both, and why `wait` stays exit-code**: streaming and batch don't compose on one handle —
`readStdout` drains the pipe, so a `wait` that returned full stdout (as the doc'd `std/process.wait`
implied) would come back empty. Rather than maintain two registries or silently break streaming, the
batch path (`exec`/`shell`) owns "collect all output", and the streaming `wait` returns just the exit
code. `spawn`/`exec` both take `(command, args)` (the doc'd `std/process` shape); the old argv-array
`spawn(["sh", ...])` form is gone.

**Mechanism**: runtime `crates/lin-runtime/src/proc.rs` → `process.rs`, intrinsics renamed
`lin_proc_*` → `lin_process_*`, adding `lin_process_{exec,shell,cwd,chdir}` (the batch fns build an
`ExecResult` object / run `Command::output()`). Streaming fns keep the monotonic-id `Child` registry
unchanged. `stdlib/proc.lin` → `process.lin`, embedded as `"std/process"` in `lin-compile`. The
stdlib wrappers dogfood ADR-053 (imported types): `ExecResult` is an exported record type and
`ProcessHandle` an exported `Int64` alias, used in the wrapper signatures.

**RC/memory**: `make_exec_result` follows the leak-clean object-build pattern (`fs::make_decode_error`)
— `lin_object_set` retains the key and the value's inner string, so the local `+1` from each
`make_string` is released afterward; the object becomes sole owner and freeing the returned box frees
everything. (The older `make_response_object`/`make_error_obj` skip this; they are program-lifetime
singletons so it never mattered, but `exec` is called repeatedly.) Verified leak-free under
LeakSanitizer (the only residual reports are pre-existing program-lifetime interned string literals).

**Migration**: the lone consumer (`examples/processes`) and the proc tests were updated to the new
`spawn(command, args)` form and `std/process` import. Verified: stdlib + example suites green; three
integration tests (`test_process_spawn_read_wait`, `test_process_wait_exit_code`,
`test_process_exec_and_shell_batch`) pass.

## ADR-056: OS resources are opaque integer fd handles

**Decision**: Operating-system resources exposed by `std/net`, `std/process`, and `std/time`
(timers) are represented to Lin code as **opaque integer handles**, never as runtime object values. A
socket fd is an `Int32` (`udpBind`/`tcpListen`/`tcpAccept`/`tcpConnect` return `Int32 | Error`); a
subprocess handle is an `Int64` (`std/process.spawn` returns a `ProcessHandle`, an exported `Int64`
alias — ADR-055). The integer is meaningful only to the runtime, which keeps the real `fd`/`Child` in
a side table keyed by the integer; user code passes it back to the relevant intrinsic
(`udpRecv(fd, …)`, `wait(handle)`, …). See spec §27.4–§27.6.

**Rationale**: This upholds the §25.1 "no hidden open-handle values" convention already used for
stdin/stdout/filesystem — there is no `Socket` or `Process` object kind to add to the runtime, no new
boxing, and no lifetime/RC story for OS handles. An integer is transferable, comparable, and trivially
representable. Fallible operations return the `T | Error` shape (§25.1); a non-blocking read with no
data yet returns `Null` (not `Error`), so poll loops read naturally (`recv`/`accept` →
`Int32 | Null | Error`).

**Consequence**: No new runtime *kind* is introduced for sockets/processes — they reuse the existing
integer representation (a typed alias like `ProcessHandle` is just `Int64` for readability, not a
distinct kind). The cost is that handles are not type-distinct from ordinary integers; misuse (passing
the wrong integer) is not caught by the type system, consistent with the deliberately-unsafe nature of
the low-level layer. The `Int32`/`Int64` split is pragmatic: fds fit in 32 bits, process handles use 64
to match the platform child representation.

## ADR-057: Share-nothing concurrency — no Mutex/atomics primitive

**Decision**: Lin provides **no** shared-memory concurrency primitives (mutexes, atomics,
cross-thread shared mutable cells). Cross-thread mutable state is modelled exclusively with a
`Worker<Msg, Reply>` (§24.6) that owns the state and serialises all access through its single-threaded
message queue. Spec §27.10 records this as a deliberate absence.

**Rationale**: The concurrency model is share-nothing (§24): `async` thunks and `parallel` may not
capture `var` bindings (compile-time error, ADR-034), and transferred values must be JSON-compatible. A
`Worker` owning its state and processing messages one at a time preserves that invariant — there is no
concurrent access to the worker's closed-over `var`, so no data race is possible without ever
introducing a lock. Adding a `Mutex`/atomic primitive would reintroduce exactly the data-race surface
the model is designed to exclude, and would need a new opaque runtime kind plus a poisoning/lifetime
story.

**Consequence**: Patterns that would use `Arc<Mutex<T>>` in Rust (a shared counter, a connection pool,
a discovered-peer-address cache) are expressed as a `Worker` whose `onMessage` handler closes over the
state (§24.6.4). This is more message-passing boilerplate than a shared cell, accepted as the price of
guaranteed freedom from data races. Single-threaded mutation via `var` is unaffected; the restriction
applies only across thread boundaries.

## ADR-058: Flat unboxed arrays for scalar element types

**Decision**: An array whose element type is a fixed-width scalar — `Int8`/`Int16`/`Int32`/`Int64`,
`UInt8`/`UInt16`/`UInt32`/`UInt64`, `Float32`/`Float64` — is stored as a **packed, unboxed,
contiguous buffer** (one element-width slot per element, no per-element tag), not as an array of boxed
`TaggedVal`s. The runtime provides a flat variant per family
(`lin_flat_array_alloc_{i8,i16,i32,i64,u8,u16,u32,u64,f32,f64}` and matching push/index). A `UInt8[]`
is therefore a literal byte buffer (spec §27.1). Semantically these remain ordinary `T[]` arrays —
every array operation (literals, indexing, in-place write, `length`, `push`, `slice`, `concat`, `==`,
the `std/array` combinators) works identically; the representation is an implementation detail.

**Rationale**: Byte/scalar buffers are the substrate for binary protocols, `std/bytes`, and socket
I/O (`recv` fills a caller-owned `UInt8[]`). Boxing each byte as a tagged value would cost ~16× the
memory and defeat the point. Because the element type is statically known, codegen selects the flat
representation with no runtime tag dispatch. Mixed/`Json`/object arrays keep the boxed tagged
representation; only statically-scalar element types go flat.

**Consequence**: Codegen and the runtime must convert between flat and tagged forms at boundaries where
a flat array meets a `Json`/dynamic context (`lin_flat_to_tagged_*`, used e.g. by `toString` and
dynamic length). The flat/boxed distinction is why `concat`/`slice` are flat-representation-aware:
slicing/concatenating a `UInt8[]` yields a `UInt8[]` whose elements read correctly, not a boxed array
of zeros. `is_flat_scalar` (codegen) is the single predicate deciding the representation and must stay
consistent with the runtime's family set.

## ADR-060: Closures OWN their captures (retain on capture, release on free)

**Decision**: A closure's environment now OWNS one reference per heap/union capture — the same ownership rule arrays and objects already follow for stored elements. At `MakeClosure` the lowerer takes ownership of each capturing value (concrete rc → `Retain` in place; union/`Json` → `CloneBox` so the env holds its own `TaggedVal*`); `lin_closure_release` releases them when the closure is freed. Mutably-captured `var` bindings are unchanged: they store the heap **cell pointer** (shared by reference, ADR-015) and keep their existing borrow-only / `FreeCell` / escaping-cell lifecycle — the env does not own the cell. Scalars need no ownership.

To let the runtime release captures, every closure carries a **capture descriptor** at closure offset 40 (`{ u32 count, u8 kinds[count] }`, a static read-only global): one `CaptureRelease` byte per capture (None / Str / Array / Object / Closure / Tagged). The closure struct grew from 40 to 48 bytes (`CLOSURE_SIZE`); `alloc_closure`/`store_capture_descriptor` centralise the layout, and `lin_closure_release` frees 48 and walks the descriptor (mirroring the recursive element release in `lin_array_release`/`lin_object_release`). Partial-application closures keep borrow semantics with a null capture descriptor. The async thread-transfer path (ADR-042) reads the same descriptor (passed explicitly from the closure, no longer stored at env offset 0) and reuses its codes.

**Rationale**: Captures were **borrow-only** — the env stored a borrowed pointer with no retain, and `lin_closure_release` freed nothing. That is sound only while a closure cannot outlive its captured values' scope. The `safe_callback_depth`/escaping-cell analysis covered that for mutable `var` cells, but immutable value captures had no ownership at all, so a closure that ESCAPES (e.g. returned from a `map`/`filter` callback into the result array) dangled: `map(xs, i => () => i)` then called a thunk returned garbage (`[[object]…]`) because the captured element box had been freed at end-of-iteration and its memory reused. Making the env an owning container, exactly like arrays/objects, fixes this uniformly and reuses the existing store-side discipline (`transfer_into_container` / `own_for_store`).

**Consequence**: `map(xs, i => () => i)` and any escaping capturing closure are now correct. **Performance is unchanged**: the added retain/release pairs are elided by the existing RC-elision pass for the overwhelmingly common non-escaping combinator-callback case (closures created and consumed in one scope) — a before/after benchmark (`benchmarks/closures.lin`, capturing `map`/`filter`/`reduce` over 2M elements) showed no measurable difference, and the full benchmark suite was flat. The cost lands only on closures that genuinely escape, where it is required for correctness. The closure struct is 8 bytes larger (40→48); `CLOSURE_SIZE` in `lin-codegen/src/codegen/call.rs` and the free size in `lin-runtime/src/memory.rs` must stay in lockstep. Verified under ASan (stdlib + every example-project test suite — heavy closure users — show no use-after-free or double-free; the only leak is the pre-existing exit-time top-level-`val` leak, identical for non-capturing `map`).

## ADR-061: Numeric literal suffixes honoured; large bare literals widen, never truncate

**Decision**: Two related fixes to integer-literal typing, both making the implementation match spec §2.6/§21:
1. **Type suffixes are honoured.** `42i8`, `5u64`, `3.14f32` etc. now pin the literal's type, overriding context/default. Previously the lexer recognised the suffix characters but *discarded* them ("we just consume them"), so `1705314600000i64` was an indistinguishable bare `IntLit` that defaulted to `Int32` and **silently truncated** to its low 32 bits (`212583488`).
2. **A bare literal beyond `Int32` widens its default instead of truncating.** With no surrounding context, a suffixless integer literal still defaults to `Int32` when it fits; when it exceeds `Int32`'s range it defaults to the smallest type that *preserves* the value (`Int64`, or `UInt64` for a decimal above `i64::MAX`). It is never silently truncated.

**Why widen rather than error.** An earlier attempt made an out-of-range bare literal a compile error ("annotate or suffix it"). That broke ergonomic, previously-working code: call arguments are inferred context-free first and *then* re-typed to the parameter width (`checker/call.rs`), so `format(1705314600000, …)` (an `Int64` param) legitimately relies on the literal surviving inference. Widening the default preserves the value for that downstream re-typing while still fixing the truncation; a genuinely-too-big-for-any-type case can't arise (the lexer already maps `> i64::MAX` decimals to the `UInt64` bit pattern). A literal assigned where an *incompatible* concrete type is required (e.g. `val x: Int32 = 5i64`, or `[256]: UInt8[]`) is still a hard error — that path was already range-checked and is unchanged.

**Mechanism**: a shared `NumSuffix` enum in `lin-common` is parsed by the lexer and carried on `TokenKind::IntLit`/`FloatLit` → surface `Expr::IntLit`/`FloatLit` (the typed IR already carries a resolved `Type`, so the suffix stops at the checker). `checker/helpers.rs` gains `suffix_to_type`, `default_int_literal_type` (the Int32→Int64→UInt64 widening ladder), and `check_int_literal_fits` (extracted from the old inline range check). `check_expr` keeps context-typing for suffixless literals; a suffixed literal flows through `infer_expr` (typed at its suffix type) and the normal compatibility tail validates it against the expected type. The formatter round-trips suffixes.

**Consequence**: `1705314600000i64` and `val ts: Int64 = 1705314600000` and a bare `1705314600000` all preserve the value; `val x: Int32 = 5i64` is a type error; small suffixed literals (`200u8`) type at their width. Covered by `stdlib/number.test.lin` (suffix preservation + arithmetic round-trip) and integration tests (`test_i64_suffix_preserves_large_literal`, `test_int64_annotation_preserves_large_literal`, `test_bare_literal_overflowing_int32_preserved`, `test_suffix_overrides_expected_context_conflict`). Full suite green; surfaced and fixed during the `std/time` work, where `format(<ms>, …)` first exposed the truncation.

## ADR-063: Cross-module generic instantiation materializes in the IMPORTING module

**Decision**: A generic `val` function (`<T>(x: T): T => x`) defined in an IMPORTED module is monomorphized in the **importing** module's lowering, not the defining one. `lin-compile` threads the already-typed `imported_modules` map into `lower_module_with_imports` → `monomorphize_with_imports`. The pass discovers a call to an imported generic (the importer's `ImportSlot.ty` is a `Function` with a generic TypeVar in its **parameters** — a return-only TypeVar, as on stdlib intrinsic wrappers like `iter: (…) => Iterator<T>`, does NOT count), clones the generic body out of the imported `TypedModule`, substitutes its quantified TypeVars at the concrete call instantiation, **re-homes** the body into the importer, and emits it as a local specialization (`id$Int32`) that the call is rerouted to.

Re-homing (`rehome_imported_body`) rewrites the cloned body's slots: every body-local slot (params, inner `val`/`var`, destructure targets) is remapped to a fresh importer slot (so it can't collide with the importer's own slots or another specialization), and every FREE reference to the origin module's scope is rewritten into an importer-side construct the importer's lowering already resolves — a sibling function/val → a synthesised `TypedStmt::Import` (a `Named` call to `{origin_key}_{name}`), an intrinsic → a merged intrinsic slot, a **thin intrinsic wrapper** (`for = (it,f) => lin_for(it,f)`) → the intrinsic itself (inlined, preserving the polymorphic builtin's concrete-element dispatch), a foreign binding → a `ForeignImport`. An import-of-import resolves to the SOURCE module's symbol, never the intermediate's. Imported modules also monomorphize their OWN sibling generic calls during `lower_import_module` (`monomorphize_import`), keeping ALL generic originals so external importers that don't specialize still resolve the boxed `{module_key}_{name}` symbol.

Two supporting fixes: (1) `subst_expr` now substitutes the declared-type field of statements inside a block (`subst_stmt_types`) — a `var acc: U` in a generic body otherwise kept `ty: TypeVar(U)`, producing a boxed-union cell while the substituted closure that captures it read the concrete type (a misaligned-pointer crash). (2) the checker's `infer_function_with_hints` now surfaces a lambda's CONCRETE body type when the expected return is a quantified generic param (id ≥ 9001), so a higher-order generic call (`mymap(arr, x => x*2)` with `f: (T) => U`) can bind `U` from the lambda body; the bare-TypeVar boxing convention is retained for the Json/`Function` polymorphic-slot case.

**Rationale**: The importer has the full `TypedModule` for every import (not just the signature), so it can see imported generic BODIES — the only place with both the body and the concrete call types. Specializing there avoids touching the imported module's compilation/caching and avoids cross-contamination (each importer derives its own specializations from the cached generic body; the `.lin-cache` stores the TypedModule with the generic body intact, keyed by source hash regardless of how importers instantiate). The no-op invariant is preserved: `module_uses_generic` gates the whole pass, so a module that neither defines nor imports a param-generic function lowers byte-for-byte as before (verified: `benchmarks/array_pipeline.lin` IR is byte-identical to baseline).

**Consequence**: User-defined cross-module generics — including higher-order ones with the `map` shape (`<T,U>(arr: T[], f: (T) => U)`) — specialize to native, unboxed code in the importer (e.g. `id$Int32` is `define i32 @"id$Int32"(i32)`). Verified end-to-end (output + IR proof + ASan, no UAF/leak) and across the cache (two importers using one imported generic at different element types each get correct specializations). **Converting stdlib `map`/`filter`/`reduce` to generic was attempted and DEFERRED at the time**: the specialized bodies are themselves nearly box-free, but (a) the result of a `[]`-plus-`push` build is a *tagged* array while a static `U[]` result type makes consumers read via the *flat* ABI — a representation mismatch — and (b) the per-element boxing at the `lin_for` callback boundary remains (the closure ABI passes a `TaggedVal*`), so the static box count rose and ~20 of the diverse stdlib/example uses regressed. The full flat/zero-box pipeline needed closure-callback ABI specialization — which later landed in ADR-069. stdlib `array.lin` was therefore left `Json`-typed here; the cross-module infrastructure is in place for when that lands.

## ADR-064: `concat` retains copied elements (move-vs-retain split in array element copy)

**Decision**: When `lin_array_concat_dyn` copies elements from a **borrowed** source array (the
tagged-element path, `elem_tag == 0xFF`), it now **retains** each element's heap payload via the new
`lin_array_concat_into_retaining`, so the result array and the source array are independent owners.
The non-retaining `lin_array_concat_into` (a raw 16-byte `TaggedVal` move, no retain) is **kept** and
used only where the source is a fresh temp whose ownership is transferred — `concat_dyn`'s
widened-flat path, where `lin_flat_to_tagged_*` boxes raw scalars at `+1` and the temp array is then
`lin_array_free`d (which frees only the struct + buffer, never the element payloads, so the boxes are
correctly *moved* into the result).

**Rationale**: The old `concat_dyn` used the move-copy (`lin_array_push_tagged`, raw 16-byte copy
without retain) for **every** path, including borrowed sources. So `concat(a, b)` left `a`/`b` and the
result sharing each element at one refcount; releasing any of them (e.g. `acc = concat(acc, […])` in a
loop, which frees the old `acc`) freed the shared payload out from under the result — a genuine
use-after-free, ASan-confirmed (`heap-use-after-free in lin_string_release` on pristine master). It was
masked in practice only because string *literals* are interned with immortal refcounts; `concat` of
computed strings/objects corrupted the heap. `lin_array_push_tagged` itself MUST stay non-retaining —
its other callers (`io`/`fs`/`json`/`async_rt`/`frozen` building arrays from freshly-owned values, and
the `map`/`minBy`/`maxBy` element-move convention noted in `lower.rs`) deliberately rely on the move
to transfer ownership; adding a retain there would leak. The fix therefore lives in a *separate
retaining copy primitive* selected per-source-ownership inside `concat_dyn`, not in the shared push.

**Consequence**: `concat` of fresh (non-interned) heap values is now memory-safe — the growing-concat
loop runs clean under ASan (only the pre-existing program-lifetime interned-string-cache leak remains;
a scoped concat-of-fresh-strings test leaks nothing of its own). The move-vs-retain split is the
load-bearing invariant: a future change must keep "copy from a still-live borrowed array → retain;
copy from a fresh temp being freed → move". Regression: `test_concat_fresh_strings_no_use_after_free`
(40-iteration growing concat of interpolated strings). The analogous
move-without-retain residual leaks in the `for`/`map` element-shell path (`lower.rs`) are a distinct,
pre-existing issue and are not addressed here.

**Supersedes/absorbs ADR-062.** `std/array`'s `append`/`prepend`/`groupBy` are backed by RC-self-contained runtime intrinsics rather than pure-Lin loops: `lin_array_append_dyn`/`lin_array_prepend_dyn` (`array.rs`) and `lin_object_get_or_insert_array` (`object.rs`), all ordinary `import foreign "lin-runtime"` symbols (ADR-009). `append`/`prepend` **preserve the input's element representation** (ADR-058) — a flat scalar source yields a flat result (coercing the item via `lin_push_dyn` + bulk `concat_into`), a tagged/`Json[]` source yields a tagged result — fixing a latent bug where the old Lin loop allocated a *tagged* array and silently boxed flat bytes. They are hand-rolled (not composed on `concat_dyn`) because `concat_dyn`'s tagged path copies element pointers *without* retaining; the intrinsics instead copy through `lin_push_dyn` (which retains) and retain the item, so the result owns its own +1 per heap element and releases independently. `get_or_insert` does a single hash lookup, retaining the interior group array (or allocating+inserting it) and returning an owned box that `push` mutates in place — making `groupBy` one lookup+push per item. By the same move-vs-retain reasoning, this is the discipline `concat` was brought into line with here.

## ADR-067: Only direct-index array accessors (`at`/`set`/`indexOf`) are genericized to `<T>(T[], …)`; allocating/builder/dyn-fn/numeric/iterator combinators stay `Json`

**Status:** accepted (Phase 6).

**Context.** Phase 6 set out to convert `stdlib/array.lin` signatures from `Json` to generic `<T>`
so concrete-element callers (`Int32[]`, `String[]`) get type-safety and, where the body is an
alloc-builder, an unboxed flat representation. The prerequisites (flat-write for alloc-builder bodies
and `T=Json` binding + safe import-path TypeVar erasure, both since folded into ADR-069) are in place.
The governing invariant is **representation consistency**: a value produced as flat (unboxed scalar
buffer) must be read flat, and a value produced as tagged (16-byte `TaggedVal` elements) must be read
tagged. A generic `T[]` return type makes a *concrete-scalar* consumer read the result via the flat
accessor; if the body actually produced a boxed/tagged value, the consumer reads garbage and/or the
runtime corrupts the heap (`array.rs` layout assertions, `ZExt` codegen on a pointer).

**What converted (verified correct on `Int32[]`, `String[]`, heterogeneous `Json[]`, ASan-clean):**
- `at`  → `<T>(arr: T[], index: Int32): T`
- `set` → `<T>(arr: T[], idx: Int32, item: T): Null`
- `indexOf` → `<T>(arr: T[], target: T): Int32`

These are **direct-index accessors**: they read/write a single element through bracket
indexing (`arr[i]` / `arr[i] = item`), which is already element-type-aware in both directions, and
they do **not** allocate a new array nor route the element through an opaque `for`/closure callback
or `push`. The element type flows straight between the typed param/return slot and the tag-aware
index path, so flat and tagged inputs both stay consistent. Purely type-safety wins — no new
allocation, no representation change, so no perf delta and the hot path of `array_pipeline`
(`map`/`filter`/`reduce`, all still `Json` at the time) is functionally unchanged (min-of-9 ≈ 550 ms
before and after; output `1892804906`).

**What stayed `Json` (at the time; converting each regressed, with the reason):**
- `map` — alloc-builder, but `result[i] = f(item)` writes the result of an **opaque closure call**,
  which arrives boxed. A flat `U[]` result would write/read it via the unboxed path → garbage
  (`[1,2,3].map(x => x*2)` printed `-1349553662`). Needs the unboxed closure ABI (later landed in ADR-069).
- `push` — the non-union `Push` codegen assumes the tagged layout; a generic `push` to a
  concrete-scalar flat array failed codegen (`ZExt only operates on integer`) and broke every test.
- `slice` / `concat` / `append` / `prepend` — backed by `lin_array_*_dyn` runtime fns that preserve
  the runtime element representation, but a `T[]`-typed monomorph reader for **sub-byte-width flat
  element types** (`UInt8[]`) reads inconsistently: genericizing `slice` regressed the byte
  consumers (`bytes`, `codec/tlv`, `raspberry-controller/rtp`+`nal`). Left `Json`.
- `filter` / `reduce` / `find` / `flatMap` / `partition` / `unique` / `compact` / `take` / `drop` /
  `takeWhile` / `dropWhile` / `zip` / `reverse` / `chunk` / `scan` / `groupBy` / `countBy` —
  `[]`+`push` builders, or alloc-builders that copy the element **through an opaque `for` callback**
  (so the element arrives boxed). Same closure-ABI gap as `map`; later resolved for `map`/`filter`/
  `reduce` in ADR-069. Left `Json` at the time.
- `sum` / `product` / `min` / `max` / `minBy` / `maxBy` — numeric reductions using `+`/`*`/`<`; a
  `<T>` body needs a `<T: Numeric>` constraint Lin lacks, so the body would not type-check. Left
  `Json[]`.
- `for` / `while` / `iter` / `iterOf` / `range` / `rangeStep` — operate on iterables
  (`Iterator<T>`), not just `T[]`; constraining to `T[]` breaks iterator callers. Left `Json`.
- `object.lin` `fromEntries` / `mapValues` / `isEmpty` — object-centric (and `length` is used on
  objects too, so it must stay `Json`); no clean `T[]` win. Left untouched.

**Behaviour note.** `at`/`set`/`indexOf` no longer accept a non-array `Json` (e.g. an `Iterator`)
directly — `range(0,10).at(5)` now reports `expected Int32[]`. No existing stdlib/example caller
relied on that (the only `at` caller, `calc/lexer`, uses `std/string`'s `at`; the only 2-arg `set`
caller uses `std/async`'s `set`). Wrap an iterator in an array (e.g. via `slice`) first.

**Consequences.** Type-safety for the three accessors; representation consistency preserved
everywhere. The flat-pipeline payoff for the builder/`for`-callback combinators (and ultimately `map`)
awaited the unboxed closure ABI, which landed in ADR-069. Regression: integration 351/0,
stdlib+examples 59/59, all example projects build, ASan-clean. Tests in `stdlib/array.test.lin`
(`at`/`set`/`indexOf` on `Int32[]`+`String[]`+round-trip).

## ADR-069: Generic `map`/`filter`/`reduce` + a capture-less-lambda inliner — the zero-per-element-box array pipeline (10x at -O2)

**Status.** Accepted. This is the shipped completion of the zero-per-element-box array pipeline.

**Context (the LINCHPIN goal).** The generics/perf milestone targets ZERO per-element boxing in a
monomorphic array pipeline `range(0,n).map(x=>x*2).filter(x=>x%3==0).reduce(0,(a,x)=>a+x)`. The
blocker is the UNIFORM ALL-PTR BOXED CLOSURE ABI: a closure's stored `fn_ptr` is a `__cls_wrapb_*`
wrapper `ptr(ptr env, ptr boxedArg…) -> ptr boxedRet`, so a combinator calling its callback via
`CallTarget::Indirect` always boxes each element and unboxes the result.

**What shipped.**
1. **Generic signatures** (`stdlib/array.lin`): `map<T,U>(arr:T[], f:(T)=>U): U[]`,
   `filter<T>(arr:T[], f:(T)=>Boolean): T[]`, `reduce<T,U>(arr:T[], init:U, f:(U,T)=>U): U`. At a
   monomorphic scalar call site the checker types the callback param at the concrete element type
   (no input boxing) and the monomorphizer picks the flat (e.g. `$Int32`) specialization.
2. **Call-site combinator-wrapper inline** (`lin-ir/monomorphize.rs`, `try_inline_combinator_wrapper`):
   when a call targets a generic thin intrinsic-combinator wrapper (`map`/`filter`/`reduce`, whose
   body is exactly `lin_map`/… forwarding its params) AND the callback argument is a CAPTURE-LESS
   LITERAL lambda, the call is rewritten in the calling module to a direct `lin_map(arr, <lambda>)`
   (re-homing the intrinsic slot), so the literal lambda is VISIBLE to the intrinsic's IR lowering.
   A capturing lambda or a stored/passed `Function` value is NOT inlined — it falls through to the
   normal closure-call specialization (still correct, just boxed).
3. **Capture-less-lambda inliner in `lower_map`/`lower_filter`/`lower_reduce`** (`lin-ir/lower.rs`,
   `inlinable_lambda` + `inline_lambda_body`): when the callback arg is a capture-less literal
   `Function`, its body is spliced directly into the loop — param bound to the flat element temp,
   body lowered inline — with NO closure alloc and NO per-element box/unbox/indirect call. `reduce`
   additionally carries a CONCRETE-SCALAR accumulator UNBOXED through the loop phi (gated on a scalar
   `result_type`; a union/heap accumulator keeps the boxed Json-phi path). RC: unboxed scalar
   elements/results carry no refcount, so there is no per-iteration box to release.

**The two regressions and how they were solved (correctness-first).**
  - **R1 (`examples/report`):** `validRecords()`/`parseErrors()` ended `…filter(…).map(r=>r["value"])`
    over a `Success | Failure` union; with precise generic `map` the element types as `Record | Null`
    (the union member access is nullable, `filter` does not narrow), not assignable to `Record[]`.
    SOLVED by restructuring the example to be well-typed under precise generics: a named helper
    `recordOf(r: Parsed): Record` (and `errorOf`) narrows the union via an idiomatic `match … has {
    "type": "success", value } => value; else => …` and the pipeline maps through it. (The narrowing
    lives in a named helper, not inline, because a multi-arm `match` can't be written inside the
    combinator's parentheses — indentation is suppressed there, ADR-004/014.)
  - **R2 (`sortBy`/`minBy`/`maxBy`/`compact`/`sum`/…):** these stdlib combinators call `map`/`reduce`/
    `filter` internally over `Json[]`; a SIBLING call to the now-generic combinator specialized at
    `$Json` cross-module, where the combinator's owned-array result and the surrounding generic body's
    RC accounting disagreed — a double-release of the intermediate array (capacity-overflow crash).
    SOLVED by keeping those internal callers on non-generic Json helpers (`_mapJ`/`_filterJ`/
    `_reduceJ`, thin wrappers over the same intrinsics). The generic exports still give the zero-box
    fast path at the user's monomorphic call site.

**Two supporting fixes (latent gaps the generic conversion exposed, both general correctness wins):**
  - **Cross-module generic specialization for IMPORTED modules** (`lower_import_module_with_imports`,
    `monomorphize_import_with_imports`): a module that imports a generic AND is itself imported (e.g.
    `examples/report` calling `std/array.reduce`) previously did not specialize its cross-module
    generic calls — they fell to the boxed type-erased original (returns `Json`), crashing a concrete
    scalar use site (`ret i32` vs `ptr`). The import path now monomorphizes with the program's imports
    map, exactly like the top-level importer.
  - **`repoint_call_native` re-coercion**: when a native specialization returns a CONCRETE scalar but
    the checker left the Call's `result_type` as the boxed/erased generic return (the `U` TypeVar
    surfaced as `Json`, e.g. `total = s` with `total: Json`), the Call is wrapped in a `Coerce {
    concrete → original }` so the boxed/unboxed handoff is explicit. Without it the consumer emits
    `store i32`/`ret i32` against a `ptr` slot (a hard codegen type mismatch).

**Result — the HONEST verified release number.** array_pipeline output 1892804906 (unchanged). The hot
map/filter/reduce loops are now FULLY UNBOXED in `main` — flat `lin_flat_array_get_i32`/`push_i32`, a
native `mul`/`srem`/`icmp`/`add`, and the reduce accumulator carried as a native `i32` through the loop
phi — ZERO `lin_box_int32`/`lin_unbox_int32`/closure-call per element. Interleaved RELEASE (`-O2`)
min-of-11, same machine, base vs after: **~328ms → ~33ms = ~10.0x** (verified twice: 10.06x, 9.95x).
This is a real `-O2` speedup, NOT a debug artifact — at `-O2` LLVM cannot elide the boxed closure ABI's
per-element malloc/indirect-call the way it elides cheaper boxing, so eliminating the closure call
itself is what unlocks the win.

**Correctness + safety.** stdlib+examples 59/59; integration 357/0 (isolated). map/filter/reduce
verified over `Int32[]` (flat), `String[]`/`Float64[]` (tagged/flat), `Json[]` (heterogeneous);
capturing lambda → closure path (correct); stored-fn-value callback → closure path (correct); chained
pipeline; non-scalar (array/string) reduce accumulator → boxed Json-phi path (correct). ASan-clean over
the full stdlib+examples leg + flat/tagged/capturing/mixed/sortBy/churn fixtures. No-op invariant: a
non-combinator program's MAIN module IR is BYTE-IDENTICAL to base (only `std/array` differs).

**R2 bug fix — `filter` over an object/heap array double-freed kept elements (the parked segfault).**
The original commit made `filter` produce a result array of the SOURCE's concrete element type. For a
CONCRETE-rc element (object/array/string — e.g. `std/test`'s `Assertion[]`, or any `Object[]`),
`filter`'s keep path pushes the element it READ from the source array via `Push`. For a concrete tagged
element that intrinsic lowers to `lin_array_push_tagged`, which raw-copies the 16-byte `TaggedVal`
WITHOUT bumping the inner refcount (MOVE — correct only for a freshly-owned value). But filter's element
is BORROWED (still owned by the source array), so source and filtered array both referenced the same
object at refcount 1, and releasing both double-freed it → heap-use-after-free (`lin_object_release`),
surfacing as the `examples/*/*.test.lin` segfault via `std/test`'s `results.filter(a =>
a["type"]=="fail")`. Fix: `push_output` takes a `borrowed` flag; `filter` passes `borrowed: true` and
emits a `Retain` first on a tagged concrete-rc push so the result owns its own reference; `map` (pushing
the lambda's freshly-owned result) passes `borrowed: false` and keeps the MOVE. Union elements
(retaining `lin_push_dyn`) and flat scalars (no refcount) need nothing, so the flat-scalar pipeline win
is untouched and the object-array inline win is RETAINED. Verified: stdlib+examples 59/59; integration
357/0; ASan-clean across every example-project test + a `filter`-over-`Object[]` fixture.

**Consequences.** The zero-per-element-box array pipeline ships at a verified ~10x `-O2` speedup.
Generic `map`/`filter`/`reduce` + capture-less-lambda inlining is the mechanism; the internal Json
helpers and the union-narrowing example rewrite keep every heterogeneous/union call site correct; the
import-path monomorphization + native-return re-coercion are general fixes for cross-module generic
calls. Capturing lambdas and stored-fn callbacks keep the (correct, boxed) closure path.

**Supersedes/absorbs ADR-065, ADR-066 and ADR-068** — the three staging steps of this saga, now
folded in as the shipped final state:

- **(from ADR-065) Flow-typing refinement pins generic combinator element types so alloc-builder bodies emit FLAT arrays.** A generic combinator returns `U[]`, but the only fully-controlled allocation intrinsic, `lin_array_allocate`, infers to the Json-wildcard `Array(TypeVar(MAX))`; monomorphization's `subst` rewrites `U` but never the MAX wildcard, so the allocation stayed TAGGED while a `U=Int32` consumer read it FLAT (garbage). The checker now refines the wildcard element of a fresh `lin_array_allocate` to the function's declared-return element — both for a direct `=> lin_array_allocate(n)` body and for the realistic intermediate-binding shape `val result = lin_array_allocate(n); …; result` — so `subst` pins `Array(Int32)` and `is_flat_scalar` emits a flat allocation matching the flat reader. STRICTLY gated to `lin_array_allocate` (every other `Json[]`-returning call stays tagged); a user annotation on the binding wins. The write uses the representation-aware `lin_array_set`, so producer/writer/reader all agree.
- **(from ADR-066) A `Json` argument binds a generic `T[]` param to the Json wildcard, and the import path erases leftover inference TypeVars to Json.** `collect_type_subs` (lin-check) and `collect_subs` (lin-ir) gained `(Array(pt), TypeVar(MAX))`/`(Iterator(pt), TypeVar(MAX))` arms so a `Json` array argument binds `T=Json` (a representation-consistent TAGGED `$Json` monomorph) while an `Int32[]` argument still binds `T=Int32` (FLAT `$Int32`). And `monomorphize_inner` runs every binding through `erase_nonconcrete_typevars`: a leftover/unsolved INFERENCE TypeVar (id `< GENERIC_TV_BASE`) is rewritten to `TypeVar(MAX)` before keying a specialization, so the import path can never emit a `$T<id>` garbage monomorph (which had read/allocated at a bogus element type → capacity overflow / heap corruption). A QUANTIFIED generic id is left untouched so a genuinely-unconstrained param still gets the clean "cannot infer a concrete type" diagnostic.
- **(from ADR-068) Route `map`/`filter`/`reduce` through the materializing `lin_*` intrinsics with representation-safe element reads.** Before the inliner landed, the three combinators were first switched from hand-written `for`/`push` loops to thin wrappers over `lin_map`/`lin_filter`/`lin_reduce`, whose lowering allocates a flat output for a flat-scalar result and reads flat only from a PROVABLY-FLAT producer (`is_provably_flat_producer` — a `range`/`map`/`filter`/flat-alloc call or a non-empty scalar array literal); otherwise it reads via the runtime-`elem_tag`-aware `lin_array_get_tagged`, keeping `[]`+push arrays (which allocate TAGGED even when typed `Int32[]`) sound. This step also routes a push into a statically-flat array through `lin_push_dyn`, and fixed three latent bugs the routing exposed: a curried callback at full arity returning a function was wrongly treated as under-application (now disambiguated by arg-count vs declared arity); `emit_index_loop`'s phi back-edge hard-coded the body block (now patched to the block that actually jumps back, needed for filter's keep/skip split); and `collect_type_subs` gained `Array`↔`Iterator` cross-unification so a generic `T[]` param applied to `range(0,n)` binds `T=Int32`. At `-O2` this intermediate step was perf-NEUTRAL on its own (LLVM already elided the cheaper boxing); the real win came only once the closure call itself was eliminated by the inliner above.

## ADR-070: `await` enforces Error handling via `T | Error` (no nominal Promise type)

**Decision.** `await` is typed as a generic `<T>(p: T): T | Error` in `stdlib/async.lin`. It is the
single point where a faulted async computation surfaces as an `Error` value (spec §24.2.2), so it is
also the single point where the `Error` member is injected into the type. The result is a union
`T | Error`, and the *existing* union-assignment check — the same machinery `fromJson` relies on
(ADR-049) — rejects assigning it to a bare target type:

```
val r: Int32 = await(p)   // Error: Expected type Int32, got ?T | { "type": String, "message": String }
```

To consume the result you must handle the `Error` case (`match … is Error => … else => …`), exactly
as the spec intends. No new checker, codegen, runtime, or intrinsic code was needed; the feature is a
pure stdlib signature change leaning on generics (ADR-063) plus the pre-existing union-vs-bare check.

**Why `await`, not `async`.** The natural reading of the spec ("async wraps its return in `T|Error`")
is *not* codegen-sound here, and an early attempt to type `async` as `<T>(f: () => T): T | Error`
crashed codegen (`Found PointerValue … but expected the IntValue variant` in `boxing.rs`). The reason:
`async`/`poolAsync`/`race`/`timeout`/`retry` all return a **live `LinPromise*` handle**, not a resolved
value — the value only materialises at `await`. A union type forces a boxed-`TaggedVal*` representation
and scalar boxing; applying it to a promise pointer makes codegen try to box the promise *as* its inner
scalar, which is a representation mismatch. So every promise-*producing* wrapper keeps its opaque `Json`
typing (a `LinPromise*` is just an opaque pointer, same as `Json`), and only the promise-*consuming*
`await` — whose runtime result genuinely is a boxed value — carries the `T | Error` union. This still
satisfies §24.2.2 (you cannot use an awaited result without handling the `Error`), which is the rule the
spec actually cares about; it just attaches the union one call later than the prose suggests.

**Why lightweight (no `Type::Promise<T>`).** Introducing a nominal `Type::Promise<T>` would touch
type compat/resolve/zonk/monomorphize, every async intrinsic signature, and codegen boxing — a large,
risky change for the §24.2.2 guarantee alone. The generic-union approach reuses what already exists.

**Known limitation (honest).** Because there is no nominal `Promise<T>` and a promise handle is erased
to `Json`, this enforces *"you must handle the Error after awaiting"* but does **not** catch *"you forgot
to await"* — i.e. using a promise as if it were the value. A real `Type::Promise<T>` would catch the
latter (a promise wouldn't be assignable to its inner type), but at the cost above. This is not a
regression: the previous all-`Json` typing didn't catch the forgotten-await case either. Deferred.

**Narrowing footnote.** Match narrowing does not strip the structural `Error` member from an
`?T | Error` union whose `T` is an unsolved type variable (the awaited value, since the promise is
`Json`). So `match await(…) is Error => … else => result` leaves `result` typed as the full union in
the `else` arm rather than `?T`. Routing the awaited value through a `Json`-typed boundary (a tiny
`(r: Json): Int32 => match r is Error => … else => r` helper) sidesteps this and coerces cleanly; the
concurrency examples and async tests use that helper for happy-path arithmetic. Improving union
narrowing over unsolved type variables is a separate, pre-existing checker concern.

## ADR-071: Test mocking via `replace` + test-lifecycle story (beforeEach/afterEach/beforeAll/afterAll)

**Status.** Accepted.

**Context.** Restructuring `examples/*` into per-module unit suites surfaced two recurring gaps in
the testing story: (1) no way to isolate a module under test from a real dependency (a unit calling
`std/fs.readFile` or a sibling module always hit the real implementation), and (2) no documented
setup/teardown lifecycle. Lin's compile-time, share-nothing model rules out a JS-style runtime
monkey-patch; the design has to fit how imports actually resolve.

**Key enabling fact.** Every call to an exported function lowers to `CallTarget::Named("<sym>")`,
where `<sym>` is derived from the import's RESOLVED module key (`mangle_module_key(path)` +
export name), NOT the surface path. Codegen resolves a `Named` call by a single
`module.get_function("<sym>")`. So `./some/file`, `./file`, and any transitively-importing module
all emit the SAME symbol, and there is exactly one LLVM definition per symbol. `lin test` compiles
each `.test.lin` as its OWN program, so anything scoped to one program is scoped to one test file.

**Decision — mocking (`replace`).**
- Syntax: `import { someExport } from "./some/file"` (brings in the real symbol + its type), then
  `replace someExport = <expr>` at top level of a `.test.lin`. Vals are replaceable too.
- Semantics (**Option A — replace EVERYWHERE**): the `replace` body becomes the canonical definition
  emitted for that export's mangled symbol; the original module's definition of that symbol is
  skipped in this program. Because resolution is symbol-name based, EVERY reference — the test file,
  the module under test, and any transitive importer, however the path is spelled — sees the mock.
  This is the "replaced no matter how you reference it" guarantee.
- The mock body is type-checked against the real export's signature (drift is a compile error); the
  landed cross-module type-import work supplies the real signature.
- Stdlib is mockable: `std/fs.readFile` etc. are ordinary compiled Lin wrappers, so `replace`
  redirects the Lin-API call sites (the C `lin_fs_*` runtime symbol is never reached). Mocking at the
  Lin-API level is the intent.
- NOT replaceable: the polymorphic INTRINSIC primitives (`print`, `map`, `filter`, `reduce`, `for`,
  `length`, `toString`, the async family) — codegen special-cases these, they are not `Named` calls.
  A `replace` targeting one is a compile error.
- `replace` is only meaningful in a `.test.lin`; using it in a `lin build` program is a hard error
  (it would silently swap stdlib in a shipped binary).
- Spies are an ordinary mock closing over a module-level `var`/`Shared` cell (count calls, capture
  args), asserted after the run — no extra framework.
- Accepted tradeoff of Option A: a mock that references its own replaced symbol self-recurses
  (`replace readFile = (p) => readFile(p)` loops); delegation/pass-through to the real impl is NOT
  possible. Chosen for simplicity over Option B (leave the test file's own binding original).

**Decision — lifecycle.** No dedicated keywords; the eager-evaluation model already supports the
common cases, and DI/combinators cover the rest:
- **beforeAll**: a module-scope `val`/statement above the suite. `test(...)` bodies run EAGERLY as
  the `tests` array is built, so module-scope setup runs once before them.
- **afterAll**: statements after `run(s)`. CAVEAT: `run` calls `exit(1)` on failure, so post-`run`
  cleanup does not execute when a test fails. For guaranteed teardown, pass cleanup to a `run`
  variant or run cleanup before `run`. (Future: `run` could return a status instead of exiting.)
- **beforeEach/afterEach + fixtures**: prefer a functional `around`/`withFixture` COMBINATOR over new
  keywords — a wrapper that builds the fixture, runs the body with it injected, and tears down:
  `val withDb = (name, body) => test(name, () => val db = open(); val r = body(db); close(db); r)`.
  This is per-test setup + teardown + dependency injection using partial application and the existing
  model; assertion FAILURES are values (don't skip teardown). True keyword `beforeEach`/`afterEach`
  would require deferring test bodies (store thunks, run setup/body/teardown in `run`) — deferred as
  a later option if the combinator proves insufficient.

**Consequences.** Mocking is a compile-time, abs-path-keyed, type-checked symbol override scoped to a
single test program — no runtime cost, no cross-test leakage, works for user modules and stdlib
wrappers alike. Lifecycle leans on Lin's functional/DI grain rather than new syntax. Implementation
spans lin-parse (a `Replace` stmt), lin-check (resolve target symbol, type-check body), and
lin-ir/lin-compile (emit the body under the export's symbol; suppress the original; enforce
test-only + intrinsic/unused diagnostics).

**As implemented.** A few details settled during implementation:
- `Stmt::Replace { name, value, span }` (no `TypedStmt::Replace` — the checker collects overrides
  into a side-channel `TypedModule::replacements`, mirroring `intrinsics`/`exported_types`, so no
  exhaustive-match churn across crates). Lowering emits each mock under the export's canonical
  mangled symbol (`{module_key}_{name}`, or `{sym}__val` for vals); the owning module skips emitting
  that symbol and routes its slot through `import_fn_slots`/`import_val_slots`, so internal sibling
  calls also become `Named` calls to the single mock definition.
- **Test-only gating is by FILENAME** (`*.test.lin`), not subcommand. This is what makes it hold for
  every entry point at once: `lin test` AND the ASan CI leg (which runs `lin build <f>.test.lin`)
  accept it, while `lin build`/`lin run` on a normal program reject it. (A subcommand flag would have
  broken the ASan leg.)
- **Type drift** is caught by an explicit `types_compatible` check after `check_expr` — the
  function-hint path treats expected param types as hints, so an annotation could otherwise override
  them silently.
- **`withFixture` is `Json`-typed**, not generic: `(() => Json, (Json) => Null, String, (Json) =>
  Assertion[]) => Test`. A generic stdlib export taking function params + returning a concrete type
  hit a monomorphization edge (resolved as a `__val` wrapper, link error); the `Json` fixture type
  sidesteps it and is sufficient for a test helper.
- **`run` now delegates to `report`**, the non-exiting variant (`(Suite) => Int32`) added for
  guaranteed afterAll teardown — `run(s) = if report(s) > 0 then exit(1) else null`.
- Worked examples: `examples/processes/` (mock `exec`), `examples/dijkstra/` (mock `std/fs`), and
  `examples/web-server/` (mock `render`) — each replaces a side-effecting dependency in its tests; the feature is
  documented in docs/SPECIFICATION.md §22.8–22.9, docs/STDLIB.md (std/test), and the doc-site
  Testing tutorial.

## ADR-072: Circular imports are a compile-time error (DFS visiting-stack)

**Decision**: A circular import is rejected at compile time with a
`CompileError::ImportCycle` carrying the cycle as a readable chain
(`circular import detected: a -> b -> a`). Import resolution
(`pre_resolve_imports_inner` in `lin-compile`) threads a `visiting: Vec<String>`
DFS stack of module *identities*; before descending into an import it checks
whether that identity is already on the stack and, if so, returns the cycle chain
instead of recursing. A module's identity is its canonicalised absolute file path
(so `../a` and `a` are one module) or, for stdlib, the `std/...` path. The
entry-point module seeds the stack so a cycle that loops back to the entry is also
caught. Each module is popped on the success paths (cache-hit and
checked-and-cached), so a **diamond** — one module reached by two independent
paths — is resolved once and is *not* flagged (the second visit hits the
`cache.contains_key` guard).

**Rationale**: Resolution recurses into each import *before* inserting it into the
cache, so the pre-existing `cache.contains_key` guard could never break a cycle —
`a -> b -> a` recursed forever and **overflowed the stack** (SIGABRT), a crash with
no diagnostic. The spec originally called for *lazy* initialisation with a
*runtime* cycle error, but the implementation resolves eagerly at compile time, so
catching the cycle during resolution (earlier, with a clean message) is both
simpler and stricter than the original design. A visiting-stack DFS is the standard
cycle-detection shape and yields the chain for free.

**Consequence**: A cyclic import fails `lin build`/`lin run` with
`circular import detected: <chain>` and a non-zero exit, never a stack overflow.
Diamonds and ordinary deep import graphs are unaffected. Spec §22.5/§20.1 and
decision-list #29 updated to "circular import is a compile-time error." Regressions:
`test_circular_import_is_diagnosed_not_stack_overflow` and
`test_diamond_imports_are_not_false_cycles` in `crates/lin/tests/integration.rs`.

## ADR-073: `std/template` delegates to minijinja (Jinja syntax; undefined→empty; render errors→Error)

**Decision**: `std/template` is now backed by the [minijinja](https://crates.io/crates/minijinja)
crate instead of the hand-rolled `${ }` substituter. Template syntax is Jinja-style:
`{{ var }}` substitutions, `{% for %}` / `{% if %}` control flow, and the standard
builtin filter set. This is a **clean break** — the old `${ }` delimiters are gone.
minijinja is added to `lin-runtime` with `default-features = false, features =
["builtins", "serde"]`: `builtins` re-enables control flow + filters (which the
default-off baseline drops); `serde` lets minijinja consume a `serde_json::Value` as
the render context directly. **Undefined / missing variables render as the empty
string** — minijinja's default `Undefined` behaviour; strict/undefined-is-error mode is
deliberately *not* enabled.

**Data bridge**: the runtime already exposes `json::tagged_to_json(tv: *const u8) ->
serde_json::Value`, and minijinja renders against a `serde_json::Value` directly. So
`lin_template_render` is just: resolve the template `LinString` to `&str` →
`tagged_to_json(data)` → fresh `minijinja::Environment` → `render(&ctx)`. The `{}` data
arg may arrive either as a `TaggedVal*(TAG_OBJECT)` or a bare `LinObject*` (the foreign
`{}` param does not force boxing on every path), so the FFI detects the leading tag and
synthesises a stack `TaggedVal(TAG_OBJECT)` for the bare-pointer case before calling
`tagged_to_json` (which only reads tag+payload — it neither frees nor owns the input).

**Error handling (option a)**: the FFI return type changed from `*mut LinString` to a
tagged value `*mut u8` (`Json`), matching the established `readFile`/`lin_fs_read_json`
convention (declared `=> Json` in the `import foreign "lin-runtime"` block, documented
conceptually as `String | Error`). Success boxes the rendered string into a
`TAG_STR` tagged value; a template **syntax error or render failure** returns
`make_error_tagged(...)` → `{ "type": "error", "message": ... }`, discriminated with
`is Error` exactly like other fallible stdlib operations. Typing the wrappers as `Json`
(rather than a literal `String | Error` annotation) is what keeps callers that use the
result directly as a string working unchanged — `out.contains(...)` in the web-server
tests and `writeFile(path, rendered)` in the docs-site builder both rely on the `Json`
wildcard, and a hard `String | Error` annotation would reject those (`Error` has no
`.contains`; `writeFile` wants a `String`). This mirrors how the docs already describe
`readFile` as `String | Error` while its signature is `Json`.

**Ownership**: `lin_string_from_bytes` returns an OWNED (+1) string; that owned
reference transfers into the `TAG_STR` tagged box, and the standard owned-release
lowering contract drops it exactly once. No borrowed-string return path is introduced,
so the recurring owned-vs-borrowed UAF/double-free class does not apply.

**Consequence**: templates gain real loops/conditionals/filters. Migrated `.jinja` files
(`examples/web-server/views/index.jinja`, `docs-site/templates/{page,home}.jinja`) and
all call sites/tests from `${x}` to `{{ x }}`. The "missing key → empty string" change
replaces the old "missing key → `null`". HTML in template variable *values* is passed
through unescaped (the environment uses a non-`.html` template name, so autoescaping is
off) and is not re-parsed as Jinja. Regressions: new `{% for %}` / `{% if %}` /
missing-var / syntax-error cases in `stdlib/template.test.lin`; web-server and docs-site
(44 pages) render verified end-to-end.

## ADR-074: `std/template` layout system via minijinja path loader (`render` is file-based)

**Decision**: `std/template` gains a real layout/inheritance system — `{% extends %}`,
`{% block %}`, `{% include %}` — rather than the flat substitution it had after ADR-073.
This is exactly the capability the old hand-rolled `${ }` engine could not express and
was the motivation for moving to minijinja.

**Why `render` had to become path-based**: template inheritance needs a *loader* — when
a template says `{% extends "base.jinja" %}`, the engine must fetch `base.jinja` by name.
After ADR-073, `render(path, data)` was implemented in Lin as `readFile(path)` →
`lin_template_render(string, data)`, i.e. it handed minijinja a single anonymous
in-memory string with no directory context, so `extends`/`include` had nothing to
resolve against. The fix is a second FFI entry point, `lin_template_render_path(path,
data)`, that splits `path` into `(dir, basename)`, sets `env.set_loader(path_loader(dir))`,
and renders the basename. minijinja then lazily loads any referenced template by name
from `dir`. `std/template.render` now calls this directly (no more `readFile` in Lin);
the "file not found" Error is produced by the loader instead of an explicit `readFile`
check, and still surfaces as `{ "type": "error", "message": ... }`.

**`renderWith` stays string-based**: it takes an in-memory template with no source
directory, so it deliberately *cannot* resolve `extends`/`include` — documented as such.
The two entry points are the natural split: inline string vs. file-with-neighbours.

**minijinja features**: this required adding **`multi_template`** (the `{% extends %}` /
`{% block %}` / `{% include %}` statements — gated separately from `builtins`; without it
the parser reports `unknown statement extends`) and **`loader`** (`path_loader`) to the
existing `["builtins", "serde"]` set. Both are pulled in with `default-features = false`.

**Consequence**: the two example projects that use templating now demonstrate proper
layouts instead of duplicated page chrome:
- `docs-site/templates/` — new `base.jinja` holds the shared `<head>`/nav/scripts with
  `{% block body_attrs %}` and `{% block main %}`; `page.jinja` and `home.jinja` shrink to
  `{% extends "base.jinja" %}` + their block overrides (home adds `class="home"` and the
  hero). The builder (`docs-site/builder/main.lin`) switched from `renderWith(readFile…)`
  to `render(path, …)` and now guards the render result as an `Error`.
- `examples/web-server/views/` — `index.jinja` extends a new `base.jinja`, which itself
  `{% include "footer.jinja" %}` to show partials.

Verified end-to-end: `stdlib/template.test.lin` gains an inheritance case (writes
base+child to a temp dir, renders the child); `examples/web-server/template.test.lin`
asserts the base skeleton + included footer appear; the docs-site builder regenerates all
44 pages with the home/regular layouts distinct and no unrendered tags or error objects.

## ADR-075: Affine resource types + move-transfer

**Decision**: A *resource type* — currently just `Stream<T>` (§27.9, ADR-076) — is **affine**: it
may be used **at most once**, and dropping it unused is fine. Resource values cross a thread boundary
by **MOVE** (a pointer handoff, no clone), where transferable JSON-shaped values still cross by deep
**COPY** (ADR-043 Option C, the existing path). Both rules yield **disjoint object graphs** per
thread, so the single-threaded non-atomic refcount (ADR-039) stays sound on both sides.

This completes the transfer model anticipated in ADR-043:

1. **Transferable values cross by deep copy (unchanged).** A JSON-shaped, acyclic, `Function`/
   `Iterator`-free value (the only kind a thunk may capture or return, §24.2) is deep-copied at the
   boundary so each thread owns a private graph. Non-atomic RC is sound because nothing is shared.

2. **Resource values cross by move (new).** A `Stream<T>` cannot be deep-copied — it owns an OS fd and
   a live read position; copying would alias the fd. Instead it is **moved**: the box pointer is handed
   to the worker thread verbatim (no clone, no source-side release), the source binding must never touch
   it again, and the **worker** owns it for the rest of its life and releases it (the RC-drop finalizer,
   ADR-076, runs on the worker). Because the source relinquishes the only reference, the moved graph is
   exactly as disjoint as a copied one — one owner, one thread — so non-atomic RC remains correct without
   atomics. A move is O(1); a copy is O(size). This is the `.promise()` hand-off (§27.9).

**Affine, not strict-linear.** A strict-linear discipline would make *dropping* a resource an error
(every stream must be explicitly consumed). We choose **affine** (use-at-most-once) because the RC-drop
finalizer (ADR-076) already closes the fd deterministically when the last reference goes away, so an
un-consumed stream is not a leak — it is closed at end of scope like any other heap value. Therefore:
- **Double-use is the only hard error.** Using a stream after it has been moved or terminally consumed
  is a compile-time error.
- **Must-use is a WARNING, not an error.** A stream that is built and never drained/awaited is almost
  always a mistake (the pipeline never runs), so the checker warns — but it is sound (the finalizer
  closes the fd), so it does not block compilation.

**The flow-sensitive use-after-move check.** The checker tracks, per local binding of resource type, a
*consumed* flag through the control-flow of a function body. A binding becomes consumed when it is:
passed as an argument (the callee takes ownership), returned, or fed to a terminal/sink op
(`.drain()`/`.promise()`/`.for(...)`/`readText`/`collect`). A later reference to a consumed binding is
the use-after-move error. Branches join conservatively: a binding consumed on *any* path is treated as
consumed afterwards (a use on the other path is still flagged, since the program cannot statically know
which path ran). This is the same shape as the `async` var-capture analysis (ADR-034) — a localized,
flow-sensitive scan over the typed body — not a full borrow checker.

**v1 placement restriction.** A `Stream` value may live **only** in a `val` binding, a function
parameter, or a function return position. It may **not** be stored in an object field, an array element,
or a `var`. This confines the move-checker to *local bindings* with a single static name — it never has
to reason about aliasing through a container or through a mutable cell that two closures share
(ADR-015). The restriction is enforced in the checker (a `Stream`-typed object/array literal element or
`var` initializer is a type error) and is **relaxable later** (container-linearity / region tracking)
without changing the surface API. It mirrors the deliberately-narrow scope choices of ADR-044 (`Shared`
is invariant, never widens to `Json`) and ADR-048 (the structured-decode gate).

**`CAP_MOVE` in the closure-env ABI.** Capture kinds live in two mirrored places that previously stopped
at `CAP_TAGGED=5` (`crates/lin-runtime/src/transfer.rs`'s `CAP_*` family and `lin-ir/src/ir.rs`'s
`enum CaptureRelease`). A resource capture gets a new kind **`CAP_MOVE=6`**. At the thread-transfer
boundary (`transfer_clone_env`, ADR-043/ADR-060) a `CAP_MOVE` capture is **moved, not cloned**: the box
pointer is copied into the worker's env and the source env's slot is **not** released (`release_env_copy`
skips it) — the worker now owns it. `env_is_transferable` accepts a `CAP_MOVE` capture (a resource is
transferable by move even though it is non-copyable). This is the single ABI mechanism that realises the
move at run time; everything else (the affine check) makes it *safe*.

**Rationale**: ADR-043 deliberately left "resource values cross by move" as future work — its Option C
deep-copy is *total* only over transferable types, and a stream is precisely the non-transferable,
single-owner value that copy cannot handle. Move is the natural dual: copy gives each side its own graph
by duplication; move gives each side its own graph by *relinquishment*. The affine check is what makes
relinquishment sound (the source provably never touches the moved value again), and it is the minimum
needed — full linearity (must-use-as-error) buys nothing once the finalizer guarantees cleanup, and a
full borrow checker is far more than local single-owner resources require. The placement restriction
keeps the v1 check tractable by removing aliasing entirely.

**Consequence**: `Stream<T>` is the first affine type; the machinery (consumed-flag flow analysis,
`CAP_MOVE`, the placement restriction) is reusable for any future single-owner OS resource. The
move-transfer path **must** be verified under AddressSanitizer (a `cargo test` pass cannot catch the
UAF/double-free class): a moved stream's fd must close **exactly once**, on the worker, with no
source-side release and no double-free. A program that violates the affine rule fails to compile
(`use of moved value 's'`); a built-but-never-run stream compiles with a `stream is never consumed`
warning. The non-atomic RC hot path (ADR-039) is untouched — `CAP_MOVE` is the only new boundary
behaviour, and it does *less* work than the copy path, not more.

## ADR-076: `Stream<T>` distinct from `Iterator`; `for`/terminal error semantics

**Decision**: A `Stream<T>` is a new **opaque runtime type** — `Type::Stream(Box<Type>)`, sibling to
`Type::Iterator` (`crates/lin-check/src/types.rs`), covariant in `T`, erased to a `TAG_STREAM` box at
runtime — and it is deliberately **NOT** modelled as an `Iterator<T>`. A stream is a lazy pull-graph
over a byte/value source plus an optional push sink; the surface API and semantics live in §27.9. This
ADR records the three semantic choices that make a stream a thing of its own.

**Why a stream is not an iterator.** The iterator protocol (§18.2, ADR-026) is built from four
**pure** state-transition functions — initial-state, continuation predicate, next-state, current-value
— and §18.2.1 even promises restartability by re-running the initial-state thunk. A stream violates
every one of those assumptions:
- It is **effectful**: pulling the next chunk performs a `read(2)` and advances an OS file position;
  there is no pure "current value at this state."
- It is **fallible**: a read can fail mid-traversal (`EIO`, connection reset), which a pure
  `(State) => T` current-value function cannot express.
- It **owns an OS resource** (an fd) with a lifetime, so it is single-owner/affine (ADR-075), whereas
  an iterator is a freely-copyable pure description.

Forcing a stream into the iterator shape would either make the protocol functions impure (breaking
§18.2's contract and the iterator-restartability guarantee) or hide the fd lifetime, so `Stream<T>` is a
distinct opaque kind. It reuses `Iterator`'s *threading* through the compiler (the ~16-file checklist:
types/compat/zonk/resolve, checker, IR lowering/monomorphize, codegen) but never its *protocol*.

**RC-drop auto-close finalizer.** A `TAG_STREAM` box carries the source backend (an fd + read state, or
an adapter node pointing at its upstream). Its **release finalizer closes the fd** when the refcount
reaches 0 if it is not already closed — deterministic cleanup with no GC, exactly the ADR-039 RC contract.
An explicit `close(s)` is **idempotent** (closing an already-closed stream is a no-op) and exists for
callers who want determinism rather than scope-end timing. This is what makes the affine *drop-is-fine*
rule of ADR-075 sound: an un-consumed stream still closes its fd. The finalizer is in the UAF/double-free
class and **must** be ASan-verified (fd closes exactly once).

**`for` over a stream returns `Null | Error`.** A stream is consumed with `.for(fn)` dot-application —
**not** `for…in`, which does not exist in Lin (iteration is always `.for(fn)`/combinators, §18). Driving
a stream to completion can end two ways, so `for` over a stream is typed **`Null | Error`** (unlike `for`
over an array/iterator, which is `Null`, §18.1):
- **EOF ends the loop normally** → the `for`-expression evaluates to `Null`.
- **A read `Error` mid-traversal becomes the `for`-expression's value** → the loop stops and yields the
  `Error`.

So a stream `for` reads like any other fallible op (§25.1):

```txt
val outcome = readStream("in.log").lines().for(line =>
  print(line)
)
match outcome
  is Error => print("read failed: ${outcome["message"]}")
  else     => null
```

The exact ending is **source-kind-dependent**: a file stream that reaches EOF ends with `Null`; a socket
stream whose peer resets ends with `Error`. `lin_for`'s lowering (`lin-ir/src/lower.rs`, `lower_for`)
gains a stream branch that drives the pull-graph and produces the `Null | Error` result.

**In-band error threading through the lazy graph.** The pipeline is a lazy pull-graph; rather than make
every adapter re-check `is Error`, a **poisoned upstream makes every downstream adapter a passthrough**.
The first read error propagates *as data* straight to the terminal op, which surfaces it as the
`Null | Error` (`for`/`drain`) or `Promise<Null | Error>` (`promise`) result. This keeps the chain
fluent — `readStream(p).lines().map(f).filter(g).writeStream(q).drain()` has no error handling at each
step, only at the terminal — and it short-circuits: once poisoned, no further reads or user callbacks
run. It is the stream analog of the §20 value-based error convention: errors are ordinary values flowing
through the graph, not exceptions.

**Rationale**: Keeping `Stream` distinct from `Iterator` preserves the iterator's purity/restartability
contract (which the optimizer and the §18 combinators rely on) while giving effectful, fallible,
fd-owning sources an honest type. The RC-drop finalizer reuses the existing deterministic-RC cleanup
(ADR-039) instead of inventing a lifetime story. Typing `for` as `Null | Error` makes a stream loop obey
the same `T | Error`-handling discipline as every other fallible stdlib op (§25.1, ADR-046), and
in-band threading keeps the fluent API the brief requires without an `is Error` at every adapter.

**Consequence**: `Stream<T>` adds one opaque runtime kind (`TAG_STREAM=19`), one finalizer, and a
stream branch in `lower_for`; it does not touch the iterator path. A stream `for`/terminal that ignores
its `Null | Error` result trips the same union-vs-bare-target check as `await` (ADR-070) when assigned
to a bare type. Distinct from `Shared<T>` (ADR-044, shared mutable, atomic-RC, copy-in/out) and
`Frozen<T>` (ADR-045, immortal read-only): a stream is single-owner, moved not shared (ADR-075). Full
surface spec in §27.9; stdlib API in `std/stream` (STDLIB.md).

## ADR-077: Unified iterable combinators via receiver dispatch (`std/iter`)

**Decision**: The iterable combinators — `map`/`filter`/`reduce`/`for`/`while`/`take`/`drop`/`flatMap`/
`takeWhile`/`dropWhile`/`flatten`/`concat`/`find`/`some`/`every` — plus the iterator constructors
`range`/`rangeStep`/`iter`/`iterOf` live in **one** module, `std/iter`, and dispatch on the **static
type of the receiver** (arg0): **eager** (a materialised `U[]`) for an `Array`/`Iterator` receiver,
**lazy** (a `Stream<U>` adapter node) for a `Stream` receiver. Terminals over a stream gain an `| Error`
arm (a stream read is fallible). One name, one import, one fluent chain over any iterable source — the
receiver's type picks eager-vs-lazy. This is Lin's first-argument-dispatch philosophy (§4.4) applied to
the combinator set.

**Why this is not a new mechanism.** `for` ALREADY dispatched this way: `lower_for` branched on
`Type::Stream(_)` to emit a stream driver (`Null | Error`) versus the array index-loop (`Null`).
Arrays and iterators were ALREADY unified under one name via a `T[] | Iterator` union param. ADR-077
folds the third source — `Stream` — into a unification that already covered two of three, rather than
inventing function overloading. The combinator name set is **closed** (the fixed builtin list above), so
this is a restricted special-case, not general overloading.

**Mechanism (one dispatch fact, three readers).** The receiver type at a concrete call site drives all
three of:
1. **Return typing** — `streamish_combinator_ret` (`lin-check/src/checker/call.rs`) re-types the
   call-site result when arg0 is *definitely* a stream: adapters return `Stream<T>`/`Stream<U>`
   (keeping the chain lazy for the next link); terminals return `U | Error` (`reduce`),
   `T | Null | Error` (`find`), `Boolean | Error` (`some`/`every`), `Null | Error` (`for`/`while`).
   For an `Array`/`Iterator` receiver the eager array-shaped type flows through unchanged.
2. **IR redirect** — `stream_combinator_intrinsic_name` (`lin-ir/src/lower.rs`) re-routes a
   `std_iter_*` call whose arg0 is `Type::Stream(_)` to the lazy `lin_stream_*` backend, bypassing the
   eager pure-Lin/`lin_map` body entirely.
3. **Affine consumption** — `callee_routes_to_stream_op` keys the use-after-move check on the SAME
   `(module, export)` dispatch fact.

The std/iter wrappers spell their iterable param as the union `T[] | Iterator | Stream` (so a `Stream`
is accepted without the opaque type leaking into a bare `Json` param) and **drop their return
annotations** so the checker-computed receiver-dependent type flows through. The "definitely a stream"
predicate (`is_definitely_stream`) accepts a bare `Stream<…>` or a `Stream<…> | Error` (a source
intrinsic's result), but NOT a mixed `Array | Iterator | Stream` union — a union with a live array
branch keeps its eager return.

**Module boundary — combinators in `std/iter`, array-shaped ops in `std/array`.** The combinators work
over *any* iterable, so they belong to the iterable module. The genuinely array-shaped ops — those that
need a materialised, indexable, ordered array (`push`/`slice`/`set`/`at`/`length`/`reverse`/`sort`/
`sortBy`/`zip`/`unique`/`chunk`/`compact`/`indexOf`/`partition`/`sum`/`product`/`min`/`max`/`minBy`/
`maxBy`/`append`/`prepend`/`scan`/`groupBy`/`countBy`/`arrayAllocate*`) — stay in `std/array`.
`std/iter` is the **lower** module in the dependency graph: it must NOT import from `std/array` (that
would form an `array → iter → array` cycle, a compile-time error, ADR-072), so the two array primitives
its bodies need internally (`length`/`push`) are duplicated there as private thin wrappers; `std/array`'s
own combinator-using helpers import the combinators back from `std/iter`.

**Combinators are NOT dual-exported.** Exporting the same combinator from both `std/iter` and (say)
`std/array`/`std/stream` was **rejected** as confusing — there must be exactly one source of each name.
`std/stream` therefore **stopped exporting** `map`/`filter`/`take`/`for`/etc.; it keeps only
stream-specific ops (`readStream`/`writeStream`/`drain`/`collect`/`readText`/`promise`/`close`/`lines`/
`linesMax`/`chunks`). A stream pipeline imports its combinators from `std/iter` and its sources/sinks
from `std/stream`.

**Affine subsumption — the prior soundness hole is closed.** The old affine check consumed a stream off
a fragile NAME allowlist (`is_stream_consuming_op`) while the IR moved a stream on ANY streamish-typed
arg; the two lists drifted (`linesMax`/`promise`/`close` slipped through, and `promise` is a cross-thread
UAF locus). Receiver dispatch makes consumption read the SAME fact the dispatch computes: any
definitely-stream value flowing into any stream-dispatched combinator or stream-specific op is MOVED, per
argument (so `concat`'s two stream args are both consumed). The checker, the IR redirect, and the affine
consume-set now read one fact, not three lists, so they cannot diverge. The five adversarial
use-after-move attacks became regression tests (all reject at check time). Cross-ref ADR-075 (affine
resource types + move-transfer).

**v1 limitation — dispatch is at the concrete call site only.** Receiver dispatch fires when a combinator
is called with a *concrete* `Stream` receiver. A stream passed THROUGH a user-defined generic `Iterable`
parameter and combined inside that function stays **array-shaped** (the param is the union, not a definite
stream), so it does not become lazy. This is the safe resolution: the eager array path is always correct;
only the lazy optimisation is forgone. Relaxable later without changing the surface API.

**Rationale**: One combinator vocabulary over arrays, iterators, and streams is the user-facing payoff —
`readStream(p).lines().drop(1).take(4).map(f).reduce(0, g)` reads identically to the array form yet runs
lazily with bounded memory. Reusing the `for` dispatch precedent (a closed name set, type-directed
return) avoids general overloading. Keying affine consumption off the dispatch fact eliminates the
allowlist-vs-IR divergence class outright rather than patching it.

**Consequence**: `std/iter` is a new stdlib module; `std/array` and `std/stream` shed the moved
combinators (existing imports migrate to `std/iter`). New lazy stream backends
(`lin_stream_map`/`filter`/`take`/`drop`/`take_while`/`drop_while`/`flat_map`/`flatten`/`concat`/`while`)
and stream terminals (`lin_stream_reduce`/`find`/`some`/`every`, alongside the existing `lin_stream_for`)
back the lazy arm; each drives-and-consumes the stream on the calling thread, fault-isolated. The
flat-producer recognition (the unboxed-scalar fast path, ADR-069) follows the moved names. Verified on
`feat/streams`: full suite green, ASan clean, all five affine attacks reject. Full stream semantics in
§27.9 (ADR-076); combinator surface in §18; stdlib API in `std/iter` (STDLIB.md).
