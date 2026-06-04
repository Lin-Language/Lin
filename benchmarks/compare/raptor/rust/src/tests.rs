//! Unit tests mirroring the reference `describe/it` cases.

use std::collections::HashMap;
use std::rc::Rc;

use indexmap::IndexMap;

use crate::date_util::UtcDate;
use crate::filter::{JourneyFilter, MultipleCriteriaFilter};
use crate::gtfs::{StopTime, Time, Trip};
use crate::journey::{journey_lists_equal, Journey};
use crate::journey_factory::JourneyFactory;
use crate::queue::QueueFactory;
use crate::raptor::{Interchange, RaptorAlgorithmFactory, TransfersByOrigin};
use crate::scan_results::Connection;
use crate::service::Service;
use crate::test_util::*;
use crate::time_parser::TimeParser;
use crate::transfer_pattern::{GraphResults, StringResults, TreeNode};

// `st` shorthands. `s(stop, arr, dep)` allows None via the `_` sentinel below.
fn s(stop: &str, arr: Option<Time>, dep: Option<Time>) -> StopTime {
    st(stop, arr, dep)
}

const N: Option<Time> = None;
fn some(v: Time) -> Option<Time> {
    Some(v)
}

// Build a Vec<Rc<Trip>>.
fn trips(v: Vec<Rc<Trip>>) -> Vec<Rc<Trip>> {
    v
}

fn empty_transfers() -> TransfersByOrigin {
    IndexMap::new()
}
fn empty_interchange() -> Interchange {
    IndexMap::new()
}

// ===========================================================================
// Service.spec.ts
// ===========================================================================
mod service_spec {
    use super::*;

    #[test]
    fn checks_the_start_date() {
        let service = Service::new(20181001, 20181015, all_days(), HashMap::new());
        assert!(!service.runs_on(20180930, 1));
    }

    #[test]
    fn checks_the_end_date() {
        let service = Service::new(20181001, 20181015, all_days(), HashMap::new());
        assert!(!service.runs_on(20181016, 1));
    }

    #[test]
    fn checks_dates_within_range() {
        let service = Service::new(20181001, 20181015, all_days(), HashMap::new());
        assert!(service.runs_on(20181010, 1));
    }

    #[test]
    fn checks_the_day_of_the_week() {
        let mut days = all_days();
        days.insert(1, false);
        let service = Service::new(20181001, 20991231, days, HashMap::new());
        assert!(!service.runs_on(20181016, 1));
    }

    #[test]
    fn checks_include_days() {
        let mut dates = HashMap::new();
        dates.insert(20181022, true);
        let service = Service::new(20991231, 20991231, all_days(), dates);
        assert!(service.runs_on(20181022, 1));
    }

    #[test]
    fn checks_exclude_days() {
        let mut dates = HashMap::new();
        dates.insert(20181022, false);
        let service = Service::new(20181001, 20991231, all_days(), dates);
        assert!(!service.runs_on(20181022, 1));
    }
}

// ===========================================================================
// TimeParser.spec.ts
// ===========================================================================
mod time_parser_spec {
    use super::*;

    #[test]
    fn turns_a_time_string_into_seconds_from_midnight() {
        let mut parser = TimeParser::new();
        assert_eq!(0, parser.get_time("00:00:00"));
        assert_eq!(10, parser.get_time("00:00:10"));
        assert_eq!(130, parser.get_time("00:02:10"));
        assert_eq!(10930, parser.get_time("03:02:10"));
    }
}

// ===========================================================================
// QueueFactory.spec.ts
// ===========================================================================
mod queue_factory_spec {
    use super::*;

    fn idx(pairs: &[(&str, usize)]) -> IndexMap<String, usize> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn enqueues_stops() {
        let mut routes_at_stop: IndexMap<String, Vec<String>> = IndexMap::new();
        routes_at_stop.insert(
            "StopA".into(),
            vec!["RouteA".into(), "RouteB".into()],
        );
        routes_at_stop.insert(
            "StopB".into(),
            vec!["RouteB".into(), "RouteC".into()],
        );

        let mut route_stop_index: IndexMap<String, IndexMap<String, usize>> = IndexMap::new();
        route_stop_index.insert("RouteA".into(), idx(&[("StopA", 1)]));
        route_stop_index.insert("RouteB".into(), idx(&[("StopA", 2), ("StopB", 1)]));
        route_stop_index.insert("RouteC".into(), idx(&[("StopB", 1)]));

        let factory = QueueFactory::new(&routes_at_stop, &route_stop_index);
        let actual = factory.get_queue(&["StopA".into(), "StopB".into()]);

        let expected: Vec<(&str, &str)> = vec![
            ("RouteA", "StopA"),
            ("RouteB", "StopB"),
            ("RouteC", "StopB"),
        ];
        let got: Vec<(&str, &str)> = actual
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn picks_the_earliest_stop_on_the_route() {
        let mut routes_at_stop: IndexMap<String, Vec<String>> = IndexMap::new();
        routes_at_stop.insert("StopA".into(), vec!["RouteA".into(), "RouteB".into()]);
        routes_at_stop.insert("StopB".into(), vec!["RouteB".into(), "RouteC".into()]);

        let mut route_stop_index: IndexMap<String, IndexMap<String, usize>> = IndexMap::new();
        route_stop_index.insert("RouteA".into(), idx(&[("StopA", 1)]));
        route_stop_index.insert("RouteB".into(), idx(&[("StopA", 1), ("StopB", 2)]));
        route_stop_index.insert("RouteC".into(), idx(&[("StopB", 1)]));

        let factory = QueueFactory::new(&routes_at_stop, &route_stop_index);
        let actual = factory.get_queue(&["StopB".into(), "StopA".into()]);

        // Expected key order: RouteB, RouteC inserted first (from StopB), then RouteA.
        let expected: Vec<(&str, &str)> = vec![
            ("RouteB", "StopA"),
            ("RouteC", "StopB"),
            ("RouteA", "StopA"),
        ];
        let got: Vec<(&str, &str)> = actual
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        // toEqual ignores key order in JS, so compare as a map.
        let got_map: HashMap<_, _> = got.iter().cloned().collect();
        let exp_map: HashMap<_, _> = expected.iter().cloned().collect();
        assert_eq!(got_map, exp_map);
    }
}

// helpers to assert equality the way `toEqual` does (order-sensitive for arrays)
fn assert_journeys_eq(actual: &[Journey], expected: &[Journey]) {
    assert!(
        journey_lists_equal(actual, expected),
        "journeys differ:\nactual={actual:#?}\nexpected={expected:#?}"
    );
}

fn plan_depart_after(
    trips: Vec<Rc<Trip>>,
    transfers: TransfersByOrigin,
    interchange: Interchange,
    origin: &str,
    destination: &str,
    date: &str,
    time: Time,
    max_days: usize,
) -> Vec<Journey> {
    let raptor = RaptorAlgorithmFactory::create(trips, transfers, interchange, None);
    let jf = JourneyFactory::new();
    let query = crate::query::DepartAfterQuery::new(&raptor, &jf, max_days);
    let mut result = query.plan(origin, destination, UtcDate::parse(date), time);
    set_default_trip(&mut result);
    result
}

// ===========================================================================
// DepartAfterQuery.spec.ts
// ===========================================================================
mod depart_after_query_spec {
    use super::*;
    use crate::test_util::LegInput::{StopTimes as ST, Transfer as TR};

