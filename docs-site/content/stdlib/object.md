# std/object

std/object — object introspection and transformation, plus Result/Option ergonomics.

```lin
import { keys, values, entries, fromEntries, get, merge, pick, omit, mapValues, isEmpty } from "std/object"
```

`keys`/`values`/`entries` are tag-aware — they work on both a plain `{}`/`AnyVal` record and a typed
index-signature map `{ String: T }`. `merge`/`pick`/`omit`/`mapValues` are generic over
`{ String: T }` and return a typed map; pass them a value annotated `{ String: T }` (there is no
implicit `AnyVal -> { String: T }` coercion). Over a typed map, key order is hash order, not insertion
order. `get` is the idiomatic defaulted read (`m[k] ?? default`) — a bare `m[k]` already yields
`T | Null`, so the helper folds in a fallback whose type `D` is independent of the value type `T`.

This module also carries the Result/Option ergonomics folded in from the former std/result
(error/isOk/isError/isNull/unwrapOr) — see the section lower in the file. Lin has no
`Result`/`Option` wrapper type: a fallible call returns `T | Error`, an absence-y call returns
`T | Null`, and the success value is the value with no unwrap ceremony on the happy path.

## Reference

#### `keys`

```lin
val keys = (obj: { String: AnyVal } | {  }): String[]
```

`keys`/`values`/`entries` are tag-aware at runtime, so one function serves both a record
(`{}`/`{ String: T }`, insertion order) and an index-signature map (hash order). The parameter is
the union `{ String: AnyVal } | {}` — accepting a record literal, a typed map, or a `AnyVal` value
(any of which may carry an object), while REJECTING a non-object argument (`keys(1)`, `keys("s")`,
`keys([…])` are compile errors). A `AnyVal` holding a non-object yields an empty result at runtime.

Return the keys of an object or map.
- **`obj`** — a record, an index-signature map, or a `AnyVal` object.
- **Returns** a `String[]` of the keys (insertion order for a record, hash order for a map).

**Example:**

```lin
keys({ "a": 1, "b": 2 })   // ["a", "b"]
```

#### `values`

```lin
val values = (obj: { String: AnyVal } | {  }): AnyVal[]
```

Return the values of an object or map.
- **`obj`** — a record, an index-signature map, or a `AnyVal` object.
- **Returns** a `AnyVal[]` of the values, in key order.

**Example:**

```lin
values({ "a": 1, "b": 2 })   // [1, 2]
```

#### `entries`

```lin
val entries = (obj: { String: AnyVal } | {  }): AnyVal[]
```

Return the key/value pairs of an object or map.
- **`obj`** — a record, an index-signature map, or a `AnyVal` object.
- **Returns** a `AnyVal[]` of `[key, value]` pairs, in key order.

**Example:**

```lin
entries({ "a": 1, "b": 2 })   // [["a", 1], ["b", 2]]
```

#### `fromEntries`

```lin
val fromEntries = <T>(pairs: [String, T][]): { String: T }
```

Build an object from a list of `[key, value]` pairs (the inverse of `entries`). Later pairs with
the same key overwrite earlier ones.
- **`pairs`** — an array of `[String, T]` pairs.
- **Returns** a `{ String: T }` map from each key to its value.

**Example:**

```lin
fromEntries([["a", 1], ["b", 2]])   // { "a": 1, "b": 2 }
```

**Example:**

```lin
entries(obj).map(([k, v]) => [k, v * 2]).fromEntries()   // double every value
```

#### `merge`

```lin
val merge = <T>(a: { String: T }, b: { String: T }): { String: T }
```

Merge two maps into a new one. Right-biased: keys present in `b` win over those in `a`.
- **`a`** — the base map.
- **`b`** — the overriding map; its entries take precedence on key collisions.
- **Returns** a new `{ String: T }` with the entries of both.

**Example:**

```lin
a.merge(b)   // { "a": 1, "b": 99, "c": 3 }   (right-side values win on conflict)
```

#### `pick`

```lin
val pick = <T>(obj: { String: T }, ks: String[]): { String: T }
```

Build a new map containing only the named keys that are present in `obj`.
- **`obj`** — the source map.
- **`ks`** — the keys to keep; keys absent from `obj` are skipped.
- **Returns** a new `{ String: T }` with just the selected present entries.

**Example:**

```lin
m.pick(["a", "c"])   // { "a": 1, "c": 3 }
```

#### `omit`

