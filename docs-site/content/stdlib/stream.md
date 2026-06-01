# std/stream

Lazy, fallible streams over OS resources — files, sockets, subprocess stdout, and stdin. A `Stream<T>` is an opaque runtime value built as a **lazy pull graph**: a *source* node (`readStream`), zero or more *adapters* (`lines`/`linesMax`/`chunks`, plus the [`std/iter`](/stdlib/iter.html) combinators `map`/`filter`/`take`/… which dispatch lazily on a stream receiver), and a *terminal* operation that drives the graph one item at a time with bounded memory.

Errors are threaded **in-band**: the first read error poisons the upstream and short-circuits to the terminal op, so error handling lives only at the terminal, not at every adapter.

```lin
import { readStream, writeStream, drain } from "std/stream"
import { map, filter, take, for } from "std/iter"

readStream("in.csv")
  .lines()
  .map(transform)
  .filter(notEmpty)
  .writeStream("out.csv")
  .drain()
```

> The generic combinators (`map`, `filter`, `take`, `drop`, `reduce`, `for`, …) are **not** part of `std/stream` — they come from [`std/iter`](/stdlib/iter.html) and dispatch to the lazy stream backend automatically when the receiver is a `Stream`. A stream pipeline imports its **sources and sinks** from `std/stream` and its **combinators** from `std/iter`.

Byte sources also come from other modules — `tcpStream` ([`std/net`](/stdlib/net.html)), `stdoutStream` ([`std/process`](/stdlib/process.html)), and `stdinStream` ([`std/io`](/stdlib/io.html)) all return `Stream<UInt8[]>` and feed the same adapters and terminals documented here.

## Lifetime — affine, single-use

A `Stream<T>` is an **affine resource**: it may be consumed at most once. Using it again after a terminal op (or after `.promise()` moves it to a worker) is a compile-time error. Dropping an unused stream is fine — the runtime closes the fd via an RC-drop finalizer.

In v1 a stream may live only in a `val`, a function parameter, or a return value. Storing one in an object/array field or a `var` is a compile-time error.

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
| `writeStream` | `(Stream<T>, String) -> Stream<T>` | sink | Write each item to a file; drive with a terminal |
| `readText` | `(Stream<UInt8[]>) -> String \| Error` | terminal | Drive to completion, return the contents as a `String` |
| `collect` | `(Stream<UInt8[]>) -> UInt8[] \| Error` | terminal | Drive to completion, return the contents as a `UInt8[]` |
| `drain` | `(Stream<T>) -> Null \| Error` | terminal | Drive the pipeline on the calling thread |
| `promise` | `(Stream<T>) -> Json` | terminal | Move the pipeline to a worker thread; return a promise |
| `close` | `(Stream<T>) -> Null` | — | Close the fd eagerly (idempotent) |

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

Builds a **sink** node that writes each upstream item to the file at `path`. It returns a `Stream` whose terminal op (`drain`/`promise`) runs the whole pipeline — pulling one item at a time and writing it — so memory stays bounded regardless of input size. Building the sink writes nothing; a terminal op must drive it.

```lin
readStream("in.csv")
  .lines()
  .map(transform)
  .writeStream("out.csv")
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
  .writeStream("out.csv")
  .drain()
match outcome
  is Error => print("copy failed: ${outcome["message"]}")
  else     => print("done")
```

---

### `promise`

Terminal. **Moves** the whole pipeline onto a **worker OS thread** and immediately returns a promise (conceptually `Promise<Null | Error>`, erased to `Json` like all promise handles). This gives real concurrency plus **fault isolation**: a runtime fault while the worker drives the stream is caught at the thread boundary and surfaces as an `Error` when the promise is awaited. Because the pipeline is moved (not copied) and the worker becomes its sole owner, non-atomic refcounting stays sound and the worker's RC-drop finalizer closes the fd.

`await` reattaches the `Null | Error` union, so the `Error` case must be handled (see [`std/async`](/stdlib/async.html)).

```lin
val p = readStream("big.log")
  .lines()
  .filter(isError)
  .writeStream("errors.log")
  .promise()
match await(p)
  is Error => print("pipeline failed")
  else     => print("done")
```

---

### `close`

Closes the underlying fd eagerly. **Idempotent** — closing an already-closed (or already-drained) stream is a no-op. Optional: the RC-drop finalizer closes the fd automatically when the last reference goes away; `close` is for callers who want deterministic timing rather than scope-end cleanup.

---

## See also

- [`std/iter`](/stdlib/iter.html) — the combinators (`map`/`filter`/`take`/`reduce`/`for`/…) that drive streams lazily.
- [Arrays & Iteration](/tutorials/06-arrays-and-iteration.html) — eager vs lazy iteration over arrays and streams.
