# 04 · Count the Vowels

How many vowels are hiding in that string? Count every `a`, `e`, `i`, `o`, `u`
— both upper and lower case count. Consonants, spaces, and punctuation are ignored.

## Your task

Implement `solve` in `exercises/04-count-vowels/exercise.lin`:

```lin
export val solve = (s: String): Int32 => ...
```

Return the number of vowels in `s`. The empty string returns `0`.

## Examples

```lin
solve("hello")   // 2
solve("AEIOU")   // 5
solve("xyz")     // 0
solve("")        // 0
```

## Run the test

```bash
lin test docs-site/exercises/04-count-vowels/
```

Hints: `toLower` from `std/string` normalises case. `split(s, "")` gives characters.
`filter` from `std/iter` lets you keep only matching characters. `length` from
`std/array` counts the result.
