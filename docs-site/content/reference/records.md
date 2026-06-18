# Records & Objects

A **record** is a structurally-typed object with a known, fixed set of named
fields. This page is the reference for record types, object literals, field
access, intersection types, and the value/reference model. For dictionaries with
an open, runtime-determined set of keys, see [Maps & Collections](/reference/maps.html).

## Object literals

An object literal is written with quoted string keys:

```lin
val person = { "name": "Alice", "age": 30 }
```

Shorthand `{ name, age }` is sugar for `{ "name": name, "age": age }`. The spread
form `{ ...base, "age": 31 }` copies `base`'s fields into a new object and then
applies the listed fields (later fields win), producing a fresh value.

```lin
val name = "Alice"
val age = 30
val person = { name, age }              // { "name": "Alice", "age": 30 }
val older = { ...person, "age": 31 }    // a fresh copy with age overridden
```

## Field access

Fields are read with **bracket notation** — dot syntax is reserved for function
application, never field access:

```lin
val n = person["name"]
```

Bracket access is **safe by default** (spec §7.1/§7.2). Accessing a key that is
not present yields `Null` rather than raising an error:

```lin
val data = { "a": { "b": "value" } }
val present = data["a"]["b"]    // "value"
```

`Null` **propagates through chains**: if any link in a chain is `Null` (a missing
key, or a `Null` value), the whole chain evaluates to `Null` with no error. This
is the built-in equivalent of optional chaining in other languages, applied to
every bracket access:

```lin
type Bag = { String: { String: String } }
val bag: Bag = {}
val deep = bag["missing"]["nope"]   // Null — no error, no intermediate checks
```

Array out-of-bounds indexing is the one exception: it is a runtime error, not
`Null` (see [Error Handling](/reference/error-handling.html)).

## Named record types

A `type` declaration names a record shape. Keys are quoted strings; each field
has a declared type:

```lin
type Person = { "name": String, "age": Int32 }
```

A named record type can be used anywhere a type is expected — a `val`/`var`
annotation, a function parameter, a return type, or an array element type:

```lin
type Person = { "name": String, "age": Int32 }

val describe = (p: Person): String =>
  "${p["name"]} is ${p["age"]}"

val people: Person[] = [{ "name": "Alice", "age": 30 }]
```

## Records are value types with a fixed shape

A **named** record type is *sealed* (spec §5.9.1, ADR-069): a value whose static
type is `Person` holds **exactly** `Person`'s fields — no more. This is a
representation guarantee. The compiler lays a named record out as a **flat packed
struct** with fields at constant offsets, not as a dynamic dictionary. Field
access on a typed record therefore **resolves to a fixed slot** — a
constant-offset load — never an association-list or hash lookup. A typed record
is dramatically faster than the equivalent dynamic `AnyVal` object, which pays a
key-lookup per access.

Representation is **type-determined**, not inferred: a value statically typed as
a sealed record is *always* the packed form. Scalars (numeric / `Boolean`) inline
at their natural offset; heap fields (`String`, an array, a map, or a nested
sealed record) are 8-byte owned pointer slots.

### Records have reference semantics

Although the layout is a packed struct, records are **observably-mutable
reference values**. Binding a record does not copy it — `val b = a` makes `b` and
`a` refer to the **same** record, and mutation through one alias is visible
through every other alias and through any parameter the record was passed to:

```lin
type Point = { "x": Int32, "y": Int32 }

val a: Point = { "x": 1, "y": 2 }
val b = a            // b and a alias the same record
// mutating a["x"] would be observed through b["x"] as well
```

When you need an **independent** value, copy explicitly — spread it (`{ ...a }`)
or rebuild it:

```lin
val independent = { ...a, "x": 9 }   // a fresh record; mutating it never touches a
```

### Lossy projection at named-type boundaries

Sealing does **not** weaken structural compatibility (see below), but it does
mean a named record cannot smuggle extra fields. When a wider value (one with
extra fields, or an `AnyVal` value) flows into a slot of named type `T` — a
parameter, an annotated binding, a typed return, or a `T[]` element — it is
**copied** into a fresh sealed value containing only `T`'s fields. The extra
fields are dropped *from the copy*; the original value, in its own scope, is
untouched.

```lin
type Named = { "name": String }

val wide = { "name": "Alice", "age": 99 }   // anonymous record with an extra field
val nm: Named = wide                         // projects to a fresh { "name": "Alice" }
// nm["age"]   → compile error: `age` is not a field of Named
```

`Person.fromJson(json)` projects the same way: it validates and keeps exactly
`Person`'s fields, dropping unknown keys. If you must preserve arbitrary extra
keys (a pass-through envelope), type the value `AnyVal`, not a named record.

## Intersection types (`&`)

Two **record** types may be combined with `&` to form the record containing
**all** of both operands' fields (spec §5.4 "Record intersection", ADR-061):

```lin
type Person = { "name": String, "age": Int32 }
type Employee = Person & { "salary": Int32 }
// Employee is { "name": String, "age": Int32, "salary": Int32 }

val alice: Employee = { "name": "Alice", "age": 30, "salary": 50000 }
```

Rules for combining:

- **Record-only.** Every operand must be an object/record type. `Int32 & String`,
  or `&` with a union, is a compile-time error
  (*"intersection `&` is only valid between record types"*).
- **Shared fields must agree.** A field present in more than one operand must have
  the **same** type in each (it is de-duplicated). Conflicting field types are a
  compile-time error (*"intersection type has conflicting field …"*).
- **Left-associative and composing.** `A & B & C` merges all three.
- **Precedence.** `&` binds **tighter** than union `|`, so `A & B | C` parses as
  `(A & B) | C`.

The result is an ordinary record type with no special runtime representation.
When bound to a named `type` declaration, the merged record is **sealed** exactly
as if it had been written out in full.

## Structural typing & assignability

Record types are **structural**: compatibility is decided by shape, not by name.
A **wider** record (one with extra fields) is assignable where a **narrower** one
is required, provided every required field is present and its type is compatible:

```lin
type Named = { "name": String }

val greet = (x: Named): String => "Hello ${x["name"]}"

// Works — the argument has at least a compatible "name" field.
greet({ "name": "Alice", "age": 30 })
```

The reverse does not hold: a value missing a required field is not assignable to a
type that demands it. Recall the projection rule above — flowing a wider value
into a *named* record slot keeps it sound by copying out exactly the declared
fields.

For an `is`/`has` pattern that narrows a record at runtime, see
[Pattern Matching](/reference/pattern-matching.html).

## Records vs maps

A **record** has a fixed, named set of fields, each with its own type — its shape
is known statically. A **map** (`{ String: V }`) has an open, homogeneous,
runtime-determined set of keys, all mapping to one value type. A value is *either*
a fixed record *or* an index-signature map, never both. Reach for a record when
the field set is known; reach for a map when the key set is dynamic or large. See
[Maps & Collections](/reference/maps.html).
