use super::*;

// -------------------------------------------------------------------------
// If lowering
// -------------------------------------------------------------------------

/// Lower a condition expression to an i1 Bool temp. A condition whose static type is not
/// already Bool (e.g. a call to an untyped `f: Function` predicate, which returns a boxed
/// Json) is coerced — codegen lowers a Json→Bool Coerce via lin_unbox_bool. Without this,
/// codegen's CondJump sees a non-i1 value and defaults the branch to `false`.
pub(crate) fn lower_cond_as_bool(cond: &TypedExpr, builder: &mut FuncBuilder, ctx: &mut LowerCtx) -> Temp {
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

pub(crate) fn lower_if(
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

    // Tag the branch entry blocks with their source spans for coverage. The merge block
    // covers no distinct source region, so it stays None.
    builder.set_block_span(then_block, then_br.span());
    builder.set_block_span(else_block, else_br.span());

    builder.terminate(Terminator::CondJump {
        cond: cond_temp,
        then_block,
        else_block,
    });

    let result_dst = builder.alloc_temp(result_type.clone());

    // Each branch gets its own ownership scope so heap temps it allocates are released
    // at the end of *that branch* — not in the merge block, where only one branch's
    // temps are live (releasing the other branch's temps there frees undefined values).
    // `coerce_if_branch` produces a value the merge OWNS independently (cloning a borrowed
    // union/concrete that aliases an enclosing-scope value), so registering+releasing the
    // merge is always balanced — no borrowed-box double-free (the historic reason a union
    // merge was left unowned: e.g. minBy's reducer `if x[0] < acc[0] then x else acc` over
    // params — now those params are cloned into a fresh +1).
    // We collect (value_temp, predecessor_block) for a Phi in the merge block, recording
    // the ACTUAL predecessor (the block current at the end of the branch, which may differ
    // from the branch entry if the branch contained nested control flow).
    let mut incomings: Vec<(Temp, BlockId)> = Vec::new();
    // Whether the merged value carries an independent +1 (so the enclosing scope owns/releases
    // it). Determined by the branch coercion; both branches agree (it is a function of the
    // result representation). Defaults to the concrete-rc rule if neither branch falls through.
    let mut merge_owned = is_rc_type(result_type) || is_union_ty(result_type);

    // Snapshot the plain SSA var-slot → temp map BEFORE the branches. A `var` mutated inside a
    // branch (`LocalSet` for a plain SSA-temp slot, line ~1853) just rebinds `slots[slot]` to a
    // NEW temp that is only defined inside that branch's block. Without merging, a read of the
    // slot AFTER the join sees either a temp that doesn't dominate the merge (SSA violation) or
    // the wrong branch's value — the closure-local-var-in-if bug. We record per-branch which
    // slots were rebound, then emit join phis below (mirroring the if-result phi). Cells and
    // global vars are NOT in this set: their `slots` entry is a stable cell pointer / they read
    // through GlobalValGet, so reassignment is already visible across the join.
    let pre_slots = builder.plain_var_slot_snapshot(ctx);

    // --- then branch ---
    builder.switch_to(then_block);
    builder.push_scope();
    // UNBOXED SUM TYPE: a sum-eligible object LITERAL branch flowing into a sum-typed `if` result
    // is constructed DIRECTLY as a SumNode (skip the build-boxed-then-project round-trip). When it
    // fires, the branch value already carries the sum type, so `coerce_if_branch` sees value_ty ==
    // result_type and just transfers the owned +1 (no re-coercion).
    let (then_raw, then_eff_ty) = match try_lower_sum_literal(then_br, result_type, builder, ctx) {
        Some(t) => (t, result_type.clone()),
        None => (lower_expr(then_br, builder, ctx), then_br.ty()),
    };
    let mut then_reassigned: Vec<(usize, Temp)> = Vec::new();
    let mut then_pred = builder.current_block;
    let then_live = if !builder.is_current_block_terminated() {
        let (then_val, mut keep, owned) = coerce_if_branch(then_raw, &then_eff_ty, result_type, builder);
        merge_owned = owned;
        // A slot the branch rebound holds a value registered owned in THIS branch scope (the
        // LocalSet's value temp). It must survive the branch pop so the join phi can forward it,
        // so add it to the keep-set: its +1 transfers up to the enclosing scope, where the phi
        // result becomes the slot's single owner (see below).
        then_reassigned = builder.collect_reassigned_slots(&pre_slots, ctx);
        for (_, t) in &then_reassigned {
            keep.push(*t);
        }
        builder.pop_scope_releasing_keep(&keep);
        then_pred = builder.current_block;
        incomings.push((then_val, builder.current_block));
        builder.terminate(Terminator::Jump(merge_block));
        true
    } else {
        builder.discard_scope();
        false
    };
    // Restore the slot map to its pre-if state so the else branch sees the ORIGINAL slot temps,
    // not the then-branch's rebindings (each branch must lower against the pre-if values).
    builder.restore_plain_var_slots(&pre_slots);

    // --- else branch ---
    builder.switch_to(else_block);
    builder.push_scope();
    let (else_raw, else_eff_ty) = match try_lower_sum_literal(else_br, result_type, builder, ctx) {
        Some(t) => (t, result_type.clone()),
        None => (lower_expr(else_br, builder, ctx), else_br.ty()),
    };
    let mut else_reassigned: Vec<(usize, Temp)> = Vec::new();
    let mut else_pred = builder.current_block;
    let else_live = if !builder.is_current_block_terminated() {
        let (else_val, mut keep, owned) = coerce_if_branch(else_raw, &else_eff_ty, result_type, builder);
        merge_owned = owned;
        else_reassigned = builder.collect_reassigned_slots(&pre_slots, ctx);
        for (_, t) in &else_reassigned {
            keep.push(*t);
        }
        builder.pop_scope_releasing_keep(&keep);
        else_pred = builder.current_block;
        incomings.push((else_val, builder.current_block));
        builder.terminate(Terminator::Jump(merge_block));
        true
    } else {
        builder.discard_scope();
        false
    };

    builder.switch_to(merge_block);

    // Merge any plain `var` slot mutated in a branch with a join phi (the slot now reads the phi
    // result after the if). For each slot reassigned in EITHER branch, the incoming value is the
    // branch's rebound temp if it reassigned, otherwise the pre-if temp (the value flowing in
    // unchanged on that edge). Exactly one phi incoming reference is live at runtime, so the phi
    // result holds a single +1 the enclosing scope owns; we drop the per-branch and pre-if
    // registrations and register the phi result once to keep ownership balanced.
    merge_var_slots(
        builder,
        &pre_slots,
        &then_reassigned,
        if then_live { Some(then_pred) } else { None },
        &else_reassigned,
        if else_live { Some(else_pred) } else { None },
    );

    // Merge the per-branch results with a Phi. (A plain Copy into a shared temp is wrong:
    // the single-pass codegen would let the last-compiled branch's value win for both paths.)
    builder.emit(Instruction::Phi {
        dst: result_dst,
        ty: result_type.clone(),
        incomings,
    });
    // The merged result is owned by the enclosing scope (released there, or kept if it is
    // the block's return value).
    if merge_owned {
        builder.register_owned(result_dst, result_type.clone());
    }
    result_dst
}

/// After both branches of an `if` have jumped to `merge_block`, reconcile any plain SSA `var`
/// slot that was reassigned in at least one branch by emitting a join phi. `pre_slots` is the
/// (slot, temp, type) snapshot taken before the branches; `*_reassigned` are the slots each branch
/// rebound (slot → branch-local temp). `*_pred` is the actual predecessor block at the end of the
/// branch (it may differ from the branch entry if the branch had nested control flow), or None if
/// that branch diverged (no edge into the merge — its slot values are unreachable there).
pub(crate) fn merge_var_slots(
    builder: &mut FuncBuilder,
    pre_slots: &[(usize, Temp, Type)],
    then_reassigned: &[(usize, Temp)],
    then_pred: Option<BlockId>,
    else_reassigned: &[(usize, Temp)],
    else_pred: Option<BlockId>,
) {
    // The set of slots needing a join phi = those reassigned in either live branch.
    use std::collections::HashSet;
    let mut changed: HashSet<usize> = HashSet::new();
    if then_pred.is_some() {
        for (s, _) in then_reassigned {
            changed.insert(*s);
        }
    }
    if else_pred.is_some() {
        for (s, _) in else_reassigned {
            changed.insert(*s);
        }
    }
    if changed.is_empty() {
        return;
    }
    let then_map: std::collections::HashMap<usize, Temp> = then_reassigned.iter().copied().collect();
    let else_map: std::collections::HashMap<usize, Temp> = else_reassigned.iter().copied().collect();
    for (slot, pre_temp, ty) in pre_slots {
        if !changed.contains(slot) {
            continue;
        }
        // Build the phi incomings: the rebound temp on a branch that reassigned, the pre-if temp
        // on a branch that left the slot alone. Only include an edge if that branch is live
        // (didn't diverge); if one branch diverged, control reaches the merge solely via the other
        // edge and a single-incoming phi is correct.
        let mut incomings: Vec<(Temp, BlockId)> = Vec::new();
        if let Some(tp) = then_pred {
            incomings.push((then_map.get(slot).copied().unwrap_or(*pre_temp), tp));
        }
        if let Some(ep) = else_pred {
            incomings.push((else_map.get(slot).copied().unwrap_or(*pre_temp), ep));
        }
        let phi_dst = builder.alloc_temp(ty.clone());
        builder.emit(Instruction::Phi { dst: phi_dst, ty: ty.clone(), incomings });
        // The pre-if value and each branch-rebound value were registered owned in the enclosing
        // scope (the pre-if init / read, plus each branch's kept LocalSet value transferred up).
        // After the join exactly ONE of them is live, reachable only as the phi result — so drop
        // those individual registrations and register the phi as the slot's single owner.
        if needs_owning(ty) {
            builder.unregister_owned(*pre_temp);
            if let Some(&t) = then_map.get(slot) {
                builder.unregister_owned(t);
            }
            if let Some(&t) = else_map.get(slot) {
                builder.unregister_owned(t);
            }
            builder.register_owned(phi_dst, ty.clone());
        }
        // The slot now reads the merged phi after the if.
        builder.slots.insert(*slot, phi_dst);
    }
}

/// Lower a short-circuiting `&&` / `||` (spec §8) as branch + merge + Phi, so the RHS is
/// only evaluated on the path that needs it.
///
/// - `a && b`: eval a; if a then eval b else `false`; phi.
/// - `a || b`: eval a; if a then `true`  else eval b; phi.
///
/// The RHS is lowered INSIDE the conditionally-executed block (its own ownership scope), so any
/// owned temps it allocates are released there and are only ever created on the taken path —
/// exactly as lower_if handles a branch arm. Both operands are booleans (scalars), so the result
/// is RC-trivial.
pub(crate) fn lower_short_circuit(
    left: &TypedExpr,
    op: BinOp,
    right: &TypedExpr,
    _result_type: &Type,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> Temp {
    let lhs = lower_cond_as_bool(left, builder, ctx);

    // The block that evaluates the RHS, and the block that short-circuits to a constant.
    let rhs_block = builder.alloc_block(if matches!(op, BinOp::And) { "and_rhs" } else { "or_rhs" });
    let short_block = builder.alloc_block(if matches!(op, BinOp::And) { "and_short" } else { "or_short" });
    let merge_block = builder.alloc_block("sc_merge");
    builder.set_block_span(rhs_block, right.span());

    // For `&&`, the RHS is evaluated when the LHS is true; for `||`, when the LHS is false.
    let (then_block, else_block) = match op {
        BinOp::And => (rhs_block, short_block),
        BinOp::Or => (short_block, rhs_block),
        _ => unreachable!("lower_short_circuit only handles And/Or"),
    };
    builder.terminate(Terminator::CondJump {
        cond: lhs,
        then_block,
        else_block,
    });

    let result_dst = builder.alloc_temp(Type::Bool);
    let mut incomings: Vec<(Temp, BlockId)> = Vec::new();

    // --- RHS block: evaluate the right operand (its own ownership scope) ---
    builder.switch_to(rhs_block);
    builder.push_scope();
    let rhs_raw = lower_cond_as_bool(right, builder, ctx);
    if !builder.is_current_block_terminated() {
        // rhs is a Bool scalar; keep it across the scope pop (nothing to release for a bool).
        builder.pop_scope_releasing_keep(&[rhs_raw]);
        incomings.push((rhs_raw, builder.current_block));
        builder.terminate(Terminator::Jump(merge_block));
    } else {
        builder.discard_scope();
    }

    // --- short-circuit block: yield the constant that the LHS already determined ---
    builder.switch_to(short_block);
    // `false && _` → false; `true || _` → true.
    let short_val = builder.const_temp(Const::Bool(matches!(op, BinOp::Or)));
    incomings.push((short_val, builder.current_block));
    builder.terminate(Terminator::Jump(merge_block));

    builder.switch_to(merge_block);
    builder.emit(Instruction::Phi {
        dst: result_dst,
        ty: Type::Bool,
        incomings,
    });
    result_dst
}

// -------------------------------------------------------------------------
// Union-variant discrimination (perf, closed-concrete-union fast path)
//
// `is V` over an object type normally lowers to a RECURSIVE `MatchesSchema`
// runtime call that re-validates every field of V (and nested types). That is
// REDUNDANT precisely when the scrutinee's STATIC TYPE is a *closed concrete
// union* — `Type::Union(variants)` (optionally with Null) where every non-Null
// variant resolves to a concrete `Type::Object` containing NO TypeVar/Json
// anywhere. In that case the value is already type-guaranteed to conform to
// exactly one variant, so `is V` only needs to DISTINGUISH V from its siblings,
// not re-validate V's fields.
//
// SOUNDNESS: this is sound IFF the scrutinee really is such a closed concrete
// union. For `Json`/`TypeVar`/any-typevar/open-or-non-object unions, the full
// `MatchesSchema` is mandatory (ADR-036 narrowing depends on it). When in doubt
// we return `None` and the caller emits the existing `MatchesSchema`.
// -------------------------------------------------------------------------

/// A proven-minimal discriminator that uniquely selects a target variant among
/// the sibling variants of a closed concrete union.
///
/// NOTE: ONLY a `StrLit` value comparison is sound here. Field-PRESENCE is NOT,
/// because Lin objects are structurally width-subtyped: a value conforming to a
/// SIBLING variant may legally carry the discriminating field as an EXTRA field
/// (verified: a `{kind,op,left,right,value}` literal type-checks as the BinOp
/// variant of `Num | BinOp`). Presence-of-`value` would then misclassify that
/// BinOp value as Num and the narrowed arm would read its `value` at the wrong
/// type — the exact silent-wrong-field-read the recursive `MatchesSchema` exists
/// to prevent. A StrLit discriminant forces the variants to be genuinely DISJOINT
/// (no value can carry two distinct StrLit values at the same key), so a value in
/// the union with `key == value` cannot inhabit any sibling — making the cheap
/// equality test exactly equivalent to the recursive validation. See report.
pub enum Discriminator {
    /// `scrut[key] == value` — the target's `key` field is a `StrLit(value)` and
    /// every sibling's `key` is a StrLit with a DISTINCT value.
    StrLit { key: String, value: String },
}

/// Resolve a (possibly `Named`) type to its concrete `Object` field map using
/// the resolved `named_defs` bodies carried by `TypeCheckDeep`. Returns `None`
/// for any non-object, or a Named not in `named_defs`.
pub(crate) fn resolve_object_fields<'a>(
    ty: &'a Type,
    named_defs: &'a [(String, Type)],
) -> Option<&'a indexmap::IndexMap<String, Type>> {
    resolve_object_fields_bounded(ty, named_defs, 0)
}

