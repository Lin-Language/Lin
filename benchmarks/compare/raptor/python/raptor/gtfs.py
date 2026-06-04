"""GTFS data types (mirrors src/gtfs/GTFS.ts).

StopID is a str, Time/Duration are ints (seconds since midnight, may exceed 24h),
DateNumber is an int like 20181225, DayOfWeek is 0..6 with Sunday=0 (JS getDay).
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import Dict, List, Optional

# Number.MAX_SAFE_INTEGER (2^53 - 1) — the "infinity" arrival sentinel.
MAX_SAFE_INTEGER = 9007199254740991

StopID = str
Time = int
Duration = int
TripID = str
ServiceID = str
DateNumber = int
DayOfWeek = int  # Sunday = 0, Monday = 1, ... Saturday = 6 (JS getDay)


@dataclass
class StopTime:
    stop: StopID
    arrivalTime: Time
    departureTime: Time
    pickUp: bool
    dropOff: bool


@dataclass
class TimetableLeg:
    """A leg with a defined departure and arrival time. trip is excluded from
    equality (per porting contract #8/#9 — setDefaultTrip overwrites it)."""
    stopTimes: List[StopTime]
    origin: StopID
    destination: StopID
    trip: Optional["Trip"] = field(default=None, compare=False)


@dataclass
class Transfer:
    origin: StopID
    destination: StopID
    duration: Duration
    startTime: Time
    endTime: Time


@dataclass
class Trip:
    tripId: TripID
    stopTimes: List[StopTime]
    serviceId: ServiceID
    service: "Service"  # noqa: F821


# A connection is either a Transfer or a [Trip, startIndex, endIndex] tuple.
# Calendar is represented inline by the Service class (see service.py).
