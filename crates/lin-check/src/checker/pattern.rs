use indexmap::IndexMap;
use lin_common::Diagnostic;
use lin_parse::ast::{MatchPattern, Pattern};

use super::Checker;
use super::helpers::collect_type_subs;
use crate::resolve::resolve_type;
use crate::typed_ir::*;
use crate::types::Type;

impl Checker {
    /// Build the `Error` discriminant pattern `{ "type": "error", "message": _ }` used to
    /// desugar `is Error` (ADR-031). Field presence is checked for both keys; `"type"` carries
    /// the literal value constraint `"error"` so a decoded value (which never has
    /// `"type" == "error"`) does not match.
    fn error_discriminant_pattern(&self, span: lin_common::Span) -> TypedPattern {
        TypedPattern::Object {
            fields: vec![
                TypedPatternField {
                    key: "type".to_string(),
                    binding_slot: None,
                    value_pattern: Some(Box::new(TypedExpr::StringLit("error".to_string(), Type::Str, span))),
                    ty: Type::Str,
                },
                TypedPatternField {
                    key: "message".to_string(),
                    binding_slot: None,
                    value_pattern: None,
                    ty: Type::Str,
                },
            ],
            rest: None,
            span,
        }
    }

    /// Narrow the scrutinee binding to `narrow_to` (the complement of preceding excluded `is`
    /// arms) within the current arm scope. Reuses the original slot via `define_narrowed` (the
    /// runtime value is identical; only the static type tightens). No-op when there is no
    /// narrowing to apply. Must be called after the arm's `push_scope`.
    fn apply_arm_complement_narrowing(&mut self, scrutinee_name: Option<&str>, narrow_to: Option<&Type>) {
        if let (Some(name), Some(ty)) = (scrutinee_name, narrow_to) {
            if let Some(info) = self.env.lookup(name) {
                let slot = info.slot;
                let mutable = info.mutable;
                let declared = info.declared_ty.clone().unwrap_or_else(|| info.ty.clone());
                self.env.define_narrowed(name.to_string(), ty.clone(), slot, mutable, declared);
            }
        }
    }

