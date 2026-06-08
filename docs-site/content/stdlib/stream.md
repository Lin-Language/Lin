# std/stream

std/stream — lazy, fallible streams over OS resources (files, sockets, subprocess stdout,
stdin). Streams brief, ADR-047.

A `Stream<T>` is an opaque runtime value built as a lazy PULL GRAPH: a SOURCE node
(`readStream`), zero or more ADAPTERS (`lines`/`linesMax`/`chunks`, plus the std/iter
combinators map/filter/take/… which dispatch lazily on a stream receiver), and a TERMINAL op
that drives the graph one item at a time with bounded memory. Building a pipeline allocates
the lazy nodes but reads NOTHING; the terminal driver pulls it to completion.

  import { readStream, writeLines, drain } from "std/stream"
  import { map, filter, take, for } from "std/iter"

  readStream("in.csv").lines().map(transform).filter(notEmpty).writeLines("out.csv").drain()

Errors are threaded IN-BAND: the first read error poisons the upstream and short-circuits to
the terminal op, so error handling lives only at the terminal, not at every adapter.

The generic combinators (map/filter/take/drop/reduce/for/…) are NOT part of std/stream — they
come from std/iter and dispatch to the lazy stream backend automatically when the receiver is
a `Stream`. A pipeline imports its SOURCES and SINKS from std/stream and its COMBINATORS from
std/iter. (The optional callback index parameter documented in std/iter is for array/iterator
receivers only — lazy stream combinators keep 1-arg callbacks.) Byte sources also come from
other modules: `tcpStream` (std/net), `stdoutStream` (std/process), and `stdinStream` (std/io)
all return `Stream<UInt8[]>` and feed the same adapters and terminals.

A STREAM CAN ONLY BE READ ONCE. A stream is consumed as you read it, so each one flows through
a single pipeline; once you've called a combinator or terminal on a stream, reaching for it
again is a compile-time error. To make a second pass, open a fresh stream. You don't have to
consume a stream — opening one and never reading it is fine; it cleans up after itself. Keep a
stream in a local `val` (or pass/return it from a function); it can't be stashed in objects,
arrays, or `var`s.

Maintainer note: Stream is opaque — `compat.rs` forbids every non-stream operation on a
`Stream` value. Transform closures operate on boxed `Json` items (a chunk is `UInt8[]`, a line
is `String` — both JSON-compatible), so map/filter take `(Json) => Json` / `(Json) => Boolean`.

## Reference

#### `readStream`

```lin
val readStream = (path: String)
```

`readStream(path)` opens a file as a lazy byte stream (`Stream<UInt8[]>`, or an `Error` if the
file cannot be opened); no bytes are read until a terminal op drives it. Example:

  readStream("notes.txt").readText()   // file contents as String | Error

#### `lines`

```lin
val lines = (s: Stream): Stream
```

Adapter: split the upstream byte stream into newline-delimited `String` items. LAZY (no reads yet).
- **`s`** — the upstream byte stream.
- **Returns** a `Stream<String>`. A single line is capped at 64 MiB, so a newline-less input fails
  in-band with an `Error` rather than buffering the whole stream; use `linesMax` to change the
  cap.
- **Example:** readStream("access.log").lines().for(line => print(line))   // `for` from std/iter

#### `linesMax`

```lin
val linesMax = (s: Stream, maxBytes: Int32): Stream
```

Adapter: like `lines`, but with an explicit per-line byte cap. LAZY.
- **`s`** — the upstream byte stream.
- **`maxBytes`** — the per-line cap in bytes; `n <= 0` keeps the 64 MiB default.
- **Returns** a `Stream<String>`; a line exceeding the cap fails in-band with an `Error`.

#### `chunks`

```lin
val chunks = (s: Stream, n: Int32): Stream
```

Adapter: regroup the upstream byte stream into `UInt8[]` chunks of `n` bytes. LAZY.
- **`s`** — the upstream byte stream.
- **`n`** — the target chunk size in bytes (the final chunk may be shorter).
- **Returns** a `Stream<UInt8[]>`.
- **Example:** readStream("frames.bin").chunks(188).for(frame => process(frame))

#### `writeStream`

