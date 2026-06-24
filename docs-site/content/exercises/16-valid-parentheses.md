# 16 · Valid Parentheses

The elf compiler checks that every open bracket in a formula is closed by the right partner
— `(` with `)`, `[` with `]`, `{` with `}`. Brackets can be nested, but must not interleave:
`{[]}` is valid, `([)]` is not.

## Your task

Implement `solve` in `exercises/16-valid-parentheses/exercise.lin`:

```lin
export val solve = (s: String): Boolean => ...
```

Return `true` if every bracket is correctly opened and closed in the right order; `false`
otherwise. An empty string is valid.

## Examples

```lin
solve("()")      // true
solve("()[]{}")  // true
solve("(]")      // false
solve("([)]")    // false
solve("{[]}")    // true
solve("")        // true
solve("(")       // false
```

## Run the test

```bash
lin test docs-site/exercises/16-valid-parentheses/
```

Hints: use a `var stack: String[] = []` as a push-down stack. When you see an opener
(`(`, `[`, `{`), push it. When you see a closer, check if the top of the stack is the
matching opener — if not (or if the stack is empty), return `false`. At the end the stack
must be empty. Import `push`/`slice`/`length` from `std/array` and `at` from `std/string`.
