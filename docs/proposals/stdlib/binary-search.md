## Status: proposal (enriches std/array)

`std/array` already ships an ordering layer — a stable `sort`/`sortBy` (a merge
sort over the `(T, T) -> Int32` comparator / `(T) -> Json` key extractor) — but it
has no way to *exploit* a sorted array. The only lookup it offers, `indexOf`, is an
unconditional linear scan with deep structural equality: `O(n)` even when the data
is sorted, and it answers only "is this exact element present?". Every mature
standard library pairs its sort with logarithmic search on the sorted result:
Python's `bisect` (`bisect_left`/`bisect_right`/`insort`), Rust's
`slice::binary_search`/`partition_point`, Java's `Arrays.binarySearch`, and the
C++ STL's `lower_bound`/`upper_bound`/`equal_range`. These are the primitives
behind sorted-set membership, range queries ("how many values fall in `[lo, hi)`?"),
percentile/quantile lookups, sorted-insert (keeping a list ordered without a full
re-sort), and merge-style joins.

This proposal adds that layer **additively** to `std/array`. Nothing existing
changes. Every new function is pure-Lin — a handful of index loops over the array,
calling the same `(T, T) -> Int32` comparator that `sort` already takes — so they
are comparator-driven generics that monomorphize exactly like `sort` (no new
intrinsic, no runtime surface). The reusable core is the two bound functions
(`lowerBound`/`upperBound`); `binarySearch`, `searchBy`, `insertSorted`, and the
`countInRange` convenience all reduce to them.

### Design decisions (justified up front)

- **The not-found return is `{ found: Boolean, index: Int32 }` — NOT `Int32 | Null`
  and NOT Java's `-(insertion_point) - 1`.** The single most valuable output of a
  failed search on sorted data is the *insertion point*: the index where the target
  *would* go to keep the array sorted. That is what powers sorted-insert, range
  counting, and "nearest neighbour" lookups. A `Int32 | Null` result throws that
  information away (a `null` tells you "absent" but not "absent *where*"), forcing
  the caller to run a *second* `lowerBound` pass to recover it. Java's
  `-(insertion_point) - 1` packs both into one `Int32`, but it is a notorious
  footgun: the caller must remember the bit-twiddle, it cannot represent "found at
  index 0" and "would insert at 0" distinctly without the sign trick, and it reads
  as line-noise (`if (i < 0) insertAt = -i - 1`). The record is self-documenting,
  needs no decoding, and gives the insertion point *whether or not the element was
  found* (when `found` is `true`, `index` is a matching index, which is also a valid
  insertion point for an equal key). This is more idiomatic for a union-and-record
  language than smuggling two meanings into a signed integer. The `found` flag
  cleanly disambiguates the present-at-0 vs absent-insert-at-0 case that a bare
  index cannot.
- **`lowerBound`/`upperBound` are the named primitives** (the C++ STL spelling),
  with `bisectLeft`/`bisectRight` available as exact aliases for readers coming from
  Python. `lowerBound` returns the **leftmost** index at which `target` could be
  inserted while keeping the array sorted (i.e. the first index `i` with
  `compare(arr[i], target) >= 0` — the start of the run of elements equal to
  `target`, or the gap before the first greater element). `upperBound` returns the
  **rightmost** such index (the first index `i` with `compare(arr[i], target) > 0` —
  one past the last element equal to `target`). Both always return a valid index in
  `[0, length(arr)]`; neither ever "fails".
- **`binarySearch` returns the LEFTMOST match.** When duplicates of `target` exist,
  `binarySearch` reports `found: true` with `index` = `lowerBound(arr, target)` (the
  first equal element), so it is deterministic and composes with the bounds. (Java
  and Rust leave the matched index *unspecified* among duplicates; pinning it to the
  leftmost is a strictly more useful guarantee and costs nothing.)
- **PRECONDITION: the array must already be sorted by the same `compare`.** Behavior
  on an unsorted (or differently-sorted) array is **undefined** — the function may
  return any index and will not error. This matches the cost model: the whole point
  is `O(log n)` with no scan, and verifying sortedness would be `O(n)`, defeating
  the purpose. This is exactly the contract of `bisect`, `slice::binary_search`,
  `Arrays.binarySearch`, and `lower_bound`. It is the caller's job to pass a sorted
  array (typically the output of `sort`/`sortBy` with the *same* comparator).
- **`searchBy` mirrors `sortBy`.** It takes a `(T) -> Json` key extractor and
  searches an array sorted by that key, comparing keys with Lin's natural `<`/`>`
  ordering — exactly the comparison `sortBy` uses internally. So `arr.sortBy(f)`
  then `arr.searchBy(key, f)` is the obvious sorted-by-key lookup pair.
