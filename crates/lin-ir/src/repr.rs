//! Representation-inference pass (Stages 1-2: PURE side-table observer).
//!
//! This computes, per function, a side table `Vec<Repr>` indexed by `Temp.0` giving the *physical
//! representation* each temp carries at runtime — packed sealed struct / packed sealed array / boxed
//! TaggedVal / unboxed flat scalar. It is the foundation for centralizing the packed-vs-boxed
//! decision that is today replicated across three type-driven predicate families
//! (`Codegen::sealed_array_elem`/`sealed_fields`/`is_flat_scalar`, `lower::is_sealed_scalar_array`,
//! `monomorphize::field_packed_scalar`). See `docs/REPR_PASS_DESIGN.md`.
//!
//! # Stage 2 status — OBSERVER ONLY
//!
//! In Stage 2 the pass ONLY computes the table; NOTHING consumes it. Codegen still uses the old
//! predicates. The deliverable is the ORACLE ([`oracle_check`]): a debug-only assertion that the
//! repr the new analysis computes at every site where the OLD predicates decide a representation
//! AGREES with what those predicates decide today. This proves the analysis conservatively
//! reproduces current behaviour before Stage 3 swaps the decision source.
//!
//! # Why a side table, not a Type attribute
//!
//! The same static `Type` is packed in one temp (just constructed) and boxed-wrapping-packed in
//! another (read from a Map slot). `Repr::Boxed(Inner::WrapsPacked)` — a boxed slot whose payload is
//! a still-packed buffer — is UNSPEAKABLE in the type system. Representation is flow-sensitive and
//! per-occurrence; Type is flow-insensitive. See the design doc, "Why NOT on Type".
//!
//! # The lattice
//!
//! `Repr ::= Unknown (TOP) | Packed(Layout) | Boxed(Inner) | FlatScalar(ScalarTy) | Bottom`.
//! Fail-safe: anything the analysis cannot prove is `Boxed(Inner::Opaque)`. A `Packed` label is only
//! ever assigned by PROOF from a definite packed producer carried along representation-preserving
//! edges; on any doubt the temp is Boxed.

use std::collections::HashMap;

use indexmap::IndexMap;
use lin_check::types::Type;

use crate::carry::{self, UnionFind};
use crate::ir::*;

// ---------------------------------------------------------------------------
// The lattice
// ---------------------------------------------------------------------------

/// Physical layout of a packed (un-boxed) sealed value. Two packed values share a layout iff they
/// have the same field map (and, for arrays, the same on-heap flag), so the field `IndexMap` is the
/// layout key. `Type::PartialEq` ignores the `sealed` flag but DOES compare field maps, so equal
/// `fields` ⇒ equal layout — exactly what we want.
#[derive(Debug, Clone, PartialEq)]
pub enum Layout {
    /// A sealed scalar/heap-field record laid out as a packed `[u32 rc | u32 size | desc | fields…]`
    /// struct (the `Codegen::sealed_fields` representation).
    PackedStruct { fields: IndexMap<String, Type> },
    /// A `LinArray` with `elem_tag == 0xFE`: a contiguous, header-less buffer of packed sealed-record
    /// elements (the `Codegen::sealed_array_elem` representation). `on_heap` records whether any
    /// element field is a heap pointer (String/Array/nested-sealed) — false today (Stage 3a scalar).
    PackedSealedArray { elem_layout: IndexMap<String, Type>, on_heap: bool },
}

/// What a `Boxed` slot wraps.
#[derive(Debug, Clone, PartialEq)]
pub enum Inner {
    /// A generic boxed value: `LinObject` / boxed `Object[]` / heterogeneous `TaggedVal`.
    Opaque,
    /// KEY refinement: a boxed slot (`TaggedVal` TAG_ARRAY / TAG_OBJECT) whose payload pointer is a
    /// STILL-PACKED `LinArray*` / packed struct*. This is the keep-packed-by-pointer representation
    /// the static type system cannot express. (Seeded but not yet materialized into IR until later
    /// stages; in Stage 2 it documents the boundary the design will exploit.)
    WrapsPacked(Layout),
}

/// The fixed-width scalar a `FlatScalar` temp holds unboxed (mirrors `Codegen::is_flat_scalar`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarTy {
    I8, U8, I16, U16, I32, U32, I64, U64, F32, F64,
}

impl ScalarTy {
    fn from_type(ty: &Type) -> Option<Self> {
        Some(match ty {
            Type::Int8 => ScalarTy::I8,
            Type::UInt8 => ScalarTy::U8,
            Type::Int16 => ScalarTy::I16,
            Type::UInt16 => ScalarTy::U16,
            Type::Int32 => ScalarTy::I32,
            Type::UInt32 => ScalarTy::U32,
            Type::Int64 => ScalarTy::I64,
            Type::UInt64 => ScalarTy::U64,
            Type::Float32 => ScalarTy::F32,
            Type::Float64 => ScalarTy::F64,
            _ => return None,
        })
    }
}

/// Per-temp representation lattice (see module docs).
#[derive(Debug, Clone, PartialEq)]
pub enum Repr {
    /// TOP — no information yet (a temp the seeding never touched). Folds away under join.
    Unknown,
    /// The value IS the physical packed thing.
    Packed(Layout),
    /// The value is a `TaggedVal*` / `LinObject*` slot wrapping `Inner`.
    Boxed(Inner),
    /// An unboxed i8/i16/i32/i64/f32/f64 (the `is_flat_scalar` primitive).
    FlatScalar(ScalarTy),
    /// No value (e.g. unreachable / never-defined temp).
    Bottom,
}

