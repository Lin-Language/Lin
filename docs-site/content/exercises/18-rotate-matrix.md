# 18 · Rotate a Matrix

The elf telescope control system needs to rotate image frames 90° clockwise before
transmitting to the ground station. Your job: take a square pixel grid and return a new
grid with each pixel rotated to its correct clockwise position.

## Your task

Implement `solve` in `exercises/18-rotate-matrix/exercise.lin`:

```lin
export val solve = (m: Int32[][]): Int32[][] => ...
```

Given an N×N matrix (array of rows), return a new N×N matrix rotated 90° clockwise.
Return `[]` for an empty input. Do not mutate the input.

## Examples

```lin
solve([[1, 2], [3, 4]])
  // [[3, 1], [4, 2]]

solve([[1, 2, 3], [4, 5, 6], [7, 8, 9]])
  // [[7, 4, 1], [8, 5, 2], [9, 6, 3]]

solve([[5]])   // [[5]]
solve([])      // []
```

## Run the test

```bash
lin test docs-site/exercises/18-rotate-matrix/
```

Hints: for 90° clockwise rotation, column `c` of the result equals the reversed row `c`
of the input. In other words: `result[c][r] = m[N-1-r][c]`. Build each output row by
iterating over source rows in reverse. Use `range`/`for` from `std/iter` and `push`/`length`
from `std/array`.
