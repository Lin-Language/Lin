//! Phase 0 monomorphization of single-module generic functions.
//!
//! A generic function (`val identity = <T>(x: T): T => x`) is type-checked once with its type
//! parameters represented as quantified `TypeVar` ids in the ≥9001 range (see `lin-check`'s
//! `forward_declare_functions` / `bind_type_params`). Those ids are deliberately NOT solved
//! globally, so the generic function's body still mentions `TypeVar(9001)` — which `lin-codegen`
//! would compile to the boxed/opaque-pointer ABI. Each *call site*, however, already carries a
//! concrete `result_type` (the checker instantiated the scheme locally via `apply_type_subs`).
//!
//! This pass closes the gap by materializing a concrete copy of each generic function per distinct
//! instantiation, substituting the quantified `TypeVar`s with the concrete types inferred at the
//! call site, naming it `name$<mangled-args>`, and routing the call to it. Because the specialized
//! body is fully concrete (e.g. `(x: Int32): Int32`), the existing codegen emits native scalars —
//! no `lin_box_int32`/`lin_unbox_int32` around the identity call.
//!
//! Scope: top-level generic `val` functions called *directly* by name (`identity(5)`), whether
//! defined in THIS module or in an IMPORTED one. A call to a generic imported from another module
//! (`monomorphize_with_imports`) is specialized HERE: the imported body is cloned, type-substituted,
//! its free references re-homed into the importer (sibling calls → `Named` exports of the origin
//! module, intrinsics → merged intrinsic slots, thin intrinsic wrappers inlined to the intrinsic),
//! and emitted as a local specialization. Imported modules also monomorphize their OWN sibling
//! generic calls AND their own cross-module generic calls during `lower_import_module`
//! (`monomorphize_import_with_imports`, which keeps all originals for external importers). Passing a
//! generic as a first-class value, and generic methods, remain
//! deferred. When a module uses no generic function (the common case) this pass is a no-op and
//! leaves the module byte-identical (the no-op invariant — see `module_uses_generic`).

use std::collections::HashMap;

use lin_check::typed_ir::*;
use lin_check::types::Type;
use lin_common::Diagnostic;

/// Maximum number of distinct *native* (unboxed) specializations minted per generic function.
/// Beyond this, further distinct instantiations fall back to a single shared boxed/type-erased
/// copy (correct, just not unboxed) so pathological programs can't blow up code size. A
/// diagnostic is emitted on first overflow so the fallback is never silent. Picked generously:
/// real programs instantiate a generic at a handful of types.
///
/// Overridable via the `LIN_SPEC_BUDGET` env var (used by tests, where minting 50+ distinct
/// concrete instantiations of one generic is otherwise impractical given the small type universe).
const SPECIALIZATION_BUDGET_DEFAULT: usize = 50;

fn specialization_budget() -> usize {
    std::env::var("LIN_SPEC_BUDGET")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(SPECIALIZATION_BUDGET_DEFAULT)
}

/// Lowest id used for a quantified generic type parameter (mirrors `lin-check`'s
/// `next_generic_tv` base; 9000 itself is the intrinsic array/iterator slot).
const GENERIC_TV_BASE: u32 = 9001;

/// True if `ty` mentions any quantified generic TypeVar (≥ `GENERIC_TV_BASE`, excluding the
/// `u32::MAX` Json wildcard). Such a type is unresolved-polymorphic and must be specialized.
fn mentions_generic_tv(ty: &Type) -> bool {
    match ty {
        Type::TypeVar(id) => *id >= GENERIC_TV_BASE && *id != u32::MAX,
        Type::Array(t) | Type::Iterator(t) | Type::Shared(t) | Type::Stream(t) | Type::Map(t) => mentions_generic_tv(t),
        Type::FixedArray(ts) | Type::Union(ts) => ts.iter().any(mentions_generic_tv),
        Type::Object { fields, .. } => fields.values().any(mentions_generic_tv),
        Type::Function { params, ret, .. } => {
            params.iter().any(mentions_generic_tv) || mentions_generic_tv(ret)
        }
        _ => false,
    }
}

/// A field type permitted in a PACKED sealed record element — delegates to the SINGLE source of
/// truth `Type::is_sealed_array_field_packable` (ADR-063 gate consolidation), in lockstep with
/// `Codegen::sealed_array_elem_field_packable`, lower.rs `is_sealed_array_elem_field_packable`, and
/// `repr::sealed_array_elem_field_packable`. A combinator over a packed sealed element must take the
/// boxed-fallback detour, else the native specialization reads packed bytes through boxed machinery.
fn field_packed_scalar(ty: &Type) -> bool {
    ty.is_sealed_array_field_packable()
}

/// True if `ty` is (or contains, transitively) a PACKED SEALED record/array — the representation
/// codegen lays out as a contiguous unboxed buffer (elem_tag 0xFE) / packed struct (not a
/// `LinObject`). The unsound generic combinators (see `combinator_unsound_over_sealed`) read such
/// elements through the boxed `Object[]`/`Json` machinery, a boxed-vs-packed mismatch. When a
/// combinator substitution binds a type parameter to such a type we route the call through the
/// type-erased `boxed_fallback_call`, whose boxed ABI materializes the sealed value to its boxed view
/// at the argument boundary (`box_value` → `sealed_array_to_tagged`) and re-coerces the boxed result
/// back to the sealed type. Heap-field sealed records stay boxed (Stage 3a gate), so they flow
/// through the generic combinator's boxed body correctly and do NOT need the detour.
fn mentions_sealed(ty: &Type) -> bool {
    match ty {
        // A sealed record whose fields are ALL scalar (numeric/Bool) is the PACKED representation
        // codegen lays out as a contiguous struct / unboxed array (elem_tag 0xFE) — the boxed-vs-packed
        // mismatch source for the unsound combinators. MUST mirror the codegen packed gate
        // (`Codegen::sealed_array_elem` / lower.rs `is_sealed_array_elem_field_packable`) via the
        // shared `field_packed_scalar` predicate (scalar-field sealed records only).
        Type::Object { fields, sealed: true } =>
            !fields.is_empty() && fields.values().all(field_packed_scalar),
        Type::Object { fields, sealed: false } => fields.values().any(mentions_sealed),
        Type::Array(t) | Type::Iterator(t) | Type::Shared(t) | Type::Stream(t) | Type::Map(t) => mentions_sealed(t),
        Type::FixedArray(ts) | Type::Union(ts) => ts.iter().any(mentions_sealed),
        Type::Function { params, ret, .. } => {
            params.iter().any(mentions_sealed) || mentions_sealed(ret)
        }
        _ => false,
    }
}

/// Generic stdlib combinators that are UNSOUND when native-specialized at a packed sealed element
/// type and must be routed through the type-erased boxed fallback (§3 materialize-to-boxed). These
/// build a `T`-typed buffer (`arrayAllocateFilled`) or store/return whole `T` elements that their
/// body then reads/writes through the boxed `Object[]`/`Json` machinery — a boxed-vs-packed mismatch.
/// Determined empirically against the native-spec output over a `type Pt = {…}` array on master
/// (`sort`→`7 7 7 7`, `sortBy`→segfault, and `minBy`/`maxBy`/`partition`/`reverse`/`unique`/`take`/
/// `drop`/`filter` returned garbage element fields). All return a `T`-containing type, so the boxed
/// result re-coerces back to the sealed representation via the sealed-array / nested-array Coerce arms.
/// (`slice`/`chunk` happened to read correctly on master but ALSO return whole `T` and are included
/// for safety — the boxed path is correct, just unbenchmarked-faster.) Projection-style combinators
/// (`map`/`reduce`/`scan`/`find`/`some`/`every`/`flatMap`/`groupBy`/`countBy`/`indexOf`/`zip`) are NOT
/// listed: they read the packed element soundly natively and/or return a non-`T` result the boxed
/// re-coerce cannot reconstruct.
fn combinator_unsound_over_sealed(name: &str) -> bool {
    matches!(name,
        "sort" | "sortBy" | "minBy" | "maxBy" | "partition"
        | "reverse" | "unique" | "take" | "drop" | "filter"
        | "slice" | "chunk" | "zip")
}

/// Generic stdlib functions that MUTATE their receiver (arg0) IN PLACE through the runtime array it
/// points at — `push` (`lin_push` → `lin_array_push_tagged` / `lin_push_dyn`) and `set`
/// (`arr[idx] = item` → `lin_array_set`). For these the receiver value is the ACTUAL array the
/// caller observes afterwards; the mutation must hit THAT array, not a fresh copy. This matters when
/// the receiver is a container-stored array read (`obj[k]` / `m[k]`): its runtime representation is a
/// BOXED tagged array, but a packed-sealed (`push$Obj_…`) specialization would coerce/MATERIALIZE the
/// receiver to a fresh detached packed buffer (`sealed_array_project_from` →`lin_sealed_array_alloc`),
/// so the push lands in the copy and is silently lost (the stored array stays empty). See
/// `receiver_mutator_over_boxed_indexed_array`. Projection-only combinators (`map`/`filter`/…) build
/// a NEW result and never write through arg0, so they are NOT listed.
fn is_receiver_inplace_mutator(name: &str) -> bool {
    matches!(name, "push" | "set")
}

/// A non-packed (boxed/Json/union) array representation: the runtime value is a tagged `Object[]`/
/// dynamic array, NOT a contiguous packed-sealed buffer. Used to detect when an in-place-mutator
/// receiver would be MATERIALIZED (detached) by a packed-sealed specialization — `obj[k]` reads a
/// `Json`/union value whose stored array must be mutated through the boxed path instead.
fn is_packed_sealed_array(ty: &Type) -> bool {
    match ty {
        Type::Array(elem) => matches!(elem.as_ref(),
            Type::Object { fields, sealed: true }
                if !fields.is_empty() && fields.values().all(field_packed_scalar)),
        _ => false,
    }
}

/// True when this call is a RECEIVER-mutating in-place op (`push`/`set`) whose receiver (arg0) is a
/// CONTAINER INDEX READ (`obj[k]` / `m[k]`) that yields a BOXED tagged array (its type is NOT itself a
/// packed-sealed array). Coercing such a receiver into a packed-sealed param would MATERIALIZE a fresh
/// detached buffer and lose the mutation (the silent data-loss bug). When this fires, every packable-
/// sealed binding is rebound to the Json wildcard so the call specializes at the boxed `$Json`
/// representation (`lin_push_dyn` / `lin_array_set` mutating the REAL stored array) — the same path
/// that already makes the direct-`Json`-receiver case correct.
fn receiver_mutator_over_boxed_indexed_array(name: &str, args: &[TypedExpr]) -> bool {
    if !is_receiver_inplace_mutator(name) {
        return false;
    }
    match args.first() {
        // The receiver must be a container index read. A plain local/param binding (`var arr: Pt[]`)
        // routes its own packed array correctly and MUST keep the packed fast path.
        Some(TypedExpr::Index { result_type, .. }) => !is_packed_sealed_array(result_type),
        _ => false,
    }
}

/// A top-level generic function discovered in the module (or in an import).
struct GenericFn {
    name: String,
    /// The full `Function` TypedExpr (params/body/ret_type/captures/span).
    func: TypedExpr,
    /// For a generic imported from another module, the module path it came from. `None` for a
    /// generic defined in the module being lowered. Cross-module specializations clone the
    /// imported body into THIS module, but the body's free references (calls to the imported
    /// module's own siblings/intrinsics/imports) must be rewritten to resolve in the importer —
    /// see `rehome_imported_body`.
    origin: Option<String>,
}

/// Substitute quantified TypeVars throughout a type.
fn subst_type(ty: &Type, subs: &HashMap<u32, Type>) -> Type {
    match ty {
        Type::TypeVar(id) => subs.get(id).cloned().unwrap_or_else(|| ty.clone()),
        Type::Array(t) => Type::Array(Box::new(subst_type(t, subs))),
        Type::Iterator(t) => Type::Iterator(Box::new(subst_type(t, subs))),
        Type::Shared(t) => Type::Shared(Box::new(subst_type(t, subs))),
        Type::Stream(t) => Type::Stream(Box::new(subst_type(t, subs))),
        Type::Map(t) => Type::Map(Box::new(subst_type(t, subs))),
        Type::FixedArray(ts) => Type::FixedArray(ts.iter().map(|t| subst_type(t, subs)).collect()),
        Type::Union(ts) => {
            // Substituting a union's members can produce DUPLICATES or collapse to a single type.
            // The canonical case is `<T, D>(…): T | D` instantiated with `T = D` (e.g. `at(ints, i,
            // 0)` over `Int32[]` ⇒ both members `Int32`): naive substitution yields the DEGENERATE
            // `Union([Int32, Int32])`. A 2-member union is a BOXED (`ptr`) representation, but a
            // single concrete `Int32` is an unboxed scalar — so a degenerate union makes the spec
            // return a boxed union while its callers (and arms) read an `i32`, an ABI mismatch
            // codegen rejects. Flatten: dedup members and collapse a singleton to the bare type, so
            // `T | D` with `T = D = Int32` becomes `Int32` (exactly what a hand-written `(…): Int32`
            // would produce). Mirrors `Type::flatten_union` in lin-check.
            let mut flat: Vec<Type> = Vec::new();
            for t in ts {
                let st = subst_type(t, subs);
                match st {
                    Type::Union(inner) => {
                        for m in inner {
                            if !flat.contains(&m) {
                                flat.push(m);
                            }
                        }
                    }
                    other => {
                        if !flat.contains(&other) {
                            flat.push(other);
                        }
                    }
                }
            }
            if flat.len() == 1 {
                flat.into_iter().next().unwrap()
            } else {
                Type::Union(flat)
            }
        }
        Type::Object { fields, sealed } => Type::Object {
            fields: fields.iter().map(|(k, v)| (k.clone(), subst_type(v, subs))).collect(),
            sealed: *sealed,
        },
        Type::Function { params, ret, required } => Type::Function {
            params: params.iter().map(|p| subst_type(p, subs)).collect(),
            ret: Box::new(subst_type(ret, subs)),
            required: *required,
        },
        _ => ty.clone(),
    }
}

/// Rewrite every LEFTOVER/UNSOLVED inference `TypeVar` mentioned in `ty` (an id `< GENERIC_TV_BASE`,
/// i.e. a fresh checker inference var that never got solved, e.g. `TypeVar(44)`) to the `u32::MAX`
/// Json wildcard. The existing Json wildcard (`u32::MAX`) is already a wildcard and is preserved.
///
/// A quantified generic param id (`>= GENERIC_TV_BASE`, `!= u32::MAX`) is deliberately LEFT
/// UNTOUCHED: a binding that still mentions one means the generic is genuinely unconstrained at
/// this call (e.g. `val mk = <T>(): T => 0; mk()`), which must keep producing the clean
/// "cannot infer a concrete type" diagnostic rather than silently erasing to Json.
///
/// Why erase the leftover inference vars: keying a specialization on a bare unsolved `TypeVar(44)`
/// mints a garbage `$T44` monomorph that reads/allocates the backing array at a bogus element type
/// (Gap 2 — runtime capacity overflow / heap corruption). Erasing to the Json wildcard yields a
/// tagged `$Json` monomorph whose element representation is the uniform tagged value — correct and
/// safe. A concrete type (Int32, String, Object, …) is left untouched, so a real `Int32[]` argument
/// still produces the flat `$Int32` specialization.
fn erase_nonconcrete_typevars(ty: &Type) -> Type {
    match ty {
        // Leftover/unsolved inference var (below the quantified-generic range): erase to Json.
        Type::TypeVar(id) if *id < GENERIC_TV_BASE => Type::TypeVar(u32::MAX),
        // Json wildcard, or a quantified generic param: leave as-is.
        Type::TypeVar(_) => ty.clone(),
        Type::Array(t) => Type::Array(Box::new(erase_nonconcrete_typevars(t))),
        Type::Iterator(t) => Type::Iterator(Box::new(erase_nonconcrete_typevars(t))),
        Type::Shared(t) => Type::Shared(Box::new(erase_nonconcrete_typevars(t))),
        Type::Stream(t) => Type::Stream(Box::new(erase_nonconcrete_typevars(t))),
        Type::Map(t) => Type::Map(Box::new(erase_nonconcrete_typevars(t))),
        Type::FixedArray(ts) => {
            Type::FixedArray(ts.iter().map(erase_nonconcrete_typevars).collect())
        }
        Type::Union(ts) => Type::Union(ts.iter().map(erase_nonconcrete_typevars).collect()),
        Type::Object { fields, sealed } => Type::Object {
            fields: fields.iter().map(|(k, v)| (k.clone(), erase_nonconcrete_typevars(v))).collect(),
            sealed: *sealed,
        },
        Type::Function { params, ret, required } => Type::Function {
            params: params.iter().map(erase_nonconcrete_typevars).collect(),
            ret: Box::new(erase_nonconcrete_typevars(ret)),
            required: *required,
        },
        _ => ty.clone(),
    }
}

