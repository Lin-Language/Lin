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
- Safe bracket access with `Null` propagation through missing keys.

## Structure

| File | What it is |
| --- | --- |
| `schema.lin` | The schema, `applyDefaults`, and `validate` (returns `String[]`). Owns `SchemaEntry`. |
| `loader.lin` | `load(raw)` = defaults + validate → `LoadResult`. Owns `Config`, `Success`, `Failure`, `LoadResult`. |
| `decode.lin` | `decodePerson(j)` via `Person.fromJson`. Owns `Person`, `Address`. |
| `main.lin` | Loads four sample configs (minimal / override / missing-field / wrong-type) and prints the outcome. |
| `config.test.lin` | Defaults, validation, and end-to-end `load` success/failure. |
| `decode.test.lin` | `fromJson` decode success and structural-mismatch errors. |

The discriminant field is typed `String` (string-literal singleton types are not
supported); the runtime shape is unchanged.

## Run / Test

```sh
lin run  examples/config/main.lin
lin test examples/config/
```
