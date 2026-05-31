# std/signal

Minimal, blocking OS signal handling. `waitSignal` blocks the calling thread until a given signal is delivered, then returns the signal number — useful for graceful shutdown.

```lin
import { waitSignal } from "std/signal"
```

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `waitSignal` | `(Int32) -> Int32` | Block until signal `sig` is delivered; returns it |

---

### `waitSignal`

The signal is first blocked in the thread's mask and consumed with `sigwait`, so a signal that arrives during setup is not lost (no handler is installed). The mask is per-thread, and a single signal is waited on per call.

```lin
import { waitSignal } from "std/signal"
import { print } from "std/io"

val sig = waitSignal(2)   // block until SIGINT (Ctrl-C); returns 2
print("caught signal ${sig}")
```

---

### Graceful shutdown

A typical pattern: start background work, then block the main thread on `waitSignal` until the user interrupts, and clean up before exiting.

```lin
import { waitSignal } from "std/signal"
import { print, exit } from "std/io"

print("running; press Ctrl-C to stop")
waitSignal(2)             // SIGINT
print("shutting down")
exit(0)
```
