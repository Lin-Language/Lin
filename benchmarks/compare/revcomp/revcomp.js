'use strict';
// revcomp.js — byte-buffer throughput (Computer Language Benchmarks Game
// "reverse-complement", checksum form). A deterministic Park-Miller MINSTD
// generator fills an N-base ACGT Uint8Array; it is reverse-complemented (A<->T,
// C<->G, read back-to-front) into a second buffer; then a rolling checksum is
// folded over the result. All arithmetic stays within Number's safe range.
// Prints exactly one stdout line "RESULT=<int>".
//
// RESULT = fold h = (h*31 + code) mod 1000000007 over the reverse-complement.
// Parameters (identical across all languages): N=20000000.
const N = 20000000;

function main() {
  const codes = [65, 67, 71, 84];
  const comp = new Int32Array(128);
  comp[65] = 84;
  comp[84] = 65;
  comp[67] = 71;
  comp[71] = 67;

  let state = 42;
  const seq = new Uint8Array(N);
  for (let i = 0; i < N; i++) {
    state = (state * 16807) % 2147483647;
    seq[i] = codes[state % 4];
  }

  const out = new Uint8Array(N);
  for (let i = 0; i < N; i++) out[i] = comp[seq[N - 1 - i]];

  let h = 0;
  for (let j = 0; j < N; j++) h = (h * 31 + out[j]) % 1000000007;
  console.log(`RESULT=${h}`);
}

main();
