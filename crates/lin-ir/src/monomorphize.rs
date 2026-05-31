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
/// Returns the module unchanged when it contains no generic functions.
pub fn monomorphize(module: &mut TypedModule) {
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
        return; // No-op for ordinary modules.
    }

    // Fresh slot allocator: start above the highest slot currently in use.
    let mut next_slot = max_slot(module) + 1;

    // 2. Walk the whole module, rewriting calls to generic functions and queuing
    //    specializations. `specs` dedups by instantiation key; `new_vals` accumulates the
    //    specialized top-level `Val`s to append.
    let mut specs: HashMap<(usize, Vec<(u32, String)>), SpecInfo> = HashMap::new();
    let mut used_generic_slots: std::collections::HashSet<usize> = std::collections::HashSet::new();

    let mut stmts = std::mem::take(&mut module.statements);
    for stmt in &mut stmts {
        rewrite_stmt(stmt, &generics, &mut specs, &mut next_slot, &mut used_generic_slots);
    }

    // 3. Materialize each specialization as a concrete top-level Val with the quantified
    //    TypeVars substituted throughout its body.
    let mut new_vals: Vec<TypedStmt> = Vec::new();
    for (_, spec) in specs.iter() {
        let g = &generics[&spec.generic_slot];
        let mut func = g.func.clone();
        subst_expr(&mut func, &spec.subs);
        if let TypedExpr::Function { name, .. } = &mut func {
            *name = Some(spec.name.clone());
        }
        let ty = func.ty();
        new_vals.push(TypedStmt::Val {
            slot: spec.slot,
            name: Some(spec.name.clone()),
            value: func,
            ty,
            span: g.func.span(),
        });
    }
    // Deterministic order so codegen/IR output is stable across runs.
    new_vals.sort_by(|a, b| {
        let sa = if let TypedStmt::Val { slot, .. } = a { *slot } else { 0 };
        let sb = if let TypedStmt::Val { slot, .. } = b { *slot } else { 0 };
        sa.cmp(&sb)
    });

    // 4. Drop now-unused generic originals (only direct calls were rewritten; an unused generic
    //    original left in place would force codegen to compile a boxed polymorphic stub). A
    //    generic still referenced (e.g. as a value) is kept as-is — out of Phase 0 scope.
    stmts.retain(|stmt| {
        if let TypedStmt::Val { slot, value: TypedExpr::Function { .. }, .. } = stmt {
            if generics.contains_key(slot) {
                return used_generic_slots.contains(slot);
            }
        }
        true
    });

    // Insert specializations just before `main`-level use. Appending keeps top-level ordering
    // valid: function `val`s are forward-declared by slot in lowering, so order is immaterial.
    stmts.extend(new_vals);
    module.statements = stmts;
}

struct SpecInfo {
    generic_slot: usize,
    slot: usize,
    name: String,
    subs: HashMap<u32, Type>,
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

fn rewrite_stmt(
    stmt: &mut TypedStmt,
    generics: &HashMap<usize, GenericFn>,
    specs: &mut HashMap<(usize, Vec<(u32, String)>), SpecInfo>,
    next_slot: &mut usize,
    used: &mut std::collections::HashSet<usize>,
) {
    match stmt {
        TypedStmt::Val { value, .. } | TypedStmt::Var { value, .. } => {
            rewrite_expr(value, generics, specs, next_slot, used);
        }
        TypedStmt::Destructure { value, .. } | TypedStmt::ArrayDestructure { value, .. } => {
            rewrite_expr(value, generics, specs, next_slot, used);
        }
        TypedStmt::Expr(e) => rewrite_expr(e, generics, specs, next_slot, used),
        TypedStmt::Import { .. } | TypedStmt::ForeignImport { .. } => {}
    }
}

fn rewrite_expr(
    expr: &mut TypedExpr,
    generics: &HashMap<usize, GenericFn>,
    specs: &mut HashMap<(usize, Vec<(u32, String)>), SpecInfo>,
    next_slot: &mut usize,
    used: &mut std::collections::HashSet<usize>,
) {
    // Rewrite a direct call to a generic function first (so we substitute before recursing into
    // its now-concrete args, which are already concrete anyway).
    if let TypedExpr::Call { func, args, result_type, .. } = expr {
        if let TypedExpr::LocalGet { slot, .. } = func.as_ref() {
            if let Some(g) = generics.get(slot) {
                if let TypedExpr::Function { params, ret_type, .. } = &g.func {
                    // Unify generic signature against the concrete call types.
                    let mut subs: HashMap<u32, Type> = HashMap::new();
                    for (p, a) in params.iter().zip(args.iter()) {
                        collect_subs(&p.ty, &a.ty(), &mut subs);
                    }
                    // The call's result_type is the checker's concrete instantiation of `ret`.
                    collect_subs(ret_type, result_type, &mut subs);

                    if subs.keys().all(|id| *id >= GENERIC_TV_BASE && *id != u32::MAX)
                        && !subs.is_empty()
                    {
                        let key = instantiation_key(*slot, &subs);
                        let spec_slot = if let Some(info) = specs.get(&key) {
                            info.slot
                        } else {
                            let s = *next_slot;
                            *next_slot += 1;
                            let name = specialization_name(&g.name, &subs);
                            specs.insert(key.clone(), SpecInfo {
                                generic_slot: *slot,
                                slot: s,
                                name,
                                subs: subs.clone(),
                            });
                            s
                        };
                        // Repoint the call's func LocalGet at the specialization. Give the
                        // LocalGet the concrete specialized function type so lowering resolves
                        // the right ABI; the global_fn_slots map keys on this slot.
                        let concrete_params: Vec<Type> =
                            params.iter().map(|p| subst_type(&p.ty, &subs)).collect();
                        let concrete_ret = subst_type(ret_type, &subs);
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
                    } else {
                        // Could not fully instantiate (e.g. generic-in-generic). Keep the
                        // original; this path is out of Phase 0 scope.
                        used.insert(*slot);
                    }
                }
            }
        }
    }

    // Recurse into children.
    for_each_child_mut(expr, &mut |c| rewrite_expr(c, generics, specs, next_slot, used));
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
