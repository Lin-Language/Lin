// pipeline.js — range -> map -> filter -> reduce, materializing each stage via
// Array methods (no lazy fusion). The reduced sum (~1.3e12) is < 2^53 so it is
// exact in a Number; printed as an integer. Prints "RESULT=<int>".
'use strict';

const N = 20000000;

function main() {
  const a = new Array(N);
  for (let i = 0; i < N; i++) a[i] = i;
  const b = a.map((x) => x * 2);
  const c = b.filter((x) => x % 3 === 0);
  let total = 0;
  for (const x of c) total += x;
  console.log(`RESULT=${total}`);
}

main();
