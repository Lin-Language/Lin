//! Query layer: GroupStationDepartAfterQuery, DepartAfterQuery, RangeQuery
//! (port of src/query/{GroupStationDepartAfterQuery,DepartAfterQuery,RangeQuery}.ts).

use indexmap::IndexMap;

use crate::date_util::UtcDate;
use crate::filter::JourneyFilter;
use crate::gtfs::{StopId, Time, MAX_SAFE_INTEGER};
use crate::journey::Journey;
use crate::journey_factory::ResultsFactory;
use crate::raptor::{RaptorAlgorithm, StopTimes};
use crate::scan_results::{Arrivals, ConnectionIndex};

pub struct GroupStationDepartAfterQuery<'a, R: ResultsFactory> {
    raptor: &'a RaptorAlgorithm,
    results_factory: &'a R,
    max_search_days: usize,
    filters: Vec<&'a dyn JourneyFilter>,
}

impl<'a, R: ResultsFactory> GroupStationDepartAfterQuery<'a, R> {
    pub fn new(
        raptor: &'a RaptorAlgorithm,
        results_factory: &'a R,
        max_search_days: usize,
        filters: Vec<&'a dyn JourneyFilter>,
    ) -> Self {
        GroupStationDepartAfterQuery {
            raptor,
            results_factory,
            max_search_days,
            filters,
        }
    }

    pub fn plan(
        &self,
        origins: &[StopId],
        destinations: &[StopId],
        date: UtcDate,
        time: Time,
    ) -> Vec<Journey> {
        let mut origin_times: StopTimes = IndexMap::new();
        for origin in origins {
            origin_times.insert(origin.clone(), time);
        }

        let results = self.get_journeys(origin_times, destinations, date);

        self.filters
            .iter()
            .fold(results, |rs, filter| filter.apply(rs))
    }

    fn get_journeys(
        &self,
        mut origins: StopTimes,
        destinations: &[StopId],
        start_date: UtcDate,
    ) -> Vec<Journey> {
        let mut connection_indexes: Vec<ConnectionIndex> = Vec::new();
        let mut start_date = start_date;

        for _ in 0..self.max_search_days {
            let date = start_date.date_number();
            let dow = start_date.day_of_week();
            let (k_connections, best_arrivals) = self.raptor.scan(&origins, date, dow);
            let results =
                self.get_journeys_from_connections(&k_connections, &connection_indexes, destinations);

            if !results.is_empty() {
                return results;
            }

            origins = Self::get_found_stations(&k_connections, &best_arrivals);
            start_date.add_day();
            connection_indexes.push(k_connections);
        }

        Vec::new()
    }

    fn get_found_stations(k_connections: &ConnectionIndex, best_arrivals: &Arrivals) -> StopTimes {
        let mut out: StopTimes = IndexMap::new();
        for (stop, rounds) in k_connections {
            if !rounds.is_empty() {
                let best = best_arrivals.get(stop).copied().unwrap_or(MAX_SAFE_INTEGER);
                out.insert(stop.clone(), std::cmp::max(1, best - 86400));
            }
        }
        out
    }

    fn get_journeys_from_connections(
        &self,
        k_connections: &ConnectionIndex,
        prev_connections: &[ConnectionIndex],
        destinations: &[StopId],
    ) -> Vec<Journey> {
        let mut initial_results: Vec<Journey> = Vec::new();
        for d in destinations {
            let has_results = k_connections.get(d).is_some_and(|r| !r.is_empty());
            if has_results {
                initial_results.extend(self.results_factory.get_results(k_connections, d));
            }
        }

        // Reverse the previous connections and work back through each day,
        // prepending journeys.
        let mut journeys = initial_results;
        for connections in prev_connections.iter().rev() {
            journeys = self.complete_journeys(journeys, connections);
        }

        journeys
    }

    fn complete_journeys(
        &self,
        results: Vec<Journey>,
        k_connections: &ConnectionIndex,
    ) -> Vec<Journey> {
        let mut out = Vec::new();
        for journey_b in &results {
            let origin = journey_b.legs[0].origin().to_string();
            for journey_a in self.results_factory.get_results(k_connections, &origin) {
                out.push(Self::merge_journeys(&journey_a, journey_b));
            }
        }
        out
    }

    fn merge_journeys(journey_a: &Journey, journey_b: &Journey) -> Journey {
        let mut legs = journey_a.legs.clone();
        legs.extend(journey_b.legs.iter().cloned());
        Journey {
            legs,
            departure_time: journey_a.departure_time,
            arrival_time: journey_b.arrival_time + 86400,
        }
    }
}

pub struct DepartAfterQuery<'a, R: ResultsFactory> {
    raptor: &'a RaptorAlgorithm,
    results_factory: &'a R,
    max_search_days: usize,
}

impl<'a, R: ResultsFactory> DepartAfterQuery<'a, R> {
    pub fn new(raptor: &'a RaptorAlgorithm, results_factory: &'a R, max_search_days: usize) -> Self {
        DepartAfterQuery {
            raptor,
            results_factory,
            max_search_days,
        }
    }

    /// No filters are applied (matches the reference DepartAfterQuery).
    pub fn plan(
        &self,
        origin: &str,
        destination: &str,
        date: UtcDate,
        time: Time,
    ) -> Vec<Journey> {
        let group = GroupStationDepartAfterQuery::new(
            self.raptor,
            self.results_factory,
            self.max_search_days,
            Vec::new(),
        );
        group.plan(&[origin.to_string()], &[destination.to_string()], date, time)
    }
}

pub struct RangeQuery<'a, R: ResultsFactory> {
    raptor: &'a RaptorAlgorithm,
    results_factory: &'a R,
    max_search_days: usize,
    filters: Vec<&'a dyn JourneyFilter>,
}

impl<'a, R: ResultsFactory> RangeQuery<'a, R> {
    pub fn new(
        raptor: &'a RaptorAlgorithm,
        results_factory: &'a R,
        max_search_days: usize,
        filters: Vec<&'a dyn JourneyFilter>,
    ) -> Self {
        RangeQuery {
            raptor,
            results_factory,
            max_search_days,
            filters,
        }
    }

    pub fn plan(
        &self,
        origin: &str,
        destination: &str,
        date: UtcDate,
        time: Time,
        end_time: Time,
    ) -> Vec<Journey> {
        let mut results: Vec<Journey> = Vec::new();
        let mut time = time;

        while time < end_time {
            let group = GroupStationDepartAfterQuery::new(
                self.raptor,
                self.results_factory,
                self.max_search_days,
                Vec::new(),
            );
            let new_results = group.plan(
                &[origin.to_string()],
                &[destination.to_string()],
                date,
                time,
            );

            if new_results.is_empty() {
                break;
            }

            let min_dep = new_results
                .iter()
                .map(|j| j.departure_time)
                .min()
                .unwrap();
            results.extend(new_results);
            time = min_dep + 1;
        }

        self.filters
            .iter()
            .fold(results, |rs, filter| filter.apply(rs))
    }
}
