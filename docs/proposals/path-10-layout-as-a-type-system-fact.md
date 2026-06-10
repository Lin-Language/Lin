# Path 10 — Layout as a type-system fact: packed records become THE representation

**Status:** Open proposal. **The architectural completion of [Path 1](path-1-integrate-packed-records.md)
and [Path 9](path-9-end-to-end-packed-records.md), not a competing direction.** Path 9 proved the
mechanism (const-offset reads work, nested `Trip.stopTimes` packs soundly, digest-exact) and precisely
located the remaining blockers; this path says the *architecture* that produced two months of repr bugs
must change before the next layer of packing lands on top of it. **No userland language change** — one
allowed strictness tweak at the `Json`↔record seam (explicit decode, §4).

**Direction in one line:** stop re-deriving the packed-vs-boxed decision in a codegen-side oracle that
drifts out of sync with the checker at every seam — make layout a **kind carried on `Type` in
`lin-check`**, decided once at type-definition time, consumed (never re-inferred) by lowering and
codegen, and flip the default so every fixed-key named record is packed unless it flows into `Json`.

---

## 1. Why the architecture, not just the next gate widen

The Path-1/9 campaign keeps paying the same tax. Every step has shipped its mechanism and then spent the
majority of its calendar time on **representation-agreement bugs between producer and consumer**:

- the producer/consumer seal asymmetry (9C): producer inferred `sealed: false` boxed while the consumer
  read packed → **live silent data corruption** (`'7 0'` garbage vs `'33 44'` correct, verified);
- the repr-oracle over-assertion (`d341824d`): the `Index` arm asserted sealed-typed ⇒ Packed while
  codegen was legitimately repr-adaptive → multi-day §H4/H5 "deep union conflict" that turned out to be
  a stale assertion;
- the triple-replication bug class that ADR-062 was created to stop (mismatched `Type`-predicate packers
  in three places);
- the map-value seam (9-D): `{ String: Trip[] }` reads materialize the whole packed array per access —
  25 GB RSS, benchmark didn't complete — because the map container and the array element disagreed about
  representation;
- the recurring packed/boxed-mismatch UAF class named as a key risk in *both* Path 1 and Path 9.

These are not five bugs; they are one bug five times: **the layout decision lives downstream of the
checker, in `lin-ir/src/repr.rs`'s per-function dataflow oracle, which re-derives what the checker
already knew and goes out of sync at every boundary** (unions, maps, module edges, TCO param slots,
worker transfer). ADR-062's "single-owner principle: repr decided in ONE place" was the right instinct
applied one stage too late — the one place should be the type system, not an IR pass.

