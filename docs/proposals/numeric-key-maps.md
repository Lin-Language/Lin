# Proposal: Numeric-key (and, later, structural-key) hashmaps

**Status:** Design / spec (pre-implementation). Sequenced AFTER the Stage-6b `object.rs` deletion lands,
because it extends the now-unified `LinMap` and the two changes touch the same files.
**Need (Linus):** a hashmap with **sparse numeric keys**. Struct/other-value keys are a desirable
extension if cheap — this design makes them a small, self-contained follow-on, not a prerequisite.

## 0. The headline

Numeric keys are a **performance win, not a cost**. Today `LinMap` is string-keyed: a lookup runs FNV-1a
over the key bytes and, on a hash hit, **dereferences the key `*LinString`** (a cache miss to a separate
allocation) and byte-compares. That key-pointer cache miss is what dominates the ~9.5% of RAPTOR spent in
`lin_map_get`. An integer-keyed map stores the **raw `i64` inline in the slot**: hash is a few arithmetic
ops (no byte loop), compare is one instruction, and there is **no key allocation and no pointer chase**. So
`{Int: T}` is strictly faster *and* smaller than `{String: T}`.

The cost the "just hash everything uniformly" idea would incur is real but belongs to a *different* design —
one universal map with boxed `TaggedVal` keys dispatching hash/eq on a runtime tag. That makes the common
int/string cases pay for generality they don't use. We avoid it by **monomorphizing the map by key kind**
(the key type is statically known — the reset's "representation is type-determined" philosophy).

## 1. Surface language

### 1.1 Type
Generalize the index-signature type from the implicit-String `{ String: T }` to a **key-typed** form:

```
{ Int: T }        // sparse integer-keyed map   (this proposal)
{ String: T }     // unchanged
{ Point: T }      // structural key (future extension, §6)
```

The key type must be a **single integer family** (`Int8`…`Int64`, `UInt8`…`UInt64`, or the `Int` alias),
`String`, or (future) a sealed record / array type. **`Float` keys are rejected** (float equality is a
footgun). A map has exactly one key kind — it is monomorphic; you cannot mix string and int keys in one map
(a type error), which is what lets each map specialize.

### 1.2 Construction and access
Integer-keyed maps are built empty from the annotation and populated by index-set; access is index-get:

```
var seen: { Int: Boolean } = {}      // empty map, key kind inferred = Int from the annotation
seen[42] = true                      // index-set with an integer key
seen[1_000_000] = true               // sparse — no array of a million slots
val hit = seen[42]                   // index-get → Boolean | Null
val miss = seen[7]                   // → Null   (missing-key safety, spec §6.1)
```

- `m[k]` on a `{ K: T }` map returns **`T | Null`** (missing key → `Null`, consistent with bracket-access
  safety, §6.1). The checker disambiguates `m[k]` as a *map* get (vs an *array* get) from `m`'s static type.
- `m[k] = v` inserts or updates. `k` must type as `K`, `v` as `T`.
- An optional literal sugar `{ 1: a, 2: b }` is **out of scope for v1** (the empty-`{}` + index-set path
  covers the need); it can be added later as parser sugar without changing the model.

### 1.3 Combinators
`keys(m): Int[]`, `values(m): T[]`, `entries(m): [Int, T][]`, `length(m): Int`, and existing map
combinators work unchanged. **Iteration order is hash order, not insertion order** (same as today's
`{String:T}` maps; §2.6 of the reset already makes this the rule for dynamic maps).

### 1.4 Key normalization & equality
All integer keys normalize to **`i64`** internally. `{Int32: T}` and `{Int64: T}` are the same physical map;
key equality is **by integer value**, matching Lin's cross-numeric `==` (so the key `5: Int32` and `5: Int64`
are the same entry). Negative keys are fine. **Key `0` is a valid key** (see the occupancy gotcha, §3.2).

## 2. Why monomorphize (the perf argument, concretely)

| key kind | key slot | hash | eq | per-access cost |
|---|---|---|---|---|
| **Int** | raw `i64` inline | `fmix64(k)` (a few ALU ops) | `i64 ==` | **< string** (no byte loop, no key deref) |
| **String** | `*LinString` | FNV-1a over bytes | byte compare | unchanged |
| **Structural** (future) | boxed value ptr | hash of fields | `lin_value_eq` | O(fields) — paid only by struct-keyed maps |

A universal-`TaggedVal`-key map would force every key boxed (16B) and branch on a tag every access; the
int/string paths would lose their inline keys. Monomorphization keeps each map paying only for what it is.

## 3. Runtime representation (`LinMap`)

### 3.1 Slot
The slot is already `{ hash: u64, key, value: TaggedVal }`. Make `key` a **`u64`** that is interpreted by
the map's key kind:
- `Int`: the raw `i64` key, bit-cast to `u64`, stored inline. No allocation.
- `String`: a `*LinString` (as today).
- `Structural`: a boxed `*TaggedVal` owning the key value.

`LinMap` gains a `key_kind: u8` field, set once at `lin_map_alloc` and never changed.

