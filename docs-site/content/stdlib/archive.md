# std/archive

std/archive — split a tar archive out of a byte stream, one entry at a time.

A tar archive is a flat sequence of 512-byte-aligned (header + body) records. These functions turn
a `Stream<UInt8[]>` into archive entries without buffering the whole archive: the parent stream is
pulled one chunk at a time, so memory stays constant regardless of archive size. A `tar.gz` is just
`gunzip()` (from std/compress) composed with the splitter:

```lin
import { readStream } from "std/stream"
import { gunzip } from "std/compress"
import { untar } from "std/archive"
```

```lin
readStream("data.tar.gz")
  .gunzip()
  .untar((meta, data) => print("${meta["name"]}: ${meta["size"]} bytes"))
```

Each function consumes the parent stream: it is moved in, and the source binding cannot be used
again afterwards.

The `data` sub-stream handed to an `untar` callback is valid only while that callback runs. The
driver pauses on the current entry, runs your callback, and skips to the next entry the moment it
returns — so you must read or drain `data` inside the callback. It cannot be stored in a field,
var, or array, handed to a worker via `.promise()`, or otherwise outlive the call.

## Reference

#### `TarHeader`

```lin
type TarHeader = { "name": String, "size": Int64, "typeflag": String, "isDir": Boolean }
```

A single tar entry's header metadata.

#### `TarFile`

```lin
type TarFile = { "name": String, "data": UInt8[], "size": Int64, "typeflag": String, "isDir": Boolean }
```

A tar entry together with its fully buffered body (`data`).

#### `untar`

```lin
val untar = (s: Stream<UInt8[]>, body: (TarHeader, Stream<UInt8[]>) => AnyVal): AnyVal
```

Drive the whole tar archive on the calling thread in constant memory, calling `body` once per entry.
- **`s`** — the upstream byte stream (moved in; not reusable after the call).
- **`body`** — `body(meta, data)`, called per entry, where `meta` is the entry's `TarHeader` and
  `data` is a byte stream over the entry's body (valid only inside the callback). Its return is ignored.
- **Returns** `Null` on success, or an `Error` if an entry body faulted or a read failed.

#### `manifest`

```lin
val manifest = (s: Stream<UInt8[]>): Stream<TarHeader>
```

Stream each entry's metadata, skipping bodies — a fast, body-free listing.
- **`s`** — the upstream byte stream (moved in).
- **Returns** a `Stream<TarHeader>`; composes with std/iter.

#### `files`

```lin
val files = (s: Stream<UInt8[]>): Stream<TarFile>
```

Stream each entry with its full body buffered into memory. Handy for normal-sized files; prefer
`untar` when entries may be arbitrarily large.
- **`s`** — the upstream byte stream (moved in).
- **Returns** a `Stream<TarFile>`, where each entry's `data` is the body as a `UInt8[]`; composes with std/iter.
