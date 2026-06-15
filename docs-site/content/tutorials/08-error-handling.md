# Error Handling

Lin has no exceptions. There is no `throw`, no `try/catch`, no implicit error propagation. Errors are ordinary values — functions that can fail return a union type that includes the error case.

## Why no exceptions?

Exceptions make control flow invisible. A function that throws can disrupt the caller without the caller's type signature saying anything about it. Lin makes failures explicit at the type level: if a function can fail, its return type says so.

## The tagged union pattern

The idiomatic pattern for fallible operations is to return a tagged object:

```lin
type Result<T, E> =
  | { "type": "success", "value": T }
  | { "type": "failure", "error": E }
```

A function that might fail:

```lin
import { isInt32, parseInt32 } from "std/number"

val parseAge = (s: String): AnyVal =>
  if isInt32(s) then
    val n = parseInt32(s)
    if n >= 0 && n <= 150 then { "type": "success", "value": n }
    else { "type": "failure", "error": "age out of range: ${s}" }
  else
    { "type": "failure", "error": "not a number: ${s}" }
```

## Handling the result

Use `match`/`has` to inspect the result:

```lin
import { print } from "std/io"

val result = parseAge("25")
match result
  has { "type": "success", value } =>
    print("age is ${value}")
  has { "type": "failure", error } =>
    print("error: ${error}")
```

## The built-in `Error` type

Lin has a built-in `Error` type, structurally `{ "type": String, "message": String }`. The standard library uses it for failures rather than a hand-rolled tag, so you discriminate it with `is Error`:

```lin
import { readFile } from "std/fs"
import { print } from "std/io"

val src = readFile("data.txt")   // String | Error
match src
  is Error => print("could not read: ${src["message"]}")
  else     => print("file contents: ${src}")
```

## Composing fallible operations

Chain results by matching each step. Use `is Error` to detect a stdlib failure and produce your own tagged result:

```lin
import { readFile, readJson } from "std/fs"

val loadConfig = (path: String): AnyVal =>
  val fileResult = readFile(path)
  match fileResult
    is Error =>
      { "type": "failure", "error": "cannot read config: ${fileResult["message"]}" }
    else =>
      val parseResult = readJson(path)
      match parseResult
        is Error =>
          { "type": "failure", "error": "cannot parse config: ${parseResult["message"]}" }
        else =>
          { "type": "success", "value": parseResult }
```

## Standard library errors

The standard library (`std/fs`, `std/http`, etc.) returns `AnyVal | Error` or `T | Error`, where `Error` is the built-in `{ "type": String, "message": String }` type. Match with `is Error` to handle failures:

```lin
import { readFile } from "std/fs"
import { print } from "std/io"

val src = readFile("data.txt")
match src
  is Error => print("could not read: ${src["message"]}")
  else     => print("file contents: ${src}")
```

## Decoding untrusted JSON with `fromJson`

When you have a `AnyVal` value of unknown shape — from a file, an HTTP response, or stdin — the recommended way to get a concrete, validated type is `fromJson` from `std/json`. It performs type-directed, recursive decoding and returns `T | Error`:

```lin
import { fromJson } from "std/json"
import { readJson } from "std/fs"
import { print } from "std/io"

type Person = { "name": String, "age": Int32 }

val raw = readJson("person.json")
val person = Person.fromJson(raw)   // Person | Error
match person
  is Error => print("invalid person: ${person["message"]}")
  else     => print("${person["name"]} is ${person["age"]}")
```

This is preferable to hand-checking each field: `fromJson` validates the whole structure (including nested objects and arrays) in one step.

## Runtime errors

A small number of operations halt the program without recovery:

- Array index out of bounds
- Integer division by zero
- Non-exhaustive `match` (no arm matched, no `else`)

These cannot be caught. They indicate programming errors, not expected conditions. For expected failure modes, use a union return type.

## Async fault isolation

The one place where runtime errors become recoverable values is inside `async` thunks:

```lin
import { async, await } from "std/async"

val p = async(() => riskyOperation())
val result = await(p)
match result
  is Error => print("async task failed")
  else     => print("success: ${result}")
```

A runtime error inside the thunk is caught at the thread boundary and surfaces as an `Error` value at the `await` call site.
