# Events

[`std/event`](/stdlib/event) provides typed event emitters in two layers. Both are **generic over
the event payload type** `T` — there is no `AnyVal` envelope and no string-tagged `{ type, data }`
shape: `emit`/`send` take a `T`, and listeners receive a `T`. If you need several event shapes
through one channel, make `T` a tagged union and `match` on it.

- **A `bus`** is a **synchronous, in-process** listener registry — decoupled pub/sub on one thread.
- **An `emitter`** is **asynchronous, worker-backed** — a long-lived worker owns some state and
  folds each delivered event into it on its own thread. This is the "stream in, process, emit"
  primitive.

The module learns from Node's `EventEmitter` mistakes: there is no magic `"error"` event that
crashes the process when unhandled, you **unsubscribe by handle** (not by fragile closure identity),
each event carries one typed payload (not positional varargs), and listener counts are queryable.

## The synchronous bus

A `bus` dispatches to its listeners immediately, on the calling thread. Create one with `bus`,
seeded with its first listener; register more with `on` (or `once`); fire with `emit`.

```lin
import { Bus, bus, on, once, off, emit, listenerCount } from "std/event"
import { toString } from "std/string"

val clicks: Bus<Int32> = bus((x: Int32): Null => print("logger: ${toString(x)}"))

val analytics = on(clicks, (x: Int32): Null => print("analytics: +1"))
once(clicks, (x: Int32): Null => print("welcome — first click only"))

emit(clicks, 10)          // all three fire; returns 3
emit(clicks, 20)          // the `once` listener is gone now; returns 2
off(clicks, analytics)    // unsubscribe by the handle `on` returned
emit(clicks, 30)          // only the logger remains; returns 1
```

A few things to note:

- **Annotate the bus binding once** — `val clicks: Bus<Int32> = …`. Lin infers type parameters from
  arguments, so this one annotation pins the payload type `T`; every later `emit`/`on`/`off` call
  resolves `T` from it. (To start with no listeners, write the literal directly:
  `val b: Bus<Int32> = { "entries": [], "nextId": 0 }`.)
- `on` and `once` return a **subscription handle** (`Sub`); pass it to `off` to unsubscribe. You
  never compare closures for identity.
- `emit` returns how many listeners fired — `0` if there are none, with no error. It also snapshots
  the listener list before firing, so a handler that subscribes or unsubscribes mid-dispatch does
  not perturb the round in progress.

The bus is generic over any payload, not just scalars — a record bus reads exactly the same:

```lin
type Leg = { "from": String, "to": String }

val legs: Bus<Leg> = bus((leg: Leg): Null => print("${leg["from"]} -> ${leg["to"]}"))
emit(legs, { "from": "A", "to": "B" })   // prints "A -> B"
```

## The async emitter

An `emitter` spawns a long-lived worker that owns a piece of **state `S`** and folds each delivered
event of type `T` into it with a reducer `(T, S) => S`. The reducer runs on the worker thread, one
event at a time — so the producer and the subscriber run concurrently, and the state needs no locks.

```lin
import { emitter, send, request, drain, stop } from "std/event"
import { toString } from "std/string"

type Hit = { "path": String, "ms": Int32 }

// reduce: (Hit, total) => total + ms. Seeded with 0. The 3rd argument is a sample Hit that
// pins the payload type T — it is never delivered to the reducer.
val sink = emitter(
  (h: Hit, total: Int32): Int32 => total + h["ms"],
  0,
  { "path": "", "ms": 0 }
)

send(sink, { "path": "/a", "ms": 12 })    // fire-and-forget
send(sink, { "path": "/b", "ms": 30 })

// `request` folds synchronously and returns the new state — a natural backpressure point.
val running: Int32 | Error = request(sink, { "path": "/c", "ms": 8 })

// `drain` blocks until every queued event is folded, then returns the final state.
val total: Int32 | Error = drain(sink, { "path": "", "ms": 0 })
stop(sink)

match total
  is Error => print("a fold faulted")
  else     => print("total ms: ${toString(total)}")   // 50
```

The verbs:

| Function | Blocks? | Returns | Use |
| --- | --- | --- | --- |
| `send(e, value)` | no | `Null` | Fire-and-forget; no backpressure |
| `request(e, value)` | yes | `S \| Error` | Fold one event, get the state back (backpressure) |
| `drain(e, sample)` | yes | `S \| Error` | Flush the queue, get the final state |
| `stop(e)` | yes | `Null` | Shut the worker down |

`request`/`drain` return `S | Error` because a fault inside the reducer is isolated at the worker
boundary and surfaces here as a value, rather than aborting the program — handle it with
`match … is Error`. The `sample` argument on `emitter`/`drain` exists only to pin the payload type
for inference; it is never folded.

> The emitter handle itself is an opaque `Worker` value (typed `AnyVal` — that is the runtime handle,
> never your payload). Everything you emit and fold stays fully typed.

## Putting it together: stream in, process, emit

This is where streams and events meet — the canonical use case. A producer **streams** a file on the
calling thread (see the [Streams tutorial](/tutorials/13-streams)), parses each line into a typed
record, and **sends** it to a subscriber worker that folds it into an index or a tally. The stream
stays on the producer thread; only the parsed, transferable record crosses to the worker, so parsing
and aggregation overlap and the whole feed is processed in constant memory.

```lin
import { readStream, lines } from "std/stream"
import { for } from "std/iter"
import { split, toString } from "std/string"
import { parseInt32 } from "std/number"
import { length } from "std/array"
import { emitter, send, drain, stop } from "std/event"

type Row = { "name": String, "score": Int32 }
val SAMPLE: Row = { "name": "", "score": 0 }

val main = (): Null =>
  // SUBSCRIBER: a worker that folds each Row into a running total.
  val sink = emitter((r: Row, total: Int32): Int32 => total + r["score"], 0, SAMPLE)

  // PRODUCER: stream the file, parse each row, send it to the worker.
  var header = true
  val streamResult = readStream("scores.csv").lines().for(line =>
    if length(line) > 0 then
      if header then
        header = false
      else
        val f = split(line, ",")
        send(sink, { "name": f[0], "score": parseInt32(f[1]) })
  )

  match streamResult
    is Error => print("stream failed")
    else =>
      val total: Int32 | Error = drain(sink, SAMPLE)
      stop(sink)
      match total
        is Error => print("aggregation faulted")
        else     => print("total score: ${toString(total)}")

main()
```

Swapping the source for an archive is the only change needed to read straight from a `.tar.gz` —
`readStream("feed.tar.gz").gunzip().untar((meta, data) => …)` — and the emitter half is identical.

> Lin has no statement separators: when a block's last expression has a value but the enclosing
> function is declared `: Null` (e.g. `main` ending on an `emit`, which returns the listener count),
> end it with a `print(...)` or a bare `null`.

## What's next?

- [std/event reference](/stdlib/event) — the full API and the generics notes.
- [Streams](/tutorials/13-streams) — the lazy I/O pipelines the producer side is built on.
- [Concurrency](/tutorials/09-concurrency) — workers, the primitive the async emitter is built on.
- The `examples/event-transfers/` project — a complete worked example (streaming a transfers file
  into a subscriber worker), with its own test suite.
