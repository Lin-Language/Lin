# std/object

std/object — object introspection and transformation, plus Result/Option ergonomics.

import { keys, values, entries, fromEntries, get, merge, pick, omit, mapValues, isEmpty } from "std/object"

`keys`/`values`/`entries` are tag-aware — they work on both a plain `{}`/`Json` record and a typed
index-signature map `{ String: T }` (ADR-055). `merge`/`pick`/`omit`/`mapValues` are generic over
`{ String: T }` and return a typed map; pass them a value annotated `{ String: T }` (there is no
implicit `Json -> { String: T }` coercion). Over a typed map, key order is hash order, not insertion
order. `get` is the idiomatic defaulted read (`m[k] ?? default`) — a bare `m[k]` already yields
`T | Null`, so the helper folds in a fallback whose type `D` is independent of the value type `T`.

This module also carries the Result/Option ergonomics folded in from the former std/result
(error/isOk/isError/isNull/unwrapOr) — see the section lower in the file. Lin has NO
`Result`/`Option` wrapper ADT: a fallible call returns `T | Error`, an absence-y call returns
`T | Null`, and the success value IS the value with no unwrap ceremony on the happy path.

## Reference

#### `keys`

```lin
val keys = (obj: Json): String[]
```

`keys`/`values`/`entries` are deliberately typed `Json`, NOT `<T>(obj: { String: T })`. They are
tag-aware at runtime (the `lin_*_any` bridges dispatch on the boxed value's tag, ADR-055), so the
SAME function serves both a plain `Json`/`{}` record (TAG_OBJECT → insertion order) and a typed
index-signature map `{ String: T }` (TAG_MAP → hash order). Re-typing the parameter to
`{ String: T }` would reject the dominant use — introspecting an arbitrary `Json`/`{}` record (e.g.
`keys(parsedConfig)`), since `Json → { String: T }` is correctly rejected (§5.1.1) — and no caller
wants a typed `T[]`/`[String,T][]` back, since these are used for dynamic introspection.

Return the keys of an object or map.
- **`obj`** — any `Json`/`{}` record or `{ String: T }` map.
- **Returns** a `String[]` of the keys (insertion order for a record, hash order for a map).
- **Example:** keys({ "a": 1, "b": 2 })   // ["a", "b"]

#### `values`

```lin
val values = (obj: Json): Json[]
```

Return the values of an object or map.
- **`obj`** — any `Json`/`{}` record or `{ String: T }` map.
- **Returns** a `Json[]` of the values, in key order.
- **Example:** values({ "a": 1, "b": 2 })   // [1, 2]

#### `entries`

```lin
val entries = (obj: Json): Json[]
```

Return the key/value pairs of an object or map.
- **`obj`** — any `Json`/`{}` record or `{ String: T }` map.
- **Returns** a `Json[]` of `[key, value]` pairs, in key order.
- **Example:** entries({ "a": 1, "b": 2 })   // [["a", 1], ["b", 2]]

#### `fromEntries`

```lin
val fromEntries = (pairs: Json): Json
```

Build an object from a list of `[key, value]` pairs (the inverse of `entries`). Later pairs with
the same key overwrite earlier ones.
- **`pairs`** — an array of `[String, value]` pairs (typed `Json`).
- **Returns** a `Json` object mapping each key to its value.
- **Example:** fromEntries([["a", 1], ["b", 2]])   // { "a": 1, "b": 2 }
- **Example:** entries(obj).map(([k, v]) => [k, v * 2]).fromEntries()   // double every value

#### `merge`

```lin
val merge = <T>(a: { String: T }, b: { String: T }): { String: T }
```

Merge two maps into a new one. Right-biased: keys present in `b` win over those in `a`.
- **`a`** — the base map.
- **`b`** — the overriding map; its entries take precedence on key collisions.
- **Returns** a new `{ String: T }` with the entries of both.
- **Example:** a.merge(b)   // { "a": 1, "b": 99, "c": 3 }   (right-side values win on conflict)

#### `pick`

```lin
val pick = <T>(obj: { String: T }, ks: String[]): { String: T }
```

Build a new map containing only the named keys that are present in `obj`.
- **`obj`** — the source map.
- **`ks`** — the keys to keep; keys absent from `obj` are skipped.
- **Returns** a new `{ String: T }` with just the selected present entries.
- **Example:** m.pick(["a", "c"])   // { "a": 1, "c": 3 }

#### `omit`

```lin
val omit = <T>(obj: { String: T }, ks: String[]): { String: T }
```

Build a new map with the named keys removed.
- **`obj`** — the source map.
- **`ks`** — the keys to drop.
- **Returns** a new `{ String: T }` with every entry of `obj` except those keyed in `ks`.
- **Example:** m.omit(["b"])   // { "a": 1, "c": 3 }

#### `mapValues`

```lin
val mapValues = <V, W>(obj: { String: V }, f: (V)
```

Transform every value of a map, keeping the keys unchanged.
- **`obj`** — the source map (value type `V`).
- **`f`** — `(value) => W` applied to each value.
- **Returns** a new `{ String: W }` with the same keys and mapped values.
- **Example:** m.mapValues(v => v * 10)   // { "a": 10, "b": 20 } : { String: Int32 }

#### `get`

```lin
val get = <T, D>(m: { String: T }, key: String, default: D = null): T | D
```

Read the value at `key`, with a fallback when the key is absent. The default's type `D` is
separate from the value type `T`, so the result is `T | D` and the default never pollutes `T`.
  - `m.get(k)`        => `T | Null`
  - `m.get(k, 0)`     => `T | Int32` (= `T` when `T = Int32`: the "definitely present" form)
  - `m.get(k, "n/a")` => `T | String`
- **`m`** — the map to read from.
- **`key`** — the key to look up.
- **`default`** — value returned when `key` is absent; defaults to `null`, pinning `D`.
- **Returns** the value at `key`, or `default` (typed `T | D`) when the key is absent.
- **Example:** counts.get("a", 0)        // 7
- **Example:** counts.get("missing", 0)  // 0
- **Example:** counts.get("z", "n/a")    // "n/a"   (independent default type -> Int32 | String)

#### `isEmpty`

```lin
val isEmpty = (x: Json): Boolean
```

Test whether an object, map, array, or string is empty.
- **`x`** — any object, map, array, or string.
- **Returns** `true` iff `x` has zero entries/elements/characters.
- **Example:** isEmpty({})        // true
- **Example:** isEmpty([])        // true
- **Example:** isEmpty({ "a": 1 })   // false

### Result/Option ergonomics (folded in from the former std/result module)

#### `error`

```lin
val error = (message: String): Error
```

std/result — ergonomics for the two fallible-value conventions: `T | Error` and `T | Null`.

Lin has NO `Result`/`Option` wrapper ADT. A fallible call returns `T | Error` (the canonical
`Error` is `{ "type":"error","message":String }`, discriminated with `is Error`, ADR-031) and an
absence-y call returns `T | Null`. The success value IS the value — no unwrap ceremony on the
happy path. This module adds small, total, side-effect-free helpers over those conventions
WITHOUT introducing a wrapper type.

SCOPE (see docs/proposals/stdlib/result-ergonomics.md). Only the helpers that are expressible in
Lin's argument-driven, monomorphized generics ship here:
  - `isOk` / `isError` / `isNull`        — Json -> Boolean predicates (no type variable to solve)
  - `unwrapOr`                           — DEFAULT-DRIVEN collapse: `T` is solved from `default`,
                                           exactly the mechanism behind `array.at`/`object.get`
  - `error`                              — the canonical-Error constructor
The map/chain/bridge family (`mapOk`/`mapError`/`andThen`/`orElse`/`okOr`/`toNull`) is BLOCKED:
it needs a union-arm-subtraction inference rule the checker does not have (it cannot bind `T` to
"the non-`Error` arm of a `T | Error` argument"). Those are intentionally NOT exported. See the
proposal's "Generics & checker constraints" section.
Construct the canonical Error value. Useful for fabricating an Error to feed a default arm, a
test, or a hand-rolled fallible function.
- **`message`** — the human-readable error message.
- **Returns** the `Error` `{ "type": "error", "message": message }`, the exact value `is Error` discriminates on (ADR-031).

#### `isError`

```lin
val isError = (x: Json): Boolean
```

Test whether `x` is the canonical Error — the `is Error` discriminant as a plain predicate, usable
in `if`/`&&` positions and combinator callbacks (e.g. `xs.filter`).
NOTE: this does NOT narrow the union — after `if isError(x)` the compiler still sees `x : T | Error`.
Use the built-in `is Error` test to unlock the success arm's fields; this is for filtering/counting/branching.
- **`x`** — any value (typed `Json`, the supertype any union flows into).
- **Returns** `true` iff `x` is the canonical Error.

#### `isOk`

```lin
val isOk = (x: Json): Boolean
```

Test whether `x` is NOT the canonical Error — the negation of `isError`. A `Null` `x` is "ok"
here (it is not an Error); use `isNull` to test absence.
- **`x`** — any value (typed `Json`).
- **Returns** `true` unless `x` is the canonical Error.

#### `isNull`

```lin
val isNull = (x: Json): Boolean
```

Test whether `x` is the `Null` value (the absence arm of a `T | Null`).
- **`x`** — any value (typed `Json`).
- **Returns** `true` iff `x` is `null`.

#### `unwrapOr`

```lin
val unwrapOr = <D>(x: Json, default: D): Json | D
```

Collapse the failure arm of a fallible value with a default. Handles BOTH conventions: the
success value is returned when `x` is neither an `Error` nor `Null`, otherwise `default`.
- **Example:** val port: Int32 = parsePort(input).unwrapOr(8080)    // T | Error -> T
- **Example:** val name: String = config["name"].unwrapOr("anon")   // T | Null  -> T
- **`x`** — any value (typed `Json`, so any `T | Error` / `T | Null` union flows in).
- **`default`** — the fallback returned for the `Error`/`Null` arms; pins `D`.
- **Returns** the success value, or `default`, typed `Json | D` (the static success type is recovered via `D`, as in `at`/`get`).
