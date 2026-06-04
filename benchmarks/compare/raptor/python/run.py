#!/usr/bin/env python3
"""CLI runner for the Python RAPTOR port.

Usage:
    python3 run.py [dataDir] [origin] [destination] [YYYY-MM-DD] [HH:MM]

Loads the GTFS feed, builds the RAPTOR algorithm (no date pre-filter — the date
is passed through the query), plans a journey, and prints results in the fixed,
language-independent contract format to stdout. Timing goes to stderr only.
"""
from __future__ import annotations

import os
import sys
import time as _time

from raptor.date_util import Date
from raptor.depart_after_query import DepartAfterQuery
from raptor.gtfs import TimetableLeg
from raptor.gtfs_loader import load_gtfs
from raptor.journey_factory import JourneyFactory
from raptor.algorithm import RaptorAlgorithmFactory


def fmt(s: int) -> str:
    """Format seconds-from-midnight as HH:MM:SS (HH may exceed 24)."""
    hh = s // 3600
    mm = (s % 3600) // 60
    ss = s % 60
    return f"{hh:02d}:{mm:02d}:{ss:02d}"


def is_timetable_leg(leg) -> bool:
    return isinstance(leg, TimetableLeg)


def main() -> None:
    argv = sys.argv[1:]
    here = os.path.dirname(os.path.abspath(__file__))
    data_dir = argv[0] if len(argv) > 0 else os.path.join(here, "..", "data")
    origin = argv[1] if len(argv) > 1 else "TBW"
    destination = argv[2] if len(argv) > 2 else "NRW"
    date_str = argv[3] if len(argv) > 3 else "2025-09-02"
    time_str = argv[4] if len(argv) > 4 else "08:00"

    # HH:MM (or HH:MM:SS) -> seconds from midnight
    parts = [int(p) for p in time_str.split(":")]
    h = parts[0] if len(parts) > 0 else 0
    m = parts[1] if len(parts) > 1 else 0
    s = parts[2] if len(parts) > 2 else 0
    time_seconds = h * 3600 + m * 60 + s

    load_start = _time.perf_counter()
    trips, transfers, interchange = load_gtfs(data_dir)
    load_ms = (_time.perf_counter() - load_start) * 1000.0

    # NO date pre-filter: pass the date through the query (matches DepartAfterQuery).
    raptor = RaptorAlgorithmFactory.create(trips, transfers, interchange)
    query = DepartAfterQuery(raptor, JourneyFactory())

    plan_start = _time.perf_counter()
    journeys = query.plan(origin, destination, Date(date_str), time_seconds)
    plan_ms = (_time.perf_counter() - plan_start) * 1000.0

    sys.stderr.write(f"load={load_ms:.1f}ms plan={plan_ms:.1f}ms\n")

    # Sort by departureTime asc then arrivalTime asc for stable output.
    journeys.sort(key=lambda j: (j.departureTime, j.arrivalTime))

    out = []
    for journey in journeys:
        out.append(
            f"JOURNEY dep={fmt(journey.departureTime)} arr={fmt(journey.arrivalTime)} legs={len(journey.legs)}"
        )
        for leg in journey.legs:
            if is_timetable_leg(leg):
                first = leg.stopTimes[0]
                last = leg.stopTimes[-1]
                out.append(
                    f"  {first.stop} {fmt(first.departureTime)} -> {last.stop} {fmt(last.arrivalTime)}"
                )
            else:
                out.append(f"  TRANSFER {leg.origin} -> {leg.destination} ({leg.duration}s)")

    # RESULT: from the journey with the earliest arrival (ties: fewest legs).
    best = None
    for journey in journeys:
        if (
            best is None
            or journey.arrivalTime < best.arrivalTime
            or (journey.arrivalTime == best.arrivalTime and len(journey.legs) < len(best.legs))
        ):
            best = journey

    if best is not None:
        out.append(
            f"RESULT dep={best.departureTime} arr={best.arrivalTime} legs={len(best.legs)} count={len(journeys)}"
        )
    else:
        out.append("RESULT dep=0 arr=0 legs=0 count=0")

    sys.stdout.write("\n".join(out) + "\n")


if __name__ == "__main__":
    main()
