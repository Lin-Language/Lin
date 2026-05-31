# Lin

Lin is a compiled, functional programming language with modern ergonomics, including:

- JSON as the native data model
- Dot application & partial application
- Pattern matching with structural `is`/`has`
- Native threads with no function colouring — share-nothing concurrency
- A robust structural type system — union types, generics, and exhaustiveness checking
- Errors as values, safe-by-default access

```lin
import { print } from "std/io"
import { filter, map, for } from "std/array"
import { toString } from "std/string"

val players = [
  { "name": "Alice", "score": 42 },
  { "name": "Bob",   "score": 17 },
  { "name": "Carol", "score": 91 }
]

players
  .filter(p => p["score"] >= 20)
  .map(p => "${p["name"]}: ${toString(p["score"])}")
  .for(line => print(line))
```

---

macOS (Apple Silicon) and Linux (x86_64):

```bash
curl -fsSL https://raw.githubusercontent.com/Lin-Language/Lin/master/install.sh | sh
```

The script detects your platform, downloads the matching release, and installs
the `lin` compiler, the `lin-lsp` language server, and `liblin_runtime.a` (the
runtime linked into every program you build) into `/usr/local/lib/lin`, with a
`lin` symlink on your `$PATH`. It uses `sudo` only for the directories that need
it. To install somewhere you own without `sudo`, set the target directories:

```bash
curl -fsSL https://raw.githubusercontent.com/Lin-Language/Lin/master/install.sh \
  | LIN_LIB_DIR="$HOME/.local/lib/lin" LIN_BIN_DIR="$HOME/.local/bin" sh
```

The binary is self-contained — no LLVM installation required. A C linker (`cc`)
must be on your `$PATH` to link compiled programs; on macOS this comes with
Xcode Command Line Tools, on Linux install `gcc` or `clang`.

