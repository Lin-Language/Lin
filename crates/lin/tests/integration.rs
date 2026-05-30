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

/// Compile `source` to a temp binary and return stdout lines.
/// Panics if compilation or execution fails.
fn run(source: &str) -> Vec<String> {
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let src_path = ws.join(format!("target/lin_test_{}.lin", id));
    let bin_path = ws.join(format!("target/lin_test_{}", id));

    fs::write(&src_path, source).unwrap();

    let compile = Command::new(lin_bin())
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

    let compile = Command::new(lin_bin())
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

    let compile = Command::new(lin_bin())
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

// ─────────────────────────────────────────────────────────────────────────────
// Core language tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn test_hello_world() {
    let output = run(r#"import { print } from "std/io"
print("hello world")"#);
    assert_eq!(output, vec!["hello world"]);
}

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

val describe = (input: Json): String =>
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

val describe = (input: Json): String =>
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

val divide = (a: Float64, b: Float64): Json =>
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

// Regression: an Array (or any heap value) passed as an argument to an INDIRECT call
// through a closure value must be boxed to Json to match the closure's `Json` parameter,
// exactly as the named/imported call paths do. Previously the indirect-call lowering passed
// the raw `LinArray*` instead of a boxed `TaggedVal*`, so the callee read its tag/payload
// from garbage and mutations through it were silently lost (the array stayed empty).
#[test]
fn test_array_passed_to_closure_value_mutates() {
    let output = run(r#"import { print } from "std/io"
import { push, length } from "std/array"
import { toString } from "std/string"

val acc = []
val f = (a: Json) => push(a, 1)
f(acc)
f(acc)
print(toString(length(acc)))
"#);
    assert_eq!(output, vec!["2"]);
}

// Regression: a fresh-alloc heap literal (array/object) passed to a Json/union parameter,
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

val id = (acc: Json): Json => acc
val wrap = (): Json => id([1, 2])
print(toString(wrap()))
"#);
    assert_eq!(passthrough, vec!["[1, 2]"]);

    // Accumulator-threading: `build(0, n, [])` returns the threaded `acc`.
    let accumulator = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { push } from "std/array"

val build = (i: Int32, n: Int32, acc: Json): Json =>
  if i >= n then acc
  else
    push(acc, i * i)
    build(i + 1, n, acc)
val squares = (n: Int32): Json => build(0, n, [])
print(toString(squares(4)))
"#);
    assert_eq!(accumulator, vec!["[0, 1, 4, 9]"]);

    // Result BOUND to a `val` and then returned (block-scope escape, not just direct return) —
    // the literal is owned in the block scope, so the block's own scope-release must also
    // transfer ownership into the escaping result, not just the function-return release.
    let bound = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val id = (acc: Json): Json => acc
val wrap = (): Json =>
  val x = id([1, 2])
  x
print(toString(wrap()))
"#);
    assert_eq!(bound, vec!["[1, 2]"]);

    // INDIRECT (closure-value) call: the literal escapes through a call whose callee is a
    // closure value (`f`), not a statically-known function.
    let indirect = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val makeId = () => (acc: Json): Json => acc
val wrap = (): Json =>
  val f = makeId()
  f([1, 2])
print(toString(wrap()))
"#);
    assert_eq!(indirect, vec!["[1, 2]"]);

    // Fresh object literal carrying a nested array, passed through and returned — the nested
    // payload must survive too (a shallow box-aliasing guard would free the inner array early).
    let nested = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val id = (acc: Json): Json => acc
val wrap = (): Json => id({ "items": [1, 2, 3] })
print(toString(wrap()))
"#);
    assert_eq!(nested, vec![r#"{"items": [1, 2, 3]}"#]);

    // TRANSIENT result (consumed, not escaped) must still be released normally — guards against
    // the keep-expansion over-suppressing the literal release and leaking.
    let transient = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"

val id = (acc: Json): Json => acc
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

#[test]
fn test_for_and_range() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range } from "std/array"
import { for } from "std/array"

range(1, 4).for(i => print(toString(i)))
"#);
    assert_eq!(output, vec!["1", "2", "3"]);
}

// Regression: a top-level mutable `var` accumulated from inside a `.for` loop body closure.
// The closure can't see main's SSA temps, so the var must be a module global written via
// GlobalValSet and read via GlobalValGet; and `acc + i` must unbox the boxed (TypeVar) loop
// element before the integer add. Previously this crashed in codegen (int op on a null ptr).
#[test]
fn test_loop_accumulates_toplevel_var() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range, for } from "std/array"

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
import { range, for } from "std/array"

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
import { range, for, map } from "std/array"

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
import { map, filter, reduce, find, some, every } from "std/array"

print(toString([1, 2, 3].map(x => x * 2)))
print(toString([1, 2, 3, 4].filter(x => x > 2)))
print(toString([1, 2, 3, 4].reduce(0, (a, b) => a + b)))
print(toString([1, 2, 3, 4].find(x => x > 2)))
print(toString([1, 2, 3].some(x => x > 2)))
print(toString([1, 2, 3].every(x => x > 0)))
"#);
    assert_eq!(output, vec!["[2, 4, 6]", "[3, 4]", "10", "3", "true", "true"]);
}

