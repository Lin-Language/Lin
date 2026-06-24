// revcomp.go — byte-buffer throughput (Computer Language Benchmarks Game
// "reverse-complement", checksum form). A deterministic Park-Miller MINSTD
// generator fills an N-base ACGT []byte; it is reverse-complemented (A<->T, C<->G,
// read back-to-front) into a second buffer; then a rolling checksum is folded over
// the result. Prints exactly one stdout line "RESULT=<int>".
//
// RESULT = fold h = (h*31 + code) mod 1000000007 over the reverse-complement.
// Parameters (identical across all languages): N=20000000.
package main

import "fmt"

const n = 20000000

func main() {
	codes := [4]byte{65, 67, 71, 84}
	var comp [128]byte
	comp[65] = 84
	comp[84] = 65
	comp[67] = 71
	comp[71] = 67

	state := int64(42)
	seq := make([]byte, n)
	for i := 0; i < n; i++ {
		state = (state * 16807) % 2147483647
		seq[i] = codes[state%4]
	}

	out := make([]byte, n)
	for i := 0; i < n; i++ {
		out[i] = comp[seq[n-1-i]]
	}

	var h int64
	for j := 0; j < n; j++ {
		h = (h*31 + int64(out[j])) % 1000000007
	}
	fmt.Printf("RESULT=%d\n", h)
}
