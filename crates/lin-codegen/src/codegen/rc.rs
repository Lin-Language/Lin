use super::builder_ext::BuilderExt;
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};
use inkwell::IntPredicate;

use lin_check::types::Type;
use super::Codegen;

impl<'ctx> Codegen<'ctx> {
    /// Release a TCO param slot's OLD value before the back-edge store overwrites it, guarded by:
    /// (1) `owns_flag` — a bool slot that is true only when the current slot value was produced by
    /// a PRIOR tail iteration (loop-owned), false for the borrowed caller-passed entry param; and
    /// (2) an alias check — `old` must differ from EVERY pointer in `new_ptrs` (the new argument
    /// values being stored this iteration). (1) prevents releasing a borrowed param the caller
    /// still owns (a use-after-free at the caller); (2) prevents a double-free when a param is
    /// threaded UNCHANGED (`old == new` for its own slot) or when some OTHER new arg still
    /// references this slot's old value. `ty` selects the per-type runtime release via
    /// `emit_release`.
    pub(crate) fn emit_tco_release_old(
        &mut self,
        llvm_fn: FunctionValue<'ctx>,
        owns_flag: PointerValue<'ctx>,
        old: PointerValue<'ctx>,
        new_ptrs: &[PointerValue<'ctx>],
        ty: &Type,
    ) {
        let i64_ty = self.context.i64_type();
        let bool_ty = self.context.bool_type();
        // owned = the current slot value was stored by a prior tail iteration (loop-owned).
        let owned = self.builder.load(bool_ty, owns_flag, "tco_owned").into_int_value();
        let old_int = self.builder.ptr_to_int(old, i64_ty, "tco_old_i");
        // cond = owned AND (old != new_j for every j)
        let mut differs = owned;
        for np in new_ptrs {
            let np_int = self.builder.ptr_to_int(*np, i64_ty, "tco_new_i");
            let ne = self.builder.int_compare(IntPredicate::NE, old_int, np_int, "tco_ne");
            differs = self.builder.and(differs, ne, "tco_diff");
        }
        let rel_bb = self.context.append_basic_block(llvm_fn, "tco_rel");
        let cont_bb = self.context.append_basic_block(llvm_fn, "tco_relcont");
        self.builder.conditional_branch(differs, rel_bb, cont_bb);
        self.builder.position_at_end(rel_bb);
        self.emit_release(old.into(), ty);
        self.builder.unconditional_branch(cont_bb);
        self.builder.position_at_end(cont_bb);
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