// Regression (call-arg-box leak): passing a CONCRETE array to a Json-typed param (`for`'s
// iterable) inside an outer loop boxes the array into a fresh TaggedVal* shell each outer
// iteration. The shell was never freed → one-box-per-iteration leak. Caller now frees the
// shell after the call. This runs the nested loop under churn; correctness here also guards
// against an over-eager shell free corrupting the borrowed array (double-free / wrong result).
#[test]
fn test_nested_for_over_concrete_array_arg_box() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range, for } from "std/array"

var k = 0
val xs = [1, 2, 3]
range(0, 5000).for(j => xs.for(s => k = k + 1))
print(toString(k))
"#);
    assert_eq!(output, vec!["15000"]);
}

// Regression (call-arg-box leak): a concrete Object passed to a Json-typed param (`keys`)
// repeatedly under churn. Each call boxes the object into a fresh shell; the shell free must
// not touch the object's inner payload (which the object's own scope-exit release owns).
#[test]
fn test_object_to_json_param_under_churn() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { range, for, length } from "std/array"
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
fn test_map_filter_reduce() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map, filter, reduce } from "std/array"
import { for } from "std/array"

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
import { map, filter, reduce } from "std/array"

val result = [1, 2, 3, 4, 5]
  .map(x => x * x)
  .filter(x => x > 5)
  .reduce(0, (sum, x) => sum + x)
print(toString(result))
"#);
    assert_eq!(output, vec!["50"]);
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

val arr = []
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
fn test_division_by_zero_error() {
    let err = run_expect_err(r#"import { print } from "std/io"
import { toString } from "std/string"

val x = 10 / 0
print(toString(x))
"#);
    assert!(err.contains("division") || err.contains("zero"), "got: {}", err);
}

#[test]
fn test_multi_param_lambda() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { reduce } from "std/array"

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
fn test_logical_not_val_and_if() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val ready = true
print(toString(!ready))
val flag = false
if !flag then print("taken") else print("not-taken")
"#);
    assert_eq!(output, vec!["false", "taken"]);
}

#[test]
fn test_logical_not_in_match_guard() {
    let output = run(r#"import { print } from "std/io"

val cond = false
val describe = (n: Int32): String =>
  match n
    has Int32 when !cond => "guard-true"
    else => "guard-false"
print(describe(1))
"#);
    assert_eq!(output, vec!["guard-true"]);
}

#[test]
fn test_logical_not_precedence() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

// !a == b parses as (!a) == b
print(toString(!true == false))
val obj = { "ok": false }
print(toString(!obj["ok"]))
val isZero = (n: Int32): Boolean => n == 0
print(toString(!isZero(5)))
val a = false
val b = true
print(toString(!a && b))
"#);
    assert_eq!(output, vec!["true", "true", "true", "true"]);
}

#[test]
fn test_logical_double_negation() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val x = true
print(toString(!!x == x))
print(toString(!!false))
"#);
    assert_eq!(output, vec!["true", "false"]);
}

