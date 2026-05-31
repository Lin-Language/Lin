// parallel.js — CPU-bound fan-out via worker_threads: 8 Workers each run the
// same walk, results summed with Promise.all. The file is both the main module
// and the worker (re-required via isMainThread). BigInt for the 64-bit sum.
// Prints exactly one stdout line "RESULT=<int>".
'use strict';
const { Worker, isMainThread, parentPort } = require('worker_threads');

const START = 27;
const ITERS = 30000000;
const CHUNKS = 8;

function chunk() {
  let start = START;
  let n = ITERS;
  let steps = 0n;
  while (n !== 0) {
    let next;
    if (start === 1) next = 27;
    else if (start % 2 === 0) next = (start / 2) | 0;
    else next = 3 * start + 1;
    steps += BigInt(start);
    start = next;
    n -= 1;
  }
  return steps;
}

if (!isMainThread) {
  // Worker: compute one chunk and post it back as a string (BigInt isn't a
  // structured-clone type, so transfer it as a decimal string).
  parentPort.postMessage(chunk().toString());
} else {
  function main() {
    const promises = [];
    for (let i = 0; i < CHUNKS; i++) {
      promises.push(
        new Promise((resolve, reject) => {
          const w = new Worker(__filename);
          w.once('message', (m) => resolve(BigInt(m)));
          w.once('error', reject);
        }),
      );
    }
    Promise.all(promises).then((results) => {
      let sum = 0n;
      for (const r of results) sum += r;
      console.log(`RESULT=${sum}`);
    });
  }
  main();
}
