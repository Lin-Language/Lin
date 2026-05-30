use inkwell::values::{
    BasicValueEnum, PointerValue,
};
use inkwell::AddressSpace;

use lin_check::types::Type;
use super::Codegen;

impl<'ctx> Codegen<'ctx> {
    /// Box a value into a tagged union pointer (TaggedVal*).
    /// For concrete types, allocates and fills a TaggedVal with the appropriate tag.
    /// For TypeVar, dispatches on the actual LLVM type (int/float/pointer) to pick the right box call.
    pub(crate) fn box_value(&mut self, val: BasicValueEnum<'ctx>, val_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr = match val_ty {
            Type::Null => self.builder.build_call(self.rt.box_null, &[], "boxnull").unwrap()
                .try_as_basic_value().unwrap_basic(),
            Type::Bool => {
                let i8v = if val.is_int_value() {
                    // Bool is i1; zero-extend to i8 for lin_box_bool(i8).
                    self.builder.build_int_z_extend_or_bit_cast(val.into_int_value(), self.context.i8_type(), "btoi8").unwrap().into()
                } else { val };
                self.builder.build_call(self.rt.box_bool, &[i8v.into()], "boxbool").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Int8 | Type::Int16 | Type::Int32 => {
                let i32v = self.builder.build_int_s_extend_or_bit_cast(val.into_int_value(), self.context.i32_type(), "toi32").unwrap();
                self.builder.build_call(self.rt.box_int32, &[i32v.into()], "boxi32").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::UInt8 | Type::UInt16 | Type::UInt32 => {
                let i32v = self.builder.build_int_z_extend_or_bit_cast(val.into_int_value(), self.context.i32_type(), "tou32").unwrap();
                self.builder.build_call(self.rt.box_int32, &[i32v.into()], "boxi32").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Int64 | Type::UInt64 => {
                let i64v = val.into_int_value();
                self.builder.build_call(self.rt.box_int64, &[i64v.into()], "boxi64").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Float32 => {
                let f64v = self.builder.build_float_ext(val.into_float_value(), self.context.f64_type(), "f32tof64").unwrap();
                self.builder.build_call(self.rt.box_float64, &[f64v.into()], "boxf64").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Float64 => {
                self.builder.build_call(self.rt.box_float64, &[val.into()], "boxf64").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Str => {
                self.builder.build_call(self.rt.box_str, &[val.into()], "boxstr").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Object(_) => {
                self.builder.build_call(self.rt.box_object, &[val.into()], "boxobj").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Array(_) if val.is_pointer_value() => {
                // Box the LinArray* directly (flat or tagged). The elem_tag field in LinArray
                // lets runtime functions (lin_array_get_tagged, lin_push_dyn, etc.) dispatch
                // correctly without needing a separate conversion copy.
                self.builder.build_call(self.rt.box_array, &[val.into()], "boxarr").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_) => {
                // Iterator values have already been converted to tagged arrays by the intrinsic.
                self.builder.build_call(self.rt.box_array, &[val.into()], "boxarr").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Function { .. } => {
                self.builder.build_call(self.rt.box_function, &[val.into()], "boxfn").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            // Union type — if value is a pointer, box as object (most common case).
            // If it's already a tagged pointer, return as-is.
            Type::Union(variants) => {
                if val.is_pointer_value() {
                    // If all variants are Object types, this is a LinObject*.
                    let all_objects = variants.iter().all(|v| matches!(v, Type::Object(_)));
                    if all_objects {
                        self.builder.build_call(self.rt.box_object, &[val.into()], "boxobj").unwrap()
                            .try_as_basic_value().unwrap_basic()
                    } else {
                        // Already tagged (or unknown) — return as-is.
                        val
                    }
                } else {
                    val
                }
            }
            Type::TypeVar(_) => {
                // TypeVar value — box by actual LLVM type.
                if val.is_int_value() {
                    let iv = val.into_int_value();
                    let i32_ty = self.context.i32_type();
                    let i64_ty = self.context.i64_type();
                    let bit_width = iv.get_type().get_bit_width();
                    if bit_width <= 32 {
                        let i32v = self.builder.build_int_s_extend_or_bit_cast(iv, i32_ty, "tvi32").unwrap();
                        self.builder.build_call(self.rt.box_int32, &[i32v.into()], "tvboxi32").unwrap()
                            .try_as_basic_value().unwrap_basic()
                    } else {
                        let i64v = self.builder.build_int_s_extend_or_bit_cast(iv, i64_ty, "tvi64").unwrap();
                        self.builder.build_call(self.rt.box_int64, &[i64v.into()], "tvboxi64").unwrap()
                            .try_as_basic_value().unwrap_basic()
                    }
                } else if val.is_float_value() {
                    let fv = val.into_float_value();
                    let f64_ty = self.context.f64_type();
                    let f64v = self.builder.build_float_ext(fv, f64_ty, "tvf64").unwrap();
                    self.builder.build_call(self.rt.box_float64, &[f64v.into()], "tvboxf64").unwrap()
                        .try_as_basic_value().unwrap_basic()
                } else {
                    val
                }
            }
            _ => val,
        };
        ptr
    }

    /// Unbox a tagged union pointer to the concrete type `target_ty`.
    pub(crate) fn unbox_value(&mut self, ptr: BasicValueEnum<'ctx>, target_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_val = ptr.into_pointer_value();
        match target_ty {
            Type::Null => self.context.ptr_type(AddressSpace::default()).const_null().into(),
            Type::Bool => {
                let v = self.builder.build_call(self.rt.unbox_bool, &[ptr_val.into()], "ubool").unwrap()
                    .try_as_basic_value().unwrap_basic();
                // Convert i8 to i1
                self.builder.build_int_truncate(v.into_int_value(), self.context.bool_type(), "utobool").unwrap().into()
            }
            Type::Int8 | Type::Int16 | Type::Int32 => {
                let v = self.builder.build_call(self.rt.unbox_int32, &[ptr_val.into()], "ui32").unwrap()
                    .try_as_basic_value().unwrap_basic();
                let ity = self.llvm_type(target_ty).into_int_type();
                self.builder.build_int_truncate_or_bit_cast(v.into_int_value(), ity, "toi").unwrap().into()
            }
            Type::UInt8 | Type::UInt16 | Type::UInt32 => {
                let v = self.builder.build_call(self.rt.unbox_int32, &[ptr_val.into()], "uu32").unwrap()
                    .try_as_basic_value().unwrap_basic();
                let ity = self.llvm_type(target_ty).into_int_type();
                self.builder.build_int_truncate_or_bit_cast(v.into_int_value(), ity, "toui").unwrap().into()
            }
            Type::Int64 | Type::UInt64 => {
                self.builder.build_call(self.rt.unbox_int64, &[ptr_val.into()], "ui64").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Float32 | Type::Float64 => {
                let v = self.builder.build_call(self.rt.unbox_float64, &[ptr_val.into()], "uf64").unwrap()
                    .try_as_basic_value().unwrap_basic();
                if matches!(target_ty, Type::Float32) {
                    self.builder.build_float_trunc(v.into_float_value(), self.context.f32_type(), "tof32").unwrap().into()
                } else {
                    v
                }
            }
            Type::Str => {
                self.builder.build_call(self.rt.unbox_ptr, &[ptr_val.into()], "ustr").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Object(_) | Type::Array(_) | Type::FixedArray(_) | Type::Function { .. } => {
                self.builder.build_call(self.rt.unbox_ptr, &[ptr_val.into()], "uptr").unwrap()
                    .try_as_basic_value().unwrap_basic()
            }
            // Already tagged — return as-is
            Type::Union(_) | Type::TypeVar(_) => ptr,
            _ => ptr,
        }
    }

    /// Build a stack-allocated TaggedVal from a value + type, return its alloca ptr.
    pub(crate) fn build_tagged_val_alloca(&mut self, val: &BasicValueEnum<'ctx>, val_ty: &Type) -> PointerValue<'ctx> {
        // TaggedVal layout: { tag: u8, pad: [u8;7], payload: u64 } = 16 bytes total
        let i8_ty = self.context.i8_type();
        let i64_ty = self.context.i64_type();
        let tagged_ty = self.context.struct_type(&[i8_ty.into(), i8_ty.array_type(7).into(), i64_ty.into()], false);
        let alloca = self.builder.build_alloca(tagged_ty, "tv").unwrap();

        let tag = Self::type_tag(val_ty);
        let tag_val = i8_ty.const_int(tag as u64, false);
        let tag_ptr = self.builder.build_struct_gep(tagged_ty, alloca, 0, "tv_tag").unwrap();
        self.builder.build_store(tag_ptr, tag_val).unwrap();

        // Write payload as u64.
        let payload_ptr = self.builder.build_struct_gep(tagged_ty, alloca, 2, "tv_payload").unwrap();
        let payload: inkwell::values::IntValue<'ctx> = match val_ty {
            Type::Null => i64_ty.const_zero(),
            Type::Bool => {
                let b = if val.is_int_value() {
                    self.builder.build_int_z_extend(val.into_int_value(), i64_ty, "bext").unwrap()
                } else { i64_ty.const_zero() };
                b
            }
            Type::Int8 | Type::Int16 | Type::Int32 | Type::UInt8 | Type::UInt16 | Type::UInt32 => {
                if val.is_int_value() {
                    self.builder.build_int_z_extend_or_bit_cast(val.into_int_value(), i64_ty, "iext").unwrap()
                } else { i64_ty.const_zero() }
            }
            Type::Int64 | Type::UInt64 => {
                if val.is_int_value() { val.into_int_value() } else { i64_ty.const_zero() }
            }
            Type::Float32 => {
                let fv = if val.is_float_value() { val.into_float_value() }
                    else { self.context.f32_type().const_float(0.0) };
                // Extend to f64 then bitcast bits to i64
                let fv64 = self.builder.build_float_ext(fv, self.context.f64_type(), "f32ext").unwrap();
                self.builder.build_bit_cast(fv64, i64_ty, "fbits").unwrap().into_int_value()
            }
            Type::Float64 => {
                let fv = if val.is_float_value() { val.into_float_value() }
                    else { self.context.f64_type().const_float(0.0) };
                // Bitcast f64 bits to i64 (reinterpret, not convert)
                self.builder.build_bit_cast(fv, i64_ty, "fbits").unwrap().into_int_value()
            }
            _ => {
                // Pointer types: str, array, object, function — store pointer as u64
                if val.is_pointer_value() {
                    self.builder.build_ptr_to_int(val.into_pointer_value(), i64_ty, "pti").unwrap()
                } else { i64_ty.const_zero() }
            }
        };
        self.builder.build_store(payload_ptr, payload).unwrap();
        alloca
    }

    pub(crate) fn compile_ir_box(&mut self, val: BasicValueEnum<'ctx>, ty: &Type) -> BasicValueEnum<'ctx> {
        // Heap-box (see compile_ir_coerce) so the boxed value can safely escape.
        self.box_value(val, ty)
    }

    pub(crate) fn compile_ir_unbox(&mut self, val: BasicValueEnum<'ctx>, result_ty: &Type) -> BasicValueEnum<'ctx> {
        self.unbox_tagged_val_to_type(val, result_ty)
    }

    /// Unbox a tagged union value to a concrete type.
    pub(crate) fn unbox_tagged_val_to_type(&mut self, tagged: BasicValueEnum<'ctx>, ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        if !tagged.is_pointer_value() { return tagged; }
        let ptr = tagged.into_pointer_value();
        match ty {
            Type::Int32 | Type::UInt32 => {
                self.builder.build_call(self.rt.unbox_int32, &[ptr.into()], "ir_u32")
                    .unwrap().try_as_basic_value().unwrap_basic()
            }
            Type::Int64 | Type::UInt64 => {
                self.builder.build_call(self.rt.unbox_int64, &[ptr.into()], "ir_u64")
                    .unwrap().try_as_basic_value().unwrap_basic()
            }
            Type::Float64 | Type::Float32 => {
                self.builder.build_call(self.rt.unbox_float64, &[ptr.into()], "ir_uf64")
                    .unwrap().try_as_basic_value().unwrap_basic()
            }
            Type::Bool => {
                let i8v = self.builder.build_call(self.rt.unbox_bool, &[ptr.into()], "ir_ubool")
                    .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
                self.builder.build_int_truncate_or_bit_cast(i8v, self.context.bool_type(), "ub_bool").unwrap().into()
            }
            Type::Str => {
                self.builder.build_call(self.rt.unbox_ptr, &[ptr.into()], "ir_ustr")
                    .unwrap().try_as_basic_value().unwrap_basic()
            }
            Type::Array(_) | Type::FixedArray(_) | Type::Object(_) | Type::Function { .. } => {
                self.builder.build_call(self.rt.unbox_ptr, &[ptr.into()], "ir_uptr")
                    .unwrap().try_as_basic_value().unwrap_basic()
            }
            Type::Null => ptr_ty.const_null().into(),
            _ => tagged, // pass through for union/unknown
        }
    }

}
