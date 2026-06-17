//! Lower TypedModule (tree-shaped) into LinModule (flat 3-address IR).
//!
//! Strategy:
//! - Walk typed IR recursively, emitting instructions into the current block.
//! - Control flow (if, match) creates new basic blocks; continuations resume in a fresh merge block.
//! - Nested functions are lifted to top-level LinFunctions.
//! - RC (Retain/Release) instructions are inserted pessimistically here; the rc_elide pass removes
//!   provably redundant pairs.

use std::collections::HashMap;

use lin_check::typed_ir::*;
use lin_check::types::Type;
use lin_parse::ast::BinOp;
use lin_common::Span;

use crate::ir::*;

mod rc;
mod coerce;
mod stmt;
mod expr;
mod call;
mod combinator;
mod match_;
mod func;

pub(crate) use rc::*;
pub use coerce::*;
pub(crate) use stmt::*;
pub(crate) use expr::*;
pub(crate) use call::*;
pub(crate) use combinator::*;
pub(crate) use match_::*;
pub(crate) use func::*;

/// Entry point: lower a TypedModule to a LinModule, plus any monomorphization diagnostics
/// (e.g. a generic call whose type parameters cannot be inferred). Diagnostics are empty for
/// ordinary modules and for well-formed generic programs.
pub fn lower_module(module: &TypedModule) -> (LinModule, Vec<lin_common::Diagnostic>) {
    let no_imports: HashMap<String, TypedModule> = HashMap::new();
    lower_module_with_imports(module, &no_imports)
}

/// Like `lower_module`, but with access to the importing program's already-typed imported modules.
/// This lets the monomorphizer specialize generic functions that live in an IMPORTED module
/// (including stdlib): a call to an imported generic is specialized in THIS module by cloning and
/// re-homing the imported body (see `monomorphize_with_imports`). Cross-module generic
/// instantiation is the whole point of the generics milestone — `range(0,n).map(x=>x*2)…` lowers
/// to a flat unboxed Int32 pipeline because `map`/`filter`/`reduce` specialize at `Int32` here.
pub fn lower_module_with_imports(
    module: &TypedModule,
    imports: &HashMap<String, TypedModule>,
) -> (LinModule, Vec<lin_common::Diagnostic>) {
    // Monomorphization: materialize concrete copies of generic functions (single-module
    // `identity$Int32` AND cross-module `std_array_map$…`) and route calls to them BEFORE lowering,
    // so the backend emits native unboxed scalars. The clone is taken only when the module actually
    // uses a generic function (its own or an imported one); ordinary modules skip it entirely and
    // lower byte-for-byte as before.
    let mut diagnostics = Vec::new();
    let owned: Option<TypedModule> = if crate::monomorphize::module_uses_generic(module, imports) {
        let mut m = module.clone();
        diagnostics = crate::monomorphize::monomorphize_with_imports(&mut m, imports);
        Some(m)
    } else {
        None
    };
    let module: &TypedModule = owned.as_ref().unwrap_or(module);

    let mut ctx = LowerCtx::new();
    ctx.intrinsics = module.intrinsics.clone();

    // Allocate the main function id FIRST so it is FuncId(0): codegen names the
    // FuncId(0) function "main", and everything else `__lin_fn_<id>` or its own name.
    let main_id = ctx.alloc_func_id();

    // Pre-collect global function slot assignments so cross-references work.
    let mut global_fn_slots: HashMap<usize, FuncId> = HashMap::new();
    for stmt in &module.statements {
        if let TypedStmt::Val {
            slot,
            value: TypedExpr::Function { name, .. },
            ..
        } = stmt
        {
            let fid = ctx.alloc_func_id();
            global_fn_slots.insert(*slot, fid);
            // WAVE D: tag a monomorphized `std/iter` `flatMap` specialization so the fusion engine's
            // `combinator_callee_name` can recognise its call. The spec's `name` is the original
            // export name (`flatMap`) or a mangled monomorph (`flatMap$Int32_…`); match the base.
            if let Some(n) = name {
                if combinator_base_name(n) == Some("flatMap") {
                    ctx.combinator_spec_slots.insert(*slot, "flatMap");
                }
            }
        }
    }
    ctx.global_fn_slots = global_fn_slots.clone();
    // D3b: record cross-module monomorphized spec slots so the call lowering can apply
    // array-element / container-insertion slot projection instead of anon-param sharing.
    ctx.spec_origin_slots = module.spec_origins.keys().copied().collect();

    // Pre-scan for `var` slots mutably captured by closures — these become heap cells.
    for stmt in &module.statements {
        collect_mutable_capture_slots_stmt(stmt, &mut ctx.mutable_cell_slots);
    }
    // Pre-scan for owning-typed `var` slots reassigned inside an `if`/`match` branch — these also
    // become heap cells (a plain SSA temp can't release-old / merge ownership across the join).
    {
        let mut owning_vars: HashMap<usize, Type> = HashMap::new();
        for stmt in &module.statements {
            collect_branch_reassigned_var_slots_stmt(
                stmt,
                false,
                &mut owning_vars,
                &mut ctx.branch_reassigned_var_slots,
            );
        }
    }

    // Top-level non-function vals AND top-level vars become module globals so closures can
    // read them (closures can't see `main`'s SSA temps). A top-level `var` additionally needs
    // its writes mirrored to the global — both at its definition and at every reassignment,
    // including reassignments inside closures (see TypedStmt::Var and LocalSet lowering).
    for stmt in &module.statements {
        match stmt {
            TypedStmt::Val { slot, value, ty, .. } if !matches!(value, TypedExpr::Function { .. }) => {
                ctx.global_val_slots.insert(*slot, ty.clone());
            }
            TypedStmt::Var { slot, ty, .. } => {
                ctx.global_val_slots.insert(*slot, ty.clone());
                ctx.global_var_slots.insert(*slot);
            }
            _ => {}
        }
    }

    // Pre-register default-argument adapters for EVERY top-level function before lowering any
    // body. A default-fill call dispatches to `default_adapters[(fid, k)]`; that entry must exist
    // by the time the call is lowered. Because a call site (in `main` or an earlier function) can
    // precede the callee's own `Val` statement — most acutely for monomorphized generic specs,
    // which are appended AFTER the call sites that reference them — registering lazily per-`Val`
    // (as `lower_stmt` also does, idempotently) is too late. Do it up front here.
    for stmt in &module.statements {
        if let TypedStmt::Val {
            slot,
            value: TypedExpr::Function { name: Some(name), params, ret_type, span: fn_span, .. },
            ..
        } = stmt
        {
            if let Some(&fid) = ctx.global_fn_slots.get(slot) {
                register_default_adapters(fid, *slot, name, params, ret_type, *fn_span, &mut ctx);
            }
        }
    }

    // Build the top-level "main" function containing module-level statements.
    let mut builder = FuncBuilder::new(main_id, None, vec![], false, Type::Int32, ctx.intrinsics.clone());

    builder.push_scope();
    for stmt in &module.statements {
        lower_stmt(stmt, &mut builder, &mut ctx);
    }
    // Release module-level owned temps (main exits, nothing to return).
    let sentinel = Temp(u32::MAX);
    builder.pop_scope_releasing(sentinel);

    // Return 0 from main.
    let zero = builder.const_temp(Const::Int(0, Type::Int32));
    builder.terminate(Terminator::Return(Some(zero)));
    builder.seal();

    let main_fn = builder.finish();
    ctx.functions.push(main_fn);

    // ADR-046: emit each test `replace` body under the import export's CANONICAL mangled symbol
    // (`{module_key}_{name}` for functions, `{sym}__val` for vals). The replaced export's own
    // module skipped emitting that symbol (see `lower_import_module_with_imports`'s
    // `replaced_exports` handling), so the single LLVM definition is the mock — every caller,
    // however the import path is spelled, resolves to it. Lowered AFTER `main` so any sibling
    // top-level fns the body references are already in `global_fn_slots`.
    lower_replacements(&module.replacements, &mut ctx);

    // Synthesize default-argument adapters queued during the main pass. Each lowers into
    // ctx.pending_functions (drained below).
    let adapters = std::mem::take(&mut ctx.pending_adapters);
    for spec in &adapters {
        lower_adapter(spec, &mut ctx);
    }

    // Compile nested functions collected during lowering.
    while let Some(pending) = ctx.pending_functions.pop() {
        ctx.functions.push(pending);
    }

    // Coverage: stamp each cross-module specialization's origin module path onto its LinFunction so
    // codegen attributes the spec's regions to the generic definition's source file. `spec_origins`
    // is keyed by the spec's top-level slot; `global_fn_slots` maps that to its FuncId.
    set_coverage_origins(&mut ctx.functions, &global_fn_slots, &module.spec_origins);

    let lin_module = LinModule {
        functions: ctx.functions,
        global_fn_slots,
        intrinsics: ctx.intrinsics,
        default_descriptors: ctx.default_descriptors,
    };
    (lin_module, diagnostics)
}

