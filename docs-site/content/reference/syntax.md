# Syntax

This page is the lexical and surface-syntax reference for Lin: how source text is tokenised, how blocks are formed, and how literals are written. For the semantics of types, see [Types](types.html); for the binding forms `val`/`var`, see [Bindings & Scope](bindings.html).

## Source encoding

- Source files use **UTF-8** text and **LF** line endings. CRLF is rejected with a diagnostic, and mixed line endings are an error.
- Indentation is always **two spaces per level**. Tabs are not permitted for indentation.

## Comments

Lin has **line comments only**. A comment begins with `//` and runs to the end of the line. There are no block comments.

```lin
// This is a comment
val x = 1 // This is also a comment
```

## Significant indentation

Indentation defines blocks. A block is introduced when a construct's body is written on indented continuation lines, and it ends when the indentation returns to the enclosing level. A block evaluates to its **final expression** — Lin is expression-based, so there is no `return` (see [Statement vs expression orientation](#statement-vs-expression-orientation)).

A single-expression function body may sit on the next line, indented one level:

```lin
val add = (a: Int32, b: Int32): Int32 =>
  a + b
```

A multi-statement block lists its bindings, then ends with the expression it evaluates to:

```lin
val calculate = (n: Int32): Int32 =>
  val doubled = n * 2
  doubled + 1
```

`if`/`then`/`else` and `match` follow the same rule. `then` appears on the condition line; each branch is an indented block; `else` returns to the `if`'s indentation:

```lin
val classify = (n: Int32): String =>
  if n < 0 then
    "negative"
  else
    "non-negative"
```

Blank lines are permitted anywhere inside a block and do not affect block structure.

### Indentation is suppressed inside brackets

Indentation tracking (the synthetic indent/dedent tokens that open and close blocks) is **suppressed inside `( )`, `[ ]`, and `{ }`**. While the lexer is inside any of these delimiters, newlines and indentation carry no block meaning. This is why bracketed expressions — and especially JSON object and array literals — can span as many lines as you like, with any indentation, without being parsed as a block:

```lin
val config = {
  "host": "localhost",
  "port": 8080,
  "tags": [
    "web",
    "api"
  ]
}
```

The practical consequence: you cannot use indentation-significant syntax (a multi-statement block) directly inside an object or array literal — the values there are plain expressions, not blocks.

### Continuation lines

A logical line may continue on the next line when the continuation begins with `&&` or `||`. The continuation must be indented at least one level deeper than the line it continues:

```lin
val isAdultBob = (person: { "age": Int32, "name": String, "active": Boolean }): Boolean =>
  person["age"] >= 18
    && person["name"] == "Bob"
    && person["active"]
```

A **dot chain** may likewise continue on the next line: a line beginning with `.` continues the chain on the value above it. This is the idiomatic way to lay out a pipeline of combinators:

```lin
import { map, filter } from "std/iter"

val pipeline = (xs: Int32[]): Int32[] =>
  xs
    .map(n => n * 2)
    .filter(n => n > 2)
```

## Literals

### Strings

Strings are delimited with double quotes. They may span multiple lines; newlines inside the literal are preserved verbatim.

```lin
val name = "Bob"
val poem = "Roses are red,
Violets are blue."
```

The recognised escape sequences are:

| Escape | Meaning |
| --- | --- |
| `\"` | double quote |
| `\\` | backslash |
| `\n` | newline |
| `\r` | carriage return |
| `\t` | tab |
| `\0` | null character |
| `\u{HHHH}` | unicode codepoint (1–6 hex digits) |

### String interpolation

Strings support interpolation with `${ expression }`. The expression is evaluated in the surrounding scope and converted to a string via `toString`. Interpolation is the **only** way to build strings from parts — there is no string `+`.

```lin
val name = "Bob"
val age = 42
val greeting = "Hello ${name}, you are ${age + 1} next year"
```

