# Post-Reset Quality + Perf + Cleanup Plan

> ## ⚑ CURRENT STATE (2026-06-16) — read this first
> **Waves A, J, A4, and B are ALL COMPLETE + MERGED** (master `1999bc3e`; 819/0, 73/73, both RAPTOR
> digests exact, ASan clean). The `[ ]` checkboxes and the "Sequencing" block in the sections BELOW are
> the *original plan* and are now historical — see the **Status** section at the bottom for what actually
> landed. **The only OPEN work is: Wave R (memory representation — see lever map), bug #8 (Float32), and
> the deferred B2.** Wave A is done — don't re-run it.

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
- [x] **BUG#1 (HIGH, verified):** `lin_map_values`/`lin_map_entries` iterate raw hash slots `0..cap`
  while `lin_map_keys` iterates `(*map).order` → `keys()[i]` ≠ `values()[i]`. Make all three iterate
  `order` (fetch each value via `lin_map_get`/`lin_map_get_int`). (option-A insertion order everywhere)
- [x] **OPT:** `lin_map_alloc` honors its `hint` (`cap = hint.next_power_of_two().max(INITIAL_CAP)`,
  size `order` to match) instead of `let _ = hint`.
- [x] **OPT:** lower linear-probe load factor 0.875 → ~0.7 (`over_load`: `len*10 >= cap*7`).
- [x] **OPT:** `alloc_slots` use `alloc_zeroed` instead of `alloc` + `write_bytes(0)`.
- [x] **MED/LOW:** dedup `lin_map_get_bytes` FNV-1a/probe against `find_slot_string`; dedup
  `find_slot_string_profiled` copy (macro / `const PROFILE` generic).

### A2 — `sealed.rs` + `array.rs` lane (sonnet)  · owns those two files
- [x] **BUG#3 (HIGH, verified):** `build_heap_desc_from_named_desc` (sealed.rs) leaks one descriptor per
  **array allocation** (called from `lin_sealed_ptr_array_alloc`, array.rs:383). Memoize one-per-type in
  a process-global `HashMap<*const u8 named_desc, *const u8>` (or have codegen emit it statically).
- [x] **OPT (RAPTOR penalty):** `materialize_named_payload_to_map`/`materialize_sealed_to_map_pub` alloc+
  free a `LinString` key per field per materialize. Intern static field-name keys as immortal
  `LinString`s once per type; `lin_map_set` then retains an immortal (no-op).
- [x] **LOW:** note `lin_record_get_field` O(fields) scan (leave as cold fallback; document).

### A3 — `codegen/boxing.rs` lane (sonnet)  · owns that file
- [x] **BUG#6 (HIGH):** silent `_ => val`/`_ => ptr`/`_ => tagged` fall-throughs in `box_value`(:236),
  `unbox_value`(:304), `unbox_tagged_val_to_type`(:555) miscompile if a tag/type is added. Replace with
  `debug_assert!`/`unreachable!` for the genuinely-unexpected type/repr combos (keep explicit
  pass-through only for Union/TypeVar that legitimately need it). Corpus gate must stay green.

### A4 — `rc_elide.rs` lane (sonnet)  · owns `crates/lin-ir/src/rc_elide.rs`
- [x] **OPT (RC-elision-on-hot-borrows):** cross-block elision silently caps at `BFS_BLOCK_LIMIT = 8`,
  skipping elision on deep CFGs (interp/RAPTOR hot fns). Replace the BFS with a **post-dominator-chain
  walk** (PostDom is already computed) — bounded by post-dom depth, not an arbitrary 8. Measure how many
  candidate pairs the old cap dropped on the bench corpus; conductor confirms byte-identical-or-better IR
  + no new ASan leak.

### A5 — `tagged.rs` + `frozen.rs` + `transfer.rs` lane (CONDUCTOR, hands-on — UAF risk)
- [x] **BUG#2 (HIGH, verified):** `lin_box_sumnode` does NOT retain; `lin_box_record` DOES `lin_rc_retain`
  despite "mirrors exactly" comment. Audit the codegen store/release sites for record- vs sumnode-slots;
  make the two box fns consistent with their store convention (and fix the comment).
