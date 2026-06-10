# Path 10 — The data adventure: every value is either a flat struct or one register

**Status:** Open proposal. **A complete, self-contained journey** — this document bundles what were
briefly three separate proposals (layout-as-a-kind, the 8-byte tagged value, and ownership
conventions) into one path you can walk end-to-end without reading anything else. Its sibling is
[Path 11 — the call adventure](path-11-the-call-adventure.md); the two are **independent
adventures, not stages of each other** — they attack different measured bottlenecks (this one owns
RAPTOR; Path 11 owns interp), they can run in parallel or alone, and each duplicates the shared
groundwork it needs (Leg 1 here = Leg 1 there; row-shape monomorphization appears in both). If both
adventures run, build the duplicated legs once — they are identical. **No userland language change**;
two allowed strictness tweaks (§Leg 2 and §Leg 1) at seams whose current behaviour is accidental.

**Direction in one line:** make Lin's *data* cost-free — every record whose shape the checker knows
becomes a flat packed struct read at constant offset (the Go/Roc/OxCaml model), every remaining
dynamic value becomes an 8-byte register-sized immediate instead of a 16-byte heap box, and
ownership becomes a verified IR fact so the borrowed fast paths this requires are sound by
construction.

**Finish line (measurable):** RAPTOR GROUP/RANGE within small-integer-multiple of Go (today ~23×),
digest byte-identical; interp's 4.93 M RECORD-class `object_get` → ~0; the packed/boxed-mismatch and
ownership UAF bug classes become compile-time errors.

---

## 1. The measured target (why data, why these three moves)

- RAPTOR query phase: **631 M of 756 M `lin_object_get` are linear scans** over <16-key heap
  objects, plus **~3.5 B box ops** (TAGGED_RELEASE 1.37 B / CLONE 1.12 B / ALLOC 1.02 B). By static
  class, residual `object_get` is **100% record-shaped** (20% RECORD, 80% OPAQUE
  records-in-disguise; 0% genuine dictionary — the dict win is banked: PREP 144 s → 25.7 s).
- **Partial typing regresses ~13%**, and the 9-D end-to-end thread regressed catastrophically at the
  `{String: Trip[]}` map seam (per-access array materialization, 25 GB RSS). Mixed representation is
  strictly worse than either pure one ⇒ the fix is **all-or-nothing by construction**.
- **Not allocation:** the LIN_NO_RC ceiling recovered ~0%; the box *pool* was 3-4% slower; tracing
  GC closed-negative. What's left of the box cost is **width + indirection + ownership churn** —
  representation problems, not allocator problems.
- Path 2's inline caches isolated the read-wrapper cost: a 99.56%-hit-rate IC was still a wash
  because the expense is key-interning + unboxing + tag dispatch + **the owning clone per read** —
  not offset resolution.
- interp shares the frontier: its `OBJECT_GET` is 100% RECORD-class (boxed `Token[]`,
  `Cursor.node`), so this adventure helps both headline benchmarks even though RAPTOR is its owner.

The three legs answer those four facts in order: ownership conventions make borrowed reads sound
(the owning clone), the layout kind makes static shapes constant-offset (the linear scans) without
the mismatch bug class, and the 8-byte value shrinks whatever dynamic seams survive (the box width).

## 2. Why one adventure (the dependency story)

Leg 3 (8-byte + borrowed reads) is unsound without Leg 1 (something must *prove* a borrow never
outlives its source) and unprofitable without Leg 2 (no point re-engineering seams Leg 2 deletes —
its go/no-go gate is a re-profile after Leg 2 lands). Leg 2's stage-1 fixes are already root-caused
and sitting on branches. So the legs are a line, not a menu — which is what makes this a single
adventure.

---

