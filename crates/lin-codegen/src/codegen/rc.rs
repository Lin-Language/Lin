use super::builder_ext::BuilderExt;
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};
use inkwell::IntPredicate;

use lin_check::types::Type;
use lin_ir::repr::Repr;
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
        repr: &Repr,
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
        // Release by PHYSICAL representation, not static type: an unboxed sum-node param slot holds a
        // raw `*SumNode` (Repr::Packed(SumNode)) even though its static type is a `Union` — releasing
        // it via the type-dispatched `lin_tagged_release` (the Union arm) would mis-interpret the raw
        // node as a boxed TaggedVal and corrupt the heap. `emit_release_repr` routes a Packed(SumNode)
        // to `lin_sumnode_release`, and defers to the type-based release for every non-Packed repr.
        self.emit_release_repr(old.into(), ty, repr);
        self.builder.unconditional_branch(cont_bb);
        self.builder.position_at_end(cont_bb);
    }

    /// Release a SINGLE TCO param slot's value on the loop-EXIT (return) path, guarded by:
    /// (1) `owns_flag` — true only after a tail back-edge has stored into this slot at least once;
    /// (2) `entry_ptrs` — EVERY original caller-passed param value (the borrowed entries). The slot
    /// must differ from ALL of them: the caller owns and frees the entry values, so releasing one
    /// here is a use-after-free at the caller. Comparing against ALL entries (not just this slot's
    /// own) is REQUIRED because a TCO loop may PERMUTE its borrowed array params between slots — the
    /// merge-sort ping-pong `_mergePass(buf, work, …) -> _mergePass(work, buf, …)` ends with `buf`'s
    /// slot holding the `work` entry (and returns one of them). A per-slot-only guard would free a
    /// borrowed buffer swapped in from another slot — a double-free. (A param threaded UNCHANGED,
    /// e.g. `arr` in `scan`, is also caught here.)
    /// (3) when `ret_ptr` is a pointer, an alias check — the slot value must differ from the
    /// returned value (a `scan` that returns its own `cur` param directly aliases the slot; the
    /// caller takes ownership of the returned value, so releasing the slot too would double-free).
    ///
    /// This is the loop-exit counterpart to [`emit_tco_release_old`]. The back-edge release frees
    /// INTERMEDIATE loop-produced values on each overwrite; this frees the FINAL slot value when the
    /// loop returns. They are disjoint — a loop-produced value is either overwritten on a back-edge
    /// OR is the final value at return, never both — so no double-free.
    pub(crate) fn emit_tco_release_final(
        &mut self,
        llvm_fn: FunctionValue<'ctx>,
        owns_flag: PointerValue<'ctx>,
        slot_val: PointerValue<'ctx>,
        entry_ptrs: &[PointerValue<'ctx>],
        ret_ptr: Option<PointerValue<'ctx>>,
        ty: &Type,
        repr: &Repr,
    ) {
        let i64_ty = self.context.i64_type();
        let bool_ty = self.context.bool_type();
        // owned = a prior tail iteration stored into this slot.
        let owned = self.builder.load(bool_ty, owns_flag, "tco_fowned").into_int_value();
        let mut cond = owned;
        let slot_int = self.builder.ptr_to_int(slot_val, i64_ty, "tco_fslot_i");
        // Don't release a value that is ANY borrowed entry param (the caller owns/frees them) — a
        // pass-through OR permuted-between-slots param ends the loop still holding a borrowed value.
        for ep in entry_ptrs {
            let ent_int = self.builder.ptr_to_int(*ep, i64_ty, "tco_fent_i");
            let ne = self.builder.int_compare(IntPredicate::NE, slot_int, ent_int, "tco_fentne");
            cond = self.builder.and(cond, ne, "tco_fentdiff");
        }
        if let Some(rp) = ret_ptr {
            // Don't release the slot if it IS the returned value — ownership transfers to caller.
            let ret_int = self.builder.ptr_to_int(rp, i64_ty, "tco_fret_i");
            let ne = self.builder.int_compare(IntPredicate::NE, slot_int, ret_int, "tco_fne");
            cond = self.builder.and(cond, ne, "tco_fdiff");
        }
        let rel_bb = self.context.append_basic_block(llvm_fn, "tco_frel");
        let cont_bb = self.context.append_basic_block(llvm_fn, "tco_frelcont");
        self.builder.conditional_branch(cond, rel_bb, cont_bb);
        self.builder.position_at_end(rel_bb);
        // Release by PHYSICAL representation, not static type (mirrors `emit_tco_release_old`): an
        // unboxed sum-node param slot holds a raw `*SumNode` (Repr::Packed(SumNode)) even though its
        // static type is a `Union` — the interp parser's `parseExprLoop`/`parseTermLoop` accumulators
        // carry `Ast` nodes through TCO. Routing through the static-type `emit_release` would free the
        // node as a boxed TaggedVal and corrupt the heap (the `lin_tagged_clone` UAF). `emit_release_repr`
        // routes a Packed(SumNode) to `lin_sumnode_release` and defers to the type-based release otherwise.
        self.emit_release_repr(slot_val.into(), ty, repr);
        self.builder.unconditional_branch(cont_bb);
        self.builder.position_at_end(cont_bb);
    }

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
            // COLUMNAR array (0xFC) → the standard array release (lin_array_release dispatches on
            // elem_tag = 0xFC and calls free_columnar_array_cols + lin_array_free).
            Repr::Packed(lin_ir::repr::Layout::ColumnarArray { .. }) => {
                self.builder.call(self.rt.array_release, &[val.into_pointer_value().into()], "");
                return;
            }
            // UNBOXED SUM TYPE (unboxed-sumtype Stage 1) → `lin_sumnode_release(ptr, total_size)`.
            // Stage 1 is scalar-only so this is a refcount decrement + free (no per-field walk).
            Repr::Packed(lin_ir::repr::Layout::SumNode { sum_ty }) => {
                let total = Self::sumnode_total_size(sum_ty);
                let i64_ty = self.context.i64_type();
                self.builder.call(
                    self.rt.sumnode_release,
                    &[val.into_pointer_value().into(), i64_ty.const_int(total, false).into()],
                    "",
                );
                return;
            }
            // Stage 3 NullableRecord: a nullable sealed struct pointer. Release the sealed struct only
            // when non-null (emit a conditional null-check + guarded emit_sealed_release).
            Repr::Packed(lin_ir::repr::Layout::NullableRecord { fields }) => {
                let fields = fields.clone();
                let p = val.into_pointer_value();
                let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                let pi = self.builder.ptr_to_int(p, self.context.i64_type(), "nr_p2i");
                let is_null = self.builder.int_compare(
                    inkwell::IntPredicate::EQ, pi, self.context.i64_type().const_zero(), "nr_isnull");
                let rel_bb = self.context.append_basic_block(llvm_fn, "nr_rel");
                let cont_bb = self.context.append_basic_block(llvm_fn, "nr_relcont");
                self.builder.conditional_branch(is_null, cont_bb, rel_bb);
                self.builder.position_at_end(rel_bb);
                self.emit_sealed_release(val, &fields);
                self.builder.unconditional_branch(cont_bb);
                self.builder.position_at_end(cont_bb);
                return;
            }
            // A boxed slot (Opaque): the box is a TaggedVal/LinMap whose release is the
            // tag-dispatched one. Fall through to the type-based dispatch which already picks the right
            // boxed releaser for the static type (object/array/tagged/map/closure/stream).
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
            // A sealed scalar record uses the packed-struct layout, NOT a LinMap — route its
            // release to lin_sealed_release (decrement rc, free on zero; no per-field release).
            // Passing it to lin_map_release would walk garbage entries. Gate FIRST.
            Type::Object { .. } if Self::sealed_scalar_fields(ty).is_some() => {
                let fields = Self::sealed_scalar_fields(ty).unwrap().clone();
                self.emit_sealed_release(val, &fields);
            }
            // Non-sealed open objects are now LinMap* (TAG_MAP) — use map_release.
            Type::Object { .. } => { self.builder.call(self.rt.map_release, &[ptr.into()], ""); }
            // Typed index-signature map (`{ K: V }`, ADR-055 + numeric-key): the hashed LinMap container.
            Type::Map { .. } => { self.builder.call(self.rt.map_release, &[ptr.into()], ""); }
            Type::Function { .. } => { self.builder.call(self.rt.closure_release, &[ptr.into()], ""); }
            // Stage 3 NullableRecord: a `T | Null` union where T is a sealed record is physically a
            // RAW nullable sealed-struct pointer (NOT a TaggedVal box) — its repr is TYPE-DETERMINED,
            // so this gate fires wherever such a value is released by static type (notably the
            // CellSet/FreeCell release-of-old-value of a `var last: Record | Null` slot). Release it
            // null-guarded as a sealed struct; routing it to `lin_tagged_release` reads offset 0 as a
            // TAG byte and dealloc's the 56-byte struct as a 16-byte TaggedVal box → mismatched-size
            // free / double-free (the captured-record-`var`-across-a-call closure crash, ADR-083).
            // Gate BEFORE the generic Union arm.
            Type::Union(_) if Self::nullable_sealed_record_type(ty).is_some() => {
                let fields = Self::nullable_sealed_record_type(ty).unwrap().clone();
                let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                let i64_ty = self.context.i64_type();
                let pi = self.builder.ptr_to_int(ptr, i64_ty, "nr_rel_p2i");
                let is_null = self.builder.int_compare(
                    inkwell::IntPredicate::EQ, pi, i64_ty.const_zero(), "nr_rel_isnull");
                let rel_bb = self.context.append_basic_block(llvm_fn, "nr_rel");
                let cont_bb = self.context.append_basic_block(llvm_fn, "nr_relcont");
                self.builder.conditional_branch(is_null, cont_bb, rel_bb);
                self.builder.position_at_end(rel_bb);
                self.emit_sealed_release(val, &fields);
                self.builder.unconditional_branch(cont_bb);
                self.builder.position_at_end(cont_bb);
            }
            Type::TypeVar(_) | Type::Union(_) => { self.builder.call(self.rt.tagged_release, &[ptr.into()], ""); }
            // Stream<T> is a boxed TaggedVal*(TAG_STREAM); its release dispatches the tag-aware
            // `lin_tagged_release`, whose TAG_STREAM arm decrements the stream box's refcount and
            // closes the fd when it hits zero (Stage 2). Owning model (is_union_ty), so scope-exit
            // and global-reassign releases land here.
            Type::Stream(_) => { self.builder.call(self.rt.tagged_release, &[ptr.into()], ""); }
            // Promise<T> is a boxed TaggedVal*(TAG_PROMISE); its release dispatches the tag-aware
            // `lin_tagged_release`, whose TAG_PROMISE arm joins/drops the promise box. Owning model.
            Type::Promise(_) => { self.builder.call(self.rt.tagged_release, &[ptr.into()], ""); }
            // Opaque handles (e.g. TarEntry) are all boxed TaggedVal*; their release dispatches
            // through `lin_tagged_release` whose tag-specific arm decrements the box RC. Owning model.
            Type::Opaque(_) => { self.builder.call(self.rt.tagged_release, &[ptr.into()], ""); }
            _ => {} // scalars: nothing to release
        }
    }

}