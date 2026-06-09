use indexmap::IndexMap;
use std::fmt;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
    /// exhaustiveness). See ADR-035.
    StrLit(String),
    Array(Box<Type>),
    FixedArray(Vec<Type>),
    /// A structural object type. `sealed` is an INERT representation marker (Stage 0.5 of the
    /// sealed-records design, ADR-057): it is `true` ONLY when this
    /// object originates from resolving a NAMED record-type declaration (`type T = { … }`) in
    /// `resolve.rs`, and `false` for every anonymous object literal type, inferred structural
    /// type, and built-in structural alias (e.g. `Error`).
    ///
    /// CRITICAL: the flag is invisible to structural compatibility (`compat.rs` ignores it) AND
    /// to `Type` equality — `PartialEq` for `Type` is implemented manually below to ignore
    /// `sealed`, so `Object(f, true) == Object(f, false)`. This keeps union dedup/flatten,
    /// narrowing, exhaustiveness, zonk, and cache identity behaving exactly as before the flag
    /// existed. Codegen ignores it entirely (no representation change). Stage 1 will be the first
    /// consumer. Construct via `Type::object(fields)` (unsealed) / `Type::sealed_object(fields)`.
    Object {
        fields: IndexMap<String, Type>,
        sealed: bool,
    },
    /// A typed index-signature object type `{ String: T }` (ADR-055): an object used as a
    /// dictionary — arbitrary string keys all mapping to value type `T`. Distinct from a fixed
    /// `Object` record. Backed at runtime by the hashed `LinMap` container (O(1) average lookup),
    /// NOT the assoc-list `LinObject`. `obj[k]` yields `T | Null`; `obj[k] = v` requires `v : T`.
    Map(Box<Type>),
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
    /// `Shared<T>` — opt-in shared *mutable* state (ADR-029). An opaque box over `T`; the ONLY
    /// operations are the `shared`/`get`/`set`/`withLock` accessors. It is deliberately NOT
    /// structurally compatible with `T` or with `Json` (see `compat.rs`), so any other operation
    /// on a `Shared<T>` — `push`, indexing, auto-unwrap — is a compile-time type error. It is
    /// constructed only by the `shared` intrinsic's return type; it cannot be spelled in source
    /// annotations (no `resolve.rs` case), so user code can never name it directly.
    Shared(Box<Type>),
    /// `Stream<T>` — an opaque, lazy, effectful pull-source that owns an OS resource (a file
    /// descriptor, socket, …) (ADR-047, streams brief). A sibling to `Iterator` but distinct:
    /// the iterator protocol's `cond`/`current` must be pure, whereas a stream is effectful and
    /// fallible. Like `Shared`, it is NOT structurally compatible with `T` or `Json` (see
    /// `compat.rs`), so any operation other than the stream API is a compile-time type error.
    /// It is NON-TRANSFERABLE across threads by copy (it owns a resource); crossing a thread
    /// boundary is a MOVE (CAP_MOVE, Stage 7). Covariant in `T`. Constructed only by the stream
    /// source intrinsics' return types; it cannot be spelled in source annotations.
    Stream(Box<Type>),
    TypeVar(u32),
    Never,
    /// A named type alias reference (used for recursive types that cannot be eagerly expanded).
    /// Equality and compatibility unfold one level via the type environment.
    Named(String),
}

