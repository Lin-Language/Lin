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
- **Named type aliases** for record shapes: `Record` (`{ name, score }`) and
  `Stats` (`{ count, total, average, top }`).
- **Tagged-union results**: `Parsed = Success | Failure`, distinguished by a
  `String` `"type"` discriminant and consumed with `has { "type": "success", ... }`
  pattern matching.
- **Typed array pipelines**: `String[]` lines flow through `map`/`filter` into
  `Record[]`, then `reduce`/`sortBy` to statistics — all with precise element types.
- String interpolation and multi-line report rendering.

## Structure

| File | What it is |
| --- | --- |
| `parse.lin` | One-line CSV parsing + validation. `parseRow(line)` returns a `Parsed` result. Owns `Record`, `Success`, `Failure`, `Parsed`. |
| `report.lin` | The batch pipeline: `validRecords`, `parseErrors`, `stats`, `render`. Owns `Stats`. |
| `source.lin` | Reads the CSV rows out of `report-data.tar.gz` via streaming `gunzip` + `untar`: `rowsFromArchive` (all rows) and `archiveMembers` (listing). |
| `report-data.tar.gz` | The input fixture: a gzipped tar of `q1.csv` / `q2.csv`. |
| `main.lin` | Lists the archive members, then prints `render(rowsFromArchive(archive))`. |
| `report.test.lin` | Unit tests: row parsing, the pipeline, edge cases, and a larger batch (an RC/ASan guard). |
| `source.test.lin` | Tests the streaming archive input: listing, row extraction, end-to-end render, and the missing-archive degrade-to-empty path. |

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
