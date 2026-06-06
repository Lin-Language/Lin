# Representation-Inference Pass — Design Blueprint

> Status: IN IMPLEMENTATION (5 stages). Centralizes the packed-vs-boxed representation decision for sealed
> records/arrays into ONE lin-ir analysis pass, replacing three replicated type-driven predicates. Makes a
> representation mismatch impossible to express silently. Decisions taken: (A) commit to keep-packed
> cross-module ABI (compiled Lin fns return raw 0xFE LinArray*/packed struct*; FFI/opaque stays boxed).
> (B) repr::verify runs in DEBUG/TESTS only (no release env-var). To be folded into an ADR + spec once landed,
> then this doc deleted.

## Lattice

REPRESENTATION (Repr) is a per-temp 3-point lattice with a sealed-array wrapping refinement, NOT a per-Type attribute:

  Repr ::= Unknown (TOP)
         | Packed(Layout)        // value IS the physical packed thing
         | Boxed(Inner)          // value is a TaggedVal* / LinObject* slot wrapping Inner
         | FlatScalar(ScalarTy)  // unboxed i32/i64/f64/i1 (the is_flat_scalar primitive)
         | Bottom (no value)

  Layout ::= PackedStruct{fields: layout-key}        // sealed scalar record, packed struct (sealed.rs)
           | PackedSealedArray{elem_layout, on_heap}  // LinArray elem_tag=0xFE
  Inner  ::= Opaque                                   // generic boxed (LinObject / Object[])
           | WrapsPacked(Layout)                       // KEY refinement: a boxed slot (TaggedVal TAG_ARRAY / TAG_OBJECT)
                                                        // whose payload pointer is a STILL-PACKED LinArray*/struct*.
                                                        // This is the representation the static Type CANNOT express
                                                        // and is the whole reason Repr is not on Type.

The lattice is shallow per-temp; nested element repr is carried in Layout.elem_layout (one level — sufficient because a LinArray is self-describing via elem_tag, so deeper nesting is keep-by-pointer recursively and never needs codegen to know the depth statically).