pub(crate) fn resolve_object_fields_bounded<'a>(
    ty: &'a Type,
    named_defs: &'a [(String, Type)],
    depth: usize,
) -> Option<&'a indexmap::IndexMap<String, Type>> {
    // A bare alias chain `A = B = …` is finite; cap depth to defend against a
    // pathological cyclic alias (`A = B; B = A`) rather than recursing forever.
    if depth > 64 {
        return None;
    }
    match ty {
        Type::Object { fields, .. } => Some(fields),
        Type::Named(n) => {
            let body = &named_defs.iter().find(|(k, _)| k == n)?.1;
            resolve_object_fields_bounded(body, named_defs, depth + 1)
        }
        _ => None,
    }
}

/// Unfold a `Named` alias chain to the type it points at (typically a `Union`),
/// using the resolved `named_defs` bodies. Returns the input unchanged for any
/// non-`Named` type or an unresolvable/cyclic `Named`. Bounded against alias
/// cycles.
pub(crate) fn resolve_union_alias<'a>(ty: &'a Type, named_defs: &'a [(String, Type)], depth: usize) -> &'a Type {
    if depth > 64 {
        return ty;
    }
    match ty {
        Type::Named(n) => match named_defs.iter().find(|(k, _)| k == n) {
            Some((_, body)) => resolve_union_alias(body, named_defs, depth + 1),
            None => ty,
        },
        _ => ty,
    }
}

