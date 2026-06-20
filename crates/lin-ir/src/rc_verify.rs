//! Static RC-balance verifier over the flat `LinIR` (Cluster 2 of `docs/COMPILER_COHERENCE.md`).
//!
//! This is a **verification-only** pass: it never mutates the IR and never changes codegen output.
//! It is gated OFF by default and only runs when `LIN_VERIFY_RC=1` is set in the environment, so it
//! can never break a normal build. Its job is to turn the project's recurring use-after-free /
//! double-free / leak bug class — currently found by multi-minute ASan/lldb bisects on the 240k-trip
//! RAPTOR bench — into an immediate, named diagnostic over the *final* lowered IR (after RC
//! insertion + `rc_elide` + the `escape` stack-alloc pass).
//!
//! ## What it checks, per function
//!
//! For every **owned heap value** (an `LinIR` temp produced by an unambiguous +1 *owning producer*),
//! across the whole control-flow graph (branches, merges, loops):
//!
//!   1. **Use-after-release** — the value is never *read* by a non-release instruction (nor by a
//!      terminator) on a path where it has already been released to refcount 0 on that path. This is
//!      a true cross-block check (forward "fully-released" dataflow to fixpoint).
//!   2. **Balance / leak / over-release** — on every control-flow path from the value's creation to
//!      a function exit, the net retain/release accounting is conservatively reconciled: a value that
//!      reaches a `Return`/`TailCall`/`Unreachable` still holding a *positive* net balance, having
//!      *never escaped* on that path (not returned, not moved into a container/closure/cell/call
//!      argument, not threaded into a tail call), is a **leak**; a value whose net balance ever goes
//!      *negative* on a straight path is an **over-release** (double-free).
//!
//! ## Conservatism (false positives are worse than misses for a first cut)
//!
//! RC is reference-counted, so retains can legitimately exceed releases and "moves" hand ownership
//! off without a `Release`. To avoid false positives the pass is deliberately *under-approximate*:
//!
//!   - It only TRACKS temps defined by an **unambiguous owning producer** (`MakeObject`/`MakeArray`/
//!     `MakeClosure`/`CloneBox`/`Box`/`Call`/owning heap `CallIntrinsic`). Borrowed producers
//!     (`FieldGet`/`Index`/`EnvCapture`/`CellGet`/`GlobalValGet`/`Unbox`) arrive with an *unknown*
//!     incoming refcount, so they are NOT balance-tracked (a release on them is not flagged).
//!   - **Escape is generous**: any pass into a `Call`/`CallIntrinsic` Own arg, `Make*` container/
//!     closure, `Cell`/`Field`/`Index` store, `GlobalValSet`, a tail-call argument, or a `Return`
//!     marks the value escaped — from then on it is *accounted for* and never leak-flagged. (This is
//!     intentionally broad: a real leak that also escapes is missed, but a legitimate move is never
//!     mis-flagged.)
//!   - Stack-allocated records (`MakeObject { stack: true }` / `MakeArray { inline / columnar }`) are
//!     NOT owning heap producers — the `escape` pass proved them non-escaping and suppressed their
//!     RC, so they carry no heap +1 to balance and are skipped.
//!   - At a CFG **merge**, a value is treated as accounted (escaped) only if it agrees on EVERY
//!     predecessor; its tracked balance is carried only when all predecessors agree, else the value
//!     is *dropped from tracking* (conservative: an ambiguous merge is never flagged).
//!   - `*IfDistinct` guarded frees and `FreeBoxShell`/`FreeCell` are treated as neutral (they free a
//!     guard/shell/cell pointer conditionally, not an unconditional refcount decrement of a tracked
//!     heap value).
//!
//! Everything skipped is documented inline. The pass reports to stderr and returns the violations;
//! it is purely advisory.

use std::collections::{HashMap, HashSet, VecDeque};

use lin_check::types::Type;

use crate::ir::*;
use crate::liveness::instr_use_def;
use crate::ownership_verify::intrinsic_conventions;

/// A single RC-balance violation. Report-only.
#[derive(Debug, Clone)]
pub struct RcViolation {
    pub func: String,
    pub block: u32,
    pub temp: Temp,
    pub kind: RcViolationKind,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RcViolationKind {
    /// An owned value reaches a function exit with a positive net balance and never escaped on that
    /// path → it is never released → a leak.
    Leak,
    /// An owned value's net balance went negative on a straight path → more releases than references
    /// → a double-free / over-release.
    OverRelease,
    /// An owned value is read (by a non-release instruction or a terminator) after it has been
    /// released to refcount 0 on that path → a use-after-free.
    UseAfterRelease,
}

impl RcViolationKind {
    pub fn label(self) -> &'static str {
        match self {
            RcViolationKind::Leak => "rc-leak",
            RcViolationKind::OverRelease => "rc-over-release",
            RcViolationKind::UseAfterRelease => "rc-use-after-release",
        }
    }
}

