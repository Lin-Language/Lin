use lin_common::{Diagnostic, NumSuffix, Span};
use lin_parse::ast::Expr;

use crate::typed_ir::*;
use crate::types::Type;
use crate::widen::widen_numeric;

/// The concrete numeric `Type` named by an explicit literal suffix (spec §2.6).
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

/// The default type for a suffixless integer literal with no surrounding context (spec §21):
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
        // a negative bit pattern. A literal source has no unary minus (spec §2.7) except the
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
/// TypeVar(u32::MAX) is the special `AnyVal` wildcard — never substituted.
pub(crate) fn collect_type_subs(pattern: &Type, actual: &Type, subs: &mut std::collections::HashMap<u32, Type>) {
    match (pattern, actual) {
        (Type::TypeVar(id), _) if *id == u32::MAX => {}  // AnyVal wildcard: skip
        (Type::TypeVar(id), t) => { subs.insert(*id, t.clone()); }
        (Type::Array(pt), Type::Array(at)) => collect_type_subs(pt, at, subs),
        (Type::Array(pt), Type::FixedArray(ats)) => {
            for at in ats { collect_type_subs(pt, at, subs); }
        }
        // Positional tuple unification: a `[String, T]` param against a `[String, Int32]` actual
        // binds `T = Int32` element-by-element. Needed for `fromEntries(pairs: [String, T][])` and
        // the keyed-pair builders, whose type parameter is nested inside a fixed-array (tuple) shape.
        // (The actual is a FixedArray only when the arg was routed through expected-type-directed
        // checking against the tuple param — see the tuple-literal path in `call.rs`.)
        (Type::FixedArray(pts), Type::FixedArray(ats)) => {
            for (pt, at) in pts.iter().zip(ats.iter()) { collect_type_subs(pt, at, subs); }
        }
        // A generic `T[]` param unified against an `AnyVal` value (the MAX wildcard): bind the
        // element TypeVar(s) to the AnyVal wildcard so the function monomorphizes to a tagged
        // `$AnyVal` instance (representation-consistent) rather than leaving `T` unbound. Same
        // for FixedArray / Iterator element holes (Gap 1).
        (Type::Array(pt), Type::TypeVar(id)) if *id == u32::MAX => {
            collect_type_subs(pt, &Type::TypeVar(u32::MAX), subs)
        }
        (Type::Iterator(pt), Type::TypeVar(id)) if *id == u32::MAX => {
            collect_type_subs(pt, &Type::TypeVar(u32::MAX), subs)
        }
        (Type::Iterator(pt), Type::Iterator(at)) => collect_type_subs(pt, at, subs),
        // An `Iterable`-shaped generic param `T[]` is routinely applied to a runtime ITERATOR
        // (e.g. `range(0,n): Iterator<Int32>` then `.map(T[], …)`). Cross-unify the element through
        // the Array↔Iterator boundary so `T` binds to the element type (Int32), and the callback
        // lambda is type-checked at the CONCRETE element type rather than defaulting to AnyVal. Without
        // this the lambda param is AnyVal → a per-element box/unbox even on the inlined flat path
        // (mirrors `lin-ir`'s monomorphize `collect_subs`).
        (Type::Array(pt), Type::Iterator(at)) => collect_type_subs(pt, at, subs),
        (Type::Iterator(pt), Type::Array(at)) => collect_type_subs(pt, at, subs),
        (Type::Iterator(pt), Type::FixedArray(ats)) => {
            for at in ats { collect_type_subs(pt, at, subs); }
        }
        (Type::Shared(pt), Type::Shared(at)) => collect_type_subs(pt, at, subs),
        (Type::Stream(pt), Type::Stream(at)) => collect_type_subs(pt, at, subs),
        (Type::Promise(pt), Type::Promise(at)) => collect_type_subs(pt, at, subs),
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
        Type::Stream(t) => Type::Stream(Box::new(apply_type_subs(t, subs))),
        Type::Promise(t) => Type::Promise(Box::new(apply_type_subs(t, subs))),
        Type::Map(t) => Type::Map(Box::new(apply_type_subs(t, subs))),
        // Substituting a union's members can DUPLICATE or collapse it: `<T, D>(…): T | D` with
        // `T = D` (e.g. `at(ints, i, 0)` over `Int32[]`, both members `Int32`) naively becomes the
        // degenerate `Int32 | Int32`. `flatten_union` dedups members and collapses a singleton to
        // the bare type, so the call-site result is the clean `Int32` a definitely-present read
        // should have (usable directly in arithmetic) — matching the monomorphizer's `subst_type`.
        Type::Union(ts) => {
            Type::flatten_union(ts.iter().map(|t| apply_type_subs(t, subs)).collect())
        }
        Type::Function { params, ret, required, lset } => Type::Function {
            params: params.iter().map(|p| apply_type_subs(p, subs)).collect(),
            ret: Box::new(apply_type_subs(ret, subs)),
            required: *required,
            lset: lset.clone(),
        },
        // Record/object fields can hold type parameters (`type Box<T> = { value: T }`,
        // `type Result<T,E> = { value: T } | { error: E }`). Without recursing here, a
        // generic call whose RESULT type-parameter only appears inside an object field
        // (the `value: U` of `mapOk(...): Result<U,E>`) leaves that field's TypeVar
        // unsubstituted even when `subs` binds it — so the call result stays
        // `{ value: ?U } | …` and an index of it degrades to `?U | Null`. Substitute
        // through the fields, preserving the sealed marker.
        Type::Object { fields, sealed } => Type::Object {
            fields: fields.iter().map(|(k, v)| (k.clone(), apply_type_subs(v, subs))).collect(),
            sealed: *sealed,
        },
        Type::FixedArray(elems) => {
            Type::FixedArray(elems.iter().map(|t| apply_type_subs(t, subs)).collect())
        }
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
/// Non-transferable: Function, Iterator, Stream, TarEntry, Never.
/// TypeVar (unknown), Promise/Worker/ThreadPool (TypeVar-resolved), are not flagged —
/// we only reject types we can statically prove are non-transferable (spec §24.3).
/// `Stream` owns an OS resource, so it can never be COPIED across a thread boundary; it crosses
/// only by MOVE (CAP_MOVE, Stage 7), which the transfer-copy path must never attempt (brief §9).
/// `TarEntry` shares a live cursor into the parent stream — it is neither deep-copyable nor
/// movable across a thread boundary.
pub(crate) fn is_definitely_non_transferable(ty: &Type) -> bool {
    match ty {
        Type::Function { .. } | Type::Iterator(_) | Type::Stream(_) | Type::TarEntry | Type::Never => true,
        Type::Array(inner) => is_definitely_non_transferable(inner),
        Type::Union(ts) => ts.iter().any(is_definitely_non_transferable),
        _ => false,
    }
}

/// Detects an evidence-free empty collection literal — an `[]` or `{}` (object literal with no
/// fields and no spreads) — used in a position where there is NO contextual type to fix its
/// element/value type. Such a literal otherwise infers a degenerate type (`Array(Never)` /
/// empty record `{}`) that silently misbehaves and cannot be checked against pushes, so we
/// require an explicit annotation instead (ADR-058). Returns the kind of empty literal so the
/// caller can phrase a tailored error, or `None` if the expression is not an evidence-free empty.
///
/// Scope is deliberately syntactic and narrow: only a *bare* empty literal triggers it. A literal
/// with contents (`[1]`, `{ "a": 1 }`) carries its own element evidence; an empty literal in a
/// context that supplies an expected type never reaches this check (those go through
/// `check_expr`, not `infer_expr`).
pub(crate) fn empty_literal_kind(expr: &Expr) -> Option<EmptyLiteralKind> {
    match expr {
        Expr::Array(elements, _, _) if elements.is_empty() => Some(EmptyLiteralKind::Array),
        Expr::Object(fields, _, _) if fields.is_empty() => Some(EmptyLiteralKind::Object),
        _ => None,
    }
}

pub(crate) enum EmptyLiteralKind {
    Array,
    Object,
}

impl EmptyLiteralKind {
    /// The diagnostic message for an evidence-free empty literal of this kind.
    pub(crate) fn message(&self) -> &'static str {
        match self {
            EmptyLiteralKind::Array => {
                "cannot infer the element type of an empty array literal; add a type annotation, \
                 e.g. `val xs: Int32[] = []`"
            }
            EmptyLiteralKind::Object => {
                "cannot infer the value type of an empty map/object literal; add a type \
                 annotation, e.g. `val m: { String: Int32 } = {}`"
            }
        }
    }
}

/// Returns true if `ty` is a legal FFI value type per spec §26.3.
/// Legal: Int8–Int64, UInt8–UInt64, Float32, Float64, Boolean, Null, String.
pub(crate) fn is_legal_ffi_value_type(ty: &Type) -> bool {
    matches!(ty,
        Type::Int8 | Type::Int16 | Type::Int32 | Type::Int64
        | Type::UInt8 | Type::UInt16 | Type::UInt32 | Type::UInt64
        | Type::Float32 | Type::Float64
        | Type::Bool | Type::Null | Type::Str
    )
}

/// Returns true if `ty` is a legal FFI binding type per spec §26.3.
/// The binding must be a function type whose params and return are legal value types.
pub(crate) fn is_legal_ffi_type(ty: &Type) -> bool {
    match ty {
        Type::Function { params, ret, .. } => {
            params.iter().all(is_legal_ffi_value_type) && is_legal_ffi_value_type(ret)
        }
        _ => false,
    }
}

/// Returns true if a *captured* value of this type is unsafe to transfer to a worker thread.
///
/// Narrower than `is_definitely_non_transferable`: `Function` values are intentionally excluded
/// here (they are safe to copy across threads — closures over immutable vals are purely
/// referentially transparent). `Stream` is excluded because it crosses thread boundaries legally
/// via CAP_MOVE. Only opaque cursor-sharing handles (`TarEntry`, `Iterator`) are rejected at the
/// capture site.
fn is_non_transferable_capture_ty(ty: &Type) -> bool {
    match ty {
        Type::TarEntry | Type::Iterator(_) => true,
        Type::Array(inner) => is_non_transferable_capture_ty(inner),
        Type::Union(ts) => ts.iter().any(is_non_transferable_capture_ty),
        _ => false,
    }
}

/// Returns the name and type of the first captured binding in a directly-nested
/// `TypedExpr::Function` whose type is non-transferable (i.e. `TarEntry` or `Iterator`), or
/// `None` if there are none. Used by the async-thunk capture gate.
///
/// Note: `Stream` and `Function` captures are intentionally NOT rejected here — streams cross
/// thread boundaries by MOVE (CAP_MOVE); functions are pure value types safe to copy across
/// threads. This helper only fires for the opaque cursor-sharing types (`TarEntry` primarily).
///
/// Does NOT recurse into inner functions (same scope restriction as `first_mutable_capture`).
pub(crate) fn first_non_transferable_capture(
    expr: &TypedExpr,
) -> Option<(String, Type)> {
    match expr {
        TypedExpr::Function { captures, .. } => {
            captures.iter().find_map(|c| {
                if is_non_transferable_capture_ty(&c.ty) {
                    Some((c.name.clone(), c.ty.clone()))
                } else {
                    None
                }
            })
        }
        TypedExpr::MakeArray { elements, .. } => {
            elements.iter().find_map(first_non_transferable_capture)
        }
        _ => None,
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
