'use strict';
// dijkstra.js — linear-scan priority-queue Dijkstra (O(V^2)) over an in-code graph.
// Graph generated in memory by a portable deterministic generator (no file I/O).
// Prints exactly one stdout line "RESULT=<int>". Generator: Park-Miller MINSTD.
const N = 30000;
const INF = 1000000000;

function main() {
  let state = 1234;
  const adj = new Array(N);
  for (let i = 0; i < N; i++) adj[i] = [];
  for (let i = 0; i < N; i++) {
    for (let d = 1; d <= 8; d++) {
      const j = i + d;
      if (j < N) {
        state = (state * 16807) % 2147483647;
        const w = state % 100 + 1;
        adj[i].push([j, w]);
      }
    }
    if (i + 1 < N) {
      state = (state * 16807) % 2147483647;
      if (state % 10 < 3) {
        const span = N - (i + 1);
        state = (state * 16807) % 2147483647;
        const j = (i + 1) + (state % span);
        state = (state * 16807) % 2147483647;
        const w = state % 100 + 1;
        adj[i].push([j, w]);
      }
    }
  }

  const dist = new Array(N).fill(INF);
  const visited = new Array(N).fill(false);
  dist[0] = 0;
  const cap = N * 9 + 1;
  const pqn = new Array(cap).fill(0);
  const pqd = new Array(cap).fill(0);
  let pql = 1;
  while (pql > 0) {
    let mi = 0;
    for (let j = 1; j < pql; j++) {
      if (pqd[j] < pqd[mi]) mi = j;
    }
    const u = pqn[mi];
    const last = pql - 1;
    pqn[mi] = pqn[last];
    pqd[mi] = pqd[last];
    pql = last;
    if (!visited[u]) {
      visited[u] = true;
      const du = dist[u];
      const nb = adj[u];
      for (let e = 0; e < nb.length; e++) {
        const v = nb[e][0];
        const nd = du + nb[e][1];
        if (nd < dist[v]) {
          dist[v] = nd;
          pqn[pql] = v;
          pqd[pql] = nd;
          pql++;
        }
      }
    }
  }
  let total = 0n;
  for (let k = 0; k < N; k++) {
    if (dist[k] < INF) total += BigInt(dist[k]);
  }
  const chk = BigInt(dist[N - 1]) * 1000003n + (total % 1000000000n);
  console.log(`RESULT=${chk}`);
}

main();
