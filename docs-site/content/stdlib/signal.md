# std/signal

std/signal — minimal, blocking OS signal handling.

`waitSignal` blocks the calling thread until a given signal is delivered, then returns the
signal number — the building block for graceful shutdown (start background work, then block the
main thread on `waitSignal(2)` for SIGINT/Ctrl-C and clean up before exiting). The signal is
first blocked in the thread's mask and consumed with `sigwait`, so a signal that arrives during
setup is not lost; no handler is installed.

  import { waitSignal } from "std/signal"

## Reference

#### `waitSignal`

```lin
val waitSignal = (sig: Int32): Int32
```

Block until the given signal is delivered to this thread.
- **`sig`** — the signal number to wait for.
- **Returns** the signal number that was delivered.
