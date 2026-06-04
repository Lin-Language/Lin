import unittest

from raptor.algorithm import RaptorAlgorithmFactory
from raptor.date_util import Date
from raptor.depart_after_query import DepartAfterQuery
from raptor.journey_factory import JourneyFactory
from raptor.service import Service
from util import all_days, j, services, set_default_trip, st, t, tf

journey_factory = JourneyFactory()


class TestDepartAfterQuery(unittest.TestCase):
    def test_finds_journeys_with_direct_connections(self):
        trips = [t(st("A", None, 1000), st("B", 1030, 1035), st("C", 1100, None))]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = DepartAfterQuery(raptor, journey_factory)
        result = query.plan("A", "C", Date("2018-10-16"), 900)
        set_default_trip(result)
        self.assertEqual(result, [j([st("A", None, 1000), st("B", 1030, 1035), st("C", 1100, None)])])

    def test_finds_the_earliest_calendars(self):
        trips = [
            t(st("A", None, 1400), st("B", 1430, 1435), st("C", 1500, None)),
            t(st("A", None, 1000), st("B", 1030, 1035), st("C", 1100, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = DepartAfterQuery(raptor, journey_factory)
        result = query.plan("A", "C", Date("2018-10-16"), 900)
        set_default_trip(result)
        self.assertEqual(result, [j([st("A", None, 1000), st("B", 1030, 1035), st("C", 1100, None)])])

    def test_finds_journeys_with_a_single_connection(self):
        trips = [
            t(st("A", None, 1000), st("B", 1030, 1035), st("C", 1100, None)),
            t(st("D", None, 1000), st("B", 1030, 1035), st("E", 1100, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = DepartAfterQuery(raptor, journey_factory)
        result = query.plan("A", "E", Date("2018-10-16"), 900)
        set_default_trip(result)
        self.assertEqual(
            result,
            [j([st("A", None, 1000), st("B", 1030, 1035)], [st("B", 1030, 1035), st("E", 1100, None)])],
        )

    def test_does_not_return_journeys_that_cannot_be_made(self):
        trips = [
            t(st("A", None, 1000), st("B", 1035, 1035), st("C", 1100, None)),
            t(st("D", None, 1000), st("B", 1030, 1030), st("E", 1100, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = DepartAfterQuery(raptor, journey_factory, 1)
        result = query.plan("A", "E", Date("2018-10-16"), 900)
        set_default_trip(result)
        self.assertEqual(result, [])

    def test_returns_the_fastest_and_the_least_changes(self):
        trips = [
            t(st("A", None, 1000), st("B", 1030, 1030), st("C", 1200, None)),
            t(st("B", None, 1030), st("C", 1100, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = DepartAfterQuery(raptor, journey_factory)
        result = query.plan("A", "C", Date("2018-10-16"), 900)
        set_default_trip(result)
        direct = j([st("A", None, 1000), st("B", 1030, 1030), st("C", 1200, None)])
        change = j([st("A", None, 1000), st("B", 1030, 1030)], [st("B", None, 1030), st("C", 1100, None)])
        self.assertEqual(result, [direct, change])

    def test_chooses_the_fastest_journey_where_the_number_of_journeys_is_the_same(self):
        trips = [
            t(st("A", None, 1000), st("B", 1030, 1030), st("C", 1100, None)),
            t(st("C", None, 1200), st("D", 1230, 1230), st("E", 1300, None)),
            t(st("A", None, 1100), st("F", 1130, 1130), st("G", 1200, None)),
            t(st("G", None, 1200), st("H", 1230, 1230), st("E", 1255, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = DepartAfterQuery(raptor, journey_factory)
        result = query.plan("A", "E", Date("2018-10-16"), 900)
        set_default_trip(result)
        fastest = j(
            [st("A", None, 1100), st("F", 1130, 1130), st("G", 1200, None)],
            [st("G", None, 1200), st("H", 1230, 1230), st("E", 1255, None)],
        )
        self.assertEqual(result, [fastest])

    def test_chooses_an_arbitrary_journey_when_they_are_the_same(self):
        trips = [
            t(st("A", None, 1000), st("B", 1030, 1030), st("C", 1100, None)),
            t(st("C", None, 1200), st("D", 1230, 1230), st("E", 1300, None)),
            t(st("A", None, 1100), st("F", 1130, 1130), st("G", 1200, None)),
            t(st("G", None, 1200), st("H", 1230, 1230), st("E", 1300, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = DepartAfterQuery(raptor, journey_factory)
        result = query.plan("A", "E", Date("2018-10-16"), 900)
        set_default_trip(result)
        journey1 = j(
            [st("A", None, 1000), st("B", 1030, 1030), st("C", 1100, None)],
            [st("C", None, 1200), st("D", 1230, 1230), st("E", 1300, None)],
        )
        self.assertEqual(result, [journey1])

    def test_chooses_the_correct_change_point(self):
        trips = [
            t(st("A", None, 1000), st("B", 1030, None)),
            t(st("A", None, 1030), st("C", 1200, None)),
            t(st("C", None, 1000), st("B", 1030, 1030), st("E", 1100, None)),
            t(st("C", None, 1200), st("B", 1230, 1230), st("E", 1300, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = DepartAfterQuery(raptor, journey_factory)
        result = query.plan("A", "E", Date("2018-10-16"), 900)
        set_default_trip(result)
        change = j([st("A", None, 1000), st("B", 1030, None)], [st("B", 1030, 1030), st("E", 1100, None)])
        self.assertEqual(result, [change])

    def test_finds_journeys_with_a_transfer(self):
        trips = [
            t(st("A", None, 1000), st("B", 1030, 1035), st("C", 1100, None)),
            t(st("D", None, 1200), st("E", 1300, None)),
        ]
        transfers = {"C": [tf("C", "D", 10)]}
        raptor = RaptorAlgorithmFactory.create(trips, transfers, {})
        query = DepartAfterQuery(raptor, journey_factory)
        result = query.plan("A", "E", Date("2018-10-16"), 900)
        set_default_trip(result)
        self.assertEqual(
            result,
            [
                j(
                    [st("A", None, 1000), st("B", 1030, 1035), st("C", 1100, None)],
                    tf("C", "D", 10),
                    [st("D", None, 1200), st("E", 1300, None)],
                )
            ],
        )

    def test_uses_a_transfer_if_it_is_faster(self):
        trips = [
            t(st("A", None, 1000), st("B", 1030, 1030), st("C", 1100, None)),
            t(st("C", None, 1130), st("D", 1200, None)),
        ]
        transfers = {"C": [tf("C", "D", 10)]}
        raptor = RaptorAlgorithmFactory.create(trips, transfers, {})
        query = DepartAfterQuery(raptor, journey_factory)
        result = query.plan("A", "D", Date("2018-10-16"), 900)
        set_default_trip(result)
        transfer = j([st("A", None, 1000), st("B", 1030, 1030), st("C", 1100, None)], tf("C", "D", 10))
        self.assertEqual(result, [transfer])

    def test_doesnt_allow_pick_up_from_locations_without_pickup_specified(self):
        trips = [
            t(st("A", None, 1000), st("B", 1030, 1030), st("C", 1200, None)),
            t(st("E", None, 1000), st("B", 1030, None), st("C", 1100, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = DepartAfterQuery(raptor, journey_factory)
        result = query.plan("A", "C", Date("2018-10-16"), 900)
        set_default_trip(result)
        direct = j([st("A", None, 1000), st("B", 1030, 1030), st("C", 1200, None)])
        self.assertEqual(result, [direct])

    def test_doesnt_allow_drop_off_at_non_drop_off_locations(self):
        trips = [
            t(st("A", None, 1000), st("B", None, 1030), st("C", 1200, None)),
            t(st("E", None, 1000), st("B", 1030, 1030), st("C", 1100, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = DepartAfterQuery(raptor, journey_factory)
        result = query.plan("A", "C", Date("2018-10-16"), 900)
        set_default_trip(result)
        direct = j([st("A", None, 1000), st("B", None, 1030), st("C", 1200, None)])
        self.assertEqual(result, [direct])

    def test_applies_interchange_times(self):
        trips = [
            t(st("A", None, 1000), st("B", 1030, 1030), st("C", 1200, None)),
            t(st("B", None, 1030), st("C", 1100, None)),
            t(st("B", None, 1040), st("C", 1110, None)),
        ]
        interchange = {"B": 10}
        raptor = RaptorAlgorithmFactory.create(trips, {}, interchange)
        query = DepartAfterQuery(raptor, journey_factory)
        result = query.plan("A", "C", Date("2018-10-16"), 900)
        set_default_trip(result)
        direct = j([st("A", None, 1000), st("B", 1030, 1030), st("C", 1200, None)])
        change = j([st("A", None, 1000), st("B", 1030, 1030)], [st("B", None, 1040), st("C", 1110, None)])
        self.assertEqual(result, [direct, change])

    def test_applies_interchange_times_to_transfers(self):
        trips = [
            t(st("A", None, 1000), st("B", 1030, None)),
            t(st("C", None, 1030), st("D", 1100, None)),
            t(st("C", None, 1050), st("D", 1110, None)),
            t(st("C", None, 1100), st("D", 1120, None)),
        ]
        transfers = {"B": [tf("B", "C", 10)]}
        interchange = {"B": 10, "C": 10}
        raptor = RaptorAlgorithmFactory.create(trips, transfers, interchange)
        query = DepartAfterQuery(raptor, journey_factory)
        result = query.plan("A", "D", Date("2018-10-16"), 900)
        set_default_trip(result)
        last_possible = j(
            [st("A", None, 1000), st("B", 1030, None)],
            tf("B", "C", 10),
            [st("C", None, 1100), st("D", 1120, None)],
        )
        self.assertEqual(result, [last_possible])

    def test_omits_calendars_not_running_that_day(self):
        trip = t(st("B", None, 1030), st("C", 1100, None))
        trip.service = Service(20181001, 20181015, all_days, {})
        trips = [
            t(st("A", None, 1000), st("B", 1030, None)),
            trip,
            t(st("B", None, 1040), st("C", 1110, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = DepartAfterQuery(raptor, journey_factory)
        result = query.plan("A", "C", Date("2018-10-16"), 900)
        set_default_trip(result)
        change = j([st("A", None, 1000), st("B", 1030, None)], [st("B", None, 1040), st("C", 1110, None)])
        self.assertEqual(result, [change])

    def test_omits_calendars_not_running_that_day_of_the_week(self):
        trip = t(st("B", None, 1030), st("C", 1100, None))
        days = {**all_days, 1: False}
        trip.service = Service(20181001, 20991231, days, {})
        trips = [
            t(st("A", None, 1000), st("B", 1030, None)),
            trip,
            t(st("B", None, 1040), st("C", 1110, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = DepartAfterQuery(raptor, journey_factory)
        result = query.plan("A", "C", Date("2018-10-22"), 900)
        set_default_trip(result)
        change = j([st("A", None, 1000), st("B", 1030, None)], [st("B", None, 1040), st("C", 1110, None)])
        self.assertEqual(result, [change])

    def test_includes_calendars_with_an_include_day(self):
        trip = t(st("B", None, 1030), st("C", 1100, None))
        trip.service = Service(20991231, 20991231, all_days, {20181022: True})
        trips = [
            t(st("A", None, 1000), st("B", 1030, None)),
            trip,
            t(st("B", None, 1040), st("C", 1110, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = DepartAfterQuery(raptor, journey_factory)
        result = query.plan("A", "C", Date("2018-10-22"), 900)
        set_default_trip(result)
        change = j([st("A", None, 1000), st("B", 1030, None)], [st("B", None, 1030), st("C", 1100, None)])
        self.assertEqual(result, [change])

    def test_omits_calendars_with_an_exclude_day(self):
        trip = t(st("B", None, 1030), st("C", 1100, None))
        trip.service = Service(20181001, 20991231, all_days, {20181022: False})
        trips = [
            t(st("A", None, 1000), st("B", 1030, None)),
            trip,
            t(st("B", None, 1040), st("C", 1110, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = DepartAfterQuery(raptor, journey_factory)
        result = query.plan("A", "C", Date("2018-10-22"), 900)
        set_default_trip(result)
        change = j([st("A", None, 1000), st("B", 1030, None)], [st("B", None, 1040), st("C", 1110, None)])
        self.assertEqual(result, [change])

    def test_finds_journeys_after_gaps_in_rounds(self):
        trips = [
            t(st("A", None, 1000), st("B", 1030, 1035), st("C", 1400, None)),
            t(st("B", None, 1035), st("D", 1100, None)),
            t(st("D", None, 1100), st("E", 1130, None)),
            t(st("E", None, 1130), st("C", 1200, None)),
            t(st("A", None, 1000), st("E", 1135, None)),
            t(st("E", None, 1135), st("C", 1330, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = DepartAfterQuery(raptor, journey_factory)
        result = query.plan("A", "C", Date("2018-10-16"), 900)
        set_default_trip(result)
        direct = j([st("A", None, 1000), st("B", 1030, 1035), st("C", 1400, None)])
        slow_change = j([st("A", None, 1000), st("E", 1135, None)], [st("E", None, 1135), st("C", 1330, None)])
        change = j(
            [st("A", None, 1000), st("B", 1030, 1035)],
            [st("B", None, 1035), st("D", 1100, None)],
            [st("D", None, 1100), st("E", 1130, None)],
            [st("E", None, 1130), st("C", 1200, None)],
        )
        self.assertEqual(result, [direct, slow_change, change])

    def test_puts_overtaken_trains_in_different_routes(self):
        trips = [
            t(
                st("A", None, 1000),
                st("B", 1030, 1030),
                st("C", 1100, 1110),
                st("D", 1130, 1130),
                st("E", 1200, None),
            ),
            t(
                st("A", None, 1010),
                st("B", 1040, 1040),
                st("C", 1050, 1100),
                st("D", 1120, 1120),
                st("E", 1150, None),
            ),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = DepartAfterQuery(raptor, journey_factory)
        result = query.plan("A", "E", Date("2018-10-16"), 900)
        set_default_trip(result)
        faster = j(
            [
                st("A", None, 1010),
                st("B", 1040, 1040),
                st("C", 1050, 1100),
                st("D", 1120, 1120),
                st("E", 1150, None),
            ]
        )
        self.assertEqual(result, [faster])

    def test_finds_journeys_that_can_only_be_made_by_waiting_for_the_next_day(self):
        trips = [
            t(st("A", None, 1000), st("B", 1035, 1035), st("C", 1100, None)),
            t(st("D", None, 1000), st("B", 1030, 1030), st("E", 1100, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = DepartAfterQuery(raptor, journey_factory, 2)
        result = query.plan("A", "E", Date("2018-10-16"), 900)
        set_default_trip(result)
        expected = j([st("A", None, 1000), st("B", 1035, 1035)], [st("B", 1030, 1030), st("E", 1100, None)])
        self.assertEqual(result[0].legs, expected.legs)

    def test_adds_a_day_to_the_arrival_time_of_journeys_that_are_made_overnight(self):
        trips = [
            t(st("A", None, 1000), st("B", 1035, 1035), st("C", 1100, None)),
            t(st("D", None, 1000), st("B", 1030, 1030), st("E", 1100, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = DepartAfterQuery(raptor, journey_factory, 2)
        result = query.plan("A", "E", Date("2018-10-16"), 900)
        set_default_trip(result)
        self.assertEqual(result[0].arrivalTime, 1100 + 86400)

    def test_increments_the_day_when_searching_subsequent_days(self):
        trips = [
            t(st("A", None, 1000), st("B", 1030, 1030), st("C", 1100, None)),
            t(st("D", None, 1000), st("B", 1035, 1035), st("E", 1100, None)),
        ]
        trips[1].service = services["2"]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = DepartAfterQuery(raptor, journey_factory, 2)
        result = query.plan("A", "E", Date("2018-12-31"), 900)
        set_default_trip(result)
        self.assertEqual(result[0].arrivalTime, 1100 + 86400)

    def test_uses_all_results_from_every_day(self):
        trips = [
            t(st("A", None, 1900), st("B", 1930, 1935), st("C", 2000, None)),
            t(st("B", None, 1035), st("D", 1100, None)),
            t(st("D", None, 1100), st("E", 1130, None)),
            t(st("C", None, 1130), st("E", 1200, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = DepartAfterQuery(raptor, journey_factory, 2)
        result = query.plan("A", "E", Date("2019-04-23"), 900)
        set_default_trip(result)
        change = j(
            [st("A", None, 1900), st("B", 1930, 1935)],
            [st("B", None, 1035), st("D", 1100, None)],
            [st("D", None, 1100), st("E", 1130, None)],
        )
        no_change = j(
            [st("A", None, 1900), st("B", 1930, 1935), st("C", 2000, None)],
            [st("C", None, 1130), st("E", 1200, None)],
        )
        expected = [no_change, change]
        for journey in expected:
            journey.arrivalTime += 86400
        self.assertEqual(result, expected)

    def test_does_not_return_overnight_journeys_that_cannot_be_made(self):
        trips = [
            t(st("A", None, 86000), st("B", 86400, 86400), st("C", 86400 + 3600 + 3600, None)),
            t(st("C", None, 3600), st("D", 3635, 3635), st("E", 3700, None)),
            t(st("C", None, 3600 + 3600), st("D", 3635 + 3600, 3635 + 3600), st("E", 3700 + 3600, None)),
        ]
        raptor = RaptorAlgorithmFactory.create(trips, {}, {})
        query = DepartAfterQuery(raptor, journey_factory, 2)
        result = query.plan("A", "E", Date("2018-12-31"), 50000)
        set_default_trip(result)
        self.assertEqual(result[0].arrivalTime, 86400 + 3700 + 3600)


if __name__ == "__main__":
    unittest.main()
