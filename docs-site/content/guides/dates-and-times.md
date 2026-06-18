# Dates & Times

Lin handles time with two complementary modules:

- [`std/datetime`](/stdlib/datetime.html) is a calendar library — immutable `Date`, `Time`, and
  `DateTime` records, plus durations, periods, fixed UTC offsets, and ISO parsing/formatting. Reach
  for it whenever you need to reason about *calendar* values: "what day is this?", "add a month",
  "how many hours between these two moments?".
- [`std/time`](/stdlib/time.html) is the low-level wall clock — a Unix-millisecond timestamp
  (`Int64`, UTC), `sleep`, monotonic timers, and strftime-style formatting. Reach for it when you
  want a raw timestamp, want to *measure* elapsed time, or want to format/parse against a pattern.

The two interoperate: a `std/time` timestamp is the exact `Int64` `std/datetime` bridges with
`fromTimestamp` / `toTimestamp`.

```lin
import { date, time, dateTime, now, today } from "std/datetime"
import { addDays, addMonths, between, hours, toIso } from "std/datetime"
import { startTimer, elapsed, format } from "std/time"
```

> **Construction validates.** `date(...)`, `time(...)`, and `dateTime(...)` return `T | Error` — an
> out-of-range field (Feb 29 in a non-leap year, hour 24) comes back as the canonical
> `{ "type": "error", ... }`. You must narrow with `is Error` before using the value. The
> *arithmetic* functions (`addDays`, `addMonths`, …) never fail; they clamp instead.

## Creating dates and times

`date(year, month, day)`, `time(hour, minute?, second?, millis?)`, and the all-in-one
`dateTime(year, month, day, hour?, …)` build validated records. Trailing time fields default to
zero, so `time(10, 30)` is 10:30:00.000. Each returns `T | Error`, so match on the result:

```lin
import { print } from "std/io"
import { date, dateTime, now, today } from "std/datetime"
import { toIsoDate, toIso } from "std/datetime"

val d = date(2024, 1, 15)
match d
  is Error => print("invalid date: ${d["message"]}")
  else => print(toIsoDate(d))

val dt = dateTime(2024, 1, 15, 10, 30, 0)
match dt
  is Error => print("invalid: ${dt["message"]}")
  else => print(toIso(dt))

// now() / today() read the system clock and never fail — they return values directly.
val rightNow = now()
print(toIso(rightNow))
print(toIsoDate(today()))
```

`now()` returns a UTC `DateTime` and `today()` a UTC `Date`. Because `DateTime` is the record
intersection `Date & Time`, a `DateTime` is *also* a `Date` and a `Time` — you can pass one straight
to any `Date`- or `Time`-typed function (`toIsoDate(dt)`, `weekday(dt)`), and it reads only the
fields it needs.

### Unwrapping once

A `T | Error` has to be narrowed before use. When you genuinely expect valid input (a literal you
control), a tiny helper that substitutes a sentinel keeps the rest of the example readable:

```lin
import { Date } from "std/datetime"

val unwrapDate = (r: Date | Error): Date =>
  match r
    is Error => { "year": 1970, "month": 1, "day": 1 }
    else => r
```

The snippets below use this pattern. In real code, prefer to propagate the `Error` (or report it)
rather than swallow it — see [Error Handling](/tutorials/error-handling.html).

## Date arithmetic

`addDays`, `addMonths`, and `addYears` shift a `Date` by a whole number of units (negative counts go
backward). They never fail: `addMonths` and `addYears` **clamp** to the last valid day of the target
month, so Jan 31 + 1 month lands on Feb 29 (or Feb 28 in a non-leap year).

```lin
import { print } from "std/io"
import { Date, date, addDays, addMonths, addYears, addPeriod, period } from "std/datetime"
import { toIsoDate } from "std/datetime"

val unwrapDate = (r: Date | Error): Date =>
  match r
    is Error => { "year": 1970, "month": 1, "day": 1 }
    else => r

val d = unwrapDate(date(2024, 1, 31))

print(toIsoDate(addDays(d, 1)))        // 2024-02-01
print(toIsoDate(addDays(d, -40)))      // 2023-12-22
print(toIsoDate(addMonths(d, 1)))      // 2024-02-29 (clamped)
print(toIsoDate(addYears(d, 1)))       // 2025-01-31

val p = period(1, 2, 10)               // 1 year, 2 months, 10 days
print(toIsoDate(addPeriod(d, p)))
```

A `Period` (`period(years, months?, days?)`) is a *calendar* span — months and years are not
fixed-length, so a `Period` only means something once added to a concrete `Date` with `addPeriod`.
`addPeriod` applies the years and months first (with clamping), then the exact days.

