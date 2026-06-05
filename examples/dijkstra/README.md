# dijkstra — shortest paths over a weighted graph

Reads a weighted directed graph from JSON, runs Dijkstra's algorithm from a source
node, prints the shortest path and distance to a target, and writes the result to
a JSON file. Exercises typed dictionary maps, recursion, and a hand-rolled
priority queue.

## What it demonstrates

- **Named type aliases**: `Edge` (`{ from, to, weight }`), `Neighbor`
  (`{ to, weight }`), `PqEntry` (`{ node, dist }`), and `DijkstraResult`
  (`{ dist, prev }`).
- **Typed arrays** flowing through the algorithm: `Edge[]` in, `PqEntry[]` queue,
  `String[]` reconstructed path.
- **Typed index-signature maps** (`{ String: T }`, ADR-082): the adjacency
  (`{ String: Neighbor[] }`), distance (`{ String: Int32 }`), and predecessor
  (`{ String: String }`) structures are keyed by node name and written with plain
  index assignment (`dist[node] = ...`) — O(1) average lookup, no `Json` unwrapping.
- Tail-recursive queue processing and path reconstruction.
- Reading/writing JSON from the filesystem (`std/fs`) and command-line `args()`.

## Structure

| File | What it is |
| --- | --- |
| `graph.lin` | `buildAdj(edges)` and `reconstructPath(prev, source, target)`. Owns `Edge`, `Neighbor`. |
| `algorithm.lin` | `dijkstra(adj, source, allNodes)` plus the priority-queue helpers. Owns `PqEntry`, `DijkstraResult`. |
| `solver.lin` | `solve(graphPath, source, target, outputPath)` — the fs-driven orchestration (read graph → run → optionally write), returning a tagged outcome. Kept separate so it is testable by mocking `std/fs`. |
| `main.lin` | Parses `argv` (graph path, source, target, output path), calls `solve`, and prints the outcome. |
| `graph.json` | Sample 5-node graph. |
| `solver.test.lin` | `solve` with `std/fs` mocked (ADR-071): happy path, read-error, no-path, and the output-write spy — no disk needed. |
| `*.test.lin` | `graph`, `algorithm`, `solver`, and integration tests. |

## Run / Test

`main.lin` takes command-line arguments: `<graph.json> <source> <target> <out.json>`.

```sh
lin run examples/dijkstra/main.lin -- examples/dijkstra/graph.json A E /tmp/dout.json
lin test examples/dijkstra/
```

Expected output for `A → E`: `path: A C B D E`, `distance: 14`.
