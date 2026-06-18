# std/datetime

std/datetime — an immutable, structural calendar library (proleptic Gregorian, UTC).

Inspired by java.time and TC39 Temporal, but built entirely from Lin records:

```lin
Date      = { year, month, day }                         — a calendar day  (Temporal.PlainDate)
Time      = { hour, minute, second, millis }             — a wall-clock time (Temporal.PlainTime)
DateTime  = Date & Time                                  — a day + time     (Temporal.PlainDateTime)
Duration  = { millis }                                   — an exact elapsed span (always milliseconds)
Period    = { years, months, days }                      — a calendar span (months are not fixed-length)
Weekday   = 0 | 1 | … | 6                                — day of week (0=Sunday), with Sun..Sat constants
OffsetDateTime = DateTime & { offsetMinutes }            — a DateTime at a fixed UTC offset (java.time OffsetDateTime)
```

Because `DateTime` is the record intersection `Date & Time` (ADR-061), every `DateTime` is *also*
a `Date` and a `Time` by structural width-subtyping — you can pass one straight to any `Date`/`Time`
function, which reads only the fields it needs. (Likewise an `OffsetDateTime` is structurally a
`DateTime`.) All fields are `Int64`, so arithmetic composes without casts; `year` may be negative
(proleptic Gregorian, no year 0 gap — astronomical numbering).

All values are immutable: every "mutating" operation returns a fresh record. Construction validates
its fields and returns `T | Error` (the canonical `{ "type":"error", ... }`, matched with `is Error`);
the calendar shifts (`addDays`/`addMonths`/`addYears`/`addPeriod`) never fail — out-of-range days are
clamped to the month end (Jan 31 + 1mo -> Feb 28/29). `now`/`today`/`nowAt` are the only impure
functions (they read the wall clock via std/time); everything else is pure.

The core is UTC. `OffsetDateTime` + `withOffset`/`atOffset`/`toOffset`/`toInstant` add support for a
FIXED UTC offset (e.g. +05:30 year-round) — enough to read and render local civil time at a constant
offset, and to parse/emit ISO `±HH:MM`. This is NOT a named timezone: there is no DST and no
historical-offset handling. Full IANA timezone support (DST, `America/New_York`) is out of scope.

```lin
import { date, dateTime, now, today, addDays, addMonths, toIso, parseIso } from "std/datetime"
import { OffsetDateTime, withOffset, atOffset, toInstant, nowAt, toIsoOffsetDateTime } from "std/datetime"
```

## Reference

### Types

#### `Date`

```lin
type Date = { "year": Int64, "month": Int64, "day": Int64 }
```

A calendar day with no time-of-day. `month` is 1-12, `day` is 1-31. (Temporal.PlainDate.)

#### `Time`

```lin
type Time = { "hour": Int64, "minute": Int64, "second": Int64, "millis": Int64 }
```

A wall-clock time with no date. `hour` 0-23, `minute`/`second` 0-59, `millis` 0-999. (Temporal.PlainTime.)

#### `DateTime`

```lin
type DateTime = Date & Time
```

A day together with a time-of-day — the intersection of `Date` and `Time`, so a `DateTime` is usable
anywhere a `Date` or a `Time` is expected. (Temporal.PlainDateTime.)

#### `Duration`

```lin
type Duration = { "millis": Int64 }
```

An exact elapsed span, held as a whole number of milliseconds. Composes by addition and scaling.

#### `Period`

```lin
type Period = { "years": Int64, "months": Int64, "days": Int64 }
```

A calendar span in human units. Unlike `Duration` these are not fixed-length (a month varies), so a
`Period` is only meaningful when added to a specific `Date` via `addPeriod`.

#### `OffsetDateTime`

```lin
type OffsetDateTime = DateTime & { "offsetMinutes": Int64 }
```

