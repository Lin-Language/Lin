//! Journey / Leg types (port of src/results/Journey.ts + GTFS legs).

use std::rc::Rc;

use crate::gtfs::{StopId, StopTime, Time, Transfer, Trip};

/// A leg is either a timetable leg or a transfer.
#[derive(Debug, Clone)]
pub enum Leg {
    Timetable(TimetableLeg),
    Transfer(Transfer),
}

#[derive(Debug, Clone)]
pub struct TimetableLeg {
    pub stop_times: Vec<StopTime>,
    pub origin: StopId,
    pub destination: StopId,
    /// Ignored by journey equality (contract #8 — setDefaultTrip overwrites it).
    pub trip: Option<Rc<Trip>>,
}

#[derive(Debug, Clone)]
pub struct Journey {
    pub legs: Vec<Leg>,
    pub departure_time: Time,
    pub arrival_time: Time,
}

impl Leg {
    pub fn origin(&self) -> &str {
        match self {
            Leg::Timetable(l) => &l.origin,
            Leg::Transfer(t) => &t.origin,
        }
    }
}

/// Structural equality that ignores trip identity (contract #8/#9). Timetable legs
/// compare by stop_times + origin + destination; transfers by all their fields.
pub fn legs_equal(a: &Leg, b: &Leg) -> bool {
    match (a, b) {
        (Leg::Timetable(x), Leg::Timetable(y)) => {
            x.stop_times == y.stop_times && x.origin == y.origin && x.destination == y.destination
        }
        (Leg::Transfer(x), Leg::Transfer(y)) => x == y,
        _ => false,
    }
}

pub fn journeys_equal(a: &Journey, b: &Journey) -> bool {
    a.departure_time == b.departure_time
        && a.arrival_time == b.arrival_time
        && a.legs.len() == b.legs.len()
        && a.legs.iter().zip(b.legs.iter()).all(|(x, y)| legs_equal(x, y))
}

pub fn journey_lists_equal(a: &[Journey], b: &[Journey]) -> bool {
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| journeys_equal(x, y))
}
