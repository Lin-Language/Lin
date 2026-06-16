# Lin — Perf / Memory / Quality TODO

> ## ⚑ CURRENT STATE (2026-06-16)
> Master is green (820/0 integration, 73/73 stdlib, RAPTOR digests exact `26203913/773022892/139`).
> Waves A, J, B (correctness bugs + Json retirement + architectural consolidation) are **all merged** —
> see *Merged history* at the bottom. This file now tracks only **live + open** work.

---

## IN FLIGHT — exploration lanes (2026-06-16, file-disjoint worktrees, sonnet, NOT merged)

Each produces a design doc + a minimal POC/spike; conductor reviews + runs heavy benches before any merge.

- [x] **`freeze`-as-repack → MERGED `46cc61f7`.** `frozen(v)` now does a one-time repack: when the deep
  immortal-seal walk hits a 0xFD pointer-spine record array, it allocates a headerless 0xFE inline buffer,
  copies payloads in, swaps `elem_tag→0xFE`, frees the old spine. Sound (frozen contract forbids post-freeze
  mutation; 0xFE read path exists). Verified: 820/0 + 73/73 + workspace + ASan UAF-clean + correct reads. The
  user-signalled build-with-push-then-`freeze` path — eliminates ~48 B/record for program-lifetime indexes
  without the store-then-push hazard. **Apply `frozen(...)` to the loadGTFS return to use it.** (The
  `std/arena` bump-allocator the agent also built is intentionally NOT merged — parked on `explore/arena`.)
- [x] **Columnar (struct-of-arrays) record arrays → MERGED `20876032`.** `0xFC` tag; escape-analysis-driven
  (auto-chosen for read-only non-aliased record arrays, same gate class as 0xFE); field-get = two-ptr-load +
  GEP + scalar-load (good for RAPTOR's departureTime field-scans). Verified: 820/0 + 74/74, fires (POC +
  loop), ASan UAF=0, leak-free on create+drop, **store-then-push hazard handled** (stays 0xFD), **RAPTOR
  digest EXACT** (no perturbation of existing arrays). `docs/design-columnar-arrays.md` has the 3-phase plan
  (Phase-2 push-scatter fusion + Phase-3 `@columnar` RAPTOR integration remain). NEXT: measure the actual
  RAPTOR field-scan win once the port uses typed records + `frozen()`.
- [ ] **True inline SSO** (`explore/sso`) — DESIGN DOC + spike done (`docs/design-inline-sso.md`); ≤15 B inline,
  handles ≤7 B = 100 % of interp/dijkstra hot strings. **DEFERRED behind the value-record repr reset** (the
  agent flagged the same "guard every consumer" fragility that sank SMI, plus it tangles with the string field
  layout in sealed structs). Revisit after the repr reset.
- [×] **Interp Option D — stack-alloc Cursor/Token** (`explore/interpd`, NOT merged) — verified sound (820/820,
  ASan-clean) but **0 % on interp: the Cursor/Token records are RETURNED up the parse chain → they escape the
  frame → 0 stack allocas fire.** Stack-alloc is the wrong tool; the interp lever is arena/region (the `freeze`
  lane). Left on the branch for reference; not pursuing.

---

## OPEN — decisions / smaller items

