use indexmap::IndexMap;
use lin_common::{Diagnostic, Span};
use lin_parse::ast::{BinOp, Expr, MatchArm, ObjectField, Stmt, StringPart};

use super::Checker;
use super::helpers::{check_int_literal_fits, default_int_literal_type, suffix_to_type, unify_types};
use crate::resolve::resolve_type;
use crate::typed_ir::*;
use crate::types::Type;

/// The "place" a flow-narrowing applies to: either a simple identifier (`x`) or an index read
/// (`base[key]…`) given as a `PlacePath`. Index places are the `{String:T}`-map idiom
/// `if m[k] != null then m[k]`, and its nested form `if obj["a"][k] != null then obj["a"][k]`.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum NarrowPlace {
    Ident(String),
    /// An index read `base[key]…`, canonicalized as a `PlacePath` whose outermost step is an
    /// `Index` (a bare `Root` is the `Ident` variant instead).
    Index(PlacePath),
}

/// A canonical, stably-re-readable place expression: an identifier root with zero or more index
/// steps. Every step's key is a `StrLit` or a simple `Ident`, and the root is an identifier, so
/// two syntactic reads that canonicalize to the same `PlacePath` are guaranteed to denote the same
/// slot — provided no identifier the path mentions is reassigned and no write lands through any
/// prefix (both enforced by `clear_index_narrowings_for` / the index-assign invalidation).
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum PlacePath {
    /// A simple identifier base (`x`).
    Root(String),
    /// One index step over a base path (`base[key]`).
    Index(Box<PlacePath>, IndexKey),
}

/// The key half of a narrowable index step. Only stable, side-effect-free keys are admitted so
/// that two reads of `base[key]` are guaranteed to denote the same slot.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum IndexKey {
    /// A string literal key (`m["foo"]`).
    StrLit(String),
    /// A simple identifier key (`m[k]`). Sound only while `k` is not reassigned in the branch.
    Ident(String),
}

/// An active index-place narrowing recorded on `Checker::index_narrowings`. `infer_index`
/// consults the stack: a read whose canonical `PlacePath` equals `path` is tightened to `ty`.
#[derive(Clone)]
pub(crate) struct IndexNarrow {
    pub(crate) path: PlacePath,
    pub(crate) ty: Type,
}

/// Canonicalize a place expression into a `PlacePath` if it is stably re-readable — an identifier
/// root with index steps whose keys are each a string-literal or a simple identifier. Otherwise
/// `None` (a call, arithmetic, etc. in the path is not guaranteed to denote the same slot twice).
fn place_path_of_expr(expr: &Expr) -> Option<PlacePath> {
    match expr {
        Expr::Ident(n, _) => Some(PlacePath::Root(n.clone())),
        Expr::Index { object, key, .. } => {
            let base = place_path_of_expr(object)?;
            let k = match key.as_ref() {
                Expr::StringLit(s, _) => IndexKey::StrLit(s.clone()),
                Expr::Ident(k, _) => IndexKey::Ident(k.clone()),
                _ => return None,
            };
            Some(PlacePath::Index(Box::new(base), k))
        }
        _ => None,
    }
}

/// True if `name` appears anywhere in `path` — as the root or as an identifier key. Reassigning
/// such an identifier invalidates the narrowing (the path may denote a different slot/value).
fn place_path_mentions(path: &PlacePath, name: &str) -> bool {
    match path {
        PlacePath::Root(n) => n == name,
        PlacePath::Index(base, key) => {
            place_path_mentions(base, name) || matches!(key, IndexKey::Ident(k) if k == name)
        }
    }
}

/// The root identifier of a place EXPRESSION (the object of an index-assign target), peeling index
/// steps. Used to invalidate narrowings rooted at the same identifier after a write through it.
fn expr_place_root(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Ident(n, _) => Some(n),
        Expr::Index { object, .. } => expr_place_root(object),
        _ => None,
    }
}

/// A flow-narrowing derived from an `if`/`else` condition that is a type/null test on a simple
/// identifier OR an index place (`m[k]`). Carries the place's narrowed static type for each
/// branch (or `None` when that branch does not tighten the type). See `Checker::null_test_narrowing`.
pub(crate) struct NarrowTest {
    place: NarrowPlace,
    then_ty: Option<Type>,
    else_ty: Option<Type>,
}

impl Checker {
    pub(crate) fn check_expr(&mut self, expr: &Expr, expected: &Type) -> Result<TypedExpr, Diagnostic> {
        // For function expressions with a known expected function type, use the expected
        // param types to guide inference (bidirectional type checking).
        if let (Expr::Function { type_params, params, return_type, body, span, full_span: _ }, Type::Function { params: expected_params, ret: expected_ret, .. }) = (expr, expected) {
            return self.infer_function_with_hints(type_params, params, return_type, body, *span, None, expected_params, expected_ret);
        }

        // Integer literal against an expected type. A suffixless literal takes its context
        // type (spec §21), re-typed at that width if the value fits (else a compile error,
        // not a silent truncation). A *suffixed* literal pins its own type (spec §2.6) — it
        // falls through to `infer_expr` below, and the tail's compatibility check verifies it
        // against `expected` like any other typed expression.
        if let Expr::IntLit(v, None, span) = expr {
            if expected.is_integer() {
                check_int_literal_fits(*v, expected, *span)?;
                return Ok(TypedExpr::IntLit(*v, expected.clone(), *span));
            }
            // Integer-literal singleton refinement (mirrors ADR-034 for StrLit). A bare integer
            // literal narrows to `IntLit(n)` when the expected type is exactly `IntLit(n)` with the
            // same value, or when it is a closed union of `IntLit` values containing `n`.
            //
            // When the expected type is a single IntLit:
            if let Type::IntLit(t) = expected {
                if v == t {
                    return Ok(TypedExpr::IntLit(*v, Type::IntLit(*t), *span));
                }
                return Err(Diagnostic::error(
                    *span,
                    format!("Expected literal type {}, got {}", t, v),
                ));
            }
            // When the expected type is a closed union of IntLit values:
            if let Some(members) = self.closed_int_literal_keys(expected) {
                if members.iter().any(|m| *m == *v) {
                    return Ok(TypedExpr::IntLit(*v, Type::IntLit(*v), *span));
                }
                return Err(Diagnostic::error(
                    *span,
                    format!("Expected type {}, got {}", expected, v),
                ));
            }
        }

        // Float literal against an expected `Float32`. A suffixless float literal infers to
        // `Float64` by default (spec §21), but when the context type is precisely `Float32`
        // it adopts that width — mirroring the suffixless integer-literal rule above. A
        // *suffixed* float literal pins its own type and falls through to `infer_expr`. The
        // default (no expected type, or expected `Float64`/`Number`) is unchanged: only an
        // exact `Float32` context re-types the literal.
        if let Expr::FloatLit(v, None, span) = expr {
            if matches!(expected, Type::Float32) {
                return Ok(TypedExpr::FloatLit(*v, Type::Float32, *span));
            }
        }

        // Array literals: push the expected element type into each element so suffixless
        // integer literals adopt the correct width (and so the produced MakeArray carries the
        // expected element representation, matching the slot type at codegen). Mirrors the
        // per-element literal-coercion above for nested literals.
        if let (Expr::Array(elements, span, _), Type::Array(expected_elem)) = (expr, expected) {
            let typed_elements: Result<Vec<_>, _> =
                elements.iter().map(|e| self.check_expr(e, expected_elem)).collect();
            let typed_elements = typed_elements?;
            // When the expected element is a TUPLE shape carrying an unresolved generic TypeVar
            // (e.g. the `[String, T]` of a `[String, T][]` param), checking each element resolves it
            // concretely (`[String, Int32]`), but `expected_elem` itself still mentions `T`. Adopt
            // the FIRST checked element's concrete type as the array element so the literal's recorded
            // type is `[String, Int32][]`, not `[String, T][]` — that is what lets the monomorphizer
            // bind `T` positionally from the argument (`fromEntries`, keyed-pair builders). Gated to a
            // FixedArray (tuple) expected element with a NON-Json TypeVar: a bare `Json[]` element
            // (`TypeVar(MAX)`) must keep its wildcard so a heterogeneous `[1, "two", true]` stays a
            // tagged `Json[]`, and a plain `T[]`/`Int32[]` element is unaffected (no tuple to rebuild).
            let tuple_elem_has_generic_tv = matches!(&**expected_elem, Type::FixedArray(ts)
                if ts.iter().any(type_mentions_generic_tv));
            let elem_ty = if tuple_elem_has_generic_tv {
                // Non-empty: adopt the first checked element's concrete tuple type. Empty: there is no
                // element to pin the tuple's generic `T`, and the result is an empty container whose
                // element representation is irrelevant — erase the tuple's generic TypeVars to the
                // `Json` wildcard so a bare `fromEntries([])` still monomorphizes (to an empty map)
                // rather than leaving `T` unconstrained ("cannot infer").
                match typed_elements.first() {
                    Some(t) => t.ty(),
                    None => erase_generic_type_vars(expected_elem),
                }
            } else {
                (**expected_elem).clone()
            };
            return Ok(TypedExpr::MakeArray {
                elements: typed_elements,
                ty: Type::Array(Box::new(elem_ty)),
                span: *span,
            });
        }

        // Array literal against an expected FIXED-LENGTH array type (`[T1, T2, ...]`, §5.3).
        // Without this, the literal infers to the unbounded `T[]` (with a unioned element type
        // for heterogeneous literals) and then fails the compatibility check against the
        // positional type. Check arity, then push each positional expected type into the
        // matching element so per-element literal coercion (e.g. integer width) applies.
        if let (Expr::Array(elements, span, _), Type::FixedArray(expected_elems)) = (expr, expected) {
            if elements.len() != expected_elems.len() {
                return Err(Diagnostic::error(
                    *span,
                    format!(
                        "Expected a {}-element array for type {}, got {} element(s)",
                        expected_elems.len(),
                        expected,
                        elements.len(),
                    ),
                ));
            }
            let typed_elements: Result<Vec<_>, _> = elements
                .iter()
                .zip(expected_elems.iter())
                .map(|(e, t)| self.check_expr(e, t))
                .collect();
            let typed_elements = typed_elements?;
            let types: Vec<Type> = typed_elements.iter().map(|t| t.ty()).collect();
            return Ok(TypedExpr::MakeArray {
                elements: typed_elements,
                ty: Type::FixedArray(types),
                span: *span,
            });
        }

        // Singleton string-literal refinement (ADR-034). A bare string literal infers to
        // `String`, but when checked against an expected `StrLit("t")` it is accepted iff its
        // value equals `t`, and the resulting typed expression is narrowed to `StrLit("t")` so
        // it satisfies the literal target (e.g. a discriminant field).
        //
        // The expected type may also be a UNION of string literals (e.g. `DayOfWeek =
        // "Monday" | … | "Sunday"`), possibly behind a `Named` alias. A bare literal that equals
        // one member narrows to that member's `StrLit`, so a `DayOfWeek`-typed binding/argument/
        // return accepts `"Monday"`. Without this, the literal stays `String` and is rejected
        // against the literal-union target. `closed_string_literal_keys` peels `Named` and returns
        // the member literals only when EVERY member is a `StrLit` (the closed-literal-union case).
        if let Expr::StringLit(s, span) = expr {
            if let Type::StrLit(t) = expected {
                if s == t {
                    return Ok(TypedExpr::StringLit(s.clone(), Type::StrLit(t.clone()), *span));
                }
                return Err(Diagnostic::error(
                    *span,
                    format!("Expected literal type \"{}\", got \"{}\"", t, s),
                ));
            }
            // Closed-literal-union target: narrow to the matching member, else error against the
            // whole union. Scoped via `closed_string_literal_keys` so an OPEN union that merely
            // CONTAINS `Str` (e.g. `String | Null`) is untouched — a bare literal stays `String`
            // there and is accepted by ordinary `Str` compatibility downstream.
            if let Some(members) = self.closed_string_literal_keys(expected) {
                if members.iter().any(|m| m == s) {
                    return Ok(TypedExpr::StringLit(s.clone(), Type::StrLit(s.clone()), *span));
                }
                return Err(Diagnostic::error(
                    *span,
                    format!("Expected type {}, got \"{}\"", expected, s),
                ));
            }
        }

        // Object-literal refinement against an expected object/union/named type. Pushing the
        // expected field types down lets a discriminant string literal narrow to its `StrLit`
        // singleton, and (for a union) selects the matching variant by its discriminant tag.
        if let Expr::Object(fields, span, _) = expr {
            if let Some(result) = self.check_object_against(fields, expected, *span)? {
                return Ok(result);
            }
        }

        // Propagate the expected type into the branches of an `if`/`else` (each branch is a
        // tail position whose value is the expression's value), so an object/string literal in
        // a branch is refined against the same expected type (ADR-034). Only when both branches
        // are present (a bare `if ... then x` has an implicit Null else and is handled below).
        //
        // Bidirectional-push fix for the match-arm-union-vs-declared-object bug: when the
        // expected type is structured (an object / named object / union), each branch is checked
        // against the expected type rather than inferred-then-unioned. This refines object
        // literals structurally AND lets a `Json`-typed branch be accepted against a structured
        // object return type (see `check_branch_against` / `branch_value_compatible`), instead of
        // forming `Json | {concrete}` and rejecting that union against the declared return.
        if let Expr::If { condition, then_branch, else_branch, span, .. } = expr {
            if expected_pushes_into_branches(expected) && !matches!(else_branch.as_ref(), Expr::NullLit(_)) {
                let in_tail = self.in_tail_position;
                self.in_tail_position = false;
                let typed_cond = self.check_expr(condition, &Type::Bool)?;
                self.in_tail_position = in_tail;
                let narrowing = self.null_test_narrowing(condition);
                self.env.push_scope();
                self.apply_null_narrowing(&narrowing, true);
                let typed_then = self.check_branch_against(then_branch, expected);
                self.env.pop_scope();
                let typed_then = typed_then?;
                self.in_tail_position = in_tail;
                self.env.push_scope();
                self.apply_null_narrowing(&narrowing, false);
                let typed_else = self.check_branch_against(else_branch, expected);
                self.env.pop_scope();
                let typed_else = typed_else?;
                // Both branches were CHECKED compatible against `expected` (the declared
                // return / context type). Prefer `expected` itself as the result type when it
                // is a structured (object / union / named) target rather than re-`unify`ing the
                // branches' independently-inferred types. The re-unify loses information the
                // declared type carries: for a recursive sum union (`Num | BinOp` where
                // `BinOp.left/right : Expr`), each branch's inferred type EXPANDS the recursive
                // child to its structural `Object {…}` shape, so `unify_types` produces a union
                // whose children are `Object`, not `Named("Expr")`. lin-ir's
                // `sum_recursive_self_name` then fails to see the recursive child and the whole
                // value falls back to the BOXED representation — and an `if`/`else` tail-return
                // sum literal mis-tags its children (the tail-return pushdown bug). Using
                // `expected` keeps the `Named` recursive-child markers so the construction stays
                // unboxed and consistent with the direct-literal-return path. Sound because the
                // branches already passed the `types_compatible(&ty, expected)` gate in
                // `check_branch_against`. SCOPED to a SUM type (`sum_type_eligible`): only there
                // does the re-unify actually lose load-bearing structure (the `Named` recursive
                // children). For plain objects / sealed records the unify path is equivalent and
                // is what the sealed-stack RC-suppression analysis is tuned for — overriding it
                // there regressed that optimisation (it keys off the unified result type), so we
                // leave it untouched.
                let result_type = if is_discriminated_sum_union(expected) {
                    expected.clone()
                } else {
                    unify_types(&[typed_then.ty(), typed_else.ty()])
                };
                return Ok(TypedExpr::If {
                    cond: Box::new(typed_cond),
                    then_br: Box::new(typed_then),
                    else_br: Box::new(typed_else),
                    result_type,
                    span: *span,
                });
            }
        }

        // Bidirectional-push for `match`: check each arm body against the expected type when the
        // expected type is structured. Same rationale as the `if` branch above — this is the root
        // cause of the match-arm-union-vs-declared-object bug (a `Json` arm + a concrete-object
        // arm declared `: R` was inferred independently, unioned into `Json | {concrete}`, and
        // that union rejected against `R`). Each arm is now checked against `R` directly.
        if let Expr::Match { scrutinee, arms, span, .. } = expr {
            if expected_pushes_into_branches(expected) {
                return self.check_match(scrutinee, arms, expected, *span);
            }
        }

        // Propagate the expected type into the final expression of a block — for `StrLit`
        // singleton refinement (ADR-034), non-default scalar widths (block whose tail is a bare
        // numeric literal adopts the declared width), and STRUCTURED types (Object / Named /
        // Union). The structured-type case lets a sealed-record expected type flow into a nested
        // `if`/`match` inside the block via `check_branch_against` → `check_object_fields`,
        // producing sealed object literals. Without this, `check_expr(block, sealed_Cursor)`
        // would fall through to `infer_expr(block)` → `infer_if` → `infer_object` → unsealed,
        // triggering a `Coerce` at the function-return boundary (sealed_project_from →
        // lin_map_get per field) even though the object was NEVER stored in a LinMap.
        if let (Expr::Block(stmts, final_expr, span, _), true) =
            (expr, expected_pushes_into_branches(expected))
        {
            self.env.push_scope();
            let mut typed_stmts = Vec::new();
            // A block's non-final statements are NOT in tail position; only its final expression
            // is. Clear the flag while checking the statements (mirroring `infer_block`) so a
            // self-recursive call in a `val` RHS isn't mis-marked a tail call — that produced a
            // bogus `TailCall` whose result temp is then read by the (live) tail expression,
            // yielding an undefined-SSA-temp crash in codegen. Restore for the final expression.
            let block_tail = self.in_tail_position;
            self.in_tail_position = false;
            for stmt in stmts {
                typed_stmts.push(self.check_stmt(stmt)?);
            }
            self.in_tail_position = block_tail;
            let typed_final = self.check_expr(final_expr, expected)?;
            let block_ty = typed_final.ty();
            self.env.pop_scope();
            return Ok(TypedExpr::Block {
                stmts: typed_stmts,
                expr: Box::new(typed_final),
                ty: block_ty,
                span: *span,
            });
        }

        let inferred = self.infer_expr(expr)?;
        let actual_ty = inferred.ty();

        // Refine a fresh `arrayAllocate(n)` against an `Array(_)` expectation (Phase 4.5). The
        // `lin_array_allocate` intrinsic returns the Json-wildcard array `Array(TypeVar(MAX))`.
        // Inside a GENERIC combinator (e.g. `map<…, U>(...): U[] => arrayAllocate(n)`), the body
        // is checked against the abstract return `Array(U)`. We retype the wildcard result to
        // that expected `Array(elem)` so the element type is `U` (the generic param), NOT the
        // never-substituted `MAX` wildcard. When the function is later MONOMORPHIZED at `U=Int32`,
        // type substitution turns the recorded `result_type` into `Array(Int32)`, which finally
        // reaches codegen's `ArrayAllocate` as a concrete-scalar element type — there it emits a
        // FLAT allocation, so the producer's representation matches the concrete-typed reader
        // (which already reads flat via `lin_flat_array_get_<sfx>`). Without this, the result
        // stays tagged while a `Int32[]`-typed reader reads it flat → reinterprets 16-byte tagged
        // slots as packed scalars → garbage.
        //
        // SOUND because (a) the value is a fresh allocation whose representation the compiler
        // fully controls end-to-end, and (b) it is gated STRICTLY to the `lin_array_allocate`
        // intrinsic — no other `Json[]`-returning call (slice/concat/parse, whose runtime
        // representation we do NOT control) is ever refined. The codegen flat/tagged decision is
        // independently re-gated on `is_flat_scalar`, so a `String[]` or a still-abstract generic
        // element stays TAGGED. NO-OP for current code: the only caller today is the non-generic
        // stdlib `arrayAllocate` wrapper, whose body is checked against `Json` (`Array(MAX)` =
        // `Array(MAX)`), so the element is unchanged and the allocation stays tagged.
        if Self::is_fresh_array_allocate_call(expr) {
            if let (Type::Array(actual_elem), Type::Array(exp_elem)) = (&actual_ty, expected) {
                let actual_is_wildcard = matches!(actual_elem.as_ref(), Type::TypeVar(n) if *n == u32::MAX);
                let exp_is_wildcard = matches!(exp_elem.as_ref(), Type::TypeVar(n) if *n == u32::MAX);
                if actual_is_wildcard && !exp_is_wildcard {
                    return Ok(Self::retype_call_result(inferred, expected.clone()));
                }
            }
        }

        if !self.types_compatible(&actual_ty, expected) {
            let mut diag = Diagnostic::error(
                expr.span(),
                format!("Expected type {}, got {}", expected, actual_ty),
            );
            // Hint for the common slip of a SCALAR annotation on an ARRAY literal value
            // (`val x: UInt8 = [1, 2, 3]`). The annotation describes one scalar but the value
            // is an array; if the array's elements are compatible with the scalar, the user
            // almost certainly meant to annotate the element type with `[]` (`UInt8[]`).
            if let (Type::Array(elem_ty), true) =
                (&actual_ty, expected.is_numeric() && matches!(expr, Expr::Array(..)))
            {
                // The element type is compatible with the scalar annotation either directly
                // (`UInt8` ← `UInt8`) or via numeric family (the literal default `Int32` vs a
                // narrower/unsigned `UInt8` annotation — same family, so `UInt8[]` is the fix).
                if self.types_compatible(elem_ty, expected) || elem_ty.is_numeric() {
                    diag = diag.with_help(format!(
                        "did you mean `{0}[]`? (the value is an array; annotate the array element type with `[]`)",
                        expected
                    ));
                }
            }
            return Err(diag);
        }

        if &actual_ty != expected && actual_ty.is_numeric() && expected.is_numeric() {
            Ok(TypedExpr::Coerce {
                span: inferred.span(),
                from: actual_ty,
                to: expected.clone(),
                expr: Box::new(inferred),
            })
        } else {
            Ok(inferred)
        }
    }

