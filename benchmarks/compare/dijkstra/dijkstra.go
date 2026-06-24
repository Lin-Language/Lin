// dijkstra.go — linear-scan priority-queue Dijkstra (O(V^2)) over an in-code graph.
// Graph generated in memory by a portable deterministic generator (no file I/O).
// Prints exactly one stdout line "RESULT=<int>". Generator: Park-Miller MINSTD.
package main

import "fmt"

const n = 30000
const INF = 1000000000

type edge struct{ to, w int }

func main() {
	state := int64(1234)
	nxt := func() int64 { state = (state * 16807) % 2147483647; return state }

	adj := make([][]edge, n)
	for i := 0; i < n; i++ {
		for d := 1; d <= 8; d++ {
			j := i + d
			if j < n {
				w := int(nxt()%100) + 1
				adj[i] = append(adj[i], edge{j, w})
			}
		}
		if i+1 < n {
			r := nxt()
			if r%10 < 3 {
				span := int64(n - (i + 1))
				j := (i + 1) + int(nxt()%span)
				w := int(nxt()%100) + 1
				adj[i] = append(adj[i], edge{j, w})
			}
		}
	}

	dist := make([]int, n)
	visited := make([]bool, n)
	for i := range dist {
		dist[i] = INF
	}
	dist[0] = 0
	capacity := n*9 + 1
	pqn := make([]int, capacity)
	pqd := make([]int, capacity)
	pql := 1
	for pql > 0 {
		mi := 0
		for j := 1; j < pql; j++ {
			if pqd[j] < pqd[mi] {
				mi = j
			}
		}
		u := pqn[mi]
		last := pql - 1
		pqn[mi] = pqn[last]
		pqd[mi] = pqd[last]
		pql = last
		if !visited[u] {
			visited[u] = true
			du := dist[u]
			for _, e := range adj[u] {
				nd := du + e.w
				if nd < dist[e.to] {
					dist[e.to] = nd
					pqn[pql] = e.to
					pqd[pql] = nd
					pql++
				}
			}
		}
	}
	var total int64
	for k := 0; k < n; k++ {
		if dist[k] < INF {
			total += int64(dist[k])
		}
	}
	chk := int64(dist[n-1])*1000003 + (total % 1000000000)
	fmt.Printf("RESULT=%d\n", chk)
}
