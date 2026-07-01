use super::*;

/// Collect `var` slots that are mutably captured by any (possibly nested) closure within
/// a statement. Such slots are stored as heap cells shared by reference.
/// Collect `var` slots of an OWNING (rc/union) type that are reassigned INSIDE an `if`/`match`
/// branch. Such a var cannot be a plain SSA temp (release-old-on-overwrite and per-branch join
/// ownership are unrepresentable — the superseded initial value leaks on the taken branch and the
/// slot can dangle), so it is routed through a heap cell (which handles release-old + coherent
/// post-join reads). We record the declared type of every `var` we descend past so that when a
/// reassignment to it is seen inside a branch we can tell whether it needs owning. Nested function
/// bodies are NOT descended into here: their `var`s have their own slot namespace and their own
/// pre-scan; a capture of an OUTER var becomes a cell via the capture analysis instead.
pub(crate) fn collect_branch_reassigned_var_slots_stmt(
    stmt: &TypedStmt,
    in_branch: bool,
    owning_vars: &mut HashMap<usize, Type>,
    out: &mut std::collections::HashSet<usize>,
) {
    match stmt {
        TypedStmt::Var { slot, ty, value, .. } => {
            if needs_owning(ty) {
                owning_vars.insert(*slot, ty.clone());
            }
            collect_branch_reassigned_var_slots_expr(value, in_branch, owning_vars, out);
        }
        TypedStmt::Val { value, .. } => {
            collect_branch_reassigned_var_slots_expr(value, in_branch, owning_vars, out);
        }
        TypedStmt::Expr(e) => collect_branch_reassigned_var_slots_expr(e, in_branch, owning_vars, out),
        TypedStmt::Destructure { value, .. } | TypedStmt::ArrayDestructure { value, .. } => {
            collect_branch_reassigned_var_slots_expr(value, in_branch, owning_vars, out);
        }
        TypedStmt::Import { .. } | TypedStmt::ForeignImport { .. } => {}
    }
}

