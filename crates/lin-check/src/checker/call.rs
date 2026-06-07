use lin_common::{Diagnostic, Span};
use lin_parse::ast::Expr;

use super::Checker;
use super::helpers::{apply_type_subs, first_mutable_capture, integer_range, is_definitely_non_transferable};
use crate::resolve::{error_type, json_type};
use crate::typed_ir::*;
use crate::types::Type;

/// Whether an argument type may satisfy a `Number` (numeric-bounded) parameter (ADR-014, reversed).
/// Accepts:
///   * a concrete numeric family (the monomorphizable case),
///   * a generic/unsolved type var (the bound flows on to the outer specialization, or is zonked to
///     a concrete family), AND
///   * the `Json` wildcard (`TypeVar(u32::MAX)`) — a dynamic `Json` value (ADR-014 §Json policy).
///     This matches the existing `Json → Int32` scalar coercion (ADR-032): a `Json` argument is
///     ACCEPTED at a `Number` parameter and monomorphizes to the default `Int32` family, unboxing
///     unchecked. Previously a DIRECT `Json` (the bare `u32::MAX` marker) was rejected here while a
///     `Json` PROJECTION (`config["k"]`, a fresh inference var) slipped past — an inconsistency now
///     removed. A `Json` holding a non-number unboxes as garbage, the SAME accepted, documented
///     unsoundness as `Json → Int32` today; `fromJson` is the validated extraction path.
/// REJECTS only genuinely non-numeric concrete types (`String`/`Bool`/`Object`/array).
/// Whether a callback's EXPECTED function type (after `apply_type_subs`) has every PARAMETER type
/// resolved to a FULLY CONCRETE type — no `TypeVar` anywhere, INCLUDING the `Json` wildcard
/// (`u32::MAX`). When so, the generic params that pin those slots have all been bound by an earlier
/// (type-pinning) argument to concrete types (e.g. `(Int32, Int32) => U` for `sort(xs, cmp)` once
/// `T = Int32` from `xs`), so the back-inferred lambda-param hints are EXACT and a body type error
/// against them is GENUINE — propagate it, closing the inference hole.
///
/// Two deliberate exclusions keep this conservative (never reject legitimate code):
///   * The RETURN type is IGNORED — `map`'s `(T, Int32) => U` legitimately leaves `U` unresolved
///     until the body determines it (`U` is solved FROM the lambda return), so a free return must
///     not force a strict check.
///   * A parameter that is (or contains) the `Json` wildcard is a PERMISSIVE/dynamic hint, not an
///     exact concrete type — e.g. `reduce`'s accumulator `U` resolving to `Json | Int32` when the
///     receiver's element is itself a `Json`-tainted inference union. Body ops there (`acc + x`)
///     are intentionally permissive under the dynamic-`Json` policy, so we keep the inference
///     fallback rather than reject. Requiring `!contains_type_var()` (which counts the wildcard)
///     excludes exactly these cases.
///   * A parameter that is (or contains) `Never` is NOT a real pin either: it comes from an EMPTY
///     collection (`[].sort((a, b) => a - b)` binds `T = Never` because there are no elements to
///     constrain it). The callback body is legitimate — there is simply no concrete element type to
///     check it against — so a `Never` param must fall back to inference rather than reject (`a - b`
///     would be "Sub to Never and Never"). `contains_never` mirrors the `contains_type_var` shape.
fn expected_fn_params_fully_pinned(expected: &Type) -> bool {
    match expected {
        Type::Function { params, .. } => {
            params.iter().all(|p| !p.contains_type_var() && !type_contains_never(p))
        }
        _ => false,
    }
}

/// Whether `ty` is, or structurally contains, `Never`. Used to exclude degenerate empty-collection
/// element pins (`T = Never`) from strict callback-body checking — see `expected_fn_params_fully_pinned`.
fn type_contains_never(ty: &Type) -> bool {
    match ty {
        Type::Never => true,
        Type::Array(t) | Type::Iterator(t) | Type::Shared(t) | Type::Stream(t) | Type::Map(t) => {
            type_contains_never(t)
        }
        Type::Union(ts) | Type::FixedArray(ts) => ts.iter().any(type_contains_never),
        Type::Function { params, ret, .. } => {
            params.iter().any(type_contains_never) || type_contains_never(ret)
        }
        Type::Object { fields, .. } => fields.values().any(type_contains_never),
        _ => false,
    }
}

fn arg_satisfies_numeric_bound(arg_ty: &Type) -> bool {
    match arg_ty {
        t if t.is_numeric() => true,
        // Any type var, INCLUDING the `Json` wildcard (`u32::MAX`): a generic/unsolved bound flows
        // on; a `Json` value monomorphizes to the default `Int32` family (see doc above).
        Type::TypeVar(_) => true,
        _ => false,
    }
}

impl Checker {
    /// Replace every `Type::Named(n)` whose alias resolves to a structural type with that resolved
    /// type, recursing into containers. Repairs a forward-declared function's return type that
    /// still carries an unresolved `Named` placeholder (mutual-recursion forward declaration). A
    /// Named that resolves back to itself (a recursive-cycle point — `resolve_type` applies the
    /// cycle guard) or to a base scalar is left untouched, so recursive types keep their cycle
    /// points and nothing widens. Containers recurse; object FIELDS are left as-is (their member
    /// types were already resolved structurally when the alias was defined, and re-walking a record
    /// risks expanding a deliberate recursive `Named` field — changing the type's identity).
    fn expand_named_aliases(&self, ty: &Type) -> Type {
        match ty {
            Type::Named(n) => match crate::resolve::resolve_type(
                &lin_parse::ast::TypeExpr::Named(n.clone(), Span::dummy()),
                &self.env,
            ) {
                Ok(resolved) if !matches!(&resolved, Type::Named(m) if m == n) => resolved,
                _ => ty.clone(),
            },
            Type::Array(inner) => Type::Array(Box::new(self.expand_named_aliases(inner))),
            Type::Iterator(inner) => Type::Iterator(Box::new(self.expand_named_aliases(inner))),
            Type::Shared(inner) => Type::Shared(Box::new(self.expand_named_aliases(inner))),
            Type::Stream(inner) => Type::Stream(Box::new(self.expand_named_aliases(inner))),
            Type::Union(variants) => {
                Type::flatten_union(variants.iter().map(|t| self.expand_named_aliases(t)).collect())
            }
            _ => ty.clone(),
        }
    }

    /// `fromJson` special form (ADR-031). `T.fromJson(value)` desugars to a DotCall and
    /// `fromJson(T, value)` to a Call; both reach here BEFORE arg0/receiver is inferred as a
    /// value (a type name like `Person` is not a runtime value). Fires only when:
    ///   * the callee/method surface name is `fromJson`, AND
    ///   * arg0/receiver is `Expr::Ident(name)` that resolves in the TYPE namespace, AND
    ///   * `fromJson` is not shadowed by a *local* (non-global) user binding.
    /// Returns `None` to defer to the normal call path (so a user 2-arg `fromJson` value, or a
    /// non-type arg0, behaves normally). Returns `Some(Err(..))` for a genuine misuse.
    fn try_from_json_special_form(
        &mut self,
        type_arg: &Expr,
        value_arg: &Expr,
        span: Span,
    ) -> Option<Result<TypedExpr, Diagnostic>> {
        // User-shadowing guard: if `fromJson` is bound in a non-global scope, the user has
        // their own local binding — defer to the normal call path entirely.
        if let Some((depth, _)) = self.env.lookup_with_depth("fromJson") {
            if depth > 0 {
                return None;
            }
        }
        // arg0 must be a bare identifier naming a type.
        let type_name = match type_arg {
            Expr::Ident(n, _) => n.as_str(),
            _ => return None,
        };
        // Resolve in the type namespace. If it is not a known type, this is not a fromJson
        // special form — defer (the normal path will report whatever is appropriate). Built-in
        // type names (String, Int32, Json, ...) resolve here even without a user `type` decl.
        let target = match crate::resolve::resolve_type(
            &lin_parse::ast::TypeExpr::Named(type_name.to_string(), span),
            &self.env,
        ) {
            Ok(t) => t,
            Err(_) => return None,
        };

        // Infer the value argument as a value; it must be Json-compatible.
        let typed_value = match self.infer_expr(value_arg) {
            Ok(t) => t,
            Err(e) => return Some(Err(e)),
        };
        let value_ty = typed_value.ty();
        if !self.types_compatible(&value_ty, &json_type()) {
            return Some(Err(Diagnostic::error(
                value_arg.span(),
                format!(
                    "fromJson expects a Json value, got {}",
                    value_ty
                ),
            )));
        }

        // Collect the resolved bodies of every Named type reachable from `target`, so codegen
        // can build the (possibly recursive) schema descriptor without a type environment.
        let mut named_defs: Vec<(String, Type)> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        self.collect_named_defs(&target, &mut seen, &mut named_defs);

        let result_type = Type::flatten_union(vec![target.clone(), error_type()]);
        Some(Ok(TypedExpr::FromJson {
            target,
            value: Box::new(typed_value),
            result_type,
            named_defs,
            span,
        }))
    }

