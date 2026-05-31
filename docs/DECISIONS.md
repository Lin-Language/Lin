# Architecture Decision Records

## ADR-001: Static typing via lin-check

**Decision**: All Lin programs are type-checked before codegen. The `lin-check` crate performs bidirectional inference, structural typing, union narrowing, and exhaustiveness checking. Runtime type tags still exist in the runtime for `is`/`has` pattern dispatch, but no program should reach codegen with unresolved type errors.

**Rationale**: A full bidirectional type system with generics, variance, and numeric widening allows LLVM to emit unboxed primitives and enables helpful compile-time error messages.

**Consequence**: Type annotations are parsed, checked, and emitted into `TypedModule`. The `lin-check` crate owns all type inference logic.

## ADR-002: Minimal built-ins, stdlib for iteration

**Decision**: Only `lin_for` and `lin_iter` are compiler intrinsics with special codegen. Higher-level functions (`map`, `filter`, `reduce`, `range`, `iterOf`) are implemented in `.lin` stdlib files (`std/array`) and must be explicitly imported by user code.

**Rationale**: `lin_for` and `lin_iter` require special compiler treatment (inline loop emission, iterator struct construction). All other iteration functions can be expressed in Lin itself. Keeping intrinsics minimal means the runtime stays small and the stdlib is readable Lin code.

**Consequence**: `range()` returns a lazy `Iterator`. User code imports `map`, `filter`, `reduce`, etc. from `std/array`. The compiler recognises `lin_for`/`lin_iter` by name and emits them as native loops rather than function calls.

## ADR-004: Objects suppress indentation tracking

**Decision**: When inside `{ }` (brace depth > 0), the lexer suppresses newline tokens and indentation tracking (no INDENT/DEDENT emitted).

**Rationale**: Multi-line JSON object literals must not trigger block parsing. This matches the behaviour for `( )` and `[ ]` which also suppress indentation.

**Consequence**: You cannot have indentation-significant syntax inside object literals (which is fine — object values are expressions, not statements).

## ADR-005: String interpolation as compound token

**Decision**: The lexer produces a single `InterpString(Vec<InterpPart>)` token for interpolated strings. Each `InterpPart::Expr` contains its own sub-token-stream that the parser processes independently.

**Rationale**: The initial approach of inlining interpolation tokens into the main token stream caused ordering issues with the pending-token queue. A compound token with embedded sub-streams is self-contained and avoids interaction with indentation tracking.

**Consequence**: Interpolation expressions are parsed in isolation (no access to outer indentation context), which is fine since they're always single expressions.

## ADR-006: Dot-chaining across newlines via lookahead

**Decision**: The parser's postfix expression loop checks for `.` across newline boundaries using a save/restore pattern. If a newline is followed by `.`, parsing continues the dot chain. Otherwise, position is restored.

**Rationale**: The spec requires `x\n  .f()` to chain. But aggressively skipping all indentation tokens breaks block structure. The save/restore pattern is conservative — it only consumes whitespace tokens when followed by a dot.

**Consequence**: Dot-chaining works across lines without breaking function bodies or if-then-else blocks.

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

## ADR-010: Multi-line if/then/else syntax

**Decision**: `then` always appears on the condition line (or the last continuation line of the condition). The body follows on an indented block (INDENT … DEDENT). `else` appears at the same indent level as `if`. The parser does not consume any INDENT before `then` — it simply expects `then` after the condition expression.

**Rationale**: Placing `then` at the end of the condition line is clearer and more consistent with how block-opening keywords work in other languages. The old approach of allowing `then` on its own indented line required the parser to tentatively consume an INDENT token before `then`, then emit a corresponding DEDENT, making the grammar more complex with three special-case DEDENT guards. The new rule is simpler: condition, `then`, body block, `else` at original indent, else body.

**Consequence**: All spec-defined if layouts (single-line, multi-line with block body, multi-line with inline body) parse correctly. Condition continuation lines with `&&`/`||` end with `then` on the last continuation line. The `then_indented` tracking variable and its three associated DEDENT guards have been removed from `parse_if_expr`.

## ADR-011: Postfix suppression after DEDENT

**Decision**: The parser's postfix expression loop (`[` and `(`) is suppressed when the immediately preceding consumed token was a DEDENT. Dot-chaining (`.`) is still allowed (as it handles cross-line chaining via a separate lookahead mechanism).

**Rationale**: After a block-bodied function expression like `() => \n  42`, the lexer produces `... IntLit(42) Newline Dedent LBracket ...` — the inner block's `skip_newlines` consumes the Newline, so after the Dedent is consumed, no Newline separates the function from the next line's `[`. Without this guard, `[x]` at the outer block level is incorrectly parsed as index access on the function expression.

**Consequence**: Array/object literals at block level after indented function definitions parse correctly as separate expressions. Same-line index access (`f()[0]`) still works because no DEDENT intervenes.

## ADR-012: Tail call optimization via eval_tail_expr

**Decision**: TCO is implemented by introducing a `TailResult` enum (`Return(Value)` | `TailCall(Vec<Value>)`) and an `eval_tail_expr` method that recognizes self-recursive calls in tail position and returns `TailCall` instead of making a new frame.

**Rationale**: The spec (§27.3) requires direct self-recursive tail calls to run in constant stack space. A trampoline approach avoids modifying the normal `eval_expr_in_env` code path — only `call_function` loops on `TailCall`. Tail positions are: the body of a function, both branches of `if/then/else`, the final expression of a block, and match arm bodies.

**Consequence**: `sum(100000, 0)` runs without stack overflow. Non-tail recursive calls (e.g., `n * factorial(n-1)`) still recurse normally. Mutual recursion is not optimized (per spec: "Mutual tail recursion is not required to be optimised in v1").

## ADR-013: Continuation line parsing via lookahead in and/or expressions

**Decision**: `parse_and_expr` and `parse_or_expr` use a `skip_continuation_newline` helper that looks past Newline tokens for `&&`/`||`. If found, parsing continues the expression; otherwise position is restored.

**Rationale**: The lexer suppresses INDENT/DEDENT for lines starting with `&&`/`||` (per spec §3.2), but still emits a Newline token at the end of the preceding line. Without the parser skip, `x >= 5\n  && active` would parse as just `x >= 5`.

**Consequence**: Multi-line boolean expressions and `if` conditions with continuation lines work as specified.

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

**Rationale**: The spec (§7.3) expects mutual recursion between top-level functions. Without forward declaration, functions must be defined before use, which prevents mutual recursion and requires careful ordering. The mutable-cell approach solves this without changing evaluation semantics — a function that calls another function reads the cell at call time, by which point the definition has been evaluated.

**Consequence**: Forward references work between functions (e.g., `isEven` calling `isOdd` and vice versa). However, eager top-level evaluation that *immediately* calls a forward-referenced function (before its definition is evaluated) will still fail with "Cannot call value of type Null". This is inherent to sequential evaluation and matches the behavior of languages like JavaScript (`let` before initialization).

## ADR-016: User module loading from filesystem

**Decision**: When an import path does not match a `std/` prefix, the interpreter resolves it relative to the importing file's directory by appending `.lin` to the path.

**Rationale**: Multi-file programs need to import user-defined modules. The resolution strategy mirrors Node.js-style relative imports without requiring a leading `./` — the `std/` prefix is the only special case, everything else is relative.

