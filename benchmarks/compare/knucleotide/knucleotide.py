# knucleotide.py — k-mer frequency count: hash-map (dict) throughput + per-window
# substring allocation. A deterministic Park-Miller MINSTD generator builds an
# N-base ACGT sequence; a sliding K-wide window is counted into a string-keyed dict.
# Prints exactly one stdout line "RESULT=<int>".
#
# RESULT = (sum over keys of count^2) + (number of distinct keys) — order-independent.
# Parameters (identical across all languages): N=4000000, K=8.
N = 4000000
K = 8
CODES = "ACGT"


def main():
    state = 42
    chars = [""] * N
    for i in range(N):
        state = (state * 16807) % 2147483647
        chars[i] = CODES[state % 4]
    seq = "".join(chars)

    counts = {}
    end = N - K + 1
    for i in range(end):
        key = seq[i:i + K]
        counts[key] = counts.get(key, 0) + 1

    sumsq = 0
    for v in counts.values():
        sumsq += v * v
    result = sumsq + len(counts)
    print(f"RESULT={result}")


if __name__ == "__main__":
    main()
