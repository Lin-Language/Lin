# Minimal JSON Expression Language — Draft Specification v3

## 1. Purpose

This language is a small, expression-based programming language built around strict JSON data, structural typing, first-argument function application, destructuring, pattern matching, opaque iterator/runtime types, and value-based error handling.

The design goal is to keep the language surface small while supporting practical programming with JSON-shaped data and functional-style pipelines.

## 2. Design Principles

1. Everything is an expression.
2. Runtime data values are strict JSON values, plus opaque runtime values such as functions, iterators, iterables, and modules.
3. JSON object keys are strings and must be quoted in object literals.
4. There are no semicolons.
5. Whitespace and indentation are significant.
6. Types are structural by default.
7. Errors are ordinary values, usually represented with union types.
8. There are no classes.
9. Behaviour is expressed with functions, closures, partial application, and opaque runtime protocols.
10. Pattern matching is the primary way to consume union types.

## 3. Lexical Structure

### 3.1 Comments

Only line comments are supported. They begin with `//` and continue to the end of the line. There are no block comments.

```txt
// This is a comment
val x = 1 // This is also a comment
```

### 3.2 Whitespace and Indentation

Indentation defines blocks. Indentation is always two spaces per level. Tabs are not permitted for indentation.

Source files use LF line endings. CRLF is rejected with a diagnostic; mixed line endings are an error.

Blank lines are permitted anywhere inside a block and do not affect block structure.

```txt
val add = (a: Int32, b: Int32) =>
  a + b
```

A block evaluates to its final expression.

```txt
val calculate = (x: Int32): Int32 =>
  val doubled = x * 2
  doubled + 1
```

A logical line may continue on the next line when the continuation begins with `&&` or `||`. The continuation must be indented at least one level deeper than the start of the line; any deeper indent is acceptable. Multiple stacked continuations are allowed.

```txt
val isAdultBob = person["age"] >= 18
  && person["name"] == "Bob"
  && person["active"]
```

### 3.3 Identifiers

Identifiers are used for values, functions, type names, imports, destructuring bindings, and local bindings.

By convention, built-in and named types use `CamelCase`:

```txt
String
Boolean
Null
Number
Json
Int32
Float64
Iterator<T>
```

Value and function names usually use lower camel case:

```txt
substring
indexOf
parseInt32
```

### 3.4 Reserved Keywords

The core reserved keywords are:

```txt
val
var
type
export
if
then
else
match
is
has
when
import
from
as
null
true
false
```

### 3.5 String Literals

Strings are delimited with double quotes (`"`).

```txt
val name = "Bob"
```

Strings may span multiple lines. Newlines inside the literal are preserved verbatim.

```txt
val poem = "Roses are red,
Violets are blue."
```

#### 3.5.1 Escape Sequences

Inside a string literal, the following escape sequences are recognised:

```txt
\"   double quote
\\   backslash
\n   newline
\r   carriage return
\t   tab
\0   null character
\u{HHHH}   unicode codepoint (1–6 hex digits)
```

#### 3.5.2 Interpolation

Strings support interpolation with `${ expression }`. The expression is evaluated in the surrounding scope and its result is converted to a string via `toString`.

```txt
val name = "Bob"
val age = 42
val greeting = "Hello ${name}, you are ${age + 1} next year"
```

Interpolated expressions can themselves contain string literals, function calls, and arbitrary expressions, but they cannot span multiple statements.

A literal `$` is written `\$` when followed by `{`. A literal `${` not intended as interpolation is written `\${`.

### 3.6 Numeric Literals

Integer literals may be written in:

```txt
42        decimal
0xFF      hexadecimal
0b1010    binary
0o755     octal
1_000_000 underscores as visual separators (no semantic effect)
```

Floating-point literals may include an exponent and underscores:

```txt
3.14
3.14e2
1_000.5
6.022e23
```

A literal may carry an explicit type suffix to override default inference:

```txt
42i8       Int8
42u32      UInt32
3.14f32    Float32
3.14f64    Float64
```

Without a suffix, integer literals default to `Int32` and floating-point literals default to `Float64`, subject to context-driven inference (see §26).

### 3.7 Negative Literals

A leading `-` is part of a numeric literal when:

1. there is no whitespace between the `-` and the digits, and
2. the previous token cannot end an expression (i.e., it is one of `(`, `,`, `=`, `=>`, `:`, an operator, or a keyword such as `then`, `else`, `is`, `has`, `when`, `return`-style position).

Otherwise the `-` is parsed as the binary subtraction operator.

```txt
val temperature: Int32 = -5      // literal
val delta: Int32 = x - 5          // subtraction
val passed = f(-5, x - 3)         // first: literal; second: subtraction
```

There is no unary minus operator on arbitrary expressions in this version of the language. To negate a computed value, subtract from zero:

```txt
val negated = 0 - x
```

There are no other unary operators in v1.

## 4. Values

### 4.1 Primitive Values

```txt
val name: String = "Bob"
val active: Boolean = true
val missing: Null = null

val count: Int32 = 42
val total: UInt64 = 9000000000
val ratio: Float64 = 3.14
```

The value `null` has type `Null`.

### 4.2 Numeric Types

The language has explicit numeric families:

```txt
Int8   Int16   Int32   Int64
UInt8  UInt16  UInt32  UInt64
Float8 Float16 Float32 Float64
```

Floating-point families follow IEEE 754 and are always signed; there is no `UFloat`. Non-negativity of a float is a runtime invariant, not a type-level one — enforce it with validation if needed.

The built-in name `Number` is a union alias covering every numeric family:

