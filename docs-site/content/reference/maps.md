# Maps & Collections

A **map** is an index-signature object type: a dictionary with an open,
runtime-determined set of homogeneous keys, all mapping to a single value type.
This page is the reference for the map type. For the fixed-field record type, see
[Records & Objects](/reference/records.html); for the full array type reference,
see [Types](/reference/types.html).

## Map type syntax `{ K: V }`

A map type is written with a **key type** and a **value type** separated by a
colon (spec §5.1.1, ADR-055). The key is written as a bare type, not a quoted
string — that is how the index-signature form is distinguished from a fixed
record:

```lin
type Counts = { String: Int32 }

var counts: Counts = {}
counts["apple"] = 3
counts["pear"] = 7
```

Both the key set and the value type are **homogeneous**: every key has the same
key type, and every value has the same value type `V`.

**String keys.** The key may be the literal `String`, or any type alias that
resolves to `String` (e.g. `type StopID = String` then `{ StopID: V }`). The
alias is purely a naming convenience documenting intent; the underlying key type
is always `String`.

**Integer keys.** The key type may instead be an integer family (`Int8`…`Int64`,
`UInt8`…`UInt64`), giving a map keyed by an integer:

```lin
var seen: { Int32: Boolean } = {}
seen[42] = true
seen[1000000] = true       // sparse keys — no dense array allocated
```

Integer keys are stored inline as a raw `i64`, so an integer map is faster and
smaller than a string map; `0`, negatives, and large sparse keys all work.
`Float` keys are rejected (equality is a footgun), as are union / `AnyVal` /
function / handle key types. A map has exactly one key kind — you cannot mix
string and integer keys in one map.

> An empty `{}` literal infers its map type from context (an annotated binding or
> return type). An evidence-free empty `{}` — no annotation, no contextual type,
> no contents — is a compile error; annotate it, e.g. `var m: { String: Int32 } = {}`.

## Reading a map: `m[k] : V | Null`

A map read uses safe bracket access (spec §6.1). A missing key yields `Null`, so
the type of a read is `V | Null`:

```lin
var counts: { String: Int32 } = {}
counts["apple"] = 3
val n = counts["apple"]      // Int32 | Null
val miss = counts["pear"]    // Null
```

For a defaulted read, use the null-coalescing operator `??` (see
[Error Handling](/reference/error-handling.html)), which collapses the result to
the bare value type:

```lin
val safe = counts["pear"] ?? 0    // Int32 — the absent-key Null is replaced
```

The key passed to `m[k]` must match the map's **key kind**: a numeric key into a
`{ String: V }` map, or a string key into a `{ Int: V }` map, is a compile-time
error. The read and write key rules are symmetric.

## Writing a map: `m[k] = v`

A write requires the value to have the map's value type and the key to match the
key kind:

```lin
var counts: { String: Int32 } = {}
counts["apple"] = 3          // ok: key is String, value is Int32
```

## Nested-write auto-vivification

A nested index-assignment **auto-vivifies absent intermediate map levels** (spec
§7.1, ADR-076). When an intermediate level along the write path is absent, an
empty map of that level's statically-known value type is created and stored back,
then the write proceeds — so a nested write always succeeds rather than being
silently dropped:

```lin
var index: { String: { String: Int32 } } = {}
index["x"]["y"] = 5          // creates the inner { String: Int32 } map, then sets "y"
val r = index["x"]["y"]      // Int32 | Null  →  5
```

This recurses outermost-first, so every intermediate map level of an arbitrarily
deep `m[a][b][c] = v` is created.

The behaviour is **bounded to map intermediates**:

- **Records are total** — their fields always exist, so there is nothing to
  vivify.
- **Arrays cannot be vivified by key** — an out-of-range array index stays a
  runtime error.

**Reads are unchanged.** A read never mutates: it null-propagates and returns
`Null` for an absent path. The read/write asymmetry is intentional — a read
retrieves (absence is a valid `Null` answer), a write stores (so it ensures the
path).

## Assignment-based index-place narrowing

A map read is nullable (`V | Null`), so the "default the slot, then mutate it"
idiom needs the re-read to be known non-null. Lin narrows it: after an
assignment `m[k] = e` with a **non-null** `e`, a later read of `m[k]` narrows to
the assigned non-null type (spec §7.2, ADR-077). This makes the array-bucket
idiom type-check:

```lin
import { push } from "std/array"

var groups: { String: Int32[] } = {}
groups["a"] = groups["a"] ?? []   // slot is now a non-null Int32[]
groups["a"].push(1)               // the re-read narrows to Int32[], so .push is valid
```

The narrowing is **deliberately conservative** — only the read *immediately*
following the assignment (before any intervening call) is narrowed. A recorded
narrowing of `m[k]` is dropped when, between the assignment and a later read:

- **(a)** the base, or any identifier in the (possibly nested) key path, is
  reassigned;
- **(b)** a write lands through the same base (`m[j] = …`);
- **(c)** **any** function call occurs — a call could delete or re-key the map;
- **(d)** the enclosing block ends.

Because the receiver read of `m[k].push(x)` captures its narrowed type *before*
`push` is dispatched, rule (c) does not interfere with that idiom. When any
precondition is uncertain, the narrowing is simply not recorded — the read merely
re-widens to `V | Null`, which is a usability nit, never an unsound type.

## Map utilities

`keys(m) : String[]`, `values(m) : V[]`, and `entries(m)` are available via
`std/object`. A `{ K: V }` value is backed at runtime by a hashed container
giving **O(1) average** lookup and insert — reach for a map whenever you have a
genuinely large or open-keyed dictionary, rather than a dynamic `AnyVal` object
(whose association-list layout is O(n) per access).

There is no implicit `AnyVal → { K: V }` coercion: decode an `AnyVal` value
through `fromJson` or narrow it, exactly as for any other concrete type (see
[Error Handling](/reference/error-handling.html)).

## Arrays

Arrays are covered in full under [Types](/reference/types.html): `T[]` is an
unbounded array of `T`, and `[T1, T2, …]` is a fixed-length array with positional
element types. Unlike maps, an out-of-range array index is a runtime error, not
`Null`.
