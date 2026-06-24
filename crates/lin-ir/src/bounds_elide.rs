//! Bounds-check elision pass for flat-scalar-array `Index` instructions.
//!
//! When an `Index { object, key, .. }` reads a flat scalar array whose key is
//! **provably** in `[0, len)`, the OOB branch in `flat_array_get` is unreachable.
//! This pass marks those `Index` instructions with `proven_inbounds = true` so
//! codegen can skip the `br i1 %flat_oob …` dispatch entirely.
//!
//! # Safety contract (make-or-break gate)
//!
//! `proven_inbounds = true` is set ONLY when the pass proves BOTH:
//!   1. `key >= 0`  (the index is non-negative on every execution reaching the site)
//!   2. `key < len` (strictly less than the array's static length)
//!
//! When either condition cannot be proved the flag stays `false` and the normal
//! runtime-bounds-check path is kept.  The pass errs heavily toward **not** eliding.
//!
//! # Analysis overview
//!
//! **Phase 1 – global array lengths.**
//! Scan every function for `GlobalValSet { slot, value: t }` where `t` was produced
//! by a `MakeArray { elements, spreads: [] }`.  The length is `elements.len()`.
//!
//! **Phase 2 – initial-call nonneg witness.**
//! Scan every function for `Call { callee: Named(fid), args }`.  For each arg
//! position, record whether ALL external call sites pass a nonneg constant.
//!
//! **Phase 3+4 – per-function forward dataflow + annotation.**
//! For each function, propagate nonneg and upper-bound facts through the CFG.
//! Mark `Index` instructions `proven_inbounds = true` when key is provably nonneg
//! AND key < static array length.
//!
//! Gate: `LIN_NO_BOUNDS_ELIDE=1` disables the pass for A/B comparison.

use std::collections::{HashMap, HashSet};

use lin_parse::ast::BinOp;

use crate::ir::*;

/// Run the bounds-elide pass on all functions in the module.
pub fn elide_bounds(module: &mut LinModule) {
    if std::env::var("LIN_NO_BOUNDS_ELIDE").is_ok() {
        return;
    }

    let slot_lengths = collect_global_array_lengths(module);
    let initial_nonneg_params = collect_initial_nonneg_params(module);

    if std::env::var("LIN_DEBUG_BOUNDS").is_ok() {
        eprintln!("[bounds_elide] slot_lengths: {:?}", slot_lengths);
        eprintln!("[bounds_elide] initial_nonneg_params ({} fns):", initial_nonneg_params.len());
        for (fid, flags) in &initial_nonneg_params {
            eprintln!("  {:?} → {:?}", fid, flags);
        }
        // Debug: show all Direct calls found
        let indirect_fns_dbg: HashSet<FuncId> = module.functions.iter()
            .flat_map(|f| f.blocks.iter())
            .flat_map(|b| b.instructions.iter())
            .filter_map(|i| if let Instruction::MakeClosure { func, .. } = i { Some(*func) } else { None })
            .collect();
        eprintln!("[bounds_elide] indirect_fns: {:?}", indirect_fns_dbg);
        for func in &module.functions {
            let const_vals = collect_const_int_vals(func);
            for block in &func.blocks {
                for instr in &block.instructions {
                    if let Instruction::Call { callee: CallTarget::Direct(fid), args, .. } = instr {
                        let nn: Vec<bool> = args.iter().map(|a| is_nonneg_const(*a, &const_vals)).collect();
                        eprintln!("[bounds_elide] Direct call from {:?} to {:?}, arg_nonneg: {:?}", func.id, fid, nn);
                    }
                }
            }
        }
    }

    let mut total_elided = 0usize;
    for func in &mut module.functions {
        let before: usize = func.blocks.iter().flat_map(|b| b.instructions.iter())
            .filter(|i| matches!(i, Instruction::Index { proven_inbounds: true, .. }))
            .count();
        annotate_function(func, &slot_lengths, &initial_nonneg_params);
        let after: usize = func.blocks.iter().flat_map(|b| b.instructions.iter())
            .filter(|i| matches!(i, Instruction::Index { proven_inbounds: true, .. }))
            .count();
        if after > before {
            total_elided += after - before;
            if std::env::var("LIN_DEBUG_BOUNDS").is_ok() {
                eprintln!("[bounds_elide] fn {:?} ({:?}): elided {} bounds checks",
                    func.id, func.name, after - before);
            }
        }
    }
    if std::env::var("LIN_DEBUG_BOUNDS").is_ok() || total_elided > 0 {
        eprintln!("[bounds_elide] total proven_inbounds: {}", total_elided);
    }
}