**Consequence**: `import { x } from "lib/math"` in `examples/main.lin` loads `examples/lib/math.lin`. Absolute paths and `..` traversal work naturally via the filesystem.

## ADR-017: Reset at_line_start unconditionally in lexer

**Decision**: The `at_line_start` flag is always reset to false at the top of `next_token()`, regardless of whether the lexer is inside balanced delimiters.

**Rationale**: Previously, `at_line_start` was only cleared when entering `handle_indentation()` (which requires `!inside_balanced()`). This left the flag true when a newline occurred inside braces (e.g., multi-line imports). When the closing brace brought depth back to 0, the stale `at_line_start = true` triggered spurious INDENT tokens on the next call. Always clearing the flag eliminates this class of bugs.

**Consequence**: Multi-line `import { ... } from "path"` statements work correctly. No change in behavior for other constructs since the flag is still set to true on `\n` when appropriate.

## ADR-018: `Number` as a built-in union alias

**Decision**: Add `Number` to the built-in types as a union alias for every numeric family (`Int8 | … | Float64`), and use it in the definition of `Json`. `Number` does not introduce a new runtime kind, a new subtype relation, or any new narrowing rule — it is exactly the union it expands to.

**Rationale**: Without a name for "any numeric," the `Json` type has to enumerate all sixteen numeric families to be accurate, and signatures that accept any numeric have no concise spelling. A true supertype with subtype assignability would introduce a third kind of type relation alongside structural typing and unions, and would force decisions about `is Number` narrowing, arithmetic on a `Number`-typed operand, and how widening (§26) interacts with the supertype. A union alias avoids all of that: `is Int32`, widening, and operator dispatch keep working exactly as they did, because under the hood there is still only a concrete numeric family at every site.

**Consequence**: Spec-only change in v0 (no type checker exists yet — `Number` already parses as a `TypeExpr::Named`). The future type checker treats `Number` as a union alias when resolving assignability and exhaustiveness. Runtime is unchanged: §27.4 still says every numeric value carries its specific family tag and there is no single `Number` representation.

## ADR-019: LLVM 22 via inkwell with dynamic linking

**Decision**: The compiler backend uses LLVM 22 (the latest stable release) via the `inkwell` 0.9.0 Rust wrapper, with the `llvm22-1-prefer-dynamic` feature flag for dynamic linking.

**Rationale**: LLVM 22 is the latest release with the best optimizations and codegen quality. The `prefer-dynamic` flag is required because Debian/Ubuntu package `LLVMPolly.so` as a dynamic library only — no `.a` static archive is provided. Without dynamic linking, the linker fails with "could not find native static library 'Polly'". The `inkwell` wrapper provides a safe, idiomatic Rust API over the LLVM C API and supports LLVM 22.

**Consequence**: The devcontainer installs LLVM 22 from `apt.llvm.org/bookworm` and sets `LLVM_SYS_221_PREFIX=/usr/lib/llvm-22`. The compiled binary dynamically links against `libLLVM-22.so` at runtime, which is available on the devcontainer but would need to be present on deployment targets.

## ADR-020: Unboxed primitive value representation in LLVM IR

**Decision**: Numeric and boolean types are represented as bare LLVM primitives: `Int32` → `i32`, `Float64` → `double`, `Bool` → `i1`. Strings are represented as `ptr` to a heap-allocated `LinString` struct (refcount + len + bytes). Closures are represented as `ptr` to a `{ fn_ptr, env_ptr }` struct. Union types use a heap-allocated tagged representation.

**Rationale**: The type checker produces `TypedIR` with a concrete `Type` for every expression. This means we know at compile time whether a value is `i32` or `f64`, enabling LLVM to treat them as first-class register-width values rather than tagged `Value` boxes. The performance difference versus the tree-walker interpreter (which boxes everything in a `Value` enum) is typically 50–200×. Strings cannot be unboxed (variable-length), so they remain as pointers.

**Consequence**: No boxing for arithmetic, comparisons, boolean operations, or function calls on primitive types. LLVM's optimizer can treat these as register values and apply standard scalar optimizations. Union types and unknown-typed values (TypeVar) fall back to pointer representation.

## ADR-021: TCO via alloca/loop transform (not trampoline)

**Decision**: Tail-recursive functions are compiled using the "loop transform": parameters are stored in `alloca` slots, the function body is wrapped in a `tco_loop` basic block, and tail self-calls store updated argument values into the alloca slots and branch back to `tco_loop` rather than making a recursive call.

**Rationale**: The alloca/loop approach produces standard LLVM IR that LLVM's optimizer understands — it can apply `mem2reg` to promote the alloca slots to phi nodes, yielding optimal machine code. A trampoline approach (returning a thunk and looping externally) requires a heap allocation per tail call and more complex call-site machinery. The loop transform produces a native loop with no allocation overhead.

**Consequence**: Tail self-calls are identified by `is_tail: bool` in `TypedExpr::Call`, set by the checker when the call is in tail position and the callee is the current function. Non-tail recursive calls and mutual recursion still use normal stack frames. `mem2reg` (run as part of `default<O2>`) eliminates all alloca slots from the final machine code.

## ADR-022: Forward-declaration for top-level mutual recursion in codegen

**Decision**: Before compiling the body of any top-level function, `compile_module` pre-scans all `TypedStmt::Val` statements to LLVM-declare any function whose `TypedExpr::Function` has a `name`. These forward declarations are stored in `global_fn_slots` (slot → `FunctionValue`). Function bodies are compiled in a second pass. Direct calls look up `global_fn_slots` first, enabling sibling functions to call each other.

**Rationale**: LLVM requires a function to be declared before it is called. Without a pre-scan, a function `f` that calls `g` (defined later in the source) would not find `g`'s `FunctionValue` in the IR. The pre-scan mirrors ADR-015 (mutable cells for forward refs in the interpreter) but at the LLVM level. The checker's `forward_declare_functions` also pre-registers function types so the body's recursive references type-check correctly and reuse the same slot.

**Consequence**: Top-level mutual recursion works. The slot assigned during type-check pre-scan is reused (via `update_type`) when the actual `val` binding is processed, ensuring the codegen's `global_fn_slots` entry aligns with the slot referenced in call expressions.

## ADR-023: Runtime library as a static archive linked into every binary

**Decision**: `lin-runtime` is compiled as a Rust `staticlib` (`crate-type = ["staticlib", "rlib"]`) that provides C-ABI functions (`lin_print`, `lin_string_concat`, `lin_int_to_string`, `lin_array_alloc`, `lin_panic`, etc.). The compile pipeline locates the `.a` file and passes it to the system linker (`cc`) alongside the LLVM-emitted `.o` file.

**Rationale**: LLVM IR cannot express Rust-level operations like `write!` or `alloc::alloc`. The runtime provides these as well-known C symbols that LLVM IR can `declare` and call. A static archive avoids a runtime shared-library dependency on deployed binaries. Using the Rust `staticlib` crate type ensures `rustc` links in all needed Rust stdlib code (allocator, panic handler, etc.).

**Consequence**: Compiled Lin binaries are self-contained: they link against `libc` (via `cc`) plus the runtime `.a`, with no Lin-specific shared libraries required. The runtime is small (~10KB stripped) since it only contains the functions LLVM IR references.

## ADR-024: Binding name propagation for function identity

