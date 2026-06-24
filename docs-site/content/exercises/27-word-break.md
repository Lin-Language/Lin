# 27 · Word Break

A text compressor removed all spaces from a sentence. Given the original dictionary of valid words, determine whether the compressed string can be fully decomposed back into dictionary words.

## Your task

Implement `solve` in `exercises/27-word-break/exercise.lin`:

```lin
export val solve = (s: String, dict: String[]): Boolean => ...
```

Return `true` if `s` can be segmented into a space-separated sequence of one or more words from `dict` (words may be reused). An empty string is always segmentable.

## Examples

```lin
solve("leetcode", ["leet","code"])                              // true
solve("applepenapple", ["apple","pen"])                        // true
solve("catsandog", ["cats","dog","sand","and","cat"])           // false
solve("", ["a"])                                               // true
```

## Run the test

```bash
lin test docs-site/exercises/27-word-break/
```

Hints: build a `{ String: Boolean }` set from `dict` for O(1) lookup. Maintain a `Boolean[]` DP array where `dp[i]` means `s[0..i)` can be segmented. Set `dp[0] = true`. For each end index `i`, try every start `j < i`: if `dp[j]` and `substring(s, j, i)` is in the dict, set `dp[i] = true`. Import `substring` from `std/string`.
