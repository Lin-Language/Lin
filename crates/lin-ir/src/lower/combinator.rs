use super::*;

/// True for a concrete fixed-width scalar (Int*/UInt*/Float*/Bool) — a type carried UNBOXED with no
/// refcount. Used to decide whether a combinator's element read can use the FLAT scalar getter
/// (`combinator_read_elem_ty`): a scalar element has no refcount and a flat representation, so a
/// flat read on a provably-flat source is sound.
pub(crate) fn is_inline_scalar(ty: &Type) -> bool {
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
/// `range` (3-arg)/`iterOf` return `Iterator<…>`, and `map`/`filter` return arrays only via
/// these intrinsics whose declared result is `Array<scalar>` from a flat producer chain — or a
/// non-empty scalar array literal (`[1,2,3]`). A `[]`+push builder returns a plain `Array`, never an
/// `Iterator`, and a bare param/projection is also not trusted.
pub(crate) fn combinator_read_elem_ty(iterable: &TypedExpr, builder: &FuncBuilder, ctx: &LowerCtx) -> Type {
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
pub(crate) fn emit_iterable_len(iterable_temp: Temp, iterable_ty: &Type, builder: &mut FuncBuilder) -> Temp {
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
pub(crate) fn is_provably_flat_producer(expr: &TypedExpr, builder: &FuncBuilder, ctx: &LowerCtx) -> bool {
    match expr {
        // A non-empty scalar array literal lowers to a flat MakeArray.
        TypedExpr::MakeArray { elements, .. } => !elements.is_empty(),
        // A call to a flat-producing builtin/combinator: range/iterOf (Iterator builtins),
        // map/filter (their lowering allocates a flat output for a flat-scalar element), and the
        // flat array allocators. Recognised by the callee slot resolving to one of those intrinsics
        // OR a stdlib export of that name (the importer's `import_fn_slots`).
        TypedExpr::Call { func, .. } => {
            if let TypedExpr::LocalGet { slot, .. } = func.as_ref() {
                if let Some(intr) = builder.intrinsic_slots.get(slot) {
                    return is_flat_producer_name(intr);
                }
                if let Some((sym, _)) = ctx.import_fn_slots.get(slot) {
                    // Imported export symbol is `{module_key}_{name}`, possibly with an ADR-074
                    // overload / monomorph suffix (`std_iter_range$Int32_Int32_84`). Strip the `$…`
                    // before matching the trailing export name — else an overloaded producer
                    // (`range`, the 3-arg rangeStep) is missed and its result reads tagged, not flat.
                    let base = sym.split('$').next().unwrap_or(sym);
                    return base.rsplit('_').next().map(is_flat_producer_export).unwrap_or(false);
                }
                if ctx.flat_producer_spec_slots.contains(slot) {
                    // A monomorphized flat-producer (e.g. `arrayAllocateFilled$Int32`) rehomed as a
                    // local spec resolves via `global_fn_slots`; tagged at module pre-scan (gated on
                    // a trusted std/iter|std/array origin + a flat-producer base name).
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

/// When `iterable` is a direct `range(...)` call, return its bounds and optional step.
/// Used to FUSE `range(a, b[, step]).for(f)` into a counted i32 loop that drives the callback
/// directly, skipping the materialized range array entirely (no array alloc, no N pushes, no N
/// index reads). Returns `(start, end, step)` where `step` is `None` for the 2-arg (step=1) form.
///
/// Recognised callee forms:
/// - Intrinsic slot resolving to `lin_range` (2-arg, step=1).
/// - Import slot whose symbol's trailing name is `range` (2-arg import or step-overload import).
/// - `global_fn_slots` spec tagged in `range2_spec_slots` (monomorphized 2-arg spec, step=1).
/// - `global_fn_slots` spec tagged in `range3_spec_slots` (monomorphized 3-arg step spec).
///
/// `range` is always eager + array-shaped, so the fused loop preserves `for` semantics exactly
/// (the only observable effect of a `for` body is its side effects, executed once per element in
/// order — identical here).
pub(crate) fn range_for_bounds<'a>(
    iterable: &'a TypedExpr,
    builder: &FuncBuilder,
    ctx: &LowerCtx,
) -> Option<(&'a TypedExpr, &'a TypedExpr, Option<&'a TypedExpr>)> {
    if let TypedExpr::Call { func, args, .. } = iterable {
        if let TypedExpr::LocalGet { slot, .. } = func.as_ref() {
            // 2-arg `range` via intrinsic slot (lin_range) or 2-arg import.
            let is_2arg_range = args.len() == 2
                && (builder
                    .intrinsic_slots
                    .get(slot)
                    .map(|intr| intr == "lin_range")
                    .unwrap_or(false)
                    || ctx
                        .import_fn_slots
                        .get(slot)
                        .map(|(sym, _)| {
                            // Strip the overload-disambiguation suffix (`$Int32_Int32_84`) added by
                            // ADR-074 before extracting the trailing export name; without this,
                            // `std_iter_range$Int32_Int32_84`.rsplit('_').next() returns "84", not
                            // "range", and fusion never fires for the 2-arg overload.
                            let base = sym.split('$').next().unwrap_or(sym);
                            base.rsplit('_').next() == Some("range")
                        })
                        .unwrap_or(false)
                    // 2-arg `range` spec rehomed to global_fn_slots after monomorphization.
                    || ctx.range2_spec_slots.contains(slot));
            if is_2arg_range {
                return Some((&args[0], &args[1], None));
            }
            // 3-arg `range(start, end, step)` via import slot or global_fn_slots spec.
            if args.len() == 3 {
                let is_3arg_range = ctx
                    .import_fn_slots
                    .get(slot)
                    .map(|(sym, _)| {
                        let base = sym.split('$').next().unwrap_or(sym);
                        base.rsplit('_').next() == Some("range")
                    })
                    .unwrap_or(false)
                    || ctx.range3_spec_slots.contains(slot);
                if is_3arg_range {
                    return Some((&args[0], &args[1], Some(&args[2])));
                }
            }
        }
    }
    None
}

/// Intrinsic names whose IR lowering allocates a FLAT scalar buffer (so a flat read on the result
/// is sound). `lin_map`/`lin_filter` allocate flat output for a flat-scalar element type.
pub(crate) fn is_flat_producer_name(name: &str) -> bool {
    matches!(name,
        "lin_range" | "lin_map" | "lin_filter"
            | "lin_array_allocate" | "lin_array_allocate_filled")
}

/// Stdlib export names that thinly wrap a flat producer (`range`→`lin_range`, `map`/`filter`, the
/// flat array allocators). Used when a combinator result reaches another combinator via the
/// importer's Named call (`import_fn_slots`) rather than an inlined intrinsic.
pub(crate) fn is_flat_producer_export(name: &str) -> bool {
    matches!(name,
        "range" | "map" | "filter" | "arrayAllocate" | "arrayAllocateFilled")
}

/// True when a (possibly overload/monomorph-mangled) function NAME demangles to a flat-producer
/// export. Strips the `$…` (ADR-074 overload / monomorph) suffix and the leading `module_` prefix
/// before matching. Used to recognise a flat-producer specialization that resolves through
/// `global_fn_slots` — e.g. a monomorphized `arrayAllocateFilled$Int32` rehomed as a local spec,
/// which neither `intrinsic_slots` nor `import_fn_slots` holds.
pub(crate) fn is_flat_producer_spec_name(name: &str) -> bool {
    let base = name.split('$').next().unwrap_or(name);
    let base = base.rsplit('_').next().unwrap_or(base);
    is_flat_producer_export(base)
}

/// True when a spec name (possibly overload/monomorph-mangled) demangles to `range`.
/// Used to tag range-spec slots for `range_for_bounds` fusion detection.
pub(crate) fn is_range_spec_name(name: &str) -> bool {
    let base = name.split('$').next().unwrap_or(name);
    let base = base.rsplit('_').next().unwrap_or(base);
    base == "range"
}


/// A capture-less literal lambda usable for INLINING into a combinator loop (ADR-044): a
/// `TypedExpr::Function` with no captures. Returns its params + body so the caller can bind each
/// param to a loop temp and lower the body inline — no closure alloc, no boxed indirect call. A
/// capturing lambda or a non-literal callback (a stored/passed `Function` value) returns `None` and
/// the caller falls back to the closure-call path.
pub(crate) fn inlinable_lambda(expr: &TypedExpr) -> Option<(&[TypedParam], &TypedExpr)> {
    match expr {
        TypedExpr::Function { params, body, captures, .. } if captures.is_empty() => {
            Some((params, body))
        }
        _ => None,
    }
}

/// A literal lambda at a combinator call site that is inlinable EVEN IF it captures. Returns its
/// params + body. The captured outer slots are NOT rebound by the inliner — when the body is spliced
/// into the enclosing function, its `LocalGet`/`CellGet`/`CellSet`/`GlobalValGet`/`GlobalValSet` on a
/// captured slot resolve THROUGH the enclosing builder's `slots`/`cell_slots`/`global_var_slots` to
/// the very binding the closure captured by reference. This preserves ADR-012 shared-`var`-cell
/// semantics AUTOMATICALLY: a captured local `var` is a `MakeCell` heap cell in the enclosing builder
/// (`cell_slots`), and a captured top-level `var` is a module global (`global_var_slots`); an inlined
/// `CellSet`/`GlobalValSet` hits the same cell/global the boxed closure would have.
///
/// GUARD (productionizing the spike, which "trusted it blindly"): every capture's `outer_slot` must be
/// provably resolvable in the enclosing builder, with a representation that matches the binding there;
/// otherwise bail to the boxed closure path (a sound fallback). A capture is resolvable when its slot
/// is a module global (`global_var_slots`), a heap cell (`cell_slots`), or a plain local value temp
/// (`slots`). A mutable capture MUST resolve to a cell or global (its writes go through one); if it is
/// only a plain local temp here (no cell), inlining a `CellSet` would have no cell to hit — bail. A
/// representation mismatch between `cap.ty` and the binding's stored type would mean the inlined body
/// reads/writes the wrong physical shape — bail.
pub(crate) fn inlinable_capturing_lambda<'a>(
    expr: &'a TypedExpr,
    builder: &FuncBuilder,
    ctx: &LowerCtx,
) -> Option<(&'a [TypedParam], &'a TypedExpr)> {
    let TypedExpr::Function { params, body, captures, .. } = expr else { return None };
    // Capture-less lambdas are handled by the existing `inlinable_lambda` fast path; this predicate
    // is only consulted as the relaxed fallback, but accepting the empty case too is harmless.
    for cap in captures {
        if !capture_resolvable(cap, builder, ctx) {
            return None;
        }
    }
    Some((params, body))
}

/// CL.4 LSS: resolve a callback expr to an inlinable lambda body, returning OWNED clones of
/// params and body. Handles both literal lambdas and stored capturing lambdas (a `LocalGet{slot}`
/// where `builder.local_fn_exprs[slot]` is a capturing Function literal).
///
/// This extends `inlinable_capturing_lambda` to cover the stored-capturing-lambda case: when a
/// lambda is bound via `val cb = (x) => x + local` and passed to a combinator as `arr.map(cb)`,
/// `inlinable_capturing_lambda` sees a `LocalGet` (not a `Function`) and bails to the boxed-closure
/// indirect call. With this, the combinator inline path fires on `cb` too. Returns owned clones
/// because the `builder` borrow (needed to look up `local_fn_exprs`) must not outlive the check —
/// the calling site immediately mutates `builder` to emit IR.
pub(crate) fn inlinable_local_fn(
    expr: &TypedExpr,
    builder: &FuncBuilder,
    ctx: &LowerCtx,
) -> Option<(Vec<TypedParam>, TypedExpr)> {
    // First try the direct inline path (inline literal lambda).
    if let Some((params, body)) = inlinable_capturing_lambda(expr, builder, ctx) {
        return Some((params.to_vec(), body.clone()));
    }
    // CL.4: for a LocalGet, look up the stored lambda expression.
    if let TypedExpr::LocalGet { slot, .. } = expr {
        if let Some(fn_expr) = builder.local_fn_exprs.get(slot) {
            if let Some((params, body)) = inlinable_capturing_lambda(fn_expr, builder, ctx) {
                return Some((params.to_vec(), body.clone()));
            }
        }
    }
    None
}

/// Is a single capture's `outer_slot` resolvable in the enclosing builder with a matching
/// representation? See `inlinable_capturing_lambda` for the rationale.
pub(crate) fn capture_resolvable(cap: &Capture, builder: &FuncBuilder, ctx: &LowerCtx) -> bool {
    let slot = cap.outer_slot;
    // Module-level `var` (global). A captured `var` write becomes a `GlobalValSet` to this slot.
    if ctx.global_var_slots.contains(&slot) {
        // The binding's stored representation (from global_val_slots, else the capture's own ty).
        let gty = ctx.global_val_slots.get(&slot).unwrap_or(&cap.ty);
        return !type_repr_differs(&cap.ty, gty);
    }
    // Mutably-captured `var` materialized as a heap cell in the enclosing builder.
    if let Some(cell_ty) = builder.cell_slots.get(&slot) {
        // A cell read/write goes through this cell; require a matching representation. The inner cell
        // type promotes a Null cell to Json (see the closure-env path), so treat a union cell as a
        // match for a Null/union capture.
        if is_union_ty(cell_ty) {
            return true;
        }
        return !type_repr_differs(&cap.ty, cell_ty);
    }
    // A mutable capture with NO cell/global binding here cannot be inlined: its `CellSet` would have
    // no cell to hit. (A mutable capture is ALWAYS backed by a cell or global in a correct program; a
    // missing one means an analysis gap — bail to the sound boxed path.)
    if cap.is_mutable {
        return false;
    }
    // Immutable capture: the value lives directly in a plain local slot temp. Require it to be present
    // and to share the capture's representation.
    if let Some(&t) = builder.slots.get(&slot) {
        let stored = builder.temp_types.get(&t).unwrap_or(&cap.ty);
        return !type_repr_differs(&cap.ty, stored);
    }
    false
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
pub(crate) fn inline_lambda_body(
    params: &[TypedParam],
    body: &TypedExpr,
    arg_temps: &[(Temp, Type)],
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> (Temp, Type) {
    let (raw, body_ty, boxes) = inline_lambda_body_tracking_elem_boxes(params, body, arg_temps, builder, ctx);
    // Objective C (uniform): reclaim each per-iteration SCALAR→union param-bind box SHELL, distinct
    // from the body result (which the caller owns/consumes). This covers EVERY inline combinator
    // (`for`/`map`/`filter`/`reduce`/`while`/…) where a flat-scalar element is bound to a Json/union
    // lambda param — the bind boxes the scalar (`lin_box_int32`) and the shell was never reclaimed,
    // leaking ~16 B/iter for any value outside the small-int cache. `FreeBoxShellIfDistinct` is
    // shell-only (the payload is a scalar — no inner heap) and cached-box-safe (a cached small-int box
    // is skipped at runtime), and the `IfDistinct(result)` guard prevents freeing a box the body
    // returned AS its result (which the caller still owns). The range-for caller frees these itself
    // (it needs the raw boxes for its own latch placement), so it uses the `_tracking_` variant
    // directly; all other callers get the reclaim here.
    for ebox in &boxes {
        builder.emit(Instruction::FreeBoxShellIfDistinct { val: *ebox, other: raw });
    }
    (raw, body_ty)
}

/// As `inline_lambda_body`, but ALSO returns the SCALAR→union element boxes freshly created by
/// `coerce_arg_to_param_repr` when binding the params. Objective C: when a scalar element (e.g. the
/// `range(0,N).for(n => …)` i32 counter) is bound to a Json/union param, the bind emits a `Coerce`
/// that codegen lowers to `lin_box_int32`/etc — a FRESH +1 TaggedVal SHELL for any value outside the
/// small-int cache. That shell is otherwise never reclaimed (the param bind is deliberately not
/// registered owned, since the temp belongs to the loop), so a body that uses `n` dynamically leaked
/// ~16 B/iter (the documented inline-for element-box leak). The caller frees each returned box's
/// SHELL after the iteration (mirroring the NON-inline `elem_boxes` + `FreeBoxShellIfDistinct` path).
/// Only SCALAR→union coercions are tracked: the payload is a scalar (no inner heap) so freeing the
/// 16-byte shell is sound, and a cached small-int box is skipped by the runtime free (no double-free).
pub(crate) fn inline_lambda_body_tracking_elem_boxes(
    params: &[TypedParam],
    body: &TypedExpr,
    arg_temps: &[(Temp, Type)],
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> (Temp, Type, Vec<Temp>) {
    builder.push_scope();
    // Snapshot: remember which cells already existed before this inline body runs, and which block
    // we start in. Cells created DURING the body (indices >= snapshot) that were created in the
    // current (pre-body) block are freed here at the body scope exit — they are scoped to this
    // iteration and will not be used again. This fixes the per-iteration `var`-cell leak when an
    // inline-inlined outer loop body declares a `var` (e.g. `var arr = []`) that it never passes
    // to an escaping closure: the cell is non-entry-block so the function-exit FreeCell skips it,
    // but it must die at the END of each iteration, not accumulate across the loop.
    // SOUNDNESS: we only free cells created in the START block — a cell created in a sub-branch
    // (conditional/nested if) does NOT dominate the post-body merge point, so we leave it leaking
    // (the existing conservative behaviour). Non-escaping and not the body result are the same
    // guards as the function-exit FreeCell; `raw` is never a cell pointer so the raw-keep is
    // purely defensive.
    let cells_before = builder.created_cells.len();
    let body_start_block = builder.current_block;
    let mut elem_boxes: Vec<Temp> = Vec::new();
    for (i, param) in params.iter().enumerate() {
        if let Some((t, arg_ty)) = arg_temps.get(i) {
            // Coerce the argument to the param's declared representation (e.g. unbox a Json
            // element into a concrete scalar param, or box a scalar into a Json param). For the
            // common monomorphic-scalar case the representations already match and this is a no-op.
            let bound = coerce_arg_to_param_repr(*t, arg_ty, &param.ty, builder);
            // A SCALAR→union bind allocated a fresh box shell (`bound != *t`, arg is a flat scalar,
            // param is a union). Track it so the caller reclaims the shell after the body. Excludes
            // the no-op case (`bound == *t`, same repr) and heap-element coercions (handled elsewhere).
            if bound != *t
                && (arg_ty.is_flat_scalar() || matches!(arg_ty, Type::Bool))
                && is_union_ty(&param.ty)
            {
                elem_boxes.push(bound);
            }
            builder.slots.insert(param.slot, bound);
        }
    }
    let raw = lower_expr(body, builder, ctx);
    let body_ty = builder.temp_types.get(&raw).cloned().unwrap_or_else(|| body.ty());
    // Free `var`-cells created during this inline body, scoped to the pre-body start block.
    // Only if the current block is not already terminated (a diverged/early-exit body skips this,
    // just like the function-exit FreeCell guard). Mirrors the function-exit FreeCell logic.
    if !builder.is_current_block_terminated() {
        let to_free: Vec<(Temp, Type)> = builder.created_cells[cells_before..]
            .iter()
            .filter(|(c, _, blk)| {
                // Only free cells created in the block we started the inline in — dominance.
                *blk == body_start_block
                    && !builder.escaping_cells.contains(c)
                    && *c != raw
            })
            .map(|(c, ty, _)| (*c, ty.clone()))
            .collect();
        for (cell, ty) in to_free {
            builder.emit(Instruction::FreeCell { cell, ty });
        }
    }
    // Release this body scope's own locals, KEEPING the result temp AND the tracked element boxes
    // (the caller frees their shells explicitly after the iteration). The bound param temps were
    // never registered owned here (they belong to the loop), so they are not double-released.
    let mut keep: Vec<Temp> = Vec::with_capacity(1 + elem_boxes.len());
    keep.push(raw);
    keep.extend_from_slice(&elem_boxes);
    builder.pop_scope_releasing_keep(&keep);
    (raw, body_ty, elem_boxes)
}

/// PATH-1 in-place packed iteration eligibility: true iff EVERY use of the element param `slot` in
/// `body` is as the immediate `object` of a field read (`param.field` or `param["literalKey"]`).
///
/// The in-place packed-element VIEW services SCALAR and HEAP (String/Array/nested-record) field
/// reads — `try_lower_packed_elem_field` handles all `is_sealed_heap_field()` types with a
/// const-offset load + Retain. Any OTHER use of the element (passing it whole to a call, storing
/// it, indexing with a non-literal key, comparing it, returning it, spreading it) needs the
/// materialized struct — and materializing-on-demand a `Json`-typed element to feed e.g.
/// `push(out, p)` would defeat the no-materialize goal. So when the body uses the element as
/// anything but a field read, this returns false and the caller falls back to the generic
/// materialize path (identical to today's boxed behaviour — no regression). Bare-key reads (`p[i]`)
/// and whole-value uses are conservatively rejected.
pub(crate) fn elem_used_only_for_scalar_fields(slot: usize, body: &TypedExpr) -> bool {
    // `ok`: this position is NOT a bare element use (it is a field-read object, handled by the caller).
    // Returns false the moment a bare/whole-value use of `slot` is found.
    fn walk(slot: usize, e: &TypedExpr) -> bool {
        match e {
            // A field read whose object is exactly the element param is FINE — and we must NOT
            // descend into the object as a bare use. (A field read on a NESTED expression that
            // merely CONTAINS the param elsewhere is handled by descending into that subexpr.)
            TypedExpr::FieldGet { object, .. } => {
                if matches!(object.as_ref(), TypedExpr::LocalGet { slot: s, .. } if *s == slot) {
                    return true;
                }
                walk(slot, object)
            }
            TypedExpr::Index { object, key, .. } => {
                let obj_is_elem = matches!(object.as_ref(), TypedExpr::LocalGet { slot: s, .. } if *s == slot);
                let key_is_lit = matches!(key.as_ref(), TypedExpr::StringLit(..));
                if obj_is_elem {
                    // `param["literal"]` ok; `param[dynamicKey]` is a whole-value-ish use → reject.
                    return key_is_lit && walk(slot, key);
                }
                walk(slot, object) && walk(slot, key)
            }
            // A bare element-param reference anywhere else is a whole-value use → reject.
            TypedExpr::LocalGet { slot: s, .. } => *s != slot,
            // Structural recursion over every other node.
            TypedExpr::LocalSet { value, .. } => walk(slot, value),
            TypedExpr::BinaryOp { left, right, .. } => walk(slot, left) && walk(slot, right),
            TypedExpr::UnaryOp { operand, .. } => walk(slot, operand),
            TypedExpr::Coerce { expr, .. } => walk(slot, expr),
            TypedExpr::Call { func, args, .. } => walk(slot, func) && args.iter().all(|a| walk(slot, a)),
            TypedExpr::If { cond, then_br, else_br, .. } =>
                walk(slot, cond) && walk(slot, then_br) && walk(slot, else_br),
            TypedExpr::FromJson { value, .. } => walk(slot, value),
            TypedExpr::Match { scrutinee, arms, .. } =>
                walk(slot, scrutinee) && arms.iter().all(|a| walk(slot, &a.body)),
            TypedExpr::Block { stmts, expr, .. } =>
                stmts.iter().all(|s| stmt_ok(slot, s)) && walk(slot, expr),
            // A nested function literal that captures the element would need the whole value — its
            // captures reference outer slots, so conservatively reject if the param slot is named.
            TypedExpr::Function { body, .. } => walk(slot, body),
            TypedExpr::MakeObject { fields, spreads, computed_fields, .. } =>
                fields.iter().all(|(_, v)| walk(slot, v))
                    && spreads.iter().all(|s| walk(slot, s))
                    && computed_fields.iter().all(|(k, v)| walk(slot, k) && walk(slot, v)),
            TypedExpr::MakeArray { elements, .. } => elements.iter().all(|x| walk(slot, x)),
            TypedExpr::IndexSet { object, key, value, .. } =>
                walk(slot, object) && walk(slot, key) && walk(slot, value),
            TypedExpr::StringInterp { parts, .. } => parts.iter().all(|p| interp_part_ok(slot, p)),
            TypedExpr::Is { expr, .. } => walk(slot, expr),
            TypedExpr::Has { expr, .. } => walk(slot, expr),
            TypedExpr::IntLit(..) | TypedExpr::FloatLit(..) | TypedExpr::StringLit(..)
            | TypedExpr::BoolLit(..) | TypedExpr::NullLit(..) => true,
        }
    }
    fn stmt_ok(slot: usize, s: &TypedStmt) -> bool {
        match s {
            TypedStmt::Expr(e) => walk(slot, e),
            TypedStmt::Val { value, .. } => walk(slot, value),
            TypedStmt::Var { value, .. } => walk(slot, value),
            TypedStmt::Destructure { value, .. } => walk(slot, value),
            TypedStmt::ArrayDestructure { value, .. } => walk(slot, value),
            // No element-param use is possible in an import/foreign-import statement.
            TypedStmt::Import { .. } | TypedStmt::ForeignImport { .. } => true,
        }
    }
    fn interp_part_ok(slot: usize, p: &TypedStringPart) -> bool {
        match p {
            TypedStringPart::Expr(e) => walk(slot, e),
            TypedStringPart::Literal(_) => true,
        }
    }
    walk(slot, body)
}


/// PATH-1 in-place packed iteration: inline a lambda body whose ELEMENT param (index 0) is bound to
/// a BORROWED packed-array element VIEW — the recorded `(array, index)` — instead of a materialized
/// struct. `param["field"]` reads inside the body lower to const-offset `SealedArrayFieldGet`
/// (`try_lower_packed_elem_field`); any whole-value use materializes on demand (`LocalGet` fallback).
/// The optional index param (index 1, the 0-based source index) binds to `idx` as normal.
///
/// The element struct is NEVER materialized when the body only does field reads — eliminating the
/// per-element `lin_sealed_alloc`+memcpy AND the `Json`-param re-box+`lin_object_get`. The view is
/// removed when this body returns (its lifetime is exactly this one inlined body), so a later
/// combinator in the same function cannot mis-resolve the slot.
pub(crate) fn inline_lambda_body_packed_view(
    params: &[TypedParam],
    body: &TypedExpr,
    array: Temp,
    index: Temp,
    elem_ty: &Type,
    idx: Temp,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> (Temp, Type) {
    builder.push_scope();
    let elem_slot = params.first().map(|p| p.slot);
    if let Some(slot) = elem_slot {
        ctx.packed_elem_slots.insert(slot, (array, index, elem_ty.clone()));
    }
    // Optional index param: bind exactly as the generic inline path does.
    if let Some(p) = params.get(1) {
        let bound = coerce_arg_to_param_repr(idx, &Type::Int32, &p.ty, builder);
        builder.slots.insert(p.slot, bound);
    }
    let raw = lower_expr(body, builder, ctx);
    let body_ty = builder.temp_types.get(&raw).cloned().unwrap_or_else(|| body.ty());
    builder.pop_scope_releasing_keep(&[raw]);
    if let Some(slot) = elem_slot {
        ctx.packed_elem_slots.remove(&slot);
    }
    (raw, body_ty)
}

/// Coerce `arg` (typed `arg_ty`) to the representation of `param_ty`: box a concrete value into a
/// union/Json param, or unbox a union value into a concrete param; pass through when the
/// representations already match. (A two-directional companion to `coerce_arg_to_param`, which only
/// boxes — the inliner can also need to UNBOX, e.g. a Json element bound to a concrete scalar param.)
pub(crate) fn coerce_arg_to_param_repr(arg: Temp, arg_ty: &Type, param_ty: &Type, builder: &mut FuncBuilder) -> Temp {
    // Two distinct numeric WIDTHS (e.g. an `Int32` element bound to an `Int64` callback param) are the
    // same *representation* (both unboxed scalars, so `type_repr_differs` is false) but NOT the same
    // physical width — a `Coerce` (sext/zext/sitofp/trunc) is still required, else the body computes on
    // the wrong-width value (`shl i32 %x, i64 1` — operand-width mismatch). This fires for a combinator
    // callback whose numeric param widens/narrows the element relative to the source array's element
    // type (`[1,2,3].map((x: Int64) => …)`, or a widening bare-fn callback's eta-expanded inline body).
    let numeric_width_differs =
        arg_ty.is_numeric() && param_ty.is_numeric() && arg_ty != param_ty;
    if !type_repr_differs(arg_ty, param_ty) && !numeric_width_differs {
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
pub(crate) fn call_body_closure(body: Temp, raw_args: &[(Temp, Type)], param_tys: &[Type], ret_ty: &Type, builder: &mut FuncBuilder) -> Temp {
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
pub(crate) fn call_body_closure_with_elem_boxes(body: Temp, raw_args: &[(Temp, Type)], param_tys: &[Type], ret_ty: &Type, builder: &mut FuncBuilder) -> (Temp, Vec<Temp>) {
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
pub(crate) fn narrow_loop_index(i: Temp, builder: &mut FuncBuilder) -> Temp {
    let dst = builder.alloc_temp(Type::Int32);
    builder.emit(Instruction::Coerce {
        dst, src: i, from_ty: Type::Int64, to_ty: Type::Int32,
    });
    dst
}

/// Coerce a concrete argument to a union/Json parameter (box it); pass through otherwise.
pub(crate) fn coerce_arg_to_param(arg: Temp, arg_ty: &Type, param_ty: Option<&Type>, builder: &mut FuncBuilder) -> Temp {
    match param_ty {
        Some(pty) if is_union_ty(pty) && !is_union_ty(arg_ty) => box_to_json(arg, arg_ty, builder),
        _ => arg,
    }
}

/// Allocate an output array whose storage matches `elem_ty`: a flat scalar array for
/// Int32/Int64/Float32/Float64, otherwise a tagged array. Returns (array_temp, is_flat).
pub(crate) fn alloc_output_array(elem_ty: &Type, result_type: &Type, builder: &mut FuncBuilder) -> (Temp, Option<FlatElemKind>) {
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
pub(crate) fn push_output(out: Temp, flat: Option<FlatElemKind>, elem_ty: &Type, val: Temp, val_ty: &Type, borrowed: bool, builder: &mut FuncBuilder) {
    let push_dst = builder.alloc_temp(Type::Null);
    match flat {
        Some(kind) => {
            // Flat arrays store raw scalars at the OUTPUT element width. Coerce the value when it
            // arrived BOXED (Json → unbox) OR as a concrete numeric of a DIFFERENT width than the
            // output element — e.g. an `Int32` element read from a `[1,2,3]` literal pushed into an
            // `Int64`-elem output, which happens when a combinator callback's WIDER param widens the
            // result element (`[1,2,3].map((x: Int64) => …)`, or a widening bare-fn callback whose
            // eta-expanded inline body now reaches this push). Without the width coercion the raw i32
            // feeds the i64 flat-push intrinsic — an LLVM signature mismatch (`Both operands … not of
            // the same type` / wrong push-arg width).
            let scalar = if is_union_ty(val_ty)
                || (val_ty.is_numeric() && elem_ty.is_numeric() && val_ty != elem_ty)
            {
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
            // RECLAIM the fresh per-element box when `box_to_json` allocated one for a CONCRETE `val`
            // (`boxed != val`; skipped when `val` was already a union — that box is owned/freed
            // elsewhere, e.g. the map elem box reclaimed by `free_combinator_elem_box`).
            //
            // The reclaim DEPTH must match what codegen's `Intrinsic::Push` does with the inner:
            //   - the MOVE push (`lin_array_push_tagged`) raw-copies the box's inner into the result
            //     slot WITHOUT retaining — the inner now lives on in the array, so only the orphaned
            //     16-byte shell is reclaimed (`FreeBoxShell`);
            //   - the RETAINING push (`lin_push_dyn`) BUMPS the inner's refcount so the result array
            //     owns its OWN reference — the fresh box still holds `val`'s original +1, which must be
            //     FULLY released (inner decrement + shell) or every element's inner heap value leaks
            //     (the ~88 B/elem `map(src, x => {…})`-into-`Object[]`/`Rec[]` leak).
            // A freshly-boxed concrete `val` is a Json/union value pushed into an `Array`, which is
            // exactly codegen's retaining-push condition (`union_elem_into_concrete`, and for a
            // union/Named result-element type also `arr_elem_dynamic`). So `boxed != val` in this
            // tagged-store branch ALWAYS takes the retaining `lin_push_dyn` → the fresh box still owns
            // `val`'s original +1 after the push and must be FULLY released (inner + shell), NOT just
            // the orphaned shell. `Release` on a Json temp lowers to `lin_tagged_release`.
            if boxed != val {
                builder.emit(Instruction::Release { val: boxed, ty: Type::TypeVar(u32::MAX) });
            }
        }
    }
}

/// True when two types have a different runtime representation such that a value of one
/// must be coerced (boxed/unboxed) to be used as the other. Specifically: one is a
/// union/Json (TaggedVal*) and the other is a concrete type.
pub(crate) fn type_repr_differs(from: &Type, to: &Type) -> bool {
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
    // Stage 3: NullableRecord boundary. A nullable sealed ptr (`T | Null`) and a sealed struct
    // (`T`) or Null have the SAME physical layout on the nullable side (raw ptr). The boundary
    // only fires when one side is NullableRecord and the other is some BOXED/union type (e.g.
    // a bare `Union` that is NOT a nullable sealed record). sealed → NullableRecord and
    // Null → NullableRecord are IDENTITY (no coerce needed); NullableRecord → union/Json IS a
    // change (needs boxing via the null-guarded TAG_RECORD path).
    // Mirror: `is_nullable_sealed_record` is excluded from `is_union_ty`, so the union arm below
    // does NOT fire for `T | Null`. We only need to coerce when NullableRecord meets a boxed slot.
    //
    // IMPORTANT: return false EARLY for the identity cases (sealed↔NullableRecord and Null↔NullableRecord).
    // Without an early return the sealed-mismatch check below (is_sealed_scalar_repr(from) !=
    // is_sealed_scalar_repr(to)) would fire for `Trip → Trip|Null` and wrongly emit a coerce.
    if is_nullable_sealed_record(from) || is_nullable_sealed_record(to) {
        // One side is NullableRecord. Determine whether a coerce is needed.
        if is_nullable_sealed_record(from) && !is_nullable_sealed_record(to) {
            // NullableRecord → sealed T (identity) or Null (identity): no coerce.
            if is_sealed_scalar_repr(to) || matches!(to, Type::Null) {
                return false;
            }
            // NullableRecord → Json/union/boxed: box with null-guard.
            return true;
        }
        if !is_nullable_sealed_record(from) && is_nullable_sealed_record(to) {
            // sealed T → NullableRecord (identity) or Null → NullableRecord (identity): no coerce.
            if is_sealed_scalar_repr(from) || matches!(from, Type::Null) {
                return false;
            }
            // Json/union/boxed → NullableRecord: project.
            return true;
        }
        // Both NullableRecord: same repr, no coerce (equal types short-circuit before we get here).
        return false;
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
    // The sealed-record ARRAY boundary (sealed-records Stage 3). A packed sealed-scalar array
    // (`Pt[]`, elem_tag 0xFE contiguous unboxed buffer) and a BOXED array of the same shape (an
    // `Object[]` of boxed `LinObject`s — e.g. a combinator result `pts.map(p => { ... })` whose
    // lambda returns an UNSEALED object literal) are physically DIFFERENT representations, even
    // though their `Type`s compare equal (PartialEq ignores `sealed`). Without a `Coerce` here, a
    // boxed `Object[]` bound to a `Pt[]`-annotated slot is read by downstream packed-dispatched ops
    // (index / `.for` / field read) as a contiguous struct buffer → garbage (the `7 7` mis-read).
    // Codegen's `compile_ir_coerce` already PROJECTS a boxed array into a fresh packed buffer
    // (`sealed_array_project_owned`) / MATERIALIZES a packed array to a tagged `Object[]`
    // (`sealed_array_to_tagged`) for exactly this Coerce — this arm just makes the lowerer emit it.
    // Gate: exactly one side is a packed sealed-scalar array AND the other is a BOXED-element array
    // (unsealed Object / Json / union / TypeVar / Map element). Mirrors `param_elem_is_boxed_repr`
    // (the function-ARGUMENT boundary's trigger) so a `Named`-element array (a self-recursive alias
    // the callee reads as the SAME packed struct) and same-shape packed↔packed arrays pass through
    // unchanged — and matches codegen's `sealed_array_elem(_).is_some()` XOR gate EXACTLY.
    if is_sealed_scalar_array(from) && param_elem_is_boxed_repr(to) {
        return true;
    }
    if param_elem_is_boxed_repr(from) && is_sealed_scalar_array(to) {
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
pub(crate) fn scalar_numeric_repr_differs(from: &Type, to: &Type) -> bool {
    if !from.is_numeric() || !to.is_numeric() {
        return false;
    }
    // Int vs float: representation differs. Float vs float: differs only across widths
    // (Float32 vs Float64). Int vs int width changes are handled separately by
    // `int_width_repr_differs` (this predicate stays focused on the int↔float / float-width
    // boundary, since a flat-int array's element type matches its members and must NOT be
    // re-strided here).
    from.is_float() != to.is_float() || (from.is_float() && to.is_float() && from != to)
}

/// True when `from` and `to` are BOTH integers of DIFFERENT bit width, so a value of `from`
/// must be physically widened/truncated (sext/zext/trunc) to occupy `to`'s LLVM integer type.
///
/// The motivating bug: a sub-Int32 flat element read inside a branch (`val b: Int32 = if c then
/// uint8arr[i] else 0`) is loaded at its native width (i8) but flows into a PHI typed at the
/// binding's declared width (i32). The PHI codegen does NOT coerce its incomings, so without a
/// widening `Coerce` on the branch value LLVM sees a `phi i32 [ %i8val, … ]` (and a downstream
/// `shl i32 %phi, 8` over an i8 operand) — rejected by the verifier. The same applies to any
/// sub-Int32 element (UInt8/Int8/UInt16/Int16) consumed where a wider int is expected, in an `if`
/// branch OR a `match` arm (both feed a typed PHI through `coerce_to_slot_type`). A direct binding
/// (`val b: Int32 = uint8arr[i]`) reaches the same choke point, so this also makes that path emit
/// the explicit widen rather than relying on a later use site.
///
/// `compile_ir_coerce` picks the extension by the SOURCE type's signedness (sext for signed,
/// zext for unsigned) — so a `UInt8` 0xFF widens to 255, a signed `Int8` -1 to -1.
pub(crate) fn int_width_repr_differs(from: &Type, to: &Type) -> bool {
    from.is_integer()
        && to.is_integer()
        && from.bit_width() != to.bit_width()
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
pub(crate) fn flat_scalar_array_repr_differs(from: &Type, to: &Type) -> bool {
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
pub(crate) fn lower_from_json(
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

pub(crate) fn box_to_json(val: Temp, val_ty: &Type, builder: &mut FuncBuilder) -> Temp {
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
pub(crate) fn lower_range(args: &[TypedExpr], builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
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
pub(crate) fn lower_iter(args: &[TypedExpr], result_type: &Type, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
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

/// How `emit_combinator_loop` obtains the per-iteration element it hands to the body callback.
pub(crate) enum ElemAccess<'a> {
    /// MATERIALIZE the element via `Instruction::Index` (`iterable[i]`) into a fresh temp of the
    /// given type, then pass that temp to the body. The standard tagged/flat read (ADR-044).
    Materialize(&'a Type),
    /// PATH-1 packed VIEW: do NOT materialize — pass the (already-lowered) `iterable` temp itself
    /// to the body so it can register a packed-element view and read fields by const-offset.
    Packed,
}

/// What a combinator loop body decided after running — drives how the latch wires back to the header.
pub(crate) enum LoopFlow {
    /// `for`/`map`/`filter`/fusion: the body fell through (it never asks to stop the loop early);
    /// the latch unconditionally increments and back-edges to the header.
    Fallthrough,
    /// `while`: continue to the next iteration only while `cond` (an i1 Bool temp) is true; a false
    /// predicate (or exhausting the source) EXITS the loop. The body's keep/stop split is emitted
    /// here as a `CondJump` from the body's final block.
    ContinueIf(Temp),
}

/// THE single counted-loop emitter shared by every inline combinator (`for`/`while`/`map`/`filter`,
/// the scalar prelude of `reduce` excepted — see its note — and the fusion appliers, which reach it
/// through `emit_index_loop`). Emits the length-bounded `preheader → header(phi i) → body → latch →
/// header` / `exit` skeleton ONCE, parameterized by:
///
///   - `access`: MATERIALIZE the element (`iterable[i]` via `Index`) vs pass a PACKED view of the
///     iterable (no per-element materialize) — the ONE line that used to fork `emit_index_loop` from
///     `emit_packed_index_loop`.
///   - `body_fn(i, elem_or_iterable, …) -> LoopFlow`: builds the body and returns whether the loop
///     falls through (`for`/`map`/`filter`) or breaks on a false predicate (`while`).
///
/// The body may switch basic blocks (an inner combinator / match / multi-branch `if`, or a
/// filter keep/skip split): the latch is a DEDICATED block that the body's final block jumps into,
/// so the header phi's back-edge predecessor is ALWAYS the latch — no `patch_phi_incoming` needed
/// (the latch-relative patching the two hand-rolled emitters used to do is subsumed by always
/// routing the back-edge through the latch). Leaves the builder positioned in the `exit` block.
///
/// ELEMENT-BOX RC is the body's responsibility and is emitted INSIDE `body_fn` (the latch only holds
/// the index increment): the reclaim discipline is identical to before — this helper centralizes the
/// loop SCAFFOLDING, not the per-element ownership decision. (The `Index` op for a union/Json `elem`
/// allocates a fresh 16-byte `TaggedVal*` shell each iteration; `map`/`filter`/`for` reclaim it via
/// their `free_combinator_*` calls, the move-vs-retain subtlety documented at those call sites.)
pub(crate) fn emit_combinator_loop<F>(
    iterable: Temp,
    iterable_ty: &Type,
    access: ElemAccess,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    body_fn: F,
) where
    F: FnOnce(Temp, Temp, &mut FuncBuilder, &mut LowerCtx) -> LoopFlow,
{
    // len = length(iterable) — tag-checked (0 for a non-array Json) when the iterable is union.
    let len = emit_iterable_len(iterable, iterable_ty, builder);
    let zero = builder.const_temp(Const::Int(0, Type::Int64));

    let preheader = builder.current_block;
    let header = builder.alloc_block("for_header");
    let body = builder.alloc_block("for_body");
    let latch = builder.alloc_block("for_latch");
    let exit = builder.alloc_block("for_exit");

    let i = builder.alloc_temp(Type::Int64);
    let i_next = builder.alloc_temp(Type::Int64);
    builder.terminate(Terminator::Jump(header));

    builder.switch_to(header);
    // The back-edge always flows through `latch` (the sole predecessor of the header on the loop
    // back-edge), so the phi incoming can be recorded directly — no latch-relative patch needed.
    builder.emit(Instruction::Phi {
        dst: i, ty: Type::Int64,
        incomings: vec![(zero, preheader), (i_next, latch)],
    });
    let cond = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::Binary {
        dst: cond, op: BinOp::Lt, lhs: i, rhs: len,
        operand_ty: Type::Int64, ty: Type::Bool,
    });
    builder.terminate(Terminator::CondJump { cond, then_block: body, else_block: exit });

    builder.switch_to(body);
    let elem = match access {
        ElemAccess::Materialize(elem_ty) => {
            // elem = iterable[i]
            let elem = builder.alloc_temp(elem_ty.clone());
            builder.emit(Instruction::Index {
                dst: elem, object: iterable, key: i,
                obj_ty: iterable_ty.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone(),
            nonneg: false,
            });
            elem
        }
        // Packed view: the body receives the iterable temp + index and reads fields by const-offset.
        ElemAccess::Packed => iterable,
    };
    let flow = body_fn(i, elem, builder, ctx);
    // `body_fn` may have switched blocks; whatever block it ended in flows into the latch — either
    // unconditionally (fallthrough) or via the keep/stop predicate (while's early exit).
    match flow {
        LoopFlow::Fallthrough => {
            if !builder.is_current_block_terminated() {
                builder.terminate(Terminator::Jump(latch));
            }
        }
        LoopFlow::ContinueIf(keep) => {
            builder.terminate(Terminator::CondJump { cond: keep, then_block: latch, else_block: exit });
        }
    }

    builder.switch_to(latch);
    let one = builder.const_temp(Const::Int(1, Type::Int64));
    builder.emit(Instruction::Binary {
        dst: i_next, op: BinOp::Add, lhs: i, rhs: one,
        operand_ty: Type::Int64, ty: Type::Int64,
    });
    builder.terminate(Terminator::Jump(header));

    builder.switch_to(exit);
}

/// Materializing index-loop over `iterable` (`for`/`map`/`filter` + the fusion appliers): a thin
/// configuration of [`emit_combinator_loop`] that reads `iterable[i]` into a fresh `elem_ty` temp
/// and always falls through to the latch. `body_fn(i, elem)` builds the body (and owns the
/// element-box reclaim). Leaves the builder in the exit block.
pub(crate) fn emit_index_loop<F: FnOnce(Temp, Temp, &mut FuncBuilder, &mut LowerCtx)>(
    iterable: Temp,
    iterable_ty: &Type,
    elem_ty: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    body_fn: F,
) {
    emit_combinator_loop(iterable, iterable_ty, ElemAccess::Materialize(elem_ty), builder, ctx,
        |i, elem, b, c| {
            body_fn(i, elem, b, c);
            LoopFlow::Fallthrough
        });
}

/// PATH-1 in-place packed iteration loop: a thin configuration of [`emit_combinator_loop`] that does
/// NOT materialize the element — it passes the loop counter `i` (Int64) to `body_fn` along with the
/// (already-lowered) `iterable` temp, so the body can register a packed-element VIEW and read fields
/// by const-offset. `body_fn(i, array, builder, ctx)`.
pub(crate) fn emit_packed_index_loop<F: FnOnce(Temp, Temp, &mut FuncBuilder, &mut LowerCtx)>(
    iterable: Temp,
    iterable_ty: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    body_fn: F,
) {
    emit_combinator_loop(iterable, iterable_ty, ElemAccess::Packed, builder, ctx,
        |i, array, b, c| {
            body_fn(i, array, b, c);
            LoopFlow::Fallthrough
        });
}

// ===========================================================================================
// COMBINATOR-CHAIN FUSION (path-6, mechanism 6a) — fold map/filter transformer stages into the
// terminal's single loop: read the element once from the base source, inline each transformer's
// literal lambda in order (a filter skip jumps to the loop latch; a map rebinds the carried value),
// and run only the terminal's loop — no intermediate array, no per-stage closure call. ONLY literal
// map/filter lambdas with capture-resolvable closures fuse (the inliner's existing gate); a Stream
// receiver stays lazy. Observably identical to the eager per-stage lowering (map/filter are pure,
// order-preserving, total over the element).
// ===========================================================================================

/// One inlinable transformer stage of a fused combinator chain.
pub(crate) enum FuseStage {
    Map { params: Vec<TypedParam>, body: TypedExpr, out_elem_ty: Type },
    Filter { params: Vec<TypedParam>, body: TypedExpr },
    /// `flatMap(f)` where `f: (T, Int32) => U[]`. Unlike Map/Filter (which transform/skip the carried
    /// value in place), a FlatMap stage WRAPS the downstream continuation in an INNER LOOP: it
    /// evaluates `inner = f(elem, idx)` (a fresh `U[]`), then runs ALL downstream stages + the terminal
    /// once per inner element. `inner_elem_ty` is `U` (the inner array's element type). A chain that
    /// contains a FlatMap stage lowers via the CPS engine `emit_flatmap_chain` (single-ownership), NOT
    /// the linear `apply_fuse_stages` path — see `chain_has_flatmap`.
    FlatMap { params: Vec<TypedParam>, body: TypedExpr, inner_elem_ty: Type },
}

/// True when a fused chain contains a FlatMap stage — it must lower via the CPS engine
/// (`emit_flatmap_chain`), since a FlatMap wraps the rest of the pipeline in an inner loop rather than
/// transforming the carried value in place. A flatMap-FREE chain keeps the original linear
/// `apply_fuse_stages`/`emit_fused_loop` lowering byte-identically.
pub(crate) fn chain_has_flatmap(stages: &[FuseStage]) -> bool {
    stages.iter().any(|s| matches!(s, FuseStage::FlatMap { .. }))
}

/// The combinator base name of a (possibly monomorphized) symbol: strip a `$…` monomorph suffix and
/// a leading `std_iter_`/`std_array_` module prefix, returning the bare export name. Used to detect a
/// `flatMap` specialization (`flatMap`, `flatMap$Int32_…`, `std_iter_flatMap`). Returns the matched
/// canonical name for the names the fusion engine cares about, else None.
pub(crate) fn combinator_base_name(sym: &str) -> Option<&'static str> {
    let base = sym.split('$').next().unwrap_or(sym);
    let base = base.rsplit('_').next().unwrap_or(base);
    match base {
        "flatMap" => Some("flatMap"),
        "some" => Some("some"),
        "every" => Some("every"),
        "find" => Some("find"),
        _ => None,
    }
}

/// Resolve `expr` to a combinator NAME ("map"/"filter"/"reduce"/"for"/"range"/"flatMap"/...) when it
/// is a direct call to one (via an intrinsic slot, an imported stdlib export, or — for `flatMap`, a
/// genuine generic with no intrinsic — a monomorphized top-level spec tagged in `combinator_spec_slots`);
/// else None.
pub(crate) fn combinator_callee_name(expr: &TypedExpr, builder: &FuncBuilder, ctx: &LowerCtx) -> Option<&'static str> {
    let TypedExpr::Call { func, args, .. } = expr else { return None };
    let TypedExpr::LocalGet { slot, .. } = func.as_ref() else { return None };
    // A monomorphized `flatMap` spec resolves via `global_fn_slots`, not an intrinsic/import slot.
    if let Some(name) = ctx.combinator_spec_slots.get(slot) {
        return Some(name);
    }
    let trailing = if let Some(intr) = builder.intrinsic_slots.get(slot) {
        intr.strip_prefix("lin_").unwrap_or(intr)
    } else if let Some((sym, _)) = ctx.import_fn_slots.get(slot) {
        // Strip the ADR-074 overload / monomorph suffix (`$Int32_…`) before the trailing-name
        // match, else an overloaded combinator import (`range`, `while`) is missed and its
        // fusion-chain stage is not recognised.
        sym.split('$').next().unwrap_or(sym).rsplit('_').next()?
    } else {
        return None;
    };
    Some(match trailing {
        "map" => "map",
        "filter" => "filter",
        "reduce" => "reduce",
        "for" => "for",
        // `while` has a condition-only 1-arg overload `(f: () => Boolean)` (ADR-081). Only
        // treat this slot as the ITERABLE combinator when ≥2 args are present (iterable + pred);
        // a 1-arg call is a plain Lin function call into the stdlib 1-arg overload body and must
        // NOT be routed to `lower_while`, which unconditionally reads `args[1]`.
        "while" if args.len() >= 2 => "while",
        "range" => "range",
        "flatMap" => "flatMap",
        _ => return None,
    })
}

/// Peel a chain of FUSIBLE map/filter/flatMap stages off a terminal's receiver `recv`, returning the
/// base source + the stages in SOURCE ORDER. Stops at the first non-fusible stage (a non-literal
/// lambda, a heap/non-scalar map output, a Stream receiver, wrong arity), returning THAT stage's call
/// as the `base`.
///
/// BARRIER SPLITS, NOT KILLS (Wave D): a mid-chain unfusable stage does NOT de-fuse the whole chain.
/// The stages peeled BELOW the barrier (downstream) are kept and fused into the terminal; the `base`
/// returned is the barrier call itself, which the caller lowers via `lower_expr` — and that recurses
/// straight back into this terminal lowering, re-running `extract_fuse_chain` on the barrier's own
/// receiver. So the barrier materialises exactly ONE intermediate array between two FUSED runs (a
/// downstream pass over the barrier's output + an upstream pass producing it), never N unfused stages.
pub(crate) fn extract_fuse_chain<'a>(
    recv: &'a TypedExpr,
    builder: &FuncBuilder,
    ctx: &LowerCtx,
) -> (&'a TypedExpr, Vec<FuseStage>) {
    let mut stages: Vec<FuseStage> = Vec::new();
    let mut cur = peel_combinator_coerce(recv, builder, ctx);
    loop {
        let Some(name) = combinator_callee_name(cur, builder, ctx) else { break };
        let TypedExpr::Call { args, .. } = cur else { break };
        let is_map = name == "map";
        let is_filter = name == "filter";
        let is_flat_map = name == "flatMap";
        if (!is_map && !is_filter && !is_flat_map) || args.len() != 2 {
            break;
        }
        if matches!(args[0].ty(), Type::Stream(_)) {
            break;
        }
        let Some((params, body)) = inlinable_local_fn(&args[1], builder, ctx) else { break };
        // REPR GATE (Step 8.1 widening — sound subset): fuse when the value FLOWING INTO this stage
        // has a representation whose per-element materialize-and-reclaim the fused loop's RC discipline
        // covers (`fuse_elem_repr_reclaimable`): an inline scalar (no RC), a SEALED-SCALAR record
        // (`free_combinator_sealed_elem` → `lin_sealed_release`), or a union/Json box
        // (`free_combinator_elem_box_full`). This widens the original scalar-only gate to the
        // sealed-record sources the RAPTOR scan + any record-combinator code use, reusing the SAME
        // materialize-and-release the single-combinator path already trusts. A plain unsealed
        // `Str[]`/`Object[]` element (neither scalar, sealed, nor union) is NOT reclaimed by either
        // helper on the fused drop path, so it still bails to the per-stage lowering.
        let in_ty = iter_elem_type(&args[0].ty());
        if !fuse_elem_repr_reclaimable(&in_ty) {
            break;
        }
        let stage = if is_flat_map {
            // `flatMap(f)` where `f: (T, Int32) => U[]`. The lambda's RESULT is the inner `U[]`;
            // recover `U` from it. The inner element is read per inner-loop iteration by an
            // `Instruction::Index` materialize, subject to the SAME reclaim discipline as the source
            // element (`free_combinator_*`), so it must have a reclaimable repr — else bail.
            let (_, ret) = callback_signature(&args[1]);
            let inner_elem_ty = iter_elem_type(&ret);
            // The inner element is read per inner-loop iteration by an `Instruction::Index`
            // materialize, subject to the SAME reclaim discipline as the source element
            // (`free_combinator_*`), so its repr must be reclaimable — UNLESS it is `Never`, the
            // element type of an `[]`-only flatMap (`x => []`): the inner array is provably empty, so
            // the inner loop runs zero times and nothing is ever materialized to reclaim. Admitting
            // `Never` lets the empty-inner case fuse (and so reclaim its fresh empty `inner` array on
            // the fused drop path) rather than fall back to the per-stage path.
            if !matches!(inner_elem_ty, Type::Never) && !fuse_elem_repr_reclaimable(&inner_elem_ty) {
                break;
            }
            FuseStage::FlatMap { params: params.to_vec(), body: body.clone(), inner_elem_ty }
        } else if is_map {
            let (_, ret) = callback_signature(&args[1]);
            // A map's OUTPUT must be an inline scalar: it becomes the carried value into the next
            // stage / terminal. A scalar carries with no RC (the proven projection case
            // `t => t["dist"]`); a non-scalar map output would be a fresh heap value threaded through
            // further stages whose multi-stage carry RC is out of scope here (it works for a single
            // map-into-terminal via the alias guard + survivor release, but to keep the fuser's
            // invariant simple we require scalar map outputs — the dominant record-projection shape).
            if !is_inline_scalar(&ret) {
                break;
            }
            FuseStage::Map { params: params.to_vec(), body: body.clone(), out_elem_ty: ret }
        } else {
            FuseStage::Filter { params: params.to_vec(), body: body.clone() }
        };
        stages.push(stage);
        cur = peel_combinator_coerce(&args[0], builder, ctx);
    }
    // After peeling, the BASE source element must also have a reclaimable repr. If not, drop the
    // peeled stages so the terminal lowers its receiver via `lower_expr` (which recurses and re-fuses
    // any sub-chain). This is the BARRIER-AT-THE-BASE case: it splits at the base boundary exactly as
    // a mid-chain barrier splits — never an N-stage de-fusion. With the Wave-D heap-element widening
    // (`is_borrowed_heap_elem`) this now fires only for a genuinely non-reclaimable base (e.g. a raw
    // `Map` element), which combinator chains effectively never have (`values()`/`keys()` interpose a
    // fresh scalar/heap array), so it is close to dead in practice.
    if !fuse_elem_repr_reclaimable(&iter_elem_type(&cur.ty())) {
        stages.clear();
    }
    stages.reverse();
    (cur, stages)
}

/// See through a representation-changing `Coerce` that wraps a FUSIBLE combinator call. A generic
/// `std/iter` `map`/`filter` is typed `(T[]) -> T[]` / `(T[]) -> U[]` over a TYPEVAR element, so its
/// result is a BOXED `Object[]`; when bound at a concrete `Trip[]` annotation the checker inserts an
/// `Array(boxed) -> Array(packed-sealed)` projection `Coerce`. That projection exists ONLY to
/// materialize the intermediate per-stage array in the concrete repr — but a FUSED chain never builds
/// that intermediate (it reads elements straight from the base packed source and runs the terminal in
/// one pass). So for the purpose of chain extraction the Coerce is transparent: peel it to the inner
/// combinator call. Only peels an `Array -> Array` Coerce around a recognised combinator call (the
/// exact generic-combinator boundary); any other Coerce is left intact (the chain stops there).
pub(crate) fn peel_combinator_coerce<'a>(expr: &'a TypedExpr, builder: &FuncBuilder, ctx: &LowerCtx) -> &'a TypedExpr {
    if let TypedExpr::Coerce { expr: inner, from, to, .. } = expr {
        if matches!(from, Type::Array(_) | Type::Iterator(_))
            && matches!(to, Type::Array(_) | Type::Iterator(_))
            && combinator_callee_name(inner, builder, ctx).is_some()
        {
            return inner;
        }
    }
    expr
}

/// True when an element of type `ty`, materialized per-iteration by `Instruction::Index` in a fused
/// combinator loop, is correctly handled by the fused drop/consume discipline. Three reclaim classes,
/// each with a proven discipline at every consuming site (`free_combinator_*` / `fm_reclaim_elem`):
///   - an inline scalar (Int/Float/Bool) — no refcount, nothing to reclaim;
///   - a SEALED-SCALAR record — `Instruction::Index` materializes a fresh +1 packed struct,
///     reclaimed by `free_combinator_sealed_elem` (`lin_sealed_release`, heap-field-walking);
///   - a union/Json box — a fresh +1 box, reclaimed (shell + retained inner) by
///     `free_combinator_elem_box_full`;
///   - a plain heap value (`Str`/`Array`/`Object`/`Iterator`) — `Instruction::Index` on an array
///     source lowers to `lin_array_get` + `unbox_ptr`, a BORROWED interior pointer with NO +1. So
///     there is nothing to reclaim: both `free_combinator_*` helpers (union-only / sealed-only) and
///     `fm_reclaim_elem` correctly NO-OP on it. The one hazard — a terminal that MOVES the borrowed
///     element into a result without taking its own reference — cannot arise in the linear fused
///     paths: every array-producing terminal there is gated to `is_inline_scalar(out_elem_ty)` (so a
///     filter-preserve / heap-map-output never fuses), and the `for`/`reduce` terminals consume by
///     reference. The flatMap CPS engine's terminals DO push the (borrowed) survivor, so they push
///     with `borrowed = is_borrowed_heap_elem(ty)` (retain into the result), mirroring the eager
///     `push(result, x)` retain exactly. (WAVE D: widened from the original scalar/sealed/union gate
///     so heap-element SOURCES and heap flatMap INNER arrays — `["a","b"].flatMap(s => [s, s])` — fuse.)
pub(crate) fn fuse_elem_repr_reclaimable(ty: &Type) -> bool {
    is_inline_scalar(ty) || is_sealed_scalar_repr(ty) || is_union_ty(ty) || is_borrowed_heap_elem(ty)
}

/// True when `ty` is a plain heap value (`Str`/`Array`/`Object`/`Iterator`) read from an array source
/// as a BORROWED interior pointer (no +1) by `Instruction::Index`. Such an element needs no reclaim
/// on a drop/consume path, but DOES need a retain when MOVED into a result array (a combinator that
/// pushes it must use `borrowed = true`, mirroring the eager `push`). Excludes sealed records (a
/// fresh +1 materialize) and unions (a fresh +1 box) — those take the dedicated reclaim helpers.
pub(crate) fn is_borrowed_heap_elem(ty: &Type) -> bool {
    !is_sealed_scalar_repr(ty) && !is_union_ty(ty) && is_heap_ty(ty)
}

/// Apply fused stages for a side-effecting terminal (a filter skip jumps to `skip_block`). Returns
/// the carried (value, type) on the keep path. Source-element reclaim: a map consuming the source
/// frees it; a filter drop frees it; a filter-only chain leaves it as the carried value for the
/// terminal to reclaim.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_fuse_stages(
    stages: &[FuseStage],
    mut elem: Temp,
    mut elem_ty: Type,
    iterable_ty: &Type,
    idx: Temp,
    skip_block: BlockId,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Option<(Temp, Type)> {
    let src_elem = elem;
    let src_elem_ty = elem_ty.clone();
    // OWNERSHIP of the per-iteration SOURCE materialize (`src_elem`): once an upstream map CONSUMES
    // it (producing a fresh value that does not alias it), the map FREES it here and the source no
    // longer owns it. A later stage that also tried to free `src_elem` would double-free (the
    // `map.filter.reduce` UAF in `lin_sealed_release`). Track liveness so each `src_elem` reclaim
    // happens exactly once.
    let mut src_alive = true;
    for stage in stages {
        match stage {
            FuseStage::Filter { params, body } => {
                let (pred_raw, pred_ty) =
                    inline_lambda_body(params, body, &[(elem, elem_ty.clone()), (idx, Type::Int32)], builder, ctx);
                let keep = if matches!(pred_ty, Type::Bool) {
                    pred_raw
                } else {
                    let d = builder.alloc_temp(Type::Bool);
                    builder.emit(Instruction::Coerce { dst: d, src: pred_raw, from_ty: pred_ty, to_ty: Type::Bool });
                    d
                };
                let keep_block = builder.alloc_block("fuse_keep");
                let drop_block = builder.alloc_block("fuse_drop");
                builder.terminate(Terminator::CondJump { cond: keep, then_block: keep_block, else_block: drop_block });
                builder.switch_to(drop_block);
                // Reclaim the dropped element: the SOURCE materialize (only if an upstream map has
                // not already consumed it), plus the CURRENT carried value when it is a distinct
                // upstream-map output (a fresh value the map produced and the source no longer owns).
                if src_alive {
                    free_combinator_elem_box_full(src_elem, &src_elem_ty, builder);
                    free_combinator_sealed_elem(src_elem, iterable_ty, &src_elem_ty, builder);
                }
                if elem != src_elem {
                    builder.emit(Instruction::Release { val: elem, ty: elem_ty.clone() });
                }
                builder.terminate(Terminator::Jump(skip_block));
                builder.switch_to(keep_block);
            }
            FuseStage::Map { params, body, out_elem_ty } => {
                let (mapped, mapped_ty) =
                    inline_lambda_body(params, body, &[(elem, elem_ty.clone()), (idx, Type::Int32)], builder, ctx);
                // ALIAS GUARD: if the body returned its INPUT unchanged (`x => x` identity, or any
                // body whose result temp IS the incoming `elem`/`src_elem`), ownership transfers to
                // the carried value — releasing the incoming here would be a use-after-free (the
                // hazard the scalar-only gate originally side-stepped). Reclaim the incoming only when
                // the map produced a genuinely-fresh result that does NOT alias it.
                let aliases_incoming = mapped == elem || mapped == src_elem;
                if !aliases_incoming {
                    if elem != src_elem {
                        builder.emit(Instruction::Release { val: elem, ty: elem_ty.clone() });
                    } else {
                        // The map consumed the SOURCE materialize → reclaim it here, exactly once,
                        // and mark it dead so no downstream stage frees it again.
                        free_combinator_elem_box_full(src_elem, &src_elem_ty, builder);
                        free_combinator_sealed_elem(src_elem, iterable_ty, &src_elem_ty, builder);
                        src_alive = false;
                    }
                }
                elem = mapped;
                elem_ty = if matches!(mapped_ty, Type::TypeVar(_)) { out_elem_ty.clone() } else { mapped_ty };
            }
            // A flatMap-bearing chain is routed to the CPS engine (`emit_flatmap_fused_loop`) before
            // ever reaching the linear applier — see `chain_has_flatmap`. Unreachable here.
            FuseStage::FlatMap { .. } => unreachable!("flatMap chains use the CPS fusion engine"),
        }
    }
    Some((elem, elem_ty))
}

/// Drive a fused chain over a base SOURCE for a side-effecting terminal (`for`): an index loop whose
/// body reads the element, applies the stages, then runs `terminal` on the survivor. A filter skip
/// jumps to a per-iteration `fuse_cont` latch; the terminal path converges there. Reuses
/// `emit_index_loop` (back-edge patched to the CURRENT block = fuse_cont, the true latch).
pub(crate) fn emit_fused_loop<T>(
    iterable: Temp,
    iterable_ty: &Type,
    read_elem_ty: &Type,
    stages: &[FuseStage],
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    mut terminal: T,
) where
    T: FnMut(Temp, Type, Temp, Temp, &Type, &mut FuncBuilder, &mut LowerCtx),
{
    let read_elem_ty = read_elem_ty.clone();
    emit_index_loop(iterable, iterable_ty, &read_elem_ty, builder, ctx, |i, elem, b, c| {
        let cont = b.alloc_block("fuse_cont");
        let idx = narrow_loop_index(i, b);
        let src_elem = elem;
        let src_elem_ty = read_elem_ty.clone();
        if let Some((val, val_ty)) = apply_fuse_stages(stages, elem, read_elem_ty.clone(), iterable_ty, idx, cont, b, c) {
            terminal(val, val_ty, idx, src_elem, &src_elem_ty, b, c);
        }
        if !b.is_current_block_terminated() {
            b.terminate(Terminator::Jump(cont));
        }
        b.switch_to(cont);
    });
}

// ===========================================================================================
// FLATMAP-FUSION (Wave D) — a `flatMap` stage in a fused combinator chain. PUSH-model fusion makes
// flatMap a LOOP NEST, not a fusion barrier: the source is driven by an outer index loop; at a
// FlatMap stage we evaluate `inner = f(elem, idx)` (a fresh `U[]`) and run ALL downstream stages +
// the terminal once per inner element in an INNER index loop. No intermediate per-stage array is
// built (beyond the flatMap's own `inner`, which the language semantics already materialize).
//
// A chain that contains a FlatMap stage lowers via the recursive CPS engine `fm_process` below
// (single-ownership reclaim at each stage's consuming site); a flatMap-FREE chain keeps the original
// linear `apply_fuse_stages`/`emit_fused_loop` lowering BYTE-IDENTICALLY (Wave D adds a stage; it does
// not touch the others). The terminal is supplied as a `&mut dyn FnMut` callback so the SAME engine
// drives `for`/`map`/`filter`/`reduce` terminals.
//
// OWNERSHIP (the RC crux — this fuser has a leak history): `fm_process` CONSUMES the element it is
// handed. On the DROP path (a filter predicate is false) it reclaims the element; on the MAP path it
// reclaims the (now-superseded) input element and threads the fresh map output downstream; on the
// PASS-THROUGH path the element flows downstream and the eventual terminal reclaims it. Reclaim is the
// SAME discipline the single-combinator/linear-fused paths trust — `free_combinator_elem_box_full`
// (union/Json shell+inner) + `free_combinator_sealed_elem` (sealed struct) — a NO-OP for an inline
// scalar. The flatMap's `inner` array is a fresh owned `U[]` released after its inner loop completes.
// Each inner element is a fresh `Index` materialize gated to a reclaimable repr (scalar/sealed/union),
// reclaimed identically to a source element. No alias-tracking is needed because a map stage's output
// is always a fresh SCALAR (gated in `extract_fuse_chain`), so the reclaim of the threaded element at
// its single consuming site never targets a stale/aliased reference (the `map.filter` double-free the
// linear path guards with `src_alive` cannot arise — the reclaim follows the live value, not `src`).

/// The position-indexed output counter for the consumer at pipeline `pos`. Downstream of a flatMap or
/// a filter, the index a stage's lambda receives is the OUTPUT-stream position at that stage (the
/// number of elements delivered to it so far), NOT the source index — matching the eager
/// `arr.flatMap(f).map((y,i)=>…)` semantics where `map` sees the flattened array's positions. Each
/// such counter is a loop-invariant `Int32` heap cell (allocated ONCE before the outer loop, init 0),
/// incremented per element ARRIVING at that position. `None` when that position's lambda is 1-arg (no
/// index used) — then no cell is allocated and no per-element increment is emitted.
pub(crate) fn fm_next_index(counters: &[Option<Temp>], pos: usize, builder: &mut FuncBuilder) -> Temp {
    match counters.get(pos).and_then(|c| *c) {
        Some(cell) => {
            let cur = builder.alloc_temp(Type::Int32);
            builder.emit(Instruction::CellGet { dst: cur, cell, ty: Type::Int32 });
            let one = builder.const_temp(Const::Int(1, Type::Int32));
            let nxt = builder.alloc_temp(Type::Int32);
            builder.emit(Instruction::Binary {
                dst: nxt, op: BinOp::Add, lhs: cur, rhs: one, operand_ty: Type::Int32, ty: Type::Int32,
            });
            builder.emit(Instruction::CellSet { cell, value: nxt, ty: Type::Int32 });
            cur
        }
        // 1-arg consumer at this position — the index is never read; a constant is harmless.
        None => builder.const_temp(Const::Int(0, Type::Int32)),
    }
}

/// Fully reclaim a per-iteration combinator element materialize (the SAME discipline the single-
/// combinator + linear-fused paths use): a union/Json box (shell + retained inner) and/or a sealed
/// struct. A no-op for an inline scalar. Used at every CONSUMING site in `fm_process`.
pub(crate) fn fm_reclaim_elem(elem: Temp, elem_ty: &Type, builder: &mut FuncBuilder) {
    free_combinator_elem_box_full(elem, elem_ty, builder);
    free_combinator_sealed_elem(elem, &Type::Null, elem_ty, builder);
}

/// Recursive CPS engine for a flatMap-bearing fused chain. Processes `stages[0]` over the carried
/// `elem` (with output index `idx`), then recurses on `stages[1..]`; an empty `stages` runs the
/// terminal. CONSUMES `elem` (see the module note). `pos` is the pipeline position of `stages[0]`
/// (0 = source); `counters[p]` feeds position `p`'s output index. `skip_block` is the latch of the
/// INNERMOST loop currently in scope — a filter drop jumps there (skipping to the next element of
/// whatever loop this stage runs in: the source loop for a pre-flatMap stage, an inner loop for a
/// post-flatMap stage).
#[allow(clippy::too_many_arguments)]
pub(crate) fn fm_process(
    stages: &[FuseStage],
    elem: Temp,
    elem_ty: Type,
    idx: Temp,
    counters: &[Option<Temp>],
    pos: usize,
    skip_block: BlockId,
    terminal: &mut dyn FnMut(Temp, Type, Temp, &mut FuncBuilder, &mut LowerCtx),
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) {
    let Some(stage) = stages.first() else {
        // Terminal: the terminal callback consumes `elem` (push/fold/side-effect + its own reclaim).
        terminal(elem, elem_ty, idx, builder, ctx);
        return;
    };
    match stage {
        FuseStage::Filter { params, body } => {
            let (pred_raw, pred_ty) =
                inline_lambda_body(params, body, &[(elem, elem_ty.clone()), (idx, Type::Int32)], builder, ctx);
            let keep = if matches!(pred_ty, Type::Bool) {
                pred_raw
            } else {
                let d = builder.alloc_temp(Type::Bool);
                builder.emit(Instruction::Coerce { dst: d, src: pred_raw, from_ty: pred_ty, to_ty: Type::Bool });
                d
            };
            let keep_block = builder.alloc_block("fm_keep");
            let drop_block = builder.alloc_block("fm_drop");
            builder.terminate(Terminator::CondJump { cond: keep, then_block: keep_block, else_block: drop_block });
            // DROP: reclaim the element, jump to this loop's latch.
            builder.switch_to(drop_block);
            fm_reclaim_elem(elem, &elem_ty, builder);
            builder.terminate(Terminator::Jump(skip_block));
            // KEEP: pass the (unchanged, still-owned) element downstream at the next position.
            builder.switch_to(keep_block);
            let next_idx = fm_next_index(counters, pos + 1, builder);
            fm_process(&stages[1..], elem, elem_ty, next_idx, counters, pos + 1, skip_block, terminal, builder, ctx);
        }
        FuseStage::Map { params, body, out_elem_ty } => {
            let (mapped, mapped_ty) =
                inline_lambda_body(params, body, &[(elem, elem_ty.clone()), (idx, Type::Int32)], builder, ctx);
            // The map produced a fresh SCALAR output (gated in `extract_fuse_chain`); the input
            // `elem` is superseded — reclaim it now (a no-op for a scalar input, a real free for a
            // sealed/union materialize). `mapped != elem` always holds for a scalar output over a
            // distinct input, but the guard keeps a scalar identity (`x => x`, scalar-gated) safe.
            if mapped != elem {
                fm_reclaim_elem(elem, &elem_ty, builder);
            }
            let next_ty = if matches!(mapped_ty, Type::TypeVar(_)) { out_elem_ty.clone() } else { mapped_ty };
            let next_idx = fm_next_index(counters, pos + 1, builder);
            fm_process(&stages[1..], mapped, next_ty, next_idx, counters, pos + 1, skip_block, terminal, builder, ctx);
        }
        FuseStage::FlatMap { params, body, inner_elem_ty } => {
            // Evaluate `inner = f(elem, idx)` — a fresh, fully-owned `U[]`.
            let (inner, inner_arr_ty) =
                inline_lambda_body(params, body, &[(elem, elem_ty.clone()), (idx, Type::Int32)], builder, ctx);
            // flatMap consumes its input element (it produced `inner` from it). `inner` is a freshly
            // allocated array, never an alias of `elem` (different value/type), but guard anyway.
            if inner != elem {
                fm_reclaim_elem(elem, &elem_ty, builder);
            }
            // Read the inner element at the inner array's PROVABLE repr (a `[x, x*2]` literal is a flat
            // scalar buffer; anything else reads tagged — sound for both). Reclaim uses that same type.
            let inner_read_ty = combinator_read_elem_ty(body, builder, ctx);
            let _ = inner_elem_ty; // the gate type; `inner_read_ty` is the read/reclaim repr.
            let rest = &stages[1..];
            let inner_pos = pos + 1;
            // INNER LOOP over `inner`: run the downstream stages + terminal once per inner element.
            emit_index_loop(inner, &inner_arr_ty, &inner_read_ty, builder, ctx, |_j, ielem, b, c| {
                // A per-inner-element continuation: a downstream filter drop jumps HERE (the inner
                // loop's latch), not the source loop's latch. Mirrors `emit_fused_loop`'s `cont`.
                let cont = b.alloc_block("fm_inner_cont");
                let inner_idx = fm_next_index(counters, inner_pos, b);
                fm_process(rest, ielem, inner_read_ty.clone(), inner_idx, counters, inner_pos, cont, terminal, b, c);
                if !b.is_current_block_terminated() {
                    b.terminate(Terminator::Jump(cont));
                }
                b.switch_to(cont);
            });
            // The inner array is fully consumed — release the fresh +1 owned `U[]`.
            builder.emit(Instruction::Release { val: inner, ty: inner_arr_ty });
        }
    }
}

/// Allocate the per-position output-index counter cells for a flatMap chain. A cell is allocated for
/// position `p` (1..=stages.len(), the terminal at `stages.len()`) ONLY when that position's consumer
/// lambda declares ≥2 params (so it actually reads an index) — otherwise `None` (no cell, no
/// per-element increment). Position 0 (the source-driven stage) always uses the raw source index.
/// Returns a vec indexed by position (length `stages.len()+1`); index 0 is always `None`.
pub(crate) fn fm_alloc_counters(
    stages: &[FuseStage],
    terminal_param_count: usize,
    builder: &mut FuncBuilder,
) -> Vec<Option<Temp>> {
    let n = stages.len();
    let mut counters: Vec<Option<Temp>> = Vec::with_capacity(n + 1);
    counters.push(None); // position 0: source index (no counter cell)
    for p in 1..=n {
        // The consumer at position `p` is `stages[p]` (a fuse stage) or, at `p == n`, the terminal.
        let param_count = if p < n {
            match &stages[p] {
                FuseStage::Map { params, .. }
                | FuseStage::Filter { params, .. }
                | FuseStage::FlatMap { params, .. } => params.len(),
            }
        } else {
            terminal_param_count
        };
        if param_count >= 2 {
            let zero = builder.const_temp(Const::Int(0, Type::Int32));
            let cell = builder.alloc_temp(Type::TypeVar(u32::MAX));
            builder.emit(Instruction::MakeCell { dst: cell, init: zero, ty: Type::Int32 });
            counters.push(Some(cell));
        } else {
            counters.push(None);
        }
    }
    counters
}

/// Free the counter cells allocated by `fm_alloc_counters` (plain `Int32` cells — `FreeCell` just
/// reclaims the allocation, no value release). Called after the outer loop completes.
pub(crate) fn fm_free_counters(counters: &[Option<Temp>], builder: &mut FuncBuilder) {
    for cell in counters.iter().flatten() {
        builder.emit(Instruction::FreeCell { cell: *cell, ty: Type::Int32 });
    }
}

/// Drive a flatMap-bearing fused chain over `base` (the source) with a terminal callback. Allocates
/// the output-index counters, runs the source index loop, and recurses through `fm_process` per source
/// element. `terminal(survivor, ty, out_idx)` is invoked at the leaf of the pipeline for each
/// surviving element (and CONSUMES it — see the module note).
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_flatmap_fused_loop<T>(
    iterable: Temp,
    iterable_ty: &Type,
    read_elem_ty: &Type,
    stages: &[FuseStage],
    terminal_param_count: usize,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
    mut terminal: T,
) where
    T: FnMut(Temp, Type, Temp, &mut FuncBuilder, &mut LowerCtx),
{
    let counters = fm_alloc_counters(stages, terminal_param_count, builder);
    let read_elem_ty = read_elem_ty.clone();
    {
        let counters = &counters;
        let stages = &stages;
        let term: &mut dyn FnMut(Temp, Type, Temp, &mut FuncBuilder, &mut LowerCtx) = &mut terminal;
        emit_index_loop(iterable, iterable_ty, &read_elem_ty, builder, ctx, |i, elem, b, c| {
            let cont = b.alloc_block("fm_src_cont");
            let idx = narrow_loop_index(i, b);
            fm_process(stages, elem, read_elem_ty.clone(), idx, counters, 0, cont, term, b, c);
            if !b.is_current_block_terminated() {
                b.terminate(Terminator::Jump(cont));
            }
            b.switch_to(cont);
        });
    }
    fm_free_counters(&counters, builder);
}

/// Path-8-B devirtualization: when a combinator callback is a BARE reference to a statically-known
/// function (a top-level `val f = (…) => …` or an imported export), resolve the DIRECT call target
/// (`CallTarget::Direct(FuncId)` for a local fn, `CallTarget::Named(sym)` for an import) so the
/// per-element call can be emitted as a direct/named call to the function's NATIVE signature —
/// skipping the heap closure shell + boxed-ABI wrapper + indirect dispatch the closure path emits.
///
/// Returns `(target, param_tys)` where `param_tys` are the callee's DECLARED parameter types (its
/// native signature), used to coerce the loop's element/index args to the native representation.
/// Returns `None` for any non-bare callback (a literal lambda or a stored/passed `Function` value).
fn bare_fn_call_target(
    expr: &TypedExpr,
    builder: &FuncBuilder,
    ctx: &LowerCtx,
) -> Option<(CallTarget, Vec<Type>)> {
    let TypedExpr::LocalGet { slot, .. } = expr else { return None };
    if builder.intrinsic_slots.contains_key(slot) {
        return None;
    }
    let (params, _) = callback_signature(expr);
    if let Some(&fid) = ctx.global_fn_slots.get(slot) {
        return Some((CallTarget::Direct(fid), params));
    }
    if let Some((sym, param_tys)) = ctx.import_fn_slots.get(slot) {
        let ptys = if param_tys.is_empty() { params } else { param_tys.clone() };
        return Some((CallTarget::Named(sym.clone()), ptys));
    }
    None
}

/// Devirtualized per-element call to a statically-known function (`bare_fn_call_target`): coerce
/// each supplied arg to the callee's native param representation and emit a DIRECT/NAMED `Call` —
/// no closure alloc, no boxed-ABI indirect dispatch. Truncates surplus args beyond param_tys.len().
fn call_body_direct(
    target: CallTarget,
    raw_args: &[(Temp, Type)],
    param_tys: &[Type],
    ret_ty: &Type,
    builder: &mut FuncBuilder,
) -> Temp {
    let n = param_tys.len();
    let mut arg_shell_boxes: Vec<Temp> = Vec::new();
    let call_args: Vec<Temp> = raw_args
        .iter()
        .take(n)
        .enumerate()
        .map(|(i, (t, ty))| {
            let arg = lower_coerce_arg(*t, ty, param_tys.get(i), builder);
            let boxed_scalar = matches!(param_tys.get(i), Some(p) if is_union_ty(p))
                && !is_union_ty(ty)
                && !is_rc_type(ty);
            if boxed_scalar {
                arg_shell_boxes.push(arg);
            }
            arg
        })
        .collect();
    let dst = builder.alloc_temp(ret_ty.clone());
    builder.emit(Instruction::Call {
        dst,
        callee: target,
        args: call_args,
        ret_ty: ret_ty.clone(),
    });
    for shell in &arg_shell_boxes {
        builder.emit(Instruction::FreeBoxShellIfDistinct { val: *shell, other: dst });
    }
    dst
}

/// `for(iterable, body)` → index loop calling `body(elem)` for side effects; returns Null.
pub(crate) fn lower_for(args: &[TypedExpr], builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
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
    // FUSED `range(a, b).for(f)`: when the receiver is a direct `range` call, drive a native i32
    // counter `i` in `[a, b)` and call the callback with `i` as the element — skipping the
    // materialized range array entirely (no `lin_range` alloc, no N pushes, no N `Index` reads,
    // and no iterator-handle leak). `range` is always eager + ordered, so this is observably
    // identical to iterating the array. The callback ABI / box-release sequence is UNCHANGED from
    // the generic path below (same boxed element, same return-box release, same shell reclaim), so
    // captured-`var` mutation and any callback return value behave exactly as before.
    if let Some((start_e, end_e, step_e)) = range_for_bounds(&args[0], builder, ctx) {
        return lower_range_for(start_e, end_e, step_e, &args[1], &param_tys, builder, ctx);
    }
    // FUSED CHAIN (path-6 6a): base.map/filter chain into the `for` loop (no intermediate array).
    // Requires an inlinable side-effecting body lambda and at least one fusible stage; bails otherwise.
    if let Some((lam_params, lam_body)) = inlinable_local_fn(&args[1], builder, ctx) {
        let (base, stages) = extract_fuse_chain(&args[0], builder, ctx);
        if !stages.is_empty() {
            let lam_params = lam_params.to_vec();
            let lam_body = lam_body.clone();
            let base_ty = base.ty();
            let read_elem_ty = combinator_read_elem_ty(base, builder, ctx);
            let iterable = lower_expr(base, builder, ctx);
            // WAVE D: a flatMap-bearing chain lowers via the CPS engine (loop nest); the `for` body is
            // the terminal, run once per surviving (flattened) element and discarding its result.
            if chain_has_flatmap(&stages) {
                emit_flatmap_fused_loop(iterable, &base_ty, &read_elem_ty, &stages, lam_params.len(),
                    builder, ctx, |val, val_ty, idx, b, c| {
                        let (res, res_ty) =
                            inline_lambda_body(&lam_params, &lam_body, &[(val, val_ty.clone()), (idx, Type::Int32)], b, c);
                        b.emit(Instruction::Release { val: res, ty: res_ty });
                        // The `for` body never moves `val` into a result — fully reclaim the survivor.
                        fm_reclaim_elem(val, &val_ty, b);
                    });
                return builder.const_temp(Const::Null);
            }
            emit_fused_loop(iterable, &base_ty, &read_elem_ty, &stages, builder, ctx,
                |val, val_ty, idx, src_elem, src_elem_ty, b, c| {
                    let (res, res_ty) =
                        inline_lambda_body(&lam_params, &lam_body, &[(val, val_ty.clone()), (idx, Type::Int32)], b, c);
                    b.emit(Instruction::Release { val: res, ty: res_ty });
                    if val == src_elem {
                        free_combinator_elem_box_full(src_elem, src_elem_ty, b);
                        free_combinator_sealed_elem(src_elem, &base_ty, src_elem_ty, b);
                    } else {
                        b.emit(Instruction::Release { val, ty: val_ty });
                    }
                });
            return builder.const_temp(Const::Null);
        }
    }
    // Read elements at the source's PROVABLE runtime representation: flat-scalar only when the
    // source is a provably-flat producer, else the tagged Json read (sound for a `[]`+push array
    // mistyped as flat). See `combinator_read_elem_ty` (ADR-044).
    let read_elem_ty = combinator_read_elem_ty(&args[0], builder, ctx);
    let iterable = lower_expr(&args[0], builder, ctx);

    // DEVIRTUALIZED FAST PATH (path-8-B): a BARE statically-known fn body (`xs.for(printIt)`) is
    // called DIRECTLY per element — no closure shell, no boxed-ABI wrapper, no per-element index
    // box, no indirect dispatch. `call_body_direct` coerces the element arg to the callee's native
    // param repr (the index arg too, when the callee declares it).
    if let Some((target, native_params)) = bare_fn_call_target(&args[1], builder, ctx) {
        let native_ret = match args[1].ty() {
            Type::Function { ret, .. } => *ret,
            _ => Type::Null,
        };
        let elem_ty = read_elem_ty.clone();
        emit_index_loop(iterable, &iterable_ty, &read_elem_ty, builder, ctx, |i, elem, b, _| {
            let idx = narrow_loop_index(i, b);
            let res = call_body_direct(
                target.clone(), &[(elem, elem_ty.clone()), (idx, Type::Int32)], &native_params, &native_ret, b);
            b.emit(Instruction::Release { val: res, ty: native_ret.clone() });
            free_combinator_elem_box_full(elem, &elem_ty, b);
            free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, b);
        });
        return builder.const_temp(Const::Null);
    }

    // INLINE FAST PATH (capturing-closure inline): a literal side-effecting lambda — capturing OR not —
    // is spliced into the loop body, its param bound to the element, with no closure alloc and no
    // per-element box ABI / indirect call. Captured slots resolve through the enclosing builder's
    // bindings (ADR-012 cell/global semantics preserved); the back-edge is patched latch-relative by
    // `emit_index_loop` even when the inlined body emits its own blocks (inner combinator / match / if).
    if let Some((lam_params, lam_body)) = inlinable_local_fn(&args[1], builder, ctx) {
        let lam_params = lam_params.to_vec();
        let lam_body = lam_body.clone();
        let elem_ty = read_elem_ty.clone();
        // PATH-1 in-place packed iteration: a packed sealed-scalar array source iterates over the
        // contiguous buffer with NO per-element materialize — the element param is bound to a
        // borrowed `(array, index)` view, and its `p["field"]` reads lower to const-offset loads
        // (`try_lower_packed_elem_field`). Gated to bodies that use the element ONLY for scalar
        // field reads (`elem_used_only_for_scalar_fields`): a whole-value use (passing `p` to a
        // call, storing it, comparing it) falls through to the generic materialize path below —
        // identical to today's boxed behaviour, so no correctness regression.
        if is_sealed_scalar_array(&iterable_ty)
            && lam_params.first().map(|p| elem_used_only_for_scalar_fields(p.slot, &lam_body)).unwrap_or(false)
        {
            let static_elem = iter_elem_type(&iterable_ty);
            emit_packed_index_loop(iterable, &iterable_ty, builder, ctx, |i, array, b, c| {
                let idx = narrow_loop_index(i, b);
                let (res, res_ty) = inline_lambda_body_packed_view(
                    &lam_params, &lam_body, array, i, &static_elem, idx, b, c);
                b.emit(Instruction::Release { val: res, ty: res_ty });
            });
            return builder.const_temp(Const::Null);
        }
        emit_index_loop(iterable, &iterable_ty, &read_elem_ty, builder, ctx, |i, elem, b, c| {
            // Optional 0-based SOURCE index; narrowed Int64→Int32. `inline_lambda_body` binds by the
            // lambda's OWN param count, so a 1-param `x => …` simply ignores this surplus arg.
            let idx = narrow_loop_index(i, b);
            let (res, res_ty) =
                inline_lambda_body(&lam_params, &lam_body, &[(elem, elem_ty.clone()), (idx, Type::Int32)], b, c);
            // `for` discards the body result; release it if it owns a heap value (no-op for a scalar).
            b.emit(Instruction::Release { val: res, ty: res_ty });
            // ELEMENT-BOX RC: a union/Json source read (`lin_array_get_tagged`) allocated a fresh +1
            // element box (shell + a retained inner). A side-effecting `for` body NEVER moves the
            // element into a result (no `push`/`set` of `elem` itself — those are `map`/`filter`/the
            // body's own owned values), so the element box is genuinely dropped: FULLY release it
            // (shell + inner) exactly as the filter-SKIP path does, else every element's inner leaks.
            // No-op for a flat-scalar read (no box was allocated).
            free_combinator_elem_box_full(elem, &elem_ty, b);
            // A PACKED sealed-array source materialized a fresh +1 element struct; the body read a copy
            // out of it (side effect), so release it or it leaks per iteration.
            free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, b);
        });
        return builder.const_temp(Const::Null);
    }

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
        // The optional 0-based SOURCE index (`(item, i) => …`); narrowed Int64→Int32 and, WHEN the
        // callback actually declares it, BOXED in IR. The boxing matters for RC: the uniform closure
        // ABI takes every arg as a `TaggedVal*`, so codegen would otherwise box this `Int32` itself — a
        // fresh per-iteration box invisible to the IR and therefore never freed (the per-iteration
        // index-box leak, ASan-confirmed ~16 B/iter independent of source representation). Boxing here
        // makes it a tracked union temp that joins `elem_boxes` below and is reclaimed each iteration;
        // codegen sees an already-boxed ptr and passes it through (no double-box). When the callback
        // declares only `(item)` the index arg is truncated away by `call_body_closure_with_elem_boxes`,
        // so we leave it a raw `Int32` (no orphan box to leak) — it is never passed.
        let idx_raw = narrow_loop_index(i, b);
        let (idx, idx_ty) = if param_tys.len() >= 2 {
            (box_to_json(idx_raw, &Type::Int32, b), Type::TypeVar(u32::MAX))
        } else {
            (idx_raw, Type::Int32)
        };
        let (ret, elem_boxes) = call_body_closure_with_elem_boxes(body, &[(elem, elem_ty.clone()), (idx, idx_ty)], &param_tys, &boxed, b);
        // Release the callback-RETURN box (a fresh, independently-owned +1; `for` discards it).
        // This fully reclaims it (inner + shell). The callback CAN return (an alias of) the
        // element box — e.g. `x => x`, or `acc = f(acc, x)` where `f` yields its element — in which
        // case `ret` IS the element box and this single release already reclaimed it.
        b.emit(Instruction::Release { val: ret, ty: boxed.clone() });
        // FULLY reclaim the per-iteration element box (inner heap payload + shell) — but ONLY when it
        // is DISTINCT from `ret` (the release above already reclaimed it otherwise; a second release
        // would double-free). `lin_array_get_tagged` returns the element box as a fresh +1 WITH its
        // inner heap payload RETAINED; a side-effecting `for` body never MOVES that inner anywhere, so
        // it must be fully released, else every heap-bearing element's inner leaks (the String-packed
        // sealed `for` leak AND the pre-existing genuine `Json[]`-of-objects `for` leak). For a
        // flat-scalar element box there is no inner, so this degrades to a shell free (the old ~36 B/iter
        // reclaim). Cached-box and non-pointer safe. for/while-only reclaim; map/filter/reduce use the
        // plain `call_body_closure` (move-into-result) and never reach this path.
        for ebox in &elem_boxes {
            b.emit(Instruction::ReleaseIfDistinct { val: *ebox, other: ret });
        }
        // A PACKED sealed-array source materialized a fresh +1 element struct each iteration; `for`
        // discards it (a side-effecting body never moves the struct out), so release it or it leaks.
        free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, b);
    });
    builder.const_temp(Const::Null)
}

/// Fused lowering for `range(start, end).for(body)`: an i32 counted loop
/// `for (i = start; i < end; i++) body(i)` that calls the callback directly with the counter as the
/// element — no materialized range array, no per-element `Index`/`lin_array_get_tagged`. The element
/// is an unboxed `Int32` (the counter); `call_body_closure_with_elem_boxes` boxes it for the callback
/// ABI exactly as the generic `for` does over a flat-i32 source, and the per-iteration release /
/// shell-reclaim sequence is byte-for-byte the generic-path logic — so RC behaviour (captured-`var`
/// mutation, callback-return discard) is identical.
pub(crate) fn lower_range_for(
    start_e: &TypedExpr,
    end_e: &TypedExpr,
    step_e: Option<&TypedExpr>,
    callback: &TypedExpr,
    param_tys: &[Type],
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Temp {
    // Bounds (and optional step) drive a native i32 counter; coerce to Int32 (mirrors `lower_range`).
    let start_raw = lower_expr(start_e, builder, ctx);
    let end_raw = lower_expr(end_e, builder, ctx);
    let start = coerce_to_slot_type(start_raw, &start_e.ty(), &Type::Int32, builder);
    let end = coerce_to_slot_type(end_raw, &end_e.ty(), &Type::Int32, builder);
    let step = step_e.map(|se| {
        let step_raw = lower_expr(se, builder, ctx);
        coerce_to_slot_type(step_raw, &se.ty(), &Type::Int32, builder)
    });

    let elem_ty = Type::Int32;
    let boxed = Type::TypeVar(u32::MAX);

    // INLINE FAST PATH (capturing-closure inline): a literal lambda callback — capturing OR not — is
    // spliced into the loop body. Its element param binds to the UNBOXED i32 counter; captured slots
    // resolve through the enclosing builder's slots/cell_slots/global_var_slots (the SAME bindings the
    // closure would have captured), so a captured `var` mutation hits the same shared global/cell —
    // ADR-012 intact. No closure alloc, no per-element box, no indirect call, no return-box release.
    let inline_lam = inlinable_local_fn(callback, builder, ctx);
    // Lower the (boxed) callback closure ONLY when we are NOT inlining — otherwise the closure value
    // is unused.
    let body = if inline_lam.is_none() {
        Some(lower_callback_in_safe_ctx(callback, builder, ctx))
    } else {
        None
    };

    // CK.1a: mark the range IV as non-negative when the start is a provably non-negative
    // constant literal. We check `start_raw` (the pre-coerce temp) because `coerce_to_slot_type`
    // returns it unchanged for the common Int32→Int32 case, and check `start` too (the coerced
    // result) in case they differ. Only the inline path (literal lambda) uses this: the indirect
    // call path boxes i through the generic closure ABI anyway.
    let start_is_nonneg = builder.temp_is_nonneg_int_const(start_raw)
        || builder.temp_is_nonneg_int_const(start);

    let preheader = builder.current_block;
    let header = builder.alloc_block("range_for_header");
    let body_block = builder.alloc_block("range_for_body");
    let exit = builder.alloc_block("range_for_exit");

    let i = builder.alloc_temp(Type::Int32);
    let i_next = builder.alloc_temp(Type::Int32);
    builder.terminate(Terminator::Jump(header));

    builder.switch_to(header);
    builder.emit(Instruction::Phi {
        dst: i,
        ty: Type::Int32,
        incomings: vec![(start, preheader), (i_next, body_block)],
    });
    let cond = builder.alloc_temp(Type::Bool);
    match step {
        // 2-arg range (step=1 implicit): i < end.
        None => {
            builder.emit(Instruction::Binary {
                dst: cond, op: BinOp::Lt, lhs: i, rhs: end,
                operand_ty: Type::Int32, ty: Type::Bool,
            });
        }
        // 3-arg range with general step: continue while (end - i) and step have the same sign,
        // i.e. (end - i) * step > 0. This is equivalent to the stdlib's direction-switch logic
        // (step > 0 → i < end, step < 0 → i > end, step == 0 → never), is a single comparison
        // in the loop header, and LLVM simplifies it to `i < end` for a constant step=1.
        Some(step_val) => {
            let diff = builder.alloc_temp(Type::Int32);
            builder.emit(Instruction::Binary {
                dst: diff, op: BinOp::Sub, lhs: end, rhs: i,
                operand_ty: Type::Int32, ty: Type::Int32,
            });
            let prod = builder.alloc_temp(Type::Int32);
            builder.emit(Instruction::Binary {
                dst: prod, op: BinOp::Mul, lhs: diff, rhs: step_val,
                operand_ty: Type::Int32, ty: Type::Int32,
            });
            let zero = builder.const_temp(Const::Int(0, Type::Int32));
            builder.emit(Instruction::Binary {
                dst: cond, op: BinOp::Gt, lhs: prod, rhs: zero,
                operand_ty: Type::Int32, ty: Type::Bool,
            });
        }
    }
    builder.terminate(Terminator::CondJump { cond, then_block: body_block, else_block: exit });

    builder.switch_to(body_block);
    if let Some((lam_params, lam_body)) = inline_lam {
        let lam_params = lam_params.to_vec();
        let lam_body = lam_body.clone();
        // CK.1a: register `i` as a non-negative range IV so `lower_expr` sets `nonneg: true`
        // on any `Index { key: i, ... }` inside the inlined body. This enables the
        // flat-array read path to skip the negative-wrap select, emitting the canonical
        // `0 <= i < len` IRCE-eligible bounds check. The signal is scoped to this inline
        // body: we remove it after the body is lowered.
        if start_is_nonneg {
            builder.nonneg_range_ivs.insert(i);
        }
        // Bind the element param (and optional index param) to the unboxed i32 counter, lower the body
        // inline, then discard its (owned) result. Captured slots are NOT rebound — they resolve to the
        // enclosing function's live bindings. `inline_lambda_body` binds by the lambda's OWN param count
        // (a 1-param `i => …` ignores the surplus index arg).
        let (res, res_ty, elem_boxes) = inline_lambda_body_tracking_elem_boxes(
            &lam_params, &lam_body, &[(i, elem_ty.clone()), (i, Type::Int32)], builder, ctx,
        );
        // Clean up: remove `i` from nonneg set after body inlining (its scope ends here).
        builder.nonneg_range_ivs.remove(&i);
        // `for` discards the body result; release it if it's an owned heap value (no-op for a scalar).
        builder.emit(Instruction::Release { val: res, ty: res_ty });
        // Objective C: reclaim each per-iteration scalar→union element box SHELL (distinct from the
        // result, which we just released). `FreeBoxShellIfDistinct` is shell-only + cached-box-safe.
        for ebox in &elem_boxes {
            builder.emit(Instruction::FreeBoxShellIfDistinct { val: *ebox, other: res });
        }
    } else {
        let body = body.expect("non-inline range-for lowers the callback closure");
        // The callback receives `i` (Int32) as BOTH the element and the optional 0-based source index
        // (for a range, element == index). Box BOTH in IR up front: the uniform closure ABI takes every
        // arg as a `TaggedVal*`, so codegen would otherwise box each raw `Int32` itself — fresh
        // per-iteration boxes invisible to the IR and therefore never freed (the index-box leak; see
        // the generic `for` path). Boxing here makes them tracked union temps that join `elem_boxes`
        // and are reclaimed each iteration; codegen passes the already-boxed ptr through (no double-box).
        // The index is only boxed/passed when the callback declares it (else it is truncated away — no
        // orphan box to leak); the element box is always passed.
        let json = Type::TypeVar(u32::MAX);
        let elem_box = box_to_json(i, &Type::Int32, builder);
        let mut raw_args = vec![(elem_box, json.clone())];
        if param_tys.len() >= 2 {
            raw_args.push((box_to_json(i, &Type::Int32, builder), json.clone()));
        }
        let (ret, elem_boxes) = call_body_closure_with_elem_boxes(
            body, &raw_args, param_tys, &boxed, builder,
        );
        // Release the callback-RETURN box, then reclaim each element box SHELL if distinct — identical
        // to the generic `for` path (see `lower_for` for the full rationale).
        builder.emit(Instruction::Release { val: ret, ty: boxed.clone() });
        for ebox in &elem_boxes {
            builder.emit(Instruction::FreeBoxShellIfDistinct { val: *ebox, other: ret });
        }
    }
    // The inlined body may have switched basic blocks (an inner combinator / match / multi-branch if):
    // the increment + back-edge must originate from the CURRENT block (the true loop latch), and the
    // header phi's back-edge predecessor patched to it so SSA dominance holds (the spike's hang bug).
    let latch = builder.current_block;
    let increment = match step {
        None => builder.const_temp(Const::Int(1, Type::Int32)),
        Some(step_val) => step_val,
    };
    builder.emit(Instruction::Binary {
        dst: i_next, op: BinOp::Add, lhs: i, rhs: increment,
        operand_ty: Type::Int32, ty: Type::Int32,
    });
    builder.terminate(Terminator::Jump(header));
    builder.patch_phi_incoming(header, i, body_block, latch);

    builder.switch_to(exit);
    builder.const_temp(Const::Null)
}

/// `while(iterable, body)` → like `for`, but stops early when `body(elem)` returns false.
pub(crate) fn lower_while(args: &[TypedExpr], builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    let iterable_ty = args[0].ty();
    let (param_tys, _) = callback_signature(&args[1]);
    // Read at the source's PROVABLE representation (ADR-044): tagged Json read unless provably flat,
    // so a `[]`+push array mistyped as a flat `T[]` is read correctly (not as raw flat scalars).
    let read_elem_ty = combinator_read_elem_ty(&args[0], builder, ctx);
    let iterable = lower_expr(&args[0], builder, ctx);

    // INLINE FAST PATH (mirrors `lower_for`): a literal predicate lambda — capturing OR not — is
    // spliced into the loop body, its param bound to the element, with no closure alloc and no
    // per-element box ABI / indirect call. The body's `Boolean` result drives the keep/stop split
    // directly: `true` continues, `false` (or exhaustion) exits. Captured slots resolve through the
    // enclosing builder's bindings (ADR-012). Unlike `for`/`map`/`filter`, `while` must EXIT early on
    // a false predicate, not just skip — so it drives `emit_combinator_loop` with `LoopFlow::ContinueIf`
    // (the helper wires the body's `keep` Bool into a `CondJump → latch / exit`).
    if let Some((lam_params, lam_body)) = inlinable_local_fn(&args[1], builder, ctx) {
        let lam_params = lam_params.to_vec();
        let lam_body = lam_body.clone();
        let elem_ty = read_elem_ty.clone();
        emit_combinator_loop(iterable, &iterable_ty, ElemAccess::Materialize(&elem_ty), builder, ctx,
            |i, elem, b, c| {
                // keep = body(elem, i) : Bool — `inline_lambda_body` binds by the lambda's OWN param
                // count, so a 1-param `x => …` ignores the surplus index arg.
                let idx = narrow_loop_index(i, b);
                let (pred_raw, pred_ty) =
                    inline_lambda_body(&lam_params, &lam_body, &[(elem, elem_ty.clone()), (idx, Type::Int32)], b, c);
                // Coerce the predicate result to an i1 Bool (concrete-Bool body: no-op; a Json/boxed-bool
                // body is unboxed via Coerce) — same as `lower_filter`'s inline path.
                let keep = if matches!(pred_ty, Type::Bool) {
                    pred_raw
                } else {
                    let d = b.alloc_temp(Type::Bool);
                    b.emit(Instruction::Coerce { dst: d, src: pred_raw, from_ty: pred_ty, to_ty: Type::Bool });
                    d
                };
                // FULLY reclaim the per-iteration element box (inner + shell): the predicate body never
                // moves the element into a result, so the box is genuinely dropped (no-op for a
                // flat-scalar read). Reclaimed BEFORE the keep/stop branch (both exits drop it), so it
                // runs once regardless of which way the branch goes; `keep` is an unboxed Bool and can
                // never alias the element box. A packed sealed-array source materialized a fresh
                // struct — release it too. (The inlined body may have switched blocks; this runs in
                // whatever block is now current, which the helper then `CondJump`s from.)
                free_combinator_elem_box_full(elem, &elem_ty, b);
                free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, b);
                LoopFlow::ContinueIf(keep)
            });
        return builder.const_temp(Const::Null);
    }

    // DEVIRTUALIZED FAST PATH (path-8-B generalized): a BARE statically-known predicate
    // (`xs.while(isValid)`) — call it DIRECTLY per element, no closure shell/indirect dispatch.
    let elem_ty = read_elem_ty;
    if let Some((target, native_params)) = bare_fn_call_target(&args[1], builder, ctx) {
        emit_combinator_loop(iterable, &iterable_ty, ElemAccess::Materialize(&elem_ty), builder, ctx,
            |i, elem, b, _| {
                let idx = narrow_loop_index(i, b);
                let keep = call_body_direct(
                    target.clone(), &[(elem, elem_ty.clone()), (idx, Type::Int32)],
                    &native_params, &Type::Bool, b);
                free_combinator_elem_box_full(elem, &elem_ty, b);
                free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, b);
                LoopFlow::ContinueIf(keep)
            });
        return builder.const_temp(Const::Null);
    }

    let body = lower_callback_in_safe_ctx(&args[1], builder, ctx);
    emit_combinator_loop(iterable, &iterable_ty, ElemAccess::Materialize(&elem_ty), builder, ctx,
        |i, elem, b, _| {
            // keep = body(elem, i) : Bool — continue only while true. `i` (the 0-based SOURCE index) is
            // narrowed Int64→Int32; truncated away when the callback declares only `(item)`.
            let idx = narrow_loop_index(i, b);
            let (keep, elem_boxes) = call_body_closure_with_elem_boxes(body, &[(elem, elem_ty.clone()), (idx, Type::Int32)], &param_tys, &Type::Bool, b);
            // FULLY reclaim the per-iteration element box (inner + shell), same mechanism + safety as
            // `lower_for`: `lin_array_get_tagged` returned a fresh +1 with its inner heap payload
            // retained, and the predicate body never moves that inner anywhere, so it must be fully
            // released or every heap-bearing element's inner leaks. The predicate's `Bool` return
            // (`keep`) is an unboxed scalar, so it can NEVER alias the element box — codegen treats the
            // non-pointer `other` as null and the release is unconditional. For a flat-scalar element
            // box this degrades to a shell free.
            for ebox in &elem_boxes {
                b.emit(Instruction::ReleaseIfDistinct { val: *ebox, other: keep });
            }
            // A PACKED sealed-array source materialized a fresh +1 element struct; `while` discards it
            // (the predicate body never moves the struct out), so release it or it leaks per iteration.
            free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, b);
            LoopFlow::ContinueIf(keep)
        });
    builder.const_temp(Const::Null)
}

/// Zero-arg `while(() => Boolean)` — condition-only loop. The callback takes no arguments and
/// returns `true` to continue, `false` to stop.
///
/// INLINE FAST PATH: when `callback` is an inlinable capturing lambda (or a stored lambda bound
/// via `inlinable_local_fn`), splice the body DIRECTLY into a `while_header → while_exit` loop
/// with no closure alloc, no per-iteration indirect call, no box/unbox ABI. The inlined body may
/// switch blocks (inner `if`/nested combinator); the back-edge `CondJump` is emitted from
/// whatever block the inlined body ends in, jumping back to `while_header` (true) or `while_exit`
/// (false). This replaces the `whileLoop` TCO path that previously allocated a closure each call
/// and dispatched it per iteration via `%ir_fnp(env)`.
///
/// Falls through to the stdlib call (`std_iter_while*`) when the callback is not inlinable —
/// guaranteeing a sound fallback for genuinely non-resolvable captures.
pub(crate) fn lower_zero_arg_while(
    callback: &TypedExpr,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Option<Temp> {
    let Some((lam_params, lam_body)) = inlinable_local_fn(callback, builder, ctx) else {
        return None;
    };
    let lam_params = lam_params.to_vec();
    let lam_body = lam_body.clone();

    let header = builder.alloc_block("while_header");
    let exit = builder.alloc_block("while_exit");
    builder.terminate(Terminator::Jump(header));

    builder.switch_to(header);
    // Inline the zero-arg body. It takes no element params; pass an empty slice and let
    // `inline_lambda_body` bind none (the lambda declares zero params). The body result is
    // `Boolean` (the loop-continuation predicate).
    let (pred_raw, pred_ty) = inline_lambda_body(&lam_params, &lam_body, &[], builder, ctx);
    // Coerce to a concrete i1 Bool (a Json/boxed-bool body is unboxed via Coerce; a concrete
    // Bool body is a no-op; same pattern as `lower_while` / `lower_filter` inline paths).
    let keep = if matches!(pred_ty, Type::Bool) {
        pred_raw
    } else {
        let d = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Coerce { dst: d, src: pred_raw, from_ty: pred_ty, to_ty: Type::Bool });
        d
    };
    // The inlined body may have switched blocks. Emit the back-edge CondJump from CURRENT block
    // (the body's final block after all inner control flow) back to `header` (true) or to `exit`
    // (false). This is sound even when the body emits its own blocks (inner if/for/match) because
    // `inline_lambda_body` returns control at the merge/exit point of those inner constructs.
    builder.terminate(Terminator::CondJump { cond: keep, then_block: header, else_block: exit });

    builder.switch_to(exit);
    Some(builder.const_temp(Const::Null))
}

/// `some(iterable, predicate)` → `true` if any element satisfies predicate, `false` otherwise.
/// Short-circuits on the first match. Uses the same `smat_fd` direct-struct-pointer path as
/// `for`/`while` when the source is a sealed-ptr array — avoids per-element materialization.
pub(crate) fn lower_some(args: &[TypedExpr], builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    let iterable_ty = args[0].ty();
    let (param_tys, _) = callback_signature(&args[1]);
    let read_elem_ty = combinator_read_elem_ty(&args[0], builder, ctx);
    let iterable = lower_expr(&args[0], builder, ctx);

    // DEVIRTUALIZED FAST PATH (path-8-B): a BARE statically-known predicate (`xs.some(isEven)`)
    // is called DIRECTLY per element — no closure shell / boxed-ABI / indirect dispatch.
    if let Some((target, native_params)) = bare_fn_call_target(&args[1], builder, ctx) {
        let elem_ty = read_elem_ty.clone();
        let len = emit_iterable_len(iterable, &iterable_ty, builder);
        let zero = builder.const_temp(Const::Int(0, Type::Int64));
        let false_val = builder.const_temp(Const::Bool(false));
        let preheader = builder.current_block;
        let header = builder.alloc_block("some_header");
        let body_block = builder.alloc_block("some_body");
        let latch = builder.alloc_block("some_latch");
        let exit = builder.alloc_block("some_exit");
        let i = builder.alloc_temp(Type::Int64);
        let i_next = builder.alloc_temp(Type::Int64);
        let found = builder.alloc_temp(Type::Bool);
        let found_next = builder.alloc_temp(Type::Bool);
        builder.terminate(Terminator::Jump(header));
        builder.switch_to(header);
        builder.emit(Instruction::Phi {
            dst: i, ty: Type::Int64, incomings: vec![(zero, preheader), (i_next, latch)],
        });
        builder.emit(Instruction::Phi {
            dst: found, ty: Type::Bool, incomings: vec![(false_val, preheader), (found_next, latch)],
        });
        let cond_len = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Binary {
            dst: cond_len, op: BinOp::Lt, lhs: i, rhs: len, operand_ty: Type::Int64, ty: Type::Bool,
        });
        let not_found = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Unary { dst: not_found, op: UnaryOp::Not, operand: found, ty: Type::Bool });
        let cond = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Binary {
            dst: cond, op: BinOp::And, lhs: cond_len, rhs: not_found, operand_ty: Type::Bool, ty: Type::Bool,
        });
        builder.terminate(Terminator::CondJump { cond, then_block: body_block, else_block: exit });
        builder.switch_to(body_block);
        let elem = builder.alloc_temp(elem_ty.clone());
        builder.emit(Instruction::Index {
            dst: elem, object: iterable, key: i,
            obj_ty: iterable_ty.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone(),
        nonneg: false,
        });
        let idx = narrow_loop_index(i, builder);
        let pred = call_body_direct(target, &[(elem, elem_ty.clone()), (idx, Type::Int32)], &native_params, &Type::Bool, builder);
        free_combinator_elem_box_full(elem, &elem_ty, builder);
        free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, builder);
        let back_block = builder.current_block;
        builder.emit(Instruction::Binary {
            dst: found_next, op: BinOp::Or, lhs: found, rhs: pred, operand_ty: Type::Bool, ty: Type::Bool,
        });
        builder.terminate(Terminator::Jump(latch));
        builder.switch_to(latch);
        let one = builder.const_temp(Const::Int(1, Type::Int64));
        builder.emit(Instruction::Binary {
            dst: i_next, op: BinOp::Add, lhs: i, rhs: one, operand_ty: Type::Int64, ty: Type::Int64,
        });
        builder.terminate(Terminator::Jump(header));
        builder.patch_phi_incoming(header, i, body_block, back_block);
        builder.patch_phi_incoming(header, found, body_block, back_block);
        builder.switch_to(exit);
        return found;
    }

    // INLINE FAST PATH: literal lambda is spliced into the loop — no closure alloc, no
    // per-element box ABI / indirect call. The loop exits on first match (ContinueIf(!found)).
    // Element-box RC: mirrors `lower_while`'s predicate path exactly (fully reclaimed).
    // Result: build an explicit PHI (Bool) that starts `false` and flips to `true` when the
    // predicate fires, then the header re-checks it to short-circuit.
    if let Some((lam_params, lam_body)) = inlinable_local_fn(&args[1], builder, ctx) {
        let lam_params = lam_params.to_vec();
        let lam_body = lam_body.clone();

        // PATH-1 in-place packed iteration: a packed sealed-scalar array source — the predicate
        // uses the element ONLY for field reads (`elem_used_only_for_scalar_fields`) — iterates
        // over the contiguous buffer with NO per-element materialize. The element param is bound
        // to a borrowed `(array, index)` view; `p["field"]` reads lower to const-offset
        // `SealedArrayFieldGet` loads. Falls back to the generic materialize path when the
        // predicate uses the element as a whole value (passing it, storing it, etc.).
        if is_sealed_scalar_array(&iterable_ty)
            && lam_params.first().map(|p| elem_used_only_for_scalar_fields(p.slot, &lam_body)).unwrap_or(false)
        {
            let static_elem = iter_elem_type(&iterable_ty);
            let len = emit_iterable_len(iterable, &iterable_ty, builder);
            let zero = builder.const_temp(Const::Int(0, Type::Int64));
            let false_val = builder.const_temp(Const::Bool(false));
            let preheader = builder.current_block;
            let header = builder.alloc_block("some_header");
            let body_block = builder.alloc_block("some_body");
            let latch = builder.alloc_block("some_latch");
            let exit = builder.alloc_block("some_exit");
            let i = builder.alloc_temp(Type::Int64);
            let i_next = builder.alloc_temp(Type::Int64);
            let found = builder.alloc_temp(Type::Bool);
            let found_next = builder.alloc_temp(Type::Bool);
            builder.terminate(Terminator::Jump(header));
            builder.switch_to(header);
            builder.emit(Instruction::Phi {
                dst: i, ty: Type::Int64, incomings: vec![(zero, preheader), (i_next, latch)],
            });
            builder.emit(Instruction::Phi {
                dst: found, ty: Type::Bool, incomings: vec![(false_val, preheader), (found_next, latch)],
            });
            let cond_len = builder.alloc_temp(Type::Bool);
            builder.emit(Instruction::Binary {
                dst: cond_len, op: BinOp::Lt, lhs: i, rhs: len,
                operand_ty: Type::Int64, ty: Type::Bool,
            });
            let not_found = builder.alloc_temp(Type::Bool);
            builder.emit(Instruction::Unary {
                dst: not_found, op: UnaryOp::Not, operand: found, ty: Type::Bool,
            });
            let cond = builder.alloc_temp(Type::Bool);
            builder.emit(Instruction::Binary {
                dst: cond, op: BinOp::And, lhs: cond_len, rhs: not_found,
                operand_ty: Type::Bool, ty: Type::Bool,
            });
            builder.terminate(Terminator::CondJump { cond, then_block: body_block, else_block: exit });
            builder.switch_to(body_block);
            let idx = narrow_loop_index(i, builder);
            // Bind element as packed view (no materialize). Any p["field"] inside the body lowers
            // to a const-offset SealedArrayFieldGet; whole-value uses fall back to Index.
            let (pred_raw, pred_ty) = inline_lambda_body_packed_view(
                &lam_params, &lam_body, iterable, i, &static_elem, idx, builder, ctx);
            let pred = if matches!(pred_ty, Type::Bool) {
                pred_raw
            } else {
                let d = builder.alloc_temp(Type::Bool);
                builder.emit(Instruction::Coerce { dst: d, src: pred_raw, from_ty: pred_ty, to_ty: Type::Bool });
                d
            };
            // No element box to reclaim — the packed view never allocated a struct.
            builder.emit(Instruction::Binary {
                dst: found_next, op: BinOp::Or, lhs: found, rhs: pred,
                operand_ty: Type::Bool, ty: Type::Bool,
            });
            let back_block = builder.current_block;
            builder.terminate(Terminator::Jump(latch));
            builder.switch_to(latch);
            let one = builder.const_temp(Const::Int(1, Type::Int64));
            builder.emit(Instruction::Binary {
                dst: i_next, op: BinOp::Add, lhs: i, rhs: one,
                operand_ty: Type::Int64, ty: Type::Int64,
            });
            builder.terminate(Terminator::Jump(header));
            builder.patch_phi_incoming(header, i, body_block, back_block);
            builder.patch_phi_incoming(header, found, body_block, back_block);
            builder.switch_to(exit);
            return found;
        }

        let elem_ty = read_elem_ty.clone();

        // Explicit loop structure so we can carry the Bool result through a phi.
        let len = emit_iterable_len(iterable, &iterable_ty, builder);
        let zero = builder.const_temp(Const::Int(0, Type::Int64));
        let false_val = builder.const_temp(Const::Bool(false));

        let preheader = builder.current_block;
        let header = builder.alloc_block("some_header");
        let body_block = builder.alloc_block("some_body");
        let latch = builder.alloc_block("some_latch");
        let exit = builder.alloc_block("some_exit");

        let i = builder.alloc_temp(Type::Int64);
        let i_next = builder.alloc_temp(Type::Int64);
        let found = builder.alloc_temp(Type::Bool);
        let found_next = builder.alloc_temp(Type::Bool);

        builder.terminate(Terminator::Jump(header));

        builder.switch_to(header);
        builder.emit(Instruction::Phi {
            dst: i, ty: Type::Int64, incomings: vec![(zero, preheader), (i_next, latch)],
        });
        builder.emit(Instruction::Phi {
            dst: found, ty: Type::Bool, incomings: vec![(false_val, preheader), (found_next, latch)],
        });
        // Continue while i < len AND !found
        let cond_len = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Binary {
            dst: cond_len, op: BinOp::Lt, lhs: i, rhs: len,
            operand_ty: Type::Int64, ty: Type::Bool,
        });
        let not_found = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Unary {
            dst: not_found, op: UnaryOp::Not, operand: found, ty: Type::Bool,
        });
        let cond = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Binary {
            dst: cond, op: BinOp::And, lhs: cond_len, rhs: not_found,
            operand_ty: Type::Bool, ty: Type::Bool,
        });
        builder.terminate(Terminator::CondJump { cond, then_block: body_block, else_block: exit });

        builder.switch_to(body_block);
        let elem = builder.alloc_temp(elem_ty.clone());
        builder.emit(Instruction::Index {
            dst: elem, object: iterable, key: i,
            obj_ty: iterable_ty.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone(),
        nonneg: false,
        });
        let idx = narrow_loop_index(i, builder);
        let (pred_raw, pred_ty) =
            inline_lambda_body(&lam_params, &lam_body, &[(elem, elem_ty.clone()), (idx, Type::Int32)], builder, ctx);
        let pred = if matches!(pred_ty, Type::Bool) {
            pred_raw
        } else {
            let d = builder.alloc_temp(Type::Bool);
            builder.emit(Instruction::Coerce { dst: d, src: pred_raw, from_ty: pred_ty, to_ty: Type::Bool });
            d
        };
        free_combinator_elem_box_full(elem, &elem_ty, builder);
        free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, builder);
        // Compute found_next = found || pred HERE (in back_block, after inline_lambda_body),
        // NOT in `latch`. `latch` is allocated BEFORE inline_lambda_body runs, so its internal
        // blocks (added during body lowering) may come AFTER `latch` in the builder's block list —
        // codegen would then process `latch` before the block defining `pred`, causing "undefined
        // rhs temp" on `pred`. Emitting found_next here ensures it's always defined before latch.
        builder.emit(Instruction::Binary {
            dst: found_next, op: BinOp::Or, lhs: found, rhs: pred,
            operand_ty: Type::Bool, ty: Type::Bool,
        });
        let back_block = builder.current_block;
        builder.terminate(Terminator::Jump(latch));

        builder.switch_to(latch);
        let one = builder.const_temp(Const::Int(1, Type::Int64));
        builder.emit(Instruction::Binary {
            dst: i_next, op: BinOp::Add, lhs: i, rhs: one,
            operand_ty: Type::Int64, ty: Type::Int64,
        });
        builder.terminate(Terminator::Jump(header));
        builder.patch_phi_incoming(header, i, body_block, back_block);
        builder.patch_phi_incoming(header, found, body_block, back_block);

        builder.switch_to(exit);
        return found;
    }

    // Non-inline path: callback is a pre-compiled closure. Use a heap cell for the result.
    let body = lower_callback_in_safe_ctx(&args[1], builder, ctx);
    let elem_ty = read_elem_ty;
    // Allocate a Bool cell initialised to `false`.
    let false_init = builder.const_temp(Const::Bool(false));
    let result_cell = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::MakeCell { dst: result_cell, init: false_init, ty: Type::Bool });
    emit_combinator_loop(iterable, &iterable_ty, ElemAccess::Materialize(&elem_ty), builder, ctx,
        |i, elem, b, _| {
            let idx = narrow_loop_index(i, b);
            let idx_box = if param_tys.len() >= 2 {
                (box_to_json(idx, &Type::Int32, b), Type::TypeVar(u32::MAX))
            } else {
                (idx, Type::Int32)
            };
            let (pred, elem_boxes) = call_body_closure_with_elem_boxes(
                body, &[(elem, elem_ty.clone()), idx_box], &param_tys, &Type::Bool, b);
            // Reclaim element boxes (predicate never moves the element).
            for ebox in &elem_boxes {
                b.emit(Instruction::ReleaseIfDistinct { val: *ebox, other: pred });
            }
            free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, b);
            // Write found=true to cell, then continue while !found.
            b.emit(Instruction::CellSet { cell: result_cell, value: pred, ty: Type::Bool });
            let not_found = b.alloc_temp(Type::Bool);
            b.emit(Instruction::Unary { dst: not_found, op: UnaryOp::Not, operand: pred, ty: Type::Bool });
            LoopFlow::ContinueIf(not_found)
        });
    let result = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::CellGet { dst: result, cell: result_cell, ty: Type::Bool });
    builder.emit(Instruction::FreeCell { cell: result_cell, ty: Type::Bool });
    result
}

/// `every(iterable, predicate)` → `true` if all elements satisfy predicate, `false` otherwise.
/// Short-circuits on the first failure. Mirrors `lower_some` but inverts the semantics.
pub(crate) fn lower_every(args: &[TypedExpr], builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    let iterable_ty = args[0].ty();
    let (param_tys, _) = callback_signature(&args[1]);
    let read_elem_ty = combinator_read_elem_ty(&args[0], builder, ctx);
    let iterable = lower_expr(&args[0], builder, ctx);

    // DEVIRTUALIZED FAST PATH (path-8-B): a BARE statically-known predicate (`xs.every(isPositive)`)
    // is called DIRECTLY per element — no closure shell / boxed-ABI / indirect dispatch.
    if let Some((target, native_params)) = bare_fn_call_target(&args[1], builder, ctx) {
        let elem_ty = read_elem_ty.clone();
        let len = emit_iterable_len(iterable, &iterable_ty, builder);
        let zero = builder.const_temp(Const::Int(0, Type::Int64));
        let true_val = builder.const_temp(Const::Bool(true));
        let preheader = builder.current_block;
        let header = builder.alloc_block("every_header");
        let body_block = builder.alloc_block("every_body");
        let latch = builder.alloc_block("every_latch");
        let exit = builder.alloc_block("every_exit");
        let i = builder.alloc_temp(Type::Int64);
        let i_next = builder.alloc_temp(Type::Int64);
        let all_match = builder.alloc_temp(Type::Bool);
        let all_match_next = builder.alloc_temp(Type::Bool);
        builder.terminate(Terminator::Jump(header));
        builder.switch_to(header);
        builder.emit(Instruction::Phi {
            dst: i, ty: Type::Int64, incomings: vec![(zero, preheader), (i_next, latch)],
        });
        builder.emit(Instruction::Phi {
            dst: all_match, ty: Type::Bool, incomings: vec![(true_val, preheader), (all_match_next, latch)],
        });
        let cond_len = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Binary {
            dst: cond_len, op: BinOp::Lt, lhs: i, rhs: len, operand_ty: Type::Int64, ty: Type::Bool,
        });
        let cond = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Binary {
            dst: cond, op: BinOp::And, lhs: cond_len, rhs: all_match, operand_ty: Type::Bool, ty: Type::Bool,
        });
        builder.terminate(Terminator::CondJump { cond, then_block: body_block, else_block: exit });
        builder.switch_to(body_block);
        let elem = builder.alloc_temp(elem_ty.clone());
        builder.emit(Instruction::Index {
            dst: elem, object: iterable, key: i,
            obj_ty: iterable_ty.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone(),
        nonneg: false,
        });
        let idx = narrow_loop_index(i, builder);
        let pred = call_body_direct(target, &[(elem, elem_ty.clone()), (idx, Type::Int32)], &native_params, &Type::Bool, builder);
        free_combinator_elem_box_full(elem, &elem_ty, builder);
        free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, builder);
        builder.emit(Instruction::Binary {
            dst: all_match_next, op: BinOp::And, lhs: all_match, rhs: pred, operand_ty: Type::Bool, ty: Type::Bool,
        });
        let back_block = builder.current_block;
        builder.terminate(Terminator::Jump(latch));
        builder.switch_to(latch);
        let one = builder.const_temp(Const::Int(1, Type::Int64));
        builder.emit(Instruction::Binary {
            dst: i_next, op: BinOp::Add, lhs: i, rhs: one, operand_ty: Type::Int64, ty: Type::Int64,
        });
        builder.terminate(Terminator::Jump(header));
        builder.patch_phi_incoming(header, i, body_block, back_block);
        builder.patch_phi_incoming(header, all_match, body_block, back_block);
        builder.switch_to(exit);
        return all_match;
    }

    // INLINE FAST PATH: literal lambda spliced in. Loop while predicate holds (stop on first false).
    // Result starts `true`; the PHI flips to `false` when predicate fails.
    if let Some((lam_params, lam_body)) = inlinable_local_fn(&args[1], builder, ctx) {
        let lam_params = lam_params.to_vec();
        let lam_body = lam_body.clone();

        // PATH-1 in-place packed iteration: mirrors lower_some's packed path but tracks `all_match`
        // (starts true, set to false on first failing element). No per-element materialize.
        if is_sealed_scalar_array(&iterable_ty)
            && lam_params.first().map(|p| elem_used_only_for_scalar_fields(p.slot, &lam_body)).unwrap_or(false)
        {
            let static_elem = iter_elem_type(&iterable_ty);
            let len = emit_iterable_len(iterable, &iterable_ty, builder);
            let zero = builder.const_temp(Const::Int(0, Type::Int64));
            let true_val = builder.const_temp(Const::Bool(true));
            let preheader = builder.current_block;
            let header = builder.alloc_block("every_header");
            let body_block = builder.alloc_block("every_body");
            let latch = builder.alloc_block("every_latch");
            let exit = builder.alloc_block("every_exit");
            let i = builder.alloc_temp(Type::Int64);
            let i_next = builder.alloc_temp(Type::Int64);
            let all_match = builder.alloc_temp(Type::Bool);
            let all_match_next = builder.alloc_temp(Type::Bool);
            builder.terminate(Terminator::Jump(header));
            builder.switch_to(header);
            builder.emit(Instruction::Phi {
                dst: i, ty: Type::Int64, incomings: vec![(zero, preheader), (i_next, latch)],
            });
            builder.emit(Instruction::Phi {
                dst: all_match, ty: Type::Bool, incomings: vec![(true_val, preheader), (all_match_next, latch)],
            });
            let cond_len = builder.alloc_temp(Type::Bool);
            builder.emit(Instruction::Binary {
                dst: cond_len, op: BinOp::Lt, lhs: i, rhs: len,
                operand_ty: Type::Int64, ty: Type::Bool,
            });
            let cond = builder.alloc_temp(Type::Bool);
            builder.emit(Instruction::Binary {
                dst: cond, op: BinOp::And, lhs: cond_len, rhs: all_match,
                operand_ty: Type::Bool, ty: Type::Bool,
            });
            builder.terminate(Terminator::CondJump { cond, then_block: body_block, else_block: exit });
            builder.switch_to(body_block);
            let idx = narrow_loop_index(i, builder);
            let (pred_raw, pred_ty) = inline_lambda_body_packed_view(
                &lam_params, &lam_body, iterable, i, &static_elem, idx, builder, ctx);
            let pred = if matches!(pred_ty, Type::Bool) {
                pred_raw
            } else {
                let d = builder.alloc_temp(Type::Bool);
                builder.emit(Instruction::Coerce { dst: d, src: pred_raw, from_ty: pred_ty, to_ty: Type::Bool });
                d
            };
            builder.emit(Instruction::Binary {
                dst: all_match_next, op: BinOp::And, lhs: all_match, rhs: pred,
                operand_ty: Type::Bool, ty: Type::Bool,
            });
            let back_block = builder.current_block;
            builder.terminate(Terminator::Jump(latch));
            builder.switch_to(latch);
            let one = builder.const_temp(Const::Int(1, Type::Int64));
            builder.emit(Instruction::Binary {
                dst: i_next, op: BinOp::Add, lhs: i, rhs: one,
                operand_ty: Type::Int64, ty: Type::Int64,
            });
            builder.terminate(Terminator::Jump(header));
            builder.patch_phi_incoming(header, i, body_block, back_block);
            builder.patch_phi_incoming(header, all_match, body_block, back_block);
            builder.switch_to(exit);
            return all_match;
        }

        let elem_ty = read_elem_ty.clone();

        let len = emit_iterable_len(iterable, &iterable_ty, builder);
        let zero = builder.const_temp(Const::Int(0, Type::Int64));
        let true_val = builder.const_temp(Const::Bool(true));

        let preheader = builder.current_block;
        let header = builder.alloc_block("every_header");
        let body_block = builder.alloc_block("every_body");
        let latch = builder.alloc_block("every_latch");
        let exit = builder.alloc_block("every_exit");

        let i = builder.alloc_temp(Type::Int64);
        let i_next = builder.alloc_temp(Type::Int64);
        let all_match = builder.alloc_temp(Type::Bool);
        let all_match_next = builder.alloc_temp(Type::Bool);

        builder.terminate(Terminator::Jump(header));

        builder.switch_to(header);
        builder.emit(Instruction::Phi {
            dst: i, ty: Type::Int64, incomings: vec![(zero, preheader), (i_next, latch)],
        });
        builder.emit(Instruction::Phi {
            dst: all_match, ty: Type::Bool, incomings: vec![(true_val, preheader), (all_match_next, latch)],
        });
        // Continue while i < len AND all_match
        let cond_len = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Binary {
            dst: cond_len, op: BinOp::Lt, lhs: i, rhs: len,
            operand_ty: Type::Int64, ty: Type::Bool,
        });
        let cond = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Binary {
            dst: cond, op: BinOp::And, lhs: cond_len, rhs: all_match,
            operand_ty: Type::Bool, ty: Type::Bool,
        });
        builder.terminate(Terminator::CondJump { cond, then_block: body_block, else_block: exit });

        builder.switch_to(body_block);
        let elem = builder.alloc_temp(elem_ty.clone());
        builder.emit(Instruction::Index {
            dst: elem, object: iterable, key: i,
            obj_ty: iterable_ty.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone(),
        nonneg: false,
        });
        let idx = narrow_loop_index(i, builder);
        let (pred_raw, pred_ty) =
            inline_lambda_body(&lam_params, &lam_body, &[(elem, elem_ty.clone()), (idx, Type::Int32)], builder, ctx);
        let pred = if matches!(pred_ty, Type::Bool) {
            pred_raw
        } else {
            let d = builder.alloc_temp(Type::Bool);
            builder.emit(Instruction::Coerce { dst: d, src: pred_raw, from_ty: pred_ty, to_ty: Type::Bool });
            d
        };
        free_combinator_elem_box_full(elem, &elem_ty, builder);
        free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, builder);
        // Compute all_match_next = all_match && pred HERE (in back_block, after inline_lambda_body),
        // NOT in `latch`. `latch` is allocated before inline_lambda_body runs; blocks added during
        // body lowering come AFTER `latch` in the builder's list — codegen would process `latch`
        // before those blocks, leaving `pred` undefined. Emitting all_match_next here ensures it's
        // always defined before latch. (Same fix as lower_some's found_next computation.)
        builder.emit(Instruction::Binary {
            dst: all_match_next, op: BinOp::And, lhs: all_match, rhs: pred,
            operand_ty: Type::Bool, ty: Type::Bool,
        });
        let back_block = builder.current_block;
        builder.terminate(Terminator::Jump(latch));

        builder.switch_to(latch);
        let one = builder.const_temp(Const::Int(1, Type::Int64));
        builder.emit(Instruction::Binary {
            dst: i_next, op: BinOp::Add, lhs: i, rhs: one,
            operand_ty: Type::Int64, ty: Type::Int64,
        });
        builder.terminate(Terminator::Jump(header));
        builder.patch_phi_incoming(header, i, body_block, back_block);
        builder.patch_phi_incoming(header, all_match, body_block, back_block);

        builder.switch_to(exit);
        return all_match;
    }

    // Non-inline path: use a MakeCell for the result (starts true, flipped to false on failure).
    let body = lower_callback_in_safe_ctx(&args[1], builder, ctx);
    let elem_ty = read_elem_ty;
    let true_init = builder.const_temp(Const::Bool(true));
    let result_cell = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::MakeCell { dst: result_cell, init: true_init, ty: Type::Bool });
    emit_combinator_loop(iterable, &iterable_ty, ElemAccess::Materialize(&elem_ty), builder, ctx,
        |i, elem, b, _| {
            let idx = narrow_loop_index(i, b);
            let idx_box = if param_tys.len() >= 2 {
                (box_to_json(idx, &Type::Int32, b), Type::TypeVar(u32::MAX))
            } else {
                (idx, Type::Int32)
            };
            let (pred_tv, elem_boxes) = call_body_closure_with_elem_boxes(
                body, &[(elem, elem_ty.clone()), idx_box], &param_tys, &Type::Bool, b);
            for ebox in &elem_boxes {
                b.emit(Instruction::ReleaseIfDistinct { val: *ebox, other: pred_tv });
            }
            free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, b);
            // Write predicate result and continue while it holds.
            b.emit(Instruction::CellSet { cell: result_cell, value: pred_tv, ty: Type::Bool });
            LoopFlow::ContinueIf(pred_tv)
        });
    let result = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::CellGet { dst: result, cell: result_cell, ty: Type::Bool });
    builder.emit(Instruction::FreeCell { cell: result_cell, ty: Type::Bool });
    result
}

