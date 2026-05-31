# std/array

Array and iterator manipulation. All transformation functions are non-mutating and return new values, except `push` which mutates in place.

```lin
import { map, filter, reduce, for, range, find, some, every, sort, sortBy, length, push } from "std/array"
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
| `concat` | `(Json[], Json[]) -> Json[]` | Concatenate two arrays |
| `countBy` | `(Json[], (Json) -> String) -> {}` | Frequency map by key function |
| `drop` | `(Json[], Int32) -> Json[]` | All elements after first n |
| `dropWhile` | `(Json[], (Json) -> Boolean) -> Json[]` | Skip elements while predicate holds |
| `every` | `(Json[], (Json) -> Boolean) -> Boolean` | True if all elements match |
| `filter` | `(Json[], (Json) -> Boolean) -> Json[]` | Keep matching elements |
| `find` | `(Json[], (Json) -> Boolean) -> Json` | First matching element or null |
| `flatMap` | `(Json[], (Json) -> Json[]) -> Json[]` | Map then flatten one level |
| `flatten` | `(Json[]) -> Json[]` | Flatten one level of nesting |
| `for` | `(Iterable, (Json) -> Json) -> Null` | Iterate over array or iterator |
| `groupBy` | `(Json[], (Json) -> String) -> {}` | Group into object of arrays |
| `indexOf` | `(Json[], Json) -> Int32` | First index of value, or -1 |
| `iter` | `(()->S, (S)->Boolean, (S)->S, (S)->T) -> Iterator` | Custom iterator |
| `iterOf` | `(Json[]) -> Iterator` | Iterator over an array |
| `length` | `(Json) -> Int32` | Length of array, string, or object |
| `map` | `(Json[], (Json) -> Json) -> Json[]` | Transform each element |
| `max` | `(Number[]) -> Number` | Maximum element |
| `maxBy` | `(Json[], (Json) -> Number) -> Json` | Element with largest key |
| `min` | `(Number[]) -> Number` | Minimum element |
| `minBy` | `(Json[], (Json) -> Number) -> Json` | Element with smallest key |
| `partition` | `(Json[], (Json) -> Boolean) -> [Json[], Json[]]` | Split into passing and failing |
| `prepend` | `(Json[], Json) -> Json[]` | Non-mutating prepend |
| `push` | `(Json[], Json) -> Null` | Append in place (mutating) |
| `range` | `(Int32, Int32) -> Iterator` | Integer range `[start, end)` |
| `rangeStep` | `(Int32, Int32, Int32) -> Iterator` | Integer range with an explicit (possibly negative) step |
| `reduce` | `(Json[], Json, (Json, Json) -> Json) -> Json` | Fold left |
| `reverse` | `(Json[]) -> Json[]` | Reversed copy |
| `scan` | `(Json[], Json, (Json, Json) -> Json) -> Json[]` | Reduce returning all intermediate values |
| `set` | `(Json[], Int32, Json) -> Null` | Set an element by index in place (mutating) |
| `slice` | `(T[], Int32, Int32) -> T[]` | Copy of `[start, end)`; preserves element type |
| `some` | `(Json[], (Json) -> Boolean) -> Boolean` | True if any element matches |
| `sort` | `(Json[], (Json, Json) -> Int32) -> Json[]` | Sort with comparator |
| `sortBy` | `(Json[], (Json) -> Json) -> Json[]` | Sort by key extractor |
| `sum` | `(Number[]) -> Number` | Sum all elements |
| `take` | `(Json[], Int32) -> Json[]` | First n elements |
| `takeWhile` | `(Json[], (Json) -> Boolean) -> Json[]` | Elements until predicate fails |
| `unique` | `(Json[]) -> Json[]` | Remove duplicates |
| `while` | `(Json[], (Json) -> Boolean) -> Null` | Iterate, stopping when callback returns false |
| `zip` | `(Json[], Json[]) -> [Json, Json][]` | Pair elements by index |

---

### `map`

```lin
[1, 2, 3].map(x => x * 2)        // [2, 4, 6]
["a", "b"].map(s => toUpper(s))   // ["A", "B"]
```

---

### `filter`

```lin
[1, 2, 3, 4].filter(x => x % 2 == 0)   // [2, 4]
```

---

### `reduce`

Accumulator is the first argument to the combining function:

```lin
[1, 2, 3, 4].reduce(0, (acc, x) => acc + x)   // 10
```

---

### `for`

```lin
[1, 2, 3].for(x => print(x))
range(0, 5).for(i => print(i))
```

---

### `range`

```lin
range(0, 5).for(i => print(i))    // 0 1 2 3 4
range(1, 6).map(i => i * i)      // [1, 4, 9, 16, 25]
```

---

### `find`

```lin
[1, 3, 5, 6].find(x => x % 2 == 0)   // 6
[1, 3, 5].find(x => x % 2 == 0)      // null
```

---

### `some` / `every`

```lin
[1, 2, 3].some(x => x > 2)    // true
[1, 2, 3].every(x => x > 0)   // true
```

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
