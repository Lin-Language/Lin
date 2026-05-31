// recursion.go — naive recursive fib + iterative sumTo. Prints "RESULT=<int>".
//
// UNTESTED on the reference machine (no Go toolchain installed); written to be
// correct and to match the other languages' checksum.
package main

import "fmt"

const (
	fibN = 38
	sumN = 50000000
)

func fib(n int32) int64 {
	if n < 2 {
		return int64(n)
	}
	return fib(n-1) + fib(n-2)
}

func sumTo(n int64) int64 {
	var acc int64 = 0
	for i := int64(1); i <= n; i++ {
		acc += i
	}
	return acc
}

func main() {
	f := fib(fibN)
	s := sumTo(sumN)
	result := f*1000000007 + s
	fmt.Printf("RESULT=%d\n", result)
}
