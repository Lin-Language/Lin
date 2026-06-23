//! Intra-basic-block redundant-read elimination for `Index` and `FieldGet`.
//!
//! # What this pass does
//!
//! When the same `Index(obj, constKey)` or `FieldGet(obj, field)` appears more than once in
//! a basic block, and `obj`'s contents provably haven't changed between the two sites, the
//! second (and further) reads are replaced with a `Copy` of the first result temp.  All
//! subsequent instructions — in particular the `CloneBox`/`Retain`/`Release` that the lowerer
//! already emitted around the second read's `dst` — remain untouched, so RC balance is
//! unchanged.
//!
//! # Soundness model
//!
//! An available read `r = Index(obj, k)` (or `FieldGet`) is INVALIDATED (dropped from the
//! available set) by any instruction that could mutate `obj`'s contents or redefine the temps
//! involved:
//!
//! 1. A redefinition of the `obj` temp (any instruction whose `defs` include `obj`).
//! 2. A redefinition of the key temp (if the key is a temp, not a `Const`).
//! 3. A `FieldSet { object, .. }` or `IndexSet { object, .. }` whose target `object` **may
//!    alias** `obj` (conservative: same temp OR `obj` has escaped).
//! 4. Any `Call`/`CallIntrinsic`/`CallIndirect` — conservative: all available reads on
//!    non-escaped objects whose `obj` temp could have been passed to the call are invalidated.
//!    We use a simple alias check: if the call's args contain `obj` (or any alias of `obj`),
//!    we invalidate.  If `obj` is not among the call's args and has not previously escaped
//!    (per the alias set), we keep the available read.
//! 5. Any `Push`/`ArrayPush`/`ObjectSet`/`ObjectSetDyn`/`ArraySetDyn` intrinsic call —
//!    treated as a potential mutation of any object passed as an argument.
//!
//! Alias tracking: `Copy`, `Bind`, `Phi` (first incoming only for intra-block), and
//! `Coerce` edges propagate alias information.  If temp `b` is an alias of `obj`, a write
//! through `b` invalidates `obj`'s available reads.
//!
//! # RC safety
//!
//! Replacing `Index { dst: t2, .. }` with `Copy { dst: t2, src: t1 }` is RC-safe because:
//! - The first result `t1` is a live value (still accessible in the block — the intra-block
//!   scope guarantees it is not yet released at the second site, since Release instructions
//!   are inserted at scope exit after all uses).
//! - The second site's subsequent `CloneBox`/`Retain`/`Release` instructions remain
//!   unchanged; they now operate on the aliased `t1` value through `t2`, which is exactly
//!   what they would have done on a fresh read of the same slot value.
//! - For SCALAR result types (Int*, Float*, Bool, Null) there are no RC operations at all.
//!
//! # Scope
//!
//! Intra-basic-block only (v1).  Cross-block CSE along dominator paths is explicitly deferred
//! to avoid the added complexity of the dominance computation and block-exit invalidation.
//! It is always correct to eliminate less.

use std::collections::HashMap;
use std::collections::HashSet;

use crate::ir::*;
use crate::liveness::instr_use_def;

/// Run redundant-read elimination on all functions in the module.
pub fn run(module: &mut LinModule) {
    for func in &mut module.functions {
        run_fn(func);
    }
}

// A read that is "available": the result temp `dst` of a prior `Index` or `FieldGet`
// that has not been invalidated yet.
#[derive(Clone)]
struct AvailRead {
    /// The result temp of the first (defining) read.
    dst: Temp,
    /// The object temp being indexed.
    obj: Temp,
    #[allow(dead_code)]
    key: ReadKey,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum ReadKey {
    /// A static string field (FieldGet field name, or a string Const key).
    Field(String),
    /// A key that is a temp (dynamic key; only CSE if the key temp hasn't changed).
    TempKey(Temp),
}

/// Map from (obj_root, key, result_repr) → available read.
/// `obj_root` is the union-find root of the `obj` temp's alias class.
/// `result_repr` is a simplified representation tag so that two reads with the
/// same obj+key but different result types (e.g. one union-boxed, one concrete)
/// are NOT considered identical — they lower to different runtime operations.
type AvailMap = HashMap<(Temp, ReadKey, ResultRepr), AvailRead>;

/// Coarse representation of an Index/FieldGet result: union-boxed or concrete.
/// Two reads that produce the same repr can share their result temp; reads with
/// different reprs cannot (they call different runtime paths / produce incompatible
/// pointer types).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum ResultRepr {
    Union,    // TaggedVal* / boxed union / AnyVal
    Concrete, // raw concrete pointer (Array*, String*, Object*) or scalar
}

