//! Test fixtures ported from test/unit/util.ts.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::gtfs::{StopTime, Time, Transfer, Trip, DayOfWeek, MAX_SAFE_INTEGER};
use crate::journey::{Journey, Leg, TimetableLeg};
use crate::service::Service;

pub fn all_days() -> HashMap<DayOfWeek, bool> {
    (0..7).map(|d| (d, true)).collect()
}

/// services["1"] and services["2"] from util.ts.
pub fn service1() -> Rc<Service> {
    Rc::new(Service::new(20180101, 20991231, all_days(), HashMap::new()))
}

pub fn service2() -> Rc<Service> {
    Rc::new(Service::new(20190101, 20991231, all_days(), HashMap::new()))
}

static TRIP_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// `t(...stopTimes)` — builds a Trip with serviceId "1".
pub fn t(stop_times: Vec<StopTime>) -> Rc<Trip> {
    let id = TRIP_COUNTER.fetch_add(1, Ordering::Relaxed);
    Rc::new(Trip {
        trip_id: format!("trip{id}"),
        stop_times,
        service_id: "1".to_string(),
        service: service1(),
    })
}

/// `st(stop, arr, dep)`:
/// arrivalTime = arr ?? dep, departureTime = dep ?? arr,
/// dropOff = arr !== null, pickUp = dep !== null.
pub fn st(stop: &str, arrival_time: Option<Time>, departure_time: Option<Time>) -> StopTime {
    StopTime {
        stop: stop.to_string(),
        arrival_time: arrival_time.or(departure_time).unwrap(),
        departure_time: departure_time.or(arrival_time).unwrap(),
        drop_off: arrival_time.is_some(),
        pick_up: departure_time.is_some(),
    }
}

/// `tf(o, d, dur)` — Transfer with startTime=0, endTime=MAX_SAFE_INTEGER.
pub fn tf(origin: &str, destination: &str, duration: Time) -> Transfer {
    Transfer {
        origin: origin.to_string(),
        destination: destination.to_string(),
        duration,
        start_time: 0,
        end_time: MAX_SAFE_INTEGER,
    }
}

/// A leg input to `j(...)`: either a list of stop times or a transfer.
pub enum LegInput {
    StopTimes(Vec<StopTime>),
    Transfer(Transfer),
}

/// `j(...legs)` — builds an expected Journey. Timetable legs get a `None` trip
/// (matching setDefaultTrip semantics: trip is ignored in equality).
pub fn j(legs: Vec<LegInput>) -> Journey {
    let departure_time = get_departure_time(&legs);
    let arrival_time = get_arrival_time(&legs);

    let built = legs
        .into_iter()
        .map(|leg| match leg {
            LegInput::Transfer(t) => Leg::Transfer(t),
            LegInput::StopTimes(stop_times) => {
                let origin = stop_times[0].stop.clone();
                let destination = stop_times[stop_times.len() - 1].stop.clone();
                Leg::Timetable(TimetableLeg {
                    stop_times,
                    origin,
                    destination,
                    trip: None,
                })
            }
        })
        .collect();

    Journey {
        legs: built,
        departure_time,
        arrival_time,
    }
}

fn get_departure_time(legs: &[LegInput]) -> Time {
    let mut transfer_duration = 0;
    for leg in legs {
        match leg {
            LegInput::Transfer(t) => transfer_duration += t.duration,
            LegInput::StopTimes(st) => return st[0].departure_time - transfer_duration,
        }
    }
    0
}

fn get_arrival_time(legs: &[LegInput]) -> Time {
    let mut transfer_duration = 0;
    for leg in legs.iter().rev() {
        match leg {
            LegInput::Transfer(t) => transfer_duration += t.duration,
            LegInput::StopTimes(st) => {
                return st[st.len() - 1].arrival_time + transfer_duration
            }
        }
    }
    0
}

/// `setDefaultTrip(results)` — set every timetable leg's trip to the default (None).
/// Our `journeys_equal` already ignores trip, so this is a no-op normaliser kept for
/// fidelity to the reference test flow.
pub fn set_default_trip(results: &mut [Journey]) {
    for journey in results {
        for leg in &mut journey.legs {
            if let Leg::Timetable(l) = leg {
                l.trip = None;
            }
        }
    }
}

// --- ergonomic constructors for stop-times in tests -----------------------

/// st with both arr and dep present.
pub fn stx(stop: &str, arr: Time, dep: Time) -> StopTime {
    st(stop, Some(arr), Some(dep))
}
