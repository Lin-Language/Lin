use indexmap::IndexMap;
use std::fmt;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Type {
    Null,
    Bool,
    Int8,
    Int16,
    Int32,
    Int64,
    UInt8,
    UInt16,
    UInt32,
    UInt64,
    Float32,
    Float64,
    Str,
    /// A singleton string-literal type, e.g. `"success"`. At runtime a `StrLit`
    /// value is represented identically to a `Str` (TAG_STR, same boxing/RC/toString);
    /// the literal only constrains type-checking (compat, bidirectional refinement,
    /// exhaustiveness). See ADR-053.
    StrLit(String),
    Array(Box<Type>),
    FixedArray(Vec<Type>),
    Object(IndexMap<String, Type>),
    Union(Vec<Type>),
    Function {
        params: Vec<Type>,
        ret: Box<Type>,
        /// Number of leading parameters that have no default value, i.e. the
        /// minimum arity a (non-partial) call must supply. `required == params.len()`
        /// for functions without default arguments. Excluded from structural
        /// compatibility — see `compat.rs`.
        required: usize,
    },
    Iterator(Box<Type>),
    /// `Shared<T>` — opt-in shared *mutable* state (ADR-044). An opaque box over `T`; the ONLY
    /// operations are the `shared`/`get`/`set`/`withLock` accessors. It is deliberately NOT
    /// structurally compatible with `T` or with `Json` (see `compat.rs`), so any other operation
    /// on a `Shared<T>` — `push`, indexing, auto-unwrap — is a compile-time type error. It is
    /// constructed only by the `shared` intrinsic's return type; it cannot be spelled in source
    /// annotations (no `resolve.rs` case), so user code can never name it directly.
    Shared(Box<Type>),
    TypeVar(u32),
    Never,
    /// A named type alias reference (used for recursive types that cannot be eagerly expanded).
    /// Equality and compatibility unfold one level via the type environment.
    Named(String),
}

