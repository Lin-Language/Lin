# std/iter

Iterable combinators and iterator constructors. A *combinator* works over any **iterable source** — an array, an `Iterator`, or a [`Stream`](/stdlib/stream.html) — and **dispatches on the type of its receiver** (its first argument, in dot-application terms):

- **Eager** over an array or iterator — it runs immediately and returns a materialised `U[]`.
- **Lazy** over a stream — it returns a `Stream<U>` adapter that reads nothing until a terminal drives it, one item at a time, with bounded memory.

A handful of combinators are **terminals** — `reduce`, `for`, `while`, `find`, `some`, `every`. Over an array they return their plain result; over a stream they drive the pipeline to completion and gain an `| Error` arm, because a stream read can fail mid-traversal. Eager combinators are non-mutating and return new values.

```lin
import { map, filter, reduce, take, drop } from "std/iter"
import { range, rangeStep, iter, iterOf } from "std/iter"
```

> Array-shaped operations (`push`, `slice`, `sort`, `sum`, …) live in [`std/array`](/stdlib/array.html). Stream sources and sinks (`readStream`, `writeStream`, `lines`, `drain`, …) live in [`std/stream`](/stdlib/stream.html). The combinators here are the single vocabulary that spans all three.

## Eager vs lazy — one vocabulary, two backends

The same chain that runs eagerly over an array runs **lazily** over a stream, just because the receiver is a `Stream`:

```lin
import { map, filter, take, drop, reduce } from "std/iter"
import { readStream } from "std/stream"

// Eager — over an array, returns a new array:
val result = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
  .filter(x => x % 2 == 0)
  .map(x => x * x)
  .reduce(0, (acc, x) => acc + x)   // 220

// Lazy — over a stream, one line at a time, bounded memory:
val total = readStream("data.csv")
  .lines()                       // Stream<String>
  .drop(1)                       // Stream<String>  (lazy adapter)
  .take(4)                       // Stream<String>  (lazy adapter)
  .map(line => line.length())    // Stream<Int32>
  .reduce(0, (acc, n) => acc + n)   // Int32 | Error  (terminal — drives the stream)

match total
  is Error => print("read failed: ${total["message"]}")
  else     => print("sum = ${total}")
```

Over a stream, a combinator **consumes** its input, so a stream flows through a single chain — using the same stream value twice is a compile-time error. To process the data again, open a fresh stream. See [`std/stream`](/stdlib/stream.html) for details.

> v1 limitation: lazy dispatch fires at a concrete combinator call with a `Stream` receiver. A stream passed through a user-defined generic `Iterable` parameter stays array-shaped (eager) inside that function — the safe resolution.

## Combinator reference

| Function | Array / Iterator | Stream | Description |
| --- | --- | --- | --- |
| `map` | `U[]` | `Stream<U>` (lazy) | Transform each element |
| `filter` | `T[]` | `Stream<T>` (lazy) | Keep matching elements |
| `reduce` | `U` | `U \| Error` (terminal) | Fold left; accumulator first |
| `for` | `Null` | `Null \| Error` (terminal) | Iterate for side effects |
| `while` | `Null` | `Null \| Error` (terminal) | Iterate until callback returns `false` |
| `take` | `T[]` | `Stream<T>` (lazy) | First `n` elements |
| `drop` | `T[]` | `Stream<T>` (lazy) | All elements after the first `n` |
| `takeWhile` | `T[]` | `Stream<T>` (lazy) | Leading elements while predicate holds |
| `dropWhile` | `T[]` | `Stream<T>` (lazy) | Skip leading elements while predicate holds |
| `flatMap` | `Json[]` | `Stream` (lazy) | Map then flatten one level |
| `flatten` | `Json[]` | `Stream` (lazy) | Flatten one level of nesting |
| `concat` | `Json[]` | `Stream` (lazy) | All of `a` then all of `b` |
| `find` | `Json` (or `null`) | `Json \| Null \| Error` (terminal) | First matching element |
| `some` | `Boolean` | `Boolean \| Error` (terminal) | True if any element matches |
| `every` | `Boolean` | `Boolean \| Error` (terminal) | True if all elements match |

## Iterator constructors

| Function | Signature | Description |
| --- | --- | --- |
| `range` | `(Int32, Int32) -> Iterator` | Integers `[start, end)`, stepping by 1 |
| `rangeStep` | `(Int32, Int32, Int32) -> Iterator` | Integers from `start` toward `end` by an explicit (possibly negative) step |
| `iter` | `(()->S, (S)->Boolean, (S)->S, (S)->T) -> Iterator` | Custom iterator from init / hasNext / next / value |
| `iterOf` | `(Json[]) -> Iterator` | Iterator over an array |

---

### `map`

Applies `f` to each element in order. Array/Iterator → eager `U[]`. Stream → lazy `Stream<U>` (`f` runs once per item as the item is pulled).

```lin
[1, 2, 3].map(x => x * 2)                 // [2, 4, 6]
["a", "b"].map(toUpper)                   // ["A", "B"]
readStream("in.csv").lines().map(toUpper)   // Stream<String> (lazy)
```

---

### `filter`

Keeps elements for which `f` returns `true`. Array/Iterator → eager `T[]`. Stream → lazy `Stream<T>`.

