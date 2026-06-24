# 21 · Longest Substring Without Repeating Characters

You are given a string and must find the length of the longest substring that contains no repeated character. For `"abcabcbb"`, the answer is `3` (`"abc"`); for `"pwwkew"` it is also `3` (`"wke"`).

## Your task

Implement `solve` in `exercises/21-longest-unique-substring/exercise.lin`:

```lin
export val solve = (s: String): Int32 => ...
```

Given a string `s`, return the length of the longest substring with all distinct characters. Return `0` for an empty string.

## Examples

```lin
solve("abcabcbb")   // 3
solve("bbbbb")      // 1
solve("pwwkew")     // 3
solve("")           // 0
solve("abcdef")     // 6
```

## Run the test

```bash
lin test docs-site/exercises/21-longest-unique-substring/
```

Hints: use a sliding window with two indices `left` and `i`. Keep a `{ String: Int32 }` map recording the last-seen index of each character. When you encounter a character you have seen at index `prev`, advance `left` to `prev + 1` if `prev >= left`. Track the maximum window width as you go.