#[test]
fn test_logical_not_typevar_operand() {
    // `!flag` where `flag` flows through a generic lambda parameter exercises
    // the unbox-to-i1 path in IR lowering.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val negate = (flag) => !flag
print(toString(negate(true)))
print(toString(negate(false)))
"#);
    assert_eq!(output, vec!["false", "true"]);
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
    // Spec §24: `&&` / `||` are SHORT-CIRCUITING — the RHS must NOT be evaluated when the LHS
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

#[test]
fn test_float32_widens_to_float64() {
    // A Float32 must widen to Float64 (fpext) across every numeric context, per spec §26
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

val describe = (items: Json): String =>
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

val describe = (items: Json): String =>
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
import { map, filter, reduce } from "std/array"

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
import { map, filter } from "std/array"

val f = (xs: Json): Json =>
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

val describe = (x: Json): String =>
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
fn test_default_args_basic() {
    // Omitting a trailing optional argument fills it from its default.
    let output = run(r#"import { print } from "std/io"

val greet = (name: String, greeting: String = "Hello") => "${greeting}, ${name}"
print(greet("World"))
print(greet("World", "Hi"))
"#);
    assert_eq!(output, vec!["Hello, World", "Hi, World"]);
}

#[test]
fn test_default_args_chained() {
    // A default may reference earlier parameters, including earlier defaults.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val box = (w: Int32, h: Int32 = w, area: Int32 = w * h) => area
print(toString(box(4)))
print(toString(box(4, 3)))
print(toString(box(4, 3, 99)))
"#);
    assert_eq!(output, vec!["16", "12", "99"]);
}

#[test]
fn test_default_args_object() {
    let output = run(r#"import { print } from "std/io"

val config = (name: String, opts: Json = { "v": false }) => "${name}:${opts}"
print(config("a"))
print(config("b", { "v": true }))
"#);
    assert_eq!(output, vec!["a:{\"v\": false}", "b:{\"v\": true}"]);
}

#[test]
fn test_default_args_indirect_value() {
    // Default-fill works when the function is held as a first-class value
    // (the closure carries a descriptor so the indirect call fills defaults).
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val scale = (x: Int32, factor: Int32 = 2) => x * factor
val g = scale
print(toString(g(5)))
print(toString(g(5, 3)))
"#);
    assert_eq!(output, vec!["10", "15"]);
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
import { iter } from "std/array"
import { for } from "std/array"

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

val arr = []
val obj = {}
print(toString(length(arr)))
print(toString(length(obj)))
"#);
    assert_eq!(output, vec!["0", "0"]);
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
import { map, for } from "std/array"

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
    // lambda literal would. Covers recursive + non-recursive callees, Json + Int args.

    // Recursive callee, Json args.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val leaf = (t: Json, p: Int32): Json => { "v": p }
val combine = (t: Json, l: Json, p: Int32, f: Function): Json =>
  if p >= 2 then { "v": l }
  else
    val r = f(t, p + 1)
    combine(t, r, r["v"], f)
val go = (t: Json): Json => combine(t, { "v": 0 }, 0, leaf)
print(toString(go([])))
"#);
    assert_eq!(output, vec![r#"{"v": {"v": 2}}"#]);

    // Non-recursive callee, Json args.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val leaf = (t: Json, p: Int32): Json => { "v": p }
val combine = (t: Json, l: Json, p: Int32, f: Function): Json => f(t, p)
val go = (t: Json): Json => combine(t, { "v": 0 }, 0, leaf)
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

val greetPerson = ({ name, age }: Json): String =>
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
import { for } from "std/array"

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
import { for } from "std/array"

val data = [1, 2, 3]
data.for(x =>
  print("a")
  print("b")
)
"#);
    assert_eq!(output, vec!["a", "b", "a", "b", "a", "b"]);
}

// A line-leading `[` after a statement inside an inline lambda body starts a NEW array-literal
// statement, not a postfix index on the previous expression. Inside `()` the line break is
// suppressed as a token (ADR-004), so the parser relies on each token's `newline_before` flag.
// Without this, `f` below parsed as `push(acc, 4)[ ... ]` and the body's value was the index
// result (Null) instead of the array. Mirrors the post-Dedent `[` suppression of ADR-011.
#[test]
fn test_line_leading_array_after_statement_in_inline_lambda() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { push, length } from "std/array"

val f = (): Json =>
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
import { map } from "std/array"
import { for } from "std/array"

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
import { length, push, concat } from "std/array"
import { for } from "std/array"

val arr = [1, 2]
push(arr, 3)
print(toString(length(arr)))

val combined = concat([1], [2, 3])
combined.for(x => print(toString(x)))
"#);
    assert_eq!(output, vec!["3", "1", "2", "3"]);
}

#[test]
fn test_keys_values_entries() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { keys, values } from "std/object"
import { for } from "std/array"

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

import { find, some, every } from "std/array"
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

#[test]
fn test_stdlib_array_flatmap_indexof_reverse() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { flatMap, indexOf, reverse } from "std/array"
import { for } from "std/array"

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
import { map } from "std/array"
import { for } from "std/array"

val process = (items: Json): Json =>
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
fn test_object_spread_basic() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { keys } from "std/object"

val src = { "a": 1, "b": 2 }
val merged = { ...src, "c": 3 }
print(toString(merged["a"]))
print(toString(merged["b"]))
print(toString(merged["c"]))
print(toString(keys(merged)))
"#);
    assert_eq!(output, vec!["1", "2", "3", "[\"a\", \"b\", \"c\"]"]);
}

#[test]
fn test_object_spread_override_explicit_after_spread() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { keys } from "std/object"

val src = { "a": 1, "b": 2 }
val merged = { ...src, "a": 99 }
print(toString(merged["a"]))
print(toString(merged["b"]))
print(toString(keys(merged)))
"#);
    assert_eq!(output, vec!["99", "2", "[\"a\", \"b\"]"]);
}

#[test]
fn test_object_spread_multiple() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { keys } from "std/object"

val a = { "x": 1, "y": 2 }
val b = { "y": 20, "z": 30 }
val merged = { ...a, ...b }
print(toString(merged["x"]))
print(toString(merged["y"]))
print(toString(merged["z"]))
print(toString(keys(merged)))
"#);
    assert_eq!(output, vec!["1", "20", "30", "[\"x\", \"y\", \"z\"]"]);
}

#[test]
fn test_object_spread_empty_source() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { keys } from "std/object"

val merged = { ...{}, "a": 1 }
print(toString(merged["a"]))
print(toString(keys(merged)))
"#);
    assert_eq!(output, vec!["1", "[\"a\"]"]);
}

#[test]
fn test_object_spread_null_error() {
    let err = run_expect_err(r#"import { print } from "std/io"

val merged = { ...null, "a": 1 }
print(merged["a"])
"#);
    assert!(err.contains("Object") || err.contains("spread") || err.contains("null"), "got: {}", err);
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
fn test_thread_pool_async() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { async, await, threadPool } from "std/async"

val pool = threadPool(2)
val p1 = async(() => 100)
val p2 = async(() => 200)
val r1 = await(p1)
val r2 = await(p2)
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
    // A worker handler may close over `var` (§32.6.4): the accumulator state is confined to
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
fn test_worker_message_fire_and_forget() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { worker, request, message, close } from "std/async"
import { push, length } from "std/array"

var log = []
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
    // in-flight request (§32.6.5); the program continues.
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
    // Sending to a closed worker yields an Error (§32.6.5), not a crash.
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
import { reduce } from "std/array"

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
import { for, range } from "std/array"

val pool = threadPool(3)
var promises = []
range(0, 30).for(i => push(promises, pool.poolAsync(() => 1)))
var total = 0
promises.for(p => total = total + await(p))
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
import { for, range } from "std/array"

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
    // §32.2.3: await auto-flattens — a thunk that itself returns a Promise resolves through.
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
    // §32.2.2: a thunk fault surfaces as an Error value; `is Error` discriminates it, and a
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
fn test_async_real_parallelism() {
    // Two thunks that each sleep 150ms. With real OS threads the wall-clock should be
    // ~150ms (overlap), not ~300ms (sequential). Assert it completed well under the
    // sequential bound — generous to avoid CI flakiness, but still proves overlap.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { async, await } from "std/async"
import { sleep, now } from "std/time"

val start = now()
val p1 = async(() =>
  sleep(150)
  1
)
val p2 = async(() =>
  sleep(150)
  2
)
val r1 = await(p1)
val r2 = await(p2)
val elapsed = now() - start
print(toString(r1 + r2))
if elapsed < 250 then print("PARALLEL") else print("SEQUENTIAL")
"#);
    assert_eq!(output, vec!["3", "PARALLEL"],
        "two 150ms thunks should overlap (real threads), completing in <250ms");
}

#[test]
fn test_async_fault_isolation_div_by_zero() {
    // A runtime fault (division by zero) inside an async thunk must be caught at the thread
    // boundary and surface as an Error value at await — the program continues (spec §32.2.2),
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
val sum = await(p1) + await(p2) + await(p3) + await(p4)
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
val total = await(a) + await(b) + await(c) + await(d)
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
    // A thunk capturing a function value (CAP_OPAQUE env) runs inline as a sound fallback.
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
import { iter } from "std/array"
import { for } from "std/array"

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

import { pathMatch } from "std/http"
val m = pathMatch("/users/:id/posts/:postId", "/users/42/posts/7")
print(m["id"])
print(m["postId"])
val none = pathMatch("/users/:id", "/products/5")
print(toString(none))
"#);
    assert_eq!(output, vec!["42", "7", "null"]);
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

#[test]
fn test_io_lines_reads_all_stdin_lines() {
    let output = run_with_stdin(r#"import { print } from "std/io"
import { for } from "std/array"
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
    let ws = workspace_root();
    let lin_bin = lin_bin();
    let mathlib_c = ws.join("examples/lib/mathlib.c");
    let mathlib_a = ws.join("examples/lib/libmathlib.a");
    let ffi_example = ws.join("examples/ffi-c.lin");
    let output_bin = ws.join("target/ffi_c_test");

    if !lin_bin.exists() {
        eprintln!("SKIP: lin binary not built; run `cargo build -p lin` first");
        return;
    }

    // Always rebuild the static library for the current platform — a pre-built .a from
    // a different arch (e.g. Linux x86_64 checked in, running on macOS ARM64) will fail to link.
    let obj = ws.join("examples/lib/mathlib.o");
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

    let compile_out = Command::new(&lin_bin)
        .args(["build", ffi_example.to_str().unwrap(), "-o", output_bin.to_str().unwrap()])
        .current_dir(&ws)
        .output()
        .expect("failed to run lin build");
    assert!(compile_out.status.success(),
        "lin build failed: {}", String::from_utf8_lossy(&compile_out.stderr));

    let run_out = Command::new(&output_bin).output().expect("failed to run ffi binary");
    assert!(run_out.status.success());
    let stdout = String::from_utf8_lossy(&run_out.stdout);
    assert!(stdout.contains("3 + 4 = 7"), "Expected '3 + 4 = 7', got: {}", stdout);
    assert!(stdout.contains("2.5^2 = 6.25"), "Expected '2.5^2 = 6.25', got: {}", stdout);
}

// ── Formatter idempotency ─────────────────────────────────────────────────────

/// Lex, parse, and format a Lin source string. Panics on parse errors.
fn fmt(source: &str) -> String {
    let tokens = lin_lex::Lexer::new(source, 0).tokenize();
    let mut parser = lin_parse::Parser::new(tokens);
    let module = parser.parse_module();
    assert!(
        parser.diagnostics.is_empty(),
        "parse errors: {:?}\nsource:\n{}",
        parser.diagnostics.iter().map(|d| d.message.clone()).collect::<Vec<_>>(),
        source
    );
    lin_parse::Formatter::new().format_module(&module)
}

#[test]
fn test_fmt_idempotent() {
    // Source with varied constructs: if/match/function/objects/arrays/imports/types.
    let source = r#"import { print } from "std/io"
import { map, filter, reduce, for } from "std/array"
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
    // The NAL-type extraction example from spec §35.2.
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
import { reduce } from "std/array"

print(toString([1, 2, 4, 8].reduce(0, (acc, x) => acc | x)))
print(toString([15, 7, 3].reduce(255, (acc, x) => acc & x)))
print(toString([1, 2, 3].reduce(1, (acc, x) => acc << x)))
"#);
    assert_eq!(output, vec!["15", "3", "64"]);
}

#[test]
fn test_bitwise_boxed_projection_operand() {
    // Regression: a bitwise op whose operand is a boxed-Json projection (`bytes[i]` out of a
    // Json array), used in a recursive call argument, must unbox the operand before the LLVM
    // integer op. Previously only Add/Sub/Mul/Div/Mod unboxed union operands; bitwise ops did
    // not, so the boxed `TaggedVal*` reached codegen as an int operand → codegen type-mismatch
    // crash. A recursive XOR checksum exercises exactly this path.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"

val checksum = (bytes: Json, i: Int32, acc: Int32): Int32 =>
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
import { for, range } from "std/array"
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
import { for, range } from "std/array"
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
    // Int8[] stores signed bytes; negative literals (written `-N` with a preceding space so
    // the lexer treats `-` as part of the literal) round-trip.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val s: Int8[] = [ -1, 127]
print(toString(s[0]))
print(toString(s[1]))
"#);
    assert_eq!(out, vec!["-1", "127"]);
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
    // is a compile-time error (spec §26 context-typed literal + range check).
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
    // the literal adopts the operand's type (spec §26) so no spurious widening crashes codegen
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
    // Regression for the union var-cell use-after-free: a captured `var` of union (Json) type
    // reassigned to a freshly-allocated OBJECT literal each iteration. Before the owning model
    // (clone-on-store/read, release-old, balanced teardown) the cell aliased a temp object that
    // was freed at closure-scope exit, so the final read saw freed/garbage memory.
    let out = run(r#"import { range, for } from "std/array"
import { print } from "std/io"
import { toString } from "std/string"

var acc: Json = { "v": 0 }
range(0, 2000).for(i => acc = { "v": i })
print(toString(acc["v"]))
"#);
    assert_eq!(out, vec!["1999"]);
}

#[test]
fn test_json_var_array_reassign_loop_no_uaf() {
    // Same bug, ARRAY literal variant: a captured `var: Json` reassigned to a fresh array each
    // iteration. A use-after-free here corrupted the length read (or crashed).
    let out = run(r#"import { range, for, length } from "std/array"
import { print } from "std/io"
import { toString } from "std/string"

var acc: Json = [0, 0, 0]
range(0, 2000).for(i => acc = [i, i, i])
print(toString(length(acc)))
"#);
    assert_eq!(out, vec!["3"]);
}

#[test]
fn test_reduce_minby_maxby_churn_no_double_free() {
    // Exercises the stdlib `reduce` Json accumulator cell plus the pass-through reducers used
    // by `minBy`/`maxBy` (which return a borrowed argument). The earlier half-fix (owning store
    // but borrowing read) double-freed these borrowed values. With the symmetric clone-based
    // owning model the accumulator cell owns its own box and never frees the borrowed inputs.
    // 2000 iterations of sum/min/max over churned arrays — a double-free corrupts results or
    // aborts the process.
    let out = run(r#"import { range, for, map, reduce, minBy, maxBy, length } from "std/array"
import { print } from "std/io"
import { toString } from "std/string"

var total: Json = 0
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
fn test_for_callback_json_assign_loop_correct() {
    // Regression for the for-callback-return box leak fix. The `for` callback's boxed-ABI
    // return is now released every iteration. For a body that is an ASSIGNMENT to a captured
    // `var: Json` (`acc = concat(acc, [i])`), the assignment expression's result is the value
    // that ALSO flows into the cell; the fix makes the global/cell own a CLONED, independent
    // box and returns an independently-owned box, so the per-iteration release frees exactly the
    // discarded return and never the value the cell keeps. Over 5000 iterations a wrong release
    // (double-free / use-after-free) corrupts the final length or aborts. The final array must
    // contain all 5000 appended elements.
    let out = run(r#"import { range, for, concat, length } from "std/array"
import { print } from "std/io"
import { toString } from "std/string"

var acc: Json = []
range(0, 5000).for(i => acc = concat(acc, [i]))
print(toString(length(acc)))
"#);
    assert_eq!(out, vec!["5000"]);
}

#[test]
fn test_for_callback_side_effect_sum_loop_correct() {
    // Regression for the for-callback-return box leak: a side-effecting body that mutates a
    // captured non-Json `var` (`s = s + i`). The callback boxes its result for the uniform ABI
    // each iteration (a fresh, independently-owned box once `s` grows past the small-int cache);
    // the fix releases that discarded box every iteration. Correctness must be unaffected:
    // sum(0..10000) = 10000*9999/2 = 49995000.
    let out = run(r#"import { range, for } from "std/array"
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
    // Int32 element into a fresh `TaggedVal*` for the Json callback param; that per-iteration box
    // was leaked (~36 B/iter). The fix reclaims the box shell every iteration via
    // `lin_tagged_free_box_if_distinct` (skipping when the callback returned that very box, e.g.
    // an identity body). Over 50000 iterations correctness must be unaffected: a wrong (double)
    // free would abort or corrupt the accumulator. sum(0..50000) = 50000*49999/2 = 1249975000.
    let out = run(r#"import { range, for } from "std/array"
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
    let out = run(r#"import { for, range } from "std/array"
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
import { concat, length } from "std/array"
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
    let out = run(r#"import { udpBind, udpSendTo, udpRecv, udpRecvFrom, udpSetNonblocking, udpClose } from "std/net"
import { print } from "std/io"
import { toString } from "std/string"

val port = 39201
val sock = udpBind(port)
print("bound: ${toString(sock["type"] != "error")}")

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
print("len: ${toString(res["len"])}")
print("addr: ${toString(res["addr"])}")
print("b0: ${toString(buf[0])}")
print("b1: ${toString(buf[1])}")
print("b2: ${toString(buf[2])}")
print("b3: ${toString(buf[3])}")

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
    let out = run(r#"import { tcpListen, tcpAccept, tcpConnect, tcpRecv, tcpSend, tcpClose } from "std/net"
import { print } from "std/io"
import { toString } from "std/string"

val port = 39202
val listener = tcpListen(port)
print("listening: ${toString(listener["type"] != "error")}")

val client = tcpConnect("127.0.0.1", port)
print("connected: ${toString(client["type"] != "error")}")

val accepted = tcpAccept(listener)
val server = accepted["fd"]
print("accepted: ${toString(accepted["type"] != "error")}")

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
// std/proc — subprocesses, and std/tty — raw terminal (Milestone 21, Layer 3)
//
// std/proc is deterministic: we spawn a real `sh -c` process, read its piped
// stdout, and wait for its exit code. std/tty cannot be exercised under the
// test harness (stdin is a pipe, not a TTY); we only assert that rawMode on a
// non-TTY returns an Error object gracefully (no panic / no crash).
// ===========================================================================

#[test]
fn test_proc_spawn_read_wait() {
    // Spawn `sh -c 'printf hello'`, read its stdout into a buffer, assert the
    // bytes, then wait for exit code 0. `sh -c` is the most portable spawn.
    let out = run(r#"import { spawn, readStdout, wait } from "std/proc"
import { print } from "std/io"
import { toString } from "std/string"

val h = spawn(["sh", "-c", "printf hello"])
print("spawned: ${toString(h["type"] != "error")}")

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

#[test]
fn test_proc_wait_exit_code() {
    // `sh -c 'exit 3'` exits with code 3.
    let out = run(r#"import { spawn, wait } from "std/proc"
import { print } from "std/io"
import { toString } from "std/string"

val h = spawn(["sh", "-c", "exit 3"])
val code = wait(h)
print("code: ${toString(code)}")
"#);
    assert_eq!(out, vec!["code: 3"]);
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
fn test_concrete_string_into_json_var_loop() {
    // Regression: reassigning a fresh CONCRETE value (toString -> String) into a Json/union
    // `var` inside a loop boxes the value via Coerce, producing a transient TaggedVal* shell.
    // The LocalSet store path used to clone that box for the global/cell AND for the result
    // but never freed the transient shell, leaking ~36 bytes per iteration. The fix frees the
    // shell (FreeBoxShell) after both clones. This asserts correctness: the var must hold the
    // last assigned value and the program must not crash (no use-after-free / double-free).
    let out = run(r#"import { range, for } from "std/array"
import { toString } from "std/string"
import { print } from "std/io"

var last: Json = ""
range(0, 5).for(i => last = toString(i))
print(toString(last))
"#);
    assert_eq!(out, vec!["4"]);
}

#[test]
fn test_concrete_object_into_json_var_loop() {
    // Regression companion to the String case: a fresh concrete Object boxed into a Json var
    // each iteration. Exercises the same transient-coercion-box free path with an Object payload
    // and confirms the final stored value is correct.
    let out = run(r#"import { range, for } from "std/array"
import { toString } from "std/string"
import { print } from "std/io"

var last: Json = null
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
    //     value inside a Json scrutinee. A binding is a named catch-all: it always matches.
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

    // A binding over a Json scrutinee mixed with a literal arm: the binding must match
    // unconditionally (it was lowered as a type-check that failed for a concrete value
    // inside a Json scrutinee, so the literal-or-else path was taken instead).
    // `examples/web-server/router.test.lin` exercises the full guarded router shape.
    let out = run(r#"import { print } from "std/io"
val classify = (req: Json): String =>
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
    // Regression for the Json call-result leak: a `map` call returns a `Json` (boxed `TaggedVal*`)
    // that is bound to a per-iteration `val m` and DISCARDED. `register_owned`'s old `is_rc_type`
    // gate excluded unions, so the owned box (and its inner array) was never released — a per-
    // iteration leak. The fix registers union import-fn call results so scope exit tag-releases
    // them. Correctness gate: over 20000 iterations, summing the lengths must stay exact and the
    // process must not abort (a wrong release would double-free the map result). 20000 * 3 = 60000.
    let out = run(r#"import { range, for, map, length } from "std/array"
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
    // Companion to the map case for `filter` (also returns a fresh `Json` array). Each iteration
    // discards the filtered array; the per-iteration release must reclaim it without corrupting
    // the source literal or the count. 20000 iterations; each filter keeps the 2 elements > 0
    // (1 and 2 are always > i is false for i>=1, so use a fixed predicate): [1,2,3,4] filtered by
    // x > 2 yields [3,4] every time → 20000 * 2 = 40000.
    let out = run(r#"import { range, for, filter, length } from "std/array"
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
    let out = run(r#"import { range, for, map, length } from "std/array"
import { print } from "std/io"
import { toString } from "std/string"

val doubled = (xs: Json): Json =>
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
    // Regression: a Json/union projection (`obj[k]` / `obj.field`) RETURNED from a function
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
val pluck = (x: Json): Json => x["name"]
print(pluck({ "name": "Alice" }))
"#);
    assert_eq!(out, vec!["Alice"]);

    // Projection returned from a map CALLBACK closure, result stored into an array then iterated:
    // each element must be an owned box the array releases exactly once.
    let out = run(r#"import { print } from "std/io"
import { for, map } from "std/array"
val records = [{ "name": "Alice" }, { "name": "Bob" }]
records.map(r => r["name"]).for(n => print(n))
"#);
    assert_eq!(out, vec!["Alice", "Bob"]);

    // Nested projection (`r["value"]["name"]`) through a map callback: the inner projection is a
    // transient read, the outer escapes — only the escaping result is cloned.
    let out = run(r#"import { print } from "std/io"
import { map, for } from "std/array"
val records = [{ "value": { "name": "Alice" } }, { "value": { "name": "Bob" } }]
val names = records.map(r => r["value"]["name"])
names.for(n => print(n))
"#);
    assert_eq!(out, vec!["Alice", "Bob"]);

    // Projection bound to a `val` and THEN returned (a different escape route into the return
    // boundary than a bare projection expression): the bound borrowed projection must still be
    // cloned to an owned box before it leaves the scope.
    let out = run(r#"import { print } from "std/io"
val pluck = (x: Json): Json =>
  val n = x["name"]
  n
print(pluck({ "name": "Carol" }))
"#);
    assert_eq!(out, vec!["Carol"]);

    // Calling the projection-returning function many times in a loop must stay balanced (the
    // per-call clone is released each iteration; a relapse to the borrowed-return double-free,
    // or a per-iteration over-clone leak, would surface here / under the ASan CI leg).
    let out = run(r#"import { print } from "std/io"
import { range, for } from "std/array"
import { toString } from "std/string"
val pluck = (x: Json): Json => x["v"]
var c = 0
range(0, 2000).for(i =>
  c = c + 1
  print(toString(pluck({ "v": "x" })))
)
print(toString(c))
"#);
    assert_eq!(out.last().map(|s| s.as_str()), Some("2000"));
}

// Regression: the error-propagation idiom `val r = <owned Json call result>; if cond then r
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
val deep = (): Json => { "type": "failure" }
val top = (b: Boolean): Json =>
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
val deep = (): Json => { "type": "failure", "error": "eof" }
val top = (): Json =>
  val r = deep()
  if r["type"] == "failure" then r
  else { "type": "success", "value": r["node"] }
print(toString(top()))
"#);
    assert_eq!(out, vec![r#"{"type": "failure", "error": "eof"}"#]);

    // Both branches are union (`r` and another call result `mk()`): the merge stays boxed and
    // must clone the borrowed `r` so the scope-release of `r` does not dangle the result.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val mk = (): Json => { "type": "failure", "k": "v" }
val pick = (i: Int32): Json =>
  val r = mk()
  if i > 0 then r else mk()
print(toString(pick(5)))
print(toString(pick(0)))
"#);
    assert_eq!(out, vec![r#"{"type": "failure", "k": "v"}"#, r#"{"type": "failure", "k": "v"}"#]);

    // Multi-level propagation: `mid` returns `r` (from `deep`) on failure, `top` returns `r`
    // (from `mid`) on failure — the owned union local is forwarded through two `if`-branches.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"
val isFailure = (x: Json): Boolean => x["type"] == "failure"
val deep = (arr: Json, pos: Int32): Json =>
  if pos >= length(arr) then { "type": "failure", "error": "eof" }
  else { "node": arr[pos], "pos": pos + 1 }
val mid = (arr: Json, pos: Int32): Json =>
  val r = deep(arr, pos)
  if isFailure(r) then r
  else { "node": r["node"], "pos": r["pos"] }
val top = (arr: Json): Json =>
  val r = mid(arr, 5)
  if isFailure(r) then r
  else { "type": "success", "value": r["node"] }
print(toString(top([1, 2])))
"#);
    assert_eq!(out, vec![r#"{"type": "failure", "error": "eof"}"#]);

    // Returned-in-a-loop with the result discarded: a per-call leak (the if-branch clone
    // re-cloned by the function return) would surface here under the ASan CI leg; functionally
    // it must just run to completion.
    let out = run(r#"import { print } from "std/io"
import { for, range } from "std/array"
val mk = (): Json => { "type": "failure", "k": "v" }
val pick = (i: Int32): Json =>
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
fn object_index_assign_of_callback_param() {
    // Regression: `obj[key] = value` where `value` is a for/map callback PARAMETER used to
    // store NULL. Under the uniform closure ABI a callback param arrives BOXED (a TaggedVal*),
    // but `compile_ir_index_set` re-wrapped it via `build_tagged_val_alloca` using the param's
    // STATIC scalar type — that path saw a pointer where it expected an int, tagged the box as
    // NULL, and dropped the value (the boxed-value-dropped bug). The fix passes an
    // already-boxed Json value straight to the object/array setter.

    // Int value via `for` callback param.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { for } from "std/array"
[5].for(n =>
  var o = {}
  o["x"] = n
  print(toString(o))
)
"#);
    assert_eq!(out, vec![r#"{"x": 5}"#]);

    // Int values accumulated via `map` callback, returning the built object.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { map } from "std/array"
val rs = [5, 6].map(n =>
  var o = {}
  o["x"] = n
  o
)
print(toString(rs))
"#);
    assert_eq!(out, vec![r#"[{"x": 5}, {"x": 6}]"#]);

    // String value via callback param.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { for } from "std/array"
["hi"].for(s =>
  var o = {}
  o["msg"] = s
  print(toString(o))
)
"#);
    assert_eq!(out, vec![r#"{"msg": "hi"}"#]);

    // Captured-`var` object accumulated across a loop, with the callback param as the KEY
    // (a boxed string key must be unboxed to a raw LinString*).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { for } from "std/array"
var out = {}
["a", "b", "c"].for(k =>
  out[k] = 1
)
print(toString(out))
"#);
    assert_eq!(out, vec![r#"{"a": 1, "b": 1, "c": 1}"#]);

    // Churn loop: building an object via index-assign of a callback param across many
    // iterations must not leak (verified under the ASan CI leg); functionally just completes.
    let out = run(r#"import { print } from "std/io"
import { for, range } from "std/array"
val main = (): Null =>
  range(0, 2000).for(i =>
    var o = {}
    o["k"] = i
    null
  )
  print("done")
main()
"#);
    assert_eq!(out, vec!["done"]);
}

// Regression: `==` against a boxed-key projection operand was ORDER-DEPENDENT. Inside a
// for/map callback, `m[k]` (with `k` the boxed callback param) is a boxed-Json projection,
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
import { for } from "std/array"
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
import { for } from "std/array"
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
import { for } from "std/array"
val sch = { "host": { "type": "string" }, "port": { "type": "number" } }
["host", "port"].for(k =>
  print(toString(sch[k]["type"] == "string"))
  print(toString("string" == sch[k]["type"]))
  print(toString(sch[k]["type"] != "string"))
)
"#);
    assert_eq!(out, vec!["true", "true", "false", "false", "false", "true"]);
}
