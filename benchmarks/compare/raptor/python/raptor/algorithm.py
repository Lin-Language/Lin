"""RaptorAlgorithm + RaptorAlgorithmFactory (mirrors src/raptor/RaptorAlgorithm.ts
and RaptorAlgorithmFactory.ts).
"""
from __future__ import annotations

from typing import Dict, List, Optional, Tuple

from .date_util import Date, getDateNumber
from .gtfs import StopID, Time, Transfer, Trip
from .queue_factory import QueueFactory, RouteID
from .route_scanner import RouteScanner, RouteScannerFactory
from .scan_results import Arrivals, ConnectionIndex, ScanResults, ScanResultsFactory

RouteStopIndex = Dict[RouteID, Dict[StopID, int]]
RoutePaths = Dict[RouteID, List[StopID]]
Interchange = Dict[StopID, Time]
TransfersByOrigin = Dict[StopID, List[Transfer]]
StopTimes = Dict[StopID, Time]


class RaptorAlgorithm:
    def __init__(
        self,
        route_stop_index: RouteStopIndex,
        route_path: RoutePaths,
        transfers: TransfersByOrigin,
        interchange: Interchange,
        scan_results_factory: ScanResultsFactory,
        queue_factory: QueueFactory,
        route_scanner_factory: RouteScannerFactory,
    ) -> None:
        self.routeStopIndex = route_stop_index
        self.routePath = route_path
        self.transfers = transfers
        self.interchange = interchange
        self.scanResultsFactory = scan_results_factory
        self.queueFactory = queue_factory
        self.routeScannerFactory = route_scanner_factory

    def scan(
        self, origins: StopTimes, date: int, dow: int
    ) -> Tuple[ConnectionIndex, Arrivals]:
        route_scanner = self.routeScannerFactory.create(date, dow)
        results = self.scanResultsFactory.create(origins)
        marked_stops = list(origins)

        while len(marked_stops) > 0:
            results.addRound()

            self._scan_routes(results, route_scanner, marked_stops)
            self._scan_transfers(results, marked_stops)

            marked_stops = results.getMarkedStops()

        return results.finalize()

    def _scan_routes(
        self, results: ScanResults, route_scanner: RouteScanner, marked_stops: List[StopID]
    ) -> None:
        queue = self.queueFactory.getQueue(marked_stops)

        for route_id, stop_p in queue.items():
            boarding_point = -1
            trip: Optional[Trip] = None
            route_path = self.routePath[route_id]
            route_path_length = len(route_path)

            pi = self.routeStopIndex[route_id][stop_p]
            while pi < route_path_length:
                stop_pi = route_path[pi]
                previous_arrival = results.previousArrival(stop_pi)

                if trip is not None:
                    i = self.interchange[stop_pi]
                    stop_time = trip.stopTimes[pi]

                    if stop_time.dropOff and stop_time.arrivalTime + i < results.bestArrival(stop_pi):
                        results.setTrip(trip, boarding_point, pi, i)
                    elif previous_arrival and previous_arrival < stop_time.arrivalTime + i:
                        new_trip = route_scanner.getTrip(route_id, pi, previous_arrival)
                        if new_trip is not None:
                            trip = new_trip
                            boarding_point = pi
                elif previous_arrival:
                    new_trip = route_scanner.getTrip(route_id, pi, previous_arrival)
                    if new_trip is not None:
                        trip = new_trip
                        boarding_point = pi

                pi += 1

    def _scan_transfers(self, results: ScanResults, marked_stops: List[StopID]) -> None:
        for stop_p in marked_stops:
            for transfer in self.transfers.get(stop_p, []):
                stop_pi = transfer.destination

                # JS reads interchange[stopPi]/bestArrival(stopPi) directly; for a
                # transfer destination not on any route path both are `undefined`,
                # so `arrival` becomes NaN and every comparison below is false (the
                # transfer is silently skipped). Mirror that here: a missing key
                # means the transfer is skipped rather than a KeyError.
                if stop_pi not in self.interchange or stop_pi not in results.bestArrivals:
                    continue

                arrival = results.previousArrival(stop_p) + transfer.duration + self.interchange[stop_pi]

                if transfer.startTime <= arrival and transfer.endTime >= arrival and arrival < results.bestArrival(stop_pi):
                    results.setTransfer(transfer, arrival)


class RaptorAlgorithmFactory:
    DEFAULT_INTERCHANGE_TIME = 0
    OVERTAKING_ROUTE_SUFFIX = "overtakes"

    @staticmethod
    def create(
        trips: List[Trip],
        transfers: TransfersByOrigin,
        interchange: Interchange,
        date: Optional[Date] = None,
    ) -> RaptorAlgorithm:
        routes_at_stop: Dict[StopID, List[RouteID]] = {}
        trips_by_route: Dict[RouteID, List[Trip]] = {}
        route_stop_index: RouteStopIndex = {}
        route_path: RoutePaths = {}
        useful_transfers: TransfersByOrigin = {}

        if date is not None:
            date_number = getDateNumber(date)
            dow = date.getDay()
            trips = [t for t in trips if t.service.runsOn(date_number, dow)]

        # Stable sort by first departureTime (Python sorted/list.sort is stable).
        trips = sorted(trips, key=lambda t: t.stopTimes[0].departureTime)

        for trip in trips:
            path = [s.stop for s in trip.stopTimes]
            route_id = RaptorAlgorithmFactory._get_route_id(trip, trips_by_route)

            if route_id not in route_stop_index:
                trips_by_route[route_id] = []
                route_stop_index[route_id] = {}
                route_path[route_id] = path

                for i in range(len(path) - 1, -1, -1):
                    route_stop_index[route_id][path[i]] = i
                    useful_transfers[path[i]] = transfers.get(path[i], [])
                    if path[i] not in interchange:
                        interchange[path[i]] = RaptorAlgorithmFactory.DEFAULT_INTERCHANGE_TIME
                    if path[i] not in routes_at_stop:
                        routes_at_stop[path[i]] = []

                    if trip.stopTimes[i].pickUp:
                        routes_at_stop[path[i]].append(route_id)

            trips_by_route[route_id].append(trip)

        return RaptorAlgorithm(
            route_stop_index,
            route_path,
            useful_transfers,
            interchange,
            ScanResultsFactory(list(useful_transfers)),
            QueueFactory(routes_at_stop, route_stop_index),
            RouteScannerFactory(trips_by_route),
        )

    @staticmethod
    def _get_route_id(trip: Trip, trips_by_route: Dict[RouteID, List[Trip]]) -> RouteID:
        route_id = ",".join(
            s.stop + ("1" if s.pickUp else "0") + ("1" if s.dropOff else "0")
            for s in trip.stopTimes
        )

        for t in trips_by_route.get(route_id, []):
            arrival_time_a = trip.stopTimes[-1].arrivalTime
            arrival_time_b = t.stopTimes[-1].arrivalTime

            if arrival_time_a < arrival_time_b:
                return route_id + RaptorAlgorithmFactory.OVERTAKING_ROUTE_SUFFIX

        return route_id
