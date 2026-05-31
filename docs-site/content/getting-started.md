# Getting Started

This guide walks you through installing Lin, writing your first program, and getting oriented in the language.

## Installation

### Install script (recommended)

The quickest way to get Lin is the install script:

```bash
curl -fsSL https://raw.githubusercontent.com/Lin-Language/Lin/master/install.sh | sh
```

To choose where the library and binary are installed, set `LIN_LIB_DIR` and `LIN_BIN_DIR`:

```bash
curl -fsSL https://raw.githubusercontent.com/Lin-Language/Lin/master/install.sh | \
  LIN_LIB_DIR="$HOME/.local/lib/lin" LIN_BIN_DIR="$HOME/.local/bin" sh
```

Verify the installation:

```bash
lin --version
```

### VS Code extension

If you use VS Code, the **Lin Language** extension is the easiest way to get started — it bundles the `lin` compiler and the `lin-lsp` language server, so there is nothing else to install.

Install it from the Marketplace (search for **"Lin Language"** in the Extensions view), or grab `lin-lang.vsix` from the [latest release](https://github.com/Lin-Language/Lin/releases/tag/latest) and install it from the command line:

```bash
code --install-extension lin-lang.vsix
```

The extension provides syntax highlighting, inline type/parse diagnostics, hover types, go-to-definition, and dot-completion with auto-import, plus **Lin: Build / Run / Test** commands from the Command Palette. It also puts the bundled `lin` on the PATH of VS Code's integrated terminal — run the **Lin: Install `lin` on PATH** command to make it available in every shell.

### Build from source

Building from source requires a Rust toolchain and LLVM 22.

```bash
git clone https://github.com/Lin-Language/Lin.git
cd Lin
cargo build --workspace
# The binary is at target/debug/lin
```

## Your first program

Create a file called `hello.lin`:

```lin
import { print } from "std/io"

print("hello, world")
```

Run it directly:

```bash
lin run hello.lin
```

Or compile it to a standalone native binary and run that:

```bash
lin build hello.lin -o hello
./hello
```

Output:

```
hello, world
```

## The CLI

Lin ships a single `lin` binary with a few subcommands:

- `lin run path/to/main.lin` — compile and run
- `lin build src/main.lin -o myapp` — build a native binary
- `lin check src/main.lin` — type-check only
- `lin test src/` — run `*.test.lin` suites

## What's next?

Work through the tutorials in order to learn the language properly:

1. [Hello World & I/O](/tutorials/01-hello-world.html) — your first Lin program
2. [Values & Types](/tutorials/02-values-and-types.html) — the type system
3. [Functions](/tutorials/03-functions.html) — functions and closures
4. [Working with JSON](/tutorials/04-json-data.html) — objects and arrays
5. [Pattern Matching](/tutorials/05-pattern-matching.html) — match and is/has
6. [Arrays & Iteration](/tutorials/06-arrays-and-iteration.html) — map/filter/reduce
7. [Modules](/tutorials/07-modules.html) — imports and exports
8. [Error Handling](/tutorials/08-error-handling.html) — errors as values
9. [Concurrency](/tutorials/09-concurrency.html) — native threads and workers
```