- **`insertSorted` is non-mutating** (returns a new array), consistent with `sort`,
  `append`, `slice`, and the rest of the transformation layer. The in-place idiom
  remains `push`/`set`.

---

### binarySearch

```txt
val binarySearch: <T>(arr: T[], target: T, compare: (T, T) -> Int32) -> { "found": Boolean, "index": Int32 }
```

Searches the **sorted** array `arr` for `target` using the same comparator shape as
[`sort`](#sort) (`compare(a, b)` is negative if `a` should come before `b`, positive
if after, `0` if equal). Runs in `O(log n)`.

Returns a record with two fields:

- `found` — `true` if an element comparing equal to `target` (`compare` returns `0`)
  is present, `false` otherwise.
- `index` — when `found` is `true`, the index of the **leftmost** matching element;
  when `found` is `false`, the **insertion point** — the index at which `target`
  could be inserted to keep `arr` sorted. The insertion point is always in
  `[0, length(arr)]`.

**Precondition:** `arr` must already be sorted by `compare`. The result is undefined
(but never an error or trap) on an unsorted array. Generic over the element type `T`:
the comparator is checked against the array's element type, so a mistyped comparator
is a compile error.

```txt
[1, 3, 5, 7, 9].binarySearch(5, (a, b) => a - b)
// { "found": true, "index": 2 }

[1, 3, 5, 7, 9].binarySearch(6, (a, b) => a - b)
// { "found": false, "index": 3 }   (6 would go between 5 and 7)

[1, 3, 5, 7, 9].binarySearch(10, (a, b) => a - b)
// { "found": false, "index": 5 }   (past the end)

[2, 2, 2].binarySearch(2, (a, b) => a - b)
// { "found": true, "index": 0 }    (leftmost match)

val r = [10, 20, 30].binarySearch(20, (a, b) => a - b)
if r["found"] then r["index"] else -1   // 1
```

---

### lowerBound

```txt
val lowerBound: <T>(arr: T[], target: T, compare: (T, T) -> Int32) -> Int32
```

Returns the **leftmost** index at which `target` could be inserted into the sorted
array `arr` while keeping it sorted — equivalently, the first index `i` for which
`compare(arr[i], target) >= 0`. If `target` is present, this is the index of its
first occurrence; if absent, it is the gap before the first element greater than
`target`. Always in `[0, length(arr)]`. Runs in `O(log n)`. This is Python's
`bisect_left` and the C++ STL `lower_bound`.

**Precondition:** `arr` must be sorted by `compare` (result undefined otherwise).

```txt
[1, 2, 2, 2, 3].lowerBound(2, (a, b) => a - b)   // 1   (first 2)
[1, 2, 2, 2, 3].lowerBound(3, (a, b) => a - b)   // 4
[1, 3, 5].lowerBound(4, (a, b) => a - b)          // 2   (insert between 3 and 5)
[1, 3, 5].lowerBound(0, (a, b) => a - b)          // 0
[1, 3, 5].lowerBound(9, (a, b) => a - b)          // 3
```

---

### upperBound

```txt
val upperBound: <T>(arr: T[], target: T, compare: (T, T) -> Int32) -> Int32
```

Returns the **rightmost** index at which `target` could be inserted into the sorted
array `arr` while keeping it sorted — equivalently, the first index `i` for which
`compare(arr[i], target) > 0`. If `target` is present, this is one **past** its last
occurrence. Always in `[0, length(arr)]`. Runs in `O(log n)`. This is Python's
`bisect_right` and the C++ STL `upper_bound`.

The half-open range `[lowerBound(arr, t), upperBound(arr, t))` is exactly the run of
elements equal to `t`, so `upperBound - lowerBound` is the count of occurrences of
`t` (the C++ `equal_range` decomposition).

**Precondition:** `arr` must be sorted by `compare` (result undefined otherwise).

```txt
[1, 2, 2, 2, 3].upperBound(2, (a, b) => a - b)   // 4   (one past last 2)
[1, 2, 2, 2, 3].upperBound(0, (a, b) => a - b)   // 0
[1, 3, 5].upperBound(3, (a, b) => a - b)          // 2

// count occurrences of 2:
val xs = [1, 2, 2, 2, 3]
xs.upperBound(2, (a, b) => a - b) - xs.lowerBound(2, (a, b) => a - b)   // 3
```

---

### searchBy

