# 09 · Keep the Evens

Sift through the list and keep only the even numbers, preserving their original
order. Odd numbers are discarded. Zero counts as even.

## Your task

Implement `solve` in `exercises/09-filter-evens/exercise.lin`:

```lin
export val solve = (xs: Int32[]): Int32[] => ...
```

Return a new array containing only the elements of `xs` that are divisible by 2.

## Examples

```lin
solve([1, 2, 3, 4, 5, 6])   // [2, 4, 6]
solve([1, 3, 5])              // []
solve([])                     // []
solve([2])                    // [2]
```

## Run the test

```bash
lin test docs-site/exercises/09-filter-evens/
```

Hints: `filter` from `std/iter` is exactly what you need. The predicate
`x => x % 2 == 0` tests whether a number is even.
