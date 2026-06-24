// binarytrees.go — allocation / GC churn (Computer Language Benchmarks Game
// "binary-trees"). Bottom-up allocate many short-lived 2-pointer structs, traverse
// each to a node count, and let the GC reclaim them. Prints exactly one stdout
// line "RESULT=<int>".
//
// RESULT = stretchCheck + (sum of all iteration checks) + longLivedCheck.
// Parameters (identical across all languages): MIN_DEPTH=4, MAX_DEPTH=16.
package main

import "fmt"

const (
	minDepth = 4
	maxDepthC = 16
)

type Tree struct {
	l, r *Tree
}

func makeTree(d int) *Tree {
	if d == 0 {
		return &Tree{nil, nil}
	}
	return &Tree{makeTree(d - 1), makeTree(d - 1)}
}

func check(t *Tree) int64 {
	if t.l == nil {
		return 1
	}
	return 1 + check(t.l) + check(t.r)
}

func main() {
	maxDepth := maxDepthC
	if minDepth+2 > maxDepth {
		maxDepth = minDepth + 2
	}
	stretchCheck := check(makeTree(maxDepth + 1))
	longLived := makeTree(maxDepth)

	total := stretchCheck
	for depth := minDepth; depth <= maxDepth; depth += 2 {
		iterations := 1 << (maxDepth - depth + minDepth)
		var s int64
		for i := 0; i < iterations; i++ {
			s += check(makeTree(depth))
		}
		total += s
	}

	total += check(longLived)
	fmt.Printf("RESULT=%d\n", total)
}