    /// True when `expr` is a direct call to the `lin_array_allocate` allocation intrinsic — a
    /// freshly-allocated array whose representation the compiler fully controls, so it is safe
    /// to refine its (Json-wildcard) element type to a concrete expected scalar array type and
    /// emit a flat allocation. Only this exact intrinsic qualifies; the user-facing
    /// `arrayAllocate` stdlib wrapper erases to `Json` and is non-generic, so refining at its
    /// call site would not change the (tagged) array it actually allocates internally.
    fn is_fresh_array_allocate_call(expr: &Expr) -> bool {
        match expr {
            Expr::Call { func, .. } => matches!(func.as_ref(), Expr::Ident(n, _) if n == "lin_array_allocate"),
            _ => false,
        }
    }

    /// Replace a `TypedExpr::Call`'s `result_type` with `new_ty`. Used to retype a fresh
    /// `arrayAllocate` from the Json-wildcard array to a concrete scalar array type.
    fn retype_call_result(call: TypedExpr, new_ty: Type) -> TypedExpr {
        match call {
            TypedExpr::Call { func, args, is_tail, partial, span, .. } => TypedExpr::Call {
                func, args, result_type: new_ty, is_tail, partial, span,
            },
            other => other,
        }
    }

    pub(crate) fn infer_expr(&mut self, expr: &Expr) -> Result<TypedExpr, Diagnostic> {
        match expr {
            // Integer literal with no surrounding context. An explicit suffix pins the type
            // (spec §2.6). Otherwise the literal defaults to Int32 (spec §21) when it fits,
            // but a value beyond Int32 widens its default to the smallest type that holds it
            // (Int64, then UInt64 for decimals above i64::MAX) so the value is PRESERVED —
            // never silently truncated. The value is still available for context re-typing at
            // call sites / operators (`call.rs`, `ops.rs`), so e.g. `f(5_000_000_000)` into an
            // Int64 param still works.
            Expr::IntLit(v, suffix, span) => {
                match suffix {
                    Some(suf) => {
                        let ty = suffix_to_type(*suf);
                        if ty.is_integer() {
                            check_int_literal_fits(*v, &ty, *span)?;
                            Ok(TypedExpr::IntLit(*v, ty, *span))
                        } else {
                            // Float suffix on an integer literal (e.g. `42f32`).
                            Ok(TypedExpr::FloatLit(*v as f64, ty, *span))
                        }
                    }
                    None => Ok(TypedExpr::IntLit(*v, default_int_literal_type(*v), *span)),
                }
            }
            Expr::FloatLit(v, suffix, span) => {
                let ty = match suffix {
                    Some(suf) => suffix_to_type(*suf),
                    None => Type::Float64,
                };
                Ok(TypedExpr::FloatLit(*v, ty, *span))
            }
            Expr::StringLit(s, span) => Ok(TypedExpr::StringLit(s.clone(), Type::Str, *span)),
            Expr::BoolLit(b, span)   => Ok(TypedExpr::BoolLit(*b, *span)),
            Expr::NullLit(span)      => Ok(TypedExpr::NullLit(*span)),
            Expr::Ident(name, span)  => self.infer_ident(name, *span),
            Expr::BinaryOp { left, op, right, span } => self.infer_binary_op(left, *op, right, *span),
            Expr::Coalesce { left, right, span } => self.infer_coalesce(left, right, *span),
            Expr::UnaryOp { op, operand, span } => self.infer_unary_op(*op, operand, *span),
            Expr::Call { func, args, partial, span, .. }  => self.infer_call(func, args, *partial, *span),
            Expr::DotCall { receiver, method, method_span, args, partial, span, .. } => self.infer_dot_call(receiver, method, *method_span, args, *partial, *span),
            Expr::Index { object, key, span, .. }         => self.infer_index(object, key, *span),
            Expr::If { condition, then_branch, else_branch, span, .. } => self.infer_if(condition, then_branch, else_branch, *span),
            Expr::Match { scrutinee, arms, span, .. }     => self.infer_match(scrutinee, arms, *span),
            Expr::Block(stmts, final_expr, span, _)      => self.infer_block(stmts, final_expr, *span),
            Expr::Function { type_params, params, return_type, body, span, .. } => self.infer_function(type_params, params, return_type, body, *span, None),
            Expr::Object(fields, span, _)                => self.infer_object(fields, *span),
            Expr::Array(elements, span, _)               => self.infer_array(elements, *span),
            Expr::Assign { target, value, span }      => self.infer_assign(target, value, *span),
            Expr::IndexAssign { object, key, value, span, .. } => self.infer_index_assign(object, key, value, *span),
            Expr::StringInterp(parts, span)           => self.infer_string_interp(parts, *span),
            Expr::Is { expr, pattern, span } => {
                let typed_expr = self.infer_expr(expr)?;
                let typed_pattern = self.check_pattern(pattern, &typed_expr.ty())?;
                Ok(TypedExpr::Is { expr: Box::new(typed_expr), pattern: typed_pattern, span: *span })
            }
            Expr::Has { expr, pattern, span } => {
                let typed_expr = self.infer_expr(expr)?;
                let typed_pattern = self.check_pattern(pattern, &typed_expr.ty())?;
                Ok(TypedExpr::Has { expr: Box::new(typed_expr), pattern: typed_pattern, span: *span })
            }
            Expr::TupleArgs(exprs, span) => {
                if exprs.len() == 1 {
                    self.infer_expr(&exprs[0])
                } else {
                    let typed: Result<Vec<_>, _> = exprs.iter().map(|e| self.infer_expr(e)).collect();
                    let typed = typed?;
                    let types: Vec<Type> = typed.iter().map(|t| t.ty()).collect();
                    Ok(TypedExpr::MakeArray { elements: typed, ty: Type::FixedArray(types), span: *span })
                }
            }
        }
    }

    pub(crate) fn infer_ident(&mut self, name: &str, span: Span) -> Result<TypedExpr, Diagnostic> {
        // ADR-074: an overloaded function name has no single type, so it cannot be used as a bare
        // value (passed, stored, returned). Only a direct call can select an overload from the
        // argument types — the direct-call path in `infer_call` bypasses this method.
        if self.env.is_overloaded(name) {
            return Err(Diagnostic::error(
                span,
                format!("`{}` is an overloaded function and cannot be used as a value", name),
            )
            .with_help(
                "call it directly so the overload can be resolved from the argument types (spec §14.6)"
                    .to_string(),
            ));
        }
        let ty = self.env.effective_type(name).ok_or_else(|| {
            let all_names = self.env.all_names();
            let suggestion = lin_common::closest_match(name, all_names.into_iter(), 2);
            let mut diag = Diagnostic::error(span, format!("Undefined variable '{}'", name));
            if let Some(s) = suggestion {
                diag = diag.with_help(format!("did you mean '{}'?", s));
            }
            diag
        })?;
        let (var_scope_depth, info) = self.env.lookup_with_depth(name).unwrap();
        let slot = info.slot;
        // `lin_*` intrinsics are compiler-internal and must only be referenced from the trusted
        // stdlib (which re-exports them under clean names) — never from user code (ADR-002/ADR-008,
        // ADR-060). The `allow_intrinsics` flag is true for stdlib modules and when the
        // LIN_ALLOW_INTRINSICS test escape hatch is set.
        if !self.allow_intrinsics {
            if let Some(intr) = self.intrinsic_slots.get(&slot) {
                return Err(Diagnostic::error(
                    span,
                    format!("`{}` is a compiler-internal intrinsic and cannot be used in user code", intr),
                ).with_help("import the equivalent function from the standard library instead (e.g. `print` from \"std/io\", `arrayAllocate` from \"std/array\", or use index-assignment `obj[key] = value` instead of `lin_object_set`)".to_string()));
            }
        }
        let is_mutable = info.mutable;
        let def_span = info.def_span;
        // Record as a capture in every enclosing function where the variable was defined
        // in a strictly outer scope. This handles multi-level captures: when an inner
        // closure (depth N) captures a variable from depth D < N, ALL intermediate
        // closures also need to capture it so each can pass it down to its inner closure.
        // Global scope (depth 0) is always accessible directly — never captured.
        if var_scope_depth > 0 {
            for (i, &fn_entry_depth) in self.function_scope_depths.iter().enumerate().rev() {
                if var_scope_depth < fn_entry_depth {
                    if let Some(captures) = self.capture_stack.get_mut(i) {
                        captures.entry(slot).or_insert_with(|| Capture {
                            name: name.to_string(),
                            outer_slot: slot,
                            is_mutable,
                            ty: ty.clone(),
                        });
                    }
                } else {
                    // This function owns or is the variable — no more outer captures needed.
                    break;
                }
            }
        }
        // Affine use-after-move check (streams brief §7): ANY read of a `Stream`-typed binding
        // that has ALREADY been consumed (moved into an ownership-taking adapter/terminal) is a
        // use-after-move ERROR. The CONSUMING happens at the call site (`mark_stream_consumed`),
        // not here — low-level BORROWS (`read`/`close`) read the binding without consuming it, so
        // a recursive pull loop may read it repeatedly; only an ownership-taking op moves it.
        if type_is_streamish(&ty) && self.consumed_streams.contains(&slot) {
            return Err(Diagnostic::error(
                span,
                format!(
                    "Stream `{}` is used after it was consumed — a Stream is an affine resource \
                     (use-at-most-once); it is moved into the first stream operation it flows into \
                     (any std/iter combinator dispatched to a stream backend — map/filter/take/\
                     drop/flatMap/takeWhile/dropWhile/flatten/concat/reduce/find/some/every/while/\
                     for — or any std/stream op: lines/linesMax/chunks/writeStream/drain/collect/\
                     readText/close/promise). Re-open the source for a second pass.",
                    name
                ),
            ).with_note(def_span.unwrap_or(span), "first bound here"));
        }
        self.span_type_map.push((span, ty.to_string(), def_span));
        Ok(TypedExpr::LocalGet { slot, ty, span })
    }

    /// Mark the Stream binding referenced by `arg` (a simple identifier bound to a streamish type)
    /// as CONSUMED — called at an OWNERSHIP-TAKING stream call site (an adapter/terminal). A later
    /// read of the same binding then errors in `infer_ident`. No-op for non-identifier args (a
    /// freshly-built pipeline expression owns itself) or non-stream bindings.
    pub(crate) fn mark_stream_consumed(&mut self, arg: &Expr) {
        if let Expr::Ident(name, _) = arg {
            if let Some((_, info)) = self.env.lookup_with_depth(name) {
                let slot = info.slot;
                let ty = info.ty.clone();
                if type_is_streamish(&ty) {
                    self.consumed_streams.insert(slot);
                }
            }
        }
    }