```txt
type Number =
  | Int8 | Int16 | Int32 | Int64
  | UInt8 | UInt16 | UInt32 | UInt64
  | Float8 | Float16 | Float32 | Float64
```

`Number` is purely a static name — it has no distinct runtime representation (§27.4). Every numeric value remains tagged with its specific family, and `is Int32`, widening (§26), and arithmetic dispatch all work exactly as if the union had been written out by hand. `Number` exists so that signatures accepting any numeric and the definition of `Json` (§5) have a concise spelling.

See §26 for coercion and inference rules.

### 4.3 Strict JSON Object Literals

Object literals use strict JSON syntax.

Rules:

1. Keys must be quoted strings.
2. Commas are required between fields.
3. Trailing commas are not allowed.
4. Runtime object values must be JSON-compatible.
5. An object literal may include spread elements of the form `...expr`. `expr` must evaluate to an object at runtime; otherwise an error is raised. Fields and spreads are processed left-to-right. When the same key is written more than once (by spread or explicitly), the later value replaces the earlier one and the key keeps its first-occurrence position in iteration order.

```txt
val person = {
  "name": "Bob",
  "age": 42,
  "active": true,
  "spouse": null
}

val older = { ...person, "age": 43 }
```

### 4.4 Arrays

Arrays use strict JSON array syntax.

```txt
val numbers = [1, 2, 3]
val names = ["Bob", "Alice"]
```

Two distinct *type* forms describe arrays — see §8.2 and §8.3.

## 5. Built-in Types

```txt
String
Boolean
Null
Number
Json
Error
Int8 Int16 Int32 Int64
UInt8 UInt16 UInt32 UInt64
Float8 Float16 Float32 Float64
Function
Iterator<T>
Iterable<T>
```

`Number` is a union alias covering every numeric family (§4.2).

`Json` represents arbitrary JSON data. It is the recursive union of every JSON-shaped value:

```txt
type Json =
  | String
  | Boolean
  | Null
  | Number
  | Json[]
  | { ...Json }      // any object whose values are Json
```

The last form is informal: there is no general index-signature syntax in v1; in practice a `Json`-valued object is any object whose fields are themselves `Json`.

`Function`, `Iterator<T>`, and `Iterable<T>` are opaque runtime types. They are not JSON values and are not described using JSON-shaped object types.

`Unknown` and `Never` are not built-in types in the initial language.

## 6. JSON Access

Bracket notation is used for both JSON object key access and array indexing. Bracket access is **safe by default**: object accesses never raise an error, and `Null` propagates through chains.

```txt
val name = person["name"]
val city = person["address"]["city"]
val first = numbers[0]
```

Dot syntax is not used for JSON field access.

### 6.1 Runtime Semantics

| Operand kind          | Access                                  | Result                          |
| ---                   | ---                                     | ---                             |
| Object, key present   | `obj["k"]`                              | the stored value                |
| Object, key missing   | `obj["k"]`                              | `Null`                          |
| `Null`                | `null["k"]`                             | `Null`                          |
| Array, index in range | `arr[i]`                                | the element                     |
| Array, index OOB      | `arr[i]`                                | runtime error                   |
| `Null`                | `null[i]`                               | `Null`                          |

Because `Null` propagates, you may chain accesses through unknown structures without intermediate checks:

```txt
val deep = obj["some"]["prop"]["that"]["doesnt"]["exist"]  // null
```

This is equivalent to the optional-chaining operator (`?.`) in other languages — but it applies to every bracket access by default.

### 6.2 Static Typing of Access

- If the operand's static type is a typed object that declares the key as `T`, the access has type `T`.
- If the operand's static type is `Json`, the access has type `Json` (which already covers `Null`).
- If the operand's static type is a typed object that does **not** declare the key, the access is a compile-time error. (Use `Json` if you need free-form access.)
- If the operand may be `Null` (e.g., a union `T | Null`), the access type widens to include `Null`.
- Array element access on `T[]` has type `T` (the static type does not include `Null`; the runtime error is the contract for OOB).

## 7. Bindings

### 7.1 Immutable Bindings

```txt
val x = 1
val name: String = "Bob"
```

`val` bindings are immutable.

### 7.2 Mutable Bindings

```txt
var count = 0
count = count + 1
```

`var` bindings are mutable.

Assignment expressions evaluate to the assigned value.

```txt
val result = count = count + 1
```

Mutable bindings are captured by reference in closures.

```txt
val makeCounter = (start: Int32) =>
  var count = start

  () =>
    count = count + 1
    count
```

### 7.3 Recursive Bindings

A `val` whose right-hand side is a function literal may reference itself by name. The name is in scope within the function body.

```txt
val factorial = (n: Int32): Int32 =>
  if n == 0
    then 1
    else n * factorial(n - 1)
```

A `val` whose right-hand side is *not* a function literal may **not** reference itself.

Mutual recursion between two top-level `val` bindings of function literals is permitted: both names are in scope across both bodies.

## 8. Type Declarations

### 8.1 Object Types

Object types are JSON-shaped but are type syntax, not JSON values.

```txt
type Person = {
  "name": String,
  "age": Int32
}
```

### 8.2 Array Types

The type of an unbounded array of `T` is written `T[]`.

```txt
val xs: Int32[] = [1, 2, 3]
val names: String[] = ["Bob", "Alice"]
```

`T[]` describes an array of any length whose every element has type `T`.

### 8.3 Fixed-Length Array Types

A type written as `[T1, T2, ..., Tn]` describes an array of exactly `n` elements, where each position has the corresponding type.

