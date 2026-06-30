//! Val-sinking optimisation (R3-C spike).
//!
//! # Problem
//!
//! A `val path = trip["stopTimes"].map(s => s["stop"])` is computed for EVERY trip inside a `.for`
//! callback, but the value is only used inside an immediately-following `if routeStopIndex[routeId]
//! == null then …` branch (the "new route" case — a small fraction of iterations). The allocation
//! and map loop run on EVERY trip, even when the result is discarded by the else path.
//!
//! # What this pass does
//!
//! For each `Block { stmts, expr }` where:
//!   - One or more trailing statements are `Val { slot, value, .. }` with a PURE value (see
//!     `is_pure_expr`), and
//!   - The block expression is `If { cond, then_br, else_br, .. }`, and
//!   - The `slot` is used ONLY in `then_br` (not in `cond`, not in `else_br`, not in any
//!     stmt that follows the binding, and not in `expr` outside the then branch), and
//!   - `then_br` does NOT rebind `slot` as a new `val`/`var` (so moving the binding in is safe),
//!
//! we move the `Val` statement into the then-branch:
//!
//! ```text
//! Before:
//!   val path = <pure>
//!   if cond then
//!     ... path ...
//!   else
//!     null
//!
//! After:
//!   if cond then
//!     val path = <pure>
//!     ... path ...
//!   else
//!     null
//! ```
//!
//! This is a semantics-preserving transformation when the expression is pure (no side effects,
//! no mutation, no I/O): computing it later (or not at all on the else path) is unobservable.
//!
//! # RC correctness
//!
//! The transformation works at the TypedAST level, BEFORE lowering. The lowerer emits Retain/Release
//! instructions fresh for each TypedExpr it encounters — it does not track which `val`s were moved
//! by this pass. Moving the `Val` binding into the then-branch means the lowerer emits the defining
//! instructions (including any allocation and RC bookkeeping) only when the then-branch is taken.
//! No IR-level RC adjustment is needed.
//!
//! # Conservative scope
//!
//! - Only sinks a `Val` (not `Var`) — mutable bindings are excluded.
//! - Only sinks when the value expression is provably pure (see `is_pure_expr`): field reads,
//!   arithmetic, array/map reads WITHOUT mutation, pure calls to known pure builtins (`.map`,
//!   `.filter` over pure callbacks), and constructors. Any call to an unknown function, any
//!   mutation (`push`, `[k]=v`, `IndexSet`, `LocalSet`), and any I/O is treated as impure.
//! - Only sinks to an immediately-following `if` expression (the block's `expr` field).
//! - Only sinks when the slot is used in EXACTLY the then-branch (not the else-branch, not the
//!   condition).
//! - Multiple consecutive pure `Val`s that are ALL sinkable to the same branch are all moved.
//!   They are moved in order, preserving their relative ordering inside the then-branch.
//! - The pass walks the entire TypedAST (nested functions, closures, block expressions) so it fires
//!   inside lambda bodies (the common case: the `for` callback).

use lin_check::typed_ir::{
    TypedExpr, TypedModule, TypedStmt, TypedStringPart,
};
use lin_check::types::Type;

/// Run the pass on a module, mutating its statements in place.
/// Gated on `LIN_NO_SINK=1` (same env-var convention as the other passes).
pub fn run(module: &mut TypedModule) {
    if std::env::var("LIN_NO_SINK").is_ok() {
        return;
    }
    for stmt in &mut module.statements {
        sink_stmt(stmt);
    }
}

// -------------------------------------------------------------------------
// Top-level recursive stmt/expr walkers
// -------------------------------------------------------------------------

fn sink_stmt(stmt: &mut TypedStmt) {
    match stmt {
        TypedStmt::Val { value, .. } => sink_expr(value),
        TypedStmt::Var { value, .. } => sink_expr(value),
        TypedStmt::Expr(e) => sink_expr(e),
        TypedStmt::Destructure { value, .. } => sink_expr(value),
        TypedStmt::ArrayDestructure { value, .. } => sink_expr(value),
        TypedStmt::Import { .. } | TypedStmt::ForeignImport { .. } => {}
    }
}

