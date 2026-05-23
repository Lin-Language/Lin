# Implementation Plan & Task List

## Strategy

We build the compiler in **vertical slices**: each milestone ends with a runnable end-to-end pipeline (lex → parse → check → execute) for a subset of the language. Each subsequent milestone widens the subset. This avoids "all parser, no semantics" purgatory and produces something demonstrable at every step.

The backend is a **tree-walking interpreter** in `lin-eval`. A native or bytecode target is deferred (spec §30).

Host language: **Rust**. Layout: Cargo workspace.

## Workspace Layout

```txt
lin-lang/
  Cargo.toml                 (workspace root)
  crates/
    lin-common/              shared types: Span, Diagnostic, intern table
    lin-lex/                 lexer, indentation tokenizer
    lin-parse/               parser, surface AST
    lin-check/               desugaring, type checker, core AST
    lin-eval/                tree-walking interpreter (v1 backend)
    lin-stdlib/              built-in stdlib functions
    lin/                     the CLI binary (cargo install entry point)
  docs/
  examples/
```

Each crate has its own `tests/` directory with unit and snapshot tests. Top-level `examples/` holds end-to-end `.lin` fixtures executed by `lin` in CI.

## Prerequisites

- [x] `cargo new --workspace` scaffolding and per-crate skeletons.
- [ ] Snapshot-test crate dependency (e.g. `insta`) in dev-dependencies.
- [ ] `lin-common`: `Span`, `SourceFile`, `Diagnostic` (with surrounding-source rendering and call-stack support), `Interner`.
- [x] `examples/` seeded with the smallest fixture (Milestone 1's hello world).
- [ ] CI workflow: `cargo test --workspace`, run every `examples/*.lin` through `lin`.

---

## Milestone 1 — Hello, world ✓

End state: `print("hello")` runs and prints `hello`.

### `lin-lex` (M1 subset)
- [x] Token kinds: identifier, string literal (no escapes, no interpolation yet), integer literal, punctuation (`(`, `)`, `,`), keywords (`val`, `import`, `from`).
- [x] Reject CRLF and tab indentation with diagnostic.
- [x] NEWLINE token; skip blank lines.
- [x] Span on every token.

### `lin-parse` (M1 subset)
- [x] `import { name } from "path"` declarations.
- [x] `val name = expr` top-level binding.
- [x] Function call expressions: `f(arg, arg, ...)`.
- [x] String and integer literal expressions.

### `lin-eval` (M1 subset)
- [x] Module loading from a single entry file (no transitive imports yet).
- [x] Built-in `print` registered directly as a host function.
- [x] Evaluate `val` declarations top-to-bottom in module scope.

### Tests
- [x] Lex/parse/run a fixture that prints a literal string.

---

## Milestone 2 — Numbers, operators, bindings ✓

End state: arithmetic, `var` mutation, assignment-as-expression, multi-statement programs.

### `lin-lex`
- [x] Numeric literal forms: decimal, hex (`0x`), binary (`0b`), octal (`0o`), underscores, exponent, type suffixes (`i8`, `u32`, `f32`, ...).
- [x] Negative-literal disambiguation: `-` joins a literal only when the previous token cannot end an expression (spec §3.7).
- [x] Operator tokens: `+ - * / % == != < <= > >= && ||`, `=`.

### `lin-parse`
- [x] Operator precedence and associativity per spec §24.2.
- [x] `var` declarations and assignment-as-expression.
- [x] `val x: Type = expr` with optional type annotation (annotation recorded but not yet checked).

### `lin-eval`
- [x] Internal numeric representation per family (tagged enum).
- [x] Literal-to-type assignment using suffix or default (`Int32` / `Float64`).
- [x] Integer division/modulo by zero → runtime error.
- [x] Float operations follow IEEE 754.

### Tests
- [x] Arithmetic fixtures, mutation, assignment-as-expression.
- [x] Negative-literal disambiguation: `f(-5, x - 3)`.

---

## Milestone 3 — Indentation-aware syntax ✓

End state: multi-line function bodies, multi-line `if/then/else`, blocks.

### `lin-lex`
- [x] Indentation tracking; synthetic `INDENT` / `DEDENT` tokens at 2-space level changes.
- [x] Continuation rule: `&&` or `||` at the start of a continuation line (any deeper indent acceptable, multiple stacked allowed).
- [x] Reject mixed line endings.

### `lin-parse`
- [x] Block expressions (final expression is the value).
- [x] Function expressions `(params) => body` with single-line and block forms; optional return type annotation.
- [x] `if cond then a else b` in all three layout forms (single-line, then/else on subsequent lines, block branches). `then` and `else` must sit exactly one indent level deeper than `if` (§12).

### Tests
- [x] Each `if` layout variant round-trips and evaluates correctly.
- [x] Indentation error cases (mixed tabs, CRLF, off-by-one dedent).

---

## Milestone 4 — JSON data, safe access, destructuring ✓

End state: object/array literals, bracket access (safe-by-default), destructuring binds.

### `lin-lex`
- [x] `{`, `}`, `[`, `]`, `:`, `...`.

### `lin-parse`
- [x] Strict JSON object literal: quoted keys, no trailing commas.
- [x] Array literal.
- [x] Bracket access `expr["key"]` and `expr[index]`.
- [x] Object destructuring in `val`: full, shorthand, alias, nested, rest-spread.
- [x] Array destructuring with rest-spread.
- [x] Destructuring in function parameters.

### `lin-check` (initial desugaring)
- [x] Destructuring → primitive bindings (`val { name } = p` ⇒ `val name = p["name"]`).

### `lin-eval`
- [x] JSON object as insertion-ordered key/value map.
- [x] Object equality: order-independent, structural, deep.
- [x] Array equality: order-sensitive, deep.
- [x] **Safe bracket access** (spec §6.1):
  - Missing object key → `Null`.
  - Bracket on `Null` (either object- or array-style) → `Null`.
  - Array index OOB → runtime error.

### Tests
- [x] All destructuring forms.
- [x] Equality fixtures matching spec §14.
- [x] Deep-chain access through missing keys and through `null`.
- [x] Array OOB raises a diagnostic with span and call stack.

---

## Milestone 5 — String literals (full) ✓

End state: escapes, multi-line strings, `${...}` interpolation.

### `lin-lex`
- [x] Escape sequences: `\" \\ \n \r \t \0 \u{HHHH}`.
- [x] Multi-line string literals (preserve newlines verbatim).
- [x] Interpolation lex: split into string-part / expression-part tokens at `${`, balanced through `}`.

### `lin-parse`
- [x] String interpolation expression as a concatenation of parts and embedded expressions.

### `lin-eval`
- [x] Strings stored UTF-8 length-prefixed.
- [x] `toString` for every primitive (formats per spec §27.8).

### Tests
- [x] Escape and Unicode fixtures.
- [x] Interpolation with nested calls.

---

## Milestone 6 — Functions, partial application, dot application ✓

End state: full call semantics including chains and partial application.

### `lin-parse`
- [x] Function expressions with multiple params.
- [x] Dot application `x.f(y)`.
- [x] Dot partial `x.f` and `(x, y).f`.
- [x] Function literals inside argument lists parse until the matching `)` (paren scope overrides indentation).

### `lin-check` (desugaring)
- [x] `x.f(y, z)` → `f(x, y, z)`.
- [x] `x.f` → `f(x)` (partial).
- [x] `(x, y).f` → `f(x, y)` (partial).

### `lin-eval`
- [x] Partial application: dedicated value with `(fn_ptr, accumulated_args)`. Further application appends; arity match invokes (spec §27.7).
- [x] Over-application: compile-time error.
- [x] Argument evaluation: left-to-right.

### Tests
- [x] Dot chaining, partial application, error on over-application.

---

## Milestone 7 — Type declarations and structural typing

End state: named types, generics syntax, structural compatibility.

### `lin-parse`
- [x] `type Name = …` declarations.
- [x] Object types, union types `|`, function types `(T) => U`.
- [x] Array type forms: `T[]` (unbounded) and `[T1, T2, T3]` (fixed-length).
- [x] Generic parameters and applications using `<…>`.
- [x] Type-expression precedence: `[]` > `<>` > `=>` > `|` (spec §8.7).

### `lin-check` (type system v1)
- [ ] Internal type representation.
- [ ] Structural compatibility check (used both for assignability and `has`).
- [ ] Exact-shape check (used for `is`).
- [ ] Recursive types: name-based with lazy unfolding and cycle-aware equality.
- [ ] Reject self-referential non-function `val` (spec §7.3).

### Tests
- [ ] Compatible/incompatible assignment fixtures.
- [ ] Recursive `Tree`, `Person` (with `"spouse": Person | Null`) fixtures.

---

## Milestone 8 — `is`, `has`, `if`, narrowing

End state: type narrowing in `if` branches via `is`/`has`.

### `lin-parse`
- [x] `value is Type | Literal | Shape` expressions.
- [x] `value has Type | Shape` expressions.
- [x] `is`/`has` permitted in any expression context (return type `Boolean`).

### `lin-check`
- [ ] Narrowing rules per spec §25.
- [ ] Reject `is` on generic applications.
- [ ] Literal types: literal expressions have base type (no singleton types).

### Tests
- [ ] Union narrowing in `if`/`else`.
- [ ] `has { name }` accepts extra fields.
- [ ] Reject `is Result<Int32, String>`.

---

## Milestone 9 — Pattern matching ✓

End state: full `match` with all arm forms.

### `lin-parse`
- [x] `match scrutinee` with one arm per line.
- [x] `is` patterns, `has` patterns, `when` guards.
- [x] `else => expr` catch-all arm.
- [x] Reject mixed `is`/`has` in a single arm.

### `lin-check`
- [ ] Narrowing per arm.
- [ ] Exhaustiveness:
  - Error for closed unions of primitives / literals / `Null` not covered by `is`/literal arms.
  - Warning otherwise.

### `lin-eval`
- [x] Sequential arm matching; on guard-false, continue to next arm.
- [x] No matching arm and no `else` → runtime error (with span and call stack).

### Tests
- [x] All pattern forms.
- [ ] Exhaustiveness error and warning fixtures.
- [x] Runtime fall-through error fixture.

---

## Milestone 10 — Generics with bidirectional inference

End state: `[1,2,3].map(i => i * i)` type-checks without annotations.

### `lin-check`
- [ ] Bidirectional checking: synthesise where possible, propagate expected types into lambdas and partial applications.
- [ ] Generic parameter substitution and unification at call sites.
- [ ] **Variance** (spec §8.8): covariant in producer positions, contravariant in consumer positions.
- [ ] **Numeric widening everywhere safe** (operators, returns, calls, assignments); never implicit narrowing.

### Tests
- [ ] Inference on `map`, `filter`, `reduce`.
- [ ] Widening across signed+unsigned and integer+float.
- [ ] `Person[]` assignable to `Json[]`.

---

## Milestone 11 — Modules and imports ✓

End state: multi-file programs with `import` and `export`.

### `lin-lex` / `lin-parse`
- [x] `export` modifier on `val`, `var`, `type`.
- [x] `import { a, b as c } from "path"` (single-line and multi-line).

### `lin-eval` (module loader)
- [x] Resolve `"a/b/c"` → `a/b/c.lin` relative to the importing file's directory.
- [x] Recognise `std/...` prefix → resolve into `lin-stdlib`.
- [x] Lazy init: first read of any export forces module init; cycles inside a single init chain are a runtime error.

### Tests
- [x] Multi-file fixture.
- [ ] Cyclic-import success and failure cases.

---

## Milestone 12 — Iterators and `for` ✓

End state: `range(0, 10).for(i => print(i))` runs.

### `lin-eval`
- [x] Opaque `Iterator` value carrying four closures + state cell (spec §27.6).
- [x] `iter` built-in constructs an iterator from the four functions.
- [x] Restartability: re-invoking the initial-state thunk yields a fresh logical start.
- [x] `for` built-in: the only direct stepper.
- [x] Arrays satisfy `Iterable<T>` automatically (compiler-known adapter to `iterOf`).

### `lin-stdlib`
- [x] `std/array`: `map`, `filter`, `reduce`, `length`, written on top of `for`.
- [x] `iterOf`, `range`.

### Tests
- [x] Each combinator against arrays and against `range`.
- [ ] Restart fixture.

---

## Milestone 13 — Closures, mutation, TCO ✓

End state: stateful counters work; deep self-recursion runs in constant stack.

### `lin-eval`
- [x] Closure capture: `val` by value, `var` as shared mutable cell (JS-style).
- [x] Two closures over the same `var` share storage.
- [x] Detect **direct self-recursive tail calls** and rewrite to a loop. Mutual TCO not required.

### Tests
- [x] `makeCounter` fixture.
- [x] Deep tail-recursive accumulator runs without stack overflow.

---

## Milestone 14 — Stdlib completion and tagged unions ✓

End state: every EXAMPLE.md flow runs.

### `lin-stdlib`
- [x] `std/result`: `Result<T, E>` and helpers.
- [x] `std/io`: `print`.
- [x] `std/string`: `trim`, `toUpper`, `toLower`, `substring`, `indexOf`, `length`, `at(s, i)`, codepoint-aware.
- [x] `std/number`: `parseInt32`, `parseFloat64`, `toInt32`, `toFloat64`, `isInt32`, etc.
- [x] `length()` over `String`, `T[]`, and `Json`.

### Tests
- [x] All EXAMPLE.md flows execute end-to-end.
- [ ] Explicit narrowing casts error on loss of information.

---

## Milestone 15 — Diagnostics polish

End state: usable error reporting.

- [ ] Spans carried through every AST node and type error.
- [ ] Diagnostics render surrounding source, the rule violated, and a call stack on runtime errors.
- [ ] Suggestions for common mistakes: unquoted JSON keys, missing `else` in `if`, mixing `is`/`has` in an arm, etc.
- [ ] Snapshot tests for representative error scenarios.

---

## Next

- What does async look like? What do long running threads look like? Do we want fork/join or do we want `worker.start`/`stop`?
- What does a mutex lock look like? `myVal.lock(lockedVal => ...)` — how about locking many vals at once?
- Implement IO and std input.
- Implement Http client.
- Implement Http server.
- Implement language server / VS Code support with syntax highlighting.

---

## Cross-cutting

These don't belong to any single milestone but must stay healthy throughout.

- [x] Each milestone adds at least one `examples/*.lin` fixture and one snapshot test per new feature.
- [ ] CI runs `cargo test --workspace` and each `examples/*.lin` on every change.
- [x] EXAMPLE.md fixtures parse-clean against the current `lin-parse`.

## Deferred (post-v1)

Tracked here so they don't get lost:

- Native or bytecode compilation target.
- Object rest-destructuring iteration-order guarantee.
- Whether `Iterable<T>` becomes a true protocol-like type or stays a compiler-known capability.
- Full pairwise numeric widening matrix and explicit-cast catalogue.
- Multi-error reporting (recoverable parse/check).
- Mutual tail-call optimisation.
