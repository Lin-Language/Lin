# std/archive

Tar splitting over a byte stream. A **tar** archive is a flat sequence of 512-byte-aligned (header + body) records; these surfaces turn a `Stream<UInt8[]>` into archive entries **without buffering the whole archive** — the parent stream is pulled one chunk at a time. A `.tar.gz` is just [`gunzip()`](/stdlib/compress.html) composed with the tar splitter. **Only the tar format is supported** (zip is not).

All three surfaces **consume** the parent stream (it is moved in); the source binding may not be used again after the call — the affine stream check rejects a reuse (re-open the source for a second pass).

```lin
import { readStream, drain, writeStream } from "std/stream"
import { gunzip } from "std/compress"
import { untar, manifest, files } from "std/archive"
import { for } from "std/iter"

// List a .tar.gz's contents without extracting anything:
readStream("logs.tar.gz").gunzip().manifest().for(m => print(m["name"]))

// Extract every member to disk in constant memory (each body streamed straight to its file):
readStream("logs.tar.gz").gunzip().untar((meta, data) =>
  data.writeStream("out/${meta["name"]}").drain()
)
```

> A `.tar.gz` is read by stacking adapters: the [`readStream`](/stdlib/stream.html) source feeds [`gunzip`](/stdlib/compress.html), which feeds the tar splitter. Each stage is lazy, so an arbitrarily large archive flows through in bounded memory.

## The entry `meta` object

Every entry's `meta` is pure JSON:

```lin
{ name: String, size: Int64, typeflag: String, isDir: Boolean }
```

where `typeflag` is the tar type byte as a one-character string (`"0"` = regular file, `"5"` = directory) and `isDir` is `true` iff `typeflag == "5"`.

## The `data` sub-stream is sync-only

`untar` hands your callback a `data` sub-stream over each entry's body. **It is valid only during the synchronous execution of the callback.** The driver is paused while your body runs and resumes — advancing to the next entry — the moment the body returns, and `data` shares a cursor with that paused driver. So `data` must be consumed (drained or read) **inside the callback**.

You **cannot** hand `data` to a worker thread: passing it to [`.promise()`](/stdlib/stream.html) would race the shared cursor and is unsupported. The [stream placement restriction](/reference/concurrency.html) bounds `data`'s lifetime to the callback — it cannot be stored in an object field, a `var`, or an array — which keeps it from escaping. (A dedicated compile-time check specifically for `.promise()` on a sub-stream is a known gap; the placement restriction is what makes the misuse hard to express.)

## Types

```lin
Stream<UInt8[]>   // the byte stream a tar archive is read from, and each entry's body sub-stream
Stream<Object>    // the meta/entry stream produced by manifest / files
```

## Function reference

| Function | Signature | Role | Description |
| --- | --- | --- | --- |
| `untar` | `(Stream<UInt8[]>, body: (Json, Stream<UInt8[]>) -> Json) -> Null \| Error` | terminal | Drive the archive, calling `body(meta, data)` once per entry |
| `manifest` | `(Stream<UInt8[]>) -> Stream<Object>` | adapter | Yield each entry's `meta`, bodies skipped (a listing) |
| `files` | `(Stream<UInt8[]>) -> Stream<Object>` | adapter | Yield each entry with its body buffered to `UInt8[]` |

---

### `untar`

The **terminal** driver: drives the whole archive on the calling thread, calling `body(meta, data)` once per entry, where `data` is a `Stream<UInt8[]>` sub-stream over that entry's body. The body's return value is ignored. Returns `Null` on a clean archive, or an `Error` if a read fault or a body fault occurs.

This is the **constant-memory primitive** — an arbitrarily large member flows through its `data` sub-stream without ever being fully buffered. Whether the body **drains** `data` or **ignores** it, the driver always skips to the next entry correctly (an undrained body is skipped automatically).

```lin
import { readStream, writeStream, drain } from "std/stream"
import { gunzip } from "std/compress"
import { untar } from "std/archive"

val outcome = readStream("logs.tar.gz").gunzip().untar((meta, data) =>
  match meta["isDir"]
    true  => Null                                       // nothing to write for a directory
    false => data.writeStream("out/${meta["name"]}").drain()
)
match outcome
  is Error => print("extract failed: ${outcome["message"]}")
  else     => print("done")
```

Remember the sub-stream is sync-only: consume `data` inside the callback; never hand it to `.promise()` or store it.

---

### `manifest`

An **adapter** yielding each entry's `meta` object, with its body **skipped** — a meta-only listing. No sub-streams are minted, so there is no lifetime concern. Composes with [`std/iter`](/stdlib/iter.html) (`filter`/`map`/`for`) like any other `Stream`:

```lin
import { readStream } from "std/stream"
import { gunzip } from "std/compress"
import { manifest } from "std/archive"
import { filter, for } from "std/iter"

readStream("logs.tar.gz")
  .gunzip()
  .manifest()
  .filter(m => !m["isDir"])
  .for(m => print("${m["name"]} (${m["size"]} bytes)"))
```

---

### `files`

An **adapter** yielding `{ name, data, size, typeflag, isDir }` per entry, where `data` is the entry's **full body buffered** into a `UInt8[]`. A convenience for normal-sized files; composes with [`std/iter`](/stdlib/iter.html). Because each body is buffered in memory, prefer `untar` for arbitrarily large entries.

```lin
import { readStream } from "std/stream"
import { gunzip } from "std/compress"
import { files } from "std/archive"
import { for } from "std/iter"

readStream("config.tar.gz")
  .gunzip()
  .files()
  .for(entry => print("${entry["name"]}: ${entry["size"]} bytes"))
```

---

## See also

- [`std/compress`](/stdlib/compress.html) — `gunzip` to read a `.tar.gz`, `gzip` to write one.
- [`std/stream`](/stdlib/stream.html) — the byte streams archives are read from, and the `writeStream`/`drain` sinks used to extract entries.
- [`std/iter`](/stdlib/iter.html) — the combinators that drive the `manifest`/`files` entry streams lazily.