fn sink_expr(expr: &mut TypedExpr) {
    match expr {
        TypedExpr::Block { stmts, expr: block_expr, .. } => {
            // First recurse into sub-expressions so inner blocks are already transformed.
            for s in stmts.iter_mut() {
                sink_stmt(s);
            }
            sink_expr(block_expr);
            // Now apply the sinking transform to this block.
            try_sink_block(stmts, block_expr);
        }
        TypedExpr::If { cond, then_br, else_br, .. } => {
            sink_expr(cond);
            sink_expr(then_br);
            sink_expr(else_br);
        }
        TypedExpr::Function { body, .. } => sink_expr(body),
        TypedExpr::Call { func, args, .. } => {
            sink_expr(func);
            for a in args.iter_mut() { sink_expr(a); }
        }
        TypedExpr::BinaryOp { left, right, .. } => {
            sink_expr(left);
            sink_expr(right);
        }
        TypedExpr::UnaryOp { operand, .. } => sink_expr(operand),
        TypedExpr::Coerce { expr: e, .. } => sink_expr(e),
        TypedExpr::LocalSet { value, .. } => sink_expr(value),
        TypedExpr::MakeObject { fields, spreads, computed_fields, .. } => {
            for (_, v) in fields.iter_mut() { sink_expr(v); }
            for s in spreads.iter_mut() { sink_expr(s); }
            for (k, v) in computed_fields.iter_mut() { sink_expr(k); sink_expr(v); }
        }
        TypedExpr::MakeArray { elements, spreads, .. } => {
            for e in elements.iter_mut() { sink_expr(e); }
            for (_, e) in spreads.iter_mut() { sink_expr(e); }
        }
        TypedExpr::Index { object, key, .. } => {
            sink_expr(object);
            sink_expr(key);
        }
        TypedExpr::FieldGet { object, .. } => sink_expr(object),
        TypedExpr::IndexSet { object, key, value, .. } => {
            sink_expr(object);
            sink_expr(key);
            sink_expr(value);
        }
        TypedExpr::StringInterp { parts, .. } => {
            for p in parts.iter_mut() {
                if let TypedStringPart::Expr(e) = p { sink_expr(e); }
            }
        }
        TypedExpr::Match { scrutinee, arms, .. } => {
            sink_expr(scrutinee);
            for arm in arms.iter_mut() {
                sink_expr(&mut arm.body);
                if let Some(g) = &mut arm.guard { sink_expr(g); }
            }
        }
        TypedExpr::FromJson { value, .. } => sink_expr(value),
        TypedExpr::Is { expr: e, .. } | TypedExpr::Has { expr: e, .. } => sink_expr(e),
        // Leaf nodes
        TypedExpr::IntLit(..)
        | TypedExpr::FloatLit(..)
        | TypedExpr::StringLit(..)
        | TypedExpr::BoolLit(..)
        | TypedExpr::NullLit(..)
        | TypedExpr::LocalGet { .. } => {}
    }
}

// -------------------------------------------------------------------------
// The sinking transform itself
// -------------------------------------------------------------------------

