# std/math

Mathematical functions and constants.

```lin
import { abs, floor, ceil, round, sqrt, pow, PI, E } from "std/math"
```

## Constants

| Constant | Type | Value |
| --- | --- | --- |
| `PI` | `Float64` | `3.141592653589793` |
| `E` | `Float64` | `2.718281828459045` |
| `INFINITY` | `Float64` | Positive infinity |
| `NAN` | `Float64` | Not-a-number |

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `abs` | `(Number) -> Number` | Absolute value |
| `acos` | `(Float64) -> Float64` | Arc cosine (radians) |
| `asin` | `(Float64) -> Float64` | Arc sine (radians) |
| `atan` | `(Float64) -> Float64` | Arc tangent (radians) |
| `atan2` | `(Float64, Float64) -> Float64` | Arc tangent of y/x |
| `ceil` | `(Float64) -> Float64` | Round toward +infinity |
| `clamp` | `(Number, Number, Number) -> Number` | Clamp to `[lo, hi]` |
| `cos` | `(Float64) -> Float64` | Cosine (radians) |
| `exp` | `(Float64) -> Float64` | e^x |
| `floor` | `(Float64) -> Float64` | Round toward -infinity |
| `isFinite` | `(Float64) -> Boolean` | True if not NaN or infinite |
| `isNaN` | `(Float64) -> Boolean` | True if NaN |
| `log` | `(Float64) -> Float64` | Natural logarithm |
| `log10` | `(Float64) -> Float64` | Base-10 logarithm |
| `log2` | `(Float64) -> Float64` | Base-2 logarithm |
| `max` | `(Number, Number) -> Number` | Larger of two scalars |
| `min` | `(Number, Number) -> Number` | Smaller of two scalars |
| `pow` | `(Float64, Float64) -> Float64` | Base raised to exponent |
| `random` | `() -> Float64` | Uniform random in `[0, 1)` |
| `round` | `(Float64) -> Float64` | Round to nearest integer |
| `sign` | `(Number) -> Int32` | -1, 0, or 1 |
| `sin` | `(Float64) -> Float64` | Sine (radians) |
| `sqrt` | `(Float64) -> Float64` | Square root |
| `tan` | `(Float64) -> Float64` | Tangent (radians) |
| `toFixed` | `(Float64, Int32) -> String` | Format to N decimal places |
| `trunc` | `(Float64) -> Float64` | Round toward zero |

---

### Examples

```lin
import { sqrt, pow, abs, floor, ceil, round, PI, toFixed } from "std/math"

sqrt(9.0)             // 3.0
pow(2.0, 10.0)        // 1024.0
abs(-5)               // 5
floor(3.9)            // 3.0
ceil(3.1)             // 4.0
round(3.5)            // 4.0
toFixed(3.14159, 2)   // "3.14"
PI                    // 3.141592653589793
```

---

### `clamp`

```lin
clamp(5, 1, 10)    // 5
clamp(-3, 1, 10)   // 1
clamp(15, 1, 10)   // 10
```

---

### `random`

```lin
val x = random()   // e.g. 0.7341293
```

---

### `isNaN`

Unlike `x == NAN` (which is always false due to IEEE 754), `isNaN(x)` correctly returns `true` for NaN:

```lin
isNaN(NAN)    // true
isNaN(0.0)    // false
```
