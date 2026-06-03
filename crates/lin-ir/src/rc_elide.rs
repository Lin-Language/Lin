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

use std::collections::{HashMap, HashSet, VecDeque};

use lin_check::types::Type;

use crate::ir::*;
use crate::liveness::Liveness;

/// Maximum number of blocks to visit during BFS when searching cross-block
/// for a paired Release. Keeps compile-time cost bounded.
const BFS_BLOCK_LIMIT: usize = 8;

/// Run the RC elision pass on all functions in a module, mutating in place.
pub fn elide_rc(module: &mut LinModule) {
    for func in &mut module.functions {
        elide_rc_fn(func);
    }
}

fn elide_rc_fn(func: &mut LinFunction) {
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
                if path_has_no_interference(*retain_val, retain_idx, release_idx, &instrs)
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
                path_has_no_interference(*retain_val, retain_idx, instrs.len(), &instrs);
            if !tail_clean {
                continue;
            }

            if let Some((release_block_idx, release_instr_idx)) = find_paired_release_cross_block(
                *retain_val,
                block_idx,
                func,
                &block_index,
                &to_remove,
            ) {
                let release_block_id = func.blocks[release_block_idx].id;
                let retain_block_id = func.blocks[block_idx].id;
                // The release block's prefix (before the Release) must also be clean.
                let prefix_clean = path_has_no_interference(
                    *retain_val,
                    usize::MAX, // sentinel: start from instruction 0
                    release_instr_idx,
                    &func.blocks[release_block_idx].instructions,
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
        }
    }
}

/// Types that participate in RC (reference counted heap values).
fn is_rc_type(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Str | Type::StrLit(_) | Type::Array(_) | Type::FixedArray(_) | Type::Object(_) | Type::Function { .. }
    )
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

/// BFS across CFG successors to find the paired Release for `temp` that was
/// Retained in `origin_block_idx`. Visits at most `BFS_BLOCK_LIMIT` blocks.
///
/// Returns `Some((block_idx, instr_idx))` of the Release if found on a path
/// with:
///   - No intermediate blocks (between origin and the release block) that
///     contain interference (call/alloc/Release-of-temp).
///   - The release block's prefix up to the Release is also clean.
///
/// All intermediate blocks must pass `block_is_clean_for` (no interference and
/// temp is not defined or released in them) for the path to be eligible.
fn find_paired_release_cross_block(
    temp: Temp,
    origin_block_idx: usize,
    func: &LinFunction,
    block_index: &HashMap<BlockId, usize>,
    claimed: &HashSet<(usize, usize)>,
) -> Option<(usize, usize)> {
    let origin_block = &func.blocks[origin_block_idx];

    // BFS queue: (block_id, must_be_clean_entirely)
    // For blocks between origin and release, all instructions must be clean.
    // For the release block, we only require the prefix up to the Release.
    let mut visited: HashSet<BlockId> = HashSet::new();
    visited.insert(origin_block.id);

    let mut queue: VecDeque<BlockId> = VecDeque::new();
    for succ in terminator_successors(&origin_block.terminator) {
        if !visited.contains(&succ) {
            queue.push_back(succ);
            visited.insert(succ);
        }
    }

    let mut blocks_visited = 0usize;

    while let Some(bid) = queue.pop_front() {
        blocks_visited += 1;
        if blocks_visited > BFS_BLOCK_LIMIT {
            break;
        }

        let Some(&idx) = block_index.get(&bid) else { continue };
        let block = &func.blocks[idx];

        // Check whether this block contains an UNCLAIMED Release (one not already paired with
        // an earlier Retain). Skipping claimed Releases keeps elision one-to-one across blocks,
        // matching the same-block rule.
        if let Some(release_pos) =
            find_release_at_block_start(temp, block, |i| claimed.contains(&(idx, i)))
        {
            // Found the Release. Check that the prefix of this block (before the
            // Release) is clean (using the sentinel usize::MAX to mean "from 0").
            return Some((idx, release_pos));
        }

        // This block must be entirely clean for the path to remain eligible.
        if !block_is_clean_for(temp, block) {
            // Path through this block is tainted — do not continue BFS through it.
            continue;
        }

        // Temp must survive the whole block (not redefined, not released).
        if !block_temp_survives(temp, block) {
            continue;
        }

        // Enqueue successors.
        for succ in terminator_successors(&block.terminator) {
            if !visited.contains(&succ) {
                visited.insert(succ);
                queue.push_back(succ);
            }
        }
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
fn block_is_clean_for(temp: Temp, block: &BasicBlock) -> bool {
    for instr in &block.instructions {
        if instr_is_interference(temp, instr) {
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
fn instr_is_interference(temp: Temp, instr: &Instruction) -> bool {
    match instr {
        Instruction::Call { .. }
        | Instruction::CallIntrinsic { .. }
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

        PostDom { post_dom }
    }

    /// True if `p` post-dominates `b` (every path from `b` to an exit goes
    /// through `p`). When `b` has no recorded post-dom set (unreachable), this
    /// returns false — we never elide on the basis of unreachable info.
    fn post_dominates(&self, p: BlockId, b: BlockId) -> bool {
        self.post_dom.get(&b).map(|set| set.contains(&p)).unwrap_or(false)
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
) -> bool {
    let start = if start_exclusive == usize::MAX { 0 } else { start_exclusive + 1 };
    let end = end_exclusive.min(instrs.len());
    for i in start..end {
        if instr_is_interference(temp, &instrs[i]) {
            return false;
        }
    }
    true
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
            blocks: vec![block],
            temp_types,
            temp_count: 3,
            intrinsic_slots: std::collections::HashMap::new(),
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
        };
        let block1 = BasicBlock {
            id: BlockId(1),
            label: None,
            instructions: instrs1,
            terminator: Terminator::Return(None),
            span: None,
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
            blocks: vec![block0, block1],
            temp_types,
            temp_count: 3,
            intrinsic_slots: std::collections::HashMap::new(),
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
        };
        let block1 = BasicBlock {
            id: BlockId(1),
            label: None,
            instructions: instrs1,
            terminator: Terminator::Jump(BlockId(2)),
            span: None,
        };
        let block2 = BasicBlock {
            id: BlockId(2),
            label: None,
            instructions: instrs2,
            terminator: Terminator::Return(None),
            span: None,
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
            blocks: vec![block0, block1, block2],
            temp_types,
            temp_count: 3,
            intrinsic_slots: std::collections::HashMap::new(),
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
        };
        let block1 = BasicBlock {
            id: BlockId(1),
            label: None,
            instructions: vec![Instruction::Release { val: Temp(0), ty: Type::Str }],
            terminator: Terminator::Return(None),
            span: None,
        };
        let block2 = BasicBlock {
            id: BlockId(2),
            label: None,
            instructions: vec![],
            terminator: Terminator::Return(None),
            span: None,
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
            blocks: vec![block0, block1, block2],
            temp_types,
            temp_count: 2,
            intrinsic_slots: std::collections::HashMap::new(),
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
}