    pub(crate) fn infer_index(&mut self, object: &Expr, key: &Expr, span: Span) -> Result<TypedExpr, Diagnostic> {
        let typed_obj = self.infer_expr(object)?;
        let typed_key = self.infer_expr(key)?;
        // Flow-narrowing for an index place (`m[k]`) under an active `if m[k] != null` test: read
        // the tightened type instead of the default `T | Null`. Sound because the same stable
        // `obj`/`key` denote the same slot; the narrowing only drops `Null`.
        if let Some(narrowed) = self.lookup_index_narrowing(object, key) {
            return Ok(TypedExpr::Index {
                object: Box::new(typed_obj),
                key: Box::new(typed_key),
                result_type: narrowed,
                span,
            });
        }
        let obj_ty = typed_obj.ty();
        let result_type = match &obj_ty {
            Type::Array(elem) => *elem.clone(),
            Type::FixedArray(elems) => {
                if let TypedExpr::IntLit(idx, _, _) = typed_key {
                    elems.get(idx as usize).cloned().unwrap_or(Type::Null)
                } else {
                    unify_types(elems)
                }
            }
            Type::Object { fields, .. } => {
                if fields.is_empty() {
                    // Empty schema (e.g. `var result = {}`): object may be populated dynamically,
                    // so any key access must be a runtime lookup → TypeVar.
                    self.env.fresh_type_var()
                } else if let TypedExpr::IntLit(ref n, _, _) = typed_key {
                    // Integer literal key on a fixed record: look up the string representation.
                    // This handles `obj[0]` / `obj[dow]` where the record was expanded from a
                    // `{ <int-literal-union>: V }` index-signature.
                    let key_str = n.to_string();
                    fields.get(&key_str).cloned().unwrap_or(Type::Null)
                } else if let TypedExpr::StringLit(ref key_str, _, _) = typed_key {
                    if !fields.contains_key(key_str) {
                        // Key not in the known object type — emit a warning with a "did you mean" hint.
                        let suggestion = lin_common::closest_match(
                            key_str,
                            fields.keys().map(|s| s.as_str()),
                            3,
                        );
                        let mut diag = lin_common::Diagnostic::warning(
                            span,
                            format!("field \"{}\" does not exist on this object type", key_str),
                        );
                        if let Some(s) = suggestion {
                            diag = diag.with_help(format!("did you mean \"{}\"?", s));
                        }
                        self.diagnostics.push(diag);
                    }
                    fields.get(key_str).cloned().unwrap_or(Type::Null)
                } else {
                    self.object_index_nonliteral(fields, &typed_key.ty())
                }
            }
            // Typed index-signature map `{ K: V }` (ADR-055 + numeric-key extension): a key access
            // yields `V | Null` (the missing-key → Null safe-bracket rule, §6.1). No per-key field
            // tracking — the key set is dynamic by construction. The key type must be compatible with
            // the map's declared key type.
            Type::Map { key: map_key_ty, value: val_ty } => {
                let key_ty = typed_key.ty();
                let key_ok = if map_key_ty.is_integer() {
                    key_ty.is_integer() || matches!(key_ty, Type::TypeVar(_))
                } else {
                    // String-keyed map
                    key_ty.is_string_ish() || matches!(key_ty, Type::TypeVar(_))
                };
                if !key_ok {
                    return Err(Diagnostic::error(
                        span,
                        format!("a `{}` is keyed by `{}`, but the key is `{}`", obj_ty, map_key_ty, key_ty),
                    ));
                }
                Type::flatten_union(vec![(**val_ty).clone(), Type::Null])
            }
            Type::Null => Type::Null,
            Type::TypeVar(_) => self.env.fresh_type_var(),
            Type::Union(variants) => {
                // Peel Null out, compute result type for the non-null variants, then add Null back.
                let non_null: Vec<Type> = variants.iter().filter(|t| **t != Type::Null).cloned().collect();
                if non_null.is_empty() {
                    Type::Null
                } else {
                    let inner = if non_null.len() == 1 {
                        match &non_null[0] {
                            Type::Object { fields, .. } => {
                                if let TypedExpr::StringLit(ref key_str, _, _) = typed_key {
                                    fields.get(key_str).cloned().unwrap_or(Type::Null)
                                } else {
                                    Type::Union(fields.values().cloned().collect())
                                }
                            }
                            Type::Array(elem) => *elem.clone(),
                            Type::FixedArray(elems) => {
                                if let TypedExpr::IntLit(idx, _, _) = typed_key {
                                    elems.get(idx as usize).cloned().unwrap_or(Type::Null)
                                } else {
                                    unify_types(elems)
                                }
                            }
                            _ => self.env.fresh_type_var(),
                        }
                    } else if let TypedExpr::StringLit(ref key_str, _, _) = typed_key {
                        // Multi-variant union indexed by a STRING-LITERAL key. When the key is
                        // statically present in at least one record variant (e.g. `value` of
                        // `{type:"success", value:U} | {type:"failure", error:E}`, a discriminated
                        // `Result`), collect the field's type from EVERY variant that declares it
                        // (and `Null` for variants that lack it — the §6.1 safe-bracket rule) and
                        // union them. This makes a generic result like `mapOk(...): Result<U,E>`
                        // index PRECISELY (to `U | Null` once `U` is pinned), so the inferred type
                        // names the RESOLVED field type instead of an opaque fresh inference var.
                        //
                        // When NO variant declares the key, fall back to a fresh TypeVar so codegen
                        // keeps a DYNAMIC lookup: union members are open objects that may carry
                        // runtime fields beyond their static shape (the decode-`Error` value's
                        // `"path"`, a width-subtyping extra) — narrowing such an access to a static
                        // `Null` would wrongly suppress the runtime lookup.
                        let mut found: Vec<Type> = Vec::new();
                        let mut any_present = false;
                        for v in &non_null {
                            if let Type::Object { fields, .. } = v {
                                if let Some(t) = fields.get(key_str) {
                                    found.push(t.clone());
                                    any_present = true;
                                }
                            }
                        }
                        if any_present {
                            Type::flatten_union(found)
                        } else {
                            self.env.fresh_type_var()
                        }
                    } else {
                        // Non-literal key into a multi-variant union: keep a fresh TypeVar so codegen
                        // performs a DYNAMIC field lookup (see the string-literal note above).
                        self.env.fresh_type_var()
                    };
                    Type::flatten_union(vec![inner, Type::Null])
                }
            }
            // A value whose static type is a `Type::Named("X")` (e.g. the result of a
            // mutually-recursive call whose return type survived forward-declaration as a Named
            // alias, or an annotated named record). Resolve the alias to its concrete body and
            // index into THAT. `resolve_named_body` peels Named aliases (with a cycle guard so a
            // self-recursive `type T = … T …` does not loop) down to the first non-Named body; if
            // that body is an indexable shape (`Object`/`Map`/`Array`/…) we re-run `infer_index`
            // logic by recursing on the resolved type. If it bottoms out at a still-`Named` (true
            // cycle with no indexable layer) or a non-indexable type, fall through to the existing
            // "Cannot index" error — i.e. fail conservatively, never invent a result type.
            Type::Named(name) => {
                if let Some(resolved) = self.resolve_named_body(&obj_ty) {
                    return self.infer_index_into(typed_obj, typed_key, &resolved, span);
                }
                // resolve_named_body returned None: either the decl has generic params (can't
                // be instantiated without type args) or it is a forward-declaration placeholder
                // (the body is `Named(name)` — set by forward_declare_types before check_stmt
                // fills in the real body). Either way we cannot statically index into this type.
                let key_ty = typed_key.ty();
                // Try to surface the expanded body. Prefer the already-resolved env entry;
                // fall back to the raw AST body recorded during forward_declare_types so
                // we can show the source-level shape even for types declared after this usage.
                let expanded: Option<String> = self.env.lookup_type(name).and_then(|decl| {
                    if matches!(&decl.body, Type::Named(n) if n == name) {
                        // Forward-declaration self-cycle — env body not yet resolved.
                        // Use the raw AST body instead.
                        self.raw_type_decls.get(name)
                            .map(|raw| lin_parse::fmt_type(raw))
                    } else if decl.params.is_empty() {
                        Some(format!("{}", decl.body))
                    } else {
                        None // generic — no single expansion
                    }
                });
                let is_generic = self.env.lookup_type(name)
                    .is_some_and(|decl| !decl.params.is_empty());
                let key_is_string_or_unknown =
                    key_ty.is_string_ish() || matches!(key_ty, Type::TypeVar(_));
                // Pick a headline that reflects the actual kind of type.
                let is_fwd_index_sig = self.raw_type_decls.get(name)
                    .is_some_and(|raw| matches!(raw, lin_parse::ast::TypeExpr::IndexSig(..)));
                let headline = if is_fwd_index_sig {
                    format!("`{}` is not yet resolved at this point", name)
                } else {
                    format!("`{}` is a fixed-shape record and cannot be indexed dynamically", name)
                };
                let mut diag = Diagnostic::error(span, headline);
                if is_generic {
                    diag = diag.with_help(format!(
                        "`{}` is a generic type — provide type arguments to index into it", name
                    ));
                } else {
                    let raw_is_index_sig = self.raw_type_decls.get(name)
                        .is_some_and(|raw| matches!(raw, lin_parse::ast::TypeExpr::IndexSig(..)));
                    let help = if raw_is_index_sig {
                        // The type IS declared as a map (`{ KeyType: ValueType }`) but the
                        // forward-declaration self-cycle prevented resolution. Tell the user
                        // the type and that moving it earlier will fix the error.
                        match &expanded {
                            Some(body) => format!(
                                "`{}` = {} — move this type declaration above its first use",
                                name, body
                            ),
                            None => format!(
                                "`{}` is declared as a map but not yet resolved — move the type declaration above its first use",
                                name
                            ),
                        }
                    } else {
                        // The type is a fixed-shape record. If key is string-like, suggest
                        // switching to a map declaration.
                        match (&expanded, key_is_string_or_unknown) {
                            (Some(body), true) => format!(
                                "`{}` = {} — to index dynamically, change to a map: `{{ String: <ValueType> }}`",
                                name, body
                            ),
                            (Some(body), false) => format!("`{}` = {}", name, body),
                            (None, true) => "to index dynamically, change the type to a map: `{ String: <ValueType> }`".to_string(),
                            (None, false) => String::new(),
                        }
                    };
                    if !help.is_empty() {
                        diag = diag.with_help(help);
                    }
                }
                return Err(diag);
            }
            _ => {
                let key_ty = typed_key.ty();
                let key_label = if matches!(key_ty, Type::TypeVar(_)) {
                    "a dynamically-typed key".to_string()
                } else {
                    format!("a key of type `{}`", key_ty)
                };
                return Err(Diagnostic::error(
                    span,
                    format!("cannot index into `{}` with {}", obj_ty, key_label),
                ));
            }
        };
        Ok(TypedExpr::Index { object: Box::new(typed_obj), key: Box::new(typed_key), result_type, span })
    }

    /// Resolve a `Type::Named` to its underlying non-`Named` body via the type environment,
    /// peeling chained aliases (`type A = B; type B = { … }`). Cycle-guarded: a self-referential
    /// alias that never reaches a concrete body (`type T = T`, or a recursive union with no
    /// indexable layer at the top) returns `None` rather than looping forever. Generic named types
    /// (params non-empty) and unknown names also return `None`. Returns the resolved body for any
    /// other (non-Named) starting type unchanged.
    fn resolve_named_body(&self, ty: &Type) -> Option<Type> {
        let mut current = ty.clone();
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        loop {
            match &current {
                Type::Named(n) => {
                    if !visited.insert(n.clone()) {
                        // Cycle with no concrete body in between → give up (conservative).
                        return None;
                    }
                    let decl = self.env.lookup_type(n)?;
                    if !decl.params.is_empty() {
                        return None;
                    }
                    current = decl.body.clone();
                }
                _ => return Some(current),
            }
        }
    }

    /// If `ty` denotes a CLOSED set of string-literal keys — a single `StrLit` or a union whose
    /// every member is a `StrLit` (after peeling `Named` aliases) — return that set of literal
    /// strings. Otherwise return `None`.
    ///
    /// Used by the index-result computation: when an object is indexed by a non-literal key whose
    /// TYPE is such a closed literal set AND every literal is a declared field, the access is
    /// provably total, so the safe-bracket `Null` (§6.1, missing-key fallback) must NOT be added.
    /// A `dow: DayOfWeek` (= `"Monday" | … | "Sunday"`) indexing a `ServiceDays` record whose keys
    /// are exactly those seven literals reads precisely `Boolean`, never `Boolean | Null`.
    pub(crate) fn closed_string_literal_keys(&self, ty: &Type) -> Option<Vec<String>> {
        let resolved = self.resolve_named_body(ty)?;
        match resolved {
            Type::StrLit(s) => Some(vec![s]),
            Type::Union(variants) => {
                let mut keys = Vec::with_capacity(variants.len());
                for v in &variants {
                    match self.resolve_named_body(v)? {
                        Type::StrLit(s) => keys.push(s),
                        _ => return None,
                    }
                }
                Some(keys)
            }
            _ => None,
        }
    }

    /// If `ty` denotes a CLOSED set of integer-literal values — a single `IntLit` or a union whose
    /// every member is an `IntLit` (after peeling `Named` aliases) — return those values.
    /// Otherwise return `None`. Mirrors `closed_string_literal_keys` for the integer domain.
    pub(crate) fn closed_int_literal_keys(&self, ty: &Type) -> Option<Vec<i64>> {
        let resolved = self.resolve_named_body(ty)?;
        match resolved {
            Type::IntLit(n) => Some(vec![n]),
            Type::Union(variants) => {
                let mut keys = Vec::with_capacity(variants.len());
                for v in &variants {
                    match self.resolve_named_body(v)? {
                        Type::IntLit(n) => keys.push(n),
                        _ => return None,
                    }
                }
                Some(keys)
            }
            _ => None,
        }
    }

    /// Compute the result type of indexing an `Object`'s `fields` with a non-literal key of type
    /// `key_ty`. Returns the precise union of just the addressed fields' types (NO safe-bracket
    /// `Null`) when `key_ty` is a closed set of string literals all present in `fields`; otherwise
    /// the conservative `<all field types> | Null`. Shared by both `infer_index` and
    /// `infer_index_into` so the precise-totality rule applies through `Named`-alias objects too.
    fn object_index_nonliteral(&self, fields: &IndexMap<String, Type>, key_ty: &Type) -> Type {
        if let Some(keys) = self.closed_string_literal_keys(key_ty) {
            if keys.iter().all(|k| fields.contains_key(k)) {
                return Type::flatten_union(
                    keys.iter().map(|k| fields[k].clone()).collect(),
                );
            }
        }
        // Closed integer-literal union key (e.g. `DayOfWeek = 0|1|…|6`) on a record expanded from
        // `{ DayOfWeek: V }`: all keys are known to be present, so access is total (no `| Null`).
        if let Some(int_keys) = self.closed_int_literal_keys(key_ty) {
            let str_keys: Vec<String> = int_keys.iter().map(|n| n.to_string()).collect();
            if str_keys.iter().all(|k| fields.contains_key(k)) {
                return Type::flatten_union(
                    str_keys.iter().map(|k| fields[k].clone()).collect(),
                );
            }
        }
        // Conservative fallback: any field value, plus the safe-bracket `Null` (§6.1). Routed
        // through `flatten_union` so duplicate value types collapse (`Boolean | Boolean | Null`
        // → `Boolean | Null`) — duplicates were what surfaced the un-collapsed `Boolean × 7`.
        let mut members: Vec<Type> = fields.values().cloned().collect();
        members.push(Type::Null);
        Type::flatten_union(members)
    }