/// Manual `PartialEq` for `Type`. Identical to the previous `#[derive(PartialEq)]` in EVERY
/// arm EXCEPT `Object`, where the `sealed` representation marker is deliberately ignored:
/// `Object { fields, sealed: true } == Object { fields, sealed: false }`. This guarantees that
/// adding the Stage-0.5 sealed flag cannot perturb any equality-driven behavior (union
/// dedup/flatten via `Vec::contains`, narrowing/exhaustiveness comparisons, `temp_types`
/// identity, zonk fixpoints, cache keys). The flag rides along structurally but is invisible to
/// `==`. See ADR-057 (Stage 0.5) and ADR-034 (the `StrLit` precedent).
impl PartialEq for Type {
    fn eq(&self, other: &Self) -> bool {
        use Type::*;
        match (self, other) {
            (Null, Null)
            | (Bool, Bool)
            | (Int8, Int8)
            | (Int16, Int16)
            | (Int32, Int32)
            | (Int64, Int64)
            | (UInt8, UInt8)
            | (UInt16, UInt16)
            | (UInt32, UInt32)
            | (UInt64, UInt64)
            | (Float32, Float32)
            | (Float64, Float64)
            | (Str, Str)
            | (Never, Never) => true,
            (StrLit(a), StrLit(b)) => a == b,
            (Array(a), Array(b)) => a == b,
            (FixedArray(a), FixedArray(b)) => a == b,
            // Ignore `sealed`: structural identity is the field map only.
            (Object { fields: a, .. }, Object { fields: b, .. }) => a == b,
            (Map(a), Map(b)) => a == b,
            (Union(a), Union(b)) => a == b,
            (
                Function { params: p1, ret: r1, required: req1 },
                Function { params: p2, ret: r2, required: req2 },
            ) => p1 == p2 && r1 == r2 && req1 == req2,
            (Iterator(a), Iterator(b)) => a == b,
            (Shared(a), Shared(b)) => a == b,
            (Stream(a), Stream(b)) => a == b,
            (TypeVar(a), TypeVar(b)) => a == b,
            (Named(a), Named(b)) => a == b,
            _ => false,
        }
    }
}

impl Type {
    /// Construct an UNSEALED structural object type (the default: anonymous literals, inferred
    /// shapes, built-in aliases). See the `Object` variant docs for the `sealed` semantics.
    pub fn object(fields: IndexMap<String, Type>) -> Type {
        Type::Object { fields, sealed: false }
    }

    /// Construct a SEALED object type — used ONLY when unfolding a named record-type declaration
    /// in `resolve.rs`. Inert in Stage 0.5 (codegen ignores it).
    pub fn sealed_object(fields: IndexMap<String, Type>) -> Type {
        Type::Object { fields, sealed: true }
    }

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

    /// THE single source of truth for Stage-3b sealed-record-array field packability (ADR-063): a
    /// field of a sealed record may live inline in a contiguous, header-less element buffer
    /// (`elem_tag` 0xFE) iff this returns true. Stage 3b lands by WIDENING this ONE predicate as the
    /// verification harness clears each field shape — never by adding per-shape branches in the RC
    /// implementation (the descriptor-driven primitives handle every heap-field kind uniformly).
    ///
    /// The four historical mirror sites — `Codegen::sealed_array_elem_field_packable`,
    /// `lin_ir::lower::is_sealed_array_elem_field_packable`, `lin_ir::monomorphize::field_packed_scalar`,
    /// and `lin_ir::repr::sealed_array_elem_field_packable` — now all delegate here, so the gate is a
    /// SINGLE definition. Any disagreement between the lowerer's ownership/Coerce insertion and
    /// codegen's physical layout would be a UAF / mis-read, which is exactly why this is centralised.
    ///
    /// CURRENTLY: scalars + Bool ONLY (Stage 3a).
    ///
    /// HEAP-FIELD PACKING (String/Array/Map/nested-record — Stage 3b steps 1-4) was implemented,
    /// shipped, and then NARROWED BACK OUT on 2026-06-08 because it is a NET LOSS in practice today:
    ///   - It REGRESSED `benchmarks/compare/interp` ~3x (Token = {kind:String, text:String}: packing
    ///     `Token[]` materializes a fresh boxed `LinObject` on every hot field read through the
    ///     generic `for`/combinator path — alloc + per-field retain — where a boxed `Object[]` is a
    ///     borrowed pointer load, strictly cheaper).
    ///   - It CRASHED `examples/raspberry-controller/tlv.test.lin` (a soundness bug in the packed
    ///     scalar-Array-field `{tag:Int32, bytes:Int32[]}[]` path).
    ///   - It delivers ZERO benefit to its intended consumer (RAPTOR) TODAY: `tripsByRoute` is still
    ///     `{String: Json[]}` and `bench.lin` packs zero sealed arrays.
    /// ROOT CAUSE (the strategic finding): packing only WINS when the value is read by const-offset
    /// through a TYPED PARAM (`(t: T) => t["f"]` → getelementptr+load). On the generic/boxed read
    /// path (`for`/`map`/union/Json index) mechanism (i) materializes the whole element per read —
    /// strictly worse than a boxed borrowed-pointer read. So heap-field packing must NOT re-land
    /// until reads through the typed iteration path are CHEAP (borrowed const-offset, no materialize)
    /// — the "spike B" / cheap-typed-reads work. The KIND_MAP/descriptor/transfer runtime plumbing is
    /// kept intact (dormant) for that re-land; only this gate predicate is narrowed.
    pub fn is_sealed_array_field_packable(&self) -> bool {
        self.is_flat_scalar() || matches!(self, Type::Bool)
    }

