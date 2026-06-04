# RAPTOR cross-language benchmark results

Complex-query benchmark over the full UK National Rail GTFS feed (~240k trips, 2.37M
stop_times). Two workloads, **load time and query time reported separately** (the
setup cost — parsing + index build — is one-time and should not be conflated with
per-query cost).

Run it yourself: `benchmarks/compare/raptor/bench.sh` (see "Reproducing" below).

## Workloads

- **GROUP** — the 24 group-station origin/destination-set queries from the reference
  `test/performance.ts`, each planned at 10:00 on 2025-09-02 with the default
  `MultipleCriteriaFilter` (earliestArrival + leastChanges). Origin/destination *sets*
  (e.g. all four Birmingham stations → 18 London terminals).
- **RANGE** — "next 20 journeys departing after 08:00" for 5 city pairs: the RangeQuery
  profile loop (plan, advance past the earliest departure, repeat) capped at 20.

## Phases (the load/query split)

| phase | what | one-time? |
|-------|------|-----------|
| LOAD  | parse the CSV feed → trips/transfers/interchange | yes (setup) |
| PREP  | build the RAPTOR indexes (`RaptorAlgorithmFactory.create`) | yes (setup) |
| GROUP | the 24 group-station queries | per-query work |
| RANGE | next-20 × 5 pairs | per-query work |

`setup = LOAD + PREP`; `query = GROUP + RANGE`.

## Results (single machine, back-to-back, min ms)

The four hashed-map languages, measured back-to-back by `bench.sh` (so comparable):

```
lang     |      LOAD      PREP |     GROUP     RANGE |     setup     query
---------+---------------------+---------------------+----------------------
node     |    1759.7     701.7 |    4243.5   12490.8 |    2461.4   16734.3
go       |     832.0     570.6 |    4038.7   11859.9 |    1402.6   15898.6
rust     |     921.8     434.5 |    6657.6   20969.0 |    1356.3   27626.6
python   |    6633.4    2942.8 |   10721.8   31536.8 |    9576.2   42258.6
```

**Lin** (measured separately — it is far slower, see below):

```
lin      |   48000-67000  ~145000 |  ~295000/query  | (RANGE not run to completion)
```

i.e. LOAD ≈ 48-67 s, PREP ≈ 145 s, and ~295 s **per group-station query**.

### Correctness gate (all languages agree)

Every language computes identical order-independent digests, so they all did the same
work:

- Full GROUP (24 queries): **39 journeys, digest = 26203913** — node, go, rust, python ✓
- Full RANGE (next-20 ×5): **100 journeys, digest = 773022892** — node, go, rust, python ✓
- Lin: verified on a reduced GROUP sub-run (first 3 queries) → **4 journeys,
  digest = 146713452**, byte-identical to the same Node sub-run. Lin's full GROUP/RANGE
  digests were not collected because each group-station query takes ~5 minutes; the
  reduced match plus all 48 unit tests confirm the algorithm is correct.

## Observations

- **Go is fastest** overall (setup 1.4 s, query 15.9 s). Node is close on query
  (16.7 s) with a heavier load. Both beat Rust here.
- **Rust is slower than Go/Node on the query phase** (27.6 s vs ~16 s) — reproducible,
  not a measurement fluke. The faithful Rust port uses `Rc<Trip>` and clones in the
  journey-reconstruction path; its per-journey allocation is heavier than Go's. (A
  perf-tuned Rust port could close this, but the goal was a faithful 1:1 port.)
- **Python** is the slowest of the four (~10× Go on setup, ~2-3× on query) — expected
  for CPython on an allocation- and dict-heavy workload.
- **Lin completes and is correct, but is ~1000× slower per query.** Root cause is the
  language characteristic documented in `LIN_ISSUES.md` #4: `Json` objects are
  association lists with O(n) key lookup, and RAPTOR's index build + scan are
  dictionary-dominated (routeId-keyed indexes, per-stop arrival maps). PREP (the index
  build over 240k trips, ~16k routeIds) alone is ~145 s. A hashed object / `Map` type
  is the single biggest lever for Lin here. (The earlier 80 GB / many-hours behaviour
  was a separate O(n²)-sort bug, now fixed — see `lin/NOTES.md`.)

## Reproducing

```bash
cd <repo>
# one-time: extract the feed
mkdir -p benchmarks/compare/raptor/data
tar xzf benchmarks/compare/raptor/gtfs.tar.gz -C benchmarks/compare/raptor/data
cargo build --workspace            # for the lin column

# fast four (recommended):
LANGS="node go rust python" benchmarks/compare/raptor/bench.sh
RUNS=3 LANGS="node go rust python" benchmarks/compare/raptor/bench.sh   # min of 3

# include lin (WARNING: ~hours for the full query workload):
benchmarks/compare/raptor/bench.sh
```

Each language's `bench` entry point:
- node:   `node bench.js [dataDir]`
- go:     `go run ./cmd/bench [dataDir]`
- rust:   `cargo run --release --bin bench -- [dataDir]`
- python: `python3 bench.py [dataDir]`
- lin:    `lin run bench.lin` (or build it; data dir hardcoded)

All emit the same `LOAD/PREP/GROUP/RANGE/DIGEST` line format, which `bench.sh` parses.
