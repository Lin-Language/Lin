"""RangeQuery (mirrors src/query/RangeQuery.ts).

Profile query: search at midnight, then one minute after the earliest departure
of each set of results, until no more results.
"""
from __future__ import annotations

from typing import List

from .algorithm import RaptorAlgorithm
from .date_util import Date
from .gtfs import StopID
from .group_query import GroupStationDepartAfterQuery
from .journey import Journey
from .journey_factory import JourneyFactory

ONE_DAY = 24 * 60 * 60


class RangeQuery:
    def __init__(
        self,
        raptor: RaptorAlgorithm,
        results_factory: JourneyFactory,
        max_search_days: int = 3,
        filters: List = None,
    ) -> None:
        self.raptor = raptor
        self.resultsFactory = results_factory
        self.maxSearchDays = max_search_days
        self.filters = filters if filters is not None else []
        self.groupQuery = GroupStationDepartAfterQuery(raptor, results_factory, max_search_days)

    def plan(
        self,
        origin: StopID,
        destination: StopID,
        date: Date,
        time: int = 1,
        end_time: int = ONE_DAY,
    ) -> List[Journey]:
        results: List[Journey] = []

        while time < end_time:
            new_results = self.groupQuery.plan([origin], [destination], date, time)

            results.extend(new_results)

            if len(new_results) == 0:
                break

            time = min(j.departureTime for j in new_results) + 1

        for f in self.filters:
            results = f.apply(results)

        return results
