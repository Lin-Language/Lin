"""JourneyFactory (mirrors src/results/JourneyFactory.ts).

Extracts Journey[] from the kConnections index for a destination. The per-
destination round map is keyed by integer k and must be iterated in NUMERIC
ASCENDING order (contract #1 exception).
"""
from __future__ import annotations

from typing import List

from .gtfs import StopID, Time, TimetableLeg, Transfer
from .journey import AnyLeg, Journey, is_transfer
from .scan_results import ConnectionIndex


class JourneyFactory:
    def getResults(self, k_connections: ConnectionIndex, destination: StopID) -> List[Journey]:
        results: List[Journey] = []

        dest_map = k_connections.get(destination) or {}
        # Round numbers are integer-like keys -> iterate numeric ascending.
        for k in sorted(dest_map, key=lambda x: int(x)):
            legs = self._get_journey_legs(k_connections, k, destination)
            departure_time = self._get_departure_time(legs)
            arrival_time = self._get_arrival_time(legs)

            results.append(Journey(legs=legs, departureTime=departure_time, arrivalTime=arrival_time))

        return results

    def _get_journey_legs(
        self, k_connections: ConnectionIndex, k, final_destination: StopID
    ) -> List[AnyLeg]:
        legs: List[AnyLeg] = []

        destination = final_destination
        i = int(k)
        while i > 0:
            connection = k_connections[destination][i]

            if is_transfer(connection):
                legs.append(connection)
                destination = connection.origin
            else:
                trip, start, end = connection
                stop_times = trip.stopTimes[start:end + 1]
                origin = stop_times[0].stop

                legs.append(
                    TimetableLeg(
                        stopTimes=stop_times,
                        origin=origin,
                        destination=destination,
                        trip=trip,
                    )
                )
                destination = origin

            i -= 1

        legs.reverse()
        return legs

    def _get_departure_time(self, legs: List[AnyLeg]) -> Time:
        transfer_duration = 0

        for leg in legs:
            if not self._is_timetable_leg(leg):
                transfer_duration += leg.duration
            else:
                return leg.stopTimes[0].departureTime - transfer_duration

        return 0

    def _get_arrival_time(self, legs: List[AnyLeg]) -> Time:
        transfer_duration = 0

        for leg in reversed(legs):
            if not self._is_timetable_leg(leg):
                transfer_duration += leg.duration
            else:
                return leg.stopTimes[-1].arrivalTime + transfer_duration

        return 0

    def _is_timetable_leg(self, connection: AnyLeg) -> bool:
        return isinstance(connection, TimetableLeg)
