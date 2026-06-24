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
/// (ADR-045). When `false` (user modules), a `Json` value is NOT assignable to a fully
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
        // intrinsic signatures take `Shared<T>` explicitly (ADR-029). Inner `Json` (TypeVar MAX)
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

        // `Promise<T>` — opaque, covariant, and (like `Shared`/`Stream`) never widens to `Json`,
        // so a promise can only flow into another `Promise<U>` (compatible inner). The MAX/Json
        // wildcard is rejected explicitly before the permissive non-MAX-TypeVar arm; a Promise into
        // a union defers to the union rules (e.g. `Promise<T>` into `Promise<T> | Error`).
        (Type::Promise(a), Type::Promise(b)) => is_compatible_env(a, b, env, lenient_json, depth),
        (Type::Promise(_), Type::TypeVar(n)) if *n == u32::MAX => false,
        (Type::TypeVar(n), Type::Promise(_)) if *n == u32::MAX => false,
        (Type::Promise(_), Type::TypeVar(_)) | (Type::TypeVar(_), Type::Promise(_)) => true,
        (Type::Promise(_), Type::Union(_)) | (Type::Union(_), Type::Promise(_)) => {
            return union_compat(value_type, target_type, env, lenient_json, depth);
        }
        (Type::Promise(_), _) => false,
        (_, Type::Promise(_)) => false,

        // `Opaque(name)` — nominal opaque handles (TarEntry and future registered names).
        // Only compatible with itself (same name), or a TypeVar wildcard for generic contexts.
        // Never widens to Json. Mismatched names are hard-rejected.
        (Type::Opaque(a), Type::Opaque(b)) if a == b => true,
        (Type::Opaque(_), Type::Opaque(_)) => false,
        (Type::Opaque(_), Type::TypeVar(n)) if *n == u32::MAX => false,
        (Type::TypeVar(n), Type::Opaque(_)) if *n == u32::MAX => false,
        (Type::Opaque(_), Type::TypeVar(_)) | (Type::TypeVar(_), Type::Opaque(_)) => true,
        (Type::Opaque(_), Type::Union(_)) | (Type::Union(_), Type::Opaque(_)) => {
            return union_compat(value_type, target_type, env, lenient_json, depth);
        }
        (Type::Opaque(_), _) => false,
        (_, Type::Opaque(_)) => false,

        // `Function` — a callable value. A `Function` must NOT silently widen to `AnyVal`
        // (TypeVar(u32::MAX)). If allowed, stdlib authors could declare params as `AnyVal`
        // when they actually mean a specific function type, and callers would silently pass
        // function arguments with no arity/signature checking. The covariant-sink arm
        // `(_, TypeVar(MAX)) => true` below would otherwise pass functions through unchecked.
        //
        // Only the Function→AnyVal direction is rejected here (value=Function, target=AnyVal).
        // The REVERSE (AnyVal→Function, i.e. TypeVar(MAX) flowing into a function param) remains
        // permissive: this is the stdlib's trusted internal forwarding pattern (e.g. `parallel`
        // accepts `AnyVal[]` and passes it to `lin_parallel(tasks: (() => T)[])`). The original
        // comment in the lenient-decode arm explicitly lists "functions" as a permissive
        // AnyVal→concrete category. Rejecting that direction would break the stdlib's internal
        // plumbing. The `(TypeVar(MAX), target)` arm below governs that path.
        (Type::Function { .. }, Type::TypeVar(n)) if *n == u32::MAX => false,

        // Anything is assignable INTO Json (covariant sink): concrete T -> Json. (ADR-045)
        // This INCLUDES a typed index-signature map `{ String: T }` -> Json: a `LinMap` widened to
        // `Json` is only ever read back through the tag-aware `lin_*_any` bridges (keys/values/
        // entries), which dispatch on the runtime tag, so the widening is representation-safe. This
        // arm must stay AHEAD of the `Json -> Map` rejection below so `keys(typedMap)` still works.
        (_, Type::TypeVar(n)) if *n == u32::MAX => true,

        // `Json -> { String: T }` (index-signature map, ADR-055): REJECT in BOTH directions of
        // trust. A `Json` value's runtime payload is a `LinObject` (or any tag), NOT a `LinMap`;
        // relabelling it to the map type at the call boundary does not convert the representation,
        // so the callee then reads `LinObject` memory as a `LinMap` and corrupts it. There is
        // intentionally no implicit `Json -> { String: T }` coercion (§5.1.1, §6.3) — the value
        // must be decoded via `fromJson`/narrowing. Crucially this guard sits BEFORE the lenient
        // `Json -> concrete` arm below, so even the trusted stdlib cannot manufacture the coercion
        // (the same memory-safety precedent as the `Shared`/`Stream` arms above: a representational
        // mismatch is unsound regardless of who wrote the code). `Map -> Json` is handled by the
        // covariant-sink arm above and stays sound; `Map -> Map` covariance is below.
        // EXCEPTION (ADR-086 revised): a `AnyVal` value DOES flow into a map parameter whose key is
        // a QUANTIFIED-GENERIC type-parameter (`<K, V>(obj: { K: V })`, key ≥9001) — the `keys`
        // wrapper. This is safe where the plain `AnyVal -> { String: V }` coercion is not, because
        // `keys` reads the value through the tag-aware `lin_tagged_keys` bridge (which inspects the
        // runtime tag: a non-object `AnyVal` yields an empty result, never a `LinObject`-as-`LinMap`
        // misread). The result is `String[]` (the key var stays unbound → defaulted in `infer_call`).
        // Must precede the blanket reject below.
        (Type::TypeVar(s), Type::Map { key, value, .. })
            if *s == u32::MAX
                && matches!(**key, Type::TypeVar(id) if (9001..u32::MAX).contains(&id))
                && matches!(**value, Type::TypeVar(id) if (9001..u32::MAX).contains(&id)) =>
        {
            true
        }
        (Type::TypeVar(s), Type::Map { .. }) if *s == u32::MAX => false,

        // Json -> a concrete structured Object (one with a required, non-nullable field):
        // this is the silent-unvalidated-decode hazard the cast-hole fix targets — e.g.
        // `val p: Person = readJson(...)`. Reject in user code; the value must be decoded
        // via `fromJson` or narrowed via `is`/`has` (ADR-045). The trusted stdlib
        // (lenient_json) keeps the old permissive behaviour. Json flowing into scalars,
        // arrays, opaque handles (`Int64`/`Int32`), buffers (`UInt8[]`), open objects (`{}`),
        // functions, iterators, or anything still containing a TypeVar stays permissive:
        // those are the language's pervasive handle/buffer/polymorphic-return patterns, not
        // structured decodes (see ADR-045 for why the line is drawn at required-field objects).
        (Type::TypeVar(s), target) if *s == u32::MAX => {
            lenient_json || !requires_structured_decode(target, env, depth)
        }
        // Non-MAX inference / generic / intrinsic TypeVars stay bidirectionally permissive.
        (_, Type::TypeVar(_)) | (Type::TypeVar(_), _) => true,

        // Singleton string-literal types (ADR-034). A `StrLit("x")` is a `String` at runtime;
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

        // Singleton integer-literal types. An `IntLit(n)` is an `Int32` at runtime; the rules
        // mirror `StrLit`:
        //  1. two literals are compatible iff equal.
        //  2. a literal widens to any integer family type (Int32 is the canonical default).
        //  3. a bare integer type is NOT assignable to a specific literal — an arbitrary Int32
        //     is not statically known to equal the singleton value.
        (Type::IntLit(a), Type::IntLit(b)) => a == b,
        (Type::IntLit(_), t) if t.is_integer() => true,
        (t, Type::IntLit(_)) if t.is_integer() => false,

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
        // structural. See ADR-057 (sealed = representation-only; compat stays structural).
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

        // The "any-map" sink `{ String: AnyVal }` (target key `String`, target value the AnyVal
        // wildcard `TypeVar(MAX)`) accepts a map with ANY key type — `{ UInt8: V }`,
        // `{ DateNumber: V }`, etc. — not just a String-keyed one. ALL map keys are stringified at
        // runtime regardless of their static key type, and `{ String: AnyVal }` is only ever read
        // back through the tag-aware `lin_*_any` bridges (the basis of `std/object`'s
        // `keys`/`values`/`entries`), so widening any-keyed map -> any-map sink is read-only and
        // representation-safe (ADR-086). The key relaxation is gated TIGHT to the AnyVal-valued sink
        // so it does NOT open up arbitrary `{ Int: V } -> { String: V }` cross-key assignment, which
        // could mask a genuine key-type mismatch in user code; a non-map argument still rejects (it
        // never reaches this Map↔Map arm). This arm must sit ahead of the strict `k1 == k2` covariance.
        (Type::Map { value: v1, .. }, Type::Map { key: k2, value: v2, .. })
            if matches!(**k2, Type::Str) && matches!(**v2, Type::TypeVar(n) if n == u32::MAX) =>
        {
            is_compatible_env(v1, v2, env, lenient_json, depth)
        }

        // A fixed `Object` RECORD flowing into a map parameter whose key is a QUANTIFIED-GENERIC
        // type-parameter (`<K, V>(obj: { K: V })`, key TypeVar ≥9001 — only a function's own type
        // param resolves into this range; ADR-086 revised). This is the `std/object.keys` wrapper:
        // `keys({ "x": 1 })` passes a record into `keys = <K, V>(obj: { K: V }): K[]`. A record's
        // keys are strings at runtime, so binding `K = String` is sound, and `keys` only READS the
        // map (no write-back), so no mutation observes the record as a hashed map. Gated TIGHT to a
        // quantified-generic key so it never admits an arbitrary `Object -> { String: V }` (a concrete
        // map target) cross-shape assignment, which would mask a genuine record-vs-map mismatch.
        (Type::Object { .. }, Type::Map { key: k2, value: v2, .. })
            if matches!(**k2, Type::TypeVar(id) if (9001..u32::MAX).contains(&id))
                && matches!(**v2, Type::TypeVar(id) if (9001..u32::MAX).contains(&id)) =>
        {
            true
        }

        // Index-signature map covariance (`{ String: U }` -> `{ String: T }` when U compat T).
        // A `Map` is its OWN thing — NOT structurally compatible with a fixed `Object` record in
        // either direction (a value is one or the other; ADR-055). A non-`Map` value can only
        // flow into a `Map` target via the TypeVar/Json arms above (and `Json -> Map` is gated as a
        // structured decode in user code).
        (Type::Map { key: k1, value: v1, .. }, Type::Map { key: k2, value: v2, .. }) => k1 == k2 && is_compatible_env(v1, v2, env, lenient_json, depth),

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


