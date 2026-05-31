# Arrays & Iteration

Lin's array model is built on JSON arrays. The `std/array` module provides a rich set of functional combinators.

## Array literals

```lin
val numbers = [1, 2, 3, 4, 5]
val words = ["apple", "banana", "cherry"]
val empty: Int32[] = []
```

Array types are written `T[]` for an unbounded array of `T`.

## Importing array functions

```lin
import { map, filter, reduce, for, range, find, some, every, sort, sortBy } from "std/array"
```

## `map` — transform each element

```lin
val doubled = [1, 2, 3].map(x => x * 2)
// [2, 4, 6]
```

## `filter` — keep matching elements

```lin
val evens = [1, 2, 3, 4, 5].filter(x => x % 2 == 0)
// [2, 4]
```

## `reduce` — fold to a single value

The accumulator comes first, then each element:

```lin
val total = [1, 2, 3, 4].reduce(0, (sum, x) => sum + x)
// 10

val longest = ["cat", "elephant", "dog"].reduce("", (acc, word) =>
  if length(word) > length(acc) then word else acc
)
// "elephant"
```

## `for` — iterate with side effects

```lin
import { print } from "std/io"
import { for } from "std/array"

[1, 2, 3].for(x => print(x))
```

`for` returns `null`; it is used for side effects.

## `range` — integer ranges

```lin
import { print } from "std/io"
import { range, for, map } from "std/array"

range(0, 5).for(i => print(i))
// 0 1 2 3 4

val squares = range(1, 6).map(i => i * i)
// [1, 4, 9, 16, 25]
```

## Chaining pipelines

Because dot syntax makes the left value the first argument, you can chain operations naturally:

```lin
import { print } from "std/io"
import { map, filter, reduce } from "std/array"

val result = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
  .filter(x => x % 2 == 0)
  .map(x => x * x)
  .reduce(0, (sum, x) => sum + x)

print(result)   // 220
```

## `find` — first matching element

Returns the first element for which the predicate returns true, or `null`:

```lin
val first = [1, 3, 5, 6, 7].find(x => x % 2 == 0)
// 6
```

## `some` and `every`

```lin
[1, 2, 3].some(x => x > 2)    // true
[1, 2, 3].every(x => x > 0)   // true
[1, 2, 3].every(x => x > 1)   // false
```

## `sort` and `sortBy`

`sort` takes a comparator:

```lin
[3, 1, 4, 1, 5].sort((a, b) => a - b)
// [1, 1, 3, 4, 5]
```

`sortBy` takes a key extractor:

```lin
val people = [
  { "name": "Charlie", "age": 35 },
  { "name": "Alice", "age": 30 },
  { "name": "Bob", "age": 25 }
]

val sorted = people.sortBy(p => p["name"])
// Alice, Bob, Charlie
```

## Mutating arrays: `push`

`push` appends to an array in place (one of the few mutating operations):

```lin
val xs = []
xs.push(1)
xs.push(2)
xs.push(3)
// xs is now [1, 2, 3]
```

## `length`

```lin
import { length } from "std/array"

length([1, 2, 3])   // 3
length([])          // 0
```
