use lin_common::{Diagnostic, Span};
use lin_parse::ast::{Expr, Module, Stmt};

use crate::compat::is_compatible_env;
use crate::env::TypeEnv;
use crate::resolve::resolve_type;
use crate::typed_ir::*;
use crate::types::Type;

mod call;
mod expr;
mod function;
mod helpers;
mod intrinsics;
mod ops;
mod pattern;
mod stmt;

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

impl Default for Checker {
    fn default() -> Self {
        Self::new()
    }
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

    pub(crate) fn types_compatible(&self, value: &Type, target: &Type) -> bool {
        is_compatible_env(value, target, Some(&self.env), &mut 0)
    }

    /// Collect all TypeVar IDs recursively from a type into `out`.
    fn collect_typevar_ids(ty: &Type, out: &mut std::collections::HashSet<u32>) {
        match ty {
            Type::TypeVar(id) => { out.insert(*id); }
            Type::Array(t) | Type::Iterator(t) | Type::Shared(t) => Self::collect_typevar_ids(t, out),
            Type::FixedArray(ts) => { for t in ts { Self::collect_typevar_ids(t, out); } }
            Type::Union(ts) => { for t in ts { Self::collect_typevar_ids(t, out); } }
            Type::Function { params, ret, .. } => {
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

    pub(crate) fn define_intrinsic(&mut self, name: &str, ty: Type) {
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
                        let required = params.iter().filter(|p| p.default.is_none()).count();
                        let fn_type = Type::Function {
                            params: param_types,
                            ret: Box::new(ret_type),
                            required,
                        };
                        let slot = self.env.define(name, fn_type, false);
                        self.forward_declared.insert(slot);
                    }
                }
            }
        }
    }
}
