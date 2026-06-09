# std/yaml

std/yaml — parse and serialise YAML.

YAML maps onto the same data model as JSON, so a parsed document is an ordinary `Json` value
(object, array, string, number, boolean, or null). Because YAML decodes to plain `Json`, it
composes directly with std/json's `fromJson` and with std/jq filters — a "yq" query is just a
`parse` followed by `jq`. Fallible functions return the canonical `Error` value
`{ "type": "error", "message": String }`, detectable with `is Error`.

```lin
import { parse, parseAll, stringify, stringifyAll } from "std/yaml"
```

Data-model notes: non-string mapping keys (`42: x`) are coerced to their string form; the YAML
1.2 core schema is followed (`yes`/`no`/`on`/`off` parse as strings, not booleans); anchors,
aliases, and merge keys (`<<`) are resolved on parse; comments are dropped (stringify
round-trips through the parsed value, not the original text).

## Reference

#### `parse`

```lin
val parse = (src: String): Json | Error
```

Parse a single YAML document into a Json value.
- **`src`** — the YAML source text.
- **Returns** the parsed value, or an `Error` if `src` is not valid YAML.

#### `parseAll`

```lin
val parseAll = (src: String): Json | Error
```

Parse a multi-document YAML stream (`---`-separated) into an array of values.
- **`src`** — the YAML source text.
- **Returns** a `Json[]` of the parsed documents, or an `Error` if `src` is not valid YAML.

**Example:**

```lin
parseAll("a: 1\n---\nb: 2\n").length()  // 2
```

#### `stringify`

```lin
val stringify = (value: Json): String
```

Serialize a Json value to a single YAML document.
- **`value`** — the value to serialize.
- **Returns** the YAML text.

#### `stringifyAll`

```lin
val stringifyAll = (values: Json[]): String
```

Serialize an array of values to a multi-document (`---`-separated) YAML stream.
- **`values`** — the values to serialize, one document each.
- **Returns** the multi-document YAML text.
