# Concurrency Reference

Import concurrency primitives from `std/async`:

```lin
import {
  async, await, parallel, race, timeout, retry,
  worker, message, request, close,
  threadPool, poolAsync,
  shared, get, set, withLock, frozen
} from "std/async"
```

## `Promise<T>`

An opaque runtime type representing a value of type `T` being computed on another OS thread.

`T` must be a **transferable** type: JSON-compatible values. The opaque types (`Function`, `Iterator`, `Worker`, `ThreadPool`, `Promise`) are not transferable.

## `async(thunk)`

Spawns the thunk on a new OS thread, returns a `Promise<T>` immediately:

```lin
val p = async(() => compute())
```

The thunk must be `() => T` (zero arguments). Thunks may not capture `var` bindings (compile-time error), and must return a transferable (JSON-shaped) value — returning a `Function` or `Iterator` is a compile-time error.

## `await(promise)`

Blocks the current thread until the promise resolves, returning `T | Error`:

```lin
val result = await(p)   // T | Error
```

A fault (runtime error) inside the thunk is caught at the thread boundary and surfaces as an `Error` value at `await`, so the result type is always `T | Error`.

`await` also accepts a `Promise[]` and returns a result array:

```lin
val [a, b] = await([asyncA, asyncB])
```

## `parallel(thunks)`

Runs all thunks concurrently and returns results in input order:

```lin
val [users, posts] = parallel([
  () => fetchUsers(),
  () => fetchPosts()
])
```

Same var-capture restriction as `async`.

## `race(promises)`

Resolves with the first promise to complete:

```lin
val fastest = race([mirror1Promise, mirror2Promise])
val data = await(fastest)
```

## `timeout(promise, ms)`

Resolves with the original value if completed within `ms` milliseconds, otherwise resolves to `Null`:

```lin
val result = await(timeout(longOp, 5000))
match result
  is Null  => print("timed out")
  is Error => print("failed")
  else     => print("got ${result}")
```

## `retry(thunk, n)`

Runs the thunk up to `n` times, returning the first non-Error result:

```lin
val data = await(retry(() => unstableNetwork(), 3))
```

## `Worker<Msg, Reply>`

A long-lived OS thread processing messages sequentially.

### `worker(handler, onClose)`

Create a worker:

```lin
val w = worker(
  (msg: String) => "echo: ${msg}",
  () => null
)
```

### `request(worker, msg)`

Send a message, wait for the reply:

```lin
val reply = request(w, "hello")
// or: val reply = w.request("hello")
```

### `message(worker, msg)`

Fire-and-forget — enqueues without waiting:

```lin
message(w, "background task")
```

### `close(worker)`

Waits for in-progress message to finish, calls `onClose`, terminates the thread:

```lin
close(w)
```

Workers may close over `var` bindings (safe because messages are processed one at a time).

## `ThreadPool`

### `threadPool(n)`

Create a pool of `n` threads:

```lin
val pool = threadPool(8)
```

### `poolAsync(pool, thunk)`

Submit a thunk to the pool, returning a `Promise`:

```lin
val p = poolAsync(pool, () => work())
// or, with dot application:
val p2 = pool.poolAsync(() => work())
val result = await(p)
```

For high-fan-out work, submit many thunks and await each promise:

```lin
import { range, push, for } from "std/array"

val promises = []
range(0, 100).for(i => push(promises, pool.poolAsync(() => work(i))))
```

## Shared state

`std/async` provides opt-in shared mutable state for cases where threads must coordinate.

### `shared(v)`, `get(s)`, `set(s, v)`, `withLock(s, f)`

`shared(v)` boxes a value into a `Shared` cell. `get` snapshots the value out, `set` copies a new value in, and `withLock` mutates it in place under a write lock:

```lin
import { shared, withLock, get } from "std/async"
import { parallel } from "std/async"
import { push } from "std/array"

val box = shared([])
parallel([
  () => withLock(box, a => push(a, 1)),
  () => withLock(box, a => push(a, 1))
])
val final = get(box)
```

Only the four accessors (`get`, `set`, `withLock`, plus `shared` to create one) may operate on a `Shared` value — using a non-accessor operation (indexing, `push`, …) on it is a compile-time type error.

### `frozen(v)`

`frozen(v)` deep-freezes a transferable graph into an immortal, immutable value that any thread can read by reference with no copies and no locks:

```lin
import { frozen } from "std/async"

val table = frozen([10, 20, 30, 40])
// read table[i] from any thread, lock-free
```

## Transferability rules

A value is transferable if it is:
- A JSON-compatible value: `String`, `Boolean`, `Null`, any numeric, `T[]` of transferable `T`, or an object with transferable values.
- A `Function` that closes over no `var` bindings.

Non-transferable: `Function` with `var` captures, `Iterator`, `Worker`, `ThreadPool`, `Promise`.
