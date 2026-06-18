# Values & Bindings

A value is a piece of data — a string, a number, a boolean, `null`, or a structured value built from them. A binding gives a value a name. This page covers how to name values and how numeric literals work; for the type system itself see [Types](/tutorials/types.html).

## Immutable bindings: `val`

```lin
val name = "Alice"
val age = 30
val active = true
val missing = null
```

`val` bindings cannot be reassigned. The type is inferred from the right-hand side.

## Mutable bindings: `var`

```lin
var counter = 0
counter = counter + 1
counter = counter + 1
```

`var` bindings can be reassigned with the `=` operator. Reach for `var` only when you genuinely need mutable state (a counter, an accumulator); default to `val`.

## Type annotations are optional

Lin infers types in most places, so an annotation is rarely required. Annotations are useful for documentation, for catching errors earlier, and in the few places where inference needs a hint:

```lin
val name: String = "Alice"
val count: Int32 = 0
val ratio: Float64 = 3.14
val flag: Boolean = false
```

An empty literal has no contents to infer from, so it needs an annotation:

```lin
val xs: Int32[] = []
```

See [Types](/tutorials/types.html) for the full set of type forms.

## Numeric literals

A bare integer literal defaults to `Int32`; a bare floating-point literal defaults to `Float64`. A **type suffix** pins a literal's type:

```lin
val a = 42i8      // Int8
val b = 42u32     // UInt32
val c = 3.14f32   // Float32
val d = 3.14f64   // Float64
```

If a bare integer literal is too large for `Int32` it widens to the smallest type that preserves the value (it is never truncated):

```lin
val big = 1705314600000   // Int64 (too large for Int32)
```

Context can resize a bare literal, but a suffixed literal that conflicts with its context is a compile error:

```lin
val x: Int64 = 42    // ok — 42 typed as Int64
// val y: Int32 = 5i64   // error — the i64 suffix pins it to Int64
```

## Numeric widening

When you mix numeric types in arithmetic, Lin automatically widens to the appropriate type — the smallest type that can represent both operands:

```lin
val x: Int32 = 10
val y: Float64 = 3.14
val z = x + y   // Float64: 13.14
```

## Explicit narrowing

There is no implicit narrowing. Going to a narrower type requires a conversion function from `std/number`:

```lin
import { toInt32 } from "std/number"

val f: Float64 = 9.7
val i = toInt32(f)   // 9 (truncates toward zero)
```

## Summary

- `val` for immutable bindings, `var` for mutable ones.
- Type annotations are optional but encouraged; an empty `[]`/`{}` needs one.
- Integer literals default to `Int32`, floats to `Float64`; a suffix (`42i8`, `3.14f32`) pins the type.
- Numbers widen automatically in arithmetic; narrowing is always an explicit call.
- For the type system — arrays, unions, intersections, records, `AnyVal` — see [Types](/tutorials/types.html).