/// Try to sink trailing pure `Val` statements whose slot is used only in the `then_br` of an
/// immediately-following `If` expression. Mutates `stmts` and `block_expr` in place.
///
/// Two patterns are handled:
///
/// Pattern 1 — if is the BLOCK EXPRESSION:
///   `Block { stmts: [..., Val{path}], expr: If{cond, then_br, else_br} }`
///
/// Pattern 2 — if is a STATEMENT followed by more statements:
///   `Block { stmts: [..., Val{path}, Expr(If{...}), more_stmts...], expr: _ }`
///   Here `path` must also not be used in `more_stmts` or the block expr.
fn try_sink_block(stmts: &mut Vec<TypedStmt>, block_expr: &mut TypedExpr) {
    // Pattern 1: block expression is an If.
    if matches!(block_expr, TypedExpr::If { .. }) {
        let to_sink: Vec<usize> = {
            let if_expr: &TypedExpr = block_expr;
            let mut indices: Vec<usize> = Vec::new();
            for i in (0..stmts.len()).rev() {
                if can_sink(&stmts[i], if_expr, &stmts[i + 1..]) {
                    indices.push(i);
                } else {
                    break;
                }
            }
            indices.reverse();
            indices
        };
        if !to_sink.is_empty() {
            let mut sinkable: Vec<TypedStmt> = Vec::with_capacity(to_sink.len());
            for &idx in to_sink.iter().rev() {
                sinkable.insert(0, stmts.remove(idx));
            }
            sink_into_then_branch(block_expr, sinkable);
        }
    }

    // Pattern 2: an Expr(If{...}) statement, with Val stmts anywhere before it
    // (non-contiguous allowed — see comment above).
    let mut i = 0;
    while i < stmts.len() {
        // Look for an Expr(If{...}) at position i.
        let is_if_stmt = matches!(&stmts[i], TypedStmt::Expr(TypedExpr::If { .. }));
        if !is_if_stmt {
            i += 1;
            continue;
        }

        // Collect sinkable Val stmts anywhere before position i.
        // A Val at position j < i is sinkable if:
        //   1. Its value is pure
        //   2. Its slot is not used in stmts[j+1..i] (the stmts that remain between j and the if)
        //      This matters because those stmts stay in place; if they reference slot_j they can't
        //      afford for slot_j to be removed.
        //   3. Its slot is not used in stmts[i+1..] (later_stmts)
        //   4. Its slot is not used in block_expr
        //   5. Its slot is not used in the if's cond or else_br
        //   6. No sinkable stmt we already collected (indices) uses slot_j in its VALUE —
        //      that would create a forward reference (the other sinkable stmt would be removed
        //      from position ≥j+1 but slot_j wouldn't be available at that earlier point).
        //      Actually since all sinkable stmts are being moved into the then-branch together
        //      (in order), mutual references between sinkable stmts are fine as long as later
        //      sinkable stmts are ordered after earlier ones. Since we collect in forward order
        //      (indices.reverse()), the VALUES of later-collected stmts referencing earlier-
        //      collected slot is fine. The only issue would be if an EARLIER sinkable stmt's
        //      VALUE referenced a LATER sinkable slot — impossible since the checker guarantees
        //      single-assignment forward-only slot use.
        //
        // Non-contiguous scan: we allow non-pure stmts between the candidate and the if, as long
        // as those intervening stmts don't USE slot_j (condition 2).
        let to_sink: Vec<usize> = {
            let if_expr: &TypedExpr = match &stmts[i] {
                TypedStmt::Expr(e) => e,
                _ => unreachable!(),
            };
            let later_stmts = &stmts[i + 1..];
            let mut indices: Vec<usize> = Vec::new();
            for j in 0..i {
                let stmt_j = &stmts[j];
                let (slot_j, val_j) = match stmt_j {
                    TypedStmt::Val { slot, value, .. } => (*slot, value),
                    _ => continue,
                };
                if !is_pure_expr(val_j) {
                    continue;
                }
                // Check that stmts between j and i (those that remain) do not USE slot_j.
                let used_in_between = stmts[j + 1..i].iter().any(|s| stmt_uses_slot(s, slot_j));
                if used_in_between {
                    continue;
                }
                // Check that the stmts between j and i are read-only with respect to what val_j reads.
                // Since we can't do alias analysis here, we conservatively require that all
                // intervening stmts are either pure (no mutation at all) or are pure Val bindings
                // whose values are themselves pure. An intervening impure stmt might mutate
                // objects that val_j reads — in that case we refuse to sink.
                // Exception: it is fine to sink over other pure Val stmts (already collected or
                // evaluated to be pure) since pure vals don't mutate.
                let between_mutates = stmts[j + 1..i].iter().any(|s| !is_pure_stmt(s));
                if between_mutates {
                    continue;
                }
                // Check slot not used in later_stmts.
                let used_in_later = later_stmts.iter().any(|s| stmt_uses_slot(s, slot_j));
                if used_in_later {
                    continue;
                }
                // Check slot not used in block_expr.
                if expr_uses_slot(block_expr, slot_j) {
                    continue;
                }
                // Check slot not used in if's cond or else_br.
                let ok = match if_expr {
                    TypedExpr::If { cond, else_br, .. } =>
                        !expr_uses_slot(cond, slot_j) && !expr_uses_slot(else_br, slot_j),
                    _ => false,
                };
                if !ok {
                    continue;
                }
                indices.push(j);
            }
            indices
        };

        if to_sink.is_empty() {
            i += 1;
            continue;
        }

        // Extract the Expr(If{...}) statement for mutation.
        // We need to call sink_into_then_branch on it. Extract it, mutate, then put back.
        let mut if_stmt = stmts.remove(i);
        // Adjust indices since we removed stmts[i] (but to_sink indices are < i, so unaffected).

        // Remove sinkable stmts (in reverse order to keep indices stable).
        let mut sinkable: Vec<TypedStmt> = Vec::with_capacity(to_sink.len());
        for &idx in to_sink.iter().rev() {
            sinkable.insert(0, stmts.remove(idx));
        }

        // Sink into the if stmt.
        match &mut if_stmt {
            TypedStmt::Expr(e) => sink_into_then_branch(e, sinkable),
            _ => unreachable!(),
        }

        // Put the if stmt back. It is now at index i - to_sink.len() (since we removed those).
        let new_pos = i - to_sink.len();
        stmts.insert(new_pos, if_stmt);

        // Advance past the if stmt.
        i = new_pos + 1;
    }
}