    /// Returns true for the dynamic "any" JSON type (TypeVar(u32::MAX)).
    pub fn is_json(&self) -> bool {
        matches!(self, Type::TypeVar(u32::MAX))
    }

    /// True for `Str` and for any string-literal singleton (`StrLit`). Used wherever
    /// a runtime-string representation is what matters (equality, comparison, boxing,
    /// RC), since a `StrLit` is a `Str` at runtime. See ADR-035.
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
    /// targets a `Json` value may NOT flow into without an explicit decode (ADR-045).
    pub fn contains_type_var(&self) -> bool {
        match self {
            Type::TypeVar(_) => true,
            Type::Array(inner) | Type::Iterator(inner) | Type::Stream(inner) => inner.contains_type_var(),
            Type::FixedArray(elems) => elems.iter().any(|t| t.contains_type_var()),
            Type::Union(variants) => variants.iter().any(|t| t.contains_type_var()),
            Type::Object { fields, .. } => fields.values().any(|t| t.contains_type_var()),
            Type::Map(v) => v.contains_type_var(),
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

    /// Drop the `Null` member from a union, returning the complement type. Used by flow-narrowing
    /// for `== null` / `is Null` tests: `T | Null` minus `Null` = `T`; `A | B | Null` minus
    /// `Null` = `A | B`. Returns `None` when `self` is not a union that actually contains a `Null`
    /// member (so the caller leaves the binding's type untouched — there is nothing to narrow).
    ///
    /// A thin wrapper over the general `without_variant`.
    pub fn without_null(&self) -> Option<Type> {
        self.without_variant(&Type::Null)
    }

    /// Subtract a single member type from a union, returning the complement (the union of the
    /// remaining members, flattened). The general flow-narrowing primitive: in a branch where an
    /// `is X` arm has been definitely excluded, the scrutinee narrows to `union minus X`.
    ///
    /// Examples (member equality is structural `Type::PartialEq`):
    ///   - `T | Null`  minus `Null`  = `T`
    ///   - `Int32 | String` minus `Int32` = `String`
    ///   - `A | B | C` minus `B` = `A | C`
    ///   - `String | Error` minus `Error` (`{ "type": String, "message": String }`) = `String`
    ///
    /// SOUNDNESS / no-guessing rule: returns `None` (leave the type untouched, narrow nothing)
    /// unless `self` is a union that contains `excluded` as an EXACTLY-matching member and the
    /// complement is non-empty. We never partially subtract or approximate: if `excluded` is not
    /// a member, or removing it would leave nothing, there is no sound narrowing to apply.
    pub fn without_variant(&self, excluded: &Type) -> Option<Type> {
        if let Type::Union(variants) = self {
            if variants.iter().any(|t| t == excluded) {
                let rest: Vec<Type> = variants.iter().filter(|t| *t != excluded).cloned().collect();
                if !rest.is_empty() {
                    return Some(Type::flatten_union(rest));
                }
            }
        }
        None
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
            Type::Object { fields, .. } => {
                if fields.is_empty() {
                    // An empty object type (e.g. the `{}` arm of an index-map param union
                    // `{ String: T } | {}`) renders as `{}`, not `{  }` — the latter reads as a
                    // stray double space in a diagnostic.
                    return write!(f, "{{}}");
                }
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
            Type::Map(v) => write!(f, "{{ String: {} }}", v),
            Type::Iterator(inner) => write!(f, "Iterator<{}>", inner),
            Type::Shared(inner) => write!(f, "Shared<{}>", inner),
            Type::Stream(inner) => write!(f, "Stream<{}>", inner),
            // `TypeVar(u32::MAX)` is the dynamic `Json` marker — render it as `Json`, not a raw id.
            // Other ids are unresolved generic/inference variables; they render as `?T<id>` so the
            // LSP's `clean_type_string` can assign distinct positional names (`T`/`U`/…) while
            // deduping repeats of the same var.
            Type::TypeVar(id) if *id == u32::MAX => write!(f, "Json"),
            Type::TypeVar(id) => write!(f, "?T{}", id),
            Type::Never => write!(f, "Never"),
            Type::Named(name) => write!(f, "{}", name),
        }
    }
}