    #[test]
    fn finds_journeys_with_direct_connections() {
        let trips = trips(vec![t(vec![
            s("A", N, some(1000)),
            s("B", some(1030), some(1035)),
            s("C", some(1100), N),
        ])]);

        let result = plan_depart_after(
            trips,
            empty_transfers(),
            empty_interchange(),
            "A",
            "C",
            "2018-10-16",
            900,
            3,
        );

        let expected = vec![j(vec![ST(vec![
            s("A", N, some(1000)),
            s("B", some(1030), some(1035)),
            s("C", some(1100), N),
        ])])];

        assert_journeys_eq(&result, &expected);
    }

    #[test]
    fn finds_the_earliest_calendars() {
        let trips = trips(vec![
            t(vec![
                s("A", N, some(1400)),
                s("B", some(1430), some(1435)),
                s("C", some(1500), N),
            ]),
            t(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1035)),
                s("C", some(1100), N),
            ]),
        ]);

        let result = plan_depart_after(
            trips,
            empty_transfers(),
            empty_interchange(),
            "A",
            "C",
            "2018-10-16",
            900,
            3,
        );

        let expected = vec![j(vec![ST(vec![
            s("A", N, some(1000)),
            s("B", some(1030), some(1035)),
            s("C", some(1100), N),
        ])])];

        assert_journeys_eq(&result, &expected);
    }

    #[test]
    fn finds_journeys_with_a_single_connection() {
        let trips = trips(vec![
            t(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1035)),
                s("C", some(1100), N),
            ]),
            t(vec![
                s("D", N, some(1000)),
                s("B", some(1030), some(1035)),
                s("E", some(1100), N),
            ]),
        ]);

        let result = plan_depart_after(
            trips,
            empty_transfers(),
            empty_interchange(),
            "A",
            "E",
            "2018-10-16",
            900,
            3,
        );

        let expected = vec![j(vec![
            ST(vec![s("A", N, some(1000)), s("B", some(1030), some(1035))]),
            ST(vec![s("B", some(1030), some(1035)), s("E", some(1100), N)]),
        ])];

        assert_journeys_eq(&result, &expected);
    }

    #[test]
    fn does_not_return_journeys_that_cannot_be_made() {
        let trips = trips(vec![
            t(vec![
                s("A", N, some(1000)),
                s("B", some(1035), some(1035)),
                s("C", some(1100), N),
            ]),
            t(vec![
                s("D", N, some(1000)),
                s("B", some(1030), some(1030)),
                s("E", some(1100), N),
            ]),
        ]);

        let result = plan_depart_after(
            trips,
            empty_transfers(),
            empty_interchange(),
            "A",
            "E",
            "2018-10-16",
            900,
            1,
        );

        assert_journeys_eq(&result, &[]);
    }

    #[test]
    fn returns_the_fastest_and_the_least_changes() {
        let trips = trips(vec![
            t(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1030)),
                s("C", some(1200), N),
            ]),
            t(vec![s("B", N, some(1030)), s("C", some(1100), N)]),
        ]);

        let result = plan_depart_after(
            trips,
            empty_transfers(),
            empty_interchange(),
            "A",
            "C",
            "2018-10-16",
            900,
            3,
        );

        let direct = j(vec![ST(vec![
            s("A", N, some(1000)),
            s("B", some(1030), some(1030)),
            s("C", some(1200), N),
        ])]);
        let change = j(vec![
            ST(vec![s("A", N, some(1000)), s("B", some(1030), some(1030))]),
            ST(vec![s("B", N, some(1030)), s("C", some(1100), N)]),
        ]);

        assert_journeys_eq(&result, &[direct, change]);
    }

    #[test]
    fn chooses_the_fastest_journey_where_the_number_of_journeys_is_the_same() {
        let trips = trips(vec![
            t(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1030)),
                s("C", some(1100), N),
            ]),
            t(vec![
                s("C", N, some(1200)),
                s("D", some(1230), some(1230)),
                s("E", some(1300), N),
            ]),
            t(vec![
                s("A", N, some(1100)),
                s("F", some(1130), some(1130)),
                s("G", some(1200), N),
            ]),
            t(vec![
                s("G", N, some(1200)),
                s("H", some(1230), some(1230)),
                s("E", some(1255), N),
            ]),
        ]);

        let result = plan_depart_after(
            trips,
            empty_transfers(),
            empty_interchange(),
            "A",
            "E",
            "2018-10-16",
            900,
            3,
        );

        let fastest = j(vec![
            ST(vec![
                s("A", N, some(1100)),
                s("F", some(1130), some(1130)),
                s("G", some(1200), N),
            ]),
            ST(vec![
                s("G", N, some(1200)),
                s("H", some(1230), some(1230)),
                s("E", some(1255), N),
            ]),
        ]);

        assert_journeys_eq(&result, &[fastest]);
    }

    #[test]
    fn chooses_an_arbitrary_journey_when_they_are_the_same() {
        let trips = trips(vec![
            t(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1030)),
                s("C", some(1100), N),
            ]),
            t(vec![
                s("C", N, some(1200)),
                s("D", some(1230), some(1230)),
                s("E", some(1300), N),
            ]),
            t(vec![
                s("A", N, some(1100)),
                s("F", some(1130), some(1130)),
                s("G", some(1200), N),
            ]),
            t(vec![
                s("G", N, some(1200)),
                s("H", some(1230), some(1230)),
                s("E", some(1300), N),
            ]),
        ]);

        let result = plan_depart_after(
            trips,
            empty_transfers(),
            empty_interchange(),
            "A",
            "E",
            "2018-10-16",
            900,
            3,
        );

        let journey1 = j(vec![
            ST(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1030)),
                s("C", some(1100), N),
            ]),
            ST(vec![
                s("C", N, some(1200)),
                s("D", some(1230), some(1230)),
                s("E", some(1300), N),
            ]),
        ]);

        assert_journeys_eq(&result, &[journey1]);
    }

    #[test]
    fn chooses_the_correct_change_point() {
        let trips = trips(vec![
            t(vec![s("A", N, some(1000)), s("B", some(1030), N)]),
            t(vec![s("A", N, some(1030)), s("C", some(1200), N)]),
            t(vec![
                s("C", N, some(1000)),
                s("B", some(1030), some(1030)),
                s("E", some(1100), N),
            ]),
            t(vec![
                s("C", N, some(1200)),
                s("B", some(1230), some(1230)),
                s("E", some(1300), N),
            ]),
        ]);

        let result = plan_depart_after(
            trips,
            empty_transfers(),
            empty_interchange(),
            "A",
            "E",
            "2018-10-16",
            900,
            3,
        );

        let change = j(vec![
            ST(vec![s("A", N, some(1000)), s("B", some(1030), N)]),
            ST(vec![s("B", some(1030), some(1030)), s("E", some(1100), N)]),
        ]);

        assert_journeys_eq(&result, &[change]);
    }

    #[test]
    fn finds_journeys_with_a_transfer() {
        let trips = trips(vec![
            t(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1035)),
                s("C", some(1100), N),
            ]),
            t(vec![s("D", N, some(1200)), s("E", some(1300), N)]),
        ]);

        let mut transfers: TransfersByOrigin = IndexMap::new();
        transfers.insert("C".into(), vec![tf("C", "D", 10)]);

        let result = plan_depart_after(
            trips,
            transfers,
            empty_interchange(),
            "A",
            "E",
            "2018-10-16",
            900,
            3,
        );

        let expected = vec![j(vec![
            ST(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1035)),
                s("C", some(1100), N),
            ]),
            TR(tf("C", "D", 10)),
            ST(vec![s("D", N, some(1200)), s("E", some(1300), N)]),
        ])];

        assert_journeys_eq(&result, &expected);
    }

    #[test]
    fn uses_a_transfer_if_it_is_faster() {
        let trips = trips(vec![
            t(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1030)),
                s("C", some(1100), N),
            ]),
            t(vec![s("C", N, some(1130)), s("D", some(1200), N)]),
        ]);

        let mut transfers: TransfersByOrigin = IndexMap::new();
        transfers.insert("C".into(), vec![tf("C", "D", 10)]);

        let result = plan_depart_after(
            trips,
            transfers,
            empty_interchange(),
            "A",
            "D",
            "2018-10-16",
            900,
            3,
        );

        let transfer = j(vec![
            ST(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1030)),
                s("C", some(1100), N),
            ]),
            TR(tf("C", "D", 10)),
        ]);

        assert_journeys_eq(&result, &[transfer]);
    }

    #[test]
    fn doesnt_allow_pick_up_from_locations_without_pickup_specified() {
        let trips = trips(vec![
            t(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1030)),
                s("C", some(1200), N),
            ]),
            t(vec![
                s("E", N, some(1000)),
                s("B", some(1030), N),
                s("C", some(1100), N),
            ]),
        ]);

        let result = plan_depart_after(
            trips,
            empty_transfers(),
            empty_interchange(),
            "A",
            "C",
            "2018-10-16",
            900,
            3,
        );

        let direct = j(vec![ST(vec![
            s("A", N, some(1000)),
            s("B", some(1030), some(1030)),
            s("C", some(1200), N),
        ])]);

        assert_journeys_eq(&result, &[direct]);
    }

    #[test]
    fn doesnt_allow_drop_off_at_non_drop_off_locations() {
        let trips = trips(vec![
            t(vec![
                s("A", N, some(1000)),
                s("B", N, some(1030)),
                s("C", some(1200), N),
            ]),
            t(vec![
                s("E", N, some(1000)),
                s("B", some(1030), some(1030)),
                s("C", some(1100), N),
            ]),
        ]);

        let result = plan_depart_after(
            trips,
            empty_transfers(),
            empty_interchange(),
            "A",
            "C",
            "2018-10-16",
            900,
            3,
        );

        let direct = j(vec![ST(vec![
            s("A", N, some(1000)),
            s("B", N, some(1030)),
            s("C", some(1200), N),
        ])]);

        assert_journeys_eq(&result, &[direct]);
    }

    #[test]
    fn applies_interchange_times() {
        let trips = trips(vec![
            t(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1030)),
                s("C", some(1200), N),
            ]),
            t(vec![s("B", N, some(1030)), s("C", some(1100), N)]),
            t(vec![s("B", N, some(1040)), s("C", some(1110), N)]),
        ]);

        let mut interchange: Interchange = IndexMap::new();
        interchange.insert("B".into(), 10);

        let result = plan_depart_after(
            trips,
            empty_transfers(),
            interchange,
            "A",
            "C",
            "2018-10-16",
            900,
            3,
        );

        let direct = j(vec![ST(vec![
            s("A", N, some(1000)),
            s("B", some(1030), some(1030)),
            s("C", some(1200), N),
        ])]);
        let change = j(vec![
            ST(vec![s("A", N, some(1000)), s("B", some(1030), some(1030))]),
            ST(vec![s("B", N, some(1040)), s("C", some(1110), N)]),
        ]);

        assert_journeys_eq(&result, &[direct, change]);
    }

    #[test]
    fn applies_interchange_times_to_transfers() {
        let trips = trips(vec![
            t(vec![s("A", N, some(1000)), s("B", some(1030), N)]),
            t(vec![s("C", N, some(1030)), s("D", some(1100), N)]),
            t(vec![s("C", N, some(1050)), s("D", some(1110), N)]),
            t(vec![s("C", N, some(1100)), s("D", some(1120), N)]),
        ]);

        let mut transfers: TransfersByOrigin = IndexMap::new();
        transfers.insert("B".into(), vec![tf("B", "C", 10)]);

        let mut interchange: Interchange = IndexMap::new();
        interchange.insert("B".into(), 10);
        interchange.insert("C".into(), 10);

        let result = plan_depart_after(
            trips,
            transfers,
            interchange,
            "A",
            "D",
            "2018-10-16",
            900,
            3,
        );

        let last_possible = j(vec![
            ST(vec![s("A", N, some(1000)), s("B", some(1030), N)]),
            TR(tf("B", "C", 10)),
            ST(vec![s("C", N, some(1100)), s("D", some(1120), N)]),
        ]);

        assert_journeys_eq(&result, &[last_possible]);
    }

    fn plan_with_service(
        trips_in: Vec<Rc<Trip>>,
        date: &str,
    ) -> Vec<Journey> {
        plan_depart_after(
            trips_in,
            empty_transfers(),
            empty_interchange(),
            "A",
            "C",
            date,
            900,
            3,
        )
    }

    #[test]
    fn omits_calendars_not_running_that_day() {
        let mut trip = (*t(vec![s("B", N, some(1030)), s("C", some(1100), N)])).clone();
        trip.service = Rc::new(Service::new(20181001, 20181015, all_days(), HashMap::new()));

        let trips = trips(vec![
            t(vec![s("A", N, some(1000)), s("B", some(1030), N)]),
            Rc::new(trip),
            t(vec![s("B", N, some(1040)), s("C", some(1110), N)]),
        ]);

        let result = plan_with_service(trips, "2018-10-16");

        let change = j(vec![
            ST(vec![s("A", N, some(1000)), s("B", some(1030), N)]),
            ST(vec![s("B", N, some(1040)), s("C", some(1110), N)]),
        ]);

        assert_journeys_eq(&result, &[change]);
    }

    #[test]
    fn omits_calendars_not_running_that_day_of_the_week() {
        let mut days = all_days();
        days.insert(1, false);
        let mut trip = (*t(vec![s("B", N, some(1030)), s("C", some(1100), N)])).clone();
        trip.service = Rc::new(Service::new(20181001, 20991231, days, HashMap::new()));

        let trips = trips(vec![
            t(vec![s("A", N, some(1000)), s("B", some(1030), N)]),
            Rc::new(trip),
            t(vec![s("B", N, some(1040)), s("C", some(1110), N)]),
        ]);

        let result = plan_with_service(trips, "2018-10-22");

        let change = j(vec![
            ST(vec![s("A", N, some(1000)), s("B", some(1030), N)]),
            ST(vec![s("B", N, some(1040)), s("C", some(1110), N)]),
        ]);

        assert_journeys_eq(&result, &[change]);
    }

    #[test]
    fn includes_calendars_with_an_include_day() {
        let mut dates = HashMap::new();
        dates.insert(20181022, true);
        let mut trip = (*t(vec![s("B", N, some(1030)), s("C", some(1100), N)])).clone();
        trip.service = Rc::new(Service::new(20991231, 20991231, all_days(), dates));

        let trips = trips(vec![
            t(vec![s("A", N, some(1000)), s("B", some(1030), N)]),
            Rc::new(trip),
            t(vec![s("B", N, some(1040)), s("C", some(1110), N)]),
        ]);

        let result = plan_with_service(trips, "2018-10-22");

        let change = j(vec![
            ST(vec![s("A", N, some(1000)), s("B", some(1030), N)]),
            ST(vec![s("B", N, some(1030)), s("C", some(1100), N)]),
        ]);

        assert_journeys_eq(&result, &[change]);
    }

    #[test]
    fn omits_calendars_with_an_exclude_day() {
        let mut dates = HashMap::new();
        dates.insert(20181022, false);
        let mut trip = (*t(vec![s("B", N, some(1030)), s("C", some(1100), N)])).clone();
        trip.service = Rc::new(Service::new(20181001, 20991231, all_days(), dates));

        let trips = trips(vec![
            t(vec![s("A", N, some(1000)), s("B", some(1030), N)]),
            Rc::new(trip),
            t(vec![s("B", N, some(1040)), s("C", some(1110), N)]),
        ]);

        let result = plan_with_service(trips, "2018-10-22");

        let change = j(vec![
            ST(vec![s("A", N, some(1000)), s("B", some(1030), N)]),
            ST(vec![s("B", N, some(1040)), s("C", some(1110), N)]),
        ]);

        assert_journeys_eq(&result, &[change]);
    }

    #[test]
    fn finds_journeys_after_gaps_in_rounds() {
        let trips = trips(vec![
            t(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1035)),
                s("C", some(1400), N),
            ]),
            t(vec![s("B", N, some(1035)), s("D", some(1100), N)]),
            t(vec![s("D", N, some(1100)), s("E", some(1130), N)]),
            t(vec![s("E", N, some(1130)), s("C", some(1200), N)]),
            t(vec![s("A", N, some(1000)), s("E", some(1135), N)]),
            t(vec![s("E", N, some(1135)), s("C", some(1330), N)]),
        ]);

        let result = plan_depart_after(
            trips,
            empty_transfers(),
            empty_interchange(),
            "A",
            "C",
            "2018-10-16",
            900,
            3,
        );

        let direct = j(vec![ST(vec![
            s("A", N, some(1000)),
            s("B", some(1030), some(1035)),
            s("C", some(1400), N),
        ])]);
        let slow_change = j(vec![
            ST(vec![s("A", N, some(1000)), s("E", some(1135), N)]),
            ST(vec![s("E", N, some(1135)), s("C", some(1330), N)]),
        ]);
        let change = j(vec![
            ST(vec![s("A", N, some(1000)), s("B", some(1030), some(1035))]),
            ST(vec![s("B", N, some(1035)), s("D", some(1100), N)]),
            ST(vec![s("D", N, some(1100)), s("E", some(1130), N)]),
            ST(vec![s("E", N, some(1130)), s("C", some(1200), N)]),
        ]);

        assert_journeys_eq(&result, &[direct, slow_change, change]);
    }

    #[test]
    fn puts_overtaken_trains_in_different_routes() {
        let trips = trips(vec![
            t(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1030)),
                s("C", some(1100), some(1110)),
                s("D", some(1130), some(1130)),
                s("E", some(1200), N),
            ]),
            t(vec![
                s("A", N, some(1010)),
                s("B", some(1040), some(1040)),
                s("C", some(1050), some(1100)),
                s("D", some(1120), some(1120)),
                s("E", some(1150), N),
            ]),
        ]);

        let result = plan_depart_after(
            trips,
            empty_transfers(),
            empty_interchange(),
            "A",
            "E",
            "2018-10-16",
            900,
            3,
        );

        let faster = j(vec![ST(vec![
            s("A", N, some(1010)),
            s("B", some(1040), some(1040)),
            s("C", some(1050), some(1100)),
            s("D", some(1120), some(1120)),
            s("E", some(1150), N),
        ])]);

        assert_journeys_eq(&result, &[faster]);
    }

    #[test]
    fn finds_journeys_that_can_only_be_made_by_waiting_for_the_next_day() {
        let trips = trips(vec![
            t(vec![
                s("A", N, some(1000)),
                s("B", some(1035), some(1035)),
                s("C", some(1100), N),
            ]),
            t(vec![
                s("D", N, some(1000)),
                s("B", some(1030), some(1030)),
                s("E", some(1100), N),
            ]),
        ]);

        let result = plan_depart_after(
            trips,
            empty_transfers(),
            empty_interchange(),
            "A",
            "E",
            "2018-10-16",
            900,
            2,
        );

        let expected = j(vec![
            ST(vec![s("A", N, some(1000)), s("B", some(1035), some(1035))]),
            ST(vec![s("B", some(1030), some(1030)), s("E", some(1100), N)]),
        ]);

        // only legs are compared in the reference
        assert!(
            crate::journey::journeys_equal(
                &Journey {
                    legs: result[0].legs.clone(),
                    departure_time: expected.departure_time,
                    arrival_time: expected.arrival_time,
                },
                &expected
            ),
            "legs differ:\n{:#?}\nvs\n{:#?}",
            result[0].legs,
            expected.legs
        );
    }

    #[test]
    fn adds_a_day_to_the_arrival_time_of_journeys_that_are_made_overnight() {
        let trips = trips(vec![
            t(vec![
                s("A", N, some(1000)),
                s("B", some(1035), some(1035)),
                s("C", some(1100), N),
            ]),
            t(vec![
                s("D", N, some(1000)),
                s("B", some(1030), some(1030)),
                s("E", some(1100), N),
            ]),
        ]);

        let result = plan_depart_after(
            trips,
            empty_transfers(),
            empty_interchange(),
            "A",
            "E",
            "2018-10-16",
            900,
            2,
        );

        assert_eq!(result[0].arrival_time, 1100 + 86400);
    }

    #[test]
    fn increments_the_day_when_searching_subsequent_days() {
        let trip2 = {
            let mut tr =
                (*t(vec![
                    s("D", N, some(1000)),
                    s("B", some(1035), some(1035)),
                    s("E", some(1100), N),
                ]))
                .clone();
            tr.service = service2();
            Rc::new(tr)
        };

        let trips = trips(vec![
            t(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1030)),
                s("C", some(1100), N),
            ]),
            trip2,
        ]);

        let result = plan_depart_after(
            trips,
            empty_transfers(),
            empty_interchange(),
            "A",
            "E",
            "2018-12-31",
            900,
            2,
        );

        assert_eq!(result[0].arrival_time, 1100 + 86400);
    }

    #[test]
    fn uses_all_results_from_every_day() {
        let trips = trips(vec![
            t(vec![
                s("A", N, some(1900)),
                s("B", some(1930), some(1935)),
                s("C", some(2000), N),
            ]),
            t(vec![s("B", N, some(1035)), s("D", some(1100), N)]),
            t(vec![s("D", N, some(1100)), s("E", some(1130), N)]),
            t(vec![s("C", N, some(1130)), s("E", some(1200), N)]),
        ]);

        let result = plan_depart_after(
            trips,
            empty_transfers(),
            empty_interchange(),
            "A",
            "E",
            "2019-04-23",
            900,
            2,
        );

        let mut change = j(vec![
            ST(vec![s("A", N, some(1900)), s("B", some(1930), some(1935))]),
            ST(vec![s("B", N, some(1035)), s("D", some(1100), N)]),
            ST(vec![s("D", N, some(1100)), s("E", some(1130), N)]),
        ]);
        let mut no_change = j(vec![
            ST(vec![
                s("A", N, some(1900)),
                s("B", some(1930), some(1935)),
                s("C", some(2000), N),
            ]),
            ST(vec![s("C", N, some(1130)), s("E", some(1200), N)]),
        ]);
        change.arrival_time += 86400;
        no_change.arrival_time += 86400;

        assert_journeys_eq(&result, &[no_change, change]);
    }

    #[test]
    fn does_not_return_overnight_journeys_that_cannot_be_made() {
        let trips = trips(vec![
            t(vec![
                s("A", N, some(86000)),
                s("B", some(86400), some(86400)),
                s("C", some(86400 + 3600 + 3600), N),
            ]),
            t(vec![
                s("C", N, some(3600)),
                s("D", some(3635), some(3635)),
                s("E", some(3700), N),
            ]),
            t(vec![
                s("C", N, some(3600 + 3600)),
                s("D", some(3635 + 3600), some(3635 + 3600)),
                s("E", some(3700 + 3600), N),
            ]),
        ]);

        let result = plan_depart_after(
            trips,
            empty_transfers(),
            empty_interchange(),
            "A",
            "E",
            "2018-12-31",
            50000,
            2,
        );

        assert_eq!(result[0].arrival_time, 86400 + 3700 + 3600);
    }
}

