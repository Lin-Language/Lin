//! Path-11 Leg 2, Stage 1 — **shadow inference + measurement** for lambda-set specialization.
//!
//! This module is pure measurement: it walks a checked `TypedModule`, finds every *closure call
//! site*, reads the `LambdaSet` the checker inferred for the function value at that site, and
//! classifies it. It changes NO types and emits NO IR — it only produces a distribution that
//! gates whether the Wave-2 lowering work (direct-call / unboxed-env specialization) is worth
//! building. Per the proposal: "Payoff validated before any lowering work."
//!
//! ## What is a "closure call site"?
//! Two disjoint populations, reported separately:
//!
//!   1. **Callback arguments** — a function-typed ARGUMENT passed to a call (`.map(f)`,
//!      `.filter(x => ...)`, `find(xs, pred)`). This is the Path-11 lever: the callback is invoked
//!      indirectly from inside the (often generic stdlib) higher-order callee, and a *singleton*
//!      here is exactly what lets the callee be specialized to a direct call. The frontier memo
//!      predicts these are overwhelmingly singleton.
//!   2. **Indirect callee invocations** — a call whose CALLEE is a function *value* held in a
//!      local/field/index/call-result rather than a bare lambda literal (`val h = pick(); h(x)`).
//!      Reported for completeness; named top-level calls are already direct (Path 8), so they are
//!      excluded by construction (a bare-lambda callee is folded immediately, not counted).
//!
//! ## Classification
//!   - `Singleton` — `Known` set of exactly one lambda id. Directly specializable.
//!   - `SmallSet`  — `Known` set of 2..=`SMALL_SET_MAX` ids. Switch-dispatchable.
//!   - `Top`       — `LambdaSet::Top`, or a `Known` set wider than the threshold. Unknowable /
//!     FFI / `Json`-stored / param-typed / recursion-knot — keeps today's boxed ABI.
//!
//! Gated entirely by the `LIN_LAMBDA_STATS` env var (zero cost when unset): the walk only runs when
//! a caller asks for it, and the caller only asks when the var is set.

use crate::typed_ir::*;
use crate::types::{LambdaSet, Type};

/// Small-set dispatch threshold (Roc uses ~8). A `Known` set wider than this classifies as `Top`.
pub const SMALL_SET_MAX: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallSiteClass {
    Singleton,
    SmallSet,
    Top,
}

/// Classify a lambda set into a call-site class.
pub fn classify(lset: &LambdaSet) -> CallSiteClass {
    match lset {
        LambdaSet::Top => CallSiteClass::Top,
        LambdaSet::Known(ids) => match ids.len() {
            1 => CallSiteClass::Singleton,
            2..=SMALL_SET_MAX => CallSiteClass::SmallSet,
            // 0 (never produced in practice) or > SMALL_SET_MAX: not usefully specializable.
            _ => CallSiteClass::Top,
        },
    }
}

/// A counted distribution over the three classes. Two of these are kept (callback args vs indirect
/// callees) and a grand total is derived.
#[derive(Debug, Default, Clone)]
pub struct Dist {
    pub singleton: u64,
    pub small_set: u64,
    pub top: u64,
    /// Of `top`: the subset whose callee/argument is a CONCRETE function type held in a bound
    /// variable or parameter (`LocalGet`) — i.e. ⊤ *only* because the enclosing higher-order
    /// function has not yet been specialized to the call-site's set. Lambda-set specialization
    /// (Wave 2) recovers these: the set flows from the call site into the specialized copy, so the
    /// param's ⊤ becomes the caller's singleton. This is the headline "ceiling" figure.
    pub top_recoverable: u64,
    /// Of `top`: genuinely dynamic — the callee is `Json`/union-typed, FFI, or an otherwise
    /// unknowable source. These keep the boxed ABI permanently. (Note: a call THROUGH a `Json`
    /// value has type `Json`, not `Function`, so it lands here only when the static type is a
    /// concrete `Function` with no recoverable binding evidence.)
    pub top_dynamic: u64,
}

