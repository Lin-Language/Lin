//! Substring-key map fusion pass.
//!
//! When a `substring(seq, i, i+K)` result is ONLY used as a STRING MAP key
//! (for `Index`/`IndexSet` on `{ String: T }`) and nowhere else, skip the
//! heap allocation entirely: pass the source bytes directly to
//! `lin_map_get_bytes` / `lin_map_set_bytes`.
//!
//! # Soundness contract
//!
//! A temp `t` is eligible for fusion ONLY when:
//!   1. Its single definition is `Call { Named("lin_string_slice"), args: [src, start, end] }`.
//!   2. Every use of `t` is one of:
//!        - `Index  { key: t, obj_ty: Map{key: Str, ..} }` — string-keyed map get
//!        - `IndexSet { key: t, obj_ty: Map{key: Str, ..} }` — string-keyed map set
//!        - `Release { val: t }` — scope-exit cleanup (deleted because no string is created)
//!      Any other use (return, store, pass to another fn, compare, etc.) disqualifies `t`.
//!   3. The source string `src` is an IMMUTABLE binding (not from a `LocalCellGet` mutable
//!      cell) — this ensures it is not freed/mutated before the map ops complete.
//!      We conservatively accept `LocalGet` and `GlobalGet` as immutable.
//!
//! The pass only populates `func.substr_fuse`; it does not mutate instructions.
//! Codegen reads `substr_fuse` to emit byte-keyed calls and skip the string
//! materialization + release.
//!
//! Gate: `LIN_NO_SUBSTR_FUSE=1` disables the pass.

use std::collections::HashMap;
use crate::ir::*;
use lin_check::types::Type;

/// Run the pass on all functions in the module.
pub fn run(module: &mut LinModule) {
    if std::env::var("LIN_NO_SUBSTR_FUSE").is_ok() {
        return;
    }
    for func in &mut module.functions {
        run_fn(func);
    }
}

fn run_fn(func: &mut LinFunction) {
    // Phase 1: collect definitions of each temp.
    // def_site[t] = Some((src, start, end)) when t is defined by lin_string_slice.
    let mut slice_defs: HashMap<Temp, [Temp; 3]> = HashMap::new();

    for block in &func.blocks {
        for instr in &block.instructions {
            if let Instruction::Call { dst, callee: CallTarget::Named(name), args, .. } = instr {
                // Match both the internal `lin_string_slice` (when called directly from
                // trusted stdlib code) and the public stdlib wrappers that user code sees:
                //   std_string_substring(str, start, end)
                //   std_string__substring(str, start, end)   [internal helper]
                // All three have the same (String, Int32, Int32) -> String signature.
                if args.len() == 3 && (name == "lin_string_slice"
                    || name == "std_string_substring"
                    || name == "std_string__substring")
                {
                    slice_defs.insert(*dst, [args[0], args[1], args[2]]);
                }
            }
        }
    }

    if slice_defs.is_empty() {
        return;
    }

    // Phase 2: verify that every use of each candidate temp is an acceptable map-key use.
    let mut disqualified: std::collections::HashSet<Temp> = std::collections::HashSet::new();

    for block in &func.blocks {
        for instr in &block.instructions {
            // Walk every operand of every instruction.
            let disq_if_slice = |t: Temp| -> bool {
                slice_defs.contains_key(&t)
            };
            match instr {
                // Acceptable uses: key operand of Index/IndexSet on a String-keyed map.
                Instruction::Index { object, key, obj_ty, .. } => {
                    if disq_if_slice(*key) && !is_string_map(obj_ty) {
                        disqualified.insert(*key);
                    }
                    // A slice temp used as the OBJECT (not key) needs a materialized LinString;
                    // disqualify it (else codegen skips its materialization and the object reads
                    // undefined).
                    if disq_if_slice(*object) {
                        disqualified.insert(*object);
                    }
                }
                Instruction::IndexSet { object, key, value, obj_ty, .. } => {
                    if disq_if_slice(*key) && !is_string_map(obj_ty) {
                        disqualified.insert(*key);
                    }
                    // A slice temp used as the OBJECT, or STORED as the VALUE, is a non-key use
                    // that must be materialized. (The original soundness hole: `m[k] = substring(...)`
                    // mis-fused the stored value, dropping its allocation → corrupted/empty value.)
                    if disq_if_slice(*object) {
                        disqualified.insert(*object);
                    }
                    if disq_if_slice(*value) {
                        disqualified.insert(*value);
                    }
                }
                // Acceptable: Retain/Release are both skipped in codegen for fused temps
                // since no LinString heap object is ever created.
                Instruction::Retain { val, .. } => { let _ = val; }
                Instruction::Release { val, .. } => { let _ = val; }
                // Everything else: if any operand is a slice-def temp, disqualify it.
                other => {
                    for t in temps_used(other) {
                        if disq_if_slice(t) {
                            disqualified.insert(t);
                        }
                    }
                }
            }
        }
        // Terminators that reference temps:
        match &block.terminator {
            Terminator::Return(Some(t)) => {
                if slice_defs.contains_key(t) { disqualified.insert(*t); }
            }
            Terminator::CondJump { cond, .. } => {
                if slice_defs.contains_key(cond) { disqualified.insert(*cond); }
            }
            _ => {}
        }
    }

    // Phase 3: Verify that the source string `src` is not a live mutable alias.
    // Temps from `CellGet` (mutable `var` slot) or `GlobalValGet(immutable=false)`
    // (mutable global `var`) can change across iterations — if we bypass the
    // LinString allocation and hold a raw byte pointer, the underlying string could
    // be released mid-loop before the map ops complete.
    // Call/CallIntrinsic results are single-assignment SSA and are always safe.
    let mut mutable_temps: std::collections::HashSet<Temp> = std::collections::HashSet::new();
    for block in &func.blocks {
        for instr in &block.instructions {
            match instr {
                // `var`-cell reads can yield a different heap object each iteration.
                Instruction::CellGet { dst, .. } => { mutable_temps.insert(*dst); }
                // Mutable global `var` reads.
                Instruction::GlobalValGet { dst, immutable, .. } if !immutable => {
                    mutable_temps.insert(*dst);
                }
                _ => {}
            }
        }
    }
    for (t, [src, _, _]) in &slice_defs {
        if mutable_temps.contains(src) {
            disqualified.insert(*t);
        }
    }

    // Commit: all non-disqualified slice-def temps become fused.
    for (t, args) in slice_defs {
        if !disqualified.contains(&t) {
            func.substr_fuse.insert(t, args);
        }
    }
}

