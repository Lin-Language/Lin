"""RouteScanner + RouteScannerFactory (mirrors src/raptor/RouteScanner.ts).

Backward scan with a stateful routeScanPosition memo, per scan() call.
"""
from __future__ import annotations

from typing import Dict, List, Optional

from .gtfs import DayOfWeek, Time, Trip

RouteID = str
TripsIndexedByRoute = Dict[RouteID, List[Trip]]


class RouteScanner:
    def __init__(self, trips_by_route: TripsIndexedByRoute, date: int, dow: DayOfWeek) -> None:
        self.tripsByRoute = trips_by_route
        self.date = date
        self.dow = dow
        self.routeScanPosition: Dict[RouteID, int] = {}

    def getTrip(self, route_id: RouteID, stop_index: int, time: Time) -> Optional[Trip]:
        if route_id not in self.routeScanPosition:
            self.routeScanPosition[route_id] = len(self.tripsByRoute[route_id]) - 1

        last_found: Optional[Trip] = None
        route_trips = self.tripsByRoute[route_id]

        # iterate backwards from where we last found a trip
        i = self.routeScanPosition[route_id]
        while i >= 0:
            trip = route_trips[i]
            stop_time = trip.stopTimes[stop_index]

            if stop_time.departureTime < time:
                break
            elif trip.service.runsOn(self.date, self.dow):
                last_found = trip

            if last_found is None or last_found is trip:
                self.routeScanPosition[route_id] = i

            i -= 1

        return last_found


class RouteScannerFactory:
    def __init__(self, trips_by_route: TripsIndexedByRoute) -> None:
        self.tripsByRoute = trips_by_route

    def create(self, date: int, dow: DayOfWeek) -> RouteScanner:
        return RouteScanner(self.tripsByRoute, date, dow)
