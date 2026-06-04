"""DepartAfterQuery (mirrors src/query/DepartAfterQuery.ts).

Single origin/destination wrapper around GroupStationDepartAfterQuery. No
filters applied.
"""
from __future__ import annotations

from typing import List

from .algorithm import RaptorAlgorithm
from .date_util import Date
from .gtfs import StopID, Time
from .group_query import GroupStationDepartAfterQuery
from .journey import Journey
from .journey_factory import JourneyFactory


class DepartAfterQuery:
    def __init__(
        self,
        raptor: RaptorAlgorithm,
        results_factory: JourneyFactory,
        max_search_days: int = 3,
    ) -> None:
        self.raptor = raptor
        self.resultsFactory = results_factory
        self.maxSearchDays = max_search_days
        self.groupQuery = GroupStationDepartAfterQuery(raptor, results_factory, max_search_days)

    def plan(self, origin: StopID, destination: StopID, date: Date, time: Time) -> List[Journey]:
        return self.groupQuery.plan([origin], [destination], date, time)
