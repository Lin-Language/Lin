# Maps & Collections

A **map** is a homogeneous dictionary: any number of dynamically-computed keys, all mapping to the same value type. It is written with an index-signature type, `{ K: V }`. This is distinct from a [record](/tutorials/json-records.html), which has a fixed, known set of named fields.

## Declaring a map

A map type names the key type and the value type, separated by a colon. The key is a bare type (not a quoted string — that is how a map is told apart from a record):

```lin
type Counts = { String: Int32 }

val seed: Counts = { "apple": 3, "pear": 7 }
```

You can also write the type inline. An empty map literal needs an annotation, because there is nothing to infer the value type from:

```lin
var counts: { String: Int32 } = {}
```

## Reading: `m[k]` is `V | Null`

A map read is **safe**: a missing key yields `Null` rather than an error. So a read has type `V | Null`:

```lin
var counts: { String: Int32 } = {}
counts["apple"] = 3

val a = counts["apple"]   // Int32 | Null  → 3
val z = counts["banana"]  // Int32 | Null  → null
```

This is the key difference from a record, whose declared fields *always* exist and so read back as a plain `V`. Default a possibly-missing read with `??`:

```lin
val n = counts["banana"] ?? 0   // Int32 → 0
```

## Writing: `m[k] = v`

Assign through bracket notation with any key of the right type:

```lin
var counts: { String: Int32 } = {}
counts["apple"] = 3
counts["pear"] = 7
```

Maps are backed by a hashed container, so reads and writes are O(1) on average. Keys may also be an integer type — `{ Int32: V }` — which stores the key inline and is even cheaper.

## Nested writes auto-vivify

A nested assignment creates any absent intermediate map level automatically — you do not have to seed it with `?? {}` first:

```lin
type Network = { String: { String: Int32 } }

var net: Network = {}
net["a"]["b"] = 5   // net["a"] is created as an empty inner map, then ["b"] is set
```

Without this you would have to write the intermediate level by hand:

```lin
// the manual equivalent — no longer necessary:
type Network = { String: { String: Int32 } }

var net: Network = {}
net["a"] = net["a"] ?? {}
net["a"]["b"] = 5
```

Vivification applies only to **map** intermediates. A record field is always present (nothing to create), and an out-of-range array index is still a runtime error.

## The "default then mutate" idiom

To accumulate into a map of arrays, default the slot and then mutate it. Assigning a non-null value narrows the slot, so the following `.push` sees a plain `V[]` rather than `V[] | Null`:

```lin
import { push } from "std/array"

var groups: { String: Int32[] } = {}

groups["fruit"] = groups["fruit"] ?? []   // slot is now non-null
groups["fruit"].push(1)                    // type-checks: receiver is Int32[]
```

The assignment `groups["fruit"] = groups["fruit"] ?? []` tells the checker the slot holds a real array, so the immediately following `.push` — which needs a non-null receiver — is allowed without an explicit `if … != null` guard.

## Maps, records, and arrays

Lin has three collection shapes, each for a different job:

- **Map** — `{ K: V }`: an open, runtime-keyed dictionary. Use it for lookups by a dynamic key.
- **[Record](/tutorials/json-records.html)** — `{ "field": T, … }`: a fixed set of named fields, all guaranteed present.
- **[Array](/tutorials/arrays.html)** — `T[]`: an ordered, indexed sequence.

Reach for a map when keys are computed at runtime and can be any number of them; reach for a record when the field set is known and fixed.
