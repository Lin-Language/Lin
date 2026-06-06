//! Shared carry-class machinery for the lin-ir dataflow passes (escape analysis, repr inference).
//!
//! A **carry class** is a set of temps connected by representation-PRESERVING aliasing edges: a
//! value flows from one temp into another WITHOUT changing its physical representation. Both the
//! escape pass (`escape.rs`) and the representation-inference pass (`repr.rs`) reason over the same
//! carry classes — a fact about a class (does it escape? what is its repr?) is the join over its
//! members. This module factors out the two pieces both passes share:
//!
//!   1. [`UnionFind`] over temp indices `0..temp_count` with path compression.
//!   2. The carry-edge classifier ([`classify_carry_edges`]) that unifies the temps connected by
//!      Copy / Bind / Phi-incoming edges, a no-op [`coerce_is_carry`] Coerce, and (at a terminator)
//!      a self-`TailCall` arg ↔ param edge.
//!
//! This was extracted verbatim from `escape.rs` (the carry-edge logic at the former lines
//! ~120-152 and ~301-330, plus the TailCall arg↔param unification at ~206-218). The extraction is a
//! PURE refactor: `escape.rs` now calls into here and its behaviour is byte-for-byte identical.

use lin_check::types::Type;

use crate::ir::*;

// ---------------------------------------------------------------------------
// Union-find over temps (carry classes)
// ---------------------------------------------------------------------------

/// Disjoint-set union over temp indices `0..n`, with path compression. Roots are temp indices.
pub struct UnionFind {
    pub parent: Vec<u32>,
}

impl UnionFind {
    pub fn new(n: u32) -> Self {
        UnionFind { parent: (0..n).collect() }
    }

    /// Find the representative of `x`'s set (path-compressing).
    pub fn find_raw(&mut self, x: u32) -> u32 {
        let mut root = x;
        while self.parent[root as usize] != root {
            root = self.parent[root as usize];
        }
        // Path compression.
        let mut cur = x;
        while self.parent[cur as usize] != root {
            let next = self.parent[cur as usize];
            self.parent[cur as usize] = root;
            cur = next;
        }
        root
    }

    pub fn find(&mut self, x: Temp) -> u32 {
        self.find_raw(x.0)
    }

    pub fn union(&mut self, a: Temp, b: Temp) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra != rb {
            self.parent[ra as usize] = rb;
        }
    }

    /// Read-only root lookup (NO path compression) so it can run behind a shared `&UnionFind`
    /// borrow (e.g. inside a `Vec::retain` closure). The union-find is already path-compressed by the
    /// preceding `find`/`find_raw` calls, so the walk is short.
    pub fn find_const(&self, x: Temp) -> u32 {
        let mut root = x.0;
        while self.parent[root as usize] != root {
            root = self.parent[root as usize];
        }
        root
    }
}

// ---------------------------------------------------------------------------
// Carry-edge classifier
// ---------------------------------------------------------------------------

/// True when a `Coerce { from_ty, to_ty }` does NOT change the physical representation, i.e. both
/// sides are the SAME stack-eligible sealed scalar record (so the coerce is a value-preserving
/// alias — a carry edge). Any other coerce (projection that allocates, boxing to Json, a different
/// field set) is representation-changing → NOT a carry edge (the chain breaks; the source's class is
/// not unified with the dst).
///
/// Extracted verbatim from `escape::coerce_is_carry`. Mirrors the same all-scalar sealed-record gate
/// (`escape::is_stack_eligible_type`) so the carry definition is identical to the historical one.
pub fn coerce_is_carry(from_ty: &Type, to_ty: &Type) -> bool {
    is_stack_eligible_type(from_ty) && is_stack_eligible_type(to_ty) && from_ty == to_ty
}

/// True iff `ty` is a sealed record whose fields are ALL unboxed scalars (Int*/UInt*/Float*/Bool).
/// This is the all-scalar restriction that defines a no-op (carry) Coerce. Kept here (rather than in
/// escape.rs) because [`coerce_is_carry`] depends on it; escape.rs's `is_stack_eligible_type` now
/// delegates to this so there is a single definition.
pub fn is_stack_eligible_type(ty: &Type) -> bool {
    match ty {
        Type::Object { fields, sealed: true } if !fields.is_empty() => {
            fields.values().all(is_scalar_field)
        }
        _ => false,
    }
}

/// A field that may appear inline in an all-scalar sealed record: a fixed-width scalar or Bool.
fn is_scalar_field(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Bool
            | Type::Int8 | Type::Int16 | Type::Int32 | Type::Int64
            | Type::UInt8 | Type::UInt16 | Type::UInt32 | Type::UInt64
            | Type::Float32 | Type::Float64
    )
}

/// Add the representation-PRESERVING carry edges contributed by one INSTRUCTION to `uf`:
///   - `Copy { dst, src }`            — dst carries src
///   - `Bind { dst, src }`            — dst carries src
///   - `Phi { dst, incomings }`       — dst carries each incoming
///   - `Coerce { dst, src }` where [`coerce_is_carry`] — dst carries src; a repr-CHANGING coerce
///     does NOT unify (the chain breaks).
///
/// Every other instruction contributes NO carry edge (its result is a fresh value); the caller is
/// responsible for whatever per-pass fact (escape mark, repr seed) those instructions imply. This
/// mirrors exactly the carry arms of the former `escape::classify_instr` (Copy/Bind/Phi/Coerce).
pub fn classify_carry_edges(instr: &Instruction, uf: &mut UnionFind) {
    match instr {
        Instruction::Copy { dst, src } => uf.union(*dst, *src),
        Instruction::Bind { dst, src, .. } => uf.union(*dst, *src),
        Instruction::Phi { dst, incomings, .. } => {
            for (t, _) in incomings {
                uf.union(*dst, *t);
            }
        }
        Instruction::Coerce { dst, src, from_ty, to_ty } => {
            if coerce_is_carry(from_ty, to_ty) {
                uf.union(*dst, *src);
            }
        }
        _ => {}
    }
}

/// Add the carry edge contributed by a self-`TailCall` terminator: arg position `i` flows into the
/// function's parameter `i`'s slot for the next loop iteration, so they are unified. Returns the
/// arg temps that had NO corresponding parameter (arity mismatch — shouldn't happen for a
/// self-tail-call) so the caller can apply its fail-safe (escape mark). Mirrors the TailCall arm of
/// the former `escape::analyze_fn` terminator handling.
pub fn classify_tailcall_carry(args: &[Temp], params: &[(Temp, Type)], uf: &mut UnionFind) -> Vec<Temp> {
    let mut unmatched = Vec::new();
    for (i, a) in args.iter().enumerate() {
        match params.get(i) {
            Some((p, _)) => uf.union(*a, *p),
            None => unmatched.push(*a),
        }
    }
    unmatched
}
