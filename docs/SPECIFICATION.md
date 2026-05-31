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

Strings support interpolation with `${ expression }`. The expression is evaluated in the surrounding scope and its result is converted to a string via `toString`. Interpolation is the only way to build strings from parts; `+` does not work on strings.

```txt
val name = "Bob"
val age = 42
val greeting = "Hello ${name}, you are ${age + 1} next year"
```

Because the compiler sees all parts of an interpolated string as a single AST node, it can compute the total length and allocate exactly once, with no intermediate allocations.

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

There are two unary operators — bitwise `~` (§35.2) and logical `!` (§24.1, §35.2); there is no unary minus.

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

Object literals use strict JSON syntax, with one shorthand extension.

Rules:

1. Keys must be quoted strings, **or** bare identifiers used as shorthand (see below).
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

**Shorthand field syntax.** When a field's key and local variable name are identical, a bare identifier may be used:

```txt
val name = "Bob"
val age = 42
val obj = { name }                         // { "name": "Bob" }
val obj2 = { name, "active": true, age }   // { "name": "Bob", "active": true, "age": 42 }
```

A bare identifier in an object literal is syntactic sugar for `"ident": ident`. Shorthand fields, explicit key-value pairs, and spread expressions may appear in any order. A bare identifier followed by `:` (e.g. `{ name: "Bob" }`) is a compile-time error — use a quoted key.

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
Promise<T>
ThreadPool
Worker<Msg, Reply>
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

A `Json` value is **not** implicitly convertible to a concrete structured object type (an object with a required, non-nullable field). Binding or passing a `Json` value where such a type is expected is a compile-time error; convert it explicitly with `fromJson` (validated decode, `std/json`) or narrow it with `is`/`has` (runtime tag checks). `Json → Json` and `Json` flowing into scalars/handles/buffers/open objects remain permissive. See ADR-046 and ADR-047.

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
  if n == 0 then 1
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
  "Hello ${item["name"]}"
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

### 9.5 Default Parameter Values

A parameter may declare a default value with `= expr` after its (optional) type
annotation, making it optional at the call site. Optional parameters must be
last. See §10.6 for the full semantics.

```txt
val greet = (name: String, greeting: String = "Hello") =>
  "${greeting}, ${name}"
```

## 10. Function Calls and Partial Application

### 10.1 Function Calls

```txt
val result = add(1, 2)
```

### 10.2 Partial Application

Functions may be partially applied from left to right. Partial application is
requested with an **explicit trailing comma** after the supplied arguments; the
result is a new function awaiting the remaining arguments.

```txt
val addTen = add(10,)
val fifteen = addTen(5)
```

The type of `addTen` is `(Int32) => Int32`.

A call without a trailing comma is a complete call. If it supplies fewer
arguments than the function declares, the omitted trailing parameters must have
default values (see §10.6), which are filled in; otherwise it is an error (§10.5).
The trailing comma is what distinguishes "call now, using defaults for the rest"
from "partially apply." A trailing comma on a fully-saturated argument list has
no effect.

```txt
val add = (a: Int32, b: Int32) => a + b

val f  = add(10)    // error: add has no default for `b`; use add(10,) to curry
val g  = add(10,)   // partial application — g : (Int32) => Int32
val s  = add(1, 2)  // complete call
```

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

### 10.6 Default Argument Values

A parameter may declare a default value with `= expr` after its (optional) type
annotation. Such a parameter is **optional**: a complete call (no trailing comma)
may omit it, and the default expression is evaluated to supply the missing value.

```txt
val greet = (name: String, greeting: String = "Hello") => "${greeting}, ${name}"

greet("World")          // "Hello, World"   — greeting defaulted
greet("World", "Hi")    // "Hi, World"
```

Rules:

- **Optional parameters must be last.** Once a parameter has a default, every
  parameter after it must also have one. A required parameter following an
  optional one is a compile-time error.
- A default expression is type-checked against its parameter's type.
- A default expression may reference parameters declared **before** it (and any
  outer binding in scope), so defaults can chain:

  ```txt
  val box = (w: Int32, h: Int32 = w, area: Int32 = w * h) => area
  box(4)        // area = 4 * 4 = 16
  box(4, 3)     // area = 4 * 3 = 12
  ```

- Default values are filled left-to-right for the omitted trailing parameters.
  A complete call must still supply at least the **required** (non-defaulted)
  parameters; supplying fewer is an error (§10.5).
- Default-fill applies uniformly to direct calls, dot-application
  (`x.f(...)`, §11), and calls through a first-class function value
  (`val g = greet; g("World")`).
- To partially apply a function that has defaults — rather than fill them — use
  an explicit trailing comma (§10.2): `greet("World",)` yields a function
  awaiting `greeting`.

Default values are evaluated by the *defining* module, so an imported function
carries its defaults across module boundaries.

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

// then at end of condition line, body indented, else at if-level
val b = if cond then
  x
else
  y

// Block branches
val c = if cond then
  val prefix = "ad"
  "${prefix}ult"
else
  val prefix = "ch"
  "${prefix}ild"
```

`then` always appears on the condition line (or the last continuation line of the condition). `else` is at the same indent level as `if`.

A logical line that begins an `if` may continue using `&&` or `||` as described in §3.2:

```txt
val label = if person["age"] >= 18
  && person["active"] then "active adult"
else "other"
```

### 12.1 Nested `if` Inside `match`

`else` always binds to the closest preceding `if` or `match` whose indent is one level shallower. Concretely:

```txt
match input
  has { name } =>
    if name == "Dave" then "Big Dave!"
    else "regular ${name}"

  else =>
    "no name"
```

The inner `if`'s `else` is at the same indent as the `if` itself; the outer `match`'s `else` is at the top match-arm level. No ambiguity.

## 13. `is` and `has` Expressions

`is` and `has` can be used in `if` expressions and `match` patterns.

### 13.1 `is`

`is` performs a **type-exact** match: the value must conform to `T`, checked recursively.

- For a named object type `T`, `value is T` is true if `value` is an object that has every field of `T` **present and of the correct type** (checked recursively into nested objects, arrays, and literal-typed fields). Extra fields are permitted — `is` validates field *types*, not field *count*. This is what makes the post-match narrowing to `T` sound (ADR-054): if `value is T` succeeds, `value` genuinely conforms to `T`, so the narrowed field types are not a lie. Field-type checking follows the same structural-validation semantics and number policy as `fromJson` (§18, `std/json`): an integer-typed field accepts an integral in-range number, a float-typed field accepts any number, a literal-typed field requires the exact value.
- For a primitive type, `value is T` is true only if the runtime value has that exact type.
- For a literal, `value is "Dave"` is true only if the value equals the literal.

The difference between `is T` and `has T` (§13.2) for an object type is therefore precisely whether field *types* are validated: `is` checks presence **and** type; `has` checks presence only. Both permit extra fields.

```txt
val describe = (input: String | Int32 | Null): String =>
  if input is Null then "No value"
  else if input is Int32 then "Int32"
  else "String"
