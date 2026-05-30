use indexmap::IndexMap;
use lin_common::Diagnostic;
use lin_parse::ast::{MatchPattern, Pattern};

use super::Checker;
use super::helpers::collect_type_subs;
use crate::resolve::resolve_type;
use crate::typed_ir::*;
use crate::types::Type;

impl Checker {
    pub(crate) fn check_match_arm(
        &mut self,
        arm: &lin_parse::ast::MatchArm,
        scrutinee_ty: &Type,
        scrutinee_name: Option<&str>,
    ) -> Result<TypedMatchArm, Diagnostic> {
        self.env.push_scope();

        let typed_pattern = match &arm.pattern {
            MatchPattern::Is(pat) => {
                let tp = self.check_pattern(pat, scrutinee_ty)?;
                // Narrow the scrutinee variable within this arm's scope.
                // Reuse the same slot so LocalGet can unbox the TaggedVal pointer correctly.
                if let (Some(name), TypedPattern::TypeCheck(ref narrowed_ty, _)) = (scrutinee_name, &tp) {
                    if let Some(orig_info) = self.env.lookup(name) {
                        let orig_slot = orig_info.slot;
                        self.env.define_narrowed(name.to_string(), narrowed_ty.clone(), orig_slot);
                    } else {
                        self.env.define(name.to_string(), narrowed_ty.clone(), false);
                    }
                }
                TypedMatchPattern::Is(tp)
            }
            MatchPattern::Has(pat) => {
                TypedMatchPattern::Has(self.check_pattern(pat, scrutinee_ty)?)
            }
            MatchPattern::Else => TypedMatchPattern::Else,
        };

        let typed_guard = if let Some(ref guard) = arm.guard {
            Some(self.check_expr(guard, &Type::Bool)?)
        } else {
            None
        };

        let typed_body = self.infer_expr(&arm.body)?;

        self.env.pop_scope();

        Ok(TypedMatchArm {
            pattern: typed_pattern,
            guard: typed_guard,
            body: typed_body,
            span: arm.span,
        })
    }

    pub(crate) fn check_pattern(
        &mut self,
        pattern: &Pattern,
        _scrutinee_ty: &Type,
    ) -> Result<TypedPattern, Diagnostic> {
        match pattern {
            Pattern::TypeName(name, span) => {
                let ty = resolve_type(
                    &lin_parse::ast::TypeExpr::Named(name.clone(), *span),
                    &self.env,
                )
                .map_err(|e| Diagnostic::error(*span, e))?;
                Ok(TypedPattern::TypeCheck(ty, *span))
            }
            Pattern::Literal(expr) => {
                let typed = self.infer_expr(expr)?;
                Ok(TypedPattern::Literal(Box::new(typed)))
            }
            Pattern::Ident(name, span) => {
                let ty = _scrutinee_ty.clone();
                let slot = self.env.define(name.clone(), ty.clone(), false);
                Ok(TypedPattern::Binding(slot, ty, *span))
            }
            Pattern::Object(fields, rest, span) => {
                let mut typed_fields = Vec::new();
                for field in fields {
                    let key = field
                        .key
                        .clone()
                        .or_else(|| match &field.pattern {
                            Pattern::Ident(name, _) => Some(name.clone()),
                            _ => None,
                        })
                        .unwrap_or_default();

                    let field_ty = if let Type::Object(ref obj_fields) = _scrutinee_ty {
                        obj_fields.get(&key).cloned().unwrap_or(Type::Null)
                    } else {
                        self.env.fresh_type_var()
                    };

                    let binding_slot = match &field.pattern {
                        Pattern::Ident(name, _) => {
                            Some(self.env.define(name.clone(), field_ty.clone(), false))
                        }
                        _ => None,
                    };

                    let value_pattern = if let Some(ref vp) = field.value_pattern {
                        Some(Box::new(self.infer_expr(vp)?))
                    } else {
                        None
                    };

                    typed_fields.push(TypedPatternField {
                        key,
                        binding_slot,
                        value_pattern,
                        ty: field_ty,
                    });
                }

                let rest_slot = rest.as_ref().map(|name| {
                    self.env
                        .define(name.clone(), Type::Object(IndexMap::new()), false)
                });

                Ok(TypedPattern::Object {
                    fields: typed_fields,
                    rest: rest_slot,
                    span: *span,
                })
            }
            Pattern::Array(elements, rest, span) => {
                let mut typed_elements = Vec::new();
                for (i, elem) in elements.iter().enumerate() {
                    let elem_ty = if let Type::Array(ref inner) = _scrutinee_ty {
                        *inner.clone()
                    } else if let Type::FixedArray(ref types) = _scrutinee_ty {
                        types.get(i).cloned().unwrap_or(Type::Never)
                    } else {
                        self.env.fresh_type_var()
                    };
                    typed_elements.push(self.check_pattern(elem, &elem_ty)?);
                }

                let rest_slot = rest.as_ref().map(|name| {
                    let elem_ty = if let Type::Array(ref inner) = _scrutinee_ty {
                        Type::Array(inner.clone())
                    } else {
                        Type::Array(Box::new(self.env.fresh_type_var()))
                    };
                    self.env.define(name.clone(), elem_ty, false)
                });

                Ok(TypedPattern::Array {
                    elements: typed_elements,
                    rest: rest_slot,
                    span: *span,
                })
            }
            Pattern::Wildcard(span) => Ok(TypedPattern::Wildcard(*span)),
        }
    }

