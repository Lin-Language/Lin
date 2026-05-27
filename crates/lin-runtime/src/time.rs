use std::time::{SystemTime, UNIX_EPOCH, Duration};
use crate::string::{LinString, lin_string_from_bytes};

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
    let mut remaining = secs;
    let mut year = 1970i32;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        let secs_in_year = days_in_year as i64 * 86400;
        if remaining < secs_in_year {
            break;
        }
        remaining -= secs_in_year;
        year += 1;
    }
    let months = [
        31i64,
        if is_leap(year) { 29 } else { 28 },
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
    ];
    let mut month = 1i32;
    for &days in &months {
        let secs_in_month = days * 86400;
        if remaining < secs_in_month {
            break;
        }
        remaining -= secs_in_month;
        month += 1;
    }
    let day = remaining / 86400 + 1;
    remaining %= 86400;
    let hour = remaining / 3600;
    remaining %= 3600;
    let min = remaining / 60;
    let sec = remaining % 60;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        year, month, day, hour, min, sec
    )
}

fn is_leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}
