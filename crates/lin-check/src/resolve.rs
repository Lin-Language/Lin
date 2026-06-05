use indexmap::IndexMap;
use lin_parse::ast::TypeExpr;
use crate::env::TypeEnv;
use crate::types::Type;

pub fn resolve_type(type_expr: &TypeExpr, env: &TypeEnv) -> Result<Type, String> {
    resolve_type_inner(type_expr, env, &mut std::collections::HashSet::new())
}

fn resolve_type_inner(
    type_expr: &TypeExpr,
    env: &TypeEnv,
    visiting: &mut std::collections::HashSet<String>,
) -> Result<Type, String> {
    match type_expr {
        TypeExpr::Named(name, _span) => resolve_named_cycle(name, env, visiting),
        TypeExpr::Generic(name, args, _span) => {
            let resolved_args: Result<Vec<Type>, String> =
                args.iter().map(|a| resolve_type_inner(a, env, visiting)).collect();
            resolve_generic(name, &resolved_args?, env, visiting)
        }
        TypeExpr::Array(inner, _span) => {
            let inner_ty = resolve_type_inner(inner, env, visiting)?;
            Ok(Type::Array(Box::new(inner_ty)))
        }
        TypeExpr::FixedArray(types, _span) => {
            let resolved: Result<Vec<Type>, String> =
                types.iter().map(|t| resolve_type_inner(t, env, visiting)).collect();
            Ok(Type::FixedArray(resolved?))
        }
        TypeExpr::Union(types, _span) => {
            let resolved: Result<Vec<Type>, String> =
                types.iter().map(|t| resolve_type_inner(t, env, visiting)).collect();
            Ok(Type::flatten_union(resolved?))
        }
        TypeExpr::Function(params, ret, _span) => {
            let param_types: Result<Vec<Type>, String> =
                params.iter().map(|p| resolve_type_inner(p, env, visiting)).collect();
            let ret_type = resolve_type_inner(ret, env, visiting)?;
            // Type annotations cannot express default arguments, so every declared
            // parameter is required.
            Ok(Type::func(param_types?, ret_type))
        }
        TypeExpr::Object(fields, _span) => {
            // An object type spelled inline (anonymous structural shape, NOT a named record
            // declaration) is UNSEALED. Only the named-type unfold path (`expand_named_body` /
            // generic `substitute`) seals. See ADR-083.
            let mut resolved = IndexMap::new();
            for (key, type_expr) in fields {
                let ty = resolve_type_inner(type_expr, env, visiting)?;
                resolved.insert(key.clone(), ty);
            }
            Ok(Type::object(resolved))
        }
        TypeExpr::IndexSig(value, _span) => {
            // `{ String: T }` — a typed index-signature object type (ADR-082). Distinct from a
            // fixed record; backed by the hashed `LinMap` at runtime.
            let val_ty = resolve_type_inner(value, env, visiting)?;
            Ok(Type::Map(Box::new(val_ty)))
        }
        TypeExpr::TaggedUnion(variants, _span) => {
            let resolved: Result<Vec<Type>, String> =
                variants.iter().map(|t| resolve_type_inner(t, env, visiting)).collect();
            Ok(Type::flatten_union(resolved?))
        }
        TypeExpr::StringLit(s, _span) => Ok(Type::StrLit(s.clone())),
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
        "Json" => Ok(json_type()),
        // `Error` is the conventional error value (spec §20, §24.2.2) and a structural object
        // alias (ADR-047): an object carrying a `type` discriminant and a `message`. Both the
        // async runtime (on a caught thunk fault) and `fromJson` produce this shape — the
        // decode-error value additionally carries `"path"`, which width subtyping permits.
        // Modelled as an Object (not a new `Type` variant) so the ~20 exhaustive `Type` matches
        // don't change (cf. ADR-044); `is Error` is a field-presence + `"type" == "error"` check.
        "Error" => Ok(error_type()),
        // Function is an opaque type annotation — any arity is acceptable.
        // Params and ret use TypeVar(u32::MAX) so compat check treats it as accepting any function.
        "Function" => Ok(Type::func(
            vec![Type::TypeVar(u32::MAX)],
            Type::TypeVar(u32::MAX),
        )),
        // Iterator without type argument: use Json wildcard element type
        "Iterator" => Ok(Type::Iterator(Box::new(json_type()))),
        // Shared without a type argument: Shared<Json>. The opaque shared-mutable-state box
        // (ADR-044); only the shared/get/set/withLock accessors operate on it.
        "Shared" => Ok(Type::Shared(Box::new(json_type()))),
        // Stream without a type argument: Stream<Json>. The opaque pull-source (streams brief).
        // NOTE: the brief's locked decision was "not spellable in source"; we relaxed that to
        // EXACTLY the Shared precedent (a bare `Stream` annotation) so the trusted stdlib's thin
        // wrappers can annotate their `Stream` params (`(s: Stream)`) — the formatter mis-renders
        // an UNANNOTATED single param as an arg-position bare lambda, which is invalid at a
        // `val =` RHS. Opacity is unchanged: `compat.rs` still forbids any non-stream op on a
        // `Stream`, so naming it buys a user nothing but the type itself.
        "Stream" => Ok(Type::Stream(Box::new(json_type()))),
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
                    Ok(expanded)
                } else {
                    Err(format!(
                        "Type '{}' requires {} type argument(s)",
                        name,
                        decl.params.len()
                    ))
                }
            } else if name == "Number" {
                // `Number` is a parameter/return CONSTRAINT (a numerically-bounded generic,
                // ADR-018), not a value type — it only lowers to a bounded var in a function
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
        Type::Object { fields, sealed: _ } => {
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
        Type::Function { params, ret, required } => Ok(Type::Function {
            params: params.iter().map(|p| expand_named_body(p, env, visiting)).collect::<Result<_, _>>()?,
            ret: Box::new(expand_named_body(ret, env, visiting)?),
            required: *required,
        }),
        Type::Iterator(inner) => Ok(Type::Iterator(Box::new(expand_named_body(inner, env, visiting)?))),
        Type::Stream(inner) => Ok(Type::Stream(Box::new(expand_named_body(inner, env, visiting)?))),
        Type::Map(v) => Ok(Type::Map(Box::new(expand_named_body(v, env, visiting)?))),
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
        Type::Object { fields, sealed } => {
            // Generic named-type instantiation (`type Box<T> = { value: T }` → `Box<Int32>`) is
            // also a named record unfold: preserve the declaration's sealed-ness. Bodies of named
            // record types arrive here sealed (set when the decl body was first resolved).
            let substituted: Result<IndexMap<String, Type>, String> = fields
                .iter()
                .map(|(k, v)| substitute(v, params, args, env, visiting).map(|t| (k.clone(), t)))
                .collect();
            Ok(Type::Object { fields: substituted?, sealed: *sealed })
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
        } => Ok(Type::Function {
            params: fn_params
                .iter()
                .map(|t| substitute(t, params, args, env, visiting))
                .collect::<Result<_, _>>()?,
            ret: Box::new(substitute(ret, params, args, env, visiting)?),
            required: *required,
        }),
        Type::Iterator(inner) => Ok(Type::Iterator(Box::new(substitute(inner, params, args, env, visiting)?))),
        Type::Stream(inner) => Ok(Type::Stream(Box::new(substitute(inner, params, args, env, visiting)?))),
        Type::Map(v) => Ok(Type::Map(Box::new(substitute(v, params, args, env, visiting)?))),
        _ => Ok(ty.clone()),
    }
}

/// The structural shape of a decode `Error` (ADR-047). An open object with the two stable
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

pub fn json_type() -> Type {
    // Json is the open dynamic type: any JSON-compatible value.
    // We use TypeVar(u32::MAX) as a special "any" marker that is_compatible always accepts.
    // This allows object literals, arrays, strings, numbers, bools, null to all satisfy Json.
    Type::TypeVar(u32::MAX)
}

#[cfg(test)]
mod sealed_marker_tests {
    //! Stage 0.5 focused marker tests (ADR-083): prove the `sealed` flag is
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
}