/// Resolve `ty` through `Named` bodies for a contains-type-var check. Returns
/// `true` if the resolved type contains any TypeVar/Json (so NOT fast-path-safe),
/// or references a `Named` we cannot resolve (conservative). `seen` guards against
/// cyclic recursive types (e.g. `Expr = Num | Add` where `Add` references `Expr`):
/// a `Named` already on the resolution stack is a safe cycle, not a TypeVar.
pub(crate) fn variant_has_type_var(ty: &Type, named_defs: &[(String, Type)], seen: &mut Vec<String>) -> bool {
    match ty {
        Type::Named(n) => {
            if seen.contains(n) {
                // Cyclic recursive reference — already being resolved; not a TypeVar.
                return false;
            }
            match named_defs.iter().find(|(k, _)| k == n) {
                Some((_, body)) => {
                    seen.push(n.clone());
                    let r = variant_has_type_var(body, named_defs, seen);
                    seen.pop();
                    r
                }
                // A Named we couldn't resolve — be conservative.
                None => true,
            }
        }
        Type::Object { fields, .. } => fields.values().any(|t| variant_has_type_var(t, named_defs, seen)),
        Type::Array(inner) | Type::Iterator(inner) | Type::Stream(inner) => {
            variant_has_type_var(inner, named_defs, seen)
        }
        Type::FixedArray(elems) => elems.iter().any(|t| variant_has_type_var(t, named_defs, seen)),
        Type::Union(vs) => vs.iter().any(|t| variant_has_type_var(t, named_defs, seen)),
        Type::Function { params, ret, .. } => {
            params.iter().any(|t| variant_has_type_var(t, named_defs, seen))
                || variant_has_type_var(ret, named_defs, seen)
        }
        Type::TypeVar(_) => true,
        _ => false,
    }
}

