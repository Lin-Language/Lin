//! Calendar service logic (port of src/gtfs/Service.ts).

use std::collections::HashMap;

use crate::gtfs::{DateNumber, DayOfWeek};

/// GTFS calendar service.
#[derive(Debug, Clone)]
pub struct Service {
    start_date: DateNumber,
    end_date: DateNumber,
    /// days[dow] = runs on that day of week.
    days: HashMap<DayOfWeek, bool>,
    /// The include/exclude index: a key present with value `true` = include,
    /// present with `false` = exclude. The key-present-vs-truthy distinction is
    /// load-bearing (contract trap #7).
    dates: HashMap<DateNumber, bool>,
}

impl Service {
    pub fn new(
        start_date: DateNumber,
        end_date: DateNumber,
        days: HashMap<DayOfWeek, bool>,
        dates: HashMap<DateNumber, bool>,
    ) -> Self {
        Service {
            start_date,
            end_date,
            days,
            dates,
        }
    }

    /// Mirrors the JS:
    /// ```text
    /// dates[date] === true OR
    /// ( !hasOwn(dates, date) && startDate <= date && endDate >= date && days[dow] )
    /// ```
    pub fn runs_on(&self, date: DateNumber, dow: DayOfWeek) -> bool {
        // `this.dates[date]` is truthy only when the key is present with value true.
        if let Some(&true) = self.dates.get(&date) {
            return true;
        }

        !self.dates.contains_key(&date)
            && self.start_date <= date
            && self.end_date >= date
            && *self.days.get(&dow).unwrap_or(&false)
    }
}
