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
| `at` | `<T, D>(T[], Int32, D = null) -> T \| D` | Element at index, or the default (`null` if omitted) when out of bounds; negative counts from end |
| `chunk` | `<T>(T[], Int32) -> T[][]` | Split into n-sized sub-arrays |
| `compact` | `(Json[]) -> Json[]` | Remove null elements |
| `countBy` | `<T>(T[], (T) -> String) -> { String: Int32 }` | Frequency map by key function |
| `groupBy` | `<T>(T[], (T) -> String) -> { String: T[] }` | Group into a typed map of arrays |
| `indexOf` | `<T>(T[], T, Int32 = 0) -> Int32` | First index of value at or after `fromIndex`, or -1 (negative counts from end) |
| `length` | `(Json) -> Int32` | Length of array, string, or object |
| `max` | `(Number[]) -> Number` | Maximum element |
| `maxBy` | `<T>(T[], (T) -> Number) -> T` | Element with largest key |
| `min` | `(Number[]) -> Number` | Minimum element |
| `minBy` | `<T>(T[], (T) -> Number) -> T` | Element with smallest key |
| `partition` | `<T>(T[], (T[, i: Int32]) -> Boolean) -> T[][]` | Split into passing and failing (`result[0]` pass, `result[1]` fail; predicate gets an optional source index) |
| `prepend` | `<T>(T[], T) -> T[]` | Non-mutating prepend |
| `product` | `(Number[]) -> Number` | Product of all elements |
| `push` | `<T>(T[], T) -> Null` | Append in place (mutating); element type enforced |
| `reverse` | `<T>(T[]) -> T[]` | Reversed copy |
| `scan` | `<T, U>(T[], U, (U, T) -> U) -> U[]` | Reduce returning all intermediate values |
| `set` | `(Json[], Int32, Json) -> Null` | Set an element by index in place (mutating) |
| `slice` | `(T[], Int32, Int32 = length(arr)) -> T[]` | Copy of `[start, end)`; end defaults to length, negatives count from end; preserves element type |
| `sort` | `<T>(T[], (T, T) -> Int32) -> T[]` | Stable sort with comparator; element type enforced |
| `sortBy` | `<T>(T[], (T) -> Json) -> T[]` | Stable sort by key extractor; element type enforced |
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
`push(intArr, "s")` is a compile error (ADR-059). An empty accumulator literal needs a type
annotation so `T` is pinned — an evidence-free `[]` cannot infer its element type (ADR-058):

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

Safe accessor with an optional default: returns the element at `index`, or `default` when the resolved index is out of bounds (it never traps). A negative index counts from the end.

The default's type `D` is an **independent** type parameter, so the result is `T | D` and the default's type never pollutes the element type `T`:

- omitting the default gives `default = null`, so `at(arr, i)` is `T | Null` — the safe bounds-checked read;
- a same-typed default collapses the union: over an `Int32[]`, `at(arr, i, 0)` is `Int32 | Int32 = Int32`, a bare scalar usable directly in arithmetic with no `null` guard;
- a differently-typed default keeps both arms: `at(arr, i, "n/a")` over an `Int32[]` is `Int32 | String`.

This one function subsumes the old `at`/`atOr` pair.

```lin
at([10, 20, 30], 0)         // 10
at([10, 20, 30], -1)        // 30
at([], 0)                   // null               (omitted default -> T | Null)
[10, 20, 30].at(1, -1)      // 20
[10, 20, 30].at(5, -1)      // -1   (out of bounds -> default)
[10, 20, 30].at(-1, -1)     // 30   (negative wraps)
[10, 20, 30].at(-9, 99)     // 99   (out-of-range negative -> default)
[10, 20, 30].at(9, "n/a")   // "n/a"               (independent default type -> Int32 | String)
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