// ===========================================================================
// GroupStationDepartAfterQuery.spec.ts
// ===========================================================================
mod group_station_spec {
    use super::*;
    use crate::test_util::LegInput::StopTimes as ST;

    fn plan_group(
        trips_in: Vec<Rc<Trip>>,
        origins: &[&str],
        destinations: &[&str],
        date: &str,
        time: Time,
    ) -> Vec<Journey> {
        let raptor = RaptorAlgorithmFactory::create(
            trips_in,
            empty_transfers(),
            empty_interchange(),
            None,
        );
        let jf = JourneyFactory::new();
        let filter = MultipleCriteriaFilter::new();
        let filters: Vec<&dyn JourneyFilter> = vec![&filter];
        let query =
            crate::query::GroupStationDepartAfterQuery::new(&raptor, &jf, 1, filters);
        let origins: Vec<String> = origins.iter().map(|s| s.to_string()).collect();
        let destinations: Vec<String> = destinations.iter().map(|s| s.to_string()).collect();
        let mut result = query.plan(&origins, &destinations, UtcDate::parse(date), time);
        set_default_trip(&mut result);
        result
    }

    #[test]
    fn plans_to_multiple_destinations() {
        let trips = vec![
            t(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1035)),
                s("C", some(1100), N),
            ]),
            t(vec![
                s("A", N, some(1200)),
                s("B", some(1230), some(1235)),
                s("D", some(1300), N),
            ]),
        ];

        let result = plan_group(trips, &["A"], &["C", "D"], "2019-04-18", 900);

        let expected = vec![
            j(vec![ST(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1035)),
                s("C", some(1100), N),
            ])]),
            j(vec![ST(vec![
                s("A", N, some(1200)),
                s("B", some(1230), some(1235)),
                s("D", some(1300), N),
            ])]),
        ];

        assert_journeys_eq(&result, &expected);
    }

    #[test]
    fn plans_from_multiple_origins() {
        let trips = vec![
            t(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1035)),
                s("C", some(1100), N),
            ]),
            t(vec![
                s("A", N, some(1200)),
                s("_", some(1230), some(1235)),
                s("D", some(1300), N),
            ]),
        ];

        let result = plan_group(trips, &["A", "B"], &["C", "D"], "2019-04-18", 900);

        let expected = vec![
            j(vec![ST(vec![
                s("B", some(1030), some(1035)),
                s("C", some(1100), N),
            ])]),
            j(vec![ST(vec![
                s("A", N, some(1200)),
                s("_", some(1230), some(1235)),
                s("D", some(1300), N),
            ])]),
        ];

        assert_journeys_eq(&result, &expected);
    }
}