An interpolated expression may contain string literals, function calls, and arbitrary expressions, but it cannot span multiple statements. To write a literal `$` immediately before `{`, escape it: `\$` (or `\${` for a literal `${`).

Internally an interpolated string is a single compound token whose embedded expressions carry their own sub-token-streams, so the compiler sees all parts as one node and can allocate the result exactly once.

### Numbers

Integer literals may be written in several bases, with `_` as a visual separator (no semantic effect):

```lin
val dec = 42
val hex = 0xFF
val bin = 0b1010
val oct = 0o755
val big = 1_000_000
```

Floating-point literals may include an exponent and underscores:

```lin
val pi = 3.14
val scaled = 3.14e2
val avogadro = 6.022e23
```

A literal may carry a **type suffix** to override default inference:

| Suffix | Type |
| --- | --- |
| `i8` / `i16` / `i32` / `i64` | signed integer of that width |
| `u8` / `u16` / `u32` / `u64` | unsigned integer of that width |
| `f32` / `f64` | floating-point of that width |

```lin
val tiny = 42i8
val flags = 7u32
val ratio = 3.14f32
```

Without a suffix, integer literals default to `Int32` (widening to a larger type only if the value does not fit) and floating-point literals default to `Float64`. Context can resize a bare literal — `val x: Int64 = 42` types `42` as `Int64`. See [Types](types.html) for the full numeric-literal typing rules.

### Negative literals and leading `-`

A leading `-` is part of a numeric literal when there is no whitespace between the `-` and the digits **and** the previous token cannot end an expression (e.g. it is `(`, `,`, `=`, `=>`, `:`, an operator, or a keyword like `then`/`else`). Otherwise `-` is binary subtraction.

```lin
val negs = (x: Int32): Null =>
  val temperature = -5     // literal
  val delta = x - 5        // subtraction
  val negated = -x         // sugar for 0 - x
  null
```

A leading `-` on a non-literal expression is parse-time sugar for `0 - x`; it is not a distinct unary operator.

### Booleans and null

```lin
val active = true
val done = false
val missing = null
```

`true` and `false` have type `Boolean`; `null` has type `Null`.

### Arrays and objects

Array literals use `[ … ]`; object literals use `{ … }` with **quoted string keys**:

```lin
val names = ["Alice", "Bob"]
val person = { "name": "Bob", "age": 42 }
```

(Destructuring patterns allow a bare-name shorthand for keys; object *literals* always require quoted keys. See [Bindings & Scope](bindings.html).)

## Statement vs expression orientation

Lin is **expression-based**. Nearly every construct produces a value:

- `if`/`then`/`else` is an expression — its value is the chosen branch.
- `match` is an expression — its value is the matched arm.
- A block evaluates to its final expression.
- An assignment (`count = count + 1`, `m[k] = v`) evaluates to the assigned value.

There is no `return` keyword and no statement-level control flow: there are no `for`/`while`/`switch` keywords. Iteration is done with ordinary functions (`map`, `filter`, `reduce`, `for`, `range`, `while`) applied through the dot — see the standard-library iteration docs. The forms that *are* statements are the declarations: `val`, `var`, `type`, `import`, `export`, and the test-only `replace`.

## Identifiers and naming

Identifiers name values, functions, type names, imports, destructuring bindings, and local bindings.

- **Types** (built-in and user-defined) conventionally use `CamelCase`: `String`, `Boolean`, `AnyVal`, `Int32`, `Iterator<T>`, `Person`.
- **Values and functions** conventionally use `lowerCamelCase`: `substring`, `indexOf`, `parseInt32`.

The `_` identifier is the wildcard: it can be used as a throwaway binding name and is exempt from the shadowing rule (see [Bindings & Scope](bindings.html)).

### Reserved keywords

The core reserved words are:

```txt
val   var   type   export   import   from   as   foreign
if    then  else   match    is       has    when
null  true  false
```

These cannot be used as ordinary identifiers.
