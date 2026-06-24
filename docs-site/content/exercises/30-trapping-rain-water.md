# 30 · Trapping Rain Water

After a heavy rain, water pools between the bars of a histogram. Each bar has a given height. Compute how many units of water are trapped in total.

## Your task

Implement `solve` in `exercises/30-trapping-rain-water/exercise.lin`:

```lin
export val solve = (heights: Int32[]): Int32 => ...
```

`heights[i]` is the height of bar `i`. Water above bar `i` is bounded by the minimum of the tallest bar to its left and the tallest bar to its right. Return the total trapped water. Return `0` for an empty array or one with no trapping (e.g. monotone ascending).

## Examples

```lin
solve([0,1,0,2,1,0,1,3,2,1,2,1])   // 6
solve([4,2,0,3,2,5])               // 9
solve([])                          // 0
solve([1,2,3])                     // 0
```

## Run the test

```bash
lin test docs-site/exercises/30-trapping-rain-water/
```

Hints: the two-pointer approach runs in O(n) time and O(1) space. Maintain `left` and `right` pointers at the two ends, and `leftMax`/`rightMax` running maxima. At each step, advance the side with the smaller height: water at that position is `max - height`. Use the condition-only `while(() => ...)` form.