**Decision**: When the checker processes `Stmt::Val { pattern: Ident("f"), value: Function { ... } }`, the resulting `TypedExpr::Function { name: Some("f"), ... }` carries the binding name. This is done by detecting the pattern name in `check_stmt` and either calling `infer_function` with the name (enabling tail-call tracking via `current_function`) or patching `name` after inference.

**Rationale**: `TypedExpr::Function` has an optional `name` field used by the codegen to (a) emit a named LLVM function rather than an anonymous `__closure_N` and (b) enable `global_fn_slots` lookup for direct calls. The parser does not embed the binding name into the function expression (names come from the `val`/`var` statement's pattern), so the checker must propagate it. Setting `current_function` during body compilation is also required for tail-call detection (`is_tail_call` only fires when in tail position of the same function).

**Consequence**: Named top-level functions emit named LLVM functions (e.g., `@factorial`) rather than anonymous closures (`@__closure_0`). Recursive calls to the function are recognized as tail calls when in tail position, enabling the TCO loop transform (ADR-021).

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

**Consequence**: `async` thunks run on true OS threads. `await` blocks the caller thread (not a coroutine yield). Values must be JSON-serializable to cross thread boundaries (spec §32.4).

## ADR-028: Cross-thread value transfer via JSON bridge

**Decision**: Values crossing thread boundaries are serialized to a `JsonValue` bridge type (no `Rc`, no `RefCell`) and deserialized on the receiving thread. Functions, iterators, promises, workers, and thread pools cannot cross thread boundaries.

**Rationale**: The compiled runtime uses refcounted heap pointers. Deep-copying at the thread boundary (via the bridge type) is unavoidable without adding `Arc`-based reference counting throughout.

**Consequence**: Async thunk return types must be JSON-compatible (spec §32.4). The serialization is O(size) but is the correct approach given the refcount model.

## ADR-029: JSON bridge type for cross-thread value transfer

**Decision**: `JsonValue` is a `Clone + Debug` enum (no `Rc`, no `RefCell`) that mirrors Lin's data types: `Null`, `Bool`, `Int`, `Float`, `String`, `Array`, `Object`, `Error`. `Value::to_json_value()` converts at the thread boundary (returning `Err` for non-serializable types like `Function`). `JsonValue::to_value()` converts back in the receiving thread.

**Rationale**: `Value` contains `Rc<RefCell<...>>` for arrays and objects, which cannot be sent across threads. Instead of adding `Arc` alternatives, a separate bridge type that is fully `Clone + Send` provides a clean serialization point. This also enforces Lin's spec requirement (§32.4) that async thunk return types must be JSON-compatible.

**Consequence**: Closures, iterators, promises, workers, and thread pools cannot be returned from async thunks (they fail `to_json_value()` with an error). Deep copies are made at the thread boundary. For large objects this is O(size) but is unavoidable given the `Rc`-based value representation.

## ADR-030: IO/FS/HTTP implemented as `lin-runtime` C functions

**Decision**: IO, filesystem, HTTP client, and server operations are implemented as `#[no_mangle] pub unsafe extern "C"` functions in `lin-runtime` (e.g. `lin_io_read_line`, `lin_fs_read_file`, `lin_http_fetch`). Stdlib `.lin` files declare them via `import foreign "lin-runtime"` and expose clean user-facing names.

**Rationale**: IO requires Rust code. Keeping implementations in `lin-runtime` means the compiler just emits `call` instructions for them. The `.lin` wrapper layer keeps user-facing APIs in Lin.

**Consequence**: All IO/FS/HTTP is synchronous on the calling thread. Programs run IO in background threads via `async`/`threadPool`. The HTTP server blocks forever; typical usage is `async(() => serve(8080, handler))`. `tiny_http` was chosen for its simplicity (no tokio required).

## ADR-031: `std/io`, `std/fs`, `std/http`, `std/server` as thin Lin wrappers

**Decision**: Each IO module is a `.lin` file (`stdlib/io.lin`, `stdlib/fs.lin`, `stdlib/http.lin`, `stdlib/server.lin`) that re-exports `__*` intrinsics with clean names and provides Lin-level helpers (`fetchJson`, `postJson`, `json`, `text`, `parseBody`, etc.). They are registered via `include_str!` in `register_stdlib_sources` and loaded on demand when the user imports `std/io`, etc.

**Rationale**: Following the existing pattern (ADR-009): keep the Rust intrinsics small and focused; provide the user-facing API in Lin. This means helpers like `fetchJson` (fetch + parseJson) and `pathMatch` routing can be written in Lin without touching Rust. The stdlib files are compiled once per interpreter session and cached by the module loader.

**Consequence**: Users get `import { readFile, writeFile } from "std/fs"` etc. The `lin_*` runtime symbols are not exported from stdlib — they're implementation details behind the clean wrapper API.

## ADR-032: FFI syntax as `import foreign "<path>"` with indented type block

**Decision**: Foreign function imports use `import foreign "<path>"` followed by an indented block of `val name: Type` declarations. The `foreign` keyword is added to the lexer. The parser reuses the existing indented-block machinery. The AST node is `Stmt::ForeignImport { path, bindings: Vec<ForeignBinding> }`. Each `ForeignBinding` carries the name, type annotation, and span.

**Rationale**: Reusing `import` as the outer keyword makes foreign imports visually consistent with regular imports. The `foreign` keyword distinguishes them syntactically without introducing a separate statement form. The indented block mirrors function body parsing (ADR-014) and keeps all bindings visually grouped under the library path.

**Consequence**: `import foreign "libmath.a"\n  val sqrt: (Float64) => Float64` parses correctly. The token `foreign` is now a reserved keyword and cannot be used as an identifier.

## ADR-033: FFI via `import foreign` and LLVM `declare`

**Decision**: The compiler emits an LLVM `declare` for each foreign binding using the C ABI type mapping. Library paths collected from `ForeignImport` nodes are passed to the linker step in `lin-compile`. `import foreign "lin-runtime"` is a special reserved path that is always linked and skips normal FFI type validation.

**Rationale**: LLVM IR's `declare` is the correct mechanism for external C symbol resolution. Keeping library path collection in the AST means `lin-compile` can drive the linker without a separate manifest.

**Consequence**: FFI requires `lin build`. End-to-end FFI tests compile a C library to `.a` and call it from Lin via `lin build`. The type checker validates that all foreign binding types are legal FFI types (numeric, Boolean, Null, or String) at compile time.

## ADR-034: `async` var-capture check via global slot tracking

**Decision**: The type checker rejects `async(f)` and `pool.async(f)` calls where the thunk `f` directly references any mutable `var` binding (either captured from a non-global outer scope, or referencing a global `var` from within the thunk body).

Implementation:
- `Checker` gains a `mutable_global_slots: HashMap<usize, String>` field, populated whenever a `Stmt::Var` is processed at global scope (when `function_scope_depths` is empty).
- `first_mutable_capture(expr, mutable_globals)` checks a `TypedExpr::Function` for: (a) any `Capture` where `is_mutable == true`; (b) any `LocalGet` in the body that references a slot in `mutable_global_slots`. Body scanning does not recurse into nested `Function` nodes (inner lambdas have their own capture check when their own `async` call is analysed).
- In `infer_call`, after building `typed_args`, if `func == Ident("async")`, every thunk argument is checked. Same check on the thunk args of `infer_dot_call` when `method == "async"`.
- The check also registers the concurrency builtins (`async`, `await`, `parallel`, `race`, `timeout`, `retry`, `threadPool`, `worker`) as intrinsics in `register_intrinsics()` using `TypeVar`-based signatures, so they resolve instead of producing "Undefined variable" errors.

**Rationale**: Sharing mutable state across OS threads without synchronisation leads to data races. Lin's `var` is captured by `Rc<RefCell<Value>>` in the interpreter and by pointer in the compiler — neither is `Send`. The spec (§32.2) requires a compile-time error. Global vars are not recorded as "captures" (they're accessed directly via `LocalGet` with slot from global env), so a two-pronged check is needed.

