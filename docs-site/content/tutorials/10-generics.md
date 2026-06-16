# Generics

Generics let you write types and functions that work over many types while keeping the relationships between them precise. Lin has generic *type* declarations and generic *functions*, both written with angle brackets.

## Generic types

A type declaration can take type parameters in `<...>`. The parameters stand in for concrete types that are filled in when the type is used:

```lin
type Box<T> = {
  "value": T,
  "label": String
}

type Pair<A, B> = {
  "first": A,
  "second": B
}
```

You apply a generic type by supplying concrete types for its parameters:

```lin
val score: Box<Int32> = { "value": 90, "label": "score" }
val entry: Pair<String, Int32> = { "first": "age", "second": 36 }
```

The most common use is a tagged-union result type, parameterised over the success and error payloads:

```lin
type Result<T, E> =
  | { "type": "success", "value": T }
  | { "type": "failure", "error": E }
```

`Result<Int32, String>` is a value that is either `{ "type": "success", "value": Int32 }` or `{ "type": "failure", "error": String }`.

## Generic functions

A `val` function can declare its own type parameters before the argument list. The simplest is the identity function, which returns its argument unchanged at whatever type it was given:

```lin
import { print } from "std/io"
import { toString } from "std/string"

val identity = <T>(x: T): T => x

print(toString(identity(42)))   // 42
print(identity("hello"))        // hello
```

You never write the type arguments at the call site — they are inferred from the values you pass. Use more than one parameter when the types relate to one another:

```lin
val pair = <A, B>(a: A, b: B): { "first": A, "second": B } =>
  { "first": a, "second": b }

val p = pair(1, "x")    // { "first": Int32, "second": String }
```

The real value of a type parameter is that it ties parts of the signature together. `firstOf` promises that the element it returns has the *same* type as the elements of the array it was given:

```lin
val firstOf = <T>(xs: T[]): T => xs[0]

print(firstOf([10, 20, 30]))    // 10  — an Int32
print(firstOf(["a", "b"]))      // a   — a String
```

## Variance

When one generic type is assignable to another, Lin follows the usual rules. Generic types are **covariant** in producer positions (return types, array elements, container contents) and **contravariant** in consumer positions (function arguments).

In practice that means a more specific type flows into a more general one where you'd expect:

```lin
import { length } from "std/array"

type Person = { "name": String, "age": Int32 }

val countAll = (items: AnyVal[]): Int32 => length(items)

val people: Person[] = [
  { "name": "Ada", "age": 36 },
  { "name": "Bob", "age": 41 }
]

countAll(people)   // Person[] is assignable to AnyVal[]
```

Likewise, `Iterator<Person>` is assignable to `Iterator<AnyVal>`, and a function returning `Person` is assignable to one returning `AnyVal`.

## Matching generic values

There is one limitation to know: you **cannot** use a generic type application in an `is` pattern. Writing `is Result<Int32, String>` is a compile-time error. Instead, match the underlying tagged shape with `has`:

```lin
val describe = (r: Result<Int32, String>): String =>
  match r
    has { "type": "success", value } => "ok: ${value}"
    has { "type": "failure", error } => "failed: ${error}"
    else                             => "unknown"

val ok: Result<Int32, String> = { "type": "success", "value": 7 }
describe(ok)   // "ok: 7"
```

This is usually what you want anyway — you match on the discriminant tag, and the bound fields come out at their declared types.

Note the type annotation on `ok`. A bare object literal infers its `"type"` field as `String`, not the singleton literal `"success"` the union expects, so annotate the binding (or the parameter it flows into) to construct a `Result` value.

## What's next?

- [Pattern Matching](/tutorials/05-pattern-matching.html) — matching on the tagged shapes generic unions produce
- [Error Handling](/tutorials/08-error-handling.html) — `Result<T, E>` and the built-in `Error` type in practice
- [Types reference](/reference/types.html#generic-types) — the full rules, including type-expression precedence