```lin
val writeStream = (s: Stream, path: String): Stream
```

NOTE (std/iter unification, Stage 3/4): the GENERIC combinators (map/filter/take/drop/reduce/
for/find/some/every/flatMap/takeWhile/dropWhile/flatten/concat) are NO LONGER exported here.
They come from `std/iter` and dispatch to the lazy `lin_stream_*` backend on a stream receiver
(the IR's `lower_call` redirect, mirroring the checker's `streamish_combinator_ret`). std/stream
keeps only stream-SPECIFIC ops (readStream/writeStream/writeLines/drain/collect/readText/promise/
close/lines/linesMax/chunks). A user writes:
  import { map, filter, take, reduce, for } from "std/iter"
  import { readStream, writeStream, drain } from "std/stream"
and `readStream(p).map(f).filter(g).take(10).reduce(...)` works lazily.
Sink: write each item's bytes verbatim (raw) to `path`, concatenated with NO separator (a String
writes its UTF-8 bytes, a UInt8[] its raw bytes). The raw sink is what lets binary pipelines (e.g.
`gzip(s).writeStream("x.gz")`) produce an uncorrupted file. LAZY. Use `writeLines` for
newline-delimited output.
- **`s`** — the upstream stream to write.
- **`path`** — the destination file path.
- **Returns** a sink `Stream` node; nothing is written until a terminal driver runs it.
- **Example:** readStream("data.txt").gzip().writeStream("data.gz").drain()   // raw bytes, valid .gz

#### `writeLines`

```lin
val writeLines = (s: Stream, path: String): Stream
```

Sink: write each item to `path` followed by a newline (one item per line). For text/line-delimited
output (CSV rows, log lines); use `writeStream` for raw verbatim bytes. LAZY.
- **`s`** — the upstream stream to write.
- **`path`** — the destination file path.
- **Returns** a sink `Stream` node; nothing is written until a terminal driver runs it.
- **Example:** readStream("in.csv").lines().map(transform).writeLines("out.csv").drain()

#### `drain`

```lin
val drain = (s: Stream): Json
```

Terminal drivers — these consume the stream on the CALLING thread and close it afterwards.
Terminal: force a sink (or any pipeline) to completion.
- **`s`** — the pipeline to drive.
- **Returns** `Null` on success, or an `Error` if a read/transform faulted.

#### `collect`

```lin
val collect = (s: Stream): Json
```

Terminal: pull all bytes into a single buffer.
- **`s`** — the byte pipeline to drive.
- **Returns** a `UInt8[]` of all bytes, or an `Error` if a read/transform faulted.
- **Example:** readStream("data.bin").collect()   // UInt8[] | Error

#### `readText`

```lin
val readText = (s: Stream): Json
```

Terminal: pull all bytes into a single UTF-8 string.
- **`s`** — the byte pipeline to drive.
- **Returns** a `String` of the decoded bytes, or an `Error` if a read/transform faulted.

#### `close`

```lin
val close = (s: Stream): Null
```

Release the stream's underlying resource explicitly.
- **`s`** — the stream to close. Idempotent; the RC finalizer also closes a dropped stream, so this
  is for determinism.

#### `promise`

```lin
val promise = (s: Stream): Json
```

`for` is NOT exported here (std/iter unification): the single unified `for` comes from std/iter
and dispatches to the stream backend (`lin_stream_for`) on a stream receiver via the IR redirect.
Import `for` from std/iter to consume a stream: `stream.lines().for(println)`.
Terminal: drive the pipeline on a WORKER thread for real concurrency + fault isolation (brief §5).
The stream is MOVED onto the worker (legal via CAP_MOVE), which owns the whole pipeline and drives
it to EOF. Run several with `parallel([...])`.
- **`s`** — the pipeline to drive off-thread.
- **Returns** a `Promise<Null | Error>`; awaiting it yields `Null` on success, or an `Error` if a
  read/transform faulted (caught at the async boundary, ADR-045 / §32.2.2). `await` reattaches
  the `Null | Error` union, so the `Error` case must be handled (see std/async).
- **Example:** val p = readStream("big.log").lines().filter(isError).writeLines("errors.log").promise()
- **Example:** match await(p) is Error => print("failed") else => print("done")
