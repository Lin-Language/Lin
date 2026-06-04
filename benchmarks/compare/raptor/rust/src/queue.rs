//! QueueFactory (port of src/raptor/QueueFactory.ts).

use indexmap::IndexMap;

use crate::gtfs::StopId;

pub type RouteId = String;
/// Routes that pass through each stop, in insertion order.
pub type RoutesIndexedByStop = IndexMap<StopId, Vec<RouteId>>;
/// For each route, the index of each stop in the route path.
pub type RouteStopIndex = IndexMap<RouteId, IndexMap<StopId, usize>>;
/// The queue: route → boarding stop. Insertion order is load-bearing (contract #1).
pub type RouteQueue = IndexMap<RouteId, StopId>;

pub struct QueueFactory<'a> {
    routes_at_stop: &'a RoutesIndexedByStop,
    route_stop_index: &'a RouteStopIndex,
}

impl<'a> QueueFactory<'a> {
    pub fn new(
        routes_at_stop: &'a RoutesIndexedByStop,
        route_stop_index: &'a RouteStopIndex,
    ) -> Self {
        QueueFactory {
            routes_at_stop,
            route_stop_index,
        }
    }

    pub fn get_queue(&self, marked_stops: &[StopId]) -> RouteQueue {
        let mut queue: RouteQueue = IndexMap::new();

        for stop in marked_stops {
            if let Some(routes) = self.routes_at_stop.get(stop) {
                for route_id in routes {
                    let keep_existing = match queue.get(route_id) {
                        Some(existing) => self.is_stop_before(route_id, existing, stop),
                        None => false,
                    };

                    if !keep_existing {
                        queue.insert(route_id.clone(), stop.clone());
                    }
                }
            }
        }

        queue
    }

    fn is_stop_before(&self, route_id: &str, stop_a: &str, stop_b: &str) -> bool {
        let index = &self.route_stop_index[route_id];
        index[stop_a] < index[stop_b]
    }
}
