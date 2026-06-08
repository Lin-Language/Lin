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
            value: TypedExpr::Function { .. },
            ..
        } = stmt
        {
            let fid = ctx.alloc_func_id();
            global_fn_slots.insert(*slot, fid);
        }
    }
    ctx.global_fn_slots = global_fn_slots.clone();

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

struct LowerCtx {
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
    /// >0 while lowering an expression that is a SYNCHRONOUS, non-retained callback argument
    /// to a known consuming combinator (for/while/map/filter/reduce). A closure literal
    /// (`MakeClosure`) lowered while this is >0 is PROVABLY consumed-and-discarded by the
    /// combinator within the same function call — it is never bound, returned, or stored — so
    /// the heap cell(s) it captures do not escape and may be freed at the creating function's
    /// scope exit. When this is 0, any captured cell is conservatively marked escaping (left
    /// leaking). See `FreeCell` and the captured-cell escape analysis.
    safe_callback_depth: u32,
    /// When lowering an IMPORTED module that has top-level mutable `var`s, this holds the
    /// once-guarded var-init function symbol (`{module_key}__var_init`). Set just before a
    /// TOP-LEVEL EXPORTED function body (or `__val` wrapper) is lowered, so the body emits a
    /// call to it on entry — guaranteeing the module's vars are initialised before any
    /// exported entry point reads them. Cleared (taken) after one prologue is emitted so
    /// nested closures within the body do not re-run init.
    import_var_init_prologue: Option<String>,
}

/// A default-fill adapter to be synthesized and lowered. `f@k` takes the first `k` parameters
/// of `f`, binds each remaining parameter to its default expression, then calls `f` with the
/// full argument list. Built as a synthetic `TypedExpr::Function` so it reuses the normal
/// function-lowering path (RC, coercion, chained/earlier-param default references).
struct AdapterSpec {
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
            safe_callback_depth: 0,
            import_var_init_prologue: None,
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

struct FuncBuilder {
    id: FuncId,
    name: Option<String>,
    params: Vec<(Temp, Type)>,
    is_closure: bool,
    ret_ty: Type,
    blocks: Vec<BasicBlock>,
    current_block: BlockId,
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
        self.current_block_mut().instructions.push(instr);
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
        LinFunction {
            id: self.id,
            name: self.name,
            params: self.params,
            is_closure: self.is_closure,
            ret_ty: self.ret_ty,
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

    #[allow(dead_code)]
    fn copy_temp(&mut self, src: Temp, ty: Type) -> Temp {
        let dst = self.alloc_temp(ty);
        self.emit(Instruction::Copy { dst, src });
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
        if !needs_owning(&ty) {
            return;
        }
        if is_union_ty(&ty) && !op_consumes_union {
            // Retain-semantics op (Push / object_set): the runtime took its own inner reference;
            // the source box stays owned by its current owner. Nothing to balance here.
            return;
        }
        if expr_is_fresh_alloc(source) {
            self.unregister_owned(temp);
        } else {
            self.emit(Instruction::Retain { val: temp, ty });
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
        // prior behavior — EXCEPT a SEALED SCALAR RECORD temp, which is released once PER
        // REGISTRATION. A fresh `val cur: Trip = {…}` / `= arr[i]` source struct threaded into a
        // `Trip | Null` tail param accrues TWO genuine owned references — the alloc/projection (+1)
        // AND `coerce_and_own_store`'s `own_for_store` RETAIN at the binding (+1) — each with its own
        // scope registration. In straight-line code both are released at scope exit (balanced); on a
        // TCO back-edge the dedup released only ONE, leaking the surplus packed struct (and its heap
        // fields) every iteration — the `Trip | Null` sealed-record-union tail-param per-iteration
        // leak. Both of a sealed record's retains are GENUINE (`own_for_store`/field retains, never a
        // read-retain rc_elide pairs on the tail path), so releasing per registration is balanced.
        // The gate is restricted to sealed-packed records precisely because BOXED (Json/union/object)
        // temps DO accrue rc_elide-elided read-retain duplicates, where per-registration release
        // over-releases (a use-after-free in calc's `parseTermLoop`) — those keep the dedup.
        let mut non_arg_seen: Vec<Temp> = Vec::new();
        for (t, ty) in to_release {
            let per_registration = is_sealed_scalar_repr(&ty);
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

fn is_rc_type(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Str | Type::StrLit(_) | Type::Array(_) | Type::FixedArray(_) | Type::Object { .. } | Type::Map(_) | Type::Iterator(_) | Type::Function { .. }
    )
    // NOTE (unboxed-sumtype Stage 2): a sum value is a refcounted `*SumNode`, but it is NOT added to
    // `is_rc_type` — Stage 1 deliberately keeps sum types OUT of the generic owning-read path
    // (`own_for_read` / read-retain). A sum local is owned by its CONSTRUCTION site
    // (`try_lower_sum_literal` → `register_owned`, released at scope exit via the SumNode branch of
    // `emit_release_repr`); every other read (match scrutinee, recursive child) is a BORROW. Adding
    // sum to `is_rc_type` would make the match scrutinee read retain the param AND the call-arg read
    // retain again — extra references whose scope-exit releases do not always balance under nested
    // recursion, leaking the tree (under-release → reuse-corruption). The recursive-CHILD ownership
    // (the node owns +1 of each child) is handled explicitly at the SumNode construction transfer
    // (`transfer_into_container`) + the runtime KIND_SUMNODE drop walk, NOT via this predicate.
}

/// The CHILD SUM TYPE of a recursive-child field read (unboxed-sumtype Stage 2), or `None`.
///
/// A recursive child read `node["left"]` reaches the lowerer with `obj_ty` being the NARROWED variant
/// record (a `Type::Object`, e.g. `BinOp = { kind:"op", left: <Ast>, right: <Ast> }`) — the match arm
/// narrows the SumNode scrutinee to its concrete variant. The recursive child field's TYPE is the
/// (one-level-inlined) sum union itself, which is `sum_type_eligible`. So a field is a recursive child
/// iff it is present in `obj_ty` with a sum-eligible field type. We return that child sum type (used
/// as the FieldGet's effective sum type so codegen reads it as a `*SumNode` and the result seeds
/// Packed(SumNode)). Returns `None` for a non-recursive / non-sum field.
fn sum_recursive_child_field_ty(obj_ty: &Type, field: &str) -> Option<Type> {
    let Type::Object { fields, .. } = obj_ty else { return None };
    let fty = fields.get(field)?;
    if crate::repr::sum_type_eligible(fty) {
        Some(fty.clone())
    } else {
        None
    }
}

/// Read the RAW physical `SumNode` temp of a sum scrutinee `object` for a recursive-child FieldGet,
/// bypassing the union→variant-object narrowing Coerce `lower_expr(LocalGet)` would otherwise emit
/// (which materializes the SumNode to a boxed object). Only handles a bare `LocalGet`/`GlobalValGet`
/// of a slot whose STORED temp is physically a SumNode (its stored type is sum-eligible) — exactly
/// the `match node is BinOp => evalNode(node["left"])` shape. Returns `None` for anything else (the
/// caller then falls back to the regular `lower_expr` path). Does NOT add a reference (the FieldGet
/// site handles the borrowed-child retain-on-escape).
fn lower_sum_scrutinee_raw(
    object: &TypedExpr,
    builder: &mut FuncBuilder,
    _ctx: &mut LowerCtx,
) -> Option<Temp> {
    let TypedExpr::LocalGet { slot, .. } = object else { return None };
    let t = *builder.slots.get(slot)?;
    let stored_ty = builder.temp_types.get(&t).cloned().unwrap_or(Type::Null);
    if crate::repr::sum_type_eligible(&stored_ty) {
        Some(t)
    } else {
        None
    }
}

/// Whether codegen's `Index` for `obj_ty[key_ty]` takes the ARRAY path (`lin_array_get_tagged`),
/// which returns a FRESH, fully-owned `TaggedVal*` (+1) — as opposed to the object/map path
/// (`lin_object_get` / `lin_map_get`), which returns a BORROWED interior pointer into the
/// container. This MUST mirror codegen's dispatch in `compile_ir_index` (data.rs): the `Map`
/// branch is checked FIRST, then `is_array_access = Array/FixedArray(obj_ty) || numeric key`.
///
/// The distinction matters for ownership: a union Index result is normally relocated off its
/// (borrowed, movable) container slot via `CloneBox`. But when the source is ALREADY a fresh
/// +1 box (the array path), that extra clone leaks the original box once per evaluation — the
/// dominant per-scanned-stop leak in `routeScanner.scanBack`'s `routeTrips[i]`. Register the
/// fresh box owned directly instead (mirrors the sealed-record-by-dynamic-key fresh-box case).
fn index_result_is_fresh_owned_box(obj_ty: &Type, key_ty: &Type) -> bool {
    if matches!(obj_ty, Type::Map(_)) {
        return false;
    }
    matches!(obj_ty, Type::Array(_) | Type::FixedArray(_)) || key_ty.is_numeric()
}

/// True when `ty` is a SEALED RECORD — a `Type::Object { sealed: true }` all of whose fields are
/// either unboxed scalars (numeric or Bool) — Stage 1 — OR eligible HEAP fields (String, Array,
/// nested sealed record) — Stage 2. MUST mirror `Codegen::sealed_fields` EXACTLY: the two decide,
/// independently, when the unboxed packed-struct layout applies, so any disagreement would make the
/// lowerer's Coerce-insertion and codegen's representation diverge (a UAF / mis-read). A sealed
/// record is still a concrete refcounted heap value (`is_rc_type` true), so the owning model treats
/// it like any object — only its physical layout and its `emit_release`/construct/field-read codegen
/// differ (routed via the sealed runtime). The function name is kept for call-site stability across
/// the (now generalized) Stage 1 + Stage 2 gate.
fn is_sealed_scalar_repr(ty: &Type) -> bool {
    matches!(ty, Type::Object { fields, sealed: true }
        if !fields.is_empty() && fields.values().all(is_sealed_field_ty))
}

/// True when `ty` is `Array(elem)` whose element is a sealed record laid out as a contiguous,
/// unboxed, header-less element buffer — sealed-records Stage 3a (all-scalar fields) AND Stage 3b
/// (packable HEAP fields: String / Array / nested-sealed). MUST mirror
/// `Codegen::sealed_array_elem` / `sealed_array_elem_field_packable` EXACTLY: the two decide,
/// independently, when the packed-element representation applies, so any disagreement makes the
/// lowerer's Coerce/ownership insertion and codegen's representation diverge (a UAF / mis-read).
/// Heap-field elements get per-element-per-field RC across construct/read/index-set/drop/transfer
/// (runtime: `release_sealed_array_elems` / `retain_sealed_payload_fields` / `lin_sealed_array_set`).
/// The function name is kept for call-site stability across the generalized gate.
fn is_sealed_scalar_array(ty: &Type) -> bool {
    match ty {
        // A field is packable into the contiguous element buffer iff scalar/Bool OR a STRING heap
        // field. Array / nested-sealed element fields are NOT yet enabled (kept boxed) — see the
        // gate note in `Codegen::sealed_array_elem_field_packable`. MUST mirror it EXACTLY.
        Type::Array(elem) => match elem.as_ref() {
            Type::Object { fields, sealed: true } => {
                !fields.is_empty() && fields.values().all(is_sealed_array_elem_field_packable)
            }
            _ => false,
        },
        _ => false,
    }
}

/// Element-field eligibility — delegates to the SINGLE source of truth
/// `Type::is_sealed_array_field_packable` (ADR-063 gate consolidation). The codegen gate
/// (`Codegen::sealed_array_elem_field_packable`), `monomorphize::field_packed_scalar`, and
/// `repr::sealed_array_elem_field_packable` all defer to the same predicate, so they cannot drift.
fn is_sealed_array_elem_field_packable(ty: &Type) -> bool {
    ty.is_sealed_array_field_packable()
}

/// True when `param_ty` is an array whose element is a BOXED runtime representation — a generic
/// TypeVar/Json wildcard, a union, a typed map, or an UNSEALED object. These are the params a sealed
/// packed array must be materialized for. A `Named`-element array (an unexpanded self-recursive alias,
/// which the callee reads as the SAME packed sealed struct) and a sealed-Object-element array are NOT
/// boxed and must pass through unchanged.
fn param_elem_is_boxed_repr(param_ty: &Type) -> bool {
    match param_ty {
        Type::Array(elem) => matches!(elem.as_ref(),
            Type::TypeVar(_) | Type::Union(_) | Type::Map(_)
            | Type::Object { sealed: false, .. }),
        _ => false,
    }
}

/// True when a sealed packed array argument flowing into `param_ty` is MATERIALIZED to a fresh boxed
/// tagged `Object[]` at the call boundary (the §3 fix): either the param is a bare Json/union (the
/// existing `length(sealedArr)` case) OR a boxed-element array (the type-erased boxed-fallback
/// combinator's `T[]` param). The materialized box is FULLY OWNED and the callee BORROWS it (a
/// TypeVar/Json-array param is not released by the owning model), so the caller must fully release it
/// right after the call — else it leaks every call (ASan-confirmed in a sort-in-loop). MUST mirror the
/// materialize trigger in `lower_coerce_arg`.
fn sealed_array_arg_materialized(arg_ty: &Type, param_ty: &Type) -> bool {
    is_sealed_scalar_array(arg_ty)
        && !is_sealed_scalar_array(param_ty)
        && (is_union_ty(param_ty) || param_elem_is_boxed_repr(param_ty))
}

/// True when a SEALED SCALAR RECORD argument (packed struct, e.g. `cur: Trip`) flowing into a
/// `Json`/union param is MATERIALIZED at the call boundary to a FRESH boxed `LinObject` wrapped in a
/// `TaggedVal*` (`box_object(materialize(struct))`). Exactly the scalar analogue of
/// `sealed_array_arg_materialized`: the materialized object is FULLY OWNED (its fields freshly
/// retained out of the source struct) and the callee BORROWS the box, so the caller must FULLY
/// release it right after the call (box shell + inner LinObject + its field references) — NOT just
/// free the box shell. Freeing only the shell (the `arg_box_is_caller_owned_shell` path) leaks the
/// materialized object every call — the `Trip | Null` (sealed-record-union) param leak, ASan-
/// confirmed. This fires on the union-boundary Coerce in `lower_coerce_arg` (the `is_union_ty(param)
/// != is_union_ty(arg)` arm), so it must mirror that trigger: arg is a sealed scalar record, param
/// is a union and NOT itself a sealed record. (A sealed→`Named` pass-through is handled earlier and
/// never reaches the union arm.)
fn sealed_record_arg_materialized(arg_ty: &Type, param_ty: &Type) -> bool {
    is_sealed_scalar_repr(arg_ty) && is_union_ty(param_ty) && !is_union_ty(arg_ty)
}

/// True when `ty` is an array whose ELEMENTS (transitively) contain a sealed-record array or a sealed
/// scalar record — i.e. a NESTED sealed structure (`Pt[][]`, `{String: Pt[]}` would be a Map). The
/// one-level `is_sealed_scalar_array`/`is_sealed_scalar_repr` don't catch the outer container, but its
/// Coerce result (built by codegen's `array_coerce_elementwise`) is still a fresh +1 owned value that
/// must be released. Mirrors codegen's `Codegen::ty_contains_sealed`, restricted to the array-outer case.
fn to_contains_sealed_array(ty: &Type) -> bool {
    fn contains(ty: &Type) -> bool {
        if is_sealed_scalar_array(ty) || is_sealed_scalar_repr(ty) {
            return true;
        }
        match ty {
            Type::Array(t) | Type::Iterator(t) | Type::Shared(t) | Type::Map(t) => contains(t),
            Type::FixedArray(ts) | Type::Union(ts) => ts.iter().any(contains),
            _ => false,
        }
    }
    matches!(ty, Type::Array(elem) if contains(elem))
}

/// True when a `Coerce { from, to }` is the NESTED sealed re-projection codegen's `compile_ir_coerce`
/// rebuilds element-wise (`array_coerce_elementwise`): `to` is an array containing a sealed structure
/// AND `from`'s inner element is a BOXED view (Json/union/TypeVar — the type-erased boxed-fallback
/// result). MUST mirror the codegen gate exactly so the ownership registration matches what codegen
/// actually emits (a same-representation array coerce is a pointer pass-through, NOT a fresh +1).
fn nested_sealed_repr_change(from: &Type, to: &Type) -> bool {
    let Type::Array(inner_to) = to else { return false };
    let Type::Array(inner_from) = from else { return false };
    // Mirror codegen's gate: a NESTED sealed re-projection fires only when `to`'s inner contains a
    // sealed structure AND the source inner element is a DIFFERENT type (a boxed view), not a verbatim
    // same-representation array. `Type` PartialEq ignores the `sealed` flag, so equal inners ⇒ no rebuild.
    to_contains_sealed_array(to) && inner_from.as_ref() != inner_to.as_ref()
}

/// A field type permitted in a sealed record: a scalar (numeric or Bool) OR an eligible heap field
/// (String/Array/nested-sealed). Mirrors `Codegen::is_sealed_field`. A nested-sealed field recurses
/// into `is_sealed_scalar_repr`; a self-recursive type survives resolution as `Type::Named` (not an
/// inlined `Object`), so the recursion terminates and a cyclic record stays boxed (fail-safe).
fn is_sealed_field_ty(ty: &Type) -> bool {
    ty.is_flat_scalar()
        || matches!(ty, Type::Bool)
        || ty.is_string_ish()
        || matches!(ty, Type::Array(_) | Type::FixedArray(_))
        || is_sealed_scalar_repr(ty)
}

/// A type that participates in the OWNING reference model for var cells / module globals:
/// a cell/global holding such a value owns one independent reference to it. This covers
/// both concrete reference-counted heap values (`is_rc_type`) AND boxed Json/union values
/// (`is_union_ty`). For unions the retain/release carried `ty` causes codegen to dispatch
/// the tag-aware `lin_tagged_retain`/`lin_tagged_release` (which bump/drop the boxed
/// payload's refcount and are null/scalar/cached-box safe). Store, read, release-old and
/// teardown must ALL use this predicate together — an asymmetry causes a double-free
/// (release without matching retain) or a leak (retain without matching release).
fn needs_owning(ty: &Type) -> bool {
    is_rc_type(ty) || is_union_ty(ty)
}

/// STORE side of the owning model: produce a value the cell/global will OWN.
/// - concrete rc (`is_rc_type`): take an independent reference in place (`Retain`); the
///   stored temp is the same heap pointer, now with rc+1.
/// - union (`is_union_ty`): clone the box (`CloneBox` → `lin_tagged_clone`) so the cell owns
///   its OWN `TaggedVal*` (not an alias of a borrowed caller box); release-old can free it
///   safely. Returns the cloned temp to store.
/// - otherwise: no-op, returns the value unchanged.
/// Mirrors `own_for_read`; together with codegen's release-old these keep the four sides
/// (store/read/release-old/teardown) symmetric for both concrete and union slot types.
fn own_for_store(t: Temp, ty: &Type, builder: &mut FuncBuilder) -> Temp {
    if is_union_ty(ty) {
        let dst = builder.alloc_temp(ty.clone());
        builder.emit(Instruction::CloneBox { dst, src: t, ty: ty.clone() });
        dst
    } else if is_rc_type(ty) {
        builder.emit(Instruction::Retain { val: t, ty: ty.clone() });
        t
    } else {
        t
    }
}

/// Coerce a value to a (possibly union) slot type and produce a value the cell/global will
/// OWN, reclaiming any transient box created by the coercion.
///
/// When `slot_ty` is a union and the coercion boxes a concrete value (`value_ty` concrete),
/// the coercion allocates a FRESH transient `TaggedVal*` box `b` wrapping the raw value
/// (which is itself separately owned and released at scope exit). `own_for_store` then clones
/// `b` into the box the cell owns — so `b` is now an orphan whose inner is owned twice over
/// (once by the raw value's scope-exit release, once by the cell's clone). We therefore free
/// `b`'s 16-byte shell (NOT its inner) to avoid a per-store box leak. When no transient box
/// was created (already-union value, or non-union slot), nothing extra is freed.
fn coerce_and_own_store(t: Temp, value_ty: &Type, slot_ty: &Type, builder: &mut FuncBuilder) -> Temp {
    let made_fresh_box = is_union_ty(slot_ty) && !is_union_ty(value_ty) && type_repr_differs(value_ty, slot_ty);
    let coerced = coerce_to_slot_type(t, value_ty, slot_ty, builder);
    let stored = own_for_store(coerced, slot_ty, builder);
    if made_fresh_box {
        builder.emit(Instruction::FreeBoxShell { val: coerced });
    }
    stored
}

/// READ side of the owning model: take an independently-owned copy of a value just loaded
/// from a cell/global and register it for scope-exit release.
/// - concrete rc: `Retain` in place + register the same temp.
/// - union: `CloneBox` into a fresh temp (the reader owns its own box; releasing it at scope
///   exit never frees the cell's box) + register the cloned temp.
/// Returns the temp to use as the read result.
fn own_for_read(t: Temp, ty: &Type, builder: &mut FuncBuilder) -> Temp {
    if is_union_ty(ty) {
        let dst = builder.alloc_temp(ty.clone());
        builder.emit(Instruction::CloneBox { dst, src: t, ty: ty.clone() });
        builder.register_owned(dst, ty.clone());
        dst
    } else if is_rc_type(ty) {
        builder.emit(Instruction::Retain { val: t, ty: ty.clone() });
        builder.register_owned(t, ty.clone());
        t
    } else {
        t
    }
}

/// Borrowed lowering of an `Index`/`FieldGet` BASE — the array/object container is only read
/// through (one element/field extracted), never stored, so it needs no owning reference. This
/// skips the `own_for_read` Retain+scope-release that `lower_expr` of a `LocalGet` would emit
/// for a CONCRETE-RC container (Array/FixedArray/Object), eliminating a retain/release PAIR per
/// element read — the dominant cost of tight index loops over a module-`var`/captured array
/// (e.g. a linear-scan PQ: `pqDist[j] < pqDist[best]` was 2 retain + 2 release per element).
///
/// Soundness: the container's true owner — the module global, the heap cell, or the enclosing
/// owned local — outlives this single read (nothing reassigns it between the load and the
/// `Index`/`FieldGet` that immediately consumes the returned temp). The element-dup at the
/// `Index`/`FieldGet` site still gives the projected VALUE its own reference when that value is
/// itself RC, so consumers are unaffected.
///
/// Returns `Some(borrowed_temp)` when the borrow shortcut applies; `None` to fall back to the
/// normal owning `lower_expr`. Conservative: only a bare `LocalGet` of a concrete-RC container
/// (never a union/Json container, never a non-`LocalGet` expression whose own evaluation might
/// allocate or call) qualifies. Whenever this returns `Some`, `lower_container_base_borrowed_check`
/// for the same `object`/`ctx` returns `true` (the eligibility predicate the caller uses to pick
/// key/base lowering order).
fn lower_container_base_borrowed(
    object: &TypedExpr,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Option<Temp> {
    let TypedExpr::LocalGet { slot, ty, .. } = object else { return None };
    if is_union_ty(ty) || !is_rc_type(ty) {
        return None;
    }
    // Module-level mutable `var` (global): plain load, no owning clone.
    if ctx.global_var_slots.contains(slot) {
        let gty = ctx.global_val_slots.get(slot).cloned().unwrap_or_else(|| ty.clone());
        // A narrowed (union global read as concrete) base would need a Coerce + in-place retain;
        // keep that on the owning path. Only the same-representation concrete case borrows.
        if is_union_ty(&gty) {
            return None;
        }
        let dst = builder.alloc_temp(gty.clone());
        // Module-level `var` (mutable global): never foldable, so `immutable: false`.
        builder.emit(Instruction::GlobalValGet { dst, slot: *slot, ty: gty, immutable: false });
        return Some(dst);
    }
    // Mutably-captured `var` (heap cell): plain load through the cell, no owning clone.
    if let Some(cell_ty) = builder.cell_slots.get(slot).cloned() {
        if is_union_ty(&cell_ty) {
            return None;
        }
        if let Some(&cell) = builder.slots.get(slot) {
            let dst = builder.alloc_temp(cell_ty.clone());
            builder.emit(Instruction::CellGet { dst, cell, ty: cell_ty });
            return Some(dst);
        }
    }
    // Plain local slot: the temp already holds the container; reuse it directly (no retain).
    if let Some(&t) = builder.slots.get(slot) {
        let stored_ty = builder.temp_types.get(&t).cloned().unwrap_or_else(|| ty.clone());
        if is_union_ty(&stored_ty) {
            return None;
        }
        return Some(t);
    }
    None
}

/// Eligibility predicate for `lower_container_base_borrowed`, WITHOUT emitting any IR. Mirrors its
/// guards exactly so the `Index` caller can decide key/base lowering order before committing.
/// Must stay in lockstep with `lower_container_base_borrowed` (every `Some` path here is `true`).
fn lower_container_base_borrowed_check(object: &TypedExpr, ctx: &LowerCtx) -> bool {
    let TypedExpr::LocalGet { slot, ty, .. } = object else { return false };
    if is_union_ty(ty) || !is_rc_type(ty) {
        return false;
    }
    if ctx.global_var_slots.contains(slot) {
        let gty = ctx.global_val_slots.get(slot).unwrap_or(ty);
        return !is_union_ty(gty);
    }
    // Cell or plain-local container: eligible unless its stored representation is a union/Json.
    // (Both load paths in the emitter borrow without a retain; the emitter re-checks the type.)
    true
}

/// Collect `var` slots that are mutably captured by any (possibly nested) closure within
/// a statement. Such slots are stored as heap cells shared by reference.
/// Collect `var` slots of an OWNING (rc/union) type that are reassigned INSIDE an `if`/`match`
/// branch. Such a var cannot be a plain SSA temp (release-old-on-overwrite and per-branch join
/// ownership are unrepresentable — the superseded initial value leaks on the taken branch and the
/// slot can dangle), so it is routed through a heap cell (which handles release-old + coherent
/// post-join reads). We record the declared type of every `var` we descend past so that when a
/// reassignment to it is seen inside a branch we can tell whether it needs owning. Nested function
/// bodies are NOT descended into here: their `var`s have their own slot namespace and their own
/// pre-scan; a capture of an OUTER var becomes a cell via the capture analysis instead.
fn collect_branch_reassigned_var_slots_stmt(
    stmt: &TypedStmt,
    in_branch: bool,
    owning_vars: &mut HashMap<usize, Type>,
    out: &mut std::collections::HashSet<usize>,
) {
    match stmt {
        TypedStmt::Var { slot, ty, value, .. } => {
            if needs_owning(ty) {
                owning_vars.insert(*slot, ty.clone());
            }
            collect_branch_reassigned_var_slots_expr(value, in_branch, owning_vars, out);
        }
        TypedStmt::Val { value, .. } => {
            collect_branch_reassigned_var_slots_expr(value, in_branch, owning_vars, out);
        }
        TypedStmt::Expr(e) => collect_branch_reassigned_var_slots_expr(e, in_branch, owning_vars, out),
        TypedStmt::Destructure { value, .. } | TypedStmt::ArrayDestructure { value, .. } => {
            collect_branch_reassigned_var_slots_expr(value, in_branch, owning_vars, out);
        }
        TypedStmt::Import { .. } | TypedStmt::ForeignImport { .. } => {}
    }
}

fn collect_branch_reassigned_var_slots_expr(
    expr: &TypedExpr,
    in_branch: bool,
    owning_vars: &mut HashMap<usize, Type>,
    out: &mut std::collections::HashSet<usize>,
) {
    let recur =
        |e: &TypedExpr, b: bool, ov: &mut HashMap<usize, Type>, o: &mut std::collections::HashSet<usize>| {
            collect_branch_reassigned_var_slots_expr(e, b, ov, o)
        };
    match expr {
        TypedExpr::LocalSet { slot, value, .. } => {
            if in_branch && owning_vars.contains_key(slot) {
                out.insert(*slot);
            }
            recur(value, in_branch, owning_vars, out);
        }
        // A nested function body owns its own slot namespace; descend with a FRESH owning-var map
        // and reset the branch flag — its declarations and reassignments are scoped to it.
        TypedExpr::Function { body, .. } => {
            let mut inner_owning: HashMap<usize, Type> = HashMap::new();
            collect_branch_reassigned_var_slots_expr(body, false, &mut inner_owning, out);
        }
        TypedExpr::Block { stmts, expr, .. } => {
            for s in stmts {
                collect_branch_reassigned_var_slots_stmt(s, in_branch, owning_vars, out);
            }
            recur(expr, in_branch, owning_vars, out);
        }
        // Reassignments inside ANY branch arm need a cell. The condition/scrutinee runs
        // unconditionally, but descending it with `in_branch` unchanged is harmless (a reassign in
        // a condition is itself only sound as a cell anyway).
        TypedExpr::If { cond, then_br, else_br, .. } => {
            recur(cond, in_branch, owning_vars, out);
            recur(then_br, true, owning_vars, out);
            recur(else_br, true, owning_vars, out);
        }
        TypedExpr::Match { scrutinee, arms, .. } => {
            recur(scrutinee, in_branch, owning_vars, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    recur(g, true, owning_vars, out);
                }
                recur(&arm.body, true, owning_vars, out);
            }
        }
        TypedExpr::Call { func, args, .. } => {
            recur(func, in_branch, owning_vars, out);
            for a in args {
                recur(a, in_branch, owning_vars, out);
            }
        }
        TypedExpr::BinaryOp { left, right, .. } => {
            recur(left, in_branch, owning_vars, out);
            recur(right, in_branch, owning_vars, out);
        }
        TypedExpr::UnaryOp { operand, .. } => recur(operand, in_branch, owning_vars, out),
        TypedExpr::Coerce { expr, .. } => recur(expr, in_branch, owning_vars, out),
        TypedExpr::MakeArray { elements, .. } => {
            for e in elements {
                recur(e, in_branch, owning_vars, out);
            }
        }
        TypedExpr::MakeObject { fields, spreads, .. } => {
            for (_, v) in fields {
                recur(v, in_branch, owning_vars, out);
            }
            for s in spreads {
                recur(s, in_branch, owning_vars, out);
            }
        }
        TypedExpr::Index { object, key, .. } => {
            recur(object, in_branch, owning_vars, out);
            recur(key, in_branch, owning_vars, out);
        }
        TypedExpr::IndexSet { object, key, value, .. } => {
            recur(object, in_branch, owning_vars, out);
            recur(key, in_branch, owning_vars, out);
            recur(value, in_branch, owning_vars, out);
        }
        TypedExpr::FieldGet { object, .. } => recur(object, in_branch, owning_vars, out),
        TypedExpr::Is { expr, .. } | TypedExpr::Has { expr, .. } => recur(expr, in_branch, owning_vars, out),
        TypedExpr::StringInterp { parts, .. } => {
            for p in parts {
                if let TypedStringPart::Expr(e) = p {
                    recur(e, in_branch, owning_vars, out);
                }
            }
        }
        _ => {}
    }
}

fn collect_mutable_capture_slots_stmt(stmt: &TypedStmt, out: &mut std::collections::HashSet<usize>) {
    match stmt {
        TypedStmt::Val { value, .. } | TypedStmt::Var { value, .. } => {
            collect_mutable_capture_slots_expr(value, out);
        }
        TypedStmt::Expr(e) => collect_mutable_capture_slots_expr(e, out),
        TypedStmt::Destructure { value, .. } | TypedStmt::ArrayDestructure { value, .. } => {
            collect_mutable_capture_slots_expr(value, out);
        }
        TypedStmt::Import { .. } | TypedStmt::ForeignImport { .. } => {}
    }
}

fn collect_mutable_capture_slots_expr(expr: &TypedExpr, out: &mut std::collections::HashSet<usize>) {
    match expr {
        TypedExpr::Function { captures, body, .. } => {
            for cap in captures {
                if cap.is_mutable {
                    out.insert(cap.outer_slot);
                }
            }
            collect_mutable_capture_slots_expr(body, out);
        }
        TypedExpr::Block { stmts, expr, .. } => {
            for s in stmts { collect_mutable_capture_slots_stmt(s, out); }
            collect_mutable_capture_slots_expr(expr, out);
        }
        TypedExpr::If { cond, then_br, else_br, .. } => {
            collect_mutable_capture_slots_expr(cond, out);
            collect_mutable_capture_slots_expr(then_br, out);
            collect_mutable_capture_slots_expr(else_br, out);
        }
        TypedExpr::Match { scrutinee, arms, .. } => {
            collect_mutable_capture_slots_expr(scrutinee, out);
            for arm in arms {
                if let Some(g) = &arm.guard { collect_mutable_capture_slots_expr(g, out); }
                collect_mutable_capture_slots_expr(&arm.body, out);
            }
        }
        TypedExpr::Call { func, args, .. } => {
            collect_mutable_capture_slots_expr(func, out);
            for a in args { collect_mutable_capture_slots_expr(a, out); }
        }
        TypedExpr::BinaryOp { left, right, .. } => {
            collect_mutable_capture_slots_expr(left, out);
            collect_mutable_capture_slots_expr(right, out);
        }
        TypedExpr::UnaryOp { operand, .. } => {
            collect_mutable_capture_slots_expr(operand, out);
        }
        TypedExpr::Coerce { expr, .. } | TypedExpr::LocalSet { value: expr, .. } => {
            collect_mutable_capture_slots_expr(expr, out);
        }
        TypedExpr::MakeArray { elements, .. } => {
            for e in elements { collect_mutable_capture_slots_expr(e, out); }
        }
        TypedExpr::MakeObject { fields, spreads, .. } => {
            for (_, v) in fields { collect_mutable_capture_slots_expr(v, out); }
            for s in spreads { collect_mutable_capture_slots_expr(s, out); }
        }
        TypedExpr::Index { object, key, .. } => {
            collect_mutable_capture_slots_expr(object, out);
            collect_mutable_capture_slots_expr(key, out);
        }
        TypedExpr::IndexSet { object, key, value, .. } => {
            collect_mutable_capture_slots_expr(object, out);
            collect_mutable_capture_slots_expr(key, out);
            collect_mutable_capture_slots_expr(value, out);
        }
        TypedExpr::FieldGet { object, .. } => collect_mutable_capture_slots_expr(object, out),
        TypedExpr::Is { expr, .. } | TypedExpr::Has { expr, .. } => {
            collect_mutable_capture_slots_expr(expr, out);
        }
        TypedExpr::StringInterp { parts, .. } => {
            for p in parts {
                if let TypedStringPart::Expr(e) = p { collect_mutable_capture_slots_expr(e, out); }
            }
        }
        _ => {}
    }
}

/// Mangle an import path into the LLVM symbol prefix codegen uses for that module's
/// exports. Must match `register_import`'s `path.replace("/", "_").replace("-", "_")`.
pub fn mangle_module_key(path: &str) -> String {
    path.replace(['/', '-'], "_")
}

/// A type stored at runtime as a TaggedVal* pointer (Json/union/dynamic).
/// Mirrors codegen's `Codegen::is_union_type`. `Shared<T>` is a boxed `TaggedVal*(TAG_SHARED)`,
/// so it belongs here: it follows the OWNING model and its RC dispatches through the tag-aware
/// `lin_tagged_retain`/`lin_tagged_release`, whose TAG_SHARED arm does the atomic box rc.
/// `Stream<T>` is likewise a boxed `TaggedVal*(TAG_STREAM)` whose RC dispatches through the
/// tag-aware path (the TAG_STREAM arm decrements the stream box's refcount, closing the fd at
/// zero) — so it is owning too.
fn is_union_ty(ty: &Type) -> bool {
    matches!(ty, Type::Union(_) | Type::TypeVar(_) | Type::Named(_) | Type::Shared(_) | Type::Stream(_))
}

/// True if `ty` IS a `Stream` or a `Union` containing one. A streamish capture crosses a thread
/// boundary by MOVE (CAP_MOVE), not copy. Mirrors `lin_check::checker::expr::type_is_streamish`.
fn type_is_streamish_ir(ty: &Type) -> bool {
    match ty {
        Type::Stream(_) => true,
        Type::Union(variants) => variants.iter().any(type_is_streamish_ir),
        _ => false,
    }
}

/// A concrete heap-allocated value type whose box wraps a refcounted heap pointer
/// (Str/Array/FixedArray/Object/Iterator). Boxing one of these into a Json/union param
/// (via Coerce → `lin_box_str`/`lin_box_array`/`lin_box_object`) allocates a FRESH 16-byte
/// `TaggedVal*` shell whose inner is the (separately owned) heap pointer. Scalars
/// (int/bool/float/null) are excluded: their boxes may be cached/immutable.
fn is_heap_ty(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Str | Type::StrLit(_) | Type::Array(_) | Type::FixedArray(_) | Type::Object { .. } | Type::Iterator(_)
    )
}

/// Whether passing an argument of `arg_ty` to a parameter of `param_ty` causes
/// `lower_coerce_arg` to box a CONCRETE HEAP value into a fresh, caller-owned `TaggedVal*`
/// shell. The shell's inner heap pointer is owned separately (released by the arg's own
/// scope-exit release), so after the call the caller must free ONLY the shell.
/// True iff: param is union, arg is concrete heap. Excludes already-union args (the box
/// belongs to someone else) and scalar args (cached boxes).
fn arg_box_is_caller_owned_shell(arg_ty: &Type, param_ty: Option<&Type>) -> bool {
    match param_ty {
        // A SEALED-RECORD ARRAY *or a SEALED SCALAR RECORD* boxed into a Json param MATERIALIZES a
        // FRESH inner heap value (a tagged `Object[]` for the array; a `box_object(LinObject)` for the
        // record) whose inner is NOT borrowed, so freeing only the shell would leak the fresh inner
        // array/object + its element/field references. Both are FULLY released via the owning model
        // instead (see the call-arg loop's `full_release_boxes`), so neither is a shell-only box.
        Some(p) => is_union_ty(p) && !is_union_ty(arg_ty) && is_heap_ty(arg_ty)
            && !is_sealed_scalar_array(arg_ty)
            && !is_sealed_scalar_repr(arg_ty),
        None => false,
    }
}

/// Whether passing a SCALAR (Int/Float/Bool/Null) argument to a `Json`/union parameter boxes it
/// into a fresh, caller-owned `TaggedVal*` shell (`lin_box_int32`/`box_float64`/…) that the callee
/// borrows and never releases. A NON-cached scalar box (a large int / any float) is a fresh heap
/// shell with NO heap inner payload, so reclaiming the SHELL after the call balances it. Freeing is
/// done via `FreeBoxShellIfDistinct` → `lin_tagged_free_box_if_distinct`, which is CACHED-BOX SAFE
/// (small-int/bool boxes are immortal statics and never freed) and result-alias safe (a callee that
/// returns its Json param hands the same box back as the result — skipped when shell == result).
/// Without this, `f(1_000_000)` / `f(3.14)` into a Json param leaked the 16-byte box shell per call.
fn arg_box_is_caller_owned_scalar_shell(arg_ty: &Type, param_ty: Option<&Type>) -> bool {
    match param_ty {
        Some(p) => is_union_ty(p) && !is_union_ty(arg_ty) && !is_heap_ty(arg_ty)
            && !is_sealed_scalar_repr(arg_ty),
        None => false,
    }
}

/// Retain a Function-typed argument that is NOT a freshly-made closure before passing it
/// to a call. AST-compiled callees release their Function-typed parameters at return; a
/// borrowed (non-fresh) closure must be retained to balance that, while a fresh closure's
/// existing +1 is consumed by the callee. Mirrors `call_global_fn`'s `arg_is_fn_owned`.
fn retain_call_arg(arg: Temp, ty: &Type, _is_fresh: bool, builder: &mut FuncBuilder) {
    if matches!(ty, Type::Function { .. }) {
        builder.emit(Instruction::Retain { val: arg, ty: ty.clone() });
    }
}

/// Whether an argument expression produces a freshly-allocated value (a function/closure
/// literal, a literal allocation, or a call result) whose +1 reference can be transferred
/// to a consuming callee or container. Mirrors AST `expr_is_owned_alloc` exactly.
fn expr_is_fresh_alloc(expr: &TypedExpr) -> bool {
    match expr {
        TypedExpr::Call { .. }
        | TypedExpr::MakeArray { .. }
        | TypedExpr::MakeObject { .. }
        | TypedExpr::StringLit { .. }
        | TypedExpr::StringInterp { .. }
        | TypedExpr::Function { .. } => true,
        // If/Match are owned iff every branch/arm is owned (exactly one runs per execution).
        TypedExpr::If { then_br, else_br, .. } => {
            expr_is_fresh_alloc(then_br) && expr_is_fresh_alloc(else_br)
        }
        TypedExpr::Match { arms, .. } => {
            !arms.is_empty() && arms.iter().all(|a| expr_is_fresh_alloc(&a.body))
        }
        TypedExpr::Block { expr, .. } => expr_is_fresh_alloc(expr),
        TypedExpr::Coerce { expr, .. } => expr_is_fresh_alloc(expr),
        _ => false,
    }
}

/// After a (non-tail) call, free the 16-byte `TaggedVal*` SHELL of each argument box that
/// WE freshly allocated by coercing a concrete heap value into a Json/union parameter (see
/// `arg_box_is_caller_owned_shell`). Json/union params are BORROWED: the callee never
/// releases them (`lower_function_expr_with_id`'s param scope only registers Function-typed
/// params for release — the universal convention for every Lin function, incl. stdlib
/// for/map/filter/reduce), so the caller owns and must reclaim the shell.
///
/// Frees only the shell, never the inner heap payload (that pointer is owned separately and
/// released by the arg's own scope-exit release — freeing it here would double-free).
///
/// Uses `FreeBoxShellIfDistinct` against the call result `dst`: a callee that simply returns
/// its Json param (e.g. an identity/pass-through) hands the very same box back as the result,
/// which the caller now owns (`register_owned(dst)`) and will release later — freeing that
/// shell here would corrupt the returned value, so we skip it when the shell == result.
fn free_arg_box_shells(shell_boxes: &[Temp], dst: Temp, builder: &mut FuncBuilder) {
    for &shell in shell_boxes {
        builder.emit(Instruction::FreeBoxShellIfDistinct { val: shell, other: dst });
    }
}

/// Coerce a call argument to the callee's declared parameter type: box a concrete value
/// for a Json/union param, OR widen/narrow a numeric mismatch (e.g. an Int32 literal `0`
/// passed to an Int64 param) so the call signature matches.
fn lower_coerce_arg(arg: Temp, arg_ty: &Type, param_ty: Option<&Type>, builder: &mut FuncBuilder) -> Temp {
    let Some(param_ty) = param_ty else { return arg; };
    // A sealed scalar-record arg flowing into a `Named` param (a recursive/self-referential type
    // reference that resolution left unexpanded) is passed THROUGH unchanged: the callee reads
    // the SAME unresolved `Named` param consistently as the sealed struct (its body's FieldGet
    // obj_ty is the expanded sealed Object), so the struct pointer flows as an opaque ptr without
    // a representation change. Without this, the `Named`-is-union-ish check below would box the
    // sealed struct (materialize → box_object) at e.g. a recursive self-call, storing a boxed
    // object back into a slot the body reads as a struct → garbage (caught by b_access). For
    // Stage 1, the only sealed-producing types are non-recursive named records whose `Named`
    // resolves to that sealed Object, so this pass-through is the correct representation.
    if is_sealed_scalar_repr(arg_ty) && matches!(param_ty, Type::Named(_)) {
        return arg;
    }
    // An UNSEALED object value (e.g. a record LITERAL, which `lower_expr` builds as a boxed
    // LinObject) flowing into a `Named` param that the callee reads as a SEALED struct. This is
    // the self-recursive-call case: the outer binding's function type resolves the param to the
    // sealed `Object`, but inside the body the recursive reference still carries the unexpanded
    // `Named` alias, so `func.ty()` hands us `Named(_)` here. The union arm below would box the
    // literal as Json — which the callee (reading constant struct offsets) then misreads as a
    // struct → heap corruption / segfault. Since the literal's fields are all sealed-eligible and
    // structural compatibility guarantees it has the named type's shape, PROJECT it into the
    // sealed struct layout (a fresh +1 owned struct) so the representation matches the callee.
    if matches!(param_ty, Type::Named(_)) {
        if let Type::Object { fields, sealed: false } = arg_ty {
            if !fields.is_empty() && fields.values().all(is_sealed_field_ty) {
                let sealed_ty = Type::sealed_object(fields.clone());
                let dst = builder.alloc_temp(sealed_ty.clone());
                builder.emit(Instruction::Coerce {
                    dst,
                    src: arg,
                    from_ty: arg_ty.clone(),
                    to_ty: sealed_ty.clone(),
                });
                builder.register_owned(dst, sealed_ty);
                return dst;
            }
        }
    }
    // SEALED-RECORD ARRAY BOUNDARY (Problem A / Stage 3b): a sealed-record array (packed/contiguous
    // representation, elem_tag 0xFE) flowing into a param that is NOT itself that same sealed-array
    // representation — a generic TypeVar/Json array (the type-erased combinator fallback's param),
    // an unsealed `Object[]`, or any boxed array — must be MATERIALIZED to the boxed tagged `Object[]`
    // view (`sealed_array_to_tagged`), else the callee reads the packed buffer through the boxed
    // `lin_array_get_tagged`/`lin_object_get` machinery → misaligned deref / corruption. The reverse
    // (boxed → sealed array) is the param's own sealed-scalar-array case, handled by `type_repr_differs`
    // below / the result Coerce. The materialized tagged array is a fresh +1 the arg scope releases.
    // Fire ONLY when the param element is a genuinely BOXED representation (a generic TypeVar/Json
    // wildcard, a union, or an unsealed `Object` — i.e. the type-erased boxed-fallback combinator's
    // `T[]` param). A param array whose element is a `Named` (an unexpanded self-referential alias,
    // the SELF-RECURSIVE-call case — e.g. `sumX(arr: Point[], …)` calling itself) reads the SAME
    // packed sealed struct consistently, so it must be passed THROUGH unchanged: materializing it
    // would store a boxed Object[] back into a slot the loop body reads as a packed array (observed:
    // recursive `arr[i].x` sum returned garbage after the first iteration). Mirrors the `Named`
    // pass-through guards above.
    if is_sealed_scalar_array(arg_ty)
        && !is_sealed_scalar_array(param_ty)
        && param_elem_is_boxed_repr(param_ty)
    {
        let dst = builder.alloc_temp(param_ty.clone());
        builder.emit(Instruction::Coerce { dst, src: arg, from_ty: arg_ty.clone(), to_ty: param_ty.clone() });
        // The materialized tagged `Object[]` is a FRESH fully-owned +1 the callee BORROWS (a TypeVar/
        // Json-array param is not released by the owning model). It is released right after the call by
        // the `sealed_array_arg_materialized` branch in the call-site arg loop (matching the existing
        // sealed-array→Json-param `full_release_boxes` path) — NOT registered owned here. The source
        // sealed array keeps its own ownership.
        return dst;
    }
    // Box/unbox across the union boundary.
    if is_union_ty(param_ty) != is_union_ty(arg_ty) {
        let dst = builder.alloc_temp(param_ty.clone());
        builder.emit(Instruction::Coerce { dst, src: arg, from_ty: arg_ty.clone(), to_ty: param_ty.clone() });
        // UNBOX (union arg → concrete param): `dst` aliases the SOURCE box's inner heap payload
        // (e.g. `concat(b,b)` returns a boxed array, unboxed to the `UInt8[]` param). The source
        // box `arg` stays registered owned in scope, so if this arg flows into a self-tail-call
        // (`doubleUp(concat(b,b), n)`), `release_owned_for_tail_call` must NOT fully release the
        // box — that would free the very array now living in the param slot (a double-free). Record
        // the alias so the box is treated as kept (its shell is left to the dead-block release, the
        // pre-existing accepted per-tail-call shell leak). The box shell still survives non-tail use
        // normally. Only matters when arg is unboxed from an owned box (union → concrete).
        if is_union_ty(arg_ty) && !is_union_ty(param_ty) {
            builder.record_escape_alias(dst, arg);
        }
        return dst;
    }
    // Numeric width/kind mismatch between two concrete numeric types.
    if arg_ty.is_numeric() && param_ty.is_numeric() && arg_ty != param_ty {
        let dst = builder.alloc_temp(param_ty.clone());
        builder.emit(Instruction::Coerce { dst, src: arg, from_ty: arg_ty.clone(), to_ty: param_ty.clone() });
        return dst;
    }
    // Sealed scalar-record boundary: a wider/Json/unsealed (or differently-shaped sealed) argument
    // flowing into a sealed scalar-record param must be PROJECTED into the param's struct layout
    // (and a sealed arg into a Json/unsealed param MATERIALIZED). Without this a DIRECT call passes
    // a boxed LinObject straight into a function that reads struct offsets → garbage. Mirrors
    // `type_repr_differs`'s sealed arm.
    if type_repr_differs(arg_ty, param_ty) {
        let dst = builder.alloc_temp(param_ty.clone());
        builder.emit(Instruction::Coerce { dst, src: arg, from_ty: arg_ty.clone(), to_ty: param_ty.clone() });
        // A projection that produces a sealed scalar record is a fresh +1 owned struct: register
        // it so the call's arg-scope releases it (sealed release path). Materialization to a boxed
        // object is likewise fresh and registered by its own (object) owning model downstream.
        if is_sealed_scalar_repr(param_ty) {
            builder.register_owned(dst, param_ty.clone());
        }
        return dst;
    }
    arg
}

/// Lower `value` directly into a sealed scalar-record `slot_ty` AS a packed struct when `value`
/// is an object LITERAL providing exactly (at least) the target fields, with no spreads. Returns
/// `Some(temp)` having constructed the struct in place (field values stored by offset) — skipping
/// the build-boxed-LinObject-then-project round-trip the generic coercion would otherwise pay
/// (an `lin_object_alloc` + N sets + N `lin_object_get`). Returns `None` when the fast path does
/// not apply (caller falls back to `lower_expr` + `coerce_to_slot_type`). This is the construction
/// half of the sealed-records win (sealed-records Stage 1); it fires for `val p: T = { … }`,
/// `(…): T => { … }` returns, and arg/assignment boundaries.
fn try_lower_sealed_literal(
    value: &TypedExpr,
    slot_ty: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Option<Temp> {
    if !is_sealed_scalar_repr(slot_ty) {
        return None;
    }
    let (fields, spreads) = match value {
        TypedExpr::MakeObject { fields, spreads, .. } => (fields, spreads),
        _ => return None,
    };
    if !spreads.is_empty() {
        return None;
    }
    let Type::Object { fields: target_fields, .. } = slot_ty else { return None };
    if !target_fields.keys().all(|k| fields.iter().any(|(fk, _)| fk == k)) {
        return None;
    }
    let lowered_fields: Vec<(String, Temp)> =
        fields.iter().map(|(k, v)| (k.clone(), lower_expr(v, builder, ctx))).collect();
    let dst = builder.alloc_temp(slot_ty.clone());
    builder.emit(Instruction::MakeObject { dst, fields: lowered_fields, spreads: vec![], ty: slot_ty.clone(), stack: false });
    builder.register_owned(dst, slot_ty.clone());
    Some(dst)
}

/// FUSED `arr[i].field` / `arr[i]["field"]` over a SEALED-RECORD ARRAY (Stage 3). When `object`
/// is `Index{ array, key }` with `array` an all-scalar sealed-record array, and `field` is a SCALAR
/// field of the element record, emit a single `SealedArrayFieldGet` (a constant-offset load from the
/// contiguous element) instead of materializing a standalone sealed struct for the element then
/// reading its field. Returns `Some(dst)` on the fast path, `None` to fall through to the generic
/// path. Sound because the field is a scalar (no RC, no escaping interior pointer). The array base
/// is BORROWED where possible (a bare local) so no retain/release pair is paid in the hot loop.
fn try_lower_sealed_array_field(
    object: &TypedExpr,
    field: &str,
    result_ty: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Option<Temp> {
    let TypedExpr::Index { object: array, key, .. } = object else { return None };
    let arr_ty = array.ty();
    if !is_sealed_scalar_array(&arr_ty) {
        return None;
    }
    // The element field must be a scalar (the only sound, RC-free fused read). A heap-field element
    // record is not a sealed-scalar-array anyway (gated above), so this is belt-and-braces.
    let elem_field_scalar = match &arr_ty {
        Type::Array(elem) => matches!(elem.as_ref(), Type::Object { fields, .. }
            if fields.get(field).map(|f| f.is_flat_scalar() || matches!(f, Type::Bool)).unwrap_or(false)),
        _ => false,
    };
    if !elem_field_scalar {
        return None;
    }
    // Lower the index, then the (borrowed where possible) array base last — mirrors the Index
    // borrow-ordering rule so a key that reassigns the array global can't dangle a borrowed base.
    let (array_temp, index_temp) = if lower_container_base_borrowed_check(array, ctx) {
        let index_temp = lower_expr(key, builder, ctx);
        let array_temp = lower_container_base_borrowed(array, builder, ctx)
            .unwrap_or_else(|| lower_expr(array, builder, ctx));
        (array_temp, index_temp)
    } else {
        let array_temp = lower_expr(array, builder, ctx);
        let index_temp = lower_expr(key, builder, ctx);
        (array_temp, index_temp)
    };
    let dst = builder.alloc_temp(result_ty.clone());
    builder.emit(Instruction::SealedArrayFieldGet {
        dst,
        array: array_temp,
        index: index_temp,
        field: field.to_string(),
        arr_ty,
        result_ty: result_ty.clone(),
    });
    Some(dst)
}

/// Lower `value` into a slot of declared type `slot_ty`, producing a temp in the slot's
/// representation. Uses the sealed-literal direct-construction fast path when applicable
/// (`try_lower_sealed_literal`), otherwise `lower_expr` + `coerce_to_slot_type`.
fn lower_value_into_slot(
    value: &TypedExpr,
    slot_ty: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Temp {
    if let Some(t) = try_lower_sum_literal(value, slot_ty, builder, ctx) {
        return t;
    }
    if let Some(t) = try_lower_sealed_literal(value, slot_ty, builder, ctx) {
        return t;
    }
    let t = lower_expr(value, builder, ctx);
    coerce_to_slot_type_owning_bind(t, &value.ty(), slot_ty, builder)
}

/// UNBOXED SUM TYPE (unboxed-sumtype Stage 1) — direct SumNode construction fast path. When `value`
/// is an object literal (no spreads) flowing into a Stage-1-eligible sum-type slot, emit a
/// `MakeObject` whose `ty` IS the sum type so the repr pass labels the temp `Packed(SumNode)` and
/// codegen's MakeObject branch packs it DIRECTLY via `sumnode_construct` — skipping the
/// build-a-boxed-`LinObject`-then-`sumnode_project_from_boxed` round-trip the generic coercion path
/// would otherwise pay every construction (the dominant cost the sum-dispatch benchmark exposed).
/// The discriminant field must be a `StrLit` (the variant tag); otherwise fall through (None).
fn try_lower_sum_literal(
    value: &TypedExpr,
    slot_ty: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Option<Temp> {
    if !crate::repr::sum_type_eligible(slot_ty) {
        return None;
    }
    let (fields, spreads) = match value {
        TypedExpr::MakeObject { fields, spreads, .. } => (fields, spreads),
        _ => return None,
    };
    if !spreads.is_empty() {
        return None;
    }
    // The discriminant field's value must be a string literal so codegen can statically pick the
    // variant tag. Codegen reads the disc field TEMP's type as `StrLit` (a string const is otherwise
    // typed plain `Str`), so force the disc temp's recorded type to `StrLit(value)`.
    let disc_key = crate::repr::sum_type_discriminant_of(slot_ty)?;
    // The variant this literal builds (selected by its discriminant StrLit value) — used to find the
    // declared field types so a RECURSIVE CHILD field's nested literal is pushed into its child sum
    // slot (so it constructs a `SumNode`, not a boxed object). NESTED-LITERAL DISCRIMINANT PUSHDOWN
    // (design §6 gap 1).
    let variant_fields: Option<indexmap::IndexMap<String, Type>> = fields
        .iter()
        .find(|(k, _)| k == &disc_key)
        .and_then(|(_, v)| match v {
            TypedExpr::StringLit(s, _, _) => crate::repr::sumnode_variant_by_disc(slot_ty, s),
            _ => None,
        });
    // The unique recursive self-name: a variant field typed `Named(self_name)` is a recursive child
    // whose CHILD SUM TYPE is `slot_ty` itself (direct self-recursion). Used to push a nested literal
    // into the right child sum slot so it constructs a `SumNode`.
    let self_name = crate::repr::sum_recursive_self_name(slot_ty);
    // Recursive-child temps whose ownership TRANSFERS into the constructed SumNode (the node owns +1
    // of each child; codegen does NOT retain at the store). Recorded with their source expr so we can
    // apply `transfer_into_container` AFTER the MakeObject: a fresh child literal is unregistered (its
    // +1 moves into the node), a borrowed child sub-expression is retained.
    let mut recursive_children: Vec<(Temp, &TypedExpr)> = Vec::new();
    let lowered_fields: Vec<(String, Temp)> = fields
        .iter()
        .map(|(k, v)| {
            // RECURSIVE CHILD pushdown (design §6 gap 1): a field whose variant slot type is a
            // recursive self-child (`Named(self_name)`) carries a nested literal that must be
            // constructed AS a SumNode. Lower it into the child sum slot (== `slot_ty`, recursing
            // through `try_lower_sum_literal`) instead of as a plain (boxed) object literal.
            let is_recursive_child = self_name.as_deref().is_some_and(|n| {
                matches!(variant_fields.as_ref().and_then(|vf| vf.get(k)),
                    Some(Type::Named(fn_name)) if fn_name == n)
            });
            let t = if is_recursive_child {
                lower_value_into_slot(v, slot_ty, builder, ctx)
            } else {
                lower_expr(v, builder, ctx)
            };
            if is_recursive_child {
                recursive_children.push((t, v));
            }
            if k == &disc_key {
                if let TypedExpr::StringLit(s, _, _) = v {
                    builder.temp_types.insert(t, Type::StrLit(s.clone()));
                }
            }
            (k.clone(), t)
        })
        .collect();
    // Bail if the discriminant didn't resolve to a StrLit temp (e.g. a computed key) — fall back to
    // the boxed coercion path, which `sumnode_project_from_boxed` handles soundly.
    let disc_ok = lowered_fields.iter().any(|(k, t)| {
        k == &disc_key && matches!(builder.temp_types.get(t), Some(Type::StrLit(_)))
    });
    if !disc_ok {
        return None;
    }
    let dst = builder.alloc_temp(slot_ty.clone());
    builder.emit(Instruction::MakeObject {
        dst,
        fields: lowered_fields,
        spreads: vec![],
        ty: slot_ty.clone(),
        stack: false,
    });
    // Transfer each recursive child's ownership into the SumNode (the node owns +1 of each child,
    // released by its KIND_SUMNODE drop walk). A fresh child literal's +1 MOVES into the node
    // (unregister from this scope); a borrowed child sub-expr is retained. Mirrors the
    // `transfer_into_container` rule for array/object element inserts — codegen does not retain at
    // the store, so this is the sole balancing reference for the node's owned child slot.
    for (t, src) in recursive_children {
        builder.transfer_into_container(t, src, true);
    }
    builder.register_owned(dst, slot_ty.clone());
    Some(dst)
}

/// Coerce a value into a (plain, non-cell) local/global SLOT the binding will OWN, transferring
/// ownership of any transient coercion box into the scope's owned set so scope-exit reclaims it.
///
/// This is the BINDING analogue of `coerce_and_own_store` (the CELL/global case, which clones the
/// box and frees the transient shell). Here the binding does NOT clone — the box IS the value the
/// slot holds — so the scope must OWN the box itself.
///
/// The leak this fixes: when `coerce_to_slot_type` boxes a CONCRETE heap value (`is_rc_type`
/// true: Str/Array/FixedArray/Object/Iterator) into a UNION slot, it emits a `Coerce`
/// (`lin_box_object`/`lin_box_array`/`lin_box_str`) producing a fresh 16-byte `TaggedVal*` shell
/// `b` that wraps the raw inner WITHOUT bumping the inner's rc, and registers NOTHING for `b`.
/// `lower_expr` already `register_owned`'d the raw inner (a fresh alloc, or a retained read).
/// Scope-exit then releases the raw inner (rc 1→0, frees it) but orphans `b`'s shell → 16 B leak.
///
/// Fix: the box `b` becomes the owned union representation. `register_owned(b, slot_ty)` so
/// scope-exit releases it via `lin_tagged_release` — which frees BOTH the shell AND drops the
/// inner's rc — and `unregister_owned(raw)` so the inner's single +1 transfers INTO the box
/// (otherwise scope-exit would also release the inner → double-free). Exactly mirrors the
/// `coerce_if_branch` "concrete value boxed to union" case (the box owns its inner via the kept
/// raw temp) and `transfer_into_container`'s fresh-alloc unregister.
///
/// No-op (delegates to `coerce_to_slot_type` only) when: representations match (no box made), the
/// slot is not a union (e.g. sealed-scalar element coercion — the only other caller of
/// `lower_value_into_slot`), or the value is already a union (the box is a clone/forward handled
/// elsewhere) or non-rc (scalar→union boxing carries no inner heap payload to balance — the cached
/// scalar box has nothing to release, and the raw scalar is not registered owned anyway).
fn coerce_to_slot_type_owning_bind(t: Temp, value_ty: &Type, slot_ty: &Type, builder: &mut FuncBuilder) -> Temp {
    let made_fresh_box = is_union_ty(slot_ty)
        && !is_union_ty(value_ty)
        && is_rc_type(value_ty)
        && type_repr_differs(value_ty, slot_ty);
    // A flat scalar array kind/width change (e.g. UInt8[] → Int32[]) MATERIALIZES a fresh,
    // independent +1-owned buffer (codegen's `flat_array_widen`) — the source array keeps its own
    // reference (released by its own scope). Unlike the union-box case the source is NOT consumed,
    // so leave `t` registered; just register the fresh result so the binding's scope releases it.
    let made_fresh_array = flat_scalar_array_repr_differs(value_ty, slot_ty);
    let coerced = coerce_to_slot_type(t, value_ty, slot_ty, builder);
    if made_fresh_box {
        // The box now owns the inner's +1; the scope releases the box (freeing shell + inner).
        builder.unregister_owned(t);
        builder.register_owned(coerced, slot_ty.clone());
    } else if made_fresh_array && coerced != t {
        builder.register_owned(coerced, slot_ty.clone());
    }
    coerced
}

/// Coerce a value temp to a slot's declared type when their runtime representations
/// differ (box concrete → union, or unbox union → concrete). Returns the (possibly new)
/// temp; a no-op when representations match.
fn coerce_to_slot_type(t: Temp, value_ty: &Type, slot_ty: &Type, builder: &mut FuncBuilder) -> Temp {
    if type_repr_differs(value_ty, slot_ty)
        || scalar_numeric_repr_differs(value_ty, slot_ty)
        || flat_scalar_array_repr_differs(value_ty, slot_ty)
    {
        let dst = builder.alloc_temp(slot_ty.clone());
        builder.emit(Instruction::Coerce {
            dst, src: t, from_ty: value_ty.clone(), to_ty: slot_ty.clone(),
        });
        dst
    } else {
        t
    }
}

/// Coerce one branch of an `if` (used as a value) to the merge's result representation, producing
/// a result the merge OWNS independently. Returns `(merge_value, keep_set, owns_plus_one)`:
/// `keep_set` lists temps the branch scope must NOT release; `owns_plus_one` is true when
/// `merge_value` carries an independent +1 reference (so the merge must register+release it).
///
/// Two use-after-free hazards drive this, both for the `if isFailure(r) then r else …`
/// propagation idiom where `r` is an owned local (`val r = deep()`) whose +1 lives in the
/// ENCLOSING (function-body) scope — not the branch scope:
///
/// 1. UNBOX to a CONCRETE merge type. A plain `Coerce` (union → concrete) yields the box's
///    INTERIOR pointer with NO new reference, so the concrete value ALIASES the box's inner
///    payload. The merge releases it once; meanwhile the enclosing scope releases `r`'s box —
///    freeing the very payload the result aliases.
/// 2. A UNION merge value that aliases `r`'s box. The merge phi just forwards `r`'s box; the
///    enclosing scope releases `r` BEFORE the function-return clone (or any later use) runs,
///    so the forwarded box dangles.
///
/// Fix in both cases: take an INDEPENDENT reference. For (1), `CloneBox` then unbox the clone
/// then free the clone's shell — the concrete result owns a +1 inner. For (2), `CloneBox` a
/// branch value that is NOT owned by the branch's own scope (a `val`-local read, a param, a
/// projection) into a fresh +1 box. A value already owned by the CURRENT branch scope (a fresh
/// allocation / call result) just transfers its +1. A concrete value boxed to union transfers
/// via the kept raw temp.
fn coerce_if_branch(
    raw: Temp,
    value_ty: &Type,
    result_type: &Type,
    builder: &mut FuncBuilder,
) -> (Temp, Vec<Temp>, bool) {
    // (1) Unbox a union/Json value to a CONCRETE rc merge representation: take an independent
    // reference via clone-then-unbox so the merge result does not alias a payload freed by the
    // source box's own owner.
    if is_union_ty(value_ty) && !is_union_ty(result_type) && is_rc_type(result_type) {
        let cloned = builder.alloc_temp(value_ty.clone());
        builder.emit(Instruction::CloneBox { dst: cloned, src: raw, ty: value_ty.clone() });
        let unboxed = builder.alloc_temp(result_type.clone());
        builder.emit(Instruction::Coerce {
            dst: unboxed, src: cloned, from_ty: value_ty.clone(), to_ty: result_type.clone(),
        });
        // The clone's inner payload (+1) now lives on as `unboxed`; reclaim the clone's
        // 16-byte box shell (the inner survives). `raw` (the source box) is left to its own
        // owner — do not keep it. The merge owns `unboxed` (+1 concrete rc).
        builder.emit(Instruction::FreeBoxShell { val: cloned });
        return (unboxed, vec![unboxed], true);
    }
    // (2) Union merge value. Ensure it is an independently-owned +1 box.
    if is_union_ty(result_type) {
        if is_union_ty(value_ty) {
            if builder.is_owned_in_current_scope(raw) {
                // Fresh in this branch (a call result / allocation owned by the branch scope):
                // transfer its +1 to the merge. Keep it across the branch pop.
                return (raw, vec![raw], true);
            }
            // Borrowed (a `val`-local read like `r`, a param, a projection): clone into a fresh
            // +1 box so an enclosing release of the source box cannot free what the merge holds.
            let cloned = builder.alloc_temp(value_ty.clone());
            builder.emit(Instruction::CloneBox { dst: cloned, src: raw, ty: value_ty.clone() });
            return (cloned, vec![cloned], true);
        }
        // Concrete value boxed to union: the fresh box owns its inner (the kept raw transfers
        // its +1 into the box). The merge owns the box.
        let boxed = coerce_to_slot_type(raw, value_ty, result_type, builder);
        return (boxed, vec![boxed, raw], true);
    }
    // Concrete merge, concrete branch (or scalar unbox): the existing coercion, no extra
    // ownership. Keep BOTH the value and the raw pre-coercion temp — a box (e.g. lin_box_object)
    // shares the underlying pointer, so releasing the raw would free what the kept box wraps.
    let val = coerce_to_slot_type(raw, value_ty, result_type, builder);
    (val, vec![val, raw], false)
}

fn const_type(c: &Const) -> Type {
    match c {
        Const::Int(_, t) => t.clone(),
        Const::Float(_, t) => t.clone(),
        Const::Bool(_) => Type::Bool,
        Const::Null => Type::Null,
        Const::Str(_) => Type::Str,
    }
}

// -------------------------------------------------------------------------
// Statement lowering
// -------------------------------------------------------------------------

fn lower_stmt(stmt: &TypedStmt, builder: &mut FuncBuilder, ctx: &mut LowerCtx) {
    match stmt {
        TypedStmt::Val { slot, value, ty, .. } => {
            // A top-level function val was pre-assigned a FuncId in `global_fn_slots`
            // during the module pre-scan (so `CallTarget::Direct` references resolve).
            // Reuse that id when lowering the function body, otherwise a fresh id is
            // allocated and the Direct call target points at a non-existent function.
            if let (TypedExpr::Function { name, params, body, ret_type, captures, span: fn_span, .. }, Some(&fid)) =
                (value, ctx.global_fn_slots.get(slot))
            {
                // Register default-fill adapters for this top-level function (no-op if it has
                // no optional parameters). The real symbol is the function's own name.
                if let Some(real_name) = name.as_deref() {
                    register_default_adapters(fid, *slot, real_name, params, ret_type, *fn_span, ctx);
                }
                let t = lower_function_expr_with_id(
                    Some(fid), None, name.as_deref(), params, body, ret_type, captures, builder, ctx,
                );
                builder.slots.insert(*slot, t);
            } else {
                // Store the value in the slot's declared representation: a concrete value
                // bound to a Json/union slot must be boxed so later reads (LocalGet, is/has)
                // see a TaggedVal*. A sealed scalar-record slot bound to an object literal is
                // constructed directly as a packed struct (fast path inside lower_value_into_slot).
                let t = lower_value_into_slot(value, ty, builder, ctx);
                builder.slots.insert(*slot, t);
                // Also publish top-level vals to their module global (for closure reads).
                // A `val` binding is single-store and never reassigned, so the global is
                // immutable: mark it foldable (`immutable: true`).
                if ctx.global_val_slots.contains_key(slot) {
                    builder.emit(Instruction::GlobalValSet { slot: *slot, value: t, ty: ty.clone(), immutable: true });
                }
            }
        }
        TypedStmt::Var { slot, value, ty, .. } => {
            if ctx.slot_is_cell(*slot) {
                // Mutably captured by a closure, or an owning-typed var reassigned inside a branch:
                // store in a heap cell shared by reference.
                // The slot maps to the cell-pointer temp; reads/writes go through it.
                //
                // Cell type: a `var x = null` is typed `Null` by the checker even when later
                // reassigned to other types (the checker doesn't widen it). A `Null` cell
                // would store/read a null pointer and box every read back to null. Promote
                // such cells to `Json` (TypeVar) so the cell holds boxed values across the
                // closure boundary — matching the AST path's pointer-cell model. Boxing of
                // the init and of each reassigned value is handled by coerce_to_slot_type.
                let cell_ty = if matches!(ty, Type::Null) { Type::TypeVar(u32::MAX) } else { ty.clone() };
                let raw = lower_expr(value, builder, ctx);
                // The cell owns an independent reference to its initial value (mirrors the
                // reassignment path in LocalSet) so the cell's release-on-reassign stays
                // balanced. Concrete rc: retain in place; union: clone the box so the cell owns
                // its own TaggedVal* (and free the transient coercion box shell).
                let t = coerce_and_own_store(raw, &value.ty(), &cell_ty, builder);
                let cell = builder.alloc_temp(Type::TypeVar(u32::MAX));
                builder.emit(Instruction::MakeCell { dst: cell, init: t, ty: cell_ty.clone() });
                builder.cell_slots.insert(*slot, cell_ty.clone());
                builder.slots.insert(*slot, cell);
                // Track this cell for the captured-cell escape analysis: it becomes a
                // scope-exit FreeCell candidate unless a capturing closure is later lowered
                // outside safe-combinator-callback context (which marks it escaping). Record the
                // creation block so we only free entry-block cells (dominance — see field doc).
                let create_block = builder.current_block;
                builder.created_cells.push((cell, cell_ty, create_block));
            } else {
                let raw = lower_expr(value, builder, ctx);
                // Transfer ownership of any transient coercion box (concrete heap value → union
                // slot) into the scope's owned set, mirroring the `val`-binding path, so the box
                // shell is reclaimed at scope exit rather than orphaned.
                let t = coerce_to_slot_type_owning_bind(raw, &value.ty(), ty, builder);
                // Plain mutable temp; tracked per var slot, updated on LocalSet.
                builder.slots.insert(*slot, t);
                // A top-level `var` is also published to its module global so closures (which
                // can't see main's SSA temps) can read/write it. Writes inside closures go
                // through GlobalValSet (see LocalSet); reads through GlobalValGet (LocalGet).
                if ctx.global_val_slots.contains_key(slot) {
                    // The global owns an independent reference to its initial value (mirrors
                    // LocalSet) so release-on-reassign stays balanced. Concrete rc: retain in
                    // place; union: clone the box so the global owns its own TaggedVal*. (This
                    // runs once per program, so the transient init box is not freed here — only
                    // per-iteration reassignment boxes, freed at the LocalSet site, matter for
                    // the leak. `t` also stays live in the plain slot, though global_var reads
                    // always go through GlobalValGet.)
                    let gv = own_for_store(t, ty, builder);
                    // Top-level `var`: mutable shared state (reassigned via LocalSet), never
                    // foldable — `immutable: false` keeps the global at external linkage.
                    builder.emit(Instruction::GlobalValSet { slot: *slot, value: gv, ty: ty.clone(), immutable: false });
                }
            }
        }
        TypedStmt::Import { path, bindings, .. } => {
            // Imported modules are compiled through the IR pipeline (compile_import_from_ir),
            // so each exported symbol already exists in the LLVM module
            // under its mangled name `{module_key}_{name}`. Resolve each binding slot to
            // either a `Named` call target (function exports) or a zero-arg val-wrapper
            // (non-function exports), matching the AST path's `compile_stmt` Import logic.
            let module_key = mangle_module_key(path);
            for b in bindings {
                if let Type::Function { params, .. } = &b.ty {
                    let sym = format!("{}_{}", module_key, b.name);
                    ctx.import_fn_slots.insert(b.slot, (sym, params.clone()));
                    // Imported stdlib combinator (map/for/filter/…): a closure passed as its
                    // callback argument is consumed synchronously and never escapes — record the
                    // callback arg index so captured cells stay freeable. Restricted to the
                    // `std/iter` module, which owns the combinator exports (ADR-077), so a
                    // same-named export from elsewhere isn't trusted. A Stream receiver bypasses
                    // this path (the stream redirect in `lower_call` returns before the callback
                    // arg is lowered), so a lazily-retained stream callback never gets the context.
                    if module_key == "std_iter" {
                        if let Some(idx) = safe_combinator_callback_index(&b.name) {
                            ctx.safe_combinator_slots.insert(b.slot, idx);
                        }
                    }
                } else {
                    let wrapper = format!("{}_{}__val", module_key, b.name);
                    ctx.import_val_slots.insert(b.slot, (wrapper, b.ty.clone()));
                }
            }
        }
        TypedStmt::ForeignImport { bindings, .. } => {
            // Foreign (FFI) functions are declared as external LLVM symbols under their
            // own unmangled name; resolve valid function bindings to a `Named` target.
            for b in bindings {
                if let Type::Function { params, .. } = &b.ty {
                    if b.valid {
                        ctx.import_fn_slots.insert(b.slot, (b.name.clone(), params.clone()));
                    }
                }
            }
        }
        TypedStmt::Destructure {
            obj_slot,
            value,
            fields,
            rest,
            obj_ty,
            ..
        } => {
            let dobj_ty = value.ty();
            let obj_temp = lower_expr(value, builder, ctx);
            builder.slots.insert(*obj_slot, obj_temp);
            for (field_name, binding_slot, field_ty) in fields {
                let _key_temp = builder.const_temp(Const::Str(field_name.clone()));
                let dst = builder.alloc_temp(field_ty.clone());
                builder.emit(Instruction::FieldGet {
                    dst,
                    object: obj_temp,
                    field: field_name.clone(),
                    obj_ty: dobj_ty.clone(),
                    result_ty: field_ty.clone(),
                });
                builder.slots.insert(*binding_slot, dst);
            }
            // `...rest` binds a new object with all fields except the destructured ones.
            if let Some(rest_slot) = rest {
                let rest_ty = Type::TypeVar(u32::MAX);
                let dst = builder.alloc_temp(rest_ty.clone());
                builder.emit(Instruction::ObjectRest {
                    dst,
                    src: obj_temp,
                    src_ty: dobj_ty.clone(),
                    exclude: fields.iter().map(|(name, _, _)| name.clone()).collect(),
                });
                builder.register_owned(dst, rest_ty);
                builder.slots.insert(*rest_slot, dst);
            }
            let _ = obj_ty;
        }
        TypedStmt::ArrayDestructure {
            arr_slot,
            value,
            elem_ty,
            elements,
            rest,
            ..
        } => {
            let arr_obj_ty = value.ty();
            let arr_temp = lower_expr(value, builder, ctx);
            builder.slots.insert(*arr_slot, arr_temp);
            for (index, binding_slot, field_ty) in elements {
                let idx_temp = builder.const_temp(Const::Int(*index as i64, Type::Int64));
                let dst = builder.alloc_temp(field_ty.clone());
                builder.emit(Instruction::Index {
                    dst,
                    object: arr_temp,
                    key: idx_temp,
                    obj_ty: arr_obj_ty.clone(),
                    key_ty: Type::Int64,
                    result_ty: field_ty.clone(),
                });
                builder.slots.insert(*binding_slot, dst);
            }
            if let Some((rest_slot, rest_ty)) = rest {
                // rest = arr[elements.len() .. length(arr)] via lin_array_slice_tagged.
                let start = builder.const_temp(Const::Int(elements.len() as i64, Type::Int64));
                let len = builder.alloc_temp(Type::Int64);
                builder.emit(Instruction::CallIntrinsic {
                    dst: len, intrinsic: Intrinsic::Length, args: vec![arr_temp], ret_ty: Type::Int64,
                });
                let dst = builder.alloc_temp(rest_ty.clone());
                builder.emit(Instruction::Call {
                    dst,
                    callee: CallTarget::Named("lin_array_slice_tagged".to_string()),
                    args: vec![arr_temp, start, len],
                    ret_ty: rest_ty.clone(),
                });
                builder.register_owned(dst, rest_ty.clone());
                builder.slots.insert(*rest_slot, dst);
            }
            let _ = elem_ty;
        }
        TypedStmt::Expr(expr) => {
            lower_expr(expr, builder, ctx);
        }
    }
}

// -------------------------------------------------------------------------
// Expression lowering
// -------------------------------------------------------------------------

fn lower_expr(expr: &TypedExpr, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    match expr {
        TypedExpr::IntLit(v, ty, _) => {
            builder.const_temp(Const::Int(*v, ty.clone()))
        }
        TypedExpr::FloatLit(v, ty, _) => {
            builder.const_temp(Const::Float(*v, ty.clone()))
        }
        TypedExpr::StringLit(s, _, _) => {
            // StrLit is Str at runtime (ADR-034): always lower to an owned Str temp.
            let t = builder.const_temp(Const::Str(s.clone()));
            builder.register_owned(t, Type::Str);
            t
        }
        TypedExpr::BoolLit(b, _) => {
            builder.const_temp(Const::Bool(*b))
        }
        TypedExpr::NullLit(_) => {
            builder.const_temp(Const::Null)
        }

        TypedExpr::LocalGet { slot, ty, .. } => {
            // Top-level mutable `var` (module global): ALWAYS load via GlobalValGet, never a
            // cached local temp — a preceding closure call may have mutated the global. (A
            // top-level immutable `val` can use the local-temp fast path below.)
            if ctx.global_var_slots.contains(slot) {
                let gty = ctx.global_val_slots.get(slot).cloned().unwrap_or_else(|| ty.clone());
                let dst = builder.alloc_temp(gty.clone());
                // Top-level `var` read: mutable global, never foldable (`immutable: false`).
                builder.emit(Instruction::GlobalValGet { dst, slot: *slot, ty: gty.clone(), immutable: false });
                // The global holds the var's declared representation; narrow to the requested
                // concrete type if this use wants one (e.g. a Json global read as Int32).
                let narrowed = is_union_ty(&gty) && !is_union_ty(ty);
                if narrowed {
                    // Narrow the loaded box to the requested concrete type. Unboxing (Coerce)
                    // does not add a reference, so the narrowed concrete value aliases the
                    // box's inner payload. Owning read at the CONCRETE representation: retain
                    // the inner in place + register, so it survives a later global reassignment
                    // (release-old) and is freed at scope exit. (`own_for_read` with the
                    // concrete `ty` retains in place — not a box clone.)
                    let d = builder.alloc_temp(ty.clone());
                    builder.emit(Instruction::Coerce { dst: d, src: dst, from_ty: gty.clone(), to_ty: ty.clone() });
                    return own_for_read(d, ty, builder);
                }
                // Not narrowed: the loaded value is the global's box. Owning read clones it so
                // the reader owns its own box (concrete rc globals retain in place).
                return own_for_read(dst, &gty, builder);
            }
            // Heap-cell slot (mutably-captured var): load the current value through the cell.
            if let Some(cell_ty) = builder.cell_slots.get(slot).cloned() {
                if let Some(&cell) = builder.slots.get(slot) {
                    let dst = builder.alloc_temp(cell_ty.clone());
                    builder.emit(Instruction::CellGet { dst, cell, ty: cell_ty.clone() });
                    // Owning read: take an independently-owned copy of the loaded value so it
                    // survives a later reassignment of the cell (release-old on CellSet) and is
                    // released at scope exit. Concrete rc: retain in place. Union: clone the box
                    // (the reader owns its OWN TaggedVal*, so releasing it at scope exit never
                    // frees the cell's box).
                    return own_for_read(dst, &cell_ty, builder);
                }
            }
            if let Some(&t) = builder.slots.get(slot) {
                // If the slot holds a boxed (Json/union) value but this use wants a concrete
                // type — e.g. a Json param narrowed to String inside a match arm — unbox it.
                let stored_ty = builder.temp_types.get(&t).cloned().unwrap_or_else(|| ty.clone());
                let narrowed = is_union_ty(&stored_ty) && !is_union_ty(ty);
                // A union narrowed to a SEALED scalar record is a PROJECTION (`sealed_project_from`):
                // the Coerce ALLOCATES a FRESH +1 owned struct (retaining the source's heap fields),
                // NOT a borrowed alias of the box's inner. So it must be registered owned but NOT
                // additionally retained — the read-retain below is for the borrowed-unbox case (a
                // union narrowed to String/Array aliases the box's inner heap payload, which the
                // retain takes a reference to). Retaining a freshly-projected struct adds a +1 that
                // never balances (only one scope-exit release runs), leaking the struct every read —
                // the `match trip is Trip => trip["dep"]` arm-narrowing leak (ASan-confirmed, both
                // recursive and non-recursive).
                let narrowed_to_sealed = narrowed && is_sealed_scalar_repr(ty);
                let t = if narrowed {
                    let dst = builder.alloc_temp(ty.clone());
                    builder.emit(Instruction::Coerce {
                        dst, src: t, from_ty: stored_ty, to_ty: ty.clone(),
                    });
                    dst
                } else {
                    t
                };
                if narrowed_to_sealed {
                    // Fresh +1 projection: register for scope-exit release, no retain.
                    builder.register_owned(t, ty.clone());
                } else if is_rc_type(ty) {
                    // Pessimistically retain heap values on every read — rc_elide removes redundant pairs.
                    builder.emit(Instruction::Retain { val: t, ty: ty.clone() });
                    builder.register_owned(t, ty.clone());
                }
                t
            } else if let Some((sym, _)) = ctx.import_fn_slots.get(slot).cloned() {
                // An imported top-level function (or FFI symbol) referenced as a VALUE rather
                // than called — e.g. passed as a `Function`-typed argument like
                // `router.serve(3000)` (desugared to `serve(router, 3000)`) or `arr.map(imported)`.
                // Without this branch the slot resolves to none of the call-position handling
                // above and falls through to the placeholder `else`, emitting NO instruction, so
                // codegen's arg collection silently DROPS the value (the "N args for an N+1-param
                // call" codegen error). Materialize it as a capture-less closure VALUE bound to the
                // external symbol — the codegen mirror of the local-named-function case below.
                let closure_ty = ty.clone();
                let dst = builder.alloc_temp(closure_ty.clone());
                builder.emit(Instruction::MakeNamedClosure {
                    dst,
                    sym,
                    ty: closure_ty.clone(),
                });
                builder.register_owned(dst, closure_ty);
                dst
            } else if let Some((wrapper, val_ty)) = ctx.import_val_slots.get(slot).cloned() {
                // Imported non-function val: call its zero-arg wrapper to compute the value.
                let dst = builder.alloc_temp(val_ty.clone());
                builder.emit(Instruction::Call {
                    dst,
                    callee: CallTarget::Named(wrapper),
                    args: vec![],
                    ret_ty: val_ty.clone(),
                });
                if is_rc_type(&val_ty) {
                    builder.register_owned(dst, val_ty);
                }
                dst
            } else if let Some(gty) = ctx.global_val_slots.get(slot).cloned() {
                // A top-level val referenced where it isn't an in-scope temp (e.g. inside a
                // closure) — load it from its module global. Owning read: take an
                // independently-owned copy (concrete rc: retain; union: clone the box) and
                // register for scope-exit release.
                let dst = builder.alloc_temp(gty.clone());
                // Reached only when the slot is in global_val_slots but NOT global_var_slots
                // (the var branch above returns first), i.e. a top-level immutable `val` read
                // from inside a closure. Foldable: `immutable: true`.
                builder.emit(Instruction::GlobalValGet { dst, slot: *slot, ty: gty.clone(), immutable: true });
                own_for_read(dst, &gty, builder)
            } else if let Some(&fid) = ctx.global_fn_slots.get(slot) {
                // A top-level NAMED function referenced as a VALUE (not in call position):
                // e.g. passed as a `Function`-typed argument `combine(t, l, p, leaf)`, or stored
                // in a binding. Top-level fn vals are NOT published as module globals (they live
                // only as `main`'s SSA temps — see lower_module's global_val_slots scan, which
                // excludes Function vals), so inside any OTHER function the slot resolves to none
                // of the branches above. Without this it fell through to the placeholder `else`
                // and emitted NO instruction, so codegen's arg collection (filter_map over
                // temp_map) silently DROPPED the arg — "3 args for a 4-param call" → codegen
                // error for a recursive callee, segfault for a non-recursive one. Materialize the
                // named fn as a closure VALUE exactly as a lambda literal would (MakeClosure with
                // no captures), so codegen wraps it in the uniform boxed-ABI desc-ret stub.
                let closure_ty = ty.clone();
                let dst = builder.alloc_temp(closure_ty.clone());
                builder.emit(Instruction::MakeClosure {
                    dst,
                    func: fid,
                    captures: vec![],
                    capture_kinds: vec![],
                    ret_ty: closure_ty.clone(),
                });
                builder.register_owned(dst, closure_ty);
                dst
            } else {
                // Slot not yet in scope — emit a placeholder null temp.
                // (Can happen for forward-declared functions resolved by codegen.)
                builder.alloc_temp(ty.clone())
            }
        }

        TypedExpr::LocalSet { slot, value, ty, .. } => {
            let val_temp = lower_expr(value, builder, ctx);
            // Heap-cell slot: write through the cell so captured closures see the update.
            if let Some(cell_ty) = builder.cell_slots.get(slot).cloned() {
                if let Some(&cell) = builder.slots.get(slot) {
                    let v = coerce_to_slot_type(val_temp, &value.ty(), &cell_ty, builder);
                    // When the slot is a union and the value was concrete, `coerce_to_slot_type`
                    // allocated a FRESH transient `TaggedVal*` box `v` wrapping the raw value (the
                    // raw value keeps its own +1, released at scope exit). We clone `v` once for the
                    // cell's owned reference and once for the assignment result, then free the
                    // orphaned `v` shell (its inner is owned by the raw value's scope-exit release).
                    // Mirrors the Var-init path's `coerce_and_own_store` and the global path below.
                    let made_fresh_box = is_union_ty(&cell_ty)
                        && !is_union_ty(&value.ty())
                        && type_repr_differs(&value.ty(), &cell_ty);
                    // The cell owns an INDEPENDENT reference to its value: take an owned copy on
                    // store so it survives the producing scope's own release, and codegen
                    // releases the cell's OLD reference on reassignment (fixing the
                    // per-reassignment leak). Concrete rc: retain `v` in place (the stored
                    // pointer is `v` with rc+1). Union: clone the box (`stored` is a fresh
                    // TaggedVal* the cell exclusively owns) so release-old never frees a
                    // borrowed box.
                    let stored = own_for_store(v, &cell_ty, builder);
                    builder.emit(Instruction::CellSet { cell, value: stored, ty: cell_ty.clone() });
                    // The assignment EXPRESSION result must be an INDEPENDENTLY-owned value (not the
                    // transient box `v`): a discarding caller (e.g. the `for` callback-return
                    // release) can then reclaim it without touching the cell's distinct reference.
                    // `own_for_read` clones the box (union) / retains (concrete rc) and registers it
                    // for scope-exit release.
                    if needs_owning(&cell_ty) {
                        let result = own_for_read(v, &cell_ty, builder);
                        // Free the transient coercion box shell AFTER both clones read it (freeing
                        // earlier would be a use-after-free of the shell). A fresh box implies a
                        // union slot, so this only runs on the owning path. `result` is a distinct
                        // box, so freeing `v`'s shell can't touch it.
                        if made_fresh_box {
                            builder.emit(Instruction::FreeBoxShell { val: v });
                        }
                        return result;
                    }
                    // Non-owning cell: `made_fresh_box` is impossible (it requires a union slot),
                    // so there is no transient box to free and `v` is the raw value itself.
                    return v;
                }
            }
            // Module-global slot (a top-level `var`): write through the global so the update
            // is visible to closures and to later reads (which load via GlobalValGet). Coerce
            // to the global's declared representation first.
            if let Some(gty) = ctx.global_val_slots.get(slot).cloned() {
                let v = coerce_to_slot_type(val_temp, &value.ty(), &gty, builder);
                // When the slot is a union and the value was concrete, `coerce_to_slot_type`
                // allocated a FRESH transient `TaggedVal*` box `v` wrapping the raw value (the
                // raw value keeps its own +1, released at scope exit). Below we clone `v` once for
                // the global's owned reference and once for the assignment result; the original
                // `v` shell is then an orphan and must have its 16-byte shell freed (its inner is
                // owned by the raw value's scope-exit release, NOT by `v`). Mirrors the Var-init
                // path's `coerce_and_own_store`. When no fresh box was made (already-union value,
                // or non-union slot), nothing extra is freed.
                let made_fresh_box =
                    is_union_ty(&gty) && !is_union_ty(&value.ty()) && type_repr_differs(&value.ty(), &gty);
                // The global owns an INDEPENDENT reference to its value (symmetric owning model,
                // mirroring the captured-cell path above). For unions this CLONES the box
                // (`own_for_store` → `CloneBox`/`lin_tagged_clone`) so the global gets its OWN
                // `TaggedVal*` shell — NOT an alias of the producer's/return's shell. (The old
                // code used `Retain`, which shared the shell: a discarding caller releasing the
                // assignment result then freed the global's shell → use-after-free.)
                let stored = own_for_store(v, &gty, builder);
                // A LocalSet to a module-global slot is a `var` REASSIGNMENT (an immutable
                // `val` is single-store and never reaches LocalSet). Mutable → `immutable: false`.
                builder.emit(Instruction::GlobalValSet { slot: *slot, value: stored, ty: gty.clone(), immutable: false });
                builder.slots.insert(*slot, v);
                // The assignment EXPRESSION result must itself be an independently-owned value so
                // a discarding caller (e.g. the `for` callback-return release below) can release
                // it without touching the global's distinct reference. `own_for_read` clones the
                // box (union) / retains (concrete rc) and registers it for scope-exit release, so
                // when the result is NOT discarded by a loop it is still reclaimed at teardown.
                if needs_owning(&gty) {
                    let result = own_for_read(v, &gty, builder);
                    // Free the transient coercion box shell AFTER cloning it for both the store
                    // (`own_for_store`) and the result (`own_for_read`) — freeing it earlier would
                    // be a use-after-free of the shell those clones read. A fresh box implies a
                    // union slot, so this only runs on the owning path here. (`result` is a
                    // distinct, independently-owned box, so freeing `v`'s shell can't touch it.)
                    if made_fresh_box {
                        builder.emit(Instruction::FreeBoxShell { val: v });
                    }
                    return result;
                }
                // Non-owning slot: `made_fresh_box` is impossible (it requires a union slot), so
                // there is no transient box to free and `v` is the raw value itself.
                return v;
            }
            // Plain SSA-temp slot (a `var` neither captured into a heap cell nor a module global).
            // Coerce the value to the slot's DECLARED representation before rebinding: a `var sts:
            // Json` holds a boxed TaggedVal*, so a concrete reassignment (`sts = groups[g]`, an
            // unboxed array) must be boxed to match — otherwise a later read (and any join phi over
            // the slot) sees a raw pointer where a box is expected (type/representation mismatch,
            // wrong-tag reads). The slot's previous value was registered owned at its definition /
            // a prior reassignment; this new value becomes the slot's value. The OLD owned
            // reference is reconciled at the next control-flow join (an enclosing `if`/match merge
            // drops the superseded registrations — see `merge_var_slots`); within straight-line
            // code the slot simply advances to the new temp and both end up released at scope exit.
            if needs_owning(ty) {
                // The slot must OWN exactly ONE independent reference to its new value: a
                // reassignment from a BORROWED projection (`sts = groups[g]`, an interior array
                // pointer the container still owns) would otherwise leave the slot aliasing the
                // container's element, so releasing the slot at scope exit AND releasing the
                // container double-frees that element. `coerce_and_own_store` boxes/coerces to the
                // slot representation and takes a single owned reference (clone the box for unions
                // / retain in place for concrete rc), reclaiming any transient coercion shell —
                // mirroring the captured-cell var path. We register that ONE reference and return
                // the SAME temp as the assignment-expression result: a `var x = e` statement is
                // value-discarded (it is not consumed/released by an enclosing block result or a
                // `for` callback-return release, which only act on the BLOCK's final expression),
                // so the slot and the result safely share the single +1. The slot's single live
                // owner is reconciled across a branch at the join (`merge_var_slots`).
                let stored = coerce_and_own_store(val_temp, &value.ty(), ty, builder);
                builder.register_owned(stored, ty.clone());
                builder.slots.insert(*slot, stored);
                return stored;
            }
            // Non-owning (scalar) slot: store the coerced value directly.
            let v = coerce_to_slot_type(val_temp, &value.ty(), ty, builder);
            builder.slots.insert(*slot, v);
            v
        }

        TypedExpr::BinaryOp { left, op, right, result_type, .. } => {
            // `&&` / `||` are SHORT-CIRCUITING (spec §8): the RHS must only be evaluated when
            // the LHS does not already decide the result. Emit branch + merge + Phi control flow
            // (mirroring lower_if) rather than a bitwise and/or over two eagerly-lowered operands.
            if matches!(op, BinOp::And | BinOp::Or) {
                return lower_short_circuit(left, *op, right, result_type, builder, ctx);
            }
            // The operand type drives equality/comparison dispatch (e.g. object/array
            // deep equality); it differs from result_type for comparisons (which yield Bool).
            let left_ty = left.ty();
            let right_ty = right.ty();
            let mut lhs = lower_expr(left, builder, ctx);
            let mut rhs = lower_expr(right, builder, ctx);
            let mut operand_ty = left_ty.clone();
            // TaggedVal* operand shells freshly boxed below (see the arith dyn-coerce branch).
            let mut fresh_operand_boxes: Vec<Temp> = Vec::new();

            // ARITHMETIC ops need concrete (unboxed) operands. If a side's STATIC type is a
            // union (Json/TypeVar) while the other is concrete — e.g. a loop/closure param
            // typed `TypeVar` used as `Int32` in `total + i` — unbox it to the concrete operand
            // type first, or codegen runs an integer op on a raw pointer (crash). We do NOT do
            // this for equality/comparison ops: those have a dedicated union path in codegen
            // (lin_tagged_eq / lin_tagged_cmp) that tolerates boxed/null operands, and unboxing
            // a possibly-null Json (e.g. `opts["k"] == true` where the key is absent) would be
            // unsound.
            // BITWISE ops (`& | ^ << >>`) need concrete integer operands too — same as
            // arithmetic. A boxed Json/union operand (e.g. `acc ^ bytes[i]` where `bytes[i]`
            // projects an Int out of a Json array) must be unboxed first, or codegen runs the
            // integer op on a raw `TaggedVal*` pointer (a codegen-time type-mismatch crash).
            if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
                          | BinOp::BAnd | BinOp::BOr | BinOp::BXor | BinOp::Shl | BinOp::Shr) {
                // A DYNAMIC operand whose static type is still a bare `TypeVar` at lowering time —
                // the `Json` wildcard (`TypeVar(u32::MAX)`) OR an unresolved/index-derived inference
                // var (e.g. `obj["k"]`, which may be Int, Float, or a missing-key Null) — must STAY
                // BOXED. Unboxing it to a concrete int would dereference a possibly-null payload and
                // crash (RAPTOR #5). A GENUINE resolved generic (a reduce accumulator, `total + i`
                // where `total` resolved to Int32, …) is a CONCRETE type by lowering time (post
                // monomorphization), NOT a `TypeVar`, so it is coerced/unboxed natively as before.
                // Box the concrete side to `Json` too so BOTH operands arrive as `TaggedVal*`
                // pointers; codegen's tagged-arith gate then routes the op through the null-safe
                // `lin_tagged_arith` runtime path (preserving the runtime numeric family and
                // faulting cleanly on a non-numeric operand). Boxing the concrete side here — rather
                // than only marking `operand_ty` — is what makes codegen's `lv.is_pointer_value()`
                // gate fire for `10 + obj["k"]`, where the literal `10` would otherwise reach the op
                // as a raw scalar.
                let left_dyn = matches!(left_ty, Type::TypeVar(_));
                let right_dyn = matches!(right_ty, Type::TypeVar(_));
                if left_dyn || right_dyn {
                    let json = Type::TypeVar(u32::MAX);
                    // Box whichever side is concrete (NOT a TypeVar) into a Json `TaggedVal*`; a
                    // side that is already a (possibly-boxed) TypeVar passes through unchanged.
                    // The fresh box is a TaggedVal* shell the tagged-arith op only READS and never
                    // takes ownership of — track it and reclaim the SHELL after the op (below), or
                    // it leaks per evaluation (the dominant RAPTOR query-phase arith leak, e.g.
                    // `acc + 100000` boxing `100000` every loop iteration). The inner payload is a
                    // scalar (no heap) or a String separately owned by its own scope.
                    if !left_dyn {
                        let boxed = coerce_to_slot_type(lhs, &left_ty, &json, builder);
                        if boxed != lhs { fresh_operand_boxes.push(boxed); }
                        lhs = boxed;
                    }
                    if !right_dyn {
                        let boxed = coerce_to_slot_type(rhs, &right_ty, &json, builder);
                        if boxed != rhs { fresh_operand_boxes.push(boxed); }
                        rhs = boxed;
                    }
                    operand_ty = json;
                } else {
                    operand_ty = if !is_union_ty(&left_ty) { left_ty.clone() }
                                 else if !is_union_ty(&right_ty) { right_ty.clone() }
                                 else { left_ty.clone() };
                    if is_union_ty(&left_ty) && !is_union_ty(&operand_ty) {
                        lhs = coerce_to_slot_type(lhs, &left_ty, &operand_ty, builder);
                    }
                    if is_union_ty(&right_ty) && !is_union_ty(&operand_ty) {
                        rhs = coerce_to_slot_type(rhs, &right_ty, &operand_ty, builder);
                    }
                }
            }
            let dst = builder.alloc_temp(result_type.clone());
            builder.emit(Instruction::Binary {
                dst,
                op: *op,
                lhs,
                rhs,
                operand_ty,
                ty: result_type.clone(),
            });
            // Reclaim any operand box shell freshly created to dispatch a tagged-arith op (the op
            // only READ its operands). Shell-only (`FreeBoxShell`) — the inner is a scalar or a
            // separately owned value. MUST come after the Binary so the operand is still live.
            for shell in fresh_operand_boxes {
                builder.emit(Instruction::FreeBoxShell { val: shell });
            }
            // A UNION-typed Binary result is a FRESHLY boxed `TaggedVal*` (+1): the dynamic-arith
            // path (`lin_tagged_arith`) and the bitwise-on-union path (`box_value` of the concrete
            // result) both ALLOCATE a new box; the eq/cmp paths return a concrete `Bool` and never
            // reach here. Register it owned so scope exit releases it (or the move/escape machinery
            // transfers it when it's stored/returned). Without this, a dynamic `acc = acc + x` whose
            // result stays `Json` orphaned the arith result box every iteration — its consumers
            // (cell store, return) each `CloneBox` a fresh +1 and never consumed the original (the
            // residual after the leak-#4b operand-box fix). Concrete-result arithmetic returns an
            // unboxed scalar (not rc) and is unaffected.
            if is_union_ty(result_type) {
                builder.register_owned(dst, result_type.clone());
            }
            dst
        }

        TypedExpr::UnaryOp { op, operand, result_type, .. } => {
            // Surface unary ops `~` (bitwise not) and `!` (logical not) both map to IR
            // `Not` (codegen emits `build_not`): for an i1, bitwise-not == logical-not.
            let ir_op = match op {
                lin_parse::ast::UnaryOp::BNot => crate::ir::UnaryOp::Not,
                lin_parse::ast::UnaryOp::Not => crate::ir::UnaryOp::Not,
            };
            // For logical `!` whose operand is not statically Bool (e.g. a boxed
            // TypeVar), coerce/unbox to a raw i1 first so the Unary sees a real bool.
            let src = if matches!(op, lin_parse::ast::UnaryOp::Not)
                && !matches!(operand.ty(), Type::Bool)
            {
                lower_cond_as_bool(operand, builder, ctx)
            } else {
                lower_expr(operand, builder, ctx)
            };
            let dst = builder.alloc_temp(result_type.clone());
            builder.emit(Instruction::Unary {
                dst,
                op: ir_op,
                operand: src,
                ty: result_type.clone(),
            });
            dst
        }

        TypedExpr::Coerce { expr, from, to, .. } => {
            // CONSTRUCTION fast path (sealed-records Stage 1): a Coerce of an object LITERAL into a
            // sealed scalar record `{ … }: T` constructs the packed struct DIRECTLY (each scalar
            // field stored by offset), instead of building a boxed LinObject and then projecting
            // it (which would pay an object alloc + N lin_object_set + N lin_object_get). Only when
            // the literal has no spreads and provides every target field — otherwise fall through
            // to the general project-from-source coercion. The lowered MakeObject is tagged with
            // the SEALED target type so codegen lays it out as a struct.
            if let Some(t) = try_lower_sealed_literal(expr, to, builder, ctx) {
                return t;
            }
            let src = lower_expr(expr, builder, ctx);
            let dst = builder.alloc_temp(to.clone());
            builder.emit(Instruction::Coerce {
                dst,
                src,
                from_ty: from.clone(),
                to_ty: to.clone(),
            });
            // A projection / materialization Coerce that PRODUCES a sealed scalar record allocates
            // a FRESH owned struct (+1) — register it so scope-exit releases it (via the sealed
            // release path). The source keeps its own ownership. Materialization to a boxed object
            // is likewise a fresh owned value, already handled by the existing owning model for the
            // result temp's (object/union) type. Only the sealed-target case needs registering here
            // because `Coerce` does not otherwise register its result.
            if is_sealed_scalar_repr(to) {
                builder.register_owned(dst, to.clone());
            } else if is_sealed_scalar_array(from) && needs_owning(to) {
                // A sealed-record ARRAY coerced to Json/Object[] (Stage 3 boundary) MATERIALIZES a
                // FRESH +1 tagged `LinArray*` (`sealed_array_to_tagged`). Register the result so the
                // owning model releases the materialized temp at scope exit (else it leaks — e.g.
                // `length(sealedArr)` whose `Json` param coerces the array each call). The source
                // sealed array keeps its own ownership.
                builder.register_owned(dst, to.clone());
            } else if is_sealed_scalar_array(to) && needs_owning(to) {
                // A Json/Object[] PROJECTED into a sealed-record array (the reverse boundary) is a
                // fresh +1 sealed array — register it for scope-exit release.
                builder.register_owned(dst, to.clone());
            } else if nested_sealed_repr_change(from, to) && needs_owning(to) {
                // A NESTED sealed structure projected from the boxed `Json` view (Problem A / Stage
                // 3b): `partition`/`chunk` (`T[][]`) routed through the type-erased boxed fallback
                // returns a boxed result whose Coerce → `array_coerce_elementwise` rebuilds a FRESH
                // +1 outer tagged array (and fresh inner sealed arrays). Register it so the owning
                // model releases the whole structure at scope exit (else it leaks, ASan-confirmed).
                // Gated (matching codegen) on the SOURCE inner being a boxed view — a same-repr array
                // (e.g. a `Neighbor[]` literal) is NOT rebuilt and must not be double-registered.
                builder.register_owned(dst, to.clone());
            }
            dst
        }

        TypedExpr::Call { func, args, result_type, is_tail, partial, .. } => {
            lower_call(func, args, result_type, *is_tail, *partial, builder, ctx)
        }

        TypedExpr::If { cond, then_br, else_br, result_type, .. } => {
            lower_if(cond, then_br, else_br, result_type, builder, ctx)
        }

        TypedExpr::FromJson { target, value, result_type, named_defs, .. } => {
            lower_from_json(target, value, result_type, named_defs, builder, ctx)
        }

        TypedExpr::Match { scrutinee, arms, result_type, .. } => {
            lower_match(scrutinee, arms, result_type, builder, ctx)
        }

        TypedExpr::Block { stmts, expr, .. } => {
            let outer_slots = builder.slots.clone();
            builder.push_scope();
            for stmt in stmts {
                lower_stmt(stmt, builder, ctx);
            }
            let result = lower_expr(expr, builder, ctx);
            // An OUTER `var` reassigned inside this block (a `LocalSet`) now binds the slot to a
            // freshly-owned temp registered in THIS block scope. That temp must SURVIVE the block
            // pop — the slot (an enclosing-scope var) still references it after the block, so
            // releasing it here would leave the slot dangling (use-after-free on the next read /
            // at the enclosing scope-exit release). Keep each such slot's current temp across the
            // pop so its +1 transfers up to the enclosing scope, which owns it from then on
            // (an enclosing `if`/match merge reconciles it at the join via `merge_var_slots`).
            let kept_slot_temps: Vec<Temp> = outer_slots
                .keys()
                .filter(|k| {
                    !stmts.iter().any(|s| stmt_defines_slot(s, **k))
                        && stmts.iter().any(|s| stmt_reassigns_slot(s, **k))
                })
                .filter_map(|k| builder.slots.get(k).copied())
                .collect();
            let mut keep = kept_slot_temps;
            keep.push(result);
            builder.pop_scope_releasing_keep_transfer(&keep);
            // Restore outer scope (block-local bindings don't leak), but PRESERVE outer slots
            // the block mutated: a `var` REASSIGNED inside the block (a `LocalSet`, e.g. `if c
            // then sts = e`) must keep its new value after the block — restoring the pre-block
            // temp would drop the write. A slot the block locally DEFINES (var/val/destructure)
            // gets a fresh distinct slot number not present in `outer_slots`, so the only outer
            // slots reached here are genuine outer bindings; we restore those untouched and keep
            // any that were reassigned. (`stmt_reassigns_slot` walks nested control flow so a
            // reassignment buried in an inner `if`/`match` is detected.)
            for (k, v) in &outer_slots {
                if !stmts.iter().any(|s| stmt_defines_slot(s, *k))
                    && !stmts.iter().any(|s| stmt_reassigns_slot(s, *k))
                {
                    builder.slots.insert(*k, *v);
                }
            }
            result
        }

        TypedExpr::Function { name, params, body, ret_type, captures, .. } => {
            lower_function_expr(name.as_deref(), params, body, ret_type, captures, builder, ctx)
        }

        TypedExpr::MakeObject { fields, spreads, ty, .. } => {
            // This is the GENERAL (boxed) MakeObject path — a sealed scalar-record TARGET is
            // constructed directly as a packed struct elsewhere (`try_lower_sealed_literal`), so
            // here `ty` is always a boxed object/Json. A field VALUE that is itself a sealed scalar
            // record is a packed struct, NOT a LinObject; storing it raw under TAG_OBJECT makes the
            // object's serialize/release walk it as object entries → heap corruption. MATERIALIZE
            // each sealed field value to a boxed LinObject (sealed→Json Coerce) first.
            let lowered_fields: Vec<(String, Temp)> = fields
                .iter()
                .map(|(k, v)| {
                    let t = lower_expr(v, builder, ctx);
                    let vty = v.ty();
                    if is_sealed_scalar_repr(&vty) {
                        let to = Type::object(match &vty {
                            Type::Object { fields, .. } => fields.clone(),
                            _ => unreachable!(),
                        });
                        let dst = builder.alloc_temp(to.clone());
                        builder.emit(Instruction::Coerce {
                            dst, src: t, from_ty: vty.clone(), to_ty: to.clone(),
                        });
                        builder.register_owned(dst, to);
                        (k.clone(), dst)
                    } else {
                        (k.clone(), t)
                    }
                })
                .collect();
            // A spread source that is a SEALED scalar record is a packed struct, NOT a LinObject;
            // the MakeObject spread codegen (`lin_object_extend`/spread) walks it as a LinObject
            // → null-ptr/heap corruption. MATERIALIZE it to a boxed LinObject first (sealed→Json
            // Coerce), so the spread sees the universal object representation. Stage 1 keeps
            // spread on the "convert-to-boxed-view" path (design §3.5/§5).
            let lowered_spreads: Vec<Temp> = spreads
                .iter()
                .map(|s| {
                    let st = lower_expr(s, builder, ctx);
                    let sty = s.ty();
                    if is_sealed_scalar_repr(&sty) {
                        let to = Type::object(match &sty {
                            Type::Object { fields, .. } => fields.clone(),
                            _ => unreachable!(),
                        });
                        let dst = builder.alloc_temp(to.clone());
                        builder.emit(Instruction::Coerce {
                            dst,
                            src: st,
                            from_ty: sty.clone(),
                            to_ty: to.clone(),
                        });
                        builder.register_owned(dst, to);
                        dst
                    } else {
                        st
                    }
                })
                .collect();
            let dst = builder.alloc_temp(ty.clone());
            builder.emit(Instruction::MakeObject {
                dst,
                fields: lowered_fields,
                spreads: lowered_spreads,
                ty: ty.clone(),
                // Default heap; the escape-analysis pass (escape.rs) flips this to `true` only for
                // an all-scalar sealed record it PROVES non-escaping.
                stack: false,
            });
            builder.register_owned(dst, ty.clone());
            dst
        }

        TypedExpr::MakeArray { elements, ty, .. } => {
            let elem_ty = match ty {
                // Arrays of ALL-SCALAR sealed records (Stage 3): contiguous UNBOXED elements. Keep
                // the SEALED element type so each element is lowered to its packed-struct
                // representation; codegen's MakeArray copies each element's field payload into the
                // contiguous sealed-array buffer (no per-element box). Heap-field element records
                // are NOT sealed-scalar-arrays (gated) and fall to the boxed branch below.
                Type::Array(inner) if is_sealed_scalar_array(ty) => *inner.clone(),
                // Arrays of HEAP-FIELD sealed records stay BOXED (Stage 3b deferred): store each
                // element as a boxed `LinObject` (the universal Json element representation). Lower
                // the element type to the UNSEALED object form so `coerce_to_slot_type` inserts a
                // sealed→Json MATERIALIZATION per element (the sealed struct is NOT a LinObject —
                // pushing it raw under TAG_OBJECT makes the array's release/serialize walk it as
                // object entries → heap corruption).
                Type::Array(inner) if is_sealed_scalar_repr(inner) => match inner.as_ref() {
                    Type::Object { fields, .. } => Type::object(fields.clone()),
                    _ => unreachable!(),
                },
                // UNBOXED SUM TYPE (unboxed-sumtype Stage 1): a `Shape[]` is a BOXED `Object[]` (the
                // union element representation). Lower the element slot type to the dynamic Json
                // wildcard so each element (a SumNode) is MATERIALIZED to a boxed `LinObject` by
                // `coerce_to_slot_type` (the codegen Coerce reads the SumNode repr and materializes).
                // The read-back (`compile_ir_index`, sum result) projects each boxed element back into
                // a fresh SumNode, keeping the repr consistent end-to-end.
                Type::Array(inner) if crate::repr::sum_type_eligible(inner) => Type::TypeVar(u32::MAX),
                Type::Array(inner) => *inner.clone(),
                // A fixed-length array (`[T1, T2, ...]`, §5.3) has heterogeneous positional
                // types, so it is stored as a TAGGED (Json) array — each element boxes to a
                // TaggedVal* via coerce_to_slot_type below, and Index reads unbox per the
                // positional result type. Without this the slot type defaulted to Null and
                // every element was coerced away to null.
                Type::FixedArray(_) => Type::TypeVar(u32::MAX),
                _ => Type::Null,
            };
            // Sealed-scalar-array (Stage 3): each element is lowered into its packed sealed-struct
            // representation and remains OWNED. Codegen's MakeArray COPIES each element's scalar
            // field payload into the contiguous buffer (no retain — scalar fields carry no RC), and
            // the source struct is released at scope exit. So we do NOT transfer ownership into the
            // container (the array has its own copy of the bytes, not the struct pointer).
            let sealed_arr = is_sealed_scalar_array(ty);
            // Coerce each element to the array's element representation. For a Json/union
            // element type (heterogeneous array) this boxes each concrete element to a
            // TaggedVal*, so codegen can push them uniformly.
            let lowered: Vec<Temp> = elements
                .iter()
                .map(|e| {
                    if sealed_arr {
                        // Direct sealed-struct construction when the element is a literal; else
                        // lower + project into the sealed element representation.
                        let t = lower_value_into_slot(e, &elem_ty, builder, ctx);
                        return t;
                    }
                    let t = lower_expr(e, builder, ctx);
                    // The array owns a reference to each heap element (lin_array_release
                    // recursively releases them when the array is freed) — apply the standard
                    // container-insert ownership rule on the RAW value before boxing/coercing.
                    // `lin_array_push_tagged` raw-copies the element's TaggedVal struct without
                    // retaining its inner (a MOVE), so a union element is CONSUMED here too —
                    // pass `op_consumes_union = true` so a fresh union element is unregistered.
                    builder.transfer_into_container(t, e, true);
                    coerce_to_slot_type(t, &e.ty(), &elem_ty, builder)
                })
                .collect();
            let dst = builder.alloc_temp(ty.clone());
            builder.emit(Instruction::MakeArray {
                dst,
                elements: lowered,
                elem_ty,
            });
            builder.register_owned(dst, ty.clone());
            dst
        }

        TypedExpr::Index { object, key, result_type, .. } => {
            let obj_ty = object.ty();
            let key_ty = key.ty();
            // FUSED `arr[i]["field"]` over a sealed-record array (Stage 3): same constant-offset
            // scalar load as `arr[i].field`. `object` is an Index of the array; `key` is a literal.
            if let TypedExpr::StringLit(name, _, _) = key.as_ref() {
                if let Some(t) = try_lower_sealed_array_field(object, name, result_type, builder, ctx) {
                    return t;
                }
            }
            // Sealed scalar record + a compile-time-known string key `x["f"]` → constant-offset
            // FieldGet (same as `x.f`). Routes to the unboxed load path rather than the dynamic
            // `lin_object_get` Index path.
            //
            // A sealed record has EXACTLY its declared fields (extras are stripped on assignment),
            // so a key that is NOT one of those fields is STATICALLY ABSENT. The checker only WARNS
            // about it (typing the access as `Null`), so we must not assume presence: emitting an
            // unboxed `FieldGet` for an absent field would assert in `sealed_field_layout` (codegen
            // panic). Instead follow the safe-access rule (§6.1: missing object key → Null): produce
            // a `Null` constant, lowering `object` first so any side effects still run. This mirrors
            // the boxed-object missing-key → Null path that `lin_object_get` already takes.
            if is_sealed_scalar_repr(&obj_ty) {
                if let TypedExpr::StringLit(name, _, _) = key.as_ref() {
                    let present = matches!(&obj_ty,
                        Type::Object { fields, .. } if fields.contains_key(name));
                    if !present {
                        let _ = lower_expr(object, builder, ctx);
                        return builder.const_temp(Const::Null);
                    }
                    let obj_temp = lower_expr(object, builder, ctx);
                    let dst = builder.alloc_temp(result_type.clone());
                    builder.emit(Instruction::FieldGet {
                        dst,
                        object: obj_temp,
                        field: name.clone(),
                        obj_ty,
                        result_ty: result_type.clone(),
                    });
                    if is_rc_type(result_type) {
                        builder.emit(Instruction::Retain { val: dst, ty: result_type.clone() });
                        builder.register_owned(dst, result_type.clone());
                    }
                    return dst;
                }
            }
            // UNBOXED SUM TYPE (unboxed-sumtype Stage 2): a recursive child read `node["left"]` over a
            // sum-typed value (`obj_ty` is sum-eligible) with a compile-time string key that names a
            // RECURSIVE child field (`*SumNode` slot) in some variant → a direct const-offset pointer
            // load yielding a BORROWED interior `*SumNode` (the parent still owns it), repr
            // Packed(SumNode). This is the chained-`evalNode(node["left"])` fast path: no materialize,
            // no `lin_object_get`. The result_type is the child sum type (so the FieldGet result seeds
            // Packed(SumNode) and the recursion re-enters the tag switch). Borrow/retain-if-escapes is
            // applied exactly like a nested-sealed-field load: retain only when the result is an
            // RC type that escapes (the recursive-call arg path retains via the call-arg machinery; a
            // bare return retains here).
            if let TypedExpr::StringLit(name, _, _) = key.as_ref() {
                // The object is physically a `SumNode` (a narrowed sum scrutinee whose variant carries
                // a recursive child, so it canNOT be projected to a sealed struct — its field reads
                // MUST go directly to the SumNode, never through a materialize-to-boxed). Read the RAW
                // SumNode temp (bypassing the union→variant-object narrowing Coerce that
                // `lower_expr(LocalGet)` emits, which would materialize the node and release its
                // borrowed children). ALL fields read directly: a SCALAR field is a const-offset load
                // (its result is a flat scalar); a RECURSIVE CHILD field is a const-offset `*SumNode`
                // pointer load (borrowed, repr Packed(SumNode)).
                if let Some(obj_temp) = lower_sum_scrutinee_raw(object, builder, ctx) {
                    // Result type: the child sum type for a recursive child (so it seeds
                    // Packed(SumNode) and the recursion re-enters the tag switch), else the field's
                    // declared `result_type`.
                    let child_sum_ty = sum_recursive_child_field_ty(&obj_ty, name);
                    let res_ty = child_sum_ty.clone().unwrap_or_else(|| result_type.clone());
                    let dst = builder.alloc_temp(res_ty.clone());
                    builder.emit(Instruction::FieldGet {
                        dst,
                        object: obj_temp,
                        field: name.clone(),
                        obj_ty,
                        result_ty: res_ty.clone(),
                    });
                    // A recursive-child result is a BORROWED interior `*SumNode` (the parent owns it,
                    // releases it via its KIND_SUMNODE drop walk). A consumer that BORROWS it (the
                    // recursive `evalNode(node["left"])` callee retains+releases its own scrutinee,
                    // net 0) needs no owning reference here — return the borrow directly so it is never
                    // double-released against the parent's owned slot (escape is handled by the
                    // surrounding owning machinery). A SCALAR result is a value (no RC). So: no
                    // retain/own here in either case. (Mirrors the borrowed sealed-array element read.)
                    return dst;
                }
            }
            // Borrow the container if it is a bare `LocalGet` of a concrete-RC array/object: the
            // element read does not need an owning reference, so skip the retain/release pair the
            // owning load would emit (the dominant cost of tight index loops over a var array).
            // Falls back to the owning `lower_expr` for any container that doesn't qualify.
            //
            // Lower the KEY FIRST, then the borrowed base: a borrowed base is a bare load with no
            // owning reference, so the base load must be the LAST thing before the `Index` — were
            // the key evaluation to reassign the container global (`arr[mutate()]`), an
            // already-loaded borrowed base would dangle. Evaluating the key first makes the borrow
            // load strictly dominate the read. (The owning fallback keeps the original
            // base-then-key order; its retain makes ordering immaterial.)
            let (obj_temp, key_temp) = match lower_container_base_borrowed_check(object, ctx) {
                true => {
                    let key_temp = lower_expr(key, builder, ctx);
                    let obj_temp = lower_container_base_borrowed(object, builder, ctx)
                        .unwrap_or_else(|| lower_expr(object, builder, ctx));
                    (obj_temp, key_temp)
                }
                false => {
                    let obj_temp = lower_expr(object, builder, ctx);
                    let key_temp = lower_expr(key, builder, ctx);
                    (obj_temp, key_temp)
                }
            };
            let dst = builder.alloc_temp(result_type.clone());
            let obj_ty_is_sealed = is_sealed_scalar_repr(&obj_ty);
            // Whether codegen's array path (`lin_array_get_tagged`) produces a FRESH +1 box for
            // this index — if so, the union relocation below must NOT clone it again (that leaks
            // the original box, once per evaluation). Computed before `obj_ty`/`key_ty` are moved
            // into the `Index` instruction.
            let result_is_fresh_owned = index_result_is_fresh_owned_box(&obj_ty, &key_ty);
            builder.emit(Instruction::Index {
                dst,
                object: obj_temp,
                key: key_temp,
                obj_ty,
                key_ty,
                result_ty: result_type.clone(),
            });
            // A sealed record indexed by a NON-LITERAL key: codegen materializes the record to a
            // boxed object, looks the key up, clones the (borrowed) result into a FRESH owned box and
            // frees the temporary object — so `dst` is already an OWNED, container-independent value.
            // Register it owned directly (the usual borrowed-interior CloneBox relocation does not
            // apply: there is no live container to dangle off of).
            if obj_ty_is_sealed && is_union_ty(result_type) {
                builder.register_owned(dst, result_type.clone());
                return dst;
            }
            // A projection has VALUE (snapshot) semantics (ADR: projection is a value, not a
            // live view). It must materialize an OWNED, container-independent value so the
            // binding survives the container being mutated/grown/freed.
            //
            // - Concrete heap result (`is_rc_type`, e.g. `Object[]`): the slot holds a stable
            //   array/object/string POINTER. Dup it (retain + register owned); the binding now
            //   holds the stable header, not the (movable) slot address.
            // - Union/Json result: `lin_object_get` / `lin_array_get` return an INTERIOR pointer
            //   into the container's entries/data buffer — that buffer MOVES when the container
            //   grows (object inline→heap migration, array realloc), so holding the interior
            //   pointer is a use-after-free. Relocate the value off the slot immediately:
            //   `CloneBox` (→ `lin_tagged_clone`) reads the slot's tag+payload into a FRESH owned
            //   box and retains the inner heap payload, so the binding holds an independent,
            //   stable box. Register it owned so the matching scope-exit / reassignment release
            //   is balanced (cached scalar boxes are returned as-is by lin_tagged_clone and
            //   no-op on release, so no needless alloc on the scalar fast path).
            if is_union_ty(result_type) {
                // Array path: `dst` is ALREADY a fresh, fully-owned +1 box from
                // `lin_array_get_tagged` (it allocated a standalone TaggedVal — for a flat array
                // it boxes the scalar, for a tagged array it copies the element box). It is NOT a
                // borrowed interior pointer, so cloning it again would leak the original box every
                // evaluation. Register it owned directly (mirrors the sealed-by-dynamic-key fresh
                // box above). The object/map path falls through to the borrowed-interior CloneBox.
                if result_is_fresh_owned {
                    builder.register_owned(dst, result_type.clone());
                    return dst;
                }
                let owned = builder.alloc_temp(result_type.clone());
                builder.emit(Instruction::CloneBox { dst: owned, src: dst, ty: result_type.clone() });
                builder.register_owned(owned, result_type.clone());
                return owned;
            }
            // Indexing a BOXED array (or any numeric-keyed container) whose ELEMENT is a sealed
            // scalar record: codegen reads the boxed element and PROJECTS it into a FRESH +1 sealed
            // struct (`unbox_tagged_val_to_type` → `sealed_project_from`, which retains the element's
            // heap fields into the new struct). The result is therefore ALREADY owned and
            // container-independent — exactly like the union `result_is_fresh_owned` box above.
            // The generic `is_rc_type` retain below would treat it as a borrowed interior value and
            // add a SECOND reference that is never released (only one scope-exit release is emitted),
            // leaking the struct (and its heap fields) once per evaluation — the dominant per-`ts[i]`
            // leak in a boxed `Trip[]` build/drop loop (ASan-confirmed). Register it owned directly,
            // skipping the spurious retain. (Gated on `result_is_fresh_owned` so a sealed value read
            // through a borrowed path — were one to reach here — still takes the retain.)
            if result_is_fresh_owned && is_sealed_scalar_repr(result_type) {
                builder.register_owned(dst, result_type.clone());
                return dst;
            }
            if is_rc_type(result_type) {
                builder.emit(Instruction::Retain { val: dst, ty: result_type.clone() });
                builder.register_owned(dst, result_type.clone());
            }
            dst
        }

        TypedExpr::FieldGet { object, field, result_type, .. } => {
            // FUSED `arr[i].field` over a sealed-record array (Stage 3): a constant-offset scalar
            // load directly from the contiguous element, skipping the per-element struct
            // materialization the generic Index path would do.
            if let Some(t) = try_lower_sealed_array_field(object, field, result_type, builder, ctx) {
                return t;
            }
            let obj_ty = object.ty();
            let obj_temp = lower_expr(object, builder, ctx);
            let dst = builder.alloc_temp(result_type.clone());
            builder.emit(Instruction::FieldGet {
                dst,
                object: obj_temp,
                field: field.clone(),
                obj_ty,
                result_ty: result_type.clone(),
            });
            // Projection has VALUE (snapshot) semantics — materialize an owned value. See the
            // Index case above for the full rationale. A union/Json field is an INTERIOR
            // `*TaggedVal` into the (movable) container storage, so relocate it into a fresh
            // owned box (`CloneBox` → `lin_tagged_clone`); a concrete heap field is a stable
            // pointer, dup it (retain + register owned).
            if is_union_ty(result_type) {
                let owned = builder.alloc_temp(result_type.clone());
                builder.emit(Instruction::CloneBox { dst: owned, src: dst, ty: result_type.clone() });
                builder.register_owned(owned, result_type.clone());
                return owned;
            }
            if is_rc_type(result_type) {
                builder.emit(Instruction::Retain { val: dst, ty: result_type.clone() });
                builder.register_owned(dst, result_type.clone());
            }
            dst
        }

        TypedExpr::StringInterp { parts, .. } => {
            lower_string_interp(parts, builder, ctx)
        }

        TypedExpr::Is { expr, pattern, .. } => {
            let val_ty = expr.ty();
            let raw = lower_expr(expr, builder, ctx);
            // The tag check needs a boxed TaggedVal*; box a concrete value first.
            let val_temp = box_to_json(raw, &val_ty, builder);
            // An object pattern (`is { .. }`, and the desugared `is Error`) is a structural
            // shape + value-constraint check, NOT a bare tag check. `pattern_type_check` maps
            // an object pattern to `Type::Never` (tag 0xFF) which would never match — route it
            // through the shared object-pattern test (field presence + discriminant equality),
            // the same path match-arm `is { .. }` uses.
            if matches!(pattern, TypedPattern::Object { .. }) {
                return match lower_object_pattern_test(pattern, val_temp, builder, ctx) {
                    PatternTest::Cond(t) => t,
                    PatternTest::Always => builder.const_temp(Const::Bool(true)),
                };
            }
            // `is <Named>` resolving to a non-empty object shape (e.g. a user object-type alias
            // like `Person`): a bare tag check (or the mere field-presence the earlier rule folded
            // into ADR-036 checked) matches objects
            // with the WRONG field types, which is unsound — the arm then narrows the binding and
            // a subsequent field access operates on the wrong runtime type. Deep-validate field
            // types recursively via the `fromJson` structural walker (ADR-036). `MatchesSchema`
            // borrows the boxed value and reads a static descriptor — no ownership change, so the
            // `val_temp` boxing is the same one the former HasPattern path used.
            if let TypedPattern::TypeCheckDeep(target, named_defs, _) = pattern {
                // FAST PATH (same soundness rule as the match-arm case): a closed
                // concrete union scrutinee guarantees the value conforms to exactly
                // one variant, so a cheap discriminator selects V without the
                // recursive `MatchesSchema` re-validation. Falls back otherwise.
                if let Some(disc) = union_discriminator(&val_ty, target, named_defs) {
                    return emit_discriminator(&disc, val_temp, &val_ty, builder);
                }
                let dst = builder.alloc_temp(Type::Bool);
                builder.emit(Instruction::MatchesSchema {
                    dst,
                    val: val_temp,
                    target: target.clone(),
                    named_defs: named_defs.clone(),
                });
                return dst;
            }
            let dst = builder.alloc_temp(Type::Bool);
            let (check_ty, _span) = pattern_type_check(pattern);
            builder.emit(Instruction::IsType {
                dst,
                val: val_temp,
                ty: check_ty,
            });
            dst
        }

        TypedExpr::Has { expr, pattern, .. } => {
            let val_ty = expr.ty();
            let raw = lower_expr(expr, builder, ctx);
            // HasPattern inspects an object via a boxed TaggedVal*; box a concrete object.
            let val_temp = box_to_json(raw, &val_ty, builder);
            let dst = builder.alloc_temp(Type::Bool);
            let required_fields = pattern_required_fields(pattern);
            builder.emit(Instruction::HasPattern {
                dst,
                val: val_temp,
                pattern: HasDesc { required_fields },
            });
            dst
        }

        TypedExpr::IndexSet { object, key, value, obj_ty, .. } => {
            let key_ty = key.ty();
            let val_ty = value.ty();
            // SEALED-RECORD FIELD WRITE (the write counterpart of the FieldGet fast path above):
            // `rec["field"] = value` over a sealed-scalar record with a compile-time string key that
            // names a STATICALLY PRESENT field → a constant-offset packed-struct store (codegen
            // `FieldSet`), NOT the dynamic `lin_object_set` path. A sealed record is a packed heap
            // struct with no runtime key table, so `lin_object_set` reads its packed bytes as a
            // LinObject header and crashes (the index-cap underflow). For a SCALAR field this is a
            // direct store; for a HEAP field codegen releases the old pointer and retains the new, so
            // the source value stays owned (released at scope exit) — exactly the object-set retain
            // semantics, so we do NOT transfer/consume the value. An absent field is statically Null
            // (§6.1); we fall through to the generic path which the codegen handles (object branch),
            // but a sealed record can never carry an extra field, so this is effectively a no-op
            // write — still safer to route present fields here and leave the rest to the fallback.
            if is_sealed_scalar_repr(obj_ty) {
                if let TypedExpr::StringLit(name, _, _) = key.as_ref() {
                    let present = matches!(obj_ty,
                        Type::Object { fields, .. } if fields.contains_key(name));
                    if present {
                        // Lower object then value (the value may reference the object's fields, e.g.
                        // `rng["state"] = rng["state"] + rng["inc"]`; reading first is correct).
                        let obj_temp = lower_expr(object, builder, ctx);
                        let val_temp = lower_expr(value, builder, ctx);
                        builder.emit(Instruction::FieldSet {
                            object: obj_temp,
                            field: name.clone(),
                            value: val_temp,
                            obj_ty: obj_ty.clone(),
                            val_ty,
                        });
                        // FieldSet evaluates to Null.
                        return builder.const_temp(Const::Null);
                    }
                }
            }
            // Borrow the container (a bare `LocalGet` of a concrete-RC array/object): a store
            // writes THROUGH the container without needing to own it, so skip the retain/release
            // the owning load would emit. The KEY and VALUE are lowered FIRST so the borrowed base
            // load is the last thing before the `IndexSet` — neither sub-expression can then leave
            // a dangling borrow by reassigning the container global. The owning fallback keeps the
            // original base→key→value order (its retain makes ordering immaterial).
            let (obj_temp, key_temp, val_temp) =
                if lower_container_base_borrowed_check(object, ctx) {
                    let key_temp = lower_expr(key, builder, ctx);
                    let val_temp = lower_expr(value, builder, ctx);
                    let obj_temp = lower_container_base_borrowed(object, builder, ctx)
                        .unwrap_or_else(|| lower_expr(object, builder, ctx));
                    (obj_temp, key_temp, val_temp)
                } else {
                    let obj_temp = lower_expr(object, builder, ctx);
                    let key_temp = lower_expr(key, builder, ctx);
                    let val_temp = lower_expr(value, builder, ctx);
                    (obj_temp, key_temp, val_temp)
                };
            // `arr[i] = v` transfers a reference into the container exactly like the
            // `lin_array_set`/`lin_object_set` intrinsics (codegen routes both through the
            // same `emit_array_set`/`emit_object_set` helpers). Balance ownership of the
            // stored value with the matching rule:
            //   - Array/FixedArray store via `lin_array_set` MOVES a union box (raw struct
            //     copy, no inner retain) ⇒ consume: a fresh union source is unregistered (and
            //     its orphaned box shell freed below), a borrowed one is retained.
            //   - Object/Named (and the runtime-dispatched TypeVar/Union case, where codegen
            //     adds a `lin_tagged_retain` on the array branch so both branches are retain-
            //     style) store via `lin_object_set`, which RETAINS the inner ⇒ no consume.
            // A concrete heap value is consumed by every store regardless of this flag.
            let op_consumes_union = matches!(obj_ty, Type::Array(_) | Type::FixedArray(_));
            // A sealed-record array store COPIES the element struct's payload (`lin_sealed_array_set`
            // retains heap fields per descriptor); the source struct stays OWNED and is released at
            // scope exit (dropping its heap fields, balancing the retains). Skip the transfer.
            //
            // A SEALED-repr element set into a BOXED (tagged `Object[]`) array is the index-set
            // analogue of `push_sealed_elem_into_tagged`: codegen MATERIALIZES a fresh boxed
            // LinObject from the sealed struct (retaining its heap fields into the new object) and
            // stores THAT — it does NOT store the source struct pointer. So the source must STAY
            // OWNED (released at scope exit, dropping its heap fields, balancing the materialization's
            // per-field retains). A `transfer_into_container` Retain here would add a reference the
            // array never holds → a per-set leak of the source struct (ASan-confirmed once the
            // materialization crash was fixed). Skip the transfer for this case too.
            let set_sealed_elem_into_tagged = is_sealed_scalar_repr(&val_ty)
                && !is_sealed_scalar_array(obj_ty);
            if !is_sealed_scalar_array(obj_ty) && !set_sealed_elem_into_tagged {
                builder.transfer_into_container(val_temp, value, op_consumes_union);
            }
            let free_shell = op_consumes_union
                && is_union_ty(&val_ty)
                && expr_is_fresh_alloc(value);
            builder.emit(Instruction::IndexSet {
                object: obj_temp,
                key: key_temp,
                value: val_temp,
                obj_ty: obj_ty.clone(),
                key_ty,
                val_ty,
            });
            // A fresh union box consumed by `lin_array_set` leaves an orphaned 16-byte shell
            // (the slot owns the inner; the source box header is unreferenced) — free it after
            // the set has read from it. Mirrors the `ArraySetDyn` intrinsic path.
            if free_shell {
                builder.emit(Instruction::FreeBoxShell { val: val_temp });
            }
            // IndexSet evaluates to Null.
            builder.const_temp(Const::Null)
        }
    }
}

// -------------------------------------------------------------------------
// Call lowering
// -------------------------------------------------------------------------

/// Lower a single call argument, coercing it to the callee's parameter type. When the
/// argument is a closure literal and the parameter declares a callback with a concrete
/// (non-union, non-void) return type, the closure is compiled to return that concrete type
/// (so an AST-compiled higher-order callee receives a raw value), bypassing the uniform
/// boxed-return ABI.
fn lower_call_arg(a: &TypedExpr, param_ty: Option<&Type>, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    if let (TypedExpr::Function { name, params, body, ret_type, captures, .. },
            Some(Type::Function { params: cb_params, ret: cb_ret, .. })) = (a, param_ty)
    {
        // Only force a concrete return when the callback's params are ALSO concrete. If any
        // param is union/Json (TypeVar), the AST closure-call convention
        // (build_closure_call_typed) calls with a boxed (ptr) return and unboxes — so the
        // closure must keep the uniform boxed ABI, not a forced concrete return.
        let concrete_params = cb_params.iter().all(|p| !is_union_ty(p));
        if concrete_params && !is_union_ty(cb_ret) && !matches!(**cb_ret, Type::Null | Type::Never) {
            return lower_callback_arg(cb_ret, name.as_deref(), params, body, ret_type, captures, builder, ctx);
        }
    }
    let t = lower_expr(a, builder, ctx);
    let coerced = lower_coerce_arg(t, &a.ty(), param_ty, builder);
    move_streamish_arg(&a.ty(), coerced, builder);
    coerced
}

/// A streamish ARGUMENT is MOVED into the callee (streams brief §7/§9): the callee/worker takes
/// ownership of the boxed-stream pointer, so the caller must NOT release it at scope exit.
/// Unregister the lowered arg temp from the caller's owning scope. The affine check guarantees
/// the caller never uses it again. No-op for non-stream args.
fn move_streamish_arg(arg_ty: &Type, t: Temp, builder: &mut FuncBuilder) {
    if type_is_streamish_ir(arg_ty) {
        builder.unregister_owned(t);
    }
}

/// Lower argument `i` of a call, combining two concerns:
///   1. When `i` is the callback index of a KNOWN synchronous combinator (`cb_idx`), enable
///      the captured-cell safe-context (`safe_callback_depth`, a counter so nested combinator
///      callbacks stay safe) so a closure lowered there keeps its captured cells freeable.
///   2. Capture the RAW (pre-coercion) temp when the argument is a fresh-alloc heap literal
///      boxed into a Json/union parameter — the temp `register_owned` tracks — so `lower_call`
///      can transfer its ownership on escape (see `escape_alias`). Returns `None` otherwise.
/// The two are mutually exclusive in practice (the combinator-callback path lowers a closure,
/// which is never a boxed fresh heap literal), but composing them keeps the call site uniform.
/// A record LITERAL flowing into a `Named` param that the callee reads as a SEALED struct (the
/// self-recursive-call case — `func.ty()` carries the unexpanded `Named` alias while the callee
/// body resolved it to the sealed `Object`). Construct the sealed struct DIRECTLY (each field
/// stored by offset) instead of building a boxed `LinObject` and projecting it back — which would
/// pay a per-call `lin_object_alloc` + N sets + N `lin_object_get` on a hot recursive path. Returns
/// `Some` having lowered the arg as a sealed struct, `None` to fall through to the generic path.
/// (The generic path's `lower_coerce_arg` still PROJECTS correctly for the cases this misses, e.g.
/// a non-literal sealed-compatible value flowing into a `Named` param — this is only the
/// construction fast path.)
fn try_lower_sealed_literal_into_named(
    a: &TypedExpr,
    param_ty: Option<&Type>,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Option<Temp> {
    if !matches!(param_ty, Some(Type::Named(_))) {
        return None;
    }
    let TypedExpr::MakeObject { fields, spreads, .. } = a else { return None };
    if !spreads.is_empty() {
        return None;
    }
    let aty = a.ty();
    let Type::Object { fields: afields, .. } = &aty else { return None };
    if afields.is_empty() || !afields.values().all(is_sealed_field_ty) {
        return None;
    }
    if !afields.keys().all(|k| fields.iter().any(|(fk, _)| fk == k)) {
        return None;
    }
    let sealed_ty = Type::sealed_object(afields.clone());
    try_lower_sealed_literal(a, &sealed_ty, builder, ctx)
}

fn lower_call_arg_tracked(
    a: &TypedExpr,
    param_ty: Option<&Type>,
    i: usize,
    cb_idx: Option<usize>,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> (Temp, Option<Temp>) {
    // Sealed record literal → `Named` param: construct the sealed struct directly (see helper).
    // Checked BEFORE the boxed-shell branch below, which would otherwise box the literal as Json.
    if let Some(t) = try_lower_sealed_literal_into_named(a, param_ty, builder, ctx) {
        return (t, None);
    }
    // Fresh-alloc heap literal boxed into a Json/union param: capture the raw temp for
    // transfer-on-escape tracking. (This path never coincides with a combinator callback.)
    if arg_box_is_caller_owned_shell(&a.ty(), param_ty) && expr_is_fresh_alloc(a) {
        let raw = lower_expr(a, builder, ctx);
        let coerced = lower_coerce_arg(raw, &a.ty(), param_ty, builder);
        let tracked = if coerced != raw { Some(raw) } else { None };
        move_streamish_arg(&a.ty(), coerced, builder);
        return (coerced, tracked);
    }
    // Combinator callback position: enable the safe captured-cell context while lowering.
    if cb_idx == Some(i) {
        ctx.safe_callback_depth += 1;
        let t = lower_call_arg(a, param_ty, builder, ctx);
        ctx.safe_callback_depth -= 1;
        return (t, None);
    }
    (lower_call_arg(a, param_ty, builder, ctx), None)
}

fn lower_call(
    func: &TypedExpr,
    args: &[TypedExpr],
    result_type: &Type,
    is_tail: bool,
    partial: bool,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Temp {
    // Check if this is an intrinsic call.
    if let TypedExpr::LocalGet { slot, .. } = func {
        if let Some(name) = builder.intrinsic_slots.get(slot).cloned() {
            return lower_intrinsic_call(&name, args, result_type, builder, ctx);
        }
        // Total declared arity of the callee — used to detect a default-fill call (fewer
        // non-partial args than parameters), which routes to a per-arity adapter symbol.
        let total_arity = match func.ty() {
            Type::Function { params, .. } => params.len(),
            _ => args.len(),
        };
        let is_default_fill = !partial && args.len() < total_arity;
        // If the callee is a KNOWN synchronous combinator (stdlib for/map/filter/…), the index
        // of its callback argument: a closure lowered there is consumed synchronously and does
        // not escape, so captured cells stay freeable. None for any other callee (conservative).
        let cb_idx = ctx.safe_combinator_slots.get(slot).copied();
        // Imported function: call the compiled symbol by its mangled name, boxing
        // concrete args passed to Json/union-typed parameters.
        if let Some((sym, param_tys)) = ctx.import_fn_slots.get(slot).cloned() {
            // STREAM COMBINATOR DISPATCH (std/iter unification Stage 3/4): when a genuine
            // `std/iter` combinator is called with a DEFINITELY-stream receiver (arg0), redirect to
            // the lazy `lin_stream_*` backend instead of running the eager array body. Keyed on the
            // import symbol (`std_iter_<name>`) so a user-defined same-named function is never
            // affected — the mirror of the checker's `streamish_combinator_ret`. The redirect
            // delegates to `lower_intrinsic_call` so the stream-arg RC + result ownership match the
            // proven std/stream wrapper path exactly.
            if let Some(stream_intr) = stream_combinator_intrinsic_name(&sym, args) {
                return lower_intrinsic_call(stream_intr, args, result_type, builder, ctx);
            }
            let mut shell_boxes: Vec<Temp> = Vec::new();
            // Fully-owned arg boxes (sealed-record array materialized to Json) released right after
            // the call.
            let mut full_release_boxes: Vec<(Temp, Type)> = Vec::new();
            let mut escape_lits: Vec<Temp> = Vec::new();
            let lowered_args: Vec<Temp> = args
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    let (arg, raw_lit) = lower_call_arg_tracked(a, param_tys.get(i), i, cb_idx, builder, ctx);
                    retain_call_arg(arg, &a.ty(), expr_is_fresh_alloc(a), builder);
                    // A sealed-record array boxed into a Json param is a FRESH fully-owned tagged
                    // array (not a borrowed-inner shell): FULLY release it RIGHT AFTER the call (box
                    // + inner array + element objects), not at function scope exit — the call may be
                    // in a tail-recursive (loop) body where a scope-exit release would leak every
                    // iteration. Other heap args boxed to Json are shell-only (borrowed inner).
                    if param_tys.get(i).map(|p| sealed_array_arg_materialized(&a.ty(), p)
                        || sealed_record_arg_materialized(&a.ty(), p)).unwrap_or(false)
                    {
                        full_release_boxes.push((arg, param_tys[i].clone()));
                    } else if arg_box_is_caller_owned_shell(&a.ty(), param_tys.get(i))
                        || arg_box_is_caller_owned_scalar_shell(&a.ty(), param_tys.get(i))
                    {
                        shell_boxes.push(arg);
                    }
                    if let Some(lit) = raw_lit {
                        escape_lits.push(lit);
                    }
                    arg
                })
                .collect();
            // A default-fill call targets the import's `{sym}$default{k}` adapter, which fills
            // the remaining defaults and tail-calls the real export.
            let callee_sym = if is_default_fill {
                format!("{}$default{}", sym, args.len())
            } else {
                sym
            };
            let dst = builder.alloc_temp(result_type.clone());
            builder.emit(Instruction::Call {
                dst,
                callee: CallTarget::Named(callee_sym),
                args: lowered_args,
                ret_ty: result_type.clone(),
            });
            free_arg_box_shells(&shell_boxes, dst, builder);
            for (b, bty) in &full_release_boxes {
                builder.emit(Instruction::Release { val: *b, ty: bty.clone() });
            }
            builder.register_owned(dst, result_type.clone());
            // No transfer-on-escape aliasing for the boxed fresh-literal args: a callee returning a
            // borrowed Json/union param does so by CLONE (the function-return path clones a borrowed
            // union body-result into a fresh +1 box — `lower_function_expr_with_id`'s `is_union_ty &&
            // !is_owned_in_scope` clone, added in da9ec08, AFTER the original escape-alias of 2af264a
            // assumed a by-REFERENCE return). The result therefore never aliases the arg literal's
            // inner payload, so the literal's own +1 must be released normally at the arg scope exit
            // (its shell was freed by `free_arg_box_shells`). Keeping it alive via escape-alias here
            // leaked the literal every call (the `firstOr([1,2,3], d)` / accumulator-threading shape).
            let _ = &escape_lits;
            return dst;
        }
        // Check global function slots.
        if let Some(&fid) = ctx.global_fn_slots.get(slot) {
            // Box concrete args to Json/union params and retain Function-typed args,
            // matching the callee's compiled signature (see imported-function path).
            let param_tys: Vec<Type> = match func.ty() {
                Type::Function { params, .. } => params,
                _ => vec![],
            };
            let mut shell_boxes: Vec<Temp> = Vec::new();
            // Fully-owned arg boxes (sealed-record array materialized to Json) released right after
            // the call.
            let mut full_release_boxes: Vec<(Temp, Type)> = Vec::new();
            let mut escape_lits: Vec<Temp> = Vec::new();
            let lowered_args: Vec<Temp> = args
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    let (arg, raw_lit) = lower_call_arg_tracked(a, param_tys.get(i), i, cb_idx, builder, ctx);
                    retain_call_arg(arg, &a.ty(), expr_is_fresh_alloc(a), builder);
                    // A sealed-record array boxed into a Json param is a FRESH fully-owned tagged
                    // array (not a borrowed-inner shell): FULLY release it RIGHT AFTER the call (box
                    // + inner array + element objects), not at function scope exit — the call may be
                    // in a tail-recursive (loop) body where a scope-exit release would leak every
                    // iteration. Other heap args boxed to Json are shell-only (borrowed inner).
                    if param_tys.get(i).map(|p| sealed_array_arg_materialized(&a.ty(), p)
                        || sealed_record_arg_materialized(&a.ty(), p)).unwrap_or(false)
                    {
                        full_release_boxes.push((arg, param_tys[i].clone()));
                    } else if arg_box_is_caller_owned_shell(&a.ty(), param_tys.get(i))
                        || arg_box_is_caller_owned_scalar_shell(&a.ty(), param_tys.get(i))
                    {
                        shell_boxes.push(arg);
                    }
                    if let Some(lit) = raw_lit {
                        escape_lits.push(lit);
                    }
                    arg
                })
                .collect();
            // A default-fill call dispatches to the pre-registered adapter for this arity
            // (Direct call). The adapter fills the remaining defaults and tail-calls the real
            // function. Partial application (`f(x,)`) keeps the real fid and is handled by
            // codegen's partial-application path.
            let callee_fid = if is_default_fill {
                ctx.default_adapters.get(&(fid, args.len())).copied().unwrap_or(fid)
            } else {
                fid
            };
            // A default-fill call routes to the adapter, which has a different (smaller) arity
            // than the current function — so it can never use the self-recursive TailCall fast
            // path (which jumps to the current function's entry expecting all parameters).
            if is_tail && !is_default_fill {
                // A tail call has no "after" block in which to free arg-box shells; the box is
                // consumed by the jump. A boxed concrete-heap arg in tail position is rare
                // (would require a self-recursive function taking a Json param a concrete heap
                // value is passed to), and the small per-tail-call shell leak is left unfixed.
                //
                // Release every per-iteration owned temp the body allocated (projections, clones,
                // string literals) on THIS live block before the diverging jump — otherwise their
                // scope-exit releases land in the unreachable `tco_post` chain and leak once per
                // iteration (the dominant RAPTOR scanBack leak). A transferring arg keeps its
                // single +1 (it moves into the param slot); a PASS-THROUGH param arg (threaded
                // unchanged) is fully released — its value is the same borrowed pointer the caller
                // owns and codegen's release-old skips it.
                builder.release_owned_for_tail_call(&lowered_args);
                builder.terminate(Terminator::TailCall { args: lowered_args.clone() });
                // Dead block to keep IR valid.
                let post = builder.alloc_block("tco_post");
                builder.diverged_blocks.insert(post);
                builder.switch_to(post);
                return builder.alloc_temp(result_type.clone());
            }
            let dst = builder.alloc_temp(result_type.clone());
            builder.emit(Instruction::Call {
                dst,
                callee: CallTarget::Direct(callee_fid),
                args: lowered_args,
                ret_ty: result_type.clone(),
            });
            free_arg_box_shells(&shell_boxes, dst, builder);
            for (b, bty) in &full_release_boxes {
                builder.emit(Instruction::Release { val: *b, ty: bty.clone() });
            }
            builder.register_owned(dst, result_type.clone());
            // No transfer-on-escape aliasing — see the imported-call path above: union param
            // returns CLONE (da9ec08), so the result never aliases the arg literal's inner; the
            // literal's +1 is released normally at the arg scope exit (shell already freed).
            let _ = &escape_lits;
            return dst;
        }
    }

    let fn_temp = lower_expr(func, builder, ctx);
    // Box concrete args to Json/union params and retain Function-typed args, matching the
    // closure's declared parameter types — exactly as the named/imported call paths above.
    // Without this, e.g. an Array passed to a `Json` closure param reaches the callee as a
    // raw `LinArray*` instead of a boxed `TaggedVal*`, so the callee reads the tag/payload
    // from garbage and mutations through it are lost (silent data corruption).
    let param_tys: Vec<Type> = match func.ty() {
        Type::Function { params, .. } => params,
        _ => vec![],
    };
    let mut escape_lits: Vec<Temp> = Vec::new();
    let mut shell_boxes: Vec<Temp> = Vec::new();
    // Fully-owned arg boxes (sealed-record array/scalar materialized to Json) released right after
    // the call — matching the named/imported call paths above.
    let mut full_release_boxes: Vec<(Temp, Type)> = Vec::new();
    let lowered_args: Vec<Temp> = args
        .iter()
        .enumerate()
        .map(|(i, a)| {
            // Indirect call through a closure value: the callee is not a known synchronous
            // combinator, so cb_idx is None (no safe captured-cell context — conservative).
            let (arg, raw_lit) = lower_call_arg_tracked(a, param_tys.get(i), i, None, builder, ctx);
            retain_call_arg(arg, &a.ty(), expr_is_fresh_alloc(a), builder);
            // A sealed-record array OR sealed scalar record boxed into a Json/union param MATERIALIZES
            // a FRESH fully-owned inner heap value (not a borrowed-inner shell): FULLY release it
            // (box + inner + element/field refs) right after the call, like the named/imported paths.
            if param_tys.get(i).map(|p| sealed_array_arg_materialized(&a.ty(), p)
                || sealed_record_arg_materialized(&a.ty(), p)).unwrap_or(false)
            {
                full_release_boxes.push((arg, param_tys[i].clone()));
            } else if arg_box_is_caller_owned_shell(&a.ty(), param_tys.get(i))
                || arg_box_is_caller_owned_scalar_shell(&a.ty(), param_tys.get(i))
            {
                // A concrete heap value (or a non-cached scalar) boxed into a Json/union closure param
                // is a caller-owned SHELL the closure never releases. Free the shell after the call,
                // like the named/imported paths. Without this, a fresh literal / large-int / float
                // passed to a Json closure param leaked its 16-byte box shell every call.
                shell_boxes.push(arg);
            }
            if let Some(lit) = raw_lit {
                escape_lits.push(lit);
            }
            arg
        })
        .collect();

    if is_tail {
        // Release per-iteration body-owned temps on the live block before the diverging jump
        // (see release_owned_for_tail_call); their scope-exit releases would otherwise land in
        // the unreachable post block and leak each iteration. A transferring arg keeps its +1
        // (moves into the param slot); a pass-through param arg is fully released.
        builder.release_owned_for_tail_call(&lowered_args);
        builder.terminate(Terminator::TailCall { args: lowered_args.clone() });
        let post = builder.alloc_block("tco_post");
        builder.diverged_blocks.insert(post);
        builder.switch_to(post);
        return builder.alloc_temp(result_type.clone());
    }

    let dst = builder.alloc_temp(result_type.clone());
    builder.emit(Instruction::Call {
        dst,
        callee: CallTarget::Indirect(fn_temp),
        args: lowered_args,
        ret_ty: result_type.clone(),
    });
    free_arg_box_shells(&shell_boxes, dst, builder);
    for (b, bty) in &full_release_boxes {
        builder.emit(Instruction::Release { val: *b, ty: bty.clone() });
    }
    // Concrete rc results are owned (+1) here; a UNION result from an INDIRECT closure call is
    // NOT registered, because the closure return ABI does NOT guarantee +1 for a boxed-union
    // return: a closure whose body yields a borrowed param/local box (e.g. minBy's
    // `(acc, x) => if x[0] < acc[0] then x else acc`) hands back a +0 box. Registering it would
    // make scope-exit release a box the callee never owned us → double-free. (Concrete rc returns
    // ARE +1: a concrete param read retains in place before the closure keeps it on return.)
    if is_rc_type(result_type) {
        builder.register_owned(dst, result_type.clone());
    }
    // No transfer-on-escape aliasing — see the named/imported call paths above: union param
    // returns CLONE (da9ec08), so the result never aliases the arg literal's inner payload; the
    // literal's +1 is released normally at the arg scope exit (its shell already freed).
    let _ = &escape_lits;
    dst
}

fn lower_intrinsic_call(
    name: &str,
    args: &[TypedExpr],
    result_type: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Temp {
    // Control-flow / iteration intrinsics are lowered to explicit LinIR basic blocks
    // (Option B) rather than opaque runtime calls, so liveness/rc_elide can see through
    // them. Each is handled by a dedicated lowering routine.
    match name {
        "lin_range" => return lower_range(args, builder, ctx),
        "lin_for" => return lower_for(args, builder, ctx),
        "lin_while" => return lower_while(args, builder, ctx),
        "lin_iter" => return lower_iter(args, result_type, builder, ctx),
        "lin_map" => return lower_map(args, result_type, builder, ctx),
        "lin_filter" => return lower_filter(args, result_type, builder, ctx),
        "lin_reduce" => return lower_reduce(args, result_type, builder, ctx),
        "lin_sort" => return lower_sort(args, result_type, builder, ctx),
        _ => {}
    }

    let intrinsic = match name {
        "lin_print" => Intrinsic::Print,
        "lin_to_string" => Intrinsic::ToString,
        "lin_length" => Intrinsic::Length,
        "lin_push" => Intrinsic::Push,
        "lin_object_set" => Intrinsic::ObjectSetDyn,
        "lin_array_set" => Intrinsic::ArraySetDyn,
        "lin_keys" => Intrinsic::Keys,
        "lin_value_key" => Intrinsic::ValueKey,
        "lin_to_json" => Intrinsic::ToJson,
        "lin_array_allocate" => Intrinsic::ArrayAllocate,
        "lin_array_allocate_filled" => Intrinsic::ArrayAllocateFilled,
        "concat" => Intrinsic::Concat,
        "lin_async" => Intrinsic::Async,
        // pool.poolAsync(f) → lin_pool_async(pool, f): same intrinsic as async, but the 2-arg
        // form routes to the bounded thread pool (codegen's Async branch detects the pool arg).
        "lin_pool_async" => Intrinsic::Async,
        "lin_await" => Intrinsic::Await,
        "lin_exit" => Intrinsic::Exit,
        "lin_parallel" => Intrinsic::Parallel,
        "lin_race" => Intrinsic::Race,
        "lin_timeout" => Intrinsic::Timeout,
        "lin_retry" => Intrinsic::Retry,
        "lin_thread_pool" => Intrinsic::ThreadPool,
        "lin_shared" => Intrinsic::SharedNew,
        "lin_shared_get" => Intrinsic::SharedGet,
        "lin_shared_set" => Intrinsic::SharedSet,
        "lin_shared_with_lock" => Intrinsic::SharedWithLock,
        "lin_freeze" => Intrinsic::Freeze,
        "lin_worker" => Intrinsic::Worker,
        "lin_serve" => Intrinsic::Serve,
        "lin_request" => Intrinsic::Request,
        "lin_message" => Intrinsic::Message,
        "lin_close" => Intrinsic::Close,
        "lin_fs_open" => Intrinsic::StreamOpen,
        "lin_stream_read" => Intrinsic::StreamRead,
        "lin_stream_close" => Intrinsic::StreamClose,
        "lin_stream_map" => Intrinsic::StreamMap,
        "lin_stream_filter" => Intrinsic::StreamFilter,
        "lin_stream_take" => Intrinsic::StreamTake,
        "lin_stream_drop" => Intrinsic::StreamDrop,
        "lin_stream_take_while" => Intrinsic::StreamTakeWhile,
        "lin_stream_drop_while" => Intrinsic::StreamDropWhile,
        "lin_stream_flat_map" => Intrinsic::StreamFlatMap,
        "lin_stream_flatten" => Intrinsic::StreamFlatten,
        "lin_stream_concat" => Intrinsic::StreamConcat,
        "lin_stream_gunzip" => Intrinsic::StreamGunzip,
        "lin_stream_gzip" => Intrinsic::StreamGzip,
        "lin_stream_inflate" => Intrinsic::StreamInflate,
        "lin_stream_deflate" => Intrinsic::StreamDeflate,
        "lin_stream_untar" => Intrinsic::StreamUntar,
        "lin_stream_manifest" => Intrinsic::StreamManifest,
        "lin_stream_files" => Intrinsic::StreamFiles,
        "lin_stream_reduce" => Intrinsic::StreamReduce,
        "lin_stream_find" => Intrinsic::StreamFind,
        "lin_stream_some" => Intrinsic::StreamSome,
        "lin_stream_every" => Intrinsic::StreamEvery,
        "lin_stream_while" => Intrinsic::StreamWhile,
        "lin_stream_lines" => Intrinsic::StreamLines,
        "lin_stream_chunks" => Intrinsic::StreamChunks,
        "lin_stream_write" => Intrinsic::StreamWrite,
        "lin_stream_write_lines" => Intrinsic::StreamWriteLines,
        "lin_stream_drain" => Intrinsic::StreamDrain,
        "lin_stream_collect" => Intrinsic::StreamCollect,
        "lin_stream_read_text" => Intrinsic::StreamReadText,
        "lin_net_tcp_stream" => Intrinsic::StreamTcp,
        "lin_process_stdout_stream" => Intrinsic::StreamStdout,
        "lin_io_stdin_stream" => Intrinsic::StreamStdin,
        "lin_stream_promise" => Intrinsic::StreamPromise,
        _ => {
            // Unknown intrinsic: lower as indirect call fallback.
            let lowered_args: Vec<Temp> = args.iter().map(|a| lower_expr(a, builder, ctx)).collect();
            let dst = builder.alloc_temp(result_type.clone());
            builder.emit(Instruction::Call {
                dst,
                callee: CallTarget::Named(name.to_string()),
                args: lowered_args,
                ret_ty: result_type.clone(),
            });
            builder.register_owned(dst, result_type.clone());
            return dst;
        }
    };
    let lowered_args: Vec<Temp> = args.iter().map(|a| lower_expr(a, builder, ctx)).collect();

    // `.promise()` MOVES its stream argument onto the worker (Stage 8): the runtime takes
    // ownership of the boxed-stream pointer, so the caller must NOT release it (handoff). Suppress
    // the source temp's scope-exit release — without this the caller AND the worker would both
    // release the box (double-free / double-close). The affine check guarantees no further use.
    if matches!(intrinsic, Intrinsic::StreamPromise) {
        if let Some(&arg0) = lowered_args.first() {
            builder.unregister_owned(arg0);
        }
    }

    // `push(arr, elem)` / `set(arr, idx, elem)` / `object_set(obj, key, val)` all transfer a
    // reference to their LAST argument into the container. For push/set, codegen stores the
    // pointer / copies the boxed value without retaining; for object_set, codegen boxes the
    // value, calls lin_object_set (which retains the inner), then releases the box (undoing
    // that retain) — net effect is also a transfer. So the standard container-insert ownership
    // rule applies to the element in every case.
    // A fresh UNION box consumed by `lin_array_set` (raw struct move, no inner retain) leaves an
    // orphaned 16-byte box SHELL: the array slot owns the inner, but the source box header is no
    // longer referenced and must be freed (shell only — freeing the inner would corrupt the slot).
    // Freed AFTER the set (the set reads from the box), via FreeBoxShell (`lin_tagged_free_box`,
    // null/cached-box safe). This is the per-element box leak inside `map`'s
    // `lin_array_set(result, i, f(item))`.
    let mut shell_to_free: Option<Temp> = None;
    // `push(arr, elem)` into a SEALED-RECORD ARRAY (Stage 3) COPIES the element struct's payload
    // (and retains its heap fields per descriptor) — it does NOT take the struct pointer. So the
    // source struct must STAY OWNED (released at scope exit, which also drops its heap fields,
    // balancing the per-field retains the copy took). Skip the ownership transfer for this case.
    let push_into_sealed_array = matches!(intrinsic, Intrinsic::Push)
        && args.first().map(|a| is_sealed_scalar_array(&a.ty())).unwrap_or(false);
    // `push(arr, elem)` where `elem` is a SEALED-repr record (`{tag:Int32, bytes:Int32[]}`, the
    // generic `push$Object` over a `Field[]`) into a TAGGED array: codegen MATERIALIZES the sealed
    // struct into a fresh boxed LinObject (retaining its heap fields) and stores THAT — it does NOT
    // store the sealed struct pointer. So, exactly like `push_into_sealed_array`, the source struct
    // must STAY OWNED (released at scope exit, dropping its heap fields, balancing the per-field
    // retains the materialization took). Skipping the transfer here avoids a double-retain of the
    // sealed source whose +1 the array never holds → a per-push leak of the struct shell + its
    // heap-field references (caught under ASan as ~7 leaked allocs/iteration).
    let push_sealed_elem_into_tagged = matches!(intrinsic, Intrinsic::Push)
        && args.last().map(|a| is_sealed_scalar_repr(&a.ty())).unwrap_or(false)
        && args.first().map(|a| !is_sealed_scalar_array(&a.ty())).unwrap_or(false);
    if matches!(intrinsic, Intrinsic::Push | Intrinsic::ArraySetDyn | Intrinsic::ObjectSetDyn)
        && !push_into_sealed_array
        && !push_sealed_elem_into_tagged
    {
        if let (Some(elem_expr), Some(&elem_temp)) = (args.last(), lowered_args.last()) {
            // For a UNION element, only `lin_array_set` (ArraySetDyn) moves the box (raw struct
            // copy, no inner retain); `Push`/`object_set` retain the inner. Concrete elements are
            // always consumed regardless of this flag.
            let op_consumes_union = matches!(intrinsic, Intrinsic::ArraySetDyn);
            builder.transfer_into_container(elem_temp, elem_expr, op_consumes_union);
            if op_consumes_union
                && is_union_ty(&elem_expr.ty())
                && expr_is_fresh_alloc(elem_expr)
            {
                shell_to_free = Some(elem_temp);
            }
        }
    }

    let dst = builder.alloc_temp(result_type.clone());
    builder.emit(Instruction::CallIntrinsic {
        dst,
        intrinsic,
        args: lowered_args,
        ret_ty: result_type.clone(),
    });
    if let Some(shell) = shell_to_free {
        builder.emit(Instruction::FreeBoxShell { val: shell });
    }
    builder.register_owned(dst, result_type.clone());
    dst
}

// -------------------------------------------------------------------------
// Control-flow / iteration lowering (Option B: explicit IR blocks)
// -------------------------------------------------------------------------

/// The element type produced by iterating a value of `iterable_ty`.
fn iter_elem_type(iterable_ty: &Type) -> Type {
    match iterable_ty {
        Type::Array(t) | Type::Iterator(t) => (**t).clone(),
        Type::FixedArray(ts) => ts.first().cloned().unwrap_or(Type::Null),
        // Json/union iterables yield dynamically-typed (boxed) elements.
        _ => Type::TypeVar(u32::MAX),
    }
}

/// If `name` is a KNOWN synchronous, non-retaining higher-order combinator, return the
/// argument index of its callback parameter. These stdlib functions (and the matching `lin_*`
/// intrinsics) invoke the callback synchronously during the call and never retain, store, or
/// return it — so a closure passed as that argument does NOT escape, and heap cells it captures
/// are safe to free at the creating function's scope exit. CONSERVATIVE: only these exact names
/// are trusted; every other callee leaves the captured cell escaping (leaking, but sound).
/// `reduce` takes (arr, init, f) — its callback is arg index 2; the rest take (arr, f) — index 1.
/// STREAM COMBINATOR DISPATCH (std/iter unification Stage 3/4). Given an imported function's
/// mangled symbol and the call's args, return the `lin_stream_*` intrinsic NAME to redirect to
/// when the callee is a genuine `std/iter` combinator AND its receiver (arg0) is DEFINITELY a
/// Stream. Returns None otherwise (the call proceeds as a normal Named import call / eager body).
///
/// Keyed on the `std_iter_` symbol prefix so only the real std/iter exports are redirected — a
/// user-defined `map`/`for`/… is mangled under a different module key and never matches. This is
/// the IR-side mirror of the checker's `streamish_combinator_ret` (which re-typed the result):
/// here we re-route the runtime dispatch to the lazy backend so the typed-stream result is backed
/// by an actual lazy pipeline. The eager pure-Lin / `lin_map` bodies are bypassed entirely for a
/// stream receiver.
fn stream_combinator_intrinsic_name(sym: &str, args: &[TypedExpr]) -> Option<&'static str> {
    let export = sym.strip_prefix("std_iter_")?;
    // The receiver must be DEFINITELY a stream — not a mixed `Array | Iterator | Stream` union
    // (which would mean the eager array body must still run). A bare `Stream(_)` is the only shape
    // that reaches a concrete call site here (the checker resolves the receiver before lowering).
    let arg0_is_stream = args.first().map(|a| matches!(a.ty(), Type::Stream(_))).unwrap_or(false);
    if !arg0_is_stream {
        return None;
    }
    Some(match export {
        "map" => "lin_stream_map",
        "filter" => "lin_stream_filter",
        "take" => "lin_stream_take",
        "drop" => "lin_stream_drop",
        "takeWhile" => "lin_stream_take_while",
        "dropWhile" => "lin_stream_drop_while",
        "flatMap" => "lin_stream_flat_map",
        "flatten" => "lin_stream_flatten",
        "concat" => "lin_stream_concat",
        "reduce" => "lin_stream_reduce",
        "find" => "lin_stream_find",
        "some" => "lin_stream_some",
        "every" => "lin_stream_every",
        "while" => "lin_stream_while",
        "for" => "lin_stream_for",
        _ => return None,
    })
}

fn safe_combinator_callback_index(name: &str) -> Option<usize> {
    match name {
        "for" | "while" | "map" | "filter" | "find" | "some" | "every" => Some(1),
        "reduce" => Some(2),
        _ => None,
    }
}

/// Lower a callback ARGUMENT to a known synchronous, invoke-and-discard combinator
/// (for/while/map/filter/reduce) with the captured-cell escape analysis enabled. While
/// `safe_callback_depth > 0`, a closure literal lowered here does NOT escape (the combinator
/// runs it synchronously and never retains/stores/returns it), so any heap cell it captures
/// stays a scope-exit FreeCell candidate. SOUNDNESS: these five combinators are the only
/// callers that mark the context safe; every other use of a closure (binding, return, store,
/// async/worker, unknown callee, or even another arg position) leaves the depth at 0, so the
/// captured cell is conservatively marked escaping and never freed.
fn lower_callback_in_safe_ctx(expr: &TypedExpr, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    ctx.safe_callback_depth += 1;
    let t = lower_expr(expr, builder, ctx);
    ctx.safe_callback_depth -= 1;
    t
}

/// The declared parameter types and return type of a callback expression, if it has a
/// statically-known `Function` type. Used to match the closure's compiled ABI when calling it.
fn callback_signature(expr: &TypedExpr) -> (Vec<Type>, Type) {
    match expr.ty() {
        Type::Function { params, ret, .. } => (params, *ret),
        _ => (vec![], Type::TypeVar(u32::MAX)),
    }
}

/// True for a concrete fixed-width scalar (Int*/UInt*/Float*/Bool) — a type carried UNBOXED with no
/// refcount. Used to decide whether a combinator's element read can use the FLAT scalar getter
/// (`combinator_read_elem_ty`): a scalar element has no refcount and a flat representation, so a
/// flat read on a provably-flat source is sound.
fn is_inline_scalar(ty: &Type) -> bool {
    matches!(ty,
        Type::Int8 | Type::UInt8 | Type::Int16 | Type::UInt16 |
        Type::Int32 | Type::UInt32 | Type::Int64 | Type::UInt64 |
        Type::Float32 | Type::Float64 | Type::Bool
    )
}

/// The element type to READ from `iterable` in a combinator loop, accounting for the fact that a
/// flat-scalar `T[]` static type does NOT guarantee a flat RUNTIME representation.
///
/// A `[]`+push builder (`val r = []; …push(r,x)…; r`) allocates a TAGGED array even when the binding
/// is later used as `Int32[]` (the empty literal is `Array(Never)`), so a flat read on it misreads
/// garbage. The static type can't distinguish a genuinely-flat producer (`range`/`map`/`filter`,
/// flat literals) from a `[]`+push builder. So:
///   - for a PROVABLY-FLAT source, return the concrete scalar element type → fast flat read;
///   - otherwise return the `Json` wildcard (`TypeVar(MAX)`) → the representation-agnostic TAGGED
///     read (`lin_array_get_tagged`, which dispatches on the array's runtime `elem_tag` and works
///     for BOTH flat and tagged arrays), keeping a `[]`+push array correct.
///
/// PROVABLY FLAT = the result of a flat-producing builtin/combinator (their lowering allocates a
/// flat buffer for a flat-scalar element), recognised by an `Iterator<scalar>` static result —
/// `range`/`rangeStep`/`iterOf` return `Iterator<…>`, and `map`/`filter` return arrays only via
/// these intrinsics whose declared result is `Array<scalar>` from a flat producer chain — or a
/// non-empty scalar array literal (`[1,2,3]`). A `[]`+push builder returns a plain `Array`, never an
/// `Iterator`, and a bare param/projection is also not trusted.
fn combinator_read_elem_ty(iterable: &TypedExpr, builder: &FuncBuilder, ctx: &LowerCtx) -> Type {
    let static_elem = iter_elem_type(&iterable.ty());
    if !is_inline_scalar(&static_elem) {
        // Heap/union element: already read tagged; nothing to gate.
        return static_elem;
    }
    if is_provably_flat_producer(iterable, builder, ctx) { static_elem } else { Type::TypeVar(u32::MAX) }
}

/// Emit the LOOP-BOUND length for a combinator (`for`/`while`/`map`/`filter`/`reduce`) over
/// `iterable` (already lowered to `iterable_temp`, typed `iterable_ty`).
///
/// For a statically-concrete `Array`/`Iterator`/`FixedArray` the value is known to be an array, so
/// the plain `Length` intrinsic (→ `lin_array_length`) is used. For a UNION/Json iterable the
/// runtime value may NOT be an array (e.g. an `ls()` error Object that slipped past a guard) — and
/// the element read below blindly unboxes the pointer and reads it as a `LinArray`. Using
/// `lin_length_dyn` there reports an Object's key count / String's length, so the loop would run and
/// misread the non-array payload as array memory (UB — the docs-builder crash). Routing the bound
/// through `lin_iterable_length` (array length, else 0) makes a non-array iterable a no-op loop,
/// keeping the combinator sound for any Json value. Concrete scalar-array pipelines (the ADR-044
/// fast path) are unaffected — they take the `Intrinsic::Length` branch.
fn emit_iterable_len(iterable_temp: Temp, iterable_ty: &Type, builder: &mut FuncBuilder) -> Temp {
    let len = builder.alloc_temp(Type::Int64);
    if is_union_ty(iterable_ty) {
        builder.emit(Instruction::Call {
            dst: len,
            callee: CallTarget::Named("lin_iterable_length".to_string()),
            args: vec![iterable_temp],
            ret_ty: Type::Int64,
        });
    } else {
        builder.emit(Instruction::CallIntrinsic {
            dst: len,
            intrinsic: Intrinsic::Length,
            args: vec![iterable_temp],
            ret_ty: Type::Int64,
        });
    }
    len
}

/// True when `expr` provably produces a FLAT-buffer array for its (flat-scalar) element type.
fn is_provably_flat_producer(expr: &TypedExpr, builder: &FuncBuilder, ctx: &LowerCtx) -> bool {
    match expr {
        // A non-empty scalar array literal lowers to a flat MakeArray.
        TypedExpr::MakeArray { elements, .. } => !elements.is_empty(),
        // A call to a flat-producing builtin/combinator: range/rangeStep/iterOf (Iterator builtins),
        // map/filter (their lowering allocates a flat output for a flat-scalar element), and the
        // flat array allocators. Recognised by the callee slot resolving to one of those intrinsics
        // OR a stdlib export of that name (the importer's `import_fn_slots`).
        TypedExpr::Call { func, .. } => {
            if let TypedExpr::LocalGet { slot, .. } = func.as_ref() {
                if let Some(intr) = builder.intrinsic_slots.get(slot) {
                    return is_flat_producer_name(intr);
                }
                if let Some((sym, _)) = ctx.import_fn_slots.get(slot) {
                    // Imported export symbol is `{module_key}_{name}`; match on the trailing name.
                    return sym.rsplit('_').next().map(is_flat_producer_export).unwrap_or(false);
                }
                if let Some(&fid) = ctx.global_fn_slots.get(slot) {
                    let _ = fid; // local sibling: not a flat producer we trust here.
                }
            }
            false
        }
        _ => false,
    }
}

/// Intrinsic names whose IR lowering allocates a FLAT scalar buffer (so a flat read on the result
/// is sound). `lin_map`/`lin_filter` allocate flat output for a flat-scalar element type.
fn is_flat_producer_name(name: &str) -> bool {
    matches!(name,
        "lin_range" | "lin_map" | "lin_filter"
            | "lin_array_allocate" | "lin_array_allocate_filled")
}

/// Stdlib export names that thinly wrap a flat producer (`range`→`lin_range`, `map`/`filter`, the
/// flat array allocators). Used when a combinator result reaches another combinator via the
/// importer's Named call (`import_fn_slots`) rather than an inlined intrinsic.
fn is_flat_producer_export(name: &str) -> bool {
    matches!(name,
        "range" | "map" | "filter" | "arrayAllocate" | "arrayAllocateFilled")
}


/// A capture-less literal lambda usable for INLINING into a combinator loop (ADR-044): a
/// `TypedExpr::Function` with no captures. Returns its params + body so the caller can bind each
/// param to a loop temp and lower the body inline — no closure alloc, no boxed indirect call. A
/// capturing lambda or a non-literal callback (a stored/passed `Function` value) returns `None` and
/// the caller falls back to the closure-call path.
fn inlinable_lambda(expr: &TypedExpr) -> Option<(&[TypedParam], &TypedExpr)> {
    match expr {
        TypedExpr::Function { params, body, captures, .. } if captures.is_empty() => {
            Some((params, body))
        }
        _ => None,
    }
}

/// Inline a capture-less lambda's body into the current block: bind each param slot to the
/// corresponding argument temp (coerced to the param's declared representation), then lower the body
/// inline. Returns the body's result temp (typed as the body's lowered type). Used by the combinator
/// inliner to splice `x => x*2` etc. directly into the loop with no boxing/closure call.
///
/// The param bindings are made in a fresh scope so the body's own `val`/locals don't leak; arguments
/// are bound BEFORE the scope is pushed-over (they are the loop's element/accumulator temps, owned by
/// the loop, not by this body scope — so the scope-exit release must not free them). We bind the raw
/// temps directly without registering them owned in the body scope.
fn inline_lambda_body(
    params: &[TypedParam],
    body: &TypedExpr,
    arg_temps: &[(Temp, Type)],
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> (Temp, Type) {
    builder.push_scope();
    for (i, param) in params.iter().enumerate() {
        if let Some((t, arg_ty)) = arg_temps.get(i) {
            // Coerce the argument to the param's declared representation (e.g. unbox a Json
            // element into a concrete scalar param, or box a scalar into a Json param). For the
            // common monomorphic-scalar case the representations already match and this is a no-op.
            let bound = coerce_arg_to_param_repr(*t, arg_ty, &param.ty, builder);
            builder.slots.insert(param.slot, bound);
        }
    }
    let raw = lower_expr(body, builder, ctx);
    let body_ty = builder.temp_types.get(&raw).cloned().unwrap_or_else(|| body.ty());
    // Release this body scope's own locals, KEEPING the result temp. The bound param temps were
    // never registered owned here (they belong to the loop), so they are not double-released.
    builder.pop_scope_releasing_keep(&[raw]);
    (raw, body_ty)
}

/// Coerce `arg` (typed `arg_ty`) to the representation of `param_ty`: box a concrete value into a
/// union/Json param, or unbox a union value into a concrete param; pass through when the
/// representations already match. (A two-directional companion to `coerce_arg_to_param`, which only
/// boxes — the inliner can also need to UNBOX, e.g. a Json element bound to a concrete scalar param.)
fn coerce_arg_to_param_repr(arg: Temp, arg_ty: &Type, param_ty: &Type, builder: &mut FuncBuilder) -> Temp {
    if !type_repr_differs(arg_ty, param_ty) {
        return arg;
    }
    let dst = builder.alloc_temp(param_ty.clone());
    builder.emit(Instruction::Coerce {
        dst, src: arg, from_ty: arg_ty.clone(), to_ty: param_ty.clone(),
    });
    dst
}

/// Call a body closure temp with arguments, coercing each argument to the closure's
/// declared parameter type (e.g. box a concrete element to Json when the callback param
/// is Json) so the closure ABI lines up. Returns the result temp typed as the closure's
/// declared return type.
fn call_body_closure(body: Temp, raw_args: &[(Temp, Type)], param_tys: &[Type], ret_ty: &Type, builder: &mut FuncBuilder) -> Temp {
    // TRUNCATE to the closure's REAL arity: a combinator may offer MORE arguments (e.g. the
    // trailing 0-based index) than the callback declares. A closure's compiled wrapper has exactly
    // `param_tys.len()` parameters, so calling it with surplus args would be ABI UB — drop them.
    // (The checker normally PADS a shorter callback to the expected arity, but this stays correct
    // even for a callback value that reached here un-padded.)
    let n = param_tys.len();
    let call_args: Vec<Temp> = raw_args
        .iter()
        .take(n)
        .enumerate()
        .map(|(i, (t, ty))| {
            let pty = param_tys.get(i);
            coerce_arg_to_param(*t, ty, pty, builder)
        })
        .collect();
    let dst = builder.alloc_temp(ret_ty.clone());
    builder.emit(Instruction::Call {
        dst,
        callee: CallTarget::Indirect(body),
        args: call_args,
        ret_ty: ret_ty.clone(),
    });
    dst
}

/// Like `call_body_closure`, but also returns each argument that is passed to the callback as
/// a boxed `TaggedVal*` (a union/Json value): the per-iteration ELEMENT BOX. This is either the
/// fresh box from `lin_array_get_tagged` (the array is statically Json, e.g. the stdlib `for`
/// wrapper) or a fresh `box_to_json` of a concrete element. Used ONLY by `for`/`while` to
/// reclaim that box's 16-byte SHELL via `FreeBoxShell` (`lin_tagged_free_box`).
///
/// SAFETY: `FreeBoxShell` frees only the box shell (NOT its inner heap payload), and is a no-op
/// on cached small-int/bool boxes and non-pointer args. The element box is ALWAYS a freshly
/// allocated, unshared shell (`lin_array_get_tagged` always allocs; `box_to_json` allocs or
/// returns an immutable cache), so freeing the shell can never double-free or corrupt — even if
/// the callback MOVED the inner into a result via `push`/`set` (those move the inner and leave
/// the shell behind; the inner stays owned by the result). For scalar elements (no inner) this
/// reclaims the whole box — the ~36 B/iter `range(...).for(...)` leak. For heap-inner elements
/// it reclaims the shell and leaves the inner's existing ownership untouched (the residual inner
/// leak is unchanged from before — provably reclaiming it needs the runtime move-vs-retain
/// conventions to change, out of scope). `map`/`filter`/`reduce` use the plain
/// `call_body_closure` and never reach this path, so their element-into-result moves are intact.
fn call_body_closure_with_elem_boxes(body: Temp, raw_args: &[(Temp, Type)], param_tys: &[Type], ret_ty: &Type, builder: &mut FuncBuilder) -> (Temp, Vec<Temp>) {
    // TRUNCATE to the closure's REAL arity (see `call_body_closure`): never pass surplus
    // combinator args (e.g. the trailing index) to a callback that declares fewer parameters.
    let n = param_tys.len();
    let mut elem_boxes = Vec::new();
    let call_args: Vec<Temp> = raw_args
        .iter()
        .take(n)
        .enumerate()
        .map(|(i, (t, ty))| {
            let pty = param_tys.get(i);
            let arg = coerce_arg_to_param(*t, ty, pty, builder);
            // The callback receives a boxed `TaggedVal*` element exactly when the parameter is a
            // union (the element arrived already-union from `lin_array_get_tagged`, or was boxed
            // from a concrete scalar by `coerce_arg_to_param`). Concrete-param callbacks get a raw
            // scalar — nothing to free.
            let boxed = matches!(pty, Some(p) if is_union_ty(p)) || is_union_ty(ty);
            if boxed {
                elem_boxes.push(arg);
            }
            arg
        })
        .collect();
    let dst = builder.alloc_temp(ret_ty.clone());
    builder.emit(Instruction::Call {
        dst,
        callee: CallTarget::Indirect(body),
        args: call_args,
        ret_ty: ret_ty.clone(),
    });
    (dst, elem_boxes)
}

/// Narrow the `Int64` combinator loop counter to the `Int32` the callback's index parameter
/// expects. `emit_index_loop` carries `i` as `Int64`, but the optional iterator-callback index
/// param is `Int32` (checker + intrinsic signatures), so an explicit truncation is emitted here
/// via `Coerce` (codegen lowers Int64→Int32 as a `trunc`). Returns a fresh `Int32` temp.
fn narrow_loop_index(i: Temp, builder: &mut FuncBuilder) -> Temp {
    let dst = builder.alloc_temp(Type::Int32);
    builder.emit(Instruction::Coerce {
        dst, src: i, from_ty: Type::Int64, to_ty: Type::Int32,
    });
    dst
}

/// Coerce a concrete argument to a union/Json parameter (box it); pass through otherwise.
fn coerce_arg_to_param(arg: Temp, arg_ty: &Type, param_ty: Option<&Type>, builder: &mut FuncBuilder) -> Temp {
    match param_ty {
        Some(pty) if is_union_ty(pty) && !is_union_ty(arg_ty) => box_to_json(arg, arg_ty, builder),
        _ => arg,
    }
}

/// Allocate an output array whose storage matches `elem_ty`: a flat scalar array for
/// Int32/Int64/Float32/Float64, otherwise a tagged array. Returns (array_temp, is_flat).
fn alloc_output_array(elem_ty: &Type, result_type: &Type, builder: &mut FuncBuilder) -> (Temp, Option<FlatElemKind>) {
    let flat = FlatElemKind::from_type(elem_ty);
    let out = builder.alloc_temp(result_type.clone());
    let intrinsic = match flat {
        Some(kind) => Intrinsic::FlatArrayAlloc(kind),
        None => Intrinsic::ArrayAlloc,
    };
    builder.emit(Instruction::CallIntrinsic {
        dst: out, intrinsic, args: vec![], ret_ty: result_type.clone(),
    });
    builder.register_owned(out, result_type.clone());
    (out, flat)
}

/// Push `val` (typed `val_ty`) into an output array allocated by `alloc_output_array`.
/// Flat arrays take the raw scalar; tagged arrays take a Json-boxed value.
///
/// `borrowed` records whether `val` is a value this combinator BORROWS rather than freshly owns:
/// `filter` pushes the very element it read from the SOURCE array (still owned by the source), so
/// the result must take its OWN reference; `map` pushes the lambda's fresh result (+1 it owns), so
/// the push MOVES it. The tagged push of a CONCRETE-rc element (`Push` → `lin_array_push_tagged`)
/// raw-copies the TaggedVal WITHOUT bumping the inner refcount (move semantics), so a borrowed
/// concrete element must be `Retain`ed first — otherwise both the source array and the result array
/// reference the same object at refcount 1, and releasing both double-frees it (the `filter` over an
/// object array UAF; ADR-044 R2). A UNION element pushes via the retaining `lin_push_dyn`, and a
/// flat-scalar element carries no refcount, so neither needs the extra retain.
fn push_output(out: Temp, flat: Option<FlatElemKind>, elem_ty: &Type, val: Temp, val_ty: &Type, borrowed: bool, builder: &mut FuncBuilder) {
    let push_dst = builder.alloc_temp(Type::Null);
    match flat {
        Some(kind) => {
            // Flat arrays store raw scalars; unbox the value if it arrived boxed (Json).
            let scalar = if is_union_ty(val_ty) {
                let dst = builder.alloc_temp(elem_ty.clone());
                builder.emit(Instruction::Coerce {
                    dst, src: val, from_ty: val_ty.clone(), to_ty: elem_ty.clone(),
                });
                dst
            } else {
                val
            };
            builder.emit(Instruction::CallIntrinsic {
                dst: push_dst, intrinsic: Intrinsic::FlatArrayPush(kind), args: vec![out, scalar], ret_ty: Type::Null,
            });
        }
        None => {
            // A borrowed CONCRETE-rc element pushed into a tagged result array via the MOVE
            // intrinsic (`lin_array_push_tagged`, which does NOT bump the inner refcount) must be
            // retained so the result array owns its own reference (else double-free with the
            // source array at teardown — the `filter`-over-object-array UAF). Union elements push
            // via the retaining `lin_push_dyn`; flat scalars carry no refcount — neither needs this.
            if borrowed && is_rc_type(val_ty) && !is_union_ty(val_ty) {
                builder.emit(Instruction::Retain { val, ty: val_ty.clone() });
            }
            let boxed = box_to_json(val, val_ty, builder);
            builder.emit(Instruction::CallIntrinsic {
                dst: push_dst, intrinsic: Intrinsic::Push, args: vec![out, boxed], ret_ty: Type::Null,
            });
            // `Push` (`lin_array_push_tagged`) MOVES the box's inner into the result slot without
            // retaining. When `box_to_json` made a FRESH box for a concrete `val` (val_ty not a
            // union), its 16-byte shell is now orphaned — the inner lives on in the array. Reclaim
            // the shell (the inner is NOT touched — that pointer belongs to the array). Skipped when
            // `boxed == val` (val was already a union; that box is owned/freed elsewhere — e.g. the
            // map elem box freed by `free_combinator_elem_box`). Without this a concrete-producing
            // `map(src, x => {…})` leaked the per-element push box shell (~16 B/elem).
            if boxed != val {
                builder.emit(Instruction::FreeBoxShell { val: boxed });
            }
        }
    }
}

/// True when two types have a different runtime representation such that a value of one
/// must be coerced (boxed/unboxed) to be used as the other. Specifically: one is a
/// union/Json (TaggedVal*) and the other is a concrete type.
fn type_repr_differs(from: &Type, to: &Type) -> bool {
    // A sealed scalar record flowing into (or out of) a `Named` type reference: same physical
    // representation (an opaque struct ptr), no conversion. `Named` is treated as union-ish by
    // `is_union_ty` (recursive types are boxed), which would otherwise box/materialize the sealed
    // struct here — see the matching guard + rationale in `lower_coerce_arg`. For Stage 1 a
    // `Named` that a sealed value is compatible with resolves to that sealed Object.
    if (is_sealed_scalar_repr(from) && matches!(to, Type::Named(_)))
        || (is_sealed_scalar_repr(to) && matches!(from, Type::Named(_)))
    {
        return false;
    }
    // UNBOXED SUM TYPE (unboxed-sumtype Stage 1): a value whose type is a Stage-1-eligible sum type
    // is physically a `SumNode*` (the seed). Crossing into a NON-sum slot (Json wildcard, a
    // differently-shaped union, an unsealed object, a concrete variant record) is a representation
    // change — the codegen Coerce materializes/projects the node. The reverse (a Json/boxed/variant
    // source into a sum slot) is likewise a change. When BOTH sides are the SAME sum type there is no
    // change (the SumNode pointer carries verbatim — `from == to` short-circuits via the equal-type
    // checks the callers already do; here we only fire on a genuine mismatch).
    // A sum value flowing into (or out of) a `Named` type reference is the SELF-RECURSIVE-call case
    // (the callee reads the SAME `Named` param consistently as a SumNode — its body's match/FieldGet
    // resolve `Named` to the sum Union). Same physical representation (a SumNode pointer), no
    // conversion — mirrors the sealed-scalar-record `Named` pass-through guard above. Without this,
    // `eligible(sum)=true != eligible(Named)=false` would wrongly materialize the node at the
    // recursive call (the recursive sum-param crash).
    if (crate::repr::sum_type_eligible(from) && matches!(to, Type::Named(_)))
        || (crate::repr::sum_type_eligible(to) && matches!(from, Type::Named(_)))
    {
        return false;
    }
    if crate::repr::sum_type_eligible(from) != crate::repr::sum_type_eligible(to) {
        return true;
    }
    // The union/Json box boundary.
    if is_union_ty(from) != is_union_ty(to) {
        return true;
    }
    // The sealed scalar-record boundary (sealed-records Stage 1). A sealed scalar struct and a
    // boxed `LinObject` (an unsealed object literal, or — via the union arm above already — a
    // Json value) are physically DIFFERENT representations even though `Type`'s PartialEq treats
    // them as structurally equal (it ignores `sealed`). So whenever exactly one side is a sealed
    // scalar record, a `Coerce` must be inserted: codegen's `compile_ir_coerce` then PROJECTS a
    // boxed source into a fresh sealed struct (Object→sealed) or MATERIALIZES a sealed struct into
    // a boxed LinObject (sealed→Object). When BOTH sides are the same sealed scalar record there
    // is no repr difference (no coercion). (sealed↔union is already covered by the union arm.)
    if is_sealed_scalar_repr(from) != is_sealed_scalar_repr(to) {
        return true;
    }
    // Both sealed scalar records but a DIFFERENT field layout (e.g. a wider sealed type projected
    // to a narrower one): their physical layouts differ, so re-project. `Type` PartialEq compares
    // the field maps, so `from != to` here means a different shape.
    if is_sealed_scalar_repr(from) && is_sealed_scalar_repr(to) && from != to {
        return true;
    }
    false
}

/// True when two CONCRETE numeric types have a different unboxed machine representation, so a
/// value of one must be converted (sitofp/fptrunc/sext/...) to be stored as the other. This is
/// distinct from `type_repr_differs`, which only covers the union/Json box boundary.
///
/// The trigger case is a mixed numeric array literal like `[0, 3.14]`: the checker unifies the
/// element type to Float64 (so the array uses the flat f64 scalar repr), but each integer
/// literal element keeps its own Int32 type. Without this conversion the i32 element flowed
/// straight into `lin_flat_array_push_f64`, producing an i32-arg-to-f64-param type mismatch in
/// the emitted IR. Reusing the existing `Coerce` instruction lets codegen's `compile_ir_coerce`
/// emit the proper int→float / float-width conversion at the push site.
fn scalar_numeric_repr_differs(from: &Type, to: &Type) -> bool {
    if !from.is_numeric() || !to.is_numeric() {
        return false;
    }
    // Int vs float: representation differs. Float vs float: differs only across widths
    // (Float32 vs Float64). Int vs int width changes are handled at their own use sites and a
    // flat-int array's element type matches its members, so don't widen those here.
    from.is_float() != to.is_float() || (from.is_float() && to.is_float() && from != to)
}

/// True when `from` and `to` are FLAT scalar arrays with DIFFERENT element types, so a value of one
/// must be CONVERTED (a fresh, dest-strided buffer with each element widened) to be used as the
/// other — NOT a pointer reinterpret. A flat scalar array stores its elements at the element type's
/// native stride and tags the buffer with that element kind, so binding e.g. a `UInt8[]` value to an
/// `Int32[]` slot and reinterpreting the pointer would read 4 source bytes as one i32 on every
/// indexed access (the whole-array `toString` reads the runtime `elem_tag` and so looked correct,
/// but `arr[0]` uses the static dest stride and did not). Codegen's `compile_ir_coerce` materializes
/// the converted buffer (`flat_array_widen`). This is the whole-array analogue of
/// `scalar_numeric_repr_differs` (the mixed-numeric array LITERAL element case).
fn flat_scalar_array_repr_differs(from: &Type, to: &Type) -> bool {
    if let (Type::Array(fe), Type::Array(te)) = (from, to) {
        return fe.is_flat_scalar() && te.is_flat_scalar() && fe != te;
    }
    false
}

/// Box a value to Json (TaggedVal*) if it is a concrete (non-union) type.
/// `fromJson` decode (ADR-031). Lower the Json value, box it to the tagged representation if
/// concrete, then emit `CallIntrinsic { FromJson(target) }`. The runtime borrows the input and
/// returns either the SAME pointer retained (+1) on success or a fresh `Error` object — so the
/// result is unconditionally +1 owned (register_owned), and the input keeps its own ownership
/// (released later by normal liveness). `result_type` is `T | Error` (a boxed union), so the
/// result temp is treated as a union box.
fn lower_from_json(
    target: &Type,
    value: &TypedExpr,
    result_type: &Type,
    named_defs: &[(String, Type)],
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Temp {
    let value_temp = lower_expr(value, builder, ctx);
    // The runtime walker expects a TaggedVal*; box concrete scalars/strings to Json.
    let boxed = box_to_json(value_temp, &value.ty(), builder);
    let dst = builder.alloc_temp(result_type.clone());
    builder.emit(Instruction::CallIntrinsic {
        dst,
        intrinsic: Intrinsic::FromJson {
            target: Box::new(target.clone()),
            named_defs: named_defs.to_vec(),
        },
        args: vec![boxed],
        ret_ty: result_type.clone(),
    });
    builder.register_owned(dst, result_type.clone());
    dst
}

fn box_to_json(val: Temp, val_ty: &Type, builder: &mut FuncBuilder) -> Temp {
    if is_union_ty(val_ty) {
        return val;
    }
    let json = Type::TypeVar(u32::MAX);
    let dst = builder.alloc_temp(json.clone());
    builder.emit(Instruction::Coerce {
        dst, src: val, from_ty: val_ty.clone(), to_ty: json,
    });
    dst
}

/// `range(start, end)` → a flat Int32 array [start, start+1, ..., end-1].
/// Lowered as: alloc flat array, then a fill loop pushing each value.
fn lower_range(args: &[TypedExpr], builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    let start_raw = lower_expr(&args[0], builder, ctx);
    let end_raw = lower_expr(&args[1], builder, ctx);
    // `lin_range` drives a NATIVE i32 loop counter, so its bounds must be concrete i32. A bound
    // whose static type is the dynamic `Json` wildcard (e.g. `i * s` where `i` is a `Json`-typed
    // `for`-lambda param, or arithmetic over a value read out of a Json array) arrives BOXED —
    // unbox it to Int32 here, else the boxed `TaggedVal*` flows into the i32 counter phi (a
    // representation mismatch the verifier rejects). `coerce_to_slot_type` is a no-op when the
    // bound is already a concrete int.
    let start = coerce_to_slot_type(start_raw, &args[0].ty(), &Type::Int32, builder);
    let end = coerce_to_slot_type(end_raw, &args[1].ty(), &Type::Int32, builder);

    // arr = arrayAllocate-style empty flat i32 array (capacity grows via push).
    let arr_ty = Type::Array(Box::new(Type::Int32));
    let arr = builder.alloc_temp(arr_ty.clone());
    builder.emit(Instruction::CallIntrinsic {
        dst: arr,
        intrinsic: Intrinsic::FlatArrayAlloc(FlatElemKind::I32),
        args: vec![],
        ret_ty: arr_ty.clone(),
    });
    builder.register_owned(arr, arr_ty.clone());

    let preheader = builder.current_block;
    let header = builder.alloc_block("range_header");
    let body = builder.alloc_block("range_body");
    let exit = builder.alloc_block("range_exit");

    // i phi node: [start, preheader], [i_next, body].
    let i = builder.alloc_temp(Type::Int32);
    builder.terminate(Terminator::Jump(header));

    builder.switch_to(header);
    // Placeholder phi; incomings filled below once i_next exists.
    let i_next = builder.alloc_temp(Type::Int32);
    builder.emit(Instruction::Phi {
        dst: i,
        ty: Type::Int32,
        incomings: vec![(start, preheader), (i_next, body)],
    });
    let cond = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::Binary {
        dst: cond, op: BinOp::Lt, lhs: i, rhs: end,
        operand_ty: Type::Int32, ty: Type::Bool,
    });
    builder.terminate(Terminator::CondJump { cond, then_block: body, else_block: exit });

    builder.switch_to(body);
    // arr.push(i)
    let push_dst = builder.alloc_temp(Type::Null);
    builder.emit(Instruction::CallIntrinsic {
        dst: push_dst,
        intrinsic: Intrinsic::FlatArrayPush(FlatElemKind::I32),
        args: vec![arr, i],
        ret_ty: Type::Null,
    });
    let one = builder.const_temp(Const::Int(1, Type::Int32));
    builder.emit(Instruction::Binary {
        dst: i_next, op: BinOp::Add, lhs: i, rhs: one,
        operand_ty: Type::Int32, ty: Type::Int32,
    });
    builder.terminate(Terminator::Jump(header));

    builder.switch_to(exit);
    arr
}

/// `iter(init, cond, next, current)` → eagerly build a Json array by looping:
/// `s = init(); while cond(s) { push(current(s)); s = next(s) }`. The four callbacks are
/// closures (uniform boxed ABI), so the state is carried as Json.
fn lower_iter(args: &[TypedExpr], result_type: &Type, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    let json = Type::TypeVar(u32::MAX);
    let init = lower_expr(&args[0], builder, ctx);
    let cond = lower_expr(&args[1], builder, ctx);
    let next = lower_expr(&args[2], builder, ctx);
    let current = lower_expr(&args[3], builder, ctx);

    // Output is a tagged Json array (elements boxed).
    let out = builder.alloc_temp(result_type.clone());
    builder.emit(Instruction::CallIntrinsic {
        dst: out, intrinsic: Intrinsic::ArrayAlloc, args: vec![], ret_ty: result_type.clone(),
    });
    builder.register_owned(out, result_type.clone());

    // s0 = init()
    let s0 = builder.alloc_temp(json.clone());
    builder.emit(Instruction::Call {
        dst: s0, callee: CallTarget::Indirect(init), args: vec![], ret_ty: json.clone(),
    });

    let preheader = builder.current_block;
    let header = builder.alloc_block("iter_header");
    let body = builder.alloc_block("iter_body");
    let exit = builder.alloc_block("iter_exit");

    let state = builder.alloc_temp(json.clone());
    let state_next = builder.alloc_temp(json.clone());
    builder.terminate(Terminator::Jump(header));

    builder.switch_to(header);
    builder.emit(Instruction::Phi {
        dst: state, ty: json.clone(), incomings: vec![(s0, preheader), (state_next, body)],
    });
    // keep = cond(state) : Bool
    let keep = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::Call {
        dst: keep, callee: CallTarget::Indirect(cond), args: vec![state], ret_ty: Type::Bool,
    });
    builder.terminate(Terminator::CondJump { cond: keep, then_block: body, else_block: exit });

    builder.switch_to(body);
    // push(out, current(state))
    let cur = builder.alloc_temp(json.clone());
    builder.emit(Instruction::Call {
        dst: cur, callee: CallTarget::Indirect(current), args: vec![state], ret_ty: json.clone(),
    });
    let push_dst = builder.alloc_temp(Type::Null);
    builder.emit(Instruction::CallIntrinsic {
        dst: push_dst, intrinsic: Intrinsic::Push, args: vec![out, cur], ret_ty: Type::Null,
    });
    // state_next = next(state)
    builder.emit(Instruction::Call {
        dst: state_next, callee: CallTarget::Indirect(next), args: vec![state], ret_ty: json.clone(),
    });
    builder.terminate(Terminator::Jump(header));

    builder.switch_to(exit);
    out
}

/// Emit the standard index-loop scaffold over `iterable` (length-bounded), invoking
/// `body_fn(i, elem)` to build the loop body. `body_fn` runs with the builder positioned
/// in the body block, receiving the current index temp and the loaded element temp; after
/// it returns, the increment + back-edge are emitted. Leaves the builder in the exit block.
fn emit_index_loop<F: FnOnce(Temp, Temp, &mut FuncBuilder, &mut LowerCtx)>(
    iterable: Temp,
    iterable_ty: &Type,
    elem_ty: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    body_fn: F,
) {
    let elem_ty = elem_ty.clone();

    // len = length(iterable) — tag-checked (0 for a non-array Json) when the iterable is union.
    let len = emit_iterable_len(iterable, iterable_ty, builder);
    let zero = builder.const_temp(Const::Int(0, Type::Int64));

    let preheader = builder.current_block;
    let header = builder.alloc_block("for_header");
    let body = builder.alloc_block("for_body");
    let exit = builder.alloc_block("for_exit");

    let i = builder.alloc_temp(Type::Int64);
    let i_next = builder.alloc_temp(Type::Int64);
    builder.terminate(Terminator::Jump(header));

    builder.switch_to(header);
    builder.emit(Instruction::Phi {
        dst: i, ty: Type::Int64,
        incomings: vec![(zero, preheader), (i_next, body)],
    });
    let cond = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::Binary {
        dst: cond, op: BinOp::Lt, lhs: i, rhs: len,
        operand_ty: Type::Int64, ty: Type::Bool,
    });
    builder.terminate(Terminator::CondJump { cond, then_block: body, else_block: exit });

    builder.switch_to(body);
    // elem = iterable[i]
    let elem = builder.alloc_temp(elem_ty.clone());
    builder.emit(Instruction::Index {
        dst: elem, object: iterable, key: i,
        obj_ty: iterable_ty.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone(),
    });
    body_fn(i, elem, builder, ctx);
    // NOTE: the `Index` op (`lin_array_get_tagged`) allocates a fresh 16-byte `TaggedVal*` shell
    // for a union/Json `elem` each iteration; this shell leaks (a residual, distinct from the
    // for-callback-return leak fixed here). It is NOT reclaimed because the runtime's
    // `lin_array_push_tagged`/`lin_array_set` MOVE an element's inner into result arrays WITHOUT
    // retaining, so the element box's inner ownership is consumed unpredictably by the body —
    // neither a tag-aware release nor a shell-only free is provably safe (both double-free
    // `map`/`minBy`/`maxBy`, which move elements into result/accumulator arrays). Reclaiming it
    // safely needs a change to those runtime move-vs-retain conventions, out of scope here.
    // `body_fn` may have switched basic blocks (e.g. filter's keep/skip split). The increment +
    // back-edge are emitted in whatever block is now current, and the header phi's back-edge
    // predecessor is patched to that block (it was provisionally recorded as `body`).
    let back_block = builder.current_block;
    let one = builder.const_temp(Const::Int(1, Type::Int64));
    builder.emit(Instruction::Binary {
        dst: i_next, op: BinOp::Add, lhs: i, rhs: one,
        operand_ty: Type::Int64, ty: Type::Int64,
    });
    builder.terminate(Terminator::Jump(header));
    builder.patch_phi_incoming(header, i, body, back_block);

    builder.switch_to(exit);
}

/// `for(iterable, body)` → index loop calling `body(elem)` for side effects; returns Null.
fn lower_for(args: &[TypedExpr], builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    let iterable_ty = args[0].ty();
    // STREAM `.for(fn)` (Stage 5): a `for` over a Stream is driven by the runtime, not the
    // index-loop. EOF ends the loop normally (→ Null); the first read Error becomes the for-expr's
    // value (→ Error). So a stream `for` has result type `Null | Error`. The body closure ESCAPES
    // into the runtime call, so it is lowered as an ordinary (non-safe-ctx) closure value.
    if matches!(iterable_ty, Type::Stream(_)) {
        let stream = lower_expr(&args[0], builder, ctx);
        let body = lower_expr(&args[1], builder, ctx);
        let ret_ty = Type::Union(vec![Type::Null, lin_check::resolve::error_type()]);
        let dst = builder.alloc_temp(ret_ty.clone());
        builder.emit(Instruction::CallIntrinsic {
            dst,
            intrinsic: Intrinsic::StreamFor,
            args: vec![stream, body],
            ret_ty: ret_ty.clone(),
        });
        builder.register_owned(dst, ret_ty);
        return dst;
    }
    let (param_tys, _) = callback_signature(&args[1]);
    // Read elements at the source's PROVABLE runtime representation: flat-scalar only when the
    // source is a provably-flat producer, else the tagged Json read (sound for a `[]`+push array
    // mistyped as flat). See `combinator_read_elem_ty` (ADR-044).
    let read_elem_ty = combinator_read_elem_ty(&args[0], builder, ctx);
    let iterable = lower_expr(&args[0], builder, ctx);
    let body = lower_callback_in_safe_ctx(&args[1], builder, ctx);
    let elem_ty = read_elem_ty.clone();
    // The callback closure uses the uniform BOXED ABI: it ALWAYS returns a freshly-allocated,
    // independently-owned `TaggedVal*` (e.g. `lin_box_null()` for a void-ish body, `lin_box_int`
    // for an int result, or — for an assignment body like `acc = concat(...)` — its own owned +1
    // ref, now distinct from the cell/global's shell thanks to the clone-on-store above). `for`
    // discards that value, so we MUST call with a union ret_ty (forcing codegen to emit a
    // `call ptr` rather than a `call void` that silently drops the returned box) and then
    // tag-aware release it every iteration, inside the loop body before the back-edge — never
    // registered as scope-owned (that would release once AFTER the loop, leaking per-iteration).
    let boxed = Type::TypeVar(u32::MAX);
    emit_index_loop(iterable, &iterable_ty, &read_elem_ty, builder, ctx, |i, elem, b, _| {
        // The optional 0-based SOURCE index (`(item, i) => …`); narrowed Int64→Int32. Truncated
        // away by `call_body_closure_with_elem_boxes` when the callback declares only `(item)`.
        let idx = narrow_loop_index(i, b);
        let (ret, elem_boxes) = call_body_closure_with_elem_boxes(body, &[(elem, elem_ty.clone()), (idx, Type::Int32)], &param_tys, &boxed, b);
        // Release the callback-RETURN box (a fresh, independently-owned +1; `for` discards it).
        // This fully reclaims it (inner + shell). The callback CAN return (an alias of) the
        // element box — e.g. `x => x`, or `acc = f(acc, x)` where `f` yields its element — in which
        // case `ret` IS the element box and this single release already reclaimed it.
        b.emit(Instruction::Release { val: ret, ty: boxed.clone() });
        // Reclaim the per-iteration element BOX SHELL — but ONLY when it is DISTINCT from `ret`
        // (the release above already reclaimed it otherwise; a second free would double-free).
        // `lin_tagged_free_box_if_distinct` frees only the 16-byte shell (cached- and
        // non-pointer-safe), never the inner payload — so it is safe for both flat (scalar, no
        // inner: full reclaim — the ~36 B/iter leak) and tagged (heap inner stays owned by the
        // source array / wherever the body moved it) element boxes. for/while-only reclaim;
        // map/filter/reduce use the plain `call_body_closure` and never reach this path.
        for ebox in &elem_boxes {
            b.emit(Instruction::FreeBoxShellIfDistinct { val: *ebox, other: ret });
        }
        // A PACKED sealed-array source materialized a fresh +1 element struct each iteration; `for`
        // discards it (a side-effecting body never moves the struct out), so release it or it leaks.
        free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, b);
    });
    builder.const_temp(Const::Null)
}

/// `while(iterable, body)` → like `for`, but stops early when `body(elem)` returns false.
fn lower_while(args: &[TypedExpr], builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    let iterable_ty = args[0].ty();
    let (param_tys, _) = callback_signature(&args[1]);
    // Read at the source's PROVABLE representation (ADR-044): tagged Json read unless provably flat,
    // so a `[]`+push array mistyped as a flat `T[]` is read correctly (not as raw flat scalars).
    let read_elem_ty = combinator_read_elem_ty(&args[0], builder, ctx);
    let iterable = lower_expr(&args[0], builder, ctx);
    let body = lower_callback_in_safe_ctx(&args[1], builder, ctx);

    let elem_ty = read_elem_ty;
    // Tag-checked length (0 for a non-array Json) when the iterable is union. See emit_iterable_len.
    let len = emit_iterable_len(iterable, &iterable_ty, builder);
    let zero = builder.const_temp(Const::Int(0, Type::Int64));

    let preheader = builder.current_block;
    let header = builder.alloc_block("while_header");
    let body_block = builder.alloc_block("while_body");
    let cont_block = builder.alloc_block("while_cont");
    let exit = builder.alloc_block("while_exit");

    let i = builder.alloc_temp(Type::Int64);
    let i_next = builder.alloc_temp(Type::Int64);
    builder.terminate(Terminator::Jump(header));

    builder.switch_to(header);
    builder.emit(Instruction::Phi {
        dst: i, ty: Type::Int64,
        incomings: vec![(zero, preheader), (i_next, cont_block)],
    });
    let cond = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::Binary {
        dst: cond, op: BinOp::Lt, lhs: i, rhs: len, operand_ty: Type::Int64, ty: Type::Bool,
    });
    builder.terminate(Terminator::CondJump { cond, then_block: body_block, else_block: exit });

    builder.switch_to(body_block);
    let elem = builder.alloc_temp(elem_ty.clone());
    builder.emit(Instruction::Index {
        dst: elem, object: iterable, key: i,
        obj_ty: iterable_ty.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone(),
    });
    // keep = body(elem, i) : Bool — continue only while true. `i` (the 0-based SOURCE index) is
    // narrowed Int64→Int32; truncated away when the callback declares only `(item)`.
    let idx = narrow_loop_index(i, builder);
    let (keep, elem_boxes) = call_body_closure_with_elem_boxes(body, &[(elem, elem_ty.clone()), (idx, Type::Int32)], &param_tys, &Type::Bool, builder);
    // Reclaim the per-iteration element BOX SHELL (same mechanism + safety as `lower_for`):
    // `FreeBoxShell` frees only the 16-byte shell, never the inner. The predicate's `Bool` return
    // is an unboxed scalar, so it can NEVER alias the element box — no de-aliasing needed here.
    for ebox in &elem_boxes {
        builder.emit(Instruction::FreeBoxShell { val: *ebox });
    }
    // A PACKED sealed-array source materialized a fresh +1 element struct; `while` discards it
    // (the predicate body never moves the struct out), so release it or it leaks per iteration.
    free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, builder);
    builder.terminate(Terminator::CondJump { cond: keep, then_block: cont_block, else_block: exit });

    builder.switch_to(cont_block);
    let one = builder.const_temp(Const::Int(1, Type::Int64));
    builder.emit(Instruction::Binary {
        dst: i_next, op: BinOp::Add, lhs: i, rhs: one, operand_ty: Type::Int64, ty: Type::Int64,
    });
    builder.terminate(Terminator::Jump(header));

    builder.switch_to(exit);
    builder.const_temp(Const::Null)
}

/// Reclaim the 16-byte SHELL of a `map`/`filter` per-element box after the loop body has consumed
/// it (pushed it / built the mapped value from it).
///
/// When the source is a TAGGED (union/Json) array, `emit_index_loop`'s `Index` lowers to
/// `lin_array_get_tagged`, which allocates a FRESH standalone `TaggedVal*` box per element. The
/// result push (`Intrinsic::Push` → `lin_array_push_tagged`) MOVES the box's inner payload into the
/// result slot WITHOUT bumping the inner refcount — so the inner's single +1 is now owned by the
/// result array, and the per-element box's SHELL is orphaned (it leaked ~16 B/elem, ASan-confirmed
/// in `map(src, x => x)` / `filter`).
///
/// We free ONLY the shell (`FreeBoxShell` → `lin_tagged_free_box`), never the inner — the inner
/// pointer belongs to the result array (moved) or, for an identity map over interned/borrowed
/// strings, to the source. A full `lin_tagged_release` here would DOUBLE-FREE the moved inner
/// (the `filter$String`-over-`split` UAF: `lin_array_push_tagged` moves, then a full release of
/// the elem box frees the very string the result array now owns). The shell is always a freshly
/// allocated, unshared 16-byte `TaggedVal*`, so freeing it is sound; it is null/cached-box safe.
///
/// No-op for a flat-scalar element read (no box was allocated — the read produced a raw scalar).
fn free_combinator_elem_box(elem: Temp, elem_ty: &Type, builder: &mut FuncBuilder) {
    // Only a union/Json read allocates a standalone box; flat-scalar reads carry no shell.
    if is_union_ty(elem_ty) {
        builder.emit(Instruction::FreeBoxShell { val: elem });
    }
}

/// FULLY reclaim a `filter` per-element box for an element that is DROPPED (predicate false):
/// the shell AND the inner +1 that `lin_array_get_tagged` retained, since nothing else owns it.
/// `lin_tagged_release` is tag-aware (releases the inner heap value then frees the shell) and
/// null/cached-box safe. Only fires for a union/Json read (a box was allocated). NEVER used on the
/// KEEP path — there the element's inner was moved/retained into the result (see
/// `free_combinator_elem_box`, the shell-only counterpart).
fn free_combinator_elem_box_full(elem: Temp, elem_ty: &Type, builder: &mut FuncBuilder) {
    if is_union_ty(elem_ty) {
        builder.emit(Instruction::Release { val: elem, ty: elem_ty.clone() });
    }
}

/// Reclaim a combinator per-element value when the element TYPE is a SEALED scalar record
/// (`is_sealed_scalar_repr`): the `Index` op materializes a FRESH +1 sealed struct each iteration,
/// from EITHER source representation —
///   - a PACKED sealed-scalar array → `Codegen::sealed_array_materialize_elem` (header + payload
///     copied; for heap-field records — Stage 3b strings — also takes its OWN +1 on each heap field
///     via `retain_sealed_payload_fields`); OR
///   - a BOXED `Object[]` (a `[]`+push array of records that isn't packable, e.g. one with a
///     String/Array/nested-record field) → `lin_array_get` + `unbox_tagged_val_to_type` →
///     `sealed_project_from`, which projects the boxed `LinObject` into a fresh +1 sealed struct
///     (likewise retaining its heap fields).
/// Either way the element is a fully-owned, fully independent value that nothing else aliases. The
/// combinator body CONSUMES it (reads a scalar field for `map`/`reduce`, a predicate for `filter`, a
/// side effect for `for`/`while`) but never moves the STRUCT itself into the result:
///   - a scalar/flat output array copies the read scalar by value (no aliasing);
///   - a boxed `Object[]` output `Coerce`s the struct to a FRESH boxed `LinObject`
///     (`sealed_materialize_to_object`) — again a copy, leaving this struct distinct;
///   - a sealed-scalar output array copies the packed bytes by value.
/// So the materialized struct is always genuinely DROPPED and must be released, else it leaks one
/// fresh sealed struct per element, per combinator call (the `map(ts, x => x["a"])` per-element leak,
/// ASan-confirmed across all sealed field shapes; mirrors `lower_for`'s per-iteration box reclaim).
/// `Release` on a sealed `Object` routes to `lin_sealed_release` (rc-- + heap-field walk on zero), so
/// the struct's own field references are reclaimed and the (separately owned) source/result
/// references are untouched — never a double-free. No-op unless the element is a sealed scalar repr.
fn free_combinator_sealed_elem(elem: Temp, _iterable_ty: &Type, elem_ty: &Type, builder: &mut FuncBuilder) {
    if is_sealed_scalar_repr(elem_ty) {
        builder.emit(Instruction::Release { val: elem, ty: elem_ty.clone() });
    }
}

/// `map(iterable, f)` → new array of `f(elem)` for each element.
fn lower_map(args: &[TypedExpr], result_type: &Type, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    let iterable_ty = args[0].ty();
    let (param_tys, cb_ret) = callback_signature(&args[1]);

    // Output element type per the map's declared result type; storage matches it.
    let out_elem_ty = match result_type {
        Type::Array(t) | Type::Iterator(t) => (**t).clone(),
        _ => Type::TypeVar(u32::MAX),
    };
    // Read at the source's PROVABLE representation: a flat scalar only for a provably-flat producer
    // (range/map/filter result, flat literal), else the tagged Json read — sound for a `[]`+push
    // array mistyped as a flat `T[]` (ADR-044).
    let elem_ty = combinator_read_elem_ty(&args[0], builder, ctx);

    let iterable = lower_expr(&args[0], builder, ctx);

    // INLINE FAST PATH (ADR-044): a capture-less literal lambda is spliced directly into the loop —
    // its param bound to the element temp, its body lowered inline — with no closure alloc and no
    // per-element box/unbox/indirect call.
    if let Some((lam_params, lam_body)) = inlinable_lambda(&args[1]) {
        let lam_params = lam_params.to_vec();
        let lam_body = lam_body.clone();
        let (out, flat) = alloc_output_array(&out_elem_ty, result_type, builder);
        emit_index_loop(iterable, &iterable_ty, &elem_ty, builder, ctx, |i, elem, b, c| {
            // Optional 0-based SOURCE index; narrowed Int64→Int32. `inline_lambda_body` binds by the
            // lambda's OWN param count, so a 1-param `x => …` simply ignores this surplus arg.
            let idx = narrow_loop_index(i, b);
            let (mapped, mapped_ty) =
                inline_lambda_body(&lam_params, &lam_body, &[(elem, elem_ty.clone()), (idx, Type::Int32)], b, c);
            // `mapped` is the lambda's freshly-owned result (+1) — MOVE it into the result array.
            push_output(out, flat, &out_elem_ty, mapped, &mapped_ty, false, b);
            free_combinator_elem_box(elem, &elem_ty, b);
            // A PACKED sealed-array source materialized a fresh +1 element struct; the body read a
            // copy out of it (scalar field / re-boxed object), so release it or it leaks per element.
            free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, b);
        });
        return out;
    }

    let f = lower_callback_in_safe_ctx(&args[1], builder, ctx);
    let (out, flat) = alloc_output_array(&out_elem_ty, result_type, builder);

    emit_index_loop(iterable, &iterable_ty, &elem_ty, builder, ctx, |i, elem, b, _| {
        let idx = narrow_loop_index(i, b);
        let mapped = call_body_closure(f, &[(elem, elem_ty.clone()), (idx, Type::Int32)], &param_tys, &cb_ret, b);
        // `mapped` is the callback's freshly-owned result (+1) — MOVE it into the result array.
        push_output(out, flat, &out_elem_ty, mapped, &cb_ret, false, b);
        free_combinator_elem_box(elem, &elem_ty, b);
        free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, b);
    });
    out
}

/// `filter(iterable, pred)` → new array of elements where `pred(elem)` is true.
fn lower_filter(args: &[TypedExpr], result_type: &Type, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    let iterable_ty = args[0].ty();
    let (param_tys, _) = callback_signature(&args[1]);

    // filter preserves the element type; storage matches it.
    let out_elem_ty = match result_type {
        Type::Array(t) | Type::Iterator(t) => (**t).clone(),
        _ => Type::TypeVar(u32::MAX),
    };
    // Read at the source's PROVABLE representation (ADR-044): flat scalar for a provably-flat
    // producer, else tagged Json (sound for a `[]`+push array mistyped as flat).
    let elem_ty = combinator_read_elem_ty(&args[0], builder, ctx);

    let iterable = lower_expr(&args[0], builder, ctx);

    // INLINE FAST PATH (ADR-044): a capture-less literal predicate lambda is spliced into the loop;
    // its body's Bool result drives the keep/skip split directly — no closure, no boxed call.
    if let Some((lam_params, lam_body)) = inlinable_lambda(&args[1]) {
        let lam_params = lam_params.to_vec();
        let lam_body = lam_body.clone();
        let (out, flat) = alloc_output_array(&out_elem_ty, result_type, builder);
        emit_index_loop(iterable, &iterable_ty, &elem_ty, builder, ctx, |i, elem, b, c| {
            // Optional 0-based SOURCE index (the source position, even though filter's OUTPUT
            // position differs); narrowed Int64→Int32. Ignored by a 1-param predicate.
            let idx = narrow_loop_index(i, b);
            let (pred_raw, pred_ty) =
                inline_lambda_body(&lam_params, &lam_body, &[(elem, elem_ty.clone()), (idx, Type::Int32)], b, c);
            // Coerce the predicate result to an i1 Bool (a concrete-Bool body needs no coercion;
            // a Json/boxed-bool body is unboxed via Coerce).
            let keep = if matches!(pred_ty, Type::Bool) {
                pred_raw
            } else {
                let d = b.alloc_temp(Type::Bool);
                b.emit(Instruction::Coerce { dst: d, src: pred_raw, from_ty: pred_ty, to_ty: Type::Bool });
                d
            };
            let keep_block = b.alloc_block("filter_keep");
            let drop_block = b.alloc_block("filter_drop");
            let join_block = b.alloc_block("filter_skip");
            b.terminate(Terminator::CondJump { cond: keep, then_block: keep_block, else_block: drop_block });
            b.switch_to(keep_block);
            // KEEP: `elem` is BORROWED from the source array — the result array must take its own
            // reference (retain on a tagged concrete-rc push; see `push_output`). The push consumed
            // the per-element box's inner; reclaim only the box SHELL (shell-only — see helper doc).
            push_output(out, flat, &out_elem_ty, elem, &elem_ty, true, b);
            free_combinator_elem_box(elem, &elem_ty, b);
            b.terminate(Terminator::Jump(join_block));
            b.switch_to(drop_block);
            // SKIP: the element is dropped, nothing owns it — FULLY release the per-element box
            // (shell + the inner +1 that `lin_array_get_tagged` retained), else every skipped
            // element's inner leaks (the `filter`-over-`split` residual).
            free_combinator_elem_box_full(elem, &elem_ty, b);
            b.terminate(Terminator::Jump(join_block));
            b.switch_to(join_block);
        });
        return out;
    }

    let pred = lower_callback_in_safe_ctx(&args[1], builder, ctx);
    let (out, flat) = alloc_output_array(&out_elem_ty, result_type, builder);
    emit_index_loop(iterable, &iterable_ty, &elem_ty, builder, ctx, |i, elem, b, _| {
        let idx = narrow_loop_index(i, b);
        let keep = call_body_closure(pred, &[(elem, elem_ty.clone()), (idx, Type::Int32)], &param_tys, &Type::Bool, b);
        let keep_block = b.alloc_block("filter_keep");
        let drop_block = b.alloc_block("filter_drop");
        let join_block = b.alloc_block("filter_skip");
        b.terminate(Terminator::CondJump { cond: keep, then_block: keep_block, else_block: drop_block });
        b.switch_to(keep_block);
        // KEEP: retain on the tagged concrete-rc push, then reclaim the box shell.
        push_output(out, flat, &out_elem_ty, elem, &elem_ty, true, b);
        free_combinator_elem_box(elem, &elem_ty, b);
        b.terminate(Terminator::Jump(join_block));
        b.switch_to(drop_block);
        // SKIP: fully release the dropped element's box.
        free_combinator_elem_box_full(elem, &elem_ty, b);
        b.terminate(Terminator::Jump(join_block));
        b.switch_to(join_block);
    });
    out
}

/// `reduce(iterable, init, f)` → fold `acc = f(acc, elem)` over the elements.
/// The reducer `f` takes `(Json, Json)`, so the accumulator and element are carried as
/// Json (boxed); the final accumulator is coerced back to `result_type`.
fn lower_reduce(args: &[TypedExpr], result_type: &Type, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    let json = Type::TypeVar(u32::MAX);
    let iterable_ty = args[0].ty();
    let (param_tys, _) = callback_signature(&args[2]);
    // Read at the source's PROVABLE representation (ADR-044): a flat scalar for a provably-flat
    // producer, else the tagged Json read (sound for a `[]`+push array mistyped as flat).
    let elem_ty = combinator_read_elem_ty(&args[0], builder, ctx);
    let init_ty = args[1].ty();

    let iterable = lower_expr(&args[0], builder, ctx);

    // INLINE FAST PATH (ADR-044): a capture-less literal reducer lambda with a CONCRETE SCALAR
    // accumulator carries the accumulator UNBOXED through the loop phi and inlines the lambda body
    // each iteration — no per-element box/unbox/closure call. Gated to a scalar `result_type` (the
    // accumulator representation): a union/Json/heap accumulator keeps the boxed Json-phi path below
    // (its phi must carry a uniform boxed ptr, and the inline machinery here assumes a value phi).
    if is_inline_scalar(result_type) {
        if let Some((lam_params, lam_body)) = inlinable_lambda(&args[2]) {
            let lam_params = lam_params.to_vec();
            let lam_body = lam_body.clone();
            let acc_ty = result_type.clone();
            let init_raw = lower_expr(&args[1], builder, ctx);
            // The init must match the accumulator representation (a concrete scalar).
            let init = coerce_arg_to_param_repr(init_raw, &init_ty, &acc_ty, builder);

            // Tag-checked length (0 for a non-array Json) when union. See emit_iterable_len.
            let len = emit_iterable_len(iterable, &iterable_ty, builder);
            let zero = builder.const_temp(Const::Int(0, Type::Int64));

            let preheader = builder.current_block;
            let header = builder.alloc_block("reduce_header");
            let body = builder.alloc_block("reduce_body");
            let exit = builder.alloc_block("reduce_exit");

            let i = builder.alloc_temp(Type::Int64);
            let i_next = builder.alloc_temp(Type::Int64);
            let acc = builder.alloc_temp(acc_ty.clone());
            builder.terminate(Terminator::Jump(header));

            builder.switch_to(header);
            builder.emit(Instruction::Phi {
                dst: i, ty: Type::Int64, incomings: vec![(zero, preheader), (i_next, body)],
            });
            // The accumulator phi back-edge value is filled in after the body is lowered (the body
            // may switch blocks, e.g. an `if` inside the reducer — `patch_phi_incoming`).
            builder.emit(Instruction::Phi {
                dst: acc, ty: acc_ty.clone(), incomings: vec![(init, preheader), (acc, body)],
            });
            let cond = builder.alloc_temp(Type::Bool);
            builder.emit(Instruction::Binary {
                dst: cond, op: BinOp::Lt, lhs: i, rhs: len, operand_ty: Type::Int64, ty: Type::Bool,
            });
            builder.terminate(Terminator::CondJump { cond, then_block: body, else_block: exit });

            builder.switch_to(body);
            let elem = builder.alloc_temp(elem_ty.clone());
            builder.emit(Instruction::Index {
                dst: elem, object: iterable, key: i,
                obj_ty: iterable_ty.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone(),
            });
            // acc_next = <lambda body>(acc, elem, i), inlined. The reducer params are (acc, elem)
            // plus the OPTIONAL 0-based SOURCE index `i` (narrowed Int64→Int32). A 2-param
            // `(acc, x) => …` reducer ignores the surplus index arg (`inline_lambda_body` binds by
            // the lambda's own param count).
            let idx = narrow_loop_index(i, builder);
            let (acc_next_raw, acc_next_ty) = inline_lambda_body(
                &lam_params, &lam_body,
                &[(acc, acc_ty.clone()), (elem, elem_ty.clone()), (idx, Type::Int32)], builder, ctx,
            );
            let acc_next = coerce_arg_to_param_repr(acc_next_raw, &acc_next_ty, &acc_ty, builder);
            // A PACKED sealed-array source materialized a fresh +1 element struct; the reducer read a
            // copy out of it into the scalar accumulator, so release it or it leaks per iteration.
            // (Only the inline-scalar-accumulator path reaches here; a heap/union acc keeps the boxed
            // Json-phi path below, which reads the element via the union box, not a sealed struct.)
            free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, builder);
            let one = builder.const_temp(Const::Int(1, Type::Int64));
            builder.emit(Instruction::Binary {
                dst: i_next, op: BinOp::Add, lhs: i, rhs: one, operand_ty: Type::Int64, ty: Type::Int64,
            });
            // The body may have switched blocks; patch both phis' back-edge predecessors + the acc
            // phi's incoming value to the actual loop-back block / computed accumulator.
            let back_block = builder.current_block;
            builder.terminate(Terminator::Jump(header));
            builder.patch_phi_incoming(header, i, body, back_block);
            builder.patch_phi_incoming_value(header, acc, acc, acc_next, back_block);

            builder.switch_to(exit);
            return acc;
        }
    }

    let init_raw = lower_expr(&args[1], builder, ctx);
    let init = box_to_json(init_raw, &init_ty, builder);
    let f = lower_callback_in_safe_ctx(&args[2], builder, ctx);

    // Tag-checked length (0 for a non-array Json) when union. See emit_iterable_len.
    let len = emit_iterable_len(iterable, &iterable_ty, builder);
    let zero = builder.const_temp(Const::Int(0, Type::Int64));

    let preheader = builder.current_block;
    let header = builder.alloc_block("reduce_header");
    let body = builder.alloc_block("reduce_body");
    let exit = builder.alloc_block("reduce_exit");

    let i = builder.alloc_temp(Type::Int64);
    let i_next = builder.alloc_temp(Type::Int64);
    let acc = builder.alloc_temp(json.clone());
    let acc_next = builder.alloc_temp(json.clone());
    builder.terminate(Terminator::Jump(header));

    builder.switch_to(header);
    builder.emit(Instruction::Phi {
        dst: i, ty: Type::Int64, incomings: vec![(zero, preheader), (i_next, body)],
    });
    builder.emit(Instruction::Phi {
        dst: acc, ty: json.clone(), incomings: vec![(init, preheader), (acc_next, body)],
    });
    let cond = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::Binary {
        dst: cond, op: BinOp::Lt, lhs: i, rhs: len, operand_ty: Type::Int64, ty: Type::Bool,
    });
    builder.terminate(Terminator::CondJump { cond, then_block: body, else_block: exit });

    builder.switch_to(body);
    let elem = builder.alloc_temp(elem_ty.clone());
    builder.emit(Instruction::Index {
        dst: elem, object: iterable, key: i,
        obj_ty: iterable_ty.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone(),
    });
    // acc_next = f(acc, elem[, i]). acc is carried as Json; coerce both args to the reducer's
    // declared param types. A 3-param reducer `(acc, item, i) => …` also receives the OPTIONAL
    // 0-based SOURCE index (narrowed Int64→Int32); a 2-param reducer omits it (its closure wrapper
    // has only two parameters, so passing a third would be ABI UB).
    let acc_arg = coerce_arg_to_param(acc, &json, param_tys.first(), builder);
    let elem_arg = coerce_arg_to_param(elem, &elem_ty, param_tys.get(1), builder);
    let mut call_args = vec![acc_arg, elem_arg];
    if param_tys.len() >= 3 {
        let idx = narrow_loop_index(i, builder);
        let idx_arg = coerce_arg_to_param(idx, &Type::Int32, param_tys.get(2), builder);
        call_args.push(idx_arg);
    }
    builder.emit(Instruction::Call {
        dst: acc_next, callee: CallTarget::Indirect(f), args: call_args, ret_ty: json.clone(),
    });
    let one = builder.const_temp(Const::Int(1, Type::Int64));
    builder.emit(Instruction::Binary {
        dst: i_next, op: BinOp::Add, lhs: i, rhs: one, operand_ty: Type::Int64, ty: Type::Int64,
    });
    builder.terminate(Terminator::Jump(header));

    builder.switch_to(exit);
    // Coerce the Json accumulator back to the declared result type.
    if is_union_ty(result_type) {
        acc
    } else {
        let out = builder.alloc_temp(result_type.clone());
        builder.emit(Instruction::Coerce {
            dst: out, src: acc, from_ty: json, to_ty: result_type.clone(),
        });
        out
    }
}

/// `sort(arr, cmp)` over a flat NUMERIC scalar array with a CAPTURE-LESS literal comparator → an
/// inline, STABLE, bottom-up merge sort over the UNBOXED flat buffer with the comparator body spliced
/// directly into the single comparison site (no per-comparison box/unbox/closure indirection — the
/// zero-box sort win). Routed here from monomorphize's `try_inline_scalar_sort`; every other `sort`
/// (non-scalar array, capturing/stored comparator, the `Json` `_sortJ` path) keeps the generic boxed
/// merge-sort, so this is a TIGHTLY-gated fast path that fails safe.
///
/// Semantics are byte-identical to the pure-Lin `stdlib/array.lin` sort: a bottom-up merge of runs of
/// doubling width, taking the LEFT run first on a tie (`cmp(a,b) <= 0`) → STABLE. We ping-pong by
/// merging `out -> work` each pass then copying `work` back into `out`, so the result always lives in
/// `out` (one extra O(n) copy per pass — O(n log n) total, negligible against the comparisons). The
/// buffers are FLAT scalar arrays (sound for a numeric scalar element); the comparator reads them via
/// the inlined flat getter. The copy-IN from `arr` uses the representation-agnostic tagged `Index`
/// (sound even for a `[]`+push array statically typed flat).
fn lower_sort(args: &[TypedExpr], result_type: &Type, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    let arr_ty = args[0].ty();
    // Element type to STORE (the buffers' flat scalar repr) — from the array's static element type.
    let elem_ty = match result_type {
        Type::Array(t) | Type::Iterator(t) => (**t).clone(),
        _ => iter_elem_type(&arr_ty),
    };
    let flat = FlatElemKind::from_type(&elem_ty)
        .expect("lower_sort gated on a flat scalar element by try_inline_scalar_sort");
    // Read elements FROM the input array at its provable representation (tagged unless provably flat),
    // matching `for`/`map` — sound for a `[]`+push array statically typed `Int32[]`.
    let read_elem_ty = combinator_read_elem_ty(&args[0], builder, ctx);

    let (lam_params, lam_body) = inlinable_lambda(&args[1])
        .expect("lower_sort gated on a capture-less literal comparator by try_inline_scalar_sort");
    let lam_params = lam_params.to_vec();
    let lam_body = lam_body.clone();

    let arr = lower_expr(&args[0], builder, ctx);
    let n = emit_iterable_len(arr, &arr_ty, builder); // Int64

    // out / work: two flat scalar buffers, both of length n (work's contents are overwritten in the
    // first pass). `out` is the array we ultimately return.
    let out = builder.alloc_temp(result_type.clone());
    builder.emit(Instruction::CallIntrinsic {
        dst: out, intrinsic: Intrinsic::FlatArrayAlloc(flat), args: vec![], ret_ty: result_type.clone(),
    });
    builder.register_owned(out, result_type.clone());
    let work = builder.alloc_temp(result_type.clone());
    builder.emit(Instruction::CallIntrinsic {
        dst: work, intrinsic: Intrinsic::FlatArrayAlloc(flat), args: vec![], ret_ty: result_type.clone(),
    });
    builder.register_owned(work, result_type.clone());

    let zero = builder.const_temp(Const::Int(0, Type::Int64));
    let one = builder.const_temp(Const::Int(1, Type::Int64));

    // ---- COPY-IN: out[i] = work[i] = arr[i] for i in 0..n (gives both buffers length n) ----
    {
        let pre = builder.current_block;
        let header = builder.alloc_block("sort_copy_hdr");
        let body = builder.alloc_block("sort_copy_body");
        let exit = builder.alloc_block("sort_copy_exit");
        let i = builder.alloc_temp(Type::Int64);
        let i_next = builder.alloc_temp(Type::Int64);
        builder.terminate(Terminator::Jump(header));
        builder.switch_to(header);
        builder.emit(Instruction::Phi { dst: i, ty: Type::Int64, incomings: vec![(zero, pre), (i_next, body)] });
        let cond = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Binary { dst: cond, op: BinOp::Lt, lhs: i, rhs: n, operand_ty: Type::Int64, ty: Type::Bool });
        builder.terminate(Terminator::CondJump { cond, then_block: body, else_block: exit });
        builder.switch_to(body);
        // elem = arr[i] (tagged-safe read), coerced to the flat scalar repr.
        let elem_raw = builder.alloc_temp(read_elem_ty.clone());
        builder.emit(Instruction::Index {
            dst: elem_raw, object: arr, key: i,
            obj_ty: arr_ty.clone(), key_ty: Type::Int64, result_ty: read_elem_ty.clone(),
        });
        let elem = coerce_arg_to_param_repr(elem_raw, &read_elem_ty, &elem_ty, builder);
        // When the source is read via the tagged path (`read_elem_ty` is the boxed wildcard —
        // a `[]`+push array not provably flat), `Index` → `lin_array_get_tagged` returns a FRESH
        // +1 box that the `Coerce` above unboxes to the flat scalar `elem`. That box is then dead;
        // reclaim its shell (mirrors `lower_for`'s per-iteration element-box reclaim) or it leaks
        // one box PER ELEMENT, PER SORT (the ~16 B/elem `sort` result leak). `lower_sort` is gated
        // to flat-scalar elements, so the box has no heap inner — freeing the shell fully reclaims
        // it and is a documented no-op on cached small-int/bool boxes. Guarded `IfDistinct` so a
        // no-op coerce (already-flat read, `elem == elem_raw`) never double-frees a live value.
        if is_union_ty(&read_elem_ty) {
            builder.emit(Instruction::FreeBoxShellIfDistinct { val: elem_raw, other: elem });
        }
        let pd1 = builder.alloc_temp(Type::Null);
        builder.emit(Instruction::CallIntrinsic { dst: pd1, intrinsic: Intrinsic::FlatArrayPush(flat), args: vec![out, elem], ret_ty: Type::Null });
        let pd2 = builder.alloc_temp(Type::Null);
        builder.emit(Instruction::CallIntrinsic { dst: pd2, intrinsic: Intrinsic::FlatArrayPush(flat), args: vec![work, elem], ret_ty: Type::Null });
        builder.emit(Instruction::Binary { dst: i_next, op: BinOp::Add, lhs: i, rhs: one, operand_ty: Type::Int64, ty: Type::Int64 });
        builder.terminate(Terminator::Jump(header));
        builder.switch_to(exit);
    }

    // ---- WIDTH loop: width = 1; while width < n; width *= 2 ----
    let w_pre = builder.current_block;
    let w_header = builder.alloc_block("sort_w_hdr");
    let w_body = builder.alloc_block("sort_w_body");
    let w_exit = builder.alloc_block("sort_w_exit");
    let width = builder.alloc_temp(Type::Int64);
    let width_next = builder.alloc_temp(Type::Int64);
    builder.terminate(Terminator::Jump(w_header));
    builder.switch_to(w_header);
    builder.emit(Instruction::Phi { dst: width, ty: Type::Int64, incomings: vec![(one, w_pre), (width_next, w_body)] });
    let w_cond = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::Binary { dst: w_cond, op: BinOp::Lt, lhs: width, rhs: n, operand_ty: Type::Int64, ty: Type::Bool });
    builder.terminate(Terminator::CondJump { cond: w_cond, then_block: w_body, else_block: w_exit });
    builder.switch_to(w_body);
    let two_w = builder.alloc_temp(Type::Int64);
    let two = builder.const_temp(Const::Int(2, Type::Int64));
    builder.emit(Instruction::Binary { dst: two_w, op: BinOp::Mul, lhs: width, rhs: two, operand_ty: Type::Int64, ty: Type::Int64 });

    // ---- LO loop: lo = 0; while lo < n; lo += 2*width — merge each run pair out[..] -> work[..] ----
    let lo_pre = builder.current_block;
    let lo_header = builder.alloc_block("sort_lo_hdr");
    let lo_body = builder.alloc_block("sort_lo_body");
    let lo_exit = builder.alloc_block("sort_lo_exit");
    let lo = builder.alloc_temp(Type::Int64);
    let lo_next = builder.alloc_temp(Type::Int64);
    builder.terminate(Terminator::Jump(lo_header));
    builder.switch_to(lo_header);
    builder.emit(Instruction::Phi { dst: lo, ty: Type::Int64, incomings: vec![(zero, lo_pre), (lo_next, lo_body)] });
    let lo_cond = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::Binary { dst: lo_cond, op: BinOp::Lt, lhs: lo, rhs: n, operand_ty: Type::Int64, ty: Type::Bool });
    builder.terminate(Terminator::CondJump { cond: lo_cond, then_block: lo_body, else_block: lo_exit });
    builder.switch_to(lo_body);
    // lo_b = min(lo + width, n); hi = min(lo + 2*width, n)
    let lo_plus_w = builder.alloc_temp(Type::Int64);
    builder.emit(Instruction::Binary { dst: lo_plus_w, op: BinOp::Add, lhs: lo, rhs: width, operand_ty: Type::Int64, ty: Type::Int64 });
    let lo_b = emit_min_i64(lo_plus_w, n, builder);
    let lo_plus_2w = builder.alloc_temp(Type::Int64);
    builder.emit(Instruction::Binary { dst: lo_plus_2w, op: BinOp::Add, lhs: lo, rhs: two_w, operand_ty: Type::Int64, ty: Type::Int64 });
    let hi = emit_min_i64(lo_plus_2w, n, builder);

    // ---- MERGE loop: i in [lo, lo_b), j in [lo_b, hi), k in [lo, hi). Stable: tie → take left. ----
    // Carries i, j, k as phis (k = i + j - lo_b always, but a phi keeps it simple). The comparator body
    // is inlined at the single `cmp(out[i], out[j]) <= 0` decision — the unboxed hot path.
    let m_pre = builder.current_block;
    let m_header = builder.alloc_block("sort_m_hdr");
    let m_body = builder.alloc_block("sort_m_body");
    let m_take_l = builder.alloc_block("sort_m_takel");
    let m_take_r = builder.alloc_block("sort_m_taker");
    let m_cmp = builder.alloc_block("sort_m_cmp");
    let m_advance = builder.alloc_block("sort_m_adv");
    let m_exit = builder.alloc_block("sort_m_exit");
    let mi = builder.alloc_temp(Type::Int64);
    let mj = builder.alloc_temp(Type::Int64);
    let mk = builder.alloc_temp(Type::Int64);
    let mi_next = builder.alloc_temp(Type::Int64);
    let mj_next = builder.alloc_temp(Type::Int64);
    let mk_next = builder.alloc_temp(Type::Int64);
    builder.terminate(Terminator::Jump(m_header));
    builder.switch_to(m_header);
    builder.emit(Instruction::Phi { dst: mi, ty: Type::Int64, incomings: vec![(lo, m_pre), (mi_next, m_advance)] });
    builder.emit(Instruction::Phi { dst: mj, ty: Type::Int64, incomings: vec![(lo_b, m_pre), (mj_next, m_advance)] });
    builder.emit(Instruction::Phi { dst: mk, ty: Type::Int64, incomings: vec![(lo, m_pre), (mk_next, m_advance)] });
    let m_cond = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::Binary { dst: m_cond, op: BinOp::Lt, lhs: mk, rhs: hi, operand_ty: Type::Int64, ty: Type::Bool });
    builder.terminate(Terminator::CondJump { cond: m_cond, then_block: m_body, else_block: m_exit });

    // m_body: decide which side to take. If left exhausted (i >= lo_b) → take right; if right exhausted
    // (j >= hi) → take left; else compare.
    builder.switch_to(m_body);
    let l_exhausted = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::Binary { dst: l_exhausted, op: BinOp::GtEq, lhs: mi, rhs: lo_b, operand_ty: Type::Int64, ty: Type::Bool });
    let after_l = builder.alloc_block("sort_m_chk_r");
    builder.terminate(Terminator::CondJump { cond: l_exhausted, then_block: m_take_r, else_block: after_l });
    builder.switch_to(after_l);
    let r_exhausted = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::Binary { dst: r_exhausted, op: BinOp::GtEq, lhs: mj, rhs: hi, operand_ty: Type::Int64, ty: Type::Bool });
    builder.terminate(Terminator::CondJump { cond: r_exhausted, then_block: m_take_l, else_block: m_cmp });

    // m_cmp: cmp(out[i], out[j]) <= 0 → take left (stable), else take right. Comparator inlined.
    builder.switch_to(m_cmp);
    let a_val = builder.alloc_temp(elem_ty.clone());
    builder.emit(Instruction::Index { dst: a_val, object: out, key: mi, obj_ty: result_type.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone() });
    let b_val = builder.alloc_temp(elem_ty.clone());
    builder.emit(Instruction::Index { dst: b_val, object: out, key: mj, obj_ty: result_type.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone() });
    let (cmp_raw, cmp_ty) = inline_lambda_body(&lam_params, &lam_body, &[(a_val, elem_ty.clone()), (b_val, elem_ty.clone())], builder, ctx);
    // The comparator returns an Int32 cmp value; coerce a boxed/widened result to a concrete Int32.
    let cmp_i32 = coerce_arg_to_param_repr(cmp_raw, &cmp_ty, &Type::Int32, builder);
    let zero32 = builder.const_temp(Const::Int(0, Type::Int32));
    let take_left = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::Binary { dst: take_left, op: BinOp::LtEq, lhs: cmp_i32, rhs: zero32, operand_ty: Type::Int32, ty: Type::Bool });
    builder.terminate(Terminator::CondJump { cond: take_left, then_block: m_take_l, else_block: m_take_r });

    // m_take_l: work[k] = out[i]; i += 1
    builder.switch_to(m_take_l);
    let lv = builder.alloc_temp(elem_ty.clone());
    builder.emit(Instruction::Index { dst: lv, object: out, key: mi, obj_ty: result_type.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone() });
    builder.emit(Instruction::IndexSet { object: work, key: mk, value: lv, obj_ty: result_type.clone(), key_ty: Type::Int64, val_ty: elem_ty.clone() });
    let mi_inc = builder.alloc_temp(Type::Int64);
    builder.emit(Instruction::Binary { dst: mi_inc, op: BinOp::Add, lhs: mi, rhs: one, operand_ty: Type::Int64, ty: Type::Int64 });
    builder.terminate(Terminator::Jump(m_advance));
    let take_l_block = m_take_l;

    // m_take_r: work[k] = out[j]; j += 1
    builder.switch_to(m_take_r);
    let rv = builder.alloc_temp(elem_ty.clone());
    builder.emit(Instruction::Index { dst: rv, object: out, key: mj, obj_ty: result_type.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone() });
    builder.emit(Instruction::IndexSet { object: work, key: mk, value: rv, obj_ty: result_type.clone(), key_ty: Type::Int64, val_ty: elem_ty.clone() });
    let mj_inc = builder.alloc_temp(Type::Int64);
    builder.emit(Instruction::Binary { dst: mj_inc, op: BinOp::Add, lhs: mj, rhs: one, operand_ty: Type::Int64, ty: Type::Int64 });
    builder.terminate(Terminator::Jump(m_advance));
    let take_r_block = m_take_r;

    // m_advance: k += 1; i/j carried via phis from whichever branch ran. Taking LEFT advances i and
    // leaves j unchanged; taking RIGHT advances j and leaves i unchanged.
    builder.switch_to(m_advance);
    builder.emit(Instruction::Phi { dst: mi_next, ty: Type::Int64, incomings: vec![(mi_inc, take_l_block), (mi, take_r_block)] });
    builder.emit(Instruction::Phi { dst: mj_next, ty: Type::Int64, incomings: vec![(mj, take_l_block), (mj_inc, take_r_block)] });
    builder.emit(Instruction::Binary { dst: mk_next, op: BinOp::Add, lhs: mk, rhs: one, operand_ty: Type::Int64, ty: Type::Int64 });
    builder.terminate(Terminator::Jump(m_header));

    // ---- after merge of this run pair: lo += 2*width ----
    builder.switch_to(m_exit);
    builder.emit(Instruction::Binary { dst: lo_next, op: BinOp::Add, lhs: lo, rhs: two_w, operand_ty: Type::Int64, ty: Type::Int64 });
    // The lo phi's back-edge predecessor is THIS block (`m_exit`), not the provisional `lo_body`: the
    // run-pair merge spans the merge sub-loop, so control returns to `lo_header` from here.
    builder.patch_phi_incoming(lo_header, lo, lo_body, m_exit);
    builder.terminate(Terminator::Jump(lo_header));

    // ---- lo_exit: copy work[0..n) back into out[0..n) so the result lives in `out` ----
    builder.switch_to(lo_exit);
    {
        let pre = builder.current_block;
        let header = builder.alloc_block("sort_cb_hdr");
        let body = builder.alloc_block("sort_cb_body");
        let exit = builder.alloc_block("sort_cb_exit");
        let i = builder.alloc_temp(Type::Int64);
        let i_next = builder.alloc_temp(Type::Int64);
        builder.terminate(Terminator::Jump(header));
        builder.switch_to(header);
        builder.emit(Instruction::Phi { dst: i, ty: Type::Int64, incomings: vec![(zero, pre), (i_next, body)] });
        let cond = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Binary { dst: cond, op: BinOp::Lt, lhs: i, rhs: n, operand_ty: Type::Int64, ty: Type::Bool });
        builder.terminate(Terminator::CondJump { cond, then_block: body, else_block: exit });
        builder.switch_to(body);
        let wv = builder.alloc_temp(elem_ty.clone());
        builder.emit(Instruction::Index { dst: wv, object: work, key: i, obj_ty: result_type.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone() });
        builder.emit(Instruction::IndexSet { object: out, key: i, value: wv, obj_ty: result_type.clone(), key_ty: Type::Int64, val_ty: elem_ty.clone() });
        builder.emit(Instruction::Binary { dst: i_next, op: BinOp::Add, lhs: i, rhs: one, operand_ty: Type::Int64, ty: Type::Int64 });
        builder.terminate(Terminator::Jump(header));
        builder.switch_to(exit);
    }
    // width *= 2, back to the width header. The width phi's back-edge predecessor is THIS block (the
    // copy-back exit), not the provisional `w_body`: a full pass spans the lo/merge/copy-back loops.
    let w_back_block = builder.current_block;
    builder.emit(Instruction::Binary { dst: width_next, op: BinOp::Mul, lhs: width, rhs: two, operand_ty: Type::Int64, ty: Type::Int64 });
    builder.patch_phi_incoming(w_header, width, w_body, w_back_block);
    builder.terminate(Terminator::Jump(w_header));

    // ---- done: `out` holds the fully-sorted result. ----
    builder.switch_to(w_exit);
    out
}

/// `min(a, b)` over two Int64 temps via a select-style CondJump+phi. Used by `lower_sort` for the
/// run-boundary clamps (`min(lo+width, n)`).
fn emit_min_i64(a: Temp, b: Temp, builder: &mut FuncBuilder) -> Temp {
    let cond = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::Binary { dst: cond, op: BinOp::Lt, lhs: a, rhs: b, operand_ty: Type::Int64, ty: Type::Bool });
    let then_b = builder.alloc_block("min_a");
    let else_b = builder.alloc_block("min_b");
    let mrg = builder.alloc_block("min_mrg");
    builder.terminate(Terminator::CondJump { cond, then_block: then_b, else_block: else_b });
    builder.switch_to(then_b);
    builder.terminate(Terminator::Jump(mrg));
    builder.switch_to(else_b);
    builder.terminate(Terminator::Jump(mrg));
    builder.switch_to(mrg);
    let out = builder.alloc_temp(Type::Int64);
    builder.emit(Instruction::Phi { dst: out, ty: Type::Int64, incomings: vec![(a, then_b), (b, else_b)] });
    out
}

// -------------------------------------------------------------------------
// If lowering
// -------------------------------------------------------------------------

/// Lower a condition expression to an i1 Bool temp. A condition whose static type is not
/// already Bool (e.g. a call to an untyped `f: Function` predicate, which returns a boxed
/// Json) is coerced — codegen lowers a Json→Bool Coerce via lin_unbox_bool. Without this,
/// codegen's CondJump sees a non-i1 value and defaults the branch to `false`.
fn lower_cond_as_bool(cond: &TypedExpr, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    let t = lower_expr(cond, builder, ctx);
    let cond_ty = cond.ty();
    if matches!(cond_ty, Type::Bool) {
        t
    } else {
        let dst = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Coerce {
            dst, src: t, from_ty: cond_ty, to_ty: Type::Bool,
        });
        dst
    }
}

fn lower_if(
    cond: &TypedExpr,
    then_br: &TypedExpr,
    else_br: &TypedExpr,
    result_type: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Temp {
    let cond_temp = lower_cond_as_bool(cond, builder, ctx);

    let then_block = builder.alloc_block("if_then");
    let else_block = builder.alloc_block("if_else");
    let merge_block = builder.alloc_block("if_merge");

    // Tag the branch entry blocks with their source spans for coverage. The merge block
    // covers no distinct source region, so it stays None.
    builder.set_block_span(then_block, then_br.span());
    builder.set_block_span(else_block, else_br.span());

    builder.terminate(Terminator::CondJump {
        cond: cond_temp,
        then_block,
        else_block,
    });

    let result_dst = builder.alloc_temp(result_type.clone());

    // Each branch gets its own ownership scope so heap temps it allocates are released
    // at the end of *that branch* — not in the merge block, where only one branch's
    // temps are live (releasing the other branch's temps there frees undefined values).
    // `coerce_if_branch` produces a value the merge OWNS independently (cloning a borrowed
    // union/concrete that aliases an enclosing-scope value), so registering+releasing the
    // merge is always balanced — no borrowed-box double-free (the historic reason a union
    // merge was left unowned: e.g. minBy's reducer `if x[0] < acc[0] then x else acc` over
    // params — now those params are cloned into a fresh +1).
    // We collect (value_temp, predecessor_block) for a Phi in the merge block, recording
    // the ACTUAL predecessor (the block current at the end of the branch, which may differ
    // from the branch entry if the branch contained nested control flow).
    let mut incomings: Vec<(Temp, BlockId)> = Vec::new();
    // Whether the merged value carries an independent +1 (so the enclosing scope owns/releases
    // it). Determined by the branch coercion; both branches agree (it is a function of the
    // result representation). Defaults to the concrete-rc rule if neither branch falls through.
    let mut merge_owned = is_rc_type(result_type) || is_union_ty(result_type);

    // Snapshot the plain SSA var-slot → temp map BEFORE the branches. A `var` mutated inside a
    // branch (`LocalSet` for a plain SSA-temp slot, line ~1853) just rebinds `slots[slot]` to a
    // NEW temp that is only defined inside that branch's block. Without merging, a read of the
    // slot AFTER the join sees either a temp that doesn't dominate the merge (SSA violation) or
    // the wrong branch's value — the closure-local-var-in-if bug. We record per-branch which
    // slots were rebound, then emit join phis below (mirroring the if-result phi). Cells and
    // global vars are NOT in this set: their `slots` entry is a stable cell pointer / they read
    // through GlobalValGet, so reassignment is already visible across the join.
    let pre_slots = builder.plain_var_slot_snapshot(ctx);

    // --- then branch ---
    builder.switch_to(then_block);
    builder.push_scope();
    // UNBOXED SUM TYPE: a sum-eligible object LITERAL branch flowing into a sum-typed `if` result
    // is constructed DIRECTLY as a SumNode (skip the build-boxed-then-project round-trip). When it
    // fires, the branch value already carries the sum type, so `coerce_if_branch` sees value_ty ==
    // result_type and just transfers the owned +1 (no re-coercion).
    let (then_raw, then_eff_ty) = match try_lower_sum_literal(then_br, result_type, builder, ctx) {
        Some(t) => (t, result_type.clone()),
        None => (lower_expr(then_br, builder, ctx), then_br.ty()),
    };
    let mut then_reassigned: Vec<(usize, Temp)> = Vec::new();
    let mut then_pred = builder.current_block;
    let then_live = if !builder.is_current_block_terminated() {
        let (then_val, mut keep, owned) = coerce_if_branch(then_raw, &then_eff_ty, result_type, builder);
        merge_owned = owned;
        // A slot the branch rebound holds a value registered owned in THIS branch scope (the
        // LocalSet's value temp). It must survive the branch pop so the join phi can forward it,
        // so add it to the keep-set: its +1 transfers up to the enclosing scope, where the phi
        // result becomes the slot's single owner (see below).
        then_reassigned = builder.collect_reassigned_slots(&pre_slots, ctx);
        for (_, t) in &then_reassigned {
            keep.push(*t);
        }
        builder.pop_scope_releasing_keep(&keep);
        then_pred = builder.current_block;
        incomings.push((then_val, builder.current_block));
        builder.terminate(Terminator::Jump(merge_block));
        true
    } else {
        builder.discard_scope();
        false
    };
    // Restore the slot map to its pre-if state so the else branch sees the ORIGINAL slot temps,
    // not the then-branch's rebindings (each branch must lower against the pre-if values).
    builder.restore_plain_var_slots(&pre_slots);

    // --- else branch ---
    builder.switch_to(else_block);
    builder.push_scope();
    let (else_raw, else_eff_ty) = match try_lower_sum_literal(else_br, result_type, builder, ctx) {
        Some(t) => (t, result_type.clone()),
        None => (lower_expr(else_br, builder, ctx), else_br.ty()),
    };
    let mut else_reassigned: Vec<(usize, Temp)> = Vec::new();
    let mut else_pred = builder.current_block;
    let else_live = if !builder.is_current_block_terminated() {
        let (else_val, mut keep, owned) = coerce_if_branch(else_raw, &else_eff_ty, result_type, builder);
        merge_owned = owned;
        else_reassigned = builder.collect_reassigned_slots(&pre_slots, ctx);
        for (_, t) in &else_reassigned {
            keep.push(*t);
        }
        builder.pop_scope_releasing_keep(&keep);
        else_pred = builder.current_block;
        incomings.push((else_val, builder.current_block));
        builder.terminate(Terminator::Jump(merge_block));
        true
    } else {
        builder.discard_scope();
        false
    };

    builder.switch_to(merge_block);

    // Merge any plain `var` slot mutated in a branch with a join phi (the slot now reads the phi
    // result after the if). For each slot reassigned in EITHER branch, the incoming value is the
    // branch's rebound temp if it reassigned, otherwise the pre-if temp (the value flowing in
    // unchanged on that edge). Exactly one phi incoming reference is live at runtime, so the phi
    // result holds a single +1 the enclosing scope owns; we drop the per-branch and pre-if
    // registrations and register the phi result once to keep ownership balanced.
    merge_var_slots(
        builder,
        &pre_slots,
        &then_reassigned,
        if then_live { Some(then_pred) } else { None },
        &else_reassigned,
        if else_live { Some(else_pred) } else { None },
    );

    // Merge the per-branch results with a Phi. (A plain Copy into a shared temp is wrong:
    // the single-pass codegen would let the last-compiled branch's value win for both paths.)
    builder.emit(Instruction::Phi {
        dst: result_dst,
        ty: result_type.clone(),
        incomings,
    });
    // The merged result is owned by the enclosing scope (released there, or kept if it is
    // the block's return value).
    if merge_owned {
        builder.register_owned(result_dst, result_type.clone());
    }
    result_dst
}

/// After both branches of an `if` have jumped to `merge_block`, reconcile any plain SSA `var`
/// slot that was reassigned in at least one branch by emitting a join phi. `pre_slots` is the
/// (slot, temp, type) snapshot taken before the branches; `*_reassigned` are the slots each branch
/// rebound (slot → branch-local temp). `*_pred` is the actual predecessor block at the end of the
/// branch (it may differ from the branch entry if the branch had nested control flow), or None if
/// that branch diverged (no edge into the merge — its slot values are unreachable there).
fn merge_var_slots(
    builder: &mut FuncBuilder,
    pre_slots: &[(usize, Temp, Type)],
    then_reassigned: &[(usize, Temp)],
    then_pred: Option<BlockId>,
    else_reassigned: &[(usize, Temp)],
    else_pred: Option<BlockId>,
) {
    // The set of slots needing a join phi = those reassigned in either live branch.
    use std::collections::HashSet;
    let mut changed: HashSet<usize> = HashSet::new();
    if then_pred.is_some() {
        for (s, _) in then_reassigned {
            changed.insert(*s);
        }
    }
    if else_pred.is_some() {
        for (s, _) in else_reassigned {
            changed.insert(*s);
        }
    }
    if changed.is_empty() {
        return;
    }
    let then_map: std::collections::HashMap<usize, Temp> = then_reassigned.iter().copied().collect();
    let else_map: std::collections::HashMap<usize, Temp> = else_reassigned.iter().copied().collect();
    for (slot, pre_temp, ty) in pre_slots {
        if !changed.contains(slot) {
            continue;
        }
        // Build the phi incomings: the rebound temp on a branch that reassigned, the pre-if temp
        // on a branch that left the slot alone. Only include an edge if that branch is live
        // (didn't diverge); if one branch diverged, control reaches the merge solely via the other
        // edge and a single-incoming phi is correct.
        let mut incomings: Vec<(Temp, BlockId)> = Vec::new();
        if let Some(tp) = then_pred {
            incomings.push((then_map.get(slot).copied().unwrap_or(*pre_temp), tp));
        }
        if let Some(ep) = else_pred {
            incomings.push((else_map.get(slot).copied().unwrap_or(*pre_temp), ep));
        }
        let phi_dst = builder.alloc_temp(ty.clone());
        builder.emit(Instruction::Phi { dst: phi_dst, ty: ty.clone(), incomings });
        // The pre-if value and each branch-rebound value were registered owned in the enclosing
        // scope (the pre-if init / read, plus each branch's kept LocalSet value transferred up).
        // After the join exactly ONE of them is live, reachable only as the phi result — so drop
        // those individual registrations and register the phi as the slot's single owner.
        if needs_owning(ty) {
            builder.unregister_owned(*pre_temp);
            if let Some(&t) = then_map.get(slot) {
                builder.unregister_owned(t);
            }
            if let Some(&t) = else_map.get(slot) {
                builder.unregister_owned(t);
            }
            builder.register_owned(phi_dst, ty.clone());
        }
        // The slot now reads the merged phi after the if.
        builder.slots.insert(*slot, phi_dst);
    }
}

/// Lower a short-circuiting `&&` / `||` (spec §8) as branch + merge + Phi, so the RHS is
/// only evaluated on the path that needs it.
///
/// - `a && b`: eval a; if a then eval b else `false`; phi.
/// - `a || b`: eval a; if a then `true`  else eval b; phi.
///
/// The RHS is lowered INSIDE the conditionally-executed block (its own ownership scope), so any
/// owned temps it allocates are released there and are only ever created on the taken path —
/// exactly as lower_if handles a branch arm. Both operands are booleans (scalars), so the result
/// is RC-trivial.
fn lower_short_circuit(
    left: &TypedExpr,
    op: BinOp,
    right: &TypedExpr,
    _result_type: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Temp {
    let lhs = lower_cond_as_bool(left, builder, ctx);

    // The block that evaluates the RHS, and the block that short-circuits to a constant.
    let rhs_block = builder.alloc_block(if matches!(op, BinOp::And) { "and_rhs" } else { "or_rhs" });
    let short_block = builder.alloc_block(if matches!(op, BinOp::And) { "and_short" } else { "or_short" });
    let merge_block = builder.alloc_block("sc_merge");
    builder.set_block_span(rhs_block, right.span());

    // For `&&`, the RHS is evaluated when the LHS is true; for `||`, when the LHS is false.
    let (then_block, else_block) = match op {
        BinOp::And => (rhs_block, short_block),
        BinOp::Or => (short_block, rhs_block),
        _ => unreachable!("lower_short_circuit only handles And/Or"),
    };
    builder.terminate(Terminator::CondJump {
        cond: lhs,
        then_block,
        else_block,
    });

    let result_dst = builder.alloc_temp(Type::Bool);
    let mut incomings: Vec<(Temp, BlockId)> = Vec::new();

    // --- RHS block: evaluate the right operand (its own ownership scope) ---
    builder.switch_to(rhs_block);
    builder.push_scope();
    let rhs_raw = lower_cond_as_bool(right, builder, ctx);
    if !builder.is_current_block_terminated() {
        // rhs is a Bool scalar; keep it across the scope pop (nothing to release for a bool).
        builder.pop_scope_releasing_keep(&[rhs_raw]);
        incomings.push((rhs_raw, builder.current_block));
        builder.terminate(Terminator::Jump(merge_block));
    } else {
        builder.discard_scope();
    }

    // --- short-circuit block: yield the constant that the LHS already determined ---
    builder.switch_to(short_block);
    // `false && _` → false; `true || _` → true.
    let short_val = builder.const_temp(Const::Bool(matches!(op, BinOp::Or)));
    incomings.push((short_val, builder.current_block));
    builder.terminate(Terminator::Jump(merge_block));

    builder.switch_to(merge_block);
    builder.emit(Instruction::Phi {
        dst: result_dst,
        ty: Type::Bool,
        incomings,
    });
    result_dst
}

// -------------------------------------------------------------------------
// Union-variant discrimination (perf, closed-concrete-union fast path)
//
// `is V` over an object type normally lowers to a RECURSIVE `MatchesSchema`
// runtime call that re-validates every field of V (and nested types). That is
// REDUNDANT precisely when the scrutinee's STATIC TYPE is a *closed concrete
// union* — `Type::Union(variants)` (optionally with Null) where every non-Null
// variant resolves to a concrete `Type::Object` containing NO TypeVar/Json
// anywhere. In that case the value is already type-guaranteed to conform to
// exactly one variant, so `is V` only needs to DISTINGUISH V from its siblings,
// not re-validate V's fields.
//
// SOUNDNESS: this is sound IFF the scrutinee really is such a closed concrete
// union. For `Json`/`TypeVar`/any-typevar/open-or-non-object unions, the full
// `MatchesSchema` is mandatory (ADR-036 narrowing depends on it). When in doubt
// we return `None` and the caller emits the existing `MatchesSchema`.
// -------------------------------------------------------------------------

/// A proven-minimal discriminator that uniquely selects a target variant among
/// the sibling variants of a closed concrete union.
///
/// NOTE: ONLY a `StrLit` value comparison is sound here. Field-PRESENCE is NOT,
/// because Lin objects are structurally width-subtyped: a value conforming to a
/// SIBLING variant may legally carry the discriminating field as an EXTRA field
/// (verified: a `{kind,op,left,right,value}` literal type-checks as the BinOp
/// variant of `Num | BinOp`). Presence-of-`value` would then misclassify that
/// BinOp value as Num and the narrowed arm would read its `value` at the wrong
/// type — the exact silent-wrong-field-read the recursive `MatchesSchema` exists
/// to prevent. A StrLit discriminant forces the variants to be genuinely DISJOINT
/// (no value can carry two distinct StrLit values at the same key), so a value in
/// the union with `key == value` cannot inhabit any sibling — making the cheap
/// equality test exactly equivalent to the recursive validation. See report.
enum Discriminator {
    /// `scrut[key] == value` — the target's `key` field is a `StrLit(value)` and
    /// every sibling's `key` is a StrLit with a DISTINCT value.
    StrLit { key: String, value: String },
}

/// Resolve a (possibly `Named`) type to its concrete `Object` field map using
/// the resolved `named_defs` bodies carried by `TypeCheckDeep`. Returns `None`
/// for any non-object, or a Named not in `named_defs`.
fn resolve_object_fields<'a>(
    ty: &'a Type,
    named_defs: &'a [(String, Type)],
) -> Option<&'a indexmap::IndexMap<String, Type>> {
    resolve_object_fields_bounded(ty, named_defs, 0)
}

fn resolve_object_fields_bounded<'a>(
    ty: &'a Type,
    named_defs: &'a [(String, Type)],
    depth: usize,
) -> Option<&'a indexmap::IndexMap<String, Type>> {
    // A bare alias chain `A = B = …` is finite; cap depth to defend against a
    // pathological cyclic alias (`A = B; B = A`) rather than recursing forever.
    if depth > 64 {
        return None;
    }
    match ty {
        Type::Object { fields, .. } => Some(fields),
        Type::Named(n) => {
            let body = &named_defs.iter().find(|(k, _)| k == n)?.1;
            resolve_object_fields_bounded(body, named_defs, depth + 1)
        }
        _ => None,
    }
}

/// Unfold a `Named` alias chain to the type it points at (typically a `Union`),
/// using the resolved `named_defs` bodies. Returns the input unchanged for any
/// non-`Named` type or an unresolvable/cyclic `Named`. Bounded against alias
/// cycles.
fn resolve_union_alias<'a>(ty: &'a Type, named_defs: &'a [(String, Type)], depth: usize) -> &'a Type {
    if depth > 64 {
        return ty;
    }
    match ty {
        Type::Named(n) => match named_defs.iter().find(|(k, _)| k == n) {
            Some((_, body)) => resolve_union_alias(body, named_defs, depth + 1),
            None => ty,
        },
        _ => ty,
    }
}

/// Resolve `ty` through `Named` bodies for a contains-type-var check. Returns
/// `true` if the resolved type contains any TypeVar/Json (so NOT fast-path-safe),
/// or references a `Named` we cannot resolve (conservative). `seen` guards against
/// cyclic recursive types (e.g. `Expr = Num | Add` where `Add` references `Expr`):
/// a `Named` already on the resolution stack is a safe cycle, not a TypeVar.
fn variant_has_type_var(ty: &Type, named_defs: &[(String, Type)], seen: &mut Vec<String>) -> bool {
    match ty {
        Type::Named(n) => {
            if seen.contains(n) {
                // Cyclic recursive reference — already being resolved; not a TypeVar.
                return false;
            }
            match named_defs.iter().find(|(k, _)| k == n) {
                Some((_, body)) => {
                    seen.push(n.clone());
                    let r = variant_has_type_var(body, named_defs, seen);
                    seen.pop();
                    r
                }
                // A Named we couldn't resolve — be conservative.
                None => true,
            }
        }
        Type::Object { fields, .. } => fields.values().any(|t| variant_has_type_var(t, named_defs, seen)),
        Type::Array(inner) | Type::Iterator(inner) | Type::Stream(inner) => {
            variant_has_type_var(inner, named_defs, seen)
        }
        Type::FixedArray(elems) => elems.iter().any(|t| variant_has_type_var(t, named_defs, seen)),
        Type::Union(vs) => vs.iter().any(|t| variant_has_type_var(t, named_defs, seen)),
        Type::Function { params, ret, .. } => {
            params.iter().any(|t| variant_has_type_var(t, named_defs, seen))
                || variant_has_type_var(ret, named_defs, seen)
        }
        Type::TypeVar(_) => true,
        _ => false,
    }
}

/// Decide whether `is target` over a scrutinee of static type `scrut_ty` may use
/// the cheap discriminator fast path, and if so which discriminator. Returns
/// `None` (→ caller emits `MatchesSchema`) whenever the fast path is not PROVEN
/// sound or no single-field-class discriminator uniquely selects `target`.
fn union_discriminator(
    scrut_ty: &Type,
    target: &Type,
    named_defs: &[(String, Type)],
) -> Option<Discriminator> {
    // The scrutinee must be a (closed) union — possibly behind a `Named` alias
    // (e.g. `type Ast = Num | BinOp`; `e: Ast` carries `Type::Named("Ast")`).
    // Unfold the alias through `named_defs` to find the underlying union.
    let resolved_scrut = resolve_union_alias(scrut_ty, named_defs, 0);
    // Strip a Null member.
    let variants: Vec<&Type> = match resolved_scrut {
        Type::Union(vs) => vs.iter().filter(|v| !matches!(v, Type::Null)).collect(),
        _ => return None,
    };
    if variants.len() < 2 {
        return None;
    }
    // EVERY non-Null variant must resolve to a concrete object with no TypeVar.
    let mut resolved: Vec<&indexmap::IndexMap<String, Type>> = Vec::with_capacity(variants.len());
    for v in &variants {
        if variant_has_type_var(v, named_defs, &mut Vec::new()) {
            return None;
        }
        match resolve_object_fields(v, named_defs) {
            Some(fields) if !fields.is_empty() => resolved.push(fields),
            // A non-object variant (or empty object) means we cannot prove the
            // value already conforms to an object-shaped variant → keep full.
            _ => return None,
        }
    }
    // The target must be a concrete object.
    if variant_has_type_var(target, named_defs, &mut Vec::new()) {
        return None;
    }
    let target_fields = resolve_object_fields(target, named_defs)?;

    // The ONLY sound discriminator: a field F where the target's F is a `StrLit`
    // and EVERY variant of the union carries F as a `StrLit`, with EXACTLY ONE
    // variant's F equal to the target's value (the rest distinct). Requiring a
    // distinct StrLit on every variant at F makes the variants genuinely disjoint
    // there, so `scrut[F] == tval` ⇔ "conforms to the target". (This is identified
    // purely by the discriminant VALUES, independent of how the variants' other
    // fields are represented — `Named("Expr")` vs an expanded `Union` for a
    // recursive type — so a recursive AST union is handled correctly.)
    //
    // NOTE: field-PRESENCE is deliberately NOT a discriminator here: Lin objects
    // are structurally width-subtyped, so a value conforming to a sibling variant
    // may legally carry the discriminating field as an EXTRA field, which would
    // misclassify it. Only a value-equality on a StrLit field is sound.
    'keys: for (key, tty) in target_fields.iter() {
        let Type::StrLit(tval) = tty else { continue };
        let mut eq_count = 0usize;
        for var in &resolved {
            match var.get(key) {
                Some(Type::StrLit(sval)) => {
                    if sval == tval {
                        eq_count += 1;
                    }
                }
                // A variant lacks the key, or carries a non-StrLit (base String)
                // there: `scrut[key] == tval` is not provably exclusive of it.
                _ => continue 'keys,
            }
        }
        if eq_count == 1 {
            return Some(Discriminator::StrLit { key: key.clone(), value: tval.clone() });
        }
    }

    // No SOUND discriminator. Fall back to the full recursive `MatchesSchema`.
    None
}

/// Emit the IR for a chosen `Discriminator` over the boxed scrutinee `scrut`,
/// returning the Bool result temp. Reuses the existing `Index`+`Eq` machinery —
/// no new instructions.
fn emit_discriminator(
    disc: &Discriminator,
    scrut: Temp,
    scrut_ty: &Type,
    builder: &mut FuncBuilder,
) -> Temp {
    match disc {
        Discriminator::StrLit { key, value } => {
            // UNBOXED SUM TYPE (unboxed-sumtype Stage 1): when the scrutinee's static type is a
            // Stage-1-eligible sum type, it is physically a `SumNode` (the seed). Replace the boxed
            // `scrut[disc] == "value"` (materialize + object_get + string-eq) with a single inline-tag
            // compare (`SumTagEq`) — the O(1) dispatch the unboxed representation exists for. The
            // resolved scrutinee type may be behind a `Named` alias; `crate::repr::sum_type_eligible`
            // only matches a bare `Union`, so unfold one level via the discriminator's own key check.
            let sum_view = if crate::repr::sum_type_eligible(scrut_ty) {
                Some(scrut_ty.clone())
            } else {
                None
            };
            if let Some(sum_ty) = sum_view {
                let dst = builder.alloc_temp(Type::Bool);
                builder.emit(Instruction::SumTagEq {
                    dst,
                    val: scrut,
                    sum_ty,
                    disc_value: value.clone(),
                });
                return dst;
            }
            builder.push_scope();
            // got = scrut[key]
            let key_temp = builder.const_temp(Const::Str(key.clone()));
            let got = builder.alloc_temp(Type::TypeVar(u32::MAX));
            builder.emit(Instruction::Index {
                dst: got,
                object: scrut,
                key: key_temp,
                obj_ty: scrut_ty.clone(),
                key_ty: Type::Str,
                result_ty: Type::TypeVar(u32::MAX),
            });
            // lit = box("value")
            let lit_raw = builder.const_temp(Const::Str(value.clone()));
            let lit = box_to_json(lit_raw, &Type::Str, builder);
            let eq = builder.alloc_temp(Type::Bool);
            builder.emit(Instruction::Binary {
                dst: eq,
                op: BinOp::Eq,
                lhs: got,
                rhs: lit,
                operand_ty: Type::TypeVar(u32::MAX),
                ty: Type::Bool,
            });
            // `eq` is a Bool (not RC) so it survives; transient RC temps (fetched
            // field, boxed literal) are released in this scope.
            builder.pop_scope_releasing(Temp(u32::MAX));
            eq
        }
    }
}

// -------------------------------------------------------------------------
// Match lowering
// -------------------------------------------------------------------------

fn lower_match(
    scrutinee: &TypedExpr,
    arms: &[TypedMatchArm],
    result_type: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Temp {
    let scrut_ty = scrutinee.ty();
    let raw_scrut = lower_expr(scrutinee, builder, ctx);
    // `is`/`has` pattern tests use runtime tag dispatch (lin_get_tag), which needs a
    // boxed TaggedVal*. Box a concrete scrutinee so type checks see a real tag.
    let scrut_temp = box_to_json(raw_scrut, &scrut_ty, builder);
    let merge_block = builder.alloc_block("match_merge");
    let result_dst = builder.alloc_temp(result_type.clone());
    // Collect (arm_result, predecessor_block) for a Phi in the merge block — a shared
    // Copy target would be overwritten per-arm by the single-pass codegen.
    let mut incomings: Vec<(Temp, BlockId)> = Vec::new();

    for (i, arm) in arms.iter().enumerate() {
        let is_last = i == arms.len() - 1;
        let body_block = builder.alloc_block(format!("arm_{}_body", i));
        // Tag the arm body block with its source span for coverage. next/nofall blocks
        // cover no distinct source region and stay None.
        builder.set_block_span(body_block, arm.body.span());
        let next_block = if is_last {
            // Last arm: no fallthrough needed (compiler ensures exhaustiveness).
            builder.alloc_block("arm_nofall")
        } else {
            builder.alloc_block(format!("arm_{}_next", i))
        };

        // Test the pattern. `scrut_ty` is threaded so a closed-concrete-union
        // scrutinee can take the cheap discriminator fast path for an `is V` arm.
        let matched = lower_match_pattern(&arm.pattern, scrut_temp, &scrut_ty, &arm.body, builder, ctx);

        match matched {
            PatternTest::Always => {
                // Unconditional match (else arm or wildcard).
                builder.terminate(Terminator::Jump(body_block));
            }
            PatternTest::Cond(cond_temp) => {
                builder.terminate(Terminator::CondJump {
                    cond: cond_temp,
                    then_block: body_block,
                    else_block: next_block,
                });
            }
        }

        // Emit body. Each arm gets its own ownership scope so heap temps it allocates
        // (bindings, body intermediates) are released within the arm — not at the
        // enclosing scope exit, where only one arm actually executed (releasing another
        // arm's temps there frees an undefined value / breaks SSA dominance).
        builder.switch_to(body_block);
        builder.push_scope();

        // Bind pattern variables BEFORE the guard — the guard may reference them
        // (e.g. `has { name, age } when age > 30`).
        lower_match_bindings(&arm.pattern, scrut_temp, builder, ctx);

        // If there's a guard, test it. On failure, discard this arm's scope (its bindings
        // are unused) and fall through to the next arm.
        if let Some(guard) = &arm.guard {
            let guard_val = lower_expr(guard, builder, ctx);
            let guard_then = builder.alloc_block(format!("arm_{}_guard_ok", i));
            let guard_fail = builder.alloc_block(format!("arm_{}_guard_fail", i));
            // The guard-ok block is reached only when the guard expression evaluated true,
            // so it is a distinct coverage region. guard_fail stays None.
            builder.set_block_span(guard_then, guard.span());
            builder.terminate(Terminator::CondJump {
                cond: guard_val,
                then_block: guard_then,
                else_block: guard_fail,
            });
            builder.switch_to(guard_fail);
            builder.terminate(Terminator::Jump(next_block));
            builder.switch_to(guard_then);
        }

        // UNBOXED SUM TYPE (unboxed-sumtype Stage 2): a sum-eligible object LITERAL arm body
        // flowing into a sum-typed match result is constructed DIRECTLY as a SumNode (with the
        // recursive-child pushdown), mirroring the `if`-branch path — skip the
        // build-boxed-then-project round-trip that mis-tags recursive children.
        let (arm_raw, arm_ty) = match try_lower_sum_literal(&arm.body, result_type, builder, ctx) {
            Some(t) => (t, result_type.clone()),
            None => (lower_expr(&arm.body, builder, ctx), arm.body.ty()),
        };
        if !builder.is_current_block_terminated() {
            let arm_val = coerce_to_slot_type(arm_raw, &arm_ty, result_type, builder);
            // If an arm returns the scrutinee itself (e.g. `match x is {..} => x`), the match
            // result aliases the scrutinee temp. The scrutinee is owned by an ENCLOSING scope
            // (it's a val/expr lowered before the match); transferring it into the match result
            // (also registered owned at the merge) would double-own it → the enclosing
            // scope-exit release frees the still-live result. Drop it from the enclosing scope
            // so exactly one owner (the match result) remains.
            if arm_val == scrut_temp || arm_raw == scrut_temp || arm_val == raw_scrut || arm_raw == raw_scrut {
                builder.unregister_owned(scrut_temp);
                builder.unregister_owned(raw_scrut);
            }
            // Release this arm's owned temps, keeping the result and its raw pre-coercion temp.
            builder.pop_scope_releasing_keep(&[arm_val, arm_raw]);
            incomings.push((arm_val, builder.current_block));
            builder.terminate(Terminator::Jump(merge_block));
        } else {
            builder.discard_scope();
        }

        builder.switch_to(next_block);
    }

    // If we fall off the last arm without matching, emit a panic.
    let panic_msg = builder.const_temp(Const::Str("non-exhaustive match".to_string()));
    builder.emit(Instruction::Panic { msg: panic_msg });
    builder.terminate(Terminator::Unreachable);

    builder.switch_to(merge_block);
    // Merge the arm results via a Phi (see lower_if). If no arm fell through to the merge
    // (all diverged), the phi has no incomings — still valid as the merge is unreachable.
    builder.emit(Instruction::Phi {
        dst: result_dst,
        ty: result_type.clone(),
        incomings,
    });
    // Only CONCRETE rc merge results are owned (see lower_if): a boxed-union match-result may be
    // a borrowed arm value (carrying no +1), so registering+releasing it would double-free.
    if is_rc_type(result_type) {
        builder.register_owned(result_dst, result_type.clone());
    }
    result_dst
}

enum PatternTest {
    Always,
    Cond(Temp),
}

fn lower_match_pattern(
    pattern: &TypedMatchPattern,
    scrut: Temp,
    scrut_ty: &Type,
    _body: &TypedExpr,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> PatternTest {
    match pattern {
        TypedMatchPattern::Else => PatternTest::Always,
        // A literal pattern matches by VALUE, not type: compare the scrutinee to the
        // literal (e.g. `"yes" => ...` must only match the string "yes", not every string).
        TypedMatchPattern::Is(TypedPattern::Literal(lit)) => {
            let lit_ty = lit.ty();
            let lit_raw = lower_expr(lit, builder, ctx);
            // Box the literal to Json so both operands are TaggedVal* for lin_tagged_eq
            // (the scrutinee is already boxed).
            let lit_temp = box_to_json(lit_raw, &lit_ty, builder);
            let dst = builder.alloc_temp(Type::Bool);
            builder.emit(Instruction::Binary {
                dst,
                op: BinOp::Eq,
                lhs: scrut,
                rhs: lit_temp,
                operand_ty: Type::TypeVar(u32::MAX),
                ty: Type::Bool,
            });
            PatternTest::Cond(dst)
        }
        // Array pattern (`is []`, `is [a, b]`, `is [x, ...rest]`): the value must be an
        // array of the right length (exact, or >= when a rest binding is present).
        TypedMatchPattern::Is(TypedPattern::Array { elements, rest, .. }) => {
            let dst = builder.alloc_temp(Type::Bool);
            builder.emit(Instruction::ArrayLenCheck {
                dst,
                val: scrut,
                n: elements.len() as u64,
                at_least: rest.is_some(),
            });
            PatternTest::Cond(dst)
        }
        // Object pattern (`is { "type": "error", "message": _ }`): the value must be an
        // object that HAS the listed fields, with any value-constrained fields matching.
        // This mirrors the `has { .. }` object handling below. The generic `Is(tp)` arm's
        // bare `IsType` is wrong here — `pattern_type_check` maps an object pattern to
        // `Type::Never`, whose tag constant is 0xFF, so the tag check would never match.
        TypedMatchPattern::Is(tp @ TypedPattern::Object { .. }) => {
            lower_object_pattern_test(tp, scrut, builder, ctx)
        }
        // `is <name>` (a binding) and `is _` (wildcard) match ANY value unconditionally —
        // they are named/anonymous catch-alls, not type checks. The generic arm below would
        // call pattern_type_check, which returns the binding's declared type (= the
        // scrutinee's static type, often Json) and emit an `IsType` tag check that can fail
        // for a concrete value inside a Json scrutinee (e.g. `match req["path"] is p when …`
        // never matched). Bindings always match; the value is bound in lower_match_bindings.
        TypedMatchPattern::Is(TypedPattern::Binding(..))
        | TypedMatchPattern::Is(TypedPattern::Wildcard(..)) => PatternTest::Always,
        // `is <Named>` where the name resolves to a non-empty object shape (a user object-type
        // alias like `Person`): a bare tag check (or the mere field-presence the earlier rule
        // folded into ADR-036 checked) matches
        // objects with the WRONG field types, which is unsound once the arm narrows the binding.
        // Deep-validate field types recursively via the `fromJson` structural walker (ADR-036).
        // `scrut` is the already-boxed scrutinee; `MatchesSchema` borrows it (no ownership change).
        TypedMatchPattern::Is(TypedPattern::TypeCheckDeep(target, named_defs, _)) => {
            // FAST PATH: when the scrutinee's static type is a closed concrete union
            // the value is type-guaranteed to conform to exactly one variant, so a
            // cheap discriminator (StrLit field value / field presence) suffices to
            // SELECT V — the recursive `MatchesSchema` re-validation is redundant.
            // Falls back to `MatchesSchema` whenever the fast path isn't proven sound.
            if let Some(disc) = union_discriminator(scrut_ty, target, named_defs) {
                let cond = emit_discriminator(&disc, scrut, scrut_ty, builder);
                return PatternTest::Cond(cond);
            }
            let dst = builder.alloc_temp(Type::Bool);
            builder.emit(Instruction::MatchesSchema {
                dst,
                val: scrut,
                target: target.clone(),
                named_defs: named_defs.clone(),
            });
            PatternTest::Cond(dst)
        }
        TypedMatchPattern::Is(tp) => {
            let (check_ty, _) = pattern_type_check(tp);
            let dst = builder.alloc_temp(Type::Bool);
            builder.emit(Instruction::IsType {
                dst,
                val: scrut,
                ty: check_ty,
            });
            PatternTest::Cond(dst)
        }
        // `has [a, ...rest]`: array shape check — value is an array with at least the
        // listed elements (rest ⇒ at-least, else exact).
        TypedMatchPattern::Has(TypedPattern::Array { elements, rest, .. }) => {
            let dst = builder.alloc_temp(Type::Bool);
            builder.emit(Instruction::ArrayLenCheck {
                dst,
                val: scrut,
                n: elements.len() as u64,
                at_least: rest.is_some(),
            });
            PatternTest::Cond(dst)
        }
        TypedMatchPattern::Has(tp) => lower_object_pattern_test(tp, scrut, builder, ctx),
    }
}

/// Lower an object pattern test (`is`/`has { k: v, .. }`): the scrutinee must be an object
/// that HAS the listed fields, with each value-constrained field equal to its literal. Used
/// by both `Is(Object)` and `Has(Object)` — for an object shape check the two are equivalent
/// (tag-is-object + required fields + value constraints).
fn lower_object_pattern_test(
    tp: &TypedPattern,
    scrut: Temp,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> PatternTest {
    let required_fields = pattern_required_fields(tp);
    let mut cond = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::HasPattern {
        dst: cond,
        val: scrut,
        pattern: HasDesc { required_fields },
    });
    // For object fields with a value constraint (e.g. `{ "type": "success" }`), also require
    // scrut[key] == literal, AND-ed into the condition. The transient comparison temps (boxed
    // literal, fetched field) are scoped so they're released in THIS test block — not at the
    // enclosing scope exit, which a per-arm test block does not dominate.
    if let TypedPattern::Object { fields, .. } = tp {
        let scrut_ty = builder.temp_types.get(&scrut).cloned().unwrap_or(Type::TypeVar(u32::MAX));
        builder.push_scope();
        for field in fields {
            if let Some(vp) = &field.value_pattern {
                let lit_ty = vp.ty();
                let lit_raw = lower_expr(vp, builder, ctx);
                let lit = box_to_json(lit_raw, &lit_ty, builder);
                // got = scrut[key]
                let key_temp = builder.const_temp(Const::Str(field.key.clone()));
                let got = builder.alloc_temp(Type::TypeVar(u32::MAX));
                builder.emit(Instruction::Index {
                    dst: got, object: scrut, key: key_temp,
                    obj_ty: scrut_ty.clone(), key_ty: Type::Str, result_ty: Type::TypeVar(u32::MAX),
                });
                let eq = builder.alloc_temp(Type::Bool);
                builder.emit(Instruction::Binary {
                    dst: eq, op: BinOp::Eq, lhs: got, rhs: lit,
                    operand_ty: Type::TypeVar(u32::MAX), ty: Type::Bool,
                });
                let combined = builder.alloc_temp(Type::Bool);
                builder.emit(Instruction::Binary {
                    dst: combined, op: BinOp::And, lhs: cond, rhs: eq,
                    operand_ty: Type::Bool, ty: Type::Bool,
                });
                cond = combined;
            }
        }
        // `cond` is a Bool (not RC), so it survives; only the transient RC temps
        // (literal strings, fetched fields) are released here.
        builder.pop_scope_releasing(Temp(u32::MAX));
    }
    PatternTest::Cond(cond)
}

/// After a pattern test succeeds, bind pattern variables into slots.
fn lower_match_bindings(
    pattern: &TypedMatchPattern,
    scrut: Temp,
    builder: &mut FuncBuilder,
    _ctx: &mut LowerCtx,
) {
    let typed_pattern = match pattern {
        TypedMatchPattern::Is(tp) | TypedMatchPattern::Has(tp) => tp,
        TypedMatchPattern::Else => return,
    };
    lower_typed_pattern_bindings(typed_pattern, scrut, builder);
}

fn lower_typed_pattern_bindings(
    pattern: &TypedPattern,
    scrut: Temp,
    builder: &mut FuncBuilder,
) {
    match pattern {
        TypedPattern::Binding(slot, ty, _) => {
            // The match scrutinee is boxed to Json/union (`box_to_json` at match entry).
            // If this binding has a CONCRETE type (e.g. `is n` where n: Int32), binding it
            // directly to the boxed pointer would later reinterpret the pointer as the
            // scalar (ptrtoint) — so a guard like `when n > 5` compares a heap address, not
            // the value, and is effectively always true. Unbox via Coerce when the
            // scrutinee is boxed but the binding is concrete; a plain Bind (alias) is
            // correct when types already match (e.g. a Json scrutinee bound to Json).
            let scrut_ty = builder.temp_types.get(&scrut).cloned().unwrap_or(Type::TypeVar(u32::MAX));
            let t = builder.alloc_temp(ty.clone());
            if is_union_ty(&scrut_ty) && !is_union_ty(ty) {
                builder.emit(Instruction::Coerce {
                    dst: t, src: scrut, from_ty: scrut_ty, to_ty: ty.clone(),
                });
            } else {
                builder.emit(Instruction::Bind { dst: t, src: scrut, ty: ty.clone() });
            }
            builder.slots.insert(*slot, t);
        }
        TypedPattern::Object { fields, .. } => {
            let scrut_ty = builder.temp_types.get(&scrut).cloned().unwrap_or(Type::TypeVar(u32::MAX));
            for field in fields {
                if let Some(slot) = field.binding_slot {
                    let t = builder.alloc_temp(field.ty.clone());
                    builder.emit(Instruction::FieldGet {
                        dst: t,
                        object: scrut,
                        field: field.key.clone(),
                        obj_ty: scrut_ty.clone(),
                        result_ty: field.ty.clone(),
                    });
                    builder.slots.insert(slot, t);
                }
            }
        }
        TypedPattern::Array { elements, rest, .. } => {
            // The scrutinee's static type (often Json/union for match arms) drives whether
            // codegen must unbox it before indexing.
            let scrut_ty = builder.temp_types.get(&scrut).cloned().unwrap_or(Type::TypeVar(u32::MAX));
            for (i, elem_pat) in elements.iter().enumerate() {
                let idx_temp = builder.const_temp(Const::Int(i as i64, Type::Int64));
                // We need the element type; infer from the pattern.
                let elem_ty = pattern_elem_type(elem_pat);
                let elem_t = builder.alloc_temp(elem_ty.clone());
                builder.emit(Instruction::Index {
                    dst: elem_t,
                    object: scrut,
                    key: idx_temp,
                    obj_ty: scrut_ty.clone(),
                    key_ty: Type::Int64,
                    result_ty: elem_ty,
                });
                lower_typed_pattern_bindings(elem_pat, elem_t, builder);
            }
            // `...rest` binds the remaining elements as a new array (slice from N onward).
            if let Some(rest_slot) = rest {
                let rest_ty = Type::Array(Box::new(Type::TypeVar(u32::MAX)));
                let start = builder.const_temp(Const::Int(elements.len() as i64, Type::Int64));
                // scrut is a boxed Json array; unbox to a raw array for length + slicing.
                let arr_raw = builder.alloc_temp(rest_ty.clone());
                builder.emit(Instruction::Coerce {
                    dst: arr_raw, src: scrut, from_ty: scrut_ty.clone(), to_ty: rest_ty.clone(),
                });
                let len = builder.alloc_temp(Type::Int64);
                builder.emit(Instruction::CallIntrinsic {
                    dst: len, intrinsic: Intrinsic::Length, args: vec![arr_raw], ret_ty: Type::Int64,
                });
                let dst = builder.alloc_temp(rest_ty.clone());
                builder.emit(Instruction::Call {
                    dst,
                    callee: CallTarget::Named("lin_array_slice_tagged".to_string()),
                    args: vec![arr_raw, start, len],
                    ret_ty: rest_ty.clone(),
                });
                builder.register_owned(dst, rest_ty.clone());
                builder.slots.insert(*rest_slot, dst);
            }
        }
        TypedPattern::TypeCheck(_, _)
        | TypedPattern::TypeCheckDeep(_, _, _)
        | TypedPattern::Literal(_)
        | TypedPattern::Wildcard(_) => {
            // No bindings.
        }
    }
}

fn pattern_elem_type(pattern: &TypedPattern) -> Type {
    match pattern {
        TypedPattern::Binding(_, ty, _) => ty.clone(),
        TypedPattern::TypeCheck(ty, _) => ty.clone(),
        TypedPattern::TypeCheckDeep(ty, _, _) => ty.clone(),
        _ => Type::Null,
    }
}

// -------------------------------------------------------------------------
// Default-argument adapters
// -------------------------------------------------------------------------

/// If `params` carry any defaults, pre-assign a FuncId + symbol for each shortfall arity
/// `required ..total` and queue an `AdapterSpec` to be lowered after the main pass. `real_fid`
/// is the real function's id; `real_slot` is its binding slot (so the adapter body can issue a
/// `Direct` call through `global_fn_slots`). Returns immediately if there are no defaults.
fn register_default_adapters(
    real_fid: FuncId,
    real_slot: usize,
    real_symbol_prefix: &str,
    params: &[TypedParam],
    ret_type: &Type,
    span: Span,
    ctx: &mut LowerCtx,
) {
    let total = params.len();
    let required = params.iter().filter(|p| p.default.is_none()).count();
    if required == total {
        return; // no optional parameters
    }
    // Idempotent: a pre-scan may register a function's adapters before its body is lowered, so the
    // per-`Val` call in `lower_stmt` must not register them a second time (which would mint a
    // duplicate adapter FuncId/symbol and emit two LLVM definitions of the same `$default` symbol).
    if ctx.default_descriptors.contains_key(&real_fid) {
        return;
    }
    let real_fn_ty = Type::Function {
        params: params.iter().map(|p| p.ty.clone()).collect(),
        ret: Box::new(ret_type.clone()),
        required,
    };
    // A monomorphized spec of a generic optional-default function can be REPRESENTATIONALLY
    // INVALID to default-fill: `at = <T, D>(…, default: D = null)` instantiated at `D = Int32`
    // (the SUPPLIED-arg call `at(ints, i, 0)`) yields a spec whose `default` param is `Int32`,
    // but its default VALUE is `null` (type `Null`, a boxed `ptr`). An adapter that binds the
    // `Int32` param to `null` emits an `i32`-param call passed a `ptr` — an LLVM ABI mismatch.
    // Such an adapter is also DEAD: an OMITTED-arg call binds `D` from the default's type (`Null`),
    // monomorphizing to the DISTINCT `at$…_Null` spec (whose default param IS `Null`-typed and
    // whose adapter is well-formed), never to the concrete-`D` spec. So when ANY defaultable param's
    // monomorphized type cannot hold its default value's representation, we skip the descriptor for
    // this spec entirely: only fully-supplied DIRECT calls reach it, and those need no adapter (no
    // under-arity indirect call to this spec value can occur). Non-generic optional params (e.g.
    // `pad: String = " "`) are unaffected — the default's type matches the param representation.
    // A default whose VALUE type cannot inhabit the (monomorphized) PARAM type. The canonical
    // hazard is a `Null` default into a concrete-scalar param: `Null` is a boxed `ptr`, while the
    // param is an unboxed `i32`/`f64`/string-ptr that a `Null` simply cannot represent. (A `Null`
    // default into a `Null`, union, or `Json` param is fine — the param holds a ptr there.)
    fn default_cannot_inhabit_param(default_ty: &Type, param_ty: &Type) -> bool {
        matches!(default_ty, Type::Null)
            && !matches!(param_ty, Type::Null)
            && !is_union_ty(param_ty)
            && !matches!(param_ty, Type::TypeVar(_))
    }
    let any_default_repr_invalid = params[required..].iter().any(|p| {
        match p.default.as_deref() {
            Some(d) => default_cannot_inhabit_param(&d.ty(), &p.ty),
            None => false,
        }
    });
    if any_default_repr_invalid {
        return;
    }

    // Descriptor entries: one per arity in required..=total. The last (k == total) is the
    // real function itself; the rest are default-fill adapters.
    let mut entries: Vec<FuncId> = Vec::with_capacity(total - required + 1);
    for arity in required..total {
        let adapter_fid = ctx.alloc_func_id();
        let symbol = format!("{}$default{}", real_symbol_prefix, arity);
        ctx.default_adapters.insert((real_fid, arity), adapter_fid);
        entries.push(adapter_fid);
        ctx.pending_adapters.push(AdapterSpec {
            adapter_fid,
            symbol,
            real_slot,
            real_fn_ty: real_fn_ty.clone(),
            params: params.to_vec(),
            arity,
            ret_type: ret_type.clone(),
            span,
        });
    }
    entries.push(real_fid);
    ctx.default_descriptors.insert(real_fid, DefaultDescriptor { required, total, entries });
}

/// Synthesize and lower one default-fill adapter (see `AdapterSpec`). The adapter is built as
/// a `TypedExpr::Function` whose parameters are the first `arity` params (defaults stripped),
/// and whose body is a block that binds each remaining parameter to its default expression and
/// then calls the real function with the full argument list. Reusing `TypedExpr` means the
/// normal lowering path handles RC, coercion, and chained/earlier-param default references.
fn lower_adapter(spec: &AdapterSpec, ctx: &mut LowerCtx) {
    let AdapterSpec { adapter_fid, symbol, real_slot, real_fn_ty, params, arity, ret_type, span } = spec;
    let span = *span;

    // Adapter parameters: the first `arity` real params, defaults removed (they are now
    // mandatory inputs). They reuse the real params' slots so default expressions that
    // reference earlier parameters resolve to the same LocalGet slots.
    let adapter_params: Vec<TypedParam> = params[..*arity]
        .iter()
        .map(|p| TypedParam { slot: p.slot, name: p.name.clone(), ty: p.ty.clone(), default: None })
        .collect();

    // Body block: bind each defaulted param to its default, then call the real function.
    let mut stmts: Vec<TypedStmt> = Vec::new();
    for p in &params[*arity..] {
        let default_expr = p.default.as_ref()
            .expect("optional param must carry a default")
            .as_ref()
            .clone();
        stmts.push(TypedStmt::Val {
            slot: p.slot,
            name: None,
            value: default_expr,
            ty: p.ty.clone(),
            span,
        });
    }

    // Full-arity call to the real function: f(p0, p1, ..., p_{total-1}).
    let real_func = TypedExpr::LocalGet { slot: *real_slot, ty: real_fn_ty.clone(), span };
    let call_args: Vec<TypedExpr> = params
        .iter()
        .map(|p| TypedExpr::LocalGet { slot: p.slot, ty: p.ty.clone(), span })
        .collect();
    let call = TypedExpr::Call {
        func: Box::new(real_func),
        args: call_args,
        result_type: ret_type.clone(),
        // NOT a tail call: the `TailCall` terminator self-jumps to the current function's
        // entry (the adapter), but this call targets the *real* function. Marking it tail
        // would make the adapter loop on itself. A plain Direct call is correct.
        is_tail: false,
        // A full-arity call: never itself a partial application or default-fill.
        partial: false,
        span,
    };
    let body = TypedExpr::Block {
        stmts,
        expr: Box::new(call),
        ty: ret_type.clone(),
        span,
    };

    // Lower through the normal function path under the adapter's forced id and symbol.
    // Adapters never capture (they only reference the real function via global_fn_slots and
    // their own params), so `captures` is empty and the function is non-closure.
    let mut host = FuncBuilder::new(
        ctx.alloc_func_id(), None, vec![], false, Type::Null, ctx.intrinsics.clone(),
    );
    host.push_scope();
    lower_function_expr_with_id(
        Some(*adapter_fid),
        None,
        Some(symbol.as_str()),
        &adapter_params,
        &body,
        ret_type,
        &[],
        &mut host,
        ctx,
    );
    host.discard_scope();
}

/// ADR-046: lower each test `replace` body to a top-level definition under the export's
/// canonical mangled symbol. Function mocks become a `LinFunction` named exactly `{module_key}_
/// {name}`; non-function (val) mocks become a zero-arg `{sym}__val` wrapper. The replaced
/// export's own module skipped emitting that symbol, so this is the sole definition and every
/// caller resolves to it (single LLVM symbol). Run from the MAIN module lowering only.
fn lower_replacements(replacements: &[Replacement], ctx: &mut LowerCtx) {
    for Replacement { sym, is_function, value, ty, span, .. } in replacements {
        let span = *span;
        if *is_function {
            // The export's declared signature drives the emitted function's ABI so callers
            // (which build the call from the import's declared param/ret types) match.
            let (decl_params, decl_ret): (Vec<Type>, Type) = match ty {
                Type::Function { params, ret, .. } => (params.clone(), (**ret).clone()),
                _ => (vec![], Type::Null),
            };
            let fid = ctx.alloc_func_id();
            let mut host = FuncBuilder::new(
                ctx.alloc_func_id(), None, vec![], false, Type::Null, ctx.intrinsics.clone(),
            );
            host.push_scope();
            match value {
                // Primary case: a lambda literal — lower its body directly under the symbol.
                // Params were checked against the export signature, so their types match the
                // declared ABI; we force the declared return type for exactness.
                TypedExpr::Function { params, body, captures, .. } => {
                    lower_function_expr_with_id(
                        Some(fid), None, Some(sym.as_str()),
                        params, body, &decl_ret, captures, &mut host, ctx,
                    );
                }
                // Fallback: any other function-typed expr (e.g. `replace f = otherFn`). Emit a
                // forwarding wrapper with synthetic params that evaluates the expr and calls it.
                other => {
                    // Synthetic param slots in a high range so they never collide with the
                    // checker's slots (referenced only within this generated body).
                    let base = usize::MAX / 2;
                    let synth_params: Vec<TypedParam> = decl_params
                        .iter()
                        .enumerate()
                        .map(|(i, pty)| TypedParam {
                            slot: base + i,
                            name: format!("__rp{}", i),
                            ty: pty.clone(),
                            default: None,
                        })
                        .collect();
                    let call_args: Vec<TypedExpr> = synth_params
                        .iter()
                        .map(|p| TypedExpr::LocalGet { slot: p.slot, ty: p.ty.clone(), span })
                        .collect();
                    let call = TypedExpr::Call {
                        func: Box::new(other.clone()),
                        args: call_args,
                        result_type: decl_ret.clone(),
                        is_tail: false,
                        partial: false,
                        span,
                    };
                    lower_function_expr_with_id(
                        Some(fid), None, Some(sym.as_str()),
                        &synth_params, &call, &decl_ret, &[], &mut host, ctx,
                    );
                }
            }
            host.discard_scope();
        } else {
            // Non-function val mock: emit the `{sym}__val` zero-arg wrapper (mirrors the import
            // export path), so reads of the binding (a `Named` call to the wrapper) hit the mock.
            let fid = ctx.alloc_func_id();
            let wrapper_name = format!("{}__val", sym);
            let mut wb = FuncBuilder::new(
                fid, Some(wrapper_name), vec![], false, ty.clone(), ctx.intrinsics.clone(),
            );
            wb.push_scope();
            let t = lower_expr(value, &mut wb, ctx);
            let t = coerce_to_slot_type(t, &value.ty(), ty, &mut wb);
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
}

// -------------------------------------------------------------------------
// Nested function lowering
// -------------------------------------------------------------------------

fn lower_function_expr(
    name: Option<&str>,
    params: &[TypedParam],
    body: &TypedExpr,
    ret_type: &Type,
    captures: &[Capture],
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Temp {
    lower_function_expr_with_id(None, None, name, params, body, ret_type, captures, builder, ctx)
}

/// Lower a closure that is being passed as a callback argument, forcing its return type to
/// the parameter's declared callback return (so AST-compiled higher-order callees receive a
/// raw value). Only used when that return is a concrete (non-union, non-void) type.
fn lower_callback_arg(
    forced_ret: &Type,
    name: Option<&str>,
    params: &[TypedParam],
    body: &TypedExpr,
    ret_type: &Type,
    captures: &[Capture],
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Temp {
    lower_function_expr_with_id(None, Some(forced_ret.clone()), name, params, body, ret_type, captures, builder, ctx)
}

/// Lower a function literal. `forced_fid` reuses a pre-assigned FuncId (for top-level
/// named functions registered in `global_fn_slots` during the pre-scan, so that
/// `CallTarget::Direct` references resolve to the actually-emitted function); pass
/// None to allocate a fresh id (anonymous/nested closures).
fn lower_function_expr_with_id(
    forced_fid: Option<FuncId>,
    forced_ret: Option<Type>,
    name: Option<&str>,
    params: &[TypedParam],
    body: &TypedExpr,
    ret_type: &Type,
    captures: &[Capture],
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Temp {
    let forced_ret = forced_ret.as_ref();
    let fid = forced_fid.unwrap_or_else(|| ctx.alloc_func_id());

    // Build param temps for the inner function.
    let mut inner_param_count = 0u32;
    let mut inner_params: Vec<(Temp, Type)> = Vec::new();

    // Closure env pointer as first param (if any captures).
    let is_closure = !captures.is_empty();
    if is_closure {
        let env_temp = Temp(inner_param_count);
        inner_param_count += 1;
        inner_params.push((env_temp, Type::Null)); // env pointer; actual type resolved at codegen
    }

    // Explicit parameters.
    let mut slot_to_temp: HashMap<usize, Temp> = HashMap::new();
    for param in params {
        let t = Temp(inner_param_count);
        inner_param_count += 1;
        inner_params.push((t, param.ty.clone()));
        slot_to_temp.insert(param.slot, t);
    }

    let mut inner_builder = FuncBuilder {
        id: fid,
        name: name.map(|s| s.to_string()),
        params: inner_params,
        is_closure,
        ret_ty: ret_type.clone(),
        blocks: Vec::new(),
        current_block: BlockId(0),
        temp_count: inner_param_count,
        temp_types: {
            let mut m = HashMap::new();
            for (t, ty) in &{
                let mut v = if is_closure { vec![(Temp(0), Type::Null)] } else { vec![] };
                for p in params { let t = Temp(v.len() as u32); v.push((t, p.ty.clone())); }
                v
            } {
                m.insert(*t, ty.clone());
            }
            m
        },
        block_counter: 1,
        slots: slot_to_temp,
        intrinsic_slots: builder.intrinsic_slots.clone(),
        scope_owned: Vec::new(),
        diverged_blocks: std::collections::HashSet::new(),
        cell_slots: HashMap::new(),
        created_cells: Vec::new(),
        escaping_cells: std::collections::HashSet::new(),
        escape_alias: HashMap::new(),
    };

    // Add entry block. Tag it with the function body's span so coverage records a
    // region covering the whole function body (the most important coverage region).
    inner_builder.blocks.push(BasicBlock {
        id: BlockId(0),
        label: Some("entry".into()),
        instructions: Vec::new(),
        terminator: Terminator::Unreachable,
        span: Some(body.span()),
    });

    // Add capture slots: captured variables become FieldGet on the env pointer.
    if is_closure {
        let env_temp = Temp(0);
        for (i, cap) in captures.iter().enumerate() {
            // A mutable capture holds a heap-cell POINTER (shared by reference); an
            // immutable one holds the captured value directly.
            let cap_ty = if cap.is_mutable { Type::TypeVar(u32::MAX) } else { cap.ty.clone() };
            let cap_t = inner_builder.alloc_temp(cap_ty.clone());
            // Env access is a raw struct load by index, NOT a Lin object field access.
            inner_builder.emit(Instruction::EnvCapture {
                dst: cap_t,
                env: env_temp,
                index: i as u32,
                ty: cap_ty,
            });
            inner_builder.slots.insert(cap.outer_slot, cap_t);
            if cap.is_mutable {
                // Inside the closure, this slot is a cell: reads/writes go through it.
                // Promote a `Null`-typed cell to `Json` to match the outer MakeCell promotion
                // (see TypedStmt::Var) — otherwise a `found = item` write would coerce the
                // value to Null (storing a null pointer) and reads would always see null.
                let inner_cell_ty = if matches!(cap.ty, Type::Null) { Type::TypeVar(u32::MAX) } else { cap.ty.clone() };
                inner_builder.cell_slots.insert(cap.outer_slot, inner_cell_ty);
            }
        }
    }

    // Push a param scope so Function-typed params are released on exit even when never
    // read inside the body. The caller always retains before passing a Function-typed
    // argument (via retain_call_arg), so the callee owns one reference per Function param
    // that must be released. The body scope below handles LocalGet retains; this param
    // scope handles the initial caller-transferred reference.
    inner_builder.push_scope(); // param scope
    for param in params {
        if matches!(param.ty, Type::Function { .. }) {
            if let Some(&t) = inner_builder.slots.get(&param.slot) {
                inner_builder.register_owned(t, param.ty.clone());
            }
        }
    }
    inner_builder.push_scope(); // body scope
    // Imported-module top-level `var` init: if this is an exported entry point, run the
    // module's once-guarded var initialiser before the body so any `var` it reads/mutates is
    // already set up. `take()` ensures only this top-level body emits the call; nested
    // closures lowered within it (which re-enter this function with the flag already cleared)
    // do not re-run init.
    if let Some(init_sym) = ctx.import_var_init_prologue.take() {
        let dst = inner_builder.alloc_temp(Type::Null);
        inner_builder.emit(Instruction::Call {
            dst,
            callee: CallTarget::Named(init_sym),
            args: vec![],
            ret_ty: Type::Null,
        });
    }
    // RETURN-position sealed-literal fast path (sealed-records Stage 1). When the body IS an object
    // LITERAL whose declared return type is a sealed scalar record, construct the packed struct
    // DIRECTLY (`try_lower_sealed_literal`) instead of lowering a boxed `lin_object_alloc` and then
    // emitting a project-into-sealed `Coerce` at the return site (below). The boxed path left the
    // boxed `LinObject` intermediate ORPHANED — `pop_scope_releasing_keep(&[ret_temp, raw_ret])`
    // keeps `raw_ret` (the box) on the assumption it backs the return value, but the actual return
    // value is the FRESH sealed struct the Coerce materialized, so the box (+ its String field[s])
    // leaked on EVERY call. The fast path produces the sealed struct as `raw_ret` itself (already
    // `register_owned`'d), so there is no box, no return-coercion, and no leak. Only fires when the
    // effective return target IS the sealed record: anonymous closures use the boxed (TypeVar) ABI
    // and so fall through to the boxed path (where the boxed-object result is correct). The
    // `effective_ret` here MUST match the one recomputed below (same inputs, all known pre-body).
    let void_ret_pre = matches!(ret_type, Type::Null | Type::Never);
    let effective_ret_pre = if let Some(fr) = forced_ret {
        fr.clone()
    } else if forced_fid.is_none() && !void_ret_pre {
        Type::TypeVar(u32::MAX)
    } else {
        ret_type.clone()
    };
    // RETURN-position UNBOXED-SUM fast path (unboxed-sumtype Stage 2). When the body IS a sum-type
    // construction LITERAL whose effective return type is a sum type, construct the packed `SumNode`
    // DIRECTLY (`try_lower_sum_literal`, which also pushes the per-variant expected type into each
    // RECURSIVE CHILD field so a nested child literal is built AS a `SumNode` too — design §6 gap 1)
    // instead of lowering a boxed `LinObject` and projecting it at the return site. The boxed path
    // boxed the nested children as plain objects, then stored those boxed-object pointers into the
    // parent node's owned `*SumNode` child slots — reading a child's discriminant then read boxed
    // memory → garbage tag → "non-exhaustive match". This makes a TAIL-RETURN sum literal lower
    // identically to a `val n: <Sum> = {…}; n` binding (which already routes through
    // `lower_value_into_slot` → `try_lower_sum_literal`), closing the tail-return pushdown gap.
    let raw_ret = if crate::repr::sum_type_eligible(&effective_ret_pre) {
        match try_lower_sum_literal(body, &effective_ret_pre, &mut inner_builder, ctx) {
            Some(t) => t,
            None => lower_expr(body, &mut inner_builder, ctx),
        }
    } else if is_sealed_scalar_repr(&effective_ret_pre) {
        match try_lower_sealed_literal(body, &effective_ret_pre, &mut inner_builder, ctx) {
            Some(t) => t,
            None => lower_expr(body, &mut inner_builder, ctx),
        }
    } else {
        lower_expr(body, &mut inner_builder, ctx)
    };
    // Use the lowered temp's ACTUAL type for the return coercion, not the surface
    // `body.ty()`. They can disagree when the body reads a mutably-captured `var` whose
    // declared type was widened by reassignment: e.g. `var found = null; ...; found` has
    // surface type `Null`, but the cell (and the CellGet temp) is `Json`. Trusting the
    // stale `Null` would coerce the live Json value to a boxed null on return.
    let body_ty = inner_builder.temp_types.get(&raw_ret).cloned().unwrap_or_else(|| body.ty());
    // A function result MUST be an OWNED (+1) reference — the uniform call convention has the
    // caller `register_owned` the result and release it at scope exit. A BORROWED union/Json
    // projection (`obj[k]` / `obj.field`) violates this: the lowerer deliberately does NOT own
    // a union projection (`lin_object_get` returns an INTERIOR `*TaggedVal` into the container,
    // not an ownable box — correct for transient in-place use), so if such a value ESCAPES as
    // the body result, the callee hands back the interior pointer and the caller's release
    // double-frees it once the container is released. Clone the borrowed box (`CloneBox` →
    // `lin_tagged_clone`, the established "own a union value" primitive — see `own_for_read`)
    // into a fresh owned +1 box so the result satisfies the convention. Only values NOT already
    // owned in scope are cloned (a fresh alloc, a retained concrete projection, or an
    // already-cloned cell/global read is left untouched — cloning it would leak). The transient
    // read fast path (read a field, use it inline, don't escape) never reaches here, so it is
    // not cloned. Skip when the block diverged (no live result temp).
    let raw_ret = if !inner_builder.is_current_block_terminated()
        && is_union_ty(&body_ty)
        && !inner_builder.is_owned_in_scope(raw_ret)
    {
        let dst = inner_builder.alloc_temp(body_ty.clone());
        inner_builder.emit(Instruction::CloneBox { dst, src: raw_ret, ty: body_ty.clone() });
        inner_builder.register_owned(dst, body_ty.clone());
        dst
    } else {
        raw_ret
    };
    // Closure return ABI:
    // - `forced_ret` (set when this closure is a callback argument whose parameter declares
    //   a concrete return, e.g. groupBy's `keyFn: (Json) => String`): return exactly that
    //   type so AST-compiled higher-order callees, which call back with the declared
    //   signature, get a raw (unboxed) value.
    // - otherwise an ANONYMOUS closure (no pre-assigned FuncId — i.e. not a top-level named
    //   function) uses the uniform boxed (Json) ABI: it is only ever reached through the
    //   closure calling convention (incl. AST `build_closure_call_typed`, which reads the
    //   result's payload at offset 8), so it must always return a boxed TaggedVal*. This
    //   applies even to capture-less closures (which were previously mis-returning raw).
    // - top-level named functions (forced_fid set) keep their declared return — they are
    //   Direct-called with exact signatures.
    // - void (Null/Never) returns stay void.
    let is_anonymous = forced_fid.is_none();
    let void_ret = matches!(ret_type, Type::Null | Type::Never);
    let effective_ret = if let Some(fr) = forced_ret {
        fr.clone()
    } else if is_anonymous && !void_ret {
        Type::TypeVar(u32::MAX)
    } else {
        ret_type.clone()
    };
    // A scalar numeric width change on the return path (e.g. a `Float32` body value returned
    // where the declaration says `Float64`, or a narrower int) is a representation change codegen
    // must materialize via fpext/fptrunc/sext (`compile_ir_coerce`'s numeric-widening arm), just
    // like at a binding/slot store (`coerce_to_slot_type`). Without it the raw (e.g. `float`) value
    // flowed straight into the `Return`/box site and emitted invalid LLVM (a `float` operand where
    // the signature declares `double`). `type_repr_differs` only covers the union/Json box boundary,
    // so the scalar-numeric case is checked separately — mirroring `coerce_to_slot_type`.
    let ret_temp = if !inner_builder.is_current_block_terminated()
        && (type_repr_differs(&body_ty, &effective_ret)
            || scalar_numeric_repr_differs(&body_ty, &effective_ret)
            || flat_scalar_array_repr_differs(&body_ty, &effective_ret))
    {
        let dst = inner_builder.alloc_temp(effective_ret.clone());
        inner_builder.emit(Instruction::Coerce {
            dst, src: raw_ret, from_ty: body_ty.clone(), to_ty: effective_ret.clone(),
        });
        dst
    } else {
        raw_ret
    };
    // Captured-cell cleanup: free PROVABLY-non-escaping cells created in this function body.
    // A cell is freed here only if NO closure capturing it escaped (see the escape analysis at
    // MakeClosure). `FreeCell` releases the cell's owned value (tag-aware/concrete) then frees
    // the cell allocation. Done at the single function-scope exit (this block, before the
    // scope-release Releases and the Return) — never inside the loop that uses it. Skipped when
    // the block already diverged (dead code). A cell pointer is never the return value (returns
    // come from CellGet copies, which `own_for_read` clones/retains independently), but we still
    // exclude ret_temp/raw_ret defensively. Escaping cells (in `escaping_cells`) are left
    // leaking — sound: freeing one would be a use-after-free when a surviving closure reads it.
    if !inner_builder.is_current_block_terminated() {
        let to_free: Vec<(Temp, Type)> = inner_builder
            .created_cells
            .iter()
            // Only free entry-block cells: the entry block dominates this exit block, so the
            // MakeCell dominates the FreeCell (LLVM SSA dominance). A cell created inside a
            // conditional/loop branch is left leaking (sound — see `created_cells` doc).
            .filter(|(c, _, blk)| {
                *blk == BlockId(0)
                    && !inner_builder.escaping_cells.contains(c)
                    && *c != ret_temp
                    && *c != raw_ret
            })
            .map(|(c, ty, _)| (*c, ty.clone()))
            .collect();
        for (cell, ty) in to_free {
            inner_builder.emit(Instruction::FreeCell { cell, ty });
        }
    }
    // Release owned temps in body scope except the return value AND the raw pre-coercion
    // temp: a box (e.g. lin_box_object) shares the underlying pointer, so releasing the
    // original would free what the returned box wraps.
    inner_builder.pop_scope_releasing_keep(&[ret_temp, raw_ret]); // body scope
    // Release Function-typed params that are not being returned. This balances the
    // retain_call_arg retain emitted by every caller for each Function argument.
    inner_builder.pop_scope_releasing_keep(&[ret_temp, raw_ret]); // param scope
    if !inner_builder.is_current_block_terminated() {
        // Void-returning functions must Return(None) — codegen gives them a void LLVM
        // signature, so returning a value would be a type mismatch.
        if void_ret {
            inner_builder.terminate(Terminator::Return(None));
        } else {
            inner_builder.terminate(Terminator::Return(Some(ret_temp)));
        }
    }

    inner_builder.ret_ty = effective_ret;
    let inner_fn = inner_builder.finish();
    ctx.pending_functions.push(inner_fn);

    // In the outer function, emit a MakeClosure instruction.
    //
    // OWNING-CAPTURE MODEL (mirrors the array/object container rule): the closure env owns one
    // reference per heap/union capture, so the closure may safely OUTLIVE the scope that
    // produced the captured value (e.g. a closure returned from a `map` callback into the result
    // array). For each captured value temp:
    //   - a mutably-captured `var` stores the CELL POINTER (shared by reference, ADR-012); the
    //     cell has its own MakeCell/FreeCell/escaping-cell lifecycle, so it stays borrow-only —
    //     do NOT retain it here, and do NOT release it on closure free.
    //   - a concrete-rc value (Str/Array/Object/Function) is retained in place (`Retain`), so the
    //     env holds an independent +1; `lin_closure_release` drops it via the capture descriptor.
    //   - a union/Json value is CLONED (`CloneBox` → `lin_tagged_clone`) so the env owns its OWN
    //     `TaggedVal*` box (never an alias of a borrowed caller box, whose free would double-free
    //     the shared box); `lin_closure_release` frees that owned box.
    // Scalars need no ownership. This is the established store-side discipline (see `own_for_store`
    // / `transfer_into_container`), applied to the closure env.
    let mut capture_temps: Vec<Temp> = Vec::with_capacity(captures.len());
    let mut capture_kinds: Vec<CaptureRelease> = Vec::with_capacity(captures.len());
    for cap in captures {
        let base = builder.slots.get(&cap.outer_slot).copied().unwrap_or_else(|| {
            builder.alloc_temp(cap.ty.clone())
        });
        // Mutable-cell captures (the var-by-reference cell pointer) stay borrow-only — the cell
        // has its own MakeCell/FreeCell/escaping-cell lifecycle, so the env must NOT own it.
        if cap.is_mutable || !needs_owning(&cap.ty) {
            capture_temps.push(base);
            capture_kinds.push(CaptureRelease::None);
            continue;
        }
        if type_is_streamish_ir(&cap.ty) {
            // MOVE capture (streams brief §9): a Stream crosses by MOVE, not copy. Hand the
            // boxed-stream pointer off VERBATIM — NO CloneBox, NO Retain. The env takes the
            // source's reference; the source's scope-exit release is SUPPRESSED (the slot is
            // recorded as moved-out below) so the fd closes exactly once, on the worker that owns
            // the env copy. The affine check (Stage 6) guarantees the source never touches it again.
            capture_temps.push(base);
            capture_kinds.push(CaptureRelease::Move);
            // Suppress the source's scope-exit release of this very temp — its +1 has been
            // transferred to the env. Without this, the source scope AND the worker's
            // `release_env_copy` would both release the same boxed-stream pointer (double-free /
            // double-close). `unregister_owned` is the established "ownership moved out" hook.
            builder.unregister_owned(base);
        } else if is_union_ty(&cap.ty) {
            // Env owns its own boxed TaggedVal*.
            let owned = builder.alloc_temp(cap.ty.clone());
            builder.emit(Instruction::CloneBox { dst: owned, src: base, ty: cap.ty.clone() });
            capture_temps.push(owned);
            capture_kinds.push(CaptureRelease::Tagged);
        } else {
            // Concrete rc: take an independent reference; the env holds the same pointer +1.
            builder.emit(Instruction::Retain { val: base, ty: cap.ty.clone() });
            capture_temps.push(base);
            capture_kinds.push(match &cap.ty {
                Type::Str | Type::StrLit(_) => CaptureRelease::Str,
                Type::Array(_) | Type::FixedArray(_) => CaptureRelease::Array,
                // A SEALED scalar record is a packed struct, not a LinObject — its capture must be
                // released via lin_sealed_release_self (CaptureRelease::Sealed), NOT lin_object_release.
                Type::Object { .. } if is_sealed_scalar_repr(&cap.ty) => CaptureRelease::Sealed,
                Type::Object { .. } => CaptureRelease::Object,
                Type::Function { .. } => CaptureRelease::Closure,
                // is_rc_type covers exactly the above; any other owning type is union (handled).
                _ => CaptureRelease::None,
            });
        }
    }

    // Captured-cell escape analysis. A mutably-captured `var` is a heap cell whose pointer is
    // shared into THIS closure's env. The cell is SAFE to free at the creating function's scope
    // exit only if EVERY closure that captures it is consumed synchronously and non-retained
    // (i.e. lowered as a direct callback argument to a known consuming combinator). When this
    // closure is lowered OUTSIDE that context (`safe_callback_depth == 0`) — it is bound,
    // returned, stored, passed to async/worker, or passed to an unknown callee — any cell it
    // captures may outlive the creating function, so we mark it escaping and never free it.
    // Conservative by construction: anything not provably a synchronous combinator callback
    // escapes. (A capture temp that is one of THIS function's created cells is the cell pointer.)
    if ctx.safe_callback_depth == 0 {
        for &cap_t in &capture_temps {
            if builder.created_cells.iter().any(|(c, _, _)| *c == cap_t) {
                builder.escaping_cells.insert(cap_t);
            }
        }
    }

    let closure_ty = Type::Function {
        params: params.iter().map(|p| p.ty.clone()).collect(),
        ret: Box::new(ret_type.clone()),
        required: params.iter().filter(|p| p.default.is_none()).count(),
    };
    let dst = builder.alloc_temp(closure_ty.clone());
    builder.emit(Instruction::MakeClosure {
        dst,
        func: fid,
        captures: capture_temps,
        capture_kinds,
        ret_ty: closure_ty.clone(),
    });
    builder.register_owned(dst, closure_ty);
    dst
}

// -------------------------------------------------------------------------
// String interpolation lowering
// -------------------------------------------------------------------------

fn lower_string_interp(
    parts: &[TypedStringPart],
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Temp {
    if parts.is_empty() {
        return builder.const_temp(Const::Str(String::new()));
    }

    // Start with an empty accumulator.
    let mut acc = builder.const_temp(Const::Str(String::new()));
    // True once `acc` holds a heap-allocated concat result (a +1 we own) rather than the initial
    // empty-string literal. Only then must the final `acc` be registered owned (a bare literal is
    // immortal — registering/releasing it is a harmless no-op, but we keep the bookkeeping honest).
    let mut acc_is_owned = false;

    for part in parts {
        let (part_temp, part_is_owned) = match part {
            TypedStringPart::Literal(s) => (builder.const_temp(Const::Str(s.clone())), false),
            TypedStringPart::Expr(expr) => {
                let val = lower_expr(expr, builder, ctx);
                // Convert to string. `ToString` now returns an OWNED (+1) string for EVERY input
                // type (the codegen Str arm retains; numeric/bool/json arms allocate fresh), so the
                // per-part temp is always a +1 we must release after the concat below.
                let dst = builder.alloc_temp(Type::Str);
                builder.emit(Instruction::CallIntrinsic {
                    dst,
                    intrinsic: Intrinsic::ToString,
                    args: vec![val],
                    ret_ty: Type::Str,
                });
                (dst, true)
            }
        };
        // Concatenate with accumulator. `lin_string_concat` BORROWS both args (copies bytes,
        // returns a fresh +1), so each owned operand must be released afterwards.
        let new_acc = builder.alloc_temp(Type::Str);
        builder.emit(Instruction::CallIntrinsic {
            dst: new_acc,
            intrinsic: Intrinsic::StringConcat,
            args: vec![acc, part_temp],
            ret_ty: Type::Str,
        });
        // Release the consumed per-part `ToString` temp (now uniformly +1; immortal-literal-safe).
        if part_is_owned && acc != part_temp {
            builder.emit(Instruction::Release { val: part_temp, ty: Type::Str });
        }
        // Release the consumed old accumulator (a prior concat result we owned).
        if acc_is_owned && acc != part_temp {
            builder.emit(Instruction::Release { val: acc, ty: Type::Str });
        }
        acc = new_acc;
        acc_is_owned = true;
    }

    // The final interpolation result is a fresh +1 string. Register it owned so the surrounding
    // lowering treats it exactly like any other fresh allocation (MakeArray / call result):
    //   - TRANSIENT use (an index-write KEY `obj["${k}"] = v`, a comparison operand, an unused
    //     `val`) → released at scope exit. This is the leak this fix closes (the runtime container
    //     `set` RETAINS the key, so the interp string's original +1 was never reclaimed).
    //   - MOVE into a consumer (val binding kept, `return`, stored map VALUE) → the existing
    //     transfer/escape machinery (`transfer_into_container`, the function-return keep-set,
    //     `expr_is_fresh_alloc(StringInterp) == true`) transfers this single +1 and unregisters it,
    //     so scope exit does not double-release. Registering here was previously feared to
    //     double-free, but interp results were in fact NEVER registered, so they leaked in EVERY
    //     case (not just transient). Registering makes them uniform with all other owned temps.
    if acc_is_owned {
        builder.register_owned(acc, Type::Str);
    }
    acc
}

// -------------------------------------------------------------------------
// Pattern helpers
// -------------------------------------------------------------------------

fn pattern_type_check(pattern: &TypedPattern) -> (Type, lin_common::Span) {
    match pattern {
        TypedPattern::TypeCheck(ty, span) => (ty.clone(), *span),
        TypedPattern::TypeCheckDeep(ty, _, span) => (ty.clone(), *span),
        TypedPattern::Binding(_, ty, span) => (ty.clone(), *span),
        TypedPattern::Wildcard(span) => (Type::Never, *span),
        TypedPattern::Literal(e) => (e.ty(), e.span()),
        TypedPattern::Object { span, .. } => (Type::Never, *span),
        TypedPattern::Array { span, .. } => (Type::Never, *span),
    }
}

fn pattern_required_fields(pattern: &TypedPattern) -> Vec<String> {
    match pattern {
        TypedPattern::Object { fields, .. } => fields.iter().map(|f| f.key.clone()).collect(),
        _ => vec![],
    }
}

fn stmt_defines_slot(stmt: &TypedStmt, slot: usize) -> bool {
    match stmt {
        TypedStmt::Val { slot: s, .. } => *s == slot,
        TypedStmt::Var { slot: s, .. } => *s == slot,
        TypedStmt::Destructure { obj_slot, fields, .. } => {
            *obj_slot == slot || fields.iter().any(|(_, s, _)| *s == slot)
        }
        _ => false,
    }
}

/// True if the statement REASSIGNS `slot` (a `var x = ...` reassignment, i.e. a `LocalSet`)
/// anywhere within it, including inside nested control flow / sub-expressions. Used by `Block`
/// lowering to decide whether an outer slot's value was mutated inside the block and so must
/// PERSIST after the block — as opposed to a block-local definition, whose binding is restored.
/// Without this, a reassignment of an outer plain-SSA `var` inside a block (e.g. `if c then sts =
/// e`) was reverted to the pre-block temp on block exit, dropping the write (the closure-local-
/// `var`-mutated-in-`if` bug).
fn stmt_reassigns_slot(stmt: &TypedStmt, slot: usize) -> bool {
    match stmt {
        TypedStmt::Val { value, .. } | TypedStmt::Var { value, .. } => expr_reassigns_slot(value, slot),
        TypedStmt::Expr(e) => expr_reassigns_slot(e, slot),
        TypedStmt::Destructure { value, .. } | TypedStmt::ArrayDestructure { value, .. } => {
            expr_reassigns_slot(value, slot)
        }
        TypedStmt::Import { .. } | TypedStmt::ForeignImport { .. } => false,
    }
}

fn expr_reassigns_slot(expr: &TypedExpr, slot: usize) -> bool {
    match expr {
        TypedExpr::LocalSet { slot: s, value, .. } => *s == slot || expr_reassigns_slot(value, slot),
        // A nested function body has its OWN slot namespace; a reassignment of the SAME outer
        // slot inside it is a captured-`var` mutation handled through the cell, not a plain-SSA
        // rebind of the enclosing block's slot — so do not recurse into nested functions here.
        TypedExpr::Function { .. } => false,
        TypedExpr::Block { stmts, expr, .. } => {
            stmts.iter().any(|s| stmt_reassigns_slot(s, slot)) || expr_reassigns_slot(expr, slot)
        }
        TypedExpr::If { cond, then_br, else_br, .. } => {
            expr_reassigns_slot(cond, slot)
                || expr_reassigns_slot(then_br, slot)
                || expr_reassigns_slot(else_br, slot)
        }
        TypedExpr::Match { scrutinee, arms, .. } => {
            expr_reassigns_slot(scrutinee, slot)
                || arms.iter().any(|arm| {
                    arm.guard.as_ref().is_some_and(|g| expr_reassigns_slot(g, slot))
                        || expr_reassigns_slot(&arm.body, slot)
                })
        }
        TypedExpr::Call { func, args, .. } => {
            expr_reassigns_slot(func, slot) || args.iter().any(|a| expr_reassigns_slot(a, slot))
        }
        TypedExpr::BinaryOp { left, right, .. } => {
            expr_reassigns_slot(left, slot) || expr_reassigns_slot(right, slot)
        }
        TypedExpr::UnaryOp { operand, .. } => expr_reassigns_slot(operand, slot),
        TypedExpr::Coerce { expr, .. } => expr_reassigns_slot(expr, slot),
        TypedExpr::MakeArray { elements, .. } => elements.iter().any(|e| expr_reassigns_slot(e, slot)),
        TypedExpr::MakeObject { fields, spreads, .. } => {
            fields.iter().any(|(_, v)| expr_reassigns_slot(v, slot))
                || spreads.iter().any(|s| expr_reassigns_slot(s, slot))
        }
        TypedExpr::Index { object, key, .. } => {
            expr_reassigns_slot(object, slot) || expr_reassigns_slot(key, slot)
        }
        TypedExpr::IndexSet { object, key, value, .. } => {
            expr_reassigns_slot(object, slot)
                || expr_reassigns_slot(key, slot)
                || expr_reassigns_slot(value, slot)
        }
        TypedExpr::FieldGet { object, .. } => expr_reassigns_slot(object, slot),
        TypedExpr::Is { expr, .. } | TypedExpr::Has { expr, .. } => expr_reassigns_slot(expr, slot),
        TypedExpr::StringInterp { parts, .. } => parts.iter().any(|p| {
            if let TypedStringPart::Expr(e) = p {
                expr_reassigns_slot(e, slot)
            } else {
                false
            }
        }),
        _ => false,
    }
}
