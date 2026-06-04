"""GroupStationDepartAfterQuery (mirrors src/query/GroupStationDepartAfterQuery.ts).

Multi-day stitching: scan each day, build journeys, advancing the date by one
real calendar day until results are found or maxSearchDays is reached.
"""
from __future__ import annotations

from typing import List

from .algorithm import RaptorAlgorithm, StopTimes
from .date_util import Date, getDateNumber
from .gtfs import MAX_SAFE_INTEGER, StopID
from .journey import Journey
from .journey_factory import JourneyFactory
from .scan_results import Arrivals, ConnectionIndex


class GroupStationDepartAfterQuery:
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

    def plan(
        self, origins: List[StopID], destinations: List[StopID], date: Date, time: int
    ) -> List[Journey]:
        origin_times = {origin: time for origin in origins}

        results = self._get_journeys(origin_times, destinations, date.clone())

        for f in self.filters:
            results = f.apply(results)

        return results

    def _get_journeys(
        self, origins: StopTimes, destinations: List[StopID], start_date: Date
    ) -> List[Journey]:
        connection_indexes: List[ConnectionIndex] = []

        for _ in range(self.maxSearchDays):
            date = getDateNumber(start_date)
            day_of_week = start_date.getDay()
            k_connections, best_arrivals = self.raptor.scan(origins, date, day_of_week)
            results = self._get_journeys_from_connections(
                k_connections, connection_indexes, destinations
            )

            if len(results) > 0:
                return results

            origins = self._get_found_stations(k_connections, best_arrivals)
            start_date.add_days(1)
            connection_indexes.append(k_connections)

        return []

    def _get_found_stations(
        self, k_connections: ConnectionIndex, best_arrivals: Arrivals
    ) -> StopTimes:
        all_stops = list(k_connections)
        stops_with_arrival = [d for d in all_stops if len(k_connections[d]) > 0]

        return {s: max(1, best_arrivals[s] - 86400) for s in stops_with_arrival}

    def _get_journeys_from_connections(
        self,
        k_connections: ConnectionIndex,
        prev_connections: List[ConnectionIndex],
        destinations: List[StopID],
    ) -> List[Journey]:
        destinations_with_results = [d for d in destinations if len(k_connections.get(d, {})) > 0]

        initial_results: List[Journey] = []
        for d in destinations_with_results:
            initial_results.extend(self.resultsFactory.getResults(k_connections, d))

        # reverse the previous connections (in place, matching JS Array.reverse
        # which mutates) and work back through each day prepending journeys
        prev_connections.reverse()
        results = initial_results
        for connections in prev_connections:
            results = self._complete_journeys(results, connections)

        return results

    def _complete_journeys(
        self, results: List[Journey], k_connections: ConnectionIndex
    ) -> List[Journey]:
        out: List[Journey] = []
        for journey_b in results:
            for journey_a in self.resultsFactory.getResults(k_connections, journey_b.legs[0].origin):
                out.append(self._merge_journeys(journey_a, journey_b))

        return out

    def _merge_journeys(self, journey_a: Journey, journey_b: Journey) -> Journey:
        return Journey(
            legs=journey_a.legs + journey_b.legs,
            departureTime=journey_a.departureTime,
            arrivalTime=journey_b.arrivalTime + 86400,
        )
