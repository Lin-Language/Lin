# Error Handling

Lin has **no exceptions**. There is no `throw`, no `try`/`catch`, and no implicit
error propagation. Failures are ordinary values: a function that can fail returns
a union type that includes the failure case, so its signature states up front that
it might fail (spec §20). This page is the reference for the error value model.

## Fallible operations return `T | Error`

A function that may fail returns a union of the success type and the failure type.
The convention for an unrecoverable-but-recoverable-by-the-caller failure is
`T | Error`:

```lin
import { readFile } from "std/fs"

val readConfig = (path: String): String =>
  match readFile(path)        // readFile : (String) => String | Error
    is Error => "read failed"
    else => "ok"
```

The fallible standard-library operations (filesystem, network, parsing, stream
terminals, `await`, …) all return `T | Error`. Narrow before use — an unhandled
`Error` is a type error at the use site, not a runtime surprise.

## The `Error` value shape

The canonical error value is the structural object:

```lin
{ "type": "error", "message": String }
```

This is the exact shape produced throughout the standard library (`std/fs`,
`std/json`, `std/http`, `std/csv`, …). It has **no special control-flow
behaviour** — it is an ordinary value. Detect it with the `is Error` pattern:

```lin
import { fromJson } from "std/json"

type Config = { "port": Int32 }

val load = (raw: String): String =>
  val decoded = Config.fromJson(raw)    // Config | Error
  match decoded
    is Error => "decode failed: ${decoded["message"]}"
    else => "port is ${decoded["port"]}"
```

`is Error` narrows the scrutinee: in the `is Error` arm `decoded["message"]` is a
typed `String` read; in the `else` arm `decoded` is the narrowed `Config`. The
built-in `Error` type is structurally `{ "type": String, "message": String }`, so
constructing one by hand is just an object literal:

```lin
val makeError = (msg: String): Error =>
  { "type": "error", "message": msg }
```

## Tagged-union result pattern

For a **domain-specific** outcome — where success and failure each carry
structured, application-meaningful data — the idiom is a tagged union with a
string-literal discriminant. This is distinct from the structural `Error` value
above; choose it when the caller branches on more than "did it fail":

```lin
import { isInt32, parseInt32 } from "std/number"

type Parsed =
  | { "type": "success", "value": Int32 }
  | { "type": "failure", "error": String }

val parseAge = (s: String): Parsed =>
  if isInt32(s) then
    { "type": "success", "value": parseInt32(s) }
  else
    { "type": "failure", "error": "not a number: ${s}" }
```

Handle it with `match` and `has` arms, which destructure the matched object:

```lin
val show = (s: String): String =>
  match parseAge(s)
    has { "type": "success", value } => "age is ${value}"
    has { "type": "failure", error } => "error: ${error}"
    else => "?"
```

Tag every variant with a **string-literal** discriminant (`"type": "success"`) so
`match` narrows cleanly and exhaustively.

## Null-coalescing `??`

`a ?? b` evaluates to `a` when `a` is non-null, and to `b` otherwise (spec §8.3,
ADR-066). `a` is evaluated **once**, and `b` only when `a` is `Null`
(short-circuit). It is the idiomatic way to default a nullable read — for example
a missing map key, which reads as `Null`:

```lin
val user: { String: String } = {}
val name = user["name"] ?? "anonymous"    // String
```

Two rules are load-bearing:

- **It coalesces `Null` only — never `Error`.** If the left operand is
  `T | Null | Error` and holds an `Error`, that `Error` flows **through** `??`
  unchanged; it is *not* replaced by the default. This keeps real failures from
  being silently swallowed — a present value or an absent-key `Null` is defaulted,
  but a genuine `Error` still surfaces and must be handled.
- **A never-null left operand is a compile error** (*"left operand of `??` is
  never null"*). The left's type must be able to be `Null` (bare `Null`, a union
  containing `Null`, or `AnyVal`); otherwise the default is dead code.

`??` binds below `&&`/`||`; mixing them unparenthesised is a parse error — write
`(a || b) ?? c`.

## Decoding untrusted data with `fromJson`

`T.fromJson(raw)` (equivalently `fromJson(T, raw)`, from `std/json`) is the
sanctioned way to turn untyped wire data into a typed value. It decodes and
**type-checks recursively** against `T`, returning `T | Error`: a structural
mismatch produces an `Error` rather than an unsound value. It is the bridge from
`AnyVal` to a concrete type — there is no implicit `AnyVal → T` coercion.

```lin
import { fromJson } from "std/json"

type Person = { "name": String, "age": Int32 }

val decoded = Person.fromJson(raw)    // Person | Error — keeps exactly Person's fields
```

## Runtime errors vs value errors

The two failure channels are distinct:

- **Value errors** are the `T | Error` and tagged-union results above — ordinary
  values you narrow and handle. This is the channel for anything a caller can
  reasonably recover from.
- **Runtime errors** are a small set of language-level faults that terminate the
  program with a diagnostic and cannot be caught (spec §20.1): array index out of
  bounds, integer division by zero, an explicit narrowing cast that loses
  information, and a non-exhaustive `match` with no matching arm and no `else`.
  They are reserved for unrecoverable program bugs.

Note the asymmetry with bracket access: a **missing object/map key** is *not* a
runtime error — it yields `Null` (see
[Records & Objects](/reference/records.html) and
[Maps & Collections](/reference/maps.html)). Only an out-of-range **array** index
faults.

## Async fault isolation

An `async` thunk is a fault-isolation boundary: a fault inside it does not crash
the program but surfaces as an `Error` value at `await`, so an awaited result is
typed `T | Error` and must be narrowed like any other fallible value. See
[Concurrency](/reference/concurrency.html).