```txt
val pair: [String, Int32] = ["age", 42]
val triple: [String, Int32, Int32] = ["coords", 10, 20]
```

These are *not* tuples — they remain JSON arrays at runtime. The `[T1, T2, ...]` type form simply constrains length and positional element types at the type level.

A fixed-length array type is assignable to the corresponding unbounded type when all positional types are compatible. The reverse is not true.

### 8.4 Union Types

Union types use `|`.

```txt
val maybeName: String | Null = null
```

```txt
type Id = String | Int64
```

### 8.5 Function Types

Function types use argument-list syntax followed by `=>`.

```txt
type Predicate<T> = (T) => Boolean
type Mapper<T, U> = (T) => U
type Reducer<T, U> = (U, T) => U
```

### 8.6 Generic Types

Generic type declarations use angle brackets.

```txt
type Result<T, E> =
  | { "type": "success", "value": T }
  | { "type": "failure", "error": E }
```

Generic type application also uses angle brackets.

```txt
type ParseInt32Result = Result<Int32, String>
```

### 8.7 Type Expression Precedence

Type-expression operators bind in this order, tightest first:

```txt
1. T[]                 (postfix array)
2. Generic<T1, T2>     (postfix generic application)
3. (T1, T2) => U       (function arrow)
4. T | U               (union)
```

So `Int32 | String[]` parses as `Int32 | (String[])`, and `(Int32) => String[]` parses as `(Int32) => (String[])`. Parenthesise to disambiguate where the surface reading is unclear.

### 8.8 Variance

Generic types are **covariant** in their parameters where they appear in producer position (return type, array element, container content), and **contravariant** in consumer position (function arguments).

Concretely:

- `Person[]` is assignable to `Json[]`.
- `Iterator<Person>` is assignable to `Iterator<Json>`.
- `(Person) => Int32` is assignable to `(Bob) => Int32` for any `Bob` compatible with `Person`.
- A function returning `Person` is assignable to one returning `Json`.

### 8.9 Structural Typing

Types are structural by default.

```txt
type Named = {
  "name": String
}

val greet = (item: Named): String =>
  "Hello " + item["name"]
```

A value with additional fields is compatible with a smaller structural type *for the purpose of function-argument passing and type ascription*.

```txt
val greeting = greet({
  "name": "Alice",
  "age": 99
})
```

This compatibility relationship is the same as `has` — see §13.

## 9. Functions

### 9.1 Function Expressions

```txt
val add = (a: Int32, b: Int32): Int32 =>
  a + b
```

Return types may be inferred where possible.

```txt
val add = (a: Int32, b: Int32) =>
  a + b
```

### 9.2 Single-expression Functions

```txt
val add = (a: Int32, b: Int32) => a + b
```

### 9.3 Blocks as Function Bodies

```txt
val total = (price: Float64, quantity: Int32): Float64 =>
  val subtotal = price * quantity.toFloat64()
  val tax = subtotal * 0.2
  subtotal + tax
```

The final expression is the function result.

### 9.4 No `return`

The language does not use `return` because all blocks are expressions.

Invalid:

```txt
val add = (a: Int32, b: Int32) =>
  return a + b
```

## 10. Function Calls and Partial Application

### 10.1 Function Calls

```txt
val result = add(1, 2)
```

### 10.2 Partial Application

Functions may be partially applied from left to right by supplying fewer arguments than the function declares. The result is a new function awaiting the remaining arguments.

```txt
val addTen = add(10)
val fifteen = addTen(5)
```

The type of `addTen` is `(Int32) => Int32`.

### 10.3 Over-Application Is an Error

Supplying more arguments than a function expects is a compile-time error.

```txt
val add = (a: Int32, b: Int32) => a + b

val bad = add(1, 2, 3)   // error: add takes 2 arguments, got 3
```

### 10.4 Argument Evaluation Order

Argument expressions are evaluated left to right before the function is called.

### 10.5 No Tuples

Parentheses are argument lists, not tuples. There are no language-level tuples.

This syntax is not a tuple value:

```txt
("hello", 1)
```

It is an argument list, and is only meaningful in call or dot-application contexts (see §11.1).

## 11. Dot Application

Dot syntax applies the expression on the left as the first argument to the function on the right.

```txt
x.f(y, z)
```

is equivalent to:

```txt
f(x, y, z)
```

Example:

```txt
val direct = substring("myString", 1, 5)
val dotted = "myString".substring(1, 5)
```

### 11.1 Dot Partial Application

Writing `x.f` with no argument list is partial application of `f` with `x` as the first argument.

```txt
val takeFirstFive = "myString".substring
val result = takeFirstFive(0, 5)
```

is equivalent to:

```txt
val takeFirstFive = substring("myString")
val result = takeFirstFive(0, 5)
```

Multiple arguments may be supplied in a leading parenthesised list to the left of the dot:

```txt
val takeNext = ("myString", 1).substring
val result = takeNext(5)
```

is equivalent to:

```txt
val takeNext = substring("myString", 1)
val result = takeNext(5)
```

The `(x, y).f` form is the only place where a parenthesised comma-separated list appears outside of a call site — and it is still an argument list, not a tuple.

### 11.2 Method Calls Require Parentheses

A function with no further arguments must still be called with `()`. There is no implicit invocation.

```txt
val n = items.length()      // correct
val n = items.length        // partial application — n is a function
```

### 11.3 Chaining

```txt
val result = "  hello  "
  .trim()
  .toUpper()
```

Equivalent to:

```txt
val result = toUpper(trim("  hello  "))
```

## 12. If Expressions

