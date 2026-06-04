use super::builder_ext::BuilderExt;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::BasicValueEnum;
use inkwell::AddressSpace;

use lin_check::types::Type;
use lin_common::tags::*;
use super::Codegen;

impl<'ctx> Codegen<'ctx> {
    pub(crate) fn llvm_type(&self, ty: &Type) -> BasicTypeEnum<'ctx> {
        match ty {
            Type::Bool => self.context.bool_type().into(),
            Type::Int8 => self.context.i8_type().into(),
            Type::Int16 => self.context.i16_type().into(),
            Type::Int32 => self.context.i32_type().into(),
            Type::Int64 => self.context.i64_type().into(),
            Type::UInt8 => self.context.i8_type().into(),
            Type::UInt16 => self.context.i16_type().into(),
            Type::UInt32 => self.context.i32_type().into(),
            Type::UInt64 => self.context.i64_type().into(),
            Type::Float32 => self.context.f32_type().into(),
            Type::Float64 => self.context.f64_type().into(),
            Type::Str | Type::StrLit(_) => self.string_ptr_type.into(),
            Type::Null => {
                // Null is represented as a pointer (null ptr), same as Union/TypeVar.
                // This ensures Null-typed vars can hold tagged values assigned later.
                self.context.ptr_type(AddressSpace::default()).into()
            }
            Type::Array(_) | Type::FixedArray(_) => self.array_ptr_type.into(),
            // Stage 0.5: codegen IGNORES the `sealed` marker — every object, sealed or not, is the
            // boxed string-keyed `LinObject` pointer, exactly as before. Stage 1 will branch here.
            Type::Object { .. } => self.context.ptr_type(AddressSpace::default()).into(),
            Type::Union(_) => {
                // Tagged union: { i8 tag, [8 x i8] payload } — 9 bytes total.
                // We use an opaque pointer to a heap-allocated tagged value.
                self.context.ptr_type(AddressSpace::default()).into()
            }
            Type::Function { .. } => {
                // Closures are represented as { fn_ptr, env_ptr } pairs.
                // Returns a pointer to the closure struct.
                self.context.ptr_type(AddressSpace::default()).into()
            }
            Type::Iterator(_) => self.context.ptr_type(AddressSpace::default()).into(),
            // Shared<T> is a boxed TaggedVal*(TAG_SHARED) at runtime — an opaque pointer.
            Type::Shared(_) => self.context.ptr_type(AddressSpace::default()).into(),
            // Stream<T> is a boxed TaggedVal*(TAG_STREAM) at runtime — an opaque pointer.
            Type::Stream(_) => self.context.ptr_type(AddressSpace::default()).into(),
            Type::Never => self.context.i8_type().into(), // unreachable
            Type::TypeVar(_) => {
                // Unresolved type var — use opaque pointer (Json/"any" type at runtime)
                self.context.ptr_type(AddressSpace::default()).into()
            }
            Type::Named(_) => {
                // Named recursive type reference — use opaque pointer (heap-allocated object)
                self.context.ptr_type(AddressSpace::default()).into()
            }
        }
    }

    pub(crate) fn llvm_param_type(&self, ty: &Type) -> BasicMetadataTypeEnum<'ctx> {
        self.llvm_type(ty).into()
    }

    /// True if `ty` is a union or TypeVar (i.e., needs tagged representation). `Shared<T>` is
    /// included: its runtime value is a boxed `TaggedVal*(TAG_SHARED)`, so box/unbox sites must
    /// treat it as an already-boxed tagged value (never re-box or reinterpret it as a scalar).
    pub(crate) fn is_union_type(ty: &Type) -> bool {
        matches!(ty, Type::Union(_) | Type::TypeVar(_) | Type::Named(_) | Type::Shared(_) | Type::Stream(_))
    }

    /// Returns the LLVM struct type for a closure header.
    ///
    /// Layout (32 bytes):
    ///   field 0: i32  refcount
    ///   field 1: i32  _pad
    ///   field 2: ptr  fn_ptr
    ///   field 3: ptr  env_ptr
    ///
    /// A trailing u64 env_size lives at offset 24 and is written directly via GEP on the
    /// raw allocation rather than as a struct field, because the closure struct type is
    /// referenced in many places and keeping it to 4 fields keeps all the call-site GEPs
    /// consistent.  The env_size write is done once at closure creation.
    pub(crate) fn closure_struct_type(&self) -> inkwell::types::StructType<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i32_ty = self.context.i32_type();
        self.context.struct_type(&[i32_ty.into(), i32_ty.into(), ptr_ty.into(), ptr_ty.into()], false)
    }

    /// Concrete (non-boxed) reference-counted heap types: the IR lowerer tracks these for
    /// scope-exit release (mirrors `lin_ir`'s `is_rc_type`), and a cell/global holding one
    /// owns an independent reference (the lowerer retains on store). Boxed Json/union/Named
    /// values are excluded: they follow the legacy borrow model where the value's true owner
    /// frees it, so a cell/global must NOT release them on reassignment (double-free).
    ///
    /// This MUST stay in lockstep with `lin_ir::lower::is_rc_type`: codegen releases the old
    /// value on reassignment only for types the lowerer also retained on store. A type present
    /// here but absent there would be released without a matching retain — a refcount underflow.
    /// (`Iterator` is deliberately omitted for that reason: the lowerer does not retain it.)
    pub(crate) fn ty_is_concrete_rc(ty: &Type) -> bool {
        matches!(
            ty,
            Type::Str
                | Type::StrLit(_)
                | Type::Array(_)
                | Type::FixedArray(_)
                | Type::Object { .. }
                | Type::Function { .. }
        )
    }

    /// Tag for how a value of `ty` is BOXED as a scalar TaggedVal — i.e. the byte stored in
    /// the tag field and matched by `is`-checks. This must EXACTLY mirror `box_value` /
    /// `tagged_payload_i64` so the runtime reads the payload back the same way it was written.
    ///
    /// Floats: both Float32 and Float64 box as TAG_FLOAT64 with an f64-bits payload (codegen
    /// fpext's a Float32 to f64 before boxing), so TAG_FLOAT32 (a flat-array elem_tag only)
    /// must NEVER be emitted for a boxed scalar — doing so made the runtime read an f64-bits
    /// payload as `f32::from_bits(payload as u32)` → garbage, and made `x is Float64` compare
    /// 5 against a value tagged 4 → dead arm.
    pub(crate) fn type_tag(ty: &Type) -> u8 {
        match ty {
            Type::Null => TAG_NULL,
            Type::Bool => TAG_BOOL,
            Type::Int8 | Type::Int16 | Type::Int32 => TAG_INT32,
            // UInt8/16/32 are zero-extended and boxed as TAG_INT64 (always-positive i64) so
            // a u32 >= 2^31 reads back correctly. Must match box_value / build_tagged_val_alloca.
            Type::UInt8 | Type::UInt16 | Type::UInt32 => TAG_INT64,
            Type::Int64 => TAG_INT64,
            // UInt64 — read back unsigned.
            Type::UInt64 => TAG_UINT64,
            // Both float widths box as f64 bits (see doc above).
            Type::Float32 | Type::Float64 => TAG_FLOAT64,
            Type::Str | Type::StrLit(_) => TAG_STR,
            Type::Object { .. } => TAG_OBJECT,
            Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_) => TAG_ARRAY,
            Type::Function { .. } => TAG_FUNCTION,
            _ => TAG_NULL,
        }
    }

    /// Byte size of `SEALED_HEADER` (refcount u32 + u32 pad). Kept in lockstep with
    /// `lin_runtime::sealed::SEALED_HEADER` (8). Sealed-record field payload begins here.
    pub(crate) const SEALED_HEADER: u64 = 8;

    /// True when `ty` is an unboxed scalar field of a sealed scalar record: a fixed-width
    /// numeric (mirrors `is_flat_scalar`) OR `Bool`. These are the ONLY field kinds that
    /// qualify a named record for the unboxed struct layout — any String/Object/Array/union/
    /// nested/Json field keeps the whole record boxed (Stage 1 scope).
    pub(crate) fn is_sealed_scalar_field(ty: &Type) -> bool {
        Self::is_flat_scalar(ty) || matches!(ty, Type::Bool)
    }

    /// THE sealed-scalar gate (sealed-records Stage 1). Returns `Some(fields)` iff `ty` is a
    /// `Type::Object { sealed: true }` whose fields are ALL unboxed scalars — the only types that
    /// get the unboxed packed-struct layout. Returns `None` (→ keep the boxed `LinObject` path)
    /// for: an unsealed object (anonymous literal/inferred shape), any object with a heap field
    /// (String/Object/Array/union/nested), and every non-object type. FAIL SAFE: when unsure,
    /// `None` (boxed). The field order is the TYPE DECLARATION's `IndexMap` order, preserved by
    /// Stage 0.5 resolution — this fixes a single canonical physical layout per type.
    pub(crate) fn sealed_scalar_fields(ty: &Type) -> Option<&indexmap::IndexMap<String, Type>> {
        match ty {
            Type::Object { fields, sealed: true } if !fields.is_empty()
                && fields.values().all(Self::is_sealed_scalar_field) =>
            {
                Some(fields)
            }
            _ => None,
        }
    }

    /// Byte offset (from the struct base, including the header) of `field` within a sealed scalar
    /// record, and the total struct byte size. Fields are packed in declaration order with NATURAL
    /// alignment (each scalar aligned to its own width); the struct is padded to an 8-byte multiple
    /// so arrays/successive allocs stay aligned. Returns `(offset_of_field, total_size)`.
    /// Panics if `field` is not in the record (a compile error already rules this out upstream).
    pub(crate) fn sealed_field_layout(fields: &indexmap::IndexMap<String, Type>, field: &str) -> (u64, u64) {
        let mut offset = Self::SEALED_HEADER;
        let mut found: Option<u64> = None;
        for (k, fty) in fields.iter() {
            let sz = fty.bit_width().map(|b| (b as u64) / 8).unwrap_or(1).max(1);
            // Natural alignment = the field's own size (1/2/4/8). Round offset up.
            let align = sz;
            offset = (offset + align - 1) / align * align;
            if k == field {
                found = Some(offset);
            }
            offset += sz;
        }
        // Pad total to 8.
        let total = (offset + 7) / 8 * 8;
        (found.unwrap_or_else(|| panic!("sealed_field_layout: field {field:?} not in record")), total)
    }

    /// Total byte size of a sealed scalar record (header + packed fields, padded to 8).
    pub(crate) fn sealed_struct_size(fields: &indexmap::IndexMap<String, Type>) -> u64 {
        let mut offset = Self::SEALED_HEADER;
        for fty in fields.values() {
            let sz = fty.bit_width().map(|b| (b as u64) / 8).unwrap_or(1).max(1);
            offset = (offset + sz - 1) / sz * sz;
            offset += sz;
        }
        (offset + 7) / 8 * 8
    }

    /// Returns true when the element type maps to a flat unboxed scalar array.
    /// Only concrete fixed-width numeric scalars qualify — not Bool (stored as i1,
    /// awkward to pack densely), not pointers, not unions.
    pub(crate) fn is_flat_scalar(ty: &Type) -> bool {
        matches!(ty,
            Type::Int8 | Type::UInt8 |
            Type::Int16 | Type::UInt16 |
            Type::Int32 | Type::UInt32 |
            Type::Int64 | Type::UInt64 |
            Type::Float32 | Type::Float64
        )
    }

    /// Suffix used in runtime function names for flat array variants.
    pub(crate) fn flat_suffix(ty: &Type) -> &'static str {
        match ty {
            Type::Int8 => "i8",
            Type::UInt8 => "u8",
            Type::Int16 => "i16",
            Type::UInt16 => "u16",
            Type::Int32 => "i32",
            Type::UInt32 => "u32",
            Type::Int64 => "i64",
            Type::UInt64 => "u64",
            Type::Float32 => "f32",
            Type::Float64 => "f64",
            _ => unreachable!("flat_suffix called with non-scalar type"),
        }
    }

    /// Narrow/widen an integer value to the integer width of `target_ty`. Non-integer
    /// values and non-integer targets are returned unchanged. Used to reconcile a runtime
    /// intrinsic that returns a fixed width (e.g. lin_array_length → i64) with a declared
    /// result type of a different width (e.g. Int32).
    pub(crate) fn coerce_int_width(&self, val: BasicValueEnum<'ctx>, target_ty: &Type) -> BasicValueEnum<'ctx> {
        if !val.is_int_value() || !target_ty.is_integer() {
            return val;
        }
        let iv = val.into_int_value();
        let target_llvm = self.llvm_type(target_ty).into_int_type();
        let iv_bits = iv.get_type().get_bit_width();
        let tgt_bits = target_llvm.get_bit_width();
        if tgt_bits == iv_bits {
            val
        } else if tgt_bits > iv_bits {
            if target_ty.is_signed() {
                self.builder.int_s_extend(iv, target_llvm, "ir_len_sext").into()
            } else {
                self.builder.int_z_extend(iv, target_llvm, "ir_len_zext").into()
            }
        } else {
            self.builder.int_truncate(iv, target_llvm, "ir_len_trunc").into()
        }
    }

    /// Return the i8 constant for the runtime tag a value of `ty` is boxed under (used by
    /// `is`-checks in match.rs). Delegates to `type_tag` so the two can never disagree — the
    /// previous hand-copied table is what let Float32/Float64 drift to TAG_FLOAT32 (4) here
    /// while box_value wrote TAG_FLOAT64 (5).
    pub(crate) fn type_tag_const(&self, ty: &Type) -> inkwell::values::IntValue<'ctx> {
        let i8_ty = self.context.i8_type();
        // Types that are never boxed as a recognised scalar tag (TypeVar/Union/etc.) used to
        // map to a sentinel 0xFF; preserve that so an `is`-check never spuriously matches.
        let tag: u8 = match ty {
            Type::Null | Type::Bool | Type::Int8 | Type::Int16 | Type::Int32
            | Type::UInt8 | Type::UInt16 | Type::UInt32 | Type::Int64 | Type::UInt64
            | Type::Float32 | Type::Float64 | Type::Str | Type::StrLit(_) | Type::Object { .. }
            | Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_) | Type::Function { .. } => {
                Self::type_tag(ty)
            }
            _ => 0xFF,
        };
        i8_ty.const_int(tag as u64, false)
    }

}