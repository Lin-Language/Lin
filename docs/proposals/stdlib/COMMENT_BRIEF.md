# JSDoc-style comment pass — shared convention

You are adding doc comments to Lin stdlib `.lin` modules. Lin uses `//` line comments only (NO `/* */`).
A "JSDoc-style" comment here means: a `//` comment block immediately above each `export`ed function (and
each `export type` / exported `val` constant) that describes WHAT it does, its parameters, return value,
and error behavior — in the spirit of JSDoc but written as plain `//` lines.

## The convention (match this shape on every exported symbol)
```
// One-line summary of what the function does (imperative mood, ends with a period).
// @param name  what it means / constraints (one line per param; omit if truly self-evident from the summary).
// @returns  what comes back, including the error/null arm for a `T | Error` / `T | Null` return.
export val foo = (name: Type): Ret => ...
```
Rules:
- EVERY exported symbol gets a comment header: `export val`, `export type`, exported constants.
- Use `@param`, `@returns`, and where useful `@example` (a `//   foo(2, 3)   // 5` line). Keep it tight —
  one line per param; don't pad. If a function is trivial (e.g. `one: BigInt`), a single summary line is fine,
  no `@param`/`@returns` needed.
- For fallible functions (`T | Error`), the `@returns` MUST state the error arm, e.g.
  "@returns the parsed value, or an `Error` if the string is not valid JSON".
- Match the existing house voice: precise, technical, no fluff. Look at `stdlib/object.lin` and
  `stdlib/array.lin` for tone.

## Strip / relocate implementation-detail comments
The user's explicit instruction: REMOVE comments that are purely implementation details, OR move them
INLINE inside the function body next to the code they explain.
- A comment ABOVE a function that explains HOW it works internally (RC discipline, codegen tricks, ADR
  rationale, "this dispatches on the boxed tag", buffer-copy mechanics) is an implementation detail. If it
  documents observable BEHAVIOR the caller needs, fold it into the `@returns`/summary. If it only explains
  the internals, MOVE it inline (a `//` line inside the body, next to the relevant code) — do not leave it
  as the function's doc header.
- The doc header should tell a CALLER what they need; inline comments serve a maintainer reading the body.
- Do NOT delete information wholesale — relocate it inline if it has maintenance value; only delete if it's
  redundant noise. When unsure, keep it inline rather than deleting.

## Scope & safety
- Only touch the module `.lin` files assigned to you (listed in your task). Do NOT touch `.test.lin` files,
  Rust files, or any module not in your list.
- Do NOT change any code — comments only. Signatures, bodies, logic stay byte-identical.
- Preserve the top-of-file module header comment and `import` lines.
- After editing, run `cargo run -p lin --quiet -- fmt --check stdlib/<yourfile>.lin` for each file — the
  formatter must still pass (comments must be well-formed). If fmt reflows something, accept its output.
- Then run `cargo run -p lin --quiet -- test stdlib/<yourfile>.test.lin` for any module that has a test
  file, to confirm you didn't accidentally break parsing. (Comments shouldn't affect tests, but verify.)

## Working directory
You are editing files in the consolidation worktree at the path given in your task. Use that absolute path
for every file. Do NOT cd elsewhere, do NOT git checkout/commit/merge — just edit the files and verify.
Report which files you commented and the fmt/test result.
