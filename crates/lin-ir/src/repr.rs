//! Representation-inference pass — the single owner of the packed-vs-boxed decision (ADR-062).
//!
//! This computes, per function, a side table `Vec<Repr>` indexed by `Temp.0` giving the *physical
//! representation* each temp carries at runtime — packed sealed struct / packed sealed array / boxed
//! TaggedVal / unboxed flat scalar — stored on `LinFunction.repr`. Codegen reads `func.repr[t]` at
//! the decide/assume sites instead of re-deriving from `Type`. See ADR-062 in `docs/DECISIONS.md`.
//!
//! # Consumed by codegen
//!
//! The DECIDE sites (MakeObject/MakeArray/Push) read the resolved repr of the produced temp; the
//! ASSUME sites (FieldGet/SealedArrayFieldGet/Index, the IndexSet RHS, and `Release`) read
//! `func.repr[operand]` to choose the packed-vs-boxed load/store/free. [`oracle_check`] (debug-only)
//! asserts the analysis agrees with the old type predicate at every decide site (so each swap is a
//! conservative no-op), and [`verify`] (debug-only) asserts the repr each opcode REQUIRES of an
//! operand equals `func.repr[operand]` — making a silent representation mismatch a compile-time panic.
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
    /// element field is a heap pointer (String/Array/nested-sealed) — true for Stage-3b heap-field
    /// arrays, meaning element drop runs per-field release (`release_sealed_array_elems`). It is a
    /// deterministic function of `elem_layout` (`elem_layout_on_heap`), so it never independently
    /// affects the lattice join.
    PackedSealedArray { elem_layout: IndexMap<String, Type>, on_heap: bool },
    /// An unboxed tagged sum-type value (`lin_runtime::sumnode` — unboxed-sumtype Stage 1): a pointer
    /// to a heap `SumNode` `[u32 rc | u32 size | u64 desc | u32 tag | u32 pad | max-variant payload]`.
    /// The layout key is the WHOLE sum type's field shape: the discriminant key plus the ordered
    /// per-variant (discriminant-value → payload field map). Two SumNode values share a layout iff
    /// their sum types are identical, so the canonical `Type::Union` itself is the key (its
    /// `PartialEq` is field-order-sensitive on each variant Object, matching the physical layout).
    /// Stage 1 is NON-RECURSIVE, SCALAR-ONLY — heap/recursive variants fall back to the boxed union.
    SumNode { sum_ty: Type },
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

    /// `Some(sum_ty)` iff this repr is an unboxed tagged sum-type value (`Layout::SumNode`) — the
    /// codegen gate for the `lin_sumnode_*` construct / tag-switch / const-offset payload path.
    pub fn sumnode_sum_ty(&self) -> Option<&Type> {
        match self {
            Repr::Packed(Layout::SumNode { sum_ty }) => Some(sum_ty),
            _ => None,
        }
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

/// Mirror of `Codegen::sum_type_discriminant` (types.rs) — the SINGLE Stage-1 sum-type gate. Returns
/// the discriminant key iff `ty` is a `Type::Union` of 2+ object variants sharing a distinct StrLit
/// discriminant whose every OTHER field is an unboxed scalar (NON-RECURSIVE, SCALAR-ONLY). Any
/// violation → `None` → the value stays a boxed union (fail-safe). Kept byte-identical to codegen.
pub fn sum_type_discriminant_of(ty: &Type) -> Option<String> {
    sum_type_discriminant(ty)
}

/// Mirror of `Codegen::sum_recursive_self_name` (unboxed-sumtype Stage 2). The UNIQUE recursive
/// self-reference name of a candidate sum union, or `None` if it has no recursive child or >1
/// distinct self-name (mutual recursion — out of scope → boxed fail-safe). Kept byte-identical.
pub fn sum_recursive_self_name(ty: &Type) -> Option<String> {
    let variants = match ty {
        Type::Union(vs) => vs,
        _ => return None,
    };
    let mut names: std::collections::HashSet<String> = std::collections::HashSet::new();
    for v in variants {
        if let Type::Object { fields, .. } = v {
            for fty in fields.values() {
                if let Type::Named(n) = fty {
                    names.insert(n.clone());
                }
            }
        }
    }
    if names.len() == 1 {
        names.into_iter().next()
    } else {
        None
    }
}

/// Mirror of `Codegen::is_sum_recursive_child` (Stage 2): a `Type::Named(self_name)` recursive child.
fn is_sum_recursive_child(fty: &Type, self_name: &str) -> bool {
    matches!(fty, Type::Named(n) if n == self_name)
}

fn sum_type_discriminant(ty: &Type) -> Option<String> {
    let variants = match ty {
        Type::Union(vs) => vs,
        _ => return None,
    };
    if variants.len() < 2 {
        return None;
    }
    // Stage 2: the unique recursive self-name (if any). A `Named(self_name)` field is a legal
    // recursive child (`*SumNode` slot); any other Named/heap/union field → boxed (fail-safe).
    let self_name = sum_recursive_self_name(ty);
    let mut recs: Vec<&IndexMap<String, Type>> = Vec::with_capacity(variants.len());
    for v in variants {
        match v {
            Type::Object { fields, .. } if !fields.is_empty() => recs.push(fields),
            _ => return None,
        }
    }
    let first = recs[0];
    'keys: for (key, kty) in first.iter() {
        if !matches!(kty, Type::StrLit(_)) {
            continue;
        }
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for rec in &recs {
            match rec.get(key) {
                Some(Type::StrLit(s)) => {
                    if !seen.insert(s.clone()) {
                        continue 'keys;
                    }
                }
                _ => continue 'keys,
            }
        }
        for rec in &recs {
            for (fk, fty) in rec.iter() {
                if fk == key {
                    continue;
                }
                if matches!(fty, Type::StrLit(_)) {
                    return None;
                }
                let is_recursive_child = self_name
                    .as_deref()
                    .is_some_and(|n| is_sum_recursive_child(fty, n));
                if !is_sealed_scalar_field(fty) && !is_recursive_child {
                    return None;
                }
            }
        }
        return Some(key.clone());
    }
    None
}

