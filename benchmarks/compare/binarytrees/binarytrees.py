# binarytrees.py — allocation churn (Computer Language Benchmarks Game "binary-trees").
# Bottom-up allocate many short-lived 2-field tree nodes, traverse each to a node
# count, and free them. A node is a (left, right) tuple; a leaf is (None, None).
# Prints exactly one stdout line "RESULT=<int>".
#
# RESULT = stretchCheck + (sum of all iteration checks) + longLivedCheck.
# Parameters (identical across all languages): MIN_DEPTH=4, MAX_DEPTH=16.
import sys

MIN_DEPTH = 4
MAX_DEPTH = 16
sys.setrecursionlimit(100000)


def make(d):
    if d == 0:
        return (None, None)
    return (make(d - 1), make(d - 1))


def check(t):
    l, r = t
    if l is None:
        return 1
    return 1 + check(l) + check(r)


def main():
    max_depth = max(MAX_DEPTH, MIN_DEPTH + 2)
    stretch_check = check(make(max_depth + 1))
    long_lived = make(max_depth)

    total = stretch_check
    for depth in range(MIN_DEPTH, max_depth + 1, 2):
        iterations = 1 << (max_depth - depth + MIN_DEPTH)
        s = 0
        for _ in range(iterations):
            s += check(make(depth))
        total += s

    total += check(long_lived)
    print(f"RESULT={total}")


if __name__ == "__main__":
    main()
