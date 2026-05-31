#!/usr/bin/env bash
# compare.sh — cross-language benchmark comparison runner.
#
# Compiles/runs five identical workloads (dijkstra, parallel, recursion,
# pipeline, async_io) across Lin, Rust, Go, Python and Node.js, then prints a
# single table of min wall-clock ms (lower = faster). Each implementation prints
# exactly one stdout line "RESULT=<int>"; the runner uses that to verify every
# language computed the SAME answer per workload (a correctness gate), and skips
# any toolchain that isn't installed — it never hard-fails on a missing one.
#
# This is indicative, not authoritative: whole-process wall-clock on one machine.
# See benchmarks/compare/README.md for the methodology and fairness rules.
#
# Usage:
#   benchmarks/compare/compare.sh                  # all workloads, all languages
#   benchmarks/compare/compare.sh recursion        # only matching workloads
#   RUNS=10 benchmarks/compare/compare.sh          # more samples (default 5)
#   LABEL=baseline benchmarks/compare/compare.sh   # tag the results file
#   LANGS="lin rs py" benchmarks/compare/compare.sh  # restrict languages
#   USE_HYPERFINE=1 benchmarks/compare/compare.sh  # use hyperfine if present
#   FAST_BUILD=1 benchmarks/compare/compare.sh     # skip the runtime rebuild
set -euo pipefail

cd "$(dirname "$0")/../.."

RUNS="${RUNS:-5}"
FILTER="${1:-}"
LABEL="${LABEL:-$(git rev-parse --short HEAD 2>/dev/null || echo local)}"
LANGS="${LANGS:-lin rs go py js}"
# Normalize friendly language aliases to the internal keys (lin rs go py js).
_normlangs=""
for _l in $LANGS; do
  case "$_l" in
    rust) _l=rs ;; python) _l=py ;; node|nodejs|js) _l=js ;; golang) _l=go ;;
  esac
  _normlangs+="${_normlangs:+ }$_l"
done
LANGS="$_normlangs"
USE_HYPERFINE="${USE_HYPERFINE:-auto}"
SUITE="benchmarks/compare"
OUTDIR="$SUITE/results"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT
mkdir -p "$OUTDIR"

# The fixed workload order (also the row order in the table). Filtered by $1.
ALL_WORKLOADS=(dijkstra parallel recursion pipeline async_io)
# The fixed language/column order, regardless of LANGS filtering — absent or
# unselected columns render as "--".
ALL_LANGS=(lin rs go py js)

# Map a language key to its source extension.
ext_of() {
  case "$1" in
    lin) echo lin ;; rs) echo rs ;; go) echo go ;; py) echo py ;; js) echo js ;;
  esac
}

# Nanosecond wall clock via bash's EPOCHREALTIME (seconds.microseconds).
now_ns() { local t="${EPOCHREALTIME/./}"; echo "$t"; }  # microseconds, 6-digit frac

want_lang() {
  local l="$1"
  for w in $LANGS; do [[ "$w" == "$l" ]] && return 0; done
  return 1
}

# ----------------------------------------------------------------------------
# Build the Lin compiler once, in release, with a forced fresh lin-runtime
# archive (cargo's staleness detection can't be trusted across commits/worktrees
# — a stale archive once produced a phantom 2.5x "regression"). Only needed if
# Lin is among the selected languages. Mirrors benchmarks/run.sh.
# ----------------------------------------------------------------------------
RT_SUM="n/a"
LIN="target/release/lin"
if want_lang lin; then
  echo "Building lin (release)..." >&2
  if [[ "${FAST_BUILD:-}" != "1" ]]; then
    rm -f target/release/liblin_runtime.a target/release/deps/liblin_runtime-*.a
  fi
  cargo build --release -p lin-runtime -p lin >&2
  RT_ARCHIVE="$(find target/release -maxdepth 1 -name liblin_runtime.a | head -1)"
  RT_SUM="$( [[ -n "$RT_ARCHIVE" ]] && md5sum "$RT_ARCHIVE" | cut -d' ' -f1 || echo unknown)"
fi

