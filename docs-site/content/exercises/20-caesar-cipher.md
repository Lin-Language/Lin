# 20 · Caesar Cipher

Julius Caesar encrypted his battle plans by shifting each letter forward in the alphabet
by a fixed amount. The elf intelligence agency still uses this method (for tradition, they
say). Your job: encode a message with a given shift. Non-letter characters pass through
unchanged.

## Your task

Implement `solve` in `exercises/20-caesar-cipher/exercise.lin`:

```lin
export val solve = (s: String, shift: Int32): String => ...
```

Shift every ASCII letter by `shift` positions, wrapping within its case (`'z'` + 1 → `'a'`,
`'Z'` + 1 → `'A'`). Leave spaces, digits, and punctuation unchanged. `shift` may be ≥ 26
(take `shift % 26`).

## Examples

```lin
solve("abc", 1)             // "bcd"
solve("xyz", 3)             // "abc"
solve("Hello, World!", 5)   // "Mjqqt, Btwqi!"
solve("abc", 0)             // "abc"
solve("abc", 26)            // "abc"
```

## Run the test

```bash
lin test docs-site/exercises/20-caesar-cipher/
```

Hints: use `byteAt(s, i)` from `std/string` to get the raw ASCII byte of each character.
Uppercase letters are bytes 65–90 (`A`–`Z`); lowercase are 97–122 (`a`–`z`). Compute the
shifted code with `((b - base + shift) % 26) + base`, then convert back with `fromCharCode`.
Build the result string with interpolation `"${acc}${c}"` inside `range(0, n).reduce(...)`.
