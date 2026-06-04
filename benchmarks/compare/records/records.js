// records.js — record-access-bound stateful simulation. A single object threaded
// through field-read + reconstruct cycles. The transient product `a*1103515245`
// (~2.3e18) exceeds 2^53, so the State fields are BigInt; the final sum is printed
// as a decimal string. Prints "RESULT=<int>".
//
// Parameters (identical across all languages): N=50000000, MOD=2147483647.
'use strict';

const N = 50000000;
const MOD = 2147483647n;

function step(s) {
  const a = (s.a * 1103515245n + s.f + 12345n) % MOD;
  const b = (s.b + s.a * 3n) % MOD;
  const c = (s.c * 5n + s.b) % MOD;
  const d = (s.d + s.c * 7n) % MOD;
  const e = (s.e * 9n + s.d) % MOD;
  const f = (s.f + s.e * 11n) % MOD;
  return { a, b, c, d, e, f };
}

function main() {
  let s = { a: 1n, b: 2n, c: 3n, d: 4n, e: 5n, f: 6n };
  for (let i = 0; i < N; i++) {
    s = step(s);
  }
  const sum = (s.a + s.b + s.c + s.d + s.e + s.f) % MOD;
  console.log(`RESULT=${sum.toString()}`);
}

main();
