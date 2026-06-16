//! Escape analysis for stack-allocating non-escaping sealed records (sealed-records Stage 4).
//!
//! # What this pass does
//!
//! It marks each `MakeObject` instruction that constructs an ALL-SCALAR sealed record
//! (`Type::Object { sealed: true }` whose fields are all unboxed scalars/Bool) with `stack = true`
//! IFF the constructed value PROVABLY does not escape its creating function frame. Codegen then
//! allocates such a record in a REUSED function-entry-block `alloca` (no `lin_sealed_alloc`, no
//! heap, no refcount churn) with an immortal-sentinel refcount so the owning model's Retain/Release
//! become safe no-ops (see `lin_runtime::sealed::lin_sealed_release`). Everything else stays heap —
//! today's behaviour, byte-for-byte.
//!
//! # Soundness (the absolute rule)
//!
//! Stack-allocating a record that actually ESCAPES (outlives its frame) is a use-after-return — a
//! silent memory-corruption class `cargo test` does NOT catch (only ASan does). Therefore the rule
//! is: stack-allocate ONLY when the record is PROVABLY non-escaping; on ANY doubt, heap-allocate.
//! The pass fails SAFE to heap.
//!
//! # The analysis
//!
//! We work over the flat IR of one function. Temps are connected into **carry classes** by
//! representation-PRESERVING aliasing edges (a value flows from one temp to another WITHOUT changing
//! its physical representation):
//!   - `Copy { dst, src }`            — dst carries src
//!   - `Phi { dst, incomings }`       — dst carries each incoming
//!   - `Bind { dst, src }`            — dst carries src
//!   - `Coerce { dst, src }` where `src`'s and `dst`'s types are the SAME sealed scalar record
//!     (a no-op coerce) — dst carries src. A REPRESENTATION-CHANGING coerce (e.g. sealed → boxed
//!     Json, or a projection that allocates a fresh struct) is NOT a carry edge: it produces a
//!     distinct value, so the chain BREAKS there.
//!   - `TailCall { args }` arg position `i` ↔ the function's parameter `i`. A self-tail-call
//!     re-enters the SAME function, so an arg flows into the corresponding param's slot for the next
//!     loop iteration. This unifies a construction-that-feeds-the-tail-call with the param it
//!     becomes — the records.lin shape. (All TailCalls in the IR are self-tail-calls; see the
//!     lowerer, which only emits `TailCall` for a direct self-recursive tail call.)
//!
//! A carry class ESCAPES if ANY member temp is used by an ESCAPING consumer. An escaping consumer is
//! ANYTHING that could make the value (or an alias of it) outlive the frame OR observe/own its heap
//! identity:
//!   - `Return(t)`                         — returned out of the frame
//!   - stored into a container: `MakeObject` field/spread, `MakeArray` elem, `IndexSet` value,
//!     `GlobalValSet` value, `MakeCell`/`CellSet` value
//!   - captured by a closure: `MakeClosure` capture
//!   - `Retain { val }`                    — someone wants an extra owner (conservative: escape)
//!   - any other use we don't explicitly know is read-only — `Call`/`CallIntrinsic` arg, `Box`,
//!     `CloneBox`, `Unbox`, `Coerce` source (repr-changing), `ObjectRest`, `IsType`,
//!     `MatchesSchema`, `HasPattern`, `ArrayLenCheck`, etc. — treated as ESCAPE (fail-safe).
//!
//! The ONLY uses that do NOT escape (a value may be a non-escaping local iff it is only ever):
//!   - read as the `object` of a `FieldGet`/`Index` (field read), or the `array` of a
//!     `SealedArrayFieldGet`,
//!   - the source of a representation-PRESERVING carry edge (handled above as carry, not escape),
//!   - dropped (`Release { val }` — the value's death; a no-op on a stack/immortal record),
//!   - passed into a self-`TailCall` arg whose corresponding param's carry class ALSO does not
//!     escape (handled by the carry-class unification: the param is in the same class, so its uses
//!     are folded in).
//!
//! Note that `Index { key }` and `FieldGet`/`Index` used as a KEY position do not arise for a
//! sealed record (its key is a literal). We classify a temp used anywhere OTHER than the listed
//! read-only positions as escaping.
//!
//! # Termination / fixpoint
//!
//! Carry classes are a union-find. The escape set is the union of classes touched by an escaping
//! use; computed in one pass after union-find is built (no iteration needed — escaping is a property
//! of the final class).

