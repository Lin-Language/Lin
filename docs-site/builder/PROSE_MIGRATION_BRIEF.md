# Prose migration brief — move docs-site prose INTO the .lin module headers

The docs-site stdlib pages are now GENERATED from the `//` comments in `stdlib/*.lin` by
`docs-site/builder/gen-stdlib.lin`. The single source of truth is the `.lin` file. Your job: take the
RICH HAND-WRITTEN PROSE that used to live in the old `content/stdlib/<mod>.md` and fold it into the
`.lin` module so the generated page keeps that quality.

## The two sources you reconcile (per module)
1. ORIGINAL hand-written page (caller-facing prose + worked examples), recover with:
   `git show HEAD:docs-site/content/stdlib/<mod>.md`
   (Some modules have no original — `regex/crypto/encoding/random/csv/bignum/decimal/url/os/result`
   were new or consolidated; for those just polish the existing `.lin` header, no recovery needed.
   Note url→http, os→process, result→object, hash→encoding: their prose belongs in the TARGET module.)
2. CURRENT module source `stdlib/<mod>.lin` — its top-of-file `//` header comment and per-function
   `//` doc comments.

## What to do for each assigned module
A. MODULE HEADER: Make the top-of-file `//` comment block a good caller-facing intro, drawing the
   useful prose from the original page's intro (the text before its first `## ` heading). Keep it
   caller-facing (what the module is for, key conventions, import example). MOVE any maintainer-only
   notes (import-cycle rationale, codegen/RC mechanics) to an inline `//` lower in the file or drop
   them from the header — they don't belong in user docs. Convert any `/stdlib/x.html` links to plain
   prose or `std/x` references (the generated page is markdown; cross-refs as `std/iter` text are fine).
B. EXAMPLES: the original pages have worked examples (```lin blocks with `// result` comments). For
   the valuable ones, fold them into the relevant function's doc comment as an `@example` line, e.g.
   `// @example [3,1,2].sort((a,b)=>a-b)   // [1,2,3]`. Don't migrate every trivial one; keep the
   illustrative ones. (The generator renders `@example` lines under the function.)
C. Do NOT change any code — comments only. Signatures/bodies/logic stay byte-identical.

## Comment doc format (match what's already there)
```
// One-line summary (imperative, ends with a period).
// @param name  meaning/constraints.
// @returns what it returns, including the error/null arm for T|Error / T|Null.
// @example expr   // result
export val foo = ...
```
The module header is the contiguous `//` block at the very top, BEFORE the first `import`/`export`.

## Verify (run from the consolidation worktree dir, which is your cwd)
After editing your modules:
1. `cargo run -p lin --quiet -- fmt --check stdlib/<mod>.lin` for each — must pass (apply `fmt` if it
   wants a reflow, then re-check).
2. `cargo run -p lin --quiet -- test stdlib/<mod>.test.lin` for each that has a test — must pass.
3. Regenerate and eyeball YOUR pages: `cargo run -p lin --quiet -- run docs-site/builder/gen-stdlib.lin`
   then read `docs-site/content/stdlib/<mod>.md` — confirm the intro reads well and examples render.

## Scope & safety
- Touch ONLY your assigned `stdlib/*.lin` files. Do NOT touch other modules, `.test.lin`, Rust, the
  generator, or the generated `.md` (those are rebuilt). Do NOT run git commit/checkout/merge.
- Work only in the consolidation worktree by the absolute paths given in your task.
Report: modules done, fmt/test results, and a one-line note on what prose you migrated per module.