/// True iff `ty` is a Stage-1-eligible unboxed sum type. Public so the lowerer (`lower.rs`) can
/// gate its sum-type boundary insertions on the IDENTICAL gate the repr pass + codegen use (the
/// single source of truth is `sum_type_discriminant`).
pub fn sum_type_eligible(ty: &Type) -> bool {
    sum_type_discriminant(ty).is_some()
}

/// The FULL field map (including the discriminant + any recursive children) of the variant of sum
/// type `ty` whose discriminant value is `disc`. Public so the lowerer can push a nested literal into
/// the correct child-sum slot (NESTED-LITERAL DISCRIMINANT PUSHDOWN, design §6 gap 1). `None` when
/// `ty` is not a sum type or no variant carries `disc`.
pub fn sumnode_variant_by_disc(ty: &Type, disc: &str) -> Option<IndexMap<String, Type>> {
    let key = sum_type_discriminant(ty)?;
    let variants = match ty {
        Type::Union(vs) => vs,
        _ => return None,
    };
    for v in variants {
        if let Type::Object { fields, .. } = v {
            if let Some(Type::StrLit(s)) = fields.get(&key) {
                if s == disc {
                    return Some(fields.clone());
                }
            }
        }
    }
    None
}

/// Delegates to the SINGLE source of truth `Type::is_sealed_array_field_packable` (ADR-063 gate
/// consolidation), in lockstep with `Codegen::sealed_array_elem_field_packable`, lower.rs
/// `is_sealed_array_elem_field_packable`, and `monomorphize::field_packed_scalar`.
fn sealed_array_elem_field_packable(ty: &Type) -> bool {
    ty.is_sealed_array_field_packable()
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
    // NOTE (unboxed-sumtype Stage 1): the SumNode SEED is intentionally NOT yet enabled here. The
    // lattice variant, the strict `sum_type_eligible` gate, the oracle/verify SumNode arms, the
    // runtime `SumNode`, and the codegen layout/construct/dispatch/materialize primitives are all in
    // place and unit-tested, but the END-TO-END representation (call ABI: passing a sum value
    // by-SumNode-pointer and reading a sum PARAM as a SumNode, plus global-init construction) is not
    // yet wired. Seeding SumNode before codegen consumes it at EVERY site would create a boxed-vs-
    // packed mismatch (the param is labelled SumNode but the body reads it boxed → UAF), which the
    // oracle/verify correctly flag. Until the ABI is wired, sum values stay boxed (fail-safe, zero
    // behavior change). Flip this on (return `Packed(SumNode)`) together with the ABI work.
    // UNBOXED SUM TYPE (unboxed-sumtype Stage 1 — LIVE): a Stage-1-eligible sum type's values are
    // physically a `SumNode*`. A param/temp whose static type is the sum type is read as a SumNode
    // (the call ABI: a sum-typed param receives a SumNode pointer; the caller materializes at every
    // Json/union/generic boundary — see `lower_coerce_arg` + `compile_ir_coerce`). Seeded BEFORE the
    // sealed-record arms because a sum type is a `Type::Union`, which those arms do not match anyway,
    // but kept first for clarity (the gate is mutually exclusive with sealed_fields/sealed_array_elem).
    if sum_type_eligible(ty) {
        return Repr::Packed(Layout::SumNode { sum_ty: ty.clone() });
    }
    if let Some(elem_fields) = sealed_array_elem(ty) {
        return Repr::Packed(Layout::PackedSealedArray {
            on_heap: elem_layout_on_heap(elem_fields),
            elem_layout: elem_fields.clone(),
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
    // UNBOXED SUM TYPE (unboxed-sumtype Stage 1 — LIVE): a MakeObject whose `ty` IS a Stage-1
    // eligible sum type and that has no spreads constructs a `SumNode` (codegen's MakeObject branch
    // reads this repr and calls `sumnode_construct`). A spread cannot be packed (unknown extra
    // fields) → fall through to the boxed object. Note: at a sum-construction site `ty` is the WHOLE
    // sum type (a `Type::Union`); the codegen branch resolves the variant from the discriminant
    // literal field's StrLit value.
    if sum_type_eligible(ty) && spreads.is_empty() {
        return Repr::Packed(Layout::SumNode { sum_ty: ty.clone() });
    }
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
            on_heap: elem_layout_on_heap(elem_fields),
            elem_layout: elem_fields.clone(),
        });
    }
    Repr::boxed_opaque()
}

