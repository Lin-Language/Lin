//! RC elision pass for LinIR.
//!
//! Eliminates Retain/Release pairs where the retained value's live range does
//! not span any allocation, call site, or escape that could observe the
//! refcount or create an independent owner.
//!
//! # Soundness invariant
//!
//! The ONLY safe transformation here is to remove a *balanced* Retain/Release
//! pair: removing exactly one Retain and exactly one matching Release leaves the
//! program's net refcount of the temp unchanged on every control-flow path. It
//! is always acceptable to elide LESS (a kept redundant pair is a perf cost, not
//! a bug); it is NEVER acceptable to leave the program unbalanced on any path.
//! The pass therefore fails *toward not eliding* whenever pairing or
//! post-dominance is uncertain.
//!
//! # Algorithm
//!
//! 1. Run liveness analysis on the function (`liveness`) and compute
//!    post-dominators (`PostDom`) over the CFG.
//! 2. For each Retain of an RC-typed temp at (block, i):
//!    - **Same block**: pair with the first *unclaimed* Release of the temp
//!      after the Retain (one-to-one — a Release already claimed by an earlier
//!      Retain is skipped). The span between them must have *no interference*:
//!      no call/alloc/escape, no Release of the temp, and no further Retain of
//!      the temp (an intervening same-temp Retain is treated as interference so
//!      the one-to-one pairing stays unambiguous and we never elide across it).
//!    - **Cross block**: a BFS across CFG successors finds the block holding the
//!      matching unclaimed Release. The pair is only elided when **the Release's
//!      block post-dominates the Retain's block** — i.e. the Release is reached
//!      on *every* path leaving the Retain. Without post-dominance the Release
//!      covers only some successor paths and eliding the Retain would leak on
//!      the others (the original cross-block soundness hole).
//! 3. **Liveness gate (the documented safety net)**: a pair is elided only when
//!    liveness confirms the temp is *dead immediately after the matched
//!    Release* (a true last-use). If the temp is still live past the Release,
//!    that Release is not the final drop — another owning reference is in play —
//!    so the pair is kept. This is what actually drives the elision decision;
//!    the structural path/post-dominance checks narrow the candidate set, and
//!    liveness has the final say.
//! 4. Remove the elided Retain/Release pairs from the instruction lists.
//!
//! This is a conservative approximation: we err on the side of keeping RC ops
//! when pairing, the path, post-dominance, or liveness is uncertain (aliasing,
//! indirect calls, branchy releases).
//!
//! Reference: Reinking et al., "Perceus: Garbage Free Reference Counting with
//! Reuse", PLDI 2021.

use std::collections::{HashMap, HashSet};

use lin_check::types::Type;

use crate::ir::*;
use crate::liveness::Liveness;
use crate::ownership_verify::intrinsic_conventions;


/// Run the RC elision pass on all functions in a module, mutating in place.
pub fn elide_rc(module: &mut LinModule) {
    // Build a stable function-id → convention table before mutating.
    // We only need the param_conventions (populated by infer_conventions which runs first).
    let conv_map: HashMap<FuncId, Vec<Convention>> = module
        .functions
        .iter()
        .map(|f| (f.id, f.param_conventions.clone()))
        .collect();

    for func in &mut module.functions {
        elide_rc_fn(func, &conv_map);
        elide_clonebox_reads_fn(func, &conv_map);
    }
}

fn elide_rc_fn(func: &mut LinFunction, conv_map: &HashMap<FuncId, Vec<Convention>>) {
    let liveness = Liveness::compute(func);
    // Post-dominators over the CFG: used to require that a cross-block Release is
    // reached on every path leaving the Retain before we elide the pair.
    let postdom = PostDom::compute(func);

    // Build a map from BlockId → index in func.blocks for fast lookup.
    let block_index: HashMap<BlockId, usize> = func
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.id, i))
        .collect();

    // Collect (block_idx, instr_idx) pairs to remove.
    let mut to_remove: HashSet<(usize, usize)> = HashSet::new();

    for block_idx in 0..func.blocks.len() {
        let instrs = func.blocks[block_idx].instructions.clone();

        // For each Retain, look forward for its matching Release with a clean path.
        for (retain_idx, instr) in instrs.iter().enumerate() {
            let Instruction::Retain { val: retain_val, ty } = instr else {
                continue;
            };
            if !is_rc_type(ty) {
                continue;
            }

            // --- same-block search ---
            // Pair with the first Release of `temp` that has NOT already been claimed by an
            // earlier Retain. Retain/Release elision must be ONE-TO-ONE: when a temp is
            // retained N times and released N times in the same block (e.g. a heap parameter
            // read N>1 times — each read emits a Retain and registers a scope-exit Release),
            // every Retain pairing to the SAME first Release would elide N Retains but, since
            // `to_remove` is a set, only ONE Release — leaving N-1 unbalanced Releases and an
            // over-release (use-after-free). Skipping already-claimed Releases keeps the
            // pairing balanced.
            if let Some(release_idx) =
                find_paired_release_in_block(*retain_val, retain_idx, &instrs, |i| {
                    to_remove.contains(&(block_idx, i))
                })
            {
                if path_has_no_interference(*retain_val, retain_idx, release_idx, &instrs, conv_map)
                    && release_is_last_use(
                        &liveness,
                        &func.blocks[block_idx],
                        release_idx,
                        *retain_val,
                    )
                {
                    to_remove.insert((block_idx, retain_idx));
                    to_remove.insert((block_idx, release_idx));
                }
                // Found a same-block Release (clean or not) — do not also do cross-block BFS.
                continue;
            }
            // No UNCLAIMED Release found. If the block nonetheless contains a Release of the
            // temp (already claimed by an earlier Retain), this Retain is matched in-block too:
            // leave it as-is (it stays) and do NOT fall through to the cross-block search, which
            // would mis-pair it with a Release in a successor block.
            if find_paired_release_in_block(*retain_val, retain_idx, &instrs, |_| false).is_some() {
                continue;
            }

            // The same-block search either found nothing or found a redefinition.
            // Check whether the temp reaches end-of-block without redefinition or
            // Release; if not, there is nothing to match cross-block.
            if !temp_survives_to_block_end(*retain_val, retain_idx, &instrs) {
                continue;
            }

            // --- cross-block BFS ---
            // The tail of the current block (instructions after the Retain) must
            // itself be clean before we leave the block.
            let tail_clean =
                path_has_no_interference(*retain_val, retain_idx, instrs.len(), &instrs, conv_map);
            if !tail_clean {
                continue;
            }

            if let Some((release_block_idx, release_instr_idx)) = find_paired_release_cross_block(
                *retain_val,
                block_idx,
                func,
                &block_index,
                &to_remove,
                &postdom,
                conv_map,
            ) {
                let release_block_id = func.blocks[release_block_idx].id;
                let retain_block_id = func.blocks[block_idx].id;
                // The release block's prefix (before the Release) must also be clean.
                let prefix_clean = path_has_no_interference(
                    *retain_val,
                    usize::MAX, // sentinel: start from instruction 0
                    release_instr_idx,
                    &func.blocks[release_block_idx].instructions,
                    conv_map,
                );
                // SOUNDNESS: the Release must be reached on EVERY path leaving the
                // Retain. If the release block only post-dominates the retain block
                // along some successor edges, eliding the Retain leaks on the others.
                let release_postdominates =
                    postdom.post_dominates(release_block_id, retain_block_id);
                // And the Release must be the temp's last use (liveness gate).
                let last_use = release_is_last_use(
                    &liveness,
                    &func.blocks[release_block_idx],
                    release_instr_idx,
                    *retain_val,
                );
                if prefix_clean && release_postdominates && last_use {
                    to_remove.insert((block_idx, retain_idx));
                    to_remove.insert((release_block_idx, release_instr_idx));
                }
            }
        }
    }

    // Remove instructions in reverse order so indices stay valid.
    for block_idx in 0..func.blocks.len() {
        let mut remove_here: Vec<usize> = to_remove
            .iter()
            .filter(|(b, _)| *b == block_idx)
            .map(|(_, i)| *i)
            .collect();
        remove_here.sort_unstable_by(|a, b| b.cmp(a)); // descending
        for idx in remove_here {
            func.blocks[block_idx].instructions.remove(idx);
            // Keep the parallel debug-span side-table in lockstep (only populated in --debug builds).
            if idx < func.blocks[block_idx].instr_spans.len() {
                func.blocks[block_idx].instr_spans.remove(idx);
            }
        }
    }
}

fn is_rc_type(ty: &Type) -> bool {
    is_concrete_rc_ty(ty)
}

/// Find the Release instruction paired with the Retain at `retain_idx` in the
/// *same* block. Returns `None` if the temp is redefined before a Release
/// (a different value) or if no Release is found in this block.
fn find_paired_release_in_block(
    temp: Temp,
    retain_idx: usize,
    instrs: &[Instruction],
    is_claimed: impl Fn(usize) -> bool,
) -> Option<usize> {
    for i in (retain_idx + 1)..instrs.len() {
        match &instrs[i] {
            // Skip a Release already claimed (paired+elided) by an earlier Retain so each
            // Release matches at most one Retain (one-to-one elision).
            Instruction::Release { val, .. } if *val == temp && is_claimed(i) => continue,
            Instruction::Release { val, .. } if *val == temp => return Some(i),
            other => {
                let (_uses, defs) = crate::liveness::instr_use_def(other);
                // If temp is redefined, the Retain was for a different live range.
                if defs.contains(&temp) {
                    return None;
                }
            }
        }
    }
    None
}

