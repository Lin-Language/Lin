use lin_common::{Diagnostic, Span};
use lin_parse::ast::{Expr, Param, Pattern};

use super::Checker;
use crate::resolve::resolve_type;
use crate::typed_ir::*;
use crate::types::Type;

impl Checker {
    pub(crate) fn infer_function(
        &mut self,
        params: &[Param],
        return_type: &Option<lin_parse::ast::TypeExpr>,
        body: &Expr,
        span: Span,
        fn_name: Option<&str>,
    ) -> Result<TypedExpr, Diagnostic> {
        // Record scope depth before pushing function scope, so LocalGet can detect captures.
        let entry_scope_depth = self.env.scope_depth();
        self.function_scope_depths.push(entry_scope_depth);
        self.capture_stack.push(std::collections::HashMap::new());

        self.env.push_scope();

        let mut typed_params = Vec::new();
        // Destructuring stmts for params with non-Ident patterns (e.g. `{ name, age }: Json`).
        let mut param_destr_stmts: Vec<TypedStmt> = Vec::new();

        for (i, param) in params.iter().enumerate() {
            let ty = if let Some(ref type_ann) = param.type_ann {
                resolve_type(type_ann, &self.env).map_err(|e| Diagnostic::error(span, e))?
            } else {
                self.env.fresh_type_var()
            };

            let (name, name_span) = match &param.pattern {
                Pattern::Ident(name, span) => (name.clone(), Some(*span)),
                _ => (format!("__param_{}", i), None),
            };

            let slot = self.env.define_at(name.clone(), ty.clone(), false, name_span);
            typed_params.push(TypedParam {
                slot,
                name,
                ty: ty.clone(),
            });

            // For destructuring patterns, emit a synthetic Destructure stmt into the body.
            if let Pattern::Object(fields, obj_rest, _) = &param.pattern {
                let obj_slot = typed_params.last().unwrap().slot;
                let mut typed_fields = Vec::new();
                for f in fields.iter() {
                    let key = f.key.clone().or_else(|| match &f.pattern {
                        Pattern::Ident(n, _) => Some(n.clone()),
                        _ => None,
                    }).unwrap_or_default();
                    let field_ty = if let Type::Object(ref obj_fields) = ty {
                        obj_fields.get(&key).cloned().unwrap_or(Type::Null)
                    } else { Type::TypeVar(u32::MAX) };
                    let fslot = match &f.pattern {
                        Pattern::Ident(fname, _) => self.env.define(fname.clone(), field_ty.clone(), false),
                        _ => self.env.define("_".to_string(), field_ty.clone(), false),
                    };
                    typed_fields.push((key, fslot, field_ty));
                }
                let rest_slot = if let Some(rest_name) = obj_rest {
                    let rslot = self.env.define(rest_name.clone(), Type::TypeVar(u32::MAX), false);
                    Some(rslot)
                } else { None };
                param_destr_stmts.push(TypedStmt::Destructure {
                    obj_slot,
                    value: TypedExpr::LocalGet { slot: obj_slot, ty: ty.clone(), span },
                    obj_ty: ty.clone(),
                    fields: typed_fields,
                    rest: rest_slot,
                    span,
                });
            }
        }

        let prev_fn = self.current_function.take();
        let prev_tail = self.in_tail_position;
        self.current_function = fn_name.map(|s| s.to_string());
        // Function body is always in tail position of itself.
        self.in_tail_position = self.current_function.is_some();

        let typed_body_raw = self.infer_expr(body)?;
        // Wrap body in a Block with destructuring preamble if needed.
        let typed_body = if param_destr_stmts.is_empty() {
            typed_body_raw
        } else {
            let body_ty = typed_body_raw.ty();
            TypedExpr::Block {
                stmts: param_destr_stmts,
                expr: Box::new(typed_body_raw),
                ty: body_ty,
                span,
            }
        };
        let body_ty = typed_body.ty();

        self.current_function = prev_fn;
        self.in_tail_position = prev_tail;
        self.env.pop_scope();

        self.function_scope_depths.pop();
        let captures_map = self.capture_stack.pop().unwrap_or_default();
        let mut captures: Vec<Capture> = captures_map.into_values().collect();
        // Stable ordering by outer_slot for deterministic codegen.
        captures.sort_by_key(|c| c.outer_slot);

        let ret_type = if let Some(ref rt) = return_type {
            let declared = resolve_type(rt, &self.env).map_err(|e| Diagnostic::error(span, e))?;
            if !self.types_compatible(&body_ty, &declared) {
                return Err(Diagnostic::error(
                    span,
                    format!(
                        "Function body has type {}, declared return type is {}",
                        body_ty, declared
                    ),
                ));
            }
            declared
        } else {
            body_ty
        };

        Ok(TypedExpr::Function {
            name: None,
            params: typed_params,
            body: Box::new(typed_body),
            ret_type,
            captures,
            span,
        })
    }

