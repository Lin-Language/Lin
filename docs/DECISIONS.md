# Architecture Decision Records

## ADR-001: Dynamic typing for v0

**Decision**: The v0 interpreter does not include a type checker. Types are parsed but not verified at compile time. Runtime type tags (on the Value enum) are used for `is`/`has` checks.

**Rationale**: A full bidirectional type system with generics, variance, and numeric widening is a multi-week effort. Dynamic execution lets us validate the language design and build a working demo overnight. Type checking can be layered on top later without changing the runtime semantics.

**Consequence**: All type annotations are parsed and stored in the AST but ignored at runtime.

## ADR-002: Minimal built-ins, stdlib for iteration

**Decision**: Only `for` and `iter` remain as Rust built-ins. Higher-level functions (`map`, `filter`, `reduce`, `range`, `iterOf`) are implemented in .lin stdlib files (`std/array`, `std/iter`) and preloaded as globals at interpreter startup.

**Rationale**: `for` needs `call_value` to drive the iterator state machine. `iter` constructs the opaque `IteratorValue` struct. All other iteration functions can be expressed in .lin using these two primitives — e.g., `map` calls `arr.for(item => ...)`. Since .lin supports higher-order functions (passing and calling function arguments), no special interpreter access is needed.

**Consequence**: ~120 lines of Rust removed. `std/iter` and `std/array` are loaded during `Interpreter::new()` via `preload_stdlib()`, making their exports available as globals without explicit imports. `range()` now returns an `Iterator` (lazy) rather than an `Array` (eager), which is transparent to consumers since all use `.for()`/`.map()`/etc.

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

## ADR-008: Environment cloning for global scope

**Decision**: Top-level statements evaluate by cloning `global_env`, evaluating in the clone, then writing back to `global_env`.

**Rationale**: Rust's borrow checker prevents holding `&mut self` (needed for `call_value` and module loading) and `&mut self.global_env` simultaneously. Cloning is O(n) in bindings but avoids unsafe code or RefCell wrapping of the entire interpreter.

**Consequence**: Performance cost is negligible for typical programs. A future version could use `Rc<RefCell<Env>>` for zero-copy sharing.

## ADR-009: Stdlib string functions as native intrinsics

**Decision**: `trim`, `toUpper`, `toLower` are implemented as Rust native functions (`__stringTrim`, etc.) exposed through .lin wrapper files.

**Rationale**: String manipulation requires access to Rust's `str` methods which cannot be expressed in lin itself. The .lin files provide the public API surface while the Rust code provides the implementation.

**Consequence**: The stdlib is a mix of .lin re-exports and Rust intrinsics, achieving the "thin runtime, fat stdlib" goal as much as possible given the language's constraints.

## ADR-010: Multi-line if/then/else with indent consumption

**Decision**: The parser consumes an INDENT token that may appear before `then`/`else` in multi-line if expressions, and matches a trailing DEDENT.

**Rationale**: When `if` condition is on one line and `then`/`else` are indented on subsequent lines, the lexer produces INDENT/DEDENT pairs. The parser must explicitly handle these to avoid confusing them with block boundaries.

**Consequence**: All three spec-defined if layouts (single-line, multi-line same indent, multi-line with block branches) parse correctly.

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

## ADR-027: Concurrency via OS threads in the tree-walking interpreter

**Decision**: `async(thunk)` spawns a real OS thread (`std::thread::spawn`) with a fresh `Interpreter::new()` instance. The thunk's `Rc<Function>` is converted to a raw pointer via `Rc::into_raw` (wrapped in `SendFunction(*mut Function)` with `unsafe impl Send`) to bypass `Rc`'s `!Send` bound. Results are communicated back via `Arc<Mutex<PromiseState>>`. `await` uses a spin-wait loop with `thread::yield_now()` until the promise resolves.

**Rationale**: A true async executor (tokio, async-std) would require cooperative `async/await` syntax throughout the interpreter, which conflicts with the simple tree-walking evaluation model. OS threads are heavyweight but correct: each thread gets its own interpreter state, so there is no shared mutable interpreter data between concurrent thunks. The `SendFunction` raw-pointer trick is safe because Lin's spec forbids `var` capture in async thunks (all captured values are immutable `val` bindings, deep-copied at lambda creation).

**Consequence**: `async` thunks run on true OS threads. `await` blocks the caller thread (not a coroutine yield). The `JsonValue` bridge enum serializes results at thread boundaries since `Value` contains `Rc<...>` fields that cannot cross thread boundaries. `ThreadPool` uses `mpsc::channel` with a fixed set of worker threads. `Worker` uses `mpsc::sync_channel` for backpressure.