/// Returns true when `temp` is still live (not redefined, not released) from
/// `retain_idx` to the end of the instruction list — i.e., it could potentially
/// be matched by a Release in a successor block.
fn temp_survives_to_block_end(temp: Temp, retain_idx: usize, instrs: &[Instruction]) -> bool {
    for instr in &instrs[(retain_idx + 1)..] {
        match instr {
            Instruction::Release { val, .. } if *val == temp => return false,
            other => {
                let (_uses, defs) = crate::liveness::instr_use_def(other);
                if defs.contains(&temp) {
                    return false;
                }
            }
        }
    }
    true
}

/// Walk down the post-dominator chain from the retain's block to find the
/// paired Release for `temp`.
///
/// The key insight: `PostDom` already tells us which blocks post-dominate the
/// retain's block.  Walking the *immediate post-dominator* chain (the unique
/// path of idom nodes from `origin` upward through the post-dominator tree)
/// visits only blocks guaranteed to be on EVERY path from the retain.  This
/// is strictly safer than BFS (which could follow a branch that only reaches
/// the Release on some paths) and removes the arbitrary `BFS_BLOCK_LIMIT` cap.
///
/// For each block on the chain:
///   - If it contains an unclaimed Release of `temp`, that is the candidate.
///   - If it contains interference (call/alloc/escape) or redefines `temp`,
///     we stop walking (path is tainted).
///   - Otherwise keep walking to the next immediate post-dominator.
///
/// We only accept the pair when the release block post-dominates the retain
/// block — guaranteed by construction here since every block on the idom chain
/// post-dominates the origin.
fn find_paired_release_cross_block(
    temp: Temp,
    origin_block_idx: usize,
    func: &LinFunction,
    block_index: &HashMap<BlockId, usize>,
    claimed: &HashSet<(usize, usize)>,
    postdom: &PostDom,
    conv_map: &HashMap<FuncId, Vec<Convention>>,
) -> Option<(usize, usize)> {
    let origin_id = func.blocks[origin_block_idx].id;

    // Walk the immediate post-dominator chain.  We build it by following
    // `idom` at each step, stopping when we revisit a node (cycle guard) or
    // reach a node that no longer post-dominates the origin.
    let mut current_id = origin_id;
    let mut visited: HashSet<BlockId> = HashSet::new();
    visited.insert(origin_id);

    loop {
        let Some(next_id) = postdom.idom(current_id) else { break };
        // Cycle guard (idom of the exit node may point to itself).
        if visited.contains(&next_id) {
            break;
        }
        visited.insert(next_id);

        // Every block on the idom chain post-dominates the origin by definition —
        // no need to re-check `post_dominates`.
        let Some(&idx) = block_index.get(&next_id) else { break };
        let block = &func.blocks[idx];

        // Does this block contain the Release?
        if let Some(release_pos) =
            find_release_at_block_start(temp, block, |i| claimed.contains(&(idx, i)))
        {
            return Some((idx, release_pos));
        }

        // If the block is tainted (interference or temp redefined) we cannot
        // skip over it; stop the walk.
        if !block_is_clean_for(temp, block, conv_map) || !block_temp_survives(temp, block) {
            break;
        }

        current_id = next_id;
    }

    None
}

/// Find the index of the first Release for `temp` in `block`.
/// Returns `None` if not found, or if a redefinition appears before the Release.
fn find_release_at_block_start(
    temp: Temp,
    block: &BasicBlock,
    is_claimed: impl Fn(usize) -> bool,
) -> Option<usize> {
    for (i, instr) in block.instructions.iter().enumerate() {
        match instr {
            Instruction::Release { val, .. } if *val == temp && is_claimed(i) => continue,
            Instruction::Release { val, .. } if *val == temp => return Some(i),
            other => {
                let (_uses, defs) = crate::liveness::instr_use_def(other);
                if defs.contains(&temp) {
                    return None;
                }
            }
        }
    }
    None
}

/// Returns true if `block` contains no call, allocation, or Release of `temp`
/// (i.e., the block is safe to traverse for cross-block elision).
fn block_is_clean_for(
    temp: Temp,
    block: &BasicBlock,
    conv_map: &HashMap<FuncId, Vec<Convention>>,
) -> bool {
    for instr in &block.instructions {
        if instr_is_interference(temp, instr, conv_map) {
            return false;
        }
    }
    true
}

/// An instruction "interferes" with a Retain/Release pair around `temp` if it could
/// observe the refcount or create an independent owner — in which case the pair is NOT
/// redundant and must be kept. This covers two categories:
///   - calls/allocations that may alias or trigger reuse, and
///   - *escapes*: instructions that store `temp` (or any value) into a longer-lived
///     location (a heap cell, an array/object slot, a module global) that will release
///     its own reference later. A retain balancing such an escape is load-bearing; eliding
///     it causes a use-after-free when the second owner releases. The escape checks are
///     value-agnostic (any escape on the path taints it) to stay conservative.
///
/// Convention-aware exceptions: a `CallIntrinsic` or `Call { Direct(..) }` is NOT
/// interference for `temp` when every position where `temp` appears in the argument
/// list has a verified `Borrow` convention — meaning the callee does not retain,
/// store, or consume that argument, so it cannot create an independent owner.
/// `Named` and `Indirect` calls remain interference (we cannot know their conventions
/// at this call site).
fn instr_is_interference(
    temp: Temp,
    instr: &Instruction,
    conv_map: &HashMap<FuncId, Vec<Convention>>,
) -> bool {
    match instr {
        // CallIntrinsic: use the hand-audited intrinsic convention table. If temp
        // only appears at Borrow or Inout positions, this call doesn't create a new owner.
        // Inout is safe here: the callee mutates the value in-place but does not retain,
        // store, return, or otherwise extend its lifetime — no independent owner is created.
        Instruction::CallIntrinsic { intrinsic, args, .. } => {
            if let Some(ic) = intrinsic_conventions(intrinsic) {
                // For each position where temp appears, check the convention.
                // If ALL such positions are Borrow or Inout, no interference.
                let all_non_escaping = args.iter().enumerate().all(|(i, &a)| {
                    if a != temp {
                        return true; // irrelevant arg — not interference for THIS temp
                    }
                    matches!(
                        ic.params.get(i).copied().unwrap_or(Convention::Own),
                        Convention::Borrow | Convention::Inout
                    )
                });
                if all_non_escaping {
                    return false;
                }
            }
            true
        }
        // Direct call to a known function: use the inferred convention table.
        // Named/Indirect calls remain interference (unknown conventions).
        Instruction::Call { callee: CallTarget::Direct(fid), args, .. } => {
            if let Some(convs) = conv_map.get(fid) {
                // Borrow or Inout positions do not create an independent owner: the callee
                // only reads (Borrow) or mutates in-place (Inout) the value; it cannot retain,
                // store, return, or escape it. Either convention is safe to elide around.
                let all_non_escaping = args.iter().enumerate().all(|(i, &a)| {
                    if a != temp {
                        return true;
                    }
                    matches!(
                        convs.get(i).copied().unwrap_or(Convention::Own),
                        Convention::Borrow | Convention::Inout
                    )
                });
                if all_non_escaping {
                    return false;
                }
            }
            true
        }
        Instruction::Call { callee: CallTarget::Named(_) | CallTarget::Indirect(_), .. }
        | Instruction::MakeObject { .. }
        | Instruction::MakeArray { .. }
        | Instruction::MakeClosure { .. }
        // Escapes — these create an independent owner of a stored value.
        | Instruction::MakeCell { .. }
        | Instruction::CellSet { .. }
        | Instruction::IndexSet { .. }
        | Instruction::GlobalValSet { .. } => true,
        Instruction::Release { val, .. } if *val == temp => true,
        // An intervening SECOND Retain of the same temp disqualifies eliding ACROSS it:
        // the one-to-one pairing of this Retain to a later Release would otherwise span a
        // sibling Retain whose own Release lies further on. Treating it as interference keeps
        // each Retain paired only with a Release in its own clean span and never elides over a
        // nested same-temp retain (fail toward not-eliding; ADR rc-elide-liveness).
        Instruction::Retain { val, .. } if *val == temp => true,
        _ => false,
    }
}

/// Returns true if `temp` is not redefined or released anywhere in `block`
/// (so it could still be live at the end of the block).
fn block_temp_survives(temp: Temp, block: &BasicBlock) -> bool {
    for instr in &block.instructions {
        match instr {
            Instruction::Release { val, .. } if *val == temp => return false,
            other => {
                let (_uses, defs) = crate::liveness::instr_use_def(other);
                if defs.contains(&temp) {
                    return false;
                }
            }
        }
    }
    true
}

/// The documented liveness safety net: a Retain/Release pair is only elided when
/// the temp is dead immediately AFTER the matched Release — i.e. the Release is a
/// true last-use (the final drop of that reference). If the temp is still live
/// past the Release, another owning reference is in play and dropping this pair
/// would either over-release that path or leave a live value with too few
/// references; we keep the pair.
///
/// Conservative on the block-exit case: if the Release is the block's last
/// instruction we treat the temp as live-after when EITHER `live_out` contains it
/// OR the terminator uses it directly (the latter is not folded into `live_out`).
fn release_is_last_use(
    liveness: &Liveness,
    block: &BasicBlock,
    release_idx: usize,
    temp: Temp,
) -> bool {
    let len = block.instructions.len();
    let live_after_instr = liveness.is_live_after(block.id, release_idx, len, temp);
    if live_after_instr {
        return false;
    }
    // If this is the final instruction, also reject if the terminator uses the temp.
    if release_idx + 1 >= len && terminator_uses_temp(&block.terminator, temp) {
        return false;
    }
    true
}

