# Lin cross-language comparison suite

A self-contained suite that compares **Lin** against **Go, Rust, Python and
Node.js** on eleven identical workloads, and prints a single table of min
wall-clock milliseconds (lower = faster).

The first seven (`dijkstra`…`async_io`) are macro/realistic + concurrency
workloads; the last four (`nbody`, `knucleotide`, `binarytrees`, `revcomp`) are
ports of the classic numeric/string/hash/allocation micro-benchmarks that every
standard cross-language suite (Computer Language Benchmarks Game, Are-We-Fast-Yet,
kostya) treats as table stakes, added so the set covers the float, string,
hash-map and GC/refcount axes the original seven did not.

This is **indicative, not authoritative**. It measures whole-process wall-clock
on one machine — it is *not* a definitive cross-language ranking. See
[Caveats](#caveats).

It is separate from the Lin-only harness (`benchmarks/run.sh` +
`benchmarks/*.lin`), which times *only* compiled Lin binaries against each other
across code changes. Nothing here touches that harness.

## What it measures

For each (workload, language) the runner does one un-timed warm-up run, then
`RUNS` timed runs of the **whole process**, and reports the **min** (most
reproducible for CPU-bound work) and computes the **median** over the runs. The
timed region is the entire process: it **includes process startup, interpreter
launch / JIT warm-up, and (for Dijkstra) input parsing**. That startup cost is a
real, interesting difference between a compiled native binary (Lin, Rust, Go)
and an interpreter/VM (Python, Node), so it is deliberately included rather than
factored out.

Lin and Rust binaries are built once per run and timed; Go is built with
`go build`; Python and Node run their scripts directly.

## The eleven workloads

Every implementation prints **exactly one** stdout line `RESULT=<int>` (all
other logging goes to stderr); the runner uses that value as a correctness gate
(see [Reading the table](#reading-the-table)). Parameters are identical across
all languages and are the single source of truth:

| Workload   | What it exercises | Fixed parameters | Pinned `RESULT` |
|------------|-------------------|------------------|-----------------|
| `dijkstra` | Graph build + linear-scan-PQ shortest path + input parsing | N=4000 nodes, ~33163 edges, source `n0`, target `n3999` | `121789671` |
| `interp`   | Expression interpreter: tokenize → recursive-descent parse → tree-walk eval | REPS=10000 over 8 fixed exprs | `10460000` |
| `parallel` | CPU-bound fan-out across threads/processes | START=27, ITERS=300000000, CHUNKS=8 | `2173714077200` |
| `recursion`| Recursive call overhead (`fib`) + iterative loop (`sumTo`) | FIB_N=42, SUM_N=50000000 | `269164297900400072` |
| `pipeline` | Eager `map`/`filter`/`reduce`, materializing each stage | N=20000000 | `133333326666666` |
| `records`  | Record-access-bound: thread one struct through field-read + reconstruct cycles (constant-offset struct-layout field access) | N=50000000, MOD=2147483647 | `1298599827` |
| `async_io` | I/O-bound bounded concurrency (latency/overlap, not runtime speed) | TASKS=200, SLEEP_MS=50, CONCURRENCY=50 | `40000` |
| `nbody`    | Floating-point numerics: symplectic n-body integrator (Float64 mul/add + sqrt over body pairs) | N=5000000 steps, dt=0.01, 5 bodies | `-171605325` |
| `knucleotide` | String-keyed hash-map throughput + per-window substring allocation (k-mer frequency count) | N=4000000 bases, K=8 | `248211949` |
| `binarytrees` | Allocation / refcount-vs-GC churn: bottom-up allocate + traverse + reclaim many 2-pointer nodes | MIN_DEPTH=4, MAX_DEPTH=16 | `14985902` |
| `revcomp`  | Byte-buffer throughput: generate + reverse-complement + checksum a large contiguous buffer | N=20000000 bases | `452296230` |

Workload sizes are chosen so each runs long enough that fixed overhead (process
start, thread spawn) is a small fraction and the cross-language ratio is stable —
a workload that finishes in ~10-20ms hides the real per-operation gap behind
startup cost (see the scaling notes in `## Caveats`).

Per-workload checksum definitions:

- **dijkstra**: `dist[n3999] * 1000003 + (sum of all finite dist values mod 1e9)`,
  in 64-bit. "Finite" means `dist < 1000000000` (the infinity sentinel). For the
  committed graph: `dist[n3999]=121`, `sumFinite=789308` → `121789671`.
- **interp**: a faithful port of `examples/calc/` — a tokenizer → recursive-descent
  parser (`expr = term (('+'|'-') term)*`, standard precedence) → tree-walking
  evaluator, run over 8 fixed integer expressions REPS=10000 times. Every
  expression evaluates successfully (no div-by-zero) and uses truncating integer
  division, so the result is deterministic. `RESULT` = sum of all evaluated values
  = (14+20+10+24+21+10+31+916) × 10000 = `10460000`. This is a second *generalized*
  workload (like dijkstra): unlike the micro-benchmarks below it stresses
  per-iteration allocation (token arrays + AST nodes), string scanning, recursion,
  and tagged-union dispatch — representative of real interpreter/parser code.
- **parallel**: sum of the 8 chunk results. Each chunk runs `walk(27, 300000000)`,
  a Collatz-style integer walk (`next = 27 if start==1 else start/2 if even else
  3*start+1`) accumulating `steps + start` in 64-bit. One chunk = `271714259650`,
  ×8 = `2173714077200`.
- **recursion**: `fib(42) * 1000000007 + sumTo(50000000)`. `fib(42)=267914296`,
  `sumTo(50000000)=1250000025000000` → `269164297900400072`.
- **pipeline**: `range(0,N).map(x=>x*2).filter(x=>x%3==0).reduce(0,+)` for
  N=20000000 → `133333326666666`.
- **records**: a single 6-field struct `State{a..f}` (all Int64) initialised `1..6`,
  threaded through N=50000000 read-all-6 / reconstruct-all-6 cycles via a bounded
  LCG-style mix kept in `[0, MOD)` (`MOD=2147483647`), then `RESULT = (a+b+c+d+e+f) % MOD`.
  Because every value stays under 2^31 the per-iteration math is bit-identical across
  i64/int64/Python-int and JS **BigInt** (the transient pre-mod product
  `a*1103515245 ≈ 2.3e18` exceeds 2^53, so `records.js` uses BigInt). Field access
  dominates: this is the workload Lin's **sealed-record struct layout** accelerates —
  the named all-scalar `type State` is laid out as a packed heap struct, so each field
  read is a constant-offset load instead of a boxed string-keyed `lin_object_get` hash
  lookup. (Measured Lin-to-Lin: the same code typed `State: Json` ran ~4× slower.) →
  `1298599827`.
- **async_io**: `sum_{i=0..199}(i*2+1) = 200*200 = 40000`. NOTE: this workload is
  latency-bound — every language pins to the `ceil(TASKS/CONCURRENCY)*SLEEP_MS`
  sleep floor, so it tests concurrency *overlap*, not runtime speed (see the source
  comment in `async_io.lin`).
- **nbody**: the Computer Language Benchmarks Game "n-body" — a symplectic-integrator
  simulation of the Sun + four Jovian planets, N=5000000 timesteps of `dt=0.01` over
  all 10 body-pairs, in Float64. This is the suite's only **float-dominant** workload
  (the axis CLBG/Are-We-Fast-Yet/kostya all lead with). `RESULT = trunc(energy * 1e9)`
  after the run → `-171605325`. **FP determinism:** every product is bound to a local
  *before* any add/subtract, so no backend contracts a multiply-add into an FMA (which
  would change the last bit); all five run the identical f64 op sequence in the same
  pair order, so the (non-associative) energy sum is bit-identical and the truncated
  checksum agrees. Lin uses 7 parallel flat `Float64[]` arrays and writes hot loops as
  tail-recursive (TCO) loops so the number reflects float arithmetic, not loop-driver
  overhead.
- **knucleotide**: a k-mer frequency count — a deterministic Park-Miller MINSTD
  generator (`state = state*16807 mod 2147483647`, seed 42; every intermediate < 2^53
  so it is bit-identical across i64/int64/Python-int/JS Number) builds an N=4000000-base
  ACGT sequence; a sliding K=8 window is counted into a string-keyed map. This exercises
  the **string-keyed hash + per-window substring** path — the counterpart to `records`,
  which deliberately measures the sealed-struct const-offset *fast* path; knucleotide
  measures the `lin_map_get`-style hashing path `records` avoids. `RESULT = (sum over
  keys of count²) + (distinct key count)` (order-independent) → `248211949`.
- **binarytrees**: the Computer Language Benchmarks Game "binary-trees" — bottom-up
  allocate many short-lived 2-pointer tree nodes, traverse each to a node-count
  checksum, and reclaim them, for even depths `MIN_DEPTH=4..MAX_DEPTH=16`. There is
  almost no arithmetic: the workload **is** allocate + traverse + reclaim, so it measures
  Lin's reference-counting allocator against Go's and Node's tracing GCs (a node is a
  named record with two optional children). `RESULT = stretchCheck + (sum of all
  iteration checks) + longLivedCheck` (the long-lived tree is referenced after the churn
  loop so it stays alive across it) → `14985902`.
- **revcomp**: a checksum form of the Computer Language Benchmarks Game
  "reverse-complement" — the same Park-Miller generator fills an N=20000000-base ACGT
  buffer; it is reverse-complemented (`A<->T`, `C<->G`, read back-to-front) into a second
  buffer; then a rolling checksum `h = (h*31 + code) mod 1000000007` is folded over the
  result (`h < 1e9` keeps `h*31 < 2^53`, so it is bit-identical across all languages).
  This is large contiguous-buffer **allocate + write-pass + read-pass** — a memory-
  bandwidth / flat-array workload distinct from `binarytrees`' small-object churn
  (Lin uses flat scalar `Int32[]` buffers, its strong path) → `452296230`.

## How to run

```bash
benchmarks/compare/compare.sh                    # all workloads, all languages
benchmarks/compare/compare.sh recursion          # only workloads matching "recursion"
RUNS=10 benchmarks/compare/compare.sh            # more samples (default 5)
LABEL=mybox benchmarks/compare/compare.sh        # tag the results file (default: git short sha)
LANGS="lin rs py" benchmarks/compare/compare.sh  # restrict languages
USE_HYPERFINE=1 benchmarks/compare/compare.sh    # use hyperfine if installed (auto = if present)
FAST_BUILD=1 benchmarks/compare/compare.sh       # skip the forced lin-runtime rebuild
```

`LANGS` accepts both the short keys and friendly aliases:
`lin`, `rs`/`rust`, `go`/`golang`, `py`/`python`, `js`/`node`/`nodejs`.

Results are written to `benchmarks/compare/results/<LABEL>.txt` and echoed to
stderr.

Like the Lin-only harness, the runner **deletes and rebuilds `liblin_runtime.a`**
before timing the Lin column (unless `FAST_BUILD=1`), because cargo's staleness
detection cannot be trusted across commits/worktrees — a stale archive once
produced a phantom regression. The results header records the archive's md5
(`# runtime:`) so the Lin number is comparable between this suite and
`benchmarks/run.sh` (both use the same release build + forced rebuild).

### Optional: hyperfine

If [`hyperfine`](https://github.com/sharkdp/hyperfine) is installed, pass
`USE_HYPERFINE=1` (or rely on `auto`) to use it for the timed runs
(`--warmup 1 --runs $RUNS --export-json`, parsed with `jq`), emitting the same
`(min, median)`. The bash `EPOCHREALTIME` timer is the **default and the
fallback** — hyperfine is never required.

## Reading the table

Rows are workloads, columns are the five languages (always shown, in the order
`lin rust go python node`). Each cell is one of:

- a number — the **min wall-clock in milliseconds**;
- `--` — the language was skipped (toolchain not installed, or excluded via
  `LANGS`), or no source file exists for that workload;
- `BUILD_FAIL` — that implementation failed to build/run (the error is logged to
  stderr);
- `MISMATCH` — that implementation computed a **different** `RESULT` than the
  reference for that workload.

**Correctness gate.** Per workload, the first available language's `RESULT` is
the reference; every other language is compared to it. Any disagreement flags the
cell `MISMATCH` and adds a line to the correctness footer
(`# correctness: ...`). When all agree the footer reads
`# correctness: all languages agreed ✓`. This guarantees every language did the
*same work* before its timing is meaningful.

## Fairness rules

1. **Matched algorithm beats matched idiom.** Every language runs the same
   complexity, even when that isn't the most idiomatic local form.
2. **Dijkstra uses a linear-scan priority queue in ALL languages** (O(V²)), not a
   binary heap. This avoids writing/diverging five different heaps and keeps the
   benchmark about each language's runtime on identical work rather than about its
   heap library. All five implementations also use the **same data structures**:
   node names `n<k>` are interned to the integer index `k`, and `dist`/`visited`/
   `adj` are integer-indexed arrays with O(1) access, with the PQ as parallel
   arrays + an O(1) swap-remove. (An earlier Lin version reused the string-keyed
   `Json` maps from `examples/dijkstra/` — whose field lookup is an O(n) linear
   scan — which was a data-structure handicap, not a fair language comparison; it
   has been rewritten to match.)
3. **Identical sizes / iteration counts** — the fixed parameters above; each
   impl hard-codes the same numbers or reads the same graph files.
4. **Same opt level:** Rust `rustc -O`, Go default-optimized `go build`, Lin
   default O2 (no `LIN_NO_OPT`). Python and Node run as-is — a documented
   asymmetry (no AOT level for interpreted/JIT runtimes).
5. **Single-thread workloads** (dijkstra, recursion, pipeline) use no threads.
6. **Parallel CPU-bound:** Python uses **`multiprocessing`** (the GIL serializes
   CPU-bound *threads*, so a thread pool would be misleadingly slow), Node uses
   `worker_threads`, Go uses goroutines + `WaitGroup`, Rust uses `std::thread`,
   Lin uses `parallel([...])`. Fixed worker count 8.
7. **I/O-bound async:** each language's natural concurrent-wait mechanism, with
   **bounded concurrency 50** enforced everywhere (so Lin's thread-per-thunk
   model doesn't finish in a single sleep while bounded languages take ~4×). Rust
   uses a dependency-free fixed pool of 50 sleeping threads (avoids forcing a
   tokio/cargo project); Python uses `asyncio` + `Semaphore(50)`; Node a 50-wide
   promise pool; Go a size-50 channel semaphore over 200 goroutines; Lin a
   `threadPool(50)` + `poolAsync`. These idiom differences are honest and
   intended.
8. **Warm-up** before every timed run.
9. **Dijkstra's graph read is inside the timed region for every language** — it
   covers each language's input parsing, a real and interesting difference.
   JSON-native languages (Lin/Python/Node) read `data/graph.json`; Go/Rust read
   the derived `data/graph.txt`. Both files encode the identical graph (written
   from the same in-memory edge list).
10. **Same machine / same session only.**

## Missing-toolchain behaviour

The runner detects each toolchain once at startup, prints a banner
(`toolchains: lin=ok rust=1.95 go=1.26 python=3.11 node=v24 hyperfine=MISSING`)
and a `skipped:` line for anything absent, then **continues with whatever is
present** — it never hard-fails on a missing toolchain and never requires
`hyperfine`. A skipped language's cells are `--`, and the run still exits 0 and
writes a complete table.

The devcontainer provisions `lin`'s toolchain (Rust + LLVM) plus Node, Python and
Go, so all five languages run out of the box. `hyperfine` is optional and not
installed by default — the bash `EPOCHREALTIME` timer is used unless you install
it and pass `USE_HYPERFINE=1`.

## Caveats

- Whole-process wall-clock includes process startup + interpreter/JIT warm-up;
  for the cheaper workloads that fixed cost is a meaningful fraction of the
  number, especially for Python/Node.
- Numbers are **machine-, core-count-, and scheduler-dependent**. The parallel
  and async workloads depend heavily on available cores and the OS scheduler.
- This is **not** a definitive language ranking — it is a coarse, indicative
  same-machine snapshot for orientation.
- Compare runs only from the **same machine in the same session**. Commit a
  results file only as a dated reference point, never as a pass/fail gate.
- **Scale matters — a too-small workload hides the real gap.** Below ~50-100ms,
  fixed overhead (process start, thread spawn) dominates and the cross-language
  ratio collapses toward 1×. Measured examples that drove the current sizes:
  recursion at FIB_N=38 showed a *false* 1.1× (the sumTo loop masked the recursive
  call cost) but 7× at FIB_N=42; parallel at ITERS=30M showed 1.5× (thread-spawn
  bound) but 15× at 300M; dijkstra showed 67× at N=1000 but the ratio *grew* with N
  (super-quadratic Lin cost), settling the choice of N=4000. When adding or
  resizing a workload, target ~150-600ms so the ratio is stable and meaningful.
  (`async_io` is the exception — it is latency-bound by `sleep`, so no size makes
  it reflect runtime speed; it stays as an overlap-correctness check.)

## How the Dijkstra graph was generated

`data/graph.json` and `data/graph.txt` are committed, generated once (NOT in the
timed path) by `data/gen_graph.py` from a hardcoded seed:

```bash
python3 benchmarks/compare/data/gen_graph.py
```

It writes both files from the *same* in-memory edge list, so they always encode
the identical graph:

- `graph.json` — `{"nodes":["n0",...],"edges":[{"from","to","weight"},...]}`
  (read by Lin/Python/Node).
- `graph.txt` — line 1 is `<num_nodes> <source> <target>` (e.g. `4000 n0 n3999`),
  then one `from to weight` line per edge (e.g. `n0 n1 7`), in the same order as
  the JSON (read by Go/Rust, so they need no JSON library and stay a single-file
  build).

Graph shape (seed `1234`, fully reproducible): 4000 nodes `n0..n3999`; for each
`i`, edges `i -> i+1 .. i+8` (skipping out-of-range), which guarantees `n0`
reaches `n3999`; plus an occasional long forward "skip" edge (each `i` with
probability ~0.3 gets one extra edge to a random `j > i`); weights are random
ints in `[1, 100]`; ~33163 edges total.

## Relationship to the Lin-only harness

`benchmarks/run.sh` times only compiled Lin binaries against each other, to guide
Lin codegen/runtime optimisation work across changes. This suite reuses its
patterns (release build, forced `liblin_runtime.a` rebuild, `# runtime:` md5
header, `EPOCHREALTIME` timing, warm-up + min/median) so that the **Lin column
here is directly comparable to the analogous `benchmarks/run.sh` benchmark** —
the constants were chosen to overlap (parallel/recursion/pipeline mirror
`parallel_speedup.lin`, `recursion.lin`, `array_pipeline.lin`). A large
divergence between the two harnesses' Lin numbers would indicate a build/flag
mistake, not a real change.