// ---------------------------------------------------------------------------
// Phase 1: global array lengths
// ---------------------------------------------------------------------------

fn collect_global_array_lengths(module: &LinModule) -> HashMap<usize, usize> {
    // Temp → MakeArray length (no spreads).
    let mut temp_len: HashMap<(FuncId, Temp), usize> = HashMap::new();
    for func in &module.functions {
        for block in &func.blocks {
            for instr in &block.instructions {
                if let Instruction::MakeArray { dst, elements, spreads, .. } = instr {
                    if spreads.is_empty() {
                        temp_len.insert((func.id, *dst), elements.len());
                    }
                }
            }
        }
    }

    // Slot → length via immutable GlobalValSet.
    let mut slot_lengths: HashMap<usize, usize> = HashMap::new();
    for func in &module.functions {
        for block in &func.blocks {
            for instr in &block.instructions {
                if let Instruction::GlobalValSet { slot, value, immutable: true, .. } = instr {
                    if let Some(&len) = temp_len.get(&(func.id, *value)) {
                        slot_lengths.insert(*slot, len);
                    }
                }
            }
        }
    }
    slot_lengths
}

// ---------------------------------------------------------------------------
// Phase 2: initial-call nonneg witnesses
// ---------------------------------------------------------------------------

fn collect_initial_nonneg_params(module: &LinModule) -> HashMap<FuncId, Vec<bool>> {
    // Collect all INDIRECT call targets (called via a temp, not a Direct FuncId).
    // If a function is ever called indirectly, we can't guarantee the args are nonneg.
    let indirect_targets: HashSet<FuncId> = {
        // Find temps that hold a function value that is then called via Indirect.
        // We approximate: a FuncId is "indirectly called" if it appears in a MakeClosure
        // AND there is an Indirect call anywhere that could reach it.
        // For simplicity: if ANY indirect calls exist in a function, treat all of that
        // function's local MakeClosure targets as potentially indirectly called.
        // This is conservative but correct.
        let mut targets = HashSet::new();
        for func in &module.functions {
            // Find all Indirect calls in this function.
            let has_indirect = func.blocks.iter()
                .flat_map(|b| b.instructions.iter())
                .any(|i| matches!(i, Instruction::Call { callee: CallTarget::Indirect(_), .. }));
            if has_indirect {
                // Any MakeClosure FuncId in this function could be the indirect callee.
                for block in &func.blocks {
                    for instr in &block.instructions {
                        if let Instruction::MakeClosure { func: fid, .. } = instr {
                            targets.insert(*fid);
                        }
                    }
                }
            }
        }
        targets
    };

    // For each Direct call, accumulate nonneg info for the callee's params.
    let mut call_nonneg: HashMap<FuncId, Option<Vec<bool>>> = HashMap::new();

    for func in &module.functions {
        let const_vals = collect_const_int_vals(func);
        for block in &func.blocks {
            for instr in &block.instructions {
                if let Instruction::Call {
                    callee: CallTarget::Direct(func_id), args, ..
                } = instr {
                    let fid = *func_id;
                    if indirect_targets.contains(&fid) {
                        // Poisoned: can't guarantee args from indirect calls.
                        call_nonneg.insert(fid, None);
                        continue;
                    }
                    let arg_nonneg: Vec<bool> = args.iter()
                        .map(|a| is_nonneg_const(*a, &const_vals))
                        .collect();
                    let entry = call_nonneg.entry(fid).or_insert_with(|| Some(arg_nonneg.clone()));
                    if let Some(existing) = entry {
                        for (i, v) in arg_nonneg.iter().enumerate() {
                            if i < existing.len() {
                                existing[i] = existing[i] && *v;
                            }
                        }
                    }
                }
            }
        }
    }

    // Return only non-poisoned entries where at least one param is nonneg.
    call_nonneg.into_iter()
        .filter_map(|(fid, opt)| opt.map(|flags| (fid, flags)))
        .filter(|(_, flags)| flags.iter().any(|&v| v))
        .collect()
}

