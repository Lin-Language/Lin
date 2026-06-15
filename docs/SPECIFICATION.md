# Lin Language Specification

> **About this document.** This is the normative specification for **Lin**, a
> small, expression-based language built around strict JSON data, structural
> typing, first-argument function application, destructuring, pattern matching,
> opaque iterator/runtime types, and value-based error handling. Where this
> document describes behaviour, it describes what the reference implementation
> (`lin build`, the Rust workspace under `crates/`) actually does today; planned
> but unimplemented features are collected in Appendix B. The standard-library
> reference lives in a separate document, `docs/STDLIB.md`; the rationale for
> non-obvious implementation choices lives in `docs/DECISIONS.md`.

---

# Part I — Overview

## 1. Introduction

### 1.1 Purpose

Lin is a small expression-based programming language built around strict JSON
data, structural typing, first-argument function application (dot syntax),
destructuring, pattern matching, opaque iterator/runtime types, and value-based
error handling.

The design goal is to keep the language surface small while supporting practical
programming with JSON-shaped data and functional-style pipelines.

### 1.2 Design Principles

1. Everything is an expression.
2. Runtime data values are strict JSON values, plus opaque runtime values such as functions, iterators, and modules.
3. JSON object keys are strings and must be quoted in object literals.
4. There are no semicolons.
5. Whitespace and indentation are significant.
6. Types are structural by default.
7. Errors are ordinary values, usually represented with union types.
8. There are no classes.
9. Behaviour is expressed with functions, closures, partial application, and opaque runtime protocols.
10. Pattern matching is the primary way to consume union types.

---

# Part II — Lexical Structure

## 2. Lexical Structure

### 2.1 Comments

Only line comments are supported. They begin with `//` and continue to the end of the line. There are no block comments.

```txt
// This is a comment
val x = 1 // This is also a comment
```

### 2.2 Whitespace and Indentation

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

### 2.3 Identifiers

Identifiers are used for values, functions, type names, imports, destructuring bindings, and local bindings.

By convention, built-in and named types use `CamelCase`:

```txt
String
Boolean
Null
AnyVal
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

### 2.4 Reserved Keywords

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
foreign
null
true
false
```

### 2.5 String Literals

Strings are delimited with double quotes (`"`).

```txt
val name = "Bob"
```

Strings may span multiple lines. Newlines inside the literal are preserved verbatim.

```txt
val poem = "Roses are red,
Violets are blue."
```

#### 2.5.1 Escape Sequences

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

#### 2.5.2 Interpolation

Strings support interpolation with `${ expression }`. The expression is evaluated in the surrounding scope and its result is converted to a string via `toString`. Interpolation is the only way to build strings from parts; `+` does not work on strings.

```txt
val name = "Bob"
val age = 42
val greeting = "Hello ${name}, you are ${age + 1} next year"
```

Because the compiler sees all parts of an interpolated string as a single AST node, it can compute the total length and allocate exactly once, with no intermediate allocations.

Interpolated expressions can themselves contain string literals, function calls, and arbitrary expressions, but they cannot span multiple statements.

A literal `$` is written `\$` when followed by `{`. A literal `${` not intended as interpolation is written `\${`.

### 2.6 Numeric Literals

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

Without a suffix, integer literals default to `Int32` and floating-point literals default to `Float64`, subject to context-driven inference (see §21).

### 2.7 Negative Literals and Leading `-`

A leading `-` is part of a numeric literal when:

1. there is no whitespace between the `-` and the digits, and
2. the previous token cannot end an expression (i.e., it is one of `(`, `,`, `=`, `=>`, `:`, an operator, or a keyword such as `then`, `else`, `is`, `has`, `when`).

Otherwise the `-` is parsed as the binary subtraction operator.

```txt
val temperature: Int32 = -5      // literal
val delta: Int32 = x - 5          // subtraction
val passed = f(-5, x - 3)         // first: literal; second: subtraction
```

A leading `-` on a **non-literal** expression is syntactic sugar for subtraction from zero: `-x` parses as `0 - x` (ADR-031). It is not a distinct unary operator — there is no negation node in the AST and no separate negation typing rule; `-x` is exactly the binary subtraction `0 - x` and obeys the same numeric typing. Both spellings are equivalent:

```txt
val negated = -x        // sugar for 0 - x
val negated = 0 - x     // explicit
```

The two genuine prefix unary *operators* are bitwise `~` (§27.2) and logical `!` (§8.1). Leading `-` is desugared at parse time and so is not counted among them.

---

# Part III — Types

## 3. Values

### 3.1 Primitive Values

```txt
val name: String = "Bob"
val active: Boolean = true
val missing: Null = null

val count: Int32 = 42
val total: UInt64 = 9000000000
val ratio: Float64 = 3.14
```

The value `null` has type `Null`.

### 3.2 Numeric Types

The language has explicit numeric families. The implemented families are:

```txt
Int8   Int16   Int32   Int64
UInt8  UInt16  UInt32  UInt64
Float32  Float64
```

Floating-point families follow IEEE 754 and are always signed; there is no `UFloat`. Non-negativity of a float is a runtime invariant, not a type-level one — enforce it with validation if needed.

> **Note.** Only `Float32` and `Float64` exist. `Float8` and `Float16` are not implemented and are not resolvable type names. Integer families span 8/16/32/64-bit signed and unsigned.

The name `Number` is a **numerically-bounded generic type parameter**, enforced at compile time with
zero runtime cost (ADR-014, which reverses the earlier "conceptual union" treatment). A parameter
(or return) annotated `Number` means "this must be a number"; it is sugar for an implicit
`<T: numeric>`:

```lin
val isEven = (x: Number) => x % 2 == 0
isEven(4)     // compiles & runs as Int32 (native srem) 
isEven(3.0)   // compiles & runs as Float64 (native frem)
isEven("hi")  // COMPILE ERROR: expected a numeric type (Number)
```

The body type-checks because the bound guarantees a numeric family (arithmetic is permitted on a
`Number`-typed operand). At each call site the concrete family flows in from the argument and the
compiler **monomorphizes** a specialized copy (`isEven$Int32`, `isEven$Float64`) that compiles to
the same native unboxed code as a hand-written concrete function — true parity, no boxing.

Each `Number` occurrence in a signature is its own independent bounded variable, so
`(a: Number, b: Number)` admits `a` and `b` at different families. A single call that combines two
distinct `Number` parameters at *different* families (e.g. `add(10, 2.5)` for
`(a: Number, b: Number) => a + b`) is **supported** and **widens exactly like concrete families**:
`add(10, 2.5)` yields `Float64` (`12.5`), `add(10, 2)` stays `Int` (both `Int32`), `add(1.5, 2.5)`
is `Float64`. The monomorphized `add$Int32_Float64` emits native `sitofp`+`fadd` — no boxing.

`Number` also works in **nested positions**: `Number[]` (an array element — homogeneous, one shared
bounded family per array) and combinator callbacks over it. For example
`(xs: Number[]) => xs.map((v: Number) => v * 2)` specializes the element family from the argument
(`f([1, 2, 3])` ⇒ `Int32`, `f([1.5, 2.5])` ⇒ `Float64`) and runs as a native unboxed loop. The
callback's `Number` parameter is tied to the array element it consumes.

A dynamic `AnyVal` value (direct or projected, e.g. `config["count"]`) **is accepted** at a `Number`
parameter, consistent with the `AnyVal → Int32` scalar coercion (ADR-032). It specializes to the
default `Int32` family and unboxes **unchecked** — an `AnyVal` holding a non-integer number unboxes as
garbage, the same accepted unsoundness as `val n: Int32 = jsonValue`. For a range-checked decode use
`Int32.fromJson(v)` (which returns `T | Error`).

Limitations: `Number` in a **higher-order function-typed parameter** (e.g.
`(f: (Number) => Number, x: Number) => f(x)`) cannot yet be inferred at the call site (the same
inference gap that affects an explicit `<T>` callback param); use a concrete numeric family there.
See §21 for coercion and inference rules.

### 3.3 Strict JSON Object Literals

Object literals use strict JSON syntax, with one shorthand extension.

Rules:

1. Keys must be quoted strings, **or** bare identifiers used as shorthand (see below).
2. Commas are required between fields.
3. Trailing commas are not allowed.
4. Runtime object values must be JSON-compatible.
5. An object literal may include spread elements of the form `...expr`. If `expr` evaluates to an object, its fields are copied in. If `expr` is `null` (or an absent/optional value that resolves to null), the spread contributes no fields — it is a no-op, not a runtime error. Fields and spreads are processed left-to-right. When the same key is written more than once (by spread or explicitly), the later value replaces the earlier one and the key keeps its first-occurrence position in iteration order.

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

### 3.4 Arrays

Arrays use strict JSON array syntax.

```txt
val numbers = [1, 2, 3]
val names = ["Bob", "Alice"]
```

Two distinct *type* forms describe arrays — see §5.2 and §5.3.

## 4. Built-in Types

The following type names are resolvable in type-annotation position:

```txt
String
Boolean
Null
AnyVal
Error
Int8 Int16 Int32 Int64
UInt8 UInt16 UInt32 UInt64
Float32 Float64
Function
Iterator<T>
Shared<T>
TarEntry
```

plus any user-declared `type` alias (§5) and any `type` imported from another module (§22).

`AnyVal` (formerly `Json`; renamed in the representation reset — ADR-069) is the **JSON-shaped dynamic top type**: arbitrary data whose shape is not statically known. It is the recursive union of every JSON-shaped value:

```txt
type AnyVal =
  | String
  | Boolean
  | Null
  | <any numeric family>
  | AnyVal[]
  | { ...AnyVal }    // any object whose values are AnyVal
```

The last form above is informal shorthand: an `AnyVal`-valued object is any object whose fields are themselves `AnyVal`. A *typed* index signature with a concrete value type does exist as real syntax — `{ String: T }` (§5.1.1).

`AnyVal` is **JSON-shaped and cannot smuggle an opaque handle**: `Stream<T>`, `Promise<T>`, `Shared<T>`, and `TarEntry` are rejected from widening into it (a live handle has RC/identity/serialization semantics a dynamic value type cannot honour). Code that must hold or forward an opaque or shape-agnostic value uses a **generic `<T>`** (a real type parameter *does* carry handles) or a **union** for a closed set — not `AnyVal`. There is intentionally no universal `Any` top type. (`Function`/`Iterator` currently still widen into `AnyVal`; tightening that is an open coherence question.)

`Error` is a built-in structural alias for the conventional error value `{ "type": String, "message": String }` (§20). It composes in unions and is discriminated with `is Error`. As a structural alias it is **not** a sealed named record (§5.9.1): an `Error`-typed value may carry extra fields, and `is Error` permits them.

`Function` is an opaque type that accepts a function of any arity. `Iterator<T>` is the opaque runtime traversal type (§18). `Shared<T>` is the opt-in shared-mutable-state box used with the concurrency accessors (§24.5). `TarEntry` is a generation-stamped, refcounted handle to a single tar archive entry, minted by `std/archive.entries` (§27 / ADR-068). It is non-transferable across thread boundaries.

### 4.1 Conceptual vs. resolvable type names

Several names that appear in prose and in conceptual signatures are **not** resolvable type-annotation names in the current implementation. They describe sets of values or runtime kinds rather than nameable types:

| Name | Status | Use instead |
| --- | --- | --- |
| `Number` | conceptual union over numeric families; not resolvable | `AnyVal`, or a concrete family (`Int32`, `Float64`, …) |
| `Iterable<T>` | conceptual ("arrays and iterators"); not resolvable | `Iterator<T>`, or `AnyVal[]` for arrays |
| `ThreadPool` | opaque runtime value, erased to `AnyVal`; not a nominal type | annotate as `AnyVal` (see §24.5) |
| `Worker<Msg, Reply>` | opaque runtime value, erased to `AnyVal`; not a nominal type | annotate as `AnyVal` (see §24.6) |

`Unknown` and `Never` are not built-in types.

## 5. Type Declarations

### 5.1 Object Types

Object types are JSON-shaped but are type syntax, not JSON values.

```txt
type Person = {
  "name": String,
  "age": Int32
}
```