/// Run the verifier over an entire module. Returns every violation found. Never mutates.
pub fn verify_module(module: &LinModule) -> Vec<RcViolation> {
    let mut out = Vec::new();
    for func in &module.functions {
        verify_fn(func, &mut out);
    }
    out
}

/// The pipeline entry point, gated on `LIN_VERIFY_RC=1`. Runs the verifier over the final lowered
/// module and prints any violations to stderr. No-op (returns immediately) when the env var is unset
/// or `0`, so it can never affect a normal build. `where_` is a short label for the diagnostic (the
/// source path or import key).
///
/// `LIN_VERIFY_RC` modes:
///   - unset / `0`    → off (a normal build never runs the pass).
///   - `1`            → informational: prints any imbalances, QUIET on success, never fails the
///                      build (the dev/exploration mode).
///   - `strict` / `2` → CI GATE: prints any imbalances, then exits non-zero to FAIL the build, so a
///                      regression that unbalances RC breaks CI instead of surfacing later as a
///                      multi-minute ASan/lldb bisect.
pub fn verify_if_enabled(module: &LinModule, where_: &str) {
    let mode = match std::env::var("LIN_VERIFY_RC") {
        Ok(v) if v != "0" => v,
        _ => return,
    };
    let violations = verify_module(module);
    if violations.is_empty() {
        // Quiet on success: a clean module prints nothing, so a CI log only ever shows real
        // imbalances. (Earlier this printed a per-module "OK" line — noise for a gate.)
        return;
    }
    eprintln!(
        "[rc-verify] {where_}: {} imbalance(s) found over {} functions:",
        violations.len(),
        module.functions.len()
    );
    for v in &violations {
        eprintln!(
            "[rc-verify]   {} fn={} block={} t{}: {}",
            v.kind.label(),
            v.func,
            v.block,
            v.temp.0,
            v.detail
        );
    }
    // Debug aid (LIN_VERIFY_RC_DUMP=<fn-name-substr>): dump the offending function's IR.
    if let Ok(want) = std::env::var("LIN_VERIFY_RC_DUMP") {
        for f in &module.functions {
            let n = fname(f);
            if n.contains(&want) {
                eprintln!("--- IR dump fn={n} ---");
                for b in &f.blocks {
                    eprintln!("  block {}:", b.id.0);
                    for i in &b.instructions {
                        eprintln!("    {i:?}");
                    }
                    eprintln!("    term: {:?}", b.terminator);
                }
            }
        }
    }
    if mode == "strict" || mode == "2" {
        eprintln!("[rc-verify] FAILED (LIN_VERIFY_RC=strict): RC imbalance(s) in {where_}");
        std::process::exit(1);
    }
}

fn fname(func: &LinFunction) -> String {
    func.name.clone().unwrap_or_else(|| format!("fn#{}", func.id.0))
}

/// If `instr` defines a value with a KNOWN fresh refcount of exactly +1 the current scope owns,
/// return its destination temp. This is the set the balance check seeds at +1. Borrowed producers
/// (`FieldGet`/`Index`/`EnvCapture`/`CellGet`/`GlobalValGet`/`Unbox`) return an interior/aliased
/// pointer with an UNKNOWN incoming count and are deliberately excluded (a release on them is not
/// balance-checked).
///
/// Stack-allocated records carry no heap +1 (the `escape` pass proved them non-escaping and
/// suppressed their RC), so they are NOT owning producers — excluded.
///
/// For a `CallIntrinsic` we only treat the result as owned when the hand-audited convention table
/// says the return is `Own` (fresh +1) AND the result is a heap value (not a scalar); a `Borrow`
/// return (e.g. `ObjectGet`/`ArrayGet`/`UnboxPtr`/`Freeze`) is an interior pointer and is excluded.
fn owning_producer_def(instr: &Instruction) -> Option<Temp> {
    use Instruction::*;
    match instr {
        MakeObject { dst, stack: false, .. } => Some(*dst),
        MakeArray { dst, inline: false, columnar: false, .. } => Some(*dst),
        MakeClosure { dst, .. } | CloneBox { dst, .. } | Box { dst, .. } => Some(*dst),
        // A direct/indirect/named call is an unambiguous fresh +1 ONLY when its result is a CONCRETE
        // refcounted heap value (`Str`/`Array`/`Object`/`Map`/`Iterator`/`Function`/nullable-record).
        // Two exclusions, both deliberate to stay conservative:
        //   - a SCALAR result (`Int`/`Bool`/`Float`/`Null`) carries no heap reference — tracking it
        //     would leak-flag a scalar that is correctly never released;
        //   - a UNION / `TypeVar` (Json) / `Named` result flows through the lowerer's box/clone/coerce
        //     ownership machinery (`CloneBox`/`Coerce`/`FreeBoxShell`), where this verifier cannot
        //     reliably tell a legitimate move from a leak. So a boxed-union call result is NOT tracked
        //     (it is conservatively assumed accounted-for by that machinery). This drops the
        //     boxed-comparator-result noise (`f(x) < pivot` whose `f` returns `TypeVar`) while keeping
        //     the high-signal concrete-heap leaks (a returned `String`/`Array`/`Object` never freed).
        Call { dst, ret_ty, .. } if is_concrete_rc_ty(ret_ty) => Some(*dst),
        CallIntrinsic { dst, intrinsic, ret_ty, .. }
            if intrinsic_conventions(intrinsic).map(|c| c.ret) == Some(Convention::Own)
                && intrinsic_ret_is_heap(intrinsic)
                && ty_is_owned_heap(ret_ty) =>
        {
            Some(*dst)
        }
        _ => None,
    }
}