```

```txt
val isDave = (input: String): Boolean =>
  if input is "Dave" then true
  else false
```

`is` is not supported against generic type applications in v1. Writing `value is Result<Int32, String>` is a compile-time error. Match the underlying tagged shape instead (see §18).

`is` and `has` are expressions of type `Boolean` and may be used in any expression context, not only `if` conditions and `match` arms:

```txt
val isAdult = person has { age } && person["age"] >= 18
```

A string literal as a **value** (e.g. on either side of `is`) has base type `String`, not a singleton. `"Dave" is "Dave"` is true and is a runtime equality check; the type of the literal value `"Dave"` is `String`. A string literal in **type** position, however, *is* a singleton type — see §18 (tagged unions) and design principle §33. So `value is "Dave"` tests value equality, whereas `type Name = "Dave"` declares a type whose only inhabitant is the string `"Dave"`.

A single `match` arm may not combine `is` and `has` patterns — each arm uses one keyword.

### 13.2 `has`

`has` performs a **structural compatibility** check — the value contains *at least* the requested shape, but may have additional fields.

- For a named object type `T`, `value has T` is true if every field of `T` is **present** in `value` (its keys exist). Field *types* are **not** validated — that is what `is T` adds (§13.1). Extra fields are permitted.
- For an inline shape `{ a, b }`, `value has { a, b }` is true if `value` is an object containing at least those keys.

So `has` is the presence-only check and `is` is the presence-and-type check; both allow extra fields. Use `has` to test/destructure shape when the field types are already known or unimportant, and `is` when a successful match must guarantee the fields' types (e.g. before narrowing a `Json` value to a typed shape).

```txt
val describeNamed = (input: Json): String =>
  if input has { name } then "Named: ${input["name"]}"
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
  "${name} is ${age}"
```

## 16. Pattern Matching

Pattern matching is used to consume unions, inspect values, and destructure JSON-shaped data.

```txt
val describe = (input: String | Int32 | Null): String =>
  match input
    is Null =>
      "No value"

    is Int32 =>
      "Int32: ${input}"

    is String =>
      "String: ${input}"
```

### 16.1 `is` Patterns

`is` is the type-exact / shape-exact form. Its precise meaning depends on the pattern kind:

- **Primitive / literal / `Null`:** exact runtime-type or value match.
- **Named object type (`is Person`):** every declared field present and correctly typed, checked
  recursively; extra fields permitted (§13.1, ADR-054).
- **Array literal pattern (`is [a, b]`):** length-exact — the array must have exactly the listed
  number of elements.
- **Inline object pattern (`is { name }`):** the listed keys must be present (and any literal
  value-constraints satisfied); extra fields are permitted. (Inline `is { .. }` is a
  presence + value-constraint check, not a recursive field-*type* check — that is what a named
  object type `is T` adds.)

```txt
match input
  is Null => "No value"
  is "Dave" => "Big Dave!"
  is String => "String"
```

For arrays (length-exact):

```txt
match items
  is [] => "empty"
  is [one] => "exactly one item"
  is [first, second] => "exactly two items"
```

For objects (listed keys present; extra fields allowed):

```txt
match input
  is { name } => "has at least the field: name"
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
      "Old person: ${name}"

    has { name } =>
      "Young person: ${name}"

    is String =>
      "Name: ${input}"
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

Tagged unions are represented with structural JSON object types. The discriminant field uses a
**string-literal singleton type** (`"success"` / `"failure"`), so the tags are checked at compile
time: a literal in this position admits only its exact value, an object literal carrying the wrong
tag (or no tag) is a **compile-time type error**, and assigning a value to a `Result<…>` selects
the matching variant by its discriminant. The `match`/`has` arms then discriminate the variants at
runtime via the `"type"` field.

```txt
type Result<T, E> =
  | { "type": "success", "value": T }
  | { "type": "failure", "error": E }
```

Both the multi-line leading-`|` form above (the canonical spelling) and the equivalent single-line
form `type Result<T, E> = { "type": "success", "value": T } | { "type": "failure", "error": E }`
parse. In the multi-line form the leading `|` is optional on the first variant; a `|` may also
begin a continuation line.

```txt
val divide = (a: Float64, b: Float64): Result<Float64, String> =>
  if b == 0.0 then {
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
    "Result: ${value}"

  has { "type": "failure", error } =>
    "Error: ${error}"
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
std/string   trim, toUpper, toLower, substring, charAt, indexOf, length,
             contains, startsWith, endsWith, split, join, replace, repeat
std/number   parseInt32, parseFloat64, toInt32, toFloat64, isInt32
std/array    map, filter, reduce, find, some, every, flatMap, indexOf, reverse
std/iter     range, iterOf  (also auto-imported as globals)
std/result   Result<T, E> type alias
std/io       print, readLine, lines, readAll
std/fs       readFile, writeFile, appendFile, readLines, readJson, writeJson, exists
std/http     fetch, fetchWith, fetchJson, postJson,
             serve, json, text, redirect, notFound, badRequest, matchPath, parseBody
```

The full signature of every function is specified in `docs/STDLIB.md`.

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
&&  ||  !
&   |   ^   <<  >>  ~      (bitwise — see §35.2)
```

These are built-in operators, not ordinary functions. They are not available through dot application or partial application.

`+` operates only on numeric types. String building uses interpolation (`"${a}${b}"`) — see §3.5.2.

The bitwise operators `&`, `|`, `^`, `<<`, `>>` require integer operands; `~` is unary. They are specified in §35.2. In type-expression position `|` remains the union separator (§8.4); the two never overlap syntactically.

There are two unary operators: bitwise `~` (§35.2) and logical `!`. There is no unary minus (see §3.7 for negative literals). Logical `!b` requires a `Bool` operand and yields `Bool`:

```txt
val notReady = !ready
```

### 24.2 Precedence

Precedence follows the standard convention used by C-family languages, from highest to lowest:

```txt
1.  ()  []  .          (call, index, dot application)
2.  ~  !               (unary bitwise not, unary logical not; right-associative)
3.  *  /  %
4.  +  -
5.  <<  >>             (bitwise shift)
6.  <  <=  >  >=
7.  ==  !=
8.  &                  (bitwise and)
9.  ^                  (bitwise xor)
10. |                  (bitwise or)
11. &&
12. ||
```

All binary arithmetic, comparison, and bitwise operators are left-associative. `&&` and `||` are left-associative and short-circuiting. The unary operators `~` and `!` are right-associative and bind tighter than `*` but looser than postfix, so `!a == b` parses as `(!a) == b`.

## 25. Type Narrowing

`is`, `has`, `if`, and `match` may narrow union types.

```txt
val display = (input: String | Null): String =>
  if input is Null then "missing"
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