impl Repr {
    /// The fail-safe representation: a boxed opaque value. Anything not proven otherwise is this.
    pub fn boxed_opaque() -> Repr {
        Repr::Boxed(Inner::Opaque)
    }

    /// `Some(fields)` iff this repr is a packed sealed RECORD (PackedStruct) — codegen's gate for
    /// reading/writing a sealed record as a packed `[rc|size|desc|fields…]` struct.
    pub fn packed_struct_fields(&self) -> Option<&IndexMap<String, Type>> {
        match self {
            Repr::Packed(Layout::PackedStruct { fields }) => Some(fields),
            _ => None,
        }
    }

    /// `Some(elem_layout)` iff this repr is a packed sealed ARRAY (`elem_tag == 0xFE`) — codegen's
    /// gate for the contiguous packed-element buffer read/write/push fast path.
    pub fn packed_sealed_array_layout(&self) -> Option<&IndexMap<String, Type>> {
        match self {
            Repr::Packed(Layout::PackedSealedArray { elem_layout, .. }) => Some(elem_layout),
            _ => None,
        }
    }

    /// True iff this repr is a packed value of EITHER kind (struct or sealed array).
    pub fn is_packed(&self) -> bool {
        matches!(self, Repr::Packed(_))
    }
}

// ---------------------------------------------------------------------------
// Lattice join (over a carry class)
// ---------------------------------------------------------------------------

/// The join of two reprs in the same carry class. Per the design's MEET/JOIN rules:
///   - `join(Unknown, x) = x`, `join(x, Bottom) = x`
///   - same `Packed(L)` stays packed (an ISLAND)
///   - `Packed(L)` vs `Packed(L')` (different layout) demotes to `Boxed(Opaque)`
///   - `Packed(_)` vs `Boxed(_)` is a CONFLICT → returns `Boxed(Opaque)` and the caller records the
///     class as a BOUNDARY (Stage 3+ will SPLIT it with a coercion; Stage 2 only observes).
///   - `FlatScalar(s)` vs `FlatScalar(s)` stays; mismatched flats demote to `Boxed(Opaque)`
///   - `Boxed(WrapsPacked(L))` vs `Boxed(WrapsPacked(L))` stays; vs `Boxed(Opaque)` → `Boxed(Opaque)`
fn join(a: &Repr, b: &Repr) -> Repr {
    use Repr::*;
    match (a, b) {
        (Unknown, x) | (x, Unknown) => x.clone(),
        (Bottom, x) | (x, Bottom) => x.clone(),
        (Packed(l1), Packed(l2)) => {
            if l1 == l2 {
                Packed(l1.clone())
            } else {
                Repr::boxed_opaque()
            }
        }
        (FlatScalar(s1), FlatScalar(s2)) => {
            if s1 == s2 {
                FlatScalar(*s1)
            } else {
                Repr::boxed_opaque()
            }
        }
        (Boxed(i1), Boxed(i2)) => Boxed(join_inner(i1, i2)),
        // Packed vs Boxed, Packed vs FlatScalar, Boxed vs FlatScalar: representation CONFLICT.
        // Fail safe to Boxed(Opaque); the design's STEP 4 (later stage) splits the class.
        _ => Repr::boxed_opaque(),
    }
}

fn join_inner(a: &Inner, b: &Inner) -> Inner {
    match (a, b) {
        (Inner::WrapsPacked(l1), Inner::WrapsPacked(l2)) if l1 == l2 => Inner::WrapsPacked(l1.clone()),
        _ => Inner::Opaque,
    }
}

// ---------------------------------------------------------------------------
// Predicate mirrors — the SINGLE place the new pass reads a Type, used for both seeding and the
// oracle. These mirror the codegen predicates EXACTLY (cited inline). Keeping them here means the
// oracle compares the analysis against the same logic the seeds use, so a disagreement is a real
// divergence between the dataflow result and the per-site type decision, not a transcription bug.
// ---------------------------------------------------------------------------

/// Mirror of `Codegen::is_sealed_scalar_field` (types.rs:166).
fn is_sealed_scalar_field(ty: &Type) -> bool {
    ty.is_flat_scalar() || matches!(ty, Type::Bool)
}

/// Mirror of `Codegen::sealed_field_kind(..).is_some()` (types.rs:177): an eligible HEAP field of a
/// sealed record (String / Array / nested-sealed).
fn is_sealed_heap_field(ty: &Type) -> bool {
    match ty {
        Type::Str | Type::StrLit(_) => true,
        Type::Array(_) | Type::FixedArray(_) => true,
        Type::Object { .. } => sealed_fields(ty).is_some(),
        _ => false,
    }
}

/// Mirror of `Codegen::is_sealed_field` (types.rs:194).
fn is_sealed_field(ty: &Type) -> bool {
    is_sealed_scalar_field(ty) || is_sealed_heap_field(ty)
}

/// Mirror of `Codegen::sealed_fields` (types.rs:210): `Some(fields)` iff `ty` is a sealed record
/// whose fields are ALL scalars or eligible heap fields. This is the PackedStruct gate.
fn sealed_fields(ty: &Type) -> Option<&IndexMap<String, Type>> {
    match ty {
        Type::Object { fields, sealed: true }
            if !fields.is_empty() && fields.values().all(is_sealed_field) =>
        {
            Some(fields)
        }
        _ => None,
    }
}

