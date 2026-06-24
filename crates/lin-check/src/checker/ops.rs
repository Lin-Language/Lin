use lin_common::{Diagnostic, Span};
use lin_parse::ast::{BinOp, Expr, UnaryOp};

use super::Checker;
use super::helpers::integer_range;
use crate::typed_ir::*;
use crate::types::Type;
use crate::widen::widen_numeric;

impl Checker {
    /// If `cand` is a *suffixless* integer literal and `other` has a concrete integer type T,
    /// re-type `cand` to T (spec §21). Errors if the literal value doesn't fit T's range.
    /// `op_span` is used for the error location. No-op when `cand` isn't an `IntLit`, was
    /// explicitly suffixed (`cand_suffixed`), or `other` isn't a concrete integer type.
    ///
    /// The suffix guard is load-bearing. A literal whose width was chosen EXPLICITLY (e.g.
    /// `1000003i64`) must NOT be re-typed to match the other operand — otherwise `x * 1000003i64`
    /// with `x: Int32` re-types the `i64` literal DOWN to Int32, the multiply overflows at Int32,
    /// and only the result is widened to Int64 (a silent overflow). A suffixless literal still
    /// adopts the operand's (possibly narrower) width, e.g. `a + 5` with `a: UInt8` (spec §21).
    pub(crate) fn retype_literal_operand(
        &mut self,
        cand: &mut TypedExpr,
        other: &TypedExpr,
        cand_suffixed: bool,
        op_span: Span,
    ) -> Result<(), Diagnostic> {
        if cand_suffixed {
            return Ok(());
        }
        if let TypedExpr::IntLit(v, _cur_ty, lit_span) = cand {
            let target = other.ty();
            // Only re-type against a concrete integer width (not Int32-default unless the
            // other side genuinely is Int32; widening to the same width is harmless).
            if let Some((lo, hi)) = integer_range(&target) {
                let (v, lit_span) = (*v, *lit_span);
                if (v as i128) < lo || (v as i128) > hi {
                    let _ = op_span;
                    return Err(Diagnostic::error(
                        lit_span,
                        format!("literal {} is out of range for type {}", v, target),
                    ));
                }
                *cand = TypedExpr::IntLit(v, target, lit_span);
            }
        }
        Ok(())
    }

    pub(crate) fn infer_binary_op(
        &mut self,
        left: &Expr,
        op: BinOp,
        right: &Expr,
        span: Span,
    ) -> Result<TypedExpr, Diagnostic> {
        // Binary operands are never in tail position.
        let prev_tail = std::mem::replace(&mut self.in_tail_position, false);
        let typed_left = self.infer_expr(left)?;
        // Flow-narrowing across short-circuit operators:
        //   `a && b`: right is only reached when `a` is TRUTHY — apply `a`'s "then" narrowing.
        //   `a || b`: right is only reached when `a` is FALSY  — apply `a`'s "else" narrowing.
        // Both mirror `infer_if`'s branch narrowing. `x != null && f(x)` and `x == null || f(x)`
        // must both type-check with `x` narrowed to non-Null inside the right operand.
        let typed_right = if matches!(op, BinOp::And | BinOp::Or) {
            let narrowing = self.null_test_narrowing(left);
            let entering_then = matches!(op, BinOp::And);
            self.env.push_scope();
            self.apply_null_narrowing(&narrowing, entering_then);
            let r = self.infer_expr(right);
            self.env.pop_scope();
            r?
        } else {
            self.infer_expr(right)?
        };
        self.in_tail_position = prev_tail;

        // Spec §21: a suffixless integer literal takes its context type. When one operand of
        // an arithmetic/bitwise op is a bare integer literal (typed Int32 by default) and the
        // OTHER operand has a concrete integer type T, re-type the literal at width T so both
        // sides share a width. This avoids a width mismatch between the checker's result type
        // and the value codegen produces. For shifts, only the LEFT operand drives the result
        // type, so we only re-type a literal LEFT against a concrete-int RIGHT.
        // An explicit numeric suffix (e.g. `1000003i64`) PINS the literal's type; such a
        // literal must never be re-typed to match the other operand (which could narrow it and
        // overflow). A suffixless literal still adopts the operand's width.
        let left_suffixed = matches!(left, Expr::IntLit(_, Some(_), _));
        let right_suffixed = matches!(right, Expr::IntLit(_, Some(_), _));
        let (mut typed_left, mut typed_right) = (typed_left, typed_right);
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
            | BinOp::BAnd | BinOp::BOr | BinOp::BXor => {
                self.retype_literal_operand(&mut typed_left, &typed_right, left_suffixed, span)?;
                self.retype_literal_operand(&mut typed_right, &typed_left, right_suffixed, span)?;
            }
            BinOp::Shl | BinOp::Shr => {
                // Only the left operand's type matters for the result; retype a literal LEFT
                // against a concrete-int RIGHT. A literal RIGHT (shift count) stays Int32.
                self.retype_literal_operand(&mut typed_left, &typed_right, left_suffixed, span)?;
            }
            _ => {}
        }

