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
            // A sealed scalar record uses the packed-struct layout, NOT a LinObject — route its
            // release to lin_sealed_release (decrement rc, free on zero; no per-field release).
            // Passing it to lin_object_release would walk garbage entries. Gate FIRST.
            Type::Object { .. } if Self::sealed_scalar_fields(ty).is_some() => {
                let fields = Self::sealed_scalar_fields(ty).unwrap().clone();
                self.emit_sealed_release(val, &fields);
            }
            Type::Object { .. } => { self.builder.call(self.rt.object_release, &[ptr.into()], ""); }
            // Typed index-signature map (`{ String: T }`, ADR-082): the hashed LinMap container.
            Type::Map(_) => { self.builder.call(self.rt.map_release, &[ptr.into()], ""); }
            Type::Function { .. } => { self.builder.call(self.rt.closure_release, &[ptr.into()], ""); }
            Type::TypeVar(_) | Type::Union(_) => { self.builder.call(self.rt.tagged_release, &[ptr.into()], ""); }
            // Stream<T> is a boxed TaggedVal*(TAG_STREAM); its release dispatches the tag-aware
            // `lin_tagged_release`, whose TAG_STREAM arm decrements the stream box's refcount and
            // closes the fd when it hits zero (Stage 2). Owning model (is_union_ty), so scope-exit
            // and global-reassign releases land here.
            Type::Stream(_) => { self.builder.call(self.rt.tagged_release, &[ptr.into()], ""); }
            _ => {} // scalars: nothing to release
        }
    }

}