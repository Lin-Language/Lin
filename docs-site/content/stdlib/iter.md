# std/iter

std/iter — iterable combinators and iterator constructors.

A combinator works over any iterable source — an array, an `Iterator`, or a `Stream` (see
std/stream) — and dispatches on the type of its receiver (its first argument, in dot-application
terms):
  - Eager over an array or iterator: it runs immediately and returns a materialised `U[]`.
  - Lazy over a stream: it returns a `Stream<U>` adapter that reads nothing until a terminal
    drives it, one item at a time, with bounded memory.

A handful of combinators are terminals — `reduce`, `for`, `while`, `find`, `some`, `every`. Over
an array they return their plain result; over a stream they drive the pipeline to completion and
gain an `| Error` arm, because a stream read can fail mid-traversal. The same chain that runs
eagerly over an array runs lazily over a stream, just because the receiver is a `Stream`. Over a
stream a combinator consumes its input, so using the same stream value twice is a compile error.

Every combinator callback optionally receives a trailing 0-based `Int32` source index (the JS
`forEach((item, idx) => …)` model); a 1-arg callback stays valid. For `reduce` the index is the
third parameter `(acc, item, i)`. The index is always the source position. The key-extractor
combinators (sortBy/minBy/maxBy/groupBy/countBy, in std/array) take no index.

```lin
import { map, filter, reduce, take, drop } from "std/iter"
import { range, rangeStep, iter, iterOf } from "std/iter"
```

Array-shaped operations (push, slice, sort, sum, …) live in std/array; stream sources and sinks
(readStream, writeStream, lines, drain, …) live in std/stream. The combinators here are the single
vocabulary that spans all three.

Lazy dispatch fires at a concrete combinator call with a `Stream` receiver. A stream passed
through a user-defined generic `Iterable` parameter stays array-shaped (eager) inside that
function.

## Reference

#### `for`

```lin
val for = <T>(iterable: T[] | Iterator | Stream | Null, f: (T, Int32) => AnyVal): Null
```

Run `f` over every item of any iterable (the universal iteration driver). For side effects only.
- **`iterable`** — an Array, Iterator, or Stream of `T`.
- **`f`** — callback `(item, index?) => …`; the trailing 0-based `Int32` index is optional.
- **Returns** `null`.

**Example:**

```lin
[1, 2, 3].for(x => print(x))
```

#### `while`

```lin
val while = <T>(arr: T[] | Iterator | Stream, f: (T, Int32) => Boolean)
```

Pull items while `f` returns `true`, stopping at the first `false` (or at exhaustion). For side
effects / short-circuiting; the per-item value is not collected.
- **`arr`** — an Array, Iterator, or Stream.
- **`f`** — predicate `(item, index?) => Boolean`; iteration continues while it returns `true`.
- **Returns** `null` for an Array/Iterator; `Null | Error` over a Stream, where a read fault surfaces in-band.

**Example:**

```lin
[1, 2, -3, 4].while(x => x >= 0)   // visits 1, 2, stops at -3
```

#### `range`

```lin
val range = (start: Int32, end: Int32)
```

Build the ascending integer sequence `[start, start+1, …, end-1]` (half-open; empty if `start >= end`).
- **`start`** — the first value (inclusive).
- **`end`** — the upper bound (exclusive).
- **Returns** an `Int32[]` of the range.

**Example:**

```lin
range(0, 5).for(i => print(i))    // 0 1 2 3 4
```

**Example:**

```lin
range(1, 6).map(i => i * i)        // [1, 4, 9, 16, 25]
```

#### `rangeStep`

```lin
val rangeStep = (start: Int32, end: Int32, step: Int32): AnyVal
```

Build an integer sequence from `start` toward `end` (exclusive) advancing by `step`.
- **`start`** — the first value (inclusive).
- **`end`** — the bound (exclusive).
- **`step`** — the increment; positive counts up, negative counts down, `0` yields an empty sequence.
- **Returns** an iterator over the stepped range.

**Example:**

```lin
rangeStep(0, 10, 2).for(i => print(i))   // 0, 2, 4, 6, 8
```

**Example:**

```lin
rangeStep(5, 0, -1).map(i => i)           // [5, 4, 3, 2, 1]
```

#### `iterOf`

```lin
val iterOf = (arr: AnyVal)
```