// ===========================================================================
// RangeQuery.spec.ts
// ===========================================================================
mod range_query_spec {
    use super::*;
    use crate::test_util::LegInput::StopTimes as ST;

    fn plan_range(trips_in: Vec<Rc<Trip>>, origin: &str, dest: &str, date: &str) -> Vec<Journey> {
        let raptor = RaptorAlgorithmFactory::create(
            trips_in,
            empty_transfers(),
            empty_interchange(),
            None,
        );
        let jf = JourneyFactory::new();
        let query = crate::query::RangeQuery::new(&raptor, &jf, 3, Vec::new());
        let mut result = query.plan(origin, dest, UtcDate::parse(date), 1, 24 * 60 * 60);
        set_default_trip(&mut result);
        result
    }

    #[test]
    fn performs_profile_queries() {
        let trips = vec![
            t(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1035)),
                s("C", some(1100), N),
            ]),
            t(vec![
                s("A", N, some(1100)),
                s("B", some(1130), some(1135)),
                s("C", some(1200), N),
            ]),
            t(vec![
                s("A", N, some(1200)),
                s("B", some(1230), some(1235)),
                s("C", some(1300), N),
            ]),
        ];

        let result = plan_range(trips, "A", "C", "2018-10-16");

        let expected = vec![
            j(vec![ST(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1035)),
                s("C", some(1100), N),
            ])]),
            j(vec![ST(vec![
                s("A", N, some(1100)),
                s("B", some(1130), some(1135)),
                s("C", some(1200), N),
            ])]),
            j(vec![ST(vec![
                s("A", N, some(1200)),
                s("B", some(1230), some(1235)),
                s("C", some(1300), N),
            ])]),
        ];

        assert_journeys_eq(&result, &expected);
    }

    #[test]
    fn does_not_share_best_arrivals_or_route_scanner() {
        let trips = vec![
            t(vec![s("A", N, some(1359)), s("C", some(1501), N)]),
            t(vec![s("A", N, some(1400)), s("B", some(1430), N)]),
            t(vec![s("B", N, some(1430)), s("C", some(1500), N)]),
        ];

        let result = plan_range(trips, "A", "C", "2018-10-16");

        let expected = vec![
            j(vec![ST(vec![s("A", N, some(1359)), s("C", some(1501), N)])]),
            j(vec![
                ST(vec![s("A", N, some(1400)), s("B", some(1430), N)]),
                ST(vec![s("B", N, some(1430)), s("C", some(1500), N)]),
            ]),
            j(vec![
                ST(vec![s("A", N, some(1400)), s("B", some(1430), N)]),
                ST(vec![s("B", N, some(1430)), s("C", some(1500), N)]),
            ]),
        ];

        assert_journeys_eq(&result, &expected);
    }
}