A `DateTime` tagged with a fixed UTC offset in minutes (`offsetMinutes`, e.g. 330 for +05:30, -300
for -05:00). The `DateTime` fields are the LOCAL wall-clock at that offset; the instant they denote
is `wall - offset` (see `toInstant`). Because it is the intersection `DateTime & { offsetMinutes }`,
an `OffsetDateTime` is structurally also a `DateTime`/`Date`/`Time` — pass it to any of their
functions and the local wall-clock fields are read. This is a FIXED offset (java.time
`OffsetDateTime`, Temporal offset string), NOT a named zone: it has no DST, so +05:00 means +05:00
year-round. Full IANA timezone support is out of scope. (Temporal.OffsetDateTime / `java.time.OffsetDateTime`.)

#### `Weekday`

```lin
type Weekday = 0 | 1 | 2 | 3 | 4 | 5 | 6
```

The day of the week as a numeric literal union: 0=Sunday .. 6=Saturday. Returned by `weekday`,
so a `match` over it is exhaustively checked and only the seven valid values type-check. The
`Sun`..`Sat` constants below name each value.

#### `Sun`

```lin
val Sun: Weekday
```

Named `Weekday` constants — use these rather than bare integers for readable `match` arms and
comparisons (`weekday(d) == Sat`).

#### `Mon`

```lin
val Mon: Weekday
```


#### `Tue`

```lin
val Tue: Weekday
```


#### `Wed`

```lin
val Wed: Weekday
```


#### `Thu`

```lin
val Thu: Weekday
```


#### `Fri`

```lin
val Fri: Weekday
```


#### `Sat`

```lin
val Sat: Weekday
```


### Leap years and month lengths

#### `isLeapYear`

```lin
val isLeapYear = (year: Int64): Boolean
```

Whether `year` is a Gregorian leap year (divisible by 4, except centuries not divisible by 400).

**Example:**

```lin
isLeapYear(2000)  // true     isLeapYear(1900)  // false
```

#### `daysInMonth`

```lin
val daysInMonth = (year: Int64, month: Int64): Int64
```

The number of days in a given (year, month). `month` is 1-12; Feb is leap-aware.

**Example:**

```lin
daysInMonth(2024, 2)  // 29
```

### Civil <> epochday (Howard Hinnant's algorithm)

#### `toEpochDay`

```lin
val toEpochDay = (d: Date): Int64
```

These convert between a (year, month, day) triple and a count of days since the Unix epoch
(1970-01-01 = day 0). They are exact for the whole Int64 range and assume truncating division,
which Lin provides. http://howardhinnant.github.io/date_algorithms.html
The number of days from 1970-01-01 to the given civil date (negative before the epoch).

#### `fromEpochDay`

```lin
val fromEpochDay = (epochDay: Int64): Date
```

The civil `Date` for a count of days since 1970-01-01 (the inverse of `toEpochDay`).

### Construction (validating)

#### `date`

```lin
val date = (year: Int64, month: Int64, day: Int64): Date | Error
```

Build a validated `Date`. Rejects out-of-range fields rather than normalising them, so typos
surface as errors. `year` is unconstrained (proleptic Gregorian, may be negative).
- **Returns** the `Date`, or an `Error` if `month` is not 1-12 or `day` not in range for the month.

**Example:**

```lin
date(2024, 2, 29)  // ok (leap day)     date(2023, 2, 29)  // Error
```

#### `time`

```lin
val time = (hour: Int64, minute: Int64 = 0, second: Int64 = 0, millis: Int64 = 0): Time | Error
```

Build a validated `Time`. Trailing fields default to zero, so `time(10, 30)` is 10:30:00.000.
- **Returns** the `Time`, or an `Error` if any field is out of range.

#### `dateTime`

```lin
val dateTime = (year: Int64, month: Int64, day: Int64, hour: Int64 = 0, minute: Int64 = 0, second: Int64 = 0, millis: Int64 = 0): DateTime | Error
```

Build a validated `DateTime` from all fields at once. Trailing time fields default to zero.
- **Returns** the `DateTime`, or the first `Error` from validating the date or the time.

### Moving between Date / Time / DateTime

#### `dateOf`

```lin
val dateOf = (dt: DateTime): Date
```

The date portion of a `DateTime` (drops the time-of-day).

