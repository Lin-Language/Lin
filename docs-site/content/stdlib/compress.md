# std/compress

std/compress — streaming gzip/deflate byte-adapters over `Stream<UInt8[]>` (ADR-047 streams).

Each adapter is LAZY: it wraps an upstream byte stream and (de)compresses incrementally — one
upstream chunk in, whatever output bytes were produced out — so a multi-gigabyte file flows
through in constant memory. A decode/encode fault surfaces IN-BAND as the canonical `Error`
value at the terminal op (no `is Error` between steps), exactly like every other adapter.

  gunzip(s) / gzip(s)     — the gzip container format (header + CRC32 + ISIZE trailer).
  inflate(s) / deflate(s) — raw DEFLATE bitstream (no gzip header/CRC).

Build a pipeline with std/stream + std/iter as usual, e.g.
  import { readStream, writeStream, drain } from "std/stream"
  import { gunzip } from "std/compress"
  readStream("data.gz").gunzip().writeStream("data.txt").drain()
Adapter: decompress a gzip-framed byte stream. LAZY.
- **`s`** — the upstream gzip-framed byte stream.
- **Returns** a `Stream<UInt8[]>` of decompressed chunks; a decode fault surfaces in-band as `Error`.

## Reference

#### `gunzip`

```lin
val gunzip = (s: Stream): Stream
```


#### `gzip`

```lin
val gzip = (s: Stream): Stream
```

Adapter: compress a byte stream into the gzip container format (header + CRC32 + ISIZE). LAZY.
- **`s`** — the upstream byte stream.
- **Returns** a `Stream<UInt8[]>` of gzip-framed chunks.

#### `inflate`

```lin
val inflate = (s: Stream): Stream
```

Adapter: decompress a raw DEFLATE byte stream (no gzip header/CRC). LAZY.
- **`s`** — the upstream raw-DEFLATE byte stream.
- **Returns** a `Stream<UInt8[]>` of decompressed chunks; a decode fault surfaces in-band as `Error`.

#### `deflate`

```lin
val deflate = (s: Stream): Stream
```

Adapter: compress a byte stream as a raw DEFLATE bitstream (no gzip header/CRC). LAZY.
- **`s`** — the upstream byte stream.
- **Returns** a `Stream<UInt8[]>` of raw-DEFLATE chunks.
