# 03 · Reverse a String

The message came in backwards — flip it around. Given a string, return the
characters in reverse order. No stripping, no case changes: exact reversal.

## Your task

Implement `solve` in `exercises/03-reverse-string/exercise.lin`:

```lin
export val solve = (s: String): String => ...
```

Return `s` with its characters reversed. The empty string reverses to itself.

## Examples

```lin
solve("hello")    // "olleh"
solve("")         // ""
solve("a")        // "a"
solve("abc")      // "cba"
```

## Run the test

```bash
lin test docs-site/exercises/03-reverse-string/
```

Hints: `split(s, "")` from `std/string` gives you an array of single-character strings.
`reverse` from `std/array` reverses an array. `join(arr, "")` from `std/string`
reassembles them.
