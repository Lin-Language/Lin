# RAPTOR — Lin port

Port of planarnetwork/raptor (TypeScript) to Lin. Mirrors the reference journey
planner + transfer-pattern subsystem.

## Test command

From the repo root, with the workspace built (`cargo build --workspace`):

```bash
target/debug/lin test benchmarks/compare/raptor/lin/
```

## Result

**9 test files, 48 cases, all passing** — one-to-one with the reference `it()` blocks:

| File | Cases |
|------|-------|
| service.test.lin | 6 |
| timeParser.test.lin | 1 |
| queueFactory.test.lin | 2 |
| departAfterQuery.test.lin | 25 |
| groupStationDepartAfterQuery.test.lin | 2 |
| rangeQuery.test.lin | 2 |
| filter.test.lin | 3 |
| graphResults.test.lin | 4 |
| stringResults.test.lin | 3 |

## Modules

`types.lin` (shared named record/union types), `service.lin`, `timeParser.lin`,
`dateUtil.lin`, `queueFactory.lin`, `routeScanner.lin`, `scanResults.lin`,
`raptor.lin` (RaptorAlgorithm + factory), `journeyFactory.lin`, `filter.lin`
(MultipleCriteriaFilter), `query.lin` (GroupStation/DepartAfter/Range),
`graphResults.lin`, `stringResults.lin`, `roundKeys.lin` (shared numeric
round-key sort), plus `testutil.lin` (t/st/tf/j/setDefaultTrip + trip-ignoring
deep journey equality).

## Typed records + maps (what's typed, what stays `Json`, and why)

The fixed-shape data is expressed with **named record + union types**
(`types.lin`), and the **createRaptor index dictionaries are now typed
`{ String: T }` index-signature maps** (ADR-055 / spec §5.1.1) — a hashed
**O(1)** container, in place of the `Json` association-list object (O(n) per
lookup) the port used before this type existed.

**Types introduced (`types.lin`):** `Date`, `StopTime`, `Service`, `Trip`,
`Transfer`, `TimetableLeg`, `Leg = Transfer | TimetableLeg`, `Journey`, and
`RaptorIndex` (the createRaptor index struct, with typed-map fields).

### Map re-typing pass (PERFORMANCE lever)

Typed → `{ String: T }`, threaded through `RaptorIndex`
(`raptor.lin`/`query.lin`/`queueFactory.lin`/`routeScanner.lin`/`bench.lin`):

| Structure | Type | Built/read in |
|---|---|---|
| `routeStopIndex` | `{ String: { String: Int32 } }` | createRaptor + scan (the ~16k-key hot index) |
| `routePath` | `{ String: String[] }` | createRaptor + scan |
| `routesAtStop` | `{ String: String[] }` | createRaptor + getQueue |
| `tripsByRoute` | `{ String: Json[] }` (trip values stay `Json`) | createRaptor + routeScanner |
| `stops` | `String[]` | createRaptor |

### Scan-state map re-typing pass (the GROUP/RANGE-phase attempt)

A second pass typed the **ScanResults arrival-time maps** (`scanResults.lin`, threaded as the
named `ScanResults` record through `raptor.lin`/`query.lin`):

| Structure | Type | Built/read in |
|---|---|---|
| `bestArrivals` | `{ String: Int64 }` | createScanResults + setTrip/setTransfer + bestArrival |
| `kArrivals` | `{ String: { String: Int64 } }` | createScanResults + addRound + setTrip/setTransfer + previousArrival/getMarkedStops |

Arrival times are **Int64** (the MAX sentinel `9007199254740991` needs it). `previousArrival`/
`bestArrival` read the typed slot (so `m[k]` is `Int64 | Null`) but **widen to `Json`** so the
arithmetic/comparison consumers in `raptor.lin`'s inner scan loop keep their existing `!= null`
guards + Json numeric ops unchanged. `getFoundStations` (`query.lin`) must keep `bestArrivals`
typed `{ String: Int64 }` (NOT widened to `Json`): a `Json m[k]` index uses the object path and
misreads the `TAG_MAP` rep — a typed-map value passed to a `Json` param round-trips fine, but
indexing it as `Json` does not tag-dispatch `TAG_MAP` (a cross-rep boundary gap; it null-deref'd
in `lin_unbox_int32` until the param was re-typed).

