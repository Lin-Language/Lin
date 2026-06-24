'use strict';
// knucleotide.js — k-mer frequency count: Map (string keys) throughput + per-window
// substring. A deterministic Park-Miller MINSTD generator builds an N-base ACGT
// sequence; a sliding K-wide window is counted into a Map. Every intermediate stays
// within Number's safe-integer range (no BigInt). Prints "RESULT=<int>".
//
// RESULT = (sum over keys of count^2) + (number of distinct keys) — order-independent.
// Parameters (identical across all languages): N=4000000, K=8.
const N = 4000000;
const K = 8;
const CODES = "ACGT";

function main() {
  let state = 42;
  const chars = new Array(N);
  for (let i = 0; i < N; i++) {
    state = (state * 16807) % 2147483647;
    chars[i] = CODES[state % 4];
  }
  const seq = chars.join("");

  const counts = new Map();
  const end = N - K + 1;
  for (let i = 0; i < end; i++) {
    const key = seq.slice(i, i + K);
    counts.set(key, (counts.get(key) || 0) + 1);
  }

  let sumsq = 0;
  for (const v of counts.values()) sumsq += v * v;
  const result = sumsq + counts.size;
  console.log(`RESULT=${result}`);
}

main();
