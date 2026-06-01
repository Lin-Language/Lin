use lin_common::{Diagnostic, Span};
use lin_parse::ast::Expr;

use super::Checker;
use super::helpers::{apply_type_subs, first_mutable_capture, integer_range, is_definitely_non_transferable};
use crate::resolve::{error_type, json_type};
use crate::typed_ir::*;
use crate::types::Type;

impl Checker {
    /// `fromJson` special form (ADR-047). `T.fromJson(value)` desugars to a DotCall and
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
            Type::Object(fields) => {
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

    pub(crate) fn infer_call(
        &mut self,
        func: &Expr,
        args: &[Expr],
        partial: bool,
        span: Span,
    ) -> Result<TypedExpr, Diagnostic> {
        // fromJson special form: `fromJson(T, value)` (ADR-047). Intercept before the callee
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
                // cast-hole gate (ADR-046), not be freshened into a permissive inference var.
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
                        let typed = self.infer_expr(arg)?;
                        self.collect_and_save_subs(param_ty, &typed.ty(), &mut subs);
                        partially_typed[i] = Some(typed);
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
                                match self.check_expr(arg, &expected) {
                                    Ok(t) => t,
                                    Err(_) => self.infer_expr(arg)?,
                                }
                            } else {
                                self.infer_expr(arg)?
                            }
                        }
                    };
                    // Collect substitutions from function args too (e.g. lambda return types).
                    self.collect_and_save_subs(&params[i], &typed.ty(), &mut subs);
                    typed_args.push(typed);
                }
                self.in_tail_position = prev_tail;

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
                }

                // Check argument compatibility against concrete params.
                for (i, (arg, param_ty)) in
                    typed_args.iter().zip(concrete_params.iter()).enumerate()
                {
                    let arg_ty = arg.ty();
                    if !self.types_compatible(&arg_ty, param_ty) {
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

                let concrete_ret = apply_type_subs(ret, &subs);
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

        // Affine consume (streams brief §7): an OWNERSHIP-TAKING stream op (adapter/terminal/`for`)
        // MOVES its stream argument — mark it consumed so a later use of the same binding errors.
        // Borrows (`read`/`close`) are NOT in the consuming set, so a pull loop reads freely.
        if !partial {
            if let (Expr::Ident(callee, _), Some(arg0)) = (func, args.first()) {
                if is_stream_consuming_op(callee) {
                    self.mark_stream_consumed(arg0);
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
                };
                return self.infer_expr(&dummy_call);
            }
        }

        // fromJson special form: `T.fromJson(value)` (ADR-047). Intercept before the receiver
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

        let typed_receiver = self.infer_expr(receiver)?;

        // Affine consume (streams brief §7): a dot-call `s.lines()`/`s.drain()`/… MOVES the
        // receiver stream into the ownership-taking op. Mark it consumed AFTER inferring the
        // receiver (so reading it AS the receiver is fine) so a later use of `s` errors. Borrows
        // (`s.read()`/`s.close()`) are not in the consuming set. Skipped for partial application.
        if !partial && is_stream_consuming_op(method) {
            self.mark_stream_consumed(receiver);
        }

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
                if let Some(p0) = method_params.first() {
                    self.collect_and_save_subs(p0, &typed_receiver.ty(), &mut subs);
                }
                let mut partially_typed: Vec<Option<TypedExpr>> = vec![None; all_arg_exprs.len()];
                partially_typed[0] = Some(typed_receiver);
                if let Some(arg_exprs) = args.as_ref() {
                    for (i, (arg, param_ty)) in arg_exprs.iter().zip(method_params.iter().skip(1)).enumerate() {
                        if !matches!(arg, Expr::Function { .. }) {
                            let typed = self.infer_expr(arg)?;
                            self.collect_and_save_subs(param_ty, &typed.ty(), &mut subs);
                            partially_typed[i + 1] = Some(typed);
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
                                match self.check_expr(arg_expr, &expected) {
                                    Ok(t) => t,
                                    Err(_) => self.infer_expr(arg_expr)?,
                                }
                            } else {
                                self.infer_expr(arg_expr)?
                            }
                        }
                    };
                    // Collect substitutions from lambda/function args too (e.g. to resolve return TypeVars).
                    if let Some(param_ty) = method_params.get(i) {
                        self.collect_and_save_subs(param_ty, &typed.ty(), &mut subs);
                    }
                    all_args.push(typed);
                }

                let concrete_params: Vec<Type> = method_params.iter()
                    .map(|p| apply_type_subs(p, &subs))
                    .collect();
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

/// True for an OWNERSHIP-TAKING stream operation (streams brief §7): the adapters and terminal
/// drivers that MOVE their stream argument and must not be followed by another use of the same
/// binding. Borrowing ops that read/close a stream in place without consuming the whole pipeline
/// (`read`/`readChunk`/`close`/`closeStream`) are deliberately ABSENT, so a low-level pull loop
/// may read the same stream binding repeatedly. Keyed on the std/stream (and std/net/process/io)
/// wrapper names, which are how user code names these ops at the call/dot-call site.
pub(crate) fn is_stream_consuming_op(name: &str) -> bool {
    matches!(
        name,
        "lines" | "map" | "filter" | "take" | "chunks" | "writeStream"
            | "drain" | "collect" | "readText" | "for"
    )
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