Build an opaque Iterator that yields the elements of `arr` in order.
- **`arr`** — the backing array (its element type is recovered at the consuming combinator, not pinned here).
- **Returns** an Iterator over `arr`'s elements.

**Example:**

```lin
val it = iterOf([10, 20, 30]); it.for(x => print(x))   // prints 10, 20, 30
```

#### `iter`

```lin
val iter = (init: AnyVal, hasNext: AnyVal, next: AnyVal, value: AnyVal)
```

Build a custom Iterator from explicit state-machine closures.
- **`init`** — `() => state` producing the initial state.
- **`hasNext`** — `(state) => Boolean` true while items remain.
- **`next`** — `(state) => state'` advancing to the next state.
- **`value`** — `(state) => item` projecting the current state to its yielded element.
- **Returns** an Iterator driven by those closures.

**Example:**

```lin
iter(() => 0, s => s < 3, s => s + 1, s => s)   // yields 0, 1, 2
```

#### `concat`

```lin
val concat = (a: AnyVal, b: AnyVal): AnyVal
```

Concatenate two iterables into one. Two flat arrays of the same type (e.g. `UInt8[]` ++ `UInt8[]`)
yield a flat array (so byte-level consumers read packed bytes); mixed/tagged element types yield a
`AnyVal[]`. Over streams (`s.lines().concat(other.lines())`) the result is a lazy concatenated Stream.
- **`a`** — the first iterable (Array, Iterator, or Stream).
- **`b`** — the second iterable, appended after `a`.
- **Returns** the concatenation; flat when both inputs are the same flat type, tagged/`AnyVal[]` otherwise.

**Example:**

```lin
concat([1, 2], [3, 4])   // [1, 2, 3, 4]
```

#### `map`

```lin
val map = <T, U>(arr: T[] | Iterator | Stream, f: (T, Int32) => U)
```

Apply `f` to every item, producing a new collection of the results.
- **`arr`** — an Array, Iterator, or Stream of `T`.
- **`f`** — mapping `(item, index?) => U`; the trailing 0-based `Int32` index is optional.
- **Returns** a `U[]` over an Array/Iterator, or a lazy `Stream<U>` over a Stream receiver.

**Example:**

```lin
[1, 2, 3].map(x => x * 2)            // [2, 4, 6]
```

**Example:**

```lin
["a", "b", "c"].map((x, i) => "${i}: ${x}")   // ["0: a", "1: b", "2: c"]
```

#### `filter`

```lin
val filter = <T>(arr: T[] | Iterator | Stream, f: (T, Int32) => Boolean)
```

Keep only the items for which `f` returns `true`.
- **`arr`** — an Array, Iterator, or Stream of `T`.
- **`f`** — predicate `(item, index?) => Boolean`; the trailing 0-based `Int32` index is optional.
- **Returns** a `T[]` of the kept items over an Array/Iterator, or a lazy `Stream<T>` over a Stream receiver.

**Example:**

```lin
[1, 2, 3, 4].filter(x => x % 2 == 0)        // [2, 4]
```

**Example:**

```lin
[10, 20, 30, 40].filter((x, i) => i % 2 == 0)   // [10, 30]  (source indices 0, 2)
```

#### `reduce`

```lin
val reduce = <T, U>(arr: T[] | Iterator | Stream, init: U, f: (U, T, Int32) => U)
```

Fold the items left-to-right into a single accumulated value.
- **`arr`** — an Array, Iterator, or Stream of `T`.
- **`init`** — the initial accumulator; pins the accumulator type `U`.
- **`f`** — folding step `(acc, item, index?) => acc'`; the trailing 0-based `Int32` index is optional.
- **Returns** the final accumulator `U` over an Array/Iterator, or `U | Error` over a Stream receiver.

**Example:**

```lin
[1, 2, 3, 4].reduce(0, (acc, x) => acc + x)   // 10
```

**Example:**

```lin
[1, 1, 1].reduce(0, (acc, x, i) => acc + i)   // 3   (0 + 0 + 1 + 2)
```

#### `find`

```lin
val find = <T>(arr: T[] | Iterator | Stream, f: (T, Int32) => Boolean): T | Null
```

Find the first item for which `f` returns `true`, short-circuiting the scan.
- **`arr`** — an Array, Iterator, or Stream of `T`.
- **`f`** — predicate `(item, index?) => Boolean`.
- **Returns** the first matching item, or `null` if none match (typed `T | Null`).

**Example:**

