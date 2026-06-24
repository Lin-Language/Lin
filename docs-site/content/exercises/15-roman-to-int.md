# 15 · Roman Numerals to Integer

The Roman elf census uses the old numbering system. To count up the total population you
need to convert each Roman numeral to a plain integer. The Romans invented a compact
trick: when a smaller symbol appears before a larger one, subtract it rather than adding.

## Your task

Implement `solve` in `exercises/15-roman-to-int/exercise.lin`:

```lin
export val solve = (s: String): Int32 => ...
```

Parse a valid Roman numeral string and return its integer value. Symbols: I=1, V=5, X=10,
L=50, C=100, D=500, M=1000. If a symbol's value is less than the next symbol's value,
subtract it; otherwise add it.

## Examples

```lin
solve("III")     // 3
solve("IV")      // 4
solve("IX")      // 9
solve("LVIII")   // 58
solve("MCMXCIV") // 1994
```

## Run the test

```bash
lin test docs-site/exercises/15-roman-to-int/
```

Hints: walk the string with `range(0, length(s))` from `std/iter`, reading each character
with `charAt` from `std/string`. For each position, compare its value to the next symbol's
value to decide add or subtract. `reduce` over the range makes this a clean one-liner.