## ADR-028: `SendFunction` raw-pointer safety invariant

**Decision**: `SendFunction(pub *mut Function)` stores the result of `Rc::into_raw(func)`. The receiving thread calls `unsafe { Rc::from_raw(ptr) }` exactly once to reconstruct the `Rc<Function>`. No other `Rc` clone of the same allocation may exist after `SendFunction::new()` is called.

**Rationale**: `Rc<T>` is `!Send` because clone/drop modify a non-atomic reference count. By consuming the `Rc` into a raw pointer (decrements no refcount, transfers ownership), and reconstructing it on exactly one other thread (where no other clones exist), we avoid all data races on the refcount. The raw pointer itself (a plain address) satisfies `Send`.

**Consequence**: Callers of `SendFunction::new` must ensure no clones of the passed `Rc<Function>` survive in the sending thread after `new()` returns. In practice, the thunk function is always obtained from a `Value::Function(rc)` match arm, and the `Value` is not retained afterward, satisfying this invariant.

## ADR-029: JSON bridge type for cross-thread value transfer

**Decision**: `JsonValue` is a `Clone + Debug` enum (no `Rc`, no `RefCell`) that mirrors Lin's data types: `Null`, `Bool`, `Int`, `Float`, `String`, `Array`, `Object`, `Error`. `Value::to_json_value()` converts at the thread boundary (returning `Err` for non-serializable types like `Function`). `JsonValue::to_value()` converts back in the receiving thread.

**Rationale**: `Value` contains `Rc<RefCell<...>>` for arrays and objects, which cannot be sent across threads. Instead of adding `Arc` alternatives, a separate bridge type that is fully `Clone + Send` provides a clean serialization point. This also enforces Lin's spec requirement (§32.4) that async thunk return types must be JSON-compatible.

**Consequence**: Closures, iterators, promises, workers, and thread pools cannot be returned from async thunks (they fail `to_json_value()` with an error). Deep copies are made at the thread boundary. For large objects this is O(size) but is unavoidable given the `Rc`-based value representation.

## ADR-030: IO/FS/HTTP implemented as native intrinsics with interpreter dispatch

**Decision**: IO (`__ioReadLine`, `__ioReadAll`, `__ioLines`), filesystem (`__fsReadFile`, `__fsWriteFile`, `__fsAppendFile`, `__fsReadLines`, `__fsExists`, `__fsReadJson`, `__fsWriteJson`), HTTP client (`__httpFetch`, `__httpFetchWith`), JSON parsing (`__parseJson`), and HTTP server (`__serverServe`, `__serverServeWithPool`, `__serverPathMatch`) are all implemented as Rust native functions registered in `register_intrinsics`. Functions that return interpreter-managed values (`__ioLines`, `__fsReadLines`, `__serverServe`, `__serverServeWithPool`) are registered as stub stubs and dispatched through `call_value`'s special-name dispatch, following the same pattern as the concurrency builtins.

**Rationale**: The `NativeFn` type is `fn(&[Value]) -> Result<Value, String>` — a bare function pointer with no captures. IO operations that need complex state (file handles, stdin), the thread pool reference (for pool serve), or interpreter method calls (for calling the handler) cannot be expressed as a `NativeFn`. The call_value dispatch table (a `match name.as_str()` inside `NativeFunction` handling) provides a clean escape hatch for these cases, consistent with how `print`, `for`, `iter`, and the concurrency builtins work.

**Consequence**: All IO/FS/HTTP is synchronous on the calling thread (no internal async). Programs can run IO in background threads via `async`/`threadPool`. The HTTP server blocks forever on the calling thread; typical usage is `async(() => serve(8080, handler))`. `tiny_http` was chosen as the server crate for its simplicity and zero-dependency feel (no tokio runtime needed).

## ADR-031: `std/io`, `std/fs`, `std/http`, `std/server` as thin Lin wrappers

**Decision**: Each IO module is a `.lin` file (`stdlib/io.lin`, `stdlib/fs.lin`, `stdlib/http.lin`, `stdlib/server.lin`) that re-exports `__*` intrinsics with clean names and provides Lin-level helpers (`fetchJson`, `postJson`, `json`, `text`, `parseBody`, etc.). They are registered via `include_str!` in `register_stdlib_sources` and loaded on demand when the user imports `std/io`, etc.

**Rationale**: Following the existing pattern (ADR-009): keep the Rust intrinsics small and focused; provide the user-facing API in Lin. This means helpers like `fetchJson` (fetch + parseJson) and `pathMatch` routing can be written in Lin without touching Rust. The stdlib files are compiled once per interpreter session and cached by the module loader.

