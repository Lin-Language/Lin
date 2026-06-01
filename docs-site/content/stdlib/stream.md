# std/stream

Lazy, fallible streams over OS resources — files, sockets, subprocess stdout, and stdin. A `Stream<T>` is an opaque runtime value built as a **lazy pull graph**: a *source* node (`readStream`), zero or more *adapters* (`lines`/`linesMax`/`chunks`, plus the [`std/iter`](/stdlib/iter.html) combinators `map`/`filter`/`take`/… which dispatch lazily on a stream receiver), and a *terminal* operation that drives the graph one item at a time with bounded memory.

Errors are threaded **in-band**: the first read error poisons the upstream and short-circuits to the terminal op, so error handling lives only at the terminal, not at every adapter.

```lin
import { readStream, writeLines, drain } from "std/stream"
import { map, filter, take, for } from "std/iter"

readStream("in.csv")
  .lines()
  .map(transform)
  .filter(notEmpty)
  .writeLines("out.csv")
  .drain()
```

> The generic combinators (`map`, `filter`, `take`, `drop`, `reduce`, `for`, …) are **not** part of `std/stream` — they come from [`std/iter`](/stdlib/iter.html) and dispatch to the lazy stream backend automatically when the receiver is a `Stream`. A stream pipeline imports its **sources and sinks** from `std/stream` and its **combinators** from `std/iter`.

Byte sources also come from other modules — `tcpStream` ([`std/net`](/stdlib/net.html)), `stdoutStream` ([`std/process`](/stdlib/process.html)), and `stdinStream` ([`std/io`](/stdlib/io.html)) all return `Stream<UInt8[]>` and feed the same adapters and terminals documented here.

## A stream can only be read once

A stream is consumed as you read it — so each one flows through a **single** pipeline. Once you've called a combinator or a terminal on a stream, that stream is used up; reaching for it again is a compile-time error. To make a second pass over the same data, open a fresh stream.

```lin
val s = readStream("data.txt")
val a = s.lines().collect()
val b = s.lines().collect()   // error: `s` was already used above

// To read it twice, open it twice:
val a = readStream("data.txt").lines().collect()
val b = readStream("data.txt").lines().collect()
```

You don't have to consume a stream — opening one and never reading it is fine; it cleans up after itself. Keep a stream in a local `val` (or pass it to/return it from a function); they can't be stashed in objects, arrays, or `var`s.

## Types

```lin
Stream<T>   // opaque; covariant in T; not JSON, not subscriptable
```

## Function reference

| Function | Signature | Role | Description |
| --- | --- | --- | --- |
| `readStream` | `(String) -> Stream<UInt8[]>` | source | Open a file as a lazy byte stream |
| `lines` | `(Stream<UInt8[]>) -> Stream<String>` | adapter | View bytes as a stream of UTF-8 lines |
| `linesMax` | `(Stream<UInt8[]>, Int32) -> Stream<String>` | adapter | Like `lines`, with an explicit per-line byte cap |
| `chunks` | `(Stream<UInt8[]>, Int32) -> Stream<UInt8[]>` | adapter | Re-chunk into fixed-size `n`-byte windows |
| `writeStream` | `(Stream<T>, String) -> Stream<T>` | sink | RAW sink: write each item's bytes verbatim, no separator |
| `writeLines` | `(Stream<T>, String) -> Stream<T>` | sink | Line sink: write each item followed by a newline |
| `readText` | `(Stream<UInt8[]>) -> String \| Error` | terminal | Drive to completion, return the contents as a `String` |
| `collect` | `(Stream<UInt8[]>) -> UInt8[] \| Error` | terminal | Drive to completion, return the contents as a `UInt8[]` |
| `drain` | `(Stream<T>) -> Null \| Error` | terminal | Drive the pipeline on the calling thread |
| `promise` | `(Stream<T>) -> Json` | terminal | Run the pipeline on a background thread; return a promise |
| `close` | `(Stream<T>) -> Null` | — | Release the file/socket now (optional; idempotent) |

---

### `readStream`

Opens the file at `path` as a lazy byte stream. No bytes are read until a terminal operation drives the stream. A failure to open, or a read failure during traversal, surfaces in-band as an `Error` at the terminal op.

