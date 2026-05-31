use lin_common::{Diagnostic, Span};
use lin_parse::ast::{Expr, Param, Pattern};

use super::Checker;
use crate::resolve::resolve_type;
use crate::typed_ir::*;
use crate::types::Type;
use crate::env::TypeDecl;

/// Records the `type_decls` entries that `bind_type_params` shadowed, so they can be restored
/// (`unbind_type_params`) once a generic function body is checked. Keeps type-param names from
/// leaking past the function and prevents a nested generic's params from clobbering the outer's.
#[derive(Default)]
pub(crate) struct TypeParamGuard {
    saved: Vec<(String, Option<TypeDecl>)>,
}

impl Checker {
    /// Bind a generic function's type parameters into the type-decl environment so bare `T`
    /// annotations resolve to a quantified TypeVar (≥9000). For a named function we reuse the id
    /// assignment recorded by `forward_declare_functions` (so signature and body agree); for an
    /// anonymous generic lambda we mint fresh ids. No-op when there are no type params, which
    /// keeps non-generic functions on their existing code path.
    ///
    /// NOTE: `type_decls` is not scope-stacked. To keep type-param bindings hygienic, this returns
    /// a `TypeParamGuard` recording every `type_decls` entry it shadowed (the prior value, or
    /// absence). The caller MUST pass it to `unbind_type_params` after checking the body, which
    /// restores the previous bindings — so a generic param `T` cannot leak past the function and a
    /// nested generic's params cannot clobber the outer one's. No-op when there are no type params,
    /// which keeps non-generic functions on their existing code path.
    pub(crate) fn bind_type_params(
        &mut self,
        type_params: &[String],
        fn_name: Option<&str>,
    ) -> TypeParamGuard {
        let mut guard = TypeParamGuard::default();
        if type_params.is_empty() {
            return guard;
        }
        // Prefer the forward-declared assignment for this binding name.
        let recorded = fn_name.and_then(|n| self.generic_fn_params.get(n).cloned());
        match recorded {
            Some(assign) => {
                for (name, id) in assign {
                    guard.saved.push((name.clone(), self.env.type_decls.get(&name).cloned()));
                    self.env.define_type(name, Vec::new(), Type::TypeVar(id));
                }
            }
            None => {
                // Anonymous generic lambda: allocate fresh quantified ids now.
                for tp in type_params {
                    let id = self.next_generic_tv;
                    self.next_generic_tv += 1;
                    guard.saved.push((tp.clone(), self.env.type_decls.get(tp).cloned()));
                    self.env.define_type(tp.clone(), Vec::new(), Type::TypeVar(id));
                }
            }
        }
        guard
    }

    /// Restore the `type_decls` entries shadowed by a prior `bind_type_params`, removing the
    /// generic param bindings (or reinstating an outer alias of the same name).
    pub(crate) fn unbind_type_params(&mut self, guard: TypeParamGuard) {
        for (name, prev) in guard.saved.into_iter().rev() {
            match prev {
                Some(decl) => {
                    self.env.type_decls.insert(name, decl);
                }
                None => {
                    self.env.type_decls.shift_remove(&name);
                }
            }
        }
    }

