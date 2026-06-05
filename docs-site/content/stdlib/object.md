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
| `get` | `<T>({ String: T }, String, T) -> T` | Value at key, or a default when absent (`m[k] ?? default`) |
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
`T | Null`, so only the defaulted form (which returns a bare `T`) is a named helper.

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
absent. Returns a bare `T`, so the result needs no `null` guard.

```lin
val counts: { String: Int32 } = { "a": 7 }

counts.get("a", 0)         // 7
counts.get("missing", 0)   // 0
counts.get("a", 0) + 1     // 8
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