    /// Like infer_function, but substitutes TypeVar parameter types with hints from expected_params.
    /// `expected_ret` is the expected return type from the calling context (e.g. TypeVar for f: Function).
    pub(crate) fn infer_function_with_hints(
        &mut self,
        params: &[Param],
        return_type: &Option<lin_parse::ast::TypeExpr>,
        body: &Expr,
        span: Span,
        fn_name: Option<&str>,
        expected_params: &[Type],
        expected_ret: &Type,
    ) -> Result<TypedExpr, Diagnostic> {
        let entry_scope_depth = self.env.scope_depth();
        self.function_scope_depths.push(entry_scope_depth);
        self.capture_stack.push(std::collections::HashMap::new());

        self.env.push_scope();

        let mut typed_params = Vec::new();
        let mut param_destr_stmts: Vec<TypedStmt> = Vec::new();
        for (i, param) in params.iter().enumerate() {
            // Use the declared annotation if present; otherwise use the hint from expected_params.
            let ty = if let Some(ref type_ann) = param.type_ann {
                resolve_type(type_ann, &self.env).map_err(|e| Diagnostic::error(span, e))?
            } else if i < expected_params.len() && !matches!(expected_params[i], Type::TypeVar(_)) {
                expected_params[i].clone()
            } else {
                self.env.fresh_type_var()
            };

            let name = match &param.pattern {
                Pattern::Ident(name, _) => name.clone(),
                _ => format!("__param_{}", i),
            };

            let slot = self.env.define(name.clone(), ty.clone(), false);
            typed_params.push(TypedParam { slot, name, ty: ty.clone() });

            if let Pattern::Object(fields, obj_rest, _) = &param.pattern {
                let obj_slot = typed_params.last().unwrap().slot;
                let mut typed_fields = Vec::new();
                for f in fields.iter() {
                    let key = f.key.clone().or_else(|| match &f.pattern {
                        Pattern::Ident(n, _) => Some(n.clone()),
                        _ => None,
                    }).unwrap_or_default();
                    let field_ty = if let Type::Object(ref obj_fields) = ty {
                        obj_fields.get(&key).cloned().unwrap_or(Type::Null)
                    } else { Type::TypeVar(u32::MAX) };
                    let fslot = match &f.pattern {
                        Pattern::Ident(fname, _) => self.env.define(fname.clone(), field_ty.clone(), false),
                        _ => self.env.define("_".to_string(), field_ty.clone(), false),
                    };
                    typed_fields.push((key, fslot, field_ty));
                }
                let rest_slot = if let Some(rest_name) = obj_rest {
                    Some(self.env.define(rest_name.clone(), Type::TypeVar(u32::MAX), false))
                } else { None };
                param_destr_stmts.push(TypedStmt::Destructure {
                    obj_slot,
                    value: TypedExpr::LocalGet { slot: obj_slot, ty: ty.clone(), span },
                    obj_ty: ty.clone(),
                    fields: typed_fields,
                    rest: rest_slot,
                    span,
                });
            }
        }

        let prev_fn = self.current_function.take();
        let prev_tail = self.in_tail_position;
        self.current_function = fn_name.map(|s| s.to_string());
        self.in_tail_position = self.current_function.is_some();

        let typed_body_raw = self.infer_expr(body)?;
        let typed_body = if param_destr_stmts.is_empty() {
            typed_body_raw
        } else {
            let body_ty = typed_body_raw.ty();
            TypedExpr::Block {
                stmts: param_destr_stmts,
                expr: Box::new(typed_body_raw),
                ty: body_ty,
                span,
            }
        };
        let body_ty = typed_body.ty();

        self.current_function = prev_fn;
        self.in_tail_position = prev_tail;
        self.env.pop_scope();

        self.function_scope_depths.pop();
        let captures_map = self.capture_stack.pop().unwrap_or_default();
        let mut captures: Vec<Capture> = captures_map.into_values().collect();
        captures.sort_by_key(|c| c.outer_slot);

        let ret_type = if let Some(ref rt) = return_type {
            let declared = resolve_type(rt, &self.env).map_err(|e| Diagnostic::error(span, e))?;
            if !self.types_compatible(&body_ty, &declared) {
                return Err(Diagnostic::error(span, format!(
                    "Function body has type {}, declared return type is {}", body_ty, declared
                )));
            }
            declared
        } else if matches!(expected_ret, Type::TypeVar(_)) {
            // Expected return is a TypeVar (e.g. worker reply, promise result).
            // Use TypeVar so codegen boxes the concrete result — ensures a consistent tagged
            // calling convention when the closure is called through a polymorphic slot.
            expected_ret.clone()
        } else {
            body_ty
        };

        Ok(TypedExpr::Function {
            name: None,
            params: typed_params,
            body: Box::new(typed_body),
            ret_type,
            captures,
            span,
        })
    }
}
