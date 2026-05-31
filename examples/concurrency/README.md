# Concurrency tour

A small project exercising every async / threading primitive Lin provides (spec §24).

## Files

- **`tasks.lin`** — the core primitives, each as a small reusable function:
  `async`/`await`, `parallel` (order-preserving), `race`, `timeout`, `retry`,
  fault isolation (a thunk fault becomes an `Error` at `await`, matched with
  `is Error`), and nested-promise auto-flatten.
- **`services.lin`** — the higher-level primitives: a bounded `ThreadPool`
  (`poolAsync`), a long-lived `Worker` (request / confined `var` state /
  `onShutdown` / fault isolation), `Shared<T>` (atomic box + `RwLock`:
  `shared`/`get`/`set`/`withLock`), and `Frozen<T>` (lock-free shared reads).
- **`main.lin`** — a human-facing tour that prints each primitive's result.
- **`tasks.test.lin` / `services.test.lin`** — assertions for every exported
  helper (run with `lin test examples/concurrency/`).

## Run

```bash
lin run examples/concurrency/main.lin      # the printed tour
lin test examples/concurrency/             # the test suites
```

## Notes

- Thunks here capture top-level `val`s or function parameters — both are
  transferred to the worker thread by deep copy (Option C). The `Worker` handler
  deliberately closes over a `var` (§24.6.4): that state is confined to the worker
  thread, so it is safe.
- `Frozen<T>` data is shared by reference across threads with zero copies; the
  parallel readers all observe the same immortal graph.
- `Shared<T>`'s `set` collides by name with `std/array`'s `set`; this example
  imports `set` from `std/async` only.
