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
        };
        self.scopes.last_mut().unwrap().bindings.insert(name, info);
        slot
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
