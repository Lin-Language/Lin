// records.go — record-access-bound stateful simulation. A single value-semantics
// struct threaded through field-read + reconstruct cycles. Prints "RESULT=<int>".
//
// Parameters (identical across all languages): N=50000000, MOD=2147483647.
package main

import "fmt"

const n = 50000000
const mod = 2147483647

type State struct {
	a, b, c, d, e, f int64
}

func step(s State) State {
	// Each pre-mod product (e.g. a*1103515245 ~ 2.3e18) fits int64; % brings it
	// back under 2^31 so the next multiply stays in range.
	a := (s.a*1103515245 + s.f + 12345) % mod
	b := (s.b + s.a*3) % mod
	c := (s.c*5 + s.b) % mod
	d := (s.d + s.c*7) % mod
	e := (s.e*9 + s.d) % mod
	f := (s.f + s.e*11) % mod
	return State{a, b, c, d, e, f}
}

func main() {
	s := State{1, 2, 3, 4, 5, 6}
	for i := int64(0); i < n; i++ {
		s = step(s)
	}
	sum := (s.a + s.b + s.c + s.d + s.e + s.f) % mod
	fmt.Printf("RESULT=%d\n", sum)
}