/// Whether a value of `ty` is a refcounted HEAP value the verifier balance-tracks (a fresh +1 that
/// must be released or escape). This is the union of the concrete-rc set and the boxed-union set —
/// the same two sets the lowerer's owning model uses (`ownership_verify::owning_strategy`'s `Clone`
/// vs `Retain` arms). A scalar (`Int`/`Bool`/`Float`/`Null`/`StrLit`-of-scalar/sum-node) is NOT
/// tracked. Conservative: when in doubt we exclude (under-track), since a missed leak is preferred
/// over a false positive.
fn ty_is_owned_heap(ty: &Type) -> bool {
    use crate::ownership_verify::owning_strategy;
    use crate::ownership_verify::OwningStrategy;
    matches!(owning_strategy(ty), OwningStrategy::Clone | OwningStrategy::Retain)
}

/// Whether an `Own`-returning intrinsic's result is a HEAP value worth balance-tracking (vs a scalar
/// like a length / bool / tag whose `Own` convention is the harmless scalar default). We only track
/// intrinsics whose result is a fresh heap allocation we could leak. Conservative: anything not
/// clearly a heap allocator is excluded.
fn intrinsic_ret_is_heap(intr: &Intrinsic) -> bool {
    use Intrinsic::*;
    matches!(
        intr,
        StringConcat
            | Concat
            | ArrayAlloc
            | ObjectAlloc
            | ArrayAllocate
            | ArrayAllocateFilled
            | Keys
            | ToString
            | TaggedToString
            | IntToString
            | FloatToString
            | BoolToString
            | NullToString
            | ValueKey
            | ToJson
            | BoxNull
            | BoxBool
            | BoxInt32
            | BoxInt64
            | BoxFloat64
            | BoxStr
            | BoxObject
            | BoxArray
            | BoxFunction
    )
}

/// Per-temp lattice value carried through the forward dataflow.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Acct {
    /// Tracked, currently holding `balance` net references (always >= 0 once normalised).
    Live(i32),
    /// Fully released to refcount 0 on this path (reading it now is a use-after-free).
    Dead,
    /// Escaped — moved into a call/container/closure/cell/global/return/tail-call. From here on it is
    /// accounted for and never leak-flagged.
    Escaped,
}

/// The dataflow state at a program point: the accounting status of every tracked owning temp.
/// A temp ABSENT from the map is "not yet tracked / dropped at an ambiguous merge".
type State = HashMap<Temp, Acct>;

/// Merge `incoming` (a predecessor's exit state) into `acc` (the block's accumulated entry state).
/// `first` is true for the first predecessor (acc starts empty). The join is the conservative
/// intersection-with-agreement: a temp survives only when every predecessor agrees on its status,
/// otherwise it is dropped from tracking (so an ambiguous merge is never flagged).
fn merge_into(acc: &mut State, incoming: &State, first: bool) {
    if first {
        *acc = incoming.clone();
        return;
    }
    acc.retain(|t, a| incoming.get(t) == Some(a));
}