MEET/JOIN (over a carry class, identical machinery to escape.rs union-find):
  - join(Packed(L), Packed(L)) = Packed(L)              // same layout: stays packed (an ISLAND)
  - join(Packed(L), Packed(L')) where L!=L' = Boxed(Opaque)  // layout disagreement: demote (can't happen for SSA carry of one value, only at Phi of distinct shapes)
  - join(Packed(L), Boxed(_))  = CONFLICT -> this class is a BOUNDARY: it must be SPLIT by inserting a Coerce; never silently joined
  - join(FlatScalar(s), FlatScalar(s)) = FlatScalar(s)
  - join(Unknown, x) = x ; join(x, Bottom)=x
  - join(Boxed(WrapsPacked(L)), Boxed(WrapsPacked(L))) = Boxed(WrapsPacked(L))   // packed-by-pointer through a box stays
  - join(Boxed(WrapsPacked(L)), Boxed(Opaque)) = Boxed(Opaque)                   // if any consumer demands a materialized box, fall to opaque (then a materialize coerce is forced)

FAIL-SAFE: the top of the lattice for any temp the analysis cannot prove is Boxed(Opaque) (mirrors escape.rs/types.rs fail-safe-to-boxed). A Packed label is only ever assigned by PROOF from a definite packed producer carried along representation-preserving edges; on any doubt the temp is Boxed.
## Where it lives (and why NOT on Type)

THE PASS: a new module crates/lin-ir/src/repr.rs, function `pub fn assign(module: &mut LinModule)`, run PER FUNCTION over the flat LinFunction (same shape as escape::analyze). It reuses a factored-out `crates/lin-ir/src/carry.rs` (UnionFind + the carry-edge classifier extracted from escape.rs:120-152,301-330) and `liveness::instr_use_def` (liveness.rs:204) for use/def enumeration.

WHAT IT ANNOTATES: it produces a side table `repr: Vec<Repr>` indexed by `Temp.0` (exactly mirroring escape.rs's `escaping: Vec<bool>`), stored on the LinFunction as a new field `pub repr: Vec<Repr>` (ir.rs LinFunction near temp_types:528). In ADDITION it REWRITES the instruction list in place to make boundaries explicit: it INSERTS `Coerce`/new `BoxKeepPacked`/`UnboxKeepPacked` instructions at every conflict edge and CANONICALIZES the existing repr-deciding instructions to carry an explicit `repr` field (MakeObject already has `stack`; MakeArray/Index/IndexSet/FieldGet/Push get a resolved `repr` tag so codegen never re-derives). The instruction-list edit uses rc_elide.rs's reverse-index discipline (rc_elide.rs:185-196) and PostDom (rc_elide.rs:466) when a coercion must hoist to a merge.

HOW CODEGEN CONSUMES IT: codegen stops calling sealed_fields/sealed_array_elem/is_flat_scalar on Type. At each DECIDE site (MakeObject mod.rs:1191, MakeArray mod.rs:1399, Push intrinsics.rs:243, emit_map_set data.rs:500) it reads the instruction's resolved `repr` field. At each ASSUME site (FieldGet data.rs:1531, SealedArrayFieldGet data.rs:905, Index data.rs:240, eq, release) it reads `func.repr[operand.0]` to know the actual runtime representation and emits the matching load/store/free; if that repr is Boxed(WrapsPacked(L)) it does the tag-checked pointer-unbox-then-packed-read (zero copy); if Boxed(Opaque) it does the dynamic path. The pass-inserted Coerce/Box/Unbox instructions lower via the existing match.rs:compile_ir_coerce machinery (which becomes the SINGLE consumer of the bridge helpers).

WHY NOT ON TYPE (the load-bearing rationale):
  1. The same static Type (e.g. `Neighbor[]`) is PACKED in one temp (just constructed) and BOXED-WRAPPING-PACKED in another (read from a Map slot, map.rs:36 always TaggedVal). Type is by definition the SAME for both; representation differs. Putting repr on Type would force Type to encode container provenance — it cannot, and shouldn't (Type is the semantic contract, ADR direction).
  2. `Type::PartialEq` deliberately IGNORES the sealed flag (data.rs:1365 sealed_repr_differs exists precisely to work around this); making repr part of Type would either break type equality used by the checker or require a parallel comparator anyway — i.e. you'd reinvent the side table on Type.
  3. The keep-packed-by-pointer state `Boxed(WrapsPacked(L))` is UNSPEAKABLE in the surface/semantic type system: there is no Lin type for "a boxed slot whose payload is a packed buffer". It is a purely physical fact discovered by dataflow.
  4. Representation is FLOW-SENSITIVE and per-occurrence (a value's repr changes as it crosses a coerce); Type is flow-insensitive. A side table indexed by Temp is the natural carrier.
  5. It must be recomputed per monomorphic specialization (subst_type:141 mints packed-typed bodies); a per-temp table on the concrete LinFunction is exactly per-specialization, whereas a Type attribute would have to be re-stamped anyway.
## The analysis

SINGLE-PASS union-find with a seed-and-resolve finish (no iterative fixpoint needed — representation is a property of the final carry class, exactly as escape.rs computes escape in one fold). RUN ORDER: monomorphize (in lower) -> lower -> REPR PASS (new) -> rc_elide -> escape (lib.rs:264-268; new pass slots immediately before rc_elide so RC sees representation-stable IR and can optimize the pass-inserted Retain/Release).

ALGORITHM per LinFunction:
  STEP 1 — build carry classes. UnionFind over 0..temp_count. Unify along representation-PRESERVING edges (reuse carry.rs, identical to escape.rs:303-311): Copy, Bind, Phi incomings, and a Coerce ONLY when coerce_is_carry (same layout both sides). A repr-CHANGING Coerce does NOT unify (breaks the chain). TailCall arg i unifies with param i (escape.rs:212).

  STEP 2 — seed reprs at DEFINITE producers (a temp's local repr before class-folding):
    PACKED seeds: MakeObject whose ty is sealed-all-scalar-eligible (the layout decision MOVES here, computed once from Type by the pass — the LAST place a Type predicate runs); MakeArray whose elem is sealed-packable (Packed(PackedSealedArray)); SealedArrayFieldGet result that itself yields a packed sub-thing (rare); a Const-folded packed literal.
    FLATSCALAR seeds: numeric/bool defs.
    BOXED seeds (the dynamic boundaries the type predicates cannot see — the crux):
      - Index/Call result reading a Map VALUE slot (map.rs:36 TaggedVal) -> Boxed(WrapsPacked(L)) if the result Type is Array(sealed)/sealed-record (KEEP-PACKED), else Boxed(Opaque).
      - Json/object field read (lin_object_get) -> Boxed(Opaque) (object slots are heterogeneous TaggedVal; could refine to WrapsPacked if the field's declared type is packed AND we control both sides — Stage 5 refinement).
      - Cross-module / non-self Call return -> Boxed(WrapsPacked(L)) when ret_ty is packed AND the callee is a Lin function compiled by us (we control the return ABI to return the raw 0xFE LinArray*); Boxed(Opaque) for FFI/opaque.
      - Closure capture read (EnvCapture, ir.rs:384) -> Boxed(WrapsPacked) or Opaque per capture descriptor.
      - Param of a function: Boxed if the param crosses a boxed ABI (generic-T, union, Json), Packed if the .sig says packed-by-pointer.

  STEP 3 — fold per-temp seeds into per-class repr via the lattice join (one HashMap<root,Repr> pass, like escape.rs:224-230). 

  STEP 4 — detect CONFLICTS and resolve. For every instruction edge producer->consumer where the producer's class repr and the consumer's REQUIRED repr (what that opcode physically reads) differ, the class join would be CONFLICT. Resolve by SPLITTING: insert an explicit coercion temp between producer and consumer (a fresh temp gets the consumer's required repr; the edge is no longer a carry edge). Two resolution kinds:
      (a) MATERIALIZE coercion: Packed -> Boxed(Opaque) (sealed_materialize_to_object / sealed_array_to_tagged) — a fresh +1 owned value; pass also schedules the matching Release (owned-reference symmetry, lower.rs:1196-1199).
      (b) KEEP-PACKED coercion: Packed -> Boxed(WrapsPacked(L)) is a 16-byte tag/ptr wrap (BoxKeepPacked, O(1), no copy, BORROWS — no extra release of the inner), and the symmetric Boxed(WrapsPacked(L)) -> Packed on read is an UnboxKeepPacked (tag-checked ptr load + retain). This is what the map hot loop selects.
  Choice (a) vs (b) at a container-store boundary: pick KEEP-PACKED whenever (i) the slot is a Map value or a we-control container, AND (ii) the value will be read back as packed somewhere (or universally — keep-packed is always sound because the runtime dispatches on elem_tag, so default to (b) and only fall to (a) for genuinely-dynamic consumers: toString/keys/spread/dynamic obj[k]/cross-FFI).

  DEFINITELY-PACKED PROOF: a temp is Packed in codegen iff its final class repr is Packed(L) — which holds ONLY if every seed in the class is Packed(L) (same layout) and NO boxed seed joined in. Because any boxed-seed join is a CONFLICT that STEP 4 splits, after STEP 4 a Packed class is closed under representation-preserving aliasing and provably carries a real packed value at every member. That is the proof that an ASSUME site reading func.repr[t]==Packed is safe.
## Coercion insertion

Coercions are inserted by STEP 4 of the pass at every conflict edge, materialized as IR instructions so codegen never guesses. Two NEW IR instructions (ir.rs) plus reuse of existing Coerce:

  1. Reuse `Coerce { dst, src, from_ty, to_ty }` for MATERIALIZE (Packed->Boxed(Opaque)) and PROJECT (Boxed(Opaque)->Packed). These already lower in match.rs:compile_ir_coerce (sealed_materialize_to_object/sealed_project_from/sealed_array_to_tagged/sealed_array_project_from). The pass becomes their ONLY emitter; all the lower.rs:1729 lower_coerce_arg ad-hoc triggers and the codegen box_value/unbox_value sealed arms are deleted.

  2. NEW `BoxKeepPacked { dst, src, layout }`: wrap a packed LinArray*/struct* pointer into a TaggedVal (TAG_ARRAY/TAG_OBJECT) WITHOUT materializing — O(1), 16 bytes, borrows the inner (no deep copy, no per-element retain; one shell +1 governed by transfer_into_container). Lowers to the existing box_array-by-pointer path (boxing.rs:128, which ALREADY does exactly this for plain arrays) generalized to the 0xFE kind. dst repr = Boxed(WrapsPacked(layout)).

  3. NEW `UnboxKeepPacked { dst, src, layout }`: tag-check the TaggedVal, load the payload LinArray*/struct* as the still-packed pointer, retain it. O(1). dst repr = Packed(layout). Lowers to unbox_ptr (boxing.rs:376) but now JUSTIFIED by the pass (the silent assumption becomes a proven one).

PLACEMENT: insertion point is the producer block immediately after the def, or hoisted to the nearest common post-dominator (rc_elide.rs:466 PostDom) when the same value reaches multiple differently-repr'd consumers (insert one coercion at the merge rather than per consumer). The edit rewrites block.instructions with reverse-index removal/splice discipline (rc_elide.rs:185-196). New temps are appended (temp_count grows); their entry in repr[] and temp_types[] is filled at insertion so liveness/rc_elide/escape (which run AFTER) see them.

WHERE EACH BOUNDARY GETS WHICH: see boundaryCatalogue. Critically emit_map_set's store of a packed Array(sealed) becomes a BoxKeepPacked (not box_value->sealed_array_to_tagged), and the map read-back becomes UnboxKeepPacked feeding a Packed reader — the dijkstra fix.
## Boundary catalogue

- Map VALUE store of packed Array(sealed) (data.rs:emit_map_set:500): KEEP-PACKED -> BoxKeepPacked(TAG_ARRAY over 0xFE LinArray*); NO sealed_array_to_tagged, O(1). THE dijkstra fix.
- Map VALUE store of packed sealed RECORD: KEEP-PACKED -> BoxKeepPacked(TAG_OBJECT over packed struct*) (runtime release dispatches on header tag).
- Map VALUE store of flat scalar (data.rs:504): keep existing unboxed-in-TaggedVal path; repr FlatScalar — no change.
- Map VALUE read-back (Index/Call Map arm, data.rs:312): UnboxKeepPacked -> Packed; feeds packed FieldGet/SealedArrayFieldGet zero-copy. CRASH SITE today, now sound.
- Json/Object field store of a sealed value (emit_object_set data.rs:468): KEEP-PACKED by default (BoxKeepPacked); COERCE-materialize only if a dynamic consumer (toString/keys/spread) reads that object.
- Json/Object field read to a packed type: UnboxKeepPacked if stored keep-packed; else PROJECT coercion.
- Nested array element Pt[][] / Map-of-array outer (lower.rs nested_sealed_repr_change:1231, codegen array_coerce_elementwise:1002): outer is a plain Object[]/0xFF whose elements are Boxed(WrapsPacked) by-pointer; DELETE element-wise rebuild — inner LinArray* rides by pointer, recursion handled by elem_tag self-description.
- Tagged-array store of a sealed element (tagged_array_push_value:168, MakeArray tagged fallback mod.rs:1452): if the array is genuinely heterogeneous/dynamic -> MATERIALIZE coercion (correct, it is a boxed island); if homogeneous packed -> the array IS the 0xFE buffer, no boundary.
- Sealed-array IndexSet RHS (compile_ir_index_set:678 sealed_repr_differs): pass tells the site the RHS repr; verbatim packed-struct store iff RHS repr==Packed(same layout), else PROJECT coercion. sealed_repr_differs DELETED.
- Call argument into a Boxed/TypeVar/union/Json param (lower_coerce_arg:1729): KEEP-PACKED (BoxKeepPacked) when callee is our Lin fn accepting by-pointer; MATERIALIZE for FFI/opaque/dynamic param. Named pass-through stays a carry edge.
- Call return / cross-module .sig (ir.rs Call ret_ty): we define the ABI to return raw 0xFE LinArray*/packed struct*; caller seeds Boxed(WrapsPacked) and UnboxKeepPacked at use. FFI returns -> Boxed(Opaque).
- Closure capture (MakeClosure/EnvCapture ir.rs:384,348): capture-slot repr recorded by the pass; a captured packed value is stored by-pointer in the env (BoxKeepPacked) unless the closure escapes to a thread (then transfer.rs clone handles it by elem_tag — still keep-packed).
- Thread transfer / async deep-copy (transfer.rs:93 clone_array elem_tag dispatch): NO coercion needed — runtime already keeps repr by self-description; pass only guarantees the value entering transfer has the repr its tag claims (the core invariant).
- Equality (emit_eq arith.rs:355, array-eq:130): mark BOTH operands Boxed-consumers and insert ONE MATERIALIZE coercion per side (acceptable; eq is not hot for records) OR, perf-refinement, keep packed and call a packed field-wise sealed_eq when both reprs are Packed(same L). Default: packed-eq when both Packed, materialize-both otherwise.
- Union member (T|Null, sealed in a union; lower_coerce_arg:1798, unbox boxing.rs:365): union membership FORCES Boxed; the box/project is the pass's single inserted coercion (the three predicate triggers deleted).
- toString / keys / values / entries / spread / dynamic obj[k] on a sealed value (arith box-both, map.rs:339-375, data.rs:381): mark Boxed(Opaque)-consumer; insert ONE MATERIALIZE coercion — these are inherently dynamic, correct to box.
- Generic combinator over packed-T (monomorphize boxed_fallback_call:1745, lin_filter bail:1550): the pass runs per-specialization; classify the combinator body's element accesses. If body reads elements as packed (native-specialized) -> Packed island, no boundary. If body uses boxed Object[] machinery -> insert ONE BoxKeepPacked at entry + UnboxKeepPacked at exit (keep-packed through the combinator) instead of materialize-and-re-seal. combinator_unsound_over_sealed allowlist DELETED.
- Return of a packed value from a function: Packed flows out by-pointer (we own the ABI); no coercion unless the declared return type is Json/union -> MATERIALIZE.
- emit_release dispatch (rc.rs:10): release shape chosen by func.repr[val] not Type — Packed(PackedSealedArray)->array_release(0xFE-aware), Packed(PackedStruct)->emit_sealed_release, Boxed(_)->tagged_release. Eliminates the wrong-release-after-divergence bug.
## Predicates / patches to DELETE

- crates/lin-codegen/src/codegen/types.rs:sealed_array_elem_field_packable (the scalar-only gate kept artificially narrow to dodge the container round-trip bug — its whole reason to exist disappears)
- crates/lin-codegen/src/codegen/data.rs:sealed_repr_differs (hand-rolled representation comparator working around Type::PartialEq ignoring sealed; replaced by func.repr comparison)
- crates/lin-codegen/src/codegen/data.rs:ty_contains_sealed (recursive coerce heuristic; replaced by per-temp repr + elem_layout)
- crates/lin-codegen/src/codegen/boxing.rs:box_value sealed-array materialize arm (boxing.rs:123) — replaced by BoxKeepPacked at container stores
- crates/lin-codegen/src/codegen/boxing.rs:box_value sealed-record materialize arm (boxing.rs:101) for the container-store case (kept only as the genuinely-dynamic Json/FFI fallback driven by the pass)
- crates/lin-ir/src/lower.rs:is_sealed_scalar_array (mirror of sealed_array_elem)
- crates/lin-ir/src/lower.rs:is_sealed_array_elem_field_packable (mirror)
- crates/lin-ir/src/lower.rs:param_elem_is_boxed_repr
- crates/lin-ir/src/lower.rs:sealed_array_arg_materialized
- crates/lin-ir/src/lower.rs:to_contains_sealed_array
- crates/lin-ir/src/lower.rs:nested_sealed_repr_change
- crates/lin-ir/src/lower.rs:type_repr_differs (THE central boundary oracle — fully subsumed by producer-repr!=consumer-repr off the analysis)
- crates/lin-ir/src/lower.rs:lower_coerce_arg ad-hoc Coerce-trigger arms (1784 sealed-array materialize, 1814 type_repr_differs projection) — coercion insertion moves to the pass
- crates/lin-ir/src/monomorphize.rs:field_packed_scalar (3rd mirror)
- crates/lin-ir/src/monomorphize.rs:mentions_sealed (routing gate)
- crates/lin-ir/src/monomorphize.rs:combinator_unsound_over_sealed (name allowlist — replaced by per-specialization classification)
- crates/lin-ir/src/lower.rs:push_into_sealed_array / push_sealed_elem_into_tagged flags (lower.rs:3680/3690) — ownership decided by repr
- NOTE: the BRIDGE helpers (sealed_array_to_tagged, sealed_array_project_from, sealed_materialize_to_object, sealed_project_from, sealed_construct, sealed_eq, emit_sealed_release) are KEPT but become the lowered form of pass-inserted Coerce/Box/Unbox nodes, no longer called from type-guessing arms
## Perf model

THE 87x ISLANDS ARE PRESERVED BY CONSTRUCTION: within a static island (a carry class with NO boxed-seed and NO conflict edge), the pass labels every temp Packed(L) and codegen emits the identical constant-offset typed loads / contiguous-buffer pushes / packed sealed_eq it does today. The pass only acts AT boundaries; an island that has none is byte-for-byte the current packed codegen. So loop kernels that construct and consume sealed records/arrays without crossing a Map/Json/closure boundary keep the 87x exactly.

THE MAP-OF-RECORD-ARRAY HOT LOOP (dijkstra {String: Neighbor[]}): KEEP-PACKED-BY-POINTER, not materialize-per-access. Store = BoxKeepPacked = one 16-byte TaggedVal write holding the existing 0xFE LinArray* (O(1), no copy, no per-element retain). Read-back = UnboxKeepPacked = tag-check + pointer load + one shell retain (O(1)), yielding a Packed temp fed directly to SealedArrayFieldGet (the fused constant-offset scalar load, ir.rs:381). The inner array NEVER materializes; the per-iteration cost is a pointer load, not an O(n) sealed_array_to_tagged copy. This is strictly FASTER than today (which forces the O(n) materialize at box_value:123 and then crashes on read-back) and matches the runtime's existing zero-copy transfer model (transfer.rs:105-108).

GRACEFUL FALLBACK: genuinely-dynamic consumers (toString, keys, spread, dynamic obj[k], FFI, heterogeneous arrays) are Boxed(Opaque) islands; the pass inserts ONE materialize coercion at the single boundary into them — same cost as today, paid once, not per access. Field-omission records and union-in-element stay boxed (language-level, correct).

COST OF THE PASS ITSELF: O(instructions) union-find with path compression (escape.rs cost profile), run once per function per specialization — negligible vs LLVM passes. Inserted coercions add temps that rc_elide/escape then optimize (they run after), so no spurious RC survives the keep-packed path.
## Soundness gates

PROVING NO SILENT MISMATCH REMAINS:
  1. STRUCTURAL: after STEP 4, the IR has the property that NO instruction reads an operand whose required physical repr differs from func.repr[operand]. Add a DEBUG verifier pass `repr::verify(func)` that walks every instruction, computes the repr each opcode REQUIRES of each operand, and asserts it equals func.repr — panicking in debug builds. A silent mismatch is now a compile-time panic, not a runtime UAF. This verifier is the formal statement of 'mismatch is inexpressible'.
  2. STAGE-2 ORACLE: the debug assert that repr==old-predicate at every DECIDE site proves the new analysis is a conservative superset of today's correct decisions before any code trusts it.
  3. ASan + CORPUS: every stage runs the full stdlib/ + examples/ + benchmarks/ corpus under `-Z sanitizer=address` (the ONLY validator for RC/repr per memory + types.rs:330 — cargo test does NOT catch these). Stage 4 adds a purpose-built dijkstra {String: Neighbor[]} hot-loop fixture and Pt[][] nested fixture (the exact crash shapes).
  4. RUN-EQUIVALENCE: reuse the formatter corpus run-equivalence harness (crates/lin/tests/integration.rs) — every example's stdout must be byte-identical before/after each stage (compare against git checkout HEAD~1, never git stash per CLAUDE.md).
  5. OWNERSHIP BALANCE: a MATERIALIZE coercion emits a +1 and a matching Release; a KEEP-PACKED coercion emits NO inner release. Add an ASan leak-check assertion that the dijkstra map drop frees exactly once. The owned-reference symmetry (lower.rs:1196-1199) is verified by running the drop-heavy fixtures under ASan with detect_leaks=1.
  6. ELEM_TAG TRUST: the runtime already dispatches free/release/transfer on elem_tag (array.rs:136,191; transfer.rs:104). The gate is: a temp with repr Packed/Boxed(WrapsPacked) MUST point at a real 0xFE LinArray — proven because keep-packed coercions never materialize and packed seeds construct genuine 0xFE buffers; the verifier (1) plus ASan (3) jointly establish it.
## Staged plan

### Stage 1 (effort M, landable True)
Factor the shared carry-class machinery out of escape.rs into crates/lin-ir/src/carry.rs (UnionFind + coerce_is_carry + the Copy/Bind/Phi/TailCall edge classifier). Pure refactor: escape.rs imports it, behaviour byte-for-byte identical. Lowest risk: no semantic change, establishes the substrate the new pass shares.

- Files: crates/lin-ir/src/carry.rs, crates/lin-ir/src/escape.rs, crates/lin-ir/src/lib.rs
- ASan focus: Run full corpus under ASan; assert escape's stack-alloc set is bit-identical before/after (dump stack_class_roots) — no new UAF, no regressed stack-alloc.

### Stage 2 (effort L, landable True)
Introduce repr.rs as a PURE side-table (no IR edits): compute Vec<Repr> per function via carry.rs, seed Packed/FlatScalar/Boxed, fold by lattice join, and assert (debug-only, behind a flag) that the computed repr at every existing DECIDE site AGREES with the current type predicate (sealed_array_elem etc.). Codegen still uses the old predicates. This is an observability/oracle stage that proves the analysis matches today's decisions before anything trusts it.

- Files: crates/lin-ir/src/repr.rs, crates/lin-ir/src/ir.rs, crates/lin-ir/src/lib.rs
- ASan focus: No runtime change expected; ASan corpus must stay green. The debug assert (repr==predicate) is the gate — any mismatch is a latent bug surfaced, must be reconciled before Stage 3.

### Stage 3 (effort XL, landable True)
Switch codegen to TRUST func.repr at the SCALAR-only sites first (the cases the old predicates already got right): MakeObject/MakeArray/FieldGet/SealedArrayFieldGet/Push/IndexSet read the resolved repr instead of calling sealed_fields/sealed_array_elem/is_flat_scalar. Insert pass-driven Coerce at conflict edges that currently exist (replacing lower_coerce_arg arms). Delete the lower.rs/monomorphize mirrors for the scalar case. NO new packing capability yet — must be a behaviour-preserving swap of the decision source.

- Files: crates/lin-ir/src/repr.rs, crates/lin-ir/src/lower.rs, crates/lin-ir/src/monomorphize.rs, crates/lin-codegen/src/codegen/mod.rs, crates/lin-codegen/src/codegen/data.rs, crates/lin-codegen/src/codegen/intrinsics.rs, crates/lin-codegen/src/codegen/types.rs
- ASan focus: Full ASan on records corpus + sealed-array Stage 3 fixtures. Verify no double-free/leak from the moved coercion ownership (the +1/Release symmetry). Run-equivalence vs HEAD~1 on all examples.

### Stage 4 (effort XL, landable True)
Add BoxKeepPacked/UnboxKeepPacked IR ops + lowering (reuse boxing.rs:128 box-array-by-pointer + unbox_ptr). Wire emit_map_set to emit BoxKeepPacked for packed Array(sealed)/sealed-record map values and the Map read-back to UnboxKeepPacked. Delete box_value's sealed-array materialize arm for the container-store case. THIS is the dijkstra Map-of-record-array fix and unlocks heap-field element arrays (Stage 3b). Highest risk: new physical path through the boxed map slot.

- Files: crates/lin-ir/src/ir.rs, crates/lin-ir/src/repr.rs, crates/lin-codegen/src/codegen/match.rs, crates/lin-codegen/src/codegen/data.rs, crates/lin-codegen/src/codegen/boxing.rs, crates/lin-codegen/src/codegen/rc.rs
- ASan focus: ASan on a NEW dijkstra fixture using {String: Neighbor[]} read-back in a hot loop, plus Pt[][] nested, plus map-of-record-array store/drop cycles. This is where the 0xFE-buffer-as-packed-vs-boxed UAF lived — must be clean and must preserve the 87x (benchmark).

### Stage 5 (effort L, landable True)
Extend keep-packed to closure captures, cross-module .sig returns, and generic combinators (delete combinator_unsound_over_sealed + lin_filter bail; classify per-specialization). Broaden sealed_array_elem_field_packable to heap fields now that round-trip is sound. Final deletion of type_repr_differs and the remaining mirrors.

- Files: crates/lin-ir/src/monomorphize.rs, crates/lin-ir/src/repr.rs, crates/lin-ir/src/lower.rs, crates/lin-codegen/src/codegen/types.rs
- ASan focus: ASan over combinator-heavy corpus (map/filter/sort over sealed arrays), cross-module sealed returns, closure-captured sealed records, async transfer of sealed values.

## Risks

1. SCOPE/EFFORT: Stages 3-4 are XL touching codegen's hottest dispatch sites; a partial swap (some sites trust repr, others still call predicates) is itself a mismatch source — the Stage-2 oracle assert mitigates by proving agreement before the swap, and the verifier catches incomplete swaps.
2. CROSS-MODULE ABI: keep-packed return across a .sig boundary commits us to returning raw 0xFE LinArray* from compiled Lin functions; if any import path was relying on the materialized boxed return, that contract changes — must audit lin-compile sig handling and keep FFI returns Boxed(Opaque) (fail-safe).
3. PHI OF MIXED REPR: a Phi merging a packed and a boxed incoming is a CONFLICT that must hoist a coercion into the predecessor block (needs PostDom, rc_elide.rs:466). Getting the insertion block wrong = wrong-repr read; the verifier catches it but it is fiddly.
4. RC SYMMETRY ON KEEP-PACKED: BoxKeepPacked borrows (no inner +1); if the surrounding transfer_into_container ownership accounting double-counts, leak or double-free — ASan-only, hence the dedicated drop fixtures in Stage 4.
5. ESCAPE INTERACTION: escape.rs's stack-class is a subset of Packed; if the repr pass inserts coercions that escape touches, the two must agree on what a stack-resident value is. Sharing carry.rs reduces drift but the run-order (repr before escape) must hold.
6. cargo test GREEN BUT ASAN RED is the standing failure mode of this whole bug class — discipline is ASan on every stage, not trusting the test suite.
## Open questions

- Should Repr live as a new field on LinFunction (repr: Vec<Repr>) consumed by codegen via the func handle, OR be threaded as resolved per-instruction fields (like MakeObject.stack)? The side-table is cleaner for the analysis but codegen accesses instructions, not the func table, at many sites — likely BOTH: per-instruction resolved tag on the DECIDE ops, side-table for ASSUME-site operand lookup.
- For cross-module returns, can we always commit to returning raw 0xFE LinArray* (keep-packed) or do some existing .sig consumers require the materialized boxed form? Needs a sweep of lin-compile import handling before Stage 5.
- Object/Json field slots: refine Boxed(Opaque)->Boxed(WrapsPacked) when we control both store and read sites (would extend keep-packed to record-in-record), or leave object fields materialized (simpler, slightly slower)? Stage 5 decision.
- Does keep-packed through a closure env interact badly with async deep-copy (transfer_clone_env)? transfer.rs dispatches on elem_tag so it should be fine, but the env capture descriptor must record WrapsPacked vs Opaque — confirm the descriptor has room.
- Phi-merge coercion hoisting: is PostDom sufficient or do we need full SSA dominance to place the coercion correctly when the two incomings have different reprs and one path also uses the value packed?
- Should the verifier (repr::verify) run in release builds behind LIN_VERIFY_REPR=1 as a permanent safety net, given ASan is the only other catcher and CI may not run ASan on every push?
