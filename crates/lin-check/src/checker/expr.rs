use indexmap::IndexMap;
use lin_common::{Diagnostic, Span};
use lin_parse::ast::{Expr, MatchArm, ObjectField, Stmt, StringPart};

use super::Checker;
use super::helpers::{check_int_literal_fits, default_int_literal_type, suffix_to_type, unify_types};
use crate::resolve::resolve_type;
use crate::typed_ir::*;
use crate::types::Type;

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
            return Ok(TypedExpr::MakeArray {
                elements: typed_elements,
                ty: Type::Array(expected_elem.clone()),
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

        // Propagate the expected type into the final expression of a block — both for `StrLit`
        // singleton refinement (ADR-034) and for a non-default scalar width, so a block whose tail
        // is a bare numeric literal (`(): Int64 => …; 28`) re-types it to the declared width
        // instead of inferring the `Int32`/`Float64` default (codegen IR-width mismatch otherwise).
        if let (Expr::Block(stmts, final_expr, span, _), true) =
            (expr, type_mentions_strlit(expected) || expected_pushes_scalar_width(expected))
        {
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
            Expr::UnaryOp { op, operand, span } => self.infer_unary_op(*op, operand, *span),
            Expr::Call { func, args, partial, span, .. }  => self.infer_call(func, args, *partial, *span),
            Expr::DotCall { receiver, method, args, partial, span, .. } => self.infer_dot_call(receiver, method, args, *partial, *span),
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
            // Typed index-signature map `{ String: T }` (ADR-055): a key access yields `T | Null`
            // (the missing-key → Null safe-bracket rule, §6.1). No per-key field tracking — the
            // key set is dynamic by construction.
            Type::Map(val_ty) => Type::flatten_union(vec![(**val_ty).clone(), Type::Null]),
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
            Type::Named(_) => {
                if let Some(resolved) = self.resolve_named_body(&obj_ty) {
                    return self.infer_index_into(typed_obj, typed_key, &resolved, span);
                }
                return Err(Diagnostic::error(span, format!("Cannot index into type {}", obj_ty)));
            }
            _ => return Err(Diagnostic::error(span, format!("Cannot index into type {}", obj_ty))),
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

    /// The body of `infer_index`'s result-type computation, factored out so the `Type::Named` arm
    /// can recurse after resolving the alias. Re-runs `infer_index` on an already-typed object and
    /// key against an explicitly-provided object type (`obj_ty`). This is only entered from the
    /// `Named` arm with a freshly-resolved concrete body, so a `Named` arriving here again is a
    /// genuine cycle and yields the "Cannot index" error.
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
                    Type::Union(vec![Type::Union(fields.values().cloned().collect()), Type::Null])
                }
            }
            Type::Map(val_ty) => Type::flatten_union(vec![(**val_ty).clone(), Type::Null]),
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
                return Err(Diagnostic::error(span, format!("Cannot index into type {}", other)))
            }
        };
        Ok(TypedExpr::Index { object: Box::new(typed_obj), key: Box::new(typed_key), result_type, span })
    }

    /// Flow-narrowing for an `if`/`else` condition: when it is a type/null test on a simple
    /// identifier whose static type is a union, narrow that binding to the COMPLEMENT (`union minus
    /// X`) in the branch where `X` is EXCLUDED. Generalizes the old null-only form to any `is X`
    /// test (incl. the structural `Error` member).
    ///
    /// Returns `Some((name, narrowed_ty, narrow_in_then))`:
    ///   - `v == null` / `v is X`  → `X` excluded in the ELSE branch (`narrow_in_then = false`)
    ///   - `v != null`             → `Null` excluded in the THEN branch (`narrow_in_then = true`)
    ///
    /// Only fires when the binding's static type is a union that contains the tested type as an
    /// exact member (so the complement is well-defined and non-empty — see `without_variant`).
    /// Composes with — and does not replace — the existing `match`/`is` narrowing.
    fn null_test_narrowing(&self, condition: &Expr) -> Option<(String, Type, bool)> {
        // `x == null` / `x != null` against the `null` literal (either operand order).
        let (name, excluded, narrow_in_then) = match condition {
            Expr::BinaryOp { left, op, right, .. }
                if matches!(op, lin_parse::ast::BinOp::Eq | lin_parse::ast::BinOp::NotEq) =>
            {
                let ident = match (left.as_ref(), right.as_ref()) {
                    (Expr::Ident(n, _), Expr::NullLit(_)) => Some(n),
                    (Expr::NullLit(_), Expr::Ident(n, _)) => Some(n),
                    _ => None,
                }?;
                // `== null`: Null holds in THEN, excluded in ELSE → narrow in ELSE.
                // `!= null`: Null excluded in THEN → narrow in THEN.
                let narrow_in_then = matches!(op, lin_parse::ast::BinOp::NotEq);
                (ident.clone(), Type::Null, narrow_in_then)
            }
            // `x is X`: `X` holds in THEN, excluded in ELSE → narrow in ELSE. `X` may be `Null`,
            // a scalar (`Int32`), a named/structural type, or `Error` (the structural alias).
            Expr::Is { expr, pattern, .. } => {
                let ident = match expr.as_ref() {
                    Expr::Ident(n, _) => n,
                    _ => return None,
                };
                let excluded = match pattern.as_ref() {
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
                (ident.clone(), excluded, false)
            }
            _ => return None,
        };
        let info = self.env.lookup(&name)?;
        let narrowed = info.ty.without_variant(&excluded)?;
        Some((name, narrowed, narrow_in_then))
    }

    /// Apply a flow-narrowing (from `null_test_narrowing`) within the CURRENT scope if it targets
    /// the branch being entered. `entering_then` says which branch we are about to check; the
    /// narrowing's third element says which branch excludes the tested type. Reuses the original
    /// slot via `define_narrowed` so `LocalGet` reads the same TaggedVal pointer (the value is
    /// bit-identical — only the static type tightens). Must be called immediately after
    /// `push_scope` for the branch and undone by the matching `pop_scope`.
    fn apply_null_narrowing(&mut self, narrowing: &Option<(String, Type, bool)>, entering_then: bool) {
        if let Some((name, narrowed_ty, narrow_in_then)) = narrowing {
            if *narrow_in_then == entering_then {
                if let Some(info) = self.env.lookup(name) {
                    let slot = info.slot;
                    self.env.define_narrowed(name.clone(), narrowed_ty.clone(), slot);
                }
            }
        }
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
        // Flow-narrow a `T | Null` binding in the branch that excludes Null (`== null`/`is Null`
        // → else; `!= null` → then). The narrowing is scoped: pushed before the relevant branch
        // and popped after, so it never leaks past the `if`.
        let narrowing = self.null_test_narrowing(condition);
        let typed_then = {
            self.env.push_scope();
            self.apply_null_narrowing(&narrowing, true);
            let r = self.infer_expr(then_branch);
            self.env.pop_scope();
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
            r?
        };
        // Merge: union of both branches' consumed sets.
        self.consumed_streams.extend(consumed_then);
        let then_ty = typed_then.ty();
        let else_ty = typed_else.ty();
        // A branch typed `Null` (or a TypeVar that is structurally compatible with everything,
        // incl. Null) must NOT collapse the merged type onto the OTHER branch via
        // `types_compatible`. The old collapse silently dropped the value-producing branch: e.g.
        // `if cond then arr[i] else null` (where `arr[i]` is a generic `T` or a `Json` element)
        // computed `result_type = Null`, so lowering built an if-merge phi/return typed `Null`
        // and the value branch was replaced by `null` at runtime (`at` returning null on a valid
        // index). When exactly one branch is `Null` and the other is not, form the union so both
        // branches survive (and survive monomorphization substitution of any generic `T`).
        // Two DISTINCT quantified-generic type parameters (e.g. `T` and `D` in
        // `<T, D>(…): T | D`, ids ≥ 9000 and ≠ the Json wildcard) must NOT be collapsed onto each
        // other by the `types_compatible` arms below. An unconstrained TypeVar unifies with
        // anything, so `types_compatible(T, D)` is vacuously true and would pick ONE of them —
        // silently dropping the other arm's type. For an `if … then arr[i] /*: T*/ else default
        // /*: D*/` body that erases the union to a single param, so after monomorphization the two
        // arms (a flat-scalar element vs a boxed default) disagree on representation and codegen
        // emits a malformed phi. Keep them as an honest `T | D` union so both arms survive
        // substitution and each gets boxed into the union representation. Same-id (`T | T`) still
        // collapses via the normal path; a generic-vs-concrete pairing also keeps its existing
        // behaviour (only BOTH being distinct quantified params triggers this).
        // Quantified-generic TypeVar ids are minted ≥ 9001 (`Checker::next_generic_tv`), above the
        // intrinsic-slot range; the Json wildcard is `u32::MAX`. A bound type PARAMETER therefore
        // lives in `[GENERIC_TV_BASE, u32::MAX)`.
        const GENERIC_TV_BASE: u32 = 9000;
        let distinct_generic_params = matches!(
            (&then_ty, &else_ty),
            (Type::TypeVar(a), Type::TypeVar(b))
                if a != b
                    && *a >= GENERIC_TV_BASE && *a != u32::MAX
                    && *b >= GENERIC_TV_BASE && *b != u32::MAX
        );
        let result_type = if distinct_generic_params {
            Type::flatten_union(vec![then_ty, else_ty])
        } else if (then_ty == Type::Null) != (else_ty == Type::Null) {
            // Exactly one branch is the literal Null type. Keep both as a union so the
            // value-producing branch survives — UNLESS the other branch is `Json` (the dynamic
            // top type, `TypeVar(u32::MAX)`), which already subsumes `Null`: there `Json | Null`
            // is redundant and would leak the internal `?T4294967295` sentinel into diagnostics,
            // so collapse to `Json` (the pre-change behaviour for this specific pairing).
            let other = if then_ty == Type::Null { &else_ty } else { &then_ty };
            if is_json_dynamic(other) {
                other.clone()
            } else {
                Type::flatten_union(vec![then_ty, else_ty])
            }
        } else if self.types_compatible(&then_ty, &else_ty) {
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
    /// it deliberately does NOT relax `is_compatible_env` (ADR-045 still rejects a direct
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
            unify_types(&arm_types)
        };

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
                        return self.check_object_against(fields, &body, span);
                    }
                }
                Ok(None)
            }
            Type::Object { fields: expected_fields, .. } => {
                // Take over with directed field-by-field checking when it would actually change
                // the outcome — otherwise stay on the existing undirected inference path so plain
                // structural objects are unaffected. Two cases need directing:
                //   (1) a `StrLit` field — so a discriminant literal narrows to its singleton;
                //   (2) a `Map` field (possibly nested inside a further record field) — so an
                //       object literal in that field position key-widens to `{ String: T }`
                //       (a `LinMap`) instead of being inferred to its own fixed-record type.
                if !expected_fields.values().any(|t| expected_field_needs_directing(t)) {
                    return Ok(None);
                }
                Ok(Some(self.check_object_fields(fields, expected_fields, span)?))
            }
            // An object literal checked against a typed index-signature map `{ String: T }`
            // (ADR-055): each literal value must be `T`; the result is typed `Map(T)` and lowered
            // into a `LinMap`. The empty `{}` literal is the common case (`var m: { String: T } =
            // {}`), which produces an empty hashed map of the right type — this is how `{}` infers
            // a map from its assignment-target / return-type context.
            Type::Map(val_ty) => {
                let mut typed_fields = Vec::new();
                for field in fields {
                    if let ObjectField::Pair(Expr::StringLit(key, _), val_expr) = field {
                        let typed_val = self.check_expr(val_expr, val_ty)?;
                        typed_fields.push((key.clone(), typed_val));
                    } else {
                        // A non-literal key or a dynamic field shape — defer to ordinary inference.
                        return Ok(None);
                    }
                }
                Ok(Some(TypedExpr::MakeObject {
                    fields: typed_fields,
                    spreads: Vec::new(),
                    ty: Type::Map(val_ty.clone()),
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
                    obj_type.insert(key.clone(), typed_val.ty());
                    typed_fields.push((key.clone(), typed_val));
                }
            }
        }
        self.in_tail_position = saved_tail;
        // The refined literal's own type stays UNSEALED (the seal lives on the expected named type;
        // Stage 1 inserts the projection at the boundary). Inert in Stage 0.5.
        Ok(TypedExpr::MakeObject { fields: typed_fields, spreads: Vec::new(), ty: Type::object(obj_type), span })
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
        Ok(TypedExpr::LocalSet { slot, value: Box::new(typed_value), ty: expected_ty, span })
    }

    pub(crate) fn infer_index_assign(&mut self, object: &Expr, key: &Expr, value: &Expr, span: Span) -> Result<TypedExpr, Diagnostic> {
        let typed_obj = self.infer_expr(object)?;
        let typed_key = self.infer_expr(key)?;
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
            // Typed index-signature map `{ String: T }` (ADR-055): the key must be a String and
            // the value must be `T`.
            Type::Map(val_ty) => {
                let key_ty = typed_key.ty();
                if !key_ty.is_string_ish() && !matches!(key_ty, Type::TypeVar(_)) {
                    return Err(Diagnostic::error(
                        span,
                        format!("a `{}` is keyed by String, but the key is `{}`", obj_ty, key_ty),
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

/// True if `ty` is the dynamic/top `Json` type (`TypeVar(u32::MAX)`). A value of this type is
/// accept-any in checked branch/arm position (see `check_branch_against`).
pub(crate) fn is_json_dynamic(ty: &Type) -> bool {
    matches!(ty, Type::TypeVar(n) if *n == u32::MAX)
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
/// — or transitively (in a record-field position) contains — either a `StrLit` singleton (so a
/// discriminant narrows) or a `Map` (so a record literal key-widens to `{ String: T }`). The
/// transitive walk handles nested records like `{ headers: { String: String } }` where the
/// outer record has no direct `StrLit`/`Map` field but a nested field does.
pub(crate) fn expected_field_needs_directing(ty: &Type) -> bool {
    match ty {
        Type::StrLit(_) | Type::Map(_) => true,
        Type::Object { fields, .. } => fields.values().any(expected_field_needs_directing),
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
        Type::Array(inner) | Type::Iterator(inner) | Type::Shared(inner) | Type::Stream(inner) => type_mentions_strlit(inner),
        Type::FixedArray(elems) => elems.iter().any(type_mentions_strlit),
        Type::Union(variants) => variants.iter().any(type_mentions_strlit),
        Type::Object { fields, .. } => fields.values().any(type_mentions_strlit),
        Type::Function { params, ret, .. } => {
            params.iter().any(type_mentions_strlit) || type_mentions_strlit(ret)
        }
        _ => false,
    }
}