# ----------------------------------------------------------------------------
# Toolchain detection (once). Record availability + a version string per lang.
# ----------------------------------------------------------------------------
declare -A AVAIL VER REASON
detect() {
  local lang="$1"
  case "$lang" in
    lin)
      if [[ -x "$LIN" ]]; then AVAIL[lin]=1; VER[lin]="ok";
      else AVAIL[lin]=0; REASON[lin]="compiler not built"; VER[lin]="MISSING"; fi ;;
    rs)
      if command -v rustc >/dev/null 2>&1; then
        AVAIL[rs]=1; VER[rs]="$(rustc --version 2>/dev/null | awk '{print $2}')";
      else AVAIL[rs]=0; REASON[rs]="not installed"; VER[rs]="MISSING"; fi ;;
    go)
      if command -v go >/dev/null 2>&1; then
        AVAIL[go]=1; VER[go]="$(go version 2>/dev/null | awk '{print $3}' | sed 's/^go//')";
      else AVAIL[go]=0; REASON[go]="not installed"; VER[go]="MISSING"; fi ;;
    py)
      if command -v python3 >/dev/null 2>&1; then
        AVAIL[py]=1; VER[py]="$(python3 --version 2>&1 | awk '{print $2}')";
      else AVAIL[py]=0; REASON[py]="not installed"; VER[py]="MISSING"; fi ;;
    js)
      if command -v node >/dev/null 2>&1; then
        AVAIL[js]=1; VER[js]="$(node --version 2>/dev/null)";
      else AVAIL[js]=0; REASON[js]="not installed"; VER[js]="MISSING"; fi ;;
  esac
  # A language not in LANGS is treated as unavailable for this run.
  if ! want_lang "$lang"; then
    AVAIL[$lang]=0
    if [[ -z "${REASON[$lang]:-}" ]]; then REASON[$lang]="not selected (LANGS)"; fi
  fi
}
for l in "${ALL_LANGS[@]}"; do detect "$l"; done

# hyperfine: use only when present AND requested (auto = present).
HF=0
if command -v hyperfine >/dev/null 2>&1; then
  HF_VER="$(hyperfine --version 2>/dev/null | awk '{print $2}')"
  case "$USE_HYPERFINE" in
    1|auto) HF=1 ;;
    *) HF=0 ;;
  esac
else
  HF_VER="MISSING"
  [[ "$USE_HYPERFINE" == "1" ]] && echo "warning: USE_HYPERFINE=1 but hyperfine not found; using bash timer" >&2
fi

# Banner + skipped list.
banner="toolchains: lin=${VER[lin]} rust=${VER[rs]} go=${VER[go]} python=${VER[py]} node=${VER[js]} hyperfine=${HF_VER}"
skipped=""
for l in "${ALL_LANGS[@]}"; do
  if [[ "${AVAIL[$l]}" != "1" ]]; then
    skipped+="${skipped:+, }$l (${REASON[$l]})"
  fi
done
echo "$banner" >&2
[[ -n "$skipped" ]] && echo "skipped: $skipped" >&2

# ----------------------------------------------------------------------------
# Per-(workload,lang) build + timed run. Sets two globals:
#   CELL  — the table cell text (min ms / "--" / "MISMATCH" / "BUILD_FAIL")
#   RESULT_VAL — the captured RESULT integer ("" if none)
# ----------------------------------------------------------------------------
CELL=""
RESULT_VAL=""
run_cell() {
  local wl="$1" lang="$2"
  local dir="$SUITE/$wl"
  local ext; ext="$(ext_of "$lang")"
  local src="$dir/$wl.$ext"
  CELL=""
  RESULT_VAL=""

  if [[ "${AVAIL[$lang]}" != "1" || ! -f "$src" ]]; then
    CELL="--"
    return 0
  fi

  # Build step (compiled languages) -> $TMPDIR/x; run command -> $cmd[@].
  local bin="$TMPDIR/${wl}_${lang}"
  local -a cmd
  case "$lang" in
    lin)
      if ! "$LIN" build "$src" -o "$bin" >"$TMPDIR/build.log" 2>&1; then
        echo "BUILD_FAIL $wl/$lang:" >&2; sed 's/^/    /' "$TMPDIR/build.log" >&2
        CELL="BUILD_FAIL"; return 0
      fi
      cmd=("$bin") ;;
    rs)
      if ! rustc -O -o "$bin" "$src" >"$TMPDIR/build.log" 2>&1; then
        echo "BUILD_FAIL $wl/$lang:" >&2; sed 's/^/    /' "$TMPDIR/build.log" >&2
        CELL="BUILD_FAIL"; return 0
      fi
      cmd=("$bin") ;;
    go)
      if ! GOCACHE="$TMPDIR/gocache" GO111MODULE=off go build -o "$bin" "$src" \
          >"$TMPDIR/build.log" 2>&1; then
        echo "BUILD_FAIL $wl/$lang:" >&2; sed 's/^/    /' "$TMPDIR/build.log" >&2
        CELL="BUILD_FAIL"; return 0
      fi
      cmd=("$bin") ;;
    py) cmd=(python3 "$src") ;;
    js) cmd=(node "$src") ;;
  esac

  # Warm-up run (un-timed): page in, prime caches, and capture RESULT.
  local warm
  if ! warm="$("${cmd[@]}" 2>/dev/null)"; then
    echo "RUN_FAIL $wl/$lang (warm-up exited non-zero)" >&2
    CELL="BUILD_FAIL"; return 0
  fi
  RESULT_VAL="$(printf '%s\n' "$warm" | sed -n 's/^RESULT=//p' | head -1)"

  # Timed runs.
  if [[ "$HF" == "1" ]]; then
    local cmdstr; printf -v cmdstr '%q ' "${cmd[@]}"
    if hyperfine --warmup 1 --runs "$RUNS" --export-json "$TMPDIR/hf.json" \
        "$cmdstr" >/dev/null 2>&1; then
      local minms medms
      minms="$(jq -r '.results[0].min * 1000 | floor' "$TMPDIR/hf.json")"
      medms="$(jq -r '.results[0].median * 1000 | floor' "$TMPDIR/hf.json")"
      CELL="$minms"
      LAST_MEDIAN="$medms"
      return 0
    fi
    # fall through to bash timer if hyperfine failed
  fi

  local times=()
  local i start end
  for ((i = 0; i < RUNS; i++)); do
    start="$(now_ns)"
    "${cmd[@]}" >/dev/null 2>&1
    end="$(now_ns)"
    times+=($(( (end - start) / 1000 )))   # microseconds -> ms
  done
  IFS=$'\n' sorted=($(sort -n <<<"${times[*]}")); unset IFS
  CELL="${sorted[0]}"
  LAST_MEDIAN="${sorted[$(( RUNS / 2 ))]}"
  return 0
}

