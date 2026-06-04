# RAPTOR Rust port — notes

## Test command

```bash
cd benchmarks/compare/raptor/rust
cargo test
```

Exits nonzero on failure. Standalone crate — it has its own `Cargo.toml` with an
empty `[workspace]` table, so it is detached from the repo's root cargo workspace
and is not referenced by the parent `Cargo.toml`.

## Result

**51 tests pass, 0 failed.** This covers every reference `it()` case plus three
extra `date_util` sanity tests (DOW / YYYYMMDD / day-rollover).

Reference spec → Rust test mapping (all mirrored by name):
- `Service.spec.ts` (6) → `tests::service_spec`
- `TimeParser.spec.ts` (1) → `tests::time_parser_spec`
- `QueueFactory.spec.ts` (2) → `tests::queue_factory_spec`
- `DepartAfterQuery.spec.ts` (24) → `tests::depart_after_query_spec`
- `GroupStationDepartAfterQuery.spec.ts` (2) → `tests::group_station_spec`
- `RangeQuery.spec.ts` (2) → `tests::range_query_spec`
- `MultipleCriteriaFilter.spec.ts` (3) → `tests::multiple_criteria_filter_spec`
- `GraphResults.spec.ts` (4) + `StringResults.spec.ts` (3) → `tests::transfer_pattern_spec`
- plus `date_util::tests` (3)

## GTFS loader + CLI runner

A real-feed loader (`src/gtfs_loader.rs`) and CLI runner (`src/bin/run.rs`) port
`node/run.js` + `node/src/gtfs/GTFSLoader.js` (which mirror the TS reference).

Build + run (use `--release` — the loader parses ~2.37M `stop_times` rows):

```bash
cd benchmarks/compare/raptor/rust
cargo run --release --bin run -- [dataDir] [origin] [destination] [YYYY-MM-DD] [HH:MM]
```

Defaults (so bare `cargo run --release --bin run` works): `dataDir` =
`<crate-dir>/../data` (i.e. `benchmarks/compare/raptor/data`, resolved via
`CARGO_MANIFEST_DIR`), query `TBW NRW 2025-09-02 08:00`.

`load_gtfs(data_dir) -> (Vec<Rc<Trip>>, TransfersByOrigin, Interchange)`: plain
buffered split-on-comma CSV (std + indexmap only, no csv/serde). Columns are read by
name via a header index map. stop_times are grouped by trip_id preserving file order;
Service is built from calendar + calendar_dates; transfers from transfers.txt
(same-stop → interchange, else a `Transfer`) plus links.txt footpaths (date/day cols
ignored, matching the reference). Times may exceed 24h (no mod). Trips whose
service_id has no calendar row are dropped and the count logged to stderr.

