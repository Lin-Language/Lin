//! Box/unbox cancellation peephole pass for LinIR.
//!
//! Cancels `unbox(box(x))` pairs (Direction A only) within a single basic block
//! when:
//!
//! 1. The intermediate boxed value flows from the box directly into the unbox
//!    with no other use (single-use def).
//! 2. The boxed-from and unboxed-to types are **scalars** (Int32, Int64, Bool,
//!    Float64, etc.) — no heap payload, no RC changes. A heap box
//!    (Str/Array/Object) must NOT be cancelled: the box owns a reference and
//!    dropping it without a matching release would corrupt the refcount.
//! 3. The unboxed result type matches the original boxed-from type (same repr).
//!    A genuine type change (Int32 boxed then read as Float64) must NOT cancel.
//!
//! Handles two instruction forms:
//! - `Coerce { from_ty: T, to_ty: Union }` → `Coerce { from_ty: Union, to_ty: T }`
//! - `Box { ty: T, val: x }` → `Unbox { result_ty: T, val: box_dst }`
//!
//! The canonical representation after the pass: the box and unbox disappear and
//! the consuming instruction reads the original scalar temp directly (via a
//! `Copy` → `dst_unbox = src`).
//!
//! # Why only Direction A?
//!
//! Direction B (`box(unbox(x)) → x`) is NOT implemented. The re-box allocates a
//! fresh TaggedVal* that the caller may own and free (e.g. via
//! `lin_tagged_free_box_if_distinct`). Replacing that fresh allocation with an
//! alias to the original union value would cause a double-free when both the alias
//! and the original are released. Proving it safe requires full escape/ownership
//! analysis — out of scope for this peephole.
//!
//! # Soundness
//!
//! This pass only fires on non-escaping, non-RC scalar types. Missing a
//! cancellation is always safe (the instructions are left in place, no behaviour
//! change). A false cancellation on a heap type would be a use-after-free — the
//! scalar guard prevents it. The single-use check prevents cancellation when the
//! box result is also read by another consumer.

use std::collections::HashMap;

use lin_check::types::Type;

use crate::ir::*;

/// Run the box/unbox cancellation pass on every function in the module.
pub fn elide_box_unbox(module: &mut LinModule) {
    for func in &mut module.functions {
        elide_in_function(func);
    }
}

fn elide_in_function(func: &mut LinFunction) {
    for block in &mut func.blocks {
        elide_in_block(block);
    }
}

/// True for scalar types that carry no heap payload and need no RC operations.
/// A box of one of these into a union/TaggedVal is purely a value copy: cancelling
/// it is safe. Heap types (Str, Array, Object, etc.) are excluded.
fn is_scalar_cancellable(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Bool
            | Type::Int8
            | Type::Int16
            | Type::Int32
            | Type::UInt8
            | Type::UInt16
            | Type::UInt32
            | Type::Int64
            | Type::UInt64
            | Type::Float32
            | Type::Float64
            | Type::IntLit(_)
            | Type::Null
    )
}

/// True when `ty` is a union / TaggedVal boundary (the "boxed" representation).
fn is_union_boundary(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Union(_) | Type::TypeVar(_)
    )
}

/// Canonical scalar identity for cancellation matching.
/// Two types are repr-identical for cancellation iff their canonical repr matches.
/// We map narrow integer variants to their box-time type (Int8/Int16 → Int32, etc.)
/// so `box(i8 x as union) → unbox(union as i8)` also cancels.
fn canonical_scalar(ty: &Type) -> Option<&'static str> {
    match ty {
        Type::Bool => Some("bool"),
        Type::Int8 | Type::Int16 | Type::Int32 | Type::IntLit(_) => Some("i32"),
        Type::UInt8 | Type::UInt16 | Type::UInt32 => Some("u64"),
        Type::Int64 => Some("i64"),
        Type::UInt64 => Some("u64"),
        Type::Float32 | Type::Float64 => Some("f64"),
        Type::Null => Some("null"),
        _ => None,
    }
}

