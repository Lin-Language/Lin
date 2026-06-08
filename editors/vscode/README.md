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

## Debugging

Lin supports source-level debugging of compiled programs (breakpoints and stepping in your `.lin` files) via DWARF line tables emitted by `lin build --debug`.

Debugging delegates to **[CodeLLDB](https://marketplace.visualstudio.com/items?itemName=vadimcn.vscode-lldb)** (`vadimcn.vscode-lldb`). It is **not** a hard dependency of this extension — the rest of the extension (syntax highlighting, diagnostics, tasks, tests) works without it. CodeLLDB is only needed for the debugger: the first time you press **F5** on a `.lin` file, if CodeLLDB isn't installed the extension prompts you to install it (with a one-click button) and then aborts that launch cleanly. Install it and press F5 again.

To debug: open a `.lin` file, set a breakpoint in the gutter, and press **F5**. With no `launch.json` the extension auto-supplies a "Debug Lin file" configuration; it builds the active file with `lin build --debug` and launches it under CodeLLDB. To customise, add a configuration of `"type": "lin"` to `launch.json`:

```json
{
  "type": "lin",
  "request": "launch",
  "name": "Debug Lin file",
  "source": "${file}",
  "program": "${fileDirname}/${fileBasenameNoExtension}",
  "cwd": "${workspaceFolder}",
  "args": []
}
```

`source` is the `.lin` file built with `--debug`; `program` is the resulting binary that is debugged.

### Inspecting values

When stopped at a breakpoint, the **Variables** and **Watch** panels show *logical Lin values* rather than the raw boxed runtime structs: integers/floats/booleans/`null` inline, strings as quoted text, arrays as `[1, 2, 3]`, and objects as `{ "a": 1, "b": true }` (expandable in the tree). This is done by lldb data formatters (`formatters/lin_formatters.py`) that decode Lin's tagged-value representation; the extension auto-imports them into every debug session via the CodeLLDB `initCommands`. The decoding is read-only — it never calls into the debuggee or mutates refcounts.

> Note: associating Lin *locals* with names/types in the panel automatically depends on richer DWARF (local-variable / type info) emitted by a later phase of the compiler. Until then the formatters are best driven from the **Watch** panel / Debug Console by casting a known address to the runtime value type, e.g. in the Debug Console:
>
> ```
> p (lin_runtime::array::LinArray*)<addr>
> p (lin_runtime::object::LinObject*)<addr>
> ```
>
> These render through the same formatters that will light up the Variables panel automatically once that compiler phase lands.

## Requirements

A C linker (`cc`) must be on your `$PATH` to link compiled programs — on macOS this comes with the Xcode Command Line Tools; on Linux install `gcc` or `clang`. No LLVM installation is required; it is bundled inside `lin`.

For debugging, the **CodeLLDB** extension (`vadimcn.vscode-lldb`) is required. It is not installed automatically — the extension prompts you to install it the first time you start a debug session (press F5) if it isn't already present.

## Learn more

- [Lin on GitHub](https://github.com/Lin-Language/Lin)
- [Language specification](https://github.com/Lin-Language/Lin/blob/master/docs/SPECIFICATION.md)
- [Standard library reference](https://github.com/Lin-Language/Lin/blob/master/docs/STDLIB.md)

## License

MIT
