# Shared implementation brief — stdlib module build

You are implementing a NEW (or enrichment) stdlib module for the Lin language, from an
already-written design proposal in this same directory. You are in an ISOLATED GIT WORKTREE.
Implement it for real: Lin source, runtime intrinsics if needed, full tests, performance, ASan.

## Read first
1. Your proposal: `docs/proposals/stdlib/<your-module>.md` — this is the spec. Follow it, but
   you may deviate where implementation reality demands; note deviations in your final report.
2. `docs/STDLIB.md` — house style for the doc you must update.
3. An existing module of the same shape, as a template:
   - Pure-Lin module → `stdlib/array.lin` + `stdlib/array.test.lin`, or `stdlib/path.lin`.
   - Intrinsic-backed module → `stdlib/jq.lin` (the canonical thin-wrapper-over-intrinsic) +
     `crates/lin-runtime/src/jq.rs` (the `#[no_mangle] extern "C"` side).

## How a module is wired (ALL sites — miss one and it won't compile/import)
A new module `std/<mod>`:
- `stdlib/<mod>.lin`           — the module source (exported `val`s).
- `stdlib/<mod>.test.lin`      — tests (see Testing below). REQUIRED.
- Register the source embed in BOTH:
  - `crates/lin-compile/src/lib.rs`  — add `"std/<mod>" => Some(include_str!("../../../stdlib/<mod>.lin")),`
    in the `stdlib_source` match (near the other `"std/bytes"` line ~591).
  - `crates/lin-lsp/src/main.rs`      — add the SAME line in its module map (~1060) AND add
    `"std/<mod>"` to the module-name list (~3581, the `"std/bytes", "std/net", ...` array).
- An ENRICHMENT (adding funcs to std/array, std/iter, std/fs, std/time) edits the EXISTING
  `stdlib/<mod>.lin` and its `.test.lin`; no new registration needed.

## How an intrinsic is wired (only if your proposal needs Rust)
Follow the jq pattern exactly:
- New file `crates/lin-runtime/src/<mod>.rs` with `#[no_mangle] pub unsafe extern "C" fn lin_<mod>_<op>(...) -> *mut u8` (or scalar returns). Study `jq.rs`, `http.rs`, `time.rs` for the
  pointer/box ABI (LinValue tagged boxes, string in/out, UInt8[] buffers). `crates/lin-runtime/src/tagged.rs` + `memory.rs` are the box/RC primitives.
- Add `pub mod <mod>;` to `crates/lin-runtime/src/lib.rs`.
- Declare it in your `.lin` file:
  ```
  import foreign "lin-runtime"
    val lin_<mod>_<op>: (ArgTy, ...) => RetTy
  ```
  There is NO separate allowlist; `import foreign "lin-runtime"` in a trusted stdlib module is
  the gate (ADR-086). User code calling `lin_*` is a hard error — that's expected.
- Add any new Rust crate deps to `crates/lin-runtime/Cargo.toml` (proposal names them).

## Lin language conventions (get these right — they're the usual failure points)
- Value-error: fallible funcs return `T | Error`, Error = `{ "type":"error","message":String }`,
  detected `is Error` (the `is Error` arm comes FIRST in a match). Non-faults return `Null`/`[]`.
- Dot-application: `x.f(y) == f(x, y)`. Strings are codepoint-aware; use `byteAt` (O(1)) for
  byte scanning, NOT `codePointAt` in a loop (O(n²) — a real, documented trap).
- Generics are MONOMORPHIZED and ARGUMENT-DRIVEN: no turbofish; a type param that appears only in
  the return or only as zero-arg never infers — it needs a witness argument or an annotated
  binding. A generic record alias does NOT propagate its T into a generic consumer. Empty `[]`
  accumulators must be annotated (`val xs: Int32[] = []`). See the proposal/limits if you hit this.
- Opaque runtime handles (like `Timer`, `Stream<T>`) are the pattern for stateful intrinsic objects.

