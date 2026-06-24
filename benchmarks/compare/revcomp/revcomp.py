# revcomp.py — byte-buffer throughput (Computer Language Benchmarks Game
# "reverse-complement", checksum form). A deterministic Park-Miller MINSTD generator
# fills an N-base ACGT bytearray; it is reverse-complemented (A<->T, C<->G, read
# back-to-front) into a second bytearray; then a rolling checksum is folded over the
# result. Prints exactly one stdout line "RESULT=<int>".
#
# RESULT = fold h = (h*31 + code) mod 1000000007 over the reverse-complement.
# Parameters (identical across all languages): N=20000000.
N = 20000000
CODES = b"ACGT"

COMP = [0] * 128
COMP[65] = 84
COMP[84] = 65
COMP[67] = 71
COMP[71] = 67


def main():
    state = 42
    seq = bytearray(N)
    for i in range(N):
        state = (state * 16807) % 2147483647
        seq[i] = CODES[state % 4]

    out = bytearray(N)
    for i in range(N):
        out[i] = COMP[seq[N - 1 - i]]

    h = 0
    for j in range(N):
        h = (h * 31 + out[j]) % 1000000007
    print(f"RESULT={h}")


if __name__ == "__main__":
    main()
