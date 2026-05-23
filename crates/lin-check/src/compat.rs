use crate::types::Type;

/// Check if `value_type` is structurally compatible with `target_type`.
/// This implements the `has`-style compatibility used for function arguments and assignments.
pub fn is_compatible(value_type: &Type, target_type: &Type) -> bool {
    if value_type == target_type {
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
            variants.iter().all(|v| is_compatible(v, target))
        }

        // Union on the target side: value must be compatible with at least one variant
        (value, Type::Union(variants)) => {
            variants.iter().any(|v| is_compatible(value, v))
        }

        // Array covariance
        (Type::Array(a), Type::Array(b)) => is_compatible(a, b),

        // Fixed array to unbounded array
        (Type::FixedArray(elements), Type::Array(elem_ty)) => {
            elements.iter().all(|e| is_compatible(e, elem_ty))
        }

        // Fixed array positional compatibility
        (Type::FixedArray(a), Type::FixedArray(b)) => {
            a.len() == b.len()
                && a.iter().zip(b.iter()).all(|(av, bv)| is_compatible(av, bv))
        }

        // Object structural compatibility: value has all target fields with compatible types
        (Type::Object(value_fields), Type::Object(target_fields)) => {
            target_fields.iter().all(|(key, target_ty)| {
                value_fields
                    .get(key)
                    .map(|vt| is_compatible(vt, target_ty))
                    .unwrap_or(false)
            })
        }

        // Function compatibility: contravariant params, covariant return
        (
            Type::Function {
                params: vp,
                ret: vr,
            },
            Type::Function {
                params: tp,
                ret: tr,
            },
        ) => {
            if vp.len() != tp.len() {
                return false;
            }
            // Contravariant: target params must be compatible with value params
            let params_ok = vp
                .iter()
                .zip(tp.iter())
                .all(|(v, t)| is_compatible(t, v));
            // Covariant: value return must be compatible with target return
            let ret_ok = is_compatible(vr, tr);
            params_ok && ret_ok
        }

        // Iterator covariance
        (Type::Iterator(a), Type::Iterator(b)) => is_compatible(a, b),

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