Explicit narrowing — assigning a wider numeric to a narrower one, or any floating-point to an integer — requires an explicit cast via stdlib and is a runtime error if the value cannot be represented exactly (for the float→int casts). Implicit narrowing is a compile-time error.

The explicit-narrowing mechanism is a family of `std/number` cast functions, each truncating to the named width with two's-complement (`as`-cast) semantics:

```txt
toInt32:  (Float64) => Int32      // truncate a float to a 32-bit int
toFloat64:(Int32)   => Float64    // widen
toUInt8 / toInt8:    (UInt64) => UInt8 / Int8       // integer narrowing
toUInt16 / toInt16:  (UInt64) => UInt16 / Int16
toUInt32 / toInt64:  (UInt64) => UInt32 / Int64
toUInt64:            (UInt64) => UInt64
```

The integer-narrowing casts take their input as `UInt64` (the widest unsigned), so any narrower *unsigned* integer — or a value first masked down to a byte/word — widens into the parameter without range loss before truncation; a bare integer literal in range is accepted directly. These are the byte-extraction primitives used by `std/bytes` (§35.3) and are generally useful wherever explicit width control is needed.

Literal inference: a numeric literal with an explicit type suffix (e.g. `5i64`, `3.14f32`, §3.6) is fixed at that type, overriding context — assigning it where an incompatible type is expected is a type error. A suffixless literal takes the type required by its surrounding context if one exists; otherwise integer literals default to `Int32` and floating-point literals default to `Float64`. An integer literal whose value exceeds `Int32`'s range but has no wider context does **not** truncate: its default widens to the smallest type that preserves the value (`Int64`, or `UInt64` for a decimal above `Int64`'s max). A literal that does not fit its required (context or suffix) type is a compile-time error, never a silent truncation.

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

v1 is pure beyond `print` and the concurrency primitives described in §32.

### 28.1 Reference Implementation

The reference implementation is written in **Rust** and laid out as a Cargo workspace:

```txt
lin-lang/
  Cargo.toml                 (workspace root)
  crates/
    lin-common/              shared types: Span, Diagnostic, intern table
    lin-lex/                 lexer, indentation tokenizer
    lin-parse/               parser, surface AST
    lin-check/               type checker, typed IR
    lin-ir/                  flat 3-address IR, liveness, RC elision
    lin-codegen/             LLVM backend via inkwell
    lin-runtime/             static library linked into every binary
    lin-compile/             compilation pipeline orchestration
    lin/                     the CLI binary
  stdlib/                    stdlib .lin files
  docs/
  examples/
```

The backend is the LLVM native-code compiler in `lin-codegen`. Source compiles to a standalone native binary via `lin build`.

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

1. **Bytecode or JIT target.** v1 compiles to native code via LLVM (§28.1). A bytecode or JIT target is deferred.
2. **Exact stdlib API.** Module layout is fixed (§20.6); precise signatures within each module are still being filled in incrementally.
4. **Tooling.** Formatter, LSP, test runner as a first-class command are deferred.
5. **Object rest destructuring iteration order.** Specified — see §4.3 and §27.5. Spread inserts source entries in source-iteration order; repeated keys keep their first-occurrence position.
6. **`Iterable<T>` mechanism.** Whether it is a true protocol-like type, a compiler-known structural capability, or purely a built-in opaque interface.
7. **Full numeric widening matrix.** §26 specifies the principle; the complete pairwise table is deferred.
8. **Multi-error reporting.** First-error-then-halt policy for v1; recoverable parsing/checking is deferred.

Decided:

1. `export` may be used on `val`, `var`, and `type` declarations.
2. `Json` is a built-in type; `Number` is a built-in union alias covering every numeric family (§4.2).
3. `Unknown` and `Never` are not built-in types initially.
4. `is Person` checks every declared field is present and correctly typed (recursively; extra fields allowed — ADR-054); `has Person` or `has { ... }` checks only that the requested fields are present (types not validated). Both allow extra fields; arrays match length-exactly (§16.1).
5. `is` on generic type applications is unsupported in v1.
6. `is`/`has` are expressions of type `Boolean` and may appear in any expression context.
7. A single `match` arm uses either `is` or `has`, not both.
8. Assignment expressions evaluate to the assigned value.
9. Operators are built-in, not ordinary functions. Exactly two unary operators — bitwise `~` and logical `!`; no unary minus (§24.1, §35.2, ADR-047).
10. `Iterator<T>` and `Iterable<T>` are opaque runtime types.
11. Arrays satisfy `Iterable<T>` automatically.
12. Array types are `T[]` (unbounded) and `[T1, T2, ...]` (fixed-length).
13. Strings use `"..."` with `${expr}` interpolation and standard escapes; UTF-8, length-prefixed; codepoint-aware indexing via stdlib (`at`).
14. Source files use the `.lin` extension; LF line endings only.
15. The language is compiled to native code via LLVM (`lin build`).
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
33. Literal types: a string literal in **type** position is a singleton type (`type Tag = "ok"` admits only the value `"ok"`). A string literal as a **value** still infers to its base type (`val x = "Dave"` is `String`); the singleton is obtained by checking it against an expected literal type. A literal widens to `String`; `String` does not narrow to a literal. (Numeric/boolean literal types are not yet supported.)
34. Runtime errors halt the program; they cannot be caught.
35. Integer division by zero is a runtime error; floating-point follows IEEE 754.
36. `toString` is defined for every primitive (§27.8); used implicitly by string interpolation.
37. `length()` works on `String`, `T[]`, and `Json` (array or object variants).
38. Comparison `<`, `<=`, `>`, `>=` uses codepoint order for strings, mathematical order for numbers.
39. Source files use LF line endings; CRLF is rejected.
40. Blank lines inside indented blocks are allowed and ignored.

