use crate::env::TypeEnv;
use crate::types::Type;

/// Check if `value_type` is structurally compatible with `target_type`.
/// This implements the `has`-style compatibility used for function arguments and assignments.
/// Named types are not unfolded here; use `is_compatible_env` when an env is available.
pub fn is_compatible(value_type: &Type, target_type: &Type) -> bool {
    is_compatible_env(value_type, target_type, None, false, &mut 0)
}

/// Env-aware compatibility check that can unfold Named types one level.
///
/// `lenient_json` controls the `Json` (`TypeVar(u32::MAX)`) → concrete-target direction
/// (ADR-046). When `false` (user modules), a `Json` value is NOT assignable to a fully
/// concrete target — it must be decoded via `fromJson` or narrowed via `is`/`has`. When
/// `true` (the trusted stdlib, whose wrappers forward `Json` handles into concrete
/// intrinsic/foreign params by design), the old fully-permissive behaviour is kept.
pub fn is_compatible_env(
    value_type: &Type,
    target_type: &Type,
    env: Option<&TypeEnv>,
    lenient_json: bool,
    depth: &mut usize,
) -> bool {
    // Guard against infinite recursion in deeply nested recursive types.
    if *depth > 32 {
        return true;
    }

    if value_type == target_type {
        return true;
    }

    // Unfold Named types one level before comparing.
    if let Type::Named(n) = value_type {
        if let Some(env) = env {
            if let Some(decl) = env.lookup_type(n) {
                if decl.params.is_empty() {
                    *depth += 1;
                    let result = is_compatible_env(&decl.body.clone(), target_type, Some(env), lenient_json, depth);
                    *depth -= 1;
                    return result;
                }
            }
        }
        // Named without env or with params: treat as compatible (unknown type)
        return true;
    }
    if let Type::Named(n) = target_type {
        if let Some(env) = env {
            if let Some(decl) = env.lookup_type(n) {
                if decl.params.is_empty() {
                    *depth += 1;
                    let result = is_compatible_env(value_type, &decl.body.clone(), Some(env), lenient_json, depth);
                    *depth -= 1;
                    return result;
                }
            }
        }
        return true;
    }

    match (value_type, target_type) {
        // Never is the bottom type: assignable to anything (kept ahead of the Shared arms so
        // `Never -> Shared<T>` stays compatible).
        (Type::Never, _) => true,
        (_, Type::Never) => false,

        // `Shared<T>` is opaque and INVARIANT: it is compatible ONLY with another `Shared<U>`
        // (with compatible inner types). Crucially these arms come BEFORE the TypeVar/`Json`
        // wildcard below, so a `Shared<T>` does NOT silently widen to `Json` (which would let it
        // flow into any `Json` parameter — e.g. `push(s, x)` — and defeat the accessor-only
        // guard). The only ops on a `Shared<T>` are the shared/get/set/withLock accessors, whose
        // intrinsic signatures take `Shared<T>` explicitly (ADR-044). Inner `Json` (TypeVar MAX)
        // still matches via the recursive call, so `shared`'s generic `T` binds normally.
        (Type::Shared(a), Type::Shared(b)) => is_compatible_env(a, b, env, lenient_json, depth),
        (Type::Shared(_), _) => false,
        (_, Type::Shared(_)) => false,

        // `Stream<T>` is covariant in T (a `Stream<U>` flows into a `Stream<T>` when `U` is
        // compatible with `T`) but, like `Shared`, is opaque: it does NOT widen to `Json` nor
        // unify with any other CONCRETE type, so the only legal operations are the stream-API
        // intrinsics whose signatures take `Stream<T>` explicitly. These arms come BEFORE the
        // `Json` wildcard so a stream can never silently flow into a `Json` sink (brief §1).
        //
        // The one exception is an inference/generic TypeVar on the OTHER side: `Stream` IS
        // spellable only via the intrinsic returns, so the stdlib's thin wrappers take it through
        // UNANNOTATED params (`readChunk = (s) => lin_stream_read(s)`) whose fresh inference var
        // must unify with `Stream<T>`. We therefore DON'T reject when the other side is a TypeVar
        // — that case falls through to the bidirectional-permissive TypeVar arm below (which both
        // accepts and lets the arg's inference var bind to the stream type). The `u32::MAX` Json
        // wildcard is NOT a TypeVar exception here: a `Stream` must never widen to `Json`, so the
        // explicit guards below reject it before the permissive arm.
        (Type::Stream(a), Type::Stream(b)) => is_compatible_env(a, b, env, lenient_json, depth),
        (Type::Stream(_), Type::TypeVar(n)) if *n == u32::MAX => false,
        (Type::TypeVar(n), Type::Stream(_)) if *n == u32::MAX => false,
        (Type::Stream(_), Type::TypeVar(_)) | (Type::TypeVar(_), Type::Stream(_)) => true,
        // A `Stream` flowing into / out of a UNION must consult the union (e.g. `Stream<UInt8[]>`
        // into `Array | Iterator | Stream<T>` — the stream-`for`/intrinsic iterable param — must
        // match the `Stream` variant). Defer to the union rules below rather than rejecting here.
        (Type::Stream(_), Type::Union(_)) | (Type::Union(_), Type::Stream(_)) => {
            // fall through to the Union arms (handled after this match block via a re-dispatch).
            return union_compat(value_type, target_type, env, lenient_json, depth);
        }
        (Type::Stream(_), _) => false,
        (_, Type::Stream(_)) => false,

        // Anything is assignable INTO Json (covariant sink): concrete T -> Json. (ADR-046)
        (_, Type::TypeVar(n)) if *n == u32::MAX => true,
        // Json -> a concrete structured Object (one with a required, non-nullable field):
        // this is the silent-unvalidated-decode hazard the cast-hole fix targets — e.g.
        // `val p: Person = readJson(...)`. Reject in user code; the value must be decoded
        // via `fromJson` or narrowed via `is`/`has` (ADR-046). The trusted stdlib
        // (lenient_json) keeps the old permissive behaviour. Json flowing into scalars,
        // arrays, opaque handles (`Int64`/`Int32`), buffers (`UInt8[]`), open objects (`{}`),
        // functions, iterators, or anything still containing a TypeVar stays permissive:
        // those are the language's pervasive handle/buffer/polymorphic-return patterns, not
        // structured decodes (see ADR-046 for why the line is drawn at required-field objects).
        (Type::TypeVar(s), target) if *s == u32::MAX => {
            lenient_json || !requires_structured_decode(target, env, depth)
        }
        // Non-MAX inference / generic / intrinsic TypeVars stay bidirectionally permissive.
        (_, Type::TypeVar(_)) | (Type::TypeVar(_), _) => true,

        // Singleton string-literal types (ADR-051). A `StrLit("x")` is a `String` at runtime;
        // these rules constrain only check-time assignability:
        //  1. two literals are compatible iff equal (unequal => reject; the equal case is also
        //     caught by the `value_type == target_type` fast path above, but the explicit arm
        //     stops an unequal pair falling through to a later, wrong branch).
        //  2. a literal widens to the open `String` type.
        //  3. `String` is NOT assignable to a literal type — load-bearing rejection: an arbitrary
        //     string is not statically known to equal the singleton.
        (Type::StrLit(a), Type::StrLit(b)) => a == b,
        (Type::StrLit(_), Type::Str) => true,
        (Type::Str, Type::StrLit(_)) => false,

        // Numeric widening: narrower assignable to wider
        (a, b) if a.is_numeric() && b.is_numeric() => is_numeric_compatible(a, b),

        // Union on the value side: every variant must be compatible with target
        (Type::Union(variants), target) => {
            variants.iter().all(|v| is_compatible_env(v, target, env, lenient_json, depth))
        }

        // Union on the target side: value must be compatible with at least one variant
        (value, Type::Union(variants)) => {
            variants.iter().any(|v| is_compatible_env(value, v, env, lenient_json, depth))
        }

        // Array covariance
        (Type::Array(a), Type::Array(b)) => is_compatible_env(a, b, env, lenient_json, depth),

        // Fixed array to unbounded array
        (Type::FixedArray(elements), Type::Array(elem_ty)) => {
            elements.iter().all(|e| is_compatible_env(e, elem_ty, env, lenient_json, depth))
        }

        // Fixed array positional compatibility
        (Type::FixedArray(a), Type::FixedArray(b)) => {
            a.len() == b.len()
                && a.iter().zip(b.iter()).all(|(av, bv)| is_compatible_env(av, bv, env, lenient_json, depth))
        }

        // Object structural compatibility: value has all target fields with compatible types.
        // A missing field is allowed when the target field type includes Null.
        // INVARIANT (Stage 0.5): the `sealed` marker is IGNORED here — a sealed named-record type
        // and an unsealed anonymous type with the same fields remain mutually compatible, and a
        // wider Json is still assignable where a named type is expected. Compatibility is purely
        // structural. See ADR-082 (sealed = representation-only; compat stays structural).
        (Type::Object { fields: value_fields, .. }, Type::Object { fields: target_fields, .. }) => {
            target_fields.iter().all(|(key, target_ty)| {
                match value_fields.get(key) {
                    Some(vt) => is_compatible_env(vt, target_ty, env, lenient_json, depth),
                    None => is_compatible_env(&Type::Null, target_ty, env, lenient_json, depth),
                }
            })
        }

        // Function compatibility: contravariant params, covariant return
        (
            Type::Function { params: vp, ret: vr, .. },
            Type::Function { params: tp, ret: tr, .. },
        ) => {
            // Opaque `Function` annotation: all params are TypeVar(MAX) and ret is TypeVar(MAX).
            // Treat as accepting any function regardless of arity.
            let is_opaque_target = tp.iter().all(|p| matches!(p, Type::TypeVar(_)))
                && matches!(tr.as_ref(), Type::TypeVar(_));
            let is_opaque_value = vp.iter().all(|p| matches!(p, Type::TypeVar(_)))
                && matches!(vr.as_ref(), Type::TypeVar(_));
            if is_opaque_target || is_opaque_value {
                return true;
            }
            // ARITY-WIDTH SUBTYPING (iterator-callback index param). A callback that declares
            // FEWER parameters is assignable where MORE are expected, PROVIDED every EXTRA expected
            // trailing parameter is `Int32`. This is the type-system half of the optional 0-based
            // index parameter on the iterable combinators (`for`/`map`/`filter`/`reduce`/`while`
            // and the derived `find`/`some`/…): the intrinsic / wrapper signatures expect a
            // `(T, Int32) => …` (or reduce's `(U, T, Int32) => U`) callback, but a user's 1-arg
            // (reduce: 2-arg) lambda must still flow through. The leniency is TIGHT: only extra
            // trailing `Int32` params are tolerated, so this does NOT open up arbitrary arity
            // subtyping (a value with EXTRA params, or extra non-`Int32` expected params, still
            // rejects). The common (leading) params and the return type are checked as usual.
            if vp.len() < tp.len() {
                let extra_all_int32 = tp[vp.len()..]
                    .iter()
                    .all(|t| matches!(t, Type::Int32));
                if extra_all_int32 {
                    let params_ok = vp
                        .iter()
                        .zip(tp.iter())
                        .all(|(v, t)| is_compatible_env(t, v, env, lenient_json, depth));
                    let ret_ok = is_compatible_env(vr, tr, env, lenient_json, depth);
                    return params_ok && ret_ok;
                }
                return false;
            }
            if vp.len() != tp.len() {
                return false;
            }
            // Contravariant: target params must be compatible with value params
            let params_ok = vp
                .iter()
                .zip(tp.iter())
                .all(|(v, t)| is_compatible_env(t, v, env, lenient_json, depth));
            // Covariant: value return must be compatible with target return
            let ret_ok = is_compatible_env(vr, tr, env, lenient_json, depth);
            params_ok && ret_ok
        }

        // Iterator covariance
        (Type::Iterator(a), Type::Iterator(b)) => is_compatible_env(a, b, env, lenient_json, depth),

        _ => false,
    }
}

