# processes

A realistic **task / build-step runner**. It reads its configuration from the
environment, runs a list of named external commands (build, lint, test, deploy …)
*concurrently*, gives slow and flaky steps a deadline and retries, and prints a
rolled-up report — the shape of a real "run my CI steps" tool. Every step's outcome is
classified (pass / fail / could-not-launch) and threaded through a Json-free generic
`Result`, so nothing crashes on a missing binary or a timeout.

This example consolidates what used to be three separate demos (a process runner, a
generic `Result`, and a concurrency tour) into one project where each primitive earns
its place in the runner's domain.

## What it demonstrates

- **Process execution** — `std/process.exec` runs a command to completion and returns
  `ExecResult | Error`. The union is narrowed immediately (`is Error` / `is ExecResult`),
  so no dynamic `Json` value ever leaks into the runner's own types.
- **The full concurrency surface** (`std/async`) doing real work:
  - `parallel` runs independent steps at once, with a `shared` + `withLock` progress log
    so completion is counted race-free across threads;
  - `timeout` gives a step a deadline — a miss becomes a `Result` *failure*, not a hang;
  - `retry` re-runs a flaky step (one that *traps*, not one that merely exits non-zero)
    up to N attempts, reporting "gave up after N attempts" if all fail;
  - `threadPool` / `poolAsync` run the steps on a bounded pool;
  - `async` / `await` (with `await`'s `T | Error` fault boundary) underpin the above.
- **A generic, Json-free `Result<T, E>`** (`outcome.lin`) with `ok` / `err` / `isOk` /
  `andThen` / `mapOk` / `unwrapOr`. The scheduler returns `Result<TaskResult, String>`;
  `main.lin` composes the combinators (`mapOk` + `unwrapOr`) into human status lines and
  chains a dependent step with `andThen` (deploy runs only if build met its deadline).
- **Environment-driven config** (`std/env`) — `getEnv` selects max-parallelism, retry
  budget and a CI flag, each narrowed from `String | Null` with documented defaults.
- **Precise types throughout** — named records (`Task`, `TaskResult`, `ExecResult`,
  `Config`, `RunReport`, `Summary`), a generic `Result<T, E>`, and unions; **no `Json`**
  in any of the example's own signatures.
- **Mocking pure logic away from I/O** (`replace`, ADR-046) — `exec` and `getEnv` are
  swapped for deterministic stubs in the tests, so concurrency, timeouts, retries and
  classification are all exercised hermetically (no real subprocesses, no host
  dependence). Pure data→text (`report.lin`) needs no mocking at all.

## Structure

- **`outcome.lin`** — the generic `Result<T, E>` tagged union and its combinators.
- **`task.lin`** — `Task` / `TaskResult` / (re-exported) `ExecResult`; `runTask`
  (spawn + classify, narrowing `ExecResult | Error`) and `runAll`.
- **`scheduler.lin`** — the execution engine: `runConcurrent`, `runWithTimeout`,
  `runWithRetry`, `runOnPool`, `runThen` over `std/async`.
- **`config.lin`** — `loadConfig` reads `maxParallel` / `retries` / `ci` from `std/env`.
- **`report.lin`** — pure `summarize` (tally pass / fail / errored) and `render`.
- **`main.lin`** — loads config, runs the pipeline concurrently, prints the report.
- **`*.test.lin`** — unit tests for each module plus an end-to-end `integration.test.lin`.

## Run / Test

```bash
lin run examples/processes/main.lin
PROCESSES_MAX_PARALLEL=2 PROCESSES_RETRIES=3 CI=1 lin run examples/processes/main.lin
lin test examples/processes/
lin test examples/processes/ --coverage
```

## Implementation notes / known compiler limitations

A few idioms in this project are shaped to avoid live compiler bugs hit while writing it.
They are documented here (rather than silently worked around) so they can be fixed:

1. **`async` thunks call top-level functions, not captured function-typed parameters.**
   `async(() => runner())` where `runner` is a captured function *parameter* runs the body
   without real concurrency, so `timeout` never trips. Calling the top-level `runTask`
   inside the thunk (and mocking its `exec`) is correct *and* keeps the scheduler testable.

2. **Result combinators that introduce a fresh result type (`andThen`, `mapOk`) bind their
   result to an annotated `val`, and `mapOk`'s test bodies are named lambdas.** A generic
   call whose return type parameter is only resolvable from the surrounding expectation
   fails to infer it when nested inside an inline anonymous thunk argument
   (`() => [ … mapOk(r, f) … ]`), leaking the return type as `?T | Null`. Binding to an
   annotated `val` and/or defining the test body as a named top-level lambda fixes it.

3. **`outcome.lin` coverage is under-reported.** `lin test --coverage` does not attribute a
   *monomorphized generic function* back to its source module, so `outcome.lin` (all
   generic) reports low line coverage even though every combinator and branch is tested.
   A trivial generic module reproduces this at 0% while its tests pass; the same generic
   exercised from a non-generic function *in the same module* reports 100%. The other four
   source files cover well above 80% normally.