## 32. Concurrency

### 32.1 Design Principles

Concurrency in Lin follows the same pattern as iteration: opaque runtime types constructed with built-in functions, consumed by built-in functions. No new syntax is introduced. Functions do not carry an "async" colour — whether a function runs synchronously or on a separate thread is decided at the call site, not in the function's definition.

### 32.2 `Promise<T>`

`Promise<T>` is an opaque runtime type representing a value of type `T` that is being computed on another OS thread.

`T` must be a **transferable** type: JSON-compatible values (`String`, `Boolean`, `Null`, all numeric types, `T[]`, and object types whose fields are transferable). The opaque types `Function`, `Iterator`, `Iterable`, `Worker`, `ThreadPool`, and `Promise` are not transferable. Attempting to spawn a thunk that returns a non-transferable type is a compile-time error where statically detectable, and a runtime error otherwise.

#### 32.2.1 Spawning

`async` spawns a thunk on a new OS thread and immediately returns a `Promise<T>`:

```txt
val p: Promise<Int32> = async(() => 1 + 1)
val p: Promise<Int32> = (() => 1 + 1).async()   // dot form
```

The thunk must be a zero-argument function: `() => T`.

A thunk may not capture `var` bindings from its enclosing scope. This is a compile-time error:

```txt
var count = 0

val p = async(() =>
  count = count + 1    // error: async thunk captures var binding 'count'
  count
)
```

A thunk may capture `val` bindings freely, including functions, provided the function itself does not close over `var` bindings. Functions that close over no `var` bindings are safe to share across threads.

```txt
val multiplier = 3
val p = async(() => multiplier * 10)   // ok: multiplier is a val

val addFive = (x: Int32) => x + 5     // no var captures
val p = async(() => addFive(10))       // ok
```

#### 32.2.2 Awaiting

`async` wraps its return type in `T | Error`, making the full type `Promise<T | Error>`. `await` blocks the calling thread until the promise resolves and returns the value:

```txt
val p = async(() => 1 + 1)           // Promise<Int32 | Error>
val result = await(p)                // Int32 | Error
val result = p.await()               // dot form

match result
  is Error => print("failed: ${result}")
  is Int32 => print("got ${result}")
```

An async fault surfaces as an `Error` value whose discriminant is the string-literal `"type": "error"`; since string literals in type position are singleton types (§18, §33), this tag is the same kind of compile-time-checked discriminant used by user-defined tagged unions. A runtime error inside the thunk (array out of bounds, integer division by zero, non-exhaustive match, etc.) is caught at the OS thread boundary and surfaces as an `Error` value at the `await` call site rather than halting the program. This makes `async` a **fault isolation boundary** — the only place in Lin where runtime errors become recoverable values. The general rule that runtime errors are uncatchable (§19.1) does not apply inside an async thunk.

If `await` is called and the result is `Error` but the caller does not inspect it (e.g. assigns to a `val` typed as `Int32`), the type checker will reject the assignment at compile time:

```txt
val p = async(() => 1 + 1)
val v: Int32 = await(p)   // compile error: Int32 | Error is not assignable to Int32
```

This is enforced. To consume an awaited result you must handle the `Error` case (typically with `match … is Error => … else => …`).

> **Implementation note (ADR-070).** Conceptually `async` produces a `Promise<T | Error>`. In the reference compiler the `T | Error` union is attached at `await` (typed `<T>(p: T): T | Error`), not at `async` — a promise handle in flight is an opaque value, and only `await` materialises a result that can be an `Error`. The observable rule is identical: you cannot use an awaited value without handling the `Error`. There is no nominal `Promise<T>` type, so the checker enforces "must handle the `Error`" but does not catch "forgot to `await`" (using a promise where its resolved value is expected).

#### 32.2.3 Nested Promises

`await` auto-flattens nested promises. If the thunk itself returns a `Promise<T>`, `await` resolves through all layers. The result is still `T | Error` (the union does not nest — `await` flattens both the promise layers and any inner `Error`):

```txt
val p = async(() => async(() => 42))
val v = await(p)   // Int32 | Error — 42 once the Error case is handled, not Promise<Int32>
match v
  is Error => print("failed")
  else     => print("${v}")   // 42
```

### 32.3 `parallel`

`parallel` is syntactic sugar for spawning an array of thunks and awaiting all results. It is the idiomatic fork/join form:

```txt
val [a, b, c] = parallel(
  () => expensiveA(),
  () => expensiveB(),
  () => expensiveC()
)
```

This is exactly equivalent to:

```txt
val [a, b, c] = await([
  async(() => expensiveA()),
  async(() => expensiveB()),
  async(() => expensiveC())
])
```

Result order matches input order regardless of completion order.

`await` on a `Promise<T>[]` also works directly:

```txt
val [a, b] = await([myFunc, myFunc2].map(f => async(f)))
```

The thunks in `parallel` are subject to the same `var`-capture restriction as `async` (§32.2.1).

### 32.4 Promise Combinators

These are built-in functions on `Promise<T>`. All return a new `Promise`:

```txt
map:     <T, U>(Promise<T | Error>, (T) => U) => Promise<U | Error>
race:    <T>(Promise<T | Error>[]) => Promise<T | Error>
timeout: <T>(Promise<T | Error>, Int32) => Promise<T | Error | Null>
retry:   <T>(() => T, Int32) => Promise<T | Error>
```

**`map`** — transforms the resolved value without blocking:

```txt
val doubled: Promise<Int32> = async(() => 21).map(v => v * 2)
val v = await(doubled)   // 42
```

**`race`** — resolves with the first promise to complete; the others continue running but their results are discarded:

```txt
val first = race([
  async(() => slowFetch("https://mirror-a/data")),
  async(() => slowFetch("https://mirror-b/data"))
])
val data = await(first)
```

**`timeout`** — resolves with the original value if the promise completes within the given number of milliseconds, or `Null` if it does not. The timed-out thread is abandoned (not cancelled — Lin has no cancellation in v1). The full result type is `T | Error | Null`:

```txt
val result: String | Error | Null = await(timeout(p, 5000))

match result
  is Null  => print("timed out")
  is Error => print("failed")
  is String => print(result)
```

**`retry`** — spawns the thunk up to `n` times, returning the first result that is not an `Error`. If all attempts return `Error`, the last `Error` is the result:

```txt
val p = retry(() => unreliableFetch(), 3)
val data = await(p)
```

