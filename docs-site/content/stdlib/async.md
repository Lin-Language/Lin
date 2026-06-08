# std/async

std/async — concurrency primitives: async/await, workers, thread pools, and shared state.

  import { async, await, parallel, race, timeout, retry } from "std/async"
  import { worker, message, request, close } from "std/async"
  import { threadPool, poolAsync } from "std/async"
  import { shared, get, set, withLock, frozen } from "std/async"

`async(thunk)` runs a zero-arg thunk on a background thread and returns a `Promise`; `await(p)`
blocks for the result as `T | Error` (a fault in the thunk surfaces as the `Error` arm, which
you MUST handle — assigning straight to the bare value type is a compile-time error, spec
§32.2.2 / ADR-045). An async thunk may not capture `var` bindings (compile-time error where
detectable); WORKERS, being single-threaded, may close over `var`.

`worker(handler, onClose)` spawns a long-lived background thread for request/reply messaging;
`threadPool(n)` bounds concurrency to `n` in-flight thunks. `shared`/`get`/`set`/`withLock`
give opt-in shared MUTABLE state safe across threads; `frozen` deep-freezes a value into
lock-free read-only state any thread can share without copying.

`async` runs thunk `f` concurrently and returns an opaque `Promise<T>` handle; resolve it with
`await` (the value materialises only at the await site, where any fault is injected as an
`Error`). Example:

  val p = async(() => expensiveComputation())   // ... do other work ...
  match await(p) is Error => print("failed") else => print("${await(p)}")

## Reference

#### `async`

```lin
val async = (f: Json): Json
```


#### `await`

```lin
val await = <T>(p: T): T | Error
```

Resolve a promise to its value (spec §32.2.2).
- **`p`** — the `Promise<T>` handle to await.
- **Returns** the resolved value, or an `Error` if the thunk faulted. The `T | Error` union must be
  handled (e.g. `match … is Error => … else => …`); assigning it to a bare binding that ignores
  the Error arm is a compile-time error.

#### `parallel`

```lin
val parallel = (tasks: Json): Json[]
```

Run a list of thunks concurrently and collect their results in order.
- **`tasks`** — an array of zero-argument thunks (`(() => T)[]`, passed as `Json`).
- **Returns** a `Json[]` of the results, one per task, in input order.
- **Example:** val [a, b, c] = parallel([() => fetchUsers(), () => fetchPosts(), () => fetchComments()])

#### `race`

```lin
val race = (promises: Json): Json
```

Return a promise that resolves to the first of `promises` to settle.
- **`promises`** — an array of `Promise` handles (passed as `Json`).
- **Returns** an opaque `Promise` handle for the first settled result; resolve with `await`.
- **Example:** val fastest = await(race([async(() => fetchFrom("a")), async(() => fetchFrom("b"))]))

#### `timeout`

```lin
val timeout = (p: Json, ms: Int32): Json
```

Return a promise that fails with a timeout `Error` if `p` does not settle within `ms`.
- **`p`** — the promise handle to bound.
- **`ms`** — the timeout in milliseconds.
- **Returns** an opaque `Promise` handle; awaiting it yields `p`'s value or a timeout `Error`.
- **Example:** match await(timeout(longOp, 5000)) is Null => print("timed out") is Error => print("failed") else => print("ok")

#### `retry`

```lin
val retry = (f: Json, times: Int32): Json
```

Retry a faulting thunk up to `times` attempts.
- **`f`** — a zero-argument thunk `() => T` (passed as `Json`).
- **`times`** — the maximum number of attempts.
- **Returns** an opaque `Promise` handle; awaiting it yields the first success, or the last `Error`.
- **Example:** val data = await(retry(() => unstableFetch(), 3))

#### `threadPool`

```lin
val threadPool = (size: Int32): Json
```

Create a fixed-size pool of worker threads.
- **`size`** — the number of worker threads in the pool.
- **Returns** an opaque pool handle for `poolAsync`.

#### `poolAsync`

```lin
val poolAsync = (pool: Json, f: Json): Json
```

Submit thunk `f` to a thread pool for execution. A pool bounds concurrency: at most `n` thunks
run at once, and excess work queues until a worker frees up. Designed for the dot-call form
`pool.poolAsync(thunk)`.
- **`pool`** — a pool handle from `threadPool`.
- **`f`** — a zero-argument thunk `() => T` (passed as `Json`).
- **Returns** an opaque `Promise` handle for the result; resolve with `await`.
- **Example:** val pool = threadPool(8)
- **Example:** val result = await(pool.poolAsync(() => heavyWork()))

#### `worker`

```lin
val worker = (handler: Json, onClose: Json): Json
```

Spawn a long-lived worker thread that owns thread-confined state.
- **`handler`** — the per-message handler run on the worker thread.
- **`onClose`** — a cleanup callback run when the worker shuts down.
- **Returns** an opaque worker handle for `request`/`message`/`close`. Workers may close over `var`
  bindings (single-threaded, no races).
- **Example:** val w = worker((msg: String) => "echo: ${msg}", () => null)
- **Example:** val reply = request(w, "hello")   // "echo: hello"
- **Example:** message(w, "fire-and-forget")
- **Example:** close(w)

#### `request`

```lin
val request = (w: Json, msg: Json): Json
```

Send `msg` to a worker and BLOCK until it replies (synchronous; gives backpressure).
- **`w`** — a worker handle from `worker`.
- **`msg`** — the message payload.
- **Returns** the worker's reply (or an `Error` injected at the worker boundary on a handler fault).

#### `message`

```lin
val message = (w: Json, msg: Json): Null
```

Send `msg` to a worker FIRE-AND-FORGET (enqueue and return immediately; no reply).
- **`w`** — a worker handle from `worker`.
- **`msg`** — the message payload.

#### `close`

```lin
val close = (w: Json): Null
```

Shut a worker down (drain in-flight work, run its `onClose`, join the thread).
- **`w`** — the worker handle to close.

#### `shared`

```lin
val shared = (v: Json): Shared
```

Box a value into opt-in shared MUTABLE state (ADR-028 §2.3.1).
- **`v`** — the value to share; a private copy is boxed.
- **Returns** a `Shared` handle. Only the accessors below operate on it — any other op (push,
  indexing, …) on a `Shared` value is a compile-time type error (ADR-029).

#### `get`

```lin
val get = (s: Shared): Json
```

Snapshot the current value out of a `Shared` cell under the lock.
- **`s`** — the `Shared` handle.
- **Returns** a copy of the shared value.

#### `set`

```lin
val set = (s: Shared, v: Json): Null
```

Copy a new value into a `Shared` cell under the write lock.
- **`s`** — the `Shared` handle.
- **`v`** — the new value to store.

#### `withLock`

```lin
val withLock = (s: Shared, f: Function): Json
```

Mutate a `Shared` cell in place under the write lock.
- **`s`** — the `Shared` handle.
- **`f`** — a function that mutates the held value while the lock is held.
- **Returns** whatever `f` returned.
- **Example:** val counter = shared(0)
- **Example:** counter.withLock(n => n + 1)   // atomic increment
- **Example:** val current = get(counter)     // snapshot copy; set(counter, 100) replaces it

#### `frozen`

```lin
val frozen = (v: Json): Json
```

Deep-freeze a transferable graph into opt-in shared READ-ONLY state (ADR-028 §2.3.2).
- **`v`** — the value graph to freeze.
- **Returns** an immortal, immutable, lock-free-readable value (readers use the plain type).
- **Example:** val config = frozen({ "retries": 3, "timeout": 5000 })
