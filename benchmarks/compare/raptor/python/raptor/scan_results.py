"""ScanResults + ScanResultsFactory (mirrors src/raptor/ScanResults.ts).

Connections are either a Transfer or a (Trip, startIndex, endIndex) tuple.
All maps are insertion-ordered dicts (contract #1). getMarkedStops returns the
insertion order of the current round's arrivals.
"""
from __future__ import annotations

from typing import Dict, List, Tuple, Union

from .gtfs import MAX_SAFE_INTEGER, StopID, Time, Transfer, Trip

Connection = Tuple[Trip, int, int]
# A kConnections entry value is keyed by round number k -> Connection | Transfer.
ConnectionIndex = Dict[StopID, Dict[int, Union[Connection, Transfer]]]
Arrivals = Dict[StopID, Time]


class ScanResults:
    def __init__(
        self,
        best_arrivals: Arrivals,
        k_arrivals: Dict[int, Arrivals],
        k_connections: ConnectionIndex,
    ) -> None:
        self.k = 0
        self.bestArrivals = best_arrivals
        self.kArrivals = k_arrivals
        self.kConnections = k_connections

    def addRound(self) -> None:
        self.k += 1
        self.kArrivals[self.k] = {}

    def previousArrival(self, stop_pi: StopID) -> Time:
        # JS returns undefined when absent; callers test truthiness, so use None.
        return self.kArrivals[self.k - 1].get(stop_pi)

    def setTrip(self, trip: Trip, start_index: int, end_index: int, interchange: int) -> None:
        time = trip.stopTimes[end_index].arrivalTime + interchange
        stop_pi = trip.stopTimes[end_index].stop

        self.kArrivals[self.k][stop_pi] = time
        self.bestArrivals[stop_pi] = time
        self.kConnections[stop_pi][self.k] = (trip, start_index, end_index)

    def setTransfer(self, transfer: Transfer, time: Time) -> None:
        stop_pi = transfer.destination

        self.kArrivals[self.k][stop_pi] = time
        self.bestArrivals[stop_pi] = time
        self.kConnections[stop_pi][self.k] = transfer

    def bestArrival(self, stop_pi: StopID) -> Time:
        return self.bestArrivals[stop_pi]

    def getMarkedStops(self) -> List[StopID]:
        return list(self.kArrivals[self.k])

    def finalize(self) -> Tuple[ConnectionIndex, Arrivals]:
        return self.kConnections, self.bestArrivals


class ScanResultsFactory:
    def __init__(self, stops: List[StopID]) -> None:
        self.stops = stops

    def create(self, origins: Dict[StopID, Time]) -> ScanResults:
        best_arrivals = {stop: origins.get(stop) or MAX_SAFE_INTEGER for stop in self.stops}
        k_arrivals = [{stop: origins.get(stop) or MAX_SAFE_INTEGER for stop in self.stops}]
        # kArrivals is keyed by round number; round 0 holds the origins.
        k_arrivals_map = {0: k_arrivals[0]}
        k_connections: ConnectionIndex = {stop: {} for stop in self.stops}

        return ScanResults(best_arrivals, k_arrivals_map, k_connections)
