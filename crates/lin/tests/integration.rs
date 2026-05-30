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
val notReady = ready == false
print(toString(notReady))
val also = false == false
print(toString(also))
"#);
    assert_eq!(output, vec!["false", "true"]);
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
    let tmp = std::env::temp_dir().join("lin_ctest_rw.txt");
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
    let tmp = std::env::temp_dir().join("lin_ctest_append.txt");
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
    let tmp = std::env::temp_dir().join("lin_ctest_exists.txt");
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
    let tmp = std::env::temp_dir().join("lin_ctest_lines.txt");
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
    let tmp = std::env::temp_dir().join("lin_ctest_json.json");
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
    let tmp = std::env::temp_dir().join("lin_ctest_isfile.txt");
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
    let tmp = std::env::temp_dir().join("lin_ctest_stat.txt");
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
    let tmp_dir = std::env::temp_dir().join("lin_ctest_listdir");
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
    let tmp_dir = std::env::temp_dir().join("lin_ctest_mkdir");
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
    let tmp_dir = std::env::temp_dir().join("lin_ctest_mkdirall").join("a").join("b");
    let _ = fs::remove_dir_all(std::env::temp_dir().join("lin_ctest_mkdirall"));
    let dir_path = tmp_dir.display().to_string();
    let output = run(&format!(r#"import {{ print }} from "std/io"
import {{ toString }} from "std/string"

import {{ mkdir, isDir }} from "std/fs"
mkdir("{dir_path}", {{ "parents": true }})
print(toString(isDir("{dir_path}")))
"#));
    let _ = fs::remove_dir_all(std::env::temp_dir().join("lin_ctest_mkdirall"));
    assert_eq!(output, vec!["true"]);
}

#[test]
fn test_fs_delete_file() {
    let tmp = std::env::temp_dir().join("lin_ctest_deletefile.txt");
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
    let src = std::env::temp_dir().join("lin_ctest_rename_src.txt");
    let dst = std::env::temp_dir().join("lin_ctest_rename_dst.txt");
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