use std::collections::HashMap;

use lin_check::types::Type;

use crate::carry::{self, UnionFind};
use crate::ir::*;

/// Run the escape-analysis pass on all functions in a module, mutating `MakeObject.stack` and
/// `MakeArray.inline` in place.
/// Phase-1: mark non-escaping sealed-record-array MakeArray sites as `inline = true` (0xFE).
/// Must run BEFORE `repr::run` so that the repr seeding pass sees the updated `inline` flags.
pub fn analyze_array_inline(module: &mut LinModule) {
    for func in &mut module.functions {
        analyze_array_inline_fn(func);
    }
}

pub fn analyze(module: &mut LinModule) {
    for func in &mut module.functions {
        analyze_fn(func);
    }
}

/// True iff `ty` is a sealed record whose fields are ALL unboxed scalars (Int*/UInt*/Float*/Bool).
/// This is the Stage-4 scope: heap-field sealed records are NEVER stack-allocated here (their stack
/// drop would have to release heap fields — deferred). Mirrors codegen's `sealed_fields` gate plus
/// an all-scalar restriction. Anonymous/unsealed objects, unions, Json, etc. → false.
///
/// Delegates to [`carry::is_stack_eligible_type`] so the all-scalar sealed-record gate has a single
/// definition shared with the carry-edge classifier (Stage-1 extraction; behaviour unchanged).
fn is_stack_eligible_type(ty: &Type) -> bool {
    carry::is_stack_eligible_type(ty)
}

