# recursion.py — naive recursive fib + iterative sumTo. Prints "RESULT=<int>".
import sys

FIB_N = 38
SUM_N = 50000000

sys.setrecursionlimit(10000)


def fib(n):
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)


def sum_to(n):
    acc = 0
    i = 1
    while i <= n:
        acc += i
        i += 1
    return acc


def main():
    f = fib(FIB_N)
    s = sum_to(SUM_N)
    result = f * 1000000007 + s
    print(f"RESULT={result}")


if __name__ == "__main__":
    main()
