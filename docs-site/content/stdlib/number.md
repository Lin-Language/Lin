# std/number

std/number — numeric parsing, conversion, and fixed-width casts.

Two parsing styles: the bare `parseInt32`/`parseFloat64` raise a runtime error on bad input, while
`tryParseInt32`/`tryParseFloat64` narrow to `null` instead so callers can test with a plain
`== null` / `is Int32`. Guard untrusted input with `isInt32`/`isFloat64`, or just use the
try-variants. `toInt32`/`toFloat64`/`toFloat32` convert between the numeric types (truncating or
widening). The fixed-width casts (toUInt8/toInt8/…/toUInt64) take a UInt64 and truncate/reinterpret
to the target width — the building blocks for the bit-level packing in std/bytes.

```lin
import { parseInt32, parseFloat64, toInt32, toFloat64, isInt32, isFloat64 } from "std/number"
```

## Reference

#### `parseInt32`

```lin
val parseInt32 = (s: String): Int32
```

Parse `s` as a base-10 Int32.
- **`s`** — the string to parse.
- **Returns** the parsed integer. Runtime error if unparseable or out of Int32 range — use
  `tryParseInt32` for safe parsing.

**Example:**

```lin
parseInt32("42")   // 42
```

#### `parseFloat64`

```lin
val parseFloat64 = (s: String): Float64
```

Parse `s` as a Float64.
- **`s`** — the string to parse.
- **Returns** the parsed floating-point value.

**Example:**

```lin
parseFloat64("1e10")   // 10000000000.0
```

#### `toInt32`

```lin
val toInt32 = (v: Float64): Int32
```

Convert a Float64 to an Int32 by truncation toward zero.
- **`v`** — the float to convert.
- **Returns** the truncated Int32. Runtime error if `v` cannot be represented as an Int32.

**Example:**

```lin
toInt32(3.9)   // 3
```

#### `toFloat64`

```lin
val toFloat64 = (v: Int32): Float64
```

Widen an Int32 to a Float64.
- **`v`** — the integer to convert.
- **Returns** the equivalent Float64 (always exact).

**Example:**

```lin
toFloat64(42)   // 42.0
```

#### `toFloat32`

```lin
val toFloat32 = (v: Float64): Float32
```

Narrow a Float64 to a Float32.
- **`v`** — the double to convert.
- **Returns** the value rounded to Float32 precision.

#### `toUInt8`

```lin
val toUInt8 = (v: UInt64): UInt8
```

Explicit narrowing integer casts. Each truncates its input to the target width using
two's-complement semantics. The input is taken as a UInt64 (the widest unsigned type) so any
narrower unsigned integer — or a value masked down to a byte or word — widens into it at the
call site without losing range; truncation to the target width is then well defined.
Truncate `v` to a UInt8.
- **`v`** — the value to narrow (taken as UInt64).
- **Returns** the low 8 bits of `v` as a UInt8.

**Example:**

```lin
toUInt8(0x1FF)   // 255  (low 8 bits)
```

#### `toInt8`

```lin
val toInt8 = (v: UInt64): Int8
```

Truncate `v` to an Int8.
- **`v`** — the value to narrow (taken as UInt64).
- **Returns** the low 8 bits of `v` as a signed Int8.

#### `toUInt16`

```lin
val toUInt16 = (v: UInt64): UInt16
```

Truncate `v` to a UInt16.
- **`v`** — the value to narrow (taken as UInt64).
- **Returns** the low 16 bits of `v` as a UInt16.

#### `toInt16`

```lin
val toInt16 = (v: UInt64): Int16
```

Truncate `v` to an Int16.
- **`v`** — the value to narrow (taken as UInt64).
- **Returns** the low 16 bits of `v` as a signed Int16.

#### `toUInt32`

```lin
val toUInt32 = (v: UInt64): UInt32
```

Truncate `v` to a UInt32.
- **`v`** — the value to narrow (taken as UInt64).
- **Returns** the low 32 bits of `v` as a UInt32.

#### `toInt64`

```lin
val toInt64 = (v: UInt64): Int64
```

Reinterpret `v` as a signed Int64.
- **`v`** — the value to convert (taken as UInt64).
- **Returns** the same bit pattern as a signed Int64.

#### `toUInt64`

```lin
val toUInt64 = (v: UInt64): UInt64
```

Identity cast keeping `v` as a UInt64.
- **`v`** — the value.
- **Returns** `v` as a UInt64.

#### `isInt32`

```lin
val isInt32 = (s: String): Boolean
```

Test whether `s` is a valid base-10 Int32.
- **`s`** — the string to test.
- **Returns** `true` if `s` parses as an Int32, otherwise `false`.

#### `isFloat64`

```lin
val isFloat64 = (s: String): Boolean
```

Test whether `s` is a valid Float64.
- **`s`** — the string to test.
- **Returns** `true` if `s` parses as a Float64, otherwise `false`.

#### `tryParseInt32`

```lin
val tryParseInt32 = (s: String): Int32 | Null
```

Parse `s` to an Int32, narrowing to `null` on failure so callers can test with a plain
`== null` / `is Int32` instead of an untyped `AnyVal` read.
- **`s`** — the string to parse.
- **Returns** the parsed Int32, or `null` if `s` is not a valid integer.

**Example:**

```lin
tryParseInt32("bad")   // null
```

#### `tryParseFloat64`

```lin
val tryParseFloat64 = (s: String): Float64 | Null
```

Parse `s` to a Float64, narrowing to `null` on failure.
- **`s`** — the string to parse.
- **Returns** the parsed Float64, or `null` if `s` is not a valid float.
