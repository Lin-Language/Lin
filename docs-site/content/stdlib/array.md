# std/array

std/array — array-shaped operations over a materialised, indexable, ordered array.

All transformation functions are non-mutating and return new values, except `push` and `set`,
which mutate in place. This module also carries the sorted-array search layer (binarySearch,
lowerBound/upperBound, insertSorted, countInRange) and the keyed aggregators (sortBy, minBy,
maxBy, groupBy, countBy).

import { sort, sortBy, length, push, slice, sum } from "std/array"

The iterable combinators (map, filter, reduce, for, while, take, drop, find, some, every,
flatMap, takeWhile, dropWhile, flatten, concat) and the iterator constructors (range, rangeStep,
iter, iterOf) live in std/iter, where they dispatch on the receiver type (eager over
arrays/iterators, lazy over streams). Import those from std/iter, not std/array.

## Reference

#### `push`

```lin
val push = <T>(arr: T[], item: T): Null
```

Append `item` to the end of `arr`, mutating it in place. The primitive for building up an
accumulator (`val acc: Int32[] = []; xs.for(x => acc.push(x))`).
- **`arr`** — the array to mutate; its element type pins `T` for element checking.
- **`item`** — the element to append; must be assignable to `T` (`push(intArr, "s")` is a compile error).
- **Returns** `null`.
- **Example:** val xs: Int32[] = []; xs.push(1); xs.push(2)   // xs: [1, 2]

#### `slice`

```lin
val slice = <T>(arr: T[], start: Int32, end: Int32 = length(arr)): T[]
```

Copy the sub-range `[start, end)` into a new array, preserving the element type (a `UInt8[]`
yields a `UInt8[]`, an `Int32[]` an `Int32[]`, a `Json[]` a `Json[]`).
- **`arr`** — the source array.
- **`start`** — start index (inclusive); negative counts from the end; clamped to `[0, length]`.
- **`end`** — end index (exclusive); defaults to `length(arr)`; negative counts from the end; clamped to `[0, length]`.
- **Returns** a new `T[]` holding the selected elements.
- **Example:** [10, 20, 30, 40, 50].slice(1, 4)   // [20, 30, 40]
- **Example:** [1, 2, 3, 4, 5].slice(1)           // [2, 3, 4, 5]  (omitted end -> length)
- **Example:** [1, 2, 3, 4, 5].slice(1, -1)       // [2, 3, 4]
- **Example:** [1, 2, 3, 4, 5].slice(-2)          // [4, 5]

#### `arrayAllocate`

```lin
val arrayAllocate = (n: Int32): Json
```

Allocate an uninitialised length-`n` array (boxed/tagged `Json`, so untyped use needs no
annotation). For a flat unboxed scalar array, use `arrayAllocateFilled` instead.
- **`n`** — the length of the array to allocate.
- **Returns** an uninitialised `Json` array of length `n`.

#### `arrayAllocateFilled`

```lin
val arrayAllocateFilled = <T>(n: Int32, fill: T): T[]
```

Allocate a length-`n` array with every slot set to `fill`. The element type is inferred from
`fill`, so codegen allocates a FLAT unboxed array for a concrete scalar (`Int32[]`, `Float64[]`,
…) and a tagged array otherwise; untyped use (`arrayAllocateFilled(n, 0)`) needs no annotation.
- **`n`** — the length of the array to allocate.
- **`fill`** — the value placed in every slot; pins the element type `T`.
- **Returns** a `T[]` of length `n` with every element equal to `fill`.

#### `length`

```lin
val length = (x: Json): Int32
```

The number of elements in an array (or characters/entries in a string/object).
- **`x`** — any array, string, or object.
- **Returns** the element/character/entry count as an `Int32`.
- **Example:** length([1, 2, 3])     // 3
- **Example:** length("hello")       // 5
- **Example:** length({ "a": 1 })    // 1

#### `indexOf`

```lin
val indexOf = <T>(arr: T[], target: T, fromIndex: Int32 = 0): Int32
```

Find the index of the first element equal to `target` (deep `==`), scanning left to right.
- **`arr`** — the array to search.
- **`target`** — the value to look for; must be assignable to the element type `T`.
- **`fromIndex`** — index to start from (default `0`); negative counts from the end.
- **Returns** the index of the first match at or after `fromIndex`, or `-1` if none.
- **Example:** [10, 20, 30].indexOf(20)     // 1
- **Example:** [1, 2, 1, 2].indexOf(2, 2)   // 3
- **Example:** [1, 2, 1, 2].indexOf(1, -1)  // -1   (search starts at index 3)

#### `reverse`

```lin
val reverse = <T>(arr: T[]): T[]
```

Return a new array with the elements of `arr` in reverse order. Does not mutate `arr`.
- **`arr`** — the source array.
- **Returns** a new `T[]` holding `arr`'s elements last-to-first.

