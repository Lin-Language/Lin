use super::*;

/// If `ty` is a MAP type (`{ K: V }`), or a `V | Null` union whose sole non-null member is a map,
/// return a reference to that map type. (`None` for record/array/scalar — not a map container.)
fn as_map_ty(ty: &Type) -> Option<&Type> {
    match ty {
        Type::Map { .. } => Some(ty),
        Type::Union(members) => {
            let mut map_ty: Option<&Type> = None;
            for m in members {
                match m {
                    Type::Null => {}
                    Type::Map { .. } if map_ty.is_none() => map_ty = Some(m),
                    // Any non-null, non-map member (or a second map) → not a clean `Map | Null`.
                    _ => return None,
                }
            }
            map_ty
        }
        _ => None,
    }
}

/// The VALUE type of an intermediate index level `parent[key]` for auto-vivification, derived from
/// the PARENT container's static type — NOT from the `Index`'s `result_type`, which the checker can
/// erase to a bare `TypeVar` for a map-of-map read (`m[a][b]` → `TypeVar | Null`), losing the map
/// shape. The parent's type is the authoritative source: indexing a `Map { value, .. }` yields
/// `value`. We vivify `parent[key]` only when (a) the parent IS a map container and (b) that
/// `value` is ITSELF a map — i.e. an intermediate map LEVEL we can auto-create when absent. A map
/// whose value is a non-map (the final leaf set's parent, e.g. `{ String: Int32 }`) is handled by
/// the recursion's base level, not here; a record/array parent returns `None` (records are total;
/// arrays can't be vivified by key).
fn vivifiable_level_map_ty(parent_ty: &Type) -> Option<Type> {
    let Type::Map { value, .. } = as_map_ty(parent_ty)? else {
        return None;
    };
    // The level's own type is `*value`; it is a vivifiable MAP level only if it is itself a map.
    if as_map_ty(value).is_some() {
        Some((**value).clone())
    } else {
        None
    }
}