fn analyze_fn(func: &mut LinFunction) {
    let n = func.temp_count;
    if n == 0 {
        return;
    }

    // Collect candidate MakeObject sites (block index, instr index, dst). A candidate is a sealed
    // all-scalar record literal with NO spreads (a spread could add fields / change shape — keep
    // those heap; codegen also only stack-allocs the exact-field no-spread literal).
    let mut candidates: Vec<(usize, usize, Temp)> = Vec::new();
    for (bi, block) in func.blocks.iter().enumerate() {
        for (ii, instr) in block.instructions.iter().enumerate() {
            if let Instruction::MakeObject { dst, spreads, ty, .. } = instr {
                if spreads.is_empty() && is_stack_eligible_type(ty) {
                    candidates.push((bi, ii, *dst));
                }
            }
        }
    }
    if candidates.is_empty() {
        return;
    }

    // Build carry classes (union-find) and the per-temp escape flag in a single walk.
    let mut uf = UnionFind::new(n);
    // `escaping[t]` = temp t (specifically) is used by an escaping consumer. Folded into classes
    // after the union-find is complete.
    let mut escaping = vec![false; n as usize];

    let mark = |escaping: &mut Vec<bool>, t: Temp| {
        if (t.0 as usize) < escaping.len() {
            escaping[t.0 as usize] = true;
        }
    };

    for block in &func.blocks {
        for instr in &block.instructions {
            classify_instr(instr, &mut uf, &mut escaping, &mark);
        }
        // Terminator handling.
        match &block.terminator {
            Terminator::Return(Some(t)) => mark(&mut escaping, *t),
            Terminator::CondJump { cond, .. } => {
                // A cond is a scalar bool read; not an escape for a sealed record (a sealed record
                // is never a branch condition). Harmless to ignore.
                let _ = cond;
            }
            Terminator::Switch { val, .. } => {
                // The switch scrutinee is a tag/boxed value; a raw sealed struct is never switched
                // on directly. Treat as escape to be safe (it's read by the runtime switch).
                mark(&mut escaping, *val);
            }
            Terminator::TailCall { args } => {
                // Self-tail-call: arg i flows into param i's slot for the next iteration. Unify each
                // arg with the corresponding param temp, so the param's (read-only) uses and the
                // arg's class are one. If the param's class escapes anywhere, the arg's class does
                // too — exactly the soundness we need. If there is NO corresponding param (arity
                // mismatch — shouldn't happen for a self-tail-call) we conservatively mark escape.
                let unmatched = carry::classify_tailcall_carry(args, &func.params, &mut uf);
                for a in unmatched {
                    mark(&mut escaping, a);
                }
            }
            Terminator::Return(None) | Terminator::Jump(_) | Terminator::Unreachable => {}
        }
    }

    // Fold per-temp escape into per-class escape.
    let mut class_escapes: HashMap<u32, bool> = HashMap::new();
    for t in 0..n {
        if escaping[t as usize] {
            let r = uf.find_raw(t);
            class_escapes.insert(r, true);
        }
    }

    // A candidate is stack-eligible iff its dst's class does NOT escape. `stack_class_roots` is the
    // set of union-find ROOTS of the classes we are about to stack-allocate — every temp in such a
    // class is a stack-resident SSA copy of a stack record, so it is NEVER refcounted and we suppress
    // every Retain/Release on it (below).
    let mut stack_dsts: Vec<(usize, usize)> = Vec::new();
    let mut stack_class_roots: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for (bi, ii, dst) in &candidates {
        let r = uf.find(*dst);
        if !class_escapes.get(&r).copied().unwrap_or(false) {
            stack_dsts.push((*bi, *ii));
            stack_class_roots.insert(r);
        }
    }

    // Apply the stack marks on the MakeObject sites.
    for (bi, ii) in stack_dsts {
        if let Instruction::MakeObject { stack, .. } = &mut func.blocks[bi].instructions[ii] {
            *stack = true;
        }
    }

    if stack_class_roots.is_empty() {
        return;
    }

    // RC-emission suppression (the whole point of this milestone): the owning model in `lower.rs`
    // emits per-read `Retain` and scope-exit `Release` instructions on a sealed-record value as if
    // it were heap. For a value PROVEN stack-resident (its class is a `stack_class_roots` member)
    // those Retain/Release are pure no-ops (the alloca header is immortal), but they are CALLs across
    // the non-inlinable runtime boundary that the optimizer cannot elide — which is exactly why the
    // Stage-4-without-suppression prototype was ~12% SLOWER than heap. We DELETE them here so the
    // stack record carries NO refcount traffic and the alloca SROA-promotes to registers.
    //
    // Soundness: a stack class, by construction, NEVER escapes (no Return / container store / closure
    // capture / repr-changing-coerce store / unknown-retaining call touches it — `classify_instr`
    // would have marked the class escaping and we would not have stack-allocated it). So the value
    // and ALL its SSA copies die when the frame returns; there is no owner that needs a refcount.
    // Removing the Retain/Release on it cannot unbalance any other value's RC because the class is
    // closed under representation-preserving aliasing and is disjoint from every heap value's class.
    for block in &mut func.blocks {
        // Drop the stack-resident RC instructions while keeping the parallel debug-span side-table
        // (--debug only) in lockstep. We rebuild both vectors in one index-aligned pass rather than
        // `retain`, so an entry in `instr_spans` is removed iff its instruction is removed.
        let have_spans = block.instr_spans.len() == block.instructions.len();
        let mut kept_instrs = Vec::with_capacity(block.instructions.len());
        let mut kept_spans = Vec::with_capacity(block.instr_spans.len());
        for (i, instr) in block.instructions.drain(..).enumerate() {
            let keep = match &instr {
                Instruction::Retain { val, .. } | Instruction::Release { val, .. } => {
                    // Keep the instruction UNLESS its target is in a stack-resident class.
                    !stack_class_roots.contains(&uf.find_const(*val))
                }
                _ => true,
            };
            if keep {
                if have_spans {
                    kept_spans.push(block.instr_spans[i]);
                }
                kept_instrs.push(instr);
            }
        }
        block.instructions = kept_instrs;
        if have_spans {
            block.instr_spans = kept_spans;
        }
    }
}

