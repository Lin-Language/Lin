# 29 · Shortest Paths (Dijkstra)

A mapping application needs to compute the shortest driving time from one city to all others. Roads are bidirectional and have non-negative travel times. Implement Dijkstra's algorithm.

## Your task

Implement `solve` in `exercises/29-dijkstra/exercise.lin`:

```lin
export val solve = (n: Int32, edges: Int32[][], src: Int32): Int32[] => ...
```

`n` is the number of nodes (numbered `0` to `n-1`). Each edge is `[u, v, w]` representing an undirected edge with weight `w ≥ 0`. Return an array `dist` of length `n` where `dist[i]` is the shortest distance from `src` to node `i`, or `-1` if node `i` is unreachable.

## Examples

```lin
solve(4, [[0,1,1],[1,2,2],[0,2,4],[2,3,1]], 0)   // [0,1,3,4]
solve(3, [[0,1,5]], 0)                            // [0,5,-1]
solve(1, [], 0)                                   // [0]
```

## Run the test

```bash
lin test docs-site/exercises/29-dijkstra/
```

Hints: build an adjacency list as a `{ Int32: Int32[][] }` map. Use a flat `Int32[]` for distances (initialised to a large sentinel) and a `Boolean[]` for the visited set. In each of `n` iterations, find the unvisited node with the smallest distance, mark it visited, then relax all its neighbours.
