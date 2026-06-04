# RAPTOR porting contract (shared across all 5 languages)

We are porting the **planarnetwork/raptor** TypeScript journey planner to Go, Rust,
Python, Node.js and Lin. The reference source is checked out at `/tmp/raptor-ref`
(src/ and test/). Read it. This file records the cross-language semantic traps —
get these wrong and the unit tests diverge silently.

Goal per language: a faithful port that **passes the same ~40 unit tests** as the
reference, PLUS a single timed benchmark entry point (added later by the lead).

## What to port (everything with a unit test)

Core journey planner:
- `gtfs/GTFS` — data types (StopTime, Trip, Transfer, Service shape, Calendar).
- `gtfs/Service` — `runsOn(date, dow)` calendar logic. (Service.spec.ts)
- `gtfs/TimeParser` — "HH:MM:SS" → seconds, cached. (TimeParser.spec.ts)
- `query/DateUtil` — `getDateNumber(Date)` → YYYYMMDD integer **in UTC**.
- `raptor/QueueFactory` — getQueue. (QueueFactory.spec.ts)
- `raptor/RouteScanner` (+Factory) — getTrip with backward scan position.
- `raptor/ScanResults` (+Factory) — per-round arrivals + connection index.
- `raptor/RaptorAlgorithm` (+Factory) — scan(), scanRoutes, scanTransfers.
- `query/GroupStationDepartAfterQuery` — multi-day stitching. (its spec)
- `query/DepartAfterQuery` — single origin/dest wrapper. (24 cases — the big one)
- `query/RangeQuery` — profile query. (RangeQuery.spec.ts)
- `results/JourneyFactory` — kConnections → Journey[]. (exercised by all query specs)
- `results/filter/MultipleCriteriaFilter` — sort + dominance filter. (its spec)

Transfer-pattern subsystem:
- `transfer-pattern/results/GraphResults` — DAG merge. (GraphResults.spec.ts)
- `transfer-pattern/results/StringResults` — pattern strings. (StringResults.spec.ts)

You may SKIP (no unit tests / Node-only I/O): `gtfs/GTFSLoader` (gtfs-stream, mysql),
`transfer-patterns.ts`, `transfer-pattern-worker.ts`, `TransferPatternRepository`,
`TransferPatternQuery` (no spec), `integration.ts`, `performance.ts`.

## THE SEMANTIC TRAPS (read carefully)

### 1. JS object key insertion-order iteration is load-bearing
`Object.keys(obj)` and `for..in` yield **string keys in insertion order** (integer-like
keys are a JS special case, but RAPTOR's stop IDs are non-numeric strings like "A",
"NRW", so they preserve insertion order). The algorithm's output ORDER depends on this:
- `ScanResults.getMarkedStops()` = insertion order of `kArrivals[k]`.
- `JourneyFactory.getResults` iterates `Object.keys(kConnections[destination])` — the
  round numbers `k`, which are integer-like → **JS sorts these numerically ascending**.
  So iterate rounds in ascending numeric order (1,2,3,...).
- `getJourneysFromConnections` order = order of `destinations` array, then per-dest the
  rounds. `MultipleCriteriaFilter` re-sorts, but several DepartAfterQuery tests run WITH
  NO FILTER and assert an exact array order — so the kConnections round order and the
  destination order must match JS.
→ **Use an insertion-ordered map** everywhere a JS object is iterated: Python `dict`
  (ordered ✓), Go must use a **slice of keys recording insertion order** alongside the
  map (Go map iteration is randomized — NEVER range a Go map for output), Rust use
  `indexmap::IndexMap` OR a Vec<(K,V)>; Lin objects preserve insertion order (verify).
  EXCEPTION: the per-destination round map keyed by integer `k` must iterate in
  **numeric ascending** order — sort the keys numerically, don't rely on insertion.

### 2. `getRouteId` — the route signature string
`trip.stopTimes.map(s => s.stop + (s.pickUp?1:0) + (s.dropOff?1:0)).join()`.
JS `Array.join()` with no arg uses **","** as separator. So routeId is e.g.
`"A10,B11,C01"` (stop + pickUp-bit + dropOff-bit, comma-joined). `pickUp`/`dropOff`
are booleans; `+ (bool?1:0)` appends "1" or "0" as a CHARACTER to the stop string.
Reproduce exactly: `stop + (pickUp?"1":"0") + (dropOff?"1":"0")`, joined by ",".
Overtaking suffix: append literal `"overtakes"` when an earlier trip on the same
routeId arrives later (see RaptorAlgorithmFactory.getRouteId).

