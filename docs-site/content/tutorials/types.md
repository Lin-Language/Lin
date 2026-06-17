# Types

Lin is statically typed, and its type system is built around JSON-shaped data. This page introduces the type forms you compose programs from. For binding mechanics and numeric literals see [Values & Bindings](/tutorials/values.html); for the precise rules see the [Types reference](/reference/types.html).

## Basic types

| Type | Example | Description |
| --- | --- | --- |
| `String` | `"hello"` | UTF-8 text |
| `Boolean` | `true` / `false` | truth value |
| `Null` | `null` | absence of value |
| `Int32` | `42` | 32-bit signed integer (the default for integer literals) |
| `Float64` | `3.14` | 64-bit floating point (the default for float literals) |

Lin also has the other integer widths (`Int8`, `Int16`, `Int64`, and the unsigned `UInt8`–`UInt64` families) and `Float32`. Pin a literal to one with a suffix — see [Values & Bindings](/tutorials/values.html).

## `Number`

When you want code to work across numeric widths without committing to one, use `Number`. It is a numeric type covering every integer and float family at once, with no runtime cost — each value keeps its specific type. See the [Types reference](/reference/types.html) for the details.

```lin
val half = (n: Number): Number => n / 2
```

## Arrays

The type of an array whose elements all have type `T` is written `T[]`:

```lin
val xs: Int32[] = [1, 2, 3]
val names: String[] = ["Bob", "Alice"]
```

`T[]` is unbounded — it describes an array of *any* length whose every element has type `T`. Element access has type `T` (no implicit `Null`):

```lin
val first: Int32 = [1, 2, 3][0]   // Int32
```

Indexing out of bounds is a runtime error — it does not return `null`. (Reading a missing *object* key does return `Null`; arrays are stricter.)

Array types nest, so a grid of integers is `Int32[][]`:

```lin
val grid: Int32[][] = [[1, 2], [3, 4]]
val cell: Int32 = grid[1][0]   // 3
```

### Fixed-length array types

A **fixed-length** array type, written `[T1, T2, ..., Tn]`, describes an array of exactly `n` elements where each position has its own type:

```lin
val pair: [String, Int32] = ["age", 42]
val point: [Float64, Float64] = [1.5, 2.0]

val key: String = pair[0]   // "age"
val n: Int32 = pair[1]      // 42
```

These are not a separate runtime kind — they remain ordinary JSON arrays, and the form simply constrains the length and the per-position element types at the type level. Supplying the wrong number of elements is a compile-time error.

## Union types

A value that might be one of several types is a **union**, written with `|`:

```lin
val maybeAge: Int32 | Null = null
val id: String | Int32 = "user-42"
```

The most common union is `T | Null` — a value that might be absent. You inspect a union by narrowing it with a pattern; see [Pattern Matching](/tutorials/pattern-matching.html).

A union is often tagged with a string-literal field so each variant is distinguishable:

```lin
type Result =
  | { "type": "ok", "value": Int32 }
  | { "type": "err", "message": String }
```

## Intersection types

An **intersection** `A & B` combines two record types into one that has *all* the fields of both. It is the record counterpart of union: where `|` means "either shape", `&` means "both shapes at once".

```lin
type Date = { "year": Int32, "month": Int32, "day": Int32 }
type Time = { "hour": Int32, "minute": Int32 }
type DateTime = Date & Time

val stamp: DateTime = {
  "year": 2026, "month": 6, "day": 17,
  "hour": 9, "minute": 30
}
```

`DateTime` has every field of `Date` plus every field of `Time`. Because records are structural, a `DateTime` is also usable anywhere a `Date` or a `Time` is expected — it has at least those fields, so it can be passed straight to a function that reads only some of them.

Intersection is record-only: both operands must be record types. A field that appears in both with the *same* type is merged; a field that appears in both with conflicting types is a compile-time error.

## `AnyVal`

`AnyVal` represents any JSON-compatible value — string, number, boolean, null, array, or object:

```lin
val data: AnyVal = { "name": "Alice", "scores": [10, 20, 30] }
```

Use `AnyVal` when the shape is not known statically — for example, data read from a file or an HTTP response. Any value assigns *into* `AnyVal` freely.

Going the other way is restricted: `AnyVal` does **not** implicitly assign *out* to a concrete record type with required fields. To turn untrusted `AnyVal` into a typed value, either validate it with `fromJson` (from `std/json`), or narrow it with an `is`/`has` pattern:

```lin
import { fromJson } from "std/json"

type Person = { "name": String, "age": Int32 }

val decoded = Person.fromJson(data)   // Person | Error
```

Prefer named records, [maps](/tutorials/maps.html), or [generics](/tutorials/generics.html) over `AnyVal` wherever the shape is known — they are both safer and faster.

## Annotations are optional

You rarely need to write these types out: Lin infers them in most positions. Annotations are for documentation, earlier errors, and the occasional inference hint — see [Values & Bindings](/tutorials/values.html). For the complete rules, structural typing, and variance, see the [Types reference](/reference/types.html).