fn uses_of(instr: &Instruction, t: Temp) -> usize {
    // Helper: count occurrences of `t` in a slice of temps.
    let count_in = |ts: &[Temp]| ts.iter().filter(|&&x| x == t).count();

    match instr {
        Instruction::Copy { src, .. } => usize::from(*src == t),
        Instruction::Coerce { src, .. } => usize::from(*src == t),
        Instruction::Box { val, .. } => usize::from(*val == t),
        Instruction::Unbox { val, .. } => usize::from(*val == t),
        Instruction::Unary { operand, .. } => usize::from(*operand == t),
        Instruction::Binary { lhs, rhs, .. } => usize::from(*lhs == t) + usize::from(*rhs == t),
        Instruction::Call { callee, args, .. } => {
            let callee_use = match callee {
                CallTarget::Indirect(c) => usize::from(*c == t),
                _ => 0,
            };
            callee_use + count_in(args)
        }
        Instruction::CallIntrinsic { args, .. } => count_in(args),
        Instruction::MakeClosure { captures, .. } => count_in(captures),
        Instruction::MakeObject { fields, spreads, computed_fields, .. } => {
            let f: usize = fields.iter().map(|(_, v)| usize::from(*v == t)).sum();
            let s = count_in(spreads);
            let c: usize = computed_fields.iter().map(|(k, v)| usize::from(*k == t) + usize::from(*v == t)).sum();
            f + s + c
        }
        Instruction::MakeArray { elements, spreads, .. } => {
            let e = count_in(elements);
            let s: usize = spreads.iter().map(|(_, v)| usize::from(*v == t)).sum();
            e + s
        }
        Instruction::Index { object, key, .. } => usize::from(*object == t) + usize::from(*key == t),
        Instruction::IndexSet { object, key, value, .. } => {
            usize::from(*object == t) + usize::from(*key == t) + usize::from(*value == t)
        }
        Instruction::FieldGet { object, .. } => usize::from(*object == t),
        Instruction::FieldSet { object, value, .. } => usize::from(*object == t) + usize::from(*value == t),
        Instruction::SealedArrayFieldGet { array, index, .. } => {
            usize::from(*array == t) + usize::from(*index == t)
        }
        Instruction::BoxedArrayFieldGet { array, index, .. } => {
            usize::from(*array == t) + usize::from(*index == t)
        }
        Instruction::EnvCapture { env, .. } => usize::from(*env == t),
        Instruction::ArrayLenCheck { val, .. } => usize::from(*val == t),
        Instruction::ObjectRest { src, .. } => usize::from(*src == t),
        Instruction::GlobalValSet { value, .. } => usize::from(*value == t),
        Instruction::MakeCell { init, .. } => usize::from(*init == t),
        Instruction::CellGet { cell, .. } => usize::from(*cell == t),
        Instruction::CellSet { cell, value, .. } => usize::from(*cell == t) + usize::from(*value == t),
        Instruction::FreeCell { cell, .. } => usize::from(*cell == t),
        Instruction::Retain { val, .. } | Instruction::Release { val, .. } => usize::from(*val == t),
        Instruction::CloneBox { src, .. } => usize::from(*src == t),
        Instruction::FreeBoxShell { val } => usize::from(*val == t),
        Instruction::FreeBoxShellIfDistinct { val, other } | Instruction::ReleaseIfDistinct { val, other } => {
            usize::from(*val == t) + usize::from(*other == t)
        }
        Instruction::IsType { val, .. } => usize::from(*val == t),
        Instruction::SumTagEq { val, .. } => usize::from(*val == t),
        Instruction::HasPattern { val, .. } => usize::from(*val == t),
        Instruction::MatchesSchema { val, .. } => usize::from(*val == t),
        Instruction::Bind { src, .. } => usize::from(*src == t),
        Instruction::Panic { msg } => usize::from(*msg == t),
        Instruction::Phi { incomings, .. } => incomings.iter().filter(|(v, _)| *v == t).count(),
        // Defs only / pure metadata — no uses.
        Instruction::Const { .. }
        | Instruction::GlobalValGet { .. }
        | Instruction::MakeNamedClosure { .. }
        | Instruction::DebugDeclare { .. } => 0,
    }
}

