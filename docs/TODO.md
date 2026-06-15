# Post-Reset Quality + Perf + Cleanup Plan

Driving source: 4 parallel opus code reviews (architecture, lin-runtime, lin-codegen, lin-check+lin-ir)
+ alloc/RC profiling of interp/dijkstra/records (instrumented runtime). Reports in `/tmp/review/*.report.md`.

Goal: fix every real bug, land every measured low-level optimization, do the architectural
single-source-of-truth consolidation, ship the SSO/RC-elision perf work, and retire the `Json` type +
bring docs and docs-site current — using maximally-parallel, **file-disjoint** subagent lanes so there
are no merge conflicts and the heavy benchmarks never contend.

## Execution rules (conflict + contention discipline)

1. **File ownership = conflict avoidance.** Each lane owns a disjoint set of files; no two concurrently
   running lanes touch the same file. Merges within a wave are therefore textually conflict-free; only
   semantic interactions are caught at integration (conductor re-runs full gates).
2. **Benches are serialized by the conductor (me), never by agents.** Agents run only LIGHT gates:
   `cargo build`, `cargo test -p lin`, `lin test stdlib/ examples/`, targeted ASan. The HEAVY benches
   (RAPTOR typed ≈10 min/≈28 GB, `compare.sh`) are run by the conductor one-at-a-time at measurement/merge.
   This is the hard rule from prior sessions (no concurrent 28 GB benches).
3. **Each lane = its own git worktree off `master`, its own branch.** Agents NEVER touch the main
   checkout, NEVER switch branches in a shared tree, NEVER merge to master. Conductor verifies + merges.
4. **Model:** sonnet for well-scoped mechanical lanes; conductor (hands-on) drives the UAF-risk and the
   interdependent architectural spine. Reviews used opus; implementation legs use sonnet.
5. **Gate every lane** (agent self-reports are NOT trusted): build clean · `cargo test -p lin` 819/0 ·
   `lin test` 73/73 · `fmt --check` · for runtime/RC lanes, targeted ASan. Conductor additionally runs
   both RAPTOR digests (must stay `26203913/773022892/139`) + sorted-IR byte-identical where claimed.

---

## WAVE A — correctness bugs + measured micro-opts (ALL PARALLEL, file-disjoint)

Six lanes, zero shared files. Launch all at once.

### A1 — `map.rs` lane (sonnet)  · owns `crates/lin-runtime/src/map.rs`
- [ ] **BUG#1 (HIGH, verified):** `lin_map_values`/`lin_map_entries` iterate raw hash slots `0..cap`
  while `lin_map_keys` iterates `(*map).order` → `keys()[i]` ≠ `values()[i]`. Make all three iterate
  `order` (fetch each value via `lin_map_get`/`lin_map_get_int`). (option-A insertion order everywhere)
- [ ] **OPT:** `lin_map_alloc` honors its `hint` (`cap = hint.next_power_of_two().max(INITIAL_CAP)`,
  size `order` to match) instead of `let _ = hint`.
- [ ] **OPT:** lower linear-probe load factor 0.875 → ~0.7 (`over_load`: `len*10 >= cap*7`).
- [ ] **OPT:** `alloc_slots` use `alloc_zeroed` instead of `alloc` + `write_bytes(0)`.
- [ ] **MED/LOW:** dedup `lin_map_get_bytes` FNV-1a/probe against `find_slot_string`; dedup
  `find_slot_string_profiled` copy (macro / `const PROFILE` generic).

### A2 — `sealed.rs` + `array.rs` lane (sonnet)  · owns those two files
- [ ] **BUG#3 (HIGH, verified):** `build_heap_desc_from_named_desc` (sealed.rs) leaks one descriptor per
  **array allocation** (called from `lin_sealed_ptr_array_alloc`, array.rs:383). Memoize one-per-type in
  a process-global `HashMap<*const u8 named_desc, *const u8>` (or have codegen emit it statically).
- [ ] **OPT (RAPTOR penalty):** `materialize_named_payload_to_map`/`materialize_sealed_to_map_pub` alloc+
  free a `LinString` key per field per materialize. Intern static field-name keys as immortal
  `LinString`s once per type; `lin_map_set` then retains an immortal (no-op).
