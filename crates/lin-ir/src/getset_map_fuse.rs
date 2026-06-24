//! Get-set map fusion pass.
//!
//! Identifies `Index` + `IndexSet` pairs on the same (`object`, `key`) where the key
//! is already a byte-slice-fused substring temp (present in `func.substr_fuse`) and the
//! `Index` result `cur` is used ONLY within the window between the two instructions
//! (which may span multiple basic blocks when an if-expression computes the new value).
//!
//! Marks the `Index` dst in `func.getset_fuse` so codegen emits a single
//! `lin_map_upsert_slot_bytes` probe instead of separate `get_bytes` + `set_bytes` calls.
//!
//! # Soundness contract
//!
//! A temp `cur` (`Index { dst: cur, object: m, key: k }` in block B) is eligible when:
//!   1. `k ∈ func.substr_fuse` — key bytes are available for the upsert call.
//!   2. There exists `IndexSet { key: k, object: m' }` in some block J, where m and m' both
//!      load from the same GlobalValGet slot S (same mutable `var`).
//!   3. No GlobalValSet to slot S on any path from B to J (exclusive).
//!   4. No other IndexSet loading from slot S on any intermediate block between B and J.
//!   5. Every use of `cur` is in block B after ii (Index position) or in an intermediate
//!      block on a path from B to J (not in J or any block outside the window).
//!   6. The map value type is a flat scalar (Int/UInt/Float — no RC).
//!
//! Gate: `LIN_NO_GETSET_FUSE=1` disables the pass.

use crate::ir::*;
use lin_check::types::Type;
use std::collections::{HashMap, HashSet};

pub fn run(module: &mut LinModule) {
    if std::env::var("LIN_NO_GETSET_FUSE").is_ok() {
        return;
    }
    for func in &mut module.functions {
        run_fn(func);
    }
}

