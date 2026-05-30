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

    /// Returns true for the dynamic "any" JSON type (TypeVar(u32::MAX)).
    pub fn is_json(&self) -> bool {
        matches!(self, Type::TypeVar(u32::MAX))
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

    pub fn flatten_union(types: Vec<Type>) -> Type {
        let mut flat = Vec::new();
        for t in types {
            match t {
                Type::Union(inner) => flat.extend(inner),
                other => flat.push(other),
            }
        }
        flat.dedup();
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
            Type::TypeVar(id) => write!(f, "?T{}", id),
            Type::Never => write!(f, "Never"),
            Type::Named(name) => write!(f, "{}", name),
        }
    }
}
