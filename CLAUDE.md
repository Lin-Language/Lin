# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this project is

`lin-lang` is the reference implementation of **Lin**, a small expression-based language built around strict JSON data, structural typing, first-argument function application (dot syntax), destructuring, pattern matching, opaque iterator/runtime types, and value-based error handling. The full language design is in `docs/SPECIFICATION.md`.

The current implementation is **v0**: a tree-walking interpreter. There is no type checker — types are parsed and stored in the AST but ignored at runtime (see ADR-001 in `docs/DECISIONS.md`). Compile-time semantics described in the spec (variance, exhaustiveness, narrowing, numeric widening) are not enforced yet.

## Build / run / test

```bash
cargo build --workspace
cargo test --workspace                          # runs all unit + integration tests
cargo test -p lin-eval test_hello_world         # run a single test
cargo run -p lin -- examples/hello.lin          # execute a .lin program
cargo run -p lin -- -                           # read source from stdin
```

There is no CI config or formatter wired up yet. There is no `cargo` available at the system shell at the time of writing — assume the user runs commands themselves.

## Workspace layout

Cargo workspace with five crates (`crates/`):

- **`lin-common`** — shared `Span`, `Diagnostic`, `Interner`. No dependencies on other crates.
- **`lin-lex`** — lexer with indentation tracking. Produces `Token` stream with synthetic `Indent`/`Dedent`/`Newline` tokens.
- **`lin-parse`** — parser, surface AST (`Module`, `Stmt`, `Expr`, `Pattern`, `TypeExpr`).
- **`lin-eval`** — tree-walking interpreter (the v0 backend). Owns `Value`, `Env`, `Interpreter`.
- **`lin`** — CLI binary. Thin wrapper that constructs an `Interpreter` and calls `run_file`.

Note: the spec (§28.1) and `docs/TODO.md` reference a `lin-check` crate (desugaring + type checker) and a `lin-stdlib` crate. **Neither exists yet.** Stdlib lives in `stdlib/*.lin` and is loaded via `include_str!` from inside `lin-eval`.

## Pipeline shape

```
source (.lin) → Lexer → Tokens → Parser → AST → Interpreter::eval → Value
```

Everything happens in `lin-eval::Interpreter`:

1. `Interpreter::new()` calls `register_intrinsics()` (native Rust functions like `print`, `length`, `toString`, `__stringTrim`, `iter`, `for`, `push`, ...) then `register_stdlib_sources()` (embeds the `stdlib/*.lin` files via `include_str!`) then `preload_stdlib()` (loads `std/iter` and `std/array` into the global env so `range`, `map`, `filter`, etc. are globally available).
2. `run_file(path)` sets `base_path` (used for resolving user imports) and calls `run(source)`.
3. `run(source)` lexes, parses, then evaluates statements top-to-bottom in `global_env`.

The interpreter is a single ~1400-line file (`crates/lin-eval/src/interpreter.rs`). Most language features live there.

## Key design choices to be aware of

These are non-obvious and easy to break. Full rationale lives in `docs/DECISIONS.md` — read it before making structural changes.