/// 0xFE inline gate (Phase 1+2): for each `MakeArray` whose element type is a sealed scalar record,
/// mark `inline = true` iff:
///   1. Every ELEMENT temp is ONLY used in that MakeArray + Release instructions, AND
///   2. The ARRAY itself (its `dst` temp) only reaches SAFE consumers: reads (FieldGet/Index/
///      SealedArrayFieldGet), Release, or CONTAINER STORES (MakeObject field, IndexSet value,
///      nested MakeArray element, MakeClosure capture, GlobalValSet, MakeCell, CellSet).
///
/// Phase-2 extension: condition 2 now ALLOWS the array to escape into containers. An array stored
/// into a map value or record field is safe for 0xFE because:
///   - The container write path (emit_map_set/compile_ir_index_set) calls `compile_ir_box_keep_packed`
///     which wraps the 0xFE pointer in a TAG_ARRAY TaggedVal — no representation change, O(1).
///   - The container read path (sealed_array_project_from/sealed_array_project_owned) dispatches on
///     the runtime elem_tag (0xFE vs 0xFD), so 0xFE arrays read back correctly.
///   - The fused `arr[i].field` read (compile_ir_sealed_array_field_get) does a runtime tag dispatch
///     when the repr is not statically known inline (container-sourced arrays: repr inline=false).
/// Container stores do NOT alias array elements — only the ARRAY POINTER is stored, not any element
/// reference. The element aliasing is still guarded by condition 1.
///
/// What is NOT allowed (still fails safe to 0xFD):
///   - `Retain { val: arr }` — someone wants an extra owner via the boxed API; ambiguous aliasing.
///   - `Call`/`CallIntrinsic` arg (unknown callee might mutate elements via index set).
///   - `Return` of the array (crosses function boundary; caller may do anything).
///   - Closure captures of the array when the closure might mutate elements.
///
/// Fail-safe: on any doubt `inline` stays `false` (0xFD pointer-backed, correct today).
fn analyze_array_inline_fn(func: &mut LinFunction) {
    // Collect sealed-record-array MakeArray sites: (bi, ii, dst, elements).
    let mut make_array_sites: Vec<(usize, usize, Temp, Vec<Temp>)> = Vec::new();
    for (bi, block) in func.blocks.iter().enumerate() {
        for (ii, instr) in block.instructions.iter().enumerate() {
            if let Instruction::MakeArray { elem_ty, elements, dst, .. } = instr {
                if Type::sealed_array_elem(&Type::Array(Box::new(elem_ty.clone()))).is_some() {
                    make_array_sites.push((bi, ii, *dst, elements.clone()));
                }
            }
        }
    }
    if make_array_sites.is_empty() {
        return;
    }

    // For each site, decide inline=true/false. Collect decisions first, then apply (two-phase
    // to avoid holding an immutable borrow of func.blocks when we later need a mutable one).
    let decisions: Vec<bool> = make_array_sites
        .iter()
        .map(|(site_bi, site_ii, arr_dst, site_elements)| {
            // Check 1: all element temps used only in the MakeArray + Release.
            for &elem in site_elements {
                if !temp_escape_free(&func.blocks, elem, *site_bi, *site_ii, false) {
                    return false;
                }
            }
            // Check 2 (Phase-2 widened gate): array DST only reaches safe consumers.
            array_inline_escape_ok(&func.blocks, *arr_dst, *site_bi, *site_ii)
        })
        .collect();

    // Apply decisions.
    for ((site_bi, site_ii, _, _), safe) in make_array_sites.iter().zip(decisions.iter()) {
        if *safe {
            if let Instruction::MakeArray { inline, .. } =
                &mut func.blocks[*site_bi].instructions[*site_ii]
            {
                *inline = true;
            }
        }
    }
}

