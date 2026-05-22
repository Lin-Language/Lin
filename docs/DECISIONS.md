# Architecture Decision Records

## ADR-001: Dynamic typing for v0

**Decision**: The v0 interpreter does not include a type checker. Types are parsed but not verified at compile time. Runtime type tags (on the Value enum) are used for `is`/`has` checks.

**Rationale**: A full bidirectional type system with generics, variance, and numeric widening is a multi-week effort. Dynamic execution lets us validate the language design and build a working demo overnight. Type checking can be layered on top later without changing the runtime semantics.

**Consequence**: All type annotations are parsed and stored in the AST but ignored at runtime.

## ADR-002: Built-in iteration functions instead of .lin stdlib

**Decision**: `for`, `map`, `filter`, `reduce`, `range`, `iterOf`, and `iter` are implemented as built-in functions in the Rust interpreter rather than in .lin stdlib files.

**Rationale**: These functions need to invoke user callbacks (closures), which requires access to the interpreter's `call_value` method. Rust's `fn` pointer type cannot capture state, so native functions registered with a simple `fn(&[Value]) -> Result<Value, String>` signature cannot call back into the interpreter. Making them built-ins avoids this architectural constraint.

**Consequence**: The stdlib .lin files for `std/array` and `std/iter` are thin re-export wrappers (or unused). The built-ins are registered directly in the global environment.

## ADR-003: Range materializes as array

**Decision**: `range(start, end)` immediately materializes all values into a `Vec<Value>` (an Array) rather than producing a lazy iterator.

**Rationale**: Creating a true lazy iterator requires closures that capture the end-bound, which conflicts with the `fn` pointer approach used for native functions. Materializing as an array is simple and correct for v0. For typical ranges in test programs (< 10000 elements), this is not a performance concern.

**Consequence**: Very large ranges will allocate proportionally. A lazy implementation can be added in v1 when the iterator infrastructure is more mature.

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