/// Stamp `LinFunction.coverage_origin` for cross-module monomorphized specializations. `spec_origins`
/// maps each specialization's top-level `val` slot to its origin module path; `global_fn_slots` maps
/// that slot to the FuncId of the emitted function. Shared by the main and import lowering paths.
fn set_coverage_origins(
    functions: &mut [LinFunction],
    global_fn_slots: &HashMap<usize, FuncId>,
    spec_origins: &HashMap<usize, String>,
) {
    if spec_origins.is_empty() {
        return;
    }
    let mut fid_origin: HashMap<FuncId, String> = HashMap::new();
    for (slot, origin) in spec_origins {
        if let Some(&fid) = global_fn_slots.get(slot) {
            fid_origin.insert(fid, origin.clone());
        }
    }
    for f in functions.iter_mut() {
        if let Some(origin) = fid_origin.get(&f.id) {
            f.coverage_origin = Some(origin.clone());
        }
    }
}

/// Lower an IMPORTED TypedModule to a LinModule for the IR pipeline.
///
/// Unlike `lower_module`, this does NOT emit a `main`; instead it lowers every top-level
/// exported binding so the importing module can resolve it by mangled symbol name:
///   - exported FUNCTIONS become `LinFunction`s named `{module_key}_{name}`, compiled with
///     their declared concrete signature (NOT the uniform boxed closure ABI) — the importer
///     resolves them via `CallTarget::Named` and builds the call from the declared types.
///   - exported NON-FUNCTION vals become zero-arg wrapper functions named
///     `{module_key}_{name}__val` that recompute and return the value on each call (the
///     importer reads them via a `Named` call). This mirrors the AST `register_import`
///     contract exactly, so importers are agnostic to which backend compiled the import.
///
/// `module_key` is `mangle_module_key(path)`. Sibling references (function→function) resolve
/// through `global_fn_slots` (Direct calls to the mangled symbols); cross-module imports,
/// foreign bindings, and intrinsics resolve exactly as in the main lowering.
pub fn lower_import_module(module: &TypedModule, module_key: &str) -> LinModule {
    let no_imports: HashMap<String, TypedModule> = HashMap::new();
    let no_replaced: std::collections::HashSet<String> = std::collections::HashSet::new();
    lower_import_module_with_imports(module, module_key, &no_imports, &no_replaced)
}