fn run_fn(func: &mut LinFunction) {
    if func.substr_fuse.is_empty() {
        return;
    }

    let n = func.blocks.len();

    // Map from temp → GlobalValGet slot (for temps that are direct GlobalValGet results).
    let mut gvg_slot: HashMap<Temp, usize> = HashMap::new();
    for block in &func.blocks {
        for instr in &block.instructions {
            if let Instruction::GlobalValGet { dst, slot, .. } = instr {
                gvg_slot.insert(*dst, *slot);
            }
        }
    }

    // Build successor lists for reachability.
    let succs: Vec<Vec<usize>> = (0..n).map(|b| block_succs(b, &func.blocks)).collect();

    // Build predecessor lists for backward reachability.
    let mut preds: Vec<Vec<usize>> = vec![vec![]; n];
    for (b, ss) in succs.iter().enumerate() {
        for &s in ss {
            preds[s].push(b);
        }
    }

    // All use sites of every temp in the function.
    let use_sites = collect_use_sites(func);

    // Walk each block looking for eligible Index instructions.
    for bi in 0..n {
        let block = &func.blocks[bi];
        'index_scan: for ii in 0..block.instructions.len() {
            let instr = &block.instructions[ii];

            // Match Index { dst: cur, object: m, key: k } with k ∈ substr_fuse.
            let (cur, m, k) = match instr {
                Instruction::Index { dst, object, key, obj_ty, .. } => {
                    if !func.substr_fuse.contains_key(key) { continue; }
                    if !is_flat_scalar_map(obj_ty) { continue; }
                    (*dst, *object, *key)
                }
                _ => continue,
            };

            // Find the GlobalValGet slot that `m` was loaded from.
            let slot_s = match gvg_slot.get(&m) {
                Some(&s) => s,
                None => continue, // m is not a direct GlobalValGet result; skip.
            };

            // Search ALL blocks for an IndexSet { key: k } whose object loads from slot_s.
            let mut set_loc: Option<(usize, usize)> = None; // (block_idx, instr_idx)
            'outer: for jj in 0..n {
                let jblock = &func.blocks[jj];
                for ji in 0..jblock.instructions.len() {
                    if let Instruction::IndexSet { key: k2, object: m2, obj_ty, .. } = &jblock.instructions[ji] {
                        if *k2 == k && is_flat_scalar_map(obj_ty) {
                            if let Some(&s2) = gvg_slot.get(m2) {
                                if s2 == slot_s {
                                    // Ensure IndexSet is actually reachable from the Index block.
                                    // (Quick check: jj != bi or ji > ii)
                                    if jj != bi || ji > ii {
                                        set_loc = Some((jj, ji));
                                        break 'outer;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let (jj, ji) = match set_loc {
                Some(loc) => loc,
                None => continue,
            };

            // Compute intermediate blocks: blocks on any path from bi to jj, exclusive.
            let inter = intermediate_blocks(bi, jj, &succs, &preds, n);

            // Condition 3 + 4: in block bi (after ii) and in each intermediate block:
            //   - No GlobalValSet to slot_s.
            //   - No IndexSet whose object loads from slot_s (other than our target).
            // Also check block bi instructions in (ii, end) and block jj instructions in [0, ji).
            {
                // Block bi: instructions after ii.
                let bi_instrs = &func.blocks[bi].instructions;
                for pos in (ii + 1)..bi_instrs.len() {
                    if check_map_write(slot_s, &bi_instrs[pos], &gvg_slot) {
                        continue 'index_scan;
                    }
                }
                // Intermediate blocks: all instructions.
                for &b in &inter {
                    for instr2 in &func.blocks[b].instructions {
                        if check_map_write(slot_s, instr2, &gvg_slot) {
                            continue 'index_scan;
                        }
                    }
                }
                // Block jj: instructions before ji (the target IndexSet itself is fine).
                let jj_instrs = &func.blocks[jj].instructions;
                for pos in 0..ji {
                    if check_map_write(slot_s, &jj_instrs[pos], &gvg_slot) {
                        continue 'index_scan;
                    }
                }
            }

            // Condition 5: every use of `cur` is in block bi (instr_idx > ii) or in an
            // intermediate block. NOT in jj, not in any other block.
            let allowed: HashSet<usize> = {
                let mut s = inter.iter().copied().collect::<HashSet<_>>();
                s.insert(bi);
                s
            };
            if let Some(sites) = use_sites.get(&cur) {
                for &(use_bi, use_ii) in sites {
                    let ok = if use_bi == bi {
                        use_ii > ii // must be after the Index in block bi
                    } else {
                        allowed.contains(&use_bi) // in an intermediate block
                    };
                    if !ok {
                        continue 'index_scan;
                    }
                }
            }

            func.getset_fuse.insert(cur);
        }
    }
}

/// Returns true if `instr` could invalidate the held upsert slot pointer across the
/// get→set window: a write to slot_s (GlobalValSet), an IndexSet on slot_s, OR ANY CALL.
///
/// The Call guard is load-bearing for SOUNDNESS: the fused codegen holds the raw slot
/// pointer returned by `lin_map_upsert_slot_bytes` (obtained at the get) and writes through
/// it at the set. A call in the window that inserts into the SAME map can trigger a
/// SwissTable grow → the slots array is reallocated and freed → the held pointer dangles
/// (use-after-free at the set). A callee can reach the map via the global slot even when
/// it is not visible as an IndexSet in this function's CFG, so we cannot prove a call is
/// map-free here. Conservatively disqualify the fusion whenever the window contains a call.
/// (knucleotide's window is pure null-check + i64 add, so it still fuses.)
fn check_map_write(slot_s: usize, instr: &Instruction, gvg_slot: &HashMap<Temp, usize>) -> bool {
    match instr {
        Instruction::GlobalValSet { slot, .. } => *slot == slot_s,
        Instruction::IndexSet { object: m2, .. } => {
            gvg_slot.get(m2).copied() == Some(slot_s)
        }
        // Any call may grow/reallocate the map (directly or transitively) → dangling slot.
        Instruction::Call { .. } | Instruction::CallIntrinsic { .. } => true,
        _ => false,
    }
}

/// Returns the direct successors of block `b`.
fn block_succs(b: usize, blocks: &[BasicBlock]) -> Vec<usize> {
    match &blocks[b].terminator {
        Terminator::Jump(id) => vec![id.0 as usize],
        Terminator::CondJump { then_block, else_block, .. } => vec![then_block.0 as usize, else_block.0 as usize],
        Terminator::Switch { cases, default, .. } => {
            let mut v: Vec<usize> = cases.iter().map(|(_, id)| id.0 as usize).collect();
            v.push(default.0 as usize);
            v.dedup();
            v
        }
        _ => vec![],
    }
}

/// Computes the set of blocks strictly between `start` and `end` on any path:
/// blocks reachable from `start` (not re-entering `start`) that can reach `end`.
fn intermediate_blocks(
    start: usize,
    end: usize,
    succs: &[Vec<usize>],
    preds: &[Vec<usize>],
    n: usize,
) -> Vec<usize> {
    // Forward reachability from start (not re-entering start).
    let mut fwd = vec![false; n];
    fwd[start] = true;
    let mut wl = vec![start];
    while let Some(b) = wl.pop() {
        for &s in &succs[b] {
            if !fwd[s] && s != start {
                fwd[s] = true;
                wl.push(s);
            }
        }
    }

    // Backward reachability from end.
    let mut bwd = vec![false; n];
    bwd[end] = true;
    let mut wl = vec![end];
    while let Some(b) = wl.pop() {
        for &p in &preds[b] {
            if !bwd[p] && p != end {
                bwd[p] = true;
                wl.push(p);
            }
        }
    }

    (0..n).filter(|&b| fwd[b] && bwd[b] && b != start && b != end).collect()
}

/// Collect all use sites (block_idx, instr_idx) for each temp.
/// Uses usize::MAX as instr_idx for terminator uses.
fn collect_use_sites(func: &LinFunction) -> HashMap<Temp, Vec<(usize, usize)>> {
    let mut map: HashMap<Temp, Vec<(usize, usize)>> = HashMap::new();
    for (bi, block) in func.blocks.iter().enumerate() {
        for (ii, instr) in block.instructions.iter().enumerate() {
            for t in crate::substr_map_fuse::temps_used(instr) {
                map.entry(t).or_default().push((bi, ii));
            }
        }
        match &block.terminator {
            Terminator::Return(Some(t)) => { map.entry(*t).or_default().push((bi, usize::MAX)); }
            Terminator::CondJump { cond, .. } => { map.entry(*cond).or_default().push((bi, usize::MAX)); }
            Terminator::TailCall { args, .. } => {
                for t in args { map.entry(*t).or_default().push((bi, usize::MAX)); }
            }
            _ => {}
        }
    }
    map
}

/// True when `ty` is a `{ String: T }` map where T is a flat scalar.
fn is_flat_scalar_map(ty: &Type) -> bool {
    match ty {
        Type::Map { key, value, .. } => key.is_string_ish() && is_flat_scalar_type(value),
        Type::Union(members) => members.iter().any(|m| matches!(m,
            Type::Map { key, value, .. } if key.is_string_ish() && is_flat_scalar_type(value)
        )),
        _ => false,
    }
}

fn is_flat_scalar_type(ty: &Type) -> bool {
    matches!(ty,
        Type::Int8 | Type::Int16 | Type::Int32 | Type::Int64 |
        Type::UInt8 | Type::UInt16 | Type::UInt32 | Type::UInt64 |
        Type::Float32 | Type::Float64
    )
}
