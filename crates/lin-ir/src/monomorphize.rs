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
//! generic calls during `lower_import_module` (`monomorphize_import`, which keeps all originals for
//! external importers). Passing a generic as a first-class value, and generic methods, remain
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
        Type::Array(t) | Type::Iterator(t) | Type::Shared(t) => mentions_generic_tv(t),
        Type::FixedArray(ts) | Type::Union(ts) => ts.iter().any(mentions_generic_tv),
        Type::Object(fields) => fields.values().any(mentions_generic_tv),
        Type::Function { params, ret, .. } => {
            params.iter().any(mentions_generic_tv) || mentions_generic_tv(ret)
        }
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
        Type::FixedArray(ts) => {
            Type::FixedArray(ts.iter().map(erase_nonconcrete_typevars).collect())
        }
        Type::Union(ts) => Type::Union(ts.iter().map(erase_nonconcrete_typevars).collect()),
        Type::Object(fields) => Type::Object(
            fields.iter().map(|(k, v)| (k.clone(), erase_nonconcrete_typevars(v))).collect(),
        ),
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

/// Monomorphize an IMPORTED module's own (single-module) generic sibling calls during its
/// `lower_import_module` compilation. Like `monomorphize`, but KEEPS every generic original (an
/// external importer that does not specialize a call still issues a boxed `Named` call to the
/// original `{module_key}_{name}` symbol, so it must remain defined). Cross-module generics
/// reachable from the import are not specialized here (the import has no `imports` map); those are
/// handled in the top-level importer via `monomorphize_with_imports`.
pub fn monomorphize_import(module: &mut TypedModule) -> Vec<Diagnostic> {
    let no_imports: HashMap<String, TypedModule> = HashMap::new();
    monomorphize_inner(module, &no_imports, true)
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
fn classify_origin_slot(origin: &TypedModule, origin_path: &str, slot: usize) -> Option<OriginRef> {
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
                        // not by `origin_path`. Re-home the reference to that source module.
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
    let origin_ref = classify_origin_slot(origin, origin_path, origin_slot)?;
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
                    for v in subs.values_mut() {
                        *v = erase_nonconcrete_typevars(v);
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