```lin
[1, 2, 3, 4].filter(x => x % 2 == 0)      // [2, 4]
readStream("app.log").lines().filter(line => line.contains("ERROR"))   // Stream<String> (lazy)
```

---

### `reduce`

Folds left-to-right from `init`; the accumulator is the **first** argument to the combining function. Array/Iterator → eager `U`. Stream → terminal `U | Error` (drives the stream on the calling thread; a read fault surfaces as `Error`).

```lin
[1, 2, 3, 4].reduce(0, (acc, x) => acc + x)   // 10
readStream("nums.txt")
  .lines()
  .reduce(0, (acc, line) => acc + line.parseInt32())   // Int32 | Error
```

---

### `for`

Iterates over each element, calling `f` (its return value is discarded). Array/Iterator → `Null`. Stream → terminal `Null | Error` (EOF ends normally as `Null`; a read error mid-traversal becomes the result). Lin has no `for…in`; iteration is always `.for(fn)`.

```lin
[1, 2, 3].for(x => print(x))
range(0, 5).for(i => print(i))

val outcome = readStream("in.log").lines().for(line => print(line))
match outcome
  is Error => print("read failed: ${outcome["message"]}")
  else     => null
```

---

### `take` / `drop`

`take` keeps the first `n` elements; `drop` skips them. Array/Iterator → eager `T[]`. Stream → lazy `Stream<T>` (`take` ends after `n` items and stops pulling upstream).

```lin
take([1, 2, 3, 4], 2)   // [1, 2]
drop([1, 2, 3, 4], 2)   // [3, 4]
readStream("huge.log")
  .lines()
  .take(100)
  .for(line => print(line))   // first 100 lines only
readStream("data.csv").lines().drop(1)                              // skip the header, lazily
```

---

### `takeWhile` / `dropWhile`

`takeWhile` keeps leading elements while `f` returns `true`, stopping at the first `false`; `dropWhile` skips them and keeps the rest unchanged. Array/Iterator → eager `T[]`. Stream → lazy `Stream<T>`.

```lin
[1, 2, 3, 4, 1].takeWhile(x => x < 3)   // [1, 2]
[1, 2, 3, 4, 1].dropWhile(x => x < 3)   // [3, 4, 1]
```

---

### `flatMap` / `flatten` / `concat`

```lin
[1, 2, 3].flatMap(x => [x, x * 2])   // [1, 2, 2, 4, 3, 6]
flatten([[1, 2], [3, 4]])            // [1, 2, 3, 4]
concat([1, 2], [3, 4])               // [1, 2, 3, 4]
```

Over streams these are lazy: `flatMap`/`flatten` yield each inner element in turn, and `concat` yields all of the first stream followed by the second (both stream arguments are consumed).

---

### `find`

The first element for which `f` returns `true`, or `null` if none. Array/Iterator → `Json`. Stream → terminal `Json | Null | Error`.

```lin
[1, 3, 5, 6].find(x => x % 2 == 0)   // 6
[1, 3, 5].find(x => x % 2 == 0)      // null
```

---

### `some` / `every`

`some` is `true` if `f` matches at least one element; `every` is `true` if it matches all (and for an empty source). Both short-circuit. Array/Iterator → `Boolean`. Stream → terminal `Boolean | Error`.

```lin
[1, 2, 3].some(x => x > 2)    // true
[1, 2, 3].every(x => x > 0)   // true
[1, 2, 3].every(x => x > 1)   // false
```

---

### `while`

Iterates calling `f` with each element, stopping as soon as `f` returns `false`. Array/Iterator → `Null` (the short-circuit primitive behind `some`/`every`/`find`). Stream → terminal `Null | Error`.

```lin
[1, 2, -3, 4].while(x => x >= 0)   // visits 1, 2, stops at -3
```

---

### `range`

Returns an iterator yielding integers from `start` up to (but not including) `end`, stepping by `1`. If `start >= end`, the iterator is empty. For a custom or negative step, use `rangeStep`.

```lin
range(0, 5).for(i => print(i))    // 0 1 2 3 4
range(1, 6).map(i => i * i)       // [1, 4, 9, 16, 25]
```

---

### `rangeStep`

Yields integers from `start` toward `end` (exclusive) by `step`: a positive step counts up while `i < end`, a negative step counts down while `i > end`, and a step of `0` yields an empty iterator.

```lin
rangeStep(0, 10, 2).for(i => print(i))   // 0, 2, 4, 6, 8
rangeStep(5, 0, -1).map(i => i)          // [5, 4, 3, 2, 1]
```

---

### `iter`

Constructs a custom iterator from four functions: `init` produces the initial state, `hasNext` tests whether to continue, `next` advances the state, and `value` extracts the current element.

```lin
// Fibonacci iterator
val fibs = iter(
  () => { "a": 0, "b": 1 },
  s => s["a"] < 100,
  s => { "a": s["b"], "b": s["a"] + s["b"] },
  s => s["a"]
)
fibs.for(n => print(n))
```

---

### `iterOf`

Returns an iterator that yields each element of `arr` in order — a first-class iterator value that can be passed around before consumption.

```lin
val it = iterOf([10, 20, 30])
it.for(x => print(x))   // prints 10, 20, 30
```
