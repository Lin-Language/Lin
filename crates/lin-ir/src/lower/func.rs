use super::*;

// -------------------------------------------------------------------------
// Default-argument adapters
// -------------------------------------------------------------------------

/// If `params` carry any defaults, pre-assign a FuncId + symbol for each shortfall arity
/// `required ..total` and queue an `AdapterSpec` to be lowered after the main pass. `real_fid`
/// is the real function's id; `real_slot` is its binding slot (so the adapter body can issue a
/// `Direct` call through `global_fn_slots`). Returns immediately if there are no defaults.
pub(crate) fn register_default_adapters(
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
        lset: lin_check::types::LambdaSet::Top,
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
            && !is_nullable_sealed_record(param_ty) // NullableRecord is a nullable ptr; null inhabits it
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
pub(crate) fn lower_adapter(spec: &AdapterSpec, ctx: &mut LowerCtx) {
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
pub(crate) fn lower_replacements(replacements: &[Replacement], ctx: &mut LowerCtx) {
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

pub(crate) fn lower_function_expr(
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
pub(crate) fn lower_callback_arg(
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
pub(crate) fn lower_function_expr_with_id(
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
        current_span: None,
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
        scope_box_shells: Vec::new(),
        diverged_blocks: std::collections::HashSet::new(),
        cell_slots: HashMap::new(),
        created_cells: Vec::new(),
        escaping_cells: std::collections::HashSet::new(),
        escape_alias: HashMap::new(),
        nonneg_range_ivs: std::collections::HashSet::new(),
        local_fn_exprs: HashMap::new(),
    };

    // Add entry block. Tag it with the function body's span so coverage records a
    // region covering the whole function body (the most important coverage region).
    inner_builder.blocks.push(BasicBlock {
        id: BlockId(0),
        label: Some("entry".into()),
        instructions: Vec::new(),
        terminator: Terminator::Unreachable,
        span: Some(body.span()),
        instr_spans: Vec::new(),
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
    // DEBUG (Phase 3): record each parameter's source name + type as a DWARF formal-parameter, so
    // a `--debug` build shows function params by name in the debugger. Purely additive metadata
    // (see `Instruction::DebugDeclare`): it emits no machine code and is ignored by non-debug
    // codegen. Use the function body span for the declared line (params have no own span). Skip
    // the implicit closure env pointer (it has no source name). Captured `var`s become cell
    // pointers and are intentionally NOT declared (their logical value is behind a deref).
    for (i, param) in params.iter().enumerate() {
        if let Some(&t) = inner_builder.slots.get(&param.slot) {
            inner_builder.emit(Instruction::DebugDeclare {
                temp: t,
                name: param.name.clone(),
                ty: param.ty.clone(),
                // 1-based parameter ordinal (DWARF `arg:` index). Each must be distinct.
                param_no: Some((i + 1) as u32),
                span: body.span(),
            });
        }
    }
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
    // Make `packed_elem_slots` function-scoped while lowering THIS body. A packed-array-element
    // VIEW recorded in an OUTER function (`val r = arr[i]`) carries (array,index) TEMPS valid only
    // there; a nested closure must NOT re-emit `SealedArrayFieldGet` off them (codegen → `ptr null`
    // → arith panic). Hide them during body lowering — captured uses fall to the generic FieldGet
    // on the env value — and restore before the MakeClosure capture-reads (below), which run in the
    // outer builder and DO need the view to materialize a captured element by value.
    let saved_packed_elem_slots = std::mem::take(&mut ctx.packed_elem_slots);
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
    let ret_coerced;
    let ret_temp = if !inner_builder.is_current_block_terminated()
        && (type_repr_differs(&body_ty, &effective_ret)
            || scalar_numeric_repr_differs(&body_ty, &effective_ret)
            || flat_scalar_array_repr_differs(&body_ty, &effective_ret)
            || anon_object_slot_repr_differs(&body_ty, &effective_ret))
    {
        let dst = inner_builder.alloc_temp(effective_ret.clone());
        inner_builder.emit(Instruction::Coerce {
            dst, src: raw_ret, from_ty: body_ty.clone(), to_ty: effective_ret.clone(),
        });
        ret_coerced = true;
        dst
    } else {
        ret_coerced = false;
        raw_ret
    };
    // `raw_ret` is normally KEPT alongside `ret_temp`: when the return coercion BOXES a concrete
    // heap value into a union/Json (`lin_box_object`/`_array`/`_str`/…), the box borrows
    // `raw_ret`'s pointer WITHOUT bumping its rc, so releasing `raw_ret` would free what the
    // returned box wraps. The ONE case where keeping `raw_ret` is wrong is the REVERSE edge: an
    // UNBOX of a union/Json body to a SCALAR (non-rc) result — e.g. a `Json` body `j[0]["x"]`
    // returned as the declared `Int32`. There codegen reads the scalar value out of the box; the
    // returned `ret_temp` is a plain scalar that owns NO heap pointer, so the box `raw_ret` is a
    // fresh +1 `TaggedVal` that nothing else references. Keeping it ORPHANS it (a 16-byte per-call
    // leak through the dynamic field/index read); releasing it frees only the box shell (plus its
    // cached/scalar inner, which is a no-op) — safe, no double-free.
    //
    // CRUCIALLY this excludes unboxing to a CONCRETE HEAP type (Object/Array/String/Map): there
    // `ret_temp` is the box's INNER pointer (the returned value), so releasing `raw_ret` would drop
    // that pointer's rc and free the value being returned (a use-after-free / double-free). So the
    // exclusion is gated on the result being NON-rc. Every other coercion (box, numeric width,
    // flat-array widen, unbox-to-heap) keeps `raw_ret` exactly as before.
    let unboxes_to_scalar = ret_coerced
        && is_union_ty(&body_ty)
        && !is_union_ty(&effective_ret)
        && !is_rc_type(&effective_ret);
    // UNBOX-TO-HEAP shell leak fix: when a union/Named body is UNBOXED to a concrete HEAP type
    // (Object/Array/String/Map), `ret_temp` holds the inner LinMap*/LinArray*/etc. pointer (the
    // actual return value), and `raw_ret` is the 16-byte TaggedVal* shell whose inner IS `ret_temp`.
    // The scope-exit Release for `raw_ret` would call `lin_tagged_release` — decrementing the inner's
    // rc (UAF/double-free since `ret_temp` also holds that reference) and then freeing the shell.
    // We must NOT release `raw_ret` via scope-exit; instead, emit `FreeBoxShell` after scope pop to
    // free ONLY the 16-byte TaggedVal shell (not the inner). The `return_keep` set includes `raw_ret`
    // (preventing scope-exit Release), and the FreeBoxShell is emitted just before `Return`.
    //
    // Root cause: `coerce_if_branch` boxes both branches to a union-typed phi (because
    // `Named("Cursor")` is treated as union-ish by `is_union_ty`). The unbox coercion at the return
    // site (`Named("Cursor") → Object{...}`) then calls `lin_unbox_ptr`, leaving the shell owned but
    // unfreed. Without this fix, the shell leaked one 16-byte TaggedVal per call to every function
    // whose `Named(T)` return type expands to a concrete Object/Array/String/Map.
    let unboxes_to_concrete_heap = ret_coerced
        && is_union_ty(&body_ty)
        && !is_union_ty(&effective_ret)
        && is_rc_type(&effective_ret)
        // A union body returned as a sealed-record ARRAY is NOT a simple unbox-to-inner: the return
        // coercion is `sealed_array_project_owned`, which OWNS the result (+1: retains the kept-packed
        // buffer in the kp branch, fresh +1 in the rebuild branch) rather than aliasing the box's
        // inner. So the FreeBoxShell-only path (which keeps `raw_ret` and frees just the 16-byte
        // shell, assuming `ret_temp` IS the box's inner) would UNDER-release: the inner array keeps
        // both its own ref AND project_owned's extra +1, leaking it (and the box shell's inner ref)
        // every call. This case is handled by `sealed_array_projection_from_union` below, which fully
        // releases `raw_ret` (shell + inner decrement) to balance project_owned's retain.
        && !is_sealed_scalar_array(&effective_ret);
    // SEALED PROJECTION from a concrete heap object: when a concrete unsealed `LinObject*`
    // (`raw_ret`) is PROJECTED into a fresh sealed struct (`ret_temp`), the projection produces
    // an INDEPENDENT copy — it calls `lin_object_get` for each field and retains the heap values
    // it stores. `raw_ret` is NOT aliased by `ret_temp` (unlike the box case where the box
    // borrows `raw_ret`'s inner pointer). Keeping `raw_ret` in `return_keep` prevents
    // `pop_scope_releasing_keep` from emitting a Release for it → the `LinObject*` leaks (one
    // per call). Release `raw_ret` by NOT keeping it.
    //
    // Gate: `ret_coerced` (a coercion happened) + `raw_ret` is a concrete RC object (not a union
    // box, so the box-inner-aliasing concern does not apply) + `effective_ret` is a sealed scalar
    // record (a projection, not a box). A sealed array target or a union target uses the box
    // path and must keep `raw_ret`.
    let sealed_projection_from_object = ret_coerced
        && !is_union_ty(&body_ty)
        && is_rc_type(&body_ty)
        && is_sealed_scalar_repr(&effective_ret);
    // D3b ANON OBJECT PROJECTION: same logic as sealed_projection_from_object but the result
    // is an unsealed LinObject* (not a packed sealed struct). `boxed_object_project` produces a
    // FRESH +1 LinObject with only the target fields — `raw_ret` is not aliased by `ret_temp`.
    // Must release `raw_ret` via scope-exit rather than keeping it, to avoid a per-call leak.
    let anon_object_projection_from_object = ret_coerced
        && !is_union_ty(&body_ty)
        && is_rc_type(&body_ty)
        && anon_object_slot_repr_differs(&body_ty, &effective_ret);
    // SEALED-ARRAY PROJECTION from a union box: a union/Json body (`raw_ret` is a `TaggedVal*` box)
    // returned as a declared sealed-record array (`: T[]`). The return coercion is
    // `sealed_array_project_owned` (compile_ir_coerce's `to_sealed_arr && !from_sealed_arr` +
    // `is_union_type(from)` arm), which produces a FRESH +1-OWNED packed array (`ret_temp`) — it
    // RETAINS the kept-packed buffer (kp) or rebuilds a new one. `ret_temp` therefore does NOT alias
    // `raw_ret` in the borrow sense, so `raw_ret` (the box) must be FULLY released at scope exit
    // (`lin_tagged_release`: free the shell AND decrement its inner), which balances project_owned's
    // retain. Keeping `raw_ret` (the default) leaks the box shell + an inner ref every call (an
    // RSS-linear leak, e.g. `f(): T[] => xs.flatMap(...).reduce(seed, …)` where reduce returns the
    // boxed seed). Mirrors `sealed_projection_from_object` but for a union (boxed) body + array target.
    let sealed_array_projection_from_union = ret_coerced
        && is_union_ty(&body_ty)
        && is_sealed_scalar_array(&effective_ret);
    let return_keep: Vec<Temp> = if void_ret {
        // A void (`: Null`/`Never`) function emits `Return(None)` — the body value is NOT returned.
        // Keep NOTHING, so an OWNED heap body value (now possible since a `: Null` body may be any
        // type — e.g. ending in `m[k] = v`, which evaluates to the stored heap value) is RELEASED
        // at scope exit rather than leaked. Previously a void body was always `Null` (a non-owning
        // const), so keeping it was harmless; an owned body value must be dropped here.
        vec![]
    } else if unboxes_to_scalar
        || sealed_projection_from_object
        || anon_object_projection_from_object
        || sealed_array_projection_from_union
    {
        // For these cases `raw_ret` does not alias `ret_temp`: release it via scope-exit. For
        // `sealed_array_projection_from_union`, `raw_ret` is a union box whose full release
        // (`lin_tagged_release`) frees the shell and decrements its inner, balancing the +1 that
        // `sealed_array_project_owned` added to the returned packed array.
        vec![ret_temp]
    } else {
        // Default: keep both ret_temp and raw_ret from scope-exit release.
        // For unboxes_to_concrete_heap, raw_ret is kept here (preventing scope-exit Release which
        // would incorrectly call lin_tagged_release → decrement inner + free shell). The shell is
        // freed separately via FreeBoxShell below, after scope pop.
        vec![ret_temp, raw_ret]
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
                    && !return_keep.contains(c)
            })
            .map(|(c, ty, _)| (*c, ty.clone()))
            .collect();
        for (cell, ty) in to_free {
            inner_builder.emit(Instruction::FreeCell { cell, ty, stack: false });
        }
    }
    // Release owned temps in body scope except the return value AND the raw pre-coercion
    // temp: a box (e.g. lin_box_object) shares the underlying pointer, so releasing the
    // original would free what the returned box wraps.
    inner_builder.pop_scope_releasing_keep(&return_keep); // body scope
    // Release Function-typed params that are not being returned. This balances the
    // retain_call_arg retain emitted by every caller for each Function argument.
    inner_builder.pop_scope_releasing_keep(&return_keep); // param scope
    // UNBOX-TO-HEAP shell leak fix: `raw_ret` was kept in `return_keep` to prevent the scope-exit
    // Release from calling `lin_tagged_release` (which would decrement the inner's rc and UAF).
    // Now that the scope pops are done, free ONLY the 16-byte TaggedVal shell via FreeBoxShell.
    // This is safe: the inner pointer lives on as `ret_temp` (the return value), and FreeBoxShell
    // frees only the box struct without touching the inner payload.
    if !inner_builder.is_current_block_terminated() && unboxes_to_concrete_heap && raw_ret != ret_temp {
        inner_builder.emit(Instruction::FreeBoxShell { val: raw_ret });
    }
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
    // Restore the outer function's packed-elem views before the capture-reads below — they run in
    // the outer builder and consult `packed_elem_slots` to materialize a captured element view.
    ctx.packed_elem_slots = saved_packed_elem_slots;

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
            // A packed-array-element VIEW (`val r = arr[i]`) is VIRTUAL — no materialized slot
            // (reads re-emit a const-offset field load), so the slot lookup above MISSES. Capturing
            // it would otherwise allocate an UNINITIALIZED temp → `r` reads as null inside the
            // closure. Materialize the whole element here (the fresh +1 sealed struct the generic
            // `Index` path yields — same as expr.rs's whole-value PATH-1) and own it for scope-exit
            // release; the owning-capture logic below then Retains it into the env.
            if let Some((array, index, elem_ty)) = ctx.packed_elem_slots.get(&cap.outer_slot).cloned() {
                let mat = builder.alloc_temp(elem_ty.clone());
                builder.emit(Instruction::Index {
                    dst: mat,
                    object: array,
                    key: index,
                    obj_ty: Type::Array(Box::new(elem_ty.clone())),
                    key_ty: Type::Int64,
                    result_ty: elem_ty.clone(),
                    nonneg: false,
                    proven_inbounds: false,
                });
                builder.register_owned(mat, elem_ty.clone());
                mat
            } else {
                builder.alloc_temp(cap.ty.clone())
            }
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
        lset: lin_check::types::LambdaSet::Top,
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

pub(crate) fn lower_string_interp(
    parts: &[TypedStringPart],
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Temp {
    if parts.is_empty() {
        return builder.const_temp(Const::Str(String::new()));
    }

    // Collect all parts as string temps. Literal parts produce immortal const temps (not owned);
    // Expr parts go through ToString and produce fresh +1 owned strings.
    let mut part_temps: Vec<Temp> = Vec::with_capacity(parts.len());
    let mut owned_parts: Vec<Temp> = Vec::new(); // owned per-part temps to release after build_n

    for part in parts {
        match part {
            TypedStringPart::Literal(s) => {
                part_temps.push(builder.const_temp(Const::Str(s.clone())));
            }
            TypedStringPart::Expr(expr) => {
                let val = lower_expr(expr, builder, ctx);
                // ToString returns OWNED (+1) for every input type.
                let dst = builder.alloc_temp(Type::Str);
                builder.emit(Instruction::CallIntrinsic {
                    dst,
                    intrinsic: Intrinsic::ToString,
                    args: vec![val],
                    ret_ty: Type::Str,
                });
                part_temps.push(dst);
                owned_parts.push(dst);
            }
        }
    }

    // If there is only one part and it was a plain literal, return it directly (immortal const).
    if part_temps.len() == 1 && owned_parts.is_empty() {
        return part_temps[0];
    }

    // Emit a single StringBuildN call: borrows all parts, returns one fresh +1 string.
    // lin_string_build_n(parts_ptr: *const *const LinString, n: u32) -> *mut LinString
    // codegen will stack-allocate the parts array and pass its pointer + count.
    let dst = builder.alloc_temp(Type::Str);
    builder.emit(Instruction::CallIntrinsic {
        dst,
        intrinsic: Intrinsic::StringBuildN,
        args: part_temps,
        ret_ty: Type::Str,
    });

    // Release the owned per-part ToString temps (now all borrowed by build_n; build_n copied them).
    for pt in owned_parts {
        builder.emit(Instruction::Release { val: pt, ty: Type::Str });
    }

    // The final result is a fresh +1 string. Register it owned identically to the old path.
    builder.register_owned(dst, Type::Str);
    dst
}

// -------------------------------------------------------------------------
// Pattern helpers
// -------------------------------------------------------------------------

pub(crate) fn pattern_type_check(pattern: &TypedPattern) -> (Type, lin_common::Span) {
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

pub(crate) fn pattern_required_fields(pattern: &TypedPattern) -> Vec<String> {
    match pattern {
        TypedPattern::Object { fields, .. } => fields.iter().map(|f| f.key.clone()).collect(),
        _ => vec![],
    }
}

pub(crate) fn stmt_defines_slot(stmt: &TypedStmt, slot: usize) -> bool {
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
pub(crate) fn stmt_reassigns_slot(stmt: &TypedStmt, slot: usize) -> bool {
    match stmt {
        TypedStmt::Val { value, .. } | TypedStmt::Var { value, .. } => expr_reassigns_slot(value, slot),
        TypedStmt::Expr(e) => expr_reassigns_slot(e, slot),
        TypedStmt::Destructure { value, .. } | TypedStmt::ArrayDestructure { value, .. } => {
            expr_reassigns_slot(value, slot)
        }
        TypedStmt::Import { .. } | TypedStmt::ForeignImport { .. } => false,
    }
}

pub(crate) fn expr_reassigns_slot(expr: &TypedExpr, slot: usize) -> bool {
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
        TypedExpr::MakeObject { fields, spreads, computed_fields, .. } => {
            fields.iter().any(|(_, v)| expr_reassigns_slot(v, slot))
                || spreads.iter().any(|s| expr_reassigns_slot(s, slot))
                || computed_fields.iter().any(|(k, v)| expr_reassigns_slot(k, slot) || expr_reassigns_slot(v, slot))
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