```lin
val omit = <T>(obj: { String: T }, ks: String[]): { String: T }
```

Build a new map with the named keys removed.
- **`obj`** — the source map.
- **`ks`** — the keys to drop.
- **Returns** a new `{ String: T }` with every entry of `obj` except those keyed in `ks`.

**Example:**

```lin
m.omit(["b"])   // { "a": 1, "c": 3 }
```

#### `mapValues`

```lin
val mapValues = <V, W>(obj: { String: V }, f: (V) => W): { String: W }
```

Transform every value of a map, keeping the keys unchanged.
- **`obj`** — the source map (value type `V`).
- **`f`** — `(value) => W` applied to each value.
- **Returns** a new `{ String: W }` with the same keys and mapped values.

**Example:**

```lin
m.mapValues(v => v * 10)   // { "a": 10, "b": 20 } : { String: Int32 }
```

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

**Example:**

```lin
counts.get("a", 0)        // 7
```

**Example:**

```lin
counts.get("missing", 0)  // 0
```

**Example:**

```lin
counts.get("z", "n/a")    // "n/a"   (independent default type -> Int32 | String)
```

#### `isEmpty`

```lin
val isEmpty = (x: AnyVal): Boolean
```

Test whether an object, map, array, or string is empty.
- **`x`** — any object, map, array, or string.
- **Returns** `true` iff `x` has zero entries/elements/characters.

**Example:**

```lin
isEmpty({})        // true
```

**Example:**

```lin
isEmpty([])        // true
```

**Example:**

```lin
isEmpty({ "a": 1 })   // false
```

#### `error`

```lin
val error = (message: String): Error
```

Result/Option ergonomics for the two fallible-value conventions: `T | Error` and `T | Null`.

Lin has no `Result`/`Option` wrapper type. A fallible call returns `T | Error` (the canonical
`Error` is `{ "type":"error","message":String }`, discriminated with `is Error`) and an absence-y
call returns `T | Null`. The success value is the value — no unwrap ceremony on the happy path.
This module adds small, total, side-effect-free helpers over those conventions: the `isOk` /
`isError` / `isNull` predicates, the default-driven `unwrapOr` collapse, and the `error`
constructor below.

Construct the canonical Error value. Useful for fabricating an Error to feed a default arm, a
test, or a hand-rolled fallible function.
- **`message`** — the human-readable error message.
- **Returns** the `Error` `{ "type": "error", "message": message }`, the exact value `is Error` discriminates on.

#### `isError`

```lin
val isError = (x: AnyVal): Boolean
```

Test whether `x` is the canonical Error — the `is Error` discriminant as a plain predicate, usable
in `if`/`&&` positions and combinator callbacks (e.g. `xs.filter`).
Note: this does not narrow the union — after `if isError(x)` the compiler still sees `x : T | Error`.
Use the built-in `is Error` test to unlock the success arm's fields; this is for filtering/counting/branching.
- **`x`** — any value (typed `AnyVal`, the supertype any union flows into).
- **Returns** `true` iff `x` is the canonical Error.

#### `isOk`

```lin
val isOk = (x: AnyVal): Boolean
```

Test whether `x` is not the canonical Error — the negation of `isError`. A `Null` `x` is "ok"
here (it is not an Error); use `isNull` to test absence.
- **`x`** — any value (typed `AnyVal`).
- **Returns** `true` unless `x` is the canonical Error.

#### `isNull`

```lin
val isNull = (x: AnyVal): Boolean
```

Test whether `x` is the `Null` value (the absence arm of a `T | Null`).
- **`x`** — any value (typed `AnyVal`).
- **Returns** `true` iff `x` is `null`.

#### `unwrapOr`

```lin
val unwrapOr = <D>(x: AnyVal, default: D): AnyVal | D
```

Collapse the failure arm of a fallible value with a default. Handles both conventions: the
success value is returned when `x` is neither an `Error` nor `Null`, otherwise `default`.

**Example:**

```lin
val port: Int32 = parsePort(input).unwrapOr(8080)    // T | Error -> T
```

**Example:**

```lin
val name: String = config["name"].unwrapOr("anon")   // T | Null  -> T
```
- **`x`** — any value (typed `AnyVal`, so any `T | Error` / `T | Null` union flows in).
- **`default`** — the fallback returned for the `Error`/`Null` arms; pins `D`.
- **Returns** the success value, or `default`, typed `AnyVal | D` (the static success type is recovered via `D`, as in `at`/`get`).