    /// Walk `ty` collecting, for each `Type::Named(n)` reachable, the resolved body of `n` (from
    /// the type environment) into `out` (deduplicated). Recurses into each collected body so
    /// mutually-recursive named types are all captured. Recursion is bounded by `seen`.
    pub(crate) fn collect_named_defs(
        &self,
        ty: &Type,
        seen: &mut std::collections::HashSet<String>,
        out: &mut Vec<(String, Type)>,
    ) {
        match ty {
            Type::Named(n) => {
                if seen.insert(n.clone()) {
                    if let Some(decl) = self.env.lookup_type(n) {
                        if decl.params.is_empty() {
                            let body = decl.body.clone();
                            out.push((n.clone(), body.clone()));
                            self.collect_named_defs(&body, seen, out);
                        }
                    }
                }
            }
            Type::Array(inner) | Type::Iterator(inner) | Type::Stream(inner) => self.collect_named_defs(inner, seen, out),
            Type::FixedArray(elems) => {
                for e in elems {
                    self.collect_named_defs(e, seen, out);
                }
            }
            Type::Union(vs) => {
                for v in vs {
                    self.collect_named_defs(v, seen, out);
                }
            }
            Type::Object { fields, .. } => {
                for v in fields.values() {
                    self.collect_named_defs(v, seen, out);
                }
            }
            Type::Function { params, ret, .. } => {
                for p in params {
                    self.collect_named_defs(p, seen, out);
                }
                self.collect_named_defs(ret, seen, out);
            }
            _ => {}
        }
    }

    /// Receiver-dependent return typing for the std/iter combinators (unification Stage 2).
    ///
    /// The intrinsic-backed combinators (`map`/`filter`/`reduce`/`while`) accept an
    /// `Array | Iterator | Stream` receiver, but their stdlib wrapper INFERS an array-shaped return
    /// (`U[]` for map, `T[]` for filter, `U` for reduce, `Null` for while) — the eager array
    /// behaviour. When arg0 (the iterable receiver) is actually a `Stream`, the result must instead
    /// be the stream-shaped type from the return-type table:
    ///   map → Stream<U>, filter → Stream<T>, reduce → U | Error, while → Null | Error.
    /// (`for` is handled by its own wrapper, which already declares `Null` / widens to `Null|Error`.)
    ///
    /// This is keyed on the IMPORT ORIGIN `(module_path, export_name)` of the callee — NOT just the
    /// surface name — so a user-defined `map`/`reduce`/etc. is never affected, and only the genuine
    /// `std/iter` exports are re-typed. `array_ret` is the already-computed array-shaped result;
    /// `arg0_ty` is the receiver type. Returns the overridden type, or `array_ret` unchanged when the
    /// callee is not a stream-aware std/iter combinator or the receiver is not DEFINITELY a stream.
    ///
    /// Crucially the receiver must be DEFINITELY a stream (`is_definitely_stream`), NOT merely
    /// streamish (a union that *includes* a Stream). A mixed `Array | Iterator | Stream` union —
    /// e.g. a user GENERIC `<T>(xs: T[] | Iterator<T> | Stream<T>)` param, or the std/iter wrapper's
    /// own param while its body is being checked — has a live array branch, so its eager array
    /// return must be preserved (the per-call-site stream re-typing then happens when the generic is
    /// actually called WITH a concrete stream). Using the looser `type_is_streamish` here would leak
    /// the stream return into the array call sites of any such generic.
    fn streamish_combinator_ret(&self, local_name: &str, array_ret: Type, arg0_ty: &Type) -> Type {
        if !is_definitely_stream(arg0_ty) {
            return array_ret;
        }
        let Some((module_path, export_name)) = self.import_origins.get(local_name) else {
            return array_ret;
        };
        if module_path != "std/iter" {
            return array_ret;
        }
        match export_name.as_str() {
            // Lazy adapters: receiver-dependent element type wrapped in a fresh Stream. The
            // array_ret is `U[]` (map) / `T[]` (filter) / a bare `Json` (the pure-Lin wrappers
            // declare `Json`); unwrap an array element to re-wrap as Stream<…>, else Stream<Json>.
            // These all back onto a lazy `lin_stream_*` adapter (the IR redirects on a stream
            // receiver), so the result is the next Stream node in the pipeline — keeping the chain
            // typed as a Stream so a following `.take`/`.reduce`/… also sees a definite-stream
            // receiver and dispatches lazily.
            "map" | "filter" | "take" | "drop" | "flatMap" | "takeWhile" | "dropWhile"
            | "flatten" | "concat" => {
                let elem = match &array_ret {
                    Type::Array(inner) => (**inner).clone(),
                    Type::Stream(inner) => (**inner).clone(),
                    _ => Type::TypeVar(u32::MAX),
                };
                Type::Stream(Box::new(elem))
            }
            // Terminal: the array_ret is the accumulator/result type U; a stream read can fault, so
            // surface `U | Error`.
            "reduce" => Type::flatten_union(vec![array_ret, crate::resolve::error_type()]),
            // Terminal: first matching item or Null if none; a read fault → Error. `T | Null | Error`
            // (the array `find` already returns `T | Null` ≈ Json, so just add the Error arm).
            "find" => Type::flatten_union(vec![array_ret, Type::Null, crate::resolve::error_type()]),
            // Terminals returning a Boolean over the stream: `Boolean | Error`.
            "some" | "every" => Type::flatten_union(vec![Type::Bool, crate::resolve::error_type()]),
            // Terminal-ish: array_ret is Null; stream form is `Null | Error`.
            "while" | "for" => Type::flatten_union(vec![Type::Null, crate::resolve::error_type()]),
            _ => array_ret,
        }
    }

    /// Does a call to the callee bound to `local_name` ROUTE to a stream operation that takes
    /// ownership of its stream argument(s)? Keyed on the callee's IMPORT ORIGIN
    /// `(module_path, export_name)` — the SAME dispatch fact that `streamish_combinator_ret`
    /// (re-typing) and `stream_combinator_intrinsic_name` (IR redirect) use — so the affine
    /// consume-check, the result re-typing, and the IR move can never diverge. A user-defined
    /// function with one of these names is never affected (it has no std/iter|std/stream origin).
    ///
    /// True for: the genuine std/iter combinators that dispatch to a `lin_stream_*` backend on a
    /// stream receiver, and the genuine std/stream stream-specific ops. The actual consumption is
    /// then applied PER ARGUMENT to every definitely-stream argument (mirroring the IR's
    /// `move_streamish_arg`), which handles `concat`'s two stream args automatically.
    fn callee_routes_to_stream_op(&self, local_name: &str) -> bool {
        let Some((module_path, export_name)) = self.import_origins.get(local_name) else {
            return false;
        };
        match module_path.as_str() {
            "std/iter" => is_std_iter_stream_combinator(export_name),
            "std/stream" => is_std_stream_consuming_export(export_name),
            "std/archive" => is_std_archive_consuming_export(export_name),
            _ => false,
        }
    }

