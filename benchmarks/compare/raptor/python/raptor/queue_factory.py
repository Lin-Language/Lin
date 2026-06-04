"""QueueFactory (mirrors src/raptor/QueueFactory.ts).

Builds a route->stop queue (insertion-ordered dict) from marked stops.
"""
from __future__ import annotations

from typing import Dict, List

from .gtfs import StopID

RouteID = str


class QueueFactory:
    def __init__(
        self,
        routes_at_stop: Dict[StopID, List[RouteID]],
        route_stop_index: Dict[RouteID, Dict[StopID, int]],
    ) -> None:
        self.routesAtStop = routes_at_stop
        self.routeStopIndex = route_stop_index

    def getQueue(self, marked_stops: List[StopID]) -> Dict[RouteID, StopID]:
        queue: Dict[RouteID, StopID] = {}

        for stop in marked_stops:
            for route_id in self.routesAtStop.get(stop, []):
                existing = queue.get(route_id)
                if existing is not None and self._is_stop_before(route_id, existing, stop):
                    queue[route_id] = existing
                else:
                    queue[route_id] = stop

        return queue

    def _is_stop_before(self, route_id: RouteID, stop_a: StopID, stop_b: StopID) -> bool:
        return self.routeStopIndex[route_id][stop_a] < self.routeStopIndex[route_id][stop_b]
