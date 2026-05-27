# Lin Standard Library Specification

This document specifies the standard library for the Lin language. All modules are importable via the `std/` prefix.

## Index

### Modules

| Module | Description |
| --- | --- |
| [`std/string`](#stdstring) | String manipulation functions |
| [`std/array`](#stdarray) | Array and iterator functions |
| [`std/number`](#stdnumber) | Numeric parsing and conversion functions |
| [`std/object`](#stdobject) | Object introspection functions |
| [`std/io`](#stdio) | stdin/stdout and terminal input |
| [`std/fs`](#stdfs) | Filesystem read and write |
| [`std/http`](#stdhttp) | HTTP client and server |
| [`std/async`](#stdasync) | Async, concurrency and workers |
| [`std/template`](#stdtemplate) | String template rendering |
| [`std/test`](#stdtest) | Test framework |

### Functions by module

**std/string**

| Function | Signature | Summary |
| --- | --- | --- |
| [`trim`](#trim) | `(String) -> String` | Remove leading and trailing whitespace |
| [`toUpper`](#toUpper) | `(String) -> String` | Convert to uppercase |
| [`toLower`](#toLower) | `(String) -> String` | Convert to lowercase |
| [`substring`](#substring) | `(String, Int32, Int32) -> String` | Extract a slice by codepoint indices |
| [`at`](#at) | `(String, Int32) -> String` | Character at index; negative indices count from end |
| [`indexOf`](#indexOf-string) | `(String, String) -> Int32` | First occurrence of needle, or -1 |
| [`length`](#length-string) | `(String) -> Int32` | Codepoint count |
| [`contains`](#contains) | `(String, String) -> Boolean` | Test whether needle is a substring |
| [`startsWith`](#startsWith) | `(String, String) -> Boolean` | Test whether string begins with prefix |
| [`endsWith`](#endsWith) | `(String, String) -> Boolean` | Test whether string ends with suffix |
| [`split`](#split) | `(String, String) -> String[]` | Split by delimiter |
| [`join`](#join) | `(String[], String) -> String` | Join array of strings with separator |
| [`replace`](#replace) | `(String, String, String) -> String` | Replace first occurrence |
| [`repeat`](#repeat) | `(String, Int32) -> String` | Repeat a string n times |
| [`toString`](#toString) | `(Json) -> String` | Convert any value to its string representation |

**std/array**

| Function | Signature | Summary |
| --- | --- | --- |
| [`for`](#for) | `(Iterable, (Json) -> Json) -> Null` | Iterate over array or iterator |
| [`push`](#push) | `(Json[], Json) -> Null` | Append an element to an array in place |
| [`length`](#length-array) | `(Json) -> Int32` | Length of array, string, or object |
| [`range`](#range) | `(Int32, Int32) -> Iterator` | Integer range [start, end) |
| [`iterOf`](#iterOf) | `(Json[]) -> Iterator` | Iterator over an array |
| [`iter`](#iter) | `(() -> S, (S) -> Boolean, (S) -> S, (S) -> T) -> Iterator` | Build a custom iterator |
| [`concat`](#concat) | `(Json[], Json[]) -> Json[]` | Concatenate two arrays |
| [`map`](#map) | `(Json[], (Json) -> Json) -> Json[]` | Transform each element |
| [`filter`](#filter) | `(Json[], (Json) -> Boolean) -> Json[]` | Keep elements matching predicate |
| [`reduce`](#reduce) | `(Json[], Json, (Json, Json) -> Json) -> Json` | Fold left with an accumulator |
| [`find`](#find) | `(Json[], (Json) -> Boolean) -> Json` | First matching element, or null |
| [`some`](#some) | `(Json[], (Json) -> Boolean) -> Boolean` | True if any element matches |
| [`every`](#every) | `(Json[], (Json) -> Boolean) -> Boolean` | True if all elements match |
| [`flatMap`](#flatMap) | `(Json[], (Json) -> Json[]) -> Json[]` | Map then flatten one level |
| [`indexOf`](#indexOf-array) | `(Json[], Json) -> Int32` | First index of value, or -1 |
| [`reverse`](#reverse) | `(Json[]) -> Json[]` | Return a reversed copy |

**std/number**

| Function | Signature | Summary |
| --- | --- | --- |
| [`parseInt32`](#parseInt32) | `(String) -> Int32` | Parse decimal string to Int32 |
| [`parseFloat64`](#parseFloat64) | `(String) -> Float64` | Parse decimal string to Float64 |
| [`toInt32`](#toInt32) | `(Float64) -> Int32` | Truncate float to Int32 |
| [`toFloat64`](#toFloat64) | `(Int32) -> Float64` | Widen Int32 to Float64 |
| [`isInt32`](#isInt32) | `(String) -> Boolean` | Test whether a string parses as Int32 |

**std/object**

| Function | Signature | Summary |
| --- | --- | --- |
| [`keys`](#keys) | `(Json) -> String[]` | Array of object keys |
| [`values`](#values) | `(Json) -> Json[]` | Array of object values |
| [`entries`](#entries) | `(Json) -> [String, Json][]` | Array of `[key, value]` pairs |

**std/io**

| Function | Signature | Summary |
| --- | --- | --- |
| [`print`](#print) | `(Json) -> Null` | Write a value to stdout |
| [`readLine`](#readLine) | `() -> String \| Null` | Read one line from stdin, or Null on EOF |
| [`lines`](#lines) | `() -> Iterator` | Iterator over stdin lines |
| [`readAll`](#readAll) | `() -> String` | Read all of stdin as one string |

**std/fs**

| Function | Signature | Summary |
| --- | --- | --- |
| [`readFile`](#readFile) | `(String) -> String \| Error` | Read entire file as a string |
| [`writeFile`](#writeFile) | `(String, String) -> Null \| Error` | Write string to file, replacing contents |
| [`appendFile`](#appendFile) | `(String, String) -> Null \| Error` | Append string to end of file |
| [`readLines`](#readLines) | `(String) -> Iterator \| Error` | Iterator over lines of a file |
| [`readJson`](#readJson) | `(String) -> Json \| Error` | Read and parse file as JSON |
| [`writeJson`](#writeJson) | `(String, Json) -> Null \| Error` | Serialise value to JSON and write to file |
| [`exists`](#exists) | `(String) -> Boolean` | Test whether a file or directory exists |

**std/http** — client

| Function | Signature | Summary |
| --- | --- | --- |
| [`fetch`](#fetch) | `(String) -> HttpResponse \| Error` | GET a URL |
| [`fetchWith`](#fetchWith) | `(String, HttpOptions) -> HttpResponse \| Error` | Request with custom method, headers, body |
| [`fetchJson`](#fetchJson) | `(String) -> Json \| Error` | GET a URL and parse the body as JSON |
| [`postJson`](#postJson) | `(String, Json) -> HttpResponse \| Error` | POST a JSON body to a URL |

**std/http** — server

| Function | Signature | Summary |
| --- | --- | --- |
| [`serve`](#serve) | `(Int32, (HttpRequest) -> HttpResponse) -> Null` | Start a sequential HTTP server |
| [`json`](#json-helper) | `(Int32, Json) -> HttpResponse` | Build a JSON response |
| [`text`](#text-helper) | `(Int32, String) -> HttpResponse` | Build a plain-text response |
| [`redirect`](#redirect) | `(String) -> HttpResponse` | Build a 302 redirect response |
| [`notFound`](#notFound) | `HttpResponse` | 404 response value |
| [`badRequest`](#badRequest) | `(String) -> HttpResponse` | Build a 400 response with a message |
| [`pathMatch`](#pathMatch) | `(String, String) -> { ...String } \| Null` | Match a path pattern, returning captured params |
| [`parseBody`](#parseBody) | `(HttpRequest) -> Json \| Error` | Parse the request body as JSON |

**std/async**

| Function | Signature | Summary |
| --- | --- | --- |
| [`async`](#async) | `(() -> T) -> Promise` | Run a thunk asynchronously |
| [`await`](#await) | `(Promise) -> T` | Block until a promise resolves |
| [`parallel`](#parallel) | `((() -> T)[]) -> T[]` | Run an array of thunks concurrently, collect results |
| [`race`](#race) | `(Promise[]) -> T` | Resolve with the first promise to complete |
| [`timeout`](#timeout) | `(Promise, Int32) -> T` | Add a millisecond timeout to a promise |
| [`retry`](#retry) | `(() -> T, Int32) -> T` | Retry a thunk up to n times on failure |
| [`threadPool`](#threadPool) | `(Int32) -> ThreadPool` | Create a thread pool with n workers |
| [`worker`](#worker) | `((Msg) -> Reply, () -> Null) -> Worker` | Create a background worker |
| [`request`](#request) | `(Worker, Msg) -> Reply` | Send a request to a worker and wait for reply |
| [`message`](#message) | `(Worker, Msg) -> Null` | Send a fire-and-forget message to a worker |
| [`close`](#close) | `(Worker) -> Null` | Shut down a worker |

**std/template**

| Function | Signature | Summary |
| --- | --- | --- |
| [`render`](#render) | `(String, {}) -> String \| Error` | Load a `.lint` file and render it with a data record |
| [`renderWith`](#renderWith) | `(String, {}) -> String` | Render a template string with a data record |

**std/test**

| Name | Signature | Summary |
| --- | --- | --- |
| [`suite`](#suite) | `(String, Test[]) -> Suite` | Group tests under a name |
| [`test`](#test) | `(String, () -> Assertion \| Assertion[]) -> Test` | Declare a single test case |
| [`run`](#run) | `(Suite[]) -> Null` | Execute suites, print results, exit non-zero on failure |
| [`expect`](#expect) | `(Json) -> Asserter` | Begin an assertion chain |

---

## std/string

String operations are codepoint-aware. All indices and lengths count Unicode codepoints, not bytes.

Import:

```txt
import { trim, toUpper, indexOf } from "std/string"
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

### substring

```txt
val substring: (s: String, start: Int32, end: Int32) -> String
```

Returns the slice of `s` covering codepoint indices `[start, end)`. If `end` exceeds the codepoint count it is clamped. If `start >= end`, returns `""`.

```txt
substring("hello", 1, 3)   // "el"
substring("hello", 0, 5)   // "hello"
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

## std/array

Array and iterator functions. All transformation functions are non-mutating and return new values.

Import:

```txt
import { map, filter, for, range } from "std/array"
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

### range

```txt
val range: (start: Int32, end: Int32) -> Iterator
```

Returns an iterator that yields the integers `start, start+1, ..., end-1`. If `start >= end`, the iterator is empty.

```txt
range(0, 3).for(i => print(toString(i)))   // prints 0, 1, 2
range(1, 4).map(i => i * 2)               // [2, 4, 6]
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

### map

```txt
val map: (arr: Json[], f: (Json) -> Json) -> Json[]
```

Returns a new array formed by applying `f` to each element of `arr` in order.

```txt
[1, 2, 3].map(x => x * 2)        // [2, 4, 6]
["a", "b"].map(s => toUpper(s))   // ["A", "B"]
```

---

### filter

```txt
val filter: (arr: Json[], f: (Json) -> Boolean) -> Json[]
```

Returns a new array containing only the elements for which `f` returns `true`.

```txt
[1, 2, 3, 4].filter(x => x > 2)   // [3, 4]
```

---

### reduce

```txt
val reduce: (arr: Json[], init: Json, f: (Json, Json) -> Json) -> Json
```

Folds `arr` left-to-right starting from `init`. `f` receives the accumulator as its first argument and the current element as its second.

```txt
[1, 2, 3, 4].reduce(0, (acc, x) => acc + x)   // 10
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

### flatMap

```txt
val flatMap: (arr: Json[], f: (Json) -> Json[]) -> Json[]
```

Applies `f` to each element and concatenates the resulting arrays into a single flat array.

```txt
[1, 2, 3].flatMap(x => [x, x * 2])   // [1, 2, 2, 4, 3, 6]
```

---

### indexOf (array) {#indexOf-array}

```txt
val indexOf: (arr: Json[], target: Json) -> Int32
```

Returns the zero-based index of the first element deeply equal to `target`, or `-1` if not found.

```txt
[10, 20, 30].indexOf(20)   // 1
[1, 2, 3].indexOf(9)       // -1
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

## std/number

Import:

```txt
import { parseInt32, parseFloat64 } from "std/number"
```

---

### parseInt32

```txt
val parseInt32: (s: String) -> Int32
```

Parses `s` as a base-10 integer. If `s` cannot be parsed or the value overflows `Int32`, the result is a runtime error. Use `isInt32` to guard untrusted input.

```txt
parseInt32("42")   // 42
parseInt32("-7")   // -7
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

### toFloat64

```txt
val toFloat64: (v: Int32) -> Float64
```

Widens an `Int32` to `Float64`. Always exact.

```txt
toFloat64(42)   // 42.0
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

## std/object

Import:

```txt
import { keys, values, entries } from "std/object"
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

### values

```txt
val values: (obj: Json) -> Json[]
```

Returns an array of the object's values in insertion order.

```txt
values({ "a": 1, "b": 2 })   // [1, 2]
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

## std/io

Import:

```txt
import { print, readLine, lines } from "std/io"
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

### lines

```txt
val lines: () -> Iterator
```

Returns an iterator that yields one `String` per line from stdin. Terminates at EOF.

```txt
lines().for(line => print(line.trim()))
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

## std/fs

Import:

```txt
import { readFile, writeFile, readLines } from "std/fs"
```

---

### readFile

```txt
val readFile: (path: String) -> String | Error
```

Reads the entire contents of the file at `path` as a UTF-8 string.

```txt
match readFile("config.txt")
  is { "type": "success", "value": contents } => process(contents)
  is { "type": "failure", "error": e }         => print("read failed: ${e}")
```

---

### writeFile

```txt
val writeFile: (path: String, content: String) -> Null | Error
```

Writes `content` to the file at `path`, replacing existing contents.

---

### appendFile

```txt
val appendFile: (path: String, content: String) -> Null | Error
```

Appends `content` to the end of the file at `path`.

---

### readLines

```txt
val readLines: (path: String) -> Iterator | Error
```

Returns an iterator that yields one `String` per line from the file.

```txt
match readLines("data.csv")
  is { "type": "failure", "error": e }         => print("cannot open: ${e}")
  is { "type": "success", "value": it } =>
    it.for(line => process(line))
```

---

### readJson

```txt
val readJson: (path: String) -> Json | Error
```

Reads and parses the file at `path` as JSON.

---

### writeJson

```txt
val writeJson: (path: String, value: Json) -> Null | Error
```

Serialises `value` to compact JSON and writes it to `path`.

---

### exists

```txt
val exists: (path: String) -> Boolean
```

Returns `true` if a file or directory exists at `path`.

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
match fetch("https://api.example.com/ping")
  is { "type": "failure", "error": e }        => print("network error: ${e}")
  is { "type": "success", "value": resp } =>
    print(toString(resp["status"]))
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

### fetchJson

```txt
val fetchJson: (url: String) -> Json | Error
```

GET `url`, parse the body as JSON. Returns an `Error` if transport fails, the status is not 2xx, or the body is not valid JSON.

```txt
match fetchJson("https://api.example.com/users")
  is { "type": "success", "value": users } =>
    users.map(u => u["name"]).for(name => print(name))
  is { "type": "failure", "error": e }     =>
    print("failed: ${e}")
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
val serve: (port: Int32, handler: (HttpRequest) -> HttpResponse) -> Null
```

Starts an HTTP server on `port` and calls `handler` for each incoming request **sequentially** — one request at a time. Blocks indefinitely.

```txt
serve(3000, req =>
  match req
    has { "method": "GET", "path": "/ping" } => text(200, "pong")
    else => notFound()
)
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

### pathMatch

```txt
val pathMatch: (pattern: String, path: String) -> { ...String } | Null
```

Matches `path` against `pattern`. Pattern segments beginning with `:` are named captures. Returns an object of captured parameters on match, or `Null`.

```txt
pathMatch("/users/:id",       "/users/42")       // { "id": "42" }
pathMatch("/users/:id/posts", "/users/42/posts") // { "id": "42" }
pathMatch("/users/:id",       "/items/42")       // null
pathMatch("/static",          "/static")         // {}
```

---

### parseBody

```txt
val parseBody: (req: HttpRequest) -> Json | Error
```

Parses `req["body"]` as JSON.

```txt
match parseBody(req)
  is { "type": "failure", "error": e }    => badRequest(e)
  is { "type": "success", "value": body } => createItem(body)
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
val await: (Promise) -> T
```

Blocks the current thread until the promise resolves, then returns its value. Can also await an array of promises — returns an array of results.

```txt
val [users, posts] = await([
  async(() => fetchJson("https://db/users")),
  async(() => fetchJson("https://db/posts"))
])
```

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

### timeout

```txt
val timeout: (Promise, Int32) -> T
```

Adds a millisecond timeout to `promise`. If the promise does not resolve within `ms` milliseconds, the result is an error.

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

Creates a thread pool with `n` worker threads. The pool can be used with `pool.async(thunk)` for submitting work, or `pool.serve(port, handler)` for a multi-threaded HTTP server.

```txt
val pool = threadPool(8)
val p = pool.async(() => heavyWork())
```

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

### request

```txt
val request: (w: Worker, msg: Msg) -> Reply
```

Sends `msg` to worker `w` and blocks until the handler returns a reply.

---

### message

```txt
val message: (w: Worker, msg: Msg) -> Null
```

Sends `msg` to worker `w` without waiting for a reply (fire-and-forget).

---

### close

```txt
val close: (w: Worker) -> Null
```

Shuts down worker `w`, calling its `onClose` function and terminating its thread.

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

Reads the file at `path` and renders it as a template against `data`. Intended for `.lint` template files.

```txt
match render("greet.lint", { "name": "Alice", "score": 42 })
  is { "type": "failure", "error": e } => print("error: ${e}")
  is { "type": "success", "value": s } => print(s)
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
  test("adds two positives", () =>
    expect(1 + 2).toBe(3)
  ),
  test("multiple assertions", () =>
    expect(0 + 0).toBe(0)
    expect(10 + -10).toBe(0)
  )
])

run([arithmetic])
```

---

### Types

```txt
type Assertion =
  | { "type": "pass" }
  | { "type": "fail", "message": String }

type Test = {
  "name": String,
  "run": () -> Assertion | Assertion[]
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
  test("one plus one", () => expect(1 + 1).toBe(2))
])
```

---

### test

```txt
val test: (name: String, body: () -> Assertion | Assertion[]) -> Test
```

Declares a single test case. All assertions in the body are evaluated before the test is marked failed.

```txt
test("string conversions", () =>
  expect(toString(42)).toBe("42")
  expect(toString(true)).toBe("true")
)
```

---

### run

```txt
val run: (suites: Suite[]) -> Null
```

Executes all suites, prints results to stdout, and exits non-zero if any test failed.

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