/// Phase-2 widened gate for the ARRAY dst of a 0xFE MakeArray site.
///
/// Returns true iff every use of temp `t` in `blocks` is INLINE-SAFE, meaning codegen handles
/// it correctly for a 0xFE array:
///   - at instruction (site_bi, site_ii) — the defining MakeArray itself,
///   - Release — scope-exit drop,
///   - read-only consumers: FieldGet/Index/SealedArrayFieldGet (local reads, repr statically inline),
///   - container stores: `MakeObject` field, `IndexSet` where `t` is the VALUE (not the container),
///     nested `MakeArray` element, `MakeClosure` capture, `GlobalValSet`, `MakeCell` init,
///     `CellSet` value.
///     These stores keep the 0xFE pointer tagged as TAG_ARRAY in the container slot; the read-back
///     path does a runtime elem_tag dispatch (0xFE vs 0xFD) so the inline layout is used correctly.
///   - Retain — a refcount bump; does NOT observe the element layout, safe.
///
/// What is NOT safe: Return (crosses frame → caller may do anything including `IndexSet` with
/// aliased element refs), TailCall, CondJump/Switch on the array value, or any Call/CallIntrinsic
/// arg (unknown callee may `IndexSet` into it using element aliases). Also NOT safe:
/// `IndexSet` where `t` is the CONTAINER (arr[k]=v, which might mutate elements).
fn array_inline_escape_ok(
    blocks: &[crate::ir::BasicBlock],
    t: Temp,
    site_bi: usize,
    site_ii: usize,
) -> bool {
    for (bi, block) in blocks.iter().enumerate() {
        for (ii, instr) in block.instructions.iter().enumerate() {
            if bi == site_bi && ii == site_ii {
                continue; // Defining instruction.
            }
            let uses = crate::liveness::instr_use_def(instr).0;
            if !uses.contains(&t) {
                continue;
            }
            let ok = match instr {
                Instruction::Release { val, .. } if *val == t => true,
                // Read-only consumers (statically known inline repr at these sites).
                Instruction::FieldGet { object, .. } if *object == t => true,
                Instruction::Index { object, .. } if *object == t => true,
                Instruction::SealedArrayFieldGet { array, .. } if *array == t => true,
                // Container stores of the ARRAY VALUE (not the container object being mutated).
                // These wrap the 0xFE pointer in TAG_ARRAY; the read path dispatches on elem_tag.
                Instruction::MakeObject { fields, spreads, .. } => {
                    // Safe only if `t` appears as a FIELD VALUE, not a spread source
                    // (a spread unpacks the object — but `t` is an array, not an object; the
                    // checker would reject `{...arr}` for a non-object type; treat defensively).
                    let as_field = fields.iter().any(|(_, fv)| *fv == t);
                    let as_spread = spreads.iter().any(|s| *s == t);
                    as_field && !as_spread
                }
                Instruction::IndexSet { value, object, .. } => {
                    // Safe if `t` is the VALUE being stored (arr goes INTO a container).
                    // NOT safe if `t` is the OBJECT (someone is doing `t[k]=v` — mutating
                    // elements of our inline array from outside, breaking the no-alias guarantee).
                    *value == t && *object != t
                }
                Instruction::MakeArray { elements, .. } => {
                    // Safe if `t` is one of the nested MakeArray's elements (storing the array
                    // pointer as an element of an outer array). The outer array stores a pointer;
                    // elements of `t` are never accessed through `t`'s element temps.
                    elements.contains(&t)
                }
                Instruction::MakeClosure { captures, .. } => {
                    // A closure that captures the array can later store into it (IndexSet via a
                    // captured arr reference with element aliases). NOT safe — fall through.
                    let _ = captures;
                    false
                }
                // GlobalValSet, MakeCell, CellSet: these store the array into a mutable location
                // that can later be read via GlobalValGet/CellGet. The read yields repr inline=false,
                // and any subsequent `Push` (Intrinsic::Push) takes the 0xFD push path, writing a
                // struct pointer into the inline byte buffer → data corruption. The `var arr = []`
                // + `push(arr, elem)` pattern hits exactly this path. NOT safe for 0xFE.
                Instruction::GlobalValSet { .. } => false,
                Instruction::MakeCell { .. } => false,
                Instruction::CellSet { .. } => false,
                // Retain is a refcount bump only; does NOT observe the inline layout.
                Instruction::Retain { val, .. } if *val == t => true,
                // Everything else (Call, CallIntrinsic args, unknown uses) — fail safe.
                _ => false,
            };
            if !ok {
                return false;
            }
        }
        // Terminators: Return and TailCall cross frame boundaries → not safe.
        let term_uses: Vec<Temp> = match &block.terminator {
            Terminator::Return(Some(v)) => vec![*v],
            Terminator::TailCall { args } => args.clone(),
            Terminator::CondJump { cond, .. } => vec![*cond],
            Terminator::Switch { val, .. } => vec![*val],
            _ => vec![],
        };
        if term_uses.contains(&t) {
            return false;
        }
    }
    true
}

