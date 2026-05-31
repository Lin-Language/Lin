# std/tty

Terminal control: raw mode and non-blocking key input on stdin. Use it to build interactive terminal programs that respond to individual keystrokes rather than full lines.

```lin
import { rawMode, readKey } from "std/tty"
```

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `rawMode` | `(Boolean) -> Null \| Error` | Enable (`true`) or disable (`false`) raw mode |
| `readKey` | `() -> Int32 \| Null` | Read one byte from stdin, non-blocking |

---

### `rawMode`

`rawMode(true)` puts the terminal into raw mode: canonical line buffering and echo are disabled and reads become non-blocking. The original terminal settings are saved and restored exactly by `rawMode(false)`. If stdin is not a terminal (for example, a pipe), `rawMode` returns an `Error` object rather than failing.

```lin
rawMode(true)    // disable canonical mode + echo
// ... read keys ...
rawMode(false)   // restore original terminal settings
```

---

### `readKey`

Reads a single byte from stdin without blocking: returns the byte value (`0..255`) as an `Int32`, or `Null` if no key is currently available. Multi-byte sequences (arrow keys, function keys) arrive one byte at a time, so a reader reassembles escape sequences itself.

```lin
import { rawMode, readKey } from "std/tty"
import { print } from "std/io"

rawMode(true)
val k = readKey()
if k != null then print("key: ${k}") else print("no key ready")
rawMode(false)
```

---

### Polling loop

A real application polls `readKey` repeatedly, treating `Null` as "nothing yet" and sleeping briefly between polls (via `std/time`'s `sleepMicros`) to avoid busy-spinning.

```lin
import { rawMode, readKey } from "std/tty"
import { sleepMicros } from "std/time"
import { range, for } from "std/array"
import { print } from "std/io"

rawMode(true)
range(0, 1000000).for(i =>
  val k = readKey()
  if k != null then print("key: ${k}")
  else sleepMicros(2000)
)
rawMode(false)
```
