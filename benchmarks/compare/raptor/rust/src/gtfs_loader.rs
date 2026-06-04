//! GTFS CSV loader (port of node/src/gtfs/GTFSLoader.js, which mirrors GTFSLoader.ts).
//!
//! Reads a directory of plain CSV files (no quoted fields in this feed — a simple
//! split-on-comma is sufficient; do NOT add a heavy CSV lib) and returns
//! `(trips, transfers, interchange)` ready for `RaptorAlgorithmFactory::create`.
//!
//! Mirrors the reference: trips with stopTimes in file order; Service built from
//! calendar + calendar_dates; transfers from transfers.txt (same-stop -> interchange,
//! else a Transfer) plus links.txt footpaths (date/day columns ignored). Trips whose
//! serviceId has no calendar row are dropped and the count reported to stderr.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::rc::Rc;

use indexmap::IndexMap;

use crate::gtfs::{DateNumber, DayOfWeek, StopTime, Transfer, Trip, MAX_SAFE_INTEGER};
use crate::raptor::{Interchange, TransfersByOrigin};
use crate::service::Service;
use crate::time_parser::TimeParser;

/// Build a column-name -> index lookup so columns are read by name (like the
/// reference's `row.<field>`), independent of incidental column order.
fn index_of_columns(header: &str) -> HashMap<String, usize> {
    header
        .split(',')
        .enumerate()
        .map(|(i, name)| (name.trim_end_matches('\r').to_string(), i))
        .collect()
}

/// Open a CSV file, returning the buffered reader and the parsed header index map.
fn open_csv(path: &Path) -> (BufReader<File>, HashMap<String, usize>) {
    let file = File::open(path).unwrap_or_else(|e| panic!("opening {}: {e}", path.display()));
    let mut reader = BufReader::new(file);
    let mut header = String::new();
    reader
        .read_line(&mut header)
        .unwrap_or_else(|e| panic!("reading header of {}: {e}", path.display()));
    let header = header.trim_end_matches('\n').trim_end_matches('\r');
    let idx = index_of_columns(header);
    (reader, idx)
}

/// Read a column from a pre-split row, defaulting to "" when missing (matches the
/// reference reading a column that doesn't exist as undefined/empty).
fn col<'a>(fields: &[&'a str], idx: &HashMap<String, usize>, name: &str) -> &'a str {
    idx.get(name).and_then(|&i| fields.get(i)).copied().unwrap_or("")
}

fn parse_int(s: &str) -> i64 {
    s.parse().unwrap_or(0)
}

