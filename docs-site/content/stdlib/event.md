# std/event

Typed event emitters in two layers, **fully generic over the event payload type** `T` — no `Json`
envelope, no string-tagged `{ type, data }`. `emit`/`send` accept a `T`; listeners receive a `T`.
For several event shapes through one channel, make `T` a tagged union and `match` on it.

```lin
import { emitter, send, request, drain, stop } from "std/event"
import { Bus, bus, on, once, off, emit, listenerCount } from "std/event"
```

- **Layer 1 — `emitter` (async, worker-backed).** A long-lived worker (`std/async`) owns the
  subscriber state `S` and folds each delivered `T` into it with a reducer `(T, S) => S` running on
  the worker thread. Use it when a producer on one thread feeds a subscriber on another — e.g.
  stream a file and `send` processed records to an indexer worker.
- **Layer 2 — `bus` (synchronous, in-process).** A listener registry dispatched on the calling
  thread, for decoupled pub/sub within one thread.

Design notes (learning from Node's `EventEmitter`): no magic `"error"` event that aborts the
process when unhandled — an async reducer fault is isolated at the worker boundary; you
**unsubscribe by handle**, not by fragile closure identity; each event is **one typed payload**, not
positional varargs; listener counts are queryable.

> **Generics note.** Lin infers type parameters from *arguments*, and a generic record type does not
> propagate its parameter back to a generic consumer. So **annotate the bus binding once** —
> `val b: Bus<Int32> = bus(firstListener)` — and every other call resolves `T` from that binding or
> a value argument. The async layer takes a representative `sample: T` argument on `emitter`/`drain`
> purely to pin `T` (a worker handler cannot otherwise recover it). The worker handle itself is the
> one opaque value (a `Worker`, erased to `Json` — that is the runtime handle, never your payload).

The module exports the types `Listener<T>` (`= (T) => Null`), `Bus<T>`, and `Sub`.

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `emitter` | `<T, S>((T, S) -> S, initial: S, sample: T) -> Emitter` | Spawn a subscriber worker that folds each `T` into state `S` |
| `send` | `<T>(Emitter, value: T) -> Null` | Fire-and-forget a value (no backpressure) |
| `request` | `<T, S>(Emitter, value: T) -> S \| Error` | Synchronously fold a value, return the new state |
| `drain` | `<T, S>(Emitter, sample: T) -> S \| Error` | Flush the queue, return the accumulated state |
| `stop` | `(Emitter) -> Null` | Shut the worker down |
| `bus` | `<T>(first: Listener<T>) -> Bus<T>` | A synchronous bus seeded with its first listener |
| `on` | `<T>(Bus<T>, Listener<T>) -> Sub` | Register a listener; returns a subscription handle |
| `once` | `<T>(Bus<T>, Listener<T>) -> Sub` | Register a listener that fires at most once |
| `off` | `<T>(Bus<T>, Sub) -> Boolean` | Remove a listener by its handle; `true` if one was removed |
| `emit` | `<T>(Bus<T>, value: T) -> Int32` | Synchronously invoke every listener; returns the count |
| `listenerCount` | `<T>(Bus<T>) -> Int32` | How many listeners are registered |

---

## Layer 1 — async emitter

The reducer runs on the worker thread, one event at a time, so its state is thread-confined. `send`
is fire-and-forget — a fast producer can outrun a slow reducer and grow the queue; use `request` to
pace, or rely on the final `drain`. `request`/`drain` return `S | Error` (the worker boundary injects
`Error` on a reducer fault); annotate the binding to recover `S`.

```lin
import { emitter, send, drain, stop } from "std/event"

val sink = emitter((x: Int32, sum: Int32) => sum + x, 0, 0)   // T = Int32, S = Int32
send(sink, 10)
send(sink, 5)
val total: Int32 | Error = drain(sink, 0)                     // 15 (once the Error case is handled)
stop(sink)
```

### Mixing a stream with an emitter (the RAPTOR use case)

A producer streams a file on the calling thread, parses each row into a typed record, and `send`s it
to a subscriber worker. The stream stays on the producer thread; only the parsed, transferable record
crosses to the worker — so parsing and folding overlap, and a large feed is processed in constant
memory.

```lin
import { readStream, lines } from "std/stream"
import { for } from "std/iter"
import { emitter, send, drain, stop } from "std/event"
import { push } from "std/array"

type Transfer = { "origin": String, "destination": String, "duration": Int32 }
val SAMPLE: Transfer = { "origin": "", "destination": "", "duration": 0 }

val sink = emitter(
  (t: Transfer, acc: Transfer[]) =>
    push(acc, t)
    acc,
  [],
  SAMPLE
)

readStream("transfers.csv")
  .lines()
  .for(line => send(sink, parseTransfer(line)))   // emit a typed Transfer per row

val collected: Transfer[] | Error = drain(sink, SAMPLE)
stop(sink)
```

Reading from an archive is the same shape with a different source — swap `readStream(path).lines()`
for `readStream("data.tar.gz").gunzip().untar((meta, data) => …)` and the emitter half is identical.

---

## Layer 2 — synchronous bus

`bus(first)` is seeded with its first listener so its `T` is inferred (a zero-argument generic
constructor could not infer `T`); to start empty, write the literal directly,
`val b: Bus<Int32> = { "entries": [], "nextId": 0 }`. `emit` snapshots the listener list before
firing, so a handler that subscribes or unsubscribes during dispatch does not perturb the current
round. `emit` with no listeners returns `0` and does nothing — no Node-style crash on an unhandled
event.

```lin
import { Bus, bus, on, once, off, emit, listenerCount } from "std/event"

val b: Bus<Int32> = bus(d => print("tick ${d}"))
val sub = on(b, d => print("also ${d}"))
once(b, d => print("first only ${d}"))

emit(b, 1)              // all three fire; returns 3
emit(b, 2)              // once-listener is gone; returns 2
off(b, sub)             // unsubscribe by handle
emit(b, 3)              // returns 1
listenerCount(b)        // 1
```

The bus is generic over any payload type, not just scalars — a record bus works the same way:

```lin
type Leg = { "from": String, "to": String }
val legs: Bus<Leg> = bus(leg => print("${leg["from"]} -> ${leg["to"]}"))
emit(legs, { "from": "A", "to": "B" })   // prints "A -> B"; returns 1
```
