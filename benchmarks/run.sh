#!/usr/bin/env bash
# Benchmark runner for compiled Lin programs.
#
# Builds each benchmarks/*.lin with the current `lin` binary, then times the
# resulting native binary over several runs. Reports min and median wall-clock
# (min is the most reproducible signal for CPU-bound work; median guards against
# a single fast outlier). Writes a results file you can diff across changes.
#
# Usage:
#   benchmarks/run.sh                 # build lin (release), run all benchmarks
#   benchmarks/run.sh recursion       # run only matching benchmarks
#   RUNS=10 benchmarks/run.sh         # override run count (default 5)
#   LABEL=baseline benchmarks/run.sh  # tag the results file (default: git short sha)
#   NO_OPT=1 benchmarks/run.sh        # compile benchmarks with LIN_NO_OPT=1
#
# Results are written to benchmarks/results/<LABEL>.txt. To compare two states,
# run with LABEL=before, make your change, run with LABEL=after, then
# `diff benchmarks/results/before.txt benchmarks/results/after.txt` (or just
# eyeball the two files — each line is "name  min_ms  median_ms").
set -euo pipefail

cd "$(dirname "$0")/.."
RUNS="${RUNS:-5}"
FILTER="${1:-}"
LABEL="${LABEL:-$(git rev-parse --short HEAD 2>/dev/null || echo local)}"
OUTDIR="benchmarks/results"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT
mkdir -p "$OUTDIR"

# Build the compiler once, in release, so we're not timing a debug `lin`.
#
# CRITICAL: force a fresh `lin-runtime` archive. Every benchmark binary is the
# compiler's object file linked against target/release/liblin_runtime.a. Cargo's
# staleness detection cannot be trusted here — a runtime archive built at an
# earlier commit (or in another worktree's target/) gets silently linked, which
# once produced a phantom 2.5x "regression" that was entirely a stale archive.
# Removing the archive before building guarantees it matches the current source.
# Set FAST_BUILD=1 to skip this if you know the runtime is current (e.g. repeated
# runs of the same tree without source changes).
echo "Building lin (release)..." >&2
if [[ "${FAST_BUILD:-}" != "1" ]]; then
  rm -f target/release/liblin_runtime.a target/release/deps/liblin_runtime-*.a
fi
cargo build --release -p lin-runtime -p lin >&2
LIN="target/release/lin"
# Record the runtime archive's checksum in the results header so two result
# files can be compared with confidence they linked the same runtime.
RT_ARCHIVE="$(find target/release -maxdepth 1 -name liblin_runtime.a | head -1)"
RT_SUM="$( [[ -n "$RT_ARCHIVE" ]] && md5sum "$RT_ARCHIVE" | cut -d' ' -f1 || echo unknown)"

opt_env=()
opt_note="O2 (default)"
if [[ "${NO_OPT:-}" == "1" ]]; then
  opt_env=(env LIN_NO_OPT=1)
  opt_note="LIN_NO_OPT=1"
fi

result_file="$OUTDIR/$LABEL.txt"
{
  echo "# Lin benchmark results"
  echo "# label:   $LABEL"
  echo "# runs:    $RUNS"
  echo "# opt:     $opt_note"
  echo "# runtime: $RT_SUM"
  echo "# columns: name  min_ms  median_ms"
} > "$result_file"

printf '%-20s %10s %10s\n' "benchmark" "min(ms)" "median(ms)" >&2
printf '%-20s %10s %10s\n' "--------------------" "----------" "----------" >&2

# Nanosecond wall clock via bash's EPOCHREALTIME (seconds.microseconds).
now_ns() { local t="${EPOCHREALTIME/./}"; echo "$t"; }  # microseconds, 6-digit frac

for src in benchmarks/*.lin; do
  name="$(basename "$src" .lin)"
  [[ -n "$FILTER" && "$name" != *"$FILTER"* ]] && continue

  bin="$TMPDIR/$name"
  if ! "${opt_env[@]}" "$LIN" build "$src" -o "$bin" >"$TMPDIR/build.log" 2>&1; then
    printf '%-20s %10s %10s\n' "$name" "BUILD" "FAIL" >&2
    sed 's/^/    /' "$TMPDIR/build.log" >&2
    echo "$name BUILD_FAIL" >> "$result_file"
    continue
  fi

  # Warm-up run (page in the binary, prime caches) — not timed.
  "$bin" >/dev/null 2>&1 || true

  times=()
  for ((i = 0; i < RUNS; i++)); do
    start="$(now_ns)"
    "$bin" >/dev/null 2>&1
    end="$(now_ns)"
    # microseconds -> milliseconds (integer ms is plenty at these magnitudes)
    times+=($(( (end - start) / 1000 )))
  done

  # min and median
  IFS=$'\n' sorted=($(sort -n <<<"${times[*]}")); unset IFS
  min="${sorted[0]}"
  mid="${sorted[$(( RUNS / 2 ))]}"

  printf '%-20s %10s %10s\n' "$name" "$min" "$mid" >&2
  printf '%-20s %10s %10s\n' "$name" "$min" "$mid" >> "$result_file"
done

echo >&2
echo "Wrote $result_file" >&2
