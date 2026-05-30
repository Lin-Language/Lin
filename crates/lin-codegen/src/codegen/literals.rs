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
        // Create a global constant for the byte data, then call lin_string_from_bytes.
        let bytes = s.as_bytes();
        let byte_array_type = self.context.i8_type().array_type(bytes.len() as u32);
        let const_bytes: Vec<_> = bytes
            .iter()
            .map(|&b| self.context.i8_type().const_int(b as u64, false))
            .collect();
        let const_array = self.context.i8_type().const_array(&const_bytes);
        let global = self.module.add_global(byte_array_type, None, "str_data");
        global.set_constant(true);
        global.set_initializer(&const_array);

        let ptr = global.as_pointer_value();
        let len = self.context.i32_type().const_int(bytes.len() as u64, false);

        self.builder
            .build_call(self.rt.string_from_bytes, &[ptr.into(), len.into()], "str")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
    }

}