- [x] **BUG#4 (HIGH):** `freeze_payload` skips `TAG_RECORD`/`TAG_SUMNODE` (`_ => {}`) → `frozen(record)`
  returns an unfrozen value; a frozen graph holding a boxed record/sum node stays mortal (cross-thread
  UAF). Add arms that walk the heap descriptor and immortal-seal each heap field + the struct's RC; stop
  materializing-to-map in the freeze path for concrete records.
- [x] **BUG#5 (HIGH):** `transfer_payload` aliases `TAG_BIGNUM`/`TAG_DECIMAL` across a thread boundary
  with no retain → double-free. Add retain arms (mirror the `TAG_SHARED` arm), or confirm+document the
  checker forbids the capture. Verify under ASan (event-transfers + a crafted bignum-in-thunk test).

### A6 — `string.rs` lane (sonnet)  · owns `crates/lin-runtime/src/string.rs`
- [x] **OPT:** per-element `String` alloc for integer map keys in `push_json_map`/`push_display_map` —
  write digits straight into `out` (`write!(out, "{}", raw as i64)`); no per-entry heap alloc.
- [x] **SSO / small-string cache (original perf item):** profiling shows interp 620K + dijkstra 137K
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
- [x] Replace `Json` type annotations with `AnyVal` across stdlib + examples. Must `lin test` 73/73 +
  `fmt --check` clean. Do NOT touch any `crates/` file.

