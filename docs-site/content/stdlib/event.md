# std/event

std/event — typed event emitters, two layers, fully generic over the event payload type.

Unlike a dynamically-typed emitter, an event here carries a value of a static type `T` — no
`Json`, no string-tagged `{ type, data }` envelope. Each emitter/bus is parameterised by its
payload type, so `emit`/`send` only accept a `T` and listeners receive a `T`. Distinct event
kinds are distinct types; use a tagged-union `T` if you want several shapes through one bus.

Two layers, picked by whether you need concurrency:

```lin
Layer 1 — `emitter` (async, worker-backed). A long-lived worker owns the subscriber state `S`
  and folds each delivered `T` into it with a reducer `(T, S) => S` that runs on the worker
  thread. Use it when a producer on one thread feeds a subscriber on another — e.g. stream a
  file and `send` processed records to an indexer worker. `send` is fire-and-forget; `request`
  is synchronous (the producer waits for the fold, giving backpressure); `drain` flushes and
  returns the accumulated `S`; `stop` shuts the worker down.
```

```lin
Layer 2 — `bus` (synchronous, in-process). A listener registry dispatched on the calling
  thread. `on` registers a `(T) => Null` listener and returns a subscription handle; `off`
  removes it by that handle, not by closure identity; `emit` invokes every listener with a
  `T` synchronously and returns how many fired.
```

Generics note: Lin infers type parameters from arguments, and a generic record type does not
propagate its parameter back to a generic consumer. So annotate the bus binding once —
`val b: Bus<Int32> = bus(firstListener)` — and every other call (`on`, `emit`, `off`,
`listenerCount`) resolves `T` from that binding or its value argument. The async handle is the
one opaque value: a worker handle, never your payload.

There are two `emit`-like verbs because they are genuinely different operations: `emit` runs
listeners synchronously on the caller's thread and returns a count; `send` enqueues to another
thread and returns immediately. Distinct names keep the semantics honest.

Design notes: there is no magic `"error"` event that aborts the process when no listener is
present (an async reducer fault is isolated at the worker boundary). Unsubscribe by handle, not
by fragile closure identity. One typed payload per event, not positional varargs. Listener
counts are queryable rather than a silent leak.

## Reference

#### `Listener`

```lin
type Listener<T> = (T) => Null
```

A listener over a payload of type `T`.

#### `emitter`

```lin
val emitter = <T, S>(reduce: (T, S) => S, initial: S, sample: T): Json
```

Spawn a subscriber worker that owns state `S` and folds each delivered `T` into it. `reduce` runs
on the worker thread, sequentially, one event at a time, so the state is thread-confined and
needs no locks.
- **`reduce`** — `(T, S) => S`, applied per event on the worker thread.
- **`initial`** — the seed state `S`.
- **`sample`** — a representative payload value that pins `T` for the internal envelope; it is
  never delivered to `reduce`.
- **Returns** an opaque emitter handle: a worker handle, never a payload. `T`/`S` are inferred from
  `reduce`/`initial`/`sample`.

**Example:**

```lin
val sink = emitter((x: Int32, sum: Int32) => sum + x, 0, 0)   // T = S = Int32
```

**Example:**

```lin
send(sink, 10)   // send(sink, 5); fire-and-forget
```

**Example:**

```lin
val total: Int32 | Error = drain(sink, 0)   // 15 (once the Error arm is handled)
```

**Example:**

```lin
stop(sink)
```

#### `send`

```lin
val send = <T>(e: Json, value: T): Null
```

Fire-and-forget: enqueue `value` and return immediately. There is no backpressure, so a fast
producer can outpace a slow reducer and grow the queue.
- **`e`** — an emitter handle from `emitter`.
- **`value`** — the payload `T` to deliver (pins `T`).

#### `request`

```lin
val request = <T, S>(e: Json, value: T): S | Error
```

