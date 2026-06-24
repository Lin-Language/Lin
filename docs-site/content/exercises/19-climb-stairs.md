# 19 · Climbing Stairs

The elf courier needs to climb a staircase to deliver the next package. She can take 1 or
2 steps at a time. How many distinct sequences of steps can she take to reach the top?
It turns out this is just the Fibonacci sequence wearing a disguise.

## Your task

Implement `solve` in `exercises/19-climb-stairs/exercise.lin`:

```lin
export val solve = (n: Int32): Int32 => ...
```

Return the number of distinct ways to climb exactly `n` steps, where each move is 1 or 2
steps. `solve(0) = 1` (one way: take no steps). Use a DP loop, not recursion.

## Examples

```lin
solve(0)    // 1
solve(1)    // 1
solve(2)    // 2
solve(3)    // 3
solve(5)    // 8
solve(10)   // 89
```

## Run the test

```bash
lin test docs-site/exercises/19-climb-stairs/
```

Hints: the recurrence is `f(n) = f(n-1) + f(n-2)` with `f(0) = f(1) = 1`. Keep only the
last two values — no need for an array. Use `range(2, n+1).reduce(...)` from `std/iter`
with a small record accumulator `{ "a": ..., "b": ... }`, advancing one Fibonacci step per
iteration.