`if` is an expression and must produce a value. Every `if` requires an `else` branch.

Three layout forms are supported:

```txt
// Single-line
val a = if cond then x else y

// then/else on subsequent lines, one block deeper than `if`
val b = if cond
  then x
  else y

// Block branches, also one block deeper than `if`
val c = if cond
  then
    val prefix = "ad"
    prefix + "ult"
  else
    val prefix = "ch"
    prefix + "ild"
```

`then` and `else` must be at the same indent level — exactly one indent level deeper than the column of `if`.

A logical line that begins an `if` may continue using `&&` or `||` as described in §3.2:

```txt
val label = if person["age"] >= 18
  && person["active"]
  then "active adult"
  else "other"
```

### 12.1 Nested `if` Inside `match`

`else` always binds to the closest preceding `if` or `match` whose indent is one level shallower. Concretely:

```txt
match input
  has { name } =>
    if name == "Dave"
      then "Big Dave!"
      else "regular ${name}"

  else =>
    "no name"
```

The inner `if`'s `else` is at column 6 (under `then`); the outer `match`'s `else` is at column 2 (under `has`). No ambiguity.

## 13. `is` and `has` Expressions

`is` and `has` can be used in `if` expressions and `match` patterns.

### 13.1 `is`

`is` performs an **exact** match.

- For a named object type `T`, `value is T` is true only if `value` has *exactly* the fields of `T`, with no extra fields, each typed correctly.
- For a primitive type, `value is T` is true only if the runtime value has that exact type.
- For a literal, `value is "Dave"` is true only if the value equals the literal.

```txt
val describe = (input: String | Int32 | Null): String =>
  if input is Null
    then "No value"
    else if input is Int32
      then "Int32"
      else "String"
```

```txt
val isDave = (input: String): Boolean =>
  if input is "Dave"
    then true
    else false
```

`is` is not supported against generic type applications in v1. Writing `value is Result<Int32, String>` is a compile-time error. Match the underlying tagged shape instead (see §18).

`is` and `has` are expressions of type `Boolean` and may be used in any expression context, not only `if` conditions and `match` arms:

```txt
val isAdult = person has { age } && person["age"] >= 18
```

Literal values used with `is` have base type, not singleton type. `"Dave" is "Dave"` is true; the type of the literal `"Dave"` is `String`.

A single `match` arm may not combine `is` and `has` patterns — each arm uses one keyword.

### 13.2 `has`

`has` performs a **structural compatibility** check — the value contains *at least* the requested shape, but may have additional fields.

- For a named object type `T`, `value has T` is true if every field of `T` is present in `value` with a compatible type. Extra fields are permitted.
- For an inline shape `{ a, b }`, `value has { a, b }` is true if `value` is an object containing at least those keys.

```txt
val describeNamed = (input: Json): String =>
  if input has { name }
    then "Named: " + input["name"]
    else "Unnamed"
```

For unions and generics, `is` and `has` apply only to concrete shapes, not to compound types. To inspect a tagged-union value, match against the underlying tag shape:

```txt
match result
  has { "type": "success", value } => ...
  has { "type": "failure", error } => ...
```

Writing `is Result<Int32, String>` is not supported; match the underlying shape instead.

## 14. Equality

Equality is structural for JSON-compatible values, and JSON objects are unordered.

```txt
val a = 1 == 1                              // true
val b = "1" == 1                            // false
val c = null == null                        // true
val d = "str" == "str"                      // true
val e = { "a": 1 } == { "a": 1 }            // true
val f = { "a": 1, "b": 2 } == { "b": 2, "a": 1 } // true (order independent)
val g = [1, 2] == [1, 2]                    // true (arrays are ordered)
val h = [1, 2] == [2, 1]                    // false
```

Function, iterator, iterable, and module equality are not defined.

Numeric equality across families: numbers compare by mathematical value after coercion to the wider type (see §26). `1 == 1.0` is true; `"1" == 1` is false because they are different runtime kinds.

## 15. Destructuring

Destructuring is supported in `val` bindings, function parameters, pattern matching, and imports.

### 15.1 Object Destructuring

```txt
val person = {
  "name": "Bob",
  "age": 42
}

val { "name": name, "age": age } = person
```

### 15.2 Object Destructuring Shorthand

In destructuring patterns, bare names are shorthand for quoted JSON keys with the same local binding name.

```txt
val { name } = person
```

is equivalent to:

```txt
val { "name": name } = person
```

This shorthand does not change object literal syntax. Object literals still require quoted keys.

### 15.3 Object Alias Binding

```txt
val { "name": displayName } = person
```

### 15.4 Nested Destructuring

```txt
val {
  "name": name,
  "address": {
    "city": city
  }
} = person
```

### 15.5 Array Destructuring

```txt
val [first, second] = ["a", "b"]
```

### 15.6 Rest Spread

Array rest spread:

```txt
val [first, ...rest] = ["a", "b", "c"]
```

Object rest spread:

```txt
val { name, ...remaining } = person
```

Object spread is also valid in object *expressions*; see §4.3.

### 15.7 Function Parameter Destructuring

```txt
val describePerson = ({ name, age }: Person): String =>
  name + " is " + age.toString()
```

## 16. Pattern Matching

Pattern matching is used to consume unions, inspect values, and destructure JSON-shaped data.

```txt
val describe = (input: String | Int32 | Null): String =>
  match input
    is Null =>
      "No value"

    is Int32 =>
      "Int32: " + input.toString()

    is String =>
      "String: " + input
```

### 16.1 `is` Patterns

`is` means exact match.

