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
            let resolved_args: Result<Vec<Type>, (Span, String, Option<String>)> =
                args.iter().map(|a| resolve_type_inner(a, env, visiting)).collect();
            resolve_generic(name, &resolved_args?, env, visiting).map_err(|m| (*span, m, None))
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
        // TarEntry — opaque, generation-stamped archive entry handle. Not generic.
        "TarEntry" => Ok(Type::TarEntry),
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
fn expand_named_body(
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