        let left_ty = typed_left.ty();
        let right_ty = typed_right.ty();

        // A `Number`-bounded generic TypeVar operand (ADR-014, reversed) drives an arithmetic op's
        // result type so it FLOWS THROUGH monomorphization: `x % 2` where `x: <T:numeric>` must yield
        // `T`, not the literal's Int32 — so the `$Float64` specialization lowers the op to a native
        // `frem`. The OTHER (literal) operand stays its own type; codegen widens an `Int32` literal
        // to the float family at the op (mixed int/float arith, incl. Mod). Comparisons yield Bool.
        let left_num_tv = matches!(left_ty, Type::TypeVar(id) if self.numeric_tvs.contains(&id));
        let right_num_tv = matches!(right_ty, Type::TypeVar(id) if self.numeric_tvs.contains(&id));

        let result_type = match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                let left_is_any = matches!(left_ty, Type::TypeVar(_));
                let right_is_any = matches!(right_ty, Type::TypeVar(_));
                if op == BinOp::Add && (left_ty.is_string_ish() || right_ty.is_string_ish()) {
                    return Err(Diagnostic::error(
                        span,
                        "String concatenation with + is not supported; use interpolation: \"${a}${b}\"".to_string(),
                    ));
                } else if left_num_tv || right_num_tv {
                    if left_num_tv { left_ty.clone() } else { right_ty.clone() }
                } else if left_ty.is_numeric() && right_ty.is_numeric() {
                    widen_numeric(&left_ty, &right_ty).unwrap_or(Type::Int32)
                } else if left_is_any || right_is_any {
                    // A DYNAMIC operand — a `Json` wildcard or an unresolved/index-derived TypeVar
                    // (e.g. `obj["k"]`, whose runtime value may be Int, Float, or a missing-key
                    // Null). The result is dynamic `Json`, NOT the concrete side: the runtime
                    // numeric family is unknown, and a missing-key Null must FAULT (not crash) at
                    // the op. Typing it `Json` keeps the value boxed so IR lowering routes the op
                    // through the null-safe `lin_tagged_arith` runtime path (RAPTOR #5). (The
                    // genuine `Number`-bounded numeric TypeVar is handled by the `*_num_tv` arm
                    // above and stays native.)
                    Type::TypeVar(u32::MAX)
                } else {
                    return Err(Diagnostic::error(
                        span,
                        format!(
                            "Cannot apply operator {:?} to {} and {}",
                            op, left_ty, right_ty
                        ),
                    ));
                }
            }
            BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                Type::Bool
            }
            BinOp::And | BinOp::Or => Type::Bool,
            // Bitwise and/or/xor (§27.2): both operands must be integer; result is the
            // widened integer type. A float operand is a compile-time error.
            BinOp::BAnd | BinOp::BOr | BinOp::BXor => {
                let left_is_any = matches!(left_ty, Type::TypeVar(_));
                let right_is_any = matches!(right_ty, Type::TypeVar(_));
                if left_ty.is_float() {
                    return Err(Diagnostic::error(
                        span,
                        format!("bitwise operator {:?} requires integer operands, got {}", op, left_ty),
                    ));
                }
                if right_ty.is_float() {
                    return Err(Diagnostic::error(
                        span,
                        format!("bitwise operator {:?} requires integer operands, got {}", op, right_ty),
                    ));
                }
                if left_ty.is_integer() && right_ty.is_integer() {
                    widen_numeric(&left_ty, &right_ty).unwrap_or(Type::Int32)
                } else if left_is_any && right_is_any {
                    Type::Int32
                } else if left_is_any {
                    right_ty.clone()
                } else if right_is_any {
                    left_ty.clone()
                } else {
                    return Err(Diagnostic::error(
                        span,
                        format!(
                            "bitwise operator {:?} requires integer operands, got {} and {}",
                            op, left_ty, right_ty
                        ),
                    ));
                }
            }
            // Shifts (§27.2): left operand integer, right operand any integer; result is
            // the type of the left operand.
            BinOp::Shl | BinOp::Shr => {
                let left_is_any = matches!(left_ty, Type::TypeVar(_));
                let right_is_any = matches!(right_ty, Type::TypeVar(_));
                if left_ty.is_float() {
                    return Err(Diagnostic::error(
                        span,
                        format!("bitwise operator {:?} requires integer operands, got {}", op, left_ty),
                    ));
                }
                if right_ty.is_float() {
                    return Err(Diagnostic::error(
                        span,
                        format!("bitwise operator {:?} requires integer operands, got {}", op, right_ty),
                    ));
                }
                if !right_is_any && !right_ty.is_integer() {
                    return Err(Diagnostic::error(
                        span,
                        format!("bitwise operator {:?} requires integer operands, got {}", op, right_ty),
                    ));
                }
                if left_ty.is_integer() {
                    left_ty.clone()
                } else if left_is_any {
                    Type::Int32
                } else {
                    return Err(Diagnostic::error(
                        span,
                        format!("bitwise operator {:?} requires integer operands, got {}", op, left_ty),
                    ));
                }
            }
        };

        Ok(TypedExpr::BinaryOp {
            left: Box::new(typed_left),
            op,
            right: Box::new(typed_right),
            result_type,
            span,
        })
    }

    // Unary `~` (bitwise not, §27.2): operand must be integer; result is the operand's type.
    pub(crate) fn infer_unary_op(
        &mut self,
        op: UnaryOp,
        operand: &Expr,
        span: Span,
    ) -> Result<TypedExpr, Diagnostic> {
        let prev_tail = std::mem::replace(&mut self.in_tail_position, false);
        let typed_operand = self.infer_expr(operand)?;
        self.in_tail_position = prev_tail;
        let operand_ty = typed_operand.ty();

        let result_type = match op {
            UnaryOp::BNot => {
                if operand_ty.is_float() {
                    return Err(Diagnostic::error(
                        span,
                        format!("bitwise operator ~ requires an integer operand, got {}", operand_ty),
                    ));
                }
                if operand_ty.is_integer() {
                    operand_ty.clone()
                } else if matches!(operand_ty, Type::TypeVar(_)) {
                    Type::Int32
                } else {
                    return Err(Diagnostic::error(
                        span,
                        format!("bitwise operator ~ requires an integer operand, got {}", operand_ty),
                    ));
                }
            }
            UnaryOp::Not => {
                if matches!(operand_ty, Type::Bool) {
                    Type::Bool
                } else if matches!(operand_ty, Type::TypeVar(_)) {
                    Type::Bool
                } else {
                    return Err(Diagnostic::error(
                        span,
                        format!("logical operator ! requires a boolean operand, got {}", operand_ty),
                    ));
                }
            }
        };

        Ok(TypedExpr::UnaryOp {
            op,
            operand: Box::new(typed_operand),
            result_type,
            span,
        })
    }
}
