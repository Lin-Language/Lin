# 08 · Word Frequencies

Count how often each word appears. Given a list of strings, return a map
from each unique word to the number of times it occurs in the list.

## Your task

Implement `solve` in `exercises/08-word-frequencies/exercise.lin`:

```lin
export val solve = (words: String[]): { String: Int32 } => ...
```

Return a map where each key is a word from `words` and its value is the count.
An empty list returns an empty map.

## Examples

```lin
solve(["a", "b", "a"])   // { "a": 2, "b": 1 }
solve([])                 // {}
solve(["x"])              // { "x": 1 }
```

## Run the test

```bash
lin test docs-site/exercises/08-word-frequencies/
```

Hints: Declare `var counts: { String: Int32 } = {}` as your accumulator. Use
`.for(w => ...)` to iterate. Read the current count with `counts[w] ?? 0`
(missing key gives `null`, and `?? 0` provides the default).