**Measured payoff: NEUTRAL within heavy noise.** Unlike the PREP/`RaptorIndex` win (~5.6×,
because those indexes were ~16k-key routeId maps scanned per-trip over 240k trips), the
scan-state maps are ~3000-key stopId maps hit in the *query* phase, and the rounds map is tiny
(a handful of keys, so its association-list scan was already cheap). Interleaved GROUP runs
(controlling for machine drift): master ≈ 86–105 s, typed ≈ 87–88 s — the variant difference is
inside the per-variant spread, so there is no reliable speedup or regression signal. The digest
(`26203913`) and journey count (`39`) are byte-identical across all runs, so this is a
**fidelity** win (the arrival maps now carry their real `{ String: Int64 }` type) with no
behaviour change and no measurable cost. (This bench is noise-dominated on this host — judge by
interleaved medians, not single runs.)

### kConnections — NOW TYPED `{ String: { String: Conn } }` (the last `Json` dictionary)

`kConnections` is now typed `{ String: { String: Conn } }` where
`Conn = [Json, Int32, Int32] | Transfer` (the trip element of the tuple stays `Json` because
composite `Json` does not flow into the named `Trip` record, spec §5.1.1 — `[Trip, Int32, Int32]`
was NOT used; `[Json, Int32, Int32]` is the head form). This was the last `Json` dictionary in the
port. Behaviour is byte-identical: `run.lin`'s TBW→NRW gate is unchanged
(`RESULT dep=29400 arr=40680 legs=3 count=1`), the unit suite is 9/9, and the `bench.lin` GROUP
gate is unchanged (`digest=26203913 journeys=39`). This is a **fidelity** win — the query-phase
scan-state maps were measured perf-neutral (see the GROUP-phase note above), so no speedup is
expected or claimed.

**Why it was blocked before, and what unblocked it.** kConnections was previously `Json` because a
typed nested union-valued map triggered a use-after-free in two consumers: the multi-day stitching
(`query.lin`) and the transfer-pattern walks (`graphResults.lin`/`stringResults.lin`). The original
trigger — a `match`-narrowed connection read off the typed nested map projected into a shared array
— is the **projection-aliasing UAF that is now fixed on master** (`val x = obj[k]` materializes a
stable owned box). With that fix, the scan build (`setTrip`/`setTransfer`), the single-pass query
path, `graphResults`, and `stringResults` all type and run cleanly.

**One residual codegen bug surfaced (worked around at the Lin level, no `crates/` change):** a
nested typed map (`{ String: { String: Conn } }`) passed through an **indirect `Function`-value
call** loses its inner-map entries on the callee side — `keys(m[k])` reads empty. Minimal repro:

```lin
// fn: Function holds completeJourneys; calling fn(prev[idx], journeys) where prev[idx] is a
// { String: { String: Conn } } makes the callee's m[origin] read an EMPTY inner map (entries
// lost), so the multi-day fold produced 0 journeys. A DIRECT call completeJourneys(prev[idx], …)
// keeps the inner map intact.
```

`query.lin`'s `reduceReversed` previously folded `completeJourneys` via a `fn: Function` parameter.
The fold is always `completeJourneys`, so the indirection was gratuitous; it now calls
`completeJourneys` **directly**, which keeps the nested map intact and is faithful to the reference
(a plain `reduce`). This is the only Lin-side change driven by the residual bug; it is worth a
separate language fix (the `Function`-typed-call argument-coercion path does not preserve the
precise nested-map type, so the inner container is not materialized on the callee side).

**Consumers converted from boolean `isTransfer`/`isTimetableLeg` guards to `match … is Transfer`:**
`journeyFactory.lin` (`buildLegs`), `graphResults.lin` (`walk`), `stringResults.lin` (`walkPath`),
`run.lin` (the leg print loop). Each now does a 3-arm `match connection is Null / is Transfer /
else` (the tuple). The standalone `isTransfer` helpers were removed; `journeyFactory`'s
`isTimetableLeg` remains for `depAt`/`arrAt`, which inspect `Json` legs (not kConnections values).

**Kept `Json` (with reasons):**

- **`transfers`, `interchange`** (the createRaptor inputs / `RaptorIndex` fields): they
  *originate* from the loader as `Json` maps and there is no implicit `Json → { String: T }`
  coercion (spec §5.1.1); they are also SMALL per-origin maps (≤ ~3079 keys), so converting
  them would cost a pass for no measurable payoff.
