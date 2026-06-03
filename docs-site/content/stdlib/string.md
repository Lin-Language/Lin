# std/string

String manipulation functions. All operations are codepoint-aware — indices and lengths count Unicode codepoints, not bytes.

```lin
import { trim, toUpper, toLower, split, join, replace, replaceAll, contains, startsWith, endsWith, substring, indexOf, length } from "std/string"
```

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `at` | `(String, Int32) -> String` | Character at index; negative counts from end |
| `byteAt` | `(String, Int32) -> Int32` | O(1) raw UTF-8 byte at byte index, or -1 if out of range |
| `charCode` | `(String, Int32) -> Int32` | Numeric codepoint at index; negative counts from end (alias of `codePointAt`) |
| `codePointAt` | `(String, Int32) -> Int32` | Numeric codepoint at index; negative counts from end |
| `contains` | `(String, String) -> Boolean` | Test whether needle is a substring |
| `endsWith` | `(String, String) -> Boolean` | Test whether string ends with suffix |
| `fromCharCode` | `(Int32) -> String` | Build a one-character string from a codepoint value |
| `fromCodePoints` | `(Int32[]) -> String` | Build string from codepoint values |
| `indexOf` | `(String, String, Int32 = 0) -> Int32` | First occurrence index at or after `fromIndex`, or -1 |
| `isBlank` | `(String) -> Boolean` | True if empty or all whitespace |
| `join` | `(String[], String) -> String` | Join array with separator |
| `lastIndexOf` | `(String, String, Int32 = length(s)) -> Int32` | Last occurrence index at or before `fromIndex`, or -1 |
| `length` | `(String) -> Int32` | Codepoint count |
| `lines` | `(String) -> String[]` | Split into lines |
| `padEnd` | `(String, Int32, String = " ") -> String` | Pad right to width (pad defaults to a space) |
| `padStart` | `(String, Int32, String = " ") -> String` | Pad left to width (pad defaults to a space) |
| `repeat` | `(String, Int32) -> String` | Repeat n times |
| `replace` | `(String, String, String) -> String` | Replace first occurrence |
| `replaceAll` | `(String, String, String) -> String` | Replace all occurrences |
| `split` | `(String, String) -> String[]` | Split by delimiter |
| `startsWith` | `(String, String) -> Boolean` | Test whether string starts with prefix |
| `substring` | `(String, Int32, Int32 = length(s)) -> String` | Slice by codepoint indices (end defaults to length) |
| `toLower` | `(String) -> String` | Convert to lowercase |
| `toString` | `(Json) -> String` | Convert any value to string |
| `toUpper` | `(String) -> String` | Convert to uppercase |
| `trim` | `(String) -> String` | Remove leading/trailing whitespace |
| `trimEnd` | `(String) -> String` | Remove trailing whitespace |
| `trimStart` | `(String) -> String` | Remove leading whitespace |

---

### `split` / `join`

```lin
split("a,b,c", ",")          // ["a", "b", "c"]
join(["a", "b", "c"], ",")   // "a,b,c"
```

---

### `substring`

```lin
substring("hello", 1, 3)    // "el"
substring("hello", 2)       // "llo"   (omitted end defaults to length)
substring("hello", 0, -1)   // "hell"  (strip last char)
```

`end` is optional and defaults to the string length, so `substring(s, start)` returns the slice from `start` to the end. Negative indices count from the end.

---

### `replace` / `replaceAll`

```lin
replace("hello world", "world", "Lin")    // "hello Lin"
replaceAll("aabbcc", "b", "x")            // "aaxxcc"
```

---

### `contains` / `startsWith` / `endsWith`

```lin
contains("hello world", "world")   // true
startsWith("hello", "hel")         // true
endsWith("hello", "llo")           // true
```

---

### `trim` / `trimStart` / `trimEnd`

```lin
trim("  hello  ")       // "hello"
trimStart("  hello  ")  // "hello  "
trimEnd("  hello  ")    // "  hello"
```

---

### `toUpper` / `toLower`

```lin
toUpper("hello")   // "HELLO"
toLower("HELLO")   // "hello"
```

---

### `indexOf` / `lastIndexOf`

```lin
indexOf("hello world", "o")      // 4
indexOf("abcabc", "bc", 2)       // 4   (search at or after fromIndex)
lastIndexOf("hello world", "o")  // 7
lastIndexOf("abcabc", "bc", 2)   // 1   (search at or before fromIndex)
```

Both take an optional `fromIndex`. For `indexOf` it defaults to `0` and the search starts at or after it; for `lastIndexOf` it defaults to the string length and the search ends at or before it.

---

### `length`

```lin
length("hello")   // 5
length("café")    // 4
```

---

### `toString`

```lin
toString(42)        // "42"
toString(true)      // "true"
toString([1, 2])    // "[1, 2]"
toString("hello")   // "hello"
```

---

### `at`

```lin
at("hello", 0)    // "h"
at("hello", -1)   // "o"
```

---

### `repeat`

```lin
repeat("-", 5)   // "-----"
```

---

### `padStart` / `padEnd`

```lin
padStart("42", 5, "0")    // "00042"
padEnd("hi", 5, ".")      // "hi..."
padStart("5", 3)          // "  5"    (pad defaults to a space)
```

The `pad` argument is optional and defaults to a single space `" "`.

---

### `codePointAt` / `charCode` / `fromCharCode`

`codePointAt` (and its alias `charCode`) returns the numeric Unicode codepoint at an index; `fromCharCode` is the inverse for a single codepoint. A negative index counts from the end codepoint-wise: `-1` is the last codepoint, `-2` the second-to-last. An out-of-range index returns `-1`. Both are codepoint-indexed and therefore O(n) per call — for fast byte/ASCII scanning use `byteAt`.

```lin
codePointAt("A", 0)    // 65
charCode("café", 3)    // 233   (é)
charCode("hi", -1)     // 105   (i, counting from the end)
fromCharCode(65)       // "A"
```

---

### `byteAt`

Returns the raw UTF-8 byte (`0..255`) at byte-index `index`, or `-1` if the index is negative or out of range. Unlike `codePointAt` this is O(1) — a direct indexed load — so scanning a whole string (`0..length(s)`) is O(n) rather than O(n²). Use it for tokenizers, parsers, and other byte-level scanning written in Lin.

Unlike `at` and `codePointAt`, `byteAt` does **not** support negative indexing from the end — it is a low-level byte primitive, and a negative index simply returns `-1`. Negative-from-end indexing belongs on the codepoint/element accessors (`at`, `charCode`, `codePointAt`).

```lin
byteAt("ABC", 0)   // 65
byteAt("ABC", 2)   // 67
byteAt("AB", 5)    // -1
```