fn run_fn(func: &mut LinFunction) {
    // Collect replacements: (block_idx, instr_idx) → new_src temp.
    // We collect first, then apply, to avoid borrow conflicts.
    let mut replacements: Vec<(usize, usize, Temp, Temp)> = Vec::new(); // (bi, ii, old_dst, new_src)

    for (bi, block) in func.blocks.iter().enumerate() {
        let mut avail: AvailMap = HashMap::new();
        // alias_map: temp → canonical alias root (tracks Copy/Bind/Coerce aliases within block)
        let mut alias_map: HashMap<Temp, Temp> = HashMap::new();
        // Track which temps have "escaped" (passed to a call or container store earlier in block).
        let mut escaped: HashSet<Temp> = HashSet::new();

        for (ii, instr) in block.instructions.iter().enumerate() {
            match instr {
                Instruction::Index { dst, object, key, obj_ty, key_ty, result_ty, .. } => {
                    let obj_root = alias_root(*object, &alias_map);
                    let read_key = classify_key(*key, key_ty, &func.temp_types);
                    if let Some(read_key) = read_key {
                        let repr = result_repr(result_ty);
                        let map_key = (obj_root, read_key.clone(), repr);
                        if let Some(avail_read) = avail.get(&map_key) {
                            // Redundant read: schedule replacement.
                            replacements.push((bi, ii, *dst, avail_read.dst));
                        } else {
                            // First read: add to available set.
                            // Only CSE on Map (string-key) and non-array types — array indexing
                            // returns a fresh owned box per element (numeric key → Own convention)
                            // and can change each access; don't CSE array reads by default.
                            // For Map (Borrow convention) and FieldGet the slot pointer is stable
                            // as long as the map/object isn't mutated.
                            if !is_array_like(obj_ty) {
                                let _ = key_ty;
                                avail.insert(map_key, AvailRead { dst: *dst, obj: *object, key: read_key });
                            }
                        }
                    }
                    // The dst is a new definition: update alias_map.
                    alias_map.insert(*dst, *dst);
                }

                Instruction::FieldGet { dst, object, field, result_ty, .. } => {
                    let obj_root = alias_root(*object, &alias_map);
                    let read_key = ReadKey::Field(field.clone());
                    let repr = result_repr(result_ty);
                    let map_key = (obj_root, read_key.clone(), repr);
                    if let Some(avail_read) = avail.get(&map_key) {
                        replacements.push((bi, ii, *dst, avail_read.dst));
                    } else {
                        avail.insert(map_key, AvailRead { dst: *dst, obj: *object, key: read_key });
                    }
                    alias_map.insert(*dst, *dst);
                }

                // Alias-propagating instructions: Copy/Bind alias src → dst (same object).
                Instruction::Copy { dst, src } | Instruction::Bind { dst, src, .. } => {
                    let root = alias_root(*src, &alias_map);
                    alias_map.insert(*dst, root);
                }
                Instruction::Coerce { dst, src, .. } => {
                    // A coerce may change representation; conservatively NOT an alias.
                    // The dst is a new distinct value (no carry for CSE purposes).
                    alias_map.insert(*dst, *dst);
                    let _ = src;
                }
                Instruction::Phi { dst, incomings, .. } => {
                    // Cross-block merges: treat as opaque (not an alias within the block).
                    alias_map.insert(*dst, *dst);
                    let _ = incomings;
                }

                // FieldSet: invalidate available reads on the object (and its aliases).
                Instruction::FieldSet { object, .. } => {
                    let obj_root = alias_root(*object, &alias_map);
                    invalidate_by_obj(&mut avail, obj_root, &escaped);
                }

                // IndexSet: invalidate available reads on the object (and its aliases).
                Instruction::IndexSet { object, .. } => {
                    let obj_root = alias_root(*object, &alias_map);
                    invalidate_by_obj(&mut avail, obj_root, &escaped);
                }

                // Calls: conservatively invalidate all reads whose obj was passed to the call,
                // or whose obj has previously escaped (might be mutated inside the call).
                Instruction::Call { callee, args, dst, .. } => {
                    let call_arg_roots: HashSet<Temp> = args.iter().map(|a| alias_root(*a, &alias_map)).collect();
                    if let CallTarget::Indirect(t) = callee {
                        // also mark the callee temp as escaped (conservative)
                        escaped.insert(alias_root(*t, &alias_map));
                    }
                    invalidate_by_call(&mut avail, &call_arg_roots, &escaped);
                    // Mark all args as escaped (they may have been stored inside the call).
                    for a in args {
                        escaped.insert(alias_root(*a, &alias_map));
                    }
                    alias_map.insert(*dst, *dst);
                }

                Instruction::CallIntrinsic { intrinsic, args, dst, .. } => {
                    // Mutation intrinsics: always invalidate anything they touch.
                    if intrinsic_may_mutate(intrinsic) {
                        let call_arg_roots: HashSet<Temp> = args.iter().map(|a| alias_root(*a, &alias_map)).collect();
                        invalidate_by_call(&mut avail, &call_arg_roots, &escaped);
                        for a in args {
                            escaped.insert(alias_root(*a, &alias_map));
                        }
                    }
                    // Non-mutating intrinsics: args don't escape (conservative: mark escaped anyway
                    // since we don't track which intrinsics are truly pure).
                    else {
                        let call_arg_roots: HashSet<Temp> = args.iter().map(|a| alias_root(*a, &alias_map)).collect();
                        invalidate_by_call(&mut avail, &call_arg_roots, &escaped);
                        for a in args {
                            escaped.insert(alias_root(*a, &alias_map));
                        }
                    }
                    alias_map.insert(*dst, *dst);
                }

                // MakeClosure captures: mark captured temps as escaped.
                Instruction::MakeClosure { dst, captures, .. } => {
                    for c in captures {
                        escaped.insert(alias_root(*c, &alias_map));
                    }
                    alias_map.insert(*dst, *dst);
                }

                // CellSet: the cell value may be an alias of something we track.
                Instruction::CellSet { cell, value, .. } => {
                    escaped.insert(alias_root(*cell, &alias_map));
                    escaped.insert(alias_root(*value, &alias_map));
                }

                // Any other instruction that defines temps: add to alias_map as new definitions.
                other => {
                    let (_uses, defs) = instr_use_def(other);
                    for d in defs {
                        alias_map.insert(d, d);
                    }
                }
            }

            // After processing the instruction: if any available read's obj temp was
            // REDEFINED by this instruction, invalidate it.
            let (_uses, defs) = instr_use_def(instr);
            for def in &defs {
                let def_root = *def; // a new definition means this temp gets a fresh alias root
                // Invalidate any available read whose obj is this defined temp.
                invalidate_by_obj_exact(&mut avail, *def);
                // Also update the alias_map entry for this temp to itself (new def).
                alias_map.insert(*def, def_root);
            }
        }
    }

    // Apply replacements: substitute second Index/FieldGet with Copy.
    // Process in reverse order within each block so indices don't shift.
    replacements.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    for (bi, ii, old_dst, new_src) in replacements {
        let block = &mut func.blocks[bi];
        let have_spans = block.instr_spans.len() == block.instructions.len();
        // Replace the Index/FieldGet with a Copy.
        let span = if have_spans { block.instr_spans[ii] } else { None };
        // Get the type for the Copy from the replaced instruction's dst type.
        let ty = func.temp_types.get(&old_dst).cloned().unwrap_or(lin_check::types::Type::Null);
        block.instructions[ii] = Instruction::Copy { dst: old_dst, src: new_src };
        if have_spans {
            block.instr_spans[ii] = span;
        }
        let _ = ty;
    }
}

