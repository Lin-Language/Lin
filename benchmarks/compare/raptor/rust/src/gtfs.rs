//! GTFS data types (port of src/gtfs/GTFS.ts).

use std::rc::Rc;

use crate::service::Service;

/// StopID e.g. "NRW".
pub type StopId = String;

/// Time in seconds since midnight (may be greater than 24 hours).
pub type Time = i64;

/// Duration in seconds.
pub type Duration = i64;

/// `Number.MAX_SAFE_INTEGER` (2^53 - 1). The "infinity" arrival sentinel and the
/// default `Transfer.endTime`.
pub const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Sunday = 0, Monday = 1 ... Saturday = 6 (JS `Date.getDay`).
pub type DayOfWeek = u32;

/// Date stored as a number, e.g. 20181225.
pub type DateNumber = i64;

/// GTFS stop time.
#[derive(Debug, Clone, PartialEq)]
pub struct StopTime {
    pub stop: StopId,
    pub arrival_time: Time,
    pub departure_time: Time,
    pub pick_up: bool,
    pub drop_off: bool,
}

/// GTFS trip.
#[derive(Debug, Clone)]
pub struct Trip {
    pub trip_id: String,
    pub stop_times: Vec<StopTime>,
    pub service_id: String,
    pub service: Rc<Service>,
}

/// Leg with a duration instead of departure and arrival time.
#[derive(Debug, Clone, PartialEq)]
pub struct Transfer {
    pub origin: StopId,
    pub destination: StopId,
    pub duration: Duration,
    pub start_time: Time,
    pub end_time: Time,
}
