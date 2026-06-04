import unittest

from raptor.service import Service
from util import all_days


class TestService(unittest.TestCase):
    def test_checks_the_start_date(self):
        service = Service(20181001, 20181015, all_days, {})
        self.assertEqual(service.runsOn(20180930, 1), False)

    def test_checks_the_end_date(self):
        service = Service(20181001, 20181015, all_days, {})
        self.assertEqual(service.runsOn(20181016, 1), False)

    def test_checks_dates_within_range(self):
        service = Service(20181001, 20181015, all_days, {})
        self.assertEqual(service.runsOn(20181010, 1), True)

    def test_checks_the_day_of_the_week(self):
        days = {**all_days, 1: False}
        service = Service(20181001, 20991231, days, {})
        self.assertEqual(service.runsOn(20181016, 1), False)

    def test_checks_include_days(self):
        service = Service(20991231, 20991231, all_days, {20181022: True})
        self.assertEqual(service.runsOn(20181022, 1), True)

    def test_checks_exclude_days(self):
        service = Service(20181001, 20991231, all_days, {20181022: False})
        self.assertEqual(service.runsOn(20181022, 1), False)


if __name__ == "__main__":
    unittest.main()