### 32.5 `ThreadPool`

By default each `async` call spawns a new OS thread. For high-fan-out work, a `ThreadPool` distributes tasks across a fixed number of threads:

```txt
threadPool: (Int32) => ThreadPool
```

```txt
val pool = threadPool(8)

// Single thunk on the pool
val p = pool.async(() => work())

// Array of thunks distributed across the pool
val results = await(pool.async([() => work(1), () => work(2), () => work(3)]))
```

`pool.async` has the same two overloads as the top-level `async`: single thunk `() => T` and array of thunks `(() => T)[]`. The same `var`-capture restriction applies (§32.2.1).

`ThreadPool` also provides `pool.serve` for multi-threaded HTTP servers (§33.5). The handler is dispatched to pool threads on each request, subject to the same `var`-capture restriction as `pool.async`.

A `ThreadPool` is an opaque runtime value. It is not transferable across async boundaries.

### 32.6 Workers

A `Worker<Msg, Reply>` is a long-lived OS thread that processes messages sequentially. It is the right primitive for stateful concurrency (shared counters, connection pools, caches) and for isolating long-running background tasks.

#### 32.6.1 Construction

```txt
worker: <Msg, Reply>(
  (Msg) => Reply,
  () => Null
) => Worker<Msg, Reply>
```

The first argument is the message handler. The second is a shutdown handler called once when `close()` is invoked. Both run on the worker's thread.

```txt
val onMessage = (msg: String): Null =>
  print("Got ${msg}")

val onShutdown = (): Null =>
  print("shutting down")

val w: Worker<String, Null> = worker(onMessage, onShutdown)
```

#### 32.6.2 Sending Messages

`message` is fire-and-forget — it enqueues the message and returns immediately:

```txt
message: <Msg, Reply>(Worker<Msg, Reply>, Msg) => Null

w.message("Hello")
```

`request` is synchronous — it enqueues the message and blocks until the handler returns, then returns the reply:

```txt
request: <Msg, Reply>(Worker<Msg, Reply>, Msg) => Reply

val reply: String = w.request("ping")
```

The handler's return value is the reply. If the handler is typed `(Msg) => Null`, `request` and `message` are equivalent (the reply is `Null`).

#### 32.6.3 Closing

```txt
close: <Msg, Reply>(Worker<Msg, Reply>) => Null

w.close()
```

`close` waits for any in-progress message to finish, calls `onShutdown`, then terminates the worker thread. Sending a message or request to a closed worker is a runtime error.

#### 32.6.4 Worker State and `var`

A worker's `onMessage` handler may close over `var` bindings to maintain state across messages. This is safe because the worker is single-threaded: messages are processed one at a time, with no concurrent access to the worker's closed-over state.

```txt
val makeCounter = (): Worker<String, Int32> =>
  var count = 0

  worker(
    (msg: String) =>
      count = count + 1
      count,

    () => null
  )

val counter = makeCounter()
val n1 = counter.request("tick")   // 1
val n2 = counter.request("tick")   // 2
```

#### 32.6.5 Worker Lifetime and Errors

A runtime error inside a message handler kills the worker. The current `request` call (if any) causes the program to halt with the worker's diagnostic. Subsequent `message` or `request` calls to a dead worker are also runtime errors.

#### 32.6.6 Transferability

`Msg` and `Reply` must be transferable types (§32.2). Functions that close over no `var` bindings may be sent as messages.

### 32.7 `print` Ordering

All workers and async thunks share a single stdout. `print` is line-atomic: a full line is written without interleaving with output from other threads. Partial output within a single `print` call will not be split.

### 32.8 Summary Table

| Primitive | Use case | Blocks caller? |
| --- | --- | --- |
| Primitive | Use case | Blocks caller? | Return type |
| --- | --- | --- | --- |
| `async(f)` | Spawn one thunk, retrieve later | No (until `await`) | `Promise<T \| Error>` |
| `await(p)` | Block until promise resolves | Yes | `T \| Error` |
| `parallel(f1, f2, ...)` | Fork/join, all results needed | Yes | `[T \| Error, ...]` |
| `race(ps)` | First result wins | No (until `await`) | `Promise<T \| Error>` |
| `timeout(p, ms)` | Bound wait time | No (until `await`) | `Promise<T \| Error \| Null>` |
| `retry(f, n)` | Retry on runtime error | No (until `await`) | `Promise<T \| Error>` |
| `threadPool(n).async(...)` | High-fan-out work, bounded threads | No (until `await`) | `Promise<T \| Error>` |
| `worker(onMsg, onShutdown)` | Long-lived stateful thread | No | `Worker<Msg, Reply>` |
| `w.message(x)` | Fire-and-forget message | No | `Null` |
| `w.request(x)` | Synchronous request/reply | Yes | `Reply` |

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
  if input.isInt32() then {
    "type": "success",
    "value": input.toInt32()
  }
  else {
    "type": "failure",
    "error": "Invalid age"
  }
```

## 33. IO, Filesystem, and HTTP Intrinsics

The functions in `std/io`, `std/fs`, and `std/http` cannot be implemented in Lin because they require OS-level syscalls and network access. They are registered as host-language (Rust) intrinsics and exposed to Lin programs via the module system exactly like any other export. Their signatures are specified here.

### 33.1 Design Principles

All three modules follow the same conventions:

1. **Blocking by default.** Every function runs synchronously. Use `async` at the call site when concurrency is needed.
2. **`T | Error` for fallible operations.** A function that may fail at the OS or network level returns a `Result`-shaped value (`{ "type": "success", "value": T } | { "type": "failure", "error": String }`). HTTP error status codes are not transport errors and do not produce `Error`.
3. **`Iterator` for sequences.** Line-oriented reads return iterators rather than loading everything into memory.
4. **No hidden global state.** Stdin, stdout, and the filesystem are the implicit context; there are no open-handle values exposed to user code.

### 33.2 `std/io` Intrinsics

The following are implemented as Rust intrinsics. `print` is additionally available as a global without importing.

```txt
__ioPrint:    (Json)   -> Null      // formats and writes to stdout + newline
__ioReadLine: ()       -> String | Null   // one line from stdin, Null on EOF
__ioLines:    ()       -> Iterator        // iterator over stdin lines
__ioReadAll:  ()       -> String          // all of stdin as one string
```

The Lin stdlib wrappers in `std/io` delegate directly to these intrinsics:

```txt
export val print    = (v: Json): Null           => __ioPrint(v)
export val readLine = (): String | Null         => __ioReadLine()
export val lines    = (): Iterator              => __ioLines()
export val readAll  = (): String                => __ioReadAll()
```

### 33.3 `std/fs` Intrinsics

```txt
__fsReadFile:   (path: String)                    -> String | Error
__fsWriteFile:  (path: String, content: String)   -> Null | Error
__fsAppendFile: (path: String, content: String)   -> Null | Error
__fsReadLines:  (path: String)                    -> Iterator | Error
__fsReadJson:   (path: String)                    -> Json | Error
__fsWriteJson:  (path: String, value: Json)       -> Null | Error
__fsExists:     (path: String)                    -> Boolean
```

The Lin stdlib wrappers in `std/fs` delegate directly:

```txt
export val readFile   = (path: String): String | Error          => __fsReadFile(path)
export val writeFile  = (path: String, content: String): Null | Error  => __fsWriteFile(path, content)
export val appendFile = (path: String, content: String): Null | Error  => __fsAppendFile(path, content)
export val readLines  = (path: String): Iterator | Error        => __fsReadLines(path)
export val readJson   = (path: String): Json | Error            => __fsReadJson(path)
export val writeJson  = (path: String, value: Json): Null | Error => __fsWriteJson(path, value)
export val exists     = (path: String): Boolean                 => __fsExists(path)
```

### 33.4 `std/http` Intrinsics

```txt
type HttpResponse = {
  "status":  Int32,
  "headers": { ...String },
  "body":    String
}