- **`getQueue`'s return** (`routeId → stopId`): rebuilt every round, small/short-lived (not
  a hotspot). It also has to compare structurally / `toString` against an object literal in
  the unit test — and **a `{ String: T }` map currently lacks cross-rep structural equality
  and `toString` in the runtime** (`lin_tagged_eq` / `lin_to_string` handle `TAG_OBJECT` but
  not `TAG_MAP`). So it stays a `Json` object. (REPORTED BLOCKER — see below.)
- The loader's intermediate maps and `Service.days`/`Service.dates`: unchanged `Json`.

### Narrowing idiom usage (the verified `match … is T` form)

A typed-map scalar read `m[k]` is `T | Null`, and a `{ String: T }` value type is **not
spellable as an `is`-pattern**, so `if x == null` / `is Null` cannot refine it. The ONLY
narrowing form is the **positive `is T` match arm**. This port uses it via small helpers and
inline matches:

- `intOr(x: Int32 | Null, d): Int32` — `raptor.lin` (interchange/routeStopIndex reads feeding
  arithmetic) and `queueFactory.lin` (`isStopBefore`).
- An inline `match queue[routeId] is String =>` in `scanRoutes` to narrow the boarding stop
  key before the `routeStopIndex[routeId][stopP]` read.
- `pathOr(x: String[] | Null): Json` — note an ARRAY value type also isn't `is`-narrowable,
  and `if x == null then [] else x` does NOT refine an `T[] | Null` to `T[]` either; the array
  is handed on as `Json` (the O(1) hashed lookup already happened on the typed-map read).

### Codegen support added for nested typed maps

Nested typed maps (`{ String: { String: T } }`) were the headline structure but exposed two
codegen gaps that had to be fixed for the maps to round-trip correctly (the maps simply lost
their entries before the fix). Both are general `{ String: T }` correctness fixes, not
RAPTOR-specific:

1. `unbox_tagged_val_to_type` had no `Type::Map` arm — a `m[k]` whose value type is itself a
   map leaked the `TAG_MAP` box through instead of unboxing to the raw `LinMap*`, so a nested
   store/read operated on the box, not the shared inner container
   (`crates/lin-codegen/src/codegen/boxing.rs`).
2. The Union/`T | Null` index read AND string-key write paths only dispatched `TAG_OBJECT`. A
   nested map's inner index (`outer[a][b]`, where `outer[a]` is `{ String: T } | Null`, not
   `is`-narrowable) runs through that union path; it now tag-dispatches `TAG_MAP → lin_map_get`
   / `emit_map_set` alongside the object path (same RETAIN ownership contract)
   (`crates/lin-codegen/src/codegen/data.rs`, new `emit_obj_or_map_set`).

### Reported blocker (a missing runtime capability)

`{ String: T }` maps lack **structural equality and `toString`** at the `TAG_MAP` tag
(`lin_tagged_eq` / `lin_to_string` only handle `TAG_OBJECT`/arrays/scalars). Until that lands,
a typed map cannot be the value compared by a unit test's `toBe(...)` nor interpolated for
display — which is why `getQueue` returns a `Json` object rather than the `{ String: String }`
its keys would suggest.

**Actively threaded through code:**

- `Date` — `parseDate(): Date`, `getDateNumber`/`dayOfWeek`/`addDay` take/return
  `Date` (`dateUtil.lin`); the live query path is `date: Date`
  (`query.lin`: `planDepartAfter`/`planGroup`/`planRange`/`getJourneys`/`searchDay`).
- `Service` — `makeService(): Service`, `runsOn(service: Service, …)`
  (`service.lin`); the `days`/`dates` fields stay `Json` (dynamic-key indexes).
- `StopTime` + `Transfer` — typed leaf constructors `makeStopTime`/`makeTransfer`
  (`gtfsLoader.lin`) build fully-typed sealed records from the CSV row scalars.

**Typed `{ String: T }` (the map re-typing pass — see the table above):**
`routeStopIndex`, `routePath`, `routesAtStop`, `tripsByRoute` (via `RaptorIndex`).

**Typed `{ String: T }` scan-state (the GROUP/RANGE pass — see the table above):**
`bestArrivals` (`{ String: Int64 }`), `kArrivals` (`{ String: { String: Int64 } }`), and now
`kConnections` (`{ String: { String: Conn } }` — the last `Json` dictionary, see the
"kConnections — NOW TYPED" section), threaded via the named `ScanResults` record
(`scanResults.lin`/`raptor.lin`/`query.lin`/`journeyFactory.lin`/`graphResults.lin`/
`stringResults.lin`).

