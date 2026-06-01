// recursion.js — naive recursive fib + iterative sumTo. BigInt for the final
// 64-bit fold (sumTo overflows Number's safe-integer range). Prints "RESULT=<int>".
'use strict';

const FIB_N = 42;
const SUM_N = 50000000;

function fib(n) {
  if (n < 2) return n;
  return fib(n - 1) + fib(n - 2);
}

function sumTo(n) {
  let acc = 0n;
  for (let i = 1n; i <= n; i++) acc += i;
  return acc;
}

function main() {
  const f = BigInt(fib(FIB_N));
  const s = sumTo(BigInt(SUM_N));
  const result = f * 1000000007n + s;
  console.log(`RESULT=${result}`);
}

main();
