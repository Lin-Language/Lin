use super::*;

// DIVERGENCE from `ir::is_concrete_rc_ty` (rc_elide / ownership_verify): NullableRecord
// (`T|Null` where T is a sealed record) is intentionally absent here. The lowerer handles
// NullableRecord ownership via `is_nullable_sealed_record` / `needs_owning` guards that fire
// BEFORE any `is_rc_type` call site is reached (the `narrowed_from_nullable` branches).
// Adding NullableRecord here would double-retain nullable-record reads → double-free.
// Sum types are also excluded: a SumNode IS refcounted but ownership is tracked via
// construction-site `register_owned` + the runtime KIND_SUMNODE drop walk, not here.
pub fn is_rc_type(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Str
            | Type::StrLit(_)
            | Type::Array(_)
            | Type::FixedArray(_)
            | Type::Object { .. }
            | Type::Map { .. }
            | Type::Iterator(_)
            | Type::Function { .. }
    )
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
pub fn sum_recursive_child_field_ty(obj_ty: &Type, field: &str) -> Option<Type> {
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
pub fn lower_sum_scrutinee_raw(
    object: &TypedExpr,
    builder: &mut FuncBuilder,
    _ctx: &mut LowerCtx,
) -> Option<(Temp, Type)> {
    let TypedExpr::LocalGet { slot, .. } = object else { return None };
    let t = *builder.slots.get(slot)?;
    let stored_ty = builder.temp_types.get(&t).cloned().unwrap_or(Type::Null);
    if crate::repr::sum_type_eligible(&stored_ty) {
        Some((t, stored_ty))
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
/// True when `ty` is a SEALED RECORD — a `Type::Object { sealed: true }` all of whose fields are
/// either unboxed scalars (numeric or Bool) — Stage 1 — OR eligible HEAP fields (String, Array,
/// nested sealed record) — Stage 2. MUST mirror `Codegen::sealed_fields` EXACTLY: the two decide,
/// independently, when the unboxed packed-struct layout applies, so any disagreement would make the
/// lowerer's Coerce-insertion and codegen's representation diverge (a UAF / mis-read). A sealed
/// record is still a concrete refcounted heap value (`is_rc_type` true), so the owning model treats
/// it like any object — only its physical layout and its `emit_release`/construct/field-read codegen
/// differ (routed via the sealed runtime). The function name is kept for call-site stability across
/// the (now generalized) Stage 1 + Stage 2 gate.
/// True when `ty` is a Stage-3 NullableRecord param: `T|Null` where `T` is a sealed record or
/// `Named("T")` resolving to one. Covers both the fully-resolved `Union([Object{sealed}, Null])`
/// (caught by `nullable_sealed_record`) and the `Union([Named("T"), Null])` Named-alias form that
/// appears in self-recursive functions before Named is expanded. Used by `lower_coerce_arg` to
/// avoid boxing a concrete sealed arg into a boxing Coerce for such a param.
pub fn is_nullable_record_param(ty: &Type) -> bool {
    if crate::repr::nullable_sealed_record(ty).is_some() {
        return true;
    }
    // Named-alias union form: Union([Named(n), Null]) or Union([Null, Named(n)]).
    let Type::Union(members) = ty else { return false };
    let mut has_named_non_null = false;
    for m in members {
        match m {
            Type::Null => {}
            Type::Named(_) => { has_named_non_null = true; }
            _ => return false, // other non-null, non-Named variant: not a simple nullable record
        }
    }
    has_named_non_null
}

/// True when `ty` is a sealed record (the PackedStruct gate). Delegates to
/// `crate::repr::sealed_fields` which calls the canonical `Type::sealed_fields`.
pub fn is_sealed_scalar_repr(ty: &Type) -> bool {
    crate::repr::sealed_fields(ty).is_some()
}

/// True when `ty` is `Array(elem)` whose element is a packed-sealed record (the PackedSealedArray
/// gate). Delegates to the canonical `Type::sealed_array_elem` via `crate::repr`.
/// The function name is kept for call-site stability.
pub fn is_sealed_scalar_array(ty: &Type) -> bool {
    Type::sealed_array_elem(ty).is_some()
}

/// True when `param_ty` is an array whose element is a BOXED runtime representation — a generic
/// TypeVar/Json wildcard, a union, a typed map, or an UNSEALED object. These are the params a sealed
/// packed array must be materialized for. A `Named`-element array (an unexpanded self-recursive alias,
/// which the callee reads as the SAME packed sealed struct) and a sealed-Object-element array are NOT
/// boxed and must pass through unchanged.
pub fn param_elem_is_boxed_repr(param_ty: &Type) -> bool {
    match param_ty {
        Type::Array(elem) => matches!(elem.as_ref(),
            // Never = an empty array literal `[]` whose element type was not yet resolved;
            // it is physically a 0xFF tagged array and must be coerced when flowing into a
            // sealed-record array param (e.g. `initial: Transfer[] = []` in an emitter call).
            Type::TypeVar(_) | Type::Union(_) | Type::Map { .. } | Type::Never
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
pub fn sealed_array_arg_materialized(arg_ty: &Type, param_ty: &Type) -> bool {
    is_sealed_scalar_array(arg_ty)
        && !is_sealed_scalar_array(param_ty)
        && (is_union_ty(param_ty) || param_elem_is_boxed_repr(param_ty))
        // A sum-projected arg is handled by `lower_coerce_arg`'s sum arm (which runs BEFORE the
        // union-boundary materialize), so it is NOT materialized to a boxed Object[] here.
        && !sum_arg_projected(arg_ty, param_ty)
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
pub fn sealed_record_arg_materialized(arg_ty: &Type, param_ty: &Type) -> bool {
    is_sealed_scalar_repr(arg_ty) && is_union_ty(param_ty) && !is_union_ty(arg_ty)
        // A sum-eligible param takes `lower_coerce_arg`'s sum arm (project to a fresh `*SumNode`,
        // registered owned), NOT the sealed-record→Json materialize-to-boxed-object path. Without
        // this exclusion the projected SumNode is BOTH full-released right after the call AND
        // released at scope exit → double `lin_sumnode_release` (the `{String:Expr}` map → `match`
        // Num-arm `eval(back)` heap-use-after-free, ADR-062 Stage 3).
        && !sum_arg_projected(arg_ty, param_ty)
}

/// True when a concrete (non-sum, non-`Named`) argument flowing into a Stage-eligible SUM param is
/// PROJECTED into a fresh `*SumNode` by `lower_coerce_arg`'s sum-coercion arm (which emits a `Coerce`
/// boxed→sum and `register_owned`s the result). MUST mirror that trigger exactly. The projected node
/// is a FULLY-OWNED +1 `*SumNode` released by the owning model's scope-exit `lin_sumnode_release` —
/// it is NOT a borrowed-inner `TaggedVal*` shell, so it must be EXCLUDED from the
/// `arg_box_is_caller_owned_shell` / `arg_box_is_caller_owned_scalar_shell` classification. Without
/// this exclusion the arg is BOTH released (sum release) AND shell-freed (`lin_tagged_free_box`
/// reading the SumNode's offset-0 RC as a 16-byte box → mismatched-size dealloc + double free; an
/// ASan heap-use-after-free for a `{String:Expr}` map / sum-union arg read-back, ADR-062 Stage 3).
pub fn sum_arg_projected(arg_ty: &Type, param_ty: &Type) -> bool {
    crate::repr::sum_type_eligible(param_ty)
        && !crate::repr::sum_type_eligible(arg_ty)
        && !matches!(arg_ty, Type::Named(_))
}

/// True when `ty` is an array whose ELEMENTS (transitively) contain a sealed-record array or a sealed
/// scalar record — i.e. a NESTED sealed structure (`Pt[][]`, `{String: Pt[]}` would be a Map). The
/// one-level `is_sealed_scalar_array`/`is_sealed_scalar_repr` don't catch the outer container, but its
/// Coerce result (built by codegen's `array_coerce_elementwise`) is still a fresh +1 owned value that
/// must be released. Mirrors codegen's `Codegen::ty_contains_sealed`, restricted to the array-outer case.
pub fn to_contains_sealed_array(ty: &Type) -> bool {
    fn contains(ty: &Type) -> bool {
        if is_sealed_scalar_array(ty) || is_sealed_scalar_repr(ty) {
            return true;
        }
        match ty {
            Type::Array(t) | Type::Iterator(t) | Type::Shared(t) => contains(t),
            Type::Map { value: t, .. } => contains(t),
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
pub fn nested_sealed_repr_change(from: &Type, to: &Type) -> bool {
    let Type::Array(inner_to) = to else { return false };
    let Type::Array(inner_from) = from else { return false };
    // Mirror codegen's gate: a NESTED sealed re-projection fires only when `to`'s inner contains a
    // sealed structure AND the source inner element is a DIFFERENT type (a boxed view), not a verbatim
    // same-representation array. `Type` PartialEq ignores the `sealed` flag, so equal inners ⇒ no rebuild.
    to_contains_sealed_array(to) && inner_from.as_ref() != inner_to.as_ref()
}

/// True when `ty` is a permissible field of a sealed record. Delegates to the canonical
/// `Type::is_sealed_field` (defined in `lin_check::types`).
pub fn is_sealed_field_ty(ty: &Type) -> bool {
    ty.is_sealed_field()
}

/// A type that participates in the OWNING reference model for var cells / module globals:
/// a cell/global holding such a value owns one independent reference to it. This covers
/// both concrete reference-counted heap values (`is_rc_type`) AND boxed Json/union values
/// (`is_union_ty`). For unions the retain/release carried `ty` causes codegen to dispatch
/// the tag-aware `lin_tagged_retain`/`lin_tagged_release` (which bump/drop the boxed
/// payload's refcount and are null/scalar/cached-box safe). Store, read, release-old and
/// teardown must ALL use this predicate together — an asymmetry causes a double-free
/// (release without matching retain) or a leak (retain without matching release).
pub fn needs_owning(ty: &Type) -> bool {
    is_rc_type(ty) || is_union_ty(ty) || is_nullable_sealed_record(ty)
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
pub fn own_for_store(t: Temp, ty: &Type, builder: &mut FuncBuilder) -> Temp {
    match crate::ownership_verify::owning_strategy(ty) {
        crate::ownership_verify::OwningStrategy::Clone => {
            let dst = builder.alloc_temp(ty.clone());
            builder.emit(Instruction::CloneBox { dst, src: t, ty: ty.clone() });
            dst
        }
        crate::ownership_verify::OwningStrategy::Retain => {
            builder.emit(Instruction::Retain { val: t, ty: ty.clone() });
            t
        }
        crate::ownership_verify::OwningStrategy::Trivial => t,
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
pub fn coerce_and_own_store(t: Temp, value_ty: &Type, slot_ty: &Type, builder: &mut FuncBuilder) -> Temp {
    // Box-shell-reclaim ownership fact (whether widening into the union slot made a fresh distinct
    // shell that must be `FreeBoxShell`d) lives in the ownership authority; `type_repr_differs` is the
    // lower-only repr predicate it requires, passed in.
    let made_fresh_box = crate::ownership_verify::box_shell_reclaim(
        value_ty,
        slot_ty,
        type_repr_differs(value_ty, slot_ty),
    );
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
pub fn own_for_read(t: Temp, ty: &Type, builder: &mut FuncBuilder) -> Temp {
    match crate::ownership_verify::owning_strategy(ty) {
        crate::ownership_verify::OwningStrategy::Clone => {
            let dst = builder.alloc_temp(ty.clone());
            builder.emit(Instruction::CloneBox { dst, src: t, ty: ty.clone() });
            builder.register_owned(dst, ty.clone());
            dst
        }
        crate::ownership_verify::OwningStrategy::Retain => {
            builder.emit(Instruction::Retain { val: t, ty: ty.clone() });
            builder.register_owned(t, ty.clone());
            t
        }
        crate::ownership_verify::OwningStrategy::Trivial => t,
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
pub fn lower_container_base_borrowed(
    object: &TypedExpr,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Option<Temp> {
    let TypedExpr::LocalGet { slot, ty, .. } = object else { return None };
    // The borrowed-container shortcut applies only when the slot's owning strategy is `Retain`
    // (a concrete refcounted heap value). A `Clone`-strategy union/Json slot owns its own box and
    // a `Trivial` scalar carries no reference — neither is a borrowable concrete container. This is
    // exactly `!is_union_ty(ty) && is_rc_type(ty)` (the sets are disjoint), now read off the
    // ownership authority instead of re-derived from the type shape.
    if crate::ownership_verify::owning_strategy(ty) != crate::ownership_verify::OwningStrategy::Retain {
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
pub fn lower_container_base_borrowed_check(object: &TypedExpr, ctx: &LowerCtx) -> bool {
    let TypedExpr::LocalGet { slot, ty, .. } = object else { return false };
    // Mirror `lower_container_base_borrowed`'s strategy gate exactly (Retain == concrete-rc,
    // non-union container).
    if crate::ownership_verify::owning_strategy(ty) != crate::ownership_verify::OwningStrategy::Retain {
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

