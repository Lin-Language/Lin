// dijkstra.go — linear-scan priority-queue Dijkstra (O(V^2)) over graph.txt.
//
// Reads the line-based graph (no JSON library, single `go build`) INSIDE the
// timed region. Prints exactly one stdout line "RESULT=<int>"; logs to stderr.
//
// UNTESTED on the reference machine (no Go toolchain installed); written to be
// correct and to match the other languages' checksum.
package main

import (
	"bufio"
	"fmt"
	"os"
	"strconv"
	"strings"
)

const inf int64 = 1000000000

type edge struct {
	to int
	w  int64
}

func main() {
	path := "benchmarks/compare/data/graph.txt"
	f, err := os.Open(path)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	defer f.Close()

	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 0, 1024*1024), 1024*1024)

	idOf := make(map[string]int)
	var adj [][]edge

	intern := func(name string) int {
		if id, ok := idOf[name]; ok {
			return id
		}
		id := len(adj)
		idOf[name] = id
		adj = append(adj, nil)
		return id
	}

	// Header: "<numNodes> <source> <target>".
	sc.Scan()
	header := strings.Fields(sc.Text())
	source := header[1]
	target := header[2]
	srcID := intern(source)
	tgtID := intern(target)

	for sc.Scan() {
		line := sc.Text()
		if line == "" {
			continue
		}
		parts := strings.Fields(line)
		from := intern(parts[0])
		to := intern(parts[1])
		w, _ := strconv.ParseInt(parts[2], 10, 64)
		adj[from] = append(adj[from], edge{to: to, w: w})
	}

	n := len(adj)
	dist := make([]int64, n)
	visited := make([]bool, n)
	for i := range dist {
		dist[i] = inf
	}
	dist[srcID] = 0

	// Linear-scan priority queue: a slice of [node, dist] entries.
	type entry struct {
		node int
		dist int64
	}
	pq := []entry{{node: srcID, dist: 0}}
	for len(pq) > 0 {
		minIdx := 0
		for i := range pq {
			if pq[i].dist < pq[minIdx].dist {
				minIdx = i
			}
		}
		u := pq[minIdx].node
		// swap-remove
		pq[minIdx] = pq[len(pq)-1]
		pq = pq[:len(pq)-1]
		if visited[u] {
			continue
		}
		visited[u] = true
		for _, e := range adj[u] {
			nd := dist[u] + e.w
			if nd < dist[e.to] {
				dist[e.to] = nd
				pq = append(pq, entry{node: e.to, dist: nd})
			}
		}
	}

	var total int64 = 0
	for _, d := range dist {
		if d < inf {
			total += d
		}
	}
	result := dist[tgtID]*1000003 + (total % 1000000000)
	fmt.Fprintf(os.Stderr, "dist[%s]=%d sumFinite=%d\n", target, dist[tgtID], total)
	fmt.Printf("RESULT=%d\n", result)
}
