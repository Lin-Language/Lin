# std/math

std/math — mathematical functions and constants.

The constants PI, E, INFINITY, and NAN are Float64 values. Most functions take and return Float64
(the trig/log/pow family); abs / min / max / clamp / sign are generic over Number, so they work
for any numeric type. `random` returns a uniform Float64 in [0, 1). Note NaN: `x == NAN` is always
false under IEEE 754, so test with `isNaN(x)` instead.

```lin
import { abs, floor, ceil, round, sqrt, pow, PI, E } from "std/math"
```

## Reference

#### `PI`

```lin
val PI
```

The ratio of a circle's circumference to its diameter, pi.

#### `E`

```lin
val E
```

Euler's number, the base of the natural logarithm.

#### `INFINITY`

```lin
val INFINITY
```

Positive floating-point infinity.

#### `NAN`

```lin
val NAN
```

The floating-point "not a number" value.

#### `floor`

```lin
val floor = (x: Float64): Float64
```

Round `x` down to the nearest integer (toward negative infinity).
- **`x`** — the value to round.
- **Returns** the largest integer not greater than `x`.

#### `ceil`

```lin
val ceil = (x: Float64): Float64
```

Round `x` up to the nearest integer (toward positive infinity).
- **`x`** — the value to round.
- **Returns** the smallest integer not less than `x`.

#### `round`

```lin
val round = (x: Float64): Float64
```

Round `x` to the nearest integer (ties round to the larger value).
- **`x`** — the value to round.
- **Returns** the nearest integer to `x`.

#### `trunc`

```lin
val trunc = (x: Float64): Float64
```

Truncate `x` toward zero, discarding the fractional part.
- **`x`** — the value to truncate.
- **Returns** the integer part of `x`.

#### `sqrt`

```lin
val sqrt = (x: Float64): Float64
```

Compute the non-negative square root of `x`.
- **`x`** — the radicand.
- **Returns** the square root of `x`, or `NaN` if `x` is negative.

**Example:**

```lin
sqrt(9.0)   // 3.0
```

#### `pow`

```lin
val pow = (base: Float64, exp: Float64): Float64
```

Raise `base` to the power `exp`.
- **`base`** — the base.
- **`exp`** — the exponent.
- **Returns** `base` raised to `exp`.

**Example:**

```lin
pow(2.0, 10.0)   // 1024.0
```

#### `exp`

```lin
val exp = (x: Float64): Float64
```

Compute e raised to the power `x`.
- **`x`** — the exponent.
- **Returns** e^`x`.

#### `log`

```lin
val log = (x: Float64): Float64
```

Compute the natural (base-e) logarithm of `x`.
- **`x`** — the value.
- **Returns** the natural log of `x`, or `NaN` if `x` is negative.

#### `log2`

```lin
val log2 = (x: Float64): Float64
```

Compute the base-2 logarithm of `x`.
- **`x`** — the value.
- **Returns** the base-2 log of `x`, or `NaN` if `x` is negative.

#### `log10`

```lin
val log10 = (x: Float64): Float64
```

Compute the base-10 logarithm of `x`.
- **`x`** — the value.
- **Returns** the base-10 log of `x`, or `NaN` if `x` is negative.

#### `sin`

```lin
val sin = (x: Float64): Float64
```

Compute the sine of `x` (in radians).
- **`x`** — the angle in radians.
- **Returns** the sine of `x`.

#### `cos`

```lin
val cos = (x: Float64): Float64
```

Compute the cosine of `x` (in radians).
- **`x`** — the angle in radians.
- **Returns** the cosine of `x`.

#### `tan`

```lin
val tan = (x: Float64): Float64
```

Compute the tangent of `x` (in radians).
- **`x`** — the angle in radians.
- **Returns** the tangent of `x`.

#### `asin`

```lin
val asin = (x: Float64): Float64
```

Compute the arcsine of `x`.
- **`x`** — the value, in [-1, 1].
- **Returns** the angle in radians, or `NaN` if `x` is out of range.

#### `acos`

```lin
val acos = (x: Float64): Float64
```

Compute the arccosine of `x`.
- **`x`** — the value, in [-1, 1].
- **Returns** the angle in radians, or `NaN` if `x` is out of range.

#### `atan`

```lin
val atan = (x: Float64): Float64
```

Compute the arctangent of `x`.
- **`x`** — the value.
- **Returns** the angle in radians, in (-pi/2, pi/2).

#### `atan2`

```lin
val atan2 = (y: Float64, x: Float64): Float64
```

Compute the angle of the point (`x`, `y`) from the positive x-axis.
- **`y`** — the y-coordinate.
- **`x`** — the x-coordinate.
- **Returns** the angle in radians, in (-pi, pi].

#### `abs`

```lin
val abs = (x: Float64): Float64
```

Compute the absolute value of `x`.
- **`x`** — the value.
- **Returns** the magnitude of `x`.

#### `min`

```lin
val min = (a: Float64, b: Float64): Float64
```

Return the smaller of `a` and `b`.
- **`a`** — the first value.
- **`b`** — the second value.
- **Returns** the lesser of `a` and `b`.

#### `max`

```lin
val max = (a: Float64, b: Float64): Float64
```

Return the larger of `a` and `b`.
- **`a`** — the first value.
- **`b`** — the second value.
- **Returns** the greater of `a` and `b`.

#### `clamp`

```lin
val clamp = (x: Float64, lo: Float64, hi: Float64): Float64
```

Constrain `x` to the range [`lo`, `hi`].
- **`x`** — the value to clamp.
- **`lo`** — the lower bound.
- **`hi`** — the upper bound.
- **Returns** `lo` if `x < lo`, `hi` if `x > hi`, otherwise `x`.

**Example:**

```lin
clamp(15, 1, 10)   // 10
```

#### `sign`

```lin
val sign = (x: Float64): Int64
```

Determine the sign of `x`.
- **`x`** — the value.
- **Returns** -1 if `x` is negative, 1 if positive, 0 if zero.

#### `isNaN`

```lin
val isNaN = (x: Float64): Boolean
```

Test whether `x` is the floating-point "not a number" value.
- **`x`** — the value to test.
- **Returns** `true` if `x` is `NaN`, otherwise `false`. Use this rather than `x == NAN`, which is
  always false under IEEE 754.

**Example:**

```lin
isNaN(NAN)   // true
```

#### `isFinite`

```lin
val isFinite = (x: Float64): Boolean
```

Test whether `x` is a finite number (neither infinite nor `NaN`).
- **`x`** — the value to test.
- **Returns** `true` if `x` is finite, otherwise `false`.

#### `random`

```lin
val random = (): Float64
```

Generate a pseudo-random number in [0, 1).
- **Returns** a Float64 in the half-open interval [0, 1).

#### `toFixed`

```lin
val toFixed = (x: Float64, decimals: Int64): String
```

Format `x` with a fixed number of decimal places.
- **`x`** — the value to format.
- **`decimals`** — the number of digits after the decimal point.
- **Returns** the formatted string.

**Example:**

```lin
toFixed(3.14159, 2)   // "3.14"
```