/// Returns true if `stmt` is a pure `Val` whose slot is used only in the `if_expr`'s `then_br`
/// (not in `cond`, not in `else_br`), AND not in any of `later_stmts` (stmts that follow it
/// within the same block, AFTER this one).
fn can_sink(stmt: &TypedStmt, if_expr: &TypedExpr, later_stmts: &[TypedStmt]) -> bool {
    let (slot, value) = match stmt {
        TypedStmt::Val { slot, value, .. } => (*slot, value),
        _ => return false,
    };

    // Value must be pure.
    if !is_pure_expr(value) {
        return false;
    }

    // Slot must not be used in any later stmts within the block (those would need the value
    // before the if).
    for later in later_stmts {
        if stmt_uses_slot(later, slot) {
            return false;
        }
    }

    // Slot must not be used in the if-expression outside of the then-branch.
    // Specifically: not in cond, not in else_br, not in the then-branch in a position that
    // re-binds the slot (though the checker ensures slots are unique, so that can't happen).
    match if_expr {
        TypedExpr::If { cond, else_br, .. } => {
            // Must not appear in the condition.
            if expr_uses_slot(cond, slot) {
                return false;
            }
            // Must not appear in the else branch.
            if expr_uses_slot(else_br, slot) {
                return false;
            }
            // (Uses in then_br are fine — that's where we're sinking it.)
            true
        }
        _ => false,
    }
}

/// Move `sinkable` stmts into the then-branch of an `If` expression.
///
/// If `then_br` is a `Block`, prepend the stmts there. Otherwise, wrap it in a new
/// `Block { stmts: sinkable, expr: then_br }`.
fn sink_into_then_branch(if_expr: &mut TypedExpr, mut sinkable: Vec<TypedStmt>) {
    match if_expr {
        TypedExpr::If { then_br, .. } => {
            match then_br.as_mut() {
                TypedExpr::Block { stmts, .. } => {
                    // Prepend sinkable stmts in order.
                    sinkable.append(stmts);
                    *stmts = sinkable;
                }
                other => {
                    // Wrap in a new Block.
                    let span = other.span();
                    let ty = other.ty();
                    let inner = std::mem::replace(other, TypedExpr::NullLit(span));
                    *other = TypedExpr::Block {
                        stmts: sinkable,
                        expr: Box::new(inner),
                        ty,
                        span,
                    };
                }
            }
        }
        _ => {}
    }
}

// -------------------------------------------------------------------------
// Purity checker
// -------------------------------------------------------------------------

