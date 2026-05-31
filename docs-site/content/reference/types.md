# Types Reference

## Primitive types

| Type | Description | Example literal |
| --- | --- | --- |
| `String` | UTF-8 text | `"hello"` |
| `Boolean` | Truth value | `true` / `false` |
| `Null` | Absence of value | `null` |
| `Int8` | 8-bit signed integer | `42i8` |
| `Int16` | 16-bit signed integer | `1000i16` |
| `Int32` | 32-bit signed integer (default) | `42` |
| `Int64` | 64-bit signed integer | `42i64` |
| `UInt8`–`UInt64` | Unsigned integer families | `255u8` |
| `Float32` | 32-bit IEEE 754 float | `3.14f32` |
| `Float64` | 64-bit IEEE 754 float (default) | `3.14` |

### Numeric literal typing

- A **type suffix pins the type**: `42i8` is `Int8`, `42u32` is `UInt32`, `3.14f32` is `Float32`, `3.14f64` is `Float64`.
- A **bare suffixless integer** defaults to `Int32` if it fits. If it exceeds the `Int32` range it widens to the smallest type that preserves the value (e.g. `1705314600000` becomes `Int64`) — it is never silently truncated.
- **Context can resize a bare literal**: `val x: Int64 = 42` types `42` as `Int64`.
- A **suffixed literal in a conflicting context is a compile error**: `val x: Int32 = 5i64` fails, because the suffix pins `5i64` to `Int64`.
- A bare floating-point literal defaults to `Float64`.

## `Number`

`Number` is a built-in union alias covering every numeric type family. It has no runtime representation of its own — every value retains its specific numeric type:

```lin
type Number =
  | Int8 | Int16 | Int32 | Int64
  | UInt8 | UInt16 | UInt32 | UInt64
  | Float32 | Float64
```

## `Json`

`Json` is the recursive union of all JSON-compatible values:

```lin
type Json =
  | String | Boolean | Null | Number
  | Json[]
  | { ...Json }   // any object whose values are Json
```

Use `Json` when the shape of data is not statically known.

### `Json` is a covariant sink

Any value assigns **into** `Json` — `val j: Json = anyValue` is always allowed. But `Json` does **not** implicitly assign **out** to a concrete object type with required fields. To go from an untrusted `Json` value to a concrete type you must either:

- validate via `fromJson` (from `std/json`), which decodes and type-checks recursively, returning `T | Error`; or
- narrow with an `is`/`has` pattern in a `match`.

```lin
import { fromJson } from "std/json"

type Person = { "name": String, "age": Int32 }

val decoded = Person.fromJson(someJson)   // Person | Error
```

## The `Error` type

`Error` is a built-in type, structurally equivalent to:

```lin
{ "type": String, "message": String }
```

Fallible stdlib operations return `T | Error`, and faults inside `async` thunks surface as `Error` at `await`. Use `is Error` to detect it.

## Union types

```lin
val x: String | Null = null
val id: String | Int32 = "user-42"

type Result<T, E> =
  | { "type": "success", "value": T }
  | { "type": "failure", "error": E }
```

Union types use `|`. The type `T | Null` is the common pattern for optional values.

## Object types

```lin
type Person = {
  "name": String,
  "age": Int32
}
```

Object types are structural. A value with additional fields is compatible with a smaller structural type.

## Array types

`T[]` — unbounded array of `T`:

```lin
val names: String[] = ["Alice", "Bob"]
```

`[T1, T2, T3]` — fixed-length array with specified element types:

```lin
val pair: [String, Int32] = ["age", 42]
```

## Function types

```lin
type Predicate<T> = (T) => Boolean
type Mapper<T, U> = (T) => U
```

## Generic types

Generic type declarations and applications are supported:

```lin
type Box<T> = {
  "value": T,
  "label": String
}

type Result<T, E> =
  | { "type": "success", "value": T }
  | { "type": "failure", "error": E }

type Mapper<T, U> = (T) => U
```

Generic types are covariant in producer positions and contravariant in consumer positions.

Limitations:

- You **cannot** use a generic application in an `is` pattern (`is Result<Int32, String>` is not supported). Match the underlying tagged shape instead via `has { "type": "success", value }`.
- Cross-module generic functions are monomorphized per importer.

## Opaque runtime types

| Type | Description |
| --- | --- |
| `Iterator<T>` | Stateful traversal producing `T` |
| `Iterable<T>` | Any value that can produce `Iterator<T>` |
| `Promise<T>` | Value being computed on another thread |
| `Worker<Msg, Reply>` | Long-lived background thread |
| `ThreadPool` | Fixed-size thread pool |
| `Function` | Opaque function reference |

These types are not JSON values and cannot be stored in JSON objects or arrays.

## Structural typing

Types are structural by default. Two types are compatible if they describe the same shape:

```lin
type Named = { "name": String }

val greet = (x: Named): String => "Hello ${x["name"]}"

// Works — the value has at least the "name" field
greet({ "name": "Alice", "age": 30 })
```

## Numeric widening

Numeric types widen automatically in arithmetic and comparison. The widened type is the smallest type that can fully represent both operands. Explicit narrowing uses stdlib functions (`toInt32`, `toFloat64`, etc.) and may fail at runtime if the value cannot be represented.