#### 5.1.1 Index-Signature (Typed Map) Object Types

An object used as a *dictionary* — arbitrary, dynamically-computed string keys all mapping to the
same value type `T` — is typed with an **index signature**:

```txt
val counts: { String: Int32 } = {}
counts["apple"] = 3
val n = counts["apple"]      // type: Int32 | Null
```

The key type must be `String` — either written literally, or **named by a type alias that resolves
to `String`** (e.g. `type StopID = String` then `{ StopID: T }`). The key is written as a bare
identifier (not a quoted string — that is how the index-signature form is told apart from a fixed
record), and that identifier is resolved as a type expression: any alias that unfolds to `String` is
accepted, at any nesting depth and in any key position. The key type is
preserved as-written so the formatter round-trips the alias name. `String` is the only *underlying*
key type for the dynamic map form. `{ String: T }` reads "any number of string keys, each mapping
to `T`". It is a distinct type from a fixed-field record `{ "f": T, … }`: a value is *either* a
fixed record *or* an index-signature map, never both.

**Numeric (integer) keys — `{ Int: T }`.** The key type may instead be an **integer family** (`Int8`…
`Int64`, `UInt8`…`UInt64`, or the `Int` alias), giving a hashmap keyed by an integer rather than a
string:

```txt
val seen: { Int32: Boolean } = {}
seen[42] = true
seen[1_000_000] = true        // sparse keys — no dense array allocated
val hit = seen[42]            // type: Boolean | Null
val miss = seen[7]            // → Null
```

Integer keys are stored as a raw `i64` inline in the hash slot (no per-key allocation, no pointer
chase), so an integer map is **faster and smaller** than a string map; key `0`, negatives, and large
sparse keys all work. Keys normalize to `i64` and compare by integer value (so an `Int32` index into
a `{ Int64: T }` map is accepted, mirroring numeric `==`). **`Float` keys are rejected** (equality is
a footgun), as are union/`AnyVal`/`Function`/handle key types. A map has exactly one key kind — you
cannot mix string and integer keys in one map. Sealed-record and array key types are a planned
extension (a *structural* key kind: hash + compare the key's fields/elements — cheap to add once the
key-kind dispatch exists, O(fields) per access, paid only by struct-keyed maps); for *dense* `0..N`
indices prefer an array, not a map.

**Literal-union key — sugar for a fixed record.** When the key identifier instead resolves to a
**closed union of string literals** (or a single string-literal type), the `{ K: V }` form is
**sugar** for the fixed record with one field per literal, all of value type `V`:

```txt
type DayOfWeek = "Monday" | "Tuesday" | "Wednesday" | "Thursday" | "Friday" | "Saturday" | "Sunday"
type Calendar  = { DayOfWeek: Boolean }
// exactly equivalent to:
// type Calendar = { "Monday": Boolean, "Tuesday": Boolean, … , "Sunday": Boolean }
```

This is an ordinary fixed record (not a map): structurally identical to the hand-written form and
interchangeable with it. Indexing it by a key of the *same* literal union is **provably total** —
`calendar[dow]` has type `Boolean`, with no `| Null`, because every member of the union is a present
field (§6.1's missing-key `Null` cannot arise). A key identifier that resolves to neither `String`
nor a string-literal union is a compile-time error
(`Index-signature key type must be String or a union of string literals, but it resolves to …`).

Because the meaning of `{ K: V }` depends on what `K` resolves to — `String` ⇒ dynamic map,
string-literal union ⇒ fixed record — these two have different runtime representations; changing a
key alias from one to the other changes the type. See ADR-055.

- `m[k]` requires `k` to match the map's **key type** (`String` for a `{ String: T }` map, the
  integer family for a `{ Int: T }` map) and yields `T | Null` (a missing key is `Null`, consistent
  with the §6.1 safe-bracket rule). A key of the wrong kind — a numeric key into a `{ String: T }`
  map, or a string key into a `{ Int: T }` map — is a compile-time error; the read and write key
  rules are symmetric. For the *defaulted* read, `m[k] ?? default` uses the built-in null-coalescing operator
  `??` (§8.3); for a keyed default the dot-applicable `object.get(m, k, default)` (`std/object`)
  remains the convenience. Both give the default an independent type `D`, so the result is `T | D`
  (and `T | Null` when the default is omitted); a same-typed default collapses `T | D` to a bare `T`.
- `m[k] = v` requires `v : T` and `k : String`.
- An empty `{}` literal infers `{ String: T }` from its context (the annotated binding / return
  type). An **evidence-free** empty `{}` — no annotation, no contextual type, no contents — is a
  **compile error** (it cannot infer a value type); annotate it, e.g. `val m: { String: Int32 } =
  {}` or `val m: {} = {}` for a dynamic record. See ADR-058.
- A **non-empty** record literal `{ "k1": v1, "k2": v2, … }` (string-literal keys) likewise widens
  to `{ String: T }` whenever it is checked against that type and every value `vi` is assignable to
  `T` — the literal becomes a hashed map carrying those entries rather than a fixed record. This
  applies in any checked position, including a **nested field**: a field typed `{ String: T }`
  inside an enclosing record (e.g. `{ "headers": { String: String }, … }`) widens the literal given
  in that field position, transitively through intermediate record fields. (A literal with a
  non-string-literal/dynamic key is not widened — it defers to ordinary inference.)
- `keys(m) : String[]`, `values(m) : T[]`, `entries(m)` are available via `std/object`.
- There is no implicit `AnyVal → { String: T }` coercion — convert an `AnyVal` value through
  `fromJson`/narrowing exactly as for any other concrete type (§6.3, §19).
- An index-signature type cannot be used as the pattern of an `is`/`has` type test in v1 (its
  type form is not spellable in pattern position).

A `{ String: T }` value is backed at runtime by a hashed container giving **O(1) average**
lookup/insert (in contrast to an `AnyVal`/`{}` record, whose small association-list layout is O(n) per
access — optimal for the handful-of-fields case, catastrophic for a large dictionary). See ADR-055.

### 5.2 Array Types

The type of an unbounded array of `T` is written `T[]`.

```txt
val xs: Int32[] = [1, 2, 3]
val names: String[] = ["Bob", "Alice"]
```

`T[]` describes an array of any length whose every element has type `T`.

A non-empty array literal infers `T` from its contents (`[1, 2, 3] : Int32[]`). An **evidence-free**
empty array literal — a bare `[]` with no annotation, no contextual type, and no elements — cannot
infer its element type and is a **compile error**: annotate it, e.g. `val xs: Int32[] = []`. An
empty `[]` still works wherever context supplies the element type (an annotated binding, a typed
function parameter in argument position, a declared return type, a typed array element). See
ADR-058.

### 5.3 Fixed-Length Array Types

A type written as `[T1, T2, ..., Tn]` describes an array of exactly `n` elements, where each position has the corresponding type.

```txt
val pair: [String, Int32] = ["age", 42]
val triple: [String, Int32, Int32] = ["coords", 10, 20]
```

These are *not* tuples — they remain JSON arrays at runtime. The `[T1, T2, ...]` type form simply constrains length and positional element types at the type level.

A fixed-length array type is assignable to the corresponding unbounded type when all positional types are compatible. The reverse is not true.

### 5.4 Union Types

Union types use `|`.

```txt
val maybeName: String | Null = null
```

```txt
type Id = String | Int64
```

#### Record intersection (`&`)

Two **record** types may be combined with `&` to form the record containing **all** of both
operands' fields:

```txt
type Person = { "age": UInt8, "name": String }
type OldPerson = Person & { "wisdom": Boolean }
// OldPerson is { "age": UInt8, "name": String, "wisdom": Boolean }
```

`&` is **record-only** in this first cut: every operand must be an object/record type. It is
left-associative and composes — `A & B & C` merges all three. The result is an ordinary record
type with no special runtime representation; when bound to a named type declaration it is **sealed**
exactly as if the merged record had been written out in full (§5.9, named records are sealed).

Rules:

- A field present in more than one operand must have the **same** type in each (it is de-duplicated).
  Conflicting field types are a compile-time error: `intersection type has conflicting field "k": T1 vs T2`.
- A non-record operand (e.g. `Int32 & String`, or `&` with a union) is a compile-time error:
  `intersection \`&\` is only valid between record types`.
- `&` binds **tighter** than union `|` (matching TypeScript), so `A & B | C` parses as `(A & B) | C`.

See ADR-061.

### 5.5 Function Types

Function types use argument-list syntax followed by `=>`.

```txt
type Predicate<T> = (T) => Boolean
type Mapper<T, U> = (T) => U
type Reducer<T, U> = (U, T) => U
```

### 5.6 Generic Types

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

### 5.7 Type Expression Precedence

Type-expression operators bind in this order, tightest first:

```txt
1. T[]                 (postfix array)
2. Generic<T1, T2>     (postfix generic application)
3. (T1, T2) => U       (function arrow)
4. T & U               (record intersection)
5. T | U               (union)
```

So `Int32 | String[]` parses as `Int32 | (String[])`, `(Int32) => String[]` parses as `(Int32) => (String[])`, and `A & B | C` parses as `(A & B) | C` (`&` binds tighter than `|`). Parenthesise to disambiguate where the surface reading is unclear.

### 5.8 Variance

Generic types are **covariant** in their parameters where they appear in producer position (return type, array element, container content), and **contravariant** in consumer position (function arguments).

Concretely:

- `Person[]` is assignable to `AnyVal[]`.
- `Iterator<Person>` is assignable to `Iterator<AnyVal>`.
- `(Person) => Int32` is assignable to `(Bob) => Int32` for any `Bob` compatible with `Person`.
- A function returning `Person` is assignable to one returning `AnyVal`.

#### Callback arity-width subtyping

