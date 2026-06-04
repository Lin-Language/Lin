//! JourneyFactory: kConnections → Journey[] (port of src/results/JourneyFactory.ts).

use std::rc::Rc;

use crate::journey::{Journey, Leg, TimetableLeg};
use crate::scan_results::{Connection, ConnectionIndex};

/// Trait for ResultsFactory (port of src/results/ResultsFactory.ts).
pub trait ResultsFactory {
    fn get_results(&self, k_connections: &ConnectionIndex, destination: &str) -> Vec<Journey>;
}

#[derive(Default)]
pub struct JourneyFactory;

impl JourneyFactory {
    pub fn new() -> Self {
        JourneyFactory
    }

    fn get_journey_legs(
        &self,
        k_connections: &ConnectionIndex,
        k: usize,
        final_destination: &str,
    ) -> Vec<Leg> {
        let mut legs: Vec<Leg> = Vec::new();
        let mut destination = final_destination.to_string();
        let mut i = k;

        while i > 0 {
            let connection = &k_connections[&destination][&i];

            match connection {
                Connection::Transfer(transfer) => {
                    let origin = transfer.origin.clone();
                    legs.push(Leg::Transfer(transfer.clone()));
                    destination = origin;
                }
                Connection::Trip(trip, start, end) => {
                    let stop_times = trip.stop_times[*start..=*end].to_vec();
                    let origin = stop_times[0].stop.clone();

                    legs.push(Leg::Timetable(TimetableLeg {
                        stop_times,
                        origin: origin.clone(),
                        destination: destination.clone(),
                        trip: Some(Rc::clone(trip)),
                    }));

                    destination = origin;
                }
            }

            i -= 1;
        }

        legs.reverse();
        legs
    }

    fn get_departure_time(&self, legs: &[Leg]) -> i64 {
        let mut transfer_duration = 0;
        for leg in legs {
            match leg {
                Leg::Transfer(t) => transfer_duration += t.duration,
                Leg::Timetable(l) => {
                    return l.stop_times[0].departure_time - transfer_duration;
                }
            }
        }
        0
    }

    fn get_arrival_time(&self, legs: &[Leg]) -> i64 {
        let mut transfer_duration = 0;
        for leg in legs.iter().rev() {
            match leg {
                Leg::Transfer(t) => transfer_duration += t.duration,
                Leg::Timetable(l) => {
                    return l.stop_times[l.stop_times.len() - 1].arrival_time + transfer_duration;
                }
            }
        }
        0
    }
}

impl ResultsFactory for JourneyFactory {
    fn get_results(&self, k_connections: &ConnectionIndex, destination: &str) -> Vec<Journey> {
        let mut results = Vec::new();

        if let Some(rounds) = k_connections.get(destination) {
            // BTreeMap iterates rounds in numeric ascending order (contract #1).
            for &k in rounds.keys() {
                let legs = self.get_journey_legs(k_connections, k, destination);
                let departure_time = self.get_departure_time(&legs);
                let arrival_time = self.get_arrival_time(&legs);
                results.push(Journey {
                    legs,
                    departure_time,
                    arrival_time,
                });
            }
        }

        results
    }
}