/// Lower an intermediate index-assignment level as GET-OR-CREATE so a nested write
/// `m[k1][k2]…[kn] = v` succeeds even when intermediate map levels are absent (auto-vivification).
///
/// `object` is the `object` operand of an `IndexSet` (or, recursively, of a parent get-or-create) —
/// i.e. an `Index { parent, key, result_type }` read whose `result_type` is a MAP level (possibly
/// `Map | Null`). Instead of the plain read (which yields `Null` and makes the downstream
/// `lin_map_set` a no-op — the silent-write footgun), this emits:
///
/// ```text
///   t = parent[key]                  ; Index read (Map | Null)
///   if t == null:                    ; absent intermediate
///       m = {}                       ; MakeObject — empty map of the level's value (map) type
///       parent[key] = m              ; IndexSet store-back (retains m)
///       result = box m to (Map|Null) ; same union repr as the read
///   else:
///       result = t                   ; existing map, already owned
/// ```
///
/// The `parent` is itself lowered via `lower_index_get_or_create` so EVERY non-final map level is
/// vivified (outermost-first). The leaf set is the caller's actual `IndexSet` — not vivified.
///
/// Returns a temp of the SAME type/representation the plain read would produce (an owned
/// `Map | Null` box), so the caller's downstream `IndexSet` is unchanged — only it is now
/// guaranteed non-null. Returns `None` (caller falls back to `lower_expr`) when `object` is not a
/// vivifiable map-level index (e.g. a record-field or array-element intermediate).
fn lower_index_get_or_create(
    object: &TypedExpr,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Option<(Temp, Type)> {
    let TypedExpr::Index { object: parent, key, .. } = object else {
        return None;
    };
    // The intermediate level `parent[key]` must be a MAP whose value is ITSELF a map — derived from
    // the PARENT's static type (the `Index`'s own `result_type` can be an erased `TypeVar | Null`
    // for a map-of-map read). `level_map_ty` is the real value-map type of THIS level (what we
    // create/return); the recursion result carries the level's real `Map` shape for shaping.
    let level_map_ty = vivifiable_level_map_ty(&parent.ty())?;
    // The read/return repr for this level: `Map | Null` of the real level map type. This replaces
    // any erased `TypeVar | Null` so the Index/IndexSet/CloneBox all see a concrete map shape.
    let read_ty = Type::Union(vec![level_map_ty.clone(), Type::Null]);

    // Lower the PARENT container, recursively vivifying any deeper intermediate map level. The
    // recursion returns `(temp, real_parent_map_ty)`; a non-vivifiable parent bottoms out at the
    // base container (`LocalGet`/etc.) whose REAL type is `parent.ty()`. The parent's real map type
    // becomes the `obj_ty` we index, so codegen routes the map get/set correctly even when the
    // typed-AST type was erased.
    //
    // We use the parent as a BORROWED write-through base (the read + store-back below write into it
    // without taking ownership), so for a base container we take the borrowed load — NOT the owning
    // `lower_expr`, whose retain would be unbalanced against the single scope-exit release here
    // (over-release → premature free of the container, e.g. a TCO param re-borrowed every iteration).
    let (parent_temp, parent_obj_ty) = match lower_index_get_or_create(parent, builder, ctx) {
        Some((t, ty)) => (t, ty),
        None => {
            let t = lower_container_base_borrowed(parent, builder, ctx)
                .unwrap_or_else(|| lower_expr(parent, builder, ctx));
            (t, parent.ty())
        }
    };

    // Read the current value at parent[key]. A RAW Index (borrowed interior pointer) for the
    // null-test + reuse — NOT the owning CloneBox the generic read path adds; ownership is
    // re-established per-branch below. Lower the key after the base so the borrowed base read
    // strictly dominates the index.
    let key_temp = lower_expr(key, builder, ctx);
    let cur = builder.alloc_temp(read_ty.clone());
    builder.emit(Instruction::Index {
        dst: cur,
        object: parent_temp,
        key: key_temp,
        obj_ty: parent_obj_ty.clone(),
        key_ty: key.ty(),
        result_ty: read_ty.clone(),
    });

    // Null test: `cur == null`.
    let null_c = builder.const_temp(Const::Null);
    let is_null = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::Binary {
        dst: is_null,
        op: BinOp::Eq,
        lhs: cur,
        rhs: null_c,
        operand_ty: read_ty.clone(),
        ty: Type::Bool,
    });

    let create_block = builder.alloc_block("viv_create");
    let have_block = builder.alloc_block("viv_have");
    let merge_block = builder.alloc_block("viv_merge");
    builder.terminate(Terminator::CondJump {
        cond: is_null,
        then_block: create_block,
        else_block: have_block,
    });

    let result_dst = builder.alloc_temp(read_ty.clone());
    let mut incomings: Vec<(Temp, BlockId)> = Vec::new();

    // --- create branch: build an empty map, store it back, box it to the read's union repr ---
    builder.switch_to(create_block);
    builder.push_scope();
    // Fresh empty map of THIS level's map type — its key-kind (int vs string) and nested value
    // type come straight from `level_map_ty`, so deeper writes target the right shape.
    let fresh = builder.alloc_temp(level_map_ty.clone());
    builder.emit(Instruction::MakeObject {
        dst: fresh,
        fields: Vec::new(),
        spreads: Vec::new(),
        computed_fields: Vec::new(),
        ty: level_map_ty.clone(),
        stack: false,
    });
    builder.register_owned(fresh, level_map_ty.clone());
    // RC: a CONCRETE-value map store (`emit_map_set` general path) is RC-NEUTRAL on the inner — it
    // boxes the pointer, `lin_map_set` retains it, then the transient box is released (retain +1 then
    // release -1 net zero). So the slot's owning reference must come from an explicit IR `Retain`
    // here, exactly as the generic `IndexSet` lowering's `transfer_into_container` does for a stored
    // value. Retain `fresh` (rc 1 → 2): one reference for the parent slot, one kept for the merge
    // value (moved into the union box below).
    builder.emit(Instruction::Retain { val: fresh, ty: level_map_ty.clone() });
    // Store it back into parent[key] (re-using the SAME borrowed parent temp and a fresh key temp).
    let store_key = lower_expr(key, builder, ctx);
    builder.emit(Instruction::IndexSet {
        object: parent_temp,
        key: store_key,
        value: fresh,
        obj_ty: parent_obj_ty.clone(),
        key_ty: key.ty(),
        val_ty: level_map_ty.clone(),
    });
    // Box the fresh map into this level's `Map | Null` representation. The concrete→union Coerce
    // (codegen `lin_box_map`) MOVES `fresh`'s pointer into the box WITHOUT a retain, so the box owns
    // `fresh`'s construction +1. Transfer ownership: unregister `fresh`, register the box. The map's
    // two references are now the parent slot (from the Retain above) and this merge box.
    let boxed = coerce_to_slot_type(fresh, &level_map_ty, &read_ty, builder);
    if boxed != fresh {
        builder.unregister_owned(fresh);
    }
    builder.register_owned(boxed, read_ty.clone());
    let create_val = boxed;
    // The boxed value transfers its single +1 up to the enclosing scope (the Phi result); keep it
    // across the branch-scope pop.
    builder.pop_scope_releasing_keep(&[create_val]);
    let create_pred = builder.current_block;
    incomings.push((create_val, create_pred));
    builder.terminate(Terminator::Jump(merge_block));

    // --- have branch: the existing map; take an independent owned reference (CloneBox), exactly
    // as the generic union read path does, so the merge owns a +1 box not a borrowed interior. ---
    builder.switch_to(have_block);
    builder.push_scope();
    let owned = builder.alloc_temp(read_ty.clone());
    builder.emit(Instruction::CloneBox { dst: owned, src: cur, ty: read_ty.clone() });
    builder.register_owned(owned, read_ty.clone());
    let have_val = owned;
    builder.pop_scope_releasing_keep(&[have_val]);
    let have_pred = builder.current_block;
    incomings.push((have_val, have_pred));
    builder.terminate(Terminator::Jump(merge_block));

    // --- merge: phi the two owned union boxes; register the result owned once. ---
    builder.switch_to(merge_block);
    builder.emit(Instruction::Phi {
        dst: result_dst,
        ty: read_ty.clone(),
        incomings,
    });
    builder.register_owned(result_dst, read_ty.clone());
    Some((result_dst, read_ty))
}

