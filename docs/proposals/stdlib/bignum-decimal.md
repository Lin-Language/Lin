## Status: proposal

# std/bignum and std/decimal

Lin's numeric surface today (`std/number`, `std/math`) is fixed-width machine arithmetic: `Int32`/`Int64`/`UInt*` wrap on overflow and `Float64` carries the usual binary-rounding error. Two real workloads have no faithful type:

1. **Arbitrary-precision integers** — cryptography (modular exponentiation on 2048-bit moduli), number theory, and exact combinatorics (`100!` is ~525 bits, far past `Int64`'s 63). Java has `BigInteger`, Python's native `int` is unbounded, Scala has `BigInt`, Go has `math/big`, Rust has `num-bigint`. Lin has nothing — a factorial silently wraps to garbage.
2. **Exact base-10 decimal** — **money**. `0.1 + 0.2 != 0.3` in `Float64`, and summing invoice line items in binary floating point accumulates cents of drift that fail an audit. The local Solirius-invoice skill computes day-rate line totals and applies 20% VAT rounded to 2 decimal places; doing that in `Float64` is wrong, and doing it in integer-pence-by-hand is error-prone the moment a percentage or a non-2dp intermediate appears. Java has `BigDecimal`, Python has `decimal`, .NET has `decimal` — but **Go, Node, and Rust have no exact decimal in their standard libraries**, so a first-class `std/decimal` is a genuine differentiator for Lin.

**Recommendation: two modules, `std/bignum` (`BigInt`) and `std/decimal` (`Decimal`).** They share an implementation strategy (opaque heap handle wrapping a Rust big-number type) but have different surfaces — `Decimal` carries a *scale* and forces a rounding policy on division and `round`, `BigInt` does not. Splitting them keeps each module's import line and mental model focused, mirrors the `std/number` vs `std/math` split, and lets a money program import only `std/decimal` without dragging in number-theory helpers (and vice versa). `std/decimal` *may* depend on `std/bignum` internally for its unscaled coefficient, but that is an implementation detail, not a public coupling.

---

## The central constraint: no operator overloading

Lin has **no operator overloading**. `+`, `-`, `*`, `/`, `<`, `==` are defined only on the built-in machine numerics; there is no language mechanism to make them work on a user/opaque type, and the spec defines no such hook. This is verified by its absence: every existing opaque type (`Timer`, `Stream<T>`, `Bus<T>`) is manipulated entirely through named functions, never operators.

The consequence is unavoidable and must be stated up front: **`BigInt` and `Decimal` arithmetic is written with named functions**, not infix operators. There is no `a + b`; there is `add(a, b)`. A polynomial like `a*x*x + b*x + c` becomes a nest of calls. This is the same trade-off Go's `math/big` lives with (`new(big.Int).Add(x, y)`) and is strictly more readable than Go because Lin's **dot-application** (`x.f(y) == f(x, y)`) lets the calls chain left-to-right in evaluation order:

```txt
// a*x*x + b*x + c   with a,b,c,x : BigInt
a.mul(x).mul(x).add(b.mul(x)).add(c)
```

This reads as a pipeline ("take a, multiply by x, multiply by x, add b·x, add c") rather than an inside-out call nest. It is the recommended idiom throughout this proposal. Whether to lobby for a language-level operator-overloading feature is discussed in *Implementation notes*; the short answer is **no** — dot-application gives most of the ergonomic win at none of the type-inference and dispatch cost, and overloading would be a large language change for two library types.

---

# std/bignum

Arbitrary-precision signed integers. A `BigInt` is an **opaque heap handle** (like `Timer`/`Stream`): it is immutable, refcounted, and every operation returns a fresh `BigInt` rather than mutating in place. There is no size limit other than available memory.

Import:

```txt
import { bigInt, parseBigInt, add, mul, pow, cmp, toString } from "std/bignum"
```

### Construction

```txt
val bigInt:      (n: Int64)  -> BigInt          // exact widen from a machine integer
val parseBigInt: (s: String) -> BigInt | Error  // base-10; Error on malformed input
val zero:        BigInt                          // the constant 0
val one:         BigInt                          // the constant 1
```

`bigInt` widens any `Int64` (and therefore any narrower integer) exactly. `parseBigInt` accepts an optional leading `-`/`+` and base-10 digits; anything else is an `Error` (`{ "type":"error", "message": String }`, matched with `is Error`). There is no fixed-width overflow, so unlike `parseInt32` there is no value too large — only syntactically invalid strings fail.

```txt
bigInt(42)                         // 42
parseBigInt("9007199254740993")    // an integer past Float64's exact range
parseBigInt("123456789012345678901234567890")  // fine, arbitrary length
parseBigInt("0x1f")                // Error (no base prefix; base-10 only)
parseBigInt("12.5")                // Error (not an integer)
```

---

### Arithmetic

```txt
val add: (a: BigInt, b: BigInt) -> BigInt
val sub: (a: BigInt, b: BigInt) -> BigInt
val mul: (a: BigInt, b: BigInt) -> BigInt
val div: (a: BigInt, b: BigInt) -> BigInt | Error   // Error on divide-by-zero
val mod: (a: BigInt, b: BigInt) -> BigInt | Error   // Error on divide-by-zero
val pow: (a: BigInt, e: Int64)  -> BigInt | Error   // Error if e < 0
val neg: (a: BigInt)            -> BigInt
val abs: (a: BigInt)            -> BigInt
```

`div` is **integer (truncating) division** — `div(bigInt(7), bigInt(2))` is `3` — and `mod` is the matching remainder, so `add(mul(div(a,b), b), mod(a,b)) == a`. Both return `Error` (not a runtime trap) when the divisor is zero, because divide-by-zero is a recoverable input condition for a fallible library.

`pow` raises to an `Int64` exponent (the exponent is a *count*, not a big number — a 2-billion-digit exponent is not a realistic use and would not fit in memory anyway). A negative exponent is an `Error` rather than a truncated-to-zero `BigInt`.

```txt
import { bigInt, mul, sub, pow } from "std/bignum"

// (x - 1)^3  with x : BigInt
val cube = sub(x, bigInt(1)).pow(3)        // dot-application reads left-to-right
```

For modular exponentiation (the crypto-critical operation, where `pow` then `mod` would build an astronomically large intermediate), provide a fused primitive:

```txt
val modPow: (base: BigInt, exp: BigInt, modulus: BigInt) -> BigInt | Error
```

`modPow` computes `base^exp mod modulus` without materialising `base^exp`; `Error` if `modulus` is zero or `exp` is negative. This is the one place a fused operation is mandatory rather than a convenience — `pow` then `mod` is not just slow, it exhausts memory for cryptographic sizes.

---

### Comparison

```txt
val cmp: (a: BigInt, b: BigInt) -> Int32   // -1 if a<b, 0 if a==b, 1 if a>b
val eq:  (a: BigInt, b: BigInt) -> Boolean
val lt:  (a: BigInt, b: BigInt) -> Boolean
val lte: (a: BigInt, b: BigInt) -> Boolean
val gt:  (a: BigInt, b: BigInt) -> Boolean
val gte: (a: BigInt, b: BigInt) -> Boolean
```

`cmp` is the primitive (a single three-way comparison, the right shape for sorting and for `sortBy`); `eq`/`lt`/… are thin conveniences over it so the common predicate reads as a word rather than `cmp(a,b) == 0`. They exist because, with no operator overloading, `a < b` on `BigInt` is a type error — `a.lt(b)` is the replacement.

```txt
bigInt(10).cmp(bigInt(7))   //  1
bigInt(3).lt(bigInt(3))     //  false
bigInt(3).lte(bigInt(3))    //  true
```

---

### Conversion

```txt
val toString:  (a: BigInt) -> String          // base-10, with leading "-" if negative
val toInt64:   (a: BigInt) -> Int64 | Error    // Error if out of Int64 range
val toFloat64: (a: BigInt) -> Float64          // LOSSY past 2^53; always succeeds
val sign:      (a: BigInt) -> Int32            // -1, 0, or 1
```

`toString` is the canonical, exact serialisation and the way a `BigInt` enters string interpolation (`"${toString(n)}"` — a `BigInt` does not auto-stringify, being opaque). `toInt64` is fallible because a `BigInt` may not fit; `toFloat64` is **lossy** (it rounds to the nearest `Float64`, losing precision past 2^53) but always succeeds — the loss is documented, not signalled, mirroring `std/number.toFloat64`.

---

### Example — exact factorial

```txt
import { bigInt, one, mul, toString } from "std/bignum"
import { range, reduce } from "std/iter"

// 100! — 158 digits, impossible in Int64
val factorial = (n: Int32): BigInt =>
  range(1, n + 1).reduce(one, (acc, i) => acc.mul(bigInt(i)))

print(toString(factorial(100)))
// 93326215443944152681699238856266700490715968264381621468592963895217...000000
```

The `reduce` seeds with `one` and folds `mul` over `bigInt(i)` for each `i` — exact at every step, no overflow, no rounding.

---

# std/decimal

Exact base-10 fixed-point arithmetic for **money and other exact-decimal** work. A `Decimal` is an **opaque heap handle** representing a value as an integer *coefficient* scaled by a power of ten — `value = coefficient × 10^(-scale)`. `1.50` has coefficient `150` and scale `2`. There is **no binary rounding error**: `decimal("0.1")` plus `decimal("0.2")` is exactly `decimal("0.3")`.

Import:

```txt
import { decimal, add, mul, div, round, setScale, toString, RoundHalfEven } from "std/decimal"
```

### Construction

```txt
val decimal:   (s: String)  -> Decimal | Error  // PREFERRED: exact from a base-10 literal string
val fromInt:   (n: Int64)   -> Decimal           // exact; scale 0
val fromFloat: (f: Float64) -> Decimal           // LOSSY — see warning
val zero:      Decimal
val one:       Decimal
```

**`decimal(s)` is the preferred constructor and the only exact one for fractional values.** It parses a base-10 numeral (optional sign, digits, optional `.` and fractional digits, optional `e`/`E` exponent) and preserves the written scale: `decimal("1.50")` has scale 2 (it remembers the trailing zero), `decimal("1.5")` has scale 1. Malformed input is an `Error`.

**`fromFloat` is lossy and is a footgun for money — avoid it.** A `Float64` *already* carries binary rounding error before `std/decimal` ever sees it: `0.1` is not exactly representable, so `fromFloat(0.1)` yields a `Decimal` near `0.1000000000000000055511151231257827021181583404541015625`, not `0.1`. It exists only for interop with values that genuinely originate as `Float64` (a sensor reading, a `std/math` result). **For money, always start from a string** (`decimal("0.10")`), never from a float literal. The function is named `fromFloat` (not `decimal`) precisely so the lossy path is explicit at every call site.

```txt
decimal("19.99")        // exact, scale 2
decimal("1.50")         // exact, scale 2 (trailing zero kept)
decimal("-0.005")       // exact, scale 3
fromInt(100)            // 100, scale 0
fromFloat(0.1)          // 0.1000000000000000055...  ← DO NOT use for money
decimal("1,000.00")     // Error (no thousands separators)
```

---

### Rounding modes

Division and `round` require an explicit **rounding mode** — there is no default, because the right policy is domain-specific and a silent default is how money bugs ship. The mode is an opaque enumerated value (a small set of module constants):

```txt
val RoundHalfUp:   RoundingMode   // 0.5 → away from zero  (2.5 → 3, -2.5 → -3); common "schoolbook" rounding
val RoundHalfEven: RoundingMode   // 0.5 → nearest even   (2.5 → 2, 3.5 → 4); "banker's rounding"
val RoundFloor:    RoundingMode   // toward -∞            (2.9 → 2, -2.1 → -3)
val RoundCeil:     RoundingMode   // toward +∞            (2.1 → 3, -2.9 → -2)
val RoundDown:     RoundingMode   // toward zero, truncate (2.9 → 2, -2.9 → -2)
```

**For money, prefer `RoundHalfEven` (banker's rounding)** — it is the IEEE 754 default and avoids the systematic upward bias of `RoundHalfUp` when rounding many half-cent values, which matters when you round thousands of line items. Use `RoundHalfUp` only when a regulation or invoice convention specifically mandates "round half up". The important point is that **the choice is forced to be visible** at the call site.

---

### Arithmetic

```txt
val add: (a: Decimal, b: Decimal) -> Decimal
val sub: (a: Decimal, b: Decimal) -> Decimal
val mul: (a: Decimal, b: Decimal) -> Decimal
val neg: (a: Decimal)             -> Decimal
val abs: (a: Decimal)             -> Decimal
```

`add`, `sub`, `mul` are **always exact** and never round: the result's scale is large enough to hold the true value (`add` takes the max input scale; `mul` sums the input scales — `1.50 * 1.50` is exactly `2.2500`, scale 4). This is the key money property: a chain of additions and a multiply-by-percentage loses nothing; you round **once, explicitly, at the end**.

```txt
val div: (a: Decimal, b: Decimal, scale: Int32, mode: RoundingMode) -> Decimal | Error
val pow: (a: Decimal, e: Int32) -> Decimal      // exact; e >= 0
```

`div` is the **only arithmetic that rounds**, because an exact quotient may be non-terminating (`1/3`). It therefore *requires* a target `scale` and `mode`: `div(a, b, 2, RoundHalfEven)` yields a 2-decimal-place result. It is `Error` on divide-by-zero. `pow` raises to a non-negative `Int32` power exactly (scale multiplies out); for money this is rarely needed but is exact when it is.

There is deliberately **no bare `div(a, b)`** — an un-rounded exact division has no representable result in general, so omitting the policy is a type error rather than a silent surprise.

---

### Comparison

```txt
val cmp: (a: Decimal, b: Decimal) -> Int32   // -1 / 0 / 1, compares VALUES not scales
val eq:  (a: Decimal, b: Decimal) -> Boolean
val lt:  (a: Decimal, b: Decimal) -> Boolean
val lte: (a: Decimal, b: Decimal) -> Boolean
val gt:  (a: Decimal, b: Decimal) -> Boolean
val gte: (a: Decimal, b: Decimal) -> Boolean
```

Comparison is by **numeric value, ignoring scale**: `eq(decimal("1.5"), decimal("1.50"))` is `true` even though the scales differ. (Use `scale` if you need to distinguish `1.5` from `1.50` as written; the values are equal.)

---

### Rounding and scale

```txt
val round:    (d: Decimal, scale: Int32, mode: RoundingMode) -> Decimal
val setScale: (d: Decimal, scale: Int32, mode: RoundingMode) -> Decimal
val scale:    (d: Decimal) -> Int32     // current number of fractional digits
```

`round` returns `d` rounded to `scale` fractional digits using `mode`. `setScale` is the same operation framed as "force this value to exactly N decimal places" — increasing scale pads with zeros exactly (no mode needed in practice, but the parameter is kept so the signature is uniform and a *decrease* still rounds correctly). For money the canonical final step is `round(total, 2, RoundHalfEven)`.

```txt
round(decimal("2.345"), 2, RoundHalfEven)   // 2.34  (4 is even, .345 → .34)
round(decimal("2.355"), 2, RoundHalfEven)   // 2.36  (6 is even)
round(decimal("2.345"), 2, RoundHalfUp)     // 2.35  (half always up)
setScale(decimal("1.5"), 2, RoundHalfEven)  // 1.50  (padded)
scale(decimal("1.50"))                       // 2
```

---

### Conversion

```txt
val toString:  (d: Decimal) -> String          // exact base-10, scale preserved
val toInt64:   (d: Decimal) -> Int64 | Error    // Error if non-integer OR out of range
val toFloat64: (d: Decimal) -> Float64          // LOSSY; always succeeds
```

`toString` is exact and scale-preserving (`toString(decimal("1.50"))` is `"1.50"`, not `"1.5"`) — it is how a `Decimal` enters interpolation and how money is rendered. `toInt64` is `Error` if the value has a non-zero fractional part *or* overflows. `toFloat64` is lossy (reintroduces binary rounding) and exists only for handing a value to a `Float64`-only API; never round-trip money through it.

---

### Example — invoice with VAT (the Solirius case)

Sum day-rate line items, apply 20% VAT, round to pence with banker's rounding — exact throughout, rounded once:

```txt
import {
  decimal, fromInt, add, mul, round, toString, zero, RoundHalfEven
} from "std/decimal"
import { reduce } from "std/iter"

type LineItem = { "description": String, "days": Int32, "dayRate": Decimal }

val items: LineItem[] = [
  { "description": "Consulting",  "days": 18, "dayRate": decimal("525.00") },
  { "description": "On-call",     "days":  2, "dayRate": decimal("612.50") },
  { "description": "Adjustment",  "days":  1, "dayRate": decimal("-87.33") }
]

// net = Σ days × dayRate   — exact, no rounding yet (scales accumulate)
val net = items.reduce(zero, (acc, it) =>
  acc.add(fromInt(it["days"]).mul(it["dayRate"])))

val vatRate = decimal("0.20")
val vat     = net.mul(vatRate)             // exact, scale grows
val gross   = net.add(vat)                 // exact

// round to pence ONCE, at the presentation boundary
val netPence   = round(net,   2, RoundHalfEven)   //  9912.67
val vatPence   = round(vat,   2, RoundHalfEven)   //  1982.53
val grossPence = round(gross, 2, RoundHalfEven)   // 11895.20

print("Net:   £${toString(netPence)}")
print("VAT:   £${toString(vatPence)}")
print("Gross: £${toString(grossPence)}")
```

No `Float64` touches the money path: `decimal("525.00")` is exact, `mul`/`add` are exact, and the single `round(..., 2, RoundHalfEven)` at the end is the only rounding — which is exactly the property an audit requires and which `Float64` cannot give.

---

## Implementation notes

### Runtime crates

- **`std/bignum` → `num-bigint`** (with `num-integer`/`num-traits` for `modpow`, `gcd`, sign). `BigInt::modpow` is provided directly, covering the crypto case. Parsing/formatting base-10 is built in.
- **`std/decimal` → `rust_decimal`** for the common money range (it is a fixed 96-bit coefficient, 28–29 significant digits, which covers every realistic invoice and is fast), **or `bigdecimal`** if truly unbounded scale/precision is required. Recommendation: **`rust_decimal`** for v1 — 28 digits is far more than money needs, the performance is good, and its `RoundingStrategy` enum maps one-to-one onto the proposed `RoundingMode` constants (`MidpointNearestEven` = `RoundHalfEven`, `MidpointAwayFromZero` = `RoundHalfUp`, etc.). If a use case needs more than 28 digits we can swap the backing type without changing the Lin surface, since the handle is opaque. (Note: `rust_decimal`'s coefficient is *not* a `num-bigint`, so the "`std/decimal` may use `std/bignum` internally" remark above applies only if we pick the `bigdecimal` backend.)

Both are exposed as **runtime intrinsics** (`lin_bigint_add`, `lin_decimal_div`, …) wrapped by thin `stdlib/bignum.lin` / `stdlib/decimal.lin` modules, exactly like `std/path` wraps the `url`-style intrinsic core. The comparison conveniences (`eq`/`lt`/…) and possibly `abs`/`neg` can be written in **pure Lin** over `cmp` and the arithmetic primitives, keeping the foreign surface minimal.

### Opaque-handle representation and RC/ownership

A `BigInt`/`Decimal` is **not** a Lin scalar (unlike `Ptr`, which is an `Int64` alias and never refcounted). It is a variable-length heap object, so it must be a **refcounted opaque handle** carried as a `TAG`-boxed pointer, in the same family as `Timer`, `Stream<T>`, and `Bus<T>`. Concretely:

- The runtime allocates a `Box<BigInt>` / `Box<Decimal>` (Rust heap), and Lin holds an opaque pointer inside a tagged box. A new tag (e.g. `TAG_BIGNUM`) carries a **destructor** that drops the Rust `Box`, wired into `release_tagged_payload` / `lin_object_release` the same way other foreign handles are. This is the load-bearing part: get the drop path wrong and it either leaks the Rust allocation (every arithmetic op allocates a fresh handle — a hot factorial/crypto loop would leak unboundedly, exactly the class of leak documented in the RAPTOR work) or double-frees it under ASan.
- **Values are immutable; every operation returns a fresh +1-owned handle.** `add(a, b)` borrows `a` and `b` (no retain) and produces a new owned box — the standard call-result ownership contract. This is allocation-heavy (a `reduce`-fold factorial allocates one `BigInt` per step) but correct and simple; `num-bigint`'s own allocation dominates anyway. The RC rules are the existing ones for owned call results — no new ownership shape — but they **must be verified under ASan**, not just `cargo test`, since this is precisely the owned-handle/destructor class that the codebase's recurring UAF/double-free bugs come from.
- `toString` produces an ordinary Lin `String` (a separate allocation); the handle itself never auto-stringifies, so interpolation requires explicit `toString`, which the proposal already states.
- Cross-thread transfer (sending a `BigInt` to a worker) follows the deep-copy thread-transfer rule (ADR-043): the transfer must clone the Rust value into the destination thread's heap, since the handle is a raw pointer that cannot be shared across the boundary. For v1 it is acceptable to **not** support sending these handles across a worker boundary (document it) rather than risk the cross-module generic worker-transfer crash class; money/bignum math is overwhelmingly main-thread.

### The no-operator-overloading ergonomic cost — and whether to lobby for a language change

The cost is real but **bounded and idiomatic**: arithmetic is named functions, and dot-application (`a.mul(x).add(b)`) recovers left-to-right pipeline readability that pure prefix calls lack. It is no worse than Go's `math/big`, and Lin's chaining makes it better. The recommendation is **not to lobby for operator overloading** for these two types:

- Operator overloading is a large, cross-cutting language change (resolution, type inference interacting with the existing argument-driven generic inference, error messages, the formatter) for the benefit of essentially two library types. The memory of Lin's inference being argument-driven and fragile around generics suggests overloading would interact badly with the existing checker.
- It would set a precedent (every opaque type wanting operators) and erode the current clean invariant that *operators mean machine arithmetic, functions mean everything else* — an invariant that makes Lin code easy to read and the checker simple.
- Dot-application already delivers ~90% of the ergonomic value. The remaining gap (deeply nested algebraic expressions) is rare in the actual use cases (money is `add`/`mul`/`round`; crypto is `modPow`), so the payoff is low.

If a future, broader case for overloading arises (e.g. user-defined vector/matrix types proliferate), it should be evaluated as a general language feature on its own merits — not bootstrapped through `std/bignum`. For this proposal, named functions + dot-application is the right answer.

### Module split, again

Ship `std/bignum` and `std/decimal` as separate modules with separate import lines and separate `STDLIB.md` sections, added to the Index table and the per-module function tables in house style. They share runtime infrastructure (the tagged-handle/destructor machinery) but present two focused surfaces. Adding them does not touch `std/number`/`std/math` (no import ripple) — they are additive.
