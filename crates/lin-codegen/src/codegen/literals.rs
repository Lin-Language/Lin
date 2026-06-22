use inkwell::values::BasicValueEnum;

use lin_check::types::Type;
use super::Codegen;

impl<'ctx> Codegen<'ctx> {
    pub(crate) fn compile_int_lit(&self, v: i64, ty: &Type) -> BasicValueEnum<'ctx> {
        match ty {
            Type::Int8 | Type::UInt8 => self.context.i8_type().const_int(v as u64, ty.is_signed()).into(),
            Type::Int16 | Type::UInt16 => self.context.i16_type().const_int(v as u64, ty.is_signed()).into(),
            Type::Int32 | Type::UInt32 => self.context.i32_type().const_int(v as u64, ty.is_signed()).into(),
            Type::Int64 | Type::UInt64 => self.context.i64_type().const_int(v as u64, ty.is_signed()).into(),
            _ => self.context.i32_type().const_int(v as u64, true).into(),
        }
    }

    pub(crate) fn compile_float_lit(&self, v: f64, ty: &Type) -> BasicValueEnum<'ctx> {
        match ty {
            Type::Float32 => self.context.f32_type().const_float(v).into(),
            Type::Float64 => self.context.f64_type().const_float(v).into(),
            _ => self.context.f64_type().const_float(v).into(),
        }
    }

    pub(crate) fn compile_string_lit(&self, s: &str) -> BasicValueEnum<'ctx> {
        // String literals are compile-time constants. Emit a full immortal `LinString` as a constant
        // global in rodata, matching the runtime's `#[repr(C)] LinString` layout:
        //   { i32 refcount=IMMORTAL_RC | i32 len | i64 hash | [len x i8] data }
        // refcount@0, len@4, hash@8, data@16.
        //
        // `hash` stores the precomputed FNV-1a of the string bytes (same as lin_map_get uses).
        // Baking it into the constant means lin_map_get/set never need to compute or cache it —
        // every rodata literal is already hash-ready at startup. Combined with the lazy-cache for
        // heap strings, this eliminates essentially all FNV-1a rehashing on RAPTOR hot paths.
        //
        // The refcount sentinel `IMMORTAL_RC` (0x8000_0000) makes retain/release a no-op.
        // Globals are deduped by content (`str_literal_globals`).
        if let Some(&ptr) = self.str_literal_globals.borrow().get(s) {
            return ptr.into();
        }

        const IMMORTAL_RC: u64 = 0x8000_0000;
        let bytes = s.as_bytes();
        let i32_type = self.context.i32_type();
        let i64_type = self.context.i64_type();
        let i8_type = self.context.i8_type();

        // Precompute FNV-1a hash at compile time (matches runtime `fnv1a_bytes_str`).
        let hash_val: u64 = {
            let mut h: u64 = 0xcbf29ce484222325;
            for &b in bytes {
                h ^= b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
            if h == 0 { 1 } else { h }
        };

        let refcount = i32_type.const_int(IMMORTAL_RC, false);
        let len = i32_type.const_int(bytes.len() as u64, false);
        let hash = i64_type.const_int(hash_val, false);
        let const_bytes: Vec<_> =
            bytes.iter().map(|&b| i8_type.const_int(b as u64, false)).collect();
        let data = i8_type.const_array(&const_bytes);

        // Struct layout: { i32, i32, i64, [N x i8] }
        // The i64 `hash` field forces 8-byte alignment; LLVM will add 0 padding between len and
        // hash because both i32 and i64 are already naturally aligned in this sequence.
        // Matches `repr(C)` LinString: refcount@0, len@4, hash@8, data@16.
        let str_const = self.context.const_struct(
            &[refcount.into(), len.into(), hash.into(), data.into()],
            false,
        );
        let global = self.module.add_global(str_const.get_type(), None, "str_lit");
        global.set_constant(true);
        global.set_initializer(&str_const);
        global.set_linkage(inkwell::module::Linkage::Internal);
        global.set_unnamed_addr(true);

        let ptr = global.as_pointer_value();
        self.str_literal_globals.borrow_mut().insert(s.to_string(), ptr);
        ptr.into()
    }

}
