use lin_common::{Diagnostic, NumSuffix, Span};

use crate::typed_ir::*;
use crate::types::Type;
use crate::widen::widen_numeric;

/// The concrete numeric `Type` named by an explicit literal suffix (spec §3.6).
pub(crate) fn suffix_to_type(suffix: NumSuffix) -> Type {
    match suffix {
        NumSuffix::I8 => Type::Int8,
        NumSuffix::I16 => Type::Int16,
        NumSuffix::I32 => Type::Int32,
        NumSuffix::I64 => Type::Int64,
        NumSuffix::U8 => Type::UInt8,
        NumSuffix::U16 => Type::UInt16,
        NumSuffix::U32 => Type::UInt32,
        NumSuffix::U64 => Type::UInt64,
        NumSuffix::F32 => Type::Float32,
        NumSuffix::F64 => Type::Float64,
    }
}

/// The default type for a suffixless integer literal with no surrounding context (spec §26):
/// `Int32` when the value fits, otherwise the smallest type that PRESERVES it — `Int64`, or
/// `UInt64` for a decimal above `i64::MAX` (lexed as a negative i64 bit pattern). This avoids
/// the silent truncation a flat `Int32` default would cause for large literals; downstream
/// context (call args / operators) may still re-type the literal to a different width.
pub(crate) fn default_int_literal_type(v: i64) -> Type {
    if v >= i32::MIN as i64 && v <= i32::MAX as i64 {
        Type::Int32
    } else if v >= 0 {
        Type::Int64
    } else {
        // Negative: either a genuine negative that fits i64, or a decimal > i64::MAX stored as
        // a negative bit pattern. A literal source has no unary minus (spec §3.7) except the
        // parser's `0 - lit` desugar, so a bare negative IntLit here is the above-i64::MAX case.
        Type::UInt64
    }
}

/// Check that integer literal `v` fits `ty`'s range. `v` is the i64 bit pattern from the lexer
/// (a decimal > i64::MAX is stored as a negative pattern; reinterpret as u64 for unsigned
/// targets). Returns a range-error diagnostic at `span` when it doesn't fit.
pub(crate) fn check_int_literal_fits(v: i64, ty: &Type, span: Span) -> Result<(), Diagnostic> {
    if let Some((lo, hi)) = integer_range(ty) {
        let signed = v as i128;
        let fits = (signed >= lo && signed <= hi)
            || (!ty.is_signed() && {
                let unsigned = (v as u64) as i128;
                unsigned >= lo && unsigned <= hi
            });
        if !fits {
            let shown = if !ty.is_signed() && v < 0 {
                format!("{}", v as u64)
            } else {
                format!("{}", v)
            };
            return Err(Diagnostic::error(
                span,
                format!("literal {} is out of range for type {}", shown, ty),
            ));
        }
    }
    Ok(())
}

/// Collect TypeVar substitutions from matching `actual` against `pattern`.
/// E.g., matching `Iterator<Int32>` against `Iterator<TypeVar(9010)>` yields `9010 -> Int32`.
/// TypeVar(u32::MAX) is the special "any"/"Json" wildcard — never substituted.
pub(crate) fn collect_type_subs(pattern: &Type, actual: &Type, subs: &mut std::collections::HashMap<u32, Type>) {
    match (pattern, actual) {
        (Type::TypeVar(id), _) if *id == u32::MAX => {}  // Json wildcard: skip
        (Type::TypeVar(id), t) => { subs.insert(*id, t.clone()); }
        (Type::Array(pt), Type::Array(at)) => collect_type_subs(pt, at, subs),
        (Type::Array(pt), Type::FixedArray(ats)) => {
            for at in ats { collect_type_subs(pt, at, subs); }
        }
        // A generic `T[]` param unified against a `Json` value (the MAX wildcard): bind the
        // element TypeVar(s) to the Json wildcard so the function monomorphizes to a tagged
        // `$Json` instance (representation-consistent) rather than leaving `T` unbound. Same
        // for FixedArray / Iterator element holes (Gap 1).
        (Type::Array(pt), Type::TypeVar(id)) if *id == u32::MAX => {
            collect_type_subs(pt, &Type::TypeVar(u32::MAX), subs)
        }
        (Type::Iterator(pt), Type::TypeVar(id)) if *id == u32::MAX => {
            collect_type_subs(pt, &Type::TypeVar(u32::MAX), subs)
        }
        (Type::Iterator(pt), Type::Iterator(at)) => collect_type_subs(pt, at, subs),
        (Type::Shared(pt), Type::Shared(at)) => collect_type_subs(pt, at, subs),
        (Type::Union(pts), actual) => {
            for pt in pts { collect_type_subs(pt, actual, subs); }
        }
        (Type::Function { params: pp, ret: pr, .. }, Type::Function { params: ap, ret: ar, .. }) => {
            for (p, a) in pp.iter().zip(ap.iter()) { collect_type_subs(p, a, subs); }
            collect_type_subs(pr, ar, subs);
        }
        _ => {}
    }
}

