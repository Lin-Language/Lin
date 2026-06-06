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

    /// True when a function body is, or ends in, a direct `lin_array_allocate(n)` call — the one
    /// allocation intrinsic whose representation the compiler fully controls (Phase 4.5). Used to
    /// decide whether to CHECK the body against an `Array(_)` declared return so the fresh
    /// allocation's Json-wildcard element type is refined to the declared element.
    fn body_is_fresh_array_allocate(body: &lin_parse::ast::Expr) -> bool {
        use lin_parse::ast::Expr;
        match body {
            Expr::Call { func, .. } => matches!(func.as_ref(), Expr::Ident(n, _) if n == "lin_array_allocate"),
            Expr::Block(_, final_expr, _) => Self::body_is_fresh_array_allocate(final_expr),
            _ => false,
        }
    }

    /// Phase 4.5b: the binding name of an INTERMEDIATE `lin_array_allocate` builder body — the
    /// common map-shape combinator idiom:
    ///
    /// ```lin
    /// <T, U>(arr: T[], f: (T) => U): U[] =>
    ///   val result = lin_array_allocate(n)   // fresh, compiler-controlled allocation
    ///   ...                                  // write into result[i]
    ///   result                               // returned bare
    /// ```
    ///
    /// Returns `Some("result")` only when the body is a `Block` whose FINAL expression is a bare
    /// `Ident(name)` AND one of the block's statements is `val name = lin_array_allocate(..)`.
    /// Used to PIN that binding's element type to the declared-return element so monomorphization
    /// produces a flat allocation matching the flat reader (see `infer_function`).
    ///
    /// STRICT GATING (Phase 4.5 safety rule): the binding must be a direct `lin_array_allocate`
    /// call — never a slice/concat/parse or any other `Json[]`-returning call whose runtime
    /// representation the compiler does NOT control. A wrongly-pinned non-flat producer would be
    /// read flat by the concrete consumer → garbage. When the pattern doesn't match exactly, this
    /// returns `None` and the binding stays `Array(MAX)` (tagged, correct).
    fn intermediate_array_allocate_binding(body: &lin_parse::ast::Expr) -> Option<String> {
        use lin_parse::ast::{Expr, Pattern, Stmt};
        let Expr::Block(stmts, final_expr, _) = body else { return None };
        // The block's value must be a bare identifier.
        let Expr::Ident(returned, _) = final_expr.as_ref() else { return None };
        // That identifier must be bound by a `val <returned> = lin_array_allocate(..)` in the block.
        for stmt in stmts {
            if let Stmt::Val { pattern: Pattern::Ident(name, _), type_ann, value, .. } = stmt {
                if name == returned {
                    // A user-supplied annotation already fixes the element type — don't override it.
                    if type_ann.is_some() {
                        return None;
                    }
                    if let Expr::Call { func, .. } = value {
                        if matches!(func.as_ref(), Expr::Ident(n, _) if n == "lin_array_allocate") {
                            return Some(returned.clone());
                        }
                    }
                    // The name is rebound to something else — bail (stay correct/tagged).
                    return None;
                }
            }
        }
        None
    }

    /// Resolve a parameter/return type annotation, lowering every `Number` occurrence to a FRESH
    /// numerically-constrained quantified generic TypeVar (ADR-014, reversed). `Number` is sugar for
    /// an implicit `<T: numeric>`: the body type-checks because the bound guarantees a numeric family
    /// (arithmetic on the var is permitted in `infer_binary_op`), and monomorphization specializes the
    /// var per call site to the concrete family — native unboxed ops, zero runtime cost. Each `Number`
    /// (even nested, e.g. `Number[]` or `(Number) => Number`) mints its OWN id, so independent numeric
    /// params don't get tied together. A non-`Number` annotation resolves exactly as before.
    /// True for an annotation that is exactly the bare name `Number`. A `Number` RETURN annotation
    /// is special-cased (ADR-014, reversed): unlike a `Number` PARAMETER (which mints a fresh bound
    /// var the call site pins from the argument), a `Number` return can't be pinned from arguments,
    /// so it would be an un-inferrable free var. Instead the function's actual return is taken from
    /// the BODY's (already numeric, bound-guaranteed) type, and we only check the body is numeric.
    pub(crate) fn is_bare_number(type_ann: &lin_parse::ast::TypeExpr) -> bool {
        matches!(type_ann, lin_parse::ast::TypeExpr::Named(n, _) if n == "Number")
    }

    pub(crate) fn resolve_type_with_number(
        &mut self,
        type_ann: &lin_parse::ast::TypeExpr,
    ) -> Result<Type, String> {
        let env = self.env.clone();
        self.resolve_type_with_number_in(type_ann, &env)
    }

    /// As `resolve_type_with_number`, but resolves non-`Number` names against an explicitly supplied
    /// `env` (used by `forward_declare_functions`, whose scratch env binds the function's `<T>` type
    /// params). The numeric-bound mint still threads through `self` so the constraint table and id
    /// counter stay shared.
    pub(crate) fn resolve_type_with_number_in(
        &mut self,
        type_ann: &lin_parse::ast::TypeExpr,
        env: &crate::env::TypeEnv,
    ) -> Result<Type, String> {
        use lin_parse::ast::TypeExpr;
        match type_ann {
            TypeExpr::Named(name, _) if name == "Number" => {
                let id = self.next_generic_tv;
                self.next_generic_tv += 1;
                self.numeric_tvs.insert(id);
                Ok(Type::TypeVar(id))
            }
            TypeExpr::Array(inner, _) => {
                Ok(Type::Array(Box::new(self.resolve_type_with_number_in(inner, env)?)))
            }
            TypeExpr::FixedArray(types, _) => Ok(Type::FixedArray(
                types.iter().map(|t| self.resolve_type_with_number_in(t, env)).collect::<Result<_, _>>()?,
            )),
            TypeExpr::Union(types, _) | TypeExpr::TaggedUnion(types, _) => {
                let resolved: Result<Vec<Type>, String> =
                    types.iter().map(|t| self.resolve_type_with_number_in(t, env)).collect();
                Ok(Type::flatten_union(resolved?))
            }
            TypeExpr::Function(params, ret, _) => {
                let param_types: Result<Vec<Type>, String> =
                    params.iter().map(|p| self.resolve_type_with_number_in(p, env)).collect();
                let ret_type = self.resolve_type_with_number_in(ret, env)?;
                Ok(Type::func(param_types?, ret_type))
            }
            TypeExpr::Object(fields, _) => {
                let mut resolved = indexmap::IndexMap::new();
                for (key, te) in fields {
                    resolved.insert(key.clone(), self.resolve_type_with_number_in(te, env)?);
                }
                // Inline (anonymous) object annotation → UNSEALED.
                Ok(Type::object(resolved))
            }
            // Generic constructors (`Iterator<Number>`, alias applications, …) and all other leaves
            // resolve through the standard path. A `Number` nested inside a generic type ARGUMENT is
            // not lowered to a constrained var here (a documented first-cut limitation); it would
            // resolve via the standard resolver where `Number` is still unknown.
            _ => resolve_type(type_ann, env),
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
                self.resolve_type_with_number(type_ann).map_err(|e| Diagnostic::error(span, e))?
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
                    let field_ty = if let Type::Object { fields: ref obj_fields, .. } = ty {
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
        // string-literal refinement (ADR-034) — see infer_function_with_hints for the rationale.
        // A bare `Number` RETURN is treated as un-annotated (the body's numeric type flows through);
        // we record `number_return` so the post-check can require the body to be numeric.
        let number_return = return_type.as_ref().is_some_and(|rt| Self::is_bare_number(rt));
        let declared_ret = match return_type {
            Some(rt) if !Self::is_bare_number(rt) => {
                Some(self.resolve_type_with_number(rt).map_err(|e| Diagnostic::error(span, e))?)
            }
            _ => None,
        };
        // CHECK the body bidirectionally against the declared return type when that type is
        // structured (an object/named/union, or one mentioning a `StrLit` singleton). This pushes
        // the expected type into `if`/`match` arms (see `check_branch_against`), which:
        //   - refines object/string literals against the declared shape (ADR-034), and
        //   - lets one arm yield a `Json` value while another yields a concrete object literal,
        //     each checked against the declared return — fixing the match-arm-union-vs-declared-
        //     object bug (previously the arms were inferred independently, unioned into
        //     `Json | {concrete}`, and that union rejected against `R`).
        // `checked_against_declared` records that `check_expr` already enforced compatibility, so
        // the post-pass `types_compatible(body_ty, declared)` re-check (which would reject the
        // surviving `Json | {R}` union type) is skipped.
        let mut checked_against_declared = false;
        // Phase 4.5b: pin an INTERMEDIATE `val result = lin_array_allocate(n)` binding's element
        // type to the declared-return element. Save/restore the hint so nested functions and
        // siblings are unaffected (hygiene). See `intermediate_array_allocate_binding` + ADR.
        let prev_alloc_hint = self.array_alloc_elem_hint.take();
        if let Some(Type::Array(elem)) = &declared_ret {
            let elem_is_wildcard = matches!(elem.as_ref(), Type::TypeVar(n) if *n == u32::MAX);
            if !elem_is_wildcard {
                if let Some(binding) = Self::intermediate_array_allocate_binding(body) {
                    self.array_alloc_elem_hint = Some((binding, (**elem).clone()));
                }
            }
        }
        let typed_body_raw = match &declared_ret {
            Some(declared) if super::expr::expected_pushes_into_branches(declared) => {
                checked_against_declared = true;
                self.check_expr(body, declared)?
            }
            // Phase 4.5: a generic combinator whose body is `=> arrayAllocate(n)` and whose
            // declared return is an `Array(_)` must be CHECKED against that return so the fresh
            // allocation's Json-wildcard element type is refined to the declared element (the
            // generic param `T`). Monomorphization then turns `Array(T)` into a concrete
            // `Array(Int32)`, letting codegen emit a flat allocation that matches the flat reader.
            // Inferring the body (the default) would leave it `Array(MAX)` (tagged) → mismatch.
            // Gated to the allocation intrinsic so no other array-returning body changes behaviour.
            Some(declared @ Type::Array(_)) if Self::body_is_fresh_array_allocate(body) => {
                self.check_expr(body, declared)?
            }
            _ => self.infer_expr(body)?,
        };
        self.array_alloc_elem_hint = prev_alloc_hint;
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
            // A bare `Number` return (ADR-014, reversed): require the body to be numeric (or itself
            // a numeric-bounded var that monomorphizes to a numeric family), then return the body's
            // type so the call site can pin it. A non-numeric body is rejected here.
            if number_return
                && !body_ty.is_numeric()
                && !matches!(body_ty, Type::TypeVar(id) if self.numeric_tvs.contains(&id))
            {
                return Err(Diagnostic::error(
                    span,
                    format!("Function body has type {}, declared return type is Number (a numeric type)", body_ty),
                ).with_help("a `Number` return requires the body to evaluate to a numeric family".to_string()));
            }
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
                // NESTED-`Number` UNIFICATION (ADR-014, reversed): a bare `Number` lambda parameter
                // whose EXPECTED type (from the enclosing combinator — e.g. `.map`'s callback param,
                // which is the receiver's element type) is ALSO a numeric-bounded var should REUSE
                // that var rather than mint a fresh independent one. This ties the inner callback's
                // numeric family to the array element it consumes, so a function like
                // `(xs: Number[]) => xs.map((v: Number) => v*2)` has its return pinned by `xs`'s
                // element family at the call site instead of leaving an un-inferrable free body var.
                // Both bounds are `numeric`, so reusing the outer var is sound (same constraint). A
                // non-`Number` annotation, or a hint that isn't a numeric-bounded var, resolves
                // normally (each `Number` otherwise mints its own var — the independent-param design).
                if Self::is_bare_number(type_ann)
                    && i < expected_params.len()
                    && matches!(&expected_params[i], Type::TypeVar(id) if self.numeric_tvs.contains(id))
                {
                    expected_params[i].clone()
                } else {
                    self.resolve_type_with_number(type_ann).map_err(|e| Diagnostic::error(span, e))?
                }
            } else if i < expected_params.len() && !matches!(expected_params[i], Type::TypeVar(_)) {
                expected_params[i].clone()
            } else if i < expected_params.len()
                && matches!(expected_params[i], Type::TypeVar(n) if n == u32::MAX)
            {
                // The expected param is the `Json` wildcard (`TypeVar(MAX)`) — e.g. the element
                // param of the `for` wrapper's `(Json, Int32) => Json` callback. Bind the param
                // DIRECTLY to `Json` rather than minting a fresh inference var: a fresh var here
                // would be left unsolved for an ambiguous element (a `[]`+push array) and default
                // to the wrong scalar (the regression that surfaced as an `i8` element). This
                // matches the opaque `Function` behaviour, where a Json-wildcard param binds to
                // Json directly. (A non-MAX generic `T`, e.g. map/filter's element, still mints a
                // fresh var below so the receiver can pin it.)
                Type::TypeVar(u32::MAX)
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
                    let field_ty = if let Type::Object { fields: ref obj_fields, .. } = ty {
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

        // OPTIONAL ITERATOR-CALLBACK INDEX PARAM (arity-width subtyping, in-place adapter).
        // The iterable combinators expect a callback whose trailing parameter(s) are the 0-based
        // `Int32` SOURCE index (`for`/`map`/`filter`/`while`: `(item, i)`; `reduce`: `(acc, item, i)`).
        // A user may write a SHORTER callback (`item => …`) — compat.rs's arity-width subtyping makes
        // that assignable. But the COMPILED closure must still have the full arity the caller invokes
        // it with: the intrinsic loop passes the index unconditionally, and a Tier-B wrapper calls
        // `f(item, idx)`. So when the EXPECTED type declares MORE params than the lambda, and each
        // extra trailing expected param is `Int32`, PAD the typed function with synthetic, unused
        // `Int32` parameters (`__idx{n}`). This is the in-place "adapter": the runtime closure gains
        // the trailing index slot(s) the caller supplies, the body ignores them, and the inline fast
        // path (ADR-044) is preserved (no captures added, no thunk). A non-`Int32` extra expected
        // param is NOT padded (it would be a genuine arity mismatch, rejected at the call site).
        if params.len() < expected_params.len() {
            for (k, extra) in expected_params[params.len()..].iter().enumerate() {
                if matches!(extra, Type::Int32) {
                    let name = format!("__idx{}", k);
                    let slot = self.env.define(name.clone(), Type::Int32, false);
                    typed_params.push(TypedParam {
                        slot,
                        name,
                        ty: Type::Int32,
                        default: None,
                    });
                }
            }
        }
        // INDEX-PARAM ANNOTATION VALIDATION: if the user explicitly annotated the parameter that
        // lines up with the EXPECTED TRAILING `Int32` index slot, it must be annotated `Int32` (an
        // unannotated param already infers `Int32` via the `expected_params[i]` hint above). Any other
        // annotation (e.g. `(x, i: String) => …`) is a clear, dedicated error rather than the generic
        // "Argument has type …" mismatch surfaced later at the call site.
        //
        // Restricted to the LAST expected slot (`i == expected_params.len() - 1`): the index is
        // always the final callback parameter (`map`/`filter`'s `(item, idx)`, `reduce`'s
        // `(acc, item, idx)`). Without this guard a NON-index leading param whose substituted
        // generic happens to resolve to `Int32` (e.g. `map`'s element `T = Int32` for an `Int32[]`)
        // would be misreported as "the index parameter must be Int32" when annotated with a wrong
        // type — masking the precise "expected Int32, got String" arg-mismatch the call site emits.
        for (i, param) in params.iter().enumerate() {
            if !expected_params.is_empty()
                && i == expected_params.len() - 1
                && matches!(expected_params[i], Type::Int32)
                && param.type_ann.is_some()
                && !matches!(typed_params[i].ty, Type::Int32)
            {
                let pspan = match &param.pattern {
                    Pattern::Ident(_, s) => *s,
                    _ => span,
                };
                return Err(Diagnostic::error(
                    pspan,
                    "the index parameter of an iterator callback must be Int32".to_string(),
                )
                .with_help(
                    "the optional trailing parameter of for/map/filter/reduce/while (and find/some/\
                     every/…) is the 0-based source index; annotate it `Int32` or leave it \
                     unannotated".to_string(),
                ));
            }
        }

        let prev_fn = self.current_function.take();
        let prev_tail = self.in_tail_position;
        self.current_function = fn_name.map(|s| s.to_string());
        self.in_tail_position = self.current_function.is_some();

        // Resolve the declared return type up front so the body can be CHECKED against it
        // (bidirectional). This pushes the expected type into the body — needed for singleton
        // string-literal refinement (ADR-034): a `{ "type": "success", .. }` literal in the
        // body narrows its discriminant to the expected `StrLit` variant. Falls back to plain
        // inference when there is no annotation. A bare `Number` return is treated as un-annotated
        // (see `infer_function`); the body's numeric type flows through.
        let number_return = return_type.as_ref().is_some_and(|rt| Self::is_bare_number(rt));
        let declared_ret = match return_type {
            Some(rt) if !Self::is_bare_number(rt) => {
                Some(self.resolve_type_with_number(rt).map_err(|e| Diagnostic::error(span, e))?)
            }
            _ => None,
        };
        // See `infer_function` for the rationale: push a structured declared return type into the
        // body's `if`/`match` arms (fixes the match-arm-union-vs-declared-object bug).
        let mut checked_against_declared = false;
        // Phase 4.5b: pin an INTERMEDIATE alloc binding (see `infer_function`).
        let prev_alloc_hint = self.array_alloc_elem_hint.take();
        if let Some(Type::Array(elem)) = &declared_ret {
            let elem_is_wildcard = matches!(elem.as_ref(), Type::TypeVar(n) if *n == u32::MAX);
            if !elem_is_wildcard {
                if let Some(binding) = Self::intermediate_array_allocate_binding(body) {
                    self.array_alloc_elem_hint = Some((binding, (**elem).clone()));
                }
            }
        }
        let typed_body_raw = match &declared_ret {
            Some(declared) if super::expr::expected_pushes_into_branches(declared) => {
                checked_against_declared = true;
                self.check_expr(body, declared)?
            }
            // Phase 4.5: a generic combinator whose body is `=> arrayAllocate(n)` and whose
            // declared return is an `Array(_)` must be CHECKED against that return so the fresh
            // allocation's Json-wildcard element type is refined to the declared element (the
            // generic param `T`). Monomorphization then turns `Array(T)` into a concrete
            // `Array(Int32)`, letting codegen emit a flat allocation that matches the flat reader.
            // Inferring the body (the default) would leave it `Array(MAX)` (tagged) → mismatch.
            // Gated to the allocation intrinsic so no other array-returning body changes behaviour.
            Some(declared @ Type::Array(_)) if Self::body_is_fresh_array_allocate(body) => {
                self.check_expr(body, declared)?
            }
            _ => self.infer_expr(body)?,
        };
        self.array_alloc_elem_hint = prev_alloc_hint;
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

        // A bare `Number` return (ADR-014, reversed): require the body to be numeric (or a
        // numeric-bounded var), then surface the body's type so the call site can pin it.
        if number_return {
            if !body_ty.is_numeric()
                && !matches!(body_ty, Type::TypeVar(id) if self.numeric_tvs.contains(&id))
            {
                return Err(Diagnostic::error(
                    span,
                    format!("Function body has type {}, declared return type is Number (a numeric type)", body_ty),
                ).with_help("a `Number` return requires the body to evaluate to a numeric family".to_string()));
            }
            return Ok(TypedExpr::Function {
                name: None,
                params: typed_params,
                body: Box::new(typed_body),
                ret_type: body_ty,
                captures,
                span,
            });
        }

        let ret_type = if let Some(declared) = declared_ret {
            if !checked_against_declared && !self.types_compatible(&body_ty, &declared) {
                return Err(Diagnostic::error(span, format!(
                    "Function body has type {}, declared return type is {}", body_ty, declared
                )));
            }
            declared
        } else if matches!(expected_ret, Type::TypeVar(id)
            if *id >= 9001 && *id != u32::MAX)
            && (!matches!(body_ty, Type::TypeVar(_))
                || matches!(body_ty, Type::TypeVar(id) if self.numeric_tvs.contains(&id)))
        {
            // Expected return is a QUANTIFIED GENERIC type parameter (`<U>`, id ≥ 9001) and the
            // body has a concrete type: surface the concrete `body_ty` as the lambda's return.
            // This is what lets a higher-order generic call (`mymap(arr, x => x*2)` where
            // `mymap`'s `f: (T) => U`) bind `U` from the lambda body — the call site's
            // `collect_and_save_subs` reads the lambda's concrete return and the result type
            // `U[]` becomes `Int32[]`, so monomorphization can specialize. Forcing the bare
            // generic TypeVar here (as the polymorphic-slot case below does) would leave `U`
            // uninferrable and the call would fall back to a boxed copy.
            // A body that is a numeric-bounded `Number` var (ADR-014, reversed) is ALSO surfaced
            // here (not a free polymorphic slot): it pins the combinator's `U` to that var so a
            // nested `(xs: Number[]) => xs.map((v: Number) => v*2)` propagates the element family
            // (`v` reuses `xs`'s element var) into the return — otherwise `U` stays an independent
            // free var and the outer call can't infer it (the bug-#4 nested-callback case).
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
