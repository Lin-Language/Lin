# std/json

Type-directed JSON decoding. `fromJson` is a compiler special form that validates a `Json` value against a named type recursively, returning either the typed value or an `Error`.

```lin
import { fromJson } from "std/json"
```

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `fromJson` | `(Type, Json) -> T \| Error` | Decode a `Json` value against type `T` |

The first argument is a **type** (a type name or `type` alias), not a runtime value. Write it idiomatically as `T.fromJson(json)`, or equivalently as `fromJson(T, json)`.

## Validation rules

`fromJson` recursively validates structure and stops at the first mismatch, returning a single `Error` value:

```lin
type DecodeError = {
  "type":    "error",
  "message": String,
  "path":    String     // JSONPath-ish location, e.g. "$.address.city"
}
```

- **Structure** — required fields must be present (a nullable field may be absent); extra fields are ignored.
- **Field types** — each field is checked against its declared type.
- **Numbers** — an integer target requires an in-range integral number (`3.14` and out-of-range values are rejected); a float target accepts any number; an unconstrained `Json` target accepts any number as-is.
- **String literals** — a string-literal field type must match exactly.
- **Unions** — the first structurally-matching variant is chosen.
- **Arrays** — fixed-length tuple targets must match the array length exactly.

---

### Decoding an object

On success, `fromJson` returns the value typed as `T`; on failure it returns the `Error` object. Discriminate with `is Error` or by matching on the `"type"` field:

```lin
import { fromJson } from "std/json"
import { print } from "std/io"

type Person = { "name": String, "age": Int32 }

val p = Person.fromJson({ "name": "Bob", "age": 30 })

match p
  is Error => print("decode failed: ${p["message"]} at ${p["path"]}")
  else     => print("hello ${p["name"]}")
```

---

### The direct call form

`fromJson(T, json)` is equivalent to `T.fromJson(json)`:

```lin
val p = fromJson(Person, { "name": "Zoe", "age": 9 })
```

---

### Errors carry a path

```lin
type Addr = { "city": String }
type Nested = { "name": String, "address": Addr }

val e = Nested.fromJson({ "name": "A", "address": { "city": 5 } })
e["type"]   // "error"
e["path"]   // "$.address.city"

type IntArr = Int32[]
val a = IntArr.fromJson([1, 2, "x"])
a["path"]   // "$[2]"
```

---

### Number policy

```lin
Person.fromJson({ "name": "Bob", "age": 3.14 })   // Error: non-integral for Int target
Person.fromJson({ "name": "Bob", "age": 5000000000.0 })   // Error: out of Int32 range
Float64.fromJson(5)   // ok: float target accepts an integer
```

---

### Unions and recursive types

A union target picks the first variant that matches structurally; recursive types decode all the way down:

```lin
type Shape = { "k": String, "r": Float64 } | { "k": String, "w": Int32 }

Shape.fromJson({ "k": "circle", "r": 1.5 })   // matches the first variant

type Tree = { "value": Int32, "children": Tree[] }

Tree.fromJson({ "value": 1, "children": [{ "value": 2, "children": [] }] })
```
