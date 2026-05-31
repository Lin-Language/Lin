# report — CSV → validated report generator

Reads `"name, score"` CSV rows, validates each one, computes statistics over the
valid records, and renders a text report. Malformed rows are reported, not fatal.
A realistic data pipeline that exercises `map`/`filter`/`reduce`/`sortBy` churn
over heap (object) records.

## What it demonstrates

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
| `main.lin` | A sample batch; prints `render(lines)`. |
| `report.test.lin` | Unit tests: row parsing, the pipeline, edge cases, and a larger batch (an RC/ASan guard). |

The discriminant field is typed `String` (not a string-literal singleton, which
the type system does not support); the runtime shape is unchanged.

## Run / Test

```sh
lin run  examples/report/main.lin
lin test examples/report/
```