/// Apply the union compatibility rules (used by the `Stream`↔`Union` arms, which must defer to
/// the union logic instead of the opaque `Stream` rejection): a union VALUE is compatible when
/// every variant is; a union TARGET is compatible when at least one variant matches.
fn union_compat(
    value_type: &Type,
    target_type: &Type,
    env: Option<&TypeEnv>,
    lenient_json: bool,
    depth: &mut usize,
) -> bool {
    match (value_type, target_type) {
        (Type::Union(variants), target) => {
            variants.iter().all(|v| is_compatible_env(v, target, env, lenient_json, depth))
        }
        (value, Type::Union(variants)) => {
            variants.iter().any(|v| is_compatible_env(value, v, env, lenient_json, depth))
        }
        _ => false,
    }
}

#[allow(dead_code)]
pub fn is_exact_match(value_type: &Type, target_type: &Type) -> bool {
    if value_type == target_type {
        return true;
    }

    match (value_type, target_type) {
        (Type::Object { fields: value_fields, .. }, Type::Object { fields: target_fields, .. }) => {
            value_fields.len() == target_fields.len()
                && target_fields.iter().all(|(key, target_ty)| {
                    value_fields
                        .get(key)
                        .map(|vt| is_exact_match(vt, target_ty))
                        .unwrap_or(false)
                })
        }
        (Type::Array(a), Type::Array(b)) => is_exact_match(a, b),
        (Type::FixedArray(a), Type::FixedArray(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|(av, bv)| is_exact_match(av, bv))
        }
        // Named types: treat as compatible for exact match (can't expand without env)
        (Type::Named(a), Type::Named(b)) => a == b,
        _ => false,
    }
}

