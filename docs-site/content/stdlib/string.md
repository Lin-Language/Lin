# std/string

String manipulation functions. All operations are codepoint-aware — indices and lengths count Unicode codepoints, not bytes.

```lin
import { trim, toUpper, toLower, split, join, replace, replaceAll, contains, startsWith, endsWith, substring, indexOf, length } from "std/string"
```

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `at` | `(String, Int32) -> String` | Character at index; negative counts from end |
| `charCode` | `(String, Int32) -> Int32` | Numeric codepoint at index (alias of `codePointAt`) |
| `codePointAt` | `(String, Int32) -> Int32` | Numeric codepoint at index |
| `contains` | `(String, String) -> Boolean` | Test whether needle is a substring |
| `endsWith` | `(String, String) -> Boolean` | Test whether string ends with suffix |
| `fromCharCode` | `(Int32) -> String` | Build a one-character string from a codepoint value |
| `fromCodePoints` | `(Int32[]) -> String` | Build string from codepoint values |
| `indexOf` | `(String, String) -> Int32` | First occurrence index, or -1 |
| `isBlank` | `(String) -> Boolean` | True if empty or all whitespace |
| `join` | `(String[], String) -> String` | Join array with separator |
| `lastIndexOf` | `(String, String) -> Int32` | Last occurrence index, or -1 |
| `length` | `(String) -> Int32` | Codepoint count |
| `lines` | `(String) -> String[]` | Split into lines |
| `padEnd` | `(String, Int32, String) -> String` | Pad right to width |
| `padStart` | `(String, Int32, String) -> String` | Pad left to width |
| `repeat` | `(String, Int32) -> String` | Repeat n times |
| `replace` | `(String, String, String) -> String` | Replace first occurrence |
| `replaceAll` | `(String, String, String) -> String` | Replace all occurrences |
| `split` | `(String, String) -> String[]` | Split by delimiter |
| `startsWith` | `(String, String) -> Boolean` | Test whether string starts with prefix |
| `substring` | `(String, Int32, Int32) -> String` | Slice by codepoint indices |
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
substring("hello", 0, -1)   // "hell"  (strip last char)
```

Negative indices count from the end.

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
lastIndexOf("hello world", "o")  // 7
```

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
```

---

### `codePointAt` / `charCode` / `fromCharCode`

`codePointAt` (and its alias `charCode`) returns the numeric Unicode codepoint at an index; `fromCharCode` is the inverse for a single codepoint.

```lin
codePointAt("A", 0)    // 65
charCode("café", 3)    // 233   (é)
fromCharCode(65)       // "A"
```