A function value that declares **fewer** parameters is assignable where a function with **more**
parameters is expected, provided every **extra expected trailing parameter is `Int32`** and the
common leading parameters and the return type are compatible. This is the type-system rule behind the
**optional 0-based index parameter** on the iterable combinators (§18.7): the combinator declares a
callback such as `(T, Int32) => U` (or `reduce`'s `(U, T, Int32) => U`), but a caller's 1-arg (reduce:
2-arg) lambda still flows through — the omitted trailing `Int32` index is simply ignored. The leniency
is tight: only extra trailing `Int32` parameters are tolerated (arbitrary arity subtyping is **not**
allowed; a value with more parameters than expected, or extra non-`Int32` expected parameters, is
rejected). An explicit annotation on the index parameter must be `Int32`; any other annotation is a
compile error.

### 5.9 Structural Typing

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

This compatibility relationship is the same as `has` — see §11.

#### 5.9.1 Sealed named records and lossy projection

A **named** record type (`type T = { … }`) is *sealed*: a value whose static
type is `T` holds **exactly** `T`'s fields — no extras. This is a representation
guarantee that lets the compiler lay sealed records out as unboxed structs with
constant-offset field access (see ADR-057); it does **not** change the structural
compatibility above.

Representation is **type-determined**, not inferred (ADR-069, which supersedes the
flow-sensitive representation-inference pass of ADR-062): a value whose static type
is a sealed record `T` is **always** a flat packed struct — there is no boxed
shadow and no per-occurrence packed-vs-boxed choice. Every field is at a constant
offset: scalars (numeric/`Boolean`) inline at their natural offset, and heap fields
— `String`, an array, a `{ String: T }` map, or a nested sealed record — each as an
8-byte owned pointer slot with per-field retain/release. Field access is always a
constant-offset load, never an association-list lookup; the only overhead over an
all-scalar record is the owned-pointer refcounting. Arrays *of* records (`Person[]`)
are pointer-backed (each element a shared sealed pointer), so `push` shares rather
than copies. Records have **reference semantics** — `val b = a` makes `b` and `a`
the same record, and mutation through a parameter is visible to all aliases.

When a record value flows into a slot whose static type is the dynamic top type
`AnyVal` or a union, it is not materialized into a string-keyed object — it is
carried as a runtime-tagged sealed pointer (`TAG_RECORD`; `T | Null` over a record
is a nullable sealed pointer, `A | B` a tag + sealed payload), and `match … is T`
narrows it back to the typed pointer, so member reads stay constant-offset. This is
transparent to programs: it affects only speed.

The two are reconciled by a **non-mutating projection** at the boundary: when a
wider value (one with extra fields, or an `AnyVal` value) flows into a slot of named
type `T` — a parameter, a `val`/`var` with a `T` annotation, a return typed `T`,
or an element of a `T[]` — it is **copied** into a fresh sealed value containing
only `T`'s fields. Extra fields are dropped **from the copy**; the original value
is unchanged and keeps its extra fields in its own scope.

```txt
type Named = { "name": String }

val wide = { "name": "Alice", "age": 99 }   // an anonymous record with an extra field
val n: Named = wide                          // projects to a fresh { "name": "Alice" }
// n["age"]  → compile error: `age` is not a field of Named
wide["age"]                                  // still 99 — `wide` is untouched
```

`T.fromJson(json)` projects the same way: it validates and keeps exactly `T`'s
fields, dropping unknown keys.

**Consequence — the one idiom this changes.** A named record type can no longer be
used as an *open carrier* that smuggles extra fields through to a later consumer:
once a value is typed as a named record, its extra fields are gone. Code that must
preserve arbitrary extra keys (a heterogeneous bag, a pass-through envelope) should
type the value `AnyVal`, not a named record type.

---

# Part IV — Expressions

## 6. Bindings

### 6.1 Immutable Bindings

```txt
val x = 1
val name: String = "Bob"
```

`val` bindings are immutable.

### 6.2 Mutable Bindings

```txt
var count = 0
count = count + 1
```

`var` bindings are mutable.

Assignment expressions evaluate to the assigned value. This holds for variable assignment
(`count = count + 1`), index assignment (`m[k] = v`), and field assignment (`rec["f"] = v`) alike —
each evaluates to the stored value, so an assignment can be the tail of a block or an `if` branch:

```txt
val result = count = count + 1

// A memoizing cache: the `then` branch's assignment yields the value it stored, so the whole `if`
// (and the function) returns the computed-or-cached value with no intermediate binding.
val parse = (time: String): Int32 =>
  if cache[time] == null then cache[time] = compute(time)
  else cache[time]
```

A function whose declared return type is `Null` is in *void* position: its body value is discarded,
so the body may evaluate to any type (e.g. it may end in an assignment) without an explicit `null`
tail.

Mutable bindings are captured by reference in closures.

```txt
val makeCounter = (start: Int32) =>
  var count = start

  () =>
    count = count + 1
    count
```

### 6.3 Recursive Bindings

A `val` whose right-hand side is a function literal may reference itself by name. The name is in scope within the function body.

```txt
val factorial = (n: Int32): Int32 =>
  if n == 0 then 1
  else n * factorial(n - 1)
```

A `val` whose right-hand side is *not* a function literal may **not** reference itself.

Mutual recursion between two top-level `val` bindings of function literals is permitted: both names are in scope across both bodies.

## 7. JSON Access

Bracket notation is used for both JSON object key access and array indexing. Bracket access is **safe by default**: object accesses never raise an error, and `Null` propagates through chains.

```txt
val name = person["name"]
val city = person["address"]["city"]
val first = numbers[0]
```

Dot syntax is not used for JSON field access.

### 7.1 Runtime Semantics

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

### 7.2 Static Typing of Access

- If the operand's static type is a typed object that declares the key as `T`, the access has type `T`.
- If the operand's static type is `AnyVal`, the access has type `AnyVal` (which already covers `Null`).
- If the operand's static type is a typed object that does **not** declare the key, the access is a compile-time error. (Use `AnyVal` if you need free-form access.)
- If the operand may be `Null` (e.g., a union `T | Null`), the access type widens to include `Null`.
- Array element access on `T[]` has type `T` (the static type does not include `Null`; the runtime error is the contract for OOB).

An `AnyVal` value is **not** implicitly convertible to a concrete structured object type (an object with a required, non-nullable field). Binding or passing an `AnyVal` value where such a type is expected is a compile-time error; convert it explicitly with `fromJson` (validated decode, `std/json`) or narrow it with `is`/`has` (runtime tag checks). `AnyVal → AnyVal` and `AnyVal` flowing into scalars/handles/buffers/open objects remain permissive. See ADR-045, ADR-032, and ADR-033.

## 8. Operators

### 8.1 Operator List

```txt
+   -   *   /   %
==  !=  >   <   >=  <=
&&  ||  !
??                        (null-coalescing — see §8.3)
&   |   ^   <<  >>  ~      (bitwise — see §27.2)
```

These are built-in operators, not ordinary functions. They are not available through dot application or partial application.

`+` operates only on numeric types. String building uses interpolation (`"${a}${b}"`) — see §2.5.2.

The bitwise operators `&`, `|`, `^`, `<<`, `>>` require integer operands; `~` is unary. They are specified in §27.2. In type-expression position `|` remains the union separator (§5.4); the two never overlap syntactically.

There are two unary operators: bitwise `~` (§27.2) and logical `!`. A leading `-` is not a unary operator — it is either part of a numeric literal (§2.7) or parse-time sugar for `0 - x` (§2.7). Logical `!b` requires a `Boolean` operand and yields `Boolean`:

```txt
val notReady = !ready
```

### 8.2 Precedence

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
13. ??                 (null-coalescing; lowest binary rung — see §8.3)
```

All binary arithmetic, comparison, and bitwise operators are left-associative. `&&` and `||` are left-associative and short-circuiting. The unary operators `~` and `!` are right-associative and bind tighter than `*` but looser than postfix, so `!a == b` parses as `(!a) == b`.

`??` is the lowest-precedence binary operator (rung 13, below `||`), left-associative, and short-circuiting — so `x ?? y ?? z` parses as `(x ?? y) ?? z`, and `a ?? b == c` parses as `a ?? (b == c)` (the same grouping JavaScript gives). To avoid the well-known ambiguity, an **unparenthesised mix of `??` directly with `&&` or `||` is a parse error** in either direction (`a || b ?? c` and `a ?? b || c`); wrap the logical sub-expression in parentheses — `(a || b) ?? c` or `a ?? (b || c)`.

### 8.3 Null-coalescing (`??`)

`a ?? b` evaluates to `a` when `a` is non-null, and to `b` otherwise. It is exactly equivalent to `if a != null then a else b`: `a` is evaluated **once**, and `b` is evaluated **only when `a` is `Null`** (short-circuit).

```txt
val counts: { String: Int32 } = {}
val n = counts["missing"] ?? 0     // n : Int32  (the absent-key Null is replaced)
```

**It coalesces `Null` only — never `Error`.** Lin's value-based error convention (§4, §20) must stay explicit, so a left operand of type `T | Null | Error` that holds an `Error` value flows that `Error` **through** to the result; it is *not* replaced by the default. The result type is `(T | Error) | D`:

```txt
val r = lookup(key) ?? fallback   // lookup : (String) => Trip | Null | Error
match r
  is Error => …    // a real failure still surfaces — the default did NOT swallow it
  else     => …    // a present Trip, or the fallback when the key was merely absent (Null)
```

Typing rules:

- The left operand's type **must include `Null`** — `Null` itself, a union containing `Null`, or `AnyVal` (which is dynamically nullable). Otherwise it is a compile-time error (*"left operand of `??` is never null"*), since the default would be dead. A bare `Null` left operand is allowed (the result is just the right operand's type).
- The result type is `(left type with Null stripped) | D`, where `D` is the right operand's type. When `D` is assignable to the stripped left type, the union **collapses to the bare stripped type** — so `counts[k] ?? 0` (with `counts[k] : Int32 | Null` and `0 : Int32`) is a plain `Int32`, usable in arithmetic without further narrowing. This mirrors the documented behaviour of `object.get` (§6.1).
- For an `AnyVal` left operand the result is `AnyVal | D` (which normalises to `AnyVal`, since `AnyVal` already subsumes any concrete `D`).

`??` is the operator form of the *defaulted read*; for a keyed map/object default the dot-applicable `object.get(m, k, default)` (§6.1, `std/object`) remains the convenience, and the two agree on the collapse-to-bare-`T` rule.

## 9. Equality

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

Function, iterator, and module equality are not defined.

Numeric equality across families: numbers compare by mathematical value after coercion to the wider type (see §21). `1 == 1.0` is true; `"1" == 1` is false because they are different runtime kinds.

## 10. If Expressions

`if` is an expression and must produce a value. The `else` branch is optional: when it is omitted, an implicit `else null` is supplied, and the expression's type widens to `T | Null` (where `T` is the then-branch type). This makes side-effect-only forms such as `if cond then push(arr, item)` idiomatic without writing `else null` (ADR-023). Supply an explicit `else` whenever the result is consumed as a non-nullable value.

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

A logical line that begins an `if` may continue using `&&` or `||` as described in §2.2:

```txt
val label = if person["age"] >= 18
  && person["active"] then "active adult"
else "other"
```

### 10.1 Nested `if` Inside `match`

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

## 11. `is` and `has` Expressions

`is` and `has` can be used in `if` expressions and `match` patterns.

### 11.1 `is`

`is` performs a **type-exact** match: the value must conform to `T`, checked recursively.

- For a named object type `T`, `value is T` is true if `value` is an object that has every field of `T` **present and of the correct type** (checked recursively into nested objects, arrays, and literal-typed fields). Extra fields are permitted — `is` validates field *types*, not field *count*. This is what makes the post-match narrowing to `T` sound (ADR-036): if `value is T` succeeds, `value` genuinely conforms to `T`, so the narrowed field types are not a lie. Field-type checking follows the same structural-validation semantics and number policy as `fromJson` (§19, `std/json`): an integer-typed field accepts an integral in-range number, a float-typed field accepts any number, a literal-typed field requires the exact value.
- For a primitive type, `value is T` is true only if the runtime value has that exact type.
- For a literal, `value is "Dave"` is true only if the value equals the literal.

The difference between `is T` and `has T` (§11.2) for an object type is therefore precisely whether field *types* are validated: `is` checks presence **and** type; `has` checks presence only. Both permit extra fields.

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

`is` is not supported against generic type applications. Writing `value is Result<Int32, String>` is a compile-time error. Match the underlying tagged shape instead (see §19).

`is` and `has` are expressions of type `Boolean` and may be used in any expression context, not only `if` conditions and `match` arms:

```txt
val isAdult = person has { age } && person["age"] >= 18
```

A string literal as a **value** (e.g. on either side of `is`) has base type `String`, not a singleton. `"Dave" is "Dave"` is true and is a runtime equality check; the type of the literal value `"Dave"` is `String`. A string literal in **type** position, however, *is* a singleton type — see §19 (tagged unions) and decision-list item 33 in Appendix B. So `value is "Dave"` tests value equality, whereas `type Name = "Dave"` declares a type whose only inhabitant is the string `"Dave"`.

A single `match` arm may not combine `is` and `has` patterns — each arm uses one keyword.

### 11.2 `has`

`has` performs a **structural compatibility** check — the value contains *at least* the requested shape, but may have additional fields.

- For a named object type `T`, `value has T` is true if every field of `T` is **present** in `value` (its keys exist). Field *types* are **not** validated — that is what `is T` adds (§11.1). Extra fields are permitted.
- For an inline shape `{ a, b }`, `value has { a, b }` is true if `value` is an object containing at least those keys.

So `has` is the presence-only check and `is` is the presence-and-type check; both allow extra fields. Use `has` to test/destructure shape when the field types are already known or unimportant, and `is` when a successful match must guarantee the fields' types (e.g. before narrowing an `AnyVal` value to a typed shape).

```txt
val describeNamed = (input: AnyVal): String =>
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

## 12. Pattern Matching

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

### 12.1 `is` Patterns

`is` is the type-exact / shape-exact form. Its precise meaning depends on the pattern kind:

- **Primitive / literal / `Null`:** exact runtime-type or value match.
- **Named object type (`is Person`):** every declared field present and correctly typed, checked
  recursively; extra fields permitted (§11.1, ADR-036).
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

### 12.2 `has` Patterns

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

### 12.3 Pattern Guards with `when`

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

### 12.4 Catch-All `else` Arm

A `match` may end with an `else` arm. It matches any value not caught by an earlier arm. `else` is written `else => expr` and is indented at the same level as the other arms.

```txt
match input
  is Null => "null"
  is String => "string"
  else => "other"
```

`else` is the only catch-all form — there is no wildcard `_`.

### 12.5 Match Arm Layout

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

### 12.6 Match Exhaustiveness

A `match` that omits `else` must exhaustively cover the static type of the scrutinee. Exhaustiveness is enforced as follows:

- For closed unions whose arms are all `is` patterns over primitive types, `Null`, or literal values, exhaustiveness is a **compile-time error** when not covered.
- For all other patterns (`has`, structural shapes, tagged unions, mixed arms), exhaustiveness is a **warning** only.

Adding `else` always satisfies exhaustiveness.

## 13. Type Narrowing

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

A null test on an **index read** narrows a re-read of the same index place: `if m[k] != null then m[k] …` (and `m[k] ?? d`) reads `m[k]` as `T` rather than `T | Null` in the guarded branch. The place may be **compound** — an identifier root followed by any number of stable index steps (string-literal or simple-identifier keys), e.g. `service["dates"][date]` — so `if service["dates"][date] != null then service["dates"][date]` narrows the inner map read. The narrowing is invalidated if any identifier the place mentions (its root or a key variable) is reassigned, or a write lands through the same root.

Narrowing is invalidated on the first assignment to a `var` whose narrowed type would no longer hold.

---

# Part V — Functions

## 14. Functions

### 14.1 Function Expressions

```txt
val add = (a: Int32, b: Int32): Int32 =>
  a + b
```

Return types may be inferred where possible.

```txt
val add = (a: Int32, b: Int32) =>
  a + b
```

### 14.2 Single-expression Functions

```txt
val add = (a: Int32, b: Int32) => a + b
```

### 14.3 Blocks as Function Bodies

```txt
val total = (price: Float64, quantity: Int32): Float64 =>
  val subtotal = price * quantity.toFloat64()
  val tax = subtotal * 0.2
  subtotal + tax
```

The final expression is the function result.

### 14.4 No `return`

The language does not use `return` because all blocks are expressions.

Invalid:

```txt
val add = (a: Int32, b: Int32) =>
  return a + b
```

### 14.5 Default Parameter Values

A parameter may declare a default value with `= expr` after its (optional) type
annotation, making it optional at the call site. Optional parameters must be
last. See §15.6 for the full semantics.

```txt
val greet = (name: String, greeting: String = "Hello") =>
  "${greeting}, ${name}"
```

## 15. Function Calls and Partial Application

### 15.1 Function Calls

```txt
val result = add(1, 2)
```

### 15.2 Partial Application

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
default values (see §15.6), which are filled in; otherwise it is an error (§15.5).
The trailing comma is what distinguishes "call now, using defaults for the rest"
from "partially apply." A trailing comma on a fully-saturated argument list has
no effect.

```txt
val add = (a: Int32, b: Int32) => a + b

val f  = add(10)    // error: add has no default for `b`; use add(10,) to curry
val g  = add(10,)   // partial application — g : (Int32) => Int32
val s  = add(1, 2)  // complete call
```

### 15.3 Over-Application Is an Error

Supplying more arguments than a function expects is a compile-time error.

```txt
val add = (a: Int32, b: Int32) => a + b

val bad = add(1, 2, 3)   // error: add takes 2 arguments, got 3
```

### 15.4 Argument Evaluation Order

Argument expressions are evaluated left to right before the function is called.

### 15.5 No Tuples

Parentheses are argument lists, not tuples. There are no language-level tuples.

This syntax is not a tuple value:

```txt
("hello", 1)
```

It is an argument list, and is only meaningful in call or dot-application contexts (see §16.1).

### 15.6 Default Argument Values

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
  parameters; supplying fewer is an error (§15.5).
- Default-fill applies uniformly to direct calls, dot-application
  (`x.f(...)`, §16), and calls through a first-class function value
  (`val g = greet; g("World")`).
- To partially apply a function that has defaults — rather than fill them — use
  an explicit trailing comma (§15.2): `greet("World",)` yields a function
  awaiting `greeting`.

Default values are evaluated by the *defining* module, so an imported function
carries its defaults across module boundaries.

## 16. Dot Application

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

### 16.1 Dot Partial Application

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

### 16.2 Method Calls Require Parentheses

A function with no further arguments must still be called with `()`. There is no implicit invocation.

```txt
val n = items.length()      // correct
val n = items.length        // partial application — n is a function
```

### 16.3 Chaining

```txt
val result = "  hello  "
  .trim()
  .toUpper()
```

Equivalent to:

```txt
val result = toUpper(trim("  hello  "))
```

---

# Part VI — Data and Numerics

## 17. Destructuring

Destructuring is supported in `val` bindings, function parameters, pattern matching, and imports.

### 17.1 Object Destructuring

```txt
val person = {
  "name": "Bob",
  "age": 42
}

val { "name": name, "age": age } = person
```

### 17.2 Object Destructuring Shorthand

In destructuring patterns, bare names are shorthand for quoted JSON keys with the same local binding name.

```txt
val { name } = person
```

is equivalent to:

```txt
val { "name": name } = person
```

This shorthand does not change object literal syntax. Object literals still require quoted keys.

### 17.3 Object Alias Binding

```txt
val { "name": displayName } = person
```

### 17.4 Nested Destructuring

```txt
val {
  "name": name,
  "address": {
    "city": city
  }
} = person
```

### 17.5 Array Destructuring

```txt
val [first, second] = ["a", "b"]
```

### 17.6 Rest Spread

Array rest spread:

```txt
val [first, ...rest] = ["a", "b", "c"]
```

Object rest spread:

```txt
val { name, ...remaining } = person
```

Object spread is also valid in object *expressions*; see §3.3.

### 17.7 Function Parameter Destructuring

```txt
val describePerson = ({ name, age }: Person): String =>
  "${name} is ${age}"
```

## 18. Iteration

Iteration is represented using opaque runtime types rather than JSON-shaped objects containing functions.

```txt
Iterator<T>
```

An `Iterator<T>` is a stateful traversal that produces values of type `T`. An *iterable* is any value that can be iterated — in practice an array or an `Iterator<T>`. "Iterable" is a conceptual notion, not a resolvable type name (§4.1); the `for` builtin accepts arrays and iterators directly.

Arrays can be iterated without conversion.

```txt
val ints: Int32[] = [1, 3, 5]

ints.for(num =>
  print(num * 2)
)
```

### 18.1 `for` Is a Built-in

`for` is a built-in function provided by the compiler. It has privileged access to the internals of opaque iterator values and is the only function that consumes an iterator by stepping through it.

```txt
for: <T>(arr-or-iterator, (T) => Null) => Null
```

All other iteration combinators (`map`, `filter`, `reduce`, etc.) are ordinary library functions defined in terms of `for`.

### 18.2 Iterator Construction

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

#### 18.2.1 Restartability

Because the initial-state producer is a thunk, consumers may obtain a fresh starting state for the same iterator. Whether a particular `Iterator<T>` is safely restartable depends on the closure over external state in its four functions. The language guarantees that calling the initial-state producer again returns a fresh logical start; it does not guarantee anything about external side effects.

### 18.3 Array Iteration

Arrays can be converted to iterators using `iterOf`.

```txt
iterOf: <T>(T[]) => Iterator<T>
```

### 18.4 Range Iteration

`range` returns an iterator. It is an ordinary library function.

```txt
range: (Int32, Int32) => Iterator<Int32>
```

```txt
range(0, 10).for(i =>
  print(i)
)
```

### 18.5 Iterator Functions

The standard iterator functions accept arrays or `Iterator<T>` values and use dot application for fluent chaining. They live in `std/iter` and, when applied to a `Stream` receiver instead, dispatch to a lazy/fallible form (§18.7).

```txt
for:    <T>(arr-or-iterator, (T) => Null) => Null
map:    <T, U>(T[], (T) => U) => U[]
filter: <T>(T[], (T) => Boolean) => T[]
reduce: <T, U>(T[], U, (U, T) => U) => U
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

### 18.6 Iterator Design Rule

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

### 18.7 Receiver-Dispatched Combinators

The iterable combinators — `map`, `filter`, `reduce`, `for`, `while`, `take`, `drop`, `flatMap`, `takeWhile`, `dropWhile`, `flatten`, `concat`, `find`, `some`, `every` — and the iterator constructors `range`, `rangeStep`, `iter`, `iterOf` are a **single** vocabulary that works over any iterable source: an array, an `Iterator`, or a `Stream` (§27.9). They live in one module, `std/iter`, and **dispatch on the static type of the receiver** (their first argument, in dot-application terms). This is Lin's first-argument-dispatch model (§4.4) applied to the combinator set.

The same name behaves differently depending on the receiver:

- Over an **array** or `Iterator` the combinator is **eager**: it materialises and returns an array (`U[]`) or a scalar terminal value.
- Over a **`Stream`** an *adapter* combinator is **lazy**: it returns a new `Stream<U>` node and reads nothing until a terminal drives it; a *terminal* combinator drives the stream on the calling thread and returns its result.

Because a stream read is fallible (§27.9.4), a combinator that **terminates** a stream gains an `| Error` arm:

```txt
                          Array / Iterator        Stream<T>
map(f)                    U[]                     Stream<U>            (lazy adapter)
filter(p)                 T[]                     Stream<T>            (lazy adapter)
take(n) / drop(n)         T[]                     Stream<T>            (lazy adapter)
flatMap / take|dropWhile  ...[]                   Stream<...>          (lazy adapter)
flatten / concat          AnyVal[]                  Stream               (lazy adapter)
for(f)                    Null                    Null | Error         (terminal)
while(p)                  Null                    Null | Error         (terminal)
reduce(init, f)           U                       U | Error            (terminal)
find(p)                   T | Null                T | Null | Error     (terminal)
some(p) / every(p)        Boolean                 Boolean | Error      (terminal)
```

The dispatch is a closed, type-directed special-case over this fixed name set, not general function overloading. The precedent is `for` (§18.1), which already returns `Null` over an array and `Null | Error` over a stream. One import gives a single fluent chain that runs eagerly over an array and lazily, with bounded memory, over a stream:

```txt
import { map, drop, take, reduce } from "std/iter"
import { readStream } from "std/stream"

val total = readStream("data.csv")
  .lines()                                   // Stream<String>
  .drop(1)                                   // Stream<String>  (lazy)
  .take(4)                                   // Stream<String>  (lazy)
  .map(line => line.length())                // Stream<Int32>
  .reduce(0, (acc, n) => acc + n)            // Int32 | Error  (terminal)
```

The combinators are **not** dual-exported: each name has exactly one home (`std/iter`). The array-shaped operations that genuinely require a materialised, indexable, ordered array — `push`, `slice`, `set`, `at`, `length`, `reverse`, `sort`/`sortBy`, `zip`, `unique`, `chunk`, `compact`, `partition`, `sum`/`product`/`min`/`max`, etc. — stay in `std/array`; `std/stream` exports only stream-specific sources, sinks, and terminals (`readStream`/`writeStream`/`writeLines`/`drain`/`collect`/`readText`/`promise`/`close`/`lines`/`chunks`).

Receiver dispatch fires at a **concrete** combinator call whose receiver is statically a `Stream`. A stream passed through a user-defined generic `Iterable` parameter (a `T[] | Iterator | Stream` union) and combined inside that function stays **array-shaped**: the lazy form is forgone there, which is the safe resolution (the eager path is always correct). Streams are also affine resources (§27.9.5): a combinator that routes to the stream backend **consumes** (moves) the stream, so a stream chain is single-use. See ADR-051.

## 19. Tagged Unions

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

## 20. Errors

There are no exceptions and no throwing in user code. Errors are ordinary values. A function that may fail should return a union type.

The built-in `Error` type is the conventional error value (§4): the structural object `{ "type": String, "message": String }`. It has no special control-flow behaviour; it is detected with `is Error`. Fallible standard-library operations return `T | Error` (§25.1).

### 20.1 Runtime Errors

A small number of language-level operations can fail at runtime. They terminate the program with a diagnostic — they do not produce a value, and they cannot be caught (the one exception is inside an `async` thunk, which is a fault-isolation boundary; see §24.2.2). They are reserved for unrecoverable program errors:

| Operation                                 | Result on failure |
| ---                                       | --- |
| Array index out of bounds                 | runtime error |
| Integer division by zero (`/`, `%`)       | runtime error |
| Explicit narrowing cast that loses information | runtime error |
| Non-exhaustive `match` (no arm matched and no `else`) | runtime error |

Object key access never causes a runtime error — missing keys produce `Null` (§7).

Floating-point operations follow IEEE 754: division by zero produces `±Infinity` or `NaN`, not an error. Integer `%` follows the sign of the dividend (Rust convention).

> **Note on import cycles.** Imports are resolved eagerly at compile time (§22.5). Cyclic **function** references between modules are supported and compile as written; only a cyclic **value** initialisation (a top-level value reading an imported value from a module that imports it back) is a compile-time error.

---

## 21. Numeric Coercion

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

The integer-narrowing casts take their input as `UInt64` (the widest unsigned), so any narrower *unsigned* integer — or a value first masked down to a byte/word — widens into the parameter without range loss before truncation; a bare integer literal in range is accepted directly. These are the byte-extraction primitives used by `std/bytes` (§27.3) and are generally useful wherever explicit width control is needed.

A parallel `narrowTo*` family takes a **signed `Int64`** input instead, covering the case the `UInt64`-input family cannot — a value *computed* in `Int64` (or any signed integer). Because `Int64 → UInt64` is not an implicit coercion (it could wrap a negative), a computed `Int64` can never reach the `toUInt8`/… casts; `narrowToUInt8`/`narrowToInt8`/`narrowToUInt16`/`narrowToInt16`/`narrowToUInt32`/`narrowToInt32`/`narrowToUInt64` accept it directly and truncate to the named width with the same two's-complement semantics. `narrowToInt32: (Int64) => Int32` also fills the integer-to-`Int32` gap (the `toInt32` above takes a `Float64`). Use these to store a wide computed result into a narrow field — e.g. a `month: UInt8` derived from `Int64` calendar arithmetic.

> **Caution.** Reading a narrow integer field *back* into wide arithmetic does **not** auto-widen the expression: a suffixless literal next to a narrow operand adopts that operand's width (per the literal-inference rule below), so `153 * month` with `month: UInt8` computes at `UInt8` width and silently overflows. Either widen the field read first (`val m: Int64 = d["month"]`) or keep hot-path numeric fields at `Int64` and narrow only at the storage boundary.

Literal inference: a numeric literal with an explicit type suffix (e.g. `5i64`, `3.14f32`, §2.6) is fixed at that type, overriding context — assigning it where an incompatible type is expected is a type error. A suffixless literal takes the type required by its surrounding context if one exists; otherwise integer literals default to `Int32` and floating-point literals default to `Float64`. An integer literal whose value exceeds `Int32`'s range but has no wider context does **not** truncate: its default widens to the smallest type that preserves the value (`Int64`, or `UInt64` for a decimal above `Int64`'s max). A literal that does not fit its required (context or suffix) type is a compile-time error, never a silent truncation.

Generic and overload-style inference uses bidirectional type checking: type information flows both from declarations into expressions and from expression context back into holes. This is sufficient for `[1,2,3].map(i => i * i)` to infer `T = Int32` and `U = Int32` without explicit annotation.

---

# Part VII — Program Structure

## 22. Modules and Imports

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

Imports use destructuring syntax.

```txt
import { substring, indexOf } from "std/string"
```

### 22.1 Aliasing

```txt
import { substring as substr } from "std/string"
```

### 22.2 Multi-line Imports

```txt
import {
  substring as substr,
  indexOf,
  trim,
  toUpper
} from "std/string"
```

### 22.3 Importing Types

Types may be imported with the same syntax (ADR-035), including `as` aliases, and used in type annotations in the importing module.

```txt
import { HttpRequest, HttpResponse } from "std/http"
```

### 22.4 Module Path Resolution

An import path is a slash-separated string that resolves to a `.lin` source file:

```txt
import { something } from "myDir/anotherDir/aFile"
```

resolves to `myDir/anotherDir/aFile.lin`, located relative to the importing file's directory by default. Paths beginning with a recognised library prefix (`std/`) resolve into the embedded standard library.

### 22.5 Import Resolution and Initialisation

Imports are resolved **eagerly at compile time**: the entry-point module and all of its transitive imports are parsed and type-checked before code generation. Each imported module is type-checked once and cached by source hash (`.lin-cache/`), and a separate signature file records just its exported name→type map so dependents can verify their usage without re-checking the full module. Module-level `val`/`var` initialise in dependency order at program start.

**Import cycles are supported for function references.** Resolution loads the whole import graph up front and decomposes it into strongly-connected components; the members of a true cycle are type-checked together (a two-pass seed-and-recheck), so mutually-recursive functions across the import boundary compile **as written**, with no extra annotations. A diamond — the same module reached by two independent import paths — is not a cycle and is resolved once.

A cyclic **value** initialisation is still a compile-time error: a top-level `val`/`var` whose initialiser reads an imported *value* (not a function) from a module that imports it back would recurse forever during module init, and is reported as `circular import detected: a <-> b (a top-level value initializer reads an imported VALUE …)`. Binding or calling a peer *function* is fine — function symbols are resolved by name, not recomputed at init.

### 22.6 Naming Conventions

Standard-library functions use lower camel case (`substring`, `indexOf`, `toUpper`, `parseInt32`); built-in and named types use `CamelCase`.

### 22.7 Standard Library

The standard library is laid out under the `std/` prefix. Every name must be imported explicitly — nothing is auto-imported as a global (ADR-002, ADR-008). The one apparent exception, `print`, is a compiler builtin re-exported by `std/io`; user code still imports it from there.

The complete module list, every function signature, and per-function semantics are specified in **`docs/STDLIB.md`** (the standard-library reference). `Result<T, E>` (§5.6, §19) is a documentation example, not an importable module — declare it locally with `type` where you need it.

### 22.8 Test Mocking — `replace`

A `replace` statement overrides an imported binding for the duration of a test
program. It is a **test-only** facility: it is permitted only in a `*.test.lin`
file, and using it anywhere else is a compile error (ADR-046).

```txt
import { readFile } from "std/fs"

replace readFile = (path: String): AnyVal => "mock contents of ${path}"
```

Syntax: `replace <name> = <expr>`, at the top level of a module, where `<name>` is
a binding brought in by an `import` above it. (`replace` is a contextual keyword —
it is an ordinary identifier everywhere except the start of this statement form, so
`std/string`'s `replace` function is unaffected.)

Semantics:

- **Whole-program override.** The `replace` body becomes the definition the entire
  test program uses for that export. Because an exported binding compiles to a single
  symbol, every reference resolves to the mock — the test file, the module under
  test, and any transitively-importing module, however the import path is spelled. A
  module that internally calls the replaced binding sees the mock with no change to
  itself. Delegation back to the original implementation is therefore not possible
  (a mock that calls its own replaced name recurses).
- **Type-checked.** The body is checked against the export's declared signature; a
  mismatch is a compile error.
- **Functions and vals.** Both a function export and a non-function `val` export may
  be replaced.
- **Stdlib is mockable** at the Lin-API level (e.g. `std/fs.readFile`,
  `std/time.now`). The polymorphic built-in primitives (`print`, `map`, `filter`,
  `reduce`, `for`, `length`, `toString`, and the concurrency family) are not
  replaceable — they are not ordinary linkable symbols.
- **Spies** are an ordinary mock that closes over a module-level `var`/`Shared` cell
  to record calls or arguments, asserted after the test run.

### 22.9 Test Lifecycle

The standard `std/test` framework provides setup/teardown without dedicated
keywords (see `docs/STDLIB.md`):

- **beforeAll**: a module-scope `val`/statement above the suite (test bodies run
  eagerly as the suite array is built).
- **afterAll**: statements after `report(suite)` — `report` returns the failure
  count instead of exiting, so teardown runs even when a test fails.
- **beforeEach/afterEach**: the `withFixture(setup, teardown, name, body)`
  combinator, which builds a fixture, injects it into the test body, and tears it
  down — failures are values, so teardown always runs.

## 23. Scoping

Bindings are lexical.

Blocks introduce nested scopes.

Closures capture bindings from their defining scope. Mutable bindings are captured as mutable cells: closures over the same `var` see the same storage.

---

# Part VIII — Concurrency

## 24. Concurrency

### 24.1 Design Principles

Concurrency in Lin follows the same pattern as iteration: opaque runtime values constructed with built-in functions, consumed by built-in functions. No new syntax is introduced. Functions do not carry an "async" colour — whether a function runs synchronously or on a separate thread is decided at the call site, not in the function's definition.

The concurrency runtime values — promises, thread pools, and workers — are **opaque** handles. `Promise<T>` is a first-class, resolvable opaque type (like `Shared<T>` and `Stream<T>`): `async` returns `Promise<T>`, `await` consumes it, and the checker tracks the payload `T` through the combinators (`race`/`timeout`/`retry`/`poolAsync`). It does not widen to `AnyVal`. `ThreadPool` and `Worker<Msg, Reply>` remain conceptual notations that are not resolvable annotation names — where you must annotate one, use `AnyVal`.

### 24.2 Promises

A promise (`Promise<T>`) represents a value of type `T` being computed on another OS thread. At runtime it is an opaque handle.

`T` must be a **transferable** type: JSON-compatible values (`String`, `Boolean`, `Null`, all numeric types, `T[]`, and object types whose fields are transferable). Functions, iterators, workers, thread pools, and promises are not transferable. Attempting to spawn a thunk that returns a non-transferable type is a compile-time error where statically detectable, and a runtime error otherwise.

#### 24.2.1 Spawning

`async` spawns a thunk on a new OS thread and immediately returns a promise:

```txt
val p = async(() => 1 + 1)
val p = (() => 1 + 1).async()   // dot form
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

#### 24.2.2 Awaiting

`await` blocks the calling thread until the promise resolves and returns the value. It is typed `<T>(p) => T | Error`: the union with `Error` is injected at `await`, so the result must handle the `Error` case (ADR-045).

```txt
val p = async(() => 1 + 1)
val result = await(p)                // Int32 | Error
val result = p.await()               // dot form

match result
  is Error => print("failed: ${result}")
  is Int32 => print("got ${result}")
```

An async fault surfaces as an `Error` value whose discriminant is the string-literal `"type": "error"`; since string literals in type position are singleton types (§19, decision-list 33), this tag is the same compile-time-checked discriminant used by user-defined tagged unions. A runtime error inside the thunk (array out of bounds, integer division by zero, non-exhaustive match, etc.) is caught at the OS thread boundary and surfaces as an `Error` value at the `await` call site rather than halting the program. This makes `async` a **fault isolation boundary** — the only place in Lin where runtime errors become recoverable values. The general rule that runtime errors are uncatchable (§20.1) does not apply inside an async thunk.

Because `await` returns `T | Error`, assigning its result to a bare target type is a compile-time error until the `Error` case is handled:

```txt
val p = async(() => 1 + 1)
val v: Int32 = await(p)   // compile error: Int32 | Error is not assignable to Int32
```

> **Implementation note (ADR-045).** The `T | Error` union is attached at `await`, not at `async`. A promise handle in flight has the opaque type `Promise<T>` — it does not widen to `AnyVal` — and only `await` materialises a result that can be an `Error`. The checker enforces "must handle the `Error`" after `await` AND, because `Promise<T>` is its own type, catches "forgot to `await`" (passing a `Promise<T>` where its resolved value is expected is a type error).

#### 24.2.3 Nested Promises

`await` auto-flattens nested promises. If the thunk itself returns a promise, `await` resolves through all layers. The result is still `T | Error` (the union does not nest):

```txt
val p = async(() => async(() => 42))
val v = await(p)   // Int32 | Error — 42 once the Error case is handled
match v
  is Error => print("failed")
  else     => print("${v}")   // 42
```

### 24.3 `parallel`

`parallel` is syntactic sugar for spawning an array of thunks and awaiting all results. It is the idiomatic fork/join form:

```txt
val [a, b, c] = parallel(
  () => expensiveA(),
  () => expensiveB(),
  () => expensiveC()
)
```

This is equivalent to:

```txt
val [a, b, c] = await([
  async(() => expensiveA()),
  async(() => expensiveB()),
  async(() => expensiveC())
])
```

Result order matches input order regardless of completion order.

`await` on an array of promises also works directly:

```txt
val [a, b] = await([myFunc, myFunc2].map(f => async(f)))
```

The thunks in `parallel` are subject to the same `var`-capture restriction as `async` (§24.2.1).

### 24.4 Promise Combinators

These are built-in functions on promises. All return a new promise:

```txt
map:     <T, U>(promise, (T) => U) => promise
race:    <T>(promise[]) => promise
timeout: <T>(promise, Int32) => promise        // resolved value or Null on timeout
retry:   <T>(() => T, Int32) => promise
```

**`map`** — transforms the resolved value without blocking:

```txt
val doubled = async(() => 21).map(v => v * 2)
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

**`timeout`** — resolves with the original value if the promise completes within the given number of milliseconds, or `Null` if it does not. The timed-out thread is abandoned (not cancelled — Lin has no cancellation). The awaited result type is `T | Error | Null`:

```txt
val result = await(timeout(p, 5000))

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

### 24.5 Thread Pools

By default each `async` call spawns a new OS thread. For high-fan-out work, a thread pool distributes tasks across a fixed number of threads:

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

`pool.async` has the same two overloads as the top-level `async`: single thunk `() => T` and array of thunks `(() => T)[]`. The same `var`-capture restriction applies (§24.2.1).

A thread pool is an opaque runtime value. It is not transferable across async boundaries.

### 24.6 Workers

A worker (conceptually `Worker<Msg, Reply>`) is a long-lived OS thread that processes messages sequentially. It is the right primitive for stateful concurrency (shared counters, connection pools, caches) and for isolating long-running background tasks.

#### 24.6.1 Construction

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

val w = worker(onMessage, onShutdown)
```

#### 24.6.2 Sending Messages

`message` is fire-and-forget — it enqueues the message and returns immediately:

```txt
message: <Msg, Reply>(worker, Msg) => Null

w.message("Hello")
```

`request` is synchronous — it enqueues the message and blocks until the handler returns, then returns the reply:

```txt
request: <Msg, Reply>(worker, Msg) => Reply

val reply: String = w.request("ping")
```

The handler's return value is the reply. If the handler returns `Null`, `request` and `message` are equivalent.

#### 24.6.3 Closing

```txt
close: <Msg, Reply>(worker) => Null

w.close()
```

`close` waits for any in-progress message to finish, calls the shutdown handler, then terminates the worker thread. Sending a message or request to a closed worker is a runtime error.

#### 24.6.4 Worker State and `var`

A worker's message handler may close over `var` bindings to maintain state across messages. This is safe because the worker is single-threaded: messages are processed one at a time, with no concurrent access to the worker's closed-over state.

```txt
val makeCounter = () =>
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

#### 24.6.5 Worker Lifetime and Errors

A runtime error inside a message handler kills the worker. The current `request` call (if any) causes the program to halt with the worker's diagnostic. Subsequent `message` or `request` calls to a dead worker are also runtime errors.

#### 24.6.6 Transferability

`Msg` and `Reply` must be transferable types (§24.2). Functions that close over no `var` bindings may be sent as messages.

### 24.7 Shared State

Lin's concurrency is share-nothing: thunks may not capture `var` (§24.2.1) and transferred values are deep-copied. For genuine shared mutable state, `std/async` provides an opt-in `Shared<T>` box accessed only through `shared`/`get`/`set`/`withLock` (ADR-029); for shared read-only state, `frozen` (ADR-030). There is **no** mutex/atomics primitive — cross-thread mutable state is otherwise modelled with a `Worker` that owns the state and serialises access through its message queue (ADR-039).

### 24.8 `print` Ordering

All workers and async thunks share a single stdout. `print` is line-atomic: a full line is written without interleaving with output from other threads. Partial output within a single `print` call will not be split.

### 24.9 Summary Table

| Primitive | Use case | Blocks caller? | Awaited result |
| --- | --- | --- | --- |
| `async(f)` | Spawn one thunk, retrieve later | No (until `await`) | — |
| `await(p)` | Block until promise resolves | Yes | `T \| Error` |
| `parallel(f1, f2, ...)` | Fork/join, all results needed | Yes | `[T \| Error, ...]` |
| `race(ps)` | First result wins | No (until `await`) | `T \| Error` |
| `timeout(p, ms)` | Bound wait time | No (until `await`) | `T \| Error \| Null` |
| `retry(f, n)` | Retry on runtime error | No (until `await`) | `T \| Error` |
| `threadPool(n).async(...)` | High-fan-out work, bounded threads | No (until `await`) | `T \| Error` |
| `worker(onMsg, onShutdown)` | Long-lived stateful thread | No | — |
| `w.message(x)` | Fire-and-forget message | No | `Null` |
| `w.request(x)` | Synchronous request/reply | Yes | `Reply` |

---

# Part IX — Systems Programming

## 25. IO, Filesystem, and HTTP Intrinsics

The functions in `std/io`, `std/fs`, and `std/http` cannot be implemented in Lin because they require OS-level syscalls and network access. They are registered as host-language (Rust) intrinsics and exposed to Lin programs via the module system exactly like any other export. Their conventions are specified here; full signatures are in `docs/STDLIB.md`.

### 25.1 Design Principles

All three modules follow the same conventions:

1. **Blocking by default.** Every function runs synchronously. Use `async` at the call site when concurrency is needed.
2. **`T | Error` for fallible operations.** A function that may fail at the OS or network level returns `T | Error`, where `Error` is the conventional error value `{ "type": "error", "message": String }` (see §20, §24.2.2), detected with `is Error`. HTTP error status codes are not transport errors and do not produce `Error`.
3. **`Iterator` for sequences.** Line-oriented reads return iterators rather than loading everything into memory.
4. **No hidden global state.** Stdin, stdout, and the filesystem are the implicit context; there are no open-handle values exposed to user code.

### 25.2 `std/io` Intrinsics

The following are implemented as Rust intrinsics. `print` is additionally available as a global without importing.

```txt
__ioPrint:    (AnyVal)   => Null      // formats and writes to stdout + newline
__ioReadLine: ()       => String | Null   // one line from stdin, Null on EOF
__ioLines:    ()       => Iterator        // iterator over stdin lines
__ioReadAll:  ()       => String          // all of stdin as one string
```

The Lin stdlib wrappers in `std/io` delegate directly to these intrinsics:

```txt
export val print    = (v: AnyVal): Null           => __ioPrint(v)
export val readLine = (): String | Null         => __ioReadLine()
export val lines    = (): Iterator              => __ioLines()
export val readAll  = (): String                => __ioReadAll()
```

### 25.3 `std/fs` Intrinsics

```txt
__fsReadFile:   (path: String)                    => String | Error
__fsWriteFile:  (path: String, content: String)   => Null | Error
__fsAppendFile: (path: String, content: String)   => Null | Error
__fsReadLines:  (path: String)                    => Iterator | Error
__FSreadJson:   (path: String)                    => AnyVal | Error
__FSwriteJson:  (path: String, value: AnyVal)       => Null | Error
__fsExists:     (path: String)                    => Boolean
```

The Lin stdlib wrappers in `std/fs` delegate directly:

```txt
export val readFile   = (path: String): String | Error          => __fsReadFile(path)
export val writeFile  = (path: String, content: String): Null | Error  => __fsWriteFile(path, content)
export val appendFile = (path: String, content: String): Null | Error  => __fsAppendFile(path, content)
export val readLines  = (path: String): Iterator | Error        => __fsReadLines(path)
export val readJson   = (path: String): AnyVal | Error            => __FSreadJson(path)
export val writeJson  = (path: String, value: AnyVal): Null | Error => __FSwriteJson(path, value)
export val exists     = (path: String): Boolean                 => __fsExists(path)
```

### 25.4 `std/http` Intrinsics

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
__httpFetch:     (url: String)                          => HttpResponse | Error
__httpFetchWith: (url: String, options: HttpOptions)    => HttpResponse | Error
```

The higher-level functions `fetchJson` and `postJson` are written in Lin on top of these two intrinsics:

```txt
export val fetch     = (url: String): HttpResponse | Error          => __httpFetch(url)
export val fetchWith = (url: String, opts: HttpOptions): HttpResponse | Error =>
  __httpFetchWith(url, opts)

export val fetchJson = (url: String): AnyVal | Error =>
  val resp = __httpFetch(url)
  if resp is Error then resp
  else if resp["status"] >= 200 && resp["status"] < 300 then
    parseJson(resp["body"])
  else
    { "type": "error", "message": "HTTP ${resp["status"]}" }

export val postJson = (url: String, body: AnyVal): HttpResponse | Error =>
  __httpFetchWith(url, {
    "method":  "POST",
    "headers": { "Content-Type": "application/json" },
    "body":    toString(body)
  })
```

`parseJson` is an intrinsic (`__parseJson: (String) => AnyVal | Error`) that parses a JSON string into a Lin value. It is not part of the public stdlib API but is available internally to `std/http`.

### 25.5 HTTP Server (`serve`)

Server support lives in `std/http` alongside the client. The serving loop itself is the one intrinsic; everything else (`json`, `text`, `redirect`, `notFound`, `badRequest`, `matchPath`, `parseBody`) is written in Lin on top of the `HttpResponse` type and `__parseJson`.

```txt
__serverServe: (handler: (HttpRequest) => HttpResponse, port: Int32) => Null
```

`__serverServe` binds a TCP listener on `port`, then serves connections **sequentially** (one request at a time): it parses each incoming HTTP/1.1 request into an `HttpRequest`, invokes `handler`, and writes the returned `HttpResponse` back on the wire. It blocks indefinitely (it only returns — as an `Error`-shaped value — if the port cannot be bound). The handler runs inside a fault-isolation boundary: a faulting handler yields a `500` response and the server keeps serving.

The handler argument comes **first** so the dot-call form reads naturally: `router.serve(3000)` desugars (first-argument application, §16.1) to `serve(router, 3000)`. Both forms are equivalent.

A pool-dispatched variant (`pool.serve`, concurrent request handling) is not yet implemented.

The Lin stdlib wrappers in `std/http`:

```txt
export val serve = (handler: (HttpRequest) => HttpResponse, port: Int32): Null =>
  __serverServe(handler, port)

export val json = (status: Int32, body: AnyVal): HttpResponse => {
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

export val parseBody = (req: HttpRequest): AnyVal | Error =>
  __parseJson(req["body"])

export val matchPath = (path: String, pattern: String): { ...String } | Null =>
  __serverPathMatch(pattern, path)
```

`__serverPathMatch` is a Rust intrinsic that splits both strings on `/`, matches literal segments exactly, captures `:name` segments by name, and returns `Null` on any mismatch. `matchPath` takes the **path first** so it reads as `req["path"].matchPath("/users/:id")` in dot-call form.

## 26. Foreign Function Interface

Lin provides a C-compatible FFI so that programs can call into native libraries written in C or Rust.

### 26.1 Design Principles

1. **C ABI only.** Lin speaks the C calling convention. C libraries are called directly. Rust libraries must expose their public API as `extern "C"` functions (with `#[no_mangle]`).
2. **Explicit, flat signatures.** Only a restricted set of Lin types are legal in `foreign` signatures — those that map cleanly onto C types. Richer Lin values cannot cross the boundary without explicit conversion.
3. **Static linking.** Foreign declarations are resolved at `lin build` time by the linker. There is no runtime `dlopen`.
4. **Unsafe by nature.** The compiler trusts the declared types. A mismatch between the declared Lin type and the actual C signature is undefined behaviour. It is the programmer's responsibility to get it right.

### 26.2 `import foreign` Syntax

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

### 26.3 Legal Foreign Types

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
| `String`                    | `LinString` (pointer + length, see §26.4) |

All other Lin types (`AnyVal`, object types, array types, `Iterator`, `Function`, etc.) are not legal in foreign signatures. Attempting to declare one is a compile-time error.

### 26.4 String Passing Convention

Lin strings are UTF-8 length-prefixed values and do not carry a null terminator. Passing a `String` across the FFI boundary uses the `LinString` struct, which the C header `lin.h` defines as:

```c
typedef struct {
    const uint8_t *ptr;
    size_t         len;
} LinString;
```

The C function receives a `LinString` by value. The pointed-to bytes are owned by the Lin runtime and must not be freed or stored past the function call. If the C side needs to retain the data it must copy it.

Returning a `String` from a foreign function is not supported. A function that needs to return text should write into a caller-supplied buffer or use an `Int32` return code and a side channel.

### 26.5 Rust Libraries

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

### 26.6 Static Analysis

The type checker treats every foreign binding as having the declared type and performs no further checking of the library contents. Foreign signatures participate in the normal type system — the declared argument and return types are enforced at every call site in Lin code.

## 27. Low-Level Primitives

Lin's domain includes low-level systems code (binary protocols, byte parsing, sockets, subprocesses). This section specifies the primitives that make such code expressible: byte buffers, bitwise operators, and a small family of OS intrinsics. They follow the existing conventions — opaque scalar handles, the `T | Error` result shape, and stdlib wrappers over Rust intrinsics — and introduce no new runtime *kinds* beyond what the unboxed-array and FFI machinery already provide.

### 27.1 Byte Buffers and Small-Integer Arrays

The small integer families `Int8`, `UInt8`, `Int16`, `UInt16` have an unboxed, contiguous array representation, exactly like `Int32`/`Int64`/`Float32`/`Float64` (§28.4). An array typed `UInt8[]` is a packed byte buffer — one byte per element, no per-element tag.

```txt
val packet: UInt8[] = [0u8, 1u8, 255u8]
val b = packet[0]            // UInt8
packet[1] = 42u8             // in-place write (§7, index assignment)
val n = length(packet)       // Int32
```

These arrays support every array operation (literals, indexing, in-place index assignment, `length`, `push`, the `std/array` combinators, equality). The representation is an implementation detail; semantically they are ordinary `T[]` arrays whose element type happens to be a small integer.

### 27.2 Bitwise Operators

Lin provides the bitwise binary operators and one unary operator:

```txt
&    bitwise and
|    bitwise or        (value position; in type position `|` is the union separator)
^    bitwise xor
<<   left shift
>>   right shift       (logical for unsigned types, arithmetic for signed)
~    bitwise not       (unary)
```

There are two unary operators in the language: bitwise `~` (here) and logical `!` (§8.1). A leading `-` is not a unary operator: it is part of a numeric literal or parse-time sugar for `0 - x` (§2.7).

**Typing.** Bitwise and shift operators require **integer** operands; a floating-point operand is a compile-time error. For `&`, `|`, `^`, the result type is the widened integer type of the two operands (§21). For `<<` and `>>`, the result type is the type of the left operand and the right operand may be any integer. For `~x`, the result type is the type of `x`. The logical-not operator `!x` requires a `Boolean` operand and yields `Boolean`.

**Precedence.** The new operators slot into the §8.2 ladder as shown there: shifts bind tighter than comparison; `&`, `^`, `|` bind between equality and `&&`, in that order (tightest first). `~` and `!` bind tighter than `*` (and are right-associative).

```txt
val nalType = header & 0x1F            // extract low 5 bits
val fuHeader = nri | 28                // set FU-A type bits
val flagged = fuHeader | 0x80          // set start bit
val high = (value >> 24) & 0xFF        // top byte of a UInt32
val inverted = ~mask                   // bitwise complement
```

`|` is unambiguous because type expressions and value expressions never overlap syntactically; the parser knows which context it is in.

### 27.3 `std/bytes`

`std/bytes` provides slicing and endian (de)serialization. The endian helpers are written in Lin on top of §27.1 and §27.2, **plus the explicit narrowing casts of §21** (exported from `std/number`). Extracting a byte from a wider integer — e.g. `(v >> 24) & 0xFF` for a `UInt32` `v` — yields a `UInt32`, which cannot be *implicitly* narrowed to a `UInt8` (§21 makes implicit narrowing a compile-time error), so an explicit `toUInt8(...)` cast is required. Conversely, assembling a wide integer from bytes widens each byte first (`toUInt32(b[off]) << 24 | ...`). The four float bit-reinterpret functions are the only true intrinsics here (a float's bit pattern cannot be obtained by shift-and-mask).

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

The narrowing casts that back the byte-extraction live in `std/number` (§21): `toUInt8`, `toInt8`, `toUInt16`, `toInt16`, `toUInt32`, `toInt64`, `toUInt64`, each `(UInt64) => <target>`, truncating with two's-complement (`as`-cast) semantics.

Slicing is a function, `slice(buf, start, end)`; there is no range-index syntax (`buf[a..b]`). `slice` preserves element type — slicing a `UInt8[]` yields a `UInt8[]`.

### 27.4 OS Handle Convention

Operating-system resources (sockets, subprocesses) are exposed to Lin as **opaque integer handles**, not as runtime object values. A handle is an `Int32` (or `Int64`) that the runtime interprets; there are no open-handle objects in user code (consistent with §25.1). This is the same convention `std/time` uses for timers.

All fallible operations return the `T | Error` result shape (§25.1). A non-blocking read that has no data available yet returns `Null` rather than `Error`, so a poll loop reads naturally.

### 27.5 `std/net` — Sockets

Both UDP and TCP sockets are exposed via runtime intrinsics. Every socket is an opaque integer fd handle (§27.4), and every fallible call returns the `T | Error` result shape; a non-blocking read with no data available yet returns `Null`.

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

`recv` fills a caller-owned `UInt8[]` (§27.1) and returns the number of bytes read; the buffer is never transferred across the boundary. Non-blocking mode plus a `Null`-on-would-block `recv`/`accept` replaces an explicit `poll`.

Note that `std/http` already provides a high-level blocking HTTP server (`serve`, §25.5) and an HTTP client (§25.4); `std/net` is the lower-level byte-stream layer beneath them, for non-HTTP protocols and custom framing.

### 27.6 `std/process` — External Processes

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

### 27.7 `std/tty` — Raw Terminal

```txt
rawMode:  (on: Boolean)  => Null | Error    // enable/disable terminal raw mode
readKey:  ()             => Int32 | Null    // keycode, or Null if no key available (non-blocking)
```

### 27.8 Timing and Signals

`std/time` provides microsecond sleep (alongside the millisecond-granularity `sleep`):

```txt
sleepMicros: (n: Int64) => Null
```

`std/signal` provides minimal signal handling:

```txt
waitSignal: (sig: Int32) => Int32           // block until the signal is delivered
```

### 27.9 Streams

A `Stream<T>` is an opaque runtime value (§18, §28) representing a **lazy, effectful, fallible sequence** of values pulled from an OS resource. It is a sibling of `Iterator<T>` but a distinct type: an iterator is a pure, restartable description built from side-effect-free state functions (§18.2), whereas a stream owns an fd, advances a read position with each pull, and can fail mid-traversal. For that reason a stream is **not** modelled as an iterator, and its protocol is private (it is not JSON, and not subscriptable). See ADR-050.

`Stream<T>` is covariant in `T`. The conceptual notation `Stream<T>` is a resolvable type name in `val`/parameter/return annotations (subject to the placement restriction in §27.9.5).

#### 27.9.1 The Pull Graph and Push Sink Model

A stream is built as a **lazy pull graph**. A *source* node is at the root (a file, socket, subprocess, or stdin); each *adapter* (`lines`, `chunks`, `map`, `filter`, `take`) wraps an upstream stream and transforms items as they are pulled. Nothing is read until a **terminal** operation drives the graph. A `writeStream`/`writeLines` builds a **push sink** node: pulling the sink pulls one item from upstream and writes it to the destination, so the whole pipeline runs one item at a time with bounded memory. `writeStream` writes each item's bytes **verbatim** (raw — no separator, the correct sink for binary output); `writeLines` writes each item followed by a newline (one item per line).

```txt
readStream("in.csv")        // source:  Stream<UInt8[]>
  .lines()                  // adapter: Stream<String>
  .map(transform)           // adapter: Stream<String>
  .filter(removeEmptyLines) // adapter: Stream<String>
  .writeLines("out.csv")    // sink node (a Stream whose terminal writes each line)
  .drain()                  // terminal: drive on this thread -> Null | Error
```

Errors are threaded **in-band** through the graph: a read error poisons the upstream, and every downstream adapter becomes a passthrough, so the first error short-circuits straight to the terminal op without an `is Error` check at every step. This is the §20 value-based error convention applied to the lazy graph (ADR-050).

#### 27.9.2 Unified Sources

File, TCP, subprocess-stdout, and stdin are all **byte streams** — `Stream<UInt8[]>` — each supplying a different read backend. Bytes are fundamental; line- and text-oriented views are adapters.

```txt
readStream:        (path: String)        => Stream<UInt8[]>   // std/stream
tcpStream:         (fd: Int32)           => Stream<UInt8[]>   // std/net
stdoutStream:      (h: ProcessHandle)    => Stream<UInt8[]>   // std/process
stdinStream:       ()                    => Stream<UInt8[]>   // std/io
```

A would-block / EOF condition ends the stream normally; a hard read failure ends it with an `Error` (the exact ending is source-kind-dependent, §27.9.4).

#### 27.9.3 Adapters

Adapters are lazy: they return a new `Stream` and read nothing until driven.

```txt
lines:   (Stream<UInt8[]>)               => Stream<String>    // split byte stream into lines
chunks:  (Stream<UInt8[]>, n: Int32)     => Stream<UInt8[]>   // re-chunk to n-byte windows
map:     <T, U>(Stream<T>, (T) => U)        => Stream<U>
filter:  <T>(Stream<T>, (T) => Boolean)     => Stream<T>
take:    <T>(Stream<T>, n: Int32)           => Stream<T>      // first n items then end
```

`lines`/`chunks` are stream-specific adapters (`std/stream`). The transform combinators `map`/`filter`/`take`/`drop`/`flatMap`/`takeWhile`/`dropWhile`/`flatten`/`concat` shown here are **not** stream-only: they are the unified `std/iter` combinators (§18.7), which return a lazy `Stream` node when their receiver is a stream and an eager array otherwise. Their callbacks use the same dot-application and lambda forms as over an array (§18.5); they are pure transforms over each item, run one at a time as items are pulled (effects such as printing are allowed).

#### 27.9.4 Terminal Operations

A terminal operation drives the pull graph. There are synchronous reads, a `for`-consumer, and two pipeline drivers.

```txt
readText: (Stream<UInt8[]>)              => String | Error    // drain a byte stream to one String
collect:  (Stream<UInt8[]>)              => UInt8[] | Error    // drain to one byte buffer
for:      <T>(Stream<T>, (T) => Null)       => Null | Error    // consume each item (dot form .for(fn))
drain:    (Stream<T>)                    => Null | Error       // run a sink pipeline on this thread
promise:  (Stream<T>)                    => Promise<Null | Error>  // run on a worker thread
```

**`.for(fn)`** consumes a stream item by item (Lin has no `for…in`; iteration is always `.for(fn)`, §18). It returns **`Null | Error`** — not `Null` like an array `for` (§18.1) — because driving a stream can end two ways: **EOF ends the loop normally** (the expression is `Null`), and **a read `Error` mid-traversal becomes the expression's value**.

```txt
val outcome = readStream("in.log").lines().for(line =>
  print(line)
)
match outcome
  is Error => print("read failed: ${outcome["message"]}")
  else     => null
```

**`.drain()`** drives a sink pipeline (typically ending in a `writeStream` or `writeLines`) on the **calling thread** and returns `Null | Error`. It uses no new runtime machinery — it is the synchronous driver.

**`.promise()`** **moves** the whole pipeline onto a **worker OS thread** (ADR-049 move-transfer) and returns `Promise<Null | Error>`. This gives real concurrency and **fault isolation**: a runtime fault while the worker drives the stream is caught at the thread boundary and surfaces as an `Error` when the promise is awaited, exactly as for any `async` thunk (§24.2.2). Awaiting follows the usual rule — the result is `Null | Error` and the `Error` case must be handled (§24.2.2, ADR-045).

```txt
val p = readStream("big.log")
  .lines()
  .filter(isError)
  .writeLines("errors.log")
  .promise()
match await(p)
  is Error => print("pipeline failed")
  else     => print("done")
```

Because the pipeline is moved (not copied) and the worker becomes its sole owner, the worker's RC-drop finalizer closes the fd — no value is shared across threads, so non-atomic refcounting stays sound (§28, ADR-049).

#### 27.9.5 Affine Semantics and Lifetime

A `Stream<T>` is an **affine resource** (use-at-most-once; ADR-049):

- **Single use.** A stream is consumed by passing it as an argument, returning it, or applying a terminal op. Using a stream again after it has been consumed (or moved to a worker by `.promise()`) is a **compile-time error** (`use of moved value`).
- **Dropping is fine.** A stream that is never consumed is **not** an error — its fd is closed deterministically by the **RC-drop finalizer** when the last reference goes away (§28, ADR-050). Building a stream and never driving it is almost certainly a mistake, so the checker emits a **warning** (`stream is never consumed`), but it does not block compilation.
- **Explicit close.** `close(s)` (`std/stream`) closes the fd eagerly and is **idempotent**; it is for callers who want deterministic timing rather than scope-end cleanup.
- **Placement restriction (v1).** A `Stream` value may live only in a `val` binding, a function parameter, or a return position. It may **not** be stored in an object field, an array element, or a `var`. This is a deliberately narrow v1 rule (it keeps the use-after-move check confined to local single-name bindings) and is relaxable later. Storing a stream in a container or `var` is a compile-time error.

#### 27.9.6 Worked Example

A streaming CSV transform — quote each field and re-join with `|`, dropping blank lines — running line by line with bounded memory:

```txt
import { readStream, lines, writeLines, drain } from "std/stream"
import { map, filter } from "std/iter"

val transform = (line: String): String =>
  line.split(",").map(f => "\"${f}\"").join("|")   // a,b,c -> "a"|"b"|"c"

val removeEmptyLines = (line: String): Boolean =>
  !line.isBlank()

val run = (): Null | Error =>
  readStream("in.csv")
    .lines()
    .map(transform)
    .filter(removeEmptyLines)
    .writeLines("out.csv")
    .drain()
```

### 27.10 What Is Deliberately Absent

Two systems facilities are **not** provided as core primitives, by design:

- **GPIO / hardware register access.** Use the C FFI (§26) to bind a native GPIO library. The only language-level support added for it is `sleepMicros` (§27.8), needed for software PWM timing.
- **Shared-memory concurrency** (mutexes, atomics, shared mutable cells across threads). Lin's concurrency is share-nothing (§24). Cross-thread mutable state is modelled with a `Worker` (§24.6) that owns the state and serialises access through its message queue, or the opt-in `Shared<T>`/`Frozen` boxes (§24.7). This preserves the share-nothing invariant rather than reintroducing data races.

---

# Part X — Semantics and Implementation

## 28. Runtime Model

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
Module
```

Objects and arrays are JSON-compatible. Functions, iterators, and modules are runtime values but are not JSON values. Promises, thread pools, and workers (§24) are additional opaque runtime values.

### 28.1 Strings

Strings are stored as length-prefixed UTF-8 byte sequences. Indexing and slicing primitives in `std/string` operate at the Unicode codepoint level, not the byte level. Byte-level access, if needed, is provided by separate stdlib functions.

### 28.2 Closures and `var`

Closures capture `var` bindings by reference. Two closures that capture the same `var` share the same underlying storage cell — writes from one are visible to the other.

### 28.3 Tail Call Optimisation

The compiler performs tail call optimisation for **direct self-recursive calls** in tail position. Recursive idioms (factorial, iterator construction over large sequences) run in constant stack space when expressed tail-recursively. Mutual tail recursion is not optimised.

### 28.4 Numbers

Each numeric family has a distinct runtime representation. There is no single runtime "number" representation: numeric values carry their family tag at runtime so that operations can dispatch on the correct width and signedness.

### 28.5 Objects

JSON objects are stored as insertion-ordered key/value maps. Iteration order matches insertion order. Equality is order-independent (§9).

Object spread in literals (§3.3) inserts each source entry in source-iteration order. If a key was already present, the value is replaced but its original position is preserved.

### 28.6 Iterators

An iterator is an opaque runtime value containing:

1. an initial-state thunk,
2. a continuation predicate,
3. a next-state function,
4. a current-value function,
5. the current state cell (set lazily on first step).

Only the `for` built-in may step through this state; user code cannot read it.

### 28.7 Partial Application

Partial application produces a value carrying the original function pointer and the accumulated arguments. Further application appends to the buffer. When the buffer matches the original arity, the function is invoked. This avoids allocating a new closure per argument.

### 28.8 Memory Management

Heap values (strings, arrays, objects, closures) use deterministic reference counting inserted by the compiler; a Perceus-style elision pass removes most RC operations on the common functional-style path. Reference cycles between long-lived heap objects are not collected and will leak — break a cycle by nulling a field before the data becomes unreachable (ADR-024).

### 28.9 `toString`

Every primitive supports `toString`:

- Integers: decimal, no leading zeros, with `-` for negatives.
- Floats: shortest round-trip decimal representation; integer-valued floats render with a trailing `.0` (e.g. `42.0`).
- `Boolean`: `"true"` / `"false"`.
- `Null`: `"null"`.
- `String`: returns itself.

`toString` is used implicitly by string interpolation `${expr}`.

### 28.10 Comparison

`<`, `<=`, `>`, `>=` on strings compare by codepoint order. On numbers, by mathematical value after widening (§21). On other types: compile-time error.

### 28.11 `length()`

`length()` is defined for:

- `String` → number of codepoints (`Int32`).
- `T[]` → number of elements (`Int32`).
- `AnyVal` → for arrays, element count; for objects, key count; for any other variant, runtime error.

It is **not** defined on plain objects of declared shape — those have a fixed schema.

## 29. Compilation Model

The language is compiled, not interpreted from the user's perspective. The compilation pipeline:

```txt
source (.lin files)
  -> lexer
  -> indentation-aware token stream
  -> parser
  -> surface AST
  -> type checking (lin-check) -> typed module
  -> flat 3-address IR (lin-ir) -> RC elision
  -> code generation (LLVM via lin-codegen)
  -> single native binary
```

A program is built from one entry-point `.lin` file and its transitive imports, and emitted as a single native binary. The language is pure beyond `print`, the IO/filesystem/network intrinsics (§25, §27), and the concurrency primitives (§24).

### 29.1 Reference Implementation

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
    lin-lsp/                 language server (in progress)
  stdlib/                    stdlib .lin files
  docs/
  examples/
```

The backend is the LLVM native-code compiler in `lin-codegen`. Source compiles to a standalone native binary via `lin build`.

### 29.2 Diagnostics

The compiler halts at the first error in a given phase. Errors are presented with:

- the source span (file, line, column),
- the surrounding source excerpt,
- the rule violated,
- where applicable, a call stack for runtime errors.

The first-error policy keeps the implementation simple; multi-error recovery is deferred (Appendix B).

## 30. Implementation Notes

Important desugarings:

```txt
x.f(y, z)         becomes  f(x, y, z)
x.f               becomes  f(x)             // partial application
(x, y).f          becomes  f(x, y)          // partial application
val { name } = p  becomes  val name = p["name"]
-x                becomes  0 - x            // leading minus on a non-literal
```

Imports bind the named exports of the resolved module into the current scope. Type-only imports erase at runtime.

Iterator construction is not desugared to JSON object construction — it creates an opaque runtime iterator value. `for` is implemented inside the compiler/runtime and is the only consumer that may step through that opaque value directly.

---

# Appendices

## Appendix A — Complete Example

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

## Appendix B — Status and Open Questions

### B.1 Decided

1. `export` may be used on `val`, `var`, and `type` declarations.
2. `AnyVal` is a built-in type. `Number` is a *conceptual* union over the numeric families used in prose/signatures, **not** a resolvable type-annotation name (§3.2, §4.1).
3. `Unknown` and `Never` are not built-in types.
4. `is Person` checks every declared field is present and correctly typed (recursively; extra fields allowed — ADR-036); `has Person` or `has { ... }` checks only that the requested fields are present (types not validated). Both allow extra fields; arrays match length-exactly (§12.1).
5. `is` on generic type applications is unsupported.
6. `is`/`has` are expressions of type `Boolean` and may appear in any expression context.
7. A single `match` arm uses either `is` or `has`, not both.
8. Assignment expressions evaluate to the assigned value.
9. Operators are built-in, not ordinary functions. Exactly two unary operators — bitwise `~` and logical `!`. A leading `-` is not a unary operator: it is part of a numeric literal or parse-time sugar for `0 - x` (§2.7, §8.1, §27.2, ADR-031).
10. `Iterator<T>` is an opaque runtime type. "Iterable" is conceptual (arrays and iterators), not a resolvable name.
11. Arrays can be iterated directly by `for` and the `std/array` combinators.
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
23. The compiler performs TCO for direct self-recursive tail calls.
24. Generic inference uses bidirectional type checking.
25. Exhaustiveness is a compile-time error for closed `is`/literal unions and a warning otherwise; non-exhaustive runtime fall-through is a runtime error.
26. Numeric widening is always to a type that can fully represent both operand ranges; widening is applied everywhere (operators, calls, returns, assignments) but narrowing is never implicit.
27. The standard-library module list and signatures live in `docs/STDLIB.md`; names are imported explicitly (nothing is auto-imported as a global). `range`/`iterOf` are in `std/array`; there is no `std/iter` or `std/result` module.
28. Two-space indentation; `&&`/`||` may begin a continuation line at any deeper indent.
29. Imports are resolved eagerly at compile time; each module is checked once and cached (cyclic members are checked together). Cyclic function references are supported (§22.5); only a cyclic value initialisation is a compile-time error. A diamond is not a cycle.
30. Bracket access is safe: missing object key → `Null`, `Null` propagates; array OOB is a runtime error.
31. Generic types are covariant in producer positions, contravariant in consumer positions.
32. Type-expression precedence: `[]` > `<>` > `=>` > `|`.
33. Literal types: a string literal in **type** position is a singleton type (`type Tag = "ok"` admits only the value `"ok"`). A string literal as a **value** still infers to its base type (`val x = "Dave"` is `String`); the singleton is obtained by checking it against an expected literal type. A literal widens to `String`; `String` does not narrow to a literal. Numeric/boolean literal types are not supported.
34. Runtime errors halt the program; they cannot be caught, except inside an `async` thunk (§24.2.2).
35. Integer division by zero is a runtime error; floating-point follows IEEE 754.
36. `toString` is defined for every primitive (§28.9); used implicitly by string interpolation.
37. `length()` works on `String`, `T[]`, and `AnyVal` (array or object variants).
38. Comparison `<`, `<=`, `>`, `>=` uses codepoint order for strings, mathematical order for numbers.
39. Source files use LF line endings; CRLF is rejected.
40. Blank lines inside indented blocks are allowed and ignored.

### B.2 Known Gaps and Deferred Work

- **`Float8` / `Float16`.** Listed in earlier drafts; not implemented. Only `Float32`/`Float64` exist.
- **Nominal `ThreadPool` / `Worker<Msg,Reply>` types.** These remain opaque runtime values erased to `AnyVal` (§4.1, §24.1). (`Promise<T>` *is* now a first-class resolvable opaque type — §24.1, ADR-045 — so "forgot to `await`" is caught.)
- **`Number` / `Iterable<T>` as resolvable names.** Conceptual only; use `AnyVal` or concrete families / `Iterator<T>`.
- **Mutual tail-call optimisation.** Only direct self-recursive tail calls are optimised (§28.3).
- **Bytecode or JIT target.** Native code via LLVM only.
- **Tooling.** Formatter exists but does not preserve comments (ADR-025); LSP is in progress; a first-class test runner command (`lin test`) exists for `*.test.lin`.
- **Full numeric widening matrix.** §21 specifies the principle; the complete pairwise table is deferred.
- **Multi-error reporting.** First-error-then-halt policy; recoverable parsing/checking is deferred.
- **`pool.serve`.** Concurrent (pool-dispatched) HTTP serving is not yet implemented; `serve` is sequential (§25.5).
