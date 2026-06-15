# std/stream

std/stream — lazy, fallible streams over OS resources (files, sockets, subprocess stdout,
stdin).

A `Stream<T>` is an opaque runtime value built as a lazy pull graph: a source node
(`readStream`), zero or more adapters (`lines`/`linesMax`/`chunks`, plus the std/iter
combinators map/filter/take/… which dispatch lazily on a stream receiver), and a terminal op
that drives the graph one item at a time with bounded memory. Building a pipeline allocates
the lazy nodes but reads nothing; the terminal driver pulls it to completion.

A pipeline imports its sources and sinks from std/stream and its combinators from std/iter,
then chains them onto an open stream:

```lin
import { readStream, lines, writeLines, drain } from "std/stream"
import { map, filter } from "std/iter"
```

```lin
val pipeline = readStream("in.csv")
  .lines()
  .map(parseRow)
  .filter(notEmpty)
  .writeLines("out.csv")
drain(pipeline)
```

Errors are threaded in-band: the first read error poisons the upstream and short-circuits to
the terminal op, so error handling lives only at the terminal, not at every adapter.

The generic combinators (map/filter/take/drop/reduce/for/…) are not part of std/stream — they
come from std/iter and dispatch to the lazy stream backend automatically when the receiver is
a `Stream`. A pipeline imports its sources and sinks from std/stream and its combinators from
std/iter. (The optional callback index parameter documented in std/iter is for array/iterator
receivers only — lazy stream combinators keep 1-arg callbacks.) Byte sources also come from
other modules: `tcpStream` (std/net), `stdoutStream` (std/process), and `stdinStream` (std/io)
all return `Stream<UInt8[]>` and feed the same adapters and terminals.

A stream is read once. A stream is consumed as you read it, so each one flows through a single
pipeline; once you've called a combinator or terminal on a stream, reaching for it again is a
compile-time error. To make a second pass, open a fresh stream. You don't have to consume a
stream — opening one and never reading it is fine; it cleans up after itself. Keep a stream in
a local `val` (or pass/return it from a function); it can't be stashed in objects, arrays, or
`var`s.

## Reference

#### `readStream`

```lin
val readStream = (path: String)
```

`readStream(path)` opens a file as a lazy byte stream (`Stream<UInt8[]>`, or an `Error` if the
file cannot be opened); no bytes are read until a terminal op drives it. Example:

```lin
readStream("notes.txt").readText()   // file contents as String | Error
```

#### `lines`

```lin
val lines = (s: Stream<UInt8[]>): Stream<String>
```

Adapter: split the upstream byte stream into newline-delimited `String` items. Lazy (no reads yet).
- **`s`** — the upstream byte stream.
- **Returns** a `Stream<String>`. A single line is capped at 64 MiB, so a newline-less input fails
  in-band with an `Error` rather than buffering the whole stream; use `linesMax` to change the
  cap.

**Example:**

```lin
readStream("access.log").lines().for(line => print(line))   // `for` from std/iter
```

#### `linesMax`

```lin
val linesMax = (s: Stream<UInt8[]>, maxBytes: Int32): Stream<String>
```

Adapter: like `lines`, but with an explicit per-line byte cap. Lazy.
- **`s`** — the upstream byte stream.
- **`maxBytes`** — the per-line cap in bytes; `n <= 0` keeps the 64 MiB default.
- **Returns** a `Stream<String>`; a line exceeding the cap fails in-band with an `Error`.

#### `chunks`

```lin
val chunks = (s: Stream<UInt8[]>, n: Int32): Stream<UInt8[]>
```

Adapter: regroup the upstream byte stream into `UInt8[]` chunks of `n` bytes. Lazy.
- **`s`** — the upstream byte stream.
- **`n`** — the target chunk size in bytes (the final chunk may be shorter).
- **Returns** a `Stream<UInt8[]>`.

**Example:**

```lin
readStream("frames.bin").chunks(188).for(frame => process(frame))
```

#### `writeStream`

```lin
val writeStream = (s: Stream, path: String): Stream
```

Sink: write each item's bytes verbatim (raw) to `path`, concatenated with no separator (a String
writes its UTF-8 bytes, a `UInt8[]` its raw bytes). The raw sink is what lets binary pipelines
(such as `gzip` followed by `writeStream`) produce an uncorrupted file. Lazy. Use `writeLines`
for newline-delimited output.
- **`s`** — the upstream stream to write.
- **`path`** — the destination file path.
- **Returns** a sink `Stream` node; nothing is written until a terminal driver runs it.

**Example:**

```lin
val sink = bytes.gzip().writeStream("data.gz")   // then drain(sink) to run it
```

#### `writeLines`

```lin
val writeLines = (s: Stream, path: String): Stream
```

Sink: write each item to `path` followed by a newline (one item per line). For text/line-delimited
output (CSV rows, log lines); use `writeStream` for raw verbatim bytes. Lazy.
- **`s`** — the upstream stream to write.
- **`path`** — the destination file path.
- **Returns** a sink `Stream` node; nothing is written until a terminal driver runs it.

**Example:**

```lin
val sink = rows.writeLines("out.csv")   // rows: a Stream<String> of lines to write
```

#### `drain`

```lin
val drain = (s: Stream): Null | Error
```

Terminal: force a sink (or any pipeline) to completion. Terminal drivers consume the stream on
the calling thread and close it afterwards.
- **`s`** — the pipeline to drive.
- **Returns** `Null` on success, or an `Error` if a read/transform faulted.

#### `collect`

```lin
val collect = (s: Stream): UInt8[] | Error
```

Terminal: pull all bytes into a single buffer.
- **`s`** — the byte pipeline to drive.
- **Returns** a `UInt8[]` of all bytes, or an `Error` if a read/transform faulted.

**Example:**

```lin
readStream("data.bin").collect()   // UInt8[] | Error
```

#### `readText`

```lin
val readText = (s: Stream): String | Error
```

Terminal: pull all bytes into a single UTF-8 string.
- **`s`** — the byte pipeline to drive.
- **Returns** a `String` of the decoded bytes, or an `Error` if a read/transform faulted.

#### `close`

```lin
val close = (s: Stream): Null
```

Release the stream's underlying resource explicitly.
- **`s`** — the stream to close. Idempotent; a dropped stream is also closed automatically, so this
  is for determinism.

#### `promise`

```lin
val promise = (s: Stream): AnyVal
```

Terminal: drive the pipeline on a worker thread for real concurrency and fault isolation. The
stream is moved onto the worker, which owns the whole pipeline and drives it to EOF. Run several
at once with `parallel([...])`.
- **`s`** — the pipeline to drive off-thread.
- **Returns** a `Promise<Null | Error>`; awaiting it yields `Null` on success, or an `Error` if a
  read/transform faulted (caught at the async boundary). `await` reattaches the `Null | Error`
  union, so the `Error` case must be handled (see std/async).

**Example:**

```lin
val p = pipeline.promise()   // pipeline: a sink built with readStream/lines/writeLines
```

**Example:**

```lin
match await(p) is Error => print("failed") else => print("done")
```