Synchronous: enqueue `value` and block until the reducer has folded it. Gives natural backpressure.
- **`e`** — an emitter handle from `emitter`.
- **`value`** — the payload `T` to deliver.
- **Returns** the resulting state `S`, or an `Error` injected at the worker boundary on a reducer
  fault. Annotate and handle the union accordingly.

#### `drain`

```lin
val drain = <T, S>(e: Json, sample: T): S | Error
```

Flush: block until the queue is fully processed and return the accumulated state. Because the
worker processes messages sequentially, the drain is served only after every prior `send`, so the
returned state reflects them all.
- **`e`** — an emitter handle from `emitter`.
- **`sample`** — a representative `T` that pins the envelope type (its `value` is ignored).
- **Returns** the accumulated state `S`, or an `Error`. Annotate the result, e.g.
  `val s: Sum | Error = drain(e, 0)`.

#### `stop`

```lin
val stop = (e: Json): Null
```

Shut the emitter's worker down (drain in-flight, run the no-op onClose, join).
- **`e`** — the emitter handle to stop. After `stop`, sending to the emitter is an error.

#### `Bus`

```lin
type Bus<T> = { "entries": Entry<T>[], "nextId": Int32 }
```

A synchronous event bus over payload type `T`.

#### `Sub`

```lin
type Sub = { "id": Int32 }
```

A subscription handle returned by `on`/`once`, consumed by `off`.

#### `bus`

```lin
val bus = <T>(first: Listener<T>): Bus<T>
```

Create a fresh bus seeded with its first listener.
- **`first`** — the seed `Listener<T>`, which pins `T` (avoids the zero-argument-inference gap).
- **Returns** a `Bus<T>`. Annotate the binding — `val b: Bus<Int32> = bus(h)` — so later bus-only
  calls (`emit`/`off`/`listenerCount`) resolve `T`. To start empty, write the literal directly:
  `val b: Bus<Int32> = { "entries": [], "nextId": 0 }`.

#### `on`

```lin
val on = <T>(b: Bus<T>, handler: Listener<T>): Sub
```

Register `handler` on the bus for its payload type.
- **`b`** — the bus.
- **`handler`** — the `Listener<T>` to add.
- **Returns** a `Sub` subscription handle to pass to `off` (a handle, not closure identity, makes
  `off` reliable).

#### `once`

```lin
val once = <T>(b: Bus<T>, handler: Listener<T>): Sub
```

Register `handler` like `on`, but auto-remove it after its first firing.
- **`b`** — the bus.
- **`handler`** — the one-shot `Listener<T>`.
- **Returns** a `Sub` subscription handle (usable with `off` before the first firing).

#### `off`

```lin
val off = <T>(b: Bus<T>, sub: Sub): Boolean
```

Remove the listener identified by `sub`.
- **`b`** — the bus.
- **`sub`** — the subscription handle returned by `on`/`once`.
- **Returns** `true` if a listener was removed, `false` if none matched.

#### `emit`

```lin
val emit = <T>(b: Bus<T>, value: T): Int32
```

Synchronously invoke every listener with `value`, in registration order; `once` listeners are
removed after firing.
- **`b`** — the bus.
- **`value`** — the payload `T` to dispatch.
- **Returns** the number of listeners invoked (0 if none; an empty bus does not crash).

**Example:**

```lin
val b: Bus<Int32> = bus(d => print("tick ${d}"))
```

**Example:**

```lin
val sub = on(b, d => print("also ${d}"))   // once(b, ...) fires at most once
```

**Example:**

```lin
emit(b, 1)   // all listeners fire; returns the count
```

**Example:**

```lin
off(b, sub)  // unsubscribe by handle; emit(b, 2) now fires one fewer
```

#### `listenerCount`

```lin
val listenerCount = <T>(b: Bus<T>): Int32
```

Count the listeners currently registered on the bus.
- **`b`** — the bus.
- **Returns** the number of registered listeners.
