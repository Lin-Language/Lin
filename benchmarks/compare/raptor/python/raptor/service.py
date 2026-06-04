"""Service calendar logic (mirrors src/gtfs/Service.ts)."""
from __future__ import annotations

from typing import Dict

from .gtfs import DateNumber, DayOfWeek


class Service:
    def __init__(
        self,
        start_date: DateNumber,
        end_date: DateNumber,
        days: Dict[DayOfWeek, bool],
        dates: Dict[DateNumber, bool],
    ) -> None:
        self.startDate = start_date
        self.endDate = end_date
        self.days = days
        # include/exclude index: key present with True = include, present with
        # False = exclude, absent = fall back to day-of-week + range.
        self.dates = dates

    def runsOn(self, date: DateNumber, dow: DayOfWeek) -> bool:
        # Replicates: dates[date] || (!hasOwn(dates, date) && start<=date<=end && days[dow])
        if self.dates.get(date):  # truthy include short-circuits
            return True

        return (
            date not in self.dates
            and self.startDate <= date
            and self.endDate >= date
            and bool(self.days.get(dow))
        )
