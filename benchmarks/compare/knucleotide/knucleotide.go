// knucleotide.go — k-mer frequency count: map[string]int64 throughput + per-window
// substring. A deterministic Park-Miller MINSTD generator builds an N-base ACGT
// sequence; a sliding K-wide window is counted into a string-keyed map. Prints
// exactly one stdout line "RESULT=<int>".
//
// RESULT = (sum over keys of count^2) + (number of distinct keys) — order-independent.
// Parameters (identical across all languages): N=4000000, K=8.
package main

import "fmt"

const (
	n     = 4000000
	k     = 8
	codes = "ACGT"
)

func main() {
	state := int64(42)
	buf := make([]byte, n)
	for i := 0; i < n; i++ {
		state = (state * 16807) % 2147483647
		buf[i] = codes[state%4]
	}
	seq := string(buf)

	counts := make(map[string]int64)
	end := n - k + 1
	for i := 0; i < end; i++ {
		counts[seq[i:i+k]]++
	}

	var sumsq int64
	for _, v := range counts {
		sumsq += v * v
	}
	result := sumsq + int64(len(counts))
	fmt.Printf("RESULT=%d\n", result)
}