## Build & test loop (DO THIS — do not self-report green without running it)
```
cargo build -p lin-runtime            # MUST rebuild runtime if you touched Rust (lin build links the prebuilt .a)
cargo build -p lin --quiet            # the compiler/CLI
cargo run -p lin --quiet -- test stdlib/<mod>.test.lin     # your unit tests
cargo run -p lin --quiet -- fmt --check stdlib/            # CI runs this — formatting must pass
cargo test --workspace                # run the WHOLE Rust suite; you must not regress it
```
ASan verification (CI gates this — replicate it for your test):
```
cargo build -p lin-runtime
RUSTFLAGS="-Zsanitizer=address" cargo +nightly build -p lin-runtime --target x86_64-unknown-linux-gnu
RT="$(find target/x86_64-unknown-linux-gnu/debug -name liblin_runtime.a | head -1)"
LIN_EMIT_IR=1 LIN_NO_OPT=1 cargo run -p lin --quiet -- build stdlib/<mod>.test.lin -o /tmp/asan_<mod>
clang-22 -fsanitize=address -g /tmp/asan_<mod>.ll "$RT" -o /tmp/asan_<mod>_bin
ASAN_OPTIONS=detect_leaks=0 /tmp/asan_<mod>_bin    # must exit 0, no ASan report
```
NOTE (from project memory): ASan-green ≠ correct for representation work — also verify outputs are
actually right (a wrong-tag bug can be ASan-invisible). RC/ownership bugs (UAF/double-free) are the
recurring bug class for intrinsics that return boxed values — be careful with retain/release on the
out-path. `cargo test` alone will NOT catch these; the ASan leg will.

## Testing bar (this is what "well tested" means here)
- Cover every exported function: happy path, empty/zero/boundary inputs, negative indices where
  relevant, the Error arm for every fallible func, Unicode/multibyte for string-touching funcs.
- Test the dot-application form AND the `f(x, ...)` form at least once.
- Use `std/test`: `suite("std/<mod>", [ test("...", () => [ expect(actual).toBe(expected) ]) ])`,
  ending with `run([s])`. Mirror `stdlib/array.test.lin`.

## Performance bar
- No accidental O(n²) (the codePointAt-in-a-loop trap). Prefer the inlined/flat-array fast paths.
- If your proposal claims a perf-sensitive path, add a quick timing sanity check (std/time) in a
  scratch program (don't commit micro-benchmarks unless the proposal calls for it). Judge by
  mechanism + median, not by a single noisy min.

## If you hit a COMPILER BUG (not your own code being wrong)
Lin compiler bugs are expected when exercising new stdlib shapes. The project rule:
1. Confirm it's genuinely a compiler/runtime bug (minimal repro: a few lines of valid Lin that
   miscompiles/crashes), not a misuse of generics/conventions above.
2. You have STANDING PERMISSION to fix compiler bugs and merge them to master. Create a SEPARATE
   git worktree for the fix (do NOT mix it into your module branch):
   `git worktree add ../lin-fix-<short-bug-name> master`
   Write a regression test (a Rust `#[test]` in `crates/lin/tests/integration.rs` or a stdlib
   `.test.lin` as appropriate), fix it, run `cargo build --workspace && cargo test --workspace`
   yourself (the WHOLE suite — a stale `lin` binary causes mass spurious failures), then commit and
   merge that fix branch to master. THEN rebase your module worktree onto updated master and continue.
3. Report every compiler bug you found + fixed in your final summary (one line each + commit SHA).
If a bug is too deep to fix safely, STOP, document it precisely (minimal repro + root-cause
hypothesis), implement as much of the module as works around it, and flag it loudly in your report.

## Final state & report
- DO NOT merge your MODULE branch to master — leave it on its worktree branch for review.
  (Compiler-BUG-fix branches ARE merged, per above.)
- Update `docs/STDLIB.md`: add your module to the Index table, the Functions-by-module table, and
  a full `## std/<mod>` section (or the new functions for an enrichment). Match existing format.
- Commit your module work on the worktree branch with a clear message.
- Final report (returned to the orchestrator) MUST state: branch name; files added/changed; which
  funcs implemented; test count + that `lin test` passed (paste the pass/fail line); that
  `cargo test --workspace` passed; that ASan was clean (or not, with detail); any compiler bugs
  found+fixed+merged (SHAs); any deviations from the proposal; anything left unimplemented and why.
