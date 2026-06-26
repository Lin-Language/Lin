# RAPTOR — five-language port

A near-direct port of the [planarnetwork/raptor](https://github.com/planarnetwork/raptor)
Round-bAsed Public Transit Optimized Router (a GTFS journey planner) to **Lin, Go,
Rust, Python and Node.js**. Node.js is the golden reference (a faithful, type-stripped
copy of the original TypeScript); the other four are checked against the same unit
tests it passes.

This is the correctness foundation for an eventual cross-language *performance*
benchmark (driven by a real GTFS feed). **Start with the unit tests** — they are
thorough and encode every subtle semantic. The shared semantics every port must
honor are documented in [`PORTING_CONTRACT.md`](./PORTING_CONTRACT.md).

## What's ported

The full journey planner and the transfer-pattern subsystem — every module that has
a unit test in the reference:

- **GTFS**: `Service.runsOn` calendar logic, `TimeParser` (`HH:MM:SS` → seconds).
- **Raptor core**: `QueueFactory`, `RouteScanner` (+factory), `ScanResults`
  (+factory), `RaptorAlgorithm` (+factory).
- **Queries**: `DepartAfterQuery`, `GroupStationDepartAfterQuery` (multi-day
  stitching), `RangeQuery` (profile queries).
- **Results**: `JourneyFactory`, `MultipleCriteriaFilter`.
- **Transfer patterns**: `GraphResults` (DAG merge), `StringResults`.

Skipped (no unit tests / Node-only I/O): `GTFSLoader`, `TransferPatternQuery`,
`TransferPatternRepository`, the worker/CLI entry points, integration/perf tests.

## Running the tests

Each port mirrors the reference's `describe`/`it` names so failures are traceable to
a specific reference case. All run with the language's native test runner and exit
nonzero on failure.

| Language | From | Command | Cases |
|----------|------|---------|-------|
| Node.js  | `node/`   | `node --test "test/*.test.js"` (or `npm test`) | 48 |
| Go       | `go/`     | `go test ./...` | 48 |
| Rust     | `rust/`   | `cargo test` | 51¹ |
| Python   | `python/` | `python3 -m unittest discover` | 51¹ |
| Lin      | repo root | `target/debug/lin test benchmarks/compare/raptor/lin-manually-typed/` | 13 |

¹ Rust and Python add a few extra date-arithmetic sanity tests (`getDateNumber`,
day-of-week, month/year rollover); all 48 reference `it()` cases are covered in every
language.

The Lin suite uses the freshly built `lin` binary, so build the workspace first:
`cargo build --workspace`.

## Per-language notes

Each directory has a `NOTES.md` with the exact command, passing count, what was
skipped, and any language-specific decisions. The Lin notes also record a compiler
bug found during the port (a top-level `var` mutated by an exported function in an
imported module miscompiles) and its workaround.

## Toolchains (devcontainer)

Node 24, Go 1.26, Rust/cargo 1.95, Python 3.11, plus the `lin` toolchain. No external
test dependencies: Node uses built-in `node:test`, Python uses stdlib `unittest`
(pytest is not installed), Rust's only dependency is `indexmap`, Go is stdlib-only.
