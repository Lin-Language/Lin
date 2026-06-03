use super::builder_ext::BuilderExt;
use inkwell::values::BasicValueEnum;
use inkwell::{AddressSpace, IntPredicate, FloatPredicate};

use lin_check::types::Type;
use lin_parse::ast::BinOp;
use lin_ir::ir as lir;
use super::Codegen;

impl<'ctx> Codegen<'ctx> {
    pub(crate) fn compile_add(
        &mut self,
        lv: BasicValueEnum<'ctx>,
        rv: BasicValueEnum<'ctx>,
        lty: &Type,
        _rty: &Type,
        _result_type: &Type,
    ) -> BasicValueEnum<'ctx> {
        if lty.is_float() {
            self.builder
                .build_float_add(lv.into_float_value(), rv.into_float_value(), "fadd")
                .unwrap()
                .into()
        } else {
            self.builder
                .build_int_add(lv.into_int_value(), rv.into_int_value(), "add")
                .unwrap()
                .into()
        }
    }

    pub(crate) fn compile_arith_op(
        &mut self,
        lv: BasicValueEnum<'ctx>,
        rv: BasicValueEnum<'ctx>,
        ty: &Type,
        op: &str,
    ) -> BasicValueEnum<'ctx> {
        if ty.is_float() {
            match op {
                "sub" => self.builder.float_sub(lv.into_float_value(), rv.into_float_value(), "fsub").into(),
                "mul" => self.builder.float_mul(lv.into_float_value(), rv.into_float_value(), "fmul").into(),
                _ => unreachable!(),
            }
        } else {
            match op {
                "sub" => self.builder.int_sub(lv.into_int_value(), rv.into_int_value(), "sub").into(),
                "mul" => self.builder.int_mul(lv.into_int_value(), rv.into_int_value(), "mul").into(),
                _ => unreachable!(),
            }
        }
    }

    pub(crate) fn compile_div(
        &mut self,
        lv: BasicValueEnum<'ctx>,
        rv: BasicValueEnum<'ctx>,
        ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        if ty.is_float() {
            self.builder.float_div(lv.into_float_value(), rv.into_float_value(), "fdiv").into()
        } else {
            self.emit_int_zero_check(rv, "division by zero");
            if ty.is_signed() {
                self.builder.int_signed_div(lv.into_int_value(), rv.into_int_value(), "sdiv").into()
            } else {
                self.builder.int_unsigned_div(lv.into_int_value(), rv.into_int_value(), "udiv").into()
            }
        }
    }

    pub(crate) fn compile_mod(
        &mut self,
        lv: BasicValueEnum<'ctx>,
        rv: BasicValueEnum<'ctx>,
        ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        if ty.is_float() {
            self.builder.float_rem(lv.into_float_value(), rv.into_float_value(), "frem").into()
        } else {
            self.emit_int_zero_check(rv, "modulo by zero");
            if ty.is_signed() {
                self.builder.int_signed_rem(lv.into_int_value(), rv.into_int_value(), "srem").into()
            } else {
                self.builder.int_unsigned_rem(lv.into_int_value(), rv.into_int_value(), "urem").into()
            }
        }
    }

    /// Emit a runtime panic if the integer value `val` is zero.
    pub(crate) fn emit_int_zero_check(&mut self, val: BasicValueEnum<'ctx>, msg: &str) {
        let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let zero = val.into_int_value().get_type().const_zero();
        let is_zero = self.builder.int_compare(inkwell::IntPredicate::EQ, val.into_int_value(), zero, "divzero_chk");
        let panic_bb = self.context.append_basic_block(llvm_fn, "divzero_panic");
        let ok_bb = self.context.append_basic_block(llvm_fn, "divzero_ok");
        self.builder.conditional_branch(is_zero, panic_bb, ok_bb);
        self.builder.position_at_end(panic_bb);
        let panic_msg = self.compile_string_lit(msg);
        let zero_i32 = self.context.i32_type().const_zero();
        self.builder.call(self.rt.panic, &[panic_msg.into(), zero_i32.into(), zero_i32.into()], "");
        self.builder.unreachable();
        self.builder.position_at_end(ok_bb);
    }

    pub(crate) fn compile_eq(
        &mut self,
        lv: BasicValueEnum<'ctx>,
        rv: BasicValueEnum<'ctx>,
        ty: &Type,
        negate: bool,
    ) -> BasicValueEnum<'ctx> {
        let i64_ty = self.context.i64_type();
        let result = if ty.is_string_ish() {
            self.builder
                .build_call(self.rt.string_eq, &[lv.into(), rv.into()], "seq")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value()
        } else if matches!(ty, Type::Object(_)) {
            // Structural object equality via runtime (order-independent).
            let eq_i8 = self.builder
                .build_call(self.rt.object_eq, &[lv.into(), rv.into()], "oeq")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            self.builder.int_truncate(eq_i8, self.context.bool_type(), "oeq_b")
        } else if let Type::Array(elem) = ty {
            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let fn_ty = self.context.i8_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
            let eq_i8 = if Self::is_flat_scalar(elem) {
                let suffix = Self::flat_suffix(elem);
                let eq_fn = self.get_or_declare_fn(&format!("lin_flat_array_eq_{}", suffix), fn_ty);
                self.builder.call(eq_fn, &[lv.into(), rv.into()], "aeq")
                    .try_as_basic_value().unwrap_basic().into_int_value()
            } else {
                let eq_fn = self.get_or_declare_fn("lin_array_eq", fn_ty);
                self.builder.call(eq_fn, &[lv.into(), rv.into()], "aeq")
                    .try_as_basic_value().unwrap_basic().into_int_value()
            };
            self.builder.int_truncate(eq_i8, self.context.bool_type(), "aeq_b")
        } else if ty.is_float() {
            self.builder
                .build_float_compare(FloatPredicate::OEQ, lv.into_float_value(), rv.into_float_value(), "feq")
                .unwrap()
        } else if lv.is_pointer_value() || rv.is_pointer_value() {
            // Pointer comparison (closures, etc.) — compare addresses.
            let lp = if lv.is_pointer_value() {
                self.builder.ptr_to_int(lv.into_pointer_value(), i64_ty, "lpi")
            } else {
                self.builder.int_s_extend_or_bit_cast(lv.into_int_value(), i64_ty, "lpx")
            };
            let rp = if rv.is_pointer_value() {
                self.builder.ptr_to_int(rv.into_pointer_value(), i64_ty, "rpi")
            } else {
                self.builder.int_s_extend_or_bit_cast(rv.into_int_value(), i64_ty, "rpx")
            };
            self.builder.int_compare(IntPredicate::EQ, lp, rp, "peq")
        } else {
            self.builder
                .build_int_compare(IntPredicate::EQ, lv.into_int_value(), rv.into_int_value(), "ieq")
                .unwrap()
        };

        if negate {
            self.builder.not(result, "neq").into()
        } else {
            result.into()
        }
    }

    pub(crate) fn compile_cmp(
        &mut self,
        lv: BasicValueEnum<'ctx>,
        rv: BasicValueEnum<'ctx>,
        ty: &Type,
        signed_pred: IntPredicate,
        unsigned_pred: IntPredicate,
        float_pred: FloatPredicate,
    ) -> BasicValueEnum<'ctx> {
        // String comparison via runtime — pointer comparison is wrong.
        if ty.is_string_ish() {
            let i32_ty = self.context.i32_type();
            let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
            let cmp_fn_ty = i32_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false);
            let cmp_fn = self.get_or_declare_fn("lin_string_cmp", cmp_fn_ty);
            let result = self.builder
                .build_call(cmp_fn, &[lv.into(), rv.into()], "scmp_result")
                .unwrap()
                .try_as_basic_value()
                .unwrap_basic()
                .into_int_value();
            let zero = i32_ty.const_zero();
            return self.builder
                .build_int_compare(signed_pred, result, zero, "scmp")
                .unwrap()
                .into();
        }

        let i64_ty = self.context.i64_type();
        // Normalize operand types: if either is a pointer, convert both to i64. Such a compare
        // is an ADDRESS comparison and must use the UNSIGNED predicate (addresses are unsigned;
        // a high-half address would otherwise read as negative).
        let is_ptr_cmp = lv.is_pointer_value() || rv.is_pointer_value();
        let (lv, rv) = if is_ptr_cmp {
            let l = if lv.is_pointer_value() {
                self.builder.ptr_to_int(lv.into_pointer_value(), i64_ty, "lpc").into()
            } else {
                self.builder.int_s_extend_or_bit_cast(lv.into_int_value(), i64_ty, "lext").into()
            };
            let r = if rv.is_pointer_value() {
                self.builder.ptr_to_int(rv.into_pointer_value(), i64_ty, "rpc").into()
            } else {
                self.builder.int_s_extend_or_bit_cast(rv.into_int_value(), i64_ty, "rext").into()
            };
            (l, r)
        } else {
            (lv, rv)
        };

        if ty.is_float() {
            self.builder.float_compare(float_pred, lv.into_float_value(), rv.into_float_value(), "fcmp").into()
        } else if is_ptr_cmp {
            // Address comparison — unsigned (see normalization above).
            self.builder.int_compare(unsigned_pred, lv.into_int_value(), rv.into_int_value(), "pcmp").into()
        } else if ty.is_signed() {
            // Signedness is determined SOLELY by the operand type. The old `bit_width == 64`
            // fallback forced signed predicates for any 64-bit operand, which made a UInt64
            // >= 2^63 (high bit set) compare as negative. UInt64 is unsigned → unsigned predicate.
            self.builder.int_compare(signed_pred, lv.into_int_value(), rv.into_int_value(), "scmp").into()
        } else {
            self.builder.int_compare(unsigned_pred, lv.into_int_value(), rv.into_int_value(), "ucmp").into()
        }
    }

    pub(crate) fn compile_ir_unary(&mut self, val: BasicValueEnum<'ctx>, op: &lir::UnaryOp, _ty: &Type) -> BasicValueEnum<'ctx> {
        match op {
            lir::UnaryOp::Neg => {
                if val.is_int_value() {
                    let iv = val.into_int_value();
                    self.builder.int_neg(iv, "ir_neg").into()
                } else if val.is_float_value() {
                    let fv = val.into_float_value();
                    self.builder.float_neg(fv, "ir_fneg").into()
                } else { val }
            }
            lir::UnaryOp::Not => {
                if val.is_int_value() {
                    let iv = val.into_int_value();
                    self.builder.not(iv, "ir_not").into()
                } else { val }
            }
        }
    }

    pub(crate) fn compile_binary_op_values(
        &mut self,
        lv: BasicValueEnum<'ctx>,
        rv: BasicValueEnum<'ctx>,
        op: &BinOp,
        lty: &Type,
        rty: &Type,
        result_ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        // Box the rhs to a TaggedVal* when comparing against a boxed (union) lhs: a concrete
        // rhs value must be boxed by its STATIC type. A raw `LinString*` (a string literal)
        // is a pointer but NOT a TaggedVal — passing it to lin_tagged_eq/_cmp would read its
        // bytes as a tag/payload and overflow. `box_value` is a no-op when rty is already a
        // union, so this is safe to apply whenever rty is concrete.
        let box_rhs = |s: &mut Self, v: BasicValueEnum<'ctx>| -> BasicValueEnum<'ctx> {
            if Self::is_union_type(rty) { v } else { s.box_value(v, rty) }
        };
        let box_lhs = |s: &mut Self, v: BasicValueEnum<'ctx>| -> BasicValueEnum<'ctx> {
            if Self::is_union_type(lty) { v } else { s.box_value(v, lty) }
        };
        // Mixed int/float arithmetic (e.g. `5 + 3.0`, or `x % 2` in a `Number`→Float64 specialization
        // where the literal stays Int32): widen the integer operand to float so both sides agree, and
        // dispatch on the float type. The checker permits these numeric combinations without inserting
        // explicit Coerce nodes on both operands. `Mod` is included so a `Number` body's `x % 2`
        // lowers to a native `frem` at Float64 (ADR-018, reversed).
        if matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod
            | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq | BinOp::Eq | BinOp::NotEq)
            && lv.is_int_value() != rv.is_int_value()
            && (lv.is_float_value() || rv.is_float_value())
            && (lv.is_int_value() || lv.is_float_value())
            && (rv.is_int_value() || rv.is_float_value())
        {
            let f64_ty = self.context.f64_type();
            let to_f = |s: &Self, v: BasicValueEnum<'ctx>| -> BasicValueEnum<'ctx> {
                if v.is_int_value() {
                    s.builder.signed_int_to_float(v.into_int_value(), f64_ty, "ir_i2f").into()
                } else {
                    s.builder.float_cast(v.into_float_value(), f64_ty, "ir_fwiden").into()
                }
            };
            let lf = to_f(self, lv);
            let rf = to_f(self, rv);
            return self.compile_binary_op_values(lf, rf, op, &Type::Float64, &Type::Float64, result_ty);
        }
        // Mismatched float widths (Float32 op Float64): widen the narrower operand to the
        // wider with fpext, then dispatch on the wider float type. Without this, `f32 + f64`
        // hit "Both operands to a binary operator are not of the same type".
        if lv.is_float_value() && rv.is_float_value() {
            let lw = lv.into_float_value().get_type().get_bit_width();
            let rw = rv.into_float_value().get_type().get_bit_width();
            if lw != rw {
                let wide_is_left = lw > rw;
                let wide = if wide_is_left { lv.into_float_value().get_type() } else { rv.into_float_value().get_type() };
                let wide_ty = if wide_is_left { lty } else { rty };
                let lf = if lw < wide.get_bit_width() { self.builder.float_ext(lv.into_float_value(), wide, "ir_fpext").into() } else { lv };
                let rf = if rw < wide.get_bit_width() { self.builder.float_ext(rv.into_float_value(), wide, "ir_fpext").into() } else { rv };
                return self.compile_binary_op_values(lf, rf, op, wide_ty, wide_ty, result_ty);
            }
        }
        // Mismatched integer widths (e.g. Int64 `n` vs an Int32 literal `0`): sign-extend
        // the narrower operand to the wider so the ICmp/arith operands agree.
        if lv.is_int_value() && rv.is_int_value() {
            let lw = lv.into_int_value().get_type().get_bit_width();
            let rw = rv.into_int_value().get_type().get_bit_width();
            if lw != rw && lw > 1 && rw > 1 {
                // Extend the narrower operand to the wider width. Choose sign- vs zero-extend
                // per the SOURCE operand's signedness so an unsigned small int (e.g. UInt8
                // 250) widens to 250, not -6. Result type is the wider operand's static type.
                let wide_is_left = lw > rw;
                let wide = if wide_is_left { lv.into_int_value().get_type() } else { rv.into_int_value().get_type() };
                let wide_ty = if wide_is_left { lty } else { rty };
                let ext = |s: &Self, v: BasicValueEnum<'ctx>, src_ty: &Type| -> BasicValueEnum<'ctx> {
                    if src_ty.is_signed() {
                        s.builder.int_s_extend(v.into_int_value(), wide, "ir_sext").into()
                    } else {
                        s.builder.int_z_extend(v.into_int_value(), wide, "ir_zext").into()
                    }
                };
                let lext = if lw < wide.get_bit_width() { ext(self, lv, lty) } else { lv };
                let rext = if rw < wide.get_bit_width() { ext(self, rv, rty) } else { rv };
                return self.compile_binary_op_values(lext, rext, op, wide_ty, wide_ty, result_ty);
            }
        }
        // Equality / ordering where EITHER operand is a boxed union (Json/TypeVar). These
        // must be ORDER-SYMMETRIC: `lit == proj` and `proj == lit` have to agree. The boxed
        // operand is a TaggedVal* whose representation differs from a concrete value (e.g. a
        // raw `LinString*` literal, or an i64), so routing through the typed `compile_eq` /
        // `compile_cmp` would misread it (it dispatches on the static `lty` and calls
        // `lin_string_eq`/etc. expecting a raw pointer). Instead box BOTH sides by their
        // STATIC type (a no-op for the already-boxed union side) and dispatch via the tagged
        // runtime ops, which tolerate boxed/null payloads of mixed shapes. The earlier
        // lhs-only branch below handled `proj == lit` but not `lit == proj` — that asymmetry
        // produced order-dependent string equality for boxed-key projections.
        if matches!(op, BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq)
            && ((Self::is_union_type(lty) && lv.is_pointer_value())
                || (Self::is_union_type(rty) && rv.is_pointer_value()))
        {
            let lv_tagged = box_lhs(self, lv);
            let rv_tagged = box_rhs(self, rv);
            match op {
                BinOp::Eq | BinOp::NotEq => {
                    let i8_ty = self.context.i8_type();
                    let eq_fn = self.get_or_declare_fn("lin_tagged_eq",
                        i8_ty.fn_type(
                            &[self.context.ptr_type(AddressSpace::default()).into(),
                              self.context.ptr_type(AddressSpace::default()).into()], false));
                    let eq_u8 = self.builder.call(eq_fn, &[lv_tagged.into(), rv_tagged.into()], "ir_teq").try_as_basic_value().unwrap_basic().into_int_value();
                    let eq = self.builder.int_truncate(eq_u8, self.context.bool_type(), "ir_teq_b");
                    return if matches!(op, BinOp::NotEq) {
                        self.builder.not(eq, "ir_tne").into()
                    } else { eq.into() };
                }
                _ => {
                    let i32_ty = self.context.i32_type();
                    let ptr_t = self.context.ptr_type(AddressSpace::default());
                    let cmp_fn = self.get_or_declare_fn("lin_tagged_cmp",
                        i32_ty.fn_type(&[ptr_t.into(), ptr_t.into()], false));
                    let ord = self.builder.call(cmp_fn, &[lv_tagged.into(), rv_tagged.into()], "ir_tcmp").try_as_basic_value().unwrap_basic().into_int_value();
                    let zero = i32_ty.const_zero();
                    let pred = match op {
                        BinOp::Lt => IntPredicate::SLT, BinOp::LtEq => IntPredicate::SLE,
                        BinOp::Gt => IntPredicate::SGT, _ => IntPredicate::SGE,
                    };
                    return self.builder.int_compare(pred, ord, zero, "ir_tcmp_b").into();
                }
            }
        }
        // When operands are boxed (Json/union), use tagged runtime ops for equality and
        // ordering (which tolerate mixed/null payloads), and unbox to a concrete numeric
        // type for arithmetic. Mirrors the AST path's TypeVar handling in compile_binary_op.
        if Self::is_union_type(lty) && lv.is_pointer_value() {
            match op {
                BinOp::Eq | BinOp::NotEq => {
                    // lin_tagged_eq returns u8 (i8), not i1 — declare it as i8 and
                    // truncate, else the call reads garbage bits and compares as always-true.
                    let i8_ty = self.context.i8_type();
                    let eq_fn = self.get_or_declare_fn("lin_tagged_eq",
                        i8_ty.fn_type(
                            &[self.context.ptr_type(AddressSpace::default()).into(),
                              self.context.ptr_type(AddressSpace::default()).into()], false));
                    // Box the rhs to a TaggedVal* by its STATIC type. A concrete rhs (incl. a
                    // raw LinString* from a string literal) must be boxed; a union rhs is
                    // already a TaggedVal*. `x == 3` boxes 3 as int; `t == "pass"` boxes the
                    // string. (box_rhs is a no-op for union rty.)
                    let rv_tagged = box_rhs(self, rv);
                    let eq_u8 = self.builder.call(eq_fn, &[lv.into(), rv_tagged.into()], "ir_teq").try_as_basic_value().unwrap_basic().into_int_value();
                    let eq = self.builder.int_truncate(eq_u8, self.context.bool_type(), "ir_teq_b");
                    return if matches!(op, BinOp::NotEq) {
                        self.builder.not(eq, "ir_tne").into()
                    } else { eq.into() };
                }
                BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                    // Boxed operands may be strings or numbers — use lin_tagged_cmp (returns
                    // -1/0/1) which dispatches on the runtime tag, then compare to 0.
                    let i32_ty = self.context.i32_type();
                    let ptr_t = self.context.ptr_type(AddressSpace::default());
                    let cmp_fn = self.get_or_declare_fn("lin_tagged_cmp",
                        i32_ty.fn_type(&[ptr_t.into(), ptr_t.into()], false));
                    let rv_tagged = box_rhs(self, rv);
                    let ord = self.builder.call(cmp_fn, &[lv.into(), rv_tagged.into()], "ir_tcmp").try_as_basic_value().unwrap_basic().into_int_value();
                    let zero = i32_ty.const_zero();
                    let pred = match op {
                        BinOp::Lt => IntPredicate::SLT, BinOp::LtEq => IntPredicate::SLE,
                        BinOp::Gt => IntPredicate::SGT, _ => IntPredicate::SGE,
                    };
                    return self.builder.int_compare(pred, ord, zero, "ir_tcmp_b").into();
                }
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => {
                    // Arithmetic on a boxed union/Json operand: the concrete numeric type
                    // (Int32/Int64/Float64/…) is only known at runtime, so unboxing to a
                    // fixed type here would reinterpret e.g. a Float64's bits as an integer.
                    // Dispatch on the runtime tags via lin_tagged_arith, which returns a
                    // freshly boxed numeric result (Float64 if either operand is a float).
                    let ptr_t = self.context.ptr_type(AddressSpace::default());
                    let i32_ty = self.context.i32_type();
                    let arith_fn = self.get_or_declare_fn("lin_tagged_arith",
                        ptr_t.fn_type(&[ptr_t.into(), ptr_t.into(), i32_ty.into()], false));
                    let rv_tagged = box_rhs(self, rv);
                    let op_code = match op {
                        BinOp::Add => 0, BinOp::Sub => 1, BinOp::Mul => 2,
                        BinOp::Div => 3, _ => 4, // Mod
                    };
                    let boxed = self.builder.call(
                        arith_fn,
                        &[lv.into(), rv_tagged.into(), i32_ty.const_int(op_code, false).into()],
                        "ir_tarith",
                    ).try_as_basic_value().unwrap_basic();
                    // The helper hands back a freshly boxed (union) value. If the surrounding
                    // context wants a concrete scalar, unbox it and reclaim the box shell —
                    // the payload is a scalar (no inner heap), so freeing the 16-byte shell is
                    // safe and avoids leaking one box per arithmetic op.
                    return if Self::is_union_type(result_ty) {
                        boxed
                    } else {
                        let concrete = self.unbox_tagged_val_to_type(boxed, result_ty);
                        let free_fn = self.get_or_declare_fn(
                            "lin_tagged_free_box",
                            self.context.void_type().fn_type(&[ptr_t.into()], false));
                        self.builder.call(free_fn, &[boxed.into()], "");
                        concrete
                    };
                }
                BinOp::BAnd | BinOp::BOr | BinOp::BXor | BinOp::Shl | BinOp::Shr => {
                    // Bitwise/shift ops are integer-only (checker-enforced); a boxed union
                    // operand (e.g. a TypeVar reduce-lambda param) must be unboxed to the
                    // concrete integer type before the LLVM int op.
                    let lconc = self.unbox_tagged_val_to_type(lv, &Type::Int32);
                    let rconc = if rv.is_pointer_value() { self.unbox_tagged_val_to_type(rv, &Type::Int32) } else { rv };
                    let concrete = self.compile_binary_op_values(lconc, rconc, op, &Type::Int32, &Type::Int32, &Type::Int32);
                    // If the surrounding context expects a union/Json value, re-box the
                    // concrete result (heap) so it can be stored/returned uniformly.
                    return if Self::is_union_type(result_ty) {
                        self.box_value(concrete, &Type::Int32)
                    } else {
                        concrete
                    };
                }
                _ => {}
            }
        }
        match op {
            BinOp::Add => self.compile_add(lv, rv, lty, lty, result_ty),
            BinOp::Sub => self.compile_arith_op(lv, rv, lty, "sub"),
            BinOp::Mul => self.compile_arith_op(lv, rv, lty, "mul"),
            BinOp::Div => self.compile_div(lv, rv, lty),
            BinOp::Mod => self.compile_mod(lv, rv, lty),
            BinOp::Eq => self.compile_eq(lv, rv, lty, false),
            BinOp::NotEq => self.compile_eq(lv, rv, lty, true),
            BinOp::Lt => self.compile_cmp(lv, rv, lty, IntPredicate::SLT, IntPredicate::ULT, FloatPredicate::OLT),
            BinOp::LtEq => self.compile_cmp(lv, rv, lty, IntPredicate::SLE, IntPredicate::ULE, FloatPredicate::OLE),
            BinOp::Gt => self.compile_cmp(lv, rv, lty, IntPredicate::SGT, IntPredicate::UGT, FloatPredicate::OGT),
            BinOp::GtEq => self.compile_cmp(lv, rv, lty, IntPredicate::SGE, IntPredicate::UGE, FloatPredicate::OGE),
            BinOp::And => self.builder.and(lv.into_int_value(), rv.into_int_value(), "ir_and").into(),
            BinOp::Or => self.builder.or(lv.into_int_value(), rv.into_int_value(), "ir_or").into(),
            // Bitwise integer operators (§27.2). Operands are integers (checker-enforced)
            // and widths have been reconciled above.
            BinOp::BAnd => self.builder.and(lv.into_int_value(), rv.into_int_value(), "ir_band").into(),
            BinOp::BOr => self.builder.or(lv.into_int_value(), rv.into_int_value(), "ir_bor").into(),
            BinOp::BXor => self.builder.xor(lv.into_int_value(), rv.into_int_value(), "ir_bxor").into(),
            BinOp::Shl => self.builder.left_shift(lv.into_int_value(), rv.into_int_value(), "ir_shl").into(),
            // `>>` is arithmetic for signed types and logical for unsigned types.
            BinOp::Shr => {
                let sign_extend = lty.is_signed();
                self.builder.right_shift(lv.into_int_value(), rv.into_int_value(), sign_extend, "ir_shr").into()
            }
        }
    }

}