fn verify_fn(func: &LinFunction, out: &mut Vec<RcViolation>) {
    if func.blocks.is_empty() {
        return;
    }

    // --- Reachability + predecessors over terminator successors ---
    let succ = |term: &Terminator| -> Vec<BlockId> {
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
    };
    let mut preds: HashMap<BlockId, Vec<BlockId>> = HashMap::new();
    let mut reachable: HashSet<BlockId> = HashSet::new();
    let mut q: VecDeque<BlockId> = VecDeque::new();
    let entry = func.blocks[0].id;
    reachable.insert(entry);
    q.push_back(entry);
    while let Some(bid) = q.pop_front() {
        if let Some(b) = func.block(bid) {
            for s in succ(&b.terminator) {
                preds.entry(s).or_default().push(bid);
                if reachable.insert(s) {
                    q.push_back(s);
                }
            }
        }
    }

    // Block order for the forward fixpoint (in the order blocks appear; the worklist below
    // re-processes until stable, so exact order only affects convergence speed).
    let block_ids: Vec<BlockId> =
        func.blocks.iter().map(|b| b.id).filter(|id| reachable.contains(id)).collect();

    // entry_state[b] = the joined dataflow state at the START of block b.
    let mut entry_state: HashMap<BlockId, State> = HashMap::new();
    for id in &block_ids {
        entry_state.insert(*id, State::new());
    }

    // Forward dataflow to fixpoint. Violations are collected only on the FINAL pass to avoid
    // duplicate reports from intermediate (non-converged) iterations.
    let mut iters = 0usize;
    // A generous cap: the lattice (Live(n)/Dead/Escaped, dropping on disagreement) is finite and
    // monotone toward "dropped"/"escaped", so it converges quickly; the cap is a safety net.
    let max_iters = block_ids.len().saturating_mul(8).max(16);
    let mut changed = true;
    while changed && iters < max_iters {
        changed = false;
        iters += 1;
        for &bid in &block_ids {
            // Join predecessors' exit states into this block's entry state.
            let mut joined = State::new();
            let mut first = true;
            let pset = preds.get(&bid).cloned().unwrap_or_default();
            for p in &pset {
                if !reachable.contains(p) {
                    continue;
                }
                let pexit = block_exit_state(func, *p, entry_state.get(p).cloned().unwrap_or_default());
                merge_into(&mut joined, &pexit, first);
                first = false;
            }
            if entry_state.get(&bid) != Some(&joined) {
                entry_state.insert(bid, joined);
                changed = true;
            }
        }
    }

    // Final pass: walk each block with its converged entry state, COLLECTING violations.
    for &bid in &block_ids {
        let block = match func.block(bid) {
            Some(b) => b,
            None => continue,
        };
        let st = entry_state.get(&bid).cloned().unwrap_or_default();
        run_block(func, block, st, out, /*collect=*/ true);
    }
}

/// Compute a block's EXIT state from its entry state, WITHOUT collecting violations (used during the
/// fixpoint). Pure transfer function.
fn block_exit_state(func: &LinFunction, bid: BlockId, entry: State) -> State {
    let block = match func.block(bid) {
        Some(b) => b,
        None => return entry,
    };
    let mut sink = Vec::new();
    run_block(func, block, entry, &mut sink, /*collect=*/ false)
}