### 3. Trip sort is by first departureTime, must be STABLE
`trips.sort((a,b) => a.stopTimes[0].departureTime - b.stopTimes[0].departureTime)`.
JS Array.sort is **stable** (guaranteed since ES2019). Go `sort.SliceStable`, Python
`sorted` (stable ✓), Rust `sort_by` (stable ✓), Lin `sort` — verify stability or the
overtaking/route-grouping tie order can differ. Same for MultipleCriteriaFilter.sort.

### 4. MultipleCriteriaFilter sort + filter
sort: by departureTime asc; tie-break arrivalTime **descending** (`b.arr - a.arr`).
filter: keep journey A unless some LATER journey B (j>i) satisfies ALL criteria.
Default criteria: `earliestArrival` (b.arr <= a.arr) AND `leastChanges`
(b.legs.length <= a.legs.length). Note it compares against journeys AFTER it in the
sorted array only. Stable sort matters for the "arbitrary when same" cases.

### 5. `Number.MAX_SAFE_INTEGER` = 9007199254740991 (2^53 - 1)
Used as the "infinity" arrival sentinel and as Transfer.endTime default. Use exactly
this constant everywhere (i64 is fine; do NOT use i32). `getFoundStations` does
`Math.max(1, bestArrivals[s] - 86400)`.

### 6. Dates are handled in UTC
`new Date("2018-10-16")` parses as **UTC midnight**. `getDateNumber` slices the
`toISOString()` (UTC) → 20181016. `date.getDay()` in the reference returns the **local**
day-of-week BUT the tests were written assuming the environment behaves such that the
chosen dates map to specific DOW; to be safe **compute DOW from the UTC date** (Zeller
or a known reference). 2018-10-16 is a Tuesday (dow=2). 2018-10-22 is Monday (dow=1).
2019-04-18 is Thursday (4). 2019-04-23 is Tuesday (2). 2018-12-31 is Monday (1).
getDay(): Sunday=0..Saturday=6. **Multi-day search increments the date by 1 day** and
recomputes dow — implement real calendar date arithmetic (handle month/year rollover),
because `getJourneys` loops up to maxSearchDays advancing the day. `getDateNumber` must
produce a comparable YYYYMMDD integer for `Service.runsOn`.

### 7. Service.runsOn exact boolean logic
```
dates[date] === true  OR
( date NOT a key in dates  AND startDate <= date <= endDate  AND days[dow] )
```
`dates` is the include/exclude index: a key present with value true = include,
present with false = exclude (so `dates[date]` truthy short-circuits include; a present
`false` makes the `!hasOwn` clause false → excluded). Replicate the `hasOwn` vs truthy
distinction precisely.

### 8. st() / t() test fixture semantics (test/unit/util.ts)
- `st(stop, arr, dep)`: `arrivalTime = arr ?? dep`, `departureTime = dep ?? arr`,
  `dropOff = arr !== null`, `pickUp = dep !== null`. So `st("A", null, 1000)` is a
  pickup-only origin (no dropOff); `st("C", 1100, null)` is dropoff-only.
