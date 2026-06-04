//! Journey filters (port of src/results/filter/{JourneyFilter,MultipleCriteriaFilter}.ts).

use crate::journey::Journey;

/// Filter a number of journeys.
pub trait JourneyFilter {
    fn apply(&self, journeys: Vec<Journey>) -> Vec<Journey>;
}

/// b arrives before or at the same time as a.
fn earliest_arrival(a: &Journey, b: &Journey) -> bool {
    b.arrival_time <= a.arrival_time
}

/// b has the same or fewer changes than a.
fn least_changes(a: &Journey, b: &Journey) -> bool {
    b.legs.len() <= a.legs.len()
}

type FilterCriteria = fn(&Journey, &Journey) -> bool;

pub struct MultipleCriteriaFilter {
    criteria: Vec<FilterCriteria>,
}

impl Default for MultipleCriteriaFilter {
    fn default() -> Self {
        MultipleCriteriaFilter {
            criteria: vec![earliest_arrival, least_changes],
        }
    }
}

impl MultipleCriteriaFilter {
    pub fn new() -> Self {
        Self::default()
    }
}

impl JourneyFilter for MultipleCriteriaFilter {
    fn apply(&self, mut journeys: Vec<Journey>) -> Vec<Journey> {
        // Sort by departure asc; tie-break by arrival descending. Stable sort.
        journeys.sort_by(|a, b| {
            if a.departure_time != b.departure_time {
                a.departure_time.cmp(&b.departure_time)
            } else {
                b.arrival_time.cmp(&a.arrival_time)
            }
        });

        // Keep journey A unless some LATER journey B satisfies ALL criteria.
        let mut out = Vec::new();
        for i in 0..journeys.len() {
            let a = &journeys[i];
            let dominated = journeys[i + 1..]
                .iter()
                .any(|b| self.criteria.iter().all(|c| c(a, b)));
            if !dominated {
                out.push(journeys[i].clone());
            }
        }

        out
    }
}