#### `timeOf`

```lin
val timeOf = (dt: DateTime): Time
```

The time portion of a `DateTime` (drops the calendar date).

#### `atTime`

```lin
val atTime = (d: Date, t: Time): DateTime
```

Combine a `Date` and a `Time` into a `DateTime`.

#### `atStartOfDay`

```lin
val atStartOfDay = (d: Date): DateTime
```

The `DateTime` at midnight (00:00:00.000) on the given `Date`.

### Unixmillisecond bridge

#### `toTimestamp`

```lin
val toTimestamp = (dt: DateTime): Int64
```

The Unix-millisecond timestamp (UTC) for a `DateTime` — the same canonical value std/time uses,
so the two libraries interoperate freely.

#### `fromTimestamp`

```lin
val fromTimestamp = (ts: Int64): DateTime
```

The `DateTime` (UTC) for a Unix-millisecond timestamp — the inverse of `toTimestamp`. Never fails:
every Int64 is a valid instant (negative is pre-1970).

#### `now`

```lin
val now = (): DateTime
```

The current wall-clock instant as a UTC `DateTime`. Impure (reads the system clock via std/time).

#### `today`

```lin
val today = (): Date
```

Today's date in UTC. Impure (reads the system clock).

### Fixed UTC offset (OffsetDateTime)

#### `withOffset`

```lin
val withOffset = (dt: DateTime, offsetMinutes: Int64): OffsetDateTime
```

Tag a local `DateTime` with a fixed UTC offset, producing an `OffsetDateTime`. The fields are taken
as the LOCAL wall-clock at `offsetMinutes`; the instant is computed as `wall - offset` by `toInstant`.
- **`offsetMinutes`** — the offset east of UTC in minutes (e.g. 330 for +05:30, -300 for -05:00).

**Example:**

```lin
dt.withOffset(330)  // tag a DateTime as +05:30 local
```

#### `offsetOf`

```lin
val offsetOf = (odt: OffsetDateTime): Int64
```

The fixed offset (minutes east of UTC) of an `OffsetDateTime`.

#### `toInstant`

```lin
val toInstant = (odt: OffsetDateTime): Int64
```

The UTC Unix-millisecond instant an `OffsetDateTime` denotes. Subtracts the offset from the local
wall-clock: 10:30 at +05:30 is the same instant as 05:00 UTC.

#### `atOffset`

```lin
val atOffset = (ts: Int64, offsetMinutes: Int64): OffsetDateTime
```

The `OffsetDateTime` a fixed-offset observer reads for a UTC instant. The local wall-clock is
`instant + offset`, tagged with the offset — the inverse of `toInstant`. Never fails.
- **`ts`** — the UTC Unix-millisecond instant.
- **`offsetMinutes`** — the observer's offset east of UTC, in minutes.

**Example:**

```lin
atOffset(0, -300)  // 1969-12-31T19:00 -05:00 (the epoch seen from UTC-5)
```

#### `toOffset`

```lin
val toOffset = (odt: OffsetDateTime, offsetMinutes: Int64): OffsetDateTime
```

Re-express an `OffsetDateTime` at a different fixed offset, preserving the instant (the local
wall-clock shifts so it denotes the same moment). `withOffset` REPLACES the offset keeping the
wall-clock; this keeps the instant and recomputes the wall-clock.

**Example:**

```lin
toOffset(atOffset(0, 0), -300)  // same instant, now read as -05:00
```

#### `toUtc`

```lin
val toUtc = (odt: OffsetDateTime): DateTime
```

The local wall-clock `DateTime` at UTC for an `OffsetDateTime` (drops the offset, shifting to the
UTC instant). Equivalent to `fromTimestamp(toInstant(odt))`.

#### `nowAt`

```lin
val nowAt = (offsetMinutes: Int64): OffsetDateTime
```

The current wall-clock as an `OffsetDateTime` at a fixed offset. Impure (reads the system clock).
- **`offsetMinutes`** — the offset east of UTC, in minutes.

### Computed fields

#### `weekday`

```lin
val weekday = (d: Date): Weekday
```

