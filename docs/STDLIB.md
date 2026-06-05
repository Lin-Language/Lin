# Lin Standard Library Specification

This document specifies the standard library for the Lin language. All modules are importable via the `std/` prefix.

## Index

### Modules

| Module | Description |
| --- | --- |
| [`std/string`](#stdstring) | String manipulation functions |
| [`std/iter`](#stditer) | Iterable combinators (over arrays, iterators, and streams) and iterator constructors |
| [`std/array`](#stdarray) | Array-shaped functions (indexable, materialised, ordered) |
| [`std/number`](#stdnumber) | Numeric parsing and conversion functions |
| [`std/bytes`](#stdbytes) | Byte-buffer slicing and endian (de)serialization |
| [`std/math`](#stdmath) | Mathematical functions |
| [`std/object`](#stdobject) | Object introspection functions |
| [`std/json`](#stdjson) | Type-directed JSON decode |
| [`std/yaml`](#stdyaml) | YAML parse and serialise |
| [`std/jq`](#stdjq) | Query Json values with jq filters |
| [`std/hash`](#stdhash) | Stable structural hash of any value |
| [`std/io`](#stdio) | stdin/stdout and terminal input |
| [`std/fs`](#stdfs) | Filesystem read and write |
| [`std/path`](#stdpath) | Path string manipulation |
| [`std/http`](#stdhttp) | HTTP client and server |
| [`std/net`](#stdnet) | UDP and TCP sockets |
| [`std/process`](#stdprocess) | Run and manage external processes |
| [`std/stream`](#stdstream) | Lazy, fallible byte/value streams over OS resources |
| [`std/compress`](#stdcompress) | Streaming gzip/DEFLATE byte-stream adapters |
| [`std/archive`](#stdarchive) | Tar splitting over a byte stream (untar / manifest / files) |
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
| [`codePointAt`](#codePointAt) | `(String, Int32) -> Int32` | Numeric codepoint value at index (O(n)); negative indices count from end |
| [`byteAt`](#byteAt) | `(String, Int32) -> Int32` | Raw UTF-8 byte at byte-index, O(1); fast scanning (no negative indices) |
| [`contains`](#contains) | `(String, String) -> Boolean` | Test whether needle is a substring |
| [`endsWith`](#endsWith) | `(String, String) -> Boolean` | Test whether string ends with suffix |
| [`fromCodePoints`](#fromCodePoints) | `(Int32[]) -> String` | Build a string from codepoint values |
| [`indexOf`](#indexOf-string) | `(String, String, Int32 = 0) -> Int32` | First occurrence of needle at/after `fromIndex`, or -1 |
| [`isBlank`](#isBlank) | `(String) -> Boolean` | True if string is empty or all whitespace |
| [`join`](#join) | `(String[], String) -> String` | Join array of strings with separator |
| [`lastIndexOf`](#lastIndexOf) | `(String, String, Int32 = length(s)) -> Int32` | Last occurrence of needle starting at/before `fromIndex`, or -1 |
| [`length`](#length-string) | `(String) -> Int32` | Codepoint count |
| [`lines`](#lines-string) | `(String) -> String[]` | Split a string into lines |
| [`padEnd`](#padEnd) | `(String, Int32, String = " ") -> String` | Pad to width on the right (pad defaults to a space) |
| [`padStart`](#padStart) | `(String, Int32, String = " ") -> String` | Pad to width on the left (pad defaults to a space) |
| [`repeat`](#repeat) | `(String, Int32) -> String` | Repeat a string n times |
| [`replace`](#replace) | `(String, String, String) -> String` | Replace first occurrence |
| [`replaceAll`](#replaceAll) | `(String, String, String) -> String` | Replace all occurrences |
| [`split`](#split) | `(String, String) -> String[]` | Split by delimiter |
| [`startsWith`](#startsWith) | `(String, String) -> Boolean` | Test whether string begins with prefix |
| [`substring`](#substring) | `(String, Int32, Int32 = length(s)) -> String` | Extract a slice by codepoint indices |
| [`toLower`](#toLower) | `(String) -> String` | Convert to lowercase |
| [`toString`](#toString) | `(Json) -> String` | Convert any value to its string representation |
| [`toUpper`](#toUpper) | `(String) -> String` | Convert to uppercase |
| [`trim`](#trim) | `(String) -> String` | Remove leading and trailing whitespace |
| [`trimEnd`](#trimEnd) | `(String) -> String` | Remove trailing whitespace only |
| [`trimStart`](#trimStart) | `(String) -> String` | Remove leading whitespace only |

**std/iter**

Combinators dispatch on the receiver type: **eager** (`U[]`) over an array or iterator, **lazy**
(`Stream<U>`) over a stream; terminals over a stream gain an `| Error` arm (ADR-077). The signatures
below show the array/iterator (eager) form; the per-function reference notes the stream form.

| Function | Signature | Summary |
| --- | --- | --- |
| [`concat`](#concat-iter) | `(Json[], Json[]) -> Json[]` | Concatenate two iterables |
| [`drop`](#drop-iter) | `<T>(T[], Int32) -> T[]` | All elements after first n |
| [`dropWhile`](#dropWhile-iter) | `<T>(T[], (T) -> Boolean) -> T[]` | Skip elements while predicate holds |
| [`every`](#every-iter) | `<T>(T[], (T) -> Boolean) -> Boolean` | True if all elements match |
| [`filter`](#filter-iter) | `<T>(T[], (T) -> Boolean) -> T[]` | Keep elements matching predicate |
| [`find`](#find-iter) | `<T>(T[], (T) -> Boolean) -> T \| Null` | First matching element, or null |
| [`flatMap`](#flatMap-iter) | `(Json[], (Json) -> Json[]) -> Json[]` | Map then flatten one level |
| [`flatten`](#flatten-iter) | `<T>(T[][]) -> T[]` | Flatten one level of nesting |
| [`for`](#for-iter) | `(Iterable, (Json) -> Json) -> Null` | Iterate over array, iterator, or stream |
| [`iter`](#iter) | `(() -> S, (S) -> Boolean, (S) -> S, (S) -> T) -> Iterator` | Build a custom iterator |
| [`iterOf`](#iterOf) | `(Json[]) -> Iterator` | Iterator over an array (element type erased into the iterator) |
| [`map`](#map-iter) | `<T, U>(T[], (T) -> U) -> U[]` | Transform each element |
| [`range`](#range) | `(Int32, Int32) -> Iterator` | Integer range `[start, end)`, step 1 |
| [`rangeStep`](#rangeStep) | `(Int32, Int32, Int32) -> Iterator` | Integer range with an explicit (possibly negative) step |
| [`reduce`](#reduce-iter) | `<T, U>(T[], U, (U, T) -> U) -> U` | Fold left with an accumulator |
| [`some`](#some-iter) | `<T>(T[], (T) -> Boolean) -> Boolean` | True if any element matches |
| [`take`](#take-iter) | `<T>(T[], Int32) -> T[]` | First n elements |
| [`takeWhile`](#takeWhile-iter) | `<T>(T[], (T) -> Boolean) -> T[]` | Elements until predicate fails |
| [`while`](#while-iter) | `(Json[], (Json) -> Boolean) -> Null` | Iterate, stopping when callback returns false |

**std/array**

Array-shaped functions only — these need a materialised, indexable, ordered array. For the iterable
combinators (`map`/`filter`/`reduce`/`for`/`take`/…) and iterator constructors (`range`/`iter`/…), see
[`std/iter`](#stditer).

| Function | Signature | Summary |
| --- | --- | --- |
| [`append`](#append) | `<T>(T[], T) -> T[]` | Non-mutating single-element append |
| [`arrayAllocate`](#arrayAllocate) | `(Int32) -> Json[]` | Allocate an array of n nulls |
| [`arrayAllocateFilled`](#arrayAllocateFilled) | `(Int32, Json) -> Json[]` | Allocate an array of n copies of a fill value |
| [`at`](#at-array) | `<T, D>(T[], Int32, D = null) -> T \| D` | Element at index, or the default (`null` if omitted) when out of bounds; negative indices count from end |
| [`chunk`](#chunk) | `<T>(T[], Int32) -> T[][]` | Split into n-sized sub-arrays |
| [`compact`](#compact) | `(Json[]) -> Json[]` | Remove null elements |
| [`countBy`](#countBy) | `<T>(T[], (T) -> String) -> { String: Int32 }` | Frequency map by key function |
| [`groupBy`](#groupBy) | `<T>(T[], (T) -> String) -> { String: T[] }` | Group into a typed map of arrays by key function |
| [`indexOf`](#indexOf-array) | `<T>(T[], T, Int32 = 0) -> Int32` | First index of value at/after `fromIndex` (negatives count from end), or -1 |
| [`length`](#length-array) | `(Json) -> Int32` | Length of array, string, or object |
| [`max`](#max-array) | `(Number[]) -> Number` | Maximum element |
| [`maxBy`](#maxBy) | `<T>(T[], (T) -> Number) -> T` | Element with the largest key |
| [`min`](#min-array) | `(Number[]) -> Number` | Minimum element |
| [`minBy`](#minBy) | `<T>(T[], (T) -> Number) -> T` | Element with the smallest key |
| [`partition`](#partition) | `<T>(T[], (T) -> Boolean) -> T[][]` | Split into passing and failing (`result[0]` pass, `result[1]` fail) |
| [`prepend`](#prepend) | `<T>(T[], T) -> T[]` | Non-mutating single-element prepend |
| [`product`](#product) | `(Number[]) -> Number` | Product of all elements |
| [`push`](#push) | `<T>(T[], T) -> Null` | Append an element to an array in place |
| [`reverse`](#reverse) | `<T>(T[]) -> T[]` | Return a reversed copy |
| [`scan`](#scan) | `<T, U>(T[], U, (U, T) -> U) -> U[]` | Reduce returning all intermediate values |
| [`set`](#set-array) | `<T>(T[], Int32, T) -> Null` | Set an element by index in place |
| [`slice`](#slice) | `<T>(T[], Int32, Int32 = length(arr)) -> T[]` | Sub-buffer copy; preserves element type; `end` optional, negatives count from end |
| [`sort`](#sort) | `<T>(T[], (T, T) -> Int32) -> T[]` | Return sorted copy using comparator |
| [`sortBy`](#sortBy) | `<T>(T[], (T) -> Json) -> T[]` | Return sorted copy using key extractor |
| [`sum`](#sum) | `(Number[]) -> Number` | Sum all elements |
| [`unique`](#unique) | `<T>(T[]) -> T[]` | Remove duplicate elements (deep equality) |
| [`zip`](#zip) | `<A, B>(A[], B[]) -> [A, B][]` | Pair elements by index |

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
| [`entries`](#entries) | `(Json) -> [String, Json][]` | Array of `[key, value]` pairs (tag-aware: object or typed map) |
| [`fromEntries`](#fromEntries) | `([String, Json][]) -> {}` | Build an object from key-value pairs |
| [`get`](#get) | `<T, D>({ String: T }, String, D = null) -> T \| D` | Value at key, or the default (`null` if omitted) when absent (the `m[k] ?? default` idiom) |
| [`isEmpty`](#isEmpty) | `(Json) -> Boolean` | True if object, array, or string is empty |
| [`keys`](#keys) | `(Json) -> String[]` | Array of object keys (tag-aware: object or typed map) |
| [`mapValues`](#mapValues) | `<V,W>({ String: V }, (V) -> W) -> { String: W }` | Transform all values, keeping keys |
| [`merge`](#merge) | `<T>({ String: T }, { String: T }) -> { String: T }` | Shallow-merge two typed maps (right wins on conflict) |
| [`omit`](#omit) | `<T>({ String: T }, String[]) -> { String: T }` | Return typed map without specified keys |
| [`pick`](#pick) | `<T>({ String: T }, String[]) -> { String: T }` | Return typed map with only specified keys |
| [`values`](#values) | `(Json) -> Json[]` | Array of object values (tag-aware: object or typed map) |

**std/yaml**

| Function | Signature | Summary |
| --- | --- | --- |
| [`parse`](#parse-yaml) | `(String) -> Json \| Error` | Parse one YAML document |
| [`parseAll`](#parseAll) | `(String) -> Json[] \| Error` | Parse a `---`-separated multi-document stream |
| [`stringify`](#stringify-yaml) | `(Json) -> String` | Serialise a value to block-style YAML |
| [`stringifyAll`](#stringifyAll) | `(Json[]) -> String` | Serialise values to a `---`-separated YAML stream |

**std/jq**

| Function | Signature | Summary |
| --- | --- | --- |
| [`jq`](#jq) | `(Json, String) -> Json[] \| Error` | Run a jq filter, collecting all outputs |
| [`jqFirst`](#jqFirst) | `(Json, String) -> Json \| Error` | Run a jq filter, returning the first output or `Null` |

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

**std/stream**

Stream-specific sources, adapters, sinks, and terminals. The unified combinators
(`map`/`filter`/`take`/`drop`/`reduce`/`for`/…) are **not** exported here — they come from
[`std/iter`](#stditer) and dispatch to the lazy stream backend on a stream receiver (ADR-077).

| Function | Signature | Summary |
| --- | --- | --- |
| [`readStream`](#readStream) | `(String) -> Stream<UInt8[]>` | Open a file as a lazy byte stream |
| [`lines`](#lines-stream) | `(Stream<UInt8[]>) -> Stream<String>` | View a byte stream as a stream of lines |
| [`linesMax`](#linesMax) | `(Stream<UInt8[]>, Int32) -> Stream<String>` | Like `lines` with an explicit per-line byte cap |
| [`chunks`](#chunks) | `(Stream<UInt8[]>, Int32) -> Stream<UInt8[]>` | Re-chunk a byte stream to fixed-size windows |
| [`readText`](#readText) | `(Stream<UInt8[]>) -> String \| Error` | Drain a byte stream to one String |
| [`collect`](#collect) | `(Stream<UInt8[]>) -> UInt8[] \| Error` | Drain a byte stream to one byte buffer |
| [`writeStream`](#writeStream) | `<T>(Stream<T>, String) -> Stream<T>` | Build a RAW sink: write each item's bytes verbatim, no separator |
| [`writeLines`](#writeLines) | `<T>(Stream<T>, String) -> Stream<T>` | Build a line-oriented sink: write each item followed by a newline |
| [`drain`](#drain) | `<T>(Stream<T>) -> Null \| Error` | Run a pipeline on the calling thread |
| [`promise`](#promise-stream) | `<T>(Stream<T>) -> Json` | Move a pipeline to a worker thread; `Promise<Null \| Error>` |
| [`close`](#close-stream) | `(Stream<T>) -> Null` | Release the file/socket now (optional; idempotent) |

**std/compress**

Lazy streaming (de)compression adapters over a `Stream<UInt8[]>`. Each transforms bytes
incrementally (constant memory) and threads decode errors in-band like every other adapter.

| Function | Signature | Summary |
| --- | --- | --- |
| [`gunzip`](#gunzip) | `(Stream<UInt8[]>) -> Stream<UInt8[]>` | Decompress a gzip-framed byte stream |
| [`gzip`](#gzip) | `(Stream<UInt8[]>) -> Stream<UInt8[]>` | Compress a byte stream into the gzip container |
| [`inflate`](#inflate) | `(Stream<UInt8[]>) -> Stream<UInt8[]>` | Decompress a raw DEFLATE byte stream |
| [`deflate`](#deflate) | `(Stream<UInt8[]>) -> Stream<UInt8[]>` | Compress a byte stream as raw DEFLATE |

**std/template**

| Function | Signature | Summary |
| --- | --- | --- |
| [`render`](#render) | `(String, {}) -> String \| Error` | Load a `.jinja` file and render it with a data record |
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

`codePointAt` is codepoint-indexed and therefore **O(n)** per call — a loop indexing `0..length(s)` with it is O(n²). For fast byte/ASCII scanning use [`byteAt`](#byteAt), which is O(1).

---

### byteAt

```txt
val byteAt: (s: String, index: Int32) -> Int32
```

Returns the raw UTF-8 **byte** (`0..255`) at byte-index `index`, or `-1` if the index is negative or out of range. Unlike [`codePointAt`](#codePointAt) this is **O(1)** — a direct indexed load — so scanning a whole string (`0..length(s)`) is O(n) rather than O(n²). For pure-ASCII text `byteAt(s, i) == codePointAt(s, i)`; for multi-byte UTF-8 it exposes the individual encoding bytes. Use it for tokenizers, parsers, and other byte-level string scanning written in Lin.

Unlike [`at`](#at) and [`codePointAt`](#codePointAt), `byteAt` does **not** support negative indexing from the end: it is a raw byte primitive (a negative index simply returns `-1`). Negative-from-end indexing belongs on the codepoint/element accessors, not the byte primitive.

```txt
byteAt("ABC", 0)   // 65
byteAt("ABC", 2)   // 67
byteAt("AB", 5)    // -1
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
val indexOf: (s: String, needle: String, fromIndex: Int32 = 0) -> Int32
```

Returns the zero-based codepoint index of the first occurrence of `needle` within `s` at or after `fromIndex`, or `-1` if not found. `fromIndex` is optional and defaults to `0` (search the whole string).

```txt
indexOf("hello world", "world")   // 6
indexOf("hello", "xyz")           // -1
indexOf("abcabc", "bc")           // 1
indexOf("abcabc", "bc", 2)        // 4
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
val lastIndexOf: (s: String, needle: String, fromIndex: Int32 = length(s)) -> Int32
```

Returns the zero-based codepoint index of the **last** occurrence of `needle` within `s` whose start is at or before `fromIndex`, or `-1` if not found. `fromIndex` is optional and defaults to the string length (search the whole string).

```txt
lastIndexOf("abcabc", "b")         // 4
lastIndexOf("/usr/local/bin", "/")  // 10
lastIndexOf("hello", "xyz")        // -1
lastIndexOf("abcabc", "bc")        // 4
lastIndexOf("abcabc", "bc", 2)     // 1
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
val padEnd: (s: String, width: Int32, pad: String = " ") -> String
```

Returns `s` padded on the right with repetitions of `pad` until the total codepoint length reaches `width`. If `s` is already at least `width` codepoints long, returns `s` unchanged. `pad` is optional and defaults to a single space `" "`.

```txt
padEnd("hi", 5, ".")    // "hi..."
padEnd("hi", 5, "-*")   // "hi-*-"
padEnd("hello", 3, ".")  // "hello"
padEnd("hi", 4)          // "hi  "
```

---

### padStart

```txt
val padStart: (s: String, width: Int32, pad: String = " ") -> String
```

Returns `s` padded on the left with repetitions of `pad` until the total codepoint length reaches `width`. If `s` is already at least `width` codepoints long, returns `s` unchanged. `pad` is optional and defaults to a single space `" "`.

```txt
padStart("42", 5, "0")    // "00042"
padStart("hi", 5, ".")    // "...hi"
padStart("hello", 3, ".")  // "hello"
padStart("5", 3)           // "  5"
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
val substring: (s: String, start: Int32, end: Int32 = length(s)) -> String
```

Returns the slice of `s` covering codepoint indices `[start, end)`. `end` is optional and defaults to the string length, so `substring(s, start)` returns the slice from `start` to the end. Negative indices count from the end: `-1` refers to the last character's position (`length - 1`), `-2` to the second-to-last, etc. Indices are resolved by adding `length` to any negative value, then clamping to `[0, length]`. If `start >= end` (after resolving negatives), returns `""`.

```txt
substring("hello", 1, 3)    // "el"
substring("hello", 0, 5)    // "hello"
substring("hello", 2)       // "llo"    (omitted end defaults to length)
substring("hello", 0, -1)   // "hell"   (strip last character)
substring("hello", 1, -1)   // "ell"
substring("hello", -3, -1)  // "ll"
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

## std/iter

Iterable combinators and iterator constructors. A *combinator* works over any **iterable source** — an
array, an `Iterator`, or a `Stream` — and **dispatches on the receiver's static type** (its first
argument, in dot-application terms): the same name is **eager** over an array/iterator (returning a
materialised `U[]`) and **lazy** over a stream (returning a `Stream<U>` adapter that reads nothing until
a terminal drives it). Terminals over a stream gain an `| Error` arm, because a stream read can fail
mid-traversal (ADR-077; stream semantics in spec §27.9). Eager combinators are non-mutating and return
new values.

Import:

```txt
import { map, filter, reduce, take } from "std/iter"
import { range, iter } from "std/iter"
```

The headline win — one combinator vocabulary across arrays and streams. The same chain that runs eagerly
over an array runs **lazily, with bounded memory** over a stream, simply because the receiver is a
`Stream`:

```txt
import { map, drop, take, reduce } from "std/iter"
import { readStream } from "std/stream"

// Drop a header line, take 4 records, sum each line's length — lazily, one line at a time:
val total = readStream("data.csv")
  .lines()                       // Stream<String>
  .drop(1)                       // Stream<String>  (lazy adapter)
  .take(4)                       // Stream<String>  (lazy adapter)
  .map(line => line.length())    // Stream<Int32>
  .reduce(0, (acc, n) => acc + n)                     // Int32 | Error  (terminal)

match total
  is Error => print("read failed: ${total["message"]}")
  else     => print("sum = ${total}")
```

> Over a stream, a combinator **consumes** its input, so a stream flows through a single chain —
> using the same stream value twice is a compile-time error. See [`std/stream`](#stdstream) for details.
>
> v1 limitation: dispatch fires at a **concrete** combinator call with a `Stream` receiver. A stream
> passed through a user-defined generic `Iterable` parameter and combined inside that function stays
> **array-shaped** (eager) — the safe resolution; the lazy form is forgone, never miscompiled.

### Optional index parameter

Every combinator callback OPTIONALLY receives a **0-based `Int32` SOURCE index** as a trailing
parameter — the JS `forEach((item, idx) => …)` model. A 1-arg callback stays valid and unchanged
(this is opt-in by arity, fully backward-compatible). It applies to `for`/`map`/`filter`/`reduce`/
`while` and the derived `find`/`some`/`every`/`flatMap`/`takeWhile`/`dropWhile` (and
`std/array`'s `partition`). For `reduce`, the index is the **third** parameter: `(acc, item, i)`.

```txt
["a", "b", "c"].map((x, i) => "${i}: ${x}")   // ["0: a", "1: b", "2: c"]
["a", "b"].for((item, i) => print("${i}: ${item}"))
[1, 1, 1].reduce(0, (acc, x, i) => acc + i)    // 0 + 0 + 1 + 2 = 3
```

The index is always the **source position**, even for `filter`/`takeWhile`/`dropWhile` where the
output position differs:

```txt
[10, 20, 30, 40].filter((x, i) => i % 2 == 0)  // [10, 30]  (source indices 0, 2)
```

The index parameter is `Int32`: an unannotated index param infers `Int32`; an explicit `Int32`
annotation is allowed; any other annotation (e.g. `(x, i: String) => …`) is a compile error. The
key-extractor/aggregator combinators (`sortBy`/`minBy`/`maxBy`/`groupBy`/`countBy`) do **not** take an
index — element position is meaningless to a key function. Indexed callbacks are **array/iterator-only**;
the runtime `Stream` combinators keep 1-arg callbacks (see [`std/stream`](#stdstream)).

---

### map (iter) {#map-iter}

```txt
val map: <T, U>(src: T[] | Iterator | Stream<T>, f: (T[, i: Int32]) -> U) -> U[] | Stream<U>
```

Applies `f` to each element in order. `f` optionally receives the 0-based source index as a second
parameter (`(x, i) => …`); a 1-arg `f` is unchanged. **Array/Iterator** → eager `U[]`. **Stream** → lazy `Stream<U>`
(a transform adapter; `f` runs once per item as the item is pulled). For a monomorphic scalar array with
a capture-less literal lambda, the body is inlined into a flat loop with no per-element boxing (ADR-069).

```txt
[1, 2, 3].map(x => x * 2)                 // [2, 4, 6]
["a", "b"].map(s => s.toUpper())          // ["A", "B"]
readStream("in.csv").lines().map(line => line.toUpper())   // Stream<String> (lazy)
```

---

### filter (iter) {#filter-iter}

```txt
val filter: <T>(src: T[] | Iterator | Stream<T>, f: (T[, i: Int32]) -> Boolean) -> T[] | Stream<T>
```

Keeps elements for which `f` returns `true`. `f` optionally receives the 0-based source index as a
second parameter; the index is the source position, even though output positions differ. **Array/Iterator** → eager `T[]`. **Stream** → lazy
`Stream<T>`.

```txt
[1, 2, 3, 4].filter(x => x > 2)           // [3, 4]
readStream("app.log").lines().filter(line => line.contains("ERROR"))   // Stream<String> (lazy)
```

---

### reduce (iter) {#reduce-iter}

```txt
val reduce: <T, U>(src: T[] | Iterator | Stream<T>, init: U, f: (U, T[, i: Int32]) -> U) -> U | (U | Error)
```

Folds left-to-right from `init`. `f` receives the accumulator first and the current element second; it
optionally receives the 0-based source index as a **third** parameter (`(acc, x, i) => …`).
**Array/Iterator** → eager `U`. **Stream** → **terminal** returning `U | Error` (it drives the stream to
completion on the calling thread; a read fault surfaces as `Error`). For a monomorphic scalar
accumulator with a capture-less literal reducer, the accumulator is carried unboxed (ADR-069).

```txt
[1, 2, 3, 4].reduce(0, (acc, x) => acc + x)   // 10
val n = readStream("nums.txt")
  .lines()
  .reduce(0, (acc, line) => acc + line.parseInt32())   // Int32 | Error
```

---

### for (iter) {#for-iter}

```txt
val for: (src: Json[] | Iterator | Stream, f: (Json[, i: Int32]) -> Json) -> Null | (Null | Error)
```

Iterates over each element, calling `f` (its return value is discarded). `f` optionally receives the
0-based source index as a second parameter (`(item, i) => …`); array/iterator only. **Array/Iterator** → `Null`.
**Stream** → **terminal** returning `Null | Error`: EOF ends the loop normally (`Null`), and a read
`Error` mid-traversal becomes the result (spec §27.9.4). Lin has no `for…in`; iteration is always
`.for(fn)`.

```txt
[1, 2, 3].for(x => print(toString(x)))
range(0, 5).for(i => print(toString(i)))

val outcome = readStream("in.log").lines().for(line => print(line))
match outcome
  is Error => print("read failed: ${outcome["message"]}")
  else     => null
```

---

### while (iter) {#while-iter}

```txt
val while: <T>(src: T[] | Iterator | Stream, f: (T[, i: Int32]) -> Boolean) -> Null | (Null | Error)
```

`f` optionally receives the 0-based source index as a second parameter; array/iterator only.

Iterates calling `f` with each element, stopping as soon as `f` returns `false`. **Array/Iterator** →
`Null`; the short-circuit primitive behind `some`/`every`/`find`. **Stream** → terminal `Null | Error`.

```txt
[1, 2, -3, 4].while(x => x >= 0)   // visits 1, 2, stops at -3
```

---

### take (iter) {#take-iter}

```txt
val take: <T>(src: T[] | Iterator | Stream<T>, n: Int32) -> T[] | Stream<T>
```

The first `n` elements. **Array/Iterator** → eager `T[]` (a copy of the whole array if `n >= length`).
**Stream** → lazy `Stream<T>` that ends after `n` items and stops pulling upstream.

```txt
take([1, 2, 3, 4], 2)   // [1, 2]
readStream("huge.log")
  .lines()
  .take(100)
  .for(line => print(line))   // first 100 lines only
```

---

### drop (iter) {#drop-iter}

```txt
val drop: <T>(src: T[] | Iterator | Stream<T>, n: Int32) -> T[] | Stream<T>
```

Skips the first `n` elements. **Array/Iterator** → eager `T[]` (`[]` if `n >= length`). **Stream** →
lazy `Stream<T>`.

```txt
drop([1, 2, 3, 4], 2)   // [3, 4]
readStream("data.csv").lines().drop(1)   // skip the header line, lazily
```

---

### takeWhile (iter) {#takeWhile-iter}

```txt
val takeWhile: <T>(src: T[] | Iterator | Stream<T>, f: (T[, i: Int32]) -> Boolean) -> T[] | Stream<T>
```

Leading elements for which `f` returns `true`; stops at the first `false`. **Array/Iterator** → eager
`T[]`. **Stream** → lazy `Stream<T>`.

```txt
[1, 2, 3, 4, 1].takeWhile(x => x < 3)   // [1, 2]
```

---

### dropWhile (iter) {#dropWhile-iter}

```txt
val dropWhile: <T>(src: T[] | Iterator | Stream<T>, f: (T[, i: Int32]) -> Boolean) -> T[] | Stream<T>
```

Drops leading elements while `f` returns `true`, then keeps the rest unchanged. **Array/Iterator** →
eager `T[]`. **Stream** → lazy `Stream<T>`.

```txt
[1, 2, 3, 4, 1].dropWhile(x => x < 3)   // [3, 4, 1]
```

---

### flatMap (iter) {#flatMap-iter}

```txt
val flatMap: (src: Json[] | Iterator | Stream, f: (Json[, i: Int32]) -> Json[]) -> Json[] | Stream
```

Applies `f` to each element and concatenates the resulting arrays. **Array/Iterator** → eager `Json[]`.
**Stream** → lazy stream that yields each inner element in turn.

```txt
[1, 2, 3].flatMap(x => [x, x * 2])   // [1, 2, 2, 4, 3, 6]
```

---

### flatten (iter) {#flatten-iter}

```txt
val flatten: <T>(src: T[][]) -> T[]
```

Removes one level of nesting: a `T[][]` becomes a `T[]`. (Backed by `flatMap(x => x)`, so a stream of
arrays also flattens lazily; the array form is generic over the inner element type `T`.)

```txt
flatten([[1, 2], [3, 4]])   // [1, 2, 3, 4]
```

---

### concat (iter) {#concat-iter}

```txt
val concat: (a: Json[] | Iterator | Stream, b: Json[] | Iterator | Stream) -> Json[] | Stream
```

Yields all of `a` followed by all of `b`. **Arrays/Iterators** → eager `Json[]`. **Streams** → lazy
(both stream arguments are consumed/moved into the concatenated source).

```txt
concat([1, 2], [3, 4])   // [1, 2, 3, 4]
```

---

### find (iter) {#find-iter}

```txt
val find: <T>(src: T[] | Iterator | Stream, f: (T[, i: Int32]) -> Boolean) -> T | Null | (T | Null | Error)
```

The first element for which `f` returns `true`, or `null` if none. **Array/Iterator** → `T | Null`
(the match or `null`). **Stream** → **terminal** `T | Null | Error`.

```txt
[1, 2, 3].find(x => x > 1)   // 2
```

---

### some (iter) {#some-iter}

```txt
val some: <T>(src: T[] | Iterator | Stream, f: (T[, i: Int32]) -> Boolean) -> Boolean | (Boolean | Error)
```

`true` if `f` returns `true` for at least one element (short-circuits). **Array/Iterator** → `Boolean`.
**Stream** → **terminal** `Boolean | Error`.

```txt
[1, 2, 3].some(x => x > 2)   // true
```

---

### every (iter) {#every-iter}

```txt
val every: <T>(src: T[] | Iterator | Stream, f: (T[, i: Int32]) -> Boolean) -> Boolean | (Boolean | Error)
```

`true` if `f` returns `true` for every element (short-circuits on the first `false`); `true` for an
empty source. **Array/Iterator** → `Boolean`. **Stream** → **terminal** `Boolean | Error`.

```txt
[1, 2, 3].every(x => x > 0)   // true
```

---

### range

```txt
val range: (start: Int32, end: Int32) -> Iterator
```

Returns an iterator yielding integers from `start` up to (but not including) `end`, stepping by `1`. If
`start >= end`, the iterator is empty. For a custom or negative step, use [`rangeStep`](#rangeStep).

```txt
range(0, 3).for(i => print(toString(i)))   // prints 0, 1, 2
range(1, 4).map(i => i * 2)                // [2, 4, 6]
```

---

### rangeStep

```txt
val rangeStep: (start: Int32, end: Int32, step: Int32) -> Iterator
```

Returns an iterator yielding integers from `start` toward `end` (exclusive), advancing by `step`. A
positive `step` counts up while `i < end`; a negative `step` counts down while `i > end`; a `step` of
`0` yields an empty iterator.

```txt
rangeStep(0, 10, 2).for(i => print(toString(i)))   // 0, 2, 4, 6, 8
rangeStep(5, 0, -1).map(i => i)                    // [5, 4, 3, 2, 1]
```

---

### iter

```txt
val iter: (init: () -> S, hasNext: (S) -> Boolean, next: (S) -> S, value: (S) -> T) -> Iterator
```

Constructs a custom iterator from four functions: `init` produces the initial state, `hasNext` tests
whether to continue, `next` advances the state, and `value` extracts the current element.

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

Returns an iterator that yields each element of `arr` in order. Produces a first-class iterator value
that can be passed around before consumption.

```txt
val it = iterOf([10, 20, 30])
it.for(x => print(toString(x)))   // prints 10, 20, 30
```

---

## std/array

Array-shaped functions — these operate on a materialised, indexable, ordered array. The iterable
combinators (`map`/`filter`/`reduce`/`for`/`while`/`take`/`drop`/`flatMap`/`takeWhile`/`dropWhile`/
`flatten`/`concat`/`find`/`some`/`every`) and the iterator constructors (`range`/`rangeStep`/`iter`/
`iterOf`) now live in [`std/iter`](#stditer). All transformation functions here are non-mutating and
return new values (except the in-place `push`/`set`).

Import:

```txt
import { push, slice, sort, sum } from "std/array"
```

---

### append

```txt
val append: <T>(arr: T[], item: T) -> T[]
```

Returns a new array with `item` added at the end. Does not modify `arr`. For in-place mutation, use `push`. Generic over the element type `T`, so the element type is enforced: `append(intArr, "s")` is a compile error. The result preserves the input's element representation: appending to a flat scalar array (e.g. `UInt8[]`, `Int32[]`) yields a flat array of the same type (a numeric LITERAL item adopts the array's element width, so `b.append(3)` on a `UInt8[]` stays `UInt8[]`), so byte-level consumers still read packed bytes; a `Json[]` stays tagged.

```txt
append([1, 2], 3)    // [1, 2, 3]
val ss: String[] = []
append(ss, "hello")  // ["hello"]
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
val at: <T, D>(arr: T[], index: Int32, default: D = null) -> T | D
```

Safe accessor with an optional default. Returns the element at `index`, or `default` when the resolved index is out of bounds (so it never traps). Negative indices count from the end: `-1` is the last element, `-2` is second-to-last.

The default's type `D` is an **independent** type parameter, so the result is `T | D` and the default's type never pollutes the element type `T`:

- omitting the default gives `default = null`, so `at(arr, i)` is `T | Null` — the safe bounds-checked read;
- a same-typed default collapses the union: over an `Int32[]`, `at(arr, i, 0)` is `Int32 | Int32 = Int32`, a bare scalar usable directly in arithmetic with no `null` guard (the "definitely present" form);
- a differently-typed default keeps both arms: `at(arr, i, "n/a")` over an `Int32[]` is `Int32 | String`.

This single function subsumes the old `at`/`atOr` pair.

```txt
at([10, 20, 30], 0)         // 10
at([10, 20, 30], -1)        // 30
at([], 0)                   // null               (omitted default -> T | Null)
[10, 20, 30].at(1, -1)      // 20
[10, 20, 30].at(5, -1)      // -1    (out of bounds -> default)
[10, 20, 30].at(-1, -1)     // 30    (negative index wraps)
[10, 20, 30].at(-9, 99)     // 99    (out-of-range negative -> default)
[10, 20, 30].at(9, "n/a")   // "n/a" (independent default type -> Int32 | String)
```

---

### chunk

```txt
val chunk: <T>(arr: T[], size: Int32) -> T[][]
```

Splits `arr` into sub-arrays of length `size`, preserving the element type `T`. The final chunk may be shorter if `arr` does not divide evenly. `size` must be at least 1. An empty array literal cannot pin `T` — annotate the input (e.g. `val xs: Int32[] = []`) in that case.

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

### countBy

```txt
val countBy: <T>(arr: T[], f: (T) -> String) -> { String: Int32 }
```

Returns a typed map (`{ String: Int32 }`, ADR-082) from each distinct key (produced by `f`) to the number of elements that produced that key.

```txt
["apple", "banana", "avocado", "blueberry"].countBy(s => s.at(0))
// { "a": 2, "b": 2 }

[1, 2, 3, 4, 5].countBy(n => if n % 2 == 0 then "even" else "odd")
// { "odd": 3, "even": 2 }
```

---

### groupBy

```txt
val groupBy: <T>(arr: T[], f: (T) -> String) -> { String: T[] }
```

Returns a typed map (`{ String: T[] }`, ADR-082) where each key is a value returned by `f`, and the corresponding value is an array of all elements that produced that key. Within each group, elements keep their encounter order. The map's keys are in **hash order** (the typed-map backing is hashed, not insertion-ordered).

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
val indexOf: <T>(arr: T[], target: T, fromIndex: Int32 = 0) -> Int32
```

Returns the zero-based index of the first element deeply equal to `target` at or after `fromIndex`, or `-1` if not found. `fromIndex` is optional and defaults to `0` (search the whole array); a negative `fromIndex` counts from the end (`length(arr) + fromIndex`).

```txt
[10, 20, 30].indexOf(20)   // 1
[1, 2, 3].indexOf(9)       // -1
[1, 2, 1, 2].indexOf(2, 2) // 3
[1, 2, 1, 2].indexOf(1, -1) // -1   (search starts at index 3)
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
val maxBy: <T>(arr: T[], f: (T) -> Number) -> T
```

Returns the element of `arr` for which `f` produces the largest value. The array must be non-empty. Generic over the element type `T`: `f` is checked against the element type, and the result is a bare `T`.

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
val minBy: <T>(arr: T[], f: (T) -> Number) -> T
```

Returns the element of `arr` for which `f` produces the smallest value. The array must be non-empty. Generic over the element type `T`: `f` is checked against the element type, and the result is a bare `T`.

```txt
[{ "name": "Alice", "age": 30 }, { "name": "Bob", "age": 25 }]
  .minBy(p => p["age"])
// { "name": "Bob", "age": 25 }
```

---

### partition

```txt
val partition: <T>(arr: T[], f: (T[, i: Int32]) -> Boolean) -> T[][]
```

Returns a two-element array `[passing, failing]` (typed `T[][]`) where `passing` (`result[0]`) contains all elements for which `f` returned `true` and `failing` (`result[1]`) contains the rest, both in their original order. An empty array literal cannot pin `T` — annotate the input in that case.

```txt
val [evens, odds] = [1, 2, 3, 4, 5].partition(x => x % 2 == 0)
// evens: [2, 4],  odds: [1, 3, 5]
```

---

### prepend

```txt
val prepend: <T>(arr: T[], item: T) -> T[]
```

Returns a new array with `item` added at the beginning. Does not modify `arr`. Generic over `T` (same element-type enforcement as `append`). Like `append`, the result preserves the input's element representation (a flat `UInt8[]`/`Int32[]` stays flat; a `Json[]` stays tagged).

```txt
prepend([2, 3], 1)    // [1, 2, 3]
val ss: String[] = []
prepend(ss, "hello")  // ["hello"]
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
val push: <T>(arr: T[], item: T) -> Null
```

Appends `item` to `arr` in place. This is one of the few mutating operations in Lin — it modifies the array that was passed in. Generic over the element type `T`, so the element type is enforced: `push(intArr, "s")` is a compile error (ADR-085). An empty accumulator literal must be annotated so `T` is pinned: an evidence-free `[]` cannot infer its element type (ADR-084) — `val xs: Int32[] = []`, not `val xs = []`.

```txt
val xs: Int32[] = []
push(xs, 1)
push(xs, 2)
// xs is now [1, 2]
```

---

### reverse

```txt
val reverse: <T>(arr: T[]) -> T[]
```

Returns a new array with the elements in reversed order, preserving the element type `T`.

```txt
[1, 2, 3].reverse()   // [3, 2, 1]
```

---

### scan

```txt
val scan: <T, U>(arr: T[], init: U, f: (U, T) -> U) -> U[]
```

Like `reduce`, but returns an array (`U[]`) of all intermediate accumulator values including the initial value. The result always has `length(arr) + 1` elements.

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
val slice: <T>(arr: T[], start: Int32, end: Int32 = length(arr)) -> T[]
```

Returns a copy of the elements in the half-open range `[start, end)`. `end` is optional and defaults to the array length, so `slice(arr, start)` returns the elements from `start` to the end. Negative indices count from the end (`-1` is the last element's position): they are resolved by adding `length(arr)` to any negative value, then clamped to `[0, length(arr)]`. The element type is preserved: slicing a `UInt8[]` yields a `UInt8[]`, an `Int32[]` an `Int32[]`, and a `Json[]` a `Json[]`. Also re-exported from `std/bytes` (with both bounds explicit). There is no range-index syntax (`arr[a..b]`).

```txt
[10, 20, 30, 40, 50].slice(1, 4)   // [20, 30, 40]
[1, 2, 3, 4, 5].slice(1)            // [2, 3, 4, 5]
[1, 2, 3, 4, 5].slice(1, -1)        // [2, 3, 4]
[1, 2, 3, 4, 5].slice(-2)           // [4, 5]
```

---

### sort

```txt
val sort: <T>(arr: T[], compare: (T, T) -> Int32) -> T[]
```

Returns a new array with elements sorted according to `compare`. The comparator must return a negative number if the first argument should come first, a positive number if the second should come first, and `0` if they are equal. Does not modify `arr`. Generic over the element type `T`: the comparator is checked against the array's element type (a mistyped comparator is a compile error), and the result is a `T[]` that preserves the element type. The sort is **stable** (equal elements keep their input order).

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
val sortBy: <T>(arr: T[], f: (T) -> Json) -> T[]
```

Returns a new array sorted in ascending order by the key extracted by `f`. Keys are compared using Lin's natural ordering (numbers numerically, strings lexicographically). Does not modify `arr`. Generic over the element type `T`: `f` is checked against the element type, and the result is a `T[]` that preserves the element type. The key value is left as `Json` (it only needs to be comparable).

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

### unique

```txt
val unique: <T>(arr: T[]) -> T[]
```

Returns a new array with duplicate elements removed, preserving the order of first occurrence and the element type `T`. Equality uses deep structural comparison.

```txt
unique([1, 2, 1, 3, 2])                      // [1, 2, 3]
unique(["a", "b", "a"])                       // ["a", "b"]
unique([{ "x": 1 }, { "x": 1 }, { "x": 2 }]) // [{ "x": 1 }, { "x": 2 }]
```

---

### zip

```txt
val zip: <A, B>(a: A[], b: B[]) -> [A, B][]
```

Returns an array of two-element tuples pairing elements from `a` and `b` by index, preserving each input's element type (`[A, B]`). The length of the result equals the length of the shorter input.

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
import { push, length } from "std/array"
import { for } from "std/iter"
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
import { keys, values, entries, fromEntries, get, merge, pick, omit, mapValues, isEmpty } from "std/object"
```

> **Typed maps (`{ String: T }`).** Two groups of `std/object` ops relate to the typed
> index-signature map (the dictionary type, ADR-082, backed by a hashed O(1) container — see
> Specification §5.1.1):
>
> - **Tag-aware introspection** — `keys`, `values`, `entries`, `length`, and `isEmpty` keep a `Json`
>   parameter and work on BOTH a plain `Json`/`{}` record AND a typed map (the runtime dispatches on
>   the value's tag). This is deliberate: they are the way to introspect *any* object, and a genuine
>   `Json` value must be able to flow in. Over a typed map their results are in **hash order**, not
>   insertion order; over a plain `{}` record they preserve insertion order.
> - **Typed map producers** — `merge`, `pick`, `omit`, `mapValues` are generic over `{ String: T }`
>   and *return* a typed map, so the element type flows through statically. They accept a typed map
>   (not a plain `Json` record — there is no implicit `Json -> { String: T }` coercion, §5.1.1; pass
>   a value annotated `{ String: T }`).
>
> A typed map also supports the built-in `m[k]` (yields `T | Null`) and `m[k] = v` directly. For a
> *defaulted* read — the `m[k] ?? default` idiom — use [`get`](#get), which returns a bare `T`
> (the present value or the default), so the result needs no `null` guard.
> `fromEntries` keeps a `Json` signature pending a compiler fix (a type parameter nested in a
> `[String, T][]` argument is not yet inferable).

---

### entries

```txt
val entries: (obj: Json) -> [String, Json][]
```

Returns an array of `[key, value]` pairs. Tag-aware: works on a plain `{}`/`Json` record (insertion order) or a typed `{ String: T }` map (hash order).

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

### get {#get}

```txt
val get: <T, D>(m: { String: T }, key: String, default: D = null) -> T | D
```

Defaulted read over a typed `{ String: T }` map: returns the value at `key`, or `default` when the key is absent. The default's type `D` is an **independent** type parameter (mirroring [`at`](#at-array)), so the result is `T | D` and the default's type never pollutes the value type `T`:

- omitting the default gives `default = null`, so `get(m, k)` is `T | Null` — the same as a bare `m[k]`;
- a same-typed default collapses the union: over a `{ String: Int32 }` map, `get(m, k, 0)` is `Int32 | Int32 = Int32`, a bare scalar usable in arithmetic;
- a differently-typed default keeps both arms: `get(m, k, "n/a")` is `Int32 | String`.

`m` must be a typed map (not a plain `Json` record — there is no implicit `Json -> { String: T }` coercion, §5.1.1).

```txt
val counts: { String: Int32 } = { "a": 7 }
counts.get("a", 0)             // 7
counts.get("missing", 0)       // 0
val present: Int32 = counts.get("a", 0)
present + 1                    // 8   (bare Int32, usable in arithmetic)
counts.get("z", "n/a")         // "n/a"   (independent default type -> Int32 | String)
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

Returns an array of the object's keys. Tag-aware: works on a plain `{}`/`Json` record (insertion order) or a typed `{ String: T }` map (hash order).

```txt
keys({ "a": 1, "b": 2 })   // ["a", "b"]
```

---

### mapValues

```txt
val mapValues: <V, W>(obj: { String: V }, f: (V) -> W) -> { String: W }
```

Returns a new typed map with the same keys as `obj` but with each value transformed by `f` from `V` to `W`. `obj` must be a typed map `{ String: V }` (annotate it, or build it with `m[k] = v`).

```txt
val m: { String: Int32 } = { "a": 1, "b": 2 }
m.mapValues(v => v * 10)         // { "a": 10, "b": 20 } : { String: Int32 }
m.mapValues(v => "v${v}")        // { "a": "v1", "b": "v2" } : { String: String }
```

---

### merge

```txt
val merge: <T>(a: { String: T }, b: { String: T }) -> { String: T }
```

Returns a new typed map containing all keys from `a` and `b`. If both maps have the same key, the value from `b` is used. Both arguments must be typed maps `{ String: T }`. (Keys are in hash order, as for any typed map.)

```txt
val a: { String: Int32 } = { "a": 1, "b": 2 }
val b: { String: Int32 } = { "b": 99, "c": 3 }
a.merge(b)   // { "a": 1, "b": 99, "c": 3 }
```

---

### omit

```txt
val omit: <T>(obj: { String: T }, keys: String[]) -> { String: T }
```

Returns a new typed map with all keys from `obj` except those listed in `keys`. Keys not present in `obj` are silently ignored. `obj` must be a typed map `{ String: T }`.

```txt
val m: { String: Int32 } = { "a": 1, "b": 2, "c": 3 }
m.omit(["b"])             // { "a": 1, "c": 3 }
m.omit(["a", "b", "x"])    // { "c": 3 }
```

---

### pick

```txt
val pick: <T>(obj: { String: T }, keys: String[]) -> { String: T }
```

Returns a new typed map containing only the keys listed in `keys`. Keys not present in `obj` are omitted from the result. `obj` must be a typed map `{ String: T }`.

```txt
val m: { String: Int32 } = { "a": 1, "b": 2, "c": 3 }
m.pick(["a", "c"])   // { "a": 1, "c": 3 }
m.pick(["a", "x"])   // { "a": 1 }
```

---

### values

```txt
val values: (obj: Json) -> Json[]
```

Returns an array of the object's values. Tag-aware: works on a plain `{}`/`Json` record (insertion order) or a typed `{ String: T }` map (hash order; the values are the map's `T` elements).

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

### toJsonString

```txt
val toJsonString: (s: String) -> String
```

Escapes a string and wraps it in double quotes, producing a valid JSON string literal. The
returned value includes the surrounding quotes and escapes `"`, `\`, newline (`\n`), carriage
return (`\r`), tab (`\t`), and other control characters (as `\uXXXX`).

```txt
toJsonString("hello")        // "\"hello\""  (the 7 chars: " h e l l o ")
toJsonString("a\"b")         // "\"a\\\"b\""
toJsonString("x\ny")         // "\"x\\ny\""
```

This is the primitive the test runner uses to build machine-readable records for
`lin test --reporter json` (see below).

### toJson

```txt
val toJson: (value: Json) -> String
```

Recursively serializes ANY Lin value to a strict, valid JSON string:

- strings are escaped and quoted (same escaping as `toJsonString`);
- object **keys** are escaped and quoted too;
- ints/floats become numeric literals; non-finite floats (`NaN`, `±Infinity`) become `null`,
  matching JavaScript's `JSON.stringify`;
- `true`/`false`/`null` become their JSON literals;
- arrays and objects recurse arbitrarily deep.

Unlike `toString` on an object/array (which is a lossy human display that does not escape string
contents or keys), `toJson` always produces output that round-trips through any conforming JSON
parser.

```txt
toJson(42)                               // "42"
toJson("a\"b")                           // "\"a\\\"b\""
toJson([1, 2, 3])                        // "[1,2,3]"
toJson({ "name": "Bob", "tags": ["a"] }) // "{\"name\":\"Bob\",\"tags\":[\"a\"]}"
```

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

## std/yaml

Import:

```txt
import { parse, parseAll, stringify, stringifyAll } from "std/yaml"
```

YAML maps to the same data model as JSON: a parsed document is an ordinary Json value (object, array, string, number, boolean, or `null`). Fallible functions return the canonical `Error` value (`{ "type": "error", "message": String }`) on a parse failure, detectable with `is Error`.

---

### parse <a name="parse-yaml"></a>

```txt
val parse: (src: String) -> Json | Error
```

Parses a single YAML document into a Json value. Returns an `Error` value if `src` is not well-formed YAML.

```txt
val cfg = parse("name: web\nreplicas: 3\n")
print(cfg["name"])      // web
print(cfg["replicas"])  // 3
```

Combine with `std/fs` and `std/jq` to query a config file (a "yq"-style pipeline):

```txt
readFile("deploy.yaml").parse().jq(".spec.containers[].image")
```

---

### parseAll

```txt
val parseAll: (src: String) -> Json[] | Error
```

Parses a multi-document YAML stream — documents separated by a `---` line — into an array of Json values. Returns an `Error` value if any document is malformed.

```txt
val docs = parseAll("a: 1\n---\nb: 2\n")
print(docs.length())  // 2
```

---

### stringify <a name="stringify-yaml"></a>

```txt
val stringify: (value: Json) -> String
```

Serialises a Json value to a block-style YAML document.

```txt
print(stringify({ "name": "web", "ports": [80, 443] }))
// name: web
// ports:
// - 80
// - 443
```

---

### stringifyAll

```txt
val stringifyAll: (values: Json[]) -> String
```

Serialises an array of Json values to a multi-document YAML stream, each document preceded by a `---` separator. The result round-trips through `parseAll`.

```txt
print(stringifyAll([{ "a": 1 }, { "b": 2 }]))
// ---
// a: 1
// ---
// b: 2
```

---

## std/jq

Import:

```txt
import { jq, jqFirst } from "std/jq"
```

Runs [jq](https://jqlang.github.io/jq/) filter programs against a Json value, using a pure-Rust jq implementation. A filter can produce zero, one, or many output values; the full result set is returned as a Json array. Both compile errors (invalid filter syntax) and runtime errors return the canonical `Error` value, detectable with `is Error`.

---

### jq

```txt
val jq: (input: Json, filter: String) -> Json[] | Error
```

Runs `filter` against `input` and returns every output value as a Json array. Returns an `Error` value if the filter fails to compile or errors at runtime.

```txt
val data = { "users": [{ "name": "Ada", "age": 36 }, { "name": "Bob", "age": 30 }] }

jq(data, ".users[] | .name")        // ["Ada", "Bob"]
jq(data, ".users | map(.age) | add") // [66]
jq(data, ".users[] | select(.age > 32) | .name") // ["Ada"]
```

Because of dot-application, this reads naturally as a pipeline:

```txt
readFile("deploy.yaml").parse().jq(".spec.containers[].image")
```

---

### jqFirst

```txt
val jqFirst: (input: Json, filter: String) -> Json | Error
```

Like [`jq`](#jq), but returns just the first output value instead of an array. Returns `Null` when the filter produces no output, and propagates an `Error` value unchanged.

```txt
val data = { "users": [{ "name": "Ada" }, { "name": "Bob" }] }

jqFirst(data, ".users[] | .name")           // "Ada"
jqFirst(data, ".users[] | select(false)")   // null
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

## std/ffi

Raw-memory and C-string helpers for **richer FFI** — calling C libraries that traffic in pointers and out-param structs, beyond the scalar-only `import foreign` surface (spec §26.3). This is a **prototype keystone**: it lets pure Lin marshal `String` arguments into NUL-terminated C strings, allocate scratch/out-param buffers, and read/write fixed-layout structs returned through a C `void*`.

These are inherently unsafe primitives — they do raw memory access with no bounds checking. They are the trusted boundary between Lin's managed values and arbitrary C memory; the caller is responsible for offsets being in-bounds and for the lifetime of any pointer handed to a C API.

`Ptr` is a pointer type aliased to `Int64` (ABI-identical to a 64-bit `void*`). It stays a scalar, so a `Ptr` value is never refcounted and can be passed straight back into another foreign function. (Follow-up: a distinct opaque newtype would let the checker forbid arithmetic on raw handles.)

```txt
cstr:     (s: String)            => Ptr        // NUL-terminated C-string copy of s (caller frees)
withCstr: <T>(s: String, body: (Ptr) => T) => T  // scoped cstr: alloc, run body, free, return T
alloc:    (n: Int64)             => Ptr        // n bytes of raw scratch memory
free:     (p: Ptr)               => Null       // free a buffer from alloc/cstr
peekU8:   (p: Ptr, off: Int64)   => UInt8      // read a u8 at byte offset off
peekU16:  (p: Ptr, off: Int64)   => UInt16     // read a u16 at byte offset off
peekU32:  (p: Ptr, off: Int64)   => UInt32     // read a u32 at byte offset off
peekU64:  (p: Ptr, off: Int64)   => UInt64     // read a u64 at byte offset off
peekI32:  (p: Ptr, off: Int64)   => Int32      // read an i32 at byte offset off
peekI64:  (p: Ptr, off: Int64)   => Int64      // read an i64 at byte offset off
peekF32:  (p: Ptr, off: Int64)   => Float32    // read an f32 at byte offset off
peekF64:  (p: Ptr, off: Int64)   => Float64    // read an f64 at byte offset off
peekPtr:  (p: Ptr, off: Int64)   => Ptr        // read a pointer-width field at byte offset off
pokeU8:   (p: Ptr, off: Int64, v: UInt8)   => Null   // write a u8 at byte offset off
pokeU16:  (p: Ptr, off: Int64, v: UInt16)  => Null   // write a u16 at byte offset off
pokeU32:  (p: Ptr, off: Int64, v: UInt32)  => Null   // write a u32 at byte offset off
pokeU64:  (p: Ptr, off: Int64, v: UInt64)  => Null   // write a u64 at byte offset off
pokeI32:  (p: Ptr, off: Int64, v: Int32)   => Null   // write an i32 at byte offset off
pokeI64:  (p: Ptr, off: Int64, v: Int64)   => Null   // write an i64 at byte offset off
pokeF32:  (p: Ptr, off: Int64, v: Float32) => Null   // write an f32 at byte offset off (SDL_FRect etc.)
pokeF64:  (p: Ptr, off: Int64, v: Float64) => Null   // write an f64 at byte offset off
pokePtr:  (p: Ptr, off: Int64, v: Ptr)     => Null   // write a pointer-width field at byte offset off
```

**Prefer `withCstr` to avoid leaks.** It allocates a NUL-terminated copy, runs your callback with the pointer, frees the buffer, and returns the callback's value — the leak-free idiom for the common "the C API copies the string during the call" case:

```lin
val win = withCstr("title", (p) => create_window_titled(p, 320, 240))
```

The bare `cstr` does **not** free its allocation — use it (paired with `free`) only when the C API *retains* the pointer and you must manage its lifetime explicitly. Calling `cstr` without a matching `free` leaks; in a hot loop that leaks unboundedly. (`withCstr` itself can only leak if the callback faults, since Lin has no try/finally — accepted for this prototype.)

Real foreign calls (`import foreign "lib.so"`) require `lin build`; `alloc`/`poke`/`peek`/`free`/`withCstr` are plain runtime symbols and also run under `lin test`. The compiler emits a **`$ORIGIN`-relative rpath** to a vendored `.so`'s directory (Linux/ELF): the produced binary and its co-located `.so` are **relocatable** — copy both together (preserving their relative layout) anywhere and the binary still finds the library at runtime without `LD_LIBRARY_PATH`, because `$ORIGIN` is resolved by the loader to wherever the binary actually lives. Vendor the `.so` next to (or at a fixed relative path from) the binary. If a relative path can't be computed the compiler falls back to an absolute rpath (robust, not relocatable). macOS `@loader_path`/`install_name` is a follow-up. See `examples/sdl/` for an end-to-end demo: two programs (`bounce.lin`, `ai_worker.lin`) drive the real SDL3 C ABI from pure Lin via `Ptr` handles, a `withCstr` title, and an `SDL_FRect` built with `pokeF32`, linking a committed headless SDL3 stub (`libs/libSDL3.so`) so they run without a display; `ai_worker.lin` also offloads a pure planning step to an `async` worker (values cross the boundary, SDL handles stay on the main thread).

```lin
import { alloc, free, peekU32, pokeU32 } from "std/ffi"

val buf = alloc(8)         // an 8-byte struct: u32 @0, u32 @4
pokeU32(buf, 0, 1)         // type
pokeU32(buf, 4, 41)        // scancode
val ty = peekU32(buf, 0)   // 1
val sc = peekU32(buf, 4)   // 41
free(buf)
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

## std/stream

Lazy, fallible streams over OS resources — files, sockets, subprocess stdout, and stdin (spec §27.9). A `Stream<T>` is an opaque runtime value built as a **lazy pull graph**: a source node (`readStream`), zero or more adapters (`lines`/`linesMax`/`chunks`, plus the `std/iter` combinators `map`/`filter`/`take`/… which dispatch lazily on a stream receiver), and a terminal operation that drives the graph one item at a time with bounded memory. Errors are threaded **in-band** — the first read error poisons the upstream and short-circuits to the terminal op, so error handling lives only at the terminal, not at every adapter.

**A stream can only be read once.** Reading consumes it, so each stream flows through a single pipeline — once you've called a combinator or terminal on it, using that stream again is a compile-time error. To make a second pass over the same data, open a fresh stream. You don't have to consume a stream (opening one and never reading it is fine — it cleans up after itself), and a stream lives in a local `val`, a function parameter, or a return value — not in an object field, array, or `var`. (The single-use rule is what lets `.promise()` safely hand the whole pipeline to a worker thread; design rationale in ADR-075.)

The combinators (`map`/`filter`/`take`/`drop`/`reduce`/`for`/…) are **not** part of `std/stream` — they
come from [`std/iter`](#stditer) and dispatch to the lazy stream backend automatically when the receiver
is a `Stream` (ADR-077). A stream pipeline imports its **sources and sinks** from `std/stream` and its
**combinators** from `std/iter`:

```txt
import { readStream, writeLines, drain } from "std/stream"
import { map, filter, take, for } from "std/iter"

readStream("in.csv")
  .lines()
  .map(transform)
  .filter(notEmpty)
  .writeLines("out.csv")
  .drain()
```

Byte sources also come from other modules — `tcpStream` (`std/net`), `stdoutStream` (`std/process`), and `stdinStream` (`std/io`) all return `Stream<UInt8[]>` (spec §27.9.2) and feed the same adapters and terminals documented here.

> The optional 0-based callback **index parameter** (`(item, i) => …`) on the `std/iter` combinators is **array/iterator-only**: over a `Stream`, the lazy combinators (`map`/`filter`/`for`/…) keep their 1-arg callbacks (a stream is pull-driven with no materialised source position).

### Types

```txt
Stream<T>   // opaque; covariant in T; not JSON, not subscriptable
```

---

### readStream

```txt
val readStream: (path: String) -> Stream<UInt8[]>
```

Opens the file at `path` as a lazy byte stream. No bytes are read until a terminal operation drives the stream. A failure to open, or a read failure during traversal, surfaces in-band as an `Error` at the terminal op (the source-kind ending rule of spec §27.9.4).

```txt
val text = readStream("notes.txt").readText()
match text
  is Error => print("read failed: ${text["message"]}")
  else     => print(text)
```

---

### lines (stream) {#lines-stream}

```txt
val lines: (Stream<UInt8[]>) -> Stream<String>
```

Lazily views a byte stream as a stream of lines (splitting on newlines, decoding UTF-8 per line). An adapter — it reads nothing until driven.

```txt
readStream("access.log").lines().for(line => print(line))   // `for` from std/iter
```

A single line is capped (default 64 MiB) so a newline-less input fails in-band with an `Error` rather
than buffering the whole stream; use [`linesMax`](#linesMax) to set an explicit cap.

---

### linesMax

```txt
val linesMax: (Stream<UInt8[]>, maxBytes: Int32) -> Stream<String>
```

Like [`lines`](#lines-stream), but with an explicit per-line byte cap. A line longer than `maxBytes`
fails in-band with an `Error`. A `maxBytes` of `0` or less keeps the default cap.

```txt
readStream("untrusted.txt").linesMax(1024).for(line => print(line))
```

---

### chunks

```txt
val chunks: (Stream<UInt8[]>, n: Int32) -> Stream<UInt8[]>
```

Re-chunks a byte stream into fixed-size `n`-byte windows (the final window may be shorter). Useful for fixed-record binary formats.

```txt
readStream("frames.bin").chunks(188).for(frame => process(frame))
```

---

### readText

```txt
val readText: (Stream<UInt8[]>) -> String | Error
```

Terminal. Drives a byte stream to completion and returns its full contents as one `String`, or an `Error` if a read fails. Synchronous (calling thread).

---

### collect

```txt
val collect: (Stream<UInt8[]>) -> UInt8[] | Error
```

Terminal. Drives a byte stream to completion and returns its full contents as one `UInt8[]` buffer, or an `Error` if a read fails. Synchronous (calling thread).

---

### writeStream

```txt
val writeStream: <T>(Stream<T>, path: String) -> Stream<T>
```

Builds a **raw sink** node that writes each upstream item's bytes to the file at `path` **verbatim**, concatenated with **no separator**: a `String` item writes its UTF-8 bytes, a `UInt8[]` item writes its raw bytes, and anything else writes its `toString` rendering. It returns a `Stream` whose terminal op (`drain`/`promise`) runs the whole pipeline — pulling one item at a time and writing it — so memory stays bounded regardless of input size. Building the sink does not write anything; a terminal op must drive it.

Because it injects no newlines, `writeStream` is the correct sink for **binary** output — e.g. compressing a file to disk, where any inserted separator would corrupt the result:

```txt
readStream("data.txt")
  .gzip()                  // from std/compress
  .writeStream("data.gz")  // raw bytes — a valid .gz file
  .drain()
```

For newline-delimited text output (CSV rows, log lines), use [`writeLines`](#writeLines).

---

### writeLines

```txt
val writeLines: <T>(Stream<T>, path: String) -> Stream<T>
```

Builds a **line-oriented sink** node that writes each upstream item to the file at `path` followed by a newline (`\n`) — i.e. one item per line. Item bytes are rendered the same way as [`writeStream`](#writeStream) (String → UTF-8, `UInt8[]` → raw bytes, otherwise `toString`); the difference is the trailing `\n` after each item. Like `writeStream` it is lazy — a terminal op (`drain`/`promise`) must drive it.

```txt
readStream("in.csv")
  .lines()
  .map(transform)
  .filter(notEmpty)
  .writeLines("out.csv")   // each transformed row on its own line
  .drain()
```

---

### drain

```txt
val drain: <T>(Stream<T>) -> Null | Error
```

Terminal. Drives the pipeline on the **calling thread** and returns `Null` on normal completion (EOF) or `Error` if a read or write fails. This is the synchronous driver — no worker thread, no new runtime machinery.

```txt
val outcome = readStream("in.csv")
  .lines()
  .writeLines("out.csv")
  .drain()
match outcome
  is Error => print("copy failed: ${outcome["message"]}")
  else     => print("done")
```

---

### promise (stream) {#promise-stream}

```txt
val promise: <T>(Stream<T>) -> Json    // Promise<Null | Error>
```

Terminal. Runs the whole pipeline on a **background thread** and returns a promise immediately, so your program can do other work while the stream is processed. `await` the promise for the result — `Null` on success, or an `Error` if anything went wrong while processing (a crash mid-stream is caught at the thread boundary and handed back as an `Error` rather than aborting the program; spec §24.2.2). Use `.drain()` when you simply want to run the pipeline and wait. (Design rationale — how the pipeline is safely handed to the worker — is in ADR-075.)

The promise type is conceptually `Promise<Null | Error>`; like all promise handles it is erased to `Json` in annotations (spec §24.1). `await` reattaches the `Null | Error` union, so the `Error` case must be handled (ADR-070).

```txt
val p = readStream("big.log")
  .lines()
  .filter(isError)
  .writeLines("errors.log")
  .promise()
match await(p)
  is Error => print("pipeline failed")
  else     => print("done")
```

---

### close (stream) {#close-stream}

```txt
val close: (Stream<T>) -> Null
```

Closes the stream now, releasing the file (or socket) it holds. **Optional** — a stream cleans up on its own once you're done with it; `close` is only for when you want to release the resource at a specific point rather than waiting for that. **Idempotent** — closing an already-closed or already-drained stream does nothing.

---

## std/compress

Streaming gzip/DEFLATE adapters over a byte stream (`Stream<UInt8[]>`). Each is a **lazy adapter**:
it wraps an upstream byte stream and (de)compresses bytes **incrementally** — one upstream chunk in,
whatever output bytes the codec produced out — so a multi-gigabyte file flows through in constant
memory. A decode/encode fault is threaded **in-band**: the first such error short-circuits straight
to the terminal op as the canonical `Error` value, exactly like every other stream adapter (no
`is Error` between steps).

Two container formats are supported: `gzip`/`gunzip` use the **gzip** container (header + CRC32 +
length trailer); `deflate`/`inflate` use the **raw DEFLATE** bitstream (no header/CRC).

```txt
import { readStream, writeStream, drain } from "std/stream"
import { gunzip } from "std/compress"

// Decompress a .gz file to disk as a streaming pipeline:
readStream("data.txt.gz")
  .gunzip()
  .writeStream("data.txt")
  .drain()
```

### gunzip

```txt
val gunzip: (Stream<UInt8[]>) -> Stream<UInt8[]>
```

Decompresses a **gzip-framed** byte stream, yielding the decompressed bytes as a new byte stream.
Invalid gzip input surfaces an `Error` in-band at the terminal op.

### gzip

```txt
val gzip: (Stream<UInt8[]>) -> Stream<UInt8[]>
```

Compresses a byte stream into the **gzip container** format (the output a `.gz` file would hold).

### inflate

```txt
val inflate: (Stream<UInt8[]>) -> Stream<UInt8[]>
```

Decompresses a **raw DEFLATE** byte stream (no gzip header/CRC). Invalid input surfaces an `Error`
in-band.

### deflate

```txt
val deflate: (Stream<UInt8[]>) -> Stream<UInt8[]>
```

Compresses a byte stream as a **raw DEFLATE** bitstream (no gzip header/CRC).

---

## std/archive

Tar splitting over a byte stream (`Stream<UInt8[]>`). A **tar** archive is a flat sequence of
512-byte-aligned (header + body) records; these surfaces turn a byte stream into archive entries
**without buffering the whole archive** — the parent stream is pulled one chunk at a time. A
`.tar.gz` is just `gunzip()` (`std/compress`) composed with the tar splitter. **Only the tar format
is supported** (zip is not).

All three surfaces **consume** the parent stream (it is moved in); the source binding may not be used
again after the call — the affine stream check rejects a reuse (re-open the source for a second pass).

Each entry's **`meta`** object is pure JSON:

```txt
{ name: String, size: Int64, typeflag: String, isDir: Boolean }
```

where `typeflag` is the tar type byte as a one-character string (`"0"` = regular file, `"5"` =
directory) and `isDir` is `true` iff `typeflag == "5"`.

```txt
import { readStream, drain, writeStream } from "std/stream"
import { gunzip } from "std/compress"
import { untar, manifest, files } from "std/archive"
import { for } from "std/iter"

// List a .tar.gz's contents without extracting anything:
readStream("data.tar.gz").gunzip().manifest().for((m) => print(m["name"]))

// Extract every member to disk in constant memory (each body streamed straight to its file):
readStream("data.tar.gz").gunzip().untar((meta, data) =>
  data.writeStream("out/${meta["name"]}").drain()
)
```

### untar

```txt
val untar: (Stream<UInt8[]>, body: (Json, Stream<UInt8[]>) -> Json) -> Null | Error
```

The **terminal** driver: drives the whole archive on the calling thread, calling `body(meta, data)`
once per entry, where `data` is a `Stream<UInt8[]>` **sub-stream** over that entry's body. The body's
return value is ignored. Returns `Null` on a clean archive, or an `Error` if a read fault or a body
fault occurs. This is the **constant-memory primitive** — an arbitrarily large member flows through
its `data` sub-stream without ever being fully buffered.

Whether the body **drains** `data` or **ignores** it, the driver always skips to the next entry
correctly (an undrained body is skipped automatically).

> **Sync-only sub-stream.** Inside the body, `data` is valid **only during the body's synchronous
> execution** — the driver is paused while the body runs and resumes (advancing to the next entry)
> the moment the body returns. `data` must therefore be consumed (drained / read) **inside the
> callback**; it shares a cursor with the paused driver. Handing it to a worker via `.promise()`
> would race that cursor and is **unsupported**. The ADR-075 stream placement restriction bounds
> `data`'s lifetime to the callback (it cannot be stored in a field, `var`, or array); a dedicated
> compile-time check specifically for `.promise()` on a sub-stream is a known gap.

### manifest

```txt
val manifest: (Stream<UInt8[]>) -> Stream<Object>
```

An **adapter** yielding each entry's `meta` object, with its body **skipped** (a meta-only listing).
Composes with `std/iter` (`filter`/`map`/`for`) like any other `Stream`. No sub-streams are minted.

### files

```txt
val files: (Stream<UInt8[]>) -> Stream<Object>
```

An **adapter** yielding `{ name, data, size, typeflag, isDir }` per entry, where `data` is the
entry's **full body buffered** into a `UInt8[]`. A convenience for normal-sized files; composes with
`std/iter`. Because each body is buffered in memory, prefer `untar` for arbitrarily large entries.

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

Templating is **Jinja-style**, backed by the [minijinja](https://crates.io/crates/minijinja) engine. Templates support:

- **Substitutions** — `{{ name }}`, `{{ user.email }}` (dot paths into the data record).
- **Loops** — `{% for item in items %}...{% endfor %}`.
- **Conditionals** — `{% if cond %}...{% else %}...{% endif %}`.
- **Filters** — the standard minijinja builtin filter set, e.g. `{{ name | upper }}`.
- **Layouts / inheritance** — `{% extends "base.jinja" %}` + `{% block %}`, and partials via `{% include "footer.jinja" %}`. **Only available through [`render`](#render)** (file-based): the referenced templates are loaded by name from the same directory as the entry file. [`renderWith`](#renderWith) takes an in-memory string with no directory to load from, so it cannot resolve `extends`/`include`.

**Undefined / missing variables render as the empty string** (not an error). A **template syntax error or render failure** is returned as an `Error` (`{ "type": "error", "message": ... }`), discriminated with `is Error` like other fallible stdlib operations.

#### Layouts

A base layout declares the page skeleton with fillable blocks:

```txt
<!-- templates/base.jinja -->
<!DOCTYPE html>
<html>
<head><title>{{ title }}</title></head>
<body{% block body_attrs %}{% endblock %}>
  {% block main %}{% endblock %}
</body>
</html>
```

A page extends it and fills the blocks; an unfilled block keeps the base's default content:

```txt
<!-- templates/page.jinja -->
{% extends "base.jinja" %}
{% block main %}<article>{{ content }}</article>{% endblock %}
```

`render("templates/page.jinja", data)` resolves `base.jinja` from `templates/` and produces the merged document. `{% include "footer.jinja" %}` pulls another file in from the same directory. See `docs-site/templates/` and `examples/web-server/views/` for worked examples.

---

### render

```txt
val render: (path: String, data: {}) -> String | Error
```

Reads the file at `path` and renders it as a template against `data`, **with layout support**: the template may `{% extends %}` a base and fill `{% block %}`s, or `{% include %}` partials — referenced files are loaded by name from `path`'s directory. Intended for `.jinja` template files. Returns an `Error` (`{ "type": "error", "message": ... }`) if the file cannot be read or the template fails to render.

```txt
val s = render("greet.jinja", { "name": "Alice", "score": 42 })
match s
  is Error => print("error: ${s["message"]}")
  else     => print(s)
```

---

### renderWith

```txt
val renderWith: (template: String, data: {}) -> String | Error
```

Renders a template string directly against `data`. Missing keys render as the empty string; a syntax/render error is returned as an `Error`. Because the template is an in-memory string with no source directory, `{% extends %}` / `{% include %}` cannot be resolved — use [`render`](#render) for layouts.

```txt
renderWith("Hello, {{ name }}!", { "name": "Alice" })
// "Hello, Alice!"

renderWith("{% for n in nums %}{{ n }} {% endfor %}", { "nums": [1, 2, 3] })
// "1 2 3 "
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

### Machine-readable output (`lin test --reporter json`)

By default `lin test` prints a human-readable summary to stderr. Pass `--reporter json` to
emit **newline-delimited JSON (NDJSON)** on **stdout** instead — one record per line — for
consumption by tooling (e.g. the VSCode Test Explorer integration). The process exit code is
unchanged: non-zero if any test file failed.

The stream is **versioned**: the first line is always a `meta` record carrying the schema
version. Consumers should read it and refuse (or warn) on an unrecognized version rather than
mis-parsing newer shapes.

Each line is one of four record shapes:

```jsonc
// Always the FIRST line — the NDJSON schema version
{ "event": "meta", "schema": 2 }

// One per test (from the suite's results)
{ "event": "test", "file": "<path>", "name": "<test name>", "status": "pass", "durationMs": <int> }
{ "event": "test", "file": "<path>", "name": "<test name>", "status": "fail", "message": "<joined failure messages>", "expected"?: <any JSON>, "actual"?: <any JSON>, "durationMs": <int> }

// OPTIONAL: the user's own `print(...)` output for a file, emitted before that file's
// `file` record. Absent when the file produced no non-runner stdout. (Added in schema 2.)
{ "event": "output", "file": "<path>", "text": "<joined stdout lines>" }

// One per test file (always emitted, after its test/output records)
{ "event": "file", "file": "<path>", "status": "pass" | "fail" | "timeout" | "compile_error", "durationMs": <int>, "message"?: "<diagnostic>" }
```

- `status` on a `test` record is `pass` or `fail`; the `message` (failures only) is the
  test's failure messages joined with `\n` (so a `toBe` mismatch carries its
  `expected: …\nactual: …` text). All strings are properly JSON-escaped.
- `expected` / `actual` are OPTIONAL structured fields on a `fail` `test` record, carrying the
  raw compared values as arbitrary JSON (any shape — strings, numbers, objects, arrays). They are
  emitted by equality-style matchers (`toBe`, `toBeNull`, `toFailWith`) and let consumers render a
  structured diff without parsing the human `message`. They are present only when EXACTLY ONE of
  the test's failing assertions carries a pair; with multiple failing assertions, or matchers
  without a meaningful pair (`toSatisfy`, `toSucceed`, `toFail`), they are omitted and the
  `message` conveys everything. Values are serialized with `toJson` (the recursive serializer).
- `durationMs` on a `test` record is the monotonic wall-clock time (in milliseconds) spent
  evaluating that test's body, measured by `std/test`.
- The `file` record reports the whole file's outcome and is the only signal for files that
  never produced per-test records — e.g. a `compile_error` (with the diagnostic in `message`)
  or a `timeout`.
- The `output` record (schema 2+) carries any stdout the test binary wrote that wasn't a runner
  record — i.e. the user's own `print(...)` calls. All such lines for a file are joined with `\n`
  into one `text` blob and emitted just before that file's `file` record. It is omitted entirely
  when there was no such output. Consumers predating schema 2 can safely ignore unknown events.

Internally the runner (`std/test`) emits each record as a `##LINTEST## `-prefixed line gated on
the `LIN_TEST_JSON` environment variable; the CLI strips the prefix, attaches the file path,
and re-serializes each record canonically. The marker lets the CLI separate runner records from
any `print` output your own test code emits on the same stdout stream.

**Running a subset by name.** Pass `--filter-test "<name>"` (repeatable) to run only the named
test(s) within the matched files; every other test is *skipped* — its body is never evaluated
(so no side effects, and no `withFixture` setup/teardown) and it emits no record. A skipped test
counts as neither pass nor fail, so a filtered run that omits a failing test still exits zero.
This backs the VSCode Test Explorer's single-test gutter run.

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
