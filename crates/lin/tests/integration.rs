// Compiler integration tests.
// Each test compiles a Lin snippet to a native binary and runs it.
// Requires `cargo build -p lin` to have been run first.
//
// Run with: cargo test -p lin

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;

static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap()
        .parent().unwrap()
        .to_path_buf()
}

fn lin_bin() -> PathBuf {
    workspace_root().join("target/debug/lin")
}

/// A debug-STRIPPED copy of `liblin_runtime.a`, built once per test process.
///
/// Every integration test compiles a tiny program with `lin build`, and each such build is
/// dominated not by codegen but by the linker pulling the runtime static archive: in dev builds
/// that archive carries ~250MB of DWARF across ~1000 codegen-unit objects, so the per-test link is
/// hundreds of ms of archive I/O regardless of how small the test program is. Linking against a
/// `strip --strip-debug`'d copy (~95MB) roughly halves total suite wall-clock.
///
/// We strip a COPY and point `lin build` at it via `LIN_RUNTIME_LIB` (see `find_runtime_lib`),
/// deliberately leaving the canonical `target/debug/liblin_runtime.a` untouched — local
/// ASan/UAF-hunting links against that and relies on its runtime symbolization. The string
/// comparisons these tests make never need a symbolized backtrace, so stripping costs them nothing.
///
/// Best-effort: if the canonical archive or the `strip` tool is missing, returns `None` and tests
/// fall back to the full-debug archive (correct, just slower). Built once via `OnceLock`.
fn stripped_runtime_lib() -> Option<&'static Path> {
    static STRIPPED: OnceLock<Option<PathBuf>> = OnceLock::new();
    STRIPPED
        .get_or_init(|| {
            let canonical = workspace_root().join("target/debug/liblin_runtime.a");
            if !canonical.exists() {
                return None;
            }
            let stripped = workspace_root().join("target/debug/liblin_runtime.stripped.a");

            // Rebuild the stripped copy if missing or older than the canonical archive (so a
            // freshly rebuilt runtime is always reflected). Both checks are best-effort.
            let needs_rebuild = match (fs::metadata(&stripped), fs::metadata(&canonical)) {
                (Ok(s), Ok(c)) => match (s.modified(), c.modified()) {
                    (Ok(sm), Ok(cm)) => sm < cm,
                    _ => true,
                },
                _ => true,
            };

            if needs_rebuild {
                // Build into a process-unique temp, then atomically rename into place. This keeps
                // the rebuild safe when more than one test process runs at once (e.g. `cargo test
                // --workspace` alongside another invocation): each writes its own temp and the
                // rename is atomic, so no reader ever observes a half-written archive.
                let pid = std::process::id();
                let tmp = workspace_root()
                    .join(format!("target/debug/.liblin_runtime.stripped.{}.a", pid));
                if fs::copy(&canonical, &tmp).is_err() {
                    return None;
                }
                // GNU strip (Linux) takes --strip-debug; Apple's strip (macOS) spells it -S. Pick
                // via host cfg — the test process runs on its own host.
                let strip_flag = if cfg!(target_os = "macos") { "-S" } else { "--strip-debug" };
                let ok = Command::new("strip")
                    .arg(strip_flag)
                    .arg(&tmp)
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                if !ok {
                    // strip unavailable/failed: drop the temp and use the canonical archive.
                    let _ = fs::remove_file(&tmp);
                    return None;
                }
                if fs::rename(&tmp, &stripped).is_err() {
                    let _ = fs::remove_file(&tmp);
                    return None;
                }
            }
            Some(stripped)
        })
        .as_deref()
}

/// A `lin` Command pre-armed with the `LIN_ALLOW_INTRINSICS` escape hatch (ADR-060) so the
/// compiler's own intrinsic-exercising fixtures (which write user-level `.lin` sources that call
/// `lin_*` directly) keep type-checking. Tests that must exercise the gate REJECTING an intrinsic
/// build a bare `Command::new(lin_bin())` instead, WITHOUT this env var.
fn lin_cmd() -> Command {
    let mut cmd = Command::new(lin_bin());
    cmd.env("LIN_ALLOW_INTRINSICS", "1");
    // Link against the debug-stripped runtime copy (see `stripped_runtime_lib`) to cut per-test
    // link time. Best-effort: absent → `lin build` falls back to the canonical archive.
    if let Some(rt) = stripped_runtime_lib() {
        cmd.env("LIN_RUNTIME_LIB", rt);
    }
    cmd
}

/// Compile `source` to a temp binary and return stdout lines.
/// Panics if compilation or execution fails.
fn run(source: &str) -> Vec<String> {
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let src_path = ws.join(format!("target/lin_test_{}.lin", id));
    let bin_path = ws.join(format!("target/lin_test_{}", id));

    fs::write(&src_path, source).unwrap();

    let compile = lin_cmd()
        .args(["build", src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");

    let _ = fs::remove_file(&src_path);

    assert!(
        compile.status.success(),
        "compilation failed:\nstderr: {}\nstdout: {}\nsource:\n{}",
        String::from_utf8_lossy(&compile.stderr),
        String::from_utf8_lossy(&compile.stdout),
        source
    );

    let run_out = Command::new(&bin_path)
        .output()
        .expect("failed to run compiled binary");

    let _ = fs::remove_file(&bin_path);

    assert!(
        run_out.status.success(),
        "runtime error:\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&run_out.stderr),
        String::from_utf8_lossy(&run_out.stdout),
    );

    let stdout = String::from_utf8_lossy(&run_out.stdout);
    stdout
        .lines()
        .map(|l| l.to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Compile and run, expect either compilation or runtime failure.
/// Returns the combined stderr + stdout for assertion.
fn run_expect_err(source: &str) -> String {
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let src_path = ws.join(format!("target/lin_test_err_{}.lin", id));
    let bin_path = ws.join(format!("target/lin_test_err_{}", id));

    fs::write(&src_path, source).unwrap();

    let compile = lin_cmd()
        .args(["build", src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary");

    let _ = fs::remove_file(&src_path);

    if !compile.status.success() {
        let _ = fs::remove_file(&bin_path);
        return format!(
            "{}{}",
            String::from_utf8_lossy(&compile.stderr),
            String::from_utf8_lossy(&compile.stdout)
        );
    }

    let run_out = Command::new(&bin_path)
        .output()
        .expect("failed to run compiled binary");

    let _ = fs::remove_file(&bin_path);

    assert!(
        !run_out.status.success(),
        "expected error but program succeeded\nstdout: {}",
        String::from_utf8_lossy(&run_out.stdout)
    );

    format!(
        "{}{}",
        String::from_utf8_lossy(&run_out.stderr),
        String::from_utf8_lossy(&run_out.stdout)
    )
}

/// Compile source to a binary, pipe stdin_data to it, return trimmed stdout.
fn run_with_stdin(source: &str, stdin_data: &str) -> String {
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let src_path = ws.join(format!("target/lin_test_stdin_{}.lin", id));
    let bin_path = ws.join(format!("target/lin_test_stdin_{}", id));

    fs::write(&src_path, source).unwrap();

    let compile = lin_cmd()
        .args(["build", src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary");

    let _ = fs::remove_file(&src_path);

    assert!(
        compile.status.success(),
        "compilation failed:\nstderr: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let mut child = Command::new(&bin_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    child.stdin.as_mut().unwrap().write_all(stdin_data.as_bytes()).unwrap();
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    let _ = fs::remove_file(&bin_path);

    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Compile source to a binary, run it with `prog_args` appended after argv[0],
/// and return its trimmed stdout. Panics if compilation or execution fails.
fn run_with_args(source: &str, prog_args: &[&str]) -> String {
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let src_path = ws.join(format!("target/lin_test_args_{}.lin", id));
    let bin_path = ws.join(format!("target/lin_test_args_{}", id));

    fs::write(&src_path, source).unwrap();

    let compile = lin_cmd()
        .args(["build", src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");

    let _ = fs::remove_file(&src_path);

    assert!(
        compile.status.success(),
        "compilation failed:\nstderr: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run_out = Command::new(&bin_path)
        .args(prog_args)
        .output()
        .expect("failed to run compiled binary");

    let _ = fs::remove_file(&bin_path);

    assert!(
        run_out.status.success(),
        "runtime error:\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&run_out.stderr),
        String::from_utf8_lossy(&run_out.stdout),
    );

    String::from_utf8_lossy(&run_out.stdout).trim().to_string()
}

// ─────────────────────────────────────────────────────────────────────────────
// Core language tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_arithmetic() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val x = 1 + 2 * 3
print(toString(x))
val y = 10 / 3
print(toString(y))
val m = 10 % 3
print(toString(m))
"#);
    assert_eq!(output, vec!["7", "3", "1"]);
}

#[test]
fn test_string_interpolation() {
    let output = run(r#"import { print } from "std/io"

val name = "Bob"
val age = 42
print("Hello ${name}, age ${age}")
"#);
    assert_eq!(output, vec!["Hello Bob, age 42"]);
}

#[test]
fn test_functions_and_partial_application() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val add = (a: Int32, b: Int32): Int32 => a + b
val addTen = add(10,)
print(toString(addTen(5)))
print(toString(add(3, 4)))
"#);
    assert_eq!(output, vec!["15", "7"]);
}

#[test]
fn test_dot_application() {
    let output = run(r#"import { print } from "std/io"

val greet = (name: String): String => "Hello ${name}"
print("world".greet())
"#);
    assert_eq!(output, vec!["Hello world"]);
}

#[test]
fn test_objects_and_safe_access() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val person = { "name": "Bob", "age": 42 }
print(person["name"])
print(toString(person["missing"]))
print(toString(person["a"]["b"]["c"]))
"#);
    assert_eq!(output, vec!["Bob", "null", "null"]);
}

#[test]
fn test_equality() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

print(toString(1 == 1))
print(toString("a" == "a"))
print(toString(null == null))
print(toString({ "a": 1, "b": 2 } == { "b": 2, "a": 1 }))
print(toString([1, 2] == [1, 2]))
print(toString([1, 2] == [2, 1]))
"#);
    assert_eq!(output, vec!["true", "true", "true", "true", "true", "false"]);
}

// Arrays whose ELEMENTS are heap values (strings, nested arrays, objects) must compare
// STRUCTURALLY, like the top-level object/array equality above. `lin_array_eq`
// (lin-runtime/src/array.rs) now recurses via `lin_tagged_eq` per element, so two
// distinct-but-equal heap elements (e.g. two "a" strings) compare equal. Scalar-element
// arrays are unaffected (their payloads are inline values, compared by value).
#[test]
fn test_array_equality_with_heap_elements() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

print(toString(["a", "b"] == ["a", "b"]))
print(toString(["a", "b"] == ["a", "c"]))
print(toString([[1, 2], [3]] == [[1, 2], [3]]))
print(toString([[1], [2, 3]] == [[1], [2, 4]]))
print(toString([{ "k": 1 }] == [{ "k": 1 }]))
print(toString([{ "k": 1 }] == [{ "k": 2 }]))
"#);
    assert_eq!(output, vec!["true", "false", "true", "false", "true", "false"]);
}

#[test]
fn test_pattern_matching_is() {
    let output = run(r#"import { print } from "std/io"

val describe = (input: AnyVal): String =>
  match input
    is Null => "null"
    is Int32 => "int"
    is String => "string"
    else => "other"

print(describe(null))
print(describe(42))
print(describe("hi"))
print(describe(true))
"#);
    assert_eq!(output, vec!["null", "int", "string", "other"]);
}

#[test]
fn test_pattern_matching_has() {
    let output = run(r#"import { print } from "std/io"

val describe = (input: AnyVal): String =>
  match input
    has { name, age } when age > 30 => "old: ${name}"
    has { name } => "young: ${name}"
    else => "other"

print(describe({ "name": "Bob", "age": 42 }))
print(describe({ "name": "Alice", "age": 20 }))
print(describe("hello"))
"#);
    assert_eq!(output, vec!["old: Bob", "young: Alice", "other"]);
}

#[test]
fn test_tagged_unions() {
    let output = run(r#"import { print } from "std/io"

val divide = (a: Float64, b: Float64): AnyVal =>
  if b == 0.0 then { "type": "failure", "error": "div by zero" }
  else { "type": "success", "value": a / b }

val msg = match divide(10.0, 2.0)
  has { "type": "success", value } => "ok: ${value}"
  has { "type": "failure", error } => "err: ${error}"

print(msg)

val err = match divide(1.0, 0.0)
  has { "type": "success", value } => "ok: ${value}"
  has { "type": "failure", error } => "err: ${error}"

print(err)
"#);
    assert_eq!(output, vec!["ok: 5.0", "err: div by zero"]);
}

// Regression (calc-example segfault): a typed sealed record built in one function,
// returned through an `AnyVal`-typed function (so it is boxed as TAG_RECORD), then
// coerced to a named union and matched with `has { ... }`. The match's discriminant
// FieldGet used to call `lin_map_get` unconditionally on the unboxed pointer, reading
// the sealed struct as a LinMap → SIGSEGV in `find_slot_string`. The union FieldGet
// must tag-dispatch (TAG_RECORD → lin_record_get_field) like the index path does.
#[test]
fn test_match_has_on_record_laundered_through_anyval() {
    let output = run(r#"import { print } from "std/io"

type Failure = { "type": String, "error": String }
type Success = { "type": String, "value": Int32 }
type R = Success | Failure

val fail = (msg: String): Failure => { "type": "failure", "error": msg }
val produce = (): AnyVal => fail("boom")
val entry = (): R => produce()

val show = (r: R): String =>
  match r
    has { "type": "success", value } => "ok: ${value}"
    has { "type": "failure", error } => "err: ${error}"
    else => "?"

print(show(entry()))
"#);
    assert_eq!(output, vec!["err: boom"]);
}

#[test]
fn test_closures_and_var() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val makeCounter = (start: Int32) =>
  var count = start
  () =>
    count = count + 1
    count

val c = makeCounter(0)
print(toString(c()))
print(toString(c()))
print(toString(c()))
"#);
    assert_eq!(output, vec!["1", "2", "3"]);
}

// Regression: a closure-local `var` (NOT captured by any inner closure) reassigned inside an
// `if` branch and READ AFTER the branch joins must observe the in-branch write. Previously the
// branch's reassignment was dropped: the surrounding block "restored" the slot's pre-block temp
// (it only preserved slots a stmt DEFINED, not ones a `LocalSet` REASSIGNED), and even with the
// mapping kept, a plain SSA temp could not model release-old / per-branch ownership at the join.
// The slot read after the `if` therefore saw its INITIAL value (`length(sts)` was always 0 →
// `[0, 0, 0]` instead of `[2, 2, 2]`). The fix routes such owning vars through a heap cell and
// preserves block reassignments. The captured-outer var (`g`) was unaffected (already a cell).
#[test]
fn test_closure_local_var_reassigned_in_if_read_after_join() {
    let output = run(r#"import { for } from "std/iter"
import { length, push } from "std/array"
import { print } from "std/io"
import { toString } from "std/string"
val run = (): Null =>
  val groups = [[10,11],[20,21],[30,31]]
  var g = 0
  var out: AnyVal = []
  ["a","b","c"].for(id =>
    var sts: AnyVal = []
    if g < length(groups) then
      sts = groups[g]
      g = g + 1
    push(out, length(sts))
  )
  print("${toString(out)}")
run()
"#);
    assert_eq!(output, vec!["[2, 2, 2]"]);
}

// Regression (narrowed variant): when the branch condition becomes false partway through the
// loop, later iterations must read the closure-local var's INITIAL value (the empty `[]`, length
// 0), and the in-branch writes from the earlier iterations must NOT bleed across iterations
// (each invocation re-initialises `sts`). Exercises both the then and else join edges.
#[test]
fn test_closure_local_var_reassigned_in_if_else_edge() {
    let output = run(r#"import { for } from "std/iter"
import { length, push } from "std/array"
import { print } from "std/io"
import { toString } from "std/string"
val run = (): Null =>
  val groups = [[10,11],[20,21],[30,31]]
  var g = 0
  var out: AnyVal = []
  ["a","b","c","d","e"].for(id =>
    var sts: AnyVal = []
    if g < length(groups) then
      sts = groups[g]
      g = g + 1
    push(out, length(sts))
  )
  print("${toString(out)}")
run()
"#);
    assert_eq!(output, vec!["[2, 2, 2, 0, 0]"]);
}

// Regression (scalar variant): a non-owning (Int32) plain-SSA `var` reassigned only in the THEN
// branch and read after the join must merge correctly via the join phi (no heap cell involved).
#[test]
fn test_local_int_var_reassigned_in_if_read_after_join() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val f = (c: Boolean): Int32 =>
  var n = 1
  if c then
    n = 42
  n
print(toString(f(true)))
print(toString(f(false)))
"#);
    assert_eq!(output, vec!["42", "1"]);
}

// Regression: an Array (or any heap value) passed as an argument to an INDIRECT call
// through a closure value must be boxed to AnyVal to match the closure's `AnyVal` parameter,
// exactly as the named/imported call paths do. Previously the indirect-call lowering passed
// the raw `LinArray*` instead of a boxed `TaggedVal*`, so the callee read its tag/payload
// from garbage and mutations through it were silently lost (the array stayed empty).
#[test]
fn test_array_passed_to_closure_value_mutates() {
    let output = run(r#"import { print } from "std/io"
import { push, length } from "std/array"
import { toString } from "std/string"

val acc: Int32[] = []
val f = (a: AnyVal) => push(a, 1)
f(acc)
f(acc)
print(toString(length(acc)))
"#);
    assert_eq!(output, vec!["2"]);
}

// Regression: a fresh-alloc heap literal (array/object) passed to a AnyVal/union parameter,
// where the call RESULT ESCAPES (is returned / outlives the literal), must NOT have its
// backing store released at the caller's scope exit while the escaping result still aliases
// it. The lowerer registers the literal as owned in the caller scope and would release it on
// exit; ownership must instead transfer into the escaping result (the eventual owner releases
// it). Previously the premature scope-release fired, corrupting the array's length header and
// crashing the returned value's later use with `capacity overflow` (a use-after-free).
// Covers the array passthrough (identity `(acc) => acc`) and the accumulator-threading idiom
// (recursive `build(i, n, acc)` returning the threaded `acc`).
#[test]
fn test_fresh_heap_arg_to_json_param_escapes_no_uaf() {
    // Array passthrough: `id([1, 2])` returned out of `wrap`.
    let passthrough = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val id = (acc: AnyVal): AnyVal => acc
val wrap = (): AnyVal => id([1, 2])
print(toString(wrap()))
"#);
    assert_eq!(passthrough, vec!["[1, 2]"]);

    // Accumulator-threading: `build(0, n, [])` returns the threaded `acc`.
    let accumulator = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { push } from "std/array"

val build = (i: Int32, n: Int32, acc: AnyVal): AnyVal =>
  if i >= n then acc
  else
    push(acc, i * i)
    build(i + 1, n, acc)
val squares = (n: Int32): AnyVal => build(0, n, [])
print(toString(squares(4)))
"#);
    assert_eq!(accumulator, vec!["[0, 1, 4, 9]"]);

    // Result BOUND to a `val` and then returned (block-scope escape, not just direct return) —
    // the literal is owned in the block scope, so the block's own scope-release must also
    // transfer ownership into the escaping result, not just the function-return release.
    let bound = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val id = (acc: AnyVal): AnyVal => acc
val wrap = (): AnyVal =>
  val x = id([1, 2])
  x
print(toString(wrap()))
"#);
    assert_eq!(bound, vec!["[1, 2]"]);

    // INDIRECT (closure-value) call: the literal escapes through a call whose callee is a
    // closure value (`f`), not a statically-known function.
    let indirect = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val makeId = () => (acc: AnyVal): AnyVal => acc
val wrap = (): AnyVal =>
  val f = makeId()
  f([1, 2])
print(toString(wrap()))
"#);
    assert_eq!(indirect, vec!["[1, 2]"]);

    // Fresh object literal carrying a nested array, passed through and returned — the nested
    // payload must survive too (a shallow box-aliasing guard would free the inner array early).
    let nested = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val id = (acc: AnyVal): AnyVal => acc
val wrap = (): AnyVal => id({ "items": [1, 2, 3] })
print(toString(wrap()))
"#);
    assert_eq!(nested, vec![r#"{"items": [1, 2, 3]}"#]);

    // TRANSIENT result (consumed, not escaped) must still be released normally — guards against
    // the keep-expansion over-suppressing the literal release and leaking.
    let transient = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"

val id = (acc: AnyVal): AnyVal => acc
val use = (): Int32 =>
  val x = id([1, 2])
  length(x)
print(toString(use()))
"#);
    assert_eq!(transient, vec!["2"]);
}

#[test]
fn test_recursion() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val factorial = (n: Int32): Int32 =>
  if n == 0 then 1 else n * factorial(n - 1)

print(toString(factorial(5)))
print(toString(factorial(0)))
"#);
    assert_eq!(output, vec!["120", "1"]);
}

// ADR-082: a LOCAL (nested-in-a-function-body) recursive function-`val` whose RETURN type is a
// union must be forward-declared so its body can call itself — exactly like the non-union case.
// Before the fix, a local function whose enclosing function declared a Union/Named/Object return
// went through the expected-type-directed block-check path (`check_expr` block-push), which —
// unlike `infer_block` — never hoisted inner function-vals, so a recursive self-call reported
// "Undefined variable". This also exercises two codegen fixes the type-check fix newly reached:
//   - the TCO arg→slot store offset for a self-tail-recursive CLOSURE (the implicit env param at
//     slot 0 must be skipped, else the counter is stored into the env slot → infinite loop);
//   - the NullableRecord indirect-call ABI bridge (a closure returns a BOXED union; the call result
//     must be Coerced boxed → NullableRecord so the consumer's `is T` narrowing sees a real record).
#[test]
fn test_local_recursive_fn_with_union_return() {
    // Annotated `T | Null` return, recursing to a base that yields a record; `is T` after must
    // narrow correctly (would print "none" if the closure-return repr were mishandled).
    let output = run(r#"import { print } from "std/io"
type Trip = { "id": Int32 }
val getTrip = (target: Int32): Trip | Null =>
  val scan = (i: Int32, lastFound: Trip | Null): Trip | Null =>
    if i < 0 then lastFound
    else if i == target then scan(i - 1, { "id": i })
    else scan(i - 1, lastFound)
  scan(20, null)
val r = getTrip(5)
if r is Trip then print("found") else print("none")
"#);
    assert_eq!(output, vec!["found"]);
}

#[test]
fn test_local_recursive_fn_with_inferred_union_return() {
    // INFERRED union return (no annotation on `go`): the inner function-val must still be
    // forward-declared so `go` is in scope in its own body.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val outer = (): Int32 | Null =>
  val go = (n: Int32) => if n < 0 then null else if n == 0 then 42 else go(n - 1)
  go(5)
val r = outer()
if r is Int32 then print(toString(r)) else print("none")
"#);
    assert_eq!(output, vec!["42"]);
}

#[test]
fn test_local_recursive_union_fn_deep_tco() {
    // A self-tail-recursive local function (a CLOSURE) returning a union must tail-call-optimize:
    // 5,000,000 iterations must terminate without a stack overflow AND produce the right answer.
    // Regression for the TCO arg→slot store landing in the env slot (infinite loop) instead of the
    // counter slot.
    let output = run(r#"import { print } from "std/io"
type Trip = { "id": Int32 }
val getTrip = (target: Int32): Trip | Null =>
  val scan = (i: Int32, lastFound: Trip | Null): Trip | Null =>
    if i < 0 then lastFound
    else if i == target then scan(i - 1, { "id": i })
    else scan(i - 1, lastFound)
  scan(5000000, null)
val r = getTrip(5)
if r is Trip then print("found") else print("none")
"#);
    assert_eq!(output, vec!["found"]);
}

// Regression: a locally-defined TCO function that also captures outer vars/params must not
// capture ITS OWN forward-declared slot. Previously the checker added the function's own binding
// slot to its captures set (var_scope_depth < fn_entry_depth for the body's block scope), causing
// the closure env to contain a phantom 6th capture with no corresponding value — the capdesc said
// 6 entries but the env only stored 5, causing an OOB read on the 6th capture slot. The fix
// suppresses self-capture by excluding the current function's own forward-declared slot.
#[test]
fn test_local_tco_fn_with_captured_var_does_not_self_capture() {
    // searchDay-style: inner TCO function that captures outer vars + recurses on itself.
    // If self-capture were happening, the closure descriptor would have one more entry than
    // the actual env, causing a segfault / misaligned-pointer panic on the phantom slot.
    let output = run(r#"import { print } from "std/io"
import { push } from "std/array"
import { toString } from "std/string"
val run = (maxI: Int32): Int32 =>
  var acc = 0
  val step = (i: Int32): Int32 =>
    if i >= maxI then acc
    else
      acc = acc + i
      step(i + 1)
  step(0)
print(toString(run(5)))
"#);
    assert_eq!(output, vec!["10"]);

    // Also test with multiple captured values to exercise the specific capdesc/env size mismatch.
    let output2 = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val outer = (base: Int32, limit: Int32): Int32 =>
  var total = base
  val iter = (i: Int32): Int32 =>
    if i >= limit then total
    else
      total = total + i
      iter(i + 1)
  iter(0)
print(toString(outer(10, 5)))
"#);
    assert_eq!(output2, vec!["20"]);
}

// ADR-082: the type-check fix also enables MUTUAL local recursion (two inner function-vals calling
// each other) with union returns to TYPE-CHECK. (Runtime mutual local recursion is gated by a
// separate, pre-existing closure-env construction-order limitation that also affects non-union
// mutual local recursion — see ADR-082; this test asserts only the type-check.)
#[test]
fn test_mutual_local_recursive_union_fn_typechecks() {
    let (ok, out) = check_source(r#"type T = { "x": Int32 }
val outer = (n: Int32): T | Null =>
  val isEven = (k: Int32): T | Null => if k == 0 then { "x": 1 } else isOdd(k - 1)
  val isOdd = (k: Int32): T | Null => if k == 0 then null else isEven(k - 1)
  isEven(n)
val r = outer(10)
"#);
    assert!(ok, "mutual local recursion with union returns should type-check:\n{}", out);
}

// ADR-082 follow-up: a self-tail-recursive function with a UNION return that passes a FRESHLY-
// CREATED record as a NullableRecord accumulator arg. The fresh `{ "x": n }` literal is typed
// UNSEALED by the checker; the param slot's repr is NullableRecord (a raw packed-struct pointer).
// Before the fix the unresolved-`Named` union param (`T | Null` on a TOP-LEVEL directly-called fn)
// caused the Coerce to box the object as TAG_MAP, while the slot's RC release used `lin_sealed_release`
// — a representation mismatch that freed the box once too often: garbage at shallow depth (`got 33`)
// and a SEGFAULT deep. Assert the exact FIELD VALUE (not just `is T`, which a corrupt box can still
// satisfy) AND survival of a deep TCO run (no stack overflow / no UAF / no per-iteration leak).
#[test]
fn test_tco_fresh_record_nullable_union_arg_value_and_deep() {
    // Shallow: the last accumulator built is `{ "x": 1 }` at n == 1 — the field must read back as 1.
    let shallow = run(r#"import { print } from "std/io"
type T = { "x": Int32 }
val go = (n: Int32, acc: T | Null): T | Null =>
  if n <= 0 then acc else go(n - 1, { "x": n })
val r = go(10, null)
print(if r == null then "null" else "got ${ r["x"] }")
"#);
    assert_eq!(shallow, vec!["got 1"]);

    // Deep: 3,000,000 iterations must TCO (no stack overflow) and still produce the right value.
    let deep = run(r#"import { print } from "std/io"
type T = { "x": Int32 }
val go = (n: Int32, acc: T | Null): T | Null =>
  if n <= 0 then acc else go(n - 1, { "x": n })
val r = go(3000000, null)
print(if r == null then "null" else "got ${ r["x"] }")
"#);
    assert_eq!(deep, vec!["got 1"]);
}

#[test]
fn test_for_and_range() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range } from "std/iter"
import { for } from "std/iter"

range(1, 4).for(i => print(toString(i)))
"#);
    assert_eq!(output, vec!["1", "2", "3"]);
}

// Regression for the fused `range(a, b).for(f)` lowering (perf/foreach-closure): the receiver is a
// literal `range(...)` call, so `for` lowers to a counted i32 loop driving the callback directly —
// no materialized range array. This MUST stay observably identical to iterating the array:
//   1. captured-`var` mutation accumulates into the SAME heap cell (sum 0..1000 = 499500), with a
//      bound large enough to exceed the small-int box cache (so the boxed-element path is exercised);
//   2. an `arr.for(...)` over a NON-range array still iterates every element (fusion must not
//      misfire on a non-range receiver);
//   3. a `range(...)` bound to a `val` first (so the `.for` receiver is a LocalGet, not a literal
//      range call) takes the generic array path and still produces the right sum.
#[test]
fn test_range_for_fusion_semantics() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range, for } from "std/iter"

// 1. fused range-for with captured-var accumulation, past the small-int box cache.
var total = 0i64
range(0, 1000).for(i => total = total + i)
print(toString(total))

// 2. non-range array .for must still iterate every element.
var seen = 0
[10, 20, 30].for(x => seen = seen + x)
print(toString(seen))

// 3. range bound to a val first → generic path, same result.
val r = range(0, 1000)
var total2 = 0i64
r.for(i => total2 = total2 + i)
print(toString(total2))
"#);
    assert_eq!(output, vec!["499500", "60", "499500"]);
}

// Regression: elements of a `split()` (and `lines()`) result must iterate correctly under the
// generic `for`/`map` path. `lin_string_split` previously pushed each element with tag 0
// (TAG_NULL) instead of TAG_STR, so generic iteration read every element as `null` (index access
// happened to work because codegen knew the static String[] element type). The runtime now tags
// split elements TAG_STR, so `.for`/`.map` see the real strings.
#[test]
fn test_split_result_iterates_as_strings() {
    let output = run(r#"import { print } from "std/io"
import { split } from "std/string"
import { for, map } from "std/iter"

val parts = split("alpha,beta,gamma", ",")
parts.for(s => print(s))
val wrapped = parts.map(s => "<${s}>")
wrapped.for(s => print(s))
"#);
    assert_eq!(
        output,
        vec!["alpha", "beta", "gamma", "<alpha>", "<beta>", "<gamma>"]
    );
}

// Regression: a top-level mutable `var` accumulated from inside a `.for` loop body closure.
// The closure can't see main's SSA temps, so the var must be a module global written via
// GlobalValSet and read via GlobalValGet; and `acc + i` must unbox the boxed (TypeVar) loop
// element before the integer add. Previously this crashed in codegen (int op on a null ptr).
#[test]
fn test_loop_accumulates_toplevel_var() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range, for } from "std/iter"

var total = 0
range(0, 5).for(i => total = total + i)
print(toString(total))
"#);
    assert_eq!(output, vec!["10"]);
}

// Regression: nested loops where the outer `.for` body mutates a top-level var by calling a
// helper that itself runs a `.for` over an inner mutable var.
#[test]
fn test_nested_loops_with_var_accumulators() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range, for } from "std/iter"

val work = (n: Int32): Int32 =>
  var s = 0
  range(0, n).for(i => s = s + i)
  s

var total = 0
range(0, 5).for(i => total = total + work(10))
print(toString(total))
"#);
    // work(10) = 0+1+..+9 = 45; summed 5 times = 225.
    assert_eq!(output, vec!["225"]);
}

// Sealed-record KIND_MAP heap field (ADR-063): a NAMED record whose fields include a typed
// index-signature map `{ String: T }` now PACKS into a sealed struct — the Map lives inline as an
// owned `*LinMap` pointer slot (descriptor kind KIND_MAP=4 / NKIND_MAP=9), exactly like a String/
// Array heap field. This is the RAPTOR `Service { days, dates }` shape. The struct must construct
// (retain the map +1), read a dynamic map key off the packed field, and drop (release the map via
// the descriptor walk) with correct values. A standalone Service, a packed `Service[]` element, and
// a nested `Outer { s: Service }` all exercise the inline-map slot at construct/read/drop.
// (Storing a packed sealed record AS a `{String:Service}` map VALUE is a separate pre-existing
// keep-packed-into-map-value bug that crashes for scalar records too — not covered here.)
#[test]
fn test_sealed_record_with_map_field_kind_map() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { push, length } from "std/array"

type Service = { "startDate": Int32, "endDate": Int32, "days": { String: Boolean }, "dates": { String: Boolean } }
type Outer = { "s": Service }

// Standalone packed Service with two map fields; dynamic-key reads off the packed slots.
var days: { String: Boolean } = {}
days["mon"] = true
days["tue"] = false
var dates: { String: Boolean } = {}
dates["2026-01-01"] = true
val svc: Service = { "startDate": 20260101, "endDate": 20261231, "days": days, "dates": dates }
print(toString(svc["startDate"]))
print(toString(svc["days"]["mon"]))
print(toString(svc["days"]["tue"]))
print(toString(svc["dates"]["2026-01-01"]))

// Packed Service[] of map-field records; field read off contiguous elements.
var arr: Service[] = []
var d0: { String: Boolean } = {}
d0["x"] = true
push(arr, { "startDate": 1, "endDate": 2, "days": d0, "dates": d0 })
var d1: { String: Boolean } = {}
d1["x"] = false
push(arr, { "startDate": 3, "endDate": 4, "days": d1, "dates": d1 })
print(toString(length(arr)))
print(toString(arr[0]["days"]["x"]))
print(toString(arr[1]["days"]["x"]))

// Nested: a map field inside a nested sealed record.
val o: Outer = { "s": svc }
print(toString(o["s"]["days"]["mon"]))
"#);
    assert_eq!(
        output,
        vec![
            "20260101", "true", "false", "true", // standalone Service reads
            "2", "true", "false",                // Service[] length + element map reads
            "true",                              // nested Outer.s.days["mon"]
        ]
    );
}

// Regression (object/record holding a typed-map FIELD is fully released): `lin_object_release`'s
// value-release loop was a hand-rolled copy that omitted TAG_MAP — so a `{ String: T }` map stored
// as a record/object FIELD (e.g. `ScanResults.bestArrivals`) was never released when the record
// dropped, leaking the whole map + nested contents on every discard. The release loop now routes
// through the canonical `release_tagged_payload` (which handles every tag). The record build/read
// must still produce correct values; an over-eager release would be a use-after-free.
#[test]
fn test_record_with_map_field_released_and_correct() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { for, range } from "std/iter"
import { keys } from "std/object"
import { length } from "std/array"

type Rec = { "m": { String: Int64 }, "n": { String: { String: Int64 } } }
val make = (): Rec =>
  var m: { String: Int64 } = {}
  var n: { String: { String: Int64 } } = {}
  range(0, 20).for(i =>
    m["S${i}"] = 5i64
    n["S${i}"] = {}
    n["S${i}"]["0"] = 7i64
  )
  { "m": m, "n": n }

var total = 0
range(0, 50).for(i =>
  val r = make()
  total = total + length(keys(r["m"]))
)
print(toString(total))
"#);
    // 20 keys × 50 iterations = 1000.
    assert_eq!(output, vec!["1000"]);
}

// Regression (dynamic-arith union result released): a `Binary` op whose RESULT type is a union
// (`AnyVal`) — the dynamic `lin_tagged_arith` path, or bitwise-on-union — produces a FRESHLY boxed
// `TaggedVal*` (+1). The lowerer now `register_owned`s it so scope exit (or the move/escape
// machinery) reclaims it; previously its consumers (a cell store, a return) each `CloneBox`'d a
// fresh +1 and the original arith-result box was orphaned (the residual after the operand-box
// leak-#4b fix — `acc = acc + x` with a `AnyVal` `acc` leaked one box/iteration). The accumulator
// must still compute correctly; an over-eager free would corrupt or crash it.
#[test]
fn test_dynamic_arith_union_result_released_and_correct() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range, for } from "std/iter"

val sumDyn = (): AnyVal =>
  var acc: AnyVal = 0
  range(0, 50).for(stop => acc = acc + stop)
  acc

var total: AnyVal = 0
range(0, 20).for(i => total = total + sumDyn())
print(toString(total))
"#);
    // sumDyn() = 0+1+..+49 = 1225; summed 20 times = 24500.
    assert_eq!(output, vec!["24500"]);
}

// Regression (captured-cell free): `map` uses a `var i` cell captured by its inner `.for`
// closure. The cell + its value were leaked on every `map` call (a per-call ~31 B leak; in a
// hot loop, unbounded RSS growth). The lowerer now frees provably-non-escaping captured cells
// at the creating function's scope exit (the closure is a synchronous, non-retained combinator
// callback argument, so it can't outlive the call). This is the discarded-map-in-loop leak
// case: it must still produce the CORRECT count, and a wrong (over-eager) free would be a
// use-after-free crashing or corrupting `map`'s accumulator.
#[test]
fn test_map_in_loop_discarded_cell_free() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range, for, map } from "std/iter"

val outer = range(0, 5000)
var c = 0
outer.for(i =>
  val m = [1, 2, 3].map(x => x + 1)
  c = c + 1
)
print(toString(c))
"#);
    assert_eq!(output, vec!["5000"]);
}

// Regression (Iterator RC): an `Iterator<T>` value is a freshly-materialised heap `LinArray`
// (`range`/`iter`/`iterOf`), but `is_rc_type`/`ty_is_concrete_rc` used to OMIT `Type::Iterator`,
// so the lowerer never registered it owned and never released it at scope exit — every bound
// `range(...)` / iterator-combinator result leaked its whole array (the RAPTOR LOAD/PREP-phase
// leak; unbounded RSS in a hot loop). Both predicates now include `Iterator` (kept in lockstep:
// an asymmetry would be a double-free). Iterators have no borrowed alias, so this is sound — the
// ASan stdlib/example leg guards the no-double-free half. This test guards that binding + consuming
// an iterator in a loop still computes correctly (an over-eager free would corrupt the sum / crash).
#[test]
fn test_iterator_bound_value_released_and_correct() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range, for } from "std/iter"
import { length } from "std/array"

val build = (): Int32 =>
  val r = range(0, 100)
  var s = 0
  r.for(i => s = s + i)
  s + length(r)

var total = 0
range(0, 50).for(i => total = total + build())
print(toString(total))
"#);
    // build() = (0+..+99) + 100 = 4950 + 100 = 5050; summed 50 times = 252500.
    assert_eq!(output, vec!["252500"]);
}

// Regression (string-interpolation transient-use RC, leak #3): a string interpolation
// `"${expr}"` used TRANSIENTLY — the RAPTOR query-phase hot path `m["${k}"] = v`, where the
// interp string is an index-write KEY — leaked its +1 on every write (~19 B / write; unbounded
// RSS in a scan). Two distinct leaks: (a) the per-part `ToString` result, which `lin_string_concat`
// only BORROWS; (b) the final accumulator, which the container `set` RETAINS internally (so the
// interp string's own +1 was never reclaimed). The fix makes `ToString` UNIFORMLY return an owned
// (+1) string (the codegen Str arm now retains, matching the fresh numeric/json arms), then
// `lower_string_interp` releases each per-part temp after the concat and `register_owned`s the
// final result — so transient uses release at scope exit while moves (val binding / return /
// stored VALUE) transfer the single +1 through the existing escape machinery. A wrong (over-eager)
// free of a key the map still holds would be a use-after-free corrupting the map / crashing; ASan
// (the stdlib+examples leg) guards the no-leak / no-double-free halves. This test guards that
// building interp-keyed maps in a hot loop and reading them back stays CORRECT.
#[test]
fn test_string_interp_key_in_loop_released_and_correct() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range, for } from "std/iter"

val build = (n: Int32): Int32 =>
  var m: AnyVal = {}
  range(0, n).for(k => m["${k}"] = k * 2)
  // Read back through an interp key used as a transient lookup operand too.
  var sum = 0
  range(0, n).for(k =>
    val v = m["${k}"]
    if v is Int32 then sum = sum + v else sum = sum
  )
  sum

var total = 0
range(0, 50).for(i => total = total + build(20))
print(toString(total))
"#);
    // build(20) writes m["0"]..m["19"] = 0,2,..,38; sum = 2*(0+..+19) = 2*190 = 380.
    // Summed 50 times = 19000.
    assert_eq!(output, vec!["19000"]);
}

// Regression (box-shell of a fresh heap value coerced to AnyVal — the dominant RAPTOR query-phase
// leak, "leak B"): binding a FRESHLY-ALLOCATED concrete heap value (`{}`, `[..]`, a String/Array
// call result) to a `val`/`var` of UNION (`AnyVal`) type boxes it via `lin_box_object`/`box_array`/
// `box_str` — a 16-byte `TaggedVal*` shell wrapping the raw inner without bumping its rc. The IR
// owning model registered only the RAW INNER, so scope exit released the inner but ORPHANED the box
// shell → 16 B leaked per binding (unbounded RSS in a hot loop). Fix: `lower_value_into_slot` / the
// plain-`var` arm now route through `coerce_to_slot_type_owning_bind`, which transfers ownership of
// the box into the scope (register the box owned, unregister the raw inner) so scope-exit releases
// the box via `lin_tagged_release` (frees shell AND drops the inner) and the inner's single +1 flows
// into the box — no leak, no double-free. A WRONG fix (releasing both, or failing to unregister the
// inner) would double-free the inner and crash / corrupt; ASan (stdlib+examples leg) guards the
// no-leak / no-double-free halves. This test guards that discarding fresh AnyVal-typed bindings in a
// hot loop — and a returned/moved one — stays CORRECT (an over-eager free would corrupt the result).
#[test]
fn test_fresh_json_binding_box_released_and_correct() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range, for } from "std/iter"
import { keys } from "std/object"
import { length } from "std/array"

// Discarded fresh AnyVal-typed `val`/`var` bindings (the box shell must be reclaimed).
val churn = (): Int32 =>
  val m: AnyVal = {}
  var a: AnyVal = [1, 2, 3]
  val s: AnyVal = "hi"
  0

// A returned fresh AnyVal binding MOVES its +1 box out — must NOT be freed at scope exit.
val makeStored = (): Int32 =>
  val o: AnyVal = {}
  o["k"] = 7
  o["j"] = 11
  length(keys(o))

var total = 0
range(0, 50).for(i =>
  total = total + churn()
  total = total + makeStored()
)
print(toString(total))
"#);
    // churn() = 0; makeStored() = 2 (two keys). 50 * (0 + 2) = 100.
    assert_eq!(output, vec!["100"]);
}

// Regression (L2 — fresh heap literal passed to a AnyVal param, result dropped): a fresh array/object
// literal passed DIRECTLY as an argument to a function whose parameter is `AnyVal` is boxed
// (`lin_box_array`/`box_object`); the caller owns and must reclaim the box shell after the call.
// Two bugs leaked it: (1) when the call result is a SCALAR (`f([1,2,3]): Int32`), codegen's
// `FreeBoxShellIfDistinct` silently skipped the free because the result temp was not a pointer →
// the shell leaked; fixed to free unconditionally when the result can't alias the box. (2) when the
// result is `AnyVal` (`firstOr([1,2,3], d)` / accumulator-threading), a now-obsolete transfer-on-escape
// alias kept the literal alive on the assumption the callee returns its param BY REFERENCE — but
// union returns CLONE (the function-return path takes an independent +1), so the literal's own +1
// leaked every call; fixed by removing the escape-alias. A WRONG fix double-frees the returned value;
// ASan (stdlib+examples leg) + the no-UAF escape test guard that. This guards correctness in a loop.
#[test]
fn test_fresh_literal_json_arg_released_and_correct() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"

// Scalar-result callee (leak #1): the boxed array arg's shell must be reclaimed.
val takesJson = (x: AnyVal): Int32 => 0
// AnyVal-result pass-through (leak #2): the literal flows out via the cloned union return.
val firstOr = (arr: AnyVal, d: AnyVal): AnyVal => if arr == null then d else arr

val build = (): Int32 =>
  val a = takesJson([1, 2, 3])
  val r = firstOr([4, 5, 6], 0)
  a + length(r)

var total = 0
val loop = (i: Int32, n: Int32): Int32 =>
  if i >= n then 0
  else
    total = total + build()
    loop(i + 1, n)
val _ = loop(0, 50)
print(toString(total))
"#);
    // build() = 0 + length([4,5,6]) = 3; summed 50 times = 150.
    assert_eq!(output, vec!["150"]);
}

// Regression (L3 — `map`/`filter` per-element box reclaim): when a combinator is NOT inlined (its
// callback is a closure value, e.g. the compiled stdlib `map`/`filter` wrapper) it reads each source
// element via `lin_array_get_tagged`, which allocates a fresh 16-byte `TaggedVal*` box (and retains
// the inner). The loop body pushed the callback result but never reclaimed that per-element box → the
// shell (always) and, for SKIPPED filter elements, the retained inner leaked every iteration. Fix:
// free the box SHELL after a `map`/`filter` push (the inner was moved/retained into the result), and
// FULLY release a filter-DROPPED element's box. A WRONG fix (full release on the keep/move path)
// double-frees the moved inner (the `filter`-over-`split` UAF). ASan guards the leak/no-double-free;
// this guards that the combinators still compute correctly through the non-inline wrapper.
#[test]
fn test_combinator_elem_box_released_and_correct() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map, filter } from "std/iter"
import { split } from "std/string"
import { length } from "std/array"

// Non-inline filter over a fresh string array (the `filter$String`-over-`split` shape): keeps one,
// drops the rest — the dropped elements' inner strings must be released.
val keep = (only: String, name: String): Int32 =>
  val wanted = split(only, ",").filter(n => n == name)
  length(wanted)

// Non-inline map over a AnyVal array, callback projects each element.
val sizes = (src: AnyVal): Int32 =>
  val m = map(src, x => x)
  length(m)

val src: AnyVal = ["aa", "bb", "cc", "dd"]
var total = 0
val loop = (i: Int32, n: Int32): Int32 =>
  if i >= n then 0
  else
    total = total + keep("a,b,c,d,e", "c")
    total = total + sizes(src)
    loop(i + 1, n)
val _ = loop(0, 50)
print(toString(total))
"#);
    // keep(...) = 1 (one match "c"); sizes(src) = 4. 50 * (1 + 4) = 250.
    assert_eq!(output, vec!["250"]);
}

// Regression (L4 — non-cached scalar boxed into a AnyVal param): passing a SCALAR (a large int or any
// float) to a function whose parameter is `AnyVal` boxes it via `lin_box_int32`/`box_float64` into a
// fresh 16-byte `TaggedVal*` shell the callee borrows and never releases. `arg_box_is_caller_owned_shell`
// only covered HEAP args, so the scalar box shell leaked every call (cached small-int/bool boxes are
// immortal statics, so only NON-cached scalars leak). Fix: a scalar→AnyVal arg box is now reclaimed via
// `FreeBoxShellIfDistinct` (cached-box safe: immortal boxes are never freed; result-alias safe: a
// pass-through callee returning its AnyVal param hands the same box back, skipped when shell == result).
// This guards correctness in a loop over large-int, float, AND cached-small-int args, plus a scalar
// returned as AnyVal (the box must survive as the result).
#[test]
fn test_scalar_json_arg_box_released_and_correct() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val takesJson = (x: AnyVal): Int32 => 0
val identJson = (x: AnyVal): AnyVal => x

val build = (): Int32 =>
  val a = takesJson(1000000)   // large (non-cached) int box shell must be reclaimed
  val b = takesJson(3.14159)   // float box shell must be reclaimed
  val c = takesJson(5)         // cached small-int box is immortal — must NOT be double-freed
  val r = identJson(2000000)   // scalar RETURNED as AnyVal — box survives as the result
  a + b + c + (if r is Int32 then r else 0)

var total = 0
val loop = (i: Int32, n: Int32): Int32 =>
  if i >= n then 0
  else
    total = total + build()
    loop(i + 1, n)
val _ = loop(0, 50)
print(toString(total))
"#);
    // build() = 0 + 0 + 0 + 2000000 = 2000000; summed 50 times = 100000000.
    assert_eq!(output, vec!["100000000"]);
}

// Regression (escape safety): a `var n` cell captured by a closure that is RETURNED from its
// creating function ESCAPES — the closure (and thus the cell) outlives the call. The lowerer
// must NOT free this cell at scope exit; doing so would be a use-after-free when the returned
// closure is later invoked. This counter factory must still increment correctly across calls.
#[test]
fn test_escaping_captured_cell_not_freed() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val mk = () =>
  var n = 0
  () =>
    n = n + 1
    n
val c = mk()
print(toString(c()))
print(toString(c()))
print(toString(c()))
"#);
    assert_eq!(output, vec!["1", "2", "3"]);
}

// Regression (captured-cell free correctness): every combinator whose stdlib body uses a `var`
// cell (map/filter/reduce/find/some/every) must still produce correct results after the cell
// free is applied — a wrong free would corrupt or crash them.
#[test]
fn test_combinators_with_var_cells_correct_after_free() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map, filter, reduce, find, some, every } from "std/iter"

print(toString([1, 2, 3].map(x => x * 2)))
print(toString([1, 2, 3, 4].filter(x => x > 2)))
print(toString([1, 2, 3, 4].reduce(0, (a, b) => a + b)))
print(toString([1, 2, 3, 4].find(x => x > 2)))
print(toString([1, 2, 3].some(x => x > 2)))
print(toString([1, 2, 3].every(x => x > 0)))
"#);
    assert_eq!(output, vec!["[2, 4, 6]", "[3, 4]", "10", "3", "true", "true"]);
}

// Regression (var-cell loop leak): a `var` declared INSIDE an inlined outer loop body (e.g.
// `range(0,N).for(_ => var arr = []; ...)`) allocates a MakeCell in the loop-body block (non-
// entry). The function-exit FreeCell only fires for entry-block cells (dominance), so the cell
// leaked on every outer iteration — O(N) RSS growth. Fix: `inline_lambda_body_tracking_elem_boxes`
// now emits FreeCell for cells created during the inline body before popping the body scope.
// This test asserts: (a) values are correct (no double-free / UAF), and (b) the loop executes
// without visible corruption regardless of REPS count.
#[test]
fn test_var_in_inline_loop_body_cell_freed_per_iteration() {
    let output = run(r#"import { push } from "std/array"
import { range, for } from "std/iter"
import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"
type P = { "x": Int32, "y": Int32 }
// Each outer iteration allocates a fresh arr, fills it with 5 elements, then drops it.
// If the cell isn't freed each iteration, RSS grows with REPS (the leak).
// Assertion: last iteration's arr length = 5 (no corruption from over-release or UAF).
var last_len = 0
range(0, 10).for(_ =>
  var arr: P[] = []
  range(0, 5).for(i => push(arr, { "x": i, "y": i * 2 }))
  last_len = length(arr)
)
print(toString(last_len))
"#);
    assert_eq!(output, vec!["5"]);
}

// Regression (var-cell loop leak, typed record array variant): same as above but uses a typed
// array annotation and reads back a field to confirm no UAF corruption from the cell free.
#[test]
fn test_var_array_in_inline_loop_body_cell_freed_values_correct() {
    let output = run(r#"import { push } from "std/array"
import { range, for } from "std/iter"
import { print } from "std/io"
import { toString } from "std/string"
type Q = { "v": Int32 }
var sum = 0
range(0, 20).for(outer =>
  var arr: Q[] = []
  range(0, 100).for(i => push(arr, { "v": i }))
  var s = 0
  arr.for(q => s = s + q["v"])
  sum = sum + s
)
// Each outer iter: sum(0..99) = 4950; 20 outer * 4950 = 99000.
print(toString(sum))
"#);
    assert_eq!(output, vec!["99000"]);
}

// Regression (escaping-var object-literal field, Bug 1): a `var` captured by closures stored as
// FIELDS of an object literal that ESCAPES (returned / pushed to a collection). The cell must
// outlive the creating scope; the escape analysis marks it escaping and the function-exit FreeCell
// correctly skips it. Covers: (a) makeCounter-style factory that returns an object with `inc` /
// `get` methods sharing a var cell; (b) the same pattern inside a range-for inline loop body
// (where the loop creates N independent counter objects and pushes them into an outer array).
// Verifies values are correct — a UAF (premature FreeCell) would corrupt the read.
#[test]
fn test_var_cell_escaping_via_object_literal_field() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val makeCounter = () =>
  var count = 0
  {
    "inc": () => count = count + 1,
    "get": () => count
  }

val c = makeCounter()
c["inc"]()
c["inc"]()
c["inc"]()
print(toString(c["get"]()))
"#);
    assert_eq!(output, vec!["3"]);
}

// Regression (escaping-var object-literal field in loop, Bug 1 variant): same pattern but the
// object factory is called inside a range-for inline body. The cell is created per-iteration in
// the loop-body block (non-entry), captured by two closures stored as object fields, and the
// object escapes into an outer array. The inline-body FreeCell must NOT fire for escaping cells.
#[test]
fn test_var_cell_escaping_via_object_in_loop_body() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { push } from "std/array"
import { range, for } from "std/iter"

var counters: {}[] = []
range(0, 5).for(i =>
  var count = i * 10
  push(counters, {
    "inc": () => count = count + 1,
    "get": () => count
  })
)

counters[0]["inc"]()
counters[1]["inc"]()
counters[1]["inc"]()
print(toString(counters[0]["get"]()))
print(toString(counters[1]["get"]()))
print(toString(counters[2]["get"]()))
print(toString(counters[3]["get"]()))
print(toString(counters[4]["get"]()))
"#);
    assert_eq!(output, vec!["1", "12", "20", "30", "40"]);
}

// Regression (worker capturing outer var, Bug 2): a worker thunk built inside a constructor fn
// that returns it; spec §24.6 makeCounter verbatim. The worker thread outlives the factory frame;
// the cell must not be freed at factory-scope exit. `lin_worker_new` retains the handler closure,
// keeping the env (and thus the cell pointer) alive for the worker's lifetime.
#[test]
fn test_var_cell_captured_by_worker() {
    let output = run(r#"import { worker, request } from "std/async"
import { print } from "std/io"
import { toString } from "std/string"

val makeCounter = () =>
  var count = 0
  worker(
    (msg: AnyVal) =>
      count = count + 1
      count,
    () => null
  )

val counter = makeCounter()
val n1 = request(counter, "tick")
val n2 = request(counter, "tick")
val n3 = request(counter, "tick")
print(toString(n1))
print(toString(n2))
print(toString(n3))
"#);
    assert_eq!(output, vec!["1", "2", "3"]);
}

// Regression (call-arg-box leak): passing a CONCRETE array to a AnyVal-typed param (`for`'s
// iterable) inside an outer loop boxes the array into a fresh TaggedVal* shell each outer
// iteration. The shell was never freed → one-box-per-iteration leak. Caller now frees the
// shell after the call. This runs the nested loop under churn; correctness here also guards
// against an over-eager shell free corrupting the borrowed array (double-free / wrong result).
#[test]
fn test_nested_for_over_concrete_array_arg_box() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range, for } from "std/iter"

var k = 0
val xs = [1, 2, 3]
range(0, 5000).for(j => xs.for(s => k = k + 1))
print(toString(k))
"#);
    assert_eq!(output, vec!["15000"]);
}

// Regression (nested-combinator-in-closure iterable leak): a FRESH combinator iterable
// (`range`/`map`/…) consumed by `.for`/`.while` INSIDE A CLOSURE BODY — e.g. the inner
// `range(0,50).for(...)` of `range(0,30).for(round => range(0,50).for(...))` — was never
// released. The fresh inner array was registered owned in the closure body scope, but the
// `for` result's transfer-on-escape alias (recorded for ANY boxed fresh-alloc arg) wrongly
// added it to the body-scope keep-set whenever the inner `for` was the closure's return
// expression — so the body-scope pop SKIPPED its release and the array leaked every outer
// iteration (~456 KB / 50 scans in the RAPTOR repro). Fix: only record the transfer-on-escape
// alias when the call RESULT is a union/AnyVal (the only thing a borrowed AnyVal param can be
// returned as) — `for`/`while` return `Null` and never hand the iterable back, so its box is a
// pure borrow already balanced by the shell free + arg-scope release. ASan is the leak guard
// (this asserts the nested loop still computes correctly — no over-release / double-free).
#[test]
fn test_nested_combinator_iterable_in_closure_no_leak() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { for, while, map, range } from "std/iter"

val scan = (): Int32 =>
  var acc = 0
  range(0, 30).for(round =>
    range(0, 50).for(stop => acc = acc + stop)
  )
  acc

// fresh `map` inner iterable
val scanMap = (): Int32 =>
  var acc = 0
  range(0, 30).for(round =>
    range(0, 50).map(x => x + 1).for(stop => acc = acc + stop)
  )
  acc

// `.while` inner over a fresh range
val scanWhile = (): Int32 =>
  var acc = 0
  range(0, 30).for(round =>
    range(0, 50).while(stop =>
      acc = acc + stop
      true
    )
  )
  acc

print(toString(scan()))
print(toString(scanMap()))
print(toString(scanWhile()))
"#);
    // scan: 30 * sum(0..49) = 30 * 1225 = 36750
    // scanMap: 30 * sum(1..50) = 30 * 1275 = 38250
    // scanWhile: same as scan = 36750
    assert_eq!(output, vec!["36750", "38250", "36750"]);
}

// Condition-only `while(() => Boolean)` overload: pure-Lin tail-recursive loop over a captured
// `var`. Verifies: (a) correct output, (b) no stack overflow from the TCO transform (the helper
// `whileLoop` self-recurses in tail position → alloca/loop in LLVM IR). Also checks that the
// existing iterable `while(xs, pred)` form is unaffected.
#[test]
fn test_condition_only_while_overload() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { while } from "std/iter"

// 1-arg overload: accumulate via captured var
var i = 0
while(() =>
  i = i + 1
  i < 5
)
print(toString(i))

// iterable while still works: collect values until pred fails
var last = 0
[1, 2, -3, 4].while(x =>
  last = x
  x >= 0
)
print(toString(last))
"#);
    assert_eq!(output, vec!["5", "-3"]);
}

// Regression (call-arg-box leak): a concrete Object passed to a AnyVal-typed param (`keys`)
// repeatedly under churn. Each call boxes the object into a fresh shell; the shell free must
// not touch the object's inner payload (which the object's own scope-exit release owns).
#[test]
fn test_object_to_json_param_under_churn() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"
import { range, for } from "std/iter"
import { keys } from "std/object"

val o = {"a": 1, "b": 2}
var n = 0
range(0, 5000).for(j => n = n + length(keys(o)))
print(toString(n))
"#);
    // keys(o) has 2 entries; summed 5000 times = 10000.
    assert_eq!(output, vec!["10000"]);
}

#[test]
fn test_combinator_bare_fn_widening_callback() {
    // Regression: a bare (named) combinator callback whose numeric param is WIDER than the source
    // array's element type. The bare-fn eta-expansion routes this through the inline loop; without a
    // width coercion at the element bind (and at the flat-result push) it emitted invalid LLVM
    // (`shl i32 %x, i64 1` / i32-into-i64 flat-push). Covers map (Int32→Int64, Int32→Float64) and a
    // widening filter predicate. The same-width and lambda forms already worked.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map, filter } from "std/iter"

val widenMul = (x: Int64) => x * 2
print(toString([1, 2, 3].map(widenMul)))

val toFloat = (x: Int32) => x * 1.5
print(toString([1, 2, 3].map(toFloat)))

val widenPred = (x: Int64) => x > 1
print(toString([1, 2, 3].filter(widenPred)))
"#);
    assert_eq!(output, vec!["[2, 4, 6]", "[1.5, 3.0, 4.5]", "[2, 3]"]);
}

#[test]
fn test_map_filter_reduce() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map, filter, reduce } from "std/iter"
import { for } from "std/iter"

val doubled = [1, 2, 3].map(x => x * 2)
doubled.for(x => print(toString(x)))

val evens = [1, 2, 3, 4].filter(x => x % 2 == 0)
evens.for(x => print(toString(x)))

val total = [1, 2, 3, 4].reduce(0, (sum, x) => sum + x)
print(toString(total))
"#);
    assert_eq!(output, vec!["2", "4", "6", "2", "4", "10"]);
}

#[test]
fn test_chaining() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map, filter, reduce } from "std/iter"

val result = [1, 2, 3, 4, 5]
  .map(x => x * x)
  .filter(x => x > 5)
  .reduce(0, (sum, x) => sum + x)
print(toString(result))
"#);
    assert_eq!(output, vec!["50"]);
}

// Path-8 Step 8.1: combinator-chain fusion widened to SEALED-RECORD element sources. A
// `trips.filter(...).map(...).reduce(...)` over a packed `Trip[]` is a SINGLE fused loop that reads
// each record field by const-offset (no `sealed_array_to_tagged` materialize of the source array, no
// per-stage intermediate array, no per-element indirect closure call). Asserts the values round-trip
// — a wrong RC/projection corrupts the fold. dur>15 keeps trips 2,3,4 → dist 200,300,400 → 900.
#[test]
fn test_fused_record_chain_reduce() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map, filter, reduce } from "std/iter"

type Trip = { "id": Int32, "dur": Int32, "dist": Int32 }
val trips: Trip[] = [
  { "id": 1, "dur": 10, "dist": 100 },
  { "id": 2, "dur": 20, "dist": 200 },
  { "id": 3, "dur": 30, "dist": 300 },
  { "id": 4, "dur": 40, "dist": 400 }
]
val total = trips.filter(t => t["dur"] > 15).map(t => t["dist"]).reduce(0, (a, x) => a + x)
print(toString(total))
"#);
    assert_eq!(output, vec!["900"]);
}

// Path-8 Step 8.1: array-producing fused terminals (`map`/`filter`) over a record chain — each is one
// pass building a single result array (no intermediate per-stage array). Asserts the kept elements'
// VALUES and order survive the fused projection/predicate (not just no-crash).
#[test]
fn test_fused_record_chain_array_terminals() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map, filter } from "std/iter"
import { length } from "std/array"

type Trip = { "id": Int32, "dur": Int32, "dist": Int32 }
val trips: Trip[] = [
  { "id": 1, "dur": 10, "dist": 100 },
  { "id": 2, "dur": 20, "dist": 200 },
  { "id": 3, "dur": 30, "dist": 300 },
  { "id": 4, "dur": 40, "dist": 400 }
]
// map TERMINAL: filter(dur>15) then project dist -> [200, 300, 400]
val ds: Int32[] = trips.filter(t => t["dur"] > 15).map(t => t["dist"])
print(toString(length(ds)))
print(toString(ds[0]))
print(toString(ds[2]))
// filter TERMINAL: project dist then filter(>150) -> [200, 300, 400]
val bs: Int32[] = trips.map(t => t["dist"]).filter(x => x > 150)
print(toString(length(bs)))
print(toString(bs[0]))
"#);
    assert_eq!(output, vec!["3", "200", "400", "3", "200"]);
}

// Path-8 Step 8.1: the for-terminal over a record chain (side-effecting) accumulating through a
// global var — the survivor reclaim must not double-free the materialized record nor leak it.
// dur>5 keeps all four → dist sum 1000.
#[test]
fn test_fused_record_chain_for_terminal() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map, filter, for } from "std/iter"

type Trip = { "id": Int32, "dur": Int32, "dist": Int32 }
val trips: Trip[] = [
  { "id": 1, "dur": 10, "dist": 100 },
  { "id": 2, "dur": 20, "dist": 200 },
  { "id": 3, "dur": 30, "dist": 300 },
  { "id": 4, "dur": 40, "dist": 400 }
]
var sum = 0
trips.filter(t => t["dur"] > 5).map(t => t["dist"]).for(x => sum = sum + x)
print(toString(sum))
"#);
    assert_eq!(output, vec!["1000"]);
}

// Path-8 Step 8.1: a heap-FIELD (String) record fused chain — the per-element materialize retains the
// String field and `lin_sealed_release` releases it; the fused path must keep that RC balanced. id>1
// keeps trips 2,3 → dist 4,6 → 10. (ASan scaling is the leak guard in the sealed-harness; this pins
// the value, which a wrong heap-field RC free would corrupt.)
#[test]
fn test_fused_heap_field_record_chain() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map, filter, reduce } from "std/iter"

type Trip = { "id": Int32, "name": String, "dist": Int32 }
val trips: Trip[] = [
  { "id": 0, "name": "a", "dist": 0 },
  { "id": 1, "name": "b", "dist": 2 },
  { "id": 2, "name": "c", "dist": 4 },
  { "id": 3, "name": "d", "dist": 6 }
]
val total = trips.filter(t => t["id"] > 1).map(t => t["dist"]).reduce(0, (a, x) => a + x)
print(toString(total))
"#);
    assert_eq!(output, vec!["10"]);
}

// Path-8 Step 8.1 regression: a 3-stage fused chain whose FIRST stage is a `map` consuming the source
// record, followed by a `filter`, then a terminal. The map frees the per-iteration source materialize;
// the downstream filter's drop path must NOT free it again (double-free → `lin_sealed_release` UAF on
// the 24-byte packed element). Covers map.filter.reduce, map.filter (array terminal) and map.filter.for.
// id 0..5 → dist*2 → keep >4 → {6,8,10} sum 24 / count 3 / for-sum 24.
#[test]
fn test_fused_map_first_then_filter_chain() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map, filter, reduce, for } from "std/iter"
import { length } from "std/array"

type Rec = { "id": Int32, "dur": Int32 }
val recs: Rec[] = [
  { "id": 0, "dur": 0 },
  { "id": 1, "dur": 1 },
  { "id": 2, "dur": 2 },
  { "id": 3, "dur": 3 },
  { "id": 4, "dur": 4 },
  { "id": 5, "dur": 5 }
]
// map FIRST, then filter, then reduce
val total = recs.map(r => r["dur"] * 2).filter(x => x > 4).reduce(0, (a, x) => a + x)
print(toString(total))
// map FIRST, then filter -> array terminal
val kept: Int32[] = recs.map(r => r["dur"] * 2).filter(x => x > 4)
print(toString(length(kept)))
// map FIRST, then filter, then for
var s = 0
recs.map(r => r["dur"] * 2).filter(x => x > 4).for(x => s = s + x)
print(toString(s))
"#);
    assert_eq!(output, vec!["24", "3", "24"]);
}

#[test]
fn test_destructuring() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val person = { "name": "Bob", "age": 42 }
val { name, age } = person
print(name)
print(toString(age))

val [first, second] = ["a", "b"]
print(first)
print(second)
"#);
    assert_eq!(output, vec!["Bob", "42", "a", "b"]);
}

#[test]
fn test_if_expressions() {
    let output = run(r#"import { print } from "std/io"

val a = if true then "yes" else "no"
print(a)

val b = if false then "yes" else "no"
print(b)

val x = 10
val c = if x > 5 then
  "big"
else
  "small"
print(c)
"#);
    assert_eq!(output, vec!["yes", "no", "big"]);
}

#[test]
fn test_if_old_syntax_error() {
    let err = run_expect_err(r#"val x = if true
  then "yes"
  else "no"
"#);
    assert!(err.contains("same line"), "got: {}", err);
}

#[test]
fn test_if_without_else() {
    let output = run(r#"import { print } from "std/io"

val arr: Int32[] = []
if true then print("ran")
if false then print("skipped")
print("done")
"#);
    assert_eq!(output, vec!["ran", "done"]);
}

#[test]
fn test_stdlib_imports() {
    let output = run(r#"
import { trim, toUpper } from "std/string"
import { print } from "std/io"

val cleaned = "  hello  ".trim().toUpper()
print(cleaned)
"#);
    assert_eq!(output, vec!["HELLO"]);
}

#[test]
fn test_array_oob_error() {
    let err = run_expect_err(r#"import { print } from "std/io"
import { toString } from "std/string"

val arr = [1, 2, 3]
val x = arr[10]
print(toString(x))
"#);
    assert!(err.contains("out of bounds") || err.contains("index"), "got: {}", err);
}

#[test]
fn test_dynamic_json_arith_missing_key_faults_cleanly() {
    // RAPTOR #5: dynamic `AnyVal` arithmetic on a missing object key (which reads as Null)
    // must route through the null-safe tagged-arith runtime path and produce a CLEAN runtime
    // fault, NOT a raw null-pointer-dereference panic from unboxing a null payload.
    // Operand order must not matter, and a present key must still compute normally.
    let err = run_expect_err(r#"import { print } from "std/io"
import { toString } from "std/string"
val run = (): Null =>
  val obj: AnyVal = { "a": 5 }
  val sum = 10 + obj["b"]
  print("sum=${toString(sum)}")
run()
"#);
    assert!(
        err.contains("cannot apply operator") && err.contains("Null"),
        "expected clean tagged-arith fault, got: {}",
        err
    );
    // CRUCIALLY: not a raw null-pointer-dereference panic.
    assert!(!err.contains("null pointer dereference"), "got raw panic: {}", err);

    // Operand-flipped form faults the same way.
    let err2 = run_expect_err(r#"import { print } from "std/io"
import { toString } from "std/string"
val run = (): Null =>
  val obj: AnyVal = { "a": 5 }
  val sum = obj["b"] + 10
  print("sum=${toString(sum)}")
run()
"#);
    assert!(
        err2.contains("cannot apply operator") && err2.contains("Null"),
        "expected clean tagged-arith fault (flipped), got: {}",
        err2
    );
    assert!(!err2.contains("null pointer dereference"), "got raw panic (flipped): {}", err2);

    // A present key computes normally through the boxed tagged path.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val run = (): Null =>
  val obj: AnyVal = { "a": 5 }
  val sum = 10 + obj["a"]
  print("sum=${toString(sum)}")
run()
"#);
    assert_eq!(out, vec!["sum=15"]);
}

#[test]
fn test_division_by_zero_error() {
    let err = run_expect_err(r#"import { print } from "std/io"
import { toString } from "std/string"

val x = 10 / 0
print(toString(x))
"#);
    assert!(err.contains("division") || err.contains("zero"), "got: {}", err);
}

#[test]
fn test_parenthesized_function_return_type() {
    // Regression: a parenthesised (grouped) function type in RETURN position —
    // `((AnyVal) => AnyVal)` — used to be a parse error ("expected Arrow, got ...") because the
    // type parser greedily consumed the function-BODY `=>`. It must parse, type-check, and run
    // identically to a named-alias / unparenthesised return type.
    let output = run(r#"import { print } from "std/io"

val mk = (h: AnyVal): ((AnyVal) => AnyVal) => (x: AnyVal): AnyVal => x

val f = mk({})
print(f(42))
"#);
    assert_eq!(output, vec!["42"]);
}

#[test]
fn test_multi_param_lambda() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { reduce } from "std/iter"

val total = [1, 2, 3].reduce(0, (sum, x) => sum + x)
print(toString(total))
"#);
    assert_eq!(output, vec!["6"]);
}

#[test]
fn test_string_literal_pattern() {
    let output = run(r#"import { print } from "std/io"

val greet = (name: String): String =>
  match name
    is "Dave" => "Big Dave!"
    is String => "Hello ${name}"

print(greet("Dave"))
print(greet("Bob"))
"#);
    assert_eq!(output, vec!["Big Dave!", "Hello Bob"]);
}

#[test]
fn test_negative_literals() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val x = -5
print(toString(x))
val f = (a: Int32, b: Int32): Int32 => a + b
val y = f(-5, 10 - 3)
print(toString(y))
"#);
    assert_eq!(output, vec!["-5", "2"]);
}

#[test]
fn test_assignment_as_expression() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

var count = 0
val result = count = count + 1
print(toString(result))
print(toString(count))
"#);
    assert_eq!(output, vec!["1", "1"]);
}

#[test]
fn test_non_exhaustive_match_error() {
    let err = run_expect_err(r#"import { print } from "std/io"

val x = 42
val y = match x
  is String => "string"
print(y)
"#);
    assert!(err.contains("non-exhaustive") || err.contains("match"), "got: {}", err);
}

#[test]
fn test_is_has_as_boolean_expressions() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val person = { "name": "Bob", "age": 42 }
val hasName = person has { name }
print(toString(hasName))
val isNull = null is Null
print(toString(isNull))
val isStr = "hello" is String
print(toString(isStr))
val isInt = "hello" is Int32
print(toString(isInt))
"#);
    assert_eq!(output, vec!["true", "true", "true", "false"]);
}

// Regression: `is T` where `T` is a generic type parameter. Before the fix the
// monomorphizer dropped the TypeVar inside the match-arm / `is`-expression pattern,
// so codegen compiled `is T` to a tag check against the 0xFF sentinel that never
// matched — the positive arm was silently dead and the DEFAULT was returned. This
// type-checked fine and returned wrong values at runtime.
#[test]
fn test_is_generic_typevar_match_form() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val genIsT = <T>(v: T | Null, d: T): T =>
  match v
    is T => v
    else => d

val run = (): Null =>
  print(toString(genIsT(7, -1)))
  print(toString(genIsT("hi", "x")))
  print(toString(genIsT(true, false)))

run()
"#);
    // The PRESENT value must be returned, not the default.
    assert_eq!(output, vec!["7", "hi", "true"]);
}

#[test]
fn test_is_generic_typevar_if_form() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val genIfT = <T>(v: T | Null, d: T): T =>
  if v is T then v else d

val run = (): Null =>
  print(toString(genIfT(42, -1)))
  print(toString(genIfT("yo", "x")))

run()
"#);
    assert_eq!(output, vec!["42", "yo"]);
}

// Concrete (non-generic) `is Int32` must still work — the fix must not disturb the
// ordinary scalar tag-check path.
#[test]
fn test_is_concrete_int32_still_works() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val concreteIsT = (v: Int32 | Null, d: Int32): Int32 =>
  match v
    is Int32 => v
    else => d

val run = (): Null =>
  print(toString(concreteIsT(7, -1)))
  print(toString(concreteIsT(null, -1)))

run()
"#);
    assert_eq!(output, vec!["7", "-1"]);
}

// `is T` where `T` resolves to a UNION (Int32 | String): the substituted `is <union>`
// must match a value whose runtime tag is ANY member of the union.
#[test]
fn test_is_generic_typevar_resolves_to_union() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val genIsT = <T>(v: T | Null, d: T): T =>
  match v
    is T => v
    else => d

val pick = (x: Int32 | String, dflt: Int32 | String): Int32 | String =>
  genIsT(x, dflt)

val run = (): Null =>
  print(toString(pick(99, "def")))
  print(toString(pick("hello", 0)))

run()
"#);
    // Both an Int32 value and a String value match `is T` (T = Int32 | String).
    assert_eq!(output, vec!["99", "hello"]);
}

#[test]
fn test_string_escape_sequences() {
    // "hello\tworld\n" has an embedded newline; print adds another.
    // Raw output: "hello\tworld\n\nshe said \"hi\"\nback\\slash\n"
    // After lines() + empty-filter the embedded \n splits into two entries.
    let output = run(r#"import { print } from "std/io"

val s = "hello\tworld\n"
print(s)
val q = "she said \"hi\""
print(q)
val bs = "back\\slash"
print(bs)
"#);
    assert_eq!(output, vec!["hello\tworld", "she said \"hi\"", "back\\slash"]);
}

#[test]
fn test_block_expression() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val result = (a: Int32): Int32 =>
  val doubled = a * 2
  val added = doubled + 1
  added

print(toString(result(5)))
"#);
    assert_eq!(output, vec!["11"]);
}

#[test]
fn test_dot_partial_application() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val add = (a: Int32, b: Int32): Int32 => a + b
val addFive = 5.add
print(toString(addFive(3)))
"#);
    assert_eq!(output, vec!["8"]);
}

#[test]
fn test_boolean_negation() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val ready = true
val notReady = !ready
print(toString(notReady))
val also = false == false
print(toString(also))
"#);
    assert_eq!(output, vec!["false", "true"]);
}

#[test]
fn test_logical_not_behaviours() {
    // Consolidated logical-`!` behaviours (5 former one-build success tests → one program; each
    // case keeps its own bindings and assertions in order). The non-bool `!5` type error keeps
    // its own `run_expect_err` test below.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

// val_and_if
val ready = true
print(toString(!ready))
val flag = false
if !flag then print("taken") else print("not-taken")

// in_match_guard: `!cond` in a `when` guard
val cond = false
val describe = (n: Int32): String =>
  match n
    has Int32 when !cond => "guard-true"
    else => "guard-false"
print(describe(1))

// precedence: `!a == b` parses as `(!a) == b`; `!` over index/call/`&&`
print(toString(!true == false))
val obj = { "ok": false }
print(toString(!obj["ok"]))
val isZero = (n: Int32): Boolean => n == 0
print(toString(!isZero(5)))
val pa = false
val pb = true
print(toString(!pa && pb))

// double_negation
val dx = true
print(toString(!!dx == dx))
print(toString(!!false))

// typevar_operand: `!b` where `b` flows through a generic lambda parameter exercises the
// unbox-to-i1 path in IR lowering.
val negate = (b) => !b
print(toString(negate(true)))
print(toString(negate(false)))
"#);
    assert_eq!(
        output,
        vec![
            "false",       // val_and_if: !ready
            "taken",       // val_and_if: if !flag
            "guard-true",  // in_match_guard
            "true",        // precedence: !true == false
            "true",        // precedence: !obj["ok"]
            "true",        // precedence: !isZero(5)
            "true",        // precedence: !pa && pb
            "true",        // double_negation: !!dx == dx
            "false",       // double_negation: !!false
            "false",       // typevar: negate(true)
            "true",        // typevar: negate(false)
        ]
    );
}

#[test]
fn test_logical_not_non_bool_error() {
    let err = run_expect_err(r#"import { print } from "std/io"
import { toString } from "std/string"

val x = !5
print(toString(x))
"#);
    assert!(
        err.contains("logical operator !") || err.contains("boolean operand"),
        "got: {}",
        err
    );
}

#[test]
fn test_string_comparison() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

print(toString("a" < "b"))
print(toString("b" < "a"))
print(toString("abc" <= "abc"))
print(toString("z" > "a"))
"#);
    assert_eq!(output, vec!["true", "false", "true", "true"]);
}

#[test]
fn test_string_vs_null_equality() {
    // Regression: comparing a String to `null` (the ubiquitous `s != null` guard) must be a
    // plain boolean, not a null-pointer deref. `lin_string_eq` previously dereferenced both
    // operands unconditionally; a Lin `null` is a null pointer, so `"s" == null` / `s != null`
    // crashed. Now null-safe (matching lin_object_eq / lin_array_eq).
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val s = "hello"
print(toString(s == null))
print(toString(s != null))
print(toString(null == s))

val obj = { "k": "v" }
print(toString(obj["k"] != null))
print(toString(obj["missing"] != null))
"#);
    assert_eq!(output, vec!["false", "true", "false", "true", "false"]);
}

#[test]
fn test_numeric_comparison() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

print(toString(1 < 2))
print(toString(2 < 1))
print(toString(5 >= 5))
print(toString(5 > 5))
print(toString(3.14 > 3.0))
print(toString(1 <= 1))
"#);
    assert_eq!(output, vec!["true", "false", "true", "false", "true", "true"]);
}

#[test]
fn test_logical_operators_short_circuit() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val x = true && true
print(toString(x))
val y = true && false
print(toString(y))
val z = false && true
print(toString(z))
val a = false || true
print(toString(a))
val b = true || false
print(toString(b))
val c = false || false
print(toString(c))
"#);
    assert_eq!(output, vec!["true", "false", "false", "true", "true", "false"]);
}

#[test]
fn test_logical_operators_short_circuit_evaluation() {
    // Spec §8: `&&` / `||` are SHORT-CIRCUITING — the RHS must NOT be evaluated when the LHS
    // already decides the result. This asserts EVALUATION order, not just the boolean value:
    //  - a side-effecting RHS (a print) must be absent from the output when short-circuited;
    //  - the canonical bounds-check guard `i < length(arr) && arr[i] > 0` must not index OOB.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"

val boomTrue = (): Boolean =>
  print("BOOM-AND")
  true
val boomFalse = (): Boolean =>
  print("BOOM-OR")
  false

// false && _ : RHS must NOT run.
val r1 = false && boomTrue()
print(toString(r1))
// true || _ : RHS must NOT run.
val r2 = true || boomFalse()
print(toString(r2))

// Guard idiom: index is out of bounds, so the LHS is false and arr[i] must not be evaluated.
val arr = [1, 2]
val safeAnd = (i: Int32): Boolean =>
  if i < length(arr) && arr[i] > 0 then true else false
print(toString(safeAnd(5)))
// `||` guard: LHS true short-circuits, so arr[i] must not be evaluated.
val safeOr = (i: Int32): Boolean =>
  if i >= length(arr) || arr[i] > 0 then true else false
print(toString(safeOr(5)))

print("end")
"#);
    // No "BOOM-AND" / "BOOM-OR" lines: the side-effecting RHS never ran.
    assert!(!output.contains(&"BOOM-AND".to_string()), "&& RHS was evaluated: {:?}", output);
    assert!(!output.contains(&"BOOM-OR".to_string()), "|| RHS was evaluated: {:?}", output);
    // Guards are safe (no OOB crash) and yield false / true respectively; program reaches "end".
    assert_eq!(output, vec!["false", "true", "false", "true", "end"]);
}

#[test]
fn test_if_block_branches() {
    let output = run(r#"import { print } from "std/io"

val x = 10
val result = if x > 5 then
  val prefix = "bi"
  "${prefix}g"
else
  val prefix = "sm"
  "${prefix}all"
print(result)
"#);
    assert_eq!(output, vec!["big"]);
}

#[test]
fn test_float_ieee754() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val inf = 1.0 / 0.0
print(toString(inf))
val neg_inf = -1.0 / 0.0
print(toString(neg_inf))
val nan = 0.0 / 0.0
print(toString(nan))
"#);
    assert_eq!(output, vec!["inf", "-inf", "NaN"]);
}

// Regression: arithmetic on two BOXED (AnyVal/union) operands — e.g. Float64 fields
// destructured from an object by a `has` pattern — dispatched on a hardcoded Int32
// unbox, so `3.0 * 4.0` reinterpreted the float bits as an integer and returned 0.
// Codegen now routes boxed-operand Add/Sub/Mul/Div/Mod through lin_tagged_arith,
// which dispatches on the runtime tag (float result if either operand is a float).
#[test]
fn test_boxed_json_float_arithmetic() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val o: AnyVal = { "a": 3.0, "b": 4.0 }
val mul = match o
  has { a, b } => a * b
  else => -1.0
val add = match o
  has { a, b } => a + b
  else => -1.0
val div = match o
  has { a, b } => a / b
  else => -1.0
print(toString(mul))
print(toString(add))
print(toString(div))

// Integer operands still use the integer path.
val oi: AnyVal = { "a": 3, "b": 4 }
val imul = match oi
  has { a, b } => a * b
  else => -1
print(toString(imul))

// Mixed int/float widens to float.
val om: AnyVal = { "a": 3, "b": 4.0 }
val mmul = match om
  has { a, b } => a * b
  else => -1.0
print(toString(mmul))
"#);
    assert_eq!(output, vec!["12.0", "7.0", "0.75", "12", "12.0"]);
}

#[test]
fn test_dynamic_json_arith_missing_key_faults() {
    // #5: dynamic `AnyVal` arithmetic where an operand is a missing object key. The key reads
    // as `Null` at runtime; the static path already rejects `Int32 + Null`, but two boxed
    // `AnyVal` operands type-check, so the runtime previously read the null payload as 0 and
    // silently produced `5 + null = 5` / `5 * null = 0`. It must now FAULT with a clear
    // message (not silently garble, and NOT invent JS NaN) — mirroring array-OOB faulting.
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let src_path = ws.join(format!("target/lin_test_json_arith_{}.lin", id));
    let bin_path = ws.join(format!("target/lin_test_json_arith_{}", id));
    fs::write(&src_path, r#"import { print } from "std/io"
import { toString } from "std/string"
val run = (): Null =>
  val obj: AnyVal = { "a": 5 }
  val x: AnyVal = obj["b"]
  val sum: AnyVal = obj["a"] + x
  print(toString(sum))
run()
"#).unwrap();
    let compile = lin_cmd()
        .args(["build", src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    let _ = fs::remove_file(&src_path);
    assert!(compile.status.success(), "compilation failed:\n{}",
        String::from_utf8_lossy(&compile.stderr));
    let run_out = Command::new(&bin_path).output().expect("failed to run compiled binary");
    let _ = fs::remove_file(&bin_path);
    assert!(!run_out.status.success(),
        "dynamic AnyVal arithmetic with a missing (Null) key must fault (non-zero exit)");
    let stderr = String::from_utf8_lossy(&run_out.stderr);
    assert!(stderr.contains("dynamic AnyVal operands") && stderr.contains("Null"),
        "expected a clear AnyVal-arithmetic runtime fault naming Null, got stderr:\n{}", stderr);
    // And it must NOT have printed a silently-garbled numeric result on stdout.
    let stdout = String::from_utf8_lossy(&run_out.stdout);
    assert!(stdout.trim().is_empty(),
        "must not silently produce a numeric result before faulting, got stdout:\n{}", stdout);
}

#[test]
fn test_dynamic_json_arith_present_keys_still_works() {
    // The fault must be narrow: arithmetic over two PRESENT numeric AnyVal keys is unaffected.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val run = (): Null =>
  val obj: AnyVal = { "a": 5, "b": 3 }
  print(toString(obj["a"] + obj["b"]))
  print(toString(obj["a"] * obj["b"]))
run()
"#);
    assert_eq!(output, vec!["8", "15"]);
}

#[test]
fn test_dynamic_json_arith_cmp_eq_operand_box_no_leak() {
    // RAPTOR leak #4: the TaggedVal* OPERAND shell freshly boxed to dispatch a tagged
    // arith/cmp/eq op (`lin_tagged_arith` / `lin_tagged_cmp` / `lin_tagged_eq`) — which only
    // READ their operands — was never reclaimed, leaking one 16-byte shell per op in hot loops.
    // The fix reclaims the shell (shell-only `lin_tagged_free_box` / IR `FreeBoxShell`); a WRONG
    // free would double-free the operand's inner (e.g. the borrowed string literal `"pass"`) and
    // crash. ASan (CI asan leg) is the leak/double-free guard; this asserts the arithmetic and
    // comparison results stay correct under the new frees. Covers: union+concrete arith (acc +
    // literal, grows past the small-int cache), dynamic float arith, union < concrete cmp, and
    // union == string-literal eq (the borrowed-string operand-shell case).
    let output = run(r#"import { print } from "std/io"
import { for, range } from "std/iter"
import { toString } from "std/string"
val rec: AnyVal = { "f": 2.5, "status": "pass" }
val arith = (): AnyVal =>
  var acc: AnyVal = 0
  range(0, 50).for(_ => acc = acc + 100000)
  acc
val floats = (): AnyVal =>
  var f: AnyVal = rec["f"]
  range(0, 4).for(_ => f = f + 1.5)
  f
val cmp = (): Int32 =>
  var hits: Int32 = 0
  range(0, 50).for(_ => if rec["f"] < 999999 then hits = hits + 1 else hits = hits)
  hits
val eqstr = (): Int32 =>
  var c: Int32 = 0
  range(0, 50).for(_ => if rec["status"] == "pass" then c = c + 1 else c = c)
  c
print(toString(arith()))
print(toString(floats()))
print("${cmp()}")
print("${eqstr()}")
"#);
    assert_eq!(output, vec!["5000000", "8.5", "50", "50"]);
}

#[test]
fn test_dynamic_json_arith_fault_catchable_in_async() {
    // A AnyVal-arithmetic fault raised inside an async thunk unwinds to the boundary and is
    // caught as an `Error` (proving lin_tagged_arith's `extern "C-unwind"` ABI), exactly like
    // a division-by-zero / OOB fault inside a boundary.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { async, await } from "std/async"
val run = (): Null =>
  val obj: AnyVal = { "a": 5 }
  val p = async((): AnyVal =>
    val x: AnyVal = obj["b"]
    obj["a"] + x
  )
  val r = await(p)
  if r is Error then print("caught") else print(toString(r))
run()
"#);
    assert_eq!(output, vec!["caught"]);
}

#[test]
fn test_scalar_error_union_narrows_and_phi() {
    // A union of a SCALAR with Error (`Int64 | Error`):
    //   (a) NARROWING under `is Error` must refine the binding to the bare scalar in the
    //       else/non-error branch, so it can flow into an `Int64`-parameter use.
    //   (b) The if/match merge that consumes the narrowed scalar alongside an int LITERAL must
    //       not MISCOMPILE its PHI. The literal `0`/`-1` defaults to Int32 while the merge result
    //       is Int64; without a width coercion at the merge the emitted PHI mixed an i32 and an
    //       i64 incoming and LLVM rejected the module ("PHI node operands are not the same type as
    //       the result"). The fix coerces the narrower-int branch to the merge's result width.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val mk = (n: Int64): Int64 | Error => if n < 0 then { "type": "error", "message": "neg" } else n
val use = (n: Int64): Int64 => n + 100
val r1 = mk(5)
val out1 = if r1 is Error then 0 else use(r1)
print(toString(out1))
val r2 = mk(0 - 3)
val out2: Int64 = match r2
  is Error => 0 - 1
  else => use(r2)
print(toString(out2))
// bare narrowed scalar as the else-arm, merged with an int literal then used in arithmetic
val r3 = mk(7)
val out3 = if r3 is Error then 0 else r3
print(toString(out3))
"#);
    assert_eq!(output, vec!["105", "-1", "7"]);
}

#[test]
fn test_scalar_error_union_error_branch_not_narrowed() {
    // The COMPLEMENT of the narrowing: in the `then` (Error) branch of `is Error` the binding is
    // NOT refined to the scalar — it stays `Int64 | Error` — so passing it where an `Int64` is
    // expected is a type error. Guards against the narrowing leaking into the wrong branch.
    let err = run_expect_err(r#"import { print } from "std/io"
val mk = (n: Int64): Int64 | Error => if n < 0 then { "type": "error", "message": "neg" } else n
val use = (n: Int64): Int64 => n
val r = mk(5)
val out = if r is Error then use(r) else 0
print(toString(out))
"#);
    assert!(
        err.contains("expected Int64") || err.contains("Argument 1 has type"),
        "expected a narrowing type error in the Error branch, got:\n{err}"
    );
}

#[test]
fn test_captured_var_array_inplace_push_in_for() {
    // Repro 1 (natural form): a captured `var` array mutated in place by `push` inside a `.for`
    // loop accumulates correctly across iterations (the heap slot is shared by reference, ADR-012).
    // `push` is in-place and returns Null, so the accumulator is NOT reassigned — `push(acc, x)`,
    // not `acc = push(acc, x)` (the latter would assign Null, a genuine — and correct — type error).
    let output = run(r#"import { range, for } from "std/iter"
import { push, length } from "std/array"
import { print } from "std/io"
import { toString } from "std/string"

var acc: Int32[] = []
range(0, 5).for(i => push(acc, i * 10))
print(toString(length(acc)))
print(toString(acc))
"#);
    assert_eq!(output, vec!["5", "[0, 10, 20, 30, 40]"]);
}

#[test]
fn test_map_index_place_narrows_through_if_null_test() {
    // Repro 2: the `{String:T}` map-build idiom. A map-index read `m[k]` is typed `T | Null`
    // (safe-bracket §6.1); `if m[k] != null then m[k] else []` must narrow the then-branch re-read
    // of `m[k]` to `T` so it is assignable to a `T[]` binding. Before the fix this failed with
    // "Expected type Int32[], got Int32[] | Null" — narrowing only fired for simple identifiers,
    // not index places. The whole loop builds a 2-key map (k = "r0"/"r1"), 3 entries each.
    let output = run(r#"import { range, for } from "std/iter"
import { push, length } from "std/array"
import { keys } from "std/object"
import { print } from "std/io"
import { toString } from "std/string"

var m: { String: Int32[] } = {}
range(0, 6).for(i =>
  val k = "r${i % 2}"
  var cur: Int32[] = if m[k] != null then m[k] else []
  m[k] = cur
  push(cur, i))
print(toString(length(keys(m))))
print(toString(length(m["r0"])))
print(toString(length(m["r1"])))
"#);
    assert_eq!(output, vec!["2", "3", "3"]);
}

#[test]
fn test_object_index_by_total_literal_union_drops_null() {
    // Indexing a record by a key whose TYPE is a closed union of string literals, ALL of which
    // are declared fields, is provably total: the safe-bracket `Null` (§6.1, missing-key
    // fallback) must NOT be added. `dow: DayOfWeek` (= the seven day literals) indexing a
    // `ServiceDays` record keyed by exactly those seven reads precisely `Boolean` — so a
    // `: Boolean` return annotation type-checks. Before the fix the body inferred
    // `Boolean | … | Boolean | Null` and failed the return-type check. This is the exact shape
    // from the user's GTFS `service.lin`, run end-to-end: the literal-union argument refinement
    // (a bare `"Monday"` narrows to the `DayOfWeek` member) lets `runsOn` be CALLED with a literal.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

type DayOfWeek = "Monday" | "Tuesday" | "Wednesday" | "Thursday" | "Friday" | "Saturday" | "Sunday"
type ServiceDays = { "Monday": Boolean, "Tuesday": Boolean, "Wednesday": Boolean, "Thursday": Boolean, "Friday": Boolean, "Saturday": Boolean, "Sunday": Boolean }

val runsOn = (days: ServiceDays, dow: DayOfWeek): Boolean => days[dow]

val d: ServiceDays = { "Monday": true, "Tuesday": false, "Wednesday": false, "Thursday": false, "Friday": false, "Saturday": false, "Sunday": false }
print(toString(runsOn(d, "Monday")))
print(toString(d.runsOn("Tuesday")))
"#);
    assert_eq!(output, vec!["true", "false"]);
}

#[test]
fn test_bare_string_literal_refines_to_literal_union() {
    // A bare string literal narrows to a member of an expected closed string-literal union
    // (ADR-034 pushdown, extended from a single `StrLit` to a union of them) — in val-binding,
    // argument, and return positions. Previously the literal stayed `String` and was rejected
    // against the union target ("got String"). A non-member literal is still an error.
    let output = run(r#"import { print } from "std/io"

type Dir = "north" | "south" | "east" | "west"

val opposite = (d: Dir): Dir =>
  if d == "north" then "south"
  else if d == "south" then "north"
  else if d == "east" then "west"
  else "east"

val start: Dir = "north"
print(opposite(start))
print(opposite("east"))
"#);
    assert_eq!(output, vec!["south", "west"]);

    // Non-member literal is rejected against the union (val-binding position).
    let err = run_expect_err(r#"type Dir = "north" | "south"
val d: Dir = "up"
"#);
    assert!(err.contains("north") && err.contains("up"), "expected union-mismatch error, got: {err}");
}

#[test]
fn test_object_index_by_partial_literal_union_keeps_null() {
    // Soundness counterpart: when the key-type union has a literal that is NOT a field of the
    // record, the access genuinely might miss, so the safe-bracket `Null` stays. The result
    // collapses duplicates (`Boolean | Null`, not `Boolean | Boolean | Null`) but still carries
    // `Null`, so a bare `: Boolean` return is correctly rejected.
    let err = run_expect_err(r#"type Two = "a" | "z"
type Rec = { "a": Boolean, "b": Boolean }
val f = (r: Rec, k: Two): Boolean => r[k]
"#);
    assert!(err.contains("Boolean | Null"), "expected collapsed `Boolean | Null`, got: {err}");
}

#[test]
fn test_index_sig_literal_union_key_expands_to_record() {
    // `{ <literal-union>: V }` is sugar for a fixed record with one field per literal (value type
    // V). `{ DayOfWeek: Boolean }` ≡ `{ "Monday": Boolean, …, "Sunday": Boolean }`. Indexing it by
    // a key of the same union is provably total (no `Null`), so `runsOn` returns `Boolean`. Run
    // end-to-end with a record literal supplying all seven fields.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

type DayOfWeek = "Monday" | "Tuesday" | "Wednesday" | "Thursday" | "Friday" | "Saturday" | "Sunday"
type Calendar = { DayOfWeek: Boolean }

val runsOn = (c: Calendar, dow: DayOfWeek): Boolean => c[dow]

val c: Calendar = { "Monday": true, "Tuesday": false, "Wednesday": false, "Thursday": false, "Friday": false, "Saturday": false, "Sunday": false }
print(toString(runsOn(c, "Monday")))
print(toString(c.runsOn("Sunday")))
"#);
    assert_eq!(output, vec!["true", "false"]);
}

#[test]
fn test_index_sig_literal_union_record_equiv_to_handwritten() {
    // The sugar produces a record STRUCTURALLY IDENTICAL to the hand-written one: a `Calendar`
    // (= `{ DayOfWeek: Boolean }`) value is assignable to the equivalent explicit record type and
    // vice versa. This pins that the expansion is plain structural typing, not a distinct kind.
    let output = run(r#"import { print } from "std/io"

type DayOfWeek = "Mon" | "Tue"
type Calendar = { DayOfWeek: Boolean }
type Hand = { "Mon": Boolean, "Tue": Boolean }

val asHand = (c: Calendar): Hand => c
val asCal = (h: Hand): Calendar => h

val c: Calendar = { "Mon": true, "Tue": false }
val h: Hand = asHand(c)
val back: Calendar = asCal(h)
print(if back["Mon"] then "ok" else "no")
"#);
    assert_eq!(output, vec!["ok"]);
}

// ── Integer-literal union types ─────────────────────────────────────────────────────────────────

/// `type DaysOfWeek = 0 | 1 | 2 | 3 | 4 | 5 | 6` — basic round-trip: assign a member value,
/// pass to a function, match exhaustively, get the right result.
#[test]
fn test_int_lit_type_member_ok() {
    let output = run(r#"import { print } from "std/io"

type DaysOfWeek = 0 | 1 | 2 | 3 | 4 | 5 | 6

val dayName = (d: DaysOfWeek): String =>
  match d
    is 0 => "Sunday"
    is 1 => "Monday"
    is 2 => "Tuesday"
    is 3 => "Wednesday"
    is 4 => "Thursday"
    is 5 => "Friday"
    is 6 => "Saturday"

val today: DaysOfWeek = 3
print(dayName(today))
"#);
    assert_eq!(output, vec!["Wednesday"]);
}

/// Assigning a value outside the declared integer-literal union is a compile-time type error.
#[test]
fn test_int_lit_type_out_of_range_is_type_error() {
    let err = run_expect_err(r#"type DaysOfWeek = 0 | 1 | 2 | 3 | 4 | 5 | 6
val bad: DaysOfWeek = 9
"#);
    // The error should mention the expected union and the bad literal.
    assert!(
        err.contains("Expected type") && err.contains("9"),
        "expected out-of-range type error, got: {err}"
    );
}

/// A non-exhaustive match on an integer-literal union emits a compile-time error listing
/// the uncovered cases.
#[test]
fn test_int_lit_type_non_exhaustive_match_is_error() {
    let err = run_expect_err(r#"type DaysOfWeek = 0 | 1 | 2 | 3 | 4 | 5 | 6
val dayName = (d: DaysOfWeek): String =>
  match d
    is 0 => "Sunday"
    is 1 => "Monday"
"#);
    assert!(
        err.contains("non-exhaustive") && err.contains("2"),
        "expected non-exhaustive match error, got: {err}"
    );
}

#[test]
fn test_index_sig_string_key_still_map_and_bad_key_errors() {
    // The `{ String: V }` map form is UNCHANGED: arbitrary string keys, read yields `V | Null`.
    let output = run(r#"import { print } from "std/io"

type Seen = { String: Boolean }
var s: Seen = {}
s["x"] = true
print(if s["x"] != null then "present" else "absent")
print(if s["y"] != null then "present" else "absent")
"#);
    assert_eq!(output, vec!["present", "absent"]);

    // A key type that is Float (not String, not integer, not string-literal union) is rejected.
    let err = run_expect_err(r#"type R = { Float64: Boolean }
"#);
    assert!(
        err.contains("Index-signature key type must be String"),
        "expected index-sig key error, got: {err}"
    );
    // Int32 is now a VALID index-signature key (numeric-key maps feature).
    let _accepted = check_source(r#"type R = { Int32: Boolean }"#);
    // A String-keyed map read with an int key is still rejected.
    let err2 = run_expect_err(r#"type M = { String: Boolean }
val f = (m: M, k: Int32): Boolean => m[k] ?? false
"#);
    assert!(
        err2.contains("keyed by") && err2.contains("String") && err2.contains("Int32"),
        "expected String-key error with Int32, got: {err2}"
    );
}

#[test]
fn test_keys_over_non_string_keyed_map_returns_native_key_type() {
    // ADR-086 (revised): `keys`/`values`/`entries` accept a map with ANY key type (`{ UInt8: V }`,
    // `{ DateNumber: V }`, …), not just `{ String: V }`. `keys()` returns the keys in their NATIVE
    // type `K[]` — a `{ UInt8: V }` map yields a `UInt8[]` of INTEGERS (3, 10), usable to re-index
    // the map and in arithmetic, NOT a stringified `String[]`.
    let output = run(r#"import { print } from "std/io"
import { keys, values } from "std/object"
import { length } from "std/array"
import { toString } from "std/string"
import { for } from "std/iter"

type M = { UInt8: String }

var m: M = {}
m[3] = "three"
m[10] = "ten"
val k: UInt8[] = m.keys()
print(toString(length(k)))
// Native int keys: iterate them, re-index the map with them, and do arithmetic.
k.for(key => print(toString(key)))
k.for(key => print(toString(m[key] ?? "?")))
print(toString(k[0] + 1))
m.values().for(v => print(v))
"#);
    assert_eq!(
        output,
        vec!["2", "3", "10", "three", "ten", "4", "three", "ten"]
    );

    // A non-map argument is still rejected: `keys(5)`, `keys("s")`, `keys([1,2])` are errors.
    for bad in ["keys(5)", "keys(\"s\")", "keys([1, 2])"] {
        let err = run_expect_err(&format!(
            "import {{ keys }} from \"std/object\"\nval x = {bad}\n"
        ));
        assert!(
            err.contains("Argument 1 has type"),
            "expected arg-type rejection for {bad}, got: {err}"
        );
    }

    // A String-keyed map keeps working exactly as before.
    let s = run(r#"import { print } from "std/io"
import { keys } from "std/object"
import { for } from "std/iter"
type S = { String: String }
var m: S = {}
m["a"] = "alpha"
m["b"] = "beta"
m.keys().for(key => print(key))
"#);
    assert_eq!(s, vec!["a", "b"]);
}

#[test]
fn test_chained_nested_map_index_resolves_value_type_and_narrows() {
    // ADR-087: a chained index `idx[a][b]` over a nested map `{ String: { K: V } }` resolves to
    // `V | Null` — identical to binding the intermediate — rather than collapsing to a fresh
    // TypeVar (`?T | Null`), which previously defeated downstream `is`/narrowing. Here the inner
    // value is a union; the chained read must narrow to `Transfer` and read its field back.
    let output = run(r#"import { print } from "std/io"

type Trip = { "tripId": String }
type Transfer = { "origin": String, "destination": String, "duration": UInt32 }
type Connection = [Trip, Int32, Int32]
type Inner = { UInt8: Connection | Transfer }
type Index = { String: Inner }

val f = (idx: Index, dest: String): String =>
  val c = idx[dest][3]
  if c is Transfer then c["origin"] else "none"

val t: Transfer = { "origin": "PADTON", "destination": "READING", "duration": 25 }
var inner: Inner = {}
inner[3] = t
var idx: Index = {}
idx["A"] = inner

print(f(idx, "A"))
print(f(idx, "B"))
"#);
    assert_eq!(output, vec!["PADTON", "none"]);

    // The pure type-check repro (chained read names the inner value type, not `?T`).
    let (ok, msg) = check_source(r#"type V = { "x": Int32 }
type Index = { String: { UInt8: V } }
val f = (idx: Index, dest: String): V | Null =>
  idx[dest][3]
"#);
    assert!(ok, "chained nested-map index should type-check to `V | Null`, got: {msg}");
}

#[test]
fn test_index_place_narrowing_else_branch_and_no_leak() {
    // Two soundness facets of index-place narrowing:
    //   (a) `== null` narrows the ELSE branch to non-null: `if m[k] == null then [] else m[k]`
    //       reads `m[k]` as `Int32[]` in the else arm.
    //   (b) the narrowing does NOT leak past the `if` — a bare `m[k]` afterwards is still
    //       `Int32[] | Null`, which the `length` arg accepts (length is total) but a `T[]`-typed
    //       binding would reject. Here we just confirm the else-branch read compiles + runs.
    let output = run(r#"import { length } from "std/array"
import { print } from "std/io"
import { toString } from "std/string"

var m: { String: Int32[] } = {}
m["x"] = [9, 8, 7]
val a: Int32[] = if m["x"] == null then [] else m["x"]
print(toString(length(a)))
"#);
    assert_eq!(output, vec!["3"]);
}

#[test]
fn test_index_place_narrowing_does_not_leak_past_if() {
    // The narrowing must be scoped to the matched branch: a read of `m[k]` OUTSIDE the `if` keeps
    // its `T | Null` type, so assigning it to a `T[]` binding is still a type error. Guards against
    // the index narrowing leaking into following statements.
    let err = run_expect_err(r#"import { print } from "std/io"
var m: { String: Int32[] } = {}
val a: Int32[] = if m["x"] != null then m["x"] else []
val b: Int32[] = m["x"]
print(toString(a))
"#);
    assert!(
        err.contains("Int32[] | Null") || err.contains("got Int32[] | Null"),
        "expected a non-narrowed `Int32[] | Null` error outside the if, got:\n{err}"
    );
}

#[test]
fn test_compound_base_index_place_narrows_through_if_null_test() {
    // Index-place null-narrowing through a COMPOUND base: `service["dates"]` is a record-field read
    // reaching an inner `{String:T}` map, and `if service["dates"][date] != null then
    // service["dates"][date]` must narrow the re-read to `T` (drop the safe-bracket `Null`). The
    // narrowing place is the full path `service -> "dates" -> date`, not just a top-level
    // identifier. Run end-to-end: an absent then a present key.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

type Dates = { String: Int32 }
type Service = { "name": String, "dates": Dates }

val lookup = (service: Service, date: String): Int32 =>
  if service["dates"][date] != null then service["dates"][date]
  else 0 - 1

var d: Dates = {}
d["2026-06-13"] = 42
val s: Service = { "name": "bus", "dates": d }
print(toString(lookup(s, "2026-06-13")))
print(toString(lookup(s, "missing")))
"#);
    assert_eq!(output, vec!["42", "-1"]);
}

#[test]
fn test_compound_base_narrowing_invalidated_by_key_reassignment() {
    // Soundness: reassigning the KEY variable after the null-test means `o["a"][k]` may denote a
    // different slot, so the narrowing must be dropped and the re-read re-widened to `T | Null`.
    // (`place_path_mentions` finds `k` as an identifier key in the path.)
    let err = run_expect_err(r#"type Inner = { String: Int32 }
type Outer = { "a": Inner }
val f = (o: Outer): Int32 =>
  var k = "x"
  if o["a"][k] != null then
    k = "y"
    o["a"][k]
  else
    0
"#);
    assert!(
        err.contains("Int32 | Null"),
        "expected re-widened `Int32 | Null` after key reassignment, got:\n{err}"
    );
}

#[test]
fn test_compound_base_narrowing_invalidated_by_write_through_root() {
    // Soundness: a write through the same root (`o["a"][j] = …`) may alias the narrowed slot
    // (`o["a"]["x"]`), so the narrowing rooted at `o` is conservatively cleared and the sibling
    // re-read re-widens to `T | Null`.
    let err = run_expect_err(r#"type Inner = { String: Int32 }
type Outer = { "a": Inner }
val f = (o: Outer, j: String): Int32 =>
  if o["a"]["x"] != null then
    o["a"][j] = 5
    o["a"]["x"]
  else
    0
"#);
    assert!(
        err.contains("Int32 | Null"),
        "expected re-widened `Int32 | Null` after write through the root, got:\n{err}"
    );
}

// ---------------------------------------------------------------------------
// ADR-076: ASSIGNMENT-based index-place narrowing. After a write of a non-null value to a stable
// index place `m[k]`, a later read of that SAME place narrows to the assigned (non-null) type —
// the `m[k] = m[k] ?? []; m[k].push(x)` idiom — until invalidated (reassign an identifier the path
// mentions, a write through the same base prefix, a call that could mutate the map, or block exit).
// Plus: stable nested place-path keys (`m[row["id"]]`) are admitted as narrowable places.
// ---------------------------------------------------------------------------

#[test]
fn test_assign_narrows_index_place_simple_key() {
    // After `m[k] = m[k] ?? []`, the re-read `m[k]` is the assigned non-null `String[]`, so
    // `m[k].push("x")` (which needs a non-null `String[]` receiver) type-checks and runs.
    let output = run(r#"import { print } from "std/io"
import { push, length } from "std/array"
import { toString } from "std/string"

val f = (m: { String: String[] }, k: String): Int32 =>
  m[k] = m[k] ?? []
  m[k].push("x")
  length(m[k])

var m: { String: String[] } = {}
print(toString(f(m, "a")))
"#);
    assert_eq!(output, vec!["1"]);
}

#[test]
fn test_assign_narrows_index_place_nested_path_key() {
    // The same idiom keyed on a stable NESTED place-path (`m2[row["id"]]`): the key `row["id"]` is
    // itself a re-readable place, so the place canonicalizes and the narrowing applies. Runs the
    // `addTransfer`/`addLink` shape end-to-end.
    let output = run(r#"import { print } from "std/io"
import { push, length } from "std/array"
import { toString } from "std/string"

val f = (m2: { String: String[] }, row: AnyVal): Int32 =>
  m2[row["id"]] = m2[row["id"]] ?? []
  m2[row["id"]].push("x")
  length(m2[row["id"]])

var m2: { String: String[] } = {}
val row = { "id": "k1" }
print(toString(f(m2, row)))
"#);
    assert_eq!(output, vec!["1"]);
}

#[test]
fn test_assign_narrowing_invalidated_by_key_reassignment() {
    // Soundness: reassigning the KEY variable after the assignment means `m[k]` may denote a
    // different slot, so the narrowing is dropped and the re-read re-widens to `String[] | Null`.
    let err = run_expect_err(r#"import { push } from "std/array"
val f = (m: { String: String[] }, otherKey: String) =>
  var k: String = "a"
  m[k] = m[k] ?? []
  k = otherKey
  m[k].push("x")
"#);
    assert!(
        err.contains("String[] | Null"),
        "expected re-widened `String[] | Null` after key reassignment, got:\n{err}"
    );
}

#[test]
fn test_assign_narrowing_invalidated_by_nested_key_root_reassignment() {
    // Soundness: the nested key path `row["id"]` mentions `row`; reassigning `row` may make the
    // place denote a different slot, so the narrowing is dropped (re-widened to `String[] | Null`).
    let err = run_expect_err(r#"import { push } from "std/array"
val f = (m2: { String: String[] }, other: AnyVal) =>
  var row: AnyVal = { "id": "a" }
  m2[row["id"]] = m2[row["id"]] ?? []
  row = other
  m2[row["id"]].push("x")
"#);
    assert!(
        err.contains("String[] | Null"),
        "expected re-widened `String[] | Null` after nested-key root reassignment, got:\n{err}"
    );
}

#[test]
fn test_assign_narrowing_invalidated_by_base_reassignment() {
    // Soundness: reassigning the BASE map binding means `m[k]` reads a different map, so the
    // narrowing rooted at `m` is cleared and the re-read re-widens to `String[] | Null`.
    let err = run_expect_err(r#"import { push } from "std/array"
val f = (k: String) =>
  var m: { String: String[] } = {}
  var m2: { String: String[] } = {}
  m[k] = m[k] ?? []
  m = m2
  m[k].push("x")
"#);
    assert!(
        err.contains("String[] | Null"),
        "expected re-widened `String[] | Null` after base reassignment, got:\n{err}"
    );
}

#[test]
fn test_assign_narrowing_different_key_still_nullable() {
    // Soundness: only the ASSIGNED key narrows. A read of a DIFFERENT key `m[other]` is still
    // `String[] | Null`, so `m[other].push(...)` is rejected.
    let err = run_expect_err(r#"import { push } from "std/array"
val f = (m: { String: String[] }, k: String, other: String) =>
  m[k] = m[k] ?? []
  m[other].push("x")
"#);
    assert!(
        err.contains("String[] | Null"),
        "expected `String[] | Null` for a different (unassigned) key, got:\n{err}"
    );
}

#[test]
fn test_assign_narrowing_cleared_by_intervening_call() {
    // Soundness: a function CALL between the assignment and the read could mutate the map (delete
    // the key), so all index-narrowings are conservatively cleared after a call — the re-read
    // re-widens to `String[] | Null`. (The receiver of `m[k].push(...)` is evaluated BEFORE the
    // push call, so the narrowed-read idiom itself is unaffected — see the positive tests above.)
    let err = run_expect_err(r#"import { push } from "std/array"
val touch = (m: { String: String[] }): Int32 => 1
val f = (m: { String: String[] }, k: String) =>
  m[k] = m[k] ?? []
  val z = touch(m)
  m[k].push("x")
"#);
    assert!(
        err.contains("String[] | Null"),
        "expected re-widened `String[] | Null` after an intervening call, got:\n{err}"
    );
}

#[test]
fn test_assign_narrowing_does_not_leak_past_block() {
    // Soundness: an assignment-narrowing established INSIDE an `if` block must not leak past it. A
    // read of `m[k]` after the block re-widens to `String[] | Null`.
    let err = run_expect_err(r#"import { push } from "std/array"
val f = (m: { String: String[] }, k: String): String[] =>
  if k != "" then
    m[k] = m[k] ?? []
    m[k].push("inside")
  val b: String[] = m[k]
  b
"#);
    assert!(
        err.contains("String[] | Null"),
        "expected re-widened `String[] | Null` after the block, got:\n{err}"
    );
}

#[test]
fn test_map_read_with_non_string_key_rejected() {
    // A `{ String: T }` map READ must reject a non-String key (mirrors the index-ASSIGN guard,
    // which already did). Previously a numeric key passed type-checking and emitted invalid LLVM
    // (`lin_map_get` takes a string pointer, so an integer key produced a malformed call — "Call
    // parameter type does not match function signature"). Now it is a clear type error.
    let err = run_expect_err(r#"type M = { String: Boolean }
val f = (m: M, k: UInt32): Boolean => m[k] ?? false
"#);
    assert!(
        err.contains("keyed by") && err.contains("String") && err.contains("UInt32"),
        "expected a String-key error mentioning UInt32, got:\n{err}"
    );

    // A genuine String key still reads fine (guard does not over-reject).
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type M = { String: Boolean }
var store: M = {}
store["x"] = true
val f = (tbl: M, k: String): Boolean => tbl[k] ?? false
print(toString(f(store, "x")))
print(toString(f(store, "y")))
"#);
    assert_eq!(output, vec!["true", "false"]);
}

#[test]
fn test_float32_widens_to_float64() {
    // A Float32 must widen to Float64 (fpext) across every numeric context, per spec §21
    // (widening is always to a type that represents both). Codegen's Coerce had no
    // float→float arm and its binary-op path didn't reconcile two floats of different
    // widths, so each of these failed with "Call parameter type does not match" /
    // "Both operands ... not of the same type". 0.5 is exact in both f32 and f64.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { toFloat32 } from "std/number"

val a: Float32 = toFloat32(0.5)

// (C) Float32 -> Float64 binding (Coerce).
val b: Float64 = a
print(toString(b))                 // 0.5

// (A) Float32 argument to a Float64 parameter.
val takesF64 = (x: Float64): Float64 => x * 2.0
print(toString(takesF64(a)))       // 1.0

// (B) Float32 + Float64 arithmetic widens to Float64.
print(toString(a + 1.0))           // 1.5
print(toString(a + a))             // 1.0 (f32 + f32 still works)

// Narrowing back is explicit via toFloat32 and must still round-trip.
val c: Float32 = toFloat32(b)
print(toString(c))                 // 0.5
"#);
    assert_eq!(output, vec!["0.5", "1.0", "1.5", "1.0", "0.5"]);
}

#[test]
fn test_float_literal_adopts_float32_context() {
    // A suffixless float literal defaults to Float64, but when the expected/context type is
    // precisely Float32 it must adopt Float32 — mirroring how a suffixless integer literal
    // adapts to Int8/UInt8/etc. Previously the checker rejected `val x: Float32 = 0.5` with
    // "Expected type Float32, got Float64", which then cascaded to "Undefined variable 'x'".
    // Exercises val-binding, array-literal element, function arg, and function return contexts.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

// (1) val binding context.
val x: Float32 = 0.5
print(toString(x))                 // 0.5

// (2) Array-literal element context (Float32[]).
val xs: Float32[] = [0.5, 0.25]
print(toString(xs))                // [0.5, 0.25]

// (3) Function argument context: a bare float literal into a Float32 parameter.
val takesF32 = (v: Float32): Float32 => v
print(toString(takesF32(0.75)))    // 0.75

// (4) Function return context: a bare float literal body declared Float32.
val g = (): Float32 => 0.125
print(toString(g()))               // 0.125

// A bare literal with no Float32 context still defaults to Float64.
val d = 0.5
print(toString(d))                 // 0.5
"#);
    assert_eq!(output, vec!["0.5", "[0.5, 0.25]", "0.75", "0.125", "0.5"]);
}

#[test]
fn test_mixed_int_float_array_literal_widens_elements() {
    // A `[int, ..., float, ...]` literal unifies its element type to Float64 (the checker
    // widens via unify_types), so the array is stored in the FLAT f64 scalar repr. The
    // integer literal elements must be converted to f64 BEFORE the flat push — otherwise
    // codegen emitted `lin_flat_array_push_f64(ptr, i32 0)`, an i32 arg to an f64 push
    // ("Call parameter type does not match function signature"). Order must not matter, so
    // exercise int-first, float-first, and float-in-the-middle. Read elements back (sum and
    // direct index) to prove the int operands became the CORRECT floats, not bit garbage.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"
import { reduce } from "std/iter"

val a = [0, -17, 3.14, 1000000]
print(toString(length(a)))                 // 4
print(toString(a[0]))                       // 0.0  (int element widened to float)
print(toString(a[1]))                       // -17.0
print(toString(a[3]))                       // 1000000.0
print(toString(reduce(a, 0.0, (acc, x) => acc + x)))  // 999986.14

val b = [3.0, 1, 2]                          // float first
print(toString(reduce(b, 0.0, (acc, x) => acc + x)))  // 6.0

val c = [1, 2, 3.0]                          // float last
print(toString(reduce(c, 0.0, (acc, x) => acc + x)))  // 6.0
"#);
    assert_eq!(
        output,
        vec!["4", "0.0", "-17.0", "1000000.0", "999986.14", "6.0", "6.0"]
    );
}

#[test]
fn test_flat_array_push_grows_inline() {
    // The flat-scalar PUSH is INLINED in codegen (fast bump-append when len < cap; cold grow
    // path defers to the runtime `lin_flat_array_push_<sfx>` realloc). Exercise the grow boundary
    // hard: a flat array starts at cap 4 (a 1-element literal), then push ~40-50 elements in place
    // via a recursive accumulator-threading builder so EVERY element hits the inline path and the
    // array reallocates several times. Cover BOTH Int32 and Float64 element reprs, plus a
    // map/filter/reduce chain (whose intermediates are flat arrays grown the same way). Read the
    // contents back (length + sum) so a mis-stored element or a stale post-realloc data pointer
    // corrupts the assertion. ASan-clean (verified separately): flat scalars carry no refcount.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length, push } from "std/array"
import { range, reduce, map, filter } from "std/iter"

val buildInts = (i: Int32, n: Int32, acc: Int32[]): Int32[] =>
  if i >= n then acc
  else
    push(acc, i)
    buildInts(i + 1, n, acc)
val ints: Int32[] = buildInts(1, 50, [0])
print(toString(length(ints)))                       // 50 (grew from cap 4)
val intsB: Int32[] = buildInts(1, 50, [0])
print(toString(reduce(intsB, 0, (a, x) => a + x)))  // 0+1+...+49 = 1225

val buildFloats = (i: Int32, n: Int32, acc: Float64[]): Float64[] =>
  if i >= n then acc
  else
    push(acc, i + 0.5)
    buildFloats(i + 1, n, acc)
val floats: Float64[] = buildFloats(1, 40, [0.5])
print(toString(length(floats)))                     // 40
val floatsB: Float64[] = buildFloats(1, 40, [0.5])
print(toString(reduce(floatsB, 0.0, (a, x) => a + x)))  // 800.0

// map/filter/reduce chain over flat int arrays (each combinator pushes into a fresh flat array)
print(toString(range(0, 1000).map(x => x * 2).filter(x => x % 3 == 0).reduce(0, (a, x) => a + x)))
"#);
    assert_eq!(output, vec!["50", "1225", "40", "800.0", "333666"]);
}

#[test]
fn test_flat_array_widening_bind() {
    // Binding a flat scalar array to a slot/return of a WIDER scalar element type is a genuine
    // representation change: a `UInt8[]` stores 1-byte elements, an `Int32[]` 4-byte elements.
    // Reinterpreting the same buffer would read 4 source bytes as one i32 on every INDEXED access
    // (the whole-array toString reads the runtime elem_tag and looked correct, but `arr[0]` used the
    // static dest stride). The fix MATERIALIZES a fresh dest-strided buffer, widening each element
    // (zext/sext/sitofp/fpext) at the coercion site — so indexed reads and the whole-array view AGREE.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val bytes: UInt8[] = [10, 20, 30, 40]
val asInt: Int32[] = bytes
print("whole: ${asInt.toString()}")   // [10, 20, 30, 40]
print("idx0: ${asInt[0].toString()}") // 10 (was 673059850 — 4 bytes read as one i32)
print("idx3: ${asInt[3].toString()}") // 40

// Int32[] -> Float64[]: each element converted via sitofp at the bind, both views agree.
val ints: Int32[] = [1, 2, 3]
val flts: Float64[] = ints
print("fidx: ${flts[1].toString()}")   // 2.0
print("fwhole: ${flts.toString()}")    // [1.0, 2.0, 3.0]
"#);
    assert_eq!(
        output,
        vec!["whole: [10, 20, 30, 40]", "idx0: 10", "idx3: 40", "fidx: 2.0", "fwhole: [1.0, 2.0, 3.0]"]
    );
}

#[test]
fn test_scalar_float32_widening_return() {
    // A `Float32` value (`f32FromBe` → LLVM `float`) returned where the function declares `Float64`
    // (LLVM `double`) must be `fpext`'d at the return. Previously NO coercion was inserted on the
    // scalar return path (`type_repr_differs` only covers the union/AnyVal box boundary, not a scalar
    // numeric width change), so codegen emitted invalid LLVM (a `float` operand where the signature
    // declares `double`) and the verifier aborted. The fix checks `scalar_numeric_repr_differs` on
    // the return path too, mirroring the binding/slot store — codegen's numeric arm emits the fpext.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { f32FromBe } from "std/bytes"
val read = (b: UInt8[]): Float64 =>
  f32FromBe(b, 0)
val buf: UInt8[] = [63, 0, 0, 0]
print(read(buf).toString())  // 0.5
"#);
    assert_eq!(output, vec!["0.5"]);
}

#[test]
fn test_subint32_flat_element_widened_into_branch_phi() {
    // Regression: a sub-Int32 flat array element read inside an `if` branch must be WIDENED to the
    // PHI's declared int width. `bytes[1]` (a `UInt8[]` element) loads at its native width (i8);
    // the branch feeds an `if … then … else …` whose result is bound to `Int32`, so the merge PHI
    // is typed i32. The PHI codegen does NOT coerce its incomings, so without a widening Coerce on
    // the branch value LLVM saw `phi i32 [ %i8val, … ]` and a downstream `shl i32 %phi, 8` over an
    // i8 operand — rejected by the verifier ("Both operands to a binary operator are not of the
    // same type! %ir_shl = shl i8 %flat_get, i32 8"). Fix: `coerce_to_slot_type` now treats an
    // int↔int width change (`int_width_repr_differs`) as a representation difference and emits the
    // widening Coerce on the branch value, so both PHI incomings are i32. Unsigned source zext's
    // (UInt8 0xFF → 255), signed source sext's (Int8 -1 → -1). Cover UInt8 (zext) and Int16 (zext
    // of a value that does NOT fit in i8, proving the source width — not i8 — drives the extension).
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val bytes: UInt8[] = [72, 105]
val b1: Int32 = if true then bytes[1] else 0
val r = b1 << 8
print(toString(r))               // 105 << 8 == 26880

val ws: Int16[] = [300, 1000]
val w1: Int32 = if true then ws[1] else 0
val rw = w1 << 4
print(toString(rw))              // 1000 << 4 == 16000

// Signed sub-Int32 element: the source's signedness drives sign-extension, NOT zext.
val sb: Int8[] = [-1, -2]
val sv: Int32 = if true then sb[0] else 0
print(toString(sv))              // -1, sign-extended (not 255)
"#);
    assert_eq!(output, vec!["26880", "16000", "-1"]);
}

#[test]
fn test_empty_array_literal_arg_flat_scalar_param() {
    // Regression: an EMPTY array literal `[]` passed as an argument to a flat-scalar `T[]`
    // parameter was mis-compiled. Pure bottom-up inference of `[]` yields `Array(Never)` (no
    // elements to infer a width from), so the call site allocated a TAGGED (boxed) buffer while
    // the callee's `Int32[]`/`Float64[]` param did flat stride-N push/get → reading back garbage
    // (`s0=2 s1=0`). The fix routes array-literal args through expected-type-directed checking
    // against a concrete (TypeVar-free) array param, so the literal adopts the flat element repr.
    // A NON-empty literal arg (`[9]`) and a locally-annotated empty `val s: Int32[] = []` already
    // worked, so this guards the empty-in-argument-position case specifically. Cover Int32 and
    // Float64, push two elements into the passed-in empty array, and read them back.
    let output = run(r#"import { push, length } from "std/array"
import { print } from "std/io"
import { toString } from "std/string"

val di = (s: Int32[]): Null =>
  push(s, 3)
  push(s, 4)
  print("i s0=${toString(s[0])} s1=${toString(s[1])} len=${toString(length(s))}")
di([])

val df = (s: Float64[]): Null =>
  push(s, 3.5)
  push(s, 4.5)
  print("f s0=${toString(s[0])} s1=${toString(s[1])} len=${toString(length(s))}")
df([])
"#);
    assert_eq!(output, vec!["i s0=3 s1=4 len=2", "f s0=3.5 s1=4.5 len=2"]);
}

#[test]
fn test_empty_object_literal_arg_map_param() {
    // Regression (sibling of the array case above): an EMPTY object literal `{}` passed as an
    // argument to a typed index-signature map param `{ String: T }` (`Type::Map`) was REJECTED at
    // type-check time. infer_call's first pass typed the arg bottom-up via `infer_expr`, so `{}`
    // inferred to an empty structural `Object`, which then failed to match the concrete `Map(T)`
    // param (`Argument 1 has type {  }, expected { String: Int32 }`). A LOCAL annotated `val m:
    // { String: T } = {}` already worked, so the runtime/Map type are fine — only the empty `{}`
    // in argument position was broken. The fix routes object-literal args through expected-type-
    // directed `check_expr` against a concrete (TypeVar-free) `Map` param, so the literal adopts
    // the param's `Map(T)` representation. Insert two keys and read them back; cover String->Int32
    // and String->String value types.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val di = (m: { String: Int32 }): Null =>
  m["a"] = 3
  m["b"] = 4
  print("a=${toString(m["a"])} b=${toString(m["b"])}")
di({})

val ds = (m: { String: String }): Null =>
  m["x"] = "hi"
  m["y"] = "bye"
  print("x=${m["x"]} y=${m["y"]}")
ds({})
"#);
    assert_eq!(output, vec!["a=3 b=4", "x=hi y=bye"]);
}

#[test]
fn test_flat_array_index_set_inline() {
    // The flat-scalar index-assign (`arr[i] = x`) is INLINED in codegen when the element type is a
    // flat scalar AND the value type matches it: a bounds-checked raw store instead of boxing +
    // the cross-staticlib `lin_array_set`. OOB and negative indices must stay byte-identical to the
    // runtime: OOB is a SILENT no-op (array set never faults, spec §6.1) and `arr[-1]` addresses
    // the last element. Cover Int32 and Float64, the in-bounds store, the negative-index store, and
    // an out-of-bounds store (must not corrupt or fault). Read every slot back.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"

val a: Int32[] = [10, 20, 30, 40]
a[1] = 99            // in-bounds inline store
a[100] = 7           // OOB -> silent no-op (cold path defers to runtime set)
a[-1] = 55           // negative index -> last element
print(toString(a[0]))      // 10
print(toString(a[1]))      // 99
print(toString(a[3]))      // 55
print(toString(length(a))) // 4 (OOB store did not grow)

val f: Float64[] = [1.5, 2.5, 3.5]
f[0] = 9.25
f[-1] = 8.75         // last element via negative index
print(toString(f[0]))      // 9.25
print(toString(f[2]))      // 8.75
"#);
    assert_eq!(output, vec!["10", "99", "55", "4", "9.25", "8.75"]);
}

#[test]
fn test_float_constants_link_under_pie() {
    // Float constants land in .rodata and, with a non-PIC reloc model, emit
    // R_X86_64_32S absolute relocations that the system `cc`'s default PIE link
    // rejects ("can not be used when making a PIE object"). Codegen uses RelocMode::PIC
    // so this links. A function returning different float arrays per branch is the
    // shape that reliably surfaced it. Regression for the PIE link failure.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val pick = (k: Int32): Float64[] =>
  if k == 1 then [0.5, 1.5]
  else if k == 2 then [2.5, 3.5]
  else [0.0, 0.0]

print(toString(pick(1)[0]))
print(toString(pick(2)[1]))
print(toString(pick(9)[0]))
"#);
    assert_eq!(output, vec!["0.5", "3.5", "0.0"]);
}

#[test]
fn test_null_propagation_deep() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val x = null
print(toString(x["a"]["b"]["c"]["d"]))
val obj = { "a": { "b": null } }
print(toString(obj["a"]["b"]["c"]))
print(toString(obj["missing"]["deep"]["chain"]))
"#);
    assert_eq!(output, vec!["null", "null", "null"]);
}

#[test]
fn test_speculative_reads_typed_union() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

type MyType = { "level1": { "level2": String } | Null }

val obj1: MyType = { "level1": { "level2": "str" } }
val obj2: MyType = { }

print(obj1["level1"]["level2"])
print(toString(obj2["level1"]["level2"]))
"#);
    assert_eq!(output, vec!["str", "null"]);
}

#[test]
fn test_comments() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

// This is a comment
val x = 1 // inline comment
// Another comment
val y = 2
print(toString(x + y))
"#);
    assert_eq!(output, vec!["3"]);
}

#[test]
fn test_mixed_numeric_operations() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val x = 5 + 3.0
print(toString(x))
val y = 10.0 - 3
print(toString(y))
val z = 2 * 3.5
print(toString(z))
"#);
    assert_eq!(output, vec!["8.0", "7.0", "7.0"]);
}

#[test]
fn test_not_equal() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

print(toString(1 != 2))
print(toString(1 != 1))
print(toString("a" != "b"))
print(toString("a" != "a"))
"#);
    assert_eq!(output, vec!["true", "false", "true", "false"]);
}

#[test]
fn test_array_pattern_matching_is() {
    let output = run(r#"import { print } from "std/io"

val describe = (items: AnyVal): String =>
  match items
    is [] => "empty"
    is [one] => "one: ${one}"
    is [a, b] => "two: ${a}, ${b}"
    else => "many"

print(describe([]))
print(describe([42]))
print(describe([1, 2]))
print(describe([1, 2, 3]))
"#);
    assert_eq!(output, vec!["empty", "one: 42", "two: 1, 2", "many"]);
}

#[test]
fn test_array_pattern_matching_has() {
    let output = run(r#"import { print } from "std/io"
import { length } from "std/array"

val describe = (items: AnyVal): String =>
  match items
    has [first, ...rest] => "first: ${first}, rest length: ${length(rest)}"
    else => "empty"

print(describe([10, 20, 30]))
print(describe([42]))
"#);
    assert_eq!(output, vec!["first: 10, rest length: 2", "first: 42, rest length: 0"]);
}

#[test]
fn test_object_rest_destructuring() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val person = { "name": "Bob", "age": 42, "city": "London" }
val { name, ...rest } = person
print(name)
print(toString(rest["age"]))
print(toString(rest["city"]))
"#);
    assert_eq!(output, vec!["Bob", "42", "London"]);
}

#[test]
fn test_integer_modulo() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

print(toString(7 % 3))
print(toString(-7 % 3))
print(toString(7 % -3))
"#);
    assert_eq!(output, vec!["1", "-1", "1"]);
}

#[test]
fn test_modulo_by_zero_error() {
    let err = run_expect_err(r#"import { print } from "std/io"
import { toString } from "std/string"

val x = 10 % 0
print(toString(x))
"#);
    assert!(err.contains("modulo") || err.contains("zero") || err.contains("division"), "got: {}", err);
}

#[test]
fn test_multiple_closures_share_var() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val makePair = () =>
  var count = 0
  val inc = () =>
    count = count + 1
    count
  val dec = () =>
    count = count - 1
    count
  [inc, dec]

val pair = makePair()
val inc = pair[0]
val dec = pair[1]
print(toString(inc()))
print(toString(inc()))
print(toString(dec()))
"#);
    assert_eq!(output, vec!["1", "2", "1"]);
}

#[test]
fn test_objlit_field_closure_captures_var_escapes() {
    // Regression: a closure that captures a `var` cell and is stored into an OBJECT-LITERAL
    // FIELD, then escapes (the object is returned from the constructing fn). The object's
    // tagged-payload retain did not handle TAG_FUNCTION, so the constructing frame's
    // `lin_closure_release` freed the closure (and its captured-var cell) while the escaping
    // object still held it — a use-after-free (SIGSEGV). The bare-return and array-element
    // forms (see test_multiple_closures_share_var) already worked; only the object field was
    // broken. Must increment correctly across calls (1, then 2).
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val mk = (): AnyVal =>
  var n = 0
  { "inc": (): Int32 =>
             n = n + 1
             n }

val c = mk()
print(toString(c["inc"]()))
print(toString(c["inc"]()))
"#);
    assert_eq!(output, vec!["1", "2"]);
}

#[test]
fn test_nested_function_calls() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val double = (x: Int32): Int32 => x * 2
val addOne = (x: Int32): Int32 => x + 1
print(toString(addOne(double(5))))
"#);
    assert_eq!(output, vec!["11"]);
}

#[test]
fn test_recursive_fibonacci() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val fib = (n: Int32): Int32 =>
  if n <= 1 then n else fib(n - 1) + fib(n - 2)

print(toString(fib(0)))
print(toString(fib(1)))
print(toString(fib(10)))
"#);
    assert_eq!(output, vec!["0", "1", "55"]);
}

#[test]
fn test_string_interpolation_concat() {
    let output = run(r#"import { print } from "std/io"

val a = "Hello"
val b = "World"
val greeting = "${a} ${b}"
print(greeting)
"#);
    assert_eq!(output, vec!["Hello World"]);
}

#[test]
fn test_object_equality_deep() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val a = { "x": { "y": [1, 2] } }
val b = { "x": { "y": [1, 2] } }
val c = { "x": { "y": [1, 3] } }
print(toString(a == b))
print(toString(a == c))
"#);
    assert_eq!(output, vec!["true", "false"]);
}

#[test]
fn test_interp_with_expressions() {
    let output = run(r#"import { print } from "std/io"

val x = 10
val y = 20
print("sum = ${x + y}")
print("cond = ${if x > 5 then "big" else "small"}")
"#);
    assert_eq!(output, vec!["sum = 30", "cond = big"]);
}

#[test]
fn test_length_function() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"

print(toString(length("hello")))
print(toString(length([1, 2, 3])))
print(toString(length({ "a": 1, "b": 2 })))
"#);
    assert_eq!(output, vec!["5", "3", "2"]);
}

#[test]
fn test_multiline_chain() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map, filter, reduce } from "std/iter"

val nums = [1, 2, 3, 4, 5, 6]
val result = nums
  .filter(x => x % 2 == 0)
  .map(x => x * 10)
  .reduce(0, (sum, x) => sum + x)
print(toString(result))
"#);
    assert_eq!(output, vec!["120"]);
}

#[test]
fn test_val_bound_multiline_chain_in_fn_body() {
    // Regression: a `val`-bound multi-line dot-chain INSIDE a function body used to
    // misparse. The `.map` continuation line is indented deeper than the `val`, so the
    // lexer emitted an INDENT that the postfix loop consumed to continue the chain,
    // leaving the enclosing inline-block's INDENT/DEDENT accounting unbalanced — the
    // `val ys` and trailing `ys` were misattributed (→ "Undefined variable 'ys'").
    // Fix: the lexer suppresses INDENT/DEDENT for a line beginning with `.method`,
    // mirroring its `&&`/`||` continuation handling. (block/dot-chain indent-balance bug)
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map, filter } from "std/iter"

val f = (xs: AnyVal): AnyVal =>
  val ys = xs
    .map(x => x + 1)
    .filter(x => x > 2)
  ys
print(toString(f([1, 2, 3])))
"#);
    assert_eq!(output, vec!["[3, 4]"]);
}

#[test]
fn test_match_with_block_body() {
    let output = run(r#"import { print } from "std/io"

val describe = (x: AnyVal): String =>
  match x
    is Int32 =>
      val doubled = x * 2
      "int doubled: ${doubled}"
    is String => "str: ${x}"
    else => "other"

print(describe(5))
print(describe("hi"))
"#);
    assert_eq!(output, vec!["int doubled: 10", "str: hi"]);
}

#[test]
fn test_partial_application_chain() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val add3 = (a: Int32, b: Int32, c: Int32): Int32 => a + b + c
val step1 = add3(1,)
val step2 = step1(2,)
val result = step2(3)
print(toString(result))
"#);
    assert_eq!(output, vec!["6"]);
}

#[test]
fn test_default_args_runtime_fill() {
    // Consolidated default-argument runtime behaviours (4 former one-build tests → one program,
    // distinct function names, every assertion preserved in order). The compile-error cases
    // (`too_few_is_error`, `required_after_optional_is_error`) and the file-writing
    // `cross_module` case keep their own tests below — they need `run_expect_err` / fixtures.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

// basic: omitting a trailing optional argument fills it from its default.
val greet = (name: String, greeting: String = "Hello") => "${greeting}, ${name}"
print(greet("World"))
print(greet("World", "Hi"))

// chained: a default may reference earlier parameters, including earlier defaults.
val box = (w: Int32, h: Int32 = w, area: Int32 = w * h) => area
print(toString(box(4)))
print(toString(box(4, 3)))
print(toString(box(4, 3, 99)))

// object: an object-typed default literal.
val config = (name: String, opts: AnyVal = { "v": false }) => "${name}:${opts}"
print(config("a"))
print(config("b", { "v": true }))

// indirect_value: default-fill works when the function is held as a first-class value
// (the closure carries a descriptor so the indirect call fills defaults).
val scale = (x: Int32, factor: Int32 = 2) => x * factor
val g = scale
print(toString(g(5)))
print(toString(g(5, 3)))
"#);
    assert_eq!(
        output,
        vec![
            "Hello, World",        // basic
            "Hi, World",           // basic (explicit)
            "16",                  // chained box(4)
            "12",                  // chained box(4, 3)
            "99",                  // chained box(4, 3, 99)
            "a:{\"v\": false}",    // object (default)
            "b:{\"v\": true}",     // object (explicit)
            "10",                  // indirect g(5)
            "15",                  // indirect g(5, 3)
        ]
    );
}

#[test]
fn test_default_args_cross_module() {
    // An imported function's defaults are filled by an adapter emitted in the
    // defining module and called by symbol from the importer.
    let dir = std::env::temp_dir().join(format!("lin_da_xmod_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("lib.lin"),
        "export val scale = (x: Int32, factor: Int32 = 2) => x * factor\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ scale }} from "{}/lib"
print(toString(scale(5)))
print(toString(scale(5, 3)))
"#, dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["10", "15"]);
}

#[test]
fn test_default_args_cross_module_nullable_record() {
    // Regression: an imported function with a `T | Null = null` default on a sealed-record param
    // failed to link — `$defaultN` wrapper was not emitted because `is_union_ty` returns false for
    // NullableRecord (a raw nullable ptr repr), causing `default_cannot_inhabit_param` to wrongly
    // bail. The fix adds `!is_nullable_sealed_record(param_ty)` to the gate so the wrapper IS emitted.
    let dir = std::env::temp_dir().join(format!("lin_da_nr_xmod_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("lib.lin"),
        "export type Foo = { \"x\": Int32 }\n\
         export val g = (a: Int32, b: Int32, c: Int32, d: Foo | Null = null): Int32 =>\n\
        \x20 if d == null then a + b + c else d[\"x\"]\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ g }} from "{}/lib"
print(toString(g(1, 2, 3)))
print(toString(g(1, 2, 3, {{ "x": 99 }})))
"#, dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["6", "99"]);
}

#[test]
fn test_imported_generic_object_message_across_worker() {
    // Regression: an IMPORTED generic function that builds an object literal with a scalar `T`
    // field and sends it to a worker (`message`/`request`, which deep-copy the value for thread
    // transfer) crashed — `lin_worker_message`'s argument was passed as the RAW `LinObject*` instead
    // of a boxed `TaggedVal*`, because the codegen boxed on `is_pointer_value()` (true for a heap
    // object) rather than the static type. The worker thread then read the object's first bytes as a
    // TaggedVal tag → misaligned-pointer deref. The same code defined INLINE worked (it monomorphized
    // in-module); only the cross-module instantiation tripped it. Fix: box `message`/`request`
    // (and `shared`/`set`) arguments on the static type — a concrete heap value is boxed even though
    // it is pointer-shaped; only an already-boxed `is_union_type` value passes through.
    let dir = std::env::temp_dir().join(format!("lin_genmsg_xmod_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("emit.lin"),
        "import { worker, message, request } from \"std/async\"\n\
         type Msg<T> = { \"kind\": String, \"value\": T }\n\
         export val mkSink = <T, S>(reduce: (T, S) => S, initial: S, sample: T): AnyVal =>\n\
        \x20 var state = initial\n\
        \x20 worker(\n\
        \x20   (m: Msg<T>): S =>\n\
        \x20     match m[\"kind\"]\n\
        \x20       is \"drain\" => state\n\
        \x20       else =>\n\
        \x20         state = reduce(m[\"value\"], state)\n\
        \x20         state,\n\
        \x20   (): Null => null\n\
        \x20 )\n\
         export val send = <T>(e: AnyVal, value: T): Null =>\n\
        \x20 message(e, { \"kind\": \"event\", \"value\": value })\n\
         export val drainSink = <T, S>(e: AnyVal, sample: T): S | Error =>\n\
        \x20 request(e, { \"kind\": \"drain\", \"value\": sample })\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ close }} from "std/async"
import {{ mkSink, send, drainSink }} from "{}/emit"
val main = (): Null =>
  val w = mkSink((x: Int32, sum: Int32): Int32 => sum + x, 0, 0)
  send(w, 10)
  send(w, 5)
  val total: Int32 | Error = drainSink(w, 0)
  close(w)
  match total
    is Error => print("err")
    else => print("total=${{toString(total)}}")
main()
"#, dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["total=15"]);
}

#[test]
fn test_imported_fn_uses_module_level_val() {
    // Regression: a top-level non-function `val` referenced inside an EXPORTED function
    // mis-lowered in the import path (lower_import_module never registered the val, so the
    // reference resolved to an unmaterialised temp → codegen panic "undefined rhs temp").
    // Covers: float val, string val, a val referencing another val, and a val used in
    // multiple exported functions — all read through their `__val` wrappers.
    let dir = std::env::temp_dir().join(format!("lin_modval_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("lib.lin"),
        "val K = 0.1\n\
         val GREETING = \"Hi, \"\n\
         val BASE = 10\n\
         val DOUBLE = BASE * 2\n\
         export val f = (x: Float64): Float64 =>\n  \
           if x == 1.0 then x + K\n  \
           else x\n\
         export val greet = (name: String): String => \"${GREETING}${name}\"\n\
         export val addBase = (x: Int32): Int32 => x + BASE\n\
         export val addDouble = (x: Int32): Int32 => x + DOUBLE\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ f, greet, addBase, addDouble }} from "{}/lib"
print(toString(f(1.0)))
print(greet("World"))
print(toString(addBase(5)))
print(toString(addDouble(5)))
"#, dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["1.1", "Hi, World", "15", "25"]);
}

#[test]
fn test_imported_fn_passed_as_value() {
    // Regression: an imported top-level function referenced as a VALUE (not called) was
    // dropped in IR lowering — the LocalGet branch had no `import_fn_slots` case, so the
    // slot fell through to a placeholder that emitted no instruction and codegen silently
    // dropped the argument ("Incorrect number of arguments passed to called function!").
    // Both forms below pass an imported fn as a value: as a higher-order arg to `map`, and
    // bound to a local `val` then called. (A local fn used the same way always worked.)
    let dir = std::env::temp_dir().join(format!("lin_impfnval_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("lib.lin"),
        "export val double = (x: Int32): Int32 => x * 2\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ map }} from "std/iter"
import {{ double }} from "{}/lib"
val doubled = [1, 2, 3].map(double)
print(toString(doubled))
val f = double
print(toString(f(21)))
"#, dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["[2, 4, 6]", "42"]);
}

#[test]
fn test_imported_type_used_in_annotation() {
    // An exported `type` decl can be imported and used in type position in a dependent
    // module — covering a plain object type, an aliased import (`as`), and a generic type.
    // Previously these failed with "Unknown type" because exported type decls were dropped
    // at the module boundary (only value exports were threaded into the importer's checker).
    let dir = std::env::temp_dir().join(format!("lin_imptype_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("lib.lin"),
        "export type Point = { \"x\": Int32, \"y\": Int32 }\n\
         export type Wrapped<T> = { \"value\": T }\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ Point, Wrapped as W }} from "{}/lib"
val sum = (p: Point): Int32 => p["x"] + p["y"]
val unwrap = (w: W<Int32>): Int32 => w["value"]
print(toString(sum({{ "x": 3, "y": 4 }})))
print(toString(unwrap({{ "value": 99 }})))
"#, dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["7", "99"]);
}

#[test]
fn test_imported_type_with_forward_referenced_field_type() {
    // Regression: an exported type alias whose body references a SIBLING type declared LATER in
    // the same file must be importable WITHOUT also importing that sibling. The importer only uses
    // the alias; the field type is an internal detail of the alias's definition.
    //
    // Previously this failed with "Unknown type 'Trip'": type-decl bodies were resolved in
    // (hoisted) source order, so `TimetableLeg` (declared before `Trip`) collapsed its `Trip`
    // field to a bare `Named("Trip")` via the cycle guard — the sibling was still a placeholder.
    // That unexpanded forward reference leaked into the module signature and then failed to resolve
    // in the importer, where `Trip` is not in scope. The export-collection pass now re-resolves
    // bodies against the fully-populated env, expanding such forward references inline.
    let dir = std::env::temp_dir().join(format!("lin_imptype_fwd_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    // `TimetableLeg` references `Trip`, declared AFTER it — mirrors gtfs.lin's ordering.
    std::fs::write(dir.join("gtfs.lin"),
        "export type Leg = { \"origin\": String }\n\
         export type TimetableLeg = Leg & { \"trip\": Trip }\n\
         export type Trip = { \"tripId\": String }\n").unwrap();
    // The importer pulls in ONLY `TimetableLeg`, never `Trip`.
    let main = format!(r#"import {{ print }} from "std/io"
import {{ TimetableLeg }} from "{}/gtfs"
val leg: TimetableLeg = {{ "origin": "NRW", "trip": {{ "tripId": "t1" }} }}
print(leg["trip"]["tripId"])
"#, dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["t1"]);
}

#[test]
fn test_imported_type_unknown_without_import() {
    // The type is only visible when imported: using `Point` without importing it from the
    // module that exports it is still "Unknown type" (the registration is scoped to imports).
    let dir = std::env::temp_dir().join(format!("lin_imptype_neg_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("lib.lin"),
        "export type Point = { \"x\": Int32, \"y\": Int32 }\n").unwrap();
    // Import a VALUE-less binding-free module reference: import nothing type-related, then
    // reference Point. (We import a dummy to make the module a dependency at all.)
    let main = format!(r#"import {{ print }} from "std/io"
val sum = (p: Point): Int32 => p["x"]
print("unused")
"#);
    let _ = &dir; // lib not imported on purpose
    let err = run_expect_err(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(err.contains("Unknown type 'Point'"), "got: {}", err);
}

#[test]
fn test_circular_import_function_reference_compiles_not_stack_overflow() {
    // Originally this was a hard error (and earlier a stack overflow): a cyclic import
    // (a -> b -> a). Cyclic FUNCTION references are now supported (SCC type-checking), so this
    // program compiles and runs — `a.fromA` and `b.fromB` reference each other across the import
    // boundary, and `a`'s top-level `val x = fromB()` calls into the cycle once at init (which
    // terminates). The stack-overflow regression is still guarded: resolution loads the graph once
    // and never recurses forever; a genuine non-terminating *value* cycle is rejected cleanly
    // (see test_cyclic_imports_value_init_cycle_still_errors).
    let dir = std::env::temp_dir().join(format!("lin_import_cycle_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("a.lin"),
        "import { fromB } from \"b\"\n\
         export val fromA = (): Int32 => 1\n\
         val x = fromB()\n").unwrap();
    std::fs::write(dir.join("b.lin"),
        "import { fromA } from \"a\"\n\
         export val fromB = (): Int32 => fromA()\n").unwrap();

    let bin_path = dir.join("a.out");
    let compile = lin_cmd()
        .args(["build", dir.join("a.lin").to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");

    let stderr = String::from_utf8_lossy(&compile.stderr).to_string();
    let stdout = String::from_utf8_lossy(&compile.stdout).to_string();
    let combined = format!("{stderr}{stdout}");

    assert!(compile.status.success(),
        "expected the function-reference cycle to compile, got: {combined}");

    // It must also run cleanly (terminate, exit 0) — not crash.
    let run_out = Command::new(&bin_path).output().expect("failed to run compiled binary");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(run_out.status.success(),
        "expected clean run, got stderr: {}", String::from_utf8_lossy(&run_out.stderr));
}

#[test]
fn test_diamond_imports_are_not_false_cycles() {
    // A module imported by two different paths (a diamond) is NOT a cycle. Resolution
    // pops each module from the visiting stack when done, so the shared dependency is
    // reached twice without being flagged.
    let dir = std::env::temp_dir().join(format!("lin_import_diamond_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("shared.lin"),
        "export val base = (): Int32 => 10\n").unwrap();
    std::fs::write(dir.join("left.lin"),
        "import { base } from \"shared\"\n\
         export val viaLeft = (): Int32 => base() + 1\n").unwrap();
    std::fs::write(dir.join("right.lin"),
        "import { base } from \"shared\"\n\
         export val viaRight = (): Int32 => base() + 2\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ viaLeft }} from "{d}/left"
import {{ viaRight }} from "{d}/right"
print(toString(viaLeft() + viaRight()))
"#, d = dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["23"]);
}

#[test]
fn test_cyclic_imports_mutual_recursion_unannotated() {
    // THE no-userland-change case: two modules whose functions are mutually recursive
    // across the import boundary, with NO return-type annotations. `a.isEven` calls
    // `b.isOdd` and vice-versa. This must compile and run as written (SCC type-checking).
    let dir = std::env::temp_dir().join(format!("lin_cyc_mutrec_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("a.lin"),
        "import { isOdd } from \"b\"\n\
         export val isEven = (n: Int32) => if n == 0 then true else isOdd(n - 1)\n").unwrap();
    std::fs::write(dir.join("b.lin"),
        "import { isEven } from \"a\"\n\
         export val isOdd = (n: Int32) => if n == 0 then false else isEven(n - 1)\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ isEven }} from "{d}/a"
print(toString(isEven(10)))
print(toString(isEven(7)))
"#, d = dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["true", "false"]);
}

#[test]
fn test_cyclic_imports_value_init_cycle_still_errors() {
    // A genuine value-init cycle: a.x reads b.y at module-init time and b.y reads a.x.
    // This is infinite init recursion and must remain a clean compile error (not a hang
    // or stack overflow), even though function-reference cycles are now allowed.
    let dir = std::env::temp_dir().join(format!("lin_cyc_valinit_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("a.lin"),
        "import { y } from \"b\"\n\
         export val x = y + 1\n").unwrap();
    std::fs::write(dir.join("b.lin"),
        "import { x } from \"a\"\n\
         export val y = x + 1\n").unwrap();

    let bin_path = dir.join("a.out");
    let compile = lin_cmd()
        .args(["build", dir.join("a.lin").to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    let stderr = String::from_utf8_lossy(&compile.stderr).to_string();
    let stdout = String::from_utf8_lossy(&compile.stdout).to_string();
    let combined = format!("{stderr}{stdout}");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(!compile.status.success(), "expected failure, got success: {combined}");
    assert!(
        combined.contains("circular") || combined.contains("cycle") || combined.contains("init"),
        "expected a value-init cycle diagnostic, got: {combined}"
    );
}

#[test]
fn test_cyclic_imports_exported_type_alias_across_cycle() {
    // ADR-083: an exported `type` alias defined in one cycle member, imported and used in TYPE
    // position by another member of the same SCC, must resolve (not "Unknown type 'T'"). `a`
    // defines `type T` and imports the VALUE `useT` from `b`; `b` imports the TYPE `T` from `a`
    // and annotates a parameter with it. Both must check, and the record must flow across the
    // boundary with a consistent representation (sealed record, not a boxed map) so `useT` reads
    // the right value back.
    let dir = std::env::temp_dir().join(format!("lin_cyc_typealias_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("a.lin"),
        "import { useT } from \"b\"\n\
         export type T = { \"x\": Int32 }\n\
         export val g = (): Int32 => useT({ \"x\": 5 })\n").unwrap();
    std::fs::write(dir.join("b.lin"),
        "import { T } from \"a\"\n\
         export val useT = (t: T): Int32 => t[\"x\"]\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ g }} from "{d}/a"
print(toString(g()))
"#, d = dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["5"]);
}

#[test]
fn test_cyclic_imports_exported_map_alias_indexed_across_cycle() {
    // ADR-084 follow-up: an exported MAP alias (`type ST = { String: UInt32 }`) defined in one SCC
    // member, imported by a peer and used as a PARAMETER type that the peer then INDEXES. Before the
    // type-alias seeding became a fixpoint that tolerated unresolved member bodies, the first sweep
    // checked the peer with `ST` still a placeholder TypeVar, so `src[k]` lost its value type and
    // nullability — `src[k] ?? d` errored "left operand of `??` is never null (its type is ?Tn)" and
    // that body error aborted the whole SCC before `ST`'s alias was ever harvested. Now `ST` resolves
    // to its real map definition, so indexing yields `UInt32 | Null` and the `??` is legal.
    let dir = std::env::temp_dir().join(format!("lin_cyc_mapalias_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("a.lin"),
        "import { helper } from \"b\"\n\
         export type ST = { String: UInt32 }\n\
         export val go = (): UInt32 =>\n  \
           val m: ST = { \"x\": 1 }\n  \
           helper(m, \"x\")\n").unwrap();
    std::fs::write(dir.join("b.lin"),
        "import { ST } from \"a\"\n\
         export val helper = (src: ST, k: String): UInt32 =>\n  \
           src[k] ?? 99\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ go }} from "{d}/a"
print(toString(go()))
"#, d = dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["1"]);
}

#[test]
fn test_cyclic_imports_map_keyed_by_imported_alias_across_cycle() {
    // Regression: a MAP type whose KEY is a type alias IMPORTED from a module OUTSIDE the import
    // cycle. `a` <-> `b` form an SCC; `c` (which exports `type Sid = String`) sits outside it.
    // `a` defines `type M = { Sid: UInt32 }` — a String-keyed map, because `Sid` is an alias of
    // String. The SCC's type-alias harvest (sweep A in `check_scc`) used to resolve `M`'s body with
    // only its SCC-peer aliases in scope, so the imported key alias `Sid` fell back to a placeholder
    // TypeVar; the index-signature arm then could not prove a String key and `M` degraded to a
    // fixed-shape RECORD with a literal field named "Sid". The peer `b`, indexing `m[k]` dynamically,
    // was rejected with "`M` is a fixed-shape record and cannot be indexed dynamically". The harvest
    // now also seeds each SCC member's ACYCLIC imported type aliases, so `Sid` resolves to String and
    // `M` is correctly a `{ String: UInt32 }` map — `m[k]` is legal and an empty map yields `?? 99`.
    let dir = std::env::temp_dir().join(format!("lin_cyc_impkey_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("c.lin"), "export type Sid = String\n").unwrap();
    std::fs::write(dir.join("a.lin"),
        "import { useM } from \"b\"\n\
         import { Sid } from \"c\"\n\
         export type M = { Sid: UInt32 }\n\
         val seed: M = {}\n\
         export val go = (): UInt32 => useM(seed, \"x\")\n").unwrap();
    std::fs::write(dir.join("b.lin"),
        "import { M } from \"a\"\n\
         export val useM = (m: M, k: String): UInt32 => m[k] ?? 99\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ go }} from "{d}/a"
print(toString(go()))
"#, d = dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["99"]);
}

#[test]
fn test_cyclic_imports_exported_type_alias_three_module_cycle() {
    // ADR-083, 3-module SCC A -> B -> C -> A: a `type P` defined in A is imported and used in
    // TYPE position by C (two hops away around the cycle). The cross-cycle type import must
    // resolve and the value flows back through B's pass-through call.
    let dir = std::env::temp_dir().join(format!("lin_cyc_type3_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("A.lin"),
        "import { fromB } from \"B\"\n\
         export type P = { \"v\": Int32 }\n\
         export val a = (): Int32 => fromB()\n").unwrap();
    std::fs::write(dir.join("B.lin"),
        "import { fromC } from \"C\"\n\
         export val fromB = (): Int32 => fromC()\n").unwrap();
    std::fs::write(dir.join("C.lin"),
        "import { P } from \"A\"\n\
         export val fromC = (): Int32 => useP({ \"v\": 7 })\n\
         export val useP = (p: P): Int32 => p[\"v\"]\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ a }} from "{d}/A"
print(toString(a()))
"#, d = dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["7"]);
}

#[test]
fn test_cyclic_imports_mutually_referencing_type_aliases() {
    // ADR-083: two type aliases that reference each other ACROSS the cycle — `Outer` (in m1)
    // wraps `Inner` (in m2), and `Inner`'s defining module imports `Outer`. Both modules export
    // a type the other imports in type position; both must resolve and the nested record must
    // round-trip.
    let dir = std::env::temp_dir().join(format!("lin_cyc_mutual_types_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("m1.lin"),
        "import { Inner } from \"m2\"\n\
         export type Outer = { \"inner\": Inner }\n\
         export val mk = (): Outer => { \"inner\": { \"n\": 9 } }\n").unwrap();
    std::fs::write(dir.join("m2.lin"),
        "import { Outer } from \"m1\"\n\
         export type Inner = { \"n\": Int32 }\n\
         export val readN = (o: Outer): Int32 => o[\"inner\"][\"n\"]\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ mk }} from "{d}/m1"
import {{ readN }} from "{d}/m2"
print(toString(readN(mk())))
"#, d = dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["9"]);
}

#[test]
fn test_acyclic_undefined_type_still_errors() {
    // Guard: the cross-cycle type-alias seeding (ADR-083) must NOT mask a genuinely undefined
    // type in an ordinary acyclic module — that still has to be a clean "Unknown type" error.
    let dir = std::env::temp_dir().join(format!("lin_acyc_undeftype_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let only = dir.join("only.lin");
    std::fs::write(&only,
        "export val f = (x: Nonexistent): Int32 => 1\n").unwrap();
    let out = lin_cmd()
        .args(["check", only.to_str().unwrap()])
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(!out.status.success(), "expected a check failure, got success: {combined}");
    assert!(
        combined.contains("Unknown type 'Nonexistent'"),
        "expected 'Unknown type' diagnostic, got: {combined}"
    );
}

#[test]
fn test_missing_stdlib_import_gives_module_not_found_with_suggestion() {
    // A doubled `std/` typo (`std/std/stream`) is not an embedded stdlib module and must not
    // fall through to a raw io error. We want an actionable "module not found" diagnostic that
    // names the import, the path we tried, and a did-you-mean for the real module `std/stream`.
    let dir = std::env::temp_dir().join(format!("lin_missing_std_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(
        dir.join("main.lin"),
        "import { collect } from \"std/std/stream\"\nprint(\"hi\")\n",
    )
    .unwrap();

    let bin_path = dir.join("a.out");
    let compile = lin_cmd()
        .args(["build", dir.join("main.lin").to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    let stderr = String::from_utf8_lossy(&compile.stderr).to_string();
    let stdout = String::from_utf8_lossy(&compile.stdout).to_string();
    let combined = format!("{stderr}{stdout}");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(!compile.status.success(), "expected failure, got success: {combined}");
    assert!(combined.contains("module not found"), "expected 'module not found', got: {combined}");
    assert!(combined.contains("std/std/stream"), "expected the import path, got: {combined}");
    assert!(
        combined.contains("not a built-in stdlib module"),
        "expected the stdlib note, got: {combined}"
    );
    assert!(
        combined.contains("did you mean \"std/stream\""),
        "expected the did-you-mean suggestion 'std/stream', got: {combined}"
    );
}

#[test]
fn test_missing_relative_import_gives_module_not_found_with_tried_path() {
    // A missing relative import should also produce a "module not found" with the path we tried,
    // rather than a raw io error. No stdlib note / suggestion for non-`std/` imports.
    let dir = std::env::temp_dir().join(format!("lin_missing_rel_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(
        dir.join("main.lin"),
        "import { x } from \"./nope\"\nprint(\"hi\")\n",
    )
    .unwrap();

    let bin_path = dir.join("a.out");
    let compile = lin_cmd()
        .args(["build", dir.join("main.lin").to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    let stderr = String::from_utf8_lossy(&compile.stderr).to_string();
    let stdout = String::from_utf8_lossy(&compile.stdout).to_string();
    let combined = format!("{stderr}{stdout}");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(!compile.status.success(), "expected failure, got success: {combined}");
    assert!(combined.contains("module not found"), "expected 'module not found', got: {combined}");
    assert!(combined.contains("./nope"), "expected the import path, got: {combined}");
    assert!(combined.contains("nope.lin"), "expected the tried path, got: {combined}");
    assert!(
        !combined.contains("not a built-in stdlib module"),
        "non-std import should not get the stdlib note, got: {combined}"
    );
}

#[test]
fn test_import_unknown_export_is_compile_error_with_cross_module_hint() {
    // `std/stream` exists and is resolved, but does NOT export `gunzip` (that lives in
    // `std/compress`). The checker must reject this at TYPE-CHECK time with an actionable
    // cross-module suggestion — NOT let it slip through to a mangled link-time
    // `undefined reference to std_stream_gunzip__val`.
    let dir = std::env::temp_dir().join(format!("lin_bad_export_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(
        dir.join("main.lin"),
        "import { print } from \"std/io\"\nimport { gunzip } from \"std/stream\"\nprint(\"hi\")\n",
    )
    .unwrap();

    let bin_path = dir.join("a.out");
    let compile = lin_cmd()
        .args(["build", dir.join("main.lin").to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    let stderr = String::from_utf8_lossy(&compile.stderr).to_string();
    let stdout = String::from_utf8_lossy(&compile.stdout).to_string();
    let combined = format!("{stderr}{stdout}");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(!compile.status.success(), "expected failure, got success: {combined}");
    assert!(combined.contains("has no export"), "expected 'has no export', got: {combined}");
    assert!(combined.contains("gunzip"), "expected the export name, got: {combined}");
    assert!(
        combined.contains("exported by \"std/compress\""),
        "expected the cross-module hint to std/compress, got: {combined}"
    );
    // Crucially: caught BEFORE the linker, so no mangled-symbol jargon should appear.
    assert!(
        !combined.contains("undefined reference"),
        "should be caught at type-check, not link, got: {combined}"
    );
}

#[test]
fn test_import_typo_export_suggests_within_module() {
    // `readStrea` is a typo of `std/stream`'s real export `readStream`. No OTHER module exports
    // `readStrea`, so the diagnostic falls back to a within-module did-you-mean.
    let dir = std::env::temp_dir().join(format!("lin_typo_export_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(
        dir.join("main.lin"),
        "import { print } from \"std/io\"\nimport { readStrea } from \"std/stream\"\nprint(\"hi\")\n",
    )
    .unwrap();

    let bin_path = dir.join("a.out");
    let compile = lin_cmd()
        .args(["build", dir.join("main.lin").to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    let stderr = String::from_utf8_lossy(&compile.stderr).to_string();
    let stdout = String::from_utf8_lossy(&compile.stdout).to_string();
    let combined = format!("{stderr}{stdout}");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(!compile.status.success(), "expected failure, got success: {combined}");
    assert!(combined.contains("has no export"), "expected 'has no export', got: {combined}");
    assert!(
        combined.contains("did you mean `readStream`?"),
        "expected the within-module did-you-mean, got: {combined}"
    );
}

#[test]
fn test_missing_foreign_library_gives_jargon_free_build_error() {
    // A foreign import of a library file that does not exist must fail with a clean, user-facing
    // message that NAMES the missing library and contains NO linker/`ld:` jargon.
    let dir = std::env::temp_dir().join(format!("lin_missing_foreign_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(
        dir.join("main.lin"),
        concat!(
            "import { print } from \"std/io\"\n",
            "import foreign \"./libnope_does_not_exist.so\"\n",
            "  val nope: (Int32) => Int32\n",
            "print(nope(1))\n",
        ),
    )
    .unwrap();

    let bin_path = dir.join("a.out");
    let compile = lin_cmd()
        .args(["build", dir.join("main.lin").to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    let stderr = String::from_utf8_lossy(&compile.stderr).to_string();
    let stdout = String::from_utf8_lossy(&compile.stdout).to_string();
    let combined = format!("{stderr}{stdout}");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(!compile.status.success(), "expected failure, got success: {combined}");
    let lowered = combined.to_lowercase();
    assert!(
        !lowered.contains("linker") && !lowered.contains("ld:") && !lowered.contains("collect2"),
        "build error must be free of linker jargon, got: {combined}"
    );
    assert!(
        combined.contains("could not build your program"),
        "expected the jargon-free top line, got: {combined}"
    );
}

#[test]
fn test_cache_invalidated_when_import_signature_changes() {
    // Regression for the stale-cache bug: a CACHED imported module's cache key MUST incorporate the
    // signatures of the modules IT imports, not just its own source bytes.
    //
    // Three modules: main.lin -> m.lin -> a.lin. `m.lin` is the cached intermediary — it imports
    // `getVal` from `a.lin` and uses the result as an Int32 (`getVal() + 1`). The main module is
    // always re-checked, so the bug only manifests through an imported (cacheable) module like m.lin.
    //   - Build #1: `a.lin` exports `getVal(): Int32`. m.lin checks clean and its `.typed` is cached.
    //   - Build #2: `a.lin`'s `getVal` is changed to return `String` (its SIGNATURE changes), while
    //     m.lin AND main.lin are left BYTE-IDENTICAL.
    //
    // With the old key (sha256 of m.lin's own source only), m.lin's `.typed` from build #1 — checked
    // against the OLD Int32 `getVal` — is reloaded unchanged. `getVal() + 1` is never re-checked, so
    // codegen lowers it as integer arithmetic over a value that is now a String pointer: a silent
    // miscompilation (on current master this surfaces as a codegen panic). With the fix, m.lin's key
    // folds in a.lin's NEW signature hash, so m.lin is re-checked against the String `getVal` and a
    // clean type error surfaces. This test FAILS on master (build #2 panics / wrongly succeeds) and
    // passes after the fix (build #2 fails with a clean type error).
    let dir = std::env::temp_dir().join(format!("lin_cache_import_sig_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);

    // a.lin v1: getVal returns Int32.
    std::fs::write(dir.join("a.lin"),
        "export val getVal = (): Int32 => 42\n").unwrap();
    // m.lin (the cached intermediary): uses getVal() as an Int32. Written once and NEVER changed.
    let m_src = "import { getVal } from \"a\"\nexport val total = (): Int32 => getVal() + 1\n";
    std::fs::write(dir.join("m.lin"), m_src).unwrap();
    // main.lin: drives m.lin's `total`. Written once and NEVER changed.
    let main_src = "import { total } from \"m\"\n\
         import { print } from \"std/io\"\n\
         import { toString } from \"std/string\"\n\
         print(toString(total()))\n";
    std::fs::write(dir.join("main.lin"), main_src).unwrap();

    let bin_path = dir.join("main.out");

    // Build #1: must succeed and populate .lin-cache (m.lin checked against Int32 getVal).
    let build1 = lin_cmd()
        .args(["build", dir.join("main.lin").to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    assert!(build1.status.success(),
        "build #1 should succeed, got:\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&build1.stderr),
        String::from_utf8_lossy(&build1.stdout));
    assert!(dir.join(".lin-cache").exists(), "build #1 should have written a .lin-cache");

    // Change a.lin's EXPORTED SIGNATURE: getVal now returns String. m.lin / main.lin are untouched.
    std::fs::write(dir.join("a.lin"),
        "export val getVal = (): String => \"hi\"\n").unwrap();
    // Sanity: the cached intermediary and entry point are still byte-identical.
    assert_eq!(std::fs::read_to_string(dir.join("m.lin")).unwrap(), m_src);
    assert_eq!(std::fs::read_to_string(dir.join("main.lin")).unwrap(), main_src);

    // Build #2: m.lin must be re-checked against the NEW (String) getVal, so `getVal() + 1` is now a
    // type error. If the cache key ignored a.lin's signature, the stale m.lin .typed would be reused
    // (codegen panic / silent miscompile).
    let build2 = lin_cmd()
        .args(["build", dir.join("main.lin").to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .output()
        .expect("failed to invoke lin binary");
    let stderr = String::from_utf8_lossy(&build2.stderr).to_string();
    let stdout = String::from_utf8_lossy(&build2.stdout).to_string();
    let combined = format!("{stderr}{stdout}");
    let _ = std::fs::remove_dir_all(&dir);

    assert!(!build2.status.success(),
        "build #2 should FAIL: m.lin must be re-checked against the changed `getVal` signature \
         (String, not Int32), making `getVal() + 1` a type error. A success means the stale \
         (import-signature-blind) cache entry was reused. Output:\n{combined}");
    // It must fail CLEANLY (a type/check error), not panic in codegen on the stale IR.
    assert!(!combined.contains("panicked"),
        "build #2 should fail with a clean type error, not a codegen panic on stale cached IR:\n{combined}");
    assert!(combined.contains("String") || combined.contains("Int32") || combined.to_lowercase().contains("type"),
        "expected a type error mentioning the String/Int32 mismatch, got:\n{combined}");
}

#[test]
fn test_cyclic_imports_peer_dependent_return_boundary_gap() {
    // Documents the KNOWN boundary-soundness gap in cyclic-import inference (ADR-052).
    // A 3-module cycle a -> b -> c -> a where the only literal lives in `fromC`, and
    // `fromA`/`fromB` get their return type only by calling through a peer.
    //
    // RUNTIME is correct: codegen calls the real symbol, so `fromA(3)` returns "done".
    // STATIC TYPE is lost at the boundary: `fromA`'s return type flows through a peer call,
    // so the single-round SCC fixed point leaves it permissive/unsolved — a consumer can
    // bind the (actually-String) result to Int32 with NO type error. That missed error is
    // the gap. If a future change iterates Phase 2 to convergence (or fails closed by
    // requiring an annotation), the second half of this test should start failing — update
    // ADR-052 and flip the assertion when it does.
    let dir = std::env::temp_dir().join(format!("lin_cyc_peerret_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("a.lin"),
        "import { fromB } from \"b\"\n\
         export val fromA = (n: Int32) => fromB(n)\n").unwrap();
    std::fs::write(dir.join("b.lin"),
        "import { fromC } from \"c\"\n\
         export val fromB = (n: Int32) => fromC(n)\n").unwrap();
    std::fs::write(dir.join("c.lin"),
        "import { fromA } from \"a\"\n\
         export val fromC = (n: Int32) => if n == 0 then \"done\" else fromA(n - 1)\n").unwrap();

    // 1. It compiles and RUNS correctly (prints "done").
    let main = format!(r#"import {{ print }} from "std/io"
import {{ fromA }} from "{d}/a"
print(fromA(3))
"#, d = dir.to_str().unwrap());
    let output = run(&main);
    assert_eq!(output, vec!["done"], "runtime result must be correct regardless of the type gap");

    // 2. The gap: binding the (actually-String) result to Int32 is wrongly ACCEPTED,
    //    because the peer-dependent return type is permissive at the module boundary.
    let bad = format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ fromA }} from "{d}/a"
val k: Int32 = fromA(3)
print(toString(k))
"#, d = dir.to_str().unwrap());
    let bad_path = dir.join("bad.lin");
    std::fs::write(&bad_path, &bad).unwrap();
    let check = lin_cmd()
        .args(["check", bad_path.to_str().unwrap()])
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    let check_out = format!("{}{}",
        String::from_utf8_lossy(&check.stderr), String::from_utf8_lossy(&check.stdout));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(check.status.success(),
        "ADR-052 boundary gap: binding a peer-dependent cyclic return to Int32 is currently \
         accepted. If this now FAILS, the gap was closed — flip this assertion and update ADR-052. \
         got: {check_out}");
}

#[test]
fn test_intrinsic_rejected_in_user_code() {
    // ADR-060: `lin_*` compiler intrinsics must not be callable from user code; they are
    // resolvable only when type-checking a trusted stdlib module (or with the LIN_ALLOW_INTRINSICS
    // test escape hatch). This test invokes `lin check` WITHOUT the escape hatch.
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let src_path = ws.join(format!("target/lin_test_intr_{}.lin", id));
    fs::write(
        &src_path,
        "import { print } from \"std/io\"\nvar o: AnyVal = {}\nlin_object_set(o, \"k\", 1)\nprint(\"x\")\n",
    )
    .unwrap();
    // NOTE: bare Command, no .env("LIN_ALLOW_INTRINSICS", ...) — the gate must be ACTIVE.
    let out = Command::new(lin_bin())
        .args(["check", src_path.to_str().unwrap()])
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "expected check to fail; stderr:\n{}",
        stderr
    );
    assert!(
        stderr.contains("compiler-internal intrinsic"),
        "wrong error:\n{}",
        stderr
    );

    // The escape hatch re-enables intrinsics for the compiler's own fixtures.
    let out_hatch = Command::new(lin_bin())
        .args(["check", src_path.to_str().unwrap()])
        .env("LIN_ALLOW_INTRINSICS", "1")
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary");
    let _ = fs::remove_file(&src_path);
    assert!(
        out_hatch.status.success(),
        "LIN_ALLOW_INTRINSICS escape hatch should permit the intrinsic; stderr:\n{}",
        String::from_utf8_lossy(&out_hatch.stderr)
    );
}

#[test]
fn test_default_args_trailing_comma_still_curries() {
    // A trailing comma requests partial application even when defaults exist,
    // rather than filling the default.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val scale = (x: Int32, factor: Int32 = 2) => x * factor
val triple = scale(3,)
print(toString(triple(4)))
"#);
    assert_eq!(output, vec!["12"]);
}

#[test]
fn test_default_args_too_few_is_error() {
    // Supplying fewer than the required (non-defaulted) arguments is an error.
    let err = run_expect_err(r#"import { print } from "std/io"
val f = (a: Int32, b: Int32 = 1) => a + b
print(f())
"#);
    assert!(err.contains("Too few arguments"), "got: {}", err);
}

#[test]
fn test_default_args_required_after_optional_is_error() {
    // A required parameter may not follow one with a default value.
    let err = run_expect_err(r#"
val bad = (a: Int32, b: Int32 = 1, c: Int32) => a + b + c
"#);
    assert!(err.contains("cannot follow a parameter with a default"), "got: {}", err);
}

#[test]
fn test_iter_builtin() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { iter } from "std/iter"
import { for } from "std/iter"

val myIter = iter(
  () => 0,
  i => i < 3,
  i => i + 1,
  i => i * 10
)
myIter.for(x => print(toString(x)))
"#);
    assert_eq!(output, vec!["0", "10", "20"]);
}

#[test]
fn test_combinator_optional_index_param() {
    // The iterator combinators OPTIONALLY pass a 0-based Int32 SOURCE index as a trailing
    // callback parameter (`(item, i) => …`); a 1-arg callback stays valid (backward compatible).
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map, filter, for, reduce, find, takeWhile, dropWhile } from "std/iter"

// map with index, and 1-arg map regression
print(toString(["a", "b", "c"].map((x, i) => i)))
print(toString([1, 2, 3].map(x => x * 2)))
// for accumulates "${i}:${x}"
["a", "b"].for((x, i) => print("${i}:${x}"))
// filter index is SOURCE position (keeps 10, 30 at indices 0, 2)
print(toString([10, 20, 30, 40].filter((x, i) => i % 2 == 0)))
// reduce 3-arg: 0 + 0 + 1 + 2 = 3
print(toString([1, 1, 1].reduce(0, (acc, x, i) => acc + i)))
// reduce 2-arg regression: 1 + 2 + 3 = 6
print(toString([1, 2, 3].reduce(0, (acc, x) => acc + x)))
// derived combinator with a 2-arg callback
print(toString([10, 20, 30].find((x, i) => i == 2)))
// takeWhile / dropWhile index correctness
print(toString([5, 6, 7, 8].takeWhile((x, i) => i < 2)))
print(toString([5, 6, 7, 8].dropWhile((x, i) => i < 2)))
"#);
    assert_eq!(
        output,
        vec![
            "[0, 1, 2]", "[2, 4, 6]", "0:a", "1:b", "[10, 30]", "3", "6", "30", "[5, 6]", "[7, 8]",
        ]
    );
}

#[test]
fn test_combinator_index_param_non_int32_annotation_is_error() {
    // An explicitly-annotated index parameter must be Int32; any other annotation is a compile
    // error. Both the PREFIX form `map([..], (x, i: String) => x)` and the DOT form
    // `[..].map((x, i: String) => x)` reject — the dot path now runs the same argument-
    // compatibility loop as the prefix path, so the wrong `String` index annotation is no longer
    // silently ignored.
    let err = run_expect_err(r#"import { print } from "std/io"
import { map } from "std/iter"
print(map([1, 2, 3], (x, i: String) => x))
"#);
    assert!(
        err.contains("Int32"),
        "expected an Int32-index diagnostic (prefix form), got: {}",
        err
    );

    // DOT form: previously slipped past the type check; now rejected.
    let err_dot = run_expect_err(r#"import { print } from "std/io"
import { map } from "std/iter"
import { toString } from "std/string"
print(toString([1, 2, 3].map((x, i: String) => x)))
"#);
    assert!(
        err_dot.contains("Int32"),
        "expected an Int32-index diagnostic (dot form), got: {}",
        err_dot
    );

    // The dedicated diagnostic surfaces when the callback is checked against an explicit
    // `(T, Int32) => …` expected type that is NOT swallowed by the combinator-arg fallback.
    let err2 = run_expect_err(r#"
val apply = (f: (Int32, Int32) => Int32) => f(1, 2)
val r = apply((x, i: String) => x)
"#);
    assert!(
        err2.contains("index parameter of an iterator callback must be Int32")
            || err2.contains("Int32"),
        "got: {}",
        err2
    );
}

#[test]
fn test_dot_call_callback_annotation_checked() {
    // GENERAL regression (independent of the index feature): the DOT-application path now runs the
    // same argument-compatibility check the PREFIX path does, so a wrong callback PARAM ANNOTATION
    // in dot form — `[1,2,3].map((x: String) => x)` — is rejected. Previously the annotation was
    // silently ignored and the program type-checked.
    let err = run_expect_err(r#"import { print } from "std/io"
import { map } from "std/iter"
import { toString } from "std/string"
print(toString([1, 2, 3].map((x: String) => x)))
"#);
    assert!(
        !err.is_empty() && (err.contains("String") || err.contains("expected")),
        "expected a callback-arg mismatch diagnostic for the dot form, got: {}",
        err
    );

    // POSITIVE guards — all must STILL type-check and run:
    //  * an unannotated callback param that infers from the receiver,
    //  * a CORRECT annotation,
    //  * a SHORTER-arity callback accepted where the indexed `(x, i)` shape is expected
    //    (arity-width subtyping must survive the new gate).
    let output = run(r#"import { print } from "std/io"
import { map } from "std/iter"
import { toString } from "std/string"
print(toString([1, 2, 3].map(x => x)))
print(toString([1, 2, 3].map((x: Int32) => x * 2)))
print(toString([1, 2, 3].map((x, i) => x + i)))
"#);
    assert_eq!(output, vec!["[1, 2, 3]", "[2, 4, 6]", "[1, 3, 5]"]);
}

#[test]
fn test_generic_phantom_union_param_and_record_field_pinned() {
    // Regression (monomorphizer root cause): a generic function with a PHANTOM type parameter `E`
    // that appears ONLY inside an un-constructed union arm of its return type
    // (`{ "type": "failure", "error": E }` of `Result<T, E>`) must NOT be rejected as
    // uninferrable. The innermost `ok(21)` pins `T = Int32` from its argument; `E` is bound to
    // itself by union-arm matching (nothing at the call carries it), which previously tripped
    // "cannot infer a concrete type". `E` is now recognised as a phantom return param and erased to
    // the `$AnyVal` wildcard (it never reaches a constructed value), so the call monomorphizes.
    //
    // It also exercises the field-substitution + per-variant union index fixes: `mapOk(ok(21), dbl)`
    // infers `U = Int32`, giving `Result<Int32, E>`, and `["value"]` resolves PRECISELY to
    // `Int32 | Null` (the `failure` arm has no `value` — §6.1 safe-bracket / ADR-044 R1), matching
    // the `MaybeInt = Int32 | Null` annotation. The program compiles and runs.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Result<T, E> = { "type": "success", "value": T } | { "type": "failure", "error": E }
type MaybeInt = Int32 | Null
val ok = <T, E>(v: T): Result<T, E> =>
  { "type": "success", "value": v }
val mapOk = <T, U, E>(r: Result<T, E>, f: (T) => U): Result<U, E> =>
  match r
    has { "type": "success", value } => ok(f(value))
    else => r
val dbl = (x: Int32): Int32 =>
  x * 2
val v: MaybeInt = mapOk(ok(21), dbl)["value"]
val arr: MaybeInt[] = [mapOk(ok(21), dbl)["value"]]
print("ok")
"#);
    assert_eq!(output, vec!["ok"]);

    // DIAGNOSTIC: feeding the same union access into a strict (non-nullable) `Int32[]` context is
    // correctly rejected — the union index is `Int32 | Null`, not `Int32`. The message must name the
    // RESOLVED `Int32 | Null` (proving the field was pinned), with no unresolved `?T…` typevar.
    let err = run_expect_err(r#"import { print } from "std/io"
import { length } from "std/array"
import { toString } from "std/string"
type Result<T, E> = { "type": "success", "value": T } | { "type": "failure", "error": E }
val ok = <T, E>(v: T): Result<T, E> =>
  { "type": "success", "value": v }
val mapOk = <T, U, E>(r: Result<T, E>, f: (T) => U): Result<U, E> =>
  match r
    has { "type": "success", value } => ok(f(value))
    else => r
val dbl = (x: Int32): Int32 =>
  x * 2
val runBody = (body: () => Int32[]): Int32[] =>
  body()
val out = runBody(() => [mapOk(ok(21), dbl)["value"]])
print(toString(length(out)))
"#);
    assert!(
        err.contains("Int32 | Null") && !err.contains("?T"),
        "result type-param must be pinned to Int32 (no unresolved ?T typevar), got: {}",
        err
    );
}

#[test]
fn test_generic_callback_param_back_inference() {
    // A generic function pins its type parameter `T` from a (type-pinning) argument and that
    // concrete type must be BACK-INFERRED into an UNANNOTATED callback parameter's body. Closes the
    // under-checking hole where `sort`'s `cmp` params stayed unconstrained when unannotated.

    // HOLE A — `sort(xs, (a, b) => a["x"])` over `Int32[]`: `a` is now `Int32`, so indexing it is a
    // genuine type error (previously type-checked because the param was left free).
    let err = run_expect_err(r#"import { sort } from "std/array"
val run = (): Null =>
  val xs: Int32[] = [3, 1, 2]
  val s = sort(xs, (a, b) => a["x"])
  null
run()
"#);
    assert!(
        err.contains("cannot index into `Int32`"),
        "case A should error: callback param `a` must be Int32, got: {}",
        err
    );

    // HOLE B — proof the param is constrained: `val z: String = a` must reject `a` (Int32).
    let err = run_expect_err(r#"import { sort } from "std/array"
val run = (): Null =>
  val xs: Int32[] = [3, 1, 2]
  val s = sort(xs, (a, b) =>
    val z: String = a
    0)
  null
run()
"#);
    assert!(
        err.contains("String") && err.contains("Int32"),
        "case B should error: `a` (Int32) is not String, got: {}",
        err
    );

    // map/filter over an `Int32[]` literal back-infer the element param identically (dot-call path).
    let err = run_expect_err(r#"import { print } from "std/io"
import { map } from "std/iter"
import { toString } from "std/string"
print(toString([1, 2, 3].map(x => x["k"])))
"#);
    assert!(
        err.contains("cannot index into `Int32`"),
        "map callback param `x` must be Int32, got: {}",
        err
    );

    // POSITIVE guards — all must STILL type-check and run:
    //  * unannotated combinator callbacks over concrete element types,
    //  * `sortBy` whose keyFn returns a DIFFERENT type than the element (the generic `U` is solved
    //    from the body, not pinned — must not be forced into a strict mismatch),
    //  * `reduce` whose accumulator `U` is pinned by `init`,
    //  * `[].sort(cmp)` over an EMPTY literal (`T = Never`, no element to constrain — the body must
    //    fall back to inference, not be rejected as "Sub to Never and Never").
    let output = run(r#"import { print } from "std/io"
import { map, filter, reduce } from "std/iter"
import { sort, sortBy } from "std/array"
import { toString } from "std/string"
val xs: Int32[] = [3, 1, 2]
print(toString(xs.map(x => x * 2)))
print(toString(xs.filter(x => x % 2 == 1)))
print(toString(xs.reduce(0, (acc, x) => acc + x)))
print(toString(sortBy(xs, n => n % 3)))
val empty: Int32[] = []
print(toString(empty.sort((a, b) => a - b)))
"#);
    assert_eq!(
        output,
        vec!["[6, 2, 4]", "[3, 1]", "6", "[3, 1, 2]", "[]"],
        "valid generic-callback programs must still type-check and run"
    );
}

#[test]
fn test_undefined_variable_error() {
    let err = run_expect_err(r#"import { print } from "std/io"
import { toString } from "std/string"

print(toString(xyz))
"#);
    assert!(err.contains("Undefined") || err.contains("undefined") || err.contains("xyz"), "got: {}", err);
}

#[test]
fn test_cannot_assign_immutable_error() {
    let err = run_expect_err(r#"import { print } from "std/io"
import { toString } from "std/string"

val x = 5
x = 10
print(toString(x))
"#);
    assert!(
        err.contains("Cannot assign") || err.contains("immutable") || err.contains("not a mutable") || err.contains("expected"),
        "got: {}", err
    );
}

#[test]
fn test_empty_array_and_object() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"

val arr: Int32[] = []
val obj: { String: Int32 } = {}
print(toString(length(arr)))
print(toString(length(obj)))
"#);
    assert_eq!(output, vec!["0", "0"]);
}

// ADR-058: an evidence-free empty array literal (no annotation, no contextual type, no contents)
// cannot infer its element type and must be a compile error pointing the user at an annotation.
#[test]
fn test_context_free_empty_array_errors() {
    let err = run_expect_err(r#"import { print } from "std/io"
val xs = []
print("unreachable")
"#);
    assert!(
        err.contains("cannot infer the element type of an empty array literal"),
        "expected the empty-array annotation error, got: {err}"
    );
}

// ADR-058: same for an evidence-free empty map/object literal.
#[test]
fn test_context_free_empty_object_errors() {
    let err = run_expect_err(r#"import { print } from "std/io"
val m = {}
print("unreachable")
"#);
    assert!(
        err.contains("cannot infer the value type of an empty map/object literal"),
        "expected the empty-map annotation error, got: {err}"
    );
}

// ADR-058: a `var` (mutable) evidence-free empty literal must error the same way.
#[test]
fn test_context_free_empty_var_errors() {
    let err = run_expect_err(r#"import { print } from "std/io"
var xs = []
print("unreachable")
"#);
    assert!(
        err.contains("cannot infer the element type of an empty array literal"),
        "expected the empty-array annotation error for a var, got: {err}"
    );
}

// ADR-058: an empty literal WITH contextual evidence still works — an annotation on the binding,
// a typed function parameter (argument position), and a typed function return are all evidence.
#[test]
fn test_empty_literal_with_context_still_works() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"

// annotation on the binding
val a: Int32[] = []
// annotation on a map binding
val m: { String: Int32 } = {}
// argument position: the typed param supplies the element type
val sized = (xs: Int32[]): Int32 => length(xs)
// return position: the declared return supplies the element type
val mkEmpty = (): Int32[] => []
print(toString(length(a)))
print(toString(length(m)))
print(toString(sized([])))
print(toString(length(mkEmpty())))
"#);
    assert_eq!(output, vec!["0", "0", "0", "0"]);
}

// ADR-058 (deferred Phase 2): `push` stays `(AnyVal, AnyVal)`, so its element type is still NOT
// checked — `push(intArr, "str")` type-checks today. Making `push` generic (`<T>(arr: T[],
// item: T)`) to close that hole is blocked on a separate monomorphized-body/`lin_push`-intrinsic
// representation bug (see the comment on `push` in stdlib/array.lin). This test PINS the current
// (intentionally lax) behavior so the deferral is explicit and a future fix flips it deliberately.
#[test]
fn test_push_element_type_is_not_yet_checked() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { push, length } from "std/array"
val xs: Int32[] = []
push(xs, 1)
print(toString(length(xs)))
"#);
    assert_eq!(output, vec!["1"]);
}

// ADR-058: the untyped-accumulator idiom WITH an annotation works end to end — build an array via
// `push` in a loop and read it back, including a String[] accumulator (heap-element push).
#[test]
fn test_annotated_push_accumulator_end_to_end() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { push, length } from "std/array"
import { for, range } from "std/iter"

val nums = (): Int32[] =>
  val acc: Int32[] = []
  range(0, 4).for(i => push(acc, i * 2))
  acc
val words = (): String[] =>
  val acc: String[] = []
  ["a", "b", "c"].for(w => push(acc, w))
  acc
val ns = nums()
val ws = words()
print(toString(length(ns)))
print(toString(ns[3]))
print(toString(length(ws)))
print(ws[2])
"#);
    assert_eq!(output, vec!["4", "6", "3", "c"]);
}

#[test]
fn test_nested_objects_access() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val data = {
  "users": [
    { "name": "Alice", "scores": [95, 87, 92] },
    { "name": "Bob", "scores": [78, 82, 90] }
  ]
}
print(data["users"][0]["name"])
print(toString(data["users"][1]["scores"][2]))
"#);
    assert_eq!(output, vec!["Alice", "90"]);
}

#[test]
fn test_tail_call_optimization() {
    // Use Int64 to avoid Int32 overflow at 100000 iterations.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val sum = (n: Int64, acc: Int64): Int64 =>
  if n == 0 then acc else sum(n - 1, acc + n)

print(toString(sum(100000, 0)))
"#);
    assert_eq!(output, vec!["5000050000"]);
}

#[test]
fn test_from_contextual_keyword_as_identifier() {
    // `from` is a contextual keyword: reserved only as the import separator, usable
    // as an ordinary identifier (parameter, variable, field, function name) elsewhere.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val dist = (from: Int32, to: Int32): Int32 =>
  to - from

val from = 100
val labelled = { "from": from, "to": 5 }

print(dist(3, 10).toString())
print(toString(from))
print(toString(labelled["from"]))
"#);
    assert_eq!(output, vec!["7", "100", "100"]);
}

#[test]
fn test_from_as_function_name_with_imports() {
    // `from` usable as a function name in a file that still uses `from` as the import
    // separator -- confirms imports remain unbroken alongside identifier use.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val from = (x: Int32): Int32 =>
  x + 1

print(toString(from(6)))
"#);
    assert_eq!(output, vec!["7"]);
}

#[test]
fn test_tco_in_match() {
    let output = run(r#"import { print } from "std/io"

val countdown = (n: Int32): String =>
  match n
    is 0 => "done"
    else => countdown(n - 1)

print(countdown(50000))
"#);
    assert_eq!(output, vec!["done"]);
}

#[test]
fn test_non_tail_self_call_in_discriminated_match_arm() {
    // Regression: a `val` whose RHS is a self-recursive call, inside a `match` arm body
    // that is checked bidirectionally against a string-literal-discriminated union, used to
    // be mis-marked a TAIL call. TCO then replaced the call with a loop back-edge, leaving
    // its result temp undefined while the (live) inner `match` still read it — a codegen
    // "undefined lhs temp" crash. The call is NOT in tail position (its value is consumed),
    // so it must be a plain call. A recursive tree evaluator exercises exactly this shape.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

type Num = { "kind": "num", "value": Int32 }
type Bin = { "kind": "bin", "op": String, "left": Expr, "right": Expr }
type Expr = Num | Bin

type Failure = { "type": "failure", "error": String }
type EvalSuccess = { "type": "success", "value": Int32 }
type Evaluated = EvalSuccess | Failure

val fail = (msg: String): Failure => { "type": "failure", "error": msg }

val apply = (op: String, a: Int32, b: Int32): Evaluated =>
  if op == "+" then { "type": "success", "value": a + b }
  else if op == "*" then { "type": "success", "value": a * b }
  else if b == 0 then fail("division by zero")
  else { "type": "success", "value": a / b }

val evalNode = (node: Expr): Evaluated =>
  match node
    is Num => { "type": "success", "value": node["value"] }
    is Bin =>
      val left = evalNode(node["left"])
      match left
        is EvalSuccess =>
          val right = evalNode(node["right"])
          match right
            is EvalSuccess => apply(node["op"], left["value"], right["value"])
            else => right
        else => left

val tree: Expr = { "kind": "bin", "op": "*", "left": { "kind": "bin", "op": "+", "left": { "kind": "num", "value": 2 }, "right": { "kind": "num", "value": 3 } }, "right": { "kind": "num", "value": 4 } }

match evalNode(tree)
  has { "type": "success", value } => print("= ${toString(value)}")
  else => print("error")
"#);
    assert_eq!(output, vec!["= 20"]);
}

#[test]
fn test_continuation_lines_and() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val person = { "age": 25, "name": "Bob", "active": true }
val result = person["age"] >= 18
  && person["name"] == "Bob"
  && person["active"]
print(toString(result))
"#);
    assert_eq!(output, vec!["true"]);
}

#[test]
fn test_continuation_lines_or() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val x = false
val y = true
val result = x
  || y
print(toString(result))
"#);
    assert_eq!(output, vec!["true"]);
}

#[test]
fn test_continuation_in_if_condition() {
    let output = run(r#"import { print } from "std/io"

val age = 25
val active = true
val result = if age >= 18
  && active then "active adult"
else "other"
print(result)
"#);
    assert_eq!(output, vec!["active adult"]);
}

#[test]
fn test_import_aliasing() {
    let output = run(r#"import { print } from "std/io"
import { trim } from "std/string"

import { trim as t } from "std/string"
val result = "  hi  ".t()
print(result)
"#);
    assert_eq!(output, vec!["hi"]);
}

#[test]
fn test_tuple_dot_application() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val sub = (a: Int32, b: Int32): Int32 => a - b
val result = (10, 3).sub
print(toString(result))
"#);
    assert_eq!(output, vec!["7"]);
}

// Fixed-length array types (`[T1, T2, ...]`, spec §5.3). An array literal checked
// against a fixed-length type is stored as a TAGGED array (heterogeneous positional
// element types); indexing reads the tagged slot and unboxes to the positional type.
// Regression: before, the literal inferred to the unbounded `T[]` and failed the type
// check; after a partial fix it type-checked but indexing read flat bytes and returned
// garbage. This covers heterogeneous + homogeneous + float positions + AnyVal[] widening.
#[test]
fn test_fixed_length_array_types() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"

val pair: [String, Int32] = ["age", 42]
val triple: [String, Int32, Int32] = ["coords", 10, 20]
print(pair[0])
print(toString(pair[1]))
print(toString(triple[2]))

val pt: [Float64, Float64] = [1.5, 2.0]
print(toString(pt[0] + pt[1]))

// A fixed-length array is assignable to the matching unbounded type.
val widened: AnyVal[] = pair
print(toString(length(widened)))
print(widened[0])
"#);
    assert_eq!(output, vec!["age", "42", "20", "3.5", "2", "age"]);
}

// Arity mismatch against a fixed-length array type is a compile-time error.
#[test]
fn test_fixed_length_array_arity_mismatch() {
    let result = run_expect_err(r#"val p: [String, Int32] = ["only-one"]
print("unreachable")
"#);
    assert!(
        result.contains("2-element") || result.contains("element"),
        "expected an arity error, got: {result}"
    );
}

// Bidirectional checking of array literals against a declared tuple (FixedArray) return type.
// Before this fix, `(): [Int32, String] => [1, "x"]` was rejected with "Function body has type
// Int32 | String[], declared return type is [Int32, String]" because `expected_pushes_into_branches`
// did not include `FixedArray`, so the body was inferred bottom-up as a homogeneous union array.
#[test]
fn test_tuple_return_type_checking() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

type Pair = [Int32, String]

// Bare tuple literal in return position — the core regression case.
val f = (): Pair => [1, "hello"]
val p = f()
print(toString(p[0]))
print(p[1])

// Inline type annotation (no named alias).
val g = (): [Int32, String] => [42, "world"]
val q = g()
print(toString(q[0]))
print(q[1])

// Block body with intermediate val — still works.
val h = (): Pair =>
  val x = 7
  [x, "block"]
val r = h()
print(toString(r[0]))
print(r[1])
"#);
    assert_eq!(output, vec!["1", "hello", "42", "world", "7", "block"]);
}

// Tuple return with arity mismatch in function body is a type error.
#[test]
fn test_tuple_return_arity_mismatch() {
    let result = run_expect_err(r#"val f = (): [Int32, String] => [1]
"#);
    assert!(
        result.contains("2-element") || result.contains("element"),
        "expected arity error, got: {result}"
    );
}

// Tuple return with element type mismatch in function body is a type error.
#[test]
fn test_tuple_return_type_mismatch() {
    let result = run_expect_err(r#"val f = (): [Int32, String] => [1, 2]
"#);
    assert!(
        result.contains("String") || result.contains("type"),
        "expected type error, got: {result}"
    );
}

#[test]
fn test_array_rest_destructuring() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"

val [first, ...rest] = [1, 2, 3, 4, 5]
print(toString(first))
print(toString(length(rest)))
print(toString(rest[0]))
"#);
    assert_eq!(output, vec!["1", "4", "2"]);
}

#[test]
fn test_stdlib_string_extended() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

import { contains, startsWith, endsWith, split, join, replace } from "std/string"

print(toString("hello world".contains("world")))
print(toString("hello".startsWith("hel")))
print(toString("hello".endsWith("xyz")))

val parts = "a,b,c".split(",")
print(parts.join("-"))
print("foo bar".replace("bar", "baz"))
"#);
    assert_eq!(output, vec!["true", "true", "false", "a-b-c", "foo baz"]);
}

#[test]
fn test_higher_order_functions() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val apply = (f: (Int32) => Int32, x: Int32): Int32 => f(x)
val double = (n: Int32): Int32 => n * 2
print(toString(apply(double, 5)))

val adder = (n: Int32) => (x: Int32) => x + n
val add5 = adder(5)
print(toString(add5(10)))
"#);
    assert_eq!(output, vec!["10", "15"]);
}

#[test]
fn test_map_returns_capturing_closures() {
    // Regression (ADR-041 owning captures): a `map` callback that RETURNS a closure capturing
    // the callback parameter. The returned thunks ESCAPE into the result array; each must own
    // its captured value (the element box), not borrow a per-iteration box that is freed and
    // reused. Before the owning-capture fix, calling a thunk returned garbage (`[[object]…]`)
    // because the captured value pointed at freed-then-reused memory.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map } from "std/iter"

val thunks = map([5, 6, 7], i => () => i)
print(toString(thunks[0]()))
print(toString(thunks[1]()))
print(toString(thunks[2]()))
"#);
    assert_eq!(output, vec!["5", "6", "7"]);
}

#[test]
fn test_closure_captures_string_escapes() {
    // A capturing closure over a String that ESCAPES its creating scope: `makeGreeter` returns a
    // thunk capturing the `name` parameter, and the returned thunk outlives `makeGreeter`'s
    // frame. The env must OWN the captured string (retain on capture / release on free) so it
    // stays alive after the call returns.
    let output = run(r#"import { print } from "std/io"

val makeGreeter = (name: String) => () => "hi ${name}"
val g0 = makeGreeter("alice")
val g1 = makeGreeter("bob")
print(g0())
print(g1())
print(g0())
"#);
    assert_eq!(output, vec!["hi alice", "hi bob", "hi alice"]);
}

#[test]
fn test_named_fn_as_opaque_function_value() {
    // Regression: passing a TOP-LEVEL NAMED function where an opaque `Function` value is
    // expected used to produce GARBAGE. The capture-less closure wrapper (`__cls_wrapb_*`)
    // copied the named fn's CONCRETE param types (e.g. i32), but the uniform closure-call ABI
    // invokes the wrapper with BOXED (ptr) args — so a TaggedVal* was reinterpreted as a scalar
    // (or vice-versa) → garbage / misaligned deref. Now the wrapper takes all-`ptr` params and
    // unboxes each to the body's concrete type, and every indirect call boxes its args uniformly.
    // Covers: scalar Int32 (1-arg), String, and a 2-param named fn through an opaque `Function`.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val dbl = (x: Int32): Int32 => x * 2
val apply = (f: Function, x: Int32): Int32 => f(x)
print(toString(apply(dbl, 5)))

val shout = (s: String): String => "${s}!"
val applyStr = (f: Function, s: String): String => f(s)
print(applyStr(shout, "hi"))

val add = (a: Int32, b: Int32): Int32 => a + b
val combine = (f: Function): Int32 => f(3, 4)
print(toString(combine(add)))
"#);
    assert_eq!(output, vec!["10", "hi!", "7"]);
}

#[test]
fn test_named_fn_in_map() {
    // Regression (wrapper-ABI bug): `[1,2,3].map(namedFn)` passes the named function as a
    // `Function` value to `map`, hitting the same boxed-vs-concrete closure-wrapper mismatch.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map, for } from "std/iter"

val dbl = (x: Int32): Int32 => x * 2
[1, 2, 3].map(dbl).for(v => print(toString(v)))
"#);
    assert_eq!(output, vec!["2", "4", "6"]);
}

#[test]
fn test_named_fn_as_function_arg_to_multiparam_user_fn() {
    // Regression: passing a top-level NAMED function as a `Function`-typed ARGUMENT to a
    // multi-param USER function (alongside other heap/scalar params) used to DROP the arg.
    // A bare `LocalGet` of a global-fn slot in value position fell through to a placeholder
    // null temp with no defining instruction, so codegen's arg collection (filter_map over
    // temp_map) silently dropped it — emitting 3 args for a 4-param call. A RECURSIVE callee
    // then failed to build ("Incorrect number of arguments passed to called function!"); a
    // NON-RECURSIVE callee built then SEGFAULTED when it invoked the missing Function arg.
    // Fix: materialize the named fn as a closure VALUE (MakeClosure, no captures) like a
    // lambda literal would. Covers recursive + non-recursive callees, AnyVal + Int args.

    // Recursive callee, AnyVal args.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val leaf = (t: AnyVal, p: Int32): AnyVal => { "v": p }
val combine = (t: AnyVal, l: AnyVal, p: Int32, f: Function): AnyVal =>
  if p >= 2 then { "v": l }
  else
    val r = f(t, p + 1)
    combine(t, r, r["v"], f)
val go = (t: AnyVal): AnyVal => combine(t, { "v": 0 }, 0, leaf)
print(toString(go([])))
"#);
    assert_eq!(output, vec![r#"{"v": {"v": 2}}"#]);

    // Non-recursive callee, AnyVal args.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val leaf = (t: AnyVal, p: Int32): AnyVal => { "v": p }
val combine = (t: AnyVal, l: AnyVal, p: Int32, f: Function): AnyVal => f(t, p)
val go = (t: AnyVal): AnyVal => combine(t, { "v": 0 }, 0, leaf)
print(toString(go([])))
"#);
    assert_eq!(output, vec![r#"{"v": 0}"#]);

    // Non-recursive callee, all-Int args.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val leaf = (t: Int32, p: Int32): Int32 => t + p
val combine = (t: Int32, l: Int32, p: Int32, f: Function): Int32 => f(t, p)
val go = (t: Int32): Int32 => combine(t, 0, 0, leaf)
print(toString(go(9)))
"#);
    assert_eq!(output, vec!["9"]);
}

#[test]
fn test_function_param_destructuring() {
    let output = run(r#"import { print } from "std/io"

val greetPerson = ({ name, age }: AnyVal): String =>
  "${name} is ${age}"

print(greetPerson({ "name": "Bob", "age": 42 }))
"#);
    assert_eq!(output, vec!["Bob is 42"]);
}

#[test]
fn test_chained_if_else() {
    let output = run(r#"import { print } from "std/io"

val classify = (x: Int32): String =>
  if x > 100 then "big"
  else if x > 10 then "medium"
  else "small"

print(classify(200))
print(classify(50))
print(classify(5))
"#);
    assert_eq!(output, vec!["big", "medium", "small"]);
}

#[test]
fn test_multi_statement_lambda_in_parens() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { for } from "std/iter"

val data = [1, 2, 3]
data.for(x =>
  val doubled = x * 2
  print(toString(doubled))
)
"#);
    assert_eq!(output, vec!["2", "4", "6"]);
}

#[test]
fn test_bare_expr_side_effects_in_inline_lambda() {
    let output = run(r#"import { print } from "std/io"
import { for } from "std/iter"

val data = [1, 2, 3]
data.for(x =>
  print("a")
  print("b")
)
"#);
    assert_eq!(output, vec!["a", "b", "a", "b", "a", "b"]);
}

// Inside a parenthesised lambda body, a multi-statement `if`-then block must keep ALL its
// statements (the ADR-003 newline-suppression bug used to drop all but the first, making the
// rest run unconditionally). The offside rule (column > the `if` keyword) delimits the block.
#[test]
fn test_multi_statement_if_then_in_inline_lambda() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range, for } from "std/iter"

var hits = 0
val run = (): Null =>
  range(0, 3).for(i =>
    if i == 0 then
      hits = hits + 1
      hits = hits + 10
      hits = hits + 100
  )
run()
print(toString(hits))
"#);
    // Only i == 0 runs the three statements: 1 + 10 + 100 == 111.
    assert_eq!(output, vec!["111"]);
}

// The statement AFTER a nested multi-statement `if` (dedented to the if's column) belongs to
// the enclosing lambda body and runs every iteration; the if-block statements run only when the
// condition holds. Distinguishes "swallowed too little" from "swallowed too much".
#[test]
fn test_statements_after_nested_if_in_inline_lambda() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range, for } from "std/iter"

var a = 0
var b = 0
val run = (): Null =>
  range(0, 2).for(i =>
    if i == 0 then
      a = a + 1
      a = a + 10
    b = b + 100
  )
run()
print(toString(a))
print(toString(b))
"#);
    // a: only i == 0 → 1 + 10 == 11. b: both iterations → 200.
    assert_eq!(output, vec!["11", "200"]);
}

// A `match` expression inside a parenthesised lambda body must parse: ADR-003 suppresses the
// Indent/Dedent that the top-level arm-block relies on, so the parser falls back to the offside
// rule (arms line up at one column). Single-expression arm bodies. Regression for the
// "unexpected token Arrow" bug.
#[test]
fn test_match_in_inline_lambda() {
    let output = run(r#"import { print } from "std/io"
import { range, for } from "std/iter"

range(0, 3).for(i =>
  val label = match i
    is 0 => "zero"
    is 1 => "one"
    else => "many"
  print(label)
)
"#);
    assert_eq!(output, vec!["zero", "one", "many"]);
}

// The statement AFTER a `match` inside a parenthesised lambda body (dedented to the body level)
// must NOT be swallowed into the last arm — it runs every iteration. Distinguishes "swallowed
// too little" (arms truncated) from "swallowed too much" (trailing stmt eaten as an arm body).
#[test]
fn test_statements_after_match_in_inline_lambda() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range, for } from "std/iter"

range(0, 2).for(i =>
  val r = match i
    is 0 => 10
    else => 20
  print(toString(r))
)
"#);
    // Each iteration prints the match result; `print` is after the match, not an arm.
    assert_eq!(output, vec!["10", "20"]);
}

// A multi-statement arm body inside a parenthesised match keeps ALL its statements (offside
// floor = the arm column), while the NEXT arm (aligned at the same column) terminates the body.
#[test]
fn test_multi_statement_match_arm_in_inline_lambda() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range, for } from "std/iter"

range(0, 3).for(i =>
  val r = match i
    is 0 =>
      val a = 1
      val b = 2
      a + b
    is 1 =>
      val x = 10
      x + 5
    else => 99
  print(toString(r))
)
"#);
    assert_eq!(output, vec!["3", "15", "99"]);
}

// A parenthesised `match (x is T)` scrutinee inside an inline lambda still parses its inner `is`
// type-test: the delimited group resets the scrutinee's is/has suppression. Guards against the
// fix over-suppressing `is`/`has`.
#[test]
fn test_match_paren_is_scrutinee_in_inline_lambda() {
    let output = run(r#"import { print } from "std/io"
import { range, for } from "std/iter"

range(0, 2).for(i =>
  val tagged = match (i is Int32)
    is true => "is-int"
    else => "no"
  print(tagged)
)
"#);
    assert_eq!(output, vec!["is-int", "is-int"]);
}

// A multiline JSON object literal passed as an argument is delimited by `{`/`}`/`,`, NOT by the
// offside column. The column guard must not fire inside a literal (literals have their own
// parser). Sanity that the offside change didn't disturb ADR-003 multiline literals in parens.
#[test]
fn test_multiline_json_literal_in_parens_unaffected() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val show = (o): Null =>
  print(toString(o["a"] + o["b"]))
show({
  "a": 1,
  "b": 2
})
"#);
    assert_eq!(output, vec!["3"]);
}

// A dot-chain split across newlines inside a parenthesised lambda body must parse as ONE
// expression (ADR-005). The offside guard only runs BETWEEN statements, never within a single
// `parse_expr()`, so continuation lines of one expression are never split.
#[test]
fn test_dot_chain_across_newlines_in_inline_lambda() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range, for, map, filter } from "std/iter"

val run = (): Null =>
  range(0, 5)
    .map(x => x * 2)
    .filter(x => x > 2)
    .for(x =>
      print(toString(x))
    )
run()
"#);
    assert_eq!(output, vec!["4", "6", "8"]);
}

// A line-leading `[` after a statement inside an inline lambda body starts a NEW array-literal
// statement, not a postfix index on the previous expression. Inside `()` the line break is
// suppressed as a token (ADR-003), so the parser relies on each token's `newline_before` flag.
// Without this, `f` below parsed as `push(acc, 4)[ ... ]` and the body's value was the index
// result (Null) instead of the array. Mirrors the post-Dedent `[` suppression of ADR-010.
#[test]
fn test_line_leading_array_after_statement_in_inline_lambda() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { push, length } from "std/array"

val f = (): AnyVal =>
  val acc = [1, 2, 3]
  push(acc, 4)
  [
    length(acc),
    acc[0]
  ]

print(toString(f()))
"#);
    assert_eq!(output, vec!["[4, 1]"]);
}

#[test]
fn test_bare_expr_side_effects_top_level_func() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val myFunc = () =>
  print("first")
  print("second")
  42

val result = myFunc()
print(toString(result))
"#);
    assert_eq!(output, vec!["first", "second", "42"]);
}

#[test]
fn test_multi_statement_paren_function() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map } from "std/iter"
import { for } from "std/iter"

val result = [10, 20, 30].map((x) =>
  val y = x + 1
  y * 2
)
result.for(r => print(toString(r)))
"#);
    assert_eq!(output, vec!["22", "42", "62"]);
}

#[test]
fn test_push_and_concat() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length, push } from "std/array"
import { concat } from "std/iter"
import { for } from "std/iter"

val arr = [1, 2]
push(arr, 3)
print(toString(length(arr)))

val combined = concat([1], [2, 3])
combined.for(x => print(toString(x)))
"#);
    assert_eq!(output, vec!["3", "1", "2", "3"]);
}

#[test]
fn test_array_allocate_filled() {
    // Regression: arrayAllocateFilled used to ignore the fill value and return all-null
    // (the generic fill path re-wrapped the already-boxed AnyVal arg in a NULL-tagged box).
    // It must now fill every slot with the value — scalars, strings, and heap values alike,
    // and a heap fill must not double-free when the array drops (each slot owns a reference).
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { arrayAllocateFilled, arrayAllocate, set, length } from "std/array"

print(arrayAllocateFilled(3, 0).toString())
print(arrayAllocateFilled(2, "x").toString())
print(arrayAllocateFilled(3, [1, 2]).toString())
print(toString(length(arrayAllocateFilled(0, 9))))

val buf = arrayAllocate(3)
set(buf, 0, "a")
print(buf.toString())
"#);
    assert_eq!(
        output,
        vec![
            "[0, 0, 0]",
            "[\"x\", \"x\"]",
            "[[1, 2], [1, 2], [1, 2]]",
            "0",
            "[\"a\", null, null]",
        ]
    );
}

#[test]
fn test_array_allocate_filled_flat_scalar_annotated() {
    // Regression: `val a: Int32[] = arrayAllocateFilled(n, v)` (a CONCRETE scalar element type
    // via an annotation) must allocate a FLAT unboxed array, matching the flat read path. The
    // wrapper used to be `(n, fill: AnyVal): AnyVal` — erasing the element type, so it always built
    // a TAGGED array while the `Int32[]`-typed reader read it flat, reinterpreting 16-byte
    // TaggedVal slots as packed scalars (garbage). Making the wrapper generic (`<T>(n, fill: T):
    // T[]`) lets the concrete element type reach the allocator. Covers fill, in-place `set`, and
    // a wider scalar (Int64) so a slot-size mismatch would corrupt neighbours.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { arrayAllocateFilled, set } from "std/array"

val a: Int32[] = arrayAllocateFilled(4, 7)
set(a, 1, 99)
print("${toString(a[0])},${toString(a[1])},${toString(a[2])},${toString(a[3])}")

val b: Float64[] = arrayAllocateFilled(2, 1.5)
set(b, 0, 2.5)
print("${toString(b[0])},${toString(b[1])}")

val c: Int64[] = arrayAllocateFilled(2, 7i64)
set(c, 0, 5000000000i64)
print("${toString(c[0])},${toString(c[1])}")
"#);
    assert_eq!(output, vec!["7,99,7,7", "2.5,1.5", "5000000000,7"]);
}

#[test]
fn test_array_allocate_filled_concrete_heap_no_double_free() {
    // Regression (heap UAF): `arrayAllocateFilled(n, <heap value>)` stores the SAME heap value
    // into all n slots, so each slot needs its own reference — else releasing the result frees
    // the shared value n times (double-free / heap-use-after-free, caught by ASan, intermittent
    // under cargo test). When the wrapper became generic, a CONCRETE heap fill (`[1,2]`, a
    // `String`) monomorphized to a non-union element type and bypassed the per-slot retain that
    // the old `fill: AnyVal` path always took. The fix retains per slot for any heap-payload fill
    // (`ty_is_concrete_rc` || boxed union). This builds and DROPS such arrays in a loop so a
    // missing retain corrupts the heap; correctness of the printed values is the visible check.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { arrayAllocateFilled } from "std/array"
import { range, for } from "std/iter"

val make = (): Null =>
  val arrs = arrayAllocateFilled(3, [1, 2])
  val strs = arrayAllocateFilled(2, "shared")
  print("${arrs[0].toString()} ${strs[1]}")

range(0, 4).for(_ => make())
print("ok")
"#);
    assert_eq!(
        output,
        vec!["[1, 2] shared", "[1, 2] shared", "[1, 2] shared", "[1, 2] shared", "ok"]
    );
}

#[test]
fn test_iterator_arg_to_array_param_free_call() {
    // Regression: the free-function form `map(range(0,n), f)` rejected a `range` result
    // (`Iterator<Int32>`) against `map`'s `T[]` param with "Argument 1 has type Iterator<Int32>,
    // expected Int32[]" (and a spurious "Undefined variable" cascade from the dropped binding),
    // even though the equivalent dot form `range(0,n).map(f)` was accepted and the spec (§17.6)
    // says the iterator functions accept "an array or an Iterator<T>". A function call argument
    // of `Iterator<T>` is now accepted where an `Array<U>` param is expected, so `f(x,y)` and
    // `x.f(y)` agree. Plain assignment (`val a: Int32[] = range(..)`) still rejects.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"
import { map, filter, reduce, range } from "std/iter"

val a = map(range(0, 5), i => i * 10)
print("${toString(length(a))} ${toString(a[0])} ${toString(a[4])}")

val b = filter(range(0, 6), i => i % 2 == 0)
print("${toString(length(b))} ${toString(b[0])} ${toString(b[2])}")

val s = reduce(range(1, 5), 0, (acc, i) => acc + i)
print(toString(s))
"#);
    assert_eq!(output, vec!["5 0 40", "3 0 4", "10"]);
}

#[test]
fn test_keys_values_entries() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { keys, values } from "std/object"
import { for } from "std/iter"

val obj = { "a": 1, "b": 2 }
val ks = keys(obj)
ks.for(k => print(k))
val vs = values(obj)
vs.for(v => print(toString(v)))
"#);
    assert_eq!(output, vec!["a", "b", "1", "2"]);
}

#[test]
fn test_stdlib_array_find_some_every() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

import { find, some, every } from "std/iter"
val nums = [1, 2, 3, 4, 5]
print(toString(nums.find(x => x > 3)))
print(toString(nums.find(x => x > 10)))
print(toString(nums.some(x => x == 3)))
print(toString(nums.some(x => x == 99)))
print(toString(nums.every(x => x > 0)))
print(toString(nums.every(x => x > 2)))
"#);
    assert_eq!(output, vec!["4", "null", "true", "false", "true", "false"]);
}

// Wave C: find/some/every called with a NAMED no-capture function (not an inline lambda) are
// devirtualized — the predicate is substituted directly into a per-callback specialization. This
// pins correctness across: two distinct named predicates on the same HOF (two specs), the no-match
// path, AND a capturing-lambda call (must keep the old indirect path and stay correct).
#[test]
fn test_wavec_named_callback_devirt_find_some_every() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { find, some, every } from "std/iter"

val isEven = (x: Int32) => x % 2 == 0
val isBig = (x: Int32) => x > 100

val xs = [1, 3, 5, 6, 7, 8]
print(toString(find(xs, isEven)))    // 6  (devirt → @isEven)
print(toString(find(xs, isBig)))     // null (devirt, no match)
print(toString(some(xs, isEven)))    // true
print(toString(every(xs, isEven)))   // false
print(toString(every(xs, isBig)))    // false
var threshold = 4
print(toString(find(xs, x => x > threshold)))   // 5 (capturing lambda — old path)
"#);
    assert_eq!(output, vec!["6", "null", "true", "false", "false", "5"]);
}

#[test]
fn test_stdlib_array_flatmap_indexof_reverse() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { indexOf, reverse } from "std/array"
import { flatMap } from "std/iter"
import { for } from "std/iter"

val nums = [1, 2, 3]
val pairs = nums.flatMap(x => [x, x * 10])
pairs.for(x => print(toString(x)))
print(toString(nums.indexOf(2)))
print(toString(nums.indexOf(99)))
val rev = nums.reverse()
rev.for(x => print(toString(x)))
"#);
    assert_eq!(output, vec!["1", "10", "2", "20", "3", "30", "1", "-1", "3", "2", "1"]);
}

// WAVE D: a `flatMap(...).filter(...)` chain (a flatMap stage with a downstream FILTER terminal)
// must fuse via the CPS loop-nest engine — `lower_filter` previously lacked the `chain_has_flatmap`
// routing that for/map/reduce have, so the linear applier hit `unreachable!`. Locks in the fix.
#[test]
fn test_stdlib_flatmap_filter_chain() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { flatMap, filter, map } from "std/iter"

print([1, 2, 3].flatMap(x => [x, x * 10]).filter(y => y > 5).toString())
print([1, 2, 3].flatMap(x => [x, x * 10]).filter(y => y > 5).map(z => z + 1).toString())
print([1, 2, 3].flatMap(x => []).filter(y => y > 5).toString())
"#);
    assert_eq!(output, vec!["[10, 20, 30]", "[11, 21, 31]", "[]"]);
}

// WAVE D: a flatMap whose INNER array is heap-element (String[]) — `s => [s, s, s]` — fused into a
// downstream map/filter/reduce chain. The inner element is read as a BORROWED interior pointer, so
// the consume-site reclaim is a no-op and a scalar-output terminal never moves it; RC-correct, no
// per-inner-element leak. Also exercises the entry-block-alloca hoist (the inner literal is built in
// the fused source loop's body). Verifies value equivalence with the eager lowering.
#[test]
fn test_stdlib_flatmap_string_inner_chain() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { flatMap, filter, map, reduce } from "std/iter"
import { length } from "std/array"

print(["x", "y"].flatMap(s => [s, s]).toString())
print(toString(["aaa", "bb", "c"].flatMap(s => [s, s]).map(z => length(z)).filter(n => n > 1).reduce(0, (a, n) => a + n)))
print(["a", "bb"].flatMap(s => [s, s]).map(z => "${z}!").toString())
"#);
    assert_eq!(output, vec![
        "[\"x\", \"x\", \"y\", \"y\"]",
        "10",
        "[\"a!\", \"a!\", \"bb!\", \"bb!\"]",
    ]);
}

// WAVE D — LONE flatMap: `xs.flatMap(f)` with no downstream combinator stage now fuses to a CPS
// loop nest (the inner loop pushes straight into the result) instead of running the eager stdlib
// body. Covers scalar inner, String (heap) inner, the index callback, the provably-empty `x => []`
// inner, and a flatMap whose receiver is itself a map/filter chain. Byte-equivalent to the eager
// lowering it replaces.
#[test]
fn test_stdlib_lone_flatmap_fuses() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { flatMap, map, filter } from "std/iter"

print([1, 2, 3].flatMap(x => [x, x * 10]).toString())
print(["x", "y"].flatMap(s => [s, s]).toString())
print(["a", "b"].flatMap((x, i) => [x, "${i}"]).toString())
print([1, 2, 3].flatMap(x => []).toString())
print([1, 2, 3].map(x => x + 1).flatMap(x => [x, x]).toString())
print([1, 2, 3, 4].filter(x => x > 1).flatMap(x => [x, x * 10]).toString())
"#);
    assert_eq!(output, vec![
        "[1, 10, 2, 20, 3, 30]",
        "[\"x\", \"x\", \"y\", \"y\"]",
        "[\"a\", \"0\", \"b\", \"1\"]",
        "[]",
        "[2, 2, 3, 3, 4, 4]",
        "[2, 20, 3, 30, 4, 40]",
    ]);
}

// flatMap whose inner lambda returns a packed-record array (e.g. `j => [j, j]` where j: Journey):
// `alloc_output_array` emits `Intrinsic::ArrayAlloc` which must allocate a 0xFD sealed-pointer
// array when the element type is a sealed record, NOT a 0xFF tagged array. If 0xFF is allocated but
// 0xFD field-read logic is applied the field access GPFs. Regression for the ArrayAlloc 0xFF/0xFD
// mismatch in lin-codegen intrinsics.rs.
#[test]
fn test_flatmap_packed_record_output_array_repr() {
    let output = run(r#"import { print } from "std/io"
import { flatMap } from "std/iter"
import { length } from "std/array"
import { toString } from "std/string"

type Leg = { "origin": String }
type Journey = { "legs": Leg[], "dep": UInt32, "arr": UInt32 }

val f = (xs: Journey[]): Journey[] =>
  xs.flatMap(j => [j, j])

val js: Journey[] = [{ "legs": [{ "origin": "A" }], "dep": 1u32, "arr": 2u32 }]
val out = f(js)
print(toString(length(out)))
val dep = out[0]["dep"]
print(toString(dep))
"#);
    assert_eq!(output, vec!["2", "1"]);
}

// WAVE D — BARRIER SPLITS, NOT KILLS: a chain with a mid-chain UNFUSABLE stage (a `map` with a
// heap/non-scalar output, which the fuser gates off) must terminate the current fused run by
// materialising ONE intermediate array and start a fresh fused run for the downstream stages — two
// fused passes, not N unfused. This already falls out of the recursive lowering: `extract_fuse_chain`
// stops peeling at the barrier and `lower_expr(base)` recursively re-fuses the prefix. Locks in the
// value equivalence across the split (and over a flatMap-bearing downstream run).
#[test]
fn test_stdlib_fusion_barrier_split() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { filter, map, reduce, flatMap } from "std/iter"
import { length } from "std/array"

// filter+map (fused) → map(heap-out) BARRIER → map+filter+reduce (fused)
print(toString([1, 2, 3, 4, 5, 6].filter(x => x > 1).map(x => x + 1).map(x => [x, x, x]).map(ys => length(ys)).filter(n => n > 0).reduce(0, (a, n) => a + n)))
// barrier with a flatMap downstream run
print([1, 2, 3].map(x => [x, x]).flatMap(ys => ys).toString())
// flatMap upstream, heap-out map barrier, scalar map+reduce downstream
print(toString([1, 2, 3].flatMap(x => [x, x * 10]).map(x => [x, x]).map(ys => length(ys)).reduce(0, (a, n) => a + n)))
"#);
    assert_eq!(output, vec![
        // filter>1 → [2,3,4,5,6]; +1 → [3,4,5,6,7]; →[[x,x,x]]; length → [3,3,3,3,3]; >0 all; sum=15
        "15",
        "[1, 1, 2, 2, 3, 3]",
        // flatMap → [1,10,2,20,3,30]; map [x,x]; length → six 2s; sum=12
        "12",
    ]);
}

#[test]
fn test_forward_reference_between_functions() {
    let output = run(r#"import { print } from "std/io"

val isEvenDesc = (n: Int32): String =>
  if n == 0 then "even"
  else isOddDesc(n - 1)

val isOddDesc = (n: Int32): String =>
  if n == 0 then "odd"
  else isEvenDesc(n - 1)

print(isEvenDesc(4))
print(isOddDesc(4))
print(isEvenDesc(3))
"#);
    assert_eq!(output, vec!["even", "odd", "odd"]);
}

#[test]
fn test_forward_reference_in_closure() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map } from "std/iter"
import { for } from "std/iter"

val process = (items: AnyVal): AnyVal =>
  items.map(x => transform(x))

val transform = (x: Int32): Int32 => x * 10

val result = process([1, 2, 3])
result.for(x => print(toString(x)))
"#);
    assert_eq!(output, vec!["10", "20", "30"]);
}

#[test]
fn test_tostring_objects_and_arrays() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val obj = { "name": "Bob", "age": 25 }
print(toString(obj))
val arr = [1, "two", true, null]
print(toString(arr))
"#);
    // Phase 2: non-sealed objects are backed by LinMap (hash map). toString serializes keys
    // in alphabetical order for deterministic output (insertion order is not preserved).
    assert_eq!(output, vec![
        r#"{"name": "Bob", "age": 25}"#,
        r#"[1, "two", true, null]"#,
    ]);
}

#[test]
fn test_multiline_import() {
    let output = run(r#"import { print } from "std/io"

import {
  trim,
  toUpper
} from "std/string"

print("  hello  ".trim().toUpper())
"#);
    assert_eq!(output, vec!["HELLO"]);
}

#[test]
fn test_object_spread_behaviours() {
    // Consolidated object-spread behaviours (5 former one-build tests → one program; each case
    // keeps uniquely-named bindings and its assertions in order).
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { keys } from "std/object"

// basic: spread then add a new key.
val basicSrc = { "a": 1, "b": 2 }
val basic = { ...basicSrc, "c": 3 }
print(toString(basic["a"]))
print(toString(basic["b"]))
print(toString(basic["c"]))
print(toString(keys(basic)))

// override: an explicit key after a spread overrides the spread value.
val ovrSrc = { "a": 1, "b": 2 }
val ovr = { ...ovrSrc, "a": 99 }
print(toString(ovr["a"]))
print(toString(ovr["b"]))
print(toString(keys(ovr)))

// multiple: a later spread overrides an earlier one on overlapping keys.
val mulA = { "x": 1, "y": 2 }
val mulB = { "y": 20, "z": 30 }
val mul = { ...mulA, ...mulB }
print(toString(mul["x"]))
print(toString(mul["y"]))
print(toString(mul["z"]))
print(toString(keys(mul)))

// empty_source: spreading `{}` contributes no fields.
val emptySrc = { ...{}, "a": 1 }
print(toString(emptySrc["a"]))
print(toString(keys(emptySrc)))

// null_noop: spreading null contributes no fields (not a runtime error).
val nullSrc = { ...null, "a": 1 }
print(toString(nullSrc["a"]))
print(toString(keys(nullSrc)))
"#);
    assert_eq!(
        output,
        vec![
            "1", "2", "3", "[\"a\", \"b\", \"c\"]", // basic
            "99", "2", "[\"a\", \"b\"]",            // override
            "1", "20", "30", "[\"x\", \"y\", \"z\"]", // multiple
            "1", "[\"a\"]",                         // empty_source
            "1", "[\"a\"]",                         // null_noop
        ]
    );
}

#[test]
fn test_object_grow_past_inline_capacity() {
    // Single-allocation objects (header + entries in one block, FLAG_INLINE) must correctly
    // MIGRATE their entries to a separately-heap-allocated buffer when grown past the initial
    // capacity via dynamic `lin_object_set` — preserving every prior key/value through the
    // migration. A literal `{...}` is alloc'd at exactly its field count, and `{}` at cap 1, so
    // adding 30 fields forces several inline→heap migrations (cap 1→2→4→…→32). The full
    // set-then-sum round-trip confirms every migrated entry is intact and the value RC balances.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { keys } from "std/object"
import { length } from "std/array"
import { for, range } from "std/iter"

var o: AnyVal = {}
range(0, 30).for(i => lin_object_set(o, "k${toString(i)}", i * 10))
var sum = 0i64
range(0, 30).for(i => sum = sum + o["k${toString(i)}"])
print(toString(length(keys(o))))
print(toString(sum))
"#);
    assert_eq!(output, vec!["30", "4350"]);
}

#[test]
fn test_typed_map_index_signature() {
    // Typed index-signature map `{ String: T }` (ADR-055): the hashed `LinMap` backing.
    // Insert/lookup of distinct keys, overwrite (length stays put), missing key -> Null,
    // and keys()/values() over the map. The empty `{}` literal infers `{ String: Int32 }`
    // from its annotation context.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { keys, values } from "std/object"
import { length } from "std/array"

var m: { String: Int32 } = {}
m["apple"] = 3
m["banana"] = 7
m["apple"] = 10
print(toString(m["apple"]))
print(toString(m["banana"]))
print(toString(m["missing"]))
print(toString(length(keys(m))))
print(toString(length(values(m))))
"#);
    assert_eq!(output, vec!["10", "7", "null", "2", "2"]);
}

#[test]
fn test_int_literal_union_keyed_object_dynamic_lookup() {
    // Regression: `{ DayOfWeek: Boolean }` where `DayOfWeek = 0|1|...|6` expands at type-check
    // time to a sealed record with string field names "0".."6" (closed-int-literal-union sugar).
    // When accessed with a DYNAMIC integer key (`dow: DayOfWeek = 1`), the codegen must convert
    // the integer key to its string form (via lin_int_to_string) before looking up in the
    // materialized string-keyed LinMap, not pass the raw integer as a LinString* (which caused
    // a misaligned-pointer panic in lin-runtime's hash_string_key). All seven day-keys round-trip.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type DayOfWeek = 0 | 1 | 2 | 3 | 4 | 5 | 6
type ServiceDays = { DayOfWeek: Boolean }
val days: ServiceDays = { 0: true, 1: false, 2: true, 3: true, 4: true, 5: true, 6: true }
val lookup = (dow: DayOfWeek): String =>
  if days[dow] then "yes" else "no"
val d0: DayOfWeek = 0
val d1: DayOfWeek = 1
val d2: DayOfWeek = 2
val d6: DayOfWeek = 6
print(lookup(d0))
print(lookup(d1))
print(lookup(d2))
print(lookup(d6))
"#);
    assert_eq!(output, vec!["yes", "no", "yes", "yes"]);
}

#[test]
fn test_json_not_assignable_to_typed_map() {
    // Type-soundness: there is intentionally NO implicit `AnyVal -> { String: T }` coercion
    // (§5.1.1, §6.3, ADR-055). A `AnyVal` value's runtime payload is a `LinObject` (or any tag),
    // NOT a `LinMap`; relabelling it to the index-signature map type at the call boundary does
    // not convert the representation, so the callee would then read `LinObject` memory as a
    // `LinMap` and corrupt it. The value must be decoded via `fromJson` / narrowing instead.
    // This closes the trusted-stdlib (`lenient_json`) hole: even the stdlib's permissive
    // AnyVal-widening must NOT manufacture this coercion (compat.rs `(TypeVar(MAX), Map) => false`,
    // which fires AHEAD of the lenient `AnyVal -> concrete` arm). The same rejection holds in user
    // code, exercised here.
    let err = run_expect_err(r#"import { print } from "std/io"
import { toString } from "std/string"

val sink = (m: { String: Int32 }): Int32 => 0

val j: AnyVal = { "a": 1, "b": 2 }
print(toString(sink(j)))
"#);
    assert!(
        err.contains("expected { String: Int32 }"),
        "expected a AnyVal -> map argument-type rejection, got: {err}"
    );
}

#[test]
fn test_typed_map_still_widens_to_json_sink() {
    // The SOUND direction `{ String: T } -> AnyVal` must keep working: a typed map flows into a
    // `AnyVal` parameter of a tag-aware reader (keys/values/entries dispatch on the runtime tag),
    // which is representation-safe. This is the companion to `test_json_not_assignable_to_typed_map`
    // — the carve-out closes only the unsound direction, not this one.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { keys, values } from "std/object"
import { length } from "std/array"

var m: { String: Int32 } = {}
m["a"] = 1
m["b"] = 2
// `keys`/`values` are typed `(AnyVal): ...`; passing the typed map here is `{String:Int32} -> AnyVal`.
print(toString(length(keys(m))))
print(toString(length(values(m))))
"#);
    assert_eq!(output, vec!["2", "2"]);
}

#[test]
fn test_generic_over_map_only_param_monomorphizes() {
    // Regression: a generic whose type parameter `T` appears ONLY inside an index-signature map
    // parameter `{ String: T }` must still monomorphize. The IR monomorphizer's `collect_subs` /
    // `mentions_generic_tv` / `subst_type` / `erase_nonconcrete_typevars` were missing a `Type::Map`
    // arm, so `T` was never recovered from the map argument. `object.get`'s third `default: T`
    // param made it register as generic but emit an undefined base symbol; this exercises the
    // map-element binding directly via a user-defined `get`-shaped accessor.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val getOr = <T>(m: { String: T }, key: String, default: T): T =>
  match m[key]
    is Null => default
    else => m[key]

var m: { String: Int32 } = {}
m["a"] = 7
print(toString(getOr(m, "a", 0)))
print(toString(getOr(m, "missing", 99)))

var s: { String: String } = {}
s["k"] = "hi"
print(getOr(s, "k", "x"))
print(getOr(s, "z", "fallback"))
"#);
    assert_eq!(output, vec!["7", "99", "hi", "fallback"]);
}

#[test]
fn test_stdlib_unified_accessors_at_and_get() {
    // The unified defaulted accessors (`std/object.get`, `std/array.at`) over the cross-module
    // monomorphization path. Both take an INDEPENDENT default-type param `D` and return `T | D`;
    // the `D = T` case (a matching-type default) collapses to bare `T` for a known element type.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { get } from "std/object"
import { at } from "std/array"

var m: { String: Int32 } = {}
m["a"] = 7
// Defaulted map reads; the result is `Int32 | Int32` collapsed to `Int32` via the annotation.
val a: Int32 = m.get("a", 0)
val miss: Int32 = m.get("missing", 5)
print(toString(a + 1))
print(toString(miss + 1))

// Over an `Int32[]`, `at(i, d)` with an Int32 default is the bare-Int32 "definitely present" form.
print(toString([10, 20, 30].at(1, -1)))
print(toString([10, 20, 30].at(5, -1)))
print(toString([10, 20, 30].at(-1, -1)))
print(toString([10, 20, 30].at(-9, 99)))
"#);
    assert_eq!(output, vec!["8", "6", "20", "-1", "30", "99"]);
}

#[test]
fn test_at_omitted_default_is_t_or_null_sound() {
    // SOUNDNESS: with the default OMITTED, `at = <T, D>(…, default: D = null)` must infer `D = Null`,
    // so `arr.at(i)` is `T | Null` — NOT bare `T`. Binding it to a bare `Int32` must be REJECTED.
    let err = run_expect_err(
        r#"import { at } from "std/array"
import { print } from "std/io"

val ints: Int32[] = [1, 2, 3]
val bad: Int32 = ints.at(9)
print("unreachable")
"#,
    );
    assert!(
        err.contains("Int32 | Null") || (err.contains("Int32") && err.contains("Null")),
        "expected a `T | Null` type error for an omitted-default `at`, got: {}",
        err
    );
}

#[test]
fn test_at_with_matching_default_is_bare_t() {
    // The dual of the soundness reject: with a same-typed default supplied, `at(arr, i, 0)` over
    // an `Int32[]` collapses `Int32 | Int32` to bare `Int32`, so `val x: Int32 = …` MUST pass and
    // run. In-bounds reads the element; out-of-bounds yields the default.
    let output = run(r#"import { at } from "std/array"
import { print } from "std/io"
import { toString } from "std/string"

val ints: Int32[] = [1, 2, 3]
val present: Int32 = ints.at(1, 0)
val fallback: Int32 = ints.at(9, 0)
print(toString(present))
print(toString(fallback))
"#);
    assert_eq!(output, vec!["2", "0"]);
}

#[test]
fn test_at_independent_default_type_t_or_d() {
    // The default's type `D` is INDEPENDENT of the element type `T`: `at(ints, i, "x")` over an
    // `Int32[]` is `Int32 | String`. Both arms must survive monomorphization (a flat-scalar element
    // vs a boxed string default in one phi) and dispatch correctly at runtime.
    let output = run(r#"import { at } from "std/array"
import { print } from "std/io"
import { toString } from "std/string"

val ints: Int32[] = [10, 20, 30]
val present = ints.at(1, "n/a")
match present
  is String => print("then-str")
  else => print("then-int")
val fallback = ints.at(9, "n/a")
match fallback
  is String => print(toString(fallback))
  else => print("else-int")
"#);
    assert_eq!(output, vec!["then-int", "n/a"]);
}

#[test]
fn test_at_omitted_default_runtime_t_or_null() {
    // The omitted-default runtime path: `at(arr, i)` => `T | Null`, with the value present
    // in-bounds and `null` out-of-bounds, over both Int32[] and String[].
    let output = run(r#"import { at } from "std/array"
import { print } from "std/io"
import { toString } from "std/string"

val ints: Int32[] = [10, 20, 30]
val hit = ints.at(1)
match hit
  is Null => print("int-null")
  else => print(toString(hit))
val miss = ints.at(9)
match miss
  is Null => print("int-null")
  else => print("int-value")

val strs: String[] = ["a", "b"]
val shit = strs.at(0)
match shit
  is Null => print("str-null")
  else => print(shit)
val smiss = strs.at(9)
match smiss
  is Null => print("str-null")
  else => print("str-value")
"#);
    assert_eq!(output, vec!["20", "int-null", "a", "str-null"]);
}

#[test]
fn test_get_independent_default_type_t_or_d() {
    // `std/object.get` mirrors `at`: an independent default type `D` over a `{ String: Int32 }`
    // map. `get(k, "x")` is `Int32 | String`; an omitted default would be `Int32 | Null`.
    let output = run(r#"import { get } from "std/object"
import { print } from "std/io"
import { toString } from "std/string"

var m: { String: Int32 } = {}
m["a"] = 7
val present = m.get("a", "n/a")
match present
  is String => print("then-str")
  else => print(toString(present))
val fallback = m.get("z", "n/a")
match fallback
  is String => print(fallback)
  else => print("else-int")
"#);
    assert_eq!(output, vec!["7", "n/a"]);
}

#[test]
fn test_typed_map_scales_linear_not_quadratic() {
    // The O(1)-average hashed backing: insert N distinct keys then look every one back up.
    // With the old O(n) assoc-list this is O(n^2); the LinMap makes it O(n). A correctness
    // check (every key reads back its value, summed) doubles as the bench oracle.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { keys } from "std/object"
import { length } from "std/array"
import { for, range } from "std/iter"

var m: { String: Int32 } = {}
range(0, 5000).for(i => m["k${toString(i)}"] = i)
var sum = 0i64
range(0, 5000).for(i =>
  val v = m["k${toString(i)}"]
  match v
    is Int32 => sum = sum + v
    else => sum = sum
)
print(toString(length(keys(m))))
print(toString(sum))
"#);
    // sum_{i=0..4999} i = 4999*5000/2 = 12497500
    assert_eq!(output, vec!["5000", "12497500"]);
}

#[test]
fn test_typed_map_string_values_rc() {
    // String (heap) values exercise the map's value retain/release discipline (mirrors
    // lin_object_set's). Building/freeing many maps with heap values that share a string would
    // surface an RC imbalance as a crash; a stable checksum confirms balance.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val loop = (i: Int64, acc: Int64): Int64 =>
  if i == 0i64 then acc
  else
    var m: { String: String } = {}
    m["k"] = "value"
    m["k2"] = "value2"
    val a = m["k"]
    val n = match a
      is String => if a == "value" then 1i64 else 0i64
      else => 0i64
    loop(i - 1i64, acc + n)

print(toString(loop(20000i64, 0i64)))
"#);
    // Each iter contributes 1 when m["k"] reads back "value"; 20000 iters -> 20000.
    assert_eq!(output, vec!["20000"]);
}

#[test]
fn test_typed_map_flat_scalar() {
    // Consolidated ADR-055 flat-scalar typed-map behaviours (5 former one-build tests → one
    // program; each case keeps a uniquely-named map/binding and its assertions, preserved in
    // order). Flat-scalar value types store the scalar UNBOXED inline in the slot (no per-value
    // heap box); the union `T|Null` carries the boxed-scalar tag for T on read-back.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { for, range } from "std/iter"
import { keys, values, entries } from "std/object"
import { length } from "std/array"

// unboxed: insert/overwrite/lookup, missing key -> Null, keys/values/entries.
var mu: { String: Int64 } = {}
mu["a"] = 100i64
mu["b"] = 200i64
mu["a"] = 111i64
print(toString(mu["a"]))
print(toString(mu["b"]))
print(toString(mu["nope"]))
print(toString(length(keys(mu))))
print(toString(length(values(mu))))
print(toString(length(entries(mu))))

// numeric_width: an Int32 source value stored into a { String: Int64 } map widens to T before
// storing, so `is Int64` matches and the value is byte-correct. A wrong tag (TAG_INT32) would
// miss the arm and yield 0. sum_{i=0..999} i = 499500.
var mw: { String: Int64 } = {}
range(0, 1000).for(i => mw["k${toString(i)}"] = i)
var sumw = 0i64
range(0, 1000).for(i =>
  val v = mw["k${toString(i)}"]
  match v
    is Int64 => sumw = sumw + v
    else => sumw = sumw
)
print(toString(sumw))

// float: Float64 flat-scalar values stored unboxed (TAG_FLOAT64 payload = f64 bits).
var mf: { String: Float64 } = {}
mf["pi"] = 3.5
mf["e"] = 2.5
mf["pi"] = 1.25
val fa = mf["pi"]
val fb = mf["e"]
val sumf = match fa
  is Float64 => match fb
    is Float64 => fa + fb
    else => 0.0
  else => 0.0
print(toString(sumf))

// rc_stress: build/free many flat-scalar maps in a tail-recursive loop. A scalar value carries
// NO heap payload, so set/overwrite/free must do NO retain/release on it — an erroneous RC op on
// an unboxed scalar would crash or corrupt before the loop ends. sum_{i=1..30000}(i+2) =
// 450015000 + 60000 = 450075000.
val rcloop = (i: Int64, acc: Int64): Int64 =>
  if i == 0i64 then acc
  else
    var ms: { String: Int64 } = {}
    ms["x"] = i
    ms["y"] = i + 1i64
    ms["x"] = i + 2i64
    val a = ms["x"]
    val n = match a
      is Int64 => a
      else => 0i64
    rcloop(i - 1i64, acc + n)
print(toString(rcloop(30000i64, 0i64)))

// literal: a non-empty flat-scalar map LITERAL checked against { String: Int64 } stores each
// value unboxed (narrower literal widened to T) via the same path as `m[k]=v`.
val ml: { String: Int64 } = { "a": 1, "b": 2, "c": 3 }
print(toString(ml["a"]))
print(toString(ml["c"]))
print(toString(ml["z"]))
print(toString(length(values(ml))))
"#);
    assert_eq!(
        output,
        vec![
            "111", "200", "null", "2", "2", "2", // unboxed
            "499500",                             // numeric_width
            "3.75",                               // float
            "450075000",                          // rc_stress
            "1", "3", "null", "3",                // literal
        ]
    );
}

#[test]
fn test_typed_map_nested() {
    // Regression: a NESTED typed map `{ String: { String: Int32 } }`. The inner write
    // `outer[k][k2] = v` and the chained read `outer[k][k2]` go through codegen's union/`T|Null`
    // string-key write + index paths (the inner `outer[k]` is `{ String: Int32 } | Null`, which is
    // NOT spellable as an `is`-pattern to narrow — ADR-055 §5.1.1 — so it stays a union at the
    // store/read site). Before the fix those paths only dispatched TAG_OBJECT, so a TAG_MAP inner
    // container had its nested writes silently dropped (reads returned the default) — and at scale
    // the mistyped pointer became a misaligned-pointer crash. With the fix the writes land and read
    // back correctly.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val intOr = (v: Int32 | Null, d: Int32): Int32 =>
  match v
    is Int32 => v
    else => d

val run = (): Null =>
  var outer: { String: { String: Int32 } } = {}
  outer["r1"] = {}
  outer["r1"]["a"] = 10
  outer["r1"]["b"] = 20
  outer["r2"] = {}
  outer["r2"]["c"] = 30
  // mutate an existing inner map through the outer key
  outer["r1"]["a"] = 100
  print(toString(intOr(outer["r1"]["a"], -1)))
  print(toString(intOr(outer["r1"]["b"], -1)))
  print(toString(intOr(outer["r2"]["c"], -1)))
  print(toString(intOr(outer["r2"]["missing"], -1)))

run()
"#);
    // r1.a was overwritten 10 -> 100; r1.b = 20; r2.c = 30; a genuinely-absent key -> default -1.
    assert_eq!(output, vec!["100", "20", "30", "-1"]);
}

#[test]
fn test_typed_map_field_in_record() {
    // Regression (ADR-055 §5.1.1): a `{ String: T }` typed map living as a FIELD of an enclosing
    // record. Before the fix, the directed object-checking path in `check_object_against` only
    // engaged when an expected field was a `StrLit` singleton, so a record like `Resp` with a
    // `headers: { String: String }` field (no StrLit field) fell back to undirected inference —
    // the inner `headers` literal got its own fixed-record type `{ "Content-Type": String }` and
    // the whole-record structural check failed. The gate now also engages on a `Map` field, so the
    // inner literal key-widens to a real `LinMap` and the entry is retrievable at runtime.
    //
    // Case (a): a NON-EMPTY header literal; Case (b): an EMPTY `{}` header (the common http path).
    let output = run(r#"import { print } from "std/io"

type Resp = { "status": Int32, "headers": { String: String }, "body": String }

val mk = (): Resp => { "status": 200, "headers": { "Content-Type": "application/json" }, "body": "x" }
val mkEmpty = (): Resp => { "status": 204, "headers": {}, "body": "" }

val r = mk()
val h: { String: String } = r["headers"]
print(h["Content-Type"])

val e = mkEmpty()
val eh: { String: String } = e["headers"]
match eh["Content-Type"]
  is String => print("present")
  else => print("absent")
"#);
    // (a) non-empty header read back; (b) empty map → missing key narrows to the else branch.
    assert_eq!(output, vec!["application/json", "absent"]);
}

#[test]
fn test_typed_map_field_nested_record() {
    // The directing gate is transitive: an outer record whose direct fields are themselves records
    // (no direct StrLit/Map field) still engages directed checking because a nested field contains
    // a `Map`. This confirms `{ inner: { meta: { String: String }, .. }, .. }` key-widens the
    // deeply-nested `meta` literal to a `LinMap`.
    let output = run(r#"import { print } from "std/io"

type Inner = { "meta": { String: String }, "name": String }
type Outer = { "inner": Inner, "count": Int32 }

val mk = (): Outer => { "inner": { "meta": { "k": "v" }, "name": "n" }, "count": 1 }
val o = mk()
val i: Inner = o["inner"]
val m: { String: String } = i["meta"]
print(m["k"])
"#);
    assert_eq!(output, vec!["v"]);
}

#[test]
fn test_inline_object_rc_field_construction() {
    // Phase 2 of the static-record optimization: a no-spread object literal whose fields are
    // all scalar OR concrete heap (Str/Array/Object) is constructed via INLINE entry stores
    // (key/tag/payload + one lin_rc_retain per heap field) instead of per-field
    // lin_object_set_fresh. The retain must EXACTLY mirror retain_tagged_payload, or a value
    // stored into the object is over-/under-counted → use-after-free or double-free.
    //
    // This builds 50k objects in a tail-recursive loop, each holding the SAME shared string in
    // two slots (refcount +2 per object, -2 on free) plus a fresh array and a nested object.
    // It reads every field back and folds them into a checksum. If the inline retain count were
    // wrong the shared string or a freed array element would corrupt long before the loop ends;
    // a stable, correct checksum across 50k build/free cycles confirms the RC accounting.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"

val shared = "shared-string-value"
val loop = (i: Int64, acc: Int64): Int64 =>
  if i == 0i64 then acc
  else
    val rec = { "a": shared, "b": shared, "tags": [i, i + 1i64], "n": i }
    loop(i - 1i64, acc + rec["n"] + length(rec["tags"]))

print(toString(loop(50000i64, 0i64)))
"#);
    // sum_{i=1..50000} (i + 2) = (50000*50001/2) + 2*50000 = 1250025000 + 100000 = 1250125000
    assert_eq!(output, vec!["1250125000"]);
}

#[test]
fn test_union_if_null_nested_dedups_to_single_null() {
    // `if … then null else (if … then v else null)` unions a literal Null with a nested
    // union that also ends in Null. `flatten_union` must drop the NON-adjacent duplicate
    // Null (it uses order-preserving set-insert, not consecutive `Vec::dedup`), so the
    // missing-arm diagnostic reads "not covered: Null", never the malformed "Null | Null".
    let err = run_expect_err(r#"import { print } from "std/io"
import { toString } from "std/string"

val a = true
val b = false
val x = if a then null else (if b then 5 else null)
val y = match x
  is Int32 => x
print(toString(y))
"#);
    assert!(err.contains("not covered: Null") && !err.contains("Null | Null"), "got: {}", err);
}

#[test]
fn test_union_if_null_else_json_collapses_to_json() {
    // When exactly one branch is literal Null and the other is `AnyVal` (the dynamic top type
    // that already subsumes Null), the result collapses to `AnyVal` rather than `AnyVal | Null`.
    // This both avoids a redundant union and keeps the internal `?T…` sentinel out of
    // diagnostics. A `AnyVal` result is assignable to `Int32` under the lenient-json rule.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val j: AnyVal = 7
val c = false
val x: Int32 = if c then null else j
print(toString(x))
"#);
    assert_eq!(output, vec!["7"]);
}

#[test]
fn test_object_shorthand_construction() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val name = "Linus"
val age = 42
val json2 = { name }
val json3 = { "title": "Engineer", name, "age": age }
print(json2["name"])
print(toString(json3["title"]))
print(json3["name"])
print(toString(json3["age"]))
"#);
    assert_eq!(output, vec!["Linus", "Engineer", "Linus", "42"]);
}

#[test]
fn test_index_assign() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val hasBeenSeen = { "Linus": false }
val name = "Linus"
hasBeenSeen[name] = true
print(toString(hasBeenSeen[name]))

val arr = [1, 2, 3]
arr[1] = 99
print(toString(arr[1]))
"#);
    assert_eq!(output, vec!["true", "99"]);
}

#[test]
fn test_async_await_basic() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { async, await } from "std/async"

val p = async(() => 42)
val result = await(p)
print(toString(result))
"#);
    assert_eq!(output, vec!["42"]);
}

#[test]
fn test_await_result_must_handle_error() {
    // §24.2.2 enforcement (ADR-045): await yields `T | Error`, so assigning it to a bare
    // binding that does not handle the Error case is a compile-time type error. The diagnostic
    // names the union vs. the bare target. (Goes through the full `build` pipeline because the
    // standalone `check` subcommand does not resolve imports.)
    let err = run_expect_err(r#"import { async, await } from "std/async"

val p = async(() => 1 + 1)
val r: Int32 = await(p)
"#);
    assert!(
        err.contains("Int32") && err.contains("\"type\""),
        "expected a union-not-assignable-to-Int32 type error, got:\n{err}"
    );
}

#[test]
fn test_await_handled_error_runs() {
    // The flip side of the enforcement: once the Error case is handled (here via `match`), the
    // program type-checks and runs, yielding the resolved value.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { async, await } from "std/async"

val p = async(() => 1 + 1)
match await(p)
  is Error => print("error")
  else => print(toString(await(p)))
"#);
    assert_eq!(output, vec!["2"]);
}

#[test]
fn test_promise_type_annotation_roundtrip() {
    // `Promise<T>` is a first-class opaque type (ADR-045 update): a promise handle can be stored
    // in an explicitly-annotated `Promise<T>` binding and a `Promise<T>[]` array, then awaited.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { async, await, race } from "std/async"
import { push } from "std/array"
import { for } from "std/iter"

val p: Promise<Int32> = async(() => 21 * 2)
val ps: Promise<Int32>[] = [async(() => 1), async(() => 2)]
val first = await(race(ps))
match await(p)
  is Error => print("error")
  else => print(toString(await(p)))
"#);
    assert_eq!(output, vec!["42"]);
}

#[test]
fn test_promise_not_assignable_to_inner_value() {
    // Because `Promise<T>` is its own type (not erased to AnyVal), "forgot to await" is caught:
    // a `Promise<Int32>` is not assignable to `Int32`.
    let err = run_expect_err(r#"import { async } from "std/async"

val p = async(() => 1 + 1)
val n: Int32 = p
"#);
    assert!(
        err.contains("Promise") && err.contains("Int32"),
        "expected a Promise-not-assignable-to-Int32 type error, got:\n{err}"
    );
}

#[test]
fn test_async_val_capture() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { async, await } from "std/async"

val x = 10
val p = async(() => x * 2)
val result = await(p)
print(toString(result))
"#);
    assert_eq!(output, vec!["20"]);
}

#[test]
fn test_parallel_three_thunks() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { parallel } from "std/async"

val results = parallel([() => 1, () => 2, () => 3])
print(toString(results))
"#);
    assert_eq!(output, vec!["[1, 2, 3]"]);
}

#[test]
fn test_parallel_already_spawned_promises() {
    // Regression: parallel([p1, p2]) where the array elements are ALREADY-SPAWNED promises
    // (TAG_PROMISE) rather than thunk closures (TAG_FUNCTION). The runtime must dispatch on
    // each element's tag and await the existing promise instead of re-spawning it as a closure
    // (which read garbage at the closure's capture-descriptor offset → misaligned-pointer abort).
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { async, parallel } from "std/async"

val p1 = async(() => 1)
val p2 = async(() => 2)
val results = parallel([p1, p2])
print(toString(results))
"#);
    assert_eq!(output, vec!["[1, 2]"]);
}

#[test]
fn test_parallel_mixed_promises_and_thunks() {
    // parallel must handle a mixed array: some elements already-spawned promises, some thunks.
    // Order is preserved exactly.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { async, parallel } from "std/async"

val p1 = async(() => 1)
val results = parallel([p1, () => 2, async(() => 3)])
print(toString(results))
"#);
    assert_eq!(output, vec!["[1, 2, 3]"]);
}

#[test]
fn test_thread_pool_async() {
    // await now yields `T | Error` (§24.2.2), so each result is handled before arithmetic.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { async, await, threadPool } from "std/async"

val unwrap = (r: AnyVal): Int32 =>
  match r
    is Error => 0
    else => r
val pool = threadPool(2)
val p1 = async(() => 100)
val p2 = async(() => 200)
val r1 = unwrap(await(p1))
val r2 = unwrap(await(p2))
print(toString(r1 + r2))
"#);
    assert_eq!(output, vec!["300"]);
}

#[test]
fn test_worker_request_reply() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { worker, request, close } from "std/async"

val w = worker(msg => msg * 2, () => null)
val reply = request(w, 21)
close(w)
print(toString(reply))
"#);
    assert_eq!(output, vec!["42"]);
}

#[test]
fn test_worker_stateful_var_capture() {
    // A worker handler may close over `var` (§24.6.4): the accumulator state is confined to
    // the worker thread and updated across sequential requests. onShutdown sees the final state.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { worker, request, close } from "std/async"

var total = 0
val acc = worker(
  n =>
    total = total + n
    total,
  () => print("final ${toString(total)}")
)
print(toString(request(acc, 10)))
print(toString(request(acc, 5)))
print(toString(request(acc, 100)))
close(acc)
"#);
    assert_eq!(output, vec!["10", "15", "115", "final 115"]);
}

#[test]
fn test_worker_captured_var_factory_escape() {
    // Regression (spec §24.6.4 makeCounter): a worker built inside a function that RETURNS the
    // worker, whose handler closes over an outer `var`, must keep working after the building
    // frame dies. Previously the factory frame's `lin_closure_release` freed the handler env
    // (and the captured `count` cell) while the worker thread still used it → garbage reads
    // (`-2147483647`). The fix has `lin_worker_new` take an OWNING reference to the handler /
    // onClose closures, released only in `close` after the thread joins.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { worker, request, close } from "std/async"

val makeCounter = (): AnyVal =>
  var count = 0
  val onMsg = (msg: String): Int32 =>
    count = count + 1
    count
  worker(onMsg, (): Null => null)

val c = makeCounter()
print(toString(request(c, "tick")))
print(toString(request(c, "tick")))
close(c)
"#);
    assert_eq!(output, vec!["1", "2"]);
}

#[test]
fn test_worker_message_fire_and_forget() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { worker, request, message, close } from "std/async"
import { push, length } from "std/array"

var log: Int32[] = []
val w = worker(
  n =>
    push(log, n)
    length(log),
  () => null
)
message(w, 1)
message(w, 2)
val count = request(w, 3)
close(w)
print(toString(count))
"#);
    assert_eq!(output, vec!["3"]);
}

#[test]
fn test_worker_handler_fault_surfaces_error() {
    // A fault in the worker handler is caught at the boundary and returned as an Error to the
    // in-flight request (§24.6.5); the program continues.
    let output = run(r#"import { print } from "std/io"
import { worker, request, close } from "std/async"

val z = 0
val w = worker(n => n / z, () => null)
val r = request(w, 5)
close(w)
print(r["type"])
"#);
    assert_eq!(output, vec!["error"]);
}

#[test]
fn test_worker_send_after_close_errors() {
    // Sending to a closed worker yields an Error (§24.6.5), not a crash.
    let output = run(r#"import { print } from "std/io"
import { worker, request, close } from "std/async"

val w = worker(msg => msg, () => null)
close(w)
val r = request(w, 1)
print(r["type"])
"#);
    assert_eq!(output, vec!["error"]);
}

#[test]
fn test_stress_high_fanout_parallel() {
    // High fan-out: 12 capture-less thunks through parallel — exercises the spawn/join +
    // result-collection machinery. (Larger fan-out via map-returning-closures hits a
    // pre-existing higher-order limitation unrelated to async, so the array is written out.)
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { parallel } from "std/async"
import { reduce } from "std/iter"

val results = parallel([
  () => 1, () => 2, () => 3, () => 4, () => 5, () => 6,
  () => 7, () => 8, () => 9, () => 10, () => 11, () => 12
])
print(toString(reduce(results, 0, (a, b) => a + b)))
"#);
    // 1+2+...+12 = 78
    assert_eq!(output, vec!["78"]);
}

#[test]
fn test_stress_pool_many_short_tasks() {
    // Many short tasks on a small pool — exercises queue draining + worker reuse across waves.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { await, threadPool, poolAsync } from "std/async"
import { push, length } from "std/array"
import { for, range } from "std/iter"

val unwrap = (r: Int32 | Error): Int32 =>
  match r
    is Error => 0
    else => r
val pool = threadPool(3)
var promises: Promise<Int32>[] = []
range(0, 30).for(i => push(promises, pool.poolAsync(() => 1)))
var total = 0
promises.for(p => total = total + unwrap(await(p)))
print(toString(total))
"#);
    assert_eq!(output, vec!["30"]);
}

#[test]
fn test_stress_worker_churn() {
    // Worker churn: spin up and tear down many workers in a loop, each handling one request.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { worker, request, close } from "std/async"
import { for, range } from "std/iter"

var total = 0
range(0, 30).for(i =>
  val w = worker(msg => msg + 1, () => null)
  total = total + request(w, i)
  close(w)
)
print(toString(total))
"#);
    // sum of (i+1) for i in 0..29 = sum 1..30 = 465
    assert_eq!(output, vec!["465"]);
}

#[test]
fn test_await_flattens_nested_promise() {
    // §24.2.3: await auto-flattens — a thunk that itself returns a Promise resolves through.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { async, await } from "std/async"

print(toString(await(async(() => async(() => 42)))))
print(toString(await(async(() => async(() => async(() => 7))))))
"#);
    assert_eq!(output, vec!["42", "7"]);
}

#[test]
fn test_is_error_matches_faulted_thunk() {
    // §24.2.2: a thunk fault surfaces as an Error value; `is Error` discriminates it, and a
    // successful result falls through to `else`.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { async, await } from "std/async"

val z = 0
match await(async(() => 42 / z))
  is Error => print("error")
  else => print("value")

match await(async(() => 99))
  is Error => print("error")
  else => print("value")
"#);
    assert_eq!(output, vec!["error", "value"]);
}

#[test]
fn test_is_error_does_not_match_plain_object() {
    // `is Error` is a structural shape check on {type, message} — a plain object without those
    // fields must NOT match (a bare object-tag check would wrongly match any object).
    let output = run(r#"import { print } from "std/io"

val obj = { "name": "alice", "age": 30 }
match obj
  is Error => print("error")
  else => print("not error")
"#);
    assert_eq!(output, vec!["not error"]);
}

#[test]
fn test_if_is_error_narrows_then_branch_non_json_union() {
    // ADR-031: the TRUE branch of `if x is Error` must narrow a NON-AnyVal `T | Error` scrutinee to
    // `Error` (the matched member), so returning `x` where `Error` is expected type-checks. The
    // FALSE branch narrows to the value type `T`. Previously the then-branch was left as the full
    // `T | Error` union and only happened to work for AnyVal (which is universally assignable);
    // `UInt8[] | Error` / `Int32[] | Error` spuriously errored.
    let output = run(r#"import { print } from "std/io"
import { length } from "std/array"

val f = (b: UInt8[] | Error): String | Error =>
  if b is Error then b else "ok"

val g = (b: Int32[] | Error): Int32 | Error =>
  if b is Error then b else length(b)

val ok = f([1u8, 2u8, 3u8])
if ok is Error then print("ok-was-error") else print(ok)

val n = g([10, 20, 30])
if n is Error then print("n-was-error") else print("len ${n}")
"#);
    assert_eq!(output, vec!["ok", "len 3"]);
}

#[test]
fn test_frozen_concurrent_reads() {
    // A frozen array read concurrently by many threads — immortal RC makes non-atomic
    // retain/release no-ops, so reads are race-free without copying or locking.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { frozen, parallel } from "std/async"
import { length } from "std/array"

val table = frozen([10, 20, 30, 40, 50])
val results = parallel([
  () => length(table),
  () => length(table),
  () => length(table),
  () => length(table)
])
print(toString(results))
"#);
    assert_eq!(output, vec!["[5, 5, 5, 5]"]);
}

#[test]
fn test_frozen_object_read() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { frozen } from "std/async"

val config = frozen({ "host": "localhost", "port": 8080 })
print(toString(config["host"]))
print(toString(config["port"]))
"#);
    assert_eq!(output, vec!["localhost", "8080"]);
}

#[test]
fn test_frozen_survives_in_async() {
    // A frozen value is immortal and shared by reference into the thunk; both the worker and
    // the parent read it correctly.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { frozen, async, await } from "std/async"
import { length } from "std/array"

val data = frozen([1, 2, 3])
val p = async(() => length(data))
print(toString(await(p)))
print(toString(length(data)))
"#);
    assert_eq!(output, vec!["3", "3"]);
}

#[test]
fn test_shared_get_set() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { shared, get, set } from "std/async"

val s = shared([4, 5, 6])
print(toString(get(s)))
set(s, [7, 8, 9])
print(toString(get(s)))
"#);
    assert_eq!(output, vec!["[4, 5, 6]", "[7, 8, 9]"]);
}

#[test]
fn test_shared_withlock_in_place_mutate() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { shared, get, withLock } from "std/async"
import { push, length } from "std/array"

val arr = shared([1, 2, 3])
withLock(arr, a => push(a, 4))
print(toString(length(withLock(arr, a => a))))
print(toString(get(arr)))
"#);
    assert_eq!(output, vec!["4", "[1, 2, 3, 4]"]);
}

#[test]
fn test_shared_escape_returns_copy() {
    // A value returned out of withLock is a COPY: mutating it does not affect the box.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { shared, get, withLock } from "std/async"
import { push } from "std/array"

val arr = shared([1, 2, 3])
val leaked = withLock(arr, a => a)
push(leaked, 999)
print(toString(get(arr)))
"#);
    assert_eq!(output, vec!["[1, 2, 3]"]);
}

#[test]
fn test_shared_concurrent_withlock_no_lost_updates() {
    // N threads each push to a shared array under the write lock → all updates land.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { shared, get, withLock, parallel } from "std/async"
import { push, length } from "std/array"

val box = shared([])
val tasks = parallel([
  () => withLock(box, a => push(a, 1)),
  () => withLock(box, a => push(a, 1)),
  () => withLock(box, a => push(a, 1)),
  () => withLock(box, a => push(a, 1)),
  () => withLock(box, a => push(a, 1)),
  () => withLock(box, a => push(a, 1))
])
print(toString(length(get(box))))
"#);
    assert_eq!(output, vec!["6"]);
}

#[test]
fn test_shared_rejects_non_accessor_op() {
    // ADR-029: Shared<T> is accessor-only. Passing a Shared value to a non-accessor (here
    // `push`, which wants an array/AnyVal) is a compile-time type error — the Shared box never
    // auto-unwraps to its inner type or to AnyVal.
    let err = run_expect_err(r#"import { print } from "std/io"
import { shared } from "std/async"
import { push } from "std/array"

val s = shared([1, 2, 3])
push(s, 7)
print("unreachable")
"#);
    assert!(
        err.contains("Shared"),
        "expected a Shared-related type error, got:\n{err}"
    );
}

#[test]
fn test_shared_get_result_is_usable_inner_type() {
    // The flip side: get(s) yields the inner type, which IS usable with ordinary ops — proving
    // the guard blocks the Shared box itself, not values copied out of it.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { shared, get } from "std/async"
import { push, length } from "std/array"

val s = shared([1, 2, 3])
val snap = get(s)
push(snap, 4)
print(toString(length(snap)))
"#);
    assert_eq!(output, vec!["4"]);
}

#[test]
fn test_shared_payload_type_preserved() {
    // Shared<T> is a properly-typed generic handle: `get` yields the concrete payload `T`, so a
    // Shared<Int32> snapshot is directly usable as an Int32 (no widening to AnyVal). This exercises
    // the box/unbox path for a scalar payload — get(si) must unbox to a real i32 for arithmetic.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { shared, get, set, withLock } from "std/async"

val si = shared(5)
val n: Int32 = get(si)
set(si, 10)
val n2: Int32 = get(si)
val sum: Int32 = n + n2
print(toString(sum))
val r: Int32 = withLock(si, x => x * 2)
print(toString(r))
"#);
    assert_eq!(output, vec!["15", "20"]);
}

#[test]
fn test_shared_string_and_record_payload() {
    // Non-scalar payloads round-trip too: Shared<String> get yields a usable String, and
    // Shared<{record}> get yields a record whose typed field access compiles to a const-slot load.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { shared, get } from "std/async"

val ss = shared("hello")
val str: String = get(ss)
print(str)

type Point = { "x": Int32, "y": Int32 }
val sp = shared({ "x": 1, "y": 2 })
val p = get(sp)
val px: Int32 = p["x"]
print(toString(px))
"#);
    assert_eq!(output, vec!["hello", "1"]);
}

#[test]
fn test_shared_not_assignable_to_inner_value() {
    // The "forgot to get()" catch: a Shared<Int32> handle must NOT be assignable to a bare Int32.
    // The opaque box keeps its payload type but never auto-unwraps — you must read it via get().
    let err = run_expect_err(r#"import { shared } from "std/async"

val s = shared(5)
val n: Int32 = s
"#);
    assert!(
        err.contains("Int32") && err.contains("Shared<Int32>"),
        "expected a Shared<Int32>-vs-Int32 mismatch, got:\n{err}"
    );
}

#[test]
fn test_async_real_parallelism() {
    // Two thunks that each sleep 150ms run on real OS threads should overlap (~150ms wall), not
    // run sequentially (~300ms). Rather than assert against a fixed absolute bound (brittle on
    // slow/oversubscribed CI runners — the old `elapsed < 290` could spuriously fail when the
    // whole machine is loaded), we self-calibrate: measure the SEQUENTIAL cost of the same two
    // sleeps in this same process, then the PARALLEL cost, and require the parallel run to be
    // clearly faster. Both measurements inflate together under load, so the RELATIVE comparison is
    // robust while still proving genuine overlap (if threads didn't overlap, par ≈ seq and the
    // test correctly fails).
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { async, await } from "std/async"
import { sleep, now } from "std/time"

val unwrap = (r: AnyVal): Int32 =>
  match r
    is Error => 0
    else => r

// Sequential baseline: two 150ms sleeps back to back (~300ms).
val seqStart = now()
sleep(150)
sleep(150)
val seqElapsed = now() - seqStart

// Parallel: the same two sleeps as concurrent thunks (~150ms if they really overlap).
val parStart = now()
val p1 = async(() =>
  sleep(150)
  1
)
val p2 = async(() =>
  sleep(150)
  2
)
val r1 = unwrap(await(p1))
val r2 = unwrap(await(p2))
val parElapsed = now() - parStart

print(toString(r1 + r2))
// Require a clear margin: parallel must beat sequential by more than a quarter of the
// sequential cost. Real overlap roughly halves it, so this has wide headroom yet still rejects
// a non-overlapping (sequential) implementation.
if parElapsed < seqElapsed - seqElapsed / 4 then print("PARALLEL") else print("SEQUENTIAL")
"#);
    assert_eq!(output, vec!["3", "PARALLEL"],
        "two 150ms thunks should overlap (real threads), beating the sequential baseline by a clear margin");
}

#[test]
fn test_async_fault_isolation_div_by_zero() {
    // A runtime fault (division by zero) inside an async thunk must be caught at the thread
    // boundary and surface as an Error value at await — the program continues (spec §24.2.2),
    // it does not abort.
    let output = run(r#"import { print } from "std/io"
import { async, await } from "std/async"

val z = 0
val p = async(() => 42 / z)
val r = await(p)
print(r["type"])
print("continued")
"#);
    assert_eq!(output, vec!["error", "continued"]);
}

#[test]
fn test_async_fault_isolation_oob() {
    // Array out-of-bounds inside a thunk is likewise caught as an Error at await.
    let output = run(r#"import { print } from "std/io"
import { async, await } from "std/async"

val arr = [1, 2, 3]
val p = async(() => arr[99])
val r = await(p)
print(r["type"])
print("ok")
"#);
    assert_eq!(output, vec!["error", "ok"]);
}

#[test]
fn test_async_string_capture_transferred() {
    // A captured String val must be deep-copied across the thread boundary and usable there.
    let output = run(r#"import { print } from "std/io"
import { async, await } from "std/async"

val name = "world"
val p = async(() => "hello ${name}")
print(await(p))
"#);
    assert_eq!(output, vec!["hello world"]);
}

#[test]
fn test_pool_async_parallel() {
    // 4 tasks of 100ms on a 4-worker pool overlap → ~100ms wall-clock, not 400ms.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { await, threadPool, poolAsync } from "std/async"
import { sleep, now } from "std/time"

val unwrap = (r: AnyVal): Int32 =>
  match r
    is Error => 0
    else => r
val pool = threadPool(4)
val start = now()
val p1 = pool.poolAsync(() =>
  sleep(100)
  1
)
val p2 = pool.poolAsync(() =>
  sleep(100)
  2
)
val p3 = pool.poolAsync(() =>
  sleep(100)
  3
)
val p4 = pool.poolAsync(() =>
  sleep(100)
  4
)
val sum = unwrap(await(p1)) + unwrap(await(p2)) + unwrap(await(p3)) + unwrap(await(p4))
val elapsed = now() - start
print(toString(sum))
if elapsed < 300 then print("PARALLEL") else print("SLOW")
"#);
    assert_eq!(output, vec!["10", "PARALLEL"]);
}

#[test]
fn test_pool_bounds_concurrency() {
    // 4 tasks of 80ms on a 2-worker pool run in 2 waves → ~160ms (bounded), not ~80ms.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { await, threadPool, poolAsync } from "std/async"
import { sleep, now } from "std/time"

val unwrap = (r: AnyVal): Int32 =>
  match r
    is Error => 0
    else => r
val pool = threadPool(2)
val start = now()
val a = pool.poolAsync(() =>
  sleep(80)
  1
)
val b = pool.poolAsync(() =>
  sleep(80)
  1
)
val c = pool.poolAsync(() =>
  sleep(80)
  1
)
val d = pool.poolAsync(() =>
  sleep(80)
  1
)
val total = unwrap(await(a)) + unwrap(await(b)) + unwrap(await(c)) + unwrap(await(d))
val elapsed = now() - start
print(toString(total))
if elapsed >= 140 then print("BOUNDED") else print("UNBOUNDED")
"#);
    assert_eq!(output, vec!["4", "BOUNDED"]);
}

#[test]
fn test_pool_async_fault_isolation() {
    let output = run(r#"import { print } from "std/io"
import { await, threadPool, poolAsync } from "std/async"

val pool = threadPool(2)
val z = 0
val p = pool.poolAsync(() => 1 / z)
val r = await(p)
print(r["type"])
"#);
    assert_eq!(output, vec!["error"]);
}

#[test]
fn test_race_first_wins() {
    let output = run(r#"import { print } from "std/io"
import { async, await, race } from "std/async"
import { sleep } from "std/time"

val winner = await(race([
  async(() =>
    sleep(200)
    "slow"
  ),
  async(() =>
    sleep(10)
    "fast"
  )
]))
print(winner)
"#);
    assert_eq!(output, vec!["fast"]);
}

#[test]
fn test_timeout_expires_to_null() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { async, await, timeout } from "std/async"
import { sleep } from "std/time"

val slow = async(() =>
  sleep(300)
  "done"
)
val r = await(timeout(slow, 30))
print(toString(r))
"#);
    assert_eq!(output, vec!["null"]);
}

#[test]
fn test_timeout_expires_when_thunk_captures_function_param() {
    // Regression: a thunk whose body calls a captured FUNCTION-VALUED parameter (`runner`) must
    // spawn a real worker just like a thunk calling a top-level function. Previously the captured
    // closure made the env "non-transferable" and the runtime ran the thunk INLINE on the calling
    // thread, so `timeout` never tripped (the 300ms work blocked the 30ms budget to completion).
    // The fix recursively deep-copies the captured closure (transfer.rs::clone_closure), so BOTH
    // forms below run on a worker and time out to `null`. 300ms-vs-30ms is a ~10x margin (matching
    // the existing timeout tests CI already runs).
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { async, await, timeout } from "std/async"
import { sleep } from "std/time"

val slowFn = (): Int32 =>
  sleep(300)
  42

val viaParam = (runner: () => Int32): AnyVal =>
  val p = async(() => runner())
  await(timeout(p, 30))

val viaTopLevel = (): AnyVal =>
  val p = async(() => slowFn())
  await(timeout(p, 30))

print(toString(viaParam(slowFn)))
print(toString(viaTopLevel()))
"#);
    // Both forms wrap 300ms of work in a 30ms timeout; both must abandon the work and yield null.
    assert_eq!(output, vec!["null", "null"],
        "captured-function-param thunk must spawn a worker (like the top-level form) so timeout trips");
}

#[test]
fn test_async_captured_function_param_correct_result() {
    // Companion to the timeout regression: when NOT timed out, the worker that runs a thunk
    // capturing a function-valued parameter must produce the CORRECT result — proving the
    // recursive closure deep-copy (including a closure that itself captures heap data) is sound,
    // not just that it spawns. `makeAdder(n)` returns a closure capturing the scalar `n`.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { async, await } from "std/async"

val makeAdder = (n: Int32): () => Int32 => () => n + 100

val viaParam = (runner: () => Int32): AnyVal =>
  val p = async(() => runner())
  await(p)

print(toString(viaParam(makeAdder(5))))
print(toString(viaParam(makeAdder(42))))
"#);
    assert_eq!(output, vec!["105", "142"]);
}

#[test]
fn test_timeout_completes_in_time() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { async, await, timeout } from "std/async"

val quick = async(() => 99)
val r = await(timeout(quick, 5000))
print(toString(r))
"#);
    assert_eq!(output, vec!["99"]);
}

#[test]
fn test_retry_succeeds_first_try() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { async, await, retry } from "std/async"

val p = retry(() => 7, 3)
print(toString(await(p)))
"#);
    assert_eq!(output, vec!["7"]);
}

#[test]
fn test_retry_all_fail_returns_error() {
    let output = run(r#"import { print } from "std/io"
import { async, await, retry } from "std/async"

val z = 0
val p = retry(() => 1 / z, 3)
val r = await(p)
print(r["type"])
"#);
    assert_eq!(output, vec!["error"]);
}

#[test]
fn test_parallel_preserves_order_with_sleep() {
    // Tasks finish in reverse order of submission, but results must stay in submission order.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { parallel } from "std/async"
import { sleep } from "std/time"

val rs = parallel([
  () =>
    sleep(120)
    1,
  () =>
    sleep(60)
    2,
  () =>
    sleep(10)
    3
])
print(toString(rs))
"#);
    assert_eq!(output, vec!["[1, 2, 3]"]);
}

#[test]
fn test_async_captures_function_value_runs() {
    // A thunk capturing a function value is deep-copied (the captured closure is recursively
    // cloned, transfer.rs::clone_closure) and run on a real worker thread; the result is correct.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { async, await } from "std/async"

val double = (x: Int32): Int32 => x * 2
val p = async(() => double(21))
print(toString(await(p)))
"#);
    assert_eq!(output, vec!["42"]);
}

#[test]
fn test_iterator_restart() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { iter } from "std/iter"
import { for } from "std/iter"

val counter = iter(
  () => 0,
  i => i < 3,
  i => i + 1,
  i => i
)
counter.for(i => print(toString(i)))
counter.for(i => print(toString(i)))
"#);
    assert_eq!(output, vec!["0", "1", "2", "0", "1", "2"],
        "Iterator should restart from initial state on second .for call");
}

#[test]
fn test_fs_write_read_roundtrip() {
    let tmp = std::env::temp_dir().join(format!("lin_ctest_rw_{}.txt", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let path = tmp.display().to_string();
    let output = run(&format!(r#"import {{ print }} from "std/io"

import {{ writeFile, readFile }} from "std/fs"
writeFile("{path}", "hello from lin")
val content = readFile("{path}")
print(content)
"#));
    let _ = fs::remove_file(&tmp);
    assert_eq!(output, vec!["hello from lin"]);
}

// Stage 3 (streams): open a file as a byte Stream<UInt8[]>, pull chunks until EOF, and count
// the bytes read. Exercises lin_fs_open → lin_stream_read end-to-end (open + read bytes), the
// TAG_STREAM box flowing through a `val`, and the EOF (Null) / chunk discrimination.
#[test]
fn test_stream_open_read_bytes_end_to_end() {
    let tmp = std::env::temp_dir().join(format!("lin_ctest_stream_{}.txt", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let path = tmp.display().to_string();
    // 13 bytes of content.
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ length }} from "std/array"
import {{ writeFile, openRead, readChunk }} from "std/fs"

val countBytes = (s, acc: Int32): Int32 =>
  val chunk = readChunk(s)
  match chunk
    is Null => acc
    is Error => acc
    else => countBytes(s, acc + length(chunk))

writeFile("{path}", "hello, stream")
val stream = openRead("{path}")
val total = match stream
  is Error => 0 - 1
  else => countBytes(stream, 0)
print(toString(total))
"#));
    let _ = fs::remove_file(&tmp);
    assert_eq!(output, vec!["13"]);
}

// Stage 4 (streams): the worked CSV example from the design brief. readStream → lines → filter
// → map → writeLines → drain, run on the calling thread. Asserts the exact transformed output
// file (`a,b,c` -> `"a"|"b"|"c"`), plus lazy adapters + in-band drain + sink working together.
#[test]
fn test_stream_csv_pipeline_drain() {
    let indir = std::env::temp_dir();
    let inp = indir.join(format!("lin_ctest_csvin_{}.csv", std::process::id()));
    let outp = indir.join(format!("lin_ctest_csvout_{}.csv", std::process::id()));
    let _ = fs::remove_file(&inp);
    let _ = fs::remove_file(&outp);
    fs::write(&inp, "a,b,c\nx,y,z\n\nfoo,bar,baz").unwrap();
    let inp_s = inp.display().to_string();
    let outp_s = outp.display().to_string();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ readStream, lines, writeLines, drain }} from "std/stream"
import {{ split, join }} from "std/string"
import {{ length }} from "std/array"
import {{ map as amap, map, filter }} from "std/iter"

val notEmpty = (line: String): Boolean => length(line) > 0
val quoteFields = (line: String): String =>
  amap(split(line, ","), f => "\"${{f}}\"").join("|")

readStream("{inp_s}").lines().filter(notEmpty).map(quoteFields).writeLines("{outp_s}").drain()
print("ok")
"#));
    let written = fs::read_to_string(&outp).unwrap_or_default();
    let _ = fs::remove_file(&inp);
    let _ = fs::remove_file(&outp);
    assert_eq!(output, vec!["ok"]);
    assert_eq!(written, "\"a\"|\"b\"|\"c\"\n\"x\"|\"y\"|\"z\"\n\"foo\"|\"bar\"|\"baz\"\n");
}

// std/iter unification (Stage 3/4): a lazy stream chain using the NET-NEW combinators that now
// dispatch to the `lin_stream_*` backend on a stream receiver. drop + take + reduce on a 5-line
// file: drop 1 → take 2 → fold count = 2.
#[test]
fn test_stream_iter_drop_take_reduce() {
    let inp = std::env::temp_dir().join(format!("lin_ctest_dtr_{}.txt", std::process::id()));
    let _ = fs::remove_file(&inp);
    fs::write(&inp, "l0\nl1\nl2\nl3\nl4\n").unwrap();
    let inp_s = inp.display().to_string();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ readStream, lines }} from "std/stream"
import {{ drop, take, reduce }} from "std/iter"

val r = readStream("{inp_s}")
val out = match r
  is Error => "open-error"
  else =>
    val total = r.lines().drop(1).take(2).reduce(0, (acc, line) => acc + 1)
    match total
      is Error => "drive-error"
      else => "count=${{total}}"
print(out)
"#));
    let _ = fs::remove_file(&inp);
    assert_eq!(output, vec!["count=2"]);
}

// std/iter unification (Stage 3/4): flatMap over a stream (each line split into fields, flattened),
// counted via reduce. "a,b,c\nd,e\nf" → 6 fields.
#[test]
fn test_stream_iter_flat_map() {
    let inp = std::env::temp_dir().join(format!("lin_ctest_fm_{}.txt", std::process::id()));
    let _ = fs::remove_file(&inp);
    fs::write(&inp, "a,b,c\nd,e\nf").unwrap();
    let inp_s = inp.display().to_string();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ readStream, lines }} from "std/stream"
import {{ flatMap, reduce }} from "std/iter"
import {{ split }} from "std/string"

val r = readStream("{inp_s}")
val out = match r
  is Error => "open-error"
  else =>
    val total = r.lines().flatMap(l => split(l, ",")).reduce(0, (a, f) => a + 1)
    match total
      is Error => "drive-error"
      else => "fields=${{total}}"
print(out)
"#));
    let _ = fs::remove_file(&inp);
    assert_eq!(output, vec!["fields=6"]);
}

// std/iter unification (Stage 3/4): takeWhile + dropWhile over a stream. Lines "aa\nbb\nc\ndd":
// takeWhile(len==2) → 2 items; dropWhile(len==2) → 2 items (c, dd).
#[test]
fn test_stream_iter_take_while_drop_while() {
    let inp = std::env::temp_dir().join(format!("lin_ctest_twdw_{}.txt", std::process::id()));
    let _ = fs::remove_file(&inp);
    fs::write(&inp, "aa\nbb\nc\ndd").unwrap();
    let inp_s = inp.display().to_string();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ readStream, lines }} from "std/stream"
import {{ takeWhile, dropWhile, reduce }} from "std/iter"
import {{ length }} from "std/array"

val tw = match readStream("{inp_s}")
  is Error => -1
  else => unwrapTw(readStream("{inp_s}").lines().takeWhile(l => length(l) == 2).reduce(0, (a, l) => a + 1))
val dw = match readStream("{inp_s}")
  is Error => -1
  else => unwrapTw(readStream("{inp_s}").lines().dropWhile(l => length(l) == 2).reduce(0, (a, l) => a + 1))
print("tw=${{tw}} dw=${{dw}}")

val unwrapTw = (r: AnyVal): Int32 =>
  match r
    is Error => -1
    else => r
"#));
    let _ = fs::remove_file(&inp);
    assert_eq!(output, vec!["tw=2 dw=2"]);
}

// std/iter unification (Stage 3/4): concat two streams (3 lines each = 6), counted via reduce.
#[test]
fn test_stream_iter_concat() {
    let inp = std::env::temp_dir().join(format!("lin_ctest_cat_{}.txt", std::process::id()));
    let _ = fs::remove_file(&inp);
    fs::write(&inp, "a\nb\nc").unwrap();
    let inp_s = inp.display().to_string();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ readStream, lines }} from "std/stream"
import {{ concat, reduce }} from "std/iter"

val a = readStream("{inp_s}")
val b = readStream("{inp_s}")
val out = match a
  is Error => "ea"
  else => match b
    is Error => "eb"
    else =>
      val n = a.lines().concat(b.lines()).reduce(0, (acc, l) => acc + 1)
      match n
        is Error => "drive-error"
        else => "n=${{n}}"
print(out)
"#));
    let _ = fs::remove_file(&inp);
    assert_eq!(output, vec!["n=6"]);
}

// std/iter unification (Stage 3/4): find/some/every terminals over a stream.
#[test]
fn test_stream_iter_find_some_every() {
    let inp = std::env::temp_dir().join(format!("lin_ctest_fse_{}.txt", std::process::id()));
    let _ = fs::remove_file(&inp);
    fs::write(&inp, "x\ny\nz").unwrap();
    let inp_s = inp.display().to_string();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ readStream, lines }} from "std/stream"
import {{ find, some, every }} from "std/iter"
import {{ length }} from "std/array"

val f = match readStream("{inp_s}")
  is Error => "e"
  else => unwrapS(readStream("{inp_s}").lines().find(l => l == "y"))
val s = match readStream("{inp_s}")
  is Error => false
  else => unwrapB(readStream("{inp_s}").lines().some(l => l == "z"))
val e = match readStream("{inp_s}")
  is Error => false
  else => unwrapB(readStream("{inp_s}").lines().every(l => length(l) == 1))
print("find=${{f}} some=${{s}} every=${{e}}")

val unwrapS = (r: AnyVal): String =>
  match r
    is Error => "err"
    else => r
val unwrapB = (r: AnyVal): Boolean =>
  match r
    is Error => false
    else => r
"#));
    let _ = fs::remove_file(&inp);
    assert_eq!(output, vec!["find=y some=true every=true"]);
}

// NON-REGRESSION: the SAME net-new combinators over an ARRAY (not a stream) must stay eager and
// unchanged — drop/take/flatMap/takeWhile/dropWhile/find/some/every/concat on a plain array.
#[test]
fn test_iter_array_combinators_unchanged() {
    let output = run(r#"import { print } from "std/io"
import { drop, take, flatMap, takeWhile, dropWhile, find, some, every, concat, reduce } from "std/iter"
import { length } from "std/array"

val xs = [1, 2, 3, 4, 5]
print("drop=${length(xs.drop(2))}")        // [3,4,5] -> 3
print("take=${length(xs.take(2))}")        // [1,2] -> 2
print("flatMap=${length(xs.flatMap(x => [x, x]))}")  // 10
print("takeWhile=${length(xs.takeWhile(x => x < 3))}") // [1,2] -> 2
print("dropWhile=${length(xs.dropWhile(x => x < 3))}") // [3,4,5] -> 3
print("find=${xs.find(x => x == 4)}")      // 4
print("some=${xs.some(x => x == 5)}")      // true
print("every=${xs.every(x => x > 0)}")     // true
print("concat=${length(xs.concat([6, 7]))}") // 7
print("reduce=${xs.reduce(0, (a, x) => a + x)}") // 15
"#);
    assert_eq!(output, vec![
        "drop=3", "take=2", "flatMap=10", "takeWhile=2", "dropWhile=3",
        "find=4", "some=true", "every=true", "concat=7", "reduce=15",
    ]);
}

// Stage 6 (streams): affine use-after-move + placement restriction (negative cases).
#[test]
fn test_stream_use_after_move_rejected() {
    let err = run_expect_err(r#"import { readStream, lines, readText } from "std/stream"
import { writeFile } from "std/fs"
writeFile("/tmp/lin_uam.txt", "x")
val s = readStream("/tmp/lin_uam.txt")
val a = s.lines()
val b = s.readText()
"#);
    assert!(
        err.contains("used more than once") || err.contains("affine"),
        "expected a use-after-move error, got:\n{err}"
    );
}

#[test]
fn test_stream_in_var_rejected() {
    let err = run_expect_err(r#"import { readStream } from "std/stream"
import { writeFile } from "std/fs"
writeFile("/tmp/lin_sv.txt", "x")
var s = readStream("/tmp/lin_sv.txt")
"#);
    assert!(
        err.contains("cannot be stored in a `var`") || err.contains("Stream"),
        "expected a var-placement error, got:\n{err}"
    );
}

#[test]
fn test_stream_in_object_field_rejected() {
    let err = run_expect_err(r#"import { readStream } from "std/stream"
import { writeFile } from "std/fs"
writeFile("/tmp/lin_so.txt", "x")
val s = readStream("/tmp/lin_so.txt")
val o = { "s": s }
"#);
    assert!(
        err.contains("object field") || err.contains("Stream"),
        "expected an object-field placement error, got:\n{err}"
    );
}

// Positive: a stream used exactly once (bound, then consumed by one terminal) type-checks + runs.
#[test]
fn test_stream_single_use_ok() {
    let tmp = std::env::temp_dir().join(format!("lin_ctest_single_{}.txt", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let path = tmp.display().to_string();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ readStream, readText }} from "std/stream"
import {{ writeFile }} from "std/fs"

writeFile("{path}", "hello affine")
val s = readStream("{path}")
val text = s.readText()
print(text)
"#));
    let _ = fs::remove_file(&tmp);
    assert_eq!(output, vec!["hello affine"]);
}

// Stage 8 (streams): `.promise()` drives a pipeline on a WORKER thread; two run concurrently and
// are awaited. Real cross-thread move (the stream is moved onto each worker). Verifies both
// outputs are correct (the workers ran the pipelines and closed their fds).
#[test]
fn test_stream_promise_concurrent() {
    let dir = std::env::temp_dir();
    let in1 = dir.join(format!("lin_ctest_pc_in1_{}.txt", std::process::id()));
    let in2 = dir.join(format!("lin_ctest_pc_in2_{}.txt", std::process::id()));
    let out1 = dir.join(format!("lin_ctest_pc_out1_{}.txt", std::process::id()));
    let out2 = dir.join(format!("lin_ctest_pc_out2_{}.txt", std::process::id()));
    for p in [&in1, &in2, &out1, &out2] { let _ = fs::remove_file(p); }
    fs::write(&in1, "a\nb").unwrap();
    fs::write(&in2, "c\nd").unwrap();
    // Drive both stream pipelines concurrently with the real `parallel([...])` primitive (each
    // `.promise()` is an already-spawned worker that moved its stream across the thread boundary;
    // `parallel` awaits both, preserving order). Exercises the parallel-over-promises path fixed
    // on master alongside the cross-thread stream move.
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ readStream, lines, writeLines, promise }} from "std/stream"
import {{ parallel }} from "std/async"
import {{ readFile }} from "std/fs"

val p1 = readStream("{i1}").lines().writeLines("{o1}").promise()
val p2 = readStream("{i2}").lines().writeLines("{o2}").promise()
val results = parallel([p1, p2])
print(readFile("{o1}"))
print(readFile("{o2}"))
"#, i1 = in1.display(), i2 = in2.display(), o1 = out1.display(), o2 = out2.display()));
    for p in [&in1, &in2, &out1, &out2] { let _ = fs::remove_file(p); }
    assert_eq!(output, vec!["a", "b", "c", "d"]);
}

// Stage 8 (streams): a fault inside a transform on a `.promise()` worker is caught at the async
// boundary and surfaces as an `Error` at `await` (ADR-045 / §32.2.2), NOT a crash.
#[test]
fn test_stream_promise_fault_isolation() {
    let tmp = std::env::temp_dir().join(format!("lin_ctest_pf_{}.txt", std::process::id()));
    let out = std::env::temp_dir().join(format!("lin_ctest_pfo_{}.txt", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let _ = fs::remove_file(&out);
    fs::write(&tmp, "a\nb\nc").unwrap();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ readStream, lines, writeLines, promise }} from "std/stream"
import {{ map }} from "std/iter"
import {{ await }} from "std/async"

val boom = (line: AnyVal): AnyVal =>
  val arr = [1, 2]
  arr[100]

val p = readStream("{inp}").lines().map(boom).writeLines("{outp}").promise()
val r = await(p)
val status = match r
  is Error => "caught error"
  else => "ok"
print(status)
"#, inp = tmp.display(), outp = out.display()));
    let _ = fs::remove_file(&tmp);
    let _ = fs::remove_file(&out);
    assert_eq!(output, vec!["caught error"]);
}

#[test]
fn test_fs_append_file() {
    let tmp = std::env::temp_dir().join(format!("lin_ctest_append_{}.txt", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let path = tmp.display().to_string();
    let output = run(&format!(r#"import {{ print }} from "std/io"

import {{ appendFile, readFile }} from "std/fs"
appendFile("{path}", "line1\n")
appendFile("{path}", "line2\n")
val content = readFile("{path}")
print(content)
"#));
    let _ = fs::remove_file(&tmp);
    assert_eq!(output, vec!["line1", "line2"]);
}

#[test]
fn test_fs_exists() {
    let tmp = std::env::temp_dir().join(format!("lin_ctest_exists_{}.txt", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let path = tmp.display().to_string();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"

import {{ writeFile, exists }} from "std/fs"
print(toString(exists("{path}")))
writeFile("{path}", "hi")
print(toString(exists("{path}")))
"#));
    let _ = fs::remove_file(&tmp);
    assert_eq!(output, vec!["false", "true"]);
}

#[test]
fn test_fs_read_missing_file_returns_error() {
    let output = run(r#"import { print } from "std/io"

import { readFile } from "std/fs"
val result = readFile("/nonexistent/path/that/does/not/exist.lin")
print(result["type"])
"#);
    assert_eq!(output, vec!["error"]);
}

#[test]
fn test_fs_read_lines() {
    let tmp = std::env::temp_dir().join(format!("lin_ctest_lines_{}.txt", std::process::id()));
    let _ = fs::remove_file(&tmp);
    fs::write(&tmp, "alpha\nbeta\ngamma\n").unwrap();
    let path = tmp.display().to_string();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ length }} from "std/array"

import {{ readLines }} from "std/fs"
val lines = readLines("{path}")
print(toString(length(lines)))
print(lines[0])
print(lines[2])
"#));
    let _ = fs::remove_file(&tmp);
    assert_eq!(output, vec!["3", "alpha", "gamma"]);
}

#[test]
fn test_fs_read_write_json() {
    let tmp = std::env::temp_dir().join(format!("lin_ctest_json_{}.json", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let path = tmp.display().to_string();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"

import {{ writeJson, readJson }} from "std/fs"
val data = {{ "name": "Lin", "version": 1 }}
writeJson("{path}", data, {{}})
val loaded = readJson("{path}")
print(loaded["name"])
print(toString(loaded["version"]))
"#));
    let _ = fs::remove_file(&tmp);
    assert_eq!(output, vec!["Lin", "1"]);
}

#[test]
fn test_yaml_parse_and_stringify() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { parse, stringify } from "std/yaml"

val doc = parse("name: Bob\nage: 30\n")
print(doc["name"])
print(toString(doc["age"]))
val back = parse(stringify(doc))
print(back["name"])
"#);
    assert_eq!(output, vec!["Bob", "30", "Bob"]);
}

#[test]
fn test_jq_query() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { jq, jqFirst } from "std/jq"

val data = { "users": [{ "name": "Ada", "age": 36 }, { "name": "Bob", "age": 30 }] }
print(toString(jq(data, ".users[] | .name")))
print(toString(jq(data, ".users | map(.age) | add")))
print(toString(jqFirst(data, ".users[] | .name")))
"#);
    assert_eq!(output, vec![r#"["Ada", "Bob"]"#, "[66]", "Ada"]);
}

#[test]
fn test_fs_is_file() {
    let tmp = std::env::temp_dir().join(format!("lin_ctest_isfile_{}.txt", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let path = tmp.display().to_string();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"

import {{ writeFile, isFile, isDir }} from "std/fs"
print(toString(isFile("{path}")))
print(toString(isDir("{path}")))
writeFile("{path}", "hello")
print(toString(isFile("{path}")))
print(toString(isDir("{path}")))
"#));
    let _ = fs::remove_file(&tmp);
    assert_eq!(output, vec!["false", "false", "true", "false"]);
}

#[test]
fn test_fs_is_dir() {
    let tmp_dir = std::env::temp_dir();
    let dir_path = tmp_dir.display().to_string();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"

import {{ isFile, isDir }} from "std/fs"
print(toString(isDir("{dir_path}")))
print(toString(isFile("{dir_path}")))
"#));
    assert_eq!(output, vec!["true", "false"]);
}

#[test]
fn test_fs_stat() {
    let tmp = std::env::temp_dir().join(format!("lin_ctest_stat_{}.txt", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let path = tmp.display().to_string();
    fs::write(&tmp, "hello lin").unwrap();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"

import {{ stat }} from "std/fs"
val s = stat("{path}")
print(toString(s["size"]))
print(toString(s["isFile"]))
print(toString(s["isDir"]))
"#));
    let _ = fs::remove_file(&tmp);
    assert_eq!(output, vec!["9", "true", "false"]);
}

#[test]
fn test_fs_stat_missing_returns_error() {
    let output = run(r#"import { print } from "std/io"

import { stat } from "std/fs"
val s = stat("/nonexistent/path/that/does/not/exist.txt")
print(s["type"])
"#);
    assert_eq!(output, vec!["error"]);
}

#[test]
fn test_fs_list_dir() {
    let tmp_dir = std::env::temp_dir().join(format!("lin_ctest_listdir_{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp_dir);
    fs::create_dir_all(&tmp_dir).unwrap();
    fs::write(tmp_dir.join("a.txt"), "").unwrap();
    fs::write(tmp_dir.join("b.txt"), "").unwrap();
    let dir_path = tmp_dir.display().to_string();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ length }} from "std/array"

import {{ ls }} from "std/fs"
val entries = ls("{dir_path}", {{}})
print(toString(length(entries)))
"#));
    let _ = fs::remove_dir_all(&tmp_dir);
    assert_eq!(output, vec!["2"]);
}

#[test]
fn test_fs_list_dir_missing_returns_error() {
    let output = run(r#"import { print } from "std/io"

import { ls } from "std/fs"
val result = ls("/nonexistent/path/that/does/not/exist", {})
print(result["type"])
"#);
    assert_eq!(output, vec!["error"]);
}

#[test]
fn test_fs_mkdir() {
    let tmp_dir = std::env::temp_dir().join(format!("lin_ctest_mkdir_{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp_dir);
    let dir_path = tmp_dir.display().to_string();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"

import {{ mkdir, isDir }} from "std/fs"
val before = isDir("{dir_path}")
mkdir("{dir_path}", {{}})
val after = isDir("{dir_path}")
print(toString(before))
print(toString(after))
"#));
    let _ = fs::remove_dir_all(&tmp_dir);
    assert_eq!(output, vec!["false", "true"]);
}

#[test]
fn test_fs_mkdir_all() {
    let root = std::env::temp_dir().join(format!("lin_ctest_mkdirall_{}", std::process::id()));
    let tmp_dir = root.join("a").join("b");
    let _ = fs::remove_dir_all(&root);
    let dir_path = tmp_dir.display().to_string();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"

import {{ mkdir, isDir }} from "std/fs"
mkdir("{dir_path}", {{ "parents": true }})
print(toString(isDir("{dir_path}")))
"#));
    let _ = fs::remove_dir_all(&root);
    assert_eq!(output, vec!["true"]);
}

#[test]
fn test_fs_delete_file() {
    let tmp = std::env::temp_dir().join(format!("lin_ctest_deletefile_{}.txt", std::process::id()));
    fs::write(&tmp, "hello").unwrap();
    let path = tmp.display().to_string();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"

import {{ rm, exists }} from "std/fs"
val before = exists("{path}")
rm("{path}", {{}})
val after = exists("{path}")
print(toString(before))
print(toString(after))
"#));
    let _ = fs::remove_file(&tmp);
    assert_eq!(output, vec!["true", "false"]);
}

#[test]
fn test_fs_delete_file_missing_returns_error() {
    let output = run(r#"import { print } from "std/io"

import { rm } from "std/fs"
val result = rm("/nonexistent/path/that/does/not/exist.txt", {})
print(result["type"])
"#);
    assert_eq!(output, vec!["error"]);
}

#[test]
fn test_fs_rename() {
    let src = std::env::temp_dir().join(format!("lin_ctest_rename_src_{}.txt", std::process::id()));
    let dst = std::env::temp_dir().join(format!("lin_ctest_rename_dst_{}.txt", std::process::id()));
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&dst);
    fs::write(&src, "hello rename").unwrap();
    let src_path = src.display().to_string();
    let dst_path = dst.display().to_string();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"

import {{ mv, exists, readFile }} from "std/fs"
mv("{src_path}", "{dst_path}")
print(toString(exists("{src_path}")))
print(toString(exists("{dst_path}")))
print(readFile("{dst_path}"))
"#));
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&dst);
    assert_eq!(output, vec!["false", "true", "hello rename"]);
}

#[test]
fn test_server_path_match() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

import { matchPath } from "std/http"
val m = matchPath("/users/42/posts/7", "/users/:id/posts/:postId")
print(m["id"])
print(m["postId"])
val none = matchPath("/products/5", "/users/:id")
print(toString(none))
"#);
    assert_eq!(output, vec!["42", "7", "null"]);
}

/// End-to-end test of the real HTTP `serve` intrinsic (spec §25.5). `serve` blocks
/// forever, so the compiled program runs as a background child process; we poll-connect
/// a raw TCP client, send an HTTP/1.1 request, and assert the wire response. The child is
/// always killed via a guard so a hung server never leaks past the test.
#[test]
fn test_serve_real_http() {
    use std::io::Read;
    use std::net::TcpStream;
    use std::time::{Duration, Instant};

    let lin_bin = lin_bin();
    if !lin_bin.exists() {
        eprintln!("SKIP test_serve_real_http: lin binary not built");
        return;
    }

    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    // Use a project dir with a SEPARATE router module: `main.lin` imports `router` and calls
    // `router.serve(port)`. This is the real example's shape and also guards the imported-fn-
    // as-value lowering fix — passing an imported function value to serve (see
    // test_imported_fn_passed_as_value).
    let dir = ws.join(format!("target/lin_serve_{}", id));
    let _ = fs::create_dir_all(&dir);
    let src_path = dir.join("main.lin");
    let bin_path = dir.join("server_bin");
    // A high, fixed-ish port derived from the test id to avoid collisions across the suite.
    let port: u16 = 41_900 + (id as u16 % 50);

    fs::write(dir.join("router.lin"),
        r#"import { json, text, matchPath } from "std/http"

export val router = (req: AnyVal): AnyVal =>
  match req["path"]
    is "/" => text(200, "hello from lin")
    is path when matchPath(path, "/users/:id") != null =>
      val m = matchPath(path, "/users/:id")
      json(200, { "id": m["id"] })
    else => json(404, { "error": "not found" })
"#).unwrap();

    let source = format!(
        r#"import {{ serve }} from "std/http"
import {{ router }} from "./router"

router.serve({port})
"#
    );
    fs::write(&src_path, &source).unwrap();

    let compile = Command::new(&lin_bin)
        .args(["build", src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary");
    assert!(
        compile.status.success(),
        "serve program compilation failed:\nstderr: {}\nsource:\n{}",
        String::from_utf8_lossy(&compile.stderr),
        source
    );

    // Guard that always kills the spawned server and removes the project dir on drop.
    struct ChildGuard {
        child: std::process::Child,
        dir: PathBuf,
    }
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    let child = Command::new(&bin_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn serve binary");
    let mut guard = ChildGuard { child, dir: dir.clone() };

    // Poll-connect until the server is accepting (or time out).
    let addr = format!("127.0.0.1:{}", port);
    let deadline = Instant::now() + Duration::from_secs(10);
    let request = |path: &str| -> String {
        let mut last_err = String::new();
        while Instant::now() < deadline {
            match TcpStream::connect(&addr) {
                Ok(mut stream) => {
                    let req = format!("GET {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n", path);
                    stream.write_all(req.as_bytes()).unwrap();
                    let mut resp = String::new();
                    stream.read_to_string(&mut resp).unwrap();
                    return resp;
                }
                Err(e) => {
                    last_err = e.to_string();
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        panic!("server never came up on {}: {}", addr, last_err);
    };

    let root = request("/");
    assert!(root.starts_with("HTTP/1.1 200 OK"), "GET / status: {}", root);
    assert!(root.contains("hello from lin"), "GET / body: {}", root);

    let user = request("/users/42");
    assert!(user.starts_with("HTTP/1.1 200 OK"), "GET /users/42 status: {}", user);
    assert!(user.contains("\"id\": \"42\""), "GET /users/42 body: {}", user);

    let missing = request("/nope");
    assert!(missing.starts_with("HTTP/1.1 404"), "GET /nope status: {}", missing);

    // Explicit kill (the guard would also do this on drop).
    let _ = guard.child.kill();
    let _ = guard.child.wait();
}

#[test]
fn test_server_json_helper() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

import { json } from "std/http"
val resp = json(200, "hello")
print(toString(resp["status"]))
print(resp["headers"]["Content-Type"])
"#);
    assert_eq!(output, vec!["200", "application/json"]);
}

#[test]
fn test_server_text_helper() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

import { text } from "std/http"
val resp = text(200, "hello world")
print(toString(resp["status"]))
print(resp["body"])
"#);
    assert_eq!(output, vec!["200", "hello world"]);
}

#[test]
fn test_server_parse_body() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

import { parseBody } from "std/http"
val req = { "method": "POST", "path": "/", "query": "", "headers": {}, "body": "{\"x\": 1}" }
val body = parseBody(req)
print(toString(body["x"]))
"#);
    assert_eq!(output, vec!["1"]);
}

#[test]
fn test_mutual_recursion_via_forward_decl() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val isEven = (n: Int32): Boolean =>
  if n == 0 then true
  else isOdd(n - 1)

val isOdd = (n: Int32): Boolean =>
  if n == 0 then false
  else isEven(n - 1)

print(toString(isEven(4)))
print(toString(isOdd(3)))
"#);
    assert_eq!(output, vec!["true", "true"]);
}

// Two MUTUALLY-recursive functions that RETURN A RECORD used to segfault: the first-checked
// function's `if`-merge result inferred as a spurious `Union([{…}, Named("R")])` (boxed) because a
// call to the not-yet-checked sibling carried the UNRESOLVED `Named("R")` alias from the forward
// declaration, while the literal branch carried the structural sealed `{…}`. The function then
// returned that boxed-union repr, but the sibling actually returns the SEALED PACKED struct → the
// return-coerce read a packed-struct pointer as a boxed TaggedVal (`lin_unbox_ptr`) → garbage
// pointer → SIGSEGV. Fix: expand `Named` aliases in a call's resolved return type against the
// now-resolved env so both sides agree on the packed sealed representation. Self-recursion never
// hit this (it TCO's — the recursive call is a back-edge, never a record-returning `call`).
#[test]
fn test_mutual_recursion_returning_sealed_record() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

type R = { "v": Int32 }
val f = (n: Int32): R =>
  if n <= 0 then { "v": 0 } else g(n - 1)
val g = (n: Int32): R =>
  if n <= 0 then { "v": 1 } else f(n - 1)
print(toString(f(5)["v"]))
print(toString(g(5)["v"]))
"#);
    // f(5)→g(4)→f(3)→g(2)→f(1)→g(0)={v:1}; g(5)→f(4)→…→f(0)={v:0}.
    assert_eq!(output, vec!["1", "0"]);
}

// Regression: a combinator result whose lambda returns an UNSEALED object literal (so the runtime
// value is a BOXED `Object[]`) bound to an explicit PACKED sealed-scalar-array annotation (`Pt[]`),
// then read through a representation-dispatched op. The annotation made downstream index/`.for`/
// field-read emit PACKED const-offset reads, but the bound value was a boxed array → the boxed
// element pointers were mis-read as inline packed struct bytes → garbage (printed `7 7`, a value no
// field holds). Fix: `lower::type_repr_differs` now detects the packed-sealed-array-vs-boxed-array
// representation disagreement at the binding boundary and emits a `Coerce`, which codegen's
// `sealed_array_project_from` materializes into a genuine packed buffer (matching the annotation).
#[test]
fn test_combinator_boxed_result_bound_to_packed_sealed_array() {
    // map → for/index/field. The lambda returns an UNSEALED literal, so map's runtime result is a
    // boxed Object[]; binding to `Pt[]` must materialize a packed buffer that reads back correctly.
    let mapped = run(r#"import { print } from "std/io"
import { map, filter, for } from "std/iter"
import { toString } from "std/string"
type Pt = { "x": Int32, "y": Int32 }
val pts: Pt[] = [{ "x": 1, "y": 2 }, { "x": 3, "y": 4 }, { "x": 5, "y": 6 }]
val shifted: Pt[] = pts.map(p => { "x": p["x"] + 10, "y": p["y"] })
shifted.for(p => print(toString(p["x"])))
print(toString(shifted[0]["x"]))
print(toString(shifted[1]["x"]))
print(toString(shifted[2]["y"]))
val kept: Pt[] = pts.filter(p => p["x"] > 2)
kept.for(p => print(toString(p["x"])))
print(toString(kept[0]["y"]))
"#);
    // shifted x's via for: 11,13,15 ; shifted[0].x=11, shifted[1].x=13, shifted[2].y=6.
    // filter x>2 keeps {3,4},{5,6}: x's via for 3,5 ; kept[0].y=4.
    assert_eq!(
        mapped,
        vec!["11", "13", "15", "11", "13", "6", "3", "5", "4"]
    );
}

// Regression: pushing record elements into a PACKED sealed-record array (elem_tag 0xFE) whose
// STATIC repr is not proven Packed — the map-value fetch/push shape. `byKey["a"] = []` under a
// `{ String: Pt[] }` annotation allocates a PACKED sealed array (lin_sealed_array_alloc, stride 8)
// and stores it as the map value; `get(byKey, "a", [])` returns it through the generic
// `get<T, D>(m, k, default): T | D` seam, where T's instantiation loses the `sealed` bit (seal is
// deliberately NOT part of type identity — types.rs: `Object{sealed:true} == Object{sealed:false}`)
// — so `push` monomorphizes to the TAGGED body, whose runtime sink (`lin_array_push`, TAG_OBJECT)
// blind-wrote 16-byte TaggedVal slots into the 8-byte-stride packed buffer. Two pushes exactly
// filled the 4×8-byte initial buffer (silent garbage); the THIRD wrote past the end → glibc
// `double free or corruption (out)` / SIGABRT at exit when the buffer was freed. The dynamic sink
// (`lin_push_dyn`) instead fell into the flat-coercion `_ => {}` arm → the push was SILENTLY LOST.
// Root fix: the dynamic/tagged array WRITE sinks (`lin_array_push`, `lin_array_push_tagged`,
// `lin_push_dyn`, `lin_array_set`) now dispatch on `elem_tag == 0xFE` and PACK the boxed LinObject
// element into a fresh packed slot via the array's NAMED descriptor (`pack_named_payload_from_object`
// — the exact WRITE-direction inverse of `lin_array_get_tagged`'s 0xFE materialize-on-read branch,
// ADR-063 mechanism (i)). `run()` asserts a clean exit, so this test guards both halves: correct
// values AND no heap corruption at drop. Kept UN-batched (heap-corruption isolation test).
#[test]
fn test_sealed_record_array_as_map_value_push_no_heap_corruption() {
    // The original repro: packed Pt[] map value, fetched + pushed INSIDE a closure, 3 pushes
    // (one past the 2-push buffer-exact boundary). Also reads elements back through the
    // materialize-on-read path to prove the packed bytes are real field values, not tagged slots.
    let closure = run(r#"import { for } from "std/iter"
import { push, length } from "std/array"
import { get } from "std/object"
import { print } from "std/io"
import { toString } from "std/string"
type Pt = { "x": Int32, "y": Int32 }
val run = (): Null =>
  var byKey: { String: Pt[] } = {}
  byKey["a"] = []
  [1, 2, 3].for(i =>
    push(get(byKey, "a", []), { "x": i, "y": i * 10 })
  )
  val pts = get(byKey, "a", [])
  print("len=${toString(length(pts))}")
  print(toString(pts[0]["x"]))
  print(toString(pts[1]["x"]))
  print(toString(pts[2]["y"]))
run()
"#);
    assert_eq!(closure, vec!["len=3", "1", "2", "30"]);

    // Straight-line variant (no closure): same seam, 5 pushes — crosses the doubling boundary so
    // the sealed grow path (realloc by stride, not by TaggedVal size) is exercised too.
    let straight = run(r#"import { push, length } from "std/array"
import { get } from "std/object"
import { print } from "std/io"
import { toString } from "std/string"
type Pt = { "x": Int32, "y": Int32 }
val run = (): Null =>
  var byKey: { String: Pt[] } = {}
  byKey["a"] = []
  push(get(byKey, "a", []), { "x": 1, "y": 1 })
  push(get(byKey, "a", []), { "x": 2, "y": 2 })
  push(get(byKey, "a", []), { "x": 3, "y": 3 })
  push(get(byKey, "a", []), { "x": 4, "y": 4 })
  push(get(byKey, "a", []), { "x": 5, "y": 5 })
  val pts = get(byKey, "a", [])
  print("len=${toString(length(pts))}")
  print(toString(pts[4]["x"]))
run()
"#);
    assert_eq!(straight, vec!["len=5", "5"]);

    // Control: the direct-capture shape (no map seam) stays on the proven-Packed sealed push.
    let direct = run(r#"import { for } from "std/iter"
import { push, length } from "std/array"
import { print } from "std/io"
import { toString } from "std/string"
type Pt = { "x": Int32, "y": Int32 }
val run = (): Null =>
  var arr: Pt[] = []
  [1, 2, 3].for(i =>
    push(arr, { "x": i, "y": i })
  )
  print("len=${toString(length(arr))}")
run()
"#);
    assert_eq!(direct, vec!["len=3"]);
}

// Regression: a LITERAL-KEY field WRITE into a PACKED SEALED RECORD (`rec["f"] = …`). Before the
// FieldSet fix, codegen routed every sealed-record `obj_ty` (Named alias or inline `{...}`) write
// through `lin_object_set`, which reads a packed sealed struct's bytes as a LinObject header and
// crashed (`index_cap` underflow in `index_probe`). The fix lowers a literal-key write of a present
// field into a constant-offset packed-struct store. This is the exact shape `std/random`'s `Rng`
// handle relies on (a mutable `{ state: UInt64, inc: UInt64 }` advanced in place through a helper).
#[test]
fn test_sealed_record_field_write_through_helper() {
    // Mutate a field of a NAMED sealed record through a function-arg reference (the mutation must be
    // visible at the call site — a sealed record is a mutable reference like an array).
    let scalar = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Counter = { "state": Int32, "inc": Int32 }
val advance = (c: Counter): Null =>
  c["state"] = c["state"] + c["inc"]
  null
val c: Counter = { "state": 0, "inc": 7 }
advance(c)
advance(c)
print(toString(c["state"]))
"#);
    assert_eq!(scalar, vec!["14"]);

    // The wide-scalar case the PCG core uses: a two-field UInt64 sealed record, advanced in place.
    // (UInt64 + named-alias + 2 fields was the precise trigger; an inline `{...}` happened to box.)
    let u64 = run(r#"import { print } from "std/io"
import { toUInt64 } from "std/number"
type Rng = { "state": UInt64, "inc": UInt64 }
val advance = (rng: Rng): Null =>
  rng["state"] = rng["state"] * toUInt64(6364136223846793005) + rng["inc"]
  null
val rng: Rng = { "state": toUInt64(0), "inc": toUInt64(1442695040888963407) }
advance(rng)
advance(rng)
print("${rng["state"]}")
"#);
    // state0=0; s1 = 0*MUL + INC = INC; s2 = INC*MUL + INC (wrapping u64).
    // INC=1442695040888963407, MUL=6364136223846793005.
    // s2 = (1442695040888963407 * 6364136223846793005 + 1442695040888963407) mod 2^64.
    assert_eq!(u64, vec!["1876011003808476466"]);

    // A HEAP field (String) write into a sealed record: the old pointer is released and the new one
    // retained (one net +1 in the struct), so no leak / UAF and the value round-trips.
    let heapf = run(r#"import { print } from "std/io"
type Box = { "tag": Int32, "name": String }
val rename = (b: Box, n: String): Null =>
  b["name"] = n
  null
val b: Box = { "tag": 1, "name": "old" }
rename(b, "new")
print(b["name"])
"#);
    assert_eq!(heapf, vec!["new"]);
}

// Regression: compound index-set `outer["key"][int_key] = value` where the outer read produces a
// `{ K: V } | Null` union type — codegen must route the integer key to the LinMap path
// (`lin_map_set_int`), NOT the array path (`lin_array_set`). Likewise the read-back
// `outer["key"][int_key]` must call `lin_map_get_int`, and the IR ownership convention for that
// result must be `Borrow` (interior slot pointer) to avoid a scope-exit double-free.
// Also covers the NKIND fix: narrow-integer sealed-record fields (UInt16/UInt32) use native-width
// NKIND codes so `materialize_named_payload_to_map` reads the correct byte widths — the bug that
// previously caused a misaligned pointer panic in `toBe` deep-equality on a sealed record held in
// a `[Int32,Int32] | Rec` union map slot.
#[test]
fn test_sealed_union_map_int_key_compound_index() {
    // Basic compound write-then-read through a nested typed Map with UInt8 key.
    let basic = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Inner = { UInt8: String }
type Outer = { String: Inner }
var o: Outer = {}
o["B"] = {}
val k: UInt8 = 1
o["B"][k] = "hello"
val v = o["B"][k]
print(toString(v))
"#);
    assert_eq!(basic, vec!["hello"]);

    // Sealed record with narrow-integer fields (UInt16, UInt32) stored into a union-typed
    // map slot `{ UInt8: [Int32,Int32] | Rec }`. `toBe` materializes both sides and compares
    // all five fields. Previously crashed with misaligned pointer (NKIND bug) and then returned
    // null for the actual (compound int-key map routing bug).
    let sealed = run(r#"import { expect, toBe, test, suite, run } from "std/test"
type Rec = { "origin": String, "destination": String, "duration": UInt16, "startTime": UInt32, "endTime": UInt32 }
type Inner = { UInt8: [Int32, Int32] | Rec }
type Outer = { String: Inner }
val s = suite("s", [ test("sealed union map", () =>
  val o: Outer = {}
  val rec: Rec = { "origin": "A", "destination": "B", "duration": 120, "startTime": 0, "endTime": 86400 }
  o["B"] = o["B"] ?? {}
  val k: UInt8 = 1
  o["B"][k] = rec
  [ expect(o["B"][k]).toBe(rec) ]
) ])
run(s)
"#);
    assert!(sealed.last().map(|s| s.as_str()) == Some("1 passed"),
        "expected '1 passed', got {:?}", sealed);
}

// Variants of the mutual-recursion-record-return fix: a multi-field sealed record, a boxed record
// (a `AnyVal` field forces the boxed `LinObject` repr), a `String` return, and a scalar return
// (the non-record case that always worked — a regression guard). All must round-trip correctly.
#[test]
fn test_mutual_recursion_record_return_variants() {
    // Multi-field sealed record (scalar fields of mixed width).
    let sealed2 = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type P = { "x": Int32, "y": Float64 }
val f = (n: Int32): P =>
  if n <= 0 then { "x": 10, "y": 1.5 } else g(n - 1)
val g = (n: Int32): P =>
  if n <= 0 then { "x": 20, "y": 2.5 } else f(n - 1)
val r = f(5)
print(toString(r["x"]))
print(toString(r["y"]))
"#);
    assert_eq!(sealed2, vec!["20", "2.5"]);

    // Boxed record: a `AnyVal`-typed field is not a sealed-scalar field, so the record is the
    // boxed `LinObject` repr — the cross-function return must stay boxed on both sides.
    let boxed = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type R = { "v": AnyVal }
val f = (n: Int32): R =>
  if n <= 0 then { "v": 0 } else g(n - 1)
val g = (n: Int32): R =>
  if n <= 0 then { "v": 1 } else f(n - 1)
print(toString(f(5)["v"]))
"#);
    assert_eq!(boxed, vec!["1"]);

    // String return (heap value, not a record).
    let s = run(r#"import { print } from "std/io"
val f = (n: Int32): String =>
  if n <= 0 then "even" else g(n - 1)
val g = (n: Int32): String =>
  if n <= 0 then "odd" else f(n - 1)
print(f(5))
"#);
    assert_eq!(s, vec!["odd"]);

    // Scalar return (the always-worked case — regression guard).
    let scalar = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val f = (n: Int32): Int32 =>
  if n <= 0 then 0 else g(n - 1)
val g = (n: Int32): Int32 =>
  if n <= 0 then 1 else f(n - 1)
print(toString(f(5)))
"#);
    assert_eq!(scalar, vec!["1"]);
}

// Reading a PACKED sealed-record array through a `AnyVal` view, then unboxing a scalar field out of
// the dynamic index read, used to leak ~104 B/call (a LINEAR per-call leak, exit 0, no UAF):
//   (1) `val j: AnyVal = ps` materialized a fresh tagged `Object[]` view of the packed `P[]` but the
//       binding-coercion ALSO `unregister_owned`'d the source sealed array (assuming the box took
//       its +1), orphaning the packed array's header + element buffer (~88 B/call).
//   (2) the function-body return path KEPT the raw pre-coercion box (`raw_ret`) unconditionally,
//       which is correct only for a concrete→union SHELL-box (the box wraps `raw_ret`); for the
//       REVERSE unbox (`AnyVal` body returned as `Int32`: `j[0]["x"]`) the scalar result does NOT own
//       the box, so keeping it orphaned the fresh +1 TaggedVal (~16 B/call) — a generic dynamic
//       field/index-read leak, not sealed-specific.
// The leak itself is gated by the ASan harness; this test guards the CORRECTNESS of both shapes (a
// wrong RC release would corrupt the result or crash).
#[test]
fn test_json_view_packed_array_read_round_trip() {
    // Sealed packed P[] read through a AnyVal view, scalar field unboxed out (fix #1 + #2).
    let sealed_view = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type P = { "x": Int32, "y": Int32 }
val once = (i: Int32): Int32 =>
  val ps: P[] = [{ "x": i, "y": 2 }, { "x": 3, "y": 4 }]
  val j: AnyVal = ps
  j[0]["x"]
val loop = (i: Int32, n: Int32, acc: Int32): Int32 =>
  if i >= n then acc else loop(i + 1, n, acc + once(i))
print(toString(loop(0, 10, 0)))
"#);
    assert_eq!(sealed_view, vec!["45"]);

    // Pure-AnyVal object field unboxed to a scalar return (fix #2 in isolation, no sealed array).
    let pure_json = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val once = (i: Int32): Int32 =>
  val j: AnyVal = { "x": i, "y": 2 }
  j["x"]
val loop = (i: Int32, n: Int32, acc: Int32): Int32 =>
  if i >= n then acc else loop(i + 1, n, acc + once(i))
print(toString(loop(0, 10, 0)))
"#);
    assert_eq!(pure_json, vec!["45"]);
}

// Self-recursion returning a record must still work (it TCO's; this guards against the fix
// perturbing the single-function path).
#[test]
fn test_self_recursion_returning_record_still_works() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type R = { "v": Int32 }
val f = (n: Int32): R =>
  if n <= 0 then { "v": 7 } else f(n - 1)
print(toString(f(5)["v"]))
"#);
    assert_eq!(output, vec!["7"]);
}

#[test]
fn test_io_lines_reads_all_stdin_lines() {
    let output = run_with_stdin(r#"import { print } from "std/io"
import { for } from "std/iter"
import { lines } from "std/io"
val all = lines()
all.for(line => print(line))
"#, "hello\nworld\nfoo\n");
    let parts: Vec<&str> = output.lines().collect();
    assert_eq!(parts, vec!["hello", "world", "foo"],
        "lines() should yield each stdin line, got: {:?}", parts);
}

#[test]
fn test_io_read_all_returns_full_content() {
    let output = run_with_stdin(r#"import { print } from "std/io"

import { readAll } from "std/io"
val content = readAll()
print(content)
"#, "hello world");
    assert_eq!(output, "hello world",
        "readAll() should return all stdin content, got: {:?}", output);
}

#[test]
fn test_io_read_line_null_on_empty_stdin() {
    let output = run_with_stdin(r#"import { print } from "std/io"
import { toString } from "std/string"

import { readLine } from "std/io"
val line = readLine()
print(toString(line))
"#, "");
    assert_eq!(output, "null",
        "readLine() on empty stdin should return null, got: {:?}", output);
}

// HTTP live tests using an in-process tiny_http server

#[test]
#[ignore = "loopback-contention flake: passes isolated and single-threaded, but the in-process tiny_http server can miss the request under full parallel load (fetchJson then yields null). Run with `--ignored` to exercise deliberately."]
fn test_http_fetch_json() {
    use std::thread;
    use std::time::Duration;
    // Bind on the test thread to an OS-assigned ephemeral port (port 0) so concurrent
    // test runs can never collide on a fixed port. Reading the port back after the bind
    // also guarantees the listener is open before the client runs — no startup sleep race.
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();
    thread::spawn(move || {
        if let Ok(Some(req)) = server.recv_timeout(Duration::from_secs(10)) {
            let _ = req.respond(tiny_http::Response::from_string(r#"{"value": 42}"#));
        }
    });
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"

import {{ fetchJson }} from "std/http"
val result = fetchJson("http://127.0.0.1:{}")
print(toString(result["value"]))
"#, port));
    assert_eq!(output, vec!["42"]);
}

#[test]
fn test_http_transport_failure_is_error() {
    let output = run(r#"import { print } from "std/io"
import { fetch } from "std/http"
val result = fetch("http://127.0.0.1:1")
print(result["type"])
"#);
    assert_eq!(output, vec!["error"]);
}

// End-to-end FFI test

#[test]
fn test_ffi_end_to_end_c_library() {
    // The "your-own-C" FFI mode (a built-from-source static archive) was folded from the old
    // examples/ffi into the sdl project at examples/sdl/clib/. This test builds a small program
    // against that archive and checks the C calls round-trip (int32_t<->Int32, double<->Float64,
    // and libm `sqrt` via magnitude2). The richer vendored-.so FFI mode is covered by the
    // SDL demo tests below; the per-function results are also asserted in examples/sdl/mathffi.test.lin.
    let ws = workspace_root();
    let lin_bin = lin_bin();
    let mathlib_c = ws.join("examples/sdl/clib/mathlib.c");
    // Everything this test produces lives under target/ — it never writes into the examples
    // tree (so the committed examples/sdl/clib/libmathlib.a is untouched and the fmt-corpus
    // test that scans examples/ can't race a transient file). The archive is rebuilt for the
    // CURRENT platform (a committed Linux x86-64 .a won't link on macOS), and the foreign
    // import references that freshly-built target/ archive.
    let mathlib_a = ws.join("target/ffi_c_mathlib.a");
    let obj = ws.join("target/ffi_c_mathlib.o");
    let output_bin = ws.join("target/ffi_c_test");
    let ffi_example = ws.join("target/ffi_c_smoke.lin");

    if !lin_bin.exists() {
        eprintln!("SKIP: lin binary not built; run `cargo build -p lin` first");
        return;
    }
    if !mathlib_c.exists() {
        eprintln!("SKIP: examples/sdl/clib/mathlib.c not present");
        return;
    }

    // Build a platform-correct static archive from the example's C source, into target/.
    let cc_status = Command::new("cc")
        .args(["-c", mathlib_c.to_str().unwrap(), "-o", obj.to_str().unwrap()])
        .status();
    if cc_status.map(|s| !s.success()).unwrap_or(true) {
        eprintln!("SKIP: failed to compile C library");
        return;
    }
    let ar_status = Command::new("ar")
        .args(["rcs", mathlib_a.to_str().unwrap(), obj.to_str().unwrap()])
        .status();
    if ar_status.map(|s| !s.success()).unwrap_or(true) {
        eprintln!("SKIP: failed to create static archive");
        return;
    }

    // A minimal program exercising the C ABI directly (mirrors examples/sdl/mathffi.lin's
    // bindings), linking the freshly-built target/ archive. Built with cwd=ws so the
    // workspace-relative foreign path resolves.
    std::fs::write(&ffi_example, concat!(
        "import { print } from \"std/io\"\n",
        "import { toString } from \"std/string\"\n",
        "import foreign \"target/ffi_c_mathlib.a\"\n",
        "  val add: (Int32, Int32) => Int32\n",
        "  val square: (Float64) => Float64\n",
        "  val magnitude2: (Float64, Float64) => Float64\n",
        "print(\"3 + 4 = ${toString(add(3, 4))}\")\n",
        "print(\"2.5^2 = ${toString(square(2.5))}\")\n",
        "print(\"|3,4| = ${toString(magnitude2(3.0, 4.0))}\")\n",
    )).expect("failed to write ffi smoke source");

    let compile_out = Command::new(&lin_bin)
        .args(["build", "target/ffi_c_smoke.lin", "-o", output_bin.to_str().unwrap()])
        .current_dir(&ws)
        .output()
        .expect("failed to run lin build");
    let _ = std::fs::remove_file(&ffi_example);
    assert!(compile_out.status.success(),
        "lin build failed: {}", String::from_utf8_lossy(&compile_out.stderr));

    let run_out = Command::new(&output_bin).output().expect("failed to run ffi binary");
    assert!(run_out.status.success());
    let stdout = String::from_utf8_lossy(&run_out.stdout);
    assert!(stdout.contains("3 + 4 = 7"), "Expected '3 + 4 = 7', got: {}", stdout);
    assert!(stdout.contains("2.5^2 = 6.25"), "Expected '2.5^2 = 6.25', got: {}", stdout);
    assert!(stdout.contains("|3,4| = 5"), "Expected '|3,4| = 5', got: {}", stdout);
}

// End-to-end richer-FFI + concurrency keystone tests: the examples/sdl/ project drives the REAL
// SDL3 3.4.10 C ABI (SDL_Init / SDL_CreateWindow / SDL_RenderFillRect / SDL_RenderReadPixels / …)
// against the committed REAL libSDL3.so (examples/sdl/libs/, soname chain
// libSDL3.so -> .so.0 -> .so.0.4.10). Each demo is compiled with `lin build`, then RUN from a
// directory other than the workspace with LD_LIBRARY_PATH cleared — so the only way the vendored
// .so resolves is the baked-in $ORIGIN-relative rpath (NEEDED is the soname libSDL3.so.0). Real
// SDL3 emits no synthetic QUIT, so each demo runs a FIXED frame count and self-terminates, then
// proves rendering by reading a pixel back with SDL_RenderReadPixels. The demos require the dummy
// video driver (no display in CI), so the spawned binary is run with SDL_VIDEODRIVER=dummy.

/// Build `example` with `lin build`, run it from the temp dir with LD_LIBRARY_PATH cleared (proving
/// the $ORIGIN rpath finds the vendored .so) and SDL_VIDEODRIVER=dummy (headless), assert exit 0,
/// and return its stdout.
fn run_sdl_demo(ws: &std::path::Path, lin_bin: &std::path::Path, example: &str, out_name: &str) -> String {
    let example_path = ws.join(example);
    let output_bin = ws.join("target").join(out_name);
    let compile_out = Command::new(lin_bin)
        .args(["build", example_path.to_str().unwrap(), "-o", output_bin.to_str().unwrap()])
        .current_dir(ws)
        .output()
        .expect("failed to run lin build");
    assert!(
        compile_out.status.success(),
        "lin build {} failed: {}",
        example,
        String::from_utf8_lossy(&compile_out.stderr)
    );
    let run_out = Command::new(&output_bin)
        .current_dir(std::env::temp_dir())
        .env_remove("LD_LIBRARY_PATH")
        .env("SDL_VIDEODRIVER", "dummy")
        .output()
        .expect("failed to run sdl demo binary");
    assert!(
        run_out.status.success(),
        "{} failed (rpath not resolving the vendored .so, or SDL init failed): status={} stderr={}",
        example,
        run_out.status,
        String::from_utf8_lossy(&run_out.stderr)
    );
    String::from_utf8_lossy(&run_out.stdout).into_owned()
}

// bounce.lin: Ptr handles (window/renderer/surface/pixels) round-trip; a String title marshalled
// via withCstr; an SDL_FRect built in a poked buffer (four f32); a FIXED 60-frame loop that
// self-terminates; and a SDL_RenderReadPixels readback that PROVES real rendering — the pixel at
// the centre of the ball's final rect equals the fill colour (255,128,0) in XRGB8888 B,G,R order.
#[test]
fn test_sdl_bounce_headless() {
    let ws = workspace_root();
    let lin_bin = lin_bin();
    if !lin_bin.exists() {
        eprintln!("SKIP: lin binary not built; run `cargo build -p lin` first");
        return;
    }
    // The committed libSDL3.so is a Linux x86-64 ELF; it will not link/load on macOS. macOS SDL
    // would need a macOS dylib (future enhancement); the rpath MECHANISM itself is covered cross-
    // platform by test_ffi_vendored_shared_lib_relocatable.
    if !cfg!(target_os = "linux") {
        eprintln!("SKIP: committed libSDL3 is a Linux ELF; SDL demo tests run on Linux only");
        return;
    }
    // The real libSDL3.so is committed; skip only if it is somehow absent on this platform.
    if !ws.join("examples/sdl/libs/libSDL3.so").exists() {
        eprintln!("SKIP: examples/sdl/libs/libSDL3.so not present");
        return;
    }
    let stdout = run_sdl_demo(&ws, &lin_bin, "examples/sdl/bounce.lin", "sdl_bounce_test");
    assert!(stdout.contains("window handle non-null: true"), "got: {}", stdout);
    assert!(stdout.contains("renderer handle non-null: true"), "got: {}", stdout);
    // Fixed frame count — the demo self-terminates (real SDL3 emits no QUIT headless).
    assert!(stdout.contains("frames drawn: 60"), "got: {}", stdout);
    // SDL_RenderReadPixels readback proves real software rendering happened: the readback
    // pixel inside the ball's final rect equals the fill colour (255,128,0) in XRGB8888
    // B,G,R order. The ball now follows a vector-velocity path with a per-frame rotation
    // (vector.lin/matrix.lin), so the final rect — and the sampled pixel — sit at [78,193].
    assert!(stdout.contains("pixel[78,193] = 255,128,0"), "got: {}", stdout);
    assert!(stdout.contains("rendered pixel matches fill: true"), "got: {}", stdout);
    // The your-own-C FFI binding (clib/libmathlib.a) is linked into the same binary as the
    // vendored SDL .so: a `magnitude2` (libm sqrt) distance is computed from the final position.
    assert!(stdout.contains("distance to centre (via C magnitude2):"), "got: {}", stdout);
    assert!(stdout.contains("done"), "got: {}", stdout);
}

// ai_worker.lin: same real-SDL main-thread loop PLUS an `async` PURE worker. Each frame deep-copies
// a plain World snapshot into the thunk and deep-copies the planned {x,y} back — no SDL handle or
// var crosses the boundary. Deterministic: the agent steps one cell/axis toward the goal each frame
// (capped at the goal), so over 60 frames from (0,0) it reaches the goal (18,11). The agent's final
// pixel is read back and asserted to be its fill colour (0,200,120).
#[test]
fn test_sdl_ai_worker_headless() {
    let ws = workspace_root();
    let lin_bin = lin_bin();
    if !lin_bin.exists() {
        eprintln!("SKIP: lin binary not built; run `cargo build -p lin` first");
        return;
    }
    // Linux-only: committed libSDL3 is a Linux ELF (see test_sdl_bounce_headless). The rpath
    // mechanism is covered cross-platform by test_ffi_vendored_shared_lib_relocatable.
    if !cfg!(target_os = "linux") {
        eprintln!("SKIP: committed libSDL3 is a Linux ELF; SDL demo tests run on Linux only");
        return;
    }
    if !ws.join("examples/sdl/libs/libSDL3.so").exists() {
        eprintln!("SKIP: examples/sdl/libs/libSDL3.so not present");
        return;
    }
    let stdout = run_sdl_demo(&ws, &lin_bin, "examples/sdl/ai_worker.lin", "sdl_ai_worker_test");
    assert!(stdout.contains("window handle non-null: true"), "got: {}", stdout);
    assert!(stdout.contains("frames drawn: 60"), "got: {}", stdout);
    assert!(stdout.contains("final agent: 18,11"), "got: {}", stdout);
    assert!(stdout.contains("pixel[148,92] = 0,200,120"), "got: {}", stdout);
    assert!(stdout.contains("rendered pixel matches fill: true"), "got: {}", stdout);
    assert!(stdout.contains("done"), "got: {}", stdout);
}

// Cross-platform proof that the vendored-shared-library rpath mechanism is RELOCATABLE on whatever
// OS this test runs on (ubuntu-22.04 and macos-latest in CI). Unlike the SDL tests, this builds its
// OWN tiny shared library from C source at test time — so there is no committed platform binary, and
// the SAME test exercises the Linux ($ORIGIN) and macOS (@loader_path + @rpath install_name) paths.
//
// The build binary lives in a SUBDIR (build/) distinct from the lib dir (libs/) so the emitted rpath
// is a NON-EMPTY relative token (e.g. `$ORIGIN/../libs` or `@loader_path/../libs`). We then relocate
// the binary + lib to a fresh tree PRESERVING that relative layout (binary at reloc/build/prog, lib
// at reloc/libs/...) and run the relocated binary from an unrelated cwd with the library search-path
// env stripped — so the only way the lib resolves is the baked-in relative rpath.
#[test]
fn test_ffi_vendored_shared_lib_relocatable() {
    let lin_bin = lin_bin();
    if !lin_bin.exists() {
        eprintln!("SKIP: lin binary not built; run `cargo build -p lin` first");
        return;
    }

    // Platform specifics.
    let is_macos = cfg!(target_os = "macos");
    let ext = if is_macos { "dylib" } else { "so" };

    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let tmp = std::env::temp_dir().join(format!("lin_relo_{}", id));
    let _ = fs::remove_dir_all(&tmp);
    let libs_dir = tmp.join("libs");
    let build_dir = tmp.join("build");
    fs::create_dir_all(&libs_dir).unwrap();
    fs::create_dir_all(&build_dir).unwrap();

    // 1. Write + compile the tiny shared library into <tmp>/libs/librelo.<ext>.
    let src_c = tmp.join("mylib.c");
    fs::write(
        &src_c,
        "#include <stdint.h>\nint32_t lin_relo_add(int32_t a, int32_t b){ return a + b; }\n",
    )
    .unwrap();
    let lib_path = libs_dir.join(format!("librelo.{}", ext));
    let cc_args: Vec<String> = if is_macos {
        // -install_name @rpath/<leaf> makes the executable's reference to the dylib `@rpath/...`,
        // so the @loader_path rpath is consulted (we control this dylib).
        vec![
            "-dynamiclib".into(),
            "-install_name".into(),
            "@rpath/librelo.dylib".into(),
            "-o".into(),
            lib_path.display().to_string(),
            src_c.display().to_string(),
        ]
    } else {
        vec![
            "-shared".into(),
            "-fPIC".into(),
            "-o".into(),
            lib_path.display().to_string(),
            src_c.display().to_string(),
        ]
    };
    let cc_status = Command::new("cc").args(&cc_args).status();
    if cc_status.map(|s| !s.success()).unwrap_or(true) {
        eprintln!("SKIP: failed to compile relocatable shared library (cc unavailable?)");
        let _ = fs::remove_dir_all(&tmp);
        return;
    }

    // 2. Write the Lin program. The foreign-library path is resolved relative to the `lin build`
    //    process CWD, which we set to <tmp> below — so "libs/librelo.<ext>" points at the lib we
    //    just built. (Both the .lin file and the libs dir live under <tmp>.)
    let prog_lin = tmp.join("prog.lin");
    fs::write(
        &prog_lin,
        format!(
            r#"import foreign "libs/librelo.{ext}"
  val lin_relo_add: (Int32, Int32) => Int32

import {{ print }} from "std/io"
import {{ toString }} from "std/string"

print("relo: ${{toString(lin_relo_add(40, 2))}}")
"#,
            ext = ext
        ),
    )
    .unwrap();

    // 3. Build into <tmp>/build/prog so the binary dir (build/) differs from the lib dir (libs/),
    //    forcing a non-empty relative rpath like `<token>/../libs`.
    let built_bin = build_dir.join("prog");
    let compile_out = Command::new(&lin_bin)
        .args(["build", prog_lin.to_str().unwrap(), "-o", built_bin.to_str().unwrap()])
        // Resolve the foreign-library import path (libs/librelo.<ext>) relative to <tmp>.
        .current_dir(&tmp)
        .output()
        .expect("failed to run lin build");
    assert!(
        compile_out.status.success(),
        "lin build failed: {}",
        String::from_utf8_lossy(&compile_out.stderr)
    );

    // 4. RELOCATION: copy the built binary + libs dir into a FRESH tree, preserving the SAME
    //    relative layout the rpath encodes (binary at reloc/build/prog, lib at reloc/libs/...).
    let reloc = tmp.join("reloc");
    let reloc_build = reloc.join("build");
    let reloc_libs = reloc.join("libs");
    fs::create_dir_all(&reloc_build).unwrap();
    fs::create_dir_all(&reloc_libs).unwrap();
    let reloc_bin = reloc_build.join("prog");
    fs::copy(&built_bin, &reloc_bin).unwrap();
    fs::copy(&lib_path, reloc_libs.join(format!("librelo.{}", ext))).unwrap();
    // Preserve the executable bit on the copied binary.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&reloc_bin).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&reloc_bin, perms).unwrap();
    }

    // 5. Run the relocated binary from an UNRELATED cwd with the lib search-path env stripped, so
    //    the only resolution path is the baked-in relative rpath.
    let mut run_cmd = Command::new(&reloc_bin);
    run_cmd.current_dir(std::env::temp_dir());
    // Strip the platform's lib search-path env. On macOS DYLD_* vars are generally not inherited
    // into a spawned child and are SIP-stripped; remove DYLD_LIBRARY_PATH for good measure anyway.
    run_cmd.env_remove("LD_LIBRARY_PATH");
    run_cmd.env_remove("DYLD_LIBRARY_PATH");
    // We just `fs::copy`'d `reloc_bin` and immediately exec it. Under the full parallel suite (600+
    // concurrent `lin build` fork/exec cycles), another forked child can still transiently hold a
    // write fd to the freshly-copied executable, so `execve` returns ETXTBSY ("text file busy") —
    // an intermittent "failed to run relocated binary" panic unrelated to what this test actually
    // checks (relative-rpath relocation). Retry a few times on that specific spawn error.
    let run_out = {
        let mut attempt = 0;
        loop {
            match run_cmd.output() {
                Ok(out) => break out,
                // ETXTBSY (errno 26): the copied binary is still open for write elsewhere. Transient.
                Err(e) if e.raw_os_error() == Some(26) && attempt < 10 => {
                    attempt += 1;
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(e) => panic!("failed to run relocated binary: {e}"),
            }
        }
    };
    let stdout = String::from_utf8_lossy(&run_out.stdout);
    let stderr = String::from_utf8_lossy(&run_out.stderr);
    assert!(
        run_out.status.success(),
        "relocated binary failed (relative rpath did not resolve the vendored lib): status={} stdout={} stderr={}",
        run_out.status, stdout, stderr
    );
    assert!(
        stdout.contains("relo: 42"),
        "expected 'relo: 42', got stdout={} stderr={}",
        stdout, stderr
    );

    let _ = fs::remove_dir_all(&tmp);
}

// ── Formatter idempotency ─────────────────────────────────────────────────────

/// Lex, parse, and format a Lin source string, preserving comments. Panics on parse errors.
fn fmt(source: &str) -> String {
    let mut lexer = lin_lex::Lexer::new(source, 0);
    let tokens = lexer.tokenize();
    let comments = lexer.comments().to_vec();
    let mut parser = lin_parse::Parser::new(tokens);
    let module = parser.parse_module();
    assert!(
        parser.diagnostics.is_empty(),
        "parse errors: {:?}\nsource:\n{}",
        parser.diagnostics.iter().map(|d| d.message.clone()).collect::<Vec<_>>(),
        source
    );
    lin_parse::Formatter::with_comments(source, comments).format_module(&module)
}

#[test]
fn test_fmt_preserves_blank_line_between_leading_comments() {
    // A blank line the author leaves BETWEEN two leading comment lines is preserved (one blank;
    // runs collapse to one) — so a module-header comment block stays visually separated from the
    // doc comment of the first declaration, instead of being glued into one block. This is what
    // lets the docs generator tell the page intro apart from the first function's doc.
    let src = "// module header line 1\n// module header line 2\n\n// doc for foo\nval foo = (n: Int32): Int32 =>\n  n + 1\n";
    let out = fmt(src);
    assert_eq!(out, src, "blank line between leading comments must be preserved");
    // Idempotent.
    assert_eq!(fmt(&out), out, "formatter not idempotent");

    // A run of 2+ blank lines between comments collapses to exactly one.
    let runs = "// a\n\n\n\n// b\nval x = (): Int32 =>\n  1\n";
    let collapsed = "// a\n\n// b\nval x = (): Int32 =>\n  1\n";
    assert_eq!(fmt(runs), collapsed, "blank-line run must collapse to one");

    // No blank between comments stays no blank (the common doc-comment-on-its-decl case).
    let glued = "// header\n// doc\nval y = (): Int32 =>\n  2\n";
    assert_eq!(fmt(glued), glued, "adjacent comments must stay adjacent");

    // A blank between the LAST leading comment and the declaration itself is also preserved — this
    // is the module-header-runs-straight-into-the-first-`export` case (no comment between). It lets
    // the docs generator separate the page intro from the first declaration cleanly.
    let hdr = "// module header\nval z = (): Int32 =>\n  3\n";
    let hdr_sep = "// module header\n\nval z = (): Int32 =>\n  3\n";
    assert_eq!(fmt(hdr_sep), hdr_sep, "blank between header and decl must be preserved");
    assert_eq!(fmt(&fmt(hdr_sep)), hdr_sep, "header/decl blank must be idempotent");
    // ...but a doc comment with NO blank still hugs its declaration (the overwhelmingly common case).
    assert_eq!(fmt(hdr), hdr, "doc comment with no blank must stay glued to its declaration");
}

// ── Formatter must never change program meaning ───────────────────────────────
// The formatter rebuilds source from the AST, which discards parentheses and the
// generic `<T>` list. If it doesn't re-emit them correctly the formatted code can
// silently MISCOMPILE (wrong operator precedence) or fail to compile (lost generics).
// These guard the specific defects found when first sweeping the stdlib, plus a
// general "format then run produces identical output" equivalence check.

#[test]
fn test_fmt_preserves_grouping_parens() {
    // `(a + b) / c` must keep its parens — `/` binds tighter than `+`, so dropping
    // them changes the value. Author-written parens are PRESERVED (we never strip a
    // grouping the author wrote, even when precedence makes it redundant — it reads worse
    // stripped, e.g. `(a / b) * c`), and parens are never ADDED where the author had none.
    assert_eq!(fmt("val x = (1 + 2) / 3\n").trim(), "val x = (1 + 2) / 3");
    assert_eq!(fmt("val x = (1 + 2 - 1) / 4\n").trim(), "val x = (1 + 2 - 1) / 4");
    assert_eq!(fmt("val x = a - (b - c)\n").trim(), "val x = a - (b - c)");
    // Author parens preserved even when redundant; none added when absent.
    assert_eq!(fmt("val x = (a - b) - c\n").trim(), "val x = (a - b) - c");
    assert_eq!(fmt("val x = a - b - c\n").trim(), "val x = a - b - c");
    assert_eq!(fmt("val x = (a || b) && c\n").trim(), "val x = (a || b) && c");
    // And it must actually still evaluate correctly end-to-end.
    let out = run("import { print } from \"std/io\"\nimport { toString } from \"std/string\"\nval r = (1 + 2) / 3\nprint(toString(r))\n");
    assert_eq!(out, vec!["1"], "( 1 + 2 ) / 3 should be 1, not 1 + (2/3) = 1");
}

#[test]
fn test_fmt_parenthesizes_if_as_binary_operand() {
    // A greedy-tailed primary (`if`/`match`/bare lambda) used as a binary operand MUST keep
    // parentheses: its trailing branch is parsed by consuming a full expression, so dropping the
    // parens re-associates the operator into the branch. `(if c then y else z) / 400` reparses as
    // `if c then y else (z / 400)` — a different value. Regression guard for the formatter soundness
    // bug where `fmt_binop_operand` only wrapped `BinaryOp`/`Coalesce` operands.
    assert_eq!(
        fmt("val x = (if c then a else b) / 400\n").trim(),
        "val x = (if c then a else b) / 400"
    );
    // Right operand too (left-associative `+` would otherwise swallow a following term).
    assert_eq!(
        fmt("val x = n + (if c then a else b)\n").trim(),
        "val x = n + (if c then a else b)"
    );
    // Idempotent — re-formatting the output must not strip the parens it just added.
    let once = fmt("val x = (if c then a else b) / 400\n");
    assert_eq!(fmt(&once), once, "formatter not idempotent on if-operand parens");
    // End-to-end: the value must be the grouped one. (-44 // 400 with truncating division is 0;
    // the buggy reparse `-(399/400)` would give a different result.)
    let out = run(
        "import { print } from \"std/io\"\nimport { toString } from \"std/string\"\n\
         val y: Int64 = 0 - 44\nval era = (if y >= 0 then y else y - 399) / 400\nprint(toString(era))\n",
    );
    assert_eq!(out, vec!["-1"], "(if y>=0 then y else y-399)/400 with y=-44 must be -1");
}

#[test]
fn test_fmt_negative_int64_field_in_direct_object_arg() {
    // A negative integer literal in an object literal passed DIRECTLY as an argument to a function
    // with an `Int64`-field record parameter must adopt the field's Int64 type (not default to
    // Int32 and get zero-extended to 2^32-n). Regression guard for the checker fix routing object
    // literals against concrete `Type::Object` params through expected-type-directed checking.
    let out = run(
        "import { print } from \"std/io\"\nimport { toString } from \"std/string\"\n\
         type R = { \"y\": Int64 }\nval readY = (r: R): Int64 => r[\"y\"]\n\
         print(toString(readY({ \"y\": 0 - 44 })))\nprint(toString(readY({ \"y\": -44 })))\n",
    );
    assert_eq!(out, vec!["-44", "-44"], "negative Int64 field in a direct object arg must not zero-extend");
}

#[test]
fn test_mixed_integer_width_arithmetic_widens_to_result() {
    // Arithmetic between two DIFFERENT integer widths must widen both operands AND the op to the
    // result width the checker assigned. Under mixed-signedness widening `Int32 + UInt8` (and even
    // `Int32 + UInt32`, both 32-bit) yields Int64, so the add must run at i64 — otherwise codegen
    // emits `add i32` into an i64 box/return slot (LLVM type mismatch / miscompile). Regression
    // guard for the operand-width-reconciliation fix in compile_binary_op_values.
    let out = run(
        "import { print } from \"std/io\"\nimport { toString } from \"std/string\"\n\
         type D = { \"a\": Int32, \"b\": UInt8 }\nval f = (d: D): Int64 => d[\"a\"] * 12 + d[\"b\"]\n\
         print(toString(f({ \"a\": 2024, \"b\": 3 })))\n",
    );
    assert_eq!(out, vec!["24291"], "Int32*12 + UInt8 must compute at the Int64 result width");

    // Mixed-sign same-width: Int32 + UInt32 -> Int64. Unsigned operand must zero-extend (stay
    // positive), not sign-extend into a negative.
    let out = run(
        "import { print } from \"std/io\"\nimport { toString } from \"std/string\"\n\
         type D = { \"a\": Int32, \"b\": UInt32 }\nval f = (d: D): Int64 => d[\"a\"] + d[\"b\"]\n\
         print(toString(f({ \"a\": 1, \"b\": 4000000000 })))\n",
    );
    assert_eq!(out, vec!["4000000001"], "Int32 + UInt32 must widen to i64 and zero-extend the unsigned operand");

    // A UInt8 read mixed with an Int32 literal in a chained bitwise/shift expression (the
    // std/bytes u32FromBe shape) must still compile — the fix must not infinitely recurse when an
    // operand's LLVM value width differs from its static type width.
    let out = run(
        "import { print } from \"std/io\"\nimport { toString } from \"std/string\"\n\
         import { u32FromBe } from \"std/bytes\"\n\
         val bytes: UInt8[] = [255, 255, 255, 255]\nprint(toString(u32FromBe(bytes, 0)))\n",
    );
    assert_eq!(out, vec!["4294967295"], "chained shift/or over UInt8 reads must compile and be correct");
}

#[test]
fn test_fmt_implicit_else_null_omitted() {
    // A statement-position `if` with an IMPLICIT null else drops the `else null`; an
    // author-written `else null` is kept (it may signal intent).
    assert_eq!(
        fmt("val f = (d: Int32): Null =>\n  if d < INF then total = total + d\n").trim(),
        "val f = (d: Int32): Null =>\n  if d < INF then total = total + d"
    );
    assert_eq!(
        fmt("val g = (d: Int32): Null =>\n  if d < INF then total = total + d else null\n").trim(),
        "val g = (d: Int32): Null =>\n  if d < INF then total = total + d else null"
    );
    // The `if` as the TAIL of a multi-statement lambda body is also statement position — an
    // implicit null else there is dropped too (regression: it came back when the body is a block).
    let block_tail = fmt("val h = arr.for(item =>\n  val keep = f(item)\n  if keep then push(result, item)\n)\n");
    assert!(!block_tail.contains("else null"), "implicit else null re-added on block tail:\n{}", block_tail);
    assert_eq!(block_tail, fmt(&block_tail), "not idempotent:\n{}", block_tail);
}

#[test]
fn test_fmt_block_bodied_val_round_trips() {
    // A `val` whose RHS is a multi-statement BLOCK must render the block on its own
    // indented lines, not inlined after `= ` (which collapsed it onto one line and
    // produced unparseable source). The canonical form below must round-trip identically.
    let src = "\
val basePath =
  val env = getEnv(\"LIN_DOCS_BASE\")
  if env == null then \"\" else env
";
    let out = fmt(src);
    assert_eq!(out, src, "block-bodied val not rendered as indented block:\n{}", out);
    assert_eq!(out, fmt(&out), "block-bodied val not idempotent:\n{}", out);
    // And the formatted text must re-parse + type-check.
    let prog = format!(
        "import {{ print }} from \"std/io\"\n\
         val getEnv = (k: String): String|Null => null\n\
         {}\nprint(basePath)\n",
        out
    );
    assert!(
        lin_check_ok_source(&prog),
        "formatted block-bodied val no longer type-checks:\n{}",
        prog
    );
}

#[test]
fn test_if_else_wrapped_inside_parens_parses_and_round_trips() {
    // Regression (LIN_ISSUES #7): a WRAPPED (multi-line) `if/else` as the RHS of a `val`
    // INSIDE a parenthesised closure body (`.for(... => …)`) used to fail with
    // `unexpected token Else`. ADR-003 suppresses Indent/Dedent inside `()`, so the branch
    // offside floor must anchor on the indentation of the LINE the `if` sits on, not on the
    // `if` keyword's (far-right) column — else the then-branch collapses to empty and the
    // newline `else` is orphaned. The one-line form always parsed; only the wrapped form broke.
    let src = "\
import { print } from \"std/io\"
import { for } from \"std/iter\"
val f = (raptor: AnyVal, marked: AnyVal): Null =>
  marked.for(stopP =>
    val transfers = if raptor[stopP] != null then
      raptor[stopP]
    else
      []
    transfers.for(t => print(t))
  )
val run = (): Null => f({ \"a\": [\"x\"] }, [\"a\"])
run()
";
    // It must compile and run (this was the original failure).
    assert_eq!(run(src), vec!["x"], "wrapped if/else inside .for(...) should run");

    // And the formatter must round-trip it: `fmt()` panics on a parse error, so if the
    // formatted output were unparseable (the bug that corrupted raptor.lin), this fails.
    let out = fmt(src);
    assert_eq!(out, fmt(&out), "wrapped if/else inside parens not idempotent:\n{}", out);
}

#[test]
fn test_nested_if_else_if_in_parens_outer_else_attaches_to_outer_if() {
    // Regression: a nested `if … else if …` chain inside a parenthesised lambda body
    // (`.for(... => …)`, where ADR-003 suppresses Indent/Dedent) used to mis-attach the OUTER
    // `else if` to the nearest INNER `if`, making it DEAD whenever the inner condition path was
    // not taken. With no Dedent to close the inner `if`, the `else` was bound greedily; the fix
    // is an offside guard — an `else` whose line-start column is left of the chain's `if` belongs
    // to the ENCLOSING `if`, so it is left for the outer parser. (This silently produced 0
    // journeys in the RAPTOR scanRoutes port: boarding's outer `else if prevArrival != 0` never
    // ran, so setTrip never fired.)
    let src = "\
import { print } from \"std/io\"
import { for, range } from \"std/iter\"
val go = (): Null =>
  range(0, 2).for(i =>
    var t = false
    if t then
      print(\"then\")
      if i == 0 then
        print(\"inner-then\")
      else if i == 1 then
        print(\"inner-elseif\")
    else if i == 0 then
      print(\"outer-elseif\")
  )
go()
";
    // `t` is always false, so neither the outer `then` nor the inner chain runs; only the OUTER
    // `else if i == 0` may fire — exactly once, for i == 0. Before the fix this printed nothing.
    assert_eq!(
        run(src),
        vec!["outer-elseif"],
        "outer else-if inside .for(...) must attach to the outer if, not the inner one"
    );

    // The formatter must also round-trip the structure without changing meaning.
    let out = fmt(src);
    assert_eq!(out, fmt(&out), "nested else-if inside parens not idempotent:\n{}", out);
}

#[test]
fn test_fmt_else_if_block_branch_comment_preserved_once() {
    // A leading own-line comment on the first statement of an `else if ... then` Block
    // branch body was emitted TWICE (the If arm's `take_leading` and `fmt_block` both
    // emitted it), compounding each pass. It must appear exactly once and be idempotent.
    let src = "\
val f = (n: Int32): Int32 =>
  if n == 0 then
    1
  else if n == 1 then
    // first branch comment
    val a = 2
    a
  else
    3
";
    let out = fmt(src);
    assert_eq!(
        out.matches("// first branch comment").count(),
        1,
        "branch-body leading comment not preserved exactly once:\n{}",
        out
    );
    assert_eq!(out, fmt(&out), "else-if branch comment fmt not idempotent:\n{}", out);
}

#[test]
fn test_fmt_array_element_trailing_comment() {
    // A trailing comment on a single-line array element stays trailing (after its comma); it is
    // NOT demoted to a leading comment on the next line. An own-line comment before an element
    // stays leading.
    let src = "val a = [\n  expect(x).toBe(1), // note\n  expect(y).toBe(2)\n]\n";
    let out = fmt(src);
    assert!(out.contains("expect(x).toBe(1), // note"), "array-element trailing comment demoted:\n{}", out);
    assert_eq!(out, fmt(&out), "not idempotent:\n{}", out);
    // Own-line comment before an element stays leading.
    let lead = fmt("val b = [\n  // group\n  expect(x).toBe(1),\n  expect(y).toBe(2)\n]\n");
    assert!(lead.contains("  // group\n  expect(x).toBe(1)"), "own-line array comment changed:\n{}", lead);
}

#[test]
fn test_fmt_preserves_author_multiline_literals() {
    // A literal the author broke across lines stays multi-line (never rolled up); an author-
    // inline literal stays inline (never rolled out).
    let ml = "val a = [\n  expect(valueOf(ok(numNode(42)))).toBe(42)\n]\n";
    assert_eq!(fmt(ml), ml, "author-multilined single-item array rolled up:\n{}", fmt(ml));
    let inline = "val b = [1, 2, 3]\n";
    assert_eq!(fmt(inline), inline, "inline array changed:\n{}", fmt(inline));
}

#[test]
fn test_fmt_multi_call_array_multiline() {
    // A multi-element array whose elements contain calls renders multi-line (packing several
    // calls on one line reads poorly). A single-call array and a plain-literal array stay inline.
    let out = fmt("val a = [expect(x).toBe(1), expect(y).toBe(2)]\n");
    assert!(out.contains("[\n  expect(x).toBe(1),\n  expect(y).toBe(2)\n]"), "multi-call array not multiline:\n{}", out);
    assert_eq!(fmt("val b = [expect(x).toBe(1)]\n").trim(), "val b = [expect(x).toBe(1)]");
    assert_eq!(fmt("val c = [1, 2, 3]\n").trim(), "val c = [1, 2, 3]");
    assert_eq!(out, fmt(&out), "not idempotent:\n{}", out);
}

#[test]
fn test_fmt_bare_lambda_in_arg_position() {
    // A single-ident / wildcard, type-less lambda is bare in ARGUMENT position, parenthesised
    // elsewhere (round-trip safe — bare doesn't parse on a `val` RHS).
    assert_eq!(fmt("val x = items.map(i => i + 1)\n").trim(), "val x = items.map(i => i + 1)");
    assert_eq!(fmt("val x = items.for(_ => g())\n").trim(), "val x = items.for(_ => g())");
    assert_eq!(fmt("val f = (x) => x + 1\n").trim(), "val f = (x) => x + 1");
    // A single-call chain whose lambda arg is multi-line stays unsplit (receiver.method(...) on
    // one line); the multi-line lambda body flows beneath.
    let chain = fmt("val x = nodes.for(_ =>\n  set(adj, ai, [])\n  ai = ai + 1\n)\n");
    assert!(chain.contains("val x = nodes.for(_ =>"), "single-call chain split or lambda parenthesised:\n{}", chain);
    assert_eq!(chain, fmt(&chain), "chain not idempotent:\n{}", chain);
}

#[test]
fn test_fmt_preserves_postfix_base_parens() {
    // A binary-op (or other non-primary) used as a postfix base must stay parenthesised:
    // postfix `.`/`[]`/`()` bind tighter than binary operators.
    assert_eq!(fmt("val a = (x + y).foo()\n").trim(), "val a = (x + y).foo()");
    assert_eq!(fmt("val b = (x + y)[0]\n").trim(), "val b = (x + y)[0]");
    assert_eq!(fmt("val c = (f + g)(3)\n").trim(), "val c = (f + g)(3)");
    // Atomic / chain bases keep NO parens. (Lambda params are always parenthesised for
    // round-trip safety — ADR-006 / d6e7bdb.)
    assert_eq!(fmt("val d = arr.map(x => x).length()\n").trim(), "val d = arr.map(x => x).length()");
}

#[test]
fn test_fmt_preserves_radix_literals() {
    // The lexer discards the radix (stores only the value), so the formatter recovers the
    // original 0x/0b/0o spelling from source — flattening 0x0F to 15 would lose intent.
    assert_eq!(fmt("val m = 0x0F\n").trim(), "val m = 0x0F");
    assert_eq!(fmt("val m = 0xFF\n").trim(), "val m = 0xFF");
    assert_eq!(fmt("val b = 0b1010\n").trim(), "val b = 0b1010");
    assert_eq!(fmt("val o = 0o17\n").trim(), "val o = 0o17");
    // Decimal stays decimal; suffix, digit separators, and a negated hex literal preserved.
    assert_eq!(fmt("val d = 15\n").trim(), "val d = 15");
    assert_eq!(fmt("val s = 0xFFu8\n").trim(), "val s = 0xFFu8");
    assert_eq!(fmt("val g = 0xDEAD_BEEF\n").trim(), "val g = 0xDEAD_BEEF");
    assert_eq!(fmt("val n = -0x10\n").trim(), "val n = -0x10");
    // The motivating case: hex preserved AND the author's grouping parens preserved.
    assert_eq!(
        fmt("val packNibbles = (high: Int32, low: Int32): Int32 =>\n  (high << 4) | (low & 0x0F)\n").trim(),
        "val packNibbles = (high: Int32, low: Int32): Int32 =>\n  (high << 4) | (low & 0x0F)"
    );
    // Idempotent.
    assert_eq!(fmt("val m = 0x0F\n"), fmt(&fmt("val m = 0x0F\n")));
}

#[test]
fn test_fmt_preserves_generic_type_params() {
    // The `<T, U>` list is not in the surface text the parser keeps as tokens — the
    // formatter must re-emit it from `Expr::Function::type_params` or the body's T/U
    // become "Unknown type". This is exactly what broke stdlib `map`/`filter`/`reduce`.
    assert_eq!(
        fmt("val map = <T, U>(arr: T[], f: (T) => U): U[] => lin_map(arr, f)\n").trim(),
        "val map = <T, U>(arr: T[], f: (T) => U): U[] => lin_map(arr, f)"
    );
    assert_eq!(
        fmt("val id = <T>(x: T): T => x\n").trim(),
        "val id = <T>(x: T): T => x"
    );
    // A generic type APPLICATION (`Name<Args>` referencing a generic type) must round-trip with
    // angle brackets — NOT be rewritten to `Name[Args]` (array syntax), which changes meaning and
    // no longer parses. Regression for the std/event `val b: Bus<Int32> = …` corruption.
    assert_eq!(
        fmt("type Bus<T> = { \"v\": T }\nval mk = <T>(x: T): Bus<T> => { \"v\": x }\n").trim(),
        "type Bus<T> = { \"v\": T }\nval mk = <T>(x: T): Bus<T> => { \"v\": x }"
    );
}

#[test]
fn test_fmt_run_equivalence() {
    // The strongest guard: formatting must not change runtime behaviour. Compile+run a
    // program with precedence-sensitive arithmetic and a generic call both before and
    // after formatting; the output must be identical.
    let source = "import { print } from \"std/io\"\n\
import { toString } from \"std/string\"\n\
import { map, reduce, range } from \"std/iter\"\n\
val poly = (n: Int32): Int32 => (n + 1) * (n - 1) / 2\n\
val doubled = <T>(xs: T[], f: (T) => T): T[] => xs.map(f)\n\
val xs = range(1, 5).map(i => poly(i))\n\
val total = xs.reduce(0, (a, x) => a + x)\n\
print(toString(total))\n";
    let formatted = fmt(source);
    let before = run(source);
    let after = run(&formatted);
    assert_eq!(before, after, "formatting changed program output\nformatted:\n{}", formatted);
}

#[test]
fn test_fmt_parenthesized_function_return_type_round_trips() {
    // The formatter must round-trip a parenthesised function return type meaning-preservingly:
    // it may canonicalise `((AnyVal) => AnyVal)` to the redundant-paren-free `(AnyVal) => AnyVal`, but
    // the formatted output must re-parse, re-type-check, and produce the same runtime result.
    let source = "import { print } from \"std/io\"\n\
val mk = (h: AnyVal): ((AnyVal) => AnyVal) => (x: AnyVal): AnyVal => x\n\
val f = mk({})\n\
print(f(42))\n";
    let formatted = fmt(source);
    // Idempotent: formatting the formatted output is a fixed point.
    assert_eq!(formatted, fmt(&formatted), "formatter not idempotent\n{formatted}");
    // Run-equivalent: same output before and after formatting.
    let before = run(source);
    let after = run(&formatted);
    assert_eq!(before, vec!["42"]);
    assert_eq!(before, after, "formatting changed program output\nformatted:\n{formatted}");
}

#[test]
fn test_fmt_idempotent() {
    // Source with varied constructs: if/match/function/objects/arrays/imports/types.
    let source = r#"import { print } from "std/io"
import { map, filter, reduce, for } from "std/iter"
import { toString } from "std/string"

type Point = { "x": Int32, "y": Int32 }

val add = (a: Int32, b: Int32): Int32 => a + b

val describe = (n: Int32): String =>
  match n
    has Int32 when n > 0 => "positive"
    has Int32 when n < 0 => "negative"
    else => "zero"

val items = [1, 2, 3, 4, 5]

val doubled = items.map(x => x * 2)

val obj = { "name": "Alice", "age": 30 }

if true then
  print("hello")
else
  print("world")

val result = items.filter(x => x > 2).map(x => x * 10).reduce(0, (a, b) => a + b)
"#;

    let formatted_once = fmt(source);
    let formatted_twice = fmt(&formatted_once);

    assert_eq!(
        formatted_once, formatted_twice,
        "formatter is not idempotent!\nFirst pass:\n{}\nSecond pass:\n{}",
        formatted_once, formatted_twice
    );
}

#[test]
fn test_fmt_preserves_leading_comments() {
    // Own-line comments at top level and inside a block (function body) must survive
    // as leading lines on the statement that follows them, at that statement's indent.
    let source = r#"import { print } from "std/io"

// top-level leading comment
val x = 1

val f = (n: Int32): Int32 =>
  // in-block leading comment
  val y = n + 1
  y
"#;
    let out = fmt(source);
    assert!(out.contains("// top-level leading comment\nval x = 1"), "top-level leading comment lost or misplaced:\n{}", out);
    assert!(out.contains("  // in-block leading comment\n  val y = n + 1"), "in-block leading comment lost or misplaced:\n{}", out);
    // Idempotent.
    assert_eq!(out, fmt(&out), "leading-comment format not idempotent:\n{}", out);
}

#[test]
fn test_fmt_preserves_trailing_comments() {
    // A trailing comment after a single-line statement round-trips with exactly one space.
    let source = "val x = 1 // note\n";
    let out = fmt(source);
    assert!(out.contains("val x = 1 // note"), "trailing comment lost:\n{}", out);
    // Exactly one space before the comment (no double space, no tab).
    assert!(!out.contains("val x = 1  // note"), "trailing comment not canonicalised to one space:\n{}", out);
    // Idempotent.
    assert_eq!(out, fmt(&out), "trailing-comment format not idempotent:\n{}", out);
}

#[test]
fn test_fmt_else_if_chain_stays_flat() {
    // A flat `else if` chain must NOT nest one indent level deeper per arm (no
    // `else { if … else { if … } }` staircase). Each `else if` sits at the `if` indent.
    // The branch values are long enough that the chain cannot collapse to one inline line.
    let source = "val f = (kind: Int32): Int32 =>\n  if kind == 1 then 1000000 + 1000000\n  else if kind == 2 then 2000000 + 2000000\n  else if kind == 3 then 3000000 + 3000000\n  else 9000000 + 9000000\n";
    let out = fmt(source);
    assert!(out.contains("\n  else if kind == 2 then"), "else-if not flat:\n{}", out);
    assert!(out.contains("\n  else if kind == 3 then"), "else-if not flat:\n{}", out);
    // No deep staircase indent (8+ spaces before an `if`).
    assert!(!out.contains("        if kind =="), "else-if chain nested into a staircase:\n{}", out);
    assert_eq!(out, fmt(&out), "else-if chain format not idempotent:\n{}", out);
}

#[test]
fn test_fmt_preserves_inline_branch_comments() {
    // Trailing comments on each arm of a one-line `if/else if` chain must stay attached
    // to their branch body when the chain is re-rendered in block form.
    let source = "val f = (k: Int32): Int32 =>\n  if k == 1 then 10 // one\n  else if k == 2 then 20 // two\n  else 0 // other\n";
    let out = fmt(source);
    assert!(out.contains("10 // one"), "then-branch comment lost:\n{}", out);
    assert!(out.contains("20 // two"), "else-if-branch comment lost:\n{}", out);
    assert!(out.contains("0 // other"), "else-branch comment lost:\n{}", out);
    // All three comments survive (none dropped, none duplicated).
    assert_eq!(out.matches("//").count(), 3, "branch comment count changed:\n{}", out);
    assert_eq!(out, fmt(&out), "branch-comment format not idempotent:\n{}", out);
}

#[test]
fn test_fmt_comments_idempotent() {
    // A fixture mixing own-line, trailing, and indented in-block comments.
    let source = r#"import { print } from "std/io"

// leading on a val
val total = 10 // trailing on a val

val classify = (n: Int32): String =>
  // explain the branch below
  val label = if n > 0 then "pos" else "nonpos"
  label // trailing inside a block
"#;
    let pass1 = fmt(source);
    let pass2 = fmt(&pass1);
    assert_eq!(pass1, pass2, "comment formatting not idempotent\npass1:\n{}\npass2:\n{}", pass1, pass2);
    // All four comments survive.
    for needle in ["// leading on a val", "// trailing on a val", "// explain the branch below", "// trailing inside a block"] {
        assert!(pass1.contains(needle), "comment {:?} dropped:\n{}", needle, pass1);
    }
}

#[test]
fn test_fmt_rule1_chain_threshold() {
    // Rule 1: a chain with MORE than CHAIN_INLINE_MAX (2) calls is always multiline,
    // regardless of length. 4 calls → one `.method(...)` per line.
    let source = "import { range, map, filter, reduce } from \"std/array\"\nval total = range(0, n).map(x => x * 2).filter(x => x % 3 == 0).reduce(0, (acc, x) => acc + x)\n";
    let out = fmt(source);
    let expected = "val total = range(0, n)\n  .map(x => x * 2)\n  .filter(x => x % 3 == 0)\n  .reduce(0, (acc, x) => acc + x)";
    assert!(out.contains(expected), "Rule 1 chain not multiline:\n{}", out);
    // A 2-call chain still stays inline.
    let two = fmt("import { range, map } from \"std/array\"\nval a = range(0, n).map(x => x)\n");
    assert!(two.contains("val a = range(0, n).map(x => x)"), "2-call chain should stay inline:\n{}", two);
    assert_eq!(out, fmt(&out), "Rule 1 not idempotent:\n{}", out);
}

#[test]
fn test_fmt_rule2_preserve_blank_lines() {
    // Rule 2: a source blank between two statements is preserved; adjacent statements
    // stay tight (no auto-injected blank); runs of 2+ blanks collapse to one.
    let adjacent = "import { print } from \"std/io\"\nimport { toString } from \"std/string\"\n";
    let out = fmt(adjacent);
    assert_eq!(out, "import { print } from \"std/io\"\nimport { toString } from \"std/string\"\n", "adjacent imports must stay tight:\n{:?}", out);

    let with_blank = "val a = 1\n\nval b = 2\n";
    assert_eq!(fmt(with_blank), "val a = 1\n\nval b = 2\n", "source blank not preserved");

    let many_blanks = "val a = 1\n\n\n\nval b = 2\n";
    assert_eq!(fmt(many_blanks), "val a = 1\n\nval b = 2\n", "blank run not collapsed to one");

    let no_blank = "val a = 1\nval b = 2\n";
    assert_eq!(fmt(no_blank), "val a = 1\nval b = 2\n", "blank auto-injected between adjacent vals");

    assert_eq!(fmt(with_blank), fmt(&fmt(with_blank)), "Rule 2 not idempotent");
}

#[test]
fn test_fmt_rule3_no_trailing_commas() {
    // Rule 3 (formatter half): multiline array/object literals have NO trailing comma.
    let arr = "val xs = [1000000000, 2000000000, 3000000000, 4000000000, 5000000000, 6000000000]\n";
    let out = fmt(arr);
    assert!(out.contains('\n'), "array should be multiline:\n{}", out);
    assert!(!out.contains(",\n]"), "trailing comma before ]:\n{}", out);
    let obj = "val o = { \"aaaaaaaaaaaaaaaaaaaaaaa\": 1, \"bbbbbbbbbbbbbbbbbbbbbbb\": 2, \"ccccccccccccccccccc\": 3 }\n";
    let out2 = fmt(obj);
    assert!(out2.contains('\n'), "object should be multiline:\n{}", out2);
    assert!(!out2.contains(",\n}"), "trailing comma before }}:\n{}", out2);
    assert_eq!(out, fmt(&out), "Rule 3 array not idempotent");
    assert_eq!(out2, fmt(&out2), "Rule 3 object not idempotent");
}

#[test]
fn test_fmt_rule4_recursive_multiline() {
    // Author-intent layout: a nested literal the AUTHOR broke across lines stays multi-line; a
    // nested literal the author wrote INLINE stays inline (we never roll a multi-line literal up,
    // and never roll an author-inline one out). Here the author broke every level → fully nested.
    let source = "val node = {\n  \"node\": {\n    \"kind\": \"num\",\n    \"value\": tokens[pos][\"text\"].parseInt32()\n  },\n  \"pos\": pos + 1\n}\n";
    let out = fmt(source);
    assert_eq!(out, source, "author-multilined nested object not preserved:\n{}", out);
    assert_eq!(out, fmt(&out), "not idempotent:\n{}", out);

    // Author wrote the inner object INLINE inside a multi-line outer array → inner stays inline.
    let mixed = "val fields = [\n  { \"tag\": 1, \"bytes\": [72, 105] },\n  { \"tag\": 2, \"bytes\": [255, 0, 128] }\n]\n";
    let mout = fmt(mixed);
    assert_eq!(mout, mixed, "author-inline inner literals not preserved:\n{}", mout);
    assert_eq!(mout, fmt(&mout), "mixed not idempotent:\n{}", mout);
}

#[test]
fn test_fmt_rule5a_trailing_lambda() {
    // Rule 5a: a call whose last arg is a multiline lambda with an array body keeps
    // `() => [` together on the call line; earlier short args stay inline. The array body
    // has two elements so it genuinely renders multiline.
    let source = "val t = test(\"evaluates a single number\", () => [expect(valueOf(\"forty-two-ish\")).toBe(42), expect(valueOf(\"seventy-seven\")).toBe(77)])\n";
    let out = fmt(source);
    assert!(out.contains("test(\"evaluates a single number\", () => [\n"), "Rule 5a `() => [` not kept together:\n{}", out);
    // The `=> [` collapse puts the array's `]` at the call indent, so the call's `)` glues
    // directly → the close reads `])` (no stray `)` line).
    assert!(out.contains("\n  expect(valueOf(\"forty-two-ish\")).toBe(42),\n  expect(valueOf(\"seventy-seven\")).toBe(77)\n])"), "Rule 5a body/close not laid out:\n{}", out);
    assert_eq!(out, fmt(&out), "Rule 5a not idempotent:\n{}", out);
}

#[test]
fn test_fmt_rule5b_fully_split_args() {
    // Rule 5b: when a NON-last arg is a multiline lambda, fully split the arg list —
    // open paren, each arg on its own line at child indent, multiline lambda renders
    // `param =>` then body indented, close paren on its own line.
    let source = "var total = 0\nval acc = worker(n =>\n  total = total + n\n  total, () => null)\n";
    let out = fmt(source);
    let expected = "val acc = worker(\n  n =>\n    total = total + n\n    total,\n  () => null\n)";
    assert!(out.contains(expected), "Rule 5b fully-split layout wrong:\n{}", out);
    assert_eq!(out, fmt(&out), "Rule 5b not idempotent:\n{}", out);
}

#[test]
fn test_fmt_rule6_comment_hoist() {
    // Rule 6: a comment between a lambda's `=>` and its array/object body is hoisted to be
    // a leading comment of the enclosing statement, and `() => [` collapses to one line.
    let source = "import { test, expect, toBe, tokenize } from \"std/test\"\ntest(\"tokenizes each arithmetic operator\", () =>\n  // --- Operators ---\n  [\n    expect(tokenize(\"+\")[0][\"kind\"]).toBe(\"op\"),\n    expect(tokenize(\"-\")[0][\"kind\"]).toBe(\"op\")\n  ])\n";
    let out = fmt(source);
    assert!(out.contains("// --- Operators ---\ntest("), "Rule 6 comment not hoisted above statement:\n{}", out);
    assert!(out.contains("() => [\n"), "Rule 6 `() => [` not collapsed:\n{}", out);
    assert_eq!(out.matches("//").count(), 1, "Rule 6 comment count changed:\n{}", out);
    assert_eq!(out, fmt(&out), "Rule 6 not idempotent:\n{}", out);
}

#[test]
fn test_fmt_rulei_iii_test_array_full_target() {
    // Full target for the three test-suite-array rules combined: comment hoisted above the
    // first test (Rule ii), closing `)` on its own line (Rule i), and a blank line between the
    // two consecutive `test(...)` elements (Rule iii). The short second test stays inline.
    let source = "import { test, expect, toBe, suite } from \"std/test\"\nval s = suite(\"bits\", [\n  test(\"plain bitwise operators\", () =>\n    // --- Plain bitwise operators ---\n    [\n      expect(12 & 10).toBe(8),\n      expect(12 | 10).toBe(14),\n      expect(255 >> 4).toBe(15)\n    ]),\n  test(\"UInt8 flat array holds 0..255 unboxed\", () => [expect(1).toBe(1)])\n])\n";
    let out = fmt(source);
    let expected = "import { test, expect, toBe, suite } from \"std/test\"\nval s = suite(\"bits\", [\n  // --- Plain bitwise operators ---\n  test(\"plain bitwise operators\", () =>\n    [\n      expect(12 & 10).toBe(8),\n      expect(12 | 10).toBe(14),\n      expect(255 >> 4).toBe(15)\n    ]\n  ),\n\n  test(\"UInt8 flat array holds 0..255 unboxed\", () => [expect(1).toBe(1)])\n])\n";
    assert_eq!(out, expected, "full target mismatch:\n{}", out);
    assert_eq!(out.matches("//").count(), 1, "comment count changed:\n{}", out);
    assert_eq!(out, fmt(&out), "not idempotent:\n{}", out);
}

#[test]
fn test_fmt_rulei_close_paren_own_line() {
    // A call whose last arg is a lambda with an array body: `() => [` collapses (Rule 6, body
    // attached), the body breaks, and since the `]` lands at the call indent the closing `)`
    // glues → `])`.
    let source = "val t = test(\"plain bitwise operators\", () =>\n  [\n    expect(valueOf(\"forty-two-ish\")).toBe(42),\n    expect(valueOf(\"seventy-seven\")).toBe(77)\n  ])\n";
    let out = fmt(source);
    let expected = "val t = test(\"plain bitwise operators\", () => [\n  expect(valueOf(\"forty-two-ish\")).toBe(42),\n  expect(valueOf(\"seventy-seven\")).toBe(77)\n])\n";
    assert_eq!(out, expected, "Rule i close-paren layout wrong:\n{}", out);
    assert_eq!(out, fmt(&out), "Rule i not idempotent:\n{}", out);
}

#[test]
fn test_fmt_rulei_single_line_lambda_keeps_paren_glued() {
    // Rule i scope: a single-line lambda arg (body does NOT span multiple lines) keeps the `)`
    // glued — the close-paren-on-own-line rule applies only to multi-line lambda bodies.
    let source = "val t = test(\"name\", () => [expect(1).toBe(1)])\n";
    let out = fmt(source);
    assert_eq!(out, "val t = test(\"name\", () => [expect(1).toBe(1)])\n", "single-line lambda `)` not glued:\n{}", out);
    assert_eq!(out, fmt(&out), "not idempotent:\n{}", out);
}

#[test]
fn test_fmt_rulei_comment_in_array_hoist() {
    // Rule ii: a comment between a lambda's `=>` and its array body, where the lambda is the
    // last arg of a `test(...)` array element, hoists to a leading comment of that element.
    let source = "val s = suite(\"bitwise behaviour\", [\n  test(\"plain bitwise operators\", () =>\n    // note A\n    [\n      expect(120000 & 100000).toBe(34464),\n      expect(255000 >> 4).toBe(15937)\n    ])\n])\n";
    let out = fmt(source);
    assert!(out.contains("  // note A\n  test(\"plain bitwise operators\", () =>\n"), "Rule ii comment not hoisted above element:\n{}", out);
    assert_eq!(out.matches("//").count(), 1, "Rule ii comment count changed:\n{}", out);
    assert_eq!(out, fmt(&out), "Rule ii not idempotent:\n{}", out);
}

#[test]
fn test_fmt_ruleiii_blank_between_test_elements() {
    // Rule iii: exactly one blank line between two consecutive `test(...)` array elements,
    // even when the source had none. Idempotent (a second pass adds no further blank).
    let source = "val s = suite(\"x\", [\n  test(\"alpha case\", () =>\n    [\n      expect(120000 & 100000).toBe(34464),\n      expect(255000 >> 4).toBe(15937)\n    ]),\n  test(\"beta case\", () =>\n    [\n      expect(120000 | 100000).toBe(185536),\n      expect(4 << 8).toBe(1024)\n    ])\n])\n";
    let out = fmt(source);
    assert!(out.contains("  ),\n\n  test(\"beta case\""), "Rule iii blank not injected between tests:\n{}", out);
    assert_eq!(out, fmt(&out), "Rule iii not idempotent:\n{}", out);
    // No double blank on a re-sweep.
    assert!(!out.contains("\n\n\n"), "Rule iii produced a double blank:\n{}", out);
}

#[test]
fn test_fmt_ruleiii_non_test_array_gets_no_blank() {
    // Rule iii scope: a non-`test` call array gets NO blank line injected between elements.
    let source = "val xs = [\n  foo(\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\", () =>\n    [\n      aaaaaaaaaaaaaaa(1),\n      bbbbbbbbbbbbbbb(2)\n    ]),\n  foo(\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\", () =>\n    [\n      ccccccccccccccc(3),\n      ddddddddddddddd(4)\n    ])\n]\n";
    let out = fmt(source);
    assert!(!out.contains("\n\n"), "non-test array got a blank line injected:\n{}", out);
    assert_eq!(out, fmt(&out), "non-test array not idempotent:\n{}", out);
}

#[test]
fn test_fmt_ruleA_author_multiline_if_stays_multiline() {
    // Rule A: an `if` the author wrote multiline (then/else on their own lines) stays
    // multiline (block form) even though it would fit on one line.
    let source = "val sumTo = (n: Int32, acc: Int32): Int32 =>\n  if n == 0 then acc\n  else sumTo(n - 1, acc + n)\n";
    let out = fmt(source);
    let expected = "val sumTo = (n: Int32, acc: Int32): Int32 =>\n  if n == 0 then\n    acc\n  else\n    sumTo(n - 1, acc + n)\n";
    assert_eq!(out, expected, "Rule A author-multiline if collapsed:\n{}", out);
    assert_eq!(out, fmt(&out), "Rule A not idempotent:\n{}", out);
}

#[test]
fn test_fmt_ruleA_author_inline_if_stays_inline() {
    // Rule A: an `if` the author wrote on one line stays inline.
    let source = "val inlineIf = (n: Int32): Int32 => if n == 0 then 1 else 2\n";
    let out = fmt(source);
    assert_eq!(out, "val inlineIf = (n: Int32): Int32 => if n == 0 then 1 else 2\n", "Rule A author-inline if changed:\n{}", out);
    assert_eq!(out, fmt(&out), "Rule A inline not idempotent:\n{}", out);
}

#[test]
fn test_fmt_ruleB_author_newline_body_stays_on_own_line() {
    // Rule B: a function whose body the author put on a NEW line keeps it on its own
    // indented line, not collapsed onto the `=> body` single line.
    let source = "val fail = (msg: String): Failure =>\n  { \"type\": \"failure\", \"error\": msg }\n";
    let out = fmt(source);
    let expected = "val fail = (msg: String): Failure =>\n  { \"type\": \"failure\", \"error\": msg }\n";
    assert_eq!(out, expected, "Rule B author-newline body collapsed:\n{}", out);
    assert_eq!(out, fmt(&out), "Rule B not idempotent:\n{}", out);
}

#[test]
fn test_fmt_ruleB_author_inline_body_stays_inline() {
    // Rule B: a function whose body the author wrote inline after `=>` stays inline.
    let source = "val fail = (msg: String): Failure => { \"type\": \"failure\", \"error\": msg }\n";
    let out = fmt(source);
    assert_eq!(out, "val fail = (msg: String): Failure => { \"type\": \"failure\", \"error\": msg }\n", "Rule B author-inline body changed:\n{}", out);
    assert_eq!(out, fmt(&out), "Rule B inline not idempotent:\n{}", out);
}

#[test]
fn test_fmt_ruleC_author_multiline_2chain_stays_multiline() {
    // Rule C: a 2-call chain the author broke across lines stays multiline.
    let source = "import { range, map, reduce } from \"std/array\"\nimport { toString, length } from \"std/string\"\nval totalLen = range(0, n)\n  .map(i => \"item-${toString(i)}\")\n  .reduce(0, (acc, s) => acc + length(s))\n";
    let out = fmt(source);
    let expected = "val totalLen = range(0, n)\n  .map(i => \"item-${toString(i)}\")\n  .reduce(0, (acc, s) => acc + length(s))";
    assert!(out.contains(expected), "Rule C author-multiline 2-chain collapsed:\n{}", out);
    assert_eq!(out, fmt(&out), "Rule C not idempotent:\n{}", out);
}

#[test]
fn test_fmt_ruleC_author_inline_2chain_stays_inline() {
    // Rule C: a 2-call chain the author wrote inline (that fits) stays inline.
    let source = "import { range, map, reduce } from \"std/array\"\nval a = range(0, n).map(f).reduce(0, g)\n";
    let out = fmt(source);
    assert!(out.contains("val a = range(0, n).map(f).reduce(0, g)"), "Rule C author-inline 2-chain changed:\n{}", out);
    assert_eq!(out, fmt(&out), "Rule C inline not idempotent:\n{}", out);
}

#[test]
fn test_fmt_ruleC_over_two_chain_always_multiline() {
    // Rule C / Rule 1: a chain with >2 calls is ALWAYS multiline even if written inline.
    let source = "import { range, map, filter, reduce } from \"std/array\"\nval t = range(0, n).map(x => x).filter(x => x > 0).reduce(0, (a, b) => a + b)\n";
    let out = fmt(source);
    let expected = "val t = range(0, n)\n  .map(x => x)\n  .filter(x => x > 0)\n  .reduce(0, (a, b) => a + b)";
    assert!(out.contains(expected), "Rule C >2 chain not multiline:\n{}", out);
    assert_eq!(out, fmt(&out), "Rule C >2 not idempotent:\n{}", out);
}

#[test]
fn test_fmt_overbudget_test_lambda_keeps_name_on_call_line() {
    // An over-80 `test("long name", () => [ … ])` must NOT fully-split the arg list (which
    // would strand the name on its own line and lose `=> [`). The lambda's array body breaks
    // instead, keeping the name + `=>` on the call line.
    let long = "n".repeat(60);
    // Single-element body → `=> [` collapse with `])` close.
    let single = format!(
        "import {{ test, expect, toBe, suite }} from \"std/test\"\nval s = suite(\"x\", [test(\"{}\", () => [expect(f(1)).toBe(5)])])\n",
        long
    );
    let so = fmt(&single);
    assert!(so.contains(&format!("test(\"{}\", () => [", long)), "name/=> [ left the call line:\n{}", so);
    assert!(so.contains("\n  ])"), "single-element close should be `])`:\n{}", so);
    assert!(!so.contains("\n    () =>"), "arg list should NOT fully-split:\n{}", so);
    assert_eq!(so, fmt(&so), "not idempotent:\n{}", so);

    // Multi-element body → `=>` then body on its own line, `)` dedented.
    let multi = format!(
        "import {{ test, expect, toBe, suite }} from \"std/test\"\nval s = suite(\"x\", [test(\"{}\", () => [expect(f(1)).toBe(5), expect(f(2)).toBe(9)])])\n",
        long
    );
    let mo = fmt(&multi);
    assert!(mo.contains(&format!("test(\"{}\", () =>", long)), "name/=> left the call line:\n{}", mo);
    assert_eq!(mo, fmt(&mo), "multi not idempotent:\n{}", mo);
}

#[test]
fn test_fmt_opt_in_match_alignment() {
    // Opt-in: a match whose `=>` the author column-aligned stays aligned, with padding
    // recomputed from the FORMATTED head widths (so string-key shorthand reflow is fine).
    let source = "val f = (s: AnyVal): String =>\n  match s\n    has { \"circle\" } when big => \"a\"\n    has { \"rect\" }            => \"bb\"\n    else                      => \"c\"\n";
    let out = fmt(source);
    // Pull the three arm lines (indented, no `val`) and check the `=>` byte offset matches.
    let arrow_cols: Vec<usize> = out
        .lines()
        .filter(|l| l.contains("=>") && (l.contains("has") || l.trim_start().starts_with("else")))
        .map(|l| l.find("=>").unwrap())
        .collect();
    assert_eq!(arrow_cols.len(), 3, "expected 3 aligned arms:\n{}", out);
    assert!(
        arrow_cols.iter().all(|&c| c == arrow_cols[0]),
        "match `=>` not column-aligned: {:?}\n{}",
        arrow_cols,
        out
    );
    // The widest head `has { circle } when big` keeps exactly one space before `=>`.
    assert!(out.contains("has { circle } when big => \"a\""), "widest head not single-spaced:\n{}", out);
    assert_eq!(out, fmt(&out), "aligned match not idempotent:\n{}", out);

    // A single-spaced match stays single-spaced (no opt-in signal).
    let single = "val g = (s: AnyVal): String =>\n  match s\n    has { \"circle\" } => \"a\"\n    has { \"rect\" } => \"bb\"\n    else => \"c\"\n";
    let so = fmt(single);
    assert!(so.contains("has { rect } => \"bb\""), "single-spaced match changed:\n{}", so);
    assert!(!so.contains("  => "), "single-spaced match got padded:\n{}", so);
    assert_eq!(so, fmt(&so), "single-spaced match not idempotent:\n{}", so);
}

#[test]
fn test_fmt_opt_in_trailing_comment_alignment() {
    // Opt-in: a val-run inside a function body with author-aligned trailing comments keeps
    // them aligned — the widest code part has one space, narrower ones are padded.
    let source = "val setup = (): Int32 =>\n  val a = 1       // first\n  val bb = 2      // second\n  val ccc = 3     // third\n  a + bb + ccc\n";
    let out = fmt(source);
    let comment_cols: Vec<usize> = out
        .lines()
        .filter(|l| l.contains("//"))
        .map(|l| l.find("//").unwrap())
        .collect();
    assert_eq!(comment_cols.len(), 3, "expected 3 aligned comments:\n{}", out);
    assert!(
        comment_cols.iter().all(|&c| c == comment_cols[0]),
        "trailing comments not aligned: {:?}\n{}",
        comment_cols,
        out
    );
    // Widest code is `  val ccc = 3` — it keeps a single space before its `//`.
    assert!(out.contains("val ccc = 3 // third"), "widest member not single-spaced:\n{}", out);
    assert_eq!(out, fmt(&out), "aligned trailing comments not idempotent:\n{}", out);

    // Single-spaced trailing comments stay single-spaced.
    let single = "val setup = (): Int32 =>\n  val a = 1 // first\n  val bb = 2 // second\n  val ccc = 3 // third\n  a + bb + ccc\n";
    let so = fmt(single);
    assert!(so.contains("val a = 1 // first"), "single-spaced trailing changed:\n{}", so);
    assert!(!so.contains("val a = 1  //"), "single-spaced trailing got padded:\n{}", so);
    assert_eq!(so, fmt(&so), "single-spaced trailing not idempotent:\n{}", so);
}

#[test]
fn test_fmt_opt_in_toplevel_trailing_alignment() {
    // Opt-in at TOP LEVEL (no enclosing function): an aligned val-run stays aligned.
    let source = "val a = 1       // first\nval bb = 2      // second\nval ccc = 3     // third\n";
    let out = fmt(source);
    let comment_cols: Vec<usize> = out
        .lines()
        .filter(|l| l.contains("//"))
        .map(|l| l.find("//").unwrap())
        .collect();
    assert_eq!(comment_cols.len(), 3, "expected 3 aligned top-level comments:\n{}", out);
    assert!(
        comment_cols.iter().all(|&c| c == comment_cols[0]),
        "top-level trailing comments not aligned: {:?}\n{}",
        comment_cols,
        out
    );
    assert!(out.contains("val ccc = 3 // third"), "widest top-level member not single-spaced:\n{}", out);
    assert_eq!(out, fmt(&out), "aligned top-level trailing not idempotent:\n{}", out);
}

/// Parse a source string and return the parser diagnostics' messages (no panic on errors).
fn parse_diagnostics(source: &str) -> Vec<String> {
    let mut lexer = lin_lex::Lexer::new(source, 0);
    let tokens = lexer.tokenize();
    let mut parser = lin_parse::Parser::new(tokens);
    let _ = parser.parse_module();
    parser.diagnostics.iter().map(|d| d.message.clone()).collect()
}

#[test]
fn test_fmt_rule3_parser_rejects_trailing_commas() {
    // Rule 3 (parser half): a trailing comma in an array/object LITERAL is a parse error.
    let arr = parse_diagnostics("val x = [1, 2,]\n");
    assert!(arr.iter().any(|m| m.contains("trailing comma is not allowed in array")),
        "array trailing comma not rejected: {:?}", arr);
    let obj = parse_diagnostics("val o = { \"a\": 1, }\n");
    assert!(obj.iter().any(|m| m.contains("trailing comma is not allowed in object")),
        "object trailing comma not rejected: {:?}", obj);
    // A function call `f(x,)` (partial application, ADR-026) is STILL accepted.
    let call = parse_diagnostics("val g = f(x,)\n");
    assert!(call.is_empty(), "f(x,) partial application must stay valid: {:?}", call);
    // Non-trailing commas are fine.
    assert!(parse_diagnostics("val x = [1, 2, 3]\n").is_empty());
    assert!(parse_diagnostics("val o = { \"a\": 1, \"b\": 2 }\n").is_empty());
}

#[test]
fn test_fmt_preserves_partial_application_comma() {
    // BUG: the formatter never read the `partial` flag and dropped the trailing comma,
    // turning a partial application `add(1,)` into a different-typed full call `add(1)`.
    // That changes program meaning, violating the formatter's core invariant.
    // Single-line plain call.
    let out = fmt("val f = add(1,)\n");
    assert!(out.contains("add(1,)"), "dropped partial comma (Call); got:\n{}", out);
    assert!(parse_diagnostics(&out).is_empty(), "formatted output must re-parse: {:?}\n{}", parse_diagnostics(&out), out);
    // Single-line dot call.
    let outd = fmt("val f = x.add(1,)\n");
    assert!(outd.contains("add(1,)"), "dropped partial comma (DotCall); got:\n{}", outd);
    // Idempotent: re-formatting keeps the comma.
    assert_eq!(out, fmt(&out), "partial-comma format not idempotent:\n{}", out);
    // A normal (non-partial) call must NOT gain a spurious trailing comma.
    let plain = fmt("val f = add(1)\n");
    assert!(!plain.contains(",)"), "spurious trailing comma added; got:\n{}", plain);
}

/// Count `//` occurrences in a string (proxy for comment count for the corpus sanity check).
fn count_comments(s: &str) -> usize {
    s.matches("//").count()
}

#[test]
fn test_fmt_corpus_idempotent_and_comments_preserved() {
    // Corpus guard: format every stdlib/*.lin and examples/**/*.lin twice; pass1 must equal
    // pass2 (idempotency over the real corpus). Also assert that a single format does not
    // change the `//` count of any file (no comment loss).
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();

    let mut files: Vec<std::path::PathBuf> = Vec::new();
    // `docs-site/builder` is outside CI's fmt scope but exercises block-bodied `val` and
    // own-line comments inside `else if` Block branch bodies — the two bugs this guard now
    // covers. Its files use sibling imports, so they hit the idempotency + comment-count
    // checks (not the standalone type-check, which `is_self_contained` skips for them).
    for dir in ["stdlib", "examples", "docs-site/builder"] {
        let pattern = format!("{}/{}/**/*.lin", root.display(), dir);
        for entry in glob::glob(&pattern).unwrap().flatten() {
            if entry.components().any(|c| c.as_os_str() == ".lin-cache") {
                continue;
            }
            files.push(entry);
        }
    }
    assert!(files.len() > 50, "expected the corpus to have many files, found {}", files.len());

    let mut non_idempotent: Vec<String> = Vec::new();
    let mut comment_changed: Vec<String> = Vec::new();
    // Formatted output that no longer type-checks — the miscompile guard. Re-emitting the
    // AST drops parentheses and the generic `<T>` list; if the formatter doesn't restore
    // them the formatted file fails `lin check` (e.g. "Unknown type 'T'"). Idempotency
    // alone does NOT catch this — broken-but-stable output passes the checks above.
    let mut check_failed: Vec<String> = Vec::new();

    for path in &files {
        let src = std::fs::read_to_string(path).unwrap();
        let before = count_comments(&src);
        let pass1 = fmt(&src);
        let pass2 = fmt(&pass1);
        if pass1 != pass2 {
            non_idempotent.push(path.display().to_string());
        }
        let after = count_comments(&pass1);
        if before != after {
            comment_changed.push(format!("{} ({} -> {})", path.display(), before, after));
        }
        // Type-check the formatted text standalone. Only for SELF-CONTAINED files (no
        // relative/sibling imports) — a temp file in the workspace root resolves `std/...`
        // and `foreign` imports against the embedded stdlib, but not `./sibling` modules,
        // which would spuriously fail to resolve. All stdlib/*.lin are self-contained, so
        // the generics-critical code (map/filter/reduce/<T>) is fully covered; sibling-
        // importing example files rely on idempotency + the targeted run-equivalence test.
        if is_self_contained(&src) && lin_check_ok(path) && !lin_check_ok_source(&pass1) {
            check_failed.push(path.display().to_string());
        }
    }

    assert!(
        non_idempotent.is_empty(),
        "formatter not idempotent on corpus files: {:?}",
        non_idempotent
    );
    assert!(
        comment_changed.is_empty(),
        "comment count changed when formatting corpus files: {:?}",
        comment_changed
    );
    assert!(
        check_failed.is_empty(),
        "formatted output no longer type-checks (miscompile!) for: {:?}",
        check_failed
    );
}

/// True if the source has no relative/sibling import (only `std/...` or `foreign`), so it
/// can be type-checked as a standalone temp file in the workspace root.
fn is_self_contained(source: &str) -> bool {
    !source.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with("import") && (t.contains("\"./") || t.contains("\"../"))
    })
}

/// `lin check <path>` succeeds.
fn lin_check_ok(path: &std::path::Path) -> bool {
    lin_cmd()
        .args(["check", path.to_str().unwrap()])
        .current_dir(workspace_root())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `lin check` succeeds on a source string (written to a temp file alongside the source
/// dir so relative imports still resolve). Used to check formatted output in place.
fn lin_check_ok_source(source: &str) -> bool {
    // Write into the workspace root so `std/...` imports resolve via the embedded stdlib;
    // corpus files use only std imports or sibling modules, and a root temp file can't see
    // siblings — so multi-module files are checked via the real path in lin_check_ok and
    // this single-file check focuses on the formatted-text validity of self-contained files.
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let p = ws.join(format!("target/fmt_check_{}.lin", id));
    std::fs::write(&p, source).unwrap();
    let ok = lin_cmd()
        .args(["check", p.to_str().unwrap()])
        .current_dir(&ws)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    let _ = std::fs::remove_file(&p);
    ok
}

#[test]
fn test_bitwise_basic_ops() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

print(toString(5 & 3))
print(toString(5 | 2))
print(toString(5 ^ 1))
print(toString(1 << 4))
print(toString(256 >> 2))
print(toString(~0))
"#);
    assert_eq!(output, vec!["1", "7", "4", "16", "64", "-1"]);
}

#[test]
fn test_bitwise_precedence() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

// & binds tighter than |  =>  1 | (2 & 3) == 1 | 2 == 3
print(toString(1 | 2 & 3))
// shift looser than +  =>  (1 + 1) << 2 == 8
print(toString(1 + 1 << 2))
// hex masking
print(toString(0xFF & 0x0F))
"#);
    assert_eq!(output, vec!["3", "8", "15"]);
}

#[test]
fn test_bitwise_nal_masking() {
    // The NAL-type extraction example from spec §27.2.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val header = 0x67
print(toString(header & 0x1F))
"#);
    assert_eq!(output, vec!["7"]);
}

#[test]
fn test_bitwise_boxed_operands() {
    // Bitwise ops on reduce-lambda params, which arrive boxed (TypeVar). The boxed
    // operand must be unboxed before the LLVM int op — regression for a panic where
    // `.into_int_value()` was called on a pointer value.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { reduce } from "std/iter"

print(toString([1, 2, 4, 8].reduce(0, (acc, x) => acc | x)))
print(toString([15, 7, 3].reduce(255, (acc, x) => acc & x)))
print(toString([1, 2, 3].reduce(1, (acc, x) => acc << x)))
"#);
    assert_eq!(output, vec!["15", "3", "64"]);
}

#[test]
fn test_bitwise_boxed_projection_operand() {
    // Regression: a bitwise op whose operand is a boxed-AnyVal projection (`bytes[i]` out of a
    // AnyVal array), used in a recursive call argument, must unbox the operand before the LLVM
    // integer op. Previously only Add/Sub/Mul/Div/Mod unboxed union operands; bitwise ops did
    // not, so the boxed `TaggedVal*` reached codegen as an int operand → codegen type-mismatch
    // crash. A recursive XOR checksum exercises exactly this path.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"

val checksum = (bytes: AnyVal, i: Int32, acc: Int32): Int32 =>
  if i >= length(bytes) then acc
  else checksum(bytes, i + 1, acc ^ bytes[i])

print(toString(checksum([1, 2, 3], 0, 0)))
print(toString(checksum([255, 1, 2], 0, 0)))
"#);
    // 1^2^3 = 0 ; 255^1^2 = 252
    assert_eq!(output, vec!["0", "252"]);
}

#[test]
fn test_bitwise_xor_precedence() {
    // `^` binds between `&` and `|`:  1 | 6 ^ 3 & 2  ==  1 | (6 ^ (3 & 2))  ==  1 | (6 ^ 2)  ==  1 | 4  ==  5
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

print(toString(1 | 6 ^ 3 & 2))
"#);
    assert_eq!(output, vec!["5"]);
}

#[test]
fn test_bitwise_float_operand_rejected() {
    // A floating-point operand to a bitwise operator is a compile-time type error.
    let err = run_expect_err(r#"import { print } from "std/io"
import { toString } from "std/string"

val x = 3.0 & 1
print(toString(x))
"#);
    assert!(
        err.contains("requires integer operand"),
        "expected a bitwise integer-operand type error, got:\n{}",
        err
    );
}

#[test]
fn test_concrete_rc_cell_reassignment_in_loop() {
    // Regression: reassigning a concrete reference-counted (here String) `var` inside a
    // closure must release the cell's OLD value and retain the NEW one, so refcounts stay
    // balanced over many reassignments. Before the fix the old value's reference was dropped
    // on the floor (leak) and the cell aliased a scope-released value (use-after-free /
    // garbage output). A 5000-iteration loop would corrupt or leak; with the fix it runs
    // cleanly and yields the final value deterministically.
    let output = run(r#"import { print } from "std/io"
import { for, range } from "std/iter"
import { trim, repeat } from "std/string"

val build = (): String =>
  var acc = "seed"
  range(0, 5000).for(i =>
    acc = trim(repeat("x", 3))
    0
  )
  acc

print(build())
"#);
    assert_eq!(output, vec!["xxx"]);
}

#[test]
fn test_concrete_rc_global_var_reassignment_in_loop() {
    // Same fix, exercised through the top-level `var` (module-global) path: a concrete-rc
    // global reassigned inside a closure must release its old value and retain the new one.
    let output = run(r#"import { print } from "std/io"
import { for, range } from "std/iter"
import { repeat } from "std/string"

var acc = "seed"
range(0, 5000).for(i =>
  acc = repeat("y", 2)
  0
)
print(acc)
"#);
    assert_eq!(output, vec!["yy"]);
}

#[test]
fn test_nested_generics_still_parse() {
    // Regression: `>>` shift detection (two ADJACENT `Gt` tokens in VALUE position) must
    // NOT break nested generic type close `>>` in TYPE position. Generic types are parsed
    // by a separate path that closes each level with expect(Gt), so the adjacent `> >` of a
    // nested generic must remain two independent tokens. We assert the parser produces no
    // diagnostics for several nested-generic annotations.
    let source = r#"type Box<T> = { "value": T }
val a: Box<Box<Int32>> = { "value": { "value": 1 } }
val b: Box<Box<Box<Int32>>> = { "value": { "value": { "value": 2 } } }
val c: Array<Array<Int32>> = [[1, 2], [3, 4]]
"#;
    let tokens = lin_lex::Lexer::new(source, 0).tokenize();
    let mut parser = lin_parse::Parser::new(tokens);
    let _module = parser.parse_module();
    assert!(
        parser.diagnostics.is_empty(),
        "nested generics regressed under `>>` shift parsing: {:?}",
        parser.diagnostics.iter().map(|d| d.message.clone()).collect::<Vec<_>>(),
    );
}

#[test]
fn test_nested_array_type_postfix() {
    // Regression: the postfix `[]` type suffix must repeat for nested arrays. `T[][]` is
    // `Array(Array(T))`; a single `if` only matched one `[]`, so `Int32[][]` / `UInt8[][]`
    // failed to parse ("expected Eq, got LBracket"). The `Array<Array<T>>` generic form
    // already worked; the postfix form must too.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"

val a: Int32[][] = [[1, 2], [3, 4]]
val b: UInt8[][] = [[255], [0, 128]]
val c: String[][][] = [[["x"]]]
print(toString(a[1][0]))
print(toString(length(b)))
print(c[0][0][0])
"#);
    assert_eq!(out, vec!["3", "2", "x"]);
}

#[test]
fn test_generic_alias_single_param() {
    // A user generic type alias `Box<T>` type-checks AND runs end-to-end: the param `T` is
    // bound while resolving the declaration body, so `Box<Int32>` substitutes correctly.
    let out = run(r#"import { print } from "std/io"
type Box<T> = { "value": T }
val a: Box<Int32> = { "value": 5 }
print("${a["value"]}")
"#);
    assert_eq!(out, vec!["5"]);
}

#[test]
fn test_generic_alias_nested_application() {
    // Nested application `Box<Box<Int32>>`: substitution recurses through the alias body.
    let out = run(r#"import { print } from "std/io"
type Box<T> = { "value": T }
val b: Box<Box<Int32>> = { "value": { "value": 7 } }
print("${b["value"]["value"]}")
"#);
    assert_eq!(out, vec!["7"]);
}

#[test]
fn test_generic_alias_multi_param() {
    // A multi-param alias `Pair<A, B>`: each param resolves independently at the use-site.
    let out = run(r#"import { print } from "std/io"
type Pair<A, B> = { "fst": A, "snd": B }
val p: Pair<Int32, String> = { "fst": 3, "snd": "hi" }
print("${p["fst"]} ${p["snd"]}")
"#);
    assert_eq!(out, vec!["3 hi"]);
}

#[test]
fn test_generic_tagged_union_match_has() {
    // A multi-param GENERIC TAGGED UNION `Result<T, E>` consumed with match/has: substitution
    // applies inside every union variant, and field-presence narrowing discriminates them.
    let out = run(r#"import { print } from "std/io"
type Result<T, E> = { "value": T } | { "error": E }
val describe = (r: Result<Int32, String>): String =>
  match r
    has { "value" } => "ok:${r["value"]}"
    has { "error" } => "err:${r["error"]}"
    else => "?"
val ok: Result<Int32, String> = { "value": 42 }
val bad: Result<Int32, String> = { "error": "boom" }
print(describe(ok))
print(describe(bad))
"#);
    assert_eq!(out, vec!["ok:42", "err:boom"]);
}

#[test]
fn test_uint8_flat_array_roundtrip() {
    // UInt8[] is an unboxed flat byte array: literals, length, index, push and print all
    // round-trip values without wrapping (255 stays 255, not -1).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { push, length } from "std/array"

val buf: UInt8[] = [1, 2, 255]
print(toString(length(buf)))
print(toString(buf[2]))
push(buf, 42)
print(toString(buf[3]))
print(toString(buf))
"#);
    assert_eq!(out, vec!["3", "255", "42", "[1, 2, 255, 42]"]);
}

#[test]
fn test_uint8_flat_array_index_assign() {
    // In-place index assignment on a flat UInt8 array writes through to the raw buffer.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val buf: UInt8[] = [1, 2, 255]
buf[1] = 200
print(toString(buf[1]))
print(toString(buf))
"#);
    assert_eq!(out, vec!["200", "[1, 200, 255]"]);
}

#[test]
fn test_int8_flat_array_negatives() {
    // Int8[] stores signed bytes; negative literals round-trip. Regression: a `-` immediately
    // after `[` (no space) must lex as a negative literal — `[-1, ...]` — not a `0 - 1`
    // subtraction (which types as Int32 and fails to narrow to Int8). Both spacings now work.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val nospace: Int8[] = [-1, -128, 127]
print(toString(nospace[0]))
print(toString(nospace[1]))
val space: Int8[] = [ -2, 100]
print(toString(space[0]))
"#);
    assert_eq!(out, vec!["-1", "-128", "-2"]);

    // The fix must NOT turn index-position subtraction into a literal: `a[i-1]` and `a[i - 1]`
    // still subtract (the `-` follows `i`, not `[`).
    let idx = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val a = [10, 20, 30]
val i = 2
print(toString(a[i-1]))
print(toString(a[i - 1]))
"#);
    assert_eq!(idx, vec!["20", "20"]);
}

#[test]
fn test_uint16_flat_array() {
    // UInt16[] is a 2-byte-per-element flat array; large values round-trip.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val w: UInt16[] = [1000, 65535]
print(toString(w[0]))
print(toString(w[1]))
"#);
    assert_eq!(out, vec!["1000", "65535"]);
}

#[test]
fn test_uint32_flat_array_unsigned_display() {
    // Regression: a flat UInt32[] whole-array toString must render elements UNSIGNED
    // (4294967295), not as a signed -1. Single-element index must also be unsigned.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val a: UInt32[] = [4294967295, 1]
print(toString(a))       // whole-array JSON
print(toString(a[0]))    // single element (scalar box path)
"#);
    assert_eq!(out, vec!["[4294967295, 1]", "4294967295"]);
}

#[test]
fn test_uint64_flat_array_unsigned_display() {
    // A flat UInt64[] renders its high-bit element unsigned, not negative.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val b: UInt64[] = [18446744073709551615, 0]
print(toString(b))
print(toString(b[0]))
"#);
    assert_eq!(out, vec!["[18446744073709551615, 0]", "18446744073709551615"]);
}

#[test]
fn test_int32_flat_array_signed_display_unchanged() {
    // Guard: signed Int32[] still renders signed (negative) — the UInt32/UInt64 unsigned
    // fix must not regress the signed flat families. (Int64 negative-literal display via
    // `0 - 1` has a separate, pre-existing literal-width bug unrelated to this change, so
    // it is not asserted here.)
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val s: Int32[] = [0 - 1, 2]
print(toString(s))
print(toString(s[0]))
"#);
    assert_eq!(out, vec!["[-1, 2]", "-1"]);
}

#[test]
fn test_uint32_flat_array_equality() {
    // Structural equality over flat UInt32 arrays (exercises lin_flat_array_eq_u32).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val a: UInt32[] = [1, 4294967295]
val b: UInt32[] = [1, 4294967295]
val c: UInt32[] = [1, 3]
print(toString(a == b))
print(toString(a == c))
"#);
    assert_eq!(out, vec!["true", "false"]);
}

#[test]
fn test_uint8_flat_array_equality() {
    // Structural equality over flat UInt8 arrays (exercises lin_flat_array_eq_u8).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val a: UInt8[] = [1, 2]
val b: UInt8[] = [1, 2]
val c: UInt8[] = [1, 3]
print(toString(a == b))
print(toString(a == c))
"#);
    assert_eq!(out, vec!["true", "false"]);
}

#[test]
fn test_uint8_literal_out_of_range_rejected() {
    // A suffixless integer literal that does not fit the target small-integer type's range
    // is a compile-time error (spec §21 context-typed literal + range check).
    let err = run_expect_err(r#"import { print } from "std/io"
val bad: UInt8[] = [256]
print("unreachable")
"#);
    assert!(
        err.contains("out of range for type UInt8"),
        "expected an out-of-range literal error, got:\n{}",
        err
    );
}

#[test]
fn test_int8_scalar_out_of_range_rejected() {
    // Scalar literal range check for a signed small integer.
    let err = run_expect_err(r#"import { print } from "std/io"
val bad: Int8 = -129
print("unreachable")
"#);
    assert!(
        err.contains("out of range for type Int8"),
        "expected an out-of-range literal error, got:\n{}",
        err
    );
}

#[test]
fn test_bare_literal_overflowing_int32_preserved() {
    // Regression: a bare integer literal larger than the default Int32 range, with no wider
    // context, used to SILENTLY TRUNCATE to its low 32 bits (1705314600000 -> 212583488).
    // It must now default to the smallest type that PRESERVES the value (Int64 here), so the
    // full value survives — no truncation, and no annotation required.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val c = 1705314600000
print(toString(c))
val big = 3000000000   // > Int32 max, fits Int64
print(toString(big))
"#);
    assert_eq!(out, vec!["1705314600000", "3000000000"]);
}

#[test]
fn test_i64_suffix_preserves_large_literal() {
    // An `i64` suffix pins the literal to Int64 (spec §2.6), so a value beyond Int32's range
    // is preserved exactly rather than truncated. (The suffix used to be lexed then discarded.)
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
print(toString(1705314600000i64))
val x = 1705314600000i64
print(toString(x + 1i64))
"#);
    assert_eq!(out, vec!["1705314600000", "1705314600001"]);
}

#[test]
fn test_mixed_int32_int64_arithmetic_widens_int32_operand() {
    // Regression (LIN_ISSUES #3): `x * 1000003i64` where `x: Int32` used to compute the
    // product in Int32 (overflowing to -194043216) and only THEN widen the result to Int64.
    // The `i64` literal operand was being re-typed DOWN to Int32 to match `x` before the op.
    // A mixed Int32 * Int64 op must now widen the Int32 operand to Int64 so the arithmetic
    // happens at Int64. Cover both operand orders, +, and -. Pure-Int32 arithmetic must STILL
    // wrap (semantics unchanged): 90000 * 50000 = 4_500_000_000 wraps to 205032704 in Int32.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val run = (): Null =>
  val x = 90000
  val mulR: Int64 = x * 1000003i64
  val mulL: Int64 = 1000003i64 * x
  val add: Int64 = x + 3000000000i64
  val sub: Int64 = 5000000000i64 - x
  val pureI32 = 90000 * 50000
  print("${toString(mulR)} ${toString(mulL)} ${toString(add)} ${toString(sub)} ${toString(pureI32)}")
run()
"#);
    assert_eq!(out, vec!["90000270000 90000270000 3000090000 4999910000 205032704"]);
}

#[test]
fn test_int64_annotation_preserves_large_literal() {
    // The annotation route to the same value: `: Int64` gives the literal Int64 context.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val ts: Int64 = 1705314600000
print(toString(ts))
"#);
    assert_eq!(out, vec!["1705314600000"]);
}

#[test]
fn test_suffix_overrides_expected_context_conflict() {
    // A suffix pins the type; assigning an i64-suffixed literal to an Int32 binding is a
    // type error (the suffix wins over context, then compatibility is checked) — not a
    // silent reinterpretation.
    let err = run_expect_err(r#"import { print } from "std/io"
val x: Int32 = 5i64
print("unreachable")
"#);
    assert!(
        err.contains("Int32") && (err.contains("Int64") || err.contains("Expected")),
        "expected a type-mismatch error for i64 suffix into Int32, got:\n{}",
        err
    );
}

#[test]
fn test_nonliteral_int32_to_uint8_still_rejected() {
    // A NON-literal Int32 value assigned to UInt8 is still a narrowing error: literal
    // context-typing must not loosen the numeric-compatibility rules for computed values.
    let err = run_expect_err(r#"import { print } from "std/io"
val x: Int32 = 100
val y: UInt8 = x
print("unreachable")
"#);
    assert!(
        err.contains("Expected type UInt8") || err.contains("UInt8"),
        "expected a narrowing type error, got:\n{}",
        err
    );
}

#[test]
fn test_smallint_value_with_bare_literal_arith() {
    // A small-int value combined with a bare integer literal must keep the small-int width:
    // the literal adopts the operand's type (spec §21) so no spurious widening crashes codegen
    // and the arithmetic result is correct.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val a: UInt8 = 250
print(toString(a + 5))
val header: UInt8 = 0x67
print(toString(header & 0x1F))
"#);
    assert_eq!(out, vec!["255", "7"]);
}

#[test]
fn test_smallint_array_elem_with_bare_literal_bitwise() {
    // Bitwise/shift ops between a UInt8[] element and a bare literal stay byte-width.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val buf: UInt8[] = [255, 4, 8]
print(toString(buf[0] & 0x0F))
print(toString(buf[1] << 1))
print(toString(buf[2] >> 1))
"#);
    assert_eq!(out, vec!["15", "8", "4"]);
}

#[test]
fn test_int32_bitwise_with_literal_unchanged() {
    // Plain Int32 bitwise arithmetic against literals is unaffected by the small-int rule.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"

print(toString(255 & 15))
print(toString(0x3 << 5 | 0x07))
"#);
    assert_eq!(out, vec!["15", "103"]);
}

#[test]
fn test_smallint_binop_literal_out_of_range_rejected() {
    // A bare literal operand that doesn't fit the small-int operand's range in an arithmetic
    // op is a compile-time error (the literal is context-typed to the operand width).
    let err = run_expect_err(r#"import { print } from "std/io"
import { toString } from "std/string"

val a: UInt8 = 250
print(toString(a + 300))
"#);
    assert!(
        err.contains("out of range for type UInt8"),
        "expected an out-of-range literal error in a small-int binop, got:\n{}",
        err
    );
}

#[test]
fn test_json_var_object_reassign_loop_no_uaf() {
    // Regression for the union var-cell use-after-free: a captured `var` of union (AnyVal) type
    // reassigned to a freshly-allocated OBJECT literal each iteration. Before the owning model
    // (clone-on-store/read, release-old, balanced teardown) the cell aliased a temp object that
    // was freed at closure-scope exit, so the final read saw freed/garbage memory.
    let out = run(r#"import { range, for } from "std/iter"
import { print } from "std/io"
import { toString } from "std/string"

var acc: AnyVal = { "v": 0 }
range(0, 2000).for(i => acc = { "v": i })
print(toString(acc["v"]))
"#);
    assert_eq!(out, vec!["1999"]);
}

#[test]
fn test_json_var_array_reassign_loop_no_uaf() {
    // Same bug, ARRAY literal variant: a captured `var: AnyVal` reassigned to a fresh array each
    // iteration. A use-after-free here corrupted the length read (or crashed).
    let out = run(r#"import { length } from "std/array"
import { range, for } from "std/iter"
import { print } from "std/io"
import { toString } from "std/string"

var acc: AnyVal = [0, 0, 0]
range(0, 2000).for(i => acc = [i, i, i])
print(toString(length(acc)))
"#);
    assert_eq!(out, vec!["3"]);
}

#[test]
fn test_reduce_minby_maxby_churn_no_double_free() {
    // Exercises the stdlib `reduce` AnyVal accumulator cell plus the pass-through reducers used
    // by `minBy`/`maxBy` (which return a borrowed argument). The earlier half-fix (owning store
    // but borrowing read) double-freed these borrowed values. With the symmetric clone-based
    // owning model the accumulator cell owns its own box and never frees the borrowed inputs.
    // 2000 iterations of sum/min/max over churned arrays — a double-free corrupts results or
    // aborts the process.
    let out = run(r#"import { minBy, maxBy, length } from "std/array"
import { range, for, map, reduce } from "std/iter"
import { print } from "std/io"
import { toString } from "std/string"

var total: AnyVal = 0
range(0, 2000).for(i =>
  val xs = [i, i + 1, i + 2, i - 5]
  val s = xs.reduce(0, (acc, x) => acc + x)
  total = s
)
print(toString(total))

val pairs = range(0, 2000).map(i => { "k": i, "w": (i * 7) % 13 })
val lo = pairs.minBy(p => p["w"])
val hi = pairs.maxBy(p => p["w"])
print(toString(lo["w"]))
print(toString(hi["w"]))
"#);
    // Last iter i=1999: 1999 + 2000 + 2001 + 1994 = 7994.
    // minBy/maxBy over (i*7)%13: minimum weight 0, maximum weight 12.
    assert_eq!(out, vec!["7994", "0", "12"]);
}

#[test]
fn test_generic_combinator_pipeline_inlined() {
    // ADR-044: generic map/filter/reduce + the capture-less-lambda inliner. The monomorphic scalar
    // pipeline `range(0,n).map(x=>x*2).filter(x=>x%3==0).reduce(0,(a,x)=>a+x)` lowers to a fully
    // unboxed flat loop (verified separately: zero per-element box/unbox in `main`). Here we assert
    // the VALUE is correct over a small n so a representation/RC bug in the inliner shows up.
    let out = run(r#"import { range, map, filter, reduce } from "std/iter"
import { print } from "std/io"
import { toString } from "std/string"
val total = range(0, 10).map(x => x * 2).filter(x => x % 3 == 0).reduce(0, (a, x) => a + x)
print(toString(total))
"#);
    // range 0..10 -> *2 = [0,2,4,6,8,10,12,14,16,18]; %3==0 -> [0,6,12,18]; sum = 36.
    assert_eq!(out, vec!["36"]);
}

#[test]
fn test_generic_combinator_inline_vs_closure_paths() {
    // ADR-044: the inliner fires ONLY for a capture-less literal lambda; a capturing lambda and a
    // stored/passed `Function` value must keep the (correct, boxed) closure path. Also exercises the
    // tagged String element path and a non-scalar (array) reduce accumulator (the boxed AnyVal-phi
    // path). All four must produce the right values.
    let out = run(r#"import { map, filter, reduce } from "std/iter"
import { print } from "std/io"
import { toString } from "std/string"

// capture-less literal -> inline path
print(toString([1, 2, 3].map(x => x + 1)))
// capturing lambda -> closure path (captures k)
val k = 100
print(toString([1, 2, 3].map(x => x + k)))
// stored fn value -> closure path
val dbl = (x: Int32): Int32 => x * 2
print(toString([1, 2, 3].map(dbl)))
// tagged String elements
print(toString(["a", "bb", "ccc"].filter(s => true)))
// non-scalar (array) reduce accumulator -> boxed AnyVal-phi path
print(toString([1, 2, 3].reduce([0], (a, x) => a)))
"#);
    assert_eq!(
        out,
        vec!["[2, 3, 4]", "[101, 102, 103]", "[2, 4, 6]", r#"["a", "bb", "ccc"]"#, "[0]"]
    );
}

#[test]
fn test_concat_fresh_strings_no_use_after_free() {
    // Regression: `lin_array_concat_dyn`'s tagged path copied each element's TaggedVal WITHOUT
    // retaining its heap payload, so `acc = concat(acc, [freshString])` in a loop left the result
    // and the freed temp/old-acc sharing one payload at refcount 1 → use-after-free / heap
    // corruption (only masked when the elements are interned string literals). The tagged-source
    // copy now retains; the result owns its elements independently. Uses interpolated (non-interned
    // per-iteration) strings so the elements are genuinely heap-owned and a missing retain faults.
    let out = run(r#"import { print } from "std/io"
import { length } from "std/array"
import { concat, range, for } from "std/iter"
import { toString } from "std/string"
val mk = (n: Int32): String => "item-${n}-${n * 13}"
var acc: String[] = []
range(0, 40).for(n =>
  acc = concat(acc, [mk(n)])
)
print(toString(length(acc)))
print(acc[0])
print(acc[39])
"#);
    assert_eq!(out, vec!["40", "item-0-0", "item-39-507"]);
}

#[test]
fn test_for_callback_json_assign_loop_correct() {
    // Regression for the for-callback-return box leak fix. The `for` callback's boxed-ABI
    // return is now released every iteration. For a body that is an ASSIGNMENT to a captured
    // `var: AnyVal` (`acc = concat(acc, [i])`), the assignment expression's result is the value
    // that ALSO flows into the cell; the fix makes the global/cell own a CLONED, independent
    // box and returns an independently-owned box, so the per-iteration release frees exactly the
    // discarded return and never the value the cell keeps. Over 5000 iterations a wrong release
    // (double-free / use-after-free) corrupts the final length or aborts. The final array must
    // contain all 5000 appended elements.
    let out = run(r#"import { length } from "std/array"
import { range, for, concat } from "std/iter"
import { print } from "std/io"
import { toString } from "std/string"

var acc: AnyVal = []
range(0, 5000).for(i => acc = concat(acc, [i]))
print(toString(length(acc)))
"#);
    assert_eq!(out, vec!["5000"]);
}

#[test]
fn test_for_callback_side_effect_sum_loop_correct() {
    // Regression for the for-callback-return box leak: a side-effecting body that mutates a
    // captured non-AnyVal `var` (`s = s + i`). The callback boxes its result for the uniform ABI
    // each iteration (a fresh, independently-owned box once `s` grows past the small-int cache);
    // the fix releases that discarded box every iteration. Correctness must be unaffected:
    // sum(0..10000) = 10000*9999/2 = 49995000.
    let out = run(r#"import { range, for } from "std/iter"
import { print } from "std/io"
import { toString } from "std/string"

var s = 0
range(0, 10000).for(i => s = s + i)
print(toString(s))
"#);
    assert_eq!(out, vec!["49995000"]);
}

#[test]
fn test_for_element_box_flat_array_churn_correct() {
    // Regression for the for-element-ARGUMENT box leak. Each `for` iteration boxes the flat
    // Int32 element into a fresh `TaggedVal*` for the AnyVal callback param; that per-iteration box
    // was leaked (~36 B/iter). The fix reclaims the box shell every iteration via
    // `lin_tagged_free_box_if_distinct` (skipping when the callback returned that very box, e.g.
    // an identity body). Over 50000 iterations correctness must be unaffected: a wrong (double)
    // free would abort or corrupt the accumulator. sum(0..50000) = 50000*49999/2 = 1249975000.
    let out = run(r#"import { range, for } from "std/iter"
import { print } from "std/io"
import { toString } from "std/string"

var s = 0
range(0, 50000).for(i => s = s + i)
print(toString(s))
"#);
    assert_eq!(out, vec!["1249975000"]);
}

#[test]
fn test_for_element_box_tagged_array_churn_correct() {
    // Regression for the for-element box reclaim on a TAGGED array (heap-inner String elements).
    // Here the per-iteration element box wraps a refcounted String; reclaiming only the box SHELL
    // (never the inner) must NOT corrupt the source array — the strings stay owned by `xs` and are
    // read again on every pass. Also covers a callback that PASSES the element to another function
    // (`contains`), proving the shared inner is intact. 20000 passes over the 3-element array; a
    // wrong inner release would free a live string and abort/corrupt the count.
    let out = run(r#"import { for, range } from "std/iter"
import { contains } from "std/string"
import { print } from "std/io"
import { toString } from "std/string"

val xs = ["alpha", "beta", "gamma"]
var total = 0
range(0, 20000).for(j => xs.for(s => if contains(s, "a") then total = total + 1 else total = total))
print(toString(total))
"#);
    // "alpha", "beta", "gamma" all contain "a" → 3 per pass * 20000 = 60000.
    assert_eq!(out, vec!["60000"]);
}

#[test]
fn test_to_uint8_narrowing() {
    // std/number toUInt8 truncates a wider integer to a byte (two's-complement / `as`).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { toUInt8 } from "std/number"

val v: UInt32 = 0x11223344
print(toString(toUInt8((v >> 24) & 0xFF)))   // 17 (0x11)
print(toString(toUInt8(0x1FF)))               // 255 (truncated)
print(toString(toUInt8(256)))                 // 0 (wraps)
"#);
    assert_eq!(out, vec!["17", "255", "0"]);
}

#[test]
fn test_slice_preserves_element_type() {
    // slice dispatches on the array's runtime element type: a UInt8[] yields a UInt8[]
    // (indexes without sign wrap), an Int32[] an Int32[], a tagged array a tagged array.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { slice, length } from "std/array"

val bytes: UInt8[] = [10, 200, 30, 40, 50]
val sub: UInt8[] = slice(bytes, 1, 4)
print(toString(length(sub)))   // 3
print(toString(sub[0]))        // 200 (no sign wrap → still flat u8)

val ints: Int32[] = [100, 200, 300, 400]
print(toString(slice(ints, 2, 4)[0]))   // 300

val words = ["a", "b", "c", "d"]
print(slice(words, 0, 2)[1])   // b
"#);
    assert_eq!(out, vec!["3", "200", "300", "b"]);
}

#[test]
fn test_concat_preserves_flat_element_type() {
    // concat dispatches on element type: two flat UInt8[] yield a flat UInt8[], so a
    // byte-level consumer (u32FromBe reads `(*arr).data as *const u8`) sees packed bytes.
    // Previously concat always built a TAGGED array (16-byte elements), so u32FromBe read
    // TaggedVal bytes and decoded garbage (e.g. 33554432 instead of 2864434397).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"
import { concat } from "std/iter"
import { u32FromBe } from "std/bytes"

val a: UInt8[] = [170, 187]
val b: UInt8[] = [204, 221]
val c = concat(a, b)
print(toString(length(c)))          // 4
print(toString(c[0]))               // 170 (element access)
print(toString(u32FromBe(c, 0)))    // 2864434397 = 0xAABBCCDD (byte-level read)

val ia: Int32[] = [10, 20]
print(toString(concat(ia, [30, 40])[2]))   // 30 (Int32[] stays flat)

val sa = ["x", "y"]
print(concat(sa, ["z"])[2])         // z (tagged stays tagged)

val flat: UInt8[] = [1, 2]
print(toString(concat(flat, ["a"])[0]))  // 1 (mixed → tagged, value preserved)
"#);
    assert_eq!(out, vec!["4", "170", "2864434397", "30", "z", "1"]);
}

#[test]
fn test_append_prepend_basic_and_representation() {
    // append/prepend are runtime intrinsics (lin_array_append_dyn / _prepend_dyn) that
    // PRESERVE the input's representation. AnyVal[] stays AnyVal[]; a flat UInt8[]/Int32[] stays
    // flat (proven byte-level via u32FromBe, which reads `(*arr).data as *const u8` — a tagged
    // result would decode garbage); String[] stays tagged and its strings survive RC retain.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { append, prepend, length } from "std/array"
import { u32FromBe } from "std/bytes"

// AnyVal[] (tagged scalars)
val nums = [1, 2, 3]
print(toString(append(nums, 4)))     // [1, 2, 3, 4]
print(toString(prepend(nums, 0)))    // [0, 1, 2, 3]
print(toString(length(append(nums, 4))))  // 4

// flat UInt8[] — latent-bug check: index AND byte-level read must be correct.
val b: UInt8[] = [170, 187, 204]
val ap: UInt8[] = append(b, 221)     // [170,187,204,221] = 0xAABBCCDD
print(toString(ap[3]))               // 221 (element access)
print(toString(u32FromBe(ap, 0)))    // 2864434397 (packed bytes ⇒ still flat)
val bb: UInt8[] = [187, 204, 221]
val pp: UInt8[] = prepend(bb, 170)   // [170,187,204,221]
print(toString(u32FromBe(pp, 0)))    // 2864434397 (prepend also stays flat)

// flat Int32[]
val ia: Int32[] = [10, 20]
print(toString(append(ia, 30)[2]))   // 30
print(toString(prepend(ia, 5)[0]))   // 5

// String[] (tagged, RC) — strings print correctly after retain.
val ss = ["a", "b"]
print(append(ss, "c")[2])            // c
print(prepend(ss, "z")[0])           // z
"#);
    assert_eq!(
        out,
        vec![
            "[1, 2, 3, 4]", "[0, 1, 2, 3]", "4",
            "221", "2864434397", "2864434397",
            "30", "5",
            "c", "z",
        ]
    );
}

// Generic push/append/prepend are `<T>(arr: T[], item: T)` (ADR-059), so the element type is
// enforced — closing the prior soundness hole where a `AnyVal` `push` accepted any item. Pushing a
// String into an Int32[] is now a COMPILE ERROR.
#[test]
fn test_generic_push_element_type_hole_closed() {
    let err = run_expect_err(r#"import { push } from "std/array"
val intArr: Int32[] = [1, 2, 3]
push(intArr, "str")
"#);
    // T unifies to String from the item, so the Int32[] container mismatches.
    assert!(
        err.contains("String") && err.contains("Int32"),
        "push(intArr, \"str\") must be a type error mentioning the element-type mismatch, got: {err}"
    );
    // append likewise enforces the element type.
    let err2 = run_expect_err(r#"import { append } from "std/array"
val intArr: Int32[] = [1, 2, 3]
append(intArr, "str")
"#);
    assert!(err2.contains("String") && err2.contains("Int32"),
        "append(intArr, \"str\") must error, got: {err2}");
}

// `sort`/`sortBy`/`minBy`/`maxBy` are now generic over the element `T` (`sort` is
// `<T>(arr: T[], cmp: (T, T) => Int32): T[]`), so the comparator/keyFn is element-type-checked at
// the call site — closing the prior soundness hole where a `AnyVal` `cmp` accepted any operation on
// its arguments. A comparator that indexes a field the element type lacks is now a COMPILE ERROR.
#[test]
fn test_generic_sort_comparator_element_type_hole_closed() {
    // A comparator typed for String elements, applied to an Int32[]: the comparator's parameter
    // type now pins `T = String`, which mismatches the `Int32[]` array argument. Under the old
    // `cmp: (AnyVal, AnyVal) => Int32` signature this was SILENTLY ACCEPTED (a String comparator was
    // assignable to a AnyVal comparator); it is now a compile error mentioning the element mismatch.
    let err = run_expect_err(r#"import { sort } from "std/array"
val xs: Int32[] = [1, 2, 3]
val cmp = (a: String, b: String): Int32 => if a < b then -1 else 1
val r = sort(xs, cmp)
"#);
    assert!(
        err.contains("Int32") && err.contains("String"),
        "sort(Int32[], stringComparator) must be an element-type error mentioning Int32/String, got: {err}"
    );
}

// The typed RESULT of a generic `sort` preserves the element type: `[3,1,2].sort(...)` is an
// `Int32[]`, so a follow-on `push(intArr, intLiteral)` type-checks while `push(intArr, "s")` does
// not. This proves the element type flows OUT of `sort` (not erased to `AnyVal`).
#[test]
fn test_generic_sort_result_element_type_preserved() {
    // Pushing an Int32 into the sorted Int32[] is fine and reads back.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { sort, push } from "std/array"
val sorted = [3, 1, 2].sort((a, b) => a - b)
push(sorted, 4)
print(toString(sorted))
"#);
    assert_eq!(out, vec!["[1, 2, 3, 4]"]);

    // Pushing a String into the sorted Int32[] is a compile error (T = Int32 flowed through sort).
    let err = run_expect_err(r#"import { sort, push } from "std/array"
val sorted = [3, 1, 2].sort((a, b) => a - b)
push(sorted, "s")
"#);
    assert!(
        err.contains("String") && err.contains("Int32"),
        "push(sortedIntArr, \"s\") must be an element-type error, got: {err}"
    );
}

// `sort` over a `[]`+push (tagged-read) array sorts correctly. The inline scalar-sort fast path
// reads each source element via the representation-agnostic tagged path (`lin_array_get_tagged`),
// which returns a fresh +1 box that the copy-in loop unboxes to the flat buffer; that box must be
// reclaimed per element (it was leaked one box/element/sort — the ~16 B/elem `sort` result leak).
// This asserts correctness of that path; the leak itself is gated by the ASan harness.
#[test]
fn test_sort_over_push_built_array_correct() {
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { sort, push, length } from "std/array"
val xs: Int32[] = []
push(xs, 5)
push(xs, 2)
push(xs, 8)
push(xs, 1)
push(xs, 9)
push(xs, 3)
val sorted = sort(xs, (a, b) => a - b)
print(toString(sorted))
print(toString(length(sorted)))
"#);
    assert_eq!(out, vec!["[1, 2, 3, 5, 8, 9]", "6"]);
}

// Regression (sealed-record combinator element leak): `map` over a sealed-record array whose
// callback returns a SCALAR FIELD (`x => x["a"]`) reads each element via the `Index` op, which
// materialises a FRESH +1 sealed struct per element (packed-array `sealed_array_materialize_elem`
// or boxed-array `sealed_project_from`, both retaining their heap fields). The body extracts a copy
// of one scalar field — the struct itself is NEVER moved into the (`Int32[]`) result — so the lowerer
// must release it each iteration (the new `free_combinator_sealed_elem`) or it leaks one struct per
// element, per `map` call (ASan-confirmed linear across all sealed field shapes; the same applies to
// `for`/`while`/`reduce` over a sealed array). cargo test can't see the leak; this guards that the
// per-element release is CORRECT — an over-eager release would free a still-referenced field and
// corrupt the result or crash. Run in a loop so a per-iteration double-free would surface as a wrong
// total / abort. The ASan stdlib+example leg + the sealed harness guard the no-double-free half.
#[test]
fn test_map_scalar_field_over_sealed_record_array_in_loop() {
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { push, length } from "std/array"
import { map } from "std/iter"

type T = { "a": Int32, "b": Int32 }

val once = (i: Int32): Int32 =>
  var ts: T[] = []
  push(ts, { "a": i, "b": 0 })
  push(ts, { "a": i + 10, "b": 0 })
  val ds: Int32[] = map(ts, (x) => x["a"])
  ds[0] + ds[1]

val loop = (i: Int32, n: Int32, acc: Int32): Int32 =>
  if i >= n then acc
  else loop(i + 1, n, acc + once(i))

print(toString(loop(0, 1000, 0)))
"#);
    // sum over i in 0..1000 of (i + (i+10)) = 2*sum(0..999) + 10*1000 = 999000 + 10000 = 1009000
    assert_eq!(out, vec!["1009000"]);
}

// `minBy`/`maxBy`/`sortBy` over an OBJECT array still work as before (the genericization keeps the
// heterogeneous `[key, item]` pair path sound — pairs built via the raw `lin_map` builtin on the
// `T` ABI, the sorted result unpacked back into a `T[]` in the generic body).
#[test]
fn test_generic_keyed_array_fns_over_objects() {
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { sortBy, minBy, maxBy } from "std/array"
val people = [{ "name": "Bob", "age": 30 }, { "name": "Alice", "age": 25 }, { "name": "Cy", "age": 40 }]
val byName = people.sortBy(p => p["name"])
print(byName[0]["name"])
print(byName[2]["name"])
val youngest = people.minBy(p => p["age"])
print(toString(youngest["age"]))
val oldest = people.maxBy(p => p["age"])
print(toString(oldest["age"]))
"#);
    assert_eq!(out, vec!["Alice", "Cy", "25", "40"]);
}

// A bare integer-LITERAL item adopts the array's element WIDTH (the literal-width inference fix):
// `b.append(3)` on a `UInt8[]` stays `UInt8[]` (not `Int32[]`), preserving the flat representation.
#[test]
fn test_generic_append_literal_width_adopts_element_type() {
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { append, prepend } from "std/array"
val b: UInt8[] = [10, 20]
val r: UInt8[] = b.append(30)
print(toString(r[2]))
val p: UInt8[] = b.prepend(5)
print(toString(p[0]))
"#);
    assert_eq!(out, vec!["30", "5"]);
}

// SILENT DATA-LOSS regression: `push(obj[k], rec)` into an array stored inside a AnyVal
// object/map field, where `rec`'s record type is packable. The packable element pinned the
// generic `push`'s `T` to a packed-sealed element, selecting the `push$Obj_…` specialization
// whose arg coercion MATERIALIZED a fresh detached packed buffer (`lin_sealed_array_alloc`) from
// the boxed array the container holds — the push mutated the copy, the stored array stayed empty,
// and `length(obj[k])` re-read it as 0 (silent drop). The fix routes an in-place-mutator receiver
// that is a container index-read through the boxed `$AnyVal` path (`lin_push_dyn`), mutating the
// REAL stored array. Asserts both the corrected length AND element read-back through the field.
#[test]
fn test_push_into_json_object_field_array_reads_back() {
    let out = run(r#"import { print } from "std/io"
import { length, push } from "std/array"
type Pt = { "x": Int32, "y": Int32 }
val mk = (x: Int32, y: Int32): Pt => { "x": x, "y": y }
val main = (): Null =>
  var obj: AnyVal = {}
  obj["k"] = []
  push(obj["k"], mk(3, 4))
  push(obj["k"], mk(5, 6))
  print("${length(obj["k"])}")
  print("${obj["k"][0]["x"]}")
  print("${obj["k"][1]["y"]}")
main()
"#);
    assert_eq!(out, vec!["2", "3", "6"]);
}

// A generic `push` of a CONCRETE-OBJECT element into a record-typed array (`Field[]`) reads back
// correctly. The element is materialized into a boxed LinObject for the tagged array (gap #2: a
// sealed-projected struct must NOT be stored raw under TAG_OBJECT — it crashed at object.rs:195 /
// a misaligned scalar deref). Covers both an exact-type item and a field-WIDENING item (UInt8 →
// the Int32 field of Field).
#[test]
fn test_generic_push_concrete_object_element_reads_back() {
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { push } from "std/array"
type Field = { "tag": Int32, "bytes": Int32[] }
val out: Field[] = []
push(out, { "tag": 5, "bytes": [1, 2, 3] })
val b: UInt8[] = [7, 8]
push(out, { "tag": b[0], "bytes": b })
print(toString(out[0]["tag"]))
print(toString(out[1]["tag"]))
print(toString(out[1]["bytes"]))
"#);
    assert_eq!(out, vec!["5", "7", "[7, 8]"]);
}

// A generic `push`/`append` of a genuinely-`AnyVal` (dynamic) element into a CONCRETE flat-scalar
// array monomorphizes DYNAMICALLY (`$AnyVal` → lin_push_dyn coerces the boxed element into the flat
// slot at runtime), matching the non-generic `push` behaviour. Previously the concrete `push$UInt8`
// monomorph received a raw boxed AnyVal pointer it box_value'd as a scalar (`zext ptr` codegen error).
#[test]
fn test_generic_push_json_element_into_flat_array() {
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { push, length } from "std/array"
val appendBytes = (buf: UInt8[], src: AnyVal, i: Int32): Null =>
  if i < length(src) then
    push(buf, src[i])
    appendBytes(buf, src, i + 1)
val src: AnyVal = [10, 20, 30]
val buf: UInt8[] = []
appendBytes(buf, src, 0)
print(toString(buf))
"#);
    assert_eq!(out, vec!["[10, 20, 30]"]);
}

// TYPE-SOUNDNESS (record field omission via a generic call). Lin records are STRUCTURALLY typed:
// a value with MORE fields than the type (extras) is assignable (width subtyping), but a value
// OMITTING a required field is NOT. The previously-open hole: omission slipped through the generic
// call path — `push(toks, {kind})` where `toks: Token[]` and the `{kind}` item omits the required
// `text`. The shared `T` was bound `T = Token` by the container arg, then silently CLOBBERED to the
// deficient `{kind}` by the item arg (the no-clobber guard's last-wins-on-conflict branch), so the
// arg-compat gate compared `{kind}` vs `{kind}` and trivially passed. Reading the omitted `text`
// (a NULL pointer in the boxed path) then SEGFAULTED. Now the canonical first binding `T = Token`
// is kept and the omitting item is rejected with a clear diagnostic naming the expected full type.
#[test]
fn test_generic_push_record_field_omission_rejected() {
    let err = run_expect_err(
        r#"import { push } from "std/array"
type Token = { "kind": String, "text": String }
val toks: Token[] = []
push(toks, { "kind": "lparen" })
"#,
    );
    // With named-type display (fix/lsp-named-type-display), the expected type shows as the
    // alias name "Token" rather than the structural form. The error still correctly identifies
    // the mismatch; just check that "Token" (the named type) is mentioned as the expected type.
    assert!(
        err.contains("Token"),
        "push of a record OMITTING the required `text` field must be a type error naming the \
         expected type Token, got: {err}"
    );
}

// The asymmetric counterpart of the omission rejection: a record with EXTRA fields (width
// subtyping) MUST still flow through the generic call, and a COMPLETE record obviously must too.
// This is the whole point of the fix — close omission WITHOUT breaking width-subtyping.
#[test]
fn test_generic_push_record_extras_and_complete_accepted() {
    let out = run(r#"import { print } from "std/io"
import { push } from "std/array"
type Token = { "kind": String, "text": String }
val toks: Token[] = []
// COMPLETE record: every required field present.
push(toks, { "kind": "lparen", "text": "(" })
// EXTRAS (width subtyping): more fields than the type requires.
push(toks, { "kind": "rparen", "text": ")", "line": 1 })
print(toks[0]["text"])
print(toks[1]["text"])
"#);
    assert_eq!(out, vec!["(", ")"]);
}

// Width subtyping through a normal (non-generic) function parameter: an `OldPerson` value (= Person
// + an extra `pension` field) is assignable where a `Person` is expected. Extras must NOT be
// rejected by the omission fix.
#[test]
fn test_record_extras_into_fn_param_accepted() {
    let out = run(r#"import { print } from "std/io"
type Person = { "name": String, "age": Int32 }
type OldPerson = { "name": String, "age": Int32, "pension": Int32 }
val sayHello = (p: Person): String =>
  "Hello ${p["name"]}"
val o: OldPerson = { "name": "Bob", "age": 70, "pension": 100 }
print(sayHello(o))
"#);
    assert_eq!(out, vec!["Hello Bob"]);
}

// REGRESSION GUARD for the omission fix: the legitimate last-wins-clobber case must still work.
// `push(uint8Buf, int32Val)` binds `T = UInt8` from the container, then the wider-numeric `Int32`
// item must clobber `T` to `Int32` (the runtime coerces it down to a byte). The narrow
// record-omission guard must NOT fire here (numeric, not a deficient-record conflict).
#[test]
fn test_generic_push_int32_into_uint8_array_still_coerces() {
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { push } from "std/array"
type Field = { "tag": Int32, "bytes": Int32[] }
val encodeField = (buf: UInt8[], field: Field): Null =>
  push(buf, field["tag"])
val buf: UInt8[] = []
val f: Field = { "tag": 5, "bytes": [1, 2] }
encodeField(buf, f)
print(toString(buf))
"#);
    assert_eq!(out, vec!["[5]"]);
}

// A generic `push` of a generic-`U`-typed element built inside ANOTHER generic function, applied
// cross-module, monomorphizes the nested push at the OUTER instantiation's concrete element type
// (`mymap<Int32,Int32>` → flat `push$Int32`), via the import-of-import thin-intrinsic-wrapper
// inlining of `push`→`lin_push`. Previously this re-homed to the boxed `std_array_push` ($AnyVal),
// which 16-byte tagged-wrote into a 4-byte flat slot → heap-buffer-overflow.
#[test]
fn test_generic_push_nested_in_cross_module_generic() {
    let dir = std::env::temp_dir().join(format!("lin_genpush_nested_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("helpers.lin"),
        "import { push } from \"std/array\"\n\
         import { for } from \"std/iter\"\n\
         export val mymap = <T, U>(arr: T[], f: (T) => U): U[] =>\n  \
           val result: U[] = []\n  \
           arr.for(item => push(result, f(item)))\n  \
           result\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ length }} from "std/array"
import {{ reduce }} from "std/iter"
import {{ mymap }} from "{}/helpers"
val ints = mymap([1, 2, 3], x => x * 10)
val strs = mymap(["a", "b"], s => s)
print(toString(ints.reduce(0, (acc, x) => acc + x)))
print(toString(length(strs)))
"#, dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["60", "2"]);
}

#[test]
fn test_group_by_even_odd_and_empty() {
    // groupBy now returns a typed index-signature map `{ String: T[] }` (ADR-055): ONE hash lookup
    // per item (lin_map_get_or_insert_array, tag-aware over LinMap) + push. Grouping by even/odd
    // splits correctly. The map itself stringifies as `{}` (TAG_MAP now has structural toString);
    // the per-key array values print normally.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { groupBy } from "std/array"

val g = groupBy([1, 2, 3, 4, 5], x => if x % 2 == 0 then "even" else "odd")
print(toString(g["even"]))   // [2, 4]
print(toString(g["odd"]))    // [1, 3, 5]

val empty: Int32[] = []
val ge = groupBy(empty, x => "k")
print(toString(ge))          // [object] (empty map)

// Single bucket: every item lands under one key.
val one = groupBy([7, 9, 11], x => "all")
print(toString(one["all"]))  // [7, 9, 11]
"#);
    assert_eq!(out, vec!["[2, 4]", "[1, 3, 5]", "{}", "[7, 9, 11]"]);
}

#[test]
fn test_u32_be_round_trip() {
    // std/bytes: a UInt32 survives a big-endian write then read.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"
import { u32ToBe, u32FromBe } from "std/bytes"

val v: UInt32 = 0xDEADBEEF
val b: UInt8[] = u32ToBe(v)
print(toString(length(b)))          // 4
print(toString(b[0]))               // 222 (0xDE)
print(toString(u32FromBe(b, 0) == v))   // true
"#);
    assert_eq!(out, vec!["4", "222", "true"]);
}

#[test]
fn test_unsigned_int_display() {
    // Boxed unsigned integers must display as unsigned, even when their value would be a
    // negative bit pattern if read signed (u32 >= 2^31, u64 >= 2^63). Regression for the
    // "prints -1 instead of 4294967295" bug.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val a: UInt32 = 4294967295
val b: UInt32 = 2864434397
val c: UInt8 = 255
val d: UInt16 = 65535
val e: UInt64 = 18446744073709551615

print(toString(a))   // 4294967295
print(toString(b))   // 2864434397
print(toString(c))   // 255
print(toString(d))   // 65535
print(toString(e))   // 18446744073709551615
"#);
    assert_eq!(out, vec![
        "4294967295",
        "2864434397",
        "255",
        "65535",
        "18446744073709551615",
    ]);
}

#[test]
fn test_signed_widening_sign_extends() {
    // Widening a signed integer to a wider type must SIGN-extend: `0 - 1` is an Int32 -1
    // (0xFFFFFFFF); storing it into an Int64 slot must give -1, not 4294967295. Regression
    // for a Coerce path that zero-extended unconditionally. Unsigned widening must still
    // zero-extend (a UInt8 200 → UInt32 stays 200), so both directions are checked.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val a: Int64 = 0 - 1
val b: Int64 = 5 - 10
val c: Int32 = 0 - 1
val big: Int64 = 3000000000

val u8v: UInt8 = 200
val uwide: UInt32 = u8v
val u16v: UInt16 = 65000
val uwide2: UInt64 = u16v

print(toString(a))       // -1
print(toString(b))       // -5
print(toString(c))       // -1
print(toString(big))     // 3000000000 (positive widening unaffected)
print(toString(uwide))   // 200 (unsigned still zero-extends)
print(toString(uwide2))  // 65000
"#);
    assert_eq!(out, vec!["-1", "-5", "-1", "3000000000", "200", "65000"]);
}

#[test]
fn test_unsigned_int_cross_compare() {
    // A boxed UInt32 (now stored as TAG_INT64) still compares correctly against a boxed Int32,
    // both for equality and ordering of large values.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val x: UInt32 = 5
val y: Int32 = 5
print(toString(x == y))   // true

val big: UInt32 = 4000000000
val one: Int32 = 1
print(toString(big > one))   // true
"#);
    assert_eq!(out, vec!["true", "true"]);
}

#[test]
fn test_unsigned_int_arithmetic_roundtrip() {
    // Boxing then using a UInt32 in arithmetic preserves the high-bit value.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val a: UInt32 = 4294967290
val b: UInt32 = a + 3
print(toString(b))   // 4294967293
"#);
    assert_eq!(out, vec!["4294967293"]);
}

#[test]
fn test_float_is_type_matches_runtime_tag() {
    // A boxed float in a union must satisfy `is Float64` / `is Float32`. Codegen's tag table
    // (type_tag_const, used by the `is` check) once mapped Float64 to TAG_FLOAT32 (4) while
    // box_value tagged it TAG_FLOAT64 (5), so `x is Float64` compared 5 against a value tagged
    // 4 → always-false dead arm. Both float widths box as TAG_FLOAT64, so both must match.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { toFloat32 } from "std/number"

val mk64 = (b: Boolean): Float64 | String =>
  match (b)
    is true => 3.5
    else => "hi"

print(toString(mk64(true) is Float64))    // true
print(toString(mk64(false) is Float64))   // false

val mk32 = (b: Boolean): Float32 | String =>
  match (b)
    is true => toFloat32(2.5)
    else => "hi"

print(toString(mk32(true) is Float32))     // true
print(toString(mk32(false) is Float32))    // false
"#);
    assert_eq!(out, vec!["true", "false", "true", "false"]);
}

#[test]
fn test_float32_object_field_roundtrips() {
    // A statically-Float32 field stored into an object TaggedVal then read DYNAMICALLY (the
    // object is AnyVal-typed, so the read routes through the runtime's tag-driven
    // lin_tagged_to_string). Codegen tagged the slot TAG_FLOAT32 (4) but wrote an f64-bits
    // payload (the value is fpext'd to f64 before storing); the runtime reads a TAG_FLOAT32
    // payload as `f32::from_bits(payload as u32)` → the low 32 bits of 1.5f64's pattern are 0
    // → it printed 0.0 / JSON "f": 0. Now stored as TAG_FLOAT64 with f64 bits → reads back 1.5.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val obj: AnyVal = { "f": 1.5f32, "n": 7 }
print(toString(obj["f"]))   // 1.5  (was 0.0)
print(toString(obj["n"]))   // 7
print(toString(obj))        // {"f": 1.5, "n": 7}  (was "f": 0)
"#);
    assert_eq!(out, vec!["1.5", "7", "{\"f\": 1.5, \"n\": 7}"]);
}

#[test]
fn test_uint64_high_bit_compare_and_stringify() {
    // A UInt64 with the high bit set (>= 2^63) must compare UNSIGNED and stringify UNSIGNED.
    // The compare predicate selection forced signed predicates for any 64-bit operand, so a
    // UInt64 >= 2^63 compared as negative; direct stringification routed UInt64 through the
    // signed lin_int_to_string. Both now treat UInt64 as unsigned.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val big: UInt64 = 18446744073709551615
val mid: UInt64 = 9223372036854775808
val one: UInt64 = 1

print(toString(big > one))     // true  (was false: big read as -1)
print(toString(mid > one))     // true  (was false: mid read as i64::MIN)
print(toString(one < mid))     // true
print(toString(mid >= mid))    // true
print(toString(mid))           // 9223372036854775808 (was -9223372036854775808)
print(toString(big))           // 18446744073709551615 (was -1)
"#);
    assert_eq!(out, vec![
        "true", "true", "true", "true",
        "9223372036854775808",
        "18446744073709551615",
    ]);
}

#[test]
fn test_computed_high_u32_display() {
    // A UInt32 computed at runtime (not a literal) from all-0xFF bytes prints 4294967295,
    // exercising the display path rather than only bit-equality.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { u32FromBe } from "std/bytes"

val bytes: UInt8[] = [255, 255, 255, 255]
print(toString(u32FromBe(bytes, 0)))   // 4294967295
"#);
    assert_eq!(out, vec!["4294967295"]);
}

// ===========================================================================
// std/net — UDP and TCP sockets (Milestone 21, Layer 2)
//
// These exercise REAL loopback sockets. They are consolidated into single test
// functions (one for UDP, one for TCP) so that all socket work for a given
// protocol runs single-threaded with deterministic ordering, and so that fixed
// high ports don't collide across parallel test threads.
// ===========================================================================

#[test]
fn test_net_udp_loopback_roundtrip() {
    // Bind one UDP socket and send a datagram to itself, then recvFrom it.
    // udpBind binds a fixed port (the API doesn't surface an OS-assigned port),
    // so we use a high port and send to 127.0.0.1:<port>.
    let out = run(r#"import { udpBind, udpSendTo, udpRecv, udpRecvFrom, udpSetNonblocking, udpClose, Datagram } from "std/net"
import { print } from "std/io"
import { toString } from "std/string"

val port = 39201
val bound = udpBind(port)
print("bound: ${toString(!(bound is Error))}")
if bound is Error then
  print("(bind failed)")
else
  val sock = bound

  // Non-blocking recv with no data pending must return Null.
  val nb = udpSetNonblocking(sock, true)
  val empty: UInt8[] = [0, 0, 0, 0]
  val none = udpRecv(sock, empty)
  print("empty-recv-null: ${toString(none == null)}")

  // Back to blocking for the round-trip.
  val nb2 = udpSetNonblocking(sock, false)
  val msg: UInt8[] = [72, 105, 33, 10]
  val sent = udpSendTo(sock, "127.0.0.1", port, msg)
  print("sent: ${toString(sent)}")

  val buf: UInt8[] = [0, 0, 0, 0, 0, 0, 0, 0]
  val res = udpRecvFrom(sock, buf)
  if res is Datagram then
    print("len: ${toString(res["len"])}")
    print("addr: ${toString(res["addr"])}")
    print("b0: ${toString(buf[0])}")
    print("b1: ${toString(buf[1])}")
    print("b2: ${toString(buf[2])}")
    print("b3: ${toString(buf[3])}")
  else
    print("(recv failed)")

  val c = udpClose(sock)
"#);
    assert_eq!(
        out,
        vec![
            "bound: true",
            "empty-recv-null: true",
            "sent: 4",
            "len: 4",
            "addr: 127.0.0.1",
            "b0: 72",
            "b1: 105",
            "b2: 33",
            "b3: 10",
        ]
    );
}

#[test]
fn test_net_tcp_loopback_echo() {
    // Single-threaded TCP ordering: listen, connect (blocking — the kernel
    // completes the handshake into the listener backlog), then a blocking accept
    // immediately returns the pending connection. The server then reads the
    // client's bytes. After the client closes, the server's recv returns 0.
    let out = run(r#"import { tcpListen, tcpAccept, tcpConnect, tcpRecv, tcpSend, tcpClose, TcpPeer } from "std/net"
import { print } from "std/io"
import { toString } from "std/string"

val port = 39202
val lis = tcpListen(port)
print("listening: ${toString(!(lis is Error))}")
val cli = tcpConnect("127.0.0.1", port)
print("connected: ${toString(!(cli is Error))}")
if lis is Error then
  print("(listen failed)")
else if cli is Error then
  print("(connect failed)")
else
  val listener = lis
  val client = cli
  val accepted = tcpAccept(listener)
  print("accepted: ${toString(accepted is TcpPeer)}")
  if accepted is TcpPeer then
    val server = accepted["fd"]
    val payload: UInt8[] = [76, 105, 110, 33]
    val sent = tcpSend(client, payload)
    print("sent: ${toString(sent)}")

    val buf: UInt8[] = [0, 0, 0, 0, 0, 0]
    val n = tcpRecv(server, buf)
    print("recv: ${toString(n)}")
    print("b0: ${toString(buf[0])}")
    print("b1: ${toString(buf[1])}")
    print("b2: ${toString(buf[2])}")
    print("b3: ${toString(buf[3])}")

    // Close the client; the server's next recv must return 0 (peer closed).
    val cc = tcpClose(client)
    val buf2: UInt8[] = [0, 0, 0, 0]
    val n2 = tcpRecv(server, buf2)
    print("recv-after-close: ${toString(n2)}")

    val sc = tcpClose(server)
    val lc = tcpClose(listener)
  else
    print("(accept failed)")
"#);
    assert_eq!(
        out,
        vec![
            "listening: true",
            "connected: true",
            "accepted: true",
            "sent: 4",
            "recv: 4",
            "b0: 76",
            "b1: 105",
            "b2: 110",
            "b3: 33",
            "recv-after-close: 0",
        ]
    );
}

// ===========================================================================
// std/process — subprocesses, and std/tty — raw terminal (Milestone 21, Layer 3)
//
// std/process is deterministic: we spawn a real `sh -c` process (streaming) and
// run small `printf`/`sh` commands to completion (batch). std/tty cannot be
// exercised under the test harness (stdin is a pipe, not a TTY); we only assert
// that rawMode on a non-TTY returns an Error object gracefully (no panic / crash).
// ===========================================================================

#[test]
fn test_process_spawn_read_wait() {
    // Spawn `sh -c 'printf hello'`, read its stdout into a buffer, assert the
    // bytes, then wait for exit code 0. `sh -c` is the most portable spawn.
    let out = run(r#"import { spawn, readStdout, wait } from "std/process"
import { print } from "std/io"
import { toString } from "std/string"

val h = spawn("sh", ["-c", "printf hello"])
print("spawned: ${toString(!(h is Error))}")
if h is Error then
  print("(spawn failed)")
else
  val buf: UInt8[] = [0, 0, 0, 0, 0, 0, 0, 0]
  val n = readStdout(h, buf)
  print("n: ${toString(n)}")
  print("b0: ${toString(buf[0])}")
  print("b1: ${toString(buf[1])}")
  print("b2: ${toString(buf[2])}")
  print("b3: ${toString(buf[3])}")
  print("b4: ${toString(buf[4])}")

  val code = wait(h)
  print("code: ${toString(code)}")
"#);
    assert_eq!(
        out,
        vec![
            "spawned: true",
            "n: 5",
            "b0: 104", // 'h'
            "b1: 101", // 'e'
            "b2: 108", // 'l'
            "b3: 108", // 'l'
            "b4: 111", // 'o'
            "code: 0",
        ]
    );
}

// Stage 5 (streams): the unified process-stdout source. `spawn` a child, wrap its piped stdout
// as a Stream<UInt8[]>, and `readText` the whole output through the stream layer.
#[test]
fn test_stream_process_stdout_source() {
    let out = run(r#"import { spawn } from "std/process"
import { stdoutStream } from "std/process"
import { readText } from "std/stream"
import { print } from "std/io"

val h = spawn("sh", ["-c", "printf 'line1\nline2\n'"])
if h is Error then
  print("(spawn failed)")
else
  val text = stdoutStream(h).readText()
  if text is Error then
    print("(read failed)")
  else
    print(text)
"#);
    assert_eq!(out, vec!["line1", "line2"]);
}

// Stage 5 (streams): the unified stdin source. Feed lines on stdin, read them back through a
// stdinStream → lines → for pipeline.
#[test]
fn test_stream_stdin_source() {
    let output = run_with_stdin(r#"import { stdinStream } from "std/io"
import { lines } from "std/stream"
import { for } from "std/iter"
import { print } from "std/io"

stdinStream().lines().for(line => print("got: ${line}"))
"#, "aaa\nbbb\nccc\n");
    let parts: Vec<&str> = output.lines().collect();
    assert_eq!(parts, vec!["got: aaa", "got: bbb", "got: ccc"]);
}

// Stage 5 (streams): the unified TCP source. Loopback connect, send bytes, close the client, and
// read the server side through a tcpStream → readText. The client close makes the server stream
// reach EOF, so readText returns the full payload.
#[test]
fn test_stream_tcp_source() {
    let out = run(r#"import { tcpListen, tcpAccept, tcpConnect, tcpSend, tcpClose, tcpStream, TcpPeer } from "std/net"
import { readText } from "std/stream"
import { print } from "std/io"

val port = 39271
val lis = tcpListen(port)
val cli = tcpConnect("127.0.0.1", port)
if lis is Error then
  print("(listen failed)")
else if cli is Error then
  print("(connect failed)")
else
  val listener = lis
  val client = cli
  val accepted = tcpAccept(listener)
  if accepted is TcpPeer then
    val server = accepted["fd"]
    val payload: UInt8[] = [72, 105, 33]
    tcpSend(client, payload)
    tcpClose(client)
    val text = tcpStream(server).readText()
    print("got: ${text}")
    tcpClose(listener)
  else
    print("(accept failed)")
"#);
    assert_eq!(out, vec!["got: Hi!"]);
}

#[test]
fn test_process_wait_exit_code() {
    // `sh -c 'exit 3'` exits with code 3.
    let out = run(r#"import { spawn, wait } from "std/process"
import { print } from "std/io"
import { toString } from "std/string"

val h = spawn("sh", ["-c", "exit 3"])
if h is Error then
  print("(spawn failed)")
else
  val code = wait(h)
  print("code: ${toString(code)}")
"#);
    assert_eq!(out, vec!["code: 3"]);
}

#[test]
fn test_process_exec_and_shell_batch() {
    // Batch API: exec collects status + full stdout into an ExecResult; shell runs
    // through /bin/sh; a non-zero exit is reported in `status`; cwd is non-empty.
    let out = run(r#"import { exec, shell, cwd } from "std/process"
import { contains } from "std/string"
import { print } from "std/io"
import { toString } from "std/string"

val r = exec("printf", ["Hello"])
print("exec status: ${toString(r["status"])}")
print("exec stdout: ${r["stdout"]}")

val r2 = shell("printf one; printf two")
print("shell stdout: ${r2["stdout"]}")

val r3 = exec("sh", ["-c", "exit 7"])
print("fail status: ${toString(r3["status"])}")

print("cwd ok: ${toString(contains(cwd(), "/"))}")
"#);
    assert_eq!(
        out,
        vec![
            "exec status: 0",
            "exec stdout: Hello",
            "shell stdout: onetwo",
            "fail status: 7",
            "cwd ok: true",
        ]
    );
}

#[test]
fn test_tty_rawmode_on_non_tty_returns_error() {
    // Under the test harness stdin is not a TTY, so tcgetattr fails and rawMode
    // must return an Error object (type == "error") rather than panicking. We
    // assert "error" (not crash) without depending on the exact message.
    let out = run(r#"import { rawMode } from "std/tty"
import { print } from "std/io"
import { toString } from "std/string"

val r = rawMode(true)
print("type: ${toString(r["type"])}")
"#);
    assert_eq!(out, vec!["type: error"]);
}

#[test]
fn test_time_sleep_micros() {
    // sleepMicros(500) should sleep ~0.5ms and then return; the program must run
    // to completion and print after the sleep. (waitSignal is not tested here as it
    // would block; see the lin-runtime signal.rs sigwait/raise unit test.)
    let out = run(r#"import { sleepMicros } from "std/time"
import { print } from "std/io"

sleepMicros(500)
print("done")
"#);
    assert_eq!(out, vec!["done"]);
}

#[test]
fn test_time_format_parse_from_iso() {
    // format (strftime, UTC), fromIso (ISO 8601 -> ms), parse (pattern -> ms), and graceful
    // Error on bad input. Expected timestamps bound as Int64 vals (a bare >Int32 literal in a
    // comparison would default to Int32 and truncate).
    let out = run(r#"import { format, fromIso, parse } from "std/time"
import { print } from "std/io"
import { toString } from "std/string"

print(format(1705314600000, "%Y-%m-%dT%H:%M:%S"))
print(format(1705314600000, "%a %B %d"))
print(toString(fromIso("2024-01-15T10:30:00Z")))
print(toString(fromIso("2024-01-15")))
print(toString(parse("15/01/2024 10:30", "%d/%m/%Y %H:%M")))
val a = fromIso("not a date")
print(a["type"])
val b = parse("bad", "%Y-%m-%d")
print(b["type"])
"#);
    assert_eq!(
        out,
        vec![
            "2024-01-15T10:30:00",
            "Mon January 15",
            "1705314600000",
            "1705276800000",
            "1705314600000",
            "error",
            "error",
        ]
    );
}

#[test]
fn test_concrete_string_into_json_var_loop() {
    // Regression: reassigning a fresh CONCRETE value (toString -> String) into a AnyVal/union
    // `var` inside a loop boxes the value via Coerce, producing a transient TaggedVal* shell.
    // The LocalSet store path used to clone that box for the global/cell AND for the result
    // but never freed the transient shell, leaking ~36 bytes per iteration. The fix frees the
    // shell (FreeBoxShell) after both clones. This asserts correctness: the var must hold the
    // last assigned value and the program must not crash (no use-after-free / double-free).
    let out = run(r#"import { range, for } from "std/iter"
import { toString } from "std/string"
import { print } from "std/io"

var last: AnyVal = ""
range(0, 5).for(i => last = toString(i))
print(toString(last))
"#);
    assert_eq!(out, vec!["4"]);
}

#[test]
fn test_concrete_object_into_json_var_loop() {
    // Regression companion to the String case: a fresh concrete Object boxed into a AnyVal var
    // each iteration. Exercises the same transient-coercion-box free path with an Object payload
    // and confirms the final stored value is correct.
    let out = run(r#"import { range, for } from "std/iter"
import { toString } from "std/string"
import { print } from "std/io"

var last: AnyVal = null
range(0, 5).for(i => last = { "n": i })
print(toString(last))
"#);
    assert_eq!(out, vec![r#"{"n": 4}"#]);
}

#[test]
fn test_flat_array_arg_used_twice_no_double_free() {
    // Regression: a flat scalar array (Float64[]) passed in two argument positions, or two
    // separate flat-array literals, must not be released more times than it was retained.
    // The callee `dot` reads each heap parameter twice (`a[0]`, `a[1]`); each read lowered to
    // a Retain + a scope-exit Release. The RC-elision pass paired BOTH Retains to the SAME
    // first Release (a HashSet deduped the second elision), eliding two Retains but only one
    // Release — leaving one extra Release and a heap-use-after-free in lin_array_release. The
    // functional guard here (prints 25.0 instead of crashing) catches it deterministically;
    // the ASan CI leg surfaces the underlying UAF.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val dot = (a: Float64[], b: Float64[]): Float64 => a[0] * b[0] + a[1] * b[1]
val v: Float64[] = [3.0, 4.0]
print(toString(dot(v, v)))
"#);
    assert_eq!(out, vec!["25.0"]);

    // Two separate flat-array literals exercise the same balance (each callee param read twice,
    // distinct caller-owned allocations).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val dot = (a: Float64[], b: Float64[]): Float64 => a[0] * b[0] + a[1] * b[1]
print(toString(dot([3.0, 4.0], [3.0, 4.0])))
"#);
    assert_eq!(out, vec!["25.0"]);

    // A single flat-array argument whose parameter is read more than once is the minimal form
    // of the same bug (one alloc, callee consumes one extra reference).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val sum2 = (a: Float64[]): Float64 => a[0] + a[1]
val v: Float64[] = [3.0, 4.0]
print(toString(sum2(v)))
"#);
    assert_eq!(out, vec!["7.0"]);
}

#[test]
fn test_match_binding_pattern_matches_and_unboxes() {
    // Two bugs in `is <binding>` match arms:
    // (1) the binding was bound to the BOXED scrutinee pointer, so a concrete binding
    //     (`is n` where n: Int32) used in a guard reinterpreted the pointer as the scalar
    //     (`ptrtoint`) — `when n > 5` compared a heap address (always true).
    // (2) the binding pattern was lowered as a type-CHECK (IsType against the binding's
    //     declared type), so `match req["path"] is p when ...` never matched a concrete
    //     value inside a AnyVal scrutinee. A binding is a named catch-all: it always matches.
    let out = run(r#"import { print } from "std/io"
val f = (x: Int32): String =>
  match x
    is n when n > 5 => "big"
    is m when m > 0 => "pos"
    else => "other"
print(f(10))
print(f(3))
print(f(0 - 1))
"#);
    assert_eq!(out, vec!["big", "pos", "other"]);

    // A binding over a AnyVal scrutinee mixed with a literal arm: the binding must match
    // unconditionally (it was lowered as a type-check that failed for a concrete value
    // inside a AnyVal scrutinee, so the literal-or-else path was taken instead).
    // `examples/web-server/router.test.lin` exercises the full guarded router shape.
    let out = run(r#"import { print } from "std/io"
val classify = (req: AnyVal): String =>
  match req["kind"]
    is "a" => "is-a"
    is other => "bound-other"
print(classify({ "kind": "a" }))
print(classify({ "kind": "z" }))
"#);
    assert_eq!(out, vec!["is-a", "bound-other"]);
}

#[test]
fn test_discarded_map_result_in_loop_correct() {
    // Regression for the AnyVal call-result leak: a `map` call returns a `AnyVal` (boxed `TaggedVal*`)
    // that is bound to a per-iteration `val m` and DISCARDED. `register_owned`'s old `is_rc_type`
    // gate excluded unions, so the owned box (and its inner array) was never released — a per-
    // iteration leak. The fix registers union import-fn call results so scope exit tag-releases
    // them. Correctness gate: over 20000 iterations, summing the lengths must stay exact and the
    // process must not abort (a wrong release would double-free the map result). 20000 * 3 = 60000.
    let out = run(r#"import { length } from "std/array"
import { range, for, map } from "std/iter"
import { print } from "std/io"
import { toString } from "std/string"

var c = 0
range(0, 20000).for(i =>
  val m = [1, 2, 3].map(x => x + i)
  c = c + length(m)
)
print(toString(c))
"#);
    assert_eq!(out, vec!["60000"]);
}

#[test]
fn test_discarded_filter_result_in_loop_correct() {
    // Companion to the map case for `filter` (also returns a fresh `AnyVal` array). Each iteration
    // discards the filtered array; the per-iteration release must reclaim it without corrupting
    // the source literal or the count. 20000 iterations; each filter keeps the 2 elements > 0
    // (1 and 2 are always > i is false for i>=1, so use a fixed predicate): [1,2,3,4] filtered by
    // x > 2 yields [3,4] every time → 20000 * 2 = 40000.
    let out = run(r#"import { length } from "std/array"
import { range, for, filter } from "std/iter"
import { print } from "std/io"
import { toString } from "std/string"

var c = 0
range(0, 20000).for(i =>
  val m = [1, 2, 3, 4].filter(x => x > 2)
  c = c + length(m)
)
print(toString(c))
"#);
    assert_eq!(out, vec!["40000"]);
}

#[test]
fn test_map_result_bound_and_returned_from_function() {
    // A function binds a `map` result to a `val` and RETURNS it: the returned union box must be
    // KEPT (transferred to the caller at +1), not released by the callee's scope-exit teardown
    // (which would hand back freed memory). Also exercises the concrete-rc return path: `val r =
    // [..]; r` must return the array at exactly +1 (the read-retain of the trailing expression is
    // released as a redundant extra registration, fixing the return-retain leak). Calling it many
    // times and summing lengths must stay exact.
    let out = run(r#"import { length } from "std/array"
import { range, for, map } from "std/iter"
import { print } from "std/io"
import { toString } from "std/string"

val doubled = (xs: AnyVal): AnyVal =>
  val m = xs.map(x => x * 2)
  m
var c = 0
range(0, 10000).for(i =>
  c = c + length(doubled([1, 2, 3, 4]))
)
print(toString(c))
print(toString(doubled([5, 6, 7])))
"#);
    assert_eq!(out, vec!["40000", "[10, 12, 14]"]);
}

#[test]
fn test_union_projection_returned_no_double_free() {
    // Regression: a AnyVal/union projection (`obj[k]` / `obj.field`) RETURNED from a function
    // double-freed. `lin_object_get` hands back a BORROWED INTERIOR `*TaggedVal` pointing into
    // the container's entry array — NOT an ownable heap box. The lowerer deliberately does not
    // own a union projection (correct for transient in-place use), but the uniform call
    // convention has the caller treat a function result as OWNED (+1) and release it. When such
    // a projection ESCAPES as the return value, the container release frees the interior value
    // AND the caller's release frees it again → `free(): invalid pointer`. The fix clones a
    // borrowed union projection (`CloneBox` → `lin_tagged_clone`) at the function return
    // boundary so the result is a genuine owned +1 box. Each case below crashed with exit 1
    // before the fix; the `run` harness asserts a successful exit, so a relapse fails the test.

    // Projection returned directly from a named function (the minimal `pluck` repro).
    let out = run(r#"import { print } from "std/io"
val pluck = (x: AnyVal): AnyVal => x["name"]
print(pluck({ "name": "Alice" }))
"#);
    assert_eq!(out, vec!["Alice"]);

    // Projection returned from a map CALLBACK closure, result stored into an array then iterated:
    // each element must be an owned box the array releases exactly once.
    let out = run(r#"import { print } from "std/io"
import { for, map } from "std/iter"
val records = [{ "name": "Alice" }, { "name": "Bob" }]
records.map(r => r["name"]).for(n => print(n))
"#);
    assert_eq!(out, vec!["Alice", "Bob"]);

    // Nested projection (`r["value"]["name"]`) through a map callback: the inner projection is a
    // transient read, the outer escapes — only the escaping result is cloned.
    let out = run(r#"import { print } from "std/io"
import { map, for } from "std/iter"
val records = [{ "value": { "name": "Alice" } }, { "value": { "name": "Bob" } }]
val names = records.map(r => r["value"]["name"])
names.for(n => print(n))
"#);
    assert_eq!(out, vec!["Alice", "Bob"]);

    // Projection bound to a `val` and THEN returned (a different escape route into the return
    // boundary than a bare projection expression): the bound borrowed projection must still be
    // cloned to an owned box before it leaves the scope.
    let out = run(r#"import { print } from "std/io"
val pluck = (x: AnyVal): AnyVal =>
  val n = x["name"]
  n
print(pluck({ "name": "Carol" }))
"#);
    assert_eq!(out, vec!["Carol"]);

    // Calling the projection-returning function many times in a loop must stay balanced (the
    // per-call clone is released each iteration; a relapse to the borrowed-return double-free,
    // or a per-iteration over-clone leak, would surface here / under the ASan CI leg).
    let out = run(r#"import { print } from "std/io"
import { range, for } from "std/iter"
import { toString } from "std/string"
val pluck = (x: AnyVal): AnyVal => x["v"]
var c = 0
range(0, 2000).for(i =>
  c = c + 1
  print(toString(pluck({ "v": "x" })))
)
print(toString(c))
"#);
    assert_eq!(out.last().map(|s| s.as_str()), Some("2000"));
}

// Regression: the error-propagation idiom `val r = <owned AnyVal call result>; if cond then r
// else <fresh value>` returned from a function. When one branch yields the owned union local
// `r` and the merge is unified to a CONCRETE representation, the then-branch used to UNBOX `r`
// (`lin_unbox_ptr`) into an INTERIOR pointer aliasing `r`'s box payload WITHOUT a reference.
// At the merge, the scope-release of `r` (`lin_tagged_release`) then freed that payload while
// the merged result still aliased it — re-boxing the freed inner produced a box around freed
// memory (a use-after-free; later reads crashed with a misaligned/null deref). The fix has the
// escaping branch take an INDEPENDENT reference (clone-then-unbox, or clone the box when the
// merge stays boxed) so the result owns its payload, and propagates that +1 up through the
// block scope so the function-return path does not re-clone (which would leak per call).
#[test]
fn test_if_branch_returns_owned_json_local_no_uaf() {
    // Minimal: then-branch returns the owned local `r`, else-branch is a fresh object.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val deep = (): AnyVal => { "type": "failure" }
val top = (b: Boolean): AnyVal =>
  val r = deep()
  if b then r else { "type": "ok" }
print(toString(top(true)))
print(toString(top(false)))
"#);
    assert_eq!(out, vec![r#"{"type": "failure"}"#, r#"{"type": "ok"}"#]);

    // The actual `if isFailure(r) then r else { ... }` idiom: the condition reads `r`, the
    // failure path returns `r` unchanged, the success path projects from `r`.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val deep = (): AnyVal => { "type": "failure", "error": "eof" }
val top = (): AnyVal =>
  val r = deep()
  if r["type"] == "failure" then r
  else { "type": "success", "value": r["node"] }
print(toString(top()))
"#);
    // Phase 2: open objects use LinMap (hash-ordered keys → alphabetical in toString).
    assert_eq!(out, vec![r#"{"type": "failure", "error": "eof"}"#]);

    // Both branches are union (`r` and another call result `mk()`): the merge stays boxed and
    // must clone the borrowed `r` so the scope-release of `r` does not dangle the result.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val mk = (): AnyVal => { "type": "failure", "k": "v" }
val pick = (i: Int32): AnyVal =>
  val r = mk()
  if i > 0 then r else mk()
print(toString(pick(5)))
print(toString(pick(0)))
"#);
    // Phase 2: open objects use LinMap (hash-ordered → sorted alphabetical in toString). k < t.
    assert_eq!(out, vec![r#"{"type": "failure", "k": "v"}"#, r#"{"type": "failure", "k": "v"}"#]);

    // Multi-level propagation: `mid` returns `r` (from `deep`) on failure, `top` returns `r`
    // (from `mid`) on failure — the owned union local is forwarded through two `if`-branches.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"
val isFailure = (x: AnyVal): Boolean => x["type"] == "failure"
val deep = (arr: AnyVal, pos: Int32): AnyVal =>
  if pos >= length(arr) then { "type": "failure", "error": "eof" }
  else { "node": arr[pos], "pos": pos + 1 }
val mid = (arr: AnyVal, pos: Int32): AnyVal =>
  val r = deep(arr, pos)
  if isFailure(r) then r
  else { "node": r["node"], "pos": r["pos"] }
val top = (arr: AnyVal): AnyVal =>
  val r = mid(arr, 5)
  if isFailure(r) then r
  else { "type": "success", "value": r["node"] }
print(toString(top([1, 2])))
"#);
    // Phase 2: open objects use LinMap (hash-ordered keys → alphabetical in toString).
    assert_eq!(out, vec![r#"{"type": "failure", "error": "eof"}"#]);

    // Returned-in-a-loop with the result discarded: a per-call leak (the if-branch clone
    // re-cloned by the function return) would surface here under the ASan CI leg; functionally
    // it must just run to completion.
    let out = run(r#"import { print } from "std/io"
import { for, range } from "std/iter"
val mk = (): AnyVal => { "type": "failure", "k": "v" }
val pick = (i: Int32): AnyVal =>
  val r = mk()
  if i > 0 then r else mk()
val main = (): Null =>
  range(0, 2000).for(i =>
    val x = pick(i)
    null
  )
  print("done")
main()
"#);
    assert_eq!(out, vec!["done"]);
}

#[test]
fn test_tco_loop_union_param_thread_no_leak_or_uaf() {
    // The "scanRouteAt" shape (TCO Leak B regression): a `T | Null` union (a record) threaded
    // through a TAIL-RECURSIVE param fed by `arr[i]`. The loop's final `cur` box was never
    // released (it leaked ~112B/call), and a naive loop-exit release double-freed either the
    // borrowed pass-through `arr` param or a buffer permuted between slots (the merge-sort
    // ping-pong). This asserts the CORRECT result; the ASan CI leg / sealed-harness verify the
    // no-leak / no-double-free guarantees.
    let out = run(r#"import { print } from "std/io"
import { push } from "std/array"
type T = { "a": Int32, "b": Int32 }
val scan = (arr: AnyVal, j: Int32, n: Int32, cur: T | Null): Int32 =>
  if j >= n then
    match cur
      is T => cur["a"]
      else => -1
  else
    val nx: T = arr[j]
    scan(arr, j + 1, n, nx)
val once = (i: Int32): Int32 =>
  var arr: AnyVal = []
  push(arr, { "a": i, "b": 0 })
  scan(arr, 0, 1, null)
val loop = (i: Int32, n: Int32, acc: Int32): Int32 =>
  if i >= n then acc
  else loop(i + 1, n, acc + once(i))
print(loop(0, 100, 0))
"#);
    // sum(0..99) = 4950
    assert_eq!(out, vec!["4950"]);

    // A TCO loop that PERMUTES borrowed array params between slots (the merge-sort ping-pong
    // distilled): the loop-exit release must NOT free a buffer swapped in from another entry
    // slot. `sort` over a record array exercises exactly this internally.
    let out = run(r#"import { print } from "std/io"
import { push, sort, length } from "std/array"
type R = { "k": Int32 }
val once = (i: Int32): Int32 =>
  var rs: R[] = []
  push(rs, { "k": i })
  push(rs, { "k": 0 })
  val s: R[] = sort(rs, (x, y) => x["k"] - y["k"])
  s[length(s) - 1]["k"]
val loop = (i: Int32, n: Int32, acc: Int32): Int32 =>
  if i >= n then acc
  else loop(i + 1, n, acc + once(i))
print(loop(0, 100, 0))
"#);
    assert_eq!(out, vec!["4950"]);
}

#[test]
fn test_index_assign_evaluates_to_assigned_value() {
    // Spec §8 / §27 rule 8: an assignment expression evaluates to the assigned value. This held
    // for `var x = v` (`LocalSet`) but `m[k] = v` (`IndexSet`) wrongly evaluated to `Null`. Now an
    // index/field assignment can be the value of an `if` branch / block tail, so the memoizing
    // cache idiom `if m[k] == null then m[k] = compute() else m[k]` type-checks and returns the
    // stored value directly — no intermediate `val` needed.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { split } from "std/string"
import { parseInt32 } from "std/number"

val createTimeParser = () =>
  val timeCache: { String: Int32 } = {}
  (time: String): Int32 =>
    if timeCache[time] == null then
      val [hh, mm, ss] = time.split(":")
      timeCache[time] = parseInt32(hh) * 60 * 60 + parseInt32(mm) * 60 + parseInt32(ss)
    else
      timeCache[time]

val parse = createTimeParser()
print(toString(parse("01:02:03")))
print(toString(parse("01:02:03")))
print(toString(parse("00:01:00")))
"#);
    assert_eq!(out, vec!["3723", "3723", "60"]);
}

#[test]
fn test_index_assign_value_result_map_object_array() {
    // The assigned-value result across container kinds and value kinds, each consumed directly:
    //   - map scalar value (returned and printed),
    //   - object field STRING value (heap rc) returned from a block,
    //   - array index value used in an expression.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"

// map, scalar value: the assignment IS the returned value
val m: { String: Int32 } = {}
print(toString(m["a"] = 42))

// object field, heap (String) value
type Rec = { "name": String }
val r: Rec = { "name": "" }
val n: String = r["name"] = "lin"
print(n)

// array slot, scalar value used in arithmetic
var xs: Int32[] = [0, 0, 0]
print(toString((xs[1] = 7) + 1))
print(toString(length(xs)))
"#);
    assert_eq!(out, vec!["42", "lin", "8", "3"]);
}

#[test]
fn test_void_function_body_may_end_in_assignment() {
    // A `: Null` (void) function's body value is DISCARDED, so it may now END in an assignment
    // expression (value-typed per spec §8) without an artificial `; null` tail. This is the
    // `addPair`-style multimap accumulator from `std/encoding`'s query parser. The heap (`String[]`)
    // value the assignment produces must be RELEASED at scope exit (void functions `Return(None)`),
    // not leaked — the ASan CI leg and the constant-RSS check guard that; here we assert behaviour.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { push, length } from "std/array"

val addPair = (m: { String: String[] }, k: String, v: String): Null =>
  val cur = m[k]
  if cur == null then
    val fresh: String[] = [v]
    m[k] = fresh
  else
    push(cur, v)

var m: { String: String[] } = {}
addPair(m, "a", "x")
addPair(m, "a", "y")
addPair(m, "b", "z")
print(toString(length(m["a"])))
print(toString(length(m["b"])))
"#);
    assert_eq!(out, vec!["2", "1"]);
}

#[test]
fn object_index_assign_of_callback_param() {
    // Regression: `obj[key] = value` where `value` is a for/map callback PARAMETER used to
    // store NULL. Under the uniform closure ABI a callback param arrives BOXED (a TaggedVal*),
    // but `compile_ir_index_set` re-wrapped it via `build_tagged_val_alloca` using the param's
    // STATIC scalar type — that path saw a pointer where it expected an int, tagged the box as
    // NULL, and dropped the value (the boxed-value-dropped bug). The fix passes an
    // already-boxed AnyVal value straight to the object/array setter.

    // Int value via `for` callback param.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { for } from "std/iter"
[5].for(n =>
  var o: {} = {}
  o["x"] = n
  print(toString(o))
)
"#);
    assert_eq!(out, vec![r#"{"x": 5}"#]);

    // Int values accumulated via `map` callback, returning the built object.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map } from "std/iter"
val rs = [5, 6].map(n =>
  var o: {} = {}
  o["x"] = n
  o
)
print(toString(rs))
"#);
    assert_eq!(out, vec![r#"[{"x": 5}, {"x": 6}]"#]);

    // String value via callback param.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { for } from "std/iter"
["hi"].for(s =>
  var o: {} = {}
  o["msg"] = s
  print(toString(o))
)
"#);
    assert_eq!(out, vec![r#"{"msg": "hi"}"#]);

    // Captured-`var` object accumulated across a loop, with the callback param as the KEY
    // (a boxed string key must be unboxed to a raw LinString*).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { for } from "std/iter"
var out: {} = {}
["a", "b", "c"].for(k =>
  out[k] = 1
)
print(toString(out))
"#);
    assert_eq!(out, vec![r#"{"a": 1, "b": 1, "c": 1}"#]);

    // Churn loop: building an object via index-assign of a callback param across many
    // iterations must not leak (verified under the ASan CI leg); functionally just completes.
    let out = run(r#"import { print } from "std/io"
import { for, range } from "std/iter"
val main = (): Null =>
  range(0, 2000).for(i =>
    var o: {} = {}
    o["k"] = i
    null
  )
  print("done")
main()
"#);
    assert_eq!(out, vec!["done"]);
}

// Regression: `==` against a boxed-key projection operand was ORDER-DEPENDENT. Inside a
// for/map callback, `m[k]` (with `k` the boxed callback param) is a boxed-AnyVal projection,
// not a raw value. `compile_eq` dispatched on the static operand type and called
// `lin_string_eq`/etc. expecting a raw pointer, so it misread the box: `m[k] == "abc"` was
// true but `"abc" == m[k]` was FALSE. The fix routes BOTH orderings through the tagged
// runtime ops (lin_tagged_eq) when either operand is a boxed union, boxing the concrete
// side — so the comparison is symmetric. This silently broke `schema[k]["type"] == "string"`
// validation.
#[test]
fn eq_boxed_key_projection_is_order_symmetric() {
    // String: boxed-key projection vs literal, both orderings.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { for } from "std/iter"
val m = { "host": "abc" }
["host"].for(k =>
  print(toString(m[k] == "abc"))
  print(toString("abc" == m[k]))
  print(toString(m[k] == "nope"))
  print(toString("nope" == m[k]))
)
"#);
    assert_eq!(out, vec!["true", "true", "false", "false"]);

    // Int: boxed-key projection vs literal, both orderings.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { for } from "std/iter"
val m = { "n": 42 }
["n"].for(k =>
  print(toString(m[k] == 42))
  print(toString(42 == m[k]))
  print(toString(m[k] == 7))
  print(toString(7 == m[k]))
)
"#);
    assert_eq!(out, vec!["true", "true", "false", "false"]);

    // Nested projection-in-closure config-validation shape: sch[k]["type"] == "string"
    // compared both orderings (and `!=`).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { for } from "std/iter"
val sch = { "host": { "type": "string" }, "port": { "type": "number" } }
["host", "port"].for(k =>
  print(toString(sch[k]["type"] == "string"))
  print(toString("string" == sch[k]["type"]))
  print(toString(sch[k]["type"] != "string"))
)
"#);
    assert_eq!(out, vec!["true", "true", "false", "false", "false", "true"]);
}

// ---------------------------------------------------------------------------
// fromJson type-directed decode (ADR-031)
// ---------------------------------------------------------------------------

#[test]
fn test_from_json_decoding() {
    // Consolidated `fromJson` decoder behaviours. These were 15 separate one-build tests; each
    // compiles+links the whole stdlib, so they are merged into a single program — one build,
    // every original assertion preserved in order (one labelled output line per former test).
    // Shapes that were identical across the originals share the `Person = {name,age}` type;
    // distinct shapes keep their own named type. The match/`is`-arm idiom keeps its own test
    // below (different program shape + a non-deterministic message assertion).
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type Person = { "name": String, "age": Int32 }
type Opt = { "name": String, "nick": String | Null }
type IntBox = { "n": Int32 }
type FloatT = Float64
type Addr = { "city": String }
type NestedPerson = { "name": String, "address": Addr }
type IntArr = Int32[]
type Pair = [String, Int32]
type Shape = { "k": String, "r": Float64 } | { "k": String, "w": Int32 }
type Tree = { "value": Int32, "children": Tree[] }

// object_success
val obj = Person.fromJson({ "name": "Bob", "age": 30 })
print(if obj["type"] == "error" then "ERR" else "${obj["name"]} ${obj["age"]}")

// direct_call_form: fromJson(T, j) equals T.fromJson(j)
val direct = fromJson(Person, { "name": "Zoe", "age": 9 })
print(if direct["type"] == "error" then "ERR" else "${direct["name"]} ${direct["age"]}")

// missing_required_field
val missing = Person.fromJson({ "name": "Bob" })
print(if missing["type"] == "error" then "ERR" else "OK")

// missing_nullable_field_ok
val nullable = Opt.fromJson({ "name": "Bob" })
print(if nullable["type"] == "error" then "ERR" else "OK ${nullable["name"]}")

// extra_field_ignored
val extra = Person.fromJson({ "name": "Bob", "age": 30, "extra": true })
print(if extra["type"] == "error" then "ERR" else "OK ${extra["name"]}")

// wrong_type
val wrong = Person.fromJson({ "name": "Bob", "age": "x" })
print(if wrong["type"] == "error" then "ERR ${wrong["path"]}" else "OK")

// int_range_reject: `3.14` is non-integral; `5000000000.0` is integral but exceeds Int32's
// range. (A bare suffixless integer literal like 5000000000 is truncated to Int32 by the lexer
// before it ever reaches the decoder — spec §21 — so the overflow case is expressed as a float.)
val rangeA = IntBox.fromJson({ "n": 3.14 })
val rangeB = IntBox.fromJson({ "n": 5000000000.0 })
print(if rangeA["type"] == "error" then "a ERR" else "a OK")
print(if rangeB["type"] == "error" then "b ERR" else "b OK")

// float_accepts_int
val flt = FloatT.fromJson(5)
print(if flt["type"] == "error" then "ERR" else "OK ${flt}")

// nested_object
val nestedOk = NestedPerson.fromJson({ "name": "A", "address": { "city": "NYC" } })
val nestedBad = NestedPerson.fromJson({ "name": "A", "address": { "city": 5 } })
print(if nestedOk["type"] == "error" then "ERR" else "OK ${nestedOk["address"]["city"]}")
print(if nestedBad["type"] == "error" then "ERR ${nestedBad["path"]}" else "OK")

// array
val arrBad = IntArr.fromJson([1, 2, "x"])
print(if arrBad["type"] == "error" then "ERR ${arrBad["path"]}" else "OK")

// fixed_array
val pairOk = Pair.fromJson(["a", 7])
val pairLen = Pair.fromJson(["a", 7, 9])
print(if pairOk["type"] == "error" then "ERR" else "OK ${pairOk[0]} ${pairOk[1]}")
print(if pairLen["type"] == "error" then "LEN_ERR" else "OK")

// union_variant: first structurally-matching variant wins (ADR-031)
val unionOk = Shape.fromJson({ "k": "circle", "r": 1.5 })
val unionNone = Shape.fromJson({ "k": "x", "z": 9 })
print(if unionOk["type"] == "error" then "ERR" else "OK ${unionOk["k"]}")
print(if unionNone["type"] == "error" then "NONE" else "OK")

// recursive_type: exercises the descriptor back-edge: a recursive type must terminate
val treeOk = Tree.fromJson({ "value": 1, "children": [{ "value": 2, "children": [] }] })
val treeBad = Tree.fromJson({ "value": 1, "children": [{ "value": "x", "children": [] }] })
print(if treeOk["type"] == "error" then "ERR" else "OK ${treeOk["children"][0]["value"]}")
print(if treeBad["type"] == "error" then "ERR ${treeBad["path"]}" else "OK")

// error_value_shape: a decode Error carries type/message/path
val errVal = Person.fromJson({ "name": "Bob", "age": "x" })
print("${errVal["type"]}")
print(if errVal["message"] == null then "NO_MSG" else "HAS_MSG")
print("${errVal["path"]}")

// is_error_discriminates: `is Error` (ADR-031) distinguishes a decode FAILURE from a
// successfully-decoded value: the Error object carries `"type": "error"`, a decoded Person does
// not. `is Error` desugars to the value-constrained object pattern `{ "type": "error", .. }`.
val good = Person.fromJson({ "name": "Ada", "age": 36 })
val bad = Person.fromJson({ "name": "Bob", "age": "old" })
print(if good is Error then "good:ERR" else "good:OK")
print(if bad is Error then "bad:ERR" else "bad:OK")
"#);
    assert_eq!(
        out,
        vec![
            "Bob 30",              // object_success
            "Zoe 9",               // direct_call_form
            "ERR",                 // missing_required_field
            "OK Bob",              // missing_nullable_field_ok
            "OK Bob",              // extra_field_ignored
            "ERR $.age",           // wrong_type
            "a ERR",               // int_range_reject (non-integral)
            "b ERR",               // int_range_reject (overflow)
            "OK 5",                // float_accepts_int
            "OK NYC",              // nested_object (ok)
            "ERR $.address.city",  // nested_object (bad)
            "ERR $[2]",            // array
            "OK a 7",              // fixed_array (ok)
            "LEN_ERR",             // fixed_array (wrong length)
            "OK circle",           // union_variant (ok)
            "NONE",                // union_variant (no match)
            "OK 2",                // recursive_type (ok)
            "ERR $.children[0].value", // recursive_type (bad)
            "error",               // error_value_shape (type)
            "HAS_MSG",             // error_value_shape (message)
            "$.age",               // error_value_shape (path)
            "good:OK",             // is_error_discriminates (good)
            "bad:ERR",             // is_error_discriminates (bad)
        ]
    );
}

#[test]
fn test_from_json_match_is_error_idiom() {
    // The idiom `match result | is Error => .. | is Person => ..`. As of ADR-036 the arm order
    // is no longer load-bearing (`is Person` checks required fields, so it does not match the
    // Error object), but the Error-first form remains valid. Exhaustiveness accepts `is Error`
    // as covering the Error variant of `Person | Error`.
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type Person = { "name": String, "age": Int32 }
val describe = (r: Person | Error): Null =>
  match r
    is Error => print("err:${r["message"]}")
    is Person => print("ok:${r["name"]}")
val main = (): Null =>
  describe(Person.fromJson({ "name": "Ada", "age": 36 }))
  describe(Person.fromJson({ "name": "Bob", "age": "old" }))
main()
"#);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0], "ok:Ada");
    assert!(out[1].starts_with("err:"), "expected decode error, got {}", out[1]);
}

// Cast-hole closing (ADR-045): AnyVal -> concrete structured object is now a type error.

#[test]
fn test_json_to_concrete_now_errors() {
    // The TWO-STEP form: a AnyVal-typed identifier assigned to a structured concrete object is a
    // type error (ADR-045). NOTE: this form already worked before the headline fix — see
    // test_json_call_result_to_concrete_now_errors for the real call-result hazard.
    let err = run_expect_err(r#"type Person = { "name": String, "age": Int32 }
val j: AnyVal = { "name": "Bob", "age": 30 }
val p: Person = j
"#);
    assert!(
        err.contains("Person") || err.contains("4294967295") || (err.to_lowercase().contains("json") || err.to_lowercase().contains("anyval")),
        "expected a AnyVal->Person type error, got:\n{}",
        err
    );
}

#[test]
fn test_json_call_result_to_concrete_now_errors() {
    // HEADLINE case (ADR-045): the RHS is a *call* whose return type is AnyVal (here the stdlib
    // `readJson`), assigned to a structured concrete object. This must be a type error. Before
    // the fix this type-checked clean because the bidirectional `val` path propagated the
    // expected concrete type down and a zero/AnyVal-param function was misclassified as opaque,
    // freshening its AnyVal return into a permissive inference var.
    let err = run_expect_err(r#"import { readJson } from "std/fs"
type Person = { "name": String, "age": Int32 }
val p: Person = readJson("p.json")
"#);
    assert!(
        err.contains("Person") || err.contains("4294967295") || (err.to_lowercase().contains("json") || err.to_lowercase().contains("anyval")),
        "expected a AnyVal call-result -> Person type error, got:\n{}",
        err
    );
}

#[test]
fn test_json_local_call_result_to_concrete_now_errors() {
    // Same headline hazard with a LOCAL AnyVal-returning function (zero params). The opaque-
    // Function misclassification used to freshen its `AnyVal` return for zero-param functions,
    // letting `val p: Person = getJson()` slip through. Must now error.
    let err = run_expect_err(r#"type Person = { "name": String, "age": Int32 }
val getJson = (): AnyVal => { "name": "Bob", "age": 30 }
val p: Person = getJson()
"#);
    assert!(
        err.contains("Person") || err.contains("4294967295") || (err.to_lowercase().contains("json") || err.to_lowercase().contains("anyval")),
        "expected a local AnyVal call-result -> Person type error, got:\n{}",
        err
    );
}

#[test]
fn test_json_arg_to_concrete_param_errors() {
    // Passing a AnyVal value into a concrete structured-object parameter is rejected (ADR-045).
    let err = run_expect_err(r#"type Person = { "name": String, "age": Int32 }
val greet = (p: Person): String => p["name"]
val j: AnyVal = { "name": "Bob", "age": 30 }
val r = greet(j)
"#);
    assert!(
        err.contains("Person") || err.contains("4294967295") || (err.to_lowercase().contains("json") || err.to_lowercase().contains("anyval")),
        "expected a AnyVal-arg type error, got:\n{}",
        err
    );
}

#[test]
fn test_for_callback_element_is_typed() {
    // `for` is `<T>(T[] | … , (T, Int32) => AnyVal)`: the callback element is typed `T`, so passing
    // a `String` element to an `Int32`-requiring function is a compile error (closing the old
    // `for(iterable: AnyVal, f: (AnyVal, …))` hole where the callback param was untyped `AnyVal`).
    let err = run_expect_err(r#"import { print } from "std/io"
import { for } from "std/iter"
val needsInt = (n: Int32): Int32 => n + 1
["a", "b"].for(x => print("${needsInt(x)}"))
"#);
    assert!(
        err.contains("String") && err.contains("Int32"),
        "expected a String-vs-Int32 type error from the for callback, got:\n{}",
        err
    );
}

#[test]
fn test_for_over_iterator_and_nullable_and_empty_branches() {
    // Regressions the generic `for` exposed and fixed: (1) `for` over an opaque `Iterator`
    // (element TypeVar defaults to AnyVal); (2) `for` over a `T[] | Null` map lookup (Null is a
    // no-op receiver); (3) `if`/`match` whose `else`/`is Null` branch is an empty `[]` no longer
    // mis-infers the result as `Never[]` — the non-empty branch's element type dominates.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { iter, for } from "std/iter"
val it = iter(() => 0, i => i < 3, i => i + 1, i => i * 10)
it.for(x => print(toString(x)))
val pick = (m: { String: Int32[] }, k: String): Null =>
  val xs = match m[k]
    is Null => []
    else => m[k]
  xs.for(v => print(toString(v)))
pick({ "a": [7, 8] }, "a")
pick({ "a": [7, 8] }, "missing")
"#);
    assert_eq!(out, vec!["0", "10", "20", "7", "8"]);
}

#[test]
fn test_keys_rejects_scalar_argument() {
    // `keys`/`values`/`entries` take `{ String: AnyVal } | {}`, so a scalar argument is a
    // compile error (closing the old `keys(obj: AnyVal)` hole where `keys(1)` type-checked).
    let err = run_expect_err(r#"import { keys } from "std/object"
import { print } from "std/io"
val r = keys(1)
print("${r}")
"#);
    assert!(
        err.contains("Int32") || err.contains("String") || err.to_lowercase().contains("expected"),
        "expected a type error rejecting a scalar to keys, got:\n{}",
        err
    );
}

#[test]
fn test_keys_values_entries_accept_record_map_json() {
    // The `{ String: AnyVal } | {}` parameter still accepts all three valid object shapes: a record
    // literal, a typed index-signature map, and a `AnyVal` value carrying an object.
    let out = run(r#"import { keys, values, entries } from "std/object"
import { length } from "std/array"
import { print } from "std/io"
import { toString } from "std/string"
val rec = { "a": 1, "b": 2 }
print(toString(length(keys(rec))))
val m: { String: Int32 } = { "x": 10, "y": 20 }
print(toString(length(values(m))))
val j: AnyVal = { "p": 1 }
print(toString(length(entries(j))))
"#);
    assert_eq!(out, vec!["2", "2", "1"]);
}

#[test]
fn test_concrete_to_json_still_ok() {
    // Concrete value -> AnyVal (covariant sink) still compiles.
    let out = run(r#"import { print } from "std/io"
val f = (x: AnyVal): AnyVal => x
val p = { "name": "Bob", "age": 30 }
print("${f(p)["name"]}")
"#);
    assert_eq!(out, vec!["Bob"]);
}

#[test]
fn test_is_narrowing_still_works() {
    // is-narrowing of a AnyVal value into a concrete branch still compiles + runs.
    let out = run(r#"import { print } from "std/io"
val pick = (j: AnyVal): String =>
  if j is String then j else "not-a-string"
print(pick("hi"))
print(pick(42))
"#);
    assert_eq!(out, vec!["hi", "not-a-string"]);
}

// ── `T | Null` complement narrowing on a null-test guard (== null / != null / is Null) ──
// Before this change all four forms FAILED to type-check: the branch that excludes Null still
// saw `v: Int32 | Null`, so a function declared `: Int32` rejected with
// "Function body has type Int32 | Null, declared return type is Int32".

#[test]
fn test_null_narrow_guard_forms() {
    // Consolidated `T | Null` complement narrowing on a null-test guard (4 former one-build tests
    // → one program). Each form excludes Null in the branch a function declared `: Int32` returns
    // from; before the change all four FAILED to type-check ("body has type Int32 | Null").
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
// (a) `if v == null then 0 else v` — Null excluded in ELSE.
val eqElse = (v: Int32 | Null): Int32 =>
  if v == null then 0 else v
// (b) `if v is Null then 0 else v` — Null excluded in ELSE.
val isElse = (v: Int32 | Null): Int32 =>
  if v is Null then 0 else v
// (c) `match v / is Null => 0 / else => v` — else arm narrows v to Int32.
val matchElse = (v: Int32 | Null): Int32 =>
  match v
    is Null => 0
    else => v
// (d) `if v != null then v else 0` — POSITIVE guard: Null excluded in THEN.
val neqThen = (v: Int32 | Null): Int32 =>
  if v != null then v else 0
print(eqElse(7).toString())
print(eqElse(null).toString())
print(isElse(7).toString())
print(isElse(null).toString())
print(matchElse(7).toString())
print(matchElse(null).toString())
print(neqThen(7).toString())
print(neqThen(null).toString())
"#);
    assert_eq!(out, vec!["7", "0", "7", "0", "7", "0", "7", "0"]);
}

#[test]
fn test_null_narrow_json_map_read() {
    // The motivating case (ADR-055): reading a `{ String: Int32 }` value yields `Int32 | Null`;
    // binding it and null-testing narrows it to Int32 in the non-null branch. Covers all four
    // forms over a real index-signature map read (present key + missing key).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val m: { String: Int32 } = { "a": 5 }
val viaEq = (k: String): Int32 =>
  val v = m[k]
  if v == null then 0 else v
val viaNeq = (k: String): Int32 =>
  val v = m[k]
  if v != null then v else -1
val viaMatch = (k: String): Int32 =>
  val v = m[k]
  match v
    is Null => 0
    else => v
print(viaEq("a").toString())
print(viaEq("missing").toString())
print(viaNeq("a").toString())
print(viaNeq("missing").toString())
print(viaMatch("a").toString())
print(viaMatch("missing").toString())
"#);
    assert_eq!(out, vec!["5", "0", "5", "-1", "5", "0"]);
}

#[test]
fn test_null_narrow_three_member_union_keeps_rest() {
    // `A | B | Null` minus Null = `A | B` (not collapsed to a single member). The narrowed branch
    // accepts a value typed `Int32 | String`, and a subsequent `is` arm discriminates it.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val describe = (v: Int32 | String | Null): String =>
  if v == null then "null" else
    match v
      is Int32 => "int:${v.toString()}"
      else => "str"
print(describe(7))
print(describe("hi"))
print(describe(null))
"#);
    assert_eq!(out, vec!["int:7", "str", "null"]);
}

// ── Generalized complement narrowing: ANY guard-free `is X` arm subtracts `X` from the union in
// the branch that excluded it (not just `is Null`). The motivating category is `T | Error`. ──

#[test]
fn test_complement_narrow_forms() {
    // Consolidated generalized complement narrowing (5 former one-build tests → one program; each
    // case keeps a uniquely-named function and its assertions in order). ANY guard-free `is X` arm
    // subtracts `X` from the union in the branch that excluded it (not just `is Null`).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"

// match_error_else: `match r / is Error => fallback / else => r` narrows the else arm to bare
// `String`, even though `Error` is the STRUCTURAL alias { "type", "message" }, not a named member.
val unwrapMatch = (r: String | Error): String =>
  match r
    is Error => "fallback"
    else => r
print(unwrapMatch("hello"))
print(unwrapMatch({ "type": "error", "message": "boom" }))

// if_error_else: if-form of the Error case narrows the else branch (Error excluded) to `String`.
val unwrapIf = (r: String | Error): String =>
  if r is Error then "fallback" else r
print(unwrapIf("hi"))
print(unwrapIf({ "type": "error", "message": "bad" }))

// match_nonerror_member: `Int32 | String` minus the `is Int32` arm = `String` in the else arm.
val pickStr = (v: Int32 | String): String =>
  match v
    is Int32 => "was int"
    else => v
print(pickStr(42))
print(pickStr("plain"))

// three_member_minus_two: two guard-free `is` arms subtract BOTH tested types, leaving `String`.
val classify = (v: Int32 | String | Boolean): String =>
  match v
    is Boolean => "bool"
    is Int32 => "int"
    else => v
print(classify(true))
print(classify(7))
print(classify("hi"))

// guard_does_not_exclude (SOUNDNESS): a `when`-guarded `is` arm does NOT guarantee exclusion, so
// it must NOT contribute to the complement. The only Int32-testing arm here is guarded, so the
// `else` can still be reached with an Int32 (guard false) — it must STAY `Int32 | String`, NOT
// narrow to `String`. Proven by declaring the return `Int32 | String` and flowing an unnarrowed
// Int32 through `else`.
val guarded = (v: Int32 | String): Int32 | String =>
  match v
    is Int32 when v > 100 => "big"
    else => v
print(guarded(5).toString())
print(guarded("hi").toString())
"#);
    assert_eq!(
        out,
        vec![
            "hello", "fallback",  // match_error_else
            "hi", "fallback",     // if_error_else
            "was int", "plain",   // match_nonerror_member
            "bool", "int", "hi",  // three_member_minus_two
            "5", "hi",            // guard_does_not_exclude
        ]
    );
}

#[test]
fn test_is_objecttype_expr_checks_required_fields() {
    // Regression (ADR-036): the EXPRESSION form `x is Person` must check that the object has
    // Person's required fields, not just that it is some object (bare TAG_OBJECT). Previously a
    // non-Person object matched, then the narrowed `x["name"]` faulted/returned null.
    let out = run(r#"import { print } from "std/io"
type Person = { "name": String, "age": Int32 }
val full = { "name": "Ada", "age": 36 }
val partial = { "name": "Bob" }
val other = { "foo": "bar" }
print(if full is Person then "full:${full["name"]}" else "full:no")
print(if partial is Person then "partial:yes" else "partial:no")
print(if other is Person then "other:yes" else "other:no")
"#);
    assert_eq!(out, vec!["full:Ada", "partial:no", "other:no"]);
}

#[test]
fn test_is_person_first_arm_no_longer_faults() {
    // Regression (ADR-036): with required-field checking, `is Person` as the FIRST arm no longer
    // swallows a decode-error object — the ADR-033 ordering footgun is gone. A decode failure
    // (which lacks name/age) falls through to the Error arm instead of faulting on r["name"].
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type Person = { "name": String, "age": Int32 }
val describe = (r: Person | Error): Null =>
  match r
    is Person => print("ok:${r["name"]}")
    is Error => print("err")
val main = (): Null =>
  describe(Person.fromJson({ "name": "Ada", "age": 36 }))
  describe(Person.fromJson({ "name": "Bob", "age": "old" }))
main()
"#);
    assert_eq!(out, vec!["ok:Ada", "err"]);
}

// ── `is <ObjectType>` deep type validation (ADR-036) ──────────────────────────

#[test]
fn test_is_objecttype_deep_validation() {
    // Consolidated `is <ObjectType>` deep type validation (ADR-035/036; 4 former one-build tests →
    // one program, uniquely-named types, assertions preserved in order). `is T` deep-validates
    // field TYPES (not just presence), recurses into nested objects, narrows soundly on a match,
    // and inherits fromJson's number policy.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Person = { "name": String, "age": Int32 }
type Nested = { "addr": { "zip": Int32 } }
type NumBox = { "n": Int32 }
type DataBox = { "data": AnyVal }

val main = (): Null =>
  // rejects_wrong_field_type: age as a string (keys present, WRONG type) must NOT match.
  val badType: DataBox = { "data": { "name": "ok", "age": "not-an-int" } }
  val v1: AnyVal = badType["data"]
  print(if v1 is Person then "WRONG-MATCH" else "rejected")
  val goodType: DataBox = { "data": { "name": "ok", "age": 5 } }
  val w1: AnyVal = goodType["data"]
  print(if w1 is Person then "matched" else "WRONG-NO-MATCH")

  // deep_nested: validation recurses into NESTED object fields.
  val badNest: DataBox = { "data": { "addr": { "zip": "oops" } } }
  val v2: AnyVal = badNest["data"]
  print(if v2 is Nested then "WRONG" else "nested-rejected")
  val goodNest: DataBox = { "data": { "addr": { "zip": 90210 } } }
  val w2: AnyVal = goodNest["data"]
  print(if w2 is Nested then "nested-matched" else "WRONG")

  // accepts_valid_and_narrows: a well-typed value matches AND the narrowed field access is sound.
  val ok: DataBox = { "data": { "name": "Ada", "age": 36 } }
  val v3: AnyVal = ok["data"]
  if v3 is Person then print("age+1=${toString(v3["age"] + 1)}") else print("no")

  // number_policy: a non-integral number fails an Int target; an integral float (5.0) satisfies it.
  val frac: DataBox = { "data": { "n": 3.14 } }
  val v4: AnyVal = frac["data"]
  print(if v4 is NumBox then "WRONG-frac" else "frac-rejected")
  val whole: DataBox = { "data": { "n": 5.0 } }
  val w4: AnyVal = whole["data"]
  print(if w4 is NumBox then "integral-matched" else "WRONG-int")
main()
"#);
    assert_eq!(
        out,
        vec![
            "rejected", "matched",                  // rejects_wrong_field_type
            "nested-rejected", "nested-matched",    // deep_nested
            "age+1=37",                             // accepts_valid_and_narrows
            "frac-rejected", "integral-matched",    // number_policy
        ]
    );
}

#[test]
fn test_is_error_still_discriminates_after_deep() {
    // ADR-035 regression: `is Error` (a value-constrained object pattern, NOT TypeCheckDeep) is
    // untouched and still discriminates a decode failure from a decoded value, in either arm order.
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type Person = { "name": String, "age": Int32 }
val describe = (r: Person | Error): Null =>
  match r
    is Error => print("err")
    is Person => print("ok:${r["name"]}")
val main = (): Null =>
  describe(Person.fromJson({ "name": "Ada", "age": 36 }))
  describe(Person.fromJson({ "name": "Bob", "age": "old" }))
main()
"#);
    assert_eq!(out, vec!["ok:Ada", "err"]);
}

// ── singleton string-literal types (ADR-034) ──────────────────────────────────

#[test]
fn test_literal_type_good_assignment() {
    // A discriminated tagged-union value with the correct literal tag is accepted, and the
    // match/has arms discriminate at runtime.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Result<T, E> = { "type": "success", "value": T } | { "type": "failure", "error": E }
val r: Result<Int32, String> = { "type": "success", "value": 7 }
val msg = match r
  has { "type": "success", value } => "ok ${toString(value)}"
  has { "type": "failure", error } => "err ${error}"
  else => "?"
print(msg)
"#);
    assert_eq!(out, vec!["ok 7"]);
}

#[test]
fn test_literal_type_wrong_tag_rejected() {
    // An object with a tag that matches no variant is a compile error naming the valid tags.
    let err = run_expect_err(r#"import { print } from "std/io"
type Result<T, E> = { "type": "success", "value": T } | { "type": "failure", "error": E }
val bad: Result<Int32, String> = { "type": "nope", "value": 1 }
print("x")
"#);
    assert!(err.contains("nope") || err.contains("success") || err.contains("failure"),
        "expected the wrong-tag error to mention the bad/valid tags, got:\n{}", err);
}

#[test]
fn test_string_not_assignable_to_literal() {
    // A plain String value is NOT assignable to a singleton literal type (load-bearing reject).
    let err = run_expect_err(r#"import { print } from "std/io"
type Tag = "ok"
val s: String = "ok"
val t: Tag = s
print("x")
"#);
    assert!(err.contains("ok") && (err.contains("Expected") || err.contains("String")),
        "expected a literal-type rejection, got:\n{}", err);
}

#[test]
fn test_literal_assignable_to_string() {
    // A literal-typed value widens to String (ADR-035 rule 2).
    let out = run(r#"import { print } from "std/io"
type Tag = "ok"
val t: Tag = "ok"
val s: String = t
print(s)
"#);
    assert_eq!(out, vec!["ok"]);
}

#[test]
fn test_bare_string_literal_still_string() {
    // §25: a bare string-literal VALUE still infers to String, usable everywhere a String is.
    let out = run(r#"import { print } from "std/io"
val x = "foo"
val y: String = x
val use = (s: String): String => s
print(use(x))
print(y)
"#);
    assert_eq!(out, vec!["foo", "foo"]);
}

#[test]
fn test_spec18_divide_discriminates() {
    // The spec §19 divide()/Result example runs and discriminates both branches at runtime.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Result<T, E> = { "type": "success", "value": T } | { "type": "failure", "error": E }
val divide = (a: Float64, b: Float64): Result<Float64, String> =>
  if b == 0.0 then { "type": "failure", "error": "Cannot divide by zero" }
  else { "type": "success", "value": a / b }
val message = (r: Result<Float64, String>): String =>
  match r
    has { "type": "success", value } => "Result: ${toString(value)}"
    has { "type": "failure", error } => "Error: ${error}"
    else => "?"
print(message(divide(10.0, 2.0)))
print(message(divide(1.0, 0.0)))
"#);
    assert_eq!(out, vec!["Result: 5.0", "Error: Cannot divide by zero"]);
}

#[test]
fn test_literal_type_survives_generic_substitution() {
    // Literal tags survive generic substitution in BOTH orderings of the type params.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Result<T, E> = { "type": "success", "value": T } | { "type": "failure", "error": E }
val a: Result<Int32, String> = { "type": "success", "value": 42 }
val b: Result<String, Int32> = { "type": "failure", "error": 7 }
val showA = (r: Result<Int32, String>): String =>
  match r
    has { "type": "success", value } => "A ok ${toString(value)}"
    has { "type": "failure", error } => "A err ${error}"
    else => "?"
val showB = (r: Result<String, Int32>): String =>
  match r
    has { "type": "success", value } => "B ok ${value}"
    has { "type": "failure", error } => "B err ${toString(error)}"
    else => "?"
print(showA(a))
print(showB(b))
"#);
    assert_eq!(out, vec!["A ok 42", "B err 7"]);
}

#[test]
fn test_match_json_arm_plus_object_arm_against_declared_object_return() {
    // Regression: the match-arm-union-vs-declared-object bug. A handler declared to return a named
    // object type `R`, whose `match` has one arm yielding a `AnyVal` value and another yielding a
    // concrete object literal, previously formed `AnyVal | {concrete}` and rejected it against `R`.
    // Each arm is now checked against `R` directly (bidirectional push). Both arms must produce a
    // value indexable as `R` at runtime.
    let out = run(r#"import { print } from "std/io"
type R = { "status": Int32, "headers": AnyVal, "body": String }
val other = (): AnyVal => { "status": 200, "headers": { "a": 1 }, "body": "ok" }
val handle = (b: Boolean): R =>
  match b
    is true => other()
    else => { "status": 404, "headers": { "a": 1 }, "body": "no" }
print(handle(true)["body"])
print(handle(false)["body"])
print("status ${handle(true)["status"]}")
"#);
    assert_eq!(out, vec!["ok", "no", "status 200"]);
}

#[test]
fn test_if_json_arm_plus_object_arm_against_declared_object_return() {
    // Same bug, `if` form: `if cond then jsonValue else objectLiteral` declared `: R`.
    let out = run(r#"import { print } from "std/io"
type R = { "status": Int32, "headers": AnyVal, "body": String }
val other = (): AnyVal => { "status": 200, "headers": { "a": 1 }, "body": "ok" }
val handle = (b: Boolean): R =>
  if b then other() else { "status": 404, "headers": { "a": 1 }, "body": "no" }
print(handle(true)["body"])
print(handle(false)["body"])
"#);
    assert_eq!(out, vec!["ok", "no"]);
}

#[test]
fn test_multiline_union_leading_pipe() {
    // The spec §19 canonical form: a multi-line tagged union with a leading `|` on each
    // variant in a `type` alias. Previously failed to parse ("unexpected token Pipe")
    // because the indented body's INDENT token sat between `=` and the first `|`.
    let out = run(r#"import { print } from "std/io"
type Result =
  | { "type": "success", "value": Int32 }
  | { "type": "failure", "error": String }
val r: Result = { "type": "success", "value": 7 }
val msg = match r
  has { "type": "success", "value": v } => "ok ${v}"
  has { "type": "failure", "error": e } => "err ${e}"
  else => "?"
print(msg)
"#);
    assert_eq!(out, vec!["ok 7"]);
}

#[test]
fn test_multiline_union_no_leading_pipe() {
    // Multi-line union whose first variant has no leading pipe and a `|` continues the
    // next line. Previously this STACK-OVERFLOWED the parser; now it parses and runs.
    let out = run(r#"import { print } from "std/io"
type Result =
  { "type": "success", "value": Int32 }
  | { "type": "failure", "error": String }
val r: Result = { "type": "failure", "error": "boom" }
val msg = match r
  has { "type": "success", "value": v } => "ok ${v}"
  has { "type": "failure", "error": e } => "err ${e}"
  else => "?"
print(msg)
"#);
    assert_eq!(out, vec!["err boom"]);
}

#[test]
fn test_multiline_single_variant_body_then_decl() {
    // An indented single-variant body (no pipe) must not swallow the following decl:
    // the trailing Dedent is consumed without over-running the statement boundary.
    let out = run(r#"import { print } from "std/io"
type Box =
  { "value": Int32 }
type Other = { "x": String }
val b: Box = { "value": 9 }
val o: Other = { "x": "hi" }
print("${b["value"]} ${o["x"]}")
"#);
    assert_eq!(out, vec!["9 hi"]);
}

#[test]
fn test_from_json_strlit_discriminates_union() {
    // ADR-033: fromJson validates the exact literal value of a StrLit field, so a tagged-union
    // decode discriminates by the discriminant tag. Correct tags decode to the right variant;
    // first-match-wins probes each variant's KIND_STRLIT check.
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type Result = { "type": "success", "value": Int32 } | { "type": "failure", "error": String }
val show = (j: AnyVal): String =>
  val r = Result.fromJson(j)
  match r
    is Error => "decode-error"
    has { "type": "success", "value": v } => "ok ${v}"
    has { "type": "failure", "error": e } => "fail ${e}"
    else => "?"
print(show({ "type": "success", "value": 7 }))
print(show({ "type": "failure", "error": "boom" }))
"#);
    assert_eq!(out, vec!["ok 7", "fail boom"]);
}

#[test]
fn test_from_json_strlit_rejects_wrong_tag() {
    // ADR-033: a wrong discriminant value is a decode error (was a silent mis-decode under the
    // old KIND_STRING placeholder), with a path-located message.
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type Tagged = { "kind": "alpha", "n": Int32 }
val r = Tagged.fromJson({ "kind": "beta", "n": 1 })
match r
  is Error => print("err: ${r["message"]}")
  else => print("ok")
"#);
    assert_eq!(out.len(), 1);
    assert!(out[0].contains("alpha") && out[0].contains("beta"),
        "expected literal-mismatch message naming both tags, got: {}", out[0]);
}

#[test]
fn test_from_json_plain_string_field_accepts_any() {
    // ADR-033 (KIND_STRLIT) must NOT regress plain String fields: they still encode as KIND_STRING and accept
    // any string value (only StrLit fields are value-checked).
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type Person = { "name": String, "age": Int32 }
val r = Person.fromJson({ "name": "anything goes", "age": 5 })
match r
  is Error => print("err")
  else => print("ok ${r["name"]}")
"#);
    assert_eq!(out, vec!["ok anything goes"]);
}

// ---------------------------------------------------------------------------
// Phase 0: monomorphized generic functions (single-module `identity<T>`).
// ---------------------------------------------------------------------------

#[test]
fn test_generic_identity_int_string_and_reuse() {
    // The canonical Phase-0 slice: one generic `val` instantiated at several types in the same
    // module — Int32 (which must run native, see the IR-proof test below), String, and Bool — with
    // Int32 used TWICE to exercise specialization de-duplication. (Merged from the former
    // `int_and_string` + `three_types_and_reuse` tests; the second already subsumed the first.)
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val identity = <T>(x: T): T => x
print(toString(identity(5)))
print(toString(identity(42)))
print(identity("hello"))
print(toString(identity(true)))
"#);
    assert_eq!(out, vec!["5", "42", "hello", "true"]);
}

#[test]
fn test_generic_identity_int_specialization_is_unboxed() {
    // IR proof: the T=Int32 specialization must pass/return a native i32 with NO
    // lin_box_int32/lin_unbox_int32 around the identity call or inside its body.
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let src_path = ws.join(format!("target/lin_test_gen_{}.lin", id));
    let bin_path = ws.join(format!("target/lin_test_gen_{}", id));
    let ll_path = bin_path.with_extension("ll");

    fs::write(&src_path, r#"import { print } from "std/io"
import { toString } from "std/string"
val identity = <T>(x: T): T => x
print(toString(identity(5)))
print(identity("hello"))
"#).unwrap();

    let compile = lin_cmd()
        .args(["build", src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .env("LIN_EMIT_IR", "1")
        .env("LIN_NO_OPT", "1")
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    let _ = fs::remove_file(&src_path);
    assert!(compile.status.success(), "compilation failed:\n{}",
        String::from_utf8_lossy(&compile.stderr));

    let ll = fs::read_to_string(&ll_path).expect("LLVM IR not emitted");
    let _ = fs::remove_file(&bin_path);
    let _ = fs::remove_file(&ll_path);

    // The specialization exists, takes and returns native i32.
    assert!(ll.contains("define i32 @\"identity$Int32\"(i32"),
        "expected an unboxed i32 specialization, IR:\n{}", ll);
    // The call site passes a native i32 directly (no boxing of the argument).
    assert!(ll.contains("call i32 @\"identity$Int32\"(i32 5)"),
        "expected a native-i32 call to the Int32 specialization, IR:\n{}", ll);

    // No box/unbox appears inside the identity$Int32 body. Slice out its definition and check.
    let body_start = ll.find("define i32 @\"identity$Int32\"").unwrap();
    let body = &ll[body_start..];
    let body_end = body.find("\n}").map(|e| e + 2).unwrap_or(body.len());
    let body = &body[..body_end];
    assert!(!body.contains("lin_box_int32") && !body.contains("lin_unbox_int32"),
        "identity$Int32 body must contain no int boxing, got:\n{}", body);
}

// ---------------------------------------------------------------------------
// Phase 4.5: element-type-aware array WRITE path. A monomorphized generic that
// allocates via `arrayAllocate` at a concrete-scalar element type must produce a
// FLAT array, so the flat-allocated producer matches the concrete-typed (flat)
// reader. Previously the alloc stayed tagged while the reader read flat → garbage.
// ---------------------------------------------------------------------------

#[test]
fn test_generic_array_allocate_int32_is_flat_and_correct() {
    // A generic allocator monomorphized at T=Int32: allocate, index-set, index-read, all as
    // a statically-typed Int32[]. Must print 40 (10 + 30). Before the fix this printed garbage
    // (a tagged array read through the flat i32 accessor).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val allocT = <T>(n: Int32, zero: T): T[] => lin_array_allocate(n)
val a: Int32[] = allocT(3, 0)
a[0] = 10
a[1] = 20
a[2] = 30
print(toString(a[0] + a[2]))
"#);
    assert_eq!(out, vec!["40"]);
}

#[test]
fn test_generic_array_allocate_int32_flat_path_in_ir() {
    // IR proof: the T=Int32 monomorph allocates FLAT (lin_flat_array_alloc_filled_i32) and the
    // reader uses the FLAT getter (lin_flat_array_get_i32) — producer and consumer agree, with
    // no tagged getter (lin_array_get_tagged) and no boxing of the read scalars on this path.
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let src_path = ws.join(format!("target/lin_test_flat_{}.lin", id));
    let bin_path = ws.join(format!("target/lin_test_flat_{}", id));
    let ll_path = bin_path.with_extension("ll");

    fs::write(&src_path, r#"import { print } from "std/io"
import { toString } from "std/string"
val allocT = <T>(n: Int32, zero: T): T[] => lin_array_allocate(n)
val a: Int32[] = allocT(3, 0)
a[0] = 10
a[1] = 20
a[2] = 30
print(toString(a[0] + a[2]))
"#).unwrap();

    let compile = lin_cmd()
        .args(["build", src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .env("LIN_EMIT_IR", "1")
        .env("LIN_NO_OPT", "1")
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    let _ = fs::remove_file(&src_path);
    assert!(compile.status.success(), "compilation failed:\n{}",
        String::from_utf8_lossy(&compile.stderr));

    let ll = fs::read_to_string(&ll_path).expect("LLVM IR not emitted");
    let _ = fs::remove_file(&bin_path);
    let _ = fs::remove_file(&ll_path);

    // The monomorph body allocates a FLAT i32 array.
    let body_start = ll.find("define ptr @\"allocT$Int32\"").expect("missing allocT$Int32 monomorph");
    let body = &ll[body_start..];
    let body_end = body.find("\n}").map(|e| e + 2).unwrap_or(body.len());
    let body = &body[..body_end];
    assert!(body.contains("lin_flat_array_alloc_filled_i32"),
        "allocT$Int32 must allocate a flat i32 array, got:\n{}", body);
    assert!(!body.contains("lin_array_alloc_null"),
        "allocT$Int32 must NOT allocate a tagged array, got:\n{}", body);

    // The reader uses the flat i32 getter (consumer matches producer).
    assert!(ll.contains("lin_flat_array_get_i32"),
        "expected a flat i32 read of the Int32[] value, IR:\n{}", ll);
}

#[test]
fn test_index_read_on_var_array_borrows_and_inlines() {
    // Two optimizations on the flat scalar-array read path, both visible in `scan`:
    //  1. RC-elision (borrowed base): reading an element from a module-`var` (global) array must
    //     NOT retain/release the array — the read borrows it; the global outlives the read.
    //  2. Inline flat read: the element load is emitted INLINE (len-load + bounds check + data
    //     load), not a `call lin_flat_array_get_i32` — the runtime accessor can't be inlined by
    //     LLVM (separate staticlib, no LTO), so a call per element dominated tight loops.
    // Together these were the linear-scan-PQ hot path: `pqDist[j] < pqDist[best]` was 2 retain +
    // 2 release + 2 calls per element; it is now plain loads.
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let src_path = ws.join(format!("target/lin_test_borrow_{}.lin", id));
    let bin_path = ws.join(format!("target/lin_test_borrow_{}", id));
    let ll_path = bin_path.with_extension("ll");

    fs::write(&src_path, r#"import { print } from "std/io"
import { toString } from "std/string"
import { arrayAllocateFilled, set } from "std/array"
var a = arrayAllocateFilled(8, 0)
set(a, 3, 9)
val scan = (j: Int32, best: Int32): Int32 =>
  if j >= 8 then best
  else if a[j] < a[best] then scan(j + 1, j)
  else scan(j + 1, best)
print(toString(scan(1, 0)))
"#).unwrap();

    let compile = lin_cmd()
        .args(["build", src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .env("LIN_EMIT_IR", "1")
        .env("LIN_NO_OPT", "1")
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    let _ = fs::remove_file(&src_path);
    assert!(compile.status.success(), "compilation failed:\n{}",
        String::from_utf8_lossy(&compile.stderr));

    let ll = fs::read_to_string(&ll_path).expect("LLVM IR not emitted");
    let _ = fs::remove_file(&bin_path);
    let _ = fs::remove_file(&ll_path);

    // Isolate the scan function body and assert it borrows (no RC on the array) AND reads the
    // element INLINE — the fast path is a direct load (`flat_get_ok` block + `flat_elem_p` GEP),
    // not a `call lin_flat_array_get_i32`. The runtime accessor only appears on the cold OOB path
    // (`flat_get_oob`), so a `call` to it inside the function is allowed but must NOT be on the
    // straight-line read path.
    let start = ll.find("define i32 @scan(").expect("missing scan fn");
    let body = &ll[start..];
    let end = body.find("\n}").map(|e| e + 2).unwrap_or(body.len());
    let body = &body[..end];
    assert!(body.contains("flat_get_ok") && body.contains("flat_elem_p"),
        "scan must read the flat array INLINE (fast-path load), got:\n{}", body);
    // The only `lin_flat_array_get_i32` reference may be the cold OOB fault path; the fast path
    // must be inline loads, so every such call sits in a `flat_get_oob` block.
    assert!(!body.contains("lin_rc_retain") && !body.contains("lin_array_release"),
        "scan must NOT retain/release the borrowed var array, got:\n{}", body);
}

#[test]
fn test_inline_flat_read_index_semantics() {
    // The inlined flat read must preserve `lin_flat_array_get`'s exact semantics: in-bounds
    // reads, Python-style negative indexing (`-1` = last, `-len` = first), and the boundary
    // indices `0` / `len-1`. (OOB faulting is covered separately; here every index is valid.)
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { arrayAllocateFilled, set } from "std/array"
val a: Int32[] = arrayAllocateFilled(4, 0)
set(a, 0, 10)
set(a, 1, 20)
set(a, 3, 40)
print("${toString(a[0])} ${toString(a[3])} ${toString(a[-1])} ${toString(a[-4])} ${toString(a[1])}")
"#);
    // a[0]=10, a[3]=40, a[-1]=a[3]=40, a[-4]=a[0]=10, a[1]=20
    assert_eq!(out, vec!["10 40 40 10 20"]);
}

#[test]
fn test_inline_flat_read_oob_still_faults() {
    // The inlined fast path's bounds check must still fault on out-of-bounds (spec §6.1), with the
    // same message the runtime accessor produced (the cold path defers to it). A non-zero exit +
    // the "out of bounds" diagnostic on stderr proves the inline bounds check fires.
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let src_path = ws.join(format!("target/lin_test_oob_{}.lin", id));
    let bin_path = ws.join(format!("target/lin_test_oob_{}", id));
    fs::write(&src_path, r#"import { print } from "std/io"
import { toString } from "std/string"
import { arrayAllocateFilled } from "std/array"
val a: Int32[] = arrayAllocateFilled(3, 7)
print(toString(a[5]))
"#).unwrap();
    let compile = lin_cmd()
        .args(["build", src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    let _ = fs::remove_file(&src_path);
    assert!(compile.status.success(), "compilation failed:\n{}",
        String::from_utf8_lossy(&compile.stderr));
    let run_out = Command::new(&bin_path).output().expect("failed to run compiled binary");
    let _ = fs::remove_file(&bin_path);
    assert!(!run_out.status.success(), "out-of-bounds read must fault (non-zero exit)");
    let stderr = String::from_utf8_lossy(&run_out.stderr);
    assert!(stderr.contains("out of bounds"),
        "expected an out-of-bounds runtime fault, got stderr:\n{}", stderr);
}

#[test]
fn test_borrowed_index_read_write_loops_are_correct() {
    // Behavioural guard for the borrowed Index / IndexSet container base: a global-`var` array
    // read in a loop, and direct `arr[i] = v` writes through a global-var array, must produce
    // correct results (the borrow must not drop/alias the container). Mirrors the dijkstra
    // pq-array access pattern that motivated the optimization.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { arrayAllocateFilled } from "std/array"
import { for } from "std/iter"

var a = arrayAllocateFilled(5, 0)
// direct index-set writes through the global-var array
var i = 0
[10, 20, 30, 40, 50].for(v =>
  a[i] = v
  i = i + 1
)
// repeated borrowed reads in a recursive scan
val sumFrom = (j: Int32, acc: Int32): Int32 =>
  if j >= 5 then acc else sumFrom(j + 1, acc + a[j])
print(toString(a[0]))
print(toString(a[4]))
print(toString(sumFrom(0, 0)))
"#);
    assert_eq!(out, vec!["10", "50", "150"]);
}

#[test]
fn test_generic_array_allocate_string_stays_tagged() {
    // A heap (NON-flat-scalar) element type must stay TAGGED: String[] is allocated tagged and
    // read tagged. Allocate, index-set string elements, read them back. Proves the flat path is
    // gated strictly to scalars and does not corrupt heap-element arrays.
    let out = run(r#"import { print } from "std/io"
val allocT = <T>(n: Int32, zero: T): T[] => lin_array_allocate(n)
val a: String[] = allocT(2, "")
a[0] = "hi"
a[1] = "there"
print(a[0])
print(a[1])
"#);
    assert_eq!(out, vec!["hi", "there"]);
}

#[test]
fn test_generic_array_allocate_string_tagged_path_in_ir() {
    // IR proof for the heap-element case: String[] monomorph stays tagged (lin_array_alloc_null),
    // never a flat allocator.
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let src_path = ws.join(format!("target/lin_test_strtag_{}.lin", id));
    let bin_path = ws.join(format!("target/lin_test_strtag_{}", id));
    let ll_path = bin_path.with_extension("ll");

    fs::write(&src_path, r#"import { print } from "std/io"
val allocT = <T>(n: Int32, zero: T): T[] => lin_array_allocate(n)
val a: String[] = allocT(2, "")
a[0] = "hi"
a[1] = "there"
print(a[0])
print(a[1])
"#).unwrap();

    let compile = lin_cmd()
        .args(["build", src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .env("LIN_EMIT_IR", "1")
        .env("LIN_NO_OPT", "1")
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    let _ = fs::remove_file(&src_path);
    assert!(compile.status.success(), "compilation failed:\n{}",
        String::from_utf8_lossy(&compile.stderr));

    let ll = fs::read_to_string(&ll_path).expect("LLVM IR not emitted");
    let _ = fs::remove_file(&bin_path);
    let _ = fs::remove_file(&ll_path);

    let body_start = ll.find("define ptr @\"allocT$Str\"")
        .or_else(|| ll.find("define ptr @\"allocT$String\""))
        .expect("missing allocT String monomorph");
    let body = &ll[body_start..];
    let body_end = body.find("\n}").map(|e| e + 2).unwrap_or(body.len());
    let body = &body[..body_end];
    assert!(body.contains("lin_array_alloc_null"),
        "String[] allocT must allocate a tagged array, got:\n{}", body);
    assert!(!body.contains("lin_flat_array_alloc"),
        "String[] allocT must NOT allocate a flat array, got:\n{}", body);
}

// ---------------------------------------------------------------------------
// Phase 4.5b: extend the element-type-aware flat-array WRITE path to the realistic
// map-shape combinator idiom where the allocation is an INTERMEDIATE `val` binding
// (`val result = lin_array_allocate(n); ...; result`) rather than the trivial
// `=> lin_array_allocate(n)` body. The checker pins the intermediate binding's
// element type to the declared-return element so monomorphization produces a flat
// allocation matching the flat reader. Previously the intermediate binding stayed
// `Array(MAX)` (tagged) while the `Int32[]`-typed consumer read flat → garbage.
// ---------------------------------------------------------------------------

#[test]
fn test_generic_map_intermediate_alloc_int32_is_flat_and_correct() {
    // The full map-shape combinator: declared `U[]`, an intermediate
    // `val result = lin_array_allocate(n)`, written in a for-loop, returned bare.
    // Monomorphized at U=Int32 it must produce a FLAT array read flat. Before the
    // fix this printed garbage (a tagged producer read through the flat i32 accessor).
    let out = run(r#"import { length } from "std/array"
import { for as afor } from "std/iter"
import { print } from "std/io"
import { toString } from "std/string"
val mymap = <T, U>(arr: T[], f: (T) => U): U[] =>
  val n = length(arr)
  val result = lin_array_allocate(n)
  var i = 0
  arr.afor(x =>
    result[i] = f(x)
    i = i + 1
  )
  result
val doubled: Int32[] = mymap([10, 20, 30], x => x * 2)
print(toString(doubled[0]))
print(toString(doubled[1]))
print(toString(doubled[2]))
"#);
    assert_eq!(out, vec!["20", "40", "60"]);
}

#[test]
fn test_generic_map_intermediate_alloc_flat_path_in_ir() {
    // IR proof: the U=Int32 monomorph allocates FLAT (lin_flat_array_alloc*) for the
    // intermediate binding and the consumer reads with the FLAT getter — producer and
    // consumer agree, no tagged allocator (lin_array_alloc_null) on this monomorph.
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let src_path = ws.join(format!("target/lin_test_imap_{}.lin", id));
    let bin_path = ws.join(format!("target/lin_test_imap_{}", id));
    let ll_path = bin_path.with_extension("ll");

    fs::write(&src_path, r#"import { length } from "std/array"
import { for as afor } from "std/iter"
import { print } from "std/io"
import { toString } from "std/string"
val mymap = <T, U>(arr: T[], f: (T) => U): U[] =>
  val n = length(arr)
  val result = lin_array_allocate(n)
  var i = 0
  arr.afor(x =>
    result[i] = f(x)
    i = i + 1
  )
  result
val doubled: Int32[] = mymap([10, 20, 30], x => x * 2)
print(toString(doubled[0]))
"#).unwrap();

    let compile = lin_cmd()
        .args(["build", src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .env("LIN_EMIT_IR", "1")
        .env("LIN_NO_OPT", "1")
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    let _ = fs::remove_file(&src_path);
    assert!(compile.status.success(), "compilation failed:\n{}",
        String::from_utf8_lossy(&compile.stderr));

    let ll = fs::read_to_string(&ll_path).expect("LLVM IR not emitted");
    let _ = fs::remove_file(&bin_path);
    let _ = fs::remove_file(&ll_path);

    let body_start = ll.find("define ptr @\"mymap$Int32_Int32\"").expect("missing mymap$Int32_Int32 monomorph");
    let body = &ll[body_start..];
    let body_end = body.find("\n}").map(|e| e + 2).unwrap_or(body.len());
    let body = &body[..body_end];
    assert!(body.contains("lin_flat_array_alloc"),
        "mymap$Int32_Int32 must allocate a flat array, got:\n{}", body);
    assert!(!body.contains("lin_array_alloc_null"),
        "mymap$Int32_Int32 must NOT allocate a tagged array, got:\n{}", body);
    // The consumer reads the Int32[] result with the flat getter.
    assert!(ll.contains("lin_flat_array_get_i32"),
        "expected a flat i32 read of the Int32[] value, IR:\n{}", ll);
}

#[test]
fn test_generic_map_intermediate_alloc_string_stays_tagged() {
    // The SAME generic map-shape combinator instantiated at U=String (heap element):
    // must stay TAGGED and correct. Proves the intermediate-alloc refinement is gated
    // strictly to flat scalars and never corrupts a heap-element result.
    let out = run(r#"import { length } from "std/array"
import { for as afor } from "std/iter"
import { print } from "std/io"
val mymap = <T, U>(arr: T[], f: (T) => U): U[] =>
  val n = length(arr)
  val result = lin_array_allocate(n)
  var i = 0
  arr.afor(x =>
    result[i] = f(x)
    i = i + 1
  )
  result
val tagged: String[] = mymap(["a", "b"], s => "${s}!")
print(tagged[0])
print(tagged[1])
"#);
    assert_eq!(out, vec!["a!", "b!"]);
}

#[test]
fn test_generic_map_intermediate_alloc_mixed_instantiations() {
    // The SAME generic instantiated at BOTH Int32 (flat) and String (tagged) in one
    // program — each monomorph picks its own representation; both must be correct.
    let out = run(r#"import { length } from "std/array"
import { for as afor } from "std/iter"
import { print } from "std/io"
import { toString } from "std/string"
val mymap = <T, U>(arr: T[], f: (T) => U): U[] =>
  val n = length(arr)
  val result = lin_array_allocate(n)
  var i = 0
  arr.afor(x =>
    result[i] = f(x)
    i = i + 1
  )
  result
val ints: Int32[] = mymap([1, 2, 3], x => x * 10)
val strs: String[] = mymap(["a", "b"], s => "${s}!")
print(toString(ints[0]))
print(toString(ints[2]))
print(strs[0])
print(strs[1])
"#);
    assert_eq!(out, vec!["10", "30", "a!", "b!"]);
}

#[test]
fn test_generic_map_intermediate_alloc_json_stays_tagged() {
    // A AnyVal (wildcard) instantiation of the same combinator stays TAGGED and correct —
    // the heterogeneous element representation is preserved.
    let out = run(r#"import { length } from "std/array"
import { for as afor } from "std/iter"
import { print } from "std/io"
import { toString } from "std/string"
val mymap = <T, U>(arr: T[], f: (T) => U): U[] =>
  val n = length(arr)
  val result = lin_array_allocate(n)
  var i = 0
  arr.afor(x =>
    result[i] = f(x)
    i = i + 1
  )
  result
val xs: AnyVal[] = [1, "two", true]
val ys: AnyVal[] = mymap(xs, (x: AnyVal): AnyVal => x)
print(toString(length(ys)))
print(toString(ys[0]))
print(toString(ys[1]))
"#);
    assert_eq!(out, vec!["3", "1", "two"]);
}

#[test]
fn test_intermediate_alloc_user_annotation_is_respected() {
    // A user-annotated intermediate binding (`val result: AnyVal[] = lin_array_allocate(n)`)
    // must NOT be re-pinned by the refinement — the explicit annotation wins, so the
    // binding stays tagged and the program is correct under the tagged accessor it uses.
    // Guards the `type_ann.is_some()` bail in intermediate_array_allocate_binding.
    let out = run(r#"import { length } from "std/array"
import { for as afor } from "std/iter"
import { print } from "std/io"
import { toString } from "std/string"
val mymap = <T>(arr: T[]): AnyVal[] =>
  val n = length(arr)
  val result: AnyVal[] = lin_array_allocate(n)
  var i = 0
  arr.afor(x =>
    result[i] = x
    i = i + 1
  )
  result
val ys: AnyVal[] = mymap([7, 8, 9])
print(toString(length(ys)))
print(toString(ys[0]))
"#);
    assert_eq!(out, vec!["3", "7"]);
}

// ---------------------------------------------------------------------------
// Phase 3.5: hardening single-module generics (nested calls, aliasing, budget,
// type-param hygiene, uninferrable type parameters).
// ---------------------------------------------------------------------------

#[test]
fn test_generic_nested_call_remonomorphized() {
    // BUG 1: a generic function whose body calls ANOTHER generic must re-monomorphize the inner
    // call under the composed substitution. `wrap$Int32` must call the native `id$Int32`, not a
    // half-generic copy. Previously printed garbage.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val id = <T>(x: T): T => x
val wrap = <U>(y: U): U => id(y)
print(toString(wrap(42)))
"#);
    assert_eq!(out, vec!["42"]);
}

#[test]
fn test_generic_nested_call_is_native_in_ir() {
    // IR proof for BUG 1: wrap$Int32 calls id$Int32 (both native i32), with no half-generic
    // `id$T...` copy and no `lin_box_int32(ptr null)`.
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let src_path = ws.join(format!("target/lin_test_gen_{}.lin", id));
    let bin_path = ws.join(format!("target/lin_test_gen_{}", id));
    let ll_path = bin_path.with_extension("ll");

    fs::write(&src_path, r#"import { print } from "std/io"
import { toString } from "std/string"
val id = <T>(x: T): T => x
val wrap = <U>(y: U): U => id(y)
print(toString(wrap(42)))
"#).unwrap();

    let compile = lin_cmd()
        .args(["build", src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .env("LIN_EMIT_IR", "1")
        .env("LIN_NO_OPT", "1")
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    let _ = fs::remove_file(&src_path);
    assert!(compile.status.success(), "compilation failed:\n{}",
        String::from_utf8_lossy(&compile.stderr));

    let ll = fs::read_to_string(&ll_path).expect("LLVM IR not emitted");
    let _ = fs::remove_file(&bin_path);
    let _ = fs::remove_file(&ll_path);

    assert!(ll.contains("define i32 @\"wrap$Int32\"(i32"),
        "expected an unboxed i32 wrap specialization, IR:\n{}", ll);
    assert!(ll.contains("define i32 @\"id$Int32\"(i32"),
        "expected an unboxed i32 id specialization, IR:\n{}", ll);
    // wrap$Int32 body must call id$Int32 directly (native).
    let body_start = ll.find("define i32 @\"wrap$Int32\"").unwrap();
    let body = &ll[body_start..];
    let body_end = body.find("\n}").map(|e| e + 2).unwrap_or(body.len());
    let body = &body[..body_end];
    assert!(body.contains("call i32 @\"id$Int32\""),
        "wrap$Int32 must call native id$Int32, got:\n{}", body);
    // No half-generic copy and no boxing of a null pointer.
    assert!(!ll.contains("id$T"),
        "no half-generic id$T... copy should exist, IR:\n{}", ll);
    assert!(!ll.contains("lin_box_int32(ptr null)"),
        "no lin_box_int32(ptr null) should appear, IR:\n{}", ll);
}

#[test]
fn test_generic_aliased_then_called() {
    // BUG 2: a generic bound to another val (`val f = id`) then called indirectly must
    // monomorphize, not crash codegen. Previously panicked in boxing.rs.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val id = <T>(x: T): T => x
val f = id
print(toString(f(5)))
"#);
    assert_eq!(out, vec!["5"]);
}

#[test]
fn test_generic_aliased_multiple_types() {
    // The alias resolves to the underlying generic at EACH call site independently.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val id = <T>(x: T): T => x
val f = id
print(toString(f(7)))
print(f("hi"))
"#);
    assert_eq!(out, vec!["7", "hi"]);
}

#[test]
fn test_generic_union_typed_arg_monomorphizes() {
    // Regression: a generic fn whose only use of a type parameter is inside a generic UNION-typed
    // argument type-checked fine but FAILED at monomorphization ("cannot infer a concrete type for
    // the type parameter(s) ... 'isOk'"). The monomorphizer's `collect_subs` did not recurse into
    // `Type::Union` members, so `T`/`E` (appearing only inside union arms) were left unbound. The
    // generic-record control case worked because it recursed into object fields.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Res<T, E> = { "type": "success", "value": T } | { "type": "failure", "error": E }
val isOk = <T, E>(r: Res<T, E>): Boolean =>
  r["type"] == "success"
val r: Res<Int32, String> = { "type": "success", "value": 5 }
print(r.isOk().toString())
"#);
    assert_eq!(out, vec!["true"]);
}

#[test]
fn test_generic_higher_order_passed_directly_still_works() {
    // Regression guard: a (non-generic) function passed directly as a callback argument and
    // applied inside the callee must keep working alongside the generic machinery.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val applyTwice = (g: (Int32) => Int32, x: Int32): Int32 => g(g(x))
val inc = (n: Int32): Int32 => n + 1
print(toString(applyTwice(inc, 5)))
"#);
    assert_eq!(out, vec!["7"]);
}

#[test]
fn test_generic_type_param_hygiene_outer_alias_survives() {
    // Type-param hygiene: a generic param `<T>` must not leak past the function body and clobber
    // an outer `type T = Int32` alias. `use: T` must still resolve to Int32 after `id`.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type T = Int32
val id = <T>(x: T): T => x
val use: T = 7
print(toString(id(3)))
print(toString(use))
"#);
    assert_eq!(out, vec!["3", "7"]);
}

#[test]
fn test_generic_nested_generics_no_param_leak() {
    // A generic whose body uses another generic, at multiple types — confirms nested generic
    // param bindings don't leak and both instantiations work.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val id = <T>(x: T): T => x
val twice = <U>(y: U): U => id(id(y))
print(toString(twice(10)))
print(twice("hi"))
"#);
    assert_eq!(out, vec!["10", "hi"]);
}

#[test]
fn test_generic_used_as_first_class_value_errors() {
    // A generic (or an alias of one) passed as a first-class value that escapes — here `f` is
    // handed to `apply` and called inside it — cannot be monomorphized. This must produce a clear
    // diagnostic, not the historical malformed IR / "Call parameter type does not match" crash.
    let err = run_expect_err(r#"import { print } from "std/io"
import { toString } from "std/string"
val id = <T>(x: T): T => x
val f = id
val apply = (g: (Int32) => Int32, x: Int32): Int32 => g(x)
print(toString(apply(f, 5)))
"#);
    assert!(err.contains("used as a first-class value"),
        "expected a first-class-value diagnostic, got:\n{}", err);
}

#[test]
fn test_generic_uninferrable_type_param_errors() {
    // A type parameter unconstrained by args/return must produce a clear diagnostic, not a
    // panic or silently-wrong code.
    let err = run_expect_err(r#"import { print } from "std/io"
import { toString } from "std/string"
val mk = <T>(): T => 0
print(toString(mk()))
"#);
    assert!(err.contains("cannot infer a concrete type for the type parameter"),
        "expected an uninferrable-type-parameter diagnostic, got:\n{}", err);
}

/// Build + run with a custom `LIN_SPEC_BUDGET`, returning the compile stderr (for the warning)
/// and the program's stdout lines.
fn run_with_spec_budget(source: &str, budget: &str) -> (String, Vec<String>) {
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let src_path = ws.join(format!("target/lin_test_budget_{}.lin", id));
    let bin_path = ws.join(format!("target/lin_test_budget_{}", id));
    fs::write(&src_path, source).unwrap();

    let compile = lin_cmd()
        .args(["build", src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .env("LIN_SPEC_BUDGET", budget)
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    let _ = fs::remove_file(&src_path);
    assert!(compile.status.success(), "compilation failed:\n{}",
        String::from_utf8_lossy(&compile.stderr));
    let stderr = String::from_utf8_lossy(&compile.stderr).to_string();

    let run_out = Command::new(&bin_path).output().expect("failed to run compiled binary");
    let _ = fs::remove_file(&bin_path);
    assert!(run_out.status.success(), "runtime error:\n{}",
        String::from_utf8_lossy(&run_out.stderr));
    let stdout = String::from_utf8_lossy(&run_out.stdout);
    let lines: Vec<String> = stdout.lines().filter(|l| !l.is_empty()).map(|l| l.to_string()).collect();
    (stderr, lines)
}

#[test]
fn test_generic_specialization_budget_falls_back_correctly() {
    // With the budget capped at 2, a third distinct instantiation overflows: it emits a warning
    // and falls back to a boxed/type-erased copy — but the program still produces correct output.
    let (stderr, out) = run_with_spec_budget(r#"import { print } from "std/io"
import { toString } from "std/string"
val id = <T>(x: T): T => x
print(toString(id(1)))
print(id("two"))
print(toString(id(true)))
"#, "2");
    assert!(stderr.contains("specialization budget"),
        "expected a budget-overflow warning, got stderr:\n{}", stderr);
    assert_eq!(out, vec!["1", "two", "true"]);
}

// ---------------------------------------------------------------------------
// Phase 4: cross-module generic instantiation (a generic defined in an IMPORTED
// module is specialized in the importing module — see lin-ir monomorphize
// `monomorphize_with_imports` + cross-module body re-homing).
// ---------------------------------------------------------------------------

#[test]
fn test_generic_cross_module_identity() {
    // Step A: a generic `id` defined in an imported user module is monomorphized at the call site
    // in the importer. T=Int32 and T=String both run natively from the same imported definition.
    let dir = std::env::temp_dir().join(format!("lin_xgen_id_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("helpers.lin"),
        "export val id = <T>(x: T): T => x\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ id }} from "{}/helpers"
print(toString(id(5)))
print(id("hi"))
"#, dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["5", "hi"]);
}

#[test]
fn test_generic_cross_module_identity_is_native_in_ir() {
    // IR proof for Step A: the imported generic specializes to a NATIVE i32 function `id$Int32`
    // in the importer, called with an unboxed i32 (no lin_box_int32 around the argument).
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let dir = ws.join(format!("target/lin_xgen_ir_{}", id));
    let _ = fs::create_dir_all(&dir);
    fs::write(dir.join("helpers.lin"), "export val id = <T>(x: T): T => x\n").unwrap();
    let src_path = dir.join("main.lin");
    let bin_path = dir.join("main");
    let ll_path = bin_path.with_extension("ll");
    fs::write(&src_path, format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ id }} from "{}/helpers"
print(toString(id(5)))
print(id("hi"))
"#, dir.to_str().unwrap())).unwrap();

    let compile = lin_cmd()
        .args(["build", src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .env("LIN_EMIT_IR", "1")
        .env("LIN_NO_OPT", "1")
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    assert!(compile.status.success(), "compilation failed:\n{}",
        String::from_utf8_lossy(&compile.stderr));
    let ll = fs::read_to_string(&ll_path).expect("LLVM IR not emitted");
    let _ = fs::remove_dir_all(&dir);

    assert!(ll.contains("define i32 @\"id$Int32\"(i32"),
        "expected a native i32 cross-module specialization, IR:\n{}", ll);
    assert!(ll.contains("call i32 @\"id$Int32\"(i32 5)"),
        "expected a native-i32 call to the cross-module Int32 specialization, IR:\n{}", ll);
}

#[test]
fn test_generic_cross_module_higher_order_map() {
    // Step B: a higher-order generic `mymap` defined in an imported module — with a Function-typed
    // param and a `for`/`push` loop body — specializes at Int32 in the importer and runs correctly.
    // Exercises cross-module re-homing of the body's sibling/intrinsic references AND the checker
    // change that lets the lambda body bind the generic return type `U`.
    let dir = std::env::temp_dir().join(format!("lin_xgen_map_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("helpers.lin"),
        "import { push } from \"std/array\"\n\
         import { for } from \"std/iter\"\n\
         export val mymap = <T, U>(arr: T[], f: (T) => U): U[] =>\n  \
           val result: U[] = []\n  \
           arr.for(item => push(result, f(item)))\n  \
           result\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ reduce }} from "std/iter"
import {{ mymap }} from "{}/helpers"
val doubled = mymap([1, 2, 3], x => x * 2)
print(toString(doubled.reduce(0, (acc, x) => acc + x)))
"#, dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["12"]);
}

#[test]
fn test_cross_module_generic_call_with_capturing_closure() {
    // Regression: an IMPORTED function that (a) calls a generic with a CONCRETE element type — so
    // the importer monomorphizes the import (e.g. `sort` → `sort$Object`) — AND (b) contains a
    // nested closure capturing one of its OWN locals must NOT mis-attribute that closure's captures
    // to itself. A failed speculative callback type-check (checking the callback against an
    // incomplete generic hint, then re-inferring hint-free) used to `?`-out of `infer_function`
    // between its push and its matching pop of the capture/scope stacks, leaking an unbalanced
    // frame; the enclosing exported function then popped it and inherited a phantom capture set,
    // gaining a spurious closure-env parameter. The importer's direct call (no env) then mismatched
    // its arity → codegen "Incorrect number of arguments passed to called function!". Fixed by
    // rolling back the transient checker state on the discarded speculative path.
    let dir = std::env::temp_dir().join(format!("lin_xgen_capclo_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("types.lin"),
        "export type Item = { \"id\": String, \"rank\": Int32 }\n\
         export type Bag = { \"n\": Int32, \"by\": { String: Item[] } }\n").unwrap();
    std::fs::write(dir.join("lib.lin"),
        "import { length, sort } from \"std/array\"\n\
         import { for } from \"std/iter\"\n\
         import { Item, Bag } from \"./types\"\n\
         export val build = (items: Item[]): Bag =>\n  \
           var by: { String: Item[] } = {}\n  \
           val sorted: Item[] = sort(items, (a, b) => a[\"rank\"] - b[\"rank\"])\n  \
           sorted.for(it => by[\"all\"] = [it])\n  \
           { \"n\": length(sorted), \"by\": by }\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ build }} from "{}/lib"
val main = (): Null =>
  var items: AnyVal = [{{ "id": "b", "rank": 2 }}, {{ "id": "a", "rank": 1 }}]
  val bag = build(items)
  print(toString(bag["n"]))
main()
"#, dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["2"]);
}

#[test]
fn test_generic_cross_module_two_instantiations() {
    // Cache/specialization correctness: the SAME imported generic instantiated at two different
    // element types from one importer mints two distinct specializations, each correct.
    let dir = std::env::temp_dir().join(format!("lin_xgen_two_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("helpers.lin"),
        "import { push } from \"std/array\"\n\
         import { for } from \"std/iter\"\n\
         export val mymap = <T, U>(arr: T[], f: (T) => U): U[] =>\n  \
           val result: U[] = []\n  \
           arr.for(item => push(result, f(item)))\n  \
           result\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ length }} from "std/array"
import {{ reduce }} from "std/iter"
import {{ mymap }} from "{}/helpers"
val ints = mymap([1, 2, 3], x => x * 10)
val strs = mymap(["a", "b"], s => s)
print(toString(ints.reduce(0, (acc, x) => acc + x)))
print(toString(length(strs)))
"#, dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["60", "2"]);
}

#[test]
fn test_generic_t_array_param_with_json_arg_is_correct() {
    // GAP 1: a generic `T[]` param unified against a `AnyVal` value binds `T = AnyVal` (the wildcard),
    // monomorphizing to a TAGGED `$AnyVal` instance — NOT leaving `T` unbound (which previously read
    // the array at a bogus element type → null/garbage). The SAME generic applied to a concrete
    // `Int32[]` still specializes to the flat `$Int32` instance. Both must produce correct values.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val firstOf = <T>(arr: T[]): T => arr[0]
val j: AnyVal = [7, 8, 9]
print(toString(firstOf(j)))
val ints: Int32[] = [10, 20, 30]
print(toString(firstOf(ints)))
"#);
    // AnyVal arg → 7 (correct, not null/garbage); Int32 arg → 10 (correct, flat).
    assert_eq!(out, vec!["7", "10"]);
}

#[test]
fn test_generic_t_array_param_json_tagged_int32_flat_in_ir() {
    // IR proof for GAP 1: the AnyVal instantiation mints a TAGGED `firstOf$AnyVal` monomorph (reads via
    // the tagged getter), while the Int32 instantiation mints a FLAT `firstOf$Int32` monomorph
    // (reads via lin_flat_array_get_i32, returns a native i32). No garbage `$T<id>` symbol.
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let src_path = ws.join(format!("target/lin_test_gap1_{}.lin", id));
    let bin_path = ws.join(format!("target/lin_test_gap1_{}", id));
    let ll_path = bin_path.with_extension("ll");

    fs::write(&src_path, r#"import { print } from "std/io"
import { toString } from "std/string"
val firstOf = <T>(arr: T[]): T => arr[0]
val j: AnyVal = [7, 8, 9]
print(toString(firstOf(j)))
val ints: Int32[] = [10, 20, 30]
print(toString(firstOf(ints)))
"#).unwrap();

    let compile = lin_cmd()
        .args(["build", src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .env("LIN_EMIT_IR", "1")
        .env("LIN_NO_OPT", "1")
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    let _ = fs::remove_file(&src_path);
    assert!(compile.status.success(), "compilation failed:\n{}",
        String::from_utf8_lossy(&compile.stderr));

    let ll = fs::read_to_string(&ll_path).expect("LLVM IR not emitted");
    let _ = fs::remove_file(&bin_path);
    let _ = fs::remove_file(&ll_path);

    // The AnyVal instantiation is named `$AnyVal` (tagged), the Int32 one `$Int32` (flat).
    assert!(ll.contains("\"firstOf$AnyVal\""),
        "expected a tagged firstOf$AnyVal monomorph for the AnyVal arg, IR:\n{}", ll);
    assert!(ll.contains("define i32 @\"firstOf$Int32\"(ptr"),
        "expected a flat i32 firstOf$Int32 monomorph for the Int32[] arg, IR:\n{}", ll);
    // Soundness guard: never an unbound-TypeVar `$T<id>` garbage monomorph.
    let re = regex_lite_find_t_id(&ll);
    assert!(re.is_none(),
        "found a garbage unbound-TypeVar monomorph '{}' — GAP 2 regression, IR:\n{}",
        re.unwrap(), ll);
}

#[test]
fn test_generic_import_path_unbound_typevar_is_safe() {
    // GAP 2 (LATENT SOUNDNESS BUG): a generic called INSIDE an imported module on that module's own
    // `AnyVal` param previously emitted a `$T<id>` garbage monomorph keyed on the UNBOUND TypeVar,
    // which read/allocated the array at a bogus element type → runtime `capacity overflow` / heap
    // corruption. The import-monomorphization path must now erase any non-concrete TypeVar to the
    // AnyVal wildcard, producing a correct tagged `$AnyVal` monomorph (the same resolution the main
    // module uses). Module `helpers` exports `doubleAll(arr: AnyVal)` whose body calls the sibling
    // generic `mymap` on its AnyVal param — exactly the import-path-unbound case.
    let dir = std::env::temp_dir().join(format!("lin_gap2_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("helpers.lin"),
        "import { push } from \"std/array\"\n\
         import { for } from \"std/iter\"\n\
         export val mymap = <T, U>(arr: T[], f: (T) => U): U[] =>\n  \
           val result: U[] = []\n  \
           arr.for(item => push(result, f(item)))\n  \
           result\n\
         export val doubleAll = (arr: AnyVal): AnyVal =>\n  \
           mymap(arr, x => x * 2)\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ reduce }} from "std/iter"
import {{ doubleAll }} from "{}/helpers"
val r: AnyVal = doubleAll([5, 6, 7])
print(toString(r.reduce(0, (acc, x) => acc + x)))
"#, dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    // 5+6+7 = 18, doubled = 36. Correct tagged result, no crash, no garbage.
    assert_eq!(output, vec!["36"]);
}

#[test]
fn test_generic_import_path_unbound_typevar_no_garbage_monomorph_in_ir() {
    // IR proof for GAP 2: the import-path `mymap` instantiation driven by `doubleAll`'s AnyVal param
    // mints a tagged `mymap$Json_...` monomorph and NEVER a `$T<id>` garbage symbol.
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let dir = ws.join(format!("target/lin_gap2_ir_{}", id));
    let _ = fs::create_dir_all(&dir);
    fs::write(dir.join("helpers.lin"),
        "import { push } from \"std/array\"\n\
         import { for } from \"std/iter\"\n\
         export val mymap = <T, U>(arr: T[], f: (T) => U): U[] =>\n  \
           val result: U[] = []\n  \
           arr.for(item => push(result, f(item)))\n  \
           result\n\
         export val doubleAll = (arr: AnyVal): AnyVal =>\n  \
           mymap(arr, x => x * 2)\n").unwrap();
    let src_path = dir.join("main.lin");
    let bin_path = dir.join("main");
    let ll_path = bin_path.with_extension("ll");
    fs::write(&src_path, format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ reduce }} from "std/iter"
import {{ doubleAll }} from "{}/helpers"
val r: AnyVal = doubleAll([5, 6, 7])
print(toString(r.reduce(0, (acc, x) => acc + x)))
"#, dir.to_str().unwrap())).unwrap();

    let compile = lin_cmd()
        .args(["build", src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .env("LIN_EMIT_IR", "1")
        .env("LIN_NO_OPT", "1")
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    assert!(compile.status.success(), "compilation failed:\n{}",
        String::from_utf8_lossy(&compile.stderr));
    let ll = fs::read_to_string(&ll_path).expect("LLVM IR not emitted");
    let _ = fs::remove_dir_all(&dir);

    let garbage = regex_lite_find_t_id(&ll);
    assert!(garbage.is_none(),
        "import-path monomorphization emitted a garbage unbound-TypeVar monomorph '{}' (GAP 2), IR:\n{}",
        garbage.unwrap(), ll);
}

#[test]
fn test_stdlib_generic_accessors_at_set_indexof() {
    // ADR-044: stdlib `at`/`set`/`indexOf` carry generic `<T>(T[], …)` signatures. They must stay
    // representation-consistent and correct on both a flat concrete-scalar `Int32[]` and a tagged
    // `String[]`, including negative-index wrap and the in-place `set` round-trip.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { at, set, indexOf } from "std/array"
val a = [10, 20, 30]
print(toString(a.at(1)))
print(toString(a.at(-1)))
set(a, 0, 99)
print(toString(a.at(0)))
print(toString(a.indexOf(30)))
val s = ["x", "y", "z"]
print(s.at(-1))
print(toString(s.indexOf("y")))
"#);
    assert_eq!(out, vec!["20", "30", "99", "2", "z", "1"]);
}

/// Find the first `$T<digits>` token in `ir` (a garbage unbound-TypeVar monomorph name). Returns
/// the matched substring, or `None`. Deliberately dependency-free (no `regex` crate in this test
/// binary): scan for the `$T` marker followed by ASCII digits.
fn regex_lite_find_t_id(ir: &str) -> Option<String> {
    let bytes = ir.as_bytes();
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == b'$' && bytes[i + 1] == b'T' && bytes[i + 2].is_ascii_digit() {
            let start = i;
            let mut j = i + 2;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            return Some(ir[start..j].to_string());
        }
        i += 1;
    }
    None
}

#[test]
fn test_map_callback_returns_curried_closure_full_apply() {
    // ADR-044: a `map` callback that RETURNS a closure (curried `i => () => i`) is a FULL
    // application of the 1-arg callback, not under-application — the indirect-call path must CALL it
    // (returning the thunk), not bundle it into a partial-application closure. Before the arg-count
    // vs arity disambiguation it returned garbage (a pointer reinterpreted as the value).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map } from "std/iter"
val thunks = map([5, 6, 7], i => () => i)
print(toString(thunks[0]()))
print(toString(thunks[1]()))
print(toString(thunks[2]()))
"#);
    assert_eq!(out, vec!["5", "6", "7"]);
}

#[test]
fn test_reduce_over_push_built_flat_typed_array_reads_correctly() {
    // ADR-044: a `[]`+push builder typed `Int32[]` allocates a TAGGED array; `reduce` over it must
    // read at the runtime representation (tagged), not flat — a flat read would misread garbage.
    // `combinator_read_elem_ty` only flat-reads provably-flat producers; a `[]`+push source falls
    // back to the tagged read.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { push } from "std/array"
import { reduce } from "std/iter"
val build = (): Int32[] =>
  val result: Int32[] = []
  push(result, 5)
  push(result, 6)
  push(result, 7)
  result
print(toString(reduce(build(), 0, (a, x) => a + x)))
"#);
    assert_eq!(out, vec!["18"]);
}

#[test]
fn test_filter_then_reduce_flat_pipeline_correct() {
    // ADR-044: filter's keep/skip block split exercises the `emit_index_loop` phi back-edge patch
    // (the back-edge predecessor is the skip block, not the nominal body block). A range→filter→
    // reduce flat pipeline must produce the right sum and valid IR.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range, filter, reduce } from "std/iter"
val total = range(0, 10).filter(x => x % 2 == 0).reduce(0, (acc, x) => acc + x)
print(toString(total))
"#);
    assert_eq!(out, vec!["20"]);
}

#[test]
fn test_filter_object_array_no_double_free() {
    // ADR-044 R2 regression: `filter` over an array of OBJECTS pushes each kept element (BORROWED
    // from the source array) into the result array. The tagged push (`lin_array_push_tagged`) MOVES
    // the TaggedVal without bumping the inner refcount, so the kept element must be RETAINED first —
    // otherwise both the source and the filtered array reference the same object at refcount 1 and
    // releasing both double-frees it (heap-use-after-free at teardown — caught by ASan, manifested
    // as the `examples/codec/bits.test.lin` etc. segfault). The source must also stay intact and
    // usable after the filter. Exercised both as a freshly-built source and re-read afterwards.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"
import { filter } from "std/iter"
type Item = { "type": String, "v": Int32 }
val items: Item[] = [
  { "type": "a", "v": 1 },
  { "type": "b", "v": 2 },
  { "type": "a", "v": 3 }
]
val kept = items.filter(i => i["type"] == "a")
print(toString(length(kept)))
print(toString(kept[0]["v"]))
print(toString(kept[1]["v"]))
print(toString(length(items)))
print(toString(items[1]["type"]))
"#);
    assert_eq!(out, vec!["2", "1", "3", "3", "b"]);
}

#[test]
fn test_combinator_over_non_array_json_is_safe_noop() {
    // Regression: a `for`/`filter`/`map`/`reduce` over a statically-`AnyVal` value whose RUNTIME value
    // is NOT an array (here an Object) must NOT misread the non-array payload as a `LinArray`.
    //
    // The combinator loop used `lin_length_dyn` for its bound (which reports an Object's KEY COUNT)
    // and then blindly unboxed the AnyVal pointer and read it through `lin_array_get_tagged` — so for
    // a 2-key object it ran 2 iterations, dereferencing the `LinObject` as a `LinArray` (UB:
    // "misaligned pointer dereference: address must be a multiple of 0x4 but is 0x41" — a string byte
    // read as an i32 flat-array buffer). This was the docs-builder crash: an `ls()` error object
    // (`{ "type": "error", ... }`) flowed into `allFiles.filter(...)` because the builder's guard
    // checked for "failure" not "error". The fix bounds the combinator loop with `lin_iterable_length`
    // (array length, else 0), so iterating a non-array AnyVal is a clean no-op and the result is empty.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"
import { filter, map, reduce, for } from "std/iter"
import { contains } from "std/string"
val mkObj = (): AnyVal => { "type": "error", "message": "boom" }
val v = mkObj()
val kept = v.filter(x => contains(x, "a"))
print(toString(length(kept)))
val mapped = v.map(x => x)
print(toString(length(mapped)))
val total = v.reduce(0, (acc, x) => acc + 1)
print(toString(total))
var n = 0
v.for(x => n = n + 1)
print(toString(n))
"#);
    assert_eq!(out, vec!["0", "0", "0", "0"]);
}

// ADR-046: `replace` is a TEST-ONLY mock. In a normal `lin build` program (this harness writes a
// `.lin`, not a `.test.lin`) it must be a hard compile error — a shipped binary must never silently
// swap a real import. The positive cases (mocking user modules + stdlib, internal call-sites seeing
// the mock, spies, val mocks, type-drift rejection) are exercised end-to-end by `lin test` over
// `crates/lin/tests/replace_mocking/*.test.lin` and under ASan in the CI example-suite leg.
#[test]
fn test_replace_rejected_in_non_test_program() {
    let err = run_expect_err(
        r#"import { print } from "std/io"
import { readFile } from "std/fs"
replace readFile = (path: String): AnyVal => "mock"
print(readFile("x"))
"#,
    );
    assert!(
        err.contains("`replace` is only allowed in a `*.test.lin`"),
        "expected test-only rejection, got:\n{}",
        err
    );
}

// -----------------------------------------------------------------------------
// `lin check` resolves imports (regression: it previously type-checked the bare
// parsed module without loading imports, so any error that depended on an
// imported symbol's real type was silently accepted — `check` passed programs
// that `build` correctly rejected).
// -----------------------------------------------------------------------------

/// Run `lin check <file>` on `source`. Returns (success, combined stderr+stdout).
fn check_source(source: &str) -> (bool, String) {
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let src_path = ws.join(format!("target/lin_check_{}.lin", id));
    fs::write(&src_path, source).unwrap();

    let out = lin_cmd()
        .args(["check", src_path.to_str().unwrap()])
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");

    let _ = fs::remove_file(&src_path);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    (out.status.success(), combined)
}

/// Run `lin build <file>` on `source` (no run). Returns whether it compiled.
fn build_succeeds(source: &str) -> bool {
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let src_path = ws.join(format!("target/lin_build_only_{}.lin", id));
    let bin_path = ws.join(format!("target/lin_build_only_{}", id));
    fs::write(&src_path, source).unwrap();

    let out = lin_cmd()
        .args(["build", src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary");

    let _ = fs::remove_file(&src_path);
    let _ = fs::remove_file(&bin_path);
    out.status.success()
}

#[test]
fn test_check_rejects_import_dependent_type_error() {
    // `trim` is imported as `(s: String): String`; calling it with an Int32 is a type error
    // that is only visible once the import has been resolved.
    let (ok, output) = check_source(
        r#"import { trim } from "std/string"
val x = trim(42)
"#,
    );
    assert!(
        !ok,
        "expected `lin check` to reject trim(42), but it passed:\n{}",
        output
    );
    assert!(
        output.contains("expected String"),
        "expected an argument-type error, got:\n{}",
        output
    );
}

#[test]
fn test_index_sig_map_key_string_alias_checks() {
    // A type alias that resolves to `String` is a valid index-signature (map) key. The literal
    // `String` key form must keep working too.
    let (ok, output) = check_source(
        r#"type StopID = String
type Stops = { StopID: { String: UInt8 } }
val s: Stops = {}
"#,
    );
    assert!(
        ok,
        "expected a String-alias map key to type-check, but it failed:\n{}",
        output
    );
}

#[test]
fn test_index_sig_map_key_non_string_alias_rejected() {
    // An index-signature key whose alias resolves to an Integer type is now accepted
    // (numeric-key maps feature). Float aliases are still rejected.
    let (ok, _output) = check_source(
        r#"type Bad = Int32
val m: { Bad: Int32 } = {}
"#,
    );
    assert!(
        ok,
        "expected an Int32 alias key to be accepted (numeric-key map), but it was rejected"
    );
    // A Float alias key is still rejected.
    let (ok2, output2) = check_source(
        r#"type Bad = Float64
val m: { Bad: Int32 } = {}
"#,
    );
    assert!(
        !ok2,
        "expected a Float64 map key alias to be rejected, but it passed:\n{}",
        output2
    );
    assert!(
        output2.contains("Index-signature key type must be String"),
        "expected the index-sig key error, got:\n{}",
        output2
    );
}

#[test]
fn test_int_map_basic_operations() {
    // Smoke test for { Int32: String } maps: insert, read hit, read miss, overwrite.
    let output = run(r#"import { print } from "std/io"

var m: { Int32: String } = {}
m[0] = "zero"
m[-1] = "neg"
m[42] = "forty-two"
m[1000000] = "million"
print(m[0] ?? "null")
print(m[-1] ?? "null")
print(m[42] ?? "null")
print(m[1000000] ?? "null")
print(m[7] ?? "null")
m[42] = "overwritten"
print(m[42] ?? "null")
"#);
    assert_eq!(output, vec![
        "zero",
        "neg",
        "forty-two",
        "million",
        "null",
        "overwritten",
    ]);
}

#[test]
fn test_int_map_key_zero_stores_correctly() {
    // Key 0 is the tricky case: the occupancy rule is hash==0 (not key==0), so key 0 must
    // be stored using fmix64(0) != 0 as the hash. Verifies it survives a round-trip.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

var m: { Int32: Int32 } = {}
m[0] = 99
print(toString(m[0] ?? -1))
print(toString(m[1] ?? -1))
"#);
    assert_eq!(output, vec!["99", "-1"]);
}

#[test]
fn test_int_map_wrong_key_type_rejected() {
    // Using a String key on an Int-keyed map must be a type error.
    let err = run_expect_err(r#"import { print } from "std/io"
var m: { Int32: String } = {}
m["hello"] = "world"
"#);
    assert!(
        err.contains("keyed by") || err.contains("Int32") || err.contains("String"),
        "expected Int-map string-key type error, got: {err}"
    );
}

// ── Integer-keyed map LITERALS (§5.1.1) ──────────────────────────────────────────────────────────

#[test]
fn test_int_map_literal_basic() {
    // Basic int-map literal: { 1: "one", 2: "two", 42: "forty-two" }
    let output = run(r#"import { print } from "std/io"

val m: { Int32: String } = { 1: "one", 2: "two", 42: "forty-two" }
print(m[1] ?? "?")
print(m[2] ?? "?")
print(m[42] ?? "?")
print(m[99] ?? "?")
"#);
    assert_eq!(output, vec!["one", "two", "forty-two", "?"]);
}

#[test]
fn test_int_map_literal_negative_keys() {
    // Negative integer literal keys in an int-map literal.
    let output = run(r#"import { print } from "std/io"

val m: { Int32: String } = { -1: "minus-one", 0: "zero", -99: "neg-ninety-nine" }
print(m[0] ?? "?")
print(m[-1] ?? "?")
print(m[-99] ?? "?")
print(m[1] ?? "?")
"#);
    assert_eq!(output, vec!["zero", "minus-one", "neg-ninety-nine", "?"]);
}

#[test]
fn test_int_map_literal_inferred_type() {
    // Without annotation: infers { Int32: String } from the int literal keys.
    let output = run(r#"import { print } from "std/io"

val n = { 0: "false", 1: "true" }
print(n[0] ?? "?")
print(n[1] ?? "?")
print(n[2] ?? "?")
"#);
    assert_eq!(output, vec!["false", "true", "?"]);
}

#[test]
fn test_int_map_literal_mixed_keys_rejected() {
    // Mixing integer and string keys in the same literal is a type error.
    let err = run_expect_err(r#"import { print } from "std/io"

val m = { 1: "a", "b": "c" }
print(m[1] ?? "?")
"#);
    assert!(
        err.contains("mixed") || err.contains("integer") || err.contains("string"),
        "expected mixed-key type error, got: {err}"
    );
}

#[test]
fn test_nested_int_keyed_map_literal_roundtrip() {
    // Regression: an int-keyed map nested as the VALUE of an outer map was read back as null.
    // Root cause: `o["B"]` yields `{ UInt8: Int32 } | Null` (union), and when the inner `[1]`
    // index was compiled the codegen fell through to the `is_array_access` check (because
    // `key_ty.is_numeric()` is true), calling `lin_array_get_tagged` on the unboxed LinMap*
    // instead of `lin_map_get_int`. Both the literal-built and write-then-read variants are
    // exercised here.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

// Literal-built nested int-keyed map: o["B"][1] must return 77.
type Outer = { String: { UInt8: Int32 } }
val o: Outer = { "B": { 1: 77 } }
print(toString(o["B"][1] ?? -1))

// Write-then-read: o2["B"][1] = 99 then read back must return 99.
val o2: Outer = { "B": {} }
o2["B"][1] = 99
print(toString(o2["B"][1] ?? -1))

// Missing key still returns null: o["B"][99] is absent.
print(toString(o["B"][99] ?? -1))
"#);
    assert_eq!(output, vec!["77", "99", "-1"]);
}

#[test]
fn test_nested_map_write_auto_vivifies_intermediate_levels() {
    // Auto-vivification: a nested write `m[k1][k2] = v` creates absent intermediate MAP levels
    // (an empty map of the static value type, stored back) so the write succeeds, instead of the
    // previous silent no-op. Deep nesting vivifies every level; reads still null-propagate WITHOUT
    // mutating the intermediate.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

// Single-level: kConn["StopB"] is absent; the write must create it and persist.
type Conn = { String: String }
type CIdx = { String: { UInt8: Conn } }
val kConn: CIdx = {}
val c: Conn = { "x": "y" }
kConn["StopB"][1] = c
print(if kConn["StopB"][1] == null then "LOST" else "stored")

// Deep (3-level): o["a"] and o["a"]["b"] are both auto-created.
type T = { String: { String: { String: Int32 } } }
val o: T = {}
o["a"]["b"]["c"] = 5
print(toString(o["a"]["b"]["c"] ?? -1))

// A read through an absent intermediate must NOT vivify it.
type R = { String: { String: Int32 } }
val r: R = {}
val got = r["a"]["b"]
print(if got == null then "read-null" else "read-val")
print(if r["a"] == null then "still-absent" else "MUTATED")
"#);
    assert_eq!(output, vec!["stored", "5", "read-null", "still-absent"]);
}

#[test]
fn test_check_accepts_valid_imported_symbol_program() {
    let (ok, output) = check_source(
        r#"import { trim } from "std/string"
import { print } from "std/io"
val x = trim("  hi  ")
print(x)
"#,
    );
    assert!(
        ok,
        "expected `lin check` to accept a valid imported-symbol program, got:\n{}",
        output
    );
    assert!(
        output.contains("Type check passed"),
        "expected success message, got:\n{}",
        output
    );
}

#[test]
fn test_foreign_decl_scalar_union_return_is_callable() {
    // Regression: a function TYPE with a union return (`(A) => B | C`) parsed the return with
    // single-leaf precedence, so `(AnyVal) => Int64 | Error` became `((AnyVal) => Int64) | Error` —
    // a non-callable union. The foreign val was then typed as that union and any call failed with
    // "Cannot call non-function type (?T) => Int64 | { ... }". `=>` is the lowest-precedence type
    // operator, so the return must bind the whole union. This blocked `std/bignum`/`std/decimal`
    // whose `lin_*_to_int64` intrinsics are declared `(BigInt) => Int64 | Error`.
    //
    // Trigger confirmed: ANY function-type union return, scalar arm (`Int64 | Error`,
    // `Int64 | Null`, `Float64 | Error`) or otherwise, in a `foreign` decl OR a normal `type`
    // alias / function annotation. Foreign scalar-union was simply the only place it surfaced,
    // since other stdlib intrinsics return `AnyVal` (no union) and wrap to `T | Error` in Lin.
    let (ok, output) = check_source(
        r#"import foreign "lin-runtime"
  val lin_demo_to_int: (AnyVal) => Int64 | Error

export val toIntDemo = (x: AnyVal): Int64 | Error => lin_demo_to_int(x)

val r = toIntDemo(5)
val out = match (r)
  is Error => 0
  else => r + 1
"#,
    );
    assert!(
        ok,
        "expected a foreign decl with a scalar-union return `(AnyVal) => Int64 | Error` to type-check \
         and be callable (result narrowing under `is Error`), got:\n{}",
        output
    );

    // Other union-return shapes must also check (scalar | Null, float | Error, multi-arm, and a
    // non-foreign `type` alias — same parse site).
    for src in [
        "import foreign \"lin-runtime\"\n  val f: (AnyVal) => Int64 | Null\nexport val g = (x: AnyVal): Int64 | Null => f(x)\n",
        "import foreign \"lin-runtime\"\n  val f: (AnyVal) => Float64 | Error\nexport val g = (x: AnyVal): Float64 | Error => f(x)\n",
        "import foreign \"lin-runtime\"\n  val f: (AnyVal) => Int64 | Null | Error\nexport val g = (x: AnyVal): Int64 | Null | Error => f(x)\n",
        "type Fn = (AnyVal) => Int64 | Error\nexport val g = (f: Fn, x: AnyVal): Int64 | Error => f(x)\n",
    ] {
        let (ok, output) = check_source(src);
        assert!(ok, "expected union-return decl to type-check:\n{}\n---\n{}", src, output);
    }

    // Regression guard: a plain (non-union) scalar foreign return must still check.
    let (ok, output) = check_source(
        "import foreign \"lin-runtime\"\n  val f: (AnyVal) => Int64\nexport val g = (x: AnyVal): Int64 => f(x)\n",
    );
    assert!(ok, "plain scalar foreign return regressed:\n{}", output);
}

#[test]
fn test_check_and_build_agree_on_import_dependent_case() {
    // The bad program: both `check` and `build` must reject it.
    let bad = r#"import { trim } from "std/string"
val x = trim(42)
"#;
    let (check_ok, _) = check_source(bad);
    let build_ok = build_succeeds(bad);
    assert!(!check_ok, "check should reject the bad program");
    assert!(!build_ok, "build should reject the bad program");
    assert_eq!(check_ok, build_ok, "check and build must agree (reject)");

    // The good program: both must accept it.
    let good = r#"import { trim } from "std/string"
import { print } from "std/io"
val x = trim("  hi  ")
print(x)
"#;
    let (check_ok, check_out) = check_source(good);
    let build_ok = build_succeeds(good);
    assert!(check_ok, "check should accept the good program:\n{}", check_out);
    assert!(build_ok, "build should accept the good program");
    assert_eq!(check_ok, build_ok, "check and build must agree (accept)");
}

// -----------------------------------------------------------------------------
// std/iter unification — Stage 2: receiver-dependent combinator return TYPING.
//
// A `std/iter` combinator (`map`/`filter`/`reduce`/`while`) applied to a Stream receiver yields a
// stream-shaped result (`Stream<U>` / `U | Error` / `Null | Error`), while the same combinator on an
// array keeps its eager array-shaped result UNCHANGED. These are `lin check`-level assertions: the
// stream combinator backends do not codegen until Stage 3, so no run tests here. Stream values come
// from `stdinStream()` (a bare `Stream`, no `| Error` open arm) to keep the receiver concrete.
// -----------------------------------------------------------------------------

#[test]
fn test_iter_stream_map_yields_stream_not_array() {
    // `stream.map(f)` must type-check AND its result must be a `Stream`, NOT an array: assert via a
    // `: Stream` annotation (accept) and a `: Int32[]` annotation (reject — the result is a Stream).
    let ok_src = r#"import { stdinStream } from "std/io"
import { map } from "std/iter"
val s: Stream = stdinStream()
val mapped: Stream = s.map(x => x)
"#;
    let (ok, out) = check_source(ok_src);
    assert!(ok, "stream.map(f) should type-check as a Stream:\n{}", out);

    let bad_src = r#"import { stdinStream } from "std/io"
import { map } from "std/iter"
val s: Stream = stdinStream()
val mapped: Int32[] = s.map(x => x)
"#;
    let (ok, out) = check_source(bad_src);
    assert!(
        !ok,
        "stream.map(f) is a Stream, must NOT satisfy an Int32[] annotation:\n{}",
        out
    );
    assert!(
        out.contains("Stream"),
        "rejection should mention the Stream result type:\n{}",
        out
    );
}

#[test]
fn test_iter_stream_reduce_and_while_widen_to_error() {
    // reduce over a stream → `U | Error`; while over a stream → `Null | Error`. Assert the `| Error`
    // arm is present (accept the union annotation) and absent forms are rejected.
    // Each terminal consumes its stream (affine resource), so use TWO separate streams — a
    // single stream cannot feed both `reduce` and `while`.
    let ok_src = r#"import { stdinStream } from "std/io"
import { reduce, while } from "std/iter"
val s1: Stream = stdinStream()
val r: Int32 | Error = s1.reduce(0, (acc, x) => acc)
val s2: Stream = stdinStream()
val w: Null | Error = s2.while(x => true)
"#;
    let (ok, out) = check_source(ok_src);
    assert!(ok, "stream reduce/while should widen to `| Error`:\n{}", out);

    // reduce over a stream is `Int32 | Error`, so a bare `Int32` annotation must be rejected.
    let bad_src = r#"import { stdinStream } from "std/io"
import { reduce } from "std/iter"
val s: Stream = stdinStream()
val r: Int32 = s.reduce(0, (acc, x) => acc)
"#;
    let (ok, out) = check_source(bad_src);
    assert!(
        !ok,
        "stream reduce is `Int32 | Error`, must NOT satisfy a bare `Int32`:\n{}",
        out
    );
}

#[test]
fn test_iter_array_map_still_yields_array_unchanged() {
    // The HARD GATE: an array receiver keeps the eager `U[]` result. Assert by chaining an
    // array-only op (`.length()` from std/array) on the map result, and by an explicit `Int32[]`
    // annotation. A `: Stream` annotation on the array result must be REJECTED.
    let ok_src = r#"import { print } from "std/io"
import { range, map } from "std/iter"
import { length } from "std/array"
val xs: Int32[] = range(0, 5).map(x => x * 2)
print(xs.length())
"#;
    let (ok, out) = check_source(ok_src);
    assert!(ok, "array map must still yield an array (chain .length()):\n{}", out);

    let bad_src = r#"import { range, map } from "std/iter"
val xs: Stream = range(0, 5).map(x => x * 2)
"#;
    let (ok, out) = check_source(bad_src);
    assert!(
        !ok,
        "array map yields an array, must NOT satisfy a `: Stream` annotation:\n{}",
        out
    );
}

#[test]
fn test_iter_generic_iterable_mixed_call_sites() {
    // Verification #3: a USER-DEFINED generic over the Iterable union, called with both an array and
    // a stream. Its OWN return type is monomorphized ONCE to the eager array shape (a mixed
    // `Array | Iterator | Stream` param is not DEFINITELY a stream, so the receiver-dependent
    // re-typing is deliberately suppressed inside the generic body — this is what prevents the
    // stream return from LEAKING into the generic's array call sites). The array call site therefore
    // type-checks as an array; the stream call site ALSO returns the array shape (documented Stage-2
    // limitation — per-call-site stream return needs a direct combinator call, not a user generic).
    let array_site = r#"import { map } from "std/iter"
val passthru = <T>(xs: T[] | Iterator | Stream, f: (T) => T) =>
  xs.map(f)
val a: Int32[] = passthru([1, 2, 3], x => x)
"#;
    let (ok, out) = check_source(array_site);
    assert!(
        ok,
        "array call site of a generic Iterable function must yield an array (no stream leak):\n{}",
        out
    );

    // The generic does NOT give a stream call site a Stream return (it is fixed to the array shape):
    // a `: Stream` annotation on the stream call site is rejected. This documents the boundary.
    let stream_site = r#"import { stdinStream } from "std/io"
import { map } from "std/iter"
val passthru = <T>(xs: T[] | Iterator | Stream, f: (T) => T) =>
  xs.map(f)
val s: Stream = stdinStream()
val b: Stream = passthru(s, x => x)
"#;
    let (ok, out) = check_source(stream_site);
    assert!(
        !ok,
        "a user generic's return is monomorphized to the array shape; the stream call site does NOT \
         produce a Stream (Stage-2 boundary):\n{}",
        out
    );

    // A direct (non-generic) combinator call on the same concrete stream DOES yield a Stream.
    let direct = r#"import { stdinStream } from "std/io"
import { map } from "std/iter"
val s: Stream = stdinStream()
val b: Stream = s.map(x => x)
"#;
    let (ok, out) = check_source(direct);
    assert!(
        ok,
        "a direct combinator call on a concrete stream must yield a Stream:\n{}",
        out
    );
}

// -----------------------------------------------------------------------------
// std/iter unification — Stage 5: affine consume-check re-keyed off the DISPATCH FACT.
//
// The use-after-move check no longer keys on a hardcoded name allowlist; it consumes any
// DEFINITELY-stream argument passed to a call that ROUTES to a stream op (a std/iter combinator
// dispatched to a stream backend, or a std/stream stream-specific op). This mirrors the IR's
// `move_streamish_arg` (lin-ir/src/lower.rs) exactly, so the checker and IR cannot diverge. These
// adversarial programs reuse a stream AFTER it was moved and MUST be rejected; the positives reuse
// fresh pipeline values / arrays and MUST pass. `stdinStream()` gives a bare concrete `Stream`.
// -----------------------------------------------------------------------------

#[test]
fn test_stream_affine_lines_then_reuse_rejected() {
    // Control (was already caught): `lines` moves the stream; a later `collect` of the same
    // binding is a use-after-move.
    let src = r#"import { stdinStream } from "std/io"
import { lines, collect } from "std/stream"
val s: Stream = stdinStream()
val a: Stream = s.lines()
val b: AnyVal = s.collect()
"#;
    let (ok, out) = check_source(src);
    assert!(!ok, "lines-then-reuse must be rejected:\n{}", out);
    assert!(
        out.contains("used after it was consumed"),
        "rejection should be the affine use-after-move error:\n{}",
        out
    );
}

#[test]
fn test_stream_affine_linesmax_then_reuse_rejected() {
    // HOLE #1 (was wrongly accepted): `linesMax` was absent from the old allowlist, so the checker
    // permitted a later `collect` while the IR moved the stream into `linesMax`.
    let src = r#"import { stdinStream } from "std/io"
import { linesMax, collect } from "std/stream"
val s: Stream = stdinStream()
val a: Stream = s.linesMax(1024)
val b: AnyVal = s.collect()
"#;
    let (ok, out) = check_source(src);
    assert!(!ok, "linesMax-then-reuse must be rejected:\n{}", out);
    assert!(
        out.contains("used after it was consumed"),
        "rejection should be the affine use-after-move error:\n{}",
        out
    );
}

#[test]
fn test_stream_affine_promise_then_reuse_rejected() {
    // HOLE #2 (the worst — cross-thread UAF): `promise` MOVES the whole pipeline onto a worker
    // thread (the worker is its sole owner); a later `collect` on the parent is a cross-thread
    // use-after-move. `promise` was absent from the old allowlist.
    let src = r#"import { stdinStream } from "std/io"
import { lines, writeStream, promise, collect } from "std/stream"
val s0: Stream = stdinStream()
val s: Stream = s0.lines().writeStream("out.txt")
val pr: AnyVal = s.promise()
val c: AnyVal = s.collect()
"#;
    let (ok, out) = check_source(src);
    assert!(!ok, "promise-then-reuse must be rejected:\n{}", out);
    assert!(
        out.contains("used after it was consumed"),
        "rejection should be the affine use-after-move error:\n{}",
        out
    );
}

#[test]
fn test_stream_affine_close_then_reuse_rejected() {
    // HOLE #3: `close` ENDS the stream's life (releases the box); a later use is meaningless and
    // a use-after-free. `close` was absent from the old allowlist (there are NO borrow ops).
    let src = r#"import { stdinStream } from "std/io"
import { close, collect } from "std/stream"
val s: Stream = stdinStream()
val unit: Null = s.close()
val c: AnyVal = s.collect()
"#;
    let (ok, out) = check_source(src);
    assert!(!ok, "close-then-reuse must be rejected:\n{}", out);
    assert!(
        out.contains("used after it was consumed"),
        "rejection should be the affine use-after-move error:\n{}",
        out
    );
}

#[test]
fn test_stream_affine_concat_then_reuse_of_either_arg_rejected() {
    // `concat` takes TWO streams; BOTH are moved into the ConcatSource. Reusing EITHER arg
    // afterwards is a use-after-move — the per-argument consume rule must mark both, not just arg0.
    let reuse_second = r#"import { stdinStream } from "std/io"
import { concat } from "std/iter"
import { collect } from "std/stream"
val a: Stream = stdinStream()
val b: Stream = stdinStream()
val c: Stream = a.concat(b)
val reuse: AnyVal = b.collect()
"#;
    let (ok, out) = check_source(reuse_second);
    assert!(!ok, "concat then reuse of the SECOND arg must be rejected:\n{}", out);
    assert!(
        out.contains("used after it was consumed"),
        "rejection should be the affine use-after-move error:\n{}",
        out
    );

    let reuse_first = r#"import { stdinStream } from "std/io"
import { concat } from "std/iter"
import { collect } from "std/stream"
val a: Stream = stdinStream()
val b: Stream = stdinStream()
val c: Stream = a.concat(b)
val reuse: AnyVal = a.collect()
"#;
    let (ok, out) = check_source(reuse_first);
    assert!(!ok, "concat then reuse of the FIRST arg must be rejected:\n{}", out);
    assert!(
        out.contains("used after it was consumed"),
        "rejection should be the affine use-after-move error:\n{}",
        out
    );
}

#[test]
fn test_stream_affine_single_use_chain_and_arrays_unaffected() {
    // POSITIVE: a single-use pipeline chain passes (each stage consumes the PREVIOUS stage's fresh
    // value, which is not a reuse of an already-moved binding).
    let chain = r#"import { stdinStream } from "std/io"
import { lines, drain } from "std/stream"
import { map } from "std/iter"
val s: Stream = stdinStream()
val r: Null | Error = s.lines().map(x => x).drain()
"#;
    let (ok, out) = check_source(chain);
    assert!(ok, "single-use stream chain must pass:\n{}", out);

    // POSITIVE: arrays/iterators are COMPLETELY unaffected — an array may be reused freely across
    // any combinator, including concat, with no affine restriction.
    let arrays = r#"import { print } from "std/io"
import { map, filter, reduce, concat } from "std/iter"
import { length } from "std/array"
val a: Int32[] = [1, 2, 3]
val b: Int32[] = a.map(x => x + 1)
val c: Int32[] = a.filter(x => x > 1)
val d: Int32 = a.reduce(0, (acc, x) => acc + x)
val e: Int32[] = a.concat([4, 5])
val f: Int32[] = a.map(x => x * 2)
print(length(a))
"#;
    let (ok, out) = check_source(arrays);
    assert!(ok, "array combinator chains must be unaffected (free reuse):\n{}", out);
}

// --- `lin test --reporter json` NDJSON contract tests --------------------------
//
// These run a fixture .test.lin through the real `lin test --reporter json` subprocess
// and assert the NDJSON contract the VSCode extension relies on. They are the guard that
// keeps the schema stable.

/// Write `source` to a uniquely-named `<name>.test.lin` under target/ and return its path.
fn write_test_fixture(source: &str) -> PathBuf {
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let path = ws.join(format!("target/lin_jsonrep_{}.test.lin", id));
    fs::write(&path, source).unwrap();
    path
}

/// Run `lin test <fixture> --reporter json [extra...]` and return (exit_success, stdout_lines).
fn run_test_json(fixture: &Path, extra: &[&str]) -> (bool, Vec<String>) {
    let ws = workspace_root();
    let mut args = vec!["test", fixture.to_str().unwrap(), "--reporter", "json"];
    args.extend_from_slice(extra);
    let out = lin_cmd()
        .args(&args)
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin test — run `cargo build -p lin` first");
    let success = out.status.success();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<String> = stdout.lines().map(|l| l.to_string()).filter(|l| !l.trim().is_empty()).collect();
    (success, lines)
}

const TWO_TEST_FIXTURE: &str = r#"import { expect, toBe, test, suite, run } from "std/test"

val s = suite("contract", [
  test("passing test", () =>
    [expect(1).toBe(1)]
  ),
  test("failing test", () =>
    [expect(1).toBe(2)]
  )
])

run(s)
"#;

#[test]
fn test_json_reporter_ndjson_contract() {
    let fixture = write_test_fixture(TWO_TEST_FIXTURE);
    let (success, lines) = run_test_json(&fixture, &[]);
    let _ = fs::remove_file(&fixture);

    // A file with a failing test exits non-zero.
    assert!(!success, "fixture with a failing test should exit non-zero");

    // Every non-empty stdout line must be valid JSON (the core contract guard).
    let records: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("invalid JSON line {:?}: {}", l, e)))
        .collect();

    // First line is the schema meta record.
    assert_eq!(
        records[0],
        serde_json::json!({"event": "meta", "schema": 2}),
        "first NDJSON line must be the meta/schema record"
    );

    // A passing `test` record exists.
    let has_pass = records.iter().any(|r| {
        r["event"] == "test" && r["status"] == "pass" && r["name"] == "passing test"
    });
    assert!(has_pass, "expected a pass test record; got:\n{:?}", records);

    // A failing `test` record exists with a diff message.
    let fail_rec = records.iter().find(|r| {
        r["event"] == "test" && r["status"] == "fail" && r["name"] == "failing test"
    });
    let fail_rec = fail_rec.unwrap_or_else(|| panic!("expected a fail test record; got:\n{:?}", records));
    let msg = fail_rec["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("expected") && msg.contains("actual"),
        "fail message should contain expected/actual diff; got {:?}",
        msg
    );

    // A `file` record with the right (fail) status.
    let has_file_fail = records.iter().any(|r| r["event"] == "file" && r["status"] == "fail");
    assert!(has_file_fail, "expected a file record with status fail; got:\n{:?}", records);
}

// A test that calls `print(...)` must have that stdout forwarded as an `output` NDJSON record
// (schema 2). This is what populates the VSCode Test Results output tab; without it the runner's
// `##LINTEST##` records swallow all other stdout.
const PRINT_FIXTURE: &str = r#"import { expect, toBe, test, suite, run } from "std/test"
import { print } from "std/io"

val s = suite("with output", [
  test("prints then passes", () =>
    val _: Null = print("hello from test")
    [
      expect(1).toBe(1)
    ]
  )
])

run(s)
"#;

#[test]
fn test_json_reporter_forwards_user_print() {
    let fixture = write_test_fixture(PRINT_FIXTURE);
    let (_success, lines) = run_test_json(&fixture, &[]);
    let _ = fs::remove_file(&fixture);

    let records: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("invalid JSON line {:?}: {}", l, e)))
        .collect();

    // Meta record carries schema 2.
    assert_eq!(
        records[0],
        serde_json::json!({"event": "meta", "schema": 2}),
        "meta record must report schema 2"
    );

    // The user's print output is forwarded as an `output` record containing the printed text.
    let out_rec = records
        .iter()
        .find(|r| r["event"] == "output")
        .unwrap_or_else(|| panic!("expected an output record; got:\n{:?}", records));
    let text = out_rec["text"].as_str().unwrap_or("");
    assert!(
        text.contains("hello from test"),
        "output record should carry the printed text; got {:?}",
        text
    );
    // It must NOT leak any `##LINTEST##` runner line into the output blob.
    assert!(
        !text.contains("##LINTEST##"),
        "output record must exclude runner records; got {:?}",
        text
    );
}

#[test]
fn test_json_reporter_filter_test() {
    let fixture = write_test_fixture(TWO_TEST_FIXTURE);
    // Select ONLY the passing test by exact name. The unselected "failing test" must be
    // skipped (no record) AND must not cause a non-zero exit.
    let (success, lines) = run_test_json(&fixture, &["--filter-test", "passing test"]);
    let _ = fs::remove_file(&fixture);

    let records: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("invalid JSON line {:?}: {}", l, e)))
        .collect();

    // The selected test's record appears.
    let test_recs: Vec<&serde_json::Value> = records.iter().filter(|r| r["event"] == "test").collect();
    assert_eq!(test_recs.len(), 1, "exactly one test record expected; got:\n{:?}", records);
    assert_eq!(test_recs[0]["name"], "passing test");
    assert_eq!(test_recs[0]["status"], "pass");

    // The unselected (deliberately failing) test produced NO record...
    let has_failing = records.iter().any(|r| r["event"] == "test" && r["name"] == "failing test");
    assert!(!has_failing, "unselected test must not emit a record; got:\n{:?}", records);

    // ...and skipping it means the run does NOT fail.
    assert!(success, "filtered run with only a passing test must exit zero");
    let file_rec = records.iter().find(|r| r["event"] == "file").expect("expected a file record");
    assert_eq!(file_rec["status"], "pass", "file status should be pass when the only failing test is skipped");
}

// A failing equality matcher (`toBe`) must carry STRUCTURED `expected`/`actual` as proper JSON
// values (not regex-scraped strings), while a non-comparison matcher (`toSatisfy`) must NOT —
// it stays message-only. This is the end-to-end contract the VSCode diff relies on.
const STRUCTURED_FIXTURE: &str = r#"import { expect, toBe, toSatisfy, test, suite, run } from "std/test"

val s = suite("structured", [
  test("equality fail", () =>
    [expect(2).toBe(3)]
  ),
  test("string with quotes", () =>
    [expect("he said \"hi\"").toBe("bye")]
  ),
  test("satisfy fail", () =>
    [expect(5).toSatisfy(x => x > 10)]
  )
])

run(s)
"#;

#[test]
fn test_json_reporter_structured_expected_actual() {
    let fixture = write_test_fixture(STRUCTURED_FIXTURE);
    let (success, lines) = run_test_json(&fixture, &[]);
    let _ = fs::remove_file(&fixture);

    assert!(!success, "fixture with failing tests should exit non-zero");

    let records: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("invalid JSON line {:?}: {}", l, e)))
        .collect();

    // The numeric `toBe` failure carries structured expected/actual as JSON NUMBERS.
    let eq = records
        .iter()
        .find(|r| r["event"] == "test" && r["name"] == "equality fail")
        .unwrap_or_else(|| panic!("missing 'equality fail' record; got:\n{:?}", records));
    assert_eq!(eq["status"], "fail");
    assert_eq!(eq["expected"], serde_json::json!(3), "expected should be the JSON number 3");
    assert_eq!(eq["actual"], serde_json::json!(2), "actual should be the JSON number 2");

    // A string containing quotes round-trips through toJson escaping as a proper JSON string.
    let q = records
        .iter()
        .find(|r| r["event"] == "test" && r["name"] == "string with quotes")
        .unwrap_or_else(|| panic!("missing 'string with quotes' record; got:\n{:?}", records));
    assert_eq!(q["expected"], serde_json::json!("bye"));
    assert_eq!(
        q["actual"],
        serde_json::json!("he said \"hi\""),
        "actual should be the JSON string with embedded quotes intact"
    );

    // A `toSatisfy` failure has NO structured pair — message only.
    let sat = records
        .iter()
        .find(|r| r["event"] == "test" && r["name"] == "satisfy fail")
        .unwrap_or_else(|| panic!("missing 'satisfy fail' record; got:\n{:?}", records));
    assert_eq!(sat["status"], "fail");
    assert!(sat.get("expected").is_none(), "toSatisfy must not carry 'expected'; got:\n{:?}", sat);
    assert!(sat.get("actual").is_none(), "toSatisfy must not carry 'actual'; got:\n{:?}", sat);
    assert!(sat["message"].as_str().unwrap_or("").contains("predicate"), "satisfy message preserved");
}

// ── `Number` as a numerically-bounded generic parameter (ADR-014, reversed) ─────────────────────
// `(x: Number)` is sugar for `<T: numeric>(x: T)`: the body type-checks (the bound permits
// arithmetic), and monomorphization specializes per call-site family to native unboxed ops.

#[test]
fn test_number_param_specializes_int_and_float() {
    // The canonical example: one `Number` parameter, called at Int32 AND Float64. Each call
    // monomorphizes to a native specialization (`isEven$Int32` srem, `isEven$Float64` frem).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val isEven = (x: Number) => x % 2 == 0
print(toString(isEven(4)))
print(toString(isEven(3.0)))
print(toString(isEven(7)))
print(toString(isEven(8.0)))
"#);
    assert_eq!(out, vec!["true", "false", "false", "true"]);
}

#[test]
fn test_number_param_string_arg_is_compile_error() {
    // A `String` argument fails the numeric bound at the call site — a clear compile error.
    let err = run_expect_err(r#"import { print } from "std/io"
val isEven = (x: Number) => x % 2 == 0
print(isEven("hi"))
"#);
    assert!(
        err.contains("expected a numeric type") && err.contains("String"),
        "String into a Number param should be a numeric-bound error; got:\n{}",
        err
    );
}

#[test]
fn test_number_binding_position_is_clear_error() {
    // `Number` is a parameter/return CONSTRAINT, not a value type — using it on a `val`/`var`
    // binding has no concrete representation. The error must point the user at a concrete family
    // rather than the misleading "Unknown type 'Number'".
    let err = run_expect_err(r#"import { print } from "std/io"
import { toString } from "std/string"
val total: Number = 0
print(toString(total))
"#);
    assert!(
        err.contains("parameter constraint, not a value type")
            && (err.contains("Int32") || err.contains("Float64")),
        "Number in binding position should give the constraint-guidance error; got:\n{}",
        err
    );
}

#[test]
fn test_number_multi_param_same_family_per_call() {
    // Two `Number` params; each CALL uses a single family. Distinct calls specialize independently
    // (`add$Int32_Int32`, `add$Float64_Float64`).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val add = (a: Number, b: Number) => a + b
print(toString(add(3, 4)))
print(toString(add(1.5, 2.5)))
"#);
    assert_eq!(out, vec!["7", "4.0"]);
}

#[test]
fn test_number_return_type_annotation() {
    // `Number` is also usable as a return-type annotation (its own fresh bounded var).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val twice = (x: Number): Number => x + x
print(toString(twice(21)))
print(toString(twice(1.25)))
"#);
    assert_eq!(out, vec!["42", "2.5"]);
}

#[test]
fn test_number_mixed_family_in_one_call_widens() {
    // Mixed numeric families in ONE call of a `Number`-returning function are SUPPORTED (ADR-014,
    // reversed): `add$Int32_Float64` is monomorphized and the arithmetic re-widens to the same
    // family the concrete `(a:Int32,b:Float64)` equivalent produces. `add(10, 2.5)` ⇒ Float64
    // `12.5`; `add(10, 2)` stays Int (both Int32); `add(1.5, 2.5)` is Float64. Native widening
    // (sitofp+fadd), no boxed `lin_tagged_arith` — see the monomorphize fix.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val add = (a: Number, b: Number) => a + b
print(toString(add(10, 2.5)))
print(toString(add(10, 2)))
print(toString(add(1.5, 2.5)))
print(toString(add(2.5, 10)))
"#);
    assert_eq!(out, vec!["12.5", "12", "4.0", "12.5"]);
}

#[test]
fn test_number_nested_array_map_specializes() {
    // Nested `Number` (ADR-014, reversed, bug #4): `Number[]` and a `Number` callback over it.
    // `resolve_type_with_number_in` recurses into the Array element; the callback's `Number` param
    // reuses the receiver element's bounded var (so its family is pinned by the argument) and its
    // body type is surfaced as the lambda return, letting the outer call infer. `f([1,2,3])` ⇒
    // element Int32 (native `mul i32` loop); `f([1.5,2.5])` ⇒ Float64.
    let out = run(r#"import { print } from "std/io"
import { map } from "std/iter"
import { toString } from "std/string"
val f = (xs: Number[]) => xs.map((v: Number) => v * 2)
print(toString(f([1, 2, 3])))
print(toString(f([1.5, 2.5])))
"#);
    assert_eq!(out, vec!["[2, 4, 6]", "[3.0, 5.0]"]);
}

#[test]
fn test_number_nested_array_reduce_and_index() {
    // `Number[]` direct numeric use also works: indexing and a reduce whose seed pins the family.
    let out = run(r#"import { print } from "std/io"
import { reduce } from "std/iter"
import { toString } from "std/string"
val sum = (xs: Number[]) => xs.reduce(0, (a, b) => a + b)
val firstTwo = (xs: Number[]) => xs[0] + xs[1]
print(toString(sum([10, 20, 30])))
print(toString(firstTwo([5, 6])))
"#);
    assert_eq!(out, vec!["60", "11"]);
}

#[test]
fn test_number_json_arg_accepted_direct_and_projected_consistent() {
    // ADR-014 (reversed) §AnyVal: a `AnyVal` value is ACCEPTED at a `Number` parameter — consistent
    // with the `AnyVal → Int32` scalar coercion gap (ADR-032), monomorphizing to the default `Int32`
    // family with an unchecked unbox. This was previously INCONSISTENT: a DIRECT `AnyVal`
    // (`val x: AnyVal = 42`, the bare `TypeVar(u32::MAX)` marker) was REJECTED while a `AnyVal`
    // PROJECTION (`config["count"]`, a fresh inference var) slipped past the bound guard and ran.
    // BOTH forms must now compile AND produce the SAME runtime answer (`isEven$AnyVal` unboxes the
    // AnyVal as Int32 and `srem`s — byte-identical specializations). 42 is even ⇒ `true` for both.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val isEven = (x: Number) => x % 2 == 0
val direct: AnyVal = 42
val config: AnyVal = { "count": 42 }
print(toString(isEven(direct)))
print(toString(isEven(config["count"])))
"#);
    assert_eq!(out, vec!["true", "true"]);
}

#[test]
fn test_number_json_arg_arithmetic_returns_right_number() {
    // A AnyVal-int through a `Number` param USED IN ARITHMETIC (a Number-returning body, not just a
    // Bool predicate) must monomorphize to `triple$AnyVal` (param unboxed Int32, native `mul i32`),
    // box the scalar result back to the AnyVal the surrounding `toString` expects, and return the
    // RIGHT number. Both the direct `AnyVal` binding and the `config[...]` projection of 14 ⇒ 42.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val triple = (x: Number) => x * 3
val direct: AnyVal = 14
val config: AnyVal = { "count": 14 }
print(toString(triple(direct)))
print(toString(triple(config["count"])))
"#);
    assert_eq!(out, vec!["42", "42"]);
}

#[test]
#[cfg(unix)]
fn test_print_broken_pipe_exits_cleanly() {
    // A Lin program that prints a lot, piped into a reader that closes early (`head -1`), must
    // NOT panic across the `extern "C"` boundary in `lin_print` (a `writeln!(..).unwrap()` on
    // EPIPE used to abort the process). We assert the Lin process terminates without an abort
    // signal (SIGABRT/SIGSEGV) and emits no Rust panic message.
    use std::os::unix::process::ExitStatusExt;

    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let src_path = ws.join(format!("target/lin_test_pipe_{}.lin", id));
    let bin_path = ws.join(format!("target/lin_test_pipe_{}", id));

    fs::write(
        &src_path,
        r#"import { print } from "std/io"
import { range, for } from "std/iter"
range(0, 1000000).for(i => print("line ${i}"))
"#,
    )
    .unwrap();

    let compile = lin_cmd()
        .args(["build", src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    let _ = fs::remove_file(&src_path);
    assert!(
        compile.status.success(),
        "compilation failed:\nstderr: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    // Spawn the Lin program with piped stdout, feed it into `head -1`, then drop the reader so
    // the pipe closes while the Lin process is still trying to print.
    let mut producer = Command::new(&bin_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn lin producer");

    let producer_stdout = producer.stdout.take().unwrap();
    let head = Command::new("head")
        .arg("-1")
        .stdin(Stdio::from(producer_stdout))
        .stdout(Stdio::null())
        .output()
        .expect("failed to run head");
    assert!(head.status.success(), "head should succeed");

    let out = producer.wait_with_output().expect("failed to wait on producer");
    let _ = fs::remove_file(&bin_path);

    let stderr = String::from_utf8_lossy(&out.stderr);

    // Must not have been killed by an abort/segv signal (a panic-across-FFI aborts with SIGABRT).
    if let Some(sig) = out.status.signal() {
        assert!(
            sig != libc_sigabrt() && sig != 11,
            "lin process died from signal {} (abort/segv) on broken pipe:\nstderr: {}",
            sig,
            stderr
        );
    }
    // And no Rust panic should have leaked to stderr.
    assert!(
        !stderr.contains("panicked") && !stderr.contains("BrokenPipe"),
        "lin process panicked on broken pipe:\nstderr: {}",
        stderr
    );
}

#[cfg(unix)]
fn libc_sigabrt() -> i32 {
    6
}

// -------------------------------------------------------------------------
// Closed-concrete-union discrimination fast path (perf/union-discrimination)
//
// `is V` over a closed concrete union no longer recursively re-validates V's
// fields (`lin_matches_schema`); when V is distinguished from its siblings by a
// StrLit discriminant field it lowers to a cheap `scrut[key] == "lit"` test. The
// optimisation MUST be behaviour-preserving and MUST NOT weaken the `:AnyVal`
// (untrusted-shape) case, which still needs full recursive validation.
// -------------------------------------------------------------------------

#[test]
fn test_union_discrim_forms() {
    // Consolidated union-discrimination behaviours (6 former one-build tests → one program; each
    // case keeps uniquely-named types/functions and its assertions in order).
    let out = run(r#"
import { print } from "std/io"

// strlit_closed_concrete: closed concrete union discriminated by a StrLit field VALUE; both arms
// select the correct variant and the narrowed binding reads the right field.
type COk = { "type": "ok", "value": Int32 }
type CErr = { "type": "err", "msg": String }
type CRes = COk | CErr
val describeC = (r: CRes): String =>
  match r
    is COk => "ok=${r["value"]}"
    is CErr => "err=${r["msg"]}"
val ca: CRes = { "type": "ok", "value": 42 }
val cb: CRes = { "type": "err", "msg": "boom" }
print(describeC(ca))
print(describeC(cb))

// strlit_three_variants: each `is` arm distinguished from BOTH siblings by its StrLit discriminant.
type TA = { "tag": "a", "x": Int32 }
type TB = { "tag": "b", "y": String }
type TC = { "tag": "c", "z": Boolean }
type ABC = TA | TB | TC
val f3 = (v: ABC): String =>
  match v
    is TA => "A:${v["x"]}"
    is TB => "B:${v["y"]}"
    is TC => "C:${v["z"]}"
val t3a: ABC = { "tag": "a", "x": 7 }
val t3b: ABC = { "tag": "b", "y": "hi" }
val t3c: ABC = { "tag": "c", "z": true }
print(f3(t3a))
print(f3(t3b))
print(f3(t3c))

// presence_only_falls_back_but_correct: variants disjoint ONLY by field PRESENCE (unsound under
// width-subtyping) FALL BACK to the full recursive MatchesSchema — must still match correctly.
type Num = { "kind": String, "value": Int32 }
type BinOp = { "kind": String, "op": String, "left": Int32, "right": Int32 }
type Ast = Num | BinOp
val evalAst = (n: Ast): Int32 =>
  match n
    is Num => n["value"]
    is BinOp => n["left"] + n["right"]
val pa: Ast = { "kind": "num", "value": 5 }
val pb: Ast = { "kind": "binop", "op": "+", "left": 3, "right": 4 }
print("${evalAst(pa)}")
print("${evalAst(pb)}")

// json_scrutinee_full_validation: a :AnyVal scrutinee keeps full recursive validation — extra-field
// values match, but a right-discriminant/WRONG-field-type value must NOT match (recursive
// MatchesSchema catches it; the fast path is not used here).
type JOk = { "type": "ok", "value": Int32 }
type JErr = { "type": "err", "msg": String }
val classify = (r: AnyVal): String =>
  match r
    is JOk => "ok"
    is JErr => "err"
    else => "neither"
print(classify({ "type": "ok", "value": 42 }))
print(classify({ "type": "ok", "value": 42, "extra": 1 }))
print(classify({ "type": "ok", "value": "wrong" }))
print(classify({ "type": "ok" }))
print(classify({ "random": 1 }))
print(classify({ "type": "err", "msg": "boom" }))

// standalone_is_expr: the fast path also applies to a standalone `is` boolean expression.
type SOk = { "type": "ok", "value": Int32 }
type SErr = { "type": "err", "msg": String }
type SRes = SOk | SErr
val label = (r: SRes): String => if r is SOk then "yes" else "no"
val sa: SRes = { "type": "ok", "value": 1 }
val sb: SRes = { "type": "err", "msg": "x" }
print(label(sa))
print(label(sb))

// nullable_union: a closed concrete union WITH a Null member — Null is stripped for the
// discriminator analysis, the object variants still discriminate by StrLit.
type MOk = { "type": "ok", "value": Int32 }
type MErr = { "type": "err", "msg": String }
type MaybeRes = MOk | MErr | Null
val describeM = (r: MaybeRes): String =>
  match r
    is MOk => "ok=${r["value"]}"
    is MErr => "err"
    else => "null"
val ma: MaybeRes = { "type": "ok", "value": 9 }
print(describeM(ma))
print(describeM(null))
"#);
    assert_eq!(
        out,
        vec![
            "ok=42", "err=boom",                          // strlit_closed_concrete
            "A:7", "B:hi", "C:true",                      // strlit_three_variants
            "5", "7",                                     // presence_only
            "ok", "ok", "neither", "neither", "neither", "err", // json_scrutinee
            "yes", "no",                                  // standalone_is_expr
            "ok=9", "null",                               // nullable_union
        ]
    );
}

// Stage 0.5 sealed-records run-equivalence: a NAMED record type now carries an (inert) `sealed`
// marker through resolution, while anonymous object literals do not. This test proves the marker
// is BEHAVIOR-INERT: a named-typed value and a structurally-equal anonymous literal still
// inter-operate exactly as before — assign across in BOTH directions, pass into the named param
// position, read fields, and compare equal (including a WIDER literal with an extra field, which
// structural compatibility still permits). See ADR-055 (Stage 0.5: inert sealed marker).
#[test]
fn test_sealed_marker_is_inert_named_vs_anonymous_interop() {
    let out = run(r#"
import { print } from "std/io"
type Point = { "x": Int32, "y": Int32 }

val dist = (p: Point): Int32 => p["x"] + p["y"]

// A named-typed binding.
val named: Point = { "x": 1, "y": 2 }
// An anonymous literal whose inferred (unsealed) type still flows into a named Point slot.
val fromAnon: Point = { "x": 4, "y": 5 }
// A WIDER anonymous literal (extra field) is still structurally compatible at the param.
val wide = { "x": 10, "y": 20, "extra": 99 }

print("named=${dist(named)}")
print("fromAnon=${dist(fromAnon)}")
print("wide=${dist(wide)}")
// Equality holds across a named value and an anonymous literal of identical fields.
val anon = { "x": 1, "y": 2 }
print("eq=${named == anon}")
// The wider value keeps its extra field outside the call (source is untouched).
print("extra=${wide["extra"]}")
"#);
    assert_eq!(
        out,
        vec!["named=3", "fromAnon=9", "wide=30", "eq=true", "extra=99"]
    );
}

// ───────────────────────── Intersection types `&` (ADR-061) ─────────────────────────
// Record-only intersection: `A & B` merges field maps into a plain Type::Object. `&` binds
// tighter than `|`. Named `type T = A & B` inherits named=sealed via expand_named_body.

#[test]
fn test_intersection_authors_example_person_oldperson() {
    // The author's motivating example: OldPerson = Person & { wisdom } has all 3 fields, and a
    // `(person: Person)` function accepts an OldPerson via width subtyping.
    let out = run(r#"
import { print } from "std/io"
type Person = { "age": UInt8, "name": String }
type OldPerson = Person & { "wisdom": Boolean }
val sayHello = (person: Person) => print("Hello ${person["name"]}")
val elder: OldPerson = { "age": 99u8, "name": "Yoda", "wisdom": true }
sayHello(elder)
print("wise: ${elder["wisdom"]}")
"#);
    assert_eq!(out, vec!["Hello Yoda", "wise: true"]);
}

#[test]
fn test_intersection_three_way_all_fields() {
    // `A & B & C` (left-assoc) merges all three field maps.
    let out = run(r#"
import { print } from "std/io"
type A = { "a": Int32 }
type B = { "b": Int32 }
type C = { "c": Int32 }
type ABC = A & B & C
val x: ABC = { "a": 1, "b": 2, "c": 3 }
print("${x["a"] + x["b"] + x["c"]}")
"#);
    assert_eq!(out, vec!["6"]);
}

#[test]
fn test_intersection_inline_param_annotation() {
    // `&` works inline in a parameter annotation, not just a `type` decl.
    let out = run(r#"
import { print } from "std/io"
type Named = { "name": String }
val greet = (p: Named & { "id": Int32 }) => print("${p["name"]}#${p["id"]}")
greet({ "name": "Zed", "id": 7 })
"#);
    assert_eq!(out, vec!["Zed#7"]);
}

#[test]
fn test_intersection_field_conflict_is_error() {
    // Same key, different types → clear type error at the type declaration.
    let (ok, out) = check_source(r#"
type X = { "k": Int32 } & { "k": String }
"#);
    assert!(!ok, "conflict must fail type check");
    assert!(
        out.contains("conflicting field \"k\""),
        "expected conflict error, got: {}",
        out
    );
}

#[test]
fn test_intersection_non_record_operand_is_error() {
    // A non-record operand → clear record-only error at the type declaration.
    let (ok, out) = check_source(r#"
type X = Int32 & String
"#);
    assert!(!ok, "non-record intersection must fail type check");
    assert!(
        out.contains("only valid between record types"),
        "expected record-only error, got: {}",
        out
    );
}

#[test]
fn test_intersection_precedence_amp_tighter_than_pipe() {
    // `A & B | C` parses as `(A & B) | C`: a value satisfying just `A & B` and a value satisfying
    // just `C` are both valid; a value with only `A`'s field is NOT.
    let out = run(r#"
import { print } from "std/io"
type A = { "a": Int32 }
type B = { "b": Int32 }
type C = { "c": Int32 }
type T = A & B | C
val x: T = { "a": 1, "b": 2 }
val y: T = { "c": 3 }
print("ok")
"#);
    assert_eq!(out, vec!["ok"]);
}

#[test]
fn test_intersection_omission_rejected_named_sealed_inherited() {
    // Omitting a merged field from an `&`-defined named type is rejected (named=sealed inherited).
    let err = run_expect_err(r#"
type Person = { "age": UInt8, "name": String }
type OldPerson = Person & { "wisdom": Boolean }
val bad: OldPerson = { "age": 1u8, "name": "x" }
"#);
    // With named-type display (fix/lsp-named-type-display), the expected type shows as the
    // alias name "OldPerson" rather than the structural form listing all fields. The error still
    // correctly rejects the literal missing the "wisdom" field.
    assert!(
        err.contains("OldPerson"),
        "expected omission error mentioning OldPerson type, got: {}",
        err
    );
}

#[test]
fn test_intersection_extras_projected_at_boundary() {
    // A wider literal (extra field) binds to an `&`-defined type; extras are projected away.
    let out = run(r#"
import { print } from "std/io"
type Person = { "age": UInt8, "name": String }
type OldPerson = Person & { "wisdom": Boolean }
val src = { "age": 1u8, "name": "x", "wisdom": true, "extra": 9 }
val p: OldPerson = src
print("${p["name"]}")
"#);
    assert_eq!(out, vec!["x"]);
}

#[test]
fn test_fmt_intersection_roundtrips() {
    // The formatter must reproduce `A & B` (and three-way) exactly.
    assert_eq!(
        fmt("type T = A & B\n").trim(),
        "type T = A & B"
    );
    assert_eq!(
        fmt("type T = A & B & C\n").trim(),
        "type T = A & B & C"
    );
    // `&` binds tighter than `|`: `A & B | C` round-trips without spurious parens.
    assert_eq!(
        fmt("type T = A & B | C\n").trim(),
        "type T = A & B | C"
    );
}

// ───────────────────────── Sealed records — Stage 1 ─────────────────────────
// Unboxed packed-struct layout + constant-offset field access for sealed all-scalar record
// types. See ADR-055 + SPECIFICATION §5.9.1 (sealed records, Stage 1).

#[test]
fn test_sealed_scalar_construct_and_field_read() {
    // (a) Construct a sealed all-scalar record and read every field — correct values via the
    // constant-offset unboxed-struct path.
    let out = run(r#"
import { print } from "std/io"
type Point3 = { "x": Int32, "y": Int32, "z": Float64 }
val p: Point3 = { "x": 10, "y": 20, "z": 1.5 }
print("${p["x"]} ${p["y"]} ${p["z"]}")
print("${p["x"] + p["y"]}")
"#);
    assert_eq!(out, vec!["10 20 1.5", "30"]);
}

#[test]
fn test_sealed_out_of_shape_field_read_is_null_not_panic() {
    // A sealed record has EXACTLY its declared fields. Reading a key NOT in the shape (here the
    // extra "wisdom" that was stripped when the wider literal was assigned to a Person) used to
    // PANIC in codegen (`sealed_field_layout: field "wisdom" not in record`). It must instead
    // follow safe-access (§6.1: missing object key → Null), matching the checker's warning that
    // the field does not exist. Also asserts in-shape reads still work and the extra was stripped.
    let out = run(r#"
import { print } from "std/io"
import { keys } from "std/object"
import { length } from "std/array"
type Person = { "name": String, "age": Int32 }
val wide = { "name": "Doris", "age": 70, "wisdom": true }
val p: Person = wide
print(if p["wisdom"] == null then "absent" else "present")
print(p["name"])
print("${p["age"]}")
print("${keys(p).length()}")
"#);
    assert_eq!(out, vec!["absent", "Doris", "70", "2"]);
}

#[test]
fn test_sealed_dynamic_key_index_no_panic() {
    // Indexing a sealed record with a NON-LITERAL key (`p[k]`) can't resolve a packed-struct slot
    // by offset; the old code read the packed struct as a LinObject and crashed the runtime.
    // Codegen now materializes the sealed record to a boxed object and does the dynamic lookup:
    // a present key returns its value, an absent key (a stripped extra) returns Null.
    let out = run(r#"
import { print } from "std/io"
type Person = { "name": String, "age": Int32 }
val wide = { "name": "Doris", "age": 70, "wisdom": true }
val p: Person = wide
val present = "name"
val absent = "wisdom"
print(p[present])
print(if p[absent] == null then "dyn-absent" else "dyn-present")
"#);
    assert_eq!(out, vec!["Doris", "dyn-absent"]);
}

#[test]
fn test_sealed_array_out_of_shape_field_read_is_null() {
    // Out-of-shape field access on a SEALED-RECORD ARRAY element (`arr[i]["gone"]`) must also be
    // Null, not a panic. The array is typed to Person[]; the source literals carry an extra field
    // that the sealed element layout does not include.
    let out = run(r#"
import { print } from "std/io"
type Person = { "name": String, "age": Int32 }
val people: Person[] = [{ "name": "A", "age": 1, "gone": 9 }, { "name": "B", "age": 2, "gone": 8 }]
print(people[0]["name"])
print("${people[1]["age"]}")
print(if people[0]["gone"] == null then "elem-absent" else "elem-present")
"#);
    assert_eq!(out, vec!["A", "2", "elem-absent"]);
}

#[test]
fn test_boxed_record_array_fused_field_read() {
    // BoxedArrayFieldGet fusion (perf/token-alloc): `arr[i].field` / `arr[i]["field"]` over a BOXED
    // `Object[]` whose element is a sealed record WITH HEAP FIELDS (a `Token` = two Strings) — the
    // calc/interp tokenizer shape. Such an array is NOT a packed sealed-scalar array (the gate rejects
    // heap-field elements), so it stays a boxed `Object[]`. The lowerer fuses the index+field read to a
    // single borrowed `lin_array_get` + `lin_object_get` instead of MATERIALIZING the whole element
    // into a fresh sealed struct (alloc + read every field + per-field retain + reload + release) per
    // access. This asserts the fused read is behaviorally correct: every field reads back its true
    // value through both `["field"]` and the helper-typed path, push grows the array, length is right,
    // and an out-of-bounds guard still returns the sentinel. (RC soundness — no UAF/leak — is verified
    // separately under ASan; this is the behavioral gate.)
    let out = run(r#"
import { print } from "std/io"
import { push, length } from "std/array"
import { toString } from "std/string"
type Token = { "kind": String, "text": String }
val build = (): Token[] =>
  var t: Token[] = []
  push(t, { "kind": "num", "text": "42" })
  push(t, { "kind": "op", "text": "+" })
  push(t, { "kind": "num", "text": "7" })
  t
val kindAt = (toks: Token[], pos: Int32): String =>
  if pos >= length(toks) then "eof" else toks[pos]["kind"]
val toks = build()
print("${length(toks)}")
print("${kindAt(toks, 0)} ${toks[0]["text"]}")
print("${kindAt(toks, 1)} ${toks[1]["text"]}")
print("${kindAt(toks, 2)} ${toks[2]["text"]}")
print(kindAt(toks, 9))
"#);
    assert_eq!(out, vec!["3", "num 42", "op +", "num 7", "eof"]);
}

#[test]
fn test_sealed_boundary_projection_drops_extras_source_untouched() {
    // (b) A wider AnyVal/anonymous literal with an EXTRA field passed to a sealed-scalar param: the
    // param sees only its own fields (extras dropped in the projecting copy), and the ORIGINAL
    // keeps its extra outside the call (non-mutating projection).
    let out = run(r#"
import { print } from "std/io"
type Vec2 = { "a": Int32, "b": Int32 }
val sumv = (v: Vec2): Int32 => v["a"] + v["b"]
val wide = { "a": 3, "b": 4, "extra": 99 }
print("${sumv(wide)}")
print("${wide["extra"]}")
print("${wide["a"]}")
"#);
    assert_eq!(out, vec!["7", "99", "3"]);
}

// ───────────────────── Stage 6a: TAG_RECORD (sealed ptr as tagged dynamic value) ─────────────────
// A typed sealed record placed in a AnyVal/dynamic slot must be observable as a TAG_RECORD value
// (Stage 6a). The runtime routes `lin_box_record` (O(1) pointer wrap, no field copy) instead of
// materializing to a LinObject. Consumer arms (eq, toString, json, field access via descriptor)
// must all dispatch correctly on TAG_RECORD. The test below is the directed proof.

#[test]
fn test_tag_record_anyval_round_trip() {
    // Stage 6a directed test: put a typed sealed record into a AnyVal binding, read fields back,
    // compare equality (TAG_RECORD == TAG_RECORD, TAG_RECORD == TAG_OBJECT same-shape), and
    // serialize to string. Proves TAG_RECORD boxing + all consumer arms are correct.
    let out = run(r#"
import { print } from "std/io"
import { toString } from "std/string"
type P = { "x": Int32, "name": String }
val p: P = { "x": 7, "name": "ada" }
val j: AnyVal = p
print("${j["x"]}")
print("${j["name"]}")
print(toString(j))
val p2: P = { "x": 7, "name": "ada" }
val j2: AnyVal = p2
print("${j == j2}")
val plain = { "x": 7, "name": "ada" }
print("${j == plain}")
print("${plain == j}")
"#);
    assert_eq!(out, vec!["7", "ada", r#"{"x": 7, "name": "ada"}"#, "true", "true", "true"]);
}

#[test]
fn test_sealed_to_json_roundtrip_prints() {
    // (c) A sealed value flowing into a AnyVal slot materializes a boxed object that prints/serializes
    // correctly (sealed → AnyVal boundary).
    let out = run(r#"
import { print } from "std/io"
import { toString } from "std/string"
type Pair = { "lo": Int32, "hi": Int32 }
val p: Pair = { "lo": 7, "hi": 42 }
val j: AnyVal = p
print(toString(j))
print("${j["lo"]} ${j["hi"]}")
"#);
    assert_eq!(out, vec![r#"{"lo": 7, "hi": 42}"#, "7 42"]);
}

#[test]
fn test_sealed_eq_same_shape_as_json_is_true() {
    // (d) Equality is order-independent and crosses representations: a sealed value equals a
    // same-shape boxed AnyVal/anonymous value, and two sealed values of the same type compare
    // field-wise.
    let out = run(r#"
import { print } from "std/io"
type P = { "x": Int32, "y": Int32 }
val a: P = { "x": 1, "y": 2 }
val b: P = { "x": 1, "y": 2 }
val c: P = { "x": 9, "y": 2 }
val anon = { "x": 1, "y": 2 }
print("${a == b}")
print("${a == c}")
print("${a == anon}")
print("${anon == a}")
"#);
    assert_eq!(out, vec!["true", "false", "true", "true"]);
}

#[test]
fn test_sealed_in_match_is_arm() {
    // (e) A sealed record narrowed in a match/`is` arm: field reads on the narrowed binding work.
    let out = run(r#"
import { print } from "std/io"
type Cmd = { "kind": Int32, "arg": Int32 }
val describe = (c: AnyVal): String =>
  match c
    is Cmd => "cmd ${c["kind"]}/${c["arg"]}"
    else => "other"
val x: Cmd = { "kind": 2, "arg": 5 }
print(describe(x))
print(describe({ "kind": 7, "arg": 8 }))
print(describe(42))
"#);
    assert_eq!(out, vec!["cmd 2/5", "cmd 7/8", "other"]);
}

#[test]
fn test_sealed_regression_string_field_stays_boxed() {
    // (f) RUN-EQUIVALENCE: a named record with a String field is now a SEALED record (Stage 2 —
    // String is an eligible heap field), so it uses the packed-struct layout with a pointer slot
    // for `name`. Its observable behaviour (field read, equality, toString) is IDENTICAL to the
    // former boxed path. An anonymous all-scalar literal (unsealed) still stays boxed (never
    // struct-laid-out). The test name is retained for history; the assertions are unchanged.
    let out = run(r#"
import { print } from "std/io"
import { toString } from "std/string"
type Named = { "id": Int32, "name": String }
val n: Named = { "id": 1, "name": "ada" }
print("${n["id"]} ${n["name"]}")
val nn: Named = { "id": 1, "name": "ada" }
print("${n == nn}")
// An anonymous all-scalar literal: unsealed, stays boxed; field read + extra all still work.
val anon = { "p": 3, "q": 4, "r": 5 }
print("${anon["p"] + anon["q"] + anon["r"]}")
print(toString(n))
"#);
    assert_eq!(out, vec!["1 ada", "true", "12", r#"{"id": 1, "name": "ada"}"#]);
}

#[test]
fn test_sealed_captured_by_closure() {
    // A sealed scalar record CAPTURED by a closure: the env owns it (retained on capture) and
    // releases it via the sealed self-sized release on closure teardown — NOT lin_object_release
    // (which would mis-walk the packed struct). ASan-gated in the asan CI job; here we check the
    // functional result.
    let out = run(r#"
import { print } from "std/io"
type Point = { "x": Int32, "y": Int32 }
val makeGetter = (): Int32 =>
  val p: Point = { "x": 3, "y": 4 }
  val getX = (): Int32 => p["x"] + p["y"]
  getX()
print("${makeGetter()}")
"#);
    assert_eq!(out, vec!["7"]);
}

#[test]
fn test_sealed_transferred_across_async_boundary() {
    // A sealed scalar record captured into an `async` thunk crosses the share-nothing thread
    // boundary by a deep byte-copy (CAP_SEALED) and rematerializes on the worker. ASan/TSan-gated
    // in CI; functional check here.
    let out = run(r#"
import { print } from "std/io"
import { async, await } from "std/async"
type Point = { "x": Int32, "y": Int32 }
val p: Point = { "x": 5, "y": 6 }
val job = async(() => p["x"] + p["y"])
print("${await(job)}")
"#);
    assert_eq!(out, vec!["11"]);
}

#[test]
fn test_sealed_spread_into_object_materializes() {
    // REGRESSION (boundary bug, found finishing Stage 1): spreading a sealed scalar record into a
    // boxed object literal must MATERIALIZE it first — the packed struct is NOT a LinObject, so
    // passing it raw to the spread/merge runtime walked it as object entries (null-ptr deref). The
    // spread source is converted to a boxed view (design §3.5 / §5 Stage 1).
    let out = run(r#"
import { print } from "std/io"
import { toString } from "std/string"
type P = { "x": Int32, "y": Int32 }
val p: P = { "x": 1, "y": 2 }
val q = { ...p, "z": 3 }
print(toString(q))
val p2: P = { ...p }
print("${p2["x"]} ${p2["y"]}")
"#);
    assert_eq!(out, vec![r#"{"x": 1, "y": 2, "z": 3}"#, "1 2"]);
}

#[test]
fn test_sealed_as_array_element_and_object_field_value() {
    // REGRESSION (boundary bug, found finishing Stage 1): a sealed scalar record used as an ARRAY
    // ELEMENT or as a FIELD VALUE in a boxed object literal must be materialized to a boxed
    // LinObject (arrays of sealed records are Stage 3; a sealed field value is not a LinObject).
    // Storing the packed struct raw under TAG_OBJECT made later serialize/release mis-walk it.
    let out = run(r#"
import { print } from "std/io"
import { toString } from "std/string"
type P = { "x": Int32, "y": Int32 }
val p: P = { "x": 1, "y": 2 }
val arr = [p, p, p]
print(toString(arr))
val wrap: AnyVal = { "pt": p, "n": 9 }
print(toString(wrap))
"#);
    assert_eq!(
        out,
        vec![
            r#"[{"x": 1, "y": 2}, {"x": 1, "y": 2}, {"x": 1, "y": 2}]"#,
            // Phase 2: open (AnyVal) objects use LinMap (hash-ordered → alphabetical).
            r#"{"pt": {"x": 1, "y": 2}, "n": 9}"#
        ]
    );
}

#[test]
fn test_sealed_var_reassign_releases_old() {
    // A `var` of sealed type reassigned multiple times: each old sealed struct must be released
    // via the sealed release path (not lin_object_release). ASan-gated in CI; functional here.
    let out = run(r#"
import { print } from "std/io"
type P = { "x": Int32, "y": Int32 }
var v: P = { "x": 0, "y": 0 }
v = { "x": 1, "y": 1 }
v = { "x": 2, "y": 3 }
print("${v["x"]} ${v["y"]}")
"#);
    assert_eq!(out, vec!["2 3"]);
}

// ───────────────────── Sealed records with HEAP fields (Stage 2) ─────────────────────
// String / Array / nested-sealed fields are stored as 8-byte owned pointer slots; per-field
// retain on construct/projection-copy, descriptor-driven release on drop. See §5 Stage 2.

#[test]
fn test_sealed_heap_string_field_construct_read_drop() {
    // A sealed record with a String field: construct, read the string and a scalar back, drop.
    let out = run(r#"
import { print } from "std/io"
type User = { "id": Int32, "name": String }
val u: User = { "id": 7, "name": "ada" }
print("${u["id"]} ${u["name"]}")
print("${u["name"]} ${u["name"]}")
"#);
    assert_eq!(out, vec!["7 ada", "ada ada"]);
}

#[test]
fn test_sealed_heap_array_field() {
    // A sealed record with an Array field: construct, read the array back, index into it.
    let out = run(r#"
import { print } from "std/io"
import { length } from "std/array"
type Bag = { "tag": Int32, "items": Int32[] }
val b: Bag = { "tag": 1, "items": [10, 20, 30] }
print("${b["tag"]} ${length(b["items"])}")
print("${b["items"][0]} ${b["items"][2]}")
"#);
    assert_eq!(out, vec!["1 3", "10 30"]);
}

// Regression (record-with-RECORD-ARRAY-field construction leak): a sealed record `T` whose field
// is itself an array OF sealed records (`type Leg = {d}; type T = {legs: Leg[], a}`) — the RAPTOR
// `Trip { stopTimes: StopTime[] }` shape. Building such a value into a `T[]` (push/index-set/map/
// drop) routes the element through the sealed→boxed materializer (`sealed_materialize_to_object` /
// `sealed_array_elem_materializer`), where `box_value` of the `legs` field MATERIALISES a FRESH +1
// tagged `Object[]` (via `sealed_array_to_tagged`) — not a borrowed pointer. The materializer used
// to free only the box SHELL (`lin_tagged_free_box`) for any non-Object heap field, leaking the
// whole fresh `legs` array (header + every `Leg` element) at ~176 B/element on every operation
// (ASan-confirmed linear; sealed harness `record_array` push_read/index_set/array_drop/map_field).
// Fixed by `tagged_release`-ing the field when `box_value_yields_fresh_owned` (sealed Object OR
// sealed-record array). The matching retain is object_set_fresh's, so the count stays balanced — an
// over-release here would corrupt/crash the read-back. cargo test can't see the leak; this guards
// the result is correct (no double-free) across a loop; the ASan harness guards the leak itself.
#[test]
fn test_sealed_record_array_field_build_push_drop_in_loop() {
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { push, length } from "std/array"
import { map } from "std/iter"

type Leg = { "d": Int32 }
type T = { "legs": Leg[], "a": Int32 }

val once = (i: Int32): Int32 =>
  var ts: T[] = []
  push(ts, { "legs": [{ "d": i }], "a": i })
  push(ts, { "legs": [{ "d": i + 1 }, { "d": i + 2 }], "a": i + 10 })
  val ds: Int32[] = map(ts, (x) => x["a"])
  ds[0] + ds[1] + length(ts[1]["legs"])

val loop = (i: Int32, n: Int32, acc: Int32): Int32 =>
  if i >= n then acc
  else loop(i + 1, n, acc + once(i))

print(toString(loop(0, 1000, 0)))
"#);
    // per i: ds[0]=i, ds[1]=i+10, legs(ts[1])=2  => 2*i + 12
    // sum over i in 0..1000 = 2*sum(0..999) + 12*1000 = 999000 + 12000 = 1011000
    assert_eq!(out, vec!["1011000"]);
}

#[test]
fn test_sealed_nested_record_field() {
    // A nested sealed record field: `type Line = { a: Pt, b: Pt }` where Pt is sealed. Releasing the
    // outer must release each inner (descriptor recursion). Read nested fields by chained access.
    let out = run(r#"
import { print } from "std/io"
type Pt = { "x": Int32, "y": Int32 }
type Line = { "a": Pt, "b": Pt }
val l: Line = { "a": { "x": 1, "y": 2 }, "b": { "x": 3, "y": 4 } }
print("${l["a"]["x"]} ${l["a"]["y"]} ${l["b"]["x"]} ${l["b"]["y"]}")
"#);
    assert_eq!(out, vec!["1 2 3 4"]);
}

#[test]
fn test_sealed_heap_projection_drops_extras_source_untouched() {
    // Projection of a WIDER concrete object with a String field into a sealed type: extras dropped
    // from the copy, the source keeps them, no leak. The projected param sees only its own fields.
    let out = run(r#"
import { print } from "std/io"
type Person = { "name": String, "age": Int32 }
val greet = (p: Person): String => "${p["name"]}=${p["age"]}"
val wide = { "name": "bob", "age": 30, "email": "b@x.io" }
print(greet(wide))
print("${wide["email"]}")
print("${wide["name"]}")
"#);
    assert_eq!(out, vec!["bob=30", "b@x.io", "bob"]);
}

#[test]
fn test_sealed_heap_equality_and_to_json() {
    // Equality crosses representations for heap-field records (deep, order-independent), and
    // sealed→AnyVal materialization serializes the heap fields correctly.
    let out = run(r#"
import { print } from "std/io"
import { toString } from "std/string"
type User = { "id": Int32, "name": String }
val a: User = { "id": 1, "name": "ada" }
val b: User = { "id": 1, "name": "ada" }
val c: User = { "id": 1, "name": "bob" }
val anon = { "id": 1, "name": "ada" }
print("${a == b} ${a == c} ${a == anon}")
val j: AnyVal = a
print(toString(j))
"#);
    assert_eq!(out, vec!["true false true", r#"{"id": 1, "name": "ada"}"#]);
}

#[test]
fn test_sealed_heap_captured_by_closure() {
    // A sealed-with-String record captured by a closure: the env owns it (retained on capture),
    // released via the descriptor-driven sealed self-release on teardown (frees the String too).
    let out = run(r#"
import { print } from "std/io"
type User = { "id": Int32, "name": String }
val make = (): String =>
  val u: User = { "id": 5, "name": "zoe" }
  val get = (): String => "${u["id"]}:${u["name"]}"
  get()
print(make())
"#);
    assert_eq!(out, vec!["5:zoe"]);
}

#[test]
fn test_sealed_heap_transferred_across_async() {
    // A sealed-with-String record captured into an async thunk crosses the share-nothing thread
    // boundary: clone_sealed deep-copies the String field per the descriptor, release frees it.
    let out = run(r#"
import { print } from "std/io"
import { async, await } from "std/async"
type Msg = { "n": Int32, "text": String }
val m: Msg = { "n": 3, "text": "hello" }
val t = async((): String => "${m["n"]}:${m["text"]}")
print(await(t))
"#);
    assert_eq!(out, vec!["3:hello"]);
}

#[test]
fn test_sealed_heap_var_reassign_releases_old() {
    // A var of sealed-with-String type reassigned: each old struct's String field released exactly
    // once via the descriptor walk (ASan-gated in CI). Functional check here.
    let out = run(r#"
import { print } from "std/io"
type Box = { "k": Int32, "s": String }
var v: Box = { "k": 0, "s": "a" }
v = { "k": 1, "s": "bb" }
v = { "k": 2, "s": "ccc" }
print("${v["k"]} ${v["s"]}")
"#);
    assert_eq!(out, vec!["2 ccc"]);
}

#[test]
fn test_sealed_heap_in_loop_construct_drop() {
    // Construct/read/drop a heap-field sealed record in a loop — exercises repeated alloc + String
    // retain/release. The accumulated result proves each iteration's value was read before drop.
    let out = run(r#"
import { print } from "std/io"
import { range, reduce } from "std/iter"
type Item = { "v": Int32, "label": String }
val total = range(0, 100).reduce(0, (acc: Int32, i: Int32): Int32 =>
  val it: Item = { "v": i, "label": "x" }
  acc + it["v"]
)
print("${total}")
"#);
    assert_eq!(out, vec!["4950"]);
}

#[test]
fn test_sealed_heap_field_array_build_drop_loop_released_and_correct() {
    // REGRESSION (boxed heap-field-record array leak): a `Trip[]` (sealed record WITH a String
    // heap field — represented as a BOXED `Object[]`, the packed-array gate is scalar-only) built
    // by `push` then read via `ts[i]["field"]` and dropped each iteration. The element-read path
    // (`ts[i]` over the boxed array) PROJECTS the boxed element into a FRESH +1 sealed struct; the
    // lowerer used to add a spurious second `Retain` that was never released → a per-iteration leak
    // of the reconstructed struct (+ its heap fields). ASan is the leak guard (asan CI job over a
    // build/drop loop like this); here we assert the values are correct across many iterations (an
    // over-eager free would corrupt or crash). Covers BOTH the literal-in-push and the val-bound
    // element-push shapes.
    let out = run(r#"
import { print } from "std/io"
import { push, length } from "std/array"
type Trip = { "id": String, "dep": Int32, "arr": Int32 }
val build = (): Int32 =>
  var ts: Trip[] = []
  push(ts, { "id": "a", "dep": 1, "arr": 2 })
  val t: Trip = { "id": "b", "dep": 3, "arr": 4 }
  push(ts, t)
  ts[0]["dep"] + ts[1]["arr"] + length(ts)
val loop = (i: Int32, n: Int32, acc: Int32): Int32 =>
  if i >= n then acc else loop(i + 1, n, acc + build())
print("${loop(0, 200, 0)}")
"#);
    // each build() = 1 (dep) + 4 (arr) + 2 (len) = 7; 200 iterations = 1400.
    assert_eq!(out, vec!["1400"]);
}

#[test]
fn test_sealed_heap_field_array_index_set_released_and_correct() {
    // REGRESSION (boxed heap-field-record array `set`): `set(ts, i, {literal})` over a BOXED
    // `Trip[]` used to CRASH (the monomorphized set stored the raw packed-struct pointer under
    // TAG_OBJECT → the runtime read the packed bytes as a LinObject header → heap-buffer-overflow).
    // Now `emit_array_set` MATERIALIZES the sealed value to a boxed LinObject (mirroring the push
    // path), `lin_array_set` RELEASES the displaced old element, and the IndexSet lowerer skips the
    // spurious source-retain for a sealed elem into a tagged array. ASan-gated for leak/double-free
    // (asan CI job); here we assert correctness: the set must replace the element and read it back.
    let out = run(r#"
import { print } from "std/io"
import { push, set } from "std/array"
type Trip = { "id": String, "dep": Int32, "arr": Int32 }
val build = (): Int32 =>
  var ts: Trip[] = []
  push(ts, { "id": "a", "dep": 1, "arr": 2 })
  set(ts, 0, { "id": "bb", "dep": 9, "arr": 8 })
  ts[0]["dep"] + ts[0]["arr"]
val loop = (i: Int32, n: Int32, acc: Int32): Int32 =>
  if i >= n then acc else loop(i + 1, n, acc + build())
print("${loop(0, 200, 0)}")
"#);
    // each build() = 9 (dep) + 8 (arr) = 17 after the set; 200 iterations = 3400.
    assert_eq!(out, vec!["3400"]);
}

#[test]
fn test_sealed_heap_field_array_nested_array_field_build_drop() {
    // A heap-field sealed record whose field is itself a nested ARRAY (`Route = {name, legs:Int32[]}`)
    // used as a boxed `Route[]`: build/read/drop in a loop. Exercises the element projection over a
    // record with a heap (Array) field. ASan-gated for leaks; correctness asserted here.
    let out = run(r#"
import { print } from "std/io"
import { push, length } from "std/array"
type Route = { "name": String, "legs": Int32[] }
val build = (): Int32 =>
  var rs: Route[] = []
  push(rs, { "name": "r1", "legs": [1, 2, 3] })
  push(rs, { "name": "r2", "legs": [4, 5] })
  length(rs[0]["legs"]) + length(rs[1]["legs"])
val loop = (i: Int32, n: Int32, acc: Int32): Int32 =>
  if i >= n then acc else loop(i + 1, n, acc + build())
print("${loop(0, 200, 0)}")
"#);
    // each build() = 3 + 2 = 5; 200 iterations = 1000.
    assert_eq!(out, vec!["1000"]);
}

#[test]
fn test_sealed_record_array_field_in_outer_array_build_drop() {
    // REGRESSION (monomorphization symbol collision → misaligned-pointer deref / abort): an outer
    // `Route[]` whose element `Route = {id:String, legs: Leg[]}` has a field that is itself an
    // ARRAY OF SEALED RECORDS (`Leg = {name:String, d:Int32}`). Pushing a `Route` and a `Leg` both
    // go through the generic `push<T>(T[], T)`; the specialization name mangled `Type::Object` to a
    // single literal `"Object"`, so `push$Route` and `push$Leg` COLLIDED on the symbol `push$Object`.
    // The monomorphizer minted two distinct specializations but under one name, so codegen emitted
    // both materialize bodies into one LLVM function — only the first (Route's) reachable. A
    // `push(Leg)` call then ran the Route body, reading the Leg struct's scalar `d` field at the
    // `legs`-pointer offset and boxing it as an array (`lin_box_array(0x1)`) → `retain_tagged_payload`
    // dereferenced the bogus pointer (`object.rs:281`, misaligned 0x1) and aborted. Fixed by mangling
    // `Type::Object` by field SHAPE so structurally-distinct records get distinct specialization
    // names. ASan (CI job over a build/drop loop) is the corruption/leak guard; here we assert the
    // length is correct across many iterations (the abort would otherwise crash the run).
    let out = run(r#"
import { print } from "std/io"
import { push, length } from "std/array"
type Leg = { "name": String, "d": Int32 }
type Route = { "id": String, "legs": Leg[] }
val build = (): Int32 =>
  var rs: Route[] = []
  var legs: Leg[] = []
  push(legs, { "name": "x", "d": 1 })
  push(rs, { "id": "r", "legs": legs })
  length(rs)
val loop = (i: Int32, n: Int32, acc: Int32): Int32 =>
  if i >= n then acc else loop(i + 1, n, acc + build())
print("${loop(0, 300, 0)}")
"#);
    // each build() pushes exactly one Route → length 1; 300 iterations = 300.
    assert_eq!(out, vec!["300"]);
}

#[test]
fn test_sealed_tail_recursive_self_call_record_literal_arg() {
    // REGRESSION (found adding the `records` cross-language benchmark): a TAIL-recursive function
    // taking a sealed-record param and passing a fresh record LITERAL as the self-call argument.
    // The outer binding's function type resolves the param to the sealed `Object`, but inside the
    // body the self-reference carries the unexpanded `Named` alias — so at the recursive tail call
    // `func.ty()` reports `Named(_)` while the callee reads the param as a sealed struct. The arg
    // literal was being boxed as AnyVal (the `Named`-is-union-ish path), which the TCO loop header
    // then misread at constant struct offsets → heap corruption / segfault past ~a few thousand
    // iterations. The fix constructs/projects the literal into the sealed layout at the boundary.
    // A small N here exercises the path; the benchmark runs 50M iterations under ASan in CI.
    let out = run(r#"
import { print } from "std/io"
import { toString } from "std/string"
type State = { "a": Int64, "b": Int64, "c": Int64 }
val step = (i: Int64, s: State): State =>
  if i == 0i64 then
    s
  else
    step(i - 1i64, { "a": s["a"] + 1i64, "b": s["b"] + s["a"], "c": s["c"] + 2i64 })
val init: State = { "a": 1i64, "b": 0i64, "c": 0i64 }
val final = step(10000i64, init)
print("${toString(final["a"] + final["b"] + final["c"])}")
"#);
    // a: 1 + 10000 = 10001; b: sum of a over iters; c: 2*10000 = 20000. The exact total is not the
    // point — the point is it RUNS (no segfault) and is deterministic.
    assert_eq!(out, vec!["50035001"]);
}

#[test]
fn test_sealed_heap_field_factory_return_literal_released_and_correct() {
    // REGRESSION (return-position sealed-literal leak): a factory function whose BODY is a sealed
    // heap-field record LITERAL returned directly (`mk = (x): Trip => { "id": "t", "dep": x }`).
    // The body-return lowering used to lower the literal as a BOXED `lin_object_alloc`, then emit a
    // project-into-sealed `Coerce` at the return site; the boxed `LinObject` intermediate (+ its
    // String field) was ORPHANED (kept by `pop_scope_releasing_keep(&[ret_temp, raw_ret])` but not
    // the actual return value) → ~88 B leaked PER CALL. The fix routes the body literal through the
    // packed-construction fast path (`try_lower_sealed_literal`) when the effective return target is
    // a sealed scalar record, so no box is built. ASan (the asan CI job over a call-in-loop like
    // this) is the leak guard — a real per-call leak SCALES with N; here we assert correctness
    // (a reordered-field literal must still read by name, and an over-eager free would crash/garble).
    let out = run(r#"
import { print } from "std/io"
type Trip = { "id": String, "dep": Int32, "arr": Int32 }
val mk = (x: Int32): Trip => { "arr": x + 1, "id": "t", "dep": x }
val build = (): Int32 =>
  val t = mk(5)
  t["dep"] * 100 + t["arr"]
val loop = (i: Int32, n: Int32, acc: Int32): Int32 =>
  if i >= n then acc else loop(i + 1, n, build())
print("${loop(0, 5000, 0)}")
"#);
    // mk(5): dep=5, arr=6; build() = 5*100 + 6 = 506 (constant); loop returns the last build() = 506.
    // The literal is written in REORDERED field order ({arr, id, dep}) to assert the packed
    // construction normalizes to declaration order and reads correctly by name.
    assert_eq!(out, vec!["506"]);
}

// ───────────────── Stack allocation of non-escaping sealed records (Stage 4) ─────────────────
// The escape analysis (lin_ir::escape) marks an all-scalar sealed-record construction whose value
// PROVABLY does not escape its frame for stack allocation (a reused function-entry-block alloca,
// no lin_sealed_alloc) AND suppresses the Retain/Release emission on it (so the alloca SROA-promotes
// to registers). The KEY soundness property — never stack-allocating a record that escapes (a
// use-after-return) — is ASan-gated in CI; these tests pin observable behaviour for the stack path,
// the heap fallbacks, a high-iteration no-stack-overflow loop, and the RC-suppressed IR shape.

/// Build `source` with LIN_EMIT_IR=1 + LIN_NO_OPT=1 and return the raw (pre-optimization) LLVM IR
/// text. Used to assert the SHAPE of the emitted IR (e.g. no lin_rc_retain / lin_sealed_release /
/// lin_sealed_alloc on a stack-resident sealed record's hot loop).
fn build_ir(source: &str) -> String {
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let src_path = ws.join(format!("target/lin_test_ir_{}.lin", id));
    let bin_path = ws.join(format!("target/lin_test_ir_{}", id));
    let ll_path = ws.join(format!("target/lin_test_ir_{}.ll", id));
    fs::write(&src_path, source).unwrap();
    let compile = lin_cmd()
        .args(["build", src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .env("LIN_EMIT_IR", "1")
        .env("LIN_NO_OPT", "1")
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary");
    let _ = fs::remove_file(&src_path);
    let _ = fs::remove_file(&bin_path);
    assert!(
        compile.status.success(),
        "compilation failed:\nstderr: {}\nsource:\n{}",
        String::from_utf8_lossy(&compile.stderr),
        source
    );
    let ir = fs::read_to_string(&ll_path).expect("LLVM IR .ll not emitted");
    let _ = fs::remove_file(&ll_path);
    ir
}

#[test]
fn test_sealed_stack_tco_loop_high_n_no_overflow() {
    // The records.lin shape at a HIGH iteration count: a TCO loop that builds a FRESH all-scalar
    // sealed State each iteration and tail-recurses. The fresh State is stack-allocated in a REUSED
    // entry-block alloca, so 5,000,000 iterations must NOT grow the stack (a per-iteration alloca
    // would overflow). Correctness: the same bounded LCG-style mix as the benchmark, deterministic.
    let out = run(r#"
import { print } from "std/io"
import { toString } from "std/string"
type State = { "a": Int64, "b": Int64, "c": Int64 }
val MOD = 2147483647i64
val step = (i: Int64, s: State): State =>
  if i == 0i64 then
    s
  else
    val a = (s["a"] * 7i64 + s["c"] + 1i64) - (s["a"] * 7i64 + s["c"] + 1i64) / MOD * MOD
    val b = (s["b"] + s["a"] * 3i64) - (s["b"] + s["a"] * 3i64) / MOD * MOD
    val c = (s["c"] + s["b"] + 5i64) - (s["c"] + s["b"] + 5i64) / MOD * MOD
    step(i - 1i64, { "a": a, "b": b, "c": c })
val init: State = { "a": 1i64, "b": 2i64, "c": 3i64 }
val final = step(5000000i64, init)
val sum = final["a"] + final["b"] + final["c"]
print("${toString(sum - (sum / MOD) * MOD)}")
"#);
    // Deterministic (matches a reference run); the point is it RUNS at 5M iters with no stack growth.
    assert_eq!(out, vec!["839929631"]);
}

#[test]
fn test_sealed_stack_tco_loop_ir_has_no_rc_or_heap_alloc() {
    // RC-emission SUPPRESSION (this milestone): the records-style hot loop builds a fresh all-scalar
    // sealed State each iteration that is PROVEN stack-resident. The emitted IR for that loop must
    // contain NO heap allocation of the State (lin_sealed_alloc) AND NO refcount traffic on it
    // (lin_rc_retain / lin_sealed_release) — those are the calls the Stage-4-without-suppression
    // prototype still emitted (~140 retain / ~37 release) that made it 12% slower than heap. We
    // assert they are GONE so the alloca can SROA-promote to registers.
    let ir = build_ir(r#"
import { print } from "std/io"
import { toString } from "std/string"
type State = { "a": Int64, "b": Int64, "c": Int64 }
val MOD = 2147483647i64
val step = (i: Int64, s: State): State =>
  if i == 0i64 then
    s
  else
    val a = (s["a"] * 7i64 + s["c"] + 1i64) - (s["a"] * 7i64 + s["c"] + 1i64) / MOD * MOD
    val b = (s["b"] + s["a"] * 3i64) - (s["b"] + s["a"] * 3i64) / MOD * MOD
    val c = (s["c"] + s["b"] + 5i64) - (s["c"] + s["b"] + 5i64) / MOD * MOD
    step(i - 1i64, { "a": a, "b": b, "c": c })
val init: State = { "a": 1i64, "b": 2i64, "c": 3i64 }
val final = step(50i64, init)
print("${toString(final["a"] + final["b"] + final["c"])}")
"#);
    // Scope the IR check to the `@step` function (the hot TCO loop). Other functions (stdlib, main)
    // legitimately use RC; what matters is the loop that rebuilds State every iteration.
    let step = ir_function(&ir, "step");
    // The hot-loop construction must use a stack alloca (named "sealed_stack" by
    // sealed_construct_stack), proving the non-escaping State is NOT heap-allocated there.
    assert!(step.contains("sealed_stack"), "expected a stack alloca for the non-escaping State:\n{step}");
    // The TCO loop must carry NO refcount traffic on the State after RC suppression. (The only
    // remaining @lin_sealed_alloc in `step` is the base-case `return s` materialization, which
    // correctly escapes and stays heap — so we do NOT forbid lin_sealed_alloc outright, only RC.)
    assert!(
        !step.contains("@lin_rc_retain"),
        "stack State hot loop must carry NO lin_rc_retain after RC suppression:\n{step}"
    );
    assert!(
        !step.contains("@lin_sealed_release"),
        "stack State hot loop must carry NO lin_sealed_release after RC suppression:\n{step}"
    );
}

#[test]
fn test_tco_loop_fresh_arg_releases_old_slot_value() {
    // Per-iteration TCO param release (fixes the dominant TCO-loop leak class): a tail-recursive
    // function whose recurring arg is a FRESH heap value each iteration must release the PRIOR
    // slot value before the back-edge overwrites it, instead of leaking it (the lowerer's
    // scope-exit release lands in the unreachable `tco_post` block). ASan is the actual leak/
    // double-free guard (see the ci.yml `asan` job + the synthetic repros); this test pins the
    // OBSERVABLE behavior: every aliasing shape computes the correct result and does not crash.
    //
    // Shapes exercised: (a) fresh array threaded as the recurring arg; (b) a SECOND param threaded
    // UNCHANGED alongside the fresh one (must not be released — the alias guard); (c) fresh union/
    // AnyVal box; (d) an array mutated IN PLACE and passed back (new value == old slot — alias guard).
    let out = run(r#"
import { push, length } from "std/array"
import { print } from "std/io"
import { toString } from "std/string"

// (a)+(b): `acc` threaded unchanged, `fresh` rebuilt every round.
val sumFresh = (acc: AnyVal, fresh: AnyVal, k: Int32): Int32 =>
  if k <= 0 then length(acc)
  else
    var f: AnyVal = []
    push(f, k)
    push(f, k + 1)
    sumFresh(acc, f, k - 1)

// (c): fresh union/AnyVal box every round.
val unionLoop = (m: String | Int32, k: Int32): Int32 =>
  if k <= 0 then k
  else
    val fresh: String | Int32 = "r${k}"
    unionLoop(fresh, k - 1)

// (d): same array mutated in place and passed back (new == old slot value).
val growInPlace = (acc: AnyVal, k: Int32): Int32 =>
  if k <= 0 then length(acc)
  else
    push(acc, k)
    growInPlace(acc, k - 1)

val a: AnyVal = [1, 2, 3]
var f0: AnyVal = []
print(toString(sumFresh(a, f0, 50)))
print(toString(unionLoop(0, 50)))
var g: AnyVal = []
print(toString(growInPlace(g, 50)))
"#);
    // sumFresh returns length(acc) = 3 (acc threaded unchanged, never mutated).
    // unionLoop returns k at the base case = 0.
    // growInPlace pushes 50 elements into the same array → length 50.
    assert_eq!(out, vec!["3", "0", "50"]);
}

#[test]
fn test_tail_recursive_if_json_branch_vs_concrete_branch_not_mistyped() {
    // A tail-recursive function whose body is an `if`/`else` where ONE terminal branch returns a
    // freshly-built `AnyVal` value (an error OBJECT) and the OTHER returns the owned ARRAY param.
    // The checker's `infer_if` merge collapsed `AnyVal | String[][]` onto the CONCRETE branch
    // (`String[][]`) because `AnyVal` (the dynamic top `TypeVar(u32::MAX)`) is `types_compatible`
    // with everything — so the if-expression was mistyped as `String[][]`. lin-ir then boxed BOTH
    // branches with the concrete array representation (`lin_box_array`), mis-tagging the AnyVal error
    // object as an array; reading the result (here, string-interpolating it) dereferenced the
    // object as an array header → null-deref/corruption. Fix: when exactly one branch is the
    // dynamic `AnyVal` top type, the merged type IS `AnyVal` (it subsumes the concrete branch), so each
    // branch boxes into its own correct representation and the merge is a uniform AnyVal box.
    //
    // Asserts BOTH terminal paths: state==0 returns the array unchanged; state==1 returns the
    // error object. ASan (ci.yml `asan` leg over this exact shape) is the leak/double-free guard;
    // this pins the OBSERVABLE result — the correct value comes back from BOTH branches.
    let out = run(r#"
import { print } from "std/io"

val mkErr = (msg: String): AnyVal => { "type": "error", "message": msg }

val step = (rows: String[][], i: Int64, n: Int64, state: Int64): AnyVal =>
  if i >= n then
    if state == 1 then mkErr("unterminated") else rows
  else
    step(rows, i + 1, n, state)

val ok = step([["a"]], 0, 1, 0)
val err = step([["a"]], 0, 1, 1)
print("${ok}")
print("${err}")
"#);
    // state==0: the owned `rows` param threads through and is returned as the array.
    // state==1: the fresh error object is returned (correctly tagged as an object, not an array).
    assert_eq!(
        out,
        vec![
            r#"[["a"]]"#.to_string(),
            // Phase 2: open objects use LinMap (hash-ordered keys → alphabetical in toString).
            r#"{"type": "error", "message": "unterminated"}"#.to_string(),
        ]
    );
}

#[test]
fn test_if_json_branch_vs_concrete_branch_not_collapsed_to_concrete() {
    // The non-tail-recursive minimal form of the same `infer_if` mistyping bug: a plain `if`
    // whose then-branch is `AnyVal` and else-branch is a concrete heap type. The merge must be
    // `AnyVal`, not the concrete type — otherwise lowering boxes the AnyVal branch with the concrete
    // representation and corrupts it on read.
    let out = run(r#"
import { print } from "std/io"

val mkErr = (msg: String): AnyVal => { "type": "error", "message": msg }

val pick = (rows: String[][], state: Int64): AnyVal =>
  if state == 1 then mkErr("x") else rows

print("${pick([["a"]], 1)}")
print("${pick([["a"]], 0)}")
"#);
    assert_eq!(
        out,
        vec![
            // Phase 2: open objects use LinMap (hash-ordered keys → alphabetical in toString).
            r#"{"type": "error", "message": "x"}"#.to_string(),
            r#"[["a"]]"#.to_string(),
        ]
    );
}

#[test]
fn test_if_branch_calling_json_function_not_mistyped_as_other_branch() {
    // ROOT: calling a value typed `(AnyVal) => AnyVal` (or the bare opaque `Function` annotation)
    // returned a FRESH, never-constrained inference TypeVar instead of `AnyVal`. Both the opaque
    // `Function` and a concrete `(AnyVal) => AnyVal` resolve to the structurally-identical
    // `func([TypeVar(MAX)], TypeVar(MAX))`, so the call-site `is_opaque` heuristic misclassified
    // the concrete signature and freshened its return into a dangling var. Nothing ever solves that
    // var, so an `if` whose then-branch is `jsonFn(x)` (typed `?T`) and whose else-branch is a
    // concrete `Bool` collapsed the merge onto `Bool` via `types_compatible` (an unconstrained
    // TypeVar is vacuously compatible with everything). Codegen then unboxed the AnyVal branch's value
    // AS a raw Bool — a NULL-pointer dereference when that value is `null` (`onRow` returns `null`).
    // Fix: an opaque/AnyVal-function call yields the dynamic top type `AnyVal`, which is concrete, so
    // the merge stays `AnyVal` and each branch boxes into its own correct representation.
    //
    // This is the minimal closure-free form. The then-branch (the AnyVal call returning `null`) is
    // forced taken; on the buggy build this null-derefs in `lin_unbox_bool`.
    let out = run(r#"
import { print } from "std/io"
val onRow = (row: AnyVal): AnyVal => null
var hd = true
val x = if hd then
    onRow([1])
  else
    hd = false
print("${x}")
"#);
    assert_eq!(out, vec!["null".to_string()]);
}

#[test]
fn test_captured_json_closure_called_in_for_if_no_null_deref() {
    // End-to-end shape of the same `(AnyVal) => AnyVal` mis-typing: a closure (`onRow`) returned from a
    // function (`mk`) and captured into a `.for` callback alongside a reassigned `var headerDone`.
    // The callback's body `if headerDone then onRow(row) else headerDone = true` merged the AnyVal
    // call result (mis-typed as a dangling var) with the `Bool` assignment and was lowered as a Bool
    // — so the SECOND iteration (when `headerDone` is true and `onRow` runs, returning the `null`
    // from `push`) unboxed that null as a Bool and aborted (`tagged.rs` null-deref). The closure /
    // var-capture machinery is sound; the defect was purely the call-result type. Crash-isolation
    // test (kept un-batched): a regression would SIGABRT the whole process.
    let out = run(r#"
import { for } from "std/iter"
import { push } from "std/array"
import { print } from "std/io"
type RowFn = (AnyVal) => AnyVal
var out: AnyVal = []
val mk = (header: AnyVal): RowFn =>
  val ia: Int32 = header[1]
  (row: AnyVal): AnyVal =>
    push(out, row[ia])
val drive = (rows: AnyVal): AnyVal =>
  val onRow = mk(rows[0])
  var headerDone = false
  rows.for(row =>
    if headerDone then
      onRow(row)
    else
      headerDone = true
  )
  null
drive([[0, 1], [10, 20], [30, 40]])
print("${out}")
"#);
    // rows[0] is the header (skipped). `ia = header[1] = 1`, so `onRow` reads index 1 of each later
    // row: [10,20]->20, [30,40]->40.
    assert_eq!(out, vec!["[20, 40]".to_string()]);
}

#[test]
fn test_tco_typed_record_array_param_no_per_iteration_leak() {
    // A TYPED sealed-record array (`Transfer[]`, currently a boxed `Object[]` with heap fields)
    // threaded UNCHANGED through a TAIL-recursive parameter and grown via `push` must not leak a
    // reference per iteration. The concrete-rc param read takes a `Retain`-in-place on every use
    // (the `push` receiver AND the tail-call arg), and the matching scope-exit releases land in the
    // dead `tco_post` block — so without `release_owned_for_tail_call` releasing every read-retain
    // of a PASS-THROUGH param, the array (header + element buffer + ~20 element records) leaked
    // once per outer `build()` call (~2800 B/call; 8.4 MB at n=3000). A `AnyVal[]` tail-param is fine
    // (no read-retain) and a non-tail typed array is fine (scope-exit release runs) — the leak fired
    // only at the intersection. ASan (ci.yml `asan` leg + the synthetic repro) is the actual leak/
    // double-free guard; this test pins the OBSERVABLE behavior: correct length + value, no crash.
    let out = run(r#"
import { push, length } from "std/array"
import { print } from "std/io"
import { toString } from "std/string"
type Transfer = { "origin": String, "destination": String, "dur": Int32 }
val makeTransfer = (o: String, d: String, dur: Int32): Transfer =>
  { "origin": o, "destination": d, "dur": dur }
// `ts` threaded UNCHANGED through every tail call (same array, grown in place).
val fill = (ts: Transfer[], i: Int32, n: Int32): Int32 =>
  if i >= n then length(ts)
  else
    push(ts, makeTransfer("A", "B", i))
    fill(ts, i + 1, n)
val build = (): Int32 =>
  var ts: Transfer[] = []
  fill(ts, 0, 20)
// Outer loop: every iteration builds a fresh 20-element Transfer[]. A per-iteration leak would
// scale RSS with the outer count; the result is invariant.
val loop = (i: Int32, n: Int32, acc: Int32): Int32 =>
  if i >= n then acc else loop(i + 1, n, build())
print(toString(loop(0, 50, 0)))
"#);
    // Every build() fills 20 elements; the loop returns the last build()'s length = 20.
    assert_eq!(out, vec!["20"]);
}

#[test]
fn test_sealed_record_union_tail_param_no_per_iteration_leak() {
    // A `Trip | Null` (sealed-record | Null union) threaded through a TAIL-recursive parameter —
    // the exact shape of RAPTOR's `scanRouteAt` `trip: Trip | Null` forward-scan param. Each tail
    // iteration binds a fresh `cur: Trip` (here a literal; the array-projection `arr[i]` form is
    // exercised by the second function) and passes it as the union arg, which codegen MATERIALIZES
    // into a boxed object. The per-iteration `cur` source packed struct must be released on the live
    // back-edge — it accrues TWO genuine owned references (the alloc/projection +1 AND
    // `coerce_and_own_store`'s `own_for_store` retain at the `val` binding +1), and the prior
    // one-per-temp dedup in `release_owned_for_tail_call` released it ONCE, leaking the surplus
    // packed struct (+ its heap "id" string) every tail iteration. Releasing sealed-record temps
    // per registration balances it. (The `match trip is Trip => trip["dep"]` arm-narrowing
    // projection — a fresh `sealed_project_from` struct — also leaked every base-case read until the
    // narrowed-union→sealed read stopped double-retaining it.) ASan (the synthetic repro + ci.yml
    // asan leg) is the actual leak/double-free guard; this test pins the observable result.
    let out = run(r#"
import { push } from "std/array"
import { print } from "std/io"
import { toString } from "std/string"
type Trip = { "id": String, "dep": Int32 }
// Fresh-literal form: each tail iteration threads a freshly-built sealed record into Trip | Null.
val scanFresh = (i: Int32, n: Int32, trip: Trip | Null): Int32 =>
  if i >= n then
    match trip
      is Trip => trip["dep"]
      else => -1
  else
    val cur: Trip = { "id": "x", "dep": i }
    scanFresh(i + 1, n, cur)
// Array-projection form: each tail iteration threads arr[i] (a projected sealed record).
val scanProj = (arr: AnyVal, i: Int32, n: Int32, trip: Trip | Null): Int32 =>
  if i >= n then
    match trip
      is Trip => trip["dep"]
      else => -1
  else
    val cur: Trip = arr[i]
    scanProj(arr, i + 1, n, cur)
val build = (): Int32 =>
  var arr: AnyVal = []
  arr.push({ "id": "a", "dep": 7 })
  // scanFresh recurses 20 deep returning the last dep (19); scanProj reads the single element (7).
  scanFresh(0, 20, null) + scanProj(arr, 0, 1, null)
// Outer loop: a per-iteration leak inside scan would scale RSS with the outer count; result is
// invariant (19 + 7 = 26 every time).
val loop = (i: Int32, n: Int32, acc: Int32): Int32 =>
  if i >= n then acc else loop(i + 1, n, build())
print(toString(loop(0, 50, 0)))
"#);
    assert_eq!(out, vec!["26"]);
}

/// Extract the body text of the LLVM function `define ... @<name>(...) { ... }` from emitted IR.
/// Matches on `@<name>(` so it doesn't catch a prefixed/suffixed symbol.
fn ir_function(ir: &str, name: &str) -> String {
    let needle = format!("@{name}(");
    let mut out = String::new();
    let mut in_fn = false;
    for line in ir.lines() {
        if !in_fn && line.starts_with("define ") && line.contains(&needle) {
            in_fn = true;
        }
        if in_fn {
            out.push_str(line);
            out.push('\n');
            if line == "}" {
                break;
            }
        }
    }
    assert!(!out.is_empty(), "function @{name} not found in IR");
    out
}

#[test]
fn test_sealed_escaping_returned_uses_heap() {
    // A constructed sealed record that is RETURNED out of the function ESCAPES → must stay heap
    // (NOT stack-allocated, which would be a use-after-return). Functional check that the returned
    // record's fields are intact after the call returns.
    let out = run(r#"
import { print } from "std/io"
type Pt = { "x": Int32, "y": Int32 }
val make = (a: Int32, b: Int32): Pt => { "x": a, "y": b }
val p = make(11, 22)
print("${p["x"]} ${p["y"]}")
val q = make(3, 4)
print("${q["x"] + q["y"]}")
print("${p["x"] + q["x"]}")
"#);
    assert_eq!(out, vec!["11 22", "7", "14"]);
}

#[test]
fn test_sealed_escaping_returned_ir_uses_heap_alloc() {
    // The returned-record escape case must STILL heap-allocate (lin_sealed_alloc present) — verify
    // the suppression did not over-reach and stack-allocate an escaping value.
    let ir = build_ir(r#"
import { print } from "std/io"
type Pt = { "x": Int32, "y": Int32 }
val make = (a: Int32, b: Int32): Pt => { "x": a, "y": b }
val p = make(11, 22)
print("${p["x"]} ${p["y"]}")
"#);
    assert!(
        ir.contains("@lin_sealed_alloc"),
        "a RETURNED sealed record must remain heap-allocated:\n{ir}"
    );
}

#[test]
fn test_sealed_escaping_stored_in_array_uses_heap() {
    // A constructed sealed record STORED into an array container ESCAPES the constructing scope →
    // heap. (Stack-allocating it would leave the array holding a dangling stack pointer.)
    let out = run(r#"
import { print } from "std/io"
import { length } from "std/array"
type Pt = { "x": Int32, "y": Int32 }
val build = (): Pt[] =>
  val a: Pt = { "x": 1, "y": 2 }
  val b: Pt = { "x": 3, "y": 4 }
  [a, b]
val arr = build()
print("${length(arr)}")
print("${arr[0]["x"]} ${arr[1]["y"]}")
"#);
    assert_eq!(out, vec!["2", "1 4"]);
}

#[test]
fn test_sealed_escaping_captured_by_closure_uses_heap() {
    // A sealed record CAPTURED by a closure that escapes (returned as a value) must stay heap — the
    // closure's env holds the record past the constructing frame.
    let out = run(r#"
import { print } from "std/io"
type Pt = { "x": Int32, "y": Int32 }
val makeAdder = () =>
  val p: Pt = { "x": 10, "y": 20 }
  () => p["x"] + p["y"]
val f = makeAdder()
print("${f()}")
print("${f()}")
"#);
    assert_eq!(out, vec!["30", "30"]);
}

#[test]
fn test_sealed_stack_local_dies_in_frame_and_heap_escape_mixed() {
    // MIXED in one program: a purely-local sealed record (constructed, fields read, dies in frame →
    // stack candidate) alongside one that is returned (→ heap). Both produce correct values.
    let out = run(r#"
import { print } from "std/io"
type Pt = { "x": Int32, "y": Int32 }
val compute = (n: Int32): Int32 =>
  val local: Pt = { "x": n, "y": n * 2 }
  local["x"] + local["y"]
val makeEscape = (n: Int32): Pt => { "x": n, "y": n }
print("${compute(5)}")
val e = makeEscape(9)
print("${e["x"] + e["y"]}")
"#);
    assert_eq!(out, vec!["15", "18"]);
}

#[test]
fn test_sealed_stack_return_on_base_path_is_sound() {
    // The records.lin SUBTLETY: the SAME binding `s` is RETURNED on the base case but the
    // freshly-constructed intermediates are not. The base-case return materializes `s` and
    // re-projects a FRESH heap struct (so the param does not escape by pointer), while the
    // tail-call intermediates are stack-allocated. Returning on the base path must NOT be a
    // use-after-return. Run a few iterations and read the returned fields — they must be intact.
    let out = run(r#"
import { print } from "std/io"
type State = { "a": Int64, "b": Int64 }
val step = (i: Int64, s: State): State =>
  if i == 0i64 then
    s
  else
    step(i - 1i64, { "a": s["a"] + 1i64, "b": s["b"] + s["a"] })
val r = step(5i64, { "a": 1i64, "b": 0i64 })
print("${r["a"]} ${r["b"]}")
"#);
    // a: 1 + 5 = 6; b accumulates a over 5 steps: 1+2+3+4+5 = 15.
    assert_eq!(out, vec!["6 15"]);
}

// ───────────────────── Arrays of sealed scalar records (Stage 3) ─────────────────────
// A `MyType[]` of an ALL-SCALAR sealed record is stored as a CONTIGUOUS, UNBOXED, header-less
// buffer (elem_tag 0xFE), not an array of boxed LinObjects. `arr[i].f` is a constant-stride GEP +
// scalar load (no per-element box / lin_object_get). See §3.11 / §5 Stage 3.

#[test]
fn test_sealed_array_construct_index_field_read() {
    // Construct a Point[] literal, read whole elements and their fields by constant-offset.
    let out = run(r#"
import { print } from "std/io"
type Point = { "x": Int32, "y": Int32 }
val pts: Point[] = [{ "x": 1, "y": 2 }, { "x": 3, "y": 4 }, { "x": 5, "y": 6 }]
print("${pts[0]["x"]} ${pts[0]["y"]}")
print("${pts[1]["x"]} ${pts[2]["y"]}")
val first = pts[0]
print("${first["x"] + first["y"]}")
"#);
    assert_eq!(out, vec!["1 2", "3 6", "3"]);
}

#[test]
fn test_sealed_array_sum_field_via_recursion_and_length() {
    // Sum a field across the array (fused arr[i].field reads) and read length().
    let out = run(r#"
import { print } from "std/io"
import { length } from "std/array"
type Point = { "x": Int64, "y": Int64 }
val pts: Point[] = [{ "x": 10i64, "y": 1i64 }, { "x": 20i64, "y": 2i64 }, { "x": 30i64, "y": 3i64 }]
val sumX = (arr: Point[], i: Int64, acc: Int64): Int64 =>
  if i == 3i64 then acc else sumX(arr, i + 1i64, acc + arr[i]["x"])
print("${sumX(pts, 0i64, 0i64)}")
print("${length(pts)}")
"#);
    assert_eq!(out, vec!["60", "3"]);
}

#[test]
fn test_sealed_array_to_json_tostring() {
    // A sealed array flowing to a AnyVal slot / toString MATERIALIZES a boxed Object[] (the fail-safe
    // boundary view) and serializes identically to a boxed array of objects.
    let out = run(r#"
import { print } from "std/io"
import { toString } from "std/string"
type Point = { "x": Int32, "y": Int32 }
val pts: Point[] = [{ "x": 1, "y": 2 }, { "x": 3, "y": 4 }]
print(toString(pts))
val j: AnyVal = pts
print(toString(j))
"#);
    assert_eq!(
        out,
        vec![
            r#"[{"x": 1, "y": 2}, {"x": 3, "y": 4}]"#,
            r#"[{"x": 1, "y": 2}, {"x": 3, "y": 4}]"#,
        ]
    );
}

#[test]
fn test_sealed_array_equality_same_shape() {
    // Two sealed arrays of equal shape compare equal (via the materialized tagged view); a differing
    // element makes them unequal.
    let out = run(r#"
import { print } from "std/io"
type P = { "x": Int32, "y": Int32 }
val a: P[] = [{ "x": 1, "y": 2 }, { "x": 3, "y": 4 }]
val b: P[] = [{ "x": 1, "y": 2 }, { "x": 3, "y": 4 }]
val c: P[] = [{ "x": 1, "y": 2 }, { "x": 9, "y": 4 }]
print("${a == b}")
print("${a == c}")
"#);
    assert_eq!(out, vec!["true", "false"]);
}

#[test]
fn test_sealed_array_in_loop_build_drop() {
    // Build + read + drop a fresh sealed array each iteration of a non-tail-recursive driver: the
    // array drop is a single free (scalar-only record). ASan-gated in CI; functional + deterministic
    // here. (Exercises lin_sealed_array_alloc + per-element struct release + lin_array_release.)
    let out = run(r#"
import { print } from "std/io"
type Point = { "x": Int32, "y": Int32 }
val build = (i: Int32): Int32 =>
  val pts: Point[] = [{ "x": i, "y": i + 1 }, { "x": i + 2, "y": i + 3 }]
  pts[0]["x"] + pts[1]["y"]
val loop = (i: Int32, acc: Int32): Int32 =>
  if i == 0 then acc else loop(i - 1, acc + build(i))
print("${loop(1000, 0)}")
"#);
    // sum over i in 1..=1000 of (i + (i+3)) = sum(2i+3) = 2*500500 + 3000 = 1004000.
    assert_eq!(out, vec!["1004000"]);
}

#[test]
fn test_nested_string_record_array_push_iter() {
    // REGRESSION (RAPTOR `Trip { stopTimes: StopTime[] }`): a packed record whose element has a
    // NESTED record-array field (`StopTime[]`) where the nested element carries a HEAP (String) field
    // — built via `push` of a AnyVal object literal, then iterated with the outer/inner `.for(...)`.
    // The push path projects the AnyVal object into the packed Trip layout; for the `stopTimes` array
    // field it must PROJECT the boxed `Object[]` into a packed `StopTime[]` buffer (not store the
    // boxed array verbatim). Storing it verbatim made the later materialize-on-read interpret the
    // boxed array's element pointers as inline packed bytes → a misaligned String deref crash
    // (string.rs `address must be a multiple of 0x4 but is 0x7`). Both the String-field and the
    // scalar-only nested element are exercised (the scalar push path was ALSO crashing on master).
    let out = run(r#"
import { print } from "std/io"
import { push } from "std/array"
import { for } from "std/iter"
type StopTime = { "stop": String, "arr": Int32, "dep": Int32 }
type Trip = { "id": Int32, "routeId": String, "stopTimes": StopTime[] }
val tripsByRoute: Trip[] = []
push(tripsByRoute, { "id": 1, "routeId": "R1", "stopTimes": [
  { "stop": "A", "arr": 0, "dep": 100 },
  { "stop": "B", "arr": 200, "dep": 250 },
  { "stop": "C", "arr": 400, "dep": 0 }
] })
push(tripsByRoute, { "id": 2, "routeId": "R2", "stopTimes": [
  { "stop": "A", "arr": 0, "dep": 500 },
  { "stop": "D", "arr": 700, "dep": 0 }
] })
var totalArr = 0
var totalDep = 0
var stopCount = 0
tripsByRoute.for((t) => t["stopTimes"].for((st) => totalArr = totalArr + st["arr"]))
tripsByRoute.for((t) => t["stopTimes"].for((st) => totalDep = totalDep + st["dep"]))
tripsByRoute.for((t) => t["stopTimes"].for((st) => stopCount = stopCount + 1))
print("arr=${totalArr} dep=${totalDep} stops=${stopCount}")
"#);
    // arr = 0+200+400+0+700 = 1300; dep = 100+250+0+500+0 = 850; stops = 5.
    assert_eq!(out, vec!["arr=1300 dep=850 stops=5"]);
}

#[test]
fn test_nested_sealed_array_field_direct_index() {
    // REGRESSION (RAPTOR `routeScanner` hot path): DIRECT INDEXED access of a nested packed
    // sealed-record-array field — `t["stopTimes"][i]["departureTime"]` where `t: Trip` and
    // `Trip = { …, stopTimes: StopTime[] }`. The repr pass's `FieldGet`/`SealedArrayFieldGet`
    // analyze arms previously did NOT classify a nested sealed-ARRAY field (only scalar / sum /
    // sealed-struct), so `t["stopTimes"]` folded to `Boxed(Opaque)` while the old gate predicate +
    // codegen read it `Packed(sealed array)` — a `repr.rs` oracle disagreement (debug panic) and a
    // release-build SEGFAULT (codegen reads packed, repr says boxed → garbage pointer). Distinct
    // from the `.for()` iteration path: this is the indexed read the scanner uses.
    let out = run(r#"
import { print } from "std/io"
import { toString } from "std/string"
import { push } from "std/array"
type StopTime = { "stop": String, "arrivalTime": Int32, "departureTime": Int32 }
type Trip = { "tripId": String, "stopTimes": StopTime[] }
val trips: Trip[] = []
push(trips, { "tripId": "t1", "stopTimes": [
  { "stop": "A", "arrivalTime": 5, "departureTime": 7 },
  { "stop": "B", "arrivalTime": 20, "departureTime": 22 }
] })
val t: Trip = trips[0]
val st0: StopTime = t["stopTimes"][0]
val st1: StopTime = t["stopTimes"][1]
print("${st0["departureTime"]} ${st1["arrivalTime"]} ${st0["stop"]} ${st1["stop"]}")
print(toString(t["stopTimes"][1]["departureTime"]))
"#);
    assert_eq!(out, vec!["7 20 A B", "22"]);
}

#[test]
fn test_sealed_array_regression_flat_scalar_array_unchanged() {
    // REGRESSION: a flat scalar Int32[] (NOT a sealed-record array) must keep its flat
    // representation and behavior — the new SEALED_ARRAY_TAG path must not perturb flat arrays.
    let out = run(r#"
import { print } from "std/io"
import { length } from "std/array"
val nums: Int32[] = [3, 1, 4, 1, 5, 9]
print("${nums[0]} ${nums[5]} ${length(nums)}")
val sum = (a: Int32[], i: Int32, acc: Int32): Int32 =>
  if i == length(a) then acc else sum(a, i + 1, acc + a[i])
print("${sum(nums, 0, 0)}")
"#);
    assert_eq!(out, vec!["3 9 6", "23"]);
}

#[test]
fn test_sealed_array_regression_heap_field_records_stay_boxed() {
    // A `Person[]` where Person has a STRING field is NOT a Stage-3 sealed-scalar array (heap-field
    // element → deferred to Stage 3b), so it stays a boxed Object[] and must still index/serialize
    // correctly. This proves the fail-safe gate keeps heap-field element arrays on the boxed path.
    let out = run(r#"
import { print } from "std/io"
import { toString } from "std/string"
type Person = { "name": String, "age": Int32 }
val ps: Person[] = [{ "name": "ann", "age": 30 }, { "name": "bob", "age": 41 }]
print("${ps[0]["name"]} ${ps[0]["age"]}")
print("${ps[1]["name"]} ${ps[1]["age"]}")
print(toString(ps))
"#);
    assert_eq!(
        out,
        vec![
            "ann 30",
            "bob 41",
            r#"[{"name": "ann", "age": 30}, {"name": "bob", "age": 41}]"#
        ]
    );
}

// Regression: a SEALED-record array returned through a `[T[], Int32]` FixedArray TUPLE, then
// destructured and read with the fused field-get `arr[i]["field"]`. The tuple element type is a
// union/Json slot, so the `T[]` was coerced into it via `compile_ir_coerce`. That coerce used to
// O(n)-MATERIALIZE the sealed array into a 0xFF dynamic-tagged `Object[]` (each element a boxed
// LinMap) — which (a) the typed fused field-get `arr[i].field` could not read (it only handled
// 0xFE/0xFD), so it SEGFAULTED, and (b) deep-copied every element into a LinMap and re-copied on
// each access (the RAPTOR `create()` PREP-phase memory blowup). Fix: box the sealed array pointer
// directly (keep-packed 0xFD/0xFE) into the union slot; generic consumers materialize-on-read via
// `lin_array_get_tagged`, and a coerce back to `T[]` is an O(1) keep-packed retain. `run()` asserts
// a clean exit, so this guards correct values AND RC balance (ASan-verified leak/UAF-free). Exercises
// BOTH the rebuild path (`.map()` source) and a heap field (`"n"`), plus the sibling scalar element.
// Kept UN-batched (RC-correctness isolation test).
#[test]
fn test_sealed_record_array_through_tuple_fused_field_get() {
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map, range, for } from "std/iter"
type R = { "a": Int32, "n": { "x": Int32 } }
val mk = (): [R[], Int32] =>
  val arr: R[] = range(0, 5).map(i => { "a": i, "n": { "x": i * 10 } })
  [arr, 7]
val main = () =>
  val [arr, z] = mk()
  var sum = 0
  range(0, 5).for(i => sum = sum + arr[i]["a"] + arr[i]["n"]["x"])
  print(toString(sum))
  print(toString(z))
main()
"#);
    // sum = Σ_{i=0..4} (i + i*10) = 11 * (0+1+2+3+4) = 110 ; z = 7.
    assert_eq!(out, vec!["110", "7"]);
}

#[test]
fn test_sealed_array_push_scalar_record() {
    // REGRESSION (heap-corruption bug): pushing a record into a sealed SCALAR-record array.
    // A `Pt[]` is a contiguous, header-less, packed-scalar-stride buffer (`lin_sealed_array_alloc`,
    // elem_tag 0xFE). Before the fix the monomorphized `push$<Pt>` body materialized a BOXED
    // LinObject and tagged-pushed it (TAG_OBJECT) into the packed buffer → pointer-sized write into
    // a scalar-stride slot → `realloc(): invalid next size` / ASan heap-buffer-overflow in
    // lin_array_push. Now `Intrinsic::Push` routes a sealed-array receiver to
    // `lin_sealed_array_push_struct_retaining` (contiguous payload copy). Stage-3 tests dodged this
    // by using array LITERALS only.
    let out = run(r#"
import { print } from "std/io"
import { push, length } from "std/array"
type Pt = { "x": Int32, "y": Int32 }
val pts: Pt[] = [{ "x": 0, "y": 0 }]
push(pts, { "x": 1, "y": 10 })
push(pts, { "x": 2, "y": 20 })
push(pts, { "x": 3, "y": 30 })
push(pts, { "x": 4, "y": 40 })
push(pts, { "x": 5, "y": 50 })
print("${length(pts)} ${pts[0]["x"]} ${pts[5]["y"]} ${pts[3]["x"]}")
"#);
    assert_eq!(out, vec!["6 0 50 3"]);
}

#[test]
fn test_sealed_array_push_scalar_record_into_empty() {
    // `val a: Pt[] = []; push(a, {..})` over a scalar-only sealed array — the element value arrives
    // as a standalone sealed-struct pointer and must be copied into the contiguous layout, not
    // boxed-and-tagged-pushed. Before the fix this printed garbage / crashed.
    let out = run(r#"
import { print } from "std/io"
import { push, length } from "std/array"
type Point = { "x": Int32, "y": Int32 }
val pts: Point[] = []
push(pts, { "x": 1, "y": 2 })
push(pts, { "x": 3, "y": 4 })
print("${length(pts)}")
print("${pts[0]["x"]} ${pts[1]["y"]}")
"#);
    assert_eq!(out, vec!["2", "1 4"]);
}

#[test]
fn test_sealed_array_push_past_grow_boundary() {
    // Push enough scalar records to force several `lin_sealed_array_push_slot` realloc grows (cap
    // doubles from 4). Each grow reallocs the packed-scalar buffer; a tagged push would overflow it.
    let out = run(r#"
import { print } from "std/io"
import { push, length } from "std/array"
type Pt = { "x": Int32, "y": Int32 }
val pts: Pt[] = []
val build = (i: Int32): Int32 =>
  if i == 50 then 0 else
    val _ = push(pts, { "x": i, "y": i * 10 })
    build(i + 1)
val _ = build(0)
print("${length(pts)} ${pts[0]["x"]} ${pts[49]["y"]} ${pts[25]["x"]}")
"#);
    // length 50; pts[0].x = 0; pts[49].y = 490; pts[25].x = 25.
    assert_eq!(out, vec!["50 0 490 25"]);
}

#[test]
fn test_sealed_array_push_float64_record() {
    // A Float64-field scalar record array: the packed stride is 8-byte doubles. Push must copy the
    // 8-byte-per-field payload into the contiguous slot, not box-and-tag-push.
    let out = run(r#"
import { print } from "std/io"
import { push, length } from "std/array"
type Vec2 = { "x": Float64, "y": Float64 }
val vs: Vec2[] = [{ "x": 1.5, "y": 2.5 }]
push(vs, { "x": 3.25, "y": 4.75 })
push(vs, { "x": 5.5, "y": 6.5 })
print("${length(vs)} ${vs[0]["x"]} ${vs[2]["y"]} ${vs[1]["x"]}")
"#);
    assert_eq!(out, vec!["3 1.5 6.5 3.25"]);
}

#[test]
fn test_sealed_array_push_heap_field_records_stay_boxed() {
    // REGRESSION: a heap-field record array (`Person` with a String field) is NOT a Stage-3 sealed
    // scalar array, so `push` must keep using the boxed `Object[]` path and still index/serialize
    // correctly. Proves the Push routing gate (`sealed_array_elem`) does not perturb boxed arrays.
    let out = run(r#"
import { print } from "std/io"
import { push, length } from "std/array"
import { toString } from "std/string"
type Person = { "name": String, "age": Int32 }
val ps: Person[] = [{ "name": "ann", "age": 30 }]
push(ps, { "name": "bob", "age": 41 })
push(ps, { "name": "cat", "age": 7 })
print("${length(ps)} ${ps[0]["name"]} ${ps[2]["age"]}")
print(toString(ps))
"#);
    assert_eq!(
        out,
        vec![
            "3 ann 7",
            r#"[{"name": "ann", "age": 30}, {"name": "bob", "age": 41}, {"name": "cat", "age": 7}]"#
        ]
    );
}

#[test]
fn test_sealed_array_push_regression_flat_int_array_unchanged() {
    // REGRESSION: pushing into a flat Int32[] must keep the flat representation (lin_push_dyn path),
    // unaffected by the sealed-array Push routing.
    let out = run(r#"
import { print } from "std/io"
import { push, length } from "std/array"
val nums: Int32[] = [3, 1, 4]
push(nums, 1)
push(nums, 5)
push(nums, 9)
print("${length(nums)} ${nums[0]} ${nums[5]}")
"#);
    assert_eq!(out, vec!["6 3 9"]);
}

#[test]
fn test_nested_sealed_array_as_outer_array_element_read() {
    // REGRESSION: indexing into a `P[][]` (outer array whose elements are pointer-backed sealed
    // arrays) panicked the repr oracle with a `SealedArrayFieldGet(array packed)` disagreement.
    // The `Index` seed in `repr.rs` was missing the `sealed_array_elem(result_ty)` arm: when the
    // result of `nest[0]` is `P[]`, the dst temp folded to `Boxed(Opaque)` while the old gate
    // predicate read it `Packed(sealed array)` — a debug panic and a release UAF.
    // Fixed by seeding `Packed(PackedSealedArray)` when `result_ty` is a sealed array.
    let out = run(r#"
import { print } from "std/io"
import { push } from "std/array"
type P = { "x": Int32, "y": Int32 }
val main = () =>
  var arr: P[] = [{ "x": 1, "y": 2 }]
  push(arr, { "x": 9, "y": 9 })
  var nest: P[][] = []
  push(nest, arr)
  print("n=${nest[0][1]["x"]}")
main()
"#);
    assert_eq!(out, vec!["n=9"]);
}

#[test]
fn test_nested_sealed_array_outer_element_share_on_push() {
    // REGRESSION + share-on-push (D5): when a `P[]` is pushed into a `P[][]`, the outer slot holds
    // the SHARED pointer to the inner array. Mutations through the inner binding are visible through
    // the outer slot (D5 share-always for pointer-backed sealed arrays).
    let out = run(r#"
import { print } from "std/io"
import { push } from "std/array"
type P = { "x": Int32, "y": Int32 }
val main = () =>
  var arr: P[] = [{ "x": 1, "y": 2 }]
  push(arr, { "x": 3, "y": 4 })
  var nest: P[][] = []
  push(nest, arr)
  // arr and nest[0] share the same P[]; bind a local alias out of the outer slot
  val inner: P[] = nest[0]
  push(inner, { "x": 5, "y": 6 })
  // all three aliases see the push: arr, nest[0], and inner are the same array
  print("arr_len=${arr[2]["x"]}")
  print("nest_len=${nest[0][2]["x"]}")
  print("inner_len=${inner[2]["x"]}")
main()
"#);
    // All three aliases share the same backing P[]; the push through `inner` is visible to all.
    assert_eq!(out, vec!["arr_len=5", "nest_len=5", "inner_len=5"]);
}

#[test]
fn test_sealed_array_index_set_in_callee() {
    // REGRESSION: `arr[i] = { .. }` over a SCALAR sealed-record array, performed inside a CALLEE
    // (recursive overwrite loop). In a callee context the RHS structural literal is typed as an
    // UNSEALED `{x,y}` object and lowered to a BOXED `lin_object_alloc`, not a packed sealed struct.
    // `compile_ir_index_set` passes the value straight to `lin_sealed_array_set`, which memcpy's
    // `value + SEALED_HEADER` into the slot — reading garbage from a boxed object's header. The fix
    // projects a representation-mismatched RHS into a fresh sealed struct first (and releases it after
    // the set takes its retained copy). Without it this read garbage / crashed.
    let out = run(r#"
import { print } from "std/io"
import { length } from "std/array"
type Pt = { "x": Int32, "y": Int32 }
val overwrite = (arr: Pt[], i: Int32): Int32 =>
  if i == length(arr) then 0 else
    val _ = arr[i] = { "x": i * 2, "y": i * 3 }
    overwrite(arr, i + 1)
val main = (): Null =>
  val pts: Pt[] = [{ "x": 0, "y": 0 }, { "x": 1, "y": 1 }, { "x": 2, "y": 2 }]
  val _ = overwrite(pts, 0)
  print("${pts[0]["x"]} ${pts[1]["y"]} ${pts[2]["x"]}")
val _ = main()
"#);
    assert_eq!(out, vec!["0 3 4"]);
}

#[test]
fn test_empty_array_literal_adopts_dotcall_param_repr() {
    // REGRESSION (ADR-062 representation drift): an inferred EMPTY array literal `[]` infers
    // bottom-up to `Array(Never)` and lowers to a BOXED buffer; a concrete packed/flat-scalar `T[]`
    // param's callee does packed stride-N push/get → a producer/consumer representation DRIFT (latent
    // packed-array UAF). `infer_call` already routed an array-literal ARGUMENT through expected-type
    // checking against a concrete array param; `infer_dot_call` did NOT (neither for the arg nor the
    // RECEIVER), so `[].fill()` / `src.scan(.., [])` over a packed `Pt[]` stayed boxed → garbage
    // stride. Both the dot-call ARG and the dot-call RECEIVER now adopt the param's resolved element
    // representation. Exercises both: a `[]` argument AND a `[]` receiver into a packed `Pt[]` param.
    let out = run(r#"
import { print } from "std/io"
import { push, length } from "std/array"
import { toString } from "std/string"
type Pt = { "x": Int32, "y": Int32 }
val fillArg = (acc: Pt[]): Pt[] =>
  push(acc, { "x": 1, "y": 2 })
  acc
val fillRecv = (acc: Pt[]): Pt[] =>
  push(acc, { "x": 3, "y": 4 })
  acc
val main = (): Null =>
  // `[]` as a dot-call ARGUMENT (`x.fillArg(...)` desugars `fillArg(x, [])`? no — receiver is the
  // array): drive the ARG path via a prefix-style dot with the empty literal as the receiver, and
  // the RECEIVER path via `[].fillRecv()`.
  val a = [].fillArg()
  val b = [].fillRecv()
  print("${toString(length(a))} ${toString(a[0]["x"])} ${toString(b[0]["y"])}")
val _ = main()
"#);
    assert_eq!(out, vec!["1 1 4"]);
}

/// Regression (LIN_ISSUES #2): a top-level mutable `var` in an IMPORTED module, mutated by an
/// EXPORTED function, used to panic codegen ("Binary: undefined lhs temp Temp(0)") because the
/// import lowering never set up the module global / its initialiser — the exported mutator
/// referenced an SSA temp that the (non-existent) `main` would have produced. The var must now be
/// a once-initialised module global with shared, persistent state across exported entry points.
#[test]
fn test_imported_module_var_mutated_by_export() {
    let lin_bin = lin_bin();
    if !lin_bin.exists() {
        eprintln!("SKIP test_imported_module_var_mutated_by_export: lin binary not built");
        return;
    }

    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let dir = ws.join(format!("target/lin_impvar_{}", id));
    let _ = fs::create_dir_all(&dir);
    let src_path = dir.join("main.lin");
    let bin_path = dir.join("impvar_bin");

    // Imported module: non-zero-initialised top-level `var`, an exported mutator that
    // increments it, and an exported reader that observes the shared persistent state.
    fs::write(dir.join("counter.lin"),
        r#"var counter = 10
export val nextId = (): Int32 =>
  counter = counter + 1
  counter
export val peek = (): Int32 => counter
"#).unwrap();

    fs::write(&src_path,
        r#"import { nextId, peek } from "./counter"
import { print } from "std/io"
import { toString } from "std/string"
print("${toString(peek())} ${toString(nextId())} ${toString(nextId())} ${toString(peek())}")
"#).unwrap();

    let compile = Command::new(&lin_bin)
        .args(["build", src_path.to_str().unwrap(), "-o", bin_path.to_str().unwrap()])
        .current_dir(&ws)
        .output()
        .expect("failed to invoke lin binary");
    assert!(
        compile.status.success(),
        "imported-var program compilation failed:\nstderr: {}\nstdout: {}",
        String::from_utf8_lossy(&compile.stderr),
        String::from_utf8_lossy(&compile.stdout),
    );

    let run_out = Command::new(&bin_path).output().expect("failed to run compiled binary");
    let _ = fs::remove_dir_all(&dir);
    assert!(
        run_out.status.success(),
        "runtime error:\nstderr: {}",
        String::from_utf8_lossy(&run_out.stderr),
    );
    let stdout = String::from_utf8_lossy(&run_out.stdout);
    let lines: Vec<String> = stdout.lines().filter(|l| !l.is_empty()).map(|l| l.to_string()).collect();
    // peek=10 (init respected), then two increments to 11 and 12, then peek sees the shared 12.
    assert_eq!(lines, vec!["10 11 12 12"]);
}

#[test]
fn test_cli_args_read_in_compiled_binary() {
    // Regression: a compiled `lin build` binary can read its command-line arguments
    // via std/io.args(). args() excludes argv[0] and returns the user args in order.
    let src = r#"
import { args, print } from "std/io"
import { for } from "std/iter"
import { length } from "std/array"
import { toString } from "std/string"
val a = args()
print("count=${toString(length(a))}")
a.for(x => print(x))
"#;
    let out = run_with_args(src, &["alpha", "beta", "gamma"]);
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        vec!["count=3", "alpha", "beta", "gamma"]
    );
}

#[test]
fn test_cli_args_empty_when_none_passed() {
    let src = r#"
import { args, print } from "std/io"
import { length } from "std/array"
import { toString } from "std/string"
print("count=${toString(length(args()))}")
"#;
    let out = run_with_args(src, &[]);
    assert_eq!(out, "count=0");
}

// --- Projection value-semantics / use-after-free regression (feat/value-semantics-cow) ---

// Stage A: a `val x = container[k]` projection must materialize an OWNED, container-independent
// value (a snapshot of the slot's tag+payload). Before the fix, the union/AnyVal projection bound a
// raw INTERIOR pointer into the container's entries buffer; growing the object (inline→heap
// migration as more keys are added) reallocs that buffer, leaving the binding dangling — a
// use-after-free that crashed in `lin_array_push` (array.rs) with a null pointer deref. After the
// fix the binding holds a stable header, so growing `results` no longer dangles `bC`/`bB`/`bA`.
#[test]
fn test_projection_uaf_object_grow_does_not_dangle() {
    let src = r#"
import { for } from "std/iter"
import { print } from "std/io"
import { keys } from "std/object"
import { length, push } from "std/array"

val main = () =>
  var results: AnyVal = {}

  results["C"] = []
  val bC = results["C"]
  bC.for(n => null)

  results["B"] = []
  val bB = results["B"]
  bB.for(n => null)

  results["A"] = []
  val bA = results["A"]
  bA.for(n => null)

  push(bA, { "label": "A" })
  push(bB, { "label": "B" })
  push(bC, { "label": "C" })

  print(
    "done keys=${length(keys(results))} C=${length(bC)} B=${length(bB)} A=${length(bA)}"
  )

main()
"#;
    let out = run(src);
    assert_eq!(out, vec!["done keys=3 C=1 B=1 A=1".to_string()]);
}

// A projected binding `val x = obj[k]` is a SHARED REFERENCE to the stored value, not a snapshot:
// mutating through it (push) updates what's stored in the container — and this is CONSISTENT with
// passing the projection to a function (Lin is call-by-sharing, so `f(obj[k])` mutating its param
// also updates the container). The UAF fix (projection materializes a stable owned box via
// lin_tagged_clone, a SHALLOW copy retaining the same underlying array) preserves these
// shared-reference semantics. This locks that in: fn-call and projection paths must agree, and a
// container-grow between projection and mutation must stay safe.
#[test]
fn test_projection_shared_reference_consistent_with_fn_call() {
    let src = r#"
import { print } from "std/io"
import { push, length } from "std/array"

val mutate = (a: AnyVal): Null =>
  push(a, 99)

val main = () =>
  var o1 = { "a": [1, 2] }
  mutate(o1["a"])
  print("fn=${length(o1["a"])}")

  var o2 = { "a": [1, 2] }
  val x = o2["a"]
  push(x, 99)
  print("proj=${length(o2["a"])}")

  var o3 = { "a": [1, 2] }
  val y = o3["a"]
  o3["b"] = [0]
  o3["c"] = [0]
  o3["d"] = [0]
  push(y, 99)
  print("grow=${length(y)}/${length(o3["a"])}")

main()
"#;
    let out = run(src);
    // Both paths mutate the shared array (length 3); the grow case stays safe and still shared.
    assert_eq!(out, vec!["fn=3".to_string(), "proj=3".to_string(), "grow=3/3".to_string()]);
}

#[test]
fn test_typed_map_through_function_value() {
    // Regression: a typed index-signature map (`{ String: T }`, `Type::Map`) passed through an
    // opaque `Function`-value call must survive the closure-ABI wrapper. The wrapper calls
    // `unbox_value`, which previously omitted `Type::Map` from its pointer-unboxing arm, so the
    // callee received a TAG_MAP TaggedVal box instead of the raw `LinMap*` — flattening the map
    // (flat read -> -1) or crashing on a null-pointer deref in lin-runtime/src/map.rs.
    let src = r#"
import { print } from "std/io"
import { keys } from "std/object"
import { length } from "std/array"

val buildFlat = (): { String: Int32 } =>
  var m: { String: Int32 } = {}
  m["x"] = 7
  m

val readX = (m: { String: Int32 }): Int32 =>
  val v = m["x"]
  match v
    is Null => -1
    else => v

val applyFlat = (fn: Function, m: { String: Int32 }): Int32 =>
  fn(m)

val buildNested = (): { String: { String: Int32 } } =>
  var m: { String: { String: Int32 } } = {}
  var inner: { String: Int32 } = {}
  inner["a"] = 1
  inner["b"] = 2
  m["k"] = inner
  m

val countInner = (m: { String: { String: Int32 } }): Int32 =>
  val inner = m["k"]
  match inner
    is Null => -1
    else => length(keys(inner))

val applyNested = (fn: Function, m: { String: { String: Int32 } }): Int32 =>
  fn(m)

val main = (): Null =>
  val f = buildFlat()
  print("flat=${applyFlat(readX, f)}")
  val n = buildNested()
  print("nested=${applyNested(countInner, n)}")

main()
"#;
    let out = run(src);
    assert_eq!(out, vec!["flat=7".to_string(), "nested=2".to_string()]);
}

#[test]
fn test_json_object_field_used_as_typed_map() {
    // Regression: a `{}` that is a FIELD of a AnyVal object literal is physically a `LinObject`, but
    // reading it back and using it where a `{ String: T }` map is expected (e.g. passing it to
    // `std/object.get`, or to a `{ String: Int32 }` parameter) used to call the map accessors
    // (`lin_map_get`/`_set`) on a `LinObject*` — `find_slot` probed its bytes as a hash table and
    // INFINITE-LOOPED on an absent key (and corrupted the heap on a present one). The fix: the
    // AnyVal/Object → Map coercion materializes a real `LinMap` (tag-dispatched: an already-map value
    // is retained as-is, an object is rebuilt), plus a defensive probe bound in `find_slot`.
    let src = r#"import { print } from "std/io"
import { get } from "std/object"
import { toString } from "std/string"

val mk = (): AnyVal => { "listeners": {  }, "n": 0 }

val readVia = (m: { String: Int32 }, k: String): Int32 =>
  get(m, k, -1)

val main = (): Null =>
  val b = mk()
  b["listeners"]["tick"] = 1
  // present key via the typed-map parameter (object → map materialize)
  print("present=${toString(readVia(b["listeners"], "tick"))}")
  // ABSENT key — this is the call that used to hang forever
  print("absent=${toString(readVia(b["listeners"], "zzz"))}")

main()
"#;
    let out = run(src);
    assert_eq!(out, vec!["present=1".to_string(), "absent=-1".to_string()]);
}

// Regression (RAPTOR `routeScanner.scanBack` per-scan leak, ~227 MB/scan → ~6 MB/scan): a
// TAIL-RECURSIVE function that allocates fresh owned temps each iteration (the projections
// `scanner["tripsByRoute"][routeId]`, `routeTrips[i]`, the route-id string literal) and threads
// `found: AnyVal` set to either a BORROWED projection (`arr[i]`) or the passed-through param. The
// body-scope-exit releases for those per-iteration temps landed in the unreachable `tco_post`
// continuation block (the back-edge means scope exit is never reached) and leaked every iteration.
// The array-index projection ALSO leaked: codegen's `lin_array_get_tagged` returns a FRESH +1 box,
// but the IR cloned it again as if borrowed, leaking the original box once per scanned element.
// Both are fixed in lin-ir lowering (release body-owned temps on the live block before the
// TailCall; register the fresh array-index box owned directly instead of re-cloning). ASan is the
// leak guard (CI asan job); this test pins CORRECTNESS — the threaded borrowed projection is
// returned right, and the durable source array (whose elements the projection borrows) SURVIVES the
// scan intact (no double-free of `tripsByRoute[routeId][i]`).
#[test]
fn test_tail_recursive_borrowed_projection_threading_durable_source_survives() {
    let src = r#"
import { print } from "std/io"
import { length } from "std/array"

// runsOn-style conditional choosing a borrowed projection (`trip`) or the passed-through param.
val scanBack = (scanner: AnyVal, routeId: String, stopIndex: Int32, time: AnyVal, i: Int32, found: AnyVal): AnyVal =>
  if i < 0 then
    found
  else
    val routeTrips = scanner["tripsByRoute"][routeId]
    val trip = routeTrips[i]
    val stopTime = trip["stopTimes"][stopIndex]
    if stopTime["departureTime"] < time then
      found
    else
      val newFound = if trip["ok"] > 0 then trip else found
      // stateful memo write under the documented condition (an object-set into a durable map)
      if newFound == null || newFound["id"] == trip["id"] then
        scanner["scanPos"][routeId] = i
      scanBack(scanner, routeId, stopIndex, time, i - 1, newFound)

val makeScanner = (): AnyVal =>
  {
    "tripsByRoute": {
      "R1": [
        { "id": 10, "ok": 1, "stopTimes": [ { "departureTime": 100 } ] },
        { "id": 20, "ok": 0, "stopTimes": [ { "departureTime": 110 } ] },
        { "id": 30, "ok": 1, "stopTimes": [ { "departureTime": 120 } ] }
      ]
    },
    "scanPos": {}
  }

// Drive the scan many times (the leak was per-iteration; the loop is itself tail recursive so it
// exercises the same body-scope-release path for its own discarded result).
val loop = (scanner: AnyVal, n: Int32, acc: Int32): Int32 =>
  if n <= 0 then acc
  else
    val r = scanBack(scanner, "R1", 0, 50, 2, null)
    loop(scanner, n - 1, acc + r["id"])

val main = (): Null =>
  val scanner = makeScanner()
  // departureTime (100..) is never < time (50), so every trip is scanned: found = first ok trip
  // walking backward from i=2 → id 30 then 10 → ends at 10.
  val total = loop(scanner, 1000, 0)
  print("found=${total}")
  // The durable source array's elements (borrowed by the projection) must survive intact.
  val trips = scanner["tripsByRoute"]["R1"]
  print("durable len=${length(trips)} first=${trips[0]["id"]} last=${trips[2]["id"]}")

main()
"#;
    let out = run(src);
    assert_eq!(
        out,
        vec![
            "found=10000".to_string(),
            "durable len=3 first=10 last=30".to_string(),
        ]
    );
}

// ---------------------------------------------------------------------------
// Unboxed-sum-type Stage 0 — checker warm-up (type-check-only) fixes.
// Gap 3: `infer_index` now resolves a `Type::Named` (or a Named-aliased union)
//        to its record/union body before indexing.
// Gap 2: an `is V` arm per variant of a discriminated union counts as exhaustive
//        coverage (matched by StrLit discriminant), so no redundant `else` is
//        required — while a genuinely missing variant STILL errors.
// ---------------------------------------------------------------------------

#[test]
fn test_st0_index_named_record_value() {
    // Gap 3: a value whose static type is a named record (`Node`) can be indexed.
    // The `build` fn returns `Node` (a `Type::Named`/record); `r["node"]` previously
    // hit the `_ => "Cannot index into type Node"` arm. Now it resolves + indexes.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Node = { "tag": String, "node": Int32 }
val build = (n: Int32): Node =>
  if n <= 0 then { "tag": "leaf", "node": 0 }
  else { "tag": "branch", "node": n }
val r: Node = build(7)
print(r["node"].toString())
"#);
    assert_eq!(out, vec!["7"]);
}

#[test]
fn test_st0_index_recursive_named_child_field() {
    // Gap 3 (recursive shape, the interp `Ast`): `node["left"]` is typed `Ast`
    // (a Named alias resolving to `Num | BinOp`); indexing `["value"]` over it
    // resolves the alias + each variant. `value` is present on `Num` but not on
    // `BinOp`, so the precise result is `Int32 | Null` (the safe-bracket Null) —
    // the program guards the Null and runs.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Num   = { "kind": "num", "value": Int32 }
type BinOp = { "kind": "op",  "op": String, "left": Ast, "right": Ast }
type Ast   = Num | BinOp
val leftVal = (node: BinOp): Int32 =>
  val v = node["left"]["value"]
  if v == null then -1 else v
val n: BinOp = { "kind": "op", "op": "+", "left": { "kind": "num", "value": 3 }, "right": { "kind": "num", "value": 4 } }
print(leftVal(n).toString())
"#);
    assert_eq!(out, vec!["3"]);
}

#[test]
fn test_st0_match_union_variants_exhaustive_no_else_2variant() {
    // Gap 2: `is Num / is BinOp` covers every variant of `Ast = Num | BinOp` →
    // exhaustive WITHOUT an `else`. Previously flagged non-exhaustive.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Num   = { "kind": "num", "value": Int32 }
type BinOp = { "kind": "op",  "value": Int32 }
type Ast   = Num | BinOp
val classify = (x: Ast): Int32 =>
  match x
    is Num => 1
    is BinOp => 2
val n: Ast = { "kind": "num", "value": 5 }
print(classify(n).toString())
"#);
    assert_eq!(out, vec!["1"]);
}

#[test]
fn test_st0_match_union_variants_exhaustive_no_else_3variant() {
    // Gap 2, 3-variant discriminated union — all covered → exhaustive, no `else`.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type A = { "kind": "a", "x": Int32 }
type B = { "kind": "b", "x": Int32 }
type C = { "kind": "c", "x": Int32 }
type T = A | B | C
val f = (v: T): Int32 =>
  match v
    is A => 1
    is B => 2
    is C => 3
val a: T = { "kind": "c", "x": 9 }
print(f(a).toString())
"#);
    assert_eq!(out, vec!["3"]);
}

#[test]
fn test_st0_match_recursive_union_variants_exhaustive_no_else() {
    // Gap 2 on the recursive interp `Ast`: the variant `BinOp` has recursive
    // `Ast` fields whose expansion depth differs between the pattern-resolved
    // type and the scrutinee union variant. StrLit-discriminant coverage makes
    // `is Num / is BinOp` exhaustive without an `else` regardless of depth.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Num   = { "kind": "num", "value": Int32 }
type BinOp = { "kind": "op",  "op": String, "left": Ast, "right": Ast }
type Ast   = Num | BinOp
val tag = (node: Ast): Int32 =>
  match node
    is Num => 1
    is BinOp => 2
val n: Ast = { "kind": "num", "value": 5 }
print(tag(n).toString())
"#);
    assert_eq!(out, vec!["1"]);
}

#[test]
fn test_st0_match_union_missing_variant_still_errors() {
    // SOUNDNESS GUARD: a genuinely non-exhaustive match (the `BinOp`/`C` variant
    // is not covered and there is no `else`) must STILL be a hard error. The
    // StrLit-discriminant coverage must not turn a partial cover exhaustive.
    let (ok, output) = check_source(r#"import { print } from "std/io"
type A = { "kind": "a", "x": Int32 }
type B = { "kind": "b", "x": Int32 }
type C = { "kind": "c", "x": Int32 }
type T = A | B | C
val f = (v: T): Int32 =>
  match v
    is A => 1
    is B => 2
val a: T = { "kind": "a", "x": 1 }
print(f(a))
"#);
    assert!(!ok, "missing-variant match must fail to type-check");
    assert!(
        output.contains("non-exhaustive"),
        "expected a non-exhaustive error, got:\n{}",
        output
    );
}

#[test]
fn test_st0_match_recursive_union_missing_variant_still_errors() {
    // SOUNDNESS GUARD on the recursive `Ast`: covering only `Num` (omitting
    // `BinOp`) with no `else` must STILL error despite the discriminant logic.
    let (ok, output) = check_source(r#"import { print } from "std/io"
type Num   = { "kind": "num", "value": Int32 }
type BinOp = { "kind": "op",  "op": String, "left": Ast, "right": Ast }
type Ast   = Num | BinOp
val ev = (node: Ast): Int32 =>
  match node
    is Num => node["value"]
val n: Ast = { "kind": "num", "value": 5 }
print(ev(n))
"#);
    assert!(!ok, "recursive missing-variant match must fail to type-check");
    assert!(
        output.contains("non-exhaustive"),
        "expected a non-exhaustive error, got:\n{}",
        output
    );
}

// ---------------------------------------------------------------------------
// Unboxed-sum-type Stage 2 — RECURSIVE sum types pack as unboxed SumNodes.
//
// A `type Ast = Num | BinOp` whose `BinOp` carries recursive `left`/`right : Ast`
// children packs end-to-end: each node is an unboxed heap `SumNode` with the
// recursive children stored as 8-byte owned `*SumNode` pointer slots (KIND_SUMNODE
// in the static SumDesc). Construction packs nested literals directly (no boxed
// round-trip), `match is` dispatches on the inline tag, a recursive-child read is
// a const-offset pointer load (borrowed interior `*SumNode`), and the whole tree
// is freed by the runtime's recursive drop walk. The RC drop (the dominant risk)
// is ASan-verified separately; these assert end-to-end CORRECTNESS.
// ---------------------------------------------------------------------------

#[test]
fn test_st2_recursive_sum_tree_eval() {
    // The interp `Ast`: construct a 2-level tree, dispatch with `match is`, read
    // the recursive children, and recurse. `3 + 4 = 7`.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Num   = { "kind": "num", "value": Int32 }
type BinOp = { "kind": "op",  "left": Ast, "right": Ast }
type Ast   = Num | BinOp
val evalNode = (node: Ast): Int32 =>
  match node
    is Num   => node["value"]
    is BinOp => evalNode(node["left"]) + evalNode(node["right"])
val tree: Ast = { "kind": "op", "left": { "kind": "num", "value": 3 }, "right": { "kind": "num", "value": 4 } }
print(evalNode(tree).toString())
"#);
    assert_eq!(out, vec!["7"]);
}

#[test]
fn test_st2_recursive_sum_deep_tree_with_scalar_field() {
    // A deeper full binary tree whose `BinOp` ALSO carries a scalar `op` field
    // (read directly from the SumNode, not via materialize): `((3+4)*(5+6)) = 77`.
    // Exercises the recursive drop walk over a 7-node tree at scope exit.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Num   = { "kind": "num", "value": Int32 }
type BinOp = { "kind": "op", "op": Int32, "left": Ast, "right": Ast }
type Ast   = Num | BinOp
val evalNode = (node: Ast): Int32 =>
  match node
    is Num   => node["value"]
    is BinOp =>
      val l = evalNode(node["left"])
      val r = evalNode(node["right"])
      if node["op"] == 0 then l + r
      else l * r
val tree: Ast = {
  "kind": "op", "op": 1,
  "left":  { "kind": "op", "op": 0, "left": { "kind": "num", "value": 3 }, "right": { "kind": "num", "value": 4 } },
  "right": { "kind": "op", "op": 0, "left": { "kind": "num", "value": 5 }, "right": { "kind": "num", "value": 6 } }
}
print(evalNode(tree).toString())
"#);
    assert_eq!(out, vec!["77"]);
}

#[test]
fn test_st2_recursive_sum_repeated_build_drop_in_loop() {
    // Build + evaluate + drop a fresh recursive tree on every loop iteration — the
    // strongest non-ASan guard that the recursive drop walk frees each tree exactly
    // once (a leak or double-free corrupts the reused node slots and crashes/garbles
    // a later iteration). Prints `i + 2` for i in 0..4 → 2 3 4 5.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range, for } from "std/iter"
type Num   = { "kind": "num", "value": Int32 }
type BinOp = { "kind": "op", "left": Ast, "right": Ast }
type Ast   = Num | BinOp
val evalNode = (node: Ast): Int32 =>
  match node
    is Num   => node["value"]
    is BinOp => evalNode(node["left"]) + evalNode(node["right"])
range(0, 4).for(i =>
  val t: Ast = { "kind": "op", "left": { "kind": "num", "value": i }, "right": { "kind": "num", "value": 2 } }
  print(evalNode(t).toString()))
"#);
    assert_eq!(out, vec!["2", "3", "4", "5"]);
}

#[test]
fn test_st2_recursive_sum_three_variant() {
    // A 3-variant recursive sum (`Num | Neg | Add`) with a unary recursive child
    // (`Neg.operand`) and a binary one (`Add.left/right`): `10 - 3 = 7`.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Num = { "kind": "num", "value": Int32 }
type Neg = { "kind": "neg", "operand": Expr }
type Add = { "kind": "add", "left": Expr, "right": Expr }
type Expr = Num | Neg | Add
val eval = (e: Expr): Int32 =>
  match e
    is Num => e["value"]
    is Neg => 0 - eval(e["operand"])
    is Add => eval(e["left"]) + eval(e["right"])
val t: Expr = { "kind": "add", "left": { "kind": "num", "value": 10 }, "right": { "kind": "neg", "operand": { "kind": "num", "value": 3 } } }
print(eval(t).toString())
"#);
    assert_eq!(out, vec!["7"]);
}

// ---------------------------------------------------------------------------
// Unboxed-sum-type Stage 2 — TAIL-RETURN construction pushdown.
//
// Regression for the tail-return pushdown bug: a recursive sum literal built in a
// function and RETURNED (the canonical parser shape) — directly, from an `if`/`else`
// tail, or from a `match` arm — must construct its nested recursive children AS
// `SumNode`s (the per-variant expected type is pushed into the children), exactly like
// a `val n: <Sum> = {…}; n` binding. Before the fix the children were boxed as plain
// `LinObject`s and stored into the parent's `*SumNode` child slots, so reading a
// child's discriminant read boxed memory → garbage tag → "non-exhaustive match"
// (ASan-clean, hence missed by the inline-construct-then-traverse Stage-2 tests).
// ---------------------------------------------------------------------------

#[test]
fn test_st2_sum_build_and_return_then_read_child() {
    // (a) Build a recursive BinOp INSIDE a function, return it, then traverse a child.
    // The returned tree's children must be unboxed SumNodes (the disc reads correctly).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Num   = { "kind": "num", "value": Int32 }
type BinOp = { "kind": "op", "op": Int32, "left": Ast, "right": Ast }
type Ast   = Num | BinOp
val build = (): Ast =>
  { "kind": "op", "op": 0, "left": { "kind": "num", "value": 3 }, "right": { "kind": "num", "value": 4 } }
val t: Ast = build()
val r = match t
  is Num   => -1
  is BinOp => match t["left"]
    is Num   => t["left"]["value"]
    is BinOp => -2
print(r.toString())
"#);
    assert_eq!(out, vec!["3"]);
}

#[test]
fn test_st2_sum_if_else_tail_return_then_eval() {
    // (b) An if/else whose tail VALUE is a nested sum literal, returned from a function,
    // then evaluated recursively. `mk(1)` → (3+4) = 7; `mk(0)` → the Num leaf 9.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Num   = { "kind": "num", "value": Int32 }
type BinOp = { "kind": "op", "op": Int32, "left": Ast, "right": Ast }
type Ast   = Num | BinOp
val eval = (e: Ast): Int32 =>
  match e
    is Num   => e["value"]
    is BinOp => eval(e["left"]) + eval(e["right"])
val mk = (n: Int32): Ast =>
  if n == 1 then
    { "kind": "op", "op": 0, "left": { "kind": "num", "value": 3 }, "right": { "kind": "num", "value": 4 } }
  else
    { "kind": "num", "value": 9 }
print(eval(mk(1)).toString())
print(eval(mk(0)).toString())
"#);
    assert_eq!(out, vec!["7", "9"]);
}

#[test]
fn test_st2_sum_match_arm_tail_return_then_eval() {
    // A `match`-arm tail VALUE that is a nested sum literal, returned from a function.
    // `mk(1)` → (3*4) = 12; `mk(0)` → the Num leaf 5.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Num   = { "kind": "num", "value": Int32 }
type BinOp = { "kind": "op", "op": Int32, "left": Ast, "right": Ast }
type Ast   = Num | BinOp
val eval = (e: Ast): Int32 =>
  match e
    is Num   => e["value"]
    is BinOp => eval(e["left"]) * eval(e["right"])
val mk = (n: Int32): Ast =>
  match n
    is 0 => { "kind": "num", "value": 5 }
    else => { "kind": "op", "op": 2, "left": { "kind": "num", "value": 3 }, "right": { "kind": "num", "value": 4 } }
print(eval(mk(1)).toString())
print(eval(mk(0)).toString())
"#);
    assert_eq!(out, vec!["12", "5"]);
}

#[test]
fn test_st2_sum_parser_style_recursive_build_and_return() {
    // (c) A small parser-style function that RECURSIVELY builds and returns a tree:
    // `chain(n)` folds `n` additions of leaf `1` into a left-leaning BinOp spine,
    // returning a fresh sub-tree at each level. Evaluating it sums to `n` (n leaves of 1).
    // Exercises the tail-return pushdown at every recursion depth + the recursive drop.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Num   = { "kind": "num", "value": Int32 }
type BinOp = { "kind": "op", "op": Int32, "left": Ast, "right": Ast }
type Ast   = Num | BinOp
val eval = (e: Ast): Int32 =>
  match e
    is Num   => e["value"]
    is BinOp => eval(e["left"]) + eval(e["right"])
val chain = (n: Int32): Ast =>
  if n <= 1 then
    { "kind": "num", "value": 1 }
  else
    { "kind": "op", "op": 0, "left": chain(n - 1), "right": { "kind": "num", "value": 1 } }
print(eval(chain(1)).toString())
print(eval(chain(4)).toString())
print(eval(chain(10)).toString())
"#);
    assert_eq!(out, vec!["1", "4", "10"]);
}

#[test]
fn test_int64_return_width_literal() {
    // Regression: a suffixless integer literal returned from an `Int64`-declared function — bare,
    // or in an `if`/`match`/block tail — must adopt the declared width (Int64), not the Int32
    // literal default. The checker accepted the widening but codegen emitted an `i32` value into
    // an `i64`-returning function ("ret i32 … i64" / invalid IR). Covers the bare-literal, the
    // nested-if (the std/time daysInMonth shape), and a block-tail form.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val bare = (): Int64 => 28

val nestedIf = (y: Int64, m: Int64): Int64 =>
  if m == 2 then
    if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 then 29 else 28
  else if m == 4 || m == 6 || m == 9 || m == 11 then 30 else 31

val blockTail = (n: Int64): Int64 =>
  val doubled = n * 2
  doubled + 1

print(toString(bare()))
print(toString(nestedIf(2024, 2)))
print(toString(nestedIf(2023, 2)))
print(toString(nestedIf(2024, 1)))
print(toString(blockTail(10)))
"#);
    assert_eq!(output, vec!["28", "29", "28", "31", "21"]);
}

// ───────────────────────────────────────────────────────────────────────────
// unboxed-sumtype Stage 3 — sum values crossing container / `sum|Null` / dynamic
// boundaries. These exercise the materialize-to-boxed boundary (a recursive sum
// value stored into a record/AnyVal field or `{String:sum}` map, passed to a
// `sum|Null` param, or fed to toString) projecting back to a correct SumNode on
// read — the canonical "build a tree, store it, read it back, traverse it, assert
// the CORRECT numeric result" patterns. (Before this fix these crashed / mis-read
// the discriminant: a SumNode pointer stored raw under TAG_OBJECT was read back as
// a LinObject → null-deref / "non-exhaustive match".)
// ───────────────────────────────────────────────────────────────────────────

const ST3_AST_PRELUDE: &str = r#"import { print } from "std/io"
import { toString } from "std/string"
type Num   = { "kind": "num", "value": Int32 }
type BinOp = { "kind": "op", "op": Int32, "left": Expr, "right": Expr }
type Expr  = Num | BinOp
val eval = (e: Expr): Int32 =>
  match e
    is Num => e["value"]
    is BinOp =>
      val l = eval(e["left"])
      val r = eval(e["right"])
      if e["op"] == 0 then l + r
      else if e["op"] == 1 then l - r
      else l * r
"#;

#[test]
fn test_st3_cursor_record_store_read_eval() {
    // (b) A parser-style cursor record `{ "node": Expr, "pos": Int32 }`: store a tree
    // in the `node` field, read it back, traverse it, and read the scalar `pos` field.
    // ((3+4)*(10-6)) = 28; pos = 7.
    let src = format!(
        r#"{ST3_AST_PRELUDE}
type Cursor = {{ "node": Expr, "pos": Int32 }}
val main = () =>
  val tree: Expr = {{
    "kind": "op", "op": 2,
    "left": {{ "kind": "op", "op": 0, "left": {{ "kind": "num", "value": 3 }}, "right": {{ "kind": "num", "value": 4 }} }},
    "right": {{ "kind": "op", "op": 1, "left": {{ "kind": "num", "value": 10 }}, "right": {{ "kind": "num", "value": 6 }} }}
  }}
  val cur: Cursor = {{ "node": tree, "pos": 7 }}
  print(eval(cur["node"]).toString())
  print(cur["pos"].toString())
main()
"#
    );
    let out = run(&src);
    assert_eq!(out, vec!["28", "7"]);
}

#[test]
fn test_st3_map_string_expr_round_trip_eval() {
    // (a) A `{ String: Expr }` map: store a tree under a key, read it back (typed
    // `Expr | Null`), narrow with `is`, and evaluate. (3+4) = 7.
    let src = format!(
        r#"{ST3_AST_PRELUDE}
val evalOpt = (e: Expr | Null): Int32 =>
  match e
    is Num => eval(e)
    is BinOp => eval(e)
    else => 0 - 1
val main = () =>
  val tree: Expr = {{ "kind": "op", "op": 0, "left": {{ "kind": "num", "value": 3 }}, "right": {{ "kind": "num", "value": 4 }} }}
  var m: {{ String: Expr }} = {{}}
  m["root"] = tree
  print(evalOpt(m["root"]).toString())
main()
"#
    );
    let out = run(&src);
    assert_eq!(out, vec!["7"]);
}

#[test]
fn test_keeppacked_map_sumvalue_round_trip_no_uaf() {
    // Regression (ADR-062 Stage 3, double-free): a `{ String: Expr }` map value read into a
    // `val back`, then narrowed by `match back` and passed to `eval(back)`, double-freed the
    // projected SumNode — the narrowed concrete variant flowing into `eval`'s sum param was BOTH
    // released by the owning model (`lin_sumnode_release`) AND classified as a caller-owned box
    // shell / sealed-record materialize (a second release + a mismatched-size box free). An ASan
    // heap-use-after-free at `lin_sumnode_release` (run-correct only because the second free landed
    // in free-list slop). Fixed in `lower.rs` by excluding `sum_arg_projected` from
    // `arg_box_is_caller_owned_shell` / `arg_box_is_caller_owned_scalar_shell` /
    // `sealed_{record,array}_arg_materialized`. Exercises BOTH variant arms (BinOp → 7, Num → 42)
    // and the OVERWRITE case (the old value released exactly once → 99). The ASan gate is the real
    // proof; this documents the shape + run-correctness.
    let src = format!(
        r#"{ST3_AST_PRELUDE}
val main = () =>
  var m: {{ String: Expr }} = {{}}
  m["root"] = {{ "kind": "op", "op": 0, "left": {{ "kind": "num", "value": 3 }}, "right": {{ "kind": "num", "value": 4 }} }}
  val back = m["root"]
  val r = match back
    is Num => eval(back)
    is BinOp => eval(back)
    else => 0 - 1
  print(r.toString())

  // Num-variant arm (this was the arm whose `eval(back)` double-freed under ASan).
  var n: {{ String: Expr }} = {{}}
  n["leaf"] = {{ "kind": "num", "value": 42 }}
  val nb = n["leaf"]
  val nr = match nb
    is Num => eval(nb)
    is BinOp => eval(nb)
    else => 0 - 1
  print(nr.toString())

  // Overwrite: the OLD stored value must be released exactly once on reassignment.
  var o: {{ String: Expr }} = {{}}
  o["k"] = {{ "kind": "op", "op": 0, "left": {{ "kind": "num", "value": 1 }}, "right": {{ "kind": "num", "value": 2 }} }}
  o["k"] = {{ "kind": "num", "value": 99 }}
  val ob = o["k"]
  val or = match ob
    is Num => eval(ob)
    is BinOp => eval(ob)
    else => 0 - 1
  print(or.toString())
main()
"#
    );
    let out = run(&src);
    assert_eq!(out, vec!["7", "42", "99"]);
}

#[test]
fn test_st3_sum_value_through_nullable_param() {
    // A recursive sum value passed to a `sum | Null` parameter must materialize to a
    // real boxed object so the callee's `match` reads the correct discriminant. (3+4) = 7.
    let src = format!(
        r#"{ST3_AST_PRELUDE}
val evalOpt = (e: Expr | Null): Int32 =>
  match e
    is Num => eval(e)
    is BinOp => eval(e)
    else => 0 - 1
val main = () =>
  val tree: Expr = {{ "kind": "op", "op": 0, "left": {{ "kind": "num", "value": 3 }}, "right": {{ "kind": "num", "value": 4 }} }}
  print(evalOpt(tree).toString())
  val leaf: Expr = {{ "kind": "num", "value": 42 }}
  print(evalOpt(leaf).toString())
main()
"#
    );
    let out = run(&src);
    assert_eq!(out, vec!["7", "42"]);
}

#[test]
fn test_st3_same_tree_to_string_materializes_correctly() {
    // (c) The SAME tree fed to `toString` (a genuinely-dynamic consumer) must still
    // MATERIALIZE to a real LinObject and print its fields correctly — not a raw
    // SumNode pointer (which would print garbage). A field read still evaluates to 7.
    let src = format!(
        r#"{ST3_AST_PRELUDE}
val main = () =>
  val tree: Expr = {{ "kind": "op", "op": 0, "left": {{ "kind": "num", "value": 3 }}, "right": {{ "kind": "num", "value": 4 }} }}
  val j: AnyVal = tree
  print(j["kind"].toString())
  print(eval(tree).toString())
main()
"#
    );
    let out = run(&src);
    assert_eq!(out, vec!["op", "7"]);
}

#[test]
fn test_st3_cursor_in_loop_repeated_store_read() {
    // Repeated build → store-in-record → read → eval in a loop: stresses the
    // materialize/project round-trip's RC across many iterations (no leak/UAF scaling,
    // verified separately under ASan). Each iteration evaluates ((i)+(1)) and sums.
    let src = format!(
        r#"{ST3_AST_PRELUDE}
import {{ range, for }} from "std/iter"
type Cursor = {{ "node": Expr, "pos": Int32 }}
val main = () =>
  var total: Int32 = 0
  range(0, 200).for((i) =>
    val tree: Expr = {{ "kind": "op", "op": 0, "left": {{ "kind": "num", "value": i }}, "right": {{ "kind": "num", "value": 1 }} }}
    val cur: Cursor = {{ "node": tree, "pos": i }}
    total = total + eval(cur["node"])
  )
  print(total.toString())
main()
"#
    );
    let out = run(&src);
    // sum over i in [0,200) of (i + 1) = (199*200/2) + 200 = 19900 + 200 = 20100
    assert_eq!(out, vec!["20100"]);
}

#[test]
fn test_st3_untyped_object_store_read_eval() {
    // SOUNDNESS HOLE (this fix): a statically-sum-typed value stored into an UNTYPED object
    // literal (no type annotation on the binding → inferred `Object`, not a sealed/named record).
    // The store materializes the SumNode to a boxed LinObject; the read-back PROJECTS it to a
    // FRESH +1 SumNode. Before the fix the IR-lowering relocation `CloneBox` ran on the projected
    // raw SumNode via `lin_tagged_clone` (reading offset 0/8 as a TaggedVal tag/payload) → garbage
    // result + heap-buffer-overflow on the later `lin_sumnode_release`. The repr pass now seeds the
    // Index dst `Packed(SumNode)` and the lowering registers the fresh projection owned directly.
    // (3+4) = 7.
    let src = format!(
        r#"{ST3_AST_PRELUDE}
val main = () =>
  val tree: Expr = {{ "kind": "op", "op": 0, "left": {{ "kind": "num", "value": 3 }}, "right": {{ "kind": "num", "value": 4 }} }}
  val cursor = {{ "node": tree, "pos": 0 }}
  val back: Expr = cursor["node"]
  print(eval(back).toString())
main()
"#
    );
    let out = run(&src);
    assert_eq!(out, vec!["7"]);
}

#[test]
fn test_st3_untyped_object_sum_to_string() {
    // Sibling case: a sum value stored into an UNTYPED object then read back and fed to a
    // genuinely-dynamic consumer (`toString`). The read-back must materialize the REAL tree, not a
    // raw SumNode pointer (which printed garbage `{"kind":"num","value":33}` before the fix).
    let src = format!(
        r#"{ST3_AST_PRELUDE}
val main = () =>
  val tree: Expr = {{ "kind": "op", "op": 0, "left": {{ "kind": "num", "value": 3 }}, "right": {{ "kind": "num", "value": 4 }} }}
  val cursor = {{ "node": tree, "pos": 0 }}
  print(cursor["node"].toString())
main()
"#
    );
    let out = run(&src);
    // Phase 2: open (BinOp has Expr union fields → unsealed → LinMap → alphabetical toString).
    assert_eq!(
        out,
        vec![r#"{"kind": "op", "op": 0, "left": {"kind": "num", "value": 3}, "right": {"kind": "num", "value": 4}}"#]
    );
}

#[test]
fn test_st3_untyped_object_in_loop_repeated_store_read() {
    // The untyped-object store/read round-trip in a loop: stresses the materialize/project RC across
    // many iterations (no SumNode leak/UAF scaling — verified separately under ASan: the per-iter
    // 48-byte projected node is freed, not retained-and-leaked). Each iteration evaluates (i + 1).
    let src = format!(
        r#"{ST3_AST_PRELUDE}
import {{ range, for }} from "std/iter"
val main = () =>
  var total: Int32 = 0
  range(0, 200).for((i) =>
    val tree: Expr = {{ "kind": "op", "op": 0, "left": {{ "kind": "num", "value": i }}, "right": {{ "kind": "num", "value": 1 }} }}
    val cursor = {{ "node": tree, "pos": i }}
    val back: Expr = cursor["node"]
    total = total + eval(back)
  )
  print(total.toString())
main()
"#
    );
    let out = run(&src);
    // sum over i in [0,200) of (i + 1) = (199*200/2) + 200 = 20100
    assert_eq!(out, vec!["20100"]);
}

#[test]
fn test_keeppacked_sumfield_cross_fn_cursor() {
    // KEEP-PACKED-THROUGH-RECORD-FIELDS (the interp-cursor optimization): a sum value stored into a
    // record FIELD is kept packed by-pointer (`TaggedVal(TAG_SUMNODE)` — no `lin_summat`/`lin_box_object`
    // materialize) and read back via a runtime-tag-dispatched unwrap. Mirrors the interp `{ node, pos }`
    // cursor: the record is BUILT in one function and the field READ in another (the keep-packed slot
    // crosses a function/return boundary). Result must equal the materialize path. ((3+4)*(10-6)) = 28.
    let src = format!(
        r#"{ST3_AST_PRELUDE}
type Cursor = {{ "node": Expr, "pos": Int32 }}
val mkCursor = (e: Expr, p: Int32): Cursor => {{ "node": e, "pos": p }}
val readNode = (c: Cursor): Int32 => eval(c["node"])
val main = () =>
  val tree: Expr = {{
    "kind": "op", "op": 2,
    "left": {{ "kind": "op", "op": 0, "left": {{ "kind": "num", "value": 3 }}, "right": {{ "kind": "num", "value": 4 }} }},
    "right": {{ "kind": "op", "op": 1, "left": {{ "kind": "num", "value": 10 }}, "right": {{ "kind": "num", "value": 6 }} }}
  }}
  val cur = mkCursor(tree, 7)
  print(readNode(cur).toString())
main()
"#
    );
    let out = run(&src);
    assert_eq!(out, vec!["28"]);
}

#[test]
fn test_keeppacked_sumfield_tostring_field_materializes() {
    // SAFETY (keep-packed boundary correctness): a kept-packed sum FIELD fed to a genuinely-dynamic
    // consumer (`toString`) must MATERIALIZE the real tree. The kept-packed `TAG_SUMNODE` that escapes
    // the field into the type-erased `toString` boundary is materialized by the runtime walker
    // (`lin_tagged_to_string` via the per-type materializer fn-ptr in the SumNode descriptor) — NOT
    // printed as `[object]` / a raw SumNode pointer.
    let src = format!(
        r#"{ST3_AST_PRELUDE}
type Cursor = {{ "node": Expr, "pos": Int32 }}
val main = () =>
  val tree: Expr = {{ "kind": "op", "op": 0, "left": {{ "kind": "num", "value": 3 }}, "right": {{ "kind": "num", "value": 4 }} }}
  val cur: Cursor = {{ "node": tree, "pos": 7 }}
  print(cur["node"].toString())
main()
"#
    );
    let out = run(&src);
    // Phase 2: open objects (BinOp has Expr union fields → unsealed → LinMap → alphabetical keys).
    assert_eq!(
        out,
        vec![r#"{"kind": "op", "op": 0, "left": {"kind": "num", "value": 3}, "right": {"kind": "num", "value": 4}}"#]
    );
}

#[test]
fn test_keeppacked_sumfield_loop_leak_free() {
    // Leak-scaling guard for the keep-packed cursor round-trip: build → store-in-record → read-back →
    // eval in a 300-iteration loop. The keep-packed store's `object_set_fresh` retain + shell-only
    // free, balanced against the read-back's tag-dispatched unwrap+retain and the cursor drop's
    // TAG_SUMNODE release, nets zero per iteration (verified separately under ASan: the keep-packed
    // path leaks strictly LESS than the materialize baseline). Each iteration evaluates (i + 1).
    let src = format!(
        r#"{ST3_AST_PRELUDE}
import {{ range, for }} from "std/iter"
type Cursor = {{ "node": Expr, "pos": Int32 }}
val main = () =>
  var total: Int32 = 0
  range(0, 300).for((i) =>
    val tree: Expr = {{ "kind": "op", "op": 0, "left": {{ "kind": "num", "value": i }}, "right": {{ "kind": "num", "value": 1 }} }}
    val cur: Cursor = {{ "node": tree, "pos": i }}
    total = total + eval(cur["node"])
  )
  print(total.toString())
main()
"#
    );
    let out = run(&src);
    // sum over i in [0,300) of (i + 1) = (299*300/2) + 300 = 44850 + 300 = 45150
    assert_eq!(out, vec!["45150"]);
}

// Regression: sealed-record array extracted from a FixedArray (tuple) then passed to toString.
// The kp path of `sealed_array_project_from` in a non-union Coerce (Array<Json>→R[]) aliased
// the source pointer `ir_arr_alloc` and `sarrp_phi`, so both scope-exit releases hit the SAME
// pointer while the tuple's slot still held it → UAF. Fix: use `sealed_array_project_owned`
// (which retains in kp) for non-union sources, keeping RC balanced across both IR paths.
#[test]
fn test_keeppacked_tuple_sealed_arr_tostring_no_uaf() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map, range } from "std/iter"

type R = { "a": Int32, "n": { "x": Int32 } }

val mk = (): [R[], Int32] =>
  val arr: R[] = range(0, 4).map(i => { "a": i, "n": { "x": i * 10 } })
  [arr, 7]

val main = () =>
  val [arr, z] = mk()
  print(toString(arr))

main()
"#);
    assert_eq!(output, vec![r#"[{"a": 0, "n": {"x": 0}}, {"a": 1, "n": {"x": 10}}, {"a": 2, "n": {"x": 20}}, {"a": 3, "n": {"x": 30}}]"#]);
}

// ---------------------------------------------------------------------------
// Capturing-closure inline (perf/capturing-closure-inline): a LITERAL lambda at a
// `.for`/combinator call site that CAPTURES an outer binding is spliced inline (no boxed
// per-element closure call). The captured slot resolves through the enclosing builder's
// cell/global/local binding, so captured-`var` mutation hits the same shared cell/global
// (ADR-012). These tests guard the correctness surfaces: var-cell accumulation, an inlined
// body that EMITS ITS OWN BLOCKS (the spike's CFG-latch hang), a captured top-level global var,
// and the unchanged fallback paths (Stream `.for` / a non-literal closure value).

// Array `.for` with a LOCAL captured `var` accumulator: the body `total = total + x` writes the
// captured heap cell each iteration; the inlined `CellSet` hits the same cell. Sum 1..=5 = 15.
#[test]
fn test_capturing_for_array_var_cell_accumulation() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { for } from "std/iter"

val main = () =>
  var total: Int32 = 0
  [1, 2, 3, 4, 5].for(x => total = total + x)
  print(toString(total))
main()
"#);
    assert_eq!(output, vec!["15"]);
}

// MANDATORY CFG case (the spike hang): an inlined `.for` body that EMITS ITS OWN BASIC BLOCKS via
// an inner combinator (`.filter` produces keep/skip/join blocks). After inlining, the loop latch is
// NOT the provisional body block but the inner construct's exit; the back-edge + header phi must be
// patched latch-relative or the CFG is malformed → infinite loop. Sum of (count of evens in
// [1..=n]) over n in {1,2,3,4} = 0+1+1+2 = 4.
#[test]
fn test_capturing_for_block_emitting_body_inner_filter() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { for, filter, length } from "std/iter"

val main = () =>
  var acc: Int32 = 0
  [1, 2, 3, 4].for(n =>
    val evens = [1, 2, 3, 4].filter(x => x <= n).filter(x => x % 2 == 0)
    acc = acc + evens.length()
  )
  print(toString(acc))
main()
"#);
    assert_eq!(output, vec!["4"]);
}

// MANDATORY CFG case (block-emitting body): an inlined `.for` body containing a `match` (multiple
// arms → multiple blocks). Same latch-relative wiring requirement as the inner-filter case.
#[test]
fn test_capturing_for_block_emitting_body_match() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { for } from "std/iter"

val main = () =>
  var acc: Int32 = 0
  [1, 2, 3, 4, 5].for(x =>
    val d = match x % 2
      is 0 => 10
      else => 1
    acc = acc + d
  )
  print(toString(acc))
main()
"#);
    // x in {1,2,3,4,5}: parity {1,0,1,0,1} → {1,10,1,10,1} → sum 23
    assert_eq!(output, vec!["23"]);
}

// Captured TOP-LEVEL `var` (a module GLOBAL, not a heap cell): the inlined body's write becomes a
// `GlobalValSet` to the same global slot. Sum 1..=4 = 10.
#[test]
fn test_capturing_for_top_level_global_var() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { for } from "std/iter"

var counter: Int32 = 0
[1, 2, 3, 4].for(x => counter = counter + x)
print(toString(counter))
"#);
    assert_eq!(output, vec!["10"]);
}

// Fused `range(a,b).for(f)` with a captured var accumulator (the spike's original path). Sum
// 0..100 = 4950.
#[test]
fn test_capturing_range_for_var_accumulation() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range, for } from "std/iter"

val main = () =>
  var total: Int64 = 0i64
  range(0, 100).for(i => total = total + 1i64)
  print(toString(total))
main()
"#);
    assert_eq!(output, vec!["100"]);
}

// `range().for` with a block-emitting (inner `if`) capturing body: latch-relative wiring for the
// hand-written range loop. Count of i in [0,10) with i%3==0 → {0,3,6,9} = 4.
#[test]
fn test_capturing_range_for_block_emitting_body() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range, for } from "std/iter"

val main = () =>
  var n: Int32 = 0
  range(0, 10).for(i =>
    val hit = if i % 3 == 0 then 1 else 0
    n = n + hit
  )
  print(toString(n))
main()
"#);
    assert_eq!(output, vec!["4"]);
}

// Capturing `.map` and `.reduce`: a captured `offset`/`base` is read inside the inlined body.
#[test]
fn test_capturing_map_and_reduce() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map, reduce, for } from "std/iter"

val main = () =>
  val offset = 100
  val shifted = [1, 2, 3].map(x => x + offset)
  shifted.for(x => print(toString(x)))
  val base = 1000
  val total = [1, 2, 3].reduce(0, (acc, x) => acc + x + base)
  print(toString(total))
main()
"#);
    assert_eq!(output, vec!["101", "102", "103", "3006"]);
}

// A NON-LITERAL closure (a lambda bound to a `val` then passed by name) must take the UNCHANGED
// boxed path — it is not a literal at the call site, so the inliner bails. Correct sum 15.
#[test]
fn test_non_literal_closure_for_unchanged_path() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { for } from "std/iter"

val main = () =>
  var total: Int32 = 0
  val adder = (x: Int32) => total = total + x
  [1, 2, 3, 4, 5].for(adder)
  print(toString(total))
main()
"#);
    assert_eq!(output, vec!["15"]);
}

// A Stream `.for` with a capturing body must NOT be eagerly inlined (ADR-051: a Stream is driven
// lazily by the runtime). The body closure escapes into the StreamFor runtime call unchanged.
#[test]
fn test_capturing_stream_for_unchanged_path() {
    let inp = std::env::temp_dir().join(format!("lin_ctest_capstream_{}.txt", std::process::id()));
    let _ = fs::remove_file(&inp);
    fs::write(&inp, "a\nb\nc\nd\n").unwrap();
    let inp_s = inp.display().to_string();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ readStream, lines }} from "std/stream"
import {{ for }} from "std/iter"

val main = () =>
  var seen: Int32 = 0
  readStream("{inp_s}").lines().for(l => seen = seen + 1)
  print(toString(seen))
main()
"#));
    let _ = fs::remove_file(&inp);
    assert_eq!(output, vec!["4"]);
}

// Regression (Path 0): a `T | Null` value (T a record with a NESTED heap field) threaded through a
// self-tail-recursive parameter must not be a use-after-free. The `else => scan(.., trip, ..)`
// pass-through and the `if new != null then scan(.., new, ..)` arms both re-box a match-narrowed
// concrete record into the `T | Null` union for the tail call; the resulting box must OWN its inner
// (the threaded slot keeps it across the back-edge), so the caller-owned-shell box arg is retained
// before `release_owned_for_tail_call` releases its source temp. Without the retain the box's inner
// is freed out from under the next iteration (ASan: heap-use-after-free in lin_rc_retain inside the
// recursive callee). This is the RAPTOR `scanRouteAt`/`Trip | Null` shape. Output 190000 verified
// against the equivalent non-recursive evaluation and ASan-clean (leaks-on + leaks-off).
#[test]
fn test_union_record_nested_field_tail_recursive_param_no_uaf() {
    let output = run(r#"import { print } from "std/io"

type StopTime = { "stop": String, "arrivalTime": Int32 }
type Service = { "days": AnyVal }
type Trip = { "tripId": String, "stopTimes": StopTime[], "service": Service }

val mkTrip = (id: String): Trip =>
  { "tripId": id, "stopTimes": [{ "stop": "A", "arrivalTime": 10 }], "service": { "days": {} } }

val getTrip = (i: Int32): Trip | Null =>
  if i % 3 == 0 then null else mkTrip("t")

val scan = (pi: Int32, n: Int32, trip: Trip | Null, acc: Int32): Int32 =>
  if pi >= n then acc
  else if trip != null then
    val st = trip["stopTimes"][0]
    val newAcc = acc + st["arrivalTime"]
    val newTrip = getTrip(pi)
    if newTrip != null then scan(pi + 1, n, newTrip, newAcc) else scan(pi + 1, n, trip, newAcc)
  else
    scan(pi + 1, n, getTrip(pi), acc)

val driver = (k: Int32, total: Int32): Int32 =>
  if k <= 0 then total else driver(k - 1, total + scan(0, 40, null, 0))

val main = () => print("${driver(500, 0)}")
main()
"#);
    assert_eq!(output, vec!["190000"]);
}

// ── Null-coalescing operator `??` (ADR-066) ───────────────────────────────────
// `a ?? b` ≡ `if a != null then a else b`: `a` once, `b` only when `a` is Null.
// Coalesces Null ONLY — an Error value flows through. Lowers to the proven if/else
// + `!= null` desugaring (no hand-rolled union-temp RC).

#[test]
fn test_coalesce_scalar_and_heap() {
    // Null left → default; non-null left → left value. Cover a scalar (Int32) and a heap
    // value (String) on both the null and non-null path.
    let output = run(r#"import { print } from "std/io"

val ni: Int32 | Null = null
val si: Int32 | Null = 5
print("${ni ?? 99}")
print("${si ?? 99}")

val ns: String | Null = null
val ss: String | Null = "hi"
print(ns ?? "default")
print(ss ?? "default")
"#);
    assert_eq!(output, vec!["99", "5", "default", "hi"]);
}

#[test]
fn test_coalesce_record_left() {
    // A heap RECORD union on the left: non-null keeps the record, null falls to the default
    // record. The result is usable as the record type (field read works without narrowing).
    let output = run(r#"import { print } from "std/io"
type Point = { "x": Int32, "y": Int32 }

val present: Point | Null = { "x": 1, "y": 2 }
val absent: Point | Null = null
val a = present ?? { "x": 0, "y": 0 }
val b = absent ?? { "x": 7, "y": 8 }
print("${a["x"]} ${a["y"]}")
print("${b["x"]} ${b["y"]}")
"#);
    assert_eq!(output, vec!["1 2", "7 8"]);
}

#[test]
fn test_coalesce_short_circuits_rhs() {
    // The RHS is only evaluated when the left is Null. A side-effecting default function must
    // NOT run when the left is non-null, and MUST run (exactly once) when it is null.
    let output = run(r#"import { print } from "std/io"

val sideEffect = (label: String): Int32 =>
  print("evaluated ${label}")
  -1

val nonNull: Int32 | Null = 5
val nullVal: Int32 | Null = null

print("${nonNull ?? sideEffect("A")}")
print("${nullVal ?? sideEffect("B")}")
"#);
    // "A" never printed (left non-null); "B" printed once before its result is used.
    assert_eq!(output, vec!["5", "evaluated B", "-1"]);
}

#[test]
fn test_coalesce_chaining() {
    // `x ?? y ?? z` is left-associative. First-wins and fall-through-to-last.
    let output = run(r#"import { print } from "std/io"

val a: Int32 | Null = null
val b: Int32 | Null = null
val c: Int32 | Null = 3

val firstWins: Int32 | Null = 1
print("${firstWins ?? b ?? 7}")
print("${a ?? c ?? 7}")
print("${a ?? b ?? 7}")
"#);
    assert_eq!(output, vec!["1", "3", "7"]);
}

#[test]
fn test_coalesce_map_read_usable_as_bare_type() {
    // `m[k] ?? default` on a `{ String: Int32 }`: present and absent keys, and the result is a
    // bare `Int32` usable in arithmetic with no further narrowing.
    // ADR-076: the present key is read DIRECTLY — after `m["a"] = 5` it narrows to a non-null
    // `Int32`, so `m["a"] ?? 99` would be a dead-default error. The absent-key coalesces (`"b"`,
    // never assigned) keep the genuine `?? default` path.
    let output = run(r#"import { print } from "std/io"

val m: { String: Int32 } = {}
m["a"] = 5

val present = m["a"]
val absent = m["b"] ?? 99
print("${present + 1}")
print("${absent + 1}")

val annotated: Int32 = m["b"] ?? 10
print("${annotated * 2}")
"#);
    assert_eq!(output, vec!["6", "100", "20"]);
}

#[test]
fn test_coalesce_error_passes_through() {
    // Left of type `T | Null | Error` holding an Error value: `??` yields the Error, NOT the
    // default. Lin's value-based error convention stays explicit.
    let output = run(r#"import { print } from "std/io"
type Trip = { "id": Int32 }

val lookup = (k: String): Trip | Null | Error =>
  if k == "bad" then { "type": "error", "message": "nope" }
  else if k == "miss" then null
  else { "id": 42 }

val onError = lookup("bad") ?? { "id": 0 }
match onError
  is Error => print("error: ${onError["message"]}")
  has { id } => print("value: ${id}")
  else => print("other")

val onMiss = lookup("miss") ?? { "id": 0 }
match onMiss
  is Error => print("error")
  has { id } => print("default: ${id}")
  else => print("other")
"#);
    assert_eq!(output, vec!["error: nope", "default: 0"]);
}

#[test]
fn test_coalesce_bare_null_left() {
    // A bare `Null` left is allowed; the result is just the right operand's type/value.
    let output = run(r#"import { print } from "std/io"
val d = null ?? "fallback"
print(d)
"#);
    assert_eq!(output, vec!["fallback"]);
}

#[test]
fn test_coalesce_continuation_line() {
    // The RHS may sit on a continuation line, mirroring `||`/`&&` (ADR-005).
    let output = run(r#"import { print } from "std/io"
val a: Int32 | Null = null
val r = a
  ?? 42
print("${r}")
"#);
    assert_eq!(output, vec!["42"]);
}

#[test]
fn test_coalesce_precedence_below_equality() {
    // `a ?? b == c` groups as `a ?? (b == c)` (`??` is the lowest binary rung, like JS).
    let output = run(r#"import { print } from "std/io"
val a: Boolean | Null = null
val r = a ?? 1 == 1
print("${r}")
"#);
    // a is null → result is `1 == 1` → true. (If `??` bound tighter than `==`, this would be a
    // type error comparing a Boolean to an Int32.)
    assert_eq!(output, vec!["true"]);
}

#[test]
fn test_coalesce_parenthesized_logical_rhs() {
    // `a ?? (b || c)` is legal (the logical op is parenthesised) and runs.
    let output = run(r#"import { print } from "std/io"
val y: Boolean | Null = null
val z = y ?? (false || true)
print("${z}")
"#);
    assert_eq!(output, vec!["true"]);
}

#[test]
fn test_coalesce_never_null_left_is_compile_error() {
    let err = run_expect_err(r#"import { print } from "std/io"
val x = 5 ?? 1
print("${x}")
"#);
    assert!(
        err.contains("never null"),
        "expected never-null diagnostic, got: {}",
        err
    );
}

#[test]
fn test_coalesce_unparenthesized_mix_or_is_parse_error() {
    // `a || b ?? c` — left operand mixes `||` with `??` unparenthesised.
    let err = run_expect_err(r#"val x = true || false ?? false
"#);
    assert!(
        err.contains("cannot mix") && err.contains("??"),
        "expected mixing diagnostic, got: {}",
        err
    );
}

#[test]
fn test_coalesce_unparenthesized_mix_rhs_is_parse_error() {
    // `a ?? b || c` — right operand mixes `??` with `||` unparenthesised.
    let err = run_expect_err(r#"val y: Boolean | Null = null
val x = y ?? false || true
"#);
    assert!(
        err.contains("cannot mix") && err.contains("??"),
        "expected mixing diagnostic, got: {}",
        err
    );
}

#[test]
fn test_coalesce_rc_soundness_loop_heap_values() {
    // Exercise `??` over heap values (String / record union `T | Null`) many times in a hot
    // loop — values must stay correct (no UAF / corruption), and a freshly-allocated record
    // RHS on the null path is built and reclaimed each iteration. Sums a deterministic series
    // so a single wrong/freed value would change the total.
    let output = run(r#"import { print } from "std/io"
type Box = { "v": Int32 }

val step = (i: Int32, acc: Int32): Int32 =>
  if i <= 0 then acc
  else
    // Alternate the left between a real heap record and null, forcing both the keep-left and
    // build-fresh-RHS paths every other iteration.
    val maybe: Box | Null = if i % 2 == 0 then { "v": i } else null
    val chosen = maybe ?? { "v": 100 }
    val s: String | Null = if i % 3 == 0 then null else "x"
    val tag = s ?? "y"
    // Read the chosen String through `==` so the freshly-allocated default actually flows out
    // and is consumed (the heap-RHS short-circuit path), contributing a deterministic +1.
    val bump = if tag == "x" then 1 else 1
    step(i - 1, acc + chosen["v"] + bump)
print("${step(2000, 0)}")
"#);
    // Deterministic; the exact total is asserted so any dropped/corrupted heap value is caught.
    // even i → v=i ; odd i → v=100. bump is always 1. Sum over i=1..2000.
    // even sum = 2+4+...+2000 = 1001000 ; odd count = 1000 → odd v contributes 1000*100=100000
    // bump total = 2000. Total = 1001000 + 100000 + 2000 = 1103000.
    assert_eq!(output, vec!["1103000"]);
}

#[test]
fn test_coalesce_empty_literal_refines_to_stripped_type() {
    // `m[k] ?? {}` where m's value type is itself a map/record should refine `{}` to that value
    // type, not produce a `ValueType | {}` union. Regression for the case where `infer_coalesce`
    // inferred the right operand with no expected type.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"

// Nested map: { String: { UInt32: Boolean } } — the exact pattern from the original bug.
// dates[key] is { UInt32: Boolean } | Null; `?? {}` must refine to { UInt32: Boolean }.
var dates: { String: { UInt32: Boolean } } = {}
dates["svc1"][1u32] = true
val entry: { UInt32: Boolean } = dates["missing"] ?? {}
print(toString(entry[1u32] ?? false))

// { String: Int32[] } — empty-array default refines to Int32[].
var lists: { String: Int32[] } = {}
val xs: Int32[] = lists["k"] ?? []
print(toString(length(xs)))
"#);
    assert_eq!(output, vec!["false", "0"]);
}

#[test]
fn test_coalesce_mismatched_default_still_unions() {
    // When the RHS default is genuinely a different type, `??` still produces a union result
    // (the documented `stripped | D` behaviour is preserved — no regression).
    // ADR-076: the present-key read is sourced from a function-PARAM map (`use`), so it stays the
    // nullable map-read `Int32 | Null` and the mismatched `?? "fallback"` is a live union — an
    // assign-then-coalesce of the SAME key would narrow to non-null `Int32` and make the default
    // dead. The absent-key coalesce (`"missing"`) is unaffected either way.
    let output = run(r#"import { print } from "std/io"

val use = (m: { String: Int32 }) =>
  val r = m["x"] ?? "fallback"
  val s = m["missing"] ?? "fallback"
  match r
    is String => print("string: ${r}")
    is Int32 => print("int: ${r}")
    else => print("other")
  match s
    is String => print("string: ${s}")
    is Int32 => print("int: ${s}")
    else => print("other")
var m: { String: Int32 } = {}
m["x"] = 7
use(m)
"#);
    assert_eq!(output, vec!["int: 7", "string: fallback"]);
}

#[test]
fn test_fmt_roundtrips_coalesce() {
    // The formatter must print `??` back faithfully, and keep parens around a parenthesised
    // logical RHS / right-nested `??` (ADR-065). Idempotent.
    let chain = "val r = a ?? b ?? 7\n";
    assert_eq!(fmt(chain), chain, "?? chain must round-trip");
    assert_eq!(fmt(&fmt(chain)), fmt(chain), "?? formatting must be idempotent");

    let paren_logical = "val q = w ?? (false || true)\n";
    assert_eq!(fmt(paren_logical), paren_logical, "parens around a logical RHS must survive");

    let right_nested = "val n = a ?? (b ?? 3)\n";
    assert_eq!(fmt(right_nested), right_nested, "right-nested ?? parens must survive");

    let with_eq = "val e = a ?? b == c\n";
    assert_eq!(fmt(with_eq), with_eq, "?? below == must round-trip without parens");
}

#[test]
fn test_fmt_coalesce_required_parens_round_trip() {
    // Regression: `??` is the LOOSEST binary rung, so when a `??` sub-expression is an operand of a
    // TIGHTER-binding operator the wrapping parens are SEMANTICALLY REQUIRED — dropping them changes
    // the parse (`(a ?? b) + 1` would reparse as `a ?? (b + 1)`). The formatter formerly stripped
    // them (it only descended into `BinaryOp` operands, never `Coalesce`), changing program meaning.
    // Every line below MUST keep its parens, exactly, and the formatting must be idempotent.
    let cases = [
        // arithmetic / comparison parents
        "val a = (m ?? 0) + 1\n",
        "val a = (m ?? 0) - 1\n",
        "val a = (m ?? 0) * 2\n",
        "val a = (m ?? 0) < 5\n",
        "val a = (m ?? 0) > 5\n",
        "val a = (m ?? 0) == 5\n",
        "val a = (m ?? 0) != 5\n",
        // logical parents (an unparenthesised `??`/`&&`/`||` mix is a parse error, so the parens
        // must survive)
        "val a = (m ?? false) && true\n",
        "val a = (m ?? false) || true\n",
        // unary / is / has parents (bind tighter than `??`)
        "val a = !(m ?? false)\n",
        "val a = (m ?? 0) is Int32\n",
        "val a = (m ?? z) has y\n",
        // `??` as a postfix base
        "val a = (m ?? other).field\n",
        // a `??` on BOTH sides of a tighter parent
        "val a = (m ?? 0) + (n ?? 1)\n",
    ];
    for src in cases {
        let out = fmt(src);
        assert_eq!(out, src, "required parens around a `??` operand must survive formatting");
        assert_eq!(fmt(&out), out, "?? required-parens formatting must be idempotent");
    }

    // And confirm we do NOT now over-parenthesise: a bare `??` (and a redundant-paren source) needs
    // no parens, and `??` as a TIGHTER child than its parent (`a + (b ?? c)` is the right operand of
    // `+`, but written that way it parses as `a + b ?? c`... which is actually `(a + b) ?? c`) — so
    // the canonical paren-free forms must stay paren-free.
    assert_eq!(fmt("val c = m ?? 0\n"), "val c = m ?? 0\n", "standalone `??` needs no parens");
    assert_eq!(fmt("val c = (m ?? 0)\n"), "val c = m ?? 0\n", "redundant outer parens are stripped");
}

#[test]
fn test_fmt_coalesce_run_equivalence() {
    // The gate that SHOULD have caught the required-parens bug: compile+run a program that mixes
    // `??` with `+`, `-`, `<`, `>`, `==`, `&&`, `||`, nested `??`, unary `!`, `is`, and `??` inside
    // string interpolation, BOTH before and after `lin fmt`. The runtime output must be identical —
    // i.e. the formatter must not have changed the parse of any load-bearing-parens `??` operand.
    let source = "import { print } from \"std/io\"\n\
val mi: { String: Int32 } = { \"a\": 5 }\n\
val mb: { String: Boolean } = { \"t\": true }\n\
val plus: Int32 = (mi[\"a\"] ?? 0) + 1\n\
val minus: Int32 = (mi[\"x\"] ?? 10) - 3\n\
val lt: Boolean = (mi[\"a\"] ?? 0) < 100\n\
val gt: Boolean = (mi[\"x\"] ?? 0) > 100\n\
val eq: Boolean = (mi[\"a\"] ?? 0) == 5\n\
val andL: Boolean = (mb[\"t\"] ?? false) && true\n\
val orL: Boolean = (mb[\"x\"] ?? false) || true\n\
val nested: Int32 = mi[\"x\"] ?? mi[\"a\"] ?? 9\n\
val notC: Boolean = !(mb[\"x\"] ?? false)\n\
val isC: Boolean = (mi[\"a\"] ?? 0) is Int32\n\
print(\"${plus} ${minus} ${lt} ${gt} ${eq} ${andL} ${orL} ${nested} ${notC} ${isC} ${(mi[\"a\"] ?? 0) + 100}\")\n";
    let formatted = fmt(source);
    let before = run(source);
    let after = run(&formatted);
    assert_eq!(
        before, after,
        "formatting changed the meaning of a `??` expression\nformatted:\n{}",
        formatted
    );
    // Idempotent re-format must also still run-equal.
    let reformatted = fmt(&formatted);
    assert_eq!(formatted, reformatted, "?? formatting not idempotent");
}

// Path-9C seal-propagation symmetry: an object literal with a nested sealed-record ARRAY field,
// built by a function returning the named record then read back, must round-trip correctly. The
// producer (`mkTrip`'s `{ "stopTimes": [{ … }] }` literal) is now DIRECTED against the sealed
// `StopTime[]` field type, so it adopts the SEALED element representation — matching what the
// consumer (`trip["stopTimes"][i]`) reads it back at. Before the fix the producer fell to
// undirected inference and built a BOXED `Object[]` while the consumer read it PACKED (the gate
// admits this all-scalar record): a silent mis-read (`{ "arr": 33 }` read back as `0`). All-scalar
// fields here so the field is packable under the current scalar+Bool gate — this asserts the two
// sides agree at the gate's live edge.
#[test]
fn test_nested_sealed_record_array_field_producer_consumer_symmetry() {
    let output = run(r#"import { print } from "std/io"

type StopTime = { "arr": Int32, "dep": Int32 }
type Trip = { "tripId": Int32, "stopTimes": StopTime[] }

val mkTrip = (id: Int32): Trip =>
  { "tripId": id, "stopTimes": [{ "arr": 11, "dep": 22 }, { "arr": 33, "dep": 44 }] }

val main = () =>
  val t = mkTrip(7)
  val s0 = t["stopTimes"][0]
  val s1 = t["stopTimes"][1]
  print("${t["tripId"]} ${s0["arr"]} ${s0["dep"]} ${s1["arr"]} ${s1["dep"]}")
main()
"#);
    assert_eq!(output, vec!["7 11 22 33 44"]);
}

// Regression (Path 9 TCO param-slot leak): a HEAP-BEARING sealed (packed) record (`Trip` with a
// `String` and a sealed-`ST[]` field) threaded through a self-tail-recursive parameter slot, with a
// FRESH record built each iteration, must release the PRIOR slot value before the back-edge
// overwrites it. `Codegen::tco_param_needs_release` formerly carved out ALL sealed records
// (`sealed_fields(ty).is_none()`), gating off both the back-edge `emit_tco_release_old` and the
// loop-exit `emit_tco_release_final` — so each iteration overwrote the slot with a fresh packed
// struct and leaked the old struct + its heap fields (linear scaling: ASan-measured ~367 B/iter on
// this shape, going CONSTANT after the fix). The carve-out is now narrowed to PURELY-scalar sealed
// records (stack-resident, RC-suppressed); a heap-bearing sealed record participates in TCO param
// release via the packed `emit_sealed_release` path. The result must be correct (a missing/wrong
// release would corrupt the packed struct read back next iteration or double-free → crash). The
// ASan leak-scaling proof is via the tools/sealed-harness-style measurement; this guards
// correctness + no-UAF under `cargo test`.
#[test]
fn test_sealed_heap_record_tail_recursive_param_no_leak() {
    let output = run(r#"import { print } from "std/io"
import { length } from "std/array"

type ST = { "stop": String, "at": Int32 }
type Trip = { "id": String, "stops": ST[] }

val makeTrip = (n: Int32): Trip =>
  { "id": "trip-${n}", "stops": [{ "stop": "s${n}", "at": n }, { "stop": "t${n}", "at": n + 1 }] }

val loop = (n: Int32, t: Trip): Int32 =>
  if n == 0 then t["stops"].length()
  else loop(n - 1, makeTrip(n))

val main = () => print("${loop(1000, makeTrip(0))}")
main()
"#);
    assert_eq!(output, vec!["2"]);
}

// TarEntry entries/header/body composable adapter end-to-end tests.
// Fixtures live at stdlib/fixtures/sample.tar (3 entries: alpha.txt/17B, bravo.txt/31B, large.txt/4390B).

// entries() lists all entry names via header(); body() is drained inline.
#[test]
fn test_tar_entries_header_body_drain() {
    let tar = workspace_root().join("stdlib/fixtures/sample.tar");
    let tar_path = tar.to_str().unwrap();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ readStream, drain }} from "std/stream"
import {{ entries, header, body }} from "std/archive"
import {{ for }} from "std/iter"
import {{ push, length }} from "std/array"
import {{ toString }} from "std/string"

val names: String[] = []
readStream("{tar_path}")
  .entries()
  .for(e =>
    names.push(e.header()["name"])
    e.body().drain()
  )
print(toString(length(names)))
print(names[0])
print(names[1])
print(names[2])
"#));
    assert_eq!(output, vec!["3", "alpha.txt", "bravo.txt", "large.txt"]);
}

// entries() with bodies auto-skipped (header-only scan).
#[test]
fn test_tar_entries_header_only_auto_skip() {
    let tar = workspace_root().join("stdlib/fixtures/sample.tar");
    let tar_path = tar.to_str().unwrap();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ readStream, drain }} from "std/stream"
import {{ entries, header }} from "std/archive"
import {{ for }} from "std/iter"
import {{ push, length }} from "std/array"
import {{ toString }} from "std/string"

val names: String[] = []
readStream("{tar_path}")
  .entries()
  .for(e =>
    names.push(e.header()["name"])
  )
print(toString(length(names)))
print(names[0])
"#));
    assert_eq!(output, vec!["3", "alpha.txt"]);
}

// body() returns the correct bytes via readText.
#[test]
fn test_tar_entries_body_readtext() {
    let tar = workspace_root().join("stdlib/fixtures/sample.tar");
    let tar_path = tar.to_str().unwrap();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ readStream, readText, drain }} from "std/stream"
import {{ entries, header, body }} from "std/archive"
import {{ for }} from "std/iter"
import {{ toString }} from "std/string"

readStream("{tar_path}")
  .entries()
  .for(e =>
    val h = e.header()
    if h["name"] == "alpha.txt" then
      val raw = e.body().readText()
      val text = match raw
        is Error => "err"
        else => raw
      print(text)
    else
      e.body().drain()
  )
"#));
    // alpha.txt has content "hello from alpha\n" — print trims trailing newline
    assert_eq!(output, vec!["hello from alpha"]);
}

// entries() correctly reports header size in Int64.
#[test]
fn test_tar_entries_header_size() {
    let tar = workspace_root().join("stdlib/fixtures/sample.tar");
    let tar_path = tar.to_str().unwrap();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ readStream }} from "std/stream"
import {{ entries, header }} from "std/archive"
import {{ for }} from "std/iter"
import {{ toString }} from "std/string"

readStream("{tar_path}")
  .entries()
  .for(e =>
    val h = e.header()
    print("${{h["name"]}}:${{toString(h["size"])}}")
  )
"#));
    assert_eq!(output, vec!["alpha.txt:17", "bravo.txt:31", "large.txt:4390"]);
}

// TarEntry handles can be stored in variables and used after the stream step. This verifies that
// TarEntry is refcounted (not affine) — a handle held across a stream step stays usable for
// header reads (though body would be expired at that point).
#[test]
fn test_tar_entry_stored_in_variable() {
    let tar = workspace_root().join("stdlib/fixtures/sample.tar");
    let tar_path = tar.to_str().unwrap();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ readStream, drain }} from "std/stream"
import {{ entries, header, body }} from "std/archive"
import {{ for }} from "std/iter"
import {{ push, length }} from "std/array"
import {{ toString }} from "std/string"

// TarEntry is refcounted: store headers from all entries in a var array.
// (Bodies are drained inline; header reads after the loop are still valid.)
val headers: AnyVal[] = []
readStream("{tar_path}")
  .entries()
  .for(e =>
    headers.push(e.header()["name"])
    e.body().drain()
  )
print(toString(length(headers)))
print(headers[0])
"#));
    assert_eq!(output, vec!["3", "alpha.txt"]);
}

// Regression test for Finding 1: TarEntry captured by an escaping closure must not UAF.
// Before the fix, `is_union_ty` in lower.rs and `is_union_owning_ty` in ownership_verify.rs
// both missed `Type::TarEntry`, causing CaptureRelease::None (no retain) for TarEntry captures.
// The creating scope's exit released the TarEntry box while the escaped closure still held it —
// a use-after-free / null-dereference at the closure call site.
#[test]
fn test_tar_entry_escaping_closure_no_uaf() {
    let tar = workspace_root().join("stdlib/fixtures/sample.tar");
    let tar_path = tar.to_str().unwrap();
    let output = run(&format!(
        r#"import {{ print }} from "std/io"
import {{ readStream }} from "std/stream"
import {{ entries, header }} from "std/archive"
import {{ find }} from "std/iter"

// Reproduces the escaping-closure shape: a TarEntry is captured by a closure that
// outlives the scope where the entry was found.
val pick = (): () => String =>
  val found = readStream("{tar_path}")
    .entries()
    .find(e => e.header()["name"] == "bravo.txt")
  match found
    is Null => () => "none"
    is Error => () => "err"
    else =>
      val t: TarEntry = found
      () => t.header()["name"]

val namer = pick()
print(namer())
print(namer())
"#
    ));
    assert_eq!(output, vec!["bravo.txt", "bravo.txt"]);
}

// Regression test for Finding 2: async thunk capturing a TarEntry must be a compile-time error.
// A TarEntry shares the archive cursor and cannot cross a thread boundary.
#[test]
fn test_async_captures_tar_entry_is_compile_error() {
    let tar = workspace_root().join("stdlib/fixtures/sample.tar");
    let tar_path = tar.to_str().unwrap();
    let err = run_expect_err(&format!(
        r#"import {{ print }} from "std/io"
import {{ readStream }} from "std/stream"
import {{ entries, header }} from "std/archive"
import {{ find }} from "std/iter"
import {{ async, await }} from "std/async"

val found = readStream("{tar_path}")
  .entries()
  .find(e => e.header()["name"] == "bravo.txt")

match found
  is Null => print("none")
  is Error => print("err")
  else =>
    val t: TarEntry = found
    val p = async(() => t.header()["name"])
    match await(p)
      is Error => print("err")
      else => print(await(p))
"#
    ));
    assert!(
        err.contains("non-transferable"),
        "expected 'non-transferable' in compile error, got:\n{err}"
    );
}

// ---------------------------------------------------------------------------
// Heap-field SumNode Stage 3 — discriminated unions whose non-discriminant
// fields include String, Array, or nested-sealed records.
//
// Prior to this change, any variant with a heap field caused the whole union
// to fall back to the BOXED path (4 lin_object_get + lin_sealed_alloc + 3
// lin_rc_retain per match-dispatch). Stage 3 widens the SumNode gate to admit
// heap fields, stores them as 8-byte owned pointer slots in the node payload,
// uses a SumDesc drop-table entry (KIND_STRING / KIND_ARRAY / KIND_SEALED) for
// release, and reads them by const-offset GEP+load+Retain in the direct-read
// fast path — eliminating the boxing round-trip from match-narrow field reads.
//
// Correctness is the primary gate here; the RC discipline (retain on read,
// descriptor-drop on free) is verified by ASan separately.
// ---------------------------------------------------------------------------

#[test]
fn test_heap_sumnode_string_field_match_read() {
    // Two-variant union with a String field ("label") and a scalar field.
    // Match-narrow reads both; assert the correct string is returned for each variant.
    let out = run(r#"import { print } from "std/io"
type Circle = { "kind": "circle", "r": Int32, "label": String }
type Square = { "kind": "square", "s": Int32, "label": String }
type Shape = Circle | Square

val describe = (s: Shape): String =>
  match s
    is Circle => "circle r=${s["r"]} label=${s["label"]}"
    is Square => "square s=${s["s"]} label=${s["label"]}"

val c: Shape = { "kind": "circle", "r": 5, "label": "big" }
val sq: Shape = { "kind": "square", "s": 3, "label": "small" }
print(describe(c))
print(describe(sq))
"#);
    assert_eq!(out, vec!["circle r=5 label=big", "square s=3 label=small"]);
}

#[test]
fn test_heap_sumnode_string_field_loop_no_leak() {
    // Build + dispatch a heap-field SumNode on every iteration — guards that the
    // runtime drop walk (via KIND_STRING in the SumDesc) releases the String exactly
    // once per iteration (no leak, no double-free). The result is deterministic.
    let out = run(r#"import { print } from "std/io"
import { range, for } from "std/iter"
type A = { "kind": "a", "name": String, "n": Int32 }
type B = { "kind": "b", "name": String, "n": Int32 }
type AB = A | B

val process = (x: AB): String =>
  match x
    is A => "a:${x["name"]}:${x["n"]}"
    is B => "b:${x["name"]}:${x["n"]}"

var last = ""
range(0, 5).for(i =>
  val s: AB =
    if i % 2 == 0 then { "kind": "a", "name": "item${i}", "n": i }
    else { "kind": "b", "name": "item${i}", "n": i }
  last = process(s))
print(last)
"#);
    // i=4: 4%2==0 → kind "a" → "a:item4:4"
    assert_eq!(out, vec!["a:item4:4"]);
}

// Regression (ADR-083): a `while`-thunk closure that captures an outer `var last: Record | Null`
// (a NullableRecord — physically a raw nullable sealed-struct pointer) AND, in the same body,
// (a) ASSIGNS a sealed record to that var (`lastFound = t`) and (b) CALLS a heap-touching function
// over a multi-map record corrupted memory. The captured var's slot holds a NullableRecord, but
// codegen's union-typed Retain (the sealed `Trip` value carries a `Trip | Null` STATIC type when it
// flows into the cell — same raw-pointer repr, no Coerce) routed it through `lin_tagged_retain`,
// which reads offset 0 of the sealed struct as a TAG byte and offset 8 as an inner payload pointer
// — type-confusion + UAF (segfault). The CellSet release-of-old likewise used `lin_tagged_release`,
// dealloc-ing the 56-byte struct as a 16-byte TaggedVal box (mismatched-size double-free). Fix:
// codegen retains/releases ANY packed-pointer repr (incl. a packed value carrying a union static
// type) by-rc / null-guarded-sealed, never by-tag. The deep loop (300k record-var assigns across a
// heap-touching call) is the per-iteration UAF/double-free guard.
#[test]
fn test_while_thunk_captured_record_var_across_heap_call_no_uaf() {
    let out = run(r#"import { print } from "std/io"
import { length } from "std/array"
import { while } from "std/iter"

type Service = { "startDate": UInt32, "endDate": UInt32, "days": { Int32: Boolean }, "dates": { Int32: Boolean } }
type Trip = { "tripId": String, "service": Service, "dep": UInt32 }

val mkService = (s: UInt32): Service =>
  { "startDate": s, "endDate": 20991231, "days": { 0: true, 1: true, 2: true, 3: true, 4: true, 5: true, 6: true }, "dates": { 20180615: true } }

val runsOn = (svc: Service, date: UInt32, dow: Int32): Boolean =>
  val exception = svc["dates"][date]
  if exception == true then true
  else if exception == false then false
  else if date < svc["startDate"] then false
  else if date > svc["endDate"] then false
  else svc["days"][dow] == true

val mkTrip = (id: String, dep: UInt32): Trip => { "tripId": id, "service": mkService(20180101), "dep": dep }

val gt = (trips: Trip[], time: UInt32): Trip | Null =>
  var i = trips.length() - 1
  var lastFound: Trip | Null = null
  while(() =>
    if i < 0 then false
    else
      val t = trips[i]
      if t["dep"] < time then false
      else
        val foundHere = t["service"].runsOn(20180615, 5)
        if foundHere then
          lastFound = t
        i = i - 1
        true
  )
  lastFound

val main = () =>
  val trips = [mkTrip("t1", 1000), mkTrip("t2", 1000), mkTrip("t3", 1000)]
  var n: Int32 = 0
  var last = "none"
  while(() =>
    if n >= 100000 then false
    else
      val r = gt(trips, 500)
      last = if r == null then "null" else r["tripId"]
      n = n + 1
      true
  )
  print(last)
main()
"#);
    // Last matching trip (highest index, since the scan walks downward and keeps the last write)
    // is "t1" (index 0, the final assignment). Survives 100k gt-calls × 3 record-var assigns.
    assert_eq!(out, vec!["t1"]);
}

#[test]
fn test_heap_sumnode_array_field_match_read() {
    // A variant with an Array field (Int32[]). Match-narrow and read the array length.
    let out = run(r#"import { print } from "std/io"
import { length } from "std/array"
type WithArr = { "kind": "arr", "items": Int32[], "n": Int32 }
type Plain   = { "kind": "plain", "n": Int32 }
type Mixed   = WithArr | Plain

val getCount = (m: Mixed): Int32 =>
  match m
    is WithArr => m["items"].length()
    is Plain   => m["n"]

val a: Mixed = { "kind": "arr", "items": [10, 20, 30], "n": 99 }
val b: Mixed = { "kind": "plain", "n": 7 }
print("${getCount(a)}")
print("${getCount(b)}")
"#);
    assert_eq!(out, vec!["3", "7"]);
}

#[test]
fn test_heap_sumnode_toString_materializer() {
    // A heap-field SumNode that escapes to toString — exercises the materializer
    // (get_or_build_sumnode_materializer), which must correctly box String/Array fields.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Lit  = { "kind": "lit", "val": Int32, "name": String }
type Pair = { "kind": "pair", "name": String, "left": Int32, "right": Int32 }
type Expr = Lit | Pair

val e1: Expr = { "kind": "lit", "val": 42, "name": "answer" }
val e2: Expr = { "kind": "pair", "name": "sum", "left": 1, "right": 2 }
print(e1.toString())
print(e2.toString())
"#);
    // toString of a SumNode materializes it → a LinObject with the correct fields.
    // The exact JSON ordering may vary; assert the key substrings are present.
    let combined = out.join("\n");
    assert!(combined.contains("\"kind\""), "expected kind in toString: {combined}");
    assert!(combined.contains("answer"), "expected 'answer' in toString: {combined}");
    assert!(combined.contains("sum"), "expected 'sum' in toString: {combined}");
}

#[test]
fn test_heap_sumnode_three_variants_string() {
    // Three-variant union all with String fields — exercises the multi-variant
    // tag switch in the descriptor and the direct-read fast path.
    let out = run(r#"import { print } from "std/io"
type Red   = { "kind": "red",   "msg": String, "v": Int32 }
type Green = { "kind": "green", "msg": String, "v": Int32 }
type Blue  = { "kind": "blue",  "msg": String, "v": Int32 }
type Color = Red | Green | Blue

val show = (c: Color): String =>
  match c
    is Red   => "R:${c["msg"]}=${c["v"]}"
    is Green => "G:${c["msg"]}=${c["v"]}"
    is Blue  => "B:${c["msg"]}=${c["v"]}"

val r: Color = { "kind": "red",   "msg": "fire",  "v": 1 }
val g: Color = { "kind": "green", "msg": "grass", "v": 2 }
val b: Color = { "kind": "blue",  "msg": "sky",   "v": 3 }
print(show(r))
print(show(g))
print(show(b))
"#);
    assert_eq!(out, vec!["R:fire=1", "G:grass=2", "B:sky=3"]);
}


// ============================================================================
// REPRESENTATION-RESET Stage-0 behaviour pins (docs/project-actually-improve-performance.md §6).
//
// Each `reset_pin_` test PINS today's observable behaviour where the reset will deliberately
// change it. The stage that changes a behaviour FLIPS the assertion in the same commit — a pin
// failing in any other circumstance is an unintended regression. The non-pin tests in this
// section are permanent correctness tests for the bare-record-map-value fix that Stage-0
// probing surfaced.
// ============================================================================

// D5 pin: array-push aliasing is REPRESENTATION-DEPENDENT today — a packed record array's push
// COPIES the element (mutating the source afterwards is NOT visible through the array), while a
// AnyVal array's push SHARES the pointer (mutation IS visible). End-state (Stage 1 flips the packed
// half): share-always — both observe 5.
#[test]
fn test_reset_pin_d5_push_aliasing_split() {
    let out = run(r#"import { print } from "std/io"
import { push } from "std/array"
type P = { "x": Int32, "y": Int32 }
val main = () =>
  var arr: P[] = [{ "x": 1, "y": 2 }]
  val t: P = { "x": 9, "y": 9 }
  push(arr, t)
  t["x"] = 5
  print("packed=${arr[1]["x"]}")
  var jarr: AnyVal = [{ "x": 1, "y": 2 }]
  val jt: AnyVal = { "x": 9, "y": 9 }
  push(jarr, jt)
  jt["x"] = 5
  print("json=${jarr[1]["x"]}")
main()
"#);
    // Stage 1 done: pointer-backed arrays share — both now observe 5.
    assert_eq!(out, vec!["packed=5", "json=5"]);
}

// D8/D4 pin: AnyVal-widening aliasing is REPRESENTATION-DEPENDENT today — widening a PACKED record
// to AnyVal materializes a copy (mutation through the record is NOT visible via the AnyVal alias),
// while a BOXED record (unpackable Function field) shares. The boxed half is the D8 wrap-not-copy
// invariant and must hold through Stages 1–5; the packed half flips to share when the record→
// AnyVal boundary carries the pointer (D4, Stage 6).
#[test]
fn test_reset_pin_d8_json_widening_aliasing_split() {
    let out = run(r#"import { print } from "std/io"
type P = { "x": Int32, "y": Int32 }
val main = () =>
  val r: P = { "x": 1, "y": 2 }
  val j: AnyVal = r
  r["x"] = 99
  print("packed=${j["x"]}")
  val f = (n: Int32): Int32 => n + 1
  val b = { "fn": f, "x": 1 }
  val jb: AnyVal = b
  b["x"] = 99
  print("boxed=${jb["x"]}")
main()
"#);
    // Stage 6a (TAG_RECORD): packed widening now SHARES (99) — the AnyVal alias holds a TAG_RECORD
    // pointer to the same sealed struct; mutation is visible through the alias. Boxed widening
    // also shares (99) unchanged. Both are 99 after Stage 6a. Pre-6a: packed=1 (copy).
    assert_eq!(out, vec!["packed=99", "boxed=99"]);
}

// Stage 6a directed test: TAG_RECORD (sealed struct by pointer in a dynamic TaggedVal slot).
// Exercises the new TAG_RECORD boxing: field read, equality (record ↔ record, record ↔ AnyVal
// object literal), toString, and fromJson round-trip. Proves the repr is correct end-to-end.
#[test]
fn test_tag_record_directed() {
    let out = run(r#"
import { print } from "std/io"
import { toString } from "std/string"
import { fromJson } from "std/json"
type P = { "x": Int32, "y": Int32 }
val p: P = { "x": 10, "y": 20 }
// Widen sealed record to AnyVal (TAG_RECORD O(1) wrap)
val j: AnyVal = p
// Field read through the AnyVal alias
print(toString(j["x"]))
print(toString(j["y"]))
// Equality: TAG_RECORD vs identical TAG_OBJECT literal
val lit: AnyVal = { "x": 10, "y": 20 }
print(if j == lit then "eq_object" else "ne_object")
// Equality: TAG_RECORD vs itself
print(if j == j then "eq_self" else "ne_self")
// toString
print(toString(j))
// fromJson round-trip: decode the TAG_RECORD box back into a typed P
val decoded = fromJson(P, j)
print(if decoded is P then "fromJson_ok" else "fromJson_fail")
"#);
    // Field reads return the original values.
    // Equality: j == lit (same shape/values, order-independent) = true; j == j = true.
    // toString: object form (key order may vary, but both keys present).
    // fromJson successfully decodes the TAG_RECORD box as P.
    assert_eq!(out[0], "10");
    assert_eq!(out[1], "20");
    assert_eq!(out[2], "eq_object");
    assert_eq!(out[3], "eq_self");
    // toString is an object literal — just check it contains both keys+values.
    assert!(out[4].contains("\"x\"") && out[4].contains("10"), "toString(j) missing x: {}", out[4]);
    assert!(out[4].contains("\"y\"") && out[4].contains("20"), "toString(j) missing y: {}", out[4]);
    assert_eq!(out[5], "fromJson_ok");
}

// D3 pin: width-subtyping into anonymous-structural slots is REPRESENTATION-DEPENDENT today —
// a BOXED wide record (unpackable AnyVal field) SHARES through an anon param and an anon-typed
// array element (mutation visible), a PACKED wide record COPIES (materialized at the boundary).
// End-state (D3): direct params monomorphise (share, all reprs); non-param slots project-copy
// (the boxed array-elem half flips to copy at Stage 1).
#[test]
fn test_reset_pin_d3_anon_slot_aliasing_split() {
    let out = run(r#"import { print } from "std/io"
import { push } from "std/array"
type Wide = { "type": String, "extra": Int32 }
val mutAnon = (r: { "type": String }) =>
  r["type"] = "mutated"
val main = () =>
  val jx: AnyVal = 7
  val bw = { "type": "orig", "blob": jx }
  mutAnon(bw)
  print("boxed-param=${bw["type"]}")
  val bw2 = { "type": "orig2", "blob": jx }
  var barr: { "type": String }[] = []
  push(barr, bw2)
  bw2["type"] = "mut2"
  print("boxed-elem=${barr[0]["type"]}")
  val pw: Wide = { "type": "orig3", "extra": 7 }
  mutAnon(pw)
  print("packed-param=${pw["type"]}")
  val pw2: Wide = { "type": "orig4", "extra": 7 }
  var parr: { "type": String }[] = []
  push(parr, pw2)
  pw2["type"] = "mut4"
  print("packed-elem=${parr[0]["type"]}")
main()
"#);
    // D3a: anon-structural params monomorphised per concrete layout → packed-param shares too.
    // D3b: non-param anon slots PROJECT-COPY → boxed-elem severed (orig2, not mut2).
    assert_eq!(out, vec![
        "boxed-param=mutated",
        "boxed-elem=orig2",
        "packed-param=mutated",
        "packed-elem=orig4",
    ]);
}

// D3a: mutation-through-param for both packed and boxed wide records.
#[test]
fn test_d3a_mutation_through_param_packed() {
    let out = run(r#"import { print } from "std/io"
type Wide = { "x": Int32, "y": Int32 }
val setX = (r: { "x": Int32 }) =>
  r["x"] = 99
val main = () =>
  val w: Wide = { "x": 1, "y": 2 }
  setX(w)
  print("x=${w["x"]} y=${w["y"]}")
main()
"#);
    // D3a: packed wide record param is specialised → shared; mutation visible in caller.
    assert_eq!(out, vec!["x=99 y=2"]);
}

#[test]
fn test_d3a_mutation_through_param_boxed() {
    let out = run(r#"import { print } from "std/io"
val setType = (r: { "type": String }) =>
  r["type"] = "changed"
val main = () =>
  val jx: AnyVal = 0
  val w = { "type": "orig", "blob": jx }
  setType(w)
  print("type=${w["type"]}")
main()
"#);
    // Boxed wide records already share through anon params (boxed path unchanged).
    assert_eq!(out, vec!["type=changed"]);
}

// D3a: exact-shape call (arg type exactly matches param type) must keep working.
#[test]
fn test_d3a_exact_shape_call() {
    let out = run(r#"import { print } from "std/io"
val setX = (r: { "x": Int32 }) =>
  r["x"] = 42
val main = () =>
  val w = { "x": 1 }
  setX(w)
  print("x=${w["x"]}")
main()
"#);
    assert_eq!(out, vec!["x=42"]);
}

// D3a: two different concrete layouts calling the same anon-param function → two specialisations.
#[test]
fn test_d3a_two_different_layouts_two_specs() {
    let out = run(r#"import { print } from "std/io"
type A = { "v": Int32, "a": Int32 }
type B = { "v": Int32, "b": String }
val setV = (r: { "v": Int32 }) =>
  r["v"] = 77
val main = () =>
  val wa: A = { "v": 1, "a": 10 }
  val wb: B = { "v": 2, "b": "hello" }
  setV(wa)
  setV(wb)
  print("a=${wa["v"]} b=${wb["v"]}")
main()
"#);
    // Each layout gets its own specialisation; mutations through both are visible.
    assert_eq!(out, vec!["a=77 b=77"]);
}

// D3a regression: 4-cell matrix — all combinations of (direct/closure-captured param) ×
// (inferred-literal arg / annotated-sealed arg) must produce the correct value.
// The bug was: spec's inner closure `get` and the original's inner closure `get` shared the same
// LLVM symbol, causing the original's closure to run the spec's sealed-offset body → garbage.
#[test]
fn test_d3a_closure_captured_param_inferred_literal_matrix() {
    let out = run(r#"import { print } from "std/io"
type Sealed = { "n": Int32, "extra": String }
val direct = (r: { "n": Int32 }): Int32 => r["n"]
val direct2 = (r: { "n": Int32 }): Int32 => r["n"]
val viaClosure = (r: { "n": Int32 }): Int32 =>
  val get = () => r["n"]
  get()
val main = () =>
  val jLit = { "n": 7, "extra": "e" }
  val sLit: Sealed = { "n": 7, "extra": "e" }
  print("case1=${direct(jLit)}")
  print("case2=${direct2(sLit)}")
  print("case3=${viaClosure(sLit)}")
  print("case4=${viaClosure(jLit)}")
main()
"#);
    assert_eq!(out, vec!["case1=7", "case2=7", "case3=7", "case4=7"]);
}

// D3a cross-module: an imported anon-param fn is now monomorphised per-layout in the importing
// module (D3 cross-module leg). The specialised body reads/writes at the CALLER's concrete record
// offsets, so mutation through the param IS visible in the caller — same as same-module D3a.
#[test]
fn test_d3a_cross_module_anon_param_shares() {
    // Two-module fixture: imported anon-param fn `stamp`, wider record arg `w`.
    // D3 cross-module: mutation through the param IS visible (same-module D3a behaviour).
    let dir = std::env::temp_dir().join(format!("lin_d3a_xmod_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("stamp.lin"),
        "export val stamp = (r: { \"v\": Int32 }) => r[\"v\"] = 99\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ stamp }} from "{}/stamp"
type Wide = {{ "v": Int32, "extra": String }}
val main = () =>
  val w: Wide = {{ "v": 1, "extra": "x" }}
  stamp(w)
  print("v=${{w["v"]}}")
main()
"#, dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    // D3 cross-module: mutation through the imported anon-param is now visible (shares, not copies).
    assert_eq!(output, vec!["v=99"]);
}

// D3a cross-module inner-closure symbol collision guard: when the imported anon-param fn contains
// an inner named closure, cross-module specialisation must call rename_inner_fns so the spec's
// inner closure gets a distinct LLVM symbol from the original's. Without the rename, both specs
// (original + layout-specialised clone) would register the same symbol `@inner`; codegen's
// get_function dedup would keep the first body for both → wrong closure body runs for one caller.
#[test]
fn test_d3a_cross_module_inner_closure_symbol_dedup() {
    let dir = std::env::temp_dir().join(format!("lin_d3a_xmod_inner_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    // `apply` has an inner named closure `adder` and an anon-structural param `r`.
    std::fs::write(dir.join("apply.lin"), concat!(
        "export val apply = (r: { \"v\": Int32 }, delta: Int32) =>\n",
        "  val adder = (x: Int32) => x + delta\n",
        "  r[\"v\"] = adder(r[\"v\"])\n",
    )).unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ apply }} from "{}/apply"
type Wide = {{ "v": Int32, "extra": String }}
val main = () =>
  val w: Wide = {{ "v": 10, "extra": "x" }}
  apply(w, 5)
  print("v=${{w["v"]}}")
main()
"#, dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    // Inner closure runs correctly in the specialised body (v = 10 + 5 = 15).
    assert_eq!(output, vec!["v=15"]);
}

// ============================================================================
// D3b: anon-structural non-param slot project-copy.
// A WIDER unsealed boxed object flowing into a NARROWER unsealed object slot (array element,
// map value) must PROJECT-COPY — building a fresh LinObject with only the slot's fields,
// severing sharing and dropping extra fields. Exact-shape pass-through (F_src == F_slot) must
// NOT copy (no projection when fields are identical).
// ============================================================================

// D3b: boxed wide source pushed into a narrower-typed array slot → extras dropped, mutation severed.
#[test]
fn test_d3b_array_elem_boxed_projects() {
    let out = run(r#"import { print } from "std/io"
import { push } from "std/array"
val main = () =>
  val jx: AnyVal = 0
  val wide = { "type": "orig", "extra": 42, "blob": jx }
  var arr: { "type": String }[] = []
  push(arr, wide)
  wide["type"] = "changed"
  print("elem=${arr[0]["type"]} extras_dropped=${arr[0]["extra"] ?? "null"}")
main()
"#);
    // D3b: projection severs sharing (elem stays "orig") and drops "extra" field.
    assert_eq!(out, vec!["elem=orig extras_dropped=null"]);
}

// D3b: exact-shape match — same field set → no projection, mutation IS visible.
#[test]
fn test_d3b_array_elem_exact_shape_shares() {
    let out = run(r#"import { print } from "std/io"
import { push } from "std/array"
val main = () =>
  val jx: AnyVal = 0
  val exact = { "type": "orig", "blob": jx }
  var arr: { "type": String, "blob": AnyVal }[] = []
  push(arr, exact)
  exact["type"] = "changed"
  print("elem=${arr[0]["type"]}")
main()
"#);
    // Exact same shape → no projection → mutation still visible through the slot.
    assert_eq!(out, vec!["elem=changed"]);
}

// D3b: boxed wide source stored as a narrower-typed map value → extras dropped, mutation severed.
#[test]
fn test_d3b_map_value_anon_slot_projects() {
    let out = run(r#"import { print } from "std/io"
val main = () =>
  val jx: AnyVal = 0
  val wide = { "type": "orig", "extra": 99, "blob": jx }
  var m: { String: { "type": String } } = {}
  m["key"] = wide
  wide["type"] = "changed"
  print("val=${m["key"]["type"]} extras_dropped=${m["key"]["extra"] ?? "null"}")
main()
"#);
    // D3b: projection severs sharing (val stays "orig") and drops "extra" field.
    assert_eq!(out, vec!["val=orig extras_dropped=null"]);
}

// D3b: return type annotation — wide body type narrowed at return boundary → projection copy.
#[test]
fn test_d3b_return_type_anon_slot_projects() {
    let out = run(r#"import { print } from "std/io"
val narrow = (w: { "type": String, "extra": Int32 }) : { "type": String } =>
  w
val main = () =>
  val wide = { "type": "orig", "extra": 42 }
  val r = narrow(wide)
  wide["type"] = "changed"
  print("ret=${r["type"]} extra=${r["extra"] ?? "null"}")
main()
"#);
    // D3b: return type annotation projects a fresh copy — mutation of wide is severed,
    // extra field is dropped from the returned object.
    assert_eq!(out, vec!["ret=orig extra=null"]);
}

// D3b: stored closure — wide arg projected at call boundary → extras dropped, mutation severed.
#[test]
fn test_d3b_stored_closure_arg_projects() {
    let out = run(r#"import { print } from "std/io"
val main = () =>
  var result = "init"
  val stored: (({ "type": String }) => Null) = (r: { "type": String }) =>
    result = r["type"]
  val wide = { "type": "orig", "extra": 42 }
  stored(wide)
  wide["type"] = "changed"
  print("captured=${result} wide=${wide["type"]}")
main()
"#);
    // D3b: stored closure receives a projected copy — closure reads "type" from the projected
    // copy ("orig"), and wide can change independently afterwards.
    assert_eq!(out, vec!["captured=orig wide=changed"]);
}

// D2 pin: a record with a Function field can be widened into AnyVal TODAY. Stage 6 flips this to a
// compile error (AnyVal transitivity rule: only transitively value-shaped records widen).
#[test]
fn test_reset_pin_d2_handle_record_widens_into_json_today() {
    let out = run(r#"import { print } from "std/io"
val main = () =>
  val f = (n: Int32): Int32 => n + 1
  val h = { "fn": f, "n": 1 }
  val jh: AnyVal = h
  print("n=${jh["n"]}")
main()
"#);
    assert_eq!(out, vec!["n=1"]);
}

// §5.7 invariant (NOT a pin — must hold at every stage): a typed (packed) record compares equal
// to a structurally-equal AnyVal object, order-independently. This is the cross-form equality that
// guards the D8 transitional period.
#[test]
fn test_reset_crossform_record_json_equality() {
    let out = run(r#"import { print } from "std/io"
type P = { "x": Int32, "y": Int32 }
val main = () =>
  val a: P = { "x": 1, "y": 2 }
  val jb: AnyVal = { "y": 2, "x": 1 }
  print("eq=${a == jb}")
main()
"#);
    assert_eq!(out, vec!["eq=true"]);
}

// ============================================================================
// Bare-record map values (the Stage-0 discovery fix). A bare packed sealed record stored as a
// `{ String: T }` map VALUE was keep-packed in the slot (TAG_OBJECT wrapping a sealed struct that
// is NOT a LinObject) while every read path assumed a boxed object → lin_object_get read sealed
// bytes as a LinObject header: index-cap underflow panic (heap-field records) or silent
// corruption/abort (all-scalar records). Map record slots now hold a MATERIALIZED boxed object
// (emit_map_set falls through to box_value), and a narrowed bare-record read projects it back to
// a fresh sealed struct. Sealed ARRAYS as map values keep their (sound, tag-dispatched)
// keep-packed path.
// ============================================================================

#[test]
fn test_map_bare_record_value_roundtrip() {
    // heap-field record (String field): union read + narrow — panicked before the fix.
    let out = run(r#"import { print } from "std/io"
type Wide = { "type": String, "extra": Int32 }
val main = () =>
  val w: Wide = { "type": "a", "extra": 7 }
  var m: { String: Wide } = {}
  m["k"] = w
  val got = m["k"]
  if got != null then
    print("got=${got["type"]} extra=${got["extra"]}")
main()
"#);
    assert_eq!(out, vec!["got=a extra=7"]);
}

#[test]
fn test_map_bare_record_value_all_scalar() {
    // all-scalar record: silently produced no output / aborted before the fix.
    let out = run(r#"import { print } from "std/io"
type P = { "x": Int32, "y": Int32 }
val main = () =>
  val p: P = { "x": 1, "y": 2 }
  var m: { String: P } = {}
  m["k"] = p
  val got = m["k"]
  if got != null then
    print("x=${got["x"]} y=${got["y"]}")
  else
    print("missing")
main()
"#);
    assert_eq!(out, vec!["x=1 y=2"]);
}

#[test]
fn test_map_bare_record_value_coalesce_and_named_narrow() {
    // ?? coalesce read (bare-T result) and a 5.9.1 named-narrow projection store — both panicked.
    // ADR-076: a present-key read AFTER `m["k"] = w` now narrows to the assigned non-null `Wide`,
    // so `m["k"] ?? d` there would be a "left operand is never null" error (the default is dead).
    // The bare-T present-key read is exercised directly; `m["absent"] ?? d` keeps the genuine
    // coalesce-over-`Null` path (the absent key is not narrowed).
    let out = run(r#"import { print } from "std/io"
type Wide = { "type": String, "extra": Int32 }
type Narrow = { "type": String }
val main = () =>
  val w: Wide = { "type": "a", "extra": 7 }
  var m: { String: Wide } = {}
  m["k"] = w
  val d: Wide = { "type": "dflt", "extra": 0 }
  val got: Wide = m["k"]
  print("coalesce=${got["type"]}")
  val miss: Wide = m["absent"] ?? d
  print("miss=${miss["type"]}")
  val w2: Wide = { "type": "b", "extra": 7 }
  var m2: { String: Narrow } = {}
  m2["k"] = w2
  val g2 = m2["k"]
  if g2 != null then print("narrowstore=${g2["type"]}")
main()
"#);
    assert_eq!(out, vec!["coalesce=a", "miss=dflt", "narrowstore=b"]);
}

#[test]
fn test_map_bare_record_value_narrowed_index_read() {
    // index-place narrowing gives the second m["k"] a BARE Wide result type — exercises the
    // boxed-slot → fresh-sealed projection read arm.
    let out = run(r#"import { print } from "std/io"
type Wide = { "type": String, "extra": Int32 }
val main = () =>
  val w: Wide = { "type": "nr", "extra": 7 }
  var m: { String: Wide } = {}
  m["k"] = w
  if m["k"] != null then
    print("narrowed=${m["k"]["type"]} extra=${m["k"]["extra"]}")
main()
"#);
    assert_eq!(out, vec!["narrowed=nr extra=7"]);
}

#[test]
fn test_map_bare_record_value_overwrite_no_leak_smoke() {
    // Overwrite the same keys many times: each store releases the previous materialized slot
    // object. (The RSS-flat scaling proof lives in the Stage-0 baseline notes; this is the
    // correctness smoke — values must be the LAST write's.)
    let out = run(r#"import { print } from "std/io"
import { range, for } from "std/iter"
type Wide = { "type": String, "extra": Int32 }
val main = () =>
  var m: { String: Wide } = {}
  range(0, 1000).for(i =>
    m["k${i % 10}"] = { "type": "t${i}", "extra": i }
  )
  val g = m["k3"]
  if g != null then print("last=${g["extra"]}")
main()
"#);
    assert_eq!(out, vec!["last=993"]);
}

// ---------------------------------------------------------------------------
// Regression: generic inner-function LLVM symbol collision.
// When a generic function's body contains a NAMED inner function (`val inner = () => …`), every
// specialisation previously emitted the same LLVM symbol for `inner`. Codegen's
// `get_function`/`add_function` name-dedup silently kept the first body for ALL
// specialisations — so e.g. `pick$Int32` and `pick$String` both ran the first-minted closure
// body, causing a misaligned-pointer deref (Int32 tag `0x7` dereferenced as a pointer).
// Fix: `rename_inner_fns` in the worklist drain appends the spec's `$…` suffix to every inner
// named function so each spec has distinct LLVM symbols.
// ---------------------------------------------------------------------------

#[test]
fn test_generic_inner_named_fn_collision_basic() {
    // Repro from the original bug report: three specialisations of `pick` — two Int32 and one
    // String — each must run ITS OWN `inner` closure and return the correct first argument.
    let out = run(r#"import { print } from "std/io"
val pick = <T>(x: T, y: T): T =>
  val inner = () => x
  inner()
val main = () =>
  print("int=${pick(7, 8)}")
  print("str=${pick("a", "b")}")
  print("int2=${pick(42, 43)}")
main()
"#);
    assert_eq!(out, vec!["int=7", "str=a", "int2=42"]);
}

#[test]
fn test_generic_inner_named_fn_captures_and_mutates() {
    // Variant: inner closure captures the generic param AND a `var` counter. Each specialisation
    // must capture its own `x` value (not bleed across specialisations).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val accumulate = <T>(x: T): (() => T) =>
  var seen = 0
  val capture = () =>
    seen = seen + 1
    x
  capture
val main = () =>
  val getInt = accumulate(99)
  val getStr = accumulate("hello")
  print(toString(getInt()))
  print(getStr())
  print(toString(getInt()))
main()
"#);
    assert_eq!(out, vec!["99", "hello", "99"]);
}

#[test]
fn test_generic_inner_named_fn_callback_devirt_axis() {
    // CallbackDevirt-axis variant: a user HOF with an inner named function is called with two
    // distinct named no-capture predicates, minting two devirt specs. Each spec's `inner` must
    // get a distinct LLVM symbol (the same collision would occur on the devirt spec as on the
    // generic spec if rename_inner_fns were not applied in the shared worklist drain).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { for } from "std/iter"
val applyCount = <T>(xs: T[], pred: (T) => Boolean): Int32 =>
  val inner = (x: T) => pred(x)
  var count = 0
  xs.for(x => if inner(x) then count = count + 1)
  count
val isEven = (x: Int32) => x % 2 == 0
val isBig  = (x: Int32) => x > 5
val xs = [1, 2, 3, 4, 6, 8]
print(toString(applyCount(xs, isEven)))
print(toString(applyCount(xs, isBig)))
"#);
    assert_eq!(out, vec!["4", "2"]);
}

// ─────────────────── Sealed-record-ARRAY with HEAP fields (Stage 2a) ────────────────────
// Stage 2a widens the `is_sealed_array_field_packable` gate to cover String / Array / Map /
// nested-sealed fields. A `Person[]` (or any heap-field record array) now goes through the
// 0xFD pointer-backed path: array slots hold 8-byte struct pointers, index loads the pointer
// then GEPs at const-offset — no per-read materialization. Drop walks the descriptor via
// `lin_sealed_release_self`.

#[test]
fn test_reset_stage2a_heapfield_push_share_string() {
    // Push a heap-field record into a typed array; mutate the String field on the original;
    // confirm arr[0]["id"] sees the mutation (0xFD pointer-backed share-on-push).
    let out = run(r#"import { print } from "std/io"
import { push } from "std/array"
type Entry = { "id": String, "n": Int32 }
val main = () =>
  var arr: Entry[] = []
  val e: Entry = { "id": "hello", "n": 1 }
  push(arr, e)
  e["id"] = "world"
  print("arr0=${arr[0]["id"]}")
  print("orig=${e["id"]}")
main()
"#);
    // 0xFD: pointer-backed share-on-push — mutation visible through arr[0].
    assert_eq!(out, vec!["arr0=world", "orig=world"]);
}

#[test]
fn test_reset_stage2a_heapfield_index_share_both_ways() {
    // Mutate through arr[0], confirm original shares; mutate original, confirm arr[0] shares.
    let out = run(r#"import { print } from "std/io"
import { push } from "std/array"
type Entry = { "id": String, "n": Int32 }
val main = () =>
  var arr: Entry[] = []
  val e: Entry = { "id": "initial", "n": 0 }
  push(arr, e)
  arr[0]["id"] = "via_arr"
  print("e_after_arr_mut=${e["id"]}")
  e["id"] = "via_orig"
  print("arr0_after_orig_mut=${arr[0]["id"]}")
main()
"#);
    assert_eq!(out, vec!["e_after_arr_mut=via_arr", "arr0_after_orig_mut=via_orig"]);
}

#[test]
fn test_reset_stage2a_heapfield_tostring_and_eq() {
    // toString and eq on a heap-field record array.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Person = { "name": String, "age": Int32 }
val ps: Person[] = [{ "name": "ann", "age": 30 }, { "name": "bob", "age": 41 }]
print(toString(ps))
val ps2: Person[] = [{ "name": "ann", "age": 30 }, { "name": "bob", "age": 41 }]
print(toString(ps == ps2))
"#);
    assert_eq!(out, vec![
        r#"[{"name": "ann", "age": 30}, {"name": "bob", "age": 41}]"#,
        "true",
    ]);
}

#[test]
fn test_reset_stage2a_heapfield_for_map_filter_typed_param() {
    // for/map/filter over a heap-field record array with typed params.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { for, map, filter } from "std/iter"
import { length } from "std/array"
type Person = { "name": String, "age": Int32 }
val ps: Person[] = [
  { "name": "ann", "age": 30 },
  { "name": "bob", "age": 41 },
  { "name": "cat", "age": 7 }
]
val names: String[] = map(ps, (p: Person) => p["name"])
print(toString(names))
val adults: Person[] = filter(ps, (p: Person) => p["age"] >= 18)
print(toString(length(adults)))
var acc = ""
ps.for((p: Person) => acc = "${acc}${p["name"]} ")
print(acc)
"#);
    assert_eq!(out, vec![
        r#"["ann", "bob", "cat"]"#,
        "2",
        "ann bob cat ",
    ]);
}

#[test]
fn test_reset_stage2a_heapfield_json_widening() {
    // AnyVal-widening of a 0xFD heap-field element: accessing arr[0] as AnyVal must still read correctly.
    let out = run(r#"import { print } from "std/io"
type Person = { "name": String, "age": Int32 }
val ps: Person[] = [{ "name": "ann", "age": 30 }, { "name": "bob", "age": 41 }]
val j: AnyVal = ps[0]
print("${j["name"]} ${j["age"]}")
"#);
    assert_eq!(out, vec!["ann 30"]);
}

#[test]
fn test_reset_stage2a_heapfield_nested_array_of_arrays() {
    // Nested Trip[][] — array of arrays of records (each row is a typed record array).
    let out = run(r#"import { print } from "std/io"
import { push, length } from "std/array"
import { toString } from "std/string"
type Stop = { "id": String, "time": Int32 }
val main = () =>
  var outer: Stop[][] = []
  val row1: Stop[] = [{ "id": "s1", "time": 10 }, { "id": "s2", "time": 20 }]
  val row2: Stop[] = [{ "id": "s3", "time": 30 }]
  push(outer, row1)
  push(outer, row2)
  print(toString(length(outer)))
  print(toString(length(outer[0])))
  print("${outer[0][0]["id"]} ${outer[0][1]["time"]}")
  print("${outer[1][0]["id"]}")
main()
"#);
    assert_eq!(out, vec!["2", "2", "s1 20", "s3"]);
}

#[test]
fn test_reset_stage2a_heapfield_map_value_store_count() {
    // {String: Trip[]} map value — store an array of heap-field records as a map value and count it.
    // Scope fence: the map-value boundary keeps arrays materialized-boxed (emit_map_set unchanged).
    // Indexing a Stop[] that came out of a map causes a repr-oracle disagreement (the type says
    // packable but the dataflow repr says Boxed at the union/map boundary). This test uses only
    // `length` (which dispatches on the runtime tag, not via the sealed fast path) to confirm
    // the value round-trips correctly without crossing the packed-Index fence.
    let out = run(r#"import { print } from "std/io"
import { push, length } from "std/array"
import { toString } from "std/string"
type Stop = { "id": String, "time": Int32 }
val main = () =>
  var routes: { String: Stop[] } = {}
  val stops: Stop[] = [{ "id": "a", "time": 1 }, { "id": "b", "time": 2 }]
  routes["r1"] = stops
  val got: Stop[] = routes["r1"]
  print(toString(length(got)))
main()
"#);
    // ADR-076: after `routes["r1"] = stops`, the re-read narrows to the assigned non-null `Stop[]`,
    // so it is directly assignable to a `Stop[]` binding (a `?? []` default would now be dead).
    assert_eq!(out, vec!["2"]);
}

#[test]
fn test_reset_stage2a_tco_union_param_heapfield_array() {
    // Regression: TCO self-recursion threading a `Trip | Null` union param where `Trip` has an
    // 0xFD-classed array field (`sts: St[]`). Earlier: `box_value_yields_fresh_owned` returned
    // `true` for sealed-record arrays even after Stage-2a changed `box_value` to `lin_box_array`
    // (borrowed pointer) — `sealed_materialize_to_object` then called `lin_tagged_release`
    // (releases inner + frees shell) instead of `lin_tagged_free_box` → UAF on re-read.
    // Also: `array_coerce_elementwise` over a union-element outer array released the source
    // element box after `push_tagged_val` already transferred the +1 (passthrough coerce) → UAF.
    let out = run(r#"import { print } from "std/io"
import { push, length } from "std/array"
import { range, for } from "std/iter"
type St = { "stop": String, "dep": Int32 }
type Trip = { "id": String, "sts": St[] }
val find = (arr: Trip[], i: Int32): Trip | Null =>
  if i < length(arr) then arr[i] else null
val scan = (arr: Trip[], pi: Int32, n: Int32, trip: Trip | Null, acc: Int32): Int32 =>
  if pi >= n then
    acc
  else
    match trip
      is Trip =>
        val d = trip["sts"][0]["dep"]
        scan(arr, pi + 1, n, trip, acc + d)
      else =>
        val nt = find(arr, pi)
        match nt
          is Trip =>
            scan(arr, pi + 1, n, nt, acc + 1)
          else =>
            scan(arr, pi + 1, n, null, acc)
val main = () =>
  var arr: Trip[] = []
  var i = 0
  range(0, 4).for(k =>
    var sts: St[] = []
    push(sts, { "stop": "s${k}", "dep": k * 10 })
    push(arr, { "id": "t${k}", "sts": sts })
  )
  print("scan=${scan(arr, 0, 8, null, 0)}")
main()
"#);
    assert_eq!(out, vec!["scan=1"]);
}

// ── Stage 6a Leg-3: lin_parse_json(object) → TAG_RECORD ─────────────────────

#[test]
fn test_stage6a_leg3_fromjson_builds_tag_record() {
    // Verify that readJson over an object payload produces a TAG_RECORD (sealed struct),
    // not a TAG_OBJECT (LinObject). Field reads, string interpolation, structural equality
    // with a code-constructed record, nested objects, and array-of-objects all exercise the
    // new descriptor-driven sealed-struct path.
    let tmp = std::env::temp_dir()
        .join(format!("lin_ctest_stage6a_leg3_{}.json", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let path = tmp.display().to_string();
    fs::write(&tmp, r#"{"x":1,"name":"a"}"#).unwrap();

    let out = run(&format!(r#"import {{ print }} from "std/io"
import {{ readJson }} from "std/fs"

val j1 = readJson("{path}")
// field reads
print("${{j1["x"]}}")
print("${{j1["name"]}}")
// structural equality with a code-constructed record (§5.8)
val rec = {{ "x": 1, "name": "a" }}
print("${{j1 == rec}}")
"#));
    let _ = fs::remove_file(&tmp);
    assert_eq!(out, vec!["1", "a", "true"]);
}

#[test]
fn test_stage6a_leg3_fromjson_nested_object() {
    // Nested object: j["p"]["q"] traversal.
    let tmp = std::env::temp_dir()
        .join(format!("lin_ctest_stage6a_leg3_nested_{}.json", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let path = tmp.display().to_string();
    fs::write(&tmp, r#"{"p":{"q":2}}"#).unwrap();

    let out = run(&format!(r#"import {{ print }} from "std/io"
import {{ readJson }} from "std/fs"

val j2 = readJson("{path}")
print("${{j2["p"]["q"]}}")
"#));
    let _ = fs::remove_file(&tmp);
    assert_eq!(out, vec!["2"]);
}

#[test]
fn test_stage6a_leg3_fromjson_array_of_objects() {
    // Array of objects: j[0]["v"] and j[1]["v"].
    let tmp = std::env::temp_dir()
        .join(format!("lin_ctest_stage6a_leg3_arr_{}.json", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let path = tmp.display().to_string();
    fs::write(&tmp, r#"[{"v":10},{"v":20}]"#).unwrap();

    let out = run(&format!(r#"import {{ print }} from "std/io"
import {{ readJson }} from "std/fs"

val j3 = readJson("{path}")
print("${{j3[0]["v"]}}")
print("${{j3[1]["v"]}}")
"#));
    let _ = fs::remove_file(&tmp);
    assert_eq!(out, vec!["10", "20"]);
}

// ── Regression: named-record `toBe` (sealed-record materialize-to-map alignment) ───────────────
//
// `toBe` deep-compares values by materializing sealed records into maps. Before the fix, named
// records whose fields included UInt8/UInt16/UInt32 (mapped to NKIND_INT64 → 8-byte read) would
// misalign reads for fields at 4-aligned offsets (e.g. the second UInt32 after a String field is
// at struct-offset 36, which is 4-aligned but not 8-aligned). This caused a misaligned-pointer
// panic at `lin_runtime::sealed::materialize_named_payload_to_map`. Fixed by adding distinct
// NKIND_UINT32/UINT16/UINT8 codes that use the correct 4/2/1-byte slot sizes.

#[test]
fn test_named_record_tobe_with_uint32_fields() {
    // { "a": String, "b": UInt32, "c": UInt32 } — "c" is at offset 36 (4-aligned, not 8-aligned).
    // The second UInt32 used to be read as i64 (8 bytes) → misaligned panic.
    let fixture = write_test_fixture(r#"import { expect, toBe, test, suite, run } from "std/test"
type R = { "a": String, "b": UInt32, "c": UInt32 }
val s = suite("sealed-tobe", [
  test("named record toBe passes (basic)", () =>
    val x: R = { "a": "A", "b": 9, "c": 5 }
    [ expect(x).toBe(x) ]
  )
])
run(s)
"#);
    let (success, lines) = run_test_json(&fixture, &[]);
    let _ = fs::remove_file(&fixture);
    let records: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("invalid JSON: {l}: {e}")))
        .collect();
    assert!(success, "named-record toBe must not panic; records:\n{records:?}");
    let pass = records.iter().any(|r| r["event"] == "test" && r["status"] == "pass");
    assert!(pass, "expected a passing test record; got:\n{records:?}");
}

#[test]
fn test_named_record_tobe_with_uint16_field() {
    // { "a": String, "u16": UInt16, "b": UInt32 } — UInt16 at offset 32, UInt32 at offset 36
    // (4-aligned). Both must be read at their natural slot widths (2 and 4 bytes respectively).
    let fixture = write_test_fixture(r#"import { expect, toBe, test, suite, run } from "std/test"
type R2 = { "a": String, "u16": UInt16, "b": UInt32 }
val s = suite("sealed-tobe-u16", [
  test("named record with UInt16 field", () =>
    val x: R2 = { "a": "hello", "u16": 42, "b": 100 }
    [ expect(x).toBe(x) ]
  )
])
run(s)
"#);
    let (success, lines) = run_test_json(&fixture, &[]);
    let _ = fs::remove_file(&fixture);
    let records: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("invalid JSON: {l}: {e}")))
        .collect();
    assert!(success, "UInt16-field named-record toBe must not panic; records:\n{records:?}");
    let pass = records.iter().any(|r| r["event"] == "test" && r["status"] == "pass");
    assert!(pass, "expected a passing test record; got:\n{records:?}");
}

#[test]
fn test_named_record_tobe_nested() {
    // { "outer": String, "svc": { "x": UInt32 } } — nested named record inside a named record.
    let fixture = write_test_fixture(r#"import { expect, toBe, test, suite, run } from "std/test"
type Inner = { "x": UInt32 }
type Outer = { "outer": String, "svc": Inner }
val s = suite("sealed-tobe-nested", [
  test("nested named record toBe", () =>
    val x: Outer = { "outer": "hello", "svc": { "x": 7 } }
    [ expect(x).toBe(x) ]
  )
])
run(s)
"#);
    let (success, lines) = run_test_json(&fixture, &[]);
    let _ = fs::remove_file(&fixture);
    let records: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("invalid JSON: {l}: {e}")))
        .collect();
    assert!(success, "nested named-record toBe must not panic; records:\n{records:?}");
    let pass = records.iter().any(|r| r["event"] == "test" && r["status"] == "pass");
    assert!(pass, "expected a passing test record; got:\n{records:?}");
}

// ---- Function overloading (ADR-074 / spec §14.6) ----

#[test]
fn test_overload_resolves_by_param_types() {
    // Overloads distinguished by parameter types; each call selects the right one.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Circle = { "radius": Float64 }
type Rect = { "width": Float64, "height": Float64 }
val area = (c: Circle): Float64 => 3.14159 * c["radius"] * c["radius"]
val area = (r: Rect): Float64 => r["width"] * r["height"]
val c: Circle = { "radius": 2.0 }
val r: Rect = { "width": 3.0, "height": 4.0 }
print(toString(area(c)))
print(toString(area(r)))
"#);
    assert_eq!(out, vec!["12.56636", "12.0"]);
}

#[test]
fn test_overload_dispatches_on_all_arguments() {
    // Selection considers the whole argument tuple, not just the first.
    let out = run(r#"import { print } from "std/io"
val combine = (a: Int32, b: Int32): String => "ints:${a + b}"
val combine = (a: Int32, b: String): String => "mix:${a}${b}"
print(combine(1, 2))
print(combine(7, "x"))
"#);
    assert_eq!(out, vec!["ints:3", "mix:7x"]);
}

#[test]
fn test_overload_concrete_preferred_over_generic() {
    // A concrete overload beats a generic one that matched only by instantiation.
    let out = run(r#"import { print } from "std/io"
val describe = <T>(x: T): String => "generic"
val describe = (x: Int32): String => "int"
print(describe(5))
print(describe("hi"))
"#);
    assert_eq!(out, vec!["int", "generic"]);
}

#[test]
fn test_overload_duplicate_signature_is_error() {
    let err = run_expect_err(r#"val f = (a: Int32): Int32 => a
val f = (a: Int32): String => "x"
"#);
    assert!(err.contains("duplicate definition"), "got: {err}");
}

#[test]
fn test_overload_no_matching_is_error() {
    let err = run_expect_err(r#"import { print } from "std/io"
import { toString } from "std/string"
val f = (a: Int32): Int32 => a
val f = (a: String): Int32 => 0
print(toString(f(true)))
"#);
    assert!(err.contains("no matching overload"), "got: {err}");
}

#[test]
fn test_overload_union_argument_is_error() {
    // A union argument matches no single overload — static-only resolution (§14.6).
    let err = run_expect_err(r#"import { print } from "std/io"
import { toString } from "std/string"
val f = (a: Int32): Int32 => a
val f = (a: String): Int32 => 0
val x: Int32 | String = 5
print(toString(f(x)))
"#);
    assert!(err.contains("no matching overload"), "got: {err}");
}

#[test]
fn test_overload_ambiguous_call_is_error() {
    // Both overloads match equally well; neither is more specific.
    let err = run_expect_err(r#"import { print } from "std/io"
import { toString } from "std/string"
type A = { "x": Int32 }
type B = { "y": Int32 }
val f = (a: A): Int32 => a["x"]
val f = (b: B): Int32 => b["y"]
val both: { "x": Int32, "y": Int32 } = { "x": 1, "y": 2 }
print(toString(f(both)))
"#);
    assert!(err.contains("ambiguous call"), "got: {err}");
}

#[test]
fn test_overload_bare_reference_is_error() {
    // An overloaded name cannot be used as a value — only called.
    let err = run_expect_err(r#"val f = (a: Int32): Int32 => a
val f = (a: String): Int32 => 0
val g = f
"#);
    assert!(err.contains("cannot be used as a value"), "got: {err}");
}

#[test]
fn test_overload_with_default_parameter() {
    // An overload with a default parameter is a candidate at every arity its default permits.
    let out = run(r#"import { print } from "std/io"
val g = (a: Int32, b: Int32 = 10): String => "int:${a + b}"
val g = (a: String): String => "str:${a}"
print(g(5))
print(g(5, 2))
print(g("hi"))
"#);
    assert_eq!(out, vec!["int:15", "int:7", "str:hi"]);
}

#[test]
fn test_overload_partial_application() {
    // Partial application of an overloaded function selects on the supplied prefix.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val h = (a: Int32, b: Int32): Int32 => a + b
val h = (a: String, b: String): String => "${a}${b}"
val add5 = h(5,)
print(toString(add5(3)))
"#);
    assert_eq!(out, vec!["8"]);
}

#[test]
fn test_overload_cross_module() {
    // ADR-074 cross-module: an imported overload set resolves at the importer's call sites.
    let dir = std::env::temp_dir().join(format!("lin_overload_xmod_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("shapes.lin"),
        "export val describe = (n: Int32): String => \"int:${n}\"\n\
         export val describe = (s: String): String => \"str:${s}\"\n\
         export val describe = (a: Int32, b: Int32): String => \"pair:${a + b}\"\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ describe }} from "{}/shapes"
print(describe(7))
print(describe("hi"))
print(describe(2, 3))
"#, dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["int:7", "str:hi", "pair:5"]);
}

#[test]
fn test_overload_cross_module_aliased() {
    // An aliased import of an overloaded name still resolves all overloads under the alias.
    let dir = std::env::temp_dir().join(format!("lin_overload_xmod_alias_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("lib.lin"),
        "export val enc = (n: Int32): String => \"i\"\n\
         export val enc = (s: String): String => \"s\"\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ enc as e }} from "{}/lib"
print(e(1))
print(e("x"))
"#, dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["i", "s"]);
}

#[test]
fn test_overload_cross_module_merged_by_receiver_type() {
    // ADR-074 cross-module merge: importing the SAME function name from TWO separate modules
    // must form a single overload set (not shadow). Selection at the call site by receiver type.
    let dir = std::env::temp_dir().join(format!("lin_overload_xmod_merge_recv_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("ma.lin"),
        "export type A = { \"a\": Int32 }\n\
         export val create = (x: A, n: Int32): Int32 => x[\"a\"] + n\n").unwrap();
    std::fs::write(dir.join("mb.lin"),
        "export type B = { \"b\": Int32 }\n\
         export val create = (y: B, s: String): String => \"v${s}\"\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ A, create }} from "{dir}/ma"
import {{ B, create }} from "{dir}/mb"
val a: A = {{ "a": 1 }}
val b: B = {{ "b": 2 }}
print(a.create(5))
print(b.create("x"))
"#, dir = dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["6", "vx"]);
}

#[test]
fn test_overload_cross_module_merged_by_arity() {
    // ADR-074 cross-module merge: same-name imports from two modules, distinguished by arity.
    let dir = std::env::temp_dir().join(format!("lin_overload_xmod_merge_arity_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("m1.lin"),
        "export val add = (a: Int32): Int32 => a + 10\n").unwrap();
    std::fs::write(dir.join("m2.lin"),
        "export val add = (a: Int32, b: Int32): Int32 => a + b\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ add }} from "{dir}/m1"
import {{ add }} from "{dir}/m2"
print(add(3))
print(add(3, 4))
"#, dir = dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(output, vec!["13", "7"]);
}

#[test]
fn test_overload_numeric_signedness_tiebreak() {
    // ADR-075: incomparable UInt64 vs Int64 overloads — an unsigned arg prefers the unsigned
    // overload, a signed/computed one the signed overload, via the numeric-conversion tie-break.
    let out = run(r#"import { print } from "std/io"
val f = (v: UInt64): String => "u64"
val f = (v: Int64): String => "i64"
val a: UInt16 = 5
val b: Int32 = 7
val c: UInt64 = 9
val d: Int64 = 11
print(f(a))
print(f(b))
print(f(c))
print(f(d))
"#);
    assert_eq!(out, vec!["u64", "i64", "u64", "i64"]);
}

#[test]
fn test_overload_numeric_same_sign_picks_narrowest() {
    // Same-signedness numeric overloads are totally ordered by subtyping, so an exact-width
    // argument already resolves to its own width (no tie-break needed).
    let out = run(r#"import { print } from "std/io"
val enc = (v: UInt16): String => "16"
val enc = (v: UInt32): String => "32"
val enc = (v: UInt64): String => "64"
val a: UInt16 = 1
val b: UInt32 = 2
val c: UInt64 = 3
print(enc(a))
print(enc(b))
print(enc(c))
"#);
    assert_eq!(out, vec!["16", "32", "64"]);
}

// ---------------------------------------------------------------------------
// Diagnostic drill-down tests
// ---------------------------------------------------------------------------

/// A union-into-non-null-param mismatch (the `keys()` case): passing `M | Null` where only
/// `M` is expected should produce a drill-down mentioning `Null` or "not assignable".
#[test]
fn test_check_drill_down_union_null_mismatch() {
    // keys() expects { String: AnyVal } | {}; m[k] returns M | Null.
    // The drill-down should explain that Null is the problematic variant.
    let (ok, output) = check_source(
        r#"import { keys } from "std/object"
type M = { String: UInt32 }
val f = (m: { UInt8: M }, k: UInt8): String[] =>
  m[k].keys()
"#,
    );
    assert!(!ok, "expected type error, but check passed:\n{}", output);
    assert!(
        output.contains("\u{21b3}") && (output.contains("Null") || output.contains("not assignable")),
        "expected drill-down with ↳ mentioning Null/not assignable, got:\n{}",
        output
    );
}

/// A simple scalar-vs-scalar mismatch must NOT produce a drill-down (message stays byte-identical).
#[test]
fn test_check_no_drill_down_scalar_mismatch() {
    let (ok, output) = check_source(
        r#"import { trim } from "std/string"
val x = trim(42)
"#,
    );
    assert!(!ok, "expected type error");
    // The ↳ character must NOT appear for a plain scalar mismatch.
    assert!(
        !output.contains("\u{21b3}"),
        "unexpected drill-down for scalar mismatch, got:\n{}",
        output
    );
}

/// A named index-signature (map) alias must render by its alias name in diagnostics, not its
/// expanded `{ K: V }` form — including when reached as a NESTED alias (the value type of an
/// outer map alias, surfaced via indexing). Regression for map-keyed alias display.
#[test]
fn test_check_map_alias_renders_by_name() {
    // `Arrivals` is the value type of `ArrivalsByNumChanges`; m[k] yields `Arrivals | Null`.
    let (ok, output) = check_source(
        r#"import { keys } from "std/object"
type Arrivals = { String: UInt32 }
type ArrivalsByNumChanges = { UInt8: Arrivals }
val f = (m: ArrivalsByNumChanges, k: UInt8): String[] =>
  m[k].keys()
"#,
    );
    assert!(!ok, "expected type error, got:\n{}", output);
    assert!(
        output.contains("Arrivals | Null"),
        "expected the nested map alias to render as `Arrivals | Null`, got:\n{}",
        output
    );
    // The expanded form must NOT leak into the headline type.
    assert!(
        !output.contains("{ String: UInt32 } | Null"),
        "map alias should not appear expanded, got:\n{}",
        output
    );
}

/// A direct map-alias argument also renders by name.
#[test]
fn test_check_direct_map_alias_renders_by_name() {
    let (ok, output) = check_source(
        r#"import { trim } from "std/string"
type Arrivals = { String: UInt32 }
val a: Arrivals = { "x": 3 }
val r = trim(a)
"#,
    );
    assert!(!ok, "expected type error");
    assert!(
        output.contains("has type Arrivals"),
        "expected `has type Arrivals`, got:\n{}",
        output
    );
}

/// The drill-down must also fire on ASSIGNMENT / annotation mismatches (the `check_expr` against
/// an expected type path), not just call arguments and returns. Regression for extending the
/// breakdown to `Expected type X, got Y` sites.
#[test]
fn test_check_drill_down_assignment_union_null() {
    let (ok, output) = check_source(
        r#"type M = { String: UInt32 }
val m: { UInt8: M } = {}
val a: M = m[0]
"#,
    );
    assert!(!ok, "expected type error, got:\n{}", output);
    assert!(
        output.contains("\u{21b3}") && output.contains("Null"),
        "expected an assignment-site drill-down mentioning Null, got:\n{}",
        output
    );
}

// ── Function-as-AnyVal rejection: integration tests (ADR-088) ────────────────────────────────────
//
// These tests verify that the stdlib signatures repaired in Parts A/B of ADR-088 still compile and
// run correctly after the signature tightening, and that the Function-vs-AnyVal guard fires at the
// call site when a bare function is passed where only data is valid.

/// `iter` with proper function params compiles and runs (stdlib/iter.lin signature fix).
#[test]
fn test_iter_with_typed_closures_compiles_and_runs() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { iter, for } from "std/iter"
import { push } from "std/array"

val it = iter(() => 0, i => i < 5, i => i + 1, i => i * 2)
val result: Int32[] = []
it.for(x => push(result, x))
print(toString(result))
"#);
    assert_eq!(output, vec!["[0, 2, 4, 6, 8]"]);
}

/// `iterOf` with a typed array compiles and iterates correctly (stdlib/iter.lin signature fix).
#[test]
fn test_iter_of_typed_array_compiles_and_runs() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { iterOf, for } from "std/iter"
import { push } from "std/array"

val arr = [10, 20, 30]
val it = iterOf(arr)
val result: Int32[] = []
it.for(x => push(result, x))
print(toString(result))
"#);
    assert_eq!(output, vec!["[10, 20, 30]"]);
}

/// `parallel` with thunks compiles and runs (regression: must still accept thunk arrays).
#[test]
fn test_parallel_thunks_regression_after_anyval_fix() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { parallel } from "std/async"

val results = parallel([() => 10, () => 20, () => 30])
print(toString(results))
"#);
    assert_eq!(output, vec!["[10, 20, 30]"]);
}

/// `worker` with typed handler compiles and runs (stdlib/async.lin signature fix).
#[test]
fn test_worker_typed_handler_regression() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { worker, request, close } from "std/async"

val w = worker(msg => msg, () => null)
val reply = request(w, "hello")
close(w)
print(toString(reply))
"#);
    assert_eq!(output, vec!["hello"]);
}

/// Passing a bare function to a data-typed `AnyVal` param is now a compile-time type error.
#[test]
fn test_function_as_anyval_param_is_type_error() {
    let (ok, output) = check_source(r#"
val acceptsData = (x: AnyVal): Null => null
acceptsData(n => n + 1)
"#);
    assert!(
        !ok,
        "Expected a type error for passing a lambda to AnyVal param, got Ok"
    );
    // The error should mention the argument type is a function.
    assert!(
        output.contains("Argument") || output.contains("argument"),
        "Expected argument error in output, got:\n{}",
        output
    );
}
// ADR-085: expected-RESULT-type-driven generic inference, propagated through method chains.
// `Object.fromEntries(stops.map(stop => [stop, v]))` — the TS-faithful, fully-UNANNOTATED form —
// must type-check and run. The lambda's `[stop, 100]` infers as a TUPLE (not the unioned
// `(String|Int32)[]`) because the call's expected result `{ String: UInt32 }` solves
// `fromEntries`'s `T = UInt32`, which flows back through `.map` to give the lambda the expected
// return `[String, UInt32]`.
#[test]
fn test_expected_result_drives_map_fromentries_tuple() {
    let output = run(r#"import { map } from "std/iter"
import { fromEntries } from "std/object"
import { print } from "std/io"
import { toString } from "std/string"
type M = { String: UInt32 }
val build = (stops: String[]): M =>
  stops.map(stop => [stop, 100]).fromEntries()
val m = build(["a", "b"])
print(toString(m["a"]))
print(toString(m["b"]))
"#);
    assert_eq!(output, vec!["100", "100"]);
}

// ADR-085: the same chain nested inside a one-element ARRAY literal whose expected element type is
// the map `{ String: UInt32 }`. The expected element flows into the inner `.map(...).fromEntries()`
// exactly as a direct binding does, so `k[0]["a"]` reads back the value.
#[test]
fn test_expected_result_drives_map_fromentries_array_of_one() {
    let output = run(r#"import { map } from "std/iter"
import { fromEntries } from "std/object"
import { print } from "std/io"
import { toString } from "std/string"
val stops = ["a", "b"]
val k: { String: UInt32 }[] = [ stops.map(stop => [stop, 100]).fromEntries() ]
print(toString(k[0]["a"]))
"#);
    assert_eq!(output, vec!["100"]);
}

// ADR-085: the empty-map-VALUE case — the tuple's second element is an empty `{}` whose type must
// be taken from the expected inner-map type `{ String: String }`. The chain type-checks, a stop key
// is present, and its inner map is empty.
#[test]
fn test_expected_result_drives_map_fromentries_empty_map_value() {
    let output = run(r#"import { map } from "std/iter"
import { fromEntries } from "std/object"
import { print } from "std/io"
type Inner = { String: String }
type Conn = { String: Inner }
val stops = ["a", "b"]
val c: Conn = stops.map(stop => [stop, {}]).fromEntries()
val inner = c["a"]
if inner != null then print("has-a") else print("no-a")
val missing = c["zzz"]
if missing != null then print("has-zzz") else print("no-zzz")
"#);
    assert_eq!(output, vec!["has-a", "no-zzz"]);
}

// ADR-085 SOUNDNESS guard: the expected-result-driven seeding must NOT back-propagate through a
// UNION declared return. `at<T, D>(…): T | D` with the default OMITTED is `T | Null`; binding
// `ints.at(9)` to a bare `Int32` must STILL be rejected (a `Union` return is excluded from
// seeding, so `D` is not unsoundly pinned to `Int32` from the expected `Int32`).
#[test]
fn test_expected_result_union_return_not_seeded_soundness() {
    let err = run_expect_err(r#"import { at } from "std/array"
import { print } from "std/io"
val ints: Int32[] = [1, 2, 3]
val bad: Int32 = ints.at(9)
print("unreachable")
"#);
    assert!(
        err.contains("Null"),
        "expected a `T | Null` soundness error for an omitted-default `at`, got: {}",
        err
    );
}

// ADR-085: the PREFIX-call mirror of the dot-chain — `fromEntries(map(stops, stop => [stop, v]))`.
// The expected result `{ String: UInt32 }` drives `fromEntries`'s `T`, whose substituted parameter
// type is then pushed into the nested `map(...)` CALL argument, solving `map`'s `U` so the lambda's
// tuple is formed. No annotations anywhere.
#[test]
fn test_expected_result_drives_prefix_map_fromentries_tuple() {
    let output = run(r#"import { map } from "std/iter"
import { fromEntries } from "std/object"
import { print } from "std/io"
import { toString } from "std/string"
type M = { String: UInt32 }
val stops = ["a", "b"]
val m: M = fromEntries(map(stops, stop => [stop, 100]))
print(toString(m["a"]))
"#);
    assert_eq!(output, vec!["100"]);
}

// ADR-085 + ADR-058: `reduce(xs, {}, f): Q` where Q is a typed map — the empty `{}` init must
// be accepted without an explicit annotation when the function's declared return type pins U.
// Before this fix, the expected-result seed bound U = Q but the {} routing still fell through to
// plain inference (Object{}), and the compat check fired "Expected type Q, got {}" at .reduce.
#[test]
fn test_reduce_empty_map_init_from_expected_return() {
    let output = run(r#"import { reduce } from "std/iter"
import { print } from "std/io"
import { toString } from "std/string"
type Q = { String: Int32 }
val build = (xs: String[]): Q =>
  xs.reduce({}, (acc, x) => acc)
val m = build(["a", "b"])
if m["a"] == null then print("null") else print("non-null")
"#);
    assert_eq!(output, vec!["null"]);
}

// ADR-085 + ADR-058: a genuinely uninferrable empty literal in a call argument must point the
// caret AT the `{}` / `[]` and emit the annotation-required message, not a misleading
// "Expected type X, got {}" at the enclosing call span.
#[test]
fn test_empty_literal_arg_uninferrable_points_at_literal() {
    let err = run_expect_err(r#"import { print } from "std/io"
val add = (x: Int32, y: Int32): Int32 => x + y
val result = add({}, 1)
print("unreachable")
"#);
    assert!(
        err.contains("cannot infer the value type of an empty map/object literal"),
        "expected the ADR-058 annotation-required diagnostic at the `{{}}` site, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Destructuring lambda parameters (ADR-079) — bare + parenthesized, array + object.
// ---------------------------------------------------------------------------

/// Bare array-destructuring lambda in argument position: `[a, b] => …` binds `a`,`b`.
#[test]
fn test_destructure_lambda_bare_array() {
    let output = run(r#"import { print } from "std/io"
import { for } from "std/iter"

val pairs: Int32[][] = [[1, 2], [3, 4]]
pairs.for([a, b] => print(a + b))
"#);
    assert_eq!(output, vec!["3", "7"]);
}

/// Parenthesized array-destructuring lambda: `([a, b]) => …` binds and runs.
#[test]
fn test_destructure_lambda_paren_array() {
    let output = run(r#"import { print } from "std/io"
import { for } from "std/iter"

val pairs: Int32[][] = [[1, 2], [3, 4]]
pairs.for(([a, b]) => print(a * b))
"#);
    assert_eq!(output, vec!["2", "12"]);
}

/// Object-destructuring lambda, both bare and parenthesized.
#[test]
fn test_destructure_lambda_object_both_forms() {
    let output = run(r#"import { print } from "std/io"
import { for } from "std/iter"

type P = { "name": String, "age": Int32 }
val items: P[] = [{ "name": "ann", "age": 30 }, { "name": "bob", "age": 40 }]
items.for(({ name, age }) => print(name))
items.for({ name, age } => print(age))
"#);
    assert_eq!(output, vec!["ann", "bob", "30", "40"]);
}

/// Multi-param lambdas mixing an ordinary param and a destructuring param, both orders.
#[test]
fn test_destructure_lambda_multi_param_mixed() {
    let output = run(r#"import { print } from "std/io"

val f = (x: Int32, [a, b]: Int32[]) => print(x + a + b)
f(1, [2, 3])
val g = ([a, b]: Int32[], y: Int32) => print(a + b + y)
g([10, 20], 5)
"#);
    assert_eq!(output, vec!["6", "35"]);
}

/// Nested array-destructuring param `([a, [b, c]]) => …`.
#[test]
fn test_destructure_lambda_nested_array() {
    let output = run(r#"import { print } from "std/io"

val f = ([a, [b, c]]: AnyVal) => print(a + b + c)
f([1, [2, 3]])
"#);
    assert_eq!(output, vec!["6"]);
}

/// Rest element in an array-destructuring param `([a, ...rest]) => …`.
#[test]
fn test_destructure_lambda_array_rest() {
    let output = run(r#"import { print } from "std/io"
import { length } from "std/array"

val f = ([a, ...rest]: Int32[]) => print(a + rest.length())
f([10, 1, 2, 3])
"#);
    // a = 10, rest = [1,2,3] → 10 + 3 = 13
    assert_eq!(output, vec!["13"]);
}

/// Map-entries-style motivating case: a `[K, V][]` iterated by a destructuring lambda,
/// both bare and parenthesized — binds `routeId`,`stopP`.
#[test]
fn test_destructure_lambda_entries_motivating() {
    let output = run(r#"import { print } from "std/io"
import { for } from "std/iter"

val entries: Int32[][] = [[10, 100], [20, 200]]
entries.for([routeId, stopP] => print(routeId + stopP))
entries.for(([routeId, stopP]) => print(routeId * stopP))
"#);
    assert_eq!(output, vec!["110", "220", "1000", "4000"]);
}

/// Negative: an array LITERAL argument (no trailing `=>`) must still parse as a literal, not be
/// mistaken for a bare destructuring lambda. Likewise a record literal and a `[..].method()` call.
#[test]
fn test_destructure_lambda_literal_not_a_lambda() {
    let (ok, output) = check_source(r#"import { print } from "std/io"
import { push, length } from "std/array"

var xs: Int32[][] = [[0]]
xs.push([1, 2])
val obj: { "a": Int32 } = { "a": 1 }
print([1, 2].length())
print(obj["a"])
"#);
    assert!(ok, "expected literals to parse cleanly, got:\n{}", output);
}

// ── ADR-078: inner-scope shadowing is a hard error ──────────────────────────

/// A nested `val` that reuses an outer `val` name is a compile-time error.
#[test]
fn test_shadowing_nested_val_is_rejected() {
    // `y` is bound in the outer scope; the inner lambda tries to rebind it.
    let (ok, output) = check_source(
        r#"import { print } from "std/io"
val y = 1
val f = (): Int32 =>
  val y = 2
  y
print("${f()}")
"#,
    );
    assert!(
        !ok,
        "expected shadowing to be rejected, but it compiled:\n{}",
        output
    );
    assert!(
        output.contains("shadows a binding from an enclosing scope"),
        "expected the shadowing diagnostic, got:\n{}",
        output
    );
}

/// A lambda parameter that reuses an outer binding name is a compile-time error.
#[test]
fn test_shadowing_lambda_param_is_rejected() {
    // `x` is bound at module level; the lambda parameter `x` shadows it.
    let (ok, output) = check_source(
        r#"import { print } from "std/io"
val x = 1
val result = [1, 2, 3].map(x => x + 1)
"#,
    );
    assert!(
        !ok,
        "expected shadowing of outer `x` by lambda param to be rejected:\n{}",
        output
    );
    assert!(
        output.contains("shadows a binding from an enclosing scope"),
        "expected the shadowing diagnostic, got:\n{}",
        output
    );
}

/// Sibling lambdas may reuse the same parameter name — only inner-shadows-outer is forbidden.
#[test]
fn test_shadowing_sibling_lambdas_same_param_accepted() {
    // Two sibling `.map` / `.filter` chains both use `x` as their param: no shadowing.
    let (ok, output) = check_source(
        r#"import { map, filter } from "std/iter"
val result = [1, 2, 3].map(x => x * 2).filter(x => x > 2)
"#,
    );
    assert!(
        ok,
        "expected sibling-lambda param reuse to be accepted, but it failed:\n{}",
        output
    );
}

/// A `val` in the same scope that binds a name already bound at that scope level is still
/// accepted (same-scope redefinition is not shadowing — it is sequencing in the same block).
#[test]
fn test_shadowing_same_scope_reuse_accepted() {
    // Both `val n` bindings are in the *same* block scope; the second is a sequenced rebind,
    // not an inner-scope shadow.
    let (ok, output) = check_source(
        r#"import { print } from "std/io"
val f = (): Int32 =>
  val n = 1
  val n = 2
  n
print("${f()}")
"#,
    );
    // Same-scope rebind is currently accepted (not a shadowing error). If the language later
    // disallows it this test should be updated to assert !ok.
    assert!(
        ok,
        "expected same-scope rebind to be accepted (not a shadowing error), got:\n{}",
        output
    );
}

// ── Regression guards for the three bugs std/csv used to work around (now FIXED) ──────────────────
// These three patterns were the documented justification for std/csv keeping `AnyVal` types,
// hand-rolled recursion, and single-shape returns. The underlying compiler bugs have since been
// fixed; these tests lock the fixed behaviour so the csv workaround removals can't silently regress.

/// Bug A: indexing a typed nested array (`String[][]`) and passing the element into a function's
/// typed `String[]` parameter used to crash in native codegen. Must now run correctly.
#[test]
fn test_typed_reindex_into_typed_param_regression() {
    let output = run(r#"import { print } from "std/io"
import { push, length } from "std/array"
import { toString } from "std/string"
val take = (row: String[]): String => row[0]
val consume = (out: String[], parsed: String[][], i: Int32, n: Int32): Null =>
  if i >= n then null
  else
    push(out, take(parsed[i]))
    consume(out, parsed, i + 1, n)
val parsed: String[][] = [["a", "b"], ["c", "d"], ["e", "f"]]
val out: String[] = []
consume(out, parsed, 0, 3)
print(toString(out))
"#);
    assert_eq!(output, vec![r#"["a", "c", "e"]"#]);
}

/// Bug B: a `.for` loop capturing an outer array while calling a function that runs its OWN `.for`
/// used to lose pushes to the captured array. All pushes must now be retained.
#[test]
fn test_captured_array_nested_for_regression() {
    let output = run(r#"import { print } from "std/io"
import { for, range } from "std/iter"
import { push, length } from "std/array"
import { toString } from "std/string"
val fill = (dst: String[]): Null =>
  range(0, 2).for(j => push(dst, "f"))
val rows: String[] = []
range(0, 4).for(i =>
  val tmp: String[] = []
  fill(tmp)
  push(rows, "row-${length(tmp)}")
)
print(toString(length(rows)))
print(rows[3])
"#);
    assert_eq!(output, vec!["4", "row-2"]);
}

/// Bug C: a tail-recursive function returning an owned array PARAM on one branch and an OBJECT on
/// another used to corrupt its output. Both branches must now return correct values.
#[test]
fn test_tco_mixed_array_object_return_regression() {
    let output = run(r#"import { print } from "std/io"
import { push, length } from "std/array"
import { toString } from "std/string"
val f = (acc: String[], n: Int32, bare: Boolean): AnyVal =>
  if bare then acc
  else if n <= 0 then { "rows": acc, "ok": true }
  else
    push(acc, "r${n}")
    f(acc, n - 1, bare)
val a: String[] = []
val obj = f(a, 3, false)
val rows: String[] = obj["rows"]
val b: String[] = ["x", "y"]
val barr: String[] = f(b, 0, true)
print(toString(length(rows)))
print(rows[0])
print(toString(length(barr)))
"#);
    assert_eq!(output, vec!["3", "r3", "2"]);
}

#[test]
fn test_push_object_literal_into_union_array() {
    // Regression: pushing an object literal with computed field values into a union-typed array
    // degraded the literal's field types to AnyVal instead of checking it against the union's
    // element type. `push<T>(arr: T[], item: T)` must solve T from the array arg first, then
    // check the literal against the substituted param type (the union variant), not infer it
    // bottom-up and degrade it.
    let output = run(r#"import { print } from "std/io"
import { push } from "std/array"
type Leg = { "origin": String, "destination": String }
type TimetableLeg = Leg & { "tripId": String }
type Transfer = Leg & { "duration": Int32 }
type AnyLeg = Transfer | TimetableLeg
val makeLeg = (tripId: String, origin: String, dest: String): AnyLeg[] =>
  val legs: AnyLeg[] = []
  legs.push({ "tripId": tripId, "origin": origin, "destination": dest })
  legs
val result = makeLeg("IC123", "Amsterdam", "Utrecht")
val leg = result[0]
if leg is TimetableLeg then
  print(leg["tripId"])
  print(leg["origin"])
else
  print("not a timetable leg")
"#);
    assert_eq!(output, vec!["IC123", "Amsterdam"]);
}

// ── TypeScript-style utility types (Partial/Required/Pick/Omit/NonNullable/Exclude/
//    Extract/ReturnType/Parameters/Record) + keyof + indexed access ──────────────────

#[test]
fn test_utility_partial_and_required() {
    let output = run(r#"import { print } from "std/io"

type User = { "id": Int32, "name": String, "email": String | Null }

type Patch = Partial<User>
type FullUser = Required<User>

val p: Patch = { "id": 1, "name": "ann", "email": null }
val f: FullUser = { "id": 2, "name": "bob", "email": "b@x" }

print(p["name"])
print(f["email"])
"#);
    assert_eq!(output, vec!["ann", "b@x"]);
}

#[test]
fn test_utility_pick_omit_keyof_indexed() {
    let output = run(r#"import { print } from "std/io"

type User = { "id": Int32, "name": String, "email": String | Null }

type Names = Pick<User, "id" | "name">
type NoEmail = Omit<User, "email">
type Keys = keyof User
type NameType = User["name"]

val n: Names = { "id": 7, "name": "cay" }
val ne: NoEmail = { "id": 8, "name": "dee" }
val k: Keys = "email"
val nm: NameType = "elle"

print(n["name"])
print(ne["name"])
print(k)
print(nm)
"#);
    assert_eq!(output, vec!["cay", "dee", "email", "elle"]);
}

#[test]
fn test_utility_exclude_extract_nonnullable() {
    let output = run(r#"import { print } from "std/io"

type Status = "active" | "inactive" | "pending"
type NotPending = Exclude<Status, "pending">
type OnlyActive = Extract<Status, "active">
type DefStr = NonNullable<String | Null>

val a: NotPending = "active"
val b: OnlyActive = "active"
val c: DefStr = "hi"

print(a)
print(b)
print(c)
"#);
    assert_eq!(output, vec!["active", "active", "hi"]);
}

#[test]
fn test_utility_record_and_function_ops() {
    let output = run(r#"import { print } from "std/io"

type Flags = Record<"a" | "b", Boolean>
type F = (Int32, String) => Boolean
type Ret = ReturnType<F>
type Params = Parameters<F>

val flags: Flags = { "a": true, "b": false }
val r: Ret = true
val ps: Params = [3, "x"]

print(if flags["a"] then "yes" else "no")
print(if r then "t" else "f")
print(ps[1])
"#);
    assert_eq!(output, vec!["yes", "t", "x"]);
}

#[test]
fn test_utility_pick_bad_key_errors() {
    // Pick with a key that is not a field of T must be a compile error naming the bad key.
    let err = run_expect_err(r#"import { print } from "std/io"

type User = { "id": Int32, "name": String }
type Bad = Pick<User, "naem">

print("unreachable")
"#);
    assert!(err.contains("naem"), "expected error to name bad key, got: {}", err);
}

#[test]
fn test_utility_record_user_shadow_still_checks() {
    // A user-defined non-generic `type Record = { … }` must still type-check (the builtin only
    // applies in `Record<…>` applied position with no user decl). Mirrors examples/report.
    let output = run(r#"import { print } from "std/io"

type Record = { "label": String, "count": Int32 }

val rec: Record = { "label": "hello", "count": 3 }
print(rec["label"])
"#);
    assert_eq!(output, vec!["hello"]);
}

/// Regression: flow-narrowing a `var` binding (e.g. `var x: Int32|Null`) inside an `if x != null`
/// guard used to flip the binding to immutable, so `x = 5` inside the narrowed branch errored with
/// "Cannot assign to immutable binding". The narrowed shadow now preserves the `var` mutability.
#[test]
fn test_narrowed_var_is_still_assignable() {
    // Basic repro: assign through a narrowed var.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val main = () =>
  var x: Int32 | Null = null
  if x != null then
    x = 5
  print(toString(x))
main()
"#);
    assert_eq!(output, vec!["null"]);

    // After assignment inside a narrowing scope, the stale refinement is invalidated:
    // a subsequent `val y: Int32 = x` must fail because `x` is back to `Int32 | Null`.
    let err = run_expect_err(r#"import { print } from "std/io"
val main = () =>
  var x: Int32 | Null = 42
  if x != null then
    x = null
    val y: Int32 = x
  x
main()
"#);
    assert!(err.contains("Int32 | Null") || err.contains("Null"), "expected type error after assignment invalidates narrowing, got: {}", err);
}

/// Regression: assigning through a narrowed var in a loop must converge correctly at runtime.
#[test]
fn test_narrowed_var_assign_in_loop() {
    let output = run(r#"import { print } from "std/io"
import { range, for } from "std/iter"
import { toString } from "std/string"

val main = () =>
  var best: Int32 | Null = null
  range(0, 5).for(i =>
    if best == null || i < best then
      best = i
  )
  print(toString(best))
main()
"#);
    assert_eq!(output, vec!["0"]);
}

// ── Compound boolean-condition narrowing (`||`, `&&`, `!`) ──────────────────────────────────────
// Before this change, flow-narrowing only applied to ATOMIC conditions (`x != null`,
// `x == null`, `x is T`). Compound connectives (`||`, `&&`, `!`) now decompose into branch facts
// using Boolean algebra:
//   `A || B` else-branch → facts of (A-false) AND (B-false) (De Morgan: ¬A ∧ ¬B)
//   `A && B` then-branch → facts of (A-true)  AND (B-true)
//   `!A`                 → swap A's then/else facts

#[test]
fn test_compound_or_else_branch_narrows_non_null() {
    // `if x == null || x > 5 then 0 else x` — else-branch: both `x == null` (false) and
    // `x > 5` (false) → `x` is non-null in the else branch.
    // Before this fix this failed: "Function body has type Int32 | Null".
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val f = (x: Int32 | Null): Int32 =>
  if x == null || x > 5 then 0 else x
print(toString(f(null)))
print(toString(f(3)))
print(toString(f(10)))
"#);
    assert_eq!(out, vec!["0", "3", "0"]);
}

#[test]
fn test_compound_and_then_branch_narrows_non_null() {
    // `if x != null && x > 5 then x else 0` — then-branch: both `x != null` (true) and
    // `x > 5` (true) → `x` is non-null in the then branch.
    // Before this fix this failed: "Function body has type Int32 | Null".
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val g = (x: Int32 | Null): Int32 =>
  if x != null && x > 5 then x else 0
print(toString(g(null)))
print(toString(g(3)))
print(toString(g(10)))
"#);
    assert_eq!(out, vec!["0", "0", "10"]);
}

#[test]
fn test_compound_not_negation_narrows_is_type() {
    // `!(v is Int32)` — else-branch (condition false) means `v is Int32` was true → v: Int32.
    // Also: `v is Int32 && v > 0` — then-branch knows v: Int32.
    // Before this fix both failed to type-check.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type U = Int32 | String
val h1 = (v: U): Int32 =>
  if !(v is Int32) then 0 else v
val h2 = (v: U): Int32 =>
  if v is Int32 && v > 0 then v else 0
print(toString(h1(42)))
print(toString(h1("hello")))
print(toString(h2(10)))
print(toString(h2(0 - 5)))
"#);
    assert_eq!(out, vec!["42", "0", "10", "0"]);
}

#[test]
fn test_or_right_operand_narrows_into_call_arg() {
    // `x == null || isBefore(x, stop)` — the right side of `||` is reached only when `x != null`,
    // so `x` must be narrowed to String inside the call argument. Before this fix the checker
    // rejected the call with "Argument 1 has type String | Null, expected String".
    let out = run(r#"import { print } from "std/io"
val isBefore = (a: String, b: String): Boolean => a < b
val pick = (cur: String | Null, stop: String): String =>
  if cur == null || isBefore(cur, stop) then stop else cur
print(pick(null, "x"))
print(pick("a", "b"))
print(pick("z", "b"))
"#);
    assert_eq!(out, vec!["x", "b", "z"]);
}

#[test]
fn test_or_right_operand_narrows_into_negated_call_arg() {
    // `x == null || !isBefore(x, stop)` — the `!` wraps the call; `x` must still be narrowed
    // to String when the call argument is checked. Before this fix the checker rejected this too.
    let out = run(r#"import { print } from "std/io"
val isBefore = (a: String, b: String): Boolean => a < b
val pick = (cur: String | Null, stop: String): String =>
  if cur == null || !isBefore(cur, stop) then stop else cur
print(pick(null, "x"))
print(pick("z", "b"))
print(pick("a", "b"))
"#);
    // pick(null,"x")  -> null → take stop → "x"
    // pick("z","b")   -> !isBefore("z","b") = !("z"<"b") = true → take stop → "b"
    // pick("a","b")   -> !isBefore("a","b") = !("a"<"b") = false → take cur → "a"
    assert_eq!(out, vec!["x", "b", "a"]);
}

/// Array spread: concat two arrays with `[...a, ...b]`.
#[test]
fn test_array_spread_concat() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val a = [1, 2]
val b = [3, 4]
val c = [...a, ...b]
print(toString(c))
"#);
    assert_eq!(output, vec!["[1, 2, 3, 4]"]);
}

/// Array spread: prepend a scalar and spread in the middle, append after.
#[test]
fn test_array_spread_prepend_append() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val a = [1, 2]
val d = [0, ...a, 5]
print(toString(d))
"#);
    assert_eq!(output, vec!["[0, 1, 2, 5]"]);
}

/// Array spread: copy via `[...a]` is independent from original (length differs after adding element).
#[test]
fn test_array_spread_copy_independence() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"
val a = [1, 2]
val e = [...a, 99]
print(toString(a))
print(toString(e))
print(toString(a.length()))
print(toString(e.length()))
"#);
    assert_eq!(output, vec!["[1, 2]", "[1, 2, 99]", "2", "3"]);
}

/// Array spread: spreading a non-array is a type error.
#[test]
fn test_array_spread_non_array_type_error() {
    let (ok, msg) = check_source(r#"import { print } from "std/io"
val x = 5
val y = [...x]
"#);
    assert!(!ok, "expected type error but check passed");
    assert!(msg.contains("spread element must be an array"), "expected spread error, got: {msg}");
}

/// Regression: 3-arg `range(start, end, step)` used `lin_iter` which typed elements as `AnyVal`.
/// An `AnyVal`-boxed int does not match a numeric map key (stored unboxed), so `m[i]` returned null.
/// Fix: 3-arg range materialises a flat `Int32[]` so the loop variable has type `Int32`.
#[test]
fn test_range_3arg_loop_var_indexes_numeric_map() {
    let output = run(r#"import { print } from "std/io"
import { range, for, map } from "std/iter"
import { toString } from "std/string"
type M = { UInt8: String }
val m: M = { 1: "one", 2: "two", 3: "three" }
range(1, 4, 1).for(i => print(m[i] ?? "NULL"))
range(3, 0, -1).for(i => print(m[i] ?? "NULL"))
print(toString(range(0, 10, 2).map(i => i)))
print(toString(range(5, 0, -1).map(i => i)))
print(toString(range(3, 3, -1).map(i => i)))
"#);
    assert_eq!(output, vec![
        "one", "two", "three",
        "three", "two", "one",
        "[0, 2, 4, 6, 8]",
        "[5, 4, 3, 2, 1]",
        "[]",
    ]);
}

#[test]
fn test_computed_key_single_entry_map() {
    // { [k]: v } infers { String: Int32 } and the entry is readable at runtime.
    let output = run(r#"import { print } from "std/io"
type M = { String: Int32 }
val f = (k: String, v: Int32): M => { [k]: v }
val m = f("hello", 42)
print("${m["hello"] ?? -1}")
print("${m["missing"] ?? -1}")
"#);
    assert_eq!(output, vec!["42", "-1"]);
}

#[test]
fn test_computed_key_spread_plus_computed() {
    // { ...acc, [k]: v } infers { String: Int32 } and merges spread + computed entry.
    let output = run(r#"import { print } from "std/io"
type M = { String: Int32 }
val g = (acc: M, k: String, v: Int32): M => { ...acc, [k]: v }
val m1: M = { "a": 1 }
val m2 = g(m1, "b", 2)
print("${m2["a"] ?? -1}")
print("${m2["b"] ?? -1}")
print("${m2["missing"] ?? -1}")
"#);
    assert_eq!(output, vec!["1", "2", "-1"]);
}

#[test]
fn test_forward_declared_void_fn_call_result_type() {
    // Regression: a top-level function with no return annotation whose body is a side-effecting
    // `.for()`/`.entries()` call (returns Null / void in LLVM) was forward-declared with a fresh
    // TypeVar as its return type. A call to it from a sibling function that appears TEXTUALLY
    // EARLIER in the file recorded that TypeVar as the IR Call's ret_ty. The checker never solved
    // TypeVar -> Null in solved_type_vars, so the zonking pass left it unresolved. Codegen then
    // saw a Direct call whose LLVM function returns void but ret_ty != Null/Never and panicked
    // on unwrap_basic(). Fixed by solving the TypeVar when bind_pattern updates the forward-
    // declared slot with the body's concrete return type.
    //
    // The minimal trigger is: caller (scan) before callee (doSideEffect) in source order, no
    // return annotation on the callee, callee body = side-effecting `.for()` call.
    let output = run(r#"import { print } from "std/io"
import { for } from "std/iter"

val scan = (items: Int32[]) =>
  doSideEffect(items)
  print("scan done")

val doSideEffect = (items: Int32[]) =>
  items.for(x =>
    print("item")
  )

scan([1, 2])
"#);
    assert_eq!(output, vec!["item", "item", "scan done"]);
}

#[test]
fn test_computed_key_reduce_builds_map() {
    // xs.reduce({}, (acc, x) => { ...acc, [x]: 1 }) builds a presence map.
    let output = run(r#"import { print } from "std/io"
import { reduce } from "std/iter"
type M = { String: Int32 }
val xs = ["alpha", "beta", "gamma"]
val m: M = xs.reduce({}, (acc: M, x: String): M => { ...acc, [x]: 1 })
print("${m["alpha"] ?? -1}")
print("${m["beta"] ?? -1}")
print("${m["gamma"] ?? -1}")
print("${m["missing"] ?? -1}")
"#);
    assert_eq!(output, vec!["1", "1", "1", "-1"]);
}

#[test]
fn test_inner_fn_capture_sibling_out_of_order() {
    // Regression: a block-local function (processZip) that captures sibling functions (addLink,
    // addCalendar) defined textually LATER in the same block caused a misaligned-pointer crash at
    // lin_rc_retain. The type checker forward-declares all inner fns so they can reference each
    // other, but IR lowering was sequential: when processZip was lowered, addLink's closure value
    // was not yet in builder.slots. The capture loop fell back to alloc_temp (uninitialised), which
    // codegen's filter_map silently dropped — the env was allocated for 1 capture but the capdesc
    // said 3, so the function body read heap garbage through offsets 16 and 24, hitting
    // lin_rc_retain(0x41) → SIGBUS. Fix: lower_block_stmts topo-sorts inner fn stmts so that
    // dependencies are lowered before their capturers.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { push } from "std/array"

val outer = () =>
  val result: Int32[] = []

  val dispatch = (tag: String) =>
    if tag == "a" then addA()
    else if tag == "b" then addB()
    else addC()

  val addA = () =>
    result.push(1)

  val addB = () =>
    result.push(2)

  val addC = () =>
    result.push(3)

  dispatch("a")
  dispatch("b")
  dispatch("c")
  dispatch("a")
  "${toString(result[0])},${toString(result[1])},${toString(result[2])},${toString(result[3])}"

print(outer())
"#);
    assert_eq!(output, vec!["1,2,3,1"]);
}

// Regression: `sealed_array_materialize_elem` had a two-way dispatch (0xFE vs else) that
// routed 0xFF dynamic arrays down the 0xFD pointer-backed path. A 0xFF slot is 16-byte
// LinArrayElem{tag, payload}; reading it as an 8-byte pointer produced address 0x14
// (TAG_MAP=20) → lin_rc_retain(0x14) → SIGSEGV. Fix: three-way dispatch adds a 0xFF
// branch that calls lin_array_get_tagged + sealed_project_from + lin_tagged_free_box.
// Also: lin_sealed_any_to_tagged had the same two-way dispatch; calling it on a 0xFF
// input (e.g. after filter round-trips a sealed-record array through a tagged store)
// crashed identically. Fix: add a 0xFF passthrough arm that retains and returns as-is.
// Kept UN-batched: RC/repr regression tests.
#[test]
fn test_sealed_array_0xff_materialize_elem_roundtrip() {
    // Build a sealed-record array, box it (which triggers sealed_any_to_tagged → 0xFF),
    // then iterate it with for — exercises sealed_array_materialize_elem 0xFF branch.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { for, filter } from "std/iter"
import { push, length } from "std/array"
type Pt = { "x": Int32, "y": Int32 }
val mkPt = (i: Int32): Pt => { "x": i, "y": i * 10 }
val run = (): Null =>
  val pts: Pt[] = [mkPt(1), mkPt(2), mkPt(3)]
  var sum = 0
  pts.for(p => sum = sum + p["x"])
  print(toString(sum))
  val big: Pt[] = pts.filter(p => p["y"] >= 20)
  print(toString(length(big)))
  print(toString(big[0]["y"]))
run()
"#);
    assert_eq!(out, vec!["6", "2", "20"]);
}

// Regression: packing a nested SEALED-RECORD field from a BOXED element faulted with
// "not supported" when a `map`-built array of records (each holding a nested sealed
// record) was stored into a `{ String: Outer[] }` map and fetched back through the
// generic seam (which strips the `sealed` bit, routing writes through the TAGGED sink).
// Root fix: `pack_named_payload_impl` now handles `NKIND_SEALED` by allocating a fresh
// nested sealed struct from TAG_MAP (materialized round-trip) or retaining a TAG_RECORD.
// Kept UN-batched: heap-layout regression test.
#[test]
fn test_nested_sealed_record_field_packed_from_boxed_element() {
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map, range } from "std/iter"
import { push, length } from "std/array"
import { get } from "std/object"
type Inner = { "x": Int32, "y": Int32 }
type Outer = { "id": Int32, "inner": Inner }
val mkOuter = (i: Int32): Outer =>
  { "id": i, "inner": { "x": i * 10, "y": i * 100 } }
val run = (): Null =>
  var byKey: { String: Outer[] } = {}
  byKey["k"] = []
  val items: Outer[] = range(0, 3).map(mkOuter)
  val stored = get(byKey, "k", [])
  push(stored, items[0])
  push(stored, items[1])
  push(stored, items[2])
  val got = get(byKey, "k", [])
  print("len=${toString(length(got))}")
  print(toString(got[0]["inner"]["x"]))
  print(toString(got[1]["inner"]["y"]))
  print(toString(got[2]["id"]))
run()
"#);
    assert_eq!(out, vec!["len=3", "0", "100", "2"]);
}

#[test]
fn test_named_param_with_fn_field_caller_before_callee() {
    // REGRESSION (bedrock-qbox2): a named record type containing a Function-typed field (e.g.
    // `type Config = { "fn": (Int32) => Int32, "value": Int32 }`) is NOT a packed sealed struct
    // (Function fields are not sealed-eligible). When the CALLER function appears textually BEFORE
    // the CALLEE in source order, the callee is forward-declared with `Named("Config")` as its
    // param type. The IR lowerer's `lower_coerce_arg` checked for `Object{sealed:false}` and
    // returned early — but the expanded `Config` type is `Object{sealed:true}` (set by
    // `expand_named_body`'s seal point) even though it is NOT a packed struct. This caused the
    // `is_union_ty(Named) = true` fallthrough to box the raw `LinMap*` arg into a 16-byte
    // `TaggedVal*` shell via `lin_box_map`. The callee then called `lin_map_get(TaggedVal*, key)`
    // reading 8 bytes past the 16-byte region — a heap-buffer-overflow.
    // Additionally, `arg_box_is_caller_owned_shell` returned true for the same (Object, Named)
    // pair, so after the (now-unwrapped) call `lin_tagged_free_box` was called on the raw
    // `LinMap*` pointer → use-after-free / segfault.
    // Fix: in `lower_coerce_arg`, extend the pass-through guard to cover `Object{sealed:true}`
    // that is NOT a packed struct; in `arg_box_is_caller_owned_shell`, exclude Object→Named pairs
    // (no box shell was created, so none should be freed).
    let out = run(r#"
import { print } from "std/io"
import { toString } from "std/string"

type Processor = {
  "fn": (Int32) => Int32,
  "value": Int32
}

val run = (p: Processor, x: Int32): Int32 =>
  val r = compute(p, x)
  r

val compute = (p: Processor, x: Int32): Int32 =>
  val fn = p["fn"]
  val v = p["value"]
  fn(x) + v

val p: Processor = {
  "fn": (x: Int32) => x * 3,
  "value": 10
}
val result = run(p, 7)
print(toString(result))
"#);
    assert_eq!(out, vec!["31"]);
}