/// True if the terminator directly uses `temp` (return value / branch condition /
/// switch scrutinee / tail-call argument).
fn terminator_uses_temp(term: &Terminator, temp: Temp) -> bool {
    match term {
        Terminator::Return(Some(t)) => *t == temp,
        Terminator::CondJump { cond, .. } => *cond == temp,
        Terminator::Switch { val, .. } => *val == temp,
        Terminator::TailCall { args } => args.contains(&temp),
        _ => false,
    }
}

/// Post-dominator information over a function's CFG.
///
/// A block `p` post-dominates block `b` if every path from `b` to a function
/// exit (a `Return`/`TailCall`/`Unreachable` block) passes through `p`. We use
/// this so that a cross-block Release is only paired+elided with a Retain when
/// the Release is reached on EVERY path leaving the Retain — otherwise the
/// Retain leaks on the paths that bypass the Release.
struct PostDom {
    /// `post_dom[b]` = the set of blocks that post-dominate `b` (including `b`).
    post_dom: HashMap<BlockId, HashSet<BlockId>>,
    /// `idom_map[b]` = the immediate post-dominator of `b` (the closest strict
    /// post-dominator).  Absent for exit blocks (no strict post-dominator).
    idom_map: HashMap<BlockId, BlockId>,
}

impl PostDom {
    fn compute(func: &LinFunction) -> Self {
        let all: HashSet<BlockId> = func.blocks.iter().map(|b| b.id).collect();
        let succs: HashMap<BlockId, Vec<BlockId>> = func
            .blocks
            .iter()
            .map(|b| (b.id, terminator_successors(&b.terminator)))
            .collect();

        // Standard iterative post-dominator dataflow:
        //   pdom[exit]  = {exit}
        //   pdom[b]     = {b} ∪ (⋂ over successors s of pdom[s])
        // Initialise every non-exit block's set to the full set so the
        // intersection converges downward to a fixpoint.
        let mut post_dom: HashMap<BlockId, HashSet<BlockId>> = HashMap::new();
        for b in &func.blocks {
            let s = terminator_successors(&b.terminator);
            if s.is_empty() {
                // Exit block: post-dominated only by itself.
                let mut set = HashSet::new();
                set.insert(b.id);
                post_dom.insert(b.id, set);
            } else {
                post_dom.insert(b.id, all.clone());
            }
        }

        let mut changed = true;
        while changed {
            changed = false;
            for b in &func.blocks {
                let s = &succs[&b.id];
                if s.is_empty() {
                    continue; // exit block: fixed at {b}
                }
                // Intersection of successors' post-dom sets.
                let mut inter: Option<HashSet<BlockId>> = None;
                for succ in s {
                    let Some(sd) = post_dom.get(succ) else { continue };
                    inter = Some(match inter {
                        None => sd.clone(),
                        Some(acc) => acc.intersection(sd).copied().collect(),
                    });
                }
                let mut new_set = inter.unwrap_or_default();
                new_set.insert(b.id);
                if new_set != post_dom[&b.id] {
                    post_dom.insert(b.id, new_set);
                    changed = true;
                }
            }
        }

        // Derive the immediate post-dominator for each block.
        // idom(b) = the strict post-dominator of b with the LARGEST post-dom set
        // (i.e. the one closest to b in the post-dom tree, since nodes further from
        // the exit have larger post-dom sets).
        let mut idom_map: HashMap<BlockId, BlockId> = HashMap::new();
        for b in &func.blocks {
            let best = post_dom[&b.id]
                .iter()
                .filter(|&&p| p != b.id)
                .max_by_key(|&&p| post_dom.get(&p).map(|s| s.len()).unwrap_or(0))
                .copied();
            if let Some(p) = best {
                idom_map.insert(b.id, p);
            }
        }

        PostDom { post_dom, idom_map }
    }

    /// True if `p` post-dominates `b` (every path from `b` to an exit goes
    /// through `p`). When `b` has no recorded post-dom set (unreachable), this
    /// returns false — we never elide on the basis of unreachable info.
    fn post_dominates(&self, p: BlockId, b: BlockId) -> bool {
        self.post_dom.get(&b).map(|set| set.contains(&p)).unwrap_or(false)
    }

    /// Returns the immediate post-dominator of `b`, or `None` for exit blocks.
    fn idom(&self, b: BlockId) -> Option<BlockId> {
        self.idom_map.get(&b).copied()
    }
}

/// Extract successor BlockIds from a terminator.
fn terminator_successors(term: &Terminator) -> Vec<BlockId> {
    match term {
        Terminator::Jump(b) => vec![*b],
        Terminator::CondJump { then_block, else_block, .. } => vec![*then_block, *else_block],
        Terminator::Switch { cases, default, .. } => {
            let mut v: Vec<BlockId> = cases.iter().map(|(_, b)| *b).collect();
            v.push(*default);
            v
        }
        Terminator::Return(_) | Terminator::TailCall { .. } | Terminator::Unreachable => vec![],
    }
}

/// Check that instructions in the range `(start_exclusive, end_exclusive)` of
/// `instrs` contain no interference for `temp`.
///
/// Special case: when `start_exclusive == usize::MAX`, the check starts from
/// instruction 0 (used for a block prefix starting at the beginning).
fn path_has_no_interference(
    temp: Temp,
    start_exclusive: usize,
    end_exclusive: usize,
    instrs: &[Instruction],
    conv_map: &HashMap<FuncId, Vec<Convention>>,
) -> bool {
    let start = if start_exclusive == usize::MAX { 0 } else { start_exclusive + 1 };
    let end = end_exclusive.min(instrs.len());
    for i in start..end {
        if instr_is_interference(temp, &instrs[i], conv_map) {
            return false;
        }
    }
    true
}

// ===========================================================================
// CloneBox-read elision
// ===========================================================================
//
// Eliminates `CloneBox { dst, src }` / `Release { val: dst }` pairs where
// `dst` is ONLY consumed by Borrow-convention instructions (comparisons,
// unbox-to-scalar, equality tests — all tagged_eq/unbox callers) and `src`
// (the borrowed interior pointer that `lin_map_get` / `lin_object_get`
// returns) remains valid throughout the span.
//
// The transformation:
//   before:  CloneBox { dst, src }  ...uses of dst at Borrow positions...  Release { dst }
//   after:   ...uses with dst replaced by src...
//
// Soundness requirements (all checked before eliding):
//   1. All uses of `dst` in every block where `dst` is live are at verified
//      Borrow-convention positions — the existing interference predicate
//      rejects Own-convention uses, escapes, stores, and non-Borrow calls.
//   2. `src` is not released or redefined in any block where `dst` is live —
//      the borrowed pointer stays valid for all reads.
//   3. The Release block post-dominates the CloneBox block — the Release is
//      reached on every execution path through the CloneBox.
//
// Why this is sound for `lin_map_get` / `lin_object_get` results:
//   Both return a pointer into the map/object's key-value backing store. That
//   pointer remains valid as long as the container is not mutated or freed.
//   The interference check rejects any `IndexSet`, `FieldSet`, `CellSet`,
//   `GlobalValSet`, or any non-Borrow call — exactly the set that could
//   mutate the backing store or drop the container.
//
// Cross-block support:
//   The CloneBox is typically in one block and the Release in a downstream
//   merge block (post-dominator). Uses of `dst` appear in intermediate
//   if-then/else blocks. The pass collects all live blocks for `dst`, checks
//   ALL of them, and applies the substitution in every live block.

/// A CloneBox/Release pair that is safe to elide (cross-block).
struct CloneBoxElision {
    /// The block containing the CloneBox.
    clone_block_idx: usize,
    clone_instr_idx: usize,
    /// The block containing the Release.
    release_block_idx: usize,
    release_instr_idx: usize,
    dst: Temp,
    src: Temp,
    /// All (block_idx, instr_idx) pairs where `dst` must be substituted by `src`.
    /// Does NOT include the CloneBox or Release themselves (those are removed).
    substitutions: Vec<(usize, usize)>,
}