/// Apply collected substitutions to a type.
pub(crate) fn apply_type_subs(ty: &Type, subs: &std::collections::HashMap<u32, Type>) -> Type {
    match ty {
        Type::TypeVar(id) => subs.get(id).cloned().unwrap_or_else(|| ty.clone()),
        Type::Array(t) => Type::Array(Box::new(apply_type_subs(t, subs))),
        Type::Iterator(t) => Type::Iterator(Box::new(apply_type_subs(t, subs))),
        Type::Shared(t) => Type::Shared(Box::new(apply_type_subs(t, subs))),
        Type::Union(ts) => Type::Union(ts.iter().map(|t| apply_type_subs(t, subs)).collect()),
        Type::Function { params, ret, required } => Type::Function {
            params: params.iter().map(|p| apply_type_subs(p, subs)).collect(),
            ret: Box::new(apply_type_subs(ret, subs)),
            required: *required,
        },
        _ => ty.clone(),
    }
}

/// Inclusive [min, max] range of values representable by an integer numeric type.
/// Returns None for non-integer types.
pub(crate) fn integer_range(ty: &Type) -> Option<(i128, i128)> {
    match ty {
        Type::Int8 => Some((i8::MIN as i128, i8::MAX as i128)),
        Type::Int16 => Some((i16::MIN as i128, i16::MAX as i128)),
        Type::Int32 => Some((i32::MIN as i128, i32::MAX as i128)),
        Type::Int64 => Some((i64::MIN as i128, i64::MAX as i128)),
        Type::UInt8 => Some((u8::MIN as i128, u8::MAX as i128)),
        Type::UInt16 => Some((u16::MIN as i128, u16::MAX as i128)),
        Type::UInt32 => Some((u32::MIN as i128, u32::MAX as i128)),
        Type::UInt64 => Some((u64::MIN as i128, u64::MAX as i128)),
        _ => None,
    }
}

/// Returns true if `ty` is definitely non-transferable across thread boundaries.
/// Non-transferable: Function, Iterator, Never.
/// TypeVar (unknown), Promise/Worker/ThreadPool (TypeVar-resolved), are not flagged —
/// we only reject types we can statically prove are non-transferable (spec §32.3).
pub(crate) fn is_definitely_non_transferable(ty: &Type) -> bool {
    match ty {
        Type::Function { .. } | Type::Iterator(_) | Type::Never => true,
        Type::Array(inner) => is_definitely_non_transferable(inner),
        Type::Union(ts) => ts.iter().any(is_definitely_non_transferable),
        _ => false,
    }
}

/// Returns true if `ty` is a legal FFI value type per spec §34.3.
/// Legal: Int8–Int64, UInt8–UInt64, Float32, Float64, Boolean, Null, String.
pub(crate) fn is_legal_ffi_value_type(ty: &Type) -> bool {
    matches!(ty,
        Type::Int8 | Type::Int16 | Type::Int32 | Type::Int64
        | Type::UInt8 | Type::UInt16 | Type::UInt32 | Type::UInt64
        | Type::Float32 | Type::Float64
        | Type::Bool | Type::Null | Type::Str
    )
}

/// Returns true if `ty` is a legal FFI binding type per spec §34.3.
/// The binding must be a function type whose params and return are legal value types.
pub(crate) fn is_legal_ffi_type(ty: &Type) -> bool {
    match ty {
        Type::Function { params, ret, .. } => {
            params.iter().all(is_legal_ffi_value_type) && is_legal_ffi_value_type(ret)
        }
        _ => false,
    }
}

/// Returns the name of the first mutable capture (or global var reference) found in a
/// directly-nested `TypedExpr::Function`, or `None` if there are none.
/// Does NOT recurse into inner functions.
pub(crate) fn first_mutable_capture(
    expr: &TypedExpr,
    mutable_globals: &std::collections::HashMap<usize, String>,
) -> Option<String> {
    match expr {
        TypedExpr::Function { captures, body, .. } => {
            // Check explicit captures (non-global vars captured from outer scope).
            if let Some(c) = captures.iter().find(|c| c.is_mutable) {
                return Some(c.name.clone());
            }
            // Check if the body references any mutable global slot.
            first_mutable_global_in_body(body, mutable_globals)
        }
        TypedExpr::MakeArray { elements, .. } => {
            elements.iter().find_map(|e| first_mutable_capture(e, mutable_globals))
        }
        _ => None,
    }
}

