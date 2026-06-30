//! RaptorAlgorithm + RaptorAlgorithmFactory
//! Integer-indexed flat-array representation.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::rc::Rc;

use indexmap::IndexMap;

use crate::date_util::UtcDate;
use crate::flat_index::{FlatIndex, FlatTransfer, RouteEntry, INF_ARRIVAL};
use crate::gtfs::{DayOfWeek, StopId, Time, Transfer, Trip, MAX_SAFE_INTEGER};
use crate::scan_results::{Arrivals, Connection, ConnectionIndex};

/// origin departure times keyed by stop (insertion-ordered).
pub type StopTimes = IndexMap<StopId, Time>;
/// for compatibility with existing callers
pub type Interchange = IndexMap<StopId, Time>;
pub type TransfersByOrigin = IndexMap<StopId, Vec<Transfer>>;

const OVERTAKING_ROUTE_SUFFIX: &str = "overtakes";
const DEFAULT_INTERCHANGE_TIME: Time = 0;

pub struct RaptorAlgorithm {
    idx: FlatIndex,
}

impl RaptorAlgorithm {
    pub fn scan(
        &self,
        origins: &StopTimes,
        date: i64,
        dow: DayOfWeek,
    ) -> (ConnectionIndex, Arrivals) {
        let idx = &self.idx;
        let num_stops = idx.stop_names.len();

        let mut best_arrivals: Vec<u32> = vec![INF_ARRIVAL; num_stops];
        let mut prev_arrivals: Vec<u32> = vec![INF_ARRIVAL; num_stops];

        // Seed connection index for all useful stops (string-keyed for output compat).
        let mut k_connections: ConnectionIndex = IndexMap::new();
        for &sidx in &idx.useful_stops_order {
            k_connections.insert(idx.stop_names[sidx as usize].clone(), BTreeMap::new());
        }

        // Initialise marked stops from origins.
        let mut marked: Vec<u32> = Vec::new();
        for (stop_name, &t) in origins {
            if let Some(&sidx) = idx.stop_index_of.get(stop_name) {
                if t != 0 {
                    let v = t as u32;
                    best_arrivals[sidx as usize] = v;
                    prev_arrivals[sidx as usize] = v;
                    marked.push(sidx);
                }
            }
        }

        let num_routes = idx.routes.len();
        let mut scan_pos: Vec<i32> = vec![-1; num_routes];
        let mut scan_pos_set: Vec<bool> = vec![false; num_routes];

        let mut k: usize = 0;
        while !marked.is_empty() {
            k += 1;
            let mut curr_arrivals: Vec<u32> = vec![INF_ARRIVAL; num_stops];

            // --- scanRoutes ---
            let mut queue_stop: Vec<u32> = vec![0; num_routes];
            let mut queue_pos: Vec<u32> = vec![0; num_routes];
            let mut queue_set: Vec<bool> = vec![false; num_routes];
            let mut queue_order: Vec<u32> = Vec::new();

            for &stop_idx in &marked {
                let base = idx.stop_routes_base[stop_idx as usize];
                let end = idx.stop_routes_end[stop_idx as usize];
                for i in base..end {
                    let ri = idx.stop_routes[i as usize];
                    let pos = idx.stop_route_pos[i as usize];
                    if !queue_set[ri as usize] {
                        queue_set[ri as usize] = true;
                        queue_stop[ri as usize] = stop_idx;
                        queue_pos[ri as usize] = pos;
                        queue_order.push(ri);
                    } else if pos < queue_pos[ri as usize] {
                        queue_stop[ri as usize] = stop_idx;
                        queue_pos[ri as usize] = pos;
                    }
                }
            }

            let mut new_marked_set: Vec<bool> = vec![false; num_stops];
            let mut new_marked: Vec<u32> = Vec::new();

            for ri in queue_order {
                let re = &idx.routes[ri as usize];
                let start_pos = queue_pos[ri as usize];
                let num_stops_r = re.num_stops;

                let mut boarding_trip: i32 = -1;
                let mut boarding_pos: u32 = 0;

                for pi in start_pos..num_stops_r {
                    let stop_pi = idx.route_stops[(re.route_stops_base + pi) as usize];
                    let prev_arr = prev_arrivals[stop_pi as usize];
                    let pa_truthy = prev_arr != INF_ARRIVAL && prev_arr != 0;

                    if boarding_trip >= 0 {
                        let ic = idx.interchange[stop_pi as usize];
                        let arr_base = re.stop_times_base + boarding_trip as u32 * num_stops_r;
                        let arr_time = idx.arrivals[(arr_base + pi) as usize] + ic;

                        let trip_obj = &idx.route_trips[ri as usize][boarding_trip as usize];
                        if trip_obj.stop_times[pi as usize].drop_off
                            && arr_time < best_arrivals[stop_pi as usize]
                        {
                            best_arrivals[stop_pi as usize] = arr_time;
                            curr_arrivals[stop_pi as usize] = arr_time;
                            let stop_name = &idx.stop_names[stop_pi as usize];
                            k_connections
                                .entry(stop_name.clone())
                                .or_default()
                                .insert(k, Connection::Trip(
                                    Rc::clone(trip_obj),
                                    boarding_pos as usize,
                                    pi as usize,
                                ));
                            if !new_marked_set[stop_pi as usize] {
                                new_marked_set[stop_pi as usize] = true;
                                new_marked.push(stop_pi);
                            }
                        } else if pa_truthy {
                            let new_ti = get_trip(
                                idx, ri, pi, prev_arr, date, dow,
                                &mut scan_pos, &mut scan_pos_set,
                            );
                            if new_ti >= 0 {
                                boarding_trip = new_ti;
                                boarding_pos = pi;
                            }
                        }
                    } else if pa_truthy {
                        let new_ti = get_trip(
                            idx, ri, pi, prev_arr, date, dow,
                            &mut scan_pos, &mut scan_pos_set,
                        );
                        if new_ti >= 0 {
                            boarding_trip = new_ti;
                            boarding_pos = pi;
                        }
                    }
                }
            }

            // --- scanTransfers ---
            for &stop_pi in &new_marked {
                new_marked_set[stop_pi as usize] = true;
            }
            for stop_p in marked {
                let prev = prev_arrivals[stop_p as usize];
                if prev == INF_ARRIVAL {
                    continue;
                }
                let flat_ts = &idx.transfers_flat[stop_p as usize];
                let full_ts = &idx.transfers_full[stop_p as usize];
                for (i, ft) in flat_ts.iter().enumerate() {
                    if ft.destination == u32::MAX {
                        continue;
                    }
                    let stop_pi = ft.destination as usize;
                    let arrival = prev + ft.duration + idx.interchange[stop_pi];
                    if ft.start_time <= arrival
                        && ft.end_time >= arrival
                        && arrival < best_arrivals[stop_pi]
                    {
                        best_arrivals[stop_pi] = arrival;
                        curr_arrivals[stop_pi] = arrival;
                        let stop_name = &idx.stop_names[stop_pi];
                        k_connections
                            .entry(stop_name.clone())
                            .or_default()
                            .insert(k, Connection::Transfer(full_ts[i].clone()));
                        if !new_marked_set[stop_pi] {
                            new_marked_set[stop_pi] = true;
                            new_marked.push(stop_pi as u32);
                        }
                    }
                }
            }

            marked = new_marked;
            prev_arrivals = curr_arrivals;
        }

        // Convert dense best_arrivals to string-keyed Arrivals map.
        let mut arrivals: Arrivals = IndexMap::new();
        for &sidx in &idx.useful_stops_order {
            let v = best_arrivals[sidx as usize];
            let t: Time = if v == INF_ARRIVAL { MAX_SAFE_INTEGER } else { v as Time };
            arrivals.insert(idx.stop_names[sidx as usize].clone(), t);
        }

        (k_connections, arrivals)
    }
}