    /// Mark CONSUMED every argument in `arg_exprs` whose inferred type (from the parallel
    /// `arg_tys`) is DEFINITELY a stream — the affine mirror of the IR's `move_streamish_arg`,
    /// which unregisters any streamish arg from the caller's owning scope. Applies only when the
    /// callee routes to a stream op (gated by the caller). Per-argument so `concat(a, b)` consumes
    /// BOTH stream arguments, not just arg0.
    fn consume_definite_stream_args<'a>(
        &mut self,
        arg_exprs: impl Iterator<Item = (&'a Expr, &'a Type)>,
    ) {
        for (expr, ty) in arg_exprs {
            if is_definitely_stream(ty) {
                self.mark_stream_consumed(expr);
            }
        }
    }

    pub(crate) fn infer_call(
        &mut self,
        func: &Expr,
        args: &[Expr],
        partial: bool,
        span: Span,
    ) -> Result<TypedExpr, Diagnostic> {
        // fromJson special form: `fromJson(T, value)` (ADR-031). Intercept before the callee
        // is inferred, since arg0 is a type name, not a value.
        if let Expr::Ident(name, _) = func {
            if name == "fromJson" && !partial && args.len() == 2 {
                if let Some(result) = self.try_from_json_special_form(&args[0], &args[1], span) {
                    return result;
                }
            }
        }
        // Function expression and arguments are not in tail position.
        let prev_tail = self.in_tail_position;
        self.in_tail_position = false;
        let typed_func = self.infer_expr(func)?;
        let func_ty = typed_func.ty();

        let (typed_args, result_type) = match &func_ty {
            Type::Function { params, ret, required } => {
                // Opaque `Function` annotation (resolve.rs: `func([TypeVar(MAX)], TypeVar(MAX))`):
                // a bare `Function` type means "any function" — accept any arity and return a
                // fresh inference var. This must NOT match a *concrete* signature that merely
                // returns `Json` (`TypeVar(MAX)`), e.g. `(): Json` or `(path: String): Json`:
                // those have a KNOWN return type (Json) that must flow through the Json→concrete
                // cast-hole gate (ADR-045), not be freshened into a permissive inference var.
                // The opaque annotation is uniquely identified by having ≥1 param that is the
                // Json wildcard `TypeVar(MAX)` (a real signature never spells a param as Json's
                // sentinel — Json params are written `Json`, which is also TypeVar(MAX), but
                // such functions take Json IN and the freshen-return behaviour was only ever
                // intended for the bare `Function` annotation). We therefore require a non-empty
                // param list whose params are ALL TypeVar(MAX) AND a TypeVar(MAX) return.
                let all_params_json = !params.is_empty()
                    && params.iter().all(|p| matches!(p, Type::TypeVar(n) if *n == u32::MAX));
                let ret_is_typevar_max = matches!(ret.as_ref(), Type::TypeVar(n) if *n == u32::MAX);
                let is_opaque = all_params_json && ret_is_typevar_max;
                if is_opaque {
                    let mut typed_args = Vec::new();
                    for arg in args {
                        typed_args.push(self.infer_expr(arg)?);
                    }
                    self.in_tail_position = prev_tail;
                    let result_type = self.env.fresh_type_var();
                    return Ok(TypedExpr::Call {
                        func: Box::new(typed_func),
                        args: typed_args,
                        result_type,
                        is_tail: self.is_tail_call(func),
                        partial,
                        span,
                    });
                }

                if args.len() > params.len() {
                    let extra = args.len() - params.len();
                    return Err(Diagnostic::error(
                        span,
                        format!(
                            "Too many arguments: expected {}, got {}",
                            params.len(),
                            args.len()
                        ),
                    ).with_help(format!(
                        "remove the {} extra argument{}{}",
                        extra,
                        if extra == 1 { "" } else { "s" },
                        if !params.is_empty() { format!(" — this function takes {}", params.len()) } else { " — this function takes no arguments".to_string() }
                    )));
                }

                // First pass: infer non-function arguments to collect TypeVar substitutions.
                let mut subs = std::collections::HashMap::new();
                let mut partially_typed: Vec<Option<TypedExpr>> = vec![None; args.len()];
                for (i, (arg, param_ty)) in args.iter().zip(params.iter()).enumerate() {
                    if !matches!(arg, Expr::Function { .. }) {
                        // An array-literal argument must adopt the parameter's element
                        // representation rather than its own bottom-up inference. Pure
                        // inference of an EMPTY literal `[]` yields `Array(Never)` (no
                        // elements to infer a width from), so codegen allocates a boxed
                        // buffer at the call site while a flat-scalar `T[]` param does
                        // stride-N push/get in the callee → corruption. Route array
                        // literals through expected-type-directed checking when the
                        // parameter is a concrete (TypeVar-free) array type, so the
                        // literal carries the param's element type at codegen.
                        //
                        // Gated to TypeVar-free params so the generic-substitution and
                        // `Number`-param paths (whose params resolve to constrained
                        // TypeVars, and the Json wildcard `TypeVar(MAX)`) still go through
                        // plain inference — there is no concrete expected type to check
                        // against, and `collect_and_save_subs` below still needs the
                        // bottom-up arg type to drive substitution.
                        let array_lit_against_concrete_array = matches!(arg, Expr::Array(..))
                            && matches!(param_ty, Type::Array(_) | Type::FixedArray(_))
                            && !param_ty.contains_type_var();
                        // Same hazard for an object literal against a typed index-signature
                        // map `{ String: T }` (`Type::Map`): an empty `{}` infers bottom-up to
                        // an empty `Object` (no fields to fix a value type), which then fails to
                        // match the concrete `Map(T)` param. Route object literals through
                        // expected-type-directed checking so `{}` (and string-keyed literals)
                        // adopt the param's `Map(T)` representation. Restricted to `Type::Map`
                        // so structural-`Object` params stay on the inference path (the
                        // `check_object_against` Object branch defers for them anyway), and
                        // gated TypeVar-free for the same substitution reasons as arrays.
                        let object_lit_against_concrete_map = matches!(arg, Expr::Object(..))
                            && matches!(param_ty, Type::Map(_))
                            && !param_ty.contains_type_var();
                        let typed = if array_lit_against_concrete_array || object_lit_against_concrete_map {
                            self.check_expr(arg, param_ty)?
                        } else {
                            self.infer_expr(arg)?
                        };
                        // DEFER substitution collection for a bare integer-literal argument whose
                        // parameter is a bare TypeVar (`item: T`). A suffixless literal infers as the
                        // Int32 default (spec §21), which would CLOBBER a binding the same TypeVar
                        // already received from a non-literal argument — e.g. `append(b, 221)` where
                        // `arr: T[]` binds `T = UInt8` from `b`, but `221: Int32` then overwrites it
                        // to `Int32`, splitting the result type (`Int32[]`) from the flat `UInt8`
                        // runtime representation. Literals defer to the literal-retyping pass below,
                        // which adopts the resolved param type and re-types the literal at that width.
                        let defer_literal_sub = matches!(arg, Expr::IntLit(_, None, _))
                            && matches!(param_ty, Type::TypeVar(id) if *id != u32::MAX);
                        // Also DEFER an EMPTY array literal `[]` flowing into a generic `T[]` param
                        // (TypeVar element): inferring it bottom-up yields `Array(Never)` and would
                        // bind `T = Never`, then a concrete sibling item (`[].append(1)`) fails as
                        // "expected Never". The deferred pass binds `T` from the sibling, then the
                        // empty literal is RE-CHECKED against the resolved `T[]` so its element type
                        // is concrete at codegen.
                        let defer_empty_array_sub = matches!(arg, Expr::Array(elems, _, _) if elems.is_empty())
                            && matches!(param_ty, Type::Array(e) if matches!(**e, Type::TypeVar(id) if id != u32::MAX));
                        if !defer_literal_sub && !defer_empty_array_sub {
                            // First arg establishes the canonical TypeVar binding; later args must not
                            // clobber it with an assignable (narrower) candidate (`push(out, item)`
                            // where `item` widens into `out`'s element type — see no_clobber doc).
                            if i == 0 {
                                self.collect_and_save_subs(param_ty, &typed.ty(), &mut subs);
                            } else {
                                self.collect_and_save_subs_no_clobber(param_ty, &typed.ty(), &mut subs);
                            }
                        }
                        partially_typed[i] = Some(typed);
                    }
                }
                // Second sub-pass: collect substitutions from the deferred integer-literal and empty
                // array-literal args, but ONLY for TypeVars that a non-literal arg did NOT already
                // bind (so a literal's Int32/Never default never clobbers a concrete binding — it
                // only supplies a binding when the TypeVar is otherwise unconstrained, e.g.
                // `push(emptyAcc, 1)` with no width evidence elsewhere).
                for (i, (arg, param_ty)) in args.iter().zip(params.iter()).enumerate() {
                    if matches!(arg, Expr::IntLit(_, None, _)) {
                        if let Type::TypeVar(id) = param_ty {
                            if *id != u32::MAX && !subs.contains_key(id) {
                                if let Some(typed) = &partially_typed[i] {
                                    self.collect_and_save_subs(param_ty, &typed.ty(), &mut subs);
                                }
                            }
                        }
                    }
                }
                // Re-check each deferred EMPTY array literal against the now-resolved `T[]` param so
                // its element type is concrete (the codegen-representation fix). If `T` is still
                // unbound (no concrete sibling), leave it as the bottom-up `Array(Never)` and let the
                // later compatibility/empty-literal-annotation gate report it.
                for (i, (arg, param_ty)) in args.iter().zip(params.iter()).enumerate() {
                    let is_empty_array = matches!(arg, Expr::Array(elems, _, _) if elems.is_empty());
                    if is_empty_array {
                        let resolved = apply_type_subs(param_ty, &subs);
                        if let Type::Array(_) = &resolved {
                            if !resolved.contains_type_var() {
                                if let Ok(rechecked) = self.check_expr(arg, &resolved) {
                                    self.collect_and_save_subs(param_ty, &rechecked.ty(), &mut subs);
                                    partially_typed[i] = Some(rechecked);
                                }
                            }
                        }
                    }
                }
                // A genuinely-`Json` (dynamic) argument flowing into a bare-TypeVar param that a
                // CONTAINER arg already pinned to a concrete type (`push(uint8Arr, jsonVal)`,
                // `push(out: Field[], field["bytes"]…)`) must REBIND that TypeVar to `Json` so the
                // call monomorphizes DYNAMICALLY: a `$Json` push routes through `lin_push_dyn`, which
                // converts the boxed element into the array's runtime element slot at RUNTIME (the
                // representation the non-generic `push` used). Keeping the concrete binding instead
                // forces an unbox-coercion the monomorphized scalar-param body can't express → a
                // `box_value` on a Json pointer (`zext ptr`) at codegen. Only a NON-first (item) arg
                // triggers this; the container arg keeps its concrete element type.
                for (i, param_ty) in params.iter().enumerate() {
                    if i == 0 { continue; }
                    if let Type::TypeVar(id) = param_ty {
                        if *id != u32::MAX {
                            if let Some(t) = partially_typed.get(i).and_then(|o| o.as_ref()) {
                                let is_json_item = matches!(t.ty(), Type::TypeVar(_));
                                let bound_concrete = subs.get(id).map(|b| !b.contains_type_var()).unwrap_or(false);
                                if is_json_item && bound_concrete {
                                    subs.insert(*id, Type::TypeVar(u32::MAX));
                                }
                            }
                        }
                    }
                }

                // Second pass: infer function arguments with concrete expected types.
                // Re-apply substitutions before each arg so earlier lambda results inform later ones.
                let mut typed_args = Vec::new();
                for (i, arg) in args.iter().enumerate() {
                    let typed = match partially_typed[i].take() {
                        Some(t) => t,
                        None => {
                            // Lambda/function arg: check against the concrete expected type.
                            // Re-apply subs each iteration so earlier lambdas inform later ones.
                            let expected = apply_type_subs(&params[i], &subs);
                            if matches!(expected, Type::Function { .. }) {
                                // When every PARAMETER of the callback's signature has been PINNED
                                // by an earlier (type-pinning) argument, the param hints are concrete
                                // (e.g. `(Int32,Int32)=>R` for `sort(xs, cmp)` once `T=Int32` from
                                // `xs`). The back-inferred param hints are then trustworthy, so a
                                // body type error (`a["x"]` on an `Int32` param) is GENUINE and must
                                // propagate — not be swallowed by a hint-free `infer_expr` retry (the
                                // inference hole this closes). The return is ignored on purpose (an
                                // unresolved `U` is solved FROM the body). When a param is still an
                                // unresolved generic, fall back to plain inference so it stays free
                                // for the call site to solve.
                                if expected_fn_params_fully_pinned(&expected) {
                                    self.check_expr(arg, &expected)?
                                } else {
                                    // SPECULATIVE check: if checking the callback against the
                                    // (possibly-incomplete) hint fails, we DISCARD it and re-infer
                                    // hint-free. A failed `infer_function`/`_with_hints` can `?`-out
                                    // BETWEEN its push of `function_scope_depths`/`capture_stack`
                                    // (+ env scope) and the matching pops, leaking an unbalanced
                                    // frame that subsequent functions then pop — mis-attributing the
                                    // discarded attempt's captures to an UNRELATED enclosing function
                                    // (it would gain a phantom closure env, breaking its call ABI).
                                    // Snapshot the transient stack/scope lengths and truncate them
                                    // back on the discarded path so the retry starts clean.
                                    let snap = self.checker_state_snapshot();
                                    match self.check_expr(arg, &expected) {
                                        Ok(t) => t,
                                        Err(_) => {
                                            self.restore_checker_state(snap);
                                            self.infer_expr(arg)?
                                        }
                                    }
                                }
                            } else {
                                self.infer_expr(arg)?
                            }
                        }
                    };
                    // Collect substitutions from function args too (e.g. lambda return types).
                    // A bare integer literal flowing into a TypeVar param must NOT clobber a binding
                    // already established for that TypeVar (its Int32 default would override a
                    // concrete width — the `append(uint8Arr, 221)` literal-width split). Mirror the
                    // first-pass deferral: skip re-collecting a literal's Int32 once the slot is bound.
                    let literal_into_bound_tv = matches!(arg, Expr::IntLit(_, None, _))
                        && matches!(&params[i], Type::TypeVar(id) if *id != u32::MAX && subs.contains_key(id));
                    if !literal_into_bound_tv {
                        if i == 0 {
                            self.collect_and_save_subs(&params[i], &typed.ty(), &mut subs);
                        } else {
                            self.collect_and_save_subs_no_clobber(&params[i], &typed.ty(), &mut subs);
                        }
                    }
                    typed_args.push(typed);
                }
                self.in_tail_position = prev_tail;

                // Omitted optional argument carrying a type-parameter default (`default: D = null`):
                // an omitted optional arg is filled from its default value, so its type parameter
                // must be bound by that default's type — exactly as if the caller had written it.
                // The supplied-arg loop above never visited the omitted params, so a bare
                // type-parameter `D` would otherwise stay an UNBOUND `TypeVar` in the return type
                // `T | D`, which `apply_type_subs` leaves free and `arg_compatible` then treats
                // permissively — unsoundly letting `arr.at(i)` satisfy a bare `T` context.
                //
                // The defaults declared on generic params in the stdlib are all `null` (`D = null`);
                // a generic param has no other spellable defaulting value (a concrete literal would
                // pin `D` away from the element type and defeat the unified `T | D` shape). We
                // therefore bind any omitted optional param whose declared type is an unbound,
                // non-sentinel type-parameter `TypeVar` to `Null`, modelling the omitted `= null`.
                // This is the missing inference that makes `arr.at(i)` soundly `T | Null` while
                // `arr.at(i, 0)` stays `T | Int32`. Concrete-typed optional params (e.g.
                // `pad: String = " "`) are unaffected — their type is not a bare `TypeVar`.
                if !partial && typed_args.len() < params.len() {
                    for param_ty in &params[typed_args.len()..] {
                        if let Type::TypeVar(id) = param_ty {
                            let is_sentinel = *id == u32::MAX || self.numeric_tvs.contains(id);
                            if !is_sentinel && !subs.contains_key(id) {
                                subs.insert(*id, Type::Null);
                            }
                        }
                    }
                }

                // Re-apply substitutions (may have new entries from lambda args).
                let concrete_params: Vec<Type> = params.iter()
                    .map(|p| apply_type_subs(p, &subs))
                    .collect();

                // Spec §21: a suffixless integer literal takes its context type. When an
                // argument is a bare integer literal and the parameter has a concrete integer
                // type T, re-type the literal at width T (if it fits) so it satisfies the
                // parameter. This lets e.g. `toUInt8(255)` or `f32FromBits(0x40600000)` pass
                // even when the parameter is a wider/unsigned integer than the Int32 default.
                for (i, param_ty) in concrete_params.iter().enumerate() {
                    if i >= typed_args.len() { break; }
                    if let TypedExpr::IntLit(v, _, lit_span) = &typed_args[i] {
                        if let Some((lo, hi)) = integer_range(param_ty) {
                            let (v, lit_span) = (*v, *lit_span);
                            // For an unsigned target, a literal above i64::MAX is stored as a
                            // negative bit pattern — also accept its unsigned reinterpretation.
                            let signed = v as i128;
                            let fits = (signed >= lo && signed <= hi)
                                || (!param_ty.is_signed() && {
                                    let unsigned = (v as u64) as i128;
                                    unsigned >= lo && unsigned <= hi
                                });
                            if fits {
                                typed_args[i] = TypedExpr::IntLit(v, param_ty.clone(), lit_span);
                            }
                        }
                    }
                    // Spec §21, float counterpart: a suffixless float literal infers to `Float64`
                    // by default but takes its context type. When an argument is a bare float
                    // literal and the parameter is `Float32`, re-type the literal at `Float32` so
                    // it satisfies the parameter (mirrors the integer-literal re-typing above).
                    if let TypedExpr::FloatLit(v, ty, lit_span) = &typed_args[i] {
                        if matches!(param_ty, Type::Float32) && matches!(ty, Type::Float64) {
                            typed_args[i] = TypedExpr::FloatLit(*v, Type::Float32, *lit_span);
                        }
                    }
                }

                // Enforce the NUMERIC bound (ADR-014, reversed). A `Number` parameter resolved to a
                // numerically-constrained generic TypeVar; at this call site it is being bound to the
                // argument's concrete family. That family must be numeric (an `Int*`/`UInt*`/`Float*`),
                // OR another numeric-constrained / unconstrained generic TypeVar (the bound flows on to
                // the outer specialization). A `String`/`Bool`/`Object`/array/`Json` argument is a
                // compile error — caught HERE rather than via `arg_compatible`, which would otherwise
                // accept any type into a bare TypeVar param.
                for (i, param_ty) in params.iter().enumerate() {
                    if i >= typed_args.len() { break; }
                    if let Type::TypeVar(id) = param_ty {
                        if self.numeric_tvs.contains(id) {
                            let arg_ty = typed_args[i].ty();
                            if !arg_satisfies_numeric_bound(&arg_ty) {
                                return Err(Diagnostic::error(
                                    args[i].span(),
                                    format!(
                                        "Argument {} has type {}, expected a numeric type (Number)",
                                        i + 1,
                                        arg_ty
                                    ),
                                ).with_help(
                                    "a `Number` parameter accepts any numeric family (Int8…Float64), \
                                     or a dynamic `Json` value (decoded as Int32, unchecked — use \
                                     `Int32.fromJson(v)` for a validated decode)".to_string()
                                ));
                            }
                        }
                    }
                }

                // Check argument compatibility against concrete params.
                for (i, (arg, param_ty)) in
                    typed_args.iter().zip(concrete_params.iter()).enumerate()
                {
                    let arg_ty = arg.ty();
                    if !self.arg_compatible(&arg_ty, param_ty) {
                        return Err(Diagnostic::error(
                            args[i].span(),
                            format!(
                                "Argument {} has type {}, expected {}",
                                i + 1,
                                arg_ty,
                                param_ty
                            ),
                        ));
                    }
                }

                // Mixed numeric families in ONE call of a `Number`-returning function (e.g.
                // `(a:Number,b:Number)=>a+b` at `add(10, 2.5)`) ARE supported (ADR-014, reversed):
                // monomorphization specializes `add$Int32_Float64` and re-widens the arithmetic
                // result to the same family the concrete `(a:Int32,b:Float64)` equivalent produces
                // (Float64 here). No guard — the previous reject was a workaround for a codegen ABI
                // mismatch (frozen-result-type) now fixed in `lin-ir::monomorphize`.

                // Expand any `Named` type-alias in the resolved return type against the (now
                // fully-resolved) env. A forward-declared function signature can carry an
                // UNRESOLVED `Named("R")` return when `R`'s alias body was still the placeholder at
                // forward-declaration time (mutual recursion: `f`/`g` are forward-declared before
                // either alias body is resolved). Leaving the call result as `Named("R")` while the
                // sibling actually returns a structural SEALED `{…}` makes the `if`-merge box the
                // packed sealed value as an opaque union and later read it back with `lin_unbox_ptr`
                // → packed-pointer-as-box → segfault. Expanding yields the SAME structural shape the
                // sibling's body produces, so every repr decision agrees.
                //
                // SCOPE: skip a DIRECT SELF-recursive call in TAIL position. Such a call is
                // TAIL-call-optimized (CLAUDE.md / ADR-016: a direct self tail call becomes a
                // back-edge), so its return value is never read cross-frame — the value flows
                // straight into the next iteration's param slot, never through a record-shaped
                // coercion. Keeping the `Named` result there preserves the Stage-4 escape analysis's
                // stack-allocation of a TCO-loop accumulator record (`test_sealed_stack_tco_loop_*`):
                // the representation boundary at the boxed-union `if`-merge is what lets the
                // per-iteration fresh record stay stack-resident. EVERY other call — a self-call in
                // NON-tail position (its result is read in-frame, e.g. `val p = f(n-1); p["v"]`) or
                // any cross-function call (mutual recursion) — must expand, since the returned record
                // IS read and must agree on the packed representation.
                let is_self_tail_call = prev_tail
                    && matches!(func, Expr::Ident(n, _) if Some(n.as_str()) == self.current_function.as_deref());
                let concrete_ret = if is_self_tail_call {
                    apply_type_subs(ret, &subs)
                } else {
                    self.expand_named_aliases(&apply_type_subs(ret, &subs))
                };
                let required = *required;

                let result_type = if partial {
                    // Explicit partial application (`f(x,)`): return a function awaiting
                    // the remaining parameters, preserving how many of those are still
                    // required. A trailing comma on a fully-supplied arg list is just a
                    // full call.
                    if typed_args.len() < params.len() {
                        let remaining_params = concrete_params[typed_args.len()..].to_vec();
                        let remaining_required = required.saturating_sub(typed_args.len());
                        Type::Function {
                            params: remaining_params,
                            ret: Box::new(concrete_ret),
                            required: remaining_required,
                        }
                    } else {
                        concrete_ret
                    }
                } else {
                    // Default-fill semantics: omitting trailing optional arguments fills
                    // them from their defaults and calls now. Supplying fewer than the
                    // required count is an error (use a trailing comma to curry instead).
                    if typed_args.len() < required {
                        let optional = params.len() - required;
                        let help = if optional == 0 {
                            format!(
                                "this function takes {} argument{} — to partially apply, add a trailing comma: f(x,)",
                                params.len(),
                                if params.len() == 1 { "" } else { "s" },
                            )
                        } else {
                            format!(
                                "this function takes {} required and {} optional argument{} — to partially apply, add a trailing comma: f(x,)",
                                required,
                                optional,
                                if optional == 1 { "" } else { "s" },
                            )
                        };
                        return Err(Diagnostic::error(
                            span,
                            format!(
                                "Too few arguments: expected at least {}, got {}",
                                required,
                                typed_args.len()
                            ),
                        ).with_help(help));
                    }
                    concrete_ret
                };
                (typed_args, result_type)
            }
            _ => {
                // Unknown or non-function type — infer all args without type guidance.
                let mut typed_args = Vec::new();
                for arg in args {
                    typed_args.push(self.infer_expr(arg)?);
                }
                self.in_tail_position = prev_tail;
                if matches!(func_ty, Type::TypeVar(_)) {
                    let result_type = self.env.fresh_type_var();
                    (typed_args, result_type)
                } else {
                    return Err(Diagnostic::error(
                        span,
                        format!("Cannot call non-function type {}", func_ty),
                    ));
                }
            }
        };

        // var-capture check and transferability check for `async(f)` / `async(fs)`.
        if let Expr::Ident(name, _) = func {
            if name == "lin_async" {
                let globals = self.mutable_global_slots.clone();
                for arg in &typed_args {
                    if let Some(var_name) = first_mutable_capture(arg, &globals) {
                        self.diagnostics.push(Diagnostic::error(
                            span,
                            format!(
                                "async thunk captures mutable variable '{}' — sharing mutable state across threads is not allowed",
                                var_name
                            ),
                        ).with_help("capture an immutable copy: `val snap = {}; async(() => snap)`".to_string()));
                    }
                    // Transferability: thunk return type must not be Function/Iterator/etc.
                    let ret_ty = match arg.ty() {
                        Type::Function { ret, .. } => Some(*ret),
                        _ => None,
                    };
                    if let Some(ret) = ret_ty {
                        if is_definitely_non_transferable(&ret) {
                            self.diagnostics.push(Diagnostic::error(
                                span,
                                format!(
                                    "async thunk returns non-transferable type '{}' — async results must be JSON-compatible values",
                                    ret
                                ),
                            ).with_help("return a JSON-serializable value (String, Boolean, Null, numeric, array, or object)".to_string()));
                        }
                    }
                }
            }
        }

        let is_tail = self.is_tail_call(func);

        // Receiver-dependent std/iter combinator return (unification Stage 2): re-type the eager
        // array result to its stream-shaped form when the callee is a std/iter combinator and the
        // iterable arg0 is a Stream. Skipped for partial application (no concrete arg0 yet).
        let result_type = if !partial {
            if let (Expr::Ident(callee, _), Some(arg0)) = (func, typed_args.first()) {
                self.streamish_combinator_ret(callee, result_type, &arg0.ty())
            } else {
                result_type
            }
        } else {
            result_type
        };

        // Affine consume (streams brief §7): a call that ROUTES to a stream op MOVES its
        // stream argument(s) — mark each definitely-stream argument consumed so a later use of the
        // same binding errors. Keyed on the callee's import origin (the dispatch fact), not a name
        // list, and applied PER ARGUMENT so `concat(a, b)` consumes BOTH streams. This mirrors the
        // IR's `move_streamish_arg` exactly; the two cannot diverge.
        if !partial {
            if let Expr::Ident(callee, _) = func {
                if self.callee_routes_to_stream_op(callee) {
                    let pairs: Vec<(&Expr, Type)> = args
                        .iter()
                        .zip(typed_args.iter())
                        .map(|(e, t)| (e, t.ty()))
                        .collect();
                    self.consume_definite_stream_args(pairs.iter().map(|(e, t)| (*e, t)));
                }
            }
        }

        Ok(TypedExpr::Call {
            func: Box::new(typed_func),
            args: typed_args,
            result_type,
            is_tail,
            partial,
            span,
        })
    }

    pub(crate) fn infer_dot_call(
        &mut self,
        receiver: &Expr,
        method: &str,
        args: &Option<Vec<Expr>>,
        partial: bool,
        span: Span,
    ) -> Result<TypedExpr, Diagnostic> {
        // A dot access with no argument list (`x.f`) is partial application of the
        // receiver (spec §16.1), never default-fill. An explicit trailing comma
        // (`x.f(y,)`) is also partial.
        let partial = partial || args.is_none();
        // Desugar: receiver.method(args) -> method(receiver, args)
        // Special case: TupleArgs receiver spreads all elements as individual args.
        // e.g. (10, 3).sub -> sub(10, 3), not sub((10, 3))
        if let Expr::TupleArgs(tuple_exprs, _) = receiver {
            if tuple_exprs.len() > 1 {
                let extra_args: Vec<&Expr> = args.as_ref().map(|a| a.as_slice()).unwrap_or(&[]).iter().collect();
                let all_arg_exprs: Vec<&Expr> = tuple_exprs.iter().chain(extra_args).collect();
                // Build a synthetic call: method(tuple_exprs[0], tuple_exprs[1], ..., extra_args)
                let dummy_call = Expr::Call {
                    func: Box::new(Expr::Ident(method.to_string(), span)),
                    args: all_arg_exprs.into_iter().cloned().collect(),
                    partial,
                    span,
                    full_span: span,
                };
                return self.infer_expr(&dummy_call);
            }
        }

        // fromJson special form: `T.fromJson(value)` (ADR-031). Intercept before the receiver
        // is inferred as a value, since `T` is a type name, not a runtime value.
        if method == "fromJson" && !partial {
            if let Some(arg_exprs) = args {
                if arg_exprs.len() == 1 {
                    if let Some(result) =
                        self.try_from_json_special_form(receiver, &arg_exprs[0], span)
                    {
                        return result;
                    }
                }
            }
        }

        let mut typed_receiver = self.infer_expr(receiver)?;

        // An array/object literal RECEIVER (`[].fill()`, `{}.merge(…)`) flowing into a CONCRETE
        // (TypeVar-free) array/map FIRST param must adopt that param's resolved element representation
        // — the receiver mirror of the prefix-`infer_call` / dot-call-argument rule. Pure inference of
        // an empty `[]` receiver yields `Array(Never)`, so it lowers a BOXED buffer while the callee's
        // packed/flat-scalar `T[]` param does packed stride-N push/get → a representation DRIFT
        // (latent scalar packed-array UAF: `[].fill()` over `Pt[]`). The generic-`T[]`-param case is
        // handled separately below by `defer_empty_receiver` (it must bind `T` from the item first);
        // this covers the CONCRETE-param case, which has a definite expected type to check against.
        if matches!(receiver, Expr::Array(..) | Expr::Object(..)) {
            if let Some(Type::Function { params, .. }) = self.env.effective_type(method) {
                if let Some(p0) = params.first() {
                    let p0_concrete_container = matches!(p0, Type::Array(_) | Type::FixedArray(_) | Type::Map(_))
                        && !p0.contains_type_var();
                    if p0_concrete_container {
                        if let Ok(rechecked) = self.check_expr(receiver, p0) {
                            typed_receiver = rechecked;
                        }
                    }
                }
            }
        }

        // Affine consume for a dot-call routing to a stream op is applied per-argument AFTER all
        // arguments are inferred (so `concat`'s second stream arg is also covered) — see the
        // `consume_definite_stream_args` calls below, gated on `callee_routes_to_stream_op(method)`.

        // Look up method type for TypeVar substitution.
        if let Some(method_ty) = self.env.effective_type(method) {
            if let Type::Function { params: method_params, ret, required: method_required } = method_ty.clone() {
                // Build all arg expressions: [receiver, ...args]
                let all_arg_exprs: Vec<&Expr> = std::iter::once(receiver)
                    .chain(args.as_ref().map(|a| a.as_slice()).unwrap_or(&[]).iter())
                    .collect();
                // We already have typed_receiver; build partial list.
                // First pass: collect substitutions from non-lambda args (receiver already typed).
                let mut subs = std::collections::HashMap::new();
                // Defer an EMPTY array-literal RECEIVER (`[].append(1)`) flowing into a generic `T[]`
                // param: collecting its bottom-up `Array(Never)` would bind `T = Never`, then the
                // concrete item fails as "expected Never". Bind `T` from the item first, then re-check
                // the receiver against the resolved `T[]` (mirror of the `infer_call` empty-array fix).
                let receiver_is_empty_array = matches!(receiver, Expr::Array(elems, _, _) if elems.is_empty());
                let defer_empty_receiver = receiver_is_empty_array
                    && matches!(method_params.first(), Some(Type::Array(e)) if matches!(**e, Type::TypeVar(id) if id != u32::MAX));
                if !defer_empty_receiver {
                    if let Some(p0) = method_params.first() {
                        self.collect_and_save_subs(p0, &typed_receiver.ty(), &mut subs);
                    }
                }
                let mut partially_typed: Vec<Option<TypedExpr>> = vec![None; all_arg_exprs.len()];
                partially_typed[0] = Some(typed_receiver);
                if let Some(arg_exprs) = args.as_ref() {
                    for (i, (arg, param_ty)) in arg_exprs.iter().zip(method_params.iter().skip(1)).enumerate() {
                        if !matches!(arg, Expr::Function { .. }) {
                            // An array-literal argument must adopt the parameter's element
                            // representation rather than its own bottom-up inference — the exact
                            // mirror of the `infer_call` rule (which dot-calls bypassed). Pure
                            // inference of an EMPTY literal `[]` yields `Array(Never)`, so the
                            // producer lowers a BOXED buffer while a concrete packed/flat-scalar
                            // `T[]` param's callee does packed stride-N push/get → a representation
                            // DRIFT (the calc-lexer `scan(.., [])` boxed-vs-packed `Token[]` UAF this
                            // change closes). Route array/object literals through expected-type-
                            // directed checking when the param is a concrete (TypeVar-free) array or
                            // typed-map type so the literal carries the param's resolved element repr
                            // at codegen, identical to the prefix-call path.
                            let array_lit_against_concrete_array = matches!(arg, Expr::Array(..))
                                && matches!(param_ty, Type::Array(_) | Type::FixedArray(_))
                                && !param_ty.contains_type_var();
                            let object_lit_against_concrete_map = matches!(arg, Expr::Object(..))
                                && matches!(param_ty, Type::Map(_))
                                && !param_ty.contains_type_var();
                            let typed = if array_lit_against_concrete_array || object_lit_against_concrete_map {
                                self.check_expr(arg, param_ty)?
                            } else {
                                self.infer_expr(arg)?
                            };
                            // Defer a bare integer literal's Int32-default substitution for a TypeVar
                            // param so it can't clobber a binding the receiver already pinned (the
                            // `uint8Arr.append(221)` literal-width split — mirror of `infer_call`).
                            let defer_literal_sub = matches!(arg, Expr::IntLit(_, None, _))
                                && matches!(param_ty, Type::TypeVar(id) if *id != u32::MAX);
                            if !defer_literal_sub {
                                // Non-receiver args must not clobber the receiver's canonical binding
                                // with an assignable candidate (`out.push(item)` element-widen case).
                                self.collect_and_save_subs_no_clobber(param_ty, &typed.ty(), &mut subs);
                            }
                            partially_typed[i + 1] = Some(typed);
                        }
                    }
                    // Supply a binding from a deferred literal only when its TypeVar is still unbound.
                    for (i, (arg, param_ty)) in arg_exprs.iter().zip(method_params.iter().skip(1)).enumerate() {
                        if matches!(arg, Expr::IntLit(_, None, _)) {
                            if let Type::TypeVar(id) = param_ty {
                                if *id != u32::MAX && !subs.contains_key(id) {
                                    if let Some(typed) = &partially_typed[i + 1] {
                                        self.collect_and_save_subs(param_ty, &typed.ty(), &mut subs);
                                    }
                                }
                            }
                        }
                    }
                }
                // Re-check a deferred empty array-literal receiver against the now-resolved `T[]` param
                // so its element type is concrete at codegen.
                if defer_empty_receiver {
                    if let Some(p0) = method_params.first() {
                        let resolved = apply_type_subs(p0, &subs);
                        if let Type::Array(_) = &resolved {
                            if !resolved.contains_type_var() {
                                if let Ok(rechecked) = self.check_expr(receiver, &resolved) {
                                    self.collect_and_save_subs(p0, &rechecked.ty(), &mut subs);
                                    partially_typed[0] = Some(rechecked);
                                }
                            }
                        }
                        // Whether or not re-check succeeded, fold the (possibly Never) receiver type in
                        // so an unconstrained call still gets a binding.
                        if let Some(t) = &partially_typed[0] {
                            self.collect_and_save_subs(p0, &t.ty(), &mut subs);
                        }
                    }
                }
                // Rebind a TypeVar bound to a concrete type by the receiver to `Json` when a NON-
                // receiver item arg is genuinely `Json` (`arr.push(jsonVal)`) — see the `infer_call`
                // sibling for why (dynamic `$Json` monomorph via `lin_push_dyn`).
                for (i, param_ty) in method_params.iter().enumerate() {
                    if i == 0 { continue; }
                    if let Type::TypeVar(id) = param_ty {
                        if *id != u32::MAX {
                            if let Some(t) = partially_typed.get(i).and_then(|o| o.as_ref()) {
                                let is_json_item = matches!(t.ty(), Type::TypeVar(_));
                                let bound_concrete = subs.get(id).map(|b| !b.contains_type_var()).unwrap_or(false);
                                if is_json_item && bound_concrete {
                                    subs.insert(*id, Type::TypeVar(u32::MAX));
                                }
                            }
                        }
                    }
                }

                let mut all_args = Vec::new();
                for (i, arg_expr) in all_arg_exprs.iter().enumerate() {
                    let typed = match partially_typed[i].take() {
                        Some(t) => t,
                        None => {
                            // Re-apply subs each iteration so earlier lambdas inform later ones.
                            let expected = method_params.get(i)
                                .map(|p| apply_type_subs(p, &subs))
                                .unwrap_or_else(|| self.env.fresh_type_var());
                            if matches!(expected, Type::Function { .. }) {
                                // Fully-pinned callback PARAMS: the back-inferred param hints are
                                // trustworthy, so a body type error must propagate (the dot-call
                                // mirror of the `infer_call` fix — `xs.map(x => x["k"])` over an
                                // `Int32[]`, where `map`'s `(T, Int32) => U` pins the param to
                                // `Int32` via the receiver even though the return `U` is free).
                                // When a param is still an unresolved generic, fall back to plain
                                // inference. The Json wildcard does not count (see helper).
                                if expected_fn_params_fully_pinned(&expected) {
                                    self.check_expr(arg_expr, &expected)?
                                } else {
                                    // Speculative; roll back leaked nesting state on the discarded
                                    // path (see `infer_call`'s sibling site + `restore_checker_state`).
                                    let snap = self.checker_state_snapshot();
                                    match self.check_expr(arg_expr, &expected) {
                                        Ok(t) => t,
                                        Err(_) => {
                                            self.restore_checker_state(snap);
                                            self.infer_expr(arg_expr)?
                                        }
                                    }
                                }
                            } else {
                                self.infer_expr(arg_expr)?
                            }
                        }
                    };
                    // Collect substitutions from lambda/function args too (e.g. to resolve return TypeVars).
                    // Don't let a bare integer literal's Int32 default clobber an already-bound TypeVar.
                    if let Some(param_ty) = method_params.get(i) {
                        let literal_into_bound_tv = matches!(arg_expr, Expr::IntLit(_, None, _))
                            && matches!(param_ty, Type::TypeVar(id) if *id != u32::MAX && subs.contains_key(id));
                        if !literal_into_bound_tv {
                            if i == 0 {
                                self.collect_and_save_subs(param_ty, &typed.ty(), &mut subs);
                            } else {
                                self.collect_and_save_subs_no_clobber(param_ty, &typed.ty(), &mut subs);
                            }
                        }
                    }
                    all_args.push(typed);
                }

                // Omitted optional argument carrying a type-parameter default (`default: D = null`):
                // mirror of the prefix `infer_call` rule. `all_args` includes the receiver, so the
                // omitted params are those at index `>= all_args.len()`. Bind any omitted optional
                // param whose declared type is an unbound, non-sentinel type-parameter `TypeVar` to
                // `Null`, modelling the omitted `= null` default. This is what makes `arr.at(i)`
                // soundly `T | Null` (rather than leaving `D` free → unsoundly satisfying bare `T`).
                // See the prefix-path comment for the full rationale.
                if !partial && all_args.len() < method_params.len() {
                    for param_ty in &method_params[all_args.len()..] {
                        if let Type::TypeVar(id) = param_ty {
                            let is_sentinel = *id == u32::MAX || self.numeric_tvs.contains(id);
                            if !is_sentinel && !subs.contains_key(id) {
                                subs.insert(*id, Type::Null);
                            }
                        }
                    }
                }

                let concrete_params: Vec<Type> = method_params.iter()
                    .map(|p| apply_type_subs(p, &subs))
                    .collect();

                // Spec §21: a suffixless integer literal takes its context type. Mirror of the
                // prefix `infer_call` pass — re-type a bare integer-literal argument at the
                // parameter's concrete integer width (if it fits) so e.g. `305419896.toUInt64()`
                // (a literal receiver into a `UInt64` param) passes, exactly as `toUInt64(305419896)`
                // does. Without this the new compatibility loop below would reject the dot form on
                // a signed-Int32-literal → unsigned/wider-integer param while the prefix form passes.
                for (i, param_ty) in concrete_params.iter().enumerate() {
                    if i >= all_args.len() { break; }
                    if let TypedExpr::IntLit(v, _, lit_span) = &all_args[i] {
                        if let Some((lo, hi)) = integer_range(param_ty) {
                            let (v, lit_span) = (*v, *lit_span);
                            let signed = v as i128;
                            let fits = (signed >= lo && signed <= hi)
                                || (!param_ty.is_signed() && {
                                    let unsigned = (v as u64) as i128;
                                    unsigned >= lo && unsigned <= hi
                                });
                            if fits {
                                all_args[i] = TypedExpr::IntLit(v, param_ty.clone(), lit_span);
                            }
                        }
                    }
                    // Float counterpart (see direct-call site): a bare float literal argument into
                    // a `Float32` parameter is re-typed at `Float32`.
                    if let TypedExpr::FloatLit(v, ty, lit_span) = &all_args[i] {
                        if matches!(param_ty, Type::Float32) && matches!(ty, Type::Float64) {
                            all_args[i] = TypedExpr::FloatLit(*v, Type::Float32, *lit_span);
                        }
                    }
                }

                // Enforce the NUMERIC bound on a dot-call to a `Number`-parameter function (ADR-014,
                // reversed). Mirrors the direct-call check; the receiver is arg 0.
                for (i, param_ty) in method_params.iter().enumerate() {
                    if i >= all_args.len() { break; }
                    if let Type::TypeVar(id) = param_ty {
                        if self.numeric_tvs.contains(id) {
                            let arg_ty = all_args[i].ty();
                            if !arg_satisfies_numeric_bound(&arg_ty) {
                                return Err(Diagnostic::error(
                                    all_arg_exprs.get(i).map(|e| e.span()).unwrap_or(span),
                                    format!(
                                        "Argument {} has type {}, expected a numeric type (Number)",
                                        i + 1,
                                        arg_ty
                                    ),
                                ).with_help(
                                    "a `Number` parameter accepts any numeric family (Int8…Float64), \
                                     or a dynamic `Json` value (decoded as Int32, unchecked — use \
                                     `Int32.fromJson(v)` for a validated decode)".to_string()
                                ));
                            }
                        }
                    }
                }

                // Check argument compatibility against concrete params (mirror of the prefix
                // `infer_call` loop). The dot path previously omitted this entirely, so a wrong
                // callback PARAM ANNOTATION — e.g. `[1,2,3].map((x: String) => x)` or
                // `[1,2,3].map((x, i: String) => x)` — was silently accepted. (A bad callback
                // annotation pollutes the substitution for the element TypeVar, so the mismatch can
                // surface on EITHER the callback arg or the receiver arg, exactly as in the prefix
                // path; the loop therefore covers ALL args, including the receiver at index 0.)
                //
                // `arg_compatible` is the SAME helper the prefix path runs workspace-wide; it
                // tolerates unsubstituted generic TypeVars, opaque `Function` params, Json, and the
                // arity-width subtyping for shorter callbacks, so legitimate callbacks survive.
                //
                // ONE carve-out: a STREAMISH ARGUMENT flowing into a STREAM-ACCEPTING param. A
                // stream source intrinsic returns `Stream<T> | Error` (the fault-propagation shape),
                // and the stdlib stream adapters/combinators declare their stream params as either
                // `Stream` (e.g. `gzip = (s: Stream)`, `readText = (s: Stream)`) or the `Json`
                // wildcard (e.g. `concat = (a: Json, b: Json)` — the stream form dispatches on the
                // receiver). `arg_compatible` does NOT accept a `Stream<T>` (nor `Stream<T> | Error`)
                // against either: a Stream must never widen to `Json` (compat.rs), and the `| Error`
                // arm fails the union all-arms rule against a bare `Stream<U>`. The prefix path
                // rejects these too, but stream pipelines are written ONLY in dot form, so they
                // historically never ran any arg check. Compatibility/consumption for stream
                // arguments is instead governed by the dedicated streamish logic above
                // (`streamish_combinator_ret`, `consume_definite_stream_args`).
                //
                // So we skip the structural check for any STREAMISH argument whose parameter can
                // structurally ACCEPT a stream — `Stream<U>` or the `Json`/inference wildcard. This
                // is a STRUCTURAL gate (no stdlib name list), so it covers std/iter, std/stream,
                // std/archive AND std/compress (`gzip`/`gunzip`/`inflate`/`deflate`) uniformly,
                // including `concat`'s SECOND stream arg. A non-stream argument (an array/object
                // receiver, or the element-type conflict on `[1,2,3].map((x:String)=>x)`'s
                // receiver) is NOT streamish, so it is still checked exactly as the prefix path
                // does; and a stream flowing into a genuinely non-stream-accepting param is still
                // rejected.
                for (i, (arg, param_ty)) in
                    all_args.iter().zip(concrete_params.iter()).enumerate()
                {
                    let arg_ty = arg.ty();
                    if super::expr::type_is_streamish(&arg_ty)
                        && param_accepts_stream(param_ty)
                    {
                        continue;
                    }
                    if !self.arg_compatible(&arg_ty, param_ty) {
                        return Err(Diagnostic::error(
                            all_arg_exprs.get(i).map(|e| e.span()).unwrap_or(span),
                            format!(
                                "Argument {} has type {}, expected {}",
                                i + 1,
                                arg_ty,
                                param_ty
                            ),
                        ));
                    }
                }

                let concrete_ret = apply_type_subs(&ret, &subs);
                let result_type = if partial {
                    if all_args.len() < method_params.len() {
                        let remaining = concrete_params[all_args.len()..].to_vec();
                        let remaining_required = method_required.saturating_sub(all_args.len());
                        Type::Function {
                            params: remaining,
                            ret: Box::new(concrete_ret),
                            required: remaining_required,
                        }
                    } else {
                        concrete_ret
                    }
                } else {
                    if all_args.len() < method_required {
                        return Err(Diagnostic::error(
                            span,
                            format!(
                                "Too few arguments to '{}': expected at least {}, got {} (including the receiver)",
                                method, method_required, all_args.len()
                            ),
                        ).with_help("to partially apply, add a trailing comma: x.f(y,)".to_string()));
                    }
                    concrete_ret
                };

                // var-capture check for pool.async(f) / pool.async(fs).
                if method == "lin_async" || method == "lin_pool_async" {
                    let globals = self.mutable_global_slots.clone();
                    for arg in &all_args[1..] {
                        if let Some(var_name) = first_mutable_capture(arg, &globals) {
                            self.diagnostics.push(Diagnostic::error(
                                span,
                                format!(
                                    "async thunk captures mutable variable '{}' — sharing mutable state across threads is not allowed",
                                    var_name
                                ),
                            ).with_help("capture an immutable copy: `val snap = {}; pool.async(() => snap)`".to_string()));
                        }
                    }
                }

                // Affine consume (streams brief §7): if `method` routes to a stream op, mark each
                // definitely-stream argument consumed (mirrors the IR's `move_streamish_arg`).
                // `all_arg_exprs[0]` is the receiver; `concat`'s second stream arg is also covered.
                if !partial && self.callee_routes_to_stream_op(method) {
                    let pairs: Vec<(&Expr, Type)> = all_arg_exprs
                        .iter()
                        .copied()
                        .zip(all_args.iter())
                        .map(|(e, t)| (e, t.ty()))
                        .collect();
                    self.consume_definite_stream_args(pairs.iter().map(|(e, t)| (*e, t)));
                }

                // Receiver-dependent std/iter combinator return (unification Stage 2): when the dot
                // receiver is a Stream and `method` is a std/iter combinator, re-type the eager
                // array result to its stream-shaped form. Skipped for partial application.
                let result_type = if !partial {
                    let arg0_ty = all_args.first().map(|a| a.ty()).unwrap_or(Type::Null);
                    self.streamish_combinator_ret(method, result_type, &arg0_ty)
                } else {
                    result_type
                };

                let info = self.env.lookup(method).unwrap();
                let func_expr = TypedExpr::LocalGet { slot: info.slot, ty: method_ty, span };
                return Ok(TypedExpr::Call {
                    func: Box::new(func_expr),
                    args: all_args,
                    result_type,
                    is_tail: false,
                    partial,
                    span,
                });
            }
        }

        // Fallback: infer all args without type guidance.
        let mut all_args = vec![self.infer_expr(receiver)?];
        if let Some(arg_exprs) = args {
            for arg in arg_exprs {
                all_args.push(self.infer_expr(arg)?);
            }
        }
        // Affine consume (fallback path): mirror the typed-method path so a stream-routing op that
        // somehow reaches here still consumes its definitely-stream argument(s).
        if !partial && self.callee_routes_to_stream_op(method) {
            let all_arg_exprs: Vec<&Expr> = std::iter::once(receiver)
                .chain(args.as_ref().map(|a| a.as_slice()).unwrap_or(&[]).iter())
                .collect();
            let pairs: Vec<(&Expr, Type)> = all_arg_exprs
                .iter()
                .copied()
                .zip(all_args.iter())
                .map(|(e, t)| (e, t.ty()))
                .collect();
            self.consume_definite_stream_args(pairs.iter().map(|(e, t)| (*e, t)));
        }
        // var-capture check for pool.async(f) / pool.async(fs) (fallback path).
        if method == "lin_async" || method == "lin_pool_async" {
            let globals = self.mutable_global_slots.clone();
            for arg in &all_args[1..] {
                if let Some(var_name) = first_mutable_capture(arg, &globals) {
                    self.diagnostics.push(Diagnostic::error(
                        span,
                        format!(
                            "async thunk captures mutable variable '{}' — sharing mutable state across threads is not allowed",
                            var_name
                        ),
                    ).with_help("capture an immutable copy: `val snap = {}; pool.async(() => snap)`".to_string()));
                }
            }
        }
        if let Some(ty) = self.env.effective_type(method) {
            let result_type = match &ty {
                Type::Function { params, ret, required } => {
                    if partial {
                        if all_args.len() < params.len() {
                            let remaining = params[all_args.len()..].to_vec();
                            Type::Function {
                                params: remaining,
                                ret: ret.clone(),
                                required: required.saturating_sub(all_args.len()),
                            }
                        } else {
                            *ret.clone()
                        }
                    } else {
                        if all_args.len() < *required {
                            return Err(Diagnostic::error(
                                span,
                                format!(
                                    "Too few arguments to '{}': expected at least {}, got {} (including the receiver)",
                                    method, required, all_args.len()
                                ),
                            ).with_help("to partially apply, add a trailing comma: x.f(y,)".to_string()));
                        }
                        *ret.clone()
                    }
                }
                _ => self.env.fresh_type_var(),
            };
            let info = self.env.lookup(method).unwrap();
            let func_expr = TypedExpr::LocalGet { slot: info.slot, ty, span };
            Ok(TypedExpr::Call {
                func: Box::new(func_expr),
                args: all_args,
                result_type,
                is_tail: false,
                partial,
                span,
            })
        } else {
            Err(Diagnostic::error(span, format!("Undefined function '{}'", method)))
        }
    }

    pub(crate) fn is_tail_call(&self, func_expr: &Expr) -> bool {
        if !self.in_tail_position {
            return false;
        }
        if let Some(ref current_fn) = self.current_function {
            if let Expr::Ident(name, _) = func_expr {
                return name == current_fn;
            }
        }
        false
    }
}

