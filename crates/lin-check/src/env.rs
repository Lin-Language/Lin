use indexmap::IndexMap;
use lin_common::Span;
use crate::types::Type;

#[derive(Debug, Clone)]
pub struct TypeEnv {
    scopes: Vec<Scope>,
    pub type_decls: IndexMap<String, TypeDecl>,
    next_slot: usize,
    next_type_var: u32,
}

#[derive(Debug, Clone)]
struct Scope {
    bindings: IndexMap<String, VarInfo>,
}

#[derive(Debug, Clone)]
pub struct VarInfo {
    pub slot: usize,
    pub ty: Type,
    pub mutable: bool,
    pub narrowed_ty: Option<Type>,
    /// The span of the binding site (the name token in val/var/param).
    pub def_span: Option<Span>,
    /// Additional function overloads sharing this name (ADR-074). Empty for the
    /// overwhelming majority of bindings; non-empty only when several functions in
    /// the same scope share a name. The primary signature lives in `ty`/`slot`; each
    /// alternate carries its own slot + (function) type. Distinguished by parameter
    /// types at the call site (`checker/call.rs`).
    pub overloads: Vec<OverloadAlt>,
}

/// One alternate in a function overload set (ADR-074). Each alternate is a distinct
/// top-level/local function with its own slot (hence its own FuncId/LLVM symbol).
#[derive(Debug, Clone)]
pub struct OverloadAlt {
    pub slot: usize,
    pub ty: Type,
    pub def_span: Option<Span>,
}

#[derive(Debug, Clone)]
pub struct TypeDecl {
    pub params: Vec<String>,
    pub body: Type,
}

impl Default for TypeEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl TypeEnv {
    pub fn new() -> Self {
        Self {
            scopes: vec![Scope {
                bindings: IndexMap::new(),
            }],
            type_decls: IndexMap::new(),
            next_slot: 0,
            next_type_var: 0,
        }
    }

    pub fn push_scope(&mut self) {
        self.scopes.push(Scope {
            bindings: IndexMap::new(),
        });
    }

    pub fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Drop scopes until `self.scopes.len() == len`. Used to roll back the scope stack after a
    /// discarded SPECULATIVE type-check whose nested `infer_function` `?`-ed out mid-body and
    /// left unbalanced pushed scopes (see `Checker::restore_checker_state`). Never grows.
    pub fn truncate_scopes(&mut self, len: usize) {
        if self.scopes.len() > len {
            self.scopes.truncate(len);
        }
    }

    pub fn define(&mut self, name: String, ty: Type, mutable: bool) -> usize {
        self.define_at(name, ty, mutable, None)
    }

    /// Shadow an existing binding with a narrowed type, reusing the same slot.
    /// Scoped: safe to call after push_scope, undone by pop_scope.
    pub fn define_narrowed(&mut self, name: String, narrowed_ty: Type, orig_slot: usize) {
        let info = VarInfo {
            slot: orig_slot,
            ty: narrowed_ty,
            mutable: false,
            narrowed_ty: None,
            def_span: None,
            overloads: Vec::new(),
        };
        self.scopes.last_mut().unwrap().bindings.insert(name, info);
    }

    pub fn define_at(&mut self, name: String, ty: Type, mutable: bool, def_span: Option<Span>) -> usize {
        let slot = self.next_slot;
        self.next_slot += 1;
        let info = VarInfo {
            slot,
            ty,
            mutable,
            narrowed_ty: None,
            def_span,
            overloads: Vec::new(),
        };
        self.scopes.last_mut().unwrap().bindings.insert(name, info);
        slot
    }

    /// Register a function overload (ADR-074). If `name` already binds a function in the
    /// CURRENT (innermost) scope, append `ty` as an additional overload sharing that name and
    /// return its fresh slot. Otherwise behave exactly like `define` (first definition →
    /// primary binding). Only ever called from the function pre-scan, so both the existing
    /// binding (if any) and `ty` are function types.
    ///
    /// Returns `(slot, is_duplicate)`. `is_duplicate` is true when an existing overload already
    /// has identical parameter types — the caller turns that into a diagnostic (the return type
    /// can never disambiguate, §14.6).
    pub fn define_fn_overload(&mut self, name: String, ty: Type, def_span: Option<Span>) -> (usize, bool) {
        let slot = self.next_slot;
        self.next_slot += 1;
        let scope = self.scopes.last_mut().unwrap();
        if let Some(existing) = scope.bindings.get_mut(&name) {
            // Only extend an existing *function* binding into an overload set. A non-function
            // binding of the same name in this scope is overwritten (ordinary shadowing) below.
            if matches!(existing.ty, Type::Function { .. }) {
                let new_params = fn_param_types(&ty);
                let dup = std::iter::once(&existing.ty)
                    .chain(existing.overloads.iter().map(|o| &o.ty))
                    .any(|t| fn_param_types(t) == new_params);
                existing.overloads.push(OverloadAlt { slot, ty, def_span });
                return (slot, dup);
            }
        }
        let info = VarInfo {
            slot,
            ty,
            mutable: false,
            narrowed_ty: None,
            def_span,
            overloads: Vec::new(),
        };
        scope.bindings.insert(name, info);
        (slot, false)
    }

