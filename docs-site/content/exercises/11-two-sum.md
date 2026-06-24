# 11 · Two Sum

You are given an array of integers and a target sum. Your mission: find the two numbers
that add up to the target and return their indices. The elves have already confirmed that
exactly one solution exists — unless of course it doesn't, in which case return `[]`.

## Your task

Implement `solve` in `exercises/11-two-sum/exercise.lin`:

```lin
export val solve = (xs: Int32[], target: Int32): Int32[] => ...
```

Given an array `xs` and a `target` integer, return `[i, j]` where `i < j` and
`xs[i] + xs[j] == target`. Return `[]` if no such pair exists.

## Examples

```lin
solve([2, 7, 11, 15], 9)   // [0, 1]
solve([3, 2, 4], 6)        // [1, 2]
solve([3, 3], 6)           // [0, 1]
solve([1, 2], 10)          // []
```

## Run the test

```bash
lin test docs-site/exercises/11-two-sum/
```

Hints: a single-pass solution uses a `{ Int32: Int32 }` map from value → index. For each
element, compute `target - x` and check if the map already contains it. If yes, you've
found your pair. If not, store `x → i` and move on.
