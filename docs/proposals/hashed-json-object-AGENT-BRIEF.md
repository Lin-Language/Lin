# Agent brief: give large `Json` objects O(1) key lookup

You are implementing the fix for the O(n) `Json` key-lookup characteristic (RAPTOR
LIN_ISSUES #4b). The DESIGN is already done — read `docs/proposals/hashed-json-object.md`
first; it picks the approach (a lazy hash side-index) and documents the ABI constraints.
This brief is the *implementation plan + acceptance gate*, not a re-design. If during
implementation you find the recommended design is wrong, stop and write up why rather than
silently diverging.

## TL;DR of the problem

`crates/lin-runtime/src/object.rs` stores `Json` objects as an association list and does a
**linear scan** for every key access (`lin_object_get/set/has/merge/copy_except/eq`). So a
dictionary of N distinct keys built by repeated `set` is O(n²). It's invisible for records
(a few fields) but catastrophic for map-shaped objects: in the RAPTOR port, keying ~16k
`routeId`s while indexing 240k trips cost ~145s vs ~0.5s in hashed-map languages, and forced
the port to avoid the language's only dictionary type entirely.

## The chosen approach (from the proposal — option a)

Keep the assoc-list entries exactly as they are (preserves insertion order, the
single-allocation small-object layout, and the codegen ABI). Add an **optional open-addressing
hash index** `hash(key) → entry-slot-index`, built **lazily** the first time a lookup happens on
an object with `len >= THRESHOLD` (start with 16). Small objects keep today's linear scan and
layout untouched; only big objects build and use the index.

Why lazy + threshold, not always-on: tiny objects are faster to scan than to hash, and the
codegen inline-literal path (see below) builds objects WITHOUT calling `lin_object_set`, so the
index cannot be eagerly maintained on construction anyway.

## The hard constraint you must not break (ABI with codegen)

`crates/lin-codegen/src/codegen/mod.rs`, the `Instruction::MakeObject` handler (currently
starts ~line 1133; the inline-construction branch is ~line 1230, guarded by `inline_eligible`
— grep for `obj_entries_pp` / `LinObjectEntry stride = 24`, the line numbers drift) does
**direct GEP at hardcoded byte offsets** into `LinObject`: reads `entries` at `obj+16`, writes
entries at stride 24 (`key@base`, `tag@base+8`, `payload@base+16`), writes `len` separately —
and **bypasses `lin_object_set`/`lin_object_set_fresh` entirely** for spread-free literals
whose fields all have a concrete representation. (Note: the matching IR op is
`MakeObject` in `crates/lin-ir/src/ir.rs`; spread/union/Function-valued literals fall back to
the runtime set path.)

Therefore:
- **Append new fields at offset ≥ 24 only. Never reorder `refcount@0 / len@4 / cap@8 /
  flags@12 / entries@16` or change the 24-byte entry stride.** If you keep those fixed, no
  codegen change is needed.
- Re-audit that inline path before you start and confirm it never reads past offset 24 (the
  proposal says it currently does not — verify on the current HEAD, the file moves).
- Because that path builds large literals with `index == null`, your lazy build MUST trigger
  off `index == null || index_dirty`, not assume any constructor maintained it.

## Implementation steps

1. **Reproduce + baseline first.** Build a microbenchmark that inserts/looks-up N distinct
   keys (e.g. N = 1, 8, 16, 64, 1k, 16k) and times it. Capture the O(n²) curve BEFORE any
   change so you can prove the fix. Also note the RAPTOR macro-benchmark numbers (below).
2. **Layout.** Append `index: *mut u32`, `index_cap: u32`, `index_dirty: u32` (or similar) to
   `LinObject` at offsets ≥ 24, per the proposal sketch. Update the inline/heap allocation
   sizing and the inline→heap migration (`migrate_*`) so the new fields are initialized
   (index = null, cap = 0, dirty = 0) for every construction path, including the
   single-allocation inline path.
3. **Lazy build + probe** in `lin_object_get` and `lin_object_has`: if `len >= THRESHOLD` and
   (`index == null` || `index_dirty`), do one O(n) pass to (re)build the open-addressing table
   of `entry_slot+1` (0 = empty); then probe O(1) average, confirming hits with the existing
   `lin_string_key_eq` (handle collisions). Below threshold, keep the linear scan.
4. **Maintain on mutation.** `lin_object_set` append branch inserts into the index when present
   (slot indices are stable across realloc — only the buffer base moves — so the table survives
   a grow). Overwrite branch: no index change. `lin_object_merge`, `lin_object_copy_except`,
   any deep-copy/`object_push_owned`: set `index_dirty = 1` (or rebuild). `lin_object_release`:
   free the `index` table before the object. `keys`/`values`/`entries`/`eq` iterate `entries`
   directly and need no change.
5. **Hash function.** A small FNV/FxHash over the key bytes (`(*key).data[..len]`) is enough.

## Acceptance gate — you must clear ALL of these

This change lives in `object.rs`, which the project's own notes
(`project_rc_ownership_invariants` in memory / `docs`) flag as the recurring UAF/double-free
hotspot, and a stale slot index is a **silent wrong result** — the exact failure class this
issue exists to remove. So the bar is higher than `cargo test`:

1. **`cargo build --workspace && cargo test --workspace`** green (build before test — a stale
   binary causes spurious failures).
2. **`cargo run -p lin -- test stdlib/ examples/`** green (71 files at time of writing).
3. **A new fuzz/interleave test** (Rust, in `crates/lin-runtime`): for objects spanning the
   threshold (N = 0,1,15,16,17,64,1000), interleave set / overwrite / merge / copy_except /
   release and assert `get`/`has`/`keys` agree with a **linear-scan oracle** on every key,
   including absent keys. This is the core correctness proof.
4. **ASan run.** `cargo test --workspace` does NOT catch the RC/pointer bugs in this file —
   build the runtime with `-Zsanitizer=address`, link via clang, and run the fuzz test + a
   `lin build`'d program that builds/mutates large objects. Zero new leaks/UAF/double-free
   beyond the known pre-existing ones (top-level data not freed at exit; the string-literal
   interp cache). The repo has done this before — grep NOTES/memory for the ASan recipe.
5. **Microbenchmark** from step 1 shows the inserts/lookups curve flattened from O(n²) to ~O(n).
6. **`lin fmt --check stdlib/ examples/ benchmarks/`** clean if you touch any `.lin`.

## Macro-benchmark: the RAPTOR query phase (the motivating workload)

The end-to-end win is visible in the Lin RAPTOR port. With the feed extracted to
`benchmarks/compare/raptor/data/` (`tar xzf benchmarks/compare/raptor/gtfs.tar.gz -C
benchmarks/compare/raptor/data`):

```
cargo build --workspace
target/debug/lin build benchmarks/compare/raptor/lin/bench.lin -o /tmp/raptor_bench
/tmp/raptor_bench    # prints LOAD / PREP / GROUP / RANGE phase timings + a DIGEST gate line
```

- **PREP** (the `createRaptor` index build — keys 3 indexes by ~16k routeIds over 240k trips)
  is the phase dominated by O(n) object lookups; it was ~145s. This is your headline number to
  move. GROUP/RANGE per-query times should drop too.
- **DO NOT change the answer.** The cross-language correctness gate must stay exactly
  `DIGEST group=26203913 range=773022892 journeys=139` (and the single-query
  `RESULT dep=29400 arr=40680 legs=3 count=1` for `run.lin`). If the digest changes, the
  index returned a wrong/stale slot — that's the bug, not a perf result.
- NOTE the full Lin bench is slow (tens of minutes) even today; for iteration, prefer the
  microbenchmark + the reduced RAPTOR sub-run (first few GROUP queries) and only run the full
  bench once for the final number.

Once this lands, the RAPTOR loader's array-join + binary-search workarounds
(`gtfsLoader.lin` sorted-array `bsearch`, contiguous-run grouping) become unnecessary and the
loader could be simplified to plain `{}` maps — a nice follow-up that also serves as a
real-world regression check, but not required for this change.

## Process (per CLAUDE.md)

Work in your own git worktree off master (`git worktree add .claude/worktrees/<name> -b
fix/lin-hashed-object master`). Add the regression/fuzz tests. Do NOT merge — commit on your
branch and report back with: the microbenchmark before/after curve, the RAPTOR PREP timing
before/after, ASan results, and confirmation the RAPTOR digest is unchanged.

## If option (a) proves unsafe

The proposal's fallback is option (b): a dedicated `Map<K,V>` runtime/stdlib type (purely
additive, no `object.rs` ABI risk, but a real language-surface feature and it leaves the `{}`
discoverability footgun). If you hit a blocker in (a) that you can't make safe with ASan/fuzz
coverage, stop and write up the specific blocker rather than shipping a risky index — a
correct O(n²) beats a fast wrong answer.

## Related, already resolved (don't redo)

- #4a (no stable `sort`) — DONE: `std/array.sort` is now a stable merge sort.
- #5 (dynamic `Json + null` faulting cleanly instead of miscompiling) — DONE in
  `crates/lin-runtime/src/tagged.rs`.
