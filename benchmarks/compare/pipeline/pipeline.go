// pipeline.go — range -> map -> filter -> reduce, materializing each stage into
// an explicit slice (no lazy fusion). Prints "RESULT=<int>".
//
// UNTESTED on the reference machine (no Go toolchain installed); written to be
// correct and to match the other languages' checksum.
package main

import "fmt"

const n = 2000000

func main() {
	a := make([]int64, n)
	for i := int64(0); i < n; i++ {
		a[i] = i
	}

	b := make([]int64, len(a))
	for i, x := range a {
		b[i] = x * 2
	}

	c := make([]int64, 0, len(b))
	for _, x := range b {
		if x%3 == 0 {
			c = append(c, x)
		}
	}

	var total int64 = 0
	for _, x := range c {
		total += x
	}
	fmt.Printf("RESULT=%d\n", total)
}
