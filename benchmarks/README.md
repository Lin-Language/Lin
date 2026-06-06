# Lin benchmarks

End-to-end runtime benchmarks for **compiled Lin programs**. Each `*.lin` here is
built with the current `lin` binary and the resulting native executable is timed.
This measures the quality of the emitted code (codegen, RC, runtime), not the
speed of the Rust compiler — for that, use `cargo bench` (none wired up yet).

## Running

```bash
benchmarks/run.sh                 # build lin (release), run every benchmark
benchmarks/run.sh recursion       # only benchmarks whose name matches "recursion"
RUNS=10 benchmarks/run.sh         # more samples (default 5)
LABEL=baseline benchmarks/run.sh  # name the results file (default: git short sha)
NO_OPT=1 benchmarks/run.sh        # compile benchmarks with LIN_NO_OPT=1
FAST_BUILD=1 benchmarks/run.sh    # skip the forced runtime rebuild (see below)
```

By default the runner **deletes and rebuilds `liblin_runtime.a`** before timing.
This is deliberate: every benchmark binary links that archive, and cargo's
staleness detection cannot be relied on across commits or worktrees — a stale
archive once produced a phantom 2.5x "regression" that vanished once the runtime
was rebuilt from current source. The results header records the archive's md5
(`# runtime:`) so two result files prove they linked the same runtime. Use
`FAST_BUILD=1` only for repeated runs of an unchanged tree.

Each benchmark is run once un-timed to warm caches, then `RUNS` timed runs; we
report the **min** (most reproducible for CPU-bound work) and **median** wall-clock
in milliseconds. Results land in `benchmarks/results/<LABEL>.txt`.

> Note: `run.sh` truncates the results file for the label it's given on each run,
> and a name filter only writes the matching rows. For a complete baseline run it
> with no filter.

## Comparing before/after a change

```bash
LABEL=before benchmarks/run.sh        # on master / before your change
# ... make the codegen/runtime change, rebuild ...
LABEL=after  benchmarks/run.sh
diff benchmarks/results/before.txt benchmarks/results/after.txt
```

Because absolute numbers depend on the machine, only compare runs taken on the
same hardware in the same session. Commit a results file only as a dated
reference point, not as a pass/fail gate.

**Measure a change on a branch in this same tree, not in a separate worktree.**
Comparing two worktrees' `target/` dirs is how the stale-runtime trap bit us:
each tree builds its own archive and they can drift. The reliable workflow is to
`LABEL=before` on the current branch, switch to the branch with your change in
the *same* checkout, rebuild (the forced runtime rebuild handles this), then
`LABEL=after`. Confirm the two result files share the same `# runtime:` line is
*expected to differ* (that's the point) but that nothing else in the environment
changed.

## What each benchmark targets

| File | Hot path exercised |
|------|--------------------|
| `recursion.lin` | Function call/return overhead, TCO trampoline (tail `sumTo`), non-tail self-recursion (`fib`). Mostly unboxed Int32, so isolates call + branch cost. |
| `array_pipeline.lin` | `map`/`filter`/`reduce` over a large range: indirect closure calls, Int32 box/unbox through the `Json` element slot, RC on intermediate arrays, allocation. |
| `object_access.lin` | Object construction + the O(n) linear-scan field lookup in `lin_object`; chained field reads multiply the scan. |
| `string_build.lin` | String allocation with no small-string optimisation, interpolation/concat, string RC. |
| `async_await.lin` | `async`/`await` spawn-per-call overhead: OS thread spawn + env deep-copy (Option C) + join, per round-trip. Trivial thunk, so time is the thread machinery (mostly `sys`). |
| `parallel_speedup.lin` | Real parallel throughput: 8 CPU-bound chunks via `parallel`. Wall-clock should approach one chunk while `user` CPU stays ~8x — measures overlap, not just spawn cost. |
| `thread_pool.lin` | `ThreadPool` enqueue + dispatch + result collection for many short tasks on a bounded pool (queue contention among workers), vs async's spawn-per-call. |
| `shared_lock.lin` | `Shared<T>` `RwLock` acquire/release + copy-in/out under real cross-thread write contention (8 threads × many `withLock` RMWs on one box). |
| `worker_roundtrip.lin` | `Worker` request/reply: mailbox send + handler dispatch + oneshot reply, per blocking `request`, over a long-lived worker thread. |

These map to the performance findings in the project's perf review (RC traffic,
boxing at polymorphic boundaries, O(n) object field access, no SSO) plus the
concurrency cost model (ADR-028/044/045: copy-by-default transfer, atomic
`Shared` box + lock, immortal `Frozen` reads). When adding an optimisation, add
or extend a benchmark that targets it.

### Reading the concurrency benchmarks

The async/threading benchmarks measure different costs, so compare each against
itself across changes, not against one another:

- `async_await` / `worker_roundtrip` are **latency** benchmarks — per-operation
  round-trip cost (thread spawn, mailbox round-trip). Most of the time is `sys`
  (syscalls / scheduling), so they're noisier than the CPU-bound benchmarks.
- `parallel_speedup` is a **throughput** benchmark — its wall-clock depends on
  core count. On a busy or low-core machine the speedup shrinks; check `user`
  time stays ~Nx wall-clock to confirm the chunks actually overlapped.
- `thread_pool` / `shared_lock` mix dispatch/lock overhead with light work; they
  surface queue and lock contention.

## Caveats

- Timing is whole-process wall-clock (includes process startup + module init),
  so workloads are sized to run long enough (~80 ms+) that fixed overhead is a
  small fraction. Keep new benchmarks in that range.
- `run.sh` uses bash `EPOCHREALTIME`; it needs bash ≥ 5.0.
- This is a coarse macro-harness for guiding optimisation work, not a precise
  microbenchmark framework.