/// Returns `(trips, transfers, interchange)` from a directory of GTFS CSV files.
pub fn load_gtfs(data_dir: &Path) -> (Vec<Rc<Trip>>, TransfersByOrigin, Interchange) {
    let mut time_parser = TimeParser::new();
    let mut transfers: TransfersByOrigin = IndexMap::new();
    let mut interchange: Interchange = IndexMap::new();

    // calendar metadata, keyed by service_id (insertion order, like JS object keys).
    struct Cal {
        start_date: DateNumber,
        end_date: DateNumber,
        days: HashMap<DayOfWeek, bool>,
    }
    let mut calendars: IndexMap<String, Cal> = IndexMap::new();
    let mut dates: HashMap<String, HashMap<DateNumber, bool>> = HashMap::new();
    let mut stop_times: HashMap<String, Vec<StopTime>> = HashMap::new();

    // --- calendar.txt -> calendars ---
    {
        let (reader, c) = open_csv(&data_dir.join("calendar.txt"));
        for line in reader.lines() {
            let line = line.unwrap();
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split(',').collect();
            let service_id = col(&f, &c, "service_id").to_string();
            let mut days = HashMap::new();
            days.insert(0, col(&f, &c, "sunday") == "1");
            days.insert(1, col(&f, &c, "monday") == "1");
            days.insert(2, col(&f, &c, "tuesday") == "1");
            days.insert(3, col(&f, &c, "wednesday") == "1");
            days.insert(4, col(&f, &c, "thursday") == "1");
            days.insert(5, col(&f, &c, "friday") == "1");
            days.insert(6, col(&f, &c, "saturday") == "1");
            calendars.insert(
                service_id,
                Cal {
                    start_date: parse_int(col(&f, &c, "start_date")),
                    end_date: parse_int(col(&f, &c, "end_date")),
                    days,
                },
            );
        }
    }

    // --- calendar_dates.txt -> dates[service_id][+date] = (exception_type === "1") ---
    {
        let (reader, c) = open_csv(&data_dir.join("calendar_dates.txt"));
        for line in reader.lines() {
            let line = line.unwrap();
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split(',').collect();
            let service_id = col(&f, &c, "service_id").to_string();
            let date = parse_int(col(&f, &c, "date"));
            let include = col(&f, &c, "exception_type") == "1";
            dates.entry(service_id).or_default().insert(date, include);
        }
    }

    // --- stop_times.txt -> stopTimes grouped by trip_id, in file order ---
    {
        let (reader, c) = open_csv(&data_dir.join("stop_times.txt"));
        for line in reader.lines() {
            let line = line.unwrap();
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split(',').collect();
            let trip_id = col(&f, &c, "trip_id").to_string();
            let pickup_type = col(&f, &c, "pickup_type");
            let drop_off_type = col(&f, &c, "drop_off_type");
            let st = StopTime {
                stop: col(&f, &c, "stop_id").to_string(),
                departure_time: time_parser.get_time(col(&f, &c, "departure_time")),
                arrival_time: time_parser.get_time(col(&f, &c, "arrival_time")),
                // "0" or empty/undefined => true; "1"/"3" => false (matches reference).
                pick_up: pickup_type == "0" || pickup_type.is_empty(),
                drop_off: drop_off_type == "0" || drop_off_type.is_empty(),
            };
            stop_times.entry(trip_id).or_default().push(st);
        }
    }

    // --- trips.txt -> raw (serviceId, tripId) pairs, in file order ---
    let raw_trips: Vec<(String, String)> = {
        let (reader, c) = open_csv(&data_dir.join("trips.txt"));
        let mut v = Vec::new();
        for line in reader.lines() {
            let line = line.unwrap();
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split(',').collect();
            v.push((
                col(&f, &c, "service_id").to_string(),
                col(&f, &c, "trip_id").to_string(),
            ));
        }
        v
    };

    // --- transfers.txt -> interchange + transfers ---
    {
        let (reader, c) = open_csv(&data_dir.join("transfers.txt"));
        for line in reader.lines() {
            let line = line.unwrap();
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split(',').collect();
            let from = col(&f, &c, "from_stop_id").to_string();
            let to = col(&f, &c, "to_stop_id").to_string();
            let min_transfer_time = parse_int(col(&f, &c, "min_transfer_time"));

            if from == to {
                interchange.insert(from, min_transfer_time);
            } else {
                let t = Transfer {
                    origin: from.clone(),
                    destination: to,
                    duration: min_transfer_time,
                    start_time: 0,
                    end_time: MAX_SAFE_INTEGER,
                };
                transfers.entry(from).or_default().push(t);
            }
        }
    }

    // --- links.txt -> transfers (footpaths; date/day columns ignored) ---
    {
        let (reader, c) = open_csv(&data_dir.join("links.txt"));
        for line in reader.lines() {
            let line = line.unwrap();
            let line = line.trim_end_matches('\r');
            if line.is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split(',').collect();
            let from = col(&f, &c, "from_stop_id").to_string();
            let t = Transfer {
                origin: from.clone(),
                destination: col(&f, &c, "to_stop_id").to_string(),
                duration: parse_int(col(&f, &c, "duration")),
                start_time: time_parser.get_time(col(&f, &c, "start_time")),
                end_time: time_parser.get_time(col(&f, &c, "end_time")),
            };
            transfers.entry(from).or_default().push(t);
        }
    }

    // --- Service resolution ---
    let mut services: HashMap<String, Rc<Service>> = HashMap::new();
    for (service_id, cal) in &calendars {
        let svc_dates = dates.get(service_id).cloned().unwrap_or_default();
        services.insert(
            service_id.clone(),
            Rc::new(Service::new(cal.start_date, cal.end_date, cal.days.clone(), svc_dates)),
        );
    }

    // Resolve stopTimes + service per trip; drop trips whose serviceId has no calendar.
    let mut trips: Vec<Rc<Trip>> = Vec::with_capacity(raw_trips.len());
    let mut dropped = 0usize;
    for (service_id, trip_id) in raw_trips {
        let service = match services.get(&service_id) {
            Some(s) => Rc::clone(s),
            None => {
                dropped += 1;
                continue;
            }
        };
        let sts = stop_times.remove(&trip_id).unwrap_or_default();
        trips.push(Rc::new(Trip {
            trip_id,
            stop_times: sts,
            service_id,
            service,
        }));
    }

    if dropped > 0 {
        eprintln!("dropped {dropped} trip(s) with no calendar row");
    }

    (trips, transfers, interchange)
}
