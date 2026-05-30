# Implementation Plan & Task List

## Strategy

We build the compiler in **vertical slices**: each milestone ends with a runnable end-to-end pipeline (lex → parse → check → execute) for a subset of the language. Each subsequent milestone widens the subset. This avoids "all parser, no semantics" purgatory and produces something demonstrable at every step.

The backend is the **LLVM native-code compiler** in `lin-codegen`.

Host language: **Rust**. Layout: Cargo workspace.

## Workspace Layout

```txt
lin-lang/
  Cargo.toml                 (workspace root)
  crates/
    lin-common/              shared types: Span, Diagnostic, intern table
    lin-lex/                 lexer, indentation tokenizer
    lin-parse/               parser, surface AST
    lin-check/               type checker, typed IR
    lin-ir/                  flat 3-address IR, liveness, RC elision
    lin-codegen/             LLVM backend via inkwell
    lin-runtime/             static library linked into every binary
    lin-compile/             compilation pipeline orchestration
    lin/                     the CLI binary (cargo install entry point)
    lin-lsp/                 language server (in progress)
  stdlib/                    stdlib .lin files
  docs/
  examples/
```

Each crate has its own `tests/` directory with unit and snapshot tests. Top-level `examples/` holds end-to-end `.lin` fixtures executed by `lin` in CI.

## Prerequisites