/// True when assigning a `Json` value into `target` would silently skip validation of a
/// *structured object shape* — i.e. `target` is (or unfolds to) an `Object` with at least one
/// required (non-nullable) field. This is the cast-hole hazard the ADR-046 fix targets:
/// `val p: Person = readJson(...)` where `Person = {name:String, age:Int32}`. An open object
/// `{}` (the stdlib "any object" sink) and a fully-optional object impose no obligation, so
/// they are NOT structured decodes. We deliberately do NOT treat scalar/array targets as
/// structured decodes: Json flowing into `Int64`/`Int32`/`UInt8[]`/etc. is the language's
/// pervasive opaque-handle / buffer / polymorphic-return pattern, which has no `fromJson`
/// remedy and predates this change.
///
/// A *total* scope (rejecting ANY `Json -> concrete T`, scalars/arrays included) was tried and
/// empirically rejected: it broke the stdlib's pervasive polymorphic-return idiom where
/// `slice`/`concat`/`accept`/`wait`/etc. return `Json` and the result is assigned to a concrete
/// `val` (`val sub: UInt8[] = slice(bytes, 1, 4)`, `val code: Int64 = wait(pid)`), and it broke
/// `is`-narrowing into a concrete branch (`if j is String then j else ""`, whose narrowed value
/// is still statically `Json`). Those have no `fromJson` remedy and forcing one is hostile, so
/// the gate is scoped to the genuine hazard — unchecked *structured object* decodes. See
/// ADR-046 for the full empirical break list.
fn requires_structured_decode(target: &Type, env: Option<&TypeEnv>, depth: &mut usize) -> bool {
    if *depth > 32 {
        return false;
    }
    match target {
        Type::Object { fields, .. } => fields.values().any(|t| !includes_null(t)),
        Type::Named(n) => {
            if let Some(env) = env {
                if let Some(decl) = env.lookup_type(n) {
                    if decl.params.is_empty() {
                        *depth += 1;
                        let r = requires_structured_decode(&decl.body.clone(), Some(env), depth);
                        *depth -= 1;
                        return r;
                    }
                }
            }
            false
        }
        _ => false,
    }
}

/// True if `t` is `Null`, or a union that includes `Null` (an optional field type).
fn includes_null(t: &Type) -> bool {
    match t {
        Type::Null => true,
        Type::Union(variants) => variants.iter().any(includes_null),
        _ => false,
    }
}

fn is_numeric_compatible(value: &Type, target: &Type) -> bool {
    let vw = value.bit_width().unwrap_or(0);
    let tw = target.bit_width().unwrap_or(0);

    match (value.is_float(), target.is_float()) {
        // Float to float: wider target is fine
        (true, true) => tw >= vw,
        // Int to float: always ok if float can represent the integer range
        (false, true) => true,
        // Float to int: not implicitly compatible
        (true, false) => false,
        // Int to int
        (false, false) => {
            if value.is_signed() == target.is_signed() {
                tw >= vw
            } else if target.is_signed() {
                // Unsigned to signed: need more bits
                tw > vw
            } else {
                // Signed to unsigned: not implicitly compatible
                false
            }
        }
    }
}
