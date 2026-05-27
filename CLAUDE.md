# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this project is

`lin-lang` is the reference implementation of **Lin**, a small expression-based language built around strict JSON data, structural typing, first-argument function application (dot syntax), destructuring, pattern matching, opaque iterator/runtime types, and value-based error handling. The full language design is in `docs/SPECIFICATION.md`.

The project has one backend:

- **`lin-codegen`** — an LLVM native-code compiler (`lin build`). Goes through a full type-checking pass (`lin-check`), lowers to flat 3-address IR (`lin-ir`), then to LLVM IR via `inkwell`. Links with a small Rust static library (`lin-runtime`) to produce a standalone binary.

Note: `lin-eval` (the tree-walking interpreter) still exists in the workspace as a test harness but is no longer a supported `lin` subcommand.

## Build / run / test

```bash
cargo build --workspace
cargo test --workspace                          # runs all unit + integration tests
cargo run -p lin -- build examples/hello.lin -o hello  # compile to native binary
cargo run -p lin -- check examples/hello.lin    # type check only
cargo run -p lin -- test stdlib/                # run stdlib test suite (*.test.lin)
```

Environment variables for `lin build`:
- `LIN_EMIT_IR=1` — write the LLVM IR (`.ll`) alongside the binary
- `LIN_NO_OPT=1` — skip LLVM optimisation passes (faster builds, slower output)

CI runs on GitHub Actions (`.github/workflows/ci.yml`): `cargo build`, `cargo test --workspace`, and all non-network `examples/*.lin` on every push. There is no formatter wired up yet. There is no `cargo` available at the system shell at the time of writing — assume the user runs commands themselves.

## Workspace layout

Cargo workspace with ten crates (`crates/`):

- **`lin-common`** — shared `Span`, `Diagnostic`, `Interner`, edit-distance helpers. No dependencies on other crates.
- **`lin-lex`** — lexer with indentation tracking. Produces `Token` stream with synthetic `Indent`/`Dedent`/`Newline` tokens.
- **`lin-parse`** — parser, surface AST (`Module`, `Stmt`, `Expr`, `Pattern`, `TypeExpr`). Includes parser error recovery and "did you mean" diagnostics.
- **`lin-check`** — type checker. Consumes the surface AST; produces `TypedModule` (typed IR). Handles bidirectional inference, structural typing, union narrowing, exhaustiveness checking, TypeVar zonking, and numeric widening. Emits `Diagnostic` values with Ariadne-style multi-span rendering.
- **`lin-ir`** — flat 3-address IR (`LinIR`) sitting between `TypedExpr` and LLVM. Contains: IR data types (`ir.rs`), the `TypedModule → LinModule` lowering pass (`lower.rs`), backwards-dataflow liveness analysis (`liveness.rs`), and the Perceus-inspired RC elision pass (`rc_elide.rs`).
- **`lin-codegen`** — LLVM backend via `inkwell`. Compiles `TypedModule` directly to LLVM IR today; `lin-ir` is available as an optional pre-pass. Handles functions, closures, objects, arrays, strings, union tagged dispatch, pattern matching, TCO, and unboxed scalar arrays.
- **`lin-runtime`** — small static library linked into every compiled binary. Provides refcounted strings/arrays/objects, intrinsics (`lin_print`, `lin_string_concat`, etc.), and flat scalar array variants (`lin_flat_array_alloc_i32`, etc.).
- **`lin-compile`** — orchestrates the full compilation pipeline: source → lex → parse → type check → codegen → link. Includes a module cache (`.lin-cache/<sha256>.typed`) and module signature files (`.lin-cache/<sha256>.sig`) to skip re-checking unchanged imports.
- **`lin-eval`** — tree-walking interpreter (legacy; no longer a CLI subcommand). Owns `Value`, `Env`, `Interpreter`. Used as a test harness only.
- **`lin`** — CLI binary. Dispatches `build`, `check`, `test` subcommands.
- **`lin-lsp`** — language server (in progress).

Stdlib lives in `stdlib/*.lin` and is loaded via `include_str!` in both `lin-eval` and `lin-compile`. Current stdlib modules: `std/io`, `std/string`, `std/number`, `std/array`, `std/iter`, `std/object`, `std/async`, `std/fs`, `std/http`, `std/server`, `std/template`, `std/test`.

## Pipeline shapes

**Interpreter (`lin run`)**:
```
source (.lin) → Lexer → Tokens → Parser → AST → Interpreter::eval → Value
```

Everything happens in `lin-eval::Interpreter`:

1. `Interpreter::new()` calls `register_intrinsics()` (Rust-implemented primitives named `lin_print`, `lin_length`, `lin_to_string`, `lin_for`, `lin_iter`, `lin_push`, `lin_keys`, etc.) then `register_stdlib_sources()` (embeds the `stdlib/*.lin` files via `include_str!`) then `preload_stdlib()` (loads `std/io`, `std/string`, `std/iter`, `std/array`, `std/object`, `std/async` into the global env so `print`, `toString`, `range`, `map`, etc. are globally available in the interpreter).
2. `run_file(path)` sets `base_path` (used for resolving user imports) and calls `run(source)`.
3. `run(source)` lexes, parses, then evaluates statements top-to-bottom in `global_env`.

The interpreter is a single ~1400-line file (`crates/lin-eval/src/interpreter.rs`). Most language features live there.

**Compiler (`lin build`)**:
```
source (.lin)
  → Lexer → Tokens → Parser → AST
  → lin-check: type checker → TypedModule
  → lin-ir (optional): TypedModule → LinModule (flat 3-address IR) → RC elision pass
  → lin-codegen: TypedModule → LLVM IR (via inkwell)
  → LLVM optimisation passes (default: O2)
  → emit .o object file
  → cc link with lin-runtime.a → native binary
```

