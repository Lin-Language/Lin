# 14 · Group Anagrams

The elf librarian has a pile of words and needs to file all the anagrams together. Two
words are anagrams when they contain exactly the same letters in any order — "eat", "tea",
and "ate" all belong in the same group. Groups must appear in order of first encounter.

## Your task

Implement `solve` in `exercises/14-group-anagrams/exercise.lin`:

```lin
export val solve = (words: String[]): String[][] => ...
```

Given an array of strings, return a list of groups where each group contains all words that
are anagrams of each other. Groups appear in order of first appearance. Within a group,
words appear in input order.

## Examples

```lin
solve(["eat", "tea", "tan", "ate", "nat", "bat"])
  // [["eat", "tea", "ate"], ["tan", "nat"], ["bat"]]
solve([])    // []
solve(["x"]) // [["x"]]
```

## Run the test

```bash
lin test docs-site/exercises/14-group-anagrams/
```

Hints: sort each word's characters to make a canonical key — `w.split("").sort(...).join("")`.
Use a `{ String: String[] }` map to accumulate groups, and a separate `String[]` to track
key insertion order (so you can emit groups in the right order). Import `split`/`join` from
`std/string` and `sort`/`push` from `std/array`.