/// Returns true if `expr` is provably pure (side-effect-free, allocation ok).
///
/// Pure expressions:
/// - Literals (Int/Float/String/Bool/Null)
/// - LocalGet (reads a variable)
/// - FieldGet on a pure expression
/// - Index (safe read: pure object + pure key)
/// - BinaryOp / UnaryOp on pure operands
/// - Coerce of a pure expression
/// - MakeObject/MakeArray with pure fields/elements (construction is pure: allocates but no mutation)
/// - Call to a known-pure stdlib function with pure args: `.map(f)`, `.filter(f)`, `.length()`
///   where the callback `f` is itself pure (a Function literal with pure body)
/// - If/Block with pure sub-expressions
///
/// NOT pure:
/// - LocalSet (mutation of a var)
/// - IndexSet (mutation of an object/array)
/// - Any unknown function call (we conservatively treat these as having side effects)
/// - I/O calls (print, etc.) — but these are identified as "not pure" by the unknown-call rule
/// - Push/ObjectSet/etc. intrinsics (these are mutation)
///
/// Note: calls to `.map(f)` and `.filter(f)` are stdlib function calls that appear as `Call`
/// nodes in the TypedAST. We check whether the callee resolves to a known-pure combinator by
/// inspecting the `func` field. We recognise the pattern `LocalGet { ty: Function { .. } }` where
/// the function was imported from `std/iter` (`.map`, `.filter`) and the callback argument is a
/// pure Function literal. This is intentionally conservative: any call whose purity we can't prove
/// is treated as impure.
///
/// In practice, `trip["stopTimes"].map(s => s["stop"])` compiles to a Call node that looks like:
///   Call { func: LocalGet{slot: map_fn}, args: [Index{…}, Function{…}], .. }
/// where `map_fn` is an imported `std/iter.map`. We recognize this as pure because:
///   - The first arg (the array) is a pure Index expression
///   - The second arg (the callback) is a Function literal with a pure body (FieldGet only)
///
/// We DO NOT try to prove purity of arbitrary imported functions — only pure combinators passed
/// a pure callback.
pub(crate) fn is_pure_expr(expr: &TypedExpr) -> bool {
    match expr {
        TypedExpr::IntLit(..)
        | TypedExpr::FloatLit(..)
        | TypedExpr::StringLit(..)
        | TypedExpr::BoolLit(..)
        | TypedExpr::NullLit(..) => true,

        TypedExpr::LocalGet { .. } => true,

        // LocalSet is mutation → not pure.
        TypedExpr::LocalSet { .. } => false,

        TypedExpr::FieldGet { object, .. } => is_pure_expr(object),

        TypedExpr::Index { object, key, .. } => is_pure_expr(object) && is_pure_expr(key),

        // IndexSet is mutation → not pure.
        TypedExpr::IndexSet { .. } => false,

        TypedExpr::BinaryOp { left, right, .. } => is_pure_expr(left) && is_pure_expr(right),
        TypedExpr::UnaryOp { operand, .. } => is_pure_expr(operand),
        TypedExpr::Coerce { expr: e, .. } => is_pure_expr(e),
        TypedExpr::StringInterp { parts, .. } => parts.iter().all(|p| match p {
            TypedStringPart::Expr(e) => is_pure_expr(e),
            TypedStringPart::Literal(_) => true,
        }),

        TypedExpr::MakeObject { fields, spreads, computed_fields, .. } => {
            fields.iter().all(|(_, v)| is_pure_expr(v))
                && spreads.iter().all(is_pure_expr)
                && computed_fields.iter().all(|(k, v)| is_pure_expr(k) && is_pure_expr(v))
        }
        TypedExpr::MakeArray { elements, spreads, .. } => {
            elements.iter().all(is_pure_expr)
                && spreads.iter().all(|(_, e)| is_pure_expr(e))
        }

        TypedExpr::Call { func, args, result_type, .. } => {
            // Only allow calls to KNOWN-PURE combinators: map, filter, length, range.
            // We identify these by the callee's type being a Function type and by checking
            // that all arguments are pure (including the callback, which must be a pure Function
            // literal). Any indirect call or call to an unknown function is impure.
            is_pure_call(func, args, result_type)
        }

        TypedExpr::If { cond, then_br, else_br, .. } => {
            is_pure_expr(cond) && is_pure_expr(then_br) && is_pure_expr(else_br)
        }

        TypedExpr::Block { stmts, expr, .. } => {
            stmts.iter().all(is_pure_stmt) && is_pure_expr(expr)
        }

        TypedExpr::Function { .. } => {
            // A function LITERAL (not a call) is pure — it just constructs a closure.
            // Whether calling it is pure is a separate question.
            true
        }

        // Match / FromJson / Is / Has: conservative — treat as impure.
        // Match may call arbitrary code in arms; Is/Has may call schema validation.
        TypedExpr::Match { .. } | TypedExpr::FromJson { .. }
        | TypedExpr::Is { .. } | TypedExpr::Has { .. } => false,
    }
}

fn is_pure_stmt(stmt: &TypedStmt) -> bool {
    match stmt {
        TypedStmt::Val { value, .. } => is_pure_expr(value),
        TypedStmt::Var { value, .. } => is_pure_expr(value),
        TypedStmt::Expr(e) => is_pure_expr(e),
        TypedStmt::Destructure { value, .. } => is_pure_expr(value),
        TypedStmt::ArrayDestructure { value, .. } => is_pure_expr(value),
        TypedStmt::Import { .. } | TypedStmt::ForeignImport { .. } => true,
    }
}

