use lin_common::{Diagnostic, Span};
use lin_parse::ast::Expr;

use super::Checker;
use super::helpers::{apply_type_subs, first_mutable_capture, integer_range, is_definitely_non_transferable};
use crate::typed_ir::*;
use crate::types::Type;

impl Checker {
    pub(crate) fn infer_call(
        &mut self,
        func: &Expr,
        args: &[Expr],
        span: Span,
    ) -> Result<TypedExpr, Diagnostic> {
        // Function expression and arguments are not in tail position.
        let prev_tail = self.in_tail_position;
        self.in_tail_position = false;
        let typed_func = self.infer_expr(func)?;
        let func_ty = typed_func.ty();

        let (typed_args, result_type) = match &func_ty {
            Type::Function { params, ret } => {
                // Opaque `Function` annotation: all params and ret are TypeVar.
                // Accept any number of arguments and return a fresh TypeVar.
                let is_opaque = params.iter().all(|p| matches!(p, Type::TypeVar(_)))
                    && matches!(ret.as_ref(), Type::TypeVar(_));
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

                // Spec §26: a suffixless integer literal takes its context type. When an
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

                let result_type = if typed_args.len() < params.len() {
                    let remaining_params = concrete_params[typed_args.len()..].to_vec();
                    Type::Function {
                        params: remaining_params,
                        ret: Box::new(concrete_ret),
                    }
                } else {
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

        Ok(TypedExpr::Call {
            func: Box::new(typed_func),
            args: typed_args,
            result_type,
            is_tail,
            span,
        })
    }

    pub(crate) fn infer_dot_call(
        &mut self,
        receiver: &Expr,
        method: &str,
        args: &Option<Vec<Expr>>,
        span: Span,
    ) -> Result<TypedExpr, Diagnostic> {
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
                    span,
                };
                return self.infer_expr(&dummy_call);
            }
        }

        let typed_receiver = self.infer_expr(receiver)?;

        // Look up method type for TypeVar substitution.
        if let Some(method_ty) = self.env.effective_type(method) {
            if let Type::Function { params: method_params, ret } = method_ty.clone() {
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
                let result_type = if all_args.len() < method_params.len() {
                    let remaining = concrete_params[all_args.len()..].to_vec();
                    Type::Function { params: remaining, ret: Box::new(concrete_ret) }
                } else {
                    concrete_ret
                };

                // var-capture check for pool.async(f) / pool.async(fs).
                if method == "lin_async" {
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

                let info = self.env.lookup(method).unwrap();
                let func_expr = TypedExpr::LocalGet { slot: info.slot, ty: method_ty, span };
                return Ok(TypedExpr::Call {
                    func: Box::new(func_expr),
                    args: all_args,
                    result_type,
                    is_tail: false,
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
        if method == "lin_async" {
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
                Type::Function { params, ret } => {
                    if all_args.len() < params.len() {
                        let remaining = params[all_args.len()..].to_vec();
                        Type::Function { params: remaining, ret: ret.clone() }
                    } else {
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
