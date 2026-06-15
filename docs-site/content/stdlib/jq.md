# std/jq

std/jq — query `AnyVal` values with jq filter programs, backed by a pure-Rust jq engine.

A filter is a stream: it can produce zero, one, or many output values. `jq` returns the full
result set as an array; `jqFirst` returns just the first (or null). Both compile errors
(invalid filter syntax) and runtime errors return the canonical `Error` value, detectable with
`is Error`. With dot-application a filter reads naturally as the tail of a pipeline, e.g.
`readFile("deploy.yaml").parse().jq(".spec.containers[].image")`.

```lin
import { jq, jqFirst } from "std/jq"
```

The engine covers essentially the full jq language: path expressions (`.a.b`, `.[0]`, `.[1:3]`,
`.[]`, `..`, optional `?`), combinators (`|`, `,`, `//`), object/array construction, arithmetic
and logic, and the standard builtins (`select`, `map`, `keys`, `has`, `length`, `to_entries`,
`add`, `sort_by`, `group_by`, `unique`, `flatten`, `split`/`join`, `test`, `empty`, and more).

## Reference

#### `jq`

```lin
val jq = (input: AnyVal, filter: String): AnyVal | Error
```

Run a jq filter over a AnyVal value and collect all results.
- **`input`** — the AnyVal value to query.
- **`filter`** — the jq filter expression.
- **Returns** a `AnyVal[]` of the filter's outputs, or an `Error` object if the filter is invalid or
  fails to evaluate.

**Example:**

```lin
jq({ "users": [{ "name": "Ada" }] }, ".users[] | .name")  // ["Ada"]
```

**Example:**

```lin
jq({ "xs": [1, 2, 3] }, ".xs | add")                      // [6]
```

#### `jqFirst`

```lin
val jqFirst = (input: AnyVal, filter: String): AnyVal | Error
```

Run a jq filter and return only its first result.
- **`input`** — the AnyVal value to query.
- **`filter`** — the jq filter expression.
- **Returns** the first output value, `null` if the filter produced none, or the `Error` object if the
  query failed.

**Example:**

```lin
jqFirst({ "users": [{ "name": "Ada" }] }, ".users[0].name")  // "Ada"
```