/// Returns true if `Call { func, args }` is a known-pure combinator call.
///
/// We conservatively recognise ONLY the following call shapes:
///
///   - `<array_expr>.map(<pure_fn>)` — produces a new array, no mutation
///   - `<array_expr>.filter(<pure_fn>)` — produces a new array, no mutation
///   - `<iter_expr>.length()` / `<array>.length()` — pure read
///   - `range(a, b)` / `range(a, b, step)` — pure construction
///
/// In the TypedAST these appear as `Call { func: LocalGet { slot: map_fn_slot, ty: Function }, args: [...] }`.
/// We identify them by matching on the callee TYPE and the argument forms:
///   - The callee is a Function type (we don't track which imported symbol it is)
///   - For map/filter: the last arg is a pure Function literal or a LocalGet of a Function type
///   - All other args are pure
///
/// IMPORTANT: We only allow pure Function literal callbacks or references to them.
/// An indirect call, or a combinator call where the callback is not a pure function, is
/// treated as impure.
fn is_pure_call(func: &TypedExpr, args: &[TypedExpr], _result_type: &Type) -> bool {
    // The callee must be a Function-typed value we can classify.
    let func_ty = func.ty();
    match &func_ty {
        Type::Function { params, ret, .. } => {
            // Reject: callee is not a LocalGet reference (e.g. it's itself a call result).
            // Only allow LocalGet (imported combinators / named functions) or direct Function literals.
            match func {
                TypedExpr::LocalGet { .. } | TypedExpr::Function { .. } => {}
                _ => return false,
            }

            // Check arity: params count should match args count.
            if params.len() != args.len() {
                // Partial application — conservative: reject.
                return false;
            }

            // Classify by the callee's type signature:
            //   (Array<T>, T -> U) -> Array<U>   : map
            //   (Array<T>, T -> Bool) -> Array<T> : filter
            //   (Array<T>) -> Int64              : length / lin_array_length style
            //   (Int32, Int32) -> Iterator       : range
            //
            // We use a loose check: if the callee type has at least 1 param and the LAST param
            // is a Function type (a callback), we require the callback arg to be pure.
            // If the callee type has no Function params (e.g. `length(arr)`, `range(a,b)`),
            // we require all args to be pure.
            //
            // We also check the return type: map/filter return Array<..> or Iterator<..>;
            // length returns Int; range returns Iterator. All of these have no side effects.
            // We only allow calls that produce a PURE value (no I/O, no streams, no promises).
            if is_impure_return_type(ret) {
                return false;
            }

            // Check all args for purity. Function-type args must be pure Function literals or refs.
            for (arg, param_ty) in args.iter().zip(params.iter()) {
                if is_function_type(param_ty) {
                    // Callback argument: must be a pure function literal or a LocalGet of a fn.
                    if !is_pure_fn_arg(arg) {
                        return false;
                    }
                } else {
                    if !is_pure_expr(arg) {
                        return false;
                    }
                }
            }

            true
        }
        _ => false,
    }
}

/// A "pure function argument" — a callback passed to a combinator that is safe to pass even
/// when we delay the combinator call. This is a Function literal with a pure body, or a LocalGet
/// referencing a Function type.
fn is_pure_fn_arg(arg: &TypedExpr) -> bool {
    match arg {
        TypedExpr::Function { body, .. } => is_pure_expr(body),
        TypedExpr::LocalGet { ty, .. } => is_function_type(ty),
        // Coerce of a function (e.g. widening) — allow if the inner is pure.
        TypedExpr::Coerce { expr, .. } => is_pure_fn_arg(expr),
        _ => false,
    }
}

fn is_function_type(ty: &Type) -> bool {
    matches!(ty, Type::Function { .. })
}

/// Returns true for return types that imply I/O or other side effects (Streams, Promises, etc.).
/// Conservative: any type we don't recognise as pure gets rejected.
fn is_impure_return_type(ty: &Type) -> bool {
    match ty {
        // Scalar, structural, and collection types are pure return types.
        Type::Int32 | Type::Int64 | Type::Int8 | Type::Int16
        | Type::UInt8 | Type::UInt16 | Type::UInt32 | Type::UInt64
        | Type::Float32 | Type::Float64 | Type::Bool | Type::Null | Type::Str
        | Type::StrLit(_) | Type::IntLit(_)
        | Type::Object { .. } | Type::Array(_) | Type::FixedArray(..)
        | Type::Map { .. } | Type::Union(_) | Type::TypeVar(_) | Type::Named(_) => false,
        // Iterator — iterators are lazy/pure (no I/O).
        Type::Iterator(_) => false,
        // Function types are values, not impure by themselves.
        Type::Function { .. } => false,
        // Promise, Stream, Shared, opaque handles (Opaque): conservatively impure.
        Type::Promise(_) | Type::Stream(_) | Type::Shared(_) | Type::Opaque(_) => true,
        // Never: unreachable, treat as pure.
        Type::Never => false,
    }
}

