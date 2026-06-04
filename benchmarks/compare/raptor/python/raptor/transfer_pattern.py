"""Transfer-pattern result stores (mirrors src/transfer-pattern/results/
GraphResults.ts and StringResults.ts).
"""
from __future__ import annotations

from typing import Dict, List, Optional, Set, Tuple

from .gtfs import MAX_SAFE_INTEGER, StopID, Time
from .journey import is_transfer
from .scan_results import ConnectionIndex

Path = List[StopID]


class TreeNode:
    """Graph node maintaining a reference to its parent node. Equality is
    structural: same label and structurally-equal parent chain (the spec
    compares the finalized tree with toEqual / deep structural equality)."""

    def __init__(self, label: StopID, parent: Optional["TreeNode"]) -> None:
        self.label = label
        self.parent = parent

    def __eq__(self, other) -> bool:
        if not isinstance(other, TreeNode):
            return NotImplemented
        return self.label == other.label and self.parent == other.parent

    def __repr__(self) -> str:
        return f"TreeNode(label={self.label!r}, parent={self.parent!r})"


TransferPatternGraph = Dict[StopID, List[TreeNode]]


class GraphResults:
    """Stores Raptor results as a DAG."""

    def __init__(self) -> None:
        self.results: TransferPatternGraph = {}

    def add(self, k_connections: ConnectionIndex) -> None:
        for path in self._get_paths(k_connections):
            self._merge_path(path)

    def finalize(self) -> TransferPatternGraph:
        return self.results

    def _get_paths(self, k_connections: ConnectionIndex) -> List[Path]:
        results: List[Path] = []

        for destination in k_connections:
            for k in k_connections[destination]:
                results.append(self._get_path(k_connections, k, destination))

        return results

    def _get_path(self, k_connections: ConnectionIndex, k, final_destination: StopID) -> Path:
        path = [final_destination]

        destination = final_destination
        i = int(k)
        while i > 0:
            connection = k_connections[destination][i]
            origin = (
                connection.origin
                if is_transfer(connection)
                else connection[0].stopTimes[connection[1]].stop
            )

            path.append(origin)
            destination = origin
            i -= 1

        return path

    def _merge_path(self, path: Path) -> TreeNode:
        head, tail = path[0], path[1:]

        if head not in self.results:
            self.results[head] = []

        node = next((n for n in self.results[head] if self._is_same(tail, n.parent)), None)

        if node is None:
            parent = self._merge_path(tail) if len(tail) > 0 else None
            node = TreeNode(label=head, parent=parent)
            self.results[head].append(node)

        return node

    def _is_same(self, path: Path, node: Optional[TreeNode]) -> bool:
        i = 0
        while node is not None:
            if node.label != (path[i] if i < len(path) else None):
                return False
            i += 1
            node = node.parent

        return True


JourneyPatternKey = str
JourneyPattern = str
TransferPatternIndex = Dict[JourneyPatternKey, Set[JourneyPattern]]


class StringResults:
    """Stores results as an index keyed by origin+destination -> Set of change
    point strings."""

    def __init__(self, interchange: Dict[StopID, Time]) -> None:
        self.interchange = interchange
        self.results: TransferPatternIndex = {}

    def add(self, k_connections: ConnectionIndex) -> int:
        next_departure_time = MAX_SAFE_INTEGER

        for destination in k_connections:
            for k in k_connections[destination]:
                path, departure_time = self._get_path(k_connections, k, destination)

                if len(path) >= 1:
                    origin = path[0]
                    tail = path[1:]
                    journey_key = (destination + origin) if origin > destination else (origin + destination)
                    if origin > destination:
                        tail.reverse()
                        path_string = ",".join(tail)
                    else:
                        path_string = ",".join(tail)

                    if journey_key not in self.results:
                        self.results[journey_key] = set()
                    self.results[journey_key].add(path_string)
                    next_departure_time = min(next_departure_time, departure_time + 1)

        return next_departure_time

    def finalize(self) -> TransferPatternIndex:
        return self.results

    def _get_path(self, k_connections: ConnectionIndex, k, final_destination: StopID) -> Tuple[Path, Time]:
        path: Path = []
        departure_time = MAX_SAFE_INTEGER

        destination = final_destination
        i = int(k)
        while i > 0:
            connection = k_connections[destination][i]
            origin = (
                connection.origin
                if is_transfer(connection)
                else connection[0].stopTimes[connection[1]].stop
            )

            if is_transfer(connection):
                departure_time = departure_time - connection.duration - self.interchange[connection.destination]
            else:
                departure_time = connection[0].stopTimes[connection[1]].departureTime

            path.insert(0, origin)
            destination = origin
            i -= 1

        return path, departure_time
