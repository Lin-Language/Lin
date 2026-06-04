//! RaptorAlgorithm + RaptorAlgorithmFactory
//! (port of src/raptor/RaptorAlgorithm.ts + RaptorAlgorithmFactory.ts).

use std::rc::Rc;

use indexmap::IndexMap;

use crate::date_util::UtcDate;
use crate::gtfs::{DayOfWeek, StopId, Time, Transfer, Trip};
use crate::queue::{QueueFactory, RouteId, RouteStopIndex, RoutesIndexedByStop};
use crate::route_scanner::{RouteScannerFactory, TripsIndexedByRoute};
use crate::scan_results::{Arrivals, ConnectionIndex, ScanResultsFactory};

pub type RoutePaths = IndexMap<RouteId, Vec<StopId>>;
pub type Interchange = IndexMap<StopId, Time>;
pub type TransfersByOrigin = IndexMap<StopId, Vec<Transfer>>;
/// origin departure times keyed by stop.
pub type StopTimes = IndexMap<StopId, Time>;

const OVERTAKING_ROUTE_SUFFIX: &str = "overtakes";
const DEFAULT_INTERCHANGE_TIME: Time = 0;

pub struct RaptorAlgorithm {
    route_stop_index: RouteStopIndex,
    route_path: RoutePaths,
    transfers: TransfersByOrigin,
    interchange: Interchange,
    scan_results_factory: ScanResultsFactory,
    routes_at_stop: RoutesIndexedByStop,
    route_scanner_factory: RouteScannerFactory,
}

/// JS truthiness for a `Time`: present and non-zero.
fn truthy(t: Option<Time>) -> Option<Time> {
    match t {
        Some(v) if v != 0 => Some(v),
        _ => None,
    }
}

impl RaptorAlgorithm {
    pub fn scan(
        &self,
        origins: &StopTimes,
        date: i64,
        dow: DayOfWeek,
    ) -> (ConnectionIndex, Arrivals) {
        let mut route_scanner = self.route_scanner_factory.create(date, dow);
        let mut results = self.scan_results_factory.create(origins);
        let mut marked_stops: Vec<StopId> = origins.keys().cloned().collect();

        while !marked_stops.is_empty() {
            results.add_round();

            // --- scanRoutes ---
            let queue = {
                let qf = QueueFactory::new(&self.routes_at_stop, &self.route_stop_index);
                qf.get_queue(&marked_stops)
            };

            for (route_id, stop_p) in &queue {
                let mut boarding_point: usize = 0;
                let mut trip: Option<Rc<Trip>> = None;
                let route_path = &self.route_path[route_id];
                let route_path_length = route_path.len();

                let start_pi = self.route_stop_index[route_id][stop_p];
                for pi in start_pi..route_path_length {
                    let stop_pi = &route_path[pi];
                    let previous_arrival = truthy(results.previous_arrival(stop_pi));

                    if let Some(current_trip) = &trip {
                        let i = self.interchange[stop_pi];
                        let stop_time = &current_trip.stop_times[pi];

                        if stop_time.drop_off
                            && stop_time.arrival_time + i < results.best_arrival(stop_pi)
                        {
                            results.set_trip(current_trip, boarding_point, pi, i);
                        } else if let Some(prev) = previous_arrival {
                            if prev < stop_time.arrival_time + i {
                                if let Some(new_trip) =
                                    route_scanner.get_trip(route_id, pi, prev)
                                {
                                    trip = Some(new_trip);
                                    boarding_point = pi;
                                }
                            }
                        }
                    } else if let Some(prev) = previous_arrival {
                        if let Some(new_trip) = route_scanner.get_trip(route_id, pi, prev) {
                            trip = Some(new_trip);
                            boarding_point = pi;
                        }
                    }
                }
            }

            // --- scanTransfers ---
            for stop_p in &marked_stops {
                if let Some(transfers) = self.transfers.get(stop_p) {
                    for transfer in transfers {
                        let stop_pi = &transfer.destination;
                        // previousArrival(stopP) is read unguarded in JS; undefined would
                        // produce NaN, but markedStops always have a prior arrival here.
                        let prev = results.previous_arrival(stop_p).unwrap_or(0);
                        let arrival = prev + transfer.duration + self.interchange[stop_pi];

                        if transfer.start_time <= arrival
                            && transfer.end_time >= arrival
                            && arrival < results.best_arrival(stop_pi)
                        {
                            results.set_transfer(transfer, arrival);
                        }
                    }
                }
            }

            marked_stops = results.get_marked_stops();
        }

        results.finalize()
    }
}