/// Transfer function for one block. Walks the instructions then the terminator, updating the
/// per-temp accounting. When `collect` is true, appends any violations found to `out`. Returns the
/// block's exit state.
fn run_block(
    func: &LinFunction,
    block: &BasicBlock,
    entry: State,
    out: &mut Vec<RcViolation>,
    collect: bool,
) -> State {
    let mut st = entry;

    for instr in &block.instructions {
        // 1. A new owning producer seeds its result at +1 (balance 1). It overwrites any prior
        //    tracking of that temp (SSA temps are single-def in practice; if reused, the fresh def
        //    legitimately resets the count).
        if let Some(d) = owning_producer_def(instr) {
            st.insert(d, Acct::Live(1));
        }

        match instr {
            // ---- Rename / merge MOVE edges: the source's reference flows into the dst ----
            // `Copy`/`Bind` rename a value; `Phi` merges per-predecessor values into one. In every
            // case the source/incoming reference is HANDED INTO the destination (the lowerer's RC
            // accounts for the dst, not the source). Mark each source escaped so it is accounted for
            // — without this, a value Phi-merged into the returned temp (e.g. RAPTOR's
            // `reduceReversed`, where a `CloneBox` result is Phi'd into the return value) false-leaks.
            // We do NOT seed the dst as a fresh owning producer: a renamed/merged value's balance is
            // owned by its real producer + the scope's release of the dst, which the lowerer emits.
            Instruction::Copy { src, .. } | Instruction::Bind { src, .. } => {
                escape(&mut st, *src);
            }
            Instruction::Phi { incomings, .. } => {
                for (t, _) in incomings {
                    escape(&mut st, *t);
                }
            }

            // ---- Coerce: an ownership-carrying MOVE edge (box/unbox/widen/no-op) ----
            // The source's reference flows into the destination (a no-op coerce is the same value;
            // a repr-changing box/unbox hands the inner +1 to the new box, whose shell the lowerer
            // reclaims via FreeBoxShell). Either way the SOURCE is handed off — mark it escaped so
            // it is accounted for, not leak-flagged. We deliberately do NOT seed `dst` as a fresh
            // owned producer: a coerced result is conservatively treated as already-accounted (the
            // lowerer's coerce/own model balances it via the FreeBoxShell + the source's own scope),
            // so we under-track here rather than risk a false positive on the balanced shell-free.
            Instruction::Coerce { src, .. } => {
                escape(&mut st, *src);
            }

            // ---- Retain: a genuine +1 on the SOURCE's own refcount ----
            Instruction::Retain { val, .. } => {
                bump(&mut st, *val, 1);
            }
            // ---- CloneBox / Box: produce an INDEPENDENT fresh box from the source ----
            // The dst is a fresh +1 (seeded above by owning_producer_def). The SOURCE is consumed by
            // the lowerer's own/clone model: a `CloneBox` is emitted precisely when the source is a
            // BORROWED interior box that must be cloned before it escapes (so the source is NOT a +1
            // this scope must release), and a `Box` wraps a scalar (no source +1 at all). Either way
            // we hand the source off — mark it escaped (accounted for), never balance it. Bumping the
            // source here was the false-positive: it double-counted a borrowed call-result that the
            // CloneBox itself is reclaiming. (`Box`/`CloneBox` of a still-needed value is covered
            // because the source's REAL owner balances it; we only stop tracking it from this site.)
            Instruction::CloneBox { src, .. } | Instruction::Box { val: src, .. } => {
                escape(&mut st, *src);
            }

            // ---- Releases: -1; flag over-release if it drives a tracked balance negative ----
            Instruction::Release { val, .. } => {
                release(&mut st, *val, func, block, out, collect, instr);
            }
            // ReleaseIfDistinct frees the value conditionally (only when distinct from `other`). It
            // is NOT an unconditional decrement we can balance-track, and reading `other` is a guard
            // compare, not a deref — treat as neutral (do not flag, do not decrement). Documented skip.
            Instruction::ReleaseIfDistinct { .. } => {}

            // FreeCell releases the cell's owned VALUE then frees the cell. The cell pointer is its
            // own lifecycle (a MakeCell result), not the boxed heap values we track; neutral.
            Instruction::FreeCell { .. } => {}
            // Box-shell frees reclaim only the 16-byte shell, not the inner heap +1 we balance —
            // neutral. (FreeBoxShellIfDistinct additionally reads a guard pointer.)
            Instruction::FreeBoxShell { .. } | Instruction::FreeBoxShellIfDistinct { .. } => {}

            // ---- Escape positions: the value is moved out / handed off ----
            Instruction::Call { args, callee, .. } => {
                for a in args {
                    escape(&mut st, *a);
                }
                if let CallTarget::Indirect(t) = callee {
                    // The closure value is read (invoked); a read after release is a UAF, but it is
                    // not consumed — check as a use, not an escape.
                    use_check(&st, *t, func, block, out, collect, instr);
                }
            }
            Instruction::CallIntrinsic { intrinsic, args, .. } => {
                // Use the hand-audited convention table: Own args escape (consumed), Borrow/Inout
                // args are reads (use-after-release checked but not escaped/decremented).
                match intrinsic_conventions(intrinsic) {
                    Some(conv) => {
                        for (i, a) in args.iter().enumerate() {
                            match conv.params.get(i).copied().unwrap_or(Convention::Own) {
                                Convention::Own => escape(&mut st, *a),
                                Convention::Borrow | Convention::Inout => {
                                    use_check(&st, *a, func, block, out, collect, instr)
                                }
                            }
                        }
                    }
                    None => {
                        for a in args {
                            escape(&mut st, *a);
                        }
                    }
                }
            }
            Instruction::MakeClosure { captures, .. } => {
                for c in captures {
                    escape(&mut st, *c);
                }
            }
            Instruction::MakeObject { fields, spreads, .. } => {
                for (_, t) in fields {
                    escape(&mut st, *t);
                }
                for s in spreads {
                    escape(&mut st, *s);
                }
            }
            Instruction::MakeArray { elements, .. } => {
                for e in elements {
                    escape(&mut st, *e);
                }
            }
            Instruction::MakeCell { init, .. } => escape(&mut st, *init),
            Instruction::CellSet { value, cell, .. } => {
                use_check(&st, *cell, func, block, out, collect, instr);
                escape(&mut st, *value);
            }
            Instruction::FieldSet { object, value, .. } => {
                use_check(&st, *object, func, block, out, collect, instr);
                escape(&mut st, *value);
            }
            Instruction::IndexSet { object, key, value, .. } => {
                use_check(&st, *object, func, block, out, collect, instr);
                use_check(&st, *key, func, block, out, collect, instr);
                escape(&mut st, *value);
            }
            Instruction::GlobalValSet { value, .. } => escape(&mut st, *value),

            // ---- Pure read positions: use-after-release check only ----
            // Everything else: the instruction's USE operands (from instr_use_def) are reads. A read
            // of a Dead temp is a use-after-free. The escape/release/store arms above all `return`
            // from the match, so this default arm covers only the read-only remainder.
            other => {
                let (uses, _defs) = instr_use_def(other);
                for u in &uses {
                    use_check(&st, *u, func, block, out, collect, other);
                }
            }
        }
    }

    // --- Terminator transfer ---
    match &block.terminator {
        Terminator::Return(Some(t)) => {
            // The returned value escapes (the caller receives the +1).
            escape(&mut st, *t);
        }
        Terminator::TailCall { args } => {
            // Tail-call args are moved into the next iteration's param slots (ownership transferred
            // by the TCO release-old machinery). Escape them.
            for a in args {
                escape(&mut st, *a);
            }
        }
        Terminator::CondJump { cond, .. } => {
            use_check(&st, *cond, func, block, out, collect, &Instruction::Panic { msg: *cond });
        }
        Terminator::Switch { val, .. } => {
            use_check(&st, *val, func, block, out, collect, &Instruction::Panic { msg: *val });
        }
        Terminator::Jump(_) | Terminator::Return(None) | Terminator::Unreachable => {}
    }

    // --- Leak check at a NORMAL function exit (Return / TailCall) ---
    // `Unreachable` is DELIBERATELY excluded: it follows a `Panic` (or other diverging call), so the
    // process aborts and the OS reclaims everything — a value "unreleased" before a panic is not a
    // leak. Flagging it false-positives on every `match` with a non-exhaustive default-Panic arm
    // (e.g. RAPTOR's `createRaptor`, which builds its result maps before a date-match whose default
    // panics). So only the two NORMAL exits are leak-checked.
    let is_exit =
        matches!(block.terminator, Terminator::Return(_) | Terminator::TailCall { .. });
    if collect && is_exit {
        for (t, a) in st.iter() {
            if let Acct::Live(n) = a {
                if *n > 0 {
                    out.push(RcViolation {
                        func: fname(func),
                        block: block.id.0,
                        temp: *t,
                        kind: RcViolationKind::Leak,
                        detail: format!(
                            "owned heap value t{} reaches a function exit (block {}) with net balance +{} and never escaped on this path — never released (leak)",
                            t.0, block.id.0, n
                        ),
                    });
                }
            }
        }
    }

    st
}

