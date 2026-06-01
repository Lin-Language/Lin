# Modules Reference

## Module structure

A source file is a module. Top-level declarations are private by default. Use `export` to make them visible to other modules.

## Export syntax

```lin
export val name = "Lin"
export val add = (a: Int32, b: Int32) => a + b
export var counter = 0
export type Person = { "name": String, "age": Int32 }
```

## Import syntax

```lin
import { add, subtract } from "./math"
import { trim, toUpper } from "std/string"
```

### Aliasing

```lin
import { trim as stripWhitespace } from "std/string"
```

### Multi-line

```lin
import {
  readFile,
  writeFile,
  exists
} from "std/fs"
```

### Importing types

Types are imported with the same syntax:

```lin
import { Person, Result } from "./types"
```

Type-only imports erase at runtime — they are only used by the type checker.

## Path resolution

| Path form | Resolves to |
| --- | --- |
| `"std/string"` | Built-in standard library module |
| `"./utils"` | `utils.lin` relative to the importing file |
| `"./utils/math"` | `utils/math.lin` relative to the importing file |

Paths do not include the `.lin` extension — it is added automatically.

Absolute paths are not supported. There are no node_modules-style resolution chains.

## Module initialisation

Module code runs top-to-bottom when the module is first imported. Initialisation is lazy — a module only runs when something first uses one of its exports.

The order of global declarations within a module matters: a `val` must appear before any code that uses it, with one exception: `val` function literals can reference each other mutually because they are pre-scanned before execution.

## Circular imports

Circular imports are permitted. If module A imports from module B and module B imports from module A:

1. The first read of an export from either module triggers full initialisation of that module.
2. If initialisation of A requires reading from B, and initialisation of B requires reading from A, and neither has finished initialising, the read is a runtime error (initialisation cycle).

In practice, circular imports work fine for functions that are only called after both modules have finished initialising (the common case). They fail when the initial values of one module depend directly on the initial values of another in a cycle.

## Module caching

The compiler caches compiled modules in `.lin-cache/` by source file hash. Unchanged modules are not re-compiled or re-checked. The cache is safe to delete — it will be rebuilt on the next build.

## Standard library paths

All standard library paths begin with `std/`:

```
std/string    std/array     std/number    std/math
std/object    std/io        std/fs        std/path
std/http      std/async     std/env       std/process
std/template  std/test      std/time      std/json
std/yaml      std/jq        std/bytes     std/hash
std/net       std/signal    std/tty
```

## Generics across modules

Generic type and function declarations work across module boundaries. Cross-module generic functions are **monomorphized per importer** — each importing module gets a specialised copy for the concrete types it uses. Exported generic types (e.g. `Result<T, E>`) can be imported and applied like any other type.

## Mocking imports in tests — `replace`

In a `*.test.lin` file, a `replace <name> = <expr>` statement overrides an imported
export for the whole test program. It swaps the export's single compiled symbol, so
**every** reference resolves to the mock — the test file, the module under test, and
any transitive importer. The body is type-checked against the export's real
signature, stdlib wrappers (e.g. `std/fs.readFile`) are mockable while the built-in
intrinsics are not, and `replace` is a hard error outside a `*.test.lin`. See
[std/test](/stdlib/test) and the Testing tutorial for details.
