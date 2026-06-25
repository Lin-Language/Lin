#!/usr/bin/env bash
# RAPTOR cross-language benchmark runner.
#
# Runs each language's `bench` over the full GTFS feed, parses the per-phase timing
# lines, verifies every language computed the SAME correctness digests (the gate),
# and prints a table with LOAD and QUERY times kept SEPARATE:
#
#   LOAD  = parse the CSV feed into trips/transfers/interchange
#   PREP  = build the RAPTOR indexes (RaptorAlgorithmFactory.create)
#   GROUP = the 24 group-station queries (with MultipleCriteriaFilter), at 10:00
#   RANGE = "next 20 journeys after 08:00" for 5 pairs
#
# LOAD+PREP together are the one-time setup; GROUP+RANGE are the actual query work.
# The table reports them in distinct columns so setup cost is never conflated with
# per-query cost. Each bench is run $RUNS times (default 1); the MIN per phase is kept.
#
# Usage:
#   benchmarks/compare/raptor/bench.sh                 # all languages present
#   RUNS=3 benchmarks/compare/raptor/bench.sh          # 3 timed runs, keep min
#   LANGS="node go rust" benchmarks/compare/raptor/bench.sh
#
# Languages are skipped (cell "--") if their toolchain is missing. The Lin bench is
# slow (minutes) and is skipped unless LANGS includes "lin" or LIN=1 is set, so a
# default run stays quick.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
DATA="$HERE/data"
RUNS="${RUNS:-1}"
LANGS="${LANGS:-node go rust python lin}"

if [ ! -d "$DATA" ]; then
  echo "ERROR: $DATA not found. Extract gtfs.tar.gz first:" >&2
  echo "  mkdir -p $DATA && tar xzf $HERE/gtfs.tar.gz -C $DATA" >&2
  exit 1
fi

# Phase values per language, keyed "lang:PHASE" -> min ms. Digests per language.
declare -A MIN
declare -A GDIG RDIG NJ

have() { command -v "$1" >/dev/null 2>&1; }

# Parse one bench's stdout: sets globals load/prep/group/range/gdig/rdig/nj.
parse_out() {
  load=$(sed -n 's/^LOAD ms=\([0-9.]*\).*/\1/p' <<<"$1")
  prep=$(sed -n 's/^PREP ms=\([0-9.]*\).*/\1/p' <<<"$1")
  group=$(sed -n 's/^GROUP .* ms=\([0-9.]*\).*/\1/p' <<<"$1")
  range=$(sed -n 's/^RANGE .* ms=\([0-9.]*\).*/\1/p' <<<"$1")
  gdig=$(sed -n 's/^DIGEST group=\([0-9]*\) .*/\1/p' <<<"$1")
  rdig=$(sed -n 's/^DIGEST group=[0-9]* range=\([0-9]*\) .*/\1/p' <<<"$1")
  nj=$(sed -n 's/^DIGEST .* journeys=\([0-9]*\).*/\1/p' <<<"$1")
}

# keep the smaller of MIN[key] and $2
keepmin() {
  local key="$1" val="$2"
  [ -z "$val" ] && return
  if [ -z "${MIN[$key]:-}" ]; then MIN[$key]="$val"; return; fi
  awk "BEGIN{exit !($val < ${MIN[$key]})}" && MIN[$key]="$val"
}

run_lang() {
  local lang="$1" out
  for _ in $(seq 1 "$RUNS"); do
    case "$lang" in
      node)   out=$(cd "$HERE/node" && node bench.js "$DATA" 2>/dev/null) ;;
      go)     out=$(cd "$HERE/go" && go run ./cmd/bench "$DATA" 2>/dev/null) ;;
      rust)   out=$(cd "$HERE/rust" && cargo run --release --quiet --bin bench -- "$DATA" 2>/dev/null) ;;
      python) out=$(cd "$HERE/python" && python3 bench.py "$DATA" 2>/dev/null) ;;
      lin)    out=$("$REPO/target/debug/lin" run "$HERE/lin-manually-typed/src/bench.lin" 2>/dev/null) ;;
    esac
    parse_out "$out"
    keepmin "$lang:LOAD" "$load"; keepmin "$lang:PREP" "$prep"
    keepmin "$lang:GROUP" "$group"; keepmin "$lang:RANGE" "$range"
    [ -n "$gdig" ] && GDIG[$lang]="$gdig" && RDIG[$lang]="$rdig" && NJ[$lang]="$nj"
  done
}