pub(crate) fn collect_branch_reassigned_var_slots_expr(
    expr: &TypedExpr,
    in_branch: bool,
    owning_vars: &mut HashMap<usize, Type>,
    out: &mut std::collections::HashSet<usize>,
) {
    let recur =
        |e: &TypedExpr, b: bool, ov: &mut HashMap<usize, Type>, o: &mut std::collections::HashSet<usize>| {
            collect_branch_reassigned_var_slots_expr(e, b, ov, o)
        };
    match expr {
        TypedExpr::LocalSet { slot, value, .. } => {
            if in_branch && owning_vars.contains_key(slot) {
                out.insert(*slot);
            }
            recur(value, in_branch, owning_vars, out);
        }
        // A nested function body owns its own slot namespace; descend with a FRESH owning-var map
        // and reset the branch flag — its declarations and reassignments are scoped to it.
        TypedExpr::Function { body, .. } => {
            let mut inner_owning: HashMap<usize, Type> = HashMap::new();
            collect_branch_reassigned_var_slots_expr(body, false, &mut inner_owning, out);
        }
        TypedExpr::Block { stmts, expr, .. } => {
            for s in stmts {
                collect_branch_reassigned_var_slots_stmt(s, in_branch, owning_vars, out);
            }
            recur(expr, in_branch, owning_vars, out);
        }
        // Reassignments inside ANY branch arm need a cell. The condition/scrutinee runs
        // unconditionally, but descending it with `in_branch` unchanged is harmless (a reassign in
        // a condition is itself only sound as a cell anyway).
        TypedExpr::If { cond, then_br, else_br, .. } => {
            recur(cond, in_branch, owning_vars, out);
            recur(then_br, true, owning_vars, out);
            recur(else_br, true, owning_vars, out);
        }
        TypedExpr::Match { scrutinee, arms, .. } => {
            recur(scrutinee, in_branch, owning_vars, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    recur(g, true, owning_vars, out);
                }
                recur(&arm.body, true, owning_vars, out);
            }
        }
        TypedExpr::Call { func, args, .. } => {
            recur(func, in_branch, owning_vars, out);
            for a in args {
                recur(a, in_branch, owning_vars, out);
            }
        }
        TypedExpr::BinaryOp { left, right, .. } => {
            recur(left, in_branch, owning_vars, out);
            recur(right, in_branch, owning_vars, out);
        }
        TypedExpr::UnaryOp { operand, .. } => recur(operand, in_branch, owning_vars, out),
        TypedExpr::Coerce { expr, .. } => recur(expr, in_branch, owning_vars, out),
        TypedExpr::MakeArray { elements, .. } => {
            for e in elements {
                recur(e, in_branch, owning_vars, out);
            }
        }
        TypedExpr::MakeObject { fields, spreads, computed_fields, .. } => {
            for (_, v) in fields {
                recur(v, in_branch, owning_vars, out);
            }
            for s in spreads {
                recur(s, in_branch, owning_vars, out);
            }
            for (k, v) in computed_fields {
                recur(k, in_branch, owning_vars, out);
                recur(v, in_branch, owning_vars, out);
            }
        }
        TypedExpr::Index { object, key, .. } => {
            recur(object, in_branch, owning_vars, out);
            recur(key, in_branch, owning_vars, out);
        }
        TypedExpr::IndexSet { object, key, value, .. } => {
            recur(object, in_branch, owning_vars, out);
            recur(key, in_branch, owning_vars, out);
            recur(value, in_branch, owning_vars, out);
        }
        TypedExpr::FieldGet { object, .. } => recur(object, in_branch, owning_vars, out),
        TypedExpr::Is { expr, .. } | TypedExpr::Has { expr, .. } => recur(expr, in_branch, owning_vars, out),
        TypedExpr::StringInterp { parts, .. } => {
            for p in parts {
                if let TypedStringPart::Expr(e) = p {
                    recur(e, in_branch, owning_vars, out);
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn collect_mutable_capture_slots_stmt(stmt: &TypedStmt, out: &mut std::collections::HashSet<usize>) {
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

pub(crate) fn collect_mutable_capture_slots_expr(expr: &TypedExpr, out: &mut std::collections::HashSet<usize>) {
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
        TypedExpr::MakeObject { fields, spreads, computed_fields, .. } => {
            for (_, v) in fields { collect_mutable_capture_slots_expr(v, out); }
            for s in spreads { collect_mutable_capture_slots_expr(s, out); }
            for (k, v) in computed_fields {
                collect_mutable_capture_slots_expr(k, out);
                collect_mutable_capture_slots_expr(v, out);
            }
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
/// `Stream<T>` is likewise a boxed `TaggedVal*(TAG_STREAM)` whose RC dispatches through the
/// tag-aware path (the TAG_STREAM arm decrements the stream box's refcount, closing the fd at
/// zero) — so it is owning too. `Promise<T>` is a boxed `TaggedVal*(TAG_PROMISE)` on the same
/// tag-aware RC path, so it belongs here too. `Opaque(_)` handles (e.g. TarEntry) are boxed
/// `TaggedVal*(TAG_*)` on the same tag-aware RC path — must be listed here so that closure
/// captures CloneBox them (rather than CaptureRelease::None, which would UAF after the
/// creating scope exits).
///
/// Stage 3 NullableRecord: `T | Null` where T is a sealed record is EXCLUDED from `is_union_ty`.
/// Such a union is represented as a raw nullable sealed-struct pointer (NOT a `TaggedVal*` box),
/// so it must not flow through any of the `TaggedVal` boxing/cloning/releasing paths. The repr
/// pass seeds it as `Packed(NullableRecord)` and codegen dispatches on that repr separately.
pub(crate) fn is_union_ty(ty: &Type) -> bool {
    if is_nullable_sealed_record(ty) { return false; }
    matches!(ty, Type::Union(_) | Type::TypeVar(_) | Type::Named(_) | Type::Shared(_) | Type::Stream(_) | Type::Promise(_) | Type::Opaque(_))
}

/// True iff `ty` is a `T | Null` union where `T` is a sealed record — the Stage-3 NullableRecord
/// repr. Such a value is physically a raw nullable `*sealed_T` pointer (not a `TaggedVal*`).
/// Delegates to `repr::nullable_sealed_record` as the single gate definition.
pub(crate) fn is_nullable_sealed_record(ty: &Type) -> bool {
    crate::repr::nullable_sealed_record(ty).is_some()
}

/// True if `ty` IS a `Stream` or a `Union` containing one. A streamish capture crosses a thread
/// boundary by MOVE (CAP_MOVE), not copy. Mirrors `lin_check::checker::expr::type_is_streamish`.
pub(crate) fn type_is_streamish_ir(ty: &Type) -> bool {
    match ty {
        Type::Stream(_) => true,
        Type::Union(variants) => variants.iter().any(type_is_streamish_ir),
        _ => false,
    }
}

/// A concrete heap-allocated value type whose box wraps a refcounted heap pointer
/// (Str/Array/FixedArray/Object/Iterator). Boxing one of these into a Json/union param
/// (via Coerce → `lin_box_str`/`lin_box_array`/`lin_box_object`) allocates a FRESH 16-byte
/// `TaggedVal*` shell whose inner is the (separately owned) heap pointer. Scalars
/// (int/bool/float/null) are excluded: their boxes may be cached/immutable.
pub(crate) fn is_heap_ty(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Str | Type::StrLit(_) | Type::Array(_) | Type::FixedArray(_) | Type::Object { .. } | Type::Iterator(_)
    )
}

/// Whether `lower_coerce_arg` PROJECTS a fresh UNSEALED object arg into a packed NullableRecord
/// struct (the `go(n-1, {"x": n})` self-tail-call accumulator case). Fires ONLY for the UNRESOLVED
/// Named param form (`Union([Named("T"), Null])`), where codegen's `nr_proj` cannot resolve the
/// sealed fields and would otherwise box the object as TAG_MAP into a NullableRecord slot — the
/// ADR-082 follow-up UAF. The result is a fully-owned packed struct (`register_owned`), NOT a
/// borrowed-inner box shell, so `arg_box_is_caller_owned_shell` must EXCLUDE it (kept in lockstep
/// with the projection guard in `lower_coerce_arg`).
pub(crate) fn arg_projects_unsealed_into_nullable_record(arg_ty: &Type, param_ty: &Type) -> bool {
    if !is_nullable_record_param(param_ty) || crate::repr::nullable_sealed_record(param_ty).is_some() {
        return false;
    }
    match arg_ty {
        Type::Object { fields, sealed: false, .. } => {
            !fields.is_empty() && fields.values().all(|f| f.is_sealed_field())
        }
        _ => false,
    }
}

/// Whether passing an argument of `arg_ty` to a parameter of `param_ty` causes
/// `lower_coerce_arg` to box a CONCRETE HEAP value into a fresh, caller-owned `TaggedVal*`
/// shell. The shell's inner heap pointer is owned separately (released by the arg's own
/// scope-exit release), so after the call the caller must free ONLY the shell.
/// True iff: param is union, arg is concrete heap. Excludes already-union args (the box
/// belongs to someone else) and scalar args (cached boxes).
pub(crate) fn arg_box_is_caller_owned_shell(arg_ty: &Type, param_ty: Option<&Type>) -> bool {
    match param_ty {
        // A SEALED SCALAR RECORD boxed into a Json param MATERIALIZES a FRESH inner heap value
        // (`box_object(LinObject)`) whose inner is NOT borrowed, so freeing only the shell would leak
        // the fresh inner object + its field references. It is FULLY released via the owning model
        // instead (see the call-arg loop's `full_release_boxes`), so it is NOT a shell-only box.
        // NOTE: a SEALED-RECORD ARRAY with keep-packed boxing (`lin_box_array`) IS a shell-only box —
        // the inner is owned by the caller's own rc scope, not by the fresh box. Include it here.
        Some(p) => is_union_ty(p) && !is_union_ty(arg_ty) && is_heap_ty(arg_ty)
            && !is_sealed_scalar_repr(arg_ty)
            // An Object arg flowing into a Named param passes through as-is (no box shell is
            // created by lower_coerce_arg — the callee compiled the Named param as its resolved
            // type, a raw LinMap*, so we pass the raw pointer directly). Claiming it IS a shell
            // here would cause lin_tagged_free_box on a raw LinMap* after the call → heap corruption.
            && !(matches!(arg_ty, Type::Object { .. }) && matches!(p, Type::Named(_)))
            // A sum-projected arg is fully owned + released by the owning model (a fresh `*SumNode`),
            // NOT a borrowed-inner box shell — freeing its "shell" would mismatched-size dealloc the
            // SumNode and the owning release would then double-free it.
            && !sum_arg_projected(arg_ty, p)
            // A fresh unsealed object PROJECTED into a NullableRecord param is a fully-owned packed
            // struct (the owning model releases it), NOT a borrowed-inner box shell — retaining it as
            // a shell on a tail call would leak one struct per iteration (ADR-082 follow-up).
            && !arg_projects_unsealed_into_nullable_record(arg_ty, p),
        None => false,
    }
}

/// Whether passing a SCALAR (Int/Float/Bool/Null) argument to a `Json`/union parameter boxes it
/// into a fresh, caller-owned `TaggedVal*` shell (`lin_box_int32`/`box_float64`/…) that the callee
/// borrows and never releases. A NON-cached scalar box (a large int / any float) is a fresh heap
/// shell with NO heap inner payload, so reclaiming the SHELL after the call balances it. Freeing is
/// done via `FreeBoxShellIfDistinct` → `lin_tagged_free_box_if_distinct`, which is CACHED-BOX SAFE
/// (small-int/bool boxes are immortal statics and never freed) and result-alias safe (a callee that
/// returns its Json param hands the same box back as the result — skipped when shell == result).
/// Without this, `f(1_000_000)` / `f(3.14)` into a Json param leaked the 16-byte box shell per call.
///
/// A `T | Null` NullableRecord is EXCLUDED: `is_union_ty` reports it `false` (it is a raw-pointer
/// repr, not a `TaggedVal` union), so it would otherwise slip through this "not-union, not-heap →
/// scalar-like" gate. But it is NOT a cached scalar — when the value is PHYSICALLY boxed (e.g. a
/// `Record | Null` bound to a union field-index result, `r["value"]`), the box→Json coerce is an
/// IDENTITY that shares the box, so `FreeBoxShellIfDistinct` would free the still-live source value
/// (`toString(v)` then reusing `v` → use-after-free). Leaving it out costs at most a 16-byte shell
/// per call in the rarer genuine-raw-pointer case; correctness on the boxed case is mandatory.
pub(crate) fn arg_box_is_caller_owned_scalar_shell(arg_ty: &Type, param_ty: Option<&Type>) -> bool {
    match param_ty {
        Some(p) => is_union_ty(p) && !is_union_ty(arg_ty) && !is_heap_ty(arg_ty)
            && !is_sealed_scalar_repr(arg_ty)
            && !is_nullable_sealed_record(arg_ty)
            && !sum_arg_projected(arg_ty, p),
        None => false,
    }
}

/// Retain a Function-typed argument that is NOT a freshly-made closure before passing it
/// to a call. AST-compiled callees release their Function-typed parameters at return; a
/// borrowed (non-fresh) closure must be retained to balance that, while a fresh closure's
/// existing +1 is consumed by the callee. Mirrors `call_global_fn`'s `arg_is_fn_owned`.
pub(crate) fn retain_call_arg(arg: Temp, ty: &Type, _is_fresh: bool, builder: &mut FuncBuilder) {
    if matches!(ty, Type::Function { .. }) {
        builder.emit(Instruction::Retain { val: arg, ty: ty.clone() });
    }
}

/// Whether an argument expression produces a freshly-allocated value (a function/closure
/// literal, a literal allocation, or a call result) whose +1 reference can be transferred
/// to a consuming callee or container. Mirrors AST `expr_is_owned_alloc` exactly.
pub(crate) fn expr_is_fresh_alloc(expr: &TypedExpr) -> bool {
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
pub(crate) fn free_arg_box_shells(shell_boxes: &[Temp], dst: Temp, builder: &mut FuncBuilder) {
    for &shell in shell_boxes {
        builder.emit(Instruction::FreeBoxShellIfDistinct { val: shell, other: dst });
    }
}

/// Coerce a call argument to the callee's declared parameter type: box a concrete value
/// for a Json/union param, OR widen/narrow a numeric mismatch (e.g. an Int32 literal `0`
/// passed to an Int64 param) so the call signature matches.
pub(crate) fn lower_coerce_arg(arg: Temp, arg_ty: &Type, param_ty: Option<&Type>, builder: &mut FuncBuilder) -> Temp {
    let Some(param_ty) = param_ty else { return arg; };
    // A sealed scalar-record arg flowing into a `Named` param (a recursive/self-referential type
    // reference that resolution left unexpanded) is passed THROUGH unchanged: the callee reads
    // the SAME unresolved `Named` param consistently as the sealed struct (its body's FieldGet
    // obj_ty is the expanded sealed Object), so the struct pointer flows as an opaque ptr without
    // a representation change. Without this, the `Named`-is-union-ish check below would box the
    // sealed struct (materialize → box_object) at e.g. a recursive self-call, storing a boxed
    // object back into a slot the body reads as a struct → garbage (caught by b_access). For
    // Stage 1, the only sealed-producing types are non-recursive named records whose `Named`
    // resolves to that sealed Object, so this pass-through is the correct representation.
    if is_sealed_scalar_repr(arg_ty) && matches!(param_ty, Type::Named(_)) {
        return arg;
    }
    // An UNSEALED object value (e.g. a record LITERAL, which `lower_expr` builds as a boxed
    // LinObject) flowing into a `Named` param that the callee reads as a SEALED struct. This is
    // the self-recursive-call case: the outer binding's function type resolves the param to the
    // sealed `Object`, but inside the body the recursive reference still carries the unexpanded
    // `Named` alias, so `func.ty()` hands us `Named(_)` here. The union arm below would box the
    // literal as Json — which the callee (reading constant struct offsets) then misreads as a
    // struct → heap corruption / segfault. Since the literal's fields are all sealed-eligible and
    // structural compatibility guarantees it has the named type's shape, PROJECT it into the
    // sealed struct layout (a fresh +1 owned struct) so the representation matches the callee.
    if matches!(param_ty, Type::Named(_)) {
        if let Type::Object { fields, sealed, .. } = arg_ty {
            if !*sealed && !fields.is_empty() && fields.values().all(is_sealed_field_ty) {
                // Unsealed object whose fields are all sealed-eligible: project it into the
                // named type's packed sealed struct layout so the callee's constant field-slot
                // reads don't misread a boxed LinObject.
                let sealed_ty = Type::sealed_object(fields.clone());
                let dst = builder.alloc_temp(sealed_ty.clone());
                builder.emit(Instruction::Coerce {
                    dst,
                    src: arg,
                    from_ty: arg_ty.clone(),
                    to_ty: sealed_ty.clone(),
                });
                builder.register_owned(dst, sealed_ty);
                return dst;
            }
            // Any Object (sealed or unsealed) flowing into a `Named` param where the callee
            // compiled the param as its resolved type — a LinMap* (boxed LinObject). Passing a
            // raw LinMap* here is correct. Without this guard the `is_union_ty(Named)` check
            // below would box it into a 16-byte TaggedVal* shell, which lin_map_get then reads
            // as a LinMap* — reading (*taggedval).len/slots instead of the real LinMap fields,
            // a heap-buffer-overflow. This fires when:
            //   1. An unsealed object with non-packable fields (e.g. Function-typed fields).
            //   2. A sealed object that is not a packed struct (has Function/non-packable fields
            //      at the top level, like GroupStationDepartAfterQuery.resultsFactory.getResults).
            // Both cases: the callee reads it as LinMap* via lin_map_get — no boxing needed.
            return arg;
        }
    }
    // SEALED-RECORD ARRAY BOUNDARY (Problem A / Stage 3b): a sealed-record array (packed/contiguous
    // representation, elem_tag 0xFE) flowing into a param that is NOT itself that same sealed-array
    // representation — a generic TypeVar/Json array (the type-erased combinator fallback's param),
    // an unsealed `Object[]`, or any boxed array — must be MATERIALIZED to the boxed tagged `Object[]`
    // view (`sealed_array_to_tagged`), else the callee reads the packed buffer through the boxed
    // `lin_array_get_tagged`/`lin_object_get` machinery → misaligned deref / corruption. The reverse
    // (boxed → sealed array) is the param's own sealed-scalar-array case, handled by `type_repr_differs`
    // below / the result Coerce. The materialized tagged array is a fresh +1 the arg scope releases.
    // Fire ONLY when the param element is a genuinely BOXED representation (a generic TypeVar/Json
    // wildcard, a union, or an unsealed `Object` — i.e. the type-erased boxed-fallback combinator's
    // `T[]` param). A param array whose element is a `Named` (an unexpanded self-referential alias,
    // the SELF-RECURSIVE-call case — e.g. `sumX(arr: Point[], …)` calling itself) reads the SAME
    // packed sealed struct consistently, so it must be passed THROUGH unchanged: materializing it
    // would store a boxed Object[] back into a slot the loop body reads as a packed array (observed:
    // recursive `arr[i].x` sum returned garbage after the first iteration). Mirrors the `Named`
    // pass-through guards above.
    if is_sealed_scalar_array(arg_ty)
        && !is_sealed_scalar_array(param_ty)
        && param_elem_is_boxed_repr(param_ty)
    {
        let dst = builder.alloc_temp(param_ty.clone());
        builder.emit(Instruction::Coerce { dst, src: arg, from_ty: arg_ty.clone(), to_ty: param_ty.clone() });
        // The materialized tagged `Object[]` is a FRESH fully-owned +1 the callee BORROWS (a TypeVar/
        // Json-array param is not released by the owning model). It is released right after the call by
        // the `sealed_array_arg_materialized` branch in the call-site arg loop (matching the existing
        // sealed-array→Json-param `full_release_boxes` path) — NOT registered owned here. The source
        // sealed array keeps its own ownership.
        return dst;
    }
    // UNBOXED SUM TYPE (unboxed-sumtype Stage 3): a BOXED/Json arg flowing into a sum-typed PARAM
    // (physically a `*SumNode` under the ABI) must be PROJECTED into a fresh `*SumNode` — codegen's
    // call-arg coercion (`compile_ir_coerce_with_repr` / `box_value` reverse) lowers a boxed→sum edge
    // via `sumnode_project_from_boxed`, which allocates a fresh +1 node. Emit an explicit `Coerce`
    // here and REGISTER IT OWNED so the call-site scope releases that +1 after the call — else it
    // leaks one SumNode subtree per call (ASan: a 48-byte/iteration leak that scales with the loop).
    // Fires when the param IS a Stage-eligible sum type but the arg is NOT already physically a sum
    // value (a boxed `sum|Null`, a partially-expanded recursive union from a container field read, or
    // a Json source). When the arg IS already the eligible sum union, it is a verbatim SumNode
    // pointer pass-through (no coercion) handled by the fall-through `arg` return below.
    if crate::repr::sum_type_eligible(param_ty)
        && !crate::repr::sum_type_eligible(arg_ty)
        && !matches!(arg_ty, Type::Named(_))
    {
        let dst = builder.alloc_temp(param_ty.clone());
        builder.emit(Instruction::Coerce { dst, src: arg, from_ty: arg_ty.clone(), to_ty: param_ty.clone() });
        builder.register_owned(dst, param_ty.clone());
        return dst;
    }
    // Stage 3 NullableRecord: `T → T|Null` where the param is NullableRecord-eligible. The
    // raw sealed struct ptr IS the NullableRecord repr — pass through with no coerce/boxing.
    // Similarly `Null → T|Null` and `T|Null → T|Null` (same type) pass through naturally.
    // Also handles `Union([Named("T"), Null])` — a self-recursive Named alias in the union;
    // the Named is the sealed record type and passes through identically.
    if is_nullable_record_param(param_ty) {
        // Check that the arg type is a NullableRecord-eligible sealed record (all fields sealable),
        // NOT just any sealed:true record. Trip{Service{Json}} is sealed:true but NOT NullableRecord.
        let arg_is_nullable_record_eligible = crate::repr::sealed_fields(arg_ty).is_some();
        let arg_is_null = matches!(arg_ty, Type::Null);
        let arg_is_nullable = crate::repr::nullable_sealed_record(arg_ty).is_some()
            || is_nullable_record_param(arg_ty);
        if arg_is_nullable_record_eligible || arg_is_null || arg_is_nullable {
            return arg;
        }
        // FRESH UNSEALED OBJECT into a NullableRecord param (the self-tail-call `go(n-1, {"x": n})`
        // accumulator case). The checker types an object literal flowing into a `T | Null` param as
        // an UNSEALED `Object { sealed: false }` (no `is_sealed_field` gate above fires).
        //
        // When the param is the RESOLVED nullable union (`Union([Object{sealed:true,name:T}, Null])`
        // — e.g. a NESTED closure `scan` whose env-captured signature carries the expanded record),
        // the generic `is_union_ty` boundary Coerce below already lowers correctly: codegen's reverse
        // `nr_proj` path resolves `sealed_fields(inner)` and projects the unsealed object into a fresh
        // packed struct matching the NullableRecord slot. Leave that case alone.
        //
        // The BUG is the UNRESOLVED Named form (`Union([Named("T"), Null])` — a TOP-LEVEL directly-
        // called `go` whose param type still names the alias). There `nr_proj` calls
        // `sealed_fields(Named("T"))` → None → falls through to `lin_box_map`, storing a BOXED
        // TaggedVal into a slot whose repr is NullableRecord (a raw packed-struct pointer). The slot's
        // RC release (`emit_tco_release_old` / `_final`) then calls `lin_sealed_release` on that boxed
        // map → reads the TaggedVal's tag byte as a refcount header, corrupting/freeing the box: a
        // use-after-free (garbage at shallow depth, segfault deep — ADR-082 follow-up).
        //
        // Fix: ONLY for the unresolved-Named form, emit the boundary Coerce to a RESOLVED nullable
        // union built from the arg's own sealable fields (`T = {x: Int32}` and the arg IS `{x: Int32}`,
        // so the sealed layout is identical). Codegen then takes the SAME reverse `nr_proj` path the
        // nested case uses, producing the packed struct the slot's NullableRecord release expects.
        // Register owned so the call-arg scope (or the TCO back-edge release-old) reclaims its +1.
        if arg_projects_unsealed_into_nullable_record(arg_ty, param_ty) {
            if let Type::Object { fields, .. } = arg_ty {
                let inner = Type::Object { fields: fields.clone(), sealed: true, name: None };
                let resolved = Type::Union(vec![inner, Type::Null]);
                let dst = builder.alloc_temp(resolved.clone());
                builder.emit(Instruction::Coerce {
                    dst,
                    src: arg,
                    from_ty: arg_ty.clone(),
                    to_ty: resolved.clone(),
                });
                builder.register_owned(dst, resolved);
                return dst;
            }
        }
    }
    // Box/unbox across the union boundary.
    if is_union_ty(param_ty) != is_union_ty(arg_ty) {
        let dst = builder.alloc_temp(param_ty.clone());
        builder.emit(Instruction::Coerce { dst, src: arg, from_ty: arg_ty.clone(), to_ty: param_ty.clone() });
        // UNBOX (union arg → concrete param). Two sub-cases, split on whether the unboxed value is a
        // HEAP pointer or a SCALAR:
        //
        //  - HEAP payload (`param_ty` is rc): `dst` aliases the SOURCE box's inner heap payload
        //    (e.g. `concat(b,b)` returns a boxed array, unboxed to the `UInt8[]` param). The inner
        //    pointer now lives in the param slot, so if this arg flows into a self-tail-call
        //    (`doubleUp(concat(b,b), n)`), `release_owned_for_tail_call` must NOT release the box —
        //    that would free the very array threaded into the slot (a double-free). Record the alias
        //    so the box is treated as kept (its shell is left to the dead-block release, the
        //    pre-existing accepted per-tail-call shell leak).
        //
        //  - SCALAR payload (`param_ty` is e.g. Int32): the unbox reads the scalar OUT of the box;
        //    the box's inner is NOT threaded into the slot, so the box `arg` is genuinely orphaned
        //    after the Coerce. Recording an escape-alias here would (wrongly) make
        //    `release_owned_for_tail_call` treat it as kept, leaking one 16-byte `TaggedVal` per loop
        //    iteration (calc `parseTermLoop`/`parseExprLoop`, csv `scanRows`). So DON'T alias: leave
        //    `arg` as a plain owned temp and let `release_owned_for_tail_call`'s non-arg live-block
        //    release reclaim it before the back-edge (leg1 FINDINGS §2).
        // (both the UNBOX heap/scalar split below and the WIDEN sealed-record exception further down
        // are now decided by the single ownership authority `escape_alias_convention`, which encodes
        // exactly this `is_union_ty`/`is_rc_type`/`is_sealed_scalar_repr` predicate — see its doc.)
        if crate::ownership_verify::escape_alias_convention(arg_ty, param_ty, is_sealed_scalar_repr(arg_ty)) {
            builder.record_escape_alias(dst, arg);
        }
        // WIDEN (concrete heap arg → union param): the Coerce `box_object`/`box_array`/… wraps the
        // inner WITHOUT bumping its rc — the box `dst` ALIASES `arg`'s inner heap payload. The source
        // `arg` stays registered owned in scope (its +1 is released at scope exit). For a normal call
        // that is correct (the box is a transient borrow consumed by the callee; the accepted
        // per-call shell residual is unchanged). But when this arg flows into a SELF-TAIL-CALL whose
        // param is the union (`scanRouteAt(…, trip, …)` re-threading a `match x is T => x` narrowed
        // `Trip` into the `Trip | Null` tail param), the box `dst` becomes the new param-slot value
        // and must SURVIVE the back-edge — yet `release_owned_for_tail_call` would release `arg`'s +1
        // (the box is not itself a kept arg unless aliased), freeing the inner while it is still
        // threaded into the slot AND still owned by a durable container (`tripsByRoute` + a
        // `kConnections` Conn) → double-free. Record the alias so the keep-set treats `arg` as kept
        // whenever the box `dst` is a kept tail-call arg (symmetric to the unbox alias above): the
        // inner survives into the slot, exactly as the source-box does in the unbox direction. No RC
        // accounting changes for non-tail calls (the alias is only consulted by the tail-call keep
        // expansion).
        //
        // EXCEPTION — a SEALED scalar record arg: the Coerce is NOT a cheap pointer-wrap. It runs
        // `sealed_materialize_to_object` → `box_object`, building a FRESH, INDEPENDENT `LinObject`
        // (it RETAINS the struct's heap fields into the new object), so the box `dst` does NOT alias
        // `arg`'s inner — `arg` (the source packed struct) is a genuine orphan once the box is built.
        // Recording the escape-alias here would mark that orphan as KEPT across the back-edge, so
        // `release_owned_for_tail_call` never releases it → the whole sealed struct (and the `id`
        // String / `stops` array it owns) leaks every iteration: the `scanRouteAt(…, mk(pi), …)`
        // tail-recursive `Trip | Null` threading leak (ASan-confirmed scaling, the RAPTOR RANGE-phase
        // RSS growth). The materialized box `dst` is the tail-call arg; the back-edge keeps it and
        // the TCO release-old reclaims it (its retained heap fields too). So DON'T alias for a sealed
        // arg — let the source struct be released before the back-edge. (Non-sealed heap args keep
        // the alias: their box genuinely shares the inner pointer threaded into the slot.)
        // [Both the UNBOX and this WIDEN decision are made by the single `escape_alias_convention`
        // call above — the predicate it evaluates is exactly the union of these two conditions.]
        return dst;
    }
    // Numeric width/kind mismatch between two concrete numeric types.
    if arg_ty.is_numeric() && param_ty.is_numeric() && arg_ty != param_ty {
        let dst = builder.alloc_temp(param_ty.clone());
        builder.emit(Instruction::Coerce { dst, src: arg, from_ty: arg_ty.clone(), to_ty: param_ty.clone() });
        return dst;
    }
    // Sealed scalar-record boundary: a wider/Json/unsealed (or differently-shaped sealed) argument
    // flowing into a sealed scalar-record param must be PROJECTED into the param's struct layout
    // (and a sealed arg into a Json/unsealed param MATERIALIZED). Without this a DIRECT call passes
    // a boxed LinObject straight into a function that reads struct offsets → garbage. Mirrors
    // `type_repr_differs`'s sealed arm.
    if type_repr_differs(arg_ty, param_ty) {
        let dst = builder.alloc_temp(param_ty.clone());
        builder.emit(Instruction::Coerce { dst, src: arg, from_ty: arg_ty.clone(), to_ty: param_ty.clone() });
        // A projection that produces a sealed scalar record is a fresh +1 owned struct: register
        // it so the call's arg-scope releases it (sealed release path). Materialization to a boxed
        // object is likewise fresh and registered by its own (object) owning model downstream.
        if is_sealed_scalar_repr(param_ty) {
            builder.register_owned(dst, param_ty.clone());
        }
        return dst;
    }
    arg
}

/// Lower `value` directly into a sealed scalar-record `slot_ty` AS a packed struct when `value`
/// is an object LITERAL providing exactly (at least) the target fields, with no spreads. Returns
/// `Some(temp)` having constructed the struct in place (field values stored by offset) — skipping
/// the build-boxed-LinObject-then-project round-trip the generic coercion would otherwise pay
/// (an `lin_object_alloc` + N sets + N `lin_object_get`). Returns `None` when the fast path does
/// not apply (caller falls back to `lower_expr` + `coerce_to_slot_type`). This is the construction
/// half of the sealed-records win (sealed-records Stage 1); it fires for `val p: T = { … }`,
/// `(…): T => { … }` returns, and arg/assignment boundaries.
pub(crate) fn try_lower_sealed_literal(
    value: &TypedExpr,
    slot_ty: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Option<Temp> {
    if !is_sealed_scalar_repr(slot_ty) {
        return None;
    }
    let (fields, spreads) = match value {
        TypedExpr::MakeObject { fields, spreads, .. } => (fields, spreads),
        _ => return None,
    };
    if !spreads.is_empty() {
        return None;
    }
    let Type::Object { fields: target_fields, .. } = slot_ty else { return None };
    if !target_fields.keys().all(|k| fields.iter().any(|(fk, _)| fk == k)) {
        return None;
    }
    let lowered_fields: Vec<(String, Temp)> =
        fields.iter().map(|(k, v)| (k.clone(), lower_expr(v, builder, ctx))).collect();
    let dst = builder.alloc_temp(slot_ty.clone());
    builder.emit(Instruction::MakeObject { dst, fields: lowered_fields, spreads: vec![], computed_fields: vec![], ty: slot_ty.clone(), stack: false });
    builder.register_owned(dst, slot_ty.clone());
    Some(dst)
}

/// FUSED `arr[i].field` / `arr[i]["field"]` over a SEALED-RECORD ARRAY (Stage 3). When `object`
/// is `Index{ array, key }` with `array` an all-scalar sealed-record array, and `field` is a SCALAR
/// field of the element record, emit a single `SealedArrayFieldGet` (a constant-offset load from the
/// contiguous element) instead of materializing a standalone sealed struct for the element then
/// reading its field. Returns `Some(dst)` on the fast path, `None` to fall through to the generic
/// path. Sound because the field is a scalar (no RC, no escaping interior pointer). The array base
/// is BORROWED where possible (a bare local) so no retain/release pair is paid in the hot loop.
pub(crate) fn try_lower_sealed_array_field(
    object: &TypedExpr,
    field: &str,
    result_ty: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Option<Temp> {
    let TypedExpr::Index { object: array, key, .. } = object else { return None };
    let arr_ty = array.ty();
    if !is_sealed_scalar_array(&arr_ty) {
        return None;
    }
    // Determine whether the field is a scalar or a heap pointer. Scalars are RC-free;
    // heap fields (String, Array, FixedArray, Map, nested sealed record) are BORROWED interior
    // pointers — the caller must Retain them into an owned reference. Non-field keys and
    // non-sealed-heap-field types fall back to the generic (materialization) path.
    let concrete_field_ty = match &arr_ty {
        Type::Array(elem) => match elem.as_ref() {
            Type::Object { fields, .. } => fields.get(field).cloned(),
            _ => None,
        },
        _ => None,
    };
    let concrete_field_ty = concrete_field_ty?;
    let scalar = concrete_field_ty.is_flat_scalar() || matches!(concrete_field_ty, Type::Bool);
    let heap_field = concrete_field_ty.is_sealed_heap_field();
    if !(scalar || heap_field) {
        return None;
    }
    // Lower the index, then the (borrowed where possible) array base last — mirrors the Index
    // borrow-ordering rule so a key that reassigns the array global can't dangle a borrowed base.
    let (array_temp, index_temp) = if lower_container_base_borrowed_check(array, ctx) {
        let index_temp = lower_expr(key, builder, ctx);
        let array_temp = lower_container_base_borrowed(array, builder, ctx)
            .unwrap_or_else(|| lower_expr(array, builder, ctx));
        (array_temp, index_temp)
    } else {
        let array_temp = lower_expr(array, builder, ctx);
        let index_temp = lower_expr(key, builder, ctx);
        (array_temp, index_temp)
    };
    // Emit the SealedArrayFieldGet at the CONCRETE field type (a raw scalar or raw interior
    // pointer). If result_ty differs (e.g. a coercion target), we coerce after the read.
    let raw = builder.alloc_temp(concrete_field_ty.clone());
    builder.emit(Instruction::SealedArrayFieldGet {
        dst: raw,
        array: array_temp,
        index: index_temp,
        field: field.to_string(),
        arr_ty,
        result_ty: concrete_field_ty.clone(),
    });
    // Heap field: the load yields a BORROWED interior pointer owned by the packed buffer.
    // Retain it so the caller holds an independent +1 (snapshot semantics — same as FieldGet).
    if heap_field {
        builder.emit(Instruction::Retain { val: raw, ty: concrete_field_ty.clone() });
        builder.register_owned(raw, concrete_field_ty.clone());
    }
    // Coerce to result_ty if needed (e.g. widening, boxing). When they match, this is a no-op.
    let coerced = coerce_to_slot_type(raw, &concrete_field_ty, result_ty, builder);
    if coerced != raw && is_union_ty(result_ty) {
        builder.register_owned(coerced, result_ty.clone());
    }
    Some(coerced)
}

/// PATH-1 in-place packed iteration: a `param["field"]` / `param.field` read where `param` is a
/// lambda element param bound to a BORROWED packed-array element VIEW (`ctx.packed_elem_slots`).
/// Emits a single const-offset `SealedArrayFieldGet` straight off the recorded `(array, index)` —
/// the SAME instruction the shipped `arr[i]["field"]` fusion uses — instead of the generic path that
/// would materialize a per-element struct then read it (or, worse, re-box it to a `LinObject` and do
/// a dynamic `lin_object_get` because the param's declared type is `Json`). Returns `Some(dst)` on
/// the fast path, `None` (no view, or an unsupported field type) to fall back.
///
/// Sound for SCALAR fields: a pure const-offset load with no RC. Sound for HEAP fields
/// (String/Array/FixedArray/Map/nested-sealed-Object): a const-offset `load ptr` returning a
/// BORROWED interior pointer owned by the packed buffer; a `Retain` is emitted so the caller
/// holds an independent +1, preventing a use-after-free if the array is freed before the field
/// value is used. This covers all `Type::is_sealed_heap_field()` field types, replacing the old
/// scalar+String-only limit that forced a whole-struct materialization for every Array/record
/// heap-field access (`retain_sealed_payload_fields` hotspot). The recorded `array`/`index` temps
/// are the view's own (live for the whole view scope), so no dangling borrowed pointer.
pub(crate) fn try_lower_packed_elem_field(
    object: &TypedExpr,
    field: &str,
    result_ty: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Option<Temp> {
    let TypedExpr::LocalGet { slot, .. } = object else { return None };
    let (array, index, elem_ty) = ctx.packed_elem_slots.get(slot)?.clone();
    // Only a SCALAR field is a sound const-offset read here (no RC, no interior-pointer escape).
    // The in-place gate is `is_sealed_scalar_array` (all fields scalar/Bool), so this always holds
    // for a declared field — but a non-declared key (safe-access → Null) must fall back.
    let concrete_field_ty = match &elem_ty {
        Type::Object { fields, .. } => fields.get(field).cloned(),
        _ => None,
    };
    let concrete_field_ty = concrete_field_ty?;
    // SCALAR/Bool: a pure const-offset load, no RC. HEAP (String, Array, FixedArray, Map, nested
    // sealed record): a const-offset `load ptr` yielding a BORROWED interior pointer (the array
    // still owns it) — sound iff the result is RETAINED when it escapes the read (snapshot
    // semantics), handled below. This covers all `is_sealed_heap_field` types, replacing the old
    // scalar+String-only limit that forced a whole-struct materialization for every Array/record
    // field access (the `retain_sealed_payload_fields` hotspot for Trip["stopTimes"]/["service"]).
    let scalar = concrete_field_ty.is_flat_scalar() || matches!(concrete_field_ty, Type::Bool);
    let heap_field = concrete_field_ty.is_sealed_heap_field();
    if !(scalar || heap_field) {
        return None;
    }
    // Read the field at its CONCRETE type (the const-offset load yields an unboxed scalar or
    // raw pointer). `result_ty` is the static type of `p["field"]`, which — because the element
    // param `p` is declared `Json` on the callback ABI — is typically `Json`/`TypeVar` (NOT the
    // concrete scalar). Allocating `dst` at `result_ty` while the instruction stores a raw scalar
    // or pointer would mistype the temp (codegen then mis-handles the value). So read at the
    // concrete type, then COERCE to `result_ty` (boxing the scalar into a `TaggedVal*` when
    // `result_ty` is Json) and register that fresh box owned so the body scope releases it —
    // without this the per-iteration operand box leaks. When `result_ty` already equals the
    // concrete type (a typed-element callback, post-monomorphization), the coerce is a no-op.
    let arr_ty = Type::Array(Box::new(elem_ty));
    let raw = builder.alloc_temp(concrete_field_ty.clone());
    builder.emit(Instruction::SealedArrayFieldGet {
        dst: raw,
        array,
        index,
        field: field.to_string(),
        arr_ty,
        result_ty: concrete_field_ty.clone(),
    });
    // A HEAP field read is a BORROWED interior pointer (the packed buffer owns it). Snapshot
    // semantics: the reader must own its own reference, so retain it (and register owned so the body
    // scope releases it). This mirrors the generic FieldGet's `is_rc_type` retain. A scalar needs no
    // RC. The subsequent coerce-to-Json (if any) boxes the (now-owned) value into a TaggedVal*.
    if heap_field {
        builder.emit(Instruction::Retain { val: raw, ty: concrete_field_ty.clone() });
        builder.register_owned(raw, concrete_field_ty.clone());
    }
    let coerced = coerce_to_slot_type(raw, &concrete_field_ty, result_ty, builder);
    if coerced != raw && is_union_ty(result_ty) {
        // A freshly boxed scalar (or boxed heap value): own it so scope exit reclaims the box.
        builder.register_owned(coerced, result_ty.clone());
    }
    Some(coerced)
}

/// FUSED `arr[i].field` / `arr[i]["field"]` over a BOXED `Object[]` whose element is a sealed/typed
/// record stored as a heap `LinObject` (the boxed `Token[]` representation: a record with heap
/// fields, which the packed-sealed-array gate REJECTS, so the array stays a boxed `Object[]`). When
/// `object` is `Index{ array, key }` with `array` such an array and `field` is a declared field of
/// the element record, emit a single `BoxedArrayFieldGet` (borrowed element box + one `lin_object_get`)
/// instead of the generic `arr[i]` path that MATERIALIZES the whole element into a fresh sealed
/// struct (alloc + read every field + per-field retain + reload + release) just to read one field.
/// Returns `Some(dst)` on the fast path, `None` to fall through to the generic path.
///
/// Sound: `lin_array_get` returns a BORROWED interior `*TaggedVal` (no fresh box, no release owed),
/// and `dst` is registered owned with a `Retain` for an RC `result_ty` — identical ownership to the
/// materialize-then-read path it replaces, so the boxed element's lifetime is unchanged. Only fires
/// for the SEALED-element case (so the element is a real `LinObject`, never a packed buffer): the
/// `is_sealed_scalar_array` guard EXCLUDES packed sealed-scalar arrays (handled by the
/// `SealedArrayFieldGet` fusion), and the element must be a sealed record (`is_sealed_scalar_repr`).
pub(crate) fn try_lower_boxed_array_field(
    object: &TypedExpr,
    field: &str,
    result_ty: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Option<Temp> {
    let TypedExpr::Index { object: array, key, .. } = object else { return None };
    let arr_ty = array.ty();
    // Must be an array of a SEALED record (the boxed `Object[]` shape) that is NOT a packed
    // sealed-scalar array (which the `SealedArrayFieldGet` fusion already handles).
    let elem = match &arr_ty {
        Type::Array(e) => e.as_ref(),
        _ => return None,
    };
    if !is_sealed_scalar_repr(elem) {
        return None;
    }
    if is_sealed_scalar_array(&arr_ty) {
        return None;
    }
    // The field must actually be a declared field of the element record (else the generic path's
    // safe-access → Null handling applies; keep that conservative behavior).
    let field_present = matches!(elem, Type::Object { fields, .. } if fields.contains_key(field));
    if !field_present {
        return None;
    }
    // Lower the index, then the (borrowed where possible) array base last — mirror the Index
    // borrow-ordering rule (a key that reassigns the array global can't dangle a borrowed base).
    let (array_temp, index_temp) = if lower_container_base_borrowed_check(array, ctx) {
        let index_temp = lower_expr(key, builder, ctx);
        let array_temp = lower_container_base_borrowed(array, builder, ctx)
            .unwrap_or_else(|| lower_expr(array, builder, ctx));
        (array_temp, index_temp)
    } else {
        let array_temp = lower_expr(array, builder, ctx);
        let index_temp = lower_expr(key, builder, ctx);
        (array_temp, index_temp)
    };
    let dst = builder.alloc_temp(result_ty.clone());
    builder.emit(Instruction::BoxedArrayFieldGet {
        dst,
        array: array_temp,
        index: index_temp,
        field: field.to_string(),
        arr_ty,
        result_ty: result_ty.clone(),
    });
    // The borrowed field read becomes an owned value (snapshot semantics), exactly like the generic
    // FieldGet/Index path: a union/Json field is relocated into a fresh owned box; a concrete heap
    // field is retained; a scalar needs nothing. This is the owning-read trichotomy — route it
    // through `own_for_read` (→ the ownership authority `owning_strategy`) instead of re-deriving the
    // union/rc/scalar split inline.
    Some(own_for_read(dst, result_ty, builder))
}

/// Lower `value` into a slot of declared type `slot_ty`, producing a temp in the slot's
/// representation. Uses the sealed-literal direct-construction fast path when applicable
/// (`try_lower_sealed_literal`), otherwise `lower_expr` + `coerce_to_slot_type`.
pub(crate) fn lower_value_into_slot(
    value: &TypedExpr,
    slot_ty: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Temp {
    if let Some(t) = try_lower_sum_literal(value, slot_ty, builder, ctx) {
        return t;
    }
    if let Some(t) = try_lower_sealed_literal(value, slot_ty, builder, ctx) {
        return t;
    }
    let t = lower_expr(value, builder, ctx);
    coerce_to_slot_type_owning_bind(t, &value.ty(), slot_ty, builder)
}

/// UNBOXED SUM TYPE (unboxed-sumtype Stage 1) — direct SumNode construction fast path. When `value`
/// is an object literal (no spreads) flowing into a Stage-1-eligible sum-type slot, emit a
/// `MakeObject` whose `ty` IS the sum type so the repr pass labels the temp `Packed(SumNode)` and
/// codegen's MakeObject branch packs it DIRECTLY via `sumnode_construct` — skipping the
/// build-a-boxed-`LinObject`-then-`sumnode_project_from_boxed` round-trip the generic coercion path
/// would otherwise pay every construction (the dominant cost the sum-dispatch benchmark exposed).
/// The discriminant field must be a `StrLit` (the variant tag); otherwise fall through (None).
pub(crate) fn try_lower_sum_literal(
    value: &TypedExpr,
    slot_ty: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Option<Temp> {
    if !crate::repr::sum_type_eligible(slot_ty) {
        return None;
    }
    let (fields, spreads) = match value {
        TypedExpr::MakeObject { fields, spreads, .. } => (fields, spreads),
        _ => return None,
    };
    if !spreads.is_empty() {
        return None;
    }
    // The discriminant field's value must be a string literal so codegen can statically pick the
    // variant tag. Codegen reads the disc field TEMP's type as `StrLit` (a string const is otherwise
    // typed plain `Str`), so force the disc temp's recorded type to `StrLit(value)`.
    let disc_key = crate::repr::sum_type_discriminant_of(slot_ty)?;
    // The variant this literal builds (selected by its discriminant StrLit value) — used to find the
    // declared field types so a RECURSIVE CHILD field's nested literal is pushed into its child sum
    // slot (so it constructs a `SumNode`, not a boxed object). NESTED-LITERAL DISCRIMINANT PUSHDOWN
    // (design §6 gap 1).
    let variant_fields: Option<indexmap::IndexMap<String, Type>> = fields
        .iter()
        .find(|(k, _)| k == &disc_key)
        .and_then(|(_, v)| match v {
            TypedExpr::StringLit(s, _, _) => crate::repr::sumnode_variant_by_disc(slot_ty, s),
            _ => None,
        });
    // The unique recursive self-name: a variant field typed `Named(self_name)` is a recursive child
    // whose CHILD SUM TYPE is `slot_ty` itself (direct self-recursion). Used to push a nested literal
    // into the right child sum slot so it constructs a `SumNode`.
    let self_name = crate::repr::sum_recursive_self_name(slot_ty);
    // `recursive_children` stores (slot_temp, src_expr, raw_temp_before_coerce).
    // `slot_temp` is the SumNode*-typed temp passed to MakeObject (after coerce if any).
    // `raw_temp_before_coerce` is the temp produced by lower_expr BEFORE any coerce was
    // applied; it may equal `slot_temp` when no coerce was needed.
    let mut recursive_children: Vec<(Temp, &TypedExpr, Temp)> = Vec::new();
    let lowered_fields: Vec<(String, Temp)> = fields
        .iter()
        .map(|(k, v)| {
            // RECURSIVE CHILD pushdown (design §6 gap 1): a field whose variant slot type is a
            // recursive self-child (`Named(self_name)`) carries a nested literal that must be
            // constructed AS a SumNode. Lower it into the child sum slot (== `slot_ty`, recursing
            // through `try_lower_sum_literal`) instead of as a plain (boxed) object literal.
            let is_recursive_child = self_name.as_deref().is_some_and(|n| {
                matches!(variant_fields.as_ref().and_then(|vf| vf.get(k)),
                    Some(Type::Named(fn_name)) if fn_name == n)
            });
            let t = if is_recursive_child {
                // Lower the raw value first (without the coerce), then coerce to the slot type.
                // We record BOTH so the ownership-transfer decision below can distinguish a
                // pfb-Coerce case (raw_temp IS owned — the CloneBox result — but the returned
                // coerced SumNode* is NOT, because pfb already provides +1) from a bare-param
                // case (raw_temp NOT owned either).
                let t_raw = if let Some(inner) = try_lower_sum_literal(v, slot_ty, builder, ctx) {
                    // Nested literal: already a SumNode*, registered owned. No coerce needed.
                    inner
                } else if let Some(inner) = try_lower_sealed_literal(v, slot_ty, builder, ctx) {
                    inner
                } else {
                    lower_expr(v, builder, ctx)
                };
                let t_coerced = coerce_to_slot_type_owning_bind(t_raw, &v.ty(), slot_ty, builder);
                recursive_children.push((t_coerced, v, t_raw));
                t_coerced
            } else {
                lower_expr(v, builder, ctx)
            };
            if k == &disc_key {
                if let TypedExpr::StringLit(s, _, _) = v {
                    builder.temp_types.insert(t, Type::StrLit(s.clone()));
                }
            }
            (k.clone(), t)
        })
        .collect();
    // Bail if the discriminant didn't resolve to a StrLit temp (e.g. a computed key) — fall back to
    // the boxed coercion path, which `sumnode_project_from_boxed` handles soundly.
    let disc_ok = lowered_fields.iter().any(|(k, t)| {
        k == &disc_key && matches!(builder.temp_types.get(t), Some(Type::StrLit(_)))
    });
    if !disc_ok {
        return None;
    }
    let dst = builder.alloc_temp(slot_ty.clone());
    builder.emit(Instruction::MakeObject {
        dst,
        fields: lowered_fields,
        spreads: vec![],
        computed_fields: vec![],
        ty: slot_ty.clone(),
        stack: false,
    });
    // Transfer each recursive child's ownership into the SumNode (the node owns +1 of each child,
    // released by its KIND_SUMNODE drop walk). A fresh child literal's +1 MOVES into the node
    // (unregister from this scope); a borrowed child sub-expr is retained. Mirrors the
    // `transfer_into_container` rule for array/object element inserts — codegen does not retain at
    // the store, so this is the sole balancing reference for the node's owned child slot.
    //
    // EXCEPTION — SUMNODE-COERCE RECURSIVE CHILD (SumNode double-retain fix):
    // When a recursive child value `v` has a type that is NOT directly sum-type-eligible (e.g.
    // `Union([...with Named("Ast") children...])` — an intermediate form the monomorphizer may
    // produce when one level of Named alias is expanded), but the `slot_ty` IS sum-type-eligible,
    // `type_repr_differs` detects a repr change and a Coerce instruction is emitted. Codegen's
    // `compile_ir_coerce` for this cross-SumNode-boundary case calls `sumnode_project_from_boxed`
    // (pfb) → the pfb_kp or pfb_proj path — BOTH of which already provide +1 for the SumNode slot.
    //
    // In this case `t_coerced` (the SumNode* from pfb) is NOT registered owned (the Coerce
    // instruction does not register its result), but `t_raw` (the pre-Coerce value, e.g. a
    // TaggedVal* from a CloneBox) IS owned in scope. The scope-exit Release of `t_raw` emits
    // `lin_tagged_release` which decrements the inner SumNode once — exactly cancelling the
    // `lin_tagged_clone`'s +1. Pfb's +1 then accounts for the BinOp.right slot ownership.
    //
    // Emitting an ADDITIONAL `Retain` via `transfer_into_container` adds a second +1 that is
    // never balanced → SumNode leak (one per BinOp construction). SKIP the Retain by checking:
    //   `t_raw IS owned` (a CloneBox/fresh source) AND `t_coerced != t_raw` (a Coerce fired).
    //
    // By contrast, for a BORROWED param child (e.g. `"left": left`):
    //   `t_raw NOT owned` AND `t_coerced == t_raw` (no Coerce, same repr) → Retain STILL fires.
    // For a FRESH literal child (nested SumNode literal, `try_lower_sum_literal` path):
    //   `t_raw IS owned` AND `t_coerced == t_raw` (no Coerce, already SumNode*) → Transfer.
    for (t_coerced, src, t_raw) in &recursive_children {
        let raw_owned = builder.is_owned_in_scope(*t_raw);
        let coerce_fired = t_coerced != t_raw;
        let pfb_provides_plus1 = coerce_fired && raw_owned;
        if !pfb_provides_plus1 {
            builder.transfer_into_container(*t_coerced, src, true);
        }
        // When pfb_provides_plus1 is true: skip — pfb (+1) + scope-exit tagged_release (-1 on
        // inner SumNode) net to exactly +1 for the slot ownership. No extra Retain needed.
    }
    builder.register_owned(dst, slot_ty.clone());
    Some(dst)
}

/// True when `from` and `to` are BOTH UNSEALED OBJECTS with DIFFERENT field sets
/// (D3b anon-slot widen): both are physically `LinObject*` but different shapes — the
/// source is WIDER (or merely different) and the slot must project-copy to sever sharing.
/// Only fires for a genuine field mismatch; exact-shape pass-through is a no-op.
///
/// EXCEPTION (`!tf.is_empty()`): an EMPTY target `{}` is the OPEN/top object type, not a closed
/// zero-field record. Coercing into it is a WIDEN ("any object"), so the source must pass through
/// keeping ALL its dynamic fields — never project-copy to a 0-field object. Without this guard a
/// value flowing into a `{}`-typed slot (e.g. a `{}[]` element, the factory-of-counters pattern)
/// is stripped to an empty `LinObject` → every field reads `Null` (scalar) or a garbage pointer
/// (Function field → segfault on call). Regression: `test_var_cell_escaping_via_object_in_loop_body`.
pub(crate) fn anon_object_slot_repr_differs(from: &Type, to: &Type) -> bool {
    matches!((from, to),
        (Type::Object { sealed: false, fields: ff, .. }, Type::Object { sealed: false, fields: tf, .. })
        if ff != tf && !tf.is_empty())
}

/// Coerce a value into a (plain, non-cell) local/global SLOT the binding will OWN, transferring
/// ownership of any transient coercion box into the scope's owned set so scope-exit reclaims it.
///
/// This is the BINDING analogue of `coerce_and_own_store` (the CELL/global case, which clones the
/// box and frees the transient shell). Here the binding does NOT clone — the box IS the value the
/// slot holds — so the scope must OWN the box itself.
///
/// The leak this fixes: when `coerce_to_slot_type` boxes a CONCRETE heap value (`is_rc_type`
/// true: Str/Array/FixedArray/Object/Iterator) into a UNION slot, it emits a `Coerce`
/// (`lin_box_object`/`lin_box_array`/`lin_box_str`) producing a fresh 16-byte `TaggedVal*` shell
/// `b` that wraps the raw inner WITHOUT bumping the inner's rc, and registers NOTHING for `b`.
/// `lower_expr` already `register_owned`'d the raw inner (a fresh alloc, or a retained read).
/// Scope-exit then releases the raw inner (rc 1→0, frees it) but orphans `b`'s shell → 16 B leak.
///
/// Fix: the box `b` becomes the owned union representation. `register_owned(b, slot_ty)` so
/// scope-exit releases it via `lin_tagged_release` — which frees BOTH the shell AND drops the
/// inner's rc — and `unregister_owned(raw)` so the inner's single +1 transfers INTO the box
/// (otherwise scope-exit would also release the inner → double-free). Exactly mirrors the
/// `coerce_if_branch` "concrete value boxed to union" case (the box owns its inner via the kept
/// raw temp) and `transfer_into_container`'s fresh-alloc unregister.
///
/// No-op (delegates to `coerce_to_slot_type` only) when: representations match (no box made), the
/// slot is not a union (e.g. sealed-scalar element coercion — the only other caller of
/// `lower_value_into_slot`), or the value is already a union (the box is a clone/forward handled
/// elsewhere) or non-rc (scalar→union boxing carries no inner heap payload to balance — the cached
/// scalar box has nothing to release, and the raw scalar is not registered owned anyway).
pub(crate) fn coerce_to_slot_type_owning_bind(t: Temp, value_ty: &Type, slot_ty: &Type, builder: &mut FuncBuilder) -> Temp {
    // Box-transfer ownership fact (whether widening this concrete-rc value into the union slot makes
    // a fresh box that takes over the source's inner +1 — so the lowerer must MOVE that reference)
    // lives in the ownership authority; `type_repr_differs` is the lower-only repr predicate it
    // requires, passed in. Distinct from `box_shell_reclaim` (the cell/global clone case) by the
    // `is_rc_type(value)` conjunct it folds in — see `bound_box_moves_inner`.
    let made_fresh_box = crate::ownership_verify::bound_box_moves_inner(
        value_ty,
        slot_ty,
        type_repr_differs(value_ty, slot_ty),
    );
    // A flat scalar array kind/width change (e.g. UInt8[] → Int32[]) MATERIALIZES a fresh,
    // independent +1-owned buffer (codegen's `flat_array_widen`) — the source array keeps its own
    // reference (released by its own scope). Unlike the union-box case the source is NOT consumed,
    // so leave `t` registered; just register the fresh result so the binding's scope releases it.
    let made_fresh_array = flat_scalar_array_repr_differs(value_ty, slot_ty);
    // A BOXED-element array (a combinator result `pts.map(...)` whose lambda returns an unsealed
    // object literal, runtime repr = `Object[]` of boxed `LinObject`s) bound to a packed
    // sealed-scalar-array slot (`Pt[]`). `type_repr_differs`'s new sealed-array arm emits a `Coerce`
    // whose codegen `sealed_array_project_owned` PROJECTS the boxed array into a FRESH +1-owned packed
    // 0xFE buffer (rebuild branch — the source is genuinely boxed, never keep-packed here because a
    // keep-packed source would be a bare Json/union caught by the union arm, not an
    // `Array(boxed-element)`). Like the flat-array widen, the source array (`t`) is NOT consumed and
    // keeps its own +1 (released by its own scope); register the fresh packed result so the binding's
    // scope releases it. Mirrors the explicit-`TypedExpr::Coerce` sealed-array arm (lower_expr ~3337).
    let sealed_array_reprojected =
        param_elem_is_boxed_repr(value_ty) && is_sealed_scalar_array(slot_ty);
    // A KEEP-PACKED sealed array (P[]) boxed into a union/AnyVal slot: codegen's `lin_box_array`
    // produces a 16-byte shell that BORROWS the source pointer (no rc bump). The source (`t`) is
    // already registered owned and will be released separately. The box shell must be freed via
    // FreeBoxShell only — NOT lin_tagged_release (which would also release the inner, causing a
    // spurious extra release → double-free). Register the box for shell-only reclaim.
    let keep_packed_arr_shell = made_fresh_box && is_sealed_scalar_array(value_ty);
    // A KEEP-PACKED sealed record (P) boxed into a union/AnyVal slot: codegen's `lin_box_record`
    // RETAINS the inner (+1), so the box is truly owned (shell + inner retain). The source (`t`)
    // keeps all its own registrations (alloc rc + own_for_read retain); the box adds one MORE owned
    // reference that must be released via lin_tagged_release (decrement inner + free shell).
    // Do NOT unregister_owned(t) — the source's registrations still balance its references.
    let keep_packed_rec_owned = made_fresh_box && is_sealed_scalar_repr(value_ty);
    let coerced = coerce_to_slot_type(t, value_ty, slot_ty, builder);
    if sealed_array_reprojected {
        // The reproject builds a new packed buffer that does NOT consume the source: the source
        // array (`t`) keeps its own +1 (released by its own scope), so register only the fresh
        // result for scope-exit release.
        if coerced != t {
            builder.register_owned(coerced, slot_ty.clone());
        }
    } else if keep_packed_arr_shell {
        // Shell-only box: the source (`t`) remains registered; only free the box shell at scope exit.
        if coerced != t {
            builder.register_box_shell(coerced);
        }
    } else if keep_packed_rec_owned {
        // Owned box (lin_box_record retains): register for full lin_tagged_release; source stays registered.
        if coerced != t {
            builder.register_owned(coerced, slot_ty.clone());
        }
    } else if made_fresh_box {
        // The box now owns the inner's +1; the scope releases the box (freeing shell + inner).
        builder.unregister_owned(t);
        builder.register_owned(coerced, slot_ty.clone());
    } else if made_fresh_array && coerced != t {
        builder.register_owned(coerced, slot_ty.clone());
    }
    coerced
}

/// Coerce a value temp to a slot's declared type when their runtime representations
/// differ (box concrete → union, or unbox union → concrete). Returns the (possibly new)
/// temp; a no-op when representations match.
pub(crate) fn coerce_to_slot_type(t: Temp, value_ty: &Type, slot_ty: &Type, builder: &mut FuncBuilder) -> Temp {
    if type_repr_differs(value_ty, slot_ty)
        || scalar_numeric_repr_differs(value_ty, slot_ty)
        || int_width_repr_differs(value_ty, slot_ty)
        || flat_scalar_array_repr_differs(value_ty, slot_ty)
    {
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
pub(crate) fn coerce_if_branch(
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
    //
    // RC OWNERSHIP: for heap result types (String/Array/Object) the branch may contain a retained
    // read of the raw value (e.g. a narrowed-union LocalGet emits Coerce+Retain+register_owned in
    // the branch scope). That Retain is kept by `pop_scope_releasing_keep` but NOT re-registered in
    // the parent scope when `owned=false` — the +1 is orphaned, causing an RC leak that inflates to
    // u32 overflow under high-iteration workloads (RAPTOR queue-factory: malloc_consolidate error).
    // Fix: return `owned=true` for RC result types so `lower_if` calls `register_owned(result_dst)`
    // in the enclosing scope, balancing the branch's kept Retain via the normal scope-exit Release.
    // Scalar result types (Bool/Int/Float/Null) are unaffected: `needs_owning` guards register_owned.
    let val = coerce_to_slot_type(raw, value_ty, result_type, builder);
    (val, vec![val, raw], is_rc_type(result_type))
}

pub(crate) fn const_type(c: &Const) -> Type {
    match c {
        Const::Int(_, t) => t.clone(),
        Const::Float(_, t) => t.clone(),
        Const::Bool(_) => Type::Bool,
        Const::Null => Type::Null,
        Const::Str(_) => Type::Str,
    }
}
