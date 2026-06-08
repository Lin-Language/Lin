## Status: proposal (enriches existing std/time)

The current `std/time` module is timestamp-centric and UTC-only: it can read the
clock (`now`), wait (`sleep`), measure (`startTimer`/`elapsed`), and convert
between a Unix-millisecond `Int64` and ISO/strftime strings (`toIso`/`fromIso`/
`format`/`parse`). What it lacks is the *calendar* layer that Java
`java.time`, Python `datetime`/`timedelta`, and Rust `chrono` all provide:
breaking a timestamp into year/month/day fields, doing calendar-aware arithmetic
(add a month, add a year, with month-end clamping), and a first-class notion of a
duration. This proposal adds that layer **additively** — every existing function
is untouched and the canonical value stays the Unix-milliseconds `Int64`. New
functions are pure transforms over that `Int64`, so they compose with everything
already shipped (`addDays(now(), 1)`, `toIso(fromComponents(c))`, etc.). Calendar
math is genuinely fiddly (leap years, month lengths, the proleptic Gregorian
calendar), so the field<->timestamp core is implemented as a small set of Rust
intrinsics built on the standard civil-date algorithms; everything else is thin
pure-Lin wrapping.

### Design decisions (justified up front)

- **`month` is 1-12** (January = 1). This matches ISO 8601, the existing
  `format`/`parse` strftime semantics (`%m` is `01`-`12`), and `fromIso` output,
  so a value read from `components` round-trips through the string functions
  without an off-by-one. Java/Python's 1-12 convention wins over the C/JS 0-11
  convention precisely to stay consistent with the strings this module already
  emits.
- **`weekday` is 0 = Sunday … 6 = Saturday.** This is what the underlying civil
  algorithm computes natively (the `days_from_civil` weekday formula yields
  `0=Thursday` for the epoch, trivially rebased to Sunday-origin) and what most
  cron/locale tables assume. A separate `isoWeekday` (1 = Monday … 7 = Sunday) is
  *not* added in this proposal; if ISO-week support is wanted later it should
  arrive together with ISO week-number (`%V`) as its own follow-up.
- **`day` is 1-31, `hour` 0-23, `minute`/`second` 0-59, `millis` 0-999,
  `yearDay` 1-366** (1 = Jan 1; 366 only in leap years).