/// Like `lower_import_module`, but with the program's already-typed imported modules so an import
/// that ITSELF makes cross-module generic calls (e.g. `examples/report` → `std/array.reduce`) gets
/// those calls specialized here instead of falling to the boxed type-erased generic (which returns
/// `Json` and crashes a concrete-scalar use site). Mirrors the top-level `lower_module_with_imports`
/// monomorphization, but keeps every generic original (external importers may still call them
/// boxed). No-op (byte-identical lowering) when the module neither defines nor uses any generic.
pub fn lower_import_module_with_imports(
    module: &TypedModule,
    module_key: &str,
    imports: &HashMap<String, TypedModule>,
    replaced_exports: &std::collections::HashSet<String>,
) -> LinModule {
    // Monomorphize this import's generic calls: its OWN single-module sibling calls (e.g. stdlib
    // `sum`/`min` calling the now-generic `reduce`) AND any CROSS-MODULE generic call it makes into
    // its imports (resolved via `imports`). Without this, such a call gets a concrete `result_type`
    // from the checker but lowers to the boxed generic symbol — a representation mismatch that
    // crashes codegen. Generic originals are all KEPT (external importers may issue a boxed `Named`
    // call to them). No-op for a module that neither defines nor uses a generic (byte-identical).
    let owned: Option<TypedModule> = if crate::monomorphize::module_uses_generic(module, imports) {
        let mut m = module.clone();
        let _ = crate::monomorphize::monomorphize_import_with_imports(&mut m, imports);
        Some(m)
    } else {
        None
    };
    let module: &TypedModule = owned.as_ref().unwrap_or(module);

    let mut ctx = LowerCtx::new();
    ctx.intrinsics = module.intrinsics.clone();

    // Pre-assign a FuncId + mangled symbol name to every top-level function so sibling
    // Direct calls (and the importer's Named calls) resolve to the same symbol.
    let mut global_fn_slots: HashMap<usize, FuncId> = HashMap::new();
    let mut fn_names: HashMap<FuncId, String> = HashMap::new();
    for stmt in &module.statements {
        if let TypedStmt::Val {
            slot,
            value: TypedExpr::Function { name: Some(name), .. },
            ty,
            ..
        } = stmt
        {
            // ADR-046: a `replace`d export is NOT emitted here. Route its slot through
            // `import_fn_slots` to the canonical symbol so even THIS module's internal sibling
            // calls become `Named` calls to the symbol the main (test) module will define. The
            // single LLVM symbol then resolves to the mock for every caller. No FuncId / body.
            if replaced_exports.contains(name) {
                let sym = format!("{}_{}", module_key, name);
                let param_tys: Vec<Type> = match ty {
                    Type::Function { params, .. } => params.clone(),
                    _ => vec![],
                };
                ctx.import_fn_slots.insert(*slot, (sym, param_tys));
                continue;
            }
            let fid = ctx.alloc_func_id();
            global_fn_slots.insert(*slot, fid);
            fn_names.insert(fid, format!("{}_{}", module_key, name));
            // Stdlib-internal combinator call (e.g. `map`'s body calls the sibling `for`):
            // record the callback arg index so cells captured by the closure passed to it stay
            // freeable. Restricted to `std/iter`, which owns the combinator exports (ADR-077);
            // a Stream receiver bypasses this path entirely (the stream redirect returns before
            // the callback arg is lowered, so a lazily-retained stream callback never gets the
            // safe context — see the stream-combinator dispatch in `lower_call`).
            if module_key == "std_iter" {
                if let Some(idx) = safe_combinator_callback_index(name) {
                    ctx.safe_combinator_slots.insert(*slot, idx);
                }
            }
        }
    }
    ctx.global_fn_slots = global_fn_slots.clone();

    // Register every top-level NON-FUNCTION `val` so references to it from inside an exported
    // function body resolve to its zero-arg `{module_key}_{name}__val` wrapper (emitted below),
    // exactly as a *cross-module* importer would resolve the binding. An imported module has no
    // `main`, so unlike `lower_module` it cannot publish these to LLVM globals + module-init;
    // instead each read recomputes the value through its wrapper (cheap, and the same recompute
    // contract the importing module already relies on). This MUST run before lowering function
    // bodies (and before emitting the wrappers, whose initialisers may reference sibling vals).
    for stmt in &module.statements {
        if let TypedStmt::Val { slot, value, ty, name: Some(name), .. } = stmt {
            if matches!(value, TypedExpr::Function { .. }) { continue; }
            let wrapper = format!("{}_{}__val", module_key, name);
            ctx.import_val_slots.insert(*slot, (wrapper, ty.clone()));
        }
    }

    // Mutable-capture pre-scan (heap cells) — same as the main lowering.
    for stmt in &module.statements {
        collect_mutable_capture_slots_stmt(stmt, &mut ctx.mutable_cell_slots);
    }
    // Branch-reassigned owning-`var` pre-scan (heap cells) — same as the main lowering.
    {
        let mut owning_vars: HashMap<usize, Type> = HashMap::new();
        for stmt in &module.statements {
            collect_branch_reassigned_var_slots_stmt(
                stmt,
                false,
                &mut owning_vars,
                &mut ctx.branch_reassigned_var_slots,
            );
        }
    }

    // Top-level mutable `var`s in an imported module are genuine persistent shared state
    // (an exported function mutates the var; subsequent calls must see the update). Unlike a
    // non-function `val` — which has no state and is recomputed per read through a `__val`
    // wrapper — a `var` needs ONE module global, read/written via GlobalValGet/Set. Register
    // each top-level `var` slot here so reads/writes inside exported function bodies route
    // through the global (LocalGet/LocalSet check `global_var_slots`). Because an imported
    // module has no `main` to run the initialiser, we emit a once-guarded init function below
    // and call it on entry to every exported function/val wrapper. A var that is mutably
    // captured by a closure is already a heap cell (MakeCell) — that path takes priority in
    // `lower_stmt`, so do NOT also make it a global (the global would never be written and
    // reads would route to it incorrectly).
    let var_init_sym = format!("{}__var_init", module_key);
    let mut import_var_slots: Vec<usize> = Vec::new();
    for stmt in &module.statements {
        if let TypedStmt::Var { slot, ty, .. } = stmt {
            if ctx.mutable_cell_slots.contains(slot) {
                continue;
            }
            ctx.global_val_slots.insert(*slot, ty.clone());
            ctx.global_var_slots.insert(*slot);
            import_var_slots.push(*slot);
        }
    }

    // Resolve this module's OWN imports/foreign bindings into the slot maps so function
    // bodies can call them. We run the relevant arms of `lower_stmt` against a throwaway
    // builder (Import/ForeignImport emit no instructions — they only populate ctx).
    let mut resolver = FuncBuilder::new(
        ctx.alloc_func_id(), None, vec![], false, Type::Null, ctx.intrinsics.clone(),
    );
    for stmt in &module.statements {
        if matches!(stmt, TypedStmt::Import { .. } | TypedStmt::ForeignImport { .. }) {
            lower_stmt(stmt, &mut resolver, &mut ctx);
        }
    }

    // Pre-register default-fill adapters for every exported function before lowering any body, so
    // a default-fill call from an EARLIER function body to a LATER one (or to a monomorphized spec
    // appended after the call site) finds its adapter. Mirrors the main-module pre-scan; the
    // per-body call below is idempotent.
    for stmt in &module.statements {
        if let TypedStmt::Val {
            slot,
            value: TypedExpr::Function { params, ret_type, span: fn_span, .. },
            ..
        } = stmt
        {
            if let Some(&fid) = ctx.global_fn_slots.get(slot) {
                if let Some(real_name) = fn_names.get(&fid).cloned() {
                    register_default_adapters(fid, *slot, &real_name, params, ret_type, *fn_span, &mut ctx);
                }
            }
        }
    }

    // Emit the once-guarded var-init function for an imported module that has top-level
    // `var`s. It runs each top-level `var` statement (writing the var's module global via
    // GlobalValSet) exactly once across the whole program, guarded by a boolean global flag.
    // Every exported function/val-wrapper calls it on entry, so the var is initialised before
    // any reader. (A non-function `val` keeps its recompute-per-read `__val` wrapper — only
    // a `var`, which is persistent mutable state, needs this.)
    if !import_var_slots.is_empty() {
        // Reserved synthetic global slot for the init flag. Slot ids come from the checker and
        // are small indices, so usize::MAX never collides with a real binding. The backing
        // global is created per-compilation by handle; same-name globals across modules are
        // disambiguated by LLVM and accessed via the stored handle, so reuse is safe.
        const VAR_INIT_FLAG_SLOT: usize = usize::MAX;
        let init_fid = ctx.alloc_func_id();
        let mut ib = FuncBuilder::new(
            init_fid, Some(var_init_sym.clone()), vec![], false, Type::Null, ctx.intrinsics.clone(),
        );
        let do_init = ib.alloc_block("var_init_do");
        let done = ib.alloc_block("var_init_done");
        // if flag { goto done } else { goto do_init }
        let flag = ib.alloc_temp(Type::Bool);
        ib.emit(Instruction::GlobalValGet { dst: flag, slot: VAR_INIT_FLAG_SLOT, ty: Type::Bool, immutable: false });
        ib.terminate(Terminator::CondJump { cond: flag, then_block: done, else_block: do_init });
        // do_init: set flag, run var initialisers, jump done.
        ib.switch_to(do_init);
        let t = ib.const_temp(Const::Bool(true));
        ib.emit(Instruction::GlobalValSet { slot: VAR_INIT_FLAG_SLOT, value: t, ty: Type::Bool, immutable: false });
        ib.push_scope();
        for stmt in &module.statements {
            if matches!(stmt, TypedStmt::Var { .. }) {
                lower_stmt(stmt, &mut ib, &mut ctx);
            }
        }
        let sentinel = Temp(u32::MAX);
        ib.pop_scope_releasing(sentinel);
        ib.terminate(Terminator::Jump(done));
        // done: return.
        ib.switch_to(done);
        ib.terminate(Terminator::Return(None));
        ib.seal();
        ctx.functions.push(ib.finish());
    }

    // Lower each exported top-level function body under its forced mangled symbol name and
    // pre-assigned FuncId. We need a host builder to call `lower_function_expr_with_id`,
    // which appends the finished function to `ctx.pending_functions`.
    let mut host = FuncBuilder::new(
        ctx.alloc_func_id(), None, vec![], false, Type::Null, ctx.intrinsics.clone(),
    );
    host.push_scope();
    for stmt in &module.statements {
        if let TypedStmt::Val {
            slot,
            value: TypedExpr::Function { params, body, ret_type, captures, span: fn_span, .. },
            ..
        } = stmt
        {
            if let Some(&fid) = ctx.global_fn_slots.get(slot) {
                let mangled = fn_names.get(&fid).cloned();
                // Register default-fill adapters under the mangled export symbol, so importers
                // can issue Named calls to `{module_key}_{name}$default{k}`.
                if let Some(real_name) = mangled.as_deref() {
                    register_default_adapters(fid, *slot, real_name, params, ret_type, *fn_span, &mut ctx);
                }
                // Run the module's var-init guard on entry to this exported function (no-op
                // after the first call). Only set if the module has top-level vars.
                if !import_var_slots.is_empty() {
                    ctx.import_var_init_prologue = Some(var_init_sym.clone());
                }
                lower_function_expr_with_id(
                    Some(fid), None, mangled.as_deref(), params, body, ret_type, captures,
                    &mut host, &mut ctx,
                );
                ctx.import_var_init_prologue = None;
            }
        }
    }
    host.discard_scope();

    // Emit a zero-arg `{module_key}_{name}__val` wrapper for each non-function exported val.
    for stmt in &module.statements {
        if let TypedStmt::Val { value, ty, name: Some(name), .. } = stmt {
            if matches!(value, TypedExpr::Function { .. }) { continue; }
            // ADR-046: a `replace`d val's wrapper is emitted by the main (test) module instead;
            // skip the original body so the single `__val` symbol resolves to the mock.
            if replaced_exports.contains(name) { continue; }
            let fid = ctx.alloc_func_id();
            let wrapper_name = format!("{}_{}__val", module_key, name);
            let mut wb = FuncBuilder::new(
                fid, Some(wrapper_name), vec![], false, ty.clone(), ctx.intrinsics.clone(),
            );
            wb.push_scope();
            // A non-function exported val may read a sibling top-level `var`; ensure the
            // module's vars are initialised first (no-op after the first call anywhere).
            if !import_var_slots.is_empty() {
                let dst = wb.alloc_temp(Type::Null);
                wb.emit(Instruction::Call {
                    dst,
                    callee: CallTarget::Named(var_init_sym.clone()),
                    args: vec![],
                    ret_ty: Type::Null,
                });
            }
            let t = lower_expr(value, &mut wb, &mut ctx);
            let t = coerce_to_slot_type(t, &value.ty(), ty, &mut wb);
            // The wrapper hands ownership of the computed value to the caller; keep it.
            wb.pop_scope_releasing_keep(&[t]);
            if !wb.is_current_block_terminated() {
                if matches!(ty, Type::Null | Type::Never) {
                    wb.terminate(Terminator::Return(None));
                } else {
                    wb.terminate(Terminator::Return(Some(t)));
                }
            }
            wb.seal();
            ctx.functions.push(wb.finish());
        }
    }

    // Synthesize default-argument adapters for exported functions.
    let adapters = std::mem::take(&mut ctx.pending_adapters);
    for spec in &adapters {
        lower_adapter(spec, &mut ctx);
    }

    // Collect all lifted/nested functions produced during lowering.
    while let Some(pending) = ctx.pending_functions.pop() {
        ctx.functions.push(pending);
    }

    // Coverage: stamp cross-module specialization origins (this import may itself specialize a
    // generic from a further module, e.g. `examples/report` → `std/array.reduce`).
    set_coverage_origins(&mut ctx.functions, &global_fn_slots, &module.spec_origins);

    LinModule {
        functions: ctx.functions,
        global_fn_slots,
        intrinsics: ctx.intrinsics,
        default_descriptors: ctx.default_descriptors,
    }
}