    pub(crate) fn infer_function(
        &mut self,
        type_params: &[String],
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

        // Bind generic type parameters to their quantified TypeVar ids so that bare `T`
        // annotations resolve. Reuse the assignment chosen at forward-declaration time (keyed by
        // the binding name) so the signature and body agree; for an anonymous generic lambda,
        // mint fresh ids on the fly. These TypeVars live in the ≥9000 range and so are never
        // globally solved — each call site instantiates them locally (Phase 0 monomorphization).
        // The guard is restored after the body so the param names don't leak (hygiene).
        let type_param_guard = self.bind_type_params(type_params, fn_name);

        let mut typed_params = Vec::new();
        // Destructuring stmts for params with non-Ident patterns (e.g. `{ name, age }: Json`).
        let mut param_destr_stmts: Vec<TypedStmt> = Vec::new();
        // Tracks whether a preceding parameter carried a default — once one does, every
        // following parameter must too (optional params must be last).
        let mut seen_default = false;

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

            // Type-check the default value (if any) before defining this parameter's
            // slot, so it may reference earlier parameters but not itself. Enforce the
            // optional-last rule: a required parameter may not follow an optional one.
            let typed_default = match &param.default {
                Some(default_expr) => {
                    let typed = self.check_expr(default_expr, &ty)?;
                    seen_default = true;
                    Some(Box::new(typed))
                }
                None => {
                    if seen_default {
                        let dspan = name_span.unwrap_or(span);
                        return Err(Diagnostic::error(
                            dspan,
                            format!(
                                "required parameter '{}' cannot follow a parameter with a default value",
                                name
                            ),
                        ).with_help("give this parameter a default too, or move it before the optional parameters".to_string()));
                    }
                    None
                }
            };

            let slot = self.env.define_at(name.clone(), ty.clone(), false, name_span);
            typed_params.push(TypedParam {
                slot,
                name,
                ty: ty.clone(),
                default: typed_default,
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

        // Resolve the declared return type up front so the body can be CHECKED against it
        // (bidirectional), pushing the expected type into the body. Needed for singleton
        // string-literal refinement (ADR-051) — see infer_function_with_hints for the rationale.
        let declared_ret = match return_type {
            Some(rt) => Some(resolve_type(rt, &self.env).map_err(|e| Diagnostic::error(span, e))?),
            None => None,
        };
        // CHECK the body bidirectionally against the declared return type when that type is
        // structured (an object/named/union, or one mentioning a `StrLit` singleton). This pushes
        // the expected type into `if`/`match` arms (see `check_branch_against`), which:
        //   - refines object/string literals against the declared shape (ADR-051), and
        //   - lets one arm yield a `Json` value while another yields a concrete object literal,
        //     each checked against the declared return — fixing the match-arm-union-vs-declared-
        //     object bug (previously the arms were inferred independently, unioned into
        //     `Json | {concrete}`, and that union rejected against `R`).
        // `checked_against_declared` records that `check_expr` already enforced compatibility, so
        // the post-pass `types_compatible(body_ty, declared)` re-check (which would reject the
        // surviving `Json | {R}` union type) is skipped.
        let mut checked_against_declared = false;
        let typed_body_raw = match &declared_ret {
            Some(declared) if super::expr::expected_pushes_into_branches(declared) => {
                checked_against_declared = true;
                self.check_expr(body, declared)?
            }
            _ => self.infer_expr(body)?,
        };
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
        // Restore any type aliases shadowed by this function's generic params (hygiene).
        self.unbind_type_params(type_param_guard);

        self.function_scope_depths.pop();
        let captures_map = self.capture_stack.pop().unwrap_or_default();
        let mut captures: Vec<Capture> = captures_map.into_values().collect();
        // Stable ordering by outer_slot for deterministic codegen.
        captures.sort_by_key(|c| c.outer_slot);

        let ret_type = if let Some(declared) = declared_ret {
            if !checked_against_declared && !self.types_compatible(&body_ty, &declared) {
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
        type_params: &[String],
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

        // Bind generic type params (see `infer_function` for rationale).
        let type_param_guard = self.bind_type_params(type_params, fn_name);

        let mut typed_params = Vec::new();
        let mut param_destr_stmts: Vec<TypedStmt> = Vec::new();
        let mut seen_default = false;
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

            // Type-check the default before defining this param's slot (earlier params
            // are in scope; self-reference is not). Enforce optional-last.
            let typed_default = match &param.default {
                Some(default_expr) => {
                    let typed = self.check_expr(default_expr, &ty)?;
                    seen_default = true;
                    Some(Box::new(typed))
                }
                None => {
                    if seen_default {
                        return Err(Diagnostic::error(
                            span,
                            format!(
                                "required parameter '{}' cannot follow a parameter with a default value",
                                name
                            ),
                        ).with_help("give this parameter a default too, or move it before the optional parameters".to_string()));
                    }
                    None
                }
            };

            let slot = self.env.define(name.clone(), ty.clone(), false);
            typed_params.push(TypedParam { slot, name, ty: ty.clone(), default: typed_default });

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

        // Resolve the declared return type up front so the body can be CHECKED against it
        // (bidirectional). This pushes the expected type into the body — needed for singleton
        // string-literal refinement (ADR-051): a `{ "type": "success", .. }` literal in the
        // body narrows its discriminant to the expected `StrLit` variant. Falls back to plain
        // inference when there is no annotation.
        let declared_ret = match return_type {
            Some(rt) => Some(resolve_type(rt, &self.env).map_err(|e| Diagnostic::error(span, e))?),
            None => None,
        };
        // See `infer_function` for the rationale: push a structured declared return type into the
        // body's `if`/`match` arms (fixes the match-arm-union-vs-declared-object bug).
        let mut checked_against_declared = false;
        let typed_body_raw = match &declared_ret {
            Some(declared) if super::expr::expected_pushes_into_branches(declared) => {
                checked_against_declared = true;
                self.check_expr(body, declared)?
            }
            _ => self.infer_expr(body)?,
        };
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
        // Restore any type aliases shadowed by this function's generic params (hygiene).
        self.unbind_type_params(type_param_guard);

        self.function_scope_depths.pop();
        let captures_map = self.capture_stack.pop().unwrap_or_default();
        let mut captures: Vec<Capture> = captures_map.into_values().collect();
        captures.sort_by_key(|c| c.outer_slot);

        let ret_type = if let Some(declared) = declared_ret {
            if !checked_against_declared && !self.types_compatible(&body_ty, &declared) {
                return Err(Diagnostic::error(span, format!(
                    "Function body has type {}, declared return type is {}", body_ty, declared
                )));
            }
            declared
        } else if matches!(expected_ret, Type::TypeVar(id)
            if *id >= 9001 && *id != u32::MAX)
            && !matches!(body_ty, Type::TypeVar(_))
        {
            // Expected return is a QUANTIFIED GENERIC type parameter (`<U>`, id ≥ 9001) and the
            // body has a concrete type: surface the concrete `body_ty` as the lambda's return.
            // This is what lets a higher-order generic call (`mymap(arr, x => x*2)` where
            // `mymap`'s `f: (T) => U`) bind `U` from the lambda body — the call site's
            // `collect_and_save_subs` reads the lambda's concrete return and the result type
            // `U[]` becomes `Int32[]`, so monomorphization can specialize. Forcing the bare
            // generic TypeVar here (as the polymorphic-slot case below does) would leave `U`
            // uninferrable and the call would fall back to a boxed copy.
            body_ty
        } else if matches!(expected_ret, Type::TypeVar(_)) {
            // Expected return is a TypeVar (e.g. worker reply, promise result, or a Json/`Function`
            // polymorphic slot). Use TypeVar so codegen boxes the concrete result — ensures a
            // consistent tagged calling convention when the closure is called through a
            // polymorphic slot.
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
