//! "HH:MM:SS" → seconds-from-midnight, cached (port of src/gtfs/TimeParser.ts).

use std::collections::HashMap;

use crate::gtfs::Time;

#[derive(Debug, Default)]
pub struct TimeParser {
    cache: HashMap<String, Time>,
}

impl TimeParser {
    pub fn new() -> Self {
        TimeParser::default()
    }

    pub fn get_time(&mut self, time: &str) -> Time {
        if let Some(&v) = self.cache.get(time) {
            return v;
        }

        let parts: Vec<&str> = time.split(':').collect();
        let hh: i64 = parts.first().map_or(0, |s| s.parse().unwrap_or(0));
        let mm: i64 = parts.get(1).map_or(0, |s| s.parse().unwrap_or(0));
        let ss: i64 = parts.get(2).map_or(0, |s| s.parse().unwrap_or(0));

        let seconds = hh * 60 * 60 + mm * 60 + ss;
        self.cache.insert(time.to_string(), seconds);

        seconds
    }
}
