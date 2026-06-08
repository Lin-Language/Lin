## Status: proposal

# std/csv

CSV is the one ubiquitous interchange format Lin's standard library does not yet handle correctly. Today the docs' own examples reach for `line.split(",")`, which is wrong the moment a field contains a comma, a newline, or a quote — exactly the cases real-world CSV (spreadsheet exports, bank statements, GTFS feeds) is full of. `std/csv` provides an [RFC 4180](https://www.rfc-editor.org/rfc/rfc4180)-correct reader and writer: quoted fields, embedded delimiters/newlines, and `""` quote-escaping, with a custom-delimiter option that covers TSV and friends. Crucially it fits Lin's existing data model (rows are `String[]`, header records are `{ String: String }`) and — the headline feature — it composes with [`std/stream`](../../STDLIB.md#stdstream) so a multi-gigabyte CSV parses lazily, row by row, in bounded memory. Java and Node ship no CSV parser at all; Python's `csv` and Go's `encoding/csv` are the bar, and a streaming row adapter built on Lin's lazy pull graph clears it.

---

## Import

```txt
import { parse, parseWithHeader, stringify, stringifyRecords } from "std/csv"
import { rows, recordRows } from "std/csv"   // stream adapters
```

## Data model

A CSV file is a sequence of **records**, each a sequence of string **fields**. `std/csv` never coerces field values — every field is a `String` (CSV has no types). Numeric or boolean interpretation is the caller's job (`field.parseInt32()`, `field.parseFloat64()`), keeping the module a faithful codec and leaving typed decoding to the existing `std/json` `fromJson` path if desired.

- `parse` / `rows` yield positional rows: `String[]`.
- `parseWithHeader` / `recordRows` yield keyed records: `{ String: String }`.

Fallible functions return the canonical `Error` value (`{ "type": "error", "message": String }`, detected with `is Error`), per the house convention. `stringify` and `stringifyRecords` are **total** (any string array round-trips) and so return a plain `String`.

## Options

A trailing options object, following the `std/fs` `{ "recursive": true }` precedent. All keys are optional; an omitted key takes its default.

```txt
type CsvOptions = {
  "delimiter": String,    // field separator, default ","  (use "\t" for TSV)
  "trim":      Boolean     // trim ASCII whitespace around UNQUOTED fields, default false
}
```

- **delimiter** must be a single codepoint that is not `"` , `\r`, or `\n`; otherwise the parse/stringify call returns/raises an `Error`. This one knob delivers TSV (`{ "delimiter": "\t" }`), semicolon CSV (the European spreadsheet dialect), and pipe-delimited files.
- **trim** (default `false` — RFC 4180 treats whitespace as significant) trims leading/trailing spaces and tabs from **unquoted** fields only. Whitespace inside a quoted field is always preserved verbatim, because quoting is the explicit "this is data" signal. This mirrors Go's `TrimLeadingSpace` being opt-in and avoids silently corrupting fixed-width-padded exports.

Every reader/writer below has an `opts`-less and an `opts` form; the `opts`-less form is the RFC 4180 default (comma delimiter, no trimming).

---

## Eager API

### parse

```txt
val parse: (s: String[, opts: CsvOptions]) -> String[][] | Error
```

Parses an entire CSV document into rows of string fields. Handles quoted fields, embedded delimiters, embedded `\n`/`\r\n`, and the `""` escape for a literal quote inside a quoted field. Both `\n` and `\r\n` line endings are accepted; a trailing newline does **not** produce a spurious empty final row. An empty input parses to `[]`. Returns an `Error` for malformed input — an unterminated quoted field (EOF inside quotes), or a stray quote in the middle of an unquoted field.

```txt
parse("a,b,c\n1,2,3\n")
// [["a", "b", "c"], ["1", "2", "3"]]

parse("name,quote\nAda,\"she said \"\"hi\"\"\"\n")
// [["name", "quote"], ["Ada", "she said \"hi\""]]

parse("a;b\n1;2\n", { "delimiter": ";" })
// [["a", "b"], ["1", "2"]]
```

**Ragged rows are preserved, not padded.** `parse` is positional and faithful: a row with three fields stays length 3 even if its neighbours have four. Validation of row width is a caller concern (`rows.every(r => r.length() == header.length())`).

---

### parseWithHeader

```txt
val parseWithHeader: (s: String[, opts: CsvOptions]) -> { String: String }[] | Error
```

Parses CSV whose **first record is a header row**, returning one `{ String: String }` map per data row, keyed by the header field names. Convenient for column-addressed access (`row["email"]`) without tracking positions.

```txt
parseWithHeader("name,age\nAda,36\nBob,30\n")
// [{ "name": "Ada", "age": "36" }, { "name": "Bob", "age": "30" }]
```

Empty input, or input with only a header row and no data rows, returns `[]`. An `Error` from the underlying parse (malformed quoting) propagates unchanged.

**Duplicate header names — last wins.** If the header contains the same name twice (`a,b,a`), the rightmost column owns the key; the earlier same-named column's value is shadowed. This is the only stable single-map outcome and matches Python's `DictReader`. A caller who needs every column must use positional `parse`. (An alternative — returning an `Error` on duplicate headers — is rejected: duplicate headers occur in real exports and should not be fatal.)

**Ragged rows — short rows omit keys, extra fields are dropped.** A data row with fewer fields than the header produces a map missing the trailing keys (a missing column reads back as `Null` via the `{ String: String }` index, which the language already returns for an absent key). A data row with *more* fields than the header drops the surplus, unkeyed fields. Neither is an `Error`: lenient record mapping is what makes the header form useful on messy data. Callers needing strictness use `parse` and check widths.

---

### stringify

```txt
val stringify: (rows: String[][][, opts: CsvOptions]) -> String
```

Serialises rows of fields to an RFC 4180 CSV `String`, CRLF-free (`\n` line endings; this is the modern Unix-correct default and round-trips through `parse`). **A field is quoted iff it contains the delimiter, a `"`, a `\r`, or a `\n`** — minimal quoting, so clean data stays unquoted and readable. A `"` inside a quoted field is escaped by doubling (`""`). Total: every `String` round-trips.

```txt
stringify([["a", "b"], ["1", "2"]])
// a,b
// 1,2

stringify([["plain", "has,comma", "has\"quote", "has\nnewline"]])
// plain,"has,comma","has""quote","has
// newline"
```

A trailing `\n` is emitted after the final row (so the output is a well-formed line-oriented file and concatenation/append is clean).

---

### stringifyRecords

```txt
val stringifyRecords: (records: { String: String }[][, opts: CsvOptions]) -> String
```

Serialises an array of record maps to CSV, **emitting a header row** derived from the keys. The output round-trips through `parseWithHeader`.

**Column ordering: insertion order of the first record's keys, then any new keys appended in first-seen order as later records introduce them.** Lin's typed maps preserve insertion order (the same property `std/yaml`/`std/json` stringify rely on for stable output), so this is deterministic and matches what the author wrote. The header is the union of all keys across all records, in first-seen order; a record missing a column emits an empty field for it.

```txt
stringifyRecords([
  { "name": "Ada", "age": "36" },
  { "name": "Bob", "age": "30" }
])
// name,age
// Ada,36
// Bob,30

// Union-of-keys, missing column -> empty field:
stringifyRecords([{ "a": "1" }, { "a": "2", "b": "y" }])
// a,b
// 1,
// 2,y
```

An empty `records` array stringifies to `""` (no header, no body — there are no keys to derive a header from).

---

## Lazy / streaming API

The reason to build CSV in Lin specifically: it slots into the [`std/stream`](../../STDLIB.md#stdstream) lazy pull graph, so an arbitrarily large file parses one record at a time with bounded memory, using the same `std/iter` combinator vocabulary as everything else.

### rows

```txt
val rows: (src: Stream<UInt8[]>[, opts: CsvOptions]) -> Stream<String[]>
```

A lazy **adapter** (not a terminal): views a byte stream as a stream of parsed CSV **rows** (`String[]`). It reads nothing until a terminal (`for`/`reduce`/`drain`/`collect`-via-iter) drives it. A parse fault (unterminated quote at EOF) surfaces **in-band** as an `Error` at the terminal, exactly like every other stream adapter — error handling lives only at the terminal.

```txt
import { rows } from "std/csv"
import { readStream } from "std/stream"
import { map, drop, filter, for } from "std/iter"

// Stream a huge CSV: skip header, keep active users, print emails — bounded memory.
val outcome = readStream("users.csv")
  .rows()                                  // Stream<String[]>
  .drop(1)                                 // skip header row
  .filter(r => r[3] == "active")
  .map(r => r[1])                          // Stream<String>
  .for(email => print(email))              // terminal: Null | Error

match outcome
  is Error => print("parse failed: ${outcome["message"]}")
  else     => null
```

### recordRows

```txt
val recordRows: (src: Stream<UInt8[]>[, opts: CsvOptions]) -> Stream<{ String: String }>
```

Like `rows`, but **consumes the first record as the header** and yields one `{ String: String }` map per subsequent data row — the streaming analogue of `parseWithHeader`, with the same duplicate-header (last-wins) and ragged-row (lenient) rules. The header is read once, lazily, on the first pull and held for the life of the pipeline.

```txt
readStream("transactions.csv")
  .recordRows()                            // Stream<{ String: String }>
  .filter(t => t["status"] == "settled")
  .reduce(0.0, (sum, t) => sum + t["amount"].parseFloat64())   // Float64 | Error
```

### The multi-line-field subtlety (and why `rows` reads bytes, not lines)

A naive `readStream(...).lines().map(parseLine)` is **wrong** for CSV, because a single quoted field may contain `\n` and therefore span several physical lines:

```txt
id,note
1,"line one
line two"
```

Here line two of the file is the *middle* of one record, not a record of its own — a line-stream is not row-aligned. Two design choices follow:

1. **`rows`/`recordRows` take a `Stream<UInt8[]>` (the byte source), not a `Stream<String>`.** They run a small stateful row-assembler over the byte stream directly: it tracks whether the scanner is currently *inside* a quoted field, and only emits a completed row when a record-terminating newline is seen **outside** quotes. A newline seen *inside* quotes is buffered as field content. This is the only correct way to re-align records, and it keeps memory bounded to a single in-flight record rather than the whole file.
2. Because the assembler is the source of record boundaries, **it cannot be expressed as a stateless `.lines().map(...)`** — it is a genuine stream adapter with carry-over state (the partial-record buffer and the in-quote flag), in the same family as `lines`/`chunks`. An unterminated quote at end-of-stream is the in-band `Error` (mirroring `lines`' newline-less-input cap), and like `linesMax` it caps the in-flight record buffer (default 64 MiB) so a runaway quoted field fails rather than buffering unbounded.

This is the killer feature: the *only* CSV reader most ecosystems offer either loads the whole file (`parse`) or forces the user to hand-roll quote-aware line reassembly. Lin gets streaming, quote-correct, bounded-memory CSV for free from the existing pull-graph machinery.

> **Single-use, like all streams.** A `Stream<String[]>` from `rows` obeys the [`std/stream`](../../STDLIB.md#stdstream) single-use rule — it flows through one pipeline and lives in a `val`/parameter/return, not an object field or `var`. To make two passes, open a fresh `readStream`.

There is **no `writeRows` terminal in `std/csv`**: stringify a row to a `String` and feed the existing `writeLines` sink. This keeps the sink side entirely in `std/stream` and avoids a redundant CSV-specific writer node.

```txt
import { stringify } from "std/csv"
import { readStream, writeLines, drain } from "std/stream"
import { rows, map } from "std/iter"   // map from std/iter

readStream("in.tsv")
  .rows({ "delimiter": "\t" })                       // parse TSV
  .map(r => stringify([r]).trimEnd())                // re-emit each row as CSV (one row)
  .writeLines("out.csv")
  .drain()
```

---

## Implementation notes

**Pure-Lin parser, not a Rust `csv`-crate intrinsic — recommended.** Unlike `std/yaml` and `std/jq` (which wrap mature, large Rust crates because YAML and jq are genuinely hard and not worth re-implementing), RFC 4180 CSV is a tiny state machine — five states (field-start, in-unquoted, in-quoted, after-quote, record-end) over a byte/codepoint scan. Writing it in pure Lin has decisive advantages here:

- **It is the streaming adapter.** The lazy `rows` adapter *must* run inside Lin's pull graph with carry-over state between pulls. A monolithic Rust `csv::Reader` intrinsic would parse a whole `String` eagerly and could not yield the bounded-memory `Stream<String[]>` — the headline feature — without re-implementing the pull protocol across the FFI boundary anyway. One pure-Lin state machine serves *both* the eager (`parse`) and lazy (`rows`) paths: `parse` runs it to completion over the full string; `rows` steps it as bytes arrive.
- **No new runtime crate, no FFI marshalling.** Returning `String[][]` / `{ String: String }[]` from Rust means boxing arrays of arrays of Lin strings across the boundary — exactly the kind of allocation-heavy marshalling these memory notes flag as costly. Building the values in Lin keeps them native.
- **It dogfoods the language.** CSV is the textbook string-scanning workload; a pure-Lin implementation is a credibility example for the stdlib.

**Use `byteAt`, not `codePointAt`, for the scan.** The structural delimiters CSV cares about — `,` `"` `\r` `\n` and a single-codepoint custom delimiter — are all ASCII (the custom-delimiter validation should reject a multi-byte delimiter, or fall back to a codepoint scan only for that one comparison). Per the `charCode O(n²) → byteAt` finding, `codePointAt`/`charCode` are O(n)-per-call, so a `codePointAt`-driven scan is O(n²) and would make CSV parsing pathologically slow on large files. Scan the UTF-8 bytes with the O(1) `byteAt`: ASCII structural bytes (< 0x80) are unambiguous in UTF-8 (a continuation byte never collides with an ASCII delimiter), so the scanner advances byte-by-byte and slices field substrings by byte offset, then the slice decodes as UTF-8 as a whole. This makes the parser O(n) and multibyte-safe without per-codepoint cost.

**Streaming row-adapter mechanics.** Model `rows` as a stream node holding (a) a growing byte buffer of the partial record, (b) the parser state (in particular the *in-quote* flag) carried across `UInt8[]` chunks, and (c) a queue of completed rows decoded from the current buffer. On each upstream pull it appends the incoming chunk, runs the state machine forward consuming as many complete records as the buffer now contains, emits them one at a time to the downstream terminal, and retains only the unterminated tail. A record completes only on a newline encountered **outside** quotes; a newline inside quotes is ordinary field content. At EOF: a clean buffer with a final partial record emits that last record; a buffer still *inside* a quoted field is the in-band `Error` (unterminated quote). The in-flight buffer is capped (default 64 MiB, à la `linesMax`) so adversarial input fails instead of OOMing. `recordRows` wraps `rows`: it pulls and consumes exactly one row as the header on first drive, then maps each subsequent `String[]` to a `{ String: String }` using the cached header.

**Generics / dispatch constraints.** `rows` and `recordRows` are *sources/adapters* exported from `std/csv` (like `readStream` from `std/stream`), not `std/iter` combinators — they take a concrete `Stream<UInt8[]>` receiver and return a concrete `Stream<String[]>` / `Stream<{ String: String }>`, so they sidestep the documented v1 limitation that a `Stream` passed through a *user-defined generic `Iterable` parameter* falls back to eager array shape. Downstream of `rows`, the standard `std/iter` combinators dispatch lazily on the concrete `Stream<…>` receiver exactly as documented. The eager `parse`/`stringify` signatures are monomorphic over `String` (no type parameters — fields are always `String`), so they need no generic inference machinery and avoid the argument-driven-inference and return-only-type-param pitfalls noted for Lin generics. The options object is a plain `Json`-shaped record read with the `{ "recursive": ... }` pattern `std/fs` already uses, so it needs no new typing support.

**Round-trip guarantee to test.** `parse(stringify(rows)) == rows` for any `String[][]`, and `parseWithHeader(stringifyRecords(recs))` equals `recs` up to the union-of-keys / missing-field-as-empty normalisation. These two properties are the acceptance bar.
