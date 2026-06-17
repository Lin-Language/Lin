# Lin

Lin combines a minimalist syntax and a maximalist standard library with structural typing, union types, and value-based error handling — so you can build performant systems utilities and full application services without switching languages or compromising on correctness.

## Why Lin?

### JSON-native
Arrays, objects, and null are first-class values, not wrappers. There is no impedance mismatch between your data and your code.

### Structural typing
Types match by shape — with union types and generics, and no class hierarchies to model. A `{ "name": String, "age": Int32 }` goes anywhere that shape is expected, and exhaustiveness checking keeps your `match` expressions honest.

### Functional, with dot application
Immutable by default, with partial application and dot-application pipelines that read top-to-bottom: `users.filter(...).map(...)` is just `map(filter(users, ...), ...)`.

### Errors as values
Tagged union results and a built-in `Error` type — never exceptions. Functions that can fail return a union you match on and handle explicitly. Bracket access is safe by default: a missing key is `Null`, not a crash.

### Native threads, no function colouring
Share-nothing concurrency with no coloured functions — you decide at the call site whether to run work on a thread. Compiled to standalone native binaries via LLVM: no runtime, no VM, no interpreter to distribute.

## Get started

[Read the Getting Started guide](/getting-started.html) to install Lin, write your first program, and explore the language in minutes.

Or jump straight into the tutorials:

- [Hello World & I/O](/tutorials/hello-world.html) — your first Lin program
- [Values & Types](/tutorials/values.html) — understanding the type system
- [Functions](/tutorials/functions.html) — first-class functions and closures
- [Pattern Matching](/tutorials/pattern-matching.html) — the primary way to inspect values
