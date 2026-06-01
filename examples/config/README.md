# config — JSON config loader with schema validation + defaults

Loads a raw config object, fills missing fields from a schema's defaults,
validates the field types, and returns a tagged result the caller pattern-matches
on. A companion module shows the language's built-in type-directed decode
(`fromJson`) as an alternative to the hand-rolled validator.

## What it demonstrates

- **Named type aliases**: `Config` (`{ host, port, debug, name }`), `SchemaEntry`
  (`{ type, default }`), and the `decode.lin` `Person`/`Address` types.
- **Tagged-union results**: `LoadResult = Success | Failure` with a `String`
  `"type"` discriminant, consumed via `has { "type": "success", value } => ...`.
- **Dynamic objects kept as `Json`** where the value genuinely is dynamic: the raw
  untyped input, the schema map (keyed by field name), and the defaults-applied
  object built with dynamic keys / `lin_object_set`.
- **Type-directed decode** (`Person.fromJson`) returning the typed value or an
  `Error` with a `path` on structural mismatch.
- **YAML config + jq queries**: `std/yaml`'s `parse` decodes a YAML document to a
  plain `Json` value that flows through the same `load()` path as inline JSON, and
  `std/jq`'s `jq`/`jqFirst` query it — the "yq" pattern, `src.parse().jqFirst(".port")`.
- Safe bracket access with `Null` propagation through missing keys.

## Structure

| File | What it is |
| --- | --- |
| `schema.lin` | The schema, `applyDefaults`, and `validate` (returns `String[]`). Owns `SchemaEntry`. |
| `loader.lin` | `load(raw)` = defaults + validate → `LoadResult`. Owns `Config`, `Success`, `Failure`, `LoadResult`. |
| `decode.lin` | `decodePerson(j)` via `Person.fromJson`. Owns `Person`, `Address`. |
| `main.lin` | Loads sample configs (minimal / override / missing-field / wrong-type, plus one parsed from YAML) and prints the outcome. |
| `schema.test.lin` | `applyDefaults` and `validate` unit tests. |
| `loader.test.lin` | `load` success/failure and the joined error message. |
| `decode.test.lin` | `fromJson` decode success and structural-mismatch errors. |
| `integration.test.lin` | End-to-end `load` (defaults + validation) success/failure. |
| `yaml.test.lin` | YAML config parsed via `std/yaml` loaded through `load`, and `std/jq` queries over it. |

The discriminant field is typed `String` (string-literal singleton types are not
supported); the runtime shape is unchanged.

## Run / Test

```sh
lin run  examples/config/main.lin
lin test examples/config/
```