/// `find(iterable, predicate)` → first element satisfying predicate, or `Null`.
/// Short-circuits on first match. Returns `T | Null`.
pub(crate) fn lower_find(args: &[TypedExpr], result_type: &Type, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    let iterable_ty = args[0].ty();
    let (ni_param_tys, _) = callback_signature(&args[1]);
    let read_elem_ty = combinator_read_elem_ty(&args[0], builder, ctx);
    let iterable = lower_expr(&args[0], builder, ctx);

    // DEVIRTUALIZED FAST PATH (path-8-B): a BARE statically-known predicate (`xs.find(isEven)`)
    // is called DIRECTLY per element — no closure shell / boxed-ABI / indirect dispatch.
    if let Some((target, native_params)) = bare_fn_call_target(&args[1], builder, ctx) {
        let elem_ty = read_elem_ty.clone();
        let json = Type::TypeVar(u32::MAX);
        let len = emit_iterable_len(iterable, &iterable_ty, builder);
        let zero = builder.const_temp(Const::Int(0, Type::Int64));
        let null_val = builder.const_temp(Const::Null);
        let false_val = builder.const_temp(Const::Bool(false));
        let preheader = builder.current_block;
        let header = builder.alloc_block("find_header");
        let body_block = builder.alloc_block("find_body");
        let latch = builder.alloc_block("find_latch");
        let exit = builder.alloc_block("find_exit");
        let i = builder.alloc_temp(Type::Int64);
        let i_next = builder.alloc_temp(Type::Int64);
        let result = builder.alloc_temp(json.clone());
        let result_next = builder.alloc_temp(json.clone());
        let found = builder.alloc_temp(Type::Bool);
        let found_next = builder.alloc_temp(Type::Bool);
        builder.terminate(Terminator::Jump(header));
        builder.switch_to(header);
        builder.emit(Instruction::Phi {
            dst: i, ty: Type::Int64, incomings: vec![(zero, preheader), (i_next, latch)],
        });
        builder.emit(Instruction::Phi {
            dst: result, ty: json.clone(), incomings: vec![(null_val, preheader), (result_next, latch)],
        });
        builder.emit(Instruction::Phi {
            dst: found, ty: Type::Bool, incomings: vec![(false_val, preheader), (found_next, latch)],
        });
        let cond_len = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Binary {
            dst: cond_len, op: BinOp::Lt, lhs: i, rhs: len, operand_ty: Type::Int64, ty: Type::Bool,
        });
        let not_found = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Unary { dst: not_found, op: UnaryOp::Not, operand: found, ty: Type::Bool });
        let cond = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Binary {
            dst: cond, op: BinOp::And, lhs: cond_len, rhs: not_found, operand_ty: Type::Bool, ty: Type::Bool,
        });
        builder.terminate(Terminator::CondJump { cond, then_block: body_block, else_block: exit });
        builder.switch_to(body_block);
        let elem = builder.alloc_temp(elem_ty.clone());
        builder.emit(Instruction::Index {
            dst: elem, object: iterable, key: i,
            obj_ty: iterable_ty.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone(),
        nonneg: false,
        });
        let idx = narrow_loop_index(i, builder);
        let pred = call_body_direct(target, &[(elem, elem_ty.clone()), (idx, Type::Int32)], &native_params, &Type::Bool, builder);
        // Keep/skip based on pred. Box elem when kept (to carry as T|Null result).
        let llvm_merge = builder.alloc_block("find_merge");
        let llvm_keep = builder.alloc_block("find_keep");
        let llvm_skip = builder.alloc_block("find_skip");
        builder.terminate(Terminator::CondJump { cond: pred, then_block: llvm_keep, else_block: llvm_skip });
        builder.switch_to(llvm_keep);
        let elem_boxed = box_to_json(elem, &elem_ty, builder);
        let keep_end = builder.current_block;
        builder.terminate(Terminator::Jump(llvm_merge));
        builder.switch_to(llvm_skip);
        free_combinator_elem_box_full(elem, &elem_ty, builder);
        free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, builder);
        let skip_end = builder.current_block;
        builder.terminate(Terminator::Jump(llvm_merge));
        builder.switch_to(llvm_merge);
        builder.emit(Instruction::Phi {
            dst: result_next, ty: json.clone(),
            incomings: vec![(elem_boxed, keep_end), (result, skip_end)],
        });
        builder.emit(Instruction::Binary {
            dst: found_next, op: BinOp::Or, lhs: found, rhs: pred, operand_ty: Type::Bool, ty: Type::Bool,
        });
        builder.terminate(Terminator::Jump(latch));
        builder.switch_to(latch);
        let one = builder.const_temp(Const::Int(1, Type::Int64));
        builder.emit(Instruction::Binary {
            dst: i_next, op: BinOp::Add, lhs: i, rhs: one, operand_ty: Type::Int64, ty: Type::Int64,
        });
        builder.terminate(Terminator::Jump(header));
        builder.switch_to(exit);
        if !matches!(result_type, Type::TypeVar(_)) {
            let dst = builder.alloc_temp(result_type.clone());
            builder.emit(Instruction::Coerce { dst, src: result, from_ty: json, to_ty: result_type.clone() });
            return dst;
        }
        return result;
    }

    // INLINE FAST PATH: literal lambda spliced in.
    if let Some((lam_params, lam_body)) = inlinable_local_fn(&args[1], builder, ctx) {
        let lam_params = lam_params.to_vec();
        let lam_body = lam_body.clone();
        let elem_ty = read_elem_ty.clone();
        let json = Type::TypeVar(u32::MAX);

        let len = emit_iterable_len(iterable, &iterable_ty, builder);
        let zero = builder.const_temp(Const::Int(0, Type::Int64));
        let null_val = builder.const_temp(Const::Null);

        let preheader = builder.current_block;
        let header = builder.alloc_block("find_header");
        let body_block = builder.alloc_block("find_body");
        let latch = builder.alloc_block("find_latch");
        let exit = builder.alloc_block("find_exit");

        let i = builder.alloc_temp(Type::Int64);
        let i_next = builder.alloc_temp(Type::Int64);
        // result: union (T | Null), starts Null
        let result = builder.alloc_temp(json.clone());
        let result_next = builder.alloc_temp(json.clone());
        // found: Bool phi to short-circuit
        let found = builder.alloc_temp(Type::Bool);
        let found_next = builder.alloc_temp(Type::Bool);
        let false_val = builder.const_temp(Const::Bool(false));

        builder.terminate(Terminator::Jump(header));

        builder.switch_to(header);
        builder.emit(Instruction::Phi {
            dst: i, ty: Type::Int64, incomings: vec![(zero, preheader), (i_next, latch)],
        });
        builder.emit(Instruction::Phi {
            dst: result, ty: json.clone(), incomings: vec![(null_val, preheader), (result_next, latch)],
        });
        builder.emit(Instruction::Phi {
            dst: found, ty: Type::Bool, incomings: vec![(false_val, preheader), (found_next, latch)],
        });
        // Continue while i < len AND !found
        let cond_len = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Binary {
            dst: cond_len, op: BinOp::Lt, lhs: i, rhs: len,
            operand_ty: Type::Int64, ty: Type::Bool,
        });
        let not_found = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Unary {
            dst: not_found, op: UnaryOp::Not, operand: found, ty: Type::Bool,
        });
        let cond = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Binary {
            dst: cond, op: BinOp::And, lhs: cond_len, rhs: not_found,
            operand_ty: Type::Bool, ty: Type::Bool,
        });
        builder.terminate(Terminator::CondJump { cond, then_block: body_block, else_block: exit });

        builder.switch_to(body_block);
        let elem = builder.alloc_temp(elem_ty.clone());
        builder.emit(Instruction::Index {
            dst: elem, object: iterable, key: i,
            obj_ty: iterable_ty.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone(),
        nonneg: false,
        });
        let idx = narrow_loop_index(i, builder);
        let (pred_raw, pred_ty) =
            inline_lambda_body(&lam_params, &lam_body, &[(elem, elem_ty.clone()), (idx, Type::Int32)], builder, ctx);
        let pred = if matches!(pred_ty, Type::Bool) {
            pred_raw
        } else {
            let d = builder.alloc_temp(Type::Bool);
            builder.emit(Instruction::Coerce { dst: d, src: pred_raw, from_ty: pred_ty, to_ty: Type::Bool });
            d
        };
        // When pred is true, box the element to union for the result phi (T | Null).
        // When pred is false, we'll reuse `result` (the previous phi value). We implement this
        // as a conditional branch: pred_true → box elem; pred_false → reuse result; merge via phi.
        let llvm_merge = builder.alloc_block("find_merge");
        let llvm_keep = builder.alloc_block("find_keep");
        let llvm_skip = builder.alloc_block("find_skip");
        let body_end_block = builder.current_block;
        builder.terminate(Terminator::CondJump { cond: pred, then_block: llvm_keep, else_block: llvm_skip });

        // pred_true: box the element, retain it (+1)
        builder.switch_to(llvm_keep);
        let elem_boxed = box_to_json(elem, &elem_ty, builder);
        let keep_end = builder.current_block;
        builder.terminate(Terminator::Jump(llvm_merge));

        // pred_false: release element (not kept), keep previous result
        builder.switch_to(llvm_skip);
        free_combinator_elem_box_full(elem, &elem_ty, builder);
        free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, builder);
        let skip_end = builder.current_block;
        builder.terminate(Terminator::Jump(llvm_merge));

        // merge: pick elem_boxed or result
        builder.switch_to(llvm_merge);
        builder.emit(Instruction::Phi {
            dst: result_next, ty: json.clone(),
            incomings: vec![(elem_boxed, keep_end), (result, skip_end)],
        });
        builder.emit(Instruction::Binary {
            dst: found_next, op: BinOp::Or, lhs: found, rhs: pred,
            operand_ty: Type::Bool, ty: Type::Bool,
        });
        builder.terminate(Terminator::Jump(latch));

        builder.switch_to(latch);
        let one = builder.const_temp(Const::Int(1, Type::Int64));
        builder.emit(Instruction::Binary {
            dst: i_next, op: BinOp::Add, lhs: i, rhs: one,
            operand_ty: Type::Int64, ty: Type::Int64,
        });
        builder.terminate(Terminator::Jump(header));
        // Suppress "unused" warnings for body_end_block — it's needed for the conditional branch.
        let _ = body_end_block;

        builder.switch_to(exit);
        // Coerce result to the declared return type (T | Null).
        if !matches!(result_type, Type::TypeVar(_)) {
            let dst = builder.alloc_temp(result_type.clone());
            builder.emit(Instruction::Coerce { dst, src: result, from_ty: json, to_ty: result_type.clone() });
            return dst;
        }
        return result;
    }

    // Non-inline path: callback is a pre-compiled closure.
    // Use an explicit loop identical to the inline path, but call the closure via CallTarget::Indirect.
    // We ALWAYS materialize elements to TaggedVal (json) so the element is always a fresh owned +1
    // reference (via lin_array_get_tagged). This avoids double-ownership issues with ref types.
    let body = lower_callback_in_safe_ctx(&args[1], builder, ctx);
    let param_tys = ni_param_tys;
    let _ = read_elem_ty; // elem is always materialized to json in non-inline path
    let json = Type::TypeVar(u32::MAX);
    // Materialize elem as tagged: always Index with result_ty = json.
    let ni_elem_ty = json.clone();

    let len = emit_iterable_len(iterable, &iterable_ty, builder);
    let zero = builder.const_temp(Const::Int(0, Type::Int64));
    let null_val2 = builder.const_temp(Const::Null);
    let false_val2 = builder.const_temp(Const::Bool(false));

    let preheader2 = builder.current_block;
    let header2 = builder.alloc_block("nifind_header");
    let body_block2 = builder.alloc_block("nifind_body");
    let latch2 = builder.alloc_block("nifind_latch");
    let exit2 = builder.alloc_block("nifind_exit");

    let i2 = builder.alloc_temp(Type::Int64);
    let i2_next = builder.alloc_temp(Type::Int64);
    let result2 = builder.alloc_temp(json.clone());
    let result2_next = builder.alloc_temp(json.clone());
    let found2 = builder.alloc_temp(Type::Bool);
    let found2_next = builder.alloc_temp(Type::Bool);

    builder.terminate(Terminator::Jump(header2));

    builder.switch_to(header2);
    builder.emit(Instruction::Phi {
        dst: i2, ty: Type::Int64, incomings: vec![(zero, preheader2), (i2_next, latch2)],
    });
    builder.emit(Instruction::Phi {
        dst: result2, ty: json.clone(), incomings: vec![(null_val2, preheader2), (result2_next, latch2)],
    });
    builder.emit(Instruction::Phi {
        dst: found2, ty: Type::Bool, incomings: vec![(false_val2, preheader2), (found2_next, latch2)],
    });
    let cond_len2 = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::Binary {
        dst: cond_len2, op: BinOp::Lt, lhs: i2, rhs: len, operand_ty: Type::Int64, ty: Type::Bool,
    });
    let not_found2 = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::Unary {
        dst: not_found2, op: UnaryOp::Not, operand: found2, ty: Type::Bool,
    });
    let cond2 = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::Binary {
        dst: cond2, op: BinOp::And, lhs: cond_len2, rhs: not_found2,
        operand_ty: Type::Bool, ty: Type::Bool,
    });
    builder.terminate(Terminator::CondJump { cond: cond2, then_block: body_block2, else_block: exit2 });

    builder.switch_to(body_block2);
    // Read as TaggedVal (always). lin_array_get_tagged returns a fresh owned +1 reference.
    let elem2 = builder.alloc_temp(ni_elem_ty.clone());
    builder.emit(Instruction::Index {
        dst: elem2, object: iterable, key: i2,
        obj_ty: iterable_ty.clone(), key_ty: Type::Int64, result_ty: ni_elem_ty.clone(),
    nonneg: false,
    });
    let idx2 = narrow_loop_index(i2, builder);
    // Call the closure with (elem, idx): elem is already union so it's passed directly;
    // idx is boxed if the param expects union.
    let (pred2_raw, elem_boxes) = call_body_closure_with_elem_boxes(
        body, &[(elem2, ni_elem_ty.clone()), (idx2, Type::Int32)], &param_tys, &json, builder);
    let pred2 = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::Coerce { dst: pred2, src: pred2_raw, from_ty: json.clone(), to_ty: Type::Bool });
    // Release idx box if it was boxed (but NOT the element box — we may keep it).
    if elem_boxes.len() >= 2 {
        builder.emit(Instruction::Release { val: elem_boxes[1], ty: json.clone() });
    }

    // When pred=true: keep elem2 as the result (don't release it).
    // When pred=false: release elem2 (it's a fresh TaggedVal; we don't keep it).
    let ni_merge2 = builder.alloc_block("nifind_merge");
    let ni_keep2 = builder.alloc_block("nifind_keep");
    let ni_skip2 = builder.alloc_block("nifind_skip");
    builder.terminate(Terminator::CondJump { cond: pred2, then_block: ni_keep2, else_block: ni_skip2 });

    // pred=true: elem2 is the found element (owned TaggedVal +1). Don't release it.
    builder.switch_to(ni_keep2);
    let ni_keep2_end = builder.current_block;
    builder.terminate(Terminator::Jump(ni_merge2));

    // pred=false: release the materialized element (inner + shell via lin_tagged_release).
    builder.switch_to(ni_skip2);
    builder.emit(Instruction::Release { val: elem2, ty: ni_elem_ty.clone() });
    let ni_skip2_end = builder.current_block;
    builder.terminate(Terminator::Jump(ni_merge2));

    builder.switch_to(ni_merge2);
    // result2_next = pred ? elem2 : result2 (previous)
    builder.emit(Instruction::Phi {
        dst: result2_next, ty: json.clone(),
        incomings: vec![(elem2, ni_keep2_end), (result2, ni_skip2_end)],
    });
    builder.emit(Instruction::Binary {
        dst: found2_next, op: BinOp::Or, lhs: found2, rhs: pred2,
        operand_ty: Type::Bool, ty: Type::Bool,
    });
    builder.terminate(Terminator::Jump(latch2));

    builder.switch_to(latch2);
    let one2 = builder.const_temp(Const::Int(1, Type::Int64));
    builder.emit(Instruction::Binary {
        dst: i2_next, op: BinOp::Add, lhs: i2, rhs: one2,
        operand_ty: Type::Int64, ty: Type::Int64,
    });
    builder.terminate(Terminator::Jump(header2));

    builder.switch_to(exit2);
    if !matches!(result_type, Type::TypeVar(_)) {
        let dst = builder.alloc_temp(result_type.clone());
        builder.emit(Instruction::Coerce { dst, src: result2, from_ty: json, to_ty: result_type.clone() });
        return dst;
    }
    result2
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
pub(crate) fn free_combinator_elem_box(elem: Temp, elem_ty: &Type, builder: &mut FuncBuilder) {
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
pub(crate) fn free_combinator_elem_box_full(elem: Temp, elem_ty: &Type, builder: &mut FuncBuilder) {
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
pub(crate) fn free_combinator_sealed_elem(elem: Temp, _iterable_ty: &Type, elem_ty: &Type, builder: &mut FuncBuilder) {
    if is_sealed_scalar_repr(elem_ty) {
        builder.emit(Instruction::Release { val: elem, ty: elem_ty.clone() });
    }
}

/// True when `func` resolves to a `flatMap` combinator (a monomorphized top-level spec tagged in
/// `combinator_spec_slots`, or an imported `std_iter_flatMap` symbol) — the lone-`flatMap` dispatch
/// the fusion engine should drive directly rather than calling the eager stdlib body. Mirrors the
/// `combinator_callee_name` resolution, restricted to `flatMap` (the only generic combinator with no
/// intrinsic). A genuine intrinsic combinator never reaches here (it dispatched earlier in `lower_call`).
pub(crate) fn callee_is_flatmap(func: &TypedExpr, ctx: &LowerCtx) -> bool {
    let TypedExpr::LocalGet { slot, .. } = func else { return false };
    if ctx.combinator_spec_slots.get(slot) == Some(&"flatMap") {
        return true;
    }
    ctx.import_fn_slots
        .get(slot)
        .map(|(sym, _)| combinator_base_name(sym) == Some("flatMap"))
        .unwrap_or(false)
}

/// WAVE D — LONE `flatMap`: fuse `base.<…>.flatMap(f)` with NO downstream stage into the CPS loop
/// nest, pushing each flattened inner element straight into the result array (no eager stdlib body,
/// no per-source-element closure call). Returns `None` (caller falls back to the eager call) when the
/// `flatMap` callback is not an inlinable lambda, the receiver is a lazy `Stream`, or an element repr
/// in the chain is not fuse-reclaimable.
///
/// OWNERSHIP of the push terminal — the inner element `val` arrives from the FlatMap stage's inner
/// `Index` read at one of three reprs, each pushed + reclaimed to mirror the eager `inner.for(x =>
/// push(result, x))` exactly:
///   - SCALAR — pushed by value into a flat result (no RC); `fm_reclaim_elem` is a no-op.
///   - BORROWED HEAP (`Str`/`Array`/`Object`) — a borrowed interior pointer (no +1). `push_output`
///     with `borrowed = true` RETAINS it into the (tagged) result and self-balances the fresh box it
///     allocates; `fm_reclaim_elem` is a no-op (not union/sealed). Net: result owns +1, the borrow is
///     untouched (the inner array's release drops its own element ref). Mirrors eager `push` (retains).
///   - UNION/SEALED — a FRESH +1 box/struct. `push_output` (borrowed=false) takes the retaining
///     `lin_push_dyn` (the result owns its own ref) and does NOT release the box; `fm_reclaim_elem`
///     fully releases the original +1 box/struct. Net balanced.
pub(crate) fn lower_flatmap_terminal(
    args: &[TypedExpr],
    result_type: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Option<Temp> {
    if args.len() != 2 {
        return None;
    }
    // A lazy Stream receiver stays lazy (driven by the runtime) — never fused.
    if matches!(args[0].ty(), Type::Stream(_)) {
        return None;
    }

    // DEVIRTUALIZED FAST PATH (path-8-B generalized): bare named fn callback —
    // outer index loop calls fn directly to get the inner array, inner index loop pushes each
    // element. No closure shell, no indirect dispatch, no intermediate eager-stdlib body call.
    if let Some((target, native_params)) = bare_fn_call_target(&args[1], builder, ctx) {
        let (_, fm_ret) = callback_signature(&args[1]);
        let out_elem_ty = match result_type {
            Type::Array(t) | Type::Iterator(t) => (**t).clone(),
            // KEEP: result_type is the checker's declared return (always Array(T) for lin_flatMap, but
            // AnyVal is the correct safe fallback if for any reason it isn't — the caller decides repr).
            _ => Type::TypeVar(u32::MAX),
        };
        let inner_elem_ty = iter_elem_type(&fm_ret);
        // Only fuse when the inner element repr is reclaimable (same gate as the inline path).
        if !matches!(inner_elem_ty, Type::Never) && !fuse_elem_repr_reclaimable(&inner_elem_ty) {
            // Fallthrough to inline path or generic stdlib.
        } else {
            let source_ty = args[0].ty();
            let read_elem_ty = combinator_read_elem_ty(&args[0], builder, ctx);
            let source = lower_expr(&args[0], builder, ctx);
            let (out, flat) = alloc_output_array(&out_elem_ty, result_type, builder);
            let json = Type::TypeVar(u32::MAX);
            // Outer loop: for each element of the source, call fn(elem) -> inner[].
            emit_index_loop(source, &source_ty, &read_elem_ty, builder, ctx, |i, elem, b, _| {
                let idx = narrow_loop_index(i, b);
                let inner = call_body_direct(
                    target.clone(), &[(elem, read_elem_ty.clone()), (idx, Type::Int32)],
                    &native_params, &fm_ret, b);
                free_combinator_elem_box(elem, &read_elem_ty, b);
                free_combinator_sealed_elem(elem, &source_ty, &read_elem_ty, b);
                // Inner loop: iterate the produced inner array and push each element.
                let inner_len = emit_iterable_len(inner, &fm_ret, b);
                let zero = b.const_temp(Const::Int(0, Type::Int64));
                let j = b.alloc_temp(Type::Int64);
                let j_next = b.alloc_temp(Type::Int64);
                let inner_preheader = b.current_block;
                let inner_header = b.alloc_block("flatmap_inner_header");
                let inner_body = b.alloc_block("flatmap_inner_body");
                let inner_exit = b.alloc_block("flatmap_inner_exit");
                b.terminate(Terminator::Jump(inner_header));
                b.switch_to(inner_header);
                b.emit(Instruction::Phi {
                    dst: j, ty: Type::Int64,
                    incomings: vec![(zero, inner_preheader), (j_next, inner_body)],
                });
                let inner_cond = b.alloc_temp(Type::Bool);
                b.emit(Instruction::Binary {
                    dst: inner_cond, op: BinOp::Lt, lhs: j, rhs: inner_len,
                    operand_ty: Type::Int64, ty: Type::Bool,
                });
                b.terminate(Terminator::CondJump {
                    cond: inner_cond, then_block: inner_body, else_block: inner_exit,
                });
                b.switch_to(inner_body);
                let inner_elem = b.alloc_temp(json.clone());
                b.emit(Instruction::Index {
                    dst: inner_elem, object: inner, key: j,
                    obj_ty: fm_ret.clone(), key_ty: Type::Int64, result_ty: json.clone(),
                    nonneg: false,
                });
                let borrowed = is_borrowed_heap_elem(&inner_elem_ty);
                push_output(out, flat, &out_elem_ty, inner_elem, &json, borrowed, b);
                if !borrowed { fm_reclaim_elem(inner_elem, &json, b); }
                let one = b.const_temp(Const::Int(1, Type::Int64));
                b.emit(Instruction::Binary {
                    dst: j_next, op: BinOp::Add, lhs: j, rhs: one,
                    operand_ty: Type::Int64, ty: Type::Int64,
                });
                b.terminate(Terminator::Jump(inner_header));
                b.switch_to(inner_exit);
                // Release the inner array (we borrowed its elements; now done).
                b.emit(Instruction::Release { val: inner, ty: fm_ret.clone() });
            });
            return Some(out);
        }
    }

    let (fm_params, fm_body) = inlinable_local_fn(&args[1], builder, ctx)?;
    let fm_params = fm_params.to_vec();
    let fm_body = fm_body.clone();
    // The flatMap inner element repr must be fuse-reclaimable (or `Never`, the provably-empty
    // `x => []` inner) — same gate as `extract_fuse_chain`'s FlatMap arm.
    let (_, ret) = callback_signature(&args[1]);
    let inner_elem_ty = iter_elem_type(&ret);
    if !matches!(inner_elem_ty, Type::Never) && !fuse_elem_repr_reclaimable(&inner_elem_ty) {
        return None;
    }
    // Peel any upstream map/filter/flatMap stages off the receiver, then append THIS flatMap as the
    // final (downstream-most) stage. `extract_fuse_chain` may have cleared the upstream stages if the
    // base element repr is not reclaimable — re-check it ourselves so we never fuse over a leaky base.
    let (base, mut stages) = extract_fuse_chain(&args[0], builder, ctx);
    if !fuse_elem_repr_reclaimable(&iter_elem_type(&base.ty())) {
        return None;
    }
    stages.push(FuseStage::FlatMap { params: fm_params, body: fm_body, inner_elem_ty });
    let base_ty = base.ty();
    let read_elem_ty = combinator_read_elem_ty(base, builder, ctx);
    let out_elem_ty = match result_type {
        Type::Array(t) | Type::Iterator(t) => (**t).clone(),
        // KEEP: same defensive fallback as the bare-fn path above — the checker always produces
        // Array(T) for flatMap, but AnyVal is safe if the declared type is unexpectedly dynamic.
        _ => Type::TypeVar(u32::MAX),
    };
    let (out, flat) = alloc_output_array(&out_elem_ty, result_type, builder);
    let iterable = lower_expr(base, builder, ctx);
    // The push terminal reads no output index → terminal_param_count = 1 (no counter cell allocated).
    emit_flatmap_fused_loop(iterable, &base_ty, &read_elem_ty, &stages, 1, builder, ctx,
        |val, val_ty, _idx, b, _c| {
            let borrowed = is_borrowed_heap_elem(&val_ty);
            push_output(out, flat, &out_elem_ty, val, &val_ty, borrowed, b);
            // Reclaim the survivor: a no-op for a scalar / borrowed-heap (push_output self-balanced
            // those), a full release for a fresh +1 union/sealed box (the result took its own ref).
            fm_reclaim_elem(val, &val_ty, b);
        });
    Some(out)
}

/// `map(iterable, f)` → new array of `f(elem)` for each element.
pub(crate) fn lower_map(args: &[TypedExpr], result_type: &Type, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    let iterable_ty = args[0].ty();
    let (param_tys, cb_ret) = callback_signature(&args[1]);

    // Output element type per the map's declared result type; storage matches it.
    let out_elem_ty = match result_type {
        Type::Array(t) | Type::Iterator(t) => (**t).clone(),
        // KEEP: lin_map always returns Array(U) per the checker; AnyVal fallback is defensive for
        // any future call site where result_type is unexpectedly a stream/union/dynamic type.
        _ => Type::TypeVar(u32::MAX),
    };
    // Read at the source's PROVABLE representation: a flat scalar only for a provably-flat producer
    // (range/map/filter result, flat literal), else the tagged Json read — sound for a `[]`+push
    // array mistyped as a flat `T[]` (ADR-044).
    let elem_ty = combinator_read_elem_ty(&args[0], builder, ctx);

    // FUSED CHAIN (path-6 6a, Step 8.1 array-producing terminal): base.map/filter chain into THIS
    // map's loop, building ONE result array in a single pass (no intermediate per-stage array). The
    // terminal map lambda is applied to each survivor and pushed. Requires an inlinable body lambda
    // and at least one fusible upstream stage; bails otherwise.
    // Gate to a SCALAR output element: the terminal map projects to a flat scalar array (the dominant
    // `.map(t => t.field)` shape), so each push is by value with no per-element RC and no packed
    // struct-push. A heap/sealed OUTPUT element (`map(t => t)` / `map(t => {…})` producing `Trip[]` /
    // `Object[]`) is left to the per-stage path (the packed-struct-push repr boundary is out of scope).
    if is_inline_scalar(&out_elem_ty) {
    if let Some((lam_params, lam_body)) = inlinable_local_fn(&args[1], builder, ctx) {
        let (base, stages) = extract_fuse_chain(&args[0], builder, ctx);
        if !stages.is_empty() {
            let lam_params = lam_params.to_vec();
            let lam_body = lam_body.clone();
            let base_ty = base.ty();
            let read_elem_ty = combinator_read_elem_ty(base, builder, ctx);
            let (out, flat) = alloc_output_array(&out_elem_ty, result_type, builder);
            let iterable = lower_expr(base, builder, ctx);
            // WAVE D: flatMap-bearing chain → CPS loop nest; the terminal map runs per flattened element.
            if chain_has_flatmap(&stages) {
                emit_flatmap_fused_loop(iterable, &base_ty, &read_elem_ty, &stages, lam_params.len(),
                    builder, ctx, |val, val_ty, idx, b, c| {
                        let (mapped, mapped_ty) =
                            inline_lambda_body(&lam_params, &lam_body, &[(val, val_ty.clone()), (idx, Type::Int32)], b, c);
                        push_output(out, flat, &out_elem_ty, mapped, &mapped_ty, false, b);
                        // The terminal map consumed the survivor `val` to produce a fresh scalar
                        // `mapped` (gated scalar output ⇒ never an alias) — reclaim `val`.
                        if mapped != val {
                            fm_reclaim_elem(val, &val_ty, b);
                        }
                    });
                return out;
            }
            emit_fused_loop(iterable, &base_ty, &read_elem_ty, &stages, builder, ctx,
                |val, val_ty, idx, src_elem, src_elem_ty, b, c| {
                    let (mapped, mapped_ty) =
                        inline_lambda_body(&lam_params, &lam_body, &[(val, val_ty.clone()), (idx, Type::Int32)], b, c);
                    // `mapped` is the terminal lambda's freshly-owned result (+1) — MOVE into `out`.
                    push_output(out, flat, &out_elem_ty, mapped, &mapped_ty, false, b);
                    // Reclaim the survivor the terminal consumed (mirrors the `for` terminal): the
                    // source-element materialize when it IS the source struct, else the upstream-map
                    // fresh value — unless the terminal returned it unchanged (`mapped` aliases it).
                    if mapped != val && mapped != src_elem {
                        if val == src_elem {
                            free_combinator_elem_box_full(src_elem, src_elem_ty, b);
                            free_combinator_sealed_elem(src_elem, &base_ty, src_elem_ty, b);
                        } else {
                            b.emit(Instruction::Release { val, ty: val_ty });
                        }
                    }
                });
            return out;
        }
    }
    }

    let iterable = lower_expr(&args[0], builder, ctx);

    // DEVIRTUALIZED FAST PATH (path-8-B): a BARE statically-known fn callback (`xs.map(square)`)
    // calls it DIRECTLY per element — no heap closure shell, no boxed-ABI wrapper, no indirect
    // dispatch — so LLVM can inline the body across the call. Args are coerced to the callee's
    // native param repr by `lower_coerce_arg` (same as any direct call).
    if let Some((target, native_params)) = bare_fn_call_target(&args[1], builder, ctx) {
        let (out, flat) = alloc_output_array(&out_elem_ty, result_type, builder);
        emit_index_loop(iterable, &iterable_ty, &elem_ty, builder, ctx, |i, elem, b, _| {
            let idx = narrow_loop_index(i, b);
            let mapped = call_body_direct(
                target.clone(), &[(elem, elem_ty.clone()), (idx, Type::Int32)], &native_params, &cb_ret, b);
            push_output(out, flat, &out_elem_ty, mapped, &cb_ret, false, b);
            free_combinator_elem_box(elem, &elem_ty, b);
            free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, b);
        });
        return out;
    }

    // INLINE FAST PATH (ADR-044 + capturing-closure inline): a literal lambda — capturing OR not — is
    // spliced directly into the loop, its param bound to the element temp and its body lowered inline,
    // with no closure alloc and no per-element box/unbox/indirect call. Captured slots resolve through
    // the enclosing builder's bindings (see `inlinable_capturing_lambda`); the CFG back-edge is patched
    // latch-relative by `emit_index_loop` even when the inlined body emits its own blocks.
    if let Some((lam_params, lam_body)) = inlinable_local_fn(&args[1], builder, ctx) {
        let lam_params = lam_params.to_vec();
        let lam_body = lam_body.clone();
        let (out, flat) = alloc_output_array(&out_elem_ty, result_type, builder);
        // PATH-1 in-place packed iteration: `map(ts, x => x["a"])` over a packed sealed-scalar array
        // reads the mapped field by const-offset, NO per-element materialize. Gated to bodies that
        // use the element ONLY for scalar field reads (`x => x["a"]`, `(x,i) => x["a"]+i`); a
        // whole-value map (`x => x`, `x => {…x…}`) needs the materialized struct and falls through.
        if is_sealed_scalar_array(&iterable_ty)
            && lam_params.first().map(|p| elem_used_only_for_scalar_fields(p.slot, &lam_body)).unwrap_or(false)
        {
            let static_elem = iter_elem_type(&iterable_ty);
            emit_packed_index_loop(iterable, &iterable_ty, builder, ctx, |i, array, b, c| {
                let idx = narrow_loop_index(i, b);
                let (mapped, mapped_ty) = inline_lambda_body_packed_view(
                    &lam_params, &lam_body, array, i, &static_elem, idx, b, c);
                push_output(out, flat, &out_elem_ty, mapped, &mapped_ty, false, b);
            });
            return out;
        }
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
pub(crate) fn lower_filter(args: &[TypedExpr], result_type: &Type, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    let iterable_ty = args[0].ty();
    let (param_tys, _) = callback_signature(&args[1]);

    // filter preserves the element type; storage matches it.
    let out_elem_ty = match result_type {
        Type::Array(t) | Type::Iterator(t) => (**t).clone(),
        // KEEP: lin_filter always returns Array(T) per the checker; AnyVal fallback is defensive for
        // any future call site where result_type is unexpectedly a stream/union/dynamic type.
        _ => Type::TypeVar(u32::MAX),
    };
    // Read at the source's PROVABLE representation (ADR-044): flat scalar for a provably-flat
    // producer, else tagged Json (sound for a `[]`+push array mistyped as flat).
    let elem_ty = combinator_read_elem_ty(&args[0], builder, ctx);

    // FUSED CHAIN (path-6 6a, Step 8.1 array-producing terminal): base.map/filter chain into THIS
    // filter's loop, building ONE result array in a single pass. Gated to a SCALAR output element
    // (the survivor a flat scalar, e.g. `xs.map(t => t.field).filter(x => x > k)`): each kept element
    // pushes by value with no per-element RC. A heap/sealed survivor (a `Trip[]`-preserving filter
    // chain) is left to the per-stage path — its packed/borrowed push RC is out of scope here.
    if is_inline_scalar(&out_elem_ty) {
        if let Some((lam_params, lam_body)) = inlinable_local_fn(&args[1], builder, ctx) {
            let (base, stages) = extract_fuse_chain(&args[0], builder, ctx);
            if !stages.is_empty() {
                let lam_params = lam_params.to_vec();
                let lam_body = lam_body.clone();
                let base_ty = base.ty();
                let read_elem_ty = combinator_read_elem_ty(base, builder, ctx);
                let (out, flat) = alloc_output_array(&out_elem_ty, result_type, builder);
                let iterable = lower_expr(base, builder, ctx);
                // WAVE D: a flatMap-bearing chain → CPS loop nest; the terminal filter runs per
                // flattened element. The survivor is a flat scalar (gated `is_inline_scalar` above),
                // so a kept element pushes by value (no RC) and a dropped one is discarded — the
                // `fm_reclaim_elem` (no-op for a scalar) keeps the consume-site discipline uniform.
                if chain_has_flatmap(&stages) {
                    emit_flatmap_fused_loop(iterable, &base_ty, &read_elem_ty, &stages, lam_params.len(),
                        builder, ctx, |val, val_ty, idx, b, c| {
                            let (pred_raw, pred_ty) = inline_lambda_body(
                                &lam_params, &lam_body, &[(val, val_ty.clone()), (idx, Type::Int32)], b, c);
                            let keep = if matches!(pred_ty, Type::Bool) {
                                pred_raw
                            } else {
                                let d = b.alloc_temp(Type::Bool);
                                b.emit(Instruction::Coerce { dst: d, src: pred_raw, from_ty: pred_ty, to_ty: Type::Bool });
                                d
                            };
                            let keep_block = b.alloc_block("fm_ffilter_keep");
                            let drop_block = b.alloc_block("fm_ffilter_drop");
                            let join_block = b.alloc_block("fm_ffilter_skip");
                            b.terminate(Terminator::CondJump { cond: keep, then_block: keep_block, else_block: drop_block });
                            // KEEP: push the scalar by value (gated), then reclaim the survivor (no-op
                            // for a scalar — keeps the consume-site discipline uniform with the engine).
                            b.switch_to(keep_block);
                            push_output(out, flat, &out_elem_ty, val, &val_ty, false, b);
                            fm_reclaim_elem(val, &val_ty, b);
                            b.terminate(Terminator::Jump(join_block));
                            // DROP: reclaim the discarded survivor (no-op scalar).
                            b.switch_to(drop_block);
                            fm_reclaim_elem(val, &val_ty, b);
                            b.terminate(Terminator::Jump(join_block));
                            b.switch_to(join_block);
                        });
                    return out;
                }
                emit_fused_loop(iterable, &base_ty, &read_elem_ty, &stages, builder, ctx,
                    |val, val_ty, idx, _src_elem, _src_elem_ty, b, c| {
                        // The survivor `val` is a flat scalar (gated). Run the terminal predicate; on
                        // keep, push the scalar by value (no RC, no retain); on drop, discard. The
                        // upstream stages already reclaimed any source materialize before producing the
                        // scalar survivor, so there is nothing to free here either way.
                        let (pred_raw, pred_ty) = inline_lambda_body(
                            &lam_params, &lam_body, &[(val, val_ty.clone()), (idx, Type::Int32)], b, c);
                        let keep = if matches!(pred_ty, Type::Bool) {
                            pred_raw
                        } else {
                            let d = b.alloc_temp(Type::Bool);
                            b.emit(Instruction::Coerce { dst: d, src: pred_raw, from_ty: pred_ty, to_ty: Type::Bool });
                            d
                        };
                        let keep_block = b.alloc_block("ffilter_keep");
                        let join_block = b.alloc_block("ffilter_skip");
                        b.terminate(Terminator::CondJump { cond: keep, then_block: keep_block, else_block: join_block });
                        b.switch_to(keep_block);
                        push_output(out, flat, &out_elem_ty, val, &val_ty, false, b);
                        b.terminate(Terminator::Jump(join_block));
                        b.switch_to(join_block);
                    });
                return out;
            }
        }
    }

    let iterable = lower_expr(&args[0], builder, ctx);

    // DEVIRTUALIZED FAST PATH (path-8-B): a BARE statically-known predicate (`xs.filter(isEven)`)
    // is called DIRECTLY per element — no closure shell / boxed-ABI / indirect dispatch. The
    // predicate's native return is Bool (i1); `call_body_direct` coerces the element arg to the
    // native param repr. The keep/skip split + element-box reclaim is byte-for-byte the closure path.
    if let Some((target, native_params)) = bare_fn_call_target(&args[1], builder, ctx) {
        let (out, flat) = alloc_output_array(&out_elem_ty, result_type, builder);
        emit_index_loop(iterable, &iterable_ty, &elem_ty, builder, ctx, |i, elem, b, _| {
            let idx = narrow_loop_index(i, b);
            let keep = call_body_direct(
                target.clone(), &[(elem, elem_ty.clone()), (idx, Type::Int32)], &native_params, &Type::Bool, b);
            let keep_block = b.alloc_block("filter_keep");
            let drop_block = b.alloc_block("filter_drop");
            let join_block = b.alloc_block("filter_skip");
            b.terminate(Terminator::CondJump { cond: keep, then_block: keep_block, else_block: drop_block });
            b.switch_to(keep_block);
            push_output(out, flat, &out_elem_ty, elem, &elem_ty, true, b);
            free_combinator_elem_box(elem, &elem_ty, b);
            b.terminate(Terminator::Jump(join_block));
            b.switch_to(drop_block);
            free_combinator_elem_box_full(elem, &elem_ty, b);
            b.terminate(Terminator::Jump(join_block));
            b.switch_to(join_block);
        });
        return out;
    }

    // INLINE FAST PATH (ADR-044 + capturing-closure inline): a literal predicate lambda — capturing
    // OR not — is spliced into the loop; its body's Bool result drives the keep/skip split directly —
    // no closure, no boxed call. Captured slots resolve through the enclosing builder's bindings; the
    // keep/skip blocks the body and the predicate join emit are patched latch-relative by
    // `emit_index_loop`.
    if let Some((lam_params, lam_body)) = inlinable_local_fn(&args[1], builder, ctx) {
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

/// WAVE D: fused `base.<…flatMap…>.reduce(init, f)` over a SCALAR accumulator. A flatMap chain is a
/// loop nest, so the accumulator can't ride a single loop phi (the linear `lower_fused_reduce` model);
/// instead it lives in a heap cell carried through the CPS engine. At the terminal we load the cell,
/// fold `acc = f(acc, survivor, outIdx)`, store it back. The accumulator is a concrete scalar (the
/// caller gates `is_inline_scalar(result_type)`), so the cell store is a plain write (no RC) and the
/// cell free reclaims only the allocation. The reducer's optional 3rd param is the OUTPUT index.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_fused_reduce_flatmap(
    base: &TypedExpr,
    stages: &[FuseStage],
    reducer_params: &[TypedParam],
    reducer_body: &TypedExpr,
    init_expr: &TypedExpr,
    init_ty: &Type,
    result_type: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Temp {
    let base_ty = base.ty();
    let acc_ty = result_type.clone();
    let read_elem_ty = combinator_read_elem_ty(base, builder, ctx);
    let iterable = lower_expr(base, builder, ctx);
    let init_raw = lower_expr(init_expr, builder, ctx);
    let init = coerce_arg_to_param_repr(init_raw, init_ty, &acc_ty, builder);
    // Accumulator cell (scalar — no value RC on store/free).
    let acc_cell = builder.alloc_temp(Type::TypeVar(u32::MAX));
    builder.emit(Instruction::MakeCell { dst: acc_cell, init, ty: acc_ty.clone() });
    // The reducer's index is its THIRD param; allocate an output counter only if it's declared.
    let want_out_idx = reducer_params.len() >= 3;
    emit_flatmap_fused_loop(iterable, &base_ty, &read_elem_ty, stages,
        if want_out_idx { 2 } else { 1 }, builder, ctx,
        |sv, sv_ty, out_idx, b, c| {
            let cur = b.alloc_temp(acc_ty.clone());
            b.emit(Instruction::CellGet { dst: cur, cell: acc_cell, ty: acc_ty.clone() });
            let (acc_next_raw, acc_next_ty) = inline_lambda_body(
                reducer_params, reducer_body,
                &[(cur, acc_ty.clone()), (sv, sv_ty.clone()), (out_idx, Type::Int32)], b, c);
            let acc_next = coerce_arg_to_param_repr(acc_next_raw, &acc_next_ty, &acc_ty, b);
            b.emit(Instruction::CellSet { cell: acc_cell, value: acc_next, ty: acc_ty.clone() });
            // The reducer READ the survivor (a fold consumes by reference) — reclaim it (no-op scalar).
            fm_reclaim_elem(sv, &sv_ty, b);
        });
    // The terminal counter (position = stages.len()) was allocated INSIDE emit_flatmap_fused_loop, but
    // the accumulator cell is ours — load the final value and free the cell.
    let result = builder.alloc_temp(acc_ty.clone());
    builder.emit(Instruction::CellGet { dst: result, cell: acc_cell, ty: acc_ty.clone() });
    builder.emit(Instruction::FreeCell { cell: acc_cell, ty: acc_ty.clone() });
    result
}

/// `reduce(iterable, init, f)` → fold `acc = f(acc, elem)` over the elements.
/// The reducer `f` takes `(Json, Json)`, so the accumulator and element are carried as
/// Json (boxed); the final accumulator is coerced back to `result_type`.
/// Fused base.<map/filter...>.reduce(init, f) over a scalar accumulator (path-6 6a): a single loop
/// over the BASE source whose body reads the element, applies the transformer `stages` inline (a
/// filter skip carries the accumulator UNCHANGED to the latch; a map rebinds the carried value), then
/// folds acc = f(acc, survivor). No intermediate array per stage, no per-stage closure call. The
/// accumulator is carried UNBOXED through a value phi (gated to a scalar result_type by the caller).
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_fused_reduce(
    base: &TypedExpr,
    stages: &[FuseStage],
    reducer_params: &[TypedParam],
    reducer_body: &TypedExpr,
    init_expr: &TypedExpr,
    init_ty: &Type,
    result_type: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Temp {
    let iterable_ty = base.ty();
    let acc_ty = result_type.clone();
    let read_elem_ty = combinator_read_elem_ty(base, builder, ctx);

    let iterable = lower_expr(base, builder, ctx);
    let init_raw = lower_expr(init_expr, builder, ctx);
    let init = coerce_arg_to_param_repr(init_raw, init_ty, &acc_ty, builder);

    let len = emit_iterable_len(iterable, &iterable_ty, builder);
    let zero = builder.const_temp(Const::Int(0, Type::Int64));

    let preheader = builder.current_block;
    let header = builder.alloc_block("freduce_header");
    let body = builder.alloc_block("freduce_body");
    let latch = builder.alloc_block("freduce_latch");
    let exit = builder.alloc_block("freduce_exit");

    let i = builder.alloc_temp(Type::Int64);
    let i_next = builder.alloc_temp(Type::Int64);
    let acc = builder.alloc_temp(acc_ty.clone());
    builder.terminate(Terminator::Jump(header));

    builder.switch_to(header);
    builder.emit(Instruction::Phi {
        dst: i, ty: Type::Int64, incomings: vec![(zero, preheader), (i_next, latch)],
    });
    builder.emit(Instruction::Phi {
        dst: acc, ty: acc_ty.clone(), incomings: vec![(init, preheader), (acc, latch)],
    });
    let cond = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::Binary {
        dst: cond, op: BinOp::Lt, lhs: i, rhs: len, operand_ty: Type::Int64, ty: Type::Bool,
    });
    builder.terminate(Terminator::CondJump { cond, then_block: body, else_block: exit });

    builder.switch_to(body);
    let elem = builder.alloc_temp(read_elem_ty.clone());
    builder.emit(Instruction::Index {
        dst: elem, object: iterable, key: i,
        obj_ty: iterable_ty.clone(), key_ty: Type::Int64, result_ty: read_elem_ty.clone(),
    nonneg: false,
    });
    let idx = narrow_loop_index(i, builder);
    // Every latch predecessor contributes (acc value, predecessor): a SKIPPED element carries the
    // unchanged `acc`; a KEPT element carries the folded `acc_next`.
    let mut acc_incomings: Vec<(Temp, BlockId)> = Vec::new();
    let survivor = apply_fuse_stages_reduce(
        stages, elem, read_elem_ty.clone(), &iterable_ty, idx, acc, latch, &mut acc_incomings, builder, ctx,
    );
    if let Some((sv, sv_ty)) = survivor {
        let (acc_next_raw, acc_next_ty) = inline_lambda_body(
            reducer_params, reducer_body,
            &[(acc, acc_ty.clone()), (sv, sv_ty.clone()), (idx, Type::Int32)], builder, ctx,
        );
        let acc_next = coerce_arg_to_param_repr(acc_next_raw, &acc_next_ty, &acc_ty, builder);
        if sv == elem {
            free_combinator_elem_box_full(elem, &read_elem_ty, builder);
            free_combinator_sealed_elem(elem, &iterable_ty, &read_elem_ty, builder);
        } else {
            // A map stage produced a FRESH survivor (e.g. a projected heap value) distinct from the
            // source element. The reducer READ it (a fold consumes by reference, producing the scalar
            // acc), so nothing else owns it — release it or it leaks one per element. A scalar
            // survivor `Release` is a no-op; a heap survivor (string/object) is reclaimed.
            builder.emit(Instruction::Release { val: sv, ty: sv_ty.clone() });
        }
        let keep_latch_pred = builder.current_block;
        builder.terminate(Terminator::Jump(latch));
        acc_incomings.push((acc_next, keep_latch_pred));
    }

    builder.switch_to(latch);
    let acc_latch = builder.alloc_temp(acc_ty.clone());
    builder.emit(Instruction::Phi { dst: acc_latch, ty: acc_ty.clone(), incomings: acc_incomings });
    let one = builder.const_temp(Const::Int(1, Type::Int64));
    builder.emit(Instruction::Binary {
        dst: i_next, op: BinOp::Add, lhs: i, rhs: one, operand_ty: Type::Int64, ty: Type::Int64,
    });
    builder.terminate(Terminator::Jump(header));
    builder.patch_phi_incoming_value(header, acc, acc, acc_latch, latch);

    builder.switch_to(exit);
    acc
}

/// Like `apply_fuse_stages`, but for the FUSED REDUCE loop: a filter skip carries the unchanged
/// accumulator `acc` to the shared `latch`. Returns the surviving (value, type) on the keep path.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_fuse_stages_reduce(
    stages: &[FuseStage],
    mut elem: Temp,
    mut elem_ty: Type,
    iterable_ty: &Type,
    idx: Temp,
    acc: Temp,
    latch: BlockId,
    acc_incomings: &mut Vec<(Temp, BlockId)>,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Option<(Temp, Type)> {
    let src_elem = elem;
    let src_elem_ty = elem_ty.clone();
    // See `apply_fuse_stages`: track whether the per-iteration SOURCE materialize is still owned, so
    // a map that consumed it followed by a filter drop does not double-free it (`lin_sealed_release`).
    let mut src_alive = true;
    for stage in stages {
        match stage {
            FuseStage::Filter { params, body } => {
                let (pred_raw, pred_ty) =
                    inline_lambda_body(params, body, &[(elem, elem_ty.clone()), (idx, Type::Int32)], builder, ctx);
                let keep = if matches!(pred_ty, Type::Bool) {
                    pred_raw
                } else {
                    let d = builder.alloc_temp(Type::Bool);
                    builder.emit(Instruction::Coerce { dst: d, src: pred_raw, from_ty: pred_ty, to_ty: Type::Bool });
                    d
                };
                let keep_block = builder.alloc_block("freduce_keep");
                let drop_block = builder.alloc_block("freduce_drop");
                builder.terminate(Terminator::CondJump { cond: keep, then_block: keep_block, else_block: drop_block });
                builder.switch_to(drop_block);
                if src_alive {
                    free_combinator_elem_box_full(src_elem, &src_elem_ty, builder);
                    free_combinator_sealed_elem(src_elem, iterable_ty, &src_elem_ty, builder);
                }
                if elem != src_elem {
                    builder.emit(Instruction::Release { val: elem, ty: elem_ty.clone() });
                }
                let drop_pred = builder.current_block;
                builder.terminate(Terminator::Jump(latch));
                acc_incomings.push((acc, drop_pred));
                builder.switch_to(keep_block);
            }
            FuseStage::Map { params, body, out_elem_ty } => {
                let (mapped, mapped_ty) =
                    inline_lambda_body(params, body, &[(elem, elem_ty.clone()), (idx, Type::Int32)], builder, ctx);
                // ALIAS GUARD (see `apply_fuse_stages`): don't release the incoming value when the
                // map body returned it unchanged — ownership transfers to the carried survivor.
                let aliases_incoming = mapped == elem || mapped == src_elem;
                if !aliases_incoming {
                    if elem != src_elem {
                        builder.emit(Instruction::Release { val: elem, ty: elem_ty.clone() });
                    } else {
                        free_combinator_elem_box_full(src_elem, &src_elem_ty, builder);
                        free_combinator_sealed_elem(src_elem, iterable_ty, &src_elem_ty, builder);
                        src_alive = false;
                    }
                }
                elem = mapped;
                elem_ty = if matches!(mapped_ty, Type::TypeVar(_)) { out_elem_ty.clone() } else { mapped_ty };
            }
            // flatMap chains route to the CPS engine before reaching this linear reduce applier.
            FuseStage::FlatMap { .. } => unreachable!("flatMap chains use the CPS fusion engine"),
        }
    }
    Some((elem, elem_ty))
}

pub(crate) fn lower_reduce(args: &[TypedExpr], result_type: &Type, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
    let json = Type::TypeVar(u32::MAX);

    // FUSED CHAIN (path-6 6a): when the reducer's receiver is a map/filter chain of inlinable lambdas
    // and the accumulator is a concrete scalar, fold the transformer stages INTO the reduce loop.
    // Only fires with at least one fusible stage; else falls through to the single-combinator paths.
    if is_inline_scalar(result_type) {
        if let Some((lam_params, lam_body)) = inlinable_local_fn(&args[2], builder, ctx) {
            let (base, stages) = extract_fuse_chain(&args[0], builder, ctx);
            if !stages.is_empty() {
                let lam_params = lam_params.to_vec();
                let lam_body = lam_body.clone();
                let init_ty = args[1].ty();
                // WAVE D: a flatMap-bearing chain can't use the acc-PHI loop (a loop nest has no single
                // back-edge for the phi) — carry the accumulator in a heap cell through the CPS engine.
                if chain_has_flatmap(&stages) {
                    return lower_fused_reduce_flatmap(
                        base, &stages, &lam_params, &lam_body, &args[1], &init_ty, result_type, builder, ctx);
                }
                return lower_fused_reduce(base, &stages, &lam_params, &lam_body, &args[1], &init_ty, result_type, builder, ctx);
            }
        }
    }

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
        if let Some((lam_params, lam_body)) = inlinable_local_fn(&args[2], builder, ctx) {
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
            // PATH-1 in-place packed iteration: a reducer `(acc, x) => acc + x["a"]` over a packed
            // sealed-scalar array whose body uses the ELEMENT param (index 1) ONLY for scalar field
            // reads → bind that param to a borrowed (array, index) view (const-offset reads, no
            // materialize). The accumulator param (index 0) carries the unboxed scalar phi as before.
            let elem_param_only_fields = lam_params.get(1)
                .map(|p| elem_used_only_for_scalar_fields(p.slot, &lam_body)).unwrap_or(false);
            let idx = narrow_loop_index(i, builder);
            let (acc_next_raw, acc_next_ty) = if is_sealed_scalar_array(&iterable_ty) && elem_param_only_fields {
                let static_elem = iter_elem_type(&iterable_ty);
                builder.push_scope();
                if let Some(p) = lam_params.first() { builder.slots.insert(p.slot, acc); }
                if let Some(p) = lam_params.get(1) {
                    ctx.packed_elem_slots.insert(p.slot, (iterable, i, static_elem.clone()));
                }
                if let Some(p) = lam_params.get(2) {
                    let b = coerce_arg_to_param_repr(idx, &Type::Int32, &p.ty, builder);
                    builder.slots.insert(p.slot, b);
                }
                let raw = lower_expr(&lam_body, builder, ctx);
                let bty = builder.temp_types.get(&raw).cloned().unwrap_or_else(|| lam_body.ty());
                builder.pop_scope_releasing_keep(&[raw]);
                if let Some(p) = lam_params.get(1) { ctx.packed_elem_slots.remove(&p.slot); }
                (raw, bty)
            } else {
                let elem = builder.alloc_temp(elem_ty.clone());
                builder.emit(Instruction::Index {
                    dst: elem, object: iterable, key: i,
                    obj_ty: iterable_ty.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone(),
                nonneg: false,
                });
                // acc_next = <lambda body>(acc, elem, i), inlined. The reducer params are (acc, elem)
                // plus the OPTIONAL 0-based SOURCE index `i` (narrowed Int64→Int32). A 2-param
                // `(acc, x) => …` reducer ignores the surplus index arg.
                let r = inline_lambda_body(
                    &lam_params, &lam_body,
                    &[(acc, acc_ty.clone()), (elem, elem_ty.clone()), (idx, Type::Int32)], builder, ctx,
                );
                // A PACKED sealed-array source materialized a fresh +1 element struct; the reducer read
                // a copy out of it into the scalar accumulator, so release it or it leaks per iteration.
                free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, builder);
                r
            };
            let acc_next = coerce_arg_to_param_repr(acc_next_raw, &acc_next_ty, &acc_ty, builder);
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

    // HEAP-ACCUMULATOR INLINE PATH (CLOS2): a capturing/literal reducer lambda whose accumulator
    // is a raw heap type (Map, open Object, sealed record) — NOT a scalar and NOT a union/Json box.
    // Carries the accumulator as a raw pointer through the phi; inlines the body with no closure
    // alloc and no boxed-ABI indirect dispatch. RC management:
    //   - init is lowered raw (no boxing) → phi starts at init (owned +1)
    //   - body is inlined; the accumulator param binds to `acc` (borrowed)
    //   - body result `acc_next` is the new accumulator (same ptr → identity; new ptr → +1)
    //   - `ReleaseRawIfDistinct { val: acc, other: acc_next, ty: acc_ty }` releases the old
    //     accumulator ONLY when distinct from the new one (correct for both identity and
    //     non-identity reducers)
    //
    // Gate: `result_type` is a CONCRETE non-union heap type (Map or non-sealed open Object); a
    // sealed record accumulator would need a projection coerce not covered here; a union/Json
    // accumulator must use the generic boxed-phi path. The body must be inlinable.
    //
    // RAPTOR `getQueue`: `markedStops.reduce({}, (queue, stop) => ...; queue)` — the reducer
    // mutates `queue` in-place and returns it unchanged (identity). This eliminates the per-call
    // closure alloc + env alloc + per-stop indirect call that profiled at ~6% of RANGE time.
    let acc_is_raw_heap = matches!(result_type,
        Type::Map { .. } | Type::Object { .. }
    ) && !is_union_ty(result_type) && !is_inline_scalar(result_type)
        && !is_sealed_scalar_repr(result_type);
    if acc_is_raw_heap {
        if let Some((lam_params, lam_body)) = inlinable_local_fn(&args[2], builder, ctx) {
            let lam_params = lam_params.to_vec();
            let lam_body = lam_body.clone();
            let acc_ty = result_type.clone();
            let init_raw = lower_expr(&args[1], builder, ctx);
            // Coerce init to acc_ty representation (no-op when already matching).
            let init = coerce_arg_to_param_repr(init_raw, &init_ty, &acc_ty, builder);
            // The init value's ownership is transferred to the accumulator phi. Unregister it
            // from the enclosing scope so the scope-exit Release does NOT double-free it. The phi
            // carries exactly one +1: either the final accumulator is released by the caller
            // (function scope exit), or by `ReleaseRawIfDistinct` each non-identity iteration.
            builder.unregister_owned(init);
            if init != init_raw { builder.unregister_owned(init_raw); }

            let len = emit_iterable_len(iterable, &iterable_ty, builder);
            let zero = builder.const_temp(Const::Int(0, Type::Int64));
            let preheader = builder.current_block;
            let header = builder.alloc_block("hreduce_header");
            let body = builder.alloc_block("hreduce_body");
            let exit = builder.alloc_block("hreduce_exit");

            let i = builder.alloc_temp(Type::Int64);
            let i_next = builder.alloc_temp(Type::Int64);
            let acc = builder.alloc_temp(acc_ty.clone());
            builder.terminate(Terminator::Jump(header));

            builder.switch_to(header);
            builder.emit(Instruction::Phi {
                dst: i, ty: Type::Int64, incomings: vec![(zero, preheader), (i_next, body)],
            });
            // Accumulator phi: carries raw heap pointer; back-edge filled after body is lowered.
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
                nonneg: false,
            });
            let idx = narrow_loop_index(i, builder);
            // Inline body: (acc, elem, idx) → acc_next_raw. The body may switch blocks.
            let (acc_next_raw, acc_next_ty) = inline_lambda_body(
                &lam_params, &lam_body,
                &[(acc, acc_ty.clone()), (elem, elem_ty.clone()), (idx, Type::Int32)],
                builder, ctx,
            );
            let acc_next = coerce_arg_to_param_repr(acc_next_raw, &acc_next_ty, &acc_ty, builder);
            // Release per-iteration element: union/Json elements are fully released; sealed-struct
            // elements from a packed source get their materialized-copy released. No-op for concrete
            // raw elements that are borrowed refs (e.g. a concrete Map* from a typed array).
            free_combinator_elem_box_full(elem, &elem_ty, builder);
            free_combinator_sealed_elem(elem, &iterable_ty, &elem_ty, builder);
            // Release the OLD accumulator raw ptr only when the body returned a NEW one. For an
            // identity reducer (body returns acc unchanged) this is a no-op. For a non-identity
            // reducer, this frees the old accumulator via the correct type-dispatched release fn.
            builder.emit(Instruction::ReleaseRawIfDistinct { val: acc, other: acc_next, ty: acc_ty.clone() });
            let one = builder.const_temp(Const::Int(1, Type::Int64));
            builder.emit(Instruction::Binary {
                dst: i_next, op: BinOp::Add, lhs: i, rhs: one, operand_ty: Type::Int64, ty: Type::Int64,
            });
            // Patch back-edges: i and acc.
            let back_block = builder.current_block;
            builder.terminate(Terminator::Jump(header));
            builder.patch_phi_incoming(header, i, body, back_block);
            builder.patch_phi_incoming_value(header, acc, acc, acc_next, back_block);

            builder.switch_to(exit);
            return acc;
        }
    }

    // DEVIRTUALIZED FAST PATH (path-8-B generalized): a BARE statically-known reducer
    // (`xs.reduce(init, add)`) calls it DIRECTLY per element — no closure shell, no boxed-ABI
    // wrapper, no indirect dispatch — so LLVM can inline the body. The accumulator phi uses the
    // declared `result_type` when it is a scalar (unboxed); otherwise falls back to Json like the
    // generic path below (a heap/union accumulator must ride a uniform boxed pointer through the phi).
    if let Some((target, native_params)) = bare_fn_call_target(&args[2], builder, ctx) {
        let native_ret = match args[2].ty() {
            Type::Function { ret, .. } => *ret,
            _ => result_type.clone(),
        };
        let acc_ty = if is_inline_scalar(result_type) { result_type.clone() } else { json.clone() };
        let init_raw = lower_expr(&args[1], builder, ctx);
        let init = coerce_arg_to_param_repr(init_raw, &init_ty, &acc_ty, builder);
        let read_elem_ty = if is_inline_scalar(&elem_ty) { elem_ty.clone() } else { json.clone() };

        let len = emit_iterable_len(iterable, &iterable_ty, builder);
        let zero = builder.const_temp(Const::Int(0, Type::Int64));
        let preheader = builder.current_block;
        let header = builder.alloc_block("reduce_header");
        let body_blk = builder.alloc_block("reduce_body");
        let exit = builder.alloc_block("reduce_exit");
        let i = builder.alloc_temp(Type::Int64);
        let i_next = builder.alloc_temp(Type::Int64);
        let acc = builder.alloc_temp(acc_ty.clone());
        let acc_next = builder.alloc_temp(acc_ty.clone());
        builder.terminate(Terminator::Jump(header));
        builder.switch_to(header);
        builder.emit(Instruction::Phi {
            dst: i, ty: Type::Int64, incomings: vec![(zero, preheader), (i_next, body_blk)],
        });
        builder.emit(Instruction::Phi {
            dst: acc, ty: acc_ty.clone(), incomings: vec![(init, preheader), (acc_next, body_blk)],
        });
        let cond = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Binary {
            dst: cond, op: BinOp::Lt, lhs: i, rhs: len, operand_ty: Type::Int64, ty: Type::Bool,
        });
        builder.terminate(Terminator::CondJump { cond, then_block: body_blk, else_block: exit });
        builder.switch_to(body_blk);
        let elem = builder.alloc_temp(read_elem_ty.clone());
        builder.emit(Instruction::Index {
            dst: elem, object: iterable, key: i,
            obj_ty: iterable_ty.clone(), key_ty: Type::Int64, result_ty: read_elem_ty.clone(),
            nonneg: false,
        });
        let idx = narrow_loop_index(i, builder);
        // Direct call: pass (acc, elem, idx) coerced to callee's native params. call_body_direct
        // truncates to the declared arity so a 2-param reducer ignores the index arg.
        let new_acc = call_body_direct(
            target, &[(acc, acc_ty.clone()), (elem, read_elem_ty.clone()), (idx, Type::Int32)],
            &native_params, &native_ret, builder);
        // Coerce the direct-call result back to the phi's accumulator type.
        let new_acc_coerced = coerce_arg_to_param_repr(new_acc, &native_ret, &acc_ty, builder);
        builder.emit(Instruction::ReleaseIfDistinct { val: elem, other: new_acc_coerced });
        let one = builder.const_temp(Const::Int(1, Type::Int64));
        builder.emit(Instruction::Binary {
            dst: i_next, op: BinOp::Add, lhs: i, rhs: one, operand_ty: Type::Int64, ty: Type::Int64,
        });
        let back_block = builder.current_block;
        builder.terminate(Terminator::Jump(header));
        builder.patch_phi_incoming(header, i, body_blk, back_block);
        builder.patch_phi_incoming_value(header, acc, acc, new_acc_coerced, back_block);
        builder.switch_to(exit);
        let result = if is_union_ty(result_type) || acc_ty == *result_type {
            acc
        } else {
            let out = builder.alloc_temp(result_type.clone());
            builder.emit(Instruction::Coerce {
                dst: out, src: acc, from_ty: acc_ty, to_ty: result_type.clone(),
            });
            out
        };
        return result;
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
    // Read the element at the TAGGED (Json) representation, not its concrete static type. The
    // per-iteration `ReleaseIfDistinct` below relies on the element being a freshly-OWNED +1 box
    // (the `lin_array_get_tagged` contract). A concrete heap element type (e.g. a monomorphized
    // `String` from a generic `reduce<T>`) would instead lower the Index to the BORROWED
    // `lin_array_get`, so releasing it each iteration over-releases the array's own reference and
    // frees live elements (a non-mutating reducer then returns a dangling pointer → blank/garbage).
    // A flat scalar element keeps its concrete read (no owned box to balance); only heap/union
    // elements are forced tagged here.
    let read_elem_ty = if is_inline_scalar(&elem_ty) { elem_ty.clone() } else { json.clone() };
    let elem = builder.alloc_temp(read_elem_ty.clone());
    builder.emit(Instruction::Index {
        dst: elem, object: iterable, key: i,
        obj_ty: iterable_ty.clone(), key_ty: Type::Int64, result_ty: read_elem_ty.clone(),
    nonneg: false,
    });
    // acc_next = f(acc, elem[, i]). acc is carried as Json; coerce both args to the reducer's
    // declared param types. A 3-param reducer `(acc, item, i) => …` also receives the OPTIONAL
    // 0-based SOURCE index (narrowed Int64→Int32); a 2-param reducer omits it (its closure wrapper
    // has only two parameters, so passing a third would be ABI UB).
    let acc_arg = coerce_arg_to_param(acc, &json, param_tys.first(), builder);
    let elem_arg = coerce_arg_to_param(elem, &read_elem_ty, param_tys.get(1), builder);
    let mut call_args = vec![acc_arg, elem_arg];
    if param_tys.len() >= 3 {
        let idx = narrow_loop_index(i, builder);
        let idx_arg = coerce_arg_to_param(idx, &Type::Int32, param_tys.get(2), builder);
        call_args.push(idx_arg);
    }
    builder.emit(Instruction::Call {
        dst: acc_next, callee: CallTarget::Indirect(f), args: call_args, ret_ty: json.clone(),
    });
    // FULLY reclaim the per-iteration element box (inner + shell): `lin_array_get_tagged` returned it
    // as a fresh +1 with its inner heap payload retained, and the reducer BORROWS it (its return
    // `acc_next` is its own freshly-owned +1), so the element box is dropped each iteration — release
    // it or every heap-bearing element's inner leaks (the boxed-reduce analogue of the `for` leak). The
    // `if distinct` guard covers a reducer that returns the element verbatim (`(acc, x) => x`), where
    // `acc_next` would alias the element box and the phi/exit release already owns it. A flat-scalar
    // element box has no inner, so this degrades to a shell free.
    builder.emit(Instruction::ReleaseIfDistinct { val: elem, other: acc_next });
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
pub(crate) fn lower_sort(args: &[TypedExpr], result_type: &Type, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
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
        nonneg: false,
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
    builder.emit(Instruction::Index { dst: a_val, object: out, key: mi, obj_ty: result_type.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone() , nonneg: false});
    let b_val = builder.alloc_temp(elem_ty.clone());
    builder.emit(Instruction::Index { dst: b_val, object: out, key: mj, obj_ty: result_type.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone() , nonneg: false});
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
    builder.emit(Instruction::Index { dst: lv, object: out, key: mi, obj_ty: result_type.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone() , nonneg: false});
    builder.emit(Instruction::IndexSet { object: work, key: mk, value: lv, obj_ty: result_type.clone(), key_ty: Type::Int64, val_ty: elem_ty.clone() });
    let mi_inc = builder.alloc_temp(Type::Int64);
    builder.emit(Instruction::Binary { dst: mi_inc, op: BinOp::Add, lhs: mi, rhs: one, operand_ty: Type::Int64, ty: Type::Int64 });
    builder.terminate(Terminator::Jump(m_advance));
    let take_l_block = m_take_l;

    // m_take_r: work[k] = out[j]; j += 1
    builder.switch_to(m_take_r);
    let rv = builder.alloc_temp(elem_ty.clone());
    builder.emit(Instruction::Index { dst: rv, object: out, key: mj, obj_ty: result_type.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone() , nonneg: false});
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
        builder.emit(Instruction::Index { dst: wv, object: work, key: i, obj_ty: result_type.clone(), key_ty: Type::Int64, result_ty: elem_ty.clone() , nonneg: false});
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

// ===========================================================================================
// ENTRIES INLINE — `obj.entries(f)` over a typed `{ K: V }` map (Type::Map receiver)
// ===========================================================================================
//
// When `std_object_entries` is called with 2 args (obj, f) and:
//   (a) `obj` is a `Type::Map` receiver (a raw `LinMap*` in IR), AND
//   (b) `f` is an inlinable capturing lambda
//
// …we bypass the stdlib body (which materializes a full entries array via `lin_entries_any`)
// and emit a direct slot-walk loop instead:
//
//   len = lin_map_raw_len(map)
//   for i in 0..len:
//       key_box = lin_map_raw_key_at(map, i)   // owned TaggedVal* (+1)
//       val_box = lin_map_raw_value_at(map, i)  // owned TaggedVal* (+1)
//       pair    = [key_box, val_box]             // pair array; MakeArray MOVES key+val in
//       body result = inline f(pair)             // lambda body runs inline, no closure alloc
//       Release(result)                          // discard body result
//       Release(pair as TypeVar(MAX))            // pair release walks + releases key/val inside
//   return Null
//
// RC contract:
//   - `lin_map_raw_key_at` / `lin_map_raw_value_at` return fresh OWNED TaggedVal* boxes (+1).
//   - `MakeArray` copies tag+payload into the array slots via `lin_array_push_tagged` (no retain);
//     ownership moves into the array. Do NOT release key_box/val_box separately.
//   - `Coerce(pair, Array(TypeVar(MAX)) → TypeVar(MAX))` → `lin_box_array(pair)` → `pair_box`
//     (TaggedVal*(TAG_ARRAY) shell, no extra retain on pair's refcount).
//   - `Release(pair_box, TypeVar(MAX))` → `lin_tagged_release(pair_box)` → TAG_ARRAY →
//     `lin_array_release(pair)` → rc=0 → free pair buffer, walk elements, release key+val.
//   - `pair_box` and `pair` are LOCAL temporaries NOT registered scope-owned; released inline in
//     the loop body before the back-edge (a scope-owned release would leak every iteration).

/// Inline lowering for `std_object_entries(map, callback)` when `map` is a `Type::Map` receiver
/// and `callback` is an inlinable capturing lambda. Falls through (`None`) otherwise.
pub(crate) fn lower_entries_inline(
    args: &[TypedExpr],
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Option<Temp> {
    // Gate: 2-arg call, Map receiver, inlinable callback OR bare named fn.
    if args.len() != 2 {
        return None;
    }
    if !matches!(args[0].ty(), Type::Map { .. }) {
        return None;
    }

    // DEVIRTUALIZED FAST PATH (path-8-B generalized): bare named fn callback — build the pair
    // array exactly like the inline path, then call the fn directly (direct/named call, not a
    // closure shell or indirect dispatch).
    if let Some((target, native_params)) = bare_fn_call_target(&args[1], builder, ctx) {
        let fn_ret_ty = match args[1].ty() {
            Type::Function { ret, .. } => *ret,
            _ => Type::Null,
        };
        let map = lower_expr(&args[0], builder, ctx);
        let json = Type::TypeVar(u32::MAX);
        let len = builder.alloc_temp(Type::Int64);
        builder.emit(Instruction::Call {
            dst: len,
            callee: CallTarget::Named("lin_map_raw_len".to_string()),
            args: vec![map],
            ret_ty: Type::Int64,
        });
        let zero = builder.const_temp(Const::Int(0, Type::Int64));
        let preheader = builder.current_block;
        let header = builder.alloc_block("entries_header");
        let body_blk = builder.alloc_block("entries_body");
        let latch = builder.alloc_block("entries_latch");
        let exit = builder.alloc_block("entries_exit");
        let i = builder.alloc_temp(Type::Int64);
        let i_next = builder.alloc_temp(Type::Int64);
        builder.terminate(Terminator::Jump(header));
        builder.switch_to(header);
        builder.emit(Instruction::Phi {
            dst: i, ty: Type::Int64,
            incomings: vec![(zero, preheader), (i_next, latch)],
        });
        let cond = builder.alloc_temp(Type::Bool);
        builder.emit(Instruction::Binary {
            dst: cond, op: BinOp::Lt, lhs: i, rhs: len,
            operand_ty: Type::Int64, ty: Type::Bool,
        });
        builder.terminate(Terminator::CondJump { cond, then_block: body_blk, else_block: exit });
        builder.switch_to(body_blk);
        let key_box = builder.alloc_temp(json.clone());
        builder.emit(Instruction::Call {
            dst: key_box, callee: CallTarget::Named("lin_map_raw_key_at".to_string()),
            args: vec![map, i], ret_ty: json.clone(),
        });
        let val_box = builder.alloc_temp(json.clone());
        builder.emit(Instruction::Call {
            dst: val_box, callee: CallTarget::Named("lin_map_raw_value_at".to_string()),
            args: vec![map, i], ret_ty: json.clone(),
        });
        let pair_ty = Type::Array(Box::new(json.clone()));
        let pair = builder.alloc_temp(pair_ty.clone());
        builder.emit(Instruction::MakeArray {
            dst: pair, elements: vec![key_box, val_box], spreads: vec![],
            elem_ty: json.clone(), inline: false, columnar: false,
        });
        let pair_box = builder.alloc_temp(json.clone());
        builder.emit(Instruction::Coerce {
            dst: pair_box, src: pair, from_ty: pair_ty, to_ty: json.clone(),
        });
        // Direct call to fn(pair_box): same pair_box ownership as the inline path.
        let res = call_body_direct(
            target, &[(pair_box, json.clone())], &native_params, &fn_ret_ty, builder);
        builder.emit(Instruction::Release { val: res, ty: fn_ret_ty });
        builder.emit(Instruction::Release { val: pair_box, ty: json.clone() });
        if !builder.is_current_block_terminated() {
            builder.terminate(Terminator::Jump(latch));
        }
        builder.switch_to(latch);
        let one = builder.const_temp(Const::Int(1, Type::Int64));
        builder.emit(Instruction::Binary {
            dst: i_next, op: BinOp::Add, lhs: i, rhs: one,
            operand_ty: Type::Int64, ty: Type::Int64,
        });
        builder.terminate(Terminator::Jump(header));
        builder.switch_to(exit);
        return Some(builder.const_temp(Const::Null));
    }

    let lam = inlinable_local_fn(&args[1], builder, ctx)?;
    let lam_params = lam.0.to_vec();
    let lam_body = lam.1.clone();

    let map = lower_expr(&args[0], builder, ctx);
    let json = Type::TypeVar(u32::MAX);

    // len = lin_map_raw_len(map)
    let len = builder.alloc_temp(Type::Int64);
    builder.emit(Instruction::Call {
        dst: len,
        callee: CallTarget::Named("lin_map_raw_len".to_string()),
        args: vec![map],
        ret_ty: Type::Int64,
    });

    let zero = builder.const_temp(Const::Int(0, Type::Int64));
    let preheader = builder.current_block;
    let header = builder.alloc_block("entries_header");
    let body_blk = builder.alloc_block("entries_body");
    let latch = builder.alloc_block("entries_latch");
    let exit = builder.alloc_block("entries_exit");

    let i = builder.alloc_temp(Type::Int64);
    let i_next = builder.alloc_temp(Type::Int64);
    builder.terminate(Terminator::Jump(header));

    builder.switch_to(header);
    builder.emit(Instruction::Phi {
        dst: i,
        ty: Type::Int64,
        incomings: vec![(zero, preheader), (i_next, latch)],
    });
    let cond = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::Binary {
        dst: cond,
        op: BinOp::Lt,
        lhs: i,
        rhs: len,
        operand_ty: Type::Int64,
        ty: Type::Bool,
    });
    builder.terminate(Terminator::CondJump { cond, then_block: body_blk, else_block: exit });

    builder.switch_to(body_blk);

    // key_box = lin_map_raw_key_at(map, i)   owned TaggedVal*
    let key_box = builder.alloc_temp(json.clone());
    builder.emit(Instruction::Call {
        dst: key_box,
        callee: CallTarget::Named("lin_map_raw_key_at".to_string()),
        args: vec![map, i],
        ret_ty: json.clone(),
    });
    // val_box = lin_map_raw_value_at(map, i)  owned TaggedVal*
    let val_box = builder.alloc_temp(json.clone());
    builder.emit(Instruction::Call {
        dst: val_box,
        callee: CallTarget::Named("lin_map_raw_value_at".to_string()),
        args: vec![map, i],
        ret_ty: json.clone(),
    });

    // pair = [key_box, val_box] — MakeArray emits lin_array_alloc + lin_array_push_tagged for each
    // element. MakeArray MOVES ownership: the elements' payloads transfer into the array slots
    // (no retain); do NOT release key_box/val_box separately.
    let pair_ty = Type::Array(Box::new(json.clone()));
    let pair = builder.alloc_temp(pair_ty.clone());
    builder.emit(Instruction::MakeArray {
        dst: pair,
        elements: vec![key_box, val_box],
        spreads: vec![],
        elem_ty: json.clone(),
        inline: false,
        columnar: false,
    });

    // Box pair (LinArray*) into a TaggedVal* shell so inline_lambda_body can pass it to the
    // lambda param as AnyVal (TypeVar(MAX)) without a second boxing coerce happening inside
    // coerce_arg_to_param_repr — that inner coerce would emit a SECOND lin_box_array on the
    // raw LinArray* pointer, which lin_unbox_ptr would then correctly strip… but the ORIGINAL
    // LinArray* would be double-owned and the lambda param's Index would still route through
    // the boxed pointer correctly. More importantly: passing the RAW LinArray* with type
    // TypeVar(MAX) misleads coerce_arg_to_param_repr into skipping the coerce (both sides
    // appear to have union repr TypeVar), so the body's Index sees a raw LinArray* and calls
    // lin_unbox_ptr on it → misaligned read → crash (observed). Box explicitly here so the
    // LLVM sees: lin_box_array(pair) → pair_box (TaggedVal*(TAG_ARRAY)), then pass pair_box
    // to the body. After the body, lin_tagged_release(pair_box) → reads TAG_ARRAY →
    // lin_array_release(pair) → dec pair rc to 0 → free pair + walk elements (key/val reclaimed).
    let pair_box = builder.alloc_temp(json.clone());
    builder.emit(Instruction::Coerce {
        dst: pair_box,
        src: pair,
        from_ty: pair_ty.clone(),
        to_ty: json.clone(),
    });

    // Inline the lambda body with `pair_box` (TaggedVal*) as the argument.
    // The body accesses pair_box[0]/pair_box[1] via Index { obj_ty: TypeVar(MAX) }:
    //   lin_unbox_ptr(pair_box) → pair (LinArray*) → lin_array_get_tagged(pair, 0) ✓
    let (res, res_ty) = inline_lambda_body(
        &lam_params,
        &lam_body,
        &[(pair_box, json.clone())],
        builder,
        ctx,
    );
    // `entries` callback is side-effecting; discard the body result.
    builder.emit(Instruction::Release { val: res, ty: res_ty });
    // Release the pair box: lin_tagged_release(pair_box) → TAG_ARRAY → lin_array_release(pair)
    // → rc=0 → free pair + walk elements → release key/val payloads inside.
    builder.emit(Instruction::Release { val: pair_box, ty: json.clone() });

    // Body may have switched blocks; jump to the latch from wherever we are.
    if !builder.is_current_block_terminated() {
        builder.terminate(Terminator::Jump(latch));
    }

    builder.switch_to(latch);
    let one = builder.const_temp(Const::Int(1, Type::Int64));
    builder.emit(Instruction::Binary {
        dst: i_next,
        op: BinOp::Add,
        lhs: i,
        rhs: one,
        operand_ty: Type::Int64,
        ty: Type::Int64,
    });
    builder.terminate(Terminator::Jump(header));

    builder.switch_to(exit);
    Some(builder.const_temp(Const::Null))
}

/// `min(a, b)` over two Int64 temps via a select-style CondJump+phi. Used by `lower_sort` for the
/// run-boundary clamps (`min(lo+width, n)`).
pub(crate) fn emit_min_i64(a: Temp, b: Temp, builder: &mut FuncBuilder) -> Temp {
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

