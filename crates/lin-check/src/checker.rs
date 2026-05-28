use indexmap::IndexMap;
use lin_common::{Diagnostic, Span};
use lin_parse::ast::{
    BinOp, Expr, MatchArm, MatchPattern, Module, ObjectField, Param, Pattern, Stmt, StringPart,
};

use crate::compat::is_compatible_env;
use crate::env::TypeEnv;
use crate::resolve::resolve_type;
use crate::typed_ir::*;
use crate::types::Type;
use crate::widen::widen_numeric;

/// Collect TypeVar substitutions from matching `actual` against `pattern`.
/// E.g., matching `Iterator<Int32>` against `Iterator<TypeVar(9010)>` yields `9010 -> Int32`.
/// TypeVar(u32::MAX) is the special "any"/"Json" wildcard — never substituted.
fn collect_type_subs(pattern: &Type, actual: &Type, subs: &mut std::collections::HashMap<u32, Type>) {
    match (pattern, actual) {
        (Type::TypeVar(id), _) if *id == u32::MAX => {}  // Json wildcard: skip
        (Type::TypeVar(id), t) => { subs.insert(*id, t.clone()); }
        (Type::Array(pt), Type::Array(at)) => collect_type_subs(pt, at, subs),
        (Type::Array(pt), Type::FixedArray(ats)) => {
            for at in ats { collect_type_subs(pt, at, subs); }
        }
        (Type::Iterator(pt), Type::Iterator(at)) => collect_type_subs(pt, at, subs),
        (Type::Union(pts), actual) => {
            for pt in pts { collect_type_subs(pt, actual, subs); }
        }
        (Type::Function { params: pp, ret: pr }, Type::Function { params: ap, ret: ar }) => {
            for (p, a) in pp.iter().zip(ap.iter()) { collect_type_subs(p, a, subs); }
            collect_type_subs(pr, ar, subs);
        }
        _ => {}
    }
}

/// Apply collected substitutions to a type.
fn apply_type_subs(ty: &Type, subs: &std::collections::HashMap<u32, Type>) -> Type {
    match ty {
        Type::TypeVar(id) => subs.get(id).cloned().unwrap_or_else(|| ty.clone()),
        Type::Array(t) => Type::Array(Box::new(apply_type_subs(t, subs))),
        Type::Iterator(t) => Type::Iterator(Box::new(apply_type_subs(t, subs))),
        Type::Union(ts) => Type::Union(ts.iter().map(|t| apply_type_subs(t, subs)).collect()),
        Type::Function { params, ret } => Type::Function {
            params: params.iter().map(|p| apply_type_subs(p, subs)).collect(),
            ret: Box::new(apply_type_subs(ret, subs)),
        },
        _ => ty.clone(),
    }
}

pub struct Checker {
    env: TypeEnv,
    diagnostics: Vec<Diagnostic>,
    current_function: Option<String>,
    /// True when compiling an expression that is in tail position of current_function.
    in_tail_position: bool,
    intrinsic_slots: std::collections::HashMap<usize, String>,
    /// Set of slots that were forward-declared and should reuse their slot on binding.
    forward_declared: std::collections::HashSet<usize>,
    /// Stack of capture sets — one entry per nested function being compiled.
    /// The inner-most function accumulates captures here.
    capture_stack: Vec<std::collections::HashMap<usize, Capture>>,
    /// Scope depth when each function was entered (parallel to capture_stack).
    function_scope_depths: Vec<usize>,
    /// (use_span, display_type, def_span) — collected for every identifier use.
    /// Used by the LSP for hover and go-to-definition.
    pub span_type_map: Vec<(Span, String, Option<Span>)>,
    /// Pre-resolved import types: (module_path, export_name) -> Type.
    /// When set, used instead of fresh TypeVars for import bindings.
    pub import_types: std::collections::HashMap<(String, String), Type>,
    /// Global accumulator of TypeVar solutions discovered during inference.
    /// Populated by every call to collect_type_subs. Used by the zonking pass.
    solved_type_vars: std::collections::HashMap<u32, Type>,
    /// TypeVar IDs from imported module signatures. These are generic "any" slots
    /// that must never be solved to a concrete type in this module's zonking pass.
    protected_type_vars: std::collections::HashSet<u32>,
    /// Slots of mutable global (`var`) bindings. Used by the async var-capture check.
    mutable_global_slots: std::collections::HashMap<usize, String>,
}

impl Checker {
    pub fn new() -> Self {
        Self {
            env: TypeEnv::new(),
            diagnostics: Vec::new(),
            current_function: None,
            in_tail_position: false,
            intrinsic_slots: std::collections::HashMap::new(),
            forward_declared: std::collections::HashSet::new(),
            capture_stack: Vec::new(),
            function_scope_depths: Vec::new(),
            span_type_map: Vec::new(),
            import_types: std::collections::HashMap::new(),
            solved_type_vars: std::collections::HashMap::new(),
            protected_type_vars: std::collections::HashSet::new(),
            mutable_global_slots: std::collections::HashMap::new(),
        }
    }

