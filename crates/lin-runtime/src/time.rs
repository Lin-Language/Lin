use std::time::{SystemTime, UNIX_EPOCH, Duration};
use crate::string::{LinString, lin_string_from_bytes};
use crate::fs::{make_string, make_error_tagged, resolve_lin_str};
use crate::tagged::lin_box_int64;

/// Current Unix timestamp in milliseconds.
#[no_mangle]
pub extern "C" fn lin_time_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Block for n milliseconds.
#[no_mangle]
pub extern "C" fn lin_time_sleep(ms: i64) {
    std::thread::sleep(Duration::from_millis(ms as u64));
}

/// Block for n microseconds.
#[no_mangle]
pub extern "C" fn lin_time_sleep_micros(us: i64) {
    std::thread::sleep(Duration::from_micros(us as u64));
}

/// Return a monotonic timer value in milliseconds (for elapsed timing).
/// Uses the same clock as lin_time_now so subtraction gives elapsed ms.
#[no_mangle]
pub extern "C" fn lin_time_start_timer() -> i64 {
    lin_time_now()
}

/// Elapsed milliseconds since timer_start (which is also a ms timestamp).
#[no_mangle]
pub extern "C" fn lin_time_elapsed(timer_start: i64) -> i64 {
    lin_time_now() - timer_start
}

/// Format a Unix ms timestamp as ISO 8601 string: "YYYY-MM-DDTHH:MM:SS.mmmZ".
#[no_mangle]
pub unsafe extern "C" fn lin_time_to_iso(ms: i64) -> *mut LinString {
    let secs = ms / 1000;
    let millis = (ms % 1000).abs();
    let dt = format_unix_timestamp(secs);
    let s = format!("{}.{:03}Z", dt, millis);
    lin_string_from_bytes(s.as_ptr(), s.len() as u32)
}

fn format_unix_timestamp(secs: i64) -> String {
    let c = CivilDateTime::from_unix_secs(secs);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        c.year, c.month, c.day, c.hour, c.min, c.sec
    )
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

// ---------------------------------------------------------------------------
// Civil date/time <-> Unix seconds (UTC). Uses Howard Hinnant's well-known
// days_from_civil / civil_from_days algorithms (proleptic Gregorian), which are
// branch-light and valid across the full i64 range — no external date crate.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct CivilDateTime {
    year: i64,
    month: i64, // 1..=12
    day: i64,   // 1..=31
    hour: i64,  // 0..=23
    min: i64,   // 0..=59
    sec: i64,   // 0..=59
}

/// Days since 1970-01-01 for a civil (y, m, d). `m` in 1..=12, `d` in 1..=31.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146097 + doe - 719468
}

/// Civil (y, m, d) from days since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Euclidean division/modulo (floor), so negative timestamps split correctly.
fn div_floor(a: i64, b: i64) -> i64 {
    let q = a / b;
    if (a % b != 0) && ((a < 0) != (b < 0)) { q - 1 } else { q }
}
fn rem_floor(a: i64, b: i64) -> i64 {
    let r = a % b;
    if r != 0 && ((r < 0) != (b < 0)) { r + b } else { r }
}

impl CivilDateTime {
    fn from_unix_secs(secs: i64) -> Self {
        let days = div_floor(secs, 86400);
        let mut rem = rem_floor(secs, 86400);
        let (year, month, day) = civil_from_days(days);
        let hour = rem / 3600;
        rem %= 3600;
        let min = rem / 60;
        let sec = rem % 60;
        CivilDateTime { year, month, day, hour, min, sec }
    }

    /// Unix seconds for this civil date/time (UTC). Caller has validated ranges.
    fn to_unix_secs(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day) * 86400
            + self.hour * 3600
            + self.min * 60
            + self.sec
    }
}

const MONTH_DAYS: [i64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];

fn days_in_month(year: i64, month: i64) -> i64 {
    if month == 2 && is_leap(year) { 29 } else { MONTH_DAYS[(month - 1) as usize] }
}

const WEEKDAY_ABBR: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const WEEKDAY_FULL: [&str; 7] = [
    "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
];
const MONTH_ABBR: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MONTH_FULL: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September",
    "October", "November", "December",
];

/// 0 = Sunday .. 6 = Saturday, for days since the epoch (1970-01-01 was a Thursday = 4).
fn weekday_from_days(days: i64) -> i64 {
    rem_floor(days + 4, 7)
}

