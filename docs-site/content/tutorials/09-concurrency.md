# Concurrency

Lin's concurrency model follows the same philosophy as its iteration model: opaque runtime types, built-in functions, no new syntax. Crucially, functions do not carry an "async colour" — whether a function runs on a background thread is decided at the call site, not in the function's definition.

## `async` and `await`

`async` spawns a zero-argument thunk on a new OS thread and immediately returns a `Promise`:

```lin
import { async, await } from "std/async"

val p = async(() => expensiveComputation())
// ... do other work while computation runs ...
val result = await(p)
```

`await` blocks the caller until the promise resolves, returning `T | Error`.

The thunk must be a zero-argument function `() => T`, and `T` must be a transferable (JSON-shaped) value — a thunk may not return a `Function` or `Iterator`.

## No async colouring

In many languages, `async` is viral — calling an async function forces you to become async too. In Lin, a function is never *marked* async or sync; there is no `async def`. `async` and `await` are ordinary functions, and you decide at the **call site** whether to run a given function on a background thread. Any function can call `async`, and any function can call `await`.

## `parallel` — fork/join

Run an array of thunks concurrently and collect all results:

```lin
import { parallel } from "std/async"
import { print } from "std/io"

val [a, b, c] = parallel([
  () => fetchData("https://api.example.com/users"),
  () => fetchData("https://api.example.com/posts"),
  () => fetchData("https://api.example.com/comments")
])
```

Results come back in the same order as the input, regardless of completion order.

## Restrictions on async thunks

A thunk passed to `async` may not capture `var` bindings from its enclosing scope. This is a compile-time error:

```lin
var count = 0
val p = async(() =>
  count = count + 1   // error: async thunk captures var binding 'count'
)
```

This prevents data races. Use `val` bindings (which are immutable) for data you want to share across threads.

## Error handling in async

`await` returns `T | Error`: a runtime error inside the thunk is caught at the thread boundary and surfaced as an `Error` at `await` (rather than crashing the program). You **must** handle the `Error` case — the type checker rejects assigning an awaited result straight to the bare value type:

```lin
import { async, await } from "std/async"
import { print } from "std/io"

val p = async(() =>
  val xs = [1, 2, 3]
  xs[99]   // array index out of bounds — becomes an Error
)

val result = await(p)   // Int32 | Error
match result
  is Error => print("task failed")
  else     => print("got: ${result}")
```

Skipping the `match` and writing `val result: Int32 = await(p)` is a compile-time error — this is how Lin guarantees async faults are handled at the call site.

## Workers — long-lived stateful threads

A `Worker<Msg, Reply>` is a long-lived OS thread that processes messages sequentially. Use workers for stateful concurrency (caches, counters, connection pools):

```lin
import { worker, request, close } from "std/async"

val makeCounter = (): Json =>
  var count = 0
  worker(
    (msg: String) =>
      count = count + 1
      count,
    () => null
  )

val counter = makeCounter()
val n1 = counter.request("tick")   // 1
val n2 = counter.request("tick")   // 2
counter.close()
```

`request` sends a message and waits for the reply. `message` is fire-and-forget.

Workers may close over `var` bindings because they are single-threaded: messages are processed one at a time, so there are no concurrent accesses.

## Thread pools

For high-fan-out work, create a `ThreadPool` to distribute tasks across a fixed number of threads:

```lin
import { threadPool, poolAsync, await } from "std/async"

val pool = threadPool(8)
val p = pool.poolAsync(() => heavyWork())   // poolAsync(pool, thunk)
val result = await(p)
```

`pool.poolAsync(thunk)` is dot application for `poolAsync(pool, thunk)`.

## Shared state

For state that several threads must coordinate on, `std/async` provides `shared`/`get`/`set`/`withLock` (a lockable mutable cell) and `frozen` (a read-only graph shared lock-free by reference):

```lin
import { shared, withLock, get, parallel } from "std/async"
import { push, length } from "std/array"

val box = shared([])
parallel([
  () => withLock(box, a => push(a, 1)),
  () => withLock(box, a => push(a, 1))
])
val total = length(get(box))   // 2
```

## Summary

| Use case | Primitive |
| --- | --- |
| Background computation | `async` + `await` |
| Multiple results needed | `parallel` |
| Stateful background thread | `worker` + `request` |
| High-fan-out work | `threadPool` + `poolAsync` |
| Coordinated shared state | `shared` / `withLock` / `frozen` |
