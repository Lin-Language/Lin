# std/tty

std/tty — terminal control: raw mode and non-blocking key input on stdin.

Use it to build interactive terminal programs that respond to individual keystrokes rather than
full lines. `rawMode(true)` disables canonical line buffering + echo and makes reads
non-blocking, saving the original settings so `rawMode(false)` restores them exactly. `readKey`
reads one byte without blocking (null when nothing is ready), so a real app polls it in a loop,
sleeping briefly between empty reads (e.g. via std/time's `sleepMicros`) to avoid busy-spinning.
Multi-byte sequences (arrow/function keys) arrive one byte at a time.

```lin
import { rawMode, readKey } from "std/tty"
```

## Reference

#### `rawMode`

```lin
val rawMode = (on: Boolean): Null | Error
```

Enable or disable terminal raw mode on stdin.
- **`on`** — `true` to enable raw mode, `false` to restore cooked mode.
- **Returns** `Null` on success, or an `Error` (e.g. when stdin is not a terminal), discriminated
  with `is Error`.

#### `readKey`

```lin
val readKey = (): Int32 | Null
```

Read a single key (one byte) from stdin without blocking.
- **Returns** the byte value as an `Int32`, or `null` if no key is available.