fn elide_in_block(block: &mut BasicBlock) {
    // Cancel same-block unbox(box(x)) pairs (Direction A only):
    //
    //   i = Coerce(concrete_scalar → Union) [box]   or  Box { ty: scalar }
    //   j = Coerce(Union → concrete_scalar_same) [unbox]  or  Unbox { result_ty: scalar }
    //
    // The intermediate `box_dst` (def at i, used at j) must be used exactly once
    // (by the unbox) so no other consumer depends on the intermediate representation.
    //
    // Safety: only scalar types (no heap payload, no RC). A heap box must never be
    // cancelled because the box owns a reference and dropping it without a matching
    // release would corrupt the refcount. Direction B (box(unbox(x))) is excluded —
    // the fresh re-box allocation may be owned and freed by the caller; replacing it
    // with an alias to the original union value would cause a double-free.

    // First pass: build a use-count map for every temp used as an input in this block.
    let total_uses: HashMap<Temp, usize> = {
        let mut map: HashMap<Temp, usize> = HashMap::new();
        for instr in &block.instructions {
            for u in all_uses(instr) {
                *map.entry(u).or_insert(0) += 1;
            }
        }
        for u in all_term_uses(&block.terminator) {
            *map.entry(u).or_insert(0) += 1;
        }
        map
    };

    // Collected cancels: (remove_idx, replace_idx, new_src, new_dst)
    // `remove_idx` → drop the instruction; `replace_idx` → Copy { dst: new_dst, src: new_src }.
    let mut cancels: Vec<(usize, usize, Temp, Temp)> = Vec::new();

    let instrs = &block.instructions;
    let n = instrs.len();

    // ── Direction A: unbox(box(x)) — scan j as the UNBOX, look backward for BOX ──
    for j in 0..n {
        // j is the UNBOX: Coerce(Union → scalar) or Unbox{…} with scalar result
        let (box_val_temp, unbox_dst, unbox_result_ty) = match &instrs[j] {
            Instruction::Coerce { src, dst, from_ty, to_ty }
                if is_union_boundary(from_ty) && is_scalar_cancellable(to_ty) =>
            {
                (*src, *dst, to_ty.clone())
            }
            Instruction::Unbox { val, dst, result_ty } if is_scalar_cancellable(result_ty) => {
                (*val, *dst, result_ty.clone())
            }
            _ => continue,
        };

        // The boxed intermediate must be used exactly once (by this unbox).
        if total_uses.get(&box_val_temp).copied().unwrap_or(0) != 1 {
            continue;
        }

        // Find the BOX instruction backward: Coerce(scalar → Union) or Box{scalar ty}.
        let box_idx = instrs[..j].iter().rposition(|instr| match instr {
            Instruction::Coerce { dst, from_ty, to_ty, .. }
                if *dst == box_val_temp
                    && is_scalar_cancellable(from_ty)
                    && is_union_boundary(to_ty) => true,
            Instruction::Box { dst, ty, .. } if *dst == box_val_temp && is_scalar_cancellable(ty) => true,
            _ => false,
        });
        let Some(i) = box_idx else { continue };

        let (orig_src, box_from_ty) = match &instrs[i] {
            Instruction::Coerce { src, from_ty, .. } => (*src, from_ty.clone()),
            Instruction::Box { val, ty, .. } => (*val, ty.clone()),
            _ => continue,
        };

        // Types must be repr-identical round-trip.
        if canonical_scalar(&box_from_ty) != canonical_scalar(&unbox_result_ty) {
            continue;
        }
        // No use of box_val_temp between i and j.
        if instrs[i + 1..j].iter().any(|instr| uses_of(instr, box_val_temp) > 0) {
            continue;
        }

        cancels.push((i, j, orig_src, unbox_dst));
    }

    if cancels.is_empty() {
        return;
    }

    // Collect indices to remove and build replacement map: unbox_dst → orig_src.
    let mut remove_indices: std::collections::HashSet<usize> = std::collections::HashSet::new();
    // Map from the unbox dst to the original src: we rewrite the unbox instruction
    // into a Copy so downstream consumers see the value without the boxed intermediate.
    // (Rewriting as Copy is the canonical form for peepholes that forward a value.)
    let mut replacements: Vec<(usize, Instruction, Option<lin_common::Span>)> = Vec::new();

    for (box_idx, unbox_idx, orig_src, unbox_dst) in cancels {
        // Replace the box instruction with a no-op (Bind forwarding to itself, or
        // we just remove it). Remove the unbox and replace with Copy.
        remove_indices.insert(box_idx);
        // Replace the unbox instruction with a Copy.
        let new_span = block.instr_spans.get(unbox_idx).copied().flatten();
        replacements.push((
            unbox_idx,
            Instruction::Copy { dst: unbox_dst, src: orig_src },
            new_span,
        ));
    }

    // Apply: build new instruction + instr_spans lists.
    let old_instrs = std::mem::take(&mut block.instructions);
    let old_spans = std::mem::take(&mut block.instr_spans);

    let mut new_instrs: Vec<Instruction> = Vec::with_capacity(old_instrs.len());
    let mut new_spans: Vec<Option<lin_common::Span>> = Vec::with_capacity(old_spans.len());

    for (idx, (instr, span)) in old_instrs.into_iter().zip(old_spans.into_iter()).enumerate() {
        if remove_indices.contains(&idx) {
            // Drop the box instruction entirely.
            continue;
        }
        if let Some((_, new_instr, _)) = replacements.iter().find(|(i, _, _)| *i == idx) {
            // Replace with Copy.
            new_instrs.push(new_instr.clone());
            new_spans.push(span);
        } else {
            new_instrs.push(instr);
            new_spans.push(span);
        }
    }

    block.instructions = new_instrs;
    block.instr_spans = new_spans;
}