/// True when assigning a `Json` value into `target` would silently skip validation of a
/// *structured object shape* — i.e. `target` is (or unfolds to) an `Object` with at least one
/// required (non-nullable) field. This is the cast-hole hazard the ADR-045 fix targets:
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
/// ADR-045 for the full empirical break list.
fn requires_structured_decode(target: &Type, env: Option<&TypeEnv>, depth: &mut usize) -> bool {
    if *depth > 32 {
        return false;
    }
    match target {
        Type::Object { fields, .. } => fields.values().any(|t| !includes_null(t)),
        // A typed index-signature map (`{ String: T }`, ADR-055) is a structured decode target:
        // a raw `Json` must be decoded via `fromJson`/narrowing, never silently assigned (parity
        // with the §6.3 Json-conversion rule). NOTE: `Json -> Map` is now rejected unconditionally
        // by the dedicated `(TypeVar(MAX), Map) => false` arm in `is_compatible_env` (which fires
        // BEFORE this lenient-gated path, so it also closes the trusted-stdlib hole). This branch is
        // retained as defensive intent — it keeps the user-code rejection self-evident here too.
        Type::Map { .. } => true,
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

/// Produce a top-down chain of human reasons explaining why `value` is not assignable to
/// `target`. `reasons[0]` is the outermost cause; deeper entries are nested causes (TypeScript-
/// style "...because..."). Returns an EMPTY vec when no structural sub-part can be named beyond
/// what the caller already prints (pure scalar-vs-scalar, or a leaf vs a union-of-leaves) — the
/// caller then appends nothing, keeping such messages byte-identical to today.
pub fn explain_incompatibility(
    value: &Type,
    target: &Type,
    env: Option<&TypeEnv>,
    lenient_json: bool,
) -> Vec<String> {
    let mut reasons = Vec::new();
    explain_walk(value, target, env, lenient_json, 0, &mut reasons);
    reasons
}

fn explain_walk(
    value: &Type,
    target: &Type,
    env: Option<&TypeEnv>,
    lenient_json: bool,
    depth: usize,
    out: &mut Vec<String>,
) {
    if depth > 32 {
        return;
    }

    // Unfold Named types one level on BOTH sides first, exactly like is_compatible_env.
    if let Type::Named(n) = value {
        if let Some(env) = env {
            if let Some(decl) = env.lookup_type(n) {
                if decl.params.is_empty() {
                    explain_walk(&decl.body.clone(), target, Some(env), lenient_json, depth + 1, out);
                    return;
                }
            }
        }
        // Named without env or with params: no useful sub-explanation.
        return;
    }
    if let Type::Named(n) = target {
        if let Some(env) = env {
            if let Some(decl) = env.lookup_type(n) {
                if decl.params.is_empty() {
                    explain_walk(value, &decl.body.clone(), Some(env), lenient_json, depth + 1, out);
                    return;
                }
            }
        }
        return;
    }

    match (value, target) {
        // Value is a union: find the first variant that is NOT compatible with the target.
        (Type::Union(vs), target) => {
            let bad_variant = vs
                .iter()
                .find(|b| !is_compatible_env(b, target, env, lenient_json, &mut 0));
            if let Some(b) = bad_variant {
                if matches!(b, Type::Null) {
                    out.push(format!("this can be `Null`, but `Null` is not assignable to `{target}`"));
                } else {
                    out.push(format!("the `{b}` case is not assignable to `{target}`"));
                    // Recurse only for structural types to avoid restating the obvious.
                    let is_structural = matches!(
                        b,
                        Type::Object { .. }
                            | Type::Map { .. }
                            | Type::Array(_)
                            | Type::FixedArray(_)
                            | Type::Function { .. }
                            | Type::Iterator(_)
                    );
                    if is_structural {
                        explain_walk(b, target, env, lenient_json, depth + 1, out);
                    }
                }
            }
        }

        // Target is a union (value is not): value must be assignable to at least one variant.
        (value, Type::Union(ts)) => {
            let target_list = ts
                .iter()
                .map(|t| format!("`{t}`"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(format!("`{value}` is not assignable to any of: {target_list}"));
        }

        // Both Object: find the first target field that doesn't match.
        (
            Type::Object { fields: value_fields, .. },
            Type::Object { fields: target_fields, .. },
        ) => {
            for (k, tf) in target_fields {
                let value_field_ty = value_fields.get(k).cloned().unwrap_or(Type::Null);
                if !is_compatible_env(&value_field_ty, tf, env, lenient_json, &mut 0) {
                    if !value_fields.contains_key(k) {
                        out.push(format!(
                            "the field \"{k}\" (type `{tf}`) is required but missing"
                        ));
                    } else {
                        out.push(format!("field \"{k}\" doesn't match:"));
                        explain_walk(&value_field_ty, tf, env, lenient_json, depth + 1, out);
                    }
                    return;
                }
            }
        }

        // Both Map: check key, then value.
        (
            Type::Map { key: k1, value: v1, .. },
            Type::Map { key: k2, value: v2, .. },
        ) => {
            if k1 != k2 {
                out.push(format!("the key type `{k1}` doesn't match `{k2}`"));
            } else {
                out.push("the map value type doesn't match:".to_string());
                explain_walk(v1, v2, env, lenient_json, depth + 1, out);
            }
        }

        // Array / Array
        (Type::Array(a), Type::Array(b)) => {
            out.push("the element type doesn't match:".to_string());
            explain_walk(a, b, env, lenient_json, depth + 1, out);
        }

        // FixedArray → Array: find the first element that doesn't match
        (Type::FixedArray(elements), Type::Array(elem_ty)) => {
            if let Some(e) = elements
                .iter()
                .find(|e| !is_compatible_env(e, elem_ty, env, lenient_json, &mut 0))
            {
                out.push("the element type doesn't match:".to_string());
                explain_walk(e, elem_ty, env, lenient_json, depth + 1, out);
            }
        }

        // Both FixedArray
        (Type::FixedArray(a), Type::FixedArray(b)) => {
            if a.len() != b.len() {
                out.push(format!(
                    "expected a {}-element tuple but found {}",
                    b.len(),
                    a.len()
                ));
            } else {
                for (i, (av, bv)) in a.iter().zip(b.iter()).enumerate() {
                    if !is_compatible_env(av, bv, env, lenient_json, &mut 0) {
                        out.push(format!("element {i} doesn't match:"));
                        explain_walk(av, bv, env, lenient_json, depth + 1, out);
                        return;
                    }
                }
            }
        }

        // Iterator / Iterator
        (Type::Iterator(a), Type::Iterator(b)) => {
            out.push("the element type doesn't match:".to_string());
            explain_walk(a, b, env, lenient_json, depth + 1, out);
        }

        // Iterator value vs Array target
        (Type::Iterator(v_elem), Type::Array(t_elem)) => {
            out.push("the element type doesn't match:".to_string());
            explain_walk(v_elem, t_elem, env, lenient_json, depth + 1, out);
        }

        // Both Function
        (
            Type::Function { params: vp, ret: vr, .. },
            Type::Function { params: tp, ret: tr, .. },
        ) => {
            if vp.len() != tp.len() {
                out.push(format!(
                    "expected a function with {} parameter{} but found {}",
                    tp.len(),
                    if tp.len() == 1 { "" } else { "s" },
                    vp.len()
                ));
            } else {
                // Check params (contravariant: target param vs value param)
                for (i, (vpi, tpi)) in vp.iter().zip(tp.iter()).enumerate() {
                    if !is_compatible_env(tpi, vpi, env, lenient_json, &mut 0) {
                        out.push(format!("parameter {} doesn't match:", i + 1));
                        explain_walk(tpi, vpi, env, lenient_json, depth + 1, out);
                        return;
                    }
                }
                // Check return (covariant)
                if !is_compatible_env(vr, tr, env, lenient_json, &mut 0) {
                    out.push("the return type doesn't match:".to_string());
                    explain_walk(vr, tr, env, lenient_json, depth + 1, out);
                }
            }
        }

        // Leaf types: push nothing (caller already shows both types).
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Type;

    fn object(fields: Vec<(&str, Type)>) -> Type {
        let map: indexmap::IndexMap<String, Type> = fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        Type::Object { fields: map, sealed: false, name: None }
    }

    fn map_type(key: Type, value: Type) -> Type {
        Type::Map { key: Box::new(key), value: Box::new(value), name: None }
    }

    fn union(vs: Vec<Type>) -> Type {
        Type::flatten_union(vs)
    }

    #[test]
    fn test_explain_union_with_null_into_map_union() {
        // { String: UInt32 } | Null into { String: AnyVal } | {}
        let value = union(vec![
            map_type(Type::Str, Type::UInt32),
            Type::Null,
        ]);
        let target = union(vec![
            map_type(Type::Str, Type::TypeVar(u32::MAX)),
            Type::Object { fields: indexmap::IndexMap::new(), sealed: false, name: None },
        ]);
        let reasons = explain_incompatibility(&value, &target, None, false);
        assert!(!reasons.is_empty(), "expected a reason for union-with-Null mismatch");
        assert!(
            reasons.iter().any(|r| r.contains("Null")),
            "expected Null mentioned, got: {:?}",
            reasons
        );
    }

    #[test]
    fn test_explain_object_field_scalar_mismatch() {
        // { "x": Int32 } into { "x": String }
        let value = object(vec![("x", Type::Int32)]);
        let target = object(vec![("x", Type::Str)]);
        let reasons = explain_incompatibility(&value, &target, None, false);
        assert!(!reasons.is_empty(), "expected reason for field mismatch");
        assert!(
            reasons[0].contains('"') && reasons[0].contains('x'),
            "expected field name in reason, got: {:?}",
            reasons
        );
    }

    #[test]
    fn test_explain_missing_required_field() {
        // {} into { "name": String }
        let value = object(vec![]);
        let target = object(vec![("name", Type::Str)]);
        let reasons = explain_incompatibility(&value, &target, None, false);
        assert!(!reasons.is_empty());
        assert!(
            reasons[0].contains("required but missing"),
            "got: {:?}",
            reasons
        );
    }

    #[test]
    fn test_explain_map_value_mismatch() {
        // { String: Int32 } into { String: String }
        let value = map_type(Type::Str, Type::Int32);
        let target = map_type(Type::Str, Type::Str);
        let reasons = explain_incompatibility(&value, &target, None, false);
        assert!(!reasons.is_empty());
        assert!(
            reasons[0].contains("map value"),
            "got: {:?}",
            reasons
        );
    }

    #[test]
    fn test_explain_array_element_mismatch() {
        // Int32[] into String[]
        let value = Type::Array(Box::new(Type::Int32));
        let target = Type::Array(Box::new(Type::Str));
        let reasons = explain_incompatibility(&value, &target, None, false);
        assert!(!reasons.is_empty());
        assert!(
            reasons[0].contains("element type"),
            "got: {:?}",
            reasons
        );
    }

    #[test]
    fn test_explain_fixed_array_length_mismatch() {
        // [Int32, Int32] into [String, String, String]
        let value = Type::FixedArray(vec![Type::Int32, Type::Int32]);
        let target = Type::FixedArray(vec![Type::Str, Type::Str, Type::Str]);
        let reasons = explain_incompatibility(&value, &target, None, false);
        assert!(!reasons.is_empty());
        assert!(
            reasons[0].contains("tuple"),
            "got: {:?}",
            reasons
        );
    }

    #[test]
    fn test_explain_scalar_vs_scalar_empty() {
        // Int32 into String: no structural sub-reason
        let reasons = explain_incompatibility(&Type::Int32, &Type::Str, None, false);
        assert!(
            reasons.is_empty(),
            "expected empty reasons for scalar-vs-scalar, got: {:?}",
            reasons
        );
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
