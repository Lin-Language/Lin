use lin_common::{Diagnostic, Span};
use lin_parse::ast::{Expr, Stmt};

use super::Checker;
use super::helpers::{empty_literal_kind, is_legal_ffi_type};
use crate::resolve::resolve_type;
use crate::typed_ir::*;
use crate::types::Type;

impl Checker {
    /// Build the diagnostic for `import { name } from "path"` where `path` is a resolved module
    /// that does not export `name`. Prefers a cross-module suggestion (another module that DOES
    /// export `name`), falling back to a within-module did-you-mean for a likely typo.
    fn unknown_import_diagnostic(&self, path: &str, name: &str, span: Span) -> Diagnostic {
        let mut help_lines: Vec<String> = Vec::new();

        // CROSS-MODULE: which OTHER modules export this exact name?
        // Source 1: the stdlib export index (covers stdlib modules not necessarily imported here).
        // Source 2: the live import_types map (covers any resolved module, stdlib or local).
        let mut other_modules: Vec<String> = Vec::new();
        if let Some(mods) = self.stdlib_export_index.get(name) {
            for m in mods {
                if m != path {
                    other_modules.push(m.clone());
                }
            }
        }
        for (p, n) in self.import_types.keys() {
            if n == name && p != path && !other_modules.contains(p) {
                other_modules.push(p.clone());
            }
        }
        other_modules.sort();
        other_modules.dedup();
        for m in &other_modules {
            help_lines.push(format!("`{}` is exported by \"{}\"", name, m));
        }

        // WITHIN-MODULE: only if no cross-module match — suggest a near-miss export of `path`.
        if help_lines.is_empty() {
            let candidates: Vec<&str> = self
                .import_types
                .keys()
                .filter(|(p, _)| p == path)
                .map(|(_, n)| n.as_str())
                .collect();
            if let Some(s) = lin_common::closest_match(name, candidates.into_iter(), 3) {
                help_lines.push(format!("did you mean `{}`?", s));
            }
        }

        let diag = Diagnostic::error(
            span,
            format!("module \"{}\" has no export `{}`", path, name),
        );
        if help_lines.is_empty() {
            diag
        } else {
            diag.with_help(help_lines.join("\n"))
        }
    }

