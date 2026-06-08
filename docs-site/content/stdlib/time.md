# std/time

std/time — timestamps, timing, formatting, and UTC calendar arithmetic.

The canonical value is a Unix timestamp in milliseconds (UTC), held as an Int64. `now` reads the
wall clock; for measuring durations prefer `startTimer`/`elapsed`, which use a monotonic clock
unaffected by wall-clock adjustments. toIso/fromIso convert to and from ISO 8601; format/parse use
strftime-style patterns. The calendar layer (components / fromComponents, the single-field
extractors, and the addDays/addMonths/diff family) is pure UTC arithmetic over the same Int64 —
durations are likewise just milliseconds (seconds/minutes/hours/days), so they compose with +/-.
Full IANA timezone support (DST, historical offsets) is out of scope; componentsAt/formatAt take a
fixed UTC offset.

import { now, sleep, toIso, fromIso, format, parse, startTimer, elapsed } from "std/time"

## Reference

#### `now`

```lin
val now = (): Int64
```

The current wall-clock time as a Unix timestamp in milliseconds (UTC).
- **Example:** now()   // e.g. 1716825600000

#### `sleep`

```lin
val sleep = (ms: Int64): Null
```

Block the current thread for `ms` milliseconds.
- **`ms`** — how long to sleep, in milliseconds.

#### `sleepMicros`

```lin
val sleepMicros = (us: Int64): Null
```

Block the current thread for `us` microseconds (finer-grained than `sleep`).
- **`us`** — how long to sleep, in microseconds.

#### `startTimer`

```lin
val startTimer = (): Int64
```

Start a monotonic timer and return its opaque handle. Pair with `elapsed`; use this rather than
`now` for measuring durations, as it is unaffected by wall-clock adjustments.
- **Returns** a timer handle to pass to `elapsed`.
- **Example:** val t = startTimer()   // ...work... print("took ${elapsed(t)}ms")

#### `elapsed`

```lin
val elapsed = (timer: Int64): Int64
```

Read the time elapsed since a timer was started.
- **`timer`** — a handle from `startTimer`.
- **Returns** the elapsed time in milliseconds.

#### `toIso`

```lin
val toIso = (ms: Int64): String
```

Render a Unix-millisecond timestamp as an ISO 8601 UTC string.
- **`ms`** — the timestamp in Unix milliseconds.
- **Returns** the ISO 8601 representation.
- **Example:** toIso(0)   // "1970-01-01T00:00:00.000Z"

#### `format`

```lin
val format = (ms: Int64, pattern: String): String
```

Render a Unix-millisecond timestamp (UTC) with a strftime-style pattern.
- **`ms`** — the timestamp in Unix milliseconds.
- **`pattern`** — a strftime-style format string (e.g. "%Y-%m-%d").
- **Returns** the formatted string.
- **Example:** format(now(), "%Y-%m-%d")   // "2025-05-27"

#### `fromIso`

```lin
val fromIso = (s: String): Int64 | Error
```

Parse an ISO 8601 date/datetime string to a Unix-millisecond timestamp.
- **`s`** — the ISO 8601 string to parse.
- **Returns** the timestamp in Unix milliseconds, or an Error if `s` is not valid ISO 8601.
- **Example:** fromIso("2024-01-15T10:30:00Z")   // 1705313400000

#### `parse`

```lin
val parse = (s: String, pattern: String): Int64 | Error
```

Parse a date string against a strftime-style pattern to a Unix-millisecond timestamp (UTC).
- **`s`** — the string to parse.
- **`pattern`** — the strftime-style format the string is expected to match.
- **Returns** the timestamp in Unix milliseconds, or an Error if `s` does not match `pattern`.
- **Example:** parse("2024-01-15", "%Y-%m-%d")   // 1705276800000

#### `components`

```lin
val components = (ts: Int64): Json
```

Break a Unix-millisecond timestamp into its UTC calendar fields. Never fails — every Int64 is a
valid instant; pre-1970 (negative `ts`) and far-future years use the proleptic Gregorian
calendar, so `year` may be negative.
- **`ts`** — the timestamp in Unix milliseconds.
- **Returns** a Json object with stable integer fields year, month (1-12), day (1-31), hour (0-23),
  minute/second (0-59), millis (0-999), weekday (0=Sunday..6=Saturday), yearDay (1-366). Typed
  `Json` because that is how the runtime hands back the built object.

#### `fromComponents`

```lin
val fromComponents = (c: Json): Int64 | Error
```

Build a Unix-millisecond timestamp (UTC) from calendar fields. Out-of-range fields are rejected,
not normalised, so calendar typos surface as errors.
- **`c`** — a Json object; only year/month/day/hour/minute/second/millis are read. `weekday`/
  `yearDay` are ignored on input, so the full record from `components` is accepted unchanged.
- **Returns** the timestamp in Unix milliseconds, or an Error if any read field is out of range.

#### `year`

```lin
val year = (ts: Int64): Int32
```

Single-field UTC extractors. Each is `components(ts).<field>`; prefer `components` when you need
several fields at once (it decomposes only once). In all of them `ts` is a Unix-millisecond timestamp.
The UTC year (may be negative for pre-1 BCE instants).

#### `month`

```lin
val month = (ts: Int64): Int32
```

The UTC month, 1-12.

#### `day`

```lin
val day = (ts: Int64): Int32
```

The UTC day of the month, 1-31.

#### `hour`

```lin
val hour = (ts: Int64): Int32
```

The UTC hour, 0-23.

#### `minute`

