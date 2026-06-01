// async_io.go — I/O-bound concurrency: 200 goroutines bounded to 50 in flight
// via a buffered channel semaphore; each sleeps 50ms then returns i*2+1. Sum
// accumulated under a mutex. Prints exactly one stdout line "RESULT=<int>".
package main

import (
	"fmt"
	"sync"
	"time"
)

const (
	tasks       = 200
	sleepMS     = 50
	concurrency = 50
)

func main() {
	sem := make(chan struct{}, concurrency)
	var wg sync.WaitGroup
	var mu sync.Mutex
	var total int64 = 0

	for i := 0; i < tasks; i++ {
		wg.Add(1)
		go func(idx int) {
			defer wg.Done()
			sem <- struct{}{}        // acquire (blocks once 50 are in flight)
			defer func() { <-sem }() // release
			time.Sleep(sleepMS * time.Millisecond)
			v := int64(idx)*2 + 1
			mu.Lock()
			total += v
			mu.Unlock()
		}(i)
	}

	wg.Wait()
	fmt.Printf("RESULT=%d\n", total)
}
