# GTFS loader + CLI runner contract (shared across all 5 languages)

The unit-test phase is done (all 5 ports green). This phase adds a **GTFS loader** and
a **CLI runner** so every port can plan a real journey over the supplied feed
(`benchmarks/compare/raptor/gtfs.tar.gz`, extracted to `benchmarks/compare/raptor/data/`).

The reference loader is `/tmp/raptor-ref/src/gtfs/GTFSLoader.ts` (+ `TimeParser.ts`).
It uses the Node-only `gtfs-stream` lib; we reimplement its logic as a plain CSV reader.
Node.js is the golden reference — its output pins the expected answer; the other four
must match it exactly.

## The feed

UK National Rail GTFS. ~240k trips, ~2.37M stop_times, ~3080 stops, ~7147 calendars,
~48.5k calendar_dates, ~3079 transfers, ~8703 links. Service window ≈ 2025-08-26..2025-11-03.
Stop IDs are 3-letter CRS codes: TBW=Tunbridge Wells, NRW=Norwich, etc.

Files (plain CSV, comma-separated, **no quoted fields in this feed** — a simple
split-on-comma is sufficient; do NOT add a heavy CSV lib):
- `stops.txt`, `routes.txt`, `agency.txt` — not needed by the planner (skip).
- `trips.txt`: header `route_id,service_id,trip_id,...`. Need service_id, trip_id.
- `stop_times.txt`: `trip_id,arrival_time,departure_time,stop_id,stop_sequence,stop_headsign,pickup_type,drop_off_type,...`
- `calendar.txt`: `service_id,monday,...,sunday,start_date,end_date`
- `calendar_dates.txt`: `service_id,date,exception_type` (1=include, 2=exclude)
- `transfers.txt`: `from_stop_id,to_stop_id,transfer_type,min_transfer_time`
- `links.txt`: `from_stop_id,to_stop_id,mode,duration,start_time,end_time,start_date,end_date,monday,...,sunday`

## Parsing rules (mirror GTFSLoader.ts exactly)

### TimeParser — "HH:MM:SS" → seconds from midnight
`hh*3600 + mm*60 + ss`. **Times may exceed 24h** (e.g. `24:30:00` = 88200, `25:11:00`).
Do NOT mod by 86400. Empty/missing time → treat as the other (see stop_time below).

### trips.txt → Trip
`{ tripId: trip_id, serviceId: service_id, stopTimes: [], service: <resolved later> }`.
stopTimes filled from stop_times grouped by trip_id, **in file order** (which is
stop_sequence order — the feed is pre-sorted; you may rely on file order like the
reference does, OR sort by stop_sequence to be safe — file order matches the reference).

### stop_times.txt → StopTime
```
stop          = stop_id
departureTime = TimeParser(departure_time)
arrivalTime   = TimeParser(arrival_time)
pickUp        = (pickup_type === "0" || pickup_type === undefined/empty)
dropOff       = (drop_off_type === "0" || drop_off_type === undefined/empty)
```
In THIS feed pickup_type/drop_off_type ∈ {0,1,3}. Only "0" (and empty) is true; "1"
and "3" are false. (The reference treats undefined as true; an empty CSV field counts
as undefined.) Group stop_times by trip_id preserving file order.

### calendar.txt → Calendar/Service
```
startDate = +start_date  (e.g. 20250826, integer)
endDate   = +end_date
days = { 0: sunday==="1", 1: monday==="1", 2: tuesday==="1", 3: wednesday==="1",
         4: thursday==="1", 5: friday==="1", 6: saturday==="1" }   // JS DOW: Sun=0
include = {} ; exclude = {}   // filled from calendar_dates
```

### calendar_dates.txt → dates index
`setNested(exception_type === "1", dates, service_id, date)`: i.e.
`dates[service_id][+date] = (exception_type === "1")`. So exception_type 1 → true
(include), 2 → false (exclude). In this feed all are 2 (excludes). The Service's
`dates` map is `dates[service_id]` (default `{}` if the service has none).

### transfers.txt → interchange + transfers
For each row:
- if `from_stop_id === to_stop_id`: `interchange[from_stop_id] = +min_transfer_time`
  (this is the per-station interchange time).
