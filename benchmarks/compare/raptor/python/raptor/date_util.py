"""Date handling (mirrors src/query/DateUtil.ts + the bits of JS Date we use).

JS `new Date("2018-10-16")` parses as UTC midnight. We model that with a plain
datetime.date (which is timezone-naive but we treat it as the UTC calendar date).

getDateNumber(date) -> YYYYMMDD integer.

DOW: JS Date.getDay returns Sunday=0..Saturday=6. Python date.weekday() returns
Monday=0..Sunday=6, so we convert: js_dow = (weekday + 1) % 7.
"""
from __future__ import annotations

from datetime import date as _date
from datetime import timedelta

from .gtfs import DateNumber, DayOfWeek


class Date:
    """Minimal mutable UTC-calendar date wrapper mirroring the JS Date methods
    the algorithm relies on: getDay() and setDate(getDate()+1)."""

    def __init__(self, iso: str) -> None:
        # iso is "YYYY-MM-DD"; parse as the UTC calendar date.
        year, month, day = (int(p) for p in iso.split("-"))
        self._d = _date(year, month, day)

    @classmethod
    def from_date(cls, d: _date) -> "Date":
        obj = cls.__new__(cls)
        obj._d = d
        return obj

    def getDay(self) -> DayOfWeek:
        # Python Mon=0..Sun=6 -> JS Sun=0..Sat=6
        return (self._d.weekday() + 1) % 7

    def add_days(self, n: int) -> None:
        """Equivalent to JS date.setDate(date.getDate() + n) with month/year
        rollover handled by real calendar arithmetic."""
        self._d = self._d + timedelta(days=n)

    def clone(self) -> "Date":
        return Date.from_date(self._d)

    @property
    def underlying(self) -> _date:
        return self._d


def getDateNumber(date: Date) -> DateNumber:
    d = date.underlying
    return int(d.strftime("%Y%m%d"))
