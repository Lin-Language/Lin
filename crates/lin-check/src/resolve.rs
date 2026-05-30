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
            let mut resolved = IndexMap::new();
            for (key, type_expr) in fields {
                let ty = resolve_type_inner(type_expr, env, visiting)?;
                resolved.insert(key.clone(), ty);
            }
            Ok(Type::Object(resolved))
        }
        TypeExpr::TaggedUnion(variants, _span) => {
            let resolved: Result<Vec<Type>, String> =
                variants.iter().map(|t| resolve_type_inner(t, env, visiting)).collect();
            Ok(Type::flatten_union(resolved?))
        }
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
        "String" => Ok(Type::Str),
        "Json" => Ok(json_type()),
        // `Error` is the conventional error value (spec §19, §32.2.2): an object carrying a
        // `type` discriminant and a `message`. The async runtime produces exactly this shape
        // (`{ "type": "error", "message": String }`) when a thunk faults. It has no special
        // control-flow behaviour — `is Error` is a structural shape check on those fields.
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
        Type::Object(fields) => {
            let mut out = IndexMap::new();
            for (k, v) in fields {
                out.insert(k.clone(), expand_named_body(v, env, visiting)?);
            }
            Ok(Type::Object(out))
        }
        Type::Function { params, ret, required } => Ok(Type::Function {
            params: params.iter().map(|p| expand_named_body(p, env, visiting)).collect::<Result<_, _>>()?,
            ret: Box::new(expand_named_body(ret, env, visiting)?),
            required: *required,
        }),
        Type::Iterator(inner) => Ok(Type::Iterator(Box::new(expand_named_body(inner, env, visiting)?))),
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
        Type::Object(fields) => {
            let substituted: Result<IndexMap<String, Type>, String> = fields
                .iter()
                .map(|(k, v)| substitute(v, params, args, env, visiting).map(|t| (k.clone(), t)))
                .collect();
            Ok(Type::Object(substituted?))
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
        _ => Ok(ty.clone()),
    }
}

pub fn json_type() -> Type {
    // Json is the open dynamic type: any JSON-compatible value.
    // We use TypeVar(u32::MAX) as a special "any" marker that is_compatible always accepts.
    // This allows object literals, arrays, strings, numbers, bools, null to all satisfy Json.
    Type::TypeVar(u32::MAX)
}

/// The built-in `Error` type: `{ "type": String, "message": String }` — the conventional
/// error value, and the exact shape the async runtime builds on a caught thunk fault. Modelled
/// structurally so `is Error` is a field-presence check and `Error` composes in unions
/// (`T | Error`). Field values are `String` (`type` is the discriminant, `message` the text).
pub fn error_type() -> Type {
    let mut fields = IndexMap::new();
    fields.insert("type".to_string(), Type::Str);
    fields.insert("message".to_string(), Type::Str);
    Type::Object(fields)
}