### J2 — `docs/` sweep + currency (sonnet)  · owns `docs/*.md` (NOT docs/TODO.md)
- [x] Replace `Json` with `AnyVal` in SPECIFICATION.md, STDLIB.md, DECISIONS.md, etc.; confirm the dynamic
  top type is described once, correctly, as `AnyVal`. Note (don't fix) any other staleness found.

### J3 — `docs-site/` sweep + currency (sonnet)  · owns `docs-site/` (content/examples/templates)
- [x] Sweep `Json` → `AnyVal` in docs-site content + examples; verify the site still builds if it has a
  builder; flag any out-of-date pages.

### J4 — alias removal (CONDUCTOR, after J1 merges)
- [x] Remove `"Json" => Ok(any_val_type())` (resolve.rs:219); sweep `Json` from lin-check comments.
  Verify full build + 819/0 + 73/73 (nothing left referencing `Json` as a type).

---

## WAVE B — architectural single-source-of-truth consolidation (AFTER Wave A merges)

The unanimous review finding: the same logic is hand-duplicated across files, kept in sync by comment.
These touch the SAME hot files (`lower.rs`, `codegen/types.rs`, `repr.rs`, tag walkers) as Wave A, so they
follow A. Within Wave B, the file-disjoint subset (B5/B7/B9) runs parallel; the `lower.rs`/`types.rs`
spine (B1/B3/B4/B8) is serial + conductor-driven.

- [x] **B1 (HIGH):** hoist the packed/boxed **gate predicate** (`sealed_fields`, `sealed_array_elem`,
  `sum_type_discriminant`, `nullable_sealed_record`) into ONE module (`lin-check::types`, a dep of both
  ir + codegen). Delete the 6 transcribed mirrors (codegen/types.rs, ir/repr.rs, lower.rs,
  monomorphize.rs, escape.rs). Oracle then guards the dataflow, not two hand-copies. Gate: sorted-IR
  byte-identical + both RAPTOR digests exact.
- [ ] **B2 (HIGH, conductor):** unify the tag walkers — one `TagClass`/`for_each_heap_payload` table that
  `lin_tagged_release`/`retain_tagged_payload`/`transfer_payload`/`freeze_payload` all dispatch through,
  making "handled the new tag everywhere?" a compile-time exhaustiveness check (this class produced
  BUG#4 + BUG#5). Builds on A5.
- [x] **B3 (HIGH):** single `nkind → byte_size/align` table in `lin-common/tags.rs`; both
  `struct_size_from_named_desc` (runtime) and codegen layout reference it; `debug_assert` reconstructed
  size == header size word.
- [x] **B4 (MED, mechanical):** split `lower.rs` (10.6 k lines / 1344-line match) into a `lower/` tree
  (`expr.rs`/`stmt.rs`/`call.rs`/`combinator.rs`/`coerce.rs`/`rc.rs`) mirroring the codegen/checker trees.
- [x] **B5 (MED, mechanical, PARALLEL):** split `codegen/data.rs` (3.1 k) into `data/{object,array,index,coerce}.rs`.
- [x] **B6 (MED, LAST):** sweep ~80 stale `TAG_OBJECT`/`LinObject`/`lin_object_*` comments across
  codegen + runtime; delete or mark-retired the `TAG_OBJECT` constant; rename
  `lin_object_get_or_insert_array` → `lin_map_get_or_insert_array`. Runs last (touches every file).
- [x] **B7 (MED, PARALLEL):** factor `infer_if`'s 140-line branch-type merge into a named
  `join_branch_types` with a unit-test matrix (`Null×T`, `Never[]×T[]`, `?unsolved×Bool`, `T9001×D9002`).
- [x] **B8 (MED):** one shared `is_concrete_rc_ty` (currently 3 copies: lower.rs, rc_elide.rs,
  ownership_verify.rs).
- [x] **B9 (LOW, PARALLEL):** reconcile ADR-069 prose with the surviving repr lattice; fix the stale
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
- [x] **Measure live-set vs RSS over time**: sample `/proc/self/statm` (RSS) alongside a periodic
  `malloc_trim(0)` / `mallinfo2` to see how much is reclaimable arena vs genuinely live. If `malloc_trim`
  collapses RSS → it's arena retention, not a leak.
- [x] **Swap the allocator**: link `mimalloc`/`jemalloc` (or set `MALLOC_ARENA_MAX=1`,
  `glibc.malloc.trim_threshold`) and re-measure RSS. A large drop confirms allocator fragmentation and may
  itself be the fix (a one-line dependency).
- [x] **Check for a true retention leak**: does RSS grow *monotonically* across the 5 RANGE queries, or
  plateau? Monotonic growth that survives `malloc_trim` = a real leak (a per-query result graph or
  materialized record array not reclaimed) — then bisect with the LIN_ALLOC_STATS counters + ASan leak mode.
- [x] **Peak working set**: if a phase genuinely needs 25 GB live (e.g. PREP materializing all StopTimes),
  that's an algorithmic materialization problem (the de-materialization lever in
  [[project_raptor_dematerialization_measured]]), not allocator.
Deliverable: root-cause (arena vs leak vs working-set) + the cheapest fix that closes most of the gap.

## WAVE M — ROOT CAUSE FOUND (2026-06-15): allocation amplification, not allocator

Measured the typed RAPTOR peak with a counting global allocator (`memprofile.rs`, branch
experiment/mem-profile — NOT merged, it's a probe):

- **Allocator ruled out**: peak RSS is ~25GB under glibc-default, ~25GB under `MALLOC_ARENA_MAX=1`, and
  ~23GB under **mimalloc** — all equal. mimalloc returns freed memory aggressively, so this is genuinely
  live, not fragmentation. (mimalloc as global allocator works as a one-liner in lin-runtime and shaved
  ~10% — worth shipping later regardless, but it is NOT the fix.)
- **Root cause = allocation COUNT**: at peak the program holds **265 MILLION live allocations / 21.4 GB**.
  Live-count by size class: ≤16B=38M (TaggedVal boxes), ≤48B=89M + ≤64B=85M (sealed StopTime/Trip
  records, ~10GB), ≤256B=51.7M (larger records/arrays, ~6-13GB).
- **Amplification ≈ 100×**: the dataset is ~2.37M stop_times + 240K trips + 3K stops ≈ 2.6M logical
  records, yet there are 265M live allocations — ~100 heap objects per logical record. Node holds the
  same data in 2-4GB because V8 stores object arrays contiguously with inline fields and small-int
  values; Lin allocates a separate heap object per record AND per field/value, with no columnar storage.

This is the dominant remaining gap and an ARCHITECTURAL project = **Wave R (representation efficiency)**.

## WAVE R — representation efficiency (levers MEASURED 2026-06-16)

The 23GB is genuinely-live object/array structures. Each lever below was measured; status reflects data.

- [x] **String interning — RULED OUT (measured negative, 0.1% RSS).** Dedup ratio is huge (347×: 65k
  distinct short strings, millions of requests) BUT peak RSS dropped only ~31MB / 25GB. RAPTOR's parse is
  STREAMING → transient duplicate strings are already freed before peak; the retained string keys are few
  and same-byte-count interned-or-not. Node's main trick does NOT transfer to a streaming loader. (Gave a
  minor ~3-4% wall-clock from fewer allocs.) Probe: experiment/string-intern (unmerged).

### UPDATE (2026-06-16, measured): the headline is LinMap, but `0xFE` inline stays a PRIORITY (Linus)
Per-kind attribution of RAPTOR's peak (132M live): **map 51.5M = 15.25GB (76%)**, sealed 69.8M = 4.47GB,
tagbox 6.9M live (transient), array/string 0.38GB. So:
- **LinMap memory efficiency is the #1 MEMORY lever** (15GB). Each LinMap = INITIAL_CAP slots × 32B
  (hash8+key8+value:TaggedVal16) + an order array, fixed regardless of entry count. Sub-levers:
  - (a) **INITIAL_CAP 8→4 — DONE+MERGED** (`c8119174`, −1.4GB, byte-identical IR). The cheap safe win.
  - (b) **unboxed/typed map values (16B TaggedVal→8B payload) — ~2.9GB, but ABI-BLOCKED, scoped not landed.**
    Per-map memory is structurally floored at 32B/slot (TaggedVal is 16B + 8-aligned; hash u64→u32 re-pads;
    slots+order can't co-allocate — they grow at different times). The only real shrink is value-unbox, but
    `lin_map_get` returns a BORROWED interior `*const TaggedVal` into the slot (runtime.rs:24). Shrinking the
    in-slot value forces get to return a pointer to a reconstructed scratch TaggedVal. **FEASIBILITY PROVEN
    (2026-06-16): every codegen consumer (index.rs:179-238, object.rs, mod.rs) loads tag+payload immediately
    and none holds two live borrowed results across a second get → a thread-local scratch return is safe.**
    BUT it narrows the documented "interior pointer" contract on the hottest 15GB-critical path → a latent
    UAF if future codegen ever interleaves two gets. **DECISION: do NOT land unsupervised; queue for a
    supervised session.** See memory `project_linmap_memory_lever`.
  - (c) inline-small-map (≤2-4 entries in header) — not pursued; same scratch-ABI issue for the value read.
  - The 51.5M COUNT is the structural floor (one `{String:Trip[]}` route table per stop, legitimately maps).
    Fewer maps = an algorithmic RAPTOR change (bigger project), not a per-map repr tweak.
- **`0xFE` inline records STAYS A PRIORITY** (not demoted): the beyond-Node array repr (headerless inline,
  no per-element header/malloc/pointer) — speed + locality + ~1.5GB; pursue alongside the map lever.
- interning ❌ (0.1%), arena ~17%/4GB (`frozen()` free half), seal-union-fields = interp speed (separate).

> NOTE: J4 (Json retirement) was INCOMPLETE — missed `Json` type refs in lin-check/lin-parse/lin-lsp TEST
> files (my J4 verify used `cargo test -p lin` against CACHED binaries, not `cargo test --workspace` clean).
> Lesson: always `cargo test --workspace` with a fresh build. Linus has an agent sweeping the rest.

- [ ] **Contiguous inline `0xFE` record arrays — the measured-correct DIRECTION, but a multi-path migration
  (not done).** KEY INSIGHT (with Linus): Node's "contiguous arrays" are a contiguous array of POINTERS to
  heap objects (V8 FixedArray) — which Lin's `0xFD` SEALED_PTR repr ALREADY IS (contiguous ptr spine +
  separate structs, reference semantics preserved). So Lin already matches Node here; inline `0xFE` (one
  buffer of header-less record payloads) goes BEYOND Node, eliminating the 24B per-element header + 16B
  malloc overhead + 8B pointer (~48B/record). It COPIES the record, so it breaks `push(arr,t); t.x=5`
  visibility → SOUND ONLY when the element does not escape/mutate after push (escape-gated; escape.rs
  exists). A 2-site spike (MakeArray literal + Push intrinsic) was BUILT (branch experiment/contiguous-
  arrays) and is alias-safe (819/0 + RAPTOR GROUP digest held) — but produced ZERO `0xFE` in RAPTOR's IR:
  RAPTOR builds record arrays via the PROJECTION path (`data/array.rs:496` sealed_array_project_from) +
  combinators + intrinsics.rs:1182, NOT the 2 sites changed. So the real lever = migrate ALL array-
  construction paths to escape-gated `0xFE`. Runtime support + read paths already accept `0xFE`. Whether
  it's worth the multi-path migration needs the per-KIND attribution (below) to confirm records dominate.

- [x] **Small-int CACHE widen — DONE (≤65536).** Widened `[-128,1024)` → 65536 in tagged.rs (2.0MB static,
  3-line change, in-range int boxes shared-immortal). DO NOT widen to dates (20991225 = 640MB static =
  memory regression + binary bloat + bad CPU-cache locality). The cache is O(range) — small ranges only.
  Dates-as-ints need the SMI mechanism below, not a cache.

- [ ] **Pointer-tagged SMI (small-int inlining) — the RIGHT mechanism for DATES-as-ints.** Stores small
  ints immediate in the value word (low tag bit; pointers are 8-aligned) → zero allocation, zero static,
  any int ≤ ~2⁶¹. ~180 consumer sites, higher risk (path-10 spike found 11 bugs but PROVED the
  architecture, 5.1× on scalar microbench). This is what Linus's "dates will be ints" feature needs (a
  cache can't cover the date range). Bigger adventure; do after the cache widen if it disappoints, OR
  pull forward for the dates feature. Probe: experiment/small-int-inline (LIN_SMI_STATS, unmerged).

- [x] **Per-KIND / per-PHASE attribution — DONE (gating measurement).** Result: at peak (132M live), **map
  51.5M = 15.25GB (76%)**, sealed 69.8M = 4.47GB, tagbox 6.9M (transient, 232M cum allocs → mostly freed),
  array 0.30GB, string 0.08GB, sumnode 0. PHASE: LOAD's allocs are 100% RETAINED into PREP (peak is during
  PREP); maps grow 2.9M→51.5M during PREP building `{String:Trip[]}` route tables; sealed grows 6.4M→69.8M
  (the Trip/StopTime records). So: the dominant lever is MAP per-map cost (→ ABI-blocked, above), NOT 0xFE
  (records are sealed=4.47GB, second place). The maps are legitimately maps, not mis-materialized records.

