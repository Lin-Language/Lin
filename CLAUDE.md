# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this project is

`lin-lang` is the reference implementation of **Lin**, a small expression-based language built around strictly typed JSON data, structural typing, first-argument function application (dot syntax), destructuring, pattern matching, opaque iterator/runtime types, and value-based error handling. The full language design is in `docs/SPECIFICATION.md`.

The project has one backend:

- **`lin-codegen`** — an LLVM native-code compiler (`lin build`). Goes through a full type-checking pass (`lin-check`), lowers to flat 3-address IR (`lin-ir`), then to LLVM IR via `inkwell`. Links with a small Rust static library (`lin-runtime`) to produce a standalone binary.

## Process

Do not work or make changes directly in the codebase. Create a subagent with a git worktree do the work there, ensure the workstree is up to date with the latest master, make sure the tests pass, then ask before merging back.

Do not use git stash for no-op comparisons in worktrees — stashes are shared and keep tangling. Compare via git checkout HEAD~1 on the committed branch only.

Expect master have moved on when you go to integrate your worktree back to master. There are multiple agents working at any one time, all with their own cadence. Pull master into your worktree, test and then merge. 

## Build / run / test

```bash
cargo build --workspace && cargo test --workspace   # always build first — integration tests invoke target/debug/lin as a subprocess, so a stale binary causes spurious failures
cargo run -p lin -- build examples/calc/main.lin -o calc  # compile to native binary
cargo run -p lin -- check examples/calc/main.lin    # type check only
cargo run -p lin -- test stdlib/ examples/      # run stdlib + example-project test suites (*.test.lin)
```

Environment variables for `lin build`:
- `LIN_EMIT_IR=1` — write the LLVM IR (`.ll`) alongside the binary
- `LIN_NO_OPT=1` — skip LLVM optimisation passes (faster builds, slower output)

CI runs on GitHub Actions (`.github/workflows/ci.yml`): `cargo build`, `cargo test --workspace`, `lin fmt --check` over `stdlib/`/`examples/`/`benchmarks/`, and all non-network `examples/*.lin` on every push.

A source formatter exists: `cargo run -p lin -- fmt <paths>` rewrites `.lin` files to canonical form in place; `lin fmt --check <paths>` verifies without writing (used by CI). It is comment-preserving and AST-faithful — it must never change program meaning, which is enforced by the corpus type-check + run-equivalence gate in the formatter tests (`crates/lin/tests/integration.rs`). See ADR-025 for comment handling.

## Releasing