// ===========================================================================
// MultipleCriteriaFilter.spec.ts
// ===========================================================================
mod multiple_criteria_filter_spec {
    use super::*;
    use crate::test_util::LegInput::StopTimes as ST;

    #[test]
    fn removes_slower_journeys() {
        let journeys = vec![
            j(vec![ST(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1035)),
                s("C", some(1100), N),
            ])]),
            j(vec![ST(vec![
                s("A", N, some(900)),
                s("B", some(1030), some(1035)),
                s("C", some(1100), N),
            ])]),
            j(vec![ST(vec![
                s("A", N, some(1100)),
                s("B", some(1130), some(1135)),
                s("C", some(1200), N),
            ])]),
            j(vec![ST(vec![
                s("A", N, some(1200)),
                s("B", some(1230), some(1235)),
                s("C", some(1300), N),
            ])]),
        ];

        let expected = vec![
            j(vec![ST(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1035)),
                s("C", some(1100), N),
            ])]),
            j(vec![ST(vec![
                s("A", N, some(1100)),
                s("B", some(1130), some(1135)),
                s("C", some(1200), N),
            ])]),
            j(vec![ST(vec![
                s("A", N, some(1200)),
                s("B", some(1230), some(1235)),
                s("C", some(1300), N),
            ])]),
        ];

        let actual = MultipleCriteriaFilter::new().apply(journeys);
        assert_journeys_eq(&actual, &expected);
    }

    #[test]
    fn keeps_slower_journeys_if_they_have_fewer_changes() {
        let journeys = vec![
            j(vec![
                ST(vec![s("A", N, some(1000)), s("B", some(1030), some(1035))]),
                ST(vec![s("C", some(1100), N)]),
            ]),
            j(vec![ST(vec![s("A", N, some(900)), s("C", some(1100), N)])]),
            j(vec![ST(vec![
                s("A", N, some(1100)),
                s("B", some(1130), some(1135)),
                s("C", some(1200), N),
            ])]),
            j(vec![ST(vec![
                s("A", N, some(1200)),
                s("B", some(1230), some(1235)),
                s("C", some(1300), N),
            ])]),
        ];

        let expected = vec![
            j(vec![ST(vec![s("A", N, some(900)), s("C", some(1100), N)])]),
            j(vec![
                ST(vec![s("A", N, some(1000)), s("B", some(1030), some(1035))]),
                ST(vec![s("C", some(1100), N)]),
            ]),
            j(vec![ST(vec![
                s("A", N, some(1100)),
                s("B", some(1130), some(1135)),
                s("C", some(1200), N),
            ])]),
            j(vec![ST(vec![
                s("A", N, some(1200)),
                s("B", some(1230), some(1235)),
                s("C", some(1300), N),
            ])]),
        ];

        let actual = MultipleCriteriaFilter::new().apply(journeys);
        assert_journeys_eq(&actual, &expected);
    }

    #[test]
    fn sorts_journeys_before_filtering_them() {
        let journeys = vec![
            j(vec![ST(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1035)),
                s("C", some(1100), N),
            ])]),
            j(vec![ST(vec![
                s("A", N, some(900)),
                s("B", some(1030), some(1035)),
                s("C", some(1100), N),
            ])]),
            j(vec![ST(vec![
                s("A", N, some(1100)),
                s("B", some(1130), some(1135)),
                s("C", some(1200), N),
            ])]),
            j(vec![ST(vec![
                s("A", N, some(1200)),
                s("B", some(1230), some(1235)),
                s("C", some(1300), N),
            ])]),
            j(vec![ST(vec![
                s("A", N, some(1200)),
                s("B", some(1230), some(1235)),
                s("C", some(1330), N),
            ])]),
        ];

        let expected = vec![
            j(vec![ST(vec![
                s("A", N, some(1000)),
                s("B", some(1030), some(1035)),
                s("C", some(1100), N),
            ])]),
            j(vec![ST(vec![
                s("A", N, some(1100)),
                s("B", some(1130), some(1135)),
                s("C", some(1200), N),
            ])]),
            j(vec![ST(vec![
                s("A", N, some(1200)),
                s("B", some(1230), some(1235)),
                s("C", some(1300), N),
            ])]),
        ];

        let actual = MultipleCriteriaFilter::new().apply(journeys);
        assert_journeys_eq(&actual, &expected);
    }
}