The runner builds the raptor via `RaptorAlgorithmFactory::create(.., None)` (no date
pre-filter — the date flows through `DepartAfterQuery::plan`, `maxSearchDays = 3`
matching Node's default), sorts journeys by departure asc then arrival asc, prints the
contract format to stdout, and emits `load=..ms plan=..ms` to stderr only.

### Observed RESULT (pinned gate — exact byte-for-byte match)

```
JOURNEY dep=08:10:00 arr=11:18:00 legs=3
  TBW 08:10:00 -> LBG 08:53:00
  TRANSFER LBG -> SRA (1620s)
  SRA 09:37:00 -> NRW 11:18:00
RESULT dep=29400 arr=40680 legs=3 count=1
```

0 trips dropped. Timing (release, single run): `load≈1120ms plan≈320ms`.

## Cross-language benchmark (`src/bin/bench.rs`)

Port of `node/bench.js` — the cross-language correctness gate. Reuses the same
loader + `RaptorAlgorithmFactory::create(.., None)` setup as `run.rs` (no date
pre-filter; the date flows through the query), builds the raptor once, then runs two
workloads:

- **GROUP** — the 24 group-station origin/destination-set queries planned at 10:00
  (36000s) on 2025-09-02 with `GroupStationDepartAfterQuery` (`maxSearchDays=3`,
  filters `[MultipleCriteriaFilter]`). Sum journey counts; accumulate digest.
- **RANGE** — "next 20 journeys departing after 08:00" for 5 pairs, no filter. The
  RangeQuery profile loop capped at N: repeatedly `plan`, advance `time =
  min(departureTime)+1`, break if empty, until 20 collected; sort by (dep asc, arr
  asc); take first 20. Sum counts; accumulate digest.

`journeyDigest(j) = ((dep%1e9)*1_000_003 + (arr%1e9)*31 + legs) % 1_000_000_007`,
accumulated `(acc + contrib) % mod`. All in `i64` (`dep*1_000_003 < 1e15`, no
intermediate overflow — matches Node's BigInt math exactly). `legs` = number of legs.

Build + run (use `--release` — the loader parses ~2.37M `stop_times` rows):

```bash
cd benchmarks/compare/raptor/rust
cargo run --release --bin bench           # default dataDir = <crate-dir>/../data
cargo run --release --bin bench -- [dataDir]
```

### Observed output (digest gate — exact match with the Node golden bench)

```
LOAD ms=1052.4
PREP ms=475.8
GROUP queries=24 journeys=39 digest=26203913 ms=7011.5
RANGE queries=5 journeys=100 digest=773022892 ms=22010.0
DIGEST group=26203913 range=773022892 journeys=139
```

The GROUP/RANGE journey counts and digests reproduce the golden gate exactly
(`group=26203913`, `range=773022892`, `journeys=139`, queries `24`/`5`). The `ms`
values are the benchmark numbers (release, single run on this host) and vary per run.

## Skipped (per the contract — no unit tests / Node-only I/O)

`transfer-patterns.ts`, `transfer-pattern-worker.ts`, `TransferPatternRepository`,
`TransferPatternQuery` (no spec), `integration.ts`, `performance.ts`.

## Module layout

`gtfs`, `service`, `time_parser`, `date_util`, `queue`, `route_scanner`,
`scan_results`, `journey`, `journey_factory`, `filter`, `raptor`, `query`,
`transfer_pattern`. Test fixtures (`t/st/tf/j/set_default_trip`) live in `test_util`,
the test cases in `tests`.

## Semantic decisions (and how the contract traps were handled)

1. **Insertion order (trap #1).** `indexmap::IndexMap` is used everywhere a JS object
   is iterated for output: `routesAtStop`, `routeStopIndex`, `routePath`,
   `usefulTransfers`, `interchange`, the queue, `bestArrivals`, per-round `kArrivals`,
   and the outer (by-stop) `kConnections`. The **per-destination round map** keyed by
   integer `k` is a `BTreeMap<usize, _>` so it iterates **numeric ascending**, matching
   JS's integer-key special case. `getMarkedStops` returns the insertion order of the
   current round's arrivals.

2. **`getRouteId` (trap #2).** `stop + (pickUp?"1":"0") + (dropOff?"1":"0")` joined by
   `","`. Overtaking appends the literal `"overtakes"` when an earlier same-route trip
   arrives later.

3. **Stable trip sort (trap #3).** `Vec::sort_by` on `stop_times[0].departure_time`
   (Rust's sort is stable). Same for `MultipleCriteriaFilter`.

4. **MultipleCriteriaFilter (trap #4).** Sort by departure asc, tie-break arrival desc;
   keep A unless some LATER B satisfies all criteria (`earliestArrival` &&
   `leastChanges`).

5. **Sentinel (trap #5).** `MAX_SAFE_INTEGER = 9_007_199_254_740_991_i64`. `Time` is
   `i64`. `getFoundStations` uses `max(1, best - 86400)`.

6. **UTC dates (trap #6).** `date_util::UtcDate` parses `"YYYY-MM-DD"`, computes
   YYYYMMDD via arithmetic, DOW via Sakamoto's algorithm (verified against the five
   dated assertions in the contract: 2018-10-16 Tue=2, 2018-10-22 Mon=1, 2019-04-18
   Thu=4, 2019-04-23 Tue=2, 2018-12-31 Mon=1), and `add_day` with full month/year
   rollover (incl. leap years). The date is copied into `getJourneys` and advanced on
   a local copy, matching the per-`plan`-call mutation in JS (RangeQuery's same-day
   searches never trigger an advance, so copy-by-value is faithful).

7. **Service.runsOn (trap #7).** `HashMap<i64,bool>` with the `contains_key` vs truthy
   distinction: `dates[date] === true` short-circuits include; a present `false` makes
   the `!hasOwn` clause false (excluded).

8/9. **Journey equality (traps #8/#9).** `journeys_equal` compares departure/arrival
   times and legs structurally; timetable legs by `stop_times + origin + destination`
   (trip ignored), transfers by all fields. `TimetableLeg.trip` is `Option<Rc<Trip>>`
   and `set_default_trip` nulls it — but since equality ignores trip, this is a
   faithful no-op normaliser.

10. **JourneyFactory.getJourneyLegs (trap #10).** Walks k connections back to k=1,
    pushing legs, then reverses. Trip leg uses `stop_times[start..=end]`.

11. **RouteScanner (trap #11).** Backward scan from a per-route `routeScanPosition`
    memo (init `len-1`), breaks on `departureTime < time`, records `lastFound` on
    matching service, updates the memo under `!lastFound || lastFound === trip`
    (`Rc::ptr_eq`). The memo is stateful within one `scan()`; a fresh scanner is
    created per day via the factory.

12. **GraphResults / StringResults (trap #12).** A `Connection` is an enum
    `{ Trip(Rc<Trip>, start, end), Transfer(Transfer) }`. `connection_origin` reads
    `trip.stop_times[start].stop`. GraphResults nodes are `Rc<TreeNode>` with a parent
    pointer; `is_same` walks the parent chain comparing labels; node equality in tests
    compares the label + parent chain structurally (not pointer identity).
    StringResults' `journeyKey` uses string `>` comparison, `pathString` reverses the
    tail when `origin > destination`, and `getPath` uses prepend (`Vec::insert(0,..)`).
    Vitest `toEqual` compares `Set`s by **membership, not order**, so the StringResults
    assertions compare sets unordered (Rust `BTreeSet`).

## Other notes

- JS truthiness of `previousArrival`/`previous-arrival` (`if (previousArrival)`) is
  modelled as "present and non-zero" via a `truthy()` helper in `raptor::scan`.
- Trips are `Rc<Trip>` so connections cheaply reference shared trips and the
  route-scanner memo can compare by pointer (`Rc::ptr_eq`) like JS `===`.
