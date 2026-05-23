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

**Decision**: `parse_function_body` detects when a function body starts with `val`/`var` (indicating a multi-statement body) and parses an "inline block" — a sequence of statements terminated by `)` rather than DEDENT.

**Rationale**: Inside parentheses, the lexer suppresses all INDENT/DEDENT and Newline tokens (ADR-004). A lambda like `x => val y = x * 2; y` inside `.for(...)` has no indentation markers, so `parse_expr_or_block` cannot detect the block. The inline block parser handles this by treating `val`/`var` as the signal for multi-statement body.

**Consequence**: Multi-statement lambdas work correctly inside `.for()`, `.map()`, and other callback-accepting function calls. Single-expression lambdas are unaffected.

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
