import unittest

from raptor.algorithm import RaptorAlgorithmFactory
from raptor.date_util import Date
from raptor.journey_factory import JourneyFactory
from raptor.range_query import RangeQuery
from util import j, set_default_trip, st, t

journey_factory = JourneyFactory()


class TestRangeQuery(unittest.TestCase):
    def test_performs_profile_queries(self):
        trips = [
            t(st("A", None, 1000), st("B", 1030, 1035), st("C", 1100, None)),
            t(st("A", None, 1100), st("B", 1130, 1135), st("C", 1200, None)),
            t(st("A", None, 1200), st("B", 1230, 1235), st("C", 1300, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = RangeQuery(raptor, journey_factory)
        result = query.plan("A", "C", Date("2018-10-16"))
        set_default_trip(result)
        self.assertEqual(
            result,
            [
                j([st("A", None, 1000), st("B", 1030, 1035), st("C", 1100, None)]),
                j([st("A", None, 1100), st("B", 1130, 1135), st("C", 1200, None)]),
                j([st("A", None, 1200), st("B", 1230, 1235), st("C", 1300, None)]),
            ],
        )

    def test_does_not_share_best_arrivals_or_route_scanner(self):
        trips = [
            t(st("A", None, 1359), st("C", 1501, None)),
            t(st("A", None, 1400), st("B", 1430, None)),
            t(st("B", None, 1430), st("C", 1500, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = RangeQuery(raptor, journey_factory)
        result = query.plan("A", "C", Date("2018-10-16"))
        set_default_trip(result)
        self.assertEqual(
            result,
            [
                j([st("A", None, 1359), st("C", 1501, None)]),
                j([st("A", None, 1400), st("B", 1430, None)], [st("B", None, 1430), st("C", 1500, None)]),
                j([st("A", None, 1400), st("B", 1430, None)], [st("B", None, 1430), st("C", 1500, None)]),
            ],
        )


if __name__ == "__main__":
    unittest.main()