**Left `Json` at the map / union-narrowing boundaries:**

- The createRaptor INPUT maps `transfers`/`interchange`; `getQueue`'s return; and the loader's
  intermediate maps (`datesList`, `servicesSorted`, …). See the "Kept `Json`" list above for the
  per-structure reason. (`kConnections` is NO LONGER here — it is now typed.)
- `Trip` values **as consumed**: the tuple connection's head element stays `Json` (composite
  `Json` does not flow into the named `Trip` record). `Journey`/`Leg`/`TimetableLeg` values built
  by `journeyFactory` (the leg objects) and read by `run.lin`/`filter.lin` stay `Json`: they are
  produced as `Json` object literals and consumed via `isTimetableLeg` field-tests, so they are
  not threaded as the named records. The kConnections CONNECTION values, however, now narrow via
  `match … is Transfer` (see the consumer list). The `Leg`/`TimetableLeg`/`Journey` types remain
  in `types.lin` as the reference shapes for when those built-leg values can also be typed.

**Type-system friction found (useful for the map-type / narrowing work):**

1. A bare `Json`-typed binding does **not** flow into a named-record parameter
   (`getDateNumber(jsonVal)` is rejected `?T … expected Date`). Only a `Json`
   *literal* or a `Json` *scalar-index* (`row[i]`) coerces on the spot into a
   concrete `String`/`Int32` field/param — composite `Json` (`stopTimes`,
   `service`) does not, which is exactly why trips/journeys built from `Json`-map
   sources can't be typed today.
2. The `if`-expression form does **not** narrow a `T | Null` named-record union to
   `T` — neither `if x != null`, `if x is Null then … else`, nor `if x is T`
   refines it. Only a `match … is Null / is T` narrows. `raptor.lin`'s optional
   date pre-filter therefore uses a `match` (not the original `if date != null`)
   to get `Date | Null` down to `Date`; the branch is dead in practice (every
   caller passes `null`) but it now type-checks cleanly.

**Sealed-record / unboxed status:** the concrete records (`Date`, `StopTime`,
`Transfer`) are sealed-eligible at the type level, but **codegen still ignores the
`sealed` marker** — `crates/lin-codegen/src/codegen/types.rs` (Stage 0.5) lowers
every object, sealed or not, to the boxed string-keyed `LinObject`. So typing
these records is currently a **fidelity** win (and a latent perf win once the
sealed-record codegen stage lands), not yet a measured speedup; runtime behaviour
is byte-for-byte identical, which is why the gate is unchanged.

**Gate after typing:** unit tests still 9/9 pass; `run.lin` still builds and
prints `RESULT dep=29400 arr=40680 legs=3 count=1` on the full feed; the
`bench.lin` GROUP digest is unchanged (`26203913`, journeys `39`) — across BOTH the
`RaptorIndex` PREP pass and the `ScanResults` scan-state pass.

**Measured PREP-phase speedup (the point of the index map re-typing pass):** the
createRaptor index build dropped from **~144 s** (when the routeId-keyed indexes were
O(n) `Json` association lists) to **~25.7 s** with the O(1) hashed `{ String: T }` maps
— a **~5.6×** speedup on the headline PREP phase. (LOAD ≈ 30 s and the GROUP/RANGE query
phases are unaffected by the index typing.)

**Measured GROUP-phase result of the scan-state pass: NEUTRAL within noise.** Typing
`bestArrivals`/`kArrivals` (the ScanResults pass) did NOT reproduce the PREP win — the
scan-state maps are smaller (~3000-key stopId maps) and hit in the query phase, so the
O(n)→O(1) change is in the noise floor. Interleaved GROUP runs: master ≈ 86–105 s, typed
≈ 87–88 s (variant difference < per-variant spread; this host is noise-dominated). Behaviour
is byte-identical — a fidelity win at no measurable cost. `kConnections` was the last `Json`
dictionary and is NOW also typed `{ String: { String: Conn } }` (the projection-aliasing UAF that
blocked it is fixed on master; see "kConnections — NOW TYPED"). Same fidelity-not-speed character.

## Design

