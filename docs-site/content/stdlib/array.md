# std/array

Array-shaped operations — these work on a materialised, indexable, ordered array. All transformation functions are non-mutating and return new values, except `push` and `set` which mutate in place.

The iterable **combinators** (`map`, `filter`, `reduce`, `for`, `while`, `take`, `drop`, `find`, `some`, `every`, `flatMap`, `takeWhile`, `dropWhile`, `flatten`, `concat`) and the iterator **constructors** (`range`, `rangeStep`, `iter`, `iterOf`) now live in [`std/iter`](/stdlib/iter.html), where they dispatch on the receiver type (eager over arrays/iterators, lazy over streams). Import those from `std/iter`, not `std/array`.

```lin
import { sort, sortBy, length, push, slice, sum } from "std/array"
```

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `append` | `<T>(T[], T) -> T[]` | Non-mutating single-element append |
| `arrayAllocate` | `(Int32) -> Json[]` | Allocate an array of n nulls |
| `arrayAllocateFilled` | `(Int32, Json) -> Json[]` | Allocate an array of n copies of a fill value |
| `at` | `<T>(T[], Int32) -> T \| Null` | Element at index, or null if out of bounds; negative counts from end |
| `atOr` | `<T>(T[], Int32, T) -> T` | Element at index, or a default when out of bounds; returns a bare `T` |
| `chunk` | `<T>(T[], Int32) -> T[][]` | Split into n-sized sub-arrays |
| `compact` | `(Json[]) -> Json[]` | Remove null elements |
| `countBy` | `<T>(T[], (T) -> String) -> { String: Int32 }` | Frequency map by key function |
| `groupBy` | `<T>(T[], (T) -> String) -> { String: T[] }` | Group into a typed map of arrays |
| `indexOf` | `<T>(T[], T, Int32 = 0) -> Int32` | First index of value at or after `fromIndex`, or -1 (negative counts from end) |
| `length` | `(Json) -> Int32` | Length of array, string, or object |
| `max` | `(Number[]) -> Number` | Maximum element |
| `maxBy` | `(Json[], (Json) -> Number) -> Json` | Element with largest key |
| `min` | `(Number[]) -> Number` | Minimum element |
| `minBy` | `(Json[], (Json) -> Number) -> Json` | Element with smallest key |
| `partition` | `<T>(T[], (T[, i: Int32]) -> Boolean) -> T[][]` | Split into passing and failing (`result[0]` pass, `result[1]` fail; predicate gets an optional source index) |
| `prepend` | `<T>(T[], T) -> T[]` | Non-mutating prepend |
| `product` | `(Number[]) -> Number` | Product of all elements |
| `push` | `<T>(T[], T) -> Null` | Append in place (mutating); element type enforced |
| `reverse` | `<T>(T[]) -> T[]` | Reversed copy |
| `scan` | `<T, U>(T[], U, (U, T) -> U) -> U[]` | Reduce returning all intermediate values |
| `set` | `(Json[], Int32, Json) -> Null` | Set an element by index in place (mutating) |
| `slice` | `(T[], Int32, Int32 = length(arr)) -> T[]` | Copy of `[start, end)`; end defaults to length, negatives count from end; preserves element type |
| `sort` | `(Json[], (Json, Json) -> Int32) -> Json[]` | Sort with comparator |
| `sortBy` | `(Json[], (Json) -> Json) -> Json[]` | Sort by key extractor |
| `sum` | `(Number[]) -> Number` | Sum all elements |
| `unique` | `<T>(T[]) -> T[]` | Remove duplicates |
| `zip` | `<A, B>(A[], B[]) -> [A, B][]` | Pair elements by index |

> The combinators `map`, `filter`, `reduce`, `for`, `while`, `take`, `drop`, `find`, `some`, `every`, `flatMap`, `takeWhile`, `dropWhile`, `flatten`, `concat` and the iterator constructors `range`, `rangeStep`, `iter`, `iterOf` are documented in [`std/iter`](/stdlib/iter.html).

---

### `sort` / `sortBy`

```lin
[3, 1, 4, 1, 5].sort((a, b) => a - b)   // [1, 1, 3, 4, 5]
people.sortBy(p => p["name"])
```

---

### `push`

Mutates the array in place. Generic (`<T>(arr: T[], item: T)`), so the element type is enforced —
`push(intArr, "s")` is a compile error (ADR-085). An empty accumulator literal needs a type
annotation so `T` is pinned — an evidence-free `[]` cannot infer its element type (ADR-084):

```lin
val xs: Int32[] = []
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

---

### `at`

Safe accessor: returns the element at `index`, or `null` if the resolved index is out of bounds (it never traps), widening the return type to `T | Null`. A negative index counts from the end.

```lin
at([10, 20, 30], 0)    // 10
at([10, 20, 30], -1)   // 30
at([], 0)              // null
```

---

### `atOr`

Defaulted bounds-safe accessor: returns the element at `index`, or `default` when the resolved index is out of bounds (negative indices count from the end, like `at`). Unlike `at`, the result is a bare `T` — usable directly in arithmetic with no `null` guard. It is a separate function because a generic `T` has no spellable default expression for a `default`-arg form of `at` (`null` would be `T | Null`).

```lin
[10, 20, 30].atOr(1, -1)    // 20
[10, 20, 30].atOr(5, -1)    // -1   (out of bounds -> default)
[10, 20, 30].atOr(-1, -1)   // 30   (negative wraps)
[10, 20, 30].atOr(-9, 99)   // 99   (out-of-range negative -> default)
```

---

### `slice`

`end` is optional and defaults to the array length, so `slice(arr, start)` returns the elements from `start` to the end. Negative `start`/`end` count from the end.

```lin
[10, 20, 30, 40, 50].slice(1, 4)   // [20, 30, 40]
[1, 2, 3, 4, 5].slice(1)           // [2, 3, 4, 5]
[1, 2, 3, 4, 5].slice(1, -1)       // [2, 3, 4]
[1, 2, 3, 4, 5].slice(-2)          // [4, 5]
```

---

### `indexOf`

Returns the index of the first element deeply equal to `target` at or after `fromIndex`, or `-1`. `fromIndex` is optional and defaults to `0`; a negative `fromIndex` counts from the end.

```lin
[10, 20, 30].indexOf(20)     // 1
[1, 2, 1, 2].indexOf(2, 2)   // 3
[1, 2, 1, 2].indexOf(1, -1)  // -1   (search starts at index 3)
```

---

### `partition`

Splits into `[passing, failing]`. The predicate optionally receives the element's 0-based source index as a second argument.

```lin
val [evens, odds] = [1, 2, 3, 4, 5].partition(x => x % 2 == 0)
// evens: [2, 4],  odds: [1, 3, 5]
```
