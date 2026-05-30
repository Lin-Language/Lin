use crate::env::TypeEnv;
use crate::types::Type;

/// Check if `value_type` is structurally compatible with `target_type`.
/// This implements the `has`-style compatibility used for function arguments and assignments.
/// Named types are not unfolded here; use `is_compatible_env` when an env is available.
pub fn is_compatible(value_type: &Type, target_type: &Type) -> bool {
    is_compatible_env(value_type, target_type, None, &mut 0)
}

/// Env-aware compatibility check that can unfold Named types one level.
pub fn is_compatible_env(
    value_type: &Type,
    target_type: &Type,
    env: Option<&TypeEnv>,
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
                    let result = is_compatible_env(&decl.body.clone(), target_type, Some(env), depth);
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
                    let result = is_compatible_env(value_type, &decl.body.clone(), Some(env), depth);
                    *depth -= 1;
                    return result;
                }
            }
        }
        return true;
    }

    match (value_type, target_type) {
        (_, Type::TypeVar(_)) | (Type::TypeVar(_), _) => true,

        (Type::Never, _) => true,
        (_, Type::Never) => false,

        // Numeric widening: narrower assignable to wider
        (a, b) if a.is_numeric() && b.is_numeric() => is_numeric_compatible(a, b),

        // Union on the value side: every variant must be compatible with target
        (Type::Union(variants), target) => {
            variants.iter().all(|v| is_compatible_env(v, target, env, depth))
        }

        // Union on the target side: value must be compatible with at least one variant
        (value, Type::Union(variants)) => {
            variants.iter().any(|v| is_compatible_env(value, v, env, depth))
        }

        // Array covariance
        (Type::Array(a), Type::Array(b)) => is_compatible_env(a, b, env, depth),

        // Fixed array to unbounded array
        (Type::FixedArray(elements), Type::Array(elem_ty)) => {
            elements.iter().all(|e| is_compatible_env(e, elem_ty, env, depth))
        }

        // Fixed array positional compatibility
        (Type::FixedArray(a), Type::FixedArray(b)) => {
            a.len() == b.len()
                && a.iter().zip(b.iter()).all(|(av, bv)| is_compatible_env(av, bv, env, depth))
        }

        // Object structural compatibility: value has all target fields with compatible types.
        // A missing field is allowed when the target field type includes Null.
        (Type::Object(value_fields), Type::Object(target_fields)) => {
            target_fields.iter().all(|(key, target_ty)| {
                match value_fields.get(key) {
                    Some(vt) => is_compatible_env(vt, target_ty, env, depth),
                    None => is_compatible_env(&Type::Null, target_ty, env, depth),
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
            if vp.len() != tp.len() {
                return false;
            }
            // Contravariant: target params must be compatible with value params
            let params_ok = vp
                .iter()
                .zip(tp.iter())
                .all(|(v, t)| is_compatible_env(t, v, env, depth));
            // Covariant: value return must be compatible with target return
            let ret_ok = is_compatible_env(vr, tr, env, depth);
            params_ok && ret_ok
        }

        // Iterator covariance
        (Type::Iterator(a), Type::Iterator(b)) => is_compatible_env(a, b, env, depth),

        _ => false,
    }
}

#[allow(dead_code)]
pub fn is_exact_match(value_type: &Type, target_type: &Type) -> bool {
    if value_type == target_type {
        return true;
    }

    match (value_type, target_type) {
        (Type::Object(value_fields), Type::Object(target_fields)) => {
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
