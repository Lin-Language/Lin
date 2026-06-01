#!/usr/bin/env python3
# gen_graph.py — one-off, deterministic generator for the Dijkstra benchmark input.
#
# Run once (NOT in the timed path) to (re)generate the two committed files:
#
#     python3 benchmarks/compare/data/gen_graph.py
#
# It writes BOTH files from the SAME in-memory edge list, so graph.json and
# graph.txt always encode the identical graph:
#
#   graph.json  (read by Lin / Python / Node — JSON-native languages)
#     {"nodes": ["n0", ..., "n999"],
#      "edges": [{"from": "n0", "to": "n1", "weight": 7}, ...]}
#
#   graph.txt   (read by Go / Rust — no JSON library needed, single build cmd)
#     line 1:    "<num_nodes> <source> <target>"   e.g. "1000 n0 n999"
#     lines 2..: "<from> <to> <weight>"            e.g. "n0 n1 7"
#     (space-separated; edges in the SAME order as graph.json)
#
# Graph shape (fixed seed -> fully reproducible):
#   N = 4000 nodes named n0..n3999.
#   For each i, edges i -> i+1 .. i+8 (skipping out-of-range targets). The
#   i -> i+1 chain guarantees n0 reaches n3999, so the graph is connected.
#   Plus a handful of long "skip" edges: each i with probability ~0.3 gets one
#   extra edge to a random j > i. Weights are random ints in [1, 100].
#   Total ~32000 edges. (Sized for the O(V^2) linear-scan PQ: large enough that
#   the inner min-scan dominates and language/runtime differences are visible.)

import json
import os
import random

SEED = 1234          # hardcoded: must be reproducible, never system time
N = 4000
SOURCE = "n0"
TARGET = "n" + str(N - 1)
FANOUT = 8           # i -> i+1 .. i+FANOUT
SKIP_PROB = 0.3      # chance i gets one extra long edge to a random j > i

rng = random.Random(SEED)

edges = []
for i in range(N):
    # Short fan-out edges i -> i+1 .. i+FANOUT (guarantees connectivity via i->i+1).
    for d in range(1, FANOUT + 1):
        j = i + d
        if j >= N:
            break
        w = rng.randint(1, 100)
        edges.append((i, j, w))
    # Occasional long forward "skip" edge.
    if i + 2 < N and rng.random() < SKIP_PROB:
        j = rng.randint(i + 2, N - 1)
        w = rng.randint(1, 100)
        edges.append((i, j, w))


def name(idx):
    return "n" + str(idx)


nodes = [name(i) for i in range(N)]

here = os.path.dirname(os.path.abspath(__file__))

# graph.json
graph = {
    "nodes": nodes,
    "edges": [{"from": name(a), "to": name(b), "weight": w} for (a, b, w) in edges],
}
with open(os.path.join(here, "graph.json"), "w") as f:
    json.dump(graph, f)
    f.write("\n")

# graph.txt (same edge order)
with open(os.path.join(here, "graph.txt"), "w") as f:
    f.write(f"{N} {SOURCE} {TARGET}\n")
    for (a, b, w) in edges:
        f.write(f"{name(a)} {name(b)} {w}\n")

print(f"wrote {len(nodes)} nodes, {len(edges)} edges to graph.json and graph.txt")
