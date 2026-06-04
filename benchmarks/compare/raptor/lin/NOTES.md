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

## Typed records (what's typed, what stays `Json`, and why)

The fixed-shape data is now expressed with **named record + union types**
(`types.lin`), mirroring the reference TypeScript types — the port is no longer
100% `Json`. The dynamic-key MAP structures must stay `Json` until the
accepted-but-unimplemented `{ String: T }` map type lands (see
`docs/proposals/typed-map-index-signature.md`).

**Types introduced (`types.lin`):** `Date`, `StopTime`, `Service`, `Trip`,
`Transfer`, `TimetableLeg`, `Leg = Transfer | TimetableLeg`, `Journey`.

**Actively threaded through code:**

- `Date` — `parseDate(): Date`, `getDateNumber`/`dayOfWeek`/`addDay` take/return
  `Date` (`dateUtil.lin`); the live query path is `date: Date`
  (`query.lin`: `planDepartAfter`/`planGroup`/`planRange`/`getJourneys`/`searchDay`).
- `Service` — `makeService(): Service`, `runsOn(service: Service, …)`
  (`service.lin`); the `days`/`dates` fields stay `Json` (dynamic-key indexes).
- `StopTime` + `Transfer` — typed leaf constructors `makeStopTime`/`makeTransfer`
  (`gtfsLoader.lin`) build fully-typed sealed records from the CSV row scalars.

**Left `Json` at the map / union-narrowing boundaries (the real signal for the
map-type work):**

- All the dictionaries: `kConnections`, `kArrivals`, `bestArrivals`,
  `routeStopIndex`, `routesAtStop`, `tripsByRoute`, `routePath`, and the loader's
  intermediate maps (`datesList`, `transfers`, `interchange`, `servicesSorted`, …).
- `Trip`/`Journey`/`Leg`/`TimetableLeg` values **as consumed**: every one of them
  is read out of the `kConnections` `Json` map (or a `Json[]` built from it) and
  inspected via a boolean field-test (`isTransfer`/`isTimetableLeg`) followed by
  field/index access — the calc-parser idiom. Lin does **not** narrow a union (or
  refine `Json`) across a plain boolean guard, so these consumers
  (`journeyFactory.lin`, `graphResults.lin`, `stringResults.lin`, `run.lin`,
  `filter.lin`) stay `Json`. The `Leg`/`TimetableLeg`/`Journey` types are kept in
  `types.lin` as the reference shapes for when the map type lets the maps — and
  therefore their typed values — be expressed.

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
prints `RESULT dep=29400 arr=40680 legs=3 count=1` on the full feed.

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

### Known characteristic: O(n) `Json` object key lookup

Lin's `Json` objects are association lists — `lin_object_get`/`lin_object_set`
(`crates/lin-runtime/src/object.rs`) linearly scan all entries; there is no hashed
container. `createRaptor` keys three indexes by `routeId` (~16k distinct), so the
per-trip hot path does O(16k) scans × 240k trips. This is why Lin's ~510s dwarfs the
~1-2s of the hashed-map languages (Node/Go/Rust/Python). It is a real language
characteristic worth a future `Map`/hashed-object type, but it is NOT a correctness
blocker — the port completes and matches the gate. The loader sidesteps it with the
sorted-array + binary-search approach above; the algorithm modules keep the
routeId-keyed objects because they are shared with the unit tests and re-grouping
would change observable output (`overtakes` detection, journey order/counts).

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