    /// The body of `infer_index`'s result-type computation, factored out so the `Type::Named` arm
    /// can recurse after resolving the alias. Re-runs `infer_index` on an already-typed object and
    /// key against an explicitly-provided object type (`obj_ty`). This is only entered from the
    /// `Named` arm with a freshly-resolved concrete body, so a `Named` arriving here again is a
    /// genuine cycle and yields the "cannot index into" error.
    fn infer_index_into(
        &mut self,
        typed_obj: TypedExpr,
        typed_key: TypedExpr,
        obj_ty: &Type,
        span: Span,
    ) -> Result<TypedExpr, Diagnostic> {
        let result_type = match obj_ty {
            Type::Array(elem) => *elem.clone(),
            Type::FixedArray(elems) => {
                if let TypedExpr::IntLit(idx, _, _) = typed_key {
                    elems.get(idx as usize).cloned().unwrap_or(Type::Null)
                } else {
                    unify_types(elems)
                }
            }
            Type::Object { fields, .. } => {
                if fields.is_empty() {
                    self.env.fresh_type_var()
                } else if let TypedExpr::StringLit(ref key_str, _, _) = typed_key {
                    if !fields.contains_key(key_str) {
                        let suggestion = lin_common::closest_match(
                            key_str,
                            fields.keys().map(|s| s.as_str()),
                            3,
                        );
                        let mut diag = lin_common::Diagnostic::warning(
                            span,
                            format!("field \"{}\" does not exist on this object type", key_str),
                        );
                        if let Some(s) = suggestion {
                            diag = diag.with_help(format!("did you mean \"{}\"?", s));
                        }
                        self.diagnostics.push(diag);
                    }
                    fields.get(key_str).cloned().unwrap_or(Type::Null)
                } else {
                    self.object_index_nonliteral(fields, &typed_key.ty())
                }
            }
            Type::Map { key: map_key_ty, value: val_ty } => {
                // Map key must match the map's key type (mirrors `infer_index` / the index-assign guard).
                let key_ty = typed_key.ty();
                let key_ok = if map_key_ty.is_integer() {
                    key_ty.is_integer() || matches!(key_ty, Type::TypeVar(_))
                } else {
                    key_ty.is_string_ish() || matches!(key_ty, Type::TypeVar(_))
                };
                if !key_ok {
                    return Err(Diagnostic::error(
                        span,
                        format!("a `{}` is keyed by `{}`, but the key is `{}`", obj_ty, map_key_ty, key_ty),
                    ));
                }
                Type::flatten_union(vec![(**val_ty).clone(), Type::Null])
            }
            Type::Null => Type::Null,
            Type::TypeVar(_) => self.env.fresh_type_var(),
            Type::Union(variants) => {
                // A Named alias resolving to a union (e.g. `type Ast = Num | BinOp`). Mirror the
                // inline multi-variant `Union` arm in `infer_index`: a single record variant is
                // indexed precisely; a multi-variant union indexed by a string-literal key collects
                // the field's type from every variant that declares it (precise per-variant lookup),
                // falling back to a fresh TypeVar only when NO variant declares the key (a dynamic
                // open-object lookup). The safe-bracket `Null` is re-added either way.
                let non_null: Vec<Type> =
                    variants.iter().filter(|t| **t != Type::Null).cloned().collect();
                let inner = if non_null.len() == 1 {
                    let body = self.resolve_named_body(&non_null[0]).unwrap_or_else(|| non_null[0].clone());
                    match &body {
                        Type::Object { fields, .. } => {
                            if let TypedExpr::StringLit(ref key_str, _, _) = typed_key {
                                fields.get(key_str).cloned().unwrap_or(Type::Null)
                            } else {
                                Type::Union(fields.values().cloned().collect())
                            }
                        }
                        Type::Array(elem) => *elem.clone(),
                        _ => self.env.fresh_type_var(),
                    }
                } else if non_null.is_empty() {
                    return Ok(TypedExpr::Index {
                        object: Box::new(typed_obj),
                        key: Box::new(typed_key),
                        result_type: Type::Null,
                        span,
                    });
                } else if let TypedExpr::StringLit(ref key_str, _, _) = typed_key {
                    let mut found: Vec<Type> = Vec::new();
                    let mut any_present = false;
                    for v in &non_null {
                        let body = self.resolve_named_body(v).unwrap_or_else(|| v.clone());
                        if let Type::Object { fields, .. } = &body {
                            if let Some(t) = fields.get(key_str) {
                                found.push(t.clone());
                                any_present = true;
                            }
                        }
                    }
                    if any_present {
                        Type::flatten_union(found)
                    } else {
                        self.env.fresh_type_var()
                    }
                } else {
                    self.env.fresh_type_var()
                };
                Type::flatten_union(vec![inner, Type::Null])
            }
            other => {
                let key_ty = typed_key.ty();
                let key_label = if matches!(key_ty, Type::TypeVar(_)) {
                    "a dynamically-typed key".to_string()
                } else {
                    format!("a key of type `{}`", key_ty)
                };
                return Err(Diagnostic::error(
                    span,
                    format!("cannot index into `{}` with {}", other, key_label),
                ));
            }
        };
        Ok(TypedExpr::Index { object: Box::new(typed_obj), key: Box::new(typed_key), result_type, span })
    }

    /// Flow-narrowing for an `if`/`else` condition: when it is a type/null test on a simple
    /// identifier whose static type is a union, narrow that binding in EACH branch where the static
    /// type tightens. Generalizes the old null-only form to any `is X` test (incl. the structural
    /// `Error` member).
    ///
    /// Returns `Some(NarrowTest { name, then_ty, else_ty })`, where each `*_ty` is the binding's
    /// narrowed type in that branch (or `None` if that branch does not tighten the type):
    ///   - `v is X`     → THEN narrows to `X` (the matched member), ELSE to the complement
    ///                    (`union minus X`).
    ///   - `v == null`  → THEN narrows to `Null`, ELSE to the complement.
    ///   - `v != null`  → THEN narrows to the complement, ELSE to `Null`.
    ///
    /// Only fires when the binding's static type is a union that contains the tested type as an
    /// exact member (so the complement is well-defined and non-empty — see `without_variant`). The
    /// matched-member (`then` for `is`) narrowing fires whenever `X` is an exact member, which is
    /// the case the union/complement check already guarantees. Composes with — and does not replace
    /// — the existing `match`/`is` narrowing.
    pub(crate) fn null_test_narrowing(&self, condition: &Expr) -> Option<NarrowTest> {
        // `x == null` / `x != null` against the `null` literal (either operand order).
        let (place, matched, then_gets_matched) = match condition {
            Expr::BinaryOp { left, op, right, .. }
                if matches!(op, lin_parse::ast::BinOp::Eq | lin_parse::ast::BinOp::NotEq) =>
            {
                let place = match (left.as_ref(), right.as_ref()) {
                    (operand, Expr::NullLit(_)) | (Expr::NullLit(_), operand) => {
                        self.narrow_place_of(operand)
                    }
                    _ => None,
                }?;
                // `== null`: Null (the matched type) holds in THEN. `!= null`: Null holds in ELSE.
                let then_gets_matched = matches!(op, lin_parse::ast::BinOp::Eq);
                (place, Type::Null, then_gets_matched)
            }
            // `x is X`: `X` (the matched type) holds in THEN, the complement in ELSE. `X` may be
            // `Null`, a scalar (`Int32`), a named/structural type, or `Error` (the structural
            // alias). Only simple-identifier subjects are narrowed here (index places use the
            // `== null`/`!= null` form above).
            Expr::Is { expr, pattern, .. } => {
                let ident = match expr.as_ref() {
                    Expr::Ident(n, _) => n,
                    _ => return None,
                };
                let matched = match pattern.as_ref() {
                    lin_parse::ast::Pattern::TypeName(name, span) => resolve_type(
                        &lin_parse::ast::TypeExpr::Named(name.clone(), *span),
                        &self.env,
                    )
                    .ok()?,
                    lin_parse::ast::Pattern::Literal(e) if matches!(e.as_ref(), Expr::NullLit(_)) => {
                        Type::Null
                    }
                    _ => return None,
                };
                (NarrowPlace::Ident(ident.clone()), matched, true)
            }
            _ => return None,
        };
        // The place's current static type — for an ident it is the binding's type; for an index
        // place it is the index read's result type (`T | Null` for a `{String:T}` map).
        let place_ty = self.narrow_place_type(&place)?;
        // The complement (union minus the matched member) — also confirms `matched` is an exact
        // member of the union, the precondition for narrowing in either direction.
        let complement = place_ty.without_variant(&matched)?;
        // The matched branch narrows the place to the matched member alone; the other branch to
        // the complement.
        let (then_ty, else_ty) = if then_gets_matched {
            (Some(matched), Some(complement))
        } else {
            (Some(complement), Some(matched))
        };
        Some(NarrowTest { place, then_ty, else_ty })
    }

    /// If `expr` is a narrowable place — a simple identifier, or an index read `base[key]…` whose
    /// base canonicalizes to an identifier-rooted `PlacePath` and whose every key is a
    /// string-literal or a simple identifier — return the corresponding `NarrowPlace`. Otherwise
    /// `None` (the expression is not stably re-readable, so narrowing it would be unsound).
    fn narrow_place_of(&self, expr: &Expr) -> Option<NarrowPlace> {
        match place_path_of_expr(expr)? {
            PlacePath::Root(n) => Some(NarrowPlace::Ident(n)),
            path @ PlacePath::Index(..) => Some(NarrowPlace::Index(path)),
        }
    }

    /// The current static type of a narrowable place. For an identifier this is the binding's
    /// declared/narrowed type; for an index path it is the type a read of that path would produce.
    fn narrow_place_type(&self, place: &NarrowPlace) -> Option<Type> {
        match place {
            NarrowPlace::Ident(name) => Some(self.env.lookup(name)?.ty.clone()),
            NarrowPlace::Index(path) => self.place_path_type(path),
        }
    }

    /// The static type a READ of `path` would produce, walked from the root. The root is a binding
    /// lookup; each index step models the read type for the base's shape — `Type::Map` (`{String:T}`
    /// → `T | Null`, the safe-bracket rule) and `Type::Array` (`T[]` → element type). A `Named`
    /// alias base is resolved one or more levels first. Any other base shape returns `None`, keeping
    /// the narrowing scoped to the map/array idiom that motivated it (a fixed-record field read does
    /// not produce a `| Null` that a null-test could strip).
    fn place_path_type(&self, path: &PlacePath) -> Option<Type> {
        match path {
            PlacePath::Root(name) => Some(self.env.lookup(name)?.ty.clone()),
            PlacePath::Index(base, key) => {
                let base_ty = self.place_path_type(base)?;
                let base_ty = self.resolve_named_body(&base_ty).unwrap_or(base_ty);
                match base_ty {
                    Type::Map { value: val_ty, .. } => {
                        Some(Type::flatten_union(vec![(*val_ty).clone(), Type::Null]))
                    }
                    // `T[]` indexed: element type (array reads are not `| Null`, so a null test on
                    // one cannot narrow — `without_variant(Null)` will fail and bail out).
                    Type::Array(elem) => Some((*elem).clone()),
                    // A fixed-record field read on an INTERMEDIATE step of the path (e.g.
                    // `service["dates"]` in `service["dates"][date]`): a string-literal key reads
                    // the declared field type. This is how a compound place reaches the inner
                    // Map/Array that the final step's null-test actually narrows. The field read
                    // itself is total for a known key, so no `| Null` is introduced here.
                    Type::Object { fields, .. } => match key {
                        IndexKey::StrLit(k) => fields.get(k).cloned(),
                        IndexKey::Ident(_) => None,
                    },
                    _ => None,
                }
            }
        }
    }

    /// Apply a flow-narrowing (from `null_test_narrowing`) within the CURRENT scope, using the
    /// narrowed type for the branch being entered (if that branch tightens the type). Reuses the
    /// original slot via `define_narrowed` so `LocalGet` reads the same TaggedVal pointer (the value
    /// is bit-identical — only the static type tightens). Must be called immediately after
    /// `push_scope` for the branch and undone by the matching `pop_scope`.
    pub(crate) fn apply_null_narrowing(&mut self, narrowing: &Option<NarrowTest>, entering_then: bool) {
        if let Some(test) = narrowing {
            let narrowed_ty = if entering_then { &test.then_ty } else { &test.else_ty };
            if let Some(narrowed_ty) = narrowed_ty {
                match &test.place {
                    NarrowPlace::Ident(name) => {
                        if let Some(info) = self.env.lookup(name) {
                            let slot = info.slot;
                            self.env.define_narrowed(name.clone(), narrowed_ty.clone(), slot);
                        }
                    }
                    // Index places are narrowed via the `index_narrowings` stack, which
                    // `infer_index` consults. The caller (`infer_if`) records the stack depth
                    // before the branch and truncates back to it afterwards.
                    NarrowPlace::Index(path) => {
                        self.index_narrowings.push(IndexNarrow {
                            path: path.clone(),
                            ty: narrowed_ty.clone(),
                        });
                    }
                }
            }
        }
    }

    /// Invalidate any active index-narrowing whose path MENTIONS `name` — as its root object or as
    /// an identifier key (e.g. after an `obj = …` reassignment, or a write to a key variable `k`
    /// used in `m[k]`). The path may now denote a different slot/value, so the tightened type no
    /// longer holds. Conservative by design: clearing too much only loses a narrowing (re-widens to
    /// `T | Null`), never unsoundly keeps a stale one. Called from assignment checking.
    pub(crate) fn clear_index_narrowings_for(&mut self, name: &str) {
        self.index_narrowings.retain(|n| !place_path_mentions(&n.path, name));
    }

    /// Look up an active index-narrowing for an index read. `infer_index` calls this with the
    /// syntactic object/key; the read is canonicalized to a `PlacePath` and a hit returns the
    /// tightened (`Null`-stripped) read type.
    pub(crate) fn lookup_index_narrowing(&self, object: &Expr, key: &Expr) -> Option<Type> {
        if self.index_narrowings.is_empty() {
            return None;
        }
        // Reconstruct the full read path `object[key]` and canonicalize it.
        let base = place_path_of_expr(object)?;
        let k = match key {
            Expr::StringLit(s, _) => IndexKey::StrLit(s.clone()),
            Expr::Ident(k, _) => IndexKey::Ident(k.clone()),
            _ => return None,
        };
        let path = PlacePath::Index(Box::new(base), k);
        // Last match wins (innermost/most-recent narrowing).
        self.index_narrowings
            .iter()
            .rev()
            .find(|n| n.path == path)
            .map(|n| n.ty.clone())
    }

    pub(crate) fn infer_if(&mut self, condition: &Expr, then_branch: &Expr, else_branch: &Expr, span: Span) -> Result<TypedExpr, Diagnostic> {
        // Condition is not in tail position; branches inherit it.
        let in_tail = self.in_tail_position;
        self.in_tail_position = false;
        let typed_cond = self.check_expr(condition, &Type::Bool)?;
        self.in_tail_position = in_tail;
        // Affine branch merge (streams brief §7): each branch starts from the consumed set as it
        // was BEFORE the `if`; afterwards a Stream is consumed if it was consumed in EITHER branch
        // (conservative — prevents a use after a possible move). The condition runs before both
        // branches, so any consume there is shared by both.
        let consumed_before = self.consumed_streams.clone();
        // Flow-narrow a union binding on a type/null test (`x is X`, `x == null`, `x != null`):
        // the matched branch narrows to the matched member, the other to the complement. The
        // narrowing is scoped: pushed before the relevant branch and popped after, so it never
        // leaks past the `if`.
        let narrowing = self.null_test_narrowing(condition);
        // Depth of the index-narrowing stack before this `if`; index-place narrowings pushed for
        // a branch are truncated back to here after the branch is checked (the ident-place
        // narrowings are scoped by push_scope/pop_scope instead).
        let index_narrow_base = self.index_narrowings.len();
        let typed_then = {
            self.env.push_scope();
            self.apply_null_narrowing(&narrowing, true);
            let r = self.infer_expr(then_branch);
            self.env.pop_scope();
            self.index_narrowings.truncate(index_narrow_base);
            r?
        };
        let consumed_then = self.consumed_streams.clone();
        self.consumed_streams = consumed_before;
        self.in_tail_position = in_tail;
        let typed_else = {
            self.env.push_scope();
            self.apply_null_narrowing(&narrowing, false);
            let r = self.infer_expr(else_branch);
            self.env.pop_scope();
            self.index_narrowings.truncate(index_narrow_base);
            r?
        };
        // Merge: union of both branches' consumed sets.
        self.consumed_streams.extend(consumed_then);
        let then_ty = typed_then.ty();
        let else_ty = typed_else.ty();
        // Path-11: snapshot branch lambda sets BEFORE the merge so a function-typed `if` whose arms
        // are distinct lambdas (`if c then f else g`) yields a 2-set rather than aliasing onto
        // whichever branch the structural collapse picks (PartialEq ignores `lset`).
        let then_lset = top_level_lambda_set(&then_ty);
        let else_lset = top_level_lambda_set(&else_ty);
        let result_type = self.join_branch_types(then_ty, else_ty);
        // Path-11: stamp the result with the join of the branch lambda sets (inert metadata).
        let result_type = with_joined_lambda_set(result_type, &then_lset, &else_lset);
        Ok(TypedExpr::If {
            cond: Box::new(typed_cond),
            then_br: Box::new(typed_then),
            else_br: Box::new(typed_else),
            result_type,
            span,
        })
    }

