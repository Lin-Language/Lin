// async_io.js — I/O-bound concurrency: 200 promises wrapping setTimeout(50),
// throttled to 50 in flight via a simple promise pool. Each resolves to i*2+1;
// sum with reduce. Prints exactly one stdout line "RESULT=<int>".
'use strict';

const TASKS = 200;
const SLEEP_MS = 50;
const CONCURRENCY = 50;

function task(i) {
  return new Promise((resolve) => {
    setTimeout(() => resolve(i * 2 + 1), SLEEP_MS);
  });
}

async function main() {
  let next = 0;
  const results = new Array(TASKS);

  // A worker pulls the next index until the work is exhausted; CONCURRENCY
  // workers run concurrently, so at most 50 timers are in flight at once. Each
  // result is stored by index (no shared-accumulator read/write race across the
  // await boundary), then summed once at the end.
  async function worker() {
    while (true) {
      const i = next++;
      if (i >= TASKS) return;
      results[i] = await task(i);
    }
  }

  const workers = [];
  for (let k = 0; k < CONCURRENCY; k++) workers.push(worker());
  await Promise.all(workers);

  let total = 0;
  for (const r of results) total += r;
  console.log(`RESULT=${total}`);
}

main();
