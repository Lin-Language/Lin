# Exercises

Thirty puzzles for learning Lin by doing — Advent-of-Code style. They begin with one-liners
and climb steadily to the kind of problems asked in Google and Facebook interviews: sliding
windows, dynamic programming, graph search, Dijkstra.

Each puzzle is a small, runnable Lin module. You're given a function called `solve` with a
stub body and a unit test that fails. Fill in the body until the test passes. A reference
solution ships alongside each one (try to solve it before you peek) and proves the puzzle is
solvable.

## How it works

Every puzzle lives in `docs-site/exercises/NN-slug/`:

- `exercise.lin` — the function you complete (starts as a deliberately-wrong stub).
- `exercise.test.lin` — the unit test. Run it: `lin test docs-site/exercises/NN-slug/`.
- `spec.lin` — the shared assertions (you don't edit this).
- `solution.lin` / `solution.test.lin` — the reference answer and its test.

Replace the stub in `exercise.lin`, then run the test until it's green. See the
[exercises README](https://github.com/lin-language/Lin/tree/master/docs-site/exercises) for
the full workflow.

## The puzzles

They are numbered `01`–`30` in increasing difficulty — work through them in order, or jump
to whatever looks fun. Each page below gives the story, the exact input/output contract,
worked examples, and a hint or two (but never the answer).

1. **Sum a List** — fold a list of numbers to its total.
2. **FizzBuzz** — the classic warm-up.
3. **Reverse a String** — characters back-to-front.
4. **Count the Vowels** — tally a, e, i, o, u.
5. **Largest in the List** — max, or null when empty.
6. **Palindrome Check** — same forwards and backwards.
7. **Factorial** — n! with a loop.
8. **Word Frequencies** — build a count map.
9. **Keep the Evens** — filter a list.
10. **Greatest Common Divisor** — Euclid's algorithm.
11. **Two Sum** — indices that add to a target (hash map).
12. **Binary Search** — find an element in a sorted array.
13. **Merge Two Sorted Arrays** — the merge step of merge sort.
14. **Group Anagrams** — bucket words by their sorted letters.
15. **Roman Numerals to Integer** — parse a Roman numeral.
16. **Valid Parentheses** — balanced brackets with a stack.
17. **Run-Length Encoding** — compress runs of characters.
18. **Rotate a Matrix** — turn an N×N grid 90°.
19. **Climbing Stairs** — count the ways (it's Fibonacci).
20. **Caesar Cipher** — shift the alphabet.
21. **Longest Substring Without Repeating Characters** — sliding window.
22. **Merge Intervals** — coalesce overlapping ranges.
23. **Top K Frequent Elements** — count, then rank.
24. **Coin Change** — fewest coins for an amount (DP).
25. **Longest Common Subsequence** — the 2-D DP classic.
26. **Edit Distance** — Levenshtein between two strings.
27. **Word Break** — segment a string from a dictionary.
28. **Shortest Path in a Grid** — BFS across a maze.
29. **Shortest Paths (Dijkstra)** — weighted shortest paths.
30. **Trapping Rain Water** — how much water the bars hold.