- [ ] **LOW:** note `lin_record_get_field` O(fields) scan (leave as cold fallback; document).

### A3 — `codegen/boxing.rs` lane (sonnet)  · owns that file
- [ ] **BUG#6 (HIGH):** silent `_ => val`/`_ => ptr`/`_ => tagged` fall-throughs in `box_value`(:236),
  `unbox_value`(:304), `unbox_tagged_val_to_type`(:555) miscompile if a tag/type is added. Replace with
  `debug_assert!`/`unreachable!` for the genuinely-unexpected type/repr combos (keep explicit
  pass-through only for Union/TypeVar that legitimately need it). Corpus gate must stay green.

### A4 — `rc_elide.rs` lane (sonnet)  · owns `crates/lin-ir/src/rc_elide.rs`
- [ ] **OPT (RC-elision-on-hot-borrows):** cross-block elision silently caps at `BFS_BLOCK_LIMIT = 8`,
  skipping elision on deep CFGs (interp/RAPTOR hot fns). Replace the BFS with a **post-dominator-chain
  walk** (PostDom is already computed) — bounded by post-dom depth, not an arbitrary 8. Measure how many
  candidate pairs the old cap dropped on the bench corpus; conductor confirms byte-identical-or-better IR
  + no new ASan leak.

### A5 — `tagged.rs` + `frozen.rs` + `transfer.rs` lane (CONDUCTOR, hands-on — UAF risk)
- [ ] **BUG#2 (HIGH, verified):** `lin_box_sumnode` does NOT retain; `lin_box_record` DOES `lin_rc_retain`
  despite "mirrors exactly" comment. Audit the codegen store/release sites for record- vs sumnode-slots;
  make the two box fns consistent with their store convention (and fix the comment).
- [ ] **BUG#4 (HIGH):** `freeze_payload` skips `TAG_RECORD`/`TAG_SUMNODE` (`_ => {}`) → `frozen(record)`
  returns an unfrozen value; a frozen graph holding a boxed record/sum node stays mortal (cross-thread
  UAF). Add arms that walk the heap descriptor and immortal-seal each heap field + the struct's RC; stop
  materializing-to-map in the freeze path for concrete records.
- [ ] **BUG#5 (HIGH):** `transfer_payload` aliases `TAG_BIGNUM`/`TAG_DECIMAL` across a thread boundary
  with no retain → double-free. Add retain arms (mirror the `TAG_SHARED` arm), or confirm+document the
  checker forbids the capture. Verify under ASan (event-transfers + a crafted bignum-in-thunk test).

### A6 — `string.rs` lane (sonnet)  · owns `crates/lin-runtime/src/string.rs`
- [ ] **OPT:** per-element `String` alloc for integer map keys in `push_json_map`/`push_display_map` —
  write digits straight into `out` (`write!(out, "{}", raw as i64)`); no per-entry heap alloc.
- [ ] **SSO / small-string cache (original perf item):** profiling shows interp 620K + dijkstra 137K
  string allocs are **100% ≤15 bytes** (avg 1.1 B). Add a small-string **freelist/arena** to
  `lin_string_alloc`/`lin_string_free` (size-classed, e.g. ≤32 B blocks) so the small-string churn hits a
  reuse pool, not malloc/free. PURE runtime — no codegen change, no other-file conflict. (Inline-in-slot
  SSO is a larger codegen-touching follow-up, deliberately out of scope this round.)

---

## WAVE J — retire the `Json` type + docs/docs-site currency (PARALLEL to Wave A; disjoint trees)

`Json` is already a parse-time **alias** for `AnyVal` (`resolve.rs:219` → `any_val_type()`). Retire it:
target name = the existing canonical **`AnyVal`**. Sweep userland/docs first (alias still works), THEN
remove the alias line last (conductor) so nothing breaks mid-flight.

### J1 — userland `.lin` sweep (sonnet)  · owns `stdlib/` + `examples/` (8 + 7 Json files)
- [ ] Replace `Json` type annotations with `AnyVal` across stdlib + examples. Must `lin test` 73/73 +
  `fmt --check` clean. Do NOT touch any `crates/` file.

