# Types Reference

## Primitive types

| Type | Description | Example literal |
| --- | --- | --- |
| `String` | UTF-8 text | `"hello"` |
| `Boolean` | Truth value | `true` / `false` |
| `Null` | Absence of value | `null` |
| `Int8` | 8-bit signed integer | `42i8` |
| `Int16` | 16-bit signed integer | `1000i16` |
| `Int32` | 32-bit signed integer (default) | `42` |
| `Int64` | 64-bit signed integer | `42i64` |
| `UInt8`–`UInt64` | Unsigned integer families | `255u8` |
| `Float32` | 32-bit IEEE 754 float | `3.14f32` |
| `Float64` | 64-bit IEEE 754 float (default) | `3.14` |

### Numeric literal typing

- A **type suffix pins the type**: `42i8` is `Int8`, `42u32` is `UInt32`, `3.14f32` is `Float32`, `3.14f64` is `Float64`.
- A **bare suffixless integer** defaults to `Int32` if it fits. If it exceeds the `Int32` range it widens to the smallest type that preserves the value (e.g. `1705314600000` becomes `Int64`) — it is never silently truncated.
- **Context can resize a bare literal**: `val x: Int64 = 42` types `42` as `Int64`.
- A **suffixed literal in a conflicting context is a compile error**: `val x: Int32 = 5i64` fails, because the suffix pins `5i64` to `Int64`.
- A bare floating-point literal defaults to `Float64`.

## `Number`

`Number` is a built-in union alias covering every numeric type family. It has no runtime representation of its own — every value retains its specific numeric type:

```lin
type Number =
  | Int8 | Int16 | Int32 | Int64
  | UInt8 | UInt16 | UInt32 | UInt64
  | Float32 | Float64
```

## `AnyVal`

`AnyVal` is the recursive union of all JSON-compatible values:

```lin
type AnyVal =
  | String | Boolean | Null | Number
  | AnyVal[]
  | { ...AnyVal }   // any object whose values are AnyVal
```

Use `AnyVal` when the shape of data is not statically known.

### `AnyVal` is a covariant sink

Any value assigns **into** `AnyVal` — `val j: AnyVal = anyValue` is always allowed. But `AnyVal` does **not** implicitly assign **out** to a concrete object type with required fields. To go from an untrusted `AnyVal` value to a concrete type you must either:

- validate via `fromJson` (from `std/json`), which decodes and type-checks recursively, returning `T | Error`; or
- narrow with an `is`/`has` pattern in a `match`.

```lin
import { fromJson } from "std/json"

type Person = { "name": String, "age": Int32 }

val decoded = Person.fromJson(someJson)   // Person | Error
```

## The `Error` type

`Error` is a built-in type, structurally equivalent to:

```lin
{ "type": String, "message": String }
```

Fallible stdlib operations return `T | Error`, and faults inside `async` thunks surface as `Error` at `await`. Use `is Error` to detect it.

## Union types

```lin
val x: String | Null = null
val id: String | Int32 = "user-42"

type Result<T, E> =
  | { "type": "success", "value": T }
  | { "type": "failure", "error": E }
```

Union types use `|`. The type `T | Null` is the common pattern for optional values.

## Intersection types

The record-only intersection operator `&` combines two record types into a single record type holding **all** the fields of both. It lets you extend a named record with extra fields without re-typing the base (ADR-061; `docs/SPECIFICATION.md` "Record intersection (`&`)"):

```lin
type Person = { "name": String, "age": Int32 }
type Employee = Person & { "salary": Int32 }

val e: Employee = { "name": "Alice", "age": 30, "salary": 50000 }
```

- `A & B` resolves to an ordinary `Type::Object` whose fields are the union of both operands' fields — there is no new runtime representation, so sealed records and width-subtyping apply to the result unchanged. When `A & B` is the body of a named `type`, the result is sealed like any named record.
- Both operands must be record types; intersecting a non-record (a scalar, an array, or a union) is a compile-time error.
- A field present in **both** operands must have a *compatible* type; the same key with conflicting types is a compile-time error (`intersection type has conflicting field …`).
- `&` binds **tighter than `|`**, so `A & B | C` parses as `(A & B) | C`, and `A & B & C` merges all three (left-associative).

## Object types

```lin
type Person = {
  "name": String,
  "age": Int32
}
```

