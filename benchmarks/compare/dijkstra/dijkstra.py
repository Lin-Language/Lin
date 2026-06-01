# dijkstra.py — linear-scan priority-queue Dijkstra (O(V^2)), reads graph.json.
#
# Reads the graph INSIDE the timed region (JSON parsing counts). Computes the
# checksum and prints exactly one stdout line "RESULT=<int>"; everything else
# goes to stderr.
import json
import os
import sys

INF = 1000000000  # sentinel for "infinity"; finite means dist < INF

HERE = os.path.dirname(os.path.abspath(__file__))
GRAPH = os.path.join(HERE, "..", "data", "graph.json")


def main():
    with open(GRAPH) as f:
        graph = json.load(f)

    nodes = graph["nodes"]
    source = "n0"
    target = nodes[-1]

    # adjacency: node -> list of (to, weight)
    adj = {}
    for e in graph["edges"]:
        adj.setdefault(e["from"], []).append((e["to"], e["weight"]))

    dist = {n: INF for n in nodes}
    dist[source] = 0
    visited = {}

    # Linear-scan priority queue: a list of (node, dist) entries.
    pq = [(source, 0)]
    while pq:
        # Find the entry with the minimum tentative distance (linear scan).
        min_idx = 0
        for i in range(len(pq)):
            if pq[i][1] < pq[min_idx][1]:
                min_idx = i
        u, _ = pq.pop(min_idx)
        if u in visited:
            continue
        visited[u] = True
        for (v, w) in adj.get(u, []):
            nd = dist[u] + w
            if nd < dist[v]:
                dist[v] = nd
                pq.append((v, nd))

    total = 0
    for n in nodes:
        if dist[n] < INF:
            total += dist[n]
    result = (dist[target] * 1000003 + (total % 1000000000)) % (1 << 63)
    sys.stderr.write(f"dist[{target}]={dist[target]} sumFinite={total}\n")
    print(f"RESULT={result}")


if __name__ == "__main__":
    main()
