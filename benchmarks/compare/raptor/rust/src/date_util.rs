//! UTC date arithmetic (port of `new Date("YYYY-MM-DD")` + `query/DateUtil.ts`).
//!
//! JS parses `new Date("2018-10-16")` as UTC midnight. `getDateNumber` slices the
//! UTC ISO string into YYYYMMDD. `getDay()` returns the day-of-week (Sun=0..Sat=6).
//! Multi-day search calls `setDate(getDate() + 1)` which rolls month/year over.

use crate::gtfs::{DateNumber, DayOfWeek};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UtcDate {
    pub year: i64,
    pub month: i64, // 1..=12
    pub day: i64,   // 1..=31
}

impl UtcDate {
    /// Parse "YYYY-MM-DD".
    pub fn parse(s: &str) -> Self {
        let year = s[0..4].parse().unwrap();
        let month = s[5..7].parse().unwrap();
        let day = s[8..10].parse().unwrap();
        UtcDate { year, month, day }
    }

    /// `getDateNumber(date)` → YYYYMMDD integer.
    pub fn date_number(&self) -> DateNumber {
        self.year * 10000 + self.month * 100 + self.day
    }

    fn is_leap(year: i64) -> bool {
        (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
    }

    fn days_in_month(year: i64, month: i64) -> i64 {
        match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if Self::is_leap(year) {
                    29
                } else {
                    28
                }
            }
            _ => unreachable!("invalid month {month}"),
        }
    }

    /// Advance by one day, rolling month/year over (mirrors `setDate(getDate()+1)`).
    pub fn add_day(&mut self) {
        self.day += 1;
        if self.day > Self::days_in_month(self.year, self.month) {
            self.day = 1;
            self.month += 1;
            if self.month > 12 {
                self.month = 1;
                self.year += 1;
            }
        }
    }

    /// Day of week, Sunday = 0 .. Saturday = 6 (matches JS `getDay` for UTC dates).
    /// Computed via Sakamoto's algorithm.
    pub fn day_of_week(&self) -> DayOfWeek {
        let t = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
        let mut y = self.year;
        let m = self.month as usize;
        if m < 3 {
            y -= 1;
        }
        let dow = (y + y / 4 - y / 100 + y / 400 + t[m - 1] + self.day) % 7;
        dow as DayOfWeek
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_days_of_week() {
        // From the contract: these dates map to specific DOW.
        assert_eq!(UtcDate::parse("2018-10-16").day_of_week(), 2); // Tuesday
        assert_eq!(UtcDate::parse("2018-10-22").day_of_week(), 1); // Monday
        assert_eq!(UtcDate::parse("2019-04-18").day_of_week(), 4); // Thursday
        assert_eq!(UtcDate::parse("2019-04-23").day_of_week(), 2); // Tuesday
        assert_eq!(UtcDate::parse("2018-12-31").day_of_week(), 1); // Monday
    }

    #[test]
    fn date_number_format() {
        assert_eq!(UtcDate::parse("2018-10-16").date_number(), 20181016);
        assert_eq!(UtcDate::parse("2019-04-17").date_number(), 20190417);
    }

    #[test]
    fn day_rollover() {
        let mut d = UtcDate::parse("2018-12-31");
        d.add_day();
        assert_eq!(d, UtcDate::parse("2019-01-01"));

        let mut d = UtcDate::parse("2020-02-28");
        d.add_day();
        assert_eq!(d, UtcDate::parse("2020-02-29")); // leap year
        d.add_day();
        assert_eq!(d, UtcDate::parse("2020-03-01"));
    }
}
