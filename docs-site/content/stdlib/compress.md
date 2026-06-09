# std/compress

std/compress — streaming gzip/deflate byte-adapters over `Stream<UInt8[]>`.

Each adapter is lazy: it wraps an upstream byte stream and (de)compresses incrementally — one
upstream chunk in, whatever output bytes were produced out — so a multi-gigabyte file flows
through in constant memory. A decode/encode fault surfaces in-band as the canonical `Error`
value at the terminal op (no `is Error` between steps), exactly like every other adapter.

Two framings are supported: `gunzip`/`gzip` handle the gzip container format (header, CRC32 and
ISIZE trailer), while `inflate`/`deflate` handle the raw DEFLATE bitstream (no gzip header or
CRC). Build a pipeline with std/stream as usual:

```lin
import { readStream, writeStream, drain } from "std/stream"
import { gunzip } from "std/compress"
```

```lin
val pipeline = readStream("data.gz")
  .gunzip()
  .writeStream("data.txt")
drain(pipeline)
```

## Reference

#### `gunzip`

```lin
val gunzip = (s: Stream<UInt8[]>): Stream<UInt8[]>
```

Adapter: decompress a gzip-framed byte stream. Lazy.
- **`s`** — the upstream gzip-framed byte stream.
- **Returns** a `Stream<UInt8[]>` of decompressed chunks; a decode fault surfaces in-band as `Error`.

#### `gzip`

```lin
val gzip = (s: Stream<UInt8[]>): Stream<UInt8[]>
```

Adapter: compress a byte stream into the gzip container format (header + CRC32 + ISIZE). Lazy.
- **`s`** — the upstream byte stream.
- **Returns** a `Stream<UInt8[]>` of gzip-framed chunks.

#### `inflate`

```lin
val inflate = (s: Stream<UInt8[]>): Stream<UInt8[]>
```

Adapter: decompress a raw DEFLATE byte stream (no gzip header/CRC). Lazy.
- **`s`** — the upstream raw-DEFLATE byte stream.
- **Returns** a `Stream<UInt8[]>` of decompressed chunks; a decode fault surfaces in-band as `Error`.

#### `deflate`

```lin
val deflate = (s: Stream<UInt8[]>): Stream<UInt8[]>
```

Adapter: compress a byte stream as a raw DEFLATE bitstream (no gzip header/CRC). Lazy.
- **`s`** — the upstream byte stream.
- **Returns** a `Stream<UInt8[]>` of raw-DEFLATE chunks.
