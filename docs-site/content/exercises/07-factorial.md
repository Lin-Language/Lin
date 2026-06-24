# 07 · Factorial

Compute `n!` — the product of all integers from 1 up to `n`. By convention
`0! = 1`. You may assume `0 ≤ n ≤ 12` (so the result fits in an `Int32`).
Use a loop, not recursion.

## Your task

Implement `solve` in `exercises/07-factorial/exercise.lin`:

```lin
export val solve = (n: Int32): Int32 => ...
```

Return `n!`. The stub returns `1`, which is accidentally right for `n=0` and
`n=1` but wrong for larger values — a good sign your tests will fail until
you implement the loop.

## Examples

```lin
solve(0)    // 1
solve(1)    // 1
solve(5)    // 120
solve(7)    // 5040
```

## Run the test

```bash
lin test docs-site/exercises/07-factorial/
```

Hints: `range(2, n+1)` from `std/iter` gives you the integers `2..n`. Use
`.for(i => ...)` to iterate and accumulate a `var result`.