/// Expand a strftime-style `pattern` for civil time `c`. Supports the common
/// UTC-relevant specifiers; an unknown `%x` is emitted verbatim (including the `%`).
fn strftime(c: &CivilDateTime, pattern: &str) -> String {
    let days = days_from_civil(c.year, c.month, c.day);
    let wd = weekday_from_days(days) as usize;
    let mut out = String::with_capacity(pattern.len() + 16);
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('Y') => out.push_str(&format!("{:04}", c.year)),
            Some('m') => out.push_str(&format!("{:02}", c.month)),
            Some('d') => out.push_str(&format!("{:02}", c.day)),
            Some('e') => out.push_str(&format!("{:2}", c.day)),
            Some('H') => out.push_str(&format!("{:02}", c.hour)),
            Some('M') => out.push_str(&format!("{:02}", c.min)),
            Some('S') => out.push_str(&format!("{:02}", c.sec)),
            Some('y') => out.push_str(&format!("{:02}", rem_floor(c.year, 100))),
            Some('j') => {
                let doy = days - days_from_civil(c.year, 1, 1) + 1;
                out.push_str(&format!("{:03}", doy));
            }
            Some('I') => {
                let h12 = if c.hour % 12 == 0 { 12 } else { c.hour % 12 };
                out.push_str(&format!("{:02}", h12));
            }
            Some('p') => out.push_str(if c.hour < 12 { "AM" } else { "PM" }),
            Some('a') => out.push_str(WEEKDAY_ABBR[wd]),
            Some('A') => out.push_str(WEEKDAY_FULL[wd]),
            Some('b') | Some('h') => out.push_str(MONTH_ABBR[(c.month - 1) as usize]),
            Some('B') => out.push_str(MONTH_FULL[(c.month - 1) as usize]),
            Some('%') => out.push('%'),
            Some(other) => {
                // Unknown specifier: emit verbatim so the output is debuggable.
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// Parse `s` against a strftime-style `pattern`, accumulating fields into a
/// CivilDateTime. Returns Err(message) on any structural or range error.
/// Only numeric specifiers are accepted for parsing (textual month/day names are
/// not parsed back). Unspecified fields default to UTC midnight on 1970-01-01.
fn strptime(s: &str, pattern: &str) -> Result<CivilDateTime, String> {
    // Defaults: 1970-01-01T00:00:00.
    let mut year = 1970i64;
    let mut month = 1i64;
    let mut day = 1i64;
    let mut hour = 0i64;
    let mut min = 0i64;
    let mut sec = 0i64;

    let sb = s.as_bytes();
    let mut si = 0usize;
    let pchars: Vec<char> = pattern.chars().collect();
    let mut pi = 0usize;

    // Read up to `max` ASCII digits starting at si; advance si; require >= 1 digit.
    let read_num = |sb: &[u8], si: &mut usize, max: usize, field: &str| -> Result<i64, String> {
        let start = *si;
        let mut v: i64 = 0;
        let mut count = 0;
        while *si < sb.len() && count < max && sb[*si].is_ascii_digit() {
            v = v * 10 + (sb[*si] - b'0') as i64;
            *si += 1;
            count += 1;
        }
        if *si == start {
            Err(format!("expected a number for {} at position {}", field, start))
        } else {
            Ok(v)
        }
    };

    while pi < pchars.len() {
        let pc = pchars[pi];
        if pc == '%' {
            pi += 1;
            let spec = pchars.get(pi).copied().ok_or_else(|| "trailing '%' in pattern".to_string())?;
            pi += 1;
            match spec {
                'Y' => year = read_num(sb, &mut si, 4, "year")?,
                'y' => {
                    let yy = read_num(sb, &mut si, 2, "year")?;
                    // POSIX: 00-68 => 2000-2068, 69-99 => 1969-1999.
                    year = if yy <= 68 { 2000 + yy } else { 1900 + yy };
                }
                'm' => month = read_num(sb, &mut si, 2, "month")?,
                'd' | 'e' => day = read_num(sb, &mut si, 2, "day")?,
                'H' => hour = read_num(sb, &mut si, 2, "hour")?,
                'M' => min = read_num(sb, &mut si, 2, "minute")?,
                'S' => sec = read_num(sb, &mut si, 2, "second")?,
                '%' => {
                    if si < sb.len() && sb[si] == b'%' { si += 1; }
                    else { return Err("expected '%'".to_string()); }
                }
                other => return Err(format!("unsupported parse specifier %{}", other)),
            }
        } else {
            // Literal char: match exactly (compare bytes; pattern literals are ASCII here).
            let mut buf = [0u8; 4];
            let enc = pc.encode_utf8(&mut buf).as_bytes();
            if si + enc.len() <= sb.len() && &sb[si..si + enc.len()] == enc {
                si += enc.len();
            } else {
                return Err(format!("expected '{}' at position {}", pc, si));
            }
            pi += 1;
        }
    }

    validate_civil(year, month, day, hour, min, sec)?;
    Ok(CivilDateTime { year, month, day, hour, min, sec })
}

fn validate_civil(year: i64, month: i64, day: i64, hour: i64, min: i64, sec: i64) -> Result<(), String> {
    if !(1..=12).contains(&month) {
        return Err(format!("month {} out of range 1..=12", month));
    }
    let dim = days_in_month(year, month);
    if day < 1 || day > dim {
        return Err(format!("day {} out of range 1..={} for {:04}-{:02}", day, dim, year, month));
    }
    if !(0..=23).contains(&hour) {
        return Err(format!("hour {} out of range 0..=23", hour));
    }
    if !(0..=59).contains(&min) {
        return Err(format!("minute {} out of range 0..=59", min));
    }
    if !(0..=59).contains(&sec) {
        return Err(format!("second {} out of range 0..=59", sec));
    }
    Ok(())
}

/// Parse an ISO 8601 date/datetime. Accepts:
///   - `YYYY-MM-DD`                                (UTC midnight)
///   - `YYYY-MM-DDThh:mm[:ss[.fff]]` with optional `Z` / `±hh:mm` / `±hhmm` offset
/// A space may replace `T`. Returns Unix milliseconds.
fn parse_iso(s: &str) -> Result<i64, String> {
    let s = s.trim();
    let bytes = s.as_bytes();
    let err = || format!("invalid ISO 8601 datetime: {:?}", s);

    // Date part: YYYY-MM-DD (exactly 10 chars).
    if bytes.len() < 10 {
        return Err(err());
    }
    let date = &s[..10];
    let dparts: Vec<&str> = date.split('-').collect();
    if dparts.len() != 3 || dparts[0].len() != 4 || dparts[1].len() != 2 || dparts[2].len() != 2 {
        return Err(err());
    }
    let year: i64 = dparts[0].parse().map_err(|_| err())?;
    let month: i64 = dparts[1].parse().map_err(|_| err())?;
    let day: i64 = dparts[2].parse().map_err(|_| err())?;

    let mut hour = 0i64;
    let mut min = 0i64;
    let mut sec = 0i64;
    let mut millis = 0i64;
    let mut offset_secs = 0i64; // east-positive; subtract to get UTC

    if bytes.len() > 10 {
        let sep = bytes[10];
        if sep != b'T' && sep != b' ' {
            return Err(err());
        }
        let mut rest = &s[11..];

        // Trailing timezone: 'Z', or ±hh:mm / ±hhmm at the end.
        if let Some(stripped) = rest.strip_suffix('Z').or_else(|| rest.strip_suffix('z')) {
            rest = stripped;
        } else if let Some(pos) = rest.rfind(['+', '-']) {
            // Guard: only treat as offset if it looks like one (not part of time).
            let sign = if rest.as_bytes()[pos] == b'+' { 1 } else { -1 };
            let off = &rest[pos + 1..];
            let (oh, om) = if let Some((h, m)) = off.split_once(':') {
                (h, m)
            } else if off.len() == 4 {
                (&off[..2], &off[2..])
            } else if off.len() == 2 {
                (off, "0")
            } else {
                return Err(err());
            };
            let oh: i64 = oh.parse().map_err(|_| err())?;
            let om: i64 = om.parse().map_err(|_| err())?;
            offset_secs = sign * (oh * 3600 + om * 60);
            rest = &rest[..pos];
        }

        // Time: hh:mm[:ss[.fff]]
        let (clock, frac) = match rest.split_once('.') {
            Some((c, f)) => (c, Some(f)),
            None => (rest, None),
        };
        let tparts: Vec<&str> = clock.split(':').collect();
        if tparts.len() < 2 || tparts.len() > 3 {
            return Err(err());
        }
        hour = tparts[0].parse().map_err(|_| err())?;
        min = tparts[1].parse().map_err(|_| err())?;
        if tparts.len() == 3 {
            sec = tparts[2].parse().map_err(|_| err())?;
        }
        if let Some(f) = frac {
            // Use the first three fractional digits as milliseconds.
            let mut ms_str = String::new();
            for ch in f.chars().take(3) {
                if !ch.is_ascii_digit() { return Err(err()); }
                ms_str.push(ch);
            }
            while ms_str.len() < 3 { ms_str.push('0'); }
            millis = ms_str.parse().map_err(|_| err())?;
        }
    }

    validate_civil(year, month, day, hour, min, sec).map_err(|_| err())?;
    let c = CivilDateTime { year, month, day, hour, min, sec };
    Ok((c.to_unix_secs() - offset_secs) * 1000 + millis)
}

// ---------------------------------------------------------------------------
// FFI entry points: format / fromIso / parse
// ---------------------------------------------------------------------------

/// format: (ts_ms: Int64, pattern: String) => String. Strftime-style, UTC.
#[no_mangle]
pub unsafe extern "C" fn lin_time_format(ms: i64, pattern: *const u8) -> *mut LinString {
    let pat = resolve_lin_str(pattern).unwrap_or_default();
    let c = CivilDateTime::from_unix_secs(div_floor(ms, 1000));
    let s = strftime(&c, &pat);
    make_string(&s)
}

/// fromIso: (s: String) => Int64 | Error. Unix milliseconds, or an Error object.
#[no_mangle]
pub unsafe extern "C" fn lin_time_from_iso(s: *const u8) -> *mut u8 {
    let input = match resolve_lin_str(s) {
        Some(v) => v,
        None => return make_error_tagged("fromIso: invalid string"),
    };
    match parse_iso(&input) {
        Ok(ms) => lin_box_int64(ms),
        Err(e) => make_error_tagged(&e),
    }
}

/// parse: (s: String, pattern: String) => Int64 | Error. Unix milliseconds (UTC).
#[no_mangle]
pub unsafe extern "C" fn lin_time_parse(s: *const u8, pattern: *const u8) -> *mut u8 {
    let input = match resolve_lin_str(s) {
        Some(v) => v,
        None => return make_error_tagged("parse: invalid string"),
    };
    let pat = match resolve_lin_str(pattern) {
        Some(v) => v,
        None => return make_error_tagged("parse: invalid pattern"),
    };
    match strptime(&input, &pat) {
        Ok(c) => lin_box_int64(c.to_unix_secs() * 1000),
        Err(e) => make_error_tagged(&format!("parse '{}' with '{}': {}", input, pat, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_roundtrip_epoch() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    #[test]
    fn known_iso_timestamps() {
        // 2024-01-15T10:30:00Z == 1705314600000 (verified against `date -u`).
        assert_eq!(parse_iso("2024-01-15T10:30:00Z").unwrap(), 1_705_314_600_000);
        // bare date == UTC midnight 1705276800000.
        assert_eq!(parse_iso("2024-01-15").unwrap(), 1_705_276_800_000);
        // fractional seconds → milliseconds.
        assert_eq!(parse_iso("2024-01-15T10:30:00.250Z").unwrap(), 1_705_314_600_250);
    }

    #[test]
    fn iso_offset_is_respected() {
        // 10:30+01:00 is 09:30Z.
        assert_eq!(
            parse_iso("2024-01-15T10:30:00+01:00").unwrap(),
            parse_iso("2024-01-15T09:30:00Z").unwrap()
        );
    }

    #[test]
    fn iso_rejects_garbage() {
        assert!(parse_iso("not a date").is_err());
        assert!(parse_iso("2024-13-01").is_err()); // month out of range
        assert!(parse_iso("2024-02-30").is_err()); // day out of range
    }

    #[test]
    fn strptime_examples() {
        assert_eq!(strptime("2024-01-15", "%Y-%m-%d").unwrap().to_unix_secs() * 1000, 1_705_276_800_000);
        assert_eq!(
            strptime("15/01/2024 10:30", "%d/%m/%Y %H:%M").unwrap().to_unix_secs() * 1000,
            1_705_314_600_000
        );
        assert!(strptime("bad", "%Y-%m-%d").is_err());
        // literal mismatch and out-of-range fields are errors.
        assert!(strptime("2024-13-01", "%Y-%m-%d").is_err());
    }

    #[test]
    fn strftime_examples() {
        // 1705314600 s == 2024-01-15T10:30:00Z.
        let c = CivilDateTime::from_unix_secs(1_705_314_600);
        assert_eq!(strftime(&c, "%Y-%m-%d"), "2024-01-15");
        assert_eq!(strftime(&c, "%H:%M:%S"), "10:30:00");
        assert_eq!(strftime(&c, "%Y-%m-%dT%H:%M:%S"), "2024-01-15T10:30:00");
        // 2024-01-15 was a Monday.
        assert_eq!(strftime(&c, "%a"), "Mon");
        assert_eq!(strftime(&c, "%B"), "January");
        // literal '%' and unknown specifier emitted verbatim.
        assert_eq!(strftime(&c, "%H%%"), "10%");
    }

    #[test]
    fn format_roundtrips_with_to_iso_date() {
        // format's date portion agrees with the existing to_iso path.
        let c = CivilDateTime::from_unix_secs(1_705_314_600);
        assert!(format_unix_timestamp(1_705_314_600).starts_with(&strftime(&c, "%Y-%m-%dT%H:%M:%S")));
    }
}