/// Bump a tracked, Live temp's balance by `delta` (no-op if untracked / Dead / Escaped).
fn bump(st: &mut State, t: Temp, delta: i32) {
    if let Some(Acct::Live(n)) = st.get_mut(&t) {
        *n += delta;
    }
}

/// Apply a `Release` to a tracked temp: -1, flag over-release on negative, mark Dead at 0.
fn release(
    st: &mut State,
    t: Temp,
    func: &LinFunction,
    block: &BasicBlock,
    out: &mut Vec<RcViolation>,
    collect: bool,
    instr: &Instruction,
) {
    match st.get(&t).copied() {
        Some(Acct::Live(n)) => {
            let nn = n - 1;
            if nn < 0 {
                if collect {
                    out.push(RcViolation {
                        func: fname(func),
                        block: block.id.0,
                        temp: t,
                        kind: RcViolationKind::OverRelease,
                        detail: format!(
                            "owned heap value t{} released more times than owned (net balance {} after {instr:?}) — over-release / double-free",
                            t.0, nn
                        ),
                    });
                }
                // Clamp at Dead so we don't cascade more reports off the same root cause.
                st.insert(t, Acct::Dead);
            } else if nn == 0 {
                st.insert(t, Acct::Dead);
            } else {
                st.insert(t, Acct::Live(nn));
            }
        }
        Some(Acct::Dead) => {
            // Releasing an already-dead value is itself an over-release.
            if collect {
                out.push(RcViolation {
                    func: fname(func),
                    block: block.id.0,
                    temp: t,
                    kind: RcViolationKind::OverRelease,
                    detail: format!("owned heap value t{} released after already fully released — double-free", t.0),
                });
            }
        }
        // Untracked (borrowed producer / param / dropped at merge) or already Escaped: not balance-
        // checked. A release of an Escaped temp is the legitimate "the container/caller took its own
        // ref and this scope drops its borrowed copy" case — we conservatively do not flag it.
        _ => {}
    }
}

/// Mark a tracked temp as escaped (moved/handed off). Untracked temps stay untracked.
fn escape(st: &mut State, t: Temp) {
    if st.contains_key(&t) {
        st.insert(t, Acct::Escaped);
    }
}