## Durations and the span between two moments

A `Duration` is an **exact** elapsed span, held as whole milliseconds. Build one with `millis`,
`seconds`, `minutes`, `hours`, or `days`, compose with `plus` / `minus` / `scale`, and read it back
with `toMillis` / `toSeconds` / `toMinutes` / `toHours` / `toDays`. Apply one to a `DateTime` with
`plusDuration` / `minusDuration`, and get the signed span between two `DateTime`s with `between`:

```lin
import { print } from "std/io"
import { DateTime, dateTime } from "std/datetime"
import { hours, minutes, plus, days, toMillis, toHours } from "std/datetime"
import { plusDuration, minusDuration, between } from "std/datetime"
import { toIso } from "std/datetime"
import { toString } from "std/string"

val unwrapDt = (r: DateTime | Error): DateTime =>
  match r
    is Error => { "year": 1970, "month": 1, "day": 1, "hour": 0, "minute": 0, "second": 0, "millis": 0 }
    else => r

val start = unwrapDt(dateTime(2024, 1, 15, 9, 0, 0))

val span = plus(hours(2), minutes(30))   // a 2½-hour Duration
print(toString(toMillis(span)))          // 9000000

val later = plusDuration(start, span)
print(toIso(later))                      // 2024-01-15T11:30:00.000
print(toIso(minusDuration(start, days(1))))

val end = unwrapDt(dateTime(2024, 1, 16, 9, 0, 0))
val elapsed = between(start, end)        // signed Duration (b - a)
print(toString(toHours(elapsed)))        // 24
```

`Duration` (exact milliseconds) and `Period` (calendar units) are deliberately different types:
`hours(24)` is always 24 hours, but "1 month" depends on which month — use `Duration` for precise
time, `Period` for human calendar shifts.

## Weekday and day-of-year

`weekday(date)` returns a `Weekday` — the numeric literal union `0 | 1 | … | 6` (0 = Sunday … 6 =
Saturday) — with named constants `Sun`..`Sat`. Because the result is a literal union, a `match` over
it is exhaustively type-checked. `dayOfYear(date)` returns 1–366.

```lin
import { print } from "std/io"
import { Date, date, weekday, dayOfYear } from "std/datetime"
import { Sat } from "std/datetime"
import { toString } from "std/string"

val unwrapDate = (r: Date | Error): Date =>
  match r
    is Error => { "year": 1970, "month": 1, "day": 1 }
    else => r

val d = unwrapDate(date(2024, 1, 20))    // a Saturday

print(toString(weekday(d)))              // 6
print(toString(dayOfYear(d)))            // 20

val isWeekend = (day: Date): Boolean =>
  match weekday(day)
    is 0 => true     // Sunday
    is 6 => true     // Saturday
    else => false

print(toString(isWeekend(d)))            // true

// The named constants read better than bare integers:
if weekday(d) == Sat then print("it's the weekend") else print("a weekday")
```

## Time zones: fixed UTC offsets