```txt
val searchBy: <T>(arr: T[], key: Json, f: (T) -> Json) -> { "found": Boolean, "index": Int32 }
```

Key-extractor variant of [`binarySearch`](#binarySearch), mirroring
[`sortBy`](#sortBy): searches an array **sorted by the key produced by `f`** for the
element whose key equals `key`. Keys are compared with Lin's natural ordering
(numbers numerically, strings lexicographically) — the same comparison `sortBy` uses
— so `arr.sortBy(f)` and `arr.searchBy(k, f)` are a matched pair. Returns the same
`{ found, index }` record as `binarySearch` (leftmost match; insertion point when
absent). The `key` value is left as `Json` because it only needs to be comparable.
Runs in `O(log n)`.

**Precondition:** `arr` must be sorted by `f`'s key (result undefined otherwise).
Generic over `T`: `f` is checked against the array's element type.

```txt
val people = [{ "name": "Alice", "age": 25 }, { "name": "Bob", "age": 30 }, { "name": "Carol", "age": 40 }]
//            ^ already sorted by age
people.searchBy(30, p => p["age"])
// { "found": true, "index": 1 }

people.searchBy(35, p => p["age"])
// { "found": false, "index": 2 }   (would insert between Bob and Carol)

val words = ["apple", "banana", "cherry"]   // sorted lexicographically
words.searchBy("cherry", s => s)
// { "found": true, "index": 2 }
```

---

### insertSorted

```txt
val insertSorted: <T>(arr: T[], item: T, compare: (T, T) -> Int32) -> T[]
```

Returns a **new** array with `item` inserted into the sorted array `arr` at the
position that keeps it sorted by `compare` — i.e. at `lowerBound(arr, item, compare)`,
so `item` is placed *before* any existing elements that compare equal to it (a stable
left-insert, matching Python's `insort_left`). Does not modify `arr`. Runs in
`O(log n)` to find the position plus `O(n)` to build the new array. Generic over `T`:
the comparator is checked against the element type, and the result is a `T[]` that
preserves the element representation.

**Precondition:** `arr` must already be sorted by `compare`. Inserting into an
unsorted array places `item` at an undefined position (the result is not re-sorted).
Repeated `insertSorted` is the way to maintain a running sorted collection without a
full `sort` on every insert.

```txt
[1, 3, 5, 7].insertSorted(4, (a, b) => a - b)    // [1, 3, 4, 5, 7]
[1, 3, 5].insertSorted(0, (a, b) => a - b)        // [0, 1, 3, 5]
[1, 3, 5].insertSorted(9, (a, b) => a - b)        // [1, 3, 5, 9]
[].insertSorted(1, (a, b) => a - b)                // [1]

// maintain a sorted list:
var sorted: Int32[] = []
[5, 2, 8, 1].for(x => sorted = sorted.insertSorted(x, (a, b) => a - b))
// sorted is [1, 2, 5, 8]
```

---

### countInRange

```txt
val countInRange: <T>(arr: T[], lo: T, hi: T, compare: (T, T) -> Int32) -> Int32
```

Returns the number of elements of the sorted array `arr` in the half-open range
`[lo, hi)` — those `x` with `compare(x, lo) >= 0` and `compare(x, hi) < 0`. Computed
as `lowerBound(arr, hi) - lowerBound(arr, lo)`, so it is `O(log n)`, not a scan.
A common analytics primitive (histogram buckets, "values between X and Y"). Generic
over `T`; comparator checked against the element type.

**Precondition:** `arr` must be sorted by `compare` (result undefined otherwise). If
`hi` compares less than `lo` the result is `0`.

```txt
[1, 2, 3, 4, 5, 6, 7].countInRange(3, 6, (a, b) => a - b)   // 3   (3, 4, 5)
[1, 2, 3, 4, 5].countInRange(0, 100, (a, b) => a - b)        // 5
[1, 2, 3, 4, 5].countInRange(3, 3, (a, b) => a - b)          // 0   (empty range)
```

---

## Implementation notes

**No new intrinsics — pure Lin.** Every function here is a small loop (or a tail of
recursive halving) over an indexed array, reading elements with the existing `arr[i]`
indexing and calling the user's comparator. There is nothing the runtime needs to do
that `std/array` cannot already express, so this enrichment adds **zero** runtime
surface. The two bound functions are the only real code; everything else is a thin
wrapper.

**`lowerBound` / `upperBound` are the reusable core.** Both are the standard
half-open binary-search loop over `[lo, hi)` with `lo = 0`, `hi = length(arr)`:

```txt
export val lowerBound = <T>(arr: T[], target: T, compare: (T, T) -> Int32): Int32 =>
  var lo = 0
  var hi = length(arr)
  lin_while_cond(() => lo < hi, () =>     // (or the module's existing loop primitive)
    val mid = lo + (hi - lo) / 2          // avoid (lo + hi) overflow on huge arrays
    if compare(arr[mid], target) < 0 then lo = mid + 1 else hi = mid
  )
  lo
```

`upperBound` is identical except the test is `compare(arr[mid], target) <= 0` (strict
`>` boundary). Use `lo + (hi - lo) / 2` rather than `(lo + hi) / 2` to avoid `Int32`
overflow on very large arrays — the standard binary-search midpoint fix. The loop
form should follow whatever iteration primitive the rest of `array.lin` uses for
index loops (`sort`'s merge helpers are written as tail-recursion over an index, so a
tail-recursive `_bisect` helper parameterised by the `< 0` vs `<= 0` boundary is the
most consistent style — write one `_bisect(arr, target, compare, strict)` and have
both `lowerBound` and `upperBound` call it, mirroring how `sortBy`/`minBy`/`maxBy`
share `_keyedPairs`).

**The rest reduce to the core:**

- `binarySearch(arr, target, compare)` = `val i = lowerBound(arr, target, compare)`,
  then `found = i < length(arr) && compare(arr[i], target) == 0`, returning
  `{ "found": found, "index": i }`. One `lowerBound` call plus one comparison — note
  this naturally yields the **leftmost** match and the insertion point in the same
  `i`, which is exactly why the record convention is cheap to produce.
- `searchBy(arr, key, f)` = `binarySearch` over a synthesized comparator
  `(x, _) => if f(x) < key then -1 else if f(x) > key then 1 else 0`, mirroring how
  `sortBy` builds its `[key, item]` comparison from `f`. Because the comparator only
  ever compares an element's key against the fixed `key` (not two elements), `key`
  can stay `Json` and be captured in the closure — the same one-type-param fallback
  `sortBy` uses (the key need only be `<`/`>`-comparable, which `Json` is).
- `insertSorted(arr, item, compare)` = `val i = lowerBound(arr, item, compare)`, then
  build the result by `slice(arr, 0, i)`, `append(_, item)`-style splicing — or, to
  keep the element representation (flat scalar vs tagged) correct, allocate
  `arrayAllocateFilled(length(arr) + 1, item)` (pinning the buffer element type to
  `T`, exactly as `sort` does) and copy `[0, i)` then `[i, n)` shifted by one. Using
  `arrayAllocateFilled(n+1, item)` rather than `arrayAllocate(n+1)` is **required**
  for the same reason `sort` needs it: a flat-scalar `T` must land in a flat buffer,
  not a tagged `Json` one, or the reads reinterpret 16-byte slots as packed scalars.
- `countInRange(arr, lo, hi, compare)` = `lowerBound(arr, hi, compare) -
  lowerBound(arr, lo, compare)`, clamped to `0`.

**Not-found convention (decided).** `{ found: Boolean, index: Int32 }`, with `index`
carrying the insertion point on a miss and the leftmost match on a hit. Chosen over
`Int32 | Null` (which discards the insertion point, the most useful output) and over
Java's `-(insertion_point) - 1` packing (a decode footgun that cannot distinguish
present-at-0 from absent-insert-at-0). See the design-decisions section above; the
spec throughout assumes this record.

**Generics.** Every function is comparator-driven (`compare: (T, T) -> Int32`) or
key-extractor-driven (`f: (T) -> Json`), so `T` is inferred from the array argument
exactly as for `sort`/`sortBy` and monomorphizes the same way — the comparator/key
closure is specialized to read `T` in its native representation (flat scalar or
tagged). The only allocation is in `insertSorted` (which must use
`arrayAllocateFilled(n+1, item)` to match `arr`'s representation, per the note above);
`binarySearch`/`lowerBound`/`upperBound`/`countInRange`/`searchBy` allocate nothing
beyond the small result record. No RC subtleties beyond reading borrowed elements and
calling the borrowed comparator — the same discipline `sort` already satisfies under
AddressSanitizer.

**Tests** should cover: present / absent / leftmost-of-duplicates / before-first /
past-last for each of `binarySearch`/`lowerBound`/`upperBound`; the
`upperBound - lowerBound = count` identity and `countInRange` against a linear-scan
oracle; `insertSorted` preserving sortedness over a randomized insert sequence and
preserving element representation for a flat `Int32[]` vs a `Json[]`; `searchBy`
paired with `sortBy` on a record array; and the empty-array and single-element edge
cases for all of them.