/// Get the canonical alias root of a temp (path-compressed union-find over alias_map).
fn alias_root(t: Temp, alias_map: &HashMap<Temp, Temp>) -> Temp {
    let mut cur = t;
    for _ in 0..32 {
        match alias_map.get(&cur) {
            Some(&parent) if parent != cur => cur = parent,
            _ => break,
        }
    }
    cur
}

/// Classify the key of an Index into a canonical ReadKey, or return None if
/// the key is not a constant string (in which case we can't CSE this read).
fn classify_key(key: Temp, key_ty: &lin_check::types::Type, temp_types: &HashMap<Temp, lin_check::types::Type>) -> Option<ReadKey> {
    // Check if the key's type is a string literal (StrLit) — a constant.
    match temp_types.get(&key).unwrap_or(key_ty) {
        lin_check::types::Type::StrLit(s) => Some(ReadKey::Field(s.clone())),
        _ => {
            // Non-constant key: use TempKey for CSE (same temp means same key value,
            // as long as the key temp hasn't been redefined).
            Some(ReadKey::TempKey(key))
        }
    }
}

/// Classify the result type of an Index/FieldGet into a coarse representation.
/// Two reads are only CSE candidates when their reprs match, because different reprs
/// imply different codegen paths (e.g. lin_unbox_ptr vs. lin_tagged_clone).
fn result_repr(ty: &lin_check::types::Type) -> ResultRepr {
    use lin_check::types::Type;
    match ty {
        Type::Union(_) | Type::TypeVar(_) => ResultRepr::Union,
        _ => ResultRepr::Concrete,
    }
}

