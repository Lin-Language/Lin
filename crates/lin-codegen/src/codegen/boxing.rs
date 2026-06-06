use super::builder_ext::BuilderExt;
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, PointerValue,
};
use inkwell::AddressSpace;

use lin_check::types::Type;
use super::Codegen;

impl<'ctx> Codegen<'ctx> {
    /// Box one argument into the uniform boxed closure-call ABI representation: a TaggedVal*
    /// (ptr). EVERY indirect/closure call passes its args this way, because every function
    /// value is stored as a boxed-ABI wrapper (`__cls_wrapb_*` / `__papp_*`) that declares all
    /// params `ptr` and unboxes each to its concrete type.
    ///
    /// An argument whose Lin type is already a union/Json is itself a boxed `ptr` — passed
    /// through unchanged to avoid double-boxing. A concrete scalar / raw String*/Array*/Object*
    /// value is boxed. This keeps both ends of every indirect call agreeing on the all-ptr ABI
    /// regardless of which args the IR pre-boxed (the IR only boxes up to the value's *declared*
    /// param arity, e.g. one for an opaque `Function`, so extra args reach here unboxed — the
    /// wrapper-ABI bug).
    pub(crate) fn box_arg_for_closure_abi(
        &mut self,
        val: BasicMetadataValueEnum<'ctx>,
        arg_ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        let basic: BasicValueEnum<'ctx> = match val {
            BasicMetadataValueEnum::IntValue(v) => v.into(),
            BasicMetadataValueEnum::FloatValue(v) => v.into(),
            BasicMetadataValueEnum::PointerValue(v) => v.into(),
            BasicMetadataValueEnum::ArrayValue(v) => v.into(),
            BasicMetadataValueEnum::StructValue(v) => v.into(),
            BasicMetadataValueEnum::VectorValue(v) => v.into(),
            _ => self.context.ptr_type(AddressSpace::default()).const_null().into(),
        };
        // Already a boxed Json/union value (a ptr) — pass through.
        if Self::is_union_type(arg_ty) {
            return basic;
        }
        self.box_value(basic, arg_ty)
    }

    /// Box a value into a tagged union pointer (TaggedVal*).
    /// For concrete types, allocates and fills a TaggedVal with the appropriate tag.
    /// For TypeVar, dispatches on the actual LLVM type (int/float/pointer) to pick the right box call.
    pub(crate) fn box_value(&mut self, val: BasicValueEnum<'ctx>, val_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr = match val_ty {
            Type::Null => self.builder.call(self.rt.box_null, &[], "boxnull")
                .try_as_basic_value().unwrap_basic(),
            Type::Bool => {
                let i8v = if val.is_int_value() {
                    // Bool is i1; zero-extend to i8 for lin_box_bool(i8).
                    self.builder.int_z_extend_or_bit_cast(val.into_int_value(), self.context.i8_type(), "btoi8").into()
                } else { val };
                self.builder.call(self.rt.box_bool, &[i8v.into()], "boxbool")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Int8 | Type::Int16 | Type::Int32 => {
                let i32v = self.builder.int_s_extend_or_bit_cast(val.into_int_value(), self.context.i32_type(), "toi32");
                self.builder.call(self.rt.box_int32, &[i32v.into()], "boxi32")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::UInt8 | Type::UInt16 | Type::UInt32 => {
                // Zero-extend to a (always-positive) i64 and box as TAG_INT64 so the value
                // reads back correctly: a u32 >= 2^31 would be a negative i32 if boxed as
                // TAG_INT32, breaking display/JSON/eq/cmp. The zero-extended i64 is positive.
                let i64v = self.builder.int_z_extend_or_bit_cast(val.into_int_value(), self.context.i64_type(), "tou64");
                self.builder.call(self.rt.box_int64, &[i64v.into()], "boxu_as_i64")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Int64 => {
                let i64v = val.into_int_value();
                self.builder.call(self.rt.box_int64, &[i64v.into()], "boxi64")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::UInt64 => {
                // Box as TAG_UINT64 so the payload is read back unsigned (a u64 >= 2^63 would
                // be negative if read as i64).
                let i64v = val.into_int_value();
                self.builder.call(self.rt.box_uint64, &[i64v.into()], "boxu64")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Float32 => {
                let f64v = self.builder.float_ext(val.into_float_value(), self.context.f64_type(), "f32tof64");
                self.builder.call(self.rt.box_float64, &[f64v.into()], "boxf64")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Float64 => {
                self.builder.call(self.rt.box_float64, &[val.into()], "boxf64")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Str | Type::StrLit(_) => {
                self.builder.call(self.rt.box_str, &[val.into()], "boxstr")
                    .try_as_basic_value().unwrap_basic()
            }
            // A SEALED SCALAR RECORD is a packed struct, NOT a LinObject — box_object would
            // treat its ptr as a LinObject header and corrupt. Materialize to a fresh boxed
            // LinObject first, then box that as TAG_OBJECT. (This is the same conversion the
            // Coerce(sealed → Json) boundary performs; it is the safety net for any path that
            // boxes a sealed value directly, e.g. heterogeneous array elements or closure args.)
            Type::Object { .. } if Self::sealed_scalar_fields(val_ty).is_some() => {
                let fields = Self::sealed_scalar_fields(val_ty).unwrap().clone();
                let obj = self.sealed_materialize_to_object(val, &fields);
                self.builder.call(self.rt.box_object, &[obj.into()], "boxsealed")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Object { .. } => {
                self.builder.call(self.rt.box_object, &[val.into()], "boxobj")
                    .try_as_basic_value().unwrap_basic()
            }
            // Typed index-signature map (`{ String: T }`, ADR-055): box the LinMap* as TAG_MAP.
            Type::Map(_) => {
                let box_map_fn = self.get_or_declare_fn("lin_box_map",
                    self.context.ptr_type(AddressSpace::default()).fn_type(&[self.context.ptr_type(AddressSpace::default()).into()], false));
                self.builder.call(box_map_fn, &[val.into()], "boxmap")
                    .try_as_basic_value().unwrap_basic()
            }
            // A SEALED-RECORD ARRAY (Stage 3) is a contiguous unboxed buffer (elem_tag 0xFE), NOT a
            // tagged/flat array the dynamic Json machinery (lin_array_get_tagged / lin_to_string /
            // lin_tagged_eq / combinators) can read. At the Json boundary MATERIALIZE it to a tagged
            // `Object[]` (each element → a boxed LinObject) via the per-type element materializer,
            // then box the tagged array. This is the fail-safe boxed view (§3 boundary).
            Type::Array(_) if val.is_pointer_value() && Self::sealed_array_elem(val_ty).is_some() => {
                let tagged = self.sealed_array_to_tagged(val, val_ty);
                self.builder.call(self.rt.box_array, &[tagged.into()], "boxsarr")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Array(_) if val.is_pointer_value() => {
                // Box the LinArray* directly (flat or tagged). The elem_tag field in LinArray
                // lets runtime functions (lin_array_get_tagged, lin_push_dyn, etc.) dispatch
                // correctly without needing a separate conversion copy.
                self.builder.call(self.rt.box_array, &[val.into()], "boxarr")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_) => {
                // Iterator values have already been converted to tagged arrays by the intrinsic.
                self.builder.call(self.rt.box_array, &[val.into()], "boxarr")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Function { .. } => {
                self.builder.call(self.rt.box_function, &[val.into()], "boxfn")
                    .try_as_basic_value().unwrap_basic()
            }
            // Union type — if value is a pointer, box as object (most common case).
            // If it's already a tagged pointer, return as-is.
            Type::Union(variants) => {
                if val.is_pointer_value() {
                    // If all variants are Object types, this is a LinObject*.
                    let all_objects = variants.iter().all(|v| matches!(v, Type::Object { .. }));
                    if all_objects {
                        self.builder.call(self.rt.box_object, &[val.into()], "boxobj")
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
                        let i32v = self.builder.int_s_extend_or_bit_cast(iv, i32_ty, "tvi32");
                        self.builder.call(self.rt.box_int32, &[i32v.into()], "tvboxi32")
                            .try_as_basic_value().unwrap_basic()
                    } else {
                        let i64v = self.builder.int_s_extend_or_bit_cast(iv, i64_ty, "tvi64");
                        self.builder.call(self.rt.box_int64, &[i64v.into()], "tvboxi64")
                            .try_as_basic_value().unwrap_basic()
                    }
                } else if val.is_float_value() {
                    let fv = val.into_float_value();
                    let f64_ty = self.context.f64_type();
                    let f64v = self.builder.float_ext(fv, f64_ty, "tvf64");
                    self.builder.call(self.rt.box_float64, &[f64v.into()], "tvboxf64")
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
                let v = self.builder.call(self.rt.unbox_bool, &[ptr_val.into()], "ubool")
                    .try_as_basic_value().unwrap_basic();
                // Convert i8 to i1
                self.builder.int_truncate(v.into_int_value(), self.context.bool_type(), "utobool").into()
            }
            Type::Int8 | Type::Int16 | Type::Int32 => {
                let v = self.builder.call(self.rt.unbox_int32, &[ptr_val.into()], "ui32")
                    .try_as_basic_value().unwrap_basic();
                let ity = self.llvm_type(target_ty).into_int_type();
                self.builder.int_truncate_or_bit_cast(v.into_int_value(), ity, "toi").into()
            }
            Type::UInt8 | Type::UInt16 | Type::UInt32 => {
                // UInt8/16/32 are boxed as TAG_INT64 (zero-extended). Read the full i64 payload
                // and truncate to the target width — this preserves all value bits.
                let v = self.builder.call(self.rt.unbox_int64, &[ptr_val.into()], "uu64")
                    .try_as_basic_value().unwrap_basic();
                let ity = self.llvm_type(target_ty).into_int_type();
                self.builder.int_truncate_or_bit_cast(v.into_int_value(), ity, "toui").into()
            }
            Type::Int64 => {
                self.builder.call(self.rt.unbox_int64, &[ptr_val.into()], "ui64")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::UInt64 => {
                // Boxed as TAG_UINT64; the bits are identical to TAG_INT64 so unbox_int64
                // returns the correct 64-bit pattern (the value's signedness only matters at
                // display/compare time, handled by the runtime tag).
                self.builder.call(self.rt.unbox_uint64, &[ptr_val.into()], "uu64v")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Float32 | Type::Float64 => {
                let v = self.builder.call(self.rt.unbox_float64, &[ptr_val.into()], "uf64")
                    .try_as_basic_value().unwrap_basic();
                if matches!(target_ty, Type::Float32) {
                    self.builder.float_trunc(v.into_float_value(), self.context.f32_type(), "tof32").into()
                } else {
                    v
                }
            }
            Type::Str | Type::StrLit(_) => {
                self.builder.call(self.rt.unbox_ptr, &[ptr_val.into()], "ustr")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Object { .. } if Self::sealed_scalar_fields(target_ty).is_some() => {
                let fields = Self::sealed_scalar_fields(target_ty).unwrap().clone();
                self.sealed_project_from(ptr, &Type::TypeVar(u32::MAX), &fields)
            }
            // Keep in sync with `unbox_tagged_val_to_type` below. A typed index-signature map
            // (`{ String: T }`, `Type::Map`) is boxed as TAG_MAP whose payload is the raw
            // `LinMap*`; unbox it back to that pointer here too, or it leaks through the
            // closure-ABI wrapper as a TaggedVal box masquerading as a `LinMap*`.
            Type::Object { .. } | Type::Array(_) | Type::FixedArray(_) | Type::Function { .. } | Type::Map(_) => {
                self.builder.call(self.rt.unbox_ptr, &[ptr_val.into()], "uptr")
                    .try_as_basic_value().unwrap_basic()
            }
            // Already tagged — return as-is
            Type::Union(_) | Type::TypeVar(_) => ptr,
            _ => ptr,
        }
    }

    /// Compute the 64-bit TaggedVal payload bits for `val` of type `val_ty`. This is the value
    /// half of a TaggedVal (scalars zero/sign-extended or float-bitcast to i64; pointers
    /// `ptrtoint`-ed). Shared by `build_tagged_val_alloca` and the inline object-construction
    /// fast path, so the two never drift on how a payload is encoded.
    pub(crate) fn tagged_payload_i64(&mut self, val: &BasicValueEnum<'ctx>, val_ty: &Type) -> inkwell::values::IntValue<'ctx> {
        let i64_ty = self.context.i64_type();
        match val_ty {
            Type::Null => i64_ty.const_zero(),
            Type::Bool => {
                if val.is_int_value() {
                    self.builder.int_z_extend(val.into_int_value(), i64_ty, "bext")
                } else { i64_ty.const_zero() }
            }
            Type::Int8 | Type::Int16 | Type::Int32 | Type::UInt8 | Type::UInt16 | Type::UInt32 => {
                if val.is_int_value() {
                    self.builder.int_z_extend_or_bit_cast(val.into_int_value(), i64_ty, "iext")
                } else { i64_ty.const_zero() }
            }
            Type::Int64 | Type::UInt64 => {
                if val.is_int_value() { val.into_int_value() } else { i64_ty.const_zero() }
            }
            Type::Float32 => {
                let fv = if val.is_float_value() { val.into_float_value() }
                    else { self.context.f32_type().const_float(0.0) };
                // Extend to f64 then bitcast bits to i64
                let fv64 = self.builder.float_ext(fv, self.context.f64_type(), "f32ext");
                self.builder.bit_cast(fv64, i64_ty, "fbits").into_int_value()
            }
            Type::Float64 => {
                let fv = if val.is_float_value() { val.into_float_value() }
                    else { self.context.f64_type().const_float(0.0) };
                // Bitcast f64 bits to i64 (reinterpret, not convert)
                self.builder.bit_cast(fv, i64_ty, "fbits").into_int_value()
            }
            _ => {
                // Pointer types: str, array, object, function — store pointer as u64
                if val.is_pointer_value() {
                    self.builder.ptr_to_int(val.into_pointer_value(), i64_ty, "pti")
                } else { i64_ty.const_zero() }
            }
        }
    }

    /// Build a stack-allocated TaggedVal from a value + type, return its alloca ptr.
    pub(crate) fn build_tagged_val_alloca(&mut self, val: &BasicValueEnum<'ctx>, val_ty: &Type) -> PointerValue<'ctx> {
        // TaggedVal layout: { tag: u8, pad: [u8;7], payload: u64 } = 16 bytes total
        let i8_ty = self.context.i8_type();
        let i64_ty = self.context.i64_type();
        let tagged_ty = self.context.struct_type(&[i8_ty.into(), i8_ty.array_type(7).into(), i64_ty.into()], false);
        let alloca = self.builder.alloca(tagged_ty, "tv");

        let tag = Self::type_tag(val_ty);
        let tag_val = i8_ty.const_int(tag as u64, false);
        let tag_ptr = self.builder.struct_gep(tagged_ty, alloca, 0, "tv_tag");
        self.builder.store(tag_ptr, tag_val);

        // Write payload as u64.
        let payload_ptr = self.builder.struct_gep(tagged_ty, alloca, 2, "tv_payload");
        let payload = self.tagged_payload_i64(val, val_ty);
        self.builder.store(payload_ptr, payload);
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
            Type::Int32 => {
                self.builder.call(self.rt.unbox_int32, &[ptr.into()], "ir_i32").try_as_basic_value().unwrap_basic()
            }
            Type::UInt32 => {
                // Boxed as TAG_INT64 (zero-extended); read i64 and truncate to i32 width.
                let v = self.builder.call(self.rt.unbox_int64, &[ptr.into()], "ir_u32_64").try_as_basic_value().unwrap_basic().into_int_value();
                self.builder.int_truncate_or_bit_cast(v, self.context.i32_type(), "ir_u32").into()
            }
            Type::Int64 => {
                self.builder.call(self.rt.unbox_int64, &[ptr.into()], "ir_i64").try_as_basic_value().unwrap_basic()
            }
            Type::UInt64 => {
                self.builder.call(self.rt.unbox_uint64, &[ptr.into()], "ir_u64").try_as_basic_value().unwrap_basic()
            }
            Type::Float64 | Type::Float32 => {
                self.builder.call(self.rt.unbox_float64, &[ptr.into()], "ir_uf64").try_as_basic_value().unwrap_basic()
            }
            Type::Bool => {
                let i8v = self.builder.call(self.rt.unbox_bool, &[ptr.into()], "ir_ubool").try_as_basic_value().unwrap_basic().into_int_value();
                self.builder.int_truncate_or_bit_cast(i8v, self.context.bool_type(), "ub_bool").into()
            }
            Type::Str | Type::StrLit(_) => {
                self.builder.call(self.rt.unbox_ptr, &[ptr.into()], "ir_ustr").try_as_basic_value().unwrap_basic()
            }
            // Unboxing a boxed Json/object into a SEALED scalar record target = a PROJECTION:
            // the boxed value is a TaggedVal* (or raw LinObject*); project it into a fresh sealed
            // struct. Routed through the central projection helper so the source representation is
            // handled correctly (it unboxes a union box to the raw LinObject internally).
            Type::Object { .. } if Self::sealed_scalar_fields(ty).is_some() => {
                let fields = Self::sealed_scalar_fields(ty).unwrap().clone();
                // The incoming `tagged` is a boxed value (Json). Use the union-typed projection
                // path: sealed_project_from unboxes a union source to the raw LinObject itself.
                self.sealed_project_from(tagged, &Type::TypeVar(u32::MAX), &fields)
            }
            // Typed index-signature map (`{ String: T }`, ADR-055): a `m[k]` whose value type is
            // itself a Map is boxed as TAG_MAP. Unbox the payload back to the raw `LinMap*` so a
            // nested store (`m[a][b] = v`) and a chained read both operate on the SHARED inner
            // container, not on the TaggedVal box (which a missing arm here would leak through,
            // making nested-map mutation a no-op).
            Type::Array(_) | Type::FixedArray(_) | Type::Object { .. } | Type::Function { .. } | Type::Map(_) => {
                self.builder.call(self.rt.unbox_ptr, &[ptr.into()], "ir_uptr").try_as_basic_value().unwrap_basic()
            }
            Type::Null => ptr_ty.const_null().into(),
            _ => tagged, // pass through for union/unknown
        }
    }

}