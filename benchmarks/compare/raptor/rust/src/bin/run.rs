//! CLI runner for the RAPTOR planner over a real GTFS feed.
//!
//! Usage: cargo run --release --bin run -- [dataDir] [origin] [destination] [YYYY-MM-DD] [HH:MM]
//! Defaults: dataDir = ../data (relative to the crate dir), TBW -> NRW on 2025-09-02 at 08:00.
//!
//! Prints the contract format to stdout (journeys sorted by departureTime asc then
//! arrivalTime asc, then a RESULT line from the earliest-arrival journey). Timing goes
//! to stderr only, keeping stdout a pure, diffable result.

use std::env;
use std::path::PathBuf;
use std::time::Instant;

use raptor::date_util::UtcDate;
use raptor::gtfs_loader::load_gtfs;
use raptor::journey::{Journey, Leg};
use raptor::journey_factory::JourneyFactory;
use raptor::query::DepartAfterQuery;
use raptor::raptor::RaptorAlgorithmFactory;

/// Format seconds-from-midnight as HH:MM:SS (HH may exceed 24).
fn fmt(s: i64) -> String {
    let hh = s / 3600;
    let mm = (s % 3600) / 60;
    let ss = s % 60;
    format!("{hh:02}:{mm:02}:{ss:02}")
}

fn main() {
    let argv: Vec<String> = env::args().skip(1).collect();

    // dataDir default: ../data relative to this crate's directory (rust/ -> ../data).
    let data_dir = argv
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("data"));
    let origin = argv.get(1).map(String::as_str).unwrap_or("TBW");
    let destination = argv.get(2).map(String::as_str).unwrap_or("NRW");
    let date_str = argv.get(3).map(String::as_str).unwrap_or("2025-09-02");
    let time_str = argv.get(4).map(String::as_str).unwrap_or("08:00");

    // HH:MM (or HH:MM:SS) -> seconds from midnight.
    let parts: Vec<i64> = time_str.split(':').map(|p| p.parse().unwrap_or(0)).collect();
    let time_seconds =
        parts.first().copied().unwrap_or(0) * 3600
            + parts.get(1).copied().unwrap_or(0) * 60
            + parts.get(2).copied().unwrap_or(0);

    let load_start = Instant::now();
    let (trips, transfers, interchange) = load_gtfs(&data_dir);
    let load_ms = load_start.elapsed().as_secs_f64() * 1000.0;

    // NO date pre-filter: pass the date through the query (matches DepartAfterQuery).
    let raptor = RaptorAlgorithmFactory::create(trips, transfers, interchange, None);
    let jf = JourneyFactory::new();
    let query = DepartAfterQuery::new(&raptor, &jf, 3);

    let plan_start = Instant::now();
    let mut journeys = query.plan(origin, destination, UtcDate::parse(date_str), time_seconds);
    let plan_ms = plan_start.elapsed().as_secs_f64() * 1000.0;

    eprintln!("load={load_ms:.1}ms plan={plan_ms:.1}ms");

    // Sort by departureTime asc then arrivalTime asc for stable cross-language output.
    journeys.sort_by(|a, b| {
        a.departure_time
            .cmp(&b.departure_time)
            .then(a.arrival_time.cmp(&b.arrival_time))
    });

    let mut out: Vec<String> = Vec::new();
    for journey in &journeys {
        out.push(format!(
            "JOURNEY dep={} arr={} legs={}",
            fmt(journey.departure_time),
            fmt(journey.arrival_time),
            journey.legs.len()
        ));
        for leg in &journey.legs {
            match leg {
                Leg::Timetable(l) => {
                    let first = &l.stop_times[0];
                    let last = &l.stop_times[l.stop_times.len() - 1];
                    out.push(format!(
                        "  {} {} -> {} {}",
                        first.stop,
                        fmt(first.departure_time),
                        last.stop,
                        fmt(last.arrival_time)
                    ));
                }
                Leg::Transfer(t) => {
                    out.push(format!(
                        "  TRANSFER {} -> {} ({}s)",
                        t.origin, t.destination, t.duration
                    ));
                }
            }
        }
    }

    // RESULT: from the journey with the earliest arrival (ties: fewest legs).
    let mut best: Option<&Journey> = None;
    for journey in &journeys {
        let take = match best {
            None => true,
            Some(b) => {
                journey.arrival_time < b.arrival_time
                    || (journey.arrival_time == b.arrival_time
                        && journey.legs.len() < b.legs.len())
            }
        };
        if take {
            best = Some(journey);
        }
    }

    match best {
        Some(b) => out.push(format!(
            "RESULT dep={} arr={} legs={} count={}",
            b.departure_time,
            b.arrival_time,
            b.legs.len(),
            journeys.len()
        )),
        None => out.push("RESULT dep=0 arr=0 legs=0 count=0".to_string()),
    }

    println!("{}", out.join("\n"));
}