/// Mirror of `Codegen::sealed_array_elem_field_packable` (types.rs:333): SCALARS ONLY today (Stage
/// 3a). Heap-field elements stay boxed.
fn sealed_array_elem_field_packable(ty: &Type) -> bool {
    is_sealed_scalar_field(ty)
}

/// Mirror of `Codegen::sealed_array_elem` (types.rs:287): `Some(elem_fields)` iff `ty` is
/// `Array(elem)` whose element is a sealed record with ALL packable (scalar) fields. The
/// PackedSealedArray gate.
fn sealed_array_elem(ty: &Type) -> Option<&IndexMap<String, Type>> {
    let elem = match ty {
        Type::Array(e) => e.as_ref(),
        _ => return None,
    };
    let fields = sealed_fields(elem)?;
    if fields.values().all(sealed_array_elem_field_packable) {
        Some(fields)
    } else {
        None
    }
}

/// The repr a value of static `Type` `ty` is READ as by every assume site that dispatches purely on
/// type (`compile_ir_field_get`, `compile_ir_index`, `SealedArrayFieldGet`, …). This is the
/// type-driven seed for params and for any temp whose only evidence is its type. Mirrors the codegen
/// dispatch order: sealed-scalar-array → Packed array; sealed record → Packed struct; flat scalar →
/// FlatScalar; everything else → Boxed(Opaque) (fail safe).
fn type_seed(ty: &Type) -> Repr {
    if let Some(elem_fields) = sealed_array_elem(ty) {
        return Repr::Packed(Layout::PackedSealedArray {
            elem_layout: elem_fields.clone(),
            on_heap: false,
        });
    }
    if let Some(fields) = sealed_fields(ty) {
        return Repr::Packed(Layout::PackedStruct { fields: fields.clone() });
    }
    if let Some(s) = ScalarTy::from_type(ty) {
        return Repr::FlatScalar(s);
    }
    Repr::boxed_opaque()
}

/// The repr the MakeObject DECIDE site (`codegen/mod.rs:1191`) actually produces. Codegen packs ONLY
/// when `sealed_scalar_fields(ty).is_some()` AND there are NO spreads AND every sealed field is
/// present in the literal (`all_present`). Field omission or a spread → the boxed `LinObject` path.
fn make_object_repr(ty: &Type, fields: &[(String, Temp)], spreads: &[Temp]) -> Repr {
    if let Some(sf) = sealed_fields(ty) {
        let all_present =
            spreads.is_empty() && sf.keys().all(|k| fields.iter().any(|(fk, _)| fk == k));
        if all_present {
            return Repr::Packed(Layout::PackedStruct { fields: sf.clone() });
        }
    }
    Repr::boxed_opaque()
}

/// The repr the MakeArray DECIDE site (`codegen/mod.rs:1399`) produces: a Packed SEALED array when
/// `sealed_array_elem(Array(elem)).is_some()`, otherwise a heap `LinArray` pointer (a flat-scalar
/// buffer when the element is a flat scalar, a boxed `Object[]` otherwise).
///
/// NOTE: a flat-scalar ARRAY is NOT `Repr::FlatScalar` — that variant is the single-unboxed-scalar
/// PRIMITIVE (a loop counter, a field value), not a contiguous buffer. The flat-array path is the
/// pre-existing `lin_flat_array_*` representation, orthogonal to the sealed packed-vs-boxed decision
/// this pass centralizes; in Stage 2 a non-sealed array temp is left as the fail-safe `Boxed(Opaque)`
/// (nothing asserts a more specific repr on an array temp — assume sites dispatch on the array TYPE,
/// not its repr). Conflating it with `FlatScalar` was an analysis bug the Stage-2 oracle surfaced on
/// `Float64[]` literals in stdlib (`nextPair`/`applyKey`).
fn make_array_repr(elem_ty: &Type) -> Repr {
    // Reconstruct the Array(elem) view the codegen predicate gates on.
    let arr_ty = Type::Array(Box::new(elem_ty.clone()));
    if let Some(elem_fields) = sealed_array_elem(&arr_ty) {
        return Repr::Packed(Layout::PackedSealedArray {
            elem_layout: elem_fields.clone(),
            on_heap: false,
        });
    }
    Repr::boxed_opaque()
}

// ---------------------------------------------------------------------------
// The analysis
// ---------------------------------------------------------------------------

