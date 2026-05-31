use lin_common::{Diagnostic, Span};
use lin_parse::ast::{Expr, Stmt};

use super::Checker;
use super::helpers::is_legal_ffi_type;
use crate::resolve::resolve_type;
use crate::typed_ir::*;
use crate::types::Type;

impl Checker {
    pub(crate) fn check_stmt(&mut self, stmt: &Stmt) -> Result<TypedStmt, Diagnostic> {
        match stmt {
            Stmt::Val {
                pattern,
                type_ann,
                value,
                span,
                ..
            } => {
                let expected = type_ann
                    .as_ref()
                    .map(|t| resolve_type(t, &self.env))
                    .transpose()
                    .map_err(|e| Diagnostic::error(*span, e))?;

                // Extract the binding name for function name propagation (TCO, direct calls).
                let binding_name = match pattern {
                    lin_parse::ast::Pattern::Ident(n, _) => Some(n.as_str()),
                    _ => None,
                };

                // Phase 4.5b: an INTERMEDIATE `val result = lin_array_allocate(n)` inside a
                // combinator whose declared return is `Array(elem)`. `infer_function` set
                // `array_alloc_elem_hint = Some((result, elem))` for exactly this binding. Check
                // the fresh allocation against `Array(elem)` so its Json-wildcard element type is
                // refined to the declared element (the generic param `U`). Monomorphization then
                // pins `Array(U)` to a concrete `Array(Int32)` and codegen emits a flat allocation
                // matching the flat reader. Gated to the allocation intrinsic (see the helper).
                // Double-gate on the value here too (the hint name was matched syntactically in
                // `infer_function`, but a nested non-function block could re-bind the same name to
                // a non-alloc value; never retype that). Only a direct `lin_array_allocate(..)`
                // call is refined.
                let value_is_array_allocate = matches!(
                    value,
                    Expr::Call { func, .. } if matches!(func.as_ref(), Expr::Ident(n, _) if n == "lin_array_allocate")
                );
                let alloc_hint_elem = match (binding_name, &self.array_alloc_elem_hint) {
                    (Some(name), Some((hint_name, elem)))
                        if name == hint_name && expected.is_none() && value_is_array_allocate =>
                    {
                        Some(Type::Array(Box::new(elem.clone())))
                    }
                    _ => None,
                };

                let mut typed_value = match (value, binding_name) {
                    (Expr::Function { type_params, params, return_type, body, span }, Some(name)) => {
                        self.infer_function(type_params, params, return_type, body, *span, Some(name))?
                    }
                    _ => {
                        if let Some(ref hint_ty) = alloc_hint_elem {
                            self.check_expr(value, hint_ty)?
                        } else if let Some(ref expected_ty) = expected {
                            self.check_expr(value, expected_ty)?
                        } else {
                            self.infer_expr(value)?
                        }
                    }
                };

                // Propagate binding name into the TypedExpr::Function so codegen
                // can forward-declare and recognize it.
                if let (TypedExpr::Function { ref mut name, .. }, Some(pat_name)) =
                    (&mut typed_value, binding_name)
                {
                    if name.is_none() {
                        *name = Some(pat_name.to_string());
                    }
                }

                let ty = expected.unwrap_or_else(|| typed_value.ty());

                // Object/array destructuring: bind each field/element to its own slot
                // by emitting a Destructure statement that codegen handles properly.
                match pattern {
                    lin_parse::ast::Pattern::Object(fields, obj_rest, _) => {
                        // First store the whole object in a temp slot.
                        let obj_slot = self.env.define("__destr_obj".to_string(), ty.clone(), false);
                        let mut typed_fields = Vec::new();
                        for f in fields.iter() {
                            let key = f.key.clone().or_else(|| match &f.pattern {
                                lin_parse::ast::Pattern::Ident(n, _) => Some(n.clone()),
                                _ => None,
                            }).unwrap_or_default();
                            let field_ty = if let Type::Object(ref obj_fields) = ty {
                                obj_fields.get(&key).cloned().unwrap_or(Type::Null)
                            } else { Type::TypeVar(u32::MAX) };
                            let slot = match &f.pattern {
                                lin_parse::ast::Pattern::Ident(name, _) => {
                                    self.env.define(name.clone(), field_ty.clone(), false)
                                }
                                _ => self.env.define("_".to_string(), field_ty.clone(), false),
                            };
                            typed_fields.push((key, slot, field_ty));
                        }
                        // Bind the rest slot if present
                        let rest_slot = if let Some(rest_name) = obj_rest {
                            let slot = self.env.define(rest_name.clone(), Type::TypeVar(u32::MAX), false);
                            Some(slot)
                        } else {
                            None
                        };
                        let result = TypedStmt::Destructure {
                            obj_slot,
                            value: typed_value,
                            obj_ty: ty.clone(),
                            fields: typed_fields,
                            rest: rest_slot,
                            span: *span,
                        };
                        return Ok(result);
                    }
                    lin_parse::ast::Pattern::Array(elements, arr_rest, _) => {
                        let arr_slot = self.env.define("__destr_arr".to_string(), ty.clone(), false);
                        let elem_ty_inner = if let Type::Array(inner) = &ty {
                            *inner.clone()
                        } else {
                            Type::TypeVar(u32::MAX)
                        };
                        let mut typed_elements = Vec::new();
                        for (i, elem) in elements.iter().enumerate() {
                            let slot = match elem {
                                lin_parse::ast::Pattern::Ident(name, _) => {
                                    self.env.define(name.clone(), elem_ty_inner.clone(), false)
                                }
                                _ => self.env.define("_".to_string(), elem_ty_inner.clone(), false),
                            };
                            typed_elements.push((i, slot, elem_ty_inner.clone()));
                        }
                        let rest_info = if let Some(rest_name) = arr_rest {
                            let rest_ty = Type::Array(Box::new(elem_ty_inner.clone()));
                            let rest_slot = self.env.define(rest_name.clone(), rest_ty.clone(), false);
                            Some((rest_slot, rest_ty))
                        } else {
                            None
                        };
                        let result = TypedStmt::ArrayDestructure {
                            arr_slot,
                            value: typed_value,
                            elem_ty: elem_ty_inner,
                            elements: typed_elements,
                            rest: rest_info,
                            span: *span,
                        };
                        return Ok(result);
                    }
                    _ => {}
                }

                let slot = self.bind_pattern(pattern, &ty, false)?;
                let stmt_name = match pattern {
                    lin_parse::ast::Pattern::Ident(n, _) => Some(n.clone()),
                    _ => None,
                };

                Ok(TypedStmt::Val {
                    slot,
                    name: stmt_name,
                    value: typed_value,
                    ty,
                    span: *span,
                })
            }
            Stmt::Var {
                name,
                type_ann,
                value,
                span,
                ..
            } => {
                let expected = type_ann
                    .as_ref()
                    .map(|t| resolve_type(t, &self.env))
                    .transpose()
                    .map_err(|e| Diagnostic::error(*span, e))?;

                let typed_value = if let Some(ref expected_ty) = expected {
                    self.check_expr(value, expected_ty)?
                } else {
                    self.infer_expr(value)?
                };

                let ty = expected.unwrap_or_else(|| typed_value.ty());
                let slot = self.env.define(name.clone(), ty.clone(), true);
                // Track mutable globals for the async var-capture check.
                if self.function_scope_depths.is_empty() {
                    self.mutable_global_slots.insert(slot, name.clone());
                }

                Ok(TypedStmt::Var {
                    slot,
                    value: typed_value,
                    ty,
                    span: *span,
                })
            }
            Stmt::TypeDecl {
                name,
                params,
                body,
                span,
                ..
            } => {
                // The placeholder was registered in forward_declare_types.
                // Now resolve the actual body; self-references stay as Named(name) (cycle guard).
                //
                // For a generic alias (`type Box<T> = ...`), the params (`T`, `E`, ...) are not
                // real types — they must survive resolution as `Type::Named("T")` so that
                // `substitute` (resolve.rs) can replace them at each use-site (`Box<Int32>`).
                // We bind each param into a scratch env as a self-referential type decl (body
                // `Named(param)`); `resolve_named_cycle` then leaves it as `Named(param)` via the
                // same cycle guard used for the alias's own self-references. Without this, a bare
                // `T` in the body resolves as `Unknown type 'T'` and the body is never stored.
                let resolved = if params.is_empty() {
                    resolve_type(body, &self.env).map_err(|e| Diagnostic::error(*span, e))?
                } else {
                    let mut scratch = self.env.clone();
                    for param in params {
                        scratch.define_type(
                            param.clone(),
                            Vec::new(),
                            Type::Named(param.clone()),
                        );
                    }
                    resolve_type(body, &scratch).map_err(|e| Diagnostic::error(*span, e))?
                };
                self.env
                    .define_type(name.clone(), params.clone(), resolved);
                // Type declarations produce no runtime code
                Ok(TypedStmt::Expr(TypedExpr::NullLit(Span::dummy())))
            }
            Stmt::Import {
                bindings,
                path,
                span,
            } => {
                let mut import_slots = Vec::new();
                for binding in bindings {
                    let local_name = binding.alias.as_ref().unwrap_or(&binding.name);
                    // Use pre-resolved type if available, else fall back to TypeVar.
                    let ty = self.import_types
                        .get(&(path.clone(), binding.name.clone()))
                        .cloned()
                        .unwrap_or_else(|| self.env.fresh_type_var());
                    let slot = self.env.define(local_name.clone(), ty.clone(), false);
                    import_slots.push(ImportSlot {
                        name: binding.name.clone(),
                        slot,
                        ty,
                    });
                }
                Ok(TypedStmt::Import {
                    path: path.clone(),
                    bindings: import_slots,
                    span: *span,
                })
            }
            Stmt::ForeignImport { path, bindings, span } => {
                let is_runtime = path == "lin-runtime";
                let mut foreign_slots = Vec::new();
                for binding in bindings {
                    let ty = resolve_type(&binding.type_ann, &self.env)
                        .map_err(|e| Diagnostic::error(binding.span, e))?;
                    // "lin-runtime" is a reserved internal path — skip FFI type validation
                    // since runtime functions use Array/Object which aren't valid in user FFI.
                    let valid = is_runtime || is_legal_ffi_type(&ty);
                    if !valid {
                        self.diagnostics.push(Diagnostic::error(
                            binding.span,
                            format!("Foreign binding '{}' has illegal FFI type '{}'; only numeric primitives, Boolean, Null (return only), and String (argument only) are allowed (spec §26.3)", binding.name, ty),
                        ));
                    }
                    let slot = self.env.define(binding.name.clone(), ty.clone(), false);
                    foreign_slots.push(ForeignSlot { name: binding.name.clone(), slot, ty, valid });
                }
                Ok(TypedStmt::ForeignImport {
                    path: path.clone(),
                    bindings: foreign_slots,
                    span: *span,
                })
            }
            Stmt::Expr(expr) => {
                let typed = self.infer_expr(expr)?;
                Ok(TypedStmt::Expr(typed))
            }
        }
    }
}