/// Unify a generic `pattern` type against a concrete `actual` type, accumulating
/// `TypeVar id -> concrete` bindings. Only quantified ids (≥ base) are recorded.
fn collect_subs(pattern: &Type, actual: &Type, subs: &mut HashMap<u32, Type>) {
    match (pattern, actual) {
        (Type::TypeVar(id), t) if *id >= GENERIC_TV_BASE && *id != u32::MAX => {
            subs.entry(*id).or_insert_with(|| t.clone());
        }
        (Type::Array(p), Type::Array(a)) => collect_subs(p, a, subs),
        (Type::Array(p), Type::FixedArray(ats)) => {
            for a in ats { collect_subs(p, a, subs); }
        }
        // Positional tuple unification (`[String, T]` vs `[String, Int32]`) — the type parameter is
        // nested inside a fixed-array (tuple) shape, as in `fromEntries(pairs: [String, T][])`.
        // Mirrors the `FixedArray` arm in lin-check's `collect_type_subs`.
        (Type::FixedArray(ps), Type::FixedArray(ats)) => {
            for (p, a) in ps.iter().zip(ats.iter()) { collect_subs(p, a, subs); }
        }
        // A generic `T[]` (or `Iterator<T>`) param unified against a `Json` value (the MAX
        // wildcard) — e.g. a stdlib fn calling a sibling generic on its own `Json` param. Bind
        // the element TypeVar(s) to the Json wildcard so the specialization is keyed at the
        // tagged `$Json` representation rather than left unbound (Gap 1, mirrors lin-check's
        // `collect_type_subs`).
        (Type::Array(p), Type::TypeVar(id)) if *id == u32::MAX => {
            collect_subs(p, &Type::TypeVar(u32::MAX), subs)
        }
        (Type::Iterator(p), Type::TypeVar(id)) if *id == u32::MAX => {
            collect_subs(p, &Type::TypeVar(u32::MAX), subs)
        }
        // An `Iterable`-shaped generic param `T[]` is routinely applied to a runtime ITERATOR
        // (e.g. `range(0,n)` returns `Iterator<Int32>`, then `.map(…)` whose param is `arr: T[]`).
        // The element type is what a specialization keys on, so cross-unify the element through the
        // Array↔Iterator boundary — without this, `T` is left unbound and `map`/`filter`/`reduce`
        // over a `range(...)` would specialize at a fresh `TypeVar` (the boxed path) instead of Int32.
        (Type::Array(p), Type::Iterator(a)) => collect_subs(p, a, subs),
        (Type::Iterator(p), Type::Array(a)) => collect_subs(p, a, subs),
        (Type::Iterator(p), Type::FixedArray(ats)) => {
            for a in ats { collect_subs(p, a, subs); }
        }
        (Type::Iterator(p), Type::Iterator(a)) => collect_subs(p, a, subs),
        (Type::Shared(p), Type::Shared(a)) => collect_subs(p, a, subs),
        (Type::Stream(p), Type::Stream(a)) => collect_subs(p, a, subs),
        // An index-signature map param `{ String: T }` unified against a concrete `{ String: A }`
        // value (or the `Json` wildcard): recover the element TypeVar from the value type, exactly
        // like the `Array`/`Iterator` element cases above.
        (Type::Map(p), Type::Map(a)) => collect_subs(p, a, subs),
        (Type::Map(p), Type::TypeVar(id)) if *id == u32::MAX => {
            collect_subs(p, &Type::TypeVar(u32::MAX), subs)
        }
        (Type::Object { fields: pf, .. }, Type::Object { fields: af, .. }) => {
            for (k, pv) in pf {
                if let Some(av) = af.get(k) { collect_subs(pv, av, subs); }
            }
        }
        (Type::Function { params: pp, ret: pr, .. }, Type::Function { params: ap, ret: ar, .. }) => {
            for (p, a) in pp.iter().zip(ap.iter()) { collect_subs(p, a, subs); }
            collect_subs(pr, ar, subs);
        }
        // A generic union-typed param (e.g. `Res<T, E> = {..T} | {..E}`) unified against a
        // concrete union argument. The type checker resolves the call, but the monomorphizer must
        // also recover the type-args from the call site or it fails to specialize. Mirror the
        // object/array recursion by walking INTO the union members. Without this, a type parameter
        // that appears ONLY inside a union member is left unbound.
        //
        // Two distinct shapes share this arm:
        //   1. STRUCTURAL members (objects/arrays/…), e.g. `Res<T,E>` = two record arms keyed by a
        //      `"type"` discriminant — each pattern member must bind against the structurally
        //      CORRESPONDING actual member, not an arbitrary one (`best_union_match`).
        //   2. A bare quantified TypeVar member, e.g. `T | Null` — `T` should absorb the ENTIRE
        //      remaining actual union (`Int32 | String`), not a single member of it.
        (Type::Union(pts), Type::Union(ats)) => {
            for pt in pts {
                if matches!(pt, Type::TypeVar(id) if *id >= GENERIC_TV_BASE && *id != u32::MAX) {
                    // Case 2: bind the bare var to the union of actual members that no concrete
                    // (non-var) pattern member matches structurally — i.e. the "leftover" arms.
                    let leftover: Vec<Type> = ats
                        .iter()
                        .filter(|at| {
                            !pts.iter().any(|other| {
                                !std::ptr::eq(other, pt)
                                    && !matches!(other, Type::TypeVar(_))
                                    && union_shape_score(other, at) > 0
                            })
                        })
                        .cloned()
                        .collect();
                    let bound = match leftover.len() {
                        0 => Type::Union(ats.clone()),
                        1 => leftover.into_iter().next().unwrap(),
                        _ => Type::Union(leftover),
                    };
                    // Only record a binding when it adds CONCRETE information. If the leftover is
                    // (or still mentions) the same generic var — e.g. the return union `S | Error`
                    // was passed unsubstituted as the call-site `result_type`, so `S` would bind to
                    // itself — leave it unbound. A self-/generic-binding makes `fully_concrete`
                    // false and spuriously trips the "cannot infer" error for a param whose value
                    // is genuinely determined elsewhere (or legitimately erased to `$Json`).
                    if !mentions_generic_tv(&bound) {
                        collect_subs(pt, &bound, subs);
                    }
                } else if let Some(at) = best_union_match(pt, ats) {
                    // Case 1: structural pattern member → best-matching actual member.
                    collect_subs(pt, at, subs);
                }
            }
        }
        // A generic union param unified against a single concrete member (a narrowed value, or a
        // record literal that only inhabits one arm). Recurse against each pattern member; only the
        // structurally-matching one will bind anything, the rest are inert.
        (Type::Union(pts), actual) => {
            for pt in pts {
                collect_subs(pt, actual, subs);
            }
        }
        _ => {}
    }
}

/// Pick the actual union member that best matches a generic pattern member, so the type vars are
/// bound against the structurally-corresponding arm rather than an arbitrary one. Prefers an exact
/// `union_shape_score` winner (e.g. matching `StrLit` discriminants like `"type": "success"`).
fn best_union_match<'a>(pattern: &Type, actuals: &'a [Type]) -> Option<&'a Type> {
    actuals
        .iter()
        .max_by_key(|a| union_shape_score(pattern, a))
}

/// A heuristic structural-overlap score between a pattern member and a candidate actual member.
/// Higher means a better match. Counts object fields present in both, with a strong bonus when a
/// shared field's pattern type is a concrete `StrLit` that equals the actual's (the discriminant
/// case, e.g. `"type": "success"` vs `"type": "failure"`).
fn union_shape_score(pattern: &Type, actual: &Type) -> i64 {
    match (pattern, actual) {
        (Type::Object { fields: pf, .. }, Type::Object { fields: af, .. }) => {
            let mut score = 0i64;
            for (k, pv) in pf {
                if let Some(av) = af.get(k) {
                    score += 1;
                    match (pv, av) {
                        (Type::StrLit(a), Type::StrLit(b)) if a == b => score += 100,
                        (Type::StrLit(_), Type::StrLit(_)) => score -= 100,
                        _ => {}
                    }
                }
            }
            score
        }
        (Type::Array(p), Type::Array(a)) | (Type::Iterator(p), Type::Iterator(a)) => {
            union_shape_score(p, a)
        }
        _ => 0,
    }
}

/// Render a concrete type into a short, identifier-safe suffix for specialization names.
fn mangle_type(ty: &Type) -> String {
    match ty {
        Type::Null => "Null".into(),
        Type::Bool => "Bool".into(),
        Type::Int8 => "Int8".into(),
        Type::Int16 => "Int16".into(),
        Type::Int32 => "Int32".into(),
        Type::Int64 => "Int64".into(),
        Type::UInt8 => "UInt8".into(),
        Type::UInt16 => "UInt16".into(),
        Type::UInt32 => "UInt32".into(),
        Type::UInt64 => "UInt64".into(),
        Type::Float32 => "Float32".into(),
        Type::Float64 => "Float64".into(),
        Type::Str => "String".into(),
        Type::StrLit(_) => "String".into(),
        Type::Array(t) => format!("Arr_{}", mangle_type(t)),
        Type::Iterator(t) => format!("Iter_{}", mangle_type(t)),
        Type::Stream(t) => format!("Stream_{}", mangle_type(t)),
        // Object records must mangle by SHAPE, not collapse to a single `Object`. Two distinct
        // record types instantiated at the same generic param (e.g. `push(Route[], route)` and
        // `push(Leg[], leg)`) would otherwise both produce `push$Object` — a SYMBOL COLLISION:
        // the monomorphizer mints two distinct specializations (keyed by `instantiation_key`,
        // which distinguishes them) but assigns them the SAME name, so codegen emits two function
        // bodies under one symbol and the second is unreachable. A `push(Leg)` call then runs the
        // `push(Route)` body — for a record-array field (`legs: Leg[]`) the Route body reads the
        // Leg struct's scalar `d` field as an array pointer and crashes (misaligned-pointer deref).
        // Include each field's name and recursively-mangled type so structurally-distinct records
        // get distinct names. Field order is the declaration `IndexMap` order (canonical, ADR Stage
        // 0.5), so identical shapes still collapse to one specialization.
        Type::Object { fields, .. } => {
            let mut s = String::from("Obj");
            for (k, fty) in fields.iter() {
                s.push('_');
                s.push_str(k);
                s.push('_');
                s.push_str(&mangle_type(fty));
            }
            s
        }
        Type::Union(_) => "Union".into(),
        Type::Function { .. } => "Fn".into(),
        // The `u32::MAX` Json wildcard (an erased non-concrete type-arg) mangles to `Json`, so a
        // type-erased specialization is named `name$Json` rather than `name$T4294967295`.
        Type::TypeVar(id) if *id == u32::MAX => "Json".into(),
        Type::TypeVar(id) => format!("T{}", id),
        _ => "X".into(),
    }
}

/// Build the specialization symbol name, e.g. `identity$Int32`. The key combines the type-param
/// ids deterministically (sorted) so identical instantiations collapse to one specialization.
fn specialization_name(base: &str, subs: &HashMap<u32, Type>) -> String {
    let mut ids: Vec<u32> = subs.keys().copied().collect();
    ids.sort_unstable();
    let parts: Vec<String> = ids.iter().map(|id| mangle_type(&subs[id])).collect();
    format!("{}${}", base, parts.join("_"))
}

/// A canonical, hashable key for an instantiation (generic slot + sorted concrete args).
fn instantiation_key(slot: usize, subs: &HashMap<u32, Type>) -> (usize, Vec<(u32, String)>) {
    let mut entries: Vec<(u32, String)> =
        subs.iter().map(|(id, t)| (*id, format!("{:?}", t))).collect();
    entries.sort();
    (slot, entries)
}

/// Cheap pre-check: does the module declare any top-level generic function? Lets callers skip the
/// clone+rewrite entirely for ordinary modules (the overwhelming common case), keeping their
/// lowering byte-identical.
pub fn module_has_generic_fn(module: &TypedModule) -> bool {
    module.statements.iter().any(|stmt| {
        if let TypedStmt::Val { value: TypedExpr::Function { params, ret_type, .. }, .. } = stmt {
            params.iter().any(|p| mentions_generic_tv(&p.ty)) || mentions_generic_tv(ret_type)
        } else {
            false
        }
    })
}

/// Cheap pre-check: does this module either declare its own generic function OR import a generic
/// function from another module? When neither holds, monomorphization is skipped entirely and the
/// module lowers byte-for-byte as before (the no-op invariant). An imported binding is "generic" if
/// its declared type mentions a quantified TypeVar — the importing module's `ImportSlot.ty` carries
/// the origin module's generic signature.
pub fn module_uses_generic(module: &TypedModule, imports: &HashMap<String, TypedModule>) -> bool {
    if module_has_generic_fn(module) {
        return true;
    }
    module.statements.iter().any(|stmt| {
        if let TypedStmt::Import { path, bindings, .. } = stmt {
            if !imports.contains_key(path) {
                return false;
            }
            // Only a binding whose type is a function with a generic in its PARAMETERS counts —
            // mirrors the cross-module discovery rule, so intrinsic-wrapper exports whose only
            // TypeVar is in the return (e.g. `iter: (…) => Iterator<T>`) don't trip the pass.
            bindings.iter().any(|b| match &b.ty {
                Type::Function { params, .. } => params.iter().any(mentions_generic_tv),
                _ => false,
            })
        } else {
            false
        }
    })
}

/// Entry point: rewrite generic-function calls to monomorphized specializations.
/// Returns the diagnostics produced (errors for generic calls that cannot be instantiated);
/// the module is left unchanged when it contains no generic functions.
///
/// Three improvements over the original Phase-0 pass (single-module hardening):
///   - **Worklist/fixpoint (BUG 1):** materializing one specialization clones the generic body,
///     substitutes its quantified TypeVars with the concrete instantiation, then re-runs the call
///     rewriter *over that body*. A nested call to another generic (`wrap`→`id`) is therefore
///     re-monomorphized under the composed substitution, routing to the native `id$Int32` instead
///     of leaving a half-generic `id$T9002` copy. New specs minted while materializing are pushed
///     back onto the worklist and processed until it drains.
///   - **Alias propagation + boxed fallback (BUG 2):** a generic bound to another `val`
///     (`val f = id`) is tracked as an alias, so an indirect call `f(5)` monomorphizes to
///     `id$Int32` exactly like a direct call. Any generic call that still can't be turned into a
///     native specialization (a generic used as a first-class value that escapes, or a budget
///     overflow) routes through a *boxed/type-erased* call to the kept generic original: the call
///     boxes its args (TypeVar params ⇒ uniform boxed ptr ABI) and the result is unboxed back to
///     the concrete type via a wrapping `Coerce`. Correct, just not unboxed — and never a panic.
///   - **Budget (`SPECIALIZATION_BUDGET`):** caps distinct native specializations per generic;
///     overflow instantiations take the boxed fallback and emit a one-time diagnostic.
pub fn monomorphize(module: &mut TypedModule) -> Vec<Diagnostic> {
    let no_imports: HashMap<String, TypedModule> = HashMap::new();
    monomorphize_inner(module, &no_imports, false)
}

/// Cross-module entry point: like `monomorphize`, but also discovers generic functions reachable
/// through this module's `import { … }` statements (whose generic bodies live in `imports`). A call
/// to an imported generic is specialized HERE — the imported body is cloned, type-substituted, its
/// free references re-homed into the importer (sibling calls → `Named` exports of the origin module,
/// intrinsics → merged intrinsic slots, imports → the importer's own re-imports), and emitted as a
/// local specialization. The importing module's call is then rerouted to that native specialization,
/// so the Int32 instantiation of e.g. `std/array.map` lowers to a flat unboxed loop. The single
/// boxed copy compiled into the imported module is left untouched (and simply goes unused when every
/// caller specializes).
pub fn monomorphize_with_imports(
    module: &mut TypedModule,
    imports: &HashMap<String, TypedModule>,
) -> Vec<Diagnostic> {
    monomorphize_inner(module, imports, false)
}