/// Collect all input temps used (not defined) by an instruction.
fn all_uses(instr: &Instruction) -> Vec<Temp> {
    let mut uses = Vec::new();
    match instr {
        Instruction::Copy { src, .. } => uses.push(*src),
        Instruction::Coerce { src, .. } => uses.push(*src),
        Instruction::Box { val, .. } => uses.push(*val),
        Instruction::Unbox { val, .. } => uses.push(*val),
        Instruction::Unary { operand, .. } => uses.push(*operand),
        Instruction::Binary { lhs, rhs, .. } => { uses.push(*lhs); uses.push(*rhs); }
        Instruction::Call { callee, args, .. } => {
            if let CallTarget::Indirect(c) = callee { uses.push(*c); }
            uses.extend_from_slice(args);
        }
        Instruction::CallIntrinsic { args, .. } => uses.extend_from_slice(args),
        Instruction::MakeClosure { captures, .. } => uses.extend_from_slice(captures),
        Instruction::MakeObject { fields, spreads, computed_fields, .. } => {
            for (_, v) in fields { uses.push(*v); }
            uses.extend_from_slice(spreads);
            for (k, v) in computed_fields { uses.push(*k); uses.push(*v); }
        }
        Instruction::MakeArray { elements, spreads, .. } => {
            uses.extend_from_slice(elements);
            for (_, v) in spreads { uses.push(*v); }
        }
        Instruction::Index { object, key, .. } => { uses.push(*object); uses.push(*key); }
        Instruction::IndexSet { object, key, value, .. } => {
            uses.push(*object); uses.push(*key); uses.push(*value);
        }
        Instruction::FieldGet { object, .. } => uses.push(*object),
        Instruction::FieldSet { object, value, .. } => { uses.push(*object); uses.push(*value); }
        Instruction::SealedArrayFieldGet { array, index, .. } => { uses.push(*array); uses.push(*index); }
        Instruction::BoxedArrayFieldGet { array, index, .. } => { uses.push(*array); uses.push(*index); }
        Instruction::EnvCapture { env, .. } => uses.push(*env),
        Instruction::ArrayLenCheck { val, .. } => uses.push(*val),
        Instruction::ObjectRest { src, .. } => uses.push(*src),
        Instruction::GlobalValSet { value, .. } => uses.push(*value),
        Instruction::MakeCell { init, .. } => uses.push(*init),
        Instruction::CellGet { cell, .. } => uses.push(*cell),
        Instruction::CellSet { cell, value, .. } => { uses.push(*cell); uses.push(*value); }
        Instruction::FreeCell { cell, .. } => uses.push(*cell),
        Instruction::Retain { val, .. } | Instruction::Release { val, .. } => uses.push(*val),
        Instruction::CloneBox { src, .. } => uses.push(*src),
        Instruction::FreeBoxShell { val } => uses.push(*val),
        Instruction::FreeBoxShellIfDistinct { val, other } | Instruction::ReleaseIfDistinct { val, other } => {
            uses.push(*val); uses.push(*other);
        }
        Instruction::IsType { val, .. } => uses.push(*val),
        Instruction::SumTagEq { val, .. } => uses.push(*val),
        Instruction::HasPattern { val, .. } => uses.push(*val),
        Instruction::MatchesSchema { val, .. } => uses.push(*val),
        Instruction::Bind { src, .. } => uses.push(*src),
        Instruction::Panic { msg } => uses.push(*msg),
        Instruction::Phi { incomings, .. } => {
            for (v, _) in incomings { uses.push(*v); }
        }
        Instruction::Const { .. }
        | Instruction::GlobalValGet { .. }
        | Instruction::MakeNamedClosure { .. }
        | Instruction::DebugDeclare { .. } => {}
    }
    uses
}