/// Returns true iff every use of temp `t` in `blocks` is either:
///   - at instruction (site_bi, site_ii) [the defining instruction], or
///   - a Release { val: t }, or
///   - (when `allow_reads`) a read-only consumer: FieldGet/Index/SealedArrayFieldGet.
fn temp_escape_free(
    blocks: &[crate::ir::BasicBlock],
    t: Temp,
    site_bi: usize,
    site_ii: usize,
    allow_reads: bool,
) -> bool {
    for (bi, block) in blocks.iter().enumerate() {
        for (ii, instr) in block.instructions.iter().enumerate() {
            if bi == site_bi && ii == site_ii {
                continue; // The defining instruction — skip
            }
            let uses = crate::liveness::instr_use_def(instr).0;
            if uses.contains(&t) {
                if matches!(instr, Instruction::Release { val, .. } if *val == t) {
                    continue; // Scope-exit release: safe
                }
                if allow_reads {
                    let is_read_only = matches!(instr,
                        Instruction::FieldGet { object, .. } if *object == t
                    ) || matches!(instr,
                        Instruction::Index { object, .. } if *object == t
                    ) || matches!(instr,
                        Instruction::SealedArrayFieldGet { array, .. } if *array == t
                    );
                    if is_read_only {
                        continue;
                    }
                }
                return false; // Escaping consumer found
            }
        }
        // Check terminators.
        let term_uses: Vec<Temp> = match &block.terminator {
            Terminator::Return(Some(v)) => vec![*v],
            Terminator::TailCall { args } => args.clone(),
            Terminator::CondJump { cond, .. } => vec![*cond],
            Terminator::Switch { val, .. } => vec![*val],
            _ => vec![],
        };
        if term_uses.contains(&t) {
            return false;
        }
    }
    true
}