pub struct RaptorAlgorithmFactory;

impl RaptorAlgorithmFactory {
    pub fn create(
        trips: Vec<Rc<Trip>>,
        transfers: TransfersByOrigin,
        mut interchange: Interchange,
        date: Option<UtcDate>,
    ) -> RaptorAlgorithm {
        let mut routes_at_stop: RoutesIndexedByStop = IndexMap::new();
        let mut trips_by_route: TripsIndexedByRoute = IndexMap::new();
        let mut route_stop_index: RouteStopIndex = IndexMap::new();
        let mut route_path: RoutePaths = IndexMap::new();
        let mut useful_transfers: TransfersByOrigin = IndexMap::new();

        let mut trips = trips;

        if let Some(date) = date {
            let date_number = date.date_number();
            let dow = date.day_of_week();
            trips.retain(|trip| trip.service.runs_on(date_number, dow));
        }

        // Sort by first departureTime, stable (contract #3).
        trips.sort_by(|a, b| {
            a.stop_times[0]
                .departure_time
                .cmp(&b.stop_times[0].departure_time)
        });

        for trip in &trips {
            let path: Vec<StopId> = trip.stop_times.iter().map(|s| s.stop.clone()).collect();
            let route_id = Self::get_route_id(trip, &trips_by_route);

            if !route_stop_index.contains_key(&route_id) {
                trips_by_route.insert(route_id.clone(), Vec::new());
                route_stop_index.insert(route_id.clone(), IndexMap::new());
                route_path.insert(route_id.clone(), path.clone());

                for i in (0..path.len()).rev() {
                    let stop = &path[i];
                    route_stop_index
                        .get_mut(&route_id)
                        .unwrap()
                        .insert(stop.clone(), i);
                    useful_transfers
                        .entry(stop.clone())
                        .or_insert_with(|| transfers.get(stop).cloned().unwrap_or_default());
                    interchange
                        .entry(stop.clone())
                        .or_insert(DEFAULT_INTERCHANGE_TIME);
                    routes_at_stop.entry(stop.clone()).or_default();

                    if trip.stop_times[i].pick_up {
                        routes_at_stop
                            .get_mut(stop)
                            .unwrap()
                            .push(route_id.clone());
                    }
                }
            }

            trips_by_route
                .get_mut(&route_id)
                .unwrap()
                .push(Rc::clone(trip));
        }

        let scan_stops: Vec<StopId> = useful_transfers.keys().cloned().collect();

        RaptorAlgorithm {
            route_stop_index,
            route_path,
            transfers: useful_transfers,
            interchange,
            scan_results_factory: ScanResultsFactory::new(scan_stops),
            routes_at_stop,
            route_scanner_factory: RouteScannerFactory::new(trips_by_route),
        }
    }

    fn get_route_id(trip: &Trip, trips_by_route: &TripsIndexedByRoute) -> RouteId {
        // stop + (pickUp?1:0) + (dropOff?1:0), comma-joined (contract #2).
        let route_id: String = trip
            .stop_times
            .iter()
            .map(|s| {
                format!(
                    "{}{}{}",
                    s.stop,
                    if s.pick_up { "1" } else { "0" },
                    if s.drop_off { "1" } else { "0" }
                )
            })
            .collect::<Vec<_>>()
            .join(",");

        if let Some(existing) = trips_by_route.get(&route_id) {
            let arrival_a = trip.stop_times[trip.stop_times.len() - 1].arrival_time;
            for t in existing {
                let arrival_b = t.stop_times[t.stop_times.len() - 1].arrival_time;
                if arrival_a < arrival_b {
                    return route_id + OVERTAKING_ROUTE_SUFFIX;
                }
            }
        }

        route_id
    }
}
