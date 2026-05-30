use indexmap::IndexMap;
use lin_common::{Diagnostic, Span};
use lin_parse::ast::{Expr, MatchArm, ObjectField, Stmt, StringPart};

use super::Checker;
use super::helpers::{integer_range, unify_types};
use crate::typed_ir::*;
use crate::types::Type;

impl Checker {
    pub(crate) fn check_expr(&mut self, expr: &Expr, expected: &Type) -> Result<TypedExpr, Diagnostic> {
        // For function expressions with a known expected function type, use the expected
        // param types to guide inference (bidirectional type checking).
        if let (Expr::Function { params, return_type, body, span }, Type::Function { params: expected_params, ret: expected_ret }) = (expr, expected) {
            return self.infer_function_with_hints(params, return_type, body, *span, None, expected_params, expected_ret);
        }

        // A suffixless integer literal takes its context type (spec §26). When the expected
        // type is an integer numeric type, re-type the literal directly at that width — but
        // only if its value fits the target's range, otherwise it's a compile error.
        if let Expr::IntLit(v, span) = expr {
            if expected.is_integer() {
                if let Some((lo, hi)) = integer_range(expected) {
                    let signed = *v as i128;
                    // A decimal literal larger than i64::MAX (e.g. UInt64 = 18446744073709551615)
                    // is lexed as the i64 bit pattern (negative). For an unsigned target, also
                    // consider the unsigned reinterpretation so such literals fit their range.
                    let fits = (signed >= lo && signed <= hi)
                        || (!expected.is_signed() && {
                            let unsigned = (*v as u64) as i128;
                            unsigned >= lo && unsigned <= hi
                        });
                    if !fits {
                        // Show the unsigned spelling only when it was lexed as an above-i64::MAX
                        // decimal (stored as a negative bit pattern) targeting an unsigned type.
                        let shown = if !expected.is_signed() && *v < 0 {
                            format!("{}", *v as u64)
                        } else {
                            format!("{}", v)
                        };
                        return Err(Diagnostic::error(
                            *span,
                            format!("literal {} is out of range for type {}", shown, expected),
                        ));
                    }
                }
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
            Expr::IntLit(v, span)    => Ok(TypedExpr::IntLit(*v, Type::Int32, *span)),
            Expr::FloatLit(v, span)  => Ok(TypedExpr::FloatLit(*v, Type::Float64, *span)),
            Expr::StringLit(s, span) => Ok(TypedExpr::StringLit(s.clone(), *span)),
            Expr::BoolLit(b, span)   => Ok(TypedExpr::BoolLit(*b, *span)),
            Expr::NullLit(span)      => Ok(TypedExpr::NullLit(*span)),
            Expr::Ident(name, span)  => self.infer_ident(name, *span),
            Expr::BinaryOp { left, op, right, span } => self.infer_binary_op(left, *op, right, *span),
            Expr::UnaryOp { op, operand, span } => self.infer_unary_op(*op, operand, *span),
            Expr::Call { func, args, span }           => self.infer_call(func, args, *span),
            Expr::DotCall { receiver, method, args, span } => self.infer_dot_call(receiver, method, args, *span),
            Expr::Index { object, key, span }         => self.infer_index(object, key, *span),
            Expr::If { condition, then_branch, else_branch, span } => self.infer_if(condition, then_branch, else_branch, *span),
            Expr::Match { scrutinee, arms, span }     => self.infer_match(scrutinee, arms, *span),
            Expr::Block(stmts, final_expr, span)      => self.infer_block(stmts, final_expr, *span),
            Expr::Function { params, return_type, body, span } => self.infer_function(params, return_type, body, *span, None),
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
                } else if let TypedExpr::StringLit(ref key_str, _) = typed_key {
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
                                if let TypedExpr::StringLit(ref key_str, _) = typed_key {
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
                if let TypedExpr::StringLit(ref key_str, _) = typed_key {
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
