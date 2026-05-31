# std/async

Concurrency primitives: async/await, workers, thread pools.

```lin
import { async, await, parallel, race, timeout, retry } from "std/async"
import { worker, message, request, close } from "std/async"
import { threadPool, poolAsync } from "std/async"
import { shared, get, set, withLock, frozen } from "std/async"
```

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `async` | `(() -> T) -> Promise` | Run thunk on background thread |
| `await` | `(Promise) -> T \| Error` | Block until promise resolves; result must handle `Error` |
| `close` | `(Worker) -> Null` | Shut down worker |
| `frozen` | `(T) -> T` | Deep-freeze a value into lock-free shared read-only state |
| `get` | `(Shared) -> T` | Read a snapshot copy out of a `Shared` |
| `message` | `(Worker, Msg) -> Null` | Fire-and-forget message to worker |
| `parallel` | `((() -> T)[]) -> T[]` | Run array of thunks concurrently |
| `poolAsync` | `(ThreadPool, () -> T) -> Promise` | Enqueue a thunk on a thread pool |
| `race` | `(Promise[]) -> T` | First promise to complete wins |
| `request` | `(Worker, Msg) -> Reply` | Synchronous request/reply to worker |
| `retry` | `(() -> T, Int32) -> T` | Retry thunk up to n times |
| `set` | `(Shared, T) -> Null` | Replace a `Shared`'s value |
| `shared` | `(T) -> Shared` | Create opt-in shared mutable state |
| `threadPool` | `(Int32) -> ThreadPool` | Create thread pool |
| `timeout` | `(Promise, Int32) -> T` | Add timeout to a promise |
| `withLock` | `(Shared, (T) -> R) -> R` | Atomic read-modify-write on a `Shared` |
| `worker` | `((Msg) -> Reply, () -> Null) -> Worker` | Create background worker |

---

### `async` / `await`

```lin
val p = async(() => expensiveComputation())
// ... do other work ...
val result = await(p)   // T | Error
match result
  is Error => print("failed")
  else     => print("${result}")
```

`await` returns `T | Error` — a fault inside the thunk surfaces as an `Error` here. You must handle the `Error` case; assigning straight to the bare value type (`val n: Int32 = await(p)`) is a compile-time error (spec §32.2.2, ADR-070).

The thunk may not capture `var` bindings (compile-time error where detectable).

---

### `parallel`

```lin
val [a, b, c] = parallel([
  () => fetchUsers(),
  () => fetchPosts(),
  () => fetchComments()
])
```

---

### `race`

```lin
val fastest = await(race([
  async(() => fetchFrom("mirror-a")),
  async(() => fetchFrom("mirror-b"))
]))
```

---

### `timeout`

```lin
val result = await(timeout(longOp, 5000))
match result
  is Null  => print("timed out")
  is Error => print("failed")
  else     => print("ok: ${result}")
```

---

### `retry`

```lin
val data = await(retry(() => unstableFetch(), 3))
```

---

### `worker`

```lin
val w = worker(
  (msg: String) => "echo: ${msg}",
  () => null
)

val reply = request(w, "hello")   // "echo: hello"
message(w, "fire-and-forget")
close(w)
```

Workers may close over `var` bindings (single-threaded, no races).

---

### `threadPool` / `poolAsync`

A thread pool bounds concurrency: at most `n` thunks run at once, and excess work queues until a worker frees up. Enqueue work with `poolAsync(pool, thunk)`, designed for the dot-call form `pool.poolAsync(thunk)`.

```lin
val pool = threadPool(8)
val p = pool.poolAsync(() => heavyWork())
val result = await(p)

// Multiple tasks:
val results = parallel([
  () => pool.poolAsync(() => work(1)),
  () => pool.poolAsync(() => work(2)),
  () => pool.poolAsync(() => work(3))
])
```

---

### `shared` / `get` / `set` / `withLock`

`shared` wraps a value in opt-in shared mutable state safe to read and update across threads. `get` reads a snapshot, `set` replaces it, and `withLock` runs an atomic read-modify-write.

```lin
val counter = shared(0)

counter.withLock(n => n + 1)   // atomic increment
val current = get(counter)     // snapshot copy
set(counter, 100)              // replace
```

---

### `frozen`

`frozen` deep-freezes a value into lock-free read-only state that any thread can share without copying.

```lin
val config = frozen({ "retries": 3, "timeout": 5000 })
```
