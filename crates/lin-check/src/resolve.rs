use indexmap::IndexMap;
use lin_common::Span;
use lin_parse::ast::TypeExpr;
use crate::env::TypeEnv;
use crate::types::Type;

/// Resolve a type expression, returning `Ok(Type)` on success or `Err(String)` on failure.
/// The error string does NOT carry span information — all existing callers that need a plain
/// `String` error (e.g. `function.rs`, `call.rs`) use this.
pub fn resolve_type(type_expr: &TypeExpr, env: &TypeEnv) -> Result<Type, String> {
    resolve_type_spanned(type_expr, env).map_err(|(_, m, _)| m)
}

/// Resolve a type expression, returning `Ok(Type)` on success or `Err((Span, String,
/// Option<String>))` on failure.  The span in the error points at the offending leaf
/// type-expression (e.g. the `Unknown type 'X'` span points at the `X` token, not the
/// surrounding declaration).  The third element is an optional help note that callers
/// may surface via `Diagnostic::with_help`.
pub fn resolve_type_spanned(
    type_expr: &TypeExpr,
    env: &TypeEnv,
) -> Result<Type, (Span, String, Option<String>)> {
    resolve_type_inner(type_expr, env, &mut std::collections::HashSet::new())
}

fn resolve_type_inner(
    type_expr: &TypeExpr,
    env: &TypeEnv,
    visiting: &mut std::collections::HashSet<String>,
) -> Result<Type, (Span, String, Option<String>)> {
    match type_expr {
        TypeExpr::Named(name, span) => {
            resolve_named_cycle(name, env, visiting).map_err(|m| (*span, m, None))
        }
        TypeExpr::Generic(name, args, span) => {
            let resolved_args: Vec<Type> =
                args.iter().map(|a| resolve_type_inner(a, env, visiting)).collect::<Result<_, _>>()?;
            // TypeScript-style utility-type operators are builtins ONLY in applied generic
            // position `Name<…>`, and ONLY when the user has NOT shadowed the name with their
            // own `type Name<…>` declaration (the user definition wins — fall through below).
            if is_utility_operator(name) && env.lookup_type(name).is_none() {
                return resolve_utility_operator(name, &resolved_args, *span)
                    .map_err(|(s, m, h)| (s, m, h));
            }
            resolve_generic(name, &resolved_args, env, visiting).map_err(|m| (*span, m, None))
        }
        TypeExpr::KeyOf(inner, span) => {
            let inner_ty = resolve_type_inner(inner, env, visiting)?;
            resolve_keyof(&inner_ty, *span)
        }
        TypeExpr::Index(base, key, span) => {
            let base_ty = resolve_type_inner(base, env, visiting)?;
            let key_ty = resolve_type_inner(key, env, visiting)?;
            resolve_indexed_access(&base_ty, &key_ty, *span)
        }
        TypeExpr::Array(inner, _span) => {
            let inner_ty = resolve_type_inner(inner, env, visiting)?;
            Ok(Type::Array(Box::new(inner_ty)))
        }
        TypeExpr::FixedArray(types, _span) => {
            let resolved: Result<Vec<Type>, (Span, String, Option<String>)> =
                types.iter().map(|t| resolve_type_inner(t, env, visiting)).collect();
            Ok(Type::FixedArray(resolved?))
        }
        TypeExpr::Union(types, _span) => {
            let resolved: Result<Vec<Type>, (Span, String, Option<String>)> =
                types.iter().map(|t| resolve_type_inner(t, env, visiting)).collect();
            Ok(Type::flatten_union(resolved?))
        }
        TypeExpr::Intersection(operands, span) => {
            // Record intersection `A & B` (ADR-061): record-only. Each operand must resolve to an
            // object/record type; the result is a plain `Type::Object` with the UNION of their
            // fields. A field present in more than one operand must have the SAME type (dedup) or
            // it is a hard error. The result is UNSEALED here exactly like an inline object literal
            // — when this intersection is the body of `type T = A & B`, the `: T` annotation path
            // (`expand_named_body`) seals it, so named=sealed is inherited automatically.
            let mut merged: IndexMap<String, Type> = IndexMap::new();
            for operand in operands {
                let ty = resolve_type_inner(operand, env, visiting)?;
                let fields = match &ty {
                    Type::Object { fields, .. } => fields,
                    other => {
                        return Err((*span, format!(
                            "intersection `&` is only valid between record types; operand `{}` is not a record",
                            other
                        ), None));
                    }
                };
                for (key, field_ty) in fields {
                    if let Some(existing) = merged.get(key) {
                        if existing != field_ty {
                            return Err((*span, format!(
                                "intersection type has conflicting field \"{}\": {} vs {}",
                                key, existing, field_ty
                            ), None));
                        }
                    } else {
                        merged.insert(key.clone(), field_ty.clone());
                    }
                }
            }
            Ok(Type::object(merged))
        }
        TypeExpr::Function(params, ret, _span) => {
            let param_types: Result<Vec<Type>, (Span, String, Option<String>)> =
                params.iter().map(|p| resolve_type_inner(p, env, visiting)).collect();
            let ret_type = resolve_type_inner(ret, env, visiting)?;
            // Type annotations cannot express default arguments, so every declared
            // parameter is required.
            Ok(Type::func(param_types?, ret_type))
        }
        TypeExpr::Object(fields, _span) => {
            // An object type spelled inline (anonymous structural shape, NOT a named record
            // declaration) is UNSEALED. Only the named-type unfold path (`expand_named_body` /
            // generic `substitute`) seals. See ADR-057.
            let mut resolved = IndexMap::new();
            for (key, type_expr) in fields {
                let ty = resolve_type_inner(type_expr, env, visiting)?;
                resolved.insert(key.clone(), ty);
            }
            Ok(Type::object(resolved))
        }
        TypeExpr::IndexSig(key, value, span) => {
            // `{ K: V }` — index-signature form. The key type-expr (which may be a type alias) is
            // resolved here, where aliases are expanded, and dispatches on what it denotes:
            //
            //   - `String` → a typed index-signature object (ADR-055): a dynamic `{ String: V }`
            //     map backed by the hashed `LinMap` at runtime, arbitrary string keys.
            //   - a CLOSED string-literal union (e.g. `DayOfWeek = "Monday" | … | "Sunday"`), or a
            //     single string-literal singleton → SUGAR for a fixed record with one field per
            //     literal, all of value type `V`. `{ DayOfWeek: Boolean }` ≡ `{ "Monday": Boolean,
            //     …, "Sunday": Boolean }`. Resolved UNSEALED, exactly like an inline object-literal
            //     type (the `TypeExpr::Object` arm above); the named-type unfold path seals it when
            //     this is the body of a `type T = …` declaration, so `named ⇒ sealed` is inherited.
            //     This composes with the total-literal-key index rule: indexing the record by a key
            //     of the SAME literal union is provably total (no safe-bracket `Null`).
            //   - anything else → an error (a non-String, non-literal key type is not indexable).
            //
            // HINT: if the key is a bare Named identifier that starts with a lowercase letter AND
            // the resolution fails with "Unknown type", we almost certainly have a record type that
            // was written without quoted keys (TypeScript-style `{ field: T }`). Attach a help note
            // explaining the quoting requirement.
            let key_result = resolve_type_inner(key, env, visiting);
            let key_ty = match key_result {
                Ok(t) => t,
                Err((key_span, msg, _existing_help)) => {
                    // Check whether the key is a bare lowercase identifier — the hallmark of a
                    // mistakenly-unquoted record field name. Uppercase identifiers are valid type
                    // names (e.g. `StopId`), so we don't hint on those.
                    let help = if msg.contains("Unknown type") {
                        if let TypeExpr::Named(key_name, _) = key.as_ref() {
                            if key_name.chars().next().map_or(false, |c| c.is_ascii_lowercase()) {
                                Some(format!(
                                    "record field keys must be quoted — write \"{name}\": T. \
                                     A bare `{name}: T` is parsed as a `{{ KeyType: ValueType }}` \
                                     map/index-signature, so `{name}` is read as a type name.",
                                    name = key_name
                                ))
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    return Err((key_span, msg, help));
                }
            };
            let val_ty = resolve_type_inner(value, env, visiting)?;
            if key_ty == Type::Str {
                Ok(Type::Map { key: Box::new(Type::Str), value: Box::new(val_ty), name: None })
            } else if key_ty.is_integer() {
                // Integer-keyed map: `{ Int: T }`, `{ Int32: T }`, etc.
                // Normalise to Int64 as the canonical key type (all integer values are stored as i64
                // in the runtime Int-map slots).
                Ok(Type::Map { key: Box::new(key_ty), value: Box::new(val_ty), name: None })
            } else if let Some(int_keys) = closed_int_literal_set(&key_ty) {
                // A closed union of integer literals (e.g. `0 | 1 | 2 | 3 | 4 | 5 | 6`): expand to
                // a fixed record keyed by the integer string representations, mirroring the
                // string-literal-union expansion. Index access is total (no `| Null`) when the key
                // type is the same literal union, exactly like `{ "mon"|"tue"|…: V }`.
                let mut fields = IndexMap::new();
                for k in int_keys {
                    fields.insert(k.to_string(), val_ty.clone());
                }
                Ok(Type::object(fields))
            } else if let Some(literals) = closed_string_literal_set(&key_ty) {
                let mut fields = IndexMap::new();
                for k in literals {
                    fields.insert(k, val_ty.clone());
                }
                Ok(Type::object(fields))
            } else if matches!(&key_ty, Type::TypeVar(id) if (9001..u32::MAX).contains(id)) {
                // A QUANTIFIED GENERIC key type-parameter `<K>` in `{ K: V }`. This is NOT spellable
                // as a free inference var — only a function's own type parameter resolves into this
                // ≥9001 range (`bind_type_params`). It lets a generic function be parametric over the
                // map's key type (the `keys` wrapper: `<K, V>(obj: { K: V }): K[]` — ADR-086 revised).
                // At each call site `K` binds to the receiver's CONCRETE key type, which was itself
                // already constrained to String/integer when that map type was built, so no unsound
                // key escapes. A genuine free inference var (id < 9001) still hits the error below.
                Ok(Type::Map { key: Box::new(key_ty), value: Box::new(val_ty), name: None })
            } else {
                Err((*span, format!(
                    "Index-signature key type must be String, an integer type, or a union of string \
                     literals, but it resolves to {}",
                    key_ty
                ), None))
            }
        }
        TypeExpr::TaggedUnion(variants, _span) => {
            let resolved: Result<Vec<Type>, (Span, String, Option<String>)> =
                variants.iter().map(|t| resolve_type_inner(t, env, visiting)).collect();
            Ok(Type::flatten_union(resolved?))
        }
        TypeExpr::StringLit(s, _span) => Ok(Type::StrLit(s.clone())),
        TypeExpr::IntLit(n, _span) => Ok(Type::IntLit(*n)),
    }
}

/// If `ty` is a single `IntLit` or a `Union` whose every member is an `IntLit`, return the integer
/// values (order-preserving). Otherwise `None`. Used by the index-signature arm to expand
/// `{ <int-literal-union>: V }` into a fixed record with integer-string keys.
fn closed_int_literal_set(ty: &Type) -> Option<Vec<i64>> {
    match ty {
        Type::IntLit(n) => Some(vec![*n]),
        Type::Union(variants) if !variants.is_empty() => {
            let mut keys = Vec::with_capacity(variants.len());
            for v in variants {
                match v {
                    Type::IntLit(n) => keys.push(*n),
                    _ => return None,
                }
            }
            Some(keys)
        }
        _ => None,
    }
}

/// If `ty` is a CLOSED set of string literals — a single `StrLit` or a `Union` whose every member
/// is a `StrLit` — return the literal strings (order-preserving). Otherwise `None`. Operates on an
/// already-resolved (concrete) `Type`, so `Named` aliases were peeled by the caller; a `Union`
/// arrives here already flattened/deduped (`flatten_union`), so the literals are distinct. Used by
/// the index-signature arm to expand `{ <literal-union>: V }` into a fixed record.
fn closed_string_literal_set(ty: &Type) -> Option<Vec<String>> {
    match ty {
        Type::StrLit(s) => Some(vec![s.clone()]),
        Type::Union(variants) if !variants.is_empty() => {
            let mut keys = Vec::with_capacity(variants.len());
            for v in variants {
                match v {
                    Type::StrLit(s) => keys.push(s.clone()),
                    _ => return None,
                }
            }
            Some(keys)
        }
        _ => None,
    }
}

// ── TypeScript-style utility type operators (Type → Type transforms) ──────────────────
//
// These are compiler builtins resolved during type-checking; they erase to ordinary
// record/union types and need NO codegen or runtime support. Each is recognised ONLY in
// applied generic position `Name<…>` (see the `Generic` arm of `resolve_type_inner`), and
// ONLY when the user has not shadowed the name with their own `type Name<…>` decl.
//
// All builtins return UNSEALED objects: when used as the body of a named decl, the named
// unfold path (`expand_named_body`) seals automatically.

/// The 10 reserved utility-operator names.
fn is_utility_operator(name: &str) -> bool {
    matches!(
        name,
        "Partial" | "Required" | "Pick" | "Omit" | "NonNullable" | "Exclude" | "Extract"
            | "ReturnType" | "Parameters" | "Record"
    )
}

type ResolveErr = (Span, String, Option<String>);

fn resolve_utility_operator(
    name: &str,
    args: &[Type],
    span: Span,
) -> Result<Type, ResolveErr> {
    // Arity validation, shared shape.
    let expect_arity = |n: usize| -> Result<(), ResolveErr> {
        if args.len() != n {
            Err((
                span,
                format!("`{}` takes exactly {} type argument(s), got {}", name, n, args.len()),
                None,
            ))
        } else {
            Ok(())
        }
    };
    match name {
        "Partial" => {
            expect_arity(1)?;
            let fields = expect_record(&args[0], name, span)?;
            let mut out = IndexMap::new();
            for (k, v) in fields {
                // Make every field nullable; flatten so an already-nullable field stays deduped.
                out.insert(
                    k.clone(),
                    Type::flatten_union(vec![v.clone(), Type::Null]),
                );
            }
            Ok(Type::object(out))
        }
        "Required" => {
            expect_arity(1)?;
            let fields = expect_record(&args[0], name, span)?;
            let mut out = IndexMap::new();
            for (k, v) in fields {
                out.insert(k.clone(), strip_null(v));
            }
            Ok(Type::object(out))
        }
        "Pick" => {
            expect_arity(2)?;
            let fields = expect_record(&args[0], name, span)?;
            let keys = expect_key_set(&args[1], name, span)?;
            let mut out = IndexMap::new();
            // Keep T's ORIGINAL field order; error on any requested key absent from T.
            for k in &keys {
                if !fields.contains_key(k) {
                    return Err(missing_field_err(k, fields, span, name));
                }
            }
            for (k, v) in fields {
                if keys.contains(k) {
                    out.insert(k.clone(), v.clone());
                }
            }
            Ok(Type::object(out))
        }
        "Omit" => {
            expect_arity(2)?;
            let fields = expect_record(&args[0], name, span)?;
            // Lenient: keys absent from T are simply ignored (no error).
            let keys = expect_key_set(&args[1], name, span)?;
            let mut out = IndexMap::new();
            for (k, v) in fields {
                if !keys.contains(k) {
                    out.insert(k.clone(), v.clone());
                }
            }
            Ok(Type::object(out))
        }
        "NonNullable" => {
            expect_arity(1)?;
            Ok(strip_null(&args[0]))
        }
        "Exclude" => {
            expect_arity(2)?;
            Ok(exclude_extract(&args[0], &args[1], true))
        }
        "Extract" => {
            expect_arity(2)?;
            Ok(exclude_extract(&args[0], &args[1], false))
        }
        "ReturnType" => {
            expect_arity(1)?;
            match &args[0] {
                Type::Function { ret, .. } => Ok((**ret).clone()),
                other => Err((
                    span,
                    format!("`ReturnType<F>` requires F to be a function type, but got {}", other),
                    None,
                )),
            }
        }
        "Parameters" => {
            expect_arity(1)?;
            match &args[0] {
                Type::Function { params, .. } => Ok(Type::FixedArray(params.clone())),
                other => Err((
                    span,
                    format!("`Parameters<F>` requires F to be a function type, but got {}", other),
                    None,
                )),
            }
        }
        "Record" => {
            expect_arity(2)?;
            // Mirror IndexSig resolution: String key → Map; closed StrLit union/single → Object.
            let key_ty = &args[0];
            let val_ty = args[1].clone();
            if *key_ty == Type::Str {
                Ok(Type::Map { key: Box::new(Type::Str), value: Box::new(val_ty), name: None })
            } else if let Some(literals) = closed_string_literal_set(key_ty) {
                let mut fields = IndexMap::new();
                for k in literals {
                    fields.insert(k, val_ty.clone());
                }
                Ok(Type::object(fields))
            } else {
                Err((
                    span,
                    format!(
                        "`Record<K, V>` requires K to be String or a closed union of string \
                         literals, but K resolves to {}",
                        key_ty
                    ),
                    None,
                ))
            }
        }
        _ => unreachable!("is_utility_operator gate is authoritative"),
    }
}

/// `keyof T`: union of T's field-name string literals, or `*key` for a map.
fn resolve_keyof(ty: &Type, span: Span) -> Result<Type, ResolveErr> {
    match ty {
        Type::Object { fields, .. } => {
            if fields.is_empty() {
                return Ok(Type::Never);
            }
            let members: Vec<Type> =
                fields.keys().map(|k| Type::StrLit(k.clone())).collect();
            Ok(Type::flatten_union(members))
        }
        Type::Map { key, .. } => Ok((**key).clone()),
        other => Err((
            span,
            format!("`keyof` requires a record type, but got {}", other),
            None,
        )),
    }
}

/// Indexed access `T[K]`: the type of the named field(s).
fn resolve_indexed_access(base: &Type, key: &Type, span: Span) -> Result<Type, ResolveErr> {
    let fields = match base {
        Type::Object { fields, .. } => fields,
        other => {
            return Err((
                span,
                format!("indexed-access type `T[K]` requires T to be a record, but got {}", other),
                None,
            ));
        }
    };
    let keys = key_set_of(key).ok_or_else(|| {
        (
            span,
            format!(
                "indexed-access key must be a string literal or a union of string literals, \
                 but got {}",
                key
            ),
            None,
        )
    })?;
    let mut selected: Vec<Type> = Vec::with_capacity(keys.len());
    for k in &keys {
        match fields.get(k) {
            Some(t) => selected.push(t.clone()),
            None => return Err(missing_field_err(k, fields, span, "indexed access")),
        }
    }
    Ok(Type::flatten_union(selected))
}

/// Require that `ty` is a record; return its field map or a clear error.
fn expect_record<'a>(
    ty: &'a Type,
    op: &str,
    span: Span,
) -> Result<&'a IndexMap<String, Type>, ResolveErr> {
    match ty {
        Type::Object { fields, .. } => Ok(fields),
        other => Err((
            span,
            format!("`{}<T>` requires T to be a record type, but got {}", op, other),
            None,
        )),
    }
}

/// Strip `Null` from a type. If `ty` is a union, remove every `Null` member and collapse a
/// singleton; if `ty` is exactly `Null`, the result is `Never`; otherwise unchanged.
fn strip_null(ty: &Type) -> Type {
    match ty {
        Type::Null => Type::Never,
        Type::Union(members) => {
            let kept: Vec<Type> =
                members.iter().filter(|m| **m != Type::Null).cloned().collect();
            if kept.is_empty() {
                Type::Never
            } else {
                Type::flatten_union(kept)
            }
        }
        other => other.clone(),
    }
}

/// `Exclude<U, M>` (remove == true) / `Extract<U, M>` (remove == false): treat U as a set of
/// members; M may itself be a union. Collapse singleton; empty → Never.
fn exclude_extract(u: &Type, m: &Type, remove: bool) -> Type {
    let u_members: Vec<Type> = match u {
        Type::Union(ms) => ms.clone(),
        other => vec![other.clone()],
    };
    let m_members: Vec<Type> = match m {
        Type::Union(ms) => ms.clone(),
        other => vec![other.clone()],
    };
    let kept: Vec<Type> = u_members
        .into_iter()
        .filter(|x| {
            let in_m = m_members.contains(x);
            if remove { !in_m } else { in_m }
        })
        .collect();
    if kept.is_empty() {
        Type::Never
    } else {
        Type::flatten_union(kept)
    }
}

/// Extract a closed set of string-literal keys from a type (a single `StrLit` or a `Union` of
/// `StrLit`s — exactly the output of `keyof`). Returns `None` if any member is not a `StrLit`.
fn key_set_of(ty: &Type) -> Option<Vec<String>> {
    closed_string_literal_set(ty)
}

/// As `key_set_of`, but with a uniform operator-flavoured error on failure.
fn expect_key_set(ty: &Type, op: &str, span: Span) -> Result<Vec<String>, ResolveErr> {
    key_set_of(ty).ok_or_else(|| {
        (
            span,
            format!(
                "`{}<T, K>` requires K to be a string literal or a union of string literals \
                 (e.g. `\"a\" | \"b\"` or `keyof T`), but got {}",
                op, ty
            ),
            None,
        )
    })
}

/// Build a "no such field" diagnostic with a did-you-mean help listing T's field names.
fn missing_field_err(
    bad_key: &str,
    fields: &IndexMap<String, Type>,
    span: Span,
    op: &str,
) -> ResolveErr {
    let available: Vec<&str> = fields.keys().map(|s| s.as_str()).collect();
    let help = match lin_common::closest_match(bad_key, available.iter().copied(), 3) {
        Some(best) => format!(
            "no field \"{}\" — did you mean \"{}\"? Available fields: {}",
            bad_key,
            best,
            available.join(", ")
        ),
        None => format!("available fields: {}", available.join(", ")),
    };
    (
        span,
        format!("`{}`: \"{}\" is not a field of the record type", op, bad_key),
        Some(help),
    )
}

fn resolve_named_cycle(
    name: &str,
    env: &TypeEnv,
    visiting: &mut std::collections::HashSet<String>,
) -> Result<Type, String> {
    match name {
        "Null" => Ok(Type::Null),
        "Boolean" => Ok(Type::Bool),
        "Int8" => Ok(Type::Int8),
        "Int16" => Ok(Type::Int16),
        "Int32" => Ok(Type::Int32),
        "Int64" => Ok(Type::Int64),
        "UInt8" => Ok(Type::UInt8),
        "UInt16" => Ok(Type::UInt16),
        "UInt32" => Ok(Type::UInt32),
        "UInt64" => Ok(Type::UInt64),
        "Float32" => Ok(Type::Float32),
        "Float64" => Ok(Type::Float64),
        // `Ptr` is a prototype FFI pointer type aliased to Int64 (the pointer-width int on the
        // 64-bit target, ABI-identical to void*). Keeping it a scalar Int64 means it is already a
        // legal FFI value type, already maps to LLVM i64, and never gets entangled in refcounting
        // — no new `Type` variant fanning across the ~20 exhaustive Type matches. FOLLOW-UP: a
        // distinct opaque newtype (e.g. `Type::Ptr`) would let the checker forbid arithmetic on
        // raw handles and prevent accidental Int64↔Ptr confusion; this alias is the prototype shortcut.
        "Ptr" => Ok(Type::Int64),
        "String" => Ok(Type::Str),
        // `AnyVal` is the dynamic top type (the former `Json` — reset §2.5, ADR-062 successor):
        // a JSON-shaped value whose shape is not statically known. `Json` is retained as a
        // deprecated alias so existing code keeps compiling; both resolve to the same wildcard.
        "AnyVal" => Ok(any_val_type()),
        // `Error` is the conventional error value (spec §20, §24.2.2) and a structural object
        // alias (ADR-031): an object carrying a `type` discriminant and a `message`. Both the
        // async runtime (on a caught thunk fault) and `fromJson` produce this shape — the
        // decode-error value additionally carries `"path"`, which width subtyping permits.
        // Modelled as an Object (not a new `Type` variant) so the ~20 exhaustive `Type` matches
        // don't change (cf. ADR-029); `is Error` is a field-presence + `"type" == "error"` check.
        "Error" => Ok(error_type()),
        // Function is an opaque type annotation — any arity is acceptable.
        // Params and ret use TypeVar(u32::MAX) so compat check treats it as accepting any function.
        "Function" => Ok(Type::func(
            vec![Type::TypeVar(u32::MAX)],
            Type::TypeVar(u32::MAX),
        )),
        // Iterator without type argument: use Json wildcard element type
        "Iterator" => Ok(Type::Iterator(Box::new(any_val_type()))),
        // Shared without a type argument: Shared<Json>. The opaque shared-mutable-state box
        // (ADR-029); only the shared/get/set/withLock accessors operate on it.
        "Shared" => Ok(Type::Shared(Box::new(any_val_type()))),
        // Stream without a type argument: Stream<Json>. The opaque pull-source (streams brief).
        // NOTE: the brief's locked decision was "not spellable in source"; we relaxed that to
        // EXACTLY the Shared precedent (a bare `Stream` annotation) so the trusted stdlib's thin
        // wrappers can annotate their `Stream` params (`(s: Stream)`) — the formatter mis-renders
        // an UNANNOTATED single param as an arg-position bare lambda, which is invalid at a
        // `val =` RHS. Opacity is unchanged: `compat.rs` still forbids any non-stream op on a
        // `Stream`, so naming it buys a user nothing but the type itself.
        "Stream" => Ok(Type::Stream(Box::new(any_val_type()))),
        // Promise without a type argument: Promise<Json>. The opaque async-result handle; the only
        // operation is `await` (which yields `T | Error`). Same opacity as Shared/Stream.
        "Promise" => Ok(Type::Promise(Box::new(any_val_type()))),
        // Opaque handle registry — names that map to Type::Opaque(name) rather than a
        // struct-expansion. Each name identifies a distinct runtime TaggedVal* box.
        //   "TarEntry"  — TAG_TAR_ENTRY: generation-stamped archive entry; non-transferable.
        //   "BigInt"    — TAG_BIGNUM: arbitrary-precision integer; refcounted Rust box.
        //   "Decimal"   — TAG_DECIMAL: exact base-10 fixed-point; refcounted Rust box.
        //   "Regex"     — program-lifetime immortal compiled pattern (leaked *regex::Regex boxed
        //                 as TAG_INT64); freely shareable across threads; RC is a no-op.
        "TarEntry" | "BigInt" | "Decimal" | "Regex" => Ok(Type::Opaque(name.to_string())),
        _ => {
            // Cycle detected: return Named(name) as an opaque reference instead of expanding.
            if visiting.contains(name) {
                return Ok(Type::Named(name.to_string()));
            }
            if let Some(decl) = env.lookup_type(name) {
                if decl.params.is_empty() {
                    visiting.insert(name.to_string());
                    let expanded = expand_named_body(&decl.body.clone(), env, visiting)?;
                    visiting.remove(name);
                    Ok(expanded.with_type_name(name))
                } else {
                    Err(format!(
                        "Type '{}' requires {} type argument(s)",
                        name,
                        decl.params.len()
                    ))
                }
            } else if name == "Number" {
                // `Number` is a parameter/return CONSTRAINT (a numerically-bounded generic,
                // ADR-014), not a value type — it only lowers to a bounded var in a function
                // signature. Reaching here means it was used in a binding/other position (e.g.
                // `val total: Number = 0`), where it has no concrete representation. Point the
                // user at a concrete family rather than the misleading "Unknown type 'Number'".
                Err("`Number` is a parameter constraint, not a value type; it is only valid on a \
                     function parameter or return (e.g. `(x: Number) => …`). Annotate this binding \
                     with a concrete numeric family such as `Int32` or `Float64`."
                    .to_string())
            } else {
                Err(format!("Unknown type '{}'", name))
            }
        }
    }
}

/// Re-expand Named(x) references inside an already-resolved type body.
/// This is needed when the body was stored before its recursive references
/// were expanded (because they pointed back at the currently-being-defined type).
pub(crate) fn expand_named_body(
    ty: &Type,
    env: &TypeEnv,
    visiting: &mut std::collections::HashSet<String>,
) -> Result<Type, String> {
    match ty {
        Type::Named(n) => resolve_named_cycle(n, env, visiting),
        Type::Array(inner) => Ok(Type::Array(Box::new(expand_named_body(inner, env, visiting)?))),
        Type::FixedArray(ts) => Ok(Type::FixedArray(
            ts.iter().map(|t| expand_named_body(t, env, visiting)).collect::<Result<_, _>>()?
        )),
        Type::Union(ts) => Ok(Type::Union(
            ts.iter().map(|t| expand_named_body(t, env, visiting)).collect::<Result<_, _>>()?
        )),
        Type::Object { fields, .. } => {
            // STAGE 0.5 SEAL POINT. `expand_named_body` is reached ONLY while unfolding the body
            // of a named record type declaration (`type T = { … }`) for a non-recursive `: T`
            // annotation. Mark the unfolded object SEALED so named-record identity survives
            // resolution (today it was discarded by collapsing to a bare field map).
            let mut out = IndexMap::new();
            for (k, v) in fields {
                out.insert(k.clone(), expand_named_body(v, env, visiting)?);
            }
            Ok(Type::sealed_object(out))
        }
        Type::Function { params, ret, required, lset } => Ok(Type::Function {
            params: params.iter().map(|p| expand_named_body(p, env, visiting)).collect::<Result<_, _>>()?,
            ret: Box::new(expand_named_body(ret, env, visiting)?),
            required: *required,
            lset: lset.clone(),
        }),
        Type::Iterator(inner) => Ok(Type::Iterator(Box::new(expand_named_body(inner, env, visiting)?))),
        Type::Stream(inner) => Ok(Type::Stream(Box::new(expand_named_body(inner, env, visiting)?))),
        Type::Promise(inner) => Ok(Type::Promise(Box::new(expand_named_body(inner, env, visiting)?))),
        // Preserve any DISPLAY-ONLY alias name already attached to a nested map (e.g. `Arrivals`
        // appearing as the value type of `type ByChanges = { UInt8: Arrivals }`): unfolding the
        // OUTER alias must not erase the INNER alias's name, or it reverts to the expanded form.
        Type::Map { key, value, name } => Ok(Type::Map { key: Box::new(expand_named_body(key, env, visiting)?), value: Box::new(expand_named_body(value, env, visiting)?), name: name.clone() }),
        other => Ok(other.clone()),
    }
}

fn resolve_generic(
    name: &str,
    args: &[Type],
    env: &TypeEnv,
    visiting: &mut std::collections::HashSet<String>,
) -> Result<Type, String> {
    match name {
        "Iterator" => {
            if args.len() != 1 {
                return Err("Iterator takes exactly 1 type argument".to_string());
            }
            Ok(Type::Iterator(Box::new(args[0].clone())))
        }
        "Shared" => {
            if args.len() != 1 {
                return Err("Shared takes exactly 1 type argument".to_string());
            }
            Ok(Type::Shared(Box::new(args[0].clone())))
        }
        "Stream" => {
            if args.len() != 1 {
                return Err("Stream takes exactly 1 type argument".to_string());
            }
            Ok(Type::Stream(Box::new(args[0].clone())))
        }
        "Promise" => {
            if args.len() != 1 {
                return Err("Promise takes exactly 1 type argument".to_string());
            }
            Ok(Type::Promise(Box::new(args[0].clone())))
        }
        _ => {
            if let Some(decl) = env.lookup_type(name) {
                if decl.params.len() != args.len() {
                    return Err(format!(
                        "Type '{}' expects {} argument(s), got {}",
                        name,
                        decl.params.len(),
                        args.len()
                    ));
                }
                let body = decl.body.clone();
                let params = decl.params.clone();
                let substituted = substitute(&body, &params, args, env, visiting)?;
                Ok(substituted)
            } else {
                Err(format!("Unknown generic type '{}'", name))
            }
        }
    }
}


fn substitute(
    ty: &Type,
    params: &[String],
    args: &[Type],
    env: &TypeEnv,
    visiting: &mut std::collections::HashSet<String>,
) -> Result<Type, String> {
    match ty {
        Type::Named(n) => {
            // If the name is one of the generic params, substitute it.
            if let Some(pos) = params.iter().position(|p| p == n) {
                return Ok(args[pos].clone());
            }
            // Otherwise expand it as a regular named type.
            resolve_named_cycle(n, env, visiting)
        }
        Type::Object { fields, sealed, .. } => {
            // Generic named-type instantiation (`type Box<T> = { value: T }` → `Box<Int32>`) is
            // also a named record unfold: preserve the declaration's sealed-ness. Bodies of named
            // record types arrive here sealed (set when the decl body was first resolved).
            // Do NOT propagate `name` here: generic instantiations keep `name: None` (structural
            // display) — the alias name is only attached for non-generic named records.
            let substituted: Result<IndexMap<String, Type>, String> = fields
                .iter()
                .map(|(k, v)| substitute(v, params, args, env, visiting).map(|t| (k.clone(), t)))
                .collect();
            Ok(Type::Object { fields: substituted?, sealed: *sealed, name: None })
        }
        Type::Array(inner) => Ok(Type::Array(Box::new(substitute(inner, params, args, env, visiting)?))),
        Type::FixedArray(types) => {
            Ok(Type::FixedArray(types.iter().map(|t| substitute(t, params, args, env, visiting)).collect::<Result<_, _>>()?))
        }
        Type::Union(types) => {
            Ok(Type::Union(types.iter().map(|t| substitute(t, params, args, env, visiting)).collect::<Result<_, _>>()?))
        }
        Type::Function {
            params: fn_params,
            ret,
            required,
            lset,
        } => Ok(Type::Function {
            params: fn_params
                .iter()
                .map(|t| substitute(t, params, args, env, visiting))
                .collect::<Result<_, _>>()?,
            ret: Box::new(substitute(ret, params, args, env, visiting)?),
            required: *required,
            lset: lset.clone(),
        }),
        Type::Iterator(inner) => Ok(Type::Iterator(Box::new(substitute(inner, params, args, env, visiting)?))),
        Type::Stream(inner) => Ok(Type::Stream(Box::new(substitute(inner, params, args, env, visiting)?))),
        Type::Promise(inner) => Ok(Type::Promise(Box::new(substitute(inner, params, args, env, visiting)?))),
        Type::Map { key, value, name } => Ok(Type::Map { key: Box::new(substitute(key, params, args, env, visiting)?), value: Box::new(substitute(value, params, args, env, visiting)?), name: name.clone() }),
        _ => Ok(ty.clone()),
    }
}

/// The structural shape of a decode `Error` (ADR-031). An open object with the two stable
/// fields user code can rely on; the runtime value also carries `"path"`, which width
/// subtyping permits. Used as the second variant of `fromJson`'s `T | Error` result.
pub fn error_type() -> Type {
    let mut fields = IndexMap::new();
    fields.insert("type".to_string(), Type::Str);
    fields.insert("message".to_string(), Type::Str);
    // `Error` is a built-in STRUCTURAL alias, NOT a sealed named record — it is used with extra
    // fields in practice (e.g. the decode-error `"path"`). Keep it UNSEALED. See §6 Q5.
    Type::object(fields)
}

pub fn any_val_type() -> Type {
    // AnyVal (formerly Json) is the dynamic top type: any value is compatible.
    // TypeVar(u32::MAX) is the special marker that is_compatible always accepts.
    Type::TypeVar(u32::MAX)
}

#[cfg(test)]
mod sealed_marker_tests {
    //! Stage 0.5 focused marker tests (ADR-057): prove the `sealed` flag is
    //! SET correctly on resolution without affecting behavior. The bulk of run-equivalence is
    //! carried by the full corpus; these pin the marker's value at the seal point.
    use super::*;
    use crate::env::TypeEnv;
    use lin_parse::ast::TypeExpr;
    use lin_common::Span;

    fn sealed_of(ty: &Type) -> Option<bool> {
        match ty {
            Type::Object { sealed, .. } => Some(*sealed),
            _ => None,
        }
    }

    #[test]
    fn named_record_resolves_sealed() {
        // type Point = { x: Int32, y: Int32 } — the decl body is an (unsealed) resolved object,
        // exactly as the checker stores it. Resolving a `: Point` annotation must seal it.
        let mut fields = IndexMap::new();
        fields.insert("x".to_string(), Type::Int32);
        fields.insert("y".to_string(), Type::Int32);
        let mut env = TypeEnv::new();
        env.define_type("Point".to_string(), vec![], Type::object(fields));

        let resolved = resolve_type(&TypeExpr::Named("Point".to_string(), Span::dummy()), &env)
            .expect("Point resolves");
        assert_eq!(sealed_of(&resolved), Some(true), "named record must resolve SEALED");
    }

    #[test]
    fn anonymous_object_literal_is_unsealed() {
        // An inline `{ "x": Int32 }` annotation is an anonymous structural type → UNSEALED.
        let mut obj_fields = Vec::new();
        obj_fields.push(("x".to_string(), TypeExpr::Named("Int32".to_string(), Span::dummy())));
        let te = TypeExpr::Object(obj_fields, Span::dummy());
        let env = TypeEnv::new();
        let resolved = resolve_type(&te, &env).expect("anon object resolves");
        assert_eq!(sealed_of(&resolved), Some(false), "anonymous literal must be UNSEALED");
    }

    #[test]
    fn error_alias_is_unsealed() {
        // The built-in `Error` structural alias is NOT a sealed named record.
        let env = TypeEnv::new();
        let resolved = resolve_type(&TypeExpr::Named("Error".to_string(), Span::dummy()), &env)
            .expect("Error resolves");
        assert_eq!(sealed_of(&resolved), Some(false), "Error alias must be UNSEALED");
    }

    fn obj_te(fields: &[(&str, &str)]) -> TypeExpr {
        TypeExpr::Object(
            fields
                .iter()
                .map(|(k, v)| (k.to_string(), TypeExpr::Named(v.to_string(), Span::dummy())))
                .collect(),
            Span::dummy(),
        )
    }

    #[test]
    fn intersection_merges_record_fields_unsealed() {
        // `{ "a": Int32 } & { "b": Int32 }` resolves to a Type::Object with BOTH fields, UNSEALED
        // (sealing happens only on the named-annotation path via expand_named_body).
        let te = TypeExpr::Intersection(
            vec![obj_te(&[("a", "Int32")]), obj_te(&[("b", "Int32")])],
            Span::dummy(),
        );
        let env = TypeEnv::new();
        let resolved = resolve_type(&te, &env).expect("intersection resolves");
        match &resolved {
            Type::Object { fields, sealed, .. } => {
                assert!(fields.contains_key("a") && fields.contains_key("b"));
                assert_eq!(*sealed, false, "inline intersection must be UNSEALED");
            }
            other => panic!("expected Object, got {:?}", other),
        }
    }

    #[test]
    fn intersection_same_field_same_type_dedups() {
        let te = TypeExpr::Intersection(
            vec![obj_te(&[("k", "Int32")]), obj_te(&[("k", "Int32")])],
            Span::dummy(),
        );
        let env = TypeEnv::new();
        let resolved = resolve_type(&te, &env).expect("dedup resolves");
        match &resolved {
            Type::Object { fields, .. } => assert_eq!(fields.len(), 1),
            other => panic!("expected Object, got {:?}", other),
        }
    }

    #[test]
    fn intersection_field_conflict_errors() {
        let te = TypeExpr::Intersection(
            vec![obj_te(&[("k", "Int32")]), obj_te(&[("k", "String")])],
            Span::dummy(),
        );
        let env = TypeEnv::new();
        let err = resolve_type(&te, &env).unwrap_err();
        assert!(err.contains("conflicting field \"k\""), "got: {}", err);
    }

    #[test]
    fn intersection_non_record_operand_errors() {
        let te = TypeExpr::Intersection(
            vec![
                TypeExpr::Named("Int32".to_string(), Span::dummy()),
                TypeExpr::Named("String".to_string(), Span::dummy()),
            ],
            Span::dummy(),
        );
        let env = TypeEnv::new();
        let err = resolve_type(&te, &env).unwrap_err();
        assert!(err.contains("only valid between record types"), "got: {}", err);
    }

    #[test]
    fn named_intersection_decl_inherits_sealed() {
        // type T = A & B; resolving `: T` must seal the merged object (named=sealed inherited).
        let mut env = TypeEnv::new();
        let mut a = IndexMap::new();
        a.insert("a".to_string(), Type::Int32);
        let mut b = IndexMap::new();
        b.insert("b".to_string(), Type::Int32);
        // The decl body, as the checker stores it: the resolved (unsealed) merged object.
        let mut merged = IndexMap::new();
        merged.insert("a".to_string(), Type::Int32);
        merged.insert("b".to_string(), Type::Int32);
        env.define_type("T".to_string(), vec![], Type::object(merged));
        let resolved = resolve_type(&TypeExpr::Named("T".to_string(), Span::dummy()), &env)
            .expect("T resolves");
        assert_eq!(sealed_of(&resolved), Some(true), "named intersection must resolve SEALED");
    }

    #[test]
    fn sealed_and_unsealed_are_equal_and_compatible() {
        // INVARIANT 2: the flag is invisible to Type equality. A sealed and an unsealed object
        // with identical fields compare EQUAL (manual PartialEq ignores `sealed`).
        let mut f = IndexMap::new();
        f.insert("x".to_string(), Type::Int32);
        let sealed = Type::sealed_object(f.clone());
        let unsealed = Type::object(f);
        assert_eq!(sealed, unsealed, "sealed must equal unsealed with same fields");
        // And structural compatibility (invariant 1) is symmetric and unaffected.
        assert!(crate::compat::is_compatible(&sealed, &unsealed));
        assert!(crate::compat::is_compatible(&unsealed, &sealed));
    }

    /// `resolve_type_spanned` must return the span of the offending leaf type-expression, not a
    /// dummy or enclosing span.  For `{ bestArrivals: Arrivals }` (an IndexSig whose key is the
    /// unknown type `bestArrivals`), the error span must equal the KEY span, not the outer IndexSig
    /// span.  This regression test pins that behaviour and also checks the new help-note field.
    #[test]
    fn resolve_type_spanned_points_at_offending_leaf() {
        // KEY_SPAN: a distinctive non-zero, non-dummy span representing the `bestArrivals` token.
        let key_span = Span::new(1, 10, 22);
        // VAL_SPAN: a different span for the value type (Arrivals) — unused in this test since the
        // key fails first, but kept distinct to prove the key span is what we get back.
        let val_span = Span::new(1, 24, 32);
        let outer_span = Span::new(1, 0, 40);

        // `{ bestArrivals: Arrivals }` — IndexSig with unknown key type "bestArrivals".
        let te = TypeExpr::IndexSig(
            Box::new(TypeExpr::Named("bestArrivals".into(), key_span)),
            Box::new(TypeExpr::Named("Arrivals".into(), val_span)),
            outer_span,
        );
        let env = TypeEnv::new();

        // resolve_type_spanned must return an Err whose span == KEY_SPAN (not val_span or
        // outer_span) and whose message mentions "bestArrivals".
        let err = resolve_type_spanned(&te, &env).unwrap_err();
        assert_eq!(err.0, key_span, "error span must point at the offending key leaf");
        assert!(
            err.1.contains("bestArrivals"),
            "error message must name the offending type; got: {}",
            err.1
        );

        // resolve_type (the backwards-compat shim) must still return the same message string.
        let plain_err = resolve_type(&te, &env).unwrap_err();
        assert_eq!(plain_err, err.1, "resolve_type must return the same message as resolve_type_spanned");
    }

    /// A lowercase bare key in an IndexSig (`{ foo: Bar }`) should carry a help note that
    /// mentions quoting the field key.  An uppercase bare key (`{ StopId: Time }`) that is
    /// simply unknown — a genuine type-alias typo — must NOT get the quoting hint, since the
    /// user most likely intended a type name.
    #[test]
    fn lowercase_bare_key_gets_quoting_hint() {
        let env = TypeEnv::new();
        let dummy = Span::dummy();

        // Lowercase: `{ foo: Bar }` — "foo" is almost certainly a mistakenly-unquoted field.
        let te_lower = TypeExpr::IndexSig(
            Box::new(TypeExpr::Named("foo".into(), dummy)),
            Box::new(TypeExpr::Named("Bar".into(), dummy)),
            dummy,
        );
        let err_lower = resolve_type_spanned(&te_lower, &env).unwrap_err();
        let help_lower = err_lower.2;
        assert!(
            help_lower.is_some(),
            "lowercase bare key must produce a help note"
        );
        let help_text = help_lower.unwrap();
        assert!(
            help_text.contains("quoted") || help_text.contains("quote"),
            "help must mention quoting; got: {}",
            help_text
        );
        assert!(
            help_text.contains("foo"),
            "help must name the offending identifier; got: {}",
            help_text
        );

        // Uppercase: `{ StopId: Time }` — "StopId" looks like a genuine type name, no hint.
        let te_upper = TypeExpr::IndexSig(
            Box::new(TypeExpr::Named("StopId".into(), dummy)),
            Box::new(TypeExpr::Named("Time".into(), dummy)),
            dummy,
        );
        let err_upper = resolve_type_spanned(&te_upper, &env).unwrap_err();
        assert_eq!(
            err_upper.2, None,
            "uppercase bare key must NOT produce a quoting hint; got: {:?}",
            err_upper.2
        );
    }

    #[test]
    fn named_record_display_shows_alias_name() {
        // type Date = { "year": Int64, "month": Int64, "day": Int64 }
        // Resolving `: Date` must produce a type whose Display is "Date", not the structural form.
        let mut fields = IndexMap::new();
        fields.insert("year".to_string(), Type::Int64);
        fields.insert("month".to_string(), Type::Int64);
        fields.insert("day".to_string(), Type::Int64);
        let mut env = TypeEnv::new();
        env.define_type("Date".to_string(), vec![], Type::object(fields));

        let date_ty = resolve_type(&TypeExpr::Named("Date".to_string(), Span::dummy()), &env)
            .expect("Date resolves");
        assert_eq!(date_ty.to_string(), "Date", "named record must display as alias name");

        // A function (Date) => UInt32 must display as "(Date) => UInt32"
        let fn_ty = Type::func(vec![date_ty.clone()], Type::UInt32);
        assert_eq!(fn_ty.to_string(), "(Date) => UInt32", "function with named param must show alias name");

        // Equality: a named Date object == equivalent anonymous object (name is inert)
        let mut anon_fields = IndexMap::new();
        anon_fields.insert("year".to_string(), Type::Int64);
        anon_fields.insert("month".to_string(), Type::Int64);
        anon_fields.insert("day".to_string(), Type::Int64);
        let anon = Type::object(anon_fields);
        assert_eq!(date_ty, anon, "named Date must equal anonymous object with same fields");

        // Compat: named Date is compatible with the anonymous structural type
        assert!(crate::compat::is_compatible(&date_ty, &anon));
        assert!(crate::compat::is_compatible(&anon, &date_ty));
    }
}

#[cfg(test)]
mod utility_type_tests {
    //! TypeScript-style utility-type operators + `keyof` + indexed-access. Each operator has a
    //! success case and (where it can fail) an error case. Tests drive the public resolver
    //! `resolve_type_spanned` over hand-built `TypeExpr`s.
    use super::*;
    use crate::env::TypeEnv;
    use lin_common::Span;
    use lin_parse::ast::TypeExpr as TE;

    fn dummy() -> Span {
        Span::dummy()
    }

    /// `type User = { "id": Int32, "name": String, "email": String | Null }` registered in env.
    fn env_with_user() -> TypeEnv {
        let mut fields = IndexMap::new();
        fields.insert("id".to_string(), Type::Int32);
        fields.insert("name".to_string(), Type::Str);
        fields.insert(
            "email".to_string(),
            Type::Union(vec![Type::Str, Type::Null]),
        );
        let mut env = TypeEnv::new();
        env.define_type("User".to_string(), vec![], Type::object(fields));
        env
    }

    fn named(n: &str) -> TE {
        TE::Named(n.to_string(), dummy())
    }
    fn strlit(s: &str) -> TE {
        TE::StringLit(s.to_string(), dummy())
    }
    fn generic(n: &str, args: Vec<TE>) -> TE {
        TE::Generic(n.to_string(), args, dummy())
    }

    fn obj_fields(ty: &Type) -> &IndexMap<String, Type> {
        match ty {
            Type::Object { fields, .. } => fields,
            other => panic!("expected Object, got {:?}", other),
        }
    }

    fn resolve(te: &TE, env: &TypeEnv) -> Type {
        resolve_type_spanned(te, env).expect("resolves")
    }

    #[test]
    fn partial_makes_fields_nullable_and_dedups() {
        let env = env_with_user();
        let ty = resolve(&generic("Partial", vec![named("User")]), &env);
        let f = obj_fields(&ty);
        assert_eq!(f["id"], Type::Union(vec![Type::Int32, Type::Null]));
        // already-nullable email must NOT gain a duplicate Null
        assert_eq!(f["email"], Type::Union(vec![Type::Str, Type::Null]));
    }

    #[test]
    fn required_strips_null() {
        let env = env_with_user();
        let ty = resolve(&generic("Required", vec![named("User")]), &env);
        let f = obj_fields(&ty);
        assert_eq!(f["id"], Type::Int32);
        // email: String | Null → String
        assert_eq!(f["email"], Type::Str);
    }

    #[test]
    fn pick_keeps_only_named_in_order() {
        let env = env_with_user();
        // Pick<User, "name" | "id"> — result keeps T's ORIGINAL order (id before name).
        let key = TE::Union(vec![strlit("name"), strlit("id")], dummy());
        let ty = resolve(&generic("Pick", vec![named("User"), key]), &env);
        let f = obj_fields(&ty);
        let keys: Vec<&String> = f.keys().collect();
        assert_eq!(keys, vec!["id", "name"]);
    }

    #[test]
    fn pick_unknown_key_errors_with_didyoumean() {
        let env = env_with_user();
        let err = resolve_type_spanned(&generic("Pick", vec![named("User"), strlit("naem")]), &env)
            .unwrap_err();
        assert!(err.1.contains("naem"), "msg: {}", err.1);
        let help = err.2.expect("help present");
        assert!(help.contains("name"), "help: {}", help);
    }

    #[test]
    fn omit_drops_named_and_is_lenient() {
        let env = env_with_user();
        // Omit<User, "email" | "nope"> — "nope" is absent from User but must NOT error.
        let key = TE::Union(vec![strlit("email"), strlit("nope")], dummy());
        let ty = resolve(&generic("Omit", vec![named("User"), key]), &env);
        let f = obj_fields(&ty);
        assert!(f.contains_key("id") && f.contains_key("name"));
        assert!(!f.contains_key("email"));
    }

    #[test]
    fn nonnullable_strips_null_union() {
        let env = TypeEnv::new();
        let arg = TE::Union(vec![named("String"), named("Null")], dummy());
        let ty = resolve(&generic("NonNullable", vec![arg]), &env);
        assert_eq!(ty, Type::Str);
    }

    #[test]
    fn nonnullable_of_null_is_never() {
        let env = TypeEnv::new();
        let ty = resolve(&generic("NonNullable", vec![named("Null")]), &env);
        assert_eq!(ty, Type::Never);
    }

    #[test]
    fn exclude_removes_members() {
        let mut env = TypeEnv::new();
        env.define_type(
            "Status".to_string(),
            vec![],
            Type::Union(vec![
                Type::StrLit("a".into()),
                Type::StrLit("b".into()),
                Type::StrLit("c".into()),
            ]),
        );
        let ty = resolve(&generic("Exclude", vec![named("Status"), strlit("b")]), &env);
        assert_eq!(
            ty,
            Type::Union(vec![Type::StrLit("a".into()), Type::StrLit("c".into())])
        );
    }

    #[test]
    fn exclude_empty_is_never() {
        let env = TypeEnv::new();
        let ty = resolve(&generic("Exclude", vec![strlit("a"), strlit("a")]), &env);
        assert_eq!(ty, Type::Never);
    }

    #[test]
    fn extract_keeps_only_matching() {
        let mut env = TypeEnv::new();
        env.define_type(
            "Status".to_string(),
            vec![],
            Type::Union(vec![
                Type::StrLit("a".into()),
                Type::StrLit("b".into()),
            ]),
        );
        let ty = resolve(&generic("Extract", vec![named("Status"), strlit("a")]), &env);
        assert_eq!(ty, Type::StrLit("a".into()));
    }

    #[test]
    fn return_type_of_function() {
        let env = TypeEnv::new();
        let f = TE::Function(vec![named("Int32")], Box::new(named("Boolean")), dummy());
        let ty = resolve(&generic("ReturnType", vec![f]), &env);
        assert_eq!(ty, Type::Bool);
    }

    #[test]
    fn return_type_non_function_errors() {
        let env = TypeEnv::new();
        let err = resolve_type_spanned(&generic("ReturnType", vec![named("Int32")]), &env)
            .unwrap_err();
        assert!(err.1.contains("function"), "msg: {}", err.1);
    }

    #[test]
    fn parameters_of_function() {
        let env = TypeEnv::new();
        let f = TE::Function(
            vec![named("Int32"), named("String")],
            Box::new(named("Boolean")),
            dummy(),
        );
        let ty = resolve(&generic("Parameters", vec![f]), &env);
        assert_eq!(ty, Type::FixedArray(vec![Type::Int32, Type::Str]));
    }

    #[test]
    fn record_string_key_is_map() {
        let env = TypeEnv::new();
        let ty = resolve(&generic("Record", vec![named("String"), named("Int32")]), &env);
        match ty {
            Type::Map { key, value, .. } => {
                assert_eq!(*key, Type::Str);
                assert_eq!(*value, Type::Int32);
            }
            other => panic!("expected Map, got {:?}", other),
        }
    }

    #[test]
    fn record_literal_union_key_is_object() {
        let env = TypeEnv::new();
        let key = TE::Union(vec![strlit("a"), strlit("b")], dummy());
        let ty = resolve(&generic("Record", vec![key, named("Boolean")]), &env);
        let f = obj_fields(&ty);
        assert_eq!(f["a"], Type::Bool);
        assert_eq!(f["b"], Type::Bool);
    }

    #[test]
    fn keyof_record_is_strlit_union() {
        let env = env_with_user();
        let ty = resolve(&TE::KeyOf(Box::new(named("User")), dummy()), &env);
        assert_eq!(
            ty,
            Type::Union(vec![
                Type::StrLit("id".into()),
                Type::StrLit("name".into()),
                Type::StrLit("email".into()),
            ])
        );
    }

    #[test]
    fn keyof_non_record_errors() {
        let env = TypeEnv::new();
        let err =
            resolve_type_spanned(&TE::KeyOf(Box::new(named("Int32")), dummy()), &env).unwrap_err();
        assert!(err.1.contains("keyof"), "msg: {}", err.1);
    }

    #[test]
    fn indexed_access_single_key() {
        let env = env_with_user();
        let te = TE::Index(Box::new(named("User")), Box::new(strlit("name")), dummy());
        let ty = resolve(&te, &env);
        assert_eq!(ty, Type::Str);
    }

    #[test]
    fn indexed_access_union_key() {
        let env = env_with_user();
        let key = TE::Union(vec![strlit("id"), strlit("name")], dummy());
        let te = TE::Index(Box::new(named("User")), Box::new(key), dummy());
        let ty = resolve(&te, &env);
        assert_eq!(ty, Type::Union(vec![Type::Int32, Type::Str]));
    }

    #[test]
    fn indexed_access_missing_field_errors() {
        let env = env_with_user();
        let te = TE::Index(Box::new(named("User")), Box::new(strlit("nope")), dummy());
        let err = resolve_type_spanned(&te, &env).unwrap_err();
        assert!(err.1.contains("nope"), "msg: {}", err.1);
        assert!(err.2.is_some(), "should have did-you-mean help");
    }

    #[test]
    fn user_shadows_builtin_record() {
        // A user `type Record = { … }` (non-generic) must win over the builtin in `Record<…>`
        // position — i.e. the builtin is only dispatched when the name is unshadowed. Here the
        // user defines a NON-generic `Record`, so `Record<String, Int32>` should fall through to
        // the normal generic-alias path and ERROR with an arity mismatch (0 params, 2 args),
        // NOT silently behave as the builtin.
        let mut env = TypeEnv::new();
        let mut f = IndexMap::new();
        f.insert("label".to_string(), Type::Str);
        env.define_type("Record".to_string(), vec![], Type::object(f));
        let err = resolve_type_spanned(
            &generic("Record", vec![named("String"), named("Int32")]),
            &env,
        )
        .unwrap_err();
        assert!(
            err.1.contains("argument") || err.1.contains("expects"),
            "user-shadowed Record must hit the alias arity error, got: {}",
            err.1
        );
    }

    #[test]
    fn arity_mismatch_errors() {
        let env = env_with_user();
        let err = resolve_type_spanned(&generic("Partial", vec![]), &env).unwrap_err();
        assert!(err.1.contains("exactly 1"), "msg: {}", err.1);
    }
}
