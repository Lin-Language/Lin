# Values & Types

Lin's type system is designed around JSON-compatible data. Every runtime value is either a JSON value (string, number, boolean, null, array, or object) or an opaque runtime value (function, iterator, promise, worker).

## Immutable bindings: `val`

```lin
val name = "Alice"
val age = 30
val active = true
val missing = null
```

`val` bindings cannot be reassigned. The type is inferred from the right-hand side.

You can add a type annotation:

```lin
val name: String = "Alice"
val count: Int32 = 0
val ratio: Float64 = 3.14
val flag: Boolean = false
```

## Mutable bindings: `var`

```lin
var counter = 0
counter = counter + 1
counter = counter + 1
```

`var` bindings can be reassigned with the `=` operator.

## Basic types

| Type | Example | Description |
| --- | --- | --- |
| `String` | `"hello"` | UTF-8 text |
| `Boolean` | `true` / `false` | truth value |
| `Null` | `null` | absence of value |
| `Int32` | `42` | 32-bit signed integer |
| `Float64` | `3.14` | 64-bit floating point |

Lin also has `Int8`, `Int16`, `Int64`, `UInt8`–`UInt64`, `Float32`, and `Float64`. The defaults are `Int32` for integer literals and `Float64` for floating-point literals.

### Numeric literals

A type suffix pins a literal's type:

```lin
val a = 42i8      // Int8
val b = 42u32     // UInt32
val c = 3.14f32   // Float32
val d = 3.14f64   // Float64
```

A bare integer literal defaults to `Int32` if it fits; if it is too large it widens to the smallest type that preserves the value (it is never truncated):

```lin
val big = 1705314600000   // Int64 (too large for Int32)
```

Context can resize a bare literal, but a suffixed literal that conflicts with its context is a compile error:

```lin
val x: Int64 = 42    // ok — 42 typed as Int64
val y: Int32 = 5i64  // error — the i64 suffix pins it to Int64
```

## Union types

A value that might be one of several types is a **union**:

```lin
val maybeAge: Int32 | Null = null
val id: String | Int32 = "user-42"
```

Union types are written with `|`. The most common union is `T | Null` — a value that might be absent.

## The `Json` type

`Json` represents any JSON-compatible value:

```lin
val data: Json = { "name": "Alice", "scores": [10, 20, 30] }
```

Use `Json` when you need a dynamically shaped value — for example, data read from a file or HTTP response.

Any value assigns *into* `Json`, but `Json` does not implicitly assign *out* to a concrete object type with required fields. To go from untrusted `Json` to a concrete type, validate it with `fromJson` (from `std/json`) or narrow it with an `is`/`has` pattern:

```lin
import { fromJson } from "std/json"

type Person = { "name": String, "age": Int32 }

val person = Person.fromJson(someJson)   // Person | Error
```

## Type annotations are optional

Lin infers types in most places. Annotations are useful for documentation, for catching errors earlier, and in places where inference needs a hint:

```lin
val greet = (name: String): String =>
  "Hello, ${name}!"
```

## Numeric widening

When you mix numeric types in arithmetic, Lin automatically widens to the appropriate type:

```lin
val x: Int32 = 10
val y: Float64 = 3.14
val z = x + y   // Float64: 13.14
```

Explicit narrowing requires a call to a conversion function:

```lin
import { toInt32 } from "std/number"

val f: Float64 = 9.7
val i = toInt32(f)   // 9 (truncates toward zero)
```

## Summary

- `val` for immutable, `var` for mutable.
- Type annotations are optional but encouraged.
- Primitive types: `String`, `Boolean`, `Null`, `Int32`, `Float64`.
- `T | U` for unions; `Json` for any JSON value.
- Numbers widen automatically; explicit narrowing uses stdlib functions.
