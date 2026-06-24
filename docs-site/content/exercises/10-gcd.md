# 10 · Greatest Common Divisor

Find the largest number that divides both `a` and `b` evenly. Euclid's
algorithm does it elegantly in a loop: repeatedly replace `(a, b)` with
`(b, a mod b)` until `b` reaches zero — the answer is `a`. You may assume
`a, b ≥ 0` and they are not both zero.

## Your task

Implement `solve` in `exercises/10-gcd/exercise.lin`:

```lin
export val solve = (a: Int32, b: Int32): Int32 => ...
```

Return the greatest common divisor of `a` and `b` using a loop.

## Examples

```lin
solve(12, 8)    // 4
solve(17, 5)    // 1
solve(0, 9)     // 9
solve(100, 10)  // 10
```

## Run the test

```bash
lin test docs-site/exercises/10-gcd/
```

Hints: Use `var` for mutable bindings and `while(() => ...)` for the condition-only
loop form. The Euclidean step is: `val t = b; b = a % b; a = t` — repeat until `b == 0`.