/// The std/stream exports that take OWNERSHIP of their `Stream` argument (streams brief §7/§9):
/// every stream-specific op in `stdlib/stream.lin`. Each is a wrapper over a `lin_stream_*`
/// intrinsic that moves the boxed-stream pointer, so the IR's `move_streamish_arg`
/// (lin-ir/src/lower.rs) unregisters the arg from the caller's owning scope — the caller must
/// never use it again. There are NO borrow ops among the exports: even `close` ENDS the stream's
/// life (its box is released), and `promise` MOVES the whole pipeline onto a worker thread (the
/// worker becomes its sole owner — a parent reuse is a cross-thread use-after-move). This set MUST
/// stay in sync with `move_streamish_arg`'s rule (any streamish arg is moved); it exists only to
/// avoid consuming a stream passed to a NON-stream callee (none exist today — Stream is opaque and
/// rejected everywhere else — but the gate keeps the rule precise and divergence-proof).
fn is_std_stream_consuming_export(export_name: &str) -> bool {
    matches!(
        export_name,
        "lines" | "linesMax" | "chunks" | "writeStream" | "drain" | "collect"
            | "readText" | "close" | "promise"
    )
}

/// The std/iter combinator exports that dispatch to a `lin_stream_*` backend when their iterable
/// arg0 is a Stream (the SAME set as `stream_combinator_intrinsic_name` in lin-ir/src/lower.rs and
/// the re-typing set in `streamish_combinator_ret` above). On a definitely-stream arg0 the IR
/// redirects the call to the lazy backend, which MOVES the stream — so these consume it too.
/// `concat` takes TWO streams; BOTH are moved into the ConcatSource (handled by per-argument
/// consumption below, not arity-special-cased here).
fn is_std_iter_stream_combinator(export_name: &str) -> bool {
    matches!(
        export_name,
        "map" | "filter" | "take" | "drop" | "flatMap" | "takeWhile" | "dropWhile"
            | "flatten" | "concat" | "reduce" | "find" | "some" | "every" | "while" | "for"
    )
}