pub(crate) fn lower_expr(expr: &TypedExpr, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    // Attribute instructions emitted while lowering this expression to its source span (debug-only
    // metadata for DWARF line tables). Restore the enclosing span afterwards so instructions emitted
    // by the PARENT after this child returns (e.g. a Binary after its operands) get the parent's span,
    // not this child's. No effect on IR semantics or non-debug codegen.
    let saved_span = builder.current_span;
    builder.set_span(expr.span());
    let result = lower_expr_inner(expr, builder, ctx);
    builder.current_span = saved_span;
    result
}

pub(crate) fn lower_expr_inner(expr: &TypedExpr, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
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
            // PATH-1: a packed-array element VIEW slot read as a WHOLE value (not a `p["field"]`
            // const-offset read, which `try_lower_packed_elem_field` already intercepts upstream).
            // Materialize a fresh +1 sealed struct from the recorded `(array, index)` — the same
            // value the generic `Index` path would have produced — and own it for scope-exit release.
            // This is the fallback for any non-field use of the param (passing it whole, storing it).
            if let Some((array, index, elem_ty)) = ctx.packed_elem_slots.get(slot).cloned() {
                let dst = builder.alloc_temp(elem_ty.clone());
                builder.emit(Instruction::Index {
                    dst,
                    object: array,
                    key: index,
                    obj_ty: Type::Array(Box::new(elem_ty.clone())),
                    key_ty: Type::Int64,
                    result_ty: elem_ty.clone(),
                });
                builder.register_owned(dst, elem_ty.clone());
                return dst;
            }
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
                // Stage 3: NullableRecord global `var x: T | Null` read at type `T` (post null-check).
                let narrowed_from_nullable = is_nullable_sealed_record(&gty) && is_sealed_scalar_repr(ty);
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
                if narrowed_from_nullable {
                    // Identity ptr cast (NullableRecord → PackedStruct): retain and own.
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
                // Stage 3 NullableRecord: a `T | Null` slot read at type `T` (the non-null
                // branch after a `?? default` or explicit null-check). The Coerce is an identity
                // ptr cast (NullableRecord → PackedStruct carry the same raw pointer; the branch
                // guarantees non-null). Like `narrowed_to_sealed` (union projection), the Coerce
                // seeds a fresh `Packed(PackedStruct)` for the dst (no allocation, just re-typing),
                // so retain the underlying struct and register it as an owned read.
                let narrowed_from_nullable = is_nullable_sealed_record(&stored_ty)
                    && is_sealed_scalar_repr(ty);
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
                let t = if narrowed || narrowed_from_nullable {
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
                } else if narrowed_from_nullable {
                    // Identity ptr cast (NullableRecord → PackedStruct); retain the underlying
                    // struct so the owning read balances against the scope-exit release.
                    builder.emit(Instruction::Retain { val: t, ty: ty.clone() });
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
                    let made_fresh_box = crate::ownership_verify::box_shell_reclaim(
                        &value.ty(),
                        &cell_ty,
                        type_repr_differs(&value.ty(), &cell_ty),
                    );
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
                let made_fresh_box = crate::ownership_verify::box_shell_reclaim(
                    &value.ty(),
                    &gty,
                    type_repr_differs(&value.ty(), &gty),
                );
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

        TypedExpr::MakeObject { fields, spreads, computed_fields, ty, .. } => {
            // This is the GENERAL (boxed) MakeObject path — a sealed scalar-record TARGET is
            // constructed directly as a packed struct elsewhere (`try_lower_sealed_literal`), so
            // here `ty` is always a boxed object/Json. A field VALUE that is itself a sealed scalar
            // record is a packed struct, NOT a LinMap; storing it raw under TAG_RECORD makes the
            // object's serialize/release walk it as map entries → heap corruption. MATERIALIZE
            // each sealed field value to a boxed LinMap (sealed→Json Coerce) first.
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
            // Lower runtime-computed key–value pairs (only present for Map-typed literals).
            let lowered_computed: Vec<(Temp, Temp)> = computed_fields
                .iter()
                .map(|(key_expr, val_expr)| {
                    let kt = lower_expr(key_expr, builder, ctx);
                    let vt = lower_expr(val_expr, builder, ctx);
                    (kt, vt)
                })
                .collect();
            let dst = builder.alloc_temp(ty.clone());
            builder.emit(Instruction::MakeObject {
                dst,
                fields: lowered_fields,
                spreads: lowered_spreads,
                computed_fields: lowered_computed,
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
                // element as a boxed `LinMap` (the universal Json element representation). Lower
                // the element type to the UNSEALED object form so `coerce_to_slot_type` inserts a
                // sealed→Json MATERIALIZATION per element (the sealed struct is NOT a LinMap —
                // pushing it raw under TAG_RECORD makes the array's release/serialize walk it as
                // map entries → heap corruption).
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
            // Each concrete element boxed into a UNION/Json element slot below produces a FRESH
            // 16-byte `TaggedVal*` shell (`box_value`). `MakeArray`'s tagged push raw-COPIES that
            // shell's 16 bytes into the array slot (a MOVE of the value) and the array owns the
            // copy — but the SOURCE shell is then an orphan: its inner payload's ownership lives in
            // the array's copy (heap inner) or in the copied scalar bytes (scalar inner), so the
            // shell must be reclaimed or it leaks every element. This mirrors the `push()` / `for`
            // shell-reclaim convention (`free_arg_box_shells` / `FreeBoxShellIfDistinct`); without
            // it a `[t, i]: Json[]` (RAPTOR's `setTrip` `[trip, start, end]` kConnections row, and
            // even a plain `[1, 2]: Json[]`) leaks one shell per boxed element per build. Collect the
            // freshly-boxed element shells and shell-free them AFTER the array is built (the 16 bytes
            // are already copied in). Shell-only is correct for every inner kind: a scalar box's
            // value is in the copied bytes; a heap box wraps a pointer the array copy / raw value
            // owns — `lin_tagged_free_box` never touches the inner. Cached-box / null safe.
            let mut fresh_box_shells: Vec<Temp> = Vec::new();
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
                    let ety = e.ty();
                    // A SEALED-record element coerced into a BOXED `LinObject` element (the heap-field
                    // `Trip[]` branch: `elem_ty` is the unsealed Object form) is MATERIALIZED by
                    // `coerce_to_slot_type`'s Coerce — `compile_ir_coerce` builds a FRESH `LinObject`,
                    // retaining the struct's HEAP FIELDS into it, and leaves the source struct fully
                    // INDEPENDENT (its pointer never enters the array; the array owns the fresh box).
                    // So the container-insert ownership rule must NOT fire: a `transfer_into_container`
                    // Retain on the source struct has no balancing release (the array releases the
                    // fresh box, never the struct), so it leaks the whole struct — and with it the
                    // `id` String / `stops` array it recursively owns — every iteration (the
                    // `val t: Trip = mk(i); val arr: Trip[] = [t]` per-iteration leak, ASan-confirmed).
                    // D3b: a wider unsealed Object element projected into a NARROWER unsealed Object
                    // array slot — build a fresh copy carrying only the slot fields, severing sharing
                    // across the insertion boundary. The original `t` keeps its own +1 (released at
                    // scope exit). The fresh projected copy (RC=1 from codegen) is transferred
                    // directly into the array — NO additional retain is needed (it's a Move).
                    // This replaces the element before `transfer_into_container`, which we skip for
                    // the original `t` (it's not going into the array).
                    let (t, ety, d3b_projected) = if anon_object_slot_repr_differs(&ety, &elem_ty) {
                        let proj = builder.alloc_temp(elem_ty.clone());
                        builder.emit(Instruction::Coerce {
                            dst: proj,
                            src: t,
                            from_ty: ety.clone(),
                            to_ty: elem_ty.clone(),
                        });
                        // `proj` is fresh (RC=1); the array's MakeArray takes sole ownership.
                        // Do NOT register for scope-exit (it's transferred into the array).
                        // Do NOT call transfer_into_container (d3b_projected skips it below).
                        (proj, elem_ty.clone(), true)
                    } else {
                        (t, ety, false)
                    };
                    // The source struct keeps its own scope-exit release; only the FRESH box transfers
                    // into the array (a MOVE, balanced by the array's recursive element release). When
                    // the element instead FLOWS (shares its pointer) into the array — every non-sealed
                    // heap element, incl. a String/Array boxed to a union TaggedVal that wraps the same
                    // pointer — the container-insert retain is still required, so this is gated tightly
                    // on the sealed→boxed materialize and everything else stays byte-identical.
                    let materializes_fresh_box = is_sealed_scalar_repr(&ety) && !is_sealed_scalar_repr(&elem_ty);
                    // D3b-projected element is already fresh (RC=1) — skip the retain/transfer that
                    // `transfer_into_container` would emit; the MakeArray takes sole ownership as-is.
                    if !materializes_fresh_box && !d3b_projected {
                        // The array owns a reference to each heap element (lin_array_release
                        // recursively releases them when the array is freed) — apply the standard
                        // container-insert ownership rule on the RAW value before boxing/coercing.
                        // `lin_array_push_tagged` raw-copies the element's TaggedVal struct without
                        // retaining its inner (a MOVE), so a union element is CONSUMED here too —
                        // pass `op_consumes_union = true` so a fresh union element is unregistered.
                        builder.transfer_into_container(t, e, true);
                    }
                    let boxed = coerce_to_slot_type(t, &ety, &elem_ty, builder);
                    // A CONCRETE element boxed into a UNION/Json element slot just produced a FRESH
                    // `TaggedVal*` shell (`box_value`, incl. the sealed→union materialize), distinct
                    // from `t`. The push raw-copies its 16 bytes into the array slot, orphaning the
                    // source shell — schedule it for a shell-free after the array is built. (A union
                    // ELEMENT, `is_union_ty(ety)`, FLOWS its existing box into the array by copy with
                    // no new shell; a concrete NON-union element slot — flat/sealed array, String[],
                    // Int32[] — never routes through a TaggedVal box here. Both excluded, so those
                    // paths stay byte-identical.)
                    if is_union_ty(&elem_ty) && !is_union_ty(&ety) && boxed != t {
                        fresh_box_shells.push(boxed);
                    }
                    boxed
                })
                .collect();
            let dst = builder.alloc_temp(ty.clone());
            builder.emit(Instruction::MakeArray {
                dst,
                elements: lowered,
                elem_ty,
                inline: false,    // escape.rs sets true for non-escaping sealed elements
                columnar: false,  // escape.rs sets true when inline=true AND all-scalar fields
            });
            // Reclaim the freshly-boxed element shells now that their 16 bytes are copied into the
            // array (the inner payload's ownership stays with the array's copy / the raw value's
            // own scope-exit release — shell-only, never the inner). Done AFTER MakeArray so the
            // push has read each shell.
            for shell in fresh_box_shells {
                builder.emit(Instruction::FreeBoxShell { val: shell });
            }
            builder.register_owned(dst, ty.clone());
            dst
        }

        TypedExpr::Index { object, key, result_type, .. } => {
            let obj_ty = object.ty();
            let key_ty = key.ty();
            // FUSED `arr[i]["field"]` over a sealed-record array (Stage 3): same constant-offset
            // scalar load as `arr[i].field`. `object` is an Index of the array; `key` is a literal.
            if let TypedExpr::StringLit(name, _, _) = key.as_ref() {
                // PATH-1: `p["field"]` where `p` is a borrowed packed-element view → const-offset load.
                if let Some(t) = try_lower_packed_elem_field(object, name, result_type, builder, ctx) {
                    return t;
                }
                if let Some(t) = try_lower_sealed_array_field(object, name, result_type, builder, ctx) {
                    return t;
                }
                // FUSED `arr[i]["field"]` over a BOXED `Object[]` of a sealed record (the `Token[]`
                // shape): one borrowed `lin_object_get` instead of materializing the whole element.
                if let Some(t) = try_lower_boxed_array_field(object, name, result_type, builder, ctx) {
                    return t;
                }
            }
            // UNBOXED SUM TYPE (Stage 2/3 — narrow-then-fieldget direct read): a `node["field"]`
            // whose scrutinee is physically a `SumNode` (the slot's STORED type is the sum type —
            // a `match node is Variant => node["field"]` arm, where `obj_ty` here is the NARROWED
            // variant). The default lowering of `node` (a `LocalGet` of the sum slot) emits a
            // union→variant Coerce that MATERIALIZES the node into a fresh boxed/sealed record
            // (`lin_sealed_alloc` + copy payload + read + release) just to read one field already at a
            // constant SumNode payload offset — the `is Num => node["value"]` interp hot-path waste
            // (1.11M throwaway sealed allocs). Read the field DIRECTLY off the raw `*SumNode` instead,
            // bypassing that Coerce. Done for:
            //   - a RECURSIVE CHILD field (`*SumNode` slot): a const-offset borrowed pointer load (the
            //     chained `evalNode(node["left"])` fast path; result repr Packed(SumNode) so the
            //     recursion re-enters the tag switch). The variant carrying a recursive child is NOT
            //     `is_sealed_scalar_repr`, so this is the only path for it.
            //   - a NON-RC SCALAR field (numeric / Bool): a const-offset value load (no alloc, no RC).
            //   - a HEAP field (String/Array/nested-sealed, Stage 3): a const-offset BORROWED pointer
            //     load + Retain (the node owns the interior pointer; the Retain hands the caller an
            //     independent +1 that the owning model releases at scope exit). This eliminates the
            //     `lin_sealed_alloc` + `lin_object_get` round-trip for heap-field SumNodes.
            //
            // The emitted FieldGet carries the SUM type as `obj_ty` (not the narrowed variant), so the
            // Stage-2 verify oracle's `sealed_fields(obj_ty)` is `None` and AGREES with the operand's
            // `Packed(SumNode)` repr — it would otherwise assert "old predicate says Packed(struct),
            // repr says Packed(SumNode)" (the documented trap). Codegen's `compile_ir_field_get`
            // dispatches on that repr (`sumnode_field_get_by_name`), reading by const offset.
            if let TypedExpr::StringLit(name, _, _) = key.as_ref() {
                let recursive_child = sum_recursive_child_field_ty(&obj_ty, name).is_some();
                // Heap-field SumNode Stage 3: `scalar_nonrc` (the fast path for non-RC payload
                // fields) requires that the scrutinee's static `obj_ty` is an OBJECT (a narrowed
                // match-arm variant), NOT the full Union. When `obj_ty` is the whole Union (e.g.
                // `r["type"]` without variant narrowing), the payload offset is variant-dependent
                // and `sumnode_field_get_by_name` only handles recursive children — non-recursive
                // payload reads from an un-narrowed union fall through to the general `Index` path
                // (which materializes the SumNode, then calls `lin_object_get`). Without this guard,
                // discriminant-field reads (`r["type"] == "success"`) on a newly-SumNode-eligible
                // union returned null (discriminant is excluded from the payload map).
                let scalar_nonrc = !is_rc_type(result_type) && matches!(obj_ty, Type::Object { .. });
                // Heap-field SumNode Stage 3: an RC heap field (String/Array) in a heap-field SumNode
                // can be read directly with a Retain (same discipline as sealed record heap field reads).
                // Detect: the scrutinee's stored sum type is now sum-eligible (gate widened), AND the
                // result type is an RC heap type (String/Array). We emit FieldGet then Retain+register_owned.
                let heap_field_in_sumnode = is_rc_type(result_type)
                    && !recursive_child
                    && {
                        // Only when the scrutinee is actually a sum-type slot (not a sealed record that
                        // happens to contain a heap field).
                        matches!(obj_ty, Type::Object { .. })
                            && lower_sum_scrutinee_raw(object, builder, ctx).is_some()
                    };
                if recursive_child || scalar_nonrc || heap_field_in_sumnode {
                    if let Some((obj_temp, sum_ty)) = lower_sum_scrutinee_raw(object, builder, ctx) {
                        // Result type: the child sum type for a recursive child (so it seeds
                        // Packed(SumNode) and the recursion re-enters the tag switch), else the field's
                        // declared `result_type` (a flat scalar or heap ptr).
                        let child_sum_ty = sum_recursive_child_field_ty(&obj_ty, name);
                        let is_recursive = child_sum_ty.is_some();
                        let res_ty = child_sum_ty.unwrap_or_else(|| result_type.clone());
                        let dst = builder.alloc_temp(res_ty.clone());
                        // Pass the NARROWED variant type as `obj_ty` for ALL non-recursive SumNode
                        // direct reads (scalars and heap fields). This lets codegen look up the
                        // correct variant-specific payload offset rather than scanning all variants
                        // with sumnode_field_get_by_name (which gives the wrong offset when the same
                        // field name appears in multiple variants at different positions — e.g. a
                        // shared trailing scalar after a heap-field slot that shifts its position).
                        // The oracle allows a sealed obj_ty + Packed(SumNode) repr (updated at the
                        // two FieldGet oracle sites in repr.rs). For recursive-child fields, use
                        // sum_ty so codegen recognizes the field as a *SumNode child.
                        let field_obj_ty = if is_recursive { sum_ty } else { obj_ty.clone() };
                        builder.emit(Instruction::FieldGet {
                            dst,
                            object: obj_temp,
                            field: name.clone(),
                            obj_ty: field_obj_ty,
                            result_ty: res_ty.clone(),
                        });
                        if heap_field_in_sumnode {
                            // Heap field: the FieldGet yields a BORROWED interior pointer; Retain to
                            // give the caller an independent owned +1. The descriptor drop walk releases
                            // the node's copy; the Retain here gives the consumer its own reference.
                            builder.emit(Instruction::Retain { val: dst, ty: res_ty.clone() });
                            builder.register_owned(dst, res_ty);
                        }
                        // Recursive-child: BORROWED interior *SumNode — no retain (parent owns it via
                        // KIND_SUMNODE drop walk). Scalar: value type — no RC. Neither needs an owned ref.
                        return dst;
                    }
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
            // the original box, once per evaluation). Read the ownership fact from the ownership
            // authority (`Own` = fresh +1; `Borrow` = interior pointer into the container) rather
            // than re-deriving it from type shape here. Computed before `obj_ty`/`key_ty` are moved
            // into the `Index` instruction.
            let result_is_fresh_owned = matches!(
                crate::ownership_verify::index_result_convention(&obj_ty, &key_ty),
                crate::ir::Convention::Own
            );
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
            // UNBOXED SUM TYPE (Stage 3): an `obj[k]` / `arr[i]` whose RESULT is a sum type is
            // PROJECTED back into a FRESH +1 `*SumNode` by codegen — the object/Json arm
            // (`unbox_tagged_val_to_type` → `sumnode_project_from_boxed`, boxing.rs:474) and the
            // array arm (data.rs:422) both `lin_sumnode_alloc` a brand-new, container-independent
            // node (a deep snapshot, NOT a borrowed interior pointer). So `dst` is ALREADY owned and
            // stable. The generic union `CloneBox` below would emit `lin_tagged_clone` on the raw
            // node (wrong op for a SumNode) OR — once the repr seed routes it to the SumNode guard —
            // a spurious `lin_rc_retain` that, against the single scope-exit `lin_sumnode_release`,
            // leaks one node per evaluation. Register it owned directly, skipping the clone — exactly
            // like the `result_is_fresh_owned` array-box case below. (This is the same
            // fresh-owned-projection class as the sealed-struct arm further down.)
            if crate::repr::sum_type_eligible(result_type) {
                builder.register_owned(dst, result_type.clone());
                return dst;
            }
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
            // PATH-1: `p.field` where `p` is a borrowed packed-element view → const-offset load.
            if let Some(t) = try_lower_packed_elem_field(object, field, result_type, builder, ctx) {
                return t;
            }
            // FUSED `arr[i].field` over a sealed-record array (Stage 3): a constant-offset scalar
            // load directly from the contiguous element, skipping the per-element struct
            // materialization the generic Index path would do.
            if let Some(t) = try_lower_sealed_array_field(object, field, result_type, builder, ctx) {
                return t;
            }
            // FUSED `arr[i].field` over a BOXED `Object[]` of a sealed record (the `Token[]` shape):
            // one borrowed `lin_object_get` instead of materializing the whole element.
            if let Some(t) = try_lower_boxed_array_field(object, field, result_type, builder, ctx) {
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
            // pointer, dup it (retain + register owned); a scalar needs nothing. This is exactly
            // the owning-read trichotomy — route it through `own_for_read` (→ the ownership
            // authority `owning_strategy`) rather than re-deriving the union/rc/scalar split here.
            own_for_read(dst, result_type, builder)
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
            // Heap-field SumNode Stage 3: if the union is SumNode-eligible, the physical value
            // is a `*SumNode` (TAG_SUMNODE). HasPattern needs a boxed TAG_MAP. Coerce to
            // Json first so codegen materializes the SumNode → boxed LinMap at this boundary.
            let val_temp = if crate::repr::sum_type_eligible(&val_ty) {
                let json = Type::TypeVar(u32::MAX);
                let dst_coerce = builder.alloc_temp(json.clone());
                builder.emit(Instruction::Coerce {
                    dst: dst_coerce, src: raw, from_ty: val_ty.clone(), to_ty: json,
                });
                dst_coerce
            } else {
                box_to_json(raw, &val_ty, builder)
            };
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
                        // The assignment evaluates to the assigned value (spec §8). Unlike the
                        // generic path, `FieldSet` does NOT transfer/consume the source (codegen
                        // retains its own field reference and releases the old), so `val_temp` is
                        // still the scope-owned value `lower_expr` produced — return it directly as
                        // the (already independently-owned) result. Adding another `own_for_read`
                        // here would leave an unbalanced +1 (a per-write leak).
                        return val_temp;
                    }
                }
            }
            // Borrow the container (a bare `LocalGet` of a concrete-RC array/object): a store
            // writes THROUGH the container without needing to own it, so skip the retain/release
            // the owning load would emit. The KEY and VALUE are lowered FIRST so the borrowed base
            // load is the last thing before the `IndexSet` — neither sub-expression can then leave
            // a dangling borrow by reassigning the container global. The owning fallback keeps the
            // original base→key→value order (its retain makes ordering immaterial).
            // `obj_ty` defaults to the typed-AST container type, but auto-vivification can replace an
            // erased intermediate type (`TypeVar | Null`) with the real `Map | Null` it reconstructs;
            // codegen routes the leaf set off this type, so adopt the vivify-returned one when it fires.
            let mut obj_ty = obj_ty.clone();
            let (obj_temp, key_temp, val_temp) =
                if lower_container_base_borrowed_check(object, ctx) {
                    let key_temp = lower_expr(key, builder, ctx);
                    let val_temp = lower_expr(value, builder, ctx);
                    let obj_temp = lower_container_base_borrowed(object, builder, ctx)
                        .unwrap_or_else(|| lower_expr(object, builder, ctx));
                    (obj_temp, key_temp, val_temp)
                } else {
                    // AUTO-VIVIFY intermediate map levels (the WRITE counterpart of the read
                    // null-propagation, spec §6.1). For `m[k1][k2] = v` the `object` here is the
                    // intermediate `Index(m, k1)`; if that intermediate is a MAP that is absent,
                    // `lower_index_get_or_create` creates and stores an empty map of its value type
                    // first, so the leaf set below targets a real container instead of silently
                    // no-opping. Records/arrays/scalars return `None` and fall back to the plain read.
                    let obj_temp = match lower_index_get_or_create(object, builder, ctx) {
                        Some((t, real_ty)) => {
                            // Adopt the reconstructed real `Map | Null` container type so codegen's
                            // leaf-set routing sees a concrete map shape, not the erased typed-AST one.
                            obj_ty = real_ty;
                            t
                        }
                        None => lower_expr(object, builder, ctx),
                    };
                    let key_temp = lower_expr(key, builder, ctx);
                    let val_temp = lower_expr(value, builder, ctx);
                    (obj_temp, key_temp, val_temp)
                };
            // D3b: unsealed boxed Object value stored into a narrower unsealed Object Map slot —
            // project into a fresh narrower copy so extra fields are severed before insertion.
            // The projected temp is fresh (RC=1 from codegen projection); the container's
            // `lin_object_set` will retain it, so we use Transfer semantics (unregister) rather
            // than Retain to avoid RC inflation.  The original val_temp stays owned and is
            // released at scope exit (via its existing entry in the owned-temps table).
            let (val_temp, val_ty) = if let Type::Map { value: map_val_ty, .. } = &obj_ty {
                if anon_object_slot_repr_differs(&val_ty, map_val_ty) {
                    let narrow_ty = *map_val_ty.clone();
                    let proj = builder.alloc_temp(narrow_ty.clone());
                    builder.emit(Instruction::Coerce {
                        dst: proj,
                        src: val_temp,
                        from_ty: val_ty.clone(),
                        to_ty: narrow_ty.clone(),
                    });
                    // Fresh — transfer (not retain): unregister so the map's retain is the sole ref.
                    builder.register_owned(proj, narrow_ty.clone());
                    builder.unregister_owned(proj);
                    (proj, narrow_ty)
                } else {
                    (val_temp, val_ty)
                }
            } else {
                (val_temp, val_ty)
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
                && !is_sealed_scalar_array(&obj_ty);
            if !is_sealed_scalar_array(&obj_ty) && !set_sealed_elem_into_tagged {
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
                val_ty: val_ty.clone(),
            });
            // The assignment EXPRESSION evaluates to the assigned value (spec §8 / §27 rule 8).
            // The container now owns ONE reference to the value (supplied by `transfer_into_container`
            // / the runtime store). The result must be an INDEPENDENTLY-owned value so a consuming
            // caller (block tail, `if` merge, `return`) can release it without touching the
            // container's distinct reference — exactly the `LocalSet` model. `own_for_read` retains
            // (concrete rc) / clones the box (union) / is a no-op (scalar) and registers the result
            // for scope-exit release. Done BEFORE `FreeBoxShell` so a union clone reads the box
            // before its orphaned shell is reclaimed.
            let result = own_for_read(val_temp, &val_ty, builder);
            // A fresh union box consumed by `lin_array_set` leaves an orphaned 16-byte shell
            // (the slot owns the inner; the source box header is unreferenced) — free it after
            // the set has read from it. Mirrors the `ArraySetDyn` intrinsic path. (`result` is a
            // DISTINCT box for the union case, so freeing `val_temp`'s shell cannot touch it.)
            if free_shell {
                builder.emit(Instruction::FreeBoxShell { val: val_temp });
            }
            result
        }
    }
}