**Consequence**: `async(() => counter = counter + 1)` where `counter` is a `var` produces a compile-time error with a help message suggesting snapshot capture. `async(() => message)` where `message` is a `val` is allowed.

## ADR-035: Match arm narrowing via scope-shadowing

**Decision**: In `check_match_arm`, when the pattern is `Is(TypeCheck(narrowed_ty))` and the scrutinee is a simple `Ident(name)`, the checker defines a new binding `name: narrowed_ty` in the arm's scope. This shadows the outer binding for the duration of the arm body, giving the narrowed type to all references to the scrutinee within that arm.

**Rationale**: Using scope-shadowing instead of mutating the original binding avoids any state-management burden when leaving the arm scope: `pop_scope()` automatically removes the shadow. It also correctly handles nested scopes and captures within the arm body.

**Consequence**: `match x is Int32 => x + 1` correctly type-checks because `x` inside the arm body resolves to the arm-scope `x: Int32`, not the outer `x: Int32 | String`.

## ADR-036: `async(array)` overload

**Decision**: `async` accepts either a single thunk `() => T` or an array of thunks `(() => T)[]`. The array overload spawns one thread per element and returns an array of `Promise` values in input order.

**Rationale**: `await(async([thunk1, thunk2, ...]))` is the natural idiom for fork-join concurrency. Without the array overload, users would need to call `async(thunk)` individually and collect results manually.

**Consequence**: `async([() => fetch(url1), () => fetch(url2)])` spawns two threads and returns two promises in order. `await` on the resulting array blocks until all complete.

## ADR-037: FFI arity checked at compile time

**Decision**: The type checker validates arity and types at every call site to foreign bindings, using the declared `TypeExpr::Function(params, ...)` signature. Arity mismatches are compile-time errors.

**Rationale**: FFI arity errors must be caught early. The type checker has all the information needed and catches them in the same pass as regular function calls.

**Consequence**: `import foreign "lib.a"\n  val add: (Int32, Int32) => Int32\nadd(1)` produces a compile-time arity error rather than a link-time or runtime failure.

## ADR-039: Memory management — deterministic reference counting, cycles are user responsibility

**Decision**: Lin uses deterministic reference counting (RC) for all heap-allocated values (strings, arrays, objects, closures). RC operations are inserted by the compiler; the runtime provides `lin_string_release`, `lin_array_release`, `lin_object_release`, and `lin_closure_release`. Release functions recurse into heap-typed elements/values so that nested structures are freed correctly. Reference cycles between heap objects are **not** detected and will leak — this is a documented limitation.

**Rationale**: RC is deterministic (no GC pauses), predictable, and systems-friendly. The Perceus approach (Reinking et al., PLDI 2021, used in Koka and Lean 4) shows that compile-time linearity analysis can elide most RC operations, making the overhead negligible for common functional-style code. Cycle detection requires either programmer annotations (`Weak<T>`, as in Swift/Rust) or a runtime trial-deletion pass (as in Nim ORC). Both add complexity. Cycles are uncommon in the data pipeline / request handler patterns Lin targets. The tradeoff is acceptable: correctness for acyclic data (the common case), documentation for the cycle edge case.

**Consequence**: Programs must not create reference cycles between long-lived heap objects if they care about memory usage. The typical fix is to break cycles by setting a field to `Null` before the data becomes unreachable. Future work: `Weak<T>` type (Option B) or ORC-style trial deletion (Option C) can be layered on top without changing the base RC contract.

## ADR-038: Optional `else` in `if` expressions — implicit `else null`

**Decision**: The `else` branch of an `if` expression is optional. When omitted, the parser synthesizes `Expr::NullLit` at the `if` expression's span as the implicit else branch. The type checker then unions the then-branch type with `Null`, yielding `T | Null` as the expression's type.

**Rationale**: Side-effect-only patterns like `if cond then push(arr, item)` are idiomatic and common in the stdlib. Requiring `else null` is pure noise in these cases — the intent is clear and the result is always discarded. The `else null` pattern also appeared in predicate-style code (`if found == null && f(item) then found = item else null`) where the explicit null was a placeholder with no meaning. Synthesizing `NullLit` at parse time means the AST shape is unchanged — no `Option<Box<Expr>>` needed in `Expr::If` or anywhere downstream.

**Consequence**: The result type widens to `T | Null` when `else` is absent. Code that uses the result of an `else`-less `if` without handling the `Null` case will pass type-checking silently (the union just grows). This is an acceptable tradeoff: the common case (result discarded) gets cleaner syntax, and the footgun (accidentally using a `T | Null` result as `T`) is the same class of error already present whenever any function returns `Null`.

## ADR-040: Formatter does not preserve comments

**Decision**: The `lin fmt` formatter (`lin-parse/src/formatter.rs`) does not preserve source comments. Comments are stripped by the lexer and are not represented anywhere in the AST. When a file is formatted, all comments are lost.

**Rationale**: Adding comment-preservation would require either (a) threading comment tokens through the AST — significant structural change with no benefit to the compiler — or (b) a separate comment-reattachment pass that heuristically associates comments with nearby AST nodes based on source positions. Both approaches are complex and fragile. The formatter's primary use case (CI canonicality checks, auto-formatting on save) does not require comment preservation. Users who care about comments should commit before formatting.

**Consequence**: Running `lin fmt` on a file that has comments will silently drop them. This is documented behaviour. Future work: a comment-preserving pass that uses `Span` information to reattach comments to the nearest following AST node.

## ADR-041: Default argument values — trailing-comma inversion + per-arity adapters

**Decision**: A parameter may carry a default value (`(a: Int32, b: Int32 = a + 1)`). Optional parameters must be last. Because Lin already gives "supply fewer arguments than declared" a meaning — left-to-right partial application (spec §10.2) — and default values want the *same* call shape to mean "call now, fill the rest from defaults", the two are disambiguated at the call site by an **explicit trailing comma**: `f(x,)` partially applies; `f(x)` is a complete call that fills any omitted trailing defaults (and is an error if an omitted parameter has no default). This inverts the previous rule, where bare under-application curried. `Type::Function` gains a `required: usize` field (count of non-defaulted leading params), excluded from structural compatibility but serialized into module signatures so importers can check arity. Defaults are filled by the **defining** module, not the caller: for a function with optional params, lowering synthesizes one **adapter** per shortfall arity (`f$default{k}`) that binds the omitted parameters to their default expressions and calls the real function. Static calls (direct, dot, imported-by-symbol) route to the adapter by name/id. For the first-class-value path (`val g = f; g(x)`), each default-bearing function gets a static **descriptor** (`{ total, required, entries[] }` of boxed-ABI wrappers) stored at closure offset 32; an indirect under-arity call dispatches through it. The closure struct grew from 32 to 40 bytes (all closures, uniformly, so the runtime frees a single fixed layout); the descriptor is a never-freed static global.

