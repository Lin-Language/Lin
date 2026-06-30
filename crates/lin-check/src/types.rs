use indexmap::IndexMap;
use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt;

/// The set of syntactic lambda identities that can inhabit a `Type::Function` (Path-11 Leg 2,
/// "lambda-set specialization", PLDI'23 / the Roc model). This is **inert metadata**: it rides
/// along on a function type but is deliberately invisible to `Type` equality (`PartialEq` below),
/// structural compatibility (`compat.rs` ignores it via `..`), `Display`, and the monomorphization
/// instantiation key (which erases it — see `Type::erase_lambda_sets`). Adding it therefore cannot
/// perturb any equality/compat/codegen-driven behaviour — exactly the discipline the `sealed` flag
/// (ADR-057) and `StrLit` (ADR-034) established for representation/metadata markers.
///
/// Each syntactic lambda (and named function) is assigned a unique `u32` id by the checker
/// (`Checker::next_lambda_id`). A bare lambda value's function type carries `Known([id])`
/// (a singleton). When two function values merge at a union join (`if`/`match` branches selecting
/// among lambdas), their sets union (`LambdaSet::join`). `Top` is the ⊤ element — an unknown /
/// unbounded inhabitant set: it is the DEFAULT for every function type the checker does not
/// populate (annotations, intrinsic signatures, FFI, function values read out of `Json` or
/// returned from opaque/recursive positions), and it is absorbing under join.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum LambdaSet {
    /// ⊤ — unknown / unbounded set of inhabitants. The default and the absorbing join element.
    Top,
    /// A known, finite, sorted-deduped set of syntactic lambda identities. An empty `Known` is
    /// never produced (a function type with no known inhabitant stays `Top`).
    Known(Vec<u32>),
}

impl LambdaSet {
    /// Storage cap: a set wider than this collapses to `Top` to bound blow-up (Roc's small-set
    /// threshold is ~8; we keep a little headroom for measurement, then give up). Classification
    /// (`crate::lambda_set_stats`) treats anything past the small-set threshold as ⊤ anyway.
    pub const MAX_KNOWN: usize = 16;

    /// `LambdaSet::Top` as a fn pointer for `#[serde(default = ...)]`.
    pub fn top() -> LambdaSet {
        LambdaSet::Top
    }

    /// A singleton set — one syntactic lambda identity.
    pub fn singleton(id: u32) -> LambdaSet {
        LambdaSet::Known(vec![id])
    }

