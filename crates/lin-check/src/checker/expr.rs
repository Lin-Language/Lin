use indexmap::IndexMap;
use lin_common::{Diagnostic, Span};
use lin_parse::ast::{Expr, MatchArm, ObjectField, Stmt, StringPart};

use super::Checker;
use super::helpers::{check_int_literal_fits, default_int_literal_type, suffix_to_type, unify_types};
use crate::typed_ir::*;
use crate::types::Type;

impl Checker {
    pub(crate) fn check_expr(&mut self, expr: &Expr, expected: &Type) -> Result<TypedExpr, Diagnostic> {
        // For function expressions with a known expected function type, use the expected
        // param types to guide inference (bidirectional type checking).
        if let (Expr::Function { type_params, params, return_type, body, span }, Type::Function { params: expected_params, ret: expected_ret, .. }) = (expr, expected) {
            return self.infer_function_with_hints(type_params, params, return_type, body, *span, None, expected_params, expected_ret);
        }

        // Integer literal against an expected type. A suffixless literal takes its context
        // type (spec §26), re-typed at that width if the value fits (else a compile error,
        // not a silent truncation). A *suffixed* literal pins its own type (spec §3.6) — it
        // falls through to `infer_expr` below, and the tail's compatibility check verifies it
        // against `expected` like any other typed expression.
        if let Expr::IntLit(v, None, span) = expr {
            if expected.is_integer() {
                check_int_literal_fits(*v, expected, *span)?;
                return Ok(TypedExpr::IntLit(*v, expected.clone(), *span));
            }
        }

        // Array literals: push the expected element type into each element so suffixless
        // integer literals adopt the correct width (and so the produced MakeArray carries the
        // expected element representation, matching the slot type at codegen). Mirrors the
        // per-element literal-coercion above for nested literals.
        if let (Expr::Array(elements, span), Type::Array(expected_elem)) = (expr, expected) {
            let typed_elements: Result<Vec<_>, _> =
                elements.iter().map(|e| self.check_expr(e, expected_elem)).collect();
            let typed_elements = typed_elements?;
            return Ok(TypedExpr::MakeArray {
                elements: typed_elements,
                ty: Type::Array(expected_elem.clone()),
                span: *span,
            });
        }

        // Singleton string-literal refinement (ADR-051). A bare string literal infers to
        // `String`, but when checked against an expected `StrLit("t")` it is accepted iff its
        // value equals `t`, and the resulting typed expression is narrowed to `StrLit("t")` so
        // it satisfies the literal target (e.g. a discriminant field).
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
        }

        // Object-literal refinement against an expected object/union/named type. Pushing the
        // expected field types down lets a discriminant string literal narrow to its `StrLit`
        // singleton, and (for a union) selects the matching variant by its discriminant tag.
        if let Expr::Object(fields, span) = expr {
            if let Some(result) = self.check_object_against(fields, expected, *span)? {
                return Ok(result);
            }
        }