Lin has no classes. The TS classes become Lin modules of exported functions over
JSON-shaped records, with state threaded explicitly (the recursive-descent /
cursor idiom from `examples/calc/`). Mutable per-scan state (`ScanResults`,
`RouteScanner` scan position) is held in `var`-backed record fields created by a
factory function and captured by the closures that mutate them.

## GTFS loader + CLI runner (`gtfsLoader.lin`, `run.lin`)

Added per `LOADER_CONTRACT.md`. The loader reads the plain-CSV feed
(`benchmarks/compare/raptor/data/`, ~2.37M `stop_times` rows) and returns
`{ trips, transfers, interchange, dropped }`. `run.lin` builds RAPTOR via
`createRaptor(trips, transfers, interchange, null)` (no date pre-filter), runs
`planDepartAfter`, and prints the contract format.

### Build + run

```bash
cd /home/linus/Work/linusnorton/lin-lang
target/debug/lin build benchmarks/compare/raptor/lin/run.lin -o /tmp/raptor_lin
/tmp/raptor_lin                 # default query TBW -> NRW 2025-09-02 08:00, data dir hardcoded
```

`run.lin` hardcodes the absolute `data/` dir and the default query (TBW->NRW
2025-09-02 08:00, maxSearchDays=3); CLI arg parsing is not wired. A non-zero dropped
count is reported on stderr via `printErr`; stdout stays a pure diffable result.

### Gate status: PASSES end-to-end on the full feed

Both pinned queries match the Node golden byte-for-byte:

```
TBW -> NRW 2025-09-02 08:00 → RESULT dep=29400 arr=40680 legs=3 count=1
LBG -> NRW 2025-09-02 08:00 → RESULT dep=29460 arr=37140 legs=2 count=1
```

End-to-end (load + createRaptor + plan) on the full 240009-trip feed: **~510s wall,
peak RSS ~1 GB**. (For comparison Node runs in ~2s — Lin is far slower here because
its `Json` objects are association lists with O(n) key lookup, see below — but it
COMPLETES and is correct.)

### The loader streams the feed (std/stream, not readFile)

`gtfsLoader.lin` reads every file with the **streaming API**
(`readStream(path).lines().for(...)`) via a shared `forEachRow` helper, NOT
`std/fs.readFile`. `readFile` + `split(text, "\n")` would hold the entire 93 MB
`stop_times.txt` text PLUS a 2.37M-element line array PLUS 2.37M split field-arrays
all live at once — gigabytes of transient heap the allocator then thrashes on.
Streaming pulls one line at a time: each line is split, folded into the durable
accumulator, and freed before the next is read, so peak transient heap is a single
line. Only the durable results (stopTimesGroups, serviceList, transfers) stay
resident.

The loader also avoids large `Json` object maps (which are O(n) per lookup — see
below): stop_times rows are contiguous by trip_id so they group by detecting changes;
trips↔stop_times share trip_id order so they positional-merge-join; service
resolution uses a sorted array + binary search instead of a per-trip object lookup.

### What made it feasible: a stable O(n log n) sort

The trip sort (240009 trips) needs a STABLE O(n log n) sort. An early version used a
hand-rolled insertion sort that **rebuilt the whole array on every insertion** (~n²/2 ≈
29 billion boxed copies → RSS past 80 GB, hours) — that, not the loader or
`createRaptor`, was the original blowup. This is now fixed at the language level:
`std/array.sort` was made a stable bottom-up merge sort, and the port simply calls it
(no local sort helper remains). See the shared `sortedRoundKeys` in `roundKeys.lin` and
the `sort(...)` calls in `raptor.lin`/`filter.lin`/`gtfsLoader.lin`/`run.lin`/`bench.lin`.

### Resolved: createRaptor's O(n) `Json` key lookup → O(1) typed maps

Lin's `Json` objects are association lists — `lin_object_get`/`lin_object_set`
(`crates/lin-runtime/src/object.rs`) linearly scan all entries. `createRaptor` keys its
indexes by `routeId` (~16k distinct), so when those indexes were `Json` the per-trip hot
path did O(16k) scans × 240k trips → the PREP phase took ~144 s.