# ----------------------------------------------------------------------------
# Drive the matrix, collecting cells and per-workload reference RESULTs.
# ----------------------------------------------------------------------------
declare -A TABLE      # TABLE["$wl,$lang"] = cell text
declare -a MISMATCHES # human-readable mismatch lines
declare -a RAN_WORKLOADS

for wl in "${ALL_WORKLOADS[@]}"; do
  [[ -n "$FILTER" && "$wl" != *"$FILTER"* ]] && continue
  RAN_WORKLOADS+=("$wl")

  ref_val=""
  ref_lang=""
  for lang in "${ALL_LANGS[@]}"; do
    run_cell "$wl" "$lang"
    TABLE["$wl,$lang"]="$CELL"

    # Correctness gate: first language with a numeric RESULT sets the reference.
    if [[ -n "$RESULT_VAL" ]]; then
      if [[ -z "$ref_val" ]]; then
        ref_val="$RESULT_VAL"; ref_lang="$lang"
      elif [[ "$RESULT_VAL" != "$ref_val" ]]; then
        TABLE["$wl,$lang"]="MISMATCH"
        MISMATCHES+=("$wl: $lang=$RESULT_VAL != $ref_lang=$ref_val")
      fi
    fi
  done
done

# ----------------------------------------------------------------------------
# Render the table to the results file and to stderr.
# ----------------------------------------------------------------------------
result_file="$OUTDIR/$LABEL.txt"
{
  echo "# Lin cross-language comparison"
  echo "# label:    $LABEL"
  echo "# runs:     $RUNS"
  echo "# runtime:  $RT_SUM"
  echo "# versions: $banner"
  echo "# skipped:  ${skipped:-none}"
  echo "# timer:    $([[ "$HF" == "1" ]] && echo "hyperfine $HF_VER" || echo "bash EPOCHREALTIME")"
  echo "# cells:    min wall-clock ms (lower=faster); -- = skipped/absent, BUILD_FAIL, MISMATCH = wrong answer"
} > "$result_file"

# Column headers (always all five languages).
hdr_fmt='%-12s %10s %10s %10s %10s %10s\n'
{
  printf "$hdr_fmt" "workload" "lin" "rust" "go" "python" "node"
  printf "$hdr_fmt" "------------" "----------" "----------" "----------" "----------" "----------"
  for wl in "${RAN_WORKLOADS[@]}"; do
    printf "$hdr_fmt" "$wl" \
      "${TABLE["$wl,lin"]:---}" \
      "${TABLE["$wl,rs"]:---}" \
      "${TABLE["$wl,go"]:---}" \
      "${TABLE["$wl,py"]:---}" \
      "${TABLE["$wl,js"]:---}"
  done
} | tee -a "$result_file" >&2

# Correctness footer.
{
  if [[ -z "${MISMATCHES[*]:-}" ]]; then
    echo "# correctness: all languages agreed ✓"
  else
    echo "# correctness: MISMATCHES detected:"
    for m in "${MISMATCHES[@]}"; do echo "#   $m"; done
  fi
} | tee -a "$result_file" >&2

echo >&2
echo "Wrote $result_file" >&2
exit 0
