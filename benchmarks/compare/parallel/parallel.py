# parallel.py — CPU-bound fan-out via multiprocessing (NOT threading: the GIL
# would serialize CPU-bound threads). 8 worker processes each run the same walk.
# Prints exactly one stdout line "RESULT=<int>".
import sys

START = 27
ITERS = 300000000
CHUNKS = 8


def chunk(_):
    start = START
    n = ITERS
    steps = 0
    while n != 0:
        if start == 1:
            nxt = 27
        elif start % 2 == 0:
            nxt = start // 2
        else:
            nxt = 3 * start + 1
        steps += start
        start = nxt
        n -= 1
    return steps


def main():
    from multiprocessing import Pool
    with Pool(CHUNKS) as p:
        results = p.map(chunk, range(CHUNKS))
    total = sum(results)
    print(f"RESULT={total}")


if __name__ == "__main__":
    main()