### External validation
This is exactly the conclusion Jane Street reached for OCaml: **OxCaml's unboxed types make layout a
*kind* tracked in the type system** (`value`, `float64`, `bits64`, unboxed products), precisely so the
compiler never has two opinions about a value's representation
([OxCaml unboxed types](https://oxcaml.org/documentation/unboxed-types/intro/),
[ICFP/tech talk](https://www.janestreet.com/tech-talks/unboxed-types-for-ocaml/)). Roc goes further and
demonstrates the *default*: records are flat structs, monomorphized, with **no uniform boxed
representation at all** where the type is known — and reaches near-C++ benchmark territory with an
RC+LLVM pipeline architecturally very close to Lin's
([roc-lang.org/functional](https://www.roc-lang.org/functional)). Cinder's Static Python is the same
move for a dynamic language: annotated classes get fixed slot layouts and field access compiles to a
constant-offset load — "three machine instructions"
([Static Python](https://github.com/facebookincubator/cinder/blob/cinder/3.8/CinderDoc/static_python.rst)).
The literature menu for structural/row-polymorphic access
([osa1, "Fast polymorphic record access"](https://osa1.net/posts/2023-01-23-fast-polymorphic-record-access.html))
ranks monomorphization-to-constant-offset first for statically-shaped code — which Lin's programs are.

## 2. The measured target (unchanged from Path 9 — restated for self-containment)

- RAPTOR query phase: **631 M of 756 M `lin_object_get` are linear scans** over <16-key heap objects,
  plus ~3.5 B box ops. 100% of residual `object_get` is record-shaped (Phase-0 class profile: 20%
  RECORD, 80% OPAQUE records-in-disguise; 0% genuine dictionary).
- **Partial typing regresses ~13%** (and the 9-D map seam regresses catastrophically): mixed
  representation is strictly worse than either pure one. The fix is all-or-nothing by construction.
- interp shares the frontier: its 4.93 M `OBJECT_GET` are 100% RECORD-class (boxed `Token[]`,
  `Cursor.node`). Path 10 is the dominant **shared** lever, not RAPTOR-only.

## 3. Mechanism

### 3a. A `Layout` kind on `Type` (the checker owns it)
Add a layout fact to `lin-check`'s type representation: every named fixed-key record type is classified
**once**, at definition/zonk time, as `Packed { stride, field_offsets, heap_field_descriptor }` or
`Boxed`, and that classification is part of the type carried on every `TypedExpr`. Rules:

- Named fixed-key records (and sealed arrays / sum types of them): **Packed by default.** The current
  `is_sealed_array_field_packable` gate (scalar+Bool+String+nested-record-array after the 9-A/9C work)
  becomes the *initial* packed set; fields that can't yet pack make the whole type Boxed — but that is a
  property of the **type**, stamped once, not of each use site.
- `Json`, open/width-polymorphic record types, and dynamic-key maps' *containers*: Boxed, forever — the
  honest dynamism seam.
- Map **values** and union **payloads** inherit the element type's layout (this is 9-E's
  `BoxKeepPacked`-for-map-values, but stated as a typing rule instead of a codegen patch).

### 3b. Lowering and codegen become layout *consumers*
`lin-ir`'s repr pass stops being an oracle and becomes a **verifier**: it checks that every producer and
consumer of a temp agree with the checker's stamped layout, and a disagreement is a compile-time ICE —
the seal-asymmetry corruption class becomes unrepresentable instead of ASan-hunted. The repr-adaptive
codegen paths (e.g. `compile_ir_index`) keep working during migration; the end state deletes the
adaptive paths.

### 3c. Flip the default, delete the second path
Once 9-E (map values) and the TCO param-slot fix land (both pinned, see
[[project_path9e_mapvalue_and_tco_leak]]), packed is no longer an opt-in fast path guarded by a gate —
it is the representation of every record the checker stamped Packed, end-to-end: literal → array → map
value → union payload → match arm → field read. Boxing happens **only** where the stamped layout is
Boxed, i.e. at genuine `Json` seams. This is Roc's posture, and it is what eliminates the
partial-typing regression by construction: there is no mixed seam left to regress across.

## 4. The one behavioural tweak (allowed strictness)
Bare `Json` no longer silently flows where a Packed named record is expected. The seam becomes an
**explicit one-time decode** at the boundary (the ADR-031 loader machinery — `Json → Trip[]` decode —
already exists and Path 9 prerequisite 4 already planned to wire it). After the decode, the program is
packed end-to-end. This is added strictness with better errors, not new syntax; spec §5.1.1 already
rejects composite-`Json`-into-named-record flows in most positions, so this mostly *ratifies* current
behaviour and closes the remaining implicit holes.

## 5. Staged plan

1. **Land the two pinned blockers on the current architecture** (they are root-caused and scoped):
   9-E map-value keep-packed (`perf/path9e-map-value-keeppacked` — repr STEP-4 propagation through a
   map-field-record array literal) and the TCO param-slot sealed-record drop
   (`codegen/types.rs:131` carve-out). Re-run the 9-D end-to-end RAPTOR measurement — this is the
   first point at which the packed thread can *win* rather than regress.
2. **Introduce the `Layout` kind** in `lin-check` (additive: stamp types, thread through `TypedModule`
   serialization and the module-signature cache), with the repr pass cross-checking its own inference
   against the stamp and reporting divergences (shadow mode — zero behaviour change, pure debt-finder).
3. **Make the stamp authoritative**: repr pass becomes verifier-only; codegen consumes
   stride/offsets/descriptors from the type. Delete the oracle arms one consumer at a time
   (Index → FieldGet → map store → union box → worker transfer), each step gated on the full suite +
   ASan + RAPTOR digest.
4. **Flip the default** (packed unless `Json`-touching) behind an env flag first
   (`LIN_PACKED_DEFAULT=0` escape hatch for one release), measure interp + RAPTOR + all
   `benchmarks/compare` workloads, then remove the flag.
5. **Delete `sealed_array_to_tagged` materialization** from every non-`Json` path; what remains is the
   decode/encode pair at the explicit seam.

## 6. Risks
- **Migration breadth:** the stamp must thread through the `.lin-cache` typed-module and signature
  serialization — a cache-format bump, cheap but easy to forget (stale caches would deserialize
  un-stamped types).
- **Width subtyping:** passing `{x, y, z}` where `{x, ...}` is expected needs either per-shape
  monomorphization (Lin already monomorphizes generics — extend to row shapes; this is also
  [Path 14](path-14-whole-program-spine.md)'s lever) or a projection-copy at the call edge (the spec's
  existing §5.9.1 narrowing copy). Choose monomorphization; the projection-copy is the
  partial-typing-regression shape again.
- **It is a long migration** — mitigated by stage 2's shadow mode, which finds every divergence before
  anything changes behaviour.
- The pre-existing `sealed_array_rebuild_from_boxed` large-array bug and the packed-record recursive
  heap-field drop (task #12 / [[project_path9_raptor_payoff_measured]]) must be fixed in stage 1; they
  are independent of the architecture change.

## 7. Relationship to other paths
- **Subsumes** the remaining Path 1 Step-3 / Path 9 work as its stage 1, then removes the bug class that
  made those steps slow to land.
- **[Path 12](path-12-eight-byte-tagged-value.md)** (8-byte dynamic value) is sequenced *after* this:
  no point re-engineering seams this path deletes.
- **[Path 13](path-13-ownership-parameter-conventions.md)** (borrow/own conventions) composes: layout
  says *what shape* a value is, conventions say *who owns it* — together they make the UAF class
  unrepresentable.
- **[Path 14](path-14-whole-program-spine.md)** multiplies this: once reads are `getelementptr + load`,
  LTO can finally hoist/fold them.