Imports are resolved recursively before the main module is checked. Each imported module is type-checked once and cached by source hash in `.lin-cache/`. If the cache hit is valid, the `TypedModule` is deserialised instead of re-checked. A separate `ModuleSignature` (`.sig` file) records just the exported name→type map; dependents only need that to verify their own usage, not the full IR.

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
- **Compiler builtins use `lin_*` names; user-facing names come from stdlib.** All polymorphic primitives (`lin_print`, `lin_for`, `lin_iter`, `lin_length`, `lin_to_string`, `lin_push`, `lin_keys`, `lin_range`, `lin_map`, `lin_filter`, `lin_reduce`, and all concurrency: `lin_async` etc.) are registered in `register_intrinsics()` and dispatched specially in codegen. They are not visible to user code. Stdlib files re-export them under their clean names: `std/io` exports `print`, `std/iter` exports `for`/`iter`/`range`, `std/array` exports `map`/`filter`/`reduce`/`push`/`length`, `std/object` exports `keys`, `std/string` exports `toString`/`length`, `std/async` exports `async`/`await` etc. In the interpreter, `preload_stdlib()` injects all these into the global env so they are available without an import. In the compiler, user code must import them explicitly (ADR-002, ADR-009).
- **Inline blocks inside parentheses.** Lambdas like `x => val y = x*2; y` passed to `.for(...)` have no INDENT/DEDENT (suppressed by ADR-004). `parse_function_body` detects `val`/`var` as the multi-statement-body signal (ADR-014).
- **Imports: `std/...` resolves into the embedded stdlib sources; everything else is resolved relative to the importing file's directory with `.lin` appended** (ADR-016). Module init is lazy; cycles within a single init chain are a runtime error.
- **`async(f)` thunks must not capture `var` bindings** and must not return `Function` or `Iterator` values. Both are compile-time errors in `lin-check`. The checker tracks mutable global slots separately (`mutable_global_slots`) because global vars are not recorded as captures (ADR-034).
- **`import foreign "path"` registers stubs in the interpreter** that error at call time. The stub arity is derived from the declared type so call-site arity checks pass (ADR-033, ADR-037). Real FFI calls work only via `lin build`.
- **`import foreign "lin-runtime"` is a reserved internal path** used by stdlib files to declare their FFI dependencies on `lin-runtime.a` symbols (e.g. `lin_string_trim`, `lin_fs_read`). The compiler recognises this path, skips normal FFI type validation (to allow Array/Object return types), and doesn't add it to `foreign_lib_paths` (it's always linked). User code cannot use this path meaningfully — the runtime symbols are only accessible through the stdlib wrappers.
- **IO/FS/HTTP/server intrinsics use the call-dispatch pattern** (same as `lin_for`, `lin_async`, `lin_worker`) so they can call interpreter methods with `&mut self`. They are registered as stub natives that are intercepted by name in `call_value` (ADR-030).

## Adding a language feature

The typical path:

1. **Tokens** — add `TokenKind` variants in `lin-lex/src/token.rs`, lex them in `lin-lex/src/lexer.rs`. Remember the indentation suppression invariants for new delimiters.
2. **AST** — add `Expr`/`Stmt`/`Pattern`/`TypeExpr` variants in `lin-parse/src/ast.rs`. Each variant carries its own `Span`. Add a branch in `Expr::span()`.
3. **Parser** — wire into `lin-parse/src/parser.rs`. For postfix operators, mind the DEDENT suppression rule (ADR-011). For continuation-line constructs, use the `skip_continuation_newline` pattern (ADR-013).
4. **Interpreter** — add a match arm in `eval_expr_in_env` (and `eval_tail_expr` if it can appear in tail position). Native helpers go through `define_native(name, arity, |args| ...)` in `register_intrinsics`.
5. **Tests** — add a case to `crates/lin-eval/tests/integration.rs` and, ideally, an end-to-end fixture in `examples/`.

There is no desugaring pass — the interpreter consumes the surface AST directly. Things the spec describes as desugarings (`x.f(y)` → `f(x, y)`, destructuring → primitive bindings) are implemented inline in the evaluator.

## Adding a stdlib function

Make sure it is included in the `docs/STDLIB.md` documentation. Add a test case to the colocated `stdlib/<module>.test.lin` file.

## Where things live by topic

- **Operator precedence** — `parse_or_expr` → `parse_and_expr` → `parse_comparison` → ... in `lin-parse/src/parser.rs`. Mirror the spec §24.2 ladder when changing.
- **Iterator semantics** — `IteratorValue` struct in `lin-eval/src/value.rs`; the `for` intrinsic and `iter` constructor in `register_intrinsics`. Per spec §17.6, do not model iterators as JSON-shaped objects.
- **Equality** — `Value::deep_eq` in `lin-eval/src/value.rs`. Objects are order-independent; arrays are ordered; cross-numeric (`Int == Float`) compares by value.
- **Display / `toString`** — `Value::to_display_string` and `to_json_string` in the same file. Used by string interpolation.

## Reading order for a new contributor

1. `docs/SPECIFICATION.md` — what the language is meant to be.
2. `docs/STDLIB.md` — specification of the stdlib.
3. `docs/DECISIONS.md` — every non-obvious implementation choice and why. **Read this before touching the lexer or parser.**
4. `docs/TODO.md` — milestone plan. Note the gap: §3 specifies type checking, but v0 has none.
5. `crates/lin-eval/src/interpreter.rs` — the engine.
6. `examples/*.lin` and `crates/lin-eval/tests/integration.rs` — what currently works.
