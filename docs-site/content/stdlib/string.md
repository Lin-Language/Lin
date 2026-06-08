# std/string

std/string — string manipulation functions.

All operations are codepoint-aware: indices and lengths count Unicode codepoints, not bytes. The
one exception is `byteAt`, a low-level O(1) raw-UTF-8 byte accessor for fast byte/ASCII scanning.

import { trim, toUpper, toLower, split, join, replace, replaceAll, contains, startsWith, endsWith, substring, indexOf, length } from "std/string"

## Reference

#### `trim`

```lin
val trim = (s: String): String
```

Remove whitespace from both ends of `s`.
- **`s`** — the string to trim.
- **Returns** `s` with leading and trailing whitespace removed.
- **Example:** trim("  hello  ")   // "hello"

#### `trimStart`

```lin
val trimStart = (s: String): String
```

Remove leading whitespace from `s`.
- **`s`** — the string to trim.
- **Returns** `s` with leading whitespace removed.

#### `trimEnd`

```lin
val trimEnd = (s: String): String
```

Remove trailing whitespace from `s`.
- **`s`** — the string to trim.
- **Returns** `s` with trailing whitespace removed.

#### `toUpper`

```lin
val toUpper = (s: String): String
```

Convert `s` to upper case.
- **`s`** — the string to convert.
- **Returns** the upper-cased string.
- **Example:** toUpper("hello")   // "HELLO"

#### `toLower`

```lin
val toLower = (s: String): String
```

Convert `s` to lower case.
- **`s`** — the string to convert.
- **Returns** the lower-cased string.
- **Example:** toLower("HELLO")   // "hello"

#### `substring`

```lin
val substring = (s: String, start: Int32, end: Int32 = _length(s)): String
```

Extract the substring of `s` from `start` (inclusive) to `end` (exclusive).
- **`s`** — the source string.
- **`start`** — the start index (inclusive).
- **`end`** — the end index (exclusive); defaults to the length of `s`.
- **Returns** the substring between `start` and `end`.
- **Example:** substring("hello", 1, 3)    // "el"
- **Example:** substring("hello", 2)       // "llo"   (omitted end defaults to length)
- **Example:** substring("hello", 0, -1)   // "hell"  (strip last char)

#### `indexOf`

```lin
val indexOf = (s: String, needle: String, fromIndex: Int32 = 0): Int32
```

Find the first occurrence of `needle` in `s`, searching from `fromIndex`.
- **`s`** — the string to search.
- **`needle`** — the substring to find.
- **`fromIndex`** — the index to start searching from (default 0).
- **Returns** the index of the first match at or after `fromIndex`, or -1 if not found.
- **Example:** indexOf("hello world", "o")   // 4
- **Example:** indexOf("abcabc", "bc", 2)    // 4   (search at or after fromIndex)

#### `lastIndexOf`

```lin
val lastIndexOf = (s: String, needle: String, fromIndex: Int32 = _length(s)): Int32
```

Find the last occurrence of `needle` in `s`, searching backwards from `fromIndex`.
- **`s`** — the string to search.
- **`needle`** — the substring to find.
- **`fromIndex`** — the index to start searching back from (default the length of `s`).
- **Returns** the index of the last match at or before `fromIndex`, or -1 if not found.
- **Example:** lastIndexOf("hello world", "o")  // 7
- **Example:** lastIndexOf("abcabc", "bc", 2)   // 1   (search at or before fromIndex)

#### `isBlank`

```lin
val isBlank = (s: String): Boolean
```

Test whether `s` is empty or contains only whitespace.
- **`s`** — the string to test.
- **Returns** `true` if `s` is blank, otherwise `false`.

#### `contains`

```lin
val contains = (s: String, needle: String): Boolean
```

Test whether `s` contains `needle` anywhere.
- **`s`** — the string to search.
- **`needle`** — the substring to look for.
- **Returns** `true` if `needle` occurs in `s`, otherwise `false`.
- **Example:** contains("hello world", "world")   // true

#### `startsWith`

```lin
val startsWith = (s: String, prefix: String): Boolean
```

Test whether `s` begins with `prefix`.
- **`s`** — the string to test.
- **`prefix`** — the prefix to check for.
- **Returns** `true` if `s` starts with `prefix`, otherwise `false`.
- **Example:** startsWith("hello", "hel")   // true

#### `endsWith`

```lin
val endsWith = (s: String, suffix: String): Boolean
```

Test whether `s` ends with `suffix`.
- **`s`** — the string to test.
- **`suffix`** — the suffix to check for.
- **Returns** `true` if `s` ends with `suffix`, otherwise `false`.
- **Example:** endsWith("hello", "llo")   // true

#### `split`

```lin
val split = (s: String, delimiter: String): String[]
```

Split `s` into parts on each occurrence of `delimiter`.
- **`s`** — the string to split.
- **`delimiter`** — the separator to split on.
- **Returns** the array of pieces between delimiters.
- **Example:** split("a,b,c", ",")   // ["a", "b", "c"]

#### `join`

```lin
val join = (arr: String[], separator: String): String
```

Concatenate the strings in `arr`, inserting `separator` between adjacent elements.
- **`arr`** — the strings to join.
- **`separator`** — the text placed between elements.
- **Returns** the joined string.
- **Example:** join(["a", "b", "c"], ",")   // "a,b,c"

#### `replace`

```lin
val replace = (s: String, pattern: String, replacement: String): String
```