// ===========================================================================
// transfer-pattern: GraphResults.spec.ts + StringResults.spec.ts
// ===========================================================================
mod transfer_pattern_spec {
    use super::*;
    use crate::scan_results::ConnectionIndex;
    use std::collections::BTreeMap;

    /// Build the synthetic kConnections used by the merge-path test helpers:
    /// kConnections[destination][i] = [{ stopTimes: [{ stop: origin }] }, 0, 1].
    fn merge_path_connections(path: &[&str]) -> ConnectionIndex {
        let mut k: ConnectionIndex = IndexMap::new();
        for i in 1..path.len() {
            let origin = path[i - 1];
            let destination = path[i];
            // synthetic trip with a single stop time carrying just the origin stop
            let trip = Rc::new(Trip {
                trip_id: String::new(),
                stop_times: vec![StopTime {
                    stop: origin.to_string(),
                    arrival_time: 0,
                    departure_time: 0,
                    pick_up: false,
                    drop_off: false,
                }],
                service_id: "1".into(),
                service: service1(),
            });
            let mut rounds: BTreeMap<usize, Connection> = BTreeMap::new();
            rounds.insert(i, Connection::Trip(trip, 0, 1));
            k.insert(destination.to_string(), rounds);
        }
        k
    }

    fn graph_merge_path(path: &[&str], tree: &GraphResults) {
        tree.add(&merge_path_connections(path));
    }