```lin
val minute = (ts: Int64): Int32
```

The UTC minute, 0-59.

#### `second`

```lin
val second = (ts: Int64): Int32
```

The UTC second, 0-59.

#### `weekday`

```lin
val weekday = (ts: Int64): Int32
```

The UTC day of the week, 0=Sunday..6=Saturday.

#### `dayOfYear`

```lin
val dayOfYear = (ts: Int64): Int32
```

The UTC day of the year, 1-366.

#### `addDays`

```lin
val addDays = (ts: Int64, n: Int64): Int64
```

Add `n` whole days to a timestamp by a fixed exact-millisecond offset.
- **`ts`** — the timestamp in Unix milliseconds.
- **`n`** — the number of days to add (may be negative to subtract).
- **Returns** the shifted timestamp. Never fails (a UTC day is always 86_400_000 ms — no DST).

#### `addHours`

```lin
val addHours = (ts: Int64, n: Int64): Int64
```

Add `n` whole hours to a timestamp.
- **`ts`** — the timestamp in Unix milliseconds.
- **`n`** — the number of hours to add (may be negative).
- **Returns** the shifted timestamp.

#### `addMinutes`

```lin
val addMinutes = (ts: Int64, n: Int64): Int64
```

Add `n` whole minutes to a timestamp.
- **`ts`** — the timestamp in Unix milliseconds.
- **`n`** — the number of minutes to add (may be negative).
- **Returns** the shifted timestamp.

#### `addSeconds`

```lin
val addSeconds = (ts: Int64, n: Int64): Int64
```

Add `n` whole seconds to a timestamp.
- **`ts`** — the timestamp in Unix milliseconds.
- **`n`** — the number of seconds to add (may be negative).
- **Returns** the shifted timestamp.

#### `addMonths`

```lin
val addMonths = (ts: Int64, n: Int64): Int64
```

Add `n` whole calendar months, preserving the time-of-day fields and clamping `day` to the last
valid day of the target month (e.g. Jan 31 + 1mo -> Feb 28/29).
- **`ts`** — the timestamp in Unix milliseconds.
- **`n`** — the number of months to add (may be negative).
- **Returns** the shifted timestamp. Never fails — clamping replaces what would be an invalid date.

#### `addYears`

```lin
val addYears = (ts: Int64, n: Int64): Int64
```

Add `n` whole calendar years, with the same month-end clamping as addMonths (Feb 29 + N years
landing on a non-leap year clamps to Feb 28). Defined as addMonths(ts, n*12).
- **`ts`** — the timestamp in Unix milliseconds.
- **`n`** — the number of years to add (may be negative).
- **Returns** the shifted timestamp. Never fails.

#### `diff`

```lin
val diff = (a: Int64, b: Int64): Int64
```

The signed millisecond difference between two timestamps (exactly `b - a`).
- **`a`** — the earlier timestamp.
- **`b`** — the later timestamp.
- **Returns** `b - a` in milliseconds; positive when `b` is later.

#### `diffDays`

```lin
val diffDays = (a: Int64, b: Int64): Int64
```

The signed number of whole UTC calendar days between two timestamps. Counts date boundaries
crossed (the difference of the two civil day indices), not elapsed 24h spans.
- **`a`** — the earlier timestamp.
- **`b`** — the later timestamp.
- **Returns** the signed count of day boundaries from `a` to `b`.

#### `seconds`

```lin
val seconds = (n: Int64): Int64
```

Duration constructors. A duration in std/time is just an Int64 of milliseconds (the same
representation as `elapsed`/`sleep`), so these scale a unit count and compose with `+`/`-`.
Each takes a unit count `n` (may be negative) and returns the equivalent milliseconds.
`n` seconds as milliseconds.

#### `minutes`

```lin
val minutes = (n: Int64): Int64
```

`n` minutes as milliseconds.

#### `hours`

```lin
val hours = (n: Int64): Int64
```

`n` hours as milliseconds.

#### `days`

```lin
val days = (n: Int64): Int64
```

`n` days as milliseconds.

#### `formatDuration`

```lin
val formatDuration = (ms: Int64): String
```

Render a millisecond duration as a compact human string using the largest non-zero units
(`d h m s ms`). Display only — there is no matching parser.
- **`ms`** — the duration in milliseconds (may be negative).
- **Returns** the formatted string: zero units are dropped, a fully zero duration is "0ms", and a
  negative duration is prefixed with "-".

#### `componentsAt`

```lin
val componentsAt = (ts: Int64, offsetMinutes: Int32): Json
```

Like `components`, but for a fixed UTC offset: `offsetMinutes` is added to the instant before
decomposition, so the returned fields are the local wall-clock an observer at that constant
offset would read (the instant itself is unchanged). Full IANA tz (DST, historical offsets) is
out of scope.
- **`ts`** — the timestamp in Unix milliseconds.
- **`offsetMinutes`** — the UTC offset in minutes (e.g. -300 for UTC-5, 330 for UTC+5:30).
- **Returns** the calendar-fields Json object for that local time (same shape as `components`).

#### `formatAt`

```lin
val formatAt = (ts: Int64, pattern: String, offsetMinutes: Int32): String
```

Like `format`, but renders the local wall-clock at a fixed UTC offset (see componentsAt).
- **`ts`** — the timestamp in Unix milliseconds.
- **`pattern`** — a strftime-style format string.
- **`offsetMinutes`** — the UTC offset in minutes (e.g. 330 for UTC+5:30).
- **Returns** the formatted local-time string.
