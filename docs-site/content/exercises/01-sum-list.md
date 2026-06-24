# 01 · Sum a List

The elves need to tally up supplies. Given a list of integers, return their total.
Handle the empty sleigh gracefully — an empty list sums to zero.

## Your task

Implement `solve` in `exercises/01-sum-list/exercise.lin`:

```lin
export val solve = (xs: Int32[]): Int32 => ...
```

Sum all elements of `xs`. Return `0` for an empty list.

## Examples

```lin
solve([1, 2, 3, 4])   // 10
solve([])             // 0
solve([-5, 5])        // 0
solve([42])           // 42
```

## Run the test

```bash
lin test docs-site/exercises/01-sum-list/
```

Hint: `std/iter` exports `reduce` — try `xs.reduce(0, (acc, x) => acc + x)`.
You can also use `std/array`'s `sum` for a one-liner.
