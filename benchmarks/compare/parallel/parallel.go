// parallel.go — CPU-bound fan-out: 8 goroutines + sync.WaitGroup, results into
// a slice. Prints exactly one stdout line "RESULT=<int>".
package main

import (
	"fmt"
	"sync"
)

const (
	start  = 27
	iters  = 300000000
	chunks = 8
)

func walk() int64 {
	s := int32(start)
	n := int32(iters)
	var steps int64 = 0
	for n != 0 {
		var next int32
		if s == 1 {
			next = 27
		} else if s%2 == 0 {
			next = s / 2
		} else {
			next = 3*s + 1
		}
		steps += int64(s)
		s = next
		n--
	}
	return steps
}

func main() {
	results := make([]int64, chunks)
	var wg sync.WaitGroup
	for i := 0; i < chunks; i++ {
		wg.Add(1)
		go func(idx int) {
			defer wg.Done()
			results[idx] = walk()
		}(i)
	}
	wg.Wait()

	var sum int64 = 0
	for _, r := range results {
		sum += r
	}
	fmt.Printf("RESULT=%d\n", sum)
}