- else: a Transfer `{ origin: from, destination: to, duration: +min_transfer_time,
  startTime: 0, endTime: MAX_SAFE_INTEGER }`, pushNested into `transfers[from]`.

### links.txt → transfers (the "link"/footpath rows)
For each row, a Transfer `{ origin: from_stop_id, destination: to_stop_id,
duration: +duration, startTime: TimeParser(start_time), endTime: TimeParser(end_time) }`,
pushNested into `transfers[from_stop_id]`. (The reference `link` processor ignores the
date/day columns — match that: do NOT filter links by start_date/day.)

### Service resolution
After parsing: for each calendar build `Service(startDate, endDate, days, dates[serviceId] || {})`.
Assign `trip.service = services[trip.serviceId]`. A trip whose service_id has NO
calendar row gets... in the reference `services[serviceId]` would be undefined →
`runsOn` would throw. DEFENSIVE: if a trip's serviceId has no calendar, DROP that trip
(skip it) rather than crash — and `log()`/stderr how many were dropped. (Verify against
the feed; ideally zero are dropped.)

Loader output (the GTFSData tuple): `[trips, transfers, interchange]` (stops not needed).

## CLI runner

Each port builds an executable/script that:
1. Takes args: `<dataDir> <origin> <destination> <YYYY-MM-DD> <HH:MM>` (or seconds).
   Provide sensible defaults: dataDir=`../data` (relative to the runner), and a default
   query so `run` with no args works.
2. Loads the feed, builds `RaptorAlgorithmFactory.create(trips, transfers, interchange)`
   (NO date pre-filter — pass the date through the query, matching DepartAfterQuery),
   runs `DepartAfterQuery(raptor, journeyFactory).plan(origin, dest, date, timeSeconds)`.
3. Prints results to stdout in a FIXED, language-independent format so all five can be
   diff'd. Use this exact format, one line per journey, legs separated by " | ":
```
JOURNEY dep=<HH:MM:SS> arr=<HH:MM:SS> changes=<n>
  <ORIG> <dep HH:MM:SS> -> <DEST> <arr HH:MM:SS>     # one line per leg
  TRANSFER <ORIG> -> <DEST> (<duration>s)            # for transfer legs
```
   where dep/arr are formatted from seconds (allow >24h: HH can be 24, 25...; format as
   `floor(s/3600):(s%3600)/60:s%60` zero-padded). changes = number of legs - 1... NO:
   the reference counts legs; print `legs=<n>` instead to avoid ambiguity. Sort journeys
   by departureTime asc then arrivalTime asc for stable cross-language output.
   Also print a final summary line: `SUMMARY journeys=<count>` and, for a
   machine-checkable digest, `DIGEST <sha-ish>` — actually simpler: print
   `RESULT dep=<firstDepSeconds> arr=<firstArrSeconds> legs=<n> count=<n>` as the LAST
   line, computed from the journey with the earliest arrival (ties: fewest legs). This
   RESULT line is the cross-language correctness gate (like the other benchmarks' RESULT).

   PIN DOWN the format with the lead's Node reference output before finalizing — the
   lead will run Node first and give you the exact expected RESULT line + sample.

## Timing
The runner should print timing to STDERR only (e.g. `load=...ms plan=...ms`), never
stdout, so stdout stays a pure diffable result. Keep the loader reasonably efficient —
2.37M stop_times rows; avoid O(n²) grouping (use a hash map trip_id -> stopTimes list).

## Directory / invocation per language (add to each NOTES.md)
- node:   `node run.js <dataDir> <orig> <dest> <date> <time>`
- go:     a `cmd/` main or `go run .` (keep tests still passing — main in package main or a subdir)
- rust:   a `src/bin/run.rs` → `cargo run --release --bin run -- <args>`
- python: `python3 run.py <args>`
- lin:    a `run.lin` built with `lin build` → run the binary, OR `lin run` if available.

Keep the unit tests passing. Do NOT modify the algorithm modules except to export what
the loader/runner needs. Confine changes to your `<lang>/` dir.