type HttpOptions = {
  "method":  String,
  "headers": { ...String },
  "body":    String
}
```

```txt
__httpFetch:     (url: String)                          -> HttpResponse | Error
__httpFetchWith: (url: String, options: HttpOptions)    -> HttpResponse | Error
```

The higher-level functions `fetchJson` and `postJson` are written in Lin on top of these two intrinsics:

```txt
export val fetch     = (url: String): HttpResponse | Error          => __httpFetch(url)
export val fetchWith = (url: String, opts: HttpOptions): HttpResponse | Error =>
  __httpFetchWith(url, opts)

export val fetchJson = (url: String): Json | Error =>
  match __httpFetch(url)
    is { "type": "failure", "error": e } => { "type": "failure", "error": e }
    is { "type": "success", "value": resp } =>
      if resp["status"] >= 200 && resp["status"] < 300 then
        parseJson(resp["body"])
      else
        { "type": "failure", "error": "HTTP ${resp["status"]}" }

export val postJson = (url: String, body: Json): HttpResponse | Error =>
  __httpFetchWith(url, {
    "method":  "POST",
    "headers": { "Content-Type": "application/json" },
    "body":    toString(body)
  })
```

`parseJson` is an intrinsic (`__parseJson: (String) -> Json | Error`) that parses a JSON string into a Lin value. It is not part of the public stdlib API but is available internally to `std/http`.

### 33.5 HTTP Server (`serve`)

Server support lives in `std/http` alongside the client. The serving loop itself is the one intrinsic; everything else (`json`, `text`, `redirect`, `notFound`, `badRequest`, `matchPath`, `parseBody`) is written in Lin on top of the `HttpResponse` type and `__parseJson`.

```txt
__serverServe: (handler: (HttpRequest) -> HttpResponse, port: Int32) -> Null
```

`__serverServe` binds a TCP listener on `port`, then serves connections **sequentially** (one request at a time): it parses each incoming HTTP/1.1 request into an `HttpRequest`, invokes `handler`, and writes the returned `HttpResponse` back on the wire. It blocks indefinitely (it only returns — as an `Error`-shaped value — if the port cannot be bound). The handler runs inside a fault-isolation boundary: a faulting handler yields a `500` response and the server keeps serving.

The handler argument comes **first** so the dot-call form reads naturally: `router.serve(3000)` desugars (first-argument application, §11.1) to `serve(router, 3000)`. Both forms are equivalent.

A pool-dispatched variant (`pool.serve`, concurrent request handling) is not yet implemented.

The Lin stdlib wrappers in `std/http`:

```txt
export val serve = (handler: (HttpRequest) -> HttpResponse, port: Int32): Null =>
  __serverServe(handler, port)

export val json = (status: Int32, body: Json): HttpResponse => {
  "status":  status,
  "headers": { "Content-Type": "application/json" },
  "body":    toString(body)
}

export val text = (status: Int32, body: String): HttpResponse => {
  "status":  status,
  "headers": { "Content-Type": "text/plain; charset=utf-8" },
  "body":    body
}

export val redirect = (url: String): HttpResponse => {
  "status":  302,
  "headers": { "Location": url },
  "body":    ""
}

export val notFound: HttpResponse =
  { "status": 404, "headers": {}, "body": "Not Found" }

export val badRequest = (message: String): HttpResponse =>
  { "status": 400, "headers": {}, "body": message }

export val parseBody = (req: HttpRequest): Json | Error =>
  __parseJson(req["body"])

export val matchPath = (path: String, pattern: String): { ...String } | Null =>
  __serverPathMatch(pattern, path)
```

`__serverPathMatch` is a Rust intrinsic that splits both strings on `/`, matches literal segments exactly, captures `:name` segments by name, and returns `Null` on any mismatch. `matchPath` takes the **path first** so it reads as `req["path"].matchPath("/users/:id")` in dot-call form.

## 34. Foreign Function Interface

Lin provides a C-compatible FFI so that programs can call into native libraries written in C or Rust.

### 34.1 Design Principles

1. **C ABI only.** Lin speaks the C calling convention. C libraries are called directly. Rust libraries must expose their public API as `extern "C"` functions (with `#[no_mangle]`).
2. **Explicit, flat signatures.** Only a restricted set of Lin types are legal in `foreign` signatures — those that map cleanly onto C types. Richer Lin values cannot cross the boundary without explicit conversion.
3. **Static linking.** Foreign declarations are resolved at `lin build` time by the linker. There is no runtime `dlopen`.
4. **Unsafe by nature.** The compiler trusts the declared types. A mismatch between the declared Lin type and the actual C signature is undefined behaviour. It is the programmer's responsibility to get it right.

### 34.2 `import foreign` Syntax

A foreign import names the library and declares the symbols it provides.

```txt
import foreign "./libmath.a"
  val sqrt: (Float64) => Float64
  val pow:  (Float64, Float64) => Float64
```

The library path is a string literal on the same line as `import foreign`. Each subsequent indented line declares one binding as `val name: Type`. The indented block ends when indentation returns to the `import` level.