    fn string_merge_path(path: &[&str], tree: &mut StringResults) {
        tree.add(&merge_path_connections(path));
    }

    // --- GraphResults ---

    fn node_chain(labels: &[&str]) -> Rc<TreeNode> {
        // labels[0] is the leaf; parent chain follows.
        let mut parent: Option<Rc<TreeNode>> = None;
        for label in labels.iter().rev() {
            parent = Some(Rc::new(TreeNode {
                label: label.to_string(),
                parent,
            }));
        }
        parent.unwrap()
    }

    fn assert_node_eq(a: &TreeNode, b: &TreeNode) {
        let mut x = Some(a);
        let mut y = Some(b);
        while let (Some(xn), Some(yn)) = (x, y) {
            assert_eq!(xn.label, yn.label);
            x = xn.parent.as_deref();
            y = yn.parent.as_deref();
        }
        assert!(x.is_none() && y.is_none(), "parent chains differ in length");
    }

    #[test]
    fn merges_a_path_into_an_empty_tree() {
        let tree = GraphResults::new();
        graph_merge_path(&["A", "B", "C"], &tree);
        let result = tree.finalize();

        // A:[A], B:[B<-A], C:[C<-B<-A]
        assert_eq!(result.keys().cloned().collect::<Vec<_>>(), vec!["A", "B", "C"]);
        assert_node_eq(&result["A"][0], &node_chain(&["A"]));
        assert_node_eq(&result["B"][0], &node_chain(&["B", "A"]));
        assert_node_eq(&result["C"][0], &node_chain(&["C", "B", "A"]));
        assert_eq!(result["A"].len(), 1);
        assert_eq!(result["B"].len(), 1);
        assert_eq!(result["C"].len(), 1);
    }