```txt
match input
  is Null => "No value"
  is "Dave" => "Big Dave!"
  is String => "String"
```

For arrays:

```txt
match items
  is [] => "empty"
  is [one] => "exactly one item"
  is [first, second] => "exactly two items"
```

For objects:

```txt
match input
  is { name } => "exactly one field: name"
```

### 16.2 `has` Patterns

`has` means compatible, contains, or unpackable.

```txt
match input
  has { name } => "has a name"
```

For arrays:

```txt
match items
  has [first] => "at least one item"
  has [first, second] => "at least two items"
  has [first, ...rest] => "one or more items"
```

### 16.3 Pattern Guards with `when`

`when` adds a guard condition to a match arm.

```txt
val describeName = (input: String | Person | Null): String =>
  match input
    is Null =>
      "No name"

    is "Dave" =>
      "Big Dave!"

    has { name, age } when age > 30 =>
      "Old person: " + name

    has { name } =>
      "Young person: " + name

    is String =>
      "Name: " + input
```

The pattern must match first. If it matches, the `when` condition is evaluated; if the guard is false, matching continues with the next arm.

### 16.4 Catch-All `else` Arm

A `match` may end with an `else` arm. It matches any value not caught by an earlier arm. `else` is written `else => expr` and is indented at the same level as the other arms.

```txt
match input
  is Null => "null"
  is String => "string"
  else => "other"
```

`else` is the only catch-all form — there is no wildcard `_`.

### 16.5 Match Arm Layout

Each arm begins on its own line, indented one level deeper than the `match` keyword. Writing multiple arms on the same line is invalid:

```txt
// invalid
match input
  is Null => "x" is "Dave" => "y"
```

The arm body may be a single expression on the same line as `=>`, or a block on subsequent lines indented one level deeper than the arm:

```txt
match input
  is Null => "no value"

  has { name, age } =>
    val label = if age > 30 then "old" else "young"
    "${label}: ${name}"
```

### 16.6 Match Exhaustiveness

A `match` that omits `else` must exhaustively cover the static type of the scrutinee. v1 enforces exhaustiveness as follows:

- For closed unions whose arms are all `is` patterns over primitive types, `Null`, or literal values, exhaustiveness is a **compile-time error** when not covered.
- For all other patterns (`has`, structural shapes, tagged unions, mixed arms), exhaustiveness is a **warning** only.

Adding `else` always satisfies exhaustiveness.

## 17. Iteration

Iteration is represented using opaque runtime types rather than JSON-shaped objects containing functions.

```txt
Iterator<T>
Iterable<T>
```

An `Iterator<T>` is a stateful traversal that produces values of type `T`.

An `Iterable<T>` is any value that can produce an `Iterator<T>`.

Arrays satisfy `Iterable<T>` automatically.

```txt
val ints: Int32[] = [1, 3, 5]

ints.for(num =>
  print(num * 2)
)
```

### 17.1 `for` Is a Built-in

`for` is a built-in function provided by the compiler. It has privileged access to the internals of opaque iterator values and is the only function that consumes an iterator by stepping through it.

```txt
for: <T>(Iterable<T>, (T) => Null) => Null
```

All other iteration combinators (`map`, `filter`, `reduce`, etc.) are ordinary library functions defined in terms of `for`.

### 17.2 Iterator Construction

The `iter` function constructs an opaque iterator from state-transition functions.

```txt
iter: <State, T>(
  () => State,
  (State) => Boolean,
  (State) => State,
  (State) => T
) => Iterator<T>
```

The arguments are:

1. **initial-state producer** — a thunk that returns a fresh starting state. It is a thunk (not a value) so that a consumer may restart the iterator by calling it again.
2. **continuation predicate** — given the current state, returns true if iteration should continue.
3. **next-state function** — given the current state, returns the next state.
4. **current-value function** — given the current state, returns the value to yield.

Example:

```txt
val list: String[] = ["a", "b", "c"]

val listIterator: Iterator<String> = iter(
  () => 0,
  i => i < list.length(),
  i => i + 1,
  i => list[i]
)
```

The returned value is an `Iterator<String>`. Its internal state is not accessible as JSON.

Invalid:

```txt
listIterator["next"]
listIterator["current"]
```

#### 17.2.1 Restartability

Because the initial-state producer is a thunk, consumers may obtain a fresh starting state for the same iterator. Whether a particular `Iterator<T>` is safely restartable depends on the closure over external state in its four functions. The language guarantees that calling the initial-state producer again returns a fresh logical start; it does not guarantee anything about external side effects.

### 17.3 Array Iteration

Arrays can be converted to iterators using `iterOf`.

```txt
iterOf: <T>(T[]) => Iterator<T>
```

### 17.4 Range Iteration

`range` returns an iterator. It is an ordinary library function.

```txt
range: (Int32, Int32) => Iterator<Int32>
```

```txt
range(0, 10).for(i =>
  print(i)
)
```

### 17.5 Iterator Functions

The standard iterator functions accept `Iterable<T>` or `Iterator<T>` values and use dot application for fluent chaining.

```txt
for:    <T>(Iterable<T>, (T) => Null) => Null
map:    <T, U>(Iterable<T>, (T) => U) => Iterator<U>
filter: <T>(Iterable<T>, (T) => Boolean) => Iterator<T>
reduce: <T, U>(Iterable<T>, U, (U, T) => U) => U
```

```txt
val squares = range(0, 10)
  .map(i => i * i)

val evenSquares = range(0, 10)
  .map(i => i * i)
  .filter(i => i % 2 == 0)

val total = [1, 2, 3]
  .reduce(0, (sum, value) => sum + value)
```

