import unittest
from types import SimpleNamespace

from raptor.transfer_pattern import GraphResults, StringResults, TreeNode


def _stop_time(stop):
    # The reference spec passes {stop: origin} only; JS reads .departureTime as
    # undefined (NaN arithmetic, never asserted). We supply 0 so the Python
    # arithmetic in StringResults._get_path does not raise. departureTime does
    # not affect the asserted result keys/strings.
    return SimpleNamespace(stop=stop, departureTime=0)


def _synthetic_trip(origin):
    # mirrors { stopTimes: [{ stop: origin }] }
    return SimpleNamespace(stopTimes=[_stop_time(origin)])


def merge_path_graph(path, tree):
    k_connections = {}
    for i in range(1, len(path)):
        origin = path[i - 1]
        destination = path[i]
        k_connections[destination] = {}
        # connection shape: [trip, start, end] = [{stopTimes:[{stop:origin}]}, 0, 1]
        k_connections[destination][i] = (_synthetic_trip(origin), 0, 1)
    tree.add(k_connections)


def merge_path_string(path, tree):
    k_connections = {}
    for i in range(1, len(path)):
        origin = path[i - 1]
        destination = path[i]
        k_connections[destination] = {}
        k_connections[destination][i] = (_synthetic_trip(origin), 0, 1)
    tree.add(k_connections)


class TestGraphResults(unittest.TestCase):
    def test_merges_a_path_into_an_empty_tree(self):
        tree = GraphResults()
        A = TreeNode("A", None)
        B = TreeNode("B", A)
        C = TreeNode("C", B)
        expected = {"A": [A], "B": [B], "C": [C]}

        merge_path_graph(["A", "B", "C"], tree)
        self.assertEqual(tree.finalize(), expected)

    def test_merges_duplicate_paths(self):
        tree = GraphResults()
        A = TreeNode("A", None)
        B = TreeNode("B", A)
        C = TreeNode("C", B)
        expected = {"A": [A], "B": [B], "C": [C]}

        merge_path_graph(["A", "B", "C"], tree)
        merge_path_graph(["A", "B"], tree)
        self.assertEqual(tree.finalize(), expected)

    def test_appends_to_existing_paths(self):
        tree = GraphResults()
        A = TreeNode("A", None)
        B = TreeNode("B", A)
        C = TreeNode("C", B)
        expected = {"A": [A], "B": [B], "C": [C]}

        merge_path_graph(["A", "B"], tree)
        merge_path_graph(["A", "B", "C"], tree)
        self.assertEqual(tree.finalize(), expected)

    def test_appends_different_paths(self):
        tree = GraphResults()
        A = TreeNode("A", None)
        B = TreeNode("B", A)
        C = TreeNode("C", B)
        D = TreeNode("D", C)
        D1 = TreeNode("D", B)
        expected = {"A": [A], "B": [B], "C": [C], "D": [D, D1]}

        merge_path_graph(["A", "B", "C", "D"], tree)
        merge_path_graph(["A", "B", "D"], tree)
        self.assertEqual(tree.finalize(), expected)


class TestStringResults(unittest.TestCase):
    def test_merges_duplicate_paths(self):
        tree = StringResults({})
        expected = {
            "AB": {""},
            "AC": {"B"},
            "AD": {"B,C"},
        }
        merge_path_string(["A", "B", "C", "D"], tree)
        merge_path_string(["A", "B", "C"], tree)
        self.assertEqual(tree.finalize(), expected)

    def test_orders_results(self):
        tree = StringResults({})
        expected = {
            "AC": {"B"},
            "BC": {"", "D"},
            "CE": {"B"},
            "CD": {""},
        }
        merge_path_string(["C", "B", "A"], tree)
        merge_path_string(["C", "D", "B"], tree)
        merge_path_string(["C", "B", "E"], tree)
        self.assertEqual(tree.finalize(), expected)

    def test_adds_different_paths(self):
        tree = StringResults({})
        expected = {
            "AC": {"", "B"},
            "AB": {"", "C"},
            "AD": {"B,C", "C,B"},
        }
        merge_path_string(["A", "B", "C", "D"], tree)
        merge_path_string(["A", "C", "B", "D"], tree)
        self.assertEqual(tree.finalize(), expected)


if __name__ == "__main__":
    unittest.main()