    #[test]
    fn graph_merges_duplicate_paths() {
        let tree = GraphResults::new();
        graph_merge_path(&["A", "B", "C"], &tree);
        graph_merge_path(&["A", "B"], &tree);
        let result = tree.finalize();

        assert_eq!(result["A"].len(), 1);
        assert_eq!(result["B"].len(), 1);
        assert_eq!(result["C"].len(), 1);
        assert_node_eq(&result["C"][0], &node_chain(&["C", "B", "A"]));
    }

    #[test]
    fn appends_to_existing_paths() {
        let tree = GraphResults::new();
        graph_merge_path(&["A", "B"], &tree);
        graph_merge_path(&["A", "B", "C"], &tree);
        let result = tree.finalize();

        assert_eq!(result["A"].len(), 1);
        assert_eq!(result["B"].len(), 1);
        assert_eq!(result["C"].len(), 1);
        assert_node_eq(&result["C"][0], &node_chain(&["C", "B", "A"]));
    }

    #[test]
    fn appends_different_paths() {
        let tree = GraphResults::new();
        graph_merge_path(&["A", "B", "C", "D"], &tree);
        graph_merge_path(&["A", "B", "D"], &tree);
        let result = tree.finalize();

        assert_eq!(result["D"].len(), 2);
        assert_node_eq(&result["D"][0], &node_chain(&["D", "C", "B", "A"]));
        assert_node_eq(&result["D"][1], &node_chain(&["D", "B", "A"]));
        assert_eq!(result["A"].len(), 1);
        assert_eq!(result["B"].len(), 1);
        assert_eq!(result["C"].len(), 1);
    }

    // --- StringResults ---

    // Sets are compared by membership (vitest `toEqual` ignores Set insertion order).
    fn set_of(items: &[&str]) -> std::collections::BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn get_set(
        tree: &crate::transfer_pattern::TransferPatternIndex,
        key: &str,
    ) -> std::collections::BTreeSet<String> {
        tree.get(key)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    #[test]
    fn string_merges_duplicate_paths() {
        let mut tree = StringResults::new(IndexMap::new());
        string_merge_path(&["A", "B", "C", "D"], &mut tree);
        string_merge_path(&["A", "B", "C"], &mut tree);
        let result = tree.finalize();

        assert_eq!(result.len(), 3);
        assert_eq!(get_set(&result, "AB"), set_of(&[""]));
        assert_eq!(get_set(&result, "AC"), set_of(&["B"]));
        assert_eq!(get_set(&result, "AD"), set_of(&["B,C"]));
    }

    #[test]
    fn string_orders_results() {
        let mut tree = StringResults::new(IndexMap::new());
        string_merge_path(&["C", "B", "A"], &mut tree);
        string_merge_path(&["C", "D", "B"], &mut tree);
        string_merge_path(&["C", "B", "E"], &mut tree);
        let result = tree.finalize();

        assert_eq!(get_set(&result, "AC"), set_of(&["B"]));
        assert_eq!(get_set(&result, "BC"), set_of(&["", "D"]));
        assert_eq!(get_set(&result, "CE"), set_of(&["B"]));
        assert_eq!(get_set(&result, "CD"), set_of(&[""]));
    }

    #[test]
    fn string_adds_different_paths() {
        let mut tree = StringResults::new(IndexMap::new());
        string_merge_path(&["A", "B", "C", "D"], &mut tree);
        string_merge_path(&["A", "C", "B", "D"], &mut tree);
        let result = tree.finalize();

        assert_eq!(get_set(&result, "AC"), set_of(&["", "B"]));
        assert_eq!(get_set(&result, "AB"), set_of(&["", "C"]));
        assert_eq!(get_set(&result, "AD"), set_of(&["B,C", "C,B"]));
    }
}