// -------------------------------------------------------------------------
// Context shared across the whole module lowering
// -------------------------------------------------------------------------

pub(crate) struct LowerCtx {
    functions: Vec<LinFunction>,
    pending_functions: Vec<LinFunction>,
    func_counter: u32,
    intrinsics: HashMap<usize, String>,
    global_fn_slots: HashMap<usize, FuncId>,
    /// Import binding slots that resolve to a compiled function in the LLVM module.
    /// slot → (mangled LLVM symbol name e.g. `std_io_print`, declared param types).
    /// Imported modules are compiled through the IR pipeline (`compile_import_from_ir`), so
    /// the symbol already exists; the IR `CallTarget::Named` resolver looks it up by name.
    /// Param types drive arg boxing (concrete → Json param).
    import_fn_slots: HashMap<usize, (String, Vec<Type>)>,
    /// Import binding slots for non-function exported vals. slot → (val-wrapper symbol
    /// name `{module_key}_{name}__val`, value type). Reading the binding calls the
    /// zero-arg wrapper to compute the value.
    import_val_slots: HashMap<usize, (String, Type)>,
    /// `var` slots that are mutably captured by an inner closure. These are stored as
    /// heap cells (MakeCell) shared by reference; reads/writes go through CellGet/CellSet
    /// and closures capture the cell pointer (ADR-012).
    mutable_cell_slots: std::collections::HashSet<usize>,
    /// `var` slots of an OWNING (rc/union) type that are REASSIGNED inside conditional control
    /// flow (an `if`/`match` branch). A plain SSA temp cannot model release-old-on-overwrite and
    /// per-branch ownership across a join: the superseded initial value leaks on the taken branch
    /// and the slot can dangle / double-free. So — like a mutably-captured var — these are routed
    /// through a heap CELL (MakeCell/CellGet/CellSet), which RELEASES the old value on each write
    /// and reads the current value coherently after the join. Scalars and straight-line-only
    /// reassignments stay on the plain-SSA fast path. (Module-global vars handle this via the
    /// global slot and are excluded.)
    branch_reassigned_var_slots: std::collections::HashSet<usize>,
    /// Top-level non-function `val` slots (with their type). These are emitted as LLVM
    /// globals so closures — which can't see `main`'s SSA temps — can read them.
    global_val_slots: HashMap<usize, Type>,
    /// The subset of `global_val_slots` that are top-level `var`s (mutable). Reads of these
    /// MUST always go through `GlobalValGet` (never a cached local SSA temp), because a
    /// closure call may have mutated the global since the last local write. Writes go through
    /// `GlobalValSet`.
    global_var_slots: std::collections::HashSet<usize>,
    /// Default-argument adapters for top-level functions. `(real fid, arity k)` → adapter fid.
    /// The adapter takes the first `k` parameters, fills the remaining defaults, and tail-calls
    /// the real function. A non-partial call supplying `k < total` args is routed here.
    default_adapters: HashMap<(FuncId, usize), FuncId>,
    /// Adapter bodies queued for lowering after the main pass (see `AdapterSpec`).
    pending_adapters: Vec<AdapterSpec>,
    /// Real FuncId → default-argument descriptor (for the closure-value indirect path).
    default_descriptors: HashMap<FuncId, DefaultDescriptor>,
    /// Function slots that are KNOWN synchronous, non-retaining higher-order combinators
    /// (`for`/`while`/`map`/`filter`/`reduce`/`find`/`some`/`every`), mapped to the argument
    /// index of their callback parameter. A closure literal lowered as THAT argument is consumed
    /// synchronously and never retained/stored/returned, so heap cells it captures do not escape.
    /// Populated for: stdlib imports (matched by export name) and stdlib-internal calls (matched
    /// in `lower_import_module`). Used alongside the intrinsic combinators (`lin_for` etc.).
    safe_combinator_slots: HashMap<usize, usize>,
    /// Top-level function slots that are a monomorphized stdlib `flatMap` specialization
    /// (`std/iter`'s `flatMap$…`), mapped to the canonical combinator name `"flatMap"`. Unlike
    /// `map`/`filter`/`reduce` (thin intrinsic wrappers rewritten to `lin_*` slots by
    /// `try_inline_combinator_wrapper`, so they carry an intrinsic slot), `flatMap` is a genuine
    /// generic that monomorphizes to a top-level spec resolved via `global_fn_slots` — neither an
    /// intrinsic nor an import slot. `combinator_callee_name` consults this map so a `flatMap` stage
    /// is recognised by the fusion engine (Wave D). Populated in the top-level pre-scan by matching
    /// the spec's `name` against the `std_iter_flatMap`/`flatMap$…` shape.
    combinator_spec_slots: HashMap<usize, &'static str>,
    /// >0 while lowering an expression that is a SYNCHRONOUS, non-retained callback argument
    /// to a known consuming combinator (for/while/map/filter/reduce). A closure literal
    /// (`MakeClosure`) lowered while this is >0 is PROVABLY consumed-and-discarded by the
    /// combinator within the same function call — it is never bound, returned, or stored — so
    /// the heap cell(s) it captures do not escape and may be freed at the creating function's
    /// scope exit. When this is 0, any captured cell is conservatively marked escaping (left
    /// leaking). See `FreeCell` and the captured-cell escape analysis.
    safe_callback_depth: u32,
    /// PATH-1 in-place packed iteration: lambda param slots that are bound to a BORROWED packed
    /// sealed-array element view rather than a materialized struct. slot → (array_temp, index_temp,
    /// sealed element type). A `param["field"]` read on such a slot lowers to a const-offset
    /// `SealedArrayFieldGet` straight off the packed buffer (no per-element materialize, no boxed
    /// `lin_object_get`); any OTHER use of the param (passing it as a whole value, storing it)
    /// materializes the element on demand via `materialize_packed_elem_view`. Populated only inside
    /// the inline combinator fast paths over a `is_sealed_scalar_array` receiver, and cleared when
    /// the inlined body's scope exits.
    packed_elem_slots: HashMap<usize, (Temp, Temp, Type)>,
    /// When lowering an IMPORTED module that has top-level mutable `var`s, this holds the
    /// once-guarded var-init function symbol (`{module_key}__var_init`). Set just before a
    /// TOP-LEVEL EXPORTED function body (or `__val` wrapper) is lowered, so the body emits a
    /// call to it on entry — guaranteeing the module's vars are initialised before any
    /// exported entry point reads them. Cleared (taken) after one prologue is emitted so
    /// nested closures within the body do not re-run init.
    import_var_init_prologue: Option<String>,
    /// Slots of CROSS-MODULE generic monomorphization specs (e.g. `push$Obj_type_String` cloned
    /// from `std/array`). Populated from `module.spec_origins`. Used by D3b: when calling a
    /// cross-module spec with a wider unsealed object arg than the param type, project-copy the
    /// arg to the narrower slot type. This does NOT apply to local anon-param functions (D3a
    /// sharing takes priority for those).
    spec_origin_slots: std::collections::HashSet<usize>,
}

/// A default-fill adapter to be synthesized and lowered. `f@k` takes the first `k` parameters
/// of `f`, binds each remaining parameter to its default expression, then calls `f` with the
/// full argument list. Built as a synthetic `TypedExpr::Function` so it reuses the normal
/// function-lowering path (RC, coercion, chained/earlier-param default references).
pub(crate) struct AdapterSpec {
    adapter_fid: FuncId,
    symbol: String,
    /// Slot of the real function (resolved through `global_fn_slots` for the inner call).
    real_slot: usize,
    real_fn_ty: Type,
    /// All parameters of the real function, in order (carrying their defaults).
    params: Vec<TypedParam>,
    /// Number of leading parameters this adapter accepts; the rest are defaulted.
    arity: usize,
    ret_type: Type,
    span: Span,
}

impl LowerCtx {
    /// True if `slot` should be lowered as a heap CELL rather than a plain SSA temp: either it is
    /// mutably captured by a closure (ADR-012) or it is an owning-typed `var` reassigned inside a
    /// branch (release-old + post-join coherence). A top-level (module-global) var is excluded —
    /// it is handled through its module global slot, which is already join-coherent.
    fn slot_is_cell(&self, slot: usize) -> bool {
        self.mutable_cell_slots.contains(&slot)
            || (self.branch_reassigned_var_slots.contains(&slot) && !self.global_var_slots.contains(&slot))
    }

