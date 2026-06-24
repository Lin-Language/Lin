use super::*;

// -------------------------------------------------------------------------
// Statement lowering
// -------------------------------------------------------------------------

pub(crate) fn lower_stmt(stmt: &TypedStmt, builder: &mut FuncBuilder, ctx: &mut LowerCtx) {
    match stmt {
        TypedStmt::Val { slot, value, ty, name, span } => {
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
                // PATH-2: a packed-elem-view slot registered by the enclosing Block pre-scan —
                // the array and index are already lowered and recorded in `packed_elem_slots`; the
                // element is never materialized here (any field read goes through
                // `try_lower_packed_elem_field`; a whole-value use falls back via the `LocalGet`
                // handler). Skip the `lower_value_into_slot` call entirely for this slot.
                if ctx.packed_elem_slots.contains_key(slot) {
                    // No slot → builder.slots entry is needed: `LocalGet` checks packed_elem_slots
                    // first and returns before reaching the builder.slots lookup.
                    return;
                }
                // CL.4 LSS: if this val is a non-global capturing Function literal, record its
                // TypedExpr so `inlinable_local_fn` can unwrap a `LocalGet{slot}` back to the
                // original lambda body when the value is later passed as a combinator callback.
                // Only non-global (local) function vals participate — top-level functions use the
                // `global_fn_slots` path above and are handled by CL.3 no-capture devirt.
                if matches!(value, TypedExpr::Function { .. })
                    && !ctx.global_fn_slots.contains_key(slot)
                {
                    builder.local_fn_exprs.insert(*slot, value.clone());
                }
                // Store the value in the slot's declared representation: a concrete value
                // bound to a Json/union slot must be boxed so later reads (LocalGet, is/has)
                // see a TaggedVal*. A sealed scalar-record slot bound to an object literal is
                // constructed directly as a packed struct (fast path inside lower_value_into_slot).
                let t = lower_value_into_slot(value, ty, builder, ctx);
                builder.slots.insert(*slot, t);
                // DEBUG (Phase 3): declare this `val` as a named DWARF local so it shows by name
                // in the debugger under `--debug`. Metadata-only (see `Instruction::DebugDeclare`).
                if let Some(n) = name {
                    builder.emit(Instruction::DebugDeclare {
                        temp: t, name: n.clone(), ty: ty.clone(), param_no: None, span: *span,
                    });
                }
                // Also publish top-level vals to their module global (for closure reads).
                // A `val` binding is single-store and never reassigned, so the global is
                // immutable: mark it foldable (`immutable: true`).
                if ctx.global_val_slots.contains_key(slot) {
                    builder.emit(Instruction::GlobalValSet { slot: *slot, value: t, ty: ty.clone(), immutable: true });
                }
            }
        }
        TypedStmt::Var { slot, value, ty, name, span } => {
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
                // DEBUG (Phase 3): declare this plain `var` as a named DWARF local. Cell-backed
                // `var`s (mutably captured by a closure) are handled separately above and are NOT
                // declared — their slot temp is a heap-cell POINTER whose logical value is behind a
                // deref, which the current emission does not model. Metadata-only.
                if let Some(n) = name {
                    builder.emit(Instruction::DebugDeclare {
                        temp: t, name: n.clone(), ty: ty.clone(), param_no: None, span: *span,
                    });
                }
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
                    // ADR-074: an imported overload member carries the exporting module's exact
                    // mangled function name in `symbol`; use it so the target matches the emitted
                    // `{module_key}_{Function.name}`. Ordinary imports use the plain export name.
                    let local_sym = b.symbol.as_deref().unwrap_or(&b.name);
                    let sym = format!("{}_{}", module_key, local_sym);
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
                nonneg: false,
                proven_inbounds: false,
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