fn collect_const_int_vals(func: &LinFunction) -> HashMap<Temp, i64> {
    let mut m = HashMap::new();
    for block in &func.blocks {
        for instr in &block.instructions {
            if let Instruction::Const { dst, val: Const::Int(v, _) } = instr {
                m.insert(*dst, *v);
            }
        }
    }
    m
}

fn is_nonneg_const(t: Temp, const_vals: &HashMap<Temp, i64>) -> bool {
    const_vals.get(&t).map_or(false, |&v| v >= 0)
}

// ---------------------------------------------------------------------------
// Phase 3+4: per-function annotation
// ---------------------------------------------------------------------------

/// Information extracted from a CondJump predecessor about what each branch knows.
#[derive(Clone)]
struct EdgeFact {
    /// The temp whose bound is constrained.
    lhs: Temp,
    /// Strict upper bound in the else-branch:  `lhs < else_upper` (if Some).
    else_upper: Option<i64>,
    /// Nonneg guarantee in the then-branch:    `lhs >= 0` (if true).
    then_nonneg: bool,
    /// Nonneg guarantee in the else-branch:    `lhs >= 0` (if true).
    else_nonneg: bool,
    /// Strict upper bound in the then-branch:  `lhs < then_upper` (if Some).
    then_upper: Option<i64>,
}