/// Decide whether `is target` over a scrutinee of static type `scrut_ty` may use
/// the cheap discriminator fast path, and if so which discriminator. Returns
/// `None` (→ caller emits `MatchesSchema`) whenever the fast path is not PROVEN
/// sound or no single-field-class discriminator uniquely selects `target`.
pub(crate) fn union_discriminator(
    scrut_ty: &Type,
    target: &Type,
    named_defs: &[(String, Type)],
) -> Option<Discriminator> {
    // The scrutinee must be a (closed) union — possibly behind a `Named` alias
    // (e.g. `type Ast = Num | BinOp`; `e: Ast` carries `Type::Named("Ast")`).
    // Unfold the alias through `named_defs` to find the underlying union.
    let resolved_scrut = resolve_union_alias(scrut_ty, named_defs, 0);
    // Strip a Null member.
    let variants: Vec<&Type> = match resolved_scrut {
        Type::Union(vs) => vs.iter().filter(|v| !matches!(v, Type::Null)).collect(),
        _ => return None,
    };
    if variants.len() < 2 {
        return None;
    }
    // EVERY non-Null variant must resolve to a concrete object with no TypeVar.
    let mut resolved: Vec<&indexmap::IndexMap<String, Type>> = Vec::with_capacity(variants.len());
    for v in &variants {
        if variant_has_type_var(v, named_defs, &mut Vec::new()) {
            return None;
        }
        match resolve_object_fields(v, named_defs) {
            Some(fields) if !fields.is_empty() => resolved.push(fields),
            // A non-object variant (or empty object) means we cannot prove the
            // value already conforms to an object-shaped variant → keep full.
            _ => return None,
        }
    }
    // The target must be a concrete object.
    if variant_has_type_var(target, named_defs, &mut Vec::new()) {
        return None;
    }
    let target_fields = resolve_object_fields(target, named_defs)?;

    // The ONLY sound discriminator: a field F where the target's F is a `StrLit`
    // and EVERY variant of the union carries F as a `StrLit`, with EXACTLY ONE
    // variant's F equal to the target's value (the rest distinct). Requiring a
    // distinct StrLit on every variant at F makes the variants genuinely disjoint
    // there, so `scrut[F] == tval` ⇔ "conforms to the target". (This is identified
    // purely by the discriminant VALUES, independent of how the variants' other
    // fields are represented — `Named("Expr")` vs an expanded `Union` for a
    // recursive type — so a recursive AST union is handled correctly.)
    //
    // NOTE: field-PRESENCE is deliberately NOT a discriminator here: Lin objects
    // are structurally width-subtyped, so a value conforming to a sibling variant
    // may legally carry the discriminating field as an EXTRA field, which would
    // misclassify it. Only a value-equality on a StrLit field is sound.
    'keys: for (key, tty) in target_fields.iter() {
        let Type::StrLit(tval) = tty else { continue };
        let mut eq_count = 0usize;
        for var in &resolved {
            match var.get(key) {
                Some(Type::StrLit(sval)) => {
                    if sval == tval {
                        eq_count += 1;
                    }
                }
                // A variant lacks the key, or carries a non-StrLit (base String)
                // there: `scrut[key] == tval` is not provably exclusive of it.
                _ => continue 'keys,
            }
        }
        if eq_count == 1 {
            return Some(Discriminator::StrLit { key: key.clone(), value: tval.clone() });
        }
    }

    // No SOUND discriminator. Fall back to the full recursive `MatchesSchema`.
    None
}