### 17.6 Iterator Design Rule

Iterator behaviour is not represented as a JSON object with function fields.

Invalid model:

```txt
type Iterator<T> = {
  "start": Function,
  "continue": Function,
  "next": Function,
  "current": Function
}
```

This is not used because JSON-shaped types should describe JSON-shaped data. Iterators are runtime traversal values, not JSON data.

## 18. Tagged Unions

Tagged unions are represented with structural JSON object types.

```txt
type Result<T, E> =
  | { "type": "success", "value": T }
  | { "type": "failure", "error": E }
```

```txt
val divide = (a: Float64, b: Float64): Result<Float64, String> =>
  if b == 0.0
    then {
      "type": "failure",
      "error": "Cannot divide by zero"
    }
    else {
      "type": "success",
      "value": a / b
    }
```

Consuming the result:

```txt
val message = match divide(10.0, 2.0)
  has { "type": "success", value } =>
    "Result: " + value.toString()

  has { "type": "failure", error } =>
    "Error: " + error
```

## 19. Errors

There are no exceptions and no throwing in user code. Errors are ordinary values. A function that may fail should return a union type.

An `Error` built-in type may exist for conventional error values, but it has no special control-flow behaviour.

### 19.1 Runtime Errors

A small number of language-level operations can fail at runtime. They terminate the program with a diagnostic — they do not produce a value, and they cannot be caught. They are reserved for unrecoverable program errors:

| Operation                                 | Result on failure |
| ---                                       | --- |
| Array index out of bounds                 | runtime error |
| Integer division by zero (`/`, `%`)       | runtime error |
| Explicit narrowing cast that loses information | runtime error |
| Non-exhaustive `match` (no arm matched and no `else`) | runtime error |
| Module initialisation cycle (§20.5)       | runtime error |

Object key access never causes a runtime error — missing keys produce `Null` (§6).

Floating-point operations follow IEEE 754: division by zero produces `±Infinity` or `NaN`, not an error. Integer `%` follows the sign of the dividend (Rust convention).

## 20. Imports

Imports use destructuring syntax.

```txt
import { substring, indexOf } from "std/string"
```

### 20.1 Aliasing

```txt
import { substring as substr } from "std/string"
```

### 20.2 Multi-line Imports

```txt
import {
  substring as substr,
  indexOf,
  trim,
  toUpper
} from "std/string"
```

### 20.3 Importing Types

Types may be imported with the same syntax.

```txt
import { Result } from "std/result"
```

### 20.4 Module Path Resolution

An import path is a slash-separated string that resolves to a `.lin` source file:

```txt
import { something } from "myDir/anotherDir/aFile"
```

resolves to `myDir/anotherDir/aFile.lin`, located relative to the importing file's directory by default. Paths beginning with a recognised library prefix (e.g. `std/`) resolve into the standard library.

### 20.5 Circular Imports

Circular imports are permitted and resolved lazily. The first read of any exported binding from a module forces that module's full initialisation. If a binding is observed while it is itself still being initialised (a cycle within a single init chain), the read is a runtime error.

### 20.6 Standard Library Modules

The v1 standard library is laid out as follows:

```txt
std/string   trim, toUpper, toLower, substring, indexOf, length, ...
std/number   parseInt32, parseFloat64, toInt32, toFloat64, isInt32, ...
std/array    map, filter, reduce, length, ...
std/result   Result type, helpers
std/io       print
```

The exact API of each module is not yet pinned down; the layout above is fixed.

## 21. Standard Library Style

Standard library functions use lower camel case.

```txt
substring  indexOf  toUpper  parseInt32
```

Built-in types use `CamelCase`.

## 22. Modules

A source file is a module. Modules may export `val`, `var`, and `type` declarations using the `export` keyword.

```txt
export val name = "Bob"

export val add = (a: Int32, b: Int32): Int32 =>
  a + b

export var counter = 0

export type Person = {
  "name": String,
  "age": Int32
}
```

Modules are not ordinary JSON values because they may contain functions and types.

## 23. Scoping

Bindings are lexical.

Blocks introduce nested scopes.

Closures capture bindings from their defining scope. Mutable bindings are captured as mutable cells: closures over the same `var` see the same storage.

## 24. Operators

### 24.1 Operator List

```txt
+   -   *   /   %
==  !=  >   <   >=  <=
&&  ||
```

These are built-in operators, not ordinary functions. They are not available through dot application or partial application.

There are no unary operators in v1 (see §3.7 for negative literals). Boolean negation must be done explicitly:

```txt
val notReady = ready == false
```

### 24.2 Precedence

Precedence follows the standard convention used by C-family languages, from highest to lowest:

```txt
1. ()  []  .          (call, index, dot application)
2. *  /  %
3. +  -
4. <  <=  >  >=
5. ==  !=
6. &&
7. ||
```

All binary arithmetic and comparison operators are left-associative. `&&` and `||` are left-associative and short-circuiting.

## 25. Type Narrowing

`is`, `has`, `if`, and `match` may narrow union types.

```txt
val display = (input: String | Null): String =>
  if input is Null
    then "missing"
    else input
```

Inside the `else` branch, `input` is narrowed to `String`.

```txt
val display = (input: String | Int32): String =>
  match input
    is String => input
    is Int32 => input.toString()
```

Narrowing carries into:

- the matched branch of an `if`/`else`,
- the matched arm of a `match`,
- nested blocks within either,
- the right-hand side of a `&&` whose left-hand side is a narrowing test (e.g. `if input is String && input.length() > 0 ...`).

