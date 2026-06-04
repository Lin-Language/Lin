#!/usr/bin/env python3
"""Cross-language RAPTOR benchmark — Python port.

Mirrors node/bench.js exactly. Two workloads over the full GTFS feed:

  GROUP  — the 24 group-station origin/destination-set queries, planned at
           10:00 (36000s) on 2025-09-02 with the default MultipleCriteriaFilter
           (earliestArrival + leastChanges).
  RANGE  — "next 20 journeys departing after 08:00" profile queries for 5 pairs:
           repeatedly plan, advance past the earliest departure, until 20
           journeys are collected or the service runs out.

Output (stdout): per-phase timing lines and a final DIGEST line (the
cross-language correctness gate). The digest is order-independent (a sum mod a
prime), so journey ordering never affects it.
"""
from __future__ import annotations

import os
import sys
import time as _time

from raptor.date_util import Date
from raptor.gtfs_loader import load_gtfs
from raptor.group_query import GroupStationDepartAfterQuery
from raptor.journey_factory import JourneyFactory
from raptor.multiple_criteria_filter import MultipleCriteriaFilter
from raptor.algorithm import RaptorAlgorithmFactory

DATE = "2025-09-02"  # Tuesday, in service window
GROUP_TIME = 36000   # 10:00, matching performance.ts
RANGE_START = 28800  # 08:00
RANGE_N = 20         # "next 20 journeys"
P = 1000000007       # digest modulus

# The 24 reference group-station queries (test/performance.ts).
GROUP_QUERIES = [
    [["MRF", "LVC", "LVJ", "LIV"], ["NRW"]],
    [["TBW", "PDW"], ["HGS"]],
    [["PDW", "MRN"], ["LVC", "LVJ", "LIV"]],
    [["PDW", "AFK"], ["NRW"]],
    [["PDW"], ["BHM", "BMO", "BSW", "BHI"]],
    [["PNZ"], ["DIS"]],
    [["YRK"], ["DIS"]],
    [["WEY"], ["RDG"]],
    [["YRK"], ["NRW"]],
    [["BHM", "BMO", "BSW", "BHI"], ["MCO", "MAN", "MCV", "EXD"]],
    [["BHM", "BMO", "BSW", "BHI"], ["EDB"]],
    [["COV", "RUG"], ["MAN", "MCV"]],
    [["YRK"], ["MCO", "MAN", "MCV", "EXD"]],
    [["STA"], ["PBO"]],
    [["PNZ"], ["EDB"]],
    [["RDG"], ["IPS"]],
    [["DVP"], ["BHM", "BMO", "BSW", "BHI"]],
    [["BXB"], ["DVP"]],
    [["MCO", "MAN", "MCV", "EXD"], ["CBW", "CBE"]],
    [["MCO", "MAN", "MCV", "EXD"], ["EUS", "MYB", "STP", "PAD", "BFR", "CTK", "CST", "CHX", "LBG", "WAE", "VIC", "VXH", "WAT", "OLD", "MOG", "KGX", "LST", "FST"]],
    [["BHM", "BMO", "BSW", "BHI"], ["EUS", "MYB", "STP", "PAD", "BFR", "CTK", "CST", "CHX", "LBG", "WAE", "VIC", "VXH", "WAT", "OLD", "MOG", "KGX", "LST", "FST"]],
    [["ORP"], ["EUS", "MYB", "STP", "PAD", "BFR", "CTK", "CST", "CHX", "LBG", "WAE", "VIC", "VXH", "WAT", "OLD", "MOG", "KGX", "LST", "FST"]],
    [["EDB"], ["EUS", "MYB", "STP", "PAD", "BFR", "CTK", "CST", "CHX", "LBG", "WAE", "VIC", "VXH", "WAT", "OLD", "MOG", "KGX", "LST", "FST"]],
    [["CBE", "CBW"], ["EUS", "MYB", "STP", "PAD", "BFR", "CTK", "CST", "CHX", "LBG", "WAE", "VIC", "VXH", "WAT", "OLD", "MOG", "KGX", "LST", "FST"]],
]

# "next 20 journeys" pairs.
RANGE_QUERIES = [
    ["TBW", "NRW"],
    ["BHM", "EDB"],
    ["PNZ", "DIS"],
    ["YRK", "NRW"],
    ["RDG", "IPS"],
]


def journey_digest(j) -> int:
    """Order-independent digest contribution of one journey."""
    dep = j.departureTime % 1000000000
    arr = j.arrivalTime % 1000000000
    legs = len(j.legs)
    return (dep * 1000003 + arr * 31 + legs) % P


def accumulate(journeys, acc: int) -> int:
    for j in journeys:
        acc = (acc + journey_digest(j)) % P
    return acc


def next_n(group, origin, destination, date, start_time, n):
    """"next N journeys departing after startTime" — RangeQuery loop, capped at N."""
    results = []
    time = start_time
    while len(results) < n:
        new_results = group.plan([origin], [destination], Date(date), time)
        if len(new_results) == 0:
            break
        results.extend(new_results)
        time = min(j.departureTime for j in new_results) + 1
    results.sort(key=lambda j: (j.departureTime, j.arrivalTime))
    return results[:n]


def main() -> None:
    here = os.path.dirname(os.path.abspath(__file__))
    data_dir = sys.argv[1] if len(sys.argv) > 1 else os.path.join(here, "..", "data")

    t0 = _time.perf_counter()
    trips, transfers, interchange = load_gtfs(data_dir)
    load_ms = (_time.perf_counter() - t0) * 1000.0

    t1 = _time.perf_counter()
    raptor = RaptorAlgorithmFactory.create(trips, transfers, interchange)
    prep_ms = (_time.perf_counter() - t1) * 1000.0

    jf = JourneyFactory()
    group_filtered = GroupStationDepartAfterQuery(raptor, jf, 3, [MultipleCriteriaFilter()])
    group_plain = GroupStationDepartAfterQuery(raptor, jf, 3, [])

    # GROUP workload
    tg = _time.perf_counter()
    group_count = 0
    group_digest = 0
    for origins, destinations in GROUP_QUERIES:
        results = group_filtered.plan(origins, destinations, Date(DATE), GROUP_TIME)
        group_count += len(results)
        group_digest = accumulate(results, group_digest)
    group_ms = (_time.perf_counter() - tg) * 1000.0

    # RANGE workload ("next 20")
    tr = _time.perf_counter()
    range_count = 0
    range_digest = 0
    for origin, destination in RANGE_QUERIES:
        results = next_n(group_plain, origin, destination, DATE, RANGE_START, RANGE_N)
        range_count += len(results)
        range_digest = accumulate(results, range_digest)
    range_ms = (_time.perf_counter() - tr) * 1000.0

    out = [
        f"LOAD ms={load_ms:.1f}",
        f"PREP ms={prep_ms:.1f}",
        f"GROUP queries={len(GROUP_QUERIES)} journeys={group_count} digest={group_digest} ms={group_ms:.1f}",
        f"RANGE queries={len(RANGE_QUERIES)} journeys={range_count} digest={range_digest} ms={range_ms:.1f}",
        f"DIGEST group={group_digest} range={range_digest} journeys={group_count + range_count}",
    ]
    sys.stdout.write("\n".join(out) + "\n")


if __name__ == "__main__":
    main()