```lin
[1, 3, 5, 6].find(x => x % 2 == 0)   // 6
```

**Example:**

```lin
[1, 3, 5].find(x => x % 2 == 0)      // null
```

#### `some`

```lin
val some = <T>(arr: T[] | Iterator | Stream, f: (T, Int32) => Boolean): Boolean
```

Test whether `f` returns `true` for at least one item, short-circuiting on the first match.
- **`arr`** — an Array, Iterator, or Stream of `T`.
- **`f`** — predicate `(item, index?) => Boolean`.
- **Returns** `true` if any item matches, `false` otherwise (`false` for an empty source).

**Example:**

```lin
[1, 2, 3].some(x => x > 2)    // true
```

#### `every`

```lin
val every = <T>(arr: T[] | Iterator | Stream, f: (T, Int32) => Boolean): Boolean
```

Test whether `f` returns `true` for every item, short-circuiting on the first failure.
- **`arr`** — an Array, Iterator, or Stream of `T`.
- **`f`** — predicate `(item, index?) => Boolean`.
- **Returns** `true` if all items match, `false` otherwise (`true` for an empty source).

**Example:**

```lin
[1, 2, 3].every(x => x > 0)   // true
```

**Example:**

```lin
[1, 2, 3].every(x => x > 1)   // false
```

#### `flatMap`

```lin
val flatMap = <T, U>(arr: T[] | Iterator | Stream, f: (T, Int32) => U[]): U[]
```

Map each item to an inner array and concatenate all the results into one array.
- **`arr`** — an Array, Iterator, or Stream of `T`.
- **`f`** — `(item, index?) => U[]`; the trailing 0-based `Int32` index is optional.
- **Returns** a `U[]` — the concatenation of every produced inner array, in order.

**Example:**

```lin
[1, 2, 3].flatMap(x => [x, x * 2])   // [1, 2, 2, 4, 3, 6]
```

#### `take`

```lin
val take = <T>(arr: T[] | Iterator | Stream, n: Int32): T[]
```

Take the first `n` items. Safe over an infinite source (`count`/infinite-`repeat`/`cycle`): it
stops pulling after `n` items rather than materialising the whole source.
- **`arr`** — an Array, Iterator, or Stream of `T`.
- **`n`** — the maximum number of items to take; fewer if the source is shorter.
- **Returns** a `T[]` of the first `n` items (a lazy `Stream<T>` over a Stream receiver).

**Example:**

```lin
take([1, 2, 3, 4], 2)   // [1, 2]
```

#### `drop`

```lin
val drop = <T>(arr: T[] | Iterator | Stream, n: Int32): T[]
```

Skip the first `n` items and keep the rest.
- **`arr`** — an Array, Iterator, or Stream of `T`.
- **`n`** — the number of leading items to skip; if `n >= length`, the result is empty.
- **Returns** a `T[]` of the remaining items (a lazy `Stream<T>` over a Stream receiver).

**Example:**

```lin
drop([1, 2, 3, 4], 2)   // [3, 4]
```

#### `takeWhile`

```lin
val takeWhile = <T>(arr: T[] | Iterator | Stream, f: (T, Int32) => Boolean): T[]
```

Take leading items while `f` returns `true`, stopping at (and excluding) the first that fails.
- **`arr`** — an Array, Iterator, or Stream of `T`.
- **`f`** — predicate `(item, index?) => Boolean`; the trailing 0-based `Int32` index is optional.
- **Returns** a `T[]` of the leading run that satisfies `f` (a lazy `Stream<T>` over a Stream receiver).

**Example:**

```lin
[1, 2, 3, 4, 1].takeWhile(x => x < 3)   // [1, 2]
```

#### `dropWhile`

```lin
val dropWhile = <T>(arr: T[] | Iterator | Stream, f: (T, Int32) => Boolean): T[]
```

Skip leading items while `f` returns `true`, then keep everything from the first failure onward.
- **`arr`** — an Array, Iterator, or Stream of `T`.
- **`f`** — predicate `(item, index?) => Boolean`; the trailing 0-based `Int32` index is optional.
- **Returns** a `T[]` starting at the first item that fails `f` (a lazy `Stream<T>` over a Stream receiver).

**Example:**

```lin
[1, 2, 3, 4, 1].dropWhile(x => x < 3)   // [3, 4, 1]
```

#### `flatten`

```lin
val flatten = <T>(arr: T[][]): T[]
```

