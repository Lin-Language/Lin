# 25 · Longest Common Subsequence

Two biologists are comparing DNA strands and want to find the longest sequence of nucleotides that appears (not necessarily contiguously) in both strands in the same order. This is the classic Longest Common Subsequence problem.

## Your task

Implement `solve` in `exercises/25-longest-common-subsequence/exercise.lin`:

```lin
export val solve = (a: String, b: String): Int32 => ...
```

Return the length of the longest common subsequence of strings `a` and `b`. A subsequence is formed by deleting zero or more characters without reordering. Return `0` if either string is empty or they share no characters.

## Examples

```lin
solve("abcde", "ace")      // 3
solve("abc", "abc")        // 3
solve("abc", "def")        // 0
solve("", "x")             // 0
solve("AGGTAB", "GXTXAYB") // 4
```

## Run the test

```bash
lin test docs-site/exercises/25-longest-common-subsequence/
```

Hints: use a flat `Int32[]` of size `(m+1)*(n+1)` as a 2-D DP table, where index `i*(n+1)+j` represents `dp[i][j]`. If `a[i-1] == b[j-1]`, then `dp[i][j] = dp[i-1][j-1]+1`; otherwise take the max of `dp[i-1][j]` and `dp[i][j-1]`. Use `at` from `std/string` to access individual characters.