# --- toolchain presence ---
declare -A OK
for lang in $LANGS; do
  case "$lang" in
    node)   have node && OK[node]=1 ;;
    go)     have go && OK[go]=1 ;;
    rust)   have cargo && OK[rust]=1 ;;
    python) have python3 && OK[python]=1 ;;
    lin)    [ -x "$REPO/target/debug/lin" ] && OK[lin]=1 ;;
  esac
done

echo "RAPTOR cross-language benchmark (RUNS=$RUNS, min ms per phase)" >&2
echo "feed: $DATA" >&2

ORDER="node go rust python lin"
for lang in $ORDER; do
  [[ " $LANGS " == *" $lang "* ]] || continue
  if [ -z "${OK[$lang]:-}" ]; then echo "skip $lang (toolchain missing)" >&2; continue; fi
  echo "running $lang ..." >&2
  run_lang "$lang"
done

# --- table ---
echo ""
printf "%-8s | %9s %9s | %9s %9s | %9s %9s\n" "lang" "LOAD" "PREP" "GROUP" "RANGE" "setup" "query"
printf -- "---------+---------------------+---------------------+----------------------\n"
for lang in $ORDER; do
  [[ " $LANGS " == *" $lang "* ]] || continue
  [ -z "${OK[$lang]:-}" ] && { printf "%-8s | %9s %9s | %9s %9s | %9s %9s\n" "$lang" -- -- -- -- -- --; continue; }
  l=${MIN[$lang:LOAD]:-?}; p=${MIN[$lang:PREP]:-?}
  g=${MIN[$lang:GROUP]:-?}; r=${MIN[$lang:RANGE]:-?}
  setup=$(awk "BEGIN{printf \"%.1f\", ${l/\?/0}+${p/\?/0}}")
  query=$(awk "BEGIN{printf \"%.1f\", ${g/\?/0}+${r/\?/0}}")
  printf "%-8s | %9s %9s | %9s %9s | %9s %9s\n" "$lang" "$l" "$p" "$g" "$r" "$setup" "$query"
done
echo ""
echo "Columns: LOAD=parse CSV, PREP=build indexes (setup=LOAD+PREP);"
echo "         GROUP=24 group-station queries, RANGE=next-20 x5 (query=GROUP+RANGE)."

# --- correctness gate ---
echo ""
ref_g=""; ref_r=""; ref_j=""; mismatch=0
for lang in $ORDER; do
  [ -n "${GDIG[$lang]:-}" ] || continue
  if [ -z "$ref_g" ]; then ref_g=${GDIG[$lang]}; ref_r=${RDIG[$lang]}; ref_j=${NJ[$lang]}; ref_lang=$lang; continue; fi
  if [ "${GDIG[$lang]}" != "$ref_g" ] || [ "${RDIG[$lang]}" != "$ref_r" ] || [ "${NJ[$lang]}" != "$ref_j" ]; then
    echo "MISMATCH: $lang group=${GDIG[$lang]} range=${RDIG[$lang]} journeys=${NJ[$lang]} vs $ref_lang group=$ref_g range=$ref_r journeys=$ref_j"
    mismatch=1
  fi
done
if [ "$mismatch" = 0 ] && [ -n "$ref_g" ]; then
  echo "correctness: all languages agreed (group=$ref_g range=$ref_r journeys=$ref_j) OK"
fi
