use super::builder_ext::BuilderExt;
use inkwell::values::BasicValueEnum;

use lin_check::types::Type;
use super::Codegen;

impl<'ctx> Codegen<'ctx> {
    /// Emit a type-dispatched release call for a heap-allocated value.
    /// No-op for scalars (non-pointer LLVM values) and null pointers.
    pub(crate) fn emit_release(&mut self, val: BasicValueEnum<'ctx>, ty: &Type) {
        if !val.is_pointer_value() { return; }
        let ptr = val.into_pointer_value();
        match ty {
            Type::Str | Type::StrLit(_) => { self.builder.call(self.rt.string_release, &[ptr.into()], ""); }
            Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_) => { self.builder.call(self.rt.array_release, &[ptr.into()], ""); }
            Type::Object(_) => { self.builder.call(self.rt.object_release, &[ptr.into()], ""); }
            Type::Function { .. } => { self.builder.call(self.rt.closure_release, &[ptr.into()], ""); }
            Type::TypeVar(_) | Type::Union(_) => { self.builder.call(self.rt.tagged_release, &[ptr.into()], ""); }
            _ => {} // scalars: nothing to release
        }
    }

}