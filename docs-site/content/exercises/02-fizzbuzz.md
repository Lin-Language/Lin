# 02 · FizzBuzz

A classic rite of passage. Return an array of `n` strings where each entry at
position `i` (1-indexed) follows the rule: `"FizzBuzz"` if `i` is divisible by
both 3 and 5, `"Fizz"` if only by 3, `"Buzz"` if only by 5, otherwise the
number as a string.

## Your task

Implement `solve` in `exercises/02-fizzbuzz/exercise.lin`:

```lin
export val solve = (n: Int32): String[] => ...
```

Return an array of exactly `n` strings. `n = 0` returns an empty array.

## Examples

```lin
solve(5)    // ["1", "2", "Fizz", "4", "Buzz"]
solve(15)   // [..., "14", "FizzBuzz"]
solve(0)    // []
```

## Run the test

```bash
lin test docs-site/exercises/02-fizzbuzz/
```

Hints: `range(1, n+1)` from `std/iter` gives you the numbers 1 through n.
Chain `.map(i => ...)` to transform each number. `toString` from `std/string`
converts an integer to its string representation.
