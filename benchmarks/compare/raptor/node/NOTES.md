# RAPTOR — Node.js golden-reference port

Plain-JS (ES modules) port of planarnetwork/raptor. This is the GOLDEN REFERENCE the other
language ports are checked against, so it stays as close to the TypeScript source as possible:
types stripped, logic identical.

## Test command

```
cd benchmarks/compare/raptor/node
node --test "test/*.test.js"
# or: npm test
```

Zero dependencies. Requires Node with `node:test` (Node 18+). Exits nonzero on any failure.

## Passing test count

**48 tests, all passing** (0 fail, 9 suites). Breakdown mirrors the reference `it` blocks 1:1:

- Service: 6
- TimeParser: 1
- QueueFactory: 2
- DepartAfterQuery: 25
- GroupStationDepartAfterQuery: 2
- RangeQuery: 2
- MultipleCriteriaFilter: 3
- GraphResults: 4
- StringResults: 3

## What was ported

All modules with a unit test, per the contract:

- `src/gtfs/Service.js`, `src/gtfs/TimeParser.js`
- `src/query/DateUtil.js`, `DepartAfterQuery.js`, `GroupStationDepartAfterQuery.js`, `RangeQuery.js`
- `src/raptor/QueueFactory.js`, `RouteScanner.js`, `ScanResults.js`, `ScanResultsFactory.js`,
  `RaptorAlgorithm.js`, `RaptorAlgorithmFactory.js`
- `src/results/JourneyFactory.js`, `ResultsFactory.js`, `filter/MultipleCriteriaFilter.js`
- `src/transfer-pattern/results/GraphResults.js`, `StringResults.js`

GTFS data shapes (`GTFS.ts`) are plain JS objects, so no separate module is needed.

## GTFS loader + CLI runner (golden reference)

`src/gtfs/GTFSLoader.js` reads the extracted CSV feed (plain split-on-comma, no CSV lib)
and returns `{ trips, transfers, interchange }` per `LOADER_CONTRACT.md`. `run.js` builds
the raptor via `RaptorAlgorithmFactory.create(trips, transfers, interchange)` (NO date
pre-filter) and runs `DepartAfterQuery(raptor, new JourneyFactory()).plan(...)`.

### Run command

```
cd benchmarks/compare/raptor/node
node run.js [dataDir] [origin] [destination] [YYYY-MM-DD] [HH:MM]
# defaults: dataDir=../data origin=TBW destination=NRW date=2025-09-02 (a Tuesday) time=08:00
```

stdout is the pure diffable result (JOURNEY blocks + final RESULT line); timing
(`load=...ms plan=...ms`) goes to STDERR only.

### Observed reference output (`node run.js`, defaults TBW->NRW 2025-09-02 08:00)

```
JOURNEY dep=08:10:00 arr=11:18:00 legs=3
  TBW 08:10:00 -> LBG 08:53:00
  TRANSFER LBG -> SRA (1620s)
  SRA 09:37:00 -> NRW 11:18:00
RESULT dep=29400 arr=40680 legs=3 count=1
```

This `RESULT dep=29400 arr=40680 legs=3 count=1` line is the cross-language correctness
gate the other four ports must match. **0 trips were dropped** for missing calendars (no
stderr drop message). Typical timing: load ≈ 1.7s, plan ≈ 0.2s.

## What was skipped (and why)

Per the contract these have no unit tests / are Node-only I/O:
`transfer-patterns.ts`, `transfer-pattern-worker.ts`, `TransferPatternRepository`,
`TransferPatternQuery`, `index.ts`, `integration.ts`, `performance.ts`. No perf/benchmark
entry point was added — the lead wires that up after correctness lands.

## Test harness

- `test/util.js` — faithful port of `test/unit/util.ts`: `t/st/tf/j/setDefaultTrip`, plus a
  `deepEqual` that replaces vitest `toEqual`.
- `test/expect.js` — a tiny vitest-like `expect(x).toBe(...)/.toEqual(...)` over `node:assert`
  so the spec files read verbatim like the originals.
- The `npm test` script globs `test/*.test.js` only, so `util.js`/`expect.js` (no tests) are not
  picked up by the runner.

## Semantic decisions / trap handling

- **Insertion order**: plain JS objects/`Object.keys`/`for..in` preserve string-key insertion
  order for the non-numeric stop IDs used here — relied on directly, no extra structure needed.
- **Numeric-ascending round keys**: `JourneyFactory.getResults` iterates
  `Object.keys(kConnections[destination])`; those keys are integer-like (`1`,`2`,...) so JS
  yields them numerically ascending — matches the reference for free.
- **`getRouteId`**: `stop + (pickUp?1:0) + (dropOff?1:0)` joined with `,` (default `Array.join`
  separator), `"overtakes"` suffix on a later-arriving trip sharing the routeId. Ported verbatim.
- **Stable sorts**: JS `Array.sort` is stable (ES2019+); both the trip sort in
  `RaptorAlgorithmFactory` and `MultipleCriteriaFilter.sort` rely on that.
- **`Number.MAX_SAFE_INTEGER`** (= 9007199254740991) used exactly as the arrival sentinel and
  `Transfer.endTime` default.
- **UTC dates / multi-day**: `getDateNumber` slices `toISOString()` (UTC); multi-day search uses
  real `Date.setDate(getDate()+1)` arithmetic. `startDate.getDay()` is the host-local DOW, same
  as the reference. The test dates (`2018-10-16` etc.) are parsed as UTC midnight; this passes in
  the local TZ used (verified green). If a port is ever run in a TZ where the local DOW of a UTC
  midnight date shifts, the contract advises computing DOW from UTC — noted but not needed here
  since the reference itself uses `getDay()`.
- **`Service.runsOn`**: replicates the `dates[date]` truthy short-circuit vs the
  `!Object.hasOwn(dates, date)` clause exactly (a present `false` excludes).
- **`setDefaultTrip`-aware equality**: `setDefaultTrip` overwrites every timetable leg's `trip`
  with the shared `defaultTrip`; `j()` already uses that same `defaultTrip`, so `deepEqual` is a
  plain structural compare (trip identity is normalised on both sides). `deepEqual` also handles
  `Set` (order-independent) for the StringResults specs and recurses the acyclic `parent` chains
  for the GraphResults specs.

## Discrepancies

None. Every reference test passes against the faithfully-ported algorithm; no test was altered to
make it pass.
