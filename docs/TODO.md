# Lin — Perf / Memory / Quality TODO

> ## ⚑ CURRENT STATE (2026-06-16)
> Master is green (820/0 integration, 73/73 stdlib, RAPTOR digests exact `26203913/773022892/139`).
> Waves A, J, B (correctness bugs + Json retirement + architectural consolidation) are **all merged** —
> see *Merged history* at the bottom. This file now tracks only **live + open** work.

---

## IN FLIGHT — exploration lanes (2026-06-16, file-disjoint worktrees, sonnet, NOT merged)

Each produces a design doc + a minimal POC/spike; conductor reviews + runs heavy benches before any merge.

- [~] **Arena / bump allocation** (`explore/arena`) — program-lifetime data (RAPTOR's index, built once in
  PREP, never freed) into a bump arena: zero per-object malloc-header / RC / free. Design doc exists
  (`docs/design-arena-allocation.md`); agent refines it + POCs an arena allocator + a region/`frozen`-scope
  routing hook. **Potentially the single biggest RAPTOR memory+speed lever.**
- [~] **Interp Option D — stack-allocate Cursor/Token** (`explore/interpd`) — the *real* interp lever
  (Option C RC-elision was sound-but-0%; interp is alloc-bound). Extend escape analysis +
  `sealed_construct_stack` (today all-scalar only) to stack-allocate non-escaping per-frame records with
  heap-pointer fields. Design doc: `docs/design-interp-call-axis.md`.
- [~] **Columnar (struct-of-arrays) record arrays** (`explore/columnar`) — beyond 0xFE: each field its own
  contiguous column (all `departureTime`s together) → SIMD-friendly, best locality for RAPTOR's field-scans.
  Design + feasibility (`docs/design-columnar-arrays.md`), optional runtime spike.
- [~] **True inline SSO** (`explore/sso`) — short strings (≤15 B = 100 % of interp/dijkstra strings) stored
  in the 16-byte value word, zero heap. Design + an HONEST consumer-audit list (same "guard every consumer"
  discipline that made SMI hard) + win-vs-existing-freelist analysis (`docs/design-inline-sso.md`).

---

## OPEN — decisions / smaller items

- [~] **SMI dates (#12) — ENABLED + working behind the default-OFF `smi` flag (`a1ba97cb`); TOGGLE REMOVAL
  BLOCKED.** It was inert (box never emitted immediates); now flipped + consumer-guarded, VERIFIED feature-ON
  (820/0 + 73/73 + fires `smi_int_boxes=4402` + RAPTOR digest-exact + ASan UAF-clean). Master is byte-identical
  with the flag OFF (safe). **But making it unconditional keeps surfacing consumer-guard bugs (whack-a-mole):
  array-slice fixed (`ed1bf6b3`); regex still SIGSEGVs.** Tests passing is necessary-but-NOT-sufficient (an
  unguarded path with no test is invisible). **RECOMMENDATION: leave flagged until a holistic re-architecture
  (a single typed SMI wrapper + an exhaustive deref-site audit), not ad-hoc fixes.** WIP parked on
  `chore/smi-unconditional`. See memory `project_linmap_memory_lever`, path-10 spike ("11 bugs").
- [ ] **mimalloc as default allocator** — ~10 % RSS + 3–5 % wall-clock, one-liner already behind a feature.
  A default-allocator/CI/platform POLICY call for Linus (flip default-on, or leave opt-in).
- [ ] **#8 Float32 sealed-record size divergence** — *(believed fixed this session via NKIND_FLOAT32; verify
  it's actually landed/closed before deleting this line.)*
- [ ] **B2 — unify tag walkers** into one `TagClass`/`for_each_heap_payload` table (release/retain/transfer/
  freeze dispatch through it → "handled the new tag?" becomes a compile-time exhaustiveness check). Deferred
  all session as risky-RC / low-remaining-value — candidate to close as won't-do unless the SMI/inline-SSO
  "guard every consumer" pain motivates it (it's the same class of problem).

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

- **RC elimination for immortal/program-lifetime graphs** — extend/infer `frozen` so program-lifetime data
  skips RC entirely. Pairs with the arena lane.
- **Header compaction 24→16 B** — one per-type metadata pointer instead of `{size,heap_desc,named_desc}`
  (~8 B/record; ~100-site UAF-risky migration; largely subsumed by 0xFE/columnar for array-held records).
- **Multi-core parallel RAPTOR queries** — the 24 GROUP + 5 RANGE queries are independent; fan out via the
  existing worker/async. Speed, not memory.
- **Broaden the benchmark suite** — dijkstra/pipeline/parallel cells beyond RAPTOR; track regressions in CI.

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
| `7140c05b`/`a1ba97cb` | **SMI** infra→enabled behind default-OFF flag |
| `c8119174` | LinMap INITIAL_CAP 8→4 (−1.4 GB) |
| docs | peak-memory finding (PERFORMANCE.md §4); arena + interp-call design docs |

Prior waves (merged): **A** (6 correctness bugs incl freeze/transfer UAF + RC micro-opts), **J** (Json type
retired → AnyVal), **B** (single-source gate predicate / nkind table / lower.rs split / etc.).