    pub(crate) fn check_stmt(&mut self, stmt: &Stmt) -> Result<TypedStmt, Diagnostic> {
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

                // Phase 4.5b: an INTERMEDIATE `val result = lin_array_allocate(n)` inside a
                // combinator whose declared return is `Array(elem)`. `infer_function` set
                // `array_alloc_elem_hint = Some((result, elem))` for exactly this binding. Check
                // the fresh allocation against `Array(elem)` so its Json-wildcard element type is
                // refined to the declared element (the generic param `U`). Monomorphization then
                // pins `Array(U)` to a concrete `Array(Int32)` and codegen emits a flat allocation
                // matching the flat reader. Gated to the allocation intrinsic (see the helper).
                // Double-gate on the value here too (the hint name was matched syntactically in
                // `infer_function`, but a nested non-function block could re-bind the same name to
                // a non-alloc value; never retype that). Only a direct `lin_array_allocate(..)`
                // call is refined.
                let value_is_array_allocate = matches!(
                    value,
                    Expr::Call { func, .. } if matches!(func.as_ref(), Expr::Ident(n, _) if n == "lin_array_allocate")
                );
                let alloc_hint_elem = match (binding_name, &self.array_alloc_elem_hint) {
                    (Some(name), Some((hint_name, elem)))
                        if name == hint_name && expected.is_none() && value_is_array_allocate =>
                    {
                        Some(Type::Array(Box::new(elem.clone())))
                    }
                    _ => None,
                };

                let mut typed_value = match (value, binding_name) {
                    (Expr::Function { type_params, params, return_type, body, span, .. }, Some(name)) => {
                        self.infer_function(type_params, params, return_type, body, *span, Some(name))?
                    }
                    _ => {
                        if let Some(ref hint_ty) = alloc_hint_elem {
                            self.check_expr(value, hint_ty)?
                        } else if let Some(ref expected_ty) = expected {
                            self.check_expr(value, expected_ty)?
                        } else {
                            // No annotation and no allocation hint: this is the genuinely
                            // evidence-free position. A bare `[]`/`{}` here has no element/value
                            // type to fix, so require an annotation rather than inferring the
                            // degenerate `Array(Never)` / empty-record type (ADR-058).
                            if let Some(kind) = empty_literal_kind(value) {
                                return Err(Diagnostic::error(value.span(), kind.message()));
                            }
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
                            let field_ty = if let Type::Object { fields: ref obj_fields, .. } = ty {
                                obj_fields.get(&key).cloned().unwrap_or(Type::Null)
                            } else { Type::TypeVar(u32::MAX) };
                            let slot = match &f.pattern {
                                lin_parse::ast::Pattern::Ident(name, name_span) => {
                                    self.env.define_at(name.clone(), field_ty.clone(), false, Some(*name_span))
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
                                lin_parse::ast::Pattern::Ident(name, name_span) => {
                                    self.env.define_at(name.clone(), elem_ty_inner.clone(), false, Some(*name_span))
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
                name_span,
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
                    // Evidence-free empty `[]`/`{}` with no annotation: require an annotation
                    // rather than inferring the degenerate element/value type (ADR-058).
                    if let Some(kind) = empty_literal_kind(value) {
                        return Err(Diagnostic::error(value.span(), kind.message()));
                    }
                    self.infer_expr(value)?
                };

                let ty = expected.unwrap_or_else(|| typed_value.ty());
                // Placement restriction (streams brief §8): a `Stream` may live only in a `val`
                // binding, function arg, or return value — NOT in a mutable `var` (which would
                // let a single resource be aliased through reassignment, defeating the affine
                // single-use guarantee). This is a hard ERROR.
                if super::expr::type_is_streamish(&ty) {
                    return Err(Diagnostic::error(
                        *span,
                        "a Stream cannot be stored in a `var` — bind it with `val` (a Stream is an \
                         affine resource, used at most once; `var` reassignment would alias it)",
                    ));
                }
                let slot = self.env.define_at(name.clone(), ty.clone(), true, Some(*name_span));
                // Track mutable globals for the async var-capture check.
                if self.function_scope_depths.is_empty() {
                    self.mutable_global_slots.insert(slot, name.clone());
                }

                Ok(TypedStmt::Var {
                    slot,
                    name: Some(name.clone()),
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
                //
                // For a generic alias (`type Box<T> = ...`), the params (`T`, `E`, ...) are not
                // real types — they must survive resolution as `Type::Named("T")` so that
                // `substitute` (resolve.rs) can replace them at each use-site (`Box<Int32>`).
                // We bind each param into a scratch env as a self-referential type decl (body
                // `Named(param)`); `resolve_named_cycle` then leaves it as `Named(param)` via the
                // same cycle guard used for the alias's own self-references. Without this, a bare
                // `T` in the body resolves as `Unknown type 'T'` and the body is never stored.
                let resolved = if params.is_empty() {
                    resolve_type(body, &self.env).map_err(|e| Diagnostic::error(*span, e))?
                } else {
                    let mut scratch = self.env.clone();
                    for param in params {
                        scratch.define_type(
                            param.clone(),
                            Vec::new(),
                            Type::Named(param.clone()),
                        );
                    }
                    resolve_type(body, &scratch).map_err(|e| Diagnostic::error(*span, e))?
                };
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
                // Is this module KNOWN to the checker? It is iff at least one export key carries
                // its path. (Some isolated/standalone checking modes seed NO signatures at all; in
                // that case we keep the fresh-TypeVar fallback and must NOT error — there's simply
                // no information to validate against.)
                let module_known = self.import_types.keys().any(|(p, _)| p == path);
                for binding in bindings {
                    let local_name = binding.alias.as_ref().unwrap_or(&binding.name);
                    // Use pre-resolved type if available, else fall back to TypeVar.
                    let has_export = self
                        .import_types
                        .contains_key(&(path.clone(), binding.name.clone()));
                    if module_known && !has_export {
                        // The module exists and was resolved, but does not export this name.
                        // Reject it here (a clean type-check error) instead of letting it slip
                        // through to a fresh TypeVar and fail later at link time with a mangled
                        // `undefined reference to <module>_<name>__val`.
                        let diag = self.unknown_import_diagnostic(path, &binding.name, *span);
                        self.diagnostics.push(diag);
                        // Still bind the local name (to a fresh TypeVar) so downstream uses don't
                        // panic on an undefined identifier; the pushed error already fails the
                        // module's overall type-check.
                        let ty = self.env.fresh_type_var();
                        let slot = self.env.define(local_name.clone(), ty.clone(), false);
                        self.import_origins.insert(
                            local_name.clone(),
                            (path.clone(), binding.name.clone()),
                        );
                        import_slots.push(ImportSlot {
                            name: binding.name.clone(),
                            slot,
                            ty,
                        });
                        continue;
                    }
                    let ty = self.import_types
                        .get(&(path.clone(), binding.name.clone()))
                        .cloned()
                        .unwrap_or_else(|| self.env.fresh_type_var());
                    let slot = self.env.define(local_name.clone(), ty.clone(), false);
                    // Record this binding's origin so a later `replace <local_name> = ...`
                    // (ADR-046) can resolve the imported export it targets.
                    self.import_origins.insert(
                        local_name.clone(),
                        (path.clone(), binding.name.clone()),
                    );
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
                            format!("Foreign binding '{}' has illegal FFI type '{}'; only numeric primitives, Boolean, Null (return only), and String (argument only) are allowed (spec §26.3)", binding.name, ty),
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
            Stmt::Replace { name, value, span } => {
                // Test-only mock (ADR-046). Resolve which imported export this targets, check the
                // body against the export's real signature, and record it as a symbol override.
                // Produces NO runtime statement itself — lowering reads `TypedModule::replacements`
                // and emits the body under the export's canonical symbol (suppressing the original).
                let Some((module_path, export_name)) = self.import_origins.get(name).cloned() else {
                    // Not an imported binding — either undefined or a local. `replace` only mocks
                    // imports; anything else is a usage error.
                    return Err(Diagnostic::error(
                        *span,
                        format!(
                            "`replace {0}` must target an imported binding; `{0}` is not imported in this file. Add `import {{ {0} }} from \"...\"` first (ADR-046).",
                            name
                        ),
                    ));
                };

                // Intrinsic primitives (print/map/filter/reduce/for/length/toString/async family)
                // are special-cased in codegen, not `Named` calls — they cannot be overridden by a
                // symbol swap. The stdlib WRAPPER (e.g. `std/array.map`) is replaceable, but a
                // direct re-export of the intrinsic name is not. Detect by the export's slot being
                // an intrinsic slot in THIS module's env.
                if let Some(info) = self.env.lookup(name) {
                    if self.intrinsic_slots.contains_key(&info.slot) {
                        return Err(Diagnostic::error(
                            *span,
                            format!(
                                "`replace {}` targets a compiler intrinsic, which cannot be mocked (it is not a linkable symbol). Mock the stdlib wrapper that calls it instead (ADR-046).",
                                name
                            ),
                        ));
                    }
                }

                // The export's declared type (the contract the mock must satisfy).
                let expected = self
                    .import_types
                    .get(&(module_path.clone(), export_name.clone()))
                    .cloned();

                // Check the body against the real signature so drift is a compile error. The
                // function-hint path uses expected param types as HINTS (an explicit annotation
                // can override them), so verify structural compatibility explicitly afterwards —
                // otherwise `replace double = (x: String): String => x` against `(Int32)=>Int32`
                // would slip through and emit a mismatched-ABI symbol.
                let typed_value = match &expected {
                    Some(ty) => {
                        let tv = self.check_expr(value, ty)?;
                        let actual = tv.ty();
                        if !self.types_compatible(&actual, ty) {
                            return Err(Diagnostic::error(
                                *span,
                                format!(
                                    "`replace {}` body has type `{}`, which does not match the import's declared type `{}` (ADR-046).",
                                    name, actual, ty
                                ),
                            ));
                        }
                        tv
                    }
                    None => self.infer_expr(value)?,
                };
                let ty = expected.unwrap_or_else(|| typed_value.ty());
                let is_function = matches!(ty, Type::Function { .. });

                let module_key: String = module_path.replace(['/', '-'], "_");
                let sym = format!("{}_{}", module_key, export_name);

                self.replacements.push(crate::typed_ir::Replacement {
                    sym,
                    export_name: export_name.clone(),
                    module_path: module_path.clone(),
                    is_function,
                    value: typed_value,
                    ty,
                    span: *span,
                });

                // No runtime statement; lowering handles emission.
                Ok(TypedStmt::Expr(TypedExpr::NullLit(*span)))
            }
            Stmt::Expr(expr) => {
                let typed = self.infer_expr(expr)?;
                // Must-use WARNING for a dropped Stream (streams brief §7): a `Stream` is an
                // affine resource — building a pipeline that is discarded as a bare statement (no
                // terminal `drain`/`for`/`collect`/`readText`/`close`) wastes it. Dropping is not
                // an ERROR (the RC finalizer still closes the fd), so this is a warning, not a
                // hard failure. A pipeline ENDED by a terminal has type `Null|Error`/`UInt8[]|…`,
                // not `Stream`, so a properly-consumed stream never triggers this.
                if super::expr::type_is_streamish(&typed.ty()) {
                    self.diagnostics.push(
                        lin_common::Diagnostic::warning(
                            expr.span(),
                            "this Stream value is unused — its pipeline is never driven \
                             (add a terminal like `.drain()`, `.for(...)`, `.collect()`, \
                             `.readText()`, or `.close()`)",
                        )
                        .with_note(expr.span(), "a dropped Stream is closed by its finalizer, but no data is ever read"),
                    );
                }
                Ok(TypedStmt::Expr(typed))
            }
        }
    }
}