The day of the week as a `Weekday` (0=Sunday .. 6=Saturday). The civil-date math runs in `Int64`;
the `match` narrows the in-range result `0..6` to the literal union so callers get an exhaustive
type. The `else` is unreachable (`floorMod(_, 7)` is always 0-6) and returns `Sat` only to satisfy
totality.

**Example:**

```lin
weekday({ "year": 1970, "month": 1, "day": 1 })  // 4 (Thu)
```

#### `dayOfYear`

```lin
val dayOfYear = (d: Date): Int64
```

The day of the year, 1-366 (Jan 1 = 1).

### Calendar arithmetic (never fails; clamps monthend)

#### `addDays`

```lin
val addDays = (d: Date, n: Int64): Date
```

Shift a `Date` by `n` whole days (`n` may be negative). Exact day arithmetic, no clamping needed.

#### `addMonths`

```lin
val addMonths = (d: Date, n: Int64): Date
```

Shift a `Date` by `n` whole calendar months, clamping `day` to the last valid day of the target
month (e.g. Jan 31 + 1mo -> Feb 28/29). `n` may be negative.

#### `addYears`

```lin
val addYears = (d: Date, n: Int64): Date
```

Shift a `Date` by `n` whole calendar years, with the same month-end clamping as `addMonths`
(Feb 29 + 1yr in a non-leap year -> Feb 28). Defined as `addMonths(d, n * 12)`.

#### `addPeriod`

```lin
val addPeriod = (d: Date, p: Period): Date
```

Shift a `Date` by a `Period`: years and months first (with month-end clamping), then exact days.

### Duration

#### `millis`

```lin
val millis = (n: Int64): Duration
```

Construct a `Duration` from a unit count (`n` may be negative). They scale a single unit, so
`hours(2)` plus `minutes(30)` (via `plus`) is a 2½-hour span.

#### `seconds`

```lin
val seconds = (n: Int64): Duration
```

`n` seconds as a `Duration`.

#### `minutes`

```lin
val minutes = (n: Int64): Duration
```

`n` minutes as a `Duration`.

#### `hours`

```lin
val hours = (n: Int64): Duration
```

`n` hours as a `Duration`.

#### `days`

```lin
val days = (n: Int64): Duration
```

`n` days (fixed 24h each) as a `Duration`.

#### `plus`

```lin
val plus = (a: Duration, b: Duration): Duration
```

The sum of two durations.

#### `minus`

```lin
val minus = (a: Duration, b: Duration): Duration
```

The difference of two durations (`a - b`).

#### `scale`

```lin
val scale = (a: Duration, factor: Int64): Duration
```

A duration scaled by an integer factor.

#### `toMillis`

```lin
val toMillis = (d: Duration): Int64
```

The whole milliseconds in a duration.

#### `toSeconds`

```lin
val toSeconds = (d: Duration): Int64
```

The whole seconds in a duration (truncated toward zero).

#### `toMinutes`

```lin
val toMinutes = (d: Duration): Int64
```

The whole minutes in a duration (truncated toward zero).

#### `toHours`

```lin
val toHours = (d: Duration): Int64
```

The whole hours in a duration (truncated toward zero).

#### `toDays`

```lin
val toDays = (d: Duration): Int64
```

The whole days in a duration (truncated toward zero).

#### `between`

```lin
val between = (a: DateTime, b: DateTime): Duration
```

The signed exact `Duration` from `a` to `b` (`b - a` as milliseconds).

#### `plusDuration`

```lin
val plusDuration = (dt: DateTime, d: Duration): DateTime
```

Shift a `DateTime` by an exact `Duration` (`d` may be negative). Pure millisecond arithmetic.

#### `minusDuration`

```lin
val minusDuration = (dt: DateTime, d: Duration): DateTime
```

Shift a `DateTime` backward by an exact `Duration`.

### Period

#### `period`

```lin
val period = (years: Int64, months: Int64 = 0, numDays: Int64 = 0): Period
```

Construct a calendar `Period`. Trailing fields default to zero, so `period(1)` is one year.

