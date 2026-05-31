# std/hash

Structural hashing for any JSON value. `hash` returns a canonical, type-tagged string key that is stable and matches Lin's structural equality (spec §14). Use it to deduplicate values or to index them by structural identity — for example as object keys in a hand-rolled set or map.

```lin
import { hash } from "std/hash"
```

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `hash` | `(Json) -> String` | Canonical, type-tagged hash key for any value |

---

### `hash`

The key carries a type tag, so values of different types never collide: `hash(42)` is `"i:42"`, while `hash("42")` is `"s:42"`. Equal values hash equal, objects hash independently of key order, and arrays hash order-sensitively.

```lin
hash(null)        // "N"
hash(true)        // "b:true"
hash(42)          // "i:42"
hash("hello")     // "s:hello"

hash([1, 2, 3]) == hash([1, 2, 3])   // true
hash([1, 2]) == hash([2, 1])         // false

// Objects are order-independent (like structural equality):
hash({ "x": 1, "y": 2 }) == hash({ "y": 2, "x": 1 })   // true

// Different types never collide:
hash(42) == hash("42")   // false
```

---

### Indexing by structural identity

Because equal values share a key, `hash` gives you a stable string to index by. Different field orders that are structurally equal collapse to the same bucket when grouped with `std/array`'s `countBy`:

```lin
import { hash } from "std/hash"
import { countBy } from "std/array"

val points = [{ "x": 1, "y": 2 }, { "y": 2, "x": 1 }, { "x": 3, "y": 4 }]

countBy(points, p => hash(p))
// the first two points share a key, so that bucket has count 2
```