Replace the first occurrence of `pattern` in `s` with `replacement`.
- **`s`** — the source string.
- **`pattern`** — the substring to replace.
- **`replacement`** — the text to substitute in.
- **Returns** `s` with the first match of `pattern` replaced, or `s` unchanged if `pattern` is absent.
- **Example:** replace("hello world", "world", "Lin")   // "hello Lin"

#### `replaceAll`

```lin
val replaceAll = (s: String, pattern: String, replacement: String): String
```

Replace every occurrence of `pattern` in `s` with `replacement`.
- **`s`** — the source string.
- **`pattern`** — the substring to replace.
- **`replacement`** — the text to substitute in.
- **Returns** `s` with all matches of `pattern` replaced.
- **Example:** replaceAll("aabbcc", "b", "x")   // "aaxxcc"

#### `repeat`

```lin
val repeat = (s: String, count: Int32): String
```

Repeat `s` `count` times.
- **`s`** — the string to repeat.
- **`count`** — the number of copies to concatenate.
- **Returns** the concatenation of `count` copies of `s`.
- **Example:** repeat("-", 5)   // "-----"

#### `at`

```lin
val at = (s: String, index: Int32): String
```

Get the character at `index`. A negative `index` counts back from the end.
- **`s`** — the source string.
- **`index`** — the character index; negative counts from the end.
- **Returns** the single-character string at the (resolved) index.
- **Example:** at("hello", 0)    // "h"
- **Example:** at("hello", -1)   // "o"

#### `charCode`

```lin
val charCode = (s: String, index: Int32): Int32
```

Get the numeric Unicode code point at character index `index` (alias of `codePointAt`). A negative
index counts from the end codepoint-wise (`-1` is the last codepoint); an out-of-range index returns
-1. Codepoint-indexed and therefore O(n) per call — for fast byte/ASCII scanning use `byteAt`.
- **`s`** — the source string.
- **`index`** — the character (code-point) index; negative counts from the end.
- **Returns** the code point value at `index`, or -1 if out of range.
- **Example:** charCode("café", 3)   // 233   (é)
- **Example:** charCode("hi", -1)    // 105   (i, counting from the end)

#### `byteAt`

```lin
val byteAt = (s: String, index: Int32): Int32
```

Get the raw UTF-8 byte at byte-index `index` in O(1). Use this for fast byte/ASCII
scanning: a loop over `0..length(s)` with `byteAt` is O(n), whereas the same loop with
`charCode` (code-point indexed) is O(n²). For ASCII text the two agree.
- **`s`** — the source string.
- **`index`** — the byte index.
- **Returns** the byte value at `index`, or -1 if out of range.
- **Example:** byteAt("ABC", 0)   // 65
- **Example:** byteAt("AB", 5)    // -1

#### `fromCharCode`

```lin
val fromCharCode = (code: Int32): String
```

Build a single-character string from a Unicode code point.
- **`code`** — the code point value.
- **Returns** the one-character string for `code`.
- **Example:** fromCharCode(65)   // "A"

#### `lines`

```lin
val lines = (s: String): String[]
```

Split `s` into its lines on `\n`.
- **`s`** — the source string.
- **Returns** the array of lines.

#### `codePointAt`

```lin
val codePointAt = (s: String, index: Int32): Int32
```

Get the Unicode code point at character index `index`.
- **`s`** — the source string.
- **`index`** — the character (code-point) index.
- **Returns** the code point value at `index`.
- **Example:** codePointAt("A", 0)   // 65

#### `fromCodePoints`

```lin
val fromCodePoints = (codes: Int32[]): String
```

Build a string from an array of Unicode code points.
- **`codes`** — the code point values, in order.
- **Returns** the concatenation of the corresponding characters.

#### `padStart`

```lin
val padStart = (s: String, width: Int32, pad: String = " "): String
```

Left-pad `s` with `pad` until it is at least `width` characters wide.
- **`s`** — the string to pad.
- **`width`** — the minimum target width.
- **`pad`** — the padding string repeated on the left (default a single space).
- **Returns** `s` padded on the left, or `s` unchanged if it is already at least `width` wide.
- **Example:** padStart("42", 5, "0")   // "00042"
- **Example:** padStart("5", 3)         // "  5"    (pad defaults to a space)

#### `padEnd`

```lin
val padEnd = (s: String, width: Int32, pad: String = " "): String
```

Right-pad `s` with `pad` until it is at least `width` characters wide.
- **`s`** — the string to pad.
- **`width`** — the minimum target width.
- **`pad`** — the padding string repeated on the right (default a single space).
- **Returns** `s` padded on the right, or `s` unchanged if it is already at least `width` wide.
- **Example:** padEnd("hi", 5, ".")   // "hi..."

#### `fromUtf8`

```lin
val fromUtf8 = (bytes: UInt8[]): String | Error
```

Decode a `UInt8[]` of UTF-8 bytes into a validated `String`. The inverse of `byteAt`: where
`byteAt` walks a `String`'s raw UTF-8 bytes, `fromUtf8` reassembles a byte buffer back into a
`String`. Unlike `fromCodePoints` (which treats each Int as a code point and would mojibake
multi-byte sequences), this validates and decodes genuine UTF-8 — use it for bytes that came
from a file, socket, or a base64/percent decode.
- **`bytes`** — the UTF-8 byte buffer to decode.
- **Returns** the decoded string, or an `Error` if `bytes` are not well-formed UTF-8.

#### `toString`

```lin
val toString = (x: Json): String
```

Render any JSON value as its string representation.
- **`x`** — the value to stringify.
- **Returns** the string form of `x`.
- **Example:** toString(42)        // "42"
- **Example:** toString(true)      // "true"
- **Example:** toString([1, 2])    // "[1, 2]"