Narrowing is invalidated on the first assignment to a `var` whose narrowed type would no longer hold.

## 26. Numeric Coercion

Numeric values automatically widen between numeric types when used in arithmetic and comparison. Widening is always to a type that can fully represent the range of both operands — never to a type that could lose information.

- Two integers widen to the smallest integer type that fully contains both ranges. A signed and an unsigned of the same width widen to the next-larger signed integer.
- An integer combined with a floating-point value widens to a floating-point type large enough to hold the integer exactly when possible, otherwise to the larger floating-point family.
- Two floating-point values widen to the larger.

Explicit narrowing — assigning a wider numeric to a narrower one, or any floating-point to an integer — requires an explicit cast via stdlib (`toInt32`, `toFloat32`, etc.) and is a runtime error if the value cannot be represented exactly. Implicit narrowing is a compile-time error.

Literal inference: a numeric literal without a suffix takes the type required by its surrounding context if one exists; otherwise integer literals default to `Int32` and floating-point literals default to `Float64`.

Generic and overload-style inference uses bidirectional type checking: type information flows both from declarations into expressions and from expression context back into holes. This is sufficient for `[1,2,3].map(i => i * i)` to infer `T = Int32` and `U = Int32` without explicit annotation.

## 27. Runtime Model

Runtime values include:

```txt
String
Boolean
Null
Int*  UInt*  Float*
Array
Object
Function
Iterator
Iterable
Module
```

Objects and arrays are JSON-compatible. Functions, iterators, iterables, and modules are runtime values but are not JSON values.

### 27.1 Strings

Strings are stored as length-prefixed UTF-8 byte sequences. Indexing and slicing primitives in `std/string` operate at the Unicode codepoint level, not the byte level. Byte-level access, if needed, is provided by separate stdlib functions.

### 27.2 Closures and `var`

Closures capture `var` bindings by reference. Two closures that capture the same `var` share the same underlying storage cell — writes from one are visible to the other.

### 27.3 Tail Call Optimisation

The compiler is required to perform tail call optimisation for **direct self-recursive calls** in tail position. Recursive idioms (factorial, iterator construction over large sequences) must run in constant stack space when expressed tail-recursively. Mutual tail recursion is not required to be optimised in v1.

### 27.4 Numbers

Each numeric family has a distinct runtime representation. `Number` (§4.2) is a static union alias, not a runtime kind — there is no single runtime "Number" representation. Numeric values carry their family tag at runtime so that operations can dispatch on the correct width and signedness.

### 27.5 Objects

JSON objects are stored as insertion-ordered key/value maps. Iteration order matches insertion order. Equality is order-independent (§14).

Object spread in literals (§4.3) inserts each source entry in source-iteration order. If a key was already present, the value is replaced but its original position is preserved.

### 27.6 Iterators

An iterator is an opaque runtime value containing:

1. an initial-state thunk,
2. a continuation predicate,
3. a next-state function,
4. a current-value function,
5. the current state cell (set lazily on first step).

Only the `for` built-in may step through this state; user code cannot read it.

### 27.7 Partial Application

Partial application produces a value carrying the original function pointer and the accumulated arguments. Further application appends to the buffer. When the buffer matches the original arity, the function is invoked. This avoids allocating a new closure per argument.

### 27.8 `toString`

Every primitive supports `toString`:

- Integers: decimal, no leading zeros, with `-` for negatives.
- Floats: shortest round-trip decimal representation; integer-valued floats render with a trailing `.0` (e.g. `42.0`).
- `Boolean`: `"true"` / `"false"`.
- `Null`: `"null"`.
- `String`: returns itself.

`toString` is used implicitly by string interpolation `${expr}`.

### 27.9 Comparison

`<`, `<=`, `>`, `>=` on strings compare by codepoint order. On numbers, by mathematical value after widening (§26). On other types: compile-time error.

### 27.10 `length()`

`length()` is defined for:

- `String` → number of codepoints (`Int32`).
- `T[]` → number of elements (`Int32`).
- `Json` → for arrays, element count; for objects, key count; for any other variant, runtime error.

It is **not** defined on plain objects of declared shape — those have a fixed schema.

## 28. Compilation Model

The language is compiled, not interpreted from the user's perspective. The compilation pipeline:

```txt
source (.lin files)
  -> lexer
  -> indentation-aware token stream
  -> parser
  -> surface AST
  -> desugaring
  -> core AST
  -> type checking
  -> code generation
  -> single output artifact
```

A program is built from one entry-point `.lin` file and its transitive imports, and emitted as a single output artifact.

v1 is pure: there are no language-level effects beyond `print`. There is no async, no concurrency, no IO library beyond textual output.

### 28.1 Reference Implementation

The reference implementation is written in **Rust** and laid out as a Cargo workspace:

```txt
lin-lang/
  Cargo.toml                 (workspace root)
  crates/
    lin-common/              shared types: Span, Diagnostic, intern table
    lin-lex/                 lexer, indentation tokenizer
    lin-parse/               parser, surface AST
    lin-check/               desugaring, type checker, core AST
    lin-eval/                tree-walking interpreter (v1 backend)
    lin-stdlib/              built-in stdlib functions
    lin/                     the CLI binary
  docs/
  examples/
```

The interpreter in `lin-eval` is the v1 backend. A native codegen target is deferred (§30).

### 28.2 Diagnostics

The compiler halts at the first error in a given phase. Errors are presented with:

- the source span (file, line, column),
- the surrounding source excerpt,
- the rule violated,
- where applicable, a call stack for runtime errors.

The first-error policy keeps the v1 implementation simple; multi-error recovery is deferred.

