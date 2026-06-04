import unittest

from raptor.algorithm import RaptorAlgorithmFactory
from raptor.date_util import Date
from raptor.group_query import GroupStationDepartAfterQuery
from raptor.journey_factory import JourneyFactory
from raptor.multiple_criteria_filter import MultipleCriteriaFilter
from util import j, set_default_trip, st, t

journey_factory = JourneyFactory()


class TestGroupStationDepartAfterQuery(unittest.TestCase):
    def setUp(self):
        self.filters = [MultipleCriteriaFilter()]

    def test_plans_to_multiple_destinations(self):
        trips = [
            t(st("A", None, 1000), st("B", 1030, 1035), st("C", 1100, None)),
            t(st("A", None, 1200), st("B", 1230, 1235), st("D", 1300, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = GroupStationDepartAfterQuery(raptor, journey_factory, 1, self.filters)
        result = query.plan(["A"], ["C", "D"], Date("2019-04-18"), 900)
        set_default_trip(result)
        self.assertEqual(
            result,
            [
                j([st("A", None, 1000), st("B", 1030, 1035), st("C", 1100, None)]),
                j([st("A", None, 1200), st("B", 1230, 1235), st("D", 1300, None)]),
            ],
        )

    def test_plans_from_multiple_origins(self):
        trips = [
            t(st("A", None, 1000), st("B", 1030, 1035), st("C", 1100, None)),
            t(st("A", None, 1200), st("_", 1230, 1235), st("D", 1300, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = GroupStationDepartAfterQuery(raptor, journey_factory, 1, self.filters)
        result = query.plan(["A", "B"], ["C", "D"], Date("2019-04-18"), 900)
        set_default_trip(result)
        self.assertEqual(
            result,
            [
                j([st("B", 1030, 1035), st("C", 1100, None)]),
                j([st("A", None, 1200), st("_", 1230, 1235), st("D", 1300, None)]),
            ],
        )


if __name__ == "__main__":
    unittest.main()
