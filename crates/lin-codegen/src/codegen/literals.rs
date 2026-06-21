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
        // global in rodata — `{ i32 refcount=IMMORTAL_RC | i32 len | [len x i8] data }`, matching the
        // runtime's `#[repr(C)] LinString` layout — and use a POINTER to that global as the literal's
        // value. No runtime `lin_string_literal` call, no intern-cache hash, no per-occurrence work.
        //
        // The refcount sentinel `IMMORTAL_RC` (0x8000_0000) makes retain/release a no-op on this
        // box (see `lin-runtime` string.rs): `lin_string_release` returns early before decrementing,
        // and every increment path funnels through `lin_string_inc_ref`, which leaves an immortal
        // string unchanged. So a rodata-resident constant `LinString` is observationally identical to
        // the heap-interned one the old path produced — but free to materialise. RAPTOR evaluated
        // ~457M string literals/run (constant object keys in hot scan loops); each is now a constant
        // pointer the optimiser can hoist and CSE.
        //
        // Globals are deduped by content (`str_literal_globals`), so identical literals share one
        // global — preserving the pointer-identity the runtime intern cache used to give (equality is
        // by content anyway, so this only helps the optimiser).
        //
        // All callers of compile_string_lit pass genuine compile-time literals (string-literal
        // expressions, object/match keys, panic messages, the "[object]" fallback). Dynamic strings
        // (concat results, interpolation parts, fs reads, etc.) do NOT route through here.
        if let Some(&ptr) = self.str_literal_globals.borrow().get(s) {
            return ptr.into();
        }

        const IMMORTAL_RC: u64 = 0x8000_0000;
        let bytes = s.as_bytes();
        let i32_type = self.context.i32_type();
        let i8_type = self.context.i8_type();

        let refcount = i32_type.const_int(IMMORTAL_RC, false);
        let len = i32_type.const_int(bytes.len() as u64, false);
        let const_bytes: Vec<_> =
            bytes.iter().map(|&b| i8_type.const_int(b as u64, false)).collect();
        let data = i8_type.const_array(&const_bytes);

        // The struct constant `{ i32, i32, [N x i8] }`. An unpacked struct of two i32s followed by an
        // i8 array has no padding (the array's alignment is 1), so its layout matches `repr(C)`
        // LinString exactly: refcount@0, len@4, data@8.
        let str_const = self.context.const_struct(
            &[refcount.into(), len.into(), data.into()],
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
