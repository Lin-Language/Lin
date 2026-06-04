"""Parses "HH:MM:SS" time strings to seconds from midnight, cached.

Mirrors src/gtfs/TimeParser.ts.
"""
from __future__ import annotations

from typing import Dict


class TimeParser:
    def __init__(self) -> None:
        self.timeCache: Dict[str, int] = {}

    def getTime(self, time: str) -> int:
        if time not in self.timeCache:
            hh, mm, ss = time.split(":")
            self.timeCache[time] = int(hh) * 60 * 60 + int(mm) * 60 + int(ss)

        return self.timeCache[time]