Multiple foreign imports are allowed in a single file:

```txt
import foreign "./libfoo.a"
  val fooInit: () => Null
  val fooProcess: (String, Int32) => Int32

import foreign "./libbar.a"
  val barVersion: () => String
```

Foreign bindings are used exactly like any other function in scope:

```txt
val result = pow(2.0, 10.0)
```

### 34.3 Legal Foreign Types

Only the following types are legal in `import foreign` signatures:

| Lin type                    | C equivalent                        |
| ---                         | ---                                 |
| `Int8`                      | `int8_t`                            |
| `Int16`                     | `int16_t`                           |
| `Int32`                     | `int32_t`                           |
| `Int64`                     | `int64_t`                           |
| `UInt8`                     | `uint8_t`                           |
| `UInt16`                    | `uint16_t`                          |
| `UInt32`                    | `uint32_t`                          |
| `UInt64`                    | `uint64_t`                          |
| `Float32`                   | `float`                             |
| `Float64`                   | `double`                            |
| `Boolean`                   | `uint8_t` (0 = false, 1 = true)     |
| `Null` (return type only)   | `void`                              |
| `String`                    | `LinString` (pointer + length, see §34.4) |

All other Lin types (`Json`, object types, array types, `Iterator`, `Function`, etc.) are not legal in foreign signatures. Attempting to declare one is a compile-time error.

### 34.4 String Passing Convention

Lin strings are UTF-8 length-prefixed values and do not carry a null terminator. Passing a `String` across the FFI boundary uses the `LinString` struct, which the C header `lin.h` defines as:

```c
typedef struct {
    const uint8_t *ptr;
    size_t         len;
} LinString;
```

The C function receives a `LinString` by value. The pointed-to bytes are owned by the Lin runtime and must not be freed or stored past the function call. If the C side needs to retain the data it must copy it.

Returning a `String` from a foreign function is not supported in v1. A function that needs to return text should write into a caller-supplied buffer or use an `Int32` return code and a side channel.

### 34.5 Rust Libraries

A Rust crate exposes FFI-compatible functions by:

1. Adding `crate-type = ["staticlib"]` (or `"cdylib"`) to its `Cargo.toml`.
2. Marking each exported function `#[no_mangle] pub extern "C"`.
3. Using only C-compatible types (`i32`, `f64`, `*const u8` + `usize` for strings, etc.).

Example Rust side:

```rust
#[no_mangle]
pub extern "C" fn add_ints(a: i32, b: i32) -> i32 {
    a + b
}
```

Lin side:

```txt
import foreign "./libadd.a"
  val addInts: (Int32, Int32) => Int32
```

The `lin build` command must be given the path to the compiled `.a` or `.so` file; it passes it to the linker as a `-l` flag.

### 34.6 Static Analysis

The type checker treats every foreign binding as having the declared type and performs no further checking of the library contents. Foreign signatures participate in the normal type system — the declared argument and return types are enforced at every call site in Lin code.

## 35. Low-Level Primitives

Lin's domain includes low-level systems code (binary protocols, byte parsing, sockets, subprocesses). This section specifies the primitives that make such code expressible: byte buffers, bitwise operators, and a small family of OS intrinsics. They follow the existing conventions — opaque scalar handles, the `T | Error` result shape, and stdlib wrappers over Rust intrinsics — and introduce no new runtime *kinds* beyond what the unboxed-array and FFI machinery already provide.

### 35.1 Byte Buffers and Small-Integer Arrays

The small integer families `Int8`, `UInt8`, `Int16`, `UInt16` have an unboxed, contiguous array representation, exactly like `Int32`/`Int64`/`Float32`/`Float64` (§27.4). An array typed `UInt8[]` is a packed byte buffer — one byte per element, no per-element tag.

```txt
val packet: UInt8[] = [0u8, 1u8, 255u8]
val b = packet[0]            // UInt8
packet[1] = 42u8             // in-place write (§6, index assignment)
val n = length(packet)       // Int32
```

These arrays support every array operation (literals, indexing, in-place index assignment, `length`, `push`, the `std/array` combinators, equality). The representation is an implementation detail; semantically they are ordinary `T[]` arrays whose element type happens to be a small integer.

### 35.2 Bitwise Operators

Lin provides the bitwise binary operators and one unary operator:

```txt
&    bitwise and
|    bitwise or        (value position; in type position `|` is the union separator)
^    bitwise xor
<<   left shift
>>   right shift       (logical for unsigned types, arithmetic for signed)
~    bitwise not       (unary)
```

There are two unary operators in the language: bitwise `~` (here) and logical `!` (§24.1). They are the exceptions to the "no unary minus" rule of §3.7/§24.1.

**Typing.** Bitwise and shift operators require **integer** operands; a floating-point operand is a compile-time error. For `&`, `|`, `^`, the result type is the widened integer type of the two operands (§26). For `<<` and `>>`, the result type is the type of the left operand and the right operand may be any integer. For `~x`, the result type is the type of `x`. The logical-not operator `!x` requires a `Bool` operand and yields `Bool`.

**Precedence.** The new operators slot into the §24.2 ladder as shown there: shifts bind tighter than comparison; `&`, `^`, `|` bind between equality and `&&`, in that order (tightest first). `~` and `!` bind tighter than `*` (and are right-associative).

```txt
val nalType = header & 0x1F            // extract low 5 bits
val fuHeader = nri | 28                // set FU-A type bits
val flagged = fuHeader | 0x80          // set start bit
val high = (value >> 24) & 0xFF        // top byte of a UInt32
val inverted = ~mask                   // bitwise complement
```

`|` is unambiguous because type expressions and value expressions never overlap syntactically; the parser knows which context it is in.

### 35.3 `std/bytes`

