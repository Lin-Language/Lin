import unittest

from raptor.time_parser import TimeParser


class TestTimeParser(unittest.TestCase):
    def test_turns_a_time_string_into_seconds_from_midnight(self):
        parser = TimeParser()
        self.assertEqual(0, parser.getTime("00:00:00"))
        self.assertEqual(10, parser.getTime("00:00:10"))
        self.assertEqual(130, parser.getTime("00:02:10"))
        self.assertEqual(10930, parser.getTime("03:02:10"))


if __name__ == "__main__":
    unittest.main()