/// Emit the IR for a chosen `Discriminator` over the boxed scrutinee `scrut`,
/// returning the Bool result temp. Reuses the existing `Index`+`Eq` machinery —
/// no new instructions.
pub(crate) fn emit_discriminator(
    disc: &Discriminator,
    scrut: Temp,
    scrut_ty: &Type,
    builder: &mut FuncBuilder,
) -> Temp {
    match disc {
        Discriminator::StrLit { key, value } => {
            // UNBOXED SUM TYPE (unboxed-sumtype Stage 1): when the scrutinee's static type is a
            // Stage-1-eligible sum type, it is physically a `SumNode` (the seed). Replace the boxed
            // `scrut[disc] == "value"` (materialize + object_get + string-eq) with a single inline-tag
            // compare (`SumTagEq`) — the O(1) dispatch the unboxed representation exists for. The
            // resolved scrutinee type may be behind a `Named` alias; `crate::repr::sum_type_eligible`
            // only matches a bare `Union`, so unfold one level via the discriminator's own key check.
            let sum_view = if crate::repr::sum_type_eligible(scrut_ty) {
                Some(scrut_ty.clone())
            } else {
                None
            };
            if let Some(sum_ty) = sum_view {
                let dst = builder.alloc_temp(Type::Bool);
                builder.emit(Instruction::SumTagEq {
                    dst,
                    val: scrut,
                    sum_ty,
                    disc_value: value.clone(),
                });
                return dst;
            }
            builder.push_scope();
            // got = scrut[key]
            let key_temp = builder.const_temp(Const::Str(key.clone()));
            let got = builder.alloc_temp(Type::TypeVar(u32::MAX));
            builder.emit(Instruction::Index {
                dst: got,
                object: scrut,
                key: key_temp,
                obj_ty: scrut_ty.clone(),
                key_ty: Type::Str,
                result_ty: Type::TypeVar(u32::MAX),
            });
            // lit = box("value")
            let lit_raw = builder.const_temp(Const::Str(value.clone()));
            let lit = box_to_json(lit_raw, &Type::Str, builder);
            let eq = builder.alloc_temp(Type::Bool);
            builder.emit(Instruction::Binary {
                dst: eq,
                op: BinOp::Eq,
                lhs: got,
                rhs: lit,
                operand_ty: Type::TypeVar(u32::MAX),
                ty: Type::Bool,
            });
            // `eq` is a Bool (not RC) so it survives; transient RC temps (fetched
            // field, boxed literal) are released in this scope.
            builder.pop_scope_releasing(Temp(u32::MAX));
            eq
        }
    }
}

