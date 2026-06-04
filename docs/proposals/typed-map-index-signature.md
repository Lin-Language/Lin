# Proposal: a typed index-signature object type `{ String: T }`

Status: **accepted — Option A (index-signature `{ String: T }`); not yet implemented.** The language
owner has chosen the index-signature form over a separate nominal `Map<K, V>` container. Option B is
recorded below only as the considered-and-rejected alternative; build Option A.

Motivated by the RAPTOR port (`benchmarks/compare/raptor/lin/`), which is written entirely in `Json`
(282 `: Json` annotations, zero named types) — not because the port was lazy, but because a chunk of
its data model has **no type to express it**.

## The gap in one sentence

Lin can type a **fixed-field record** (`type StopTime = { "stop": String, "arrivalTime": Int32, … }`)
and **unions** of them (`Connection = TripConnection | Transfer`, discriminated with `match … is`),
but it has **no type for an object used as a dictionary** — arbitrary, dynamically-computed string
keys all mapping to the same value type `T`. Today that forces `Json`, which is untyped and (because
`Json` objects are association lists) O(n) per lookup.

## What exists today vs. what's missing

`crates/lin-codegen/src/codegen/types.rs`: `Type::Object { fields, sealed }` is a **fixed, known set
of named fields**. With `sealed: true` and all-concrete fields it now lowers to an unboxed struct
(the sealed-records work). There is no representation for "an object whose key set is not known at
compile time". The spec (§5) only has record object types and `Json`; there is no `Record<K,V>`,
index signature, or `Map`.

Concretely, in the RAPTOR reference (TypeScript, fully typed) these are all `Record<…>`:

```ts
type ConnectionIndex = Record<StopID, Record<number, Connection>>;  // kConnections
type ArrivalsByNumChanges = Record<number, Arrivals>;               // kArrivals
type RouteStopIndex = Record<RouteID, Record<StopID, number>>;      // routeStopIndex
type RoutesIndexedByStop = Record<StopID, RouteID[]>;               // routesAtStop
type Arrivals = Record<StopID, Time>;                               // bestArrivals
```

In the Lin port every one of these is `Json`. They are also the **hot, large** structures (keyed by
~16k routeIds / ~3k stops), so they are simultaneously the typing gap AND the O(n²) performance wall.

## Why this matters (three payoffs)

1. **Fidelity.** The RAPTOR port can't be a faithful, typed translation of the reference until the
   dictionary types exist. ~60% of its `Json` (the fixed records — StopTime/Trip/Transfer/Service/
   Journey + the Leg/Connection unions) *could* be typed today; the other ~40% (the maps above) cannot.
2. **Performance.** A typed map carries a known value type `T`, so the runtime can (a) use a hashed
   representation for O(1) lookup and (b) store/return `T` unboxed instead of boxing every value as
   `Json`. This subsumes the separate hashed-`Json`-object proposal
   (`docs/proposals/hashed-json-object.md`, #4b) — see "Relationship" below.
3. **Stdlib tightening.** A whole cluster of `std/object` signatures is lax *because the return type
   isn't expressible*, not by choice:
   ```
   keys   = (obj: Json): String[]          values = (obj: Json): Json
   entries = (obj: Json): Json             fromEntries = (pairs: Json): Json
   merge / pick / omit / mapValues         → all : Json
   ```
   With a map type these become properly generic, e.g.
   ```
   export val fromEntries = <T>(pairs: [String, T][]): { String: T }
   export val values      = <V>(m: { String: V }): V[]
   export val mapValues   = <V, W>(m: { String: V }, f: (V) => W): { String: W }
   ```
   and the value type flows through instead of collapsing to `Json`.

## The design to build — index-signature object type `{ String: T }`

A new object-type form where the key is a type (`String`) rather than literal field names, meaning
"any number of string keys, each mapping to `T`". It reads naturally, composes with the existing
object syntax, and the **value-level surface is unchanged** — `{}` literals, `obj[k]`, `obj[k] = v`,
`keys(obj)` all work exactly as today; only the *type* `{ String: T }` is new. This is the chosen
approach (smaller surface change than a nominal `Map`, tightens the existing discoverable `{}` type and
the `std/object` stdlib, and is exactly the String-keyed shape the RAPTOR maps need).

Spec / implementation points to settle while building:

- **Type grammar (§5).** Add `{ String: ValueType }` alongside the fixed-field `{ "f": T, … }` form.
  Key type is **`String` only for v1** — it matches JS objects and the current runtime (object keys are
  `LinString`), and it's all RAPTOR needs (`kArrivals`' numeric round keys are already stringified —
  see `scanResults.lin`). Leave the grammar open enough that an `Int`-keyed form could be added later,
  but don't build it now.
- **Checker.** `obj[k]` on `{ String: T }` yields `T | Null` (missing key → `Null`, consistent with the
  §6.1 safe-bracket-access rule); `obj[k] = v` requires `v : T`; an empty `{}` literal infers
  `{ String: T }` from context (assignment target / return type) or stays a fixed record otherwise.
  `keys(obj) : String[]`, `values(obj) : T[]`. Decide `is`/`has` against an index-signature type
  (either validate "object whose values are all `T`", or disallow it like generic application — `is
  Result<…>` is already rejected in §11; disallowing is the cheaper v1).
