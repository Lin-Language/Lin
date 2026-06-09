# std/csv

std/csv — RFC 4180 CSV reader and writer.

A CSV document is a sequence of records, each a sequence of String fields. Field values are
never coerced — every field is a String, since CSV has no types. Quoted fields, embedded
delimiters and newlines, and the `""` quote-escape are all handled per RFC 4180. The eager API
(`parse`/`parseWithHeader`/`stringify`/`stringifyRecords`) reads a whole document at once; the
streaming API (`rows`/`recordRows`) is a quote-aware row assembler over a byte stream.

## Reference

### options

#### `CsvOptions`

```lin
type CsvOptions = { "delimiter": String | Null, "trim": Boolean | Null }
```

Parser/serialiser options; both keys optional (an omitted key reads as `Null`, taking the
default). `delimiter` must be a single ASCII byte that is not `"`/CR/LF; `trim` strips ASCII
space/tab from unquoted fields.

### eager parse

#### `parse`

```lin
val parse = (s: String, opts: CsvOptions = {  }): String[][] | Error
```

Parse a CSV document into positional records (a list of rows, each a list of String fields).
Quoted fields, embedded delimiters and newlines, and the `""` quote-escape are handled per
RFC 4180; fields are never coerced, so every field is a String.
- **`s`** — the CSV text.
- **`opts`** — accepts `{ delimiter, trim }`. `delimiter` defaults to `,` and must be a single
             character that is not `"`, CR, or LF; `trim`, when true, strips leading and trailing
             ASCII spaces and tabs from unquoted fields.
- **Returns** the parsed rows, or an Error on an invalid delimiter option or malformed input (an
         unterminated quoted field or a stray quote).

**Example:**

```lin
parse("a,b\n1,2")  // [["a", "b"], ["1", "2"]]
```

### header records

#### `parseWithHeader`

```lin
val parseWithHeader = (s: String, opts: CsvOptions = {  }): { String: String }[] | Error
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
val stringify = (rows: String[][], opts: CsvOptions = {  }): String
```

Serialize positional rows to a CSV document. A field is quoted only when it contains the
delimiter, a `"`, a CR, or an LF (embedded `"` is doubled); rows are LF-terminated.
- **`rows`** — the rows to render (each a list of String fields).
- **`opts`** — accepts `{ delimiter }`; the delimiter defaults to `,`.
- **Returns** the CSV text, or "" for no rows.

#### `stringifyRecords`

```lin
val stringifyRecords = (records: { String: String }[], opts: CsvOptions = {  }): String
```

Serialize keyed records to a CSV document with a header row. The header is the union of all keys
across all records; a record missing a column emits an empty field.
- **`records`** — the records to render.
- **`opts`** — accepts `{ delimiter }`; the delimiter defaults to `,`.
- **Returns** the CSV text (header line first), or "" for no records. Column order follows the
         record map's key-iteration order — deterministic, but not necessarily first-seen.

### streaming adapters

#### `rows`

```lin
val rows = (src: Stream<UInt8[]>, opts: CsvOptions = {  }): Stream<String[]>
```

Lazily parse a byte stream into a stream of CSV rows (`String[]` per row), quote-aware so a
quoted field may span newlines without splitting the row. Memory is bounded; a malformed input
(such as an unterminated quote) surfaces as an `Error` at the terminal.
- **`src`** — the source byte stream (e.g. from `readStream`).
- **`opts`** — accepts `{ delimiter, trim }`; the delimiter defaults to ",".
- **Returns** a `Stream<String[]>` of rows.

#### `recordRows`

```lin
val recordRows = (src: Stream<UInt8[]>, opts: CsvOptions = {  }): Stream
```

Lazily parse a byte stream into a stream of keyed records, using the first row as the header.
Each subsequent row becomes a record keyed by the header columns. The header is consumed on the
first pull and held for the life of the pipeline.
- **`src`** — the source byte stream.
- **`opts`** — accepts `{ delimiter, trim }`; the delimiter defaults to ",".
- **Returns** a `Stream` yielding `{ String: String }` records.