// -------------------------------------------------------------------------
// Match lowering
// -------------------------------------------------------------------------

pub(crate) fn lower_match(
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
        // Tag the arm body block with its source span for coverage. next/nofall blocks
        // cover no distinct source region and stay None.
        builder.set_block_span(body_block, arm.body.span());
        let next_block = if is_last {
            // Last arm: no fallthrough needed (compiler ensures exhaustiveness).
            builder.alloc_block("arm_nofall")
        } else {
            builder.alloc_block(format!("arm_{}_next", i))
        };

        // Test the pattern. `scrut_ty` is threaded so a closed-concrete-union
        // scrutinee can take the cheap discriminator fast path for an `is V` arm.
        let matched = lower_match_pattern(&arm.pattern, scrut_temp, &scrut_ty, &arm.body, builder, ctx);

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
            // The guard-ok block is reached only when the guard expression evaluated true,
            // so it is a distinct coverage region. guard_fail stays None.
            builder.set_block_span(guard_then, guard.span());
            builder.terminate(Terminator::CondJump {
                cond: guard_val,
                then_block: guard_then,
                else_block: guard_fail,
            });
            builder.switch_to(guard_fail);
            builder.terminate(Terminator::Jump(next_block));
            builder.switch_to(guard_then);
        }

        // UNBOXED SUM TYPE (unboxed-sumtype Stage 2): a sum-eligible object LITERAL arm body
        // flowing into a sum-typed match result is constructed DIRECTLY as a SumNode (with the
        // recursive-child pushdown), mirroring the `if`-branch path — skip the
        // build-boxed-then-project round-trip that mis-tags recursive children.
        let (arm_raw, arm_ty) = match try_lower_sum_literal(&arm.body, result_type, builder, ctx) {
            Some(t) => (t, result_type.clone()),
            None => (lower_expr(&arm.body, builder, ctx), arm.body.ty()),
        };
        if !builder.is_current_block_terminated() {
            let arm_val = coerce_to_slot_type(arm_raw, &arm_ty, result_type, builder);
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
    // Only CONCRETE rc merge results are owned (see lower_if): a boxed-union match-result may be
    // a borrowed arm value (carrying no +1), so registering+releasing it would double-free.
    if is_rc_type(result_type) {
        builder.register_owned(result_dst, result_type.clone());
    }
    result_dst
}