```lin
val text = readStream("notes.txt").readText()
match text
  is Error => print("read failed: ${text["message"]}")
  else     => print(text)
```

---

### `lines` / `linesMax`

`lines` lazily views a byte stream as a stream of lines (splitting on newlines, decoding UTF-8 per line) — an adapter that reads nothing until driven. A single line is capped (default 64 MiB) so a newline-less input fails in-band with an `Error` rather than buffering the whole stream. `linesMax` sets an explicit per-line byte cap; a line longer than `maxBytes` fails in-band, and a `maxBytes` of `0` or less keeps the default cap.

```lin
readStream("access.log").lines().for(line => print(line))   // `for` from std/iter
readStream("untrusted.txt").linesMax(1024).for(line => print(line))
```

---

### `chunks`

Re-chunks a byte stream into fixed-size `n`-byte windows (the final window may be shorter). Useful for fixed-record binary formats.

```lin
readStream("frames.bin").chunks(188).for(frame => process(frame))
```

---

### `writeStream`

Builds a **raw sink** node that writes each upstream item's bytes to the file at `path` **verbatim**, concatenated with **no separator** (a `String` writes its UTF-8 bytes, a `UInt8[]` its raw bytes, anything else its `toString`). It returns a `Stream` whose terminal op (`drain`/`promise`) runs the whole pipeline — pulling one item at a time and writing it — so memory stays bounded regardless of input size. Building the sink writes nothing; a terminal op must drive it.

Because it injects no newlines, `writeStream` is the correct sink for **binary** output — e.g. compressing a file to disk, where any inserted separator would corrupt the result:

```lin
readStream("data.txt")
  .gzip()                  // from std/compress
  .writeStream("data.gz")  // raw bytes — a valid .gz file
  .drain()
```

For newline-delimited text output, use `writeLines`.

---

### `writeLines`

Builds a **line-oriented sink** node that writes each upstream item to the file at `path` followed by a newline (`\n`) — one item per line. Item bytes are rendered the same way as `writeStream`; the difference is the trailing `\n` after each item. Like `writeStream` it is lazy — a terminal op must drive it.

```lin
readStream("in.csv")
  .lines()
  .map(transform)
  .writeLines("out.csv")
  .drain()
```

---

### `readText` / `collect`

Terminals that drive a byte stream to completion on the calling thread and return its full contents — `readText` as one `String`, `collect` as one `UInt8[]` buffer — or an `Error` if a read fails.

```lin
val all  = readStream("notes.txt").readText()   // String | Error
val buf  = readStream("data.bin").collect()      // UInt8[] | Error
```

---

### `drain`

Terminal. Drives the pipeline on the **calling thread** and returns `Null` on normal completion (EOF) or `Error` if a read or write fails. This is the synchronous driver — no worker thread.

```lin
val outcome = readStream("in.csv")
  .lines()
  .writeLines("out.csv")
  .drain()
match outcome
  is Error => print("copy failed: ${outcome["message"]}")
  else     => print("done")
```

---

### `promise`

Terminal. Runs the whole pipeline on a **background thread** and returns a promise immediately, so your program can do other work while the stream is processed. `await` the promise to get the result — `Null` on success, or an `Error` if anything went wrong while processing (including a crash mid-stream, which is caught and handed back rather than taking down your program). Use `.drain()` instead when you just want to run the pipeline and wait for it.

`await` reattaches the `Null | Error` union, so the `Error` case must be handled (see [`std/async`](/stdlib/async.html)).

```lin
val p = readStream("big.log")
  .lines()
  .filter(isError)
  .writeLines("errors.log")
  .promise()
match await(p)
  is Error => print("pipeline failed")
  else     => print("done")
```

---

### `close`

Closes the stream now, releasing the file (or socket) it holds. **Optional** — a stream cleans up on its own once you're done with it; `close` is only for when you want to release the resource at a specific point rather than waiting for that. **Idempotent** — closing an already-closed or already-drained stream does nothing.

---

## See also

- [`std/iter`](/stdlib/iter.html) — the combinators (`map`/`filter`/`take`/`reduce`/`for`/…) that drive streams lazily.
- [Arrays & Iteration](/tutorials/06-arrays-and-iteration.html) — eager vs lazy iteration over arrays and streams.
