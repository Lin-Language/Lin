//! Global integer-indexed flat arrays for the RAPTOR algorithm.

use std::collections::HashMap;
use std::rc::Rc;

use crate::gtfs::{Transfer, Trip};

/// Sentinel arrival: large but has headroom so +interchange can't overflow u32.
pub const INF_ARRIVAL: u32 = 3_999_999_999;

pub struct RouteEntry {
    pub stop_times_base: u32,
    pub route_stops_base: u32,
    pub num_stops: u32,
    pub num_trips: u32,
}

/// Hot-path transfer data.
pub struct FlatTransfer {
    pub destination: u32, // u32::MAX = sentinel (dest not in any route)
    pub duration: u32,
    pub start_time: u32,
    pub end_time: u32,
}

pub struct FlatIndex {
    pub stop_index_of: HashMap<String, u32>,
    pub stop_names: Vec<String>,

    pub routes: Vec<RouteEntry>,

    /// Trip-major: route r, trip t, stop s at [stop_times_base + t*num_stops + s]
    pub arrivals: Vec<u32>,
    pub departures: Vec<u32>,

    /// route_stops[route_stops_base + p] = stop index
    pub route_stops: Vec<u32>,

    /// Inverse: stop_routes[stop_routes_base[s]..stop_routes_end[s]) = route indices
    pub stop_routes: Vec<u32>,
    pub stop_route_pos: Vec<u32>,
    pub stop_routes_base: Vec<u32>,
    pub stop_routes_end: Vec<u32>,

    /// Interchange time (seconds) per stop index
    pub interchange: Vec<u32>,

    /// Transfers per source stop index (flat for hot path, full for reconstruction)
    pub transfers_flat: Vec<Vec<FlatTransfer>>,
    pub transfers_full: Vec<Vec<Transfer>>,

    /// Trips per route index (for calendar check + reconstruction)
    pub route_trips: Vec<Vec<Rc<Trip>>>,

    /// Useful stop indices in insertion order (for connection seeding and output)
    pub useful_stops_order: Vec<u32>,
}
