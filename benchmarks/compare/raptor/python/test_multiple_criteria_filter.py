import unittest

from raptor.multiple_criteria_filter import MultipleCriteriaFilter
from util import j, st


class TestMultipleCriteriaFilter(unittest.TestCase):
    def setUp(self):
        self.filter = MultipleCriteriaFilter()

    def test_removes_slower_journeys(self):
        journeys = [
            j([st("A", None, 1000), st("B", 1030, 1035), st("C", 1100, None)]),
            j([st("A", None, 900), st("B", 1030, 1035), st("C", 1100, None)]),
            j([st("A", None, 1100), st("B", 1130, 1135), st("C", 1200, None)]),
            j([st("A", None, 1200), st("B", 1230, 1235), st("C", 1300, None)]),
        ]
        expected = [
            j([st("A", None, 1000), st("B", 1030, 1035), st("C", 1100, None)]),
            j([st("A", None, 1100), st("B", 1130, 1135), st("C", 1200, None)]),
            j([st("A", None, 1200), st("B", 1230, 1235), st("C", 1300, None)]),
        ]
        self.assertEqual(self.filter.apply(journeys), expected)

    def test_keeps_slower_journeys_if_they_have_fewer_changes(self):
        journeys = [
            j([st("A", None, 1000), st("B", 1030, 1035)], [st("C", 1100, None)]),
            j([st("A", None, 900), st("C", 1100, None)]),
            j([st("A", None, 1100), st("B", 1130, 1135), st("C", 1200, None)]),
            j([st("A", None, 1200), st("B", 1230, 1235), st("C", 1300, None)]),
        ]
        expected = [
            j([st("A", None, 900), st("C", 1100, None)]),
            j([st("A", None, 1000), st("B", 1030, 1035)], [st("C", 1100, None)]),
            j([st("A", None, 1100), st("B", 1130, 1135), st("C", 1200, None)]),
            j([st("A", None, 1200), st("B", 1230, 1235), st("C", 1300, None)]),
        ]
        self.assertEqual(self.filter.apply(journeys), expected)

    def test_sorts_journeys_before_filtering_them(self):
        journeys = [
            j([st("A", None, 1000), st("B", 1030, 1035), st("C", 1100, None)]),
            j([st("A", None, 900), st("B", 1030, 1035), st("C", 1100, None)]),
            j([st("A", None, 1100), st("B", 1130, 1135), st("C", 1200, None)]),
            j([st("A", None, 1200), st("B", 1230, 1235), st("C", 1300, None)]),
            j([st("A", None, 1200), st("B", 1230, 1235), st("C", 1330, None)]),
        ]
        expected = [
            j([st("A", None, 1000), st("B", 1030, 1035), st("C", 1100, None)]),
            j([st("A", None, 1100), st("B", 1130, 1135), st("C", 1200, None)]),
            j([st("A", None, 1200), st("B", 1230, 1235), st("C", 1300, None)]),
        ]
        self.assertEqual(self.filter.apply(journeys), expected)


if __name__ == "__main__":
    unittest.main()
