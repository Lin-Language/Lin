# RAPTOR — Go port notes

## Test command

```bash
cd benchmarks/compare/raptor/go
go test ./...
```

Exits nonzero on failure. Verbose subtest names: `go test -v ./...`.

## Passing count

**48 / 48** leaf subtests pass (9 top-level `Test*` functions), exactly mirroring
the reference's 48 `it()` blocks:

| File                        | Subtests |
|-----------------------------|----------|
| Service.spec                | 6        |
| TimeParser.spec             | 1        |
| QueueFactory.spec           | 2        |
| DepartAfterQuery.spec       | 25       |
| GroupStationDepartAfterQuery| 2        |
| RangeQuery.spec             | 2        |
| MultipleCriteriaFilter.spec | 3        |
| GraphResults.spec           | 4        |
| StringResults.spec          | 3        |

Verified stable across 20 runs with `go test -count=1 -shuffle=on ./...` (no
ordering flakiness — see "Map ordering" below).

## GTFS loader + CLI runner

A plain-CSV GTFS loader (`gtfsloader.go`, `raptor.LoadGTFS`) and a CLI runner
(`cmd/run/main.go`, `package main`) plan a real journey over the extracted feed
at `benchmarks/compare/raptor/data/`. The runner lives in a subdirectory so the
library `raptor` package stays test-clean.

### Build + run

```bash
cd benchmarks/compare/raptor/go
go run ./cmd/run                          # default: ../data TBW NRW 2025-09-02 08:00
go run ./cmd/run ../data TBW NRW 2025-09-02 08:00   # explicit
go build -o run ./cmd/run && ./run        # or build a binary first
```

Args: `[dataDir] [origin] [destination] [YYYY-MM-DD] [HH:MM]`. `dataDir`
defaults to `../data` (relative to `go/`). stdout is the contract format;
timing (`load=...ms plan=...ms`) goes to stderr only.

### Observed RESULT (pinned cross-language gate)

```
JOURNEY dep=08:10:00 arr=11:18:00 legs=3
  TBW 08:10:00 -> LBG 08:53:00
  TRANSFER LBG -> SRA (1620s)
  SRA 09:37:00 -> NRW 11:18:00
RESULT dep=29400 arr=40680 legs=3 count=1
```

Matches the Node golden reference byte-for-byte. Zero trips dropped (every
serviceId has a calendar row). Load ~0.9s, plan ~0.2s on this machine.

### Loader semantics (mirror GTFSLoader.ts)

- `bufio.Scanner` with a 16MB buffer; columns read by header name
  (`indexOfColumns`), so column order is incidental. stop_times grouped by
  trip_id into `map[string][]StopTime` preserving file order.
- `TimeParser`: `hh*3600+mm*60+ss`, no mod 86400 (times exceed 24h).
- `pickUp`/`dropOff` = `pickup_type/drop_off_type ∈ {"0",""}` (true); `"1"`/`"3"`
  false — matches the reference's "0 or undefined" rule (empty CSV field = undefined).
- `calendar_dates`: `dates[serviceId][+date] = (exception_type === "1")`.
- transfers: same-stop -> `interchange[from]`, else a `Transfer` with
  `startTime=0, endTime=MaxSafeInteger`; links.txt -> footpath `Transfer`s
  (date/day cols ignored).
- Trips whose serviceId has no calendar row are dropped, count to stderr.
- Runner builds raptor via `CreateRaptorAlgorithm(..., nil)` (NO date pre-filter)
  and runs `DepartAfterQuery.Plan`.

## Benchmark runner (cross-language gate)

`cmd/bench/main.go` (`package main`) reproduces `node/bench.js` exactly: same
24-entry GROUP query list, same 5 RANGE pairs, same `journeyDigest` formula
(`(dep%1e9*1000003 + arr%1e9*31 + legs) % 1000000007`, accumulated `(acc+c)%P`,
all `uint64`). It reuses the loader (`raptor.LoadGTFS`) and factory setup from
`cmd/run` (build raptor once via `CreateRaptorAlgorithm(..., nil)` — NO date
pre-filter, date passed through the query).

- GROUP: each query planned with `GroupStationDepartAfterQuery(r, jf, 3,
  [MultipleCriteriaFilter])` at date=2025-09-02, time=36000.
- RANGE: "next 20 departing after 28800" via `nextN` — repeatedly `Plan` with no
  filter, advance `time = min(departureTime of newResults)+1`, break on empty,
  until ≥20 collected, then sort (dep asc, arr asc) and take first 20.

### Build + run

```bash
cd benchmarks/compare/raptor/go
go run ./cmd/bench            # default dataDir=../data
go run ./cmd/bench ../data    # explicit
```

All output goes to stdout (timings to 1 dp via `time.Now()` monotonic clock).

### Observed run (this machine — digests are the gate, ms are the numbers)

```
LOAD ms=796.5
PREP ms=540.5
GROUP queries=24 journeys=39 digest=26203913 ms=4147.8
RANGE queries=5 journeys=100 digest=773022892 ms=12867.1
DIGEST group=26203913 range=773022892 journeys=139
```