- **A Duration is just an `Int64` of milliseconds** — no new struct. This is the
  simplest thing that fits the existing model exactly: `now() - start` already
  *is* a duration, `sleep` already takes one, and `Int64` arithmetic
  (`hours(2) + minutes(30)`) just works with no wrapper. A dedicated `Duration`
  record would force boxing, lose the `+`/`-` operators, and diverge from the
  `elapsed`/`sleep` signatures already in use. The cost (no compile-time "this is
  ms not a timestamp" distinction) is acceptable; the constructor helpers
  (`hours`, `minutes`, …) document intent at the call site.

---

### components

```txt
val components: (ts: Int64) -> { "year": Int32, "month": Int32, "day": Int32, "hour": Int32, "minute": Int32, "second": Int32, "millis": Int32, "weekday": Int32, "yearDay": Int32 }
```

Breaks the Unix millisecond timestamp `ts` into its UTC calendar fields. `month`
is 1-12, `day` 1-31, `hour` 0-23, `minute`/`second` 0-59, `millis` 0-999,
`weekday` 0-6 (0 = Sunday), and `yearDay` 1-366 (1 = January 1). This never
fails: every `Int64` is a valid instant. Years before 1970 (negative `ts`) and
far-future years are handled by the proleptic Gregorian calendar, so `year` may
be negative.

```txt
components(0)
// { "year": 1970, "month": 1, "day": 1, "hour": 0, "minute": 0, "second": 0, "millis": 0, "weekday": 4, "yearDay": 1 }

components(1705314600000)
// { "year": 2024, "month": 1, "day": 15, "hour": 10, "minute": 30, "second": 0, "millis": 0, "weekday": 1, "yearDay": 15 }

components(now()).month   // e.g. 6
```

---

### fromComponents

```txt
val fromComponents: (c: { "year": Int32, "month": Int32, "day": Int32, "hour": Int32, "minute": Int32, "second": Int32, "millis": Int32 }) -> Int64 | Error
```

The inverse of `components`: builds a Unix millisecond timestamp (interpreted as
UTC) from calendar fields. `weekday` and `yearDay` are *outputs* of `components`
and are not part of the input — they are derived, so the input record omits them
(width subtyping means the full record returned by `components` is also accepted).
Returns an `Error` (the standard `{ "type": "error", "message": String }`,
discriminated with `is Error`) when any field is out of range: `month` outside
1-12, `day` outside 1 and the number of days in that month (leap-year aware), or
`hour`/`minute`/`second`/`millis` outside their ranges. Out-of-range fields are
rejected rather than normalised so that calendar typos surface as errors instead
of silently rolling over.

```txt
fromComponents({ "year": 2024, "month": 1, "day": 15, "hour": 10, "minute": 30, "second": 0, "millis": 0 })
// 1705314600000

fromComponents({ "year": 2024, "month": 2, "day": 29, "hour": 0, "minute": 0, "second": 0, "millis": 0 })
// 1709164800000   (2024 is a leap year)

fromComponents({ "year": 2023, "month": 2, "day": 29, "hour": 0, "minute": 0, "second": 0, "millis": 0 })
// { "type": "error", "message": "day 29 out of range for month 2 of 2023" }

fromComponents({ "year": 2024, "month": 13, "day": 1, "hour": 0, "minute": 0, "second": 0, "millis": 0 })
// { "type": "error", "message": "month 13 out of range" }
```

---

### year / month / day / hour / minute / second / weekday / dayOfYear

```txt
val year:      (ts: Int64) -> Int32
val month:     (ts: Int64) -> Int32
val day:       (ts: Int64) -> Int32
val hour:      (ts: Int64) -> Int32
val minute:    (ts: Int64) -> Int32
val second:    (ts: Int64) -> Int32
val weekday:   (ts: Int64) -> Int32
val dayOfYear: (ts: Int64) -> Int32
```

Convenience extractors that pull a single UTC field out of a timestamp. Each is
exactly `components(ts).<field>` and shares its ranges and conventions (`month`
1-12, `weekday` 0 = Sunday, `dayOfYear` 1-366). Prefer `components` when you need
several fields at once — it does the calendar decomposition once; reach for these
when you want just one.

```txt
year(1705314600000)       // 2024
month(1705314600000)      // 1
weekday(1705314600000)    // 1  (Monday)
dayOfYear(1705314600000)  // 15
```

---

### addDays / addHours / addMinutes / addSeconds

```txt
val addDays:    (ts: Int64, n: Int64) -> Int64
val addHours:   (ts: Int64, n: Int64) -> Int64
val addMinutes: (ts: Int64, n: Int64) -> Int64
val addSeconds: (ts: Int64, n: Int64) -> Int64
```

Add a fixed number of days/hours/minutes/seconds to a timestamp. These are exact
millisecond offsets (a day is always 86 400 000 ms — there is no DST in a UTC
model), so they are pure `Int64` arithmetic and `n` may be negative to subtract.
They never fail.

```txt
addDays(0, 1)       // 86400000        (1970-01-02T00:00:00Z)
addHours(0, -1)     // -3600000        (1969-12-31T23:00:00Z)
addMinutes(now(), 90)
addDays(fromIso("2024-01-15") |> orElse(0), 30)
```

---

### addMonths / addYears

```txt
val addMonths: (ts: Int64, n: Int64) -> Int64
val addYears:  (ts: Int64, n: Int64) -> Int64
```

Add whole calendar months or years, **clamping the day to the last valid day of
the target month**. Unlike `addDays`, a month is not a fixed number of
milliseconds, so this is calendar arithmetic: the time-of-day fields
(`hour`/`minute`/`second`/`millis`) are preserved, the year/month advance, and if
the original day does not exist in the target month it is clamped down. `n` may be
negative. `addYears(ts, n)` is defined as `addMonths(ts, n * 12)` and so applies
the same clamping (the only case is Feb 29 in a leap year + N years landing on a
non-leap year, which clamps to Feb 28). These never fail — clamping replaces what
would otherwise be an invalid date.

```txt
// Jan 31 + 1 month -> Feb has no 31st, clamp to the 28th (2023 is not a leap year)
addMonths(fromIso("2023-01-31") |> orElse(0), 1)
// 1677542400000  == 2023-02-28T00:00:00Z

// Jan 31 + 1 month in a leap year -> Feb 29
addMonths(fromIso("2024-01-31") |> orElse(0), 1)
// 1709164800000  == 2024-02-29T00:00:00Z

// Mar 31 - 1 month -> Feb, clamp to 28/29
addMonths(fromIso("2024-03-31") |> orElse(0), -1)
// == 2024-02-29T00:00:00Z

// Feb 29 + 1 year -> non-leap year, clamp to Feb 28
addYears(fromIso("2024-02-29") |> orElse(0), 1)
// == 2025-02-28T00:00:00Z

addMonths(now(), 18)   // a year and a half from now
```

---

### diff

```txt
val diff: (a: Int64, b: Int64) -> Int64
```

The signed duration from `a` to `b`, in milliseconds. Exactly `b - a` — provided
as a named, intention-revealing companion to the duration helpers and
`formatDuration`. Positive when `b` is later than `a`.

```txt
diff(0, 90000)              // 90000
diff(now(), addDays(now(), 1))   // 86400000
```

---

### diffDays

```txt
val diffDays: (a: Int64, b: Int64) -> Int64
```

The number of whole UTC calendar days from `a` to `b`. This counts *date
boundaries crossed*, not elapsed 24-hour spans: it compares the civil dates of
`a` and `b` (the day index `days_from_civil(y, m, d)` for each), so
`23:00`-to-`01:00` the next morning is `1` day even though only two hours elapsed.
Signed; negative when `b` precedes `a`.

```txt
diffDays(fromIso("2024-01-15") |> orElse(0), fromIso("2024-01-20") |> orElse(0))   // 5
diffDays(fromIso("2024-01-15T23:00:00Z") |> orElse(0), fromIso("2024-01-16T01:00:00Z") |> orElse(0))   // 1
diffDays(fromIso("2024-03-01") |> orElse(0), fromIso("2024-01-01") |> orElse(0))   // -60
```

---

### seconds / minutes / hours / days

```txt
val seconds: (n: Int64) -> Int64
val minutes: (n: Int64) -> Int64
val hours:   (n: Int64) -> Int64
val days:    (n: Int64) -> Int64
```

Duration constructors. A duration in `std/time` is just an `Int64` count of
milliseconds (the same representation as `elapsed`'s return and `sleep`'s
argument), so these are unit-scaling helpers that read at the call site:
`seconds(n) == n * 1000`, `minutes(n) == n * 60000`, `hours(n) == n * 3600000`,
`days(n) == n * 86400000`. Because durations are plain `Int64`, they add and
subtract with `+`/`-` and feed directly into `sleep`, `addDays`-style offsets, or
a timestamp.

```txt
sleep(seconds(2))                 // wait 2 seconds
hours(1) + minutes(30)            // 5400000  (a Duration of 1h30m, in ms)
now() + days(7)                   // one week from now, as a timestamp
minutes(-5)                       // -300000  (negative durations are fine)
```

---

### formatDuration

```txt
val formatDuration: (ms: Int64) -> String
```

Renders a millisecond duration as a compact human string using the largest
non-zero units, e.g. `"1h 30m"`. Units are days/hours/minutes/seconds/ms
(`d h m s ms`); zero-valued leading and trailing units are dropped, but a fully
zero duration renders as `"0ms"`. Negative durations are prefixed with `-`. This
is a display helper, not a parser — there is no `parseDuration` in this proposal.

```txt
formatDuration(0)            // "0ms"
formatDuration(1500)         // "1s 500ms"
formatDuration(5400000)      // "1h 30m"
formatDuration(90061000)     // "1d 1h 1m 1s"
formatDuration(-3600000)     // "-1h"
```

---

### componentsAt / formatAt

```txt
val componentsAt: (ts: Int64, offsetMinutes: Int32) -> { "year": Int32, "month": Int32, "day": Int32, "hour": Int32, "minute": Int32, "second": Int32, "millis": Int32, "weekday": Int32, "yearDay": Int32 }
val formatAt:     (ts: Int64, pattern: String, offsetMinutes: Int32) -> String
```

Fixed-offset variants of `components` and `format` for rendering a timestamp in a
zone that is a constant number of minutes from UTC. `offsetMinutes` is added to
the instant before decomposition, so `componentsAt(ts, -300)` yields the fields
an observer at UTC-5 (e.g. US Eastern *standard* time) would read, and
`formatAt(ts, "%H:%M", 330)` formats for UTC+5:30 (India). The returned fields
are the *local* wall-clock fields; the instant itself is unchanged. This covers
the common "show this in a known fixed offset" case without a timezone database.

```txt
// 2024-01-15T10:30:00Z viewed at UTC-5
componentsAt(1705314600000, -300).hour    // 5
formatAt(1705314600000, "%Y-%m-%d %H:%M", 330)   // "2024-01-15 16:00"  (UTC+5:30)
formatAt(now(), "%H:%M", 0)               // same as format(now(), "%H:%M")
```

**Full IANA timezone support is out of scope for this proposal.** A proper
`America/New_York`-style zone requires the IANA tz database (DST transition
tables, historical offset changes, leap-second policy) — that is a large,
data-bearing dependency (megabytes of zoneinfo, periodic updates) that does not
belong wired into the core `std/time` module by default. The recommendation is to
defer it to a separate optional module (e.g. `std/tz`) backed by the system
zoneinfo or a bundled `tzdb`, layered on top of these fixed-offset primitives.
The fixed-offset variants here are deliberately the floor that unblocks the
majority of real use (logging in a known office offset, server-side rendering for
a single region) without that weight.

---

## Implementation notes

**Duration = `Int64` (decided).** No new type, no boxing, no operator loss; the
duration helpers (`seconds`/`minutes`/`hours`/`days`) are one-line pure-Lin
multiplications and `formatDuration`/`diff`/`diffDays` build on the same `Int64`.
This is the recommended option and is what the spec above assumes throughout.

**What needs Rust intrinsics.** The civil-date core is the only fiddly part and
should be two new runtime intrinsics, both UTC, both pure:

- `lin_time_components(ts: Int64) -> Json` — decompose an instant into the fields
  record. Implementation: split `ts` into whole days and a millisecond
  remainder (`floor_div`/`floor_mod` so negative `ts` decomposes correctly, *not*
  truncating division), convert the day index to `(year, month, day)` via the
  proleptic-Gregorian **`civil_from_days`** algorithm (Howard Hinnant,
  *chrono-Compatible Low-Level Date Algorithms*), and derive `weekday` from the
  day index (`weekday_from_days`, rebased to 0 = Sunday) and `yearDay` from the
  day-of-year. This is ~25 lines of integer arithmetic with no external crate
  needed, but it is exactly the kind of code where a typo (leap-year boundary,
  the era/`doe`/`yoe` shifts) is invisible to ASan and only catches in tests, so
  it must have a round-trip test corpus (epoch, pre-epoch negatives, every
  month-end, Feb 29 in and out of leap years, year 0 / negative years).
- `lin_time_from_components(year, month, day, hour, minute, second, millis: Int32) -> Json`
  — the inverse, returning either the `Int64` timestamp (boxed) or the standard
  error record. Implementation: validate ranges (reject rather than normalise;
  `day` validated against a leap-year-aware days-in-month table), then
  **`days_from_civil`** (the inverse Hinnant algorithm) to get the day index, then
  combine with the time-of-day milliseconds. Range validation lives here so all
  callers (`fromComponents`, and the clamping done by `addMonths`) share one
  source of truth.

Using a Rust crate (`time` or `chrono`) is an option and would be battle-tested,
but the civil algorithms are tiny, dependency-free, and exactly the subset we
need; pulling in `chrono` mainly buys parsing/formatting we already have. The
existing `lin_time_format`/`lin_time_to_iso`/`lin_time_from_iso` intrinsics
already do internal civil-date math, so the new intrinsics should **share that
helper** rather than duplicate it — if the existing code uses a crate, reuse it;
if it hand-rolls, factor the civil functions into one runtime module.

**What is pure-Lin (no new intrinsics).** Everything else layers on top of those
two intrinsics plus existing `Int64` arithmetic:

- `year`/`month`/`day`/`hour`/`minute`/`second`/`weekday`/`dayOfYear` =
  `components(ts).<field>`.
- `addDays`/`addHours`/`addMinutes`/`addSeconds` = add a constant `Int64` offset.
- `addMonths` = `components(ts)`, advance `(year, month)` with `floor_div`/
  `floor_mod` over 12 (so negative `n` and underflow into the previous year work),
  clamp `day` to that month's length via a days-in-month helper, then
  `fromComponents`. Because the day is pre-clamped, the `fromComponents` call
  cannot fail and the result is the bare `Int64`; an internal `orElse` keeps the
  signature non-fallible. `addYears(ts, n) = addMonths(ts, n * 12)`.
- `diff` = `b - a`; `diffDays` = difference of the two civil day indices
  (`components` gives `(year, month, day)`; reuse `days_from_civil` — either
  expose it as a tiny third intrinsic `lin_time_day_index(ts) -> Int64` or compute
  `floor_div(ts, 86400000)` in Lin, which is the same day index — prefer the
  `floor_div` form, no new intrinsic).
- `seconds`/`minutes`/`hours`/`days` = unit multiplications; `formatDuration` =
  pure `Int64` decomposition + string building (sign, then largest-to-smallest
  unit assembly, trimming zero units).
- `componentsAt(ts, off) = components(ts + minutes(off))` reusing the duration
  helper; `formatAt(ts, p, off) = format(ts + off*60000, p)`. Both are one-liners
  over existing functions — the fixed-offset story needs **no** new intrinsic.

So the entire enrichment is **two new Rust intrinsics** (`components`,
`from_components`) plus pure-Lin glue; the timezone fixed-offset layer and the
duration layer add zero runtime surface. Tests must include the round-trip
`fromComponents(components(ts)) == ts` over the awkward corpus above, the
month-end clamping cases enumerated in `addMonths`, and the negative-timestamp /
pre-1970 path (the most common place civil-date code is wrong).
