# Expressions Reference

Everything in Lin is an expression. Blocks, `if`, `match`, function bodies — all produce values.

## Operator precedence

From highest to lowest:

| Level | Operators | Notes |
| --- | --- | --- |
| 1 | `()`, `[]`, `.` | Call, index, dot application |
| 2 | `~`, `!` | Unary bitwise NOT and logical NOT (the only unary operators) |
| 3 | `*`, `/`, `%` | Multiplication, division, modulo |
| 4 | `+`, `-` | Addition, subtraction |
| 5 | `<<`, `>>` | Bitwise shift |
| 6 | `<`, `<=`, `>`, `>=` | Comparison |
| 7 | `==`, `!=` | Equality |
| 8 | `&` | Bitwise AND |
| 9 | `^` | Bitwise XOR |
| 10 | `\|` | Bitwise OR (not union — value context) |
| 11 | `&&` | Logical AND (short-circuit) |
| 12 | `\|\|` | Logical OR (short-circuit) |
| 13 | `??` | Null-coalescing (short-circuit; lowest rung) |

All binary operators are left-associative.

`??` is the lowest-precedence binary operator (below `||`), so `a ?? b == c` parses as `a ?? (b == c)`. An **unparenthesised mix of `??` directly with `&&` or `||`** is a parse error in either direction (`a || b ?? c`, `a ?? b || c`) — parenthesise the logical sub-expression: `(a || b) ?? c`.

## Arithmetic

```lin
val a = 10 + 3     // 13
val b = 10 - 3     // 7
val c = 10 * 3     // 30
val d = 10 / 3     // 3 (integer division)
val e = 10 % 3     // 1
```

`+` works only on numeric types. String concatenation uses interpolation.

There is **no unary minus operator**. To negate a value, subtract it from zero:

```lin
val neg = 0 - x
```

## Comparison

```lin
val eq = 1 == 1        // true
val ne = 1 != 2        // true
val lt = 3 < 5         // true
val obj = { "a": 1 } == { "a": 1 }   // true (structural)
```

Object equality is order-independent; array equality is ordered.

## Logical

```lin
val a = true && false   // false
val b = true || false   // true
```

Logical NOT uses the unary `!` operator:

```lin
val notReady = !ready
```

(`ready == false` also works, but `!` is the idiomatic form.)

## Null-coalescing

`a ?? b` yields `a` when `a` is non-null, and `b` otherwise. It is exactly equivalent to `if a != null then a else b`: `a` is evaluated **once**, and `b` only when `a` is `Null` (short-circuit).

```lin
val counts: { String: Int32 } = {}
val n = counts["missing"] ?? 0      // n : Int32 — the absent-key Null is replaced
```

The result type strips `Null` from the left and unions the right's type, collapsing to the bare type when the default is assignable to it — so `counts[k] ?? 0` is a plain `Int32`, usable in arithmetic without further narrowing.

`??` chains left to right, which makes it ideal for a cascade of fallbacks:

```lin
val m: { String: String } = {}
val pick = m["y"] ?? m["x"] ?? "default"
```

**It coalesces `Null` only — never `Error`.** A left operand of type `T | Null | Error` that holds an `Error` flows that `Error` **through** to the result; it is *not* replaced by the default, so a real failure is never silently swallowed.

**A statically never-null left operand is a compile error** (ADR-066). `5 ?? 1` makes the default dead code, so the compiler reports *"left operand of `??` is never null"*. The left type must be able to be `Null` — `Null` itself, a union containing `Null`, or `AnyVal`.

## Bitwise

```lin
val a = 0xFF & 0x0F    // 15
val b = 1 << 4          // 16
val c = ~0              // -1 (all bits set)
```

Bitwise operators require integer operands.

## String interpolation

```lin
val name = "Alice"
val age = 30
val s = "Hello ${name}, you are ${age} years old."
```

Any expression can appear inside `${...}`. It is the only way to build strings from parts — `+` does not work on strings.

## Bracket access

```lin
val obj = { "key": "value" }
val arr = [1, 2, 3]

obj["key"]    // "value"
arr[0]        // 1
obj["missing"] // null (never errors)
arr[99]       // runtime error (out of bounds)
```

## `if` expression

Every `if` requires an `else`. Two layout forms:

```lin
// Inline
val label = if score >= 90 then "A" else "B"

// Block with then on condition line
val label = if score >= 90 then
  "A"
else
  "B"
```

## `match` expression

```lin
val desc = match value
  is Null   => "null"
  is String => "string"
  else      => "other"
```

See [Pattern Matching Reference](/reference/pattern-matching.html) for full syntax.

## `is` and `has` as expressions

`is` and `has` return `Boolean` and can appear anywhere:

```lin
val isAdult = person has { age } && person["age"] >= 18
val isNull = value is Null
```

## Negating boolean results

Use the unary `!` operator:

```lin
val isNotNull = !(value is Null)
```

## Assignments as expressions

Assignment evaluates to the assigned value:

```lin
var x = 0
val result = x = x + 1   // result is 1, x is 1
```
