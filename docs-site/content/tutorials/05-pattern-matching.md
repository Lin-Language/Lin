# Pattern Matching

Pattern matching is the primary way to inspect union types, destructure JSON-shaped data, and make exhaustive decisions in Lin.

## Basic match

```lin
import { print } from "std/io"

val describe = (x: String | Int32 | Null): String =>
  match x
    is Null   => "nothing"
    is Int32  => "a number: ${x}"
    is String => "a string: ${x}"

print(describe(null))    // nothing
print(describe(42))      // a number: 42
print(describe("hi"))    // a string: hi
```

`match` evaluates to the value of the matched arm. Every `match` must be exhaustive — if no arm matches and there is no `else`, it is a runtime error.

## `is` vs `has` — the key distinction

This is the most important thing to understand about Lin's patterns:

- **`is` validates types deeply and recursively.** `value is Person` checks that the value is an object **and** that every declared field of `Person` is present **and** correctly typed, recursing into nested objects and arrays. Extra fields are allowed. For primitives, `is Int32` is an exact type check, `is "Dave"` is exact value equality, and `is Null` is an exact check.
- **`has` only checks that fields are present.** `value has { name, age }` matches if both keys exist, regardless of their types. Extra fields are allowed.

Concretely, given:

```lin
type Person = { "name": String, "age": Int32 }
```

the value `{ "name": "Bob", "age": "not a number" }`:

- does **not** match `is Person` — `age` is a `String`, not an `Int32`, and `is` checks types recursively.
- **does** match `has { name, age }` — both keys are present, and `has` ignores their types.

Use `is` when you need a sound type guarantee before reading fields; use `has` when you already know the types from context and just want to destructure.

## `is` — exact / deep type match

`is` checks an exact type, value, or — for object types — a full recursive type match:

```lin
match input
  is Null    => "null"
  is "Dave"  => "Big Dave!"
  is String  => "some string"
  is Int32   => "some integer"
```

Arms are checked top-to-bottom. For literal values (`is "Dave"`), only that exact value matches.

## `has` — structural (presence-only) match

`has` checks that an object contains at least certain fields, allowing extra fields. It does **not** check the types of those fields:

```lin
val greet = (person: Json): String =>
  match person
    has { "name": "Alice" }  => "Hello, Alice!"
    has { name, age }        =>
      "${name} is ${age} years old"
    else => "unknown person"
```

Shorthand `{ name }` in a `has` pattern binds the field to a local variable with the same name.

## `when` guards

Add a condition to a match arm with `when`:

```lin
val classify = (n: Int32): String =>
  match n
    is Int32 when n < 0  => "negative"
    is Int32 when n == 0 => "zero"
    is Int32             => "positive"
```

The arm only matches if both the pattern and the guard are satisfied. If the guard fails, matching continues with the next arm.

## `else` — catch-all

`else` matches any value not caught by an earlier arm:

```lin
match input
  is Null   => "null"
  is String => "string"
  else      => "something else"
```

## Matching tagged unions

Represent errors and results as tagged objects — then match on the tag:

```lin
val divide = (a: Float64, b: Float64): Json =>
  if b == 0.0 then { "type": "failure", "error": "division by zero" }
  else { "type": "success", "value": a / b }

val msg = match divide(10.0, 2.0)
  has { "type": "success", value } => "result: ${value}"
  has { "type": "failure", error } => "error: ${error}"
```

Note: you cannot match on a *generic application* directly — `is Result<Float64, String>` is not supported. Match the underlying tagged shape instead, as above with `has { "type": "success", value }`.

## Narrowing in branches

After an `is` check, the type is narrowed in the matched branch:

```lin
val process = (input: String | Int32 | Null): String =>
  match input
    is Null   => "nothing"
    is Int32  => "number times two: ${input * 2}"   // input is Int32 here
    is String => "length: ${length(input)}"          // input is String here
```

## Nested patterns

Match arms can contain blocks with local bindings:

```lin
val summarise = (result: Json): String =>
  match result
    has { "type": "success", value } =>
      val rounded = value
      "ok: ${rounded}"

    has { "type": "failure", error } =>
      val msg = "failed: ${error}"
      msg

    else => "unknown"
```
