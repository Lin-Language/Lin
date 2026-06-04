//! Cross-language RAPTOR benchmark — complex queries over the full GTFS feed.
//!
//! Port of node/bench.js. Two workloads:
//!   GROUP — the 24 group-station origin/destination-set queries planned at 10:00
//!           (36000s) on 2025-09-02 with the default MultipleCriteriaFilter.
//!   RANGE — "next 20 journeys departing after 08:00" for 5 pairs (RangeQuery profile
//!           loop, capped at N), no filter.
//!
//! Output (stdout): per-phase timing lines (the benchmark numbers) and a final DIGEST
//! line (the cross-language correctness gate). The digest is order-independent (a sum
//! mod a prime), so journey ordering never affects it.
//!
//! Usage: cargo run --release --bin bench -- [dataDir]
//! Default dataDir = <crate-dir>/../data (matching run.rs).

use std::env;
use std::path::PathBuf;
use std::time::Instant;

use raptor::date_util::UtcDate;
use raptor::filter::MultipleCriteriaFilter;
use raptor::filter::JourneyFilter;
use raptor::gtfs_loader::load_gtfs;
use raptor::journey::Journey;
use raptor::journey_factory::JourneyFactory;
use raptor::query::GroupStationDepartAfterQuery;
use raptor::raptor::RaptorAlgorithmFactory;

const DATE: &str = "2025-09-02"; // Tuesday, in service window
const GROUP_TIME: i64 = 36000; // 10:00, matching performance.ts
const RANGE_START: i64 = 28800; // 08:00
const RANGE_N: usize = 20; // "next 20 journeys"
const P: i64 = 1_000_000_007; // digest modulus

/// The 24 reference group-station queries (test/performance.ts).
fn group_queries() -> Vec<(Vec<&'static str>, Vec<&'static str>)> {
    let london = vec![
        "EUS", "MYB", "STP", "PAD", "BFR", "CTK", "CST", "CHX", "LBG", "WAE", "VIC", "VXH", "WAT",
        "OLD", "MOG", "KGX", "LST", "FST",
    ];
    vec![
        (vec!["MRF", "LVC", "LVJ", "LIV"], vec!["NRW"]),
        (vec!["TBW", "PDW"], vec!["HGS"]),
        (vec!["PDW", "MRN"], vec!["LVC", "LVJ", "LIV"]),
        (vec!["PDW", "AFK"], vec!["NRW"]),
        (vec!["PDW"], vec!["BHM", "BMO", "BSW", "BHI"]),
        (vec!["PNZ"], vec!["DIS"]),
        (vec!["YRK"], vec!["DIS"]),
        (vec!["WEY"], vec!["RDG"]),
        (vec!["YRK"], vec!["NRW"]),
        (vec!["BHM", "BMO", "BSW", "BHI"], vec!["MCO", "MAN", "MCV", "EXD"]),
        (vec!["BHM", "BMO", "BSW", "BHI"], vec!["EDB"]),
        (vec!["COV", "RUG"], vec!["MAN", "MCV"]),
        (vec!["YRK"], vec!["MCO", "MAN", "MCV", "EXD"]),
        (vec!["STA"], vec!["PBO"]),
        (vec!["PNZ"], vec!["EDB"]),
        (vec!["RDG"], vec!["IPS"]),
        (vec!["DVP"], vec!["BHM", "BMO", "BSW", "BHI"]),
        (vec!["BXB"], vec!["DVP"]),
        (vec!["MCO", "MAN", "MCV", "EXD"], vec!["CBW", "CBE"]),
        (vec!["MCO", "MAN", "MCV", "EXD"], london.clone()),
        (vec!["BHM", "BMO", "BSW", "BHI"], london.clone()),
        (vec!["ORP"], london.clone()),
        (vec!["EDB"], london.clone()),
        (vec!["CBE", "CBW"], london.clone()),
    ]
}

/// "next 20 journeys" pairs.
const RANGE_QUERIES: [(&str, &str); 5] = [
    ("TBW", "NRW"),
    ("BHM", "EDB"),
    ("PNZ", "DIS"),
    ("YRK", "NRW"),
    ("RDG", "IPS"),
];

