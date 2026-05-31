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

/// Entry point: lower a TypedModule to a LinModule.
pub fn lower_module(module: &TypedModule) -> LinModule {
    // Phase 0 monomorphization: materialize concrete copies of single-module generic functions
    // (e.g. `identity$Int32`) and route calls to them BEFORE lowering, so the backend emits
    // native unboxed scalars. The clone is taken only when the module actually has a generic
    // function; ordinary modules skip it entirely and lower byte-for-byte as before.
    let owned: Option<TypedModule> = if crate::monomorphize::module_has_generic_fn(module) {
        let mut m = module.clone();
        crate::monomorphize::monomorphize(&mut m);
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

    LinModule {
        functions: ctx.functions,
        global_fn_slots,
        intrinsics: ctx.intrinsics,
        default_descriptors: ctx.default_descriptors,
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
            ..
        } = stmt
        {
            let fid = ctx.alloc_func_id();
            global_fn_slots.insert(*slot, fid);
            fn_names.insert(fid, format!("{}_{}", module_key, name));
            // Stdlib-internal combinator call (e.g. `map`'s body calls the sibling `for`):
            // record the callback arg index so cells captured by the closure passed to it stay
            // freeable. Restricted to `std/array` so only its trusted combinators qualify.
            if module_key == "std_array" {
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
                lower_function_expr_with_id(
                    Some(fid), None, mangled.as_deref(), params, body, ret_type, captures,
                    &mut host, &mut ctx,
                );
            }
        }
    }
    host.discard_scope();

    // Emit a zero-arg `{module_key}_{name}__val` wrapper for each non-function exported val.
    for stmt in &module.statements {
        if let TypedStmt::Val { value, ty, name: Some(name), .. } = stmt {
            if matches!(value, TypedExpr::Function { .. }) { continue; }
            let fid = ctx.alloc_func_id();
            let wrapper_name = format!("{}_{}__val", module_key, name);
            let mut wb = FuncBuilder::new(
                fid, Some(wrapper_name), vec![], false, ty.clone(), ctx.intrinsics.clone(),
            );
            wb.push_scope();
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
    /// and closures capture the cell pointer (ADR-015).
    mutable_cell_slots: std::collections::HashSet<usize>,
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
            global_val_slots: HashMap::new(),
            default_adapters: HashMap::new(),
            pending_adapters: Vec::new(),
            default_descriptors: HashMap::new(),
            safe_combinator_slots: HashMap::new(),
            safe_callback_depth: 0,
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
        let keep = self.expand_keep_for_escape(&[keep]);
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
        Type::Str | Type::StrLit(_) | Type::Array(_) | Type::FixedArray(_) | Type::Object(_) | Type::Function { .. }
    )
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

/// Collect `var` slots that are mutably captured by any (possibly nested) closure within
/// a statement. Such slots are stored as heap cells shared by reference.
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
fn is_union_ty(ty: &Type) -> bool {
    matches!(ty, Type::Union(_) | Type::TypeVar(_) | Type::Named(_) | Type::Shared(_))
}

/// A concrete heap-allocated value type whose box wraps a refcounted heap pointer
/// (Str/Array/FixedArray/Object/Iterator). Boxing one of these into a Json/union param
/// (via Coerce → `lin_box_str`/`lin_box_array`/`lin_box_object`) allocates a FRESH 16-byte
/// `TaggedVal*` shell whose inner is the (separately owned) heap pointer. Scalars
/// (int/bool/float/null) are excluded: their boxes may be cached/immutable.
fn is_heap_ty(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Str | Type::StrLit(_) | Type::Array(_) | Type::FixedArray(_) | Type::Object(_) | Type::Iterator(_)
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
        Some(p) => is_union_ty(p) && !is_union_ty(arg_ty) && is_heap_ty(arg_ty),
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
    // Box/unbox across the union boundary.
    if is_union_ty(param_ty) != is_union_ty(arg_ty) {
        let dst = builder.alloc_temp(param_ty.clone());
        builder.emit(Instruction::Coerce { dst, src: arg, from_ty: arg_ty.clone(), to_ty: param_ty.clone() });
        return dst;
    }
    // Numeric width/kind mismatch between two concrete numeric types.
    if arg_ty.is_numeric() && param_ty.is_numeric() && arg_ty != param_ty {
        let dst = builder.alloc_temp(param_ty.clone());
        builder.emit(Instruction::Coerce { dst, src: arg, from_ty: arg_ty.clone(), to_ty: param_ty.clone() });
        return dst;
    }
    arg
}

/// Coerce a value temp to a slot's declared type when their runtime representations
/// differ (box concrete → union, or unbox union → concrete). Returns the (possibly new)
/// temp; a no-op when representations match.
fn coerce_to_slot_type(t: Temp, value_ty: &Type, slot_ty: &Type, builder: &mut FuncBuilder) -> Temp {
    if type_repr_differs(value_ty, slot_ty) {
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
                let t = lower_expr(value, builder, ctx);
                // Store the value in the slot's declared representation: a concrete value
                // bound to a Json/union slot must be boxed so later reads (LocalGet, is/has)
                // see a TaggedVal*.
                let t = coerce_to_slot_type(t, &value.ty(), ty, builder);
                builder.slots.insert(*slot, t);
                // Also publish top-level vals to their module global (for closure reads).
                if ctx.global_val_slots.contains_key(slot) {
                    builder.emit(Instruction::GlobalValSet { slot: *slot, value: t, ty: ty.clone() });
                }
            }
        }
        TypedStmt::Var { slot, value, ty, .. } => {
            if ctx.mutable_cell_slots.contains(slot) {
                // Mutably captured by a closure: store in a heap cell shared by reference.
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
                let t = lower_expr(value, builder, ctx);
                let t = coerce_to_slot_type(t, &value.ty(), ty, builder);
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
                    builder.emit(Instruction::GlobalValSet { slot: *slot, value: gv, ty: ty.clone() });
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
                    // `std/array` module so a same-named export from elsewhere isn't trusted.
                    if module_key == "std_array" {
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
            // StrLit is Str at runtime (ADR-051): always lower to an owned Str temp.
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
                builder.emit(Instruction::GlobalValGet { dst, slot: *slot, ty: gty.clone() });
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
                let t = if is_union_ty(&stored_ty) && !is_union_ty(ty) {
                    let dst = builder.alloc_temp(ty.clone());
                    builder.emit(Instruction::Coerce {
                        dst, src: t, from_ty: stored_ty, to_ty: ty.clone(),
                    });
                    dst
                } else {
                    t
                };
                // Pessimistically retain heap values on every read — rc_elide removes redundant pairs.
                if is_rc_type(ty) {
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
                builder.emit(Instruction::GlobalValGet { dst, slot: *slot, ty: gty.clone() });
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

        TypedExpr::LocalSet { slot, value, .. } => {
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
                builder.emit(Instruction::GlobalValSet { slot: *slot, value: stored, ty: gty.clone() });
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
            builder.slots.insert(*slot, val_temp);
            // LocalSet returns the value.
            val_temp
        }

        TypedExpr::BinaryOp { left, op, right, result_type, .. } => {
            // `&&` / `||` are SHORT-CIRCUITING (spec §24): the RHS must only be evaluated when
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
            let dst = builder.alloc_temp(result_type.clone());
            builder.emit(Instruction::Binary {
                dst,
                op: *op,
                lhs,
                rhs,
                operand_ty,
                ty: result_type.clone(),
            });
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
            let src = lower_expr(expr, builder, ctx);
            let dst = builder.alloc_temp(to.clone());
            builder.emit(Instruction::Coerce {
                dst,
                src,
                from_ty: from.clone(),
                to_ty: to.clone(),
            });
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
            // Release all owned temps in this scope except the result.
            builder.pop_scope_releasing(result);
            // Restore outer scope (block-local bindings don't leak).
            // But keep slots that were already present (var updates).
            for (k, v) in &outer_slots {
                if !stmts.iter().any(|s| stmt_defines_slot(s, *k)) {
                    builder.slots.insert(*k, *v);
                }
            }
            result
        }

        TypedExpr::Function { name, params, body, ret_type, captures, .. } => {
            lower_function_expr(name.as_deref(), params, body, ret_type, captures, builder, ctx)
        }

        TypedExpr::MakeObject { fields, spreads, ty, .. } => {
            let lowered_fields: Vec<(String, Temp)> = fields
                .iter()
                .map(|(k, v)| (k.clone(), lower_expr(v, builder, ctx)))
                .collect();
            let lowered_spreads: Vec<Temp> = spreads
                .iter()
                .map(|s| lower_expr(s, builder, ctx))
                .collect();
            let dst = builder.alloc_temp(ty.clone());
            builder.emit(Instruction::MakeObject {
                dst,
                fields: lowered_fields,
                spreads: lowered_spreads,
                ty: ty.clone(),
            });
            builder.register_owned(dst, ty.clone());
            dst
        }

        TypedExpr::MakeArray { elements, ty, .. } => {
            let elem_ty = match ty {
                Type::Array(inner) => *inner.clone(),
                _ => Type::Null,
            };
            // Coerce each element to the array's element representation. For a Json/union
            // element type (heterogeneous array) this boxes each concrete element to a
            // TaggedVal*, so codegen can push them uniformly.
            let lowered: Vec<Temp> = elements
                .iter()
                .map(|e| {
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
            let obj_temp = lower_expr(object, builder, ctx);
            let key_temp = lower_expr(key, builder, ctx);
            let dst = builder.alloc_temp(result_type.clone());
            builder.emit(Instruction::Index {
                dst,
                object: obj_temp,
                key: key_temp,
                obj_ty,
                key_ty,
                result_ty: result_type.clone(),
            });
            // A projection returns a BORROWED reference into the container. Dup it (retain +
            // register as owned) so the result behaves like any owned value: a consuming var
            // that releases on reassignment, or a scope-exit release, is then balanced and the
            // container's own release stays safe. Without this, releasing the container frees
            // a value still aliased by the projected binding (the AST path masks this by
            // leaking the container instead). A union/Json result is NOT dup'd here: the
            // runtime accessor (lin_object_get) returns an INTERIOR pointer to the entry's
            // TaggedVal, not an ownable heap box — treating it as owned and releasing it would
            // free an interior address. Concrete heap projections ARE real owned values.
            if is_rc_type(result_type) {
                builder.emit(Instruction::Retain { val: dst, ty: result_type.clone() });
                builder.register_owned(dst, result_type.clone());
            }
            dst
        }

        TypedExpr::FieldGet { object, field, result_type, .. } => {
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
            // Dup the projected heap reference — see the Index case above for the rationale.
            // (Union/Json results are interior pointers — not dup'd; see the Index case.)
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
            // like `Person`): a bare tag check (or mere field-presence, ADR-050) matches objects
            // with the WRONG field types, which is unsound — the arm then narrows the binding and
            // a subsequent field access operates on the wrong runtime type. Deep-validate field
            // types recursively via the `fromJson` structural walker (ADR-053). `MatchesSchema`
            // borrows the boxed value and reads a static descriptor — no ownership change, so the
            // `val_temp` boxing is the same one the former HasPattern path used.
            if let TypedPattern::TypeCheckDeep(target, named_defs, _) = pattern {
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
            let obj_temp = lower_expr(object, builder, ctx);
            let key_temp = lower_expr(key, builder, ctx);
            let val_temp = lower_expr(value, builder, ctx);
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
            builder.transfer_into_container(val_temp, value, op_consumes_union);
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
    lower_coerce_arg(t, &a.ty(), param_ty, builder)
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
fn lower_call_arg_tracked(
    a: &TypedExpr,
    param_ty: Option<&Type>,
    i: usize,
    cb_idx: Option<usize>,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> (Temp, Option<Temp>) {
    // Fresh-alloc heap literal boxed into a Json/union param: capture the raw temp for
    // transfer-on-escape tracking. (This path never coincides with a combinator callback.)
    if arg_box_is_caller_owned_shell(&a.ty(), param_ty) && expr_is_fresh_alloc(a) {
        let raw = lower_expr(a, builder, ctx);
        let coerced = lower_coerce_arg(raw, &a.ty(), param_ty, builder);
        let tracked = if coerced != raw { Some(raw) } else { None };
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
            let mut shell_boxes: Vec<Temp> = Vec::new();
            let mut escape_lits: Vec<Temp> = Vec::new();
            let lowered_args: Vec<Temp> = args
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    let (arg, raw_lit) = lower_call_arg_tracked(a, param_tys.get(i), i, cb_idx, builder, ctx);
                    retain_call_arg(arg, &a.ty(), expr_is_fresh_alloc(a), builder);
                    if arg_box_is_caller_owned_shell(&a.ty(), param_tys.get(i)) {
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
            builder.register_owned(dst, result_type.clone());
            for lit in escape_lits {
                builder.record_escape_alias(dst, lit);
            }
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
            let mut escape_lits: Vec<Temp> = Vec::new();
            let lowered_args: Vec<Temp> = args
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    let (arg, raw_lit) = lower_call_arg_tracked(a, param_tys.get(i), i, cb_idx, builder, ctx);
                    retain_call_arg(arg, &a.ty(), expr_is_fresh_alloc(a), builder);
                    if arg_box_is_caller_owned_shell(&a.ty(), param_tys.get(i)) {
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
            builder.register_owned(dst, result_type.clone());
            for lit in escape_lits {
                builder.record_escape_alias(dst, lit);
            }
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
    let lowered_args: Vec<Temp> = args
        .iter()
        .enumerate()
        .map(|(i, a)| {
            // Indirect call through a closure value: the callee is not a known synchronous
            // combinator, so cb_idx is None (no safe captured-cell context — conservative).
            let (arg, raw_lit) = lower_call_arg_tracked(a, param_tys.get(i), i, None, builder, ctx);
            retain_call_arg(arg, &a.ty(), expr_is_fresh_alloc(a), builder);
            if let Some(lit) = raw_lit {
                escape_lits.push(lit);
            }
            arg
        })
        .collect();

    if is_tail {
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
    // Concrete rc results are owned (+1) here; a UNION result from an INDIRECT closure call is
    // NOT registered, because the closure return ABI does NOT guarantee +1 for a boxed-union
    // return: a closure whose body yields a borrowed param/local box (e.g. minBy's
    // `(acc, x) => if x[0] < acc[0] then x else acc`) hands back a +0 box. Registering it would
    // make scope-exit release a box the callee never owned us → double-free. (Concrete rc returns
    // ARE +1: a concrete param read retains in place before the closure keeps it on return.)
    if is_rc_type(result_type) {
        builder.register_owned(dst, result_type.clone());
    }
    // Record transfer-on-escape aliasing regardless of whether the result was registered owned:
    // when the result escapes (is kept in a scope's keep-set), the fresh literal args it aliases
    // must be kept too (their scope-exit release would otherwise free the payload the escaping
    // result still aliases — the arg-box use-after-free).
    for lit in escape_lits {
        builder.record_escape_alias(dst, lit);
    }
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
    if matches!(intrinsic, Intrinsic::Push | Intrinsic::ArraySetDyn | Intrinsic::ObjectSetDyn) {
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

/// Call a body closure temp with arguments, coercing each argument to the closure's
/// declared parameter type (e.g. box a concrete element to Json when the callback param
/// is Json) so the closure ABI lines up. Returns the result temp typed as the closure's
/// declared return type.
fn call_body_closure(body: Temp, raw_args: &[(Temp, Type)], param_tys: &[Type], ret_ty: &Type, builder: &mut FuncBuilder) -> Temp {
    let call_args: Vec<Temp> = raw_args
        .iter()
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
    let mut elem_boxes = Vec::new();
    let call_args: Vec<Temp> = raw_args
        .iter()
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
fn push_output(out: Temp, flat: Option<FlatElemKind>, elem_ty: &Type, val: Temp, val_ty: &Type, builder: &mut FuncBuilder) {
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
            let boxed = box_to_json(val, val_ty, builder);
            builder.emit(Instruction::CallIntrinsic {
                dst: push_dst, intrinsic: Intrinsic::Push, args: vec![out, boxed], ret_ty: Type::Null,
            });
        }
    }
}

/// True when two types have a different runtime representation such that a value of one
/// must be coerced (boxed/unboxed) to be used as the other. Specifically: one is a
/// union/Json (TaggedVal*) and the other is a concrete type.
fn type_repr_differs(from: &Type, to: &Type) -> bool {
    is_union_ty(from) != is_union_ty(to)
}

/// Box a value to Json (TaggedVal*) if it is a concrete (non-union) type.
/// `fromJson` decode (ADR-047). Lower the Json value, box it to the tagged representation if
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
    let start = lower_expr(&args[0], builder, ctx);
    let end = lower_expr(&args[1], builder, ctx);

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
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    body_fn: F,
) {
    let elem_ty = iter_elem_type(iterable_ty);

    // len = length(iterable)
    let len = builder.alloc_temp(Type::Int64);
    builder.emit(Instruction::CallIntrinsic {
        dst: len,
        intrinsic: Intrinsic::Length,
        args: vec![iterable],
        ret_ty: Type::Int64,
    });
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
    let one = builder.const_temp(Const::Int(1, Type::Int64));
    builder.emit(Instruction::Binary {
        dst: i_next, op: BinOp::Add, lhs: i, rhs: one,
        operand_ty: Type::Int64, ty: Type::Int64,
    });
    builder.terminate(Terminator::Jump(header));

    builder.switch_to(exit);
}

/// `for(iterable, body)` → index loop calling `body(elem)` for side effects; returns Null.
fn lower_for(args: &[TypedExpr], builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    let iterable_ty = args[0].ty();
    let (param_tys, _) = callback_signature(&args[1]);
    let iterable = lower_expr(&args[0], builder, ctx);
    let body = lower_callback_in_safe_ctx(&args[1], builder, ctx);
    let elem_ty = iter_elem_type(&iterable_ty);
    // The callback closure uses the uniform BOXED ABI: it ALWAYS returns a freshly-allocated,
    // independently-owned `TaggedVal*` (e.g. `lin_box_null()` for a void-ish body, `lin_box_int`
    // for an int result, or — for an assignment body like `acc = concat(...)` — its own owned +1
    // ref, now distinct from the cell/global's shell thanks to the clone-on-store above). `for`
    // discards that value, so we MUST call with a union ret_ty (forcing codegen to emit a
    // `call ptr` rather than a `call void` that silently drops the returned box) and then
    // tag-aware release it every iteration, inside the loop body before the back-edge — never
    // registered as scope-owned (that would release once AFTER the loop, leaking per-iteration).
    let boxed = Type::TypeVar(u32::MAX);
    emit_index_loop(iterable, &iterable_ty, builder, ctx, |_, elem, b, _| {
        let (ret, elem_boxes) = call_body_closure_with_elem_boxes(body, &[(elem, elem_ty.clone())], &param_tys, &boxed, b);
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
    });
    builder.const_temp(Const::Null)
}

/// `while(iterable, body)` → like `for`, but stops early when `body(elem)` returns false.
fn lower_while(args: &[TypedExpr], builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    let iterable_ty = args[0].ty();
    let (param_tys, _) = callback_signature(&args[1]);
    let iterable = lower_expr(&args[0], builder, ctx);
    let body = lower_callback_in_safe_ctx(&args[1], builder, ctx);

    let elem_ty = iter_elem_type(&iterable_ty);
    let len = builder.alloc_temp(Type::Int64);
    builder.emit(Instruction::CallIntrinsic {
        dst: len, intrinsic: Intrinsic::Length, args: vec![iterable], ret_ty: Type::Int64,
    });
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
    // keep = body(elem) : Bool — continue only while true.
    let (keep, elem_boxes) = call_body_closure_with_elem_boxes(body, &[(elem, elem_ty.clone())], &param_tys, &Type::Bool, builder);
    // Reclaim the per-iteration element BOX SHELL (same mechanism + safety as `lower_for`):
    // `FreeBoxShell` frees only the 16-byte shell, never the inner. The predicate's `Bool` return
    // is an unboxed scalar, so it can NEVER alias the element box — no de-aliasing needed here.
    for ebox in &elem_boxes {
        builder.emit(Instruction::FreeBoxShell { val: *ebox });
    }
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

/// `map(iterable, f)` → new array of `f(elem)` for each element.
fn lower_map(args: &[TypedExpr], result_type: &Type, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    let iterable_ty = args[0].ty();
    let (param_tys, cb_ret) = callback_signature(&args[1]);
    let iterable = lower_expr(&args[0], builder, ctx);
    let f = lower_callback_in_safe_ctx(&args[1], builder, ctx);

    // Output element type per the map's declared result type; storage matches it.
    let out_elem_ty = match result_type {
        Type::Array(t) | Type::Iterator(t) => (**t).clone(),
        _ => Type::TypeVar(u32::MAX),
    };
    let (out, flat) = alloc_output_array(&out_elem_ty, result_type, builder);
    let elem_ty = iter_elem_type(&iterable_ty);

    emit_index_loop(iterable, &iterable_ty, builder, ctx, |_, elem, b, _| {
        let mapped = call_body_closure(f, &[(elem, elem_ty.clone())], &param_tys, &cb_ret, b);
        push_output(out, flat, &out_elem_ty, mapped, &cb_ret, b);
    });
    out
}

/// `filter(iterable, pred)` → new array of elements where `pred(elem)` is true.
fn lower_filter(args: &[TypedExpr], result_type: &Type, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    let iterable_ty = args[0].ty();
    let (param_tys, _) = callback_signature(&args[1]);
    let iterable = lower_expr(&args[0], builder, ctx);
    let pred = lower_callback_in_safe_ctx(&args[1], builder, ctx);

    // filter preserves the element type; storage matches it.
    let out_elem_ty = match result_type {
        Type::Array(t) | Type::Iterator(t) => (**t).clone(),
        _ => Type::TypeVar(u32::MAX),
    };
    let (out, flat) = alloc_output_array(&out_elem_ty, result_type, builder);
    let elem_ty = iter_elem_type(&iterable_ty);

    emit_index_loop(iterable, &iterable_ty, builder, ctx, |_, elem, b, _| {
        let keep = call_body_closure(pred, &[(elem, elem_ty.clone())], &param_tys, &Type::Bool, b);
        let keep_block = b.alloc_block("filter_keep");
        let skip_block = b.alloc_block("filter_skip");
        b.terminate(Terminator::CondJump { cond: keep, then_block: keep_block, else_block: skip_block });
        b.switch_to(keep_block);
        push_output(out, flat, &out_elem_ty, elem, &elem_ty, b);
        b.terminate(Terminator::Jump(skip_block));
        b.switch_to(skip_block);
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
    let iterable = lower_expr(&args[0], builder, ctx);
    let init_ty = args[1].ty();
    let init_raw = lower_expr(&args[1], builder, ctx);
    let init = box_to_json(init_raw, &init_ty, builder);
    let f = lower_callback_in_safe_ctx(&args[2], builder, ctx);
    let elem_ty = iter_elem_type(&iterable_ty);

    let len = builder.alloc_temp(Type::Int64);
    builder.emit(Instruction::CallIntrinsic {
        dst: len, intrinsic: Intrinsic::Length, args: vec![iterable], ret_ty: Type::Int64,
    });
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
    // acc_next = f(acc, elem). acc is carried as Json; coerce both args to the reducer's
    // declared param types.
    let acc_arg = coerce_arg_to_param(acc, &json, param_tys.first(), builder);
    let elem_arg = coerce_arg_to_param(elem, &elem_ty, param_tys.get(1), builder);
    builder.emit(Instruction::Call {
        dst: acc_next, callee: CallTarget::Indirect(f), args: vec![acc_arg, elem_arg], ret_ty: json.clone(),
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

    // --- then branch ---
    builder.switch_to(then_block);
    builder.push_scope();
    let then_raw = lower_expr(then_br, builder, ctx);
    if !builder.is_current_block_terminated() {
        let (then_val, keep, owned) = coerce_if_branch(then_raw, &then_br.ty(), result_type, builder);
        merge_owned = owned;
        builder.pop_scope_releasing_keep(&keep);
        incomings.push((then_val, builder.current_block));
        builder.terminate(Terminator::Jump(merge_block));
    } else {
        builder.discard_scope();
    }

    // --- else branch ---
    builder.switch_to(else_block);
    builder.push_scope();
    let else_raw = lower_expr(else_br, builder, ctx);
    if !builder.is_current_block_terminated() {
        let (else_val, keep, owned) = coerce_if_branch(else_raw, &else_br.ty(), result_type, builder);
        merge_owned = owned;
        builder.pop_scope_releasing_keep(&keep);
        incomings.push((else_val, builder.current_block));
        builder.terminate(Terminator::Jump(merge_block));
    } else {
        builder.discard_scope();
    }

    builder.switch_to(merge_block);
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

/// Lower a short-circuiting `&&` / `||` (spec §24) as branch + merge + Phi, so the RHS is
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

        // Test the pattern.
        let matched = lower_match_pattern(&arm.pattern, scrut_temp, &arm.body, builder, ctx);

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

        let arm_raw = lower_expr(&arm.body, builder, ctx);
        if !builder.is_current_block_terminated() {
            let arm_val = coerce_to_slot_type(arm_raw, &arm.body.ty(), result_type, builder);
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
        // alias like `Person`): a bare tag check (or mere field-presence, ADR-050) matches
        // objects with the WRONG field types, which is unsound once the arm narrows the binding.
        // Deep-validate field types recursively via the `fromJson` structural walker (ADR-053).
        // `scrut` is the already-boxed scrutinee; `MatchesSchema` borrows it (no ownership change).
        TypedMatchPattern::Is(TypedPattern::TypeCheckDeep(target, named_defs, _)) => {
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
    let real_fn_ty = Type::Function {
        params: params.iter().map(|p| p.ty.clone()).collect(),
        ret: Box::new(ret_type.clone()),
        required,
    };
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
    let raw_ret = lower_expr(body, &mut inner_builder, ctx);
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
    let ret_temp = if !inner_builder.is_current_block_terminated()
        && type_repr_differs(&body_ty, &effective_ret)
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
    let capture_temps: Vec<Temp> = captures
        .iter()
        .map(|cap| {
            builder.slots.get(&cap.outer_slot).copied().unwrap_or_else(|| {
                builder.alloc_temp(cap.ty.clone())
            })
        })
        .collect();

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

    for part in parts {
        let part_temp = match part {
            TypedStringPart::Literal(s) => builder.const_temp(Const::Str(s.clone())),
            TypedStringPart::Expr(expr) => {
                let val = lower_expr(expr, builder, ctx);
                // Convert to string.
                let dst = builder.alloc_temp(Type::Str);
                builder.emit(Instruction::CallIntrinsic {
                    dst,
                    intrinsic: Intrinsic::ToString,
                    args: vec![val],
                    ret_ty: Type::Str,
                });
                dst
            }
        };
        // Concatenate with accumulator.
        let new_acc = builder.alloc_temp(Type::Str);
        builder.emit(Instruction::CallIntrinsic {
            dst: new_acc,
            intrinsic: Intrinsic::StringConcat,
            args: vec![acc, part_temp],
            ret_ty: Type::Str,
        });
        // Release old accumulator (it was just consumed).
        if acc != part_temp {
            // Only release non-literal temps.
            builder.emit(Instruction::Release { val: acc, ty: Type::Str });
        }
        acc = new_acc;
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
