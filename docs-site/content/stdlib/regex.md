# std/regex

std/regex — RE2-style regular expressions (linear time, no backtracking, no ReDoS).

Compile a pattern once into an opaque `Regex` handle, then match, search, replace, or split
against any string. All offsets exposed here are codepoint offsets, since Lin strings are
codepoint-aware.

A `Regex` is a program-lifetime handle: a compiled pattern is never freed, which makes it
freely shareable. Treat it as an opaque value.

The only fault case in the module is an invalid pattern, which surfaces as an `Error` value
from `compile`. Everything downstream of a successfully compiled `Regex` is total: it returns
Null, a Boolean, or an array, and never an Error.

## Reference

#### `Regex`

```lin
type Regex = Json
```


#### `Match`

```lin
type Match = { "text": String, "start": Int32, "end": Int32, "groups": Json, "named": Json }
```

A single match. `groups` is the array of positional capture groups: participating groups are
`String`, and non-participating positional holes are `Null`, so `m["groups"][1]` indexes it
directly. `named` is an object of named capture groups; an absent named group reads as `Null`.

#### `compile`

```lin
val compile = (pattern: String): Regex | Error
```

Compile a pattern into a reusable Regex handle. This is the only fault case in the module.
- **`pattern`** — the RE2 source pattern.
- **Returns** the compiled Regex, or an Error (detect with `is Error`) if the pattern is not valid
         RE2 syntax — an unbalanced group, an unterminated class, or an unsupported
         backreference or lookaround construct.

#### `isMatch`

```lin
val isMatch = (re: Regex, s: String): Boolean
```

Test whether `re` matches anywhere within `s`. Cheapest operation; allocates no Match.
- **`re`** — the compiled pattern.
- **`s`** — the subject string.
- **Returns** true if there is a match anywhere in `s`.

**Example:**

```lin
matches("\\d+", "abc123")  // true
```

#### `find`

```lin
val find = (re: Regex, s: String): Match | Null
```

Find the leftmost match of `re` in `s`.
- **`re`** — the compiled pattern.
- **`s`** — the subject string.
- **Returns** the leftmost Match, or Null if there is no match.

#### `findAll`

```lin
val findAll = (re: Regex, s: String): Match[]
```

Find every non-overlapping match of `re` in `s`, left to right.
- **`re`** — the compiled pattern.
- **`s`** — the subject string.
- **Returns** the matches in order, or an empty array if none.

#### `replace`

```lin
val replace = (re: Regex, s: String, replacement: String): String
```

Replace the first match of `re` in `s`.
- **`re`** — the compiled pattern.
- **`s`** — the subject string.
- **`replacement`** — the replacement template ($1 / ${name} / $$ substitution).
- **Returns** `s` with the first match replaced, or `s` unchanged if there is no match.

#### `replaceAll`

```lin
val replaceAll = (re: Regex, s: String, replacement: String): String
```

Replace every non-overlapping match of `re` in `s`.
- **`re`** — the compiled pattern.
- **`s`** — the subject string.
- **`replacement`** — the replacement template ($1 / ${name} / $$ substitution).
- **Returns** `s` with all matches replaced.

#### `split`

```lin
val split = (re: Regex, s: String): String[]
```

Split `s` around each non-overlapping match of `re`.
- **`re`** — the compiled pattern.
- **`s`** — the subject string.
- **Returns** the pieces between matches; the single-element array `[s]` if the pattern never matches.

#### `matches`

```lin
val matches = (pattern: String, s: String): Boolean | Error
```

Compile `pattern` then test it against `s` in one call. Prefer `compile` + `isMatch` when
matching the same pattern many times.
- **`pattern`** — the RE2 source pattern.
- **`s`** — the subject string.
- **Returns** true if `pattern` matches `s`, or the compile Error if `pattern` is invalid.
