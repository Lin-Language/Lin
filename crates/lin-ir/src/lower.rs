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

/// Mangle an import path into the LLVM symbol prefix codegen uses for that module's
/// exports. Must match `register_import`'s `path.replace("/", "_").replace("-", "_")`.
fn mangle_module_key(path: &str) -> String {
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
/// literal or a call result) whose +1 reference can be transferred to a consuming callee.
fn expr_is_fresh_alloc(expr: &TypedExpr) -> bool {
    matches!(expr, TypedExpr::Function { .. } | TypedExpr::Call { .. })
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
                    Some(fid), name.as_deref(), params, body, ret_type, captures, builder, ctx,
                );
                builder.slots.insert(*slot, t);
            } else {
                let t = lower_expr(value, builder, ctx);
                // Store the value in the slot's declared representation: a concrete value
                // bound to a Json/union slot must be boxed so later reads (LocalGet, is/has)
                // see a TaggedVal*.
                let t = coerce_to_slot_type(t, &value.ty(), ty, builder);
                builder.slots.insert(*slot, t);
            }
        }
        TypedStmt::Var { slot, value, ty, .. } => {
            let t = lower_expr(value, builder, ctx);
            let t = coerce_to_slot_type(t, &value.ty(), ty, builder);
            // Var slots are represented as mutable temps. The "cell" indirection
            // used in codegen (Alloca) is handled by codegen consuming LinIR, not here.
            // We track the current temp for each var slot, updated on LocalSet.
            builder.slots.insert(*slot, t);
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
            } else {
                // Slot not yet in scope — emit a placeholder null temp.
                // (Can happen for forward-declared functions resolved by codegen.)
                builder.alloc_temp(ty.clone())
            }
        }

        TypedExpr::LocalSet { slot, value, .. } => {
            let val_temp = lower_expr(value, builder, ctx);
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
                    let t = lower_expr(a, builder, ctx);
                    let param_ty = param_tys.get(i);
                    let boxed = lower_box_for_param(t, &a.ty(), param_ty, builder);
                    retain_call_arg(boxed, &a.ty(), expr_is_fresh_alloc(a), builder);
                    boxed
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
                    let t = lower_expr(a, builder, ctx);
                    let param_ty = param_tys.get(i);
                    let boxed = lower_box_for_param(t, &a.ty(), param_ty, builder);
                    retain_call_arg(boxed, &a.ty(), expr_is_fresh_alloc(a), builder);
                    boxed
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
        "concat" => Intrinsic::Concat,
        "lin_async" => Intrinsic::Async,
        "lin_await" => Intrinsic::Await,
        "lin_exit" => Intrinsic::Exit,
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

fn lower_if(
    cond: &TypedExpr,
    then_br: &TypedExpr,
    else_br: &TypedExpr,
    result_type: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Temp {
    let cond_temp = lower_expr(cond, builder, ctx);

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
        // `Object` branch value boxed to a `Json` if-result).
        let then_val = coerce_to_slot_type(then_raw, &then_br.ty(), result_type, builder);
        builder.pop_scope_releasing(then_val);
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
        builder.pop_scope_releasing(else_val);
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

        // Emit body.
        builder.switch_to(body_block);

        // If there's a guard, test it.
        if let Some(guard) = &arm.guard {
            let guard_val = lower_expr(guard, builder, ctx);
            let guard_then = builder.alloc_block(format!("arm_{}_guard_ok", i));
            builder.terminate(Terminator::CondJump {
                cond: guard_val,
                then_block: guard_then,
                else_block: next_block,
            });
            builder.switch_to(guard_then);
        }

        // Emit bindings from the pattern into slots.
        lower_match_bindings(&arm.pattern, scrut_temp, builder, ctx);

        let arm_val = lower_expr(&arm.body, builder, ctx);
        if !builder.is_current_block_terminated() {
            incomings.push((arm_val, builder.current_block));
            builder.terminate(Terminator::Jump(merge_block));
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
        TypedMatchPattern::Has(tp) => {
            let required_fields = pattern_required_fields(tp);
            let mut cond = builder.alloc_temp(Type::Bool);
            builder.emit(Instruction::HasPattern {
                dst: cond,
                val: scrut,
                pattern: HasDesc { required_fields },
            });
            // For object fields with a value constraint (e.g. `has { "type": "success" }`),
            // also require scrut[key] == literal, AND-ed into the condition.
            if let TypedPattern::Object { fields, .. } = tp {
                let scrut_ty = builder.temp_types.get(&scrut).cloned().unwrap_or(Type::TypeVar(u32::MAX));
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
            }
            PatternTest::Cond(cond)
        }
    }
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
    lower_function_expr_with_id(None, name, params, body, ret_type, captures, builder, ctx)
}

/// Lower a function literal. `forced_fid` reuses a pre-assigned FuncId (for top-level
/// named functions registered in `global_fn_slots` during the pre-scan, so that
/// `CallTarget::Direct` references resolve to the actually-emitted function); pass
/// None to allocate a fresh id (anonymous/nested closures).
fn lower_function_expr_with_id(
    forced_fid: Option<FuncId>,
    name: Option<&str>,
    params: &[TypedParam],
    body: &TypedExpr,
    ret_type: &Type,
    captures: &[Capture],
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Temp {
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
            let cap_ty = cap.ty.clone();
            let cap_t = inner_builder.alloc_temp(cap_ty.clone());
            // Env access is a raw struct load by index, NOT a Lin object field access.
            inner_builder.emit(Instruction::EnvCapture {
                dst: cap_t,
                env: env_temp,
                index: i as u32,
                ty: cap_ty,
            });
            inner_builder.slots.insert(cap.outer_slot, cap_t);
        }
    }

    inner_builder.push_scope();
    let body_ty = body.ty();
    let raw_ret = lower_expr(body, &mut inner_builder, ctx);
    // Closures use a UNIFORM boxed ABI: they always return Json (TaggedVal*), so a closure
    // value can be called through an opaque `Function` type without knowing its concrete
    // return type (the indirect call site unboxes). Non-closure top-level functions keep
    // their declared concrete return (they are only called Direct, with exact signatures).
    let effective_ret = if is_closure { Type::TypeVar(u32::MAX) } else { ret_type.clone() };
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
    // Release owned temps in function scope except the return value.
    inner_builder.pop_scope_releasing(ret_temp);
    if !inner_builder.is_current_block_terminated() {
        inner_builder.terminate(Terminator::Return(Some(ret_temp)));
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