fn elide_clonebox_reads_fn(func: &mut LinFunction, conv_map: &HashMap<FuncId, Vec<Convention>>) {
    // Compute liveness and post-dominators once.
    let liveness = Liveness::compute(func);
    let postdom = PostDom::compute(func);
    let block_index: HashMap<BlockId, usize> = func.blocks.iter().enumerate().map(|(i, b)| (b.id, i)).collect();

    let mut elisions: Vec<CloneBoxElision> = Vec::new();

    // For each block, scan for CloneBox instructions.
    for clone_block_idx in 0..func.blocks.len() {
        let clone_block_id = func.blocks[clone_block_idx].id;
        let instrs = func.blocks[clone_block_idx].instructions.clone();

        for (clone_instr_idx, instr) in instrs.iter().enumerate() {
            let (dst, src) = match instr {
                Instruction::CloneBox { dst, src, ty } if is_union_clonebox_ty(ty) => (*dst, *src),
                _ => continue,
            };

            // STEP 1: Find the Release for `dst` using the idom chain (same as cross-block
            // Retain/Release search). The Release must post-dominate the CloneBox block.
            let release_loc = find_clonebox_release_cross_block(
                dst,
                clone_block_idx,
                clone_instr_idx,
                func,
                &block_index,
                &postdom,
                conv_map,
            );
            let (release_block_idx, release_instr_idx) = match release_loc {
                Some(x) => x,
                None => continue,
            };
            let release_block_id = func.blocks[release_block_idx].id;

            // STEP 2: Collect all blocks where `dst` is live (live_in or live_out).
            // These are the blocks that have uses of `dst` and need checking.
            // We only need blocks that are between the CloneBox and the Release:
            // those dominated by clone_block and that post-dom from which release
            // is reachable. Using liveness is sufficient: `dst` is live in exactly
            // those blocks.
            //
            // Special cases:
            // - Clone block: live from clone_instr_idx+1 to end of block (may flow out).
            // - Release block: live from start to release_instr_idx (exclusive).
            // - Intermediate blocks: `dst ∈ live_in`.

            // Collect all block_indices where dst is live_in (intermediate blocks):
            let live_blocks: Vec<usize> = func.blocks.iter().enumerate().filter_map(|(bi, b)| {
                if liveness.live_in.get(&b.id).map_or(false, |s| s.contains(&dst)) {
                    Some(bi)
                } else {
                    None
                }
            }).collect();

            // STEP 3: Check all blocks for sound elision.
            // For each block where dst is live, verify:
            //   (a) all uses of dst are at Borrow positions (no interference),
            //   (b) src is not invalidated.
            let mut all_clean = true;
            let mut substitutions: Vec<(usize, usize)> = Vec::new();

            // Check clone block (after clone_instr_idx).
            {
                let instrs_cb = &func.blocks[clone_block_idx].instructions;
                let start = clone_instr_idx + 1;
                let end = if clone_block_idx == release_block_idx {
                    release_instr_idx
                } else {
                    instrs_cb.len()
                };
                for i in start..end {
                    let instr = &instrs_cb[i];
                    if instr_uses_temp(instr, dst) {
                        if instr_is_interference(dst, instr, conv_map) {
                            all_clean = false;
                            break;
                        }
                        substitutions.push((clone_block_idx, i));
                    }
                    if src_instr_invalidates(src, instr) {
                        all_clean = false;
                        break;
                    }
                }
            }
            if !all_clean { continue; }

            // Check intermediate blocks (live_in blocks that are not the clone/release block).
            for &bi in &live_blocks {
                if bi == clone_block_idx || bi == release_block_idx { continue; }
                let instrs_b = &func.blocks[bi].instructions;
                let end = instrs_b.len();
                for i in 0..end {
                    let instr = &instrs_b[i];
                    if instr_uses_temp(instr, dst) {
                        if instr_is_interference(dst, instr, conv_map) {
                            all_clean = false;
                            break;
                        }
                        substitutions.push((bi, i));
                    }
                    if src_instr_invalidates(src, instr) {
                        all_clean = false;
                        break;
                    }
                }
                if !all_clean { break; }
            }
            if !all_clean { continue; }

            // Check release block (before release_instr_idx), if different from clone block.
            if release_block_idx != clone_block_idx {
                let instrs_rb = &func.blocks[release_block_idx].instructions;
                for i in 0..release_instr_idx {
                    let instr = &instrs_rb[i];
                    if instr_uses_temp(instr, dst) {
                        if instr_is_interference(dst, instr, conv_map) {
                            all_clean = false;
                            break;
                        }
                        substitutions.push((release_block_idx, i));
                    }
                    if src_instr_invalidates(src, instr) {
                        all_clean = false;
                        break;
                    }
                }
                if !all_clean { continue; }
            }

            // STEP 4: Verify Bm (release block) post-dominates B0 (clone block).
            if !postdom.post_dominates(release_block_id, clone_block_id) {
                continue;
            }

            elisions.push(CloneBoxElision {
                clone_block_idx,
                clone_instr_idx,
                release_block_idx,
                release_instr_idx,
                dst,
                src,
                substitutions,
            });
        }
    }

    if elisions.is_empty() {
        return;
    }

    // Apply elisions: substitute dst→src in all collected sites, remove CloneBox + Release.
    // Sort substitutions by (block_idx, instr_idx) ascending; removals descending per block.
    let mut to_remove: HashSet<(usize, usize)> = HashSet::new();
    let mut all_subs: Vec<(usize, usize, Temp, Temp)> = Vec::new(); // (bi, ii, old, new)

    for e in &elisions {
        to_remove.insert((e.clone_block_idx, e.clone_instr_idx));
        to_remove.insert((e.release_block_idx, e.release_instr_idx));
        for &(bi, ii) in &e.substitutions {
            all_subs.push((bi, ii, e.dst, e.src));
        }
    }

    // Apply substitutions (forward order within each block; index shifts from removals
    // happen AFTER substitution, so indices are still valid here).
    for (bi, ii, old, new) in &all_subs {
        substitute_temp_in_instr(&mut func.blocks[*bi].instructions[*ii], *old, *new);
    }

    // Remove in descending order per block.
    for block_idx in 0..func.blocks.len() {
        let mut remove_here: Vec<usize> = to_remove
            .iter()
            .filter(|(b, _)| *b == block_idx)
            .map(|(_, i)| *i)
            .collect();
        remove_here.sort_unstable_by(|a, b| b.cmp(a));
        for idx in remove_here {
            func.blocks[block_idx].instructions.remove(idx);
            if idx < func.blocks[block_idx].instr_spans.len() {
                func.blocks[block_idx].instr_spans.remove(idx);
            }
        }
    }
}

/// Returns true for union-typed CloneBox — the case where a fresh box is
/// allocated by `lin_tagged_clone`. Concrete-RC CloneBox degrades to a plain
/// Retain (no allocation), so the Retain/Release pass already handles it.
fn is_union_clonebox_ty(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Union(_) | Type::TypeVar(_) | Type::Named(_)
            | Type::Shared(_) | Type::Stream(_) | Type::Promise(_)
            | Type::Opaque(_)
    )
}

/// Returns true if `instr` uses `temp` in any position (use, def, or otherwise).
/// We use this to enumerate substitution sites.
fn instr_uses_temp(instr: &Instruction, temp: Temp) -> bool {
    let (uses, _) = crate::liveness::instr_use_def(instr);
    uses.contains(&temp)
}

/// Returns true if `instr` invalidates the borrowed source pointer `src`.
/// A source is invalidated when it is Released (container freed) or Redefined
/// (the slot now points to something else). We do NOT flag ownership-transferring
/// calls here because `src` is the borrowed result of `lin_map_get` (an interior
/// pointer); only releasing the CONTAINER that produced `src` would dangle it,
/// which the interference check on `dst` already prevents (any Own-convention use
/// of the container in a call is interference). Being conservative: release or
/// def of `src` itself invalidates it.
fn src_instr_invalidates(src: Temp, instr: &Instruction) -> bool {
    match instr {
        Instruction::Release { val, .. } if *val == src => return true,
        _ => {}
    }
    let (_, defs) = crate::liveness::instr_use_def(instr);
    defs.contains(&src)
}

/// Find the Release for `dst` using the idom post-dominator chain, starting from
/// the clone block. Returns `(release_block_idx, release_instr_idx)` when a clean,
/// post-dominating Release is found; `None` otherwise.
///
/// Mirrors `find_paired_release_cross_block` for Retain/Release, extended to:
///   - start the idom walk from the clone block itself (same-block release falls
///     through to the idom chain as well),
///   - scan from `clone_instr_idx+1` within the clone block (not from 0).
fn find_clonebox_release_cross_block(
    dst: Temp,
    clone_block_idx: usize,
    clone_instr_idx: usize,
    func: &LinFunction,
    block_index: &HashMap<BlockId, usize>,
    postdom: &PostDom,
    conv_map: &HashMap<FuncId, Vec<Convention>>,
) -> Option<(usize, usize)> {
    let clone_block_id = func.blocks[clone_block_idx].id;

    // Check same block first (scan from clone_instr_idx+1).
    let tail_instrs = &func.blocks[clone_block_idx].instructions;
    for i in (clone_instr_idx + 1)..tail_instrs.len() {
        match &tail_instrs[i] {
            Instruction::Release { val, .. } if *val == dst => {
                return Some((clone_block_idx, i));
            }
            other => {
                let (_, defs) = crate::liveness::instr_use_def(other);
                if defs.contains(&dst) {
                    return None; // dst redefined
                }
            }
        }
    }

    // Walk the idom chain from the clone block's immediate post-dominator.
    let mut current_id = clone_block_id;
    let mut visited: HashSet<BlockId> = HashSet::new();
    visited.insert(clone_block_id);

    loop {
        let Some(next_id) = postdom.idom(current_id) else { break };
        if visited.contains(&next_id) { break; }
        visited.insert(next_id);

        let Some(&idx) = block_index.get(&next_id) else { break };
        let block = &func.blocks[idx];

        // Search for the Release at the start of this block.
        if let Some(release_pos) = find_release_at_block_start(dst, block, |_| false) {
            return Some((idx, release_pos));
        }

        // If this block is tainted for `dst` or redefs it, stop.
        if !block_is_clean_for(dst, block, conv_map) || !block_temp_survives(dst, block) {
            break;
        }

        current_id = next_id;
    }

    None
}

