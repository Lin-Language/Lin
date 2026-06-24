# 05 · Largest in the List

Scan the list and find the biggest number. But watch out for the empty case —
there is no "largest" in an empty list, so return `null` instead of crashing.

## Your task

Implement `solve` in `exercises/05-max-of/exercise.lin`:

```lin
export val solve = (xs: Int32[]): Int32 | Null => ...
```

Return the maximum element of `xs`, or `null` if `xs` is empty.

## Examples

```lin
solve([3, 1, 4, 1, 5])   // 5
solve([-3, -7, -2])       // -2
solve([])                 // null
solve([9])                // 9
```

## Run the test

```bash
lin test docs-site/exercises/05-max-of/
```

Hints: Check `length(xs) == 0` first (import `length` from `std/array`). Then use
`reduce` from `std/iter` to fold over the array, keeping the running maximum.
