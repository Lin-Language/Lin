// dijkstra.js — linear-scan priority-queue Dijkstra (O(V^2)) over graph.json.
// Reads + parses the graph INSIDE the timed region. Prints exactly one stdout
// line "RESULT=<int>"; everything else to stderr. BigInt for the 64-bit math.
'use strict';
const fs = require('fs');
const path = require('path');

const INF = 1000000000;
const graphPath = path.join(__dirname, '..', 'data', 'graph.json');

function main() {
  const graph = JSON.parse(fs.readFileSync(graphPath, 'utf8'));
  const nodes = graph.nodes;
  const source = 'n0';
  const target = nodes[nodes.length - 1];

  const adj = new Map();
  for (const e of graph.edges) {
    if (!adj.has(e.from)) adj.set(e.from, []);
    adj.get(e.from).push([e.to, e.weight]);
  }

  const dist = new Map();
  for (const n of nodes) dist.set(n, INF);
  dist.set(source, 0);
  const visited = new Set();

  // Linear-scan priority queue: an array of [node, dist] entries.
  let pq = [[source, 0]];
  while (pq.length > 0) {
    let minIdx = 0;
    for (let i = 0; i < pq.length; i++) {
      if (pq[i][1] < pq[minIdx][1]) minIdx = i;
    }
    const [u] = pq[minIdx];
    pq.splice(minIdx, 1);
    if (visited.has(u)) continue;
    visited.add(u);
    const neighbors = adj.get(u) || [];
    for (const [v, w] of neighbors) {
      const nd = dist.get(u) + w;
      if (nd < dist.get(v)) {
        dist.set(v, nd);
        pq.push([v, nd]);
      }
    }
  }

  let total = 0n;
  for (const n of nodes) {
    const d = dist.get(n);
    if (d < INF) total += BigInt(d);
  }
  const result = BigInt(dist.get(target)) * 1000003n + (total % 1000000000n);
  process.stderr.write(`dist[${target}]=${dist.get(target)} sumFinite=${total}\n`);
  console.log(`RESULT=${result}`);
}

main();
