# std/json

std/json — type-directed JSON decoding (ADR-031).

`fromJson` is a compiler special form: the type checker recognises the surface name
`fromJson` at the call site (in both the `Person.fromJson(json)` dot form and the
`fromJson(Person, json)` direct form) and decodes the Json value against the target type T,
returning `T | Error`. Decoding stops at the first structural mismatch and returns a single
Error value: { "type": "error", "message": String, "path": String } (the `path` is a
JSONPath-ish location such as `$.address.city`).

Number policy is target-driven: an integer target requires an in-range integral number; a
float target accepts any number; a Json/unconstrained target accepts any number as-is.
Union targets pick the FIRST structurally-matching variant.

Decode a Json value against a target type (compiler special form; see the module header).
- **`_`** — the target TYPE (e.g. `Person`); use the `Person.fromJson(json)` or
  `fromJson(Person, json)` form. No ordinary value-level wrapper can express a type argument.
- **`value`** — the Json value to decode.
- **Returns** the decoded `T`, or an `Error` `{ type, message, path }` at the first structural
  mismatch. (The export exists so the import resolves and `lin check` does not flag `fromJson`;
  its body is never evaluated — the checker rewrites real call sites into the decode special
  form.)

## Reference

#### `fromJson`

```lin
val fromJson = (_: Json, value: Json): Json
```


#### `toJsonString`

```lin
val toJsonString = (s: String): String
```

Escape a string and wrap it in double quotes, producing a valid JSON string literal.
- **`s`** — the string to escape.
- **Returns** the quoted, escaped JSON string literal. Used by std/test to emit NDJSON records.

#### `toJson`

```lin
val toJson = (value: Json): String
```

Recursively serialize ANY Lin value to a strict, valid JSON string.
- **`value`** — the value to serialize; arrays and objects recurse arbitrarily deep.
- **Returns** the JSON text. Strings (and object keys) are escaped+quoted; numbers/bools/null become
  JSON literals (non-finite floats become `null`, matching JSON.stringify).
- **Example:** toJson({ "a": 1, "b": [true, null] })  // {"a":1,"b":[true,null]}
