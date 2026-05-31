# std/number

Numeric parsing and conversion functions.

```lin
import { parseInt32, parseFloat64, toInt32, toFloat64, isInt32, isFloat64 } from "std/number"
```

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `isFloat64` | `(String) -> Boolean` | True if string parses as Float64 |
| `isInt32` | `(String) -> Boolean` | True if string parses as Int32 |
| `parseFloat64` | `(String) -> Float64` | Parse decimal string to Float64 |
| `parseInt32` | `(String) -> Int32` | Parse decimal string to Int32 |
| `toFloat32` | `(Float64) -> Float32` | Narrow Float64 to Float32 |
| `toFloat64` | `(Int32) -> Float64` | Widen Int32 to Float64 |
| `toInt8` | `(UInt64) -> Int8` | Truncate to an 8-bit signed byte |
| `toInt16` | `(UInt64) -> Int16` | Truncate to a 16-bit signed int |
| `toInt32` | `(Float64) -> Int32` | Truncate Float64 to Int32 |
| `toInt64` | `(UInt64) -> Int64` | Reinterpret to a 64-bit signed int |
| `toUInt8` | `(UInt64) -> UInt8` | Truncate to an 8-bit unsigned byte |
| `toUInt16` | `(UInt64) -> UInt16` | Truncate to a 16-bit unsigned int |
| `toUInt32` | `(UInt64) -> UInt32` | Truncate to a 32-bit unsigned int |
| `toUInt64` | `(UInt64) -> UInt64` | Identity / reinterpret to 64-bit unsigned int |
| `tryParseFloat64` | `(String) -> Float64 \| Null` | Parse Float64 safely |
| `tryParseInt32` | `(String) -> Int32 \| Null` | Parse Int32 safely |

---

### `parseInt32`

```lin
val parseInt32: (s: String) -> Int32
```

Parses `s` as a base-10 integer. Runtime error if unparseable or overflows `Int32`. Use `tryParseInt32` for safe parsing.

```lin
parseInt32("42")    // 42
parseInt32("-7")    // -7
```

---

### `tryParseInt32`

```lin
val tryParseInt32: (s: String) -> Int32 | Null
```

Returns `Null` if `s` is not a valid `Int32`, instead of a runtime error.

```lin
tryParseInt32("42")    // 42
tryParseInt32("bad")   // null
tryParseInt32("3.14")  // null
```

---

### `parseFloat64`

```lin
val parseFloat64: (s: String) -> Float64
```

```lin
parseFloat64("3.14")   // 3.14
parseFloat64("1e10")   // 10000000000.0
```

---

### `tryParseFloat64`

```lin
val tryParseFloat64: (s: String) -> Float64 | Null
```

```lin
tryParseFloat64("3.14")   // 3.14
tryParseFloat64("bad")    // null
```

---

### `toInt32`

```lin
val toInt32: (v: Float64) -> Int32
```

Truncates toward zero. Runtime error if value cannot be represented as `Int32`.

```lin
toInt32(3.9)    // 3
toInt32(-2.1)   // -2
```

---

### `toFloat64`

```lin
val toFloat64: (v: Int32) -> Float64
```

Widens `Int32` to `Float64`. Always exact.

```lin
toFloat64(42)   // 42.0
```

---

### `isInt32` / `isFloat64`

Use these to guard untrusted input before calling the non-safe parse functions:

```lin
val safe = (s: String): Int32 | Null =>
  if isInt32(s) then parseInt32(s)
  else null
```

Or prefer `tryParseInt32` / `tryParseFloat64` directly.

---

### Narrowing casts

The fixed-width casts (`toUInt8`, `toInt8`, `toUInt16`, `toInt16`, `toUInt32`, `toInt64`, `toUInt64`) all take a `UInt64` and truncate/reinterpret it to the target width. They are the building blocks for the bit-level packing in `std/bytes`.

```lin
toUInt8(0x1FF)    // 255   (low 8 bits)
toUInt16(0x1FFFF) // 65535 (low 16 bits)
toFloat32(3.14159265358979)   // 3.1415927  (Float32 precision)
```