Collapse one level of nesting, concatenating the sub-arrays in order.
- **`arr`** — an array of arrays (`T[][]`).
- **Returns** the concatenated `T[]`.

**Example:**

```lin
flatten([[1, 2], [3, 4]])   // [1, 2, 3, 4]
```

#### `sliding`

```lin
val sliding = <T>(src: T[] | Iterator | Stream, size: Int32): T[][]
```

Overlapping fixed-width windows advancing by one (window k is `src[k .. k+size]`).
- **`src`** — an Array, Iterator, or Stream of `T`.
- **`size`** — the window width; values below 1 are treated as 1.
- **Returns** a `T[][]` of windows (`[]` when the source is shorter than `size`); a lazy `Stream<T[]>` over a Stream receiver.

#### `pairwise`

```lin
val pairwise = <T>(src: T[] | Iterator | Stream): [T, T][]
```

Pair each item with its successor: `[[src[0],src[1]], [src[1],src[2]], …]`.
- **`src`** — an Array, Iterator, or Stream of `T`.
- **Returns** a `[T, T][]` of adjacent overlapping pairs (`[]` for fewer than 2 items); a lazy `Stream<[T, T]>` over a Stream receiver.

#### `zipWith`

```lin
val zipWith = <A, B, C>(a: A[] | Stream, b: B[], f: (A, B) => C): C[]
```

Combine two sources element-wise with `f` in a single pass (the fused `zip(a, b).map(f)`).
- **`a`** — the first source, an `A[]` or a Stream of `A`.
- **`b`** — the second source, a materialised `B[]`.
- **`f`** — combiner `(itemA, itemB) => C`.
- **Returns** a `C[]` of length `min` of the two sources; a lazy `Stream<C>` when `a` is a Stream.

#### `intersperse`

```lin
val intersperse = <T>(src: T[] | Iterator | Stream, sep: T): T[]
```

Insert `sep` between every pair of adjacent items (not before the first, not after the last).
- **`src`** — an Array, Iterator, or Stream of `T`.
- **`sep`** — the separator value to interleave.
- **Returns** a `T[]` (length `2*n - 1` for `n >= 1`; a 0- or 1-item source is unchanged); a lazy `Stream<T>` over a Stream receiver.

#### `dedup`

```lin
val dedup = <T>(src: T[] | Iterator | Stream): T[]
```

Collapse consecutive runs of equal items to a single item, using deep structural equality (`==`).
Distinct from `std/array.unique`, which dedups globally.
- **`src`** — an Array, Iterator, or Stream of `T`.
- **Returns** a `T[]` with consecutive duplicates removed (a lazy `Stream<T>` over a Stream receiver).

#### `count`

```lin
val count = (start: Int32, step: Int32 = 1): Stream
```

Infinite stream constructors. These build a lazy `Stream<T>` pull-source (the same backend that
powers file/socket streams), rather than an eager array that would try to materialise an infinite
source and never return. Bound them downstream with a short-circuiting stream combinator
(`take`/`takeWhile`/`find`/`some`) before any exhaustive terminal (`for`/`reduce`/`collect`);
otherwise they loop forever, just like a hand-written `while true`. Because they return a Stream,
the chained `.map`/`.take`/… dispatch to the lazy stream backend. To materialise a bounded result,
drive the bounded stream with a terminal, e.g.
`count(0).take(5).reduce([], (acc, x) => acc.push(x); acc)` or `.for(...)`.

Build an infinite counting stream: `start, start+step, start+2*step, …`. Must be bounded downstream.
- **`start`** — the first value.
- **`step`** — the increment between successive values (default `1`).
- **Returns** an infinite `Stream` of counted `Int32` values.

#### `repeat`

```lin
val repeat = <T>(value: T, n: Int32 = -1): Stream
```

Build a stream that yields `value` repeatedly. Must be bounded downstream when `n < 0`.
- **`value`** — the value to repeat.
- **`n`** — how many times to yield it; when `n < 0` (the default) the stream is infinite.
- **Returns** a `Stream` yielding `value` `n` times, or infinitely.

#### `cycle`

```lin
val cycle = <T>(src: T[]): Stream
```

Repeat the elements of a finite, materialised array endlessly. Must be bounded downstream.
- **`src`** — the source array; it takes an array because cycling requires re-reading the source.
- **Returns** an infinite `Stream` cycling through `src` (an empty stream if `src` is empty, not an infinite loop).