**Digests match the golden gate byte-for-byte:** group=26203913,
range=773022892, journeys=139 (group 24/39 + range 5/100). Only the ms values
are machine-specific.

## Skipped

Per the porting contract: `transfer-patterns.ts`,
`transfer-pattern-worker.ts`, `TransferPatternRepository`, `TransferPatternQuery`,
`integration.ts`, `performance.ts`. None have unit tests.

## Semantic decisions

- **Map ordering (contract trap #1).** Go map iteration is randomized, so an
  `OrderedMap[V]` (insertion-ordered key slice + map) backs everywhere JS relies
  on `Object.keys`/`for..in` order: the scan's initial marked stops
  (`Object.keys(origins)`), `getMarkedStops` (`kArrivals[k]`), the queue
  (`Object.entries`), the connection-index stop keys (`getFoundStations`,
  `GraphResults.getPaths`, `StringResults.add`), `usefulTransfers` (defines the
  scan stop universe), and `routesAtStop`. `StopTimes` (origin departure times)
  is therefore `*OrderedMap[Time]`, not a plain map. The per-destination round
  map keyed by integer `k` iterates in **numeric ascending** order
  (`sort.Ints`) in `JourneyFactory`/`GraphResults`/`StringResults`, matching JS
  integer-key ordering.
- **`getRouteId`** = `stop + (pickUp?"1":"0") + (dropOff?"1":"0")` joined by `","`
  (JS `Array.join()` default separator), with `"overtakes"` appended when an
  earlier same-route trip arrives later.
- **Stable sorts.** `sort.SliceStable` for trip sort (by `stopTimes[0].departureTime`)
  and for `MultipleCriteriaFilter` (departure asc, arrival desc tiebreak), matching
  ES2019 stable `Array.sort`.
- **`MAX_SAFE_INTEGER`** = `9007199254740991` as a typed `int64` constant
  (`MaxSafeInteger`). All times are `int64`.
- **Dates in UTC.** `SearchDate` parses ISO dates as UTC midnight via
  `time.Parse`, yields a YYYYMMDD `DateNumber`, computes JS-style DOW
  (Sun=0..Sat=6) from `time.Weekday()`, and `AddDay()` does real calendar
  arithmetic (month/year rollover). Verified DOW: 2018-10-16=Tue(2),
  2018-10-22=Mon(1), 2019-04-18=Thu(4), 2019-04-23=Tue(2), 2018-12-31=Mon(1).
- **`Service.runsOn` key-present vs truthy.** Implemented with a comma-ok map
  read: a present key returns its boolean value directly (so a present `false`
  excludes and short-circuits, matching the JS `dates[date] || (!hasOwn && ...)`);
  an absent key falls through to start/end/day-of-week.
- **JS truthiness in the scan.** `previousArrival` returning `undefined` *or* `0`
  is falsy in JS (`if (previousArrival)`); the Go port treats "absent or 0" as
  not-truthy (`paOK && previousArrival != 0`). Origin departure times use
  `origins[stop] || MAX_SAFE_INTEGER`, so a stored `0` becomes infinity — handled
  by the same `t != 0` check in `ScanResultsFactory.Create`.
- **`st()` null vs 0.** The reference distinguishes `null` from `0`
  (`dropOff = arr !== null`). Go has no nullable int literal, so the test helper
  `st(stop, arr, dep *Time)` takes `*Time`; `nil` = JS `null`, `p(0)` = a real
  zero. `arrivalTime = arr ?? dep`, `departureTime = dep ?? arr`,
  `dropOff = arr != nil`, `pickUp = dep != nil`.
- **Journey equality (contract #8/#9).** `setDefaultTrip` overwrites each
  timetable leg's `Trip`, and `journeysEqual`/`legEqual` compare timetable legs by
  `stopTimes` + `origin` + `destination` only (trip ignored), and transfer legs by
  value. Leg kind is distinguished via the `AnyLeg` interface (`*TimetableLeg` vs
  `*Transfer`).
- **`GraphResults`/`StringResults` synthetic input.** The spec `mergePath` helper
  feeds `kConnections[dest][i] = [{stopTimes:[{stop:origin}]}, 0, 1]`. Reproduced
  by `buildSyntheticKConnections`. GraphResults equality compares each node's
  label + parent-chain (`nodeChain`); StringResults compares set membership
  (order-independent, like JS `Set`/object `toEqual`), with key `origin>destination
  ? dest+origin : origin+dest` and the reversed-tail path string.
- **RangeQuery date mutation.** In TS the same `Date` object is shared across
  RangeQuery iterations and would be mutated by `setDate` *only if* a day yields
  no results. In all RangeQuery tests every iteration finds results on day 1, so
  no mutation occurs; the Go `SearchDate` value semantics (each `Plan` starts from
  the original date) are equivalent for these tests. Noted in case a future
  multi-day range test is added.
