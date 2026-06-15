use lin_common::{Diagnostic, Span};
use lin_parse::ast::{Expr, Module, Stmt};

use crate::compat::is_compatible_env;
use crate::env::TypeEnv;
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
    /// Stdlib export index: export-name -> list of stdlib module paths that export it. Used to
    /// suggest the RIGHT module when an `import { x } from "m"` names an `x` that `m` doesn't
    /// export but some other stdlib module does (e.g. `gunzip` lives in `std/compress`).
    pub stdlib_export_index: std::collections::HashMap<String, Vec<String>>,
    /// Exported `type` decls visible from imports: (module_path, type_name) -> (params, body).
    /// An `import { Foo } from "m"` whose `Foo` matches an entry here registers it into the
    /// type env so `Foo` resolves in type annotations (the type-level analogue of `import_types`).
    pub import_type_decls: std::collections::HashMap<(String, String), (Vec<String>, Type)>,
    /// Import paths whose FULL signature (value + type exports) is loaded — i.e. resolved into a
    /// complete `TypedModule`. An `import { x } from "p"` naming an `x` that path `p` does not
    /// export is rejected ONLY when `p` is in this set, so partial-knowledge contexts (SCC Phase-2
    /// value-only seeding, isolated checks) never produce a false "no export".
    pub fully_resolved_import_paths: std::collections::HashSet<String>,
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
    /// an explicit `fromJson` decode (ADR-045). Set only for the trusted stdlib, whose
    /// wrappers forward `Json` handles into concrete intrinsic/foreign params by design.
    /// User modules check with `false`, so `val p: Person = readJson(...)` is a type error.
    pub lenient_json: bool,
    /// When true, `lin_*` compiler intrinsics may be referenced by name (as call targets or
    /// values). True for trusted stdlib modules (which re-export them under clean names) and when
    /// the `LIN_ALLOW_INTRINSICS` test escape hatch is set; false for user code, which must use the
    /// stdlib wrappers (ADR-002/ADR-008, ADR-060). Set by the compile pipeline from `is_stdlib`.
    pub allow_intrinsics: bool,
    /// Phase 0 monomorphized generics: maps a generic function's binding name to the
    /// (type-param name → quantified TypeVar id) assignment chosen during forward declaration.
    /// `infer_function` reuses the SAME ids so the forward-declared signature (used by call-site
    /// inference) and the body's parameter types agree. The ids live in the ≥9000 range so they
    /// behave like intrinsic generic slots: never globally solved, instantiated per call site via
    /// a local subs map (`collect_and_save_subs` skips ≥9000).
    generic_fn_params: std::collections::HashMap<String, Vec<(String, u32)>>,
    /// Next free quantified-generic TypeVar id (≥9000, above the intrinsic slot 9000).
    next_generic_tv: u32,
    /// Quantified-generic TypeVar ids carrying a NUMERIC bound (ADR-014, reversed). A `Number`
    /// parameter annotation resolves to a FRESH constrained generic TypeVar in this set: arithmetic
    /// is permitted on it (the bound guarantees a numeric family), and at each call site the binding
    /// concrete type must satisfy the bound (a `String`/`Bool`/… argument is rejected). Each `Number`
    /// occurrence mints its own id, so `(a: Number, b: Number)` lets `a`/`b` specialize independently.
    /// Monomorphization (`lin-ir`) then produces a native unboxed copy per concrete family — zero
    /// runtime cost, matching `Int32`. See `resolve_param_type` and the call-site bound check.
    pub(crate) numeric_tvs: std::collections::HashSet<u32>,
    /// Phase 4.5b: element-type hint for an INTERMEDIATE `val <name> = lin_array_allocate(..)`
    /// binding inside a combinator whose declared return is `Array(elem)`. When the active value
    /// binding's name matches `.0` and its RHS is a fresh `lin_array_allocate` call, `check_stmt`
    /// pins the binding's element type to `.1` (the declared-return element), so monomorphization
    /// turns `Array(U)` into a concrete `Array(Int32)` and codegen emits a flat allocation that
    /// matches the flat reader. Set/cleared around the body in `infer_function`; gated to the
    /// allocation intrinsic so no other binding's representation changes. See ADR for rationale.
    array_alloc_elem_hint: Option<(String, Type)>,
    /// Origin of each value-import binding, keyed by the LOCAL name it is bound under (honouring
    /// `as` aliases). `(module_path, export_name)`. Used by `replace <name> = ...` (ADR-046) to
    /// resolve which imported export a mock targets, so lowering can override its canonical symbol.
    import_origins: std::collections::HashMap<String, (String, String)>,
    /// Collected `replace` overrides (ADR-046), threaded into `TypedModule::replacements`.
    replacements: Vec<crate::typed_ir::Replacement>,
    /// Definition-site (`name_span`, inferred `Type`) entry for EVERY function/lambda parameter
    /// with a name span — recorded when the param is typed in `infer_function`(`_with_hints`),
    /// whether or not the param is ever used. At the end of `check_module` each `Type` is zonked
    /// (so a parameter whose type was inferred from the call context resolves to the concrete
    /// type, not an unsolved `?T`) and appended to `span_type_map` as a self-entry
    /// (`use_span == def_span == name_span`). This makes a parameter's inferred type recoverable
    /// directly at its name span for LSP inlay hints, including UNUSED unannotated params (which
    /// `infer_ident` never records, since only USES populate `span_type_map`). Metadata-only.
    param_def_span_types: Vec<(Span, Type)>,
    /// Path-11 lambda-set inference: monotonic id generator for syntactic lambdas. Each
    /// `TypedExpr::Function` the checker builds is stamped with a fresh id (≥1; 0 means
    /// "unassigned"), so its function type can carry `LambdaSet::singleton(id)`. Inert metadata —
    /// used only by the shadow-mode classification/statistics pass (`crate::lambda_set_stats`).
    next_lambda_id: u32,
    /// Active flow-narrowings for INDEX PLACES (`obj[key]`) derived from an `if`/`else`
    /// null-test condition (`if m[k] != null then m[k] else …`). `infer_index` consults this
    /// to tighten a matching read's static type (drop `Null`) while the relevant branch is being
    /// checked. Pushed before a branch (`apply_null_narrowing`) and truncated back after it
    /// (`infer_if`). Sound because re-reading `obj[key]` with `obj`/`key` unchanged yields the
    /// same value — the narrowing only tightens the type, never widens. Any assignment to `obj`
    /// (or `key`) within the branch invalidates the matching entries (`clear_index_narrowings_for`).
    pub(crate) index_narrowings: Vec<crate::checker::expr::IndexNarrow>,
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
            stdlib_export_index: std::collections::HashMap::new(),
            import_type_decls: std::collections::HashMap::new(),
            fully_resolved_import_paths: std::collections::HashSet::new(),
            solved_type_vars: std::collections::HashMap::new(),
            protected_type_vars: std::collections::HashSet::new(),
            mutable_global_slots: std::collections::HashMap::new(),
            consumed_streams: std::collections::HashSet::new(),
            lenient_json: false,
            allow_intrinsics: false,
            generic_fn_params: std::collections::HashMap::new(),
            // Start above the intrinsic generic slot (9000) so quantified generics never
            // collide with `lin_map`/`lin_iter` et al.
            next_generic_tv: 9001,
            numeric_tvs: std::collections::HashSet::new(),
            array_alloc_elem_hint: None,
            import_origins: std::collections::HashMap::new(),
            replacements: Vec::new(),
            param_def_span_types: Vec::new(),
            next_lambda_id: 1,
            index_narrowings: Vec::new(),
        }
    }

    /// Mint a fresh syntactic-lambda identity (Path-11 lambda sets). Ids start at 1; 0 is reserved
    /// for "unassigned" (cache-default / synthesized functions), which `ty()` maps to `Top`.
    pub(crate) fn next_lambda_id(&mut self) -> u32 {
        let id = self.next_lambda_id;
        self.next_lambda_id += 1;
        id
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
        // function literal so mutual recursion works (mirrors ADR-012).
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
                spec_origins: std::collections::HashMap::new(),
            };
            // Zonking pass: replace solved TypeVar nodes with their concrete types.
            let subs = self.solved_type_vars.clone();
            crate::zonk::zonk_module(&mut typed_module, &subs);
            // Append a definition-site entry to `span_type_map` for every function/lambda
            // parameter, using the FINAL zonked type (so a parameter whose type was inferred from
            // its call context resolves to the concrete type, not an unsolved `?T`). The use-span
            // IS the def-span (the param's name span), making the parameter's inferred type
            // recoverable directly at its name span for LSP inlay hints — including UNUSED
            // unannotated params, which `infer_ident` never records (only USES populate the map).
            // Purely additive metadata: it never affects `typed_module`, inferred types, or
            // diagnostics. See `param_def_span_types`.
            for (name_span, ty) in std::mem::take(&mut self.param_def_span_types) {
                let resolved = crate::zonk::zonk_type(&ty, &subs);
                self.span_type_map.push((name_span, resolved.to_string(), Some(name_span)));
            }
            Ok(typed_module)
        }
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Snapshot the transient nesting state that a nested `infer_function` may grow. Paired with
    /// `restore_checker_state` to roll back a DISCARDED speculative type-check (a callback checked
    /// against an incomplete hint that fails and is re-inferred hint-free). A failed `infer_function`
    /// can `?`-out between its pushes and matching pops, leaking an unbalanced frame; restoring the
    /// lengths prevents an unrelated enclosing function from later popping it and inheriting a
    /// phantom capture set (which would give a top-level function a spurious closure env and break
    /// its call ABI). Restore only ever TRUNCATES (never grows) — a clean attempt leaves the lengths
    /// unchanged, so this is a no-op on the success path.
    pub(crate) fn checker_state_snapshot(&self) -> (usize, usize, usize, Option<String>, bool, usize) {
        (
            self.function_scope_depths.len(),
            self.capture_stack.len(),
            self.env.scope_depth(),
            self.current_function.clone(),
            self.in_tail_position,
            // Drop any param def-site entries pushed by the discarded speculative attempt, so a
            // failed hint can't leave a wrongly-typed parameter entry in `span_type_map` — only
            // the kept attempt's params are recorded.
            self.param_def_span_types.len(),
        )
    }

    pub(crate) fn restore_checker_state(&mut self, snap: (usize, usize, usize, Option<String>, bool, usize)) {
        let (fsd_len, cap_len, scope_len, cur_fn, tail, param_types_len) = snap;
        self.function_scope_depths.truncate(fsd_len);
        self.capture_stack.truncate(cap_len);
        self.env.truncate_scopes(scope_len);
        self.current_function = cur_fn;
        self.in_tail_position = tail;
        self.param_def_span_types.truncate(param_types_len);
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
            Type::Array(t) | Type::Iterator(t) | Type::Shared(t) | Type::Stream(t) | Type::Promise(t) => Self::collect_typevar_ids(t, out),
            Type::FixedArray(ts) => { for t in ts { Self::collect_typevar_ids(t, out); } }
            Type::Union(ts) => { for t in ts { Self::collect_typevar_ids(t, out); } }
            Type::Function { params, ret, .. } => {
                for p in params { Self::collect_typevar_ids(p, out); }
                Self::collect_typevar_ids(ret, out);
            }
            Type::Object { fields, .. } => { for v in fields.values() { Self::collect_typevar_ids(v, out); } }
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

    /// Forward-declare `val name = (...) => ...` functions in `stmts` so that
    /// they can call each other (mutual recursion, ADR-012 equivalent).
    /// Called for module-level statements and for every block body, enabling
    /// hoisting of inner function literals within a closure or function body.
    pub(crate) fn forward_declare_functions_in(&mut self, stmts: &[lin_parse::ast::Stmt]) {
        for stmt in stmts {
            if let Stmt::Val { pattern, value, .. } = stmt {
                if let Expr::Function { type_params, params, return_type, .. } = value {
                    let name = match pattern {
                        lin_parse::ast::Pattern::Ident(n, _) => Some(n.clone()),
                        _ => None,
                    };
                    if let Some(name) = name {
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
                        let mut param_types: Vec<Type> = Vec::with_capacity(params.len());
                        for p in params {
                            let ty = match &p.type_ann {
                                Some(t) => self
                                    .resolve_type_with_number_in(t, &env_for_resolve)
                                    .unwrap_or(Type::TypeVar(self.env.next_slot() as u32)),
                                None => Type::TypeVar(self.env.next_slot() as u32),
                            };
                            param_types.push(ty);
                        }
                        let ret_type = match return_type {
                            Some(t) if !Self::is_bare_number(t) => self
                                .resolve_type_with_number_in(t, &env_for_resolve)
                                .unwrap_or(Type::TypeVar(self.env.next_slot() as u32)),
                            _ => Type::TypeVar(self.env.next_slot() as u32),
                        };
                        let required = params.iter().filter(|p| p.default.is_none()).count();
                        let fn_type = Type::Function {
                            params: param_types,
                            ret: Box::new(ret_type),
                            required,
                            lset: crate::types::LambdaSet::Top,
                        };
                        let slot = self.env.define(name, fn_type, false);
                        self.forward_declared.insert(slot);
                    }
                }
            }
        }
    }

    /// Forward-declare top-level `val name = (...) => ...` functions so that
    /// they can call each other (mutual recursion, ADR-012 equivalent).
    fn forward_declare_functions(&mut self, module: &Module) {
        self.forward_declare_functions_in(&module.statements);
    }
}
