"""Test fixture helpers ported from test/unit/util.ts.

t/st/tf/j/set_default_trip plus journey equality semantics. Note: TimetableLeg
excludes `trip` from equality (gtfs.py), so set_default_trip is faithfully
implemented but structurally redundant for the assertions.
"""
from __future__ import annotations

from typing import Dict, List, Union

from raptor.gtfs import MAX_SAFE_INTEGER, StopID, StopTime, Time, TimetableLeg, Transfer, Trip
from raptor.journey import Journey
from raptor.service import Service

all_days: Dict[int, bool] = {0: True, 1: True, 2: True, 3: True, 4: True, 5: True, 6: True}

services: Dict[str, Service] = {
    "1": Service(20180101, 20991231, all_days, {}),
    "2": Service(20190101, 20991231, all_days, {}),
}

_trip_id = 0


def t(*stop_times: StopTime) -> Trip:
    global _trip_id
    trip = Trip(
        tripId=f"trip{_trip_id}",
        stopTimes=list(stop_times),
        serviceId="1",
        service=services["1"],
    )
    _trip_id += 1
    return trip


def st(stop: StopID, arrival_time, departure_time) -> StopTime:
    return StopTime(
        stop=stop,
        arrivalTime=arrival_time if arrival_time is not None else departure_time,
        departureTime=departure_time if departure_time is not None else arrival_time,
        dropOff=arrival_time is not None,
        pickUp=departure_time is not None,
    )


_default_trip = Trip(tripId="1", serviceId="1", stopTimes=[], service=services["1"])


def _is_transfer(leg) -> bool:
    return isinstance(leg, Transfer)


def _get_departure_time(legs: List[Union[List[StopTime], Transfer]]) -> Time:
    transfer_duration = 0
    for leg in legs:
        if _is_transfer(leg):
            transfer_duration += leg.duration
        else:
            return leg[0].departureTime - transfer_duration
    return 0


def _get_arrival_time(legs: List[Union[List[StopTime], Transfer]]) -> Time:
    transfer_duration = 0
    for leg in reversed(legs):
        if _is_transfer(leg):
            transfer_duration += leg.duration
        else:
            return leg[-1].arrivalTime + transfer_duration
    return 0


def j(*leg_stop_times: Union[List[StopTime], Transfer]) -> Journey:
    legs = []
    for stop_times in leg_stop_times:
        if _is_transfer(stop_times):
            legs.append(stop_times)
        else:
            legs.append(
                TimetableLeg(
                    stopTimes=stop_times,
                    origin=stop_times[0].stop,
                    destination=stop_times[-1].stop,
                    trip=_default_trip,
                )
            )

    return Journey(
        legs=legs,
        departureTime=_get_departure_time(list(leg_stop_times)),
        arrivalTime=_get_arrival_time(list(leg_stop_times)),
    )


def tf(origin: StopID, destination: StopID, duration: Time) -> Transfer:
    return Transfer(
        origin=origin,
        destination=destination,
        duration=duration,
        startTime=0,
        endTime=MAX_SAFE_INTEGER,
    )


def set_default_trip(results: List[Journey]) -> None:
    for trip in results:
        for leg in trip.legs:
            if isinstance(leg, TimetableLeg) and leg.trip is not None:
                leg.trip = _default_trip