Object types are structural. A value with additional fields is compatible with a smaller structural type (see [Structural typing](#structural-typing) below). Keys are quoted strings.

### Sealed named records

A **named** record type — `type Person = { … }` — is **sealed**: a value whose static type is `Person` holds *exactly* `Person`'s fields, no more. This is a representation guarantee that lets the compiler lay sealed records out as unboxed structs with constant-offset field access, so a typed record is dramatically faster than the equivalent dynamic `AnyVal` object.

Sealing does **not** weaken structural compatibility. When a wider value (one with extra fields) flows into a `Person`-typed slot — a parameter, an annotated `val`/`var`, a typed return, or a `Person[]` element — it is **copied** into a fresh value containing only `Person`'s fields. The extra fields are dropped *from the copy*; the original value is untouched in its own scope.

```lin
type Person = { "name": String }

val wide = { "name": "Alice", "age": 99 }   // anonymous record, extra field
val p: Person = wide                         // projects to a fresh { "name": "Alice" }
// p["age"]   → compile error: `age` is not a field of Person
wide["age"]                                  // still 99 — `wide` is unchanged
```

The practical consequence: a named record type can't be used as an open carrier that smuggles extra fields through to a later consumer — once a value is typed as a named record, its extra fields are gone. If you need to preserve arbitrary extra keys, type the value `AnyVal`, not a named record.

`Person.fromJson(json)` projects the same way: it validates and keeps exactly `Person`'s fields, dropping unknown keys.

### Index-signature (hashmap) types

A `{ String: T }` type is a **hashmap**: a dictionary with *any number* of dynamically-computed string keys, all mapping to value type `T`. This is distinct from a fixed record — a record has a known, fixed set of keys; a hashmap has an open, runtime-determined key set.

```lin
type Counts = { String: Int32 }

var counts: Counts = {}
counts["apple"] = 3
counts["pear"]  = 7
val n = counts["apple"]      // Int32 | Null  (a missing key reads as Null)
```

- `m[k]` yields `T | Null` (a missing key is `Null`). For a defaulted read, use `object.get(m, k, default)` from `std/object`.
- `m[k] = v` accepts any string key `k` and requires `v : T`.
- `keys(m) : String[]`, `values(m) : T[]`, and `entries(m)` are available via `std/object`.
- A hashmap is backed at runtime by a hashed container giving **O(1) average** lookup/insert — unlike a dynamic `AnyVal` object, whose small association-list layout is O(n) per access. Reach for `{ String: T }` whenever you have a genuinely large or open-keyed dictionary.
- There is no implicit `AnyVal → { String: T }` coercion — decode a `AnyVal` value through `fromJson` or narrow it, exactly as for any other concrete type.

**Aliased keys.** The key may be written as the literal `String`, or as **any type alias that resolves to `String`**. This lets a domain alias document intent at the call site:

```lin
type StopID = String

// A nested hashmap: outer keyed by stop, inner keyed by stop, value = hop count.
type Network = {
  StopID: {
    StopID: UInt8
  }
}

val n: Network = {}
```

Both `StopID` keys above resolve to `String`, so the type checks. An alias works at any nesting depth and in any key position. A key whose alias resolves to a *non*-`String` type is a compile-time error (`Map key type must be String, but it resolves to …`) — the underlying key type is always `String`; the alias is purely a naming convenience.

## Array types

`T[]` — unbounded array of `T`:

```lin
val names: String[] = ["Alice", "Bob"]
```

`[T1, T2, T3]` — fixed-length array with specified element types:

```lin
val pair: [String, Int32] = ["age", 42]
```

## Function types

```lin
type Predicate<T> = (T) => Boolean
type Mapper<T, U> = (T) => U
```

## Generic types

See [Generics](/reference/generics.html) for the full treatment.

Generic type declarations and applications both use angle brackets:

```lin
type Box<T> = {
  "value": T,
  "label": String
}

type Result<T, E> =
  | { "type": "success", "value": T }
  | { "type": "failure", "error": E }

type Mapper<T, U> = (T) => U

// Application: supply concrete types for the parameters.
type ParseResult = Result<Int32, String>
```

## Generic functions

A `val` function may declare type parameters before its argument list. They are inferred from the arguments at the call site:

```lin
val identity = <T>(x: T): T => x
val firstOf = <T>(xs: T[]): T => xs[0]
val pair = <A, B>(a: A, b: B): { "first": A, "second": B } =>
  { "first": a, "second": b }
```

`firstOf([1, 2, 3])` returns an `Int32`; `firstOf(["a"])` returns a `String` — the type parameter ties the result to the element type of the argument.

## Variance

Generic types are **covariant** in producer positions (return type, array element, container content) and **contravariant** in consumer positions (function arguments):

- `Person[]` is assignable to `AnyVal[]`.
- `Iterator<Person>` is assignable to `Iterator<AnyVal>`.
- A function returning `Person` is assignable to one returning `AnyVal`.
- `(AnyVal) => Int32` is assignable to `(Person) => Int32` (a consumer of `AnyVal` accepts a `Person`).

## Type-expression precedence

Type operators bind tightest-first:

| Order | Form | Example |
| --- | --- | --- |
| 1 | `T[]` | postfix array |
| 2 | `Generic<…>` | postfix generic application |
| 3 | `(T1, T2) => U` | function arrow |
| 4 | `T \| U` | union |

So `Int32 | String[]` parses as `Int32 | (String[])`, and `(Int32) => String[]` as `(Int32) => (String[])`. Parenthesise where the surface reading is unclear.

## Limitations

- You **cannot** use a generic application in an `is` pattern — `is Result<Int32, String>` is a compile-time error. Match the underlying tagged shape instead via `has { "type": "success", value }`.
- Cross-module generic functions are monomorphized per importer (each importing module compiles its own specialisations).

## Opaque runtime types

| Type | Description |
| --- | --- |
| `Iterator<T>` | Stateful traversal producing `T` |
| `Iterable<T>` | Any value that can produce `Iterator<T>` |
| `Promise<T>` | Value being computed on another thread |
| `Worker<Msg, Reply>` | Long-lived background thread |
| `ThreadPool` | Fixed-size thread pool |
| `Function` | Opaque function reference |

These types are not JSON values and cannot be stored in JSON objects or arrays.

## Structural typing

Types are structural by default. Two types are compatible if they describe the same shape:

```lin
type Named = { "name": String }

val greet = (x: Named): String => "Hello ${x["name"]}"

// Works — the value has at least the "name" field
greet({ "name": "Alice", "age": 30 })
```

## Numeric widening

Numeric types widen automatically in arithmetic. The result type is the smallest type that can fully represent **every value of both operands** (ADR-072). When operands differ in width *or* signedness, the result widens past both — because neither operand's type can represent the full range of the other:

```lin
val a: Int32 = 100
val b: UInt8 = 50
val sum: Int64 = a + b     // Int32 + UInt8 → Int64 (not Int32)
```

`Int32 + UInt32` widens to `Int64` for the same reason, even though both operands are 32-bit. (Comparison, bitwise, and shift operators keep the operand width; only arithmetic folds the result width in.)

## Explicit numeric narrowing

There is **no implicit narrowing** — a wider value never silently flows into a narrower type. To store a wide computed value into a narrow field, convert it explicitly with the `to*` conversion family from `std/number`. Each integer-narrowing cast is **overloaded** by argument type (ADR-073, folded into `to*` as overloads by ADR-075):

- a `UInt64` overload — for an already-*unsigned* (or bit-masked) source;
- an `Int64` overload — for a *signed/computed* `Int64` source. (`Int64 → UInt64` is not an implicit coercion, since it could wrap a negative, so a computed `Int64` resolves to this overload.)

Both truncate to the named width with two's-complement semantics; the overload merely records whether the source was signed or unsigned. Truncation never fails — an out-of-range value keeps its low bits.

```lin
import { toUInt8, toUInt16, toInt32 } from "std/number"

val a: Int32 = 100
val b: UInt8 = 50
val wide: Int64 = a + b           // a computed Int64

val month: UInt8 = toUInt8(wide)  // Int64 → UInt8  (low 8 bits)
val word: UInt16 = toUInt16(wide) // Int64 → UInt16
val small: Int32 = toInt32(wide)  // Int64 → Int32  (low 32 bits)
```

The family is `toInt8`/`toUInt8`/`toInt16`/`toUInt16`/`toUInt32`/`toInt64`/`toUInt64`, plus `toInt32` (which has both a `Float64` overload and an `Int64` overload) and `toFloat64`/`toFloat32`. Reach for these only where a value genuinely crosses into a narrower type — hot numeric record fields are typically kept `Int64` to avoid silent overflow on read-back.
