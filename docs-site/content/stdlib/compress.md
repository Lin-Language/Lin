# std/compress

Streaming gzip/DEFLATE codecs over a byte stream. Each is a **lazy adapter** in a [`std/stream`](/stdlib/stream.html) pipeline: it wraps an upstream `Stream<UInt8[]>` and (de)compresses bytes **incrementally** — one upstream chunk in, whatever output bytes the codec produced out — so a multi-gigabyte file flows through in constant memory. A decode/encode fault is threaded **in-band**: the first such error short-circuits straight to the terminal op as the canonical `Error` value, exactly like every other stream adapter (no `is Error` check between steps).

Two container formats are supported: `gzip`/`gunzip` use the **gzip** container (header + CRC32 + length trailer — what a `.gz` file holds); `deflate`/`inflate` use the **raw DEFLATE** bitstream (no header, no CRC).

```lin
import { readStream, writeStream, drain } from "std/stream"
import { gunzip } from "std/compress"

// Decompress a .gz file to disk as a streaming pipeline:
readStream("data.txt.gz")
  .gunzip()
  .writeStream("data.txt")
  .drain()
```

> These four codecs are **adapters** — they read nothing on their own. They sit between a source (`readStream`, or any other `Stream<UInt8[]>` source) and a terminal op (`drain`/`collect`/`promise`/`untar`), and only move bytes once that terminal drives the pipeline. They compose freely with the [`std/iter`](/stdlib/iter.html) combinators and the [`std/archive`](/stdlib/archive.html) tar splitter.

## Round-trip

`gzip` and `gunzip` are exact inverses, as are `deflate` and `inflate` — compressing then decompressing a stream reproduces the original bytes:

```lin
import { readStream, writeStream, drain } from "std/stream"
import { gzip, gunzip } from "std/compress"

readStream("report.csv").gzip().writeStream("report.csv.gz").drain()
readStream("report.csv.gz").gunzip().writeStream("report.csv").drain()  // == original
```

## Types

```lin
Stream<UInt8[]>   // opaque byte stream; the I/O currency for all four codecs
```

## Function reference

| Function | Signature | Role | Description |
| --- | --- | --- | --- |
| `gunzip` | `(Stream<UInt8[]>) -> Stream<UInt8[]>` | adapter | Decompress a gzip-framed byte stream |
| `gzip` | `(Stream<UInt8[]>) -> Stream<UInt8[]>` | adapter | Compress a byte stream into the gzip container |
| `inflate` | `(Stream<UInt8[]>) -> Stream<UInt8[]>` | adapter | Decompress a raw DEFLATE byte stream |
| `deflate` | `(Stream<UInt8[]>) -> Stream<UInt8[]>` | adapter | Compress a byte stream as raw DEFLATE |

---

### `gunzip`

Decompresses a **gzip-framed** byte stream, yielding the decompressed bytes as a new byte stream. Invalid gzip input (bad header, truncated frame, CRC mismatch) surfaces an `Error` in-band at the terminal op.

Decompress, decode as lines, and filter — all lazily, in bounded memory:

```lin
import { readStream } from "std/stream"
import { gunzip } from "std/compress"
import { filter, for } from "std/iter"
import { println } from "std/io"

readStream("access.log.gz")
  .gunzip()
  .lines()
  .filter(line => line.contains("ERROR"))
  .for(println)
```

---

### `gzip`

Compresses a byte stream into the **gzip container** format (the bytes a `.gz` file would hold). Use the raw [`writeStream`](/stdlib/stream.html) sink — never `writeLines` — so the compressed bytes land on disk verbatim; any injected separator would corrupt the `.gz` framing:

```lin
import { readStream, writeStream, drain } from "std/stream"
import { gzip } from "std/compress"

readStream("data.txt")
  .gzip()
  .writeStream("data.txt.gz")   // RAW sink — correct for binary
  .drain()
```

---

### `inflate`

Decompresses a **raw DEFLATE** byte stream — a bare DEFLATE bitstream with no gzip header or CRC trailer. Invalid input surfaces an `Error` in-band at the terminal op. Use this (not `gunzip`) for DEFLATE payloads embedded in other formats.

```lin
import { readStream, writeStream, drain } from "std/stream"
import { inflate } from "std/compress"

readStream("payload.deflate").inflate().writeStream("payload").drain()
```

---

### `deflate`

Compresses a byte stream as a **raw DEFLATE** bitstream — no gzip header, no CRC. The counterpart to `inflate`, for producing bare DEFLATE payloads.

```lin
import { readStream, writeStream, drain } from "std/stream"
import { deflate } from "std/compress"

readStream("payload").deflate().writeStream("payload.deflate").drain()
```

---

## See also

- [`std/stream`](/stdlib/stream.html) — the sources, sinks, and terminals these codecs plug into. Use the raw `writeStream` sink for compressed output.
- [`std/archive`](/stdlib/archive.html) — tar splitting; a `.tar.gz` is just `gunzip()` composed with the tar splitter.
- [`std/iter`](/stdlib/iter.html) — the combinators (`map`/`filter`/`for`/…) that drive byte streams lazily.