### J2 — `docs/` sweep + currency (sonnet)  · owns `docs/*.md` (NOT docs/TODO.md)
- [ ] Replace `Json` with `AnyVal` in SPECIFICATION.md, STDLIB.md, DECISIONS.md, etc.; confirm the dynamic
  top type is described once, correctly, as `AnyVal`. Note (don't fix) any other staleness found.

### J3 — `docs-site/` sweep + currency (sonnet)  · owns `docs-site/` (content/examples/templates)
- [ ] Sweep `Json` → `AnyVal` in docs-site content + examples; verify the site still builds if it has a
  builder; flag any out-of-date pages.

### J4 — alias removal (CONDUCTOR, after J1 merges)
- [ ] Remove `"Json" => Ok(any_val_type())` (resolve.rs:219); sweep `Json` from lin-check comments.
  Verify full build + 819/0 + 73/73 (nothing left referencing `Json` as a type).

---

## WAVE B — architectural single-source-of-truth consolidation (AFTER Wave A merges)

The unanimous review finding: the same logic is hand-duplicated across files, kept in sync by comment.
These touch the SAME hot files (`lower.rs`, `codegen/types.rs`, `repr.rs`, tag walkers) as Wave A, so they
follow A. Within Wave B, the file-disjoint subset (B5/B7/B9) runs parallel; the `lower.rs`/`types.rs`
spine (B1/B3/B4/B8) is serial + conductor-driven.

- [ ] **B1 (HIGH):** hoist the packed/boxed **gate predicate** (`sealed_fields`, `sealed_array_elem`,
  `sum_type_discriminant`, `nullable_sealed_record`) into ONE module (`lin-check::types`, a dep of both
  ir + codegen). Delete the 6 transcribed mirrors (codegen/types.rs, ir/repr.rs, lower.rs,
  monomorphize.rs, escape.rs). Oracle then guards the dataflow, not two hand-copies. Gate: sorted-IR
  byte-identical + both RAPTOR digests exact.
- [ ] **B2 (HIGH, conductor):** unify the tag walkers — one `TagClass`/`for_each_heap_payload` table that
  `lin_tagged_release`/`retain_tagged_payload`/`transfer_payload`/`freeze_payload` all dispatch through,
  making "handled the new tag everywhere?" a compile-time exhaustiveness check (this class produced
  BUG#4 + BUG#5). Builds on A5.
- [ ] **B3 (HIGH):** single `nkind → byte_size/align` table in `lin-common/tags.rs`; both
  `struct_size_from_named_desc` (runtime) and codegen layout reference it; `debug_assert` reconstructed
  size == header size word.
- [ ] **B4 (MED, mechanical):** split `lower.rs` (10.6 k lines / 1344-line match) into a `lower/` tree
  (`expr.rs`/`stmt.rs`/`call.rs`/`combinator.rs`/`coerce.rs`/`rc.rs`) mirroring the codegen/checker trees.
- [ ] **B5 (MED, mechanical, PARALLEL):** split `codegen/data.rs` (3.1 k) into `data/{object,array,index,coerce}.rs`.
- [ ] **B6 (MED, LAST):** sweep ~80 stale `TAG_OBJECT`/`LinObject`/`lin_object_*` comments across
  codegen + runtime; delete or mark-retired the `TAG_OBJECT` constant; rename
  `lin_object_get_or_insert_array` → `lin_map_get_or_insert_array`. Runs last (touches every file).
- [ ] **B7 (MED, PARALLEL):** factor `infer_if`'s 140-line branch-type merge into a named
  `join_branch_types` with a unit-test matrix (`Null×T`, `Never[]×T[]`, `?unsolved×Bool`, `T9001×D9002`).
- [ ] **B8 (MED):** one shared `is_concrete_rc_ty` (currently 3 copies: lower.rs, rc_elide.rs,
  ownership_verify.rs).
- [ ] **B9 (LOW, PARALLEL):** reconcile ADR-069 prose with the surviving repr lattice; fix the stale
  "shadow mode" comment on `ownership_verify` (lib.rs:369-373); document that `lin-ir` hosts the
  monomorphizer. Collapse the single-inhabitant `Repr::Inner` / delete stale repr.rs paragraphs.

---

## WAVE M — investigate high memory usage (typed RAPTOR ~25 GB vs Node 2–4 GB)  [NEW, high priority]

The typed RAPTOR bench peaks at ~25–28 GB RSS in its RANGE phase; Node does the equivalent in 2–4 GB —
a 6–12× gap. This is a real competitiveness problem, separate from the correctness/perf work above.

Known data (prior measurement, [[project_gc_retired_not_alloc_bound]]): the run churns ~1.8 B allocations
but **< 1.3 GB is ever live**. So RSS ≫ live-set by ~20×. That points away from a true unbounded leak and
toward **allocator retention / fragmentation** (glibc malloc keeps freed small blocks in per-thread arenas
and rarely `madvise`s them back), but a real retention bug (RC holding per-query result graphs across the
RANGE loop) is NOT yet ruled out.

Investigation steps (in order — cheap discriminators first):
- [ ] **Measure live-set vs RSS over time**: sample `/proc/self/statm` (RSS) alongside a periodic
  `malloc_trim(0)` / `mallinfo2` to see how much is reclaimable arena vs genuinely live. If `malloc_trim`
  collapses RSS → it's arena retention, not a leak.
- [ ] **Swap the allocator**: link `mimalloc`/`jemalloc` (or set `MALLOC_ARENA_MAX=1`,
  `glibc.malloc.trim_threshold`) and re-measure RSS. A large drop confirms allocator fragmentation and may
  itself be the fix (a one-line dependency).
- [ ] **Check for a true retention leak**: does RSS grow *monotonically* across the 5 RANGE queries, or
  plateau? Monotonic growth that survives `malloc_trim` = a real leak (a per-query result graph or
  materialized record array not reclaimed) — then bisect with the LIN_ALLOC_STATS counters + ASan leak mode.
- [ ] **Peak working set**: if a phase genuinely needs 25 GB live (e.g. PREP materializing all StopTimes),
  that's an algorithmic materialization problem (the de-materialization lever in
  [[project_raptor_dematerialization_measured]]), not allocator.
Deliverable: root-cause (arena vs leak vs working-set) + the cheapest fix that closes most of the gap.

## Sequencing

```
NOW (parallel):  A1 A2 A3 A4 A6 | J1 J2 J3      (+ conductor drives A5)
then conductor:  merge each lane after its gates + RAPTOR digests pass; run J4 after J1
then:            Wave B  (B5 B7 B9 parallel; B1→B3→B4→B8 serial spine; B2 conductor; B6 last)
```

## Status

### MERGED to master (conductor-verified: 819/0, 73/73, fmt, both RAPTOR digests exact, ASan clean)
- **Wave A** — A1 (map: bug#1 + hint/load-factor/alloc_zeroed/FNV-1a), A2 (sealed: bug#3 desc-leak
  memoize + field-key interning), A3 (boxing: bug#6 debug_assert), A5 (tagged/frozen/transfer/sumnode:
  bug#2 doc-audit, bug#4 freeze TAG_RECORD/SUMNODE, bug#5 bignum/decimal transfer retain), A6 (string:
  int-key opt + small-string freelist). Merged `4c66505c`.
- **Wave J** — J1 (userland: already AnyVal, no-op), J2 (docs sweep), J3 (docs-site sweep),
  **J4 (Json type RETIRED** — alias removed, `$Json`→`$AnyVal` mangle, runtime msg, 433-annotation corpus
  sweep; `Json` is now a hard "Unknown type" error). Merged `e1dfc0c3`.
- **A4** (rc_elide BFS→post-dom idom-chain walk): IR diff = only-removed-RC-pairs, ASan clean, RAPTOR
  JSON digest exact; typed digest confirming → merge pending.

### TODO — Wave B (architectural single-source-of-truth consolidation), as specified above.
B1 gate predicate · B2 TagClass walker (conductor) · B3 nkind→size table · B4 lower.rs split ·
B5 data.rs split · B6 stale-comment sweep (last) · B7 infer_if join · B8 is_rc_type shared · B9 docs/ADR.