**Rationale**: Synthesizing adapters as `TypedExpr::Function` and lowering them through the normal function path means RC, coercion, and earlier-parameter/chained default references (`(a, b = a + 1, c = b + 1)`) all work for free — defaults are just ordinary expressions evaluated in a scope where the preceding parameters are bound. Filling defaults in the defining module (rather than serializing default *expressions* into `.sig` files for callers to inline) keeps signatures small and makes cross-module defaults work by symbol reference. The trailing-comma marker resolves the currying/default-fill ambiguity at the exact site where intent lives, with zero new tokens. Putting `required` in `Type::Function` but excluding it from compatibility means default-ness never blocks an assignment or argument match — a `(Int32, Int32) => Int32` value is interchangeable whether or not its second parameter had a default.

**Consequence**: Existing code that relied on bare under-application to curry (e.g. `add(10)`) must add a trailing comma (`add(10,)`); within this repo only one example needed migration. The closure ABI change (32→40 bytes) touches every closure allocation site and `lin_closure_release`; all are updated together. A self-recursive *default-fill* tail call cannot use the TCO fast path (it targets a different-arity adapter), so it lowers as an ordinary call. Implementing the indirect path surfaced and fixed a pre-existing bug in the boxed-ABI wrapper: it inferred the Lin return type from the LLVM return kind and treated every pointer return as already-boxed Json, so a function value returning a raw `String`/`Array`/`Object` crashed the indirect caller (which unboxes); the wrapper now takes the real Lin return type and boxes correctly.

## ADR-043: Async concurrency — copy-by-default RC, catchable faults at the thread boundary

**Decision**: Turning the synchronous async stub into real OS-thread concurrency (spec §32) is gated on three model decisions, locked in here (see `docs/ASYNC_DESIGN.md` for the full plan):