- **Record vs map.** A value is either a fixed record or an index-signature map, not both. A map type is
  its own thing; converting a `Json` into one needs `fromJson`/narrowing exactly like §19 (don't make
  `Json → { String: T }` an implicit coercion — keep parity with the §6.3 `Json`-conversion rule).
- **Runtime / codegen — the performance point.** Because the value type `T` is known, the backing
  representation should be **hashed (O(1) average lookup)** and should store/return `T` **unboxed**
  rather than boxing every value as `Json`. Reusing `LinObject` is possible but inherits the inline
  `MakeObject` ABI constraint (`codegen/mod.rs` GEPs at `entries@16`, 24-byte stride — see
  `hashed-json-object.md`); a distinct backing buffer for index-signature objects sidesteps that. Either
  way, the O(1) + unboxing is the whole performance payoff — a typed map that's still an O(n) assoc-list
  of boxed values would tighten types but not move the RAPTOR `PREP` number.

### Rejected alternative — a nominal `Map<K, V>` container

A first-class hashed map distinct from `{}` objects (`Map<K, V>` type, `lin_map_*` runtime, a `std/map`
module). More powerful (allows non-String keys, cleanly separates dictionaries from records) but a
larger surface (new literal/constructor syntax, `for`/destructuring/equality interactions) and it leaves
a **discoverability footgun** — users reach for `{}` first and only find `Map` after hitting the O(n²)
wall (the same trap #4a had with `sort` vs a missing `sortStable`). Not chosen. If non-String-keyed
maps are ever needed, this can be revisited as an addition; it is not a prerequisite.

## Relationship to the hashed-`Json`-object proposal (#4b)

`docs/proposals/hashed-json-object.md` proposes a lazy hash side-index on `Json` objects to fix O(n)
lookup *without* a type-system change. This proposal is the **type-system** route to the same
performance win, plus fidelity and stdlib benefits. They are not independent:

- If a typed map type lands with a hashed backing representation, it largely **obviates** the need to
  retrofit a hash index onto generic `Json` objects — code that needs dictionaries uses the map type
  and gets O(1) by construction, while `Json`/`{}` record literals keep their cheap small-object
  assoc-list layout (which is optimal for the handful-of-fields case).
- Conversely, if only the `Json` hash-index lands, the typing gap and the stdlib laxness remain.
- **The implementing agent should read `hashed-json-object.md` and decide whether to supersede it.**
  Doing the typed map first is arguably the better order: it fixes performance *and* types *and* the
  stdlib in one coherent feature, rather than papering over the dynamic type.

The codegen ABI constraint documented in `hashed-json-object.md` (the inline `MakeObject` fast path in
`codegen/mod.rs` GEPs `LinObject` at hardcoded offsets — `entries@16`, 24-byte stride) still applies if
a map reuses the `LinObject` layout; a distinct `Map` container (Option B) sidesteps it entirely.

## What "done" looks like

1. The chosen type form parses, type-checks, and lowers; `obj[k]`/`obj[k]=v`/`keys` work with proper
   value typing and **O(1) average lookup** (hashed backing).
2. The `std/object` cluster above is re-typed generically where the new type allows; `std/object`
   tests still pass and the value type now flows through (a `mapValues` over `{String: Int32}` yields a
   typed result, not `Json`).
3. A microbenchmark shows insert/lookup of N distinct keys is ~O(n), not O(n²) (mirror the bench in the
   #4b brief).
4. Faithfulness check (optional but the real motivator): the RAPTOR Lin port's dictionary structures
   (`kConnections`, `kArrivals`, `routeStopIndex`, `routesAtStop`, `bestArrivals`) can be re-typed to
   the new map type, the fixed records (`StopTime`/`Trip`/`Transfer`/`Service`/`Journey`) to named
   sealed records, and the union legs/connections to `match … is`. Then re-run
   `benchmarks/compare/raptor/lin/bench.lin` — the `PREP` phase (the `createRaptor` index build,
   currently ~144s and dominated by O(n) `Json` lookups) should collapse, and the cross-language
   correctness gate must stay exactly `DIGEST group=26203913 range=773022892 journeys=139`. This is the
   end-to-end proof that the type was the missing piece, and it's also when sealed-records finally has
   something to bite on (today RAPTOR is all `Json`, so the sealed-records optimization is a no-op on it).

## Process

Spec/ADR-level change (the decision — Option A, index-signature `{ String: T }` — is already made;
write the §5 grammar addition + an ADR as part of the work). Work in a worktree off master, with
checker + codegen + runtime + stdlib changes, the microbenchmark, and (ideally) the RAPTOR re-typing as
the macro validation. Don't merge without review.

## Already resolved (don't redo)

- Unions and generics already exist (`T | U`, `<T>`, `match … is`/`has`) — discriminating a
  `Connection`/`Leg` union is fully supported today; only the *map* types are missing.
- Sealed/unboxed named records already exist (the sealed-records work) — the fixed RAPTOR records can be
  typed now; they just haven't been because they flow into the `Json` maps.
- `std/array.sort` is a stable merge sort (#4a); dynamic `Json + null` faults cleanly (#5).
