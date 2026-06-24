# 22 · Merge Intervals

A calendar system stores events as `[start, end]` pairs. When two events overlap or touch, they should be merged into a single event. Your job is to take a list of such intervals and return the minimal non-overlapping list, sorted by start time.

## Your task

Implement `solve` in `exercises/22-merge-intervals/exercise.lin`:

```lin
export val solve = (intervals: Int32[][]): Int32[][] => ...
```

Each element of `intervals` is a two-element array `[start, end]` (both inclusive). Merge all overlapping or touching intervals and return the result sorted by start ascending. Return `[]` for an empty input.

## Examples

```lin
solve([[1,3],[2,6],[8,10],[15,18]])   // [[1,6],[8,10],[15,18]]
solve([[1,4],[4,5]])                  // [[1,5]]
solve([])                            // []
solve([[1,4],[2,3]])                  // [[1,4]]
```

## Run the test

```bash
lin test docs-site/exercises/22-merge-intervals/
```

Hints: `sortBy(iv => iv[0])` puts intervals in start order. Then sweep left-to-right: track the current merged interval's start and end, and extend it when the next interval overlaps (`nextStart <= curEnd`). Use `std/array`'s `push` to build the result.
