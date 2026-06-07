#!/usr/bin/env bash
# Sealed-record representation harness runner (ADR-063 Phase 1).
# Builds every generated matrix cell at two loop counts, ASan-checks each:
#   (1) detect_leaks=0 -> exit 0 (no UAF/double-free)
#   (2) stdout == expected (a wrong RC free corrupts the result)
#   (3) leaked bytes do NOT scale between the two loop counts (no per-iteration leak)
# Exit 0 iff every cell passes all three. Prints a per-cell PASS/FAIL table.
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
LIN="$REPO/target/debug/lin"
GEN="$HERE/gen.py"
N1=300; N2=3000
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

pass=0; fail=0
printf "%-34s %-8s %s\n" "CELL" "RESULT" "detail"
while IFS=$'\t' read -r name formula; do
  [ -z "$name" ] && continue
  tmpl="$WORK/$name.lin.tmpl"
  r1=$(build_run "$tmpl" "$N1" "$WORK/${name}_1")
  IFS='|' read -r ec1 lk1 so1 <<<"$r1"
  if [ "$ec1" = "BUILDFAIL" ] || [ "$ec1" = "LINKFAIL" ]; then
    printf "%-34s %-8s %s\n" "$name" "ERROR" "$ec1"; fail=$((fail+1)); continue
  fi
  r2=$(build_run "$tmpl" "$N2" "$WORK/${name}_2")
  IFS='|' read -r ec2 lk2 so2 <<<"$r2"
  exp1=$(expected "$formula" "$N1"); exp2=$(expected "$formula" "$N2")
  problems=""
  [ "$ec1" != 0 ] || [ "$ec2" != 0 ] && problems="$problems UAF/df(exit $ec1,$ec2)"
  [ "$so1" != "$exp1" ] && problems="$problems wrong-result(got $so1 want $exp1 @N=$N1)"
  [ "$so2" != "$exp2" ] && problems="$problems wrong-result(got $so2 want $exp2 @N=$N2)"
  # leak scaling: if lk grows roughly with N (lk2 - lk1 large), it's a per-iteration leak.
  # constant residual (string-intern cache) -> lk1 ~= lk2. Flag if lk2 exceeds lk1 by > 1KB.
  if [ "$((lk2 - lk1))" -gt 1024 ]; then problems="$problems per-iter-leak(${lk1}->${lk2} B)"; fi
  if [ -z "$problems" ]; then
    printf "%-34s %-8s %s\n" "$name" "PASS" "leak=${lk2}B(const) result=$so2"; pass=$((pass+1))
  else
    printf "%-34s %-8s %s\n" "$name" "FAIL" "$problems"; fail=$((fail+1))
  fi
done < "$WORK/MANIFEST.tsv"
echo "----"
echo "PASS=$pass FAIL=$fail"
[ "$fail" = 0 ]
