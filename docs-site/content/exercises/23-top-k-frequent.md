# 23 · Top K Frequent Elements

A search engine logs every query. Given the full log, find the `k` most frequently queried terms. Your function works on integer IDs, but the problem is the same.

## Your task

Implement `solve` in `exercises/23-top-k-frequent/exercise.lin`:

```lin
export val solve = (xs: Int32[], k: Int32): Int32[] => ...
```

Return exactly `k` elements ordered by frequency **descending**. Break ties by value **ascending**. You may assume `k ≤` the number of distinct values.

## Examples

```lin
solve([1,1,1,2,2,3], 2)   // [1,2]
solve([4,4,5,5,6], 2)     // [4,5]
solve([7], 1)             // [7]
solve([1,2,2,3,3], 2)     // [2,3]
```

## Run the test

```bash
lin test docs-site/exercises/23-top-k-frequent/
```

Hints: count frequencies into a `{ Int32: Int32 }` map. Extract the distinct keys with `keys` from `std/object`. Sort with a custom comparator from `std/array`'s `sort`: compare by `-(freqB - freqA)` first, then by value ascending as a tie-break. Finish with `slice(sorted, 0, k)`.
