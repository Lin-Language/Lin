use super::*;


// -------------------------------------------------------------------------
// Call lowering
// -------------------------------------------------------------------------

/// Lower a single call argument, coercing it to the callee's parameter type. When the
/// argument is a closure literal and the parameter declares a callback with a concrete
/// (non-union, non-void) return type, the closure is compiled to return that concrete type
/// (so an AST-compiled higher-order callee receives a raw value), bypassing the uniform
/// boxed-return ABI.
pub(crate) fn lower_call_arg(a: &TypedExpr, param_ty: Option<&Type>, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
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
    // Sealed record literal → sealed record param: construct the packed struct DIRECTLY (each
    // field stored by offset) without the build-boxed-LinObject-then-project round-trip that the
    // generic `lower_expr` + `lower_coerce_arg` path pays (lin_object_alloc + N sets + N
    // lin_object_get). Fires when the param is a sealed scalar record and the argument is an
    // object literal with no spreads and at least the target fields — the standard
    // `try_lower_sealed_literal` preconditions. The `Named`-param analogue lives in
    // `try_lower_sealed_literal_into_named` (called from `lower_call_arg_tracked` above).
    //
    // GATED to records whose fields are ALL scalar or String: no Array / nested-record fields.
    // `try_lower_sealed_literal` calls plain `lower_expr` for each field, which produces
    // unsealed/boxed representations for array literals and nested object literals. When a sealed
    // struct field is an array (e.g. `stopTimes: StopTime[]`), the field value from `lower_expr`
    // is a boxed `Object[]`, but the sealed slot expects a packed `StopTime[]`. Storing verbatim
    // corrupts the struct (the codegen's `sealed_repr_differs` returns false for Array→Array,
    // so no coerce fires). The Coerce path (lower_expr + lower_coerce_arg) handles this correctly
    // via `sealed_project_from_boxed` / `sealed_array_project_owned` inside codegen. Restrict to
    // scalar+String records where the field representation is already correct from `lower_expr`.
    if let Some(pt) = param_ty {
        if is_sealed_scalar_repr(pt) {
            let all_scalar_or_string = match pt {
                Type::Object { fields, .. } =>
                    fields.values().all(|f| f.is_flat_scalar() || f.is_string_ish() || matches!(f, Type::Bool)),
                _ => false,
            };
            if all_scalar_or_string {
                if let Some(t) = try_lower_sealed_literal(a, pt, builder, ctx) {
                    return t;
                }
            }
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
pub(crate) fn move_streamish_arg(arg_ty: &Type, t: Temp, builder: &mut FuncBuilder) {
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
pub(crate) fn try_lower_sealed_literal_into_named(
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

pub(crate) fn lower_call_arg_tracked(
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

pub(crate) fn lower_call(
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
        // WAVE D — LONE `flatMap`: `base.<…>.flatMap(f)` with no downstream combinator stage. Unlike
        // map/filter/reduce/for (intrinsics handled above), `flatMap` is a genuine generic with no
        // intrinsic, so its lone call reaches the generic dispatch below and would run the eager
        // stdlib body. Route it through the CPS fusion engine (loop nest pushing into the result)
        // when the callback is inlinable; bail to the eager call (fall through) otherwise.
        if !partial && !is_tail && callee_is_flatmap(func, ctx) {
            if let Some(out) = lower_flatmap_terminal(args, result_type, builder, ctx) {
                return out;
            }
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
            // PATH-1 in-place packed-array op: a combinator over a PACKED sealed-scalar array
            // receiver is lowered via the matching intrinsic (`lin_for`/`lin_length`) at the
            // receiver's CONCRETE type — skipping the `Json`-param whole-array `sealed_array_to_tagged`
            // materialize and the boxed `std_*` dispatch (cost #2). See
            // `packed_array_combinator_intrinsic_name`.
            if let Some(intr) = packed_array_combinator_intrinsic_name(&sym, args) {
                return lower_intrinsic_call(intr, args, result_type, builder, ctx);
            }
            // SPIKE 6b: redirect a concrete-typed `std/array` length/push to its intrinsic so the
            // receiver is NOT boxed into the `Json` dynamic ABI on entry to the `std_array_*`
            // wrapper (see `array_op_intrinsic_name`). Fail-safe: a union/Json/TypeVar receiver
            // keeps the Named-call path below unchanged. (Disjoint from the packed redirect above:
            // that matches sealed-scalar arrays, this matches Array/FixedArray/Iterator/Str.)
            if let Some(arr_intr) = array_op_intrinsic_name(&sym, args) {
                return lower_intrinsic_call(arr_intr, args, result_type, builder, ctx);
            }
            // FUSED `range(a, b).for(f)` across the module boundary: in the IMPORTING module `for`
            // and `range` are calls to the compiled `std_iter_for` / `std_iter_range` symbols, so the
            // intrinsic-level fusion in `lower_for` never sees them. When `std_iter_for`'s receiver is
            // a direct `range(...)` call, redirect to the `lin_for` intrinsic lowering — which then
            // recognises the range receiver (`range_for_bounds`) and emits the fused counted loop,
            // skipping the materialized range array. Only fires for a literal range receiver; every
            // other `.for` receiver (array / iterator / union / stream) keeps the Named-call path with
            // unchanged semantics. (`for` is the only combinator redirected: it discards its result,
            // so the fused loop is observably identical; map/filter/reduce would need their result
            // arrays and are left alone.)
            if sym == "std_iter_for" && range_for_bounds(&args[0], builder, ctx).is_some() {
                return lower_intrinsic_call("lin_for", args, result_type, builder, ctx);
            }
            // ZERO-ARG WHILE INLINE: `while(() => Boolean)` — the condition-only overload that
            // normally delegates to `whileLoop` (TCO). Intercept here so `lower_zero_arg_while`
            // can splice the body directly into a `while_header → while_exit` loop with no closure
            // alloc and no per-iteration indirect call. The guard is: symbol starts with
            // `std_iter_while` and exactly 1 argument (the `() => Boolean` callback, no iterable).
            // Falls through to the Named-call path when the callback is not inlinable.
            if sym.starts_with("std_iter_while") && args.len() == 1 {
                if let Some(out) = lower_zero_arg_while(&args[0], builder, ctx) {
                    return out;
                }
            }
            // ENTRIES INLINE: `obj.entries(f)` over a typed `{ K: V }` map receiver (Type::Map)
            // with an inlinable capturing lambda. Bypasses the stdlib body that materializes a full
            // entries array via `lin_entries_any(obj).for(f)`, replacing it with a direct LinMap
            // slot-walk loop driven by `lin_map_raw_len`/`lin_map_raw_key_at`/`lin_map_raw_value_at`.
            // Falls through to the Named-call path when the receiver is not a Type::Map or the
            // callback is not inlinable — the guard is conservative (sound fallback).
            if sym.starts_with("std_object_entries") && args.len() == 2 {
                if let Some(out) = lower_entries_inline(args, builder, ctx) {
                    return out;
                }
            }
            // SHORT-CIRCUIT COMBINATOR REDIRECT: when `some`/`every`/`find` is called with a
            // CONCRETE (non-union) array receiver, redirect to the `lin_some`/`lin_every`/`lin_find`
            // intrinsic so the lowerer sees the ORIGINAL call-site lambda (not a monomorphized
            // function-param binding). This lets `inlinable_capturing_lambda` fire → the loop body
            // is spliced inline with `Index { result_ty: T }` (direct sealed-struct access, no
            // `lin_array_get_tagged` materialization). Without this redirect, the call goes through
            // the compiled `some<T>` stdlib body where `f` is an opaque parameter — the inline
            // check fails and every element gets materialized.
            if let Some(intr) = concrete_array_shortcircuit_intrinsic_name(&sym, args) {
                return lower_intrinsic_call(intr, args, result_type, builder, ctx);
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
            // SHORT-CIRCUIT: when the callee is a monomorphized spec of `some`/`every`/`find`
            // (tagged in `combinator_spec_slots` during module pre-scan) AND the receiver is a
            // CONCRETE array/iterator, redirect to the `lin_some`/`lin_every`/`lin_find` intrinsic
            // before lowering any arguments. This lets `lower_some/every/find` see the ORIGINAL
            // call-site arguments (concrete array + inline lambda) rather than the compiled
            // `some$T` body's opaque-param view — `inlinable_capturing_lambda` can fire on the
            // inline lambda, emitting an unboxed loop with `Index { result_ty: T }` (direct
            // sealed-struct access, no per-element `lin_array_get_tagged` materialization).
            if let Some(&spec_name) = ctx.combinator_spec_slots.get(slot) {
                if let Some(intr) = concrete_array_shortcircuit_intrinsic_name_for_spec(spec_name, args) {
                    return lower_intrinsic_call(intr, args, result_type, builder, ctx);
                }
            }
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
                    // D3b: cross-module monomorphized spec (e.g. push$Obj_type_String) receiving a
                    // wider unsealed boxed-object arg — project into a fresh narrower copy so the
                    // extra fields are not visible inside the callee or through the stored element.
                    // Guarded by spec_origin_slots so local anon-param functions (D3a sharing)
                    // are unaffected.
                    if ctx.spec_origin_slots.contains(slot) {
                        if let Some(param_ty) = param_tys.get(i) {
                            if anon_object_slot_repr_differs(&a.ty(), param_ty) {
                                let proj = builder.alloc_temp(param_ty.clone());
                                builder.emit(Instruction::Coerce {
                                    dst: proj,
                                    src: arg,
                                    from_ty: a.ty(),
                                    to_ty: param_ty.clone(),
                                });
                                builder.register_owned(proj, param_ty.clone());
                                return proj;
                            }
                        }
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
                // consumed by the jump and BECOMES the next iteration's param-slot value.
                //
                // OWNERSHIP FIX (the `Trip|Null` tail-recursive-param UAF): a CALLER-OWNED-SHELL
                // box arg (`arg_box_is_caller_owned_shell`: a concrete heap value boxed into a
                // union/Json param, e.g. a `match`-narrowed `Trip` threaded back into the `Trip |
                // Null` param) wraps an inner heap pointer it does NOT own — the inner is owned by
                // the SOURCE temp (the narrowed unbox+retain), which `release_owned_for_tail_call`
                // releases below. In a NON-tail call the box shell is freed after the call and the
                // source release balances it. But in a TAIL call the box LIVES ON in the param
                // slot, so releasing the source frees the box's inner out from under the slot — the
                // next iteration's read (and codegen's release-old) then touches freed memory
                // (ASan: heap-use-after-free in `lin_rc_retain` inside the recursive callee). So
                // the threaded box must take its OWN inner reference: retain it here so the +1 the
                // source held TRANSFERS into the box (source release nets it to zero; the slot now
                // owns a genuine +1, freed by the eventual release-old / teardown). For a union box
                // `Retain` lowers to `lin_tagged_retain` (bumps the inner payload's rc, tag-aware).
                for &shell in &shell_boxes {
                    let sty = builder.temp_types.get(&shell).cloned().unwrap_or(Type::Null);
                    builder.emit(Instruction::Retain { val: shell, ty: sty });
                }
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
            // D3b: indirect (stored-closure) call with a WIDER unsealed object arg — project into
            // a fresh narrower copy so extra fields are not visible inside the callee and the
            // caller's mutation can't affect the closure's copy after the call.
            if let Some(param_ty) = param_tys.get(i) {
                if anon_object_slot_repr_differs(&a.ty(), param_ty) {
                    let proj = builder.alloc_temp(param_ty.clone());
                    builder.emit(Instruction::Coerce {
                        dst: proj,
                        src: arg,
                        from_ty: a.ty(),
                        to_ty: param_ty.clone(),
                    });
                    builder.register_owned(proj, param_ty.clone());
                    return proj;
                }
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

    // UNBOXED SUM TYPE (unboxed-sumtype Stage 3 — indirect-call ABI bridge):
    // An anonymous closure always returns a BOXED TaggedVal* (`TypeVar(MAX)` ABI — see
    // `lower_function_expr_with_id`). When the DECLARED `result_type` is a SumNode-eligible
    // union (e.g. `Result<Int32, String>`), the closure materializes its SumNode to a boxed
    // LinObject and returns it as a +1 TaggedVal*. WITHOUT a Coerce, the repr pass seeds the
    // `Call dst` as `Packed(SumNode)` (from `type_seed(SumNode)`) even though the physical
    // value is a boxed ptr — a CloneBox on that temp calls `lin_rc_retain` on a TaggedVal*,
    // treating offset-0 (the tag byte) as the refcount: silent data corruption.
    //
    // Fix: emit the Call with `ret_ty = TypeVar(MAX)` (the ACTUAL closure return type so the
    // repr pass seeds it `Boxed`), then emit a `Coerce { from: TypeVar(MAX), to: SumNode }`.
    // Codegen compiles that Coerce to `sumnode_project_from_boxed` (the pfb tag-dispatch):
    // TAG_SUMNODE → unwrap+retain; TAG_MAP → project a fresh node. The intermediate box is
    // registered owned (closure SumNode returns ARE +1 — materialization always allocates a
    // fresh box) so scope-exit releases it; the Coerce result (fresh +1 SumNode) is the
    // value consumed downstream.
    if crate::repr::sum_type_eligible(result_type) {
        let json_ty = Type::TypeVar(u32::MAX);
        // Emit the call with the ACTUAL closure ABI return type (boxed union) so the repr
        // pass assigns Boxed(Opaque) to the call result, not Packed(SumNode).
        let call_dst = builder.alloc_temp(json_ty.clone());
        builder.emit(Instruction::Call {
            dst: call_dst,
            callee: CallTarget::Indirect(fn_temp),
            args: lowered_args,
            ret_ty: json_ty.clone(),
        });
        free_arg_box_shells(&shell_boxes, call_dst, builder);
        for (b, bty) in &full_release_boxes {
            builder.emit(Instruction::Release { val: *b, ty: bty.clone() });
        }
        // The closure returns a fresh +1 boxed TaggedVal for SumNode result types (the SumNode
        // is always materialized before returning in anonymous-closure ABI). Register the box
        // as owned so scope-exit releases it after the Coerce consumes it.
        builder.register_owned(call_dst, json_ty.clone());
        // Coerce from boxed → SumNode; codegen compiles this to `sumnode_project_from_boxed`,
        // producing a fresh +1 *SumNode the caller uses. The intermediate box is owned (above)
        // and released at scope-exit; the Coerce result is NOT registered (SumNode ownership is
        // tracked per-construction, not via `needs_owning` — consistent with the construction
        // site in `try_lower_sum_literal` and named call Coerce paths).
        let coerce_dst = builder.alloc_temp(result_type.clone());
        builder.emit(Instruction::Coerce {
            dst: coerce_dst,
            src: call_dst,
            from_ty: json_ty,
            to_ty: result_type.clone(),
        });
        let _ = &escape_lits;
        return coerce_dst;
    }
    // STAGE 3 NullableRecord (indirect-call ABI bridge) — the closure-return analogue of the
    // SumNode case above. An anonymous closure always returns a BOXED TaggedVal* (`TypeVar(MAX)`
    // ABI — see `lower_function_expr_with_id`). When the DECLARED `result_type` is a nullable
    // sealed record (`Trip | Null`), the repr pass would otherwise seed the `Call dst` as
    // `Packed(NullableRecord)` (a RAW sealed-struct pointer) from `type_seed`, even though the
    // physical value is a boxed `TaggedVal*`. The downstream `is Trip` narrowing then calls
    // `lin_box_record` on the already-boxed value (double-box) → schema match fails → the record
    // is mistaken for null. This is exactly why a LOCAL recursive `getTrip`-style scan returning
    // `Trip | Null` (now type-checkable, ADR-082) produced wrong runtime results.
    //
    // Fix (mirrors the SumNode bridge): emit the Call with `ret_ty = TypeVar(MAX)` so the repr
    // pass seeds the result `Boxed`, then Coerce boxed → NullableRecord. Codegen compiles that
    // Coerce via the reverse `nr_proj` path (`compile_ir_coerce_with_repr`): TAG_NULL → null ptr,
    // else `sealed_project_from` into a fresh packed struct — yielding the raw NullableRecord
    // pointer the consumer expects. The intermediate box is registered owned (closure union
    // returns are a fresh +1) so scope-exit releases it after the Coerce consumes it.
    if crate::repr::nullable_sealed_record(result_type).is_some() {
        let json_ty = Type::TypeVar(u32::MAX);
        let call_dst = builder.alloc_temp(json_ty.clone());
        builder.emit(Instruction::Call {
            dst: call_dst,
            callee: CallTarget::Indirect(fn_temp),
            args: lowered_args,
            ret_ty: json_ty.clone(),
        });
        free_arg_box_shells(&shell_boxes, call_dst, builder);
        for (b, bty) in &full_release_boxes {
            builder.emit(Instruction::Release { val: *b, ty: bty.clone() });
        }
        builder.register_owned(call_dst, json_ty.clone());
        let coerce_dst = builder.alloc_temp(result_type.clone());
        builder.emit(Instruction::Coerce {
            dst: coerce_dst,
            src: call_dst,
            from_ty: json_ty,
            to_ty: result_type.clone(),
        });
        builder.register_owned(coerce_dst, result_type.clone());
        let _ = &escape_lits;
        return coerce_dst;
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

pub(crate) fn lower_intrinsic_call(
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
        "lin_some" => return lower_some(args, builder, ctx),
        "lin_every" => return lower_every(args, builder, ctx),
        "lin_find" => return lower_find(args, result_type, builder, ctx),
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
        "lin_stream_sliding" => Intrinsic::StreamSliding,
        "lin_stream_pairwise" => Intrinsic::StreamPairwise,
        "lin_stream_intersperse" => Intrinsic::StreamIntersperse,
        "lin_stream_dedup" => Intrinsic::StreamDedup,
        "lin_stream_zip_with" => Intrinsic::StreamZipWith,
        "lin_stream_count" => Intrinsic::StreamCount,
        "lin_stream_repeat" => Intrinsic::StreamRepeat,
        "lin_stream_cycle" => Intrinsic::StreamCycle,
        "lin_stream_gunzip" => Intrinsic::StreamGunzip,
        "lin_stream_gzip" => Intrinsic::StreamGzip,
        "lin_stream_inflate" => Intrinsic::StreamInflate,
        "lin_stream_deflate" => Intrinsic::StreamDeflate,
        "lin_stream_untar" => Intrinsic::StreamUntar,
        "lin_stream_manifest" => Intrinsic::StreamManifest,
        "lin_stream_files" => Intrinsic::StreamFiles,
        "lin_stream_tar_entries" => Intrinsic::StreamTarEntries,
        "lin_tar_header" => Intrinsic::TarHeader,
        "lin_tar_body" => Intrinsic::TarBody,
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
    // `push(arr, elem)` ownership accounting — three sealed cases:
    //
    // A. `elem` is sealed-repr AND `arr` is a sealed-record array (0xFD pointer-backed OR 0xFE
    //    inline): RC is balanced WITHOUT any extra transfer here.
    //      0xFD: `lin_sealed_ptr_array_push` internally retains the struct pointer (+1). Caller
    //            keeps its own +1; scope-exit release reclaims it. Net: array owns +1, caller
    //            releases +1 at scope exit. Do NOT emit a retain here → would cause leak.
    //      0xFE: `lin_sealed_array_push_struct_retaining` copies the payload + retains heap fields
    //            (+1 each). Caller's struct is released at scope exit (struct header freed +
    //            heap fields −1 each). Net: array owns the heap fields at rc=1; source freed. RC
    //            balanced without any extra ownership transfer.
    //
    // B. `elem` is sealed-repr AND `arr` is a TAGGED array (the `push$Object` materialization
    //    path): codegen MATERIALIZES the sealed struct into a fresh boxed LinObject and stores
    //    THAT — it does NOT store the sealed struct pointer. The source struct must STAY OWNED
    //    (released at scope exit) — skip transfer to avoid double-retain.
    //
    // C. All other cases: standard `transfer_into_container` ownership.
    let push_sealed_elem_into_ptr_array = matches!(intrinsic, Intrinsic::Push)
        && args.last().map(|a| is_sealed_scalar_repr(&a.ty())).unwrap_or(false)
        && args.first().map(|a| is_sealed_scalar_array(&a.ty())).unwrap_or(false);
    let push_sealed_elem_into_tagged = matches!(intrinsic, Intrinsic::Push)
        && args.last().map(|a| is_sealed_scalar_repr(&a.ty())).unwrap_or(false)
        && args.first().map(|a| !is_sealed_scalar_array(&a.ty())).unwrap_or(false);
    if matches!(intrinsic, Intrinsic::Push | Intrinsic::ArraySetDyn | Intrinsic::ObjectSetDyn)
        && !push_sealed_elem_into_tagged
        && !push_sealed_elem_into_ptr_array
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
pub(crate) fn iter_elem_type(iterable_ty: &Type) -> Type {
    match iterable_ty {
        Type::Array(t) | Type::Iterator(t) => (**t).clone(),
        // Frozen<T[]> iterates the same element type as T[].
        Type::Frozen(inner) => iter_elem_type(inner),
        Type::FixedArray(ts) => ts.first().cloned().unwrap_or(Type::Null),
        // Union of iterable types (e.g. T[] | Iterator<T> | Stream<T> from a generic combinator
        // signature): if ALL arms agree on the same concrete element type, use that type. This
        // lets sealed-record array elements skip materialization in for/map/filter/some/every/find
        // even when the iterable is typed as a union (e.g. inside a monomorphized stdlib wrapper).
        Type::Union(arms) => {
            let mut agreed: Option<Type> = None;
            for arm in arms {
                let arm_elem = match arm {
                    Type::Array(t) | Type::Iterator(t) | Type::Stream(t) => Some((**t).clone()),
                    _ => None,
                };
                match (arm_elem, &agreed) {
                    // KEEP: a non-iterable arm (e.g. Null in T[]|Null) means element type is unknowable.
                    (None, _) => return Type::TypeVar(u32::MAX), // non-iterable arm → can't agree
                    (Some(e), None) => agreed = Some(e),
                    (Some(e), Some(prev)) if e == *prev => {} // arms agree
                    // KEEP: union arms have different element types → dynamic dispatch needed at runtime.
                    _ => return Type::TypeVar(u32::MAX), // arms disagree
                }
            }
            // KEEP: empty union (degenerate) → no element type to agree on; AnyVal is safe fallback.
            agreed.unwrap_or(Type::TypeVar(u32::MAX))
        }
        // KEEP: AnyVal/String/Object/Map iterables yield dynamically-typed boxed elements at runtime.
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
pub(crate) fn stream_combinator_intrinsic_name(sym: &str, args: &[TypedExpr]) -> Option<&'static str> {
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
        "sliding" => "lin_stream_sliding",
        "pairwise" => "lin_stream_pairwise",
        "intersperse" => "lin_stream_intersperse",
        "dedup" => "lin_stream_dedup",
        "zipWith" => "lin_stream_zip_with",
        "reduce" => "lin_stream_reduce",
        "find" => "lin_stream_find",
        "some" => "lin_stream_some",
        "every" => "lin_stream_every",
        "while" => "lin_stream_while",
        "for" => "lin_stream_for",
        _ => return None,
    })
}

/// PATH-1 in-place packed-array op dispatch (mechanism (a), representation-dispatched lowering).
///
/// When a `std/iter` or `std/array` combinator is called with a PACKED sealed-scalar array receiver
/// (`arg0` is `is_sealed_scalar_array`), the receiver would otherwise be coerced to the `Json`
/// parameter — materializing the WHOLE packed buffer into a boxed `Object[]` via
/// `lin_sealed_array_to_tagged` (cost #2, the dominant cost) — and then dispatched through the
/// compiled `std_*` symbol that reads each element through the dynamic ABI. Redirecting to the
/// matching INTRINSIC lowering (`lin_for`/`lin_length`/...) makes the op see the receiver at its
/// CONCRETE `Pt[]` type, so `lower_for` etc. emit an in-place index loop over the `0xFE` buffer and
/// `length` loads the u64 size at offset 8 — no whole-array materialize, no `std_*` boxed call.
///
/// Keyed on the exact import symbol so a user function with the same name is never affected (mirrors
/// `stream_combinator_intrinsic_name`). Only the ops whose intrinsic lowering ALREADY handles a
/// packed/concrete `Array(elem)` receiver in place are redirected. `map`/`filter`/`reduce` already
/// reach their inline `lower_*` path (they are generic `<T>`, not `Json`), so they are left alone —
/// the redirect targets the `Json`-typed ops (`for`, `length`) that force the whole-array box.
pub(crate) fn packed_array_combinator_intrinsic_name(sym: &str, args: &[TypedExpr]) -> Option<&'static str> {
    // The receiver must be a PACKED sealed-scalar array; only then is the redirect a win (and sound:
    // the intrinsic lowering reads the packed buffer directly via the same `is_sealed_scalar_array`
    // gate the codegen `Index`/`Length` paths honour).
    if !args.first().map(|a| is_sealed_scalar_array(&a.ty())).unwrap_or(false) {
        return None;
    }
    match sym {
        "std_iter_for" => Some("lin_for"),
        "std_array_length" | "std_iter_length" => Some("lin_length"),
        _ => None,
    }
}

/// SHORT-CIRCUIT COMBINATOR REDIRECT (import-slot path): redirect `std_iter_some/every/find` to
/// their IR intrinsics when the receiver is a CONCRETE sealed-record array or iterator.
/// This path fires when calling an imported (non-generic) stdlib export directly — rare in practice
/// since `some/every/find` are generic and go through monomorphization. Kept for completeness.
///
/// Gate: receiver must be `Array(T)` or `Iterator(T)` with a SEALED RECORD element type.
/// Other receivers are left on the Named-call path unchanged (no regression).
pub(crate) fn concrete_array_shortcircuit_intrinsic_name(sym: &str, args: &[TypedExpr]) -> Option<&'static str> {
    let recv_ty = args.first().map(|a| a.ty())?;
    let elem_ty = match &recv_ty {
        Type::Array(t) | Type::Iterator(t) => t.as_ref(),
        _ => return None,
    };
    if !is_sealed_scalar_repr(elem_ty) {
        return None;
    }
    match sym {
        "std_iter_for" => Some("lin_for"),
        "std_iter_some" => Some("lin_some"),
        "std_iter_every" => Some("lin_every"),
        "std_iter_find" => Some("lin_find"),
        _ => None,
    }
}

/// SHORT-CIRCUIT COMBINATOR REDIRECT (global-fn-slot / monomorphized-spec path): redirect a
/// monomorphized `some$T`/`every$T`/`find$T` call to the matching `lin_some/every/find` intrinsic
/// so the ORIGINAL call-site lambda is passed directly to the intrinsic lowerer, bypassing the
/// compiled spec body where the lambda appears as an opaque function parameter.
///
/// The `spec_name` is the base combinator name ("some"/"every"/"find") from `combinator_spec_slots`.
///
/// Gate: receiver arg must be a concrete Array/Iterator whose element type is a SEALED RECORD
/// (`is_sealed_scalar_repr`). This is the only case where the inline direct-struct-access path
/// (`Index { result_ty: T }`) actually fires — bypassing `lin_array_get_tagged` materialization.
/// Scalar arrays, function arrays (`FilterCriteria[]`), union-element arrays, and TypeVar elements
/// all fall through to the compiled spec body unchanged (no regression).
pub(crate) fn concrete_array_shortcircuit_intrinsic_name_for_spec(
    spec_name: &str,
    args: &[TypedExpr],
) -> Option<&'static str> {
    let recv_ty = args.first().map(|a| a.ty())?;
    let elem_ty = match &recv_ty {
        Type::Array(t) | Type::Iterator(t) => t.as_ref(),
        _ => return None,
    };
    // Only redirect when the element type is a sealed record (packed struct). This is the case
    // where `lower_some/every/find`'s inline path uses `Index { result_ty: elem_ty }` directly,
    // avoiding the `lin_array_get_tagged` materialization. Any other element type (TypeVar, union,
    // Function, Named that resolves to a function, scalar) must stay on the compiled-spec path.
    if !is_sealed_scalar_repr(elem_ty) {
        return None;
    }
    match spec_name {
        "some" => Some("lin_some"),
        "every" => Some("lin_every"),
        "find" => Some("lin_find"),
        _ => None,
    }
}

/// SPIKE (6b monomorphic/specialized dispatch): redirect a concrete-typed `std/array` op
/// (`length`, `push`) to its intrinsic lowering so the receiver is NOT boxed into the `Json`
/// dynamic ABI on entry to the compiled `std_array_*` wrapper. The intrinsic codegen dispatches
/// on the receiver's STATIC type (`Array`→`lin_array_length`/`tagged_array_push`,
/// `Str`→`lin_string_length`, …) and falls back to the dynamic `lin_*_dyn` path only for a
/// genuinely-`Json`/union/TypeVar receiver — which is ALREADY a boxed `TaggedVal` at runtime, so
/// passing it raw is correct. The win: a concrete `Trip[]`/`Token[]` receiver skips the per-call
/// `lin_box_array` + `lin_tagged_free_box` shell churn AND the intrinsic takes the direct typed
/// path instead of `lin_length_dyn`.
///
/// Fail-safe: only fires for a receiver whose static type the intrinsic handles WITHOUT the
/// dynamic fallback (concrete array / string / object / map). A `Json`/union/TypeVar receiver
/// keeps the Named-call path (no behaviour change). `push` additionally requires a concrete
/// `Array` receiver (the intrinsic's element-coercion is keyed on the array's element type).
pub(crate) fn array_op_intrinsic_name(sym: &str, args: &[TypedExpr]) -> Option<&'static str> {
    let export = sym.strip_prefix("std_array_")?;
    let recv_ty = args.first().map(|a| a.ty())?;
    // Concrete (statically-resolved) receiver whose `lin_length` intrinsic branch emits a FAITHFUL
    // direct op with no box. Arrays/iterators/strings only: `lin_array_length`/`lin_string_length`
    // read a real length field. `Object`/`Named`/`Map` are DELIBERATELY excluded — a packed sealed
    // record is NOT a runtime `LinObject`, so the intrinsic's `lin_object_length` would read a
    // struct byte as the count (`length(rec)` → 32 instead of the key count). Those keep the
    // dynamic Named-call wrapper, which boxes/materializes correctly. (This is the documented
    // "specialization machinery has bitten before" hazard — fail safe to the dynamic ABI.)
    let recv_is_concrete_lengthable = matches!(
        &recv_ty,
        Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_)
            | Type::Str | Type::StrLit(_)
    );
    match export {
        // `length(x)` — non-generic `(x: Json)` wrapper; the concrete receiver is otherwise boxed.
        "length" if recv_is_concrete_lengthable => Some("lin_length"),
        // `push(arr, item)` — generic `<T>(arr: T[], item: T)`; redirect only for a concrete array
        // receiver (the Push intrinsic dispatches the element store on the array's element type).
        "push" if matches!(&recv_ty, Type::Array(_)) => Some("lin_push"),
        _ => None,
    }
}

pub(crate) fn safe_combinator_callback_index(name: &str) -> Option<usize> {
    match name {
        "for" | "while" | "map" | "filter" | "find" | "some" | "every"
        | "lin_some" | "lin_every" | "lin_find" => Some(1),
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
pub(crate) fn lower_callback_in_safe_ctx(expr: &TypedExpr, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    ctx.safe_callback_depth += 1;
    let t = lower_expr(expr, builder, ctx);
    ctx.safe_callback_depth -= 1;
    t
}

/// The declared parameter types and return type of a callback expression, if it has a
/// statically-known `Function` type. Used to match the closure's compiled ABI when calling it.
pub(crate) fn callback_signature(expr: &TypedExpr) -> (Vec<Type>, Type) {
    match expr.ty() {
        Type::Function { params, ret, .. } => (params, *ret),
        _ => (vec![], Type::TypeVar(u32::MAX)),
    }
}