        // Propagate the expected type into the branches of an `if`/`else` (each branch is a
        // tail position whose value is the expression's value), so an object/string literal in
        // a branch is refined against the same expected type (ADR-051). Only when both branches
        // are present (a bare `if ... then x` has an implicit Null else and is handled below).
        //
        // Bidirectional-push fix for the match-arm-union-vs-declared-object bug: when the
        // expected type is structured (an object / named object / union), each branch is checked
        // against the expected type rather than inferred-then-unioned. This refines object
        // literals structurally AND lets a `Json`-typed branch be accepted against a structured
        // object return type (see `check_branch_against` / `branch_value_compatible`), instead of
        // forming `Json | {concrete}` and rejecting that union against the declared return.
        if let Expr::If { condition, then_branch, else_branch, span } = expr {
            if expected_pushes_into_branches(expected) && !matches!(else_branch.as_ref(), Expr::NullLit(_)) {
                let in_tail = self.in_tail_position;
                self.in_tail_position = false;
                let typed_cond = self.check_expr(condition, &Type::Bool)?;
                self.in_tail_position = in_tail;
                let typed_then = self.check_branch_against(then_branch, expected)?;
                self.in_tail_position = in_tail;
                let typed_else = self.check_branch_against(else_branch, expected)?;
                let result_type = unify_types(&[typed_then.ty(), typed_else.ty()]);
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
        if let Expr::Match { scrutinee, arms, span } = expr {
            if expected_pushes_into_branches(expected) {
                return self.check_match(scrutinee, arms, expected, *span);
            }
        }

        // Propagate the expected type into the final expression of a block.
        if let (Expr::Block(stmts, final_expr, span), true) = (expr, type_mentions_strlit(expected)) {
            self.env.push_scope();
            let mut typed_stmts = Vec::new();
            for stmt in stmts {
                typed_stmts.push(self.check_stmt(stmt)?);
            }
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

        if !self.types_compatible(&actual_ty, expected) {
            return Err(Diagnostic::error(
                expr.span(),
                format!("Expected type {}, got {}", expected, actual_ty),
            ));
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

    pub(crate) fn infer_expr(&mut self, expr: &Expr) -> Result<TypedExpr, Diagnostic> {
        match expr {
            // Integer literal with no surrounding context. An explicit suffix pins the type
            // (spec §3.6). Otherwise the literal defaults to Int32 (spec §26) when it fits,
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
            Expr::UnaryOp { op, operand, span } => self.infer_unary_op(*op, operand, *span),
            Expr::Call { func, args, partial, span }  => self.infer_call(func, args, *partial, *span),
            Expr::DotCall { receiver, method, args, partial, span } => self.infer_dot_call(receiver, method, args, *partial, *span),
            Expr::Index { object, key, span }         => self.infer_index(object, key, *span),
            Expr::If { condition, then_branch, else_branch, span } => self.infer_if(condition, then_branch, else_branch, *span),
            Expr::Match { scrutinee, arms, span }     => self.infer_match(scrutinee, arms, *span),
            Expr::Block(stmts, final_expr, span)      => self.infer_block(stmts, final_expr, *span),
            Expr::Function { type_params, params, return_type, body, span } => self.infer_function(type_params, params, return_type, body, *span, None),
            Expr::Object(fields, span)                => self.infer_object(fields, *span),
            Expr::Array(elements, span)               => self.infer_array(elements, *span),
            Expr::Assign { target, value, span }      => self.infer_assign(target, value, *span),
            Expr::IndexAssign { object, key, value, span } => self.infer_index_assign(object, key, value, *span),
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
        self.span_type_map.push((span, ty.to_string(), def_span));
        Ok(TypedExpr::LocalGet { slot, ty, span })
    }

    pub(crate) fn infer_index(&mut self, object: &Expr, key: &Expr, span: Span) -> Result<TypedExpr, Diagnostic> {
        let typed_obj = self.infer_expr(object)?;
        let typed_key = self.infer_expr(key)?;
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
            Type::Object(fields) => {
                if fields.is_empty() {
                    // Empty schema (e.g. `var result = {}`): object may be populated dynamically,
                    // so any key access must be a runtime lookup → TypeVar.
                    self.env.fresh_type_var()
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
                    Type::Union(vec![Type::Union(fields.values().cloned().collect()), Type::Null])
                }
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
                            Type::Object(fields) => {
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
                    } else {
                        self.env.fresh_type_var()
                    };
                    Type::flatten_union(vec![inner, Type::Null])
                }
            }
            _ => return Err(Diagnostic::error(span, format!("Cannot index into type {}", obj_ty))),
        };
        Ok(TypedExpr::Index { object: Box::new(typed_obj), key: Box::new(typed_key), result_type, span })
    }

    pub(crate) fn infer_if(&mut self, condition: &Expr, then_branch: &Expr, else_branch: &Expr, span: Span) -> Result<TypedExpr, Diagnostic> {
        // Condition is not in tail position; branches inherit it.
        let in_tail = self.in_tail_position;
        self.in_tail_position = false;
        let typed_cond = self.check_expr(condition, &Type::Bool)?;
        self.in_tail_position = in_tail;
        let typed_then = self.infer_expr(then_branch)?;
        self.in_tail_position = in_tail;
        let typed_else = self.infer_expr(else_branch)?;
        let then_ty = typed_then.ty();
        let else_ty = typed_else.ty();
        let result_type = if self.types_compatible(&then_ty, &else_ty) {
            else_ty
        } else if self.types_compatible(&else_ty, &then_ty) {
            then_ty
        } else {
            Type::flatten_union(vec![then_ty, else_ty])
        };
        Ok(TypedExpr::If {
            cond: Box::new(typed_cond),
            then_br: Box::new(typed_then),
            else_br: Box::new(typed_else),
            result_type,
            span,
        })
    }

    /// Check a single `if`/`match` branch body against the expected (declared return / context)
    /// type, refining object/string literals structurally. A branch whose value is `Json`
    /// (`TypeVar(u32::MAX)`, the top/dynamic type) is accepted where any structured type is
    /// expected: `Json` is accept-any in this checked-arm position, so a function declared to
    /// return `R` may yield a `Json` value from one arm and a concrete `R`-shaped object from
    /// another. This is the bidirectional-push counterpart to the union-vs-declared-object bug;
    /// it deliberately does NOT relax `is_compatible_env` (ADR-046 still rejects a direct
    /// `val p: Person = jsonValue` decode).
    pub(crate) fn check_branch_against(&mut self, body: &Expr, expected: &Type) -> Result<TypedExpr, Diagnostic> {
        // First try the bidirectional refinement path (object/string-literal/nested if/match).
        let typed = self.check_expr_branch_inner(body, expected)?;
        let ty = typed.ty();
        if is_json_dynamic(&ty) || self.types_compatible(&ty, expected) {
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
        for arm in arms {
            let typed_arm = self.check_match_arm_against(arm, &scrutinee_ty, scrutinee_name, expected)?;
            arm_types.push(typed_arm.body.ty());
            typed_arms.push(typed_arm);
        }
        let result_type = if arm_types.is_empty() { Type::Never } else { unify_types(&arm_types) };

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
        for arm in arms {
            let typed_arm = self.check_match_arm(arm, &scrutinee_ty, scrutinee_name)?;
            arm_types.push(typed_arm.body.ty());
            typed_arms.push(typed_arm);
        }
        let result_type = if arm_types.is_empty() { Type::Never } else { unify_types(&arm_types) };

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
        let mut typed_fields = Vec::new();
        let mut spreads = Vec::new();
        let mut obj_type = IndexMap::new();
        for field in fields {
            match field {
                ObjectField::Pair(key_expr, val_expr) => {
                    let typed_val = self.infer_expr(val_expr)?;
                    let val_ty = typed_val.ty();
                    if let Expr::StringLit(key, _) = key_expr {
                        obj_type.insert(key.clone(), val_ty);
                        typed_fields.push((key.clone(), typed_val));
                    }
                }
                ObjectField::Spread(expr) => {
                    let typed_spread = self.infer_expr(expr)?;
                    if let Type::Object(ref fields) = typed_spread.ty() {
                        for (k, v) in fields { obj_type.insert(k.clone(), v.clone()); }
                    }
                    spreads.push(typed_spread);
                }
            }
        }
        Ok(TypedExpr::MakeObject { fields: typed_fields, spreads, ty: Type::Object(obj_type), span })
    }

    /// Bidirectional refinement for an object literal against an expected type (ADR-051).
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
                        return self.check_object_against(fields, &body, span);
                    }
                }
                Ok(None)
            }
            Type::Object(expected_fields) => {
                // Only take over when at least one expected field is a literal singleton; this
                // keeps plain structural objects on the existing inference path.
                if !expected_fields.values().any(|t| matches!(t, Type::StrLit(_))) {
                    return Ok(None);
                }
                Ok(Some(self.check_object_fields(fields, expected_fields, span)?))
            }
            Type::Union(variants) => {
                // Discriminant selection: find the variant whose literal-typed field matches a
                // matching literal field in the object. Only consider variants that have a
                // discriminant (a StrLit field) — these are the tagged-union cases.
                let literal_variants: Vec<&IndexMap<String, Type>> = variants
                    .iter()
                    .filter_map(|v| match v {
                        Type::Object(f) if f.values().any(|t| matches!(t, Type::StrLit(_))) => Some(f),
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
                    Some(vf) => Ok(Some(self.check_object_fields(fields, vf, span)?)),
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
        span: Span,
    ) -> Result<TypedExpr, Diagnostic> {
        let mut typed_fields = Vec::new();
        let mut obj_type = IndexMap::new();
        for field in fields {
            if let ObjectField::Pair(key_expr, val_expr) = field {
                if let Expr::StringLit(key, _) = key_expr {
                    let typed_val = match expected_fields.get(key) {
                        Some(ft) => self.check_expr(val_expr, ft)?,
                        None => self.infer_expr(val_expr)?,
                    };
                    obj_type.insert(key.clone(), typed_val.ty());
                    typed_fields.push((key.clone(), typed_val));
                }
            }
        }
        Ok(TypedExpr::MakeObject { fields: typed_fields, spreads: Vec::new(), ty: Type::Object(obj_type), span })
    }

    pub(crate) fn infer_array(&mut self, elements: &[Expr], span: Span) -> Result<TypedExpr, Diagnostic> {
        let typed_elements: Result<Vec<_>, _> = elements.iter().map(|e| self.infer_expr(e)).collect();
        let typed_elements = typed_elements?;
        let elem_types: Vec<Type> = typed_elements.iter().map(|t| t.ty()).collect();
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
        Ok(TypedExpr::LocalSet { slot, value: Box::new(typed_value), ty: expected_ty, span })
    }

    pub(crate) fn infer_index_assign(&mut self, object: &Expr, key: &Expr, value: &Expr, span: Span) -> Result<TypedExpr, Diagnostic> {
        let typed_obj = self.infer_expr(object)?;
        let typed_key = self.infer_expr(key)?;
        let obj_ty = typed_obj.ty();
        let typed_value = match &obj_ty {
            Type::Object(fields) => {
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
/// bidirectional literal refinement (ADR-051) so the if/block expected-type propagation only
/// fires for literal-typed targets, leaving all other inference behaviour unchanged.
/// True when the expected type is one we want pushed into `if`/`match` branch bodies for
/// bidirectional checking: a structured object, a named (alias) type, a union, or anything that
/// mentions a `StrLit` singleton (ADR-051). Plain scalars / arrays / iterators / `Json` keep the
/// old inference-then-unify path (pushing into them buys nothing and risks behaviour changes).
pub(crate) fn expected_pushes_into_branches(ty: &Type) -> bool {
    match ty {
        Type::Object(_) | Type::Named(_) | Type::Union(_) => true,
        _ => type_mentions_strlit(ty),
    }
}

/// True if `ty` is the dynamic/top `Json` type (`TypeVar(u32::MAX)`). A value of this type is
/// accept-any in checked branch/arm position (see `check_branch_against`).
pub(crate) fn is_json_dynamic(ty: &Type) -> bool {
    matches!(ty, Type::TypeVar(n) if *n == u32::MAX)
}

pub(crate) fn type_mentions_strlit(ty: &Type) -> bool {
    match ty {
        Type::StrLit(_) => true,
        Type::Array(inner) | Type::Iterator(inner) | Type::Shared(inner) => type_mentions_strlit(inner),
        Type::FixedArray(elems) => elems.iter().any(type_mentions_strlit),
        Type::Union(variants) => variants.iter().any(type_mentions_strlit),
        Type::Object(fields) => fields.values().any(type_mentions_strlit),
        Type::Function { params, ret, .. } => {
            params.iter().any(type_mentions_strlit) || type_mentions_strlit(ret)
        }
        _ => false,
    }
}
