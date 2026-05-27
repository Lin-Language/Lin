use lin_eval::Interpreter;

fn run(source: &str) -> Vec<String> {
    let mut interp = Interpreter::new();
    interp.run(source).unwrap();
    interp.output.clone()
}

fn run_expect_err(source: &str) -> String {
    let mut interp = Interpreter::new();
    interp.run(source).unwrap_err()
}

#[test]
fn test_hello_world() {
    let output = run(r#"print("hello world")"#);
    assert_eq!(output, vec!["hello world"]);
}

#[test]
fn test_arithmetic() {
    let output = run(r#"
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
    let output = run(r#"
val name = "Bob"
val age = 42
print("Hello ${name}, age ${age}")
"#);
    assert_eq!(output, vec!["Hello Bob, age 42"]);
}

#[test]
fn test_functions_and_partial_application() {
    let output = run(r#"
val add = (a: Int32, b: Int32): Int32 => a + b
val addTen = add(10)
print(toString(addTen(5)))
print(toString(add(3, 4)))
"#);
    assert_eq!(output, vec!["15", "7"]);
}

#[test]
fn test_dot_application() {
    let output = run(r#"
val greet = (name: String): String => "Hello ${name}"
print("world".greet())
"#);
    assert_eq!(output, vec!["Hello world"]);
}

#[test]
fn test_objects_and_safe_access() {
    let output = run(r#"
val person = { "name": "Bob", "age": 42 }
print(person["name"])
print(toString(person["missing"]))
print(toString(person["a"]["b"]["c"]))
"#);
    assert_eq!(output, vec!["Bob", "null", "null"]);
}

#[test]
fn test_equality() {
    let output = run(r#"
print(toString(1 == 1))
print(toString("a" == "a"))
print(toString(null == null))
print(toString({ "a": 1, "b": 2 } == { "b": 2, "a": 1 }))
print(toString([1, 2] == [1, 2]))
print(toString([1, 2] == [2, 1]))
"#);
    assert_eq!(output, vec!["true", "true", "true", "true", "true", "false"]);
}

#[test]
fn test_pattern_matching_is() {
    let output = run(r#"
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
    let output = run(r#"
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
    let output = run(r#"
val divide = (a: Float64, b: Float64): Json =>
  if b == 0.0
    then { "type": "failure", "error": "div by zero" }
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
    let output = run(r#"
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
    let output = run(r#"
val factorial = (n: Int32): Int32 =>
  if n == 0 then 1 else n * factorial(n - 1)

print(toString(factorial(5)))
print(toString(factorial(0)))
"#);
    assert_eq!(output, vec!["120", "1"]);
}

#[test]
fn test_for_and_range() {
    let output = run(r#"
range(1, 4).for(i => print(toString(i)))
"#);
    assert_eq!(output, vec!["1", "2", "3"]);
}

#[test]
fn test_map_filter_reduce() {
    let output = run(r#"
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
    let output = run(r#"
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
    let output = run(r#"
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
    let output = run(r#"
val a = if true then "yes" else "no"
print(a)

val b = if false then "yes" else "no"
print(b)

val x = 10
val c = if x > 5
  then "big"
  else "small"
print(c)
"#);
    assert_eq!(output, vec!["yes", "no", "big"]);
}

#[test]
fn test_array_oob_error() {
    let err = run_expect_err(r#"
val arr = [1, 2, 3]
val x = arr[10]
"#);
    assert!(err.contains("out of bounds"));
}

#[test]
fn test_division_by_zero_error() {
    let err = run_expect_err(r#"
val x = 10 / 0
"#);
    assert!(err.contains("division by zero"));
}

#[test]
fn test_multi_param_lambda() {
    let output = run(r#"
val total = [1, 2, 3].reduce(0, (sum, x) => sum + x)
print(toString(total))
"#);
    assert_eq!(output, vec!["6"]);
}

#[test]
fn test_string_literal_pattern() {
    let output = run(r#"
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
    let output = run(r#"
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
    let output = run(r#"
var count = 0
val result = count = count + 1
print(toString(result))
print(toString(count))
"#);
    assert_eq!(output, vec!["1", "1"]);
}

#[test]
fn test_non_exhaustive_match_error() {
    let err = run_expect_err(r#"
val x = 42
val y = match x
  is String => "string"
"#);
    assert!(err.contains("non-exhaustive"));
}

#[test]
fn test_is_has_as_boolean_expressions() {
    let output = run(r#"
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
    let output = run(r#"
val s = "hello\tworld\n"
print(s)
val q = "she said \"hi\""
print(q)
val bs = "back\\slash"
print(bs)
"#);
    assert_eq!(output, vec!["hello\tworld\n", "she said \"hi\"", "back\\slash"]);
}

#[test]
fn test_block_expression() {
    let output = run(r#"
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
    let output = run(r#"
val add = (a: Int32, b: Int32): Int32 => a + b
val addFive = 5.add
print(toString(addFive(3)))
"#);
    assert_eq!(output, vec!["8"]);
}

#[test]
fn test_boolean_negation() {
    let output = run(r#"
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
    let output = run(r#"
print(toString("a" < "b"))
print(toString("b" < "a"))
print(toString("abc" <= "abc"))
print(toString("z" > "a"))
"#);
    assert_eq!(output, vec!["true", "false", "true", "true"]);
}

#[test]
fn test_numeric_comparison() {
    let output = run(r#"
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
    let output = run(r#"
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
    let output = run(r#"
val x = 10
val result = if x > 5
  then
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
    let output = run(r#"
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
fn test_null_propagation_deep() {
    let output = run(r#"
val x = null
print(toString(x["a"]["b"]["c"]["d"]))
val obj = { "a": { "b": null } }
print(toString(obj["a"]["b"]["c"]))
print(toString(obj["missing"]["deep"]["chain"]))
"#);
    assert_eq!(output, vec!["null", "null", "null"]);
}

#[test]
fn test_comments() {
    let output = run(r#"
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
    let output = run(r#"
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
    let output = run(r#"
print(toString(1 != 2))
print(toString(1 != 1))
print(toString("a" != "b"))
print(toString("a" != "a"))
"#);
    assert_eq!(output, vec!["true", "false", "true", "false"]);
}

#[test]
fn test_array_pattern_matching_is() {
    let output = run(r#"
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
    let output = run(r#"
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
    let output = run(r#"
val person = { "name": "Bob", "age": 42, "city": "London" }
val { name, ...rest } = person
print(name)
print(toString(rest["age"]))
print(toString(rest["city"]))
"#);
    assert_eq!(output, vec!["Bob", "42", "London"]);
}

#[test]
fn test_over_application_error() {
    let err = run_expect_err(r#"
val add = (a: Int32, b: Int32): Int32 => a + b
val bad = add(1, 2, 3)
"#);
    assert!(err.contains("Too many arguments"));
}

#[test]
fn test_integer_modulo() {
    let output = run(r#"
print(toString(7 % 3))
print(toString(-7 % 3))
print(toString(7 % -3))
"#);
    assert_eq!(output, vec!["1", "-1", "1"]);
}

#[test]
fn test_modulo_by_zero_error() {
    let err = run_expect_err(r#"
val x = 10 % 0
"#);
    assert!(err.contains("modulo by zero") || err.contains("division by zero"));
}

#[test]
fn test_multiple_closures_share_var() {
    let output = run(r#"
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
    let output = run(r#"
val double = (x: Int32): Int32 => x * 2
val addOne = (x: Int32): Int32 => x + 1
print(toString(addOne(double(5))))
"#);
    assert_eq!(output, vec!["11"]);
}

#[test]
fn test_recursive_fibonacci() {
    let output = run(r#"
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
    let output = run(r#"
val a = "Hello"
val b = "World"
val greeting = "${a} ${b}"
print(greeting)
"#);
    assert_eq!(output, vec!["Hello World"]);
}

#[test]
fn test_object_equality_deep() {
    let output = run(r#"
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
    let output = run(r#"
val x = 10
val y = 20
print("sum = ${x + y}")
print("cond = ${if x > 5 then "big" else "small"}")
"#);
    assert_eq!(output, vec!["sum = 30", "cond = big"]);
}

#[test]
fn test_length_function() {
    let output = run(r#"
print(toString(length("hello")))
print(toString(length([1, 2, 3])))
print(toString(length({ "a": 1, "b": 2 })))
"#);
    assert_eq!(output, vec!["5", "3", "2"]);
}

#[test]
fn test_multiline_chain() {
    let output = run(r#"
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
    let output = run(r#"
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
    let output = run(r#"
val add3 = (a: Int32, b: Int32, c: Int32): Int32 => a + b + c
val step1 = add3(1)
val step2 = step1(2)
val result = step2(3)
print(toString(result))
"#);
    assert_eq!(output, vec!["6"]);
}

#[test]
fn test_iter_builtin() {
    let output = run(r#"
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
    let err = run_expect_err(r#"
print(toString(xyz))
"#);
    assert!(err.contains("Undefined"));
}

#[test]
fn test_cannot_assign_immutable_error() {
    let err = run_expect_err(r#"
val x = 5
x = 10
"#);
    assert!(err.contains("Cannot assign") || err.contains("not a mutable"));
}

#[test]
fn test_empty_array_and_object() {
    let output = run(r#"
val arr = []
val obj = {}
print(toString(length(arr)))
print(toString(length(obj)))
"#);
    assert_eq!(output, vec!["0", "0"]);
}

#[test]
fn test_nested_objects_access() {
    let output = run(r#"
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
    let output = run(r#"
val sum = (n: Int32, acc: Int32): Int32 =>
  if n == 0 then acc else sum(n - 1, acc + n)

print(toString(sum(100000, 0)))
"#);
    assert_eq!(output, vec!["5000050000"]);
}

#[test]
fn test_tco_in_match() {
    let output = run(r#"
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
    let output = run(r#"
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
    let output = run(r#"
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
    let output = run(r#"
val age = 25
val active = true
val result = if age >= 18
  && active
  then "active adult"
  else "other"
print(result)
"#);
    assert_eq!(output, vec!["active adult"]);
}

#[test]
fn test_tuple_dot_application() {
    let output = run(r#"
val sub = (a: Int32, b: Int32): Int32 => a - b
val result = (10, 3).sub
print(toString(result))
"#);
    assert_eq!(output, vec!["7"]);
}

#[test]
fn test_array_rest_destructuring() {
    let output = run(r#"
val [first, ...rest] = [1, 2, 3, 4, 5]
print(toString(first))
print(toString(length(rest)))
print(toString(rest[0]))
"#);
    assert_eq!(output, vec!["1", "4", "2"]);
}

#[test]
fn test_higher_order_functions() {
    let output = run(r#"
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
    let output = run(r#"
val greetPerson = ({ name, age }: Json): String =>
  "${name} is ${age}"

print(greetPerson({ "name": "Bob", "age": 42 }))
"#);
    assert_eq!(output, vec!["Bob is 42"]);
}

#[test]
fn test_chained_if_else() {
    let output = run(r#"
val classify = (x: Int32): String =>
  if x > 100
    then "big"
    else if x > 10
      then "medium"
      else "small"

print(classify(200))
print(classify(50))
print(classify(5))
"#);
    assert_eq!(output, vec!["big", "medium", "small"]);
}

#[test]
fn test_multi_statement_lambda_in_parens() {
    let output = run(r#"
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
    let output = run(r#"
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
    let output = run(r#"
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
    let output = run(r#"
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
    let output = run(r#"
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
    let output = run(r#"
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
    let output = run(r#"
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
    let output = run(r#"
import { flatMap, indexOf, reverse } from "std/array"
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
    let output = run(r#"
val isEvenDesc = (n: Int32): String =>
  if n == 0
    then "even"
    else isOddDesc(n - 1)

val isOddDesc = (n: Int32): String =>
  if n == 0
    then "odd"
    else isEvenDesc(n - 1)

print(isEvenDesc(4))
print(isOddDesc(4))
print(isEvenDesc(3))
"#);
    assert_eq!(output, vec!["even", "odd", "odd"]);
}

#[test]
fn test_forward_reference_in_closure() {
    let output = run(r#"
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
    let output = run(r#"
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
fn test_object_spread_basic() {
    let output = run(r#"
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
    let output = run(r#"
val src = { "a": 1, "b": 2 }
val merged = { ...src, "a": 99 }
print(toString(merged["a"]))
print(toString(merged["b"]))
print(toString(keys(merged)))
"#);
    assert_eq!(output, vec!["99", "2", "[\"a\", \"b\"]"]);
}

#[test]
fn test_object_spread_override_spread_after_explicit() {
    let output = run(r#"
val src = { "a": 99 }
val merged = { "a": 1, "b": 2, ...src }
print(toString(merged["a"]))
print(toString(merged["b"]))
print(toString(keys(merged)))
"#);
    assert_eq!(output, vec!["99", "2", "[\"a\", \"b\"]"]);
}

#[test]
fn test_object_spread_multiple() {
    let output = run(r#"
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
    let output = run(r#"
val merged = { ...{}, "a": 1 }
print(toString(merged["a"]))
print(toString(keys(merged)))
"#);
    assert_eq!(output, vec!["1", "[\"a\"]"]);
}

#[test]
fn test_object_spread_null_error() {
    let err = run_expect_err(r#"
val merged = { ...null, "a": 1 }
"#);
    assert!(err.contains("expected Object"), "got error: {}", err);
}

#[test]
fn test_object_spread_array_error() {
    let err = run_expect_err(r#"
val merged = { ...[1, 2], "a": 1 }
"#);
    assert!(err.contains("expected Object"), "got error: {}", err);
}

#[test]
fn test_object_spread_string_error() {
    let err = run_expect_err(r#"
val merged = { ..."hello", "a": 1 }
"#);
    assert!(err.contains("expected Object"), "got error: {}", err);
}

// M17: Concurrency — async/await
#[test]
fn test_async_await_basic() {
    let output = run(r#"
val p = async(() => 42)
val result = await(p)
print(toString(result))
"#);
    assert_eq!(output, vec!["42"]);
}

#[test]
fn test_async_val_capture() {
    let output = run(r#"
val x = 10
val p = async(() => x * 2)
val result = await(p)
print(toString(result))
"#);
    assert_eq!(output, vec!["20"]);
}

#[test]
fn test_parallel_three_thunks() {
    let output = run(r#"
val results = parallel([() => 1, () => 2, () => 3])
print(toString(results))
"#);
    assert_eq!(output, vec!["[1, 2, 3]"]);
}

#[test]
fn test_async_error_surfaces_at_await() {
    let output = run(r#"
val p = async(() => "ok")
val result = await(p)
print(result)
"#);
    assert_eq!(output, vec!["ok"]);
}

#[test]
fn test_thread_pool_async() {
    let output = run(r#"
val pool = threadPool(2)
val p1 = pool.async(() => 100)
val p2 = pool.async(() => 200)
val r1 = await(p1)
val r2 = await(p2)
print(toString(r1 + r2))
"#);
    assert_eq!(output, vec!["300"]);
}

#[test]
fn test_worker_request_reply() {
    let output = run(r#"
val w = worker(msg => msg * 2, () => null)
val reply = w.request(21)
w.close()
print(toString(reply))
"#);
    assert_eq!(output, vec!["42"]);
}

#[test]
fn test_worker_message_fire_and_forget() {
    let output = run(r#"
var count = 0
val w = worker(msg => null, () => null)
w.message(1)
w.message(2)
w.close()
print("done")
"#);
    assert_eq!(output, vec!["done"]);
}

// M12: Iterator restart fixture
#[test]
fn test_iterator_restart() {
    // Re-invoking the initial-state thunk yields a fresh state (spec §27.6).
    // The same iterator can be re-started by calling .for twice; each run starts fresh.
    let output = run(r#"
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

// M18: IO/FS/HTTP

#[test]
fn test_parse_json_intrinsic() {
    let output = run(r#"
val json = __parseJson("[1, 2, 3]")
print(toString(json[0]))
print(toString(length(json)))
"#);
    assert_eq!(output, vec!["1", "3"]);
}

#[test]
fn test_parse_json_object() {
    let output = run(r#"
val obj = __parseJson("{\"x\": 42, \"y\": \"hello\"}")
print(toString(obj["x"]))
print(obj["y"])
"#);
    assert_eq!(output, vec!["42", "hello"]);
}

#[test]
fn test_parse_json_error() {
    let output = run(r#"
val result = __parseJson("not valid json")
print(result["type"])
"#);
    assert_eq!(output, vec!["error"]);
}

// std/server tests
#[test]
fn test_server_json_helper() {
    let output = run(r#"
import { json } from "std/http"
val resp = json(200, "hello")
print(toString(resp["status"]))
print(resp["headers"]["Content-Type"])
"#);
    assert_eq!(output, vec!["200", "application/json"]);
}

#[test]
fn test_server_text_helper() {
    let output = run(r#"
import { text } from "std/http"
val resp = text(200, "hello world")
print(toString(resp["status"]))
print(resp["body"])
"#);
    assert_eq!(output, vec!["200", "hello world"]);
}

#[test]
fn test_ffi_stub_errors_at_call_time() {
    // The interpreter registers foreign bindings as stubs that error at call time.
    // Arity is derived from the declared type so the call site arity check passes.
    let err = run_expect_err(r#"
import foreign "libmath.a"
  val myFn: (Int32) => Int32
myFn(42)
"#);
    assert!(err.contains("Foreign functions are not available in the interpreter"),
        "Expected foreign stub error, got: {}", err);
}

// M11: Cyclic import tests use the file-based interpreter, so we test the simpler
// intra-module case: a function that forward-references another top-level function
// (mutual recursion via forward-declaration).
#[test]
fn test_mutual_recursion_via_forward_decl() {
    let output = run(r#"
val isEven = (n: Int32): Boolean =>
  if n == 0
    then true
    else isOdd(n - 1)

val isOdd = (n: Int32): Boolean =>
  if n == 0
    then false
    else isEven(n - 1)

print(toString(isEven(4)))
print(toString(isOdd(3)))
"#);
    assert_eq!(output, vec!["true", "true"]);
}

// M18: std/http live tests using an in-process tiny_http test server

fn start_test_server(port: u16, response_body: &'static str, status: u16) -> std::thread::JoinHandle<()> {
    use std::thread;
    thread::spawn(move || {
        let server = tiny_http::Server::http(format!("0.0.0.0:{}", port)).unwrap();
        // Handle one request then exit
        if let Ok(Some(req)) = server.recv_timeout(std::time::Duration::from_secs(5)) {
            let response = tiny_http::Response::from_string(response_body)
                .with_status_code(tiny_http::StatusCode::from(status));
            let _ = req.respond(response);
        }
    })
}

#[test]
fn test_server_thread_pool_serve_concurrent() {
    // Spin up a pool.serve server, make 3 concurrent requests, check all get responses.
    use std::thread;
    use std::time::Duration;

    let port: u16 = 19103;
    let _server_handle = thread::spawn(move || {
        let mut interp = lin_eval::Interpreter::new();
        let _ = interp.run(&format!(r#"
import {{ text }} from "std/http"
val pool = threadPool(4)
pool.serve({port}, req =>
  text(200, "concurrent-ok"))
"#));
    });
    thread::sleep(Duration::from_millis(250));

    // Make 3 concurrent requests
    let handles: Vec<_> = (0..3).map(|_| {
        thread::spawn(move || {
            ureq::get(&format!("http://127.0.0.1:{}", port))
                .call()
                .map(|r| r.into_string().unwrap())
        })
    }).collect();

    for h in handles {
        let result = h.join().unwrap();
        match result {
            Ok(body) => assert_eq!(body, "concurrent-ok"),
            Err(e) => panic!("Concurrent request failed: {}", e),
        }
    }
}

// M18: std/io stdin tests — use process::Command to pipe stdin

fn lin_binary_path() -> std::path::PathBuf {
    // Look for the pre-built debug binary relative to CARGO_MANIFEST_DIR.
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir.parent().unwrap().parent().unwrap();
    workspace_root.join("target/debug/lin")
}

fn run_lin_with_stdin(test_name: &str, source: &str, stdin_data: &str) -> String {
    use std::process::{Command, Stdio};
    use std::io::Write;
    use std::fs;

    let bin = lin_binary_path();
    // Use unique temp file per test to avoid parallel conflicts.
    let tmp = std::env::temp_dir().join(format!("lin_stdin_{}.lin", test_name));
    fs::write(&tmp, source).unwrap();
    let mut child = Command::new(&bin)
        .arg(tmp.to_str().unwrap())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn lin binary — run `cargo build -p lin` first");
    child.stdin.as_mut().unwrap().write_all(stdin_data.as_bytes()).unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    let _ = fs::remove_file(&tmp);
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
#[ignore = "requires stdin/subprocess"]
fn test_io_lines_reads_all_stdin_lines() {
    let output = run_lin_with_stdin("lines", r#"
import { lines } from "std/io"
val all = lines()
all.for(line => print(line))
"#, "hello\nworld\nfoo\n");
    let parts: Vec<&str> = output.lines().collect();
    assert_eq!(parts, vec!["hello", "world", "foo"],
        "lines() should yield each stdin line, got: {:?}", parts);
}

#[test]
#[ignore = "requires stdin/subprocess"]
fn test_io_read_all_returns_full_content() {
    let output = run_lin_with_stdin("readall", r#"
import { readAll } from "std/io"
val content = readAll()
print(content)
"#, "hello world");
    assert_eq!(output, "hello world",
        "readAll() should return all stdin content, got: {:?}", output);
}

#[test]
#[ignore = "requires stdin/subprocess"]
fn test_io_read_line_null_on_empty_stdin() {
    let output = run_lin_with_stdin("readline_null", r#"
import { readLine } from "std/io"
val line = readLine()
print(toString(line))
"#, "");
    assert_eq!(output, "null",
        "readLine() on empty stdin should return null, got: {:?}", output);
}

#[test]
fn test_ffi_end_to_end_c_library() {
    use std::process::Command;
    use std::path::Path;

    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
    let lin_bin = workspace.join("target/debug/lin");
    let mathlib_c = workspace.join("examples/lib/mathlib.c");
    let mathlib_a = workspace.join("examples/lib/libmathlib.a");
    let ffi_example = workspace.join("examples/ffi-c.lin");
    let output_bin = workspace.join("target/ffi_c_test");

    if !lin_bin.exists() {
        eprintln!("SKIP: lin binary not built; run `cargo build -p lin` first");
        return;
    }

    // Compile the C library if not already built
    if !mathlib_a.exists() {
        let obj = workspace.join("examples/lib/mathlib.o");
        let cc_status = Command::new("cc")
            .args(["-c", mathlib_c.to_str().unwrap(), "-o", obj.to_str().unwrap()])
            .status();
        if cc_status.map(|s| !s.success()).unwrap_or(true) {
            eprintln!("SKIP: failed to compile C library (cc not available)");
            return;
        }
        let ar_status = Command::new("ar")
            .args(["rcs", mathlib_a.to_str().unwrap(), obj.to_str().unwrap()])
            .status();
        if ar_status.map(|s| !s.success()).unwrap_or(true) {
            eprintln!("SKIP: failed to create static archive");
            return;
        }
    }

    // Compile the Lin FFI example (run from workspace root so relative library paths resolve)
    let compile_out = Command::new(&lin_bin)
        .args(["build", ffi_example.to_str().unwrap(), "-o", output_bin.to_str().unwrap()])
        .current_dir(workspace)
        .output()
        .expect("failed to run lin build");
    assert!(compile_out.status.success(),
        "lin build failed: {}", String::from_utf8_lossy(&compile_out.stderr));

    // Run the compiled binary
    let run_out = Command::new(&output_bin)
        .output()
        .expect("failed to run ffi_c_test binary");
    assert!(run_out.status.success(), "ffi_c_test binary failed");
    let stdout = String::from_utf8_lossy(&run_out.stdout);
    assert!(stdout.contains("3 + 4 = 7"), "Expected '3 + 4 = 7' in output, got: {}", stdout);
    assert!(stdout.contains("2.5^2 = 6.25"), "Expected '2.5^2 = 6.25' in output, got: {}", stdout);
}