    pub(crate) fn bind_pattern(
        &mut self,
        pattern: &Pattern,
        ty: &Type,
        mutable: bool,
    ) -> Result<usize, Diagnostic> {
        match pattern {
            Pattern::Ident(name, span) => {
                // If this name was forward-declared (pre-scan for mutual recursion),
                // reuse the existing slot and update its type.
                if let Some(existing) = self.env.lookup(name) {
                    if self.forward_declared.contains(&existing.slot) {
                        let slot = existing.slot;
                        self.env.update_type(name, ty.clone());
                        self.forward_declared.remove(&slot);
                        return Ok(slot);
                    }
                }
                Ok(self.env.define_at(name.clone(), ty.clone(), mutable, Some(*span)))
            }
            Pattern::Wildcard(_) => Ok(self.env.define("_".to_string(), ty.clone(), false)),
            Pattern::Object(fields, rest, span) => {
                for field in fields {
                    let key = field
                        .key
                        .clone()
                        .or_else(|| match &field.pattern {
                            Pattern::Ident(name, _) => Some(name.clone()),
                            _ => None,
                        })
                        .unwrap_or_default();

                    let field_ty = if let Type::Object(ref obj_fields) = ty {
                        obj_fields.get(&key).cloned().unwrap_or(Type::Null)
                    } else if ty.is_json() {
                        crate::resolve::json_type()
                    } else {
                        return Err(Diagnostic::error(
                            *span,
                            format!("Cannot destructure non-object type {}", ty),
                        ));
                    };

                    self.bind_pattern(&field.pattern, &field_ty, mutable)?;
                }
                if let Some(rest_name) = rest {
                    // rest collects remaining fields as a Json object
                    self.env.define(rest_name.clone(), crate::resolve::json_type(), mutable);
                }
                Ok(self.env.next_slot() - 1)
            }
            Pattern::Array(elements, rest, span) => {
                for (i, elem) in elements.iter().enumerate() {
                    let elem_ty = if let Type::Array(ref inner) = ty {
                        *inner.clone()
                    } else if let Type::FixedArray(ref types) = ty {
                        types.get(i).cloned().unwrap_or(Type::Never)
                    } else if ty.is_json() {
                        // Dynamic JSON value — treat element type as Json
                        crate::resolve::json_type()
                    } else {
                        return Err(Diagnostic::error(
                            *span,
                            format!("Cannot destructure non-array type {}", ty),
                        ));
                    };
                    self.bind_pattern(elem, &elem_ty, mutable)?;
                }
                if let Some(rest_name) = rest {
                    let rest_ty = if let Type::Array(inner) = ty {
                        Type::Array(inner.clone())
                    } else {
                        Type::Array(Box::new(crate::resolve::json_type()))
                    };
                    self.env.define(rest_name.clone(), rest_ty, mutable);
                }
                Ok(self.env.next_slot() - 1)
            }
            _ => Ok(0),
        }
    }

    /// Collect TypeVar substitutions from a (pattern, actual) pair and save them
    /// to the global solved_type_vars map so the zonking pass can apply them later.
    pub(crate) fn collect_and_save_subs(&mut self, pattern: &Type, actual: &Type, local: &mut std::collections::HashMap<u32, Type>) {
        collect_type_subs(pattern, actual, local);
        for (id, ty) in local.iter() {
            // Intrinsic TypeVars (≥ 9000) are generic slots — don't solve them globally.
            // Protected TypeVars come from imported module signatures — never solve them either.
            if *id < 9000 && !self.protected_type_vars.contains(id) {
                self.solved_type_vars.entry(*id).or_insert_with(|| ty.clone());
            }
        }
    }
}