#### `set`

```lin
val set = <T>(arr: T[], idx: Int32, item: T): Null
```

Write `item` into `arr` at index `idx`, mutating in place.
- **`arr`** — the array to mutate.
- **`idx`** — the slot to write (no bounds checking here).
- **`item`** — the element to store; must be assignable to `T`.
- **Returns** `null`.

#### `at`

```lin
val at = <T, D>(arr: T[], index: Int32, default: D = null): T | D
```

Bounds-safe read with a fallback. Returns the element at `index`, or `default` if `index` is out
of bounds. The default's type `D` is separate from the element type `T`, so the result is `T | D`
and the default never pollutes the element type. Subsumes the old `at`/`atOr` pair.
  - `arr.at(i)`        => `T | Null`  (must be null-checked before use as `T`)
  - `arr.at(i, 0)`     => `T | Int32` (= `T` when `T = Int32`: the "definitely present" form)
  - `arr.at(i, "n/a")` => `T | String`
- **`arr`** — the array to read from.
- **`index`** — the index; negative counts from the end.
- **`default`** — value returned when `index` is out of bounds; defaults to `null`, pinning `D`.
- **Returns** the element at `index`, or `default` (typed `T | D`) when out of bounds.
- **Example:** [10, 20, 30].at(0)         // 10
- **Example:** [10, 20, 30].at(-1)        // 30   (negative wraps)
- **Example:** [].at(0)                   // null               (omitted default -> T | Null)
- **Example:** [10, 20, 30].at(5, -1)     // -1   (out of bounds -> default)
- **Example:** [10, 20, 30].at(9, "n/a")  // "n/a"              (independent default type -> Int32 | String)

#### `partition`

```lin
val partition = <T>(arr: T[], f: (T, Int32)
```

Split `arr` into the elements that satisfy `f` and those that do not, in source order.
- **`arr`** — the array to partition.
- **`f`** — predicate `(item, index?) => Boolean`; the trailing 0-based `Int32` index is optional.
- **Returns** a 2-element array `[pass, fail]`, each a `T[]` (`result[0]` = matches, `result[1]` = the rest).
- **Example:** val [evens, odds] = [1, 2, 3, 4, 5].partition(x => x % 2 == 0)   // evens: [2, 4], odds: [1, 3, 5]

#### `zip`

```lin
val zip = <A, B>(a: A[], b: B[]): [A, B][]
```

Pair up elements at the same index, truncating to the shorter input.
- **`a`** — the first array (element type `A`).
- **`b`** — the second array (element type `B`).
- **Returns** a `[A, B][]` of paired elements, with length `min(length(a), length(b))`.

#### `unique`

```lin
val unique = <T>(arr: T[]): T[]
```

Return a new array with duplicate elements removed, keeping the first occurrence of each in
source order. Elements are compared by deep structural value.
- **`arr`** — the source array.
- **Returns** a new `T[]` of the distinct elements, in first-seen order.

#### `chunk`

```lin
val chunk = <T>(arr: T[], size: Int32): T[][]
```

Split `arr` into consecutive sub-arrays of at most `size` elements (the last may be shorter).
- **`arr`** — the source array.
- **`size`** — the maximum chunk length; values below 1 are treated as 1.
- **Returns** a `T[][]` of the chunks, in order.

#### `compact`

```lin
val compact = (arr: Json): Json
```

Return a new array with all `null` elements removed.
- **`arr`** — any array (typed `Json` so it accepts any element type).
- **Returns** a new array of the non-null elements, in source order.

#### `sort`

```lin
val sort = <T>(arr: T[], cmp: (T, T)
```

STABLE, O(n log n) sort (bottom-up merge sort, O(n) extra space). Equal elements keep their
original input order, and it scales to large inputs. Does not mutate `arr`.
- **`arr`** — the array to sort.
- **`cmp`** — comparator `(a, b) => Int32`: negative if `a` sorts before `b`, positive if after, 0 if equal.
- **Returns** a new sorted `T[]`; ties retain their original relative order.
- **Example:** [3, 1, 4, 1, 5].sort((a, b) => a - b)   // [1, 1, 3, 4, 5]

#### `sortBy`

```lin
val sortBy = <T>(arr: T[], keyFn: (T)
```

Stably sort `arr` by a key extracted from each element, comparing keys with Lin's natural
`<`/`>` ordering. Does not mutate `arr`. (Paired with `searchBy`, which searches such an array.)
- **`arr`** — the array to sort.
- **`keyFn`** — maps an element to its (comparable `Json`) sort key; must accept the element type `T`.
- **Returns** a new `T[]` sorted ascending by key, ties in original order.
- **Example:** people.sortBy(p => p["name"])

### Sortedarray search layer (binary search / bounds / sortedinsert)

#### `lowerBound`

