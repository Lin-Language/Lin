"""Journey + leg types (mirrors src/results/Journey.ts).

A Journey has legs (each a TimetableLeg or Transfer), departureTime, arrivalTime.
Equality is structural; TimetableLeg.trip is excluded from equality (contract #9).
"""
from __future__ import annotations

from dataclasses import dataclass, field
from typing import List, Union

from .gtfs import Time, TimetableLeg, Transfer

AnyLeg = Union[Transfer, TimetableLeg]


@dataclass
class Journey:
    legs: List[AnyLeg]
    departureTime: Time
    arrivalTime: Time


def is_transfer(connection) -> bool:
    """True if the connection/leg is a Transfer (has an `origin` attr and a
    `duration`, distinguishing it from a TimetableLeg which has stopTimes)."""
    return isinstance(connection, Transfer)
