# Additional iterable combinators — design proposal

## Status: proposal (enriches std/iter & std/array)

Lin's `std/iter` already carries the hard part: a single combinator vocabulary that
dispatches on the receiver's static type — **eager** over an array/iterator (`T[]`) and
**lazy** over a `Stream<T>` (an adapter that pulls nothing until a terminal drives it), with
terminals over a stream gaining an `| Error` arm (ADR-051). What is missing is breadth. The
itertools/collections "second tier" — overlapping windows, adjacent pairing, zip-with-a-function,
interspersing a separator, infinite generators, run-length de-duplication, index-returning
search — is exactly the set every mature standard library ships: Rust's
[`itertools`](https://docs.rs/itertools) (`tuple_windows`, `dedup`, `intersperse`, `zip_eq`),
the [Scala collections](https://www.scala-lang.org/api/current/scala/collection/Iterator.html)
(`sliding`, `zip`, `LazyList.continually`), Python's
[`itertools`](https://docs.python.org/3/library/itertools.html) (`pairwise`, `count`, `cycle`,
`repeat`, `groupby`), and [Lodash](https://lodash.com/docs) (`chunk`, `zipWith`, `findIndex`,
`findLastIndex`, `sortedUniq`). Each appears here because Lin users currently hand-roll it with
an index `var` and a `push` loop — the precise toil a combinator library exists to remove. The
infinite generators are the most interesting addition: they are only meaningful **lazily**, and
they exercise the same pull backend that already powers streams, so they let `range`-style
iterator construction grow without a new runtime concept.

This proposal is **opinionated about scope**. It ships the high-value, low-ambiguity members and
explicitly defers the ones whose semantics are contested, whose value the existing index
parameter already delivers, or whose representation cost is not yet justified.

The guiding placement rule, matching the current split:

- **`std/iter`** — anything that has a sensible **lazy stream form** (an adapter that yields
  without materialising), or that constructs an `Iterator`. These get the eager-over-array /
  lazy-over-stream dual.
- **`std/array`** — anything that is **materialised-only** by nature (it must see the whole array,
  or it returns an index into a positional structure that a stream does not have).

All eager forms are non-mutating and return new values, in keeping with the rest of the module.

---

## Ship

### sliding (iter) {#sliding-iter}

```txt
val sliding: <T>(src: T[] | Iterator | Stream<T>, size: Int32) -> T[][] | Stream<T[]>
```

Overlapping fixed-length windows of width `size`, advancing by one element. Window *k* is
`src[k .. k+size]`. A source shorter than `size` yields **no** windows (`[]`), matching Scala's
`sliding` and Rust's `windows`. `size` must be at least 1.

- **Array/Iterator** → eager `T[][]`.
- **Stream** → lazy `Stream<T[]>`: the adapter keeps a bounded ring buffer of the last `size`
  items and emits a window per pulled item once the buffer is full. This is the headline lazy
  case — moving-average / n-gram passes over an unbounded log read in O(size) memory.

```txt
sliding([1, 2, 3, 4], 2)              // [[1, 2], [2, 3], [3, 4]]
sliding([1, 2], 3)                    // []
[1, 2, 3, 4, 5].sliding(3).map(w => sum(w) / 3.0)   // moving average of width 3

readStream("ticks.csv").lines()
  .map(parseTick)
  .sliding(20)                        // Stream<Tick[]>  (lazy 20-tick window)
  .map(window => sma(window))
```

The name is `sliding` (not `windows`) because it reads better in dot-application
(`.sliding(3)`) and matches Scala; `windows` would be an acceptable alias but we ship a single
name to keep the surface tight. **Ship — `std/iter`.** This is the flagship addition and the
clearest lazy-stream win in the list.

---

### pairwise (iter) {#pairwise-iter}

```txt
val pairwise: <T>(src: T[] | Iterator | Stream<T>) -> [T, T][] | Stream<[T, T]>
```

Adjacent overlapping pairs: `[[src[0], src[1]], [src[1], src[2]], …]`. Exactly
`sliding(src, 2)` re-typed from `T[][]` to the tuple `[T, T][]` — the tuple shape is the point,
because it destructures cleanly and types each position. A source of fewer than 2 elements yields
`[]`. Matches Python 3.10's `itertools.pairwise` and Rust's `tuple_windows::<(_, _)>`.

- **Array/Iterator** → eager `[T, T][]`.
- **Stream** → lazy `Stream<[T, T]>`.

```txt
pairwise([1, 2, 3, 4])                          // [[1, 2], [2, 3], [3, 4]]
[10, 13, 9, 12].pairwise().map(([a, b]) => b - a)   // deltas: [3, -4, 3]
```

**Ship — `std/iter`.** Cheap given `sliding`, and the typed tuple makes "compare each element to
its neighbour" a one-liner — the single most common reason to reach for `sliding(_, 2)`.

---

### zipWith (iter) {#zipWith-iter}

```txt
val zipWith: <A, B, C>(a: A[] | Stream<A>, b: B[], f: (A, B) -> C) -> C[] | Stream<C>
```

Zips two sources element-wise and applies `f` to each pair in one pass — the fused form of
`zip(a, b).map(([x, y]) => f(x, y))`, avoiding the intermediate tuple array. Length is the
shorter of the two inputs. Matches Lodash `zipWith`, Scala `lazyZip(_).map(_)`, Haskell `zipWith`.

- **Array** → eager `C[]`.
- **Stream** (receiver `a` is a stream; `b` stays a materialised `T[]`) → lazy `Stream<C>` that
  pulls `a` lazily and indexes into the in-memory `b`, ending when `a` ends or `b` is exhausted.
  Zipping **two** streams is deferred (see below) — the common case is "decorate a stream with a
  parallel in-memory table".

```txt
zipWith([1, 2, 3], [10, 20, 30], (x, y) => x + y)   // [11, 22, 33]
zipWith(names, scores, (n, s) => "${n}: ${s}")       // ["alice: 90", …]
```

**Ship — `std/iter`.** Common enough (combine two columns) that the fused form pulls its weight;
the existing `zip` + `map` works but allocates a throwaway tuple per element.

---

### intersperse (iter) {#intersperse-iter}

```txt
val intersperse: <T>(src: T[] | Iterator | Stream<T>, sep: T) -> T[] | Stream<T>
```

Inserts `sep` between every pair of adjacent elements (not before the first, not after the last).
A source of 0 or 1 element is returned unchanged. Matches Rust `itertools::intersperse` and
Haskell `intersperse`.

- **Array/Iterator** → eager `T[]` (length `2*n - 1` for `n >= 1`).
- **Stream** → lazy `Stream<T>` (emit item, then emit `sep` before each subsequent item).

```txt
intersperse([1, 2, 3], 0)              // [1, 0, 2, 0, 3]
intersperse(["a", "b", "c"], "-")      // ["a", "-", "b", "-", "c"]
```

Note this interleaves a *separator element*, distinct from string `join` (which produces a single
`String`); use `std/string.join` to build delimited text. **Ship — `std/iter`.** Low-cost, and the
lazy form is genuinely useful (interleaving a sentinel into a stream pipeline).

---

### count (iter constructor) {#count-iter}

```txt
val count: (start: Int32, step: Int32 = 1) -> Iterator
```

An **infinite** counting iterator: `start`, `start+step`, `start+2*step`, … forever. The lazy
analogue of `range`/`rangeStep` with no upper bound. Mirrors Python `itertools.count`.

```txt
count(0).map(i => i * i).take(5)        // [0, 1, 4, 9, 16]
count(10, -2).take(3)                   // [10, 8, 6]
```

Only safe when bounded downstream by `take`/`takeWhile`/`find`/`some` — a `for`/`map`/`reduce`
over an unbounded `count` never terminates (the same footgun as any infinite loop; documented, not
prevented). **Ship — `std/iter`.** It is the natural completion of the `range`/`rangeStep` family
and reuses the existing `lin_iter` constructor verbatim (`init = () => start`,
`hasNext = _ => true`, `next = i => i + step`, `value = i => i`).

---

### repeat (iter constructor) {#repeat-iter}

```txt
val repeat: <T>(value: T, n: Int32 = -1) -> Iterator
```

Yields `value` either `n` times, or — when `n < 0` (the default) — **infinitely**. Mirrors Python
`itertools.repeat(value[, times])` and Rust `std::iter::repeat` / `repeat_n`.

```txt
repeat(0, 4)                            // iterator -> [0, 0, 0, 0]
repeat("x").take(3)                     // ["x", "x", "x"]
zipWith(names, repeat(true), (n, _) => n)   // tag every name
```

**Ship — `std/iter`.** Both the bounded and infinite forms are useful (padding, constant columns
for `zipWith`). Bounded `repeat(v, n)` overlaps `arrayAllocateFilled(n, v)`, but the iterator form
composes lazily into a pipeline without materialising.

---

### cycle (iter) {#cycle-iter}

```txt
val cycle: <T>(src: T[]) -> Iterator
```

Repeats the elements of a **finite, materialised** `src` endlessly: `src[0], …, src[n-1], src[0],
…`. An empty `src` yields an empty iterator (not an infinite loop over nothing). Mirrors Python
`itertools.cycle` and Rust `Iterator::cycle`.

```txt
cycle([1, 2, 3]).take(7)                // [1, 2, 3, 1, 2, 3, 1]
zipWith(rows, cycle(["odd", "even"]), (r, cls) => style(r, cls))   // alternating row classes
```

Takes an **array** (not a stream/iterator) source by design: cycling requires re-reading the
source, which a single-use stream cannot provide, and a general iterator has no replay. This keeps
the contract honest — `cycle` buffers nothing it cannot re-read, because it holds the whole array.
**Ship — `std/iter`** (constructor; input is an array, output is a lazy `Iterator`). Must be
bounded downstream like `count`/infinite-`repeat`.

---

### dedup (iter) {#dedup-iter}

```txt
val dedup: <T>(src: T[] | Iterator | Stream<T>) -> T[] | Stream<T>
```

Collapses **consecutive** runs of equal elements to a single element, using deep structural
equality. Distinct from `std/array.unique`, which removes **all** duplicates globally (and must see
the whole array); `dedup` only compares each element to the one before it, so it is streamable in
O(1) memory. Mirrors Rust `itertools::dedup`, Python `groupby`-then-key, Unix `uniq`.

- **Array/Iterator** → eager `T[]`.
- **Stream** → lazy `Stream<T>` (hold the last-emitted value; emit only when the next differs).

```txt
dedup([1, 1, 2, 3, 3, 3, 1])           // [1, 2, 3, 1]   (note trailing 1 survives — run-based)
unique([1, 1, 2, 3, 3, 3, 1])          // [1, 2, 3]      (global, for contrast)

readStream("sensor.log").lines()
  .map(parseReading)
  .dedup()                              // drop runs of unchanged readings, lazily
```

**Ship — `std/iter`.** The streamable run-collapse is a real capability `unique` cannot offer
(`unique` needs a global seen-set). The contrast with `unique` is worth a doc cross-reference.

---

### dedupBy (array) {#dedupBy-array}

```txt
val dedupBy: <T>(arr: T[], f: (T) -> Json) -> T[][]
```

Groups **consecutive** elements that produce the same key under `f` into sub-arrays — the
positional analogue of `groupBy` (which is global and hashed). Each group is a maximal run of
adjacent equal-keyed elements, in order. This is Python's `itertools.groupby` and Rust's
`group_by`/`chunk_by`.

```txt
[1, 1, 2, 2, 2, 1].dedupBy(x => x)
// [[1, 1], [2, 2, 2], [1]]

events.dedupBy(e => e["date"])          // runs of same-day events, order preserved
```

**Ship — `std/array`.** Returns `T[][]` (a materialised grouping), and the consecutive-run
semantics are exactly what `groupBy` cannot express. Named `dedupBy` to pair with `dedup`; a
streaming `Stream<T[]>` form is plausible but deferred (window-of-unknown-length buffering needs
the same backend work as `sliding` and adds little over the array form for now).

---

### findIndex (array) {#findIndex-array}

```txt
val findIndex: <T>(arr: T[], f: (T[, i: Int32]) -> Boolean) -> Int32
```

Returns the index of the first element for which `f` returns `true`, or `-1` if none. The
index-returning sibling of `find` (which returns the element). Carries the optional 0-based index
callback param, consistent with the other predicate combinators. Mirrors Lodash/JS `findIndex`,
Rust `position`.

```txt
[10, 20, 30].findIndex(x => x > 15)    // 1
[1, 2, 3].findIndex(x => x > 9)        // -1
```

**Ship — `std/array`.** Returns a positional index, which only makes sense over a materialised,
indexable array — hence `std/array`, not the dual-dispatch `std/iter`. (A stream has no stable
source index for a terminal to return; the existing `find` already covers the stream case by
yielding the element.)

---

### findLast / findLastIndex (array) {#findLast-array}

```txt
val findLast:      <T>(arr: T[], f: (T[, i: Int32]) -> Boolean) -> T | Null
val findLastIndex: <T>(arr: T[], f: (T[, i: Int32]) -> Boolean) -> Int32
```

Search from the **end**: `findLast` returns the last matching element (or `null`), `findLastIndex`
its index (or `-1`). Mirrors Lodash `findLast`/`findLastIndex` and JS `findLast`.

```txt
[1, 2, 3, 4].findLast(x => x % 2 == 0)        // 4
[1, 2, 3, 4].findLastIndex(x => x % 2 == 0)   // 3
```

**Ship — `std/array`.** A right-to-left scan inherently requires a materialised array (a stream
cannot be walked backwards), so both belong in `std/array`. They round out the `find` family
without ambiguity.

---

## Deferred / rejected

- **`enumerate(arr) -> [Int32, T][]`** — *rejected.* The optional 0-based index callback parameter
  already in place (`map((x, i) => …)`, `filter((x, i) => …)`) covers essentially every use of
  `enumerate`, and does so without allocating a tuple array. The one thing a standalone
  `enumerate` adds — a first-class `[Int32, T][]` you can pass around — is rare enough not to earn
  a dedicated combinator; if needed, `map((x, i) => [i, x])` is explicit and one line. Shipping
  both would be two ways to do the same thing.

- **Deep / recursive `flatten`** — *deferred.* The current one-level `flatten: <T>(T[][]) -> T[]`
  is well-typed precisely because the nesting depth is exactly one (the type `T[][]` proves it). A
  recursive `flattenDeep` cannot be given a sound monomorphized return type — `T[][][]…` of unknown
  depth collapses to `Json[]`, losing the element type, and Lin's argument-driven inference has no
  way to bound the depth. It would only be expressible over `Json[]` with an erased element type.
  Defer until there is a concrete demand that justifies the `Json`-typed exception; the typed
  one-level form plus an explicit `flatMap` chain covers structured cases today.

- **`zip` of two streams (`zipWith`/`zip` over `Stream<A>, Stream<B>`)** — *deferred.* Pulling two
  lazy pull-graphs in lockstep needs the backend to drive two upstreams alternately and reconcile
  their independent in-band error arms — a genuinely new stream-engine capability and a separate
  ADR. The shipped `zipWith` handles the dominant case (a stream against an in-memory table);
  stream-vs-stream waits for a real use case.

- **`tap` / `forEach`** — *rejected.* `for` already is the side-effecting driver; `forEach` is a
  pure rename, and `tap` (run a side effect, pass the value through) is `.map(x => { eff(x); x })`.
  Neither earns a name.

- **`minByKey` / `maxByKey`** — *already present* as `minBy`/`maxBy` in `std/array`. No addition.

- **`scanLazy` (streaming `scan`)** — *deferred, noted.* `scan` is array-only today
  (`std/array`). A lazy `Stream<U>` form (emit each running accumulator as items are pulled) is a
  natural and cheap stream adapter and a good follow-up, but it is out of scope for this
  array-staple-focused proposal; folded into the implementation-notes backend work if `sliding`
  lands.

---

## Implementation notes

**Pure-Lin, eager, today (no runtime work):** `pairwise`, `intersperse`, `dedup` (array arm),
`dedupBy`, `findIndex`, `findLast`, `findLastIndex`, `zipWith` (array arm), and the bounded form of
`repeat`/`cycle` can all be written as thin Lin functions over the existing primitives — `for` with
an index `var` and `push`/`lin_array_allocate`, exactly like the current `take`/`drop`/`takeWhile`
bodies in `stdlib/iter.lin`. `sliding`'s array arm is a `slice`-per-window loop. None of these need
new intrinsics. Following the existing file's self-containment rule, `std/iter` keeps using its
private `length`/`push` wrappers and must **not** import `std/array` (that would form the
`array → iter → array` import cycle the header warns about); the `std/array`-resident members
(`dedupBy`, `findIndex`, `findLast`, `findLastIndex`) may use `std/array`'s own primitives freely.

**Infinite iterator constructors (`count`, `repeat` infinite, `cycle`):** these need **no new
backend** — they are direct uses of the existing `lin_iter(init, hasNext, next, value)` constructor
with a `hasNext` that is `_ => true` (or, for `cycle`, indexes modulo the buffered array length).
The eager combinators already drive an `Iterator` to exhaustion via `lin_while`/`lin_for`, so the
only contract is the documented one: an infinite iterator **must** be bounded by a short-circuiting
combinator (`take`/`takeWhile`/`find`/`some`) before any exhaustive terminal
(`map`/`reduce`/`for`). This is the one place worth a checker nicety later — flagging an obviously
unbounded `count().map(…)`/`reduce` — but it is not required for correctness and is out of scope
here (it is the same non-termination footgun as a hand-written `while true` loop).

**Lazy stream adapters (require runtime work):** the `Stream` arms of `sliding`/`pairwise`,
`intersperse`, `dedup`, and `zipWith` are genuinely new lazy pull-graph nodes. Each must register
in the same name-based streamish dispatch the existing `map`/`filter`/`take` use
(`streamish_combinator_ret` re-typing the call-site result to the stream-shaped form, the IR
redirecting to a `lin_stream_*` adapter rather than the eager `lin_map`/etc.), and each must thread
the in-band error arm so a mid-stream read fault still short-circuits to the terminal (ADR-051,
spec §27.9.4). Their memory profiles are bounded and small:

- `sliding`/`pairwise` — a ring buffer of the last `size` items; emit a window once full.
- `intersperse` — a one-bit "emitted-first-yet" latch; emit `sep` before each non-first item.
- `dedup` — hold the single last-emitted value for the equality compare.
- `zipWith` (stream arm) — pull the stream lazily, index into the captured in-memory `b`, end on
  the shorter side.

These four adapters are the only items that touch the runtime; everything else is pure Lin. A
pragmatic phasing: ship all the **eager/array** forms and the **iterator constructors** first
(pure-Lin, immediately useful, no backend risk), then add the lazy stream adapters as a second
increment once the `sliding` ring-buffer node is built (it is the template the others follow). Note
the existing constraint that the optional index callback param is **array/iterator-only** — the
stream arms of these new combinators keep 1-arg callbacks, consistent with the rest of `std/iter`
over a `Stream`.
