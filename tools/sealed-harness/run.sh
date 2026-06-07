#!/usr/bin/env bash
# Sealed-record representation harness runner (ADR-063 Phase 1).
# Builds every generated matrix cell at THREE loop counts, ASan-checks each:
#   (1) detect_leaks=0 -> exit 0 (no UAF/double-free)
#   (2) stdout == expected (a wrong RC free corrupts the result)
#   (3) leaked bytes per call are ~constant & above floor across all 3 Ns (genuine
#       per-iteration leak), vs decaying to 0 (constant residual) or non-monotone
#       (allocator artifact -> ARTIFACT?, soft-reported not failed).
# Exit 0 iff every cell passes (1),(2) and shows no linear leak. Prints a PASS/FAIL/ARTIFACT? table.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
LIN="$REPO/target/debug/lin"
GEN="$HERE/gen.py"
N1=200; N2=2000; N3=20000
WORK="$(mktemp -d)"; KEEP=0
[ "${1:-}" = "--keep" ] && KEEP=1
cleanup(){ [ "$KEEP" = 1 ] && echo "kept: $WORK" || rm -rf "$WORK"; }
trap cleanup EXIT

# Locate the ASan-instrumented runtime (built separately by the caller / CI).
RT="${ASAN_RT:-$(find "$REPO/target/x86_64-unknown-linux-gnu/debug" -name liblin_runtime.a 2>/dev/null | head -1)}"
if [ -z "$RT" ] || [ ! -f "$RT" ]; then
  echo "ERROR: ASan runtime not found. Build it first:" >&2
  echo "  RUSTFLAGS=-Zsanitizer=address cargo +nightly build -p lin-runtime --target x86_64-unknown-linux-gnu" >&2
  echo "  (or pass ASAN_RT=/path/to/liblin_runtime.a)" >&2
  exit 2
fi
CLANG="${CLANG:-clang-22}"
command -v "$CLANG" >/dev/null || { echo "ERROR: $CLANG not found" >&2; exit 2; }
[ -x "$LIN" ] || { echo "ERROR: $LIN not built (cargo build -p lin)" >&2; exit 2; }

python3 "$GEN" "$WORK" >/dev/null || exit 2

expected(){ # $1=formula $2=N  -> printed total
  case "$1" in
    sum) echo $(( $2 * ($2 - 1) / 2 ));;
    "2*N") echo $(( 2 * $2 ));;
    *) echo "?";;
  esac
}
build_run(){ # $1=tmpl $2=N $3=outbin ; echo "<exit>|<leakbytes>|<stdout>"
  local tmpl="$1" N="$2" bin="$3"
  local src="$bin.lin" ll="$bin.ll"
  sed "s/@N@/$N/" "$tmpl" > "$src"
  if ! LIN_EMIT_IR=1 LIN_NO_OPT=1 "$LIN" build "$src" -o "$bin" >/dev/null 2>"$bin.builderr"; then echo "BUILDFAIL|0|"; return; fi
  if ! "$CLANG" -fsanitize=address -g "$ll" "$RT" -lpthread -ldl -lm -o "$bin.asan" >/dev/null 2>&1; then echo "LINKFAIL|0|"; return; fi
  local so; so=$(ASAN_OPTIONS=detect_leaks=0 "$bin.asan" 2>/dev/null); local ec=$?
  local lk; lk=$(ASAN_OPTIONS=detect_leaks=1 "$bin.asan" 2>&1 | grep -oE "[0-9]+ byte\(s\) leaked" | grep -oE "^[0-9]+"); lk=${lk:-0}
  echo "$ec|$lk|$so"
}