impl Type {
    /// Construct a function type with no default arguments (`required == params.len()`).
    pub fn func(params: Vec<Type>, ret: Type) -> Type {
        let required = params.len();
        Type::Function { params, ret: Box::new(ret), required }
    }

    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            Type::Int8
                | Type::Int16
                | Type::Int32
                | Type::Int64
                | Type::UInt8
                | Type::UInt16
                | Type::UInt32
                | Type::UInt64
                | Type::Float32
                | Type::Float64
        )
    }

    pub fn is_integer(&self) -> bool {
        matches!(
            self,
            Type::Int8
                | Type::Int16
                | Type::Int32
                | Type::Int64
                | Type::UInt8
                | Type::UInt16
                | Type::UInt32
                | Type::UInt64
        )
    }

    pub fn is_float(&self) -> bool {
        matches!(self, Type::Float32 | Type::Float64)
    }

    /// True when this element type maps to a FLAT unboxed scalar array (a concrete
    /// fixed-width numeric). Mirrors codegen's `Codegen::is_flat_scalar` — the two MUST
    /// agree, since the checker decides when to refine an `arrayAllocate` result to a
    /// concrete array type and codegen decides whether to emit a flat allocation.
    pub fn is_flat_scalar(&self) -> bool {
        self.is_integer() || self.is_float()
    }

    /// Returns true for the dynamic "any" JSON type (TypeVar(u32::MAX)).
    pub fn is_json(&self) -> bool {
        matches!(self, Type::TypeVar(u32::MAX))
    }

    /// True for `Str` and for any string-literal singleton (`StrLit`). Used wherever
    /// a runtime-string representation is what matters (equality, comparison, boxing,
    /// RC), since a `StrLit` is a `Str` at runtime. See ADR-053.
    pub fn is_string_ish(&self) -> bool {
        matches!(self, Type::Str | Type::StrLit(_))
    }

    pub fn is_signed(&self) -> bool {
        matches!(
            self,
            Type::Int8 | Type::Int16 | Type::Int32 | Type::Int64
        )
    }

    pub fn is_unsigned(&self) -> bool {
        matches!(
            self,
            Type::UInt8 | Type::UInt16 | Type::UInt32 | Type::UInt64
        )
    }

    pub fn bit_width(&self) -> Option<u8> {
        match self {
            Type::Int8 | Type::UInt8 => Some(8),
            Type::Int16 | Type::UInt16 => Some(16),
            Type::Int32 | Type::UInt32 | Type::Float32 => Some(32),
            Type::Int64 | Type::UInt64 | Type::Float64 => Some(64),
            _ => None,
        }
    }

    /// True if this type contains any `TypeVar` anywhere in its structure
    /// (including the Json marker `TypeVar(u32::MAX)`, generic params, and fresh
    /// inference vars). A type with no TypeVar is "fully concrete" — the only
    /// targets a `Json` value may NOT flow into without an explicit decode (ADR-046).
    pub fn contains_type_var(&self) -> bool {
        match self {
            Type::TypeVar(_) => true,
            Type::Array(inner) | Type::Iterator(inner) => inner.contains_type_var(),
            Type::FixedArray(elems) => elems.iter().any(|t| t.contains_type_var()),
            Type::Union(variants) => variants.iter().any(|t| t.contains_type_var()),
            Type::Object(fields) => fields.values().any(|t| t.contains_type_var()),
            Type::Function { params, ret, .. } => {
                params.iter().any(|t| t.contains_type_var()) || ret.contains_type_var()
            }
            // Named types are opaque references; their bodies may contain Json but
            // are resolved/unfolded elsewhere. Treat a bare Named as non-vargenic
            // here (a concrete user type like `Person`).
            Type::Named(_) => false,
            _ => false,
        }
    }

    pub fn flatten_union(types: Vec<Type>) -> Type {
        let mut flat: Vec<Type> = Vec::new();
        for t in types {
            match t {
                Type::Union(inner) => {
                    // Order-preserving set insert. `Type` derives only `PartialEq` (no `Hash`/`Eq`),
                    // and union members can be NON-adjacent duplicates — e.g. `Null | Int32 | Null`
                    // arises from nesting `if … else null` (the literal-Null branch is unioned first,
                    // then a nested `if … else null` contributes another `Null` later). A
                    // consecutive-only `Vec::dedup()` would leave that duplicate in place and leak
                    // malformed types like `Null | Null` into diagnostics.
                    for m in inner {
                        if !flat.contains(&m) {
                            flat.push(m);
                        }
                    }
                }
                other => {
                    if !flat.contains(&other) {
                        flat.push(other);
                    }
                }
            }
        }
        if flat.len() == 1 {
            flat.into_iter().next().unwrap()
        } else {
            Type::Union(flat)
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Null => write!(f, "Null"),
            Type::Bool => write!(f, "Boolean"),
            Type::Int8 => write!(f, "Int8"),
            Type::Int16 => write!(f, "Int16"),
            Type::Int32 => write!(f, "Int32"),
            Type::Int64 => write!(f, "Int64"),
            Type::UInt8 => write!(f, "UInt8"),
            Type::UInt16 => write!(f, "UInt16"),
            Type::UInt32 => write!(f, "UInt32"),
            Type::UInt64 => write!(f, "UInt64"),
            Type::Float32 => write!(f, "Float32"),
            Type::Float64 => write!(f, "Float64"),
            Type::Str => write!(f, "String"),
            Type::StrLit(s) => write!(f, "\"{}\"", s),
            Type::Array(inner) => write!(f, "{}[]", inner),
            Type::FixedArray(types) => {
                write!(f, "[")?;
                for (i, t) in types.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", t)?;
                }
                write!(f, "]")
            }
            Type::Object(fields) => {
                write!(f, "{{ ")?;
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "\"{}\": {}", k, v)?;
                }
                write!(f, " }}")
            }
            Type::Union(types) => {
                for (i, t) in types.iter().enumerate() {
                    if i > 0 {
                        write!(f, " | ")?;
                    }
                    write!(f, "{}", t)?;
                }
                Ok(())
            }
            Type::Function { params, ret, required } => {
                write!(f, "(")?;
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    // Optional (defaulted) params render with a trailing `?`.
                    if i >= *required {
                        write!(f, "{}?", p)?;
                    } else {
                        write!(f, "{}", p)?;
                    }
                }
                write!(f, ") => {}", ret)
            }
            Type::Iterator(inner) => write!(f, "Iterator<{}>", inner),
            Type::Shared(inner) => write!(f, "Shared<{}>", inner),
            Type::TypeVar(id) => write!(f, "?T{}", id),
            Type::Never => write!(f, "Never"),
            Type::Named(name) => write!(f, "{}", name),
        }
    }
}