/// Classify one instruction: add carry edges (union) and mark escaping uses. EVERY use of a temp
/// that is not an explicit read-only / carry edge is marked ESCAPING (fail-safe).
///
/// The carry-edge UNIFICATION (Copy/Bind/Phi/Coerce-carry) is delegated to
/// [`carry::classify_carry_edges`] — the same machinery the repr pass uses — so the carry classes
/// here are byte-for-byte the historical ones. This function then layers the escape-specific marking
/// on top.
fn classify_instr(
    instr: &Instruction,
    uf: &mut UnionFind,
    escaping: &mut Vec<bool>,
    mark: &impl Fn(&mut Vec<bool>, Temp),
) {
    // Carry edges first (Copy/Bind/Phi/Coerce-carry unify; everything else no-ops here).
    carry::classify_carry_edges(instr, uf);

    match instr {
        // ---- Representation-preserving carry edges: unification already done above; do NOT mark. ----
        Instruction::Copy { .. } | Instruction::Bind { .. } | Instruction::Phi { .. } => {}
        Instruction::Coerce { dst, src, from_ty, to_ty } => {
            if carry::coerce_is_carry(from_ty, to_ty) {
                // Carry edge — unified above, no escape mark.
            } else if is_stack_eligible_type(from_ty)
                && !matches!(to_ty, Type::TypeVar(_))
            {
                // A representation-changing coerce whose SOURCE is a sealed scalar record and
                // whose TARGET is NOT a TypeVar (Json/AnyVal) ALWAYS goes through
                // `sealed_materialize_to_object` in codegen (compile_ir_coerce's `from_sealed`
                // arm), which COPIES each field value into a FRESH boxed `LinObject` — the source
                // struct pointer is NEVER stored in the result. So the source is only READ.
                //
                // EXCEPTION (Stage 6a, TypeVar target only): when the target IS a TypeVar
                // (Json = TypeVar(u32::MAX)), codegen emits `lin_box_record` which stores the
                // sealed struct pointer directly in the TAG_RECORD payload. The source DOES escape
                // through this coerce (the box may outlive the frame via the Json binding).
                // Mark the source as escaping ONLY for TypeVar targets to prevent stack-allocation
                // of a sealed struct whose pointer will be stored in a persistent TAG_RECORD box.
                //
                // Union and Named targets still use `sealed_materialize_to_object` in codegen
                // (they are if-merge/function-boundary cases, not persistent dynamic slots) —
                // the source sealed struct is read-only (content copied, pointer not stored).
                let _ = (dst, src);
            } else {
                // The source is NOT a stack-eligible sealed record (e.g. a Json/union value coerced
                // INTO a sealed record — the dst is the fresh struct, the src is some other value).
                // Mark the src escaping fail-safe; it is not in our candidate classes anyway unless
                // it carries a sealed record, in which case being conservative is correct.
                mark(escaping, *src);
            }
        }

        // ---- Read-only uses (do NOT mark escape) ----
        Instruction::FieldGet { object, .. } => {
            // The dst is the FIELD value (a fresh scalar/value), not an alias of the record. Reading
            // a field does not escape the record.
            let _ = object;
        }
        Instruction::Index { object, key, .. } => {
            // object is read; key is a literal/scalar for a sealed record. Neither escapes the record.
            let _ = (object, key);
        }
        Instruction::SealedArrayFieldGet { array, index, .. } => {
            let _ = (array, index);
        }
        Instruction::BoxedArrayFieldGet { array, index, .. } => {
            // The array is read (a borrowed element box, single field load); neither the array nor
            // the index escapes — the result is a fresh/owned scalar-or-heap field copy.
            let _ = (array, index);
        }
        // `object.field = value` stores `value` into the record's heap slot (it now outlives the
        // local), so the VALUE escapes. The `object` is only written through — not aliased out —
        // but a record that is mutated in place must stay heap-resident, so mark it escaping too
        // (fail-safe; suppresses stack allocation of a mutated record).
        Instruction::FieldSet { object, value, .. } => {
            mark(escaping, *object);
            mark(escaping, *value);
        }
        // A Release is the value's death (scope-exit drop). For a stack/immortal record it's a
        // no-op; it never makes the value escape.
        Instruction::Release { .. } => {}
        // A Retain only bumps a refcount; it does NOT store the pointer anywhere. The pointer's
        // actual destinations are tracked by the OTHER instructions that consume it. On a
        // stack/immortal record the retain is a no-op. So a Retain alone is never an escape — this
        // is what lets the owning model's per-read Retain+Release pairs on the param `s` coexist
        // with stack allocation (records.lin reads each field via an owning read that retains `s`).
        Instruction::Retain { .. } => {}

        // ---- Everything else: mark EVERY used temp as escaping (fail-safe) ----
        other => {
            let (uses, _defs) = crate::liveness::instr_use_def(other);
            for u in uses {
                mark(escaping, u);
            }
        }
    }
}
