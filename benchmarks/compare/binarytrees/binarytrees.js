'use strict';
// binarytrees.js — allocation / GC churn (Computer Language Benchmarks Game
// "binary-trees"). Bottom-up allocate many short-lived 2-field objects, traverse
// each to a node count, and let the GC reclaim them. All counts stay within
// Number's safe-integer range (no BigInt). Prints "RESULT=<int>".
//
// RESULT = stretchCheck + (sum of all iteration checks) + longLivedCheck.
// Parameters (identical across all languages): MIN_DEPTH=4, MAX_DEPTH=16.
const MIN_DEPTH = 4;
const MAX_DEPTH = 16;

function make(d) {
  if (d === 0) return { l: null, r: null };
  return { l: make(d - 1), r: make(d - 1) };
}

function check(t) {
  if (t.l === null) return 1;
  return 1 + check(t.l) + check(t.r);
}

function main() {
  const maxDepth = Math.max(MAX_DEPTH, MIN_DEPTH + 2);
  const stretchCheck = check(make(maxDepth + 1));
  const longLived = make(maxDepth);

  let total = stretchCheck;
  for (let depth = MIN_DEPTH; depth <= maxDepth; depth += 2) {
    const iterations = 1 << (maxDepth - depth + MIN_DEPTH);
    let s = 0;
    for (let i = 0; i < iterations; i++) s += check(make(depth));
    total += s;
  }

  total += check(longLived);
  console.log(`RESULT=${total}`);
}

main();