/// True if the object type is an array (numeric-keyed container).
/// Array indexing uses `lin_array_get_tagged` which returns a FRESH owned value per access;
/// reads of the same index give independent values and CSE would change semantics.
fn is_array_like(ty: &lin_check::types::Type) -> bool {
    matches!(ty, lin_check::types::Type::Array(_) | lin_check::types::Type::FixedArray(_))
}

/// Invalidate all available reads whose `obj` root matches `obj_root`,
/// accounting for escape: if an available read's obj has escaped, it is also vulnerable
/// to aliased writes (but since we track by root, an alias of the same root already
/// covers that).
fn invalidate_by_obj(avail: &mut AvailMap, obj_root: Temp, _escaped: &HashSet<Temp>) {
    avail.retain(|(o, _, _), _| *o != obj_root);
}

/// Invalidate available reads whose `obj` temp is exactly `t` (raw temp, not alias root).
/// Used after a definition of a temp to kill any read that used the old value of `t`.
fn invalidate_by_obj_exact(avail: &mut AvailMap, t: Temp) {
    avail.retain(|(o, _, _), r| {
        // Remove if the obj root is this temp, or if the stored obj matches.
        *o != t && r.obj != t
    });
}

/// Invalidate available reads whose object was passed to a call (or has escaped).
fn invalidate_by_call(avail: &mut AvailMap, call_arg_roots: &HashSet<Temp>, escaped: &HashSet<Temp>) {
    avail.retain(|(obj_root, _, _), r| {
        // Keep if: the obj root was NOT an argument of this call, AND
        //          the obj root has NOT previously escaped (escaped objs are unsafe around any call).
        !call_arg_roots.contains(obj_root)
            && !escaped.contains(obj_root)
            && !call_arg_roots.contains(&r.obj)
            && !escaped.contains(&r.obj)
    });
}

/// True for intrinsics that mutate their arguments (object/array mutation ops).
fn intrinsic_may_mutate(intrinsic: &Intrinsic) -> bool {
    matches!(
        intrinsic,
        Intrinsic::Push
            | Intrinsic::ArrayPush
            | Intrinsic::ObjectSet
            | Intrinsic::ObjectSetDyn
            | Intrinsic::ArraySetDyn
            | Intrinsic::FlatArrayPush(_)
    )
}