    pub fn check_module(&mut self, module: &Module) -> Result<TypedModule, Vec<Diagnostic>> {
        self.register_intrinsics();

        // Pre-scan: forward-declare all top-level type aliases as Named placeholders
        // so that recursive types (type Tree = { ..., children: Tree[] }) can be resolved.
        self.forward_declare_types(module);

        // Pre-scan: forward-declare all top-level val bindings whose RHS is a
        // function literal so mutual recursion works (mirrors ADR-015).
        self.forward_declare_functions(module);

        let mut stmts = Vec::new();
        for stmt in &module.statements {
            match self.check_stmt(stmt) {
                Ok(typed_stmt) => stmts.push(typed_stmt),
                Err(diag) => self.diagnostics.push(diag),
            }
        }

        if self.diagnostics.iter().any(|d| d.severity == lin_common::Severity::Error) {
            Err(self.diagnostics.clone())
        } else {
            let mut typed_module = TypedModule {
                statements: stmts,
                span: module.span,
                intrinsics: self.intrinsic_slots.clone(),
            };
            // Zonking pass: replace solved TypeVar nodes with their concrete types.
            let subs = self.solved_type_vars.clone();
            crate::zonk::zonk_module(&mut typed_module, &subs);
            Ok(typed_module)
        }
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    fn types_compatible(&self, value: &Type, target: &Type) -> bool {
        is_compatible_env(value, target, Some(&self.env), &mut 0)
    }

    fn check_stmt(&mut self, stmt: &Stmt) -> Result<TypedStmt, Diagnostic> {
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

                let mut typed_value = match (value, binding_name) {
                    (Expr::Function { params, return_type, body, span }, Some(name)) => {
                        self.infer_function(params, return_type, body, *span, Some(name))?
                    }
                    _ => {
                        if let Some(ref expected_ty) = expected {
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
                let resolved = resolve_type(body, &self.env)
                    .map_err(|e| Diagnostic::error(*span, e))?;
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
                            format!("Foreign binding '{}' has illegal FFI type '{}'; only numeric primitives, Boolean, Null (return only), and String (argument only) are allowed (spec §34.3)", binding.name, ty),
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

    fn check_expr(&mut self, expr: &Expr, expected: &Type) -> Result<TypedExpr, Diagnostic> {
        // For function expressions with a known expected function type, use the expected
        // param types to guide inference (bidirectional type checking).
        if let (Expr::Function { params, return_type, body, span }, Type::Function { params: expected_params, ret: expected_ret }) = (expr, expected) {
            return self.infer_function_with_hints(params, return_type, body, *span, None, expected_params, expected_ret);
        }

        let inferred = self.infer_expr(expr)?;
        let actual_ty = inferred.ty();

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

    fn infer_expr(&mut self, expr: &Expr) -> Result<TypedExpr, Diagnostic> {
        match expr {
            Expr::IntLit(v, span)    => Ok(TypedExpr::IntLit(*v, Type::Int32, *span)),
            Expr::FloatLit(v, span)  => Ok(TypedExpr::FloatLit(*v, Type::Float64, *span)),
            Expr::StringLit(s, span) => Ok(TypedExpr::StringLit(s.clone(), *span)),
            Expr::BoolLit(b, span)   => Ok(TypedExpr::BoolLit(*b, *span)),
            Expr::NullLit(span)      => Ok(TypedExpr::NullLit(*span)),
            Expr::Ident(name, span)  => self.infer_ident(name, *span),
            Expr::BinaryOp { left, op, right, span } => self.infer_binary_op(left, *op, right, *span),
            Expr::Call { func, args, span }           => self.infer_call(func, args, *span),
            Expr::DotCall { receiver, method, args, span } => self.infer_dot_call(receiver, method, args, *span),
            Expr::Index { object, key, span }         => self.infer_index(object, key, *span),
            Expr::If { condition, then_branch, else_branch, span } => self.infer_if(condition, then_branch, else_branch, *span),
            Expr::Match { scrutinee, arms, span }     => self.infer_match(scrutinee, arms, *span),
            Expr::Block(stmts, final_expr, span)      => self.infer_block(stmts, final_expr, *span),
            Expr::Function { params, return_type, body, span } => self.infer_function(params, return_type, body, *span, None),
            Expr::Object(fields, span)                => self.infer_object(fields, *span),
            Expr::Array(elements, span)               => self.infer_array(elements, *span),
            Expr::Assign { target, value, span }      => self.infer_assign(target, value, *span),
            Expr::IndexAssign { object, key, value, span } => self.infer_index_assign(object, key, value, *span),
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

    fn infer_ident(&mut self, name: &str, span: Span) -> Result<TypedExpr, Diagnostic> {
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
        self.span_type_map.push((span, ty.to_string(), def_span));
        Ok(TypedExpr::LocalGet { slot, ty, span })
    }

    fn infer_index(&mut self, object: &Expr, key: &Expr, span: Span) -> Result<TypedExpr, Diagnostic> {
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
            Type::Object(fields) => {
                if fields.is_empty() {
                    // Empty schema (e.g. `var result = {}`): object may be populated dynamically,
                    // so any key access must be a runtime lookup → TypeVar.
                    self.env.fresh_type_var()
                } else if let TypedExpr::StringLit(ref key_str, _) = typed_key {
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
                            Type::Object(fields) => {
                                if let TypedExpr::StringLit(ref key_str, _) = typed_key {
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
                    } else {
                        self.env.fresh_type_var()
                    };
                    Type::flatten_union(vec![inner, Type::Null])
                }
            }
            _ => return Err(Diagnostic::error(span, format!("Cannot index into type {}", obj_ty))),
        };
        Ok(TypedExpr::Index { object: Box::new(typed_obj), key: Box::new(typed_key), result_type, span })
    }

    fn infer_if(&mut self, condition: &Expr, then_branch: &Expr, else_branch: &Expr, span: Span) -> Result<TypedExpr, Diagnostic> {
        // Condition is not in tail position; branches inherit it.
        let in_tail = self.in_tail_position;
        self.in_tail_position = false;
        let typed_cond = self.check_expr(condition, &Type::Bool)?;
        self.in_tail_position = in_tail;
        let typed_then = self.infer_expr(then_branch)?;
        self.in_tail_position = in_tail;
        let typed_else = self.infer_expr(else_branch)?;
        let then_ty = typed_then.ty();
        let else_ty = typed_else.ty();
        let result_type = if self.types_compatible(&then_ty, &else_ty) {
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

    fn infer_match(&mut self, scrutinee: &Expr, arms: &[MatchArm], span: Span) -> Result<TypedExpr, Diagnostic> {
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
        for arm in arms {
            let typed_arm = self.check_match_arm(arm, &scrutinee_ty, scrutinee_name)?;
            arm_types.push(typed_arm.body.ty());
            typed_arms.push(typed_arm);
        }
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

    fn infer_block(&mut self, stmts: &[Stmt], final_expr: &Expr, span: Span) -> Result<TypedExpr, Diagnostic> {
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

    fn infer_object(&mut self, fields: &[ObjectField], span: Span) -> Result<TypedExpr, Diagnostic> {
        let mut typed_fields = Vec::new();
        let mut spreads = Vec::new();
        let mut obj_type = IndexMap::new();
        for field in fields {
            match field {
                ObjectField::Pair(key_expr, val_expr) => {
                    let typed_val = self.infer_expr(val_expr)?;
                    let val_ty = typed_val.ty();
                    if let Expr::StringLit(key, _) = key_expr {
                        obj_type.insert(key.clone(), val_ty);
                        typed_fields.push((key.clone(), typed_val));
                    }
                }
                ObjectField::Spread(expr) => {
                    let typed_spread = self.infer_expr(expr)?;
                    if let Type::Object(ref fields) = typed_spread.ty() {
                        for (k, v) in fields { obj_type.insert(k.clone(), v.clone()); }
                    }
                    spreads.push(typed_spread);
                }
            }
        }
        Ok(TypedExpr::MakeObject { fields: typed_fields, spreads, ty: Type::Object(obj_type), span })
    }

    fn infer_array(&mut self, elements: &[Expr], span: Span) -> Result<TypedExpr, Diagnostic> {
        let typed_elements: Result<Vec<_>, _> = elements.iter().map(|e| self.infer_expr(e)).collect();
        let typed_elements = typed_elements?;
        let elem_types: Vec<Type> = typed_elements.iter().map(|t| t.ty()).collect();
        let ty = if elem_types.is_empty() {
            Type::Array(Box::new(Type::Never))
        } else {
            Type::Array(Box::new(unify_types(&elem_types)))
        };
        Ok(TypedExpr::MakeArray { elements: typed_elements, ty, span })
    }

    fn infer_assign(&mut self, target: &str, value: &Expr, span: Span) -> Result<TypedExpr, Diagnostic> {
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

    fn infer_index_assign(&mut self, object: &Expr, key: &Expr, value: &Expr, span: Span) -> Result<TypedExpr, Diagnostic> {
        let typed_obj = self.infer_expr(object)?;
        let typed_key = self.infer_expr(key)?;
        let obj_ty = typed_obj.ty();
        let typed_value = match &obj_ty {
            Type::Object(fields) => {
                if let TypedExpr::StringLit(ref key_str, _) = typed_key {
                    if let Some(field_ty) = fields.get(key_str) {
                        self.check_expr(value, field_ty)?
                    } else {
                        self.infer_expr(value)?
                    }
                } else {
                    self.infer_expr(value)?
                }
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

    fn infer_string_interp(&mut self, parts: &[StringPart], span: Span) -> Result<TypedExpr, Diagnostic> {
        let mut typed_parts = Vec::new();
        for part in parts {
            match part {
                StringPart::Literal(s) => typed_parts.push(TypedStringPart::Literal(s.clone())),
                StringPart::Expr(e) => typed_parts.push(TypedStringPart::Expr(self.infer_expr(e)?)),
            }
        }
        Ok(TypedExpr::StringInterp { parts: typed_parts, span })
    }

    fn infer_binary_op(
        &mut self,
        left: &Expr,
        op: BinOp,
        right: &Expr,
        span: Span,
    ) -> Result<TypedExpr, Diagnostic> {
        // Binary operands are never in tail position.
        let prev_tail = std::mem::replace(&mut self.in_tail_position, false);
        let typed_left = self.infer_expr(left)?;
        let typed_right = self.infer_expr(right)?;
        self.in_tail_position = prev_tail;
        let left_ty = typed_left.ty();
        let right_ty = typed_right.ty();

        let result_type = match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                let left_is_any = matches!(left_ty, Type::TypeVar(_));
                let right_is_any = matches!(right_ty, Type::TypeVar(_));
                if op == BinOp::Add && (left_ty == Type::Str || right_ty == Type::Str) {
                    return Err(Diagnostic::error(
                        span,
                        "String concatenation with + is not supported; use interpolation: \"${a}${b}\"".to_string(),
                    ));
                } else if left_ty.is_numeric() && right_ty.is_numeric() {
                    widen_numeric(&left_ty, &right_ty).unwrap_or(Type::Int32)
                } else if left_is_any || right_is_any {
                    // Dynamic operand — use the known side's type, or Int32 as fallback
                    if left_is_any { right_ty.clone() } else { left_ty.clone() }
                } else {
                    return Err(Diagnostic::error(
                        span,
                        format!(
                            "Cannot apply operator {:?} to {} and {}",
                            op, left_ty, right_ty
                        ),
                    ));
                }
            }
            BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                Type::Bool
            }
            BinOp::And | BinOp::Or => Type::Bool,
        };

        Ok(TypedExpr::BinaryOp {
            left: Box::new(typed_left),
            op,
            right: Box::new(typed_right),
            result_type,
            span,
        })
    }

    fn infer_call(
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
                        if params.len() > 0 { format!(" — this function takes {}", params.len()) } else { " — this function takes no arguments".to_string() }
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

    fn infer_dot_call(
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

    fn infer_function(
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
    fn infer_function_with_hints(
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

    fn check_match_arm(
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

    fn check_pattern(
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

    fn is_tail_call(&self, func_expr: &Expr) -> bool {
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

    fn bind_pattern(
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
    fn collect_and_save_subs(&mut self, pattern: &Type, actual: &Type, local: &mut std::collections::HashMap<u32, Type>) {
        collect_type_subs(pattern, actual, local);
        for (id, ty) in local.iter() {
            // Intrinsic TypeVars (≥ 9000) are generic slots — don't solve them globally.
            // Protected TypeVars come from imported module signatures — never solve them either.
            if *id < 9000 && !self.protected_type_vars.contains(id) {
                self.solved_type_vars.entry(*id).or_insert_with(|| ty.clone());
            }
        }
    }

    /// Collect all TypeVar IDs recursively from a type into `out`.
    fn collect_typevar_ids(ty: &Type, out: &mut std::collections::HashSet<u32>) {
        match ty {
            Type::TypeVar(id) => { out.insert(*id); }
            Type::Array(t) | Type::Iterator(t) => Self::collect_typevar_ids(t, out),
            Type::FixedArray(ts) => { for t in ts { Self::collect_typevar_ids(t, out); } }
            Type::Union(ts) => { for t in ts { Self::collect_typevar_ids(t, out); } }
            Type::Function { params, ret } => {
                for p in params { Self::collect_typevar_ids(p, out); }
                Self::collect_typevar_ids(ret, out);
            }
            Type::Object(fields) => { for v in fields.values() { Self::collect_typevar_ids(v, out); } }
            _ => {}
        }
    }

    /// Register TypeVar IDs from all import types as protected so they won't be
    /// solved/zonked based on call-site argument types in this module.
    pub fn protect_import_typevars(&mut self) {
        let types: Vec<Type> = self.import_types.values().cloned().collect();
        for ty in &types {
            Self::collect_typevar_ids(ty, &mut self.protected_type_vars);
        }
    }

    fn define_intrinsic(&mut self, name: &str, ty: Type) {
        let slot = self.env.define(name.to_string(), ty, false);
        self.intrinsic_slots.insert(slot, name.to_string());
    }

    /// Forward-declare top-level `val name = (...) => ...` functions so that
    /// they can call each other (mutual recursion, ADR-015 equivalent).
    /// Pre-register all top-level type aliases as Named(name) placeholders.
    /// This allows recursive types to be resolved: when `type Tree = { ..., children: Tree[] }`
    /// is resolved, the occurrence of `Tree` in the body will be already in the env.
    fn forward_declare_types(&mut self, module: &Module) {
        for stmt in &module.statements {
            if let Stmt::TypeDecl { name, params, .. } = stmt {
                // Register a placeholder body of Named(name) for now; the real body
                // will be resolved and replaced when check_stmt processes TypeDecl.
                // Using Named(name) as the placeholder means self-references in the body
                // will be detected by the cycle guard in resolve.rs and left as Named(name).
                self.env.define_type(
                    name.clone(),
                    params.clone(),
                    Type::Named(name.clone()),
                );
            }
        }
    }

    fn forward_declare_functions(&mut self, module: &Module) {
        for stmt in &module.statements {
            if let Stmt::Val { pattern, value, .. } = stmt {
                if let Expr::Function { params, return_type, .. } = value {
                    let name = match pattern {
                        lin_parse::ast::Pattern::Ident(n, _) => Some(n.clone()),
                        _ => None,
                    };
                    if let Some(name) = name {
                        let param_types: Vec<Type> = params
                            .iter()
                            .map(|p| {
                                p.type_ann
                                    .as_ref()
                                    .and_then(|t| resolve_type(t, &self.env).ok())
                                    .unwrap_or(Type::TypeVar(self.env.next_slot() as u32))
                            })
                            .collect();
                        let ret_type = return_type
                            .as_ref()
                            .and_then(|t| resolve_type(t, &self.env).ok())
                            .unwrap_or(Type::TypeVar(self.env.next_slot() as u32));
                        let fn_type = Type::Function {
                            params: param_types,
                            ret: Box::new(ret_type),
                        };
                        let slot = self.env.define(name, fn_type, false);
                        self.forward_declared.insert(slot);
                    }
                }
            }
        }
    }

    fn register_intrinsics(&mut self) {
        // print: (T) => Null — accepts any value, converts to string at runtime
        let print_param = self.env.fresh_type_var();
        self.define_intrinsic(
            "lin_print",
            Type::Function {
                params: vec![print_param],
                ret: Box::new(Type::Null),
            },
        );

        // toString: (T) => String — accepts any value
        let to_string_param = self.env.fresh_type_var();
        self.define_intrinsic(
            "lin_to_string",
            Type::Function {
                params: vec![to_string_param],
                ret: Box::new(Type::Str),
            },
        );

        // length: (String | Array<T> | Iterator<T> | Object) => Int32
        // Uses TypeVar(u32::MAX) as the "any" Json type for the object case.
        self.define_intrinsic(
            "lin_length",
            Type::Function {
                params: vec![Type::Union(vec![
                    Type::Str,
                    Type::Array(Box::new(Type::TypeVar(9000))),
                    Type::Iterator(Box::new(Type::TypeVar(9000))),
                    Type::TypeVar(u32::MAX),
                ])],
                ret: Box::new(Type::Int32),
            },
        );

        // push: (T[], T) => Null
        self.define_intrinsic(
            "lin_push",
            Type::Function {
                params: vec![
                    Type::Array(Box::new(Type::TypeVar(9001))),
                    Type::TypeVar(9001),
                ],
                ret: Box::new(Type::Null),
            },
        );

        // set: (T[], Int32, T) => Null — in-place array element mutation
        self.define_intrinsic(
            "lin_array_set",
            Type::Function {
                params: vec![
                    Type::Array(Box::new(Type::TypeVar(9200))),
                    Type::Int32,
                    Type::TypeVar(9200),
                ],
                ret: Box::new(Type::Null),
            },
        );

        // keys: (Object) => String[]
        self.define_intrinsic(
            "lin_keys",
            Type::Function {
                params: vec![Type::Object(IndexMap::new())],
                ret: Box::new(Type::Array(Box::new(Type::Str))),
            },
        );

        // lin_object_set: (Object, String, Json) => Null — in-place object key mutation
        self.define_intrinsic(
            "lin_object_set",
            Type::Function {
                params: vec![Type::Object(IndexMap::new()), Type::Str, Type::TypeVar(u32::MAX)],
                ret: Box::new(Type::Null),
            },
        );

        // for: (Iterable<T>, (T) => Json) => Null  — callback return type is ignored
        self.define_intrinsic(
            "lin_for",
            Type::Function {
                params: vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9010))),
                        Type::Iterator(Box::new(Type::TypeVar(9010))),
                    ]),
                    Type::Function {
                        params: vec![Type::TypeVar(9010)],
                        ret: Box::new(Type::TypeVar(u32::MAX)),
                    },
                ],
                ret: Box::new(Type::Null),
            },
        );

        // while: (Array<T> | Iterator<T>, (T) => Boolean) => Null
        self.define_intrinsic(
            "lin_while",
            Type::Function {
                params: vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9011))),
                        Type::Iterator(Box::new(Type::TypeVar(9011))),
                    ]),
                    Type::Function {
                        params: vec![Type::TypeVar(9011)],
                        ret: Box::new(Type::Bool),
                    },
                ],
                ret: Box::new(Type::Null),
            },
        );

        // iter: (() => State, (State) => Boolean, (State) => State, (State) => T) => Iterator<T>
        self.define_intrinsic(
            "lin_iter",
            Type::Function {
                params: vec![
                    Type::Function {
                        params: vec![],
                        ret: Box::new(Type::TypeVar(9020)),
                    },
                    Type::Function {
                        params: vec![Type::TypeVar(9020)],
                        ret: Box::new(Type::Bool),
                    },
                    Type::Function {
                        params: vec![Type::TypeVar(9020)],
                        ret: Box::new(Type::TypeVar(9020)),
                    },
                    Type::Function {
                        params: vec![Type::TypeVar(9020)],
                        ret: Box::new(Type::TypeVar(9021)),
                    },
                ],
                ret: Box::new(Type::Iterator(Box::new(Type::TypeVar(9021)))),
            },
        );

        // range: (Int32, Int32) => Iterator<Int32>
        self.define_intrinsic(
            "lin_range",
            Type::Function {
                params: vec![Type::Int32, Type::Int32],
                ret: Box::new(Type::Iterator(Box::new(Type::Int32))),
            },
        );

        // map: (Iterable<T>, (T) => U) => Iterator<U>
        self.define_intrinsic(
            "lin_map",
            Type::Function {
                params: vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9030))),
                        Type::Iterator(Box::new(Type::TypeVar(9030))),
                    ]),
                    Type::Function {
                        params: vec![Type::TypeVar(9030)],
                        ret: Box::new(Type::TypeVar(9031)),
                    },
                ],
                ret: Box::new(Type::Iterator(Box::new(Type::TypeVar(9031)))),
            },
        );

        // filter: (Iterable<T>, (T) => Boolean) => Iterator<T>
        self.define_intrinsic(
            "lin_filter",
            Type::Function {
                params: vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9040))),
                        Type::Iterator(Box::new(Type::TypeVar(9040))),
                    ]),
                    Type::Function {
                        params: vec![Type::TypeVar(9040)],
                        ret: Box::new(Type::Bool),
                    },
                ],
                ret: Box::new(Type::Iterator(Box::new(Type::TypeVar(9040)))),
            },
        );

        // reduce: (Iterable<T>, U, (U, T) => U) => U
        self.define_intrinsic(
            "lin_reduce",
            Type::Function {
                params: vec![
                    Type::Union(vec![
                        Type::Array(Box::new(Type::TypeVar(9050))),
                        Type::Iterator(Box::new(Type::TypeVar(9050))),
                    ]),
                    Type::TypeVar(9051),
                    Type::Function {
                        params: vec![Type::TypeVar(9051), Type::TypeVar(9050)],
                        ret: Box::new(Type::TypeVar(9051)),
                    },
                ],
                ret: Box::new(Type::TypeVar(9051)),
            },
        );

        // Concurrency intrinsics (spec §32)
        // async: (() => T) => Promise<T>  (TypeVar-based, overloaded: also accepts T[])
        let promise_t = Type::TypeVar(9100);
        self.define_intrinsic("lin_async", Type::Function {
            params: vec![Type::Union(vec![
                Type::Function { params: vec![], ret: Box::new(promise_t.clone()) },
                Type::Array(Box::new(Type::Function { params: vec![], ret: Box::new(promise_t.clone()) })),
            ])],
            ret: Box::new(Type::TypeVar(9100)),
        });
        // await: accepts a promise or array of promises
        self.define_intrinsic("lin_await", Type::Function {
            params: vec![Type::TypeVar(9101)],
            ret: Box::new(Type::TypeVar(9101)),
        });
        // parallel: variadic — always returns a tagged array (TypeVar(u32::MAX) = Json/any).
        // Using u32::MAX prevents zonking from resolving the element type to a flat scalar,
        // which would cause codegen to use a flat array representation for a tagged array.
        self.define_intrinsic("lin_parallel", Type::Function {
            params: vec![Type::Array(Box::new(Type::Function {
                params: vec![],
                ret: Box::new(Type::TypeVar(9102)),
            }))],
            ret: Box::new(Type::Array(Box::new(Type::TypeVar(u32::MAX)))),
        });
        // race: Promise[] => Promise
        self.define_intrinsic("lin_race", Type::Function {
            params: vec![Type::Array(Box::new(Type::TypeVar(9103)))],
            ret: Box::new(Type::TypeVar(9103)),
        });
        // timeout: (Promise, Int32) => Promise
        self.define_intrinsic("lin_timeout", Type::Function {
            params: vec![Type::TypeVar(9104), Type::Int32],
            ret: Box::new(Type::TypeVar(9104)),
        });
        // retry: (() => T, Int32) => Promise<T>
        self.define_intrinsic("lin_retry", Type::Function {
            params: vec![
                Type::Function { params: vec![], ret: Box::new(Type::TypeVar(9105)) },
                Type::Int32,
            ],
            ret: Box::new(Type::TypeVar(9105)),
        });
        // threadPool: (Int32) => ThreadPool
        self.define_intrinsic("lin_thread_pool", Type::Function {
            params: vec![Type::Int32],
            ret: Box::new(Type::TypeVar(9106)),
        });
        // worker: ((Msg) => Reply, () => Null) => Worker
        self.define_intrinsic("lin_worker", Type::Function {
            params: vec![
                Type::Function { params: vec![Type::TypeVar(9107)], ret: Box::new(Type::TypeVar(9108)) },
                Type::Function { params: vec![], ret: Box::new(Type::Null) },
            ],
            ret: Box::new(Type::TypeVar(9109)),
        });
        // worker.request(msg): (Worker, Msg) => Reply
        self.define_intrinsic("lin_request", Type::Function {
            params: vec![Type::TypeVar(9109), Type::TypeVar(9107)],
            ret: Box::new(Type::TypeVar(9108)),
        });
        // worker.message(msg): (Worker, Msg) => Null
        self.define_intrinsic("lin_message", Type::Function {
            params: vec![Type::TypeVar(9109), Type::TypeVar(9107)],
            ret: Box::new(Type::Null),
        });
        // worker.close(): (Worker) => Null
        self.define_intrinsic("lin_close", Type::Function {
            params: vec![Type::TypeVar(9109)],
            ret: Box::new(Type::Null),
        });

        // exit: (Int32) => Null — terminates the process with a status code
        self.define_intrinsic("lin_exit", Type::Function {
            params: vec![Type::Int32],
            ret: Box::new(Type::Null),
        });

        // value_key: (any) => String — canonical type-tagged key for any value
        self.define_intrinsic("lin_value_key", Type::Function {
            params: vec![Type::TypeVar(u32::MAX)],
            ret: Box::new(Type::Str),
        });

        // arrayAllocate(n) => Json[] — null-filled tagged array of length n
        self.define_intrinsic("lin_array_allocate", Type::Function {
            params: vec![Type::Int32],
            ret: Box::new(Type::Array(Box::new(Type::TypeVar(u32::MAX)))),
        });

        // arrayAllocateFilled(n, val) => T[] — flat scalar array of length n filled with val
        // Uses TypeVar(u32::MAX) for val so any scalar can be passed; returns Json[] (TypeVar).
        self.define_intrinsic("lin_array_allocate_filled", Type::Function {
            params: vec![Type::Int32, Type::TypeVar(u32::MAX)],
            ret: Box::new(Type::Array(Box::new(Type::TypeVar(u32::MAX)))),
        });
    }
}