fn annotate_function(
    func: &mut LinFunction,
    slot_lengths: &HashMap<usize, usize>,
    initial_nonneg_params: &HashMap<FuncId, Vec<bool>>,
) {
    let const_vals = collect_const_int_vals(func);

    // Build CFG.
    let successors = build_successors(func);
    let predecessors = build_predecessors(func, &successors);

    // Per-block entry facts.
    let mut block_upper: HashMap<BlockId, HashMap<Temp, i64>> = HashMap::new();
    let mut block_nonneg: HashMap<BlockId, HashSet<Temp>> = HashMap::new();
    for block in &func.blocks {
        block_upper.insert(block.id, HashMap::new());
        block_nonneg.insert(block.id, HashSet::new());
    }

    // Seed entry with param nonneg from call-site analysis.
    let param_nonneg_flags = initial_nonneg_params.get(&func.id);
    let initially_nonneg: HashSet<Temp> = func.params.iter().enumerate()
        .filter_map(|(i, (t, _))| {
            param_nonneg_flags
                .and_then(|f| f.get(i))
                .copied()
                .and_then(|ok| if ok { Some(*t) } else { None })
        })
        .collect();

    // TCO inductive nonneg: params provably nonneg via induction through TailCall.
    let tco_nonneg = compute_tco_inductive_nonneg(func, &initially_nonneg, &const_vals);

    if let Some(entry_nonneg) = block_nonneg.get_mut(&func.blocks[0].id) {
        for t in &initially_nonneg { entry_nonneg.insert(*t); }
        for t in &tco_nonneg { entry_nonneg.insert(*t); }
    }

    // Build per-block edge facts: for each block with a CondJump, what do we know
    // in the then/else successors?
    let edge_facts: HashMap<BlockId, EdgeFact> = func.blocks.iter()
        .filter_map(|block| {
            if let Terminator::CondJump { cond, then_block, else_block } = block.terminator {
                extract_edge_fact(block, cond, then_block, else_block, &const_vals)
                    .map(|ef| (block.id, ef))
            } else {
                None
            }
        })
        .collect();

    // Global-temp → slot map for GlobalValGet.
    let gvget_slot: HashMap<Temp, usize> = func.blocks.iter()
        .flat_map(|b| b.instructions.iter())
        .filter_map(|i| if let Instruction::GlobalValGet { dst, slot, .. } = i { Some((*dst, *slot)) } else { None })
        .collect();

    // Forward dataflow fixpoint.
    let block_ids: Vec<BlockId> = func.blocks.iter().map(|b| b.id).collect();
    let entry_id = func.blocks[0].id;

    for _ in 0..8 {
        let mut changed = false;

        for &bid in &block_ids {
            let preds: Vec<BlockId> = predecessors.get(&bid).cloned().unwrap_or_default();

            // Compute incoming facts by merging from all predecessors.
            let (new_nonneg, new_upper) = if bid == entry_id {
                // Entry: keep seeded facts (don't clobber with TailCall back-edges).
                // For subsequent iterations, keep whatever we accumulated.
                let existing_nn = block_nonneg.get(&bid).cloned().unwrap_or_default();
                let existing_ub = block_upper.get(&bid).cloned().unwrap_or_default();
                (existing_nn, existing_ub)
            } else if preds.is_empty() {
                (HashSet::new(), HashMap::new())
            } else {
                // Merge: we compute the intersection of facts from ALL predecessors,
                // accounting for the edge facts each predecessor adds.
                let mut pred_nonneg_sets: Vec<HashSet<Temp>> = Vec::new();
                let mut pred_upper_maps: Vec<HashMap<Temp, i64>> = Vec::new();

                for &pred in &preds {
                    // Start with pred's EXIT facts (= block's entry facts propagated through block).
                    let (mut nn, mut ub) = {
                        let b_nn = block_nonneg.get(&pred).cloned().unwrap_or_default();
                        let b_ub = block_upper.get(&pred).cloned().unwrap_or_default();
                        let pred_block = func.blocks.iter().find(|b| b.id == pred).unwrap();
                        propagate_through_block(pred_block, b_nn, b_ub, &const_vals)
                    };

                    // Apply edge-specific facts from pred → bid.
                    if let Some(ef) = edge_facts.get(&pred) {
                        if let Terminator::CondJump { then_block, else_block, .. } =
                            func.blocks.iter().find(|b| b.id == pred).unwrap().terminator
                        {
                            if bid == then_block {
                                if ef.then_nonneg { nn.insert(ef.lhs); }
                                if let Some(ub_val) = ef.then_upper {
                                    let e = ub.entry(ef.lhs).or_insert(i64::MAX);
                                    *e = (*e).min(ub_val);
                                }
                            } else if bid == else_block {
                                if ef.else_nonneg { nn.insert(ef.lhs); }
                                if let Some(ub_val) = ef.else_upper {
                                    let e = ub.entry(ef.lhs).or_insert(i64::MAX);
                                    *e = (*e).min(ub_val);
                                }
                            }
                        }
                    }

                    pred_nonneg_sets.push(nn);
                    pred_upper_maps.push(ub);
                }

                // Intersection across all predecessors.
                let merged_nn = intersect_nonneg_sets(&pred_nonneg_sets);
                let merged_ub = intersect_upper_maps(&pred_upper_maps);
                (merged_nn, merged_ub)
            };

            let cur_nn = block_nonneg.get(&bid).cloned().unwrap_or_default();
            let cur_ub = block_upper.get(&bid).cloned().unwrap_or_default();
            if new_nonneg != cur_nn { changed = true; block_nonneg.insert(bid, new_nonneg); }
            if new_upper != cur_ub { changed = true; block_upper.insert(bid, new_upper); }
        }

        if !changed { break; }
    }

    // Phase 4: annotate Index instructions.
    for block in &mut func.blocks {
        let bid = block.id;
        // Get the facts at the START of this block, then propagate to each instruction.
        let b_start_nn = block_nonneg.get(&bid).cloned().unwrap_or_default();
        let b_start_ub = block_upper.get(&bid).cloned().unwrap_or_default();
        let (mut b_nn, mut b_ub) = (b_start_nn, b_start_ub);

        for instr in &mut block.instructions {
            // Update running facts for this instruction.
            match instr {
                Instruction::Const { dst, val: Const::Int(v, _) } => {
                    if *v >= 0 { b_nn.insert(*dst); }
                    b_ub.insert(*dst, *v + 1);
                }
                Instruction::Binary { dst, op: BinOp::Add, lhs, rhs, .. } => {
                    let lhs_nn = b_nn.contains(lhs) || const_vals.get(lhs).map_or(false, |&v| v >= 0);
                    let rhs_nn = b_nn.contains(rhs) || const_vals.get(rhs).map_or(false, |&v| v >= 0);
                    if lhs_nn && rhs_nn { b_nn.insert(*dst); }
                    if let (Some(&ub_l), Some(&c)) = (b_ub.get(lhs), const_vals.get(rhs)) {
                        b_ub.insert(*dst, ub_l.saturating_add(c));
                    } else if let (Some(&c), Some(&ub_r)) = (const_vals.get(lhs), b_ub.get(rhs)) {
                        b_ub.insert(*dst, ub_r.saturating_add(c));
                    }
                }
                Instruction::Copy { dst, src, .. } => {
                    if b_nn.contains(src) { b_nn.insert(*dst); }
                    if let Some(&ub) = b_ub.get(src) { b_ub.insert(*dst, ub); }
                }
                Instruction::Index { object, key, obj_ty, proven_inbounds, nonneg, .. } => {
                    if is_flat_scalar_array_ty(obj_ty) {
                        if let Some(&slot) = gvget_slot.get(object) {
                            if let Some(&arr_len) = slot_lengths.get(&slot) {
                                let arr_len = arr_len as i64;
                                let key_nn = b_nn.contains(key)
                                    || const_vals.get(key).map_or(false, |&v| v >= 0);
                                let key_lt = if let Some(&v) = const_vals.get(key) {
                                    v >= 0 && v < arr_len
                                } else {
                                    b_ub.get(key).map_or(false, |&ub| ub <= arr_len)
                                };
                                if key_nn && key_lt {
                                    *proven_inbounds = true;
                                    *nonneg = true;
                                }
                            }
                        }
                    }
                    // Don't update running facts for Index — it defines dst but we don't
                    // propagate nonneg through index results here.
                }
                _ => {}
            }
        }
    }
}

