# Modules & Imports

Lin programs are organised into modules. Each `.lin` source file is a module. Modules export named values and types, and import them from other modules.

## Exporting

Use the `export` keyword before a `val`, `var`, or `type` declaration:

```lin
// math.lin
export val add = (a: Int32, b: Int32): Int32 => a + b
export val subtract = (a: Int32, b: Int32): Int32 => a - b

export type Pair = {
  "first": Int32,
  "second": Int32
}
```

Declarations without `export` are private to the module.

## Importing

Import named exports using destructuring syntax:

```lin
import { add, subtract } from "./math"
import { Pair } from "./math"
```

Paths are relative to the importing file's directory. The `.lin` extension is added automatically.

## Aliasing imports

Use `as` to rename an imported binding:

```lin
import { add as sum } from "./math"

val result = sum(1, 2)   // 3
```

## Multi-line imports

For readability when importing many names:

```lin
import {
  trim,
  toUpper,
  toLower,
  split,
  join
} from "std/string"
```

## Standard library

Standard library modules use the `std/` prefix:

```lin
import { print } from "std/io"
import { map, filter, reduce } from "std/iter"
import { trim, toUpper } from "std/string"
import { readFile, writeFile } from "std/fs"
import { parseInt32 } from "std/number"
```

## Module resolution

- Paths starting with `std/` resolve to the embedded standard library.
- All other paths resolve relative to the importing file's directory with `.lin` appended.
- Absolute paths are not supported.

## Module initialisation

Module code runs top-to-bottom when the module is first imported. Module initialisation is lazy — a module is only initialised when something first tries to use one of its exports.

Circular imports are permitted. The first read of an export from a module forces full initialisation of that module. If a circular dependency causes a module to be read while it is still being initialised, the runtime reports an error.

## Multi-file example

```
project/
  main.lin
  utils/
    string-helpers.lin
    math-helpers.lin
```

`utils/string-helpers.lin`:

```lin
import { toUpper, trim } from "std/string"

export val shout = (s: String): String =>
  s.trim().toUpper()
```

`main.lin`:

```lin
import { print } from "std/io"
import { shout } from "./utils/string-helpers"

print(shout("  hello world  "))
// HELLO WORLD
```
