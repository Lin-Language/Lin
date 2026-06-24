# dijkstra.py — linear-scan priority-queue Dijkstra (O(V^2)) over an in-code graph.
#
# The graph is GENERATED IN MEMORY by a portable deterministic generator (identical
# across all languages), not loaded from a file — so the timed region is the
# algorithm itself plus a fast O(N+E) build, with no file I/O or parse cost. Prints
# exactly one stdout line "RESULT=<int>".
#
# Generator: Park-Miller MINSTD state = state*16807 mod 2147483647 (seed 1234).
N = 30000
INF = 1000000000


def main():
    state = 1234
    adj = [[] for _ in range(N)]
    for i in range(N):
        for d in range(1, 9):
            j = i + d
            if j < N:
                state = (state * 16807) % 2147483647
                w = state % 100 + 1
                adj[i].append((j, w))
        if i + 1 < N:
            state = (state * 16807) % 2147483647
            if state % 10 < 3:
                span = N - (i + 1)
                state = (state * 16807) % 2147483647
                j = (i + 1) + (state % span)
                state = (state * 16807) % 2147483647
                w = state % 100 + 1
                adj[i].append((j, w))

    dist = [INF] * N
    visited = [False] * N
    dist[0] = 0
    cap = N * 9 + 1
    pqn = [0] * cap
    pqd = [0] * cap
    pql = 1
    while pql > 0:
        mi = 0
        for j in range(1, pql):
            if pqd[j] < pqd[mi]:
                mi = j
        u = pqn[mi]
        last = pql - 1
        pqn[mi] = pqn[last]
        pqd[mi] = pqd[last]
        pql = last
        if not visited[u]:
            visited[u] = True
            du = dist[u]
            for (v, w) in adj[u]:
                nd = du + w
                if nd < dist[v]:
                    dist[v] = nd
                    pqn[pql] = v
                    pqd[pql] = nd
                    pql += 1
    total = sum(d for d in dist if d < INF)
    chk = dist[N - 1] * 1000003 + (total % 1000000000)
    print(f"RESULT={chk}")


if __name__ == "__main__":
    main()
