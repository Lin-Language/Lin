use super::builder_ext::BuilderExt;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::BasicValueEnum;
use inkwell::AddressSpace;

use lin_check::types::Type;
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
            Type::Str => self.string_ptr_type.into(),
            Type::Null => {
                // Null is represented as a pointer (null ptr), same as Union/TypeVar.
                // This ensures Null-typed vars can hold tagged values assigned later.
                self.context.ptr_type(AddressSpace::default()).into()
            }
            Type::Array(_) | Type::FixedArray(_) => self.array_ptr_type.into(),
            Type::Object(_) => self.context.ptr_type(AddressSpace::default()).into(),
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
        matches!(ty, Type::Union(_) | Type::TypeVar(_) | Type::Named(_) | Type::Shared(_))
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
                | Type::Array(_)
                | Type::FixedArray(_)
                | Type::Object(_)
                | Type::Function { .. }
        )
    }

    /// Tag constant for a concrete type (for `is` type checks).
    pub(crate) fn type_tag(ty: &Type) -> u8 {
        match ty {
            Type::Null => 0,
            Type::Bool => 1,
            Type::Int8 | Type::Int16 | Type::Int32 => 2,
            // UInt8/16/32 are zero-extended and boxed as TAG_INT64 (always-positive i64) so
            // a u32 >= 2^31 reads back correctly. Must match box_value / build_tagged_val_alloca.
            Type::UInt8 | Type::UInt16 | Type::UInt32 => 3,
            Type::Int64 => 3,
            // UInt64 — read back unsigned. TAG_UINT64 = 14 in lin-runtime/src/tagged.rs.
            Type::UInt64 => 14,
            Type::Float32 => 4,
            Type::Float64 => 5,
            Type::Str => 6,
            Type::Object(_) => 7,
            Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_) => 8,
            Type::Function { .. } => 9,
            _ => 0,
        }
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

    /// Return the i8 constant for the runtime tag of a type.
    pub(crate) fn type_tag_const(&self, ty: &Type) -> inkwell::values::IntValue<'ctx> {
        let i8_ty = self.context.i8_type();
        let tag: u64 = match ty {
            Type::Null => 0,
            Type::Bool => 1,
            Type::Int32 => 2,
            // UInt8/16/32 boxed as TAG_INT64; UInt64 as TAG_UINT64. Keep in sync with type_tag
            // / box_value so `is`-checks against a boxed value match its actual runtime tag.
            Type::UInt32 | Type::Int64 => 3,
            Type::UInt64 => 14,
            Type::Float32 | Type::Float64 => 4,
            Type::Str => 6,
            Type::Object(_) => 7,
            Type::Array(_) | Type::FixedArray(_) => 8,
            Type::Function { .. } => 9,
            _ => 0xFF,
        };
        i8_ty.const_int(tag, false)
    }

}