/// The std/archive exports that CONSUME their `Stream` argument: `untar` (terminal driver),
/// `manifest`/`files` (adapters that move the parent stream into the splitter). All three route to a
/// `lin_stream_*` intrinsic that moves the boxed-stream pointer, so a later use of the same binding
/// must be a compile-time error — mirrors the IR's type-based `move_streamish_arg`.
fn is_std_archive_consuming_export(export_name: &str) -> bool {
    matches!(export_name, "untar" | "manifest" | "files")
}

/// True when `param_ty` can structurally ACCEPT a `Stream` argument: a `Stream<U>` (incl. the
/// bare `Stream` = `Stream<Json>` the stdlib adapters declare), the `Json`/inference wildcard
/// (`TypeVar` — the `Json`-typed combinator params like `concat`'s, or an unsubstituted generic),
/// or a union that has any such member (a streamish iterable union). Used by the dot-call arg
/// compatibility carve-out: a streamish argument flowing into a stream-accepting param is governed
/// by the dedicated streamish typing/consume logic, not the structural `arg_compatible` check.
fn param_accepts_stream(param_ty: &Type) -> bool {
    match param_ty {
        Type::Stream(_) => true,
        Type::TypeVar(_) => true,
        Type::Union(variants) => variants.iter().any(param_accepts_stream),
        _ => false,
    }
}

