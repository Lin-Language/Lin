# 06 · Palindrome Check

Does it read the same backwards? Check whether the string is a palindrome —
exact characters, case-sensitive, no stripping. An empty string and a single
character both count as palindromes.

## Your task

Implement `solve` in `exercises/06-is-palindrome/exercise.lin`:

```lin
export val solve = (s: String): Boolean => ...
```

Return `true` if `s` is identical when reversed, `false` otherwise.

## Examples

```lin
solve("racecar")   // true
solve("hello")     // false
solve("")          // true
solve("a")         // true
solve("ab")        // false
```

## Run the test

```bash
lin test docs-site/exercises/06-is-palindrome/
```

Hints: You already know how to reverse a string from exercise 03. Compare
the original with the reversed version using `==`.
