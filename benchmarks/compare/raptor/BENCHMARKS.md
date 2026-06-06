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

All five languages, measured by `bench.sh` (the fast four back-to-back; Lin run
separately as it is slower, but with the same optimized native compile and the same
internally-timed phases, so comparable):

```
lang     |      LOAD      PREP |     GROUP     RANGE |     setup     query
---------+---------------------+---------------------+----------------------
node     |    1859.3     785.2 |    4499.4   12457.8 |    2644.5   16957.2
go       |     879.0     527.9 |    4375.0   12806.7 |    1406.9   17181.7
rust     |    1036.4     504.7 |    7054.8   20344.9 |    1541.1   27399.7
python   |    6710.3    2779.1 |   11739.5   48249.2 |    9489.4   59988.7
lin      |   28806.0   30254.0 |   99646.0  299499.0 |   59060.0  399145.0
```

i.e. Lin: LOAD ≈ 28.8 s, PREP ≈ 30.3 s, GROUP ≈ 99.6 s (all 24 queries, ~4.2 s/query),
RANGE ≈ 299.5 s (5 profile queries).

### Correctness gate (all languages agree)

Every language computes identical order-independent digests, so they all did the same
work:

- Full GROUP (24 queries): **39 journeys, digest = 26203913** — all five ✓
- Full RANGE (next-20 ×5): **100 journeys, digest = 773022892** — all five ✓
- Combined: **139 journeys, group=26203913 range=773022892** — node, go, rust, python,
  **lin** all agree. Lin now completes the full GROUP+RANGE workload (not a reduced
  sub-run), byte-identical to the other four.

### Lin: ~70× faster on query since the hashed-map migration

The Lin numbers above are after the typed `{ String: T }` index-signature maps (ADR-082,
hashed O(1) containers) replaced the O(n) association-list `Json` objects throughout the
port — the lever called out in `LIN_ISSUES.md` #4. Versus the earlier assoc-list run:

| phase | old (`Json` assoc-list) | now (hashed maps) | speedup |
|-------|-------------------------|-------------------|---------|
| LOAD  | ~48–67 s                | 28.8 s            | ~1.7–2.3× |
| PREP  | ~145 s                  | 30.3 s            | ~4.8× |
| GROUP | ~295 s **per query** (~7000 s / 24) | 99.6 s for all 24 | ~70× |
| RANGE | never completed         | 299.5 s (completes) | — |

The previously-recorded "~1000× slower per query / RANGE not run to completion" no longer
holds: Lin completes the full faithful benchmark and lands in the same order of magnitude
as the dynamic languages.

## Observations

- **Go is fastest** overall (setup 1.4 s, query 15.9 s). Node is close on query
  (16.7 s) with a heavier load. Both beat Rust here.
- **Rust is slower than Go/Node on the query phase** (27.6 s vs ~16 s) — reproducible,
  not a measurement fluke. The faithful Rust port uses `Rc<Trip>` and clones in the
  journey-reconstruction path; its per-journey allocation is heavier than Go's. (A
  perf-tuned Rust port could close this, but the goal was a faithful 1:1 port.)
- **Python** is the slowest of the fast four (~5–7× Go on setup, ~3.5× on query) —
  expected for CPython on an allocation- and dict-heavy workload.
- **Lin completes the full benchmark correctly and is now in the same order of magnitude
  as the dynamic languages** — ~23× slower than Go on query, ~6.6× slower than Python.
  This is after the typed `{ String: T }` hashed-map migration (ADR-082) eliminated the
  O(n) `Json` key-lookup bottleneck that `LIN_ISSUES.md` #4 identified: PREP dropped from
  ~145 s to ~30 s (~4.8×) and GROUP from ~295 s/query to ~4.2 s/query (~70×). The earlier
  "~1000×" figure and the 80 GB / many-hours behaviour (a separate O(n²)-sort bug) are
  both resolved — see `lin/NOTES.md`.
- **Lin's remaining cost is dominated by RANGE** (299.5 s of the 399 s query total): the
  profile-query loop re-plans repeatedly, advancing past each earliest departure, so it
  amplifies whatever per-scan cost remains. That scan cost — not dictionary lookup — is
  now the frontier for Lin on this workload.

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

# include lin (~6-7 min for the full LOAD+PREP+GROUP+RANGE workload):
benchmarks/compare/raptor/bench.sh
```

Each language's `bench` entry point:
- node:   `node bench.js [dataDir]`
- go:     `go run ./cmd/bench [dataDir]`
- rust:   `cargo run --release --bin bench -- [dataDir]`
- python: `python3 bench.py [dataDir]`
- lin:    `lin run bench.lin` (or build it; data dir hardcoded)

All emit the same `LOAD/PREP/GROUP/RANGE/DIGEST` line format, which `bench.sh` parses.