The calendar core is UTC. For local civil time at a *fixed* offset, `std/datetime` adds
`OffsetDateTime` — a `DateTime` tagged with `offsetMinutes` (e.g. `330` for +05:30, `-300` for
−05:00). This models a constant offset year-round (java.time's `OffsetDateTime`); it is **not** a
named IANA zone, so there is no DST or historical-offset handling.

- `withOffset(dt, offsetMinutes)` tags a local `DateTime` with an offset.
- `toInstant(odt)` gives the UTC epoch-millis the value denotes (subtracting the offset).
- `atOffset(ts, offsetMinutes)` reconstructs the local `OffsetDateTime` an observer reads for a UTC
  instant.
- `toUtc(odt)` shifts to the UTC wall-clock `DateTime`; `nowAt(offsetMinutes)` is `now()` at an offset.

```lin
import { print } from "std/io"
import { DateTime, dateTime } from "std/datetime"
import { withOffset, toInstant, atOffset, toUtc, nowAt } from "std/datetime"
import { toIso, toIsoOffsetDateTime } from "std/datetime"
import { toString } from "std/string"

val unwrapDt = (r: DateTime | Error): DateTime =>
  match r
    is Error => { "year": 1970, "month": 1, "day": 1, "hour": 0, "minute": 0, "second": 0, "millis": 0 }
    else => r

// 10:30 local at +05:30 (offsetMinutes = 330).
val local = unwrapDt(dateTime(2024, 1, 15, 10, 30, 0))
val odt = withOffset(local, 330)

print(toIsoOffsetDateTime(odt))          // 2024-01-15T10:30:00.000+05:30
print(toString(toInstant(odt)))          // the UTC instant in epoch millis
print(toIso(toUtc(odt)))                 // 2024-01-15T05:00:00.000

// The current moment seen at a fixed offset:
val nyNow = nowAt(-300)                   // UTC-05:00
print(toIsoOffsetDateTime(nyNow))

// Reconstruct an OffsetDateTime from a UTC instant + offset:
val epochInNy = atOffset(0, -300)        // 1969-12-31T19:00 -05:00
print(toIsoOffsetDateTime(epochInNy))
```

An `OffsetDateTime` is structurally still a `DateTime`/`Date`/`Time`, so all the calendar functions
above work on it directly (they read the local wall-clock fields).

## Formatting and parsing

There are two ISO surfaces, matched to the two value kinds.

**Calendar records** (`std/datetime`) render and parse with `toIso` / `parseIso` (and the
date/time/offset variants `toIsoDate`, `toIsoTime`, `toIsoOffsetDateTime`, `parseIsoDate`,
`parseIsoTime`, `parseIsoOffsetDateTime`). Parsing validates, so a malformed *or* impossible value
returns an `Error`:

```lin
import { print } from "std/io"
import { DateTime, dateTime, toIso, parseIso } from "std/datetime"

val unwrapDt = (r: DateTime | Error): DateTime =>
  match r
    is Error => { "year": 1970, "month": 1, "day": 1, "hour": 0, "minute": 0, "second": 0, "millis": 0 }
    else => r

// Render a DateTime to ISO 8601:
val dt = unwrapDt(dateTime(2024, 1, 15, 10, 30, 45, 500))
print(toIso(dt))                         // 2024-01-15T10:30:45.500

// Parse one back — a malformed or invalid string returns an Error:
val parsed = parseIso("2024-01-15T10:30:45.500Z")
match parsed
  is Error => print("bad timestamp: ${parsed["message"]}")
  else => print(toIso(parsed))

// An invalid date is rejected, not silently normalised:
val bad = parseIso("2023-02-29T00:00:00Z")  // Feb 29 in a non-leap year
match bad
  is Error => print("rejected: ${bad["message"]}")
  else => print("(unreachable)")
```

**Raw timestamps** (`std/time`) render and parse with `toIso` / `fromIso` for ISO 8601, and
`format` / `parse` for strftime-style patterns — all operating on the epoch-millis `Int64`.
`fromIso` and `parse` are fallible and return `Int64 | Error`:

```lin
import { print } from "std/io"
import { now, toIso, fromIso, format, parse } from "std/time"
import { fromTimestamp, toIso as dtToIso } from "std/datetime"

// An epoch-millisecond timestamp:
val ms = now()                           // Int64 — e.g. 1716825600000

// Render it as ISO 8601 (UTC), or with a strftime-style pattern:
print(toIso(ms))                         // "2024-…T…Z"
print(format(ms, "%Y-%m-%d"))            // "2024-05-27"

// Parse strings back to epoch millis — both are fallible:
val a = fromIso("2024-01-15T10:30:00Z")  // Int64 | Error
match a
  is Error => print("not ISO 8601")
  else => print(toIso(a))

val b = parse("2024-01-15", "%Y-%m-%d")  // Int64 | Error
match b
  is Error => print("did not match pattern")
  else => print(toIso(b))

// Bridge to the calendar library: a std/time timestamp is the same Int64
// std/datetime uses, so fromTimestamp turns it into a DateTime record.
print(dtToIso(fromTimestamp(ms)))
```

## Measuring elapsed time

To *measure* how long something takes, do **not** subtract two `now()` timestamps — the wall clock
can jump (NTP adjustments, leap smearing). `std/time` provides a **monotonic** timer for exactly
this: `startTimer()` returns a handle, and `elapsed(handle)` reads the milliseconds since it started.

```lin
import { print } from "std/io"
import { startTimer, elapsed, sleep } from "std/time"
import { toString } from "std/string"

val timer = startTimer()
// ... do some work ...
sleep(50)
print("took ${toString(elapsed(timer))}ms")
```

Rule of thumb: use `now()` when you want to *record when* something happened (a timestamp to store
or display), and `startTimer` / `elapsed` when you want to *measure how long* it took (benchmarking,
timeouts).

## What's next?

- [Error Handling](/tutorials/error-handling.html) — narrowing `T | Error` the right way, instead of
  swallowing it with a sentinel.
- [Pattern Matching](/tutorials/pattern-matching.html) — the `match … is` narrowing used throughout.
- [std/datetime reference](/stdlib/datetime.html) — every calendar type and function.
- [std/time reference](/stdlib/time.html) — timestamps, timers, sleep, and pattern formatting.