- [ ] **Header compaction 24→16B** — merge `{size,heap_desc,named_desc}` (16B) into one per-type metadata
  pointer (8B). ~100-site / 20-file layout migration (every field offset shifts 8B, UAF risk). ~8B/record.
  SUBSUMED by `0xFE` inline for array-held records; mainly helps STANDALONE records. Lower priority.

- [ ] **mimalloc global allocator** — one-liner in lin-runtime, ~10% RSS + 3-5% wall-clock, drop-in. A
  default-allocator/dependency/CI/platform POLICY call for Linus (or behind a feature flag). Not the fix.

Deliverables achieved: root cause (allocation amplification, allocator ruled out) + interning ruled out +
contiguous=Node-already-matched insight + the lever map. NEXT: per-kind attribution → then the chosen lever.

## SEQUENCING THE REMAINING WORK (max parallel; big memory levers are serial on shared core files)

**Phase R0 — PARALLEL NOW (file-disjoint lanes + throwaway-experiment measurements):**
- [ ] **R0-attr** — per-kind/per-phase attribution (experiment, instruments allocators; never merges) → the
  gating data for the big lever. [task #13]
- [ ] **R0-cache** — small-int cache widen 1024→65536 (`tagged.rs` only). cheap, measure wall-clock+RSS. [#11]
- [ ] **R0-float32** — bug #8 fix (`lin-common/tags.rs` + `codegen/types.rs` + `sealed.rs`). ASan+digest gate. [#8]
- [ ] **R0-mimalloc** — mimalloc behind a cargo FEATURE flag (`lin-runtime/Cargo.toml` + `lib.rs`, disjoint).
  measure RSS/wall-clock; default off (policy call).
- [ ] **R0-explore×N** — read-only exploration agents for the Additional Areas below (no conflict).

**Phase R1 — the BIG memory lever, chosen by R0-attr (SERIAL, conductor-driven):**
- [ ] IF records dominate → **contiguous `0xFE` inline migration** (all array-build paths: projection
  data/array.rs:496 + combinators + intrinsics.rs:1182 + MakeArray + Push), escape-gated. [#9]
- [ ] OR for the dates feature → **pointer-tagged SMI** (~180 sites). [#12]
  (These two + header compaction overlap core files → cannot run concurrently with each other.)

**Phase R2 — secondary (serial):** header compaction 24→16B (if not subsumed by inline) · B2 TagClass walker.

## ADDITIONAL AREAS TO EXPLORE (new — Wave R+ and beyond)

- [ ] **Arena / bump allocation for program-lifetime data** — RAPTOR's index is built once in PREP and never
  freed until exit. A bump arena (one region, freed all-at-once / never) → ZERO per-object malloc header,
  ZERO per-object RC, ZERO free. The LIN_NO_RC experiment proved RC is pure overhead for this retention
  pattern. **Potentially the single biggest RAPTOR memory+speed lever.** Needs an arena allocator + a
  lifetime/region marker (escape-analysis-inferred, or an explicit `region {}`/`frozen`-like scope).
- [ ] **Columnar (struct-of-arrays) record arrays** — beyond `0xFE` array-of-structs: each field its own
  contiguous column (all `departureTime`s together). Max compactness (no per-record padding), SIMD-friendly,
  best cache locality for field-at-a-time scans (RAPTOR scans departureTimes). Bigger than `0xFE`.
- [ ] **RC elimination for immortal/program-lifetime graphs** — extend/infer `frozen` so program-lifetime
  data skips RC entirely (retain/release → no-ops). Pairs with arena. The LIN_NO_RC ceiling showed the win.
- [ ] **True inline SSO** — short strings (≤15 B = 100% of interp strings) stored IN the value slot, zero
  heap. The small-string freelist (A6) only reuses; inline eliminates the alloc. Codegen-touching.
- [ ] **Multi-core parallel RAPTOR queries** — the 24 GROUP + 5 RANGE queries are independent; fan out across
  cores via the existing worker/async. Speed, not memory.
- [ ] **Interp cell — call/value box-unbox axis** — Lin's WEAKEST cell (363ms; loses to Python 216 AND Node
  42). Gap is indirect-call + box/unbox on the hot loop, NOT representation (project_interp_profile_measured).
  Separate speed project: devirtualize hot calls + cancel box/unbox across the call boundary.
- [ ] **Shrink LinMap + sealed headers** — pack rc/size widths, share per-type metadata pointers (header
  compaction generalized to maps too).
- [ ] **Stack-allocate more non-escaping records** — extend escape analysis to keep short-lived records off
  the heap (alloca, not malloc) — eliminates the alloc entirely for the common fresh-temporary case.
- [ ] **Broaden the benchmark suite** — dijkstra/pipeline/parallel cells beyond RAPTOR; track regressions in CI.

## Sequencing (HISTORICAL — this was the original plan; A/J/B all executed + merged, see Status)

```
DONE:  A1 A2 A3 A4 A5 A6 | J1 J2 J3 J4   (Wave A + Json retirement — merged)
DONE:  Wave B  (B1 B3 B4 B5 B6 B7 B8 B9 merged; B2 deferred) + bc codegen cleanup
OPEN:  Wave R (memory) · #8 Float32 · B2 (deferred)
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

### Wave B — COMPLETE + MERGED (all byte-identical-IR verified vs staged baselines, or digest-verified)
B1 gate predicate single-source (−440 lines, found IntLit drift) · B3 nkind→size table (found Float32
divergence = bug below) · B4 lower.rs split (10.6k→9 files) · B5 data.rs split · B7 infer_if join+tests ·
B8 is_rc_type shared (found lower.rs's is intentionally narrower) · B9 docs/ADR-069 addendum · B6
TAG_OBJECT comment sweep + lin_object→lin_map rename. **B2 (TagClass walker) DEFERRED** — risky RC
refactor, motivating bugs already fixed. Plus **bc** codegen box/unbox cleanup (5 items: unbox dedup,
type_tag_open delete, box_map_of, BuilderExt::select, RuntimeFns fields). Master `1999bc3e`.

### IN FLIGHT / RESOLVED — parallel lanes launched 2026-06-16 (file-disjoint worktrees, Bedrock sonnet)
Conductor (me) runs the heavy RAPTOR digest+RSS at integration; agents run light gates only.
- [x] **Lane V — value-unbox (#16) → MERGED to master `a63e9603`.** Variable-stride slots (24B homogeneous /
  32B MIXED), ABI preserved via per-thread scratch ring; record-materializers birth maps MIXED
  (`lin_map_alloc_mixed`) to kill the conversion churn (240M→24.6M, ~0.7% residual). Sound + effectively
  NEUTRAL today (RAPTOR digest EXACT, RSS 24278≈24291, ASan A/B identical to master, 123+820+73 green) — and
  becomes a real win once the RAPTOR port stops materializing records to maps. In-house earlier commit `bdc00134`.
  **MEASURED ROOT CAUSE (LIN_VKIND_STATS): 99.99% of maps go MIXED** — first-insert tag is TAG_STR (204.8M) /
  TAG_INT32 (35.1M) then a different tag → the 51.5M dominant maps are **materialized RECORDS** (StopTime/Trip
  `{stopId:String, arrivalTime:Int64,…}`), NOT `{String:T}` index maps. Heterogeneous ⇒ value-unbox can't
  shrink them. **THE HEADLINE LEVER IS THEREFORE: eliminate record→map materialization (keep records PACKED)
  = the 0xFE / Path-9 packed-record campaign**, not map-slot value-unbox. Branch parked, ready for a
  homogeneous-scalar-map workload. See memory `project_linmap_memory_lever`.
- [ ] **Lane U — seal union-ptr fields (#17)** · /tmp/wt-union, `lane/seal-union-ptr` · owns lin-check checker +
  `codegen/boxing.rs`+`types.rs`. Admit single-pointer union fields (interp `Cursor.node`) into sealing.
  STILL RUNNING (no commits yet).
- [~] **Interp Option C (RC elision at Borrow calls) → SOUND but ~0% measured.** `lane/interp-borrow-rc` commit
  `139b35bd`: convention-aware RC elision across Borrow calls + intrinsics (reuses ownership_verify, no new
  analysis), 28 RC ops elided (IR-confirmed), 820/0 + 73/73 + ASan-clean on interp/calc/report/sumtree. Wall:
  2.69→2.71s (NOISE). REFUTES the design's 15-30% — interp is ALLOC/MAP-CHURN bound (lin_map_alloc per AST
  node), NOT RC-bound. Keepable as a general RC-traffic reduction but RC elision is UAF-sensitive → gate on
  RAPTOR digest+ASan before merge. **The real interp lever = Option D: stack-allocate per-frame Cursor/Token
  records (alloc elimination)** — overlaps lane F escape.rs, sequence after F.

> META-PATTERN (2026-06-16): the "clean" repr/RC optimizations (map value-unbox, RC elision) are each SOUND but
> move ~0% on the headline workloads — both bottlenecks are ALLOCATION/MATERIALIZATION, not repr or RC traffic.
> RAPTOR = record→map materialization (lane F 0xFE / packed records); interp = per-AST-node alloc churn (Option D
> stack-alloc). The real wins require attacking alloc/materialization directly.

- [~] **Interp LEAK investigation — agent on `investigate/interp-leak` (/tmp/wt-leak).** The interp benchmark
  leaks **33,960,676 bytes / 1,490,048 allocs PER RUN** under LeakSanitizer — PRE-EXISTING on master (surfaced
  by the Option C ASan A/B, which was byte-identical, so NOT caused by any recent change). Likely an RC
  imbalance or a reference CYCLE (ADR-024 — RC can't collect cycles) in the interp's Token/Cursor/AST graph,
  or benign program-lifetime data held at exit. Agent: confirm real-vs-residual (scale REPS), find the dominant
  leaking alloc site (ASan stacks), root-cause (cycle / missing-release / lifetime), and fix if low-risk.
  If a real leak, this is also part of WHY interp is alloc-bound (it never reclaims per-iteration garbage).
- [~] **SMI dates (#12) → INFRASTRUCTURE merged `7140c05b` but INERT; being ENABLED FOR REAL on `feat/enable-smi`.**
  CRITICAL FINDING (2026-06-16): the `smi` feature is a NO-OP — `lin_box_int32/int64` (tagged.rs:349,363) never
  emit immediates; they still heap-box ("for now, always use heap boxes... consumer guards are the remaining
  work"). So feature-ON passed all gates because SMI does NOTHING (verifying inert code). The encode/decode
  helpers + MANY tagged.rs guards (eq/arith/unbox/retain/release) exist, but the BOX is never flipped and the
  CONTAINER-STORE boundary (lin_map_set val-param, lin_array_push_tagged) is unguarded. Linus chose COMPLETE+ENABLE:
  agent flips the box to smi_encode + completes the store-boundary/retain-release guards, with hard gates that
  prove SMI actually FIRES (LIN_SMI_STATS int_boxes>0) + ASan-clean with SMI LIVE + 820/73 + RAPTOR digest.
  Conductor removes the toggle ONLY after this is proven sound. Data-flow model: SMI immediates are transient
  opaque boxes; container-store converts them to real 16B TaggedVals so freeze/transfer/downstream stay safe.
- [x] **Lane F — 0xFE phase-2 (#9) → MERGED to master `c2f77121` (sound; no RAPTOR win today, keepable win for
  local read-only record arrays + foundation for build-then-store ports).** Conductor-verified comprehensively:
  gate audited (strict allowlist, fails-safe — only Index/FieldGet/SealedArrayFieldGet/Retain/Release promote
  to 0xFE; push/sort/Call/IndexSet/Return/capture/Phi all → 0xFD), workspace 820/0 + 73/73, RAPTOR digest-EXACT
  + no-crash, 0xFE-firing correctness test PASS (10 & 100), ASan-CLEAN on a 0xFE-firing build (channel confirmed
  via UAF control). Regression tests committed: 0xfe_inline_read.test.lin + 0xfe_sort_repro.lin.
  Crash fixed `be60bf77` (removed the unsound container-escape allowance). ROOT FINDING via the repro: RAPTOR
  builds arrays with **store-then-push** (`groups[key]=[]; push(groups[key], trip)`) — fundamentally
  incompatible with 0xFE inline (push corrupts the inline buffer / breaks element addressing). So
  container-escaping arrays MUST stay 0xFD, and 0xFE only ever fires for LOCAL read-only arrays (which RAPTOR's
  hot path doesn't have) → no RAPTOR win, ≈ Phase-1. Gates green (820/0, 73/73, ASan-clean, repro passes).
  **KEEPABLE FOUNDATION (Linus's RAPTOR port):** 0xFE becomes viable IFF the port restructures to
  **build-the-array-fully-THEN-store** (read-only after escape) — then inline saves ~48B/record (header+malloc+
  ptr) AND avoids read-materialization. Parked as a sound gate, ready for that port shape. Branch unmerged.

### OPEN — concrete next items (post-Wave-B)
- [ ] **Wave R** (above) — per-kind attribution DONE → the 4 lanes above are the chosen levers.
  Headline LinMap value-unbox is ABI-blocked (lane V attempts the thread-local-scratch workaround).
- [ ] **#8 Float32 sealed-record size divergence** — sealed_named_field_kind(Float32)=NKIND_FLOAT64(8B) vs
  physical 4B → over-alloc in the dynamic alloc path. Fix = NKIND_FLOAT32 in table + materializer arm.
  Changes boxing semantics (not byte-identical) → ASan+digest+crafted-test. Attended-grade.
- [ ] **B2 TagClass walker** — deferred (risky RC, low remaining value).
- [ ] **mimalloc as default allocator** — ~10% RSS, policy call.