/// True iff a packed-element layout has ANY heap field (String / Array / nested-sealed) — i.e. an
/// element drop must release per-field owned pointers (`release_sealed_array_elems`), not just free
/// the contiguous scalar buffer. Recorded on `Layout::PackedSealedArray` so two layouts with the same
/// field map but a different heap-ness (which cannot actually occur for a given field map, but the
/// flag must stay a deterministic function of `elem_layout` so the lattice join never spuriously
/// demotes) compare equal. Mirrors `Codegen::sealed_field_kind` heap-ness.
fn elem_layout_on_heap(fields: &IndexMap<String, Type>) -> bool {
    fields.values().any(is_sealed_heap_field)
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
            if sum_type_eligible(result_ty) {
                // UNBOXED SUM TYPE (Stage 3): an `obj[k]` / `arr[i]` whose RESULT is a sum type is
                // PROJECTED back into a fresh `*SumNode` by codegen — both the array arm
                // (`sumnode_project_from_boxed`, data.rs:422) and the object/Json arm
                // (`unbox_tagged_val_to_type`'s sum arm, boxing.rs:474) materialize a packed node.
                // So the dst's repr is `Packed(SumNode)`, NOT the Boxed default the missing arm left
                // it at. Without this seed the dst folds to Boxed, and the IR lowering's union-result
                // `CloneBox` (lower.rs:3514) emits `lin_tagged_clone` on the raw `*SumNode` — reading
                // offsets 0/8 (refcount/desc) as a TaggedVal tag/payload → heap-buffer-overflow on the
                // later `lin_sumnode_release`. With the seed, the CloneBox codegen's SumNode guard
                // (mod.rs:925) instead bumps the node's refcount via `lin_rc_retain`. Twin of the
                // FieldGet sum arm below (the recursive-child read). The verify Index ASSUME site only
                // constrains the OBJECT operand, never the dst, so this seed cannot trip the oracle.
                set(seeds, *dst, Repr::Packed(Layout::SumNode { sum_ty: result_ty.clone() }));
            } else if let Some(f) = sealed_fields(result_ty) {
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
            } else if sum_type_eligible(result_ty) {
                // UNBOXED SUM TYPE (unboxed-sumtype Stage 2): a RECURSIVE child read yields a borrowed
                // interior `*SumNode` whose repr is the child sum type's SumNode layout — so a chained
                // `evalNode(node["left"])` reads the child as Packed(SumNode) and re-enters the switch.
                set(seeds, *dst, Repr::Packed(Layout::SumNode { sum_ty: result_ty.clone() }));
            } else if let Some(f) = sealed_fields(result_ty) {
                set(seeds, *dst, Repr::Packed(Layout::PackedStruct { fields: f.clone() }));
            }
        }
        Instruction::SealedArrayFieldGet { dst, result_ty, .. } => {
            if let Some(s) = ScalarTy::from_type(result_ty) {
                set(seeds, *dst, Repr::FlatScalar(s));
            }
        }
        // A single field read of a BOXED `Object[]` element: the result repr is whatever its static
        // `result_ty` implies (scalar field → FlatScalar; a String/Array field → Boxed(Opaque); a
        // nested sealed record → PackedStruct). Seed it from the type exactly like any FieldGet.
        Instruction::BoxedArrayFieldGet { dst, result_ty, .. } => {
            set(seeds, *dst, type_seed(result_ty));
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
        // Keep-packed coercions: BoxKeepPacked produces a Boxed(WrapsPacked(L)) handle; the inner
        // layout comes from the source's type. UnboxKeepPacked produces a Packed(L) temp.
        Instruction::BoxKeepPacked { dst, src, .. } => {
            let inner = func.temp_types.get(src).cloned().unwrap_or(Type::Null);
            let layout = match type_seed(&inner) {
                Repr::Packed(l) => Some(l),
                _ => None,
            };
            set(seeds, *dst, match layout {
                Some(l) => Repr::Boxed(Inner::WrapsPacked(l)),
                None => Repr::boxed_opaque(),
            });
        }
        Instruction::UnboxKeepPacked { dst, ty, .. } => {
            set(seeds, *dst, type_seed(ty));
        }
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

/// Is `repr` a SumNode for sum type `sum_ty`?
fn is_sumnode(repr: &Repr, sum_ty: &Type) -> bool {
    matches!(repr, Repr::Packed(Layout::SumNode { sum_ty: s }) if s == sum_ty)
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
                        Repr::Packed(Layout::SumNode { sum_ty }) => {
                            if !is_sumnode(r, sum_ty) {
                                report(&mut bad, "MakeObject(sumnode)", *dst, "Packed(SumNode)", r);
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
                // ASSUME: FieldSet — object is written as a packed struct iff sealed_scalar_fields
                // (mirrors FieldGet, both directions).
                Instruction::FieldSet { object, obj_ty, .. } => {
                    let r = &repr[object.0 as usize];
                    match sealed_fields(obj_ty) {
                        Some(f) => {
                            if !is_packed_struct(r, f) {
                                report(&mut bad, "FieldSet(object packed)", *object, "Packed(struct)", r);
                            }
                        }
                        None => {
                            if r.packed_struct_fields().is_some() {
                                report(&mut bad, "FieldSet(object NOT predicate-packed)", *object, "non-Packed-struct", r);
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
                // ASSUME: BoxedArrayFieldGet — the array is the BOXED `Object[]` representation, so it
                // must NOT carry a packed sealed-array repr (the lowerer only emits this when the
                // element record is boxed, i.e. `sealed_array_elem` returns None).
                Instruction::BoxedArrayFieldGet { array, .. } => {
                    let r = &repr[array.0 as usize];
                    if r.packed_sealed_array_layout().is_some() {
                        report(&mut bad, "BoxedArrayFieldGet(array NOT predicate-packed)", *array, "non-Packed-array", r);
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
/// LOAD-BEARING. Codegen reads `func.repr[operand]` (not the static `Type`) at every repr-consuming
/// site checked here to decide the packed vs boxed load/store/free/push. A violation (an operand an
/// opcode reads as packed whose `func.repr` is NOT the matching Packed layout) means codegen would
/// emit a packed access against a value that is not physically packed: a silent representation
/// mismatch / UAF. The `debug_assert!` in [`run`] turns that into a compile-time panic. This is the
/// formal statement of the design's "a mismatch is inexpressible" invariant.
///
/// COVERED OPCODES (every repr-consuming site): the READ assume sites `FieldGet` /
/// `SealedArrayFieldGet` / `Index` (packed constant-offset load), plus the WRITE/CONSUME sites
/// `Push` (the array operand + pushed element — the exact opcode the producer/consumer DRIFT bug
/// flowed through: a boxed array pushed where codegen reads packed → garbage stride → crash; its
/// element arrives as a standalone Packed struct from the monomorphized `push$T` body, so it IS
/// asserted) and the sealed-array `IndexSet` (array operand). The IndexSet RHS *value* and a
/// map/object store decide storage from the CONTAINER and COERCE the value at the slot
/// (`sealed_project_from` projects a boxed `arr[i] = { … }` literal in), so they carry their own
/// (often Boxed) repr and are NOT asserted. Returns the violations (empty == sound).
pub fn verify(func: &LinFunction, repr: &[Repr]) -> Vec<String> {
    let mut bad = Vec::new();
    let fname = func.name.clone().unwrap_or_else(|| format!("fn#{}", func.id.0));
    for block in &func.blocks {
        for instr in &block.instructions {
            match instr {
                Instruction::FieldGet { object, obj_ty, .. } => {
                    if sum_type_eligible(obj_ty) {
                        if !is_sumnode(&repr[object.0 as usize], obj_ty) {
                            bad.push(format!(
                                "{fname}: FieldGet requires Packed(SumNode) of t{}, has {:?}",
                                object.0, repr[object.0 as usize]
                            ));
                        }
                    } else if let Some(f) = sealed_fields(obj_ty) {
                        if !is_packed_struct(&repr[object.0 as usize], f) {
                            bad.push(format!(
                                "{fname}: FieldGet requires Packed(struct) of t{}, has {:?}",
                                object.0, repr[object.0 as usize]
                            ));
                        }
                    }
                }
                // FieldSet has the SAME object-repr obligation as FieldGet: the literal-key sealed
                // field write is only emitted for a sealed-scalar record, so the object operand must
                // carry the matching Packed(struct) repr.
                Instruction::FieldSet { object, obj_ty, .. } => {
                    if let Some(f) = sealed_fields(obj_ty) {
                        if !is_packed_struct(&repr[object.0 as usize], f) {
                            bad.push(format!(
                                "{fname}: FieldSet requires Packed(struct) of t{}, has {:?}",
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
                // PUSH — the opcode the producer/consumer drift bug flowed through (a BOXED array
                // pushed at a site that codegen reads as PACKED → garbage `elem_stride` → crash). The
                // codegen `Intrinsic::Push` reads `arg_reprs[0].packed_sealed_array_layout()` to choose
                // the packed `lin_sealed_array_push_struct_retaining` fast path, so when the array
                // operand's TYPE is a packed sealed array its repr MUST be the matching Packed(sealed
                // array) — else the packed push writes a struct into a buffer that is not physically
                // packed. The pushed ELEMENT must then be the matching Packed(struct). This arm is the
                // structural proof the drift class is now inexpressible (a debug panic, not a UAF).
                Instruction::CallIntrinsic { intrinsic: Intrinsic::Push, args, .. } if args.len() >= 2 => {
                    let arr = args[0];
                    let arr_ty = func.temp_types.get(&arr).cloned().unwrap_or(Type::Null);
                    if let Some(ef) = sealed_array_elem(&arr_ty) {
                        if !is_packed_sealed_array(&repr[arr.0 as usize], ef) {
                            bad.push(format!(
                                "{fname}: Push requires Packed(sealed array) of t{}, has {:?}",
                                arr.0, repr[arr.0 as usize]
                            ));
                        }
                        let elem = args[1];
                        if !is_packed_struct(&repr[elem.0 as usize], ef) {
                            bad.push(format!(
                                "{fname}: Push element requires Packed(struct) of t{}, has {:?}",
                                elem.0, repr[elem.0 as usize]
                            ));
                        }
                    }
                }
                // INDEXSET — `arr[i] = v` over a packed sealed array. Codegen's sealed-array set path
                // (`emit_sealed_array_set`, dispatched on the ARRAY operand's repr) reads the array as
                // a packed buffer, so the array operand MUST be Packed(sealed array) when its type is
                // one — the assertion that the destination buffer codegen writes packed bytes into is
                // physically packed. The RHS VALUE is NOT asserted: like the map/object store,
                // `emit_sealed_array_set` COERCES the value into the slot (`sealed_project_from`
                // projects a boxed/Json RHS into the packed element), so the value operand legitimately
                // carries its own (often Boxed) repr — `arr[i] = { … }` builds the literal via the
                // boxed object path and projects it at the store (confirmed by the
                // `sealed_array_index_set_in_callee` corpus test, whose RHS repr is Boxed).
                Instruction::IndexSet { object, obj_ty, .. } => {
                    if let Some(ef) = sealed_array_elem(obj_ty) {
                        if !is_packed_sealed_array(&repr[object.0 as usize], ef) {
                            bad.push(format!(
                                "{fname}: IndexSet requires Packed(sealed array) of t{}, has {:?}",
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
                instr_spans: Vec::new(),
            }],
            temp_types,
            temp_count,
            intrinsic_slots: HashMap::new(),
            repr: Vec::new(),
            coverage_origin: None,
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

    fn shape_union() -> Type {
        // type Shape = { kind: "circle", r: Int32 } | { kind: "square", side: Int32 }
        let mut circle = IndexMap::new();
        circle.insert("kind".into(), Type::StrLit("circle".into()));
        circle.insert("r".into(), Type::Int32);
        let mut square = IndexMap::new();
        square.insert("kind".into(), Type::StrLit("square".into()));
        square.insert("side".into(), Type::Int32);
        Type::Union(vec![
            Type::Object { fields: circle, sealed: true },
            Type::Object { fields: square, sealed: true },
        ])
    }

    #[test]
    fn sum_type_gate_accepts_scalar_variants() {
        assert!(super::sum_type_eligible(&shape_union()));
        assert_eq!(super::sum_type_discriminant(&shape_union()).as_deref(), Some("kind"));
    }

    #[test]
    fn sum_type_gate_rejects_heap_field_variant() {
        // A variant with a String (non-scalar) field is out of Stage-1 scope → fall back to boxed.
        let mut a = IndexMap::new();
        a.insert("kind".into(), Type::StrLit("a".into()));
        a.insert("name".into(), Type::Str);
        let mut b = IndexMap::new();
        b.insert("kind".into(), Type::StrLit("b".into()));
        b.insert("n".into(), Type::Int32);
        let u = Type::Union(vec![
            Type::Object { fields: a, sealed: true },
            Type::Object { fields: b, sealed: true },
        ]);
        assert!(!super::sum_type_eligible(&u), "heap-field variant must be boxed");
    }

    #[test]
    fn sum_type_gate_rejects_no_distinct_discriminant() {
        // No shared distinct StrLit key → boxed.
        let mut a = IndexMap::new();
        a.insert("x".into(), Type::Int32);
        let mut b = IndexMap::new();
        b.insert("y".into(), Type::Int32);
        let u = Type::Union(vec![
            Type::Object { fields: a, sealed: true },
            Type::Object { fields: b, sealed: true },
        ]);
        assert!(!super::sum_type_eligible(&u));
    }

    /// The canonical recursive `Ast` union as it reaches the gate (the checker leaves the recursive
    /// child as `Named("Ast")`): `Num | BinOp` with `BinOp.left/right : Named("Ast")`.
    fn ast_union() -> Type {
        let mut num = IndexMap::new();
        num.insert("kind".into(), Type::StrLit("num".into()));
        num.insert("value".into(), Type::Int32);
        let mut binop = IndexMap::new();
        binop.insert("kind".into(), Type::StrLit("op".into()));
        binop.insert("left".into(), Type::Named("Ast".into()));
        binop.insert("right".into(), Type::Named("Ast".into()));
        Type::Union(vec![
            Type::Object { fields: num, sealed: true },
            Type::Object { fields: binop, sealed: true },
        ])
    }

    #[test]
    fn sum_type_gate_accepts_recursive_self_child() {
        // Stage 2: a self-recursive sum type (recursive `Named` children) is eligible, with self-name
        // `Ast` detected env-free as the unique `Named` appearing in a variant field.
        let ast = ast_union();
        assert!(super::sum_type_eligible(&ast), "recursive sum type must be eligible");
        assert_eq!(super::sum_type_discriminant(&ast).as_deref(), Some("kind"));
        assert_eq!(super::sum_recursive_self_name(&ast).as_deref(), Some("Ast"));
    }

    #[test]
    fn sum_type_gate_rejects_two_distinct_named_children() {
        // Mutual recursion / a foreign Named alongside the self-reference → >1 distinct Named name →
        // `sum_recursive_self_name` is None → those Named fields fail the scalar check → boxed.
        let mut a = IndexMap::new();
        a.insert("kind".into(), Type::StrLit("a".into()));
        a.insert("next".into(), Type::Named("A".into()));
        let mut b = IndexMap::new();
        b.insert("kind".into(), Type::StrLit("b".into()));
        b.insert("other".into(), Type::Named("B".into())); // a SECOND distinct Named
        let u = Type::Union(vec![
            Type::Object { fields: a, sealed: true },
            Type::Object { fields: b, sealed: true },
        ]);
        assert!(super::sum_recursive_self_name(&u).is_none(), "two distinct Names → no self-name");
        assert!(!super::sum_type_eligible(&u), "ambiguous recursion must be boxed (fail-safe)");
    }

    #[test]
    fn sum_type_recursive_param_is_sumnode() {
        // A recursive sum-typed PARAM seeds Packed(SumNode); the recursive-child FieldGet result
        // (typed as the child sum type) also seeds Packed(SumNode) so the recursion re-enters the
        // SumNode path. (Layout key is the whole canonical recursive union.)
        let ast = ast_union();
        let instrs = vec![Instruction::FieldGet {
            dst: Temp(1),
            object: Temp(0),
            field: "left".into(),
            obj_ty: ast.clone(),
            result_ty: ast.clone(),
        }];
        let f = func_of(instrs, Some(Temp(1)), 2, vec![(Temp(0), ast.clone())]);
        let repr = analyze(&f);
        assert!(
            matches!(&repr[0], Repr::Packed(Layout::SumNode { sum_ty }) if sum_ty == &ast),
            "recursive sum param must be Packed(SumNode), got {:?}", repr[0]
        );
        assert!(
            matches!(&repr[1], Repr::Packed(Layout::SumNode { sum_ty }) if sum_ty == &ast),
            "recursive child FieldGet result must be Packed(SumNode), got {:?}", repr[1]
        );
    }

    #[test]
    fn sum_type_seed_live_packs_construction() {
        // STAGE 1 (LIVE): the SumNode seed is enabled and the call ABI wired. A sum-type construction
        // (a sealed-variant literal of an eligible sum type, no spread) is labelled `Packed(SumNode)`
        // so codegen emits `sumnode_construct`. The gate itself is proven by
        // `sum_type_gate_accepts_scalar_variants`.
        let shape = shape_union();
        let instrs = vec![
            Instruction::Const { dst: Temp(0), val: Const::Int(5, Type::Int32) },
            Instruction::Const { dst: Temp(1), val: Const::Str("circle".into()) },
            Instruction::MakeObject {
                dst: Temp(2),
                fields: vec![("kind".into(), Temp(1)), ("r".into(), Temp(0))],
                spreads: vec![],
                ty: shape.clone(),
                stack: false,
            },
        ];
        let f = func_of(instrs, Some(Temp(2)), 3, vec![]);
        let repr = analyze(&f);
        assert!(
            matches!(&repr[2], Repr::Packed(Layout::SumNode { sum_ty }) if sum_ty == &shape),
            "seed live: sum literal must be Packed(SumNode), got {:?}",
            repr[2]
        );
        // Oracle + verify must hold with the live seed.
        assert!(oracle_check(&f, &repr).is_empty());
        assert!(verify(&f, &repr).is_empty());
    }

    #[test]
    fn sum_type_param_is_sumnode_and_fieldget_verifies() {
        // A sum-typed PARAM is seeded Packed(SumNode) (the callee reads it as a SumNode pointer — the
        // call ABI). A FieldGet on it (after narrowing, obj_ty is the sum type) requires SumNode of
        // the same type, which verify proves.
        let shape = shape_union();
        let instrs = vec![Instruction::FieldGet {
            dst: Temp(1),
            object: Temp(0),
            field: "r".into(),
            obj_ty: shape.clone(),
            result_ty: Type::Int32,
        }];
        let f = func_of(instrs, Some(Temp(1)), 2, vec![(Temp(0), shape.clone())]);
        let repr = analyze(&f);
        assert!(
            matches!(&repr[0], Repr::Packed(Layout::SumNode { sum_ty }) if sum_ty == &shape),
            "sum param must be Packed(SumNode), got {:?}",
            repr[0]
        );
        assert!(verify(&f, &repr).is_empty(), "FieldGet on a SumNode param must verify");
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

    #[test]
    fn verify_catches_push_repr_drift() {
        // THE structural proof the producer/consumer DRIFT class is now a debug panic, not a silent
        // UAF: a `Push` whose array operand's TYPE is a packed sealed array but whose actual repr is
        // Boxed (the calc-lexer `scan(.., [])` shape — a boxed `[]` flowing into a packed `T[]` param)
        // is flagged by `verify`. Construct that exact mismatch by HAND (a param typed `Pt[]` but
        // seeded Boxed) and assert verify reports it. Before the verify extension this site was a
        // blind spot — the bug was only catchable under ASan at runtime.
        let pt = sealed(pt_fields());
        let arr_ty = Type::Array(Box::new(pt.clone()));
        let instrs = vec![
            // t1 = a fresh packed Pt element to push.
            Instruction::Const { dst: Temp(1), val: Const::Int(1, Type::Int32) },
            Instruction::Const { dst: Temp(2), val: Const::Int(2, Type::Int32) },
            Instruction::MakeObject {
                dst: Temp(3),
                fields: vec![("x".into(), Temp(1)), ("y".into(), Temp(2))],
                spreads: vec![],
                ty: pt.clone(),
                stack: false,
            },
            Instruction::CallIntrinsic {
                dst: Temp(4),
                intrinsic: Intrinsic::Push,
                args: vec![Temp(0), Temp(3)],
                ret_ty: Type::Null,
            },
        ];
        let f = func_of(instrs, None, 5, vec![(Temp(0), arr_ty.clone())]);
        // Hand-build a repr table where the ARRAY operand t0 is wrongly Boxed (the drift) while the
        // pushed element t3 is correctly Packed.
        let mut repr = analyze(&f);
        repr[0] = Repr::boxed_opaque();
        let violations = verify(&f, &repr);
        assert!(
            violations.iter().any(|v| v.contains("Push requires Packed(sealed array) of t0")),
            "verify must flag the boxed-array Push drift, got: {violations:?}"
        );
        // And the well-typed analysis result (t0 seeded Packed from its param type) is clean.
        let clean = analyze(&f);
        assert!(verify(&f, &clean).is_empty(), "well-typed Push must verify clean");
    }
}
