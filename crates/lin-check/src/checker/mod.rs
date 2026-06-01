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
    /// Exported `type` decls visible from imports: (module_path, type_name) -> (params, body).
    /// An `import { Foo } from "m"` whose `Foo` matches an entry here registers it into the
    /// type env so `Foo` resolves in type annotations (the type-level analogue of `import_types`).
    pub import_type_decls: std::collections::HashMap<(String, String), (Vec<String>, Type)>,
    /// Global accumulator of TypeVar solutions discovered during inference.
    /// Populated by every call to collect_type_subs. Used by the zonking pass.
    solved_type_vars: std::collections::HashMap<u32, Type>,
    /// TypeVar IDs from imported module signatures. These are generic "any" slots
    /// that must never be solved to a concrete type in this module's zonking pass.
    protected_type_vars: std::collections::HashSet<u32>,
    /// Slots of mutable global (`var`) bindings. Used by the async var-capture check.
    mutable_global_slots: std::collections::HashMap<usize, String>,
    /// Affine resource tracking (streams brief §7): slots of `Stream`-typed `val` bindings that
    /// have already been CONSUMED (read as a value). A `Stream` is use-at-most-once; reading the
    /// same binding twice is a use-after-move ERROR. Flow-sensitive: if/match snapshot this set
    /// per branch and merge conservatively (a binding consumed in ANY branch is consumed after).
    /// Dropping (never consuming) is FINE — the RC finalizer closes the fd; only double-use errors.
    consumed_streams: std::collections::HashSet<usize>,
    /// When true, a `Json` value is permitted to flow into a fully-concrete target without
    /// an explicit `fromJson` decode (ADR-046). Set only for the trusted stdlib, whose
    /// wrappers forward `Json` handles into concrete intrinsic/foreign params by design.
    /// User modules check with `false`, so `val p: Person = readJson(...)` is a type error.
    pub lenient_json: bool,
    /// Phase 0 monomorphized generics: maps a generic function's binding name to the
    /// (type-param name → quantified TypeVar id) assignment chosen during forward declaration.
    /// `infer_function` reuses the SAME ids so the forward-declared signature (used by call-site
    /// inference) and the body's parameter types agree. The ids live in the ≥9000 range so they
    /// behave like intrinsic generic slots: never globally solved, instantiated per call site via
    /// a local subs map (`collect_and_save_subs` skips ≥9000).
    generic_fn_params: std::collections::HashMap<String, Vec<(String, u32)>>,
    /// Next free quantified-generic TypeVar id (≥9000, above the intrinsic slot 9000).
    next_generic_tv: u32,
    /// Phase 4.5b: element-type hint for an INTERMEDIATE `val <name> = lin_array_allocate(..)`
    /// binding inside a combinator whose declared return is `Array(elem)`. When the active value
    /// binding's name matches `.0` and its RHS is a fresh `lin_array_allocate` call, `check_stmt`
    /// pins the binding's element type to `.1` (the declared-return element), so monomorphization
    /// turns `Array(U)` into a concrete `Array(Int32)` and codegen emits a flat allocation that
    /// matches the flat reader. Set/cleared around the body in `infer_function`; gated to the
    /// allocation intrinsic so no other binding's representation changes. See ADR for rationale.
    array_alloc_elem_hint: Option<(String, Type)>,
    /// Origin of each value-import binding, keyed by the LOCAL name it is bound under (honouring
    /// `as` aliases). `(module_path, export_name)`. Used by `replace <name> = ...` (ADR-071) to
    /// resolve which imported export a mock targets, so lowering can override its canonical symbol.
    import_origins: std::collections::HashMap<String, (String, String)>,
    /// Collected `replace` overrides (ADR-071), threaded into `TypedModule::replacements`.
    replacements: Vec<crate::typed_ir::Replacement>,
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
            import_type_decls: std::collections::HashMap::new(),
            solved_type_vars: std::collections::HashMap::new(),
            protected_type_vars: std::collections::HashSet::new(),
            mutable_global_slots: std::collections::HashMap::new(),
            consumed_streams: std::collections::HashSet::new(),
            lenient_json: false,
            generic_fn_params: std::collections::HashMap::new(),
            // Start above the intrinsic generic slot (9000) so quantified generics never
            // collide with `lin_map`/`lin_iter` et al.
            next_generic_tv: 9001,
            array_alloc_elem_hint: None,
            import_origins: std::collections::HashMap::new(),
            replacements: Vec::new(),
        }
    }

    pub fn check_module(&mut self, module: &Module) -> Result<TypedModule, Vec<Diagnostic>> {
        self.register_intrinsics();

        // Pre-scan: register any imported `type` decls into the type env, so that a name
        // brought in by `import { Foo } from "m"` resolves in type annotations below. Must
        // run before forward_declare_* (whose signatures may annotate with imported types).
        self.register_imported_types(module);

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
            // Collect exported `type` decls as module metadata so dependents can use them in
            // type position. Resolve each from the env (forward-declared + body-resolved by now);
            // self-referential/recursive types keep their `Named(name)` cycle points.
            let mut exported_types = std::collections::HashMap::new();
            for stmt in &module.statements {
                if let lin_parse::ast::Stmt::TypeDecl { name, exported: true, .. } = stmt {
                    if let Some(decl) = self.env.lookup_type(name) {
                        exported_types.insert(name.clone(), (decl.params.clone(), decl.body.clone()));
                    }
                }
            }
            let mut typed_module = TypedModule {
                statements: stmts,
                span: module.span,
                intrinsics: self.intrinsic_slots.clone(),
                exported_types,
                replacements: self.replacements.clone(),
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
        is_compatible_env(value, target, Some(&self.env), self.lenient_json, &mut 0)
    }

    /// Argument compatibility for a (non-dot) function call. Identical to `types_compatible`
    /// except it additionally accepts an `Iterator<T>` argument where an `Array<U>` parameter
    /// is expected (elements compatible). The standard iterator functions (`map`/`filter`/
    /// `reduce`/…) are specified to accept "an array or an `Iterator<T>`" (spec §17.6), and the
    /// dot form `it.map(f)` already does — its arg-binding path never runs the strict array
    /// check. This makes the equivalent free-call form `map(it, f)` accept the same arguments,
    /// so `f(x, y)` and `x.f(y)` agree (the lowering handles either iterable identically). The
    /// leniency is confined to call arguments — plain assignment (`val a: T[] = someIterator`)
    /// still rejects, since an iterator is not indexable.
    pub(crate) fn arg_compatible(&self, value: &Type, target: &Type) -> bool {
        if let (Type::Iterator(v_elem), Type::Array(t_elem)) = (value, target) {
            if self.types_compatible(v_elem, t_elem) {
                return true;
            }
        }
        self.types_compatible(value, target)
    }

    /// Collect all TypeVar IDs recursively from a type into `out`.
    fn collect_typevar_ids(ty: &Type, out: &mut std::collections::HashSet<u32>) {
        match ty {
            Type::TypeVar(id) => { out.insert(*id); }
            Type::Array(t) | Type::Iterator(t) | Type::Shared(t) | Type::Stream(t) => Self::collect_typevar_ids(t, out),
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

    /// Register imported `type` decls into the type env. For each `import { Name } from "m"`
    /// binding whose `(m, Name)` is a known exported type decl, define it under its local name
    /// (honouring `as` aliases) so that `Name` resolves when used in a type annotation. Value
    /// imports are unaffected — a name can be both (rare); both registrations are harmless.
    fn register_imported_types(&mut self, module: &Module) {
        for stmt in &module.statements {
            if let Stmt::Import { bindings, path, .. } = stmt {
                for binding in bindings {
                    if let Some((params, body)) =
                        self.import_type_decls.get(&(path.clone(), binding.name.clone())).cloned()
                    {
                        let local_name = binding.alias.as_ref().unwrap_or(&binding.name);
                        self.env.define_type(local_name.clone(), params, body);
                    }
                }
            }
        }
    }

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

    /// Forward-declare top-level `val name = (...) => ...` functions so that
    /// they can call each other (mutual recursion, ADR-015 equivalent).
    fn forward_declare_functions(&mut self, module: &Module) {
        for stmt in &module.statements {
            if let Stmt::Val { pattern, value, .. } = stmt {
                if let Expr::Function { type_params, params, return_type, .. } = value {
                    let name = match pattern {
                        lin_parse::ast::Pattern::Ident(n, _) => Some(n.clone()),
                        _ => None,
                    };
                    if let Some(name) = name {
                        // Generic function: allocate one quantified TypeVar (≥9000) per type
                        // param and resolve the signature in a scratch env binding each param to
                        // that TypeVar. The SAME id assignment is recorded in `generic_fn_params`
                        // and reused by `infer_function`, so the forward-declared signature (driving
                        // call-site inference) and the lowered body's param types are consistent.
                        let (env_for_resolve, param_assign) = if type_params.is_empty() {
                            (self.env.clone(), Vec::new())
                        } else {
                            let mut assign = Vec::new();
                            let mut scratch = self.env.clone();
                            for tp in type_params {
                                let id = self.next_generic_tv;
                                self.next_generic_tv += 1;
                                assign.push((tp.clone(), id));
                                scratch.define_type(tp.clone(), Vec::new(), Type::TypeVar(id));
                            }
                            (scratch, assign)
                        };
                        if !param_assign.is_empty() {
                            self.generic_fn_params.insert(name.clone(), param_assign);
                        }

                        let param_types: Vec<Type> = params
                            .iter()
                            .map(|p| {
                                p.type_ann
                                    .as_ref()
                                    .and_then(|t| resolve_type(t, &env_for_resolve).ok())
                                    .unwrap_or(Type::TypeVar(self.env.next_slot() as u32))
                            })
                            .collect();
                        let ret_type = return_type
                            .as_ref()
                            .and_then(|t| resolve_type(t, &env_for_resolve).ok())
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