    fn new() -> Self {
        Self {
            functions: Vec::new(),
            pending_functions: Vec::new(),
            func_counter: 0,
            intrinsics: HashMap::new(),
            global_fn_slots: HashMap::new(),
            global_var_slots: std::collections::HashSet::new(),
            import_fn_slots: HashMap::new(),
            import_val_slots: HashMap::new(),
            mutable_cell_slots: std::collections::HashSet::new(),
            branch_reassigned_var_slots: std::collections::HashSet::new(),
            global_val_slots: HashMap::new(),
            default_adapters: HashMap::new(),
            pending_adapters: Vec::new(),
            default_descriptors: HashMap::new(),
            safe_combinator_slots: HashMap::new(),
            combinator_spec_slots: HashMap::new(),
            safe_callback_depth: 0,
            packed_elem_slots: HashMap::new(),
            import_var_init_prologue: None,
            spec_origin_slots: std::collections::HashSet::new(),
        }
    }

    fn alloc_func_id(&mut self) -> FuncId {
        let id = FuncId(self.func_counter);
        self.func_counter += 1;
        id
    }
}

// -------------------------------------------------------------------------
// Function builder — accumulates blocks for a single function being compiled
// -------------------------------------------------------------------------

pub(crate) struct FuncBuilder {
    id: FuncId,
    name: Option<String>,
    params: Vec<(Temp, Type)>,
    is_closure: bool,
    ret_ty: Type,
    blocks: Vec<BasicBlock>,
    current_block: BlockId,
    /// The source span attributed to instructions emitted right now. Threaded by the lowerer at
    /// statement/expression boundaries (`with_span`) and stamped onto every instruction by `emit`,
    /// so the codegen DWARF pass can attach statement-granularity `DILocation`s under `--debug`.
    /// Purely debug metadata — does not affect IR semantics or non-debug codegen.
    current_span: Option<lin_common::Span>,
    temp_count: u32,
    temp_types: HashMap<Temp, Type>,
    block_counter: u32,
    /// Lin slot → temp mapping for the current scope.
    slots: HashMap<usize, Temp>,
    intrinsic_slots: HashMap<usize, String>,
    /// Stack of owned-temp frames for scope-exit release.
    /// Each frame holds (temp, type) pairs for freshly-allocated heap values
    /// introduced in the current scope that must be released on exit.
    scope_owned: Vec<Vec<(Temp, Type)>>,
    /// Blocks that are dead continuations after a diverging TailCall. They carry a fresh
    /// temp so `lower_expr` can return one, but control never reaches them; they must not
    /// become phi predecessors of an enclosing if/match merge.
    diverged_blocks: std::collections::HashSet<BlockId>,
    /// Slots stored as heap cells (mutably-captured `var`s): slot → stored value type.
    /// `slots[slot]` holds the cell-pointer temp; LocalGet/LocalSet go through the cell.
    cell_slots: HashMap<usize, Type>,
    /// Captured-`var` heap cells (MakeCell) created in THIS function body, in creation order:
    /// (cell temp, stored value type, creation block). Candidates for scope-exit freeing. Only
    /// cells created in the ENTRY block (BlockId(0)) are freed — the entry block dominates every
    /// block, so the function-scope-exit block (where FreeCell is emitted) is guaranteed
    /// dominated by the MakeCell, satisfying LLVM SSA dominance. Cells created inside a
    /// conditional/loop branch (e.g. `_qsort`'s `var i` inside `if lo < hi`) are NOT in the
    /// entry block, would fail dominance at the merge exit, and are left leaking (sound).
    created_cells: Vec<(Temp, Type, BlockId)>,
    /// The subset of `created_cells` proven to ESCAPE (a capturing closure was lowered outside
    /// safe-combinator-callback context). Escaping cells are NEVER freed (leak, but sound).
    escaping_cells: std::collections::HashSet<Temp>,
    /// Transfer-on-escape aliasing: a call-result `dst` → the RAW fresh-alloc heap-literal
    /// temps whose payload that result aliases (because the literal was boxed into a
    /// Json/union parameter and the callee borrows + returns it, e.g. `(acc) => acc`).
    ///
    /// The literal is `register_owned` in this scope and would normally be released at
    /// scope exit. That is correct when the call result is TRANSIENT (consumed/discarded —
    /// the single release balances the single +1). But when the result ESCAPES (is kept in
    /// the return keep-set), releasing the literal frees the payload the escaping result
    /// still aliases → use-after-free. So `pop_scope_releasing_keep` transitively expands
    /// the keep-set through this map: keeping a result also keeps the literals it aliases,
    /// transferring ownership into the escaping value (its eventual owner does the release).
    escape_alias: HashMap<Temp, Vec<Temp>>,
}

impl FuncBuilder {
    fn new(
        id: FuncId,
        name: Option<String>,
        params: Vec<(Temp, Type)>,
        is_closure: bool,
        ret_ty: Type,
        intrinsic_slots: HashMap<usize, String>,
    ) -> Self {
        let entry_id = BlockId(0);
        let entry_block = BasicBlock {
            id: entry_id,
            label: Some("entry".into()),
            instructions: Vec::new(),
            terminator: Terminator::Unreachable,
            span: None,
            instr_spans: Vec::new(),
        };
        let mut temp_types = HashMap::new();
        let mut temp_count = 0u32;
        for (t, ty) in &params {
            temp_types.insert(*t, ty.clone());
            if t.0 >= temp_count {
                temp_count = t.0 + 1;
            }
        }
        Self {
            id,
            name,
            params,
            is_closure,
            ret_ty,
            blocks: vec![entry_block],
            current_block: entry_id,
            current_span: None,
            temp_count,
            temp_types,
            block_counter: 1,
            slots: HashMap::new(),
            intrinsic_slots,
            scope_owned: Vec::new(),
            diverged_blocks: std::collections::HashSet::new(),
            cell_slots: HashMap::new(),
            created_cells: Vec::new(),
            escaping_cells: std::collections::HashSet::new(),
            escape_alias: HashMap::new(),
        }
    }

    fn alloc_temp(&mut self, ty: Type) -> Temp {
        let t = Temp(self.temp_count);
        self.temp_count += 1;
        self.temp_types.insert(t, ty);
        t
    }

    fn alloc_block(&mut self, label: impl Into<String>) -> BlockId {
        let id = BlockId(self.block_counter);
        self.block_counter += 1;
        self.blocks.push(BasicBlock {
            id,
            label: Some(label.into()),
            instructions: Vec::new(),
            terminator: Terminator::Unreachable,
            span: None,
            instr_spans: Vec::new(),
        });
        id
    }

    /// Record the source span of a block (used for coverage region emission).
    /// Only sets the span if it has not already been set.
    fn set_block_span(&mut self, id: BlockId, span: lin_common::Span) {
        if let Some(b) = self.blocks.iter_mut().find(|b| b.id == id) {
            if b.span.is_none() {
                b.span = Some(span);
            }
        }
    }

    fn current_block_mut(&mut self) -> &mut BasicBlock {
        let id = self.current_block;
        self.blocks.iter_mut().find(|b| b.id == id).unwrap()
    }

    fn emit(&mut self, instr: Instruction) {
        let span = self.current_span;
        let block = self.current_block_mut();
        block.instructions.push(instr);
        // Keep the per-instruction debug-span side-table in lockstep with `instructions`.
        // Backfill with `None` if some earlier `emit` somehow skipped (defensive; in practice
        // every push goes through here so they stay 1:1).
        while block.instr_spans.len() < block.instructions.len() - 1 {
            block.instr_spans.push(None);
        }
        block.instr_spans.push(span);
    }

    /// Set the source span attributed to subsequently-emitted instructions. Called at
    /// statement/expression lowering boundaries so DWARF gets statement-granularity locations.
    fn set_span(&mut self, span: lin_common::Span) {
        self.current_span = Some(span);
    }

    fn terminate(&mut self, term: Terminator) {
        self.current_block_mut().terminator = term;
    }

    fn switch_to(&mut self, block: BlockId) {
        self.current_block = block;
    }