**Consequence**: Users get `import { readFile, writeFile } from "std/fs"` etc. The intrinsics are not exported but are accessible as globals (registered in global env), so advanced users can call `__fsReadFile(path)` directly.

## ADR-032: FFI syntax as `import foreign "<path>"` with indented type block

**Decision**: Foreign function imports use `import foreign "<path>"` followed by an indented block of `val name: Type` declarations. The `foreign` keyword is added to the lexer. The parser reuses the existing indented-block machinery. The AST node is `Stmt::ForeignImport { path, bindings: Vec<ForeignBinding> }`. Each `ForeignBinding` carries the name, type annotation, and span.

**Rationale**: Reusing `import` as the outer keyword makes foreign imports visually consistent with regular imports. The `foreign` keyword distinguishes them syntactically without introducing a separate statement form. The indented block mirrors function body parsing (ADR-014) and keeps all bindings visually grouped under the library path.

**Consequence**: `import foreign "libmath.a"\n  val sqrt: (Float64) => Float64` parses correctly. The token `foreign` is now a reserved keyword and cannot be used as an identifier.

## ADR-033: FFI interpreter stubs; full FFI requires the compiler

**Decision**: In the tree-walking interpreter, each foreign binding is registered as a zero-arity `NativeFunction` stub that returns `Err("Foreign functions are not available in the interpreter; use \`lin build\` to compile")`. The codegen path emits real LLVM `declare` directives and stores the library paths in `Codegen::foreign_lib_paths` for the linker.

**Rationale**: The interpreter cannot load `.a` or `.so` files at runtime without libffi or dlopen, which would add significant complexity. Since the primary value of FFI is in compiled binaries (for performance-critical C interop), stubs in the interpreter are sufficient for development testing. The interpreter stub gives a clear error message rather than a cryptic segfault.

**Consequence**: `lin run` programs with `import foreign` can be loaded and type-checked but foreign functions will panic if called. `lin build` programs can call C functions correctly after linking. End-to-end FFI tests require the full `lin build` pipeline.

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

## ADR-036: `async(array)` overload in interpreter

**Decision**: `builtin_async` detects `Value::Array` as its input and spawns one thread per element, returning an array of `Promise` values. This mirrors the type-level overload already modelled in the checker (`async` accepts `Function | Function[]`).

**Rationale**: `await(async([thunk1, thunk2, ...]))` is the natural idiom for fork-join concurrency. Without the array overload, users would need to call `async(thunk)` individually and collect results manually. The overload also enables the `test_concurrent_async_fetchjson_no_data_races` test.

**Consequence**: `async([() => fetch(url1), () => fetch(url2)])` spawns two threads and returns two promises in order. `await` on the resulting array blocks until all complete.

## ADR-037: FFI stub arity derived from declared type

**Decision**: When the interpreter registers an `import foreign` binding as a stub, it reads the declared `TypeExpr::Function(params, ...)` to determine the arity, instead of hardcoding `arity: 0`. This allows call-site arity checks to pass during interpreter execution, so the stub's error message is reached instead of "Too many arguments".

**Rationale**: The interpreter stub is meant to give a clear diagnostic when FFI functions are called. If the arity doesn't match the declared signature, the interpreter would error before reaching the stub's message, creating confusing diagnostics.

**Consequence**: `import foreign "lib.a"\n  val add: (Int32, Int32) => Int32\nadd(1, 2)` now correctly produces "Foreign functions are not available in the interpreter" rather than "Too many arguments".

## ADR-038: Optional `else` in `if` expressions — implicit `else null`

**Decision**: The `else` branch of an `if` expression is optional. When omitted, the parser synthesizes `Expr::NullLit` at the `if` expression's span as the implicit else branch. The type checker then unions the then-branch type with `Null`, yielding `T | Null` as the expression's type.

**Rationale**: Side-effect-only patterns like `if cond then push(arr, item)` are idiomatic and common in the stdlib. Requiring `else null` is pure noise in these cases — the intent is clear and the result is always discarded. The `else null` pattern also appeared in predicate-style code (`if found == null && f(item) then found = item else null`) where the explicit null was a placeholder with no meaning. Synthesizing `NullLit` at parse time means the AST shape is unchanged — no `Option<Box<Expr>>` needed in `Expr::If` or anywhere downstream.

**Consequence**: The result type widens to `T | Null` when `else` is absent. Code that uses the result of an `else`-less `if` without handling the `Null` case will pass type-checking silently (the union just grows). This is an acceptable tradeoff: the common case (result discarded) gets cleaner syntax, and the footgun (accidentally using a `T | Null` result as `T`) is the same class of error already present whenever any function returns `Null`.
