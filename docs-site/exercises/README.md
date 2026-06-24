# Lin Exercises

Thirty Advent-of-Code-style puzzles for learning Lin by doing. They start easy and get
progressively harder — by the end you're solving Google/Facebook-interview-level problems
(sliding windows, dynamic programming, BFS, Dijkstra).

Each puzzle lives in its own numbered folder and is a real, runnable Lin module:

```
NN-slug/
  exercise.lin        # the function you complete — starts as a stub
  exercise.test.lin   # the unit test you make pass (run this)
  spec.lin            # the shared assertions (you don't need to edit this)
  solution.lin        # reference answer (the spoiler — try not to peek!)
  solution.test.lin   # proves the puzzle is solvable
```

## How to solve a puzzle

1. Open `NN-slug/exercise.lin` and read the `// TODO`. The matching write-up — story,
   examples, hints — is at `/exercises/NN-slug` in the docs site.
2. Replace the stub body of `solve` with your implementation.
3. Run its test until it's green:

   ```bash
   lin test docs-site/exercises/NN-slug/
   ```

Out of the box `exercise.test.lin` **fails** (the stub is deliberately wrong) and
`solution.test.lin` **passes** (the reference answer is correct). Your job is to make
`exercise.test.lin` pass too.

Run the whole set at once with `lin test docs-site/exercises/` — but you'll see every
unsolved exercise reported as failing, which is expected.