Prefer to do it by hand, or on another platform? Grab a tarball from the
[latest release](https://github.com/Lin-Language/Lin/releases/tag/latest),
extract its three files into one directory, and put that directory on your
`$PATH`.

**Verify**
```bash
lin --version
```

---

## VS Code Extension

Download `lin-lang.vsix` from the [latest release](https://github.com/Lin-Language/Lin/releases/tag/latest) and install it:

```bash
code --install-extension lin-lang.vsix
```

The extension includes:

- **Syntax highlighting** for `.lin` files
- **Diagnostics** — type errors and parse errors shown inline as you type
- **Hover types** — hover over any expression to see its inferred type
- **Go to definition** — jump to where a binding is declared
- **Dot-completion with auto-import** — type `myArr.` and the completion list shows only functions that accept an array as their first argument (`map`, `filter`, `reduce`, …). Selecting one automatically inserts the `import` at the top of the file if it isn't there yet.
- **Lin: Build / Run / Test** commands — open the Command Palette (`Ctrl+Shift+P` / `Cmd+Shift+P`) and search for "Lin" to compile, run, or test the active file without leaving the editor.

The extension bundles the `lin` compiler and `lin-lsp` language server — no separate installation required.

**`lin` on your PATH, no curl needed.** When the extension is active, the
bundled `lin` is automatically added to the PATH of VS Code's integrated
terminal — open a terminal and `lin run foo.lin` just works. To use `lin` in
**any** shell (outside VS Code too), run the **Lin: Install `lin` on PATH**
command from the palette: it symlinks the bundled compiler into `~/.local/bin`.
Both always point at the version shipped with the installed extension, and the
integrated-terminal entry is removed automatically when you uninstall.

---

## Quick Start

```bash
# Compile and run immediately
lin run examples/calc/main.lin

# Compile to a binary
lin build examples/calc/main.lin -o calc
./calc

# Type-check without compiling
lin check examples/calc/main.lin
```

---

## Commands

### `lin build` — compile to a native binary

```bash
lin build <file.lin> [options]
```

| Flag | Description |
|---|---|
| `-o, --output <path>` | Output binary path (default: source filename stem) |
| `--emit-ir` | Write LLVM IR (`.ll` file) alongside the binary |
| `--no-opt` | Disable optimisation passes (faster compile, slower output) |
| `--verbose` | Print build timing |

```bash
lin build src/main.lin -o myapp
lin build src/main.lin --emit-ir --verbose
```

### `lin run` — compile and execute

Compiles to a temporary binary, runs it, and forwards its exit code. Arguments after `--` are passed to the compiled program.

```bash
lin run <file.lin> [options] [-- <args>...]
```

| Flag | Description |
|---|---|
| `--no-opt` | Disable optimisation (faster startup) |
| `--emit-ir` | Write LLVM IR alongside the temp binary |

```bash
lin run src/main.lin
lin run src/main.lin -- --port 8080
```

### `lin check` — type-check only

```bash
lin check <file.lin>
```

Reports type errors without producing a binary. Useful as a pre-commit hook or editor integration.

### `lin test` — run test suites

Discovers `*.test.lin` files and compiles + runs each one. A test binary exits 0 to pass, non-zero to fail.

```bash
lin test [paths...] [options]
```

| Flag | Description |
|---|---|
| `--filter <str>` | Only run tests whose path contains `<str>` |
| `--parallel <N>` | Number of parallel runners (default: CPU count) |
| `--timeout <secs>` | Kill a test binary after this many seconds (default: 30) |
| `-v, --verbose` | Show stdout/stderr from passing tests too |
| `--coverage` | Instrument binaries for source coverage |
| `--format <fmt>` | Coverage output format: `console` (default) or `llvm-cov` |
| `--output <path>` | Output file for `--format=llvm-cov` (default: `lcov.info`) |

```bash
# Run all tests in a directory (recursive)
lin test src/

# Glob patterns
lin test 'src/**/*.test.lin'

# Run matching tests in parallel
lin test src/ --filter=array --parallel=8

# Coverage summary in the terminal
lin test src/ --coverage

# Write lcov.info for CI upload (e.g. Codecov)
lin test src/ --coverage --format=llvm-cov --output=coverage/lcov.info
```

### `lin watch` — rebuild on file changes

Watches for file changes and re-runs a command automatically. Debounces 200 ms.

```bash
lin watch <file> [options]
```

| Flag | Description |
|---|---|
| `--command <cmd>` | What to re-run: `build` (default), `run`, `test` |
| `--include <glob>` | Only trigger on paths matching this glob (comma-separated or repeated) |
| `--exclude <glob>` | Never trigger on paths matching this glob |

```bash
# Rebuild on any change under the source directory
lin watch src/main.lin

# Rebuild and run on .lin or .json changes, ignoring generated files
lin watch src/main.lin --command=run \
  --include='**/*.lin,**/*.json' \
  --exclude='**/*.lang.json'

# Re-run tests on change
lin watch src/ --command=test
```

### `lin clean` — remove build artefacts

```bash
lin clean [path]
```

Removes all `.lin-cache/` directories under the given path (default: current directory).

---

## Language Tour

### Values and bindings

```lin
val x = 42
val name = "Alice"
val active = true
val nothing = null
```

`val` bindings are immutable. Use `var` for mutable bindings:

```lin
var count = 0
count = count + 1
```

### Functions

```lin
val add = (a: Int32, b: Int32): Int32 => a + b

print(toString(add(3, 4)))   // 7
```

Multi-statement bodies use indentation:

```lin
val gradeFor = (avg: Int32): String =>
  match avg
    has Int32 when avg >= 90 => "A"
    has Int32 when avg >= 80 => "B"
    has Int32 when avg >= 70 => "C"
    else => "F"
```

Parameters may have default values (which must come last, and may reference
earlier parameters). Omit a trailing argument to use its default:

```lin
val greet = (name: String, greeting: String = "Hello") => "${greeting}, ${name}"

print(greet("World"))         // Hello, World
print(greet("World", "Hi"))   // Hi, World
```

Supplying fewer arguments than declared with a trailing comma partially applies
the function instead:

```lin
val add = (a: Int32, b: Int32) => a + b
val addTen = add(10,)         // a function awaiting `b`
print(toString(addTen(5)))    // 15
```

### Dot chaining

`x.f(y)` is sugar for `f(x, y)`:

```lin
val result = "  hello  ".trim().toUpper()
print(result)   // HELLO
```

### Pattern matching

```lin
val describe = (input: String | Int32 | Null): String =>
  match input
    is Null    => "nothing"
    is Int32   => "an integer"
    is String  => "a string"
```

Destructure objects with `has`:

```lin
val describePerson = (p: Json): String =>
  match p
    has { name, age } when age > 30 => "Old: ${name}"
    has { name }                     => "Young: ${name}"
    else                             => "unknown"
```

### Arrays and pipelines

```lin
val numbers = [1, 2, 3, 4, 5]

val evens   = numbers.filter(n => n % 2 == 0)
val doubled = evens.map(n => n * 2)
val total   = doubled.reduce(0, (acc, n) => acc + n)

print(toString(total))   // 12
```

### String interpolation

```lin
val name = "Lin"
val version = 1
print("${name} v${toString(version)}")
```

### Imports

```lin
import { trim, toUpper } from "std/string"
import { parseInt32 }    from "std/number"
import { print }         from "std/io"
import { square }        from "lib/math"   // relative path
```

### Value-based error handling

```lin
val divide = (a: Float64, b: Float64): Json =>
  if b == 0.0
    then { "type": "failure", "error": "division by zero" }
    else { "type": "success", "value": a / b }

val result = divide(10.0, 2.0)
val message = match result
  has { "type": "success", value } => "Result: ${toString(value)}"
  has { "type": "failure", error } => "Error: ${error}"

print(message)
```

---

## Standard Library

| Module | Exports |
|---|---|
| `std/io` | `print`, `readLine`, `readAll`, `lines` |
| `std/string` | `trim`, `toUpper`, `toLower`, `split`, `join`, `contains`, `replace`, `startsWith`, `endsWith`, `indexOf`, `charAt`, `repeat` |
| `std/number` | `parseInt32`, `parseFloat64`, `isInt32`, `toInt32`, `toFloat64` |
| `std/array` | `map`, `filter`, `reduce`, `for`, `range`, `length`, `push`, `concat` |
| `std/iter` | `iter`, `range`, iterator combinators |
| `std/fs` | `readFile`, `writeFile`, `appendFile`, `readLines`, `readJson`, `writeJson`, `exists` |
| `std/http` | `fetch`, `fetchWith`, `fetchJson`, `postJson`, `serve`, `json`, `text`, `redirect`, `notFound`, `badRequest`, `parseBody`, `matchPath` |

### Concurrency

```lin
// Spawn a background task
val p = async(() =>
  val result = fetchJson("https://api.example.com/data")
  result["value"]
)

// Block until done
val value = await(p)
print(toString(value))

// Fork-join: run tasks in parallel, collect results in order
val results = parallel(
  () => computeA(),
  () => computeB(),
  () => computeC()
)
```

### HTTP server

```lin
import { serve, json, text, matchPath } from "std/http"

val router = (req: Json): Json =>
  match req["path"]
    is path when matchPath(path, "/users/:id") != null =>
      val m = matchPath(path, "/users/:id")
      json(200, { "userId": m["id"] })
    else => text(404, "not found")

router.serve(8080)
```

### Foreign functions (C / Rust interop)

Call functions from compiled C or Rust static libraries. Requires `lin build`.

```lin
import foreign "libmathlib.a"
  val sqrt: (Float64) => Float64
  val add: (Int32, Int32) => Int32

print(toString(sqrt(2.0)))   // 1.4142...
```

The C header `crates/lin-runtime/lin.h` defines `LinString` and `LinArray` for passing non-primitive types across the boundary. See `examples/ffi/main.lin` for a complete example.

---

## Building from Source

**Prerequisites:** Rust toolchain, LLVM 22 (`llvm-22-dev`, `libpolly-22-dev`), a C linker.

```bash
git clone https://github.com/Lin-Language/Lin
cd lin-lang
cargo build --release -p lin
# binary is at target/release/lin
```

Running the test suite:

```bash
cargo test --workspace
lin test stdlib/
```

---

## Project Layout

```
crates/
  lin-common/   shared Span, Diagnostic, edit-distance helpers
  lin-lex/      lexer
  lin-parse/    parser and surface AST (with error recovery)
  lin-check/    type checker — produces TypedModule
  lin-ir/       flat 3-address IR, liveness analysis, RC elision pass
  lin-codegen/  LLVM backend (via inkwell)
  lin-runtime/  runtime library linked into compiled binaries
  lin-compile/  compilation pipeline (lex → parse → check → codegen → link)
  lin/          CLI binary
  lin-lsp/      language server (in progress)
stdlib/         standard library (.lin)
examples/       example programs
docs/           language specification and design decisions
```
