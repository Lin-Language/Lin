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

use crate::ir::*;

/// Entry point: lower a TypedModule to a LinModule.
pub fn lower_module(module: &TypedModule) -> LinModule {
    let mut ctx = LowerCtx::new();
    ctx.intrinsics = module.intrinsics.clone();

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
    let main_id = ctx.alloc_func_id();
    let mut builder = FuncBuilder::new(main_id, None, vec![], false, Type::Int32, ctx.intrinsics.clone());

    for stmt in &module.statements {
        lower_stmt(stmt, &mut builder, &mut ctx);
    }

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
}

impl LowerCtx {
    fn new() -> Self {
        Self {
            functions: Vec::new(),
            pending_functions: Vec::new(),
            func_counter: 0,
            intrinsics: HashMap::new(),
            global_fn_slots: HashMap::new(),
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

    fn is_current_block_terminated(&self) -> bool {
        let id = self.current_block;
        self.blocks
            .iter()
            .find(|b| b.id == id)
            .map(|b| !matches!(b.terminator, Terminator::Unreachable))
            .unwrap_or(false)
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
        TypedStmt::Val { slot, value, .. } => {
            let t = lower_expr(value, builder, ctx);
            builder.slots.insert(*slot, t);
        }
        TypedStmt::Var { slot, value, ty, .. } => {
            let t = lower_expr(value, builder, ctx);
            // Var slots are represented as mutable temps. The "cell" indirection
            // used in codegen (Alloca) is handled by codegen consuming LinIR, not here.
            // We track the current temp for each var slot, updated on LocalSet.
            builder.slots.insert(*slot, t);
            let _ = ty; // type is in temp_types
        }
        TypedStmt::Import { bindings, .. } => {
            // Import bindings are resolved at a higher level; here we just record
            // placeholder temps so slot references don't panic.
            for b in bindings {
                let t = builder.alloc_temp(b.ty.clone());
                builder.slots.insert(b.slot, t);
            }
        }
        TypedStmt::ForeignImport { bindings, .. } => {
            // Foreign bindings are declared external symbols; record placeholder temps.
            for b in bindings {
                let t = builder.alloc_temp(b.ty.clone());
                builder.slots.insert(b.slot, t);
            }
        }
        TypedStmt::Destructure {
            obj_slot,
            value,
            fields,
            obj_ty,
            ..
        } => {
            let obj_temp = lower_expr(value, builder, ctx);
            builder.slots.insert(*obj_slot, obj_temp);
            for (field_name, binding_slot, field_ty) in fields {
                let _key_temp = builder.const_temp(Const::Str(field_name.clone()));
                let dst = builder.alloc_temp(field_ty.clone());
                builder.emit(Instruction::FieldGet {
                    dst,
                    object: obj_temp,
                    field: field_name.clone(),
                    result_ty: field_ty.clone(),
                });
                builder.slots.insert(*binding_slot, dst);
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
            let arr_temp = lower_expr(value, builder, ctx);
            builder.slots.insert(*arr_slot, arr_temp);
            for (index, binding_slot, field_ty) in elements {
                let idx_temp = builder.const_temp(Const::Int(*index as i64, Type::Int64));
                let dst = builder.alloc_temp(field_ty.clone());
                builder.emit(Instruction::Index {
                    dst,
                    object: arr_temp,
                    key: idx_temp,
                    result_ty: field_ty.clone(),
                });
                builder.slots.insert(*binding_slot, dst);
            }
            if let Some((rest_slot, rest_ty)) = rest {
                // Rest slicing is handled fully in codegen; emit a copy of the source arr as placeholder.
                let dst = builder.alloc_temp(rest_ty.clone());
                builder.emit(Instruction::Copy { dst, src: arr_temp });
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
            builder.const_temp(Const::Str(s.clone()))
        }
        TypedExpr::BoolLit(b, _) => {
            builder.const_temp(Const::Bool(*b))
        }
        TypedExpr::NullLit(_) => {
            builder.const_temp(Const::Null)
        }

        TypedExpr::LocalGet { slot, ty, .. } => {
            if let Some(&t) = builder.slots.get(slot) {
                t
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
            let lhs = lower_expr(left, builder, ctx);
            let rhs = lower_expr(right, builder, ctx);
            let dst = builder.alloc_temp(result_type.clone());
            builder.emit(Instruction::Binary {
                dst,
                op: *op,
                lhs,
                rhs,
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
            for stmt in stmts {
                lower_stmt(stmt, builder, ctx);
            }
            let result = lower_expr(expr, builder, ctx);
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
            dst
        }

        TypedExpr::MakeArray { elements, ty, .. } => {
            let elem_ty = match ty {
                Type::Array(inner) => *inner.clone(),
                _ => Type::Null,
            };
            let lowered: Vec<Temp> = elements
                .iter()
                .map(|e| lower_expr(e, builder, ctx))
                .collect();
            let dst = builder.alloc_temp(ty.clone());
            builder.emit(Instruction::MakeArray {
                dst,
                elements: lowered,
                elem_ty,
            });
            dst
        }

        TypedExpr::Index { object, key, result_type, .. } => {
            let obj_temp = lower_expr(object, builder, ctx);
            let key_temp = lower_expr(key, builder, ctx);
            let dst = builder.alloc_temp(result_type.clone());
            builder.emit(Instruction::Index {
                dst,
                object: obj_temp,
                key: key_temp,
                result_ty: result_type.clone(),
            });
            dst
        }

        TypedExpr::FieldGet { object, field, result_type, .. } => {
            let obj_temp = lower_expr(object, builder, ctx);
            let dst = builder.alloc_temp(result_type.clone());
            builder.emit(Instruction::FieldGet {
                dst,
                object: obj_temp,
                field: field.clone(),
                result_ty: result_type.clone(),
            });
            dst
        }

        TypedExpr::StringInterp { parts, .. } => {
            lower_string_interp(parts, builder, ctx)
        }

        TypedExpr::Is { expr, pattern, .. } => {
            let val_temp = lower_expr(expr, builder, ctx);
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
            let val_temp = lower_expr(expr, builder, ctx);
            let dst = builder.alloc_temp(Type::Bool);
            let required_fields = pattern_required_fields(pattern);
            builder.emit(Instruction::HasPattern {
                dst,
                val: val_temp,
                pattern: HasDesc { required_fields },
            });
            dst
        }

        TypedExpr::IndexSet { .. } => {
            // IndexSet is handled directly in codegen; lin-ir doesn't lower it.
            builder.alloc_temp(Type::Null)
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
        // Check global function slots.
        if let Some(&fid) = ctx.global_fn_slots.get(slot) {
            let lowered_args: Vec<Temp> = args.iter().map(|a| lower_expr(a, builder, ctx)).collect();
            if is_tail {
                builder.terminate(Terminator::TailCall { args: lowered_args.clone() });
                // Dead block to keep IR valid.
                let post = builder.alloc_block("tco_post");
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
            return dst;
        }
    }

    let fn_temp = lower_expr(func, builder, ctx);
    let lowered_args: Vec<Temp> = args.iter().map(|a| lower_expr(a, builder, ctx)).collect();

    if is_tail {
        builder.terminate(Terminator::TailCall { args: lowered_args.clone() });
        let post = builder.alloc_block("tco_post");
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
    dst
}

fn lower_intrinsic_call(
    name: &str,
    args: &[TypedExpr],
    result_type: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Temp {
    let intrinsic = match name {
        "lin_print" => Intrinsic::Print,
        "lin_to_string" => Intrinsic::ToString,
        "lin_length" => Intrinsic::Length,
        "lin_push" => Intrinsic::Push,
        "concat" => Intrinsic::Concat,
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
    dst
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

    // Allocate result temp in the pre-branch block so it's accessible post-merge.
    let result_dst = builder.alloc_temp(result_type.clone());

    // --- then branch ---
    builder.switch_to(then_block);
    let then_val = lower_expr(then_br, builder, ctx);
    if !builder.is_current_block_terminated() {
        builder.emit(Instruction::Copy { dst: result_dst, src: then_val });
        builder.terminate(Terminator::Jump(merge_block));
    }

    // --- else branch ---
    builder.switch_to(else_block);
    let else_val = lower_expr(else_br, builder, ctx);
    if !builder.is_current_block_terminated() {
        builder.emit(Instruction::Copy { dst: result_dst, src: else_val });
        builder.terminate(Terminator::Jump(merge_block));
    }

    builder.switch_to(merge_block);
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
    let scrut_temp = lower_expr(scrutinee, builder, ctx);
    let merge_block = builder.alloc_block("match_merge");
    let result_dst = builder.alloc_temp(result_type.clone());

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
            builder.emit(Instruction::Copy { dst: result_dst, src: arm_val });
            builder.terminate(Terminator::Jump(merge_block));
        }

        builder.switch_to(next_block);
    }

    // If we fall off the last arm without matching, emit a panic.
    let panic_msg = builder.const_temp(Const::Str("non-exhaustive match".to_string()));
    builder.emit(Instruction::Panic { msg: panic_msg });
    builder.terminate(Terminator::Unreachable);

    builder.switch_to(merge_block);
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
    _ctx: &mut LowerCtx,
) -> PatternTest {
    match pattern {
        TypedMatchPattern::Else => PatternTest::Always,
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
        TypedMatchPattern::Has(tp) => {
            let required_fields = pattern_required_fields(tp);
            let dst = builder.alloc_temp(Type::Bool);
            builder.emit(Instruction::HasPattern {
                dst,
                val: scrut,
                pattern: HasDesc { required_fields },
            });
            PatternTest::Cond(dst)
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
            for field in fields {
                if let Some(slot) = field.binding_slot {
                    let t = builder.alloc_temp(field.ty.clone());
                    builder.emit(Instruction::FieldGet {
                        dst: t,
                        object: scrut,
                        field: field.key.clone(),
                        result_ty: field.ty.clone(),
                    });
                    builder.slots.insert(slot, t);
                }
            }
        }
        TypedPattern::Array { elements, .. } => {
            for (i, elem_pat) in elements.iter().enumerate() {
                let idx_temp = builder.const_temp(Const::Int(i as i64, Type::Int64));
                // We need the element type; infer from the pattern.
                let elem_ty = pattern_elem_type(elem_pat);
                let elem_t = builder.alloc_temp(elem_ty.clone());
                builder.emit(Instruction::Index {
                    dst: elem_t,
                    object: scrut,
                    key: idx_temp,
                    result_ty: elem_ty,
                });
                lower_typed_pattern_bindings(elem_pat, elem_t, builder);
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
    let fid = ctx.alloc_func_id();

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
            inner_builder.emit(Instruction::FieldGet {
                dst: cap_t,
                object: env_temp,
                field: i.to_string(), // env field by index
                result_ty: cap_ty,
            });
            inner_builder.slots.insert(cap.outer_slot, cap_t);
        }
    }

    let ret_temp = lower_expr(body, &mut inner_builder, ctx);
    if !inner_builder.is_current_block_terminated() {
        inner_builder.terminate(Terminator::Return(Some(ret_temp)));
    }

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
        ret_ty: closure_ty,
    });
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
