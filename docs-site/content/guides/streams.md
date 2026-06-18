# Streaming I/O

A `Stream` is a **lazy, fallible sequence** over an OS resource — a file, a socket, a subprocess's
output. You build a pipeline by chaining adapters, then a *terminal* drives it. Nothing is read
until the terminal runs, and only one item is ever in flight, so a multi-gigabyte file is processed
in constant memory.

Streams live in [`std/stream`](/stdlib/stream); the transform combinators (`map`/`filter`/`take`/…)
are the same [`std/iter`](/stdlib/iter) names you already use on arrays — they just dispatch to the
lazy stream backend when the receiver is a `Stream`.

```lin
import { readStream, lines, drain } from "std/stream"
import { map, filter, drop, reduce, for } from "std/iter"
```

## Sources, adapters, terminals

A pipeline has three parts:

- a **source** opens the resource — `readStream(path)` gives a `Stream<UInt8[]>` (raw bytes);
- **adapters** transform it lazily and return a new `Stream`, reading nothing yet —
  `lines`, `chunks`, and the `std/iter` combinators (`map`, `filter`, `drop`, `take`, …);
- a **terminal** drives the pipeline to completion — `reduce`, `for`, `readText`, `collect`, `drain`.

```lin
import { readStream, lines } from "std/stream"
import { drop, map, reduce } from "std/iter"
import { length } from "std/array"

// Drop the header, then sum each remaining line's length — one line at a time.
val total = readStream("data.csv")
  .lines()                       // Stream<String>   (adapter, reads nothing)
  .drop(1)                       // Stream<String>   (adapter)
  .map(line => length(line))     // Stream<Int32>    (adapter)
  .reduce(0, (acc, n) => acc + n)   // terminal — drives the stream
```

Up to `.reduce`, no bytes have been read; the chain is just a lazy description. The terminal pulls
items through one at a time.

## Errors thread through the pipeline

A stream read can fail partway through (a disk error, a malformed line, a missing file). Rather than
forcing an `is Error` check between every step, a stream threads the **first** error straight to the
terminal — so the whole chain stays fluent and you handle failure **once**, at the end. Every
terminal over a stream therefore returns `T | Error`:

```lin
val total = readStream("data.csv")
  .lines()
  .drop(1)
  .map(line => length(line))
  .reduce(0, (acc, n) => acc + n)   // Int32 | Error

match total
  is Error => print("read failed: ${total["message"]}")
  else     => print("sum: ${toString(total)}")
```

Even a source that fails to open flows through: `readStream("missing.csv").lines().for(...)`
returns an `Error` from the terminal rather than trapping.

## Driving a pipeline: the terminals

| Terminal | Result | Use |
| --- | --- | --- |
| `reduce(init, f)` | `U \| Error` | Fold to a single value |
| `for(f)` | `Null \| Error` | Run a side effect per item |
| `find(f)` / `some(f)` / `every(f)` | `… \| Error` | Short-circuiting queries |
| `readText()` | `String \| Error` | Drain all bytes into one string |
| `collect()` | `UInt8[] \| Error` | Drain all bytes into one buffer |
| `drain()` | `Null \| Error` | Force a sink to completion (see below) |

```lin
import { readStream, readText } from "std/stream"

val contents = readStream("notes.txt").readText()
match contents
  is Error => print("could not read")
  else     => print(contents)
```

## Writing: sinks

A **sink** adapter (`writeStream` / `writeLines`) describes an output file; the `drain` terminal runs
the pipeline and performs the writes. `writeLines` adds a newline after each item; `writeStream`
writes each item's bytes verbatim (use it for binary output).

```lin
import { readStream, lines, writeLines, drain } from "std/stream"
import { drop, map } from "std/iter"

// Transform a CSV's rows and write them to a new file — streaming, constant memory.
val r = readStream("in.csv")
  .lines()
  .drop(1)
  .map(line => "row: ${line}")
  .writeLines("out.txt")
  .drain()                       // Null | Error

match r
  is Error => print("write failed")
  else     => print("done")
```

## Compression and archives, for free

Because compression and tar are just byte-stream adapters, they slot into the same pipeline. A
`tar.gz` is literally `gunzip()` composed with `untar()`:

```lin
import { readStream, writeStream, drain, readText } from "std/stream"
import { gzip, gunzip } from "std/compress"

// Compress a file to disk, then read it back.
readStream("notes.txt").gzip().writeStream("notes.txt.gz").drain()
val recovered = readStream("notes.txt.gz").gunzip().readText()   // String | Error
```

`untar` (from [`std/archive`](/stdlib/archive)) walks each entry of a tar stream, handing you the
entry's metadata and a sub-stream of its bytes — so you can process a multi-file archive without
ever holding it all in memory:

```lin
import { readStream } from "std/stream"
import { gunzip } from "std/compress"
import { untar } from "std/archive"
import { lines, for } from "std/iter"

// Process transfers.txt out of a .tar.gz, line by line.
readStream("feed.tar.gz")
  .gunzip()
  .untar((meta, data) =>
    if meta["name"] == "transfers.txt" then
      data.lines().for(line => process(line))
    else
      null
  )
```

> `untar`'s `data` sub-stream is **sync-only**: it is valid only while the callback runs, so consume
> it inside the callback (don't store it or hand it to another thread).

## Moving work off the calling thread: `.promise()`

A terminal normally drives the pipeline on the calling thread. `.promise()` instead moves the whole
pipeline onto a **worker thread** and returns a `Promise<Null | Error>` you `await` later — so a
long copy/transform runs in the background while you do other work. Run several at once with
`parallel`.

```lin
import { readStream, lines, writeLines, promise } from "std/stream"
import { map } from "std/iter"
import { toUpper } from "std/string"
import { await } from "std/async"

val p = readStream("in.txt")
  .lines()
  .map(line => line.toUpper())
  .writeLines("out.txt")
  .promise()                     // moves the pipeline to a worker

val result = await(p)            // Null | Error
match result
  is Error => print("pipeline failed")
  else     => print("done")
```

## A stream is single-use

A combinator that routes to a stream **consumes** it (the stream is an affine resource), so a
pipeline can be driven only once — using the same stream value twice is a compile-time error. This
is what lets the runtime own the underlying file descriptor and close it deterministically. Build a
fresh `readStream(...)` for each pass.

## What's next?

- [std/stream reference](/stdlib/stream) — every source, adapter, sink, and terminal.
- [std/iter reference](/stdlib/iter) — the eager/lazy dispatch rules behind `map`/`filter`/`reduce`.
- [std/compress](/stdlib/compress) and [std/archive](/stdlib/archive) — gzip/DEFLATE and tar.
- [Events](/guides/events) — stream records on one thread and emit the processed results to a
  subscriber on another.