/// Returns true if `ty` is definitely non-transferable across thread boundaries.
/// Non-transferable: Function, Iterator, Never.
/// TypeVar (unknown), Promise/Worker/ThreadPool (TypeVar-resolved), are not flagged —
/// we only reject types we can statically prove are non-transferable (spec §32.3).
fn is_definitely_non_transferable(ty: &Type) -> bool {
    match ty {
        Type::Function { .. } | Type::Iterator(_) | Type::Never => true,
        Type::Array(inner) => is_definitely_non_transferable(inner),
        Type::Union(ts) => ts.iter().any(is_definitely_non_transferable),
        _ => false,
    }
}

/// Returns true if `ty` is a legal FFI value type per spec §34.3.
/// Legal: Int8–Int64, UInt8–UInt64, Float32, Float64, Boolean, Null, String.
fn is_legal_ffi_value_type(ty: &Type) -> bool {
    matches!(ty,
        Type::Int8 | Type::Int16 | Type::Int32 | Type::Int64
        | Type::UInt8 | Type::UInt16 | Type::UInt32 | Type::UInt64
        | Type::Float32 | Type::Float64
        | Type::Bool | Type::Null | Type::Str
    )
}

/// Returns true if `ty` is a legal FFI binding type per spec §34.3.
/// The binding must be a function type whose params and return are legal value types.
fn is_legal_ffi_type(ty: &Type) -> bool {
    match ty {
        Type::Function { params, ret } => {
            params.iter().all(is_legal_ffi_value_type) && is_legal_ffi_value_type(ret)
        }
        _ => false,
    }
}

