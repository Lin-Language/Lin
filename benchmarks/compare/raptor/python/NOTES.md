# RAPTOR Python port — notes

## Test command

```
cd benchmarks/compare/raptor/python
python3 -m unittest discover            # or: python3 -m unittest discover -v
```

Exits nonzero on failure (verified). Python 3.11. No third-party deps (stdlib
`unittest` only — pytest is not installed).

## Passing count

**51 tests, all passing** (`Ran 51 tests ... OK`).

Breakdown vs the reference specs:
- Service: 6
- TimeParser: 1
- QueueFactory: 2
- MultipleCriteriaFilter: 3
- GraphResults: 4
- StringResults: 3
- DepartAfterQuery: 24 (the big one)
- GroupStationDepartAfterQuery: 2
- RangeQuery: 2
- DateUtil: 4 (extra — not in the reference; added to pin getDateNumber + the
  contract's DOW values + month/year rollover. DateUtil has no reference spec.)

Reference spec total = 47 `it()` cases; all 47 are mirrored 1:1 by method name.

## Modules ported

`raptor/` package: `gtfs.py`, `service.py`, `time_parser.py`, `date_util.py`,
`queue_factory.py`, `route_scanner.py`, `scan_results.py`, `algorithm.py`
(RaptorAlgorithm + RaptorAlgorithmFactory), `journey.py`, `journey_factory.py`,
`multiple_criteria_filter.py`, `group_query.py`, `depart_after_query.py`,
`range_query.py`, `transfer_pattern.py` (GraphResults + StringResults).

Test helpers in `util.py` (ported from `test/unit/util.ts`): `t`, `st`, `tf`,
`j`, `set_default_trip`, plus `all_days` / `services`.

## GTFS loader + CLI runner

`raptor/gtfs_loader.py` (`load_gtfs(data_dir) -> (trips, transfers, interchange)`)
and `run.py` mirror `node/src/gtfs/GTFSLoader.js` + `node/run.js`.

### Run command + observed RESULT

```
cd benchmarks/compare/raptor/python
python3 run.py                       # defaults: ../data TBW NRW 2025-09-02 08:00
```

Full stdout (byte-for-byte equal to the Node golden reference — verified with
`diff`):

```
JOURNEY dep=08:10:00 arr=11:18:00 legs=3
  TBW 08:10:00 -> LBG 08:53:00
  TRANSFER LBG -> SRA (1620s)
  SRA 09:37:00 -> NRW 11:18:00
RESULT dep=29400 arr=40680 legs=3 count=1
```

- Matches the pinned gate exactly. **0 trips dropped** (no calendar-less trips).
- Timing (stderr only): `load≈7.1s plan≈0.46s` on this box (CPython parsing 2.37M
  stop_times rows — expected to take several seconds).
- DOW for 2025-09-02 computes to 2 (Tuesday, JS Sun=0 convention).

## Benchmark (cross-language gate)

```
cd benchmarks/compare/raptor/python
python3 bench.py                     # optional arg: [dataDir], default ../data
```

`bench.py` mirrors `node/bench.js` exactly: same 24 GROUP queries, 5 RANGE pairs,
`journeyDigest` formula, and output line format. Reuses the loader + factory
setup from `run.py` (builds the raptor once, no date pre-filter). GROUP uses
`MultipleCriteriaFilter`; RANGE uses no filter and the "next 20 after 08:00"
profile loop.

Observed stdout on this box (CPython 3.11):

```
LOAD ms=6719.4
PREP ms=2985.9
GROUP queries=24 journeys=39 digest=26203913 ms=10937.5
RANGE queries=5 journeys=100 digest=773022892 ms=32065.8
DIGEST group=26203913 range=773022892 journeys=139
```

**Digest gate matches the Node golden bench exactly**: `group=26203913`,
`range=773022892`, `journeys=139` (GROUP 39 + RANGE 100). Timing ms is per-box
and not part of the gate; CPython is slow on RANGE (~32s) as expected — that
workload reruns the full RAPTOR scan up to 20 times per pair. The unit suite
stays green (51 tests) and `run.py` is unchanged.

### Loader semantics

Plain `split(",")` CSV (no quoted fields). `stop_times` grouped by `trip_id` in
file order (plain dict). Times via `TimeParser` (no mod 86400, may exceed 24h).
`pickUp`/`dropOff` True only when the field is `"0"` or empty. `transfers.txt`:
same-stop → `interchange[from]`, else a `Transfer` pushed onto `transfers[from]`.
`links.txt`: footpath `Transfer`s with parsed `start_time`/`end_time` (date/day
columns ignored, matching the reference `link` processor). Trips with no calendar
row are dropped and counted to stderr.

### Algorithm fix surfaced by real data (faithful-port gap)

`algorithm._scan_transfers` indexed `self.interchange[stop_pi]` and
`results.bestArrival(stop_pi)` directly. For a transfer destination not on any
route path (e.g. `ZCO`), both keys are absent. In JS those reads yield
`undefined`, so `arrival` becomes `NaN` and every comparison is false → the
transfer is silently skipped. Python raised `KeyError` instead (the unit tests
never hit this; only the full feed does). Fixed by skipping the transfer when
`stop_pi` is missing from `interchange`/`bestArrivals`, which reproduces JS's
NaN/undefined behaviour exactly. All 51 unit tests still pass and the pinned gate
matches byte-for-byte.

## Semantic decisions / trap handling

1. **Insertion order**: Python `dict` preserves insertion order, so all
   `Object.keys`/`for..in` iterations map directly (`list(d)`, `d.items()`).
   The per-destination round map keyed by integer `k` is iterated **numeric
   ascending** via `sorted(dest_map, key=lambda x: int(x))` in JourneyFactory
   and is left in insertion order (which equals numeric here) in the
   transfer-pattern stores, matching JS's integer-key iteration.

2. **getRouteId**: `",".join(stop + ("1" if pickUp else "0") + ("1" if dropOff
   else "0") for s in stopTimes)`, plus the `"overtakes"` suffix when an earlier
   trip on the same routeId arrives later.

3. **Stable sort**: trip sort by `stopTimes[0].departureTime` and the filter
   sort both use Python `sorted` (stable). Filter tie-break is
   `key=(departureTime, -arrivalTime)` = dep asc, arr desc.

4. **MAX_SAFE_INTEGER** = `9007199254740991` used as the arrival sentinel and
   `tf` endTime default. `getFoundStations` uses `max(1, bestArrivals[s] - 86400)`.

5. **Dates / DOW**: `Date` wraps a `datetime.date` treated as the UTC calendar
   date. `getDay()` converts Python Mon=0..Sun=6 to JS Sun=0..Sat=6 via
   `(weekday + 1) % 7`. `add_days` uses `timedelta` for real month/year rollover.
   `getDateNumber` = `int(strftime("%Y%m%d"))`. The pinned DOW values from the
   contract are asserted in `test_date_util.py`.

6. **Service.runsOn**: `dates.get(date)` (truthy include) OR `(date not in dates
   and start <= date <= end and days.get(dow))` — preserving the present-False
   (exclude) vs absent distinction.

7. **Journey equality ignores trip**: `TimetableLeg.trip` is declared
   `field(..., compare=False)` in the dataclass, so structural equality compares
   `stopTimes`/`origin`/`destination` only. `set_default_trip` is still ported
   faithfully (it overwrites the trip) but is structurally redundant for the
   assertions because of `compare=False`. Transfer and TimetableLeg are distinct
   dataclasses, so the two leg kinds never compare equal.

8. **prevConnections.reverse()**: JS `Array.reverse` mutates in place and the
   same list is reused across search days, so I mutate the Python list in place
   too (`group_query._get_journeys_from_connections`) rather than using
   `reversed()`. (No test exceeds 2 search days, where the two are identical, but
   the faithful behaviour is preserved.)

9. **StringResults synthetic stopTime**: the reference `mergePath` test helper
   feeds `{stopTimes: [{stop: origin}]}` with no `departureTime`. JS reads it as
   `undefined` → NaN arithmetic that is never asserted. The Python test fixture
   gives the synthetic stopTime `departureTime=0` so the arithmetic in
   `StringResults._get_path` does not raise; this does not affect the asserted
   result keys/pattern strings.

## Skipped (no unit tests / Node-only I/O)

Per the contract: `GTFSLoader` (gtfs-stream/mysql), `transfer-patterns.ts`,
`transfer-pattern-worker.ts`, `TransferPatternRepository`,
`TransferPatternQuery` (no spec), `integration.ts`, `performance.ts`. No
benchmark/perf entry point added (the lead wires that up later).

## Contradicting tests

None. All 47 reference cases ported without fudging.