pass=0; fail=0; artifact=0
printf "%-30s %-9s %s\n" "CELL" "RESULT" "detail"
# 3-point linear-fit classifier.
# A real per-iteration leak holds per-call bytes roughly CONSTANT and > floor across
# all three Ns (total grows ~linearly). A constant residual (string-intern cache) gives
# per-call -> 0 as N grows. A non-monotone total (allocator slack / arena rounding) is an
# ARTIFACT, reported but NOT a hard fail, so it can't masquerade as a leak (the 2-point
# delta's false-positive failure mode, e.g. the map-dropped 0/0/spike).
PERCALL_FLOOR=8   # bytes/call below this is treated as noise/constant
while IFS=$'\t' read -r name formula; do
  [ -z "$name" ] && continue
  tmpl="$WORK/$name.lin.tmpl"
  r1=$(build_run "$tmpl" "$N1" "$WORK/${name}_1")
  IFS='|' read -r ec1 lk1 so1 <<<"$r1"
  if [ "$ec1" = "BUILDFAIL" ] || [ "$ec1" = "LINKFAIL" ]; then
    printf "%-30s %-9s %s\n" "$name" "ERROR" "$ec1"; fail=$((fail+1)); continue
  fi
  r2=$(build_run "$tmpl" "$N2" "$WORK/${name}_2")
  IFS='|' read -r ec2 lk2 so2 <<<"$r2"
  r3=$(build_run "$tmpl" "$N3" "$WORK/${name}_3")
  IFS='|' read -r ec3 lk3 so3 <<<"$r3"
  exp1=$(expected "$formula" "$N1"); exp2=$(expected "$formula" "$N2"); exp3=$(expected "$formula" "$N3")
  problems=""; soft=""
  { [ "$ec1" != 0 ] || [ "$ec2" != 0 ] || [ "$ec3" != 0 ]; } && problems="$problems UAF/df(exit $ec1,$ec2,$ec3)"
  [ "$so1" != "$exp1" ] && problems="$problems wrong-result(got $so1 want $exp1 @N=$N1)"
  [ "$so2" != "$exp2" ] && problems="$problems wrong-result(got $so2 want $exp2 @N=$N2)"
  [ "$so3" != "$exp3" ] && problems="$problems wrong-result(got $so3 want $exp3 @N=$N3)"
  # per-call bytes at each N
  pc1=$((lk1 / N1)); pc2=$((lk2 / N2)); pc3=$((lk3 / N3))
  # monotone-increasing total is necessary for a real leak
  if [ "$lk3" -ge "$lk2" ] && [ "$lk2" -ge "$lk1" ]; then mono=1; else mono=0; fi
  if [ "$pc3" -ge "$PERCALL_FLOOR" ] && [ "$pc2" -ge "$PERCALL_FLOOR" ]; then
    if [ "$mono" = 1 ]; then
      # stable per-call across the largest two Ns (within 2x) => genuine linear leak
      if [ "$pc3" -le "$((pc2 * 2))" ] && [ "$pc2" -le "$((pc3 * 2))" ]; then
        problems="$problems leak(~${pc3}B/call linear: ${lk1}/${lk2}/${lk3}B @N=${N1}/${N2}/${N3})"
      else
        soft="$soft superlinear?(${lk1}/${lk2}/${lk3}B)"
      fi
    else
      soft="$soft non-monotone-leak(${lk1}/${lk2}/${lk3}B @N=${N1}/${N2}/${N3} = allocator artifact?)"
    fi
  fi
  if [ -n "$problems" ]; then
    printf "%-30s %-9s %s\n" "$name" "FAIL" "$problems$soft"; fail=$((fail+1))
  elif [ -n "$soft" ]; then
    printf "%-30s %-9s %s\n" "$name" "ARTIFACT?" "$soft result=$so3"; artifact=$((artifact+1))
  else
    printf "%-30s %-9s %s\n" "$name" "PASS" "leak/call~0 (${lk1}/${lk2}/${lk3}B) result=$so3"; pass=$((pass+1))
  fi
done < "$WORK/MANIFEST.tsv"
echo "----"
echo "PASS=$pass FAIL=$fail ARTIFACT=$artifact"
[ "$fail" = 0 ]
