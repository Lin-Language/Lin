<p align="center">
  <img src="https://raw.githubusercontent.com/Lin-Language/Lin/master/logo.png" width="128" height="128" alt="Lin logo" />
</p>

# Lin Language

Language support for [**Lin**](https://github.com/Lin-Language/Lin) — a compiled, functional programming language built around JSON data, structural typing, pattern matching, and dot-chained pipelines.

The extension bundles the `lin` compiler and `lin-lsp` language server, so there is **nothing else to install** — syntax highlighting and type-aware editing work the moment it activates on a `.lin` file.

## Features

- **Syntax highlighting** for `.lin` files.
- **Diagnostics** — type errors and parse errors shown inline as you type.
- **Hover types** — hover over any expression to see its inferred type.
- **Inlay hints** — inferred type annotations shown inline on `val`/`var` bindings (on by default; hold `Ctrl`+`Alt` to toggle).
- **Semantic highlighting** — type-aware token colouring (variables, parameters, functions, types, namespaces), on by default.
- **Signature help** — parameter hints while typing a call.
- **Code actions** — quick fixes and refactors offered inline.
- **Go to definition** — jump to where a binding is declared.
- **Find references, rename, and workspace symbols** — cross-file navigation across your project.
- **Dot-completion with auto-import** — type `myArr.` and the completion list shows only functions that accept an array as their first argument (`map`, `filter`, `reduce`, …). Selecting one automatically inserts the `import` at the top of the file if it isn't already there.
- **Snippets** — idiomatic scaffolds for `import`, `val`/`var`, function literals, `type`/union declarations, `match`, dot-chained `map`/`filter`/`reduce`, and `test`/`suite` blocks. Type the prefix and press `Tab`.
- **Test Explorer & CodeLens** — `*.test.lin` suites are discovered into the Testing view with per-test gutter ▶ and a **Run Test** CodeLens; runs and code coverage are reported inline.
- **Tasks & Problems** — a `lin` task type (`build`/`run`/`test`) with a problem matcher that turns compiler diagnostics into clickable entries in the Problems panel.
- **Editor & explorer commands** — a ▶ Run button in the editor title bar plus a **Lin** submenu (Build / Run / Test / Format) on `.lin` files in the editor and explorer context menus.
- **`lin` on your PATH, no install step** — when the extension is active, the bundled `lin` is automatically added to the PATH of VS Code's integrated terminal, so `lin run foo.lin` just works. To use `lin` in any shell, run **Lin: Install `lin` on PATH** from the Command Palette.

Screenshots can be added here to showcase highlighting, the Testing view, and the Problems panel.

## Commands

Open the Command Palette (`Ctrl+Shift+P` / `Cmd+Shift+P`) and search for "Lin":

| Command | Description |
|---|---|
| **Lin: Build** | Compile the active `.lin` file to a native binary. |
| **Lin: Run** | Compile and run the active `.lin` file. |
| **Lin: Test** | Run the `*.test.lin` suites in the active file's directory. |
| **Lin: Test File** / **Lin: Test Project** | Run the active test file, or every suite in the project, via the Test Explorer. |
| **Lin: Format** | Format the active `.lin` file. |
| **Lin: Install `lin` on PATH** | Symlink the bundled `lin` into `~/.local/bin` for use in any terminal. |

A **Get Started with Lin** walkthrough (Help → Welcome) guides you through installing `lin`, writing a first program, and running it.

## Requirements

A C linker (`cc`) must be on your `$PATH` to link compiled programs — on macOS this comes with the Xcode Command Line Tools; on Linux install `gcc` or `clang`. No LLVM installation is required; it is bundled inside `lin`.

## Learn more

- [Lin on GitHub](https://github.com/Lin-Language/Lin)
- [Language specification](https://github.com/Lin-Language/Lin/blob/master/docs/SPECIFICATION.md)
- [Standard library reference](https://github.com/Lin-Language/Lin/blob/master/docs/STDLIB.md)

## License

MIT