`std/bytes` provides slicing and endian (de)serialization. The endian helpers are written in Lin on top of §35.1 and §35.2, **plus the explicit narrowing casts of §26** (exported from `std/number`). The earlier claim that the endian helpers are pure shift-and-mask was incomplete: extracting a byte from a wider integer — e.g. `(v >> 24) & 0xFF` for a `UInt32` `v` — yields a `UInt32`, which cannot be *implicitly* narrowed to a `UInt8` (§26 makes implicit narrowing a compile-time error), so an explicit `toUInt8(...)` cast is required. Conversely, assembling a wide integer from bytes widens each byte first (`toUInt32(b[off]) << 24 | ...`). The four float bit-reinterpret functions are the only true intrinsics here (a float's bit pattern cannot be obtained by shift-and-mask).

```txt
slice:       (UInt8[], Int32, Int32) => UInt8[]   // also exported from std/array; sub-buffer copy

u16FromBe / u32FromBe / u64FromBe:  (UInt8[], Int32) => UIntN     // read big-endian at offset
u16ToBe   / u32ToBe   / u64ToBe:    (UIntN) => UInt8[]            // write big-endian
// little-endian variants: u16FromLe, u32FromLe, u64FromLe, u16ToLe, u32ToLe, u64ToLe

f32ToBits:   (Float32) => UInt32        // intrinsic: bit reinterpret
f32FromBits: (UInt32) => Float32
f64ToBits:   (Float64) => UInt64
f64FromBits: (UInt64) => Float64

f32ToBe / f32ToLe:     (Float32) => UInt8[]          // compose bits + endian write
f32FromBe / f32FromLe: (UInt8[], Int32) => Float32   // compose endian read + bits
f64ToBe / f64ToLe:     (Float64) => UInt8[]
f64FromBe / f64FromLe: (UInt8[], Int32) => Float64
```

The narrowing casts that back the byte-extraction live in `std/number` (§26): `toUInt8`, `toInt8`, `toUInt16`, `toInt16`, `toUInt32`, `toInt64`, `toUInt64`, each `(UInt64) => <target>`, truncating with two's-complement (`as`-cast) semantics.

Slicing is a function, `slice(buf, start, end)`; there is no range-index syntax (`buf[a..b]`) in this version. `slice` preserves element type — slicing a `UInt8[]` yields a `UInt8[]`.

### 35.4 OS Handle Convention

Operating-system resources (sockets, subprocesses) are exposed to Lin as **opaque integer handles**, not as runtime object values. A handle is an `Int32` (or `Int64`) that the runtime interprets; there are no open-handle objects in user code (consistent with §33.1). This is the same convention `std/time` uses for timers.

All fallible operations return the `T | Error` result shape (§33.1). A non-blocking read that has no data available yet returns `Null` rather than `Error`, so a poll loop reads naturally.

### 35.5 `std/net` — Sockets

Both UDP and TCP sockets are exposed via runtime intrinsics. Every socket is an opaque integer fd handle (§35.4), and every fallible call returns the `T | Error` result shape; a non-blocking read with no data available yet returns `Null`.

**UDP** is connectionless — bind, then send/receive datagrams with explicit peer addresses:

```txt
udpBind:           (port: Int32)                              => Int32 | Error    // fd handle
udpRecv:           (fd: Int32, buf: UInt8[])                  => Int32 | Null | Error  // bytes read; Null = would-block
udpRecvFrom:       (fd: Int32, buf: UInt8[])                  => { "len": Int32, "addr": String, "port": Int32 } | Null | Error
udpSendTo:         (fd: Int32, addr: String, port: Int32, buf: UInt8[]) => Int32 | Error
udpSetNonblocking: (fd: Int32, on: Boolean)                   => Null | Error
udpClose:          (fd: Int32)                                => Null | Error
```

**TCP** is connection-oriented. A listener accepts connections, each of which is itself an fd; a client connects directly. Reads and writes operate on a connected fd:

```txt
tcpListen:         (port: Int32)                  => Int32 | Error            // listener fd
tcpAccept:         (fd: Int32)                    => { "fd": Int32, "addr": String, "port": Int32 } | Null | Error  // Null = would-block
tcpConnect:        (host: String, port: Int32)    => Int32 | Error            // connected fd
tcpRecv:           (fd: Int32, buf: UInt8[])       => Int32 | Null | Error      // bytes read; 0 = peer closed; Null = would-block
tcpSend:           (fd: Int32, buf: UInt8[])       => Int32 | Error            // bytes written
tcpSetNonblocking: (fd: Int32, on: Boolean)       => Null | Error
tcpClose:          (fd: Int32)                    => Null | Error
```

`recv` fills a caller-owned `UInt8[]` (§35.1) and returns the number of bytes read; the buffer is never transferred across the boundary. Non-blocking mode plus a `Null`-on-would-block `recv`/`accept` replaces an explicit `poll`.

Note that `std/http` already provides a high-level blocking HTTP server (`serve`, §33.5) and an HTTP client (§33.4); `std/net` is the lower-level byte-stream layer beneath them, for non-HTTP protocols and custom framing.

### 35.6 `std/process` — External Processes

Two styles share one module. **Batch** runs a command to completion and collects its full output; **streaming** spawns a child and reads its stdout incrementally. `ProcessHandle` is an opaque `Int64` id (not an OS pid).

```txt
type ExecResult = { "status": Int32, "stdout": String, "stderr": String }

// batch
exec:        (command: String, args: String[]) => ExecResult | Error
shell:       (command: String)                 => ExecResult | Error   // via /bin/sh -c
cwd:         ()                                 => String
chdir:       (path: String)                     => Null | Error
// streaming
spawn:       (command: String, args: String[]) => ProcessHandle | Error
readStdout:  (handle: ProcessHandle, buf: UInt8[]) => Int32 | Error     // bytes; 0 = EOF
kill:        (handle: ProcessHandle)            => Null | Error
wait:        (handle: ProcessHandle)            => Int32 | Error         // exit code
```

### 35.7 `std/tty` — Raw Terminal

```txt
rawMode:  (on: Boolean)  => Null | Error    // enable/disable terminal raw mode
readKey:  ()             => Int32 | Null    // keycode, or Null if no key available (non-blocking)
```

### 35.8 Timing and Signals

`std/time` gains microsecond sleep (the existing `sleep` is millisecond-granularity):

```txt
sleepMicros: (n: Int64) => Null
```

`std/signal` provides minimal signal handling:

```txt
waitSignal: (sig: Int32) => Int32           // block until the signal is delivered
```

### 35.9 What Is Deliberately Absent

Two systems facilities are **not** provided as core primitives, by design:

- **GPIO / hardware register access.** Use the C FFI (§34) to bind a native GPIO library. The only language-level support added for it is `sleepMicros` (§35.8), needed for software PWM timing.
- **Shared-memory concurrency** (mutexes, atomics, shared mutable cells across threads). Lin's concurrency is share-nothing (§32). Cross-thread mutable state is modelled with a `Worker<Msg, Reply>` (§32.6) that owns the state and serialises access through its message queue. This preserves the share-nothing invariant rather than reintroducing data races.