// ---------------------------------------------------------------------------
// TCO inductive nonneg
// ---------------------------------------------------------------------------

fn compute_tco_inductive_nonneg(
    func: &LinFunction,
    initially_nonneg: &HashSet<Temp>,
    const_vals: &HashMap<Temp, i64>,
) -> HashSet<Temp> {
    let tail_call_args: Vec<Vec<Temp>> = func.blocks.iter()
        .filter_map(|b| if let Terminator::TailCall { args } = &b.terminator { Some(args.clone()) } else { None })
        .collect();

    if tail_call_args.is_empty() || initially_nonneg.is_empty() {
        return HashSet::new();
    }

    // Build a body-wide nonneg over-approximation (ignoring control flow).
    let body_nonneg = compute_body_nonneg(func, initially_nonneg, const_vals);

    // A param at position k is inductively nonneg if:
    // - It's in initially_nonneg (nonneg at all external call sites), AND
    // - Every TailCall arg at position k is in body_nonneg.
    let mut inductive = HashSet::new();
    for (k, (p, _)) in func.params.iter().enumerate() {
        if !initially_nonneg.contains(p) { continue; }
        let ok = tail_call_args.iter().all(|args| {
            args.get(k).map_or(true, |a| body_nonneg.contains(a))
        });
        if ok { inductive.insert(*p); }
    }
    inductive
}

