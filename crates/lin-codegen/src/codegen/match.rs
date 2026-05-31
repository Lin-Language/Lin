use super::builder_ext::BuilderExt;
use inkwell::values::BasicValueEnum;
use inkwell::{AddressSpace, IntPredicate};

use lin_check::types::Type;
use lin_ir::ir as lir;
use super::Codegen;

impl<'ctx> Codegen<'ctx> {
    pub(crate) fn compile_ir_is_type(&mut self, val: BasicValueEnum<'ctx>, ty: &Type) -> inkwell::values::IntValue<'ctx> {
        // Use get_tag and compare.
        if val.is_pointer_value() {
            let tag = self.builder.call(self.rt.get_tag, &[val.into()], "ir_tag").try_as_basic_value().unwrap_basic().into_int_value();
            let expected = self.type_tag_const(ty);
            self.builder.int_compare(IntPredicate::EQ, tag, expected, "ir_is")
        } else {
            self.context.bool_type().const_zero()
        }
    }

    pub(crate) fn compile_ir_has_pattern(&mut self, val: BasicValueEnum<'ctx>, pattern: &lir::HasDesc) -> inkwell::values::IntValue<'ctx> {
        let bool_ty = self.context.bool_type();
        if !val.is_pointer_value() { return bool_ty.const_zero(); }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i8_ty = self.context.i8_type();
        // BRANCHLESS: lin_value_has_field does the tag check + unbox + presence test in the
        // runtime, returning 0 for null/non-object values. Emitting no LLVM branches keeps
        // this IR instruction within a single basic block (avoids out-of-order block
        // creation that breaks SSA dominance when used inside match arms).
        let has_fn = self.get_or_declare_fn("lin_value_has_field",
            i8_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
        let mut all_present = bool_ty.const_int(1, false);
        for field in &pattern.required_fields {
            let key_str = self.compile_string_lit(field).into_pointer_value();
            let has_i8 = self.builder.call(has_fn, &[val.into(), key_str.into()], "ir_has").try_as_basic_value().unwrap_basic().into_int_value();
            self.builder.call(self.rt.string_release, &[key_str.into()], "");
            let has_bool = self.builder.int_truncate_or_bit_cast(has_i8, bool_ty, "has_b");
            all_present = self.builder.and(all_present, has_bool, "has_acc");
        }
        all_present
    }

    /// `is <ObjectType>` deep type validation (ADR-053). Emits the SAME schema descriptor the
    /// `fromJson` path builds (`emit_from_json_descriptor`) and calls `lin_matches_schema(value,
    /// descriptor)`, which runs the `fromJson` structural walker and returns an `i8` bool (`1` iff
    /// `val` recursively conforms to `target`). `val` is a boxed `TaggedVal*`, borrowed (no
    /// ownership change). Branchless — one runtime call, single basic block — so it composes
    /// inside match-arm test blocks just like `compile_ir_has_pattern`.
    pub(crate) fn compile_ir_matches_schema(
        &mut self,
        val: BasicValueEnum<'ctx>,
        target: &Type,
        named_defs: &[(String, Type)],
    ) -> inkwell::values::IntValue<'ctx> {
        let bool_ty = self.context.bool_type();
        if !val.is_pointer_value() {
            return bool_ty.const_zero();
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i8_ty = self.context.i8_type();
        let desc_ptr = self.emit_from_json_descriptor(target, named_defs);
        let matches_fn = self.get_or_declare_fn(
            "lin_matches_schema",
            i8_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false),
        );
        let r_i8 = self
            .builder
            .call(matches_fn, &[val.into(), desc_ptr.into()], "ir_matches_schema")
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        self.builder.int_truncate_or_bit_cast(r_i8, bool_ty, "matches_b")
    }

    pub(crate) fn compile_ir_coerce(&mut self, val: BasicValueEnum<'ctx>, from_ty: &Type, to_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        // Numeric widening.
        if from_ty.is_numeric() && to_ty.is_numeric() {
            if val.is_int_value() && to_ty.is_float() {
                let iv = val.into_int_value();
                let ft = if matches!(to_ty, Type::Float32) { self.context.f32_type().into() } else { self.context.f64_type() };
                return self.builder.signed_int_to_float(iv, ft, "ir_i2f").into();
            }
            if val.is_float_value() && to_ty.is_integer() {
                let fv = val.into_float_value();
                let it = self.llvm_type(to_ty).into_int_type();
                return self.builder.float_to_signed_int(fv, it, "ir_f2i").into();
            }
            if val.is_float_value() && to_ty.is_float() {
                // Float ↔ float width change: fpext (Float32→Float64) or fptrunc
                // (Float64→Float32). Without this arm the value stayed at its source
                // width and the downstream call/store saw the wrong float type.
                let fv = val.into_float_value();
                let ft = self.llvm_type(to_ty).into_float_type();
                let from_bits = fv.get_type().get_bit_width();
                let to_bits = ft.get_bit_width();
                return if to_bits > from_bits {
                    self.builder.float_ext(fv, ft, "ir_fpext").into()
                } else if to_bits < from_bits {
                    self.builder.float_trunc(fv, ft, "ir_fptrunc").into()
                } else {
                    val
                };
            }
            if val.is_int_value() && to_ty.is_integer() {
                let iv = val.into_int_value();
                let it = self.llvm_type(to_ty).into_int_type();
                let from_bits = iv.get_type().get_bit_width();
                let to_bits = it.get_bit_width();
                return if to_bits > from_bits {
                    // Widen by the SOURCE type's signedness: a signed Int32 -1 (0xFFFFFFFF)
                    // must sign-extend to Int64 -1, not zero-extend to 4294967295. Using
                    // zero-extend unconditionally corrupted `val x: Int64 = 0 - 1`.
                    if from_ty.is_signed() {
                        self.builder.int_s_extend_or_bit_cast(iv, it, "ir_sext").into()
                    } else {
                        self.builder.int_z_extend_or_bit_cast(iv, it, "ir_zext").into()
                    }
                } else {
                    self.builder.int_truncate_or_bit_cast(iv, it, "ir_trunc").into()
                };
            }
            return val;
        }
        // Box to union. Use heap boxing (lin_box_*) rather than a stack alloca, because
        // a coerced value may escape its defining function (returned, stored in an array,
        // captured) — a stack TaggedVal would dangle.
        if Self::is_union_type(to_ty) {
            return self.box_value(val, from_ty);
        }
        // Unbox from union.
        if Self::is_union_type(from_ty) && val.is_pointer_value() {
            return self.unbox_tagged_val_to_type(val, to_ty);
        }
        let _ = (from_ty, to_ty);
        if val.get_type() == self.llvm_type(to_ty) { val } else { ptr_ty.const_null().into() }
    }

}