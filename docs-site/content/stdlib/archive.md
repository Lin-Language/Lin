# std/archive

std/archive — tar splitting over a `Stream<UInt8[]>` (ADR-047 streams).

A tar archive is a flat sequence of 512-byte-aligned (header + body) records. These surfaces turn
a byte stream into archive entries WITHOUT buffering the whole archive: the parent stream is
pulled one chunk at a time. `tar.gz` is just `gunzip()` (std/compress) composed with the splitter:

  import { readStream, drain, writeStream } from "std/stream"
  import { gunzip } from "std/compress"
  import { untar, manifest, files } from "std/archive"
  readStream("data.tar.gz").gunzip().untar((meta, data) => ...)

Each surface CONSUMES the parent stream (it is moved in) — the source binding may not be used
again after the call (the affine stream check rejects a reuse).

SYNC-ONLY SUB-STREAM CAVEAT (untar): inside the `untar` body, `data` is a sub-stream over the
CURRENT entry's bytes. It is valid only DURING the body's synchronous execution — the driver is
paused while the body runs and resumes (skipping to the next entry) the moment the body returns.
`data` therefore MUST be consumed (drained / read) inside the callback; it cannot be handed to a
worker via `.promise()`, stored in a field/var/array, or otherwise outlive the callback. The
ADR-049 stream placement restriction enforces the lifetime bound; a `.promise()` on `data` would
race the shared cursor and is UNSUPPORTED (a dedicated compile-time check for that is a known gap).

## Reference

#### `untar`

```lin
val untar = (s: Stream, body: (Json, Stream)
```

Drive the whole tar archive on the calling thread in constant memory, calling `body` per entry.
- **`s`** — the upstream byte stream (moved in — may not be reused after the call).
- **`body`** — `body(meta, data)`, called per entry; `meta` is `{ name, size, typeflag, isDir }`
  and `data` is a `Stream<UInt8[]>` sub-stream over the entry's body (sync-only, see the module
  caveat). Its return is ignored.
- **Returns** `Null` on success, or an `Error` if an entry body faulted or a read failed.

#### `manifest`

```lin
val manifest = (s: Stream): Stream
```

Stream each entry's metadata, skipping bodies (a meta-only listing).
- **`s`** — the upstream byte stream (moved in).
- **Returns** a `Stream<Object>` of each entry's `meta` object; composes with std/iter.

#### `files`

```lin
val files = (s: Stream): Stream
```

Stream each entry with its full body buffered into memory (convenience for normal-sized files;
prefer `untar` for arbitrarily large entries).
- **`s`** — the upstream byte stream (moved in).
- **Returns** a `Stream<Object>` of `{ name, data, size, typeflag, isDir }` where `data` is the entry
  body as a `UInt8[]`; composes with std/iter.
