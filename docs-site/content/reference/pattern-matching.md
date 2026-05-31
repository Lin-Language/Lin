# Pattern Matching Reference

## `match` expression

```lin
match scrutinee
  arm1
  arm2
  ...
  else => fallback
```

Arms are checked top-to-bottom. The value of the `match` expression is the value of the first matching arm. If no arm matches and there is no `else`, the program terminates with a runtime error.

## `is` vs `has` at a glance

The single most important distinction: **`is` validates types deeply and recursively; `has` only checks that fields are present.**

- **`is T`** — for an object type, checks the value is an object, every declared field of `T` is present, and every field has the correct type, recursing into nested objects and arrays. Extra fields are allowed. For primitives, `is Int32` is an exact type check, `is "Dave"` is exact value equality, `is Null` is exact.
- **`has { ... }`** — checks only that the named fields are present. Their types are **not** checked. Extra fields are allowed.

Given `type Person = { "name": String, "age": Int32 }`, the value `{ "name": "Bob", "age": "not a number" }` does **not** match `is Person` (age is a `String`), but **does** match `has { name, age }` (both keys present).

Use `is` when you need a sound type guarantee before reading fields; use `has` to destructure when you already know the types from context.

> Generic applications cannot appear in `is` patterns — `is Result<Int32, String>` is not supported. Match the underlying tagged shape with `has { "type": "success", value }` instead.

## `is` patterns — deep type / value match

`is` checks for an exact type or value, validating object types recursively.

### Type patterns

```lin
match x
  is Null    => "null"
  is Boolean => "boolean"
  is Int32   => "integer"
  is Float64 => "float"
  is String  => "string"
```

### Literal patterns

```lin
match name
  is "Alice" => "hello Alice"
  is "Bob"   => "hello Bob"
  is String  => "hello stranger"
```

### Array patterns

```lin
match items
  is []         => "empty"
  is [one]      => "exactly one"
  is [a, b]     => "exactly two"
```

### Object patterns

```lin
match obj
  is { "x": x, "y": y } => "point with x=${x} y=${y}"
```

`is` on an object requires every named field to be present **and** correctly typed (recursively). Extra fields beyond those named are allowed.

## `has` patterns — presence-only structural match

`has` checks for structural presence — the value must contain at least the named fields. The **types** of those fields are not checked.

### Object patterns

```lin
match person
  has { name, age } => "${name} is ${age}"
  has { name }      => "name only: ${name}"
  else              => "no match"
```

Bare identifiers in `has` patterns are shorthand for `{ "key": key }` — they bind the field value to a local variable.

### Tagged union matching

```lin
match result
  has { "type": "success", value } =>
    "got: ${value}"
  has { "type": "failure", error } =>
    "error: ${error}"
```

### Array patterns

```lin
match items
  has [first]           => "at least one"
  has [first, second]   => "at least two"
  has [head, ...rest]   => "head=${head} rest-length=${length(rest)}"
```

## `when` guards

```lin
match x
  is Int32 when x > 100 => "big"
  is Int32 when x > 10  => "medium"
  is Int32              => "small"
```

The arm matches if the pattern matches **and** the guard is `true`. If the guard fails, matching continues to the next arm.

## `else` catch-all

`else` always matches. It must be the last arm:

```lin
match x
  is Null => "null"
  else    => "something"
```

There is no `_` wildcard — `else` is the only catch-all form.

## Exhaustiveness

For closed unions matched entirely with `is` patterns over primitive types (`is Null`, `is Int32`, etc.), the compiler checks exhaustiveness at compile time and reports an error if a case is missing.

For all other patterns (`has`, structural shapes, tagged unions, mixed arms), exhaustiveness is checked at runtime: a non-exhaustive `match` with no `else` terminates the program when no arm matches.

Adding `else` always satisfies exhaustiveness.

## Narrowing

After a successful `is` arm, the scrutinee's type is narrowed to the matched type within that arm:

```lin
val process = (x: String | Int32): String =>
  match x
    is String => "string length: ${length(x)}"   // x: String here
    is Int32  => "number doubled: ${x * 2}"       // x: Int32 here
```

## `is` and `has` as Boolean expressions

`is` and `has` can be used outside of `match`, in any expression context:

```lin
val flag = value is Null
val hasName = obj has { name }
val isAdult = (person has { age }) && person["age"] >= 18
```