    /// Resolve the static type of an `if`/`else` expression given the independently-inferred types
    /// of its two branches. This is a lattice join over Lin's type structure; the arms below encode
    /// a fixed precedence — earlier arms override later ones. The ordering is load-bearing:
    ///
    /// 1. **Empty-array wins**: `Never[]` (the bottom array `[]`) yields to any non-empty-array
    ///    branch; `types_compatible` otherwise picks `Never[]` and discards the real element type.
    /// 2. **Distinct generic params**: two different quantified `TypeVar`s (`T9001 × D9002`) must
    ///    form a union — `types_compatible` collapses them to one and breaks monomorphization.
    /// 3. **Exactly-one-Null**: keep the union so the value-producing branch survives
    ///    monomorphization; `Json | Null` collapses to `Json` (redundant, leaks the sentinel id).
    /// 4. **Dynamic top type**: one `AnyVal`/unconstrained-inference-var branch means the result
    ///    IS `AnyVal` — `types_compatible` would otherwise pick the concrete branch, giving the
    ///    `AnyVal` value a wrong static tag at the phi merge → crash/corruption on unbox.
    /// 5. **`types_compatible` collapse**: standard structural subtype check (then⊆else, or vice
    ///    versa); picks the more general side.
    /// 6. **Fallback union**: neither is a subtype of the other → honest union.
    ///
    /// Lambda-set stamping (`Path-11`) is applied by the caller AFTER this function so it can
    /// snapshot the branch sets before they are consumed by the merge.
    pub(crate) fn join_branch_types(&self, then_ty: Type, else_ty: Type) -> Type {
        // TypeVar ids ≥ 9001 are quantified generic params; the Json wildcard is u32::MAX.
        const GENERIC_TV_BASE: u32 = 9000;

        // Arm 1 — empty-array branch: `Array(Never)` (bottom array `[]`) must not win.
        // `types_compatible` treats `Never[]` as assignable to/from anything, so the collapse in
        // arm 5 would otherwise pick `Never[]` and discard the real element type.
        let then_empty_arr = matches!(&then_ty, Type::Array(e) if matches!(**e, Type::Never));
        let else_empty_arr = matches!(&else_ty, Type::Array(e) if matches!(**e, Type::Never));
        if then_empty_arr != else_empty_arr {
            return if then_empty_arr { else_ty } else { then_ty };
        }

        // Arm 2 — distinct quantified generic params: `T × D` (both ids in [GENERIC_TV_BASE,
        // u32::MAX), different ids). `types_compatible` unifies an unconstrained TypeVar with
        // anything, so arm 5 would collapse them to one — breaking monomorphization.
        let distinct_generic_params = matches!(
            (&then_ty, &else_ty),
            (Type::TypeVar(a), Type::TypeVar(b))
                if a != b
                    && *a >= GENERIC_TV_BASE && *a != u32::MAX
                    && *b >= GENERIC_TV_BASE && *b != u32::MAX
        );
        if distinct_generic_params {
            return Type::flatten_union(vec![then_ty, else_ty]);
        }

        // Arm 3 — exactly one branch is `Null`: form a union so the value-producing branch
        // survives. Exception: `Json | Null` collapses to `Json` (Json subsumes Null; keeping the
        // union would leak the `?T4294967295` sentinel into diagnostics).
        if (then_ty == Type::Null) != (else_ty == Type::Null) {
            let other = if then_ty == Type::Null { &else_ty } else { &then_ty };
            return if is_any_val(other) {
                other.clone()
            } else {
                Type::flatten_union(vec![then_ty, else_ty])
            };
        }

        // An unsolved inference TypeVar (id < GENERIC_TV_BASE, not the Json wildcard, not in
        // `solved_type_vars`) behaves like `AnyVal` for the merge: it holds a boxed Json value at
        // runtime. Treating it as concrete would give it the wrong static tag at the phi merge.
        let is_unconstrained_var = |t: &Type| matches!(t, Type::TypeVar(id)
            if *id < GENERIC_TV_BASE && *id != u32::MAX
                && !self.solved_type_vars.contains_key(id));
        let then_dynamic = is_any_val(&then_ty) || is_unconstrained_var(&then_ty);
        let else_dynamic = is_any_val(&else_ty) || is_unconstrained_var(&else_ty);

        // Arm 4 — exactly one dynamic branch: result is `AnyVal`. `types_compatible` would pick
        // the concrete branch's type, boxing the dynamic branch value at the wrong tag → crash.
        if then_dynamic != else_dynamic {
            return Type::TypeVar(u32::MAX);
        }

        // Arm 4.5 — sealed-record wins over its unsealed twin. When one branch is a sealed object
        // (`sealed=true`) and the other is the structurally identical unsealed object, prefer the
        // sealed representation so the phi does not degrade it to a boxed TAG_MAP. `sealed` is a
        // representation detail invisible to `types_compatible` (which ignores the flag), so
        // without this arm Arm 5 would pick `else_ty` (the first ⊆ check fires) regardless of
        // which side is sealed — degrading a sealed literal to boxed when the else branch is the
        // declared-return-type unsealed alias.
        match (&then_ty, &else_ty) {
            (Type::Object { fields: tf, sealed: true }, Type::Object { fields: ef, sealed: false })
                if tf == ef => return then_ty,
            (Type::Object { fields: tf, sealed: false }, Type::Object { fields: ef, sealed: true })
                if tf == ef => return else_ty,
            _ => {}
        }

        // Arm 5 — structural subtype collapse: standard path.
        if self.types_compatible(&then_ty, &else_ty) {
            return else_ty;
        }
        if self.types_compatible(&else_ty, &then_ty) {
            return then_ty;
        }

        // Arm 6 — fallback: neither is a subtype of the other.
        Type::flatten_union(vec![then_ty, else_ty])
    }

    /// Null-coalescing `left ?? right` (ADR-065). Semantically `if left != null then left else
    /// right`: `left` is evaluated exactly once and `right` only when `left` is Null. Coalesces
    /// `Null` ONLY — an `Error` member of the left type flows through unchanged (Lin's value-based
    /// error convention stays explicit).
    ///
    /// Typing: the left type must INCLUDE Null (bare `Null`, a union containing `Null`, or the
    /// dynamic `Json`) — otherwise a "left operand of `??` is never null" diagnostic. The result is
    /// `(left minus Null) | D` (D = right's type), collapsed to the bare stripped type when D is
    /// assignable to it (mirroring `object.get`, SPECIFICATION.md §6.1).
    ///
    /// We DESUGAR to `{ val tmp = left; if tmp != null then tmp else right }` at the typed-IR
    /// level (NOT in the parser — the formatter round-trips the surface `??`). This inherits the
    /// proven if/else + `!= null` lowering, ownership, and RC-reconciliation paths verbatim
    /// instead of hand-rolling union-temp RC (the #1 source of leaks/UAF here).
    pub(crate) fn infer_coalesce(&mut self, left: &Expr, right: &Expr, span: Span) -> Result<TypedExpr, Diagnostic> {
        let typed_left = self.infer_expr(left)?;
        let left_ty = typed_left.ty();

        // The left type must be able to be Null. `Json` is dynamically nullable; a bare `Null` is
        // allowed (then the value is always null and the result is just `right`'s type); a union is
        // null-inclusive iff it has a `Null` member.
        let is_bare_null = left_ty == Type::Null;
        let union_has_null = matches!(&left_ty, Type::Union(vs) if vs.iter().any(|v| *v == Type::Null));
        let left_is_json = is_any_val(&left_ty);
        if !is_bare_null && !union_has_null && !left_is_json {
            return Err(Diagnostic::error(
                left.span(),
                format!("left operand of `??` is never null (its type is {})", left_ty),
            )
            .with_help(
                "`??` supplies a default only when the left operand can be Null; here it never is, so the default is dead — drop the `?? …`",
            ));
        }

        // The non-null contribution of the left operand. For `Json` it stays `Json` (which already
        // subsumes Null and is its own normalisation below); for a `T | Null` union it is the union
        // with `Null` removed (`without_null`); for a bare `Null` left there is nothing left, so the
        // then-branch is dead and the result is purely `right`'s type — model that as `Never`.
        let stripped = if left_is_json {
            left_ty.clone()
        } else if is_bare_null {
            Type::Never
        } else {
            left_ty.without_null().unwrap_or(Type::Never)
        };

        // When the stripped left type is a concrete pushable type (not Never, not Json), try to
        // check the right operand against it so that a bare literal default (`?? {}`, `?? []`)
        // refines to the stripped type rather than inferring as an unrelated `{}` / `[]`.
        // We fall back to plain inference if the directed check fails (genuine different-shaped
        // default → documented `stripped | D` union result).
        //
        // Diagnostic-leak guard: check_expr_branch_inner only returns Err via Diagnostic return
        // (no self.diagnostics.push before Err for Object/Array arms), but snapshot defensively.
        let (typed_right, right_ty) = if !left_is_json && stripped != Type::Never {
            let diag_len_before = self.diagnostics.len();
            match self.check_expr_branch_inner(right, &stripped) {
                Ok(typed) => {
                    let ty = typed.ty();
                    (typed, ty)
                }
                Err(_) => {
                    // Discard stray diagnostics pushed before the error (defensive).
                    self.diagnostics.truncate(diag_len_before);
                    let typed = self.infer_expr(right)?;
                    let ty = typed.ty();
                    (typed, ty)
                }
            }
        } else {
            let typed = self.infer_expr(right)?;
            let ty = typed.ty();
            (typed, ty)
        };

        // Result type = stripped | D, collapsed to `stripped` when D is assignable to it. A bare
        // `Null` left collapses to `right`'s type (its stripped half is `Never`). For a `Json` left
        // the union goes through `flatten_union`, which subsumes a concrete `D` into `Json` exactly
        // as the documented `object.get`/if-merge normalisation does.
        let result_type = if is_bare_null {
            right_ty.clone()
        } else if left_is_json {
            Type::flatten_union(vec![stripped.clone(), right_ty.clone()])
        } else if self.types_compatible(&right_ty, &stripped) {
            stripped.clone()
        } else {
            Type::flatten_union(vec![stripped.clone(), right_ty.clone()])
        };

        // --- desugar to `{ val tmp = left; if tmp != null then tmp else right }` ---
        // Bind `left` to a fresh anonymous slot so it is evaluated exactly once. The synthetic name
        // can never collide with a user binding (it is not a valid identifier).
        let tmp_slot = self.env.define("$coalesce".to_string(), left_ty.clone(), false);
        // The then-branch reads the slot at its NON-NULL (stripped) type — mirroring how the real
        // `if x != null then x` narrows `x` inside the then-branch. For a bare-`Null`/Json left this
        // is `Never`/`Json` respectively; lowering coerces the branch to `result_type` regardless.
        let then_get = TypedExpr::LocalGet { slot: tmp_slot, ty: stripped.clone(), span };
        let cond = TypedExpr::BinaryOp {
            left: Box::new(TypedExpr::LocalGet { slot: tmp_slot, ty: left_ty.clone(), span }),
            op: BinOp::NotEq,
            right: Box::new(TypedExpr::NullLit(span)),
            result_type: Type::Bool,
            span,
        };
        let if_expr = TypedExpr::If {
            cond: Box::new(cond),
            then_br: Box::new(then_get),
            else_br: Box::new(typed_right),
            result_type: result_type.clone(),
            span,
        };
        Ok(TypedExpr::Block {
            stmts: vec![TypedStmt::Val {
                slot: tmp_slot,
                name: None,
                value: typed_left,
                ty: left_ty,
                span,
            }],
            expr: Box::new(if_expr),
            ty: result_type,
            span,
        })
    }

    /// Check a single `if`/`match` branch body against the expected (declared return / context)
    /// type, refining object/string literals structurally. A branch whose value is `Json`
    /// (`TypeVar(u32::MAX)`, the top/dynamic type) is accepted where any structured type is
    /// expected: `Json` is accept-any in this checked-arm position, so a function declared to
    /// return `R` may yield a `Json` value from one arm and a concrete `R`-shaped object from
    /// another. This is the bidirectional-push counterpart to the union-vs-declared-object bug;
    /// it deliberately does NOT relax `is_compatible_env` (ADR-045 still rejects a direct
    /// `val p: Person = jsonValue` decode).
    pub(crate) fn check_branch_against(&mut self, body: &Expr, expected: &Type) -> Result<TypedExpr, Diagnostic> {
        // First try the bidirectional refinement path (object/string-literal/nested if/match).
        let typed = self.check_expr_branch_inner(body, expected)?;
        let ty = typed.ty();
        if is_any_val(&ty) || self.types_compatible(&ty, expected) {
            Ok(typed)
        } else {
            Err(Diagnostic::error(
                body.span(),
                format!("Expected type {}, got {}", expected, ty),
            ))
        }
    }

    /// Infer/refine a branch body, pushing the expected type in where it helps (object literals,
    /// nested `if`/`match`, string literals) but tolerating a mismatch here — the caller
    /// (`check_branch_against`) makes the final compatibility decision (including the Json-arm
    /// allowance). We can't call `check_expr` directly because it errors on a `Json` value vs a
    /// structured object target.
    fn check_expr_branch_inner(&mut self, body: &Expr, expected: &Type) -> Result<TypedExpr, Diagnostic> {
        match body {
            Expr::Object(..) | Expr::If { .. } | Expr::Match { .. } | Expr::StringLit(..)
            | Expr::IntLit(..) | Expr::Array(..) | Expr::Block(..) | Expr::Function { .. } => {
                // These have bidirectional handling in check_expr that does not spuriously reject
                // a Json value (objects/literals refine; nested if/match recurse through this same
                // branch logic via expected_pushes_into_branches).
                self.check_expr(body, expected)
            }
            _ => self.infer_expr(body),
        }
    }

    /// Compute the COMPLEMENT narrowing for the arm at `idx`: the scrutinee type with every type
    /// definitely excluded by a guard-free `is`-arm STRICTLY BEFORE `idx` subtracted from it.
    /// Match arms are tried top-to-bottom, so any arm reached after a guard-free `is X` arm
    /// operates on a value that is not an `X`; subtracting each such `X` from the union is sound.
    ///
    /// Generalizes the old `null_excluded_before`/`without_null` pairing: `is Null` excludes
    /// `Null` (`T | Null` → `T`), `is Error` excludes the structural `Error` member
    /// (`String | Error` → `String`), `is Int32` excludes `Int32` from a numeric/mixed union,
    /// etc. Returns `None` when nothing was excluded or the subtraction is not well-defined (the
    /// excluded type is not an exact union member) — in that case the arm sees the unnarrowed
    /// scrutinee type rather than an unsound guess (see `Type::without_variant`).
    fn complement_narrowing(&self, scrutinee_ty: &Type, arms: &[MatchArm], idx: usize) -> Option<Type> {
        let mut narrowed: Option<Type> = None;
        for arm in &arms[..idx] {
            if arm.guard.is_some() {
                continue;
            }
            let Some(excluded) = self.arm_excluded_type(arm) else { continue };
            let base = narrowed.as_ref().unwrap_or(scrutinee_ty);
            if let Some(next) = base.without_variant(&excluded) {
                narrowed = Some(next);
            }
        }
        narrowed
    }

    /// The `Type` an `is`-arm definitely tests for (so a later arm can subtract it from the
    /// scrutinee union). Returns `None` for non-`is` arms, `is` arms whose pattern is not a plain
    /// type/`null`-literal check (bindings, destructuring object/array patterns, etc.), and types
    /// that fail to resolve. `is Null`/`is null` → `Null`; `is Error` → the structural Error alias
    /// `{ "type": String, "message": String }` (so it subtracts the union's Error member);
    /// `is Int32` / `is <Name>` → the resolved named/scalar type. Soundness for the resolved type
    /// is delegated to `without_variant`, which only subtracts an exactly-matching union member.
    fn arm_excluded_type(&self, arm: &MatchArm) -> Option<Type> {
        let lin_parse::ast::MatchPattern::Is(pat) = &arm.pattern else { return None };
        match pat {
            lin_parse::ast::Pattern::Literal(e) if matches!(e.as_ref(), Expr::NullLit(_)) => {
                Some(Type::Null)
            }
            lin_parse::ast::Pattern::TypeName(name, span) => {
                // `is Error` resolves to the structural Error alias; resolving "Error" via the
                // env yields exactly `error_type()`, which is the union member to subtract.
                resolve_type(
                    &lin_parse::ast::TypeExpr::Named(name.clone(), *span),
                    &self.env,
                )
                .ok()
            }
            _ => None,
        }
    }

