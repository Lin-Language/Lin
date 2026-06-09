# Path 9 — End-to-end packed records for the `Json`-read-bound case (the measured RAPTOR fix)

**Status:** Open proposal. **A continuation of [Path 1](path-1-integrate-packed-records.md), not a new
direction** — it is "Path 1 Steps 1+2 landed, Step 3 (heap-field packing) finished, then threaded
end-to-end through one real program (RAPTOR)." Written after the [RAPTOR cost-attribution
profile](path-8-make-functions-free.md) measured exactly what RAPTOR's query phase spends on. **No userland
language change** (it is a representation change behind the grammar's existing struct-vs-map line — the
same no-surface-change claim Path 1 carries; explicitly NOT [Path 5](path-5-value-records.md)'s breaking
value semantics).

**Direction in one line:** RAPTOR is slow because its hot records are `Json` — **631 M of 756 M
`lin_object_get` are linear scans** + ~3.5 B box ops — and the *only* lever the profile shows will help is
making those records packed sealed structs **end-to-end** (loader → index → map store → read), because
*partial* typing measurably **regresses ~13%**. This path is the concrete, sequenced plan to do that, and
it is blocked on two named representation gaps that are Path 1's Step 3.

---

## Why this is its own document (but still Path 1's continuation)
The cost map in [Path 8](path-8-make-functions-free.md) split the perf problem into **two bottlenecks in
two programs**: interp is **call-bound** (→ Path 8's inlining), RAPTOR is **`Json`-representation-bound**
(→ here). Path 1 already owns "make packed records pay off"; this path is *not* a competing idea — it is
the **RAPTOR-specific, end-to-end integration** of Path 1, plus the precise prerequisite chain and the
loader-boundary work Path 1 never scoped. It gets its own file because (a) it is gated on a specific stack
of prerequisites that must land in order, and (b) "thread it end-to-end through one program without a
partial-typing regression" is itself a design problem Path 1's per-operation framing doesn't address.

## The measured target (from the RAPTOR profile — see [[project_raptor_json_read_bottleneck]])
Query-phase hot-call counts, baseline (env-gated counters; digest `group=26203913 range=773022892
journeys=139` held):
- **OBJECT_GET 756 M, of which 631 M (83%) are LINEAR SCANS** over small (<16-key) objects — every `Json`
  trip / stopTime / leg / journey field read. (Verified in codegen: a `Json` value's read is *always*
  `lin_object_get`; const-offset `FieldGet` exists only for sealed records. Verified in
  `lin-runtime/src/object.rs`: objects with `len < HASH_INDEX_THRESHOLD` (16) *always* linear-scan.)
- **~3.5 B value-box ops** (TAGGED_RELEASE 1.37 B / CLONE 1.12 B / ALLOC 1.02 B).
- MAP_GET 269 M — **O(1), NOT the problem** (the `{String:T}` dictionary typing win is already banked on
  master: PREP 144 s → 25.7 s, `8859f713`). Map cost is a dead lever; do not re-chase it.
- STRING_ALLOC 203 M — the round-key `"${k}"` frontier; secondary, separate.

**The trap (measured, load-bearing):** typing trips *partially* — a `Trip` read back from a still-`Json`
source — **REGRESSES ~13%** (GROUP +19%, RANGE +11%), because reading a `Json`-built value as a typed
record **materializes a fresh sealed struct per access** (SEALED_ALLOC 65 M, RC_RETAIN 778 M → 2.09 B) on
top of the unchanged scan. This is the mechanism behind the old H5/H9 "typing regresses 2×." **So the fix
must be all-or-nothing end-to-end** — there is no incremental typing path that monotonically improves.

### ✅ Phase-0 call-site-CLASS profile (branch `investigate/raptor-callsite-class`, `5353d8eb`) — the aggregate `OBJECT_GET` counter, split by static type-class, and it sharpens this path's mandate
The retrospective in [path-0](path-0-prerequisites.md) flagged that the aggregate `OBJECT_GET` counter conflated two cost-classes with opposite fixes (dict-lookup, type-it-away vs record-read, must-pack). That split was finally **built and measured** (a `lin_count_object_get_class(class_id)` emitted at every index/field-get site in codegen, classifying the receiver's *static* checked type; `LIN_COUNT`-gated, zero-cost off). The result **strengthens this path and closes the "is there still a free dict win?" question**:
- **RAPTOR query-phase container reads 1.03 B** (object_get 756 M + map_get 269 M), by static class: **RECORD field-read 151 M (14.8%)** (named records `RaptorIndex`/`Service`/`ScanResults` with heap fields → currently boxed), **`Json`-DICT 209 M (20.3%)** (already on the O(1) hashed-map path — the banked PREP lever), **OPAQUE `Json` 666 M (64.9%)** (the `trip[…]`/`stopTime[…]` reads in the inner scan — records-in-disguise kept `Json`).
- **Of `lin_object_get` specifically: 20% RECORD, 80% OPAQUE — 100% record-shaped, 0% genuine-dictionary.** *There is no remaining cheap dict-typing win in the query phase* (the 20.3% DICT bucket is already banked); every residual `object_get` is a record/trip read that **only** collapses under end-to-end packing — i.e. exactly this path.
- **interp corroboration (refines [path-8](path-8-make-functions-free.md)):** interp's `OBJECT_GET` is **not** ≈0 — it is 4.93 M, 100% RECORD-class (boxed `Token[]` / `Cursor.node` heap field / `Ast` sum-node project path), i.e. **interp's records are *not* actually sealed at codegen either.** So the two headline benchmarks share *one* root frontier — the boxed heap-field record — more than the call-vs-repr split implied. Path 9's heap-field packing helps **both**; it is the dominant *shared* lever, not a RAPTOR-only fix. See [[project_phase0_callsite_class_profile]].

## The prerequisite stack (in dependency order — each gates the next)

> **⚠️ STATUS UPDATE 2026-06-09 (see RESULTS section at the bottom):** prereqs 1, 2, AND 3 are now BUILT
> (unmerged, parallel branches). Step 1+2 (in-place packed-array ABI) MERGED to master (`acf35a83`). Step
> 3's "real blocker" (the repr-oracle reconciliation) is SOLVED (`d341824d` — it was a stale over-assertion,
> not a deep conflict) and the producer/consumer seal asymmetry is SOLVED (9C, `253ea5f6`). Heap-field
> **String** packing now works end-to-end on `perf/path9a-widen-on-9c` (blocker fixed, gates green,
> linear-scan→const-offset verified). The text below is the ORIGINAL plan; what remains is **9-D** (retype
> RAPTOR's `Json` trips) + the `stopTimes` nested-array (built on `perf/path9-raptor-payoff`) + pre-existing
> leak cleanup — NOT the repr-oracle work, which is done.

1. **Path 1 Steps 1+2 — the in-place packed-array ABI — MERGED to master.** *(Currently only on the
   `path1-packed-records` / `worktree-agent-aa197d6e` branches; `is_packed_scalar_struct` /
   `sealed_array_elem_ptr` / the in-place `for`/`length` redirect are NOT yet on master — verified.)*
   Without this, `length`/`for`/`map` over a packed `Trip[]` materialize the whole array (`sealed_array_to_tagged`,
   cost #2) and packing loses. **This is the gating first move and is independently shippable** (Path 1
   measured ~4.5× on packed-scalar iteration, all gates green). **Do this first, regardless of Path 9.**
2. **Path 1 Step 3 — heap-field packing — the repr-oracle reconciliation.** `Trip` is *all heap fields*
   (`tripId: String`, `stopTimes: StopTime[]`, `service: Service`), so it never matches today's
   scalar+Bool gate (`Type::is_sealed_array_field_packable`, `lin-check/src/types.rs:219`). The machinery
   to pack heap fields was **built, shipped, then narrowed back out** (documented at that gate) for a
   precise reason: packing only wins when read **by const-offset through a typed param**; on the
   generic/boxed iteration path mechanism (i) *materializes the whole element per read* — strictly worse.
   So Step 3 cannot re-land until **(2a)** the cheap-typed-reads spike (`spike/cheap-typed-reads`: borrowed
   const-offset heap-field read, no materialize) lands **on top of** Step 1's in-place iteration, **and
   (2b)** the repr oracle/verifier is reconciled with the widened gate at the `T|Null`/union/`Index`
   boundary (the precisely-located §H4/H5 divergence — Path 1 names this the "highest-risk step, multi-day
   repr-pass reconciliation"). **This is the hard core and the real blocker.**
3. **The `stopTimes: StopTime[]` inner-array gap.** Even with Step 3, a packed record whose field is itself
   a record-array still boxes the inner array. A packed `Trip` must hold its `StopTime[]` as a packed
   contiguous buffer (or a pointer to one) for the nested `trip["stopTimes"][i]["arrivalTime"]` read — the
   actual hot read — to be const-offset. This is the nested-packed-array case Path 1's Step 3 lists.
4. **The loader `Json → Trip[]` decode boundary.** `loadGTFS` builds trips as `Json` literals from CSV
   rows; `createRaptor(tripsIn: Json)` ingests them. For end-to-end packing the loader must produce
   *packed* `Trip[]`, which needs a validated decode at that boundary. **Good news: the machinery exists**
   — `lin_from_json(value, descriptor)` (ADR-031, `lin-runtime/src/decode.rs`) already does type-directed
   `Json`→typed-value validation. The work is *wiring it to emit the packed representation* for a
   record-array target, not building a decoder. (Alternatively: type the loader's row-construction
   directly so trips are born packed, skipping a decode pass — measure which is cheaper.)

## The end-to-end thread (once 1–4 exist)
With the prerequisites in place, the RAPTOR change is the retype that previously regressed — but now
monotonic because every stage is packed and no materialization boundary is inserted:
- `loadGTFS` → packed `Trip[]` (step 4).
- `createRaptor(tripsIn: Trip[])`, `tripsByRoute: { String: Trip[] }` storing already-packed trips.
- the scan reads `trip["stopTimes"][i]["arrivalTime"]` as nested const-offset loads (steps 2+3), no
  per-access materialize, no linear scan.
- `sort` over `Trip[]` (the segfault that blocked this is **fixed on master** — verified, via the
  `fix/sealed-array-map-repr` merge).

**Acceptance is the inverse of the trap:** the retype must now show OBJECT_GET's 631 M linear scans
collapse (toward 0 on the typed reads) AND GROUP/RANGE wall-clock *drop*, with the digest byte-identical —
i.e. the thing partial typing could not do. If any stage is left `Json`, the materialization boundary
reappears and it regresses; the gate is "no `lin_object_get` linear scan in the typed scan hot path" in IR
**and** the wall-clock win.

## What this fixes / does not fix
- **RAPTOR query phase (the 631 M scans + 3.5 B box ops):** yes — the only measured lever for it.
- **All `Json`-record-read-bound code:** yes, transitively (the packed-by-default gate widening helps any
  fixed-key record array, not just RAPTOR).
- **interp:** no — interp's records are already sealed; its cost is the calls ([Path 8](path-8-make-functions-free.md)).
  These two are **non-competing, parallel** workstreams.
- **The 203 M round-key string allocs:** no — separate frontier (string interning).
- **Construction/RC in absolute terms:** packing removes per-element-per-field box ops on the typed path,
  but the H12 ceiling test says alloc/RC *in isolation* isn't recoverable; the win here is the *reads*
  (scans → loads) and the *boxing boundary*, not arena-style alloc elimination.

## Cons / risks
- **The whole value is behind Step 2's repr-oracle reconciliation** — the multi-day, highest-risk
  §H4/H5 packed-vs-boxed-classification work Path 1 names. Until that lands soundly, heap-field packing
  re-regresses (interp ~3×, the TLV crash) — *this is why it was narrowed out before.* Path 9 is gated on
  it, not a way around it.
- **All-or-nothing means a big-bang retype** at the end — but the prerequisites (1–4) are each
  independently testable/shippable, so only the final thread is big-bang, and it's digest-gated.
- **The recurring packed/boxed-mismatch UAF class** (the `project_repr_pass` / `project_sealed_*` saga) is
  the risk surface at every boundary where a packed record meets `Json`/union/FFI/transfer; the repr
  oracle/verifier must cover each. ASan `detect_leaks=1`-with-scaling mandatory.
- **It only helps `Json`-read-bound programs.** Worth doing because RAPTOR is the headline benchmark and
  the profile proves nothing else helps it — but it is not a general speedup the way Path 8 Tier 2 is.

## Relationship to the other paths
- **Continuation of Path 1** — literally Steps 1+2 (merge) → Step 3 (finish) → end-to-end thread. If Path 1
  Step 3 lands generally, Path 9 is "apply it to RAPTOR + the loader decode."
- **The non-breaking form of [Path 5](path-5-value-records.md)** — Path 5's cost diagnosis was right but
  its value-semantics mechanism is breaking (records are mutable references, verified). Path 9 reaches most
  of the same layout/read benefit via *representation* (packed-by-default) with **no semantic change**.
- **Parallel and non-competing with [Path 8](path-8-make-functions-free.md)** — Path 8 fixes call-bound
  code (interp), Path 9 fixes `Json`-read-bound code (RAPTOR). Different programs, different costs, can
  proceed at once.
- **Supersedes Paths 3/4/7 for RAPTOR** — the profile shows RAPTOR is read-bound, not alloc-bound, so
  arenas/regions/GC are not its lever.

## Acceptance gates
Per prerequisite: the shared gates (full `cargo test --workspace`; sealed/iterator harness + IR-mechanism
assertion; RAPTOR digest byte-identical `group=26203913 range=773022892 journeys=139`; ASan
`=0` AND `=1`-scaling; cross-language non-regression). For the final end-to-end thread specifically: the
**mechanism** (`lin_object_get` linear-scan count in the typed scan hot path → ~0; `sealed_array_to_tagged`
→ 0) **and** the **wall-clock** (GROUP/RANGE median ≥5 at low load must *drop* vs the `Json` baseline — the
inverse of the measured −13% partial-typing regression).

## Verdict
The measured fix for the `Json`-read-bound half of Lin's perf problem, and the reason RAPTOR specifically
is slow: 631 M linear-scan reads + 3.5 B box ops on a boxed-`Json` record model that *partial* typing only
makes worse. It is **not a new idea** — it is [Path 1](path-1-integrate-packed-records.md) carried to
completion (Steps 1+2 merged, Step 3's repr-oracle reconciliation finished, the `stopTimes` nested-array
gap closed, the loader decode wired) and threaded end-to-end through one real program without inserting a
materialization boundary. Its prerequisites are a strict stack — **start by merging Path 1 Steps 1+2 (the
in-place ABI, independently a ~4.5× win and the gating first move), then the cheap-typed-reads spike, then
the repr-oracle reconciliation that has blocked heap-field packing twice.** It is the non-breaking
representation alternative to Path 5's value semantics, parallel to and non-competing with Path 8's
call-cost lever (they fix different programs). Pursue it for the `Json`-read-bound workloads; lead the
overall effort with whichever of {Path 8 Tier 2, Path 1 Step 1} is lower-risk-per-win — both are gating
first moves for their respective halves.

---

## ✅ RESULTS 2026-06-09 — 9C seal-propagation + 9-A String packing: BLOCKER CLEARED, mechanism VERIFIED (my exploration line; all UNMERGED, parallel)

Three of this path's prerequisites are now built and I verified them myself. Branch lineage (each cut from the prior, off master): `perf/path9b-repr-reconcile` → `perf/path9c-seal-propagation` → `perf/path9a-widen-on-9c`. (Sibling parallel explorations reached overlapping states via different routes: `perf/path9c-face2` (String, different worker-boundary fix), `perf/path9-raptor-payoff` (String + nested record-array + RAPTOR retype) — RECONCILE all three when comparing notes.)

### Prereq 1+2 — cheap borrowed heap-field read + repr-oracle reconciliation (on the `path9b`/`path9c` base)
- **Cheap read** (`aa7a550d`): a packed heap-field read is a const-offset borrowed `load ptr` + retain-if-escapes, NOT a per-access materialize — the fix for the H6 dead-end.
- **Repr-oracle reconciliation** (`d341824d`): the multi-day §H4/H5 blocker was a STALE OVER-ASSERTION in repr.rs's `Index` oracle/verify arms (asserted sealed-typed ⇒ Packed, but `compile_ir_index` is repr-adaptive with a sound Boxed path). Fix relaxes the Index forward arm only. NOT a deep union conflict.

### Prereq 3 — 9C symmetric seal-propagation (`perf/path9c-seal-propagation`, commits `253ea5f6`+`a26a2cc9`, checker-only)
The blocker that crashed every prior heap-field widen was a **checker producer/consumer seal ASYMMETRY**: the consumer reads `trip["stopTimes"]` as sealed/packed (from the `Trip` annotation), but the producer's object literal fell to undirected inference → `Array(Object{sealed:false})` → BOXED. Fix: `expected_field_needs_directing` (expr.rs:1579) now returns true for a sealed record / sealed-record-array, so `check_object_fields` DIRECTS those fields and the producer seals to match the consumer — **gated on `is_sealed_array_field_packable` per field**, so it auto-extends when the gate widens, with no further checker change.
- **Fixes LIVE silent data corruption (I verified both sides):** a nested all-scalar sealed-record-array read `outer["items"][0]["a"]` printed garbage **`7 0` on master** → correct **`33 44`** with 9C. Producer now emits `sealed_array_alloc`.
- **Gates I ran:** 696 integration + 72/72 lin + RAPTOR 9/9 + fmt all green; gate stays scalar+Bool (9C is checker-only). **Worth landing for the corruption fix alone, independent of perf.**

### 9-A — gate WIDENED to String + the worker-transfer blocker FIXED (`perf/path9a-widen-on-9c`)
With 9C in place, widening `is_sealed_array_field_packable` to `Str|StrLit` (types.rs) makes the producer/consumer symmetric for String-field records. The one remaining blocker (a hard `lin test` gate) was **`examples/event-transfers`**: a packed sealed-record array crossing the std/event worker boundary arrives BOXED (elem_tag 0xFF), but `lin_sealed_array_push_struct_retaining` / `lin_sealed_array_to_tagged` assumed a packed 0xFE buffer → `[null]`/misaligned-deref (PRE-EXISTING — scalar records hit it too; String widening merely exposed it).
- **Fix (commit `c001352c`, runtime `array.rs`):** new `lin_sealed_array_push_struct_retaining_named` — on a 0xFE array delegates to the packed copy; on a BOXED array materializes the struct to a boxed `LinObject` and does a retaining tagged push. `lin_sealed_array_to_tagged` returns a +1 alias on a non-0xFE array instead of striding. Made the runtime primitives **representation-agnostic** at the boxed/packed boundary (most contained — fixes any boxed-vs-packed mismatch, not just std/event). Codegen `Push` passes the named descriptor.
- **MECHANISM VERIFIED BY ME (load-independent, definitive):** typed String-record read `ts[i]["text"]` — **master = 2 `lin_object_get` (linear scan), 0 packed; branch = 2 packed const-offset reads (`sealed_array_elem_ptr`+`sealed_fld`), linear-scan eliminated.** This is the linear-scan→const-offset conversion the whole path targets. (Agent measured ~5.4× wall-clock on a typed-index read; I could not reproduce the *number* — my microbenches were loop-invariant and LLVM-hoisted to ~0.019s — so I trust the IR mechanism, which is unambiguous.)
- **Gates I ran:** 696 integration + 72/72 lin test (incl. **event-transfers** — the blocker) + fmt + RAPTOR digest byte-identical; String value-correctness (filter/map/reduce/index over `Tok[]` → correct).
- **Leak truth (I isolated it — the agent's "constant residual" was imprecise):** the new gate-widen + boxed-push primitive add **NO leak** (read/index path = 10 B constant; the new primitive's ASan regression test is RC-balanced, no UAF). The scaling leaks visible on this branch are **two PRE-EXISTING bugs it inherits**: (1) the Step-8.1 multi-stage-chain leak (scalar chain leaks identically; fixed only on `perf/path8-step1-record-fusion`, not here); (2) the `range().map()`-into-sealed-array BUILD leak (String build ≈ scalar build, byte-identical → pre-existing on master).
- **Scope:** String only (Array/Map/nested kept boxed). **RAPTOR digest UNCHANGED** because `tripsByRoute` is still `Json`-typed in the `.lin` source — RAPTOR realizes the win only after its trips are retyped to concrete `Trip` (the end-to-end **9-D** thread, explored on `perf/path9-raptor-payoff`: correct + digest-exact but not-yet-a-win, gated on a packed-record drop-walk codegen fix + the boxed `Trip|Null` construction).

### Net (this line)
Heap-field **String** packing is shippable-quality end-to-end on `perf/path9a-widen-on-9c`: blocker fixed, all hard gates green, the linear-scan→const-offset mechanism verified. The remaining work to the **headline** win is **9-D** (retype RAPTOR's `Json` trips → `Trip`) + cleaning up the two inherited pre-existing leaks (one already fixed on the 8.1 branch). The repr-oracle blocker that stalled this path for weeks is solved; the producer/consumer asymmetry is solved; what's left is plumbing (typed RAPTOR source) and pre-existing leak cleanup, not a new design problem.

---

## ✅ RESULTS 2026-06-09 (second exploration line) — 9D MEASURED the end-to-end thread + the 9C-branch reconciliation (all UNMERGED)

A parallel exploration line (branch lineage `perf/path9c-face2` → `perf/path9c-nested-materializer` → `perf/path9d-measure`) carried the end-to-end thread to a **measured verdict** and reconciled the competing 9C branches. This refines — and in one place corrects the *root-cause* attribution of — the "not-yet-a-win" status above.

### 9-D — end-to-end RAPTOR retype: MEASURED, REGRESSES catastrophically, **confirms the all-or-nothing thesis AND locates the precise remaining blocker**
`perf/path9d-measure` (head `15a5fbab`, off the full-retype `perf/path9-raptor-payoff`, current master merged). Trips typed end-to-end (`createRaptor(tripsIn: Trip[])`, `tripsByRoute: { String: Trip[] }`, scan path threaded with a concrete `Trip` + a `Boolean hasTrip` deliberately avoiding the boxed `Trip|Null` union, `Service.days/dates: { String: Boolean }` to make `Trip` packable, loader reached).
- **Correct** — 9/9 RAPTOR `.test.lin`, `run.lin` prints the right journey, 696 integration + 72/72 lin green; the `Json` baseline (same compiler) reproduces the digest **byte-identical** `group=26203913 range=773022892 journeys=139`.
- **Performance: CATASTROPHIC regression — the query phase ran 16+ min CPU at 25 GB RSS and *did not complete*** (baseline finishes ~7.5 min at 2.4 GB; killed). Wall-clock A/B was therefore *infeasible* (typed never completes) — reported as a non-completion, not a median.
- **Mechanism SPLIT (the diagnostic):** static `lin_object_get` per hot fn, Json → typed:
  - concrete-`Trip`-**param** fns WIN: `scanRouteAt` 6→2, `getRouteId` 4→0, `indexRoute` 6→4 (const-offset reads).
  - **map-READ** fns REGRESS: `getTrip` 7→**28** (+2 `sealed_array_to_tagged`), `scanBack` 8→**13** (+a new `lin_sealed_array_alloc`+push).
- **ROOT CAUSE precisely located — the `{ String: Trip[] }` MAP-VALUE seam** (this path's Prereq #3/#4, NOT closed): `routeTrips = get(scanner["tripsByRoute"], routeId)` reads a `Trip[]` out of a **boxed** map, so `routeTrips[i]["stopTimes"][k]` **materializes the entire `StopTime[]` per access** (a `lin_sealed_array_alloc` + per-element push loop) inside the *innermost backward-scan loop* — O(stopTimes) work + a leaked array every step. This is the exact H5/H9 materialize-on-read regression, now pinned to the map boundary rather than vaguely "construction." Functions reading a `Trip` *param* got the genuine const-offset win; functions reading trips *out of the map* regress and dominate.
- **This is a *refinement* of the `perf/path9-raptor-payoff` "gated on a drop-walk codegen fix + boxed `Trip|Null` construction" note above:** the dominant cost is not the drop-walk or the union construction per se — it is the **per-access whole-array materialize at the boxed-map-value read** in the hottest loop. ASan: leak-only, linear-scaling, ~7× amplified by the per-read materialize, **no new mem-safety class**.
- **VERDICT: CONFIRMS all-or-nothing.** The concrete-param win is real but swamped by the map-read regression; leaving the map value boxed reintroduces the materialize boundary exactly as the thesis predicted.

### The remaining blocker → **Path 9-E: extend `BoxKeepPacked` to MAP VALUES**
The single thing between current capability and RAPTOR's win: a `Trip[]` read out of a boxed `{ String: Trip[] }` map must be a **borrowed const-offset packed** read, not materialize-per-access — i.e. extend the `BoxKeepPacked` direction (see `project_repr_pass` / ADR-062) from record fields to **map values**. 9-B/9-C/9-A covered record fields and the worker boundary; **none covered map values.** Until that lands, no end-to-end RAPTOR thread can avoid the regression — this is now the well-scoped next move, not a new design problem.

### 9C-branch reconciliation — the two competing 9C implementations: use `perf/path9c-face2`, do NOT merge the seal-propagation branch on top of it
Two branches fix the **same** nested-all-scalar sealed-record-array corruption (`outer["items"][0]["a"]`: master `7 7 0 7 0` garbage → both `7 11 22 33 44` correct) at **different layers** — verified by running both repros on both branches:
- **`perf/path9c-face2`** (codegen + repr): makes `compile_ir_sealed_array_field_get` **repr-adaptive** (runtime `elem_tag` dispatch: 0xFE→const-offset, else boxed) + projects boxed→packed in `unbox_value` + **widens the gate to String**. Fixes the corruption on the **read side** (boxed-tolerant).
- **`perf/path9c-seal-propagation`** (checker only): seal-propagates the producer literal so it builds packed to match the consumer; **gate stays scalar+Bool**. Fixes the corruption on the **write side**.
- Each is green **alone** (face-2 690/0, seal-prop 696/0; both pass the RAPTOR tail-recursive UAF regression test → `190000`). They cherry-pick cleanly — **but the COMBINATION IS UNSOUND:** the combined build crashes `test_union_record_nested_field_tail_recursive_param_no_uaf` (misaligned deref `0xa` at `object.rs:218`). Mechanism: face-2's String-gate widen makes `StopTime{stop:String,…}` packable → seal-propagation then auto-seals the producer to a **packed** nested array → the `Trip|Null` union-box + tail-recursive re-box path can't handle a packed nested heap-field array → crash. (This is *exactly* what the seal-propagation commit message predicted would happen once the gate widens.)
- **RECOMMENDATION:** base **9-E / the RAPTOR thread on `perf/path9c-face2`** — it already widens the gate and carries the repr-adaptive read path 9-E needs, and stays sound under widening because its reads are repr-adaptive rather than producer-contingent. The seal-propagation branch's producer-symmetry is worth landing **only** for its corruption fix in isolation, or later once codegen handles packed nested arrays through the union-box + TCO re-box path (the same codegen work as face-2's still-open `sealed_array_rebuild_from_boxed` large-array defect — a packed scalar `{a:Int32}` threaded repeatedly through a generic indirect call at N≳realloc size silently yields `0`). **Do not naively merge the two** — it reintroduces the `Trip|Null` tail-recursive nested-heap-field UAF.

### Branch map (this line, all UNMERGED, off master; durable handles)
| Branch | Carries | Status |
|---|---|---|
| `perf/path9a-cheap-typed-reads` | cheap borrowed heap-field read (9-A capability), gate dormant scalar+Bool | verified, dormant |
| `perf/raptor-dict-on-json-typing` | dict→Map (`routeScanPosition` memo), 783 k dict reads→`map_get` | digest-identical, fidelity (not a measurable speedup) |
| `perf/path9c-face2` | repr-adaptive reads + boxed→packed projection + **String gate** + nested-array materializer fix | the recommended 9-E base |
| `perf/path9c-seal-propagation` | checker-only producer seal-propagation, gate scalar+Bool | green alone; land for corruption fix only, NOT on top of face-2 |
| `perf/path9d-measure` | full RAPTOR retype + the measured regression + NOTES | the verdict + the located map-value blocker |

### ✅ RESULTS 2026-06-09 (third exploration line — the orchestrated Phase-0→9 fan-out) — independent route to String + **nested-record-array** packing; reconciled with the lines above
A separately-orchestrated fan-out (Phase-0 profiler first, then a 9-A→9-A-cont→9-B branch stack, then the end-to-end measurement) reached the **same capability state by a different code route**, which mutually corroborates the two lines above and adds one piece neither named explicitly — the **codegen-side** nested-record-array fix. All UNMERGED, durable handles.

- **9-A repr-oracle reconciliation** (`worktree-agent-a0d957eb066312ec5`, `d50f7ac5`) and **9-A-cont String gate + worker-transfer** (`worktree-agent-aca959b609c365316`, `3a33ad1d`): converged on the **identical root cause** as `perf/path9b-repr-reconcile`/`perf/path9c-face2` (the stale `Index` over-assertion; the boxed↔packed worker boundary) — independent rediscovery, raising confidence the diagnosis is correct. The 9-A-cont line *also* pinned a distinct contributing bug: an **unannotated empty `[]` flowing into a generic param resolved only by a later lambda arg** stays `Array(Never)` → descriptor-less `lin_array_alloc` → a later packed push walks a garbage descriptor (fixed in `checker/call.rs`: defer the empty-array sub-collection for a bare-`TypeVar` param, re-check once the lambda binds it).
- **9-B nested record-array packing** (`perf/path9b-nested-record-array`, `65471ba2`): widens the gate to `Array(sealed-record)` (recursing) so `Trip{stopTimes:StopTime[]}` packs, with the inner array held **pointer-to-packed-buffer**. **Root cause was NOT the `__sealedarrmat_*` materializer offsets** (those were correct) — it was `Codegen::sealed_repr_differs` (`codegen/data.rs`) having a blanket `Array↔Array ⇒ no-repr-change` short-circuit that stored a **boxed `Object[]` literal verbatim into a packed field slot**; the reader then strode it as a `0xFE` buffer → garbage/`0x07`. Fix routes that store through `sealed_array_project_from` (fresh +1 packed buffer). **Reconciliation with `perf/path9c-face2`:** these are two layers of the *same* nested-array corruption — face-2 fixes it on the **read** side (repr-adaptive `elem_tag` dispatch, boxed-tolerant), 9-B fixes it on the **construct/store** side (producer emits a packed buffer). They are complementary (read-side robustness + write-side correctness); 9-B's value-correctness was verified (built `Outer[]`/`Trip[]` == read-back). Whichever base 9-E builds on should carry the **read-side** repr-adaptive fix (face-2) for soundness under widening; 9-B's store-side fix is the producer half and should be reconciled in rather than duplicated.
- **End-to-end 9-D** was first measured on this line's `perf/path9-raptor-payoff` (`1ac2a339`) — correct, digest-exact, not-yet-a-win — which the second line's `perf/path9d-measure` then **built directly on top of** (it descends from `perf/path9-raptor-payoff`) and sharpened to the **`{String: Trip[]}` map-value seam** root cause + the **Path 9-E** scope above. So the two "9-D" notes are one continuous result: my line produced the correct-but-regressing thread and the initial drop-walk/`Trip|Null`-construction attribution; the continuation re-attributed the *dominant* cost to the boxed-map-value materialize-on-read. **Trust the `perf/path9d-measure` attribution (map-value seam → 9-E `BoxKeepPacked` for map values); the earlier "drop-walk + union-construction" framing is a contributing-but-secondary factor.**

### Branch map (third line)
| Branch | Carries | Status |
|---|---|---|
| `investigate/raptor-callsite-class` (`5353d8eb`) | the Phase-0 per-call-site-CLASS profiler (record/dict/opaque split) | the measurement that reframed both paths; reusable, dormant when `LIN_COUNT` unset |
| **`perf/path9b-nested-record-array`** (`65471ba2`) | **the consolidated stack** — 9-A repr-oracle (`7f71229a`) + 9-A-cont String gate/worker-transfer/empty-`[]`-fix (`3b73e7d7`) + 9-B nested-record-array store-side `sealed_repr_differs` fix (`bc7de3ad`) + value-correctness test | verified-by-me; **this is the durable handle for the whole third line** — the store-side half to reconcile into the face-2 read-side base |
| `worktree-agent-a0d957eb066312ec5` / `worktree-agent-aca959b609c365316` | the original 9-A / 9-A-cont worktree branches | **ephemeral — content rebased into `perf/path9b-…` above; do NOT cherry-pick these, use path9b** |
| `perf/path9-raptor-payoff` (`1ac2a339`) | full RAPTOR retype (parent of `perf/path9d-measure`) | the correct-but-regressing end-to-end thread |
