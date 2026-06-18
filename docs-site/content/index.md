# Lin

Lin pairs a minimalist syntax with a maximalist standard library — dot-application pipelines, structural typing, and value-based errors — with one obvious way to do most things, so you can build performant systems utilities and full application services without switching languages or compromising on correctness.

## Why Lin?

### Functional, reads like OO
`a.f(b)` is exactly `f(a, b)` — dot-application, not methods. Functions are free-standing, first-class verbs you compose and partially apply, yet you call them noun-first, so `users.filter(...).map(...)` reads top-to-bottom and autocompletes like a method chain. It's functional programming with the call-site ergonomics of OO — no classes to route every verb through, and no [kingdom of nouns](https://steve-yegge.blogspot.com/2006/03/execution-in-kingdom-of-nouns.html).

### Structural typing, no classes
Types are shapes, not hierarchies — and the shapes are JSON. Records compose with `&` and vary with `|`; a `{ "name": String, "age": Int32 }` goes anywhere that shape is expected, generics stay zero-cost, and exhaustiveness checking keeps every `match` honest.

### Errors are values
No exceptions, no hidden control flow. A function that can fail returns a union — `T | Error` — that you `match` on and handle explicitly. Bracket access is safe by default: a missing key is `Null`, not a crash.

### High-level to write, native to run
Records compile to flat packed structs and generics monomorphize away, so idiomatic code is the fast code. The LLVM backend emits standalone native binaries — no runtime, no VM, no GC, nothing to install alongside.

### Concurrency without colouring
Share-nothing native threads with no coloured `async` functions — you decide at the call site whether work runs on a thread. The same code composes whether it runs sequentially or in parallel.

### Batteries included
A maximalist standard library — HTTP, sockets, processes, compression and archives, CSV/YAML/JSON/jq, crypto, dates, streams, events, testing — so you build real programs without leaving the language or pulling in a dependency tree.

## Get started

[Read the Getting Started guide](/getting-started.html) to install Lin, write your first program, and explore the language in minutes.

Or jump straight into the tutorials:

- [Hello World & I/O](/tutorials/hello-world.html) — your first Lin program
- [Values & Bindings](/tutorials/values.html) — val, var, and literals
- [Functions](/tutorials/functions.html) — first-class functions and closures
- [Pattern Matching](/tutorials/pattern-matching.html) — the primary way to inspect values