    /// Check a `match` expression with the expected type pushed into each arm body.
    pub(crate) fn check_match(
        &mut self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        expected: &Type,
        span: Span,
    ) -> Result<TypedExpr, Diagnostic> {
        let typed_scrutinee = self.infer_expr(scrutinee)?;
        let scrutinee_ty = typed_scrutinee.ty();
        let scrutinee_name = if let Expr::Ident(name, _) = scrutinee {
            Some(name.as_str())
        } else {
            None
        };
        let mut typed_arms = Vec::new();
        let mut arm_types = Vec::new();
        // Flow-narrow the scrutinee union across arms: once a preceding guard-free `is X` arm has
        // handled (and thus excluded) the `X` case, every subsequent arm (notably `else`) sees the
        // scrutinee narrowed to the complement (`union minus X`). Generalizes the old Null-only
        // narrowing to any `is` test, incl. the structural `Error` member. See
        // `complement_narrowing`.
        for (i, arm) in arms.iter().enumerate() {
            let narrowed = self.complement_narrowing(&scrutinee_ty, arms, i);
            let typed_arm = self.check_match_arm_against(arm, &scrutinee_ty, scrutinee_name, expected, narrowed.as_ref())?;
            arm_types.push(typed_arm.body.ty());
            typed_arms.push(typed_arm);
        }
        // Prefer the (structured) `expected` as the result type — every arm was CHECKED
        // compatible against it (see the `if` counterpart for the full rationale: re-`unify`ing
        // the arm types expands a recursive sum union's `Named` children to structural `Object`,
        // which defeats lin-ir's `sum_recursive_self_name` and forces the BOXED representation).
        let result_type = if is_discriminated_sum_union(expected) && !arm_types.is_empty() {
            expected.clone()
        } else if arm_types.is_empty() {
            Type::Never
        } else {
            unify_types(&drop_empty_array_arms(&arm_types))
        };
        // Path-11: join arm inhabitant sets onto a function-typed result (see `infer_match`).
        let result_type = join_arm_lambda_sets(result_type, &arm_types);

        let exhaustiveness_diags =
            crate::exhaustiveness::check_exhaustiveness(&scrutinee_ty, &typed_arms, span);
        for d in exhaustiveness_diags {
            self.diagnostics.push(d);
        }

        Ok(TypedExpr::Match { scrutinee: Box::new(typed_scrutinee), arms: typed_arms, result_type, span })
    }

    pub(crate) fn infer_match(&mut self, scrutinee: &Expr, arms: &[MatchArm], span: Span) -> Result<TypedExpr, Diagnostic> {
        let typed_scrutinee = self.infer_expr(scrutinee)?;
        let scrutinee_ty = typed_scrutinee.ty();
        // Extract the scrutinee variable name for narrowing, if it's a simple identifier.
        let scrutinee_name = if let Expr::Ident(name, _) = scrutinee {
            Some(name.as_str())
        } else {
            None
        };
        let mut typed_arms = Vec::new();
        let mut arm_types = Vec::new();
        // Affine branch merge (streams brief §7): each arm starts from the consumed set AFTER the
        // scrutinee (the scrutinee runs once, before every arm); afterwards a Stream is consumed
        // if it was consumed in ANY arm (conservative). Mirrors `infer_if`.
        let consumed_before_arms = self.consumed_streams.clone();
        let mut consumed_union = consumed_before_arms.clone();
        // See `check_match`: narrow the scrutinee to the complement of every preceding guard-free
        // `is X` arm (the `X` cases are already handled) in arms reached after them.
        for (i, arm) in arms.iter().enumerate() {
            self.consumed_streams = consumed_before_arms.clone();
            let narrowed = self.complement_narrowing(&scrutinee_ty, arms, i);
            let typed_arm = self.check_match_arm(arm, &scrutinee_ty, scrutinee_name, narrowed.as_ref())?;
            consumed_union.extend(self.consumed_streams.iter().copied());
            arm_types.push(typed_arm.body.ty());
            typed_arms.push(typed_arm);
        }
        self.consumed_streams = consumed_union;
        let result_type = if arm_types.is_empty() { Type::Never } else { unify_types(&drop_empty_array_arms(&arm_types)) };
        // Path-11: a function-typed match (arms returning distinct lambdas) carries the JOIN of the
        // arms' inhabitant sets, since `unify_types`/PartialEq ignore `lset`.
        let result_type = join_arm_lambda_sets(result_type, &arm_types);

        // Exhaustiveness check: emit diagnostics but don't fail — warnings stay as warnings,
        // errors are collected alongside other diagnostics and reported together.
        let exhaustiveness_diags = crate::exhaustiveness::check_exhaustiveness(
            &scrutinee_ty,
            &typed_arms,
            span,
        );
        for d in exhaustiveness_diags {
            self.diagnostics.push(d);
        }

        Ok(TypedExpr::Match { scrutinee: Box::new(typed_scrutinee), arms: typed_arms, result_type, span })
    }

    pub(crate) fn infer_block(&mut self, stmts: &[Stmt], final_expr: &Expr, span: Span) -> Result<TypedExpr, Diagnostic> {
        self.env.push_scope();
        // Forward-declare any function-literal `val` bindings in this block so they can refer
        // to each other regardless of definition order (hoisting, mirrors module-level ADR-012).
        self.forward_declare_functions_in(stmts);
        let mut typed_stmts = Vec::new();
        let block_tail = self.in_tail_position;
        self.in_tail_position = false;
        for stmt in stmts {
            match self.check_stmt(stmt) {
                Ok(ts) => typed_stmts.push(ts),
                Err(diag) => { self.env.pop_scope(); return Err(diag); }
            }
        }
        self.in_tail_position = block_tail;
        let typed_final = self.infer_expr(final_expr)?;
        let ty = typed_final.ty();
        self.env.pop_scope();
        Ok(TypedExpr::Block { stmts: typed_stmts, expr: Box::new(typed_final), ty, span })
    }

    pub(crate) fn infer_object(&mut self, fields: &[ObjectField], span: Span) -> Result<TypedExpr, Diagnostic> {
        // Detect whether this is an integer-keyed map literal (§5.1.1).
        // A bare integer literal key (`1:`, `-1:`, `42:`) unambiguously signals `{ Int: T }`.
        // Negative literals arrive as BinaryOp(0 - v) from the parser's Minus prefix rule.
        let first_int_key = fields.iter().find_map(|f| {
            if let ObjectField::Pair(k, _) = f { extract_int_key(k) } else { None }
        });
        let first_str_key = fields.iter().any(|f| {
            matches!(f, ObjectField::Pair(Expr::StringLit(..), _))
        });
        if first_int_key.is_some() {
            // All keys must be integer literals — mixing string and int keys is a type error.
            if first_str_key {
                return Err(Diagnostic::error(
                    span,
                    "mixed key types in object literal: cannot mix integer keys and string keys \
                     (use all integer keys for `{ Int: T }` or all string keys for `{ String: T }`)",
                ));
            }
            return self.infer_int_map_literal(fields, span);
        }
        let mut typed_fields = Vec::new();
        let mut spreads = Vec::new();
        let mut obj_type = IndexMap::new();
        // An object literal's field values are not in tail position (see `check_object_fields`):
        // clear the flag so a self-recursive call in a field isn't mis-marked a tail call.
        let saved_tail = self.in_tail_position;
        self.in_tail_position = false;
        for field in fields {
            match field {
                ObjectField::Pair(key_expr, val_expr) => {
                    let typed_val = self.infer_expr(val_expr)?;
                    let val_ty = typed_val.ty();
                    // Placement restriction (streams brief §8): a Stream may not live in an object
                    // field — confining the affine move-checker to local bindings (no container
                    // linearity). Hard ERROR.
                    if type_is_streamish(&val_ty) {
                        return Err(Diagnostic::error(
                            val_expr.span(),
                            "a Stream cannot be stored in an object field — keep it in a `val` \
                             binding (a Stream is an affine resource; v1 confines it to local bindings)",
                        ));
                    }
                    if let Expr::StringLit(key, _) = key_expr {
                        obj_type.insert(key.clone(), val_ty);
                        typed_fields.push((key.clone(), typed_val));
                    }
                }
                ObjectField::Spread(expr) => {
                    let typed_spread = self.infer_expr(expr)?;
                    if let Type::Object { ref fields, .. } = typed_spread.ty() {
                        for (k, v) in fields { obj_type.insert(k.clone(), v.clone()); }
                    }
                    spreads.push(typed_spread);
                }
            }
        }
        self.in_tail_position = saved_tail;
        // Object literal → anonymous structural type → UNSEALED.
        Ok(TypedExpr::MakeObject { fields: typed_fields, spreads, ty: Type::object(obj_type), span })
    }

    /// Infer an integer-keyed map literal `{ 1: v, -1: w, 42: x }`.
    /// Keys are stored as their decimal string representation in `TypedExpr::MakeObject.fields`
    /// so the existing IR/codegen machinery is reused; codegen dispatches to `lin_map_set_int`
    /// when the map type has an integer key (it already allocates with KEY_KIND_INT).
    fn infer_int_map_literal(&mut self, fields: &[ObjectField], span: Span) -> Result<TypedExpr, Diagnostic> {
        let saved_tail = self.in_tail_position;
        self.in_tail_position = false;
        let mut typed_fields: Vec<(String, TypedExpr)> = Vec::new();
        let mut value_types: Vec<Type> = Vec::new();
        for field in fields {
            match field {
                ObjectField::Pair(key_expr, val_expr) => {
                    let key_int = extract_int_key(key_expr).ok_or_else(|| {
                        Diagnostic::error(
                            key_expr.span(),
                            "integer-keyed map literals must use integer literal keys",
                        )
                    })?;
                    let typed_val = self.infer_expr(val_expr)?;
                    let val_ty = typed_val.ty();
                    if type_is_streamish(&val_ty) {
                        return Err(Diagnostic::error(
                            val_expr.span(),
                            "a Stream cannot be stored in a map value — keep it in a `val` binding",
                        ));
                    }
                    value_types.push(val_ty);
                    typed_fields.push((key_int.to_string(), typed_val));
                }
                ObjectField::Spread(_) => {
                    return Err(Diagnostic::error(
                        span,
                        "spread syntax is not supported in integer-keyed map literals",
                    ));
                }
            }
        }
        self.in_tail_position = saved_tail;
        let value_ty = unify_types(&value_types);
        let key_ty = Type::Int32;
        Ok(TypedExpr::MakeObject {
            fields: typed_fields,
            spreads: Vec::new(),
            ty: Type::Map { key: Box::new(key_ty), value: Box::new(value_ty) },
            span,
        })
    }

