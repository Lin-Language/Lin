# records.py — record-access-bound stateful simulation. A small __slots__ class
# (the fair analog of a record) threaded through field-read + reconstruct cycles.
# Prints "RESULT=<int>".
#
# Parameters (identical across all languages): N=50000000, MOD=2147483647.
N = 50000000
MOD = 2147483647


class State:
    __slots__ = ("a", "b", "c", "d", "e", "f")

    def __init__(self, a, b, c, d, e, f):
        self.a = a
        self.b = b
        self.c = c
        self.d = d
        self.e = e
        self.f = f


def step(s):
    a = (s.a * 1103515245 + s.f + 12345) % MOD
    b = (s.b + s.a * 3) % MOD
    c = (s.c * 5 + s.b) % MOD
    d = (s.d + s.c * 7) % MOD
    e = (s.e * 9 + s.d) % MOD
    f = (s.f + s.e * 11) % MOD
    return State(a, b, c, d, e, f)


def main():
    s = State(1, 2, 3, 4, 5, 6)
    for _ in range(N):
        s = step(s)
    result = (s.a + s.b + s.c + s.d + s.e + s.f) % MOD
    print(f"RESULT={result}")


if __name__ == "__main__":
    main()
