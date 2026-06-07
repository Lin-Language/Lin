# report — CSV → validated report generator

Reads `"name, score"` CSV rows **out of a gzipped tar of CSV files**, validates
each one, computes statistics over the valid records, and renders a text report.
Malformed rows are reported, not fatal. A realistic data pipeline that exercises
`map`/`filter`/`reduce`/`sortBy` churn over heap (object) records — fed by a
streaming, constant-memory decompress + untar of the input archive.

## What it demonstrates

- **Streaming archive input** (`std/compress` + `std/archive`): the input is a
  `.tar.gz` of CSV files, read via `readStream(path).gunzip().untar((meta, data) => …)`.
  Each member's body arrives as a `data` sub-stream pulled to text on the fly, so no
  step holds the whole archive — or even a whole member — in memory at once. A
  `manifest()` pass lists the members without extracting anything.
- **Lazy `Stream<T>` pipelines** (`std/stream`): a plain (uncompressed) CSV file is
  read as a line stream and transformed file-to-file —
  `readStream(p).lines().map(trim).filter(nonBlank).writeLines(out).drain()` — holding
  at most one line in memory. `drain()` returns `Null | Error`; a failed source threads
  the `Error` in-band.
- **Named type aliases + record intersection (`&`, ADR-061)**: `Record`
  (`{ name, score }`), `Stats`; the tagged result factors a shared discriminant into
  `Tagged = { "type": String }` and extends it per variant —
  `Success = Tagged & { value }`, `Failure = Tagged & { error }`.
- **Tagged-union results**: `Parsed = Success | Failure`, consumed with
  `has { "type": "success", ... }` pattern matching.
- **Typed dictionaries (`{ String: T }`, ADR-082)**: a grade-band histogram
  (`frequency.lin`) tallies records per letter band into `{ String: Int32 }`; `std/hash`
  gives an O(1) structural-key dedup set (`{ String: Boolean }`) in `uniqueRecords`.
- **Indexed combinators**: the ranked report rows are numbered with the 2-arg
  `map((r, i) => …)` form (the 0-based source index, opt-in by arity).
- **`std/path`**: the cleaned-output path is derived with `dirname`/`stem`/`join`
  rather than string concatenation.
- **Typed array pipelines**: `String[]` lines flow through `map`/`filter` into
  `Record[]`, then `reduce`/`sortBy` to statistics — all with precise element types.
- String interpolation and multi-line report rendering.

## Structure

| File | What it is |
| --- | --- |
| `parse.lin` | One-line CSV parsing + validation. `parseRow(line)` returns a `Parsed` result. Owns `Record`, `Tagged`, `Success`, `Failure`, `Parsed`. |
| `report.lin` | The batch pipeline: `validRecords`, `uniqueRecords`, `parseErrors`, `stats`, `render`. Owns `Stats`. |
| `frequency.lin` | The grade-band histogram over `{ String: Int32 }`: `band`, `tally`, `renderTally`. |
| `source.lin` | Where the rows come from. Archive path: `rowsFromArchive`/`archiveMembers` (streaming `gunzip` + `untar`). Plain-file path: `rowsFromFile` (lazy line stream), `normalizeCsv` (lazy file-to-file clean), `cleanedPath` (std/path). |
| `report-data.tar.gz` | The input fixture: a gzipped tar of `q1.csv` / `q2.csv`. |
| `main.lin` | Lists the archive members, renders the archive report, then normalises + renders a plain CSV file. |
| `report.test.lin` | Unit tests: parsing, the pipeline, dedup, edge cases, and a larger batch (an RC/ASan guard). |
| `frequency.test.lin` | Unit tests for the band thresholds, tally, and rendered histogram. |
| `source.test.lin` | Tests both input paths: archive listing/extraction, the plain-file line stream, `normalizeCsv`, and the missing-input degrade paths. |

The `data` sub-stream handed to the `untar` callback is **sync-only** — valid only
during that callback (it shares the archive's single read cursor), so `source.lin`
drains it there with `readText` rather than letting it escape.

The discriminant field is typed `String` (not a string-literal singleton, which
the type system does not support); the runtime shape is unchanged.

## Run / Test

```sh
lin run  examples/report/main.lin
lin test examples/report/
```