    /// Join two sets (the union-merge that runs when two function values merge at a control-flow
    /// join). `Top` is absorbing; two `Known` sets union (sorted-deduped); overflow past
    /// `MAX_KNOWN` collapses to `Top`.
    pub fn join(&self, other: &LambdaSet) -> LambdaSet {
        match (self, other) {
            (LambdaSet::Top, _) | (_, LambdaSet::Top) => LambdaSet::Top,
            (LambdaSet::Known(a), LambdaSet::Known(b)) => {
                let mut merged = a.clone();
                for id in b {
                    if !merged.contains(id) {
                        merged.push(*id);
                    }
                }
                if merged.len() > LambdaSet::MAX_KNOWN {
                    return LambdaSet::Top;
                }
                merged.sort_unstable();
                LambdaSet::Known(merged)
            }
        }
    }
}

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
    /// A singleton integer-literal type, e.g. `3` or `-1` in type position. At runtime an
    /// `IntLit` value is represented identically to an `Int32` (plain integer, no boxing, no RC);
    /// the literal only constrains type-checking (compat, bidirectional refinement,
    /// exhaustiveness). Mirrors `StrLit` for the integer domain.
    IntLit(i64),
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
    ///
    /// `name` is a DISPLAY-ONLY inert alias name. `Some(n)` iff this object resolved from a
    /// NON-GENERIC named record decl `type n = { … }` (set in resolve.rs). Invisible to
    /// `PartialEq` (manual impl below already ignores everything but `fields`), to `compat.rs`,
    /// and to the monomorphization key (erased by `erase_lambda_sets`). Renders as the bare name
    /// in `Display`. `#[serde(default)]` so old caches decode as `None`.
    Object {
        fields: IndexMap<String, Type>,
        sealed: bool,
        /// DISPLAY-ONLY inert alias name. See variant docs above.
        #[serde(default)]
        name: Option<String>,
    },
    /// A typed index-signature object type `{ K: V }` (ADR-055): an object used as a
    /// dictionary — arbitrary keys all mapping to value type `V`. Distinct from a fixed
    /// `Object` record. Backed at runtime by the hashed `LinMap` container (O(1) average lookup),
    /// NOT the assoc-list `LinObject`. `obj[k]` yields `V | Null`; `obj[k] = v` requires `v : V`.
    /// `key` is the key type: `Type::Str` for String-keyed maps, `Type::Int64` for Int-keyed maps.
    ///
    /// `name` is a DISPLAY-ONLY inert alias name, mirroring `Object::name`. `Some(n)` iff this map
    /// resolved from a NON-GENERIC named index-signature decl `type n = { K: V }` (set in
    /// resolve.rs). Invisible to `PartialEq` (manual impl below ignores it), to `compat.rs`, and to
    /// the monomorphization key (erased by `erase_lambda_sets`). Renders as the bare name in
    /// `Display`. `#[serde(default)]` so old caches decode as `None`.
    Map {
        key: Box<Type>,
        value: Box<Type>,
        /// DISPLAY-ONLY inert alias name. See variant docs above.
        #[serde(default)]
        name: Option<String>,
    },
    Union(Vec<Type>),
    Function {
        params: Vec<Type>,
        ret: Box<Type>,
        /// Number of leading parameters that have no default value, i.e. the
        /// minimum arity a (non-partial) call must supply. `required == params.len()`
        /// for functions without default arguments. Excluded from structural
        /// compatibility — see `compat.rs`.
        required: usize,
        /// Path-11 lambda-set metadata: the set of syntactic lambdas that can inhabit this
        /// function type. INERT — invisible to equality, compatibility, Display, and the
        /// monomorphization key (see `LambdaSet`). `#[serde(default)]` => deserializes to `Top`
        /// for any cache/signature written before this field existed; the cache stamp is bumped
        /// regardless so stale bincode is rejected rather than mis-decoded.
        #[serde(default = "LambdaSet::top")]
        lset: LambdaSet,
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
    /// `Promise<T>` — an opaque handle to a value being computed on a background thread
    /// (`async`/`parallel`/`race`/`timeout`/`retry`/`poolAsync`). A sibling to `Shared`/`Stream`:
    /// an opaque boxed pointer (`TaggedVal*(TAG_PROMISE)` at runtime), NOT structurally compatible
    /// with `T` or `Json`, so the only operation is resolving it via `await` (which yields
    /// `T | Error`). Covariant in `T`. Spellable in source as `Promise<T>` / bare `Promise`
    /// (= `Promise<Json>`); see `resolve.rs`.
    Promise(Box<Type>),
    /// `Opaque(name)` — a named opaque handle type backed at runtime by a `TaggedVal*` box whose
    /// inner tag uniquely identifies the handle kind. Opaque handles are nominal (name-equality,
    /// not structural), non-generic (no type parameter), and non-transferable across threads when
    /// their runtime backing is a cursor-sharing resource.
    ///
    /// Current registered names and their runtime tags:
    ///   - `"TarEntry"` — `TAG_TAR_ENTRY`: generation-stamped archive entry handle; non-transferable
    ///     (shares a live cursor into the parent byte stream).
    ///
    /// Spellable in source as the bare name (e.g. `TarEntry`); see `resolve.rs` for the registry.
    Opaque(String),
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
            (IntLit(a), IntLit(b)) => a == b,
            (Array(a), Array(b)) => a == b,
            (FixedArray(a), FixedArray(b)) => a == b,
            // Ignore `sealed`: structural identity is the field map only.
            (Object { fields: a, .. }, Object { fields: b, .. }) => a == b,
            // Ignore `name`: structural identity is the key/value types only (mirrors `sealed`).
            (Map { key: k1, value: v1, .. }, Map { key: k2, value: v2, .. }) => k1 == k2 && v1 == v2,
            (Union(a), Union(b)) => a == b,
            // Ignore `lset`: the lambda-set metadata rides along structurally but is invisible to
            // `==` (mirrors the `sealed` flag above). Two function types with identical
            // params/ret/required but different inhabitant sets are equal — this keeps union
            // dedup/flatten, narrowing, exhaustiveness, zonk fixpoints, and cache identity behaving
            // exactly as before the field existed.
            (
                Function { params: p1, ret: r1, required: req1, .. },
                Function { params: p2, ret: r2, required: req2, .. },
            ) => p1 == p2 && r1 == r2 && req1 == req2,
            (Iterator(a), Iterator(b)) => a == b,
            (Shared(a), Shared(b)) => a == b,
            (Stream(a), Stream(b)) => a == b,
            (Promise(a), Promise(b)) => a == b,
            (Opaque(a), Opaque(b)) => a == b,
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
        Type::Object { fields, sealed: false, name: None }
    }

    /// Construct a SEALED object type — used ONLY when unfolding a named record-type declaration
    /// in `resolve.rs`. Inert in Stage 0.5 (codegen ignores it).
    pub fn sealed_object(fields: IndexMap<String, Type>) -> Type {
        Type::Object { fields, sealed: true, name: None }
    }

    /// Attach a DISPLAY-ONLY alias name to an object type (no-op on non-Object). Used by
    /// resolve.rs when unfolding a named record decl so the name survives to Display/LSP.
    pub fn with_type_name(self, name: &str) -> Type {
        match self {
            Type::Object { fields, sealed, .. } => Type::Object { fields, sealed, name: Some(name.to_string()) },
            Type::Map { key, value, .. } => Type::Map { key, value, name: Some(name.to_string()) },
            other => other,
        }
    }

    /// Construct a function type with no default arguments (`required == params.len()`).
    /// The lambda set defaults to `Top` (unknown inhabitants) — populated lazily by the checker
    /// for actual lambda values.
    pub fn func(params: Vec<Type>, ret: Type) -> Type {
        let required = params.len();
        Type::Function { params, ret: Box::new(ret), required, lset: LambdaSet::Top }
    }

    /// Recursively rewrite every `Type::Function`'s lambda set to `Top`. Used to canonicalize a
    /// type before it is turned into a monomorphization key (`instantiation_key` uses `Debug`), so
    /// the lambda-set metadata can never split or merge specializations — i.e. it is byte-identical
    /// to the pre-Path-11 keying. Purely a normalization; never observed by codegen.
    pub fn erase_lambda_sets(&self) -> Type {
        match self {
            Type::Array(t) => Type::Array(Box::new(t.erase_lambda_sets())),
            Type::Iterator(t) => Type::Iterator(Box::new(t.erase_lambda_sets())),
            Type::Stream(t) => Type::Stream(Box::new(t.erase_lambda_sets())),
            Type::Shared(t) => Type::Shared(Box::new(t.erase_lambda_sets())),
            Type::Promise(t) => Type::Promise(Box::new(t.erase_lambda_sets())),
            Type::Map { key, value, .. } => Type::Map { key: Box::new(key.erase_lambda_sets()), value: Box::new(value.erase_lambda_sets()), name: None },
            Type::FixedArray(ts) => Type::FixedArray(ts.iter().map(|t| t.erase_lambda_sets()).collect()),
            Type::Union(ts) => Type::Union(ts.iter().map(|t| t.erase_lambda_sets()).collect()),
            Type::Object { fields, sealed, .. } => Type::Object {
                fields: fields.iter().map(|(k, v)| (k.clone(), v.erase_lambda_sets())).collect(),
                sealed: *sealed,
                name: None,  // STRIP name from mono keys — inert display metadata is not part of the key
            },
            Type::Function { params, ret, required, .. } => Type::Function {
                params: params.iter().map(|t| t.erase_lambda_sets()).collect(),
                ret: Box::new(ret.erase_lambda_sets()),
                required: *required,
                lset: LambdaSet::Top,
            },
            other => other.clone(),
        }
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

    /// True when a map with this KEY type should use the integer-keyed runtime representation
    /// (`lin_map_*_int`). Covers concrete fixed-width integer types AND closed integer-literal
    /// sets — `IntLit(_)` alone, or a `Union` whose every member is an integer-map key (e.g.
    /// `type DayOfWeek = 0 | 1 | 2 | 3 | 4 | 5 | 6`). An empty union does NOT qualify.
    /// All four map alloc/set/get key-kind dispatch sites in codegen MUST use this one predicate
    /// so that allocation, stores, and loads always agree on the key representation.
    pub fn is_int_map_key(&self) -> bool {
        match self {
            t if t.is_integer() => true,
            Type::IntLit(_) => true,
            Type::Union(variants) if !variants.is_empty() => {
                variants.iter().all(|v| v.is_int_map_key())
            }
            _ => false,
        }
    }

    /// True when a map with this KEY type qualifies for the dense (flat-array) representation.
    /// Requires a provably non-negative bounded integer: UInt8, UInt16, or UInt32.
    /// UInt64 is excluded (would require a 16GB flat array for max-key). Int* types are excluded
    /// (can be negative). IntLit unions that are all non-negative and fit in u32 also qualify.
    pub fn is_dense_int_key(&self) -> bool {
        match self {
            Type::UInt8 | Type::UInt16 | Type::UInt32 => true,
            Type::Union(variants) if !variants.is_empty() => {
                variants.iter().all(|v| match v {
                    Type::IntLit(n) => *n >= 0 && *n <= u32::MAX as i64,
                    t => t.is_dense_int_key(),
                })
            }
            _ => false,
        }
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
    /// Stage 2a: scalars + Bool + String + Array + Map + nested sealed records.
    ///
    /// This is the Stage 2a widening from the 0xFD pointer-backed path. The previous Stage 3b
    /// heap-field packing attempt (narrowed back out 2026-06-08) used the 0xFE INLINE contiguous
    /// payload path, which materialized a fresh `LinObject` on every generic read — the Token[]
    /// regression and the tlv.test.lin crash. Stage 2a uses the 0xFD POINTER-BACKED path instead:
    /// array slots are 8-byte struct pointers; an index read loads the pointer
    /// (`lin_sealed_ptr_array_get_ptr`) then GEPs into the struct at const-offset — no
    /// materialization. Drop already walks the descriptor via `lin_sealed_release_self`.
    ///
    /// Mirrors `lin_ir::lower::is_sealed_field_ty` EXACTLY (recursive nested-sealed check included).
    pub fn is_sealed_array_field_packable(&self) -> bool {
        self.is_flat_scalar()
            || matches!(self, Type::Bool)
            || self.is_string_ish()
            || matches!(self, Type::Array(_) | Type::FixedArray(_))
            || matches!(self, Type::Map { .. })
            || matches!(self, Type::Object { fields, sealed: true, .. }
                if !fields.is_empty() && fields.values().all(|f| f.is_sealed_array_field_packable()))
    }

    /// Returns true for the `AnyVal` dynamic top type (`TypeVar(u32::MAX)`).
    pub fn is_any_val(&self) -> bool {
        matches!(self, Type::TypeVar(u32::MAX))
    }

    /// True for `Str` and for any string-literal singleton (`StrLit`). Used wherever
    /// a runtime-string representation is what matters (equality, comparison, boxing,
    /// RC), since a `StrLit` is a `Str` at runtime. See ADR-035.
    pub fn is_string_ish(&self) -> bool {
        matches!(self, Type::Str | Type::StrLit(_))
    }

    /// True for any integer width AND for any integer-literal singleton (`IntLit`). An `IntLit`
    /// is represented identically to `Int32` at runtime; this predicate is the integer analogue
    /// of `is_string_ish` — use it wherever runtime integer representation is what matters.
    pub fn is_int_ish(&self) -> bool {
        self.is_integer() || matches!(self, Type::IntLit(_))
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
            Type::Array(inner) | Type::Iterator(inner) | Type::Stream(inner) | Type::Shared(inner) | Type::Promise(inner) => inner.contains_type_var(),
            Type::FixedArray(elems) => elems.iter().any(|t| t.contains_type_var()),
            Type::Union(variants) => variants.iter().any(|t| t.contains_type_var()),
            Type::Object { fields, .. } => fields.values().any(|t| t.contains_type_var()),
            Type::Map { key, value, .. } => key.contains_type_var() || value.contains_type_var(),
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

    // ── Packed-repr gate predicates (ADR-063 / ADR-069) ────────────────────────
    //
    // These are the SINGLE source of truth for the packed-vs-boxed decision. Every
    // crate that decides whether a value is packed (lin-ir, lin-codegen) calls these
    // instead of maintaining a local transcription. Downstream callers:
    //   - `lin_codegen::codegen::types::Codegen` methods delegate to these.
    //   - `lin_ir::repr` free-functions delegate to these.
    //   - `lin_ir::lower` predicates delegate via `lin_ir::repr`.
    //   - `lin_ir::monomorphize` and `lin_ir::escape` already call
    //     `Type::is_sealed_array_field_packable` (which lives here).

    /// True when `self` is a pure integer-literal union: a `Union` whose every member is an
    /// `IntLit`. Examples: `DayOfWeek = 0 | 1 | 2 | 3 | 4 | 5 | 6`. At runtime, stored as a
    /// 32-bit scalar (i32) — the same physical representation as `Int32` / `IntLit`. This is the
    /// sealed-scalar gate for integer-enum types: a pure-IntLit-union field in a sealed record
    /// stores inline as i32, eliminating the heap `TaggedVal*` slot that would otherwise block
    /// sealing of the whole record.
    pub fn is_pure_int_lit_union(&self) -> bool {
        match self {
            Type::Union(variants) if !variants.is_empty() => {
                variants.iter().all(|v| matches!(v, Type::IntLit(_)))
            }
            _ => false,
        }
    }

    /// True when `ty` is an unboxed scalar field of a sealed record: a fixed-width numeric,
    /// `Bool`, `IntLit` (Int32 at runtime), or a pure-IntLit union (i32 at runtime).
    /// Stored inline; no per-field RC.
    pub fn is_sealed_scalar_field(&self) -> bool {
        self.is_flat_scalar() || matches!(self, Type::Bool | Type::IntLit(_)) || self.is_pure_int_lit_union()
    }

    /// True when `ty` is an eligible HEAP field of a sealed record (String/StrLit, Array/FixedArray,
    /// Map, nested sealed record, a single-pointer union — a sum type whose runtime value is a
    /// `*SumNode` — or a NullableRecord `T|Null` where T is a sealed record, stored as a nullable
    /// 8-byte pointer with null-safe RC). Stored as an 8-byte owned pointer slot.
    ///
    /// Uses a thread-local visited set to break the infinite recursion that arises with recursive
    /// types like `Tree = { l: Tree|Null, r: Tree|Null }`: checking `Tree|Null` via
    /// `nullable_sealed_record` calls `sealed_fields(Tree)` which calls `is_sealed_field(Tree|Null)`
    /// which calls back here. When re-entered for the same union Display key, we return `true`
    /// optimistically — the type IS a valid nullable pointer field by construction.
    pub fn is_sealed_heap_field(&self) -> bool {
        if self.is_string_ish()
            || matches!(self, Type::Array(_) | Type::FixedArray(_))
            || matches!(self, Type::Map { .. })
            || (matches!(self, Type::Object { .. }) && Type::sealed_fields(self).is_some())
            || Type::sum_type_eligible(self)
        {
            return true;
        }
        // Named-alias union form: `Union([Named(n), Null])` — a self-recursive type alias reference
        // like `Tree | Null` where `Tree` is defined as a sealed record. The `Named` wrapper is the
        // cycle-breaking opaque ref the checker emits for self-recursive types; by construction the
        // named type IS a sealed record (if it weren't, the outer Object would not seal). Accept it
        // eagerly here; the IR repr pass will validate via `is_nullable_record_param`.
        if let Type::Union(members) = self {
            if !Type::sum_type_eligible(self) {
                let mut has_named = false;
                let mut only_named_or_null = true;
                for m in members {
                    match m {
                        Type::Null => {}
                        Type::Named(_) => { has_named = true; }
                        _ => { only_named_or_null = false; break; }
                    }
                }
                if has_named && only_named_or_null {
                    return true;
                }
            }
        }
        // NullableRecord gate with recursion guard: `T | Null` where T is a sealed record.
        // Guard against the cycle `nullable_sealed_record(T|Null)` → `sealed_fields(T)` →
        // `is_sealed_field(T|Null)` → here. When we're already evaluating this exact union, return
        // true (we're mid-proof that it IS a valid nullable sealed pointer field).
        thread_local! {
            static NR_VISITING: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
        }
        let key = format!("{}", self);
        let already_visiting = NR_VISITING.with(|v| v.borrow().contains(&key));
        if already_visiting {
            return true;
        }
        NR_VISITING.with(|v| v.borrow_mut().insert(key.clone()));
        let result = Type::nullable_sealed_record(self).is_some();
        NR_VISITING.with(|v| v.borrow_mut().remove(&key));
        result
    }

    /// True when `ty` is a permissible field of a sealed record: scalar or eligible heap field.
    pub fn is_sealed_field(&self) -> bool {
        self.is_sealed_scalar_field() || self.is_sealed_heap_field()
    }

    /// THE sealed-record gate. `Some(fields)` iff `ty` is a `Type::Object { sealed: true }` whose
    /// fields are all sealed-eligible. FAIL SAFE: `None` → boxed path.
    pub fn sealed_fields(ty: &Type) -> Option<&IndexMap<String, Type>> {
        match ty {
            Type::Object { fields, sealed: true, .. }
                if !fields.is_empty() && fields.values().all(|f| f.is_sealed_field()) =>
            {
                Some(fields)
            }
            _ => None,
        }
    }

    /// THE sealed-record-ARRAY gate. `Some(elem_fields)` iff `ty` is `Array(elem)` whose element
    /// is a sealed record with all fields packable. FAIL SAFE: `None` → boxed/flat array path.
    pub fn sealed_array_elem(ty: &Type) -> Option<&IndexMap<String, Type>> {
        let elem = match ty {
            Type::Array(e) => e.as_ref(),
            _ => return None,
        };
        let fields = Type::sealed_fields(elem)?;
        if fields.values().all(|f| f.is_sealed_array_field_packable()) {
            Some(fields)
        } else {
            None
        }
    }

    /// The UNIQUE recursive self-reference name of a candidate sum union (`None` if absent or
    /// ambiguous — mutual recursion → boxed, fail-safe).
    pub fn sum_recursive_self_name(ty: &Type) -> Option<String> {
        let variants = match ty {
            Type::Union(vs) => vs,
            _ => return None,
        };
        let mut names: std::collections::HashSet<String> = std::collections::HashSet::new();
        for v in variants {
            if let Type::Object { fields, .. } = v {
                for fty in fields.values() {
                    if let Type::Named(n) = fty {
                        names.insert(n.clone());
                    }
                }
            }
        }
        if names.len() == 1 { names.into_iter().next() } else { None }
    }

    /// THE Stage-1 sum-type gate. Returns the discriminant key iff `ty` is a `Type::Union` of 2+
    /// variants sharing a distinct `StrLit` discriminant and all other fields are sealed-eligible
    /// or a recursive self-child. Any violation → `None` → boxed union (fail-safe).
    pub fn sum_type_discriminant(ty: &Type) -> Option<String> {
        let variants = match ty {
            Type::Union(vs) => vs,
            _ => return None,
        };
        if variants.len() < 2 {
            return None;
        }
        let self_name = Type::sum_recursive_self_name(ty);
        let mut recs: Vec<&IndexMap<String, Type>> = Vec::with_capacity(variants.len());
        for v in variants {
            match v {
                Type::Object { fields, .. } if !fields.is_empty() => recs.push(fields),
                _ => return None,
            }
        }
        let first = recs[0];
        'keys: for (key, kty) in first.iter() {
            if !matches!(kty, Type::StrLit(_)) {
                continue;
            }
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            for rec in &recs {
                match rec.get(key) {
                    Some(Type::StrLit(s)) => {
                        if !seen.insert(s.clone()) {
                            continue 'keys;
                        }
                    }
                    _ => continue 'keys,
                }
            }
            for rec in &recs {
                for (fk, fty) in rec.iter() {
                    if fk == key {
                        continue;
                    }
                    if matches!(fty, Type::StrLit(_)) {
                        return None;
                    }
                    let is_recursive_child = self_name
                        .as_deref()
                        .is_some_and(|n| matches!(fty, Type::Named(name) if name == n));
                    if !fty.is_sealed_field() && !is_recursive_child {
                        return None;
                    }
                }
            }
            return Some(key.clone());
        }
        None
    }

    /// True when `ty` is a Stage-1-eligible unboxed sum type.
    pub fn sum_type_eligible(ty: &Type) -> bool {
        Type::sum_type_discriminant(ty).is_some()
    }

    /// Stage-3 NullableRecord gate: `Some(fields)` iff `ty` is `T | Null` where `T` is a sealed
    /// record. Excludes sum types (those use the SumNode path). FAIL SAFE: `None` → boxed.
    pub fn nullable_sealed_record(ty: &Type) -> Option<&IndexMap<String, Type>> {
        let variants = match ty {
            Type::Union(vs) => vs,
            _ => return None,
        };
        if Type::sum_type_eligible(ty) {
            return None;
        }
        let mut record: Option<&IndexMap<String, Type>> = None;
        for v in variants {
            if matches!(v, Type::Null) {
                continue;
            }
            match Type::sealed_fields(v) {
                Some(f) if record.is_none() => record = Some(f),
                _ => return None,
            }
        }
        record
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
            Type::IntLit(n) => write!(f, "{}", n),
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
            Type::Object { fields, name, .. } => {
                if let Some(n) = name {
                    return write!(f, "{}", n);
                }
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
            Type::Function { params, ret, required, .. } => {
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
            Type::Map { key, value, name, .. } => {
                if let Some(n) = name {
                    return write!(f, "{}", n);
                }
                write!(f, "{{ {}: {} }}", key, value)
            }
            Type::Iterator(inner) => write!(f, "Iterator<{}>", inner),
            Type::Shared(inner) => write!(f, "Shared<{}>", inner),
            Type::Stream(inner) => write!(f, "Stream<{}>", inner),
            Type::Promise(inner) => write!(f, "Promise<{}>", inner),
            Type::Opaque(name) => write!(f, "{}", name),
            // `TypeVar(u32::MAX)` is the dynamic `AnyVal` marker — render it as `AnyVal` (the former
            // `Json`; reset §2.5), not a raw id. Other ids are unresolved generic/inference variables;
            // they render as `?T<id>` so the LSP's `clean_type_string` can assign distinct positional
            // names (`T`/`U`/…) while deduping repeats of the same var.
            Type::TypeVar(id) if *id == u32::MAX => write!(f, "AnyVal"),
            Type::TypeVar(id) => write!(f, "?T{}", id),
            Type::Never => write!(f, "Never"),
            Type::Named(name) => write!(f, "{}", name),
        }
    }
}