fn all_term_uses(term: &Terminator) -> Vec<Temp> {
    match term {
        Terminator::Return(Some(v)) => vec![*v],
        Terminator::CondJump { cond, .. } => vec![*cond],
        Terminator::Switch { val, .. } => vec![*val],
        Terminator::TailCall { args } => args.clone(),
        Terminator::Return(None) | Terminator::Jump(_) | Terminator::Unreachable => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lin_check::types::Type;

    fn make_block(id: u32, instructions: Vec<Instruction>, term: Terminator) -> BasicBlock {
        let spans = vec![None; instructions.len()];
        BasicBlock {
            id: BlockId(id),
            label: None,
            instructions,
            terminator: term,
            span: None,
            instr_spans: spans,
        }
    }

    fn make_func(blocks: Vec<BasicBlock>) -> LinFunction {
        let mut temp_types = std::collections::HashMap::new();
        temp_types.insert(Temp(0), Type::Int32);
        temp_types.insert(Temp(1), Type::Union(vec![Type::Int32, Type::Null]));
        temp_types.insert(Temp(2), Type::Int32);
        LinFunction {
            id: FuncId(0),
            name: Some("test".to_string()),
            params: vec![],
            is_closure: false,
            ret_ty: Type::Int32,
            param_conventions: vec![],
            ret_convention: Convention::Own,
            blocks,
            temp_types,
            temp_count: 3,
            intrinsic_slots: std::collections::HashMap::new(),
            repr: vec![],
            coverage_origin: None,
        }
    }

    fn union_ty() -> Type {
        Type::Union(vec![Type::Int32, Type::Null])
    }

    /// Box(Int32) → Unbox(→ Int32): the pair must cancel to Copy.
    #[test]
    fn test_box_unbox_int32_cancels() {
        let instructions = vec![
            // t0 = arg (Int32) — represented as a Const here.
            Instruction::Const { dst: Temp(0), val: Const::Int(42, Type::Int32) },
            // t1 = box(t0 as Int32 → union)
            Instruction::Coerce { dst: Temp(1), src: Temp(0), from_ty: Type::Int32, to_ty: union_ty() },
            // t2 = unbox(t1 → Int32)
            Instruction::Coerce { dst: Temp(2), src: Temp(1), from_ty: union_ty(), to_ty: Type::Int32 },
        ];
        let block = make_block(0, instructions, Terminator::Return(Some(Temp(2))));
        let mut func = make_func(vec![block]);
        elide_in_function(&mut func);

        // After elision: the box (index 1) is removed; the unbox (was index 2) is
        // replaced with Copy { dst: t2, src: t0 }. So we have 2 instructions.
        let blk = &func.blocks[0];
        assert_eq!(blk.instructions.len(), 2, "box+unbox should collapse to const+copy");
        assert!(
            matches!(&blk.instructions[1], Instruction::Copy { dst: Temp(2), src: Temp(0) }),
            "unbox should become Copy(t2 ← t0), got {:?}", &blk.instructions[1]
        );
    }

    /// Box(Int32) → used in TWO places → must NOT cancel.
    #[test]
    fn test_box_multi_use_not_cancelled() {
        let instructions = vec![
            Instruction::Const { dst: Temp(0), val: Const::Int(1, Type::Int32) },
            // t1 = box(t0)
            Instruction::Coerce { dst: Temp(1), src: Temp(0), from_ty: Type::Int32, to_ty: union_ty() },
            // t2 = unbox(t1) — one use of t1
            Instruction::Coerce { dst: Temp(2), src: Temp(1), from_ty: union_ty(), to_ty: Type::Int32 },
            // t3 (use t1 again) — second use → cancel must NOT fire
            Instruction::Bind { dst: Temp(3), src: Temp(1), ty: union_ty() },
        ];
        let block = make_block(0, instructions, Terminator::Return(Some(Temp(2))));
        let mut func = make_func(vec![block]);
        elide_in_function(&mut func);

        // No change: t1 has 2 uses.
        assert_eq!(func.blocks[0].instructions.len(), 4, "multi-use box must not cancel");
    }

    /// Box(Int32 → Union) → unbox → Float64: repr mismatch, must NOT cancel.
    #[test]
    fn test_type_mismatch_not_cancelled() {
        let instructions = vec![
            Instruction::Const { dst: Temp(0), val: Const::Int(1, Type::Int32) },
            Instruction::Coerce { dst: Temp(1), src: Temp(0), from_ty: Type::Int32, to_ty: union_ty() },
            // unbox to Float64 — different repr, MUST NOT cancel
            Instruction::Coerce { dst: Temp(2), src: Temp(1), from_ty: union_ty(), to_ty: Type::Float64 },
        ];
        let block = make_block(0, instructions, Terminator::Return(Some(Temp(2))));
        let mut func = make_func(vec![block]);
        elide_in_function(&mut func);

        assert_eq!(func.blocks[0].instructions.len(), 3, "type mismatch must not cancel");
    }

    /// Box/Unbox instruction form (not Coerce): same round-trip, must cancel.
    #[test]
    fn test_box_unbox_instrs_cancel() {
        let instructions = vec![
            Instruction::Const { dst: Temp(0), val: Const::Int(5, Type::Int32) },
            Instruction::Box { dst: Temp(1), val: Temp(0), ty: Type::Int32 },
            Instruction::Unbox { dst: Temp(2), val: Temp(1), result_ty: Type::Int32 },
        ];
        let block = make_block(0, instructions, Terminator::Return(Some(Temp(2))));
        let mut func = make_func(vec![block]);
        elide_in_function(&mut func);

        let blk = &func.blocks[0];
        assert_eq!(blk.instructions.len(), 2, "Box+Unbox should collapse");
        assert!(
            matches!(&blk.instructions[1], Instruction::Copy { dst: Temp(2), src: Temp(0) }),
            "Unbox should become Copy, got {:?}", &blk.instructions[1]
        );
    }

    /// Direction B (box(unbox(x))) is NOT cancelled — the rebox allocates a fresh
    /// TaggedVal* that the caller may own/free; aliasing it to the original union
    /// value would cause a double-free. Verify the pair is left unchanged.
    #[test]
    fn test_unbox_rebox_not_cancelled() {
        let instructions = vec![
            Instruction::Bind { dst: Temp(0), src: Temp(0), ty: union_ty() },
            // t1 = unbox t0 → Int32
            Instruction::Coerce { dst: Temp(1), src: Temp(0), from_ty: union_ty(), to_ty: Type::Int32 },
            // t2 = rebox t1 → union
            Instruction::Coerce { dst: Temp(2), src: Temp(1), from_ty: Type::Int32, to_ty: union_ty() },
        ];
        let block = make_block(0, instructions, Terminator::Return(Some(Temp(2))));
        let mut func = make_func(vec![block]);
        elide_in_function(&mut func);

        // Direction B is not implemented: the pair must be left unchanged.
        assert_eq!(func.blocks[0].instructions.len(), 3, "unbox+rebox must NOT cancel (Direction B disabled)");
    }

    /// Negative case: the intermediate boxed value escapes (is also passed to a call).
    /// Must NOT cancel even though directions match.
    #[test]
    fn test_escape_not_cancelled() {
        // t0 = Int32 param
        // t1 = box t0 (Int32 → union) — ALSO used as a call arg below
        // t2 = unbox t1 (union → Int32) — single unbox, but t1 has 2 uses total
        // call foo(t1)
        let instructions = vec![
            Instruction::Const { dst: Temp(0), val: Const::Int(7, Type::Int32) },
            Instruction::Coerce { dst: Temp(1), src: Temp(0), from_ty: Type::Int32, to_ty: union_ty() },
            Instruction::Coerce { dst: Temp(2), src: Temp(1), from_ty: union_ty(), to_ty: Type::Int32 },
            // Second use of t1: pass to a named call.
            Instruction::Call {
                dst: Temp(3),
                callee: CallTarget::Named("foo".into()),
                args: vec![Temp(1)],
                ret_ty: Type::Null,
            },
        ];
        let block = make_block(0, instructions, Terminator::Return(Some(Temp(2))));
        let mut func = make_func(vec![block]);
        elide_in_function(&mut func);

        assert_eq!(func.blocks[0].instructions.len(), 4, "escaping box must not cancel");
    }
}