/// Return true when `ty` is (or contains) a `{String: V}` map — the only map kind
/// where `lin_map_get_bytes` / `lin_map_set_bytes` are safe.
fn is_string_map(ty: &Type) -> bool {
    match ty {
        Type::Map { key, .. } => key.is_string_ish(),
        Type::Union(members) => members.iter().any(|m| matches!(m, Type::Map { key, .. } if key.is_string_ish())),
        _ => false,
    }
}

/// Collect all temp operands referenced by an instruction (excluding dst).
fn temps_used(instr: &Instruction) -> Vec<Temp> {
    let mut out = Vec::new();
    match instr {
        Instruction::Const { .. } => {}
        Instruction::Copy { src, .. } => out.push(*src),
        Instruction::Phi { incomings, .. } => {
            for (t, _) in incomings { out.push(*t); }
        }
        Instruction::Unary { operand, .. } => out.push(*operand),
        Instruction::Binary { lhs, rhs, .. } => { out.push(*lhs); out.push(*rhs); }
        Instruction::Coerce { src, .. } => out.push(*src),
        Instruction::Call { args, .. } => out.extend_from_slice(args),
        Instruction::CallIntrinsic { args, .. } => out.extend_from_slice(args),
        Instruction::MakeClosure { captures, .. } => out.extend_from_slice(captures),
        Instruction::MakeNamedClosure { .. } => {}
        Instruction::MakeObject { fields, spreads, computed_fields, .. } => {
            for (_, t) in fields { out.push(*t); }
            out.extend_from_slice(spreads);
            for (k, v) in computed_fields { out.push(*k); out.push(*v); }
        }
        Instruction::MakeArray { elements, spreads, .. } => {
            out.extend_from_slice(elements);
            for (_, t) in spreads { out.push(*t); }
        }
        Instruction::Index { object, key, .. } => { out.push(*object); out.push(*key); }
        Instruction::IndexSet { object, key, value, .. } => {
            out.push(*object); out.push(*key); out.push(*value);
        }
        Instruction::FieldGet { object, .. } => out.push(*object),
        Instruction::FieldSet { object, value, .. } => { out.push(*object); out.push(*value); }
        Instruction::GlobalValGet { .. } => {}
        Instruction::GlobalValSet { value, .. } => out.push(*value),
        Instruction::MakeCell { init, .. } => out.push(*init),
        Instruction::CellGet { cell, .. } => out.push(*cell),
        Instruction::CellSet { cell, value, .. } => { out.push(*cell); out.push(*value); }
        Instruction::FreeCell { cell, .. } => out.push(*cell),
        Instruction::Retain { val, .. } => out.push(*val),
        Instruction::Release { val, .. } => out.push(*val),
        Instruction::CloneBox { src, .. } => out.push(*src),
        Instruction::FreeBoxShell { val } => out.push(*val),
        Instruction::FreeBoxShellIfDistinct { val, other } => { out.push(*val); out.push(*other); }
        Instruction::ReleaseIfDistinct { val, other } => { out.push(*val); out.push(*other); }
        Instruction::ReleaseRawIfDistinct { val, other, .. } => { out.push(*val); out.push(*other); }
        Instruction::IsType { val, .. } => out.push(*val),
        Instruction::SumTagEq { val, .. } => out.push(*val),
        Instruction::HasPattern { val, .. } => out.push(*val),
        Instruction::MatchesSchema { val, .. } => out.push(*val),
        Instruction::Box { val, .. } => out.push(*val),
        Instruction::Unbox { val, .. } => out.push(*val),
        Instruction::Bind { src, .. } => out.push(*src),
        Instruction::Panic { msg } => out.push(*msg),
        Instruction::SealedArrayFieldGet { array, index, .. } => { out.push(*array); out.push(*index); }
        Instruction::BoxedArrayFieldGet { array, index, .. } => { out.push(*array); out.push(*index); }
        Instruction::EnvCapture { env, .. } => out.push(*env),
        Instruction::ArrayLenCheck { val, .. } => out.push(*val),
        Instruction::ObjectRest { src, .. } => out.push(*src),
        _ => {}
    }
    out
}