/// Stateful backward trip scan — mirrors RouteScannerFactory but inlined for the flat index.
fn get_trip(
    idx: &FlatIndex,
    ri: u32,
    stop_index: u32,
    time: u32,
    date: i64,
    dow: u32,
    scan_pos: &mut Vec<i32>,
    scan_pos_set: &mut Vec<bool>,
) -> i32 {
    let ri_usize = ri as usize;
    if !scan_pos_set[ri_usize] {
        scan_pos[ri_usize] = idx.routes[ri_usize].num_trips as i32 - 1;
        scan_pos_set[ri_usize] = true;
    }

    let re = &idx.routes[ri_usize];
    let num_stops = re.num_stops;
    let trips = &idx.route_trips[ri_usize];
    let departures = &idx.departures;

    let mut last_found: i32 = -1;
    let mut i = scan_pos[ri_usize];
    while i >= 0 {
        let dep = departures[(re.stop_times_base + i as u32 * num_stops + stop_index) as usize];
        if dep < time {
            break;
        }
        let trip = &trips[i as usize];
        if trip.service.runs_on(date, dow) {
            last_found = i;
        }
        if last_found < 0 || last_found == i {
            scan_pos[ri_usize] = i;
        }
        i -= 1;
    }

    last_found
}

pub struct RaptorAlgorithmFactory;

