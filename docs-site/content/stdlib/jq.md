# std/jq

Query `Json` values with [jq](https://jqlang.github.io/jq/) filter programs, backed by a pure-Rust jq engine. A filter can produce zero, one, or many output values; `jq` returns the full result set as an array, and `jqFirst` returns just the first. Both compile errors (invalid filter syntax) and runtime errors return the canonical `Error` value, detectable with `is Error`.

```lin
import { jq, jqFirst } from "std/jq"
```

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `jq` | `(Json, String) -> Json[] \| Error` | Run a jq filter, collecting all outputs |
| `jqFirst` | `(Json, String) -> Json \| Error` | Run a jq filter, returning the first output (or `Null`) |

---

### `jq`

Runs `filter` against `input` and returns every output value as a `Json` array. A jq filter is a *stream*: `.users[]` emits one value per element, so the result is an array even for a single match.

```lin
import { jq } from "std/jq"

val data = { "users": [{ "name": "Ada", "age": 36 }, { "name": "Bob", "age": 30 }] }

jq(data, ".users[] | .name")                      // ["Ada", "Bob"]
jq(data, ".users | map(.age) | add")              // [66]
jq(data, ".users[] | select(.age > 32) | .name")  // ["Ada"]
```

Because of dot-application, a filter reads naturally as the tail of a pipeline:

```lin
import { readFile } from "std/fs"
import { parse } from "std/yaml"

readFile("deploy.yaml").parse().jq(".spec.containers[].image")
```

A bad filter or a runtime error inside the filter returns an `Error`:

```lin
match jq(data, ".[")
  is Error => print("bad filter: ${jq(data, ".[")["message"]}")
  else     => null
```

---

### `jqFirst`

Like [`jq`](#jq), but returns just the first output value instead of an array. Returns `Null` when the filter produces no output, and propagates an `Error` unchanged.

```lin
import { jqFirst } from "std/jq"

val data = { "users": [{ "name": "Ada" }, { "name": "Bob" }] }

jqFirst(data, ".users[0].name")             // "Ada"
jqFirst(data, ".users[] | select(false)")   // null
```

---

### Supported filters

The engine covers essentially the full jq language:

- **Path expressions** — identity `.`, fields `.a.b`, indices `.[0]`, slices `.[1:3]`, iteration `.[]`, optional `?`, recursive descent `..`
- **Combinators** — pipe `|`, comma `,`, the alternative operator `//`
- **Construction** — object `{a: .x}` and array `[.foo[]]` literals
- **Arithmetic & logic** — `+ - * / %`, comparisons, `and` / `or` / `not`
- **Builtins** — `select`, `map`, `keys`, `values`, `has`, `length`, `type`, `to_entries` / `from_entries` / `with_entries`, `add`, `min` / `max`, `sort` / `sort_by`, `group_by`, `unique`, `flatten`, `range`, `split` / `join`, `ascii_downcase`, `startswith`, `test`, `empty`, `error`, and more.
