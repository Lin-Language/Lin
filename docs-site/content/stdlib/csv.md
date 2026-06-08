# std/csv

std/csv — RFC 4180 CSV reader and writer (csv proposal, docs/proposals/stdlib/csv.md).

A CSV document is a sequence of records, each a sequence of String fields. std/csv never
coerces field values — every field is a String (CSV has no types). Quoted fields, embedded
delimiters/newlines, and the `""` quote-escape are all handled per RFC 4180. The eager API
(`parse`/`parseWithHeader`/`stringify`/`stringifyRecords`) is a PURE-LIN state machine; the
streaming API (`rows`/`recordRows`) is a quote-aware row assembler over a byte Stream.

SCANNING: the structural delimiters (`,` `"` `\r` `\n` and a single-codepoint custom delimiter)
are ASCII, and an ASCII byte (< 0x80) never collides with a UTF-8 continuation byte. So the
parser scans RAW BYTES with the O(1) `byteAt` and slices fields by byte offset (each slice then
decodes as valid UTF-8 as a whole). `lin_string_length` is a BYTE length, so the scan bound and
the slice offsets live in the same byte-index space — the parser is O(n), not the O(n²) trap a
`codePointAt`-driven scan would be (see std/string.byteAt).

TCO-BUG WORKAROUND: a tail-recursive function that returns an owned array PARAM on one terminal
branch and an OBJECT on another corrupts its output (the TCO owned-param release bug, being fixed
separately). To sidestep it, the recursive record scanner ALWAYS returns ONE consistent object
shape — a cursor `{ "fields": String[], "pos": Int32, "ok": Boolean }` — and signals a malformed
field through the `ok` flag (a separate channel), never returning a bare array on one branch and
an Error object on another. The canonical `Error` value is built only in the NON-recursive outer
`parse` wrapper.

## Reference

### eager parse

#### `parse`

```lin
val parse = (s: String, opts: Json = {  }): String[][] | Error
```

Parse a CSV document into positional records (a list of rows, each a list of String fields).
Quoted fields, embedded delimiters/newlines, and the `""` quote-escape are handled per RFC 4180;
fields are never coerced (every field is a String).
- **`s`** — the CSV text.
- **`opts`** — optional `{ "delimiter": String, "trim": Boolean }`; delimiter defaults to `,` and
             must be a single character that is not `"`, CR, or LF; trim strips leading/trailing
             ASCII spaces/tabs from unquoted fields.
- **Returns** the parsed rows, or an Error on an invalid delimiter option or malformed input (an
         unterminated quoted field or a stray quote).
- **Example:** parse("a,b\n1,2")  // [["a", "b"], ["1", "2"]]

### header records

#### `parseWithHeader`

```lin
val parseWithHeader = (s: String, opts: Json = {  }): { String: String }[] | Error
```

Parse a CSV document whose first row is a header, mapping each subsequent data row to a
`{ header: field }` record. Last-wins on duplicate headers; lenient on ragged rows (short rows
omit trailing keys, extra fields are dropped).
- **`s`** — the CSV text (first row treated as the header).
- **`opts`** — optional options, as for `parse`.
- **Returns** the data rows as keyed records (empty if there is only a header / no rows), or an Error
         if the underlying parse fails.

### stringify

#### `stringify`

```lin
val stringify = (rows: String[][], opts: Json = {  }): String
```

Serialize positional rows to a CSV document. Each field is quoted iff it contains the delimiter,
a `"`, a CR, or an LF (embedded `"` doubled); rows are LF-terminated.
- **`rows`** — the rows to render (each a list of String fields).
- **`opts`** — optional `{ "delimiter": String }`; delimiter defaults to `,`.
- **Returns** the CSV text, or "" for no rows.

#### `stringifyRecords`

```lin
val stringifyRecords = (records: { String: String }[], opts: Json = {  }): String
```

Serialize keyed records to a CSV document with a header row. The header is the union of all keys
across all records; a record missing a column emits an empty field.
- **`records`** — the records to render.
- **`opts`** — optional `{ "delimiter": String }`; delimiter defaults to `,`.
- **Returns** the CSV text (header line first), or "" for no records. NOTE: column order follows the
         runtime map's key-iteration order (deterministic, but not necessarily first-seen — see
         the collectKeys note below).

### streaming adapters

#### `rows`

```lin
val rows = (src: Stream, opts: Json = {  }): Stream
```

Lazily parse a byte stream into a stream of CSV rows (`String[]` per row), quote-aware so a
quoted field may span newlines without splitting the row. Bounded memory; a malformed (e.g.
unterminated-quote) input surfaces as an `Error` at the terminal.
- **`src`** — the source byte stream (e.g. from `readStream`).
- **`opts`** — optional `{ delimiter, trim }` (delimiter defaults to ",").
- **Returns** a `Stream` of `String[]` rows.

#### `recordRows`

```lin
val recordRows = (src: Stream, opts: Json = {  }): Stream
```

Lazily parse a byte stream into a stream of keyed records, using the first row as the header.
Each subsequent row becomes a `{ String: String }` keyed by the header columns.
- **`src`** — the source byte stream.
- **`opts`** — optional `{ delimiter, trim }` (delimiter defaults to ",").
- **Returns** a `Stream` of `{ String: String }` records.
