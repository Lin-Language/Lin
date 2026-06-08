//! Liveness analysis for LinIR temps within a single function.
//!
//! We compute, for each instruction position, the set of temps that are live
//! (will be used again in the future). This is the standard backwards dataflow:
//!
//!   live_in[b]  = use[b] ∪ (live_out[b] − def[b])
//!   live_out[b] = ∪ { live_in[s] | s ∈ successors(b) }
//!
//! We iterate to fixpoint.

use std::collections::{HashMap, HashSet};

use crate::ir::*;

/// Liveness information for a function.
pub struct Liveness {
    /// Maps BlockId → set of temps live at the start of the block.
    pub live_in: HashMap<BlockId, HashSet<Temp>>,
    /// Maps BlockId → set of temps live at the end of the block.
    pub live_out: HashMap<BlockId, HashSet<Temp>>,
    /// Instruction-level liveness: for each (BlockId, instr_index), the live set
    /// *before* the instruction executes (used by the RC elision pass).
    pub instr_live_before: HashMap<(BlockId, usize), HashSet<Temp>>,
}

impl Liveness {
    /// Compute liveness for a function.
    pub fn compute(func: &LinFunction) -> Self {
        let mut live_in: HashMap<BlockId, HashSet<Temp>> = HashMap::new();
        let mut live_out: HashMap<BlockId, HashSet<Temp>> = HashMap::new();

        for block in &func.blocks {
            live_in.insert(block.id, HashSet::new());
            live_out.insert(block.id, HashSet::new());
        }

        // Build a predecessor map for computing live_out.
        let successors = compute_successors(func);

        // Iterate to fixpoint.
        let mut changed = true;
        while changed {
            changed = false;
            // Process blocks in reverse order for faster convergence.
            for block in func.blocks.iter().rev() {
                // live_out[b] = union of live_in[s] for all successors s.
                let mut new_out: HashSet<Temp> = HashSet::new();
                for &succ in successors.get(&block.id).unwrap_or(&vec![]) {
                    if let Some(s_in) = live_in.get(&succ) {
                        for t in s_in {
                            new_out.insert(*t);
                        }
                    }
                }

                // live_in[b] = use[b] ∪ (live_out[b] − def[b])
                let (uses, defs) = block_use_def(block);
                let mut new_in: HashSet<Temp> = uses;
                for t in &new_out {
                    if !defs.contains(t) {
                        new_in.insert(*t);
                    }
                }

                if new_out != *live_out.get(&block.id).unwrap() {
                    changed = true;
                    live_out.insert(block.id, new_out);
                }
                if new_in != *live_in.get(&block.id).unwrap() {
                    changed = true;
                    live_in.insert(block.id, new_in);
                }
            }
        }

        // Compute per-instruction live sets (live before each instruction).
        let mut instr_live_before: HashMap<(BlockId, usize), HashSet<Temp>> = HashMap::new();
        for block in &func.blocks {
            // Start with live_out for this block and walk backwards.
            let mut live_set = live_out.get(&block.id).cloned().unwrap_or_default();

            // Process terminator uses.
            for t in terminator_uses(&block.terminator) {
                live_set.insert(t);
            }

            // Walk instructions in reverse to compute live-before.
            for (i, instr) in block.instructions.iter().enumerate().rev() {
                // Before instruction i, live set is: (live_after_i − def(i)) ∪ use(i)
                let (uses, defs) = instr_use_def(instr);
                for d in &defs {
                    live_set.remove(d);
                }
                for u in &uses {
                    live_set.insert(*u);
                }
                instr_live_before.insert((block.id, i), live_set.clone());
            }
        }

        Liveness { live_in, live_out, instr_live_before }
    }

    /// Returns true if `temp` is live immediately before instruction `idx` in `block`.
    pub fn is_live_before(&self, block: BlockId, idx: usize, temp: Temp) -> bool {
        self.instr_live_before
            .get(&(block, idx))
            .map(|s| s.contains(&temp))
            .unwrap_or(false)
    }

    /// Returns true if `temp` is live immediately AFTER instruction `idx` in
    /// `block` (i.e. some later instruction in this block, the terminator, or a
    /// successor block still uses it).
    ///
    /// `block_len` is the number of instructions in the block. When `idx` is the
    /// final instruction, "live after" means "live at the block exit", which is
    /// `live_out[block]`. Otherwise it is `live_before[idx + 1]`.
    ///
    /// NOTE: for the final instruction this checks `live_out` only; a temp used
    /// directly by the terminator (e.g. `Return(temp)`) is NOT folded into
    /// `live_out`, so callers that care about that case must check the terminator
    /// separately (see `rc_elide::release_is_last_use`).
    pub fn is_live_after(&self, block: BlockId, idx: usize, block_len: usize, temp: Temp) -> bool {
        if idx + 1 < block_len {
            self.instr_live_before
                .get(&(block, idx + 1))
                .map(|s| s.contains(&temp))
                .unwrap_or(false)
        } else {
            self.live_out
                .get(&block)
                .map(|s| s.contains(&temp))
                .unwrap_or(false)
        }
    }
}

// -------------------------------------------------------------------------
// Helpers
// -------------------------------------------------------------------------

fn compute_successors(func: &LinFunction) -> HashMap<BlockId, Vec<BlockId>> {
    let mut map: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
    for block in &func.blocks {
        let succs = terminator_successors(&block.terminator);
        map.insert(block.id, succs);
    }
    map
}

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

