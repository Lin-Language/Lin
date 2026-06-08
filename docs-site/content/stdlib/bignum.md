# std/bignum

std/bignum — arbitrary-precision signed integers.

A `BigInt` is an opaque, immutable, refcounted heap handle (the Timer/Stream/Shared family),
backed by `num-bigint` in the runtime. It has no size limit other than memory; every operation
returns a fresh `BigInt`. With no operator overloading, arithmetic is written with named
functions and reads left-to-right via dot-application: `a.mul(x).add(b)`.

`BigInt` is a type alias to `Json` because the value is carried as a universal opaque tagged
box — the runtime tags it `TAG_BIGNUM` so its refcount/destructor are dispatched correctly.
Fallible functions return `BigInt | Error` (the canonical `{ "type":"error", "message" }`,
matched with `is Error`).

NOTE: cross-worker transfer of a BigInt is NOT supported in v1 (the handle is a raw pointer that
cannot be shared across a thread boundary); bignum math is main-thread.

## Reference

#### `BigInt`

```lin
type BigInt = Json
```


### Construction

#### `bigInt`

```lin
val bigInt = (n: Int64): BigInt
```

Build a `BigInt` from a machine integer.
- **`n`** — the Int64 value to widen.
- **Returns** the `BigInt`.

#### `parseBigInt`

```lin
val parseBigInt = (s: String): BigInt | Error
```

Parse a base-10 integer string of any length into a `BigInt`.
- **`s`** — the decimal string (optional leading `-`).
- **Returns** the `BigInt`, or an `Error` if `s` is not a valid integer.
- **Example:** parseBigInt("2").pow(100).toString()  // "1267650600228229401496703205376"

#### `zero`

```lin
val zero: BigInt
```

The `BigInt` constant 0.

#### `one`

```lin
val one: BigInt
```

The `BigInt` constant 1.

### Arithmetic

#### `add`

```lin
val add = (a: BigInt, b: BigInt): BigInt
```

Sum of `a` and `b`.
- **Returns** `a + b`.

#### `sub`

```lin
val sub = (a: BigInt, b: BigInt): BigInt
```

Difference of `a` and `b`.
- **Returns** `a - b`.

#### `mul`

```lin
val mul = (a: BigInt, b: BigInt): BigInt
```

Product of `a` and `b`.
- **Returns** `a * b`.

#### `div`

```lin
val div = (a: BigInt, b: BigInt): BigInt | Error
```

Truncating integer quotient of `a` and `b`.
- **Returns** `a / b`, or an `Error` if `b` is zero.

#### `mod`

```lin
val mod = (a: BigInt, b: BigInt): BigInt | Error
```

Remainder of `a` divided by `b` (sign follows the dividend).
- **Returns** `a % b`, or an `Error` if `b` is zero.

#### `pow`

```lin
val pow = (a: BigInt, e: Int64): BigInt | Error
```

Raise `a` to a non-negative integer power.
- **`e`** — the exponent.
- **Returns** `a ** e`, or an `Error` if `e` is negative.

#### `modPow`

```lin
val modPow = (base: BigInt, exp: BigInt, modulus: BigInt): BigInt | Error
```

Modular exponentiation `base ** exp mod modulus` (single fused op for crypto-sized values).
- **Returns** the result, or an `Error` if `modulus` is zero or `exp` is negative.

#### `neg`

```lin
val neg = (a: BigInt): BigInt
```

Negation.
- **Returns** `-a`.

#### `abs`

```lin
val abs = (a: BigInt): BigInt
```

Absolute value.
- **Returns** `|a|`.

### Comparison

#### `cmp`

```lin
val cmp = (a: BigInt, b: BigInt): Int32
```

Three-way compare.
- **Returns** -1 if `a < b`, 0 if equal, 1 if `a > b`. The comparison primitive
the predicates below are built on.

#### `eq`

```lin
val eq = (a: BigInt, b: BigInt): Boolean
```

- **Returns** true if `a == b`.

#### `lt`

```lin
val lt = (a: BigInt, b: BigInt): Boolean
```

- **Returns** true if `a < b`.

#### `lte`

```lin
val lte = (a: BigInt, b: BigInt): Boolean
```

- **Returns** true if `a <= b`.

#### `gt`

```lin
val gt = (a: BigInt, b: BigInt): Boolean
```

- **Returns** true if `a > b`.

#### `gte`

```lin
val gte = (a: BigInt, b: BigInt): Boolean
```

- **Returns** true if `a >= b`.

### Conversion

#### `toString`

```lin
val toString = (a: BigInt): String
```

Render as a base-10 string (the inverse of `parseBigInt`).
- **Returns** the decimal string.

#### `toInt64`

```lin
val toInt64 = (a: BigInt): Int64 | Error
```

Narrow to a machine integer.
- **Returns** the `Int64` value, or an `Error` if `a` does not fit in 64 bits.

#### `toFloat64`

```lin
val toFloat64 = (a: BigInt): Float64
```

Convert to a double (may lose precision for large magnitudes).
- **Returns** the `Float64` value.

#### `sign`

```lin
val sign = (a: BigInt): Int32
```

- **Returns** the sign of `a`: -1, 0, or 1.
