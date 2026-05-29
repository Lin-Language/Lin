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

use crate::ir::*;

/// Entry point: lower a TypedModule to a LinModule.
pub fn lower_module(module: &TypedModule) -> LinModule {
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

    // Top-level non-function vals become module globals (so closures can read them).
    for stmt in &module.statements {
        if let TypedStmt::Val { slot, value, ty, .. } = stmt {
            if !matches!(value, TypedExpr::Function { .. }) {
                ctx.global_val_slots.insert(*slot, ty.clone());
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

    // Compile nested functions collected during lowering.
    while let Some(pending) = ctx.pending_functions.pop() {
        ctx.functions.push(pending);
    }

    LinModule {
        functions: ctx.functions,
        global_fn_slots,
        intrinsics: ctx.intrinsics,
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
        }
    }
    ctx.global_fn_slots = global_fn_slots.clone();

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
            value: TypedExpr::Function { params, body, ret_type, captures, .. },
            ..
        } = stmt
        {
            if let Some(&fid) = ctx.global_fn_slots.get(slot) {
                let mangled = fn_names.get(&fid).cloned();
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

    // Collect all lifted/nested functions produced during lowering.
    while let Some(pending) = ctx.pending_functions.pop() {
        ctx.functions.push(pending);
    }

    LinModule {
        functions: ctx.functions,
        global_fn_slots,
        intrinsics: ctx.intrinsics,
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
    /// Imported modules are compiled by codegen's AST `register_import` regardless of
    /// the IR path, so the symbol already exists; the IR `CallTarget::Named` resolver
    /// looks it up by name. Param types drive arg boxing (concrete → Json param).
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
}

impl LowerCtx {
    fn new() -> Self {
        Self {
            functions: Vec::new(),
            pending_functions: Vec::new(),
            func_counter: 0,
            intrinsics: HashMap::new(),
            global_fn_slots: HashMap::new(),
            import_fn_slots: HashMap::new(),
            import_val_slots: HashMap::new(),
            mutable_cell_slots: std::collections::HashSet::new(),
            global_val_slots: HashMap::new(),
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
        });
        id
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
    fn register_owned(&mut self, t: Temp, ty: Type) {
        if is_rc_type(&ty) {
            if let Some(frame) = self.scope_owned.last_mut() {
                frame.push((t, ty));
            }
        }
    }

    /// Register an owned temp that may be a boxed union/Json value (whose heap payload still
    /// needs releasing). Used for projections (`obj[k]` / `obj.field`) whose result type is a
    /// union: the projected TaggedVal* aliases a value inside the container, so it must be
    /// dup'd and tracked for release like any other owned heap value. `Release` codegen is
    /// tag-aware, so releasing a union temp frees its inner payload correctly.
    fn register_owned_rc_or_union(&mut self, t: Temp, ty: Type) {
        if is_rc_type(&ty) || is_union_ty(&ty) {
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

    /// Pop the current scope frame and emit Release for all owned temps except `keep`.
    fn pop_scope_releasing(&mut self, keep: Temp) {
        if let Some(frame) = self.scope_owned.pop() {
            for (t, ty) in frame {
                if t != keep {
                    self.emit(Instruction::Release { val: t, ty });
                }
            }
        }
    }

    /// Pop the current scope frame, releasing all owned temps except those in `keep`.
    fn pop_scope_releasing_keep(&mut self, keep: &[Temp]) {
        if let Some(frame) = self.scope_owned.pop() {
            for (t, ty) in frame {
                if !keep.contains(&t) {
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
        Type::Str | Type::Array(_) | Type::FixedArray(_) | Type::Object(_) | Type::Function { .. }
    )
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
    path.replace('/', "_").replace('-', "_")
}

/// A type stored at runtime as a TaggedVal* pointer (Json/union/dynamic).
/// Mirrors codegen's `Codegen::is_union_type`.
fn is_union_ty(ty: &Type) -> bool {
    matches!(ty, Type::Union(_) | Type::TypeVar(_) | Type::Named(_))
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

/// Box a concrete argument when the callee's parameter is a Json/union type.
/// Emits a `Coerce` (which codegen lowers to `build_tagged_val_alloca`) and returns the
/// boxed temp; otherwise returns the argument temp unchanged. Mirrors the AST path's
/// arg-boxing rule in `call_global_fn` (concrete arg → union param ⇒ box).
fn lower_box_for_param(arg: Temp, arg_ty: &Type, param_ty: Option<&Type>, builder: &mut FuncBuilder) -> Temp {
    let Some(param_ty) = param_ty else { return arg; };
    if is_union_ty(param_ty) && !is_union_ty(arg_ty) {
        let dst = builder.alloc_temp(param_ty.clone());
        builder.emit(Instruction::Coerce {
            dst,
            src: arg,
            from_ty: arg_ty.clone(),
            to_ty: param_ty.clone(),
        });
        dst
    } else {
        arg
    }
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
            if let (TypedExpr::Function { name, params, body, ret_type, captures, .. }, Some(&fid)) =
                (value, ctx.global_fn_slots.get(slot))
            {
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
                let t = lower_expr(value, builder, ctx);
                let t = coerce_to_slot_type(t, &value.ty(), &cell_ty, builder);
                let cell = builder.alloc_temp(Type::TypeVar(u32::MAX));
                builder.emit(Instruction::MakeCell { dst: cell, init: t, ty: cell_ty.clone() });
                builder.cell_slots.insert(*slot, cell_ty);
                builder.slots.insert(*slot, cell);
            } else {
                let t = lower_expr(value, builder, ctx);
                let t = coerce_to_slot_type(t, &value.ty(), ty, builder);
                // Plain mutable temp; tracked per var slot, updated on LocalSet.
                builder.slots.insert(*slot, t);
            }
        }
        TypedStmt::Import { path, bindings, .. } => {
            // Imported modules are compiled by codegen's AST `register_import` even on
            // the IR path, so each exported symbol already exists in the LLVM module
            // under its mangled name `{module_key}_{name}`. Resolve each binding slot to
            // either a `Named` call target (function exports) or a zero-arg val-wrapper
            // (non-function exports), matching the AST path's `compile_stmt` Import logic.
            let module_key = mangle_module_key(path);
            for b in bindings {
                if let Type::Function { params, .. } = &b.ty {
                    let sym = format!("{}_{}", module_key, b.name);
                    ctx.import_fn_slots.insert(b.slot, (sym, params.clone()));
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
        TypedExpr::StringLit(s, _) => {
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
            // Heap-cell slot (mutably-captured var): load the current value through the cell.
            if let Some(cell_ty) = builder.cell_slots.get(slot).cloned() {
                if let Some(&cell) = builder.slots.get(slot) {
                    let dst = builder.alloc_temp(cell_ty.clone());
                    builder.emit(Instruction::CellGet { dst, cell, ty: cell_ty.clone() });
                    if is_rc_type(&cell_ty) {
                        builder.emit(Instruction::Retain { val: dst, ty: cell_ty.clone() });
                        builder.register_owned(dst, cell_ty);
                    }
                    return dst;
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
                // closure) — load it from its module global.
                let dst = builder.alloc_temp(gty.clone());
                builder.emit(Instruction::GlobalValGet { dst, slot: *slot, ty: gty.clone() });
                if is_rc_type(&gty) {
                    builder.emit(Instruction::Retain { val: dst, ty: gty.clone() });
                    builder.register_owned(dst, gty);
                }
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
                    builder.emit(Instruction::CellSet { cell, value: v, ty: cell_ty });
                    return v;
                }
            }
            builder.slots.insert(*slot, val_temp);
            // LocalSet returns the value.
            val_temp
        }

        TypedExpr::BinaryOp { left, op, right, result_type, .. } => {
            // The operand type drives equality/comparison dispatch (e.g. object/array
            // deep equality); it differs from result_type for comparisons (which yield Bool).
            let operand_ty = left.ty();
            let lhs = lower_expr(left, builder, ctx);
            let rhs = lower_expr(right, builder, ctx);
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

        TypedExpr::Call { func, args, result_type, is_tail, .. } => {
            lower_call(func, args, result_type, *is_tail, builder, ctx)
        }

        TypedExpr::If { cond, then_br, else_br, result_type, .. } => {
            lower_if(cond, then_br, else_br, result_type, builder, ctx)
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
                    // recursively releases them when the array is freed). Manage ownership on
                    // the RAW underlying heap value `t`, NOT the boxed TaggedVal — retaining a
                    // box would bump the wrong refcount and the inner value would still be
                    // double-freed. Mirrors AST compile_make_array's `!expr_is_owned_alloc`.
                    let et = e.ty();
                    if is_rc_type(&et) {
                        if expr_is_fresh_alloc(e) {
                            // Fresh allocation: transfer its +1 into the array. Drop it from
                            // the owning scope so the scope-exit release doesn't double-free
                            // (the array's recursive release already accounts for it).
                            builder.unregister_owned(t);
                        } else {
                            // Borrowed value (e.g. a LocalGet): retain so the array's copy and
                            // the original owner can each release independently.
                            builder.emit(Instruction::Retain { val: t, ty: et.clone() });
                        }
                    }
                    coerce_to_slot_type(t, &et, &elem_ty, builder)
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
            builder.emit(Instruction::IndexSet {
                object: obj_temp,
                key: key_temp,
                value: val_temp,
                obj_ty: obj_ty.clone(),
                key_ty,
                val_ty,
            });
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
            Some(Type::Function { params: cb_params, ret: cb_ret })) = (a, param_ty)
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

fn lower_call(
    func: &TypedExpr,
    args: &[TypedExpr],
    result_type: &Type,
    is_tail: bool,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Temp {
    // Check if this is an intrinsic call.
    if let TypedExpr::LocalGet { slot, .. } = func {
        if let Some(name) = builder.intrinsic_slots.get(slot).cloned() {
            return lower_intrinsic_call(&name, args, result_type, builder, ctx);
        }
        // Imported function: call the compiled symbol by its mangled name, boxing
        // concrete args passed to Json/union-typed parameters.
        if let Some((sym, param_tys)) = ctx.import_fn_slots.get(slot).cloned() {
            let lowered_args: Vec<Temp> = args
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    let arg = lower_call_arg(a, param_tys.get(i), builder, ctx);
                    retain_call_arg(arg, &a.ty(), expr_is_fresh_alloc(a), builder);
                    arg
                })
                .collect();
            let dst = builder.alloc_temp(result_type.clone());
            builder.emit(Instruction::Call {
                dst,
                callee: CallTarget::Named(sym),
                args: lowered_args,
                ret_ty: result_type.clone(),
            });
            builder.register_owned(dst, result_type.clone());
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
            let lowered_args: Vec<Temp> = args
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    let arg = lower_call_arg(a, param_tys.get(i), builder, ctx);
                    retain_call_arg(arg, &a.ty(), expr_is_fresh_alloc(a), builder);
                    arg
                })
                .collect();
            if is_tail {
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
                callee: CallTarget::Direct(fid),
                args: lowered_args,
                ret_ty: result_type.clone(),
            });
            builder.register_owned(dst, result_type.clone());
            return dst;
        }
    }

    let fn_temp = lower_expr(func, builder, ctx);
    let lowered_args: Vec<Temp> = args.iter().map(|a| lower_expr(a, builder, ctx)).collect();

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
    builder.register_owned(dst, result_type.clone());
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
        "lin_await" => Intrinsic::Await,
        "lin_exit" => Intrinsic::Exit,
        "lin_parallel" => Intrinsic::Parallel,
        "lin_race" => Intrinsic::Race,
        "lin_timeout" => Intrinsic::Timeout,
        "lin_retry" => Intrinsic::Retry,
        "lin_thread_pool" => Intrinsic::ThreadPool,
        "lin_worker" => Intrinsic::Worker,
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

    // `push(arr, elem)` / `set(arr, idx, elem)` transfer a reference to the element into the
    // array, and codegen's push/set do NOT retain (they store the pointer / copy the boxed
    // value). So manage the element's ownership like a MakeArray element: a fresh allocation
    // transfers its +1 (drop it from the owning scope so the scope-exit release doesn't free a
    // value the array now holds); a borrowed heap value is retained so both owners can release.
    // The element is the LAST argument in all three: push(arr, elem), set(arr, idx, elem),
    // object_set(obj, key, val). For push/set, codegen does NOT retain (stores the pointer);
    // for object_set, codegen boxes the value, calls lin_object_set (which retains the inner),
    // then releases the box (undoing that retain) — so the net effect is also a transfer.
    // Either way: a fresh allocation transfers its +1 into the container (drop it from the
    // owning scope so scope-exit doesn't double-free); a borrowed heap value is retained.
    if matches!(intrinsic, Intrinsic::Push | Intrinsic::ArraySetDyn | Intrinsic::ObjectSetDyn) {
        if let (Some(elem_expr), Some(&elem_temp)) = (args.last(), lowered_args.last()) {
            let et = elem_expr.ty();
            if is_rc_type(&et) {
                if expr_is_fresh_alloc(elem_expr) {
                    builder.unregister_owned(elem_temp);
                } else {
                    builder.emit(Instruction::Retain { val: elem_temp, ty: et });
                }
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

/// The declared parameter types and return type of a callback expression, if it has a
/// statically-known `Function` type. Used to match the closure's compiled ABI when calling it.
fn callback_signature(expr: &TypedExpr) -> (Vec<Type>, Type) {
    match expr.ty() {
        Type::Function { params, ret } => (params, *ret),
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
    let body = lower_expr(&args[1], builder, ctx);
    let elem_ty = iter_elem_type(&iterable_ty);
    emit_index_loop(iterable, &iterable_ty, builder, ctx, |_, elem, b, _| {
        call_body_closure(body, &[(elem, elem_ty.clone())], &param_tys, &Type::Null, b);
    });
    builder.const_temp(Const::Null)
}

/// `while(iterable, body)` → like `for`, but stops early when `body(elem)` returns false.
fn lower_while(args: &[TypedExpr], builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    let iterable_ty = args[0].ty();
    let (param_tys, _) = callback_signature(&args[1]);
    let iterable = lower_expr(&args[0], builder, ctx);
    let body = lower_expr(&args[1], builder, ctx);

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
    let keep = call_body_closure(body, &[(elem, elem_ty.clone())], &param_tys, &Type::Bool, builder);
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
    let f = lower_expr(&args[1], builder, ctx);

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
    let pred = lower_expr(&args[1], builder, ctx);

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
    let f = lower_expr(&args[2], builder, ctx);
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

    builder.terminate(Terminator::CondJump {
        cond: cond_temp,
        then_block,
        else_block,
    });

    let result_dst = builder.alloc_temp(result_type.clone());
    let result_is_rc = is_rc_type(result_type);

    // Each branch gets its own ownership scope so heap temps it allocates are released
    // at the end of *that branch* — not in the merge block, where only one branch's
    // temps are live (releasing the other branch's temps there frees undefined values).
    // The branch's result value is kept (released as part of the merge's owned set).
    // We collect (value_temp, predecessor_block) for a Phi in the merge block, recording
    // the ACTUAL predecessor (the block current at the end of the branch, which may differ
    // from the branch entry if the branch contained nested control flow).
    let mut incomings: Vec<(Temp, BlockId)> = Vec::new();

    // --- then branch ---
    builder.switch_to(then_block);
    builder.push_scope();
    let then_raw = lower_expr(then_br, builder, ctx);
    if !builder.is_current_block_terminated() {
        // Coerce to the if's result representation so both phi inputs agree (e.g. an
        // `Object` branch value boxed to a `Json` if-result). Keep BOTH the kept result
        // and its raw pre-coercion temp: a box shares the underlying pointer, so releasing
        // the original would free what the kept box wraps.
        let then_val = coerce_to_slot_type(then_raw, &then_br.ty(), result_type, builder);
        builder.pop_scope_releasing_keep(&[then_val, then_raw]);
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
        let else_val = coerce_to_slot_type(else_raw, &else_br.ty(), result_type, builder);
        builder.pop_scope_releasing_keep(&[else_val, else_raw]);
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
    if result_is_rc {
        builder.register_owned(result_dst, result_type.clone());
    }
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
            let t = builder.alloc_temp(ty.clone());
            builder.emit(Instruction::Bind { dst: t, src: scrut, ty: ty.clone() });
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
        TypedPattern::TypeCheck(_, _) | TypedPattern::Literal(_) | TypedPattern::Wildcard(_) => {
            // No bindings.
        }
    }
}

fn pattern_elem_type(pattern: &TypedPattern) -> Type {
    match pattern {
        TypedPattern::Binding(_, ty, _) => ty.clone(),
        TypedPattern::TypeCheck(ty, _) => ty.clone(),
        _ => Type::Null,
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
    };

    // Add entry block.
    inner_builder.blocks.push(BasicBlock {
        id: BlockId(0),
        label: Some("entry".into()),
        instructions: Vec::new(),
        terminator: Terminator::Unreachable,
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

    inner_builder.push_scope();
    let raw_ret = lower_expr(body, &mut inner_builder, ctx);
    // Use the lowered temp's ACTUAL type for the return coercion, not the surface
    // `body.ty()`. They can disagree when the body reads a mutably-captured `var` whose
    // declared type was widened by reassignment: e.g. `var found = null; ...; found` has
    // surface type `Null`, but the cell (and the CellGet temp) is `Json`. Trusting the
    // stale `Null` would coerce the live Json value to a boxed null on return.
    let body_ty = inner_builder.temp_types.get(&raw_ret).cloned().unwrap_or_else(|| body.ty());
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
    // Release owned temps in function scope except the return value AND the raw
    // pre-coercion temp: a box (e.g. lin_box_object) shares the underlying pointer, so
    // releasing the original would free what the returned box wraps.
    inner_builder.pop_scope_releasing_keep(&[ret_temp, raw_ret]);
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

    let closure_ty = Type::Function {
        params: params.iter().map(|p| p.ty.clone()).collect(),
        ret: Box::new(ret_type.clone()),
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