```lin
val lowerBound = <T>(arr: T[], target: T, compare: (T, T)
```

Leftmost insertion index: the first `i` with `compare(arr[i], target) >= 0`. If `target` is
present this is the index of its first occurrence; if absent it is the gap before the first
greater element. O(log n) (Python's `bisect_left` / C++ `lower_bound`). PRECONDITION: `arr` sorted by `compare`.
- **`arr`** — a sorted array.
- **`target`** — the value to locate.
- **`compare`** — the same comparator `arr` is sorted by.
- **Returns** the leftmost index in `[0, length(arr)]` where `target` could be inserted to stay sorted.

#### `upperBound`

```lin
val upperBound = <T>(arr: T[], target: T, compare: (T, T)
```

Rightmost insertion index: the first `i` with `compare(arr[i], target) > 0`. If `target` is
present this is one past its last occurrence, so `upperBound - lowerBound` is its occurrence count.
O(log n) (Python's `bisect_right` / C++ `upper_bound`). PRECONDITION: `arr` sorted by `compare`.
- **`arr`** — a sorted array.
- **`target`** — the value to locate.
- **`compare`** — the same comparator `arr` is sorted by.
- **Returns** the rightmost index in `[0, length(arr)]` where `target` could be inserted to stay sorted.

#### `bisectLeft`

```lin
val bisectLeft = <T>(arr: T[], target: T, compare: (T, T)
```

Alias of `lowerBound`, named for readers coming from Python's `bisect`.
- **Returns** the leftmost insertion index for `target`; see `lowerBound`.

#### `bisectRight`

```lin
val bisectRight = <T>(arr: T[], target: T, compare: (T, T)
```

Alias of `upperBound`, named for readers coming from Python's `bisect`.
- **Returns** the rightmost insertion index for `target`; see `upperBound`.

#### `binarySearch`

```lin
val binarySearch = <T>(arr: T[], target: T, compare: (T, T)
```

Binary search a sorted array for `target`. O(log n). PRECONDITION: `arr` sorted by `compare`.
- **`arr`** — a sorted array.
- **`target`** — the value to find.
- **`compare`** — the same comparator `arr` is sorted by (negative if a before b, positive if after, 0 if equal).
- **Returns** `{ found, index }`: `found` is true iff an equal element is present; `index` is the leftmost
         matching index on a hit, or the insertion point (in `[0, length(arr)]`) on a miss.

#### `searchBy`

```lin
val searchBy = <T>(arr: T[], key: Json, f: (T)
```

Key-extractor variant of `binarySearch`, mirroring `sortBy`: search an array sorted by `f`'s key
for the element whose key equals `key` (keys compared by natural `<`/`>`, as `sortBy` uses, so
`arr.sortBy(f)` then `arr.searchBy(k, f)` are a matched pair). O(log n). PRECONDITION: `arr` sorted by `f`'s key.
- **`arr`** — the array, sorted by `f`'s key.
- **`key`** — the comparable key value to find.
- **`f`** — the same key extractor `arr` was sorted by.
- **Returns** `{ found, index }` as in `binarySearch` (leftmost match; insertion point when absent).

#### `insertSorted`

```lin
val insertSorted = <T>(arr: T[], item: T, compare: (T, T)
```

Return a new array with `item` inserted into the sorted `arr` at `lowerBound(arr, item)`, before
any existing equal elements (a stable left-insert, matching Python's `insort_left`). Does not
modify `arr`. O(log n) to find the position + O(n) to build the result. Repeated `insertSorted`
maintains a running sorted collection without a full re-sort. PRECONDITION: `arr` sorted by `compare`.
- **`arr`** — the sorted array.
- **`item`** — the element to insert.
- **`compare`** — the same comparator `arr` is sorted by.
- **Returns** a new sorted `T[]` of length `length(arr) + 1` containing `item`.

#### `countInRange`

```lin
val countInRange = <T>(arr: T[], lo: T, hi: T, compare: (T, T)
```

Count the elements of the sorted `arr` in the half-open range [lo, hi) — those `x` with
`compare(x, lo) >= 0` and `compare(x, hi) < 0`. O(log n), not a scan. A common analytics
primitive (histogram buckets, "values between X and Y"). PRECONDITION: `arr` sorted by `compare`.
- **`arr`** — the sorted array.
- **`lo`** — inclusive lower bound.
- **`hi`** — exclusive upper bound.
- **`compare`** — the same comparator `arr` is sorted by.
- **Returns** the count, clamped to 0 when `hi` compares less than `lo`.

#### `sum`

```lin
val sum = (arr: Json[]): Json
```

Sum the elements of `arr`.
- **`arr`** — an array of numeric `Json` values.
- **Returns** the sum (`0` for an empty array).

#### `product`

```lin
val product = (arr: Json[]): Json
```

Multiply the elements of `arr` together.
- **`arr`** — an array of numeric `Json` values.
- **Returns** the product (`1` for an empty array).

#### `min`

```lin
val min = (arr: Json[]): Json
```

Find the smallest element of `arr` by natural `<` ordering.
- **`arr`** — a non-empty array of comparable `Json` values.
- **Returns** the minimum element.

#### `max`

```lin
val max = (arr: Json[]): Json
```

Find the largest element of `arr` by natural `>` ordering.
- **`arr`** — a non-empty array of comparable `Json` values.
- **Returns** the maximum element.

#### `minBy`

```lin
val minBy = <T>(arr: T[], keyFn: (T)
```

Find the element with the smallest key under `keyFn`.
- **`arr`** — a non-empty array.
- **`keyFn`** — maps an element to a `Number` key, compared with `<`; checked against the element type.
- **Returns** the element whose key is smallest (the first such element on a tie).

#### `maxBy`

```lin
val maxBy = <T>(arr: T[], keyFn: (T)
```

Find the element with the largest key under `keyFn`.
- **`arr`** — a non-empty array.
- **`keyFn`** — maps an element to a `Number` key, compared with `>`; checked against the element type.
- **Returns** the element whose key is largest (the first such element on a tie).

#### `append`

```lin
val append = <T>(arr: T[], item: T): T[]
```

Append `item` to the end of `arr`, returning a NEW array (does not mutate `arr`). A flat array
(e.g. `UInt8[]`) stays flat; a tagged `Json[]` stays tagged.
- **`arr`** — the source array; its element type pins `T` for element checking.
- **`item`** — the element to append; must be assignable to `T` (`append(intArr, "s")` is a compile error).
- **Returns** a new `T[]` with `item` appended.

#### `prepend`

```lin
val prepend = <T>(arr: T[], item: T): T[]
```

Prepend `item` to the front of `arr`, returning a NEW array (does not mutate `arr`). Same
flat-preservation and RC discipline as `append`.
- **`arr`** — the source array; its element type pins `T`.
- **`item`** — the element to prepend; must be assignable to `T`.
- **Returns** a new `T[]` with `item` at the front.

#### `scan`

```lin
val scan = <T, U>(arr: T[], init: U, f: (U, T)
```

Like `reduce`, but return every intermediate accumulator value, including `init`.
- **`arr`** — the array to fold over.
- **`init`** — the initial accumulator (also the first output element); pins the accumulator type `U`.
- **`f`** — `(acc, item) => acc'` folding step.
- **Returns** a `U[]` of length `length(arr) + 1`: `init` followed by each successive accumulator.

#### `groupBy`

```lin
val groupBy = <T>(arr: T[], keyFn: (T)
```

Group elements into buckets by a string key (a global, hashed grouping).
- **`arr`** — the array to group.
- **`keyFn`** — maps each element to its `String` group key.
- **Returns** a `{ String: T[] }` map from key to the elements with that key, each group in source order.

#### `countBy`

```lin
val countBy = <T>(arr: T[], keyFn: (T)
```

Count elements by a string key.
- **`arr`** — the array to count over.
- **`keyFn`** — maps each element to its `String` bucket key.
- **Returns** a `{ String: Int32 }` map from key to the number of elements with that key.

#### `findIndex`

```lin
val findIndex = <T>(arr: T[], f: (T, Int32)
```

Index of the first element for which `f` returns `true` (the index-returning sibling of `find`).
- **`arr`** — the array to scan.
- **`f`** — predicate `(item, index?) => Boolean`; the trailing 0-based `Int32` index is optional.
- **Returns** the index of the first match, or `-1` if none match.

#### `findLast`

```lin
val findLast = <T>(arr: T[], f: (T, Int32)
```

Find the last element for which `f` returns `true`.
- **`arr`** — the array to scan.
- **`f`** — predicate `(item, index?) => Boolean`; the trailing 0-based `Int32` index is optional.
- **Returns** the last matching element, or `null` if none match (typed `T | Null`).

#### `findLastIndex`

```lin
val findLastIndex = <T>(arr: T[], f: (T, Int32)
```

Index of the last element for which `f` returns `true`.
- **`arr`** — the array to scan.
- **`f`** — predicate `(item, index?) => Boolean`; the trailing 0-based `Int32` index is optional.
- **Returns** the index of the last match, or `-1` if none match.

#### `dedupBy`

```lin
val dedupBy = <T>(arr: T[], f: (T)
```

Group CONSECUTIVE elements with the same key into sub-arrays (maximal runs of adjacent
equal-keyed elements, in order). The positional analogue of `groupBy` (which is global and hashed).
- **`arr`** — the array to scan.
- **`f`** — maps each element to its (comparable `Json`) run key.
- **Returns** a `T[][]` of the consecutive runs (`[]` for an empty input).
