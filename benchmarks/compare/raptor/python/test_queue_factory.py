import unittest

from raptor.queue_factory import QueueFactory


class TestQueueFactory(unittest.TestCase):
    def test_enqueues_stops(self):
        factory = QueueFactory(
            {"StopA": ["RouteA", "RouteB"], "StopB": ["RouteB", "RouteC"]},
            {
                "RouteA": {"StopA": 1},
                "RouteB": {"StopA": 2, "StopB": 1},
                "RouteC": {"StopB": 1},
            },
        )

        actual = factory.getQueue(["StopA", "StopB"])
        expected = {"RouteA": "StopA", "RouteB": "StopB", "RouteC": "StopB"}
        self.assertEqual(actual, expected)

    def test_picks_the_earliest_stop_on_the_route(self):
        factory = QueueFactory(
            {"StopA": ["RouteA", "RouteB"], "StopB": ["RouteB", "RouteC"]},
            {
                "RouteA": {"StopA": 1},
                "RouteB": {"StopA": 1, "StopB": 2},
                "RouteC": {"StopB": 1},
            },
        )

        actual = factory.getQueue(["StopB", "StopA"])
        expected = {"RouteA": "StopA", "RouteB": "StopA", "RouteC": "StopB"}
        self.assertEqual(actual, expected)


if __name__ == "__main__":
    unittest.main()