fn terminator_uses(term: &Terminator) -> Vec<Temp> {
    match term {
        Terminator::Return(Some(t)) => vec![*t],
        Terminator::CondJump { cond, .. } => vec![*cond],
        Terminator::Switch { val, .. } => vec![*val],
        Terminator::TailCall { args } => args.clone(),
        _ => vec![],
    }
}

/// Returns (uses, defs) for a block (not per-instruction, for fixpoint).
fn block_use_def(block: &BasicBlock) -> (HashSet<Temp>, HashSet<Temp>) {
    let mut uses: HashSet<Temp> = HashSet::new();
    let mut defs: HashSet<Temp> = HashSet::new();

    for instr in &block.instructions {
        let (u, d) = instr_use_def(instr);
        // use set: temps used before being defined in this block.
        for t in u {
            if !defs.contains(&t) {
                uses.insert(t);
            }
        }
        for t in d {
            defs.insert(t);
        }
    }

    // Terminator uses.
    for t in terminator_uses(&block.terminator) {
        if !defs.contains(&t) {
            uses.insert(t);
        }
    }

    (uses, defs)
}

/// Returns (uses, defs) for a single instruction.
pub fn instr_use_def(instr: &Instruction) -> (Vec<Temp>, Vec<Temp>) {
    match instr {
        Instruction::Const { dst, .. } => (vec![], vec![*dst]),
        Instruction::Copy { dst, src } => (vec![*src], vec![*dst]),
        // Phi operands are conceptually used along the predecessor edges, not at the phi
        // itself; treating them as used here is a safe over-approximation for RC elision.
        Instruction::Phi { dst, incomings, .. } => {
            (incomings.iter().map(|(t, _)| *t).collect(), vec![*dst])
        }
        Instruction::Unary { dst, operand, .. } => (vec![*operand], vec![*dst]),
        Instruction::Binary { dst, lhs, rhs, .. } => (vec![*lhs, *rhs], vec![*dst]),
        Instruction::Coerce { dst, src, .. } => (vec![*src], vec![*dst]),
        Instruction::Call { dst, callee, args, .. } => {
            let mut uses = args.clone();
            if let CallTarget::Indirect(t) = callee {
                uses.push(*t);
            }
            (uses, vec![*dst])
        }
        Instruction::CallIntrinsic { dst, args, .. } => (args.clone(), vec![*dst]),
        Instruction::MakeClosure { dst, captures, .. } => (captures.clone(), vec![*dst]),
        Instruction::MakeNamedClosure { dst, .. } => (vec![], vec![*dst]),
        Instruction::MakeObject { dst, fields, spreads, .. } => {
            let mut uses: Vec<Temp> = fields.iter().map(|(_, t)| *t).collect();
            uses.extend(spreads.iter().copied());
            (uses, vec![*dst])
        }
        Instruction::MakeArray { dst, elements, .. } => (elements.clone(), vec![*dst]),
        Instruction::Index { dst, object, key, .. } => (vec![*object, *key], vec![*dst]),
        Instruction::IndexSet { object, key, value, .. } => (vec![*object, *key, *value], vec![]),
        Instruction::FieldGet { dst, object, .. } => (vec![*object], vec![*dst]),
        Instruction::FieldSet { object, value, .. } => (vec![*object, *value], vec![]),
        Instruction::SealedArrayFieldGet { dst, array, index, .. } => (vec![*array, *index], vec![*dst]),
        Instruction::EnvCapture { dst, env, .. } => (vec![*env], vec![*dst]),
        Instruction::ArrayLenCheck { dst, val, .. } => (vec![*val], vec![*dst]),
        Instruction::ObjectRest { dst, src, .. } => (vec![*src], vec![*dst]),
        Instruction::GlobalValSet { value, .. } => (vec![*value], vec![]),
        Instruction::GlobalValGet { dst, .. } => (vec![], vec![*dst]),
        Instruction::MakeCell { dst, init, .. } => (vec![*init], vec![*dst]),
        Instruction::CellGet { dst, cell, .. } => (vec![*cell], vec![*dst]),
        Instruction::CellSet { cell, value, .. } => (vec![*cell, *value], vec![]),
        Instruction::FreeCell { cell, .. } => (vec![*cell], vec![]),
        Instruction::Retain { val, .. } => (vec![*val], vec![]),
        Instruction::Release { val, .. } => (vec![*val], vec![]),
        Instruction::CloneBox { dst, src, .. } => (vec![*src], vec![*dst]),
        Instruction::FreeBoxShell { val } => (vec![*val], vec![]),
        Instruction::FreeBoxShellIfDistinct { val, other } => (vec![*val, *other], vec![]),
        Instruction::IsType { dst, val, .. } => (vec![*val], vec![*dst]),
        Instruction::SumTagEq { dst, val, .. } => (vec![*val], vec![*dst]),
        Instruction::HasPattern { dst, val, .. } => (vec![*val], vec![*dst]),
        Instruction::MatchesSchema { dst, val, .. } => (vec![*val], vec![*dst]),
        Instruction::Box { dst, val, .. } => (vec![*val], vec![*dst]),
        Instruction::Unbox { dst, val, .. } => (vec![*val], vec![*dst]),
        Instruction::BoxKeepPacked { dst, src, .. } => (vec![*src], vec![*dst]),
        Instruction::UnboxKeepPacked { dst, src, .. } => (vec![*src], vec![*dst]),
        Instruction::Bind { dst, src, .. } => (vec![*src], vec![*dst]),
        Instruction::Panic { msg } => (vec![*msg], vec![]),
    }
}
