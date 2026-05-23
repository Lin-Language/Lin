use indexmap::IndexMap;
use lin_parse::ast::TypeExpr;
use crate::env::TypeEnv;
use crate::types::Type;

pub fn resolve_type(type_expr: &TypeExpr, env: &TypeEnv) -> Result<Type, String> {
    match type_expr {
        TypeExpr::Named(name, _span) => resolve_named(name, env),
        TypeExpr::Generic(name, args, _span) => {
            let resolved_args: Result<Vec<Type>, String> =
                args.iter().map(|a| resolve_type(a, env)).collect();
            resolve_generic(name, &resolved_args?, env)
        }
        TypeExpr::Array(inner, _span) => {
            let inner_ty = resolve_type(inner, env)?;
            Ok(Type::Array(Box::new(inner_ty)))
        }
        TypeExpr::FixedArray(types, _span) => {
            let resolved: Result<Vec<Type>, String> =
                types.iter().map(|t| resolve_type(t, env)).collect();
            Ok(Type::FixedArray(resolved?))
        }
        TypeExpr::Union(types, _span) => {
            let resolved: Result<Vec<Type>, String> =
                types.iter().map(|t| resolve_type(t, env)).collect();
            Ok(Type::flatten_union(resolved?))
        }
        TypeExpr::Function(params, ret, _span) => {
            let param_types: Result<Vec<Type>, String> =
                params.iter().map(|p| resolve_type(p, env)).collect();
            let ret_type = resolve_type(ret, env)?;
            Ok(Type::Function {
                params: param_types?,
                ret: Box::new(ret_type),
            })
        }
        TypeExpr::Object(fields, _span) => {
            let mut resolved = IndexMap::new();
            for (key, type_expr) in fields {
                let ty = resolve_type(type_expr, env)?;
                resolved.insert(key.clone(), ty);
            }
            Ok(Type::Object(resolved))
        }
        TypeExpr::TaggedUnion(variants, _span) => {
            let resolved: Result<Vec<Type>, String> =
                variants.iter().map(|t| resolve_type(t, env)).collect();
            Ok(Type::flatten_union(resolved?))
        }
    }
}

fn resolve_named(name: &str, env: &TypeEnv) -> Result<Type, String> {
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
        _ => {
            if let Some(decl) = env.lookup_type(name) {
                if decl.params.is_empty() {
                    Ok(decl.body.clone())
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

fn resolve_generic(name: &str, args: &[Type], env: &TypeEnv) -> Result<Type, String> {
    match name {
        "Iterator" => {
            if args.len() != 1 {
                return Err("Iterator takes exactly 1 type argument".to_string());
            }
            Ok(Type::Iterator(Box::new(args[0].clone())))
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
                Ok(substitute(&decl.body, &decl.params, args))
            } else {
                Err(format!("Unknown generic type '{}'", name))
            }
        }
    }
}

fn substitute(ty: &Type, params: &[String], args: &[Type]) -> Type {
    match ty {
        Type::Object(fields) => {
            let substituted = fields
                .iter()
                .map(|(k, v)| (k.clone(), substitute(v, params, args)))
                .collect();
            Type::Object(substituted)
        }
        Type::Array(inner) => Type::Array(Box::new(substitute(inner, params, args))),
        Type::FixedArray(types) => {
            Type::FixedArray(types.iter().map(|t| substitute(t, params, args)).collect())
        }
        Type::Union(types) => {
            Type::Union(types.iter().map(|t| substitute(t, params, args)).collect())
        }
        Type::Function {
            params: fn_params,
            ret,
        } => Type::Function {
            params: fn_params
                .iter()
                .map(|t| substitute(t, params, args))
                .collect(),
            ret: Box::new(substitute(ret, params, args)),
        },
        Type::Iterator(inner) => Type::Iterator(Box::new(substitute(inner, params, args))),
        _ => {
            // Check if this is a type parameter reference by name
            // This works because we store type params as Named types before resolution
            ty.clone()
        }
    }
}

pub fn json_type() -> Type {
    // Json is the open dynamic type: any JSON-compatible value.
    // We use TypeVar(u32::MAX) as a special "any" marker that is_compatible always accepts.
    // This allows object literals, arrays, strings, numbers, bools, null to all satisfy Json.
    Type::TypeVar(u32::MAX)
}
