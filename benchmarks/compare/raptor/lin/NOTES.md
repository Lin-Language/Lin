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

`service.lin`, `timeParser.lin`, `dateUtil.lin`, `queueFactory.lin`,
`routeScanner.lin`, `scanResults.lin`, `raptor.lin` (RaptorAlgorithm + factory),
`journeyFactory.lin`, `filter.lin` (MultipleCriteriaFilter), `query.lin`
(GroupStation/DepartAfter/Range), `graphResults.lin`, `stringResults.lin`,
plus `sortutil.lin` (stable sort) and `testutil.lin` (t/st/tf/j/setDefaultTrip +
trip-ignoring deep journey equality).

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

### THE FIX that made it feasible: stable merge sort, not O(n²) insertion sort

The original `sortutil.lin` `stableSort` was an insertion sort that **rebuilt the
entire output array on every insertion** — for each of the 240009 trips it allocated
a fresh array copying all prior (boxed) trips, i.e. ~n²/2 ≈ 29 billion element-copies.
That, not the loader and not `createRaptor`, was what drove RSS past 80 GB and the
runtime to many hours. (An earlier version of this note misattributed the blowup to
`createRaptor`'s object indexing — that was measured with the O(n²) sort still in the
path and was wrong.) `stableSort` is now a **bottom-up merge sort** (O(n log n) time,
O(n) space) with the same stable contract, so the trip sort and the
MultipleCriteriaFilter sort both scale. All 9 unit-test files still pass.

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

### scanTransfers null-skip fix (the semantic hazard)

On the FULL feed a transfer/link destination `stopPi` may not lie on any route path,
so `raptor.interchange[stopPi]` and `bestArrival(results, stopPi)` are `null`. JS
turns `number + undefined` into `NaN` and every comparison against NaN is false, so
the transfer is silently skipped. Lin would instead error on `number + null` /
`number < null`. `raptor.lin`'s `scanTransfers` now guards `ic != null && best !=
null` before computing/comparing — reproducing JS's skip-on-missing semantics. All 9
unit-test files still pass (they don't exercise this path).

### Another local-var-in-closure miscompile (worked around in the loader)

While wiring the trips↔stop_times positional merge-join, hit a second instance of the
mutable-`var`-in-closure codegen bug (same class as the `t()`/global-`var` bug below).
Inside a `.for` closure, **reassigning a local `var` within an `if` and reading it
later in the same iteration drops the write** — the read still sees the var's initial
value. Minimal repro:

```lin
var g = 0
ids.for(id =>
  var sts: Json = []
  if g < length(groups) then
    sts = groups[g]      // write is lost...
    g = g + 1            // ...but THIS write (to an outer var) persists
  push(out, length(sts)) // reads 0, not groups[g].length
)
```

`g` (outer-scope var) advances correctly; `sts` (closure-local var, reassigned in the
`if`) does not. Workaround in `gtfsLoader.lin`: bind the matched group into a `val`
via a conditional expression instead of a reassigned `var`
(`val sts = if matched then stopTimesGroups[g]["stopTimes"] else []`). This bug
deserves its own minimal regression test + codegen fix.

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

## Lin language finding (compiler bug, worked around)

The faithful port of `util.ts`'s `t()` wants a module-level `var tripId` that an
exported function increments (`trip${tripId++}`). This **miscompiles**: a top-level
`var` mutated by an *exported* function in an *imported* module panics codegen:

```
thread 'main' panicked at crates/lin-codegen/src/codegen/mod.rs:782:75:
Binary: undefined lhs temp Temp(0)
```

Minimal repro: a module with `var counter = 0` and
`export val nextId = () => counter = counter + 1; counter`, imported and called from
another file via `lin build`. (Reproduced 2026-06-03 on this worktree.)

Workaround in `testutil.lin`: `t()` derives a content-based `tripId` from the stop /
time signature instead of a global counter. This is behaviorally equivalent for the
tests — `tripId` is never asserted (`setDefaultTrip` overwrites every leg's trip
before comparison), and the algorithm only needs trips on a single route to be
distinguishable, which a content signature guarantees. This bug deserves its own
regression test + fix outside this benchmark.
