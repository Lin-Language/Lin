# 13 · Merge Two Sorted Arrays

Two convoys of elves are marching in order of arrival time. You need to combine them into
a single convoy, preserving the ascending order. The trick: both convoys are already sorted,
so you can do it in one linear pass — no need to sort afterwards.

## Your task

Implement `solve` in `exercises/13-merge-sorted/exercise.lin`:

```lin
export val solve = (a: Int32[], b: Int32[]): Int32[] => ...
```

Given two ascending-sorted arrays `a` and `b`, return a single ascending-sorted array
containing all elements from both. Keep duplicates. Either input may be empty.

## Examples

```lin
solve([1, 3, 5], [2, 4, 6])   // [1, 2, 3, 4, 5, 6]
solve([], [1, 2])              // [1, 2]
solve([1, 1], [1])             // [1, 1, 1]
solve([5], [])                 // [5]
```

## Run the test

```bash
lin test docs-site/exercises/13-merge-sorted/
```

Hints: maintain two indices `i` and `j` into `a` and `b`. At each step, pick the smaller
front element and advance that pointer. When one array is exhausted, append the remainder
of the other. Use `var` + `while(() => ...)` loops (from `std/iter`) and `push` (from
`std/array`).
