# std/yaml

Parse and serialise YAML. YAML maps onto the same data model as JSON, so a parsed document is an ordinary `Json` value — object, array, string, number, boolean, or `null`. Fallible functions return the canonical `Error` value (`{ "type": "error", "message": String }`) on a parse failure, detectable with `is Error`.

```lin
import { parse, parseAll, stringify, stringifyAll } from "std/yaml"
```

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `parse` | `(String) -> Json \| Error` | Parse one YAML document |
| `parseAll` | `(String) -> Json[] \| Error` | Parse a `---`-separated multi-document stream |
| `stringify` | `(Json) -> String` | Serialise a value to block-style YAML |
| `stringifyAll` | `(Json[]) -> String` | Serialise values to a `---`-separated YAML stream |

---

### `parse`

Parses a single YAML document into a `Json` value. Returns an `Error` if the input is not well-formed YAML.

```lin
import { parse } from "std/yaml"
import { print } from "std/io"

val cfg = parse("name: web\nreplicas: 3\nports:\n  - 80\n  - 443\n")

match cfg
  is Error => print("bad config: ${cfg["message"]}")
  else     => print(cfg["name"])   // web
```

Because YAML decodes to a plain `Json` value, it composes directly with `std/json`'s type-directed decode:

```lin
import { fromJson } from "std/json"

type Service = { "name": String, "replicas": Int32 }

val svc = Service.fromJson(parse("name: web\nreplicas: 3\n"))
// svc is Service | Error
```

---

### `parseAll`

Parses a multi-document YAML stream — documents separated by a `---` line — into an array of `Json` values. Returns an `Error` if any document is malformed.

```lin
import { parseAll } from "std/yaml"

val docs = parseAll("a: 1\n---\nb: 2\n---\nc: 3\n")
docs.length()   // 3
docs[1]["b"]    // 2
```

---

### `stringify`

Serialises a `Json` value to a block-style YAML document. Object key order follows insertion order (consistent with `writeJson`).

```lin
import { stringify } from "std/yaml"
import { print } from "std/io"

print(stringify({ "name": "web", "ports": [80, 443] }))
// name: web
// ports:
// - 80
// - 443
```

---

### `stringifyAll`

Serialises an array of `Json` values to a multi-document YAML stream, each document preceded by a `---` separator. The result round-trips through `parseAll`.

```lin
import { stringifyAll } from "std/yaml"

stringifyAll([{ "a": 1 }, { "b": 2 }])
// ---
// a: 1
// ---
// b: 2
```

---

### yq-style pipelines

YAML decodes to `Json`, and `std/jq` filters `Json`, so a "yq" query is just `parse` followed by `jq` — read naturally as a dot-chained pipeline:

```lin
import { readFile } from "std/fs"
import { parse } from "std/yaml"
import { jq } from "std/jq"

readFile("deploy.yaml").parse().jq(".spec.containers[].image")
// ["nginx:1.25", "redis:7"]
```

---

### Notes on the YAML data model

A few places where YAML's surface differs from JSON's value model:

- **Non-string mapping keys** (YAML allows `42: x`, `true: y`) are coerced to their string form to fit Lin's object-key model: `{ "42": ... }`.
- **YAML 1.2 core schema** is followed, so `yes`/`no`/`on`/`off` parse as strings, not booleans. Timestamps and `!!`-tagged custom types decode to strings (`Json` has no date type).
- **Anchors and aliases** (`&a` / `*a`) are resolved on parse, and merge keys (`<<`) are applied.
- **Comments are dropped** — `stringify` round-trips through the parsed value, not the original text.
