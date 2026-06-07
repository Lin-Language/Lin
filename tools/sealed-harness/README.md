# Sealed-record representation harness (ADR-063, Phase 1)

Exhaustive differential + ASan matrix over {operation} × {value position} × {field shape} for
sealed-record values — the cross-product the hand-written sealed integration tests only SAMPLE.
This is the merge gate for every Stage-3b gate-widening step (ADR-063): you may only widen
`sealed_array_elem_field_packable` to admit a new field shape once this harness is green for that
shape under full ASan (leaks-on AND leaks-off) and run-equivalence.

It would have caught all 8 RC/correctness bugs fixed on 2026-06-07 (heap-field array
index/push/drop/return/TCO-param leaks + the index-set heap-buffer-overflow + the monomorph symbol
collision), none of which the 48 existing sealed tests exercised (they never loop-drop a heap-field
record under ASan).

## What it does
`gen.py` emits one minimal `.lin` program per matrix cell, each running the operation in a
build/drop LOOP (so a per-iteration leak SCALES with iteration count — a constant residual is the
program-lifetime string-intern cache and is expected). `run.sh`:
  1. builds each program with the workspace `lin` to native + the ASan-instrumented `liblin_runtime.a`;
  2. runs it under `ASAN_OPTIONS=detect_leaks=0` → must exit 0 (no UAF / double-free);
  3. runs it under `ASAN_OPTIONS=detect_leaks=1` at TWO loop counts (e.g. 300 and 3000) → the leaked
     bytes must NOT scale with the count (constant ⇒ no per-iteration leak);
  4. checks the program's stdout against the expected value (a wrong RC free corrupts the result).

## Run-equivalence (Phase 2 hook)
The differential "packed result == boxed result" check requires a force-box toggle (env-gated gate
off) that does NOT yet exist — adding it (in the single consolidated gate predicate) is the FIRST
Phase-2 step per ADR-063. Until then the harness validates the CURRENT gate (heap-field arrays are
boxed today), which is exactly the surface the 2026-06-07 fixes corrected — so it is a useful
standing regression net right now. When the toggle lands, `run.sh --differential` will additionally
build each program with the gate forced OFF and assert byte-identical stdout.

## Usage
    tools/sealed-harness/run.sh                 # build+ASan-check every generated cell
    tools/sealed-harness/run.sh --keep          # keep generated .lin + binaries for inspection
    ASAN_RT=path/to/liblin_runtime.a tools/sealed-harness/run.sh   # override ASan runtime path

## Known limitation (to harden)
The leak-scaling check is currently a 2-point delta (N=300 vs 3000, flag if >1KB growth). This has
false-positive risk: a non-linear allocator artifact (observed: `map`-result-dropped leaks 0 B at
N≤1000 then spikes at N≥5000 — NOT a clean per-call leak) can trip it, while a genuine per-call leak
(observed: `sort` leaks a clean ~229 B/call, linear across N=100/1000/5000) is unambiguous. BEFORE
using this as the Stage-3b merge gate, replace the 2-point delta with a 3+-point linear-fit (per-call
bytes ≈ constant ⇒ real leak; sub-linear/spiky ⇒ investigate, likely arena artifact). Until then,
treat a FAIL as "investigate this cell by hand at 3 Ns", not "definitely a per-iteration leak".