    /// Bidirectional refinement for an object literal against an expected type (ADR-034).
    ///
    /// Returns `Ok(Some(_))` when it produced a refined typed object; `Ok(None)` to defer to
    /// ordinary inference (e.g. the expected type is not object-shaped, or the literal contains
    /// spreads, which the refinement path does not narrow). Only fires when the expected type
    /// actually carries a `StrLit` field somewhere — otherwise it defers, leaving non-literal
    /// behaviour exactly as before.
    fn check_object_against(
        &mut self,
        fields: &[ObjectField],
        expected: &Type,
        span: Span,
    ) -> Result<Option<TypedExpr>, Diagnostic> {
        // Spreads are not refined (their static shape is opaque here) — defer.
        if fields.iter().any(|f| matches!(f, ObjectField::Spread(_))) {
            return Ok(None);
        }
        match expected {
            // Unfold a non-generic Named alias one level and retry.
            Type::Named(n) => {
                if let Some(decl) = self.env.lookup_type(n) {
                    if decl.params.is_empty() {
                        let body = decl.body.clone();
                        // Guard against a self-referential body. A named record type's stored body
                        // can be the opaque self-reference `Named(n)` itself (the cycle sentinel a
                        // recursive type resolves to, and — pre-existing — for some record shapes
                        // whose declared body never got re-expanded). Unfolding that and retrying
                        // would loop forever (stack overflow). Defer to ordinary inference instead.
                        if matches!(&body, Type::Named(b) if b == n) {
                            return Ok(None);
                        }
                        return self.check_object_against(fields, &body, span);
                    }
                }
                Ok(None)
            }
            Type::Object { fields: expected_fields, sealed } => {
                // Integer-literal-keyed object literal against a fixed record whose keys are all
                // decimal digit strings (produced by expanding `{ DayOfWeek: Boolean }` where
                // `DayOfWeek = 0|1|...|6`). Treat each integer key as its decimal string form and
                // check each value against the corresponding expected field type.
                let all_int_keys = !fields.is_empty()
                    && fields.iter().all(|f| matches!(f, ObjectField::Pair(k, _) if extract_int_key(k).is_some()));
                let expected_all_digit_keys = !expected_fields.is_empty()
                    && expected_fields.keys().all(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_digit()));
                if all_int_keys && expected_all_digit_keys {
                    let mut typed_fields = Vec::new();
                    for field in fields {
                        if let ObjectField::Pair(key_expr, val_expr) = field {
                            let n = extract_int_key(key_expr).unwrap();
                            let key_str = n.to_string();
                            let expected_val_ty = expected_fields.get(&key_str)
                                .cloned()
                                .unwrap_or_else(|| self.env.fresh_type_var());
                            let typed_val = self.check_expr(val_expr, &expected_val_ty)?;
                            typed_fields.push((key_str, typed_val));
                        }
                    }
                    let ty = Type::Object {
                        fields: expected_fields.clone(),
                        sealed: *sealed,
                    };
                    return Ok(Some(TypedExpr::MakeObject {
                        fields: typed_fields,
                        spreads: Vec::new(),
                        ty,
                        span,
                    }));
                }
                // Take over with directed field-by-field checking when it would actually change
                // the outcome — otherwise stay on the existing undirected inference path so plain
                // structural objects are unaffected. Cases that need directing:
                //   (1) a `StrLit` field — so a discriminant literal narrows to its singleton;
                //   (2) a `Map` field (possibly nested inside a further record field) — so an
                //       object literal in that field position key-widens to `{ String: T }`
                //       (a `LinMap`) instead of being inferred to its own fixed-record type;
                //   (3) THIS expected type is itself a sealed record, or a (nested) field is a
                //       sealed record(-array) — so the producer adopts the sealed element type and
                //       builds the same packed/sealed representation the consumer reads back at
                //       (Path-9C seal-propagation symmetry — see `expected_field_needs_directing`).
                if !*sealed && !expected_fields.values().any(|t| expected_field_needs_directing(t)) {
                    return Ok(None);
                }
                // OMISSION GUARD (§5.9.1 soundness): the directed path only checks the fields the
                // literal actually carries — it would silently accept a literal that OMITS a
                // required field (one whose expected type does not admit `Null`). The undirected
                // inference + structural compatibility path catches that and reports the proper
                // "Expected type … got …" diagnostic, so DEFER to it whenever a required field is
                // absent. (Extending directing to sealed records — every plain all-scalar named
                // record is sealed — made this reachable for shapes like `Point = {x,y}` that
                // previously always deferred; without the guard, `val p: Point = { x: 1 }` would
                // wrongly type-check. A missing field whose type INCLUDES `Null` stays directed:
                // that is a permitted omission and the result type is unchanged from inference.)
                let present: std::collections::HashSet<&str> = fields
                    .iter()
                    .filter_map(|f| match f {
                        ObjectField::Pair(Expr::StringLit(k, _), _) => Some(k.as_str()),
                        _ => None,
                    })
                    .collect();
                let omits_required = expected_fields.iter().any(|(k, ft)| {
                    !present.contains(k.as_str())
                        && !crate::compat::is_compatible(&Type::Null, ft)
                });
                if omits_required {
                    return Ok(None);
                }
                Ok(Some(self.check_object_fields(fields, expected_fields, *sealed, span)?))
            }
            // An object literal checked against a typed index-signature map `{ K: V }`
            // (ADR-055 + numeric-key extension): each literal value must be `V`; the result is
            // typed `Map{K,V}` and lowered into a `LinMap`. The empty `{}` literal is the common
            // case (`var m: { String: T } = {}`), which produces an empty hashed map of the right
            // type — this is how `{}` infers a map from its assignment-target / return-type context.
            Type::Map { key: map_key_ty, value: val_ty } => {
                let mut typed_fields = Vec::new();
                if map_key_ty.is_integer() {
                    // Integer-keyed map: each key must be an integer literal.
                    for field in fields {
                        match field {
                            ObjectField::Pair(key_expr, val_expr) => {
                                let key_int = match extract_int_key(key_expr) {
                                    Some(k) => k,
                                    None => return Ok(None),
                                };
                                let typed_val = self.check_expr(val_expr, val_ty)?;
                                typed_fields.push((key_int.to_string(), typed_val));
                            }
                            ObjectField::Spread(_) => return Ok(None),
                        }
                    }
                } else {
                    for field in fields {
                        if let ObjectField::Pair(Expr::StringLit(key, _), val_expr) = field {
                            let typed_val = self.check_expr(val_expr, val_ty)?;
                            typed_fields.push((key.clone(), typed_val));
                        } else {
                            // A non-literal key or a dynamic field shape — defer to ordinary inference.
                            return Ok(None);
                        }
                    }
                }
                Ok(Some(TypedExpr::MakeObject {
                    fields: typed_fields,
                    spreads: Vec::new(),
                    ty: Type::Map { key: map_key_ty.clone(), value: val_ty.clone() },
                    span,
                }))
            }
            Type::Union(variants) => {
                // Discriminant selection: find the variant whose literal-typed field matches a
                // matching literal field in the object. Only consider variants that have a
                // discriminant (a StrLit field) — these are the tagged-union cases.
                let literal_variants: Vec<&IndexMap<String, Type>> = variants
                    .iter()
                    .filter_map(|v| match v {
                        Type::Object { fields: f, .. } if f.values().any(|t| matches!(t, Type::StrLit(_))) => Some(f),
                        _ => None,
                    })
                    .collect();
                if literal_variants.is_empty() {
                    return Ok(None);
                }
                // Collect the object literal's string-literal field values for matching.
                let lit_field_value = |key: &str| -> Option<String> {
                    for f in fields {
                        if let ObjectField::Pair(k, v) = f {
                            if let (Expr::StringLit(kk, _), Expr::StringLit(vv, _)) = (k, v) {
                                if kk == key {
                                    return Some(vv.clone());
                                }
                            }
                        }
                    }
                    None
                };
                // Pick the first variant all of whose StrLit fields are matched by the literal.
                let chosen = literal_variants.iter().find(|vf| {
                    vf.iter().all(|(k, t)| match t {
                        Type::StrLit(want) => lit_field_value(k).as_deref() == Some(want.as_str()),
                        _ => true,
                    })
                });
                match chosen {
                    // A union variant selected by its discriminant is a named record variant; the
                    // chosen field map came from a `Type::Object` variant whose seal flag is not
                    // threaded through `literal_variants` (it only kept the field maps). These
                    // tagged-union variants are not packed as sealed scalar arrays, so directing
                    // them UNSEALED preserves existing behaviour (the discriminant still narrows).
                    Some(vf) => Ok(Some(self.check_object_fields(fields, vf, false, span)?)),
                    None => {
                        // No variant matched: report the valid discriminant tags.
                        let mut tags = Vec::new();
                        for vf in &literal_variants {
                            for t in vf.values() {
                                if let Type::StrLit(s) = t {
                                    tags.push(format!("\"{}\"", s));
                                }
                            }
                        }
                        tags.sort();
                        tags.dedup();
                        Err(Diagnostic::error(
                            span,
                            format!(
                                "Object does not match any variant of {}; expected a discriminant tag in [{}]",
                                expected,
                                tags.join(", ")
                            ),
                        ))
                    }
                }
            }
            _ => Ok(None),
        }
    }

    /// Check each object-literal field against the matching expected field type, narrowing
    /// literal-typed fields. The resulting object type uses the expected field types where a
    /// field is present (preserving `StrLit` singletons), so the whole object is assignable to
    /// the expected (object or selected union variant) type.
    fn check_object_fields(
        &mut self,
        fields: &[ObjectField],
        expected_fields: &IndexMap<String, Type>,
        sealed: bool,
        span: Span,
    ) -> Result<TypedExpr, Diagnostic> {
        let mut typed_fields = Vec::new();
        let mut obj_type = IndexMap::new();
        // An object LITERAL is never itself a tail call, and none of its field values are in
        // tail position — even when the literal is the tail expression of a function (an
        // `if`/`match` branch value). A self-recursive call in a field (`{ left: chain(n-1) }`,
        // the canonical recursive-constructor shape) would otherwise inherit the enclosing tail
        // flag and be mis-marked a tail call → codegen's TCO transform would loop it back,
        // DISCARDING the surrounding node construction (the recursive child never materializes).
        // Clear the flag for the field values and restore it after.
        let saved_tail = self.in_tail_position;
        self.in_tail_position = false;
        for field in fields {
            if let ObjectField::Pair(key_expr, val_expr) = field {
                if let Expr::StringLit(key, _) = key_expr {
                    let typed_val = match expected_fields.get(key) {
                        Some(ft) => self.check_expr(val_expr, ft)?,
                        None => self.infer_expr(val_expr)?,
                    };
                    // When building a SEALED record and the expected field type is a sum-type union
                    // (which maps to a single *SumNode pointer at runtime), record the EXPECTED type
                    // rather than the value's narrowed variant type. Without this, checking
                    // `{ "node": { "kind": "num", "value": 42 } }` against `{ "node": Expr, "pos": Int32 }`
                    // would record `node: unsealed_variant_obj` (the matched variant type), which
                    // `is_sealed_heap_field()` rejects — preventing the outer Cursor from being sealed.
                    // Using the expected sum type is sound: the value is compatible with it (the
                    // check above already enforced that), and codegen's `sealed_construct` emits a
                    // `compile_ir_coerce(val, from=variant_obj, to=Expr)` → `sumnode_project_from_boxed`
                    // to project the variant into the correct *SumNode layout.
                    let field_ty = match expected_fields.get(key) {
                        Some(ft) if Type::sum_type_eligible(ft)
                            && self.types_compatible(&typed_val.ty(), ft) =>
                        {
                            ft.clone()
                        }
                        _ => typed_val.ty(),
                    };
                    obj_type.insert(key.clone(), field_ty);
                    typed_fields.push((key.clone(), typed_val));
                }
            }
        }
        self.in_tail_position = saved_tail;
        // Carry the EXPECTED type's seal flag onto the refined literal's own type (Path-9C) — but
        // ONLY when every field is sealed-eligible. This is the producer/consumer SEAL SYMMETRY fix:
        // recording the literal SEALED makes the PRODUCER build the same packed representation the
        // CONSUMER reads back at, keeping the two sides' repr classification (and codegen layout) in
        // agreement. `is_sealed_field` is the canonical sealed-record gate (scalar + Bool + String +
        // Array + Map + nested sealed record + sum-type pointer). Sealed-ARRAY eligibility
        // (`is_sealed_array_field_packable`) is a stricter sub-gate: only fields whose type allows
        // a packed sealed-array element (not sum-type pointers yet — pack_named_payload_impl lacks
        // NKIND_SUMNODE support). The sealed flag on the LITERAL drives whether the standalone record
        // is a packed struct; the sealed-ARRAY gate is checked separately by `sealed_array_elem` when
        // the record type appears as an array element. Keeping both gates in sync:
        //   - A record with only sealed-array-packable fields → sealed literal AND sealed array elem.
        //   - A record with a sum-type pointer field (e.g. Cursor{node:Ast, pos:Int32}) → sealed
        //     literal (no map alloc, field reads are const-offset), but Cursor[] stays a boxed array
        //     (sealed_array_elem returns None). No pack_named_payload_impl NKIND_SUMNODE path taken.
        // `Type` equality/subtyping ignores the seal flag, so this is representation-only and never
        // changes assignability. A field directed only because of a nested `StrLit`/`Map` (the outer
        // literal itself unsealed) keeps the historical UNSEALED result.
        //
        // Two-gate rule for auto-seal:
        //   (A) ALL fields pass `is_sealed_field()` — the necessary condition.
        //   (B) EITHER `sealed=true` was passed by the caller (the expected type was already
        //       sealed, so the producer/consumer repr is already coordinated) OR at least one
        //       field is `sum_type_eligible` (a SumNode-pointer field like Cursor.node — the new
        //       capability this PR adds, not covered by the old packability gate).
        //
        // Gate B guards against spurious auto-sealing of types like Journey/Trip/TimetableLeg
        // (whose fields are Array/String/Int32, all passing gate A) that the CONSUMER reads as
        // boxed TAG_MAP. If the consumer path was built without `sealed=true` on the expected
        // type, it never set up packed-struct readers, so a sealed producer would mismatch →
        // segfault. The SumNode exemption is safe: SumNode-pointer records are always consumed
        // via the sealed-record path (codegen checks `is_sum_type` and routes to SumNode reads),
        // so no consumer/producer repr mismatch arises.
        let all_fields_sealed =
            !obj_type.is_empty() && obj_type.values().all(|t| t.is_sealed_field());
        let has_sum_field = obj_type.values().any(|t| Type::sum_type_eligible(t));
        let ty = if all_fields_sealed && (sealed || has_sum_field) {
            // Use the EXPECTED field order for the sealed struct descriptor, not the literal's
            // field order. The sealed struct's byte layout is determined by the type's field order,
            // and the consumer (field reads, function return coerces) uses the declared type's
            // order. A literal like `{ "departureTime": x, "legs": y }` checked against
            // `Journey = { "legs": Leg[], "departureTime": Int32 }` must produce a struct with
            // `legs` at the first slot (matching Journey's declared order), not `departureTime`.
            // Without this, the producer writes `departureTime` at offset 24 but the consumer
            // reads `legs` (a pointer) from offset 24 → mis-read → crash.
            let ordered: IndexMap<String, Type> = expected_fields.keys()
                .filter_map(|k| obj_type.get(k).map(|v| (k.clone(), v.clone())))
                .chain(obj_type.iter()
                    .filter(|(k, _)| !expected_fields.contains_key(k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone())))
                .collect();
            Type::sealed_object(ordered)
        } else {
            Type::object(obj_type)
        };
        Ok(TypedExpr::MakeObject { fields: typed_fields, spreads: Vec::new(), ty, span })
    }

    pub(crate) fn infer_array(&mut self, elements: &[Expr], span: Span) -> Result<TypedExpr, Diagnostic> {
        let typed_elements: Result<Vec<_>, _> = elements.iter().map(|e| self.infer_expr(e)).collect();
        let typed_elements = typed_elements?;
        let elem_types: Vec<Type> = typed_elements.iter().map(|t| t.ty()).collect();
        // Placement restriction (streams brief §8): a Stream may not live in an array element.
        if let Some((i, _)) = elem_types.iter().enumerate().find(|(_, t)| type_is_streamish(t)) {
            return Err(Diagnostic::error(
                elements[i].span(),
                "a Stream cannot be stored in an array element — keep it in a `val` binding \
                 (a Stream is an affine resource; v1 confines it to local bindings)",
            ));
        }
        let ty = if elem_types.is_empty() {
            Type::Array(Box::new(Type::Never))
        } else {
            Type::Array(Box::new(unify_types(&elem_types)))
        };
        Ok(TypedExpr::MakeArray { elements: typed_elements, ty, span })
    }

    pub(crate) fn infer_assign(&mut self, target: &str, value: &Expr, span: Span) -> Result<TypedExpr, Diagnostic> {
        let (var_scope_depth, info) = self.env.lookup_with_depth(target).ok_or_else(|| {
            Diagnostic::error(span, format!("Undefined variable '{}'", target))
        })?;
        if !info.mutable {
            return Err(Diagnostic::error(span, format!("Cannot assign to immutable binding '{}'", target)));
        }
        let expected_ty = info.ty.clone();
        let slot = info.slot;
        let def_span = info.def_span;
        let is_mutable = info.mutable;
        // Register as a capture in every enclosing function where the variable is defined
        // in a strictly outer scope (same multi-level propagation as infer_ident).
        if var_scope_depth > 0 {
            for (i, &fn_entry_depth) in self.function_scope_depths.iter().enumerate().rev() {
                if var_scope_depth < fn_entry_depth {
                    if let Some(captures) = self.capture_stack.get_mut(i) {
                        captures.entry(slot).or_insert_with(|| Capture {
                            name: target.to_string(),
                            outer_slot: slot,
                            is_mutable,
                            ty: expected_ty.clone(),
                        });
                    }
                } else {
                    break;
                }
            }
        }
        let typed_value = self.check_expr(value, &expected_ty)?;
        self.span_type_map.push((span, expected_ty.to_string(), def_span));
        self.env.clear_narrowing(target);
        // Reassigning `target` invalidates any active index-narrowing whose path mentions it —
        // whether as the root object (`target[..]`, `target["a"][k]`) or as a key (`m[target]`) —
        // since the path may denote a possibly-different value now.
        self.clear_index_narrowings_for(target);
        Ok(TypedExpr::LocalSet { slot, value: Box::new(typed_value), ty: expected_ty, span })
    }

    pub(crate) fn infer_index_assign(&mut self, object: &Expr, key: &Expr, value: &Expr, span: Span) -> Result<TypedExpr, Diagnostic> {
        let typed_obj = self.infer_expr(object)?;
        let typed_key = self.infer_expr(key)?;
        // A write `obj[..] = ..` (where `obj` may itself be a compound place like `rec["a"]`)
        // invalidates any active index-narrowing rooted at the same identifier — the value at any
        // path under that root may have changed (e.g. `if m[k] == null then m[k] = []` followed by
        // a read, or a write through `rec["a"][k] = v` aliasing a narrowed `rec["a"][j]`).
        // Conservatively keyed on the ROOT identifier: clearing all narrowings under that root is
        // sound (over-clearing only re-widens), and avoids reasoning about index aliasing.
        if let Some(root) = expr_place_root(object) {
            self.clear_index_narrowings_for(root);
        }
        let obj_ty = typed_obj.ty();
        let typed_value = match &obj_ty {
            Type::Object { fields, .. } => {
                if let TypedExpr::StringLit(ref key_str, _, _) = typed_key {
                    if let Some(field_ty) = fields.get(key_str) {
                        self.check_expr(value, field_ty)?
                    } else {
                        self.infer_expr(value)?
                    }
                } else {
                    self.infer_expr(value)?
                }
            }
            // Typed index-signature map `{ K: V }` (ADR-055 + numeric-key extension): the key must
            // be compatible with `K` and the value must be `V`.
            Type::Map { key: map_key_ty, value: val_ty } => {
                let key_ty = typed_key.ty();
                let key_ok = if map_key_ty.is_integer() {
                    key_ty.is_integer() || matches!(key_ty, Type::TypeVar(_))
                } else {
                    key_ty.is_string_ish() || matches!(key_ty, Type::TypeVar(_))
                };
                if !key_ok {
                    return Err(Diagnostic::error(
                        span,
                        format!("a `{}` is keyed by `{}`, but the key is `{}`", obj_ty, map_key_ty, key_ty),
                    ));
                }
                self.check_expr(value, val_ty)?
            }
            Type::Array(elem) => self.check_expr(value, elem)?,
            Type::FixedArray(elems) => {
                if let TypedExpr::IntLit(idx, _, _) = typed_key {
                    if let Some(elem_ty) = elems.get(idx as usize) {
                        self.check_expr(value, elem_ty)?
                    } else {
                        self.infer_expr(value)?
                    }
                } else {
                    self.infer_expr(value)?
                }
            }
            Type::TypeVar(_) | Type::Union(_) | Type::Null => self.infer_expr(value)?,
            _ => return Err(Diagnostic::error(span, format!("Cannot assign into type {}", obj_ty))),
        };
        Ok(TypedExpr::IndexSet {
            object: Box::new(typed_obj),
            key: Box::new(typed_key),
            value: Box::new(typed_value),
            obj_ty,
            span,
        })
    }

    pub(crate) fn infer_string_interp(&mut self, parts: &[StringPart], span: Span) -> Result<TypedExpr, Diagnostic> {
        let mut typed_parts = Vec::new();
        for part in parts {
            match part {
                StringPart::Literal(s) => typed_parts.push(TypedStringPart::Literal(s.clone())),
                StringPart::Expr(e) => typed_parts.push(TypedStringPart::Expr(self.infer_expr(e)?)),
            }
        }
        Ok(TypedExpr::StringInterp { parts: typed_parts, span })
    }
}