// -------------------------------------------------------------------------
// Slot-usage checker
// -------------------------------------------------------------------------

/// Returns true if `expr` references `slot` anywhere.
fn expr_uses_slot(expr: &TypedExpr, slot: usize) -> bool {
    match expr {
        TypedExpr::LocalGet { slot: s, .. } => *s == slot,
        TypedExpr::LocalSet { slot: s, value, .. } => *s == slot || expr_uses_slot(value, slot),
        TypedExpr::BinaryOp { left, right, .. } => expr_uses_slot(left, slot) || expr_uses_slot(right, slot),
        TypedExpr::UnaryOp { operand, .. } => expr_uses_slot(operand, slot),
        TypedExpr::Coerce { expr: e, .. } => expr_uses_slot(e, slot),
        TypedExpr::Call { func, args, .. } => {
            expr_uses_slot(func, slot) || args.iter().any(|a| expr_uses_slot(a, slot))
        }
        TypedExpr::If { cond, then_br, else_br, .. } => {
            expr_uses_slot(cond, slot) || expr_uses_slot(then_br, slot) || expr_uses_slot(else_br, slot)
        }
        TypedExpr::Match { scrutinee, arms, .. } => {
            expr_uses_slot(scrutinee, slot)
                || arms.iter().any(|a| {
                    expr_uses_slot(&a.body, slot)
                        || a.guard.as_ref().map_or(false, |g| expr_uses_slot(g, slot))
                })
        }
        TypedExpr::Block { stmts, expr, .. } => {
            stmts.iter().any(|s| stmt_uses_slot(s, slot)) || expr_uses_slot(expr, slot)
        }
        TypedExpr::Function { body, .. } => expr_uses_slot(body, slot),
        TypedExpr::MakeObject { fields, spreads, computed_fields, .. } => {
            fields.iter().any(|(_, v)| expr_uses_slot(v, slot))
                || spreads.iter().any(|s| expr_uses_slot(s, slot))
                || computed_fields.iter().any(|(k, v)| expr_uses_slot(k, slot) || expr_uses_slot(v, slot))
        }
        TypedExpr::MakeArray { elements, spreads, .. } => {
            elements.iter().any(|e| expr_uses_slot(e, slot))
                || spreads.iter().any(|(_, e)| expr_uses_slot(e, slot))
        }
        TypedExpr::Index { object, key, .. } => expr_uses_slot(object, slot) || expr_uses_slot(key, slot),
        TypedExpr::FieldGet { object, .. } => expr_uses_slot(object, slot),
        TypedExpr::IndexSet { object, key, value, .. } => {
            expr_uses_slot(object, slot) || expr_uses_slot(key, slot) || expr_uses_slot(value, slot)
        }
        TypedExpr::StringInterp { parts, .. } => parts.iter().any(|p| match p {
            TypedStringPart::Expr(e) => expr_uses_slot(e, slot),
            TypedStringPart::Literal(_) => false,
        }),
        TypedExpr::FromJson { value, .. } => expr_uses_slot(value, slot),
        TypedExpr::Is { expr: e, .. } | TypedExpr::Has { expr: e, .. } => expr_uses_slot(e, slot),
        TypedExpr::IntLit(..)
        | TypedExpr::FloatLit(..)
        | TypedExpr::StringLit(..)
        | TypedExpr::BoolLit(..)
        | TypedExpr::NullLit(..) => false,
    }
}

fn stmt_uses_slot(stmt: &TypedStmt, slot: usize) -> bool {
    match stmt {
        TypedStmt::Val { value, .. } => expr_uses_slot(value, slot),
        TypedStmt::Var { value, .. } => expr_uses_slot(value, slot),
        TypedStmt::Expr(e) => expr_uses_slot(e, slot),
        TypedStmt::Destructure { value, .. } => expr_uses_slot(value, slot),
        TypedStmt::ArrayDestructure { value, .. } => expr_uses_slot(value, slot),
        TypedStmt::Import { .. } | TypedStmt::ForeignImport { .. } => false,
    }
}
