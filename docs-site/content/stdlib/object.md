# std/object

Object introspection and transformation functions.

```lin
import { keys, values, entries, fromEntries, merge, pick, omit, mapValues, isEmpty } from "std/object"
```

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `entries` | `(Json) -> [String, Json][]` | Array of `[key, value]` pairs |
| `fromEntries` | `([String, Json][]) -> {}` | Build object from key-value pairs |
| `isEmpty` | `(Json) -> Boolean` | True if object, array, or string is empty |
| `keys` | `(Json) -> String[]` | Array of object keys |
| `mapValues` | `({}, (Json) -> Json) -> {}` | Transform values, keep keys |
| `merge` | `({}, {}) -> {}` | Shallow-merge (right wins on conflict) |
| `omit` | `({}, String[]) -> {}` | Return object without specified keys |
| `pick` | `({}, String[]) -> {}` | Return object with only specified keys |
| `values` | `(Json) -> Json[]` | Array of object values |

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

### `merge`

```lin
merge({ "a": 1, "b": 2 }, { "b": 99, "c": 3 })
// { "a": 1, "b": 99, "c": 3 }
```

Right-side values win on conflict.

---

### `pick`

```lin
pick({ "a": 1, "b": 2, "c": 3 }, ["a", "c"])
// { "a": 1, "c": 3 }
```

---

### `omit`

```lin
omit({ "a": 1, "b": 2, "c": 3 }, ["b"])
// { "a": 1, "c": 3 }
```

---

### `mapValues`

```lin
mapValues({ "a": 1, "b": 2 }, v => v * 10)
// { "a": 10, "b": 20 }
```

---

### `isEmpty`

```lin
isEmpty({})     // true
isEmpty([])     // true
isEmpty("")     // true
isEmpty({ "a": 1 })  // false
```