- [x] `cargo new --workspace` scaffolding and per-crate skeletons.
- [x] Snapshot-test crate dependency (e.g. `insta`) in dev-dependencies.
- [x] `lin-common`: `Span`, `SourceFile`, `Diagnostic` (with surrounding-source rendering and call-stack support), `Interner`.
- [x] `examples/` seeded with the smallest fixture (Milestone 1's hello world).
- [x] CI workflow: `cargo test --workspace`, run every `examples/*.lin` through `lin`.

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

### `lin-codegen` (M1 subset)
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

### `lin-codegen`
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

### `lin-codegen`
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

### `lin-codegen`
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

### `lin-codegen`
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
- [x] Internal type representation.
- [x] Structural compatibility check (used both for assignability and `has`).
- [x] Exact-shape check (used for `is`).
- [x] Recursive types: name-based with lazy unfolding and cycle-aware equality.
- [x] Reject self-referential non-function `val` (spec §7.3) — naturally blocked: non-function `val` name is not in scope during RHS evaluation.

### Tests
- [x] Compatible/incompatible assignment fixtures.
- [x] Recursive `Tree`, `Person` (with `"spouse": Person | Null`) fixtures.

---

## Milestone 8 — `is`, `has`, `if`, narrowing

End state: type narrowing in `if` branches via `is`/`has`.

### `lin-parse`
- [x] `value is Type | Literal | Shape` expressions.
- [x] `value has Type | Shape` expressions.
- [x] `is`/`has` permitted in any expression context (return type `Boolean`).

### `lin-check`
- [x] Narrowing rules per spec §25.
- [x] Reject `is` on generic applications — naturally blocked at parser level; parser only produces `Pattern::TypeName(string)`, not generic patterns.
- [x] Literal types: literal expressions have base type (no singleton types).

### Tests
- [x] Union narrowing in `if`/`else`.
- [x] `has { name }` accepts extra fields.
- [x] Reject `is Result<Int32, String>` — cannot be expressed in parser.

---

## Milestone 9 — Pattern matching ✓

End state: full `match` with all arm forms.

### `lin-parse`
- [x] `match scrutinee` with one arm per line.
- [x] `is` patterns, `has` patterns, `when` guards.
- [x] `else => expr` catch-all arm.
- [x] Reject mixed `is`/`has` in a single arm.

### `lin-check`
- [x] Narrowing per arm.
- [x] Exhaustiveness:
  - Error for closed unions of primitives / literals / `Null` not covered by `is`/literal arms.
  - Warning otherwise.

### `lin-codegen`
- [x] Sequential arm matching; on guard-false, continue to next arm.
- [x] No matching arm and no `else` → runtime error (with span and call stack).

### Tests
- [x] All pattern forms.
- [x] Exhaustiveness error and warning fixtures.
- [x] Runtime fall-through error fixture.

---

## Milestone 10 — Generics with bidirectional inference

End state: `[1,2,3].map(i => i * i)` type-checks without annotations.

### `lin-check`
- [x] Bidirectional checking: synthesise where possible, propagate expected types into lambdas and partial applications.
- [x] Generic parameter substitution and unification at call sites.
- [x] **Variance** (spec §8.8): covariant in producer positions, contravariant in consumer positions.
- [x] **Numeric widening everywhere safe** (operators, returns, calls, assignments); never implicit narrowing.

### Tests
- [x] Inference on `map`, `filter`, `reduce` — covered by compiler integration tests.
- [x] Widening across signed+unsigned and integer+float.
- [x] `Person[]` assignable to `Json[]`.

---

## Milestone 11 — Modules and imports ✓

End state: multi-file programs with `import` and `export`.

### `lin-lex` / `lin-parse`
- [x] `export` modifier on `val`, `var`, `type`.
- [x] `import { a, b as c } from "path"` (single-line and multi-line).

### `lin-compile` (module loader)
- [x] Resolve `"a/b/c"` → `a/b/c.lin` relative to the importing file's directory.
- [x] Recognise `std/...` prefix → resolve into `lin-stdlib`.
- [x] Lazy init: first read of any export forces module init; cycles inside a single init chain are a runtime error.

### Tests
- [x] Multi-file fixture.
- [x] Cyclic-import success: mutual recursion via forward-declaration tested. File-level cyclic import is a compile-time error.

---

## Milestone 12 — Iterators and `for` ✓

End state: `range(0, 10).for(i => print(i))` runs.

### `lin-codegen`
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
- [x] Restart fixture.

---

## Milestone 13 — Closures, mutation, TCO ✓

End state: stateful counters work; deep self-recursion runs in constant stack.

### `lin-codegen`
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
- [x] Explicit narrowing casts error on loss of information — tested in `test_narrowing_disallowed`.

---

## Milestone 15 — Diagnostics polish

End state: usable error reporting.

- [x] Spans carried through every AST node and type error.
- [x] Diagnostics render surrounding source, the rule violated, and a call stack on runtime errors.
- [x] Suggestions for common mistakes: unquoted JSON keys, missing `else` in `if`, mixing `is`/`has` in an arm, etc.
- [x] Snapshot tests for representative error scenarios (`crates/lin-check/tests/snapshots.rs` with `insta`).

---

## Milestone 16 — Compiler quality improvements

Improvements based on compiler architecture best-practice research (Maranget 2008, Dunfield & Krishnaswami 2013, Perceus PLDI 2021, Gleam/Roc/Koka reference implementations).

### Type checker

- [x] **TypeVar zonking pass** — after `check_module` completes, walk the `TypedModule` and replace all solved `TypeVar(id)` with their concrete solutions. Report unsolved TypeVars as errors so they never silently reach codegen. Reference: "Typing Haskell in Haskell" (Jones 1999). Effort: ~1 day.

- [x] **Maranget exhaustiveness checking** — implement the matrix-decomposition algorithm (Maranget 2008, "Compiling Pattern Matching to Good Decision Trees") for `match` exhaustiveness. Produce a counterexample witness when non-exhaustive. Study: `gleam/compiler-core/src/type_/pattern.rs` and `ocaml/typing/parmatch.ml`. Effort: ~1 week.

### Diagnostics

- [x] **Ariadne error rendering** — add `ariadne` crate for multi-span diagnostic rendering (used by Gleam, Roc). Extend `Diagnostic` with `notes: Vec<(Span, String)>` and `help: Option<String>`. Update CLI in `lin/src/main.rs` to use `ariadne::Report`. Effort: ~1 day.

- [x] **Parser error recovery** — on parse failure, push the diagnostic and skip to the next statement boundary (`Newline` after error) to report multiple errors per compile run. Effort: ~2 days.

- [x] **"Did you mean" suggestions** — for the 5 most common mistakes: unquoted JSON keys, missing `else`, wrong arity, `=` where `==` is needed, accessing a missing field on a known object type (edit distance). Effort: ~2 days each.

### Codegen

- [x] **LLVM `switch` for tag dispatch in `match`** — when arms reduce to tag tests on a union type, emit a single `switch i8 %tag` instruction instead of a chain of `icmp`/`br`. LLVM lowers this to an O(1) jump table. Effort: ~2 hours.

- [x] **Unbox monomorphic arrays** — when the element type is a known concrete scalar (e.g. `Int32`, `Float64`), emit a bare `*mut i32` / `*mut f64` array instead of `*mut LinArray` with tagged elements. Requires typed array variants in `lin-runtime` (`lin_array_alloc_i32`, `lin_array_get_i32`, etc.). The type checker already carries the element type. 5–10x improvement in array access for numeric code. Effort: ~3–4 days.

### Build system

- [x] **Module cache by source hash** — serialize `TypedModule` to `.lin-cache/<sha256(source)>.typed` using `serde` + `bincode`. On rebuild, deserialise and skip re-checking if hash matches. Instant rebuilds for unchanged modules. Effort: ~2–3 days.

- [x] **Module signatures** — extract a `ModuleSignature` (public types and function signatures only) from `TypedModule`. Dependents import `ModuleSignature`, not the full `TypedModule`, so an implementation change that doesn't alter the interface doesn't trigger re-checking of dependents. Reference: Haskell `.hi` files, rustc crate metadata. Effort: ~1 week.

### Mid-level IR (deferred until bottleneck is clear)

- [x] **Flat `LinIR`** — add a 3-address IR between `TypedExpr` and LLVM codegen. Enables: RC elision pass, escape analysis, dead code elimination, and proper liveness analysis before LLVM sees the code. Required prerequisite for Perceus reuse analysis. Study: Gleam 0.18→0.20 refactor, rustc MIR. Effort: ~2 weeks.

- [x] **RC elision pass** (requires `LinIR`) — eliminate retain/release pairs where a temporary's live range doesn't span any allocation or call site. Reference: Perceus (Reinking et al., PLDI 2021), Koka `src/Backend/C/ParcReuse.hs`. Effort: ~1 week after `LinIR`.

---

## Milestone 17 — Concurrency: `async`/`await`, `parallel`, `worker`

End state: thunks run on OS threads; `parallel` fork-joins; workers handle messages.

See spec §32 for the full design.

### New runtime value types (`lin-runtime`)

- [x] `Promise<T>` — opaque value wrapping `Arc<Mutex<PromiseState<T>>>` where state is `Pending | Resolved(Value) | Failed(String)`. Carries the join handle from `std::thread::spawn`.
- [x] `ThreadPool` — opaque value wrapping a fixed-size Rayon or manual thread-pool. Holds a sender end of a task channel.
- [x] `Worker<Msg, Reply>` — opaque value wrapping a sender channel to a dedicated OS thread that loops over incoming messages, calling `onMessage` for each and optionally sending a reply back.

### Static analysis (`lin-check`)

- [x] `var`-capture check: at every `async(f)` / `pool.async(f)` call site, verify the thunk closure captures no `var` bindings. Compile-time error if it does.
- [x] Transferability check: `T` in `Promise<T>` must be a JSON-compatible type (`String`, `Boolean`, `Null`, numeric, `T[]`, object with transferable fields). Reject `Function`, `Iterator`, `Iterable`, `Worker`, `ThreadPool`, `Promise` as return types of async thunks — error where statically detectable.

### Built-in functions

- [x] `async(f: () => T) => Promise<T | Error>` — spawns one OS thread; catches runtime errors and wraps them as `Error` values rather than halting the program.
- [x] `async(fs: (() => T)[]) => Promise<T | Error>[]` — overload; spawns one thread per thunk.
- [x] `await(p: Promise<T | Error>) => T | Error` — blocks the calling thread until resolved.
- [x] `await(ps: Promise<T | Error>[]) => (T | Error)[]` — overload; blocks until all resolve; result order matches input order.
- [x] `parallel(f1, f2, ...) => (T | Error)[]` — sugar for `await(async([f1, f2, ...]))`; variadic, all thunks must have the same return type.
- [x] `map` on `Promise<T | Error>` — transforms the value without blocking; returns a new `Promise`.
- [x] `race(ps: Promise<T | Error>[]) => Promise<T | Error>` — resolves with the first completed promise; others continue running but results are discarded.
- [x] `timeout(p: Promise<T | Error>, ms: Int32) => Promise<T | Error | Null>` — resolves `Null` if the promise does not complete within `ms` milliseconds; the underlying thread is abandoned (not cancelled).
- [x] `retry(f: () => T, n: Int32) => Promise<T | Error>` — re-spawns the thunk up to `n` times; returns the first non-`Error` result; if all attempts fail, returns the last `Error`.
- [x] `threadPool(n: Int32) => ThreadPool` — constructs a thread pool of `n` OS threads.
- [x] `pool.async(f)` and `pool.async(fs)` — same semantics as top-level `async` but dispatches work to the pool's threads.
- [x] `worker(onMessage: (Msg) => Reply, onShutdown: () => Null) => Worker<Msg, Reply>` — spawns a dedicated OS thread running the message loop.
- [x] `w.message(msg: Msg) => Null` — enqueues message; returns immediately.
- [x] `w.request(msg: Msg) => Reply` — enqueues message; blocks until handler returns.
- [x] `w.close() => Null` — drains the queue, calls `onShutdown`, terminates the worker thread. Subsequent sends are runtime errors.

### `print` thread safety

- [x] Wrap the stdout sink in a `Mutex` and flush one complete line atomically so output from concurrent threads does not interleave mid-line (spec §32.7).

### Tests

- [x] `async` + `await` round-trip: thunk returns value, caller receives it.
- [x] `var`-capture rejection: confirm compile-time error.
- [x] `val` + pure-function capture: confirm this compiles and runs correctly.
- [x] `parallel` with three thunks: result order matches input order regardless of which finishes first.
- [x] Runtime error inside a thunk surfaces as `Error` at `await`, does not halt the parent.
- [x] `race` resolves with whichever thunk returns first.
- [x] `timeout` returns `Null` for a thunk that sleeps past the deadline.
- [x] `retry` succeeds on the second attempt after a simulated first-attempt failure.
- [x] `threadPool(4).async([...])` distributes work across the pool.
- [x] `worker` round-trip: `message` delivers, `request` returns reply, `close` shuts down cleanly.
- [x] Stateful worker with `var` closure accumulates state correctly across sequential `request` calls.
- [x] `print` from multiple concurrent thunks produces complete, non-interleaved lines.

---

## Milestone 18 — IO, Filesystem, and HTTP

End state: programs can read stdin, read/write files, and make HTTP requests.

See spec §33 for the full intrinsic signatures. See `docs/STDLIB.md` for the complete public API.

### `std/io` — stdin

- [x] `__ioReadLine` intrinsic: block on `stdin.lock().lines().next()`, strip trailing newline, return `String | Null` (Null on EOF).
- [x] `__ioLines` intrinsic: read all lines from stdin eagerly, return as `Array<String>`.
- [x] `__ioReadAll` intrinsic: `io::read_to_string(&mut stdin)`, return `String`.
- [x] `std/io` Lin module: thin wrappers (`readLine`, `lines`, `readAll`) delegating to the above intrinsics.
- [x] `print` thread-safety: wrap stdout sink in a `Mutex` so concurrent async thunks don't interleave mid-line (spec §32.7).

### `std/fs` — filesystem

- [x] `__fsReadFile` intrinsic: `fs::read_to_string(path)`, return `String | Error` (OS error message as string).
- [x] `__fsWriteFile` intrinsic: `fs::write(path, content)`, return `Null | Error`.
- [x] `__fsAppendFile` intrinsic: open with `OpenOptions::append(true).create(true)`, write content, return `Null | Error`.
- [x] `__fsReadLines` intrinsic: open file, read all lines eagerly, return `Array<String> | Error`.
- [x] `__fsReadJson` intrinsic: read file then call internal JSON parser, return `Json | Error`.
- [x] `__fsWriteJson` intrinsic: serialise `Json` value to compact JSON string, write file, return `Null | Error`.
- [x] `__fsExists` intrinsic: `Path::new(path).exists()`, return `Boolean`.
- [x] `std/fs` Lin module: thin wrappers for all seven functions.
- [x] Resolve relative paths against the process working directory, not the source file directory.

### `std/http` — HTTP client

- [x] Add `ureq` (or equivalent minimal Rust HTTP client) to workspace dependencies.
- [x] `__httpFetch` intrinsic: send GET request, return `HttpResponse | Error`. Populate `"status"`, `"headers"`, `"body"`. Transport errors (DNS, TLS, connection refused) → `Error`; HTTP error status codes → successful `HttpResponse`.
- [x] `__httpFetchWith` intrinsic: same as `__httpFetch` but accepts `HttpOptions` object; read `"method"`, `"headers"`, `"body"` fields (missing fields use defaults: `"GET"`, empty headers, empty body).
- [x] `__parseJson` intrinsic (internal): parse a JSON string to `Json | Error`; used by `fetchJson` and `std/server`.
- [x] `std/http` Lin module: `fetch` and `fetchWith` delegate to intrinsics; `fetchJson` and `postJson` written in Lin on top of them (spec §33.4).
- [x] Define `HttpResponse` and `HttpOptions` as exported types from `std/http`.

### `std/server` — HTTP server

- [x] Add `tiny_http` crate dependency to workspace.
- [x] `__serverServe` intrinsic: bind TCP listener on `port`, accept in a loop, parse each connection into an `HttpRequest` object (`"method"`, `"path"`, `"query"`, `"headers"`, `"body"`), call `handler`, write the `HttpResponse` back to the socket. Block the calling thread indefinitely.
- [x] `__serverServeWithPool` intrinsic: same as above but hand each request to the `ThreadPool` task channel.
- [x] `pool.serve` dot-call: extend the runtime's dot-call dispatch on `ThreadPool` values so `.serve(port, handler)` routes to `__serverServeWithPool`.
- [x] `__serverPathMatch` intrinsic: split pattern and path on `/`; match literal segments exactly; collect `:name` segments as string captures into a result object; return `Null` on length mismatch or literal segment mismatch.
- [x] `std/server` Lin module: `serve`, `json`, `text`, `redirect`, `notFound`, `badRequest`, `parseBody`, `pathMatch` — written in Lin on top of the two server intrinsics, `__serverPathMatch`, and `__parseJson` (spec §33.5).
- [x] Export `HttpRequest` type from `std/server`.

### Tests

- [x] `std/io`: pipe a multi-line string to stdin, verify `lines()` yields each line correctly.
- [x] `std/io`: `readAll()` returns full piped content.
- [x] `std/io`: `readLine()` returns `Null` on empty stdin.
- [x] `std/fs`: write a temp file, read it back, verify contents match.
- [x] `std/fs`: `readLines` iterates all lines of a file.
- [x] `std/fs`: `readJson` / `writeJson` round-trip a JSON value.
- [x] `std/fs`: `exists` returns `true` for an existing file and `false` for a missing path.
- [x] `std/fs`: `readFile` on a missing path returns `Error`.
- [x] `std/http`: `fetchJson` against a local test server returns parsed JSON.
- [x] `std/http`: `postJson` sends correct `Content-Type` header and body.
- [x] `std/http`: HTTP 404 response is returned as `HttpResponse` (not `Error`); transport failure is `Error`.
- [x] Concurrent `async(() => fetchJson(...))` calls complete without data races.
- [x] `std/server`: `serve` on a background thread responds correctly to a GET request.
- [x] `std/server`: `pathMatch` extracts named parameters and returns `Null` on mismatch.
- [x] `std/server`: `parseBody` returns `Error` for non-JSON bodies.
- [x] `std/server`: `json` helper sets `Content-Type: application/json`; `text` sets `text/plain`; `redirect` sets `Location`.
- [x] `std/server`: `threadPool(4).serve` handles concurrent requests.

---

## Milestone 19 — Foreign Function Interface (C and Rust)

End state: a Lin program can call functions from a compiled C or Rust static library.

See spec §34 for the full design.

### Parser (`lin-parse`)

- [x] New token: `foreign` keyword (add to `TokenKind` and reserved keywords list).
- [x] Parse `import foreign "<path>"` followed by an indented block of `val name: Type` declarations into a new `Stmt::ForeignImport { path: String, bindings: Vec<ForeignBinding> }` AST node.
- [x] Reuse the existing indented-block parsing pattern; the block ends on dedent back to the `import` column.

### Type checker (`lin-check`)

- [x] Validate that every type in a `foreign` binding is a legal foreign function type (§34.3): params and return must be numeric, Boolean, Null, or String. Emit a compile-time error for any other type.
- [x] Register each foreign binding in the module's type environment with its declared type, exactly like a top-level `val`.
- [x] Thread the library path through to codegen; stored in `TypedStmt::ForeignImport`.

### Code generation (`lin-codegen`)

- [x] For each `ForeignImport`, emit an LLVM `declare` for every valid binding using the C ABI type mapping. Example: `val sqrt: (Float64) => Float64` → `declare double @sqrt(double)`.
- [x] Call sites in Lin code that target a foreign binding emit a direct `call` to the declared symbol.
- [x] Pass the library path(s) collected from `ForeignImport` nodes to the linker step in `lin-compile`.

### Compiler driver (`lin-compile`)

- [x] Collect all `ForeignImport` library paths from the compiled modules via `cg.foreign_lib_paths`.
- [x] Pass them to the linker invocation: static archives (`.a`) as positional arguments, shared libraries (`.so`/`.dylib`) as `-l<name>` with the containing directory added via `-L`.
- [x] Produce a clear error if the library file does not exist at the given path before invoking the linker.

### Runtime header (`lin.h`)

- [x] Add a `lin.h` C header (`crates/lin-runtime/lin.h`) defining `LinString` and `LinArray` so C/C++ library authors can include it.

### Tests

- [x] Compile-time check: `import foreign "..."` bindings are validated at compile time; illegal types produce a clear error.
- [x] Compile-time error: using `Json` in a foreign signature produces an "illegal FFI type" error.
- [x] End-to-end test: compile a C library to `.a`, call from Lin via `lin build`. (Requires compiler pipeline to be fully functional.)
- [x] `examples/ffi_c.lin` — end-to-end fixture calling a C test library.

---

---

## Milestone 20 — Stdlib Performance

End state: O(n log n) sort, O(n) string building, O(n) unique/omit, constant-factor improvements across map/zip/reverse/append.

### Critical fixes (algorithmic complexity)

- [x] **`sort` quicksort** — replaced O(n²) selection sort with in-place quicksort written in Lin using `lin_while` for partition. O(n log n).
- [x] **`lin_string_join_arr` intrinsic** — single-pass allocation in Rust. Transforms `string.join` and `string.fromCodePoints` from O(n²) to O(n).
- [x] **`lin_string_replace_all` intrinsic** — delegates to Rust's `str::replace`. Removes the O(n²) loop and wrong iteration bound.
- [x] **`lin_value_key` intrinsic + `std/hash`** — canonical type-tagged key for any value. Used by `unique` (O(n²) → O(n)) and `omit` (O(|obj|×|ks|) → O(|obj|+|ks|)). Exposed via `std/hash.hash(val)`.
- [x] **`unique` O(n) rewrite** — uses `lin_value_key` + plain object as seen-set.
- [x] **`omit` O(n) rewrite** — builds a skip-set from `ks` first, then single pass over object keys.

### Significant fixes (double key evaluations)

- [x] **Schwartzian transform for `sortBy` / `minBy` / `maxBy`** — `stdlib/array.lin` maps to `[keyFn(item), item]` pairs once (O(n) `keyFn` calls), sorts/reduces by `pair[0]`, then extracts values.
- [x] **`string.isBlank` allocation** — `lin_string_is_blank` intrinsic scans chars directly (`string.rs`); no trimmed copy.
- [x] **`string.startsWith` / `string.endsWith` allocation** — `lin_string_starts_with` / `lin_string_ends_with` intrinsics compare in-place via Rust `str::starts_with`/`ends_with` (`string.rs`); no substring copy.

### Minor fixes (constant-factor and allocation)

- [x] **`lin_array_alloc_sized(n)` intrinsic** — `lin_array_alloc(cap)` preallocates with the known output size; used by `map`, `zip`, `take`, `reverse`, `append`, `prepend`.
- [x] **`lin_array_concat` intrinsic** — `lin_array_concat_into(dst, src)` exists in `array.rs` for bulk copy. (`concat` in `stdlib/array.lin` still uses a manual `set` loop rather than the intrinsic — acceptable.)
- [ ] **`append` / `prepend` intermediate alloc** — no `lin_array_append` / `lin_array_prepend` intrinsic; stdlib allocates an `n+1` array and copies. Minor.
- [x] **`object.values` / `object.entries` two-pass** — `lin_object_values` / `lin_object_entries` intrinsics traverse the internal map in a single pass (`object.rs`); no intermediate key array.
- [x] **`countBy` two passes** — `stdlib/array.lin` `countBy` accumulates counts directly in a single pass; no longer routes through `groupBy`.
- [ ] **`groupBy` double key lookup** — still a null-check-then-push double lookup in Lin; no `lin_object_get_or_insert` intrinsic. Minor.

---

## Milestone 21 — Low-Level Primitives

End state: binary protocol code (byte parsing, packet (de)serialization), UDP sockets, subprocesses, and raw-terminal input are expressible in Lin. Validated against a real systems target (the `deathbot` UDP/RTP/NAL server and keyboard client).

See spec §35 for the full design. **Gated on the IR switchover landing on master** — all operator and intrinsic-dispatch work targets the post-IR pipeline (operators lowered as `LinIR::Binary`/`Unary` in `lin-ir`; intrinsics dispatched in `lin-ir/src/lower.rs`). Implementing against the old `codegen.rs` path would be thrown away by the migration.

Sequenced in layers. Layer 1 (bytes + bitwise) is the keystone — everything else depends on it, and it alone makes the protocol-parsing core (NAL/RTP, ~480 lines, no OS access) writable in Lin.

### Layer 1 — Bytes and bitwise operators

- [x] **Small-int flat array variants** (`lin-runtime/src/array.rs`) — full flat family (`alloc/push/get/set/free/alloc_filled/concat_into/eq/slice`) for `i8`, `u8`, `i16`, `u16` present via macro expansion, plus `lin_flat_to_tagged_*` converters.
- [x] **Flat-scalar dispatch** (`lin-codegen`) — `is_flat_scalar` and `flat_suffix` cover `Int8/UInt8/Int16/UInt16` (suffixes `i8/u8/i16/u16`) alongside the 32/64-bit families.
- [x] **Bitwise tokens** (`lin-lex`) — new tokens `Amp` (`&`), `Caret` (`^`), `Tilde` (`~`). Maximal-munch: lone `&` → `Amp`, `&&` → existing `And`. Reuse existing `Pipe` (`|`) in value position. NOTE: `<<`/`>>` are deliberately NOT lexed as combined `Shl`/`Shr` tokens — that would break nested generic close `>>` (`Promise<Promise<Int32>>`). Shifts are detected at the parser level from two ADJACENT `Lt`/`Gt` tokens in value position (`parse_shift_expr` / `adjacent_pair`).
- [x] **Bitwise AST + parser** — `BinOp::{BAnd, BOr, BXor, Shl, Shr}` and surface `UnaryOp::BNot` + `Expr::UnaryOp` (ast.rs). Precedence rungs per spec §24.2: `~` above `*`; `<<`/`>>` between `+`/`-` and comparison; `&`, `^`, `|` between `==`/`!=` and `&&`, in that order (`parse_bitor_expr` → `parse_bitxor_expr` → `parse_bitand_expr` → ... → `parse_shift_expr` → `parse_additive_expr` → ... → `parse_unary_expr`).
- [x] **Bitwise type rules** (`lin-check`, `infer_binary_op` + new `infer_unary_op`) — integer-only operands; float operand is a compile-time error. `& | ^`: result = widened integer type (reuse `widen_numeric`). `<< >>`: result = left operand's type. `~x`: result = type of `x`. TypeVar/dynamic operands fall back to the other side's type or `Int32`.
- [x] **Bitwise codegen / IR lowering** (`lin-ir/src/lower.rs`, `lin-codegen`) — `BinOp::{BAnd,BOr,BXor,Shl,Shr}` map to LLVM `build_and/or/xor/left_shift/right_shift` (logical shift for unsigned, arithmetic for signed via `lty.is_signed()`); surface `UnaryOp::BNot` lowers to IR `UnaryOp::Not` → `build_not`.
- [ ] **`slice` function** — `slice(arr, start, end)` in `std/array` (and re-exported from `std/bytes`), backed by `lin_flat_array_slice_<suffix>` for flat element types and the tagged-array slice path otherwise. No range-index syntax. NOTE: the runtime `lin_flat_array_slice_<suffix>` intrinsics already exist (`array.rs`); only the stdlib `slice` wrapper is missing.
- [ ] **Float bit-reinterpret intrinsics** (`lin-runtime`) — `lin_f32_to_bits`/`lin_f32_from_bits`/`lin_f64_to_bits`/`lin_f64_from_bits` (`f32::to_bits` etc.).
- [ ] **`std/bytes` module** (pure Lin + the four float intrinsics) — `u16/u32/u64` big- and little-endian read/write via shift-and-mask; `f32/f64` (de)serialization via the bit-reinterpret intrinsics; `slice`.

### Layer 2 — Sockets

- [ ] **UDP intrinsics** (`lin-runtime/src/net.rs`) — `lin_udp_bind`, `lin_udp_recv`, `lin_udp_recv_from`, `lin_udp_send_to`, `lin_udp_set_nonblocking`, `lin_udp_close`. fd returned as opaque `Int32` (spec §35.4); non-blocking would-block surfaces as `Null`, not `Error`.
- [ ] **TCP intrinsics** (`lin-runtime/src/net.rs`) — `lin_tcp_listen`, `lin_tcp_accept`, `lin_tcp_connect`, `lin_tcp_recv`, `lin_tcp_send`, `lin_tcp_set_nonblocking`, `lin_tcp_close`. `accept` returns a connection fd + peer addr (or `Null` when would-block); `recv` returns `0` on peer-closed.
- [ ] **`std/net` module** — UDP: `udpBind`, `udpRecv`, `udpRecvFrom`, `udpSendTo`, `udpSetNonblocking`, `udpClose`. TCP: `tcpListen`, `tcpAccept`, `tcpConnect`, `tcpRecv`, `tcpSend`, `tcpSetNonblocking`, `tcpClose`. All wrap intrinsics in the `T | Error` / `Null`-on-would-block convention.

### Layer 3 — Subprocess and raw terminal

- [ ] **Subprocess intrinsics** — `lin_proc_spawn(String[])`, `lin_proc_read_stdout`, `lin_proc_kill`, `lin_proc_wait`; opaque `Int64` handle. **`std/proc`**: `spawn`, `readStdout`, `kill`, `wait`.
- [ ] **Raw-TTY intrinsics** — `lin_tty_raw_mode(Boolean)`, `lin_tty_read_key()` (non-blocking). **`std/tty`**: `rawMode`, `readKey` (`Int32 | Null`).

### Layer 4 — Timing, signals; FFI and Worker for the rest

- [ ] **`std/time.sleepMicros`** — `lin_time_sleep_micros(Int64)` intrinsic + wrapper.
- [ ] **`std/signal.waitSignal`** — `lin_signal_wait(Int32)` intrinsic + wrapper. (Open: blocking-wait vs registered-handler form — decide here.)
- [ ] **GPIO via existing FFI** — no new core primitive; validate `import foreign` against a C GPIO library, using `sleepMicros` for software PWM.
- [ ] **Cross-thread state via Worker** — no new core primitive; confirm a `Worker<Msg, Reply>` owning shared state (e.g. a discovered client address) replaces `Arc<Mutex<…>>`.

### Spec / docs amendments

- [x] Spec §35 (this section), §24.1/§24.2 (bitwise operators + precedence), §3.7 (`~` is the one unary).
- [ ] `docs/STDLIB.md` — full signatures for `std/bytes`, `std/net`, `std/proc`, `std/tty`, `std/signal`, and the `std/time.sleepMicros` addition.
- [ ] `docs/DECISIONS.md` — ADRs for (a) fd-as-opaque-Int handle convention, (b) share-nothing upheld over a Mutex primitive, (c) flat unboxed small-int arrays, (d) `~` as the single sanctioned unary operator.

### Tests

- [ ] `UInt8[]` literals, indexing, in-place write, `length`, `push`, `slice`, `==`.
- [ ] Each bitwise operator (`& | ^ << >> ~`) with integer fixtures; float-operand rejection is a compile-time error; precedence fixtures.
- [ ] `std/bytes` round-trips: `u32ToBe`/`u32FromBe`, `f32ToBits`/`f32FromBits`, the 8-byte two-f32 control packet.
- [ ] `std/net`: UDP loopback send/recv; non-blocking recv returns `Null` when no data.
- [ ] `std/net`: TCP loopback — listener accepts a connection, echoes bytes back to a connected client; `recv` returns `0` after the peer closes.
- [ ] `std/proc`: spawn a process, read its stdout to EOF, exit code via `wait`.
- [ ] `examples/`: a NAL-parser / RTP-packetizer fixture (the protocol core, no OS), plus a UDP echo fixture.

---

## Next

- Tidy up stdlib, add .at for strings and re-implement into more native Lin
- Implement language server / VS Code support with syntax highlighting.
- More stdlib utilities

---

## Cross-cutting

These don't belong to any single milestone but must stay healthy throughout.

- [x] Each milestone adds at least one `examples/*.lin` fixture and one snapshot test per new feature.
- [x] CI runs `cargo test --workspace` and each `examples/*.lin` on every change.
- [x] EXAMPLE.md fixtures parse-clean against the current `lin-parse`.

## Deferred (post-v1)

Tracked here so they don't get lost:

- Native or bytecode compilation target.
- Object rest-destructuring iteration-order guarantee.
- Whether `Iterable<T>` becomes a true protocol-like type or stays a compiler-known capability.
- Full pairwise numeric widening matrix and explicit-cast catalogue.
- Multi-error reporting (recoverable parse/check).
- Mutual tail-call optimisation.