### 3.2 Occupancy gotcha (the one non-obvious bit)
Today an empty slot is marked by `key == null`. That **breaks for integer keys**, because `0` is a valid
key and would read as empty. Fix: track occupancy via the **hash field** instead — reserve `hash == 0` for
"empty", and make every kind's hash function return a **nonzero** value for a real key (e.g. `fmix64` then
`h |= 1` or map a 0 result to a fixed sentinel). Then `0`/`null` keys are fine and occupancy is uniform
across all kinds. (Open addressing, no tombstones — there is still no delete op, matching today.)

### 3.3 Hash / eq dispatch
`hash_key(kind, key)` and `key_eq(kind, a, b)` branch on `key_kind`:
- `Int`: `fmix64(k)` — a finalizer mix (xorshift + odd-constant multiplies). **An identity hash is not
  enough**: sequential keys (1,2,3,…) into a power-of-two table cluster and degrade to linear probing, so the
  mix is load-bearing.
- `String`: existing FNV-1a + byte compare.
- `Structural`: hash combines child hashes via the same dispatch; eq calls `lin_value_eq` (the existing
  order-independent record / ordered array structural equality used by `emit_eq`).

### 3.4 RC / ownership
- `Int`: key is a scalar — **no retain/release on keys** (cheaper than strings, which retain the `LinString`).
- `Structural`: the map owns a `+1` on the boxed key (retain on insert, release on map free), mirroring the
  current value ownership.

## 4. Codegen

- **Alloc:** `lin_map_alloc(cap, key_kind)` gains the `key_kind` arg, derived from the static key type of the
  `{K:T}` being constructed (empty `{}` annotated `{Int:T}` → `key_kind = Int`).
- **Index get/set:** when `m: {Int:T}`, lower `m[k]` / `m[k]=v` to integer-key entry points that pass the key
  as a raw `i64` (no boxing) — e.g. `lin_map_get_int(m, k_i64)` / `lin_map_set_int(m, k_i64, v)`. String maps
  keep the current `*LinString` entry points. A single dispatched `lin_map_get(m, key_u64)` that reads the
  map's own `key_kind` is also viable; prefer the kind-specialized entry points so the hot path has no branch.
- The checker already resolves bracket access against the receiver type; it must route `{Int:T}` receivers to
  the map-get path with an integer key (today an integer index implies *array* get — disambiguate on the
  receiver being a `Map`, not an `Array`).

## 5. Type checker

- Represent the index-signature key type: change `Type::Map(value)` (today implicitly String-keyed) to carry
  the key type — `Type::Map { key: Box<Type>, value: Box<Type> }`. This is a `Type` enum change and touches
  the exhaustive `Type` matches, but it is the principled representation and unblocks structural keys for free.
- Resolve `{ K: T }` syntax for `K` ∈ integer families / `String` / (future) sealed record / array; **reject
  `Float` and union/`AnyVal`/`Function`/handle key types** (no stable, cheap equality).
- Type `m[k]` as `T | Null` and `m[k] = v` requiring `k: K`, `v: T`.
- Cross-numeric key compatibility: an `Int32` index into a `{Int64: T}` map is accepted (normalized to i64),
  mirroring numeric `==`/widening.

## 6. Structural keys — the cheap follow-on (not in v1)

Once §3.3's kind dispatch exists, struct/array keys are a **small** addition:
- Allow sealed record and array types as `K` in the checker.
- Add `key_kind = Structural`: store the key as a boxed value, hash via a structural hash (walk fields like
  `emit_eq` walks them), compare via the existing `lin_value_eq`.
Cost: O(fields) hash+compare **per access**, paid only by maps that use structural keys — never by int/string
maps. So: *cheap to add, not free to use.* Records compare order-independently; arrays compare ordered.
Mutable struct keys are a hazard (mutating a key after insertion corrupts the table) — either document
"don't mutate keys", or (later) require the key type be treated as frozen on insert.

## 7. Non-goals / decisions

- **Float keys:** rejected (equality footgun).
- **Mixed-key maps:** no — one key kind per map (monomorphic) is the whole point.
- **Dense integer indices (`0..N` contiguous):** use an array/growable vector instead — zero hashing, true
  O(1), best cache behaviour. This proposal is specifically for **sparse** integer keys.
- **Delete operation:** still none (open addressing without tombstones), matching today's maps.
- **Literal `{1: v}` sugar:** deferred (empty-`{}` + index-set covers the need).

## 8. Estimated touch list

- `lin-check`: `Type::Map{key,value}` + resolution of `{K:T}` + index get/set typing + bracket
  disambiguation. (Largest piece — the `Type` enum change ripples through the exhaustive matches.)
- `lin-codegen`: `key_kind` at alloc; integer-key get/set lowering; bracket dispatch on receiver kind.
- `lin-runtime` (`map.rs`): `key_kind` field; `u64` key slot; `hash_key`/`key_eq` dispatch; `fmix64`;
  occupancy-via-`hash==0`; integer entry points; (structural later).
- `stdlib`: `keys/values/entries/length` already dispatch on the map; verify they return `Int[]` for int maps.
- Tests: `crates/lin/tests/integration.rs` (int-map get/set/missing/iteration/key-0/negative/sparse-1M) +
  a `stdlib` map test; a microbench vs `{String:T}` to confirm the win.