fn compute_body_nonneg(
    func: &LinFunction,
    seed: &HashSet<Temp>,
    const_vals: &HashMap<Temp, i64>,
) -> HashSet<Temp> {
    let mut nonneg: HashSet<Temp> = seed.clone();
    for (&t, &v) in const_vals { if v >= 0 { nonneg.insert(t); } }
    let mut changed = true;
    while changed {
        changed = false;
        for block in &func.blocks {
            for instr in &block.instructions {
                match instr {
                    Instruction::Binary { dst, op: BinOp::Add, lhs, rhs, .. } => {
                        if nonneg.contains(lhs) && nonneg.contains(rhs) && nonneg.insert(*dst) {
                            changed = true;
                        }
                    }
                    Instruction::Copy { dst, src, .. } => {
                        if nonneg.contains(src) && nonneg.insert(*dst) { changed = true; }
                    }
                    _ => {}
                }
            }
        }
    }
    nonneg
}

// ---------------------------------------------------------------------------
// Edge fact extraction
// ---------------------------------------------------------------------------

fn extract_edge_fact(
    block: &BasicBlock,
    cond: Temp,
    _then_block: BlockId,
    _else_block: BlockId,
    const_vals: &HashMap<Temp, i64>,
) -> Option<EdgeFact> {
    // Find the Binary instruction that defines `cond`.
    for instr in block.instructions.iter().rev() {
        if let Instruction::Binary { dst, op, lhs, rhs, .. } = instr {
            if *dst != cond { continue; }
            match op {
                BinOp::GtEq => {
                    // cond = lhs >= rhs_const
                    // then: lhs >= rhs_const (lhs is nonneg if rhs_const >= 0)
                    // else: lhs < rhs_const  (upper bound = rhs_const)
                    if let Some(&rhs_c) = const_vals.get(rhs) {
                        return Some(EdgeFact {
                            lhs: *lhs,
                            else_upper: Some(rhs_c),
                            else_nonneg: false,
                            then_nonneg: rhs_c >= 0,
                            then_upper: None,
                        });
                    }
                }
                BinOp::Gt => {
                    // cond = lhs > rhs_const
                    // else: lhs <= rhs_const → lhs < rhs_const+1
                    if let Some(&rhs_c) = const_vals.get(rhs) {
                        return Some(EdgeFact {
                            lhs: *lhs,
                            else_upper: Some(rhs_c + 1),
                            else_nonneg: false,
                            then_nonneg: rhs_c + 1 >= 0,
                            then_upper: None,
                        });
                    }
                }
                BinOp::Lt => {
                    // cond = lhs < rhs_const
                    // then: lhs < rhs_const (upper bound)
                    // else: lhs >= rhs_const (nonneg if rhs_const >= 0)
                    if let Some(&rhs_c) = const_vals.get(rhs) {
                        return Some(EdgeFact {
                            lhs: *lhs,
                            else_upper: None,
                            else_nonneg: rhs_c >= 0,
                            then_nonneg: false,
                            then_upper: Some(rhs_c),
                        });
                    }
                }
                BinOp::LtEq => {
                    // cond = lhs <= rhs_const
                    // then: lhs < rhs_const+1 (upper bound)
                    // else: lhs > rhs_const → lhs >= rhs_const+1 (nonneg if rhs_const+1 >= 0)
                    if let Some(&rhs_c) = const_vals.get(rhs) {
                        return Some(EdgeFact {
                            lhs: *lhs,
                            else_upper: None,
                            else_nonneg: rhs_c + 1 >= 0,
                            then_nonneg: false,
                            then_upper: Some(rhs_c + 1),
                        });
                    }
                }
                _ => return None,
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Dataflow helpers
// ---------------------------------------------------------------------------

fn propagate_through_block(
    block: &BasicBlock,
    mut nonneg: HashSet<Temp>,
    mut upper: HashMap<Temp, i64>,
    const_vals: &HashMap<Temp, i64>,
) -> (HashSet<Temp>, HashMap<Temp, i64>) {
    for instr in &block.instructions {
        match instr {
            Instruction::Const { dst, val: Const::Int(v, _) } => {
                if *v >= 0 { nonneg.insert(*dst); }
                upper.insert(*dst, *v + 1);
            }
            Instruction::Binary { dst, op: BinOp::Add, lhs, rhs, .. } => {
                let lhs_nn = nonneg.contains(lhs) || const_vals.get(lhs).map_or(false, |&v| v >= 0);
                let rhs_nn = nonneg.contains(rhs) || const_vals.get(rhs).map_or(false, |&v| v >= 0);
                if lhs_nn && rhs_nn { nonneg.insert(*dst); }
                if let (Some(&ub), Some(&c)) = (upper.get(lhs), const_vals.get(rhs)) {
                    upper.insert(*dst, ub.saturating_add(c));
                } else if let (Some(&c), Some(&ub)) = (const_vals.get(lhs), upper.get(rhs)) {
                    upper.insert(*dst, ub.saturating_add(c));
                }
            }
            Instruction::Copy { dst, src, .. } => {
                if nonneg.contains(src) { nonneg.insert(*dst); }
                if let Some(&ub) = upper.get(src) { upper.insert(*dst, ub); }
            }
            _ => {}
        }
    }
    (nonneg, upper)
}

fn intersect_nonneg_sets(sets: &[HashSet<Temp>]) -> HashSet<Temp> {
    let mut it = sets.iter();
    match it.next() {
        None => HashSet::new(),
        Some(first) => {
            let mut result = first.clone();
            for s in it { result = result.intersection(s).cloned().collect(); }
            result
        }
    }
}

fn intersect_upper_maps(maps: &[HashMap<Temp, i64>]) -> HashMap<Temp, i64> {
    let mut it = maps.iter();
    match it.next() {
        None => HashMap::new(),
        Some(first) => {
            let mut result = first.clone();
            for m in it { result = intersect_two_upper(&result, m); }
            result
        }
    }
}

/// Intersection of two upper-bound maps: a bound is kept only when present in both;
/// the kept bound is the MAX (the more conservative / weaker of the two).
fn intersect_two_upper(a: &HashMap<Temp, i64>, b: &HashMap<Temp, i64>) -> HashMap<Temp, i64> {
    let mut out = HashMap::new();
    for (&t, &ua) in a {
        if let Some(&ub) = b.get(&t) {
            out.insert(t, ua.max(ub)); // MAX = most conservative bound valid on ALL paths
        }
    }
    out
}

// ---------------------------------------------------------------------------
// CFG
// ---------------------------------------------------------------------------

fn build_successors(func: &LinFunction) -> HashMap<BlockId, Vec<BlockId>> {
    let entry_id = func.blocks[0].id;
    func.blocks.iter().map(|block| {
        let s = match &block.terminator {
            Terminator::Jump(t) => vec![*t],
            Terminator::CondJump { then_block, else_block, .. } => vec![*then_block, *else_block],
            Terminator::Switch { cases, default, .. } => {
                let mut v: Vec<_> = cases.iter().map(|(_, b)| *b).collect();
                v.push(*default);
                v
            }
            Terminator::TailCall { .. } => vec![entry_id],
            Terminator::Return(_) | Terminator::Unreachable => vec![],
        };
        (block.id, s)
    }).collect()
}

fn build_predecessors(func: &LinFunction, succs: &HashMap<BlockId, Vec<BlockId>>) -> HashMap<BlockId, Vec<BlockId>> {
    let mut preds: HashMap<BlockId, Vec<BlockId>> = func.blocks.iter().map(|b| (b.id, vec![])).collect();
    for (&bid, ss) in succs {
        for &s in ss { preds.entry(s).or_default().push(bid); }
    }
    preds
}

// ---------------------------------------------------------------------------
// Type helpers
// ---------------------------------------------------------------------------

fn is_flat_scalar_array_ty(ty: &lin_check::types::Type) -> bool {
    use lin_check::types::Type;
    matches!(ty,
        Type::Array(elem) if matches!(
            elem.as_ref(),
            Type::Float64 | Type::Int32 | Type::Int64 | Type::Bool
        )
    )
}