impl Dist {
    pub fn total(&self) -> u64 {
        self.singleton + self.small_set + self.top
    }
    fn bump_kind(&mut self, c: CallSiteClass, recoverable: bool) {
        match c {
            CallSiteClass::Singleton => self.singleton += 1,
            CallSiteClass::SmallSet => self.small_set += 1,
            CallSiteClass::Top => {
                self.top += 1;
                if recoverable {
                    self.top_recoverable += 1;
                } else {
                    self.top_dynamic += 1;
                }
            }
        }
    }
    fn add(&mut self, other: &Dist) {
        self.singleton += other.singleton;
        self.small_set += other.small_set;
        self.top += other.top;
        self.top_recoverable += other.top_recoverable;
        self.top_dynamic += other.top_dynamic;
    }
    fn pct(n: u64, total: u64) -> f64 {
        if total == 0 { 0.0 } else { (n as f64) * 100.0 / (total as f64) }
    }
    /// Render `"78.1% singleton, 14.9% small-set, 7.0% top [5.0% recoverable] (n=1234)"`.
    pub fn summary(&self) -> String {
        let t = self.total();
        format!(
            "{:.1}% singleton, {:.1}% small-set, {:.1}% top [{:.1}% recoverable / {:.1}% dynamic] (n={})",
            Self::pct(self.singleton, t),
            Self::pct(self.small_set, t),
            Self::pct(self.top, t),
            Self::pct(self.top_recoverable, t),
            Self::pct(self.top_dynamic, t),
            t
        )
    }
}

#[derive(Debug, Default, Clone)]
pub struct Stats {
    /// Function-typed arguments passed to calls (the Path-11 callback lever).
    pub callback_args: Dist,
    /// Indirect callee invocations (callee is a function VALUE, not a bare lambda literal).
    pub indirect_callees: Dist,
}

impl Stats {
    pub fn add(&mut self, other: &Stats) {
        self.callback_args.add(&other.callback_args);
        self.indirect_callees.add(&other.indirect_callees);
    }

    /// The combined "all closure call sites" distribution (callbacks + indirect callees) — the
    /// headline population the proposal asks for.
    pub fn combined(&self) -> Dist {
        let mut d = self.callback_args.clone();
        d.add(&self.indirect_callees);
        d
    }
}

/// Collect lambda-set statistics over a whole checked module.
pub fn collect_module(module: &TypedModule) -> Stats {
    let mut s = Stats::default();
    for stmt in &module.statements {
        walk_stmt(stmt, &mut s);
    }
    // Mock-replacement bodies (test files) also contain real call sites.
    for r in &module.replacements {
        walk_expr(&r.value, &mut s);
    }
    s
}

fn walk_stmt(stmt: &TypedStmt, s: &mut Stats) {
    match stmt {
        TypedStmt::Val { value, .. } | TypedStmt::Var { value, .. } => walk_expr(value, s),
        TypedStmt::Destructure { value, .. } | TypedStmt::ArrayDestructure { value, .. } => {
            walk_expr(value, s)
        }
        TypedStmt::Expr(e) => walk_expr(e, s),
        TypedStmt::Import { .. } | TypedStmt::ForeignImport { .. } => {}
    }
}

/// Is this callee expression a bare lambda literal being applied directly (`(x => x)(3)`)? Such a
/// call is NOT an indirect closure invocation — it is folded directly — so it is not counted as a
/// callee site (its body is still walked).
fn is_literal_callee(func: &TypedExpr) -> bool {
    matches!(func, TypedExpr::Function { .. })
}

/// For a ⊤-classified site, is the ⊤ RECOVERABLE by lambda-set specialization? True when the
/// function value comes from a `LocalGet` (a parameter or `val`/`var` binding): specialization
/// pushes the concrete set from the call site into the specialized callee copy, turning the param's
/// ⊤ into the caller's known set. False for function values pulled out of a field/index/call result
/// or any other opaque source — those have no single binding identity to specialize through, so
/// they keep the boxed ABI. A `LocalGet` whose set is already a singleton never reaches here (it is
/// classified `Singleton`); this only fires on the ⊤ residue, which is dominated by generic stdlib
/// higher-order params (`map`'s `f`, `reduce`'s `acc`).
fn is_recoverable_top(func: &TypedExpr) -> bool {
    matches!(func, TypedExpr::LocalGet { .. })
}

