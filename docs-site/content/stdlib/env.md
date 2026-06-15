# std/env

std/env — environment variable access and modification.

Read, set, unset, and snapshot the process environment. `getEnv` returns `String | Null` so a
missing variable narrows with a plain `== null` test. `setEnv`/`unsetEnv` affect the current
process and any child processes spawned afterwards.

```lin
import { getEnv, setEnv, unsetEnv, environ } from "std/env"
```

## Reference

#### `getEnv`

```lin
val getEnv = (name: String): String | Null
```

Read an environment variable.
- **`name`** — the variable name.
- **Returns** the value as a `String`, or `null` if it is unset. The narrow `String | Null` type lets
  callers narrow with a plain `== null` test (the else branch narrows to a bare `String`).

#### `setEnv`

```lin
val setEnv = (name: String, value: String): Null
```

Set an environment variable for this process.
- **`name`** — the variable name.
- **`value`** — the value to set.

#### `unsetEnv`

```lin
val unsetEnv = (name: String): Null
```

Remove an environment variable from this process.
- **`name`** — the variable name to unset.

#### `environ`

```lin
val environ = (): AnyVal
```

Snapshot all environment variables.
- **Returns** a AnyVal object mapping each variable name to its String value.
