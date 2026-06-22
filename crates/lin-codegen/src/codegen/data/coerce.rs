use super::super::builder_ext::BuilderExt;
use inkwell::values::BasicValueEnum;
use inkwell::IntPredicate;
use lin_check::types::Type;
use super::super::Codegen;

impl<'ctx> Codegen<'ctx> {
    /// Project a wider/Json/`Object[]` source array (`src`, statically `src_ty`) into a FRESH
    /// SEALED-RECORD ARRAY of `arr_ty`'s element type (sealed-records Stage 3 boundary, §3.2). Builds
    /// a new contiguous sealed array and, per element, reads the source element as a boxed value,
    /// projects it into the element record's struct layout, and copies the projected payload into the
    /// sealed slot. Non-mutating; `src` keeps its own ownership. Rare path; correctness over speed.
    /// True when `ty` is (or transitively contains) a representation that needs a sealed projection
    /// when crossing from the boxed `Json` view: a sealed scalar record, OR a sealed-record array, OR
    /// a container/union holding one. Used to decide whether a NESTED-array Coerce must recurse
    /// element-wise (vs. the one-level sealed-array arms or a plain pointer pass-through).
    pub(crate) fn ty_contains_sealed(ty: &Type) -> bool {
        if Self::sealed_array_elem(ty).is_some() || Self::sealed_scalar_fields(ty).is_some() {
            return true;
        }
        match ty {
            Type::Array(t) | Type::Iterator(t) | Type::Shared(t) => Self::ty_contains_sealed(t),
            Type::Map { value: t, .. } => Self::ty_contains_sealed(t),
            Type::FixedArray(ts) | Type::Union(ts) => ts.iter().any(Self::ty_contains_sealed),
            _ => false,
        }
    }

