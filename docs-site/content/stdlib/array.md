# std/array

Array-shaped operations — these work on a materialised, indexable, ordered array. All transformation functions are non-mutating and return new values, except `push` and `set` which mutate in place.

The iterable **combinators** (`map`, `filter`, `reduce`, `for`, `while`, `take`, `drop`, `find`, `some`, `every`, `flatMap`, `takeWhile`, `dropWhile`, `flatten`, `concat`) and the iterator **constructors** (`range`, `rangeStep`, `iter`, `iterOf`) now live in [`std/iter`](/stdlib/iter.html), where they dispatch on the receiver type (eager over arrays/iterators, lazy over streams). Import those from `std/iter`, not `std/array`.

```lin
import { sort, sortBy, length, push, slice, sum } from "std/array"
```

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `append` | `(Json[], Json) -> Json[]` | Non-mutating single-element append |
| `arrayAllocate` | `(Int32) -> Json[]` | Allocate an array of n nulls |
| `arrayAllocateFilled` | `(Int32, Json) -> Json[]` | Allocate an array of n copies of a fill value |
| `at` | `(Json[], Int32) -> Json` | Element at index; negative from end |
| `chunk` | `(Json[], Int32) -> Json[][]` | Split into n-sized sub-arrays |
| `compact` | `(Json[]) -> Json[]` | Remove null elements |
| `countBy` | `(Json[], (Json) -> String) -> {}` | Frequency map by key function |
| `groupBy` | `(Json[], (Json) -> String) -> {}` | Group into object of arrays |
| `indexOf` | `(Json[], Json) -> Int32` | First index of value, or -1 |
| `length` | `(Json) -> Int32` | Length of array, string, or object |
| `max` | `(Number[]) -> Number` | Maximum element |
| `maxBy` | `(Json[], (Json) -> Number) -> Json` | Element with largest key |
| `min` | `(Number[]) -> Number` | Minimum element |
| `minBy` | `(Json[], (Json) -> Number) -> Json` | Element with smallest key |
| `partition` | `(Json[], (Json) -> Boolean) -> [Json[], Json[]]` | Split into passing and failing |
| `prepend` | `(Json[], Json) -> Json[]` | Non-mutating prepend |
| `product` | `(Number[]) -> Number` | Product of all elements |
| `push` | `(Json[], Json) -> Null` | Append in place (mutating) |
| `reverse` | `(Json[]) -> Json[]` | Reversed copy |
| `scan` | `(Json[], Json, (Json, Json) -> Json) -> Json[]` | Reduce returning all intermediate values |
| `set` | `(Json[], Int32, Json) -> Null` | Set an element by index in place (mutating) |
| `slice` | `(T[], Int32, Int32) -> T[]` | Copy of `[start, end)`; preserves element type |
| `sort` | `(Json[], (Json, Json) -> Int32) -> Json[]` | Sort with comparator |
| `sortBy` | `(Json[], (Json) -> Json) -> Json[]` | Sort by key extractor |
| `sum` | `(Number[]) -> Number` | Sum all elements |
| `unique` | `(Json[]) -> Json[]` | Remove duplicates |
| `zip` | `(Json[], Json[]) -> [Json, Json][]` | Pair elements by index |

> The combinators `map`, `filter`, `reduce`, `for`, `while`, `take`, `drop`, `find`, `some`, `every`, `flatMap`, `takeWhile`, `dropWhile`, `flatten`, `concat` and the iterator constructors `range`, `rangeStep`, `iter`, `iterOf` are documented in [`std/iter`](/stdlib/iter.html).

---

### `sort` / `sortBy`

```lin
[3, 1, 4, 1, 5].sort((a, b) => a - b)   // [1, 1, 3, 4, 5]
people.sortBy(p => p["name"])
```

---

### `push`

Mutates the array in place:

```lin
val xs = []
xs.push(1)
xs.push(2)
// xs: [1, 2]
```

---

### `length`

```lin
length([1, 2, 3])     // 3
length("hello")       // 5
length({ "a": 1 })    // 1
```