    pub(crate) fn check_match_arm(
        &mut self,
        arm: &lin_parse::ast::MatchArm,
        scrutinee_ty: &Type,
        scrutinee_name: Option<&str>,
        narrow_to: Option<&Type>,
    ) -> Result<TypedMatchArm, Diagnostic> {
        self.env.push_scope();

        let typed_pattern = match &arm.pattern {
            MatchPattern::Is(pat) => {
                let tp = self.check_pattern(pat, scrutinee_ty)?;
                // Narrow the scrutinee variable within this arm's scope.
                // Reuse the same slot so LocalGet can unbox the TaggedVal pointer correctly.
                // `TypeCheckDeep` (ADR-036) narrows to its object type exactly like `TypeCheck`.
                let narrowed = match &tp {
                    TypedPattern::TypeCheck(ty, _) => Some(ty.clone()),
                    TypedPattern::TypeCheckDeep(ty, _, _) => Some(ty.clone()),
                    // No positive narrowing for this `is`-pattern (e.g. `is Error` desugars to an
                    // object discriminant pattern, a literal check, a binding/destructure). Fall
                    // back to the complement of preceding excluded arms, if any — this still tightens
                    // the scrutinee away from cases already handled above.
                    _ => narrow_to.cloned(),
                };
                if let (Some(name), Some(narrowed_ty)) = (scrutinee_name, narrowed) {
                    if let Some(orig_info) = self.env.lookup(name) {
                        let orig_slot = orig_info.slot;
                        let orig_mutable = orig_info.mutable;
                        let orig_declared = orig_info.declared_ty.clone().unwrap_or_else(|| orig_info.ty.clone());
                        self.env.define_narrowed(name.to_string(), narrowed_ty.clone(), orig_slot, orig_mutable, orig_declared);
                    } else {
                        self.env.define(name.to_string(), narrowed_ty.clone(), false);
                    }
                }
                TypedMatchPattern::Is(tp)
            }
            MatchPattern::Has(pat) => {
                self.apply_arm_complement_narrowing(scrutinee_name, narrow_to);
                TypedMatchPattern::Has(self.check_pattern(pat, scrutinee_ty)?)
            }
            MatchPattern::Else => {
                // Flow-narrow the scrutinee to the complement of every preceding guard-free `is X`
                // arm (those `X` cases are already handled) in this `else`/later arm. Generalizes
                // the old Null-only rule. See `complement_narrowing`.
                self.apply_arm_complement_narrowing(scrutinee_name, narrow_to);
                TypedMatchPattern::Else
            }
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

    /// Like `check_match_arm`, but checks the arm body against an `expected` type (bidirectional
    /// push). Used when a `match` is in a position with a known structured expected type (e.g. a
    /// declared object return type) so each arm refines/accepts against it directly, rather than
    /// inferring arms independently and unioning them. See `check_branch_against` for the
    /// arm-body compatibility rule (including the `Json`-arm allowance).
    pub(crate) fn check_match_arm_against(
        &mut self,
        arm: &lin_parse::ast::MatchArm,
        scrutinee_ty: &Type,
        scrutinee_name: Option<&str>,
        expected: &Type,
        narrow_to: Option<&Type>,
    ) -> Result<TypedMatchArm, Diagnostic> {
        self.env.push_scope();

        let typed_pattern = match &arm.pattern {
            MatchPattern::Is(pat) => {
                let tp = self.check_pattern(pat, scrutinee_ty)?;
                let narrowed = match &tp {
                    TypedPattern::TypeCheck(ty, _) => Some(ty.clone()),
                    TypedPattern::TypeCheckDeep(ty, _, _) => Some(ty.clone()),
                    // See `check_match_arm`: fall back to the complement when this `is`-pattern has
                    // no positive narrowing (e.g. `is Error`).
                    _ => narrow_to.cloned(),
                };
                if let (Some(name), Some(narrowed_ty)) = (scrutinee_name, narrowed) {
                    if let Some(orig_info) = self.env.lookup(name) {
                        let orig_slot = orig_info.slot;
                        let orig_mutable = orig_info.mutable;
                        let orig_declared = orig_info.declared_ty.clone().unwrap_or_else(|| orig_info.ty.clone());
                        self.env.define_narrowed(name.to_string(), narrowed_ty.clone(), orig_slot, orig_mutable, orig_declared);
                    } else {
                        self.env.define(name.to_string(), narrowed_ty.clone(), false);
                    }
                }
                TypedMatchPattern::Is(tp)
            }
            MatchPattern::Has(pat) => {
                self.apply_arm_complement_narrowing(scrutinee_name, narrow_to);
                TypedMatchPattern::Has(self.check_pattern(pat, scrutinee_ty)?)
            }
            MatchPattern::Else => {
                self.apply_arm_complement_narrowing(scrutinee_name, narrow_to);
                TypedMatchPattern::Else
            }
        };

        let typed_guard = if let Some(ref guard) = arm.guard {
            Some(self.check_expr(guard, &Type::Bool)?)
        } else {
            None
        };

        let typed_body = self.check_branch_against(&arm.body, expected)?;

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
        scrutinee_ty: &Type,
    ) -> Result<TypedPattern, Diagnostic> {
        match pattern {
            Pattern::TypeName(name, span) => {
                let ty = resolve_type(
                    &lin_parse::ast::TypeExpr::Named(name.clone(), *span),
                    &self.env,
                )
                .map_err(|e| Diagnostic::error(*span, e))?;
                // `is Error` / `match | is Error` (ADR-031): `Error` is a structural object
                // alias `{ "type": String, "message": String }`. A bare tag check would match
                // ANY object (e.g. a successfully-decoded `Person`), so `is Error` could not
                // discriminate a decode failure from a decoded value. Desugar `is Error` into a
                // value-constrained object pattern `{ "type": "error", "message": _ }`, reusing
                // the existing object-pattern lowering which checks field presence AND the
                // `"type" == "error"` discriminant at runtime. The decode-error object always
                // carries `"type": "error"`; a decoded value of any other shape does not.
                if name == "Error" {
                    return Ok(self.error_discriminant_pattern(*span));
                }
                // `is <Name>` resolving to a non-empty object type (ADR-036): validate field
                // TYPES recursively at runtime, not just the field presence the earlier rule
                // (now folded into ADR-036) checked. A bare
                // presence check let `{ "name": 1, "age": "x" }` match `Person`, then the arm
                // narrowed the binding and a subsequent `x["age"] + 1` operated on the wrong
                // runtime type — unsound. Reuse the `fromJson` structural walker by carrying the
                // target object type + the resolved bodies of every reachable Named type (so IR
                // lowering can build the recursive schema descriptor without a type env). An
                // empty object type `{}` keeps the cheap bare tag check (nothing to validate).
                if let Type::Object { ref fields, .. } = ty {
                    if !fields.is_empty() {
                        let mut named_defs: Vec<(String, Type)> = Vec::new();
                        let mut seen: std::collections::HashSet<String> =
                            std::collections::HashSet::new();
                        self.collect_named_defs(&ty, &mut seen, &mut named_defs);
                        // Also collect the named-type bodies reachable from the
                        // SCRUTINEE type (e.g. the `Ast = Num | BinOp` alias and its
                        // variants). IR lowering uses these to recognise a closed
                        // concrete union scrutinee and take the cheap discriminator
                        // fast path instead of the recursive `MatchesSchema`. These
                        // extra entries are a pure lookup table — they never change
                        // the schema descriptor (built from `target` only), so adding
                        // them is behaviour-preserving for the fallback path.
                        self.collect_named_defs(scrutinee_ty, &mut seen, &mut named_defs);
                        return Ok(TypedPattern::TypeCheckDeep(ty, named_defs, *span));
                    }
                }
                Ok(TypedPattern::TypeCheck(ty, *span))
            }
            Pattern::Literal(expr) => {
                let typed = self.infer_expr(expr)?;
                Ok(TypedPattern::Literal(Box::new(typed)))
            }
            Pattern::Ident(name, span) => {
                let ty = scrutinee_ty.clone();
                self.check_shadowing(name, *span);
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

                    let field_ty = if let Type::Object { fields: ref obj_fields, .. } = scrutinee_ty {
                        obj_fields.get(&key).cloned().unwrap_or(Type::Null)
                    } else {
                        self.env.fresh_type_var()
                    };

                    let binding_slot = match &field.pattern {
                        Pattern::Ident(name, name_span) => {
                            self.check_shadowing(name, *name_span);
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

                let rest_slot = if let Some(name) = rest {
                    self.check_shadowing(name, *span);
                    Some(self.env.define(name.clone(), Type::object(IndexMap::new()), false))
                } else {
                    None
                };

                Ok(TypedPattern::Object {
                    fields: typed_fields,
                    rest: rest_slot,
                    span: *span,
                })
            }
            Pattern::Array(elements, rest, span) => {
                let mut typed_elements = Vec::new();
                for (i, elem) in elements.iter().enumerate() {
                    let elem_ty = if let Type::Array(ref inner) = scrutinee_ty {
                        *inner.clone()
                    } else if let Type::FixedArray(ref types) = scrutinee_ty {
                        types.get(i).cloned().unwrap_or(Type::Never)
                    } else {
                        self.env.fresh_type_var()
                    };
                    typed_elements.push(self.check_pattern(elem, &elem_ty)?);
                }

                let rest_slot = if let Some(name) = rest {
                    self.check_shadowing(name, *span);
                    let elem_ty = if let Type::Array(ref inner) = scrutinee_ty {
                        Type::Array(inner.clone())
                    } else {
                        Type::Array(Box::new(self.env.fresh_type_var()))
                    };
                    Some(self.env.define(name.clone(), elem_ty, false))
                } else {
                    None
                };

                Ok(TypedPattern::Array {
                    elements: typed_elements,
                    rest: rest_slot,
                    span: *span,
                })
            }
            Pattern::Wildcard(span) => Ok(TypedPattern::Wildcard(*span)),
        }
    }

    /// ADR-074: find the forward-declared overload slot of `name` that this val (with parameter
    /// types `params`) defines. Matches the first STILL-UNBOUND candidate whose parameters are
    /// type-compatible (treating un-annotated `TypeVar` params as wildcards). The unbound gate
    /// resolves order among equally-matching candidates: the Nth source definition binds the Nth
    /// forward-declared slot. Tolerant `types_compatible` matching absorbs named-vs-structural
    /// differences between the pre-scan's and the body check's type resolution.
    fn match_overload_slot(&self, name: &str, params: &[Type]) -> Option<usize> {
        for (slot, ty) in self.env.overload_candidates(name) {
            if !self.forward_declared.contains(&slot) {
                continue;
            }
            if let Type::Function { params: cps, .. } = &ty {
                if cps.len() == params.len()
                    && cps.iter().zip(params.iter()).all(|(a, b)| {
                        matches!(a, Type::TypeVar(_))
                            || matches!(b, Type::TypeVar(_))
                            || (self.types_compatible(a, b) && self.types_compatible(b, a))
                    })
                {
                    return Some(slot);
                }
            }
        }
        None
    }

    pub(crate) fn bind_pattern(
        &mut self,
        pattern: &Pattern,
        ty: &Type,
        mutable: bool,
    ) -> Result<usize, Diagnostic> {
        match pattern {
            Pattern::Ident(name, span) => {
                // ADR-074: when `name` is a function overload set, this val re-binds its OWN
                // forward-declared slot — matched by parameter types, not the primary's slot —
                // so each overload keeps its distinct slot (hence FuncId/symbol).
                if self.env.is_overloaded(name) {
                    if let Type::Function { params, .. } = ty {
                        if let Some(slot) = self.match_overload_slot(name, params) {
                            self.env.update_overload_type(slot, ty.clone());
                            self.forward_declared.remove(&slot);
                            return Ok(slot);
                        }
                    }
                }
                // If this name was forward-declared (pre-scan for mutual recursion),
                // reuse the existing slot and update its type.
                if let Some(existing) = self.env.lookup(name) {
                    if self.forward_declared.contains(&existing.slot) {
                        let slot = existing.slot;
                        // When the forward-declared return type was a fresh TypeVar (minted by
                        // forward_declare_functions_in because no annotation was present), and the
                        // now-checked body gives a concrete return, solve that TypeVar so the
                        // zonking pass can replace every call-site result_type that still carries
                        // it. Without this, a call to a forward-declared void/Null-returning
                        // function that appears textually BEFORE the function definition emits
                        // Instruction::Call { ret_ty: TypeVar(N) } into the IR, TypeVar(N) never
                        // reaches solved_type_vars, zonking leaves it unresolved, and codegen
                        // treats the call as returning a non-void value — unwrap_basic() panics.
                        if let (
                            Type::Function { ret: old_ret, .. },
                            Type::Function { ret: new_ret, .. },
                        ) = (&existing.ty, ty)
                        {
                            if let Type::TypeVar(id) = old_ret.as_ref() {
                                let id = *id;
                                if id < 9000 && !self.protected_type_vars.contains(&id) {
                                    self.solved_type_vars.entry(id).or_insert_with(|| *new_ret.clone());
                                }
                            }
                        }
                        self.env.update_type(name, ty.clone());
                        self.forward_declared.remove(&slot);
                        return Ok(slot);
                    }
                }
                self.check_shadowing(name, *span);
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

                    let field_ty = if let Type::Object { fields: ref obj_fields, .. } = ty {
                        obj_fields.get(&key).cloned().unwrap_or(Type::Null)
                    } else if ty.is_any_val() {
                        crate::resolve::any_val_type()
                    } else {
                        return Err(Diagnostic::error(
                            *span,
                            format!("Cannot destructure non-object type {}", ty),
                        ));
                    };

                    self.bind_pattern(&field.pattern, &field_ty, mutable)?;
                }
                if let Some(rest_name) = rest {
                    self.check_shadowing(rest_name, *span);
                    // rest collects remaining fields as a Json object
                    self.env.define(rest_name.clone(), crate::resolve::any_val_type(), mutable);
                }
                Ok(self.env.next_slot() - 1)
            }
            Pattern::Array(elements, rest, span) => {
                for (i, elem) in elements.iter().enumerate() {
                    let elem_ty = if let Type::Array(ref inner) = ty {
                        *inner.clone()
                    } else if let Type::FixedArray(ref types) = ty {
                        types.get(i).cloned().unwrap_or(Type::Never)
                    } else if ty.is_any_val() {
                        // Dynamic JSON value — treat element type as Json
                        crate::resolve::any_val_type()
                    } else {
                        return Err(Diagnostic::error(
                            *span,
                            format!("Cannot destructure non-array type {}", ty),
                        ));
                    };
                    self.bind_pattern(elem, &elem_ty, mutable)?;
                }
                if let Some(rest_name) = rest {
                    self.check_shadowing(rest_name, *span);
                    let rest_ty = if let Type::Array(inner) = ty {
                        Type::Array(inner.clone())
                    } else {
                        Type::Array(Box::new(crate::resolve::any_val_type()))
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

    /// Like `collect_and_save_subs`, but does NOT clobber an existing local binding for a TypeVar
    /// when the freshly-collected candidate is `arg_compatible` (assignable) to the established
    /// binding. This makes the FIRST canonical binding win across multiple arguments that share one
    /// TypeVar: e.g. `push(out: Field[], { … }: {tag:UInt8,…})` binds `T = Field` from the container
    /// arg, and the structurally-narrower item (whose `UInt8` fields WIDEN into `Field`'s `Int32`
    /// fields) must not overwrite it to the narrower object type — which would then reject the
    /// container arg as "expected {tag:UInt8,…}[]".
    ///
    /// TYPE-SOUNDNESS GUARD (record field omission): the established binding is ALSO kept when the
    /// fresh candidate is a record that OMITS a required (non-nullable) field the established record
    /// binding has — clobbering to it would widen `T` to a structurally DEFICIENT object. This is the
    /// omission hole: `push(toks, {kind})` where `toks: Token[]` binds `T = Token = {kind:String,
    /// text:String}`, and the omitting item `{kind}` is NOT `arg_compatible` with `Token` (missing
    /// `text`). Last-wins-clobber would silently rebind `T` to `{kind}`, after which the arg-compat
    /// gate (`call.rs`) compares `{kind}` vs `{kind}` and trivially passes — letting a value missing a
    /// required field flow into `Token[]` and segfault on read of the missing (NULL) field. Keeping
    /// the canonical `T = Token` lets the arg-compat gate reject the omitting item with a clear
    /// diagnostic ("expected { kind: String, text: String }").
    ///
    /// The guard is TIGHT — it fires ONLY for the record-omission shape — so the legitimate
    /// last-wins-clobber cases are untouched: a narrower NUMERIC item widening into `T` (e.g.
    /// `push(uint8Buf, int32Val)`, where the runtime coerces the Int32 down to a byte) still
    /// clobbers `T` to the wider numeric type, and unrelated-type conflicts still last-wins.
    pub(crate) fn collect_and_save_subs_no_clobber(&mut self, pattern: &Type, actual: &Type, local: &mut std::collections::HashMap<u32, Type>) {
        let mut fresh = std::collections::HashMap::new();
        collect_type_subs(pattern, actual, &mut fresh);
        for (id, ty) in fresh {
            match local.get(&id) {
                // Keep the established binding when the new candidate is assignable to it
                // (the narrower-item-widens-into-T case, e.g. UInt8 fields into an Int32 record).
                Some(existing) if self.arg_compatible(&ty, existing) => {}
                // Keep the established record binding when the candidate record OMITS a required
                // field it has (the soundness guard above) — so the arg-compat gate rejects the
                // deficient argument instead of silently rebinding `T` past it.
                Some(existing) if Self::omits_required_field(&ty, existing) => {}
                _ => {
                    local.insert(id, ty.clone());
                    if id < 9000 && !self.protected_type_vars.contains(&id) {
                        self.solved_type_vars.entry(id).or_insert(ty);
                    }
                }
            }
        }
    }

    /// True when `candidate` is an object that omits a required (non-nullable) field present in the
    /// `existing` object binding — i.e. assigning `candidate` where `existing` is expected would be
    /// the unsound width-OMISSION case (the value is missing a field the type requires). EXTRAS on
    /// the candidate are fine (width subtyping); only a MISSING required field trips this. Non-object
    /// pairs never trip it, so numeric/other conflicts keep their last-wins behaviour.
    fn omits_required_field(candidate: &Type, existing: &Type) -> bool {
        if let (
            Type::Object { fields: cand_fields, .. },
            Type::Object { fields: exist_fields, .. },
        ) = (candidate, existing)
        {
            exist_fields.iter().any(|(key, exist_ty)| {
                !cand_fields.contains_key(key) && !Self::type_includes_null(exist_ty)
            })
        } else {
            false
        }
    }

    /// True if `t` is `Null`, or a union that includes `Null` (an optional field type).
    fn type_includes_null(t: &Type) -> bool {
        match t {
            Type::Null => true,
            Type::Union(variants) => variants.iter().any(Self::type_includes_null),
            _ => false,
        }
    }
}