- `t(...stopTimes)`: tripId = `trip${counter++}` (GLOBAL incrementing counter, reset
  per process — but tests don't assert tripId since setDefaultTrip overwrites it).
- `tf(o,d,dur)`: Transfer with startTime=0, endTime=MAX_SAFE_INTEGER.
- `j(...legs)`: builds a Journey; each leg is either a Transfer (has `origin`) or a
  StopTime[] → TimetableLeg {stopTimes, origin=first.stop, destination=last.stop, trip}.
- `setDefaultTrip(results)`: overwrites every timetable leg's `trip` with a fixed
  default object BEFORE comparison — so **journey equality ignores trip identity**.
  Your equality check must compare legs by stopTimes + origin + destination, NOT trip.

### 9. Journey equality semantics for tests
A Journey = {legs, departureTime, arrivalTime}. A timetable leg = {stopTimes[], origin,
destination} (trip ignored per #8). A transfer leg = {origin, destination, duration,
startTime, endTime}. The specs compare with `toEqual` (deep structural). Implement a
deep-equality that distinguishes the two leg kinds and ignores trip. departureTime /
arrivalTime are computed by JourneyFactory (see getDepartureTime/getArrivalTime: walk
legs accounting for transferDuration before the first / after the last timetable leg).

### 10. JourneyFactory.getJourneyLegs builds legs in reverse then reverses
Walks k connections from finalDestination back to k=1. Transfer → push transfer, move to
its origin. Timetable connection [trip,start,end] → leg stopTimes = trip.stopTimes
slice(start, end+1), origin = that slice[0].stop. Then `legs.reverse()`.

### 11. RouteScanner.getTrip backward scan + routeScanPosition memo
Per route, remembers the last index it scanned from (init = trips.length-1). Iterates
**backward** from there; breaks when `stopTime.departureTime < time` (unreachable);
records lastFound when service runs that day; updates routeScanPosition under the
documented condition (`!lastFound || lastFound === trip`). This memo is STATEFUL across
calls within one scan — preserve it. A fresh RouteScanner is created per `scan()` call
(per day), via RouteScannerFactory.

### 12. GraphResults / StringResults string details
- GraphResults.mergePath uses head/tail destructuring; nodes are {label, parent} with
  parent pointer; `isSame` walks parent chain comparing labels to path[i]. finalize()
  returns Record<label, TreeNode[]>. The spec compares node identity via structure
  (label + parent chain) — see GraphResults.spec.ts mergePath helper for the
  kConnections shape it feeds in: `kConnections[dest][i] = [{stopTimes:[{stop:origin}]}, 0, 1]`.
- StringResults: journeyKey = origin>destination ? dest+origin : origin+dest (string
  comparison `>`). pathString = tail joined by "," (reversed if origin>destination).
  finalize returns Record<key, Set<string>>. Replicate the Set + ordering. The spec
  feeds the same synthetic kConnections shape. `getPath` uses `unshift` (prepend).

## Test harness expectations

Each language's tests must be runnable by the lead with a single documented command
and must EXIT NONZERO on failure. Mirror the reference `describe/it` names so failures
are traceable. Match each language's local idiom:
- **Node**: vitest is heavy; instead write a zero-dependency `assert`-based runner
  (`node --test` with `node:test` + `node:assert`, OR a tiny custom harness). Reuse the
  reference's `t/st/tf/j/setDefaultTrip` helpers ported to plain JS. Tests in `*.test.js`.
- **Go**: standard `go test` with table-driven tests; one module under raptor/go/.
- **Rust**: `#[cfg(test)]` + `cargo test` in a standalone crate raptor/rust/ (NOT a
  workspace member — keep it out of the root Cargo.toml; use its own Cargo.toml).
- **Python**: stdlib `unittest` (pytest is NOT installed). `python3 -m unittest`.
- **Lin**: `*.test.lin` using `std/test` (expect/toBe/toEqual via toSatisfy + suite/run).
  Run via the freshly built `lin` binary: `<repo>/target/debug/lin test <dir>`. Lin has
  no class system — model the algorithm with closures over records (Json objects) or
  explicit state-threading, following examples/calc/ idioms. Insertion-ordered objects:
  verify Lin object key order matches insertion (test it early).

## Directory layout (create under benchmarks/compare/raptor/)
- `node/`   — package.json (type:module, no deps), src + *.test.js + runner
- `go/`     — go.mod, raptor.go (or split), raptor_test.go
- `rust/`   — Cargo.toml (standalone), src/lib.rs (+ modules), tests inline or tests/
- `python/` — raptor.py (or package), test_raptor.py
- `lin/`    — *.lin sources + *.test.lin

## Deliverable per agent
1. Full port of all listed modules.
2. Unit tests mirroring the reference, ALL PASSING.
3. A short NOTES.md in your language dir: exact test command, what you skipped & why,
   any semantic decision you had to make. Report the passing test count.
DO NOT add a perf/benchmark main yet — the lead wires that up after correctness lands.
DO NOT modify anything outside your `benchmarks/compare/raptor/<lang>/` directory.
