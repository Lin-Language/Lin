# std/object

Object introspection and transformation functions.

```lin
import { keys, values, entries, fromEntries, get, merge, pick, omit, mapValues, isEmpty } from "std/object"
```

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `entries` | `(Json) -> [String, Json][]` | Array of `[key, value]` pairs (object or typed map) |
| `fromEntries` | `([String, Json][]) -> {}` | Build object from key-value pairs |
| `get` | `<T, D>({ String: T }, String, D = null) -> T \| D` | Value at key, or the default (`null` if omitted) when absent (`m[k] ?? default`) |
| `isEmpty` | `(Json) -> Boolean` | True if object, array, or string is empty |
| `keys` | `(Json) -> String[]` | Array of object keys (object or typed map) |
| `mapValues` | `<V,W>({ String: V }, (V) -> W) -> { String: W }` | Transform values, keep keys |
| `merge` | `<T>({ String: T }, { String: T }) -> { String: T }` | Shallow-merge typed maps (right wins on conflict) |
| `omit` | `<T>({ String: T }, String[]) -> { String: T }` | Return typed map without specified keys |
| `pick` | `<T>({ String: T }, String[]) -> { String: T }` | Return typed map with only specified keys |
| `values` | `(Json) -> Json[]` | Array of object values (object or typed map) |

`keys`/`values`/`entries` are tag-aware — they work on both a plain `{}`/`Json` record and a typed
index-signature map `{ String: T }` (ADR-082). `merge`/`pick`/`omit`/`mapValues` are generic over
`{ String: T }` and *return* a typed map; pass them a value annotated `{ String: T }` (there is no
implicit `Json -> { String: T }` coercion). Over a typed map, key order is hash order, not insertion
order. `get` is the idiomatic *defaulted* read (`m[k] ?? default`) — a bare `m[k]` already yields
`T | Null`, so the named helper folds in a fallback whose type `D` is independent of the value type
`T` (result `T | D`); omitting the default gives back `T | Null`.

---

### `keys`

```lin
keys({ "a": 1, "b": 2 })   // ["a", "b"]
```

---

### `values`

```lin
values({ "a": 1, "b": 2 })   // [1, 2]
```

---

### `entries`

```lin
entries({ "a": 1, "b": 2 })   // [["a", 1], ["b", 2]]
```

---

### `fromEntries`

```lin
fromEntries([["a", 1], ["b", 2]])   // { "a": 1, "b": 2 }
```

Inverse of `entries`. Transform all values then reconstruct:

```lin
entries(obj)
  .map(([k, v]) => [k, v * 2])
  .fromEntries()
```

---

### `get`

Defaulted read over a typed `{ String: T }` map — the value at `key`, or `default` when the key is
absent. The default's type `D` is an **independent** type parameter (mirroring `array.at`), so the
result is `T | D` and the default never pollutes the value type `T`:

- omitting the default gives `default = null`, so `get(m, k)` is `T | Null` — the same as `m[k]`;
- a same-typed default collapses the union: over a `{ String: Int32 }` map, `get(m, k, 0)` is
  `Int32 | Int32 = Int32`, a bare scalar usable directly in arithmetic;
- a differently-typed default keeps both arms: `get(m, k, "n/a")` is `Int32 | String`.

```lin
val counts: { String: Int32 } = { "a": 7 }

counts.get("a", 0)               // 7
counts.get("missing", 0)         // 0
val present: Int32 = counts.get("a", 0)
present + 1                      // 8   (bare Int32, usable in arithmetic)
counts.get("z", "n/a")           // "n/a"   (independent default type -> Int32 | String)
```

---

### `merge`

```lin
val a: { String: Int32 } = { "a": 1, "b": 2 }
val b: { String: Int32 } = { "b": 99, "c": 3 }
a.merge(b)
// { "a": 1, "b": 99, "c": 3 }
```

Right-side values win on conflict.

---

### `pick`

```lin
val m: { String: Int32 } = { "a": 1, "b": 2, "c": 3 }
m.pick(["a", "c"])
// { "a": 1, "c": 3 }
```

---

### `omit`

```lin
val m: { String: Int32 } = { "a": 1, "b": 2, "c": 3 }
m.omit(["b"])
// { "a": 1, "c": 3 }
```

---

### `mapValues`

```lin
val m: { String: Int32 } = { "a": 1, "b": 2 }
m.mapValues(v => v * 10)
// { "a": 10, "b": 20 } : { String: Int32 }
```

---

### `isEmpty`

```lin
isEmpty({})     // true
isEmpty([])     // true
isEmpty("")     // true
isEmpty({ "a": 1 })  // false
```
