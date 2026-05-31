# Lin

Lin is a compiled, functional-leaning language with the feel of a modern scripting language. It combines a minimalist syntax and maximalist standard library with structural typing, union types, and value-based error handling — so you can build performant systems utilities and full application services without switching languages or compromising on correctness.

[Get Started](/getting-started.html) · [GitHub](https://github.com/Lin-Language/Lin)

---

## A taste of Lin

```lin
import { print } from "std/io"
import { map, filter, for } from "std/array"

type User = { "name": String, "age": Int32 }

val users: User[] = [
  { "name": "Alice", "age": 30 },
  { "name": "Bob", "age": 17 },
  { "name": "Carol", "age": 25 }
]

users
  .filter(u => u["age"] >= 18)
  .map(u => u["name"])
  .for(name => print(name))
```

---

## Why Lin?

### JSON-native
Arrays, objects, and null are first-class values, not wrappers. There is no impedance mismatch between your data and your code.

### Structural typing
Types match by shape — with union types and generics, and no class hierarchies to model. A `{ "name": String, "age": Int32 }` goes anywhere that shape is expected.

### Pattern matching
Exhaustive matching with narrowing and guards. Use structural `is` for a deep type check and `has` for a presence check.

### Functional-first
Immutable by default, with partial application and dot-application pipelines that read top-to-bottom.

### Errors as values
Tagged union results and a built-in `Error` type — never exceptions. Functions that can fail return a union you match on and handle explicitly.

### Native threads, no function colouring
Share-nothing concurrency with no coloured functions. Compiled to standalone native binaries via LLVM — no runtime, no VM, no interpreter to distribute.

---

## Get started

[Read the Getting Started guide](/getting-started.html) to install Lin, write your first program, and explore the language in minutes.

Or jump straight into the tutorials:

- [Hello World & I/O](/tutorials/01-hello-world.html) — your first Lin program
- [Values & Types](/tutorials/02-values-and-types.html) — understanding the type system
- [Functions](/tutorials/03-functions.html) — first-class functions and closures
- [Pattern Matching](/tutorials/05-pattern-matching.html) — the primary way to inspect values
