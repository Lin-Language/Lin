use std::collections::HashMap;
use std::rc::Rc;
use std::cell::RefCell;
use crate::value::Value;

#[derive(Clone, Debug)]
pub struct Env {
    bindings: HashMap<String, Binding>,
    parent: Option<Rc<RefCell<EnvInner>>>,
}

#[derive(Clone, Debug)]
struct EnvInner {
    bindings: HashMap<String, Binding>,
    parent: Option<Rc<RefCell<EnvInner>>>,
}

#[derive(Clone, Debug)]
pub enum Binding {
    Immutable(Value),
    Mutable(Rc<RefCell<Value>>),
}

impl Env {
    pub fn new() -> Self {
        Self {
            bindings: HashMap::new(),
            parent: None,
        }
    }

    pub fn child(parent: &Env) -> Self {
        let inner = EnvInner {
            bindings: parent.bindings.clone(),
            parent: parent.parent.clone(),
        };
        Self {
            bindings: HashMap::new(),
            parent: Some(Rc::new(RefCell::new(inner))),
        }
    }

    pub fn define(&mut self, name: String, value: Value) {
        self.bindings.insert(name, Binding::Immutable(value));
    }

    pub fn define_mut(&mut self, name: String, value: Value) {
        self.bindings.insert(name, Binding::Mutable(Rc::new(RefCell::new(value))));
    }

    pub fn get(&self, name: &str) -> Option<Value> {
        if let Some(binding) = self.bindings.get(name) {
            return Some(match binding {
                Binding::Immutable(v) => v.clone(),
                Binding::Mutable(cell) => cell.borrow().clone(),
            });
        }
        if let Some(parent) = &self.parent {
            return Self::get_from_inner(&parent.borrow(), name);
        }
        None
    }

    fn get_from_inner(inner: &EnvInner, name: &str) -> Option<Value> {
        if let Some(binding) = inner.bindings.get(name) {
            return Some(match binding {
                Binding::Immutable(v) => v.clone(),
                Binding::Mutable(cell) => cell.borrow().clone(),
            });
        }
        if let Some(parent) = &inner.parent {
            return Self::get_from_inner(&parent.borrow(), name);
        }
        None
    }

    pub fn set(&mut self, name: &str, value: Value) -> bool {
        if let Some(binding) = self.bindings.get(name) {
            if let Binding::Mutable(cell) = binding {
                *cell.borrow_mut() = value;
                return true;
            }
            return false;
        }
        if let Some(parent) = &self.parent {
            return Self::set_in_inner(&mut parent.borrow_mut(), name, value);
        }
        false
    }

    fn set_in_inner(inner: &mut EnvInner, name: &str, value: Value) -> bool {
        if let Some(binding) = inner.bindings.get(name) {
            if let Binding::Mutable(cell) = binding {
                *cell.borrow_mut() = value;
                return true;
            }
            return false;
        }
        if let Some(parent) = &inner.parent {
            return Self::set_in_inner(&mut parent.borrow_mut(), name, value);
        }
        false
    }

    pub fn keys(&self) -> Vec<String> {
        self.bindings.keys().cloned().collect()
    }

    pub fn get_mutable_cell(&self, name: &str) -> Option<Rc<RefCell<Value>>> {
        if let Some(Binding::Mutable(cell)) = self.bindings.get(name) {
            return Some(cell.clone());
        }
        if let Some(parent) = &self.parent {
            return Self::get_cell_from_inner(&parent.borrow(), name);
        }
        None
    }

    fn get_cell_from_inner(inner: &EnvInner, name: &str) -> Option<Rc<RefCell<Value>>> {
        if let Some(Binding::Mutable(cell)) = inner.bindings.get(name) {
            return Some(cell.clone());
        }
        if let Some(parent) = &inner.parent {
            return Self::get_cell_from_inner(&parent.borrow(), name);
        }
        None
    }
}
