# dijkstra — shortest paths over a weighted graph

Reads a weighted directed graph from JSON, runs Dijkstra's algorithm from a source
node, prints the shortest path and distance to a target, and writes the result to
a JSON file. Exercises object-as-map mutation, recursion, and a hand-rolled
priority queue.

## What it demonstrates

- **Named type aliases**: `Edge` (`{ from, to, weight }`), `Neighbor`
  (`{ to, weight }`), `PqEntry` (`{ node, dist }`), and `DijkstraResult`
  (`{ dist, prev }`).
- **Typed arrays** flowing through the algorithm: `Edge[]` in, `PqEntry[]` queue,
  `String[]` reconstructed path.
- **Dynamic `Json` maps kept dynamic** where appropriate: the adjacency, distance,
  and predecessor structures are keyed by node name at runtime and built with
  `lin_object_set`.
- Tail-recursive queue processing and path reconstruction.
- Reading/writing JSON from the filesystem (`std/fs`) and command-line `args()`.

## Structure

| File | What it is |
| --- | --- |
| `graph.lin` | `buildAdj(edges)` and `reconstructPath(prev, source, target)`. Owns `Edge`, `Neighbor`. |
| `algorithm.lin` | `dijkstra(adj, source, allNodes)` plus the priority-queue helpers. Owns `PqEntry`, `DijkstraResult`. |
| `main.lin` | Reads `argv` (graph path, source, target, output path), runs the search, prints + writes the result. |
| `graph.json` | Sample 5-node graph. |
| `*.test.lin` | `graph`, `algorithm`, `e2e`, and `minimal` unit/integration tests. |

## Run / Test

`main.lin` takes command-line arguments: `<graph.json> <source> <target> <out.json>`.

```sh
lin run examples/dijkstra/main.lin -- examples/dijkstra/graph.json A E /tmp/dout.json
lin test examples/dijkstra/
```

Expected output for `A → E`: `path: A C B D E`, `distance: 14`.