/// Returns the name of the first mutable capture (or global var reference) found in a
/// directly-nested `TypedExpr::Function`, or `None` if there are none.
/// Does NOT recurse into inner functions.
fn first_mutable_capture(
    expr: &TypedExpr,
    mutable_globals: &std::collections::HashMap<usize, String>,
) -> Option<String> {
    match expr {
        TypedExpr::Function { captures, body, .. } => {
            // Check explicit captures (non-global vars captured from outer scope).
            if let Some(c) = captures.iter().find(|c| c.is_mutable) {
                return Some(c.name.clone());
            }
            // Check if the body references any mutable global slot.
            first_mutable_global_in_body(body, mutable_globals)
        }
        TypedExpr::MakeArray { elements, .. } => {
            elements.iter().find_map(|e| first_mutable_capture(e, mutable_globals))
        }
        _ => None,
    }
}

/// Walk a `TypedExpr` body looking for a `LocalGet` that references a mutable global slot.
/// Stops at nested function boundaries (does not recurse into `TypedExpr::Function`).
fn first_mutable_global_in_body(
    expr: &TypedExpr,
    mutable_globals: &std::collections::HashMap<usize, String>,
) -> Option<String> {
    match expr {
        TypedExpr::LocalGet { slot, .. } => mutable_globals.get(slot).cloned(),
        TypedExpr::LocalSet { slot, value, .. } => {
            mutable_globals.get(slot).cloned()
                .or_else(|| first_mutable_global_in_body(value, mutable_globals))
        }
        TypedExpr::Function { .. } => None, // don't recurse into nested functions
        TypedExpr::BinaryOp { left, right, .. } => {
            first_mutable_global_in_body(left, mutable_globals)
                .or_else(|| first_mutable_global_in_body(right, mutable_globals))
        }
        TypedExpr::Call { func, args, .. } => {
            first_mutable_global_in_body(func, mutable_globals)
                .or_else(|| args.iter().find_map(|a| first_mutable_global_in_body(a, mutable_globals)))
        }
        TypedExpr::If { cond, then_br, else_br, .. } => {
            first_mutable_global_in_body(cond, mutable_globals)
                .or_else(|| first_mutable_global_in_body(then_br, mutable_globals))
                .or_else(|| first_mutable_global_in_body(else_br, mutable_globals))
        }
        TypedExpr::Block { stmts, expr, .. } => {
            stmts.iter().find_map(|s| match s {
                TypedStmt::Val { value, .. } | TypedStmt::Var { value, .. } => {
                    first_mutable_global_in_body(value, mutable_globals)
                }
                TypedStmt::Expr(e) => first_mutable_global_in_body(e, mutable_globals),
                _ => None,
            }).or_else(|| first_mutable_global_in_body(expr, mutable_globals))
        }
        TypedExpr::MakeObject { fields, spreads, .. } => {
            fields.iter().find_map(|(_, v)| first_mutable_global_in_body(v, mutable_globals))
                .or_else(|| spreads.iter().find_map(|s| first_mutable_global_in_body(s, mutable_globals)))
        }
        TypedExpr::MakeArray { elements, .. } => {
            elements.iter().find_map(|e| first_mutable_global_in_body(e, mutable_globals))
        }
        TypedExpr::Index { object, key, .. } => {
            first_mutable_global_in_body(object, mutable_globals)
                .or_else(|| first_mutable_global_in_body(key, mutable_globals))
        }
        TypedExpr::FieldGet { object, .. } => first_mutable_global_in_body(object, mutable_globals),
        TypedExpr::Match { scrutinee, arms, .. } => {
            first_mutable_global_in_body(scrutinee, mutable_globals)
                .or_else(|| arms.iter().find_map(|a| {
                    a.guard.as_ref().and_then(|g| first_mutable_global_in_body(g, mutable_globals))
                        .or_else(|| first_mutable_global_in_body(&a.body, mutable_globals))
                }))
        }
        TypedExpr::StringInterp { parts, .. } => {
            parts.iter().find_map(|p| match p {
                TypedStringPart::Expr(e) => first_mutable_global_in_body(e, mutable_globals),
                _ => None,
            })
        }
        _ => None,
    }
}

fn unify_types(types: &[Type]) -> Type {
    if types.is_empty() {
        return Type::Never;
    }
    if types.len() == 1 {
        return types[0].clone();
    }

    let first = &types[0];
    if types.iter().all(|t| t == first) {
        return first.clone();
    }

    // If all are numeric, widen
    if types.iter().all(|t| t.is_numeric()) {
        let mut result = types[0].clone();
        for t in &types[1..] {
            if let Some(widened) = widen_numeric(&result, t) {
                result = widened;
            }
        }
        return result;
    }

    Type::flatten_union(types.to_vec())
}
