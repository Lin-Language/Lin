# 12 · Binary Search

The warehouse inventory system stores items in ascending order. When a scout needs to
locate a specific item, a linear scan would take too long — the shelves stretch for miles.
Your job: write a binary search that finds items in O(log n) time.

## Your task

Implement `solve` in `exercises/12-binary-search/exercise.lin`:

```lin
export val solve = (xs: Int32[], target: Int32): Int32 => ...
```

Given an ascending-sorted array `xs` and a `target`, return the index of `target` in `xs`,
or `-1` if it is not present. Use a `lo`/`hi` loop, not a linear scan.

## Examples

```lin
solve([1, 3, 5, 7, 9], 5)   // 2
solve([1, 3, 5, 7, 9], 6)   // -1
solve([], 1)                 // -1
solve([4], 4)                // 0
```

## Run the test

```bash
lin test docs-site/exercises/12-binary-search/
```

Hints: start with `lo = 0` and `hi = length(xs) - 1`. Compute `mid = (lo + hi) / 2`,
compare `xs[mid]` to the target, and narrow the search window. Use a `while(() => ...)` loop
(the condition-only form imported from `std/iter`).