pub enum PatternTest {
    Always,
    Cond(Temp),
}

pub(crate) fn lower_match_pattern(
    pattern: &TypedMatchPattern,
    scrut: Temp,
    scrut_ty: &Type,
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
        // `is <name>` (a binding) and `is _` (wildcard) match ANY value unconditionally —
        // they are named/anonymous catch-alls, not type checks. The generic arm below would
        // call pattern_type_check, which returns the binding's declared type (= the
        // scrutinee's static type, often Json) and emit an `IsType` tag check that can fail
        // for a concrete value inside a Json scrutinee (e.g. `match req["path"] is p when …`
        // never matched). Bindings always match; the value is bound in lower_match_bindings.
        TypedMatchPattern::Is(TypedPattern::Binding(..))
        | TypedMatchPattern::Is(TypedPattern::Wildcard(..)) => PatternTest::Always,
        // `is <Named>` where the name resolves to a non-empty object shape (a user object-type
        // alias like `Person`): a bare tag check (or the mere field-presence the earlier rule
        // folded into ADR-036 checked) matches
        // objects with the WRONG field types, which is unsound once the arm narrows the binding.
        // Deep-validate field types recursively via the `fromJson` structural walker (ADR-036).
        // `scrut` is the already-boxed scrutinee; `MatchesSchema` borrows it (no ownership change).
        TypedMatchPattern::Is(TypedPattern::TypeCheckDeep(target, named_defs, _)) => {
            // FAST PATH: when the scrutinee's static type is a closed concrete union
            // the value is type-guaranteed to conform to exactly one variant, so a
            // cheap discriminator (StrLit field value / field presence) suffices to
            // SELECT V — the recursive `MatchesSchema` re-validation is redundant.
            // Falls back to `MatchesSchema` whenever the fast path isn't proven sound.
            if let Some(disc) = union_discriminator(scrut_ty, target, named_defs) {
                let cond = emit_discriminator(&disc, scrut, scrut_ty, builder);
                return PatternTest::Cond(cond);
            }
            let dst = builder.alloc_temp(Type::Bool);
            builder.emit(Instruction::MatchesSchema {
                dst,
                val: scrut,
                target: target.clone(),
                named_defs: named_defs.clone(),
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
        TypedMatchPattern::Has(tp) => lower_object_pattern_test(tp, scrut, builder, ctx),
    }
}

/// Lower an object pattern test (`is`/`has { k: v, .. }`): the scrutinee must be an object
/// that HAS the listed fields, with each value-constrained field equal to its literal. Used
/// by both `Is(Object)` and `Has(Object)` — for an object shape check the two are equivalent
/// (tag-is-object + required fields + value constraints).
pub(crate) fn lower_object_pattern_test(
    tp: &TypedPattern,
    scrut: Temp,
    builder: &mut FuncBuilder,
    ctx: &mut LowerCtx,
) -> PatternTest {
    // Heap-field SumNode Stage 3: `HasPattern` (→ `lin_value_has_field`) checks the tag byte at
    // offset 0 of the pointer, expecting a TaggedVal. A raw `*SumNode` has its RC field at offset 0
    // (not a tag byte), so `lin_value_has_field` misidentifies it and returns false for every field.
    // When the scrutinee is a SumNode-eligible union, coerce it to Json (materialize to a boxed
    // `LinMap`, TAG_MAP) first, so `HasPattern` and the discriminant `Index` value-constraints see a
    // valid tagged object. The materialized pointer is registered as owned and released at scope end.
    let json_ty = Type::TypeVar(u32::MAX);
    builder.push_scope();
    let actual_scrut = {
        let scrut_ty = builder.temp_types.get(&scrut).cloned().unwrap_or(json_ty.clone());
        if crate::repr::sum_type_eligible(&scrut_ty) {
            let materialized = builder.alloc_temp(json_ty.clone());
            builder.emit(Instruction::Coerce {
                dst: materialized, src: scrut, from_ty: scrut_ty, to_ty: json_ty.clone(),
            });
            builder.register_owned(materialized, json_ty.clone());
            materialized
        } else {
            scrut
        }
    };
    let required_fields = pattern_required_fields(tp);
    let mut cond = builder.alloc_temp(Type::Bool);
    builder.emit(Instruction::HasPattern {
        dst: cond,
        val: actual_scrut,
        pattern: HasDesc { required_fields },
    });
    // For object fields with a value constraint (e.g. `{ "type": "success" }`), also require
    // scrut[key] == literal, AND-ed into the condition. The transient comparison temps (boxed
    // literal, fetched field) are scoped so they're released in THIS test block — not at the
    // enclosing scope exit, which a per-arm test block does not dominate.
    if let TypedPattern::Object { fields, .. } = tp {
        let obj_ty = builder.temp_types.get(&actual_scrut).cloned().unwrap_or(json_ty.clone());
        for field in fields {
            if let Some(vp) = &field.value_pattern {
                let lit_ty = vp.ty();
                let lit_raw = lower_expr(vp, builder, ctx);
                let lit = box_to_json(lit_raw, &lit_ty, builder);
                // got = actual_scrut[key]
                let key_temp = builder.const_temp(Const::Str(field.key.clone()));
                let got = builder.alloc_temp(json_ty.clone());
                builder.emit(Instruction::Index {
                    dst: got, object: actual_scrut, key: key_temp,
                    obj_ty: obj_ty.clone(), key_ty: Type::Str, result_ty: json_ty.clone(),
                });
                let eq = builder.alloc_temp(Type::Bool);
                builder.emit(Instruction::Binary {
                    dst: eq, op: BinOp::Eq, lhs: got, rhs: lit,
                    operand_ty: json_ty.clone(), ty: Type::Bool,
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
    // `cond` is a Bool (not RC), so it survives the scope pop. The transient RC temps
    // (literal strings, fetched fields, and the materialized SumNode object if any) are released.
    builder.pop_scope_releasing(Temp(u32::MAX));
    PatternTest::Cond(cond)
}

/// After a pattern test succeeds, bind pattern variables into slots.
pub(crate) fn lower_match_bindings(
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

pub(crate) fn lower_typed_pattern_bindings(
    pattern: &TypedPattern,
    scrut: Temp,
    builder: &mut FuncBuilder,
) {
    match pattern {
        TypedPattern::Binding(slot, ty, _) => {
            // The match scrutinee is boxed to Json/union (`box_to_json` at match entry).
            // If this binding has a CONCRETE type (e.g. `is n` where n: Int32), binding it
            // directly to the boxed pointer would later reinterpret the pointer as the
            // scalar (ptrtoint) — so a guard like `when n > 5` compares a heap address, not
            // the value, and is effectively always true. Unbox via Coerce when the
            // scrutinee is boxed but the binding is concrete; a plain Bind (alias) is
            // correct when types already match (e.g. a Json scrutinee bound to Json).
            let scrut_ty = builder.temp_types.get(&scrut).cloned().unwrap_or(Type::TypeVar(u32::MAX));
            let t = builder.alloc_temp(ty.clone());
            if is_union_ty(&scrut_ty) && !is_union_ty(ty) {
                builder.emit(Instruction::Coerce {
                    dst: t, src: scrut, from_ty: scrut_ty, to_ty: ty.clone(),
                });
                // Unboxing (Coerce) does NOT add a reference — the narrowed concrete value aliases
                // the scrutinee box's inner payload. For a CONCRETE RC binding (an Object/Array/Map/
                // String narrowed out of a `T | Null` union, e.g. `match trip is Trip => trip`), the
                // binding must own its own reference: otherwise passing it on (e.g. storing it into a
                // container that later releases it) over-decrements the shared payload while the
                // union's true owner still holds it — a use-after-free. Mirror the global narrowed
                // read at lower.rs ~2891: retain the inner in place + register owned so it is freed
                // at scope exit. (`own_for_read` is a no-op for scalars, so the `is n: Int32` case is
                // unaffected.)
                let owned = own_for_read(t, ty, builder);
                builder.slots.insert(*slot, owned);
                return;
            } else {
                builder.emit(Instruction::Bind { dst: t, src: scrut, ty: ty.clone() });
            }
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
        TypedPattern::TypeCheck(_, _)
        | TypedPattern::TypeCheckDeep(_, _, _)
        | TypedPattern::Literal(_)
        | TypedPattern::Wildcard(_) => {
            // No bindings.
        }
    }
}

pub(crate) fn pattern_elem_type(pattern: &TypedPattern) -> Type {
    match pattern {
        TypedPattern::Binding(_, ty, _) => ty.clone(),
        TypedPattern::TypeCheck(ty, _) => ty.clone(),
        TypedPattern::TypeCheckDeep(ty, _, _) => ty.clone(),
        _ => Type::Null,
    }
}