    /// Coerce a boxed/Json source ARRAY into `Array(inner_to)` element-wise, where `inner_to` itself
    /// contains a sealed representation (so a verbatim pointer reuse would mis-type the elements). The
    /// outer array is rebuilt as a TAGGED array (its elements are heap pointers — sealed arrays or
    /// boxed records — not packed scalars), and each source element is recursively `compile_ir_coerce`d
    /// from its boxed view (the Json wildcard) into `inner_to`, then pushed (materialized to its boxed/tagged
    /// slot by `tagged_array_push_value`). The source elements are read via `lin_array_get_tagged`
    /// (fresh +1 boxes), released after the coerce takes its own +1. Used for `partition`/`groupBy`/
    /// `chunk`-shaped combinator results routed through the type-erased boxed fallback.
    pub(crate) fn array_coerce_elementwise(
        &mut self,
        src: BasicValueEnum<'ctx>,
        src_ty: &Type,
        inner_to: &Type,
    ) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        // Unbox a boxed Json/union source to the raw LinArray*.
        let src_raw = if Self::is_union_type(src_ty) {
            self.builder.call(self.rt.unbox_ptr, &[src.into()], "nestarr_unbox").try_as_basic_value().unwrap_basic()
        } else { src };
        let len_fn = self.get_or_declare_fn("lin_array_length", i64_ty.fn_type(&[ptr_ty.into()], false));
        let len = self.builder.call(len_fn, &[src_raw.into()], "nestarr_len").try_as_basic_value().unwrap_basic().into_int_value();
        let out = self.builder.call(self.rt.array_alloc, &[len.into()], "nestarr_out").try_as_basic_value().unwrap_basic();
        let get_tagged = self.get_or_declare_fn("lin_array_get_tagged", ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false));
        let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let head = self.context.append_basic_block(llvm_fn, "nestarr_head");
        let body = self.context.append_basic_block(llvm_fn, "nestarr_body");
        let done = self.context.append_basic_block(llvm_fn, "nestarr_done");
        let idx_slot = self.entry_block_alloca(i64_ty, "nestarr_i");
        self.builder.store(idx_slot, i64_ty.const_zero());
        self.builder.unconditional_branch(head);
        self.builder.position_at_end(head);
        let i = self.builder.load(i64_ty, idx_slot, "nestarr_iv").into_int_value();
        let cond = self.builder.int_compare(IntPredicate::SLT, i, len, "nestarr_cond");
        self.builder.conditional_branch(cond, body, done);
        self.builder.position_at_end(body);
        let elem_box = self.builder.call(get_tagged, &[src_raw.into(), i.into()], "nestarr_get").try_as_basic_value().unwrap_basic();
        // `lin_array_get_tagged` ALWAYS returns a boxed `TaggedVal*` (the dynamic Json view of the
        // element), regardless of the source array's static element type. So coerce FROM the Json
        // wildcard — not `inner_from` (e.g. `Array(TypeVar)`, which the inner sealed-array projection
        // would NOT recognize as boxed and so would read the box as a raw `LinArray*` → crash). The
        // wildcard makes `sealed_array_project_owned` / `sealed_project_from` unbox the element first.
        let coerced = self.compile_ir_coerce(elem_box, &Type::TypeVar(u32::MAX), inner_to);
        // `lin_array_push` (via `tagged_array_push_value`) does NOT retain — it copies the 8-byte
        // payload and TAKES OWNERSHIP of the inner heap value. The `coerced` element is a fresh +1
        // (a projected sealed array, a materialized boxed record, or an unboxed scalar), so its +1
        // transfers into the output slot — do NOT release it here (that would double-free).
        self.tagged_array_push_value(out, coerced, inner_to);
        // The source element box from `lin_array_get_tagged` is a fresh +1 we own.
        // CASE 1: coerce produced a FRESH independent +1 (different LLVM value) — the push
        // consumed `coerced`'s +1, but `elem_box` still holds its own +1 to the source element;
        // release it (inner RC -1, box shell freed).
        // CASE 2: coerce was a passthrough (coerced == elem_box, e.g. Union/TypeVar target with
        // pointer value) — the push TRANSFERRED `elem_box`'s +1 to the output slot; releasing
        // would over-decrement the inner RC → UAF. Only free the box shell.
        if elem_box.is_pointer_value() {
            use inkwell::values::{AnyValue, AsValueRef};
            let coerce_is_passthrough = coerced.is_pointer_value()
                && coerced.as_any_value_enum().as_value_ref() == elem_box.as_any_value_enum().as_value_ref();
            if coerce_is_passthrough {
                let free_box = self.get_or_declare_fn("lin_tagged_free_box",
                    self.context.void_type().fn_type(&[self.context.ptr_type(inkwell::AddressSpace::default()).into()], false));
                self.builder.call(free_box, &[elem_box.into()], "");
            } else {
                self.builder.call(self.rt.tagged_release, &[elem_box.into()], "");
            }
        }
        let next = self.builder.int_add(i, i64_ty.const_int(1, false), "nestarr_next");
        self.builder.store(idx_slot, next);
        self.builder.unconditional_branch(head);
        self.builder.position_at_end(done);
        out
    }

    /// Widen/convert a FLAT scalar array (`UInt8[]`, `Int32[]`, `Float32[]`, …) to a flat scalar
    /// array of a DIFFERENT element type (`from_elem` → `to_elem`). A flat array stores its elements
    /// at the element type's native stride (1 byte for `UInt8`, 4 for `Int32`, …) and tags the buffer
    /// with that element kind (`elem_tag`). Binding a `UInt8[]` value to an `Int32[]` slot is therefore
    /// a genuine representation change: reinterpreting the same buffer would read 4 source bytes as one
    /// i32. Materialize a FRESH `to_elem`-strided buffer, reading each source element at the SOURCE
    /// stride and storing it widened/converted (sext/zext/sitofp/fpext/… via `compile_ir_coerce`'s
    /// numeric arm) at the DEST stride. The result is a +1-owned independent array; the source keeps
    /// its own ownership (released by its own scope).
    pub(crate) fn flat_array_widen(
        &mut self,
        src: BasicValueEnum<'ctx>,
        from_elem: &Type,
        to_elem: &Type,
    ) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let src_raw = src;
        let len_fn = self.get_or_declare_fn("lin_array_length", i64_ty.fn_type(&[ptr_ty.into()], false));
        let len = self.builder.call(len_fn, &[src_raw.into()], "fwiden_len")
            .try_as_basic_value().unwrap_basic().into_int_value();
        // Allocate a fresh dest-strided flat buffer with capacity = len (so the fast push path is
        // taken for every element; the cold grow path is never needed).
        let to_suffix = Self::flat_suffix(to_elem);
        let alloc_fn = self.get_or_declare_fn(
            &format!("lin_flat_array_alloc_{}", to_suffix),
            ptr_ty.fn_type(&[i64_ty.into()], false));
        let out = self.builder.call(alloc_fn, &[len.into()], "fwiden_out")
            .try_as_basic_value().unwrap_basic();
        let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let head = self.context.append_basic_block(llvm_fn, "fwiden_head");
        let body = self.context.append_basic_block(llvm_fn, "fwiden_body");
        let done = self.context.append_basic_block(llvm_fn, "fwiden_done");
        let idx_slot = self.entry_block_alloca(i64_ty, "fwiden_i");
        self.builder.store(idx_slot, i64_ty.const_zero());
        self.builder.unconditional_branch(head);
        self.builder.position_at_end(head);
        let i = self.builder.load(i64_ty, idx_slot, "fwiden_iv").into_int_value();
        let cond = self.builder.int_compare(IntPredicate::SLT, i, len, "fwiden_cond");
        self.builder.conditional_branch(cond, body, done);
        self.builder.position_at_end(body);
        // Read the element at the SOURCE element type (its native stride), convert to the DEST
        // scalar (numeric widen/convert), and push at the DEST stride.
        let elem = self.flat_array_get(src_raw, i, from_elem, false);
        let conv = self.compile_ir_coerce(elem, from_elem, to_elem);
        self.flat_array_push(out, conv, to_elem);
        let next = self.builder.int_add(i, i64_ty.const_int(1, false), "fwiden_next");
        self.builder.store(idx_slot, next);
        self.builder.unconditional_branch(head);
        self.builder.position_at_end(done);
        out
    }
}