/// Compute the per-temp representation table for one function. Single-pass union-find over carry
/// classes (shared `carry.rs`), seed each temp at its definite producer / param / type, fold per
/// class via the lattice join, then write the class repr back to every member.
///
/// Indexed by `Temp.0`; `result[t]` is the repr of temp `t`.
pub fn analyze(func: &LinFunction) -> Vec<Repr> {
    let n = func.temp_count as usize;
    if n == 0 {
        return Vec::new();
    }

    // STEP 1 — build carry classes (representation-preserving edges only).
    let mut uf = UnionFind::new(func.temp_count);
    for block in &func.blocks {
        for instr in &block.instructions {
            carry::classify_carry_edges(instr, &mut uf);
        }
        if let Terminator::TailCall { args } = &block.terminator {
            // Self-tail-call arg i carries into param i (next iteration). Mirrors escape.rs.
            let _ = carry::classify_tailcall_carry(args, &func.params, &mut uf);
        }
    }

    // STEP 2 — seed per-temp local reprs at definite producers, params, and type evidence.
    let mut seeds: Vec<Repr> = vec![Repr::Unknown; n];

    // Params: seeded by their declared type (the assume sites dispatch on type, so this is the repr
    // codegen reads a param as). A boxed-ABI param (generic-T / union / Json) seeds Boxed(Opaque).
    for (p, ty) in &func.params {
        if (p.0 as usize) < n {
            seeds[p.0 as usize] = type_seed(ty);
        }
    }

    for block in &func.blocks {
        for instr in &block.instructions {
            seed_instr(instr, func, &mut seeds);
        }
    }

    // STEP 3 — fold per-temp seeds into per-class reprs via the lattice join.
    let mut class_repr: HashMap<u32, Repr> = HashMap::new();
    for t in 0..func.temp_count {
        let s = &seeds[t as usize];
        if matches!(s, Repr::Unknown) {
            continue;
        }
        let root = uf.find_raw(t);
        let entry = class_repr.entry(root).or_insert(Repr::Unknown);
        *entry = join(entry, s);
    }

    // Write the class repr back to every member. A temp whose class has no seed defaults to
    // Boxed(Opaque) (fail safe).
    let mut result = vec![Repr::boxed_opaque(); n];
    for t in 0..func.temp_count {
        let root = uf.find_raw(t);
        if let Some(r) = class_repr.get(&root) {
            result[t as usize] = r.clone();
        }
    }
    result
}

