# processes

A small **task runner**: run a sequence of named external commands as subprocesses,
classify each outcome (pass / fail / error), and print a summary report — the shape
of a "run my build & check steps" tool.

## What it demonstrates

- `std/process.exec` — run a command to completion and collect `{ status, stdout, stderr }`.
- Classifying a subprocess result into a tagged `TaskResult` (`pass` on exit 0, `fail`
  on non-zero, `error` when the command can't be launched — no crash).
- Recursion over a task list to run them in order.
- Separating impure I/O (`task.lin`, which spawns) from pure data→text (`report.lin`,
  which summarizes/renders) so the reporting logic unit-tests without subprocesses.

## Structure

- **`task.lin`** — `Task` / `TaskResult` types; `runTask` (spawn + classify) and `runAll`.
- **`report.lin`** — pure `summarize` (tally pass/fail) and `render` (format the report).
- **`main.lin`** — defines a few tasks, runs them, prints the report.
- **`task.test.lin`** — unit tests for `runTask`/`runAll` with `std/process.exec`
  mocked (ADR-046), so classification (pass / fail / could-not-launch) is tested
  deterministically without spawning anything or depending on host binaries.
- **`report.test.lin`** — pure unit tests for `summarize`/`render` over synthetic results.
- **`integration.test.lin`** — end-to-end: a mixed task list through the whole
  pipeline (also mocking `exec`) asserting the rolled-up summary and rendered report.

## Run / Test

```bash
lin run examples/processes/main.lin
lin test examples/processes/
```