/// True if `ty` contains a `StrLit` singleton anywhere in its structure. Used to scope the
/// bidirectional literal refinement (ADR-034) so the if/block expected-type propagation only
/// fires for literal-typed targets, leaving all other inference behaviour unchanged.
/// True when the expected type is one we want pushed into `if`/`match` branch bodies for
/// bidirectional checking: a structured object, a named (alias) type, a union, or anything that
/// mentions a `StrLit` singleton (ADR-034). Plain scalars / arrays / iterators / `Json` keep the
/// old inference-then-unify path (pushing into them buys nothing and risks behaviour changes).
pub(crate) fn expected_pushes_into_branches(ty: &Type) -> bool {
    match ty {
        Type::Object { .. } | Type::Named(_) | Type::Union(_) => true,
        _ => expected_pushes_scalar_width(ty) || type_mentions_strlit(ty),
    }
}

/// True for a concrete scalar type whose *width* must be pushed into branch / block-tail
/// positions so a suffixless numeric literal there adopts it instead of the default
/// (`Int32` / `Float64`). Covers every non-default sized integer and `Float32`. Without this,
/// `(): Int64 => if c then 28 else 31` infers the literals as `Int32` and codegen emits an `i32`
/// value into an `i64`-returning function (invalid IR / "ret i32 ... i64"). `Int32` / `Float64`
/// are the literal defaults, so pushing them buys nothing and is omitted (keeps the old path).
pub(crate) fn expected_pushes_scalar_width(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Int8
            | Type::Int16
            | Type::Int64
            | Type::UInt8
            | Type::UInt16
            | Type::UInt32
            | Type::UInt64
            | Type::Float32
    )
}

/// True if `ty` is the `AnyVal` dynamic top type (`TypeVar(u32::MAX)`). A value of this type is
/// accept-any in checked branch/arm position (see `check_branch_against`).
pub(crate) fn is_any_val(ty: &Type) -> bool {
    matches!(ty, Type::TypeVar(n) if *n == u32::MAX)
}

/// Drop empty-array (`Array(Never)`) members from a set of branch/arm types when at least one
/// NON-empty-array member is present. An `[]` arm (`match x is Null => [] else => xs`, or the `if`
/// counterpart) is the bottom array type; it carries no element information and must not pollute or
/// win the merged union — the real arm (`xs: Neighbor[]`) carries the element type. Without this the
/// union keeps a `Never[]` member, and iterating it (`.for`) lowers the element at a bogus repr.
/// If EVERY member is an empty array (or there are none), the list is returned unchanged.
fn drop_empty_array_arms(arm_types: &[Type]) -> Vec<Type> {
    let is_empty_arr = |t: &Type| matches!(t, Type::Array(e) if matches!(**e, Type::Never));
    if arm_types.iter().any(|t| !is_empty_arr(t)) && arm_types.iter().any(is_empty_arr) {
        arm_types.iter().filter(|t| !is_empty_arr(t)).cloned().collect()
    } else {
        arm_types.to_vec()
    }
}

/// True if `ty` IS a `Stream` or a `Union` that includes a `Stream` variant. The source
/// intrinsics return `Stream<…> | Error`, so a binding's static type is usually the union; the
/// affine/placement checks must treat the union-with-Stream the same as a bare Stream (a stream
/// pipeline `.lines()` narrows the union to the Stream variant at the dot-call boundary).
pub(crate) fn type_is_streamish(ty: &Type) -> bool {
    match ty {
        Type::Stream(_) => true,
        Type::Union(variants) => variants.iter().any(type_is_streamish),
        _ => false,
    }
}

/// True if an expected field type warrants the directed object-checking path (so an object
/// literal in that field position is checked AGAINST the type rather than freely inferred).
/// This is the gate for `check_object_against`'s `Type::Object` arm. It fires when the type is
/// — or transitively (in a record-field position) contains — any of:
///   - a `StrLit` singleton (so a discriminant narrows);
///   - a `Map` (so a record literal key-widens to `{ String: T }`);
///   - a SEALED record, or an `Array`/`FixedArray` whose element is (transitively) a sealed
///     record (Path-9C seal-propagation symmetry). Directing the literal against a sealed
///     record(-array) field makes the PRODUCER adopt the sealed element type, so the
///     `MakeArray`/`MakeObject` it builds carries the same sealed representation the CONSUMER
///     reads it back at (`trip["stopTimes"]` whose `result_ty` is the sealed `StopTime[]`).
///     Without this the field falls to UNDIRECTED inference → an UNSEALED `Object[]`, and a
///     producer/consumer representation divergence (a silent mis-read for scalar fields today;
///     a misaligned-pointer crash once heap-field packing widens the gate).
/// The transitive walk handles nested records like `{ headers: { String: String } }` (Map) and
/// `{ stopTimes: StopTime[] }` (sealed-record array) where the outer record has no DIRECT
/// directing field but a nested one does.
pub(crate) fn expected_field_needs_directing(ty: &Type) -> bool {
    match ty {
        Type::StrLit(_) | Type::Map { .. } => true,
        Type::Object { sealed: true, .. } => true,
        Type::Object { fields, sealed: false } => fields.values().any(expected_field_needs_directing),
        Type::Array(elem) | Type::Iterator(elem) | Type::Shared(elem) | Type::Stream(elem) => {
            expected_field_needs_directing(elem)
        }
        Type::FixedArray(elems) | Type::Union(elems) => elems.iter().any(expected_field_needs_directing),
        _ => false,
    }
}

/// True when `ty` is a DISCRIMINATED sum union: a `Union` of ≥2 record variants where every
/// variant carries a `StrLit` discriminant field. This is the (checker-side, conservative)
/// recognizer for the type shape lin-ir packs as an unboxed `SumNode` — used to decide when an
/// `if`/`match` checked against this type should adopt it AS the result type (preserving any
/// `Named` recursive-child markers the structural re-unify would erase, which lin-ir needs to
/// keep the construction unboxed). Deliberately narrow: a plain object / sealed record does NOT
/// match, so the existing structural-unify result type — and the optimisations keyed off it —
/// stay unchanged for those.
pub(crate) fn is_discriminated_sum_union(ty: &Type) -> bool {
    let Type::Union(variants) = ty else { return false };
    if variants.len() < 2 {
        return false;
    }
    variants.iter().all(|v| match v {
        Type::Object { fields, .. } => fields.values().any(|t| matches!(t, Type::StrLit(_))),
        _ => false,
    })
}

pub(crate) fn type_mentions_strlit(ty: &Type) -> bool {
    match ty {
        Type::StrLit(_) => true,
        Type::Array(inner) | Type::Iterator(inner) | Type::Shared(inner) | Type::Stream(inner) | Type::Promise(inner) => type_mentions_strlit(inner),
        Type::FixedArray(elems) => elems.iter().any(type_mentions_strlit),
        Type::Union(variants) => variants.iter().any(type_mentions_strlit),
        Type::Object { fields, .. } => fields.values().any(type_mentions_strlit),
        Type::Function { params, ret, .. } => {
            params.iter().any(type_mentions_strlit) || type_mentions_strlit(ret)
        }
        _ => false,
    }
}

/// Path-11: the lambda set carried at the TOP LEVEL of `ty` if it is a function type, else `None`
/// (a non-function branch contributes nothing to a function-typed merge's inhabitant set).
fn top_level_lambda_set(ty: &Type) -> Option<crate::types::LambdaSet> {
    match ty {
        Type::Function { lset, .. } => Some(lset.clone()),
        _ => None,
    }
}

/// Path-11: if `ty` is a function type and at least one branch contributed a set, stamp it with the
/// JOIN of the two branch sets. A missing branch set (non-function branch) is treated as `Top` — a
/// merge of a function with a non-function can only be reasoned about as ⊤. Inert: only the `lset`
/// metadata changes; params/ret/required are untouched.
fn with_joined_lambda_set(
    ty: Type,
    then_lset: &Option<crate::types::LambdaSet>,
    else_lset: &Option<crate::types::LambdaSet>,
) -> Type {
    use crate::types::LambdaSet;
    if let Type::Function { params, ret, required, .. } = ty {
        let a = then_lset.clone().unwrap_or(LambdaSet::Top);
        let b = else_lset.clone().unwrap_or(LambdaSet::Top);
        Type::Function { params, ret, required, lset: a.join(&b) }
    } else {
        ty
    }
}

/// Path-11: if `result` is a function type, stamp it with the JOIN of every arm's top-level lambda
/// set. A non-function arm (or an arm with no set) contributes `Top`. Inert metadata only.
fn join_arm_lambda_sets(result: Type, arm_types: &[Type]) -> Type {
    use crate::types::LambdaSet;
    if let Type::Function { params, ret, required, .. } = result {
        let mut joined = LambdaSet::Known(vec![]);
        for at in arm_types {
            let s = top_level_lambda_set(at).unwrap_or(LambdaSet::Top);
            joined = joined.join(&s);
        }
        Type::Function { params, ret, required, lset: joined }
    } else {
        result
    }
}

/// Replace every GENERIC (quantified, non-Json-wildcard) `TypeVar` in `ty` with the `Json` wildcard.
/// Used to erase the unconstrained tuple element type of an empty array literal flowing into a
/// `[String, T][]` param, so the empty container monomorphizes instead of leaving `T` unsolved.
pub(crate) fn erase_generic_type_vars(ty: &Type) -> Type {
    match ty {
        Type::TypeVar(id) if *id != u32::MAX => Type::TypeVar(u32::MAX),
        Type::Array(t) => Type::Array(Box::new(erase_generic_type_vars(t))),
        Type::Iterator(t) => Type::Iterator(Box::new(erase_generic_type_vars(t))),
        Type::Shared(t) => Type::Shared(Box::new(erase_generic_type_vars(t))),
        Type::Stream(t) => Type::Stream(Box::new(erase_generic_type_vars(t))),
        Type::Promise(t) => Type::Promise(Box::new(erase_generic_type_vars(t))),
        Type::Map { key, value } => Type::Map { key: Box::new(erase_generic_type_vars(key)), value: Box::new(erase_generic_type_vars(value)) },
        Type::FixedArray(ts) => Type::FixedArray(ts.iter().map(erase_generic_type_vars).collect()),
        Type::Union(ts) => Type::Union(ts.iter().map(erase_generic_type_vars).collect()),
        Type::Object { fields, sealed } => Type::Object {
            fields: fields.iter().map(|(k, v)| (k.clone(), erase_generic_type_vars(v))).collect(),
            sealed: *sealed,
        },
        Type::Function { params, ret, required, lset } => Type::Function {
            params: params.iter().map(erase_generic_type_vars).collect(),
            ret: Box::new(erase_generic_type_vars(ret)),
            required: *required,
            lset: lset.clone(),
        },
        _ => ty.clone(),
    }
}

/// True when `ty` mentions a GENERIC (quantified, non-Json-wildcard) `TypeVar`. The `Json` wildcard
/// `TypeVar(u32::MAX)` is excluded — it is a concrete dynamic type, not an unresolved type parameter.
pub(crate) fn type_mentions_generic_tv(ty: &Type) -> bool {
    match ty {
        Type::TypeVar(id) => *id != u32::MAX,
        Type::Array(inner) | Type::Iterator(inner) | Type::Shared(inner) | Type::Stream(inner) | Type::Promise(inner) => type_mentions_generic_tv(inner),
        Type::Map { key, value } => type_mentions_generic_tv(key) || type_mentions_generic_tv(value),
        Type::FixedArray(elems) => elems.iter().any(type_mentions_generic_tv),
        Type::Union(variants) => variants.iter().any(type_mentions_generic_tv),
        Type::Object { fields, .. } => fields.values().any(type_mentions_generic_tv),
        Type::Function { params, ret, .. } => {
            params.iter().any(type_mentions_generic_tv) || type_mentions_generic_tv(ret)
        }
        _ => false,
    }
}

/// Extract a compile-time i64 from an integer-literal key expression.
/// Accepts bare `IntLit(v)` and the parser's negation encoding `BinaryOp(0 - v)` so that
/// `{ -1: … }` is recognised as key `-1` (§5.1.1 disambiguation rule).
fn extract_int_key(expr: &Expr) -> Option<i64> {
    match expr {
        Expr::IntLit(v, _, _) => Some(*v),
        Expr::BinaryOp { op: BinOp::Sub, left, right, .. } => {
            // Parser lowers `-N` as `0 - N`; both sides must be literal integers.
            if let (Expr::IntLit(0, None, _), Expr::IntLit(v, _, _)) = (left.as_ref(), right.as_ref()) {
                Some(-v)
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checker::Checker;

    fn join(then_ty: Type, else_ty: Type) -> Type {
        Checker::new().join_branch_types(then_ty, else_ty)
    }

    /// Arm 3: exactly one `Null` branch forms a union with the value-producing branch.
    #[test]
    fn null_and_concrete_forms_union() {
        let result = join(Type::Null, Type::Bool);
        assert_eq!(result, Type::Union(vec![Type::Null, Type::Bool]));

        let result = join(Type::Bool, Type::Null);
        assert_eq!(result, Type::Union(vec![Type::Bool, Type::Null]));
    }

    /// Arm 3 exception: `Null × AnyVal` collapses to `AnyVal` (Json subsumes Null).
    #[test]
    fn null_and_anyval_collapses_to_anyval() {
        let any_val = Type::TypeVar(u32::MAX);
        assert_eq!(join(Type::Null, any_val.clone()), any_val.clone());
        assert_eq!(join(any_val.clone(), Type::Null), any_val);
    }

    /// Arm 1: `Never[]` (empty-array bottom) yields to a non-empty-array branch.
    #[test]
    fn empty_array_branch_yields_to_real_array() {
        let never_arr = Type::Array(Box::new(Type::Never));
        let string_arr = Type::Array(Box::new(Type::Str));
        // then=Never[], else=String[] → String[]
        assert_eq!(join(never_arr.clone(), string_arr.clone()), string_arr.clone());
        // then=String[], else=Never[] → String[]
        assert_eq!(join(string_arr.clone(), never_arr.clone()), string_arr);
        // both Never[] → Never[] (unchanged)
        assert_eq!(join(never_arr.clone(), never_arr.clone()), never_arr);
    }

    /// Arm 2: two DISTINCT quantified generic params form a union, not a collapse.
    #[test]
    fn distinct_generic_params_form_union() {
        let t = Type::TypeVar(9001);
        let d = Type::TypeVar(9002);
        let result = join(t.clone(), d.clone());
        assert_eq!(result, Type::Union(vec![t, d]));
    }

    /// Arm 2 does NOT fire for the same generic param on both sides (T | T → T).
    #[test]
    fn same_generic_param_collapses() {
        let t = Type::TypeVar(9001);
        // types_compatible is permissive on non-MAX TypeVars → arm 5 picks else_ty = t
        let result = join(t.clone(), t.clone());
        assert_eq!(result, t);
    }

    /// Arm 4: `AnyVal` (TypeVar(u32::MAX)) × concrete → result is `AnyVal`.
    #[test]
    fn anyval_and_concrete_yields_anyval() {
        let any_val = Type::TypeVar(u32::MAX);
        assert_eq!(join(any_val.clone(), Type::Bool), any_val.clone());
        assert_eq!(join(Type::Int32, any_val.clone()), any_val.clone());
    }

    /// Arm 4: unsolved inference TypeVar (id < 9000, not u32::MAX, not solved) × concrete → AnyVal.
    #[test]
    fn unsolved_inference_var_and_concrete_yields_anyval() {
        // A fresh Checker has an empty solved_type_vars map, so TypeVar(42) is unsolved.
        let unsolved = Type::TypeVar(42);
        let result = join(unsolved, Type::Bool);
        assert_eq!(result, Type::TypeVar(u32::MAX));
    }

    /// Arm 5: standard subtype collapse — Bool ⊆ Bool → Bool.
    #[test]
    fn compatible_types_collapse() {
        assert_eq!(join(Type::Bool, Type::Bool), Type::Bool);
        assert_eq!(join(Type::Int32, Type::Int32), Type::Int32);
    }

    /// Arm 6: unrelated concrete types form a union.
    #[test]
    fn unrelated_types_form_union() {
        let result = join(Type::Bool, Type::Str);
        assert_eq!(result, Type::Union(vec![Type::Bool, Type::Str]));
    }
}
