# 17 · Run-Length Encoding

The elf archivist needs to compress repetitive data before transmitting. Run-length
encoding is the simplest approach: replace each run of identical characters with the
character followed by its count. It's not ZIP, but it works wonders on snowflake patterns.

## Your task

Implement `solve` in `exercises/17-run-length-encode/exercise.lin`:

```lin
export val solve = (s: String): String => ...
```

Encode consecutive runs of equal characters as `<char><count>`. Every character appears
with its count, including singletons. An empty string encodes to `""`.

## Examples

```lin
solve("aaabbc")   // "a3b2c1"
solve("abc")      // "a1b1c1"
solve("")         // ""
solve("aaaa")     // "a4"
```

## Run the test

```bash
lin test docs-site/exercises/17-run-length-encode/
```

Hints: track the current character and a run counter using `var`. Walk the string with
`range(1, length(s)).for(...)`. On a character change, flush `cur + toString(count)` to
the result and reset. Don't forget to flush the last run after the loop. Import `at` and
`toString` from `std/string`, `length` from `std/array`, and `range`/`for` from `std/iter`.
