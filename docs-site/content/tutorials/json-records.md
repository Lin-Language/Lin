# Working with JSON

Lin's data model is built directly on JSON. Objects, arrays, strings, numbers, booleans, and null are all first-class values in the language.

## Object literals

Objects use quoted string keys (strict JSON syntax):

```lin
val person = {
  "name": "Alice",
  "age": 30,
  "active": true,
  "address": null
}
```

### Shorthand field syntax

When the key name matches a local variable, you can omit the quotes and colon:

```lin
val name = "Alice"
val age = 30
val person = { name, age }
// same as: { "name": name, "age": age }
```

### Spread operator

Copy fields from one object into another:

```lin
val base = { "name": "Alice", "age": 30 }
val updated = { ...base, "age": 31 }
// { "name": "Alice", "age": 31 }
```

Later fields (including spreads) override earlier ones.

## Accessing fields

Use bracket notation:

```lin
val name = person["name"]    // "Alice"
val age = person["age"]      // 30
```

**Null propagation**: accessing a missing key returns `null` instead of an error. Accessing a field on `null` also returns `null`:

```lin
val city = person["address"]["city"]   // null (no error)
```

This makes deep access safe without intermediate null checks.

## Array literals

```lin
val numbers = [1, 2, 3, 4, 5]
val names = ["Alice", "Bob", "Charlie"]
val mixed = [1, "hello", true, null]
```

Arrays are zero-indexed:

```lin
val first = numbers[0]    // 1
val last = numbers[4]     // 5
```

Array index out of bounds is a runtime error (unlike missing object keys, which return null).

## Nested access

```lin
val data = {
  "user": {
    "profile": {
      "bio": "Loves programming"
    }
  }
}

val bio = data["user"]["profile"]["bio"]
// "Loves programming"

val missing = data["user"]["settings"]["theme"]
// null (null propagates safely through the chain)
```

## Type-safe objects

For typed objects, declare a type alias:

```lin
type Person = {
  "name": String,
  "age": Int32
}

val describe = (p: Person): String =>
  "${p["name"]} is ${p["age"]} years old"
```

The type checker enforces that the object has the required fields.

A named record type like this is a **value type**: it compiles to a flat, packed struct with a fixed set of fields — not a dynamic dictionary. That's why field access is fast (a known field sits at a known offset) and why the shape is fixed once declared. If you need open-ended, dynamically-keyed data, reach for a [map](/tutorials/maps.html) instead.

## Combining record types

Use intersection `&` to build a new record type with all the fields of two others:

```lin
type Person = { "name": String, "age": Int32 }
type Employee = Person & { "salary": Int32 }

val e: Employee = { "name": "Alice", "age": 30, "salary": 50000 }
```

`Employee` has every field of `Person` plus `salary`. The operands must be record types, and any shared field must agree on its type. For dynamic key→value data rather than a fixed set of named fields, use a [map](/tutorials/maps.html).

## Working with `AnyVal`

When you don't know the shape in advance — e.g., data from a file or HTTP request — use the `AnyVal` type:

```lin
import { readJson } from "std/fs"
import { print } from "std/io"

val result = readJson("config.json")   // AnyVal | Error
match result
  is Error => print("error: ${result["message"]}")
  else =>
    val config = result
    print(config["version"])
```

`AnyVal` allows accessing any key without type errors. The result of a bracket access on `AnyVal` is also `AnyVal`.

Note that `AnyVal` is a *covariant sink*: any value assigns into a `AnyVal`, but a `AnyVal` value does not implicitly assign out to a concrete object type with required fields. To convert untrusted `AnyVal` into a typed value, validate it with `fromJson` (from `std/json`) or narrow it with `is`/`has`.