    /// True when `name` resolves to a function overload set (≥2 signatures).
    pub fn is_overloaded(&self, name: &str) -> bool {
        self.lookup(name).is_some_and(|info| !info.overloads.is_empty())
    }

    /// All function overloads bound to `name` in the nearest scope that defines it, as
    /// `(slot, function_type)` pairs (primary first, then alternates in definition order).
    /// Returns an empty vec when `name` is unbound.
    pub fn overload_candidates(&self, name: &str) -> Vec<(usize, Type)> {
        match self.lookup(name) {
            Some(info) => std::iter::once((info.slot, info.ty.clone()))
                .chain(info.overloads.iter().map(|o| (o.slot, o.ty.clone())))
                .collect(),
            None => Vec::new(),
        }
    }

    /// Update the type of the overload entry (primary or alternate) identified by `slot`.
    pub fn update_overload_type(&mut self, slot: usize, ty: Type) {
        for scope in self.scopes.iter_mut().rev() {
            for info in scope.bindings.values_mut() {
                if info.slot == slot {
                    info.ty = ty;
                    return;
                }
                for alt in info.overloads.iter_mut() {
                    if alt.slot == slot {
                        alt.ty = ty;
                        return;
                    }
                }
            }
        }
    }

    pub fn lookup(&self, name: &str) -> Option<&VarInfo> {
        for scope in self.scopes.iter().rev() {
            if let Some(info) = scope.bindings.get(name) {
                return Some(info);
            }
        }
        None
    }

    /// Update the declared type of an existing binding (used for forward-declared functions).
    pub fn update_type(&mut self, name: &str, ty: Type) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(info) = scope.bindings.get_mut(name) {
                info.ty = ty;
                return;
            }
        }
    }

    pub fn narrow(&mut self, name: &str, narrowed: Type) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(info) = scope.bindings.get_mut(name) {
                info.narrowed_ty = Some(narrowed);
                return;
            }
        }
    }

    pub fn clear_narrowing(&mut self, name: &str) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(info) = scope.bindings.get_mut(name) {
                info.narrowed_ty = None;
                return;
            }
        }
    }

    pub fn effective_type(&self, name: &str) -> Option<Type> {
        self.lookup(name).map(|info| {
            info.narrowed_ty.clone().unwrap_or_else(|| info.ty.clone())
        })
    }

    pub fn define_type(&mut self, name: String, params: Vec<String>, body: Type) {
        self.type_decls.insert(name, TypeDecl { params, body });
    }

    pub fn lookup_type(&self, name: &str) -> Option<&TypeDecl> {
        self.type_decls.get(name)
    }

    pub fn fresh_type_var(&mut self) -> Type {
        let id = self.next_type_var;
        self.next_type_var += 1;
        Type::TypeVar(id)
    }

    pub fn next_slot(&self) -> usize {
        self.next_slot
    }

    /// Returns how many scopes are currently on the stack.
    pub fn scope_depth(&self) -> usize {
        self.scopes.len()
    }

    /// Look up a name and return the scope index where it lives (0 = global).
    pub fn lookup_with_depth(&self, name: &str) -> Option<(usize, &VarInfo)> {
        for (depth, scope) in self.scopes.iter().enumerate().rev() {
            if let Some(info) = scope.bindings.get(name) {
                return Some((depth, info));
            }
        }
        None
    }

    /// Return all visible binding names across all scopes.
    pub fn all_names(&self) -> Vec<&str> {
        let mut names = Vec::new();
        for scope in &self.scopes {
            for key in scope.bindings.keys() {
                names.push(key.as_str());
            }
        }
        names
    }
}

/// The parameter types of a function type, or an empty vec for a non-function. Used by the
/// overload machinery (ADR-074).
fn fn_param_types(ty: &Type) -> Vec<Type> {
    match ty {
        Type::Function { params, .. } => params.clone(),
        _ => Vec::new(),
    }
}