- **Indentation lexing is suppressed inside `{ }`, `( )`, `[ ]`.** This lets JSON object literals span lines without triggering block parsing. Don't add INDENT/DEDENT logic inside delimiter-balanced spans (ADR-004, ADR-017).
- **String interpolation is one compound token** (`InterpString(Vec<InterpPart>)`) whose `Expr` parts each carry their own sub-token-stream. The parser recurses into those sub-streams (ADR-005).
- **Dot-chaining across newlines uses save/restore lookahead** in the parser's postfix loop. Don't aggressively skip newlines — it breaks block structure (ADR-006). After a `Dedent`, postfix `[` and `(` are suppressed but `.` is allowed (ADR-011).
- **Bare-identifier lambdas (`x => x * 2`) are only recognised in argument position.** `is_bare_lambda()` looks ahead from inside argument parsing (ADR-007).
- **`val` whose RHS is a function literal is forward-declared via mutable cells** before evaluation, so mutual recursion works between top-level functions (ADR-015). Non-function `val` cannot self-reference (spec §7.3).
- **Top-level statements clone `global_env`, evaluate, then write back.** This is to dodge a borrow checker conflict between `&mut self` (needed for `call_value`) and `&mut self.global_env` (ADR-008). The clone is O(n) bindings — acceptable for v0.
- **TCO uses a `TailResult` trampoline.** `eval_tail_expr` recognises direct self-recursive calls in tail position (function body, if branches, block tails, match arm bodies) and returns `TailCall(args)` instead of recursing. Only `call_function` loops on it. Mutual TCO is not implemented (ADR-012, spec §27.3).
- **`var` is captured by reference via shared `Rc<RefCell<Value>>` cells.** Two closures over the same `var` see the same storage (spec §27.2, ADR-015).
- **Bracket access is safe by default.** Missing object key → `Null`; `Null` propagates through chains; array OOB is a runtime error (spec §6.1).
- **Stdlib split: `for` and `iter` are Rust intrinsics, everything else is .lin.** `range`, `iterOf`, `map`, `filter`, `reduce` live in `stdlib/{iter,array}.lin` and are preloaded as globals (ADR-002). String functions are .lin wrappers around `__stringFoo` Rust intrinsics (ADR-009).
- **Inline blocks inside parentheses.** Lambdas like `x => val y = x*2; y` passed to `.for(...)` have no INDENT/DEDENT (suppressed by ADR-004). `parse_function_body` detects `val`/`var` as the multi-statement-body signal (ADR-014).
- **Imports: `std/...` resolves into the embedded stdlib sources; everything else is resolved relative to the importing file's directory with `.lin` appended** (ADR-016). Module init is lazy; cycles within a single init chain are a runtime error.

## Adding a language feature

The typical path:

1. **Tokens** — add `TokenKind` variants in `lin-lex/src/token.rs`, lex them in `lin-lex/src/lexer.rs`. Remember the indentation suppression invariants for new delimiters.
2. **AST** — add `Expr`/`Stmt`/`Pattern`/`TypeExpr` variants in `lin-parse/src/ast.rs`. Each variant carries its own `Span`. Add a branch in `Expr::span()`.
3. **Parser** — wire into `lin-parse/src/parser.rs`. For postfix operators, mind the DEDENT suppression rule (ADR-011). For continuation-line constructs, use the `skip_continuation_newline` pattern (ADR-013).
4. **Interpreter** — add a match arm in `eval_expr_in_env` (and `eval_tail_expr` if it can appear in tail position). Native helpers go through `define_native(name, arity, |args| ...)` in `register_intrinsics`.
5. **Tests** — add a case to `crates/lin-eval/tests/integration.rs` and, ideally, an end-to-end fixture in `examples/`.

There is no desugaring pass — the interpreter consumes the surface AST directly. Things the spec describes as desugarings (`x.f(y)` → `f(x, y)`, destructuring → primitive bindings) are implemented inline in the evaluator.

## Where things live by topic

- **Operator precedence** — `parse_or_expr` → `parse_and_expr` → `parse_comparison` → ... in `lin-parse/src/parser.rs`. Mirror the spec §24.2 ladder when changing.
- **Iterator semantics** — `IteratorValue` struct in `lin-eval/src/value.rs`; the `for` intrinsic and `iter` constructor in `register_intrinsics`. Per spec §17.6, do not model iterators as JSON-shaped objects.
- **Equality** — `Value::deep_eq` in `lin-eval/src/value.rs`. Objects are order-independent; arrays are ordered; cross-numeric (`Int == Float`) compares by value.
- **Display / `toString`** — `Value::to_display_string` and `to_json_string` in the same file. Used by string interpolation.

## Reading order for a new contributor

1. `docs/SPECIFICATION.md` — what the language is meant to be.
2. `docs/DECISIONS.md` — every non-obvious implementation choice and why. **Read this before touching the lexer or parser.**
3. `docs/TODO.md` — milestone plan. Note the gap: §3 specifies type checking, but v0 has none.
4. `crates/lin-eval/src/interpreter.rs` — the engine.
5. `examples/*.lin` and `crates/lin-eval/tests/integration.rs` — what currently works.
