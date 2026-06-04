//! ScanResults + ScanResultsFactory (port of src/raptor/ScanResults.ts).

use std::collections::BTreeMap;
use std::rc::Rc;

use indexmap::IndexMap;

use crate::gtfs::{StopId, Time, Transfer, Trip, MAX_SAFE_INTEGER};

/// A kConnection entry: either a boarded trip leg `[trip, start, end]` or a transfer.
#[derive(Debug, Clone)]
pub enum Connection {
    Trip(Rc<Trip>, usize, usize),
    Transfer(Transfer),
}

/// best arrival time per stop. Insertion-ordered (load-bearing for `getFoundStations`).
pub type Arrivals = IndexMap<StopId, Time>;

/// kConnections[stop][k]. Outer map preserves stop insertion order; inner map is
/// keyed by round number `k` and iterates **numeric ascending** (contract #1).
pub type ConnectionIndex = IndexMap<StopId, BTreeMap<usize, Connection>>;

/// kArrivals[k] = arrivals for round k; per-round map preserves insertion order
/// (the source of `getMarkedStops` ordering).
type RoundArrivals = IndexMap<StopId, Time>;

pub struct ScanResults {
    k: usize,
    best_arrivals: Arrivals,
    k_arrivals: Vec<RoundArrivals>,
    k_connections: ConnectionIndex,
}

impl ScanResults {
    pub fn add_round(&mut self) {
        self.k += 1;
        // kArrivals[++k] = {}. Indices 0..=k must exist.
        while self.k_arrivals.len() <= self.k {
            self.k_arrivals.push(IndexMap::new());
        }
    }

    pub fn previous_arrival(&self, stop_pi: &str) -> Option<Time> {
        self.k_arrivals[self.k - 1].get(stop_pi).copied()
    }

    pub fn set_trip(
        &mut self,
        trip: &Rc<Trip>,
        start_index: usize,
        end_index: usize,
        interchange: Time,
    ) {
        let time = trip.stop_times[end_index].arrival_time + interchange;
        let stop_pi = trip.stop_times[end_index].stop.clone();

        self.k_arrivals[self.k].insert(stop_pi.clone(), time);
        self.best_arrivals.insert(stop_pi.clone(), time);
        self.k_connections
            .entry(stop_pi)
            .or_default()
            .insert(self.k, Connection::Trip(Rc::clone(trip), start_index, end_index));
    }

    pub fn set_transfer(&mut self, transfer: &Transfer, time: Time) {
        let stop_pi = transfer.destination.clone();

        self.k_arrivals[self.k].insert(stop_pi.clone(), time);
        self.best_arrivals.insert(stop_pi.clone(), time);
        self.k_connections
            .entry(stop_pi)
            .or_default()
            .insert(self.k, Connection::Transfer(transfer.clone()));
    }

    pub fn best_arrival(&self, stop_pi: &str) -> Time {
        // JS reads bestArrivals[stopPi]; every relevant stop is seeded by the factory.
        *self.best_arrivals.get(stop_pi).unwrap_or(&MAX_SAFE_INTEGER)
    }

    pub fn get_marked_stops(&self) -> Vec<StopId> {
        self.k_arrivals[self.k].keys().cloned().collect()
    }

    pub fn finalize(self) -> (ConnectionIndex, Arrivals) {
        (self.k_connections, self.best_arrivals)
    }
}

pub struct ScanResultsFactory {
    /// `Object.keys(usefulTransfers)` — the seeded stops, in insertion order.
    stops: Vec<StopId>,
}

impl ScanResultsFactory {
    pub fn new(stops: Vec<StopId>) -> Self {
        ScanResultsFactory { stops }
    }

    pub fn create(&self, origins: &IndexMap<StopId, Time>) -> ScanResults {
        let mut best_arrivals: Arrivals = IndexMap::new();
        let mut round0: RoundArrivals = IndexMap::new();
        let mut k_connections: ConnectionIndex = IndexMap::new();

        for stop in &self.stops {
            let seed = origins.get(stop).copied().unwrap_or(MAX_SAFE_INTEGER);
            best_arrivals.insert(stop.clone(), seed);
            round0.insert(stop.clone(), seed);
            k_connections.insert(stop.clone(), BTreeMap::new());
        }

        ScanResults {
            k: 0,
            best_arrivals,
            k_arrivals: vec![round0],
            k_connections,
        }
    }
}
