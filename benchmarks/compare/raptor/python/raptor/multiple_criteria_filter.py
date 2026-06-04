"""MultipleCriteriaFilter (mirrors src/results/filter/MultipleCriteriaFilter.ts).

Sort by departureTime asc, arrivalTime desc (tie-break), then keep a journey
unless some LATER journey dominates it on all criteria.
"""
from __future__ import annotations

from typing import Callable, List

from .journey import Journey

FilterCriteria = Callable[[Journey, Journey], bool]


def earliest_arrival(a: Journey, b: Journey) -> bool:
    return b.arrivalTime <= a.arrivalTime


def least_changes(a: Journey, b: Journey) -> bool:
    return len(b.legs) <= len(a.legs)


class MultipleCriteriaFilter:
    def __init__(self, criteria: List[FilterCriteria] = None) -> None:
        self.criteria = criteria if criteria is not None else [earliest_arrival, least_changes]

    def apply(self, journeys: List[Journey]) -> List[Journey]:
        # Stable sort: dep asc, arr desc.
        journeys = sorted(journeys, key=lambda j: (j.departureTime, -j.arrivalTime))

        return [a for i, a in enumerate(journeys) if self._compare(a, i, journeys)]

    def _compare(self, journey_a: Journey, index: int, journeys: List[Journey]) -> bool:
        for j in range(index + 1, len(journeys)):
            journey_b = journeys[j]
            if all(criteria(journey_a, journey_b) for criteria in self.criteria):
                return False

        return True