- [x] **SMI dates (#12) — DROPPED + STRIPPED from master (`51febe63`, −811 lines); full impl on `reference/smi`.**
  DECISION (2026-06-16): the manually-typed RAPTOR port (`lin-manually-typed/`) is FULLY TYPED — zero AnyVal,
  dates are `UInt32` stored *unboxed* (record fields), as *raw* integer map keys (`{DateNumber: Boolean}`), and
  as scalar map values. None call `lin_box_int`, which is SMI's ONLY optimization target. So SMI fires ZERO
  times on the actual direction of travel ("dates as ints" = typed UInt32 scalars, the opposite of what SMI
  helps). The real levers here are value-unbox (homogeneous `{_:UInt32}`/`{_:Boolean}` maps → 8B slot — already
  merged) + de-materialization. Kept on `reference/smi` for any future genuinely-untyped int-boxing workload.
- [ ] **mimalloc as default allocator** — ~10 % RSS + 3–5 % wall-clock, one-liner already behind a feature.
  A default-allocator/CI/platform POLICY call for Linus (flip default-on, or leave opt-in). The only real
  open *decision* here.

---

## BLOCKED ON the RAPTOR-port restructuring (parked compiler foundations — merged, sound, 0 % today)

These are real wins the moment the port changes how data is *written*:

- **0xFE inline record arrays** (merged `c2f77121`, sound) → fires for RAPTOR's `Trip[]`/`StopTime[]` once the
  port **builds each array fully THEN stores it** (read-only after escape), instead of store-then-push
  (`groups[k]=[]; push(groups[k], trip)`) which is fundamentally incompatible with inline (push corrupts the
  buffer). Then inline saves ~48 B/record (header+malloc+ptr) AND avoids read-materialization.
- **value-unbox** (merged `a63e9603`, neutral) → wins once the port **stops materializing records into maps**
  (99.99 % of the 51.5 M dominant maps are materialized records — heterogeneous → can't shrink; genuine
  `{String:T}` index maps DO get the 8 B-slot win).

---

## Bigger future levers (not yet scoped)

- **Multi-core parallel RAPTOR queries** — the 24 GROUP + 5 RANGE queries are independent; fan out via the
  existing worker/async. Speed, not memory; unscoped.
- **Broaden the benchmark suite** — dijkstra/pipeline/parallel cells beyond RAPTOR; track regressions in CI.

> *Dropped (won't do):* **Header compaction 24→16 B** — subsumed by `freeze`-repack/0xFE/columnar (which
> remove the per-element header *entirely* for the dominant array-held records), ~100-site UAF-risky for ~8 B
> on the standalone-record minority, and conflicts with the freeze/columnar lanes. **B2 tag-walker unification**
> — deferred all session; the motivating UAF bugs (#4/#5) are long fixed; low value. **#8 Float32** — DONE
> (NKIND_FLOAT32 shipped). **SMI** — dropped (see above; on `reference/smi`). **Interp stack-alloc (Option D)**
> — doesn't fire (Cursor escapes); the lever is the `freeze`/arena lane.

---

## Key measured findings (don't re-derive)

- **RAPTOR 25 GB is allocation amplification, not the allocator.** ~265 M live allocs / ~100× per logical
  record; mimalloc/arena-max/glibc all ≈ equal. Per-kind: **maps 15.25 GB (76 %)**, sealed 4.47 GB. The maps
  are *materialized records*, not index maps. → the lever is **stop materializing / keep records packed**.
- **interp is alloc-bound, not RC- or repr-bound** — per-AST-node `lin_map_alloc`. RC-elision moved ~0 %;
  Cursor-sealing (lane U) moved map_gets 1.66 M→0; the leak fix drained 34 MB/run → 424 B. Next = stack-alloc.
- **The "clean" repr/RC optimizations are sound-but-~0 % on the headline workloads** — both bottlenecks are
  ALLOCATION/MATERIALIZATION, which live in *how programs are written*, not the representation.
- **Process lessons:** (1) agent self-reports are NOT trustworthy — conductor re-runs every gate. (2) stale /
  feature-mixed incremental builds give *flaky* test failures — `cargo build --workspace` first, then test,
  `cargo clean` between feature states (per CLAUDE.md). (3) "tests pass" ≠ "every deref guarded" (SMI/SSO).

---

## Merged history (this session, on top of Waves A/J/B)

| Commit | What |
|---|---|
| `99f01a81`+ | **Lane U** — seal single-pointer union fields (Cursor.node); interp map_gets 1.66 M→0, leak −10× |
| `05140712` | **Interp leak fix** — String-TCO under-release; 34 MB/run → 424 B |
| `a63e9603` | **value-unbox** LinMap slots + MIXED-birth churn-fix (neutral; win after de-materialization) |
| `c2f77121` | **0xFE inline** record arrays (sound; win after build-then-store) |
| `139b35bd` | **RC-elision** at Borrow calls (sound; ~0 % — interp is alloc-bound) |
| `51febe63` | **SMI** stripped (inert→enabled→DROPPED; never fires on typed dates; on `reference/smi`) |
| `c8119174` | LinMap INITIAL_CAP 8→4 (−1.4 GB) |
| docs | peak-memory finding (PERFORMANCE.md §4); arena + interp-call design docs |

Prior waves (merged): **A** (6 correctness bugs incl freeze/transfer UAF + RC micro-opts), **J** (Json type
retired → AnyVal), **B** (single-source gate predicate / nkind table / lower.rs split / etc.).
