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
//! Scope (Phase 0): single module only. Generic functions must be top-level `val` bindings called
//! *directly* by name (`identity(5)`). Passing a generic function as a first-class value, generic
//! methods, and cross-module/stdlib generics are deferred to later phases. When a module contains
//! no generic functions (the common case) this pass is a no-op and leaves the module byte-identical.

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
        Type::Array(t) | Type::Iterator(t) | Type::Shared(t) => mentions_generic_tv(t),
        Type::FixedArray(ts) | Type::Union(ts) => ts.iter().any(mentions_generic_tv),
        Type::Object(fields) => fields.values().any(mentions_generic_tv),
        Type::Function { params, ret, .. } => {
            params.iter().any(mentions_generic_tv) || mentions_generic_tv(ret)
        }
        _ => false,
    }
}

/// A top-level generic function discovered in the module.
struct GenericFn {
    name: String,
    /// The full `Function` TypedExpr (params/body/ret_type/captures/span).
    func: TypedExpr,
}

/// Substitute quantified TypeVars throughout a type.
fn subst_type(ty: &Type, subs: &HashMap<u32, Type>) -> Type {
    match ty {
        Type::TypeVar(id) => subs.get(id).cloned().unwrap_or_else(|| ty.clone()),
        Type::Array(t) => Type::Array(Box::new(subst_type(t, subs))),
        Type::Iterator(t) => Type::Iterator(Box::new(subst_type(t, subs))),
        Type::Shared(t) => Type::Shared(Box::new(subst_type(t, subs))),
        Type::FixedArray(ts) => Type::FixedArray(ts.iter().map(|t| subst_type(t, subs)).collect()),
        Type::Union(ts) => Type::Union(ts.iter().map(|t| subst_type(t, subs)).collect()),
        Type::Object(fields) => Type::Object(
            fields.iter().map(|(k, v)| (k.clone(), subst_type(v, subs))).collect(),
        ),
        Type::Function { params, ret, required } => Type::Function {
            params: params.iter().map(|p| subst_type(p, subs)).collect(),
            ret: Box::new(subst_type(ret, subs)),
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
        (Type::Iterator(p), Type::Iterator(a)) => collect_subs(p, a, subs),
        (Type::Shared(p), Type::Shared(a)) => collect_subs(p, a, subs),
        (Type::Object(pf), Type::Object(af)) => {
            for (k, pv) in pf {
                if let Some(av) = af.get(k) { collect_subs(pv, av, subs); }
            }
        }
        (Type::Function { params: pp, ret: pr, .. }, Type::Function { params: ap, ret: ar, .. }) => {
            for (p, a) in pp.iter().zip(ap.iter()) { collect_subs(p, a, subs); }
            collect_subs(pr, ar, subs);
        }
        _ => {}
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
        Type::Object(_) => "Object".into(),
        Type::Union(_) => "Union".into(),
        Type::Function { .. } => "Fn".into(),
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
    // 1. Discover top-level generic functions (slot -> GenericFn).
    let mut generics: HashMap<usize, GenericFn> = HashMap::new();
    for stmt in &module.statements {
        if let TypedStmt::Val { slot, name: Some(name), value, .. } = stmt {
            if let TypedExpr::Function { params, ret_type, .. } = value {
                let is_generic = params.iter().any(|p| mentions_generic_tv(&p.ty))
                    || mentions_generic_tv(ret_type);
                if is_generic {
                    generics.insert(*slot, GenericFn { name: name.clone(), func: value.clone() });
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

    let mut state = MonoState {
        generics,
        aliases,
        specs: HashMap::new(),
        worklist: Vec::new(),
        per_generic_count: HashMap::new(),
        boxed_fallback_used: std::collections::HashSet::new(),
        next_slot: max_slot(module) + 1,
        used_generic_slots: std::collections::HashSet::new(),
        diagnostics: Vec::new(),
        budget: specialization_budget(),
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
    while let Some(key) = state.worklist.pop() {
        let (generic_slot, spec_slot, spec_name, subs) = {
            let info = &state.specs[&key];
            (info.generic_slot, info.slot, info.name.clone(), info.subs.clone())
        };
        let g = &state.generics[&generic_slot];
        let mut func = g.func.clone();
        let span = g.func.span();
        subst_expr(&mut func, &subs);
        if let TypedExpr::Function { name, .. } = &mut func {
            *name = Some(spec_name.clone());
        }
        // Re-monomorphize calls inside the now-concrete body (worklist fixpoint).
        rewrite_expr(&mut func, &mut state);
        let ty = func.ty();
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
    let keep: std::collections::HashSet<usize> = state
        .used_generic_slots
        .union(&state.boxed_fallback_used)
        .copied()
        .collect();
    stmts.retain(|stmt| {
        if let TypedStmt::Val { slot, value: TypedExpr::Function { .. }, .. } = stmt {
            if generic_slots.contains(slot) {
                return keep.contains(slot);
            }
        }
        true
    });

    // Insert specializations after the originals. Order is immaterial — top-level function `val`s
    // are forward-declared by slot in lowering.
    stmts.extend(materialized);
    module.statements = stmts;
    state.diagnostics
}

/// Mutable working state threaded through the rewrite/worklist passes.
struct MonoState {
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

fn rewrite_stmt(stmt: &mut TypedStmt, state: &mut MonoState) {
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

fn rewrite_expr(expr: &mut TypedExpr, state: &mut MonoState) {
    // Recurse into children FIRST so any nested generic calls (e.g. in this call's arguments) are
    // rewritten before we handle this node. Doing it first also means that after we (possibly) wrap
    // a generic call in a `Coerce` for the boxed fallback, we do NOT re-descend into the wrapped
    // call — which would otherwise re-trigger the rewrite and loop forever.
    for_each_child_mut(expr, &mut |c| rewrite_expr(c, state));

    // Handle a call to a generic function (directly by name, or through a `val f = id` alias).
    if let TypedExpr::Call { func, args, result_type, span, .. } = expr {
        if let TypedExpr::LocalGet { slot, .. } = func.as_ref() {
            // Resolve the underlying generic slot (direct or via alias chain).
            let generic_slot = if state.generics.contains_key(slot) {
                Some(*slot)
            } else {
                state.aliases.get(slot).copied()
            };
            if let Some(gslot) = generic_slot {
                let g = &state.generics[&gslot];
                if let TypedExpr::Function { params, ret_type, .. } = &g.func {
                    let params = params.clone();
                    let ret_type = ret_type.clone();
                    // Unify the generic signature against the concrete call types.
                    let mut subs: HashMap<u32, Type> = HashMap::new();
                    for (p, a) in params.iter().zip(args.iter()) {
                        collect_subs(&p.ty, &a.ty(), &mut subs);
                    }
                    collect_subs(&ret_type, result_type, &mut subs);

                    // Fully instantiated ⇔ every quantified id has a concrete (no remaining
                    // generic TypeVar) binding AND nothing is left unconstrained.
                    let all_quantified = subs
                        .keys()
                        .all(|id| *id >= GENERIC_TV_BASE && *id != u32::MAX);
                    let fully_concrete = !subs.is_empty()
                        && all_quantified
                        && subs.values().all(|t| !mentions_generic_tv(t));

                    if fully_concrete {
                        // Respect the per-generic native-specialization budget.
                        let key = instantiation_key(gslot, &subs);
                        let known = state.specs.contains_key(&key);
                        let count = *state.per_generic_count.get(&gslot).unwrap_or(&0);
                        if known || count < state.budget {
                            let base_name = g_name(state, gslot);
                            let spec_slot = native_spec_slot(state, gslot, &base_name, key, subs.clone());
                            repoint_call_native(func, &params, &ret_type, &subs, spec_slot);
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
fn g_name(state: &MonoState, gslot: usize) -> String {
    state.generics[&gslot].name.clone()
}

/// Mint (or look up) a native specialization for `gslot` at `subs`, returning its slot. New specs
/// bump the per-generic budget counter and are pushed onto the worklist for materialization.
fn native_spec_slot(
    state: &mut MonoState,
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

/// Repoint a generic call's `func` LocalGet at the native specialization slot, giving it the
/// concrete specialized function type so lowering resolves the unboxed ABI.
fn repoint_call_native(
    func: &mut Box<TypedExpr>,
    params: &[TypedParam],
    ret_type: &Type,
    subs: &HashMap<u32, Type>,
    spec_slot: usize,
) {
    let concrete_params: Vec<Type> = params.iter().map(|p| subst_type(&p.ty, subs)).collect();
    let concrete_ret = subst_type(ret_type, subs);
    let required = params.iter().filter(|p| p.default.is_none()).count();
    let fn_ty = Type::Function {
        params: concrete_params,
        ret: Box::new(concrete_ret),
        required,
    };
    if let TypedExpr::LocalGet { slot: fslot, ty, .. } = func.as_mut() {
        *fslot = spec_slot;
        *ty = fn_ty;
    }
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
    _state: &mut MonoState,
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

/// Collect every quantified generic TypeVar id (≥ base, excluding the Json wildcard) in `ty`.
fn collect_quantified_ids(ty: &Type, out: &mut std::collections::HashSet<u32>) {
    match ty {
        Type::TypeVar(id) if *id >= GENERIC_TV_BASE && *id != u32::MAX => {
            out.insert(*id);
        }
        Type::Array(t) | Type::Iterator(t) | Type::Shared(t) => collect_quantified_ids(t, out),
        Type::FixedArray(ts) | Type::Union(ts) => {
            ts.iter().for_each(|t| collect_quantified_ids(t, out))
        }
        Type::Object(fields) => fields.values().for_each(|t| collect_quantified_ids(t, out)),
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
        TypedExpr::Match { result_type, .. } => *result_type = subst_type(result_type, subs),
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
        TypedExpr::StringInterp { .. } | TypedExpr::Is { .. } | TypedExpr::Has { .. } => {}
    }
    // Recurse into children to substitute nested types.
    for_each_child_mut(expr, &mut |c| subst_expr(c, subs));
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