## Leg 1 — Ownership as IR fact: borrow/own/inout conventions + a call-edge verifier
*(duplicated verbatim as Path 11's Leg 1 — build once if both adventures run)*

### The diagnosis
The project's RC bugs are one structural gap in many costumes — TCO param-slot overwrites
(`9a1a735`), releases emitted into unreachable `tco_post` blocks (`b2e6d35`), the pinned
sealed-record param-slot leak (`codegen/types.rs:131`), union-boundary double-frees
(`record_escape_alias`/`own_for_read`), the `Trip|Null` TCO UAF (`object.rs:218`), projection
re-CloneBox leaks, `lower_coerce_arg` double-frees. Ownership is an emergent property of scattered
retain/release emission; the rules exist only as prose (`docs/MEMORY_MANAGEMENT.md`). Meanwhile
every call boundary pays *defensive* RC/cloning because callee intent is unknown (the measured
~13× call-boundary cliff; Lobster's equivalent inference eliminates ~95% of RC ops —
[Lobster](https://aardappel.github.io/lobster/memory_management.html)).

### The move (Swift SIL / Lean "Counting Immutable Beans" model, kept internal)
1. Every `LinIR` function signature declares per-param/per-return conventions: `borrow` (caller
   keeps ownership, callee may not store), `own` (transferred), `inout`. Lowering infers user/stdlib
   functions (read-only-never-escaping → `borrow`; stored/captured/returned → `own`; doubt → `own`,
   today's behaviour). Runtime intrinsics get one hand-audited table in `RuntimeFns`
   (`codegen/runtime.rs`) — e.g. `lin_object_get(borrow, borrow) -> borrow`, `lin_push(inout, own)`.
2. A verifier checks every call edge and block: owned args have owned-and-then-dead sources, borrows
   outlive the call, TCO back-edges release the old param-slot value *before* the store (the entire
   `tco_post` class becomes "verifier rejects emission into an unreachable block"), scope exits
   release exactly the owned-live set. **Run it in shadow mode first** — every violation over the
   full corpus + RAPTOR is either a latent bug (the pinned TCO/union leaks should fall out
   immediately) or a wrong inference. Zero behaviour change until it's clean.
3. Codegen then *consumes* conventions: the per-site heuristics (`own_for_read`,
   `record_escape_alias`, `index_result_is_fresh_owned_box`, the `tco_owns` runtime alias-compare)
   are deleted one at a time, ASan-gated. `rc_elide.rs` keeps running; most of its work disappears
   at the source.

**Allowed strictness tweak:** escaping borrows get a defined retain-on-escape, and the
escaping-`var`-capture class (obj-literal closure-var segfault, worker captured-var garbage) gets a
*checked* capture convention instead of a latent miscompile — behavioural tightenings of
currently-buggy corners.

---

## Leg 2 — Layout as a type-system fact: packed records become THE representation

### The diagnosis
The Path-1/9 campaign shipped its mechanisms (const-offset reads verified; nested
`Trip.stopTimes` packs soundly; digest-exact) and spent most of its calendar on
**representation-agreement bugs**: the 9C producer/consumer seal asymmetry (live `'7 0'` data
corruption), the repr-oracle over-assertion (`d341824d` — a multi-day "deep union conflict" that was
a stale assert), ADR-062's triple-replication class, the 9-D map-value seam. One bug, five times:
**the layout decision lives downstream of the checker in `lin-ir/src/repr.rs`'s per-function oracle,
which re-derives what the checker knew and drifts at every seam** (unions, maps, module edges, TCO
slots, worker transfer). This is exactly why OxCaml made layout a **kind tracked in the type
system** ([unboxed types](https://oxcaml.org/documentation/unboxed-types/intro/)), why Roc has *no*
uniform boxed representation where the type is known
([roc-lang.org/functional](https://www.roc-lang.org/functional)), and why Cinder compiles annotated
field access to "three machine instructions"
([Static Python](https://github.com/facebookincubator/cinder/blob/cinder/3.8/CinderDoc/static_python.rst)).

### The move
1. **Land the two pinned blockers on the current architecture first** (root-caused, on branches):
   9-E map-value keep-packed (`perf/path9e-map-value-keeppacked` — repr STEP-4 propagation through a
   map-field-record array literal) and the TCO sealed-record param-slot drop (now also a Leg-1
   verifier finding). Re-run the 9-D end-to-end RAPTOR measurement — the first point at which the
   packed thread can *win* rather than regress.
2. **Add a `Layout` kind to `Type` in `lin-check`**: every named fixed-key record type is stamped
   once, at definition/zonk time — `Packed { stride, offsets, heap-field descriptor }` or `Boxed`.
   Packed by default; `Json`, open/width-polymorphic types, and dynamic-key map *containers* are
   Boxed forever (the honest dynamism seam). Map **values** and union **payloads** inherit the
   element type's layout (9-E restated as a typing rule). Thread the stamp through the `.lin-cache`
   typed-module + signature serialization (cache-format bump).
3. **Shadow mode:** the repr pass cross-checks its own inference against the stamp; divergences are
   reported, not acted on. Pure debt-finder, mirroring Leg 1's verifier rollout.
4. **Make the stamp authoritative:** repr pass becomes verifier-only; codegen consumes
   stride/offsets/descriptors from the type; the repr-adaptive arms are deleted one consumer at a
   time (Index → FieldGet → map store → union box → worker transfer), each step gated on suite +
   ASan + RAPTOR digest. Producer/consumer disagreement becomes a compile-time ICE.
5. **Flip the default** behind `LIN_PACKED_DEFAULT=0` for one release; measure interp + RAPTOR +
   `benchmarks/compare`; remove the flag; delete `sealed_array_to_tagged` from every non-`Json`
   path.
6. **Width subtyping** (passing `{x,y,z}` where `{x,...}` is expected) is solved by **row-shape
   monomorphization**: extend the existing generics monomorphizer to specialize per concrete caller
   shape, deduped by layout (shapes with identical offsets-for-used-fields share a specialization).
   The literature's first choice for statically-shaped code
   ([osa1's survey](https://osa1.net/posts/2023-01-23-fast-polymorphic-record-access.html); MLton
   precedent). *(Also appears as Path 11's Leg 3e — build once if both adventures run.)*

**Allowed strictness tweak:** bare `Json` no longer silently flows where a Packed record is
expected — the seam becomes an explicit one-time decode (the ADR-031 loader machinery, already
built), after which the program is packed end-to-end. Mostly ratifies spec §5.1.1.

---

## Leg 3 — The 8-byte tagged value + borrowed reads (the dynamic remainder)

### Gate first
**Re-profile after Leg 2 lands** (packed default ON): count residual box ops / dynamic reads on
RAPTOR + interp. **If the remaining seam traffic is <10% of runtime, the adventure ends at Leg 2** —
declare victory and stop; this leg's migration doesn't pay. The borrowed-reads half ships
regardless (it serves packed heap-field reads too).

### The move (if the gate says go)
1. **Encoding:** replace the 16-byte heap-boxed `TaggedVal` (`tagged.rs:40`) with an 8-byte
   immediate tagged value — low-bit tagging (ints/pointers dominate Lin's profile) or NaN-boxing;
   decide by an A/B spike with scalars only, behind a compile-time switch. Every production JS
   engine converged on 8 bytes; width dominates tag scheme
   ([survey](https://coredumped.dev/2024/09/09/what-is-the-best-pointer-tagging-method/)); float
   self-tagging measured 2.3× on float-heavy code ([POPL'25](https://arxiv.org/pdf/2411.16544)) and
   is the follow-up if a float workload ever appears.
2. **What it deletes:** scalar box allocation and the small-int caches (the immediate *is* the
   value), box-shell RC and its whole historical leak class, and half the memory traffic on every
   tagged array element, map value, and union payload slot (16 → 8 bytes).
3. **Borrowed reads** (this is where Leg 1 pays): `lin_object_get`/map-get/array-get-tagged return
   `borrow`; lowering inserts retain-on-escape; the Leg-1 verifier proves the borrow never outlives
   its source. The projection clone moves from *every* read to *escaping* reads — the minority on
   RAPTOR's inspect-and-compare loops.
4. **Same-sweep runtime polish:** small-string optimization (every concat is a malloc today —
   `string.rs:6` has no SSO) and universal interned-key comparison. Re-audit `deep_clone` transfer
   and `Shared<T>` under the new encoding (tag bits must survive the transfer walk).

The blast radius (every `lin_*` intrinsic signature) is the argument for doing this *now*, while
the runtime ABI is still freely changeable — and for the façade pattern (`RuntimeFns`/`BuilderExt`)
keeping it one mechanical sweep.

---

## Risks (whole-adventure)
- **Leg 1's hand-audited intrinsic table is load-bearing** — a wrong declaration is a miscompile.
  Shadow mode cross-checks it against the corpus before anything changes.
- **Leg 2 is a long migration** — mitigated by stamp-then-shadow-then-consume staging; the
  cache-format bump must not be forgotten (stale caches would deserialize un-stamped types).
- **Leg 3 may be cancelled by Leg 2's success.** That is the gate working, not a failure; the
  adventure's finish line is the RAPTOR number, not leg count.
- Pre-existing siblings to fix in passing: `sealed_array_rebuild_from_boxed` large-array bug, the
  packed-record recursive heap-field drop (task #12), the `Trip|Null` union-materialization leak.

## Shared/duplicated work with Path 11
If both adventures run: Leg 1 (ownership conventions) is identical — build once. Row-shape
monomorphization (Leg 2 step 6 = Path 11 Leg 3e) is identical — build once. Both adventures bump
the `.lin-cache` format — coordinate as one bump. Everything else is disjoint (different crates'
hot paths, different benchmark gates).

## What stays dead on this route (measured, do not revisit)
Tracing GC ([Path 7](path-7-tracing-gc-foundation.md)), arenas/regions
([Path 3](path-3-arena-allocation-construction-cost.md)/[4](path-4-whole-program-region-inference.md)),
box pooling, inline caches ([Path 2](path-2-fast-dynamic-inline-caches.md) — built, 99.56% hit
rate, still a wash), value-semantics records ([Path 5](path-5-value-records.md) — breaking, premise
falsified), incremental/partial RAPTOR typing.
