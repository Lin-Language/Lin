use lin_common::{Diagnostic, Span};
use lin_parse::ast::{Expr, Module, Stmt};

use crate::compat::is_compatible_env;
use crate::env::TypeEnv;
use crate::resolve::resolve_type_spanned;
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
    /// Imported function overload sets (ADR-074 cross-module): (module_path, name) → all members
    /// as (function type, mangled emitted symbol). Seeded from `ModuleSignature.overloads`. When an
    /// imported name appears here it is registered as an overload set in the env, so the ordinary
    /// call-site overload resolution applies and each member lowers to its own `Named` target.
    pub import_overloads: std::collections::HashMap<(String, String), Vec<(Type, String)>>,
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
    /// Definition-site (`name_span`, inferred `Type`) entry for every simple `val`/`var` binding.
    /// Pushed in `check_stmt` once the binding's final type is known (after `typed_value.ty()`
    /// and any annotation reconciliation). At end of `check_module` each `Type` is zonked and
    /// appended to `span_type_map` as a self-entry (`use_span == def_span == name_span`), so
    /// the LSP can colour the binding NAME correctly (e.g. `function` when the RHS is a lambda).
    /// Only simple `Pattern::Ident` bindings emit an entry; destructuring patterns are skipped.
    /// Purely additive metadata: does not affect `typed_module`, inferred types, or diagnostics.
    binding_def_span_types: Vec<(Span, Type)>,
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
    /// Raw AST type bodies for every top-level `type` declaration in the current module,
    /// keyed by name. Populated during `forward_declare_types` so error messages can show
    /// the source-level shape of a Named type even before `check_stmt` has resolved it.
    pub(crate) raw_type_decls: std::collections::HashMap<String, lin_parse::ast::TypeExpr>,
    /// Expected RESULT type for the IMMEDIATELY-NEXT generic call/dot-call inference (ADR-085).
    /// Set by `check_expr` just before it falls through to `infer_expr` for a `Call`/`DotCall`
    /// whose context supplies an expected type; CONSUMED (taken) at the top of `infer_call`/
    /// `infer_dot_call` so it never leaks into nested argument inference. Drives expected-result-
    /// type-directed generic inference: the call unifies this against the function's declared
    /// return to pre-seed its type-parameter substitutions (and, for a dot-call, derives an
    /// expected type for the receiver call, propagating through method chains). When `None` (or it
    /// fails to determine a param), inference falls back to today's bottom-up, argument-driven path.
    expected_call_result: Option<Type>,
    /// Spans already reported as shadowing errors (keyed by `(file_id, start, end)`).
    /// Prevents duplicate diagnostics when a speculative type-check re-visits a binding site.
    reported_shadow_spans: std::collections::HashSet<(u32, u32, u32)>,
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
            import_overloads: std::collections::HashMap::new(),
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
            binding_def_span_types: Vec::new(),
            next_lambda_id: 1,
            index_narrowings: Vec::new(),
            raw_type_decls: std::collections::HashMap::new(),
            expected_call_result: None,
            reported_shadow_spans: std::collections::HashSet::new(),
        }
    }

    /// Emit a hard-error diagnostic when `name` at `span` shadows a binding in an ENCLOSING scope.
    ///
    /// Rules:
    /// - Same-scope redefinition (overloads, forward-declared functions, destructuring temps) is NOT
    ///   flagged — only inner-shadows-outer.
    /// - Synthetic compiler-internal names are skipped: `_`, `__destr_*`, `__param_*`, `$*`,
    ///   `lin_*`, and the empty string.
    /// - Duplicate reporting at the same span (from speculative re-checks) is suppressed.
    pub(crate) fn check_shadowing(&mut self, name: &str, span: Span) {
        // Skip synthetic / wildcard names.
        if name.is_empty()
            || name == "_"
            || name.starts_with("__destr_")
            || name.starts_with("__param_")
            || name.starts_with('$')
            || name.starts_with("lin_")
        {
            return;
        }

        // Dedup: don't re-report the same physical binding site.
        let key = (span.file_id, span.start, span.end);
        if self.reported_shadow_spans.contains(&key) {
            return;
        }

        let current_depth = self.env.scope_depth(); // number of scopes = len
        let current_innermost = current_depth.saturating_sub(1); // 0-based index of innermost

        if let Some((def_depth, info)) = self.env.lookup_with_depth(name) {
            // Only flag if it lives in a STRICTLY outer scope (not the current innermost scope).
            if def_depth < current_innermost {
                // If it's a forward-declared (pending) binding, don't flag — the val is completing
                // its own pre-scan slot in the same block.
                if !self.forward_declared.contains(&info.slot) {
                    self.reported_shadow_spans.insert(key);
                    let mut diag = Diagnostic::error(
                        span,
                        format!("`{}` shadows a binding from an enclosing scope", name),
                    )
                    .with_help(format!(
                        "rename this binding; Lin does not allow an inner scope to reuse the outer name `{}`",
                        name
                    ));
                    if let Some(prev_span) = info.def_span {
                        diag = diag.with_note(prev_span, format!("`{}` is already bound here", name));
                    }
                    self.diagnostics.push(diag);
                }
            }
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
        // Hoist all type declarations to the front so that types defined anywhere in the
        // module are resolved before any function body is checked. This mirrors how
        // forward_declare_functions hoists val bindings: a type used before its textual
        // declaration should not be an error. TypeDecl statements produce no runtime code,
        // so reordering them is always safe.
        let (type_decls, other_stmts): (Vec<_>, Vec<_>) = module
            .statements
            .iter()
            .partition(|s| matches!(s, Stmt::TypeDecl { .. }));
        // Module-level transient state to restore after each statement. A statement whose check
        // fails (e.g. a function body that `?`-returned mid-check) can leak the scopes/frames it
        // pushed before erroring; left in place, that leak poisons later statements — in particular
        // the shadowing check would see a failed sibling function's parameters as an enclosing
        // scope and report a spurious shadow. Restoring (truncate-only, never grows) is a no-op on
        // the success path and does NOT touch `self.diagnostics`, so the real error is preserved.
        let module_scope_depth = self.env.scope_depth();
        let module_fsd = self.function_scope_depths.len();
        let module_cap = self.capture_stack.len();
        for stmt in type_decls.into_iter().chain(other_stmts) {
            match self.check_stmt(stmt) {
                Ok(typed_stmt) => stmts.push(typed_stmt),
                Err(diag) => self.diagnostics.push(diag),
            }
            self.env.truncate_scopes(module_scope_depth);
            self.function_scope_depths.truncate(module_fsd);
            self.capture_stack.truncate(module_cap);
        }

        if self.diagnostics.iter().any(|d| d.severity == lin_common::Severity::Error) {
            Err(self.diagnostics.clone())
        } else {
            // Collect exported `type` decls as module metadata so dependents can use them in
            // type position. Resolve each from the env (forward-declared + body-resolved by now);
            // self-referential/recursive types keep their `Named(name)` cycle points.
            let mut exported_types = std::collections::HashMap::new();
            for stmt in &module.statements {
                if let lin_parse::ast::Stmt::TypeDecl { name, params, body, exported: true, .. } = stmt {
                    // Re-resolve the body against the now-complete env. The first resolution pass
                    // runs in (hoisted) source order, so an exported type that references a sibling
                    // declared LATER in the file collapsed that reference to a bare `Named(...)` via
                    // the cycle guard (the sibling was still a placeholder at the time). Left
                    // unexpanded, that forward reference leaks into this module's signature and then
                    // fails to resolve in a consumer that imports the alias but not the sibling
                    // (e.g. `import { TimetableLeg }` where `TimetableLeg` has a `Trip` field but
                    // `Trip` is never imported). Re-resolving now expands all such forward references
                    // inline; genuine recursive cycles still terminate at the cycle guard and keep
                    // their `Named(self)` (which the consumer can resolve, since it imports the alias).
                    let resolved = self
                        .resolve_type_decl_body(params, body)
                        .ok()
                        .or_else(|| self.env.lookup_type(name).map(|d| d.body.clone()));
                    if let Some(resolved) = resolved {
                        exported_types.insert(name.clone(), (params.clone(), resolved));
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
            // Append definition-site entries for simple val/var bindings so the LSP can colour
            // the binding name correctly (e.g. `function` when the RHS is a lambda).
            for (name_span, ty) in std::mem::take(&mut self.binding_def_span_types) {
                let resolved = crate::zonk::zonk_type(&ty, &subs);
                self.span_type_map.push((name_span, resolved.to_string(), Some(name_span)));
            }
            Ok(typed_module)
        }
    }

    /// Resolve this module's EXPORTED `type` aliases against the current import-type seeding, WITHOUT
    /// type-checking any value/function bodies. Used by the cyclic-SCC fixpoint in `lin-compile`
    /// (ADR-084) to harvest a member's type aliases even when its bodies don't yet type-check — e.g.
    /// a param annotated with a peer alias that is still a placeholder TypeVar produces a spurious
    /// body error in the first sweep, which `check_module` would surface as `Err`, dropping the
    /// (perfectly resolvable) alias map. Mirrors `check_module`'s type-namespace prescans
    /// (`register_imported_types` + `forward_declare_types`) and its export re-resolution, so a
    /// self-contained alias (`type ST = { String: UInt32 }`) resolves fully and an alias that
    /// references a peer's alias resolves once that peer's decl has been seeded (driving the fixpoint).
    pub fn collect_exported_type_decls(
        &mut self,
        module: &Module,
    ) -> std::collections::HashMap<String, (Vec<String>, Type)> {
        self.register_intrinsics();
        self.register_imported_types(module);
        self.forward_declare_types(module);
        // Resolve every top-level type decl into the env (in hoisted order, like `check_module`), so
        // sibling references within the module resolve. Resolution errors are swallowed — this is a
        // best-effort harvest; a genuinely-undefined alias simply won't appear in the result.
        for stmt in &module.statements {
            if let Stmt::TypeDecl { name, params, body, .. } = stmt {
                if let Ok(resolved) = self.resolve_type_decl_body(params, body) {
                    self.env.define_type(name.clone(), params.clone(), resolved);
                }
            }
        }
        // Re-resolve exported aliases against the now-complete env (expands forward references to
        // later-declared siblings inline, matching `check_module`'s export-collection pass).
        let mut exported_types = std::collections::HashMap::new();
        for stmt in &module.statements {
            if let Stmt::TypeDecl { name, params, body, exported: true, .. } = stmt {
                let resolved = self
                    .resolve_type_decl_body(params, body)
                    .ok()
                    .or_else(|| self.env.lookup_type(name).map(|d| d.body.clone()));
                if let Some(resolved) = resolved {
                    exported_types.insert(name.clone(), (params.clone(), resolved));
                }
            }
        }
        exported_types
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
    pub(crate) fn checker_state_snapshot(&self) -> (usize, usize, usize, Option<String>, bool, usize, usize) {
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
            // Drop any shadow-error diagnostics pushed during a discarded speculative check.
            // The `reported_shadow_spans` dedup set is intentionally NOT rolled back — keeping
            // a site marked as "reported" prevents a re-checked site from emitting the diagnostic
            // a second time on the committed path, but rolling it back would allow double-reporting.
            self.diagnostics.len(),
        )
    }

    pub(crate) fn restore_checker_state(&mut self, snap: (usize, usize, usize, Option<String>, bool, usize, usize)) {
        let (fsd_len, cap_len, scope_len, cur_fn, tail, param_types_len, diag_len) = snap;
        self.function_scope_depths.truncate(fsd_len);
        self.capture_stack.truncate(cap_len);
        self.env.truncate_scopes(scope_len);
        self.current_function = cur_fn;
        self.in_tail_position = tail;
        self.param_def_span_types.truncate(param_types_len);
        self.diagnostics.truncate(diag_len);
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

    /// Render a drill-down breakdown of why `value` is not assignable to `target`, as an indented
    /// tree to append to a mismatch diagnostic message. Returns None when there is no useful
    /// structural breakdown (caller appends nothing).
    pub(crate) fn explain_mismatch(&self, value: &Type, target: &Type) -> Option<String> {
        let reasons = crate::compat::explain_incompatibility(
            value,
            target,
            Some(&self.env),
            self.lenient_json,
        );
        if reasons.is_empty() {
            return None;
        }
        let mut out = String::new();
        for (depth, r) in reasons.iter().enumerate() {
            out.push('\n');
            for _ in 0..(depth + 1) {
                out.push_str("  ");
            }
            out.push_str("\u{21b3} ");
            out.push_str(r);
        }
        Some(out)
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
                    let key = (path.clone(), binding.name.clone());
                    if let Some((params, body)) = self.import_type_decls.get(&key).cloned() {
                        let local_name = binding.alias.as_ref().unwrap_or(&binding.name);
                        self.env.define_type(local_name.clone(), params, body);
                    } else if !self.fully_resolved_import_paths.contains(path) {
                        // Cyclic-import Phase 1 (ADR-083): the peer module that exports this name is
                        // still mid-resolution, so its `type` decls are not seeded yet. If this name
                        // turns out to be an imported TYPE alias, leaving it undefined makes any
                        // type-position use ("(t: T)") fail with "Unknown type 'T'", which aborts
                        // Phase 1 before we can extract the peer's signature. Register a permissive
                        // placeholder (a fresh TypeVar) so the body still checks; Phase 2 re-runs with
                        // the peer's real `type` decl seeded into `import_type_decls` (the branch
                        // above), which overrides this placeholder. A genuinely-undefined type from a
                        // FULLY-RESOLVED module is excluded by the guard and still errors as before.
                        // Registering a phantom alias for a name that is actually a VALUE import is
                        // harmless: it lives in the type namespace and is never consulted unless the
                        // name is used in type position.
                        let local_name = binding.alias.as_ref().unwrap_or(&binding.name).clone();
                        let placeholder = self.env.fresh_type_var();
                        self.env.define_type(local_name, Vec::new(), placeholder);
                    }
                }
            }
        }
    }

    /// Resolve a single type-declaration body against the current type env. For a generic alias
    /// (`type Box<T> = …`) each declared param is bound into a scratch env as a self-referential
    /// `Named(param)` so it survives resolution (and `substitute` replaces it at each use-site).
    /// Shared by the in-order `check_stmt` resolution pass and the export-collection re-resolution
    /// (`check_module`) that repairs forward references to later-declared sibling types.
    fn resolve_type_decl_body(
        &self,
        params: &[String],
        body: &lin_parse::ast::TypeExpr,
    ) -> Result<Type, Diagnostic> {
        let map_resolve_err = |(s, e, help): (Span, String, Option<String>)| {
            let d = Diagnostic::error(s, e);
            if let Some(h) = help { d.with_help(h) } else { d }
        };
        if params.is_empty() {
            resolve_type_spanned(body, &self.env).map_err(map_resolve_err)
        } else {
            let mut scratch = self.env.clone();
            for param in params {
                scratch.define_type(param.clone(), Vec::new(), Type::Named(param.clone()));
            }
            resolve_type_spanned(body, &scratch).map_err(map_resolve_err)
        }
    }

    /// Pre-register all top-level type aliases as Named(name) placeholders.
    /// This allows recursive types to be resolved: when `type Tree = { ..., children: Tree[] }`
    /// is resolved, the occurrence of `Tree` in the body will be already in the env.
    fn forward_declare_types(&mut self, module: &Module) {
        for stmt in &module.statements {
            if let Stmt::TypeDecl { name, params, body, .. } = stmt {
                // Register a placeholder body of Named(name) for now; the real body
                // will be resolved when check_stmt processes TypeDecl.
                // Using Named(name) as the placeholder means self-references in the body
                // will be detected by the cycle guard in resolve.rs and left as Named(name).
                self.env.define_type(
                    name.clone(),
                    params.clone(),
                    Type::Named(name.clone()),
                );
                // Store the raw AST body so error messages can show the source-level shape
                // of this type before check_stmt has had a chance to resolve it.
                self.raw_type_decls.insert(name.clone(), body.clone());
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
                    let name_and_span = match pattern {
                        lin_parse::ast::Pattern::Ident(n, sp) => Some((n.clone(), *sp)),
                        _ => None,
                    };
                    if let Some((name, name_span)) = name_and_span {
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
                        // ADR-074: register as a function overload. A name with several function
                        // definitions in this scope forms an overload set; resolution at the call
                        // site selects by argument types. The first definition is the primary.
                        let (slot, dup) =
                            self.env.define_fn_overload(name.clone(), fn_type, Some(name_span));
                        if dup {
                            self.diagnostics.push(
                                Diagnostic::error(
                                    name_span,
                                    format!(
                                        "duplicate definition: an overload of `{}` with these \
                                         parameter types already exists",
                                        name
                                    ),
                                )
                                .with_help(
                                    "function overloads must differ in their parameter types — \
                                     the return type alone cannot distinguish them (spec §14.6)"
                                        .to_string(),
                                ),
                            );
                        }
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
