/// Exhaustiveness checking for `match` expressions.
///
/// Implements the core of Maranget's (2008) matrix-decomposition approach for
/// Lin's concrete match patterns:
///
/// - `is Type`  — tag check against a closed union
/// - `is Null`  — null check
/// - `else`     — wildcard / catch-all
/// - Literal    — equality check (always partial unless combined with else)
/// - `has { }` patterns — structural checks (always partial unless combined with else)
///
/// For a `match` on a `Union(variants)`:
///   - The match is exhaustive if every variant type appears in at least one `Is(TypeCheck)` arm,
///     OR if an `else` arm is present.
///   - If the match is non-exhaustive, we report a counterexample: one of the uncovered variants.
///
/// For a `match` on a non-union type with no `else` arm:
///   - If ALL arms are `Is` / `has` / literal patterns (i.e. there is no catch-all),
///     we emit a warning. We cannot prove completeness without an `else`.
use lin_common::{Diagnostic, Span};
use crate::typed_ir::{TypedMatchArm, TypedMatchPattern, TypedPattern};
use crate::types::Type;

/// Check the arms of a `match` expression for exhaustiveness.
///
/// Returns a list of `Diagnostic`s (errors for missing mandatory coverage on closed unions,
/// warnings for potentially-partial matches on open types).
pub fn check_exhaustiveness(
    scrutinee_ty: &Type,
    arms: &[TypedMatchArm],
    span: Span,
) -> Vec<Diagnostic> {
    let has_else = arms.iter().any(|a| matches!(a.pattern, TypedMatchPattern::Else));

    // An `else` arm makes the match trivially exhaustive.
    if has_else {
        return Vec::new();
    }

    match scrutinee_ty {
        Type::Union(variants) => check_union_exhaustiveness(variants, arms, span),
        Type::Bool => check_bool_exhaustiveness(arms, span),
        _ => {
            // For non-union types, only warn if there are pattern arms that could miss.
            // We cannot prove completeness without an else arm or full type coverage.
            let all_patterns_are_is = arms.iter().all(|a| {
                matches!(a.pattern,
                    TypedMatchPattern::Is(_) |
                    TypedMatchPattern::Has(_)
                )
            });
            if all_patterns_are_is && !arms.is_empty() {
                vec![Diagnostic::warning(
                    span,
                    "match may be non-exhaustive: no `else` arm. Add `else => ...` to handle unmatched cases.",
                )]
            } else {
                Vec::new()
            }
        }
    }
}

/// Check that every variant in the union is covered by an `Is(TypeCheck)` arm.
fn check_union_exhaustiveness(
    variants: &[Type],
    arms: &[TypedMatchArm],
    span: Span,
) -> Vec<Diagnostic> {
    // Collect all types that are explicitly covered by `is T` arms.
    let covered: Vec<&Type> = arms.iter().filter_map(|a| {
        // `TypeCheckDeep` (ADR-053, `is <ObjectType>`) covers its variant exactly like `TypeCheck`.
        match &a.pattern {
            TypedMatchPattern::Is(TypedPattern::TypeCheck(ty, _)) => Some(ty),
            TypedMatchPattern::Is(TypedPattern::TypeCheckDeep(ty, _, _)) => Some(ty),
            _ => None,
        }
    }).collect();

    // `is Error` desugars to a value-constrained object pattern `{ "type": "error", .. }`
    // (ADR-047), so it is NOT a `TypeCheck` arm. Recognise it here so it counts as covering the
    // `Error` object variant of a `T | Error` union; otherwise a `match | is T | is Error`
    // would be reported non-exhaustive.
    let covers_error = arms.iter().any(|a| {
        matches!(&a.pattern, TypedMatchPattern::Is(p) if is_error_pattern(p))
    });

    // Find union members not covered by any arm.
    let missing: Vec<&Type> = variants.iter().filter(|v| {
        if covers_error && is_error_variant(v) {
            return false;
        }
        !covered.iter().any(|c| types_overlap(c, v))
    }).collect();

    if missing.is_empty() {
        return Vec::new();
    }

    // Build a witness string: the first uncovered variant.
    let witness = missing.iter()
        .map(|t| format!("{}", t))
        .collect::<Vec<_>>()
        .join(" | ");

    vec![Diagnostic::error(
        span,
        format!(
            "non-exhaustive match: the following types are not covered: {}. Add an `is {}` arm or an `else` arm.",
            witness, missing[0]
        ),
    ).with_help(format!(
        "Add `is {} => ...` or a catch-all `else => ...` arm.",
        missing[0]
    ))]
}

/// Check that both `true` and `false` are covered in a Boolean match.
fn check_bool_exhaustiveness(arms: &[TypedMatchArm], span: Span) -> Vec<Diagnostic> {
    let covers_true = arms.iter().any(|a| {
        if let TypedMatchPattern::Is(TypedPattern::Literal(lit)) = &a.pattern {
            matches!(lit.as_ref(), crate::typed_ir::TypedExpr::BoolLit(true, _))
        } else {
            false
        }
    });
    let covers_false = arms.iter().any(|a| {
        if let TypedMatchPattern::Is(TypedPattern::Literal(lit)) = &a.pattern {
            matches!(lit.as_ref(), crate::typed_ir::TypedExpr::BoolLit(false, _))
        } else {
            false
        }
    });

    match (covers_true, covers_false) {
        (true, true) => Vec::new(),
        (false, true) => vec![Diagnostic::error(span, "non-exhaustive Boolean match: `true` is not covered.")
            .with_help("Add `is true => ...` or an `else` arm.")],
        (true, false) => vec![Diagnostic::error(span, "non-exhaustive Boolean match: `false` is not covered.")
            .with_help("Add `is false => ...` or an `else` arm.")],
        (false, false) => vec![Diagnostic::warning(span, "match may be non-exhaustive: no `else` arm.")],
    }
}

/// True if `p` is the desugared `is Error` pattern: an object pattern that constrains the
/// `"type"` field to the literal `"error"` (ADR-047).
fn is_error_pattern(p: &TypedPattern) -> bool {
    if let TypedPattern::Object { fields, .. } = p {
        fields.iter().any(|f| {
            f.key == "type"
                && matches!(
                    f.value_pattern.as_deref(),
                    Some(crate::typed_ir::TypedExpr::StringLit(s, _, _)) if s == "error"
                )
        })
    } else {
        false
    }
}

/// True if `v` is (structurally) the `Error` object variant `{ "type": String, "message": .. }`.
fn is_error_variant(v: &Type) -> bool {
    if let Type::Object { fields, .. } = v {
        fields.contains_key("type") && fields.contains_key("message")
    } else {
        false
    }
}

/// Returns true if two types overlap (for coverage purposes).
/// For Lin's flat union check, we just use structural equality.
fn types_overlap(a: &Type, b: &Type) -> bool {
    // Exact match always overlaps.
    if a == b {
        return true;
    }
    // Int8/Int16/Int32/UInt8/UInt16/UInt32 all have tag 2 — an `is Int32` arm covers Int32 only.
    // We match exactly here: coverage is per-type, not per-tag.
    false
}