/// Order-independent digest contribution of one journey.
fn journey_digest(j: &Journey) -> i64 {
    // dep,arr < 1e9 so dep*1_000_003 < 1e15, fits in i64 (max ~9.2e18) — matches the
    // Node BigInt math exactly without intermediate overflow.
    let dep = j.departure_time % 1_000_000_000;
    let arr = j.arrival_time % 1_000_000_000;
    let legs = j.legs.len() as i64;
    (dep * 1_000_003 + arr * 31 + legs) % P
}

fn accumulate(journeys: &[Journey], mut acc: i64) -> i64 {
    for j in journeys {
        acc = (acc + journey_digest(j)) % P;
    }
    acc
}

fn main() {
    let argv: Vec<String> = env::args().skip(1).collect();
    let data_dir = argv
        .first()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("data"));

    let t0 = Instant::now();
    let (trips, transfers, interchange) = load_gtfs(&data_dir);
    let load_ms = t0.elapsed().as_secs_f64() * 1000.0;

    let t1 = Instant::now();
    let raptor = RaptorAlgorithmFactory::create(trips, transfers, interchange, None);
    let prep_ms = t1.elapsed().as_secs_f64() * 1000.0;

    let jf = JourneyFactory::new();
    let mcf = MultipleCriteriaFilter::new();
    let filters: Vec<&dyn JourneyFilter> = vec![&mcf];
    let group_filtered = GroupStationDepartAfterQuery::new(&raptor, &jf, 3, filters);
    let group_plain = GroupStationDepartAfterQuery::new(&raptor, &jf, 3, Vec::new());

    let date = UtcDate::parse(DATE);

    // GROUP workload
    let tg = Instant::now();
    let mut group_count: usize = 0;
    let mut group_digest: i64 = 0;
    let queries = group_queries();
    for (origins, destinations) in &queries {
        let origins: Vec<String> = origins.iter().map(|s| s.to_string()).collect();
        let destinations: Vec<String> = destinations.iter().map(|s| s.to_string()).collect();
        let results = group_filtered.plan(&origins, &destinations, date, GROUP_TIME);
        group_count += results.len();
        group_digest = accumulate(&results, group_digest);
    }
    let group_ms = tg.elapsed().as_secs_f64() * 1000.0;

    // RANGE workload ("next 20")
    let tr = Instant::now();
    let mut range_count: usize = 0;
    let mut range_digest: i64 = 0;
    for (origin, destination) in RANGE_QUERIES.iter() {
        let results = next_n(&group_plain, origin, destination, date, RANGE_START, RANGE_N);
        range_count += results.len();
        range_digest = accumulate(&results, range_digest);
    }
    let range_ms = tr.elapsed().as_secs_f64() * 1000.0;

    println!("LOAD ms={load_ms:.1}");
    println!("PREP ms={prep_ms:.1}");
    println!(
        "GROUP queries={} journeys={} digest={} ms={:.1}",
        queries.len(),
        group_count,
        group_digest,
        group_ms
    );
    println!(
        "RANGE queries={} journeys={} digest={} ms={:.1}",
        RANGE_QUERIES.len(),
        range_count,
        range_digest,
        range_ms
    );
    println!(
        "DIGEST group={} range={} journeys={}",
        group_digest,
        range_digest,
        group_count + range_count
    );
}

/// "next N journeys departing after startTime" — the RangeQuery profile loop, capped at N.
fn next_n<R: raptor::journey_factory::ResultsFactory>(
    group: &GroupStationDepartAfterQuery<R>,
    origin: &str,
    destination: &str,
    date: UtcDate,
    start_time: i64,
    n: usize,
) -> Vec<Journey> {
    let origins = [origin.to_string()];
    let destinations = [destination.to_string()];
    let mut results: Vec<Journey> = Vec::new();
    let mut time = start_time;

    while results.len() < n {
        let new_results = group.plan(&origins, &destinations, date, time);
        if new_results.is_empty() {
            break;
        }
        let min_dep = new_results.iter().map(|j| j.departure_time).min().unwrap();
        results.extend(new_results);
        time = min_dep + 1;
    }

    results.sort_by(|a, b| {
        a.departure_time
            .cmp(&b.departure_time)
            .then(a.arrival_time.cmp(&b.arrival_time))
    });
    results.truncate(n);
    results
}
