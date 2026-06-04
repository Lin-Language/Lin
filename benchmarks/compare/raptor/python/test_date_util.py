import unittest

from raptor.date_util import Date, getDateNumber


class TestDateUtil(unittest.TestCase):
    def test_get_date_number(self):
        self.assertEqual(getDateNumber(Date("2018-10-16")), 20181016)
        self.assertEqual(getDateNumber(Date("2019-04-18")), 20190418)

    def test_pinned_days_of_week(self):
        # JS getDay: Sunday=0..Saturday=6 (contract #6 pinned values)
        self.assertEqual(Date("2018-10-16").getDay(), 2)  # Tuesday
        self.assertEqual(Date("2018-10-22").getDay(), 1)  # Monday
        self.assertEqual(Date("2019-04-18").getDay(), 4)  # Thursday
        self.assertEqual(Date("2019-04-23").getDay(), 2)  # Tuesday
        self.assertEqual(Date("2018-12-31").getDay(), 1)  # Monday

    def test_add_days_rolls_over_month_and_year(self):
        d = Date("2018-12-31")
        d.add_days(1)
        self.assertEqual(getDateNumber(d), 20190101)


if __name__ == "__main__":
    unittest.main()
