# std/time

Timestamps, timing, and formatting. All timestamps are Unix time in milliseconds.

```lin
import { now, sleep, sleepMicros, toIso, fromIso, format, parse, startTimer, elapsed } from "std/time"
```

The timer handle returned by `startTimer` is an `Int64` opaque token; pass it back to `elapsed`.

## Function reference

| Function | Signature | Description |
| --- | --- | --- |
| `elapsed` | `(Int64) -> Int64` | Milliseconds since timer was started |
| `format` | `(Int64, String) -> String` | Format timestamp with strftime pattern |
| `fromIso` | `(String) -> Int64 \| Error` | Parse ISO 8601 string to ms timestamp |
| `now` | `() -> Int64` | Current Unix timestamp in milliseconds |
| `parse` | `(String, String) -> Int64 \| Error` | Parse date string with format pattern |
| `sleep` | `(Int64) -> Null` | Block for n milliseconds |
| `sleepMicros` | `(Int64) -> Null` | Block for n microseconds |
| `startTimer` | `() -> Int64` | Start high-resolution elapsed timer |
| `toIso` | `(Int64) -> String` | Format timestamp as ISO 8601 |

---

### `now`

```lin
val ts = now()   // e.g. 1716825600000
```

---

### `sleep` / `sleepMicros`

```lin
sleep(1000)        // wait 1 second
sleepMicros(500)   // wait ~0.5 ms
```

---

### `toIso` / `fromIso`

```lin
toIso(0)       // "1970-01-01T00:00:00.000Z"
toIso(now())   // e.g. "2025-05-27T14:32:07.123Z"

fromIso("2024-01-15T10:30:00Z")   // 1705313400000
fromIso("2024-01-15")             // 1705276800000
fromIso("bad")                    // { "type": "failure", "error": "..." }
```

---

### `format` / `parse`

Uses strftime-style patterns:

```lin
format(now(), "%Y-%m-%d")           // "2025-05-27"
format(now(), "%H:%M:%S")           // "14:32:07"
format(now(), "%Y-%m-%dT%H:%M:%S")  // "2025-05-27T14:32:07"

parse("2024-01-15", "%Y-%m-%d")   // 1705276800000
```

---

### `startTimer` / `elapsed`

For high-resolution timing of code sections:

```lin
val t = startTimer()
doHeavyWork()
print("took ${elapsed(t)}ms")
```

---

### Measuring elapsed time without a timer

```lin
val start = now()
doWork()
val duration = now() - start
print("elapsed: ${duration}ms")
```