    /// Patch the back-edge predecessor block of the `Phi` writing `dst` in `header`. Used by loop
    /// scaffolds whose body may switch basic blocks (e.g. `filter`'s keep/skip split): the phi's
    /// back-edge must name the block that actually jumps back to the header, not the nominal loop
    /// body block. `old_block` is the placeholder predecessor recorded when the phi was emitted.
    fn patch_phi_incoming(&mut self, header: BlockId, dst: Temp, old_block: BlockId, new_block: BlockId) {
        if old_block == new_block {
            return;
        }
        if let Some(b) = self.blocks.iter_mut().find(|b| b.id == header) {
            for instr in b.instructions.iter_mut() {
                if let Instruction::Phi { dst: pdst, incomings, .. } = instr {
                    if *pdst == dst {
                        for inc in incomings.iter_mut() {
                            if inc.1 == old_block {
                                inc.1 = new_block;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Patch a header phi `dst`'s back-edge incoming: replace the provisional `(old_value,
    /// old_block)` pair with the real `(new_value, new_block)`. Used by the inlined-reduce loop,
    /// whose accumulator phi back-edge value (the lowered reducer body's result) and predecessor
    /// block are only known after the body — which may switch blocks — is lowered.
    fn patch_phi_incoming_value(
        &mut self,
        header: BlockId,
        dst: Temp,
        old_value: Temp,
        new_value: Temp,
        new_block: BlockId,
    ) {
        if let Some(b) = self.blocks.iter_mut().find(|b| b.id == header) {
            for instr in b.instructions.iter_mut() {
                if let Instruction::Phi { dst: pdst, incomings, .. } = instr {
                    if *pdst == dst {
                        for inc in incomings.iter_mut() {
                            if inc.0 == old_value {
                                inc.0 = new_value;
                                inc.1 = new_block;
                            }
                        }
                    }
                }
            }
        }
    }

    fn seal(&mut self) {
        // No-op placeholder for future dominance/phi optimizations.
    }

    fn finish(self) -> LinFunction {
        if let Ok(want) = std::env::var("LIN_DUMP_IR") {
            let nm = self.name.clone().unwrap_or_default();
            if want.is_empty() || nm.contains(&want) {
                eprintln!("=== IR fn {} ({:?}) ===", nm, self.params.iter().map(|(t, ty)| (t.0, ty)).collect::<Vec<_>>());
                for b in &self.blocks {
                    eprintln!("  block {} {:?}:", b.id.0, b.label);
                    for inst in &b.instructions {
                        eprintln!("    {:?}", inst);
                    }
                    eprintln!("    term: {:?}", b.terminator);
                }
            }
        }
        LinFunction {
            id: self.id,
            name: self.name,
            params: self.params,
            is_closure: self.is_closure,
            ret_ty: self.ret_ty,
            // Conventions are inferred by a dedicated pass (`infer_conventions`) AFTER the whole
            // module is lowered (it needs the finished blocks + liveness). Default to the
            // conservative `Own` everywhere here — byte-for-byte today's behaviour.
            param_conventions: Vec::new(),
            ret_convention: Convention::Own,
            blocks: self.blocks,
            temp_types: self.temp_types,
            temp_count: self.temp_count,
            intrinsic_slots: self.intrinsic_slots.clone(),
            repr: Vec::new(),
            coverage_origin: None,
        }
    }

    /// Emit a Const instruction and return the fresh temp.
    fn const_temp(&mut self, val: Const) -> Temp {
        let ty = const_type(&val);
        let dst = self.alloc_temp(ty);
        self.emit(Instruction::Const { dst, val });
        dst
    }

    /// Push a new ownership scope frame.
    fn push_scope(&mut self) {
        self.scope_owned.push(Vec::new());
    }

    /// Register an owned temp in the current scope frame.
    ///
    /// Uses `needs_owning` (concrete rc OR boxed union/Json), not just `is_rc_type`: an owned
    /// boxed-union value (e.g. the result of a `map`/`filter`/`reduce`/`concat`/`keys` call,
    /// which all return `Json`) is a freshly-allocated `TaggedVal*` (+1) that the scope must
    /// release at exit, exactly like a concrete rc value. The scope-exit `Release { ty: <union> }`
    /// dispatches the tag-aware `lin_tagged_release` (null/scalar/cached-box safe; frees the box
    /// shell and drops the inner payload's rc). Restricting to `is_rc_type` silently dropped
    /// union registrations (the historic source of the per-call Json leak).
    fn register_owned(&mut self, t: Temp, ty: Type) {
        if needs_owning(&ty) {
            if let Some(frame) = self.scope_owned.last_mut() {
                frame.push((t, ty));
            }
        }
    }

    /// Remove a temp from the owned set across all live scope frames. Used when ownership
    /// of a freshly-allocated heap value is *transferred* into a container (array/object)
    /// or a consuming callee: the container now holds the +1, so the originating scope must
    /// NOT also release it (that would double-free, since the container releases it on drop).
    fn unregister_owned(&mut self, t: Temp) {
        for frame in self.scope_owned.iter_mut() {
            frame.retain(|(owned, _)| *owned != t);
        }
    }

    /// True if `t` is registered as owned (holds an independent +1) in any live scope frame.
    /// Used at the function return boundary to distinguish a value the scope already owns
    /// (fresh alloc, retained projection, cloned cell/global read — return it as-is) from a
    /// BORROWED interior pointer (e.g. a union/Json `obj[k]` projection — `lin_object_get`
    /// hands back a `*TaggedVal` pointing INTO the container, which the lowerer deliberately
    /// does NOT own). The latter must be cloned before it escapes as the result, or the
    /// caller's uniform "result is owned +1, release it" convention double-frees the interior
    /// value when the container is also released.
    fn is_owned_in_scope(&self, t: Temp) -> bool {
        self.scope_owned
            .iter()
            .any(|frame| frame.iter().any(|(owned, _)| *owned == t))
    }

    /// True if `t` is registered owned in the INNERMOST (current) scope frame only — i.e. a
    /// value freshly produced and owned by THIS scope (a +1 the scope will release on pop),
    /// as opposed to one owned by an enclosing frame (e.g. a `val r = …` local read inside a
    /// branch: `r`'s +1 lives in the function-body scope, not the branch scope). Used by an
    /// `if`/match branch to decide whether a union value flowing into the merge can TRANSFER
    /// its branch-scope +1 (current-frame owned) or must be CLONED (owned elsewhere / borrowed),
    /// so the merge ends up with an independently-owned box that an enclosing release can't free.
    fn is_owned_in_current_scope(&self, t: Temp) -> bool {
        self.scope_owned
            .last()
            .is_some_and(|frame| frame.iter().any(|(owned, _)| *owned == t))
    }

    /// THE container-insert ownership rule, in one place.
    ///
    /// When a value is stored into a container that takes ownership of one reference
    /// (array element, object field, `push`/`set`), the source's refcount must end up
    /// balanced so that exactly one owner frees it:
    ///   - a **fresh allocation** (`expr_is_fresh_alloc`) already holds the only +1; transfer
    ///     it by dropping the temp from the owning scope so scope-exit won't also release it
    ///     (the container's drop accounts for it);
    ///   - a **borrowed** heap value (e.g. a `LocalGet`) is shared, so retain it — the
    ///     container's copy and the original owner can then each release independently.
    /// Non-RC values need nothing. Centralising this means a new container-insert site can't
    /// silently get the rule half-right (the historical source of double-free / leak bugs).
    ///
    /// `temp` is the RAW underlying heap value (for concrete rc, never a boxed TaggedVal —
    /// retaining a box bumps the wrong refcount; for unions it IS the boxed TaggedVal). `source`
    /// is the expression that produced it. `op_consumes_union` records whether the container op,
    /// for a UNION element, MOVES the box into the slot (raw struct copy, no inner retain) rather
    /// than retaining the inner — see the runtime semantics below.
    ///
    /// Union elements need op-specific handling because the runtime is NOT uniform:
    ///   - `Push` (tagged array, `lin_push_dyn`) and `object_set` RETAIN the boxed value's inner
    ///     payload — the slot gets its own reference. The source box keeps its own reference and
    ///     is released by its owner (scope-exit for a fresh call result, the original owner for a
    ///     borrowed value). So we do NOTHING: leave it registered, do not retain.
    ///   - `lin_array_set` into a tagged array does a raw `copy_nonoverlapping` of the TaggedVal
    ///     struct and does NOT bump the inner rc — it CONSUMES the box. A fresh source must be
    ///     unregistered (else scope-exit + the slot both free the same inner → double-free); a
    ///     borrowed source must be retained (so the slot owns its own inner reference, mirroring
    ///     the concrete-rc rule).
    /// For CONCRETE rc elements every op consumes (codegen never retains a concrete element on
    /// insert), so the original fresh-vs-borrowed rule applies regardless of `op_consumes_union`.
    fn transfer_into_container(&mut self, temp: Temp, source: &TypedExpr, op_consumes_union: bool) {
        let ty = source.ty();
        // The fresh-vs-borrowed (move vs +1-retain) decision now lives in the single ownership
        // authority `container_insert_convention`, which encodes the exact same three-way branch
        // (not-owning / retain-semantics-union ⇒ nothing; fresh ⇒ transfer; borrowed ⇒ retain).
        // `expr_is_fresh_alloc` is the one `lower`-only AST predicate, computed here and passed in
        // (mirroring how `escape_alias_convention` takes `is_sealed_scalar_repr`); the lowerer
        // performs the matching action, so the emitted IR — and the RC — is byte-identical.
        match crate::ownership_verify::container_insert_convention(&ty, op_consumes_union, expr_is_fresh_alloc(source)) {
            crate::ownership_verify::ContainerInsert::Nothing => {}
            crate::ownership_verify::ContainerInsert::Transfer => self.unregister_owned(temp),
            crate::ownership_verify::ContainerInsert::Retain => self.emit(Instruction::Retain { val: temp, ty }),
        }
    }

    /// Pop the current scope frame and emit Release for all owned temps except those in the kept
    /// set. The kept set is `keep` expanded through `escape_alias` — the fresh literals whose
    /// ownership transfers into `keep` when it escapes this scope (e.g. a block whose result is
    /// `id([1,2])` must keep the `[1,2]` literal alive, not just the result temp). Each kept temp
    /// transfers EXACTLY ONE owned reference: a temp registered more than once in this scope
    /// (e.g. `val r = [..]; r`, where the block result `r` is registered at the array allocation
    /// AND again by the `LocalGet` read-retain of the trailing expression) leaks every reference
    /// beyond the first unless the extras are released — so keep each temp's FIRST occurrence and
    /// RELEASE the rest. (Mirrors `pop_scope_releasing_keep`.)
    fn pop_scope_releasing(&mut self, keep: Temp) {
        self.pop_scope_releasing_keep_transfer(&[keep]);
    }

    /// Like `pop_scope_releasing` but keeps SEVERAL survivors (the block result PLUS any outer
    /// `var` slots reassigned inside the block, whose freshly-owned temp must transfer up to the
    /// enclosing scope rather than be released at the block boundary). Each kept temp transfers
    /// EXACTLY ONE reference (the first occurrence) and is re-registered owned in the parent
    /// scope so the parent — or a containing `if`/match merge — releases it exactly once.
    fn pop_scope_releasing_keep_transfer(&mut self, keep: &[Temp]) {
        let keep = self.expand_keep_for_escape(keep);
        if let Some(frame) = self.scope_owned.pop() {
            let mut kept: Vec<(Temp, Type)> = Vec::new();
            for (t, ty) in frame {
                if keep.contains(&t) && !kept.iter().any(|(k, _)| *k == t) {
                    kept.push((t, ty));
                } else {
                    self.emit(Instruction::Release { val: t, ty });
                }
            }
            // The kept survivors' +1 references TRANSFER UP to the now-current (parent) scope:
            // re-register them so the parent owns and releases them (or keeps them again if the
            // value is the parent's own survivor). Without this, a block whose result is an
            // owned +1 (e.g. an `if`/match merge value, a fresh call result) would be seen as
            // unowned by the enclosing function-return path — which then takes a SECOND +1 via
            // CloneBox/Retain, leaking one reference per evaluation (a per-iteration leak inside
            // a loop). Mirrors `pop_scope_releasing_keep`.
            for (t, ty) in kept {
                self.register_owned(t, ty);
            }
        }
    }

    /// Record that the call result `dst` aliases the payload of the raw fresh-alloc literal
    /// `lit` (see `escape_alias`). Used by `lower_call` when a fresh heap literal is boxed
    /// into a Json/union parameter; ownership of `lit` transfers into `dst` if `dst` escapes.
    fn record_escape_alias(&mut self, dst: Temp, lit: Temp) {
        self.escape_alias.entry(dst).or_default().push(lit);
    }

    /// Expand a return keep-set transitively through `escape_alias`: if a kept temp is a
    /// call result that aliases fresh literal(s), those literals must be kept too (their
    /// ownership transfers into the escaping result). Follows chains (e.g. `wrap` returning
    /// `mid([1,2])` where `mid` returns `id(acc)`).
    fn expand_keep_for_escape(&self, keep: &[Temp]) -> Vec<Temp> {
        let mut out: Vec<Temp> = keep.to_vec();
        let mut i = 0;
        while i < out.len() {
            let t = out[i];
            if let Some(lits) = self.escape_alias.get(&t) {
                for &lit in lits {
                    if !out.contains(&lit) {
                        out.push(lit);
                    }
                }
            }
            i += 1;
        }
        out
    }

    /// Pop the current scope frame, releasing all owned temps except those in `keep`.
    ///
    /// A kept temp transfers EXACTLY ONE owned reference to the survivor (the function return,
    /// or an if/match branch value flowing into the merge phi). The same temp can be registered
    /// MULTIPLE times in one scope: e.g. `val r = [..]; r` registers `r` once at the array
    /// allocation and again at the `LocalGet` read-retain of the return expression. Keeping ALL
    /// registrations would leak every reference beyond the first (the classic concrete-rc
    /// return-retain leak: the array is freed by the caller's single release but stays at the
    /// extra refcount). So we keep only the FIRST occurrence of each kept temp and RELEASE the
    /// rest, leaving the survivor at exactly +1 for the caller.
    fn pop_scope_releasing_keep(&mut self, keep: &[Temp]) {
        let keep = self.expand_keep_for_escape(keep);
        if let Some(frame) = self.scope_owned.pop() {
            let mut kept_seen: Vec<Temp> = Vec::new();
            for (t, ty) in frame {
                if keep.contains(&t) && !kept_seen.contains(&t) {
                    // Transfer this single reference to the survivor.
                    kept_seen.push(t);
                } else {
                    // Either not kept at all, or a redundant extra registration of a kept temp
                    // (a leaked read-retain) — release it.
                    self.emit(Instruction::Release { val: t, ty });
                }
            }
        }
    }

    /// Pop the current ownership scope without emitting releases. Used when the block
    /// is already terminated (e.g. ends in a tail call or return), so any cleanup
    /// would be unreachable / handled by the terminating construct.
    fn discard_scope(&mut self) {
        self.scope_owned.pop();
    }

    /// Release every owned temp live in ANY scope frame, EXCEPT the temps passed as
    /// tail-call arguments (which are consumed by the back-edge: they become the next
    /// iteration's param-slot values, and codegen's TCO release-old machinery frees the
    /// PREVIOUS slot value). This MUST run on the live block immediately before a `TailCall`
    /// terminator is emitted.
    ///
    /// Why this exists: a self-tail-call diverges, so `lower_call` switches to a dead
    /// `tco_post` block afterwards. Every enclosing scope-exit `Release` (block scopes, `if`
    /// branch scopes, the function body scope) is then emitted into that unreachable block
    /// (or a chain of dead blocks) and NEVER RUNS. For a tail-recursive function whose body
    /// allocates fresh owned temps each iteration — e.g. the projections/`lin_tagged_clone`s
    /// of `routeScanner.scanBack` (`scanner["tripsByRoute"][routeId]`, `routeTrips[i]`,
    /// `trip["stopTimes"]`, the route-id/key string literals) — every one of those clones
    /// leaks once per iteration (the dominant RAPTOR per-scan leak, ~190 MB/scan). Releasing
    /// them here, on the live back-edge block, balances the per-iteration allocation.
    ///
    /// The frames are left in place (`discard_scope`/`pop_scope_releasing_keep` still pop them
    /// afterwards, but into the dead post block where any further Release is unreachable and
    /// harmless). A temp registered owned MORE THAN ONCE across frames is released ONCE here
    /// (the runtime release decrements one reference per call; the redundant registrations are
    /// read-retains the single-pop logic also collapses to one). A tail-call arg temp is
    /// skipped even when it is also owned in scope (e.g. the `if cond then trip else lastFound`
    /// merge value threaded as the next `lastFound`): its +1 transfers into the param slot.
    fn release_owned_for_tail_call(&mut self, args: &[Temp]) {
        // Tail-call arg temps fall into two ownership classes — distinguished by whether the arg
        // temp is one of THIS function's PARAMETER temps (a borrowed value the caller owns) or a
        // value the body freshly owns:
        //
        //  - TRANSFERRING args (a freshly-owned value: a literal/call/closure alloc, a `val`
        //    local read, a union box clone — NOT a bare param temp): the body holds exactly one
        //    transferable +1, which moves into the param slot (codegen's release-old frees the
        //    PRIOR slot value on the next back-edge; the final value is freed at teardown / is the
        //    accepted single residual). Keep the FIRST owned registration of such a temp and
        //    release the rest, exactly like `pop_scope_releasing_keep` (a redundant extra
        //    registration — e.g. a fresh value also read back via `LocalGet` — would otherwise
        //    leak). Also keep any temp transitively kept through `escape_alias` (an owned union box
        //    whose UNBOXED inner is the arg, e.g. `concat(b,b)`): fully releasing it would free the
        //    array now threaded into the slot.
        //
        //  - PASS-THROUGH args (the arg temp IS one of this function's param temps — a bare
        //    `LocalGet` of a parameter threaded UNCHANGED): NO +1 transfers. The value stored into
        //    the slot is the same borrowed pointer the caller still owns, so codegen's release-old
        //    correctly skips it (its alias guard sees old == new). The body's read-retains on such
        //    a param (e.g. `is_rc_type` reads `Retain` in place for a concrete-rc Array/Object) are
        //    pure borrows that must net to ZERO — exactly like the Json/union param path, which
        //    takes no read-retain at all. RELEASE every registration. Otherwise each read leaks one
        //    reference per iteration: the typed sealed-record array threaded through a tail param
        //    (`Transfer[]` grown via `push`) leaked ~2 refs/iteration because both the `push`
        //    receiver read and the tail-call-arg read retained the array and neither release ran on
        //    the live back-edge (the scope-exit releases land in the dead `tco_post` block).
        let keep = self.expand_keep_for_escape(args);
        // The set of param temps the args alias as PASS-THROUGH (borrowed, no +1 transfers). A param
        // threaded unchanged reads back as its own param temp; releasing all its registrations nets
        // to zero against the caller's owned reference.
        let param_temps: Vec<Temp> = self.params.iter().map(|(t, _)| *t).collect();
        let mut kept_seen: Vec<Temp> = Vec::new();
        let mut to_release: Vec<(Temp, Type)> = Vec::new();
        for frame in &self.scope_owned {
            for (t, ty) in frame {
                // A pass-through param arg transfers no +1 — release every registration.
                if args.contains(t) && param_temps.contains(t) {
                    to_release.push((*t, ty.clone()));
                    continue;
                }
                // A kept (transferring) arg, or a temp kept only through escape-aliasing: keep the
                // FIRST registration (transfers into the slot / is the survivor) and release any
                // redundant extra registration.
                if keep.contains(t) {
                    if kept_seen.contains(t) {
                        // Redundant extra registration of a kept temp — release it (a leaked
                        // read-retain), matching `pop_scope_releasing_keep`.
                        to_release.push((*t, ty.clone()));
                    } else {
                        kept_seen.push(*t);
                    }
                    continue;
                }
                // A non-arg owned temp (per-iteration body allocation) — release once (dedup below).
                to_release.push((*t, ty.clone()));
            }
        }
        // Release each scheduled temp. Non-arg owned temps are released ONCE (dedup), preserving
        // prior behavior — EXCEPT temps whose every scope registration corresponds to a GENUINE,
        // non-elide-able Retain. Two classes qualify:
        //
        // 1. SEALED SCALAR RECORD: a fresh `val cur: Trip = {…}` / `= arr[i]` source struct
        //    threaded into a `Trip | Null` tail param accrues TWO genuine owned references — the
        //    alloc/projection (+1) AND `coerce_and_own_store`'s `own_for_store` RETAIN at the
        //    binding (+1) — each with its own scope registration. In straight-line code both are
        //    released at scope exit (balanced); on a TCO back-edge the dedup released only ONE,
        //    leaking the surplus packed struct (and its heap fields) every iteration. Both of a
        //    sealed record's retains are GENUINE (`own_for_store`/field retains, never rc_elide
        //    read-retain pairs on the tail path), so releasing per registration is balanced.
        //
        // 2. STRING (Str / StrLit): a `val op = tokens[pos]["text"]` sealed-field read registers
        //    the string once (field-read retain). If `op` is then used as the LEFT operand of a
        //    `&&` short-circuit, `lower_short_circuit` calls `lower_cond_as_bool(left)` in the
        //    OUTER scope (no push_scope/pop_scope wrapper) — the LocalGet emits a second Retain
        //    and a second scope registration. Because the `&&` branch terminates in a TailCall
        //    back-edge, the enclosing scope never pops: both registrations remain in `scope_owned`
        //    when `release_owned_for_tail_call` fires, but the dedup emitted only ONE Release,
        //    leaking one reference per operator consumed. A String LocalGet registration in
        //    `scope_owned` at TailCall time is ALWAYS genuine: if the Retain and its Release had
        //    been in the same block (pop_scope_releasing_keep), the registration would already have
        //    been removed from `scope_owned` — so every remaining registration maps 1-to-1 to a
        //    surviving, cross-block Retain that needs exactly one Release.
        //
        // The gate EXCLUDES BOXED (union/object/Json) temps: their registrations CAN be phantom
        // (e.g. a union narrowed to a sealed record via `narrowed_to_sealed` registers owned
        // WITHOUT a Retain; per-registration release would over-release — the use-after-free in
        // calc's `parseTermLoop` that originally motivated the dedup). Those keep the dedup.
        let mut non_arg_seen: Vec<Temp> = Vec::new();
        for (t, ty) in to_release {
            let per_registration = is_sealed_scalar_repr(&ty)
                || matches!(ty, Type::Str | Type::StrLit(_));
            if !args.contains(&t) && !per_registration {
                if non_arg_seen.contains(&t) {
                    continue;
                }
                non_arg_seen.push(t);
            }
            self.emit(Instruction::Release { val: t, ty });
        }
    }

    /// Snapshot the plain SSA var-slot → (temp, type) bindings before lowering a branch. Excludes
    /// heap-cell slots (their `slots` entry is a stable cell pointer — reassignment goes through
    /// the cell, not by rebinding the slot) and global var slots (read/written through the module
    /// global, so reassignment is already join-coherent). What remains is exactly the set of slots
    /// whose value lives in a function-local SSA temp that a branch can rebind — the slots that
    /// need a join phi if mutated. Vals never reassign, so although vals are included here they
    /// can never be detected as "reassigned" and so never get a (harmless) phi.
    fn plain_var_slot_snapshot(&self, ctx: &LowerCtx) -> Vec<(usize, Temp, Type)> {
        self.slots
            .iter()
            .filter(|(slot, _)| {
                !self.cell_slots.contains_key(*slot) && !ctx.global_var_slots.contains(*slot)
            })
            .map(|(slot, temp)| {
                let ty = self.temp_types.get(temp).cloned().unwrap_or(Type::Null);
                (*slot, *temp, ty)
            })
            .collect()
    }

    /// Given a pre-branch slot snapshot, return the slots whose `slots` entry the branch REBOUND
    /// to a different temp (i.e. a `var` reassigned inside the branch), as (slot, new temp).
    fn collect_reassigned_slots(&self, pre: &[(usize, Temp, Type)], ctx: &LowerCtx) -> Vec<(usize, Temp)> {
        pre.iter()
            .filter_map(|(slot, old_temp, _)| {
                if self.cell_slots.contains_key(slot) || ctx.global_var_slots.contains(slot) {
                    return None;
                }
                match self.slots.get(slot) {
                    Some(new_temp) if new_temp != old_temp => Some((*slot, *new_temp)),
                    _ => None,
                }
            })
            .collect()
    }

    /// Restore the plain var-slot bindings captured by `plain_var_slot_snapshot` so a sibling
    /// branch lowers against the pre-if slot temps rather than the previous branch's rebindings.
    fn restore_plain_var_slots(&mut self, pre: &[(usize, Temp, Type)]) {
        for (slot, temp, _) in pre {
            self.slots.insert(*slot, *temp);
        }
    }

    fn is_current_block_terminated(&self) -> bool {
        let id = self.current_block;
        // A diverged (post-tail-call) block is effectively terminated: control never
        // reaches it, so callers must not append a Jump or treat it as a phi predecessor.
        if self.diverged_blocks.contains(&id) {
            return true;
        }
        self.blocks
            .iter()
            .find(|b| b.id == id)
            .map(|b| !matches!(b.terminator, Terminator::Unreachable))
            .unwrap_or(false)
    }
}