/// Like `monomorphize_with_imports`, but compiling a module that is itself being lowered as an
/// IMPORT (`lower_import_module`). Cross-module generic calls it makes (e.g. `examples/report`'s
/// `records.reduce(0, …)` calling the generic `std/array.reduce`) are specialized HERE using the
/// program's `imports` map — exactly like the top-level importer — so the boxed-generic fallback
/// (which returns a type-erased `Json`, mismatching a concrete scalar use site and crashing
/// codegen) is avoided. `keep_all_originals` is true because an external importer of THIS module's
/// own generics may still issue a boxed `Named` call to the kept originals.
pub fn monomorphize_import_with_imports(
    module: &mut TypedModule,
    imports: &HashMap<String, TypedModule>,
) -> Vec<Diagnostic> {
    monomorphize_inner(module, imports, true)
}

fn monomorphize_inner(
    module: &mut TypedModule,
    imports: &HashMap<String, TypedModule>,
    keep_all_originals: bool,
) -> Vec<Diagnostic> {
    // 1. Discover top-level generic functions defined in THIS module (slot -> GenericFn).
    let mut generics: HashMap<usize, GenericFn> = HashMap::new();
    for stmt in &module.statements {
        if let TypedStmt::Val { slot, name: Some(name), value, .. } = stmt {
            if let TypedExpr::Function { params, ret_type, .. } = value {
                let is_generic = params.iter().any(|p| mentions_generic_tv(&p.ty))
                    || mentions_generic_tv(ret_type);
                if is_generic {
                    generics.insert(*slot, GenericFn { name: name.clone(), func: value.clone(), origin: None });
                }
            }
        }
    }

    // 1a. Discover generic functions reachable through imports. For each `import { name } from
    //     "path"` binding whose imported definition is a generic function, register it keyed by the
    //     IMPORTER's binding slot, tagged with its origin module path. A call through that binding
    //     slot is then specialized exactly like a local generic call (the body is re-homed first).
    for stmt in &module.statements {
        if let TypedStmt::Import { path, bindings, .. } = stmt {
            let Some(origin) = imports.get(path) else { continue };
            for b in bindings {
                if let Some(func) = find_exported_fn(origin, &b.name) {
                    if let TypedExpr::Function { params, .. } = &func {
                        // A TRUE cross-module generic has a `<T>` parameter mentioned in its PARAMS
                        // (the call site can then pin it from argument types). We deliberately do
                        // NOT treat a function generic only in its RETURN as monomorphizable here:
                        // stdlib intrinsic wrappers (`iter`, `iterOf`, `range`, …) carry an
                        // intrinsic-polymorphism TypeVar (e.g. `Iterator<TypeVar(9021)>`) in their
                        // INFERRED return — those are not user generics and must keep their single
                        // boxed compilation (specializing them would be both wrong and uninferrable).
                        let is_generic = params.iter().any(|p| mentions_generic_tv(&p.ty));
                        if is_generic {
                            generics.insert(
                                b.slot,
                                GenericFn { name: b.name.clone(), func, origin: Some(path.clone()) },
                            );
                        }
                    }
                }
            }
        }
    }
    if generics.is_empty() {
        return Vec::new(); // No-op for ordinary modules.
    }

    // 1b. Build the alias map: `val f = id` where `id` (transitively) names a generic. The call
    //     rewriter treats a call through an alias slot exactly like a direct call to the underlying
    //     generic. This is what lets `val f = id; f(5)` monomorphize correctly (BUG 2).
    let aliases = collect_generic_aliases(&module.statements, &generics);

    // The slot allocator must clear not just the importer's own max slot, but every slot
    // appearing inside an imported generic body we may clone in (origin-module param/local slots
    // live in the origin module's numbering and would otherwise collide). Take the max across all.
    let mut next_slot = max_slot(module) + 1;
    for g in generics.values() {
        if g.origin.is_some() {
            let mut m = 0usize;
            max_slot_expr(&g.func, &mut m);
            next_slot = next_slot.max(m + 1);
        }
    }

    let direct_callable_fn_slots = collect_direct_callable_fn_slots(module);

    let mut state = MonoState {
        generics,
        aliases,
        specs: HashMap::new(),
        worklist: Vec::new(),
        per_generic_count: HashMap::new(),
        boxed_fallback_used: std::collections::HashSet::new(),
        next_slot,
        used_generic_slots: std::collections::HashSet::new(),
        diagnostics: Vec::new(),
        budget: specialization_budget(),
        imports,
        rehomed_imports: Vec::new(),
        rehomed_intrinsics: HashMap::new(),
        rehome_binding_cache: HashMap::new(),
        rehome_intrinsic_cache: HashMap::new(),
        direct_callable_fn_slots,
    };

    // 2. Walk the whole module, rewriting calls to generic functions and queuing specializations.
    let mut stmts = std::mem::take(&mut module.statements);
    for stmt in &mut stmts {
        rewrite_stmt(stmt, &mut state);
    }

    // 3. Drain the worklist: materialize each native specialization by cloning the generic body,
    //    substituting its quantified TypeVars, then re-running the call rewriter over the body
    //    (which may mint further specializations — pushed back onto the worklist). Fixpoint.
    let mut materialized: Vec<TypedStmt> = Vec::new();
    // Coverage attribution for cross-module specializations: spec slot → origin module path.
    let mut spec_origins: HashMap<usize, String> = HashMap::new();
    while let Some(key) = state.worklist.pop() {
        let (generic_slot, spec_slot, spec_name, subs) = {
            let info = &state.specs[&key];
            (info.generic_slot, info.slot, info.name.clone(), info.subs.clone())
        };
        let origin = state.generics[&generic_slot].origin.clone();
        let mut func = state.generics[&generic_slot].func.clone();
        let span = func.span();
        subst_expr(&mut func, &subs);
        if let TypedExpr::Function { name, .. } = &mut func {
            *name = Some(spec_name.clone());
        }
        // For a CROSS-MODULE generic, the cloned body's free references (sibling calls,
        // intrinsics, the origin module's own imports/vals) and its local slots are numbered in
        // the ORIGIN module's scope — meaningless in the importer. Re-home them: remap every
        // local slot to a fresh importer slot, and rewrite each free reference into an
        // importer-side construct (a Named import binding / merged intrinsic / re-import) that the
        // importer's lowering already knows how to resolve. Must run BEFORE `rewrite_expr` so its
        // re-monomorphization of nested generic calls sees importer-stable slots.
        if let Some(origin_path) = &origin {
            rehome_imported_body(&mut func, origin_path, &mut state);
        }
        // Re-monomorphize calls inside the now-concrete body (worklist fixpoint).
        rewrite_expr(&mut func, &mut state);
        let ty = func.ty();
        // Coverage: a CROSS-MODULE specialization's body was cloned from `origin`; its block spans
        // index into the ORIGIN module's source, not the importer's. Record the origin path so
        // lowering can stamp it on the spec's LinFunction and codegen attributes the spec's regions
        // to the generic definition's file (otherwise the imported generic reports 0% coverage even
        // though its instances run).
        if let Some(origin_path) = &origin {
            spec_origins.insert(spec_slot, origin_path.clone());
        }
        materialized.push(TypedStmt::Val {
            slot: spec_slot,
            name: Some(spec_name),
            value: func,
            ty,
            span,
        });
    }
    // Deterministic order so codegen/IR output is stable across runs.
    materialized.sort_by_key(|s| if let TypedStmt::Val { slot, .. } = s { *slot } else { 0 });

    // 3b. A generic function used as a FIRST-CLASS VALUE that escapes (e.g. passed as an argument
    //     to another function, `apply(f, 5)`) cannot be monomorphized: there is no single concrete
    //     type to specialize at, and emitting the bare generic as a closure value would feed
    //     codegen a half-typed function (the historical malformed-IR / parameter-type-mismatch).
    //     Detect any surviving generic/alias `LocalGet` that is not (a) the direct callee of a
    //     boxed-fallback call or (b) the RHS of a plain alias `val`, and report a clear diagnostic
    //     rather than letting codegen emit broken IR. (Out of single-module Phase 0/3.5 scope.)
    let generic_slots: std::collections::HashSet<usize> = state.generics.keys().copied().collect();
    let alias_slots: std::collections::HashSet<usize> = state.aliases.keys().copied().collect();
    let mut value_use: Option<(usize, lin_common::Span)> = None;
    for stmt in stmts.iter().chain(materialized.iter()) {
        scan_value_uses(stmt, &generic_slots, &alias_slots, &mut |slot, span| {
            if value_use.is_none() {
                value_use = Some((slot, span));
            }
        });
    }
    if let Some((slot, span)) = value_use {
        let gslot = if generic_slots.contains(&slot) { slot } else { state.aliases[&slot] };
        let name = state.generics[&gslot].name.clone();
        state.diagnostics.push(
            Diagnostic::error(span, format!(
                "generic function '{}' is used as a first-class value here, which is not supported",
                name
            ))
            .with_help("call the generic directly (e.g. `f(x)`) so it can be monomorphized to a concrete type".to_string())
        );
    }

    // 4. Drop generic originals that are no longer referenced. An original is KEPT when it is still
    //    used: either directly as a first-class value, or as the target of a boxed-fallback call.
    //    `keep_all_originals` (import compilation) additionally keeps EVERY local generic original
    //    so that an external importer that doesn't specialize a call still resolves the boxed
    //    `{module_key}_{name}` symbol. (Cross-module re-homed generics — `origin.is_some()` — are
    //    never emitted as locals anyway; only this module's own generics are subject to the drop.)
    let keep: std::collections::HashSet<usize> = state
        .used_generic_slots
        .union(&state.boxed_fallback_used)
        .copied()
        .collect();
    stmts.retain(|stmt| {
        if let TypedStmt::Val { slot, value: TypedExpr::Function { .. }, .. } = stmt {
            if generic_slots.contains(slot) {
                return keep_all_originals || keep.contains(slot);
            }
        }
        true
    });

    // Merge any intrinsic slots discovered while re-homing cross-module bodies into the module's
    // intrinsic map, so lowering resolves them (e.g. `lin_array_allocate`, `lin_for`).
    for (slot, name) in &state.rehomed_intrinsics {
        module.intrinsics.insert(*slot, name.clone());
    }

    // Prepend the re-homed import statements (sibling/foreign/val bindings of the origin modules)
    // so lowering's Import pre-pass registers their Named symbols before the specializations that
    // call them are lowered.
    let rehomed = std::mem::take(&mut state.rehomed_imports);

    // Insert specializations after the originals. Order is immaterial — top-level function `val`s
    // are forward-declared by slot in lowering.
    stmts.extend(materialized);
    let mut final_stmts = rehomed;
    final_stmts.extend(stmts);
    module.statements = final_stmts;
    module.spec_origins = spec_origins;
    state.diagnostics
}

/// Find an exported top-level function `val name = <Function>` in `module` by name, returning a
/// clone of its `TypedExpr::Function`. Used to pull an imported generic's body into the importer.
fn find_exported_fn(module: &TypedModule, name: &str) -> Option<TypedExpr> {
    module.statements.iter().find_map(|s| match s {
        TypedStmt::Val { name: Some(n), value: value @ TypedExpr::Function { .. }, .. } if n == name => {
            Some(value.clone())
        }
        _ => None,
    })
}

// ---------------------------------------------------------------------------
// Cross-module body re-homing
// ---------------------------------------------------------------------------

/// How a free (non-local) slot referenced inside a re-homed cross-module body resolves in the
/// origin module — used to pick the importer-side construct it should be rewritten into.
/// If `ty` is a combinator's iterable-union parameter `T[] | Iterator | Stream` (in any order),
/// return the quantified element TypeVar id of the `T[]` arm. Used to default `T` to the `Json`
/// wildcard when such a parameter is applied to an OPAQUE `Iterator`/`Stream` argument that carries
/// no static element type. Only fires for the exact iterable-union shape (an `Array(TypeVar)` arm
/// alongside `Iterator`/`Stream` arms); a plain `T[]` param is NOT matched (its `T` binds normally
/// from the array's element type).
fn iterable_union_elem_tv(ty: &Type) -> Option<u32> {
    let Type::Union(arms) = ty else { return None };
    let mut has_iter_or_stream = false;
    let mut elem_id = None;
    for arm in arms {
        match arm {
            Type::Array(inner) => {
                if let Type::TypeVar(id) = **inner {
                    if id >= GENERIC_TV_BASE && id != u32::MAX {
                        elem_id = Some(id);
                    }
                }
            }
            Type::Iterator(_) | Type::Stream(_) => has_iter_or_stream = true,
            _ => {}
        }
    }
    if has_iter_or_stream { elem_id } else { None }
}

/// True if a cross-module generic's body has a FREE reference to a top-level mutable `var` of its
/// origin module (e.g. `std/random`'s global `g: Rng` read by `pick`/`shuffled`). Such a global is
/// genuine shared state living in the origin module; a native specialization clones the body into
/// the IMPORTER, where that global is NOT in scope — `classify_origin_slot` cannot re-home a `Var`
/// (it has no exported symbol), so the reference is silently dropped (its `LocalGet` produces no
/// value → the call to a function taking it loses an argument → codegen arity error). Detecting this
/// lets the caller route to the boxed (type-erased) fallback, which keeps the original body in the
/// origin module where the global resolves.
fn body_refs_origin_global_var(body: &TypedExpr, origin: &TypedModule) -> bool {
    // Origin top-level `var` slots (the genuine module globals).
    let mut origin_var_slots = std::collections::HashSet::new();
    for stmt in &origin.statements {
        if let TypedStmt::Var { slot, .. } = stmt {
            origin_var_slots.insert(*slot);
        }
    }
    if origin_var_slots.is_empty() {
        return false;
    }
    // Slots bound locally inside the body (params/vals/vars) are not the origin globals.
    let mut locals = std::collections::HashSet::new();
    collect_local_slots(body, &mut locals);
    let mut found = false;
    walk_local_slot_refs(body, &mut |slot| {
        if origin_var_slots.contains(&slot) && !locals.contains(&slot) {
            found = true;
        }
    });
    found
}

/// Visit every `LocalGet`/`LocalSet` slot referenced anywhere in `expr`.
fn walk_local_slot_refs(expr: &TypedExpr, f: &mut impl FnMut(usize)) {
    match expr {
        TypedExpr::LocalGet { slot, .. } | TypedExpr::LocalSet { slot, .. } => f(*slot),
        _ => {}
    }
    for_each_child(expr, &mut |c| walk_local_slot_refs(c, f));
}

enum OriginRef {
    /// An intrinsic (origin's `intrinsics[slot]` = name). Merged into the importer's intrinsic map.
    Intrinsic(String),
    /// A top-level function/val (or import-of-import) that resolves to a `Named` export of some
    /// module. `path` is the module that actually DEFINES the symbol (the origin module itself for
    /// a local sibling, or the origin's own import source for an import-of-import — the symbol lives
    /// under THAT module's mangled prefix, never the intermediate importer's).
    Sibling { path: String, name: String, ty: Type },
    /// A foreign (FFI) binding `name` with type `ty`. Re-declared as a ForeignImport so the raw
    /// symbol resolves.
    Foreign(String, Type),
}