Releases are automated with [release-plz](https://release-plz.dev/) and driven by Conventional Commits.

- **Single source of truth for the version**: `[workspace.package].version` in the root `Cargo.toml`. Every crate inherits it via `version.workspace = true`; never bump a crate version by hand.
- **Cutting a release**: every push to `master` updates an open "chore: release" PR (version bump + generated `CHANGELOG.md`). **Merging that PR cuts the release** — release-plz then creates the `vX.Y.Z` tag + GitHub release and attaches the platform binaries/VSIX. Commit prefixes matter: `feat` → minor, `fix`/`perf` → patch, `!`/`BREAKING CHANGE` → major; `chore`/`ci`/`test`/`style` are skipped.
- **Bleeding edge**: `release.yml` still publishes a rolling `latest` *prerelease* on every push to `master`.
- **Install**: `install.sh` installs the newest *stable* release by default (skips the rolling prerelease); `LIN_VERSION=vX.Y.Z` pins one.

## Workspace layout

Cargo workspace with nine crates (`crates/`):

- **`lin-common`** — shared `Span`, `Diagnostic`, `Interner`, edit-distance helpers. No dependencies on other crates.
- **`lin-lex`** — lexer with indentation tracking. Produces `Token` stream with synthetic `Indent`/`Dedent`/`Newline` tokens.
- **`lin-parse`** — parser, surface AST (`Module`, `Stmt`, `Expr`, `Pattern`, `TypeExpr`). Includes parser error recovery and "did you mean" diagnostics. The recursive-descent parser is one `impl Parser` split across a `parser/` module tree by concern (`parser/mod.rs` holds `Parser` + token-cursor helpers; `stmt.rs`, `expr.rs` incl. the precedence ladder, `function.rs`, `pattern.rs`, `types.rs`).
- **`lin-check`** — type checker. Consumes the surface AST; produces `TypedModule` (typed IR). Handles bidirectional inference, structural typing, union narrowing, exhaustiveness checking, TypeVar zonking, and numeric widening. Emits `Diagnostic` values with Ariadne-style multi-span rendering. The `Checker` is one `impl` split across a `checker/` module tree (`checker/mod.rs` holds `Checker` + module-level passes; `stmt.rs`, `expr.rs`, `ops.rs`, `call.rs`, `function.rs`, `pattern.rs`, `intrinsics.rs`, `helpers.rs`).
- **`lin-ir`** — flat 3-address IR (`LinIR`) sitting between `TypedExpr` and LLVM, and the **sole** lowering path. Contains: IR data types (`ir.rs`), the `TypedModule → LinModule` lowering pass (`lower.rs`, incl. `lower_module` for the main module and `lower_import_module` for imports), backwards-dataflow liveness analysis (`liveness.rs`), and the Perceus-inspired RC elision pass (`rc_elide.rs`).
- **`lin-codegen`** — LLVM backend via `inkwell`. Compiles a `LinModule` (the flat IR) to LLVM IR via `compile_module_from_ir` (main module) and `compile_import_from_ir` (imports). Handles functions, closures, objects, arrays, strings, union tagged dispatch, pattern matching, TCO, and unboxed scalar arrays. (The former TypedAST-direct backend was removed once the IR path reached parity.) One `impl Codegen` split across a `codegen/` module tree (`mod.rs`, `runtime.rs` = `RuntimeFns` runtime decls, `builder_ext.rs` = `BuilderExt` façade over inkwell's `build_*().unwrap()`, plus `types`/`boxing`/`literals`/`arith`/`call`/`data`/`intrinsics`/`match`/`rc`).
- **`lin-runtime`** — small static library linked into every compiled binary. Provides refcounted strings/arrays/objects, intrinsics (`lin_print`, `lin_string_concat`, etc.), and flat scalar array variants (`lin_flat_array_alloc_i32`, etc.).
- **`lin-compile`** — orchestrates the full compilation pipeline: source → lex → parse → type check → codegen → link. Includes a module cache (`.lin-cache/<sha256>.typed`) and module signature files (`.lin-cache/<sha256>.sig`) to skip re-checking unchanged imports.
- **`lin`** — CLI binary. Dispatches `build`, `check`, `test` subcommands.
- **`lin-lsp`** — language server (in progress).

Stdlib lives in `stdlib/*.lin` and is loaded via `include_str!` in `lin-compile`. Current stdlib modules: `std/io`, `std/string`, `std/number`, `std/iter`, `std/array`, `std/object`, `std/async`, `std/fs`, `std/stream`, `std/compress`, `std/archive`, `std/http`, `std/net`, `std/process`, `std/tty`, `std/signal`, `std/template`, `std/test`, `std/time`.

## Pipeline shape

**Compiler (`lin build`)**:
```
source (.lin)
  → Lexer → Tokens → Parser → AST
  → lin-check: type checker → TypedModule
  → lin-ir: TypedModule → LinModule (flat 3-address IR) → RC elision pass
  → lin-codegen: LinModule → LLVM IR (via inkwell)
  → LLVM optimisation passes (default: O2)
  → emit .o object file
  → cc link with lin-runtime.a → native binary
```

Imports are resolved recursively before the main module is checked. Each imported module is type-checked once and cached by source hash in `.lin-cache/`. If the cache hit is valid, the `TypedModule` is deserialised instead of re-checked. A separate `ModuleSignature` (`.sig` file) records just the exported name→type map; dependents only need that to verify their own usage, not the full IR.

## Key design choices to be aware of

These are non-obvious and easy to break. Full rationale lives in `docs/DECISIONS.md` — read it before making structural changes.

- **Indentation lexing is suppressed inside `{ }`, `( )`, `[ ]`.** This lets JSON object literals span lines without triggering block parsing. Don't add INDENT/DEDENT logic inside delimiter-balanced spans (ADR-003).
- **String interpolation is one compound token** (`InterpString(Vec<InterpPart>)`) whose `Expr` parts each carry their own sub-token-stream. The parser recurses into those sub-streams (ADR-004).
- **Dot-chaining across newlines uses save/restore lookahead** in the parser's postfix loop. Don't aggressively skip newlines — it breaks block structure (ADR-005). After a `Dedent`, postfix `[` and `(` are suppressed but `.` is allowed (ADR-010).
- **Bare-identifier lambdas (`x => x * 2`) are only recognised in argument position.** `is_bare_lambda()` looks ahead from inside argument parsing (ADR-006).
- **`val` whose RHS is a function literal is forward-declared** before codegen via a pre-scan, so mutual recursion works between top-level functions (ADR-012). Non-function `val` cannot self-reference (spec §7.3).
- **TCO uses an alloca/loop transform in codegen.** Direct self-recursive calls in tail position are emitted as jumps back to a `tco_loop` header. Mutual TCO is not implemented (ADR-016, spec §27.3).
- **`var` is captured by reference** — a heap-allocated mutable slot shared by all closures over the same binding (spec §27.2, ADR-012).
- **Bracket access is safe by default.** Missing object key → `Null`; `Null` propagates through chains; array OOB is a runtime error (spec §6.1).
- **Prefer generics and named/intersection (`&`) record types over `Json` in `.lin` code.** `Json` is a dynamic escape hatch for genuinely unknowable shapes (untyped wire data, recursive ASTs) — it disables type checking and is a perf cliff (field access on `Json` is an optimisation barrier; typed records inline to a constant slot load). Default to monomorphised generics `<T>` for shape-agnostic code and named records (composed with `&`, ADR-061) for known shapes.
- **Compiler builtins use `lin_*` names; user-facing names come from stdlib.** All polymorphic primitives (`lin_print`, `lin_for`, `lin_iter`, `lin_length`, `lin_to_string`, `lin_push`, `lin_keys`, and all concurrency: `lin_async` etc.) are dispatched specially in codegen. They are not visible to user code (now ENFORCED in the checker: `lin_*` names resolve only when type-checking a trusted stdlib module — ADR-060). Stdlib files re-export them under their clean names: `std/io` exports `print`, `std/iter` exports the iterable combinators `map`/`filter`/`reduce`/`for`/`range` (and dispatches them on the receiver's type — eager over Array/Iterator, lazy over `Stream`; ADR-051), `std/array` exports array-shaped ops `push`/`length`/`slice`/`sort`/…, `std/object` exports `keys`, `std/string` exports `toString`, `std/async` exports `async`/`await` etc. User code must import them explicitly (ADR-002, ADR-008).
- **Inline blocks inside parentheses.** Lambdas like `x => val y = x*2; y` passed to `.for(...)` have no INDENT/DEDENT (suppressed by ADR-003). `parse_function_body` detects `val`/`var` as the multi-statement-body signal (ADR-011).
- **Imports: `std/...` resolves into the embedded stdlib sources; everything else is resolved relative to the importing file's directory with `.lin` appended** (ADR-013). Module init is lazy; cycles within a single init chain are a compile-time error.
- **`async(f)` thunks must not capture `var` bindings** and must not return `Function` or `Iterator` values. Both are compile-time errors in `lin-check`. The checker tracks mutable global slots separately (`mutable_global_slots`) because global vars are not recorded as captures (ADR-022).
- **`import foreign "path"` declares external C symbols.** The compiler emits LLVM `declare` directives and passes library paths to the linker (ADR-021). Real FFI calls work only via `lin build`.
- **`import foreign "lin-runtime"` is a reserved internal path** used by stdlib files to declare their FFI dependencies on `lin-runtime.a` symbols (e.g. `lin_string_trim`, `lin_fs_read`). The compiler recognises this path, skips normal FFI type validation (to allow Array/Object return types), and doesn't add it to `foreign_lib_paths` (it's always linked). User code cannot use this path meaningfully — the runtime symbols are only accessible through the stdlib wrappers.

## Adding a language feature

The typical path:

1. **Tokens** — add `TokenKind` variants in `lin-lex/src/token.rs`, lex them in `lin-lex/src/lexer.rs`. Remember the indentation suppression invariants for new delimiters.
2. **AST** — add `Expr`/`Stmt`/`Pattern`/`TypeExpr` variants in `lin-parse/src/ast.rs`. Each variant carries its own `Span`. Add a branch in `Expr::span()`.
3. **Parser** — wire into the `lin-parse/src/parser/` module tree (expressions in `expr.rs`, statements in `stmt.rs`, etc.). For postfix operators, mind the DEDENT suppression rule (ADR-010). For continuation-line constructs, use the `skip_continuation_newline` pattern (ADR-005).
4. **Type checker** — add handling in the `lin-check/src/checker/` module tree (expression inference in `expr.rs`, statements in `stmt.rs`, etc.).
5. **Codegen** — add handling in the `lin-codegen/src/codegen/` module tree (instruction dispatch in `mod.rs`; intrinsics in `intrinsics.rs`, etc.). If a new runtime intrinsic is needed, add it to `lin-runtime/src/` and declare it in `codegen/runtime.rs`'s `RuntimeFns`.
6. **Tests** — add an end-to-end test in `crates/lin/tests/integration.rs` and a fixture in `examples/`.

## Adding a stdlib function

Make sure it is included in the `docs/STDLIB.md` documentation. Add a test case to the colocated `stdlib/<module>.test.lin` file.

## Where things live by topic

- **Operator precedence** — `parse_or_expr` → `parse_and_expr` → `parse_comparison` → ... in `lin-parse/src/parser/expr.rs`. Mirror the spec §24.2 ladder when changing.
- **Iterator semantics** — `lin_iter` / `lin_for` intrinsics in `lin-runtime`; `lin_iter` constructs an opaque iterator handle. Per spec §17.6, do not model iterators as JSON-shaped objects.
- **Equality** — implemented in codegen's `emit_eq`; objects are order-independent, arrays are ordered, cross-numeric (`Int == Float`) compares by value.
- **Display / `toString`** — `lin_to_string` in `lin-runtime/src/`.

## Reading order for a new contributor

1. `docs/SPECIFICATION.md` — what the language is meant to be.
2. `docs/STDLIB.md` — specification of the stdlib.
3. `docs/DECISIONS.md` — every non-obvious implementation choice and why. **Read this before touching the lexer or parser.**
4. `docs/TODO.md` — milestone plan.
5. `crates/lin/tests/integration.rs` — what currently works end-to-end.
6. `examples/*.lin` — example programs.
