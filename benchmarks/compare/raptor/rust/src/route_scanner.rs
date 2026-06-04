//! RouteScanner + RouteScannerFactory (port of src/raptor/RouteScanner.ts).

use std::collections::HashMap;
use std::rc::Rc;

use indexmap::IndexMap;

use crate::gtfs::{DateNumber, DayOfWeek, Time, Trip};
use crate::queue::RouteId;

pub type TripsIndexedByRoute = IndexMap<RouteId, Vec<Rc<Trip>>>;

/// Returns trips for specific routes, remembering the last scan position per route
/// to reduce plan time. This memo is stateful across calls within one `scan()`.
pub struct RouteScanner<'a> {
    trips_by_route: &'a TripsIndexedByRoute,
    date: DateNumber,
    dow: DayOfWeek,
    route_scan_position: HashMap<RouteId, usize>,
}

impl<'a> RouteScanner<'a> {
    pub fn get_trip(
        &mut self,
        route_id: &str,
        stop_index: usize,
        time: Time,
    ) -> Option<Rc<Trip>> {
        let route_trips = &self.trips_by_route[route_id];

        let start = *self
            .route_scan_position
            .entry(route_id.to_string())
            .or_insert(route_trips.len() - 1);

        let mut last_found: Option<Rc<Trip>> = None;

        // Iterate backwards through the trips, starting where we last found a trip.
        let mut i = start as isize;
        while i >= 0 {
            let trip = &route_trips[i as usize];
            let stop_time = &trip.stop_times[stop_index];

            // Unreachable: this trip departs before we can board → stop.
            if stop_time.departure_time < time {
                break;
            } else if trip.service.runs_on(self.date, self.dow) {
                last_found = Some(Rc::clone(trip));
            }

            // Update the memo when we've found nothing yet, or when the last found
            // trip is the current one (subsequent scans are for earlier times).
            let update = match &last_found {
                None => true,
                Some(found) => Rc::ptr_eq(found, trip),
            };
            if update {
                self.route_scan_position.insert(route_id.to_string(), i as usize);
            }

            i -= 1;
        }

        last_found
    }
}

pub struct RouteScannerFactory {
    trips_by_route: TripsIndexedByRoute,
}

impl RouteScannerFactory {
    pub fn new(trips_by_route: TripsIndexedByRoute) -> Self {
        RouteScannerFactory { trips_by_route }
    }

    pub fn create(&self, date: DateNumber, dow: DayOfWeek) -> RouteScanner<'_> {
        RouteScanner {
            trips_by_route: &self.trips_by_route,
            date,
            dow,
            route_scan_position: HashMap::new(),
        }
    }
}