/// Classify an origin-module slot. Returns `None` for a slot that is local to the function body
/// (param / inner `val`/`var`/destructure) — those are slot-remapped, not re-imported.
///
/// `origin_path` is the module the body came from. For an import-of-import (the origin module
/// itself imported the name from elsewhere), the resolved `Sibling.path` is the SOURCE module that
/// defines the symbol — the symbol lives under that module's mangled prefix, never the
/// intermediate's. This is what makes `helpers.lin`'s `import { for, push } from "std/array"`
/// re-home to `std_array_for` / `std_array_push`, not the non-existent `._helpers_for`.
fn classify_origin_slot(
    origin: &TypedModule,
    origin_path: &str,
    slot: usize,
    imports: &HashMap<String, TypedModule>,
) -> Option<OriginRef> {
    if let Some(name) = origin.intrinsics.get(&slot) {
        return Some(OriginRef::Intrinsic(name.clone()));
    }
    for stmt in &origin.statements {
        match stmt {
            TypedStmt::Val { slot: s, name: Some(name), value, ty, .. } if *s == slot => {
                // A thin intrinsic wrapper (`for = (it, f) => lin_for(it, f)`,
                // `push = (a, x) => lin_push(a, x)`, `length = (x) => lin_length(x)`) is INLINED to
                // the underlying intrinsic. This is the flat-array lever: routing the re-homed body
                // through the polymorphic `lin_*` builtin (which dispatches on the array's concrete
                // runtime element type) keeps Int32 elements unboxed, whereas a `Named` call to the
                // boxed `{key}_{name}` wrapper would force the uniform boxed-Function/TaggedVal
                // element ABI — defeating the specialization (and, for `for`'s callback, mismatching
                // the concrete-element closure → a tagged-value misread at runtime).
                if let Some(intr) = thin_intrinsic_wrapper(origin, value) {
                    return Some(OriginRef::Intrinsic(intr));
                }
                // Otherwise: a real sibling, resolved through a Named symbol under the origin's
                // mangled prefix (`{key}_{name}` for fns, `{key}_{name}__val` for non-fn vals).
                return Some(OriginRef::Sibling {
                    path: origin_path.to_string(),
                    name: name.clone(),
                    ty: ty.clone(),
                });
            }
            TypedStmt::ForeignImport { bindings, .. } => {
                for b in bindings {
                    if b.slot == slot {
                        return Some(OriginRef::Foreign(b.name.clone(), b.ty.clone()));
                    }
                }
            }
            TypedStmt::Import { path, bindings, .. } => {
                for b in bindings {
                    if b.slot == slot {
                        // Import-of-import: the symbol is defined by `path` (the source module),
                        // not by `origin_path`. If the SOURCE module defines it as a thin intrinsic
                        // wrapper (`push = <T>(a, x) => lin_push(a, x)`), INLINE it to the intrinsic —
                        // exactly as the direct-sibling arm does. Without this, a generic re-homed
                        // body that calls a re-exported thin wrapper (e.g. `helpers.lin`'s
                        // `import { push } from "std/array"` used inside a generic `mymap<T,U>`)
                        // re-homes to the boxed `std_array_push` $Json specialization, which routes a
                        // monomorphized FLAT `Int32[]` element through `lin_array_push_tagged` (a
                        // 16-byte tagged write into a 4-byte flat slot → heap-buffer-overflow). The
                        // intrinsic dispatches on the array's concrete runtime element type, keeping
                        // the flat representation correct.
                        if let Some(src) = imports.get(path) {
                            if let Some(intr) = src.statements.iter().find_map(|s| match s {
                                TypedStmt::Val { name: Some(n), value, .. } if *n == b.name =>
                                    thin_intrinsic_wrapper(src, value),
                                _ => None,
                            }) {
                                return Some(OriginRef::Intrinsic(intr));
                            }
                        }
                        // Re-home the reference to that source module as a sibling Named call.
                        return Some(OriginRef::Sibling {
                            path: path.clone(),
                            name: b.name.clone(),
                            ty: b.ty.clone(),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// If `value` is a thin intrinsic wrapper — a function whose body is exactly a call to an origin
/// intrinsic `lin_X` forwarding its parameters 1:1 in order (modulo a transparent `Coerce`/`Block`
/// wrapper) — return the intrinsic name `lin_X`. Used to INLINE such wrappers (`for`, `push`,
/// `length`, …) to the intrinsic when re-homing, so the polymorphic builtin's concrete-element
/// dispatch is preserved. Returns `None` for any non-trivial body.
fn thin_intrinsic_wrapper(origin: &TypedModule, value: &TypedExpr) -> Option<String> {
    let TypedExpr::Function { params, body, .. } = value else { return None };
    // Unwrap a transparent trailing-expression Block or a Coerce around the call.
    let mut inner = body.as_ref();
    loop {
        match inner {
            TypedExpr::Block { stmts, expr, .. } if stmts.is_empty() => inner = expr,
            TypedExpr::Coerce { expr, .. } => inner = expr,
            _ => break,
        }
    }
    let TypedExpr::Call { func, args, .. } = inner else { return None };
    // Callee must be an intrinsic LocalGet of THIS module.
    let TypedExpr::LocalGet { slot, .. } = func.as_ref() else { return None };
    let intr = origin.intrinsics.get(slot)?;
    // Arguments must be exactly the params, in order, by slot (each possibly Coerce-wrapped).
    if args.len() != params.len() {
        return None;
    }
    for (a, p) in args.iter().zip(params.iter()) {
        let mut ai = a;
        while let TypedExpr::Coerce { expr, .. } = ai {
            ai = expr;
        }
        match ai {
            TypedExpr::LocalGet { slot: s, .. } if *s == p.slot => {}
            _ => return None,
        }
    }
    Some(intr.clone())
}

/// Collect every slot that is BOUND locally within a function body: its own params, plus any
/// `val`/`var`/destructure slot introduced inside (including nested functions' params/captures
/// targets). These are remapped to fresh importer slots; everything else is a free reference.
fn collect_local_slots(func: &TypedExpr, out: &mut std::collections::HashSet<usize>) {
    if let TypedExpr::Function { params, body, .. } = func {
        for p in params {
            out.insert(p.slot);
        }
        collect_local_slots_expr(body, out);
    }
}

fn collect_local_slots_expr(expr: &TypedExpr, out: &mut std::collections::HashSet<usize>) {
    match expr {
        TypedExpr::Function { params, body, .. } => {
            for p in params { out.insert(p.slot); }
            collect_local_slots_expr(body, out);
        }
        TypedExpr::Block { stmts, expr, .. } => {
            for s in stmts { collect_local_slots_stmt(s, out); }
            collect_local_slots_expr(expr, out);
        }
        _ => for_each_child(expr, &mut |c| collect_local_slots_expr(c, out)),
    }
}

fn collect_local_slots_stmt(stmt: &TypedStmt, out: &mut std::collections::HashSet<usize>) {
    match stmt {
        TypedStmt::Val { slot, value, .. } | TypedStmt::Var { slot, value, .. } => {
            out.insert(*slot);
            collect_local_slots_expr(value, out);
        }
        TypedStmt::Destructure { obj_slot, value, fields, rest, .. } => {
            out.insert(*obj_slot);
            for (_, s, _) in fields { out.insert(*s); }
            if let Some(s) = rest { out.insert(*s); }
            collect_local_slots_expr(value, out);
        }
        TypedStmt::ArrayDestructure { arr_slot, value, elements, rest, .. } => {
            out.insert(*arr_slot);
            for (_, s, _) in elements { out.insert(*s); }
            if let Some((s, _)) = rest { out.insert(*s); }
            collect_local_slots_expr(value, out);
        }
        TypedStmt::Expr(e) => collect_local_slots_expr(e, out),
        TypedStmt::Import { .. } | TypedStmt::ForeignImport { .. } => {}
    }
}

/// Re-home a cloned cross-module generic body into the importing module.
///
/// 1. Every locally-bound slot (params, inner `val`/`var`, destructure targets) is remapped to a
///    FRESH importer slot, so it can't collide with the importer's own slots or with another
///    specialization minted from the same/another origin module.
/// 2. Every FREE slot (a reference to the origin module's own scope — a sibling function, an
///    intrinsic, a foreign binding, or a non-function val) is rewritten to a fresh importer slot
///    that is registered with the importer via either a synthesised `TypedStmt::Import` /
///    `ForeignImport` (so lowering issues a `Named` call to the origin module's exported symbol)
///    or a merged intrinsic-slot entry. References are deduped per (origin, name) so one binding
///    serves all uses across all specializations.
fn rehome_imported_body(func: &mut TypedExpr, origin_path: &str, state: &mut MonoState<'_>) {
    let origin = match state.imports.get(origin_path) {
        Some(m) => m.clone(),
        None => return,
    };
    // 1. Determine which slots are local to this body.
    let mut locals = std::collections::HashSet::new();
    collect_local_slots(func, &mut locals);

    // 2. Build the slot remap: locals → fresh importer slots; frees → fresh importer slots backed
    //    by a re-homed binding/intrinsic. Done lazily during the rewrite walk.
    let mut remap: HashMap<usize, usize> = HashMap::new();
    for &local in &locals {
        let fresh = state.next_slot;
        state.next_slot += 1;
        remap.insert(local, fresh);
    }

    // 3. Resolve (or mint) the importer slot for a free origin slot, registering the matching
    //    importer-side binding the first time it is seen.
    rehome_walk(func, &origin, origin_path, &locals, &mut remap, state);
}

/// Resolve the importer slot a free origin slot should be rewritten to, minting + registering the
/// re-homed binding/intrinsic on first encounter (deduped per origin+name).
fn rehome_free_slot(
    origin_slot: usize,
    origin: &TypedModule,
    origin_path: &str,
    state: &mut MonoState<'_>,
) -> Option<usize> {
    let origin_ref = classify_origin_slot(origin, origin_path, origin_slot, state.imports)?;
    match origin_ref {
        OriginRef::Intrinsic(name) => {
            // Intrinsics are global runtime builtins — dedupe by name alone (not per-origin) so a
            // single merged intrinsic slot serves every re-homed body that uses it.
            let key = (String::new(), name.clone());
            if let Some(&s) = state.rehome_intrinsic_cache.get(&key) {
                return Some(s);
            }
            let fresh = state.next_slot;
            state.next_slot += 1;
            state.rehome_intrinsic_cache.insert(key, fresh);
            state.rehomed_intrinsics.insert(fresh, name);
            Some(fresh)
        }
        OriginRef::Sibling { path, name, ty } => {
            rehome_import_binding(&path, &name, ty, false, state)
        }
        OriginRef::Foreign(name, ty) => {
            rehome_import_binding(origin_path, &name, ty, true, state)
        }
    }
}

/// Mint (deduped) a fresh importer slot for a re-homed import/foreign binding and append the
/// matching one-binding `TypedStmt::Import`/`ForeignImport` to `rehomed_imports`.
fn rehome_import_binding(
    origin_path: &str,
    name: &str,
    ty: Type,
    foreign: bool,
    state: &mut MonoState<'_>,
) -> Option<usize> {
    let key = (origin_path.to_string(), name.to_string());
    if let Some(&s) = state.rehome_binding_cache.get(&key) {
        return Some(s);
    }
    let fresh = state.next_slot;
    state.next_slot += 1;
    state.rehome_binding_cache.insert(key, fresh);
    let span = lin_common::Span::dummy();
    if foreign {
        state.rehomed_imports.push(TypedStmt::ForeignImport {
            path: "lin-runtime".to_string(),
            bindings: vec![ForeignSlot { name: name.to_string(), slot: fresh, ty, valid: true }],
            span,
        });
    } else {
        state.rehomed_imports.push(TypedStmt::Import {
            path: origin_path.to_string(),
            bindings: vec![ImportSlot { name: name.to_string(), slot: fresh, ty }],
            span,
        });
    }
    Some(fresh)
}

/// Rewrite slots throughout a cloned body: locals via `remap`, frees via `rehome_free_slot`
/// (registering the importer binding on first encounter and extending `remap`).
fn rehome_walk(
    expr: &mut TypedExpr,
    origin: &TypedModule,
    origin_path: &str,
    locals: &std::collections::HashSet<usize>,
    remap: &mut HashMap<usize, usize>,
    state: &mut MonoState<'_>,
) {
    // Resolve a single slot to its importer-side target, minting bindings as needed.
    fn resolve(
        slot: usize,
        origin: &TypedModule,
        origin_path: &str,
        locals: &std::collections::HashSet<usize>,
        remap: &mut HashMap<usize, usize>,
        state: &mut MonoState<'_>,
    ) -> usize {
        if let Some(&s) = remap.get(&slot) {
            return s;
        }
        if locals.contains(&slot) {
            // A local we somehow hadn't pre-mapped (shouldn't happen — pre-seeded). Mint one.
            let fresh = state.next_slot;
            state.next_slot += 1;
            remap.insert(slot, fresh);
            return fresh;
        }
        if let Some(fresh) = rehome_free_slot(slot, origin, origin_path, state) {
            remap.insert(slot, fresh);
            fresh
        } else {
            // Unknown free slot (e.g. a forward-declared origin global not classified). Leave it;
            // lowering will treat it as an out-of-scope placeholder. Record identity to avoid loop.
            remap.insert(slot, slot);
            slot
        }
    }

    match expr {
        TypedExpr::LocalGet { slot, .. } | TypedExpr::LocalSet { slot, .. } => {
            *slot = resolve(*slot, origin, origin_path, locals, remap, state);
        }
        TypedExpr::Function { params, captures, .. } => {
            for p in params.iter_mut() {
                p.slot = resolve(p.slot, origin, origin_path, locals, remap, state);
            }
            for c in captures.iter_mut() {
                c.outer_slot = resolve(c.outer_slot, origin, origin_path, locals, remap, state);
            }
        }
        _ => {}
    }
    // Statement-bound slots inside blocks need their binding slot remapped too.
    if let TypedExpr::Block { stmts, .. } = expr {
        for s in stmts.iter_mut() {
            rehome_stmt_slots(s, origin, origin_path, locals, remap, state);
        }
    }
    for_each_child_mut(expr, &mut |c| rehome_walk(c, origin, origin_path, locals, remap, state));
}

fn rehome_stmt_slots(
    stmt: &mut TypedStmt,
    origin: &TypedModule,
    origin_path: &str,
    locals: &std::collections::HashSet<usize>,
    remap: &mut HashMap<usize, usize>,
    state: &mut MonoState<'_>,
) {
    let r = |slot: usize, state: &mut MonoState<'_>, remap: &mut HashMap<usize, usize>| {
        if let Some(&s) = remap.get(&slot) { return s; }
        let fresh = state.next_slot;
        state.next_slot += 1;
        remap.insert(slot, fresh);
        fresh
    };
    match stmt {
        TypedStmt::Val { slot, .. } | TypedStmt::Var { slot, .. } => {
            *slot = r(*slot, state, remap);
        }
        TypedStmt::Destructure { obj_slot, fields, rest, .. } => {
            *obj_slot = r(*obj_slot, state, remap);
            for (_, s, _) in fields.iter_mut() { *s = r(*s, state, remap); }
            if let Some(s) = rest { *s = r(*s, state, remap); }
        }
        TypedStmt::ArrayDestructure { arr_slot, elements, rest, .. } => {
            *arr_slot = r(*arr_slot, state, remap);
            for (_, s, _) in elements.iter_mut() { *s = r(*s, state, remap); }
            if let Some((s, _)) = rest { *s = r(*s, state, remap); }
        }
        _ => {}
    }
    let _ = (origin, origin_path, locals);
}

/// Mutable working state threaded through the rewrite/worklist passes.
struct MonoState<'a> {
    /// Top-level generic functions, keyed by their `val` slot.
    generics: HashMap<usize, GenericFn>,
    /// Alias slot -> underlying generic slot (`val f = id`).
    aliases: HashMap<usize, usize>,
    /// Deduped specializations, keyed by (generic slot + sorted concrete args).
    specs: HashMap<(usize, Vec<(u32, String)>), SpecInfo>,
    /// Spec keys awaiting materialization (worklist for the fixpoint).
    worklist: Vec<(usize, Vec<(u32, String)>)>,
    /// Native specialization count per generic slot (for the budget).
    per_generic_count: HashMap<usize, usize>,
    /// Generic slots that have emitted the one-time budget-overflow diagnostic.
    boxed_fallback_used: std::collections::HashSet<usize>,
    next_slot: usize,
    /// Generic slots still referenced as plain first-class values (kept, not dropped).
    used_generic_slots: std::collections::HashSet<usize>,
    diagnostics: Vec<Diagnostic>,
    /// Per-generic native-specialization cap (see `specialization_budget`).
    budget: usize,
    /// Imported TypedModules, keyed by import path — the source of cross-module generic bodies
    /// and the scope used to classify a re-homed body's free references.
    imports: &'a HashMap<String, TypedModule>,
    /// `TypedStmt::Import`/`ForeignImport` statements synthesised while re-homing cross-module
    /// bodies (sibling/foreign/val bindings of the origin modules), prepended to the module.
    rehomed_imports: Vec<TypedStmt>,
    /// Intrinsic slots (fresh importer slot → intrinsic name) discovered while re-homing; merged
    /// into the module's intrinsic map so lowering resolves them.
    rehomed_intrinsics: HashMap<usize, String>,
    /// Dedup: (origin_path, exported-name) → the fresh importer slot already minted for re-homing
    /// a reference to that origin binding (sibling fn / foreign / val). Keeps one binding per use.
    rehome_binding_cache: HashMap<(String, String), usize>,
    /// Dedup: (origin_path, intrinsic-name) → fresh importer slot already minted.
    rehome_intrinsic_cache: HashMap<(String, String), usize>,
    /// Module-level slots that name a DIRECT-CALLABLE function: a top-level `val f = (…) => …` or an
    /// imported/FFI function binding. A bare reference to one of these as a combinator callback can
    /// be eta-expanded to `(p…) => f(p…)` (a genuinely CAPTURE-LESS lambda — `f` is a module symbol,
    /// not a captured slot) so `try_inline_combinator_wrapper` routes it through the inline intrinsic.
    /// A first-class function PARAMETER, a stored closure, or a local fn-typed `val` is NOT here, so
    /// it keeps the closure-call path. Populated once in `monomorphize_with_imports`.
    direct_callable_fn_slots: std::collections::HashSet<usize>,
}

struct SpecInfo {
    generic_slot: usize,
    slot: usize,
    name: String,
    subs: HashMap<u32, Type>,
}

/// True if `slot` names a generic function or an alias of one.
fn is_generic_or_alias(
    slot: usize,
    generic_slots: &std::collections::HashSet<usize>,
    alias_slots: &std::collections::HashSet<usize>,
) -> bool {
    generic_slots.contains(&slot) || alias_slots.contains(&slot)
}

/// Walk a top-level statement reporting any `LocalGet` of a generic/alias slot that ESCAPES as a
/// first-class value. Legitimate, non-escaping occurrences are skipped:
///   - a plain alias `val f = <generic LocalGet>` RHS (just records the binding), and
///   - the direct callee (`func`) of a `Call` (a call we either monomorphized or routed through
///     the boxed fallback — both fine).
fn scan_value_uses(
    stmt: &TypedStmt,
    generic_slots: &std::collections::HashSet<usize>,
    alias_slots: &std::collections::HashSet<usize>,
    report: &mut dyn FnMut(usize, lin_common::Span),
) {
    match stmt {
        // Skip the RHS of a pure alias binding (`val f = id`).
        TypedStmt::Val { value: TypedExpr::LocalGet { slot, .. }, .. }
            if is_generic_or_alias(*slot, generic_slots, alias_slots) => {}
        TypedStmt::Val { value, .. } | TypedStmt::Var { value, .. } => {
            scan_value_uses_expr(value, generic_slots, alias_slots, report)
        }
        TypedStmt::Destructure { value, .. } | TypedStmt::ArrayDestructure { value, .. } => {
            scan_value_uses_expr(value, generic_slots, alias_slots, report)
        }
        TypedStmt::Expr(e) => scan_value_uses_expr(e, generic_slots, alias_slots, report),
        TypedStmt::Import { .. } | TypedStmt::ForeignImport { .. } => {}
    }
}

fn scan_value_uses_expr(
    expr: &TypedExpr,
    generic_slots: &std::collections::HashSet<usize>,
    alias_slots: &std::collections::HashSet<usize>,
    report: &mut dyn FnMut(usize, lin_common::Span),
) {
    // A direct LocalGet of a generic/alias slot reached here (i.e. NOT excluded as a Call func or
    // alias RHS) is an escaping value use.
    if let TypedExpr::LocalGet { slot, span, .. } = expr {
        if is_generic_or_alias(*slot, generic_slots, alias_slots) {
            report(*slot, *span);
            return;
        }
    }
    // For a Call, the callee `func` is allowed to be a generic/alias LocalGet (monomorphized or
    // boxed-fallback call). Scan only the arguments for escaping value uses.
    if let TypedExpr::Call { args, .. } = expr {
        for a in args {
            scan_value_uses_expr(a, generic_slots, alias_slots, report);
        }
        return;
    }
    for_each_child(expr, &mut |c| scan_value_uses_expr(c, generic_slots, alias_slots, report));
}

/// Build the alias map: every `val X = <LocalGet of a generic-or-alias slot>` records `X`'s slot
/// pointing at the underlying generic. Resolved transitively so `val g = f; val f = id` both map
/// to `id`. Only plain re-bindings are aliases; any other use is a real value reference.
fn collect_generic_aliases(
    stmts: &[TypedStmt],
    generics: &HashMap<usize, GenericFn>,
) -> HashMap<usize, usize> {
    let mut aliases: HashMap<usize, usize> = HashMap::new();
    // Direct generic-slot targets first.
    let mut changed = true;
    while changed {
        changed = false;
        for stmt in stmts {
            if let TypedStmt::Val { slot, value: TypedExpr::LocalGet { slot: src, .. }, .. } = stmt {
                let target = if generics.contains_key(src) {
                    Some(*src)
                } else {
                    aliases.get(src).copied()
                };
                if let Some(t) = target {
                    if aliases.insert(*slot, t) != Some(t) {
                        changed = true;
                    }
                }
            }
        }
    }
    aliases
}

/// Collect the module-level slots that name a DIRECT-CALLABLE function — a binding whose bare
/// reference may be eta-expanded to `(p…) => f(p…)` (capture-less, since `f` is a module symbol) at a
/// combinator call site. These are: top-level `val f = (…) => …` (function literals), and
/// imported / FFI function bindings (their slots carry a `Type::Function`). Deliberately EXCLUDES a
/// fn-typed top-level `val` whose RHS is NOT a literal function (an alias / stored closure) — calling
/// it is still indirect — and of course any nested local binding or parameter (those aren't
/// module-level statements). See `MonoState::direct_callable_fn_slots` and `eta_expand_named_arg`.
fn collect_direct_callable_fn_slots(module: &TypedModule) -> std::collections::HashSet<usize> {
    let mut out = std::collections::HashSet::new();
    for stmt in &module.statements {
        match stmt {
            // A top-level function definition (`val f = (…) => …`). A non-function val RHS is not a
            // direct-callable symbol (it's data); a generic one is still a top-level fn and remains
            // direct-callable, but the combinator-inline path only fires for a NON-generic callback
            // anyway (a generic callback isn't a concrete combinator argument here).
            TypedStmt::Val { slot, value: TypedExpr::Function { .. }, .. } => {
                out.insert(*slot);
            }
            // Imported functions and FFI functions: direct-callable by their mangled symbol.
            TypedStmt::Import { bindings, .. } => {
                for b in bindings {
                    if matches!(b.ty, Type::Function { .. }) {
                        out.insert(b.slot);
                    }
                }
            }
            TypedStmt::ForeignImport { bindings, .. } => {
                for b in bindings {
                    if matches!(b.ty, Type::Function { .. }) {
                        out.insert(b.slot);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Highest slot index referenced anywhere in the module (Val/Var/param/destructure/LocalGet).
fn max_slot(module: &TypedModule) -> usize {
    let mut m = 0usize;
    for (slot, _) in module.intrinsics.iter() {
        m = m.max(*slot);
    }
    for stmt in &module.statements {
        max_slot_stmt(stmt, &mut m);
    }
    m
}

fn max_slot_stmt(stmt: &TypedStmt, m: &mut usize) {
    match stmt {
        TypedStmt::Val { slot, value, .. } => { *m = (*m).max(*slot); max_slot_expr(value, m); }
        TypedStmt::Var { slot, value, .. } => { *m = (*m).max(*slot); max_slot_expr(value, m); }
        TypedStmt::Destructure { obj_slot, value, fields, rest, .. } => {
            *m = (*m).max(*obj_slot);
            max_slot_expr(value, m);
            for (_, s, _) in fields { *m = (*m).max(*s); }
            if let Some(s) = rest { *m = (*m).max(*s); }
        }
        TypedStmt::ArrayDestructure { arr_slot, value, elements, rest, .. } => {
            *m = (*m).max(*arr_slot);
            max_slot_expr(value, m);
            for (_, s, _) in elements { *m = (*m).max(*s); }
            if let Some((s, _)) = rest { *m = (*m).max(*s); }
        }
        TypedStmt::Import { bindings, .. } => {
            for b in bindings { *m = (*m).max(b.slot); }
        }
        TypedStmt::ForeignImport { bindings, .. } => {
            for b in bindings { *m = (*m).max(b.slot); }
        }
        TypedStmt::Expr(e) => max_slot_expr(e, m),
    }
}

fn max_slot_expr(expr: &TypedExpr, m: &mut usize) {
    match expr {
        TypedExpr::LocalGet { slot, .. } | TypedExpr::LocalSet { slot, .. } => {
            *m = (*m).max(*slot);
        }
        TypedExpr::Function { params, body, captures, .. } => {
            for p in params { *m = (*m).max(p.slot); if let Some(d) = &p.default { max_slot_expr(d, m); } }
            for c in captures { *m = (*m).max(c.outer_slot); }
            max_slot_expr(body, m);
        }
        _ => for_each_child(expr, &mut |c| max_slot_expr(c, m)),
    }
    // LocalSet has a value child handled via for_each_child; cover params/captures above.
    if let TypedExpr::LocalSet { value, .. } = expr {
        max_slot_expr(value, m);
    }
}

// ---------------------------------------------------------------------------
// Call rewriting
// ---------------------------------------------------------------------------

fn rewrite_stmt(stmt: &mut TypedStmt, state: &mut MonoState<'_>) {
    match stmt {
        // The body of a top-level generic function is a TEMPLATE whose param/return types are
        // still symbolic TypeVars. Do NOT rewrite calls inside it here — its calls are only
        // resolvable once the body is cloned and substituted at a concrete instantiation
        // (materialization re-runs `rewrite_expr` on the substituted body). Rewriting the template
        // in place would see an inner call like `id(y:U)` as an unconstrained generic call.
        TypedStmt::Val { slot, value: TypedExpr::Function { .. }, .. }
            if state.generics.contains_key(slot) => {}
        TypedStmt::Val { value, .. } | TypedStmt::Var { value, .. } => rewrite_expr(value, state),
        TypedStmt::Destructure { value, .. } | TypedStmt::ArrayDestructure { value, .. } => {
            rewrite_expr(value, state)
        }
        TypedStmt::Expr(e) => rewrite_expr(e, state),
        TypedStmt::Import { .. } | TypedStmt::ForeignImport { .. } => {}
    }
}

fn rewrite_expr(expr: &mut TypedExpr, state: &mut MonoState<'_>) {
    // Recurse into children FIRST so any nested generic calls (e.g. in this call's arguments) are
    // rewritten before we handle this node. Doing it first also means that after we (possibly) wrap
    // a generic call in a `Coerce` for the boxed fallback, we do NOT re-descend into the wrapped
    // call — which would otherwise re-trigger the rewrite and loop forever.
    for_each_child_mut(expr, &mut |c| rewrite_expr(c, state));

    // Resolve the underlying generic slot of a call's callee (direct or via a `val f = id` alias).
    let callee_generic_slot = if let TypedExpr::Call { func, .. } = expr {
        if let TypedExpr::LocalGet { slot, .. } = func.as_ref() {
            if state.generics.contains_key(slot) {
                Some(*slot)
            } else {
                state.aliases.get(slot).copied()
            }
        } else {
            None
        }
    } else {
        None
    };

    // CAPTURE-LESS-LAMBDA INLINE (the zero-box win, ADR-044): if the callee is a thin
    // intrinsic-combinator wrapper (`map`/`filter`/`reduce` = `lin_map`/… forwarding its params) AND
    // the callback argument is a capture-less LITERAL lambda, inline the wrapper at THIS call site —
    // rewriting the call to a direct `lin_map(arr, <lambda>)` — so the literal lambda becomes visible
    // to the intrinsic's IR lowering (`lower_map`/…), which inlines the lambda body straight into the
    // loop with NO per-element box/unbox/closure-call. Done before the type-spec path so the inlined
    // direct-intrinsic form is what lowers. Capturing lambdas and stored-fn callbacks fall through to
    // the normal (closure-call) specialization path.
    if let Some(gslot) = callee_generic_slot {
        if try_inline_combinator_wrapper(expr, gslot, state) {
            return;
        }
        // SCALAR-SORT INLINE (the zero-box sort win): `sort(arr, cmp)` over a provably-flat scalar
        // array with a CAPTURE-LESS LITERAL comparator is rewritten to a `lin_sort(arr, cmp)`
        // intrinsic, whose IR lowering (`lower_sort`) emits an inline stable merge sort over the flat
        // unboxed buffer with the comparator body spliced in — NO per-comparison box/unbox/closure
        // call. Everything else (non-scalar arrays, capturing/stored comparators, `_sortJ`'s boxed
        // `Json` path) falls through to the normal generic specialization (the pure-Lin merge sort,
        // still correct). Done before the type-spec path so the inlined intrinsic form is what lowers.
        if try_inline_scalar_sort(expr, gslot, state) {
            return;
        }
    }

    // Handle a call to a generic function (directly by name, or through a `val f = id` alias).
    if let TypedExpr::Call { func, args, result_type, span, .. } = expr {
        if let TypedExpr::LocalGet { .. } = func.as_ref() {
            // STREAM RECEIVER: a generic combinator (`map`/`filter`/`reduce`/`while`) called with a
            // DEFINITELY-stream arg0 must NOT be native-specialized to its eager array body. Leave
            // the call as a plain Named import call so the IR lowerer (`lower_call`) redirects it to
            // the lazy `lin_stream_*` backend (mirrors the inline-bail above). Skipping the generic
            // path keeps the original import-binding `func` slot intact for that redirect.
            let arg0_is_stream = args.first().map(|a| matches!(a.ty(), Type::Stream(_))).unwrap_or(false);
            let generic_slot = if arg0_is_stream { None } else { callee_generic_slot };
            if let Some(gslot) = generic_slot {
                let g = &state.generics[&gslot];
                if let TypedExpr::Function { params, ret_type, body, .. } = &g.func {
                    let params = params.clone();
                    let ret_type = ret_type.clone();
                    let body = (**body).clone();
                    // Unify the generic signature against the concrete call types.
                    let mut subs: HashMap<u32, Type> = HashMap::new();
                    for (p, a) in params.iter().zip(args.iter()) {
                        collect_subs(&p.ty, &a.ty(), &mut subs);
                    }
                    collect_subs(&ret_type, result_type, &mut subs);

                    // ITERABLE-UNION over a source with NO usable static element type. A combinator
                    // param of the form `T[] | Iterator | Stream` (the `for`/`while`/`map`/… iterable
                    // union) applied to an opaque `Iterator`/`Stream`, a dynamic `Json` value, or an
                    // empty `Never[]` literal carries no concrete element, so its element TypeVar `T`
                    // is left unbound or bound to a non-concrete/garbage type. Such a source drives the
                    // combinator through the TAGGED runtime path (`lin_for`/…), the element arriving
                    // BOXED — so default `T` to the `Json` wildcard. Without this, the call either
                    // errors "cannot infer" (the only-type-param `for`/`while` over an opaque iterator)
                    // or binds `T` to a representation that mismatches the boxed element (`for` over
                    // `if … else []`, a `Json` value backed at runtime by a tagged array → an
                    // `i8`-vs-`ptr` callback-ABI clash at codegen).
                    for (p, a) in params.iter().zip(args.iter()) {
                        if let Some(elem_id) = iterable_union_elem_tv(&p.ty) {
                            let arg_ty = a.ty();
                            let arg_no_elem = matches!(arg_ty, Type::Iterator(_) | Type::Stream(_) | Type::TypeVar(_))
                                || matches!(&arg_ty, Type::Array(e) if matches!(**e, Type::Never));
                            let unresolved = match subs.get(&elem_id) {
                                None => true,
                                // `Never` (an empty `[]` literal element) and any non-concrete
                                // TypeVar are both "no usable element type" → take the Json default.
                                Some(Type::Never) => true,
                                Some(t) => mentions_generic_tv(t),
                            };
                            if arg_no_elem && unresolved {
                                subs.insert(elem_id, Type::TypeVar(u32::MAX));
                            }
                        }
                    }

                    // A genuinely-`Json` (wildcard) NON-FIRST argument flowing into a bare-TypeVar
                    // param that an earlier CONTAINER argument pinned to a concrete type
                    // (`push(uint8Arr, jsonVal)`, `push(out: Field[], field["bytes"]…)`) must REBIND
                    // that param's TypeVar to the Json wildcard, so the call monomorphizes at the
                    // DYNAMIC `$Json` representation. A `$Json` push routes through `lin_push_dyn`,
                    // which converts the boxed element into the array's runtime element slot at
                    // RUNTIME — the representation the non-generic `push` used. Keeping the concrete
                    // binding instead forces the scalar-param body to receive a raw boxed Json
                    // pointer it then `box_value`s as a scalar (`zext ptr` → codegen verifier error).
                    // Mirrors lin-check's `infer_call`/`infer_dot_call` Json-item rebind. First arg is
                    // the container and keeps its concrete element binding.
                    for (i, p) in params.iter().enumerate() {
                        if i == 0 { continue; }
                        if let Type::TypeVar(id) = &p.ty {
                            if *id != u32::MAX {
                                // A bare-TypeVar item type — the Json wildcard `MAX` OR a leftover
                                // unsolved checker inference var (e.g. `src[i]` indexing a `Json` is
                                // typed `TypeVar(2)`) — is a DYNAMIC value with no concrete scalar
                                // representation. It cannot be soundly stored into a concrete-scalar
                                // element slot, so route the push dynamically.
                                let item_is_json = args.get(i)
                                    .map(|a| matches!(a.ty(), Type::TypeVar(_)))
                                    .unwrap_or(false);
                                let bound_concrete = subs.get(id).map(|b| !b.contains_type_var()).unwrap_or(false);
                                if item_is_json && bound_concrete {
                                    subs.insert(*id, Type::TypeVar(u32::MAX));
                                }
                            }
                        }
                    }

                    // IN-PLACE RECEIVER MUTATOR over a CONTAINER-STORED array (silent data-loss
                    // fix): `push(obj[k], rec)` / `set(m[k], i, rec)` where `rec`'s record type is
                    // PACKABLE pins `T` to the packed-sealed element via the `item` arg, which would
                    // select the `push$Obj_…`/packed specialization. But the receiver `obj[k]` reads a
                    // BOXED tagged array out of the container, so the packed specialization's arg-coercion
                    // MATERIALIZES a fresh detached packed buffer (`sealed_array_project_from`), the push
                    // mutates the copy, and the array still stored in the container is never written back
                    // (`length(obj[k])` re-reads the empty original). REBIND every packable-sealed binding
                    // to the Json wildcard so the call specializes at the boxed `$Json` representation
                    // (`lin_push_dyn` / `lin_array_set`), mutating the REAL stored array — the SAME path
                    // that already makes the direct-`Json`-receiver `push(arr, rec)` case correct. Only
                    // fires for an in-place-mutator receiver that is a container index read yielding a
                    // boxed array; a typed `Pt[]` local/param keeps its packed fast path untouched.
                    if receiver_mutator_over_boxed_indexed_array(&g_name(state, gslot), args) {
                        for v in subs.values_mut() {
                            if mentions_sealed(v) {
                                *v = Type::TypeVar(u32::MAX);
                            }
                        }
                    }

                    // GAP 2 SAFETY: a quantified type param may be bound to a type that still
                    // mentions a NON-CONCRETE TypeVar — either the `u32::MAX` Json wildcard (a
                    // `Json` argument, see Gap 1) or a leftover/unsolved checker inference var
                    // (e.g. `TypeVar(44)`, id < GENERIC_TV_BASE). Materializing a specialization
                    // keyed on such a value would read/allocate the array at a BOGUS element type
                    // (`$T44` / `$T4294967295` garbage monomorph → runtime capacity overflow /
                    // heap corruption). The MAIN-module path historically tolerated this only
                    // because such cases rarely arose; the IMPORT path (a stdlib fn calling a
                    // sibling generic on its own `Json` param) hits it routinely. Resolve EVERY
                    // non-concrete TypeVar (any id) to the Json wildcard, producing a tagged
                    // `$Json` monomorph that is representation-consistent and correct.
                    // A quantified param `U` whose return position is determined by a LAMBDA arg's
                    // body — i.e. `U` appears as the RETURN of a function-typed param (`f: (T)=>U`)
                    // — can be left self-bound (`U -> TypeVar(U)`) when that body's inferred type is
                    // dynamic (the `Json` wildcard or an unresolved/index-derived TypeVar):
                    // bidirectional checking records the lambda's return as the expected `U` rather
                    // than the dynamic type, so the self-binding is a no-op that hides a `Json`
                    // result (e.g. `map(arr, x => x + i)` where `i` is a `Json` `for`-lambda param
                    // so `x + i` is `Json` — the RAPTOR #5 cascade). Resolve such ids to `Json` so
                    // the call materialises a correct tagged `$Json` monomorph instead of erroring
                    // as "cannot infer". A genuinely uninferrable param (e.g. `<T>(): T => 0` called
                    // bare) is NOT a lambda-return param, so it still errors below.
                    let lambda_return_ids = function_param_return_tv_ids(&params);
                    // PHANTOM RETURN PARAMS: a quantified id that appears ONLY nested inside the
                    // return type (e.g. the `E` of `ok = <T,E>(v: T): Result<T,E>`, which lives
                    // exclusively in the un-constructed `failure` arm `{ error: E }` of the result
                    // union) is determined by nothing at the call — no argument carries it and the
                    // value built never inhabits that arm. The union-arm matching binds it to ITSELF
                    // (`E -> TypeVar(E)`), which would trip the "cannot infer" error. Such a param is
                    // a representation-irrelevant phantom: erase it to the `$Json` wildcard exactly
                    // like a `Json`-bodied lambda-return param. This is NOT the genuinely-uninferrable
                    // `mk = <T>(): T => 0` case — there `T` IS the BARE return type (a top-level
                    // occurrence), so it is excluded below and still errors.
                    let phantom_return_ids = phantom_return_param_ids(&params, &ret_type);
                    for (id, v) in subs.iter_mut() {
                        let is_self_bound = matches!(v, Type::TypeVar(vid) if *vid == *id);
                        if is_self_bound
                            && (lambda_return_ids.contains(id) || phantom_return_ids.contains(id))
                        {
                            *v = Type::TypeVar(u32::MAX);
                        } else {
                            *v = erase_nonconcrete_typevars(v);
                        }
                    }

                    // Fully instantiated ⇔ every quantified id has a (now Json-erased) binding
                    // that no longer mentions a quantified generic TypeVar AND nothing is left
                    // unconstrained.
                    let all_quantified = subs
                        .keys()
                        .all(|id| *id >= GENERIC_TV_BASE && *id != u32::MAX);
                    let fully_concrete = !subs.is_empty()
                        && all_quantified
                        && subs.values().all(|t| !mentions_generic_tv(t));

                    // SEALED-RECORD BOUNDARY (Problem A / Stage 3b): some generic combinators are
                    // UNSOUND when native-specialized at a packed sealed element type — their body
                    // builds a `T`-typed merge/result buffer (`arrayAllocateFilled(n, arr[0])`) or
                    // stores/returns whole `T` elements, then reads/writes them through the boxed
                    // `Object[]`/`Json` machinery (`set`/`lin_array_get_tagged`/`_keyedPairs`/`_sortJ`),
                    // a boxed-vs-packed mismatch → silent corruption / misaligned deref (verified on
                    // master: `sort` → `7 7 7 7`, `sortBy` → segfault; also `minBy`/`maxBy`/`partition`/
                    // `reverse`/`unique`/`take`/`drop`). Route THOSE through the type-erased boxed
                    // fallback, which materializes the sealed array to its boxed view at the arg boundary
                    // (`box_value`) and re-coerces the boxed `T`-containing result back to the sealed
                    // representation (the sealed-array / nested-array Coerce arms). Combinators that only
                    // PROJECT each element through a callback to a DIFFERENT-typed result (`map`→`U[]`,
                    // `reduce`→`U`, `scan`→`U[]`, `find`, `some`, `every`, `flatMap`, `groupBy`/`countBy`)
                    // read the packed element soundly in their native spec AND keep a result the boxed
                    // re-coerce could not reconstruct (a flat-scalar `U[]`, a `{String: …}` map) — they
                    // are NOT routed. The gate is conservative (correctness fallback); it only fires for
                    // a genuinely-packed sealed arg AND a known-unsound combinator name.
                    let sealed_arg = subs.values().any(mentions_sealed);
                    let unsound_combinator = combinator_unsound_over_sealed(&g_name(state, gslot));

                    // A cross-module generic whose body reads a top-level `var` GLOBAL of its origin
                    // module (e.g. `std/random`'s `pick`/`shuffled` over the global `g: Rng`) cannot
                    // be native-specialized: the spec is cloned into THIS module, where that global
                    // is out of scope and the reference is dropped (an arg to a call vanishes →
                    // codegen arity error). Route to the boxed (type-erased) original, which stays in
                    // the origin module where the global resolves.
                    let refs_origin_global = state.generics.get(&gslot)
                        .and_then(|gf| gf.origin.as_ref())
                        .and_then(|p| state.imports.get(p).map(|m| (p.clone(), m)))
                        .map(|(_, m)| body_refs_origin_global_var(&body, m))
                        .unwrap_or(false);

                    if fully_concrete && refs_origin_global {
                        // Boxed fallback: keep the origin-module body (global in scope), share one copy.
                        state.boxed_fallback_used.insert(gslot);
                        boxed_fallback_call(expr, gslot, &params, &ret_type, state);
                    } else if fully_concrete && sealed_arg && unsound_combinator {
                        // Materialize-to-boxed boundary: keep the type-erased generic original and
                        // route this call through it. `box_value` converts the sealed array/record
                        // to its boxed view at the arg boundary; the wrapping Coerce re-seals the
                        // result. (No specialization budget interaction — the boxed original is shared.)
                        state.boxed_fallback_used.insert(gslot);
                        boxed_fallback_call(expr, gslot, &params, &ret_type, state);
                    } else if fully_concrete {
                        // Sound to native-specialize (no sealed arg, or a sealed arg through a
                        // projection-style combinator that reads the packed element correctly).
                        // Respect the per-generic native-specialization budget.
                        let key = instantiation_key(gslot, &subs);
                        let known = state.specs.contains_key(&key);
                        let count = *state.per_generic_count.get(&gslot).unwrap_or(&0);
                        if known || count < state.budget {
                            let base_name = g_name(state, gslot);
                            let spec_slot = native_spec_slot(state, gslot, &base_name, key, subs.clone());
                            repoint_call_native(expr, &params, &ret_type, &body, &subs, spec_slot);
                        } else {
                            // Budget exceeded: fall back to one shared boxed copy of the original.
                            if state.boxed_fallback_used.insert(gslot) {
                                let name = g_name(state, gslot);
                                let budget = state.budget;
                                state.diagnostics.push(
                                    Diagnostic::warning(*span, format!(
                                        "generic function '{}' exceeded the specialization budget of {} distinct instantiations",
                                        name, budget
                                    ))
                                    .with_help("further instantiations are compiled as a single boxed (type-erased) copy — correct, but slower than a per-type specialization".to_string())
                                );
                            }
                            boxed_fallback_call(expr, gslot, &params, &ret_type, state);
                        }
                    } else if mentions_unconstrained(&subs, &params, &ret_type) {
                        // A type parameter is not pinned down by the arguments or the result type:
                        // we cannot pick a concrete monomorphization. This is a hard error rather
                        // than silently-wrong code.
                        let name = g_name(state, gslot);
                        state.diagnostics.push(
                            Diagnostic::error(*span, format!(
                                "cannot infer a concrete type for the type parameter(s) of generic function '{}' at this call",
                                name
                            ))
                            .with_help("annotate the argument(s) or the surrounding context so every type parameter is determined".to_string())
                        );
                        // Keep the original around so codegen still has a (boxed) definition.
                        state.boxed_fallback_used.insert(gslot);
                        boxed_fallback_call(expr, gslot, &params, &ret_type, state);
                    } else {
                        // No substitution at all (e.g. a generic used purely as a value here).
                        state.used_generic_slots.insert(gslot);
                    }
                }
            }
        }
    }
}

/// Name of the generic function for slot `gslot`.
fn g_name(state: &MonoState<'_>, gslot: usize) -> String {
    state.generics[&gslot].name.clone()
}

/// Mint (or look up) a native specialization for `gslot` at `subs`, returning its slot. New specs
/// bump the per-generic budget counter and are pushed onto the worklist for materialization.
fn native_spec_slot(
    state: &mut MonoState<'_>,
    gslot: usize,
    base_name: &str,
    key: (usize, Vec<(u32, String)>),
    subs: HashMap<u32, Type>,
) -> usize {
    if let Some(info) = state.specs.get(&key) {
        return info.slot;
    }
    let s = state.next_slot;
    state.next_slot += 1;
    let name = specialization_name(base_name, &subs);
    state.specs.insert(key.clone(), SpecInfo { generic_slot: gslot, slot: s, name, subs });
    *state.per_generic_count.entry(gslot).or_insert(0) += 1;
    state.worklist.push(key);
    s
}

/// Intrinsic-combinator wrappers whose callback body the IR lowering can inline (ADR-044). Each is
/// `lin_X(params…)` forwarding its parameters 1:1, so a call's existing args are already in the
/// intrinsic's argument order and need no reordering when we repoint the callee.
///
/// `lin_for`/`lin_while` are included so a `.for(body)` / `.while(pred)` call is rewritten to the
/// intrinsic at the call site — exposing the literal lambda to `lower_for`/`lower_while`'s inline
/// loop — INSTEAD of being native-specialized into a `for$Int32`/`while$Int32` wrapper that takes
/// the callback as an opaque closure parameter and dispatches it indirectly per element. (`for`/
/// `while` became generic in commit 9d6d2970, which incidentally routed them onto that slow
/// specialization path; this restores their inline dispatch.)
fn combinator_intrinsic(name: &str) -> bool {
    matches!(name, "lin_map" | "lin_filter" | "lin_reduce" | "lin_for" | "lin_while")
}

/// If `arg` is a bare reference to a direct-callable module function (see
/// `MonoState::direct_callable_fn_slots`) with a known `Function` type, eta-expand it IN PLACE into
/// the capture-less lambda `(p0, p1, …) => f(p0, p1, …)`. This is the rewrite that lets a bare-fn
/// combinator callback (`.map(square)`) take the same inline-intrinsic path as a literal lambda
/// (`.map(x => square(x))`): the resulting `Function` has no captures (its only free name is the
/// module symbol `f`), so `try_inline_combinator_wrapper`'s capture-less-lambda gate accepts it and
/// the call is routed through `lin_map`/… to `lower_*`, which inlines the body with a DIRECT call to
/// `f` — no closure alloc, no per-element indirect dispatch. Returns true if it rewrote `arg`.
///
/// Fresh param slots come from `state.next_slot` (the same allocator used for re-homing), so they
/// cannot collide with any real or re-homed slot. Fails safe (returns false, leaves `arg` untouched)
/// for anything that is not a bare direct-callable fn reference — an opaque `Function` parameter, a
/// stored closure, or a non-function value all keep the existing closure-call specialization path.
fn eta_expand_named_arg(arg: &mut TypedExpr, state: &mut MonoState<'_>) -> bool {
    let (slot, fn_ty, span) = match arg {
        TypedExpr::LocalGet { slot, ty, span } if state.direct_callable_fn_slots.contains(slot) => {
            (*slot, ty.clone(), *span)
        }
        _ => return false,
    };
    let (param_tys, ret) = match &fn_ty {
        Type::Function { params, ret, .. } => (params.clone(), (**ret).clone()),
        _ => return false,
    };
    let params: Vec<TypedParam> = param_tys
        .iter()
        .enumerate()
        .map(|(i, ty)| {
            let s = state.next_slot;
            state.next_slot += 1;
            TypedParam { slot: s, name: format!("__eta{}", i), ty: ty.clone(), default: None }
        })
        .collect();
    let call_args: Vec<TypedExpr> = params
        .iter()
        .map(|p| TypedExpr::LocalGet { slot: p.slot, ty: p.ty.clone(), span })
        .collect();
    let body = TypedExpr::Call {
        func: Box::new(TypedExpr::LocalGet { slot, ty: fn_ty, span }),
        args: call_args,
        result_type: ret.clone(),
        is_tail: false,
        partial: false,
        span,
    };
    *arg = TypedExpr::Function {
        name: None,
        params,
        body: Box::new(body),
        ret_type: ret,
        captures: vec![],
        span,
    };
    true
}

/// Try to inline a thin intrinsic-combinator wrapper call (`map`/`filter`/`reduce`) at the call
/// site when its callback argument is a CAPTURE-LESS LITERAL lambda. On success, repoints the call's
/// `func` at a (re-homed) intrinsic slot for `lin_map`/`lin_filter`/`lin_reduce` so the call lowers
/// straight through the intrinsic — exposing the literal lambda to `lower_map`/… which inlines its
/// body into the loop (zero per-element box/unbox/closure-call). Returns true if it rewrote the call.
///
/// A bare direct-callable function callback (`.map(square)`) is first eta-expanded to the equivalent
/// capture-less lambda by `eta_expand_named_arg`, so it qualifies for this same inline path.
///
/// Conditions (all required):
///   - the generic is a thin intrinsic wrapper for a combinator intrinsic (`thin_intrinsic_wrapper`);
///   - exactly one argument is a `TypedExpr::Function` with NO captures (a capture-less literal
///     lambda — a capturing lambda or a stored/passed `Function` value is NOT inlinable here and
///     must keep the closure path);
///   - the intrinsic's origin module is known (so its name is resolvable).
/// The wrapper forwards its params 1:1, so the call args map directly to the intrinsic args.
fn try_inline_combinator_wrapper(
    expr: &mut TypedExpr,
    gslot: usize,
    state: &mut MonoState<'_>,
) -> bool {
    let g = &state.generics[&gslot];
    // Resolve the intrinsic this wrapper forwards to. The wrapper's body is type-checked in its
    // ORIGIN module, so classify the intrinsic against that module's intrinsic map.
    let origin_module: Option<&TypedModule> = match &g.origin {
        Some(path) => state.imports.get(path),
        None => None,
    };
    let intrinsic = match origin_module {
        Some(m) => match thin_intrinsic_wrapper(m, &g.func) {
            Some(intr) if combinator_intrinsic(&intr) => intr,
            _ => return false,
        },
        // A locally-defined generic combinator (not the stdlib import case) — not inlined here.
        None => return false,
    };

    let TypedExpr::Call { args, .. } = expr else { return false };
    // A STREAM receiver (arg0) must NOT inline to `lin_map`/… (the eager array loop). Stream
    // combinator dispatch happens at the call site (`lower_call`), redirecting to the lazy
    // `lin_stream_*` backend. Bail so the call stays a Named import call the lowerer intercepts.
    if args.first().map(|a| matches!(a.ty(), Type::Stream(_))).unwrap_or(false) {
        return false;
    }
    // SEALED-RECORD ARRAY RECEIVER (Problem A / Stage 3b): `lin_filter` over a packed sealed-record
    // array (elem_tag 0xFE) PUSHES whole elements through `emit_index_loop`/`push_output` — which
    // read/store them via the boxed `Object[]` machinery (`lin_array_get_tagged`/per-element retain),
    // a misaligned read + garbage push for a packed struct (observed: a filtered `Pt[]` element's
    // fields came back as garbage). Bail `lin_filter` to the type-erased boxed-fallback path, which
    // materializes the array to its boxed view first (§3). `lin_map`/`lin_reduce` PROJECT each element
    // through the callback to a derived (typically scalar) value rather than passing the whole struct
    // to the output, so their inline element-field reads are sound over the packed representation and
    // keep the zero-box win; they are NOT bailed.
    if intrinsic == "lin_filter"
        && args.first().map(|a| mentions_sealed(&a.ty())).unwrap_or(false)
    {
        return false;
    }
    // ETA-EXPAND a bare direct-callable function callback (`.map(square)`) into the equivalent
    // capture-less lambda `(p…) => square(p…)`, so the capture-less-lambda gate below accepts it and
    // the call routes through `lin_map`/… → `lower_*` (inline body + DIRECT call to `square`), the
    // same fast path a literal lambda takes. The combinator wrapper forwards its params 1:1, so the
    // callback is always the LAST argument (`map(arr,f)`/`filter(arr,p)`/`reduce(arr,init,f)`). A
    // no-op for a literal lambda (already a `Function`) or an opaque fn-value (not direct-callable).
    if let Some(last) = args.last_mut() {
        eta_expand_named_arg(last, state);
    }
    // Exactly one capture-less literal-lambda argument qualifies.
    let mut lambda_args = 0;
    for a in args.iter() {
        if let TypedExpr::Function { captures, .. } = a {
            if captures.is_empty() {
                lambda_args += 1;
            } else {
                // A capturing literal lambda: do not inline (the loop body would need its captured
                // environment, which the closure path provides). Bail to the closure specialization.
                return false;
            }
        }
    }
    if lambda_args != 1 {
        return false;
    }

    // Mint (deduped by name) a fresh importer intrinsic slot for `lin_*` and repoint the callee.
    let key = (String::new(), intrinsic.clone());
    let slot = if let Some(&s) = state.rehome_intrinsic_cache.get(&key) {
        s
    } else {
        let fresh = state.next_slot;
        state.next_slot += 1;
        state.rehome_intrinsic_cache.insert(key, fresh);
        state.rehomed_intrinsics.insert(fresh, intrinsic.clone());
        fresh
    };

    if let TypedExpr::Call { func, .. } = expr {
        if let TypedExpr::LocalGet { slot: fslot, .. } = func.as_mut() {
            *fslot = slot;
        }
    }
    true
}

/// Try to redirect a `sort(arr, cmp)` call to the inline scalar-sort intrinsic (`lin_sort`). On
/// success, repoints the call's `func` at a (re-homed) `lin_sort` intrinsic slot so the call lowers
/// through `lower_sort`, which emits an inline stable merge sort over the flat unboxed buffer with
/// the comparator body spliced in. Returns true if it rewrote the call.
///
/// Conditions (all required — gated TIGHTLY, fails safe to the generic boxed merge-sort path):
///   - the generic is the `std/array` export named `sort`;
///   - exactly two args: `arr` whose static element type is a flat NUMERIC scalar (Int*/UInt*/Float*),
///     and a CAPTURE-LESS literal lambda comparator (`(T, T) => Int32`). A capturing/stored comparator
///     or a non-scalar (object/string/union/Json) array is NOT eligible and keeps the boxed path.
///
/// Soundness note: the buffers `lower_sort` allocates are FLAT scalar arrays (sound for a numeric
/// scalar `T`), and the comparator reads them flat — identical to the element representation the
/// existing `sort$T` specialization already assumes for a scalar `T`. The copy-IN from `arr` uses the
/// representation-agnostic tagged read (sound even for a `[]`+push array mistyped as flat).
fn try_inline_scalar_sort(
    expr: &mut TypedExpr,
    gslot: usize,
    state: &mut MonoState<'_>,
) -> bool {
    let g = &state.generics[&gslot];
    // Must be the std/array `sort` export (origin is an imported stdlib module).
    if g.name != "sort" || g.origin.is_none() {
        return false;
    }
    let TypedExpr::Call { args, .. } = expr else { return false };
    if args.len() != 2 {
        return false;
    }
    // arg0: a statically-known flat NUMERIC scalar array element type.
    let elem_ok = match args[0].ty() {
        Type::Array(t) | Type::Iterator(t) => t.is_flat_scalar(),
        _ => false,
    };
    if !elem_ok {
        return false;
    }
    // arg1: a capture-less literal lambda comparator (a capturing/stored comparator is NOT inlinable
    // here — the merge loop would need its captured environment, which the closure path provides).
    match &args[1] {
        TypedExpr::Function { captures, .. } if captures.is_empty() => {}
        _ => return false,
    }

    // Mint (deduped by name) a fresh importer intrinsic slot for `lin_sort` and repoint the callee.
    let key = (String::new(), "lin_sort".to_string());
    let slot = if let Some(&s) = state.rehome_intrinsic_cache.get(&key) {
        s
    } else {
        let fresh = state.next_slot;
        state.next_slot += 1;
        state.rehome_intrinsic_cache.insert(key, fresh);
        state.rehomed_intrinsics.insert(fresh, "lin_sort".to_string());
        fresh
    };

    if let TypedExpr::Call { func, .. } = expr {
        if let TypedExpr::LocalGet { slot: fslot, .. } = func.as_mut() {
            *fslot = slot;
        }
    }
    true
}

/// Repoint a generic Call at the native specialization slot, giving its `func` LocalGet the
/// concrete specialized function type so lowering resolves the unboxed ABI.
///
/// The specialization returns the CONCRETE return type (e.g. `reduce$Union_Int32` returns a native
/// `i32`). The checker, however, may have left the Call's `result_type` as the boxed/erased generic
/// return (the `U` TypeVar surfaced as `Json` in the surrounding context — e.g. `total = s` where
/// `total: Json`), and the surrounding lowering relies on the result having THAT representation. If
/// the concrete return and the original `result_type` differ in representation (scalar vs boxed
/// ptr), we set the Call's own `result_type` to the concrete type and wrap the whole Call in a
/// `Coerce { from: concrete, to: original }` so the boxed/unboxed handoff to the surrounding context
/// is explicit — mirroring `boxed_fallback_call`'s Coerce, but in the native (unboxed) direction.
/// Without this the closure/global that consumes the result would emit `ret i32`/`store i32` against
/// a `ptr` slot (a hard codegen type mismatch).
fn repoint_call_native(
    expr: &mut TypedExpr,
    params: &[TypedParam],
    ret_type: &Type,
    body: &TypedExpr,
    subs: &HashMap<u32, Type>,
    spec_slot: usize,
) {
    let concrete_params: Vec<Type> = params.iter().map(|p| subst_type(&p.ty, subs)).collect();
    let mut concrete_ret = subst_type(ret_type, subs);
    // ADR-014 (reversed) mixed-family fix: a bare `Number` return whose value comes from arithmetic
    // over two DISTINCT bounded vars (`(a:Number,b:Number)=>a+b`) had `ret_type` recorded as ONE of
    // those vars, so `subst_type` freezes it to the first family. The materialized spec actually
    // returns the WIDENED family (`function_tail_type` over the substituted body — same as the spec
    // function's own re-synced `ret_type`). Re-derive it here so the Call's recorded result type and
    // the spec's signature agree (otherwise the caller reads an `i32` slot the spec fills with a
    // `double`). Fires when the substituted return is numeric, OR (ADR-014 §Json) when it is the
    // Json wildcard: a `Json` argument binds the `Number` var to `u32::MAX`, so `subst_type` makes
    // `concrete_ret = Json` (a `ptr`) while the spec body's arithmetic re-widens to a native scalar.
    // Mirrors the spec function's own ret-type re-sync in `subst_expr`; the `repr_differs` Coerce
    // below then boxes the native result back to the Json the surrounding context expects.
    let ret_is_json_wildcard = matches!(concrete_ret, Type::TypeVar(id) if id == u32::MAX);
    if concrete_ret.is_numeric() || ret_is_json_wildcard {
        let mut sb = body.clone();
        subst_expr(&mut sb, subs);
        let body_ty = function_tail_type(&sb);
        if body_ty.is_numeric() {
            concrete_ret = body_ty;
        }
    }
    let required = params.iter().filter(|p| p.default.is_none()).count();
    let fn_ty = Type::Function {
        params: concrete_params,
        ret: Box::new(concrete_ret.clone()),
        required,
    };
    let TypedExpr::Call { func, result_type, .. } = expr else { return };
    if let TypedExpr::LocalGet { slot: fslot, ty, .. } = func.as_mut() {
        *fslot = spec_slot;
        *ty = fn_ty;
    }
    let original_result = result_type.clone();
    // The native spec produces `concrete_ret`. Make the Call node report that.
    *result_type = concrete_ret.clone();
    // If the surrounding context expected a different representation (the checker's erased result),
    // re-coerce the native result back to it.
    if repr_differs(&concrete_ret, &original_result) {
        let span = expr.span();
        let inner = std::mem::replace(expr, TypedExpr::NullLit(span));
        *expr = TypedExpr::Coerce {
            expr: Box::new(inner),
            from: concrete_ret,
            to: original_result,
            span,
        };
    }
}

/// True when two types differ in runtime representation such that a value of one must be
/// boxed/unboxed to be used as the other: one is a union/Json (boxed `TaggedVal*` / `u32::MAX`
/// wildcard or a `Union`), the other a concrete non-union type. (Mirrors `lin-ir`'s
/// `type_repr_differs`; kept local to avoid a cross-module dependency.)
fn repr_differs(a: &Type, b: &Type) -> bool {
    fn is_boxed(t: &Type) -> bool {
        matches!(t, Type::Union(_)) || matches!(t, Type::TypeVar(id) if *id == u32::MAX)
    }
    is_boxed(a) != is_boxed(b)
}

/// Rewrite `expr` (a generic Call) into a boxed/type-erased call to the kept generic original.
///
/// The call's `func` is repointed at the generic original's slot with the *generic* (TypeVar)
/// signature, so lowering boxes each concrete argument into the uniform boxed-ptr ABI the original
/// (with TypeVar params) expects, and the Direct call returns a boxed ptr. The whole call is then
/// wrapped in a `Coerce { from: <generic ret TypeVar>, to: <concrete result> }` so the boxed
/// result is unboxed back to the type the surrounding context expects. Correct, just not unboxed.
fn boxed_fallback_call(
    expr: &mut TypedExpr,
    gslot: usize,
    params: &[TypedParam],
    ret_type: &Type,
    _state: &mut MonoState<'_>,
) {
    let TypedExpr::Call { func, result_type, .. } = expr else { return };
    let concrete_result = result_type.clone();
    let required = params.iter().filter(|p| p.default.is_none()).count();
    // Give the func LocalGet the generic original's slot + generic signature so lowering uses the
    // boxed (TypeVar ⇒ ptr) ABI and boxes the args.
    let generic_fn_ty = Type::Function {
        params: params.iter().map(|p| p.ty.clone()).collect(),
        ret: Box::new(ret_type.clone()),
        required,
    };
    if let TypedExpr::LocalGet { slot: fslot, ty, .. } = func.as_mut() {
        *fslot = gslot;
        *ty = generic_fn_ty;
    }
    // The Direct call now yields the generic return type (a boxed ptr for a TypeVar). Make the
    // Call's own result_type match that so lowering reads a ptr, then unbox via Coerce.
    *result_type = ret_type.clone();
    let span = expr.span();
    let inner = std::mem::replace(expr, TypedExpr::NullLit(span));
    *expr = TypedExpr::Coerce {
        expr: Box::new(inner),
        from: ret_type.clone(),
        to: concrete_result,
        span,
    };
}

/// True if any of the generic function's type parameters (the quantified ids appearing in its
/// params/ret) is left unconstrained or unresolved by `subs` (no binding, or a binding that still
/// mentions a generic TypeVar). Such an instantiation cannot be made concrete.
fn mentions_unconstrained(
    subs: &HashMap<u32, Type>,
    params: &[TypedParam],
    ret_type: &Type,
) -> bool {
    let mut ids = std::collections::HashSet::new();
    for p in params {
        collect_quantified_ids(&p.ty, &mut ids);
    }
    collect_quantified_ids(ret_type, &mut ids);
    ids.iter().any(|id| match subs.get(id) {
        None => true,
        Some(t) => mentions_generic_tv(t),
    })
}

/// Collect the quantified generic TypeVar ids (≥ base, excluding the Json wildcard) that appear in
/// the RETURN position of any function-typed parameter — i.e. the `U` in an `f: (T) => U` param.
/// Such an id is determined by the body of the LAMBDA passed for that param, so a `Json`-bodied
/// lambda can legitimately leave it self-bound (see the call site for why).
fn function_param_return_tv_ids(params: &[TypedParam]) -> std::collections::HashSet<u32> {
    let mut out = std::collections::HashSet::new();
    for p in params {
        if let Type::Function { ret, .. } = &p.ty {
            collect_quantified_ids(ret, &mut out);
        }
    }
    out
}

/// Quantified generic ids that are PHANTOM return parameters: they appear EXCLUSIVELY nested inside
/// the return type (inside a union member / object field / container element) and NEVER as a bare
/// top-level return position NOR anywhere in a parameter type. Such an id is determined by nothing
/// at a call — no argument carries it, and the constructed value does not inhabit the arm it lives
/// in — so it can be soundly erased to the `$Json` wildcard rather than producing a spurious
/// "cannot infer" error (e.g. the `E` of `ok = <T,E>(v: T): Result<T,E>`).
///
/// The exclusions are what keep the genuinely-uninferrable case erroring: `mk = <T>(): T => 0` has
/// `T` as the BARE top-level return (caught by `ret_bare_top`), and `<T>(x: Int32): Int32 => …` with
/// an unused `T` never reaches here because `T` would not be nested in the return at all.
fn phantom_return_param_ids(
    params: &[TypedParam],
    ret_type: &Type,
) -> std::collections::HashSet<u32> {
    // Ids that appear ANYWHERE in a parameter type are determined by (or constrained against) an
    // argument — never phantom.
    let mut param_ids = std::collections::HashSet::new();
    for p in params {
        collect_quantified_ids(&p.ty, &mut param_ids);
    }
    // A bare top-level return TypeVar (the `T` of `(): T`) directly determines the result's runtime
    // representation, so it is NOT a phantom — keep it erroring when unconstrained.
    let mut ret_bare_top = std::collections::HashSet::new();
    if let Type::TypeVar(id) = ret_type {
        if *id >= GENERIC_TV_BASE && *id != u32::MAX {
            ret_bare_top.insert(*id);
        }
    }
    // Ids appearing nested anywhere in the return type.
    let mut ret_ids = std::collections::HashSet::new();
    collect_quantified_ids(ret_type, &mut ret_ids);

    ret_ids
        .into_iter()
        .filter(|id| !param_ids.contains(id) && !ret_bare_top.contains(id))
        .collect()
}

/// Collect every quantified generic TypeVar id (≥ base, excluding the Json wildcard) in `ty`.
fn collect_quantified_ids(ty: &Type, out: &mut std::collections::HashSet<u32>) {
    match ty {
        Type::TypeVar(id) if *id >= GENERIC_TV_BASE && *id != u32::MAX => {
            out.insert(*id);
        }
        Type::Array(t) | Type::Iterator(t) | Type::Shared(t) | Type::Stream(t) => collect_quantified_ids(t, out),
        Type::FixedArray(ts) | Type::Union(ts) => {
            ts.iter().for_each(|t| collect_quantified_ids(t, out))
        }
        Type::Object { fields, .. } => fields.values().for_each(|t| collect_quantified_ids(t, out)),
        Type::Function { params, ret, .. } => {
            params.iter().for_each(|t| collect_quantified_ids(t, out));
            collect_quantified_ids(ret, out);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Type substitution over a TypedExpr tree (used to build specialized bodies)
// ---------------------------------------------------------------------------

/// Substitute the quantified TypeVars in a `match`-arm pattern (`is T` / `has T`). Only the
/// type-bearing variants matter: `TypeCheck`/`TypeCheckDeep` carry the `is`-target type, which is
/// the one that was being dropped. The nested patterns (Object/Array/Binding) also carry types and
/// are recursed for completeness — a generic destructuring `match` arm would otherwise keep a stale
/// TypeVar on its bound slots.
fn subst_match_pattern(pat: &mut TypedMatchPattern, subs: &HashMap<u32, Type>) {
    match pat {
        TypedMatchPattern::Is(p) | TypedMatchPattern::Has(p) => subst_pattern(p, subs),
        TypedMatchPattern::Else => {}
    }
}

fn subst_pattern(pat: &mut TypedPattern, subs: &HashMap<u32, Type>) {
    match pat {
        TypedPattern::TypeCheck(ty, _) => *ty = subst_type(ty, subs),
        TypedPattern::TypeCheckDeep(ty, named_defs, _) => {
            *ty = subst_type(ty, subs);
            for (_, t) in named_defs.iter_mut() {
                *t = subst_type(t, subs);
            }
        }
        TypedPattern::Literal(e) => subst_expr(e, subs),
        TypedPattern::Object { fields, .. } => {
            for f in fields.iter_mut() {
                f.ty = subst_type(&f.ty, subs);
                if let Some(vp) = f.value_pattern.as_mut() {
                    subst_expr(vp, subs);
                }
            }
        }
        TypedPattern::Array { elements, .. } => {
            for e in elements.iter_mut() {
                subst_pattern(e, subs);
            }
        }
        TypedPattern::Binding(_, ty, _) => *ty = subst_type(ty, subs),
        TypedPattern::Wildcard(_) => {}
    }
}

fn subst_expr(expr: &mut TypedExpr, subs: &HashMap<u32, Type>) {
    match expr {
        TypedExpr::IntLit(_, ty, _)
        | TypedExpr::FloatLit(_, ty, _)
        | TypedExpr::StringLit(_, ty, _) => *ty = subst_type(ty, subs),
        TypedExpr::BoolLit(..) | TypedExpr::NullLit(..) => {}
        TypedExpr::LocalGet { ty, .. } | TypedExpr::LocalSet { ty, .. } => {
            *ty = subst_type(ty, subs);
        }
        TypedExpr::BinaryOp { result_type, .. } | TypedExpr::UnaryOp { result_type, .. } => {
            *result_type = subst_type(result_type, subs);
        }
        TypedExpr::Coerce { from, to, .. } => {
            *from = subst_type(from, subs);
            *to = subst_type(to, subs);
        }
        TypedExpr::Call { result_type, .. } => *result_type = subst_type(result_type, subs),
        TypedExpr::If { result_type, .. } => *result_type = subst_type(result_type, subs),
        TypedExpr::FromJson { target, result_type, .. } => {
            *target = subst_type(target, subs);
            *result_type = subst_type(result_type, subs);
        }
        TypedExpr::Match { result_type, arms, .. } => {
            *result_type = subst_type(result_type, subs);
            // The arm PATTERNS carry their own target types (e.g. `is T` → `TypeCheck(TypeVar(T))`).
            // `for_each_child_mut` only descends into arm guards/bodies, never the pattern type — so
            // without this an `is T` test inside a generic body keeps its TypeVar after specialization
            // and codegen's `type_tag_const` maps it to the 0xFF sentinel → the arm is silently dead.
            for arm in arms.iter_mut() {
                subst_match_pattern(&mut arm.pattern, subs);
            }
        }
        TypedExpr::Block { ty, .. } => *ty = subst_type(ty, subs),
        TypedExpr::Function { params, ret_type, captures, .. } => {
            for p in params.iter_mut() {
                p.ty = subst_type(&p.ty, subs);
                if let Some(d) = p.default.as_mut() { subst_expr(d, subs); }
            }
            *ret_type = subst_type(ret_type, subs);
            for c in captures.iter_mut() { c.ty = subst_type(&c.ty, subs); }
        }
        TypedExpr::MakeObject { ty, .. } | TypedExpr::MakeArray { ty, .. } => {
            *ty = subst_type(ty, subs);
        }
        TypedExpr::Index { result_type, .. } | TypedExpr::FieldGet { result_type, .. } => {
            *result_type = subst_type(result_type, subs);
        }
        TypedExpr::IndexSet { obj_ty, .. } => *obj_ty = subst_type(obj_ty, subs),
        TypedExpr::Is { pattern, .. } | TypedExpr::Has { pattern, .. } => {
            // `if v is T then …` (the standalone form). Same reason as the `Match` arms above:
            // the pattern's target type must be specialized or the `is T` tag check stays a dead
            // 0xFF comparison.
            subst_pattern(pattern, subs);
        }
        TypedExpr::StringInterp { .. } => {}
    }
    // Substitute the type fields carried on STATEMENTS inside a block. `for_each_child_mut` only
    // descends into a statement's value EXPRESSION, never its declared-type field — so without this
    // a `var acc: U = …` inside a generic body keeps its `ty: TypeVar(U)` after substitution. That
    // would make the lowered cell a boxed union while the (substituted) closure that captures it
    // reads it as the concrete type → a representation mismatch and a misaligned-pointer crash.
    if let TypedExpr::Block { stmts, .. } = expr {
        for s in stmts.iter_mut() {
            subst_stmt_types(s, subs);
        }
    }
    // Recurse into children to substitute nested types.
    for_each_child_mut(expr, &mut |c| subst_expr(c, subs));

    // ADR-014 (reversed) mixed-family fix: a `Number` arithmetic op's result type was recorded by
    // the checker as ONE of its bounded operand vars (e.g. `a + b` with `a: TypeVar(9001)`,
    // `b: TypeVar(9002)` stored `result_type = TypeVar(9001)`). Plain `subst_type` then freezes it
    // to the FIRST family (Int32) — but when the two vars bind to DIFFERENT families (`add(10,2.5)`
    // ⇒ {9001→Int32, 9002→Float64}) the value actually WIDENS to Float64, so the slot type must be
    // re-derived from the now-concrete operands via `widen_numeric` (exactly what a concrete
    // `(a:Int32,b:Float64)` param pair produces). Without this the result slot is `Int32` while
    // codegen emits a `double` → `lin_box_int32(double)` / `ret double` ABI mismatch (the historical
    // crash). Re-widen AFTER children are substituted so operand `.ty()` is concrete. Only touches
    // arithmetic ops with two concrete numeric operands; comparisons (Bool) and boxed/union operands
    // are left as substituted.
    if let TypedExpr::BinaryOp { op, left, right, result_type, .. } = expr {
        use lin_parse::ast::BinOp;
        if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod) {
            let lt = left.ty();
            let rt = right.ty();
            let is_json = |t: &Type| matches!(t, Type::TypeVar(id) if *id == u32::MAX);
            if lt.is_numeric() && rt.is_numeric() {
                if let Some(widened) = lin_check::widen::widen_numeric(&lt, &rt) {
                    *result_type = widened;
                }
            } else if lt.is_numeric() && is_json(&rt) {
                // ADR-014 (reversed) §Json: a `Number` operand bound to the Json wildcard. The IR
                // (`lower_expr` BinaryOp) unboxes the wildcard side to the CONCRETE operand's family
                // and emits a native scalar op — so the result is that concrete numeric family, not
                // the boxed `result_type` the checker recorded (the bounded var → Json). Mirror that
                // here so the slot type matches the value codegen produces (else `ret i32` vs a
                // declared `ptr`/boxed slot — the `triple$Json` mismatch).
                *result_type = lt;
            } else if rt.is_numeric() && is_json(&lt) {
                *result_type = rt;
            }
        }
    }

    // Same mixed-family fix, one level up: a function whose declared return is a `Number` var
    // (e.g. `(a:Number,b:Number)=>a+b`, `ret_type = TypeVar(9001)`) had `ret_type` frozen to the
    // first family by `subst_type`. After the body is substituted (and its tail arithmetic
    // re-widened above), the function's actual return is the body's tail type — re-sync `ret_type`
    // to it so the emitted function signature matches the value the body returns (no `ret double`
    // vs declared-`i32` mismatch). Fires when `subst_type` left the return either numeric (the
    // mixed-family case) OR as the Json wildcard (the ADR-014 §Json case below) AND the re-derived
    // body type is numeric — a structural or already-concrete-non-wildcard return is left exactly as
    // `subst_type` produced it.
    //
    // ADR-014 (reversed) §Json: a `Json` argument binds the `Number` param's bounded var to the Json
    // wildcard (`u32::MAX`), so `subst_type` makes the spec's `ret_type = Json` (a `ptr`). But the
    // body's arithmetic over the (Int32-unboxed) operand re-widens to a NATIVE scalar (e.g. `x*3` ⇒
    // `i32`) — so the function returns `i32` against a declared `ptr` (the `triple$Json` mismatch).
    // Re-syncing `ret_type` to the concrete body tail makes the signature honest; the call site's
    // `repoint_call_native` then sees the differing representation and re-coerces (boxes) the scalar
    // back to the Json the surrounding context expects.
    if let TypedExpr::Function { ret_type, body, .. } = expr {
        let ret_is_json_wildcard = matches!(ret_type, Type::TypeVar(id) if *id == u32::MAX);
        if ret_type.is_numeric() || ret_is_json_wildcard {
            let body_ty = function_tail_type(body);
            if body_ty.is_numeric() && body_ty != *ret_type {
                *ret_type = body_ty;
            }
        }
    }
}

/// The type a function body ultimately evaluates to: the trailing expression of a Block (skipping
/// statements), peeking through a transparent Coerce, otherwise the expression's own type. Used to
/// re-sync a `Number` function's return type to its (re-widened) body after substitution.
///
/// A Coerce normally reports its `to` type. But a `from == to` Coerce is a REPRESENTATION NO-OP that
/// codegen elides — passing the inner value through unchanged. After substitution this arises for a
/// `Number` body whose value flows through a `Coerce { from: numvar, to: numvar }` that both
/// substituted to the SAME type (e.g. the Json wildcard, ADR-014 §Json): codegen returns the inner
/// (re-widened) scalar, so the tail type must be the INNER expression's type, not the elided `to`.
fn function_tail_type(body: &TypedExpr) -> Type {
    match body {
        TypedExpr::Block { expr, .. } => function_tail_type(expr),
        TypedExpr::Coerce { expr, from, to, .. } if from == to => function_tail_type(expr),
        TypedExpr::Coerce { to, .. } => to.clone(),
        other => other.ty(),
    }
}

/// Substitute generic TypeVars in the declared-type fields of a statement (the value expression's
/// own types are handled by `subst_expr` recursing into it via `for_each_child_mut`).
fn subst_stmt_types(stmt: &mut TypedStmt, subs: &HashMap<u32, Type>) {
    match stmt {
        TypedStmt::Val { ty, .. } | TypedStmt::Var { ty, .. } => {
            *ty = subst_type(ty, subs);
        }
        TypedStmt::Destructure { obj_ty, fields, .. } => {
            *obj_ty = subst_type(obj_ty, subs);
            for (_, _, fty) in fields.iter_mut() {
                *fty = subst_type(fty, subs);
            }
        }
        TypedStmt::ArrayDestructure { elem_ty, elements, rest, .. } => {
            *elem_ty = subst_type(elem_ty, subs);
            for (_, _, ety) in elements.iter_mut() {
                *ety = subst_type(ety, subs);
            }
            if let Some((_, rty)) = rest {
                *rty = subst_type(rty, subs);
            }
        }
        TypedStmt::Expr(_) | TypedStmt::Import { .. } | TypedStmt::ForeignImport { .. } => {}
    }
}

// ---------------------------------------------------------------------------
// Generic child traversal
// ---------------------------------------------------------------------------

fn for_each_child(expr: &TypedExpr, f: &mut dyn FnMut(&TypedExpr)) {
    match expr {
        TypedExpr::BinaryOp { left, right, .. } => { f(left); f(right); }
        TypedExpr::UnaryOp { operand, .. } => f(operand),
        TypedExpr::Coerce { expr, .. } => f(expr),
        TypedExpr::LocalSet { value, .. } => f(value),
        TypedExpr::Call { func, args, .. } => { f(func); for a in args { f(a); } }
        TypedExpr::If { cond, then_br, else_br, .. } => { f(cond); f(then_br); f(else_br); }
        TypedExpr::FromJson { value, .. } => f(value),
        TypedExpr::Match { scrutinee, arms, .. } => {
            f(scrutinee);
            for arm in arms {
                if let Some(g) = &arm.guard { f(g); }
                f(&arm.body);
            }
        }
        TypedExpr::Block { stmts, expr, .. } => {
            for s in stmts { for_each_child_stmt(s, f); }
            f(expr);
        }
        TypedExpr::Function { body, .. } => f(body),
        TypedExpr::MakeObject { fields, spreads, .. } => {
            for (_, v) in fields { f(v); }
            for s in spreads { f(s); }
        }
        TypedExpr::MakeArray { elements, .. } => { for e in elements { f(e); } }
        TypedExpr::Index { object, key, .. } => { f(object); f(key); }
        TypedExpr::FieldGet { object, .. } => f(object),
        TypedExpr::IndexSet { object, key, value, .. } => { f(object); f(key); f(value); }
        TypedExpr::StringInterp { parts, .. } => {
            for p in parts { if let TypedStringPart::Expr(e) = p { f(e); } }
        }
        TypedExpr::Is { expr, .. } | TypedExpr::Has { expr, .. } => f(expr),
        _ => {}
    }
}

fn for_each_child_stmt(stmt: &TypedStmt, f: &mut dyn FnMut(&TypedExpr)) {
    match stmt {
        TypedStmt::Val { value, .. } | TypedStmt::Var { value, .. } => f(value),
        TypedStmt::Destructure { value, .. } | TypedStmt::ArrayDestructure { value, .. } => f(value),
        TypedStmt::Expr(e) => f(e),
        _ => {}
    }
}

fn for_each_child_mut(expr: &mut TypedExpr, f: &mut dyn FnMut(&mut TypedExpr)) {
    match expr {
        TypedExpr::BinaryOp { left, right, .. } => { f(left); f(right); }
        TypedExpr::UnaryOp { operand, .. } => f(operand),
        TypedExpr::Coerce { expr, .. } => f(expr),
        TypedExpr::LocalSet { value, .. } => f(value),
        TypedExpr::Call { func, args, .. } => { f(func); for a in args { f(a); } }
        TypedExpr::If { cond, then_br, else_br, .. } => { f(cond); f(then_br); f(else_br); }
        TypedExpr::FromJson { value, .. } => f(value),
        TypedExpr::Match { scrutinee, arms, .. } => {
            f(scrutinee);
            for arm in arms {
                if let Some(g) = arm.guard.as_mut() { f(g); }
                f(&mut arm.body);
            }
        }
        TypedExpr::Block { stmts, expr, .. } => {
            for s in stmts { for_each_child_stmt_mut(s, f); }
            f(expr);
        }
        TypedExpr::Function { params, body, .. } => {
            for p in params.iter_mut() { if let Some(d) = p.default.as_mut() { f(d); } }
            f(body);
        }
        TypedExpr::MakeObject { fields, spreads, .. } => {
            for (_, v) in fields { f(v); }
            for s in spreads { f(s); }
        }
        TypedExpr::MakeArray { elements, .. } => { for e in elements { f(e); } }
        TypedExpr::Index { object, key, .. } => { f(object); f(key); }
        TypedExpr::FieldGet { object, .. } => f(object),
        TypedExpr::IndexSet { object, key, value, .. } => { f(object); f(key); f(value); }
        TypedExpr::StringInterp { parts, .. } => {
            for p in parts.iter_mut() { if let TypedStringPart::Expr(e) = p { f(e); } }
        }
        TypedExpr::Is { expr, .. } | TypedExpr::Has { expr, .. } => f(expr),
        _ => {}
    }
}

fn for_each_child_stmt_mut(stmt: &mut TypedStmt, f: &mut dyn FnMut(&mut TypedExpr)) {
    match stmt {
        TypedStmt::Val { value, .. } | TypedStmt::Var { value, .. } => f(value),
        TypedStmt::Destructure { value, .. } | TypedStmt::ArrayDestructure { value, .. } => f(value),
        TypedStmt::Expr(e) => f(e),
        _ => {}
    }
}
