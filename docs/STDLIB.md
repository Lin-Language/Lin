# Lin Standard Library Specification

This document specifies the standard library for the Lin language. All modules are importable via the `std/` prefix.

## Index

### Modules

| Module | Description |
| --- | --- |
| [`std/string`](#stdstring) | String manipulation functions |
| [`std/array`](#stdarray) | Array and iterator functions |
| [`std/number`](#stdnumber) | Numeric parsing and conversion functions |
| [`std/bytes`](#stdbytes) | Byte-buffer slicing and endian (de)serialization |
| [`std/math`](#stdmath) | Mathematical functions |
| [`std/object`](#stdobject) | Object introspection functions |
| [`std/json`](#stdjson) | Type-directed JSON decode |
| [`std/hash`](#stdhash) | Stable structural hash of any value |
| [`std/io`](#stdio) | stdin/stdout and terminal input |
| [`std/fs`](#stdfs) | Filesystem read and write |
| [`std/path`](#stdpath) | Path string manipulation |
| [`std/http`](#stdhttp) | HTTP client and server |
| [`std/net`](#stdnet) | UDP and TCP sockets |
| [`std/process`](#stdprocess) | Run and manage external processes |
| [`std/tty`](#stdtty) | Raw terminal mode and key reads |
| [`std/signal`](#stdsignal) | Blocking wait for OS signals |
| [`std/async`](#stdasync) | Async, concurrency and workers |
| [`std/env`](#stdenv) | Environment variables |
| [`std/template`](#stdtemplate) | String template rendering |
| [`std/test`](#stdtest) | Test framework |
| [`std/time`](#stdtime) | Timestamps and timing |

### Functions by module

**std/string**

| Function | Signature | Summary |
| --- | --- | --- |
| [`at`](#at) | `(String, Int32) -> String` | Character at index; negative indices count from end |
| [`codePointAt`](#codePointAt) | `(String, Int32) -> Int32` | Numeric codepoint value at index |
| [`contains`](#contains) | `(String, String) -> Boolean` | Test whether needle is a substring |
| [`endsWith`](#endsWith) | `(String, String) -> Boolean` | Test whether string ends with suffix |
| [`fromCodePoints`](#fromCodePoints) | `(Int32[]) -> String` | Build a string from codepoint values |
| [`indexOf`](#indexOf-string) | `(String, String) -> Int32` | First occurrence of needle, or -1 |
| [`isBlank`](#isBlank) | `(String) -> Boolean` | True if string is empty or all whitespace |
| [`join`](#join) | `(String[], String) -> String` | Join array of strings with separator |
| [`lastIndexOf`](#lastIndexOf) | `(String, String) -> Int32` | Last occurrence of needle, or -1 |
| [`length`](#length-string) | `(String) -> Int32` | Codepoint count |
| [`lines`](#lines-string) | `(String) -> String[]` | Split a string into lines |
| [`padEnd`](#padEnd) | `(String, Int32, String) -> String` | Pad to width on the right |
| [`padStart`](#padStart) | `(String, Int32, String) -> String` | Pad to width on the left |
| [`repeat`](#repeat) | `(String, Int32) -> String` | Repeat a string n times |
| [`replace`](#replace) | `(String, String, String) -> String` | Replace first occurrence |
| [`replaceAll`](#replaceAll) | `(String, String, String) -> String` | Replace all occurrences |
| [`split`](#split) | `(String, String) -> String[]` | Split by delimiter |
| [`startsWith`](#startsWith) | `(String, String) -> Boolean` | Test whether string begins with prefix |
| [`substring`](#substring) | `(String, Int32, Int32) -> String` | Extract a slice by codepoint indices |
| [`toLower`](#toLower) | `(String) -> String` | Convert to lowercase |
| [`toString`](#toString) | `(Json) -> String` | Convert any value to its string representation |
| [`toUpper`](#toUpper) | `(String) -> String` | Convert to uppercase |
| [`trim`](#trim) | `(String) -> String` | Remove leading and trailing whitespace |
| [`trimEnd`](#trimEnd) | `(String) -> String` | Remove trailing whitespace only |
| [`trimStart`](#trimStart) | `(String) -> String` | Remove leading whitespace only |

**std/array**

| Function | Signature | Summary |
| --- | --- | --- |
| [`append`](#append) | `(Json[], Json) -> Json[]` | Non-mutating single-element append |
| [`arrayAllocate`](#arrayAllocate) | `(Int32) -> Json[]` | Allocate an array of n nulls |
| [`arrayAllocateFilled`](#arrayAllocateFilled) | `(Int32, Json) -> Json[]` | Allocate an array of n copies of a fill value |
| [`at`](#at-array) | `<T>(T[], Int32) -> T` | Element at index; negative indices count from end |
| [`chunk`](#chunk) | `(Json[], Int32) -> Json[][]` | Split into n-sized sub-arrays |
| [`compact`](#compact) | `(Json[]) -> Json[]` | Remove null elements |
| [`concat`](#concat) | `(Json[], Json[]) -> Json[]` | Concatenate two arrays |
| [`countBy`](#countBy) | `(Json[], (Json) -> String) -> { ...Int32 }` | Frequency map by key function |
| [`drop`](#drop) | `(Json[], Int32) -> Json[]` | All elements after first n |
| [`dropWhile`](#dropWhile) | `(Json[], (Json) -> Boolean) -> Json[]` | Skip elements while predicate holds |
| [`every`](#every) | `(Json[], (Json) -> Boolean) -> Boolean` | True if all elements match |
| [`filter`](#filter) | `<T>(T[], (T) -> Boolean) -> T[]` | Keep elements matching predicate |
| [`find`](#find) | `(Json[], (Json) -> Boolean) -> Json` | First matching element, or null |
| [`flatMap`](#flatMap) | `(Json[], (Json) -> Json[]) -> Json[]` | Map then flatten one level |
| [`flatten`](#flatten) | `(Json[]) -> Json[]` | Flatten one level of nesting |
| [`for`](#for) | `(Iterable, (Json) -> Json) -> Null` | Iterate over array or iterator |
| [`while`](#while) | `(Json[], (Json) -> Boolean) -> Null` | Iterate, stopping when callback returns false |
| [`groupBy`](#groupBy) | `(Json[], (Json) -> String) -> { ...Json[] }` | Group into object of arrays by key function |
| [`indexOf`](#indexOf-array) | `<T>(T[], T) -> Int32` | First index of value, or -1 |
| [`iter`](#iter) | `(() -> S, (S) -> Boolean, (S) -> S, (S) -> T) -> Iterator` | Build a custom iterator |
| [`iterOf`](#iterOf) | `(Json[]) -> Iterator` | Iterator over an array |
| [`length`](#length-array) | `(Json) -> Int32` | Length of array, string, or object |
| [`map`](#map) | `<T, U>(T[], (T) -> U) -> U[]` | Transform each element |
| [`max`](#max-array) | `(Number[]) -> Number` | Maximum element |
| [`maxBy`](#maxBy) | `(Json[], (Json) -> Number) -> Json` | Element with the largest key |
| [`min`](#min-array) | `(Number[]) -> Number` | Minimum element |
| [`minBy`](#minBy) | `(Json[], (Json) -> Number) -> Json` | Element with the smallest key |
| [`partition`](#partition) | `(Json[], (Json) -> Boolean) -> [Json[], Json[]]` | Split into passing and failing |
| [`prepend`](#prepend) | `(Json[], Json) -> Json[]` | Non-mutating single-element prepend |
| [`product`](#product) | `(Number[]) -> Number` | Product of all elements |
| [`push`](#push) | `(Json[], Json) -> Null` | Append an element to an array in place |
| [`range`](#range) | `(Int32, Int32) -> Iterator` | Integer range `[start, end)`, step 1 |
| [`rangeStep`](#rangeStep) | `(Int32, Int32, Int32) -> Iterator` | Integer range with an explicit (possibly negative) step |
| [`reduce`](#reduce) | `<T, U>(T[], U, (U, T) -> U) -> U` | Fold left with an accumulator |
| [`reverse`](#reverse) | `(Json[]) -> Json[]` | Return a reversed copy |
| [`scan`](#scan) | `(Json[], Json, (Json, Json) -> Json) -> Json[]` | Reduce returning all intermediate values |
| [`set`](#set-array) | `<T>(T[], Int32, T) -> Null` | Set an element by index in place |
| [`slice`](#slice) | `(T[], Int32, Int32) -> T[]` | Sub-buffer copy; preserves element type |
| [`some`](#some) | `(Json[], (Json) -> Boolean) -> Boolean` | True if any element matches |
| [`sort`](#sort) | `(Json[], (Json, Json) -> Int32) -> Json[]` | Return sorted copy using comparator |
| [`sortBy`](#sortBy) | `(Json[], (Json) -> Json) -> Json[]` | Return sorted copy using key extractor |
| [`sum`](#sum) | `(Number[]) -> Number` | Sum all elements |
| [`take`](#take) | `(Json[], Int32) -> Json[]` | First n elements |
| [`takeWhile`](#takeWhile) | `(Json[], (Json) -> Boolean) -> Json[]` | Elements until predicate fails |
| [`unique`](#unique) | `(Json[]) -> Json[]` | Remove duplicate elements (deep equality) |
| [`zip`](#zip) | `(Json[], Json[]) -> [Json, Json][]` | Pair elements by index |

**std/number**

| Function | Signature | Summary |
| --- | --- | --- |
| [`isFloat64`](#isFloat64) | `(String) -> Boolean` | Test whether a string parses as Float64 |
| [`isInt32`](#isInt32) | `(String) -> Boolean` | Test whether a string parses as Int32 |
| [`parseFloat64`](#parseFloat64) | `(String) -> Float64` | Parse decimal string to Float64 |
| [`parseInt32`](#parseInt32) | `(String) -> Int32` | Parse decimal string to Int32 |
| [`toFloat64`](#toFloat64) | `(Int32) -> Float64` | Widen Int32 to Float64 |
| [`toInt32`](#toInt32) | `(Float64) -> Int32` | Truncate float to Int32 |
| [`toUInt8`](#narrowing-casts) | `(UInt64) -> UInt8` | Truncate to an 8-bit unsigned byte |
| [`toInt8`](#narrowing-casts) | `(UInt64) -> Int8` | Truncate to an 8-bit signed byte |
| [`toUInt16`](#narrowing-casts) | `(UInt64) -> UInt16` | Truncate to a 16-bit unsigned int |
| [`toInt16`](#narrowing-casts) | `(UInt64) -> Int16` | Truncate to a 16-bit signed int |
| [`toUInt32`](#narrowing-casts) | `(UInt64) -> UInt32` | Truncate to a 32-bit unsigned int |
| [`toInt64`](#narrowing-casts) | `(UInt64) -> Int64` | Reinterpret to a 64-bit signed int |
| [`toUInt64`](#narrowing-casts) | `(UInt64) -> UInt64` | Identity / reinterpret to 64-bit unsigned int |
| [`tryParseFloat64`](#tryParseFloat64) | `(String) -> Float64 \| Null` | Parse Float64, returning Null on failure |
| [`tryParseInt32`](#tryParseInt32) | `(String) -> Int32 \| Null` | Parse Int32, returning Null on failure |

**std/math**

| Name | Signature | Summary |
| --- | --- | --- |
| [`E`](#E) | `Float64` | Euler's number (2.71828…) |
| [`INFINITY`](#INFINITY) | `Float64` | Positive infinity |
| [`NAN`](#NAN) | `Float64` | Not-a-number sentinel |
| [`PI`](#PI) | `Float64` | Pi (3.14159…) |
| [`abs`](#abs) | `(Number) -> Number` | Absolute value |
| [`acos`](#acos) | `(Float64) -> Float64` | Arc cosine (radians) |
| [`asin`](#asin) | `(Float64) -> Float64` | Arc sine (radians) |
| [`atan`](#atan) | `(Float64) -> Float64` | Arc tangent (radians) |
| [`atan2`](#atan2) | `(Float64, Float64) -> Float64` | Arc tangent of y/x |
| [`ceil`](#ceil) | `(Float64) -> Float64` | Round toward positive infinity |
| [`clamp`](#clamp) | `(Number, Number, Number) -> Number` | Clamp value to `[lo, hi]` |
| [`cos`](#cos) | `(Float64) -> Float64` | Cosine (radians) |
| [`exp`](#exp) | `(Float64) -> Float64` | e raised to the power x |
| [`floor`](#floor) | `(Float64) -> Float64` | Round toward negative infinity |
| [`isFinite`](#isFinite) | `(Float64) -> Boolean` | True if value is neither NaN nor infinite |
| [`isNaN`](#isNaN) | `(Float64) -> Boolean` | True if value is NaN |
| [`log`](#log) | `(Float64) -> Float64` | Natural logarithm |
| [`log10`](#log10) | `(Float64) -> Float64` | Base-10 logarithm |
| [`log2`](#log2) | `(Float64) -> Float64` | Base-2 logarithm |
| [`max`](#max-math) | `(Number, Number) -> Number` | Larger of two scalars |
| [`min`](#min-math) | `(Number, Number) -> Number` | Smaller of two scalars |
| [`pow`](#pow) | `(Float64, Float64) -> Float64` | Base raised to exponent |
| [`random`](#random) | `() -> Float64` | Uniform random number in `[0, 1)` |
| [`round`](#round) | `(Float64) -> Float64` | Round to nearest integer (half-up) |
| [`sign`](#sign) | `(Number) -> Int32` | -1, 0, or 1 |
| [`sin`](#sin) | `(Float64) -> Float64` | Sine (radians) |
| [`sqrt`](#sqrt) | `(Float64) -> Float64` | Square root |
| [`tan`](#tan) | `(Float64) -> Float64` | Tangent (radians) |
| [`toFixed`](#toFixed) | `(Float64, Int32) -> String` | Format float to N decimal places |
| [`trunc`](#trunc) | `(Float64) -> Float64` | Round toward zero |

**std/object**

| Function | Signature | Summary |
| --- | --- | --- |
| [`entries`](#entries) | `(Json) -> [String, Json][]` | Array of `[key, value]` pairs |
| [`fromEntries`](#fromEntries) | `([String, Json][]) -> {}` | Build an object from key-value pairs |
| [`isEmpty`](#isEmpty) | `(Json) -> Boolean` | True if object, array, or string is empty |
| [`keys`](#keys) | `(Json) -> String[]` | Array of object keys |
| [`mapValues`](#mapValues) | `({}, (Json) -> Json) -> {}` | Transform all values, keeping keys |
| [`merge`](#merge) | `({}, {}) -> {}` | Shallow-merge two objects (right wins on conflict) |
| [`omit`](#omit) | `({}, String[]) -> {}` | Return object without specified keys |
| [`pick`](#pick) | `({}, String[]) -> {}` | Return object with only specified keys |
| [`values`](#values) | `(Json) -> Json[]` | Array of object values |

**std/io**

| Function | Signature | Summary |
| --- | --- | --- |
| [`args`](#args) | `() -> String[]` | Command-line arguments (argv after the script name) |
| [`exit`](#exit) | `(Int32) -> Null` | Terminate the process with an exit code |
| [`lines`](#lines-io) | `() -> Iterator` | Iterator over stdin lines |
| [`print`](#print) | `(Json) -> Null` | Write a value to stdout |
| [`printErr`](#printErr) | `(Json) -> Null` | Write a value to stderr |
| [`prompt`](#prompt) | `(String) -> String \| Null` | Print a message then read one line from stdin |
| [`readAll`](#readAll) | `() -> String` | Read all of stdin as one string |
| [`readLine`](#readLine) | `() -> String \| Null` | Read one line from stdin, or Null on EOF |

**std/fs**

| Function | Signature | Summary |
| --- | --- | --- |
| [`appendFile`](#appendFile) | `(String, String) -> Null \| Error` | Append string to end of file |
| [`cp`](#cp) | `(String, String) -> Null \| Error` | Copy a file |
| [`exists`](#exists) | `(String) -> Boolean` | Test whether a file or directory exists |
| [`isDir`](#isDir) | `(String) -> Boolean` | True if path is a directory |
| [`isFile`](#isFile) | `(String) -> Boolean` | True if path is a regular file |
| [`ls`](#ls) | `(String, Json) -> String[] \| Error` | List directory entry names; supports `{ recursive }` |
| [`mkdir`](#mkdir) | `(String, Json) -> Null \| Error` | Create a directory; supports `{ parents }` option |
| [`mv`](#mv) | `(String, String) -> Null \| Error` | Move or rename a file |
| [`readFile`](#readFile) | `(String) -> String \| Error` | Read entire file as a string |
| [`readFileBytes`](#readFileBytes) | `(String) -> UInt8[] \| Error` | Read file as a raw byte buffer |
| [`readJson`](#readJson) | `(String) -> Json \| Error` | Read and parse file as JSON |
| [`readLines`](#readLines) | `(String) -> String[] \| Error` | Read lines of a file into an array |
| [`rm`](#rm) | `(String, Json) -> Null \| Error` | Remove a file or directory; supports `{ recursive }` |
| [`stat`](#stat) | `(String) -> FileStat \| Error` | File metadata |
| [`writeFile`](#writeFile) | `(String, String) -> Null \| Error` | Write string to file, replacing contents |
| [`writeFileBytes`](#writeFileBytes) | `(String, UInt8[]) -> Null \| Error` | Write a raw byte buffer to file |
| [`writeJson`](#writeJson) | `(String, Json, Json) -> Null \| Error` | Serialise value to pretty JSON; supports `{ compact }` option |
| [`writeLines`](#writeLines) | `(String, String[]) -> Null \| Error` | Write an array of strings, one per line |

**std/path**

| Function | Signature | Summary |
| --- | --- | --- |
| [`basename`](#basename) | `(String) -> String` | Final component of a path |
| [`dirname`](#dirname) | `(String) -> String` | All components except the last |
| [`extname`](#extname) | `(String) -> String` | File extension including dot, or `""` |
| [`isAbsolute`](#isAbsolute) | `(String) -> Boolean` | True if path starts from root |
| [`join`](#join-path) | `(String[]) -> String` | Join path segments with the OS separator |
| [`normalize`](#normalize) | `(String) -> String` | Resolve `..` and `.` segments |
| [`relative`](#relative) | `(String, String) -> String` | Relative path from one location to another |
| [`resolve`](#resolve) | `(String) -> String` | Resolve to an absolute path using cwd |
| [`split`](#split-path) | `(String) -> String[]` | Split a path into its components |
| [`stem`](#stem) | `(String) -> String` | Basename without the extension |

**std/http** — client

| Function | Signature | Summary |
| --- | --- | --- |
| [`fetch`](#fetch) | `(String) -> HttpResponse \| Error` | GET a URL |
| [`fetchJson`](#fetchJson) | `(String) -> Json \| Error` | GET a URL and parse the body as JSON |
| [`fetchWith`](#fetchWith) | `(String, HttpOptions) -> HttpResponse \| Error` | Request with custom method, headers, body |
| [`postJson`](#postJson) | `(String, Json) -> HttpResponse \| Error` | POST a JSON body to a URL |

**std/http** — server

| Function | Signature | Summary |
| --- | --- | --- |
| [`badRequest`](#badRequest) | `(String) -> HttpResponse` | Build a 400 response with a message |
| [`json`](#json-helper) | `(Int32, Json) -> HttpResponse` | Build a JSON response |
| [`notFound`](#notFound) | `HttpResponse` | 404 response value |
| [`parseBody`](#parseBody) | `(HttpRequest) -> Json \| Error` | Parse the request body as JSON |
| [`matchPath`](#matchPath) | `(String, String) -> { ...String } \| Null` | Match a path against a pattern, returning captured params |
| [`redirect`](#redirect) | `(String) -> HttpResponse` | Build a 302 redirect response |
| [`serve`](#serve) | `((HttpRequest) -> HttpResponse, Int32) -> Null` | Start a sequential HTTP server |
| [`text`](#text-helper) | `(Int32, String) -> HttpResponse` | Build a plain-text response |

**std/async**

| Function | Signature | Summary |
| --- | --- | --- |
| [`async`](#async) | `(() -> T) -> Promise` | Run a thunk asynchronously |
| [`await`](#await) | `<T>(T) -> T \| Error` | Block until a promise resolves; result must handle `Error` |
| [`close`](#close) | `(Worker) -> Null` | Shut down a worker |
| [`frozen`](#frozen) | `<T>(T) -> T` | Deep-freeze a value into lock-free shared read-only state |
| [`get`](#shared--get--set--withlock) | `<T>(Shared<T>) -> T` | Read a snapshot copy out of a `Shared` |
| [`message`](#message) | `(Worker, Msg) -> Null` | Send a fire-and-forget message to a worker |
| [`parallel`](#parallel) | `((() -> T)[]) -> T[]` | Run an array of thunks concurrently, collect results |
| [`poolAsync`](#poolAsync) | `(ThreadPool, () -> T) -> Promise` | Enqueue a thunk on a thread pool |
| [`race`](#race) | `(Promise[]) -> T` | Resolve with the first promise to complete |
| [`request`](#request) | `(Worker, Msg) -> Reply` | Send a request to a worker and wait for reply |
| [`retry`](#retry) | `(() -> T, Int32) -> T` | Retry a thunk up to n times on failure |
| [`set`](#shared--get--set--withlock) | `<T>(Shared<T>, T) -> Null` | Replace a `Shared`'s value |
| [`shared`](#shared--get--set--withlock) | `<T>(T) -> Shared<T>` | Create opt-in shared mutable state |
| [`threadPool`](#threadPool) | `(Int32) -> ThreadPool` | Create a thread pool with n workers |
| [`timeout`](#timeout) | `(Promise, Int32) -> T` | Add a millisecond timeout to a promise |
| [`withLock`](#shared--get--set--withlock) | `<T, R>(Shared<T>, (T) -> R) -> R` | Atomic read-modify-write on a `Shared` |
| [`worker`](#worker) | `((Msg) -> Reply, () -> Null) -> Worker` | Create a background worker |

**std/env**

| Function | Signature | Summary |
| --- | --- | --- |
| [`environ`](#environ) | `() -> { ...String }` | All environment variables as an object |
| [`getEnv`](#getEnv) | `(String) -> String \| Null` | Value of an environment variable, or Null |
| [`setEnv`](#setEnv) | `(String, String) -> Null` | Set an environment variable for the current process |
| [`unsetEnv`](#unsetEnv) | `(String) -> Null` | Unset an environment variable |

**std/process**

| Function | Signature | Summary |
| --- | --- | --- |
| [`exec`](#exec) | `(String, String[]) -> ExecResult \| Error` | Run a command to completion, collect output |
| [`shell`](#shell) | `(String) -> ExecResult \| Error` | Run a shell command string via `/bin/sh -c` |
| [`cwd`](#cwd) | `() -> String` | Current working directory |
| [`chdir`](#chdir) | `(String) -> Null \| Error` | Change working directory |
| [`spawn`](#spawn) | `(String, String[]) -> ProcessHandle \| Error` | Start a process without waiting |
| [`readStdout`](#readStdout) | `(ProcessHandle, UInt8[]) -> Int32 \| Error` | Read piped stdout into a buffer (0 = EOF) |
| [`kill`](#kill) | `(ProcessHandle) -> Null \| Error` | Send SIGTERM to a spawned process |
| [`wait`](#wait) | `(ProcessHandle) -> Int32 \| Error` | Wait for a spawned process; returns exit code |

**std/template**

| Function | Signature | Summary |
| --- | --- | --- |
| [`render`](#render) | `(String, {}) -> String \| Error` | Load a `.lint` file and render it with a data record |
| [`renderWith`](#renderWith) | `(String, {}) -> String` | Render a template string with a data record |

**std/test**

| Name | Signature | Summary |
| --- | --- | --- |
| [`expect`](#expect) | `(Json) -> Asserter` | Begin an assertion chain |
| [`run`](#run-test) | `(Suite[]) -> Null` | Execute suites, print results, exit non-zero on failure |
| [`suite`](#suite) | `(String, Test[]) -> Suite` | Group tests under a name |
| [`test`](#test) | `(String, () -> Assertion[]) -> Test` | Declare a single test case |

**std/time**

| Function | Signature | Summary |
| --- | --- | --- |
| [`elapsed`](#elapsed) | `(Timer) -> Int64` | Milliseconds since a timer was started |
| [`format`](#format-time) | `(Int64, String) -> String` | Format a timestamp using a strftime-style pattern |
| [`fromIso`](#fromIso) | `(String) -> Int64 \| Error` | Parse an ISO 8601 string to a millisecond timestamp |
| [`now`](#now) | `() -> Int64` | Current Unix timestamp in milliseconds |
| [`parse`](#parse-time) | `(String, String) -> Int64 \| Error` | Parse a date string with a format pattern |
| [`sleep`](#sleep) | `(Int64) -> Null` | Block for n milliseconds |
| [`sleepMicros`](#sleepMicros) | `(Int64) -> Null` | Block for n microseconds |
| [`startTimer`](#startTimer) | `() -> Timer` | Start a high-resolution elapsed timer |
| [`toIso`](#toIso) | `(Int64) -> String` | Format a timestamp as ISO 8601 |

---

## std/string

String operations are codepoint-aware. All indices and lengths count Unicode codepoints, not bytes.

Import:

```txt
import { trim, toUpper, indexOf } from "std/string"
```

---

### at

```txt
val at: (s: String, index: Int32) -> String
```

Returns the single-codepoint string at `index`. Negative indices count from the end: `-1` is the last character, `-2` is second-to-last. If the resolved index is out of bounds, returns `""`.

```txt
at("hello", 0)    // "h"
at("hello", -1)   // "o"
at("hello", -2)   // "l"
```

---

### codePointAt

```txt
val codePointAt: (s: String, index: Int32) -> Int32
```

Returns the numeric Unicode codepoint value of the character at `index`. Negative indices count from the end. If the resolved index is out of bounds, returns `-1`.

```txt
codePointAt("A", 0)      // 65
codePointAt("café", 3)   // 233   (é)
codePointAt("hi", -1)    // 105   (i)
```

---

### contains

```txt
val contains: (s: String, needle: String) -> Boolean
```

Returns `true` if `needle` appears anywhere within `s`.

```txt
contains("hello world", "world")   // true
contains("hello", "xyz")           // false
```

---

### endsWith

```txt
val endsWith: (s: String, suffix: String) -> Boolean
```

Returns `true` if `s` ends with `suffix`.

```txt
endsWith("hello", "llo")   // true
endsWith("hello", "hel")   // false
```

---

### fromCodePoints

```txt
val fromCodePoints: (codepoints: Int32[]) -> String
```

Builds a string from an array of Unicode codepoint values. This is the inverse of applying `codePointAt` to each index.

```txt
fromCodePoints([72, 101, 108, 108, 111])   // "Hello"
fromCodePoints([233])                       // "é"
fromCodePoints([])                          // ""
```

---

### indexOf (string) {#indexOf-string}

```txt
val indexOf: (s: String, needle: String) -> Int32
```

Returns the zero-based codepoint index of the first occurrence of `needle` within `s`, or `-1` if not found.

```txt
indexOf("hello world", "world")   // 6
indexOf("hello", "xyz")           // -1
```

---

### isBlank

```txt
val isBlank: (s: String) -> Boolean
```

Returns `true` if `s` is empty or contains only whitespace characters.

```txt
isBlank("")         // true
isBlank("  \t\n")   // true
isBlank("  hi  ")   // false
```

---

### join

```txt
val join: (arr: String[], separator: String) -> String
```

Concatenates the elements of `arr` into a single string, with `separator` inserted between each pair.

```txt
join(["a", "b", "c"], ",")   // "a,b,c"
join([], "-")                 // ""
```

---

### lastIndexOf

```txt
val lastIndexOf: (s: String, needle: String) -> Int32
```

Returns the zero-based codepoint index of the **last** occurrence of `needle` within `s`, or `-1` if not found.

```txt
lastIndexOf("abcabc", "b")         // 4
lastIndexOf("/usr/local/bin", "/")  // 10
lastIndexOf("hello", "xyz")        // -1
```

---

### length (string) {#length-string}

```txt
val length: (s: String) -> Int32
```

Returns the number of Unicode codepoints in `s`.

```txt
length("hello")   // 5
length("café")    // 4
```

---

### lines (string) {#lines-string}

```txt
val lines: (s: String) -> String[]
```

Splits `s` into an array of lines. Lines are separated by `\n`, `\r\n`, or `\r`. The line terminators are not included in the results.

```txt
lines("a\nb\nc")     // ["a", "b", "c"]
lines("a\r\nb")      // ["a", "b"]
lines("")            // [""]
```

---

### padEnd

```txt
val padEnd: (s: String, width: Int32, pad: String) -> String
```

Returns `s` padded on the right with repetitions of `pad` until the total codepoint length reaches `width`. If `s` is already at least `width` codepoints long, returns `s` unchanged. `pad` defaults to `" "` if empty.

```txt
padEnd("hi", 5, ".")    // "hi..."
padEnd("hi", 5, "-*")   // "hi-*-"
padEnd("hello", 3, ".")  // "hello"
```

---

### padStart

```txt
val padStart: (s: String, width: Int32, pad: String) -> String
```

Returns `s` padded on the left with repetitions of `pad` until the total codepoint length reaches `width`. If `s` is already at least `width` codepoints long, returns `s` unchanged. `pad` defaults to `" "` if empty.

```txt
padStart("42", 5, "0")    // "00042"
padStart("hi", 5, ".")    // "...hi"
padStart("hello", 3, ".")  // "hello"
```

---

### repeat

```txt
val repeat: (s: String, count: Int32) -> String
```

Returns a string consisting of `s` repeated `count` times. If `count` is `0`, returns `""`.

```txt
repeat("ab", 3)   // "ababab"
repeat("-", 5)    // "-----"
```

---

### replace

```txt
val replace: (s: String, pattern: String, replacement: String) -> String
```

Returns a copy of `s` with the **first** occurrence of `pattern` replaced by `replacement`.

```txt
replace("hello world", "world", "Lin")   // "hello Lin"
replace("aaa", "a", "b")                 // "baa"
```

---

### replaceAll

```txt
val replaceAll: (s: String, pattern: String, replacement: String) -> String
```

Returns a copy of `s` with **every** occurrence of `pattern` replaced by `replacement`.

```txt
replaceAll("aaa", "a", "b")              // "bbb"
replaceAll("hello world", "l", "r")      // "herro worrd"
replaceAll("no match", "xyz", "abc")     // "no match"
```

---

### split

```txt
val split: (s: String, delimiter: String) -> String[]
```

Splits `s` at each occurrence of `delimiter` and returns the resulting parts as an array.

```txt
split("a,b,c", ",")   // ["a", "b", "c"]
split("hello", "x")   // ["hello"]
```

---

### startsWith

```txt
val startsWith: (s: String, prefix: String) -> Boolean
```

Returns `true` if `s` begins with `prefix`.

```txt
startsWith("hello", "hel")   // true
startsWith("hello", "llo")   // false
```

---

### substring

```txt
val substring: (s: String, start: Int32, end: Int32) -> String
```

Returns the slice of `s` covering codepoint indices `[start, end)`. Negative indices count from the end: `-1` refers to one past the last character (equivalent to `length(s)`), `-2` to the last character, etc. If `end` exceeds the codepoint count it is clamped. If `start >= end` (after resolving negatives), returns `""`.

```txt
substring("hello", 1, 3)    // "el"
substring("hello", 0, 5)    // "hello"
substring("hello", 0, -1)   // "hell"   (strip last character)
substring("hello", 1, -1)   // "ell"
```

---

### toLower

```txt
val toLower: (s: String) -> String
```

Returns a copy of `s` with every codepoint mapped to its Unicode lowercase equivalent.

```txt
toLower("HELLO")   // "hello"
toLower("CAFÉ")    // "café"
```

---

### toString

```txt
val toString: (value: Json) -> String
```

Converts any value to its string representation. Strings are returned as-is. Numbers, booleans, `null`, arrays, and objects are formatted as JSON.

```txt
toString(42)           // "42"
toString(true)         // "true"
toString([1, 2])       // "[1, 2]"
toString("hello")      // "hello"
```

---

### toUpper

```txt
val toUpper: (s: String) -> String
```

Returns a copy of `s` with every codepoint mapped to its Unicode uppercase equivalent.

```txt
toUpper("hello")   // "HELLO"
toUpper("café")    // "CAFÉ"
```

---

### trim

```txt
val trim: (s: String) -> String
```

Returns a copy of `s` with all leading and trailing ASCII whitespace characters (`' '`, `'\t'`, `'\n'`, `'\r'`) removed.

```txt
trim("  hello  ")   // "hello"
trim("\t\n")        // ""
```

---

### trimEnd

```txt
val trimEnd: (s: String) -> String
```

Returns a copy of `s` with all trailing ASCII whitespace characters removed. Leading whitespace is preserved.

```txt
trimEnd("  hello  ")   // "  hello"
trimEnd("line\n")      // "line"
```

---

### trimStart

```txt
val trimStart: (s: String) -> String
```

Returns a copy of `s` with all leading ASCII whitespace characters removed. Trailing whitespace is preserved.

```txt
trimStart("  hello  ")   // "hello  "
trimStart("\t\ndata")    // "data"
```

---

## std/array

Array and iterator functions. All transformation functions are non-mutating and return new values.

Import:

```txt
import { map, filter, for, range } from "std/array"
```

---

### append

```txt
val append: (arr: Json[], item: Json) -> Json[]
```

Returns a new array with `item` added at the end. Does not modify `arr`. For in-place mutation, use `push`. The result preserves the input's element representation: appending to a flat scalar array (e.g. `UInt8[]`, `Int32[]`) yields a flat array of the same type (the item is coerced into the element type), so byte-level consumers still read packed bytes; a `Json[]` stays tagged.

```txt
append([1, 2], 3)    // [1, 2, 3]
append([], "hello")  // ["hello"]
```

---

### arrayAllocate

```txt
val arrayAllocate: (n: Int32) -> Json[]
```

Returns a new array of length `n` with every element initialised to `null`. Useful as a
pre-sized buffer to fill by index with [`set`](#set-array).

```txt
val buf = arrayAllocate(3)   // [null, null, null]
set(buf, 0, "a")
```

---

### arrayAllocateFilled

```txt
val arrayAllocateFilled: (n: Int32, fill: Json) -> Json[]
```

Returns a new array of length `n` with every element set to `fill`. When `fill` is a heap value (array, object, or string), the elements share the same value (each slot reads it back equal); replace a slot wholesale with [`set`](#set-array) to give it a distinct value.

```txt
arrayAllocateFilled(3, 0)        // [0, 0, 0]
arrayAllocateFilled(2, "x")      // ["x", "x"]
arrayAllocateFilled(3, [1, 2])   // [[1, 2], [1, 2], [1, 2]]
arrayAllocateFilled(0, 9)        // []
```

---

### at (array) {#at-array}

```txt
val at: <T>(arr: T[], index: Int32) -> T
```

Returns the element at `index`. Negative indices count from the end: `-1` is the last element, `-2` is second-to-last. If the resolved index is out of bounds, returns `null`.

```txt
at([10, 20, 30], 0)    // 10
at([10, 20, 30], -1)   // 30
at([10, 20, 30], -2)   // 20
at([], 0)              // null
```

---

### chunk

```txt
val chunk: (arr: Json[], size: Int32) -> Json[][]
```

Splits `arr` into sub-arrays of length `size`. The final chunk may be shorter if `arr` does not divide evenly. `size` must be at least 1.

```txt
chunk([1, 2, 3, 4, 5], 2)   // [[1, 2], [3, 4], [5]]
chunk([1, 2, 3], 3)          // [[1, 2, 3]]
chunk([], 2)                  // []
```

---

### compact

```txt
val compact: (arr: Json[]) -> Json[]
```

Returns a new array with all `null` elements removed.

```txt
compact([1, null, 2, null, 3])   // [1, 2, 3]
compact([null, null])             // []
compact([1, 2, 3])               // [1, 2, 3]
```

---

### concat

```txt
val concat: (a: Json[], b: Json[]) -> Json[]
```

Returns a new array containing all elements of `a` followed by all elements of `b`.

```txt
concat([1, 2], [3, 4])   // [1, 2, 3, 4]
concat([], [1])           // [1]
```

---

### countBy

```txt
val countBy: (arr: Json[], f: (Json) -> String) -> { ...Int32 }
```

Returns an object mapping each distinct key (produced by `f`) to the number of elements that produced that key.

```txt
["apple", "banana", "avocado", "blueberry"].countBy(s => s.at(0))
// { "a": 2, "b": 2 }

[1, 2, 3, 4, 5].countBy(n => if n % 2 == 0 then "even" else "odd")
// { "odd": 3, "even": 2 }
```

---

### drop

```txt
val drop: (arr: Json[], n: Int32) -> Json[]
```

Returns a new array with the first `n` elements removed. If `n >= length(arr)`, returns `[]`.

```txt
drop([1, 2, 3, 4], 2)   // [3, 4]
drop([1, 2], 5)          // []
drop([1, 2, 3], 0)       // [1, 2, 3]
```

---

### dropWhile

```txt
val dropWhile: (arr: Json[], f: (Json) -> Boolean) -> Json[]
```

Returns a new array with leading elements removed while `f` returns `true`. As soon as `f` returns `false`, the remaining elements are kept unchanged.

```txt
[1, 2, 3, 4, 1].dropWhile(x => x < 3)   // [3, 4, 1]
[1, 2, 3].dropWhile(x => x > 0)          // []
[1, 2, 3].dropWhile(x => x > 9)          // [1, 2, 3]
```

---

### every

```txt
val every: (arr: Json[], f: (Json) -> Boolean) -> Boolean
```

Returns `true` if `f` returns `true` for every element. Returns `true` for an empty array.

```txt
[1, 2, 3].every(x => x > 0)   // true
[1, 2, 3].every(x => x > 1)   // false
```

---

### filter

```txt
val filter: <T>(arr: T[], f: (T) -> Boolean) -> T[]
```

Returns a new array containing only the elements for which `f` returns `true`. Generic: the result
element type is the input element type. For a monomorphic scalar array with a capture-less literal
predicate, the predicate is inlined into a flat loop with no per-element boxing (ADR-069).

```txt
[1, 2, 3, 4].filter(x => x > 2)   // [3, 4]
```

---

### find

```txt
val find: (arr: Json[], f: (Json) -> Boolean) -> Json
```

Returns the first element for which `f` returns `true`, or `null` if none.

```txt
[1, 2, 3].find(x => x > 1)   // 2
[1, 2, 3].find(x => x > 9)   // null
```

---

### flatMap

```txt
val flatMap: (arr: Json[], f: (Json) -> Json[]) -> Json[]
```

Applies `f` to each element and concatenates the resulting arrays into a single flat array.

```txt
[1, 2, 3].flatMap(x => [x, x * 2])   // [1, 2, 2, 4, 3, 6]
```

---

### flatten

```txt
val flatten: (arr: Json[]) -> Json[]
```

Returns a new array with one level of nesting removed. Non-array elements are kept as-is.

```txt
flatten([[1, 2], [3, 4]])      // [1, 2, 3, 4]
flatten([[1, [2]], [3]])       // [1, [2], 3]
flatten([1, 2, 3])             // [1, 2, 3]
```

---

### for

```txt
val for: (iterable: Json[] | Iterator, f: (Json) -> Json) -> Null
```

Iterates over each element of `iterable`, calling `f` with each element. The return value of `f` is discarded. Works on arrays and iterators.

```txt
[1, 2, 3].for(x => print(toString(x)))
range(0, 5).for(i => print(toString(i)))
```

---

### while

```txt
val while: (arr: Json[], f: (Json) -> Boolean) -> Null
```

Iterates over each element of `arr`, calling `f` with each element. Stops as soon as `f` returns `false`. If `f` always returns `true`, the entire array is visited. This is the primitive used to implement short-circuiting operations such as `some`, `every`, `find`, `indexOf`, and `takeWhile`.

```txt
// stop at first negative number
[1, 2, -3, 4].while(x => x >= 0)   // visits 1, 2, stops at -3

// equivalent to some — stop on first match
var found = false
arr.while(x => val m = x > 5; if m then found = true; !m)
```

---

### groupBy

```txt
val groupBy: (arr: Json[], f: (Json) -> String) -> { ...Json[] }
```

Returns an object where each key is a value returned by `f`, and the corresponding value is an array of all elements that produced that key. Insertion order of keys follows first occurrence.

```txt
["one", "two", "three", "four"].groupBy(s => toString(length(s)))
// { "3": ["one", "two"], "5": ["three"], "4": ["four"] }

[{ "team": "a", "score": 1 }, { "team": "b", "score": 2 }, { "team": "a", "score": 3 }]
  .groupBy(x => x["team"])
// { "a": [{ "team": "a", "score": 1 }, { "team": "a", "score": 3 }],
//   "b": [{ "team": "b", "score": 2 }] }
```

---

### indexOf (array) {#indexOf-array}

```txt
val indexOf: <T>(arr: T[], target: T) -> Int32
```

Returns the zero-based index of the first element deeply equal to `target`, or `-1` if not found.

```txt
[10, 20, 30].indexOf(20)   // 1
[1, 2, 3].indexOf(9)       // -1
```

---

### iter

```txt
val iter: (init: () -> S, hasNext: (S) -> Boolean, next: (S) -> S, value: (S) -> T) -> Iterator
```

Constructs a custom iterator from four functions: `init` produces the initial state, `hasNext` tests whether to continue, `next` advances the state, and `value` extracts the current element.

```txt
// Fibonacci iterator
val fibs = iter(
  () => { "a": 0, "b": 1 },
  s => s["a"] < 100,
  s => { "a": s["b"], "b": s["a"] + s["b"] },
  s => s["a"]
)
fibs.for(n => print(toString(n)))
```

---

### iterOf

```txt
val iterOf: (arr: Json[]) -> Iterator
```

Returns an iterator that yields each element of `arr` in order. Produces a first-class iterator value that can be passed around before consumption.

```txt
val it = iterOf([10, 20, 30])
it.for(x => print(toString(x)))   // prints 10, 20, 30
```

---

### length (array) {#length-array}

```txt
val length: (x: Json) -> Int32
```

Returns the length of an array, string, or object (number of keys).

```txt
length([1, 2, 3])        // 3
length("hello")          // 5
length({ "a": 1 })       // 1
```

---

### map

```txt
val map: <T, U>(arr: T[], f: (T) -> U) -> U[]
```

Returns a new array formed by applying `f` to each element of `arr` in order. Generic: the result
element type is the callback's return type. For a monomorphic scalar array with a capture-less literal
lambda, the lambda body is inlined into a flat loop with no per-element boxing or closure call
(ADR-069).

```txt
[1, 2, 3].map(x => x * 2)        // [2, 4, 6]
["a", "b"].map(s => toUpper(s))   // ["A", "B"]
```

---

### max (array) {#max-array}

```txt
val max: (arr: Number[]) -> Number
```

Returns the largest value in `arr`. The array must be non-empty; passing an empty array is a runtime error.

```txt
max([3, 1, 4, 1, 5, 9])   // 9
max([42])                   // 42
```

---

### maxBy

```txt
val maxBy: (arr: Json[], f: (Json) -> Number) -> Json
```

Returns the element of `arr` for which `f` produces the largest value. The array must be non-empty.

```txt
[{ "name": "Alice", "age": 30 }, { "name": "Bob", "age": 25 }]
  .maxBy(p => p["age"])
// { "name": "Alice", "age": 30 }
```

---

### min (array) {#min-array}

```txt
val min: (arr: Number[]) -> Number
```

Returns the smallest value in `arr`. The array must be non-empty; passing an empty array is a runtime error.

```txt
min([3, 1, 4, 1, 5, 9])   // 1
min([42])                   // 42
```

---

### minBy

```txt
val minBy: (arr: Json[], f: (Json) -> Number) -> Json
```

Returns the element of `arr` for which `f` produces the smallest value. The array must be non-empty.

```txt
[{ "name": "Alice", "age": 30 }, { "name": "Bob", "age": 25 }]
  .minBy(p => p["age"])
// { "name": "Bob", "age": 25 }
```

---

### partition

```txt
val partition: (arr: Json[], f: (Json) -> Boolean) -> [Json[], Json[]]
```

Returns a two-element array `[passing, failing]` where `passing` contains all elements for which `f` returned `true` and `failing` contains the rest, both in their original order.

```txt
val [evens, odds] = [1, 2, 3, 4, 5].partition(x => x % 2 == 0)
// evens: [2, 4],  odds: [1, 3, 5]
```

---

### prepend

```txt
val prepend: (arr: Json[], item: Json) -> Json[]
```

Returns a new array with `item` added at the beginning. Does not modify `arr`. Like `append`, the result preserves the input's element representation (a flat `UInt8[]`/`Int32[]` stays flat; a `Json[]` stays tagged).

```txt
prepend([2, 3], 1)    // [1, 2, 3]
prepend([], "hello")  // ["hello"]
```

---

### product

```txt
val product: (arr: Number[]) -> Number
```

Returns the product of all elements in `arr`. Returns `1` for an empty array.

```txt
product([1, 2, 3, 4])   // 24
product([])              // 1
```

---

### push

```txt
val push: (arr: Json[], item: Json) -> Null
```

Appends `item` to `arr` in place. This is one of the few mutating operations in Lin — it modifies the array that was passed in.

```txt
val xs = []
push(xs, 1)
push(xs, 2)
// xs is now [1, 2]
```

---

### range

```txt
val range: (start: Int32, end: Int32) -> Iterator
```

Returns an iterator that yields integers from `start` up to (but not including) `end`, stepping by `1`. If `start >= end`, the iterator is empty. For a custom or negative step, use [`rangeStep`](#rangeStep).

```txt
range(0, 3).for(i => print(toString(i)))   // prints 0, 1, 2
range(1, 4).map(i => i * 2)               // [2, 4, 6]
range(5, 5)                               // empty
```

---

### rangeStep

```txt
val rangeStep: (start: Int32, end: Int32, step: Int32) -> Iterator
```

Returns an iterator that yields integers from `start` toward `end` (exclusive), advancing by `step` each time. With a positive `step` it counts up while `i < end`; with a negative `step` it counts down while `i > end`. A `step` of `0` yields an empty iterator.

```txt
rangeStep(0, 10, 2).for(i => print(toString(i)))   // 0, 2, 4, 6, 8
rangeStep(5, 0, -1).map(i => i)                    // [5, 4, 3, 2, 1]
```

---

### reduce

```txt
val reduce: <T, U>(arr: T[], init: U, f: (U, T) -> U) -> U
```

Folds `arr` left-to-right starting from `init`. `f` receives the accumulator as its first argument and the current element as its second. Generic: the accumulator/result type is the type of `init`. For a monomorphic scalar accumulator with a capture-less literal reducer, the accumulator is carried unboxed through the loop and the reducer body is inlined with no per-element boxing or closure call (ADR-069).

```txt
[1, 2, 3, 4].reduce(0, (acc, x) => acc + x)   // 10
```

---

### reverse

```txt
val reverse: (arr: Json[]) -> Json[]
```

Returns a new array with the elements in reversed order.

```txt
[1, 2, 3].reverse()   // [3, 2, 1]
```

---

### scan

```txt
val scan: (arr: Json[], init: Json, f: (Json, Json) -> Json) -> Json[]
```

Like `reduce`, but returns an array of all intermediate accumulator values including the initial value. The result always has `length(arr) + 1` elements.

```txt
[1, 2, 3, 4].scan(0, (acc, x) => acc + x)   // [0, 1, 3, 6, 10]
[].scan(0, (acc, x) => acc + x)              // [0]
```

---

### set (array) {#set-array}

```txt
val set: <T>(arr: T[], index: Int32, item: T) -> Null
```

Sets the element at `index` to `item` **in place** — a mutating operation, the index-assignment counterpart to [`push`](#push). `index` must be in bounds (`0 <= index < length(arr)`); an out-of-bounds index is a runtime error. Often paired with [`arrayAllocate`](#arrayAllocate) to fill a pre-sized buffer.

```txt
val buf = arrayAllocate(3)
set(buf, 0, "a")
set(buf, 1, "b")
// buf is now ["a", "b", null]
```

---

### slice

```txt
val slice: (arr: T[], start: Int32, end: Int32) -> T[]
```

Returns a copy of the elements in the half-open range `[start, end)`. `start` and `end` are clamped to `[0, length(arr)]`. The element type is preserved: slicing a `UInt8[]` yields a `UInt8[]`, an `Int32[]` an `Int32[]`, and a `Json[]` a `Json[]`. Also re-exported from `std/bytes`. There is no range-index syntax (`arr[a..b]`).

```txt
[10, 20, 30, 40, 50].slice(1, 4)   // [20, 30, 40]
```

---

### some

```txt
val some: (arr: Json[], f: (Json) -> Boolean) -> Boolean
```

Returns `true` if `f` returns `true` for at least one element. Returns `false` for an empty array.

```txt
[1, 2, 3].some(x => x > 2)   // true
[1, 2, 3].some(x => x > 9)   // false
```

---

### sort

```txt
val sort: (arr: Json[], compare: (Json, Json) -> Int32) -> Json[]
```

Returns a new array with elements sorted according to `compare`. The comparator must return a negative number if the first argument should come first, a positive number if the second should come first, and `0` if they are equal. Does not modify `arr`.

```txt
[3, 1, 4, 1, 5].sort((a, b) => a - b)   // [1, 1, 3, 4, 5]
[3, 1, 4, 1, 5].sort((a, b) => b - a)   // [5, 4, 3, 1, 1]

[{ "n": 3 }, { "n": 1 }, { "n": 2 }]
  .sort((a, b) => a["n"] - b["n"])
// [{ "n": 1 }, { "n": 2 }, { "n": 3 }]
```

---

### sortBy

```txt
val sortBy: (arr: Json[], f: (Json) -> Json) -> Json[]
```

Returns a new array sorted in ascending order by the key extracted by `f`. Keys are compared using Lin's natural ordering (numbers numerically, strings lexicographically). Does not modify `arr`.

```txt
["banana", "apple", "cherry"].sortBy(s => s)
// ["apple", "banana", "cherry"]

[{ "name": "Bob", "age": 25 }, { "name": "Alice", "age": 30 }]
  .sortBy(p => p["name"])
// [{ "name": "Alice", "age": 30 }, { "name": "Bob", "age": 25 }]
```

---

### sum

```txt
val sum: (arr: Number[]) -> Number
```

Returns the sum of all elements in `arr`. Returns `0` for an empty array.

```txt
sum([1, 2, 3, 4])   // 10
sum([])              // 0
```

---

### take

```txt
val take: (arr: Json[], n: Int32) -> Json[]
```

Returns a new array containing only the first `n` elements. If `n >= length(arr)`, returns a copy of the full array.

```txt
take([1, 2, 3, 4], 2)   // [1, 2]
take([1, 2], 5)          // [1, 2]
take([1, 2, 3], 0)       // []
```

---

### takeWhile

```txt
val takeWhile: (arr: Json[], f: (Json) -> Boolean) -> Json[]
```

Returns a new array of leading elements for which `f` returns `true`. Stops at the first element where `f` returns `false`.

```txt
[1, 2, 3, 4, 1].takeWhile(x => x < 3)   // [1, 2]
[1, 2, 3].takeWhile(x => x > 0)          // [1, 2, 3]
[1, 2, 3].takeWhile(x => x > 9)          // []
```

---

### unique

```txt
val unique: (arr: Json[]) -> Json[]
```

Returns a new array with duplicate elements removed, preserving the order of first occurrence. Equality uses deep structural comparison.

```txt
unique([1, 2, 1, 3, 2])                      // [1, 2, 3]
unique(["a", "b", "a"])                       // ["a", "b"]
unique([{ "x": 1 }, { "x": 1 }, { "x": 2 }]) // [{ "x": 1 }, { "x": 2 }]
```

---

### zip

```txt
val zip: (a: Json[], b: Json[]) -> [Json, Json][]
```

Returns an array of two-element arrays pairing elements from `a` and `b` by index. The length of the result equals the length of the shorter input.

```txt
zip([1, 2, 3], ["a", "b", "c"])   // [[1, "a"], [2, "b"], [3, "c"]]
zip([1, 2], ["a", "b", "c"])      // [[1, "a"], [2, "b"]]
zip([], [1, 2])                    // []
```

---

## std/number

Import:

```txt
import { parseInt32, parseFloat64 } from "std/number"
```

---

### isFloat64

```txt
val isFloat64: (s: String) -> Boolean
```

Returns `true` if `s` can be successfully parsed as a `Float64`.

```txt
isFloat64("3.14")   // true
isFloat64("1e10")   // true
isFloat64("42")     // true
isFloat64("abc")    // false
isFloat64("")       // false
```

---

### isInt32

```txt
val isInt32: (s: String) -> Boolean
```

Returns `true` if `s` can be successfully parsed as an `Int32`.

```txt
isInt32("42")      // true
isInt32("3.14")    // false
isInt32("")        // false
```

---

### parseFloat64

```txt
val parseFloat64: (s: String) -> Float64
```

Parses `s` as a base-10 floating-point number.

```txt
parseFloat64("3.14")   // 3.14
parseFloat64("1e10")   // 10000000000.0
```

---

### parseInt32

```txt
val parseInt32: (s: String) -> Int32
```

Parses `s` as a base-10 integer. If `s` cannot be parsed or the value overflows `Int32`, the result is a runtime error. Use `isInt32` to guard untrusted input, or `tryParseInt32` for a safe alternative.

```txt
parseInt32("42")   // 42
parseInt32("-7")   // -7
```

---

### toFloat64

```txt
val toFloat64: (v: Int32) -> Float64
```

Widens an `Int32` to `Float64`. Always exact.

```txt
toFloat64(42)   // 42.0
```

---

### toInt32

```txt
val toInt32: (v: Float64) -> Int32
```

Converts a `Float64` to `Int32` by truncating toward zero.

```txt
toInt32(3.9)    // 3
toInt32(-2.1)   // -2
```

---

### Narrowing casts

```txt
val toUInt8:  (v: UInt64) -> UInt8
val toInt8:   (v: UInt64) -> Int8
val toUInt16: (v: UInt64) -> UInt16
val toInt16:  (v: UInt64) -> Int16
val toUInt32: (v: UInt64) -> UInt32
val toInt64:  (v: UInt64) -> Int64
val toUInt64: (v: UInt64) -> UInt64
```

Explicit integer narrowing (spec §21). Implicit narrowing — assigning a wider numeric to a narrower one — is a compile-time error; these casts perform it explicitly, truncating to the target width with two's-complement (`as`-cast) semantics. The input is taken as `UInt64` (the widest unsigned), so any narrower *unsigned* integer — or a value masked down to a byte/word — widens into the parameter without range loss; a bare integer literal in range is accepted directly. They are the byte-extraction mechanism used by `std/bytes`, but are generally useful wherever explicit width control is needed.

```txt
toUInt8(0x1234)              // 0x34  (52)
toUInt8((v >> 24) & 0xFF)    // top byte of a UInt32 v
toUInt16(b[0]) << 8          // widen a byte for endian assembly
```

---

### tryParseFloat64

```txt
val tryParseFloat64: (s: String) -> Float64 | Null
```

Parses `s` as a floating-point number. Returns `Null` if `s` is not a valid `Float64`, instead of a runtime error. Prefer this over `isFloat64` + `parseFloat64` for safe parsing of untrusted input.

```txt
tryParseFloat64("3.14")   // 3.14
tryParseFloat64("bad")    // null
```

---

### tryParseInt32

```txt
val tryParseInt32: (s: String) -> Int32 | Null
```

Parses `s` as a base-10 integer. Returns `Null` if `s` is not a valid `Int32`, instead of a runtime error. Prefer this over `isInt32` + `parseInt32` for safe parsing of untrusted input.

```txt
tryParseInt32("42")    // 42
tryParseInt32("3.14")  // null
tryParseInt32("bad")   // null
```

---

## std/bytes

Slicing and endian (de)serialization on `UInt8[]` byte buffers (spec §27.1–§27.3). The endian helpers are written in Lin on top of the bitwise operators (§27.2) and the `std/number` narrowing casts (extracting a byte from a wider integer needs an explicit narrowing cast). The four float bit-reinterpret functions are runtime intrinsics, since a float's bit pattern cannot be obtained by shift-and-mask.

| Function | Signature | Description |
| --- | --- | --- |
| `slice` | `(UInt8[], Int32, Int32) -> UInt8[]` | Sub-buffer copy (re-export of `std/array` slice) |
| `u16FromBe` / `u32FromBe` / `u64FromBe` | `(UInt8[], Int32) -> UIntN` | Read big-endian at offset |
| `u16FromLe` / `u32FromLe` / `u64FromLe` | `(UInt8[], Int32) -> UIntN` | Read little-endian at offset |
| `u16ToBe` / `u32ToBe` / `u64ToBe` | `(UIntN) -> UInt8[]` | Write big-endian |
| `u16ToLe` / `u32ToLe` / `u64ToLe` | `(UIntN) -> UInt8[]` | Write little-endian |
| `f32ToBits` | `(Float32) -> UInt32` | Reinterpret a float's bits (intrinsic) |
| `f32FromBits` | `(UInt32) -> Float32` | Reinterpret bits as a float (intrinsic) |
| `f64ToBits` | `(Float64) -> UInt64` | Reinterpret a double's bits (intrinsic) |
| `f64FromBits` | `(UInt64) -> Float64` | Reinterpret bits as a double (intrinsic) |
| `f32ToBe` / `f32ToLe` | `(Float32) -> UInt8[]` | Serialize a float (big/little-endian) |
| `f32FromBe` / `f32FromLe` | `(UInt8[], Int32) -> Float32` | Deserialize a float at offset |
| `f64ToBe` / `f64ToLe` | `(Float64) -> UInt8[]` | Serialize a double (big/little-endian) |
| `f64FromBe` / `f64FromLe` | `(UInt8[], Int32) -> Float64` | Deserialize a double at offset |

Reads take a buffer and a byte offset; writes return a freshly allocated `UInt8[]` of the type's width (2, 4, or 8 bytes). Slicing is a function, `slice(buf, start, end)`; there is no range-index syntax.

Example — an 8-byte two-`Float32` control packet (e.g. two motor speeds) round-tripped through a big-endian buffer:

```txt
import { push, length, for } from "std/array"
import { f32ToBe, f32FromBe, f32FromBits } from "std/bytes"

// Float32 literals are not yet context-narrowed, so build them from bit patterns:
// 1.5f = 0x3FC00000, -2.25f = 0xC0100000.
val leftMotor: Float32 = f32FromBits(0x3FC00000)
val rightMotor: Float32 = f32FromBits(0xC0100000)

val packet: UInt8[] = []
f32ToBe(leftMotor).for(x => push(packet, x))
f32ToBe(rightMotor).for(x => push(packet, x))
// length(packet) == 8

val a: Float32 = f32FromBe(packet, 0)   // 1.5
val b: Float32 = f32FromBe(packet, 4)   // -2.25
```

---

## std/math

Mathematical functions and constants.

Import:

```txt
import { abs, floor, ceil, round, sqrt, pow, PI } from "std/math"
```

---

### Constants

#### E

```txt
val E: Float64
```

Euler's number: `2.718281828459045`.

#### INFINITY

```txt
val INFINITY: Float64
```

Positive infinity. Use `-INFINITY` for negative infinity.

#### NAN

```txt
val NAN: Float64
```

The IEEE 754 not-a-number sentinel. Use `isNaN` to test for it; `NAN == NAN` is `false`.

#### PI

```txt
val PI: Float64
```

The ratio of a circle's circumference to its diameter: `3.141592653589793`.

---

### abs

```txt
val abs: (n: Number) -> Number
```

Returns the absolute value of `n`. The return type matches the input type.

```txt
abs(-5)     // 5
abs(3.14)   // 3.14
abs(0)      // 0
```

---

### acos

```txt
val acos: (x: Float64) -> Float64
```

Returns the arc cosine of `x` in radians. `x` must be in `[-1, 1]`; values outside that range return `NAN`.

```txt
acos(1.0)    // 0.0
acos(0.0)    // 1.5707963…  (π/2)
acos(-1.0)   // 3.1415926…  (π)
```

---

### asin

```txt
val asin: (x: Float64) -> Float64
```

Returns the arc sine of `x` in radians. `x` must be in `[-1, 1]`; values outside that range return `NAN`.

```txt
asin(0.0)    // 0.0
asin(1.0)    // 1.5707963…  (π/2)
```

---

### atan

```txt
val atan: (x: Float64) -> Float64
```

Returns the arc tangent of `x` in radians, in the range `(-π/2, π/2)`.

```txt
atan(0.0)   // 0.0
atan(1.0)   // 0.7853981…  (π/4)
```

---

### atan2

```txt
val atan2: (y: Float64, x: Float64) -> Float64
```

Returns the arc tangent of `y/x` in radians, using the signs of both arguments to determine the correct quadrant. Result is in `(-π, π]`.

```txt
atan2(1.0, 1.0)    // 0.7853981…  (π/4)
atan2(1.0, -1.0)   // 2.3561944…  (3π/4)
```

---

### ceil

```txt
val ceil: (x: Float64) -> Float64
```

Returns the smallest integer value greater than or equal to `x` (round toward positive infinity).

```txt
ceil(3.2)    // 4.0
ceil(-3.2)   // -3.0
ceil(3.0)    // 3.0
```

---

### clamp

```txt
val clamp: (v: Number, lo: Number, hi: Number) -> Number
```

Returns `lo` if `v < lo`, `hi` if `v > hi`, otherwise `v`.

```txt
clamp(5, 1, 10)    // 5
clamp(-3, 1, 10)   // 1
clamp(15, 1, 10)   // 10
```

---

### cos

```txt
val cos: (x: Float64) -> Float64
```

Returns the cosine of `x` (in radians).

```txt
cos(0.0)   // 1.0
cos(PI)    // -1.0
```

---

### exp

```txt
val exp: (x: Float64) -> Float64
```

Returns `e` raised to the power `x`.

```txt
exp(0.0)   // 1.0
exp(1.0)   // 2.71828…
```

---

### floor

```txt
val floor: (x: Float64) -> Float64
```

Returns the largest integer value less than or equal to `x` (round toward negative infinity).

```txt
floor(3.9)    // 3.0
floor(-3.1)   // -4.0
floor(3.0)    // 3.0
```

---

### isFinite

```txt
val isFinite: (x: Float64) -> Boolean
```

Returns `true` if `x` is neither `NAN` nor infinite.

```txt
isFinite(3.14)      // true
isFinite(INFINITY)  // false
isFinite(NAN)       // false
```

---

### isNaN

```txt
val isNaN: (x: Float64) -> Boolean
```

Returns `true` if `x` is `NAN`. Unlike `x == NAN`, this function returns `true` for NaN.

```txt
isNaN(NAN)    // true
isNaN(0.0)    // false
isNaN(1.0)    // false
```

---

### log

```txt
val log: (x: Float64) -> Float64
```

Returns the natural logarithm (base `e`) of `x`. Returns `NAN` for negative values and `-INFINITY` for `0.0`.

```txt
log(1.0)   // 0.0
log(E)     // 1.0
```

---

### log10

```txt
val log10: (x: Float64) -> Float64
```

Returns the base-10 logarithm of `x`.

```txt
log10(100.0)   // 2.0
log10(1.0)     // 0.0
```

---

### log2

```txt
val log2: (x: Float64) -> Float64
```

Returns the base-2 logarithm of `x`.

```txt
log2(8.0)    // 3.0
log2(1.0)    // 0.0
```

---

### max (math) {#max-math}

```txt
val max: (a: Number, b: Number) -> Number
```

Returns the larger of two scalar values. For the maximum of an array, see `std/array`'s [`max`](#max-array).

```txt
max(3, 7)      // 7
max(-1, -5)    // -1
max(2.5, 2.4)  // 2.5
```

---

### min (math) {#min-math}

```txt
val min: (a: Number, b: Number) -> Number
```

Returns the smaller of two scalar values. For the minimum of an array, see `std/array`'s [`min`](#min-array).

```txt
min(3, 7)      // 3
min(-1, -5)    // -5
min(2.5, 2.4)  // 2.4
```

---

### pow

```txt
val pow: (base: Float64, exp: Float64) -> Float64
```

Returns `base` raised to the power `exp`.

```txt
pow(2.0, 10.0)   // 1024.0
pow(9.0, 0.5)    // 3.0
```

---

### random

```txt
val random: () -> Float64
```

Returns a uniformly distributed random `Float64` in the range `[0, 1)`.

```txt
val x = random()   // e.g. 0.7341293...
```

---

### round

```txt
val round: (x: Float64) -> Float64
```

Returns `x` rounded to the nearest integer. Halves round away from zero (half-up for positive, half-down for negative).

```txt
round(3.4)    // 3.0
round(3.5)    // 4.0
round(-3.5)   // -4.0
```

---

### sign

```txt
val sign: (n: Number) -> Int32
```

Returns `-1` if `n` is negative, `1` if positive, and `0` if zero.

```txt
sign(-42)   // -1
sign(0)     // 0
sign(7)     // 1
```

---

### sin

```txt
val sin: (x: Float64) -> Float64
```

Returns the sine of `x` (in radians).

```txt
sin(0.0)        // 0.0
sin(PI / 2.0)   // 1.0
```

---

### sqrt

```txt
val sqrt: (x: Float64) -> Float64
```

Returns the non-negative square root of `x`. Returns `NAN` for negative values.

```txt
sqrt(9.0)    // 3.0
sqrt(2.0)    // 1.41421356…
sqrt(-1.0)   // NAN
```

---

### tan

```txt
val tan: (x: Float64) -> Float64
```

Returns the tangent of `x` (in radians).

```txt
tan(0.0)        // 0.0
tan(PI / 4.0)   // 1.0
```

---

### toFixed

```txt
val toFixed: (x: Float64, decimals: Int32) -> String
```

Formats `x` as a decimal string with exactly `decimals` digits after the decimal point. Rounds using half-up. `decimals` must be `>= 0`.

```txt
toFixed(3.14159, 2)   // "3.14"
toFixed(1.0, 3)       // "1.000"
toFixed(0.005, 2)     // "0.01"
```

---

### trunc

```txt
val trunc: (x: Float64) -> Float64
```

Returns the integer part of `x` by discarding the fractional digits (rounds toward zero).

```txt
trunc(3.9)    // 3.0
trunc(-3.9)   // -3.0
trunc(3.0)    // 3.0
```

---

## std/object

Import:

```txt
import { keys, values, entries, fromEntries, merge, pick, omit, mapValues, isEmpty } from "std/object"
```

---

### entries

```txt
val entries: (obj: Json) -> [String, Json][]
```

Returns an array of `[key, value]` pairs in insertion order.

```txt
entries({ "a": 1, "b": 2 })   // [["a", 1], ["b", 2]]
```

---

### fromEntries

```txt
val fromEntries: (pairs: [String, Json][]) -> {}
```

Builds an object from an array of `[key, value]` pairs. This is the inverse of `entries`. If the same key appears more than once, the last value wins.

```txt
fromEntries([["a", 1], ["b", 2]])   // { "a": 1, "b": 2 }

entries(obj).map(([k, v]) => [k, v * 2]).fromEntries()   // double all values
```

---

### isEmpty

```txt
val isEmpty: (x: Json) -> Boolean
```

Returns `true` if `x` is an empty object (`{}`), an empty array (`[]`), or an empty string (`""`).

```txt
isEmpty({})          // true
isEmpty([])          // true
isEmpty("")          // true
isEmpty({ "a": 1 })  // false
isEmpty([1])         // false
isEmpty("hi")        // false
```

---

### keys

```txt
val keys: (obj: Json) -> String[]
```

Returns an array of the object's keys in insertion order.

```txt
keys({ "a": 1, "b": 2 })   // ["a", "b"]
```

---

### mapValues

```txt
val mapValues: (obj: {}, f: (Json) -> Json) -> {}
```

Returns a new object with the same keys as `obj` but with each value transformed by `f`. Key order is preserved.

```txt
mapValues({ "a": 1, "b": 2 }, v => v * 10)   // { "a": 10, "b": 20 }
mapValues({ "x": "hello" }, s => toUpper(s))  // { "x": "HELLO" }
```

---

### merge

```txt
val merge: (a: {}, b: {}) -> {}
```

Returns a new object containing all keys from `a` and `b`. If both objects have the same key, the value from `b` is used. Key order follows `a`'s keys first (preserving insertion order), then any new keys from `b`.

```txt
merge({ "a": 1, "b": 2 }, { "b": 99, "c": 3 })   // { "a": 1, "b": 99, "c": 3 }
merge({}, { "x": 1 })                               // { "x": 1 }
```

---

### omit

```txt
val omit: (obj: {}, keys: String[]) -> {}
```

Returns a new object with all keys from `obj` except those listed in `keys`. Keys not present in `obj` are silently ignored.

```txt
omit({ "a": 1, "b": 2, "c": 3 }, ["b"])        // { "a": 1, "c": 3 }
omit({ "a": 1, "b": 2 }, ["a", "b", "x"])       // {}
```

---

### pick

```txt
val pick: (obj: {}, keys: String[]) -> {}
```

Returns a new object containing only the keys listed in `keys`. Keys not present in `obj` are omitted from the result.

```txt
pick({ "a": 1, "b": 2, "c": 3 }, ["a", "c"])   // { "a": 1, "c": 3 }
pick({ "a": 1 }, ["a", "x"])                     // { "a": 1 }
```

---

### values

```txt
val values: (obj: Json) -> Json[]
```

Returns an array of the object's values in insertion order.

```txt
values({ "a": 1, "b": 2 })   // [1, 2]
```

---

## std/json

Import:

```txt
import { fromJson } from "std/json"
```

---

### fromJson

```txt
val fromJson: (Type, value: Json) -> T | Error
```

Type-directed decode: validates a `Json` value against the target type `T` and returns either
the decoded value (typed as `T`) or an `Error`. Write it idiomatically as `T.fromJson(json)` or
equivalently as `fromJson(T, json)`. `T` is a **type** (a type name or `type` alias), not a
runtime value.

```txt
type Person = { "name": String, "age": Int32 }

val p = Person.fromJson({ "name": "Bob", "age": 30 })
// p is Person | Error
```

**Detecting failure.** On the first structural mismatch `fromJson` returns a single `Error`
object — it stops at the first error and does not collect all of them. The `Error` shape is:

```txt
{ "type": "error", "message": String, "path": String }
```

`path` is a JSONPath-ish location of the mismatch, e.g. `$.address.city` or `$[2]`. Detect a
decode failure with `is Error` or, equivalently, the discriminant `result["type"] == "error"`.
`is Error` is special-cased to check the `"type": "error"` discriminant (not just the object
tag), so it distinguishes a decode failure from a successfully-decoded value (see ADR-047).

```txt
// Idiomatic: match on `T | Error`. The `is Error` arm MUST come first — a structural object
// type like `Person` is matched by a bare object tag check, so a later `is Person` arm would
// also catch the Error object (union first-match-wins, ADR-047).
val describe = (r: Person | Error): Null =>
  match r
    is Error => print("decode failed at ${r["path"]}: ${r["message"]}")
    is Person => print("hello, ${r["name"]}")

// Equivalent, using the discriminant directly:
val r = Person.fromJson(input)
if r["type"] == "error" then
  print("decode failed at ${r["path"]}: ${r["message"]}")
else
  print("hello, ${r["name"]}")
```

**What is validated.**

- **Objects**: every required field must be present with a compatible type; a field is optional
  (may be absent) iff its target type includes `Null` (e.g. `String | Null`). Extra keys are
  ignored (width subtyping).
- **Arrays** (`T[]`): every element is validated against `T`. **Fixed arrays** (`[A, B]`): the
  length must match exactly and each position is validated.
- **Unions**: the **first** structurally-matching variant wins. Prefer a discriminant field for
  overlapping object variants (ADR-047).
- **Numbers** (target-driven): an **integer** target requires an integral, in-range number
  (`3.14` → error; out-of-range → error); a **float** target accepts any number; a
  `Json`/unconstrained target accepts any number as-is.
- Recursive types (e.g. `type Tree = { "value": Int32, "children": Tree[] }`) are supported.

Array, fixed-array, and union targets must be named via a `type` alias (the receiver must be a
bare type name): `type IntArr = Int32[]; IntArr.fromJson([1, 2, 3])`.

A `Json` value cannot be assigned to a concrete structured object without decoding — `fromJson`
(or `is`/`has` narrowing) is the sound conversion (ADR-046).

---

## std/hash

Import:

```txt
import { hash } from "std/hash"
```

---

### hash

```txt
val hash: (x: Json) -> String
```

Returns a canonical, type-tagged string key for any JSON value. The key is stable and matches Lin's structural equality (spec §9): equal values produce equal keys, objects hash independently of key order, and arrays hash order-sensitively. Values of different types never collide — the key carries a type tag, so `hash(42)` (`"i:42"`) differs from `hash("42")` (`"s:42"`). Use it to deduplicate or index values by structural identity (e.g. as object keys in a manual set/map).

```txt
hash(null)        // "N"
hash(true)        // "b:true"
hash(42)          // "i:42"
hash("hello")     // "s:hello"
hash([1, 2, 3]) == hash([1, 2, 3])   // true
hash([1, 2]) == hash([2, 1])         // false
hash(42) == hash("42")               // false
```

---

## std/io

Import:

```txt
import { print, readLine, lines } from "std/io"
```

---

### args

```txt
val args: () -> String[]
```

Returns the command-line arguments passed to the program, starting from the first user argument (i.e., `argv` after the script name). Returns an empty array if no arguments were provided.

```txt
// ./program foo bar
val arguments = args()   // ["foo", "bar"]
```

---

### exit

```txt
val exit: (code: Int32) -> Null
```

Terminates the process immediately with the given exit code. `0` conventionally indicates success; any non-zero value indicates failure. This function does not return.

```txt
exit(0)   // success
exit(1)   // failure
```

---

### lines (io) {#lines-io}

```txt
val lines: () -> Iterator
```

Returns an iterator that yields one `String` per line from stdin. Terminates at EOF.

```txt
lines().for(line => print(line.trim()))
```

---

### print

```txt
val print: (value: Json) -> Null
```

Writes `value` to standard output followed by a newline. Strings are printed without quotes; other values are formatted as JSON.

```txt
print("hello")       // hello
print(42)            // 42
print([1, 2, 3])     // [1, 2, 3]
```

---

### printErr

```txt
val printErr: (value: Json) -> Null
```

Writes `value` to standard error followed by a newline. Identical in behaviour to `print` but writes to stderr instead of stdout.

```txt
printErr("warning: file not found")
printErr({ "code": 500, "message": "internal error" })
```

---

### prompt

```txt
val prompt: (message: String) -> String | Null
```

Prints `message` to stdout (without a trailing newline), then reads one line from stdin. Returns the line with the trailing newline stripped, or `Null` on EOF.

```txt
val name = prompt("Enter your name: ")
match name
  is Null  => print("no input")
  else     => print("Hello, ${name}!")
```

---

### readAll

```txt
val readAll: () -> String
```

Reads all of stdin and returns it as a single string including embedded newlines.

```txt
val raw = readAll()
```

---

### readLine

```txt
val readLine: () -> String | Null
```

Reads one line from stdin, stripping the trailing newline. Returns `Null` on EOF.

```txt
val name = readLine()
match name
  is Null => print("no input")
  else    => print("hello ${name}")
```

---

## std/fs

Import:

```txt
import { readFile, writeFile, readLines, ls, rm, cp, mv } from "std/fs"
```

### Types

```txt
type FileStat = {
  "size":     Int64,
  "modified": Int64,
  "created":  Int64,
  "isFile":   Boolean,
  "isDir":    Boolean,
  "mode":     Int32
}
```

`size` is in bytes. `modified` and `created` are Unix timestamps in milliseconds. `mode` is the Unix file permission bits (0 on non-Unix platforms).

---

### appendFile

```txt
val appendFile: (path: String, content: String) -> Null | Error
```

Appends `content` to the end of the file at `path`.

---

### cp

```txt
val cp: (src: String, dst: String) -> Null | Error
```

Copies the file at `src` to `dst`. Returns `Null` on success, `Error` on failure.

```txt
match cp("src/main.lin", "backup/main.lin")
  is { "type": "error", message } => print("copy failed: ${message}")
  else => null
```

---

### exists

```txt
val exists: (path: String) -> Boolean
```

Returns `true` if a file or directory exists at `path`.

---

### isDir

```txt
val isDir: (path: String) -> Boolean
```

Returns `true` if `path` exists and is a directory. Returns `false` for regular files and for paths that do not exist.

```txt
isDir("src")        // true
isDir("main.lin")   // false
isDir("missing")    // false
```

---

### isFile

```txt
val isFile: (path: String) -> Boolean
```

Returns `true` if `path` exists and is a regular file. Returns `false` for directories and for paths that do not exist.

```txt
isFile("main.lin")   // true
isFile("src")        // false
isFile("missing")    // false
```

---

### ls

```txt
val ls: (path: String, opts: Json) -> String[] | Error
```

Returns an array of entry names in the directory at `path`. Pass `{ "recursive": true }` to walk subdirectories recursively (returns relative paths). Returns an `Error` if `path` does not exist or is not a directory.

```txt
val entries = ls("src", {})
val allFiles = ls("src", { "recursive": true })
```

---

### mkdir

```txt
val mkdir: (path: String, opts: Json) -> Null | Error
```

Creates the directory at `path`. Pass `{ "parents": true }` to create all missing parent directories (equivalent to `mkdir -p`). Returns an `Error` if the path already exists (without `parents`) or if a parent is missing.

```txt
mkdir("output", {})
mkdir("output/reports/2024", { "parents": true })
```

---

### mv

```txt
val mv: (src: String, dst: String) -> Null | Error
```

Moves or renames the file at `src` to `dst`. On most systems this is atomic if both paths are on the same filesystem.

```txt
match mv("tmp/output.json", "output.json")
  is { "type": "error", message } => print("move failed: ${message}")
  else => null
```

---

### readFile

```txt
val readFile: (path: String) -> String | Error
```

Reads the entire contents of the file at `path` as a UTF-8 string.

```txt
match readFile("config.txt")
  is { "type": "error", message } => print("read failed: ${message}")
  else => process(readFile("config.txt"))
```

---

### readFileBytes

```txt
val readFileBytes: (path: String) -> UInt8[] | Error
```

Reads the file at `path` as a packed `UInt8[]` byte buffer (§27.1) — one byte per element. Returns an `Error` if the file cannot be read.

```txt
val bytes = readFileBytes("image.png")
```

---

### readJson

```txt
val readJson: (path: String) -> Json | Error
```

Reads and parses the file at `path` as JSON.

---

### readLines

```txt
val readLines: (path: String) -> String[] | Error
```

Reads the file at `path` and returns an array of strings, one per line. Returns an `Error` if the file cannot be read.

```txt
match readLines("data.csv")
  is { "type": "error", message } => print("cannot open: ${message}")
  else =>
    val lines = readLines("data.csv")
    lines.for(line => process(line))
```

---

### rm

```txt
val rm: (path: String, opts: Json) -> Null | Error
```

Removes the file or directory at `path`. Pass `{ "recursive": true }` to remove a directory and all its contents. Without `recursive`, only files (not directories) can be removed.

```txt
rm("tmp/cache.json", {})
rm("tmp/old-output", { "recursive": true })
```

---

### stat

```txt
val stat: (path: String) -> FileStat | Error
```

Returns metadata for the file or directory at `path`. On success the result object has fields `size`, `modified`, `created`, `isFile`, `isDir`, and `mode`.

```txt
val info = stat("data.csv")
print("size: ${toString(info["size"])} bytes")
```

---

### writeFile

```txt
val writeFile: (path: String, content: String) -> Null | Error
```

Writes `content` to the file at `path`, replacing existing contents.

---

### writeFileBytes

```txt
val writeFileBytes: (path: String, bytes: UInt8[]) -> Null | Error
```

Writes a `UInt8[]` byte buffer (§27.1) to the file at `path`. Returns `Null` on success, `Error` on failure.

---

### writeJson

```txt
val writeJson: (path: String, value: Json, opts: Json) -> Null | Error
```

Serialises `value` to JSON and writes it to `path`. By default the output is pretty-printed. Pass `{ "compact": true }` to write compact single-line JSON.

```txt
writeJson("output.json", data, {})
writeJson("output.json", data, { "compact": true })
```

---

### writeLines

```txt
val writeLines: (path: String, lines: String[]) -> Null | Error
```

Writes each element of `lines` to the file at `path`, separated and terminated by newlines. Returns `Null` on success, `Error` on failure.

```txt
writeLines("names.txt", ["alice", "bob", "carol"])
```

---

## std/path

Pure path string manipulation — no filesystem access. All functions work with both POSIX and Windows-style paths on their respective platforms.

Import:

```txt
import { join, basename, dirname, extname } from "std/path"
```

---

### basename

```txt
val basename: (path: String) -> String
```

Returns the final component of `path`. Trailing separators are ignored.

```txt
basename("/usr/local/bin/lin")   // "lin"
basename("src/main.lin")         // "main.lin"
basename("/")                    // "/"
```

---

### dirname

```txt
val dirname: (path: String) -> String
```

Returns all components of `path` except the last. Trailing separators are ignored.

```txt
dirname("/usr/local/bin/lin")   // "/usr/local/bin"
dirname("src/main.lin")         // "src"
dirname("main.lin")             // "."
```

---

### extname

```txt
val extname: (path: String) -> String
```

Returns the file extension of the last component of `path`, including the leading dot. Returns `""` if there is no extension.

```txt
extname("main.lin")       // ".lin"
extname("archive.tar.gz") // ".gz"
extname("README")         // ""
extname(".gitignore")     // ""
```

---

### isAbsolute

```txt
val isAbsolute: (path: String) -> Boolean
```

Returns `true` if `path` is absolute (begins with `/` on POSIX, or a drive letter + `\` on Windows).

```txt
isAbsolute("/usr/local")    // true
isAbsolute("src/main.lin")  // false
```

---

### join (path) {#join-path}

```txt
val join: (parts: String[]) -> String
```

Joins path segments together using the OS path separator, normalising redundant separators.

```txt
join(["usr", "local", "bin"])   // "usr/local/bin"
join(["/usr", "local/bin"])     // "/usr/local/bin"
join(["src", "", "main.lin"])   // "src/main.lin"
```

---

### normalize

```txt
val normalize: (path: String) -> String
```

Resolves `.` and `..` segments and removes redundant separators. Does not access the filesystem.

```txt
normalize("a/b/../c")    // "a/c"
normalize("/a/./b/c")    // "/a/b/c"
normalize("a//b")        // "a/b"
```

---

### relative

```txt
val relative: (from: String, to: String) -> String
```

Returns the relative path from `from` to `to`. Both arguments should be absolute or both relative.

```txt
relative("/usr/local", "/usr/local/bin/lin")   // "bin/lin"
relative("/usr/local/bin", "/usr/share")       // "../../share"
```

---

### resolve

```txt
val resolve: (path: String) -> String
```

Resolves `path` to an absolute path by joining it with the current working directory. If `path` is already absolute, returns it normalised.

```txt
// assuming cwd = "/home/user/project"
resolve("src/main.lin")   // "/home/user/project/src/main.lin"
resolve("/etc/hosts")     // "/etc/hosts"
```

---

### split (path) {#split-path}

```txt
val split: (path: String) -> String[]
```

Splits `path` into its individual components. A leading separator produces an empty string as the first element.

```txt
split("/usr/local/bin")   // ["", "usr", "local", "bin"]
split("src/main.lin")     // ["src", "main.lin"]
split("main.lin")         // ["main.lin"]
```

---

### stem

```txt
val stem: (path: String) -> String
```

Returns the basename of `path` without its extension.

```txt
stem("main.lin")       // "main"
stem("archive.tar.gz") // "archive.tar"
stem("README")         // "README"
```

---

## std/http

HTTP client functions and server helpers. All client functions are synchronous and blocking.

Import:

```txt
import { fetch, fetchJson, serve, json, notFound } from "std/http"
```

### Types

```txt
type HttpRequest = {
  "method":  String,
  "path":    String,
  "query":   { ...String },
  "headers": { ...String },
  "body":    String
}

type HttpResponse = {
  "status":  Int32,
  "headers": { ...String },
  "body":    String
}

type HttpOptions = {
  "method":  String,
  "headers": { ...String },
  "body":    String
}
```

`HttpOptions` fields are all optional — omitted fields use defaults (`"GET"`, empty headers, empty body).

---

### fetch

```txt
val fetch: (url: String) -> HttpResponse | Error
```

Sends a GET request to `url`. Returns an `Error` only on transport-level failure; HTTP error status codes (4xx, 5xx) are returned as `HttpResponse` values.

```txt
val resp = fetch("https://api.example.com/ping")
match resp
  is Error => print("network error: ${resp["message"]}")
  else     => print(toString(resp["status"]))
```

---

### fetchJson

```txt
val fetchJson: (url: String) -> Json | Error
```

GET `url`, parse the body as JSON. Returns an `Error` if transport fails, the status is not 2xx, or the body is not valid JSON.

```txt
val users = fetchJson("https://api.example.com/users")
match users
  is Error => print("failed: ${users["message"]}")
  else     => users.map(u => u["name"]).for(name => print(name))
```

---

### fetchWith

```txt
val fetchWith: (url: String, options: HttpOptions) -> HttpResponse | Error
```

Sends a request using the method, headers, and body in `options`.

```txt
val resp = fetchWith("https://api.example.com/items", {
  "method": "DELETE",
  "headers": { "Authorization": "Bearer ${token}" }
})
```

---

### postJson

```txt
val postJson: (url: String, body: Json) -> HttpResponse | Error
```

POST `body` as JSON to `url` with `Content-Type: application/json`.

---

### serve

```txt
val serve: (handler: (HttpRequest) -> HttpResponse, port: Int32) -> Null
```

Starts an HTTP server on `port` and calls `handler` for each incoming request **sequentially** — one request at a time. Parses each HTTP/1.1 request into an `HttpRequest`, then writes the returned `HttpResponse` back on the wire. Blocks indefinitely (it only returns — as an `Error` — if the port cannot be bound). A handler that faults yields a `500` response and the server keeps serving.

The handler is the **first** argument so the dot-call form reads naturally: `router.serve(3000)` is `serve(router, 3000)`.

```txt
val router = (req: HttpRequest): HttpResponse =>
  match req["path"]
    is "/ping" => text(200, "pong")
    else => notFound

router.serve(3000)
```

---

### json (helper) {#json-helper}

```txt
val json: (status: Int32, body: Json) -> HttpResponse
```

Builds an `HttpResponse` with the JSON serialisation of `body` and `Content-Type: application/json`.

```txt
json(200, { "users": ["Alice", "Bob"] })
json(404, { "error": "not found" })
```

---

### text (helper) {#text-helper}

```txt
val text: (status: Int32, body: String) -> HttpResponse
```

Builds an `HttpResponse` with `Content-Type: text/plain`.

```txt
text(200, "pong")
```

---

### redirect

```txt
val redirect: (url: String) -> HttpResponse
```

Builds a 302 response with a `Location` header.

```txt
redirect("/login")
```

---

### notFound

```txt
val notFound: HttpResponse
```

A pre-built 404 response with body `"Not Found"`. Used as a value, not called.

```txt
else => notFound
```

---

### badRequest

```txt
val badRequest: (message: String) -> HttpResponse
```

Builds a 400 response with `message` as the plain-text body.

```txt
badRequest("missing required field: name")
```

---

### matchPath

```txt
val matchPath: (path: String, pattern: String) -> { ...String } | Null
```

Matches `path` against `pattern`. Pattern segments beginning with `:` are named captures. Returns an object of captured parameters on match, or `Null`. The path is the first argument so the function chains naturally off a request path.

```txt
matchPath("/users/42",       "/users/:id")       // { "id": "42" }
matchPath("/users/42/posts", "/users/:id/posts") // { "id": "42" }
matchPath("/items/42",       "/users/:id")        // null
matchPath("/static",         "/static")           // {}

// dot-chaining from a request:
req["path"].matchPath("/users/:id")
```

---

### parseBody

```txt
val parseBody: (req: HttpRequest) -> Json | Error
```

Parses `req["body"]` as JSON.

```txt
val body = parseBody(req)
match body
  is Error => badRequest(body["message"])
  else     => createItem(body)
```

---

## std/net

Low-level UDP and TCP sockets — the byte-stream layer beneath `std/http`, for non-HTTP protocols and custom framing. Every socket is an opaque integer fd handle (spec §27.4): there are no open-socket objects in user code, just the raw OS fd as an `Int32`. Every fallible call returns the `T | Error` result shape; a non-blocking read with no data available yet returns `Null` (so a poll loop reads naturally). IPv4 only; `bind`/`listen` bind to `0.0.0.0`.

`recv`/`recvFrom`/`tcpRecv` fill a **caller-owned** `UInt8[]` and return the number of bytes read; the buffer is never transferred across the boundary. The buffer's length bounds the read — pre-size it to the maximum datagram/chunk you want to accept (e.g. `[0,0,...]` of N elements).

### UDP

```txt
udpBind:           (port: Int32)                              => Int32 | Error    // fd handle
udpRecv:           (fd: Int32, buf: UInt8[])                  => Int32 | Null | Error  // bytes read; Null = would-block
udpRecvFrom:       (fd: Int32, buf: UInt8[])                  => { "len": Int32, "addr": String, "port": Int32 } | Null | Error
udpSendTo:         (fd: Int32, addr: String, port: Int32, buf: UInt8[]) => Int32 | Error
udpSetNonblocking: (fd: Int32, on: Boolean)                   => Null | Error
udpClose:          (fd: Int32)                                => Null | Error
```

### TCP

A listener accepts connections, each of which is itself an fd; a client connects directly. Reads and writes operate on a connected fd.

```txt
tcpListen:         (port: Int32)                  => Int32 | Error            // listener fd
tcpAccept:         (fd: Int32)                    => { "fd": Int32, "addr": String, "port": Int32 } | Null | Error  // Null = would-block
tcpConnect:        (host: String, port: Int32)    => Int32 | Error            // connected fd
tcpRecv:           (fd: Int32, buf: UInt8[])      => Int32 | Null | Error      // bytes read; 0 = peer closed; Null = would-block
tcpSend:           (fd: Int32, buf: UInt8[])      => Int32 | Error            // bytes written
tcpSetNonblocking: (fd: Int32, on: Boolean)       => Null | Error
tcpClose:          (fd: Int32)                    => Null | Error
```

### UDP echo example

```txt
import { udpBind, udpSendTo, udpRecvFrom, udpClose } from "std/net"
import { print } from "std/io"

val sock = udpBind(39201)
val msg: UInt8[] = [72, 105, 33]               // "Hi!"
udpSendTo(sock, "127.0.0.1", 39201, msg)       // send to self

val buf: UInt8[] = [0, 0, 0, 0, 0, 0, 0, 0]
val res = udpRecvFrom(sock, buf)
print("got ${res["len"]} bytes from ${res["addr"]}")   // got 3 bytes from 127.0.0.1
udpClose(sock)
```

### TCP echo example

```txt
import { tcpListen, tcpAccept, tcpConnect, tcpRecv, tcpSend, tcpClose } from "std/net"
import { print } from "std/io"

val listener = tcpListen(39202)
val client   = tcpConnect("127.0.0.1", 39202)  // kernel completes the handshake
val accepted = tcpAccept(listener)             // returns the pending connection
val server   = accepted["fd"]

val payload: UInt8[] = [76, 105, 110, 33]      // "Lin!"
tcpSend(client, payload)

val buf: UInt8[] = [0, 0, 0, 0, 0, 0]
val n = tcpRecv(server, buf)                   // n == 4
print("echoed ${n} bytes")

tcpClose(client)
val n2 = tcpRecv(server, buf)                  // 0 — peer closed
tcpClose(server)
tcpClose(listener)
```

---

## std/process

Run and manage external processes. Two styles share one module:

- **Batch** — `exec`/`shell` run a command to completion and collect its full stdout/stderr into an `ExecResult`. `cwd`/`chdir` query/change the working directory.
- **Streaming** — `spawn` starts a child and returns an opaque `ProcessHandle`; `readStdout` reads its piped stdout incrementally; `kill` signals it; `wait` blocks for the exit code.

Every fallible call returns the `T | Error` result shape (spec §27.6).

### Types

```txt
type ExecResult = { "status": Int32, "stdout": String, "stderr": String }
```

`ProcessHandle` is an opaque `Int64` id the runtime interprets — a monotonic id, not an OS pid (so it is immune to pid-reuse races).

```txt
exec:        (command: String, args: String[]) => ExecResult | Error
shell:       (command: String)                 => ExecResult | Error
cwd:         ()                                 => String
chdir:       (path: String)                     => Null | Error
spawn:       (command: String, args: String[]) => ProcessHandle | Error
readStdout:  (handle: ProcessHandle, buf: UInt8[]) => Int32 | Error   // bytes read; 0 = EOF
kill:        (handle: ProcessHandle)            => Null | Error
wait:        (handle: ProcessHandle)            => Int32 | Error      // exit code
```

`exec` runs `command` with `args` (no shell — no injection risk), waits, and returns its status plus captured stdout/stderr. `shell` runs a command string through `/bin/sh -c` (POSIX); prefer `exec` when possible. `command` is looked up on `PATH` or given as an absolute path.

`spawn` starts a child without waiting: its stdin is connected to `/dev/null`, its stdout is captured into a pipe (so `readStdout` works), and its stderr is inherited. `readStdout` fills a **caller-owned** `UInt8[]` and returns the number of bytes read, reading incrementally from the same pipe across calls; `0` means end-of-stream. `wait` blocks until the child exits, returns its exit code (`-1` if terminated by a signal), and reaps the process — after `wait` the handle is no longer valid. (stdout streamed via `readStdout` is not re-collected by `wait`; use `exec` for batch output.) `kill` sends SIGTERM; killing an already-exited child is tolerated and returns `Null`.

### Example — batch: run a command and read its output

```txt
import { exec } from "std/process"
import { print } from "std/io"

val r = exec("git", ["status", "--short"])
match r
  is Error => print("exec failed: ${r["message"]}")
  else =>
    print("exit ${toString(r["status"])}")
    print(r["stdout"])
```

### Example — streaming: capture a subprocess's output incrementally

```txt
import { spawn, readStdout, wait } from "std/process"
import { print } from "std/io"

val h = spawn("sh", ["-c", "printf hello"])
val buf: UInt8[] = [0, 0, 0, 0, 0, 0, 0, 0]
val n = readStdout(h, buf)          // n == 5
print("read ${n} bytes, first = ${buf[0]}")   // read 5 bytes, first = 104 ('h')
val code = wait(h)                  // 0
print("exited ${code}")
```

---

## std/tty

Raw terminal mode and non-blocking key input on stdin (spec §27.7).

```txt
rawMode:  (on: Boolean)  => Null | Error    // enable/disable terminal raw mode
readKey:  ()             => Int32 | Null    // keycode, or Null if no key available (non-blocking)
```

`rawMode(true)` puts the terminal into raw mode: canonical line buffering and echo are disabled, and reads become non-blocking. The original terminal settings are saved and restored exactly by `rawMode(false)`. If stdin is not a terminal (e.g. a pipe), `rawMode` returns an `Error` object rather than panicking.

`readKey` reads a single byte from stdin without blocking: it returns the byte value (`0..255`) as an `Int32`, or `Null` if no key is currently available. Multi-byte sequences (arrow keys, function keys) arrive one byte at a time, so a reader reassembles escape sequences itself.

### Example — poll for a key in raw mode

```txt
import { rawMode, readKey } from "std/tty"
import { print } from "std/io"

rawMode(true)            // disable canonical mode + echo; reads are non-blocking
val k = readKey()        // a byte value, or null if nothing was typed
if k != null then print("key: ${k}") else print("no key ready")
rawMode(false)           // restore the original terminal settings
```

A real application polls `readKey` repeatedly (typically via a `range(...).for(...)` driven loop with `std/time` `sleepMicros` between polls), treating `null` as "nothing yet" and acting on byte values as keys.

---

## std/signal

Minimal, blocking signal handling. Import:

```txt
import { waitSignal } from "std/signal"
```

---

### waitSignal

```txt
val waitSignal: (sig: Int32) -> Int32
```

Blocks the calling thread until OS signal `sig` is delivered, then returns the signal number. The signal is first blocked in the thread's mask and consumed with `sigwait`, so a signal that arrives during setup is not lost (no handler is installed). The mask is per-thread and a single signal is waited on per call.

```txt
val sig = waitSignal(2)   // block until SIGINT (2); returns 2
print("caught signal ${toString(sig)}")
```

---

## std/async

Concurrency primitives. Import what you need:

```txt
import { async, await, parallel } from "std/async"
import { worker, request, close } from "std/async"
import { threadPool } from "std/async"
```

---

### async

```txt
val async: (() -> T) -> Promise
```

Runs a zero-argument thunk asynchronously on a background thread. Returns a `Promise` that resolves to the thunk's return value.

```txt
val p = async(() => fetchJson("https://api.example.com/data"))
val result = await(p)
```

---

### await

```txt
val await: <T>(p: T) -> T | Error
```

Blocks the current thread until the promise resolves, then returns its value as `T | Error`. Can also await an array of promises — returns an array of results.

```txt
val [users, posts] = await([
  async(() => fetchJson("https://db/users")),
  async(() => fetchJson("https://db/posts"))
])
```

`await` auto-flattens nested promises (§24.2.3): if the thunk itself returns a `Promise`, `await`
resolves through every layer (`await(async(() => async(() => 42)))` is `42`).

If the thunk faults (array out of bounds, division by zero, …), the fault is caught at the thread
boundary and surfaces as an `Error` value (an object `{ "type": "error", "message": String }`)
rather than halting the program. Discriminate it with the built-in `Error` type:

```txt
match await(p)
  is Error => print("failed: ${result["message"]}")
  else     => use(result)
```

The static check from §24.2.2 *is* enforced: because `await` returns `T | Error`, assigning the
result to a binding that does not handle the `Error` case is a compile-time error.

```txt
val r: Int32 = await(p)   // type error: Int32 | Error is not assignable to Int32
```

You must handle the `Error` (e.g. with the `match` above) before using the value as a plain `T`.

> Limitation (ADR-070): there is no nominal `Promise<T>` type — a promise handle is erased to
> `Json` — so this enforces "you must handle the `Error` after awaiting" but does **not** catch
> "you forgot to `await`" (using a promise as if it were the value). Error injection happens at
> `await` (where the value materialises), not at `async`, because the other async primitives
> return live promise handles, not values.

---

### close

```txt
val close: (w: Worker) -> Null
```

Shuts down worker `w`, calling its `onClose` function and terminating its thread.

---

### message

```txt
val message: (w: Worker, msg: Msg) -> Null
```

Sends `msg` to worker `w` without waiting for a reply (fire-and-forget).

---

### parallel

```txt
val parallel: ((() -> T)[]) -> T[]
```

Runs an array of zero-argument thunks concurrently and returns an array of their results in the same order. Blocks until all thunks complete.

```txt
val results = parallel([
  () => heavyComputation(1),
  () => heavyComputation(2),
  () => heavyComputation(3)
])
```

---

### race

```txt
val race: (Promise[]) -> T
```

Resolves with the value of the first promise in the array to complete.

---

### request

```txt
val request: (w: Worker, msg: Msg) -> Reply
```

Sends `msg` to worker `w` and blocks until the handler returns a reply.

---

### retry

```txt
val retry: (() -> T, Int32) -> T
```

Runs the thunk up to `n` times, returning the first successful result. If all attempts fail, returns the last error.

---

### threadPool

```txt
val threadPool: (Int32) -> ThreadPool
```

Creates a bounded thread pool with `n` worker threads draining a shared task queue. Submit
work with `pool.poolAsync(thunk)` (see below). The pool bounds concurrency: at most `n` thunks
run at once; excess work queues until a worker frees up.

```txt
val pool = threadPool(8)
val p = pool.poolAsync(() => heavyWork())
val r = await(p)
```

---

### poolAsync

```txt
val poolAsync: (ThreadPool, () => T) -> Promise<T | Error>
```

Enqueues `thunk` on `pool` and returns a `Promise` for its result, resolved when a pool worker
runs it. Designed for the dot-call form `pool.poolAsync(thunk)`. Same transferable-capture rules
as the top-level `async` (the thunk's `val` captures are deep-copied across the boundary; it must
not capture `var`). A fault inside the thunk is isolated and surfaces as an `Error` at `await`.

> Note: the spec spells this `pool.async(...)`; in this implementation the pool submission method
> is exported as `poolAsync` (a distinct name from the top-level `async`, which takes only a
> thunk). `pool.serve(...)` for multi-threaded HTTP is not yet implemented.

---

### timeout

```txt
val timeout: (Promise, Int32) -> T
```

Adds a millisecond timeout to `promise`. If the promise does not resolve within `ms` milliseconds, the result is an error.

---

### shared / get / set / withLock

```txt
val shared:   <T>(T) -> Shared<T>
val get:      <T>(Shared<T>) -> T
val set:      <T>(Shared<T>, T) -> Null
val withLock: <T, R>(Shared<T>, (T) -> R) -> R
```

`Shared<T>` is opt-in **shared mutable state** for many threads (ADR-043 §2.3.1): an
atomic-refcounted box wrapping a reader-writer lock over a private copy of the value.

- `shared(v)` creates a `Shared<T>` boxing a deep copy of `v` (must be transferable).
- `get(s)` takes the **read** lock and returns a deep-copied snapshot (concurrent with other
  `get`s).
- `set(s, v)` takes the **write** lock and replaces the inner value with a deep copy of `v`.
- `withLock(s, f)` holds the **write** lock across `f`, which receives the inner value mutable
  in place (e.g. `a => push(a, 7)`); `f`'s result is copied out. Use this for atomic
  read-modify-write.

```txt
val s = shared([4, 5, 6])
val snap = s.get()                  // snapshot copy
s.set([7, 8, 9])                    // replace wholesale
s.withLock(arr => push(arr, 7))     // atomic in-place mutate
val n = s.withLock(arr => length(arr))   // read a derived value out
```

Safety: every value entering is copied in, every value leaving is copied out, so no live
reference into the box escapes the lock. `get`/`set` are individually atomic but not across the
gap (last-writer-wins); use `withLock` when the update must be atomic.

> `Shared<T>` is **accessor-only**: `shared`/`get`/`set`/`withLock` are the only operations.
> Passing a `Shared` value to anything else (e.g. `push(s, 7)`, indexing) is a compile-time type
> error — the box never auto-unwraps to its inner type or to `Json` (ADR-044). The inner value
> is reachable only via `get`/`withLock`, which copy it out. (This check is enforced by
> `lin build`/`lin run`, which resolve imports; a bare `lin check` does not resolve imports and
> so won't show it.)
>
> Caveat: `withLock` mutates **in place**, so a scalar accumulator (`n => n + 1`) does not
> persist — use a one-element array or `get`/`set`. Importing both `std/array`'s `set` and this
> `set` in one file collides — alias one.

---

### frozen

```txt
val frozen: <T>(T) -> T
```

`frozen(v)` deep-freezes a transferable graph into shared **read-only** state (ADR-045 §2.3.2):
every heap node is sealed immortal+immutable, so many threads can read it concurrently with
**zero copies, no lock, and no atomics**. The value keeps its plain type, so readers use it
transparently:

```txt
val timetable = frozen(loadTimetable())
val routes = parallel(
  journeys.map(j => () => planJourney(timetable, j))   // shared by reference, not copied
)
```

> **Immortal ⇒ never freed.** Use `frozen` for load-once, program-lifetime reference data (a
> timetable, routing table, config). A `frozen()` value created and discarded in a loop leaks.
> The compile-time read-only coercion / mutation-inference (rejecting a frozen value passed to a
> mutating parameter) is deferred (ADR-045): mutating a frozen value is currently a silent no-op
> rather than a compile error. Concurrent reads are fully safe.

---

### worker

```txt
val worker: (handler: (Msg) -> Reply, onClose: () -> Null) -> Worker
```

Creates a background worker thread. `handler` is called for each message received via `request` or `message`. `onClose` is called when the worker is shut down via `close`.

```txt
val w = worker(
  msg => msg * 2,
  () => null
)
val result = request(w, 21)   // 42
close(w)
```

---

## std/env

Access to environment variables for the current process.

Import:

```txt
import { getEnv, environ } from "std/env"
```

---

### environ

```txt
val environ: () -> { ...String }
```

Returns an object containing all environment variables as string key-value pairs.

```txt
val env = environ()
print(env["HOME"])
```

---

### getEnv

```txt
val getEnv: (name: String) -> String | Null
```

Returns the value of the environment variable `name`, or `Null` if it is not set.

```txt
val home = getEnv("HOME")              // e.g. "/home/alice"
val missing = getEnv("DOES_NOT_EXIST") // null
```

---

### setEnv

```txt
val setEnv: (name: String, value: String) -> Null
```

Sets the environment variable `name` to `value` for the current process. This affects the process's own environment and any child processes spawned after this call.

```txt
setEnv("APP_ENV", "production")
```

---

### unsetEnv

```txt
val unsetEnv: (name: String) -> Null
```

Removes the environment variable `name`. If the variable is not set, this is a no-op.

```txt
unsetEnv("DEBUG")
```

---

## std/template

Import:

```txt
import { render, renderWith } from "std/template"
```

Template syntax uses `${key}` holes where `key` is a field name or dot-separated path into the data record.

---

### render

```txt
val render: (path: String, data: {}) -> String | Error
```

Reads the file at `path` and renders it as a template against `data`. Intended for `.lint` template files. Returns an `Error` (`{ "type": "error", "message": ... }`) if the file cannot be read.

```txt
val s = render("greet.lint", { "name": "Alice", "score": 42 })
match s
  is Error => print("error: ${s["message"]}")
  else     => print(s)
```

---

### renderWith

```txt
val renderWith: (template: String, data: {}) -> String
```

Renders a template string directly against `data`. Missing keys render as `"null"`.

```txt
renderWith("Hello, ${name}!", { "name": "Alice" })
// "Hello, Alice!"
```

---

## std/test

A lightweight test framework. Tests are plain Lin values.

Import:

```txt
import { suite, test, run, expect } from "std/test"
```

**Basic usage:**

```txt
import { suite, test, run, expect } from "std/test"

val arithmetic = suite("arithmetic", [
  test("adds two positives", () => [
    expect(1 + 2).toBe(3)
  ]),
  test("multiple assertions", () => [
    expect(0 + 0).toBe(0),
    expect(10 + -10).toBe(0)
  ])
])

run([arithmetic])
```

**A test body must return an array of assertions.** Each matcher
(`expect(...).toBe(...)`, etc.) produces one `Assertion`; the body returns them
as an `Assertion[]` (a comma-separated array literal), and **every** assertion in
the array is evaluated — a test fails if any one of them fails. This is enforced
by the type system: a bare single assertion or a sequence of bare assertion
statements is a compile error, which is what guarantees no assertion is silently
skipped. Even a single assertion is wrapped in `[ ... ]`.

When a test needs setup before its assertions, write the setup statements
followed by the array literal as the body's final expression:

```txt
test("sorts ascending", () =>
  val input = [3, 1, 2]
  val sorted = input.sort((a, b) => a - b)
  [
    expect(input.toString()).toBe("[3, 1, 2]"),
    expect(sorted.toString()).toBe("[1, 2, 3]")
  ]
)
```

---

### Types

```txt
type Assertion = { "type": "pass" } | { "type": "fail", "message": String }

type Test = {
  "name": String,
  "run": () -> Assertion[]
}

type Suite = {
  "name": String,
  "tests": Test[]
}
```

---

### suite

```txt
val suite: (name: String, tests: Test[]) -> Suite
```

Groups a list of `Test` values under a name.

```txt
val myTests = suite("math", [
  test("one plus one", () => [ expect(1 + 1).toBe(2) ])
])
```

---

### test

```txt
val test: (name: String, body: () -> Assertion[]) -> Test
```

Declares a single test case. All assertions in the body are evaluated before the test is marked failed.

```txt
test("string conversions", () => [
  expect((42).toString()).toBe("42"),
  expect(true.toString()).toBe("true")
])
```

---

### run {#run-test}

```txt
val run: (suites: Suite[]) -> Null
```

Executes all suites in order, prints results to stdout, and exits non-zero if any test failed.

```txt
run([unitTests, integrationTests])
```

Output format:

```txt
arithmetic
  ok  adds two positives
  FAIL  identity element
    expected: 1
    actual:   0

1 failed, 2 passed
```

---

### expect

```txt
val expect: (value: Json) -> Asserter
```

Wraps `value` in an `Asserter`. Call one assertion method to produce an `Assertion`.

```txt
expect(1 + 1).toBe(2)
expect(result).toSucceed()
expect(name).toSatisfy(s => length(s) > 0)
```

#### .toBe

Passes when `value` is deeply equal to `expected`.

#### .toBeNull

Passes when `value` is `null`.

#### .toSatisfy

Passes when `pred(value)` returns `true`.

#### .toSucceed

Passes when `value` has shape `{ "type": "success", ... }`.

#### .toFail

Passes when `value` has shape `{ "type": "failure", ... }`.

#### .toFailWith

Passes when `value` has shape `{ "type": "failure", "error": e }` and `e == message`.

---

### withFixture

```txt
val withFixture: (
  setup: () -> Json,
  teardown: (Json) -> Null,
  name: String,
  body: (Json) -> Assertion[]
) -> Test
```

Per-test setup/teardown with dependency injection. Builds a fixture with `setup`,
injects it into `body`, then tears it down with `teardown` — all inside one `test`.
Because assertion failures are **values** (not exceptions), teardown always runs,
even when the body's assertions fail. This is the functional alternative to keyword
`beforeEach`/`afterEach`; compose it into a reusable per-fixture helper with partial
application:

```txt
val withDb = withFixture(openDb, closeDb,)   // trailing comma = partial application

val tests = [
  withDb("inserts a row", (db) => [ expect(db.count()).toBe(1) ]),
  withDb("reads it back", (db) => [ expect(db.first().name).toBe("ada") ])
]
```

---

### report

```txt
val report: (s: Suite) -> Int32
```

Like `run`, but **returns the failure count** (0 = all passed) instead of exiting.
Statements after it execute regardless of outcome — the building block for
guaranteed `afterAll` teardown:

```txt
val failures = report(suite("db", tests))
closeConnections()                  // always runs, even on failure
if failures > 0 then exit(1) else null
```

---

### Setup & teardown (lifecycle)

Lin's eager test model needs no dedicated lifecycle keywords:

- **beforeAll** — a module-scope `val`/statement above the suite. Test bodies run
  eagerly as the `tests` array is built, so module-scope setup runs once before them.
- **afterAll** — statements after the run. `run` calls `exit(1)` on failure, so for
  teardown that must run even when a test fails, use `report` (which returns instead
  of exiting) and place cleanup after it.
- **beforeEach / afterEach** — use [`withFixture`](#withfixture), which runs
  setup+teardown around each test body and injects the fixture.

---

### Mocking with `replace`

A test-only `replace <name> = <expr>` statement overrides an imported export for the
whole test program — for mocking sibling modules and stdlib wrappers:

```txt
import { readFile } from "std/fs"

replace readFile = (path: String): Json => "mock contents of ${path}"
```

- **Replaced everywhere.** The override applies to every caller of that export —
  the test file, the module under test, and any transitive importer — because it
  swaps the export's single compiled symbol. A module that internally calls the
  replaced function sees the mock without any change to itself.
- **Stdlib is mockable** at the Lin-API level (`std/fs.readFile`, `std/time.now`,
  …). The polymorphic intrinsics (`print`, `map`, `filter`, `reduce`, `for`,
  `length`, `toString`, the async family) are **not** replaceable.
- **Type-checked.** The replacement body is checked against the export's real
  signature; a mismatch is a compile error.
- **Vals too.** `replace maxRetries = 99` overrides a non-function export.
- **Spies** are an ordinary mock closing over a module-level `var` cell to record
  calls/arguments, asserted after the run.
- **Test-only.** `replace` is permitted only in a `*.test.lin` file; using it in a
  `lin build`/`lin run` program is a hard compile error.

For worked examples see `examples/processes/` (mocking `std/process.exec`),
`examples/dijkstra/` (mocking `std/fs` read/write), and `examples/web-server/`
(mocking `std/template.render`); ADR-071 has the design.

---

## std/time

Timestamps, delays, and timing. All timestamps are Unix time in milliseconds.

Import:

```txt
import { now, sleep, toIso } from "std/time"
```

`Timer` is an opaque runtime type returned by `startTimer`.

---

### elapsed

```txt
val elapsed: (t: Timer) -> Int64
```

Returns the number of milliseconds that have passed since `t` was created by `startTimer`.

```txt
val t = startTimer()
heavyWork()
print("took ${toString(elapsed(t))}ms")
```

---

### format (time) {#format-time}

```txt
val format: (ts: Int64, pattern: String) -> String
```

Formats the Unix millisecond timestamp `ts` as a string using a strftime-style `pattern`. The timestamp is interpreted in UTC. Supported specifiers: `%Y %y %m %d %e %H %I %M %S %p %j %a %A %b %B %h` and `%%` (literal `%`); an unrecognised `%x` is emitted verbatim.

```txt
format(now(), "%Y-%m-%d")           // e.g. "2025-05-27"
format(now(), "%H:%M:%S")           // e.g. "14:32:07"
format(now(), "%Y-%m-%dT%H:%M:%S")  // e.g. "2025-05-27T14:32:07"
format(now(), "%a %B %d")           // e.g. "Tue May 27"
```

---

### fromIso

```txt
val fromIso: (s: String) -> Int64 | Error
```

Parses an ISO 8601 date/datetime string and returns the corresponding Unix millisecond timestamp. Timezone offsets are respected; bare dates (`2024-01-15`) are interpreted as UTC midnight.

```txt
fromIso("2024-01-15T10:30:00Z")   // 1705314600000
fromIso("2024-01-15")             // 1705276800000
fromIso("not a date")             // { "type": "error", "message": "..." }
```

---

### now

```txt
val now: () -> Int64
```

Returns the current Unix timestamp in milliseconds.

```txt
val start = now()
doWork()
print("elapsed: ${toString(now() - start)}ms")
```

---

### parse (time) {#parse-time}

```txt
val parse: (s: String, pattern: String) -> Int64 | Error
```

Parses the date/time string `s` using a strftime-style `pattern` and returns the Unix millisecond timestamp (UTC). Unspecified fields default to UTC midnight on 1970-01-01. Numeric specifiers (`%Y %y %m %d %e %H %M %S`) and literal characters are supported; textual names (`%a`/`%B`) are format-only, not parsed. Out-of-range fields and literal mismatches return an `Error`.

```txt
parse("2024-01-15", "%Y-%m-%d")              // 1705276800000
parse("15/01/2024 10:30", "%d/%m/%Y %H:%M")  // 1705314600000
parse("bad", "%Y-%m-%d")                     // { "type": "error", "message": "..." }
```

---

### sleep

```txt
val sleep: (ms: Int64) -> Null
```

Blocks the current thread for at least `ms` milliseconds.

```txt
sleep(1000)   // wait 1 second
```

---

### sleepMicros

```txt
val sleepMicros: (n: Int64) -> Null
```

Blocks the current thread for at least `n` microseconds. Microsecond-granularity counterpart to `sleep`, intended for tight timing loops such as software PWM.

```txt
sleepMicros(500)   // wait ~0.5 ms
```

---

### startTimer

```txt
val startTimer: () -> Timer
```

Starts a high-resolution wall-clock timer. Use `elapsed` to read the time since it was started.

```txt
val t = startTimer()
process(data)
print("processed in ${toString(elapsed(t))}ms")
```

---

### toIso

```txt
val toIso: (ts: Int64) -> String
```

Formats the Unix millisecond timestamp `ts` as an ISO 8601 string in UTC.

```txt
toIso(0)       // "1970-01-01T00:00:00.000Z"
toIso(now())   // e.g. "2025-05-27T14:32:07.123Z"
```