## 29. Implementation Notes

Important desugarings:

```txt
x.f(y, z)         becomes  f(x, y, z)
x.f               becomes  f(x)             // partial application
(x, y).f          becomes  f(x, y)          // partial application
val { name } = p  becomes  val name = p["name"]
```

Imports bind the named exports of the resolved module into the current scope. Type-only imports erase at runtime.

Iterator construction is not desugared to JSON object construction — it creates an opaque runtime iterator value. `for` is implemented inside the compiler/runtime and is the only consumer that may step through that opaque value directly.

## 30. Open Questions

Deferred — not required to begin implementation:

1. **Native compilation target.** v1 uses a tree-walking interpreter (§28.1). A native or bytecode target is deferred.
2. **Concurrency model.** v1 is single-threaded and synchronous.
3. **Exact stdlib API.** Module layout is fixed (§20.6); precise signatures within each module are still being filled in incrementally.
4. **Tooling.** Formatter, LSP, test runner as a first-class command are deferred.
5. **Object rest destructuring iteration order.** Specified — see §4.3 and §27.5. Spread inserts source entries in source-iteration order; repeated keys keep their first-occurrence position.
6. **`Iterable<T>` mechanism.** Whether it is a true protocol-like type, a compiler-known structural capability, or purely a built-in opaque interface.
7. **Full numeric widening matrix.** §26 specifies the principle; the complete pairwise table is deferred.
8. **Multi-error reporting.** First-error-then-halt policy for v1; recoverable parsing/checking is deferred.

Decided:

1. `export` may be used on `val`, `var`, and `type` declarations.
2. `Json` is a built-in type; `Number` is a built-in union alias covering every numeric family (§4.2).
3. `Unknown` and `Never` are not built-in types initially.
4. `is Person` is exact; `has Person` or `has { ... }` matches shape and allows extra fields.
5. `is` on generic type applications is unsupported in v1.
6. `is`/`has` are expressions of type `Boolean` and may appear in any expression context.
7. A single `match` arm uses either `is` or `has`, not both.
8. Assignment expressions evaluate to the assigned value.
9. Operators are built-in, not ordinary functions. No unary operators in v1.
10. `Iterator<T>` and `Iterable<T>` are opaque runtime types.
11. Arrays satisfy `Iterable<T>` automatically.
12. Array types are `T[]` (unbounded) and `[T1, T2, ...]` (fixed-length).
13. Strings use `"..."` with `${expr}` interpolation and standard escapes; UTF-8, length-prefixed; codepoint-aware indexing via stdlib (`at`).
14. Source files use the `.lin` extension; LF line endings only.
15. The language is compiled; the v1 backend is a tree-walking Rust interpreter.
16. `for` is a built-in function; `map`, `filter`, `reduce`, `range`, `iter`, `iterOf` are library functions.
17. `else` is the catch-all in `match`; arms each take their own indented line; no `_` wildcard.
18. Over-application of a function is a compile-time error.
19. JSON objects are unordered for equality; insertion-ordered at runtime; arrays are ordered.
20. `length()` and other accessor-style functions always require parentheses.
21. Recursive `val` is permitted only when the right-hand side is a function literal.
22. Closures capture `var` bindings as shared mutable cells.
23. The compiler must perform TCO for direct self-recursive tail calls.
24. Generic inference uses bidirectional type checking.
25. Exhaustiveness is a compile-time error for closed `is`/literal unions and a warning otherwise; non-exhaustive runtime fall-through is a runtime error.
26. Numeric widening is always to a type that can fully represent both operand ranges; widening is applied everywhere (operators, calls, returns, assignments) but narrowing is never implicit.
27. Standard library layout is `std/string`, `std/number`, `std/array`, `std/result`, `std/io`.
28. Two-space indentation; `&&`/`||` may begin a continuation line at any deeper indent.
29. Circular imports resolve lazily; first read of an export forces full init; cycles inside an init chain are a runtime error.
30. Bracket access is safe: missing object key → `Null`, `Null` propagates; array OOB is a runtime error.
31. Generic types are covariant in producer positions, contravariant in consumer positions.
32. Type-expression precedence: `[]` > `<>` > `=>` > `|`.
33. Literal types: literal values have their base type (`"Dave"` is `String`), not a singleton type.
34. Runtime errors halt the program; they cannot be caught.
35. Integer division by zero is a runtime error; floating-point follows IEEE 754.
36. `toString` is defined for every primitive (§27.8); used implicitly by string interpolation.
37. `length()` works on `String`, `T[]`, and `Json` (array or object variants).
38. Comparison `<`, `<=`, `>`, `>=` uses codepoint order for strings, mathematical order for numbers.
39. Source files use LF line endings; CRLF is rejected.
40. Blank lines inside indented blocks are allowed and ignored.

## 31. Complete Example

```txt
import { trim, toUpper } from "std/string"
import { print } from "std/io"

type Person = {
  "name": String,
  "age": Int32
}

type Result<T, E> =
  | { "type": "success", "value": T }
  | { "type": "failure", "error": E }

val describeName = (input: String | Person | Null): String =>
  match input
    is Null =>
      "No name"

    is "Dave" =>
      "Big Dave!"

    has { name, age } when age > 30 =>
      "Old person: ${name}"

    has { name } =>
      "Young person: ${name}"

    is String =>
      "Name: ${input}"

val parseAge = (input: String): Result<Int32, String> =>
  if input.isInt32()
    then {
      "type": "success",
      "value": input.toInt32()
    }
    else {
      "type": "failure",
      "error": "Invalid age"
    }
```