/// Walk a `TypedExpr` body looking for a `LocalGet` that references a mutable global slot.
/// Stops at nested function boundaries (does not recurse into `TypedExpr::Function`).
pub(crate) fn first_mutable_global_in_body(
    expr: &TypedExpr,
    mutable_globals: &std::collections::HashMap<usize, String>,
) -> Option<String> {
    match expr {
        TypedExpr::LocalGet { slot, .. } => mutable_globals.get(slot).cloned(),
        TypedExpr::LocalSet { slot, value, .. } => {
            mutable_globals.get(slot).cloned()
                .or_else(|| first_mutable_global_in_body(value, mutable_globals))
        }
        TypedExpr::Function { .. } => None, // don't recurse into nested functions
        TypedExpr::BinaryOp { left, right, .. } => {
            first_mutable_global_in_body(left, mutable_globals)
                .or_else(|| first_mutable_global_in_body(right, mutable_globals))
        }
        TypedExpr::UnaryOp { operand, .. } => {
            first_mutable_global_in_body(operand, mutable_globals)
        }
        TypedExpr::Call { func, args, .. } => {
            first_mutable_global_in_body(func, mutable_globals)
                .or_else(|| args.iter().find_map(|a| first_mutable_global_in_body(a, mutable_globals)))
        }
        TypedExpr::If { cond, then_br, else_br, .. } => {
            first_mutable_global_in_body(cond, mutable_globals)
                .or_else(|| first_mutable_global_in_body(then_br, mutable_globals))
                .or_else(|| first_mutable_global_in_body(else_br, mutable_globals))
        }
        TypedExpr::Block { stmts, expr, .. } => {
            stmts.iter().find_map(|s| match s {
                TypedStmt::Val { value, .. } | TypedStmt::Var { value, .. } => {
                    first_mutable_global_in_body(value, mutable_globals)
                }
                TypedStmt::Expr(e) => first_mutable_global_in_body(e, mutable_globals),
                _ => None,
            }).or_else(|| first_mutable_global_in_body(expr, mutable_globals))
        }
        TypedExpr::MakeObject { fields, spreads, .. } => {
            fields.iter().find_map(|(_, v)| first_mutable_global_in_body(v, mutable_globals))
                .or_else(|| spreads.iter().find_map(|s| first_mutable_global_in_body(s, mutable_globals)))
        }
        TypedExpr::MakeArray { elements, .. } => {
            elements.iter().find_map(|e| first_mutable_global_in_body(e, mutable_globals))
        }
        TypedExpr::Index { object, key, .. } => {
            first_mutable_global_in_body(object, mutable_globals)
                .or_else(|| first_mutable_global_in_body(key, mutable_globals))
        }
        TypedExpr::FieldGet { object, .. } => first_mutable_global_in_body(object, mutable_globals),
        TypedExpr::Match { scrutinee, arms, .. } => {
            first_mutable_global_in_body(scrutinee, mutable_globals)
                .or_else(|| arms.iter().find_map(|a| {
                    a.guard.as_ref().and_then(|g| first_mutable_global_in_body(g, mutable_globals))
                        .or_else(|| first_mutable_global_in_body(&a.body, mutable_globals))
                }))
        }
        TypedExpr::StringInterp { parts, .. } => {
            parts.iter().find_map(|p| match p {
                TypedStringPart::Expr(e) => first_mutable_global_in_body(e, mutable_globals),
                _ => None,
            })
        }
        _ => None,
    }
}

pub(crate) fn unify_types(types: &[Type]) -> Type {
    if types.is_empty() {
        return Type::Never;
    }
    if types.len() == 1 {
        return types[0].clone();
    }

    let first = &types[0];
    if types.iter().all(|t| t == first) {
        return first.clone();
    }

    // If all are numeric, widen
    if types.iter().all(|t| t.is_numeric()) {
        let mut result = types[0].clone();
        for t in &types[1..] {
            if let Some(widened) = widen_numeric(&result, t) {
                result = widened;
            }
        }
        return result;
    }

    Type::flatten_union(types.to_vec())
}
