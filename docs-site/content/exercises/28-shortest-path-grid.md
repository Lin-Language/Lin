# 28 · Shortest Path in a Grid

A robot must navigate a warehouse floor from the top-left corner to the bottom-right corner. Some cells contain obstacles. The robot moves in four directions (up, down, left, right). Find the shortest path, measured in cells visited.

## Your task

Implement `solve` in `exercises/28-shortest-path-grid/exercise.lin`:

```lin
export val solve = (grid: Int32[][]): Int32 => ...
```

`grid[r][c]` is `0` (open) or `1` (wall). Find the shortest path from `grid[0][0]` to `grid[rows-1][cols-1]`. Return the number of cells on the path (start and end inclusive), or `-1` if no path exists or if the start or end cell is a wall.

## Examples

```lin
solve([[0,0,0],[0,1,0],[0,0,0]])   // 5
solve([[0,1],[1,0]])               // -1
solve([[0,0],[0,0]])               // 3
solve([[0]])                       // 1
```

## Run the test

```bash
lin test docs-site/exercises/28-shortest-path-grid/
```

Hints: BFS guarantees the shortest path in an unweighted graph. Use an `Int32[][]` array as a queue with a `head` index (no shifting needed — just increment the pointer). Encode visited cells as a `{ Int32: Boolean }` map keyed by `r * cols + c` to avoid visiting the same cell twice.