fn walk_expr(expr: &TypedExpr, s: &mut Stats) {
    match expr {
        TypedExpr::Call { func, args, .. } => {
            // (1) Indirect callee invocation: the callee is a function VALUE (not a bare lambda
            // literal). A `LocalGet`/`FieldGet`/`Index`/`Call`-result of function type is a real
            // indirect dispatch. (A bare-lambda callee is folded — excluded.)
            if !is_literal_callee(func) {
                if let Type::Function { lset, .. } = func.ty() {
                    let c = classify(&lset);
                    s.indirect_callees.bump_kind(c, is_recoverable_top(func));
                }
            }
            // (2) Callback arguments: every function-typed argument is a closure flowing into a
            // higher-order callee — the population Path-11 specializes.
            for a in args {
                if let Type::Function { lset, .. } = a.ty() {
                    let c = classify(&lset);
                    s.callback_args.bump_kind(c, is_recoverable_top(a));
                }
            }
            walk_expr(func, s);
            for a in args {
                walk_expr(a, s);
            }
        }
        TypedExpr::Function { body, .. } => walk_expr(body, s),
        TypedExpr::If { cond, then_br, else_br, .. } => {
            walk_expr(cond, s);
            walk_expr(then_br, s);
            walk_expr(else_br, s);
        }
        TypedExpr::Match { scrutinee, arms, .. } => {
            walk_expr(scrutinee, s);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    walk_expr(g, s);
                }
                walk_expr(&arm.body, s);
            }
        }
        TypedExpr::Block { stmts, expr, .. } => {
            for st in stmts {
                walk_stmt(st, s);
            }
            walk_expr(expr, s);
        }
        TypedExpr::BinaryOp { left, right, .. } => {
            walk_expr(left, s);
            walk_expr(right, s);
        }
        TypedExpr::UnaryOp { operand, .. } => walk_expr(operand, s),
        TypedExpr::Coerce { expr, .. } => walk_expr(expr, s),
        TypedExpr::LocalSet { value, .. } => walk_expr(value, s),
        TypedExpr::FromJson { value, .. } => walk_expr(value, s),
        TypedExpr::MakeObject { fields, spreads, .. } => {
            for (_, e) in fields {
                walk_expr(e, s);
            }
            for sp in spreads {
                walk_expr(sp, s);
            }
        }
        TypedExpr::MakeArray { elements, .. } => {
            for e in elements {
                walk_expr(e, s);
            }
        }
        TypedExpr::Index { object, key, .. } => {
            walk_expr(object, s);
            walk_expr(key, s);
        }
        TypedExpr::FieldGet { object, .. } => walk_expr(object, s),
        TypedExpr::IndexSet { object, key, value, .. } => {
            walk_expr(object, s);
            walk_expr(key, s);
            walk_expr(value, s);
        }
        TypedExpr::StringInterp { parts, .. } => {
            for p in parts {
                if let TypedStringPart::Expr(e) = p {
                    walk_expr(e, s);
                }
            }
        }
        TypedExpr::Is { expr, .. } | TypedExpr::Has { expr, .. } => walk_expr(expr, s),
        // Leaves.
        TypedExpr::IntLit(..)
        | TypedExpr::FloatLit(..)
        | TypedExpr::StringLit(..)
        | TypedExpr::BoolLit(..)
        | TypedExpr::NullLit(..)
        | TypedExpr::LocalGet { .. } => {}
    }
}

/// True when lambda-set statistics collection is requested (the `LIN_LAMBDA_STATS` env var is set
/// to a non-empty value). Cheap; called once per compile.
pub fn enabled() -> bool {
    std::env::var_os("LIN_LAMBDA_STATS").is_some_and(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LambdaSet;

    #[test]
    fn classify_singleton_small_top() {
        assert_eq!(classify(&LambdaSet::singleton(7)), CallSiteClass::Singleton);
        assert_eq!(classify(&LambdaSet::Known(vec![1, 2])), CallSiteClass::SmallSet);
        assert_eq!(classify(&LambdaSet::Known(vec![1, 2, 3, 4, 5, 6, 7, 8])), CallSiteClass::SmallSet);
        // One past the small-set threshold → Top.
        assert_eq!(classify(&LambdaSet::Known((1..=9).collect())), CallSiteClass::Top);
        assert_eq!(classify(&LambdaSet::Top), CallSiteClass::Top);
    }

    #[test]
    fn join_unions_and_saturates() {
        // singleton ∪ singleton = 2-set
        let s = LambdaSet::singleton(1).join(&LambdaSet::singleton(2));
        assert_eq!(classify(&s), CallSiteClass::SmallSet);
        // joining the same id stays singleton
        let s = LambdaSet::singleton(1).join(&LambdaSet::singleton(1));
        assert_eq!(classify(&s), CallSiteClass::Singleton);
        // Top is absorbing
        assert!(matches!(LambdaSet::singleton(1).join(&LambdaSet::Top), LambdaSet::Top));
        // overflow past MAX_KNOWN collapses to Top
        let wide = LambdaSet::Known((1..=LambdaSet::MAX_KNOWN as u32).collect());
        let overflow = wide.join(&LambdaSet::singleton(9999));
        assert!(matches!(overflow, LambdaSet::Top));
    }

    #[test]
    fn dist_summary_percentages() {
        let mut d = Dist::default();
        d.bump_kind(CallSiteClass::Singleton, false);
        d.bump_kind(CallSiteClass::Singleton, false);
        d.bump_kind(CallSiteClass::SmallSet, false);
        d.bump_kind(CallSiteClass::Top, true);
        assert_eq!(d.total(), 4);
        assert_eq!(d.singleton, 2);
        assert_eq!(d.top_recoverable, 1);
        assert!(d.summary().contains("50.0% singleton"));
    }
}
