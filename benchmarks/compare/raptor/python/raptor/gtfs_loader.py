"""GTFS loader (mirrors src/gtfs/GTFSLoader.ts, and node/src/gtfs/GTFSLoader.js).

Reads a directory of plain CSV GTFS files (no quoted fields in this feed) and
returns (trips, transfers, interchange):

  - trips: List[Trip] with stopTimes (in file order) and service resolved.
  - transfers: Dict[StopID, List[Transfer]] from transfers.txt (non-same-stop)
    and links.txt footpaths.
  - interchange: Dict[StopID, Time] from same-stop transfers.txt rows.

Times may exceed 24h (no mod 86400). pickup/dropoff are True only when the field
is "0" or empty. Trips whose serviceId has no calendar row are dropped and the
count reported to stderr.
"""
from __future__ import annotations

import os
import sys
from typing import Dict, List, Tuple

from .gtfs import MAX_SAFE_INTEGER, StopID, StopTime, Time, Transfer, Trip
from .service import Service
from .time_parser import TimeParser


def _read_csv(path: str) -> Tuple[List[str], List[List[str]]]:
    """Read a plain CSV file; return (header, rows) where header is the list of
    column names and rows is a list of string lists. Trailing empty lines are
    skipped (split-on-comma is sufficient — no quoted fields in this feed)."""
    with open(path, "r", encoding="utf-8") as f:
        text = f.read()

    lines = text.split("\n")
    header = lines[0].split(",")
    rows: List[List[str]] = []

    for i in range(1, len(lines)):
        line = lines[i]
        if len(line) == 0:
            continue
        rows.append(line.split(","))

    return header, rows


def _index_of_columns(header: List[str]) -> Dict[str, int]:
    """column-name -> index lookup, so we read columns by name independent of
    incidental column order."""
    return {name: i for i, name in enumerate(header)}


def load_gtfs(
    data_dir: str,
) -> Tuple[List[Trip], Dict[StopID, List[Transfer]], Dict[StopID, Time]]:
    time_parser = TimeParser()
    trips: List[Trip] = []
    transfers: Dict[StopID, List[Transfer]] = {}
    interchange: Dict[StopID, Time] = {}
    calendars: Dict[str, dict] = {}
    dates: Dict[str, Dict[int, bool]] = {}
    stop_times: Dict[str, List[StopTime]] = {}

    # --- calendar.txt -> calendars ---
    header, rows = _read_csv(os.path.join(data_dir, "calendar.txt"))
    c = _index_of_columns(header)
    for row in rows:
        service_id = row[c["service_id"]]
        calendars[service_id] = {
            "serviceId": service_id,
            "startDate": int(row[c["start_date"]]),
            "endDate": int(row[c["end_date"]]),
            "days": {
                0: row[c["sunday"]] == "1",
                1: row[c["monday"]] == "1",
                2: row[c["tuesday"]] == "1",
                3: row[c["wednesday"]] == "1",
                4: row[c["thursday"]] == "1",
                5: row[c["friday"]] == "1",
                6: row[c["saturday"]] == "1",
            },
        }

    # --- calendar_dates.txt -> dates[service_id][+date] = (exception_type == "1") ---
    header, rows = _read_csv(os.path.join(data_dir, "calendar_dates.txt"))
    c = _index_of_columns(header)
    for row in rows:
        service_id = row[c["service_id"]]
        date = int(row[c["date"]])
        include = row[c["exception_type"]] == "1"

        if service_id not in dates:
            dates[service_id] = {}
        dates[service_id][date] = include

    # --- stop_times.txt -> stopTimes grouped by trip_id, in file order ---
    header, rows = _read_csv(os.path.join(data_dir, "stop_times.txt"))
    c = _index_of_columns(header)
    ci_trip = c["trip_id"]
    ci_stop = c["stop_id"]
    ci_dep = c["departure_time"]
    ci_arr = c["arrival_time"]
    ci_pickup = c["pickup_type"]
    ci_dropoff = c["drop_off_type"]
    get_time = time_parser.getTime
    for row in rows:
        trip_id = row[ci_trip]
        pickup_type = row[ci_pickup]
        drop_off_type = row[ci_dropoff]

        stop_time = StopTime(
            stop=row[ci_stop],
            departureTime=get_time(row[ci_dep]),
            arrivalTime=get_time(row[ci_arr]),
            # "0" or empty => True; "1"/"3" => False (matches reference).
            pickUp=pickup_type == "0" or pickup_type == "",
            dropOff=drop_off_type == "0" or drop_off_type == "",
        )

        lst = stop_times.get(trip_id)
        if lst is None:
            lst = []
            stop_times[trip_id] = lst
        lst.append(stop_time)

    # --- trips.txt -> trips (stopTimes/service resolved after) ---
    header, rows = _read_csv(os.path.join(data_dir, "trips.txt"))
    c = _index_of_columns(header)
    for row in rows:
        trips.append(
            Trip(
                tripId=row[c["trip_id"]],
                stopTimes=[],
                serviceId=row[c["service_id"]],
                service=None,  # resolved below
            )
        )

    # --- transfers.txt -> interchange + transfers ---
    header, rows = _read_csv(os.path.join(data_dir, "transfers.txt"))
    c = _index_of_columns(header)
    for row in rows:
        from_stop = row[c["from_stop_id"]]
        to_stop = row[c["to_stop_id"]]

        if from_stop == to_stop:
            interchange[from_stop] = int(row[c["min_transfer_time"]])
        else:
            t = Transfer(
                origin=from_stop,
                destination=to_stop,
                duration=int(row[c["min_transfer_time"]]),
                startTime=0,
                endTime=MAX_SAFE_INTEGER,
            )
            transfers.setdefault(from_stop, []).append(t)

    # --- links.txt -> transfers (footpaths; date/day columns ignored) ---
    header, rows = _read_csv(os.path.join(data_dir, "links.txt"))
    c = _index_of_columns(header)
    for row in rows:
        from_stop = row[c["from_stop_id"]]
        t = Transfer(
            origin=from_stop,
            destination=row[c["to_stop_id"]],
            duration=int(row[c["duration"]]),
            startTime=get_time(row[c["start_time"]]),
            endTime=get_time(row[c["end_time"]]),
        )
        transfers.setdefault(from_stop, []).append(t)

    # --- Service resolution ---
    services: Dict[str, Service] = {}
    for service_id, cal in calendars.items():
        services[service_id] = Service(
            cal["startDate"], cal["endDate"], cal["days"], dates.get(service_id, {})
        )

    # Resolve stopTimes + service per trip; drop trips whose serviceId has no calendar.
    resolved_trips: List[Trip] = []
    dropped = 0
    for t in trips:
        service = services.get(t.serviceId)
        if service is None:
            dropped += 1
            continue
        t.stopTimes = stop_times.get(t.tripId, [])
        t.service = service
        resolved_trips.append(t)

    if dropped > 0:
        sys.stderr.write(f"dropped {dropped} trip(s) with no calendar row\n")

    return resolved_trips, transfers, interchange
