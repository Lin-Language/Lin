# 24 · Coin Change

You have an unlimited supply of coins in several denominations. What is the fewest number of coins you need to make a given amount? If it is impossible, return `-1`.

## Your task

Implement `solve` in `exercises/24-coin-change/exercise.lin`:

```lin
export val solve = (coins: Int32[], amount: Int32): Int32 => ...
```

`coins` is a list of available denominations. Each coin may be used any number of times. Return the minimum number of coins whose sum equals `amount`, or `-1` if no combination sums to `amount`. Return `0` when `amount` is `0`.

## Examples

```lin
solve([1,2,5], 11)        // 3  (5+5+1)
solve([2], 3)             // -1
solve([1], 0)             // 0
solve([1,5,10,25], 30)    // 2  (25+5)
```

## Run the test

```bash
lin test docs-site/exercises/24-coin-change/
```

Hints: build a DP array `dp` of length `amount+1`, initialised to a large sentinel value (`amount+1`). Set `dp[0] = 0`. For each amount `i` from `1` to `amount`, try every coin `c`: if `c <= i`, update `dp[i] = min(dp[i], dp[i-c]+1)`. Use `arrayAllocateFilled` and `set` from `std/array`.
