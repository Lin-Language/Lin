# std/regex — design proposal

## Status: proposal

Regular expressions are table stakes for a general-purpose language: log parsing, input
validation, tokenization, search-and-replace. Every mainstream language ships them —
Java (`java.util.regex`), Go (`regexp`), Rust (`regex`), Python (`re`), Node (`RegExp`).
This proposal binds the Rust [`regex`](https://docs.rs/regex) crate, a RE2-style engine
that matches in guaranteed linear time with **no backtracking**. That is a deliberate
no-foot-guns choice that fits Lin's posture: there is no pathological input that can hang
a Lin program in a regex (no ReDoS). The trade-off is explicit and permanent: **the
pattern dialect supports no backreferences and no lookaround** (`\1`, `(?=...)`,
`(?<=...)`, `(?!...)` are rejected at `compile` time). Everything RE2 supports —
character classes, anchors, alternation, repetition, non-capturing groups `(?:...)`,
named captures `(?P<name>...)`, inline flags `(?i)`/`(?m)`/`(?s)`/`(?x)`, Unicode classes
`\p{...}` — is supported.

This unblocks `std/log` parsing, validation helpers, a real `std/template` tokenizer, and
removes a whole category of hand-rolled string scanners across the stdlib and user code.

---

## std/regex

Compile a pattern once into an opaque `Regex` handle, then match, search, replace, or split
against any string. The matching engine is a Rust runtime intrinsic (RE2-style, linear
time, no backtracking, no ReDoS). All offsets exposed to Lin are **codepoint** offsets, to
match Lin's codepoint-aware strings (see [Offsets](#offsets-codepoints-not-bytes)).

Import:

```txt
import { compile, isMatch, find, findAll, replaceAll, split } from "std/regex"
```

`Regex` is an opaque runtime type returned by [`compile`](#compile). It is immutable and
freely shareable. The only fault case in this module is an invalid pattern, which surfaces
as the canonical `Error` value from `compile`; everything downstream of a successfully
compiled `Regex` is total (it returns `Null`, `Boolean`, or arrays — never `Error`).

### The `Match` type

```txt
type Match = {
  "text": String,                  // the whole matched substring
  "start": Int32,                  // codepoint offset of the match start (inclusive)
  "end": Int32,                    // codepoint offset just past the match (exclusive)
  "groups": (String | Null)[],     // positional captures; index 0 is the whole match
  "named": { String: String }      // named captures (?P<name>...); only groups that matched
}
```

`groups[0]` is always the whole match (equal to `text`); `groups[1]` is `$1`, and so on.
A capture group that did not participate in the match is `Null` (a positional hole), so
the array length is stable across matches of the same pattern. `named` contains only the
named groups that actually matched — absent ones are simply not present as keys, so a
missing named group reads back as `Null` via normal object indexing.

### Functions

| Function | Signature | Summary |
| --- | --- | --- |
| [`compile`](#compile) | `(String) -> Regex \| Error` | Compile a pattern; the one fault case is invalid syntax |
| [`isMatch`](#isMatch) | `(Regex, String) -> Boolean` | True if the pattern matches anywhere in the string |
| [`find`](#find) | `(Regex, String) -> Match \| Null` | First match, or `Null` if none |
| [`findAll`](#findAll) | `(Regex, String) -> Match[]` | All non-overlapping matches, left to right |
| [`replace`](#replace) | `(Regex, String, String) -> String` | Replace the first match using `$1`/`${name}` substitution |
| [`replaceAll`](#replaceAll) | `(Regex, String, String) -> String` | Replace every match using `$1`/`${name}` substitution |
| [`split`](#split) | `(Regex, String) -> String[]` | Split the string on every match |
| [`matches`](#matches) | `(String, String) -> Boolean \| Error` | Convenience: compile + `isMatch` in one call |

---

### compile

```txt
val compile: (pattern: String) -> Regex | Error
```

Compiles `pattern` into a reusable `Regex` handle. Returns an `Error` value (detectable
with `is Error`) if the pattern is not valid RE2 syntax — for example an unbalanced group,
an unterminated character class, or a backreference/lookaround construct (which this engine
deliberately does not support). This is the **only** error-producing function in the module;
compile once at the top of a program or module and reuse the handle.

```txt
val ipPart = compile("[0-9]{1,3}")
ipPart is Error                              // false

compile("(unbalanced")                       // { "type": "error", "message": "..." }
compile("(\\w+)\\1")                          // Error: backreferences are not supported

if compile(userPattern) is Error then
  print("bad pattern")
else
  // ...
```

---

### isMatch

```txt
val isMatch: (re: Regex, s: String) -> Boolean
```

Returns `true` if `re` matches anywhere within `s`. This is the cheapest operation — it
stops at the first match and does not allocate a `Match`. Anchor the pattern with `^…$` to
require a full-string match.

```txt
val digits = compile("^[0-9]+$")

digits.isMatch("12345")     // true
digits.isMatch("12a45")     // false
isMatch(digits, "")         // false
```

A loose email-ish validator (anchored, so the whole string must match):

```txt
val email = compile("^[^@\\s]+@[^@\\s]+\\.[^@\\s]+$")

email.isMatch("ada@example.com")   // true
email.isMatch("not-an-email")      // false
email.isMatch("a@b.c d")           // false (the \s rejects the space)
```

---

### find

```txt
val find: (re: Regex, s: String) -> Match | Null
```

Returns the leftmost match of `re` in `s` as a [`Match`](#the-match-type), or `Null` if
there is no match. Returning `Null` (not `Error`) for "no match" follows the Lin convention:
a non-match is an ordinary outcome, not a fault.

```txt
val word = compile("[a-z]+")
val m = word.find("  hello world  ")

m["text"]    // "hello"
m["start"]   // 2
m["end"]     // 7
```

Extracting fields from a log line with named captures:

```txt
val logLine = compile("(?P<ip>\\d+\\.\\d+\\.\\d+\\.\\d+) \\S+ \\S+ \\[(?P<ts>[^\\]]+)\\] \"(?P<method>\\w+) (?P<path>\\S+)")
val line = "127.0.0.1 - - [07/Jun/2026:14:02:11 +0000] \"GET /index.html HTTP/1.1\" 200 1024"

val m = logLine.find(line)
if m == null then
  print("unparseable line")
else
  m["named"]["ip"]      // "127.0.0.1"
  m["named"]["method"]  // "GET"
  m["named"]["path"]    // "/index.html"
  m["groups"][1]        // "127.0.0.1" (same group, positional)
```

---

### findAll

```txt
val findAll: (re: Regex, s: String) -> Match[]
```

Returns every non-overlapping match of `re` in `s`, left to right, as an array of
[`Match`](#the-match-type). Returns an empty array (`[]`) when there are no matches — never
`Null`, so callers can iterate unconditionally.

```txt
val num = compile("-?\\d+")
val ms = num.findAll("a=1, b=-20, c=300")

ms.map((m: Match): String => m["text"])   // ["1", "-20", "300"]
ms.length                                  // 3
```

Pulling all `key=value` pairs out of a query string with captures:

```txt
val pair = compile("(?P<k>\\w+)=(?P<v>[^&]+)")
"a=1&b=two&c=3"
  .findAll(pair)
  .for((m: Match): Null => print("${m["named"]["k"]} -> ${m["named"]["v"]}"))
// a -> 1
// b -> two
// c -> 3
```

---

### replace

```txt
val replace: (re: Regex, s: String, replacement: String) -> String
```

Replaces the **first** match of `re` in `s` with `replacement`, returning the new string.
If there is no match, `s` is returned unchanged. The `replacement` string supports
backreference substitution (see [Substitution syntax](#substitution-syntax)).

```txt
val ws = compile("\\s+")
ws.replace("a   b   c", "_")     // "a_b   c"   (only the first run collapses)
```

---

### replaceAll

```txt
val replaceAll: (re: Regex, s: String, replacement: String) -> String
```

Replaces **every** non-overlapping match of `re` in `s` with `replacement`. If there is no
match, `s` is returned unchanged. `replacement` supports backreference substitution.

#### Substitution syntax

Within `replacement`:

- `$1`, `$2`, … insert the corresponding positional capture group. `$0` inserts the whole
  match.
- `${name}` inserts a named capture group `(?P<name>...)`. The brace form also
  disambiguates a numbered group from following digits: `${1}0` means "group 1 then a
  literal `0`", whereas `$10` means "group 10".
- `$$` inserts a literal `$`.
- A reference to a group that did not match (or does not exist) inserts the empty string.

```txt
val date = compile("(?P<y>\\d{4})-(?P<m>\\d{2})-(?P<d>\\d{2})")

date.replaceAll("2026-06-07", "${d}/${m}/${y}")   // "07/06/2026"
date.replaceAll("2026-06-07", "$3/$2/$1")         // "07/06/2026"  (positional)

val ws = compile("\\s+")
ws.replaceAll("  hello   world  ", " ").trim()    // "hello world"

compile("(\\w+)@(\\w+)")
  .replaceAll("a@b and c@d", "$2.$1")             // "b.a and d.c"
```

---

### split

```txt
val split: (re: Regex, s: String) -> String[]
```

Splits `s` around each non-overlapping match of `re`, returning the pieces between matches.
A leading or trailing match yields an empty string at that end; consecutive matches yield
empty strings between them. If the pattern never matches, the result is the single-element
array `[s]`.

```txt
val ws = compile("\\s+")
ws.split("the   quick\tbrown\nfox")    // ["the", "quick", "brown", "fox"]

compile(",").split("a,b,,c")           // ["a", "b", "", "c"]
compile("x").split("hello")            // ["hello"]
```

Note the difference from `std/string`'s `split`, which splits on a literal delimiter; this
splits on a *pattern*, which is what makes `\s+`-style whitespace splitting possible.

---

### matches

```txt
val matches: (pattern: String, s: String) -> Boolean | Error
```

Convenience wrapper that compiles `pattern` and tests it against `s` in one call. Returns an
`Error` if the pattern is invalid (the compile fault is propagated), otherwise a `Boolean`.
Prefer [`compile`](#compile) + [`isMatch`](#isMatch) when matching the same pattern many
times, since this recompiles on every call.

```txt
matches("^\\d+$", "42")         // true
matches("^\\d+$", "4x2")        // false
matches("(bad", "42")           // { "type": "error", "message": "..." }
```

This is the only convenience function this proposal recommends. A `find`/`findAll`-on-a-
String shortcut is deliberately omitted: those allocate `Match` records and are almost always
used in a loop, where recompiling the pattern each iteration is a real cost — the `Error`
union would also have to leak into the `Match | Null` / `Match[]` return types, muddying the
common path. Compile once, reuse the handle.

---

## Offsets: codepoints, not bytes

Lin strings are codepoint-aware: `std/string.length`, `at`, `substring`, and `indexOf` all
count and index by Unicode codepoint (`byteAt` is the one explicit O(1) byte escape hatch).
For consistency, `Match.start` and `Match.end` are **codepoint** offsets, so that:

```txt
val m = compile("café").find("a café here")
m["start"]                                  // 2  (codepoints)
"a café here".substring(m["start"], m["end"])  // "café"
```

`m["text"].substring(m["start"], m["end"])` always round-trips against the original string,
which would silently corrupt for multi-byte text if offsets were bytes. The cost is that the
runtime intrinsic, which works in UTF-8 bytes internally (as the `regex` crate does), must
translate byte offsets to codepoint offsets before handing a `Match` back to Lin. That
translation is O(n) in the matched prefix; for the overwhelmingly common ASCII case the
runtime can short-circuit (byte offset == codepoint offset) and pay nothing. This is the
right default — correctness over a micro-optimization that only matters for huge non-ASCII
inputs, where the user can drop to `byteAt`-based scanning if they truly need byte offsets.

---

## Implementation notes

### Intrinsics (must be Rust)

The matching engine cannot be expressed in Lin; it is the `regex` crate behind a thin set of
runtime intrinsics, declared in `stdlib/regex.lin` as `import foreign "lin-runtime"`:

```txt
import foreign "lin-runtime"
  val lin_regex_compile:    (String) => Regex          // invalid pattern => Error value
  val lin_regex_is_match:   (Regex, String) => Boolean
  val lin_regex_find:       (Regex, String) => Match | Null
  val lin_regex_find_all:   (Regex, String) => Match[]
  val lin_regex_replace:    (Regex, String, String, Boolean) => String  // last arg = all?
  val lin_regex_split:      (Regex, String) => String[]
```

Everything else is a pure-Lin wrapper (matching the `std/string`, `std/jq`, `std/bytes`
pattern of intrinsic-plus-wrapper):

- `compile`, `isMatch`, `find`, `findAll`, `split` forward 1:1 to their intrinsics.
- `replace` / `replaceAll` are wrappers over a single `lin_regex_replace` intrinsic with a
  `Boolean` "replace all" flag — the `$1`/`${name}` substitution is parsed and applied
  inside the intrinsic (the `regex` crate's `Replacer` already implements exactly this
  syntax, so we expose it verbatim rather than reimplement it in Lin).
- `matches` is `compile` then `isMatch`, propagating the compile `Error`.

`Regex` is registered as an opaque runtime handle type (like `Timer` / `Stream<T>`), backed
by a refcounted `regex::Regex` on the Rust side. It is immutable, so it can be shared across
worker boundaries by reference (cf. the thread-transfer rules in ADR-043) once the runtime
marks it as a deep-copyable/immortal handle; a first cut can keep it main-thread-only and
relax later.

### Building the `Match` record

The intrinsic constructs the `Match` as a boxed `LinObject` with the five fields above. Two
representation points:

- `groups` is a `(String | Null)[]` — a heap array of boxed `String | Null` values, since
  non-participating groups must be a genuine `Null` hole, not `""`. This is an ordinary
  union-element array; no packed/sealed representation applies.
- `named` is a typed `{ String: String }` map built only from the groups that matched. Per
  the std/object absent-key work, indexing an absent key returns `Null`, which is the
  desired behaviour for "this named group didn't participate".

The byte→codepoint offset translation lives entirely in the intrinsic; Lin only ever sees
codepoint `Int32`s.

### Generics / compiler constraints

Nothing in this surface needs generics — every function is monomorphic over concrete
`String` / `Regex` / `Match`. The argument-driven inference and no-turbofish limits
therefore do not bite. The `Match | Null` and `Match[]` return types are concrete unions
over a named record, which the checker already handles (cf. `array.at -> T | Null`). The one
thing to verify at integration time is that the intrinsic returning a freshly-allocated
`Match` (and the `Match[]` array of them) follows the owned-result RC contract — a fresh +1
box per the lin-ir ownership invariants — and is ASan-clean, since record-returning
intrinsics are exactly the class that has produced use-after-free / double-free bugs before.

### Out of scope (deliberate)

- **Backreferences and lookaround.** Not a missing feature — a rejected one. Patterns using
  them fail at `compile`. This is the linear-time guarantee's cost and is the headline design
  decision.
- **Streaming / incremental matching** over `Stream<T>`. Could be a later addition
  (`findAll` over a byte stream); this proposal is string-only.
- **A regex literal syntax** in the language. Patterns are plain `String`s compiled at
  runtime; no `/…/` literal is proposed.
