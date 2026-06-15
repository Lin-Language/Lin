# std/json

std/json — type-directed JSON decoding.

`fromJson` decodes a AnyVal value against a target type T, written either as `Person.fromJson(json)`
or `fromJson(Person, json)`, and returns `T | Error`. Decoding stops at the first structural
mismatch and returns a single Error value: { "type": "error", "message": String, "path": String },
where `path` is a JSONPath-style location such as `$.address.city`.

Number handling is driven by the target: an integer target requires an in-range integral number,
a float target accepts any number, and a AnyVal or unconstrained target accepts any number as-is.
Union targets pick the first structurally-matching variant.

## Reference

#### `fromJson`

```lin
val fromJson = (_: AnyVal, value: AnyVal): AnyVal
```

Decode a AnyVal value against a target type (see the module header).
- **`_`** — the target type (e.g. `Person`); use the `Person.fromJson(json)` or
  `fromJson(Person, json)` form.
- **`value`** — the AnyVal value to decode.
- **Returns** the decoded `T`, or an `Error` `{ type, message, path }` at the first structural
  mismatch.

#### `toJsonString`

```lin
val toJsonString = (s: String): String
```

Escape a string and wrap it in double quotes, producing a valid JSON string literal.
- **`s`** — the string to escape.
- **Returns** the quoted, escaped JSON string literal. Used by std/test to emit NDJSON records.

#### `toJson`

```lin
val toJson = (value: AnyVal): String
```

Recursively serialize ANY Lin value to a strict, valid JSON string.
- **`value`** — the value to serialize; arrays and objects recurse arbitrarily deep.
- **Returns** the JSON text. Strings (and object keys) are escaped+quoted; numbers/bools/null become
  JSON literals (non-finite floats become `null`, matching JSON.stringify).

**Example:**

```lin
toJson({ "a": 1, "b": [true, null] })  // {"a":1,"b":[true,null]}
```
