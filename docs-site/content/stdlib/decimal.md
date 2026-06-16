# std/decimal

std/decimal — exact base-10 fixed-point arithmetic for money and other exact-decimal work.

A `Decimal` is an opaque, immutable, refcounted heap value carrying 28–29 significant digits.
It represents a value as an integer coefficient scaled by a power of ten, so there is no binary
rounding error: `decimal("0.1")` plus `decimal("0.2")` is exactly `decimal("0.3")`.

`add`, `sub`, and `mul` are always exact and never round. `div`, `round`, and `setScale`
require an explicit rounding mode and scale; there is no silent default, which is a common
source of money bugs. The recommended flow is to build values from strings, add and multiply
exactly, then `round(x, 2, …)` once at the presentation boundary.

Fallible functions return `Decimal | Error`, matched with `is Error`.

A Decimal cannot be transferred across workers, since the handle is a pointer.

## Reference

#### `Decimal`

```lin
type Decimal = AnyVal
```


#### `RoundingMode`

```lin
type RoundingMode = Int32
```

A rounding strategy, one of the `Round*` constants below. Passed to `div`/`round`/`setScale`.

### Rounding modes (opaque enumerated constants)

#### `RoundHalfUp`

```lin
val RoundHalfUp: RoundingMode
```

0.5 → away from zero (schoolbook); 0.5 → nearest even (banker's, the money default);
toward -inf; toward +inf; toward zero (truncate).
Round 0.5 away from zero (schoolbook rounding).

#### `RoundHalfEven`

```lin
val RoundHalfEven: RoundingMode
```

Round 0.5 to the nearest even digit (banker's rounding — the money default).

#### `RoundFloor`

```lin
val RoundFloor: RoundingMode
```

Round toward negative infinity.

#### `RoundCeil`

```lin
val RoundCeil: RoundingMode
```

Round toward positive infinity.

#### `RoundDown`

```lin
val RoundDown: RoundingMode
```

Round toward zero (truncate).

### Construction

#### `decimal`

```lin
val decimal = (s: String): Decimal | Error
```

Parse a base-10 numeral into an exact `Decimal`, preserving the written scale (`"1.50"` → scale 2).
This is the preferred constructor: it is exact, with no float intermediary.
- **`s`** — the decimal numeral.
- **Returns** the `Decimal`, or an `Error` if `s` is not a valid number.

**Example:**

```lin
decimal("0.1").add(decimal("0.2")).toString()  // "0.3" (exact, no binary error)
```

#### `fromInt`

```lin
val fromInt = (n: Int64): Decimal
```

Build a `Decimal` from an integer (exact, scale 0).
- **`n`** — the Int64 value.
- **Returns** the `Decimal`.

#### `fromFloat`

```lin
val fromFloat = (f: Float64): Decimal
```

Build a `Decimal` from a Float64. This is lossy, because a Float64 already carries binary
rounding error; do not use it for money — use `decimal` on a string instead.
- **`f`** — the float value.
- **Returns** the `Decimal` nearest to `f`.

#### `zero`

```lin
val zero: Decimal
```

The `Decimal` constant 0.

#### `one`

```lin
val one: Decimal
```

The `Decimal` constant 1.

### Arithmetic

#### `add`

```lin
val add = (a: Decimal, b: Decimal): Decimal
```

Exact sum.
- **Returns** `a + b`.

#### `sub`

```lin
val sub = (a: Decimal, b: Decimal): Decimal
```

Exact difference.
- **Returns** `a - b`.

#### `mul`

```lin
val mul = (a: Decimal, b: Decimal): Decimal
```

Exact product.
- **Returns** `a * b`.

#### `div`

```lin
val div = (a: Decimal, b: Decimal, scale: Int32, mode: RoundingMode): Decimal | Error
```

Divide `a` by `b`, rounding to `scale` decimal places with `mode`. This is the only rounding
arithmetic: division has no exact result in general, so a target scale and mode are required.
- **`scale`** — number of decimal places in the result.
- **`mode`** — one of the `Round*` constants.
- **Returns** `a / b` rounded, or an `Error` if `b` is zero.

#### `pow`

```lin
val pow = (a: Decimal, e: Int32): Decimal
```

Raise `a` to a non-negative integer power (exact).
- **Returns** `a ** e`.

#### `neg`

```lin
val neg = (a: Decimal): Decimal
```

Negation.
- **Returns** `-a`.

#### `abs`

```lin
val abs = (a: Decimal): Decimal
```

Absolute value.
- **Returns** `|a|`.

### Comparison (by numeric value, ignoring scale)

#### `cmp`

```lin
val cmp = (a: Decimal, b: Decimal): Int32
```

Three-way compare by numeric value (so `1.5` and `1.50` are equal).
- **Returns** -1 if `a < b`, 0 if equal, 1 if `a > b`.

#### `eq`

```lin
val eq = (a: Decimal, b: Decimal): Boolean
```

- **Returns** true if `a == b` by value (scale-insensitive).

#### `lt`

```lin
val lt = (a: Decimal, b: Decimal): Boolean
```

- **Returns** true if `a < b`.

#### `lte`

```lin
val lte = (a: Decimal, b: Decimal): Boolean
```

- **Returns** true if `a <= b`.

#### `gt`

```lin
val gt = (a: Decimal, b: Decimal): Boolean
```

- **Returns** true if `a > b`.

#### `gte`

```lin
val gte = (a: Decimal, b: Decimal): Boolean
```

- **Returns** true if `a >= b`.

### Rounding and scale

#### `round`

```lin
val round = (d: Decimal, scale: Int32, mode: RoundingMode): Decimal
```

Round `d` to `scale` decimal places using `mode`.
- **`scale`** — target number of decimal places.
- **`mode`** — one of the `Round*` constants.
- **Returns** the rounded `Decimal`.

#### `setScale`

```lin
val setScale = (d: Decimal, scale: Int32, mode: RoundingMode): Decimal
```

Set `d`'s scale to exactly `scale` places (rounding with `mode` if reducing, padding if growing).
- **Returns** the rescaled `Decimal`.

#### `scale`

```lin
val scale = (d: Decimal): Int32
```

- **Returns** the number of decimal places currently stored in `d`.

### Conversion

#### `toString`

```lin
val toString = (d: Decimal): String
```

Render as a string, preserving scale (`"1.50"` stays `"1.50"`).
- **Returns** the decimal string.

#### `toInt64`

```lin
val toInt64 = (d: Decimal): Int64 | Error
```

Narrow to an integer, dropping any fractional part.
- **Returns** the `Int64` value, or an `Error` if the integer part does not fit in 64 bits.

#### `toFloat64`

```lin
val toFloat64 = (d: Decimal): Float64
```

Convert to a double (may lose precision).
- **Returns** the `Float64` value.
