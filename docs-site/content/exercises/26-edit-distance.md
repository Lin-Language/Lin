# 26 · Edit Distance

A spell-checker needs to suggest the closest dictionary word to a mistyped input. The closeness metric is the Levenshtein edit distance: the minimum number of single-character insertions, deletions, or substitutions required to transform one string into another.

## Your task

Implement `solve` in `exercises/26-edit-distance/exercise.lin`:

```lin
export val solve = (a: String, b: String): Int32 => ...
```

Return the minimum edit distance between strings `a` and `b`. Return `0` for identical strings; return `length(b)` when `a` is empty.

## Examples

```lin
solve("horse", "ros")          // 3
solve("intention", "execution") // 5
solve("", "abc")               // 3
solve("abc", "abc")            // 0
```

## Run the test

```bash
lin test docs-site/exercises/26-edit-distance/
```

Hints: use the same flat 2-D DP array trick as LCS. Initialise row 0 as `dp[0][j] = j` (deleting all of `b`'s prefix) and column 0 as `dp[i][0] = i`. If characters match, copy the diagonal; if not, take `1 + min(delete, insert, substitute)`.