/// A read of `t` by a non-release instruction: if `t` is Dead on this path, it's a use-after-free.
fn use_check(
    st: &State,
    t: Temp,
    func: &LinFunction,
    block: &BasicBlock,
    out: &mut Vec<RcViolation>,
    collect: bool,
    instr: &Instruction,
) {
    if collect && matches!(st.get(&t), Some(Acct::Dead)) {
        out.push(RcViolation {
            func: fname(func),
            block: block.id.0,
            temp: t,
            kind: RcViolationKind::UseAfterRelease,
            detail: format!("owned heap value t{} read after it was fully released on this path, in {instr:?} — use-after-free", t.0),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn func(blocks: Vec<BasicBlock>, params: Vec<(Temp, Type)>, temp_count: u32) -> LinFunction {
        let mut temp_types = HashMap::new();
        for (t, ty) in &params {
            temp_types.insert(*t, ty.clone());
        }
        LinFunction {
            id: FuncId(0),
            name: Some("t".into()),
            params,
            is_closure: false,
            ret_ty: Type::Null,
            param_conventions: Vec::new(),
            ret_convention: Convention::Own,
            blocks,
            temp_types,
            temp_count,
            intrinsic_slots: HashMap::new(),
            repr: Vec::new(),
            coverage_origin: None,
        }
    }

    fn blk(id: u32, instrs: Vec<Instruction>, term: Terminator) -> BasicBlock {
        BasicBlock {
            id: BlockId(id),
            label: None,
            instructions: instrs,
            terminator: term,
            span: None,
            instr_spans: Vec::new(),
        }
    }

    fn arr_ty() -> Type {
        Type::Array(Box::new(Type::Int32))
    }

    fn make_arr(dst: u32) -> Instruction {
        Instruction::MakeArray {
            dst: Temp(dst),
            elements: vec![],
            spreads: vec![],
            elem_ty: Type::Int32,
            inline: false,
            columnar: false,
        }
    }

    /// A fresh owned array that is created and never released and never escapes → leak.
    #[test]
    fn catches_straight_line_leak() {
        let f = func(vec![blk(0, vec![make_arr(0)], Terminator::Return(None))], vec![], 1);
        let mut out = Vec::new();
        verify_fn(&f, &mut out);
        assert!(
            out.iter().any(|v| v.kind == RcViolationKind::Leak && v.temp == Temp(0)),
            "expected a leak for t0, got {out:?}"
        );
    }

    /// A fresh owned array created and released exactly once → balanced, no violation.
    #[test]
    fn balanced_create_release_is_clean() {
        let f = func(
            vec![blk(
                0,
                vec![make_arr(0), Instruction::Release { val: Temp(0), ty: arr_ty() }],
                Terminator::Return(None),
            )],
            vec![],
            1,
        );
        let mut out = Vec::new();
        verify_fn(&f, &mut out);
        assert!(out.is_empty(), "balanced create+release must be clean, got {out:?}");
    }

    /// A fresh owned array released twice with no intervening retain → over-release.
    #[test]
    fn catches_over_release() {
        let f = func(
            vec![blk(
                0,
                vec![
                    make_arr(0),
                    Instruction::Release { val: Temp(0), ty: arr_ty() },
                    Instruction::Release { val: Temp(0), ty: arr_ty() },
                ],
                Terminator::Return(None),
            )],
            vec![],
            1,
        );
        let mut out = Vec::new();
        verify_fn(&f, &mut out);
        assert!(
            out.iter().any(|v| v.kind == RcViolationKind::OverRelease && v.temp == Temp(0)),
            "expected over-release for t0, got {out:?}"
        );
    }

    /// Balanced retain/release: +1 (make) +1 (retain) -1 -1 → net 0, no over-release, no leak.
    #[test]
    fn balanced_retain_release_pairs_clean() {
        let f = func(
            vec![blk(
                0,
                vec![
                    make_arr(0),
                    Instruction::Retain { val: Temp(0), ty: arr_ty() },
                    Instruction::Release { val: Temp(0), ty: arr_ty() },
                    Instruction::Release { val: Temp(0), ty: arr_ty() },
                ],
                Terminator::Return(None),
            )],
            vec![],
            1,
        );
        let mut out = Vec::new();
        verify_fn(&f, &mut out);
        assert!(out.is_empty(), "balanced retain/release pairs must be clean, got {out:?}");
    }

    /// A fresh owned array returned (escapes) → the caller owns the +1; no leak.
    #[test]
    fn returned_value_escapes_no_leak() {
        let f = func(vec![blk(0, vec![make_arr(0)], Terminator::Return(Some(Temp(0))))], vec![], 1);
        let mut out = Vec::new();
        verify_fn(&f, &mut out);
        assert!(out.is_empty(), "returned (escaped) value must not leak, got {out:?}");
    }

    /// Use-after-release: the array is released to 0, then read by a FieldGet → use-after-free.
    #[test]
    fn catches_use_after_release() {
        let rec = arr_ty();
        let f = func(
            vec![blk(
                0,
                vec![
                    make_arr(0),
                    Instruction::Release { val: Temp(0), ty: rec.clone() },
                    Instruction::FieldGet {
                        dst: Temp(1),
                        object: Temp(0),
                        field: "x".into(),
                        obj_ty: rec.clone(),
                        result_ty: Type::Int32,
                    },
                ],
                Terminator::Return(None),
            )],
            vec![],
            2,
        );
        let mut out = Vec::new();
        verify_fn(&f, &mut out);
        assert!(
            out.iter().any(|v| v.kind == RcViolationKind::UseAfterRelease && v.temp == Temp(0)),
            "expected use-after-release for t0, got {out:?}"
        );
    }

    /// Branch + merge: a value released on BOTH arms is balanced; released on only ONE arm leaks on
    /// the other.
    #[test]
    fn branch_released_on_both_arms_clean_but_one_arm_leaks() {
        // Balanced: released on both arms.
        let balanced = func(
            vec![
                blk(0, vec![make_arr(0)], Terminator::CondJump { cond: Temp(0), then_block: BlockId(1), else_block: BlockId(2) }),
                blk(1, vec![Instruction::Release { val: Temp(0), ty: arr_ty() }], Terminator::Return(None)),
                blk(2, vec![Instruction::Release { val: Temp(0), ty: arr_ty() }], Terminator::Return(None)),
            ],
            vec![],
            1,
        );
        let mut out = Vec::new();
        verify_fn(&balanced, &mut out);
        assert!(out.is_empty(), "value released on both arms must be clean, got {out:?}");

        // Leaks: released only on the THEN arm; the ELSE arm reaches Return still +1.
        let leaky = func(
            vec![
                blk(0, vec![make_arr(0)], Terminator::CondJump { cond: Temp(0), then_block: BlockId(1), else_block: BlockId(2) }),
                blk(1, vec![Instruction::Release { val: Temp(0), ty: arr_ty() }], Terminator::Return(None)),
                blk(2, vec![], Terminator::Return(None)),
            ],
            vec![],
            1,
        );
        let mut out2 = Vec::new();
        verify_fn(&leaky, &mut out2);
        assert!(
            out2.iter().any(|v| v.kind == RcViolationKind::Leak && v.temp == Temp(0) && v.block == 2),
            "expected a leak on the else arm (block 2), got {out2:?}"
        );
    }

    /// A value passed as an argument to a Call escapes (moved into the callee) → no leak.
    #[test]
    fn call_arg_escapes_no_leak() {
        let f = func(
            vec![blk(
                0,
                vec![
                    make_arr(0),
                    Instruction::Call {
                        dst: Temp(1),
                        callee: CallTarget::Named("sink".into()),
                        args: vec![Temp(0)],
                        ret_ty: Type::Null,
                    },
                    // The call result t1 is owning; release it so it doesn't itself leak.
                    Instruction::Release { val: Temp(1), ty: Type::Null },
                ],
                Terminator::Return(None),
            )],
            vec![],
            2,
        );
        let mut out = Vec::new();
        verify_fn(&f, &mut out);
        assert!(!out.iter().any(|v| v.temp == Temp(0)), "value moved into a call must not leak, got {out:?}");
    }

    /// A SCALAR-returning call (e.g. a length / comparison helper) must NOT be balance-tracked — its
    /// `Int32` result carries no heap reference, so reaching exit without a Release is not a leak.
    /// (Regression for the std-string-`contains` false positive: `lin_string_index_of -> Int32`.)
    #[test]
    fn scalar_returning_call_is_not_tracked() {
        let f = func(
            vec![blk(
                0,
                vec![Instruction::Call {
                    dst: Temp(0),
                    callee: CallTarget::Named("lin_string_index_of".into()),
                    args: vec![],
                    ret_ty: Type::Int32,
                }],
                Terminator::Return(None),
            )],
            vec![],
            1,
        );
        let mut out = Vec::new();
        verify_fn(&f, &mut out);
        assert!(out.is_empty(), "a scalar-returning call result must not be leak-tracked, got {out:?}");
    }

    /// An owned value live at a `Panic`/`Unreachable` exit must NOT be leak-flagged: the process
    /// aborts, so the OS reclaims everything — a value unreleased before a panic is not a leak.
    /// (Regression for the RAPTOR `createRaptor` non-exhaustive-match-default-Panic false positives.)
    #[test]
    fn value_live_at_panic_unreachable_exit_is_not_a_leak() {
        let f = func(
            vec![blk(
                0,
                vec![
                    make_arr(0),
                    Instruction::Const { dst: Temp(1), val: crate::ir::Const::Str("boom".into()) },
                    Instruction::Panic { msg: Temp(1) },
                ],
                Terminator::Unreachable,
            )],
            vec![],
            2,
        );
        let mut out = Vec::new();
        verify_fn(&f, &mut out);
        assert!(out.is_empty(), "a value live at a panic/Unreachable exit must not be leak-flagged, got {out:?}");
    }

    /// A fresh owned value that flows through a `Phi` into the returned temp ESCAPES (it becomes the
    /// caller-owned result) and must NOT leak-flag. (Regression for the RAPTOR `getRouteId` /
    /// `reduceReversed` false positives, where a `Box`/`CloneBox` result is Phi-merged into the
    /// function's return value on one arm.)
    #[test]
    fn value_phi_merged_into_return_escapes_no_leak() {
        // block 0: create owned t0; branch.
        // block 1: Box t0 -> t2; jump merge.
        // block 2: jump merge (t0 released on this arm).
        // block 3 (merge): Phi t3 = [(t2, b1), (t0, b2)]; return t3.
        let f = func(
            vec![
                blk(0, vec![make_arr(0)], Terminator::CondJump { cond: Temp(0), then_block: BlockId(1), else_block: BlockId(2) }),
                blk(
                    1,
                    vec![Instruction::Box { dst: Temp(2), val: Temp(0), ty: arr_ty() }],
                    Terminator::Jump(BlockId(3)),
                ),
                blk(2, vec![], Terminator::Jump(BlockId(3))),
                blk(
                    3,
                    vec![Instruction::Phi {
                        dst: Temp(3),
                        ty: arr_ty(),
                        incomings: vec![(Temp(2), BlockId(1)), (Temp(0), BlockId(2))],
                    }],
                    Terminator::Return(Some(Temp(3))),
                ),
            ],
            vec![],
            4,
        );
        let mut out = Vec::new();
        verify_fn(&f, &mut out);
        assert!(out.is_empty(), "a value Phi-merged into the return must not leak-flag, got {out:?}");
    }
}