/// Seed the local repr of the temp(s) defined by one instruction (STEP 2). Only DEFINITE producers
/// and type-evident defs are seeded; everything else stays `Unknown` and inherits its class fold (or
/// fails safe to Boxed at the end).
fn seed_instr(instr: &Instruction, func: &LinFunction, seeds: &mut [Repr]) {
    let set = |seeds: &mut [Repr], t: Temp, r: Repr| {
        if (t.0 as usize) < seeds.len() {
            // A def's seed is authoritative for that temp; if two producers ever defined the same
            // temp (they don't in SSA) we would join, but SSA guarantees one def, so overwrite.
            seeds[t.0 as usize] = r;
        }
    };
    match instr {
        // ---- DEFINITE PACKED / FLATSCALAR producers ----
        Instruction::MakeObject { dst, ty, fields, spreads, .. } => {
            set(seeds, *dst, make_object_repr(ty, fields, spreads));
        }
        Instruction::MakeArray { dst, elem_ty, .. } => {
            set(seeds, *dst, make_array_repr(elem_ty));
        }
        // A whole sealed-record element read by Index yields a PACKED struct REGARDLESS of whether
        // the array is packed: a packed sealed array goes through `sealed_array_materialize_elem`
        // (data.rs:347), and a BOXED sealed-record array's `arr[i]` is unboxed via
        // `unbox_tagged_val_to_type` whose sealed-target arm PROJECTS the boxed LinObject into a
        // fresh packed struct (boxing.rs:365 -> sealed_project_from). So the discriminator is the
        // RESULT type, not the array repr. A flat element is FlatScalar.
        Instruction::Index { dst, obj_ty, result_ty, .. } => {
            if let Some(f) = sealed_fields(result_ty) {
                set(seeds, *dst, Repr::Packed(Layout::PackedStruct { fields: f.clone() }));
            } else if let Some(s) = ScalarTy::from_type(result_ty) {
                // Flat element read (only when the array is genuinely flat — but the result type
                // already encodes that for the assume site; the oracle checks the site precisely).
                if !matches!(obj_ty, Type::FixedArray(_)) {
                    set(seeds, *dst, Repr::FlatScalar(s));
                }
            }
        }
        // A field READ of a sealed record. A SCALAR field is a flat scalar; a NESTED-SEALED field
        // (KIND_SEALED) is stored as an 8-byte POINTER to the inner packed struct, so `sealed_field_get`
        // loads that pointer and the result is itself a Packed struct (e.g. `line["a"]` : Pt). Seed both
        // so a chained `line["a"]["x"]` reads t10 (the inner Pt) as Packed at the second FieldGet.
        Instruction::FieldGet { dst, result_ty, .. } => {
            if let Some(s) = ScalarTy::from_type(result_ty) {
                set(seeds, *dst, Repr::FlatScalar(s));
            } else if let Some(f) = sealed_fields(result_ty) {
                set(seeds, *dst, Repr::Packed(Layout::PackedStruct { fields: f.clone() }));
            }
        }
        Instruction::SealedArrayFieldGet { dst, result_ty, .. } => {
            if let Some(s) = ScalarTy::from_type(result_ty) {
                set(seeds, *dst, Repr::FlatScalar(s));
            }
        }
        // Scalar arithmetic / comparison / constants → FlatScalar.
        Instruction::Const { dst, val } => {
            let s = match val {
                Const::Int(_, ty) | Const::Float(_, ty) => ScalarTy::from_type(ty),
                _ => None,
            };
            if let Some(s) = s {
                set(seeds, *dst, Repr::FlatScalar(s));
            }
        }
        Instruction::Binary { dst, ty, .. } => {
            if let Some(s) = ScalarTy::from_type(ty) {
                set(seeds, *dst, Repr::FlatScalar(s));
            }
        }
        Instruction::Unary { dst, ty, .. } => {
            if let Some(s) = ScalarTy::from_type(ty) {
                set(seeds, *dst, Repr::FlatScalar(s));
            }
        }
        // ---- Cross-boundary reads: BOXED seeds (the dynamic boundaries the type cannot see) ----
        // A self/direct/named Call return whose type is packed is returned by-pointer by our ABI
        // (commitment from the design); seed Boxed(WrapsPacked) for a packed return so the use-site
        // unbox is justified, else type_seed. For the oracle's purposes the call DST is not itself
        // an old-predicate decide site, so any reasonable seed is acceptable as long as the join
        // does not contradict an assume site reading it; default to type_seed (conservative).
        Instruction::Call { dst, ret_ty, .. } => {
            set(seeds, *dst, type_seed(ret_ty));
        }
        Instruction::CallIntrinsic { dst, ret_ty, .. } => {
            set(seeds, *dst, type_seed(ret_ty));
        }
        // Coerce: the dst's repr is its target type's repr (a repr-changing coerce is NOT a carry
        // edge, so dst is a fresh value seeded by to_ty). A carry coerce was already unified in
        // STEP 1 and dst inherits the class.
        Instruction::Coerce { dst, to_ty, from_ty, .. } => {
            if !carry::coerce_is_carry(from_ty, to_ty) {
                set(seeds, *dst, type_seed(to_ty));
            }
        }
        // Box / Unbox: explicit representation changes. Box → Boxed; Unbox → the unboxed type repr.
        Instruction::Box { dst, .. } => set(seeds, *dst, Repr::boxed_opaque()),
        Instruction::Unbox { dst, result_ty, .. } => set(seeds, *dst, type_seed(result_ty)),
        // PURE CARRY edges define a temp that is an ALIAS of its source: its repr comes from the
        // source's class fold (STEP 1 already unified them), NOT from a fresh seed. Seeding them by
        // their declared type would inject a spurious seed that, if it differed from the carried
        // value's proven repr (e.g. a Copy of a Packed struct whose dst type_seed is also Packed but
        // for a Boxed-typed alias, or any imprecision), would JOIN as a CONFLICT and wrongly demote
        // the class. So leave Copy/Bind/Phi/carry-Coerce defs UNSEEDED — they inherit the class.
        Instruction::Copy { .. } | Instruction::Bind { .. } | Instruction::Phi { .. } => {}
        // Everything else that defines a temp: seed by the temp's recorded type (the fail-safe
        // type_seed). This covers EnvCapture, GlobalValGet, CellGet, ObjectRest, CloneBox, etc.
        other => {
            let (_uses, defs) = crate::liveness::instr_use_def(other);
            for d in defs {
                if matches!(seeds.get(d.0 as usize), Some(Repr::Unknown)) {
                    let ty = func.temp_types.get(&d).cloned().unwrap_or(Type::Null);
                    seeds[d.0 as usize] = type_seed(&ty);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Stage-2 ORACLE — repr == old-predicate at every DECIDE / ASSUME site.
// ---------------------------------------------------------------------------

/// Is `repr` a Packed struct with exactly `fields`?
fn is_packed_struct(repr: &Repr, fields: &IndexMap<String, Type>) -> bool {
    matches!(repr, Repr::Packed(Layout::PackedStruct { fields: f }) if f == fields)
}

/// Is `repr` a Packed sealed array whose element layout is `elem_fields`?
fn is_packed_sealed_array(repr: &Repr, elem_fields: &IndexMap<String, Type>) -> bool {
    matches!(repr, Repr::Packed(Layout::PackedSealedArray { elem_layout, .. }) if elem_layout == elem_fields)
}

/// The Stage-2 oracle: at every site where the OLD predicates decide a representation, assert the
/// new analysis AGREES. Debug-only (callers gate with `cfg(debug_assertions)`). A disagreement is
/// either a bug in the new analysis OR a latent bug in the old predicates — it must be reconciled
/// before Stage 3 trusts the table.
///
/// Returns the list of disagreements as human-readable strings (empty == agreement). Panicking is
/// the caller's choice (the pipeline driver does `debug_assert!(disagreements.is_empty())`).
pub fn oracle_check(func: &LinFunction, repr: &[Repr]) -> Vec<String> {
    let mut bad = Vec::new();
    let fname = func.name.clone().unwrap_or_else(|| format!("fn#{}", func.id.0));
    let report = |bad: &mut Vec<String>, site: &str, t: Temp, expect: &str, got: &Repr| {
        bad.push(format!(
            "{fname}: {site} on t{} — old predicate says {expect}, repr says {got:?}",
            t.0
        ));
    };

    for block in &func.blocks {
        for instr in &block.instructions {
            match instr {
                // DECIDE: MakeObject — packed-construct iff sealed + all_present + no spread.
                Instruction::MakeObject { dst, ty, fields, spreads, .. } => {
                    let expected = make_object_repr(ty, fields, spreads);
                    let r = &repr[dst.0 as usize];
                    match &expected {
                        Repr::Packed(Layout::PackedStruct { fields: f }) => {
                            if !is_packed_struct(r, f) {
                                report(&mut bad, "MakeObject(packed)", *dst, "Packed(struct)", r);
                            }
                        }
                        _ => {
                            if matches!(r, Repr::Packed(_)) {
                                report(&mut bad, "MakeObject(boxed)", *dst, "Boxed", r);
                            }
                        }
                    }
                }
                // DECIDE: MakeArray — packed SEALED array vs. a (flat or boxed) heap LinArray. Only
                // the packed-sealed-array decision is a sealed packed-vs-boxed call this pass owns;
                // the flat-vs-boxed-Object[] distinction is the orthogonal pre-existing flat-array
                // path (assume sites dispatch on the array TYPE), so the oracle only asserts the
                // sealed-array case is Packed and the non-sealed case is NOT Packed.
                Instruction::MakeArray { dst, elem_ty, .. } => {
                    let expected = make_array_repr(elem_ty);
                    let r = &repr[dst.0 as usize];
                    match &expected {
                        Repr::Packed(Layout::PackedSealedArray { elem_layout, .. }) => {
                            if !is_packed_sealed_array(r, elem_layout) {
                                report(&mut bad, "MakeArray(packed)", *dst, "Packed(sealed array)", r);
                            }
                        }
                        _ => {
                            if matches!(r, Repr::Packed(_)) {
                                report(&mut bad, "MakeArray(non-sealed)", *dst, "non-Packed", r);
                            }
                        }
                    }
                }
                // ASSUME: FieldGet — object is read as a packed struct iff sealed_scalar_fields.
                // STAGE 3 swaps this site to read `func.repr`; the oracle now checks BOTH directions
                // so the swap is provably byte identical: (forward) the old predicate ⇒ repr Packed,
                // AND (reverse) repr Packed-struct ⇒ the old predicate fired (else codegen would take
                // the packed path where it previously boxed).
                Instruction::FieldGet { object, obj_ty, .. } => {
                    let r = &repr[object.0 as usize];
                    match sealed_fields(obj_ty) {
                        Some(f) => {
                            if !is_packed_struct(r, f) {
                                report(&mut bad, "FieldGet(object packed)", *object, "Packed(struct)", r);
                            }
                        }
                        None => {
                            if r.packed_struct_fields().is_some() {
                                report(&mut bad, "FieldGet(object NOT predicate-packed)", *object, "non-Packed-struct", r);
                            }
                        }
                    }
                }
                // ASSUME: SealedArrayFieldGet — array is read as a packed sealed array (both directions).
                Instruction::SealedArrayFieldGet { array, arr_ty, .. } => {
                    let r = &repr[array.0 as usize];
                    match sealed_array_elem(arr_ty) {
                        Some(ef) => {
                            if !is_packed_sealed_array(r, ef) {
                                report(&mut bad, "SealedArrayFieldGet(array packed)", *array, "Packed(sealed array)", r);
                            }
                        }
                        None => {
                            if r.packed_sealed_array_layout().is_some() {
                                report(&mut bad, "SealedArrayFieldGet(array NOT predicate-packed)", *array, "non-Packed-array", r);
                            }
                        }
                    }
                }
                // ASSUME: Index — object read as packed sealed array (whole-element materialize).
                // STAGE 3 swaps the sealed-array gate AND the sealed-record dynamic-key gate to repr;
                // check both directions for each (packed-array and packed-struct).
                Instruction::Index { object, obj_ty, .. } => {
                    let r = &repr[object.0 as usize];
                    match sealed_array_elem(obj_ty) {
                        Some(ef) => {
                            if !is_packed_sealed_array(r, ef) {
                                report(&mut bad, "Index(object packed array)", *object, "Packed(sealed array)", r);
                            }
                        }
                        None => {
                            if r.packed_sealed_array_layout().is_some() {
                                report(&mut bad, "Index(object NOT predicate-packed-array)", *object, "non-Packed-array", r);
                            }
                        }
                    }
                    // The sealed-record (dynamic non-literal key) gate: codegen reads
                    // `obj_repr.packed_struct_fields()`. Verify repr Packed-struct iff the type is a
                    // sealed record. (A sealed-record obj reaching Index is the `p[k]` runtime-key case.)
                    match sealed_fields(obj_ty) {
                        Some(f) => {
                            if !is_packed_struct(r, f) {
                                report(&mut bad, "Index(object packed struct)", *object, "Packed(struct)", r);
                            }
                        }
                        None => {
                            if r.packed_struct_fields().is_some() {
                                report(&mut bad, "Index(object NOT predicate-packed-struct)", *object, "non-Packed-struct", r);
                            }
                        }
                    }
                }
                // DECIDE/ASSUME: Push — array operand repr (packed sealed array / flat); element
                // operand repr for a packed array is a Packed struct.
                Instruction::CallIntrinsic { intrinsic: Intrinsic::Push, args, .. } if args.len() >= 2 => {
                    let arr = args[0];
                    let arr_ty = func.temp_types.get(&arr).cloned().unwrap_or(Type::Null);
                    if let Some(ef) = sealed_array_elem(&arr_ty) {
                        let r = &repr[arr.0 as usize];
                        if !is_packed_sealed_array(r, ef) {
                            report(&mut bad, "Push(array packed)", arr, "Packed(sealed array)", r);
                        }
                        // The pushed element is a standalone packed struct of the elem layout.
                        let elem = args[1];
                        let er = &repr[elem.0 as usize];
                        if !is_packed_struct(er, ef) {
                            report(&mut bad, "Push(elem packed struct)", elem, "Packed(struct)", er);
                        }
                    }
                }
                // NOTE: IndexSet is NOT oracled here. emit_map_set / emit_sealed_array_set decide the
                // SLOT storage purely from the container's element TYPE and COERCE the value operand
                // into it at the store (a flat-scalar slot coerces a boxed Json value in; a sealed
                // array slot projects a representation-mismatched RHS via sealed_project_from). The
                // value operand therefore carries its own declared `val_ty` repr (no fixed packed/flat
                // operand requirement to assert), so there is no decide-site disagreement to check —
                // confirmed by the dijkstra `Map(Int64)` += and `Map(Neighbor[])` store sites where the
                // RHS arrives as a Json/boxed temp and is coerced at the slot. (Stage 3 makes those
                // coercions explicit IR; today they are emitted inside emit_map_set.)
                _ => {}
            }
        }
    }
    bad
}

// ---------------------------------------------------------------------------
// Soundness verifier — repr each opcode REQUIRES of an operand == repr[operand].
// ---------------------------------------------------------------------------

/// DEBUG/TEST-only soundness gate (design soundness gate #1). Walks every instruction, computes the
/// repr each opcode REQUIRES of each operand, and returns any mismatch with `repr[operand]`.
///
/// STAGE 3: this verifier is now LOAD-BEARING. Codegen reads `func.repr[operand]` (not the static
/// `Type`) at exactly the ASSUME sites checked here — `FieldGet`, `SealedArrayFieldGet`, `Index` —
/// to decide the packed constant-offset load. So a violation (an operand the opcode reads as packed
/// whose `func.repr` is NOT the matching Packed layout) would mean codegen emits a packed load
/// against a value that is not physically packed: a silent representation mismatch / UAF. The
/// `debug_assert!` in [`run`] turns that into a compile-time panic. This is the formal statement of
/// the design's "a mismatch is inexpressible" invariant for the swapped sites. Returns the list of
/// violations (empty == sound).
pub fn verify(func: &LinFunction, repr: &[Repr]) -> Vec<String> {
    let mut bad = Vec::new();
    let fname = func.name.clone().unwrap_or_else(|| format!("fn#{}", func.id.0));
    for block in &func.blocks {
        for instr in &block.instructions {
            match instr {
                Instruction::FieldGet { object, obj_ty, .. } => {
                    if let Some(f) = sealed_fields(obj_ty) {
                        if !is_packed_struct(&repr[object.0 as usize], f) {
                            bad.push(format!(
                                "{fname}: FieldGet requires Packed(struct) of t{}, has {:?}",
                                object.0, repr[object.0 as usize]
                            ));
                        }
                    }
                }
                Instruction::SealedArrayFieldGet { array, arr_ty, .. } => {
                    if let Some(ef) = sealed_array_elem(arr_ty) {
                        if !is_packed_sealed_array(&repr[array.0 as usize], ef) {
                            bad.push(format!(
                                "{fname}: SealedArrayFieldGet requires Packed(sealed array) of t{}, has {:?}",
                                array.0, repr[array.0 as usize]
                            ));
                        }
                    }
                }
                Instruction::Index { object, obj_ty, .. } => {
                    if let Some(ef) = sealed_array_elem(obj_ty) {
                        if !is_packed_sealed_array(&repr[object.0 as usize], ef) {
                            bad.push(format!(
                                "{fname}: Index requires Packed(sealed array) of t{}, has {:?}",
                                object.0, repr[object.0 as usize]
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    bad
}

/// Run the analysis for every function in a module and STORE the result on `func.repr`
/// (Stage 3: codegen consumes this table at every packed-vs-boxed DECIDE / ASSUME site).
///
/// In debug builds this ALSO runs the Stage-2 oracle (repr == old type predicate at every site
/// where the old predicates decide a representation) as a regression check that the swap changed
/// no decisions, and the soundness verifier (no opcode reads an operand whose required physical
/// repr differs from `func.repr[operand]`). Wired into the pipeline immediately before `rc_elide`,
/// so RC sees representation-stable IR.
pub fn run(module: &mut LinModule) {
    for func in &mut module.functions {
        let repr = analyze(func);
        #[cfg(debug_assertions)]
        {
            let disagreements = oracle_check(func, &repr);
            debug_assert!(
                disagreements.is_empty(),
                "repr Stage-2 ORACLE disagreement(s) (new analysis != old predicate):\n{}",
                disagreements.join("\n")
            );
            let violations = verify(func, &repr);
            debug_assert!(
                violations.is_empty(),
                "repr verifier violation(s) (opcode-required repr != repr[operand]):\n{}",
                violations.join("\n")
            );
        }
        // Stage 3: codegen now reads this table.
        func.repr = repr;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;

    fn pt_fields() -> IndexMap<String, Type> {
        let mut m = IndexMap::new();
        m.insert("x".into(), Type::Int32);
        m.insert("y".into(), Type::Int32);
        m
    }

    fn sealed(fields: IndexMap<String, Type>) -> Type {
        Type::Object { fields, sealed: true }
    }

    /// Build a single-block function from a list of instructions + a return temp.
    fn func_of(instrs: Vec<Instruction>, ret: Option<Temp>, temp_count: u32, params: Vec<(Temp, Type)>) -> LinFunction {
        let mut temp_types = HashMap::new();
        for (t, ty) in &params {
            temp_types.insert(*t, ty.clone());
        }
        LinFunction {
            id: FuncId(0),
            name: Some("t".into()),
            params,
            is_closure: false,
            ret_ty: Type::Null,
            blocks: vec![BasicBlock {
                id: BlockId(0),
                label: None,
                instructions: instrs,
                terminator: Terminator::Return(ret),
                span: None,
            }],
            temp_types,
            temp_count,
            intrinsic_slots: HashMap::new(),
            repr: Vec::new(),
        }
    }

    #[test]
    fn make_object_sealed_all_present_is_packed() {
        let pt = sealed(pt_fields());
        let instrs = vec![
            Instruction::Const { dst: Temp(0), val: Const::Int(1, Type::Int32) },
            Instruction::Const { dst: Temp(1), val: Const::Int(2, Type::Int32) },
            Instruction::MakeObject {
                dst: Temp(2),
                fields: vec![("x".into(), Temp(0)), ("y".into(), Temp(1))],
                spreads: vec![],
                ty: pt.clone(),
                stack: false,
            },
        ];
        let f = func_of(instrs, Some(Temp(2)), 3, vec![]);
        let repr = analyze(&f);
        assert!(matches!(repr[2], Repr::Packed(Layout::PackedStruct { .. })));
        assert!(oracle_check(&f, &repr).is_empty());
        assert!(verify(&f, &repr).is_empty());
    }

    #[test]
    fn make_object_field_omitted_is_boxed() {
        // A sealed type whose literal OMITS a field → codegen boxes; repr must agree (not Packed).
        let pt = sealed(pt_fields());
        let instrs = vec![
            Instruction::Const { dst: Temp(0), val: Const::Int(1, Type::Int32) },
            Instruction::MakeObject {
                dst: Temp(1),
                fields: vec![("x".into(), Temp(0))], // "y" omitted
                spreads: vec![],
                ty: pt,
                stack: false,
            },
        ];
        let f = func_of(instrs, Some(Temp(1)), 2, vec![]);
        let repr = analyze(&f);
        assert!(!matches!(repr[1], Repr::Packed(_)), "omitted-field literal must not be Packed");
        assert!(oracle_check(&f, &repr).is_empty());
    }

    #[test]
    fn flat_scalar_array_is_not_flatscalar_repr() {
        // A Float64[] literal is a heap LinArray buffer, NOT the single-scalar FlatScalar repr.
        let instrs = vec![
            Instruction::Const { dst: Temp(0), val: Const::Float(1.0, Type::Float64) },
            Instruction::Const { dst: Temp(1), val: Const::Float(2.0, Type::Float64) },
            Instruction::MakeArray { dst: Temp(2), elements: vec![Temp(0), Temp(1)], elem_ty: Type::Float64 },
        ];
        let f = func_of(instrs, Some(Temp(2)), 3, vec![]);
        let repr = analyze(&f);
        assert!(!matches!(repr[2], Repr::FlatScalar(_)), "flat array temp must not be FlatScalar");
        assert!(!matches!(repr[2], Repr::Packed(_)), "flat (non-sealed) array must not be Packed");
        assert!(oracle_check(&f, &repr).is_empty());
    }

    #[test]
    fn packed_carries_through_copy() {
        let pt = sealed(pt_fields());
        let instrs = vec![
            Instruction::Const { dst: Temp(0), val: Const::Int(1, Type::Int32) },
            Instruction::Const { dst: Temp(1), val: Const::Int(2, Type::Int32) },
            Instruction::MakeObject {
                dst: Temp(2),
                fields: vec![("x".into(), Temp(0)), ("y".into(), Temp(1))],
                spreads: vec![],
                ty: pt.clone(),
                stack: false,
            },
            Instruction::Copy { dst: Temp(3), src: Temp(2) },
            Instruction::FieldGet { dst: Temp(4), object: Temp(3), field: "x".into(), obj_ty: pt, result_ty: Type::Int32 },
        ];
        let f = func_of(instrs, Some(Temp(4)), 5, vec![]);
        let repr = analyze(&f);
        // The copied temp carries the packed repr, so the FieldGet object operand is Packed.
        assert!(matches!(repr[3], Repr::Packed(Layout::PackedStruct { .. })));
        assert!(matches!(repr[4], Repr::FlatScalar(ScalarTy::I32)));
        assert!(oracle_check(&f, &repr).is_empty());
        assert!(verify(&f, &repr).is_empty());
    }

    #[test]
    fn nested_sealed_field_read_is_packed() {
        // line["a"] where Line = { a: Pt, b: Pt } yields a Packed Pt struct (KIND_SEALED ptr load).
        let pt = sealed(pt_fields());
        let mut line_f = IndexMap::new();
        line_f.insert("a".into(), pt.clone());
        line_f.insert("b".into(), pt.clone());
        let line = sealed(line_f);
        let instrs = vec![
            Instruction::FieldGet { dst: Temp(1), object: Temp(0), field: "a".into(), obj_ty: line.clone(), result_ty: pt.clone() },
            Instruction::FieldGet { dst: Temp(2), object: Temp(1), field: "x".into(), obj_ty: pt, result_ty: Type::Int32 },
        ];
        let f = func_of(instrs, Some(Temp(2)), 3, vec![(Temp(0), line)]);
        let repr = analyze(&f);
        assert!(matches!(repr[1], Repr::Packed(Layout::PackedStruct { .. })), "nested sealed field read must be Packed");
        assert!(oracle_check(&f, &repr).is_empty());
        assert!(verify(&f, &repr).is_empty());
    }
}
