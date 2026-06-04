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
import { range } from "std/iter"
import { for } from "std/iter"

range(1, 4).for(i => print(toString(i)))
"#);
    assert_eq!(output, vec!["1", "2", "3"]);
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

// Regression (call-arg-box leak): passing a CONCRETE array to a Json-typed param (`for`'s
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

// Regression (call-arg-box leak): a concrete Object passed to a Json-typed param (`keys`)
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

// Regression: arithmetic on two BOXED (Json/union) operands — e.g. Float64 fields
// destructured from an object by a `has` pattern — dispatched on a hardcoded Int32
// unbox, so `3.0 * 4.0` reinterpreted the float bits as an integer and returned 0.
// Codegen now routes boxed-operand Add/Sub/Mul/Div/Mod through lin_tagged_arith,
// which dispatches on the runtime tag (float result if either operand is a float).
#[test]
fn test_boxed_json_float_arithmetic() {
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val o: Json = { "a": 3.0, "b": 4.0 }
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
val oi: Json = { "a": 3, "b": 4 }
val imul = match oi
  has { a, b } => a * b
  else => -1
print(toString(imul))

// Mixed int/float widens to float.
val om: Json = { "a": 3, "b": 4.0 }
val mmul = match om
  has { a, b } => a * b
  else => -1.0
print(toString(mmul))
"#);
    assert_eq!(output, vec!["12.0", "7.0", "0.75", "12", "12.0"]);
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
    let compile = Command::new(lin_bin())
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
    let compile = Command::new(lin_bin())
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
    let build1 = Command::new(lin_bin())
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
    let build2 = Command::new(lin_bin())
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
    // Documents the KNOWN boundary-soundness gap in cyclic-import inference (ADR-078).
    // A 3-module cycle a -> b -> c -> a where the only literal lives in `fromC`, and
    // `fromA`/`fromB` get their return type only by calling through a peer.
    //
    // RUNTIME is correct: codegen calls the real symbol, so `fromA(3)` returns "done".
    // STATIC TYPE is lost at the boundary: `fromA`'s return type flows through a peer call,
    // so the single-round SCC fixed point leaves it permissive/unsolved — a consumer can
    // bind the (actually-String) result to Int32 with NO type error. That missed error is
    // the gap. If a future change iterates Phase 2 to convergence (or fails closed by
    // requiring an annotation), the second half of this test should start failing — update
    // ADR-078 and flip the assertion when it does.
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
    let check = Command::new(lin_bin())
        .args(["check", bad_path.to_str().unwrap()])
        .output()
        .expect("failed to invoke lin binary — run `cargo build -p lin` first");
    let check_out = format!("{}{}",
        String::from_utf8_lossy(&check.stderr), String::from_utf8_lossy(&check.stdout));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(check.status.success(),
        "ADR-078 boundary gap: binding a peer-dependent cyclic return to Int32 is currently \
         accepted. If this now FAILS, the gap was closed — flip this assertion and update ADR-078. \
         got: {check_out}");
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

// Fixed-length array types (`[T1, T2, ...]`, spec §5.3). An array literal checked
// against a fixed-length type is stored as a TAGGED array (heterogeneous positional
// element types); indexing reads the tagged slot and unboxes to the positional type.
// Regression: before, the literal inferred to the unbounded `T[]` and failed the type
// check; after a partial fix it type-checked but indexing read flat bytes and returned
// garbage. This covers heterogeneous + homogeneous + float positions + Json[] widening.
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
val widened: Json[] = pair
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
    // Regression (ADR-060 owning captures): a `map` callback that RETURNS a closure capturing
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
// statements (the ADR-004 newline-suppression bug used to drop all but the first, making the
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

// A `match` expression inside a parenthesised lambda body must parse: ADR-004 suppresses the
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
// parser). Sanity that the offside change didn't disturb ADR-004 multiline literals in parens.
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
// expression (ADR-006). The offside guard only runs BETWEEN statements, never within a single
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
    // (the generic fill path re-wrapped the already-boxed Json arg in a NULL-tagged box).
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
    // wrapper used to be `(n, fill: Json): Json` — erasing the element type, so it always built
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
    // the old `fill: Json` path always took. The fix retains per slot for any heap-payload fill
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

var o = {}
range(0, 30).for(i => lin_object_set(o, "k${toString(i)}", i * 10))
var sum = 0i64
range(0, 30).for(i => sum = sum + o["k${toString(i)}"])
print(toString(length(keys(o))))
print(toString(sum))
"#);
    assert_eq!(output, vec!["30", "4350"]);
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
fn test_object_spread_null_noop() {
    // Spreading null contributes no fields (it is not a runtime error).
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { keys } from "std/object"

val merged = { ...null, "a": 1 }
print(toString(merged["a"]))
print(toString(keys(merged)))
"#);
    assert_eq!(output, vec!["1", "[\"a\"]"]);
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
    // When exactly one branch is literal Null and the other is `Json` (the dynamic top type
    // that already subsumes Null), the result collapses to `Json` rather than `Json | Null`.
    // This both avoids a redundant union and keeps the internal `?T…` sentinel out of
    // diagnostics. A `Json` result is assignable to `Int32` under the lenient-json rule.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val j: Json = 7
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
    // §24.2.2 enforcement (ADR-070): await yields `T | Error`, so assigning it to a bare
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

val unwrap = (r: Json): Int32 =>
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

val unwrap = (r: Json): Int32 =>
  match r
    is Error => 0
    else => r
val pool = threadPool(3)
var promises = []
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
    // ADR-044: Shared<T> is accessor-only. Passing a Shared value to a non-accessor (here
    // `push`, which wants an array/Json) is a compile-time type error — the Shared box never
    // auto-unwraps to its inner type or to Json.
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
fn test_async_real_parallelism() {
    // Two thunks that each sleep 150ms. With real OS threads the wall-clock should be
    // ~150ms (overlap), not ~300ms (sequential). Assert it completed under the sequential
    // bound — generous to avoid CI flakiness (slow/oversubscribed runners), but still
    // proves overlap since the sequential floor is ~300ms.
    let output = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { async, await } from "std/async"
import { sleep, now } from "std/time"

val unwrap = (r: Json): Int32 =>
  match r
    is Error => 0
    else => r
val start = now()
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
val elapsed = now() - start
print(toString(r1 + r2))
if elapsed < 290 then print("PARALLEL") else print("SEQUENTIAL")
"#);
    assert_eq!(output, vec!["3", "PARALLEL"],
        "two 150ms thunks should overlap (real threads), completing well under the ~300ms sequential floor");
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

val unwrap = (r: Json): Int32 =>
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

val unwrap = (r: Json): Int32 =>
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

val unwrapTw = (r: Json): Int32 =>
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

val unwrapS = (r: Json): String =>
  match r
    is Error => "err"
    else => r
val unwrapB = (r: Json): Boolean =>
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
// boundary and surfaces as an `Error` at `await` (ADR-070 / §32.2.2), NOT a crash.
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

val boom = (line: Json): Json =>
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

export val router = (req: Json): Json =>
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
    // SDL_RenderReadPixels readback proves real software rendering happened.
    assert!(stdout.contains("pixel[184,124] = 255,128,0"), "got: {}", stdout);
    assert!(stdout.contains("rendered pixel matches fill: true"), "got: {}", stdout);
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
    let run_out = run_cmd.output().expect("failed to run relocated binary");
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
    // round-trip safety — ADR-007 / d6e7bdb.)
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
    let source = "val f = (s: Json): String =>\n  match s\n    has { \"circle\" } when big => \"a\"\n    has { \"rect\" }            => \"bb\"\n    else                      => \"c\"\n";
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
    let single = "val g = (s: Json): String =>\n  match s\n    has { \"circle\" } => \"a\"\n    has { \"rect\" } => \"bb\"\n    else => \"c\"\n";
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
    // A function call `f(x,)` (partial application, ADR-041) is STILL accepted.
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
    Command::new(lin_bin())
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
    let ok = Command::new(lin_bin())
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
    // Regression for the union var-cell use-after-free: a captured `var` of union (Json) type
    // reassigned to a freshly-allocated OBJECT literal each iteration. Before the owning model
    // (clone-on-store/read, release-old, balanced teardown) the cell aliased a temp object that
    // was freed at closure-scope exit, so the final read saw freed/garbage memory.
    let out = run(r#"import { range, for } from "std/iter"
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
    let out = run(r#"import { length } from "std/array"
import { range, for } from "std/iter"
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
    let out = run(r#"import { minBy, maxBy, length } from "std/array"
import { range, for, map, reduce } from "std/iter"
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
fn test_generic_combinator_pipeline_inlined() {
    // ADR-069: generic map/filter/reduce + the capture-less-lambda inliner. The monomorphic scalar
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
    // ADR-069: the inliner fires ONLY for a capture-less literal lambda; a capturing lambda and a
    // stored/passed `Function` value must keep the (correct, boxed) closure path. Also exercises the
    // tagged String element path and a non-scalar (array) reduce accumulator (the boxed Json-phi
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
// non-scalar (array) reduce accumulator -> boxed Json-phi path
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
    // `var: Json` (`acc = concat(acc, [i])`), the assignment expression's result is the value
    // that ALSO flows into the cell; the fix makes the global/cell own a CLONED, independent
    // box and returns an independently-owned box, so the per-iteration release frees exactly the
    // discarded return and never the value the cell keeps. Over 5000 iterations a wrong release
    // (double-free / use-after-free) corrupts the final length or aborts. The final array must
    // contain all 5000 appended elements.
    let out = run(r#"import { length } from "std/array"
import { range, for, concat } from "std/iter"
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
    // Int32 element into a fresh `TaggedVal*` for the Json callback param; that per-iteration box
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
    // PRESERVE the input's representation. Json[] stays Json[]; a flat UInt8[]/Int32[] stays
    // flat (proven byte-level via u32FromBe, which reads `(*arr).data as *const u8` — a tagged
    // result would decode garbage); String[] stays tagged and its strings survive RC retain.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { append, prepend, length } from "std/array"
import { u32FromBe } from "std/bytes"

// Json[] (tagged scalars)
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

#[test]
fn test_group_by_even_odd_and_empty() {
    // groupBy now does ONE hash lookup per item (lin_object_get_or_insert_array) + push,
    // instead of get-then-set. Grouping by even/odd splits correctly; an empty input is {}.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { groupBy } from "std/array"

val g = groupBy([1, 2, 3, 4, 5], x => if x % 2 == 0 then "even" else "odd")
print(toString(g["even"]))   // [2, 4]
print(toString(g["odd"]))    // [1, 3, 5]

val ge = groupBy([], x => "k")
print(toString(ge))          // {}

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
    // object is Json-typed, so the read routes through the runtime's tag-driven
    // lin_tagged_to_string). Codegen tagged the slot TAG_FLOAT32 (4) but wrote an f64-bits
    // payload (the value is fpext'd to f64 before storing); the runtime reads a TAG_FLOAT32
    // payload as `f32::from_bits(payload as u32)` → the low 32 bits of 1.5f64's pattern are 0
    // → it printed 0.0 / JSON "f": 0. Now stored as TAG_FLOAT64 with f64 bits → reads back 1.5.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"

val obj: Json = { "f": 1.5f32, "n": 7 }
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

// Stage 5 (streams): the unified process-stdout source. `spawn` a child, wrap its piped stdout
// as a Stream<UInt8[]>, and `readText` the whole output through the stream layer.
#[test]
fn test_stream_process_stdout_source() {
    let out = run(r#"import { spawn } from "std/process"
import { stdoutStream } from "std/process"
import { readText } from "std/stream"
import { print } from "std/io"

val h = spawn("sh", ["-c", "printf 'line1\nline2\n'"])
val text = stdoutStream(h).readText()
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
    let out = run(r#"import { tcpListen, tcpAccept, tcpConnect, tcpSend, tcpClose, tcpStream } from "std/net"
import { readText } from "std/stream"
import { print } from "std/io"

val port = 39271
val listener = tcpListen(port)
val client = tcpConnect("127.0.0.1", port)
val accepted = tcpAccept(listener)
val server = accepted["fd"]
val payload: UInt8[] = [72, 105, 33]
tcpSend(client, payload)
tcpClose(client)
val text = tcpStream(server).readText()
print("got: ${text}")
tcpClose(listener)
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
    // Regression: reassigning a fresh CONCRETE value (toString -> String) into a Json/union
    // `var` inside a loop boxes the value via Coerce, producing a transient TaggedVal* shell.
    // The LocalSet store path used to clone that box for the global/cell AND for the result
    // but never freed the transient shell, leaking ~36 bytes per iteration. The fix frees the
    // shell (FreeBoxShell) after both clones. This asserts correctness: the var must hold the
    // last assigned value and the program must not crash (no use-after-free / double-free).
    let out = run(r#"import { range, for } from "std/iter"
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
    let out = run(r#"import { range, for } from "std/iter"
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
    // Companion to the map case for `filter` (also returns a fresh `Json` array). Each iteration
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
import { range, for } from "std/iter"
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
import { for, range } from "std/iter"
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
import { for } from "std/iter"
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
import { map } from "std/iter"
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
import { for } from "std/iter"
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
import { for } from "std/iter"
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
import { for, range } from "std/iter"
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
// fromJson type-directed decode (ADR-047)
// ---------------------------------------------------------------------------

#[test]
fn test_from_json_object_success() {
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type Person = { "name": String, "age": Int32 }
val p = Person.fromJson({ "name": "Bob", "age": 30 })
print(if p["type"] == "error" then "ERR" else "${p["name"]} ${p["age"]}")
"#);
    assert_eq!(out, vec!["Bob 30"]);
}

#[test]
fn test_from_json_direct_call_form() {
    // fromJson(T, j) equals T.fromJson(j).
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type Person = { "name": String, "age": Int32 }
val p = fromJson(Person, { "name": "Zoe", "age": 9 })
print(if p["type"] == "error" then "ERR" else "${p["name"]} ${p["age"]}")
"#);
    assert_eq!(out, vec!["Zoe 9"]);
}

#[test]
fn test_from_json_missing_required_field() {
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type Person = { "name": String, "age": Int32 }
val p = Person.fromJson({ "name": "Bob" })
print(if p["type"] == "error" then "ERR" else "OK")
"#);
    assert_eq!(out, vec!["ERR"]);
}

#[test]
fn test_from_json_missing_nullable_field_ok() {
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type Opt = { "name": String, "nick": String | Null }
val p = Opt.fromJson({ "name": "Bob" })
print(if p["type"] == "error" then "ERR" else "OK ${p["name"]}")
"#);
    assert_eq!(out, vec!["OK Bob"]);
}

#[test]
fn test_from_json_extra_field_ignored() {
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type Person = { "name": String, "age": Int32 }
val p = Person.fromJson({ "name": "Bob", "age": 30, "extra": true })
print(if p["type"] == "error" then "ERR" else "OK ${p["name"]}")
"#);
    assert_eq!(out, vec!["OK Bob"]);
}

#[test]
fn test_from_json_wrong_type() {
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type Person = { "name": String, "age": Int32 }
val p = Person.fromJson({ "name": "Bob", "age": "x" })
print(if p["type"] == "error" then "ERR ${p["path"]}" else "OK")
"#);
    assert_eq!(out, vec!["ERR $.age"]);
}

#[test]
fn test_from_json_int_range_reject() {
    // `3.14` is non-integral; `5000000000.0` is integral but exceeds Int32's range. (A bare
    // suffixless integer literal like 5000000000 is truncated to Int32 by the lexer before it
    // ever reaches the decoder — spec §21 — so the overflow case is expressed as a float.)
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type T = { "n": Int32 }
val a = T.fromJson({ "n": 3.14 })
val b = T.fromJson({ "n": 5000000000.0 })
print(if a["type"] == "error" then "a ERR" else "a OK")
print(if b["type"] == "error" then "b ERR" else "b OK")
"#);
    assert_eq!(out, vec!["a ERR", "b ERR"]);
}

#[test]
fn test_from_json_float_accepts_int() {
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type T = Float64
val x = T.fromJson(5)
print(if x["type"] == "error" then "ERR" else "OK ${x}")
"#);
    assert_eq!(out, vec!["OK 5"]);
}

#[test]
fn test_from_json_nested_object() {
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type Addr = { "city": String }
type Person = { "name": String, "address": Addr }
val ok = Person.fromJson({ "name": "A", "address": { "city": "NYC" } })
val bad = Person.fromJson({ "name": "A", "address": { "city": 5 } })
print(if ok["type"] == "error" then "ERR" else "OK ${ok["address"]["city"]}")
print(if bad["type"] == "error" then "ERR ${bad["path"]}" else "OK")
"#);
    assert_eq!(out, vec!["OK NYC", "ERR $.address.city"]);
}

#[test]
fn test_from_json_array() {
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type T = Int32[]
val bad = T.fromJson([1, 2, "x"])
print(if bad["type"] == "error" then "ERR ${bad["path"]}" else "OK")
"#);
    assert_eq!(out, vec!["ERR $[2]"]);
}

#[test]
fn test_from_json_fixed_array() {
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type Pair = [String, Int32]
val ok = Pair.fromJson(["a", 7])
val wrong_len = Pair.fromJson(["a", 7, 9])
print(if ok["type"] == "error" then "ERR" else "OK ${ok[0]} ${ok[1]}")
print(if wrong_len["type"] == "error" then "LEN_ERR" else "OK")
"#);
    assert_eq!(out, vec!["OK a 7", "LEN_ERR"]);
}

#[test]
fn test_from_json_union_variant() {
    // First structurally-matching variant wins (ADR-047).
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type Shape = { "k": String, "r": Float64 } | { "k": String, "w": Int32 }
val ok = Shape.fromJson({ "k": "circle", "r": 1.5 })
val none = Shape.fromJson({ "k": "x", "z": 9 })
print(if ok["type"] == "error" then "ERR" else "OK ${ok["k"]}")
print(if none["type"] == "error" then "NONE" else "OK")
"#);
    assert_eq!(out, vec!["OK circle", "NONE"]);
}

#[test]
fn test_from_json_recursive_type() {
    // Exercises the descriptor back-edge: a recursive type must terminate.
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type Tree = { "value": Int32, "children": Tree[] }
val ok = Tree.fromJson({ "value": 1, "children": [{ "value": 2, "children": [] }] })
val bad = Tree.fromJson({ "value": 1, "children": [{ "value": "x", "children": [] }] })
print(if ok["type"] == "error" then "ERR" else "OK ${ok["children"][0]["value"]}")
print(if bad["type"] == "error" then "ERR ${bad["path"]}" else "OK")
"#);
    assert_eq!(out, vec!["OK 2", "ERR $.children[0].value"]);
}

#[test]
fn test_from_json_error_value_shape() {
    // A decode Error carries type/message/path.
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type Person = { "name": String, "age": Int32 }
val e = Person.fromJson({ "name": "Bob", "age": "x" })
print("${e["type"]}")
print(if e["message"] == null then "NO_MSG" else "HAS_MSG")
print("${e["path"]}")
"#);
    assert_eq!(out, vec!["error", "HAS_MSG", "$.age"]);
}

#[test]
fn test_from_json_is_error_discriminates() {
    // `is Error` (ADR-047) distinguishes a decode FAILURE from a successfully-decoded value:
    // the Error object carries `"type": "error"`, a decoded Person does not. `is Error`
    // desugars to the value-constrained object pattern `{ "type": "error", .. }`.
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type Person = { "name": String, "age": Int32 }
val good = Person.fromJson({ "name": "Ada", "age": 36 })
val bad = Person.fromJson({ "name": "Bob", "age": "old" })
print(if good is Error then "good:ERR" else "good:OK")
print(if bad is Error then "bad:ERR" else "bad:OK")
"#);
    assert_eq!(out, vec!["good:OK", "bad:ERR"]);
}

#[test]
fn test_from_json_match_is_error_idiom() {
    // The idiom `match result | is Error => .. | is Person => ..`. As of ADR-054 the arm order
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

// Cast-hole closing (ADR-046): Json -> concrete structured object is now a type error.

#[test]
fn test_json_to_concrete_now_errors() {
    // The TWO-STEP form: a Json-typed identifier assigned to a structured concrete object is a
    // type error (ADR-046). NOTE: this form already worked before the headline fix — see
    // test_json_call_result_to_concrete_now_errors for the real call-result hazard.
    let err = run_expect_err(r#"type Person = { "name": String, "age": Int32 }
val j: Json = { "name": "Bob", "age": 30 }
val p: Person = j
"#);
    assert!(
        err.contains("Person") || err.contains("4294967295") || err.to_lowercase().contains("json"),
        "expected a Json->Person type error, got:\n{}",
        err
    );
}

#[test]
fn test_json_call_result_to_concrete_now_errors() {
    // HEADLINE case (ADR-046): the RHS is a *call* whose return type is Json (here the stdlib
    // `readJson`), assigned to a structured concrete object. This must be a type error. Before
    // the fix this type-checked clean because the bidirectional `val` path propagated the
    // expected concrete type down and a zero/Json-param function was misclassified as opaque,
    // freshening its Json return into a permissive inference var.
    let err = run_expect_err(r#"import { readJson } from "std/fs"
type Person = { "name": String, "age": Int32 }
val p: Person = readJson("p.json")
"#);
    assert!(
        err.contains("Person") || err.contains("4294967295") || err.to_lowercase().contains("json"),
        "expected a Json call-result -> Person type error, got:\n{}",
        err
    );
}

#[test]
fn test_json_local_call_result_to_concrete_now_errors() {
    // Same headline hazard with a LOCAL Json-returning function (zero params). The opaque-
    // Function misclassification used to freshen its `Json` return for zero-param functions,
    // letting `val p: Person = getJson()` slip through. Must now error.
    let err = run_expect_err(r#"type Person = { "name": String, "age": Int32 }
val getJson = (): Json => { "name": "Bob", "age": 30 }
val p: Person = getJson()
"#);
    assert!(
        err.contains("Person") || err.contains("4294967295") || err.to_lowercase().contains("json"),
        "expected a local Json call-result -> Person type error, got:\n{}",
        err
    );
}

#[test]
fn test_json_arg_to_concrete_param_errors() {
    // Passing a Json value into a concrete structured-object parameter is rejected (ADR-046).
    let err = run_expect_err(r#"type Person = { "name": String, "age": Int32 }
val greet = (p: Person): String => p["name"]
val j: Json = { "name": "Bob", "age": 30 }
val r = greet(j)
"#);
    assert!(
        err.contains("Person") || err.contains("4294967295") || err.to_lowercase().contains("json"),
        "expected a Json-arg type error, got:\n{}",
        err
    );
}

#[test]
fn test_concrete_to_json_still_ok() {
    // Concrete value -> Json (covariant sink) still compiles.
    let out = run(r#"import { print } from "std/io"
val f = (x: Json): Json => x
val p = { "name": "Bob", "age": 30 }
print("${f(p)["name"]}")
"#);
    assert_eq!(out, vec!["Bob"]);
}

#[test]
fn test_is_narrowing_still_works() {
    // is-narrowing of a Json value into a concrete branch still compiles + runs.
    let out = run(r#"import { print } from "std/io"
val pick = (j: Json): String =>
  if j is String then j else "not-a-string"
print(pick("hi"))
print(pick(42))
"#);
    assert_eq!(out, vec!["hi", "not-a-string"]);
}

#[test]
fn test_is_objecttype_expr_checks_required_fields() {
    // Regression (ADR-054): the EXPRESSION form `x is Person` must check that the object has
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
    // Regression (ADR-054): with required-field checking, `is Person` as the FIRST arm no longer
    // swallows a decode-error object — the ADR-049 ordering footgun is gone. A decode failure
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

// ── `is <ObjectType>` deep type validation (ADR-054) ──────────────────────────

#[test]
fn test_is_objecttype_deep_rejects_wrong_field_type() {
    // ADR-054: `is Person` deep-validates field TYPES, not just presence. A Json value
    // whose `age` is a string (both keys present, WRONG type) must NOT match Person, so the arm
    // falls through to `else` instead of narrowing and operating on the wrong runtime type.
    let out = run(r#"import { print } from "std/io"
type Person = { "name": String, "age": Int32 }
type Box = { "data": Json }
val main = (): Null =>
  val bad: Box = { "data": { "name": "ok", "age": "not-an-int" } }
  val v: Json = bad["data"]
  print(if v is Person then "WRONG-MATCH" else "rejected")
  val good: Box = { "data": { "name": "ok", "age": 5 } }
  val w: Json = good["data"]
  print(if w is Person then "matched" else "WRONG-NO-MATCH")
main()
"#);
    assert_eq!(out, vec!["rejected", "matched"]);
}

#[test]
fn test_is_objecttype_deep_nested() {
    // ADR-053: deep validation recurses into NESTED object fields. A wrong type in a nested field
    // (zip as a string) is rejected; a correct nested value matches.
    let out = run(r#"import { print } from "std/io"
type T = { "addr": { "zip": Int32 } }
type Box = { "data": Json }
val main = (): Null =>
  val bad: Box = { "data": { "addr": { "zip": "oops" } } }
  val v: Json = bad["data"]
  print(if v is T then "WRONG" else "nested-rejected")
  val good: Box = { "data": { "addr": { "zip": 90210 } } }
  val w: Json = good["data"]
  print(if w is T then "nested-matched" else "WRONG")
main()
"#);
    assert_eq!(out, vec!["nested-rejected", "nested-matched"]);
}

#[test]
fn test_is_objecttype_deep_accepts_valid_and_narrows() {
    // ADR-053: a fully well-typed value matches AND the narrowed field access is sound — `v["age"]`
    // is a real Int32, so `v["age"] + 1` produces a correct number (the unsoundness the earlier
    // presence-only rule left open, now folded into ADR-054, is closed).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
type Person = { "name": String, "age": Int32 }
type Box = { "data": Json }
val main = (): Null =>
  val b: Box = { "data": { "name": "Ada", "age": 36 } }
  val v: Json = b["data"]
  if v is Person then print("age+1=${toString(v["age"] + 1)}") else print("no")
main()
"#);
    assert_eq!(out, vec!["age+1=37"]);
}

#[test]
fn test_is_objecttype_deep_number_policy() {
    // ADR-053 inherits fromJson's number policy: a non-integral number fails an Int target;
    // an integral float (5.0) satisfies it.
    let out = run(r#"import { print } from "std/io"
type N = { "n": Int32 }
type Box = { "data": Json }
val main = (): Null =>
  val frac: Box = { "data": { "n": 3.14 } }
  val v: Json = frac["data"]
  print(if v is N then "WRONG-frac" else "frac-rejected")
  val whole: Box = { "data": { "n": 5.0 } }
  val w: Json = whole["data"]
  print(if w is N then "integral-matched" else "WRONG-int")
main()
"#);
    assert_eq!(out, vec!["frac-rejected", "integral-matched"]);
}

#[test]
fn test_is_error_still_discriminates_after_deep() {
    // ADR-053 regression: `is Error` (a value-constrained object pattern, NOT TypeCheckDeep) is
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

// ── singleton string-literal types (ADR-051) ──────────────────────────────────

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
    // A literal-typed value widens to String (ADR-053 rule 2).
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
    // object type `R`, whose `match` has one arm yielding a `Json` value and another yielding a
    // concrete object literal, previously formed `Json | {concrete}` and rejected it against `R`.
    // Each arm is now checked against `R` directly (bidirectional push). Both arms must produce a
    // value indexable as `R` at runtime.
    let out = run(r#"import { print } from "std/io"
type R = { "status": Int32, "headers": Json, "body": String }
val other = (): Json => { "status": 200, "headers": { "a": 1 }, "body": "ok" }
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
type R = { "status": Int32, "headers": Json, "body": String }
val other = (): Json => { "status": 200, "headers": { "a": 1 }, "body": "ok" }
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
    // ADR-049: fromJson validates the exact literal value of a StrLit field, so a tagged-union
    // decode discriminates by the discriminant tag. Correct tags decode to the right variant;
    // first-match-wins probes each variant's KIND_STRLIT check.
    let out = run(r#"import { print } from "std/io"
import { fromJson } from "std/json"
type Result = { "type": "success", "value": Int32 } | { "type": "failure", "error": String }
val show = (j: Json): String =>
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
    // ADR-049: a wrong discriminant value is a decode error (was a silent mis-decode under the
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
    // ADR-049 (KIND_STRLIT) must NOT regress plain String fields: they still encode as KIND_STRING and accept
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
fn test_generic_identity_int_and_string() {
    // The canonical Phase-0 slice: one generic `val` function instantiated at two types
    // in the same module. T=Int32 must run native (no boxing — see the IR-proof test below).
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val identity = <T>(x: T): T => x
print(toString(identity(5)))
print(identity("hello"))
"#);
    assert_eq!(out, vec!["5", "hello"]);
}

#[test]
fn test_generic_identity_three_types_and_reuse() {
    // Generic over a third type (Bool), plus the SAME type used twice (Int32) to exercise
    // specialization de-duplication.
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

    let compile = Command::new(lin_bin())
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

    let compile = Command::new(lin_bin())
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

    let compile = Command::new(lin_bin())
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
    let compile = Command::new(lin_bin())
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

    let compile = Command::new(lin_bin())
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

    let compile = Command::new(lin_bin())
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
    // A Json (wildcard) instantiation of the same combinator stays TAGGED and correct —
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
val xs: Json[] = [1, "two", true]
val ys: Json[] = mymap(xs, (x: Json): Json => x)
print(toString(length(ys)))
print(toString(ys[0]))
print(toString(ys[1]))
"#);
    assert_eq!(out, vec!["3", "1", "two"]);
}

#[test]
fn test_intermediate_alloc_user_annotation_is_respected() {
    // A user-annotated intermediate binding (`val result: Json[] = lin_array_allocate(n)`)
    // must NOT be re-pinned by the refinement — the explicit annotation wins, so the
    // binding stays tagged and the program is correct under the tagged accessor it uses.
    // Guards the `type_ann.is_some()` bail in intermediate_array_allocate_binding.
    let out = run(r#"import { length } from "std/array"
import { for as afor } from "std/iter"
import { print } from "std/io"
import { toString } from "std/string"
val mymap = <T>(arr: T[]): Json[] =>
  val n = length(arr)
  val result: Json[] = lin_array_allocate(n)
  var i = 0
  arr.afor(x =>
    result[i] = x
    i = i + 1
  )
  result
val ys: Json[] = mymap([7, 8, 9])
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

    let compile = Command::new(lin_bin())
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

    let compile = Command::new(lin_bin())
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

    let compile = Command::new(lin_bin())
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
           val result = []\n  \
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
fn test_generic_cross_module_two_instantiations() {
    // Cache/specialization correctness: the SAME imported generic instantiated at two different
    // element types from one importer mints two distinct specializations, each correct.
    let dir = std::env::temp_dir().join(format!("lin_xgen_two_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("helpers.lin"),
        "import { push } from \"std/array\"\n\
         import { for } from \"std/iter\"\n\
         export val mymap = <T, U>(arr: T[], f: (T) => U): U[] =>\n  \
           val result = []\n  \
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
    // GAP 1: a generic `T[]` param unified against a `Json` value binds `T = Json` (the wildcard),
    // monomorphizing to a TAGGED `$Json` instance — NOT leaving `T` unbound (which previously read
    // the array at a bogus element type → null/garbage). The SAME generic applied to a concrete
    // `Int32[]` still specializes to the flat `$Int32` instance. Both must produce correct values.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val firstOf = <T>(arr: T[]): T => arr[0]
val j: Json = [7, 8, 9]
print(toString(firstOf(j)))
val ints: Int32[] = [10, 20, 30]
print(toString(firstOf(ints)))
"#);
    // Json arg → 7 (correct, not null/garbage); Int32 arg → 10 (correct, flat).
    assert_eq!(out, vec!["7", "10"]);
}

#[test]
fn test_generic_t_array_param_json_tagged_int32_flat_in_ir() {
    // IR proof for GAP 1: the Json instantiation mints a TAGGED `firstOf$Json` monomorph (reads via
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
val j: Json = [7, 8, 9]
print(toString(firstOf(j)))
val ints: Int32[] = [10, 20, 30]
print(toString(firstOf(ints)))
"#).unwrap();

    let compile = Command::new(lin_bin())
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

    // The Json instantiation is named `$Json` (tagged), the Int32 one `$Int32` (flat).
    assert!(ll.contains("\"firstOf$Json\""),
        "expected a tagged firstOf$Json monomorph for the Json arg, IR:\n{}", ll);
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
    // `Json` param previously emitted a `$T<id>` garbage monomorph keyed on the UNBOUND TypeVar,
    // which read/allocated the array at a bogus element type → runtime `capacity overflow` / heap
    // corruption. The import-monomorphization path must now erase any non-concrete TypeVar to the
    // Json wildcard, producing a correct tagged `$Json` monomorph (the same resolution the main
    // module uses). Module `helpers` exports `doubleAll(arr: Json)` whose body calls the sibling
    // generic `mymap` on its Json param — exactly the import-path-unbound case.
    let dir = std::env::temp_dir().join(format!("lin_gap2_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(dir.join("helpers.lin"),
        "import { push } from \"std/array\"\n\
         import { for } from \"std/iter\"\n\
         export val mymap = <T, U>(arr: T[], f: (T) => U): U[] =>\n  \
           val result = []\n  \
           arr.for(item => push(result, f(item)))\n  \
           result\n\
         export val doubleAll = (arr: Json): Json =>\n  \
           mymap(arr, x => x * 2)\n").unwrap();
    let main = format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ reduce }} from "std/iter"
import {{ doubleAll }} from "{}/helpers"
val r: Json = doubleAll([5, 6, 7])
print(toString(r.reduce(0, (acc, x) => acc + x)))
"#, dir.to_str().unwrap());
    let output = run(&main);
    let _ = std::fs::remove_dir_all(&dir);
    // 5+6+7 = 18, doubled = 36. Correct tagged result, no crash, no garbage.
    assert_eq!(output, vec!["36"]);
}

#[test]
fn test_generic_import_path_unbound_typevar_no_garbage_monomorph_in_ir() {
    // IR proof for GAP 2: the import-path `mymap` instantiation driven by `doubleAll`'s Json param
    // mints a tagged `mymap$Json_...` monomorph and NEVER a `$T<id>` garbage symbol.
    let id = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let ws = workspace_root();
    let dir = ws.join(format!("target/lin_gap2_ir_{}", id));
    let _ = fs::create_dir_all(&dir);
    fs::write(dir.join("helpers.lin"),
        "import { push } from \"std/array\"\n\
         import { for } from \"std/iter\"\n\
         export val mymap = <T, U>(arr: T[], f: (T) => U): U[] =>\n  \
           val result = []\n  \
           arr.for(item => push(result, f(item)))\n  \
           result\n\
         export val doubleAll = (arr: Json): Json =>\n  \
           mymap(arr, x => x * 2)\n").unwrap();
    let src_path = dir.join("main.lin");
    let bin_path = dir.join("main");
    let ll_path = bin_path.with_extension("ll");
    fs::write(&src_path, format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"
import {{ reduce }} from "std/iter"
import {{ doubleAll }} from "{}/helpers"
val r: Json = doubleAll([5, 6, 7])
print(toString(r.reduce(0, (acc, x) => acc + x)))
"#, dir.to_str().unwrap())).unwrap();

    let compile = Command::new(lin_bin())
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
    // ADR-067: stdlib `at`/`set`/`indexOf` carry generic `<T>(T[], …)` signatures. They must stay
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
    // ADR-069: a `map` callback that RETURNS a closure (curried `i => () => i`) is a FULL
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
    // ADR-069: a `[]`+push builder typed `Int32[]` allocates a TAGGED array; `reduce` over it must
    // read at the runtime representation (tagged), not flat — a flat read would misread garbage.
    // `combinator_read_elem_ty` only flat-reads provably-flat producers; a `[]`+push source falls
    // back to the tagged read.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { push } from "std/array"
import { reduce } from "std/iter"
val build = (): Int32[] =>
  val result = []
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
    // ADR-069: filter's keep/skip block split exercises the `emit_index_loop` phi back-edge patch
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
    // ADR-069 R2 regression: `filter` over an array of OBJECTS pushes each kept element (BORROWED
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
    // Regression: a `for`/`filter`/`map`/`reduce` over a statically-`Json` value whose RUNTIME value
    // is NOT an array (here an Object) must NOT misread the non-array payload as a `LinArray`.
    //
    // The combinator loop used `lin_length_dyn` for its bound (which reports an Object's KEY COUNT)
    // and then blindly unboxed the Json pointer and read it through `lin_array_get_tagged` — so for
    // a 2-key object it ran 2 iterations, dereferencing the `LinObject` as a `LinArray` (UB:
    // "misaligned pointer dereference: address must be a multiple of 0x4 but is 0x41" — a string byte
    // read as an i32 flat-array buffer). This was the docs-builder crash: an `ls()` error object
    // (`{ "type": "error", ... }`) flowed into `allFiles.filter(...)` because the builder's guard
    // checked for "failure" not "error". The fix bounds the combinator loop with `lin_iterable_length`
    // (array length, else 0), so iterating a non-array Json is a clean no-op and the result is empty.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
import { length } from "std/array"
import { filter, map, reduce, for } from "std/iter"
import { contains } from "std/string"
val mkObj = (): Json => { "type": "error", "message": "boom" }
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

// ADR-071: `replace` is a TEST-ONLY mock. In a normal `lin build` program (this harness writes a
// `.lin`, not a `.test.lin`) it must be a hard compile error — a shipped binary must never silently
// swap a real import. The positive cases (mocking user modules + stdlib, internal call-sites seeing
// the mock, spies, val mocks, type-drift rejection) are exercised end-to-end by `lin test` over
// `crates/lin/tests/replace_mocking/*.test.lin` and under ASan in the CI example-suite leg.
#[test]
fn test_replace_rejected_in_non_test_program() {
    let err = run_expect_err(
        r#"import { print } from "std/io"
import { readFile } from "std/fs"
replace readFile = (path: String): Json => "mock"
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

    let out = Command::new(lin_bin())
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

    let out = Command::new(lin_bin())
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
val b: Json = s.collect()
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
val b: Json = s.collect()
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
val pr: Json = s.promise()
val c: Json = s.collect()
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
val c: Json = s.collect()
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
val reuse: Json = b.collect()
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
val reuse: Json = a.collect()
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
    let out = Command::new(lin_bin())
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

// ── `Number` as a numerically-bounded generic parameter (ADR-018, reversed) ─────────────────────
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
    // Mixed numeric families in ONE call of a `Number`-returning function are SUPPORTED (ADR-018,
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
    // Nested `Number` (ADR-018, reversed, bug #4): `Number[]` and a `Number` callback over it.
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
    // ADR-018 (reversed) §Json: a `Json` value is ACCEPTED at a `Number` parameter — consistent
    // with the `Json → Int32` scalar coercion gap (ADR-048), monomorphizing to the default `Int32`
    // family with an unchecked unbox. This was previously INCONSISTENT: a DIRECT `Json`
    // (`val x: Json = 42`, the bare `TypeVar(u32::MAX)` marker) was REJECTED while a `Json`
    // PROJECTION (`config["count"]`, a fresh inference var) slipped past the bound guard and ran.
    // BOTH forms must now compile AND produce the SAME runtime answer (`isEven$Json` unboxes the
    // Json as Int32 and `srem`s — byte-identical specializations). 42 is even ⇒ `true` for both.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val isEven = (x: Number) => x % 2 == 0
val direct: Json = 42
val config: Json = { "count": 42 }
print(toString(isEven(direct)))
print(toString(isEven(config["count"])))
"#);
    assert_eq!(out, vec!["true", "true"]);
}

#[test]
fn test_number_json_arg_arithmetic_returns_right_number() {
    // A Json-int through a `Number` param USED IN ARITHMETIC (a Number-returning body, not just a
    // Bool predicate) must monomorphize to `triple$Json` (param unboxed Int32, native `mul i32`),
    // box the scalar result back to the Json the surrounding `toString` expects, and return the
    // RIGHT number. Both the direct `Json` binding and the `config[...]` projection of 14 ⇒ 42.
    let out = run(r#"import { print } from "std/io"
import { toString } from "std/string"
val triple = (x: Number) => x * 3
val direct: Json = 14
val config: Json = { "count": 14 }
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

    let compile = Command::new(lin_bin())
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
// optimisation MUST be behaviour-preserving and MUST NOT weaken the `:Json`
// (untrusted-shape) case, which still needs full recursive validation.
// -------------------------------------------------------------------------

#[test]
fn test_union_discrim_strlit_closed_concrete() {
    // Closed concrete union discriminated by a StrLit field VALUE. Both arms must
    // select the correct variant and the narrowed binding must read the right field.
    let out = run(r#"
import { print } from "std/io"
type Ok = { "type": "ok", "value": Int32 }
type Err = { "type": "err", "msg": String }
type Res = Ok | Err

val describe = (r: Res): String =>
  match r
    is Ok => "ok=${r["value"]}"
    is Err => "err=${r["msg"]}"

val a: Res = { "type": "ok", "value": 42 }
val b: Res = { "type": "err", "msg": "boom" }
print(describe(a))
print(describe(b))
"#);
    assert_eq!(out, vec!["ok=42", "err=boom"]);
}

#[test]
fn test_union_discrim_strlit_three_variants() {
    // Three-variant StrLit-discriminated closed concrete union: each `is` arm must
    // be distinguished from BOTH siblings by its distinct StrLit discriminant.
    let out = run(r#"
import { print } from "std/io"
type A = { "tag": "a", "x": Int32 }
type B = { "tag": "b", "y": String }
type C = { "tag": "c", "z": Boolean }
type ABC = A | B | C

val f = (v: ABC): String =>
  match v
    is A => "A:${v["x"]}"
    is B => "B:${v["y"]}"
    is C => "C:${v["z"]}"

val a: ABC = { "tag": "a", "x": 7 }
val b: ABC = { "tag": "b", "y": "hi" }
val c: ABC = { "tag": "c", "z": true }
print(f(a))
print(f(b))
print(f(c))
"#);
    assert_eq!(out, vec!["A:7", "B:hi", "C:true"]);
}

#[test]
fn test_union_discrim_presence_only_falls_back_but_correct() {
    // Closed concrete union whose variants are disjoint ONLY by field PRESENCE
    // (the base-String `kind`, the calc AST `Num | BinOp` shape). Field presence is
    // UNSOUND under structural width-subtyping, so this case FALLS BACK to the full
    // recursive `MatchesSchema` — it must still match correctly.
    let out = run(r#"
import { print } from "std/io"
type Num = { "kind": String, "value": Int32 }
type BinOp = { "kind": String, "op": String, "left": Int32, "right": Int32 }
type Ast = Num | BinOp

val eval = (n: Ast): Int32 =>
  match n
    is Num => n["value"]
    is BinOp => n["left"] + n["right"]

val a: Ast = { "kind": "num", "value": 5 }
val b: Ast = { "kind": "binop", "op": "+", "left": 3, "right": 4 }
print(eval(a))
print(eval(b))
"#);
    assert_eq!(out, vec!["5", "7"]);
}

#[test]
fn test_union_discrim_json_scrutinee_full_validation() {
    // A `:Json` scrutinee against the SAME variant types MUST keep full recursive
    // validation: extra-field values still match, but a value with the right
    // discriminant and a WRONG field TYPE must NOT match that variant (it is the
    // recursive `MatchesSchema` that catches this — the fast path is not used here).
    let out = run(r#"
import { print } from "std/io"
type Ok = { "type": "ok", "value": Int32 }
type Err = { "type": "err", "msg": String }

val classify = (r: Json): String =>
  match r
    is Ok => "ok"
    is Err => "err"
    else => "neither"

// well-formed
print(classify({ "type": "ok", "value": 42 }))
// well-formed with an EXTRA field — width subtyping, still an Ok
print(classify({ "type": "ok", "value": 42, "extra": 1 }))
// right discriminant, WRONG field type — full validation must reject -> neither
print(classify({ "type": "ok", "value": "wrong" }))
// missing required field -> neither
print(classify({ "type": "ok" }))
// completely wrong shape -> neither
print(classify({ "random": 1 }))
print(classify({ "type": "err", "msg": "boom" }))
"#);
    assert_eq!(out, vec!["ok", "ok", "neither", "neither", "neither", "err"]);
}

#[test]
fn test_union_discrim_standalone_is_expr() {
    // The fast path also applies to a standalone `is` boolean expression (not just
    // match arms) over a closed concrete union scrutinee.
    let out = run(r#"
import { print } from "std/io"
type Ok = { "type": "ok", "value": Int32 }
type Err = { "type": "err", "msg": String }
type Res = Ok | Err

val label = (r: Res): String => if r is Ok then "yes" else "no"

val a: Res = { "type": "ok", "value": 1 }
val b: Res = { "type": "err", "msg": "x" }
print(label(a))
print(label(b))
"#);
    assert_eq!(out, vec!["yes", "no"]);
}

#[test]
fn test_union_discrim_nullable_union() {
    // A closed concrete union WITH a Null member: the Null is stripped for the
    // discriminator analysis, the object variants still discriminate by StrLit.
    let out = run(r#"
import { print } from "std/io"
type Ok = { "type": "ok", "value": Int32 }
type Err = { "type": "err", "msg": String }
type MaybeRes = Ok | Err | Null

val describe = (r: MaybeRes): String =>
  match r
    is Ok => "ok=${r["value"]}"
    is Err => "err"
    else => "null"

val a: MaybeRes = { "type": "ok", "value": 9 }
print(describe(a))
print(describe(null))
"#);
    assert_eq!(out, vec!["ok=9", "null"]);
}

// Stage 0.5 sealed-records run-equivalence: a NAMED record type now carries an (inert) `sealed`
// marker through resolution, while anonymous object literals do not. This test proves the marker
// is BEHAVIOR-INERT: a named-typed value and a structurally-equal anonymous literal still
// inter-operate exactly as before — assign across in BOTH directions, pass into the named param
// position, read fields, and compare equal (including a WIDER literal with an extra field, which
// structural compatibility still permits). See SEALED_RECORDS_DESIGN.md §5 (Stage 0.5).
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

// ───────────────────────── Sealed records — Stage 1 ─────────────────────────
// Unboxed packed-struct layout + constant-offset field access for sealed all-scalar record
// types. See SEALED_RECORDS_DESIGN.md §3 (semantics matrix) and §5 (Stage 1).

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
fn test_sealed_boundary_projection_drops_extras_source_untouched() {
    // (b) A wider Json/anonymous literal with an EXTRA field passed to a sealed-scalar param: the
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

#[test]
fn test_sealed_to_json_roundtrip_prints() {
    // (c) A sealed value flowing into a Json slot materializes a boxed object that prints/serializes
    // correctly (sealed → Json boundary).
    let out = run(r#"
import { print } from "std/io"
import { toString } from "std/string"
type Pair = { "lo": Int32, "hi": Int32 }
val p: Pair = { "lo": 7, "hi": 42 }
val j: Json = p
print(toString(j))
print("${j["lo"]} ${j["hi"]}")
"#);
    assert_eq!(out, vec![r#"{"lo": 7, "hi": 42}"#, "7 42"]);
}

#[test]
fn test_sealed_eq_same_shape_as_json_is_true() {
    // (d) Equality is order-independent and crosses representations: a sealed value equals a
    // same-shape boxed Json/anonymous value, and two sealed values of the same type compare
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
val describe = (c: Json): String =>
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
    // (f) REGRESSION: a named record with a NON-scalar (String) field is NOT a sealed-scalar
    // record — it keeps the boxed LinObject path and behaves exactly as before. An anonymous
    // all-scalar literal (unsealed) likewise stays boxed (it is never struct-laid-out).
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