/// True when `ty` can ONLY be a `Stream` at runtime — a bare `Stream`, or a union whose every
/// non-`Error` member is a Stream (e.g. the `Stream<…> | Error` shape a source intrinsic returns).
/// Distinct from `type_is_streamish`, which is true for ANY union that merely *includes* a Stream
/// (e.g. the `Array | Iterator | Stream` Iterable union). Receiver-dependent combinator re-typing
/// keys on THIS predicate: a mixed Iterable union has a live array branch, so its eager array
/// return must survive; only a definite-stream receiver flips the result to the stream-shaped type.
fn is_definitely_stream(ty: &Type) -> bool {
    match ty {
        Type::Stream(_) => true,
        Type::Union(variants) => {
            let mut saw_stream = false;
            for v in variants {
                match v {
                    Type::Stream(_) => saw_stream = true,
                    // An Error arm is tolerated (a source intrinsic's `Stream | Error`); any other
                    // non-stream member means the receiver could be a non-stream → not definite.
                    other if is_error_shape(other) => {}
                    _ => return false,
                }
            }
            saw_stream
        }
        _ => false,
    }
}

/// True if `ty` is the canonical fallible-stdlib Error object shape (`{type, message}`), the arm a
/// source intrinsic unions onto a `Stream`. Compared structurally against `resolve::error_type()`.
fn is_error_shape(ty: &Type) -> bool {
    ty == &crate::resolve::error_type()
}
