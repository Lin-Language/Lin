use super::builder_ext::BuilderExt;
use inkwell::values::BasicValueEnum;

use lin_check::types::Type;
use lin_ir::repr::Repr;
use super::Codegen;

impl<'ctx> Codegen<'ctx> {
    /// Emit a release dispatched on the value's PHYSICAL representation (`func.repr`). PART C
    /// (single-owner): when the pass proved a temp `Packed`, the release SHAPE is chosen from that
    /// fact, not re-derived from the static `Type`. A `Packed` temp is constructed packed, so today's
    /// type-dispatch already routes it to the packed releaser — making this override BYTE-IDENTICAL on
    /// the current corpus (no Packed-repr temp is ever a non-sealed type). For any non-Packed repr the
    /// dispatch defers verbatim to the existing static-type [`emit_release`], so the boxed/scalar paths
    /// are unchanged. (The fuller `Boxed`-with-sealed-type divergence fix — releasing a boxed value
    /// typed sealed with the boxed shape — is a BEHAVIOR change, deferred per the Part C byte-identical
    /// gate; it is unreachable on the current corpus because sealed-typed temps that reach Release are
    /// Packed.)
    pub(crate) fn emit_release_repr(&mut self, val: BasicValueEnum<'ctx>, ty: &Type, repr: &Repr) {
        if !val.is_pointer_value() { return; }
        match repr {
            // PACKED sealed record → packed struct release (decrement rc, free on zero, per-heap-field
            // release walked inside emit_sealed_release).
            Repr::Packed(lin_ir::repr::Layout::PackedStruct { fields }) => {
                let fields = fields.clone();
                self.emit_sealed_release(val, &fields);
                return;
            }
            // PACKED sealed array → the 0xFE-aware array release.
            Repr::Packed(lin_ir::repr::Layout::PackedSealedArray { .. }) => {
                self.builder.call(self.rt.array_release, &[val.into_pointer_value().into()], "");
                return;
            }
            // A boxed slot (Opaque OR WrapsPacked-by-pointer): the box is a TaggedVal/LinObject whose
            // release is the tag-dispatched one. WrapsPacked borrows its inner packed buffer; the box
            // shell's release (tagged_release) decrements the inner via the runtime's tag dispatch.
            // Fall through to the type-based dispatch which already picks the right boxed releaser for
            // the static type (object/array/tagged/map/closure/stream).
            Repr::Boxed(_) | Repr::FlatScalar(_) | Repr::Unknown | Repr::Bottom => {}
        }
        self.emit_release(val, ty);
    }

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
            // Typed index-signature map (`{ String: T }`, ADR-055): the hashed LinMap container.
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