### Comparison

#### `compareDate`

```lin
val compareDate = (a: Date, b: Date): Int64
```

Three-way compare of two `Date`s by calendar order: -1 if `a < b`, 0 if equal, 1 if `a > b`.

#### `compare`

```lin
val compare = (a: DateTime, b: DateTime): Int64
```

Three-way compare of two `DateTime`s by instant: -1 if `a < b`, 0 if equal, 1 if `a > b`.

#### `isBefore`

```lin
val isBefore = (a: DateTime, b: DateTime): Boolean
```

Whether `a` is strictly before `b` (by instant).

#### `isAfter`

```lin
val isAfter = (a: DateTime, b: DateTime): Boolean
```

Whether `a` is strictly after `b` (by instant).

### ISO 8601 formatting

#### `toIsoDate`

```lin
val toIsoDate = (d: Date): String
```

The ISO 8601 calendar-date string for a `Date`, e.g. "2024-01-15".

#### `toIsoTime`

```lin
val toIsoTime = (t: Time): String
```

The ISO 8601 time string for a `Time`, e.g. "10:30:00.500" (always with milliseconds).

#### `toIso`

```lin
val toIso = (dt: DateTime): String
```

The ISO 8601 date-time string for a `DateTime`, e.g. "2024-01-15T10:30:00.500".

#### `toIsoOffset`

```lin
val toIsoOffset = (offsetMinutes: Int64): String
```

The ISO 8601 offset suffix for `offsetMinutes`: "Z" for zero, otherwise "+HH:MM" / "-HH:MM".

**Example:**

```lin
toIsoOffset(330)  // "+05:30"     toIsoOffset(0)  // "Z"     toIsoOffset(-300)  // "-05:00"
```

#### `toIsoOffsetDateTime`

```lin
val toIsoOffsetDateTime = (odt: OffsetDateTime): String
```

The ISO 8601 date-time string for an `OffsetDateTime`, e.g. "2024-01-15T10:30:00.500+05:30" (the
local wall-clock followed by the offset suffix). Use `toIso` for the bare wall-clock without offset.

### ISO 8601 parsing

#### `parseIsoDate`

```lin
val parseIsoDate = (s: String): Date | Error
```

Parse an ISO 8601 calendar date "YYYY-MM-DD" (years 0000-9999) into a validated `Date`.
- **Returns** the `Date`, or an `Error` if the string is malformed or the date is invalid.

#### `parseIsoTime`

```lin
val parseIsoTime = (s: String): Time | Error
```

Parse an ISO 8601 time "HH:MM:SS" or "HH:MM:SS.sss" into a validated `Time`.
- **Returns** the `Time`, or an `Error` if the string is malformed or the time is invalid.

#### `parseIso`

```lin
val parseIso = (s: String): DateTime | Error
```

Parse an ISO 8601 date-time "YYYY-MM-DDTHH:MM:SS(.sss)?" (a trailing "Z" is accepted and ignored,
since these values are always UTC) into a validated `DateTime`.
- **Returns** the `DateTime`, or an `Error` if either half is malformed or out of range.

#### `parseIsoOffset`

```lin
val parseIsoOffset = (s: String): Int64 | Error
```

Parse an ISO 8601 offset suffix ("Z", "+HH:MM", or "-HH:MM") to minutes east of UTC. "Z" is 0.
- **Returns** the offset in minutes, or an `Error` if malformed or out of the ±18:00 range.

#### `parseIsoOffsetDateTime`

```lin
val parseIsoOffsetDateTime = (s: String): OffsetDateTime | Error
```

Parse an ISO 8601 date-time WITH an offset, "YYYY-MM-DDThh:mm:ss(.sss)?(Z|±HH:MM)", into an
`OffsetDateTime`. The wall-clock fields and the offset are taken as written (the local time at that
offset); use `toInstant` for the UTC instant. A missing offset is an error — use `parseIso` for the
offset-less form.
- **Returns** the `OffsetDateTime`, or an `Error` if any part is malformed, out of range, or the offset
  is absent.