1. **RC under threads = Option C (transfer by deep copy) by default, plus two opt-in shared types `Shared<T>` and `Frozen<T>`.** Refcounts stay non-atomic on the single-threaded hot path. Values crossing a thread boundary (a thunk's captured env, and the transferable result returned through a promise) are **deep-copied** so each thread owns a private, disjoint object graph — nothing is shared, so non-atomic RC is sound. The set of boundary-crossing values is exactly the transferable types (JSON-shaped, acyclic, no `Function`/`Iterator`/cycles — already enforced by the checker), so a deep copy is total and bounded. `Shared<T>` (atomic-RC box + `RwLock`, accessor-only, copy in/out) is the escape hatch for shared *mutable* state; `Frozen<T>` (immortal deep-frozen graph, zero-copy lock-free reads via mutation-inference coercion) for shared *read-only* state. Atomic-RC-everywhere (Option A) and dynamic shared-flag RC (Option D) and COW are rejected (§2.3, §2.3.3) — they tax the non-threaded hot path we just optimised.

2. **Catchable faults via a thread-local async-boundary flag.** A runtime fault (`lin_panic`, array OOB, division by zero, non-exhaustive match, null-spread) historically called `std::process::exit(1)` — uncatchable, correct at the top level (spec §19.1). All such sites now route through `crate::fault::runtime_fault(msg)`: inside an async boundary (thread-local depth > 0) it `panic!`s and unwinds to the boundary's `catch_unwind` (becoming an `Error` at `await`, spec §32.2.2); outside, it keeps the `process::exit(1)` behaviour. The spawned thunk runs inside `fault::with_async_boundary`. `lin_exit` (user `exit()`) is unaffected — intentional termination stays a real exit.

3. **`nounwind` is dropped program-wide when the program uses async.** User-emitted Lin functions are marked `nounwind` (sound: value-based errors, frames never unwind) — but a fault inside a thunk now unwinds *through* Lin frames to the boundary, so `nounwind` is unsound for any function reachable from a thunk. We cannot cheaply prove a given function is unreachable from a thunk, so codegen conservatively drops `nounwind` from all user functions whenever the program references any concurrency intrinsic (detected in `lin-compile` by scanning every module's intrinsic map for the `lin_async`/`lin_parallel`/`lin_worker`/… family, which is reachable only through `std/async`). The overwhelmingly common non-async program keeps `nounwind` and its optimisation value (doc §2.4.3 option a).

**Rationale**: The spec's correctness-by-construction guards (`var`-capture ban, transferable-only returns) were designed anticipating threads — they guarantee a thunk shares only immutable, JSON-shaped, acyclic data with its parent, which is exactly what makes Option C's deep copy total and keeps the single-threaded path atomic-free. Catchable faults are the entire point of `async` being Lin's fault-isolation boundary; routing every fault through one helper that branches on a thread-local keeps the top-level `exit` semantics intact while making thunk faults recoverable. The runtime is `panic = "unwind"` (unchanged), so `catch_unwind` works and unwinding crosses the LLVM/Rust boundary; the only requirement is that the Lin frames in between are not `nounwind`, hence decision 3.

**Consequence**: Programs that use async pay a small code-size/optimisation cost (no `nounwind` on user functions) — measured negligible, and zero for non-async programs. Deep-copying large transferable results at a boundary is the cost of Option C; `Shared<T>`/`Frozen<T>` are the escape hatches so we are never forced into all-atomic RC. `Shared<T>` reintroduces deadlock and RC-cycle hazards (documented); `Frozen<T>`'s immortal graphs are never freed (load-once data only). A genuine (non-fault) panic inside a thunk is also caught and surfaced as an `Error` — acceptable, since a runtime bug in a worker should isolate to that worker rather than abort the process. (Implementation note, post-merge with Rust 1.81+: a panic must not unwind out of a plain `extern "C"` runtime fn — the faulting runtime functions and the thunk-call transmutes are `extern "C-unwind"`, and async-reachable Lin frames get `uwtable` so the unwinder can walk through them.)

## ADR-042: All call paths must coerce arguments to parameter types

**Decision**: Every call-lowering path in `lower_call` (`lin-ir/src/lower.rs`) coerces each argument to the callee's declared parameter type via `lower_call_arg` (which boxes a concrete value to `Json`/`TaggedVal*` when the parameter is union/Json) and retains heap arguments via `retain_call_arg`. This includes the fallback **indirect-call path** — a call through a closure *value* (`val f = ...; f(x)`, a closure passed as a parameter, or any non-statically-resolved callee) — which previously lowered its arguments with a bare `lower_expr` and no coercion.

**Rationale**: Lin's uniform closure ABI passes `Json` parameters as boxed `TaggedVal*`. The named-function and imported-function paths already box concrete arguments (an `Array`, `Object`, or scalar) to match a `Json` parameter; the indirect path is just another way to reach the same ABI and must follow the same rule. The callee's parameter types are read from the callee expression's `Type::Function` signature, identically to the other paths.

**Consequence**: Fixes silent data corruption — before this, an `Array` (or any heap value) passed to a `Json`-typed closure parameter reached the callee as a raw `LinArray*` instead of a boxed `TaggedVal*`. The callee read its tag/payload from garbage, so the value behaved as a different (or empty) object and *mutations through it were lost* (e.g. `push` into an accumulator passed to a stored closure left the original array empty). This is the argument-side analog of the return-side boxing bug noted in ADR-041; together they make the first-class-function/closure path representation-correct for all heap types. Regression: `test_array_passed_to_closure_value_mutates` in `crates/lin/tests/integration.rs`.

## ADR-043: Line-leading `[`/`(` is a new statement, not a postfix index/call

**Decision**: Inside an inline lambda body (a `() => ...` body with no `Indent`, parsed by `parse_inline_block`), a `[` or `(` that begins a new source line starts a NEW statement (an array literal, or a parenthesised expression) rather than continuing the previous expression as an index or call. The lexer records, per token, whether a source newline precedes it (`Token::newline_before`) — set in a post-tokenize pass that scans the gap between consecutive token spans — and `parse_postfix_expr` suppresses the `LBracket`/`LParen` postfix arms when `at_line_start()` is true.

**Rationale**: Inside `()`/`[]`/`{}` the lexer suppresses newline tokens entirely (ADR-004), so the parser otherwise has no signal that a `[` opens a new line, and its postfix loop greedily reads `expr \n [ ... ]` as `expr[...]`. This made a line-leading array literal after a statement (the natural way to return a list of values from a multi-statement inline body) silently parse as an index into the preceding expression. The `newline_before` flag recovers the suppressed line break without re-introducing block-structuring newlines into delimited spans. This mirrors the existing post-`Dedent` suppression of postfix `[`/`(` at top-level block boundaries (ADR-011) — same intent, applied where the boundary is a suppressed newline rather than a Dedent.

**Consequence**: `std/test` bodies that do setup then return assertions can use the natural form — `val xs = f(); push(xs, y); [ expect(...).toBe(...) ]` — instead of binding the array to a throwaway `val checks` just to avoid the index-gluing. Same-line indexing (`arr[0]`) and same-line/continuation method chains (`x.map(...)\n  .filter(...)`) are unaffected: the postfix `.` arm is not gated on `at_line_start`, and a same-line `[`/`(` has no preceding newline. Multi-line dot chains assigned through an inline-body `val` (`val r = xs\n  .map(...)`) remain a separate pre-existing inline-body limitation, unchanged by this ADR.

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
value (spec §19) and the exact object the async runtime builds when a thunk faults
(`{ "type": "error", "message": <msg> }`). `is Error` (and any `is <ObjectShape>`) lowers to a
**field-presence** check (`HasPattern` on the object's keys) rather than a bare tag check, so it
matches error-shaped objects specifically instead of every object. This makes the spec's §32.2.2
pattern work:

```txt
match await(p)
  is Error => print("failed: ${result}")
  else     => use(result)
```

**Rationale**: `Error` has no special control-flow behaviour (§19), so a structural object type is
the faithful model — it composes in unions and narrows by shape. Routing object-shaped `is`
checks through the existing `HasPattern` machinery reuses the same field-presence test as
`is { .. }`, with no new runtime support.

**Consequence (deferred)**: The other half of §32.2.2 — `async` wrapping its result as
`Promise<T | Error>` so the checker **rejects using an uninspected `Error` as a plain `T`** — is
**not implemented**, and is not a localized change. The entire async surface is `Json`-typed
through the stdlib wrappers (`async = (f: Json): Json`, `await = (p: Json): Json`); there is no
parametric `Promise<T>` tracking (there never was — the synchronous stub was `Json`-typed too).
`await(p)` therefore returns `Json`, which coerces freely to any type, so the "reject uninspected
Error" rule cannot be enforced without first making `async`/`await` **generic over the thunk's
return type** — a parametric-opaque-type feature spanning the checker's inference, the intrinsic
signatures, and module signatures. That is its own project; until then `is Error` gives users the
*runtime* discrimination the spec intends, and a fault is always a well-formed `Error` object,
just not statically forced to be handled. Likewise §32.2.3 nested-promise auto-flatten IS now
implemented (runtime: `await` recurses through a `TAG_PROMISE` result).

## ADR-047: Logical `!` as the second unary operator

**Decision**: Add a prefix logical-not operator `!` (e.g. `if !ready`, `match ... when !cond`, `val x = !flag`). It sits at the same precedence level as bitwise `~` — tighter than `*`, looser than postfix — and is right-associative, so `!!x` parses as `!(!x)` and `!a == b` parses as `(!a) == b`. Both its operand and its result are `Bool`. A non-`Bool`, non-`TypeVar` operand is a compile-time error. Negated *patterns* (e.g. `is !true`) are explicitly out of scope.

**Rationale**: The previous spec stated `~` was the only unary operator and that boolean negation had to be written `ready == false`. That boilerplate appeared throughout the stdlib (`std/array`) and user code. `!` removes it. Implementation is cheap because it reuses the existing unary pipeline end-to-end: the lexer emits a new `Bang` token, the AST gains `UnaryOp::Not`, and IR lowering maps `UnaryOp::Not` to the same `crate::ir::UnaryOp::Not` as `~` — for an `i1`, a bitwise-not *is* a logical-not, so codegen's existing `build_not` arm needs no change. When the operand is not statically `Bool` (e.g. a boxed `TypeVar` flowing through a generic lambda), the lowering routes it through `lower_cond_as_bool` first to unbox/coerce to a raw `i1` before the `Unary` instruction.

**Consequence**: This supersedes the prior "the only unary operator is `~`" statement in §3.7, §24.1, and §35.2 of the specification. The language now has exactly two unary operators (`~` bitwise, `!` logical); there is still no unary minus.

## ADR-048: `Json` is a covariant sink — closing the Json→concrete cast hole

**Decision**: `Json` (modelled as `Type::TypeVar(u32::MAX)`, see `types.rs::is_json`) is made a **covariant sink**: anything is assignable *into* `Json` (concrete `T → Json` stays allowed, so `writeJson(value: Json)` and the pervasive "store anything as Json" patterns keep working), but a `Json` *value* is **not** assignable *out* to a fully-concrete **structured object** target — one that (after unfolding `Named` types) is an `Object` with at least one required, non-nullable field. So `val p: Person = readJson(...)` (where `Person = {name: String, age: Int32}`) is now a type error; the value must be decoded via `fromJson` (ADR-047) or narrowed via `is`/`has`. The fix splits the old blanket `(_, TypeVar(_)) | (TypeVar(_), _) => true` arm in `compat.rs` into: (1) `(_, TypeVar(MAX)) => true` (sink), (2) `(TypeVar(MAX), target) => lenient_json || !requires_structured_decode(target)`, (3) the existing permissive arm for all *other* (non-MAX) TypeVars — genuine inference vars, the `9000+` generic slots, intrinsic vars — so inference is unchanged. `requires_structured_decode` deliberately treats only required-field objects as the hazard: `Json` flowing into scalars (`Int32`/`Int64`), buffers (`UInt8[]`), opaque handles, open objects (`{}`), arrays, functions, iterators, or anything still containing a TypeVar stays permissive — those are the language's handle/buffer/polymorphic-return patterns, which have no `fromJson` remedy and predate this change.

**Rationale**: The old rule made `Json` bidirectionally compatible with *everything*, so a value read from a `Json` source could be silently bound to a richly-typed annotation with **zero validation** — the annotation was a lie. Drawing the line at required-field objects catches the real "I claimed this JSON is a `Person`" hazard (the one a decoder can fix) while not breaking the thousands of existing scalar/handle/buffer flows. The leniency is scoped: a per-`Checker` `lenient_json` flag is set **only** for the trusted embedded stdlib (whose wrappers forward `Json` handles into concrete intrinsic/foreign params by design, e.g. `lin_parse_json`, `pathMatch`); user modules and user-defined imported modules always check strictly. After this change the only sound `Json → T` conversions are (a) `fromJson` (validated decode) and (b) `is`/`has` narrowing (a separate `checker/pattern.rs` path that branches on `ty.is_json()` directly and is backed by runtime tag checks, so it stays sound and is unaffected).

**Consequence**: The predicted blast-radius migration sites (`pathMatch`'s `String` params, `lin_parse_json`'s `String` param) did **not** in fact break: their targets are scalars, not required-field objects, so the narrower `requires_structured_decode` rule leaves them permissive — no stdlib widening was needed. The whole stdlib, example suite, and test suite compile unchanged. The scalar/handle escape hatch is a deliberate soundness gap (`Json → Int32` is still unchecked) accepted to avoid a disruptive migration; tightening it later (e.g. a `fromJson` for scalars) is additive. `lin build` of `val p: Person = readJson(...)` (and the direct `val p: Person = jsonReturningCall()` form) now surfaces `Expected type {...}, got ?T4294967295`; the remedy is `Person.fromJson(...)`. (Note: `lin check` of a *single* file leaves imported functions' return types as fresh inference vars rather than `Json`, so the gate cannot fire there for an imported call — the full `lin build` pipeline, which resolves import signatures, is the authority. A bug where a zero-param or all-`Json`-param `Json`-returning function was misclassified as the opaque `Function` annotation — and its `Json` return freshened into a permissive inference var, slipping the gate — was fixed in `infer_call` by requiring the opaque shortcut to have a *non-empty* all-`TypeVar(MAX)` param list.)

**Scope decision — total vs structured (empirically locked)**: a *total* gate (rejecting ANY `Json → concrete T`, including scalars and arrays) was implemented and run against the full suite. It broke (a) the stdlib's pervasive **polymorphic-return idiom** — `slice`/`concat`/`accept`/`wait`/etc. return `Json` and are routinely assigned to concrete `val`s (`val sub: UInt8[] = slice(bytes, 1, 4)`, `val code: Int64 = wait(pid)`, socket `accept(): Int32`) — and (b) **`is`-narrowing into a concrete branch** (`if j is String then j else ""`, whose narrowed value is still statically `Json`). Empirically: `test_is_narrowing_still_works`, `test_slice_preserves_element_type`, `test_net_tcp_loopback_echo`, `test_net_udp_loopback_roundtrip`, `test_proc_spawn_read_wait`, `test_proc_wait_exit_code` all failed under the total gate. These patterns have **no `fromJson` remedy** and forcing one is hostile, so the total scope was rejected and the gate is **scoped to required-field structured objects** — the genuine "unchecked object decode" hazard. This matches the structured-object-only conclusion anticipated in the plan.

## ADR-049: `fromJson` — type-directed JSON decode (descriptor-driven runtime interpreter)

**Decision**: `T.fromJson(json)` (and the equivalent `fromJson(T, json)`) is a **checker special form** that validates a `Json` value against the target type `T` and yields `T | Error`. It is recognised by the surface name `fromJson` at the call site (intercepted in both `infer_call` and `infer_dot_call` *before* arg0/receiver is inferred as a value, since arg0 is a *type*, not a runtime value — so unlike `print`/`for` no `lin_*` wrapper can express it). `std/json` exports a `fromJson` stub purely so the import resolves and `lin check` sees the name; the stub body is never used for real call sites. Validation is implemented as a single generic runtime interpreter `lin_from_json(value, descriptor)` driven by a compact, position-relative byte **descriptor** emitted per call site by codegen (`DescEncoder`), so emitted code is O(1) per site and recursion/cycles are finite back-edges (memoised by named type). The interpreter walks value+descriptor in lockstep, returns the input **cloned** (`lin_tagged_clone`, +1 independently owned) on success or a fresh `Error` on the first structural mismatch, building a JSONPath-ish `path` (e.g. `$.address.city`) during the walk. `Error` is a structural object alias `{ "type": "error", "message": String }` (resolved by `resolve_named_cycle`, not a new `Type` variant — cf. ADR-044), and the runtime error value also carries a `"path"` field, which width subtyping permits.

This ADR records three load-bearing semantic choices and their trade-offs:

- **(a) Union variant selection is FIRST-MATCH-WINS.** A `KIND_UNION` node tries each variant in declaration order and accepts the first that validates. **Trade-off**: for overlapping, non-discriminated object variants the most-permissive / first-listed variant *shadows* more-specific ones — e.g. with `{ "k": String } | { "k": String, "w": Int32 }`, an input that has both `k` and `w` matches the first variant. The *runtime data is fully preserved* (the same value is returned, no fields are dropped), but the *static type* the program reasons about is the matched variant, which may be the wider one. **Recommendation**: give union variants a discriminant field (e.g. a literal `"type"` tag) so exactly one variant matches; list more-specific variants first when overlap is unavoidable. First-match-wins was chosen for v1 because it is predictable, order-explicit, and matches the spec's first-error policy spirit; a "best/most-specific match" rule would be ambiguous and costly.

- **(b) Number policy is target-type-driven.** An **integer** target requires the JSON number to be integral and within the target's width/signedness range (a float like `3.14` is rejected; an integral float like `5000000000.0` against `Int32` is rejected as out of range). A **float** target (`Float32`/`Float64`) accepts any JSON number. An **unconstrained** target (`Json`/a `TypeVar`, encoded as `KIND_JSON`) accepts any number as-is with no narrowing — number-range validation is intentionally skipped there by design. (Note: a bare suffixless integer *literal* in Lin source is typed `Int32` and truncated by the lexer per spec §26 *before* it can reach `fromJson`, so genuine out-of-range integers arriving from real JSON parsing are the cases the range check guards.)

- **(c) `Error` is a structural object alias, but `is Error` IS made to discriminate.** `Error` is `{ "type": String, "message": String }` (open, resolved by `resolve_named_cycle`; the runtime value also carries `"path"`). A *bare* tag check would match any object, so to make the agreed idiom `match result | is Error => .. | is Person => ..` work, **`is Error` is desugared in the checker (`check_pattern`, `Pattern::TypeName == "Error"`) into the value-constrained object pattern `{ "type": "error", "message": _ }`.** This reuses the existing object-pattern lowering (`lower_object_pattern_test`) which checks field presence AND `scrut["type"] == "error"` at runtime — exactly what distinguishes a decode failure (always `"type": "error"`) from a decoded value (any other shape). Standalone `Expr::Is` was routed through the same object-pattern path (its old `IsType` lowering mapped an object pattern to `Type::Never`/tag `0xFF`, which never matched). Exhaustiveness was taught to count this desugared pattern as covering the `Error` union variant. Chosen over adding a dedicated `Type::StrLit` literal-type (which would touch ~20 exhaustive `Type` matches across codegen/boxing/representation — too invasive, cf. ADR-044) and over a new `Type` variant. **Former residual trade-off, now RESOLVED by ADR-050:** when this ADR was written, the standalone expression form `result is Person` compiled to a bare `TAG_OBJECT` check, so it *also* matched the Error object and the `is Error` arm had to come first or a decode failure would route into the `Person` arm and fault on `result["name"]`. ADR-050 makes `is <ObjectType>` check the target's required fields in **both** the match-arm and expression paths, so `is Person` no longer matches an Error object and the arm order is no longer load-bearing. The `result["type"] == "error"` discriminant still works and remains valid for code that prefers it.

**Rationale**: A descriptor + one generic interpreter (validator strategy C) beats inlining per-site LLVM (code bloat, recursion needs emitted helpers) and per-type generated functions (still heavy IR, forward-decl cycle handling): it keeps generated code tiny, reuses the existing tag/unbox runtime primitives, makes recursion trivial (table indices), and makes the walker ordinary, unit-testable Rust. Returning a **clone** rather than the same retained pointer (a deviation from the original plan note) keeps ownership symmetric: the `fromJson` result is registered `+1 owned` in lin-ir, and the input value temp is independently owned and released by normal liveness — returning the *same* pointer would alias two owners and double-free. Verified under AddressSanitizer: no use-after-free / double-free; the decode-`Error` builder (`make_decode_error`) releases its locally-created key/value strings after `lin_object_set` retains them, so error values are leak-clean (only the program-lifetime interned string-literal cache leaks, as elsewhere).

**Consequence**: The input `Json` is **borrowed** (never consumed); the result is unconditionally a fresh `+1`-owned value (clone on success, fresh `Error` on failure); the descriptor is a static const global (never freed). Array/fixed-array/object/union targets can only be named via a `type` alias or built-in name because the special form requires arg0 to be a bare identifier (`Int32[].fromJson(...)` does not parse; use `type IntArr = Int32[]`). A user who shadows `fromJson` with a *local* binding defers to the normal call path; a global user `fromJson` called with a real type-name arg0 would still be intercepted (an accepted, low-risk corner). `Iterator`/`Function`/`Never`/`TypeVar` target fields encode as `KIND_JSON` (accept-any), since they are not JSON-shaped and have no meaningful structural check.

## ADR-050: `is <ObjectType>` checks required fields (in both match-arm and expression forms)

**Decision**: `x is Person` (where `Person` resolves to an object type with at least one field) checks at runtime that `x` is an object **and has all of `Person`'s required field keys present** — not merely that `x` carries the `TAG_OBJECT` tag. This holds uniformly in both lowering paths: the match-arm path (`lower_match_pattern`) already emitted a `HasPattern` field-presence check for `Is(TypeCheck(Object(fields)))`; this ADR adds the *same* arm to the standalone-expression path (`TypedExpr::Is` in `lower.rs`), which previously fell through to a bare `IsType` tag check. An empty object type (`{}`) still degrades to the tag check (nothing to require).

**Rationale**: A bare tag check made `is <ObjectType>` unsound. `is Person` matched *any* object — including a `fromJson` decode error `{ "type": "error", ... }` or an arbitrary `{ "foo": "bar" }`. Because the matched arm then statically **narrows** the binding to `Person`, a subsequent `x["name"]` is typed `String` and compiled through the non-null-safe field path; on an object actually lacking `name`, `lin_object_get` returns null and string interpolation null-derefs (`lin_string_concat`), crashing. The narrowing was a lie the runtime did not enforce. Mirroring `is { .. }` / `has` field-presence checking closes the hole with the existing `HasPattern` → `lin_value_has_field` machinery — no new instruction, no new runtime primitive.

**Consequence**: This **supersedes the ADR-049 arm-ordering rule**: `is Person` no longer matches an `Error` object (an `Error` has `type`/`message`, not `name`/`age`), so `match | is Person => .. | is Error => ..` is now sound in either order. Checking is **field-presence only**, not recursive field-*type* validation: `is Person` on `{ "name": 1, "age": "x" }` (both keys present, wrong value types) still matches — full structural+type validation is what `fromJson` (ADR-049) is for. This keeps `is` cheap (one `lin_value_has_field` per required key) and consistent with `has`/`is { .. }`, which are also presence checks. Width subtyping is preserved: extra fields on `x` don't prevent a match. Verified: a real `Person` matches; an object missing a required field does not; the former decode-failure crash is gone; full suite green (278 integration + 6 + 33 + 7 + 24).

## ADR-051: Closures OWN their captures (retain on capture, release on free)

**Decision**: A closure's environment now OWNS one reference per heap/union capture — the same ownership rule arrays and objects already follow for stored elements. At `MakeClosure` the lowerer takes ownership of each capturing value (concrete rc → `Retain` in place; union/`Json` → `CloneBox` so the env holds its own `TaggedVal*`); `lin_closure_release` releases them when the closure is freed. Mutably-captured `var` bindings are unchanged: they store the heap **cell pointer** (shared by reference, ADR-015) and keep their existing borrow-only / `FreeCell` / escaping-cell lifecycle — the env does not own the cell. Scalars need no ownership.

To let the runtime release captures, every closure carries a **capture descriptor** at closure offset 40 (`{ u32 count, u8 kinds[count] }`, a static read-only global): one `CaptureRelease` byte per capture (None / Str / Array / Object / Closure / Tagged). The closure struct grew from 40 to 48 bytes (`CLOSURE_SIZE`); `alloc_closure`/`store_capture_descriptor` centralise the layout, and `lin_closure_release` frees 48 and walks the descriptor (mirroring the recursive element release in `lin_array_release`/`lin_object_release`). Partial-application closures keep borrow semantics with a null capture descriptor. The async thread-transfer path (ADR-042) reads the same descriptor (passed explicitly from the closure, no longer stored at env offset 0) and reuses its codes.

**Rationale**: Captures were **borrow-only** — the env stored a borrowed pointer with no retain, and `lin_closure_release` freed nothing. That is sound only while a closure cannot outlive its captured values' scope. The `safe_callback_depth`/escaping-cell analysis covered that for mutable `var` cells, but immutable value captures had no ownership at all, so a closure that ESCAPES (e.g. returned from a `map`/`filter` callback into the result array) dangled: `map(xs, i => () => i)` then called a thunk returned garbage (`[[object]…]`) because the captured element box had been freed at end-of-iteration and its memory reused. Making the env an owning container, exactly like arrays/objects, fixes this uniformly and reuses the existing store-side discipline (`transfer_into_container` / `own_for_store`).

**Consequence**: `map(xs, i => () => i)` and any escaping capturing closure are now correct. **Performance is unchanged**: the added retain/release pairs are elided by the existing RC-elision pass for the overwhelmingly common non-escaping combinator-callback case (closures created and consumed in one scope) — a before/after benchmark (`benchmarks/closures.lin`, capturing `map`/`filter`/`reduce` over 2M elements) showed no measurable difference, and the full benchmark suite was flat. The cost lands only on closures that genuinely escape, where it is required for correctness. The closure struct is 8 bytes larger (40→48); `CLOSURE_SIZE` in `lin-codegen/src/codegen/call.rs` and the free size in `lin-runtime/src/memory.rs` must stay in lockstep. Verified under ASan (stdlib + every example-project test suite — heavy closure users — show no use-after-free or double-free; the only leak is the pre-existing exit-time top-level-`val` leak, identical for non-capturing `map`).