impl RaptorAlgorithmFactory {
    pub fn create(
        trips: Vec<Rc<Trip>>,
        transfers: TransfersByOrigin,
        mut interchange: Interchange,
        date: Option<UtcDate>,
    ) -> RaptorAlgorithm {
        let mut trips = trips;

        if let Some(d) = date {
            let dn = d.date_number();
            let dow = d.day_of_week();
            trips.retain(|trip| trip.service.runs_on(dn, dow));
        }

        trips.sort_by(|a, b| {
            a.stop_times[0]
                .departure_time
                .cmp(&b.stop_times[0].departure_time)
        });

        // --- Pass 0: intern stops, group trips by route ---
        let mut stop_index_of: HashMap<String, u32> = HashMap::new();
        let mut stop_names: Vec<String> = Vec::new();
        let intern_stop = |s: &str,
                           stop_index_of: &mut HashMap<String, u32>,
                           stop_names: &mut Vec<String>|
         -> u32 {
            if let Some(&idx) = stop_index_of.get(s) {
                return idx;
            }
            let idx = stop_names.len() as u32;
            stop_index_of.insert(s.to_string(), idx);
            stop_names.push(s.to_string());
            idx
        };

        struct RouteBuild {
            path: Vec<String>,
            trips: Vec<Rc<Trip>>,
        }
        let mut route_list: Vec<RouteBuild> = Vec::new();
        let mut route_map: IndexMap<String, usize> = IndexMap::new(); // route sig -> index in route_list
        let mut trips_by_route_sig: HashMap<String, Vec<Rc<Trip>>> = HashMap::new();

        let mut useful_stop_seen: HashMap<u32, bool> = HashMap::new();
        let mut useful_stops_order: Vec<u32> = Vec::new();
        let track_stop = |sidx: u32,
                          useful_stop_seen: &mut HashMap<u32, bool>,
                          useful_stops_order: &mut Vec<u32>| {
            if !useful_stop_seen.contains_key(&sidx) {
                useful_stop_seen.insert(sidx, true);
                useful_stops_order.push(sidx);
            }
        };

        for trip in &trips {
            for st in &trip.stop_times {
                intern_stop(&st.stop, &mut stop_index_of, &mut stop_names);
            }

            let sig = route_sig(trip);
            let mut route_id = sig.clone();

            let arr_a = trip.stop_times[trip.stop_times.len() - 1].arrival_time;
            if let Some(prev_trips) = trips_by_route_sig.get(&route_id) {
                for t in prev_trips {
                    let arr_b = t.stop_times[t.stop_times.len() - 1].arrival_time;
                    if arr_a < arr_b {
                        route_id = sig.clone() + OVERTAKING_ROUTE_SUFFIX;
                        break;
                    }
                }
            }
            trips_by_route_sig
                .entry(sig)
                .or_default()
                .push(Rc::clone(trip));

            if !route_map.contains_key(&route_id) {
                let path: Vec<String> =
                    trip.stop_times.iter().map(|s| s.stop.clone()).collect();
                let ri = route_list.len();
                route_map.insert(route_id.clone(), ri);
                // Register useful stops and interchange (reverse order, like original).
                for i in (0..path.len()).rev() {
                    let sidx = intern_stop(&path[i], &mut stop_index_of, &mut stop_names);
                    track_stop(sidx, &mut useful_stop_seen, &mut useful_stops_order);
                    interchange
                        .entry(path[i].clone())
                        .or_insert(DEFAULT_INTERCHANGE_TIME);
                    // also seed useful transfers side
                }
                route_list.push(RouteBuild {
                    path,
                    trips: Vec::new(),
                });
            }
            let ri = route_map[&route_id];
            route_list[ri].trips.push(Rc::clone(trip));
        }

        let num_stops = stop_names.len() as u32;
        let num_routes = route_list.len() as u32;

        // --- Pass 1: compute base offsets ---
        let mut routes: Vec<RouteEntry> = Vec::with_capacity(num_routes as usize);
        let mut total_stop_times: u32 = 0;
        let mut total_route_stops: u32 = 0;
        for rb in &route_list {
            let ns = rb.path.len() as u32;
            let nt = rb.trips.len() as u32;
            routes.push(RouteEntry {
                stop_times_base: total_stop_times,
                route_stops_base: total_route_stops,
                num_stops: ns,
                num_trips: nt,
            });
            total_stop_times += ns * nt;
            total_route_stops += ns;
        }

        // --- Pass 2: fill flat arrays ---
        let mut arrivals: Vec<u32> = vec![0; total_stop_times as usize];
        let mut departures: Vec<u32> = vec![0; total_stop_times as usize];
        let mut route_stops_flat: Vec<u32> = vec![0; total_route_stops as usize];

        for (ri, rb) in route_list.iter().enumerate() {
            let re = &routes[ri];
            for (si, sname) in rb.path.iter().enumerate() {
                route_stops_flat[(re.route_stops_base + si as u32) as usize] =
                    stop_index_of[sname];
            }
            for (ti, trip) in rb.trips.iter().enumerate() {
                let base = re.stop_times_base + ti as u32 * re.num_stops;
                for (si, st) in trip.stop_times.iter().enumerate() {
                    arrivals[(base + si as u32) as usize] = st.arrival_time as u32;
                    departures[(base + si as u32) as usize] = st.departure_time as u32;
                }
            }
        }

        // --- Build stop_routes inverse index ---
        let mut stop_route_count: Vec<u32> = vec![0; num_stops as usize];
        for (ri, rb) in route_list.iter().enumerate() {
            for (si, sname) in rb.path.iter().enumerate() {
                let sidx = stop_index_of[sname] as usize;
                if rb.trips[0].stop_times[si].pick_up {
                    stop_route_count[sidx] += 1;
                }
            }
        }

        let mut stop_routes_base: Vec<u32> = vec![0; num_stops as usize + 1];
        for s in 0..num_stops as usize {
            stop_routes_base[s + 1] = stop_routes_base[s] + stop_route_count[s];
        }
        let total_inverse = stop_routes_base[num_stops as usize];
        let mut stop_routes: Vec<u32> = vec![0; total_inverse as usize];
        let mut stop_route_pos: Vec<u32> = vec![0; total_inverse as usize];
        let mut stop_routes_end: Vec<u32> = stop_routes_base[..num_stops as usize].to_vec();

        for (ri, rb) in route_list.iter().enumerate() {
            for (si, sname) in rb.path.iter().enumerate() {
                let sidx = stop_index_of[sname] as usize;
                if rb.trips[0].stop_times[si].pick_up {
                    let cursor = stop_routes_end[sidx] as usize;
                    stop_routes[cursor] = ri as u32;
                    stop_route_pos[cursor] = si as u32;
                    stop_routes_end[sidx] += 1;
                }
            }
        }

        // --- Interchange flat ---
        let mut interchange_flat: Vec<u32> = vec![0; num_stops as usize];
        for (sname, &ic) in &interchange {
            if let Some(&sidx) = stop_index_of.get(sname) {
                interchange_flat[sidx as usize] = ic as u32;
            }
        }

        // --- Flat transfers ---
        let mut transfers_flat: Vec<Vec<FlatTransfer>> =
            (0..num_stops as usize).map(|_| Vec::new()).collect();
        let mut transfers_full: Vec<Vec<Transfer>> =
            (0..num_stops as usize).map(|_| Vec::new()).collect();

        for (sname, tlist) in &transfers {
            let sidx = match stop_index_of.get(sname) {
                Some(&i) => i as usize,
                None => continue,
            };
            let mut flat_row: Vec<FlatTransfer> = Vec::with_capacity(tlist.len());
            let mut full_row: Vec<Transfer> = Vec::with_capacity(tlist.len());
            for tr in tlist {
                let dst = match stop_index_of.get(&tr.destination) {
                    Some(&d) => d,
                    None => {
                        flat_row.push(FlatTransfer {
                            destination: u32::MAX,
                            duration: 0,
                            start_time: 0,
                            end_time: 0,
                        });
                        full_row.push(tr.clone());
                        continue;
                    }
                };
                let end_time = if tr.end_time > INF_ARRIVAL as i64 {
                    INF_ARRIVAL
                } else {
                    tr.end_time as u32
                };
                flat_row.push(FlatTransfer {
                    destination: dst,
                    duration: tr.duration as u32,
                    start_time: tr.start_time as u32,
                    end_time,
                });
                full_row.push(tr.clone());
            }
            transfers_flat[sidx] = flat_row;
            transfers_full[sidx] = full_row;
        }

        // --- route_trips ---
        let mut route_trips: Vec<Vec<Rc<Trip>>> = Vec::with_capacity(route_list.len());
        for rb in &route_list {
            route_trips.push(rb.trips.iter().map(|t| Rc::clone(t)).collect());
        }

        RaptorAlgorithm {
            idx: FlatIndex {
                stop_index_of,
                stop_names,
                routes,
                arrivals,
                departures,
                route_stops: route_stops_flat,
                stop_routes,
                stop_route_pos,
                stop_routes_base: stop_routes_base[..num_stops as usize].to_vec(),
                stop_routes_end,
                interchange: interchange_flat,
                transfers_flat,
                transfers_full,
                route_trips,
                useful_stops_order,
            },
        }
    }
}

fn route_sig(trip: &Trip) -> String {
    trip.stop_times
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
        .join(",")
}