This is now fixed by the **map re-typing pass** above: `routeStopIndex`/`routePath`/
`routesAtStop`/`tripsByRoute` are typed `{ String: T }` index-signature maps (ADR-055),
backed by a hashed O(1) container (`crates/lin-runtime/src/map.rs`). PREP dropped to
~25.7 s (~5.6×). The scan-state arrival maps (`bestArrivals`/`kArrivals`) were later typed too
(the ScanResults pass) but that was GROUP-neutral within noise — these are smaller stopId maps
in the query phase, not the build hotspot. (`kConnections` was later typed too — the last `Json`
dictionary — once the projection-aliasing UAF was fixed on master.) The remaining `Json` maps are
the small per-origin `transfers`/`interchange` and `getQueue`'s return — see the "Kept `Json`"
list for why each stays `Json`. Lin is still
slower than the hashed-map languages (Node/Go/Rust/Python ~1–2 s) on the full query
phase, but the index-build hotspot is no longer the dominant cost.

## Skipped (as per the porting contract — no unit tests / Node-only I/O)

`TransferPatternQuery` (no spec), `TransferPatternRepository`,
`transfer-patterns.ts`, integration/performance tests.

## Semantic traps honored

- **Insertion-ordered objects**: verified Lin objects preserve key insertion order,
  so `keys(obj)` iteration matches JS `Object.keys`. The per-destination round map
  keyed by integer `k` is iterated in numeric-ascending order regardless.
- `getRouteId`: comma-join of `stop + pickUp("1"/"0") + dropOff("1"/"0")`, with the
  `overtakes` suffix when an earlier same-signature trip arrives later.
- Sentinel `9007199254740991i64` (MAX_SAFE_INTEGER) for "infinity" arrivals and
  Transfer.endTime; `getFoundStations` uses `max(1, best - 86400)`.
- UTC date → YYYYMMDD integer, real calendar day-increment (month/year rollover),
  day-of-week Sun=0..Sat=6. Verified against the contract's pinned DOW values.
- `Service.runsOn`: distinguishes "date is a key in dates" (present-false = exclude)
  from absent, matching the reference's `hasOwn`-vs-truthy logic.
- Stable sorts for the trip sort (by `stopTimes[0].departureTime`) and the
  MultipleCriteriaFilter sort (departure asc, arrival desc tiebreak) + dominance.
- `setDefaultTrip`-aware journey equality: legs compare by stopTimes + origin +
  destination (timetable) or the transfer fields; trip identity is ignored.

## Lin language issues found during the port — all fixed

The port originally danced around several Lin bugs; each was fixed at the language /
stdlib level and the workaround removed. The code now reads as a direct port. Full
detail + repros are in `../LIN_ISSUES.md`; in brief:

1. **Closure-local `var` written in an `if` lost after the join** (#1) — the loader's
   trips merge-join and several accumulators used to need a `val`-conditional dance;
   they now use plain `var` reassignment.
2. **Top-level `var` mutated by an exported function in an imported module panicked
   codegen** (#2) — `testutil.lin`'s `t()` had to derive a content-based `tripId`; it
   now uses a plain `var tripCounter` exactly like the reference `util.ts`.
3. **`Int32 * Int64`-literal overflowed in Int32** (#3) — `bench.lin`'s digest no
   longer needs manual per-operand `Int64` widening.
4. **No stable stdlib sort** (#4a) — `std/array.sort` is now a stable merge sort; the
   hand-rolled `sortutil.lin` was deleted and three duplicated `sortedRoundKeys`
   insertion sorts collapsed into the shared `roundKeys.lin`.
5. **Self-write `obj[k] = obj[k]` aliasing fault** — `queueFactory.lin` now uses the
   clean `queue[r] = if before then existing else stop` form.
6. **Wrapped multi-line `if/else` inside parens was unparseable** (#7) — fixed in the
   parser; the whole `lin/` dir is now `lin fmt`-clean.

The one remaining honest characteristic (not a bug): Lin `Json` objects have O(n) key
lookup, so the query phase is far slower than the hashed-map languages — see the
"O(n) `Json` object key lookup" section above and `LIN_ISSUES.md` #4b.

## scanTransfers null guard (a faithful-semantics choice, not a workaround)

On the FULL feed a transfer/link destination may not lie on any route path, so its
`interchange`/`bestArrival` are `null`. JS turns `number + undefined` into `NaN` and
every comparison is false, so the transfer is silently skipped. `raptor.lin`'s
`scanTransfers` guards `ic != null && best != null` to reproduce that skip-on-missing
semantics (Lin now *faults* on `number + null` rather than miscompiling — LIN_ISSUES
#5 — so the explicit guard is the correct faithful port, not a bug dodge).