/// Substitute `old_temp` → `new_temp` in all USE positions of `instr`.
/// Only replaces USE occurrences (input operands), never DEF positions
/// (which are outputs / destinations). This is safe because elision removes
/// the instruction that defined `old_temp`, so no instruction after the
/// CloneBox should define it — but we guard conservatively anyway.
fn substitute_temp_in_instr(instr: &mut Instruction, old: Temp, new: Temp) {
    macro_rules! sub {
        ($t:expr) => { if *$t == old { *$t = new; } };
    }
    match instr {
        Instruction::Copy { src, .. } => { sub!(src); }
        Instruction::Phi { incomings, .. } => {
            for (t, _) in incomings.iter_mut() { sub!(t); }
        }
        Instruction::Unary { operand, .. } => { sub!(operand); }
        Instruction::Binary { lhs, rhs, .. } => { sub!(lhs); sub!(rhs); }
        Instruction::Coerce { src, .. } => { sub!(src); }
        Instruction::Call { callee, args, .. } => {
            for a in args.iter_mut() { sub!(a); }
            if let CallTarget::Indirect(t) = callee { sub!(t); }
        }
        Instruction::CallIntrinsic { args, .. } => {
            for a in args.iter_mut() { sub!(a); }
        }
        Instruction::MakeClosure { captures, .. } => {
            for c in captures.iter_mut() { sub!(c); }
        }
        Instruction::MakeObject { fields, spreads, computed_fields, .. } => {
            for (_, v) in fields.iter_mut() { sub!(v); }
            for s in spreads.iter_mut() { sub!(s); }
            for (k, v) in computed_fields.iter_mut() { sub!(k); sub!(v); }
        }
        Instruction::MakeArray { elements, spreads, .. } => {
            for e in elements.iter_mut() { sub!(e); }
            for (_, t) in spreads.iter_mut() { sub!(t); }
        }
        Instruction::Index { object, key, .. } => { sub!(object); sub!(key); }
        Instruction::IndexSet { object, key, value, .. } => { sub!(object); sub!(key); sub!(value); }
        Instruction::FieldGet { object, .. } => { sub!(object); }
        Instruction::FieldSet { object, value, .. } => { sub!(object); sub!(value); }
        Instruction::SealedArrayFieldGet { array, index, .. } => { sub!(array); sub!(index); }
        Instruction::BoxedArrayFieldGet { array, index, .. } => { sub!(array); sub!(index); }
        Instruction::EnvCapture { env, .. } => { sub!(env); }
        Instruction::ArrayLenCheck { val, .. } => { sub!(val); }
        Instruction::ObjectRest { src, .. } => { sub!(src); }
        Instruction::GlobalValSet { value, .. } => { sub!(value); }
        Instruction::MakeCell { init, .. } => { sub!(init); }
        Instruction::CellGet { cell, .. } => { sub!(cell); }
        Instruction::CellSet { cell, value, .. } => { sub!(cell); sub!(value); }
        Instruction::FreeCell { cell, .. } => { sub!(cell); }
        Instruction::Retain { val, .. } => { sub!(val); }
        Instruction::Release { val, .. } => { sub!(val); }
        Instruction::CloneBox { src, .. } => { sub!(src); }  // dst is a def — not substituted
        Instruction::FreeBoxShell { val } => { sub!(val); }
        Instruction::FreeBoxShellIfDistinct { val, other } => { sub!(val); sub!(other); }
        Instruction::ReleaseIfDistinct { val, other } => { sub!(val); sub!(other); }
        Instruction::ReleaseRawIfDistinct { val, other, .. } => { sub!(val); sub!(other); }
        Instruction::IsType { val, .. } => { sub!(val); }
        Instruction::SumTagEq { val, .. } => { sub!(val); }
        Instruction::HasPattern { val, .. } => { sub!(val); }
        Instruction::MatchesSchema { val, .. } => { sub!(val); }
        Instruction::Box { val, .. } => { sub!(val); }
        Instruction::Unbox { val, .. } => { sub!(val); }
        Instruction::Bind { src, .. } => { sub!(src); }
        Instruction::Panic { msg } => { sub!(msg); }
        // No-use instructions: Const, GlobalValGet, MakeNamedClosure, DebugDeclare
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a single-block function with the given instructions.
    fn make_fn(id: FuncId, instrs: Vec<Instruction>) -> LinFunction {
        make_fn_with_term(id, instrs, Terminator::Return(None))
    }

    fn make_fn_with_term(id: FuncId, instrs: Vec<Instruction>, term: Terminator) -> LinFunction {
        let block = BasicBlock {
            id: BlockId(0),
            label: None,
            instructions: instrs,
            terminator: term,
            span: None,
            instr_spans: Vec::new(),
        };
        let mut temp_types = std::collections::HashMap::new();
        temp_types.insert(Temp(0), Type::Str);
        temp_types.insert(Temp(1), Type::Str);
        temp_types.insert(Temp(2), Type::Str);
        LinFunction {
            id,
            name: None,
            params: vec![],
            is_closure: false,
            ret_ty: Type::Null,
            param_conventions: Vec::new(),
            ret_convention: Convention::Own,
            blocks: vec![block],
            temp_types,
            temp_count: 3,
            intrinsic_slots: std::collections::HashMap::new(),
            repr: Vec::new(),
            coverage_origin: None,
            substr_fuse: std::collections::HashMap::new(),
            getset_fuse: std::collections::HashSet::new(),
        }
    }

    /// Build a two-block function:
    ///   block 0 → instrs0, terminates with Jump(BlockId(1))
    ///   block 1 → instrs1, terminates with Return(None)
    fn make_two_block_fn(
        id: FuncId,
        instrs0: Vec<Instruction>,
        instrs1: Vec<Instruction>,
    ) -> LinFunction {
        let block0 = BasicBlock {
            id: BlockId(0),
            label: None,
            instructions: instrs0,
            terminator: Terminator::Jump(BlockId(1)),
            span: None,
            instr_spans: Vec::new(),
        };
        let block1 = BasicBlock {
            id: BlockId(1),
            label: None,
            instructions: instrs1,
            terminator: Terminator::Return(None),
            span: None,
            instr_spans: Vec::new(),
        };
        let mut temp_types = std::collections::HashMap::new();
        temp_types.insert(Temp(0), Type::Str);
        temp_types.insert(Temp(1), Type::Str);
        temp_types.insert(Temp(2), Type::Str);
        LinFunction {
            id,
            name: None,
            params: vec![],
            is_closure: false,
            ret_ty: Type::Null,
            param_conventions: Vec::new(),
            ret_convention: Convention::Own,
            blocks: vec![block0, block1],
            temp_types,
            temp_count: 3,
            intrinsic_slots: std::collections::HashMap::new(),
            repr: Vec::new(),
            coverage_origin: None,
            substr_fuse: std::collections::HashMap::new(),
            getset_fuse: std::collections::HashSet::new(),
        }
    }

    /// Build a three-block function:
    ///   block 0 → instrs0, terminates with Jump(BlockId(1))
    ///   block 1 → instrs1, terminates with Jump(BlockId(2))
    ///   block 2 → instrs2, terminates with Return(None)
    fn make_three_block_fn(
        id: FuncId,
        instrs0: Vec<Instruction>,
        instrs1: Vec<Instruction>,
        instrs2: Vec<Instruction>,
    ) -> LinFunction {
        let block0 = BasicBlock {
            id: BlockId(0),
            label: None,
            instructions: instrs0,
            terminator: Terminator::Jump(BlockId(1)),
            span: None,
            instr_spans: Vec::new(),
        };
        let block1 = BasicBlock {
            id: BlockId(1),
            label: None,
            instructions: instrs1,
            terminator: Terminator::Jump(BlockId(2)),
            span: None,
            instr_spans: Vec::new(),
        };
        let block2 = BasicBlock {
            id: BlockId(2),
            label: None,
            instructions: instrs2,
            terminator: Terminator::Return(None),
            span: None,
            instr_spans: Vec::new(),
        };
        let mut temp_types = std::collections::HashMap::new();
        temp_types.insert(Temp(0), Type::Str);
        temp_types.insert(Temp(1), Type::Str);
        temp_types.insert(Temp(2), Type::Str);
        LinFunction {
            id,
            name: None,
            params: vec![],
            is_closure: false,
            ret_ty: Type::Null,
            param_conventions: Vec::new(),
            ret_convention: Convention::Own,
            blocks: vec![block0, block1, block2],
            temp_types,
            temp_count: 3,
            intrinsic_slots: std::collections::HashMap::new(),
            repr: Vec::new(),
            coverage_origin: None,
            substr_fuse: std::collections::HashMap::new(),
            getset_fuse: std::collections::HashSet::new(),
        }
    }

    fn make_module(func: LinFunction) -> LinModule {
        LinModule {
            functions: vec![func],
            global_fn_slots: std::collections::HashMap::new(),
            intrinsics: std::collections::HashMap::new(),
            default_descriptors: std::collections::HashMap::new(),
        }
    }

    // -------------------------------------------------------------------------
    // Existing single-block tests (regression)
    // -------------------------------------------------------------------------

    #[test]
    fn elides_adjacent_retain_release_with_no_interference() {
        // Retain(t0) followed immediately by Release(t0) with t0 still live = elide both.
        let instrs = vec![
            Instruction::Retain { val: Temp(0), ty: Type::Str },
            // Some use of t0 that keeps it live.
            Instruction::Copy { dst: Temp(1), src: Temp(0) },
            Instruction::Release { val: Temp(0), ty: Type::Str },
        ];
        let mut module = make_module(make_fn(FuncId(0), instrs));
        elide_rc(&mut module);
        let remaining = &module.functions[0].blocks[0].instructions;
        assert!(
            !remaining.iter().any(|i| matches!(i, Instruction::Retain { .. })),
            "Retain should be elided"
        );
        assert!(
            !remaining.iter().any(|i| matches!(i, Instruction::Release { .. })),
            "Release should be elided"
        );
    }

    /// Two Retains and two Releases of the same temp in one block (e.g. a heap parameter read
    /// twice) must elide ONE-TO-ONE: exactly one Retain/Release pair removed, one of each kept.
    /// Regression for the flat-array-arg-used-twice use-after-free, where both Retains paired to
    /// the first Release — eliding two Retains but (set-deduped) only one Release, leaving an
    /// unbalanced extra Release.
    #[test]
    fn elides_one_to_one_for_double_retain_double_release() {
        // Retain(t0), Copy, Retain(t0), Copy, Release(t0), Release(t0) — net balanced.
        let instrs = vec![
            Instruction::Retain { val: Temp(0), ty: Type::Str },
            Instruction::Copy { dst: Temp(1), src: Temp(0) },
            Instruction::Retain { val: Temp(0), ty: Type::Str },
            Instruction::Copy { dst: Temp(2), src: Temp(0) },
            Instruction::Release { val: Temp(0), ty: Type::Str },
            Instruction::Release { val: Temp(0), ty: Type::Str },
        ];
        let mut module = make_module(make_fn(FuncId(0), instrs));
        elide_rc(&mut module);
        let remaining = &module.functions[0].blocks[0].instructions;
        let retains = remaining.iter().filter(|i| matches!(i, Instruction::Retain { .. })).count();
        let releases = remaining.iter().filter(|i| matches!(i, Instruction::Release { .. })).count();
        // Balance must be preserved: equal counts of Retain and Release survive.
        assert_eq!(
            retains, releases,
            "retain/release counts must stay balanced (got {retains} retains, {releases} releases)"
        );
    }

    #[test]
    fn keeps_retain_release_with_call_in_between() {
        // Retain(t0) + Call + Release(t0): cannot elide because the call may alias.
        let instrs = vec![
            Instruction::Retain { val: Temp(0), ty: Type::Str },
            Instruction::Call {
                dst: Temp(1),
                callee: CallTarget::Named("foo".into()),
                args: vec![],
                ret_ty: Type::Null,
            },
            Instruction::Release { val: Temp(0), ty: Type::Str },
        ];
        let mut module = make_module(make_fn(FuncId(0), instrs));
        elide_rc(&mut module);
        let remaining = &module.functions[0].blocks[0].instructions;
        assert!(
            remaining.iter().any(|i| matches!(i, Instruction::Retain { .. })),
            "Retain should be kept"
        );
        assert!(
            remaining.iter().any(|i| matches!(i, Instruction::Release { .. })),
            "Release should be kept"
        );
    }

    // -------------------------------------------------------------------------
    // New cross-block tests
    // -------------------------------------------------------------------------

    /// Retain in block 0, Release in block 1 (direct successor), no interference
    /// anywhere — elide both.
    #[test]
    fn cross_block_elides_retain_release_clean_path() {
        // block 0: Retain(t0), Copy(t1, t0)  → Jump block 1
        // block 1: Release(t0)               → Return
        let instrs0 = vec![
            Instruction::Retain { val: Temp(0), ty: Type::Str },
            Instruction::Copy { dst: Temp(1), src: Temp(0) },
        ];
        let instrs1 = vec![
            Instruction::Release { val: Temp(0), ty: Type::Str },
        ];
        let mut module = make_module(make_two_block_fn(FuncId(0), instrs0, instrs1));
        elide_rc(&mut module);
        let b0 = &module.functions[0].blocks[0].instructions;
        let b1 = &module.functions[0].blocks[1].instructions;
        assert!(
            !b0.iter().any(|i| matches!(i, Instruction::Retain { .. })),
            "Retain in block 0 should be elided"
        );
        assert!(
            !b1.iter().any(|i| matches!(i, Instruction::Release { .. })),
            "Release in block 1 should be elided"
        );
    }

    /// Retain in block 0 with a Call also in block 0, Release in block 1 —
    /// path is tainted by the call, so keep both.
    #[test]
    fn cross_block_keeps_when_call_in_retain_block() {
        // block 0: Retain(t0), Call(t1, "foo", [])  → Jump block 1
        // block 1: Release(t0)                       → Return
        let instrs0 = vec![
            Instruction::Retain { val: Temp(0), ty: Type::Str },
            Instruction::Call {
                dst: Temp(1),
                callee: CallTarget::Named("foo".into()),
                args: vec![],
                ret_ty: Type::Null,
            },
        ];
        let instrs1 = vec![
            Instruction::Release { val: Temp(0), ty: Type::Str },
        ];
        let mut module = make_module(make_two_block_fn(FuncId(0), instrs0, instrs1));
        elide_rc(&mut module);
        let b0 = &module.functions[0].blocks[0].instructions;
        let b1 = &module.functions[0].blocks[1].instructions;
        assert!(
            b0.iter().any(|i| matches!(i, Instruction::Retain { .. })),
            "Retain should be kept (call in path)"
        );
        assert!(
            b1.iter().any(|i| matches!(i, Instruction::Release { .. })),
            "Release should be kept (call in path)"
        );
    }

    /// Retain in block 0, intermediate block 1 has a call, Release in block 2 —
    /// path through block 1 is tainted, so keep both.
    #[test]
    fn cross_block_keeps_when_call_in_intermediate_block() {
        // block 0: Retain(t0)                        → Jump block 1
        // block 1: Call(t1, "bar", [])               → Jump block 2
        // block 2: Release(t0)                        → Return
        let instrs0 = vec![
            Instruction::Retain { val: Temp(0), ty: Type::Str },
        ];
        let instrs1 = vec![
            Instruction::Call {
                dst: Temp(1),
                callee: CallTarget::Named("bar".into()),
                args: vec![],
                ret_ty: Type::Null,
            },
        ];
        let instrs2 = vec![
            Instruction::Release { val: Temp(0), ty: Type::Str },
        ];
        let mut module =
            make_module(make_three_block_fn(FuncId(0), instrs0, instrs1, instrs2));
        elide_rc(&mut module);
        let b0 = &module.functions[0].blocks[0].instructions;
        let b2 = &module.functions[0].blocks[2].instructions;
        assert!(
            b0.iter().any(|i| matches!(i, Instruction::Retain { .. })),
            "Retain should be kept (call in intermediate block)"
        );
        assert!(
            b2.iter().any(|i| matches!(i, Instruction::Release { .. })),
            "Release should be kept (call in intermediate block)"
        );
    }

    /// Retain in block 0, Release in block 1, temp NOT in live_out of block 1
    /// (last-use scenario). Path is clean. Both Retain and Release are elided.
    ///
    /// Note: because t0 is not returned and is not used in any successor after
    /// block 1 (block 1 terminates with Return), it is not in live_out of block 1.
    /// The liveness analysis confirms this, but elision logic is symmetric with
    /// the clean-path case — we elide both when the path is clean.
    #[test]
    fn cross_block_last_use_elides_retain_and_release() {
        // block 0: Retain(t0)    → Jump block 1
        // block 1: Release(t0)   → Return(None)
        // t0 is NOT in live_out of block 1 (last use).
        let instrs0 = vec![
            Instruction::Retain { val: Temp(0), ty: Type::Str },
        ];
        let instrs1 = vec![
            Instruction::Release { val: Temp(0), ty: Type::Str },
        ];
        let func = make_two_block_fn(FuncId(0), instrs0, instrs1);

        // Verify liveness: t0 should NOT be in live_out of block 1.
        let liveness = Liveness::compute(&func);
        let live_out_b1 = liveness.live_out.get(&BlockId(1)).cloned().unwrap_or_default();
        assert!(
            !live_out_b1.contains(&Temp(0)),
            "t0 should not be live_out of block 1 (last use)"
        );

        let mut module = make_module(func);
        elide_rc(&mut module);
        let b0 = &module.functions[0].blocks[0].instructions;
        let b1 = &module.functions[0].blocks[1].instructions;
        assert!(
            !b0.iter().any(|i| matches!(i, Instruction::Retain { .. })),
            "Retain should be elided on last-use clean path"
        );
        assert!(
            !b1.iter().any(|i| matches!(i, Instruction::Release { .. })),
            "Release should be elided on last-use clean path"
        );
    }

    // -------------------------------------------------------------------------
    // Soundness regression tests for the two RC-elision holes (fix/rc-elide-liveness)
    // -------------------------------------------------------------------------

    /// Count remaining Retain/Release of a temp in a whole function.
    fn count_rc(func: &LinFunction, temp: Temp) -> (usize, usize) {
        let mut retains = 0;
        let mut releases = 0;
        for block in &func.blocks {
            for instr in &block.instructions {
                match instr {
                    Instruction::Retain { val, .. } if *val == temp => retains += 1,
                    Instruction::Release { val, .. } if *val == temp => releases += 1,
                    _ => {}
                }
            }
        }
        (retains, releases)
    }

    /// HOLE 1: an intervening SECOND Retain of the same temp between an outer
    /// Retain and the single Release. The temp is retained TWICE but released
    /// ONCE (net +1 — e.g. the second retain balances an escape/return that is
    /// off the modelled path). RC elision must preserve the net refcount: it may
    /// only remove a *balanced* pair, never change the net count.
    #[test]
    fn does_not_unbalance_with_intervening_same_temp_retain() {
        // Retain(t0); Copy(t1,t0); Retain(t0); Copy(t2,t0); Release(t0)
        // 2 retains, 1 release (net +1).
        let instrs = vec![
            Instruction::Retain { val: Temp(0), ty: Type::Str },
            Instruction::Copy { dst: Temp(1), src: Temp(0) },
            Instruction::Retain { val: Temp(0), ty: Type::Str },
            Instruction::Copy { dst: Temp(2), src: Temp(0) },
            Instruction::Release { val: Temp(0), ty: Type::Str },
        ];
        let mut module = make_module(make_fn(FuncId(0), instrs));
        let before = count_rc(&module.functions[0], Temp(0));
        assert_eq!(before, (2, 1), "precondition: input is net +1");
        elide_rc(&mut module);
        let (retains, releases) = count_rc(&module.functions[0], Temp(0));
        let net_before = before.0 as i64 - before.1 as i64;
        let net_after = retains as i64 - releases as i64;
        assert_eq!(
            net_after, net_before,
            "RC elision must preserve net refcount (before net {net_before}, after net {net_after}); \
             got {retains} retains, {releases} releases"
        );
    }

    /// HOLE 1 (balanced variant): Retain; Retain; Copy; Release; Release — two of
    /// each, net 0. The pass may elide pairs but must keep retains == releases.
    #[test]
    fn balanced_double_retain_adjacent_stays_balanced() {
        let instrs = vec![
            Instruction::Retain { val: Temp(0), ty: Type::Str },
            Instruction::Retain { val: Temp(0), ty: Type::Str },
            Instruction::Copy { dst: Temp(1), src: Temp(0) },
            Instruction::Release { val: Temp(0), ty: Type::Str },
            Instruction::Release { val: Temp(0), ty: Type::Str },
        ];
        let mut module = make_module(make_fn(FuncId(0), instrs));
        elide_rc(&mut module);
        let (retains, releases) = count_rc(&module.functions[0], Temp(0));
        assert_eq!(
            retains, releases,
            "retain/release counts must stay balanced (got {retains} retains, {releases} releases)"
        );
    }

    /// HOLE 3: cross-block Release that does NOT post-dominate the Retain.
    /// block0 retains t0 then CondJumps to block1 (releases t0) or block2 (does
    /// NOT release t0). Eliding the Retain against the block1 Release leaks on the
    /// block2 path. The pass must NOT elide unless the Release post-dominates the
    /// Retain — here it does not, so both must survive.
    #[test]
    fn cross_block_keeps_when_release_does_not_postdominate() {
        let block0 = BasicBlock {
            id: BlockId(0),
            label: None,
            instructions: vec![Instruction::Retain { val: Temp(0), ty: Type::Str }],
            terminator: Terminator::CondJump {
                cond: Temp(1),
                then_block: BlockId(1),
                else_block: BlockId(2),
            },
            span: None,
            instr_spans: Vec::new(),
        };
        let block1 = BasicBlock {
            id: BlockId(1),
            label: None,
            instructions: vec![Instruction::Release { val: Temp(0), ty: Type::Str }],
            terminator: Terminator::Return(None),
            span: None,
            instr_spans: Vec::new(),
        };
        let block2 = BasicBlock {
            id: BlockId(2),
            label: None,
            instructions: vec![],
            terminator: Terminator::Return(None),
            span: None,
            instr_spans: Vec::new(),
        };
        let mut temp_types = std::collections::HashMap::new();
        temp_types.insert(Temp(0), Type::Str);
        temp_types.insert(Temp(1), Type::Bool);
        let func = LinFunction {
            id: FuncId(0),
            name: None,
            params: vec![],
            is_closure: false,
            ret_ty: Type::Null,
            param_conventions: Vec::new(),
            ret_convention: Convention::Own,
            blocks: vec![block0, block1, block2],
            temp_types,
            temp_count: 2,
            intrinsic_slots: std::collections::HashMap::new(),
            repr: Vec::new(),
            coverage_origin: None,
            substr_fuse: std::collections::HashMap::new(),
            getset_fuse: std::collections::HashSet::new(),
        };
        let mut module = make_module(func);
        elide_rc(&mut module);
        let func = &module.functions[0];
        let (retains, releases) = count_rc(func, Temp(0));
        assert_eq!(retains, 1, "Retain must be kept (release does not post-dominate)");
        assert_eq!(releases, 1, "Release must be kept (release does not post-dominate)");
    }

    /// Cross-block POSITIVE control: the Release DOES post-dominate (single
    /// successor chain), so eliding the pair is sound and must still happen.
    #[test]
    fn cross_block_elides_when_release_postdominates() {
        let instrs0 = vec![Instruction::Retain { val: Temp(0), ty: Type::Str }];
        let instrs1 = vec![Instruction::Release { val: Temp(0), ty: Type::Str }];
        let mut module = make_module(make_two_block_fn(FuncId(0), instrs0, instrs1));
        elide_rc(&mut module);
        let (retains, releases) = count_rc(&module.functions[0], Temp(0));
        assert_eq!((retains, releases), (0, 0), "clean post-dominating pair should elide");
    }

    /// Build an N-block linear chain:
    ///   block 0 → instrs0, Jump(1)
    ///   block 1 → [],       Jump(2)
    ///   ...
    ///   block N-1 → instrsN, Return
    fn make_linear_chain_fn(
        id: FuncId,
        instrs_first: Vec<Instruction>,
        depth: usize,
        instrs_last: Vec<Instruction>,
    ) -> LinFunction {
        let mut blocks = Vec::new();
        let total = depth + 2; // first + `depth` intermediates + last
        for i in 0..total {
            let instructions = if i == 0 {
                instrs_first.clone()
            } else if i == total - 1 {
                instrs_last.clone()
            } else {
                vec![]
            };
            let terminator = if i + 1 < total {
                Terminator::Jump(BlockId((i + 1) as u32))
            } else {
                Terminator::Return(None)
            };
            blocks.push(BasicBlock {
                id: BlockId(i as u32),
                label: None,
                instructions,
                terminator,
                span: None,
                instr_spans: Vec::new(),
            });
        }
        let mut temp_types = std::collections::HashMap::new();
        temp_types.insert(Temp(0), Type::Str);
        LinFunction {
            id,
            name: None,
            params: vec![],
            is_closure: false,
            ret_ty: Type::Null,
            param_conventions: Vec::new(),
            ret_convention: Convention::Own,
            blocks,
            temp_types,
            temp_count: 1,
            intrinsic_slots: std::collections::HashMap::new(),
            repr: Vec::new(),
            coverage_origin: None,
            substr_fuse: std::collections::HashMap::new(),
            getset_fuse: std::collections::HashSet::new(),
        }
    }

    /// Retain in block 0, Release in block 11 (10 clean intermediate blocks).
    /// Old BFS_BLOCK_LIMIT=8 would stop before reaching the Release; the
    /// post-dominator chain walk finds it because the chain post-dominates.
    #[test]
    fn deep_idom_chain_elides_beyond_old_bfs_limit() {
        let instrs_first = vec![Instruction::Retain { val: Temp(0), ty: Type::Str }];
        let instrs_last = vec![Instruction::Release { val: Temp(0), ty: Type::Str }];
        // 10 intermediate clean blocks → release is 11 idom hops away
        let func = make_linear_chain_fn(FuncId(0), instrs_first, 10, instrs_last);
        let mut module = make_module(func);
        elide_rc(&mut module);
        let (retains, releases) = count_rc(&module.functions[0], Temp(0));
        assert_eq!(
            (retains, releases),
            (0, 0),
            "deep idom chain (>8 blocks) should still elide"
        );
    }

    /// Retain in block 0, interference in block 5 (a call), Release in block 11.
    /// The idom chain walk stops at the interference block; both must be kept.
    #[test]
    fn deep_idom_chain_stops_at_interference() {
        // Build a 12-block chain manually: Retain at 0, Call at block 5, Release at 11.
        let total = 12usize;
        let mut blocks = Vec::new();
        for i in 0..total {
            let instructions = match i {
                0 => vec![Instruction::Retain { val: Temp(0), ty: Type::Str }],
                5 => vec![Instruction::Call {
                    dst: Temp(0), // reuse slot (won't matter, path is blocked)
                    callee: CallTarget::Named("side_effect".into()),
                    args: vec![],
                    ret_ty: Type::Null,
                }],
                11 => vec![Instruction::Release { val: Temp(0), ty: Type::Str }],
                _ => vec![],
            };
            let terminator = if i + 1 < total {
                Terminator::Jump(BlockId((i + 1) as u32))
            } else {
                Terminator::Return(None)
            };
            blocks.push(BasicBlock {
                id: BlockId(i as u32),
                label: None,
                instructions,
                terminator,
                span: None,
                instr_spans: Vec::new(),
            });
        }
        let mut temp_types = std::collections::HashMap::new();
        temp_types.insert(Temp(0), Type::Str);
        let func = LinFunction {
            id: FuncId(0),
            name: None,
            params: vec![],
            is_closure: false,
            ret_ty: Type::Null,
            param_conventions: Vec::new(),
            ret_convention: Convention::Own,
            blocks,
            temp_types,
            temp_count: 1,
            intrinsic_slots: std::collections::HashMap::new(),
            repr: Vec::new(),
            coverage_origin: None,
            substr_fuse: std::collections::HashMap::new(),
            getset_fuse: std::collections::HashSet::new(),
        };
        let mut module = make_module(func);
        elide_rc(&mut module);
        let (retains, releases) = count_rc(&module.functions[0], Temp(0));
        assert_eq!(retains, 1, "Retain must be kept (interference in intermediate block)");
        assert_eq!(releases, 1, "Release must be kept (interference in intermediate block)");
    }

    // -------------------------------------------------------------------------
    // CloneBox-read elision tests
    // -------------------------------------------------------------------------

    /// Count CloneBox instructions in a function (for a specific dst temp).
    fn count_clonebox(func: &LinFunction, dst: Temp) -> usize {
        func.blocks.iter().flat_map(|b| &b.instructions).filter(|i| {
            matches!(i, Instruction::CloneBox { dst: d, .. } if *d == dst)
        }).count()
    }

    /// Count Release instructions for a specific temp in a function.
    fn count_release(func: &LinFunction, temp: Temp) -> usize {
        func.blocks.iter().flat_map(|b| &b.instructions).filter(|i| {
            matches!(i, Instruction::Release { val, .. } if *val == temp)
        }).count()
    }

    /// Returns true if any instruction in the function uses `new_temp` in a Borrow position
    /// where `old_temp` was used before elision.
    fn any_uses_of(func: &LinFunction, temp: Temp) -> bool {
        func.blocks.iter().flat_map(|b| &b.instructions).any(|i| {
            let (uses, _) = crate::liveness::instr_use_def(i);
            uses.contains(&temp)
        })
    }

    /// Build a helper function with a union type for the dst temp.
    fn make_clonebox_fn(id: FuncId, instrs: Vec<Instruction>) -> LinFunction {
        // Temp(0) = src (borrowed map-get result, e.g. Union type)
        // Temp(1) = dst (CloneBox output)
        // Temp(2) = scratch result of intrinsic
        let union_ty = Type::Union(vec![Type::Int32, Type::Null]);
        let mut temp_types = std::collections::HashMap::new();
        temp_types.insert(Temp(0), union_ty.clone());
        temp_types.insert(Temp(1), union_ty.clone());
        temp_types.insert(Temp(2), Type::Int32);
        let block = BasicBlock {
            id: BlockId(0),
            label: None,
            instructions: instrs,
            terminator: Terminator::Return(None),
            span: None,
            instr_spans: Vec::new(),
        };
        LinFunction {
            id,
            name: None,
            params: vec![],
            is_closure: false,
            ret_ty: Type::Null,
            param_conventions: Vec::new(),
            ret_convention: Convention::Own,
            blocks: vec![block],
            temp_types,
            temp_count: 3,
            intrinsic_slots: std::collections::HashMap::new(),
            repr: Vec::new(),
            coverage_origin: None,
            substr_fuse: std::collections::HashMap::new(),
            getset_fuse: std::collections::HashSet::new(),
        }
    }

    fn make_clonebox_two_block_fn(
        id: FuncId,
        instrs0: Vec<Instruction>,
        instrs1: Vec<Instruction>,
    ) -> LinFunction {
        let union_ty = Type::Union(vec![Type::Int32, Type::Null]);
        let mut temp_types = std::collections::HashMap::new();
        temp_types.insert(Temp(0), union_ty.clone());
        temp_types.insert(Temp(1), union_ty.clone());
        temp_types.insert(Temp(2), Type::Int32);
        let block0 = BasicBlock {
            id: BlockId(0),
            label: None,
            instructions: instrs0,
            terminator: Terminator::Jump(BlockId(1)),
            span: None,
            instr_spans: Vec::new(),
        };
        let block1 = BasicBlock {
            id: BlockId(1),
            label: None,
            instructions: instrs1,
            terminator: Terminator::Return(None),
            span: None,
            instr_spans: Vec::new(),
        };
        LinFunction {
            id,
            name: None,
            params: vec![],
            is_closure: false,
            ret_ty: Type::Null,
            param_conventions: Vec::new(),
            ret_convention: Convention::Own,
            blocks: vec![block0, block1],
            temp_types,
            temp_count: 3,
            intrinsic_slots: std::collections::HashMap::new(),
            repr: Vec::new(),
            coverage_origin: None,
            substr_fuse: std::collections::HashMap::new(),
            getset_fuse: std::collections::HashSet::new(),
        }
    }

    /// Positive: CloneBox(dst, src) + UnboxInt32(dst) [Borrow] + Release(dst) in one block.
    /// All uses are Borrow-convention → elide CloneBox and Release, substitute dst→src.
    #[test]
    fn clonebox_same_block_borrow_use_elided() {
        let union_ty = Type::Union(vec![Type::Int32, Type::Null]);
        let instrs = vec![
            Instruction::CloneBox { dst: Temp(1), src: Temp(0), ty: union_ty.clone() },
            Instruction::CallIntrinsic {
                dst: Temp(2),
                intrinsic: Intrinsic::UnboxInt32,
                args: vec![Temp(1)],
                ret_ty: Type::Int32,
            },
            Instruction::Release { val: Temp(1), ty: union_ty.clone() },
        ];
        let mut module = make_module(make_clonebox_fn(FuncId(0), instrs));
        elide_rc(&mut module);
        let func = &module.functions[0];
        assert_eq!(count_clonebox(func, Temp(1)), 0, "CloneBox should be elided");
        assert_eq!(count_release(func, Temp(1)), 0, "Release(dst) should be elided");
        // The UnboxInt32 call should now reference Temp(0) (src), not Temp(1) (dst).
        let uses_dst = any_uses_of(func, Temp(1));
        assert!(!uses_dst, "No instruction should reference dst after elision");
    }

    /// Positive: CloneBox in block 0, Borrow use (UnboxInt32) in block 0,
    /// Release in block 1 (the direct successor = post-dominator).
    /// Cross-block elision should fire.
    #[test]
    fn clonebox_cross_block_borrow_use_elided() {
        let union_ty = Type::Union(vec![Type::Int32, Type::Null]);
        let instrs0 = vec![
            Instruction::CloneBox { dst: Temp(1), src: Temp(0), ty: union_ty.clone() },
            Instruction::CallIntrinsic {
                dst: Temp(2),
                intrinsic: Intrinsic::UnboxInt32,
                args: vec![Temp(1)],
                ret_ty: Type::Int32,
            },
        ];
        let instrs1 = vec![
            Instruction::Release { val: Temp(1), ty: union_ty.clone() },
        ];
        let mut module = make_module(make_clonebox_two_block_fn(FuncId(0), instrs0, instrs1));
        elide_rc(&mut module);
        let func = &module.functions[0];
        assert_eq!(count_clonebox(func, Temp(1)), 0, "CloneBox should be elided (cross-block)");
        assert_eq!(count_release(func, Temp(1)), 0, "Release(dst) should be elided (cross-block)");
        let uses_dst = any_uses_of(func, Temp(1));
        assert!(!uses_dst, "No instruction should reference dst after elision");
    }

    /// Negative: CloneBox + CallTarget::Named call passing dst (unknown convention = Own).
    /// Must NOT elide: the call transfers ownership of dst.
    #[test]
    fn clonebox_kept_when_own_use() {
        let union_ty = Type::Union(vec![Type::Int32, Type::Null]);
        let instrs = vec![
            Instruction::CloneBox { dst: Temp(1), src: Temp(0), ty: union_ty.clone() },
            // Named call = unknown convention = Own for all args → interference
            Instruction::Call {
                dst: Temp(2),
                callee: CallTarget::Named("some_fn".into()),
                args: vec![Temp(1)],
                ret_ty: Type::Null,
            },
            Instruction::Release { val: Temp(1), ty: union_ty.clone() },
        ];
        let mut module = make_module(make_clonebox_fn(FuncId(0), instrs));
        elide_rc(&mut module);
        let func = &module.functions[0];
        assert_eq!(count_clonebox(func, Temp(1)), 1, "CloneBox must be kept (Own use)");
        assert_eq!(count_release(func, Temp(1)), 1, "Release must be kept (Own use)");
    }

    /// Negative: CloneBox where the type is NOT a union (e.g. plain Str).
    /// The pass only targets union-typed boxes; non-union CloneBox degrades to Retain
    /// and is handled by the Retain/Release pass, not this one. Confirm no elision here.
    #[test]
    fn clonebox_non_union_type_not_elided_by_clonebox_pass() {
        // Use Type::Str — NOT a union type per is_union_clonebox_ty.
        let instrs = vec![
            Instruction::CloneBox { dst: Temp(1), src: Temp(0), ty: Type::Str },
            Instruction::Copy { dst: Temp(2), src: Temp(1) },
            Instruction::Release { val: Temp(1), ty: Type::Str },
        ];
        // For this test we only check that elide_clonebox_reads_fn itself doesn't fire.
        // The Retain/Release pass may or may not convert a non-union CloneBox — that is not
        // what we're testing here.
        let conv_map = HashMap::new();
        let mut func = make_clonebox_fn(FuncId(0), instrs);
        elide_clonebox_reads_fn(&mut func, &conv_map);
        // The CloneBox (Str-typed) must survive the CloneBox-read pass.
        assert_eq!(count_clonebox(&func, Temp(1)), 1, "Non-union CloneBox must not be touched by clonebox-read pass");
    }
}
