use super::builder_ext::BuilderExt;
use inkwell::types::BasicType;
use inkwell::values::BasicValueEnum;
use lin_common::tags::{TAG_INT32, TAG_INT64, TAG_OBJECT};
use inkwell::{AddressSpace, IntPredicate};

use lin_check::types::Type;
use super::Codegen;

impl<'ctx> Codegen<'ctx> {
    /// Push a scalar into a flat unboxed array.
    ///
    /// INLINED (like `flat_array_get`): `lin_flat_array_push_<sfx>` lives in the separately
    /// compiled `lin-runtime` staticlib, so LLVM cannot inline it across that boundary (no LTO).
    /// Every element of an eager combinator chain over a flat scalar array (e.g. the pipeline
    /// benchmark `range(0,20M).map(...).filter(...).reduce(...)`) paid a full call + prologue. The
    /// common case is the FAST path (`len < cap`): bump-append the element. The COLD grow path
    /// (`len == cap`) still defers to the runtime push so the realloc/layout logic is never
    /// duplicated and the behaviour stays byte-identical.
    ///
    /// LinArray layout (repr(C), in sync with `flat_array_get`/lin-runtime): refcount u32 @ byte 0,
    /// elem_tag u8 @ byte 4, len u64 @ byte 8, cap u64 @ byte 16, data ptr @ byte 24.
    pub(crate) fn flat_array_push(&mut self, arr: BasicValueEnum<'ctx>, val: BasicValueEnum<'ctx>, elem_ty: &Type) {
        let suffix = Self::flat_suffix(elem_ty);
        let push_name = format!("lin_flat_array_push_{}", suffix);
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let llvm_elem_ty = self.llvm_type(elem_ty);
        let push_fn = self.get_or_declare_fn(&push_name,
            self.context.void_type().fn_type(&[ptr_ty.into(), llvm_elem_ty.into()], false));
        let arr_ptr = arr.into_pointer_value();

        // len = *(u64*)(arr + 8); cap = *(u64*)(arr + 16)
        let len_ptr = unsafe {
            self.builder.gep(self.context.i8_type(), arr_ptr, &[i64_ty.const_int(8, false)], "fpush_len_p")
        };
        let len = self.builder.load(i64_ty, len_ptr, "fpush_len").into_int_value();
        let cap_ptr = unsafe {
            self.builder.gep(self.context.i8_type(), arr_ptr, &[i64_ty.const_int(16, false)], "fpush_cap_p")
        };
        let cap = self.builder.load(i64_ty, cap_ptr, "fpush_cap").into_int_value();
        let full = self.builder.int_compare(IntPredicate::EQ, len, cap, "fpush_full");

        let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let grow_b = self.context.append_basic_block(llvm_fn, "fpush_grow");
        let fast_b = self.context.append_basic_block(llvm_fn, "fpush_fast");
        let cont_b = self.context.append_basic_block(llvm_fn, "fpush_cont");
        self.builder.conditional_branch(full, grow_b, fast_b);

        // Cold grow path: the runtime push reallocates, stores, and bumps len — byte-identical.
        self.builder.position_at_end(grow_b);
        self.builder.call(push_fn, &[arr_ptr.into(), val.into()], "");
        self.builder.unconditional_branch(cont_b);

        // Fast path: data = *(ptr*)(arr + 24); data[len] = val; len = len + 1
        self.builder.position_at_end(fast_b);
        let data_ptr_ptr = unsafe {
            self.builder.gep(self.context.i8_type(), arr_ptr, &[i64_ty.const_int(24, false)], "fpush_data_pp")
        };
        let data_ptr = self.builder.load(ptr_ty, data_ptr_ptr, "fpush_data").into_pointer_value();
        let elem_ptr = unsafe {
            self.builder.in_bounds_gep(llvm_elem_ty, data_ptr, &[len], "fpush_elem_p")
        };
        self.builder.store(elem_ptr, val);
        let new_len = self.builder.int_add(len, i64_ty.const_int(1, false), "fpush_newlen");
        self.builder.store(len_ptr, new_len);
        self.builder.unconditional_branch(cont_b);

        self.builder.position_at_end(cont_b);
    }

    /// Load a scalar element from a flat unboxed array.
    ///
    /// This is INLINED rather than a `call lin_flat_array_get_<suffix>`: the runtime accessor
    /// lives in the separately-compiled `lin-runtime` staticlib, so LLVM can't inline it (no LTO
    /// across that boundary), and every read paid a full call + prologue + bounds-check. In a tight
    /// scalar loop (e.g. Dijkstra's linear-scan min over `pqDist[j]`, ~21M reads) that call
    /// dominated. Emitting the load inline lets LLVM keep the array pointer in a register, hoist
    /// the length, and fold the bounds check — matching what Rust/Go compile `a[i]` to.
    ///
    /// Semantics mirror `lin_flat_array_get_<sfx>` exactly (lin-runtime/src/array.rs): Python-style
    /// negative indexing (`idx < 0 → len + idx`) and an OOB runtime fault (spec §6.1). The cold OOB
    /// path defers to the runtime accessor so the fault message/behaviour stays identical and there
    /// is no new runtime symbol. LinArray layout (repr(C)): len @ byte 8 (u64), data ptr @ byte 24.
    pub(crate) fn flat_array_get(&mut self, arr: BasicValueEnum<'ctx>, idx: inkwell::values::IntValue<'ctx>, elem_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let llvm_elem_ty = self.llvm_type(elem_ty);
        let arr_ptr = arr.into_pointer_value();
        // Index arrives as i64 (sign-extended at the call site).
        let idx = if idx.get_type().get_bit_width() == 64 {
            idx
        } else {
            self.builder.int_s_extend(idx, i64_ty, "flat_idx64")
        };

        // len = *(u64*)(arr + 8)
        let len_ptr = unsafe {
            self.builder.gep(self.context.i8_type(), arr_ptr, &[i64_ty.const_int(8, false)], "flat_len_p")
        };
        let len = self.builder.load(i64_ty, len_ptr, "flat_len").into_int_value();

        // actual = idx < 0 ? len + idx : idx   (matches the runtime's negative-index handling)
        let zero = i64_ty.const_zero();
        let is_neg = self.builder.int_compare(IntPredicate::SLT, idx, zero, "flat_idx_neg");
        let wrapped = self.builder.int_add(len, idx, "flat_idx_wrap");
        let actual = self.builder
            .build_select(is_neg, wrapped, idx, "flat_idx_actual")
            .unwrap()
            .into_int_value();

        // Bounds check folded to a single UNSIGNED compare: `(u64)actual >= (u64)len` catches
        // BOTH `actual < 0` (a still-negative wrap reads as a huge unsigned value ≥ len) and
        // `actual >= len`. Equivalent to the runtime's two signed compares, one instruction.
        let oob = self.builder.int_compare(IntPredicate::UGE, actual, len, "flat_oob");

        let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let ok_b = self.context.append_basic_block(llvm_fn, "flat_get_ok");
        let oob_b = self.context.append_basic_block(llvm_fn, "flat_get_oob");
        self.builder.conditional_branch(oob, oob_b, ok_b);

        // Cold OOB path: call the runtime accessor with the ORIGINAL index so its fault message
        // ("array index {idx} out of bounds") is byte-identical. It does not return.
        self.builder.position_at_end(oob_b);
        let suffix = Self::flat_suffix(elem_ty);
        let get_fn = self.get_or_declare_fn(&format!("lin_flat_array_get_{}", suffix),
            llvm_elem_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false));
        self.builder.call(get_fn, &[arr_ptr.into(), idx.into()], "flat_get_oob");
        self.builder.unreachable();

        // Fast path: data = *(ptr*)(arr + 24); return data[actual]
        self.builder.position_at_end(ok_b);
        let data_ptr_ptr = unsafe {
            self.builder.gep(self.context.i8_type(), arr_ptr, &[i64_ty.const_int(24, false)], "flat_data_pp")
        };
        let data_ptr = self.builder.load(ptr_ty, data_ptr_ptr, "flat_data").into_pointer_value();
        let elem_ptr = unsafe {
            self.builder.in_bounds_gep(llvm_elem_ty, data_ptr, &[actual], "flat_elem_p")
        };
        self.builder.load(llvm_elem_ty, elem_ptr, "flat_get")
    }

    /// Push a dynamically-typed value (TypeVar or Union) into a tagged LinArray*.
    /// Ensures the value is a TaggedVal* before calling lin_array_push_tagged,
    /// boxing scalars (e.g. i32 from a TypeVar that resolved concretely) as needed.
    pub(crate) fn push_tagged_val(&mut self, arr: BasicValueEnum<'ctx>, val: BasicValueEnum<'ctx>, val_ty: &Type) {
        let val_ptr = if val.is_pointer_value() {
            val.into_pointer_value()
        } else {
            self.box_value(val, val_ty).into_pointer_value()
        };
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let rt_push_tagged = self.get_or_declare_fn("lin_array_push_tagged",
            self.context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
        self.builder.call(rt_push_tagged, &[arr.into(), val_ptr.into()], "");
    }

    /// Push a value into a tagged LinArray* always using tagged format (never flat).
    /// Use this when the array was allocated with rt_array_alloc (tagged format).
    pub(crate) fn tagged_array_push_value(&mut self, arr: BasicValueEnum<'ctx>, val: BasicValueEnum<'ctx>, val_ty: &Type) {
        let i8_ty = self.context.i8_type();
        match val_ty {
            Type::TypeVar(_) | Type::Union(_) => self.push_tagged_val(arr, val, val_ty),
            _ => {
                let tag_val = Self::type_tag(val_ty);
                let tag = i8_ty.const_int(tag_val as u64, false);
                // lin_array_push copies a full 8 bytes from the cell into the payload, so the
                // cell must hold 8 defined bytes. Pointers are stored as the raw 8-byte pointer;
                // every scalar (int/bool/float) is encoded into its 8-byte TaggedVal payload via
                // tagged_payload_i64 — the SAME encoder box_value uses — and stored as an i64
                // cell. This is critical for Float32: it is fpext'd to f64 and stored as f64 bits
                // under TAG_FLOAT64, so the runtime (which reads a TAG_FLOAT64 payload as f64)
                // round-trips it. (Storing a native 4-byte f32 cell under TAG_FLOAT64 would have
                // the runtime read 8 bytes including 4 undefined bytes → garbage.)
                let cell = match val_ty {
                    Type::Str | Type::StrLit(_) | Type::Array(_) | Type::Object { .. } | Type::Iterator(_) | Type::Function { .. } => {
                        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                        let cell = self.builder.alloca(ptr_ty, "arr_cell");
                        self.builder.store(cell, val);
                        cell
                    }
                    _ => {
                        let i64_ty = self.context.i64_type();
                        let payload = self.tagged_payload_i64(&val, val_ty);
                        let cell = self.builder.alloca(i64_ty, "arr_cell");
                        self.builder.store(cell, payload);
                        cell
                    }
                };
                self.builder.call(self.rt.array_push, &[arr.into(), cell.into(), tag.into()], "arr_push");
            }
        }
    }

    /// Coerce an IR value to a raw heap pointer (LinObject*/LinArray*/LinString*): if the
    /// static type is a union (boxed TaggedVal*) OR the value isn't already a pointer, unbox
    /// it; otherwise pass through. Used by the dynamic object/array helper intrinsics.
    pub(crate) fn ir_as_raw_ptr(&mut self, v: BasicValueEnum<'ctx>, ty: &Type) -> BasicValueEnum<'ctx> {
        if Self::is_union_type(ty) || !v.is_pointer_value() {
            self.builder.call(self.rt.unbox_ptr, &[v.into()], "ir_raw_ptr").try_as_basic_value().unwrap_basic()
        } else {
            v
        }
    }

    /// Normalise an array-length argument to i64: unbox a boxed Int32 if needed, then
    /// sign-extend. Used by the array-allocate helpers.
    pub(crate) fn ir_n_to_i64(&mut self, n: Option<BasicValueEnum<'ctx>>, n_ty: Option<&Type>) -> inkwell::values::IntValue<'ctx> {
        let i64_ty = self.context.i64_type();
        let Some(n) = n else { return i64_ty.const_zero() };
        if n.is_pointer_value() {
            let n_i32 = self.builder.call(self.rt.unbox_int32, &[n.into()], "ir_n_unbox").try_as_basic_value().unwrap_basic().into_int_value();
            return self.builder.int_s_extend(n_i32, i64_ty, "ir_n64");
        }
        if n.is_int_value() {
            let _ = n_ty;
            self.builder.int_s_extend_or_bit_cast(n.into_int_value(), i64_ty, "ir_n64")
        } else {
            i64_ty.const_zero()
        }
    }

    pub(crate) fn compile_ir_index(&mut self, obj: BasicValueEnum<'ctx>, key: BasicValueEnum<'ctx>, obj_ty: &Type, key_ty: &Type, result_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        if !obj.is_pointer_value() {
            return ptr_ty.const_null().into();
        }
        // When the object is statically Json/union, `obj` is a TaggedVal* wrapping the
        // real Array/Object pointer — unbox it to the raw container pointer before
        // calling the runtime accessors (which expect LinArray*/LinObject*).
        let container = if Self::is_union_type(obj_ty) {
            self.builder.call(self.rt.unbox_ptr, &[obj.into()], "ir_idx_unbox").try_as_basic_value().unwrap_basic()
        } else {
            obj
        };
        // When the object is Json/union AND the key is a runtime-boxed value whose kind isn't
        // statically known (e.g. `arr[j]` where j is a closure param typed Json — it could be
        // an int array-index or a string object-key at runtime), dispatch on the KEY's tag:
        // int → array get, otherwise → object get. The static `is_array_access` test below
        // would misclassify this as object access and a runtime array would return null.
        // Mirrors the AST compile_index pointer-key runtime dispatch.
        if Self::is_union_type(obj_ty)
            && Self::is_union_type(key_ty)
            && key.is_pointer_value()
        {
            let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
            let k_tag = self.builder.call(self.rt.get_tag, &[key.into()], "ir_idxk_tag").try_as_basic_value().unwrap_basic().into_int_value();
            let i8t = self.context.i8_type();
            let is_i32 = self.builder.int_compare(IntPredicate::EQ, k_tag, i8t.const_int(TAG_INT32 as u64, false), "ir_k_i32");
            let is_i64 = self.builder.int_compare(IntPredicate::EQ, k_tag, i8t.const_int(TAG_INT64 as u64, false), "ir_k_i64");
            let is_int = self.builder.or(is_i32, is_i64, "ir_k_int");
            let int_b = self.context.append_basic_block(llvm_fn, "ir_idx_intk");
            let str_b = self.context.append_basic_block(llvm_fn, "ir_idx_strk");
            let mrg = self.context.append_basic_block(llvm_fn, "ir_idx_kmrg");
            self.builder.conditional_branch(is_int, int_b, str_b);
            // int key → array get (always returns a valid TaggedVal*).
            self.builder.position_at_end(int_b);
            let idx = self.unbox_value(key, &Type::Int64).into_int_value();
            let get_tagged_fn = self.get_or_declare_fn("lin_array_get_tagged",
                ptr_ty.fn_type(&[ptr_ty.into(), self.context.i64_type().into()], false));
            let arr_res = self.builder.call(get_tagged_fn, &[container.into(), idx.into()], "ir_idx_aget").try_as_basic_value().unwrap_basic();
            let int_exit = self.builder.get_insert_block().unwrap();
            self.builder.unconditional_branch(mrg);
            // string key → object get, guarded by an object-tag check on the container source.
            self.builder.position_at_end(str_b);
            let key_raw = self.builder.call(self.rt.unbox_ptr, &[key.into()], "ir_idxk_str").try_as_basic_value().unwrap_basic();
            let obj_tag = self.builder.call(self.rt.get_tag, &[obj.into()], "ir_idx_otag").try_as_basic_value().unwrap_basic().into_int_value();
            let is_obj = self.builder.int_compare(IntPredicate::EQ, obj_tag, i8t.const_int(TAG_OBJECT as u64, false), "ir_idx_isobj");
            let oget_b = self.context.append_basic_block(llvm_fn, "ir_idx_oget");
            let onull_b = self.context.append_basic_block(llvm_fn, "ir_idx_onull");
            let omrg = self.context.append_basic_block(llvm_fn, "ir_idx_omrg");
            self.builder.conditional_branch(is_obj, oget_b, onull_b);
            self.builder.position_at_end(oget_b);
            let oget = self.builder.call(self.rt.object_get, &[container.into(), key_raw.into()], "ir_idx_osget").try_as_basic_value().unwrap_basic();
            let oget_exit = self.builder.get_insert_block().unwrap();
            self.builder.unconditional_branch(omrg);
            self.builder.position_at_end(onull_b);
            self.builder.unconditional_branch(omrg);
            self.builder.position_at_end(omrg);
            let ophi = self.builder.phi(ptr_ty, "ir_idx_ophi");
            ophi.add_incoming(&[(&oget, oget_exit), (&ptr_ty.const_null(), onull_b)]);
            let str_res = ophi.as_basic_value();
            let str_exit = self.builder.get_insert_block().unwrap();
            self.builder.unconditional_branch(mrg);
            self.builder.position_at_end(mrg);
            let phi = self.builder.phi(ptr_ty, "ir_idx_kphi");
            phi.add_incoming(&[(&arr_res, int_exit), (&str_res, str_exit)]);
            let res = phi.as_basic_value();
            return if Self::is_union_type(result_ty) { res } else { self.unbox_tagged_val_to_type(res, result_ty) };
        }
        // Array indexing when the object is an array type or the key is numeric (any int
        // width — e.g. an Int32 literal index like `lines[0]`, not just i64).
        let is_array_access = matches!(obj_ty, Type::Array(_) | Type::FixedArray(_))
            || key_ty.is_numeric()
            || (key.is_int_value() && key.get_type() != self.context.bool_type().into());
        if is_array_access {
            // Key may arrive as a raw int or a boxed TaggedVal* — unbox to i64.
            let idx = if key.is_int_value() {
                self.builder.int_s_extend_or_bit_cast(key.into_int_value(), self.context.i64_type(), "ir_idx")
            } else if key.is_pointer_value() {
                let unboxed = self.unbox_value(key, &Type::Int64);
                unboxed.into_int_value()
            } else {
                return ptr_ty.const_null().into();
            };
            // Flat scalar element: read the unboxed scalar directly (mirrors AST `flat_array_get`).
            // A fixed-length array (`[T1, T2, ...]`) is always stored TAGGED — its positional
            // element types are heterogeneous — so even when the result type is a scalar the
            // slot is a TaggedVal*, not raw bytes. Skip the flat shortcut and take the tagged
            // read + unbox path below; reading it as flat would return garbage.
            if Self::is_flat_scalar(result_ty) && !matches!(obj_ty, Type::FixedArray(_)) {
                return self.flat_array_get(container, idx, result_ty);
            }
            // For TypeVar/Union result, use lin_array_get_tagged so the result is always
            // a valid TaggedVal* regardless of whether the array is flat or tagged.
            if Self::is_union_type(result_ty) {
                let get_tagged_fn = self.get_or_declare_fn("lin_array_get_tagged",
                    ptr_ty.fn_type(&[ptr_ty.into(), self.context.i64_type().into()], false));
                return self.builder.call(get_tagged_fn, &[container.into(), idx.into()], "ir_aget_tv").try_as_basic_value().unwrap_basic();
            }
            let tagged = self.builder.call(self.rt.array_get, &[container.into(), idx.into()], "ir_aget").try_as_basic_value().unwrap_basic();
            return self.unbox_tagged_val_to_type(tagged, result_ty);
        }
        // Object key access. lin_object_get expects a raw *LinString key; unbox a boxed key.
        let key_str = if key_ty.is_string_ish() {
            key
        } else if Self::is_union_type(key_ty) && key.is_pointer_value() {
            self.builder.call(self.rt.unbox_ptr, &[key.into()], "ir_key_unbox").try_as_basic_value().unwrap_basic()
        } else {
            key
        };
        // When the object is statically Json/union, its runtime value may NOT be an object
        // (e.g. `results["type"]` where results is actually an array). Guard the lookup with
        // a tag check — TAG_OBJECT(7) → look up the key; otherwise return Null. Without this,
        // lin_object_get would read a LinArray*/scalar as a LinObject* and crash. Mirrors the
        // AST compile_index string-key-on-Json path.
        if Self::is_union_type(obj_ty) {
            let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
            let obj_tag = self.builder.call(self.rt.get_tag, &[obj.into()], "ir_idx_tag").try_as_basic_value().unwrap_basic().into_int_value();
            let is_obj = self.builder.int_compare(
                IntPredicate::EQ, obj_tag, self.context.i8_type().const_int(TAG_OBJECT as u64, false), "ir_idx_is_obj");
            let ok = self.context.append_basic_block(llvm_fn, "ir_idx_obj_ok");
            let no = self.context.append_basic_block(llvm_fn, "ir_idx_obj_no");
            let mrg = self.context.append_basic_block(llvm_fn, "ir_idx_obj_mrg");
            self.builder.conditional_branch(is_obj, ok, no);
            self.builder.position_at_end(ok);
            let entry = self.builder.call(self.rt.object_get, &[container.into(), key_str.into()], "ir_oget").try_as_basic_value().unwrap_basic();
            let ok_exit = self.builder.get_insert_block().unwrap();
            self.builder.unconditional_branch(mrg);
            self.builder.position_at_end(no);
            let null_res = ptr_ty.const_null();
            self.builder.unconditional_branch(mrg);
            self.builder.position_at_end(mrg);
            let phi = self.builder.phi(ptr_ty, "ir_idx_obj_phi");
            phi.add_incoming(&[(&entry, ok_exit), (&null_res, no)]);
            let result_ptr = phi.as_basic_value();
            return self.unbox_tagged_val_to_type(result_ptr, result_ty);
        }
        let tagged = self.builder.call(self.rt.object_get, &[container.into(), key_str.into()], "ir_oget").try_as_basic_value().unwrap_basic();
        self.unbox_tagged_val_to_type(tagged, result_ty)
    }

    /// Store `value` into an object: `lin_object_set(obj_ptr, key_ptr, box(value))`.
    /// `obj_ptr`/`key_ptr` must already be RAW (unboxed) `LinObject*`/`LinString*`.
    ///
    /// A concrete value is heap-boxed; a union value (already a `TaggedVal*` under the
    /// uniform ABI) is passed straight through. `lin_object_set` copies the 16-byte
    /// TaggedVal and RETAINS its inner payload, so for a fresh box we release it afterwards
    /// (undoing the box's own +0, freeing the shell) — net codegen effect on the inner is
    /// zero; the slot's single reference is supplied by the IR `transfer_into_container`
    /// emitted in `IndexSet`/`ObjectSetDyn` lowering. Shared by `compile_ir_index_set` and
    /// `Intrinsic::ObjectSetDyn` so the two paths can never drift (the historical RC-bug
    /// source).
    pub(crate) fn emit_object_set(&mut self, obj_ptr: BasicValueEnum<'ctx>, key_ptr: BasicValueEnum<'ctx>, value: BasicValueEnum<'ctx>, val_ty: &Type) {
        let val_is_fresh_box = !Self::is_union_type(val_ty);
        let val_tagged = if val_is_fresh_box {
            self.box_value(value, val_ty)
        } else { value };
        self.builder.call(self.rt.object_set,
            &[obj_ptr.into(), key_ptr.into(), val_tagged.into()], "");
        if val_is_fresh_box && val_tagged.is_pointer_value() {
            self.builder.call(self.rt.tagged_release, &[val_tagged.into()], "");
        }
    }

    /// Store `value` into an array slot: `lin_array_set(arr_ptr, idx_i64, tagged(value))`.
    /// `arr_ptr` must already be a RAW (unboxed) `LinArray*`.
    ///
    /// `lin_array_set` raw-copies the 16-byte TaggedVal INLINE into the slot WITHOUT
    /// retaining the inner (it CONSUMES the source). So:
    ///   - a CONCRETE value is marshalled through a STACK `TaggedVal` (no heap allocation) —
    ///     the 16 bytes are copied inline and the stack memory is reclaimed automatically;
    ///     heap-boxing here would orphan the box shell (the `FreeBoxShell` reclaim only
    ///     covers union values), leaking 16 bytes per store.
    ///   - a UNION value is already a heap box: pass it straight through to be consumed; a
    ///     fresh source box's orphaned shell is freed by the `FreeBoxShell` the IR emits.
    /// The slot's owning reference is supplied by the IR `transfer_into_container` emitted in
    /// `IndexSet`/`ArraySetDyn` lowering. Shared by `compile_ir_index_set` and `ArraySetDyn`.
    /// Store a scalar into a flat unboxed array slot, INLINED.
    ///
    /// The symmetric write twin of `flat_array_get`: the generic `emit_array_set` always boxes the
    /// value into a stack `TaggedVal` and calls the cross-staticlib `lin_array_set`, which then
    /// re-decodes the tag and stores the raw scalar. For a flat `Int32[]`/`Float64[]` whose value
    /// type already matches the element type, that round-trip is pure overhead. This emits the
    /// bounds-checked raw store directly. The COLD OOB path defers to `lin_array_set` (a silent
    /// no-op on OOB, spec §6.1 — array set never faults) so the behaviour stays byte-identical and
    /// no new runtime symbol is needed.
    ///
    /// Only valid when the value's static type equals the flat element type (no widening/narrowing
    /// conversion needed); the caller guarantees this. LinArray layout: len u64 @ byte 8, data ptr
    /// @ byte 24 (in sync with `flat_array_get`/`flat_array_push`).
    pub(crate) fn flat_array_set(&mut self, arr_ptr: BasicValueEnum<'ctx>, idx_i64: inkwell::values::IntValue<'ctx>, value: BasicValueEnum<'ctx>, elem_ty: &Type) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let llvm_elem_ty = self.llvm_type(elem_ty);
        let arr_ptr = arr_ptr.into_pointer_value();
        let idx = if idx_i64.get_type().get_bit_width() == 64 {
            idx_i64
        } else {
            self.builder.int_s_extend(idx_i64, i64_ty, "fset_idx64")
        };

        // len = *(u64*)(arr + 8)
        let len_ptr = unsafe {
            self.builder.gep(self.context.i8_type(), arr_ptr, &[i64_ty.const_int(8, false)], "fset_len_p")
        };
        let len = self.builder.load(i64_ty, len_ptr, "fset_len").into_int_value();

        // actual = idx < 0 ? len + idx : idx  (matches lin_array_set negative-index handling)
        let zero = i64_ty.const_zero();
        let is_neg = self.builder.int_compare(IntPredicate::SLT, idx, zero, "fset_idx_neg");
        let wrapped = self.builder.int_add(len, idx, "fset_idx_wrap");
        let actual = self.builder
            .build_select(is_neg, wrapped, idx, "fset_idx_actual")
            .unwrap()
            .into_int_value();
        // Single unsigned compare catches both a still-negative wrap and actual >= len.
        let oob = self.builder.int_compare(IntPredicate::UGE, actual, len, "fset_oob");

        let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let ok_b = self.context.append_basic_block(llvm_fn, "fset_ok");
        let oob_b = self.context.append_basic_block(llvm_fn, "fset_oob");
        let cont_b = self.context.append_basic_block(llvm_fn, "fset_cont");
        self.builder.conditional_branch(oob, oob_b, ok_b);

        // Cold OOB path: defer to the runtime set with the ORIGINAL index (silent no-op on OOB),
        // so out-of-range writes behave byte-identically to the generic path.
        self.builder.position_at_end(oob_b);
        let stack_tagged = self.build_tagged_val_alloca(&value, elem_ty);
        let set_fn = self.get_or_declare_fn("lin_array_set",
            self.context.void_type().fn_type(&[ptr_ty.into(), i64_ty.into(), ptr_ty.into()], false));
        self.builder.call(set_fn, &[arr_ptr.into(), idx.into(), stack_tagged.into()], "");
        self.builder.unconditional_branch(cont_b);

        // Fast path: data = *(ptr*)(arr + 24); data[actual] = value
        self.builder.position_at_end(ok_b);
        let data_ptr_ptr = unsafe {
            self.builder.gep(self.context.i8_type(), arr_ptr, &[i64_ty.const_int(24, false)], "fset_data_pp")
        };
        let data_ptr = self.builder.load(ptr_ty, data_ptr_ptr, "fset_data").into_pointer_value();
        let elem_ptr = unsafe {
            self.builder.in_bounds_gep(llvm_elem_ty, data_ptr, &[actual], "fset_elem_p")
        };
        self.builder.store(elem_ptr, value);
        self.builder.unconditional_branch(cont_b);

        self.builder.position_at_end(cont_b);
    }

    pub(crate) fn emit_array_set(&mut self, arr_ptr: BasicValueEnum<'ctx>, idx_i64: inkwell::values::IntValue<'ctx>, value: BasicValueEnum<'ctx>, val_ty: &Type) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let void_ty = self.context.void_type();
        let elem_tagged: BasicValueEnum<'ctx> = if Self::is_union_type(val_ty) {
            value
        } else {
            self.build_tagged_val_alloca(&value, val_ty).into()
        };
        let set_fn = self.get_or_declare_fn("lin_array_set",
            void_ty.fn_type(&[ptr_ty.into(), i64_ty.into(), ptr_ty.into()], false));
        self.builder.call(set_fn, &[arr_ptr.into(), idx_i64.into(), elem_tagged.into()], "");
    }

    /// `object[key] = value` for the IR path. Mirrors the AST `compile_index_set`:
    /// dispatch on the object's static type; for Json/union objects, dispatch at
    /// runtime on the key's tag (int key ⇒ array set, string key ⇒ object set),
    /// unboxing the boxed container first. Stores go through the shared `emit_object_set`/
    /// `emit_array_set` helpers so the boxing/retain/release sequence is IDENTICAL to the
    /// `lin_object_set`/`lin_array_set` intrinsics; the matching IR-level ownership transfer
    /// is emitted in `IndexSet` lowering (`lin-ir`).
    pub(crate) fn compile_ir_index_set(&mut self, obj: BasicValueEnum<'ctx>, key: BasicValueEnum<'ctx>, value: BasicValueEnum<'ctx>, obj_ty: &Type, key_ty: &Type, val_ty: &Type) {
        // Resolve an object key to a raw `LinString*`. A string key that is a callback param
        // arrives boxed (a `TaggedVal*`); unbox it, or `lin_object_set` reads the box as a
        // LinString and corrupts the key.
        let resolve_obj_key = |this: &mut Self, k: BasicValueEnum<'ctx>| -> BasicValueEnum<'ctx> {
            if Self::is_union_type(key_ty) && k.is_pointer_value() {
                this.builder.call(this.rt.unbox_ptr, &[k.into()], "iset_key_unbox").try_as_basic_value().unwrap_basic()
            } else {
                k
            }
        };
        match obj_ty {
            Type::Object { .. } | Type::Named(_) => {
                if obj.is_pointer_value() && key.is_pointer_value() {
                    let key_str = resolve_obj_key(self, key);
                    self.emit_object_set(obj, key_str, value, val_ty);
                }
            }
            Type::Array(elem) => {
                let idx = self.index_value_to_i64(key);
                // Flat scalar element AND the value already has the matching scalar type ⇒ inline a
                // bounds-checked raw store (no box + cross-staticlib `lin_array_set` round-trip).
                // A differing scalar width/kind would need the runtime's value conversion, so fall
                // back. A FixedArray is always stored tagged (heterogeneous slots) — handled below.
                if Self::is_flat_scalar(elem) && elem.as_ref() == val_ty {
                    self.flat_array_set(obj, idx, value, val_ty);
                } else {
                    self.emit_array_set(obj, idx, value, val_ty);
                }
            }
            Type::FixedArray(_) => {
                let idx = self.index_value_to_i64(key);
                self.emit_array_set(obj, idx, value, val_ty);
            }
            Type::TypeVar(_) | Type::Union(_) => {
                if !obj.is_pointer_value() { return; }
                // Unbox the boxed container, then dispatch on the key's runtime kind. A boxed
                // string key (TaggedVal*) and a boxed int key are both pointers, so dispatch
                // on the unboxed key's tag rather than the LLVM kind when the key is union.
                let container = self.builder.call(self.rt.unbox_ptr, &[obj.into()], "iset_unbox").try_as_basic_value().unwrap_basic();
                if Self::is_union_type(key_ty) && key.is_pointer_value() {
                    // Runtime-typed key: tag-dispatch int (array) vs string (object). The op is
                    // not statically known, so the IR uses a uniform RETAIN contract for a union
                    // value (`op_consumes_union = false`): object-set retains naturally, and the
                    // array branch below adds a `lin_tagged_retain` to match — so both branches
                    // leave the source box owned by its current owner. (A concrete value is
                    // boxed/retained identically by both helpers.)
                    let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                    let i8t = self.context.i8_type();
                    let k_tag = self.builder.call(self.rt.get_tag, &[key.into()], "iset_ktag").try_as_basic_value().unwrap_basic().into_int_value();
                    let is_i32 = self.builder.int_compare(IntPredicate::EQ, k_tag, i8t.const_int(TAG_INT32 as u64, false), "iset_k_i32");
                    let is_i64 = self.builder.int_compare(IntPredicate::EQ, k_tag, i8t.const_int(TAG_INT64 as u64, false), "iset_k_i64");
                    let is_int = self.builder.or(is_i32, is_i64, "iset_k_int");
                    let int_b = self.context.append_basic_block(llvm_fn, "iset_intk");
                    let str_b = self.context.append_basic_block(llvm_fn, "iset_strk");
                    let mrg = self.context.append_basic_block(llvm_fn, "iset_kmrg");
                    self.builder.conditional_branch(is_int, int_b, str_b);
                    self.builder.position_at_end(int_b);
                    // Array (consume) branch: for a union value, retain the inner first so the
                    // slot owns its own reference — matching object-set's retain semantics, so
                    // the IR's uniform `op_consumes_union = false` is correct for either branch.
                    if Self::is_union_type(val_ty) && value.is_pointer_value() {
                        let retain_fn = self.get_or_declare_fn("lin_tagged_retain",
                            self.context.void_type().fn_type(&[self.context.ptr_type(AddressSpace::default()).into()], false));
                        self.builder.call(retain_fn, &[value.into()], "");
                    }
                    let idx = self.index_value_to_i64(key);
                    self.emit_array_set(container, idx, value, val_ty);
                    self.builder.unconditional_branch(mrg);
                    self.builder.position_at_end(str_b);
                    let key_str = self.builder.call(self.rt.unbox_ptr, &[key.into()], "iset_key_unbox").try_as_basic_value().unwrap_basic();
                    self.emit_object_set(container, key_str, value, val_ty);
                    self.builder.unconditional_branch(mrg);
                    self.builder.position_at_end(mrg);
                } else if key.is_pointer_value() {
                    // Statically a string (object) key.
                    self.emit_object_set(container, key, value, val_ty);
                } else if key.is_int_value() {
                    let idx = self.index_value_to_i64(key);
                    self.emit_array_set(container, idx, value, val_ty);
                }
            }
            _ => {}
        }
    }

    /// Normalise an index value (raw int or boxed TaggedVal*) to an i64.
    pub(crate) fn index_value_to_i64(&mut self, key: BasicValueEnum<'ctx>) -> inkwell::values::IntValue<'ctx> {
        if key.is_int_value() {
            self.builder.int_s_extend_or_bit_cast(key.into_int_value(), self.context.i64_type(), "ir_idx64")
        } else if key.is_pointer_value() {
            let i32_key = self.builder.call(self.rt.unbox_int32, &[key.into()], "ir_skey_i32").try_as_basic_value().unwrap_basic().into_int_value();
            self.builder.int_s_extend(i32_key, self.context.i64_type(), "ir_skey_i64")
        } else {
            self.context.i64_type().const_zero()
        }
    }

    // ───────────────────────── Sealed records (Stages 1–2) ─────────────────────────
    //
    // A sealed record (gate: `Codegen::sealed_fields`) is a packed heap struct
    // `[ u32 rc | u32 size | u64 desc_ptr | fields… ]` allocated by `lin_sealed_alloc`, with fields
    // at the natural-aligned byte offsets `Codegen::sealed_field_layout` computes (declaration
    // order). Scalar fields are stored inline; HEAP fields (String/Array/nested-sealed, Stage 2)
    // are stored as an 8-byte owned (+1) pointer slot. The LLVM value is an opaque `ptr` (so it
    // flows through the existing object-as-ptr ABI). The descriptor at offset 8 lists the heap
    // fields so every drop site can release them without the static type (see lin_runtime::sealed).

    /// Emit (and cache) the static field DESCRIPTOR global for a sealed record and return a pointer
    /// to it (NULL pointer constant when the record has no heap fields — a scalar-only Stage-1
    /// record). The descriptor is `{ u32 count, { u32 offset, u32 kind } * count }`, listing ONLY
    /// the heap fields (scalars need no per-field RC). The runtime release/transfer walk it. Cached
    /// by the field layout so identical sealed types share one descriptor.
    pub(crate) fn sealed_descriptor(&mut self, fields: &indexmap::IndexMap<String, Type>) -> inkwell::values::PointerValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i32_ty = self.context.i32_type();
        // Collect (offset, kind) for each heap field, in layout order.
        let mut heap: Vec<(u64, u32)> = Vec::new();
        for (k, fty) in fields.iter() {
            if let Some(kind) = Self::sealed_field_kind(fty) {
                let (offset, _) = Self::sealed_field_layout(fields, k);
                heap.push((offset, kind));
            }
        }
        if heap.is_empty() {
            return ptr_ty.const_null();
        }
        // Cache key: the (offset,kind) sequence.
        let key: String = format!(
            "__sealeddesc_{}",
            heap.iter().map(|(o, kd)| format!("{}_{}", o, kd)).collect::<Vec<_>>().join("__")
        );
        if let Some(g) = self.module.get_global(&key) {
            return g.as_pointer_value();
        }
        let count_const = i32_ty.const_int(heap.len() as u64, false);
        // Each entry is two i32s laid out contiguously; model the whole entry block as an [N*2 x i32]
        // so the in-memory layout is exactly { u32 offset, u32 kind } per entry (8 bytes).
        let mut words: Vec<inkwell::values::IntValue<'ctx>> = Vec::with_capacity(heap.len() * 2);
        for (off, kind) in &heap {
            words.push(i32_ty.const_int(*off, false));
            words.push(i32_ty.const_int(*kind as u64, false));
        }
        let entries_arr = i32_ty.const_array(&words);
        let desc_ty = self.context.struct_type(&[i32_ty.into(), entries_arr.get_type().into()], false);
        let desc_val = self.context.const_struct(&[count_const.into(), entries_arr.into()], false);
        let global = self.module.add_global(desc_ty, None, &key);
        global.set_initializer(&desc_val);
        global.set_constant(true);
        // Plain named constant, no unnamed_addr — same reasoning as emit_capture_descriptor (avoids
        // the R_X86_64_32S link error from the mergeable .rodata.cstN section under PIE).
        global.as_pointer_value()
    }

    /// Load `field` from a sealed record at its constant byte offset — THE win: a single typed load,
    /// no `lin_object_get` call / hash lookup / unbox. `obj` is the struct ptr. For a HEAP field the
    /// loaded value is the BORROWED heap pointer (the struct owns it); the IR `FieldGet`/`Index`
    /// lowering emits the owning `Retain` separately (same contract as a boxed-object field read).
    pub(crate) fn sealed_field_get(
        &mut self,
        obj: BasicValueEnum<'ctx>,
        field: &str,
        fields: &indexmap::IndexMap<String, Type>,
        result_ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        let (offset, _total) = Self::sealed_field_layout(fields, field);
        let i64_ty = self.context.i64_type();
        let base = obj.into_pointer_value();
        let fld_ty = fields.get(field).cloned().unwrap_or(Type::Null);
        let llvm_fld = self.llvm_type(&fld_ty);
        let p = unsafe {
            self.builder.gep(self.context.i8_type(), base, &[i64_ty.const_int(offset, false)], "sealed_fld_p")
        };
        let loaded = self.builder.load(llvm_fld, p, "sealed_fld");
        // The declared result_ty may be a wider numeric than the stored field (e.g. field Int32
        // read into an Int64 slot); reconcile via the standard coerce.
        if &fld_ty == result_ty { loaded } else { self.compile_ir_coerce(loaded, &fld_ty, result_ty) }
    }

    /// Allocate a fresh sealed record (carrying its field descriptor) and store each field by
    /// offset. `field_vals` are (name, value, value_ty, already_owned) in any order. Returns the
    /// struct ptr (+1 owned). Scalar fields need no RC. Each HEAP field's payload must end up owned
    /// by the struct (+1):
    ///   - `already_owned == true`: the value is a FRESH +1 the caller transfers into the struct
    ///     (e.g. a projection/materialization the caller produced). Store verbatim, NO retain.
    ///   - `already_owned == false`: the value is a BORROWED reference (owned by the caller's temp,
    ///     released at the caller's scope exit). The struct RETAINS it to own its own +1 (mirroring
    ///     the boxed-object inline construction's per-field `lin_rc_retain`).
    /// This explicit flag replaces a fragile representation-difference guess — the caller knows the
    /// ownership of each value it hands in, and getting it wrong is the exact UAF/leak bug class.
    pub(crate) fn sealed_construct(
        &mut self,
        fields: &indexmap::IndexMap<String, Type>,
        field_vals: &[(String, BasicValueEnum<'ctx>, Type, bool)],
    ) -> BasicValueEnum<'ctx> {
        let total = Self::sealed_struct_size(fields);
        let i64_ty = self.context.i64_type();
        let desc = self.sealed_descriptor(fields);
        let obj = self.builder.call(self.rt.sealed_alloc, &[i64_ty.const_int(total, false).into(), desc.into()], "sealed_obj")
            .try_as_basic_value().unwrap_basic().into_pointer_value();
        for (name, val, val_ty, already_owned) in field_vals {
            let (offset, _) = Self::sealed_field_layout(fields, name);
            let fld_ty = fields.get(name).cloned().unwrap_or(Type::Null);
            // Convert the supplied value to the field's stored representation if needed. NOTE:
            // `Type`'s PartialEq IGNORES the `sealed` flag, so an unsealed `{x,y}` literal value
            // compares EQUAL to a sealed `Pt` field type — but their runtime representations DIFFER
            // (boxed LinObject vs packed struct). Use the representation-aware `sealed_repr_differs`
            // (not `!=`) to decide whether to coerce, so a nested sealed field's value is PROJECTED
            // into the struct layout rather than stored as a raw boxed object (which `release` would
            // then mis-walk as a sealed struct — the crash this fixes). A coerce that changes
            // representation produces a FRESH +1, so the stored value is owned regardless of the
            // caller's flag.
            let repr_change = Self::sealed_repr_differs(val_ty, &fld_ty);
            let stored = if repr_change { self.compile_ir_coerce(*val, val_ty, &fld_ty) } else { *val };
            let p = unsafe {
                self.builder.gep(self.context.i8_type(), obj, &[i64_ty.const_int(offset, false)], "sealed_set_p")
            };
            self.builder.store(p, stored);
            let owned = *already_owned || repr_change;
            if Self::sealed_field_kind(&fld_ty).is_some() && !owned && stored.is_pointer_value() {
                self.builder.call(self.rt.rc_retain, &[stored.into_pointer_value().into()], "sealed_fld_retain");
            }
        }
        obj.into()
    }

    /// True when storing a value of type `from` into a sealed field of type `to` requires a
    /// representation-changing coerce (project/materialize → a FRESH +1), as opposed to a verbatim
    /// pointer store (the value stays BORROWED). REPRESENTATION-AWARE: unlike `Type`'s PartialEq it
    /// distinguishes a SEALED object from a structurally-equal unsealed/boxed one.
    ///   - `to` is a SEALED record: a verbatim store is sound ONLY if `from` is the SAME sealed
    ///     record (same fields AND sealed). An unsealed `{x,y}` or `Json` source MUST be projected.
    ///   - String↔String, Array↔Array: same runtime pointer representation → no change.
    ///   - otherwise: a change iff the types differ.
    fn sealed_repr_differs(from: &Type, to: &Type) -> bool {
        if let Some(to_fields) = Self::sealed_fields(to) {
            // Verbatim only when `from` is the identical sealed record (same fields, sealed:true).
            return match Self::sealed_fields(from) {
                Some(from_fields) => from_fields != to_fields,
                None => true, // unsealed/boxed/Json source → project
            };
        }
        if from.is_string_ish() && to.is_string_ish() { return false; }
        if matches!(from, Type::Array(_) | Type::FixedArray(_)) && matches!(to, Type::Array(_) | Type::FixedArray(_)) {
            return false;
        }
        from != to
    }

    /// Materialize a sealed record into a fresh boxed `LinObject` (TAG_OBJECT semantics): the
    /// universal Json representation. Used at the sealed→Json/unsealed boundary so all the existing
    /// dynamic object machinery (toString/keys/print/dynamic-index/eq-vs-Json) operates unchanged on
    /// a normal LinObject. Returns the raw `LinObject*` (+1 owned). Each field is loaded by offset,
    /// boxed, and `lin_object_set_fresh`'d under its interned string key.
    ///
    /// RC contract per field: `lin_object_set_fresh` RETAINS the value's inner payload (the object
    /// takes a +1). For a SCALAR field there is no inner heap, so the fresh box's shell would leak —
    /// `lin_tagged_release` reclaims it (no-op on the absent inner). For a HEAP field the loaded
    /// pointer is BORROWED (the struct still owns its original +1); after `object_set_fresh` retains
    /// the inner (object +1), only the box SHELL is freed (`lin_tagged_free_box`) — NOT
    /// `lin_tagged_release`, which would also drop the inner and leave the object holding a pointer
    /// it never accounted for (a use-after-free once the struct releases). The struct keeps its
    /// reference; the materialized object owns an independent +1. Both balanced.
    pub(crate) fn sealed_materialize_to_object(
        &mut self,
        obj: BasicValueEnum<'ctx>,
        fields: &indexmap::IndexMap<String, Type>,
    ) -> BasicValueEnum<'ctx> {
        let i32_ty = self.context.i32_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let new_obj = self.builder.call(self.rt.object_alloc, &[i32_ty.const_int(fields.len() as u64, false).into()], "sealed_mat")
            .try_as_basic_value().unwrap_basic().into_pointer_value();
        let keys: Vec<String> = fields.keys().cloned().collect();
        let free_box_shell = self.get_or_declare_fn("lin_tagged_free_box", self.context.void_type().fn_type(&[ptr_ty.into()], false));
        for k in &keys {
            let fld_ty = fields.get(k).cloned().unwrap_or(Type::Null);
            let is_heap = Self::sealed_field_kind(&fld_ty).is_some();
            let v = self.sealed_field_get(obj, k, fields, &fld_ty);
            // box_value(heap) wraps the BORROWED pointer (no retain); box_value(scalar) wraps the
            // scalar (cached/heap box). For a nested sealed field, box_value materializes it to its
            // own boxed LinObject (a fresh +1), which object_set_fresh then retains — handled below.
            let boxed = self.box_value(v, &fld_ty);
            let key_str = self.compile_string_lit(k).into_pointer_value();
            self.builder.call(self.rt.object_set_fresh, &[new_obj.into(), key_str.into(), boxed.into()], "");
            if boxed.is_pointer_value() {
                if is_heap {
                    // A nested SEALED field's box_value produced a FRESH boxed LinObject (+1 inner)
                    // that object_set_fresh retained (+2 on the materialized inner); full
                    // tagged_release drops it back to +1 owned by the object, and frees the shell.
                    // A String/Array field's box wraps a BORROWED inner that object_set_fresh
                    // retained — free only the shell so the borrowed inner is not dropped.
                    if matches!(fld_ty, Type::Object { .. }) {
                        self.builder.call(self.rt.tagged_release, &[boxed.into()], "");
                    } else {
                        self.builder.call(free_box_shell, &[boxed.into()], "");
                    }
                } else {
                    // Scalar: no inner heap — full release reclaims the (cache-safe) box shell.
                    self.builder.call(self.rt.tagged_release, &[boxed.into()], "");
                }
            }
        }
        new_obj.into()
    }

    /// Project a source value (`src`, statically `src_ty`) into a FRESH sealed scalar record of
    /// `target_fields`. THE central boundary op. Non-mutating: `src` is untouched (its own owner
    /// releases it), extras are ignored, and the result is an independent +1 struct. The source
    /// is read by whatever representation it has:
    ///   - another sealed scalar record → field copy by offset;
    ///   - a boxed `LinObject` / Json TaggedVal → `lin_object_get` per target field, unbox, store.
    pub(crate) fn sealed_project_from(
        &mut self,
        src: BasicValueEnum<'ctx>,
        src_ty: &Type,
        target_fields: &indexmap::IndexMap<String, Type>,
    ) -> BasicValueEnum<'ctx> {
        // Source already a sealed record of (possibly) a different shape: copy fields by offset.
        // Every field value `sealed_field_get` returns is BORROWED from the source struct (the
        // source is non-mutating and keeps its own ownership) — including a nested-sealed pointer —
        // so the fresh struct must RETAIN each heap field (`already_owned = false`).
        if let Some(src_fields) = Self::sealed_scalar_fields(src_ty) {
            let src_fields = src_fields.clone();
            let vals: Vec<(String, BasicValueEnum<'ctx>, Type, bool)> = target_fields.keys().map(|k| {
                let fty = target_fields.get(k).cloned().unwrap_or(Type::Null);
                let v = self.sealed_field_get(src, k, &src_fields, &fty);
                (k.clone(), v, fty, false)
            }).collect();
            return self.sealed_construct(target_fields, &vals);
        }
        // Source is a boxed object / Json. Unbox to the raw LinObject* if it is a union/Json box.
        let container = if Self::is_union_type(src_ty) {
            self.builder.call(self.rt.unbox_ptr, &[src.into()], "sealed_proj_unbox").try_as_basic_value().unwrap_basic()
        } else {
            src
        };
        let target_keys: Vec<String> = target_fields.keys().cloned().collect();
        let mut vals: Vec<(String, BasicValueEnum<'ctx>, Type, bool)> = Vec::with_capacity(target_keys.len());
        for k in &target_keys {
            let fty = target_fields.get(k).cloned().unwrap_or(Type::Null);
            let key_str = self.compile_string_lit(k).into_pointer_value();
            // lin_object_get returns an INTERIOR pointer to the entry's TaggedVal (borrowed); unbox
            // it to the field value. For a scalar nothing is owned. For a String/Array the unbox
            // yields the BORROWED inner heap pointer (owned by the source object entry) → the struct
            // must retain it (`already_owned = false`). For a NESTED SEALED field the unbox RECURSES
            // into `sealed_project_from`, producing a FRESH +1 sealed struct → transfer ownership
            // (`already_owned = true`, no extra retain).
            let tagged = self.builder.call(self.rt.object_get, &[container.into(), key_str.into()], "sealed_proj_get").try_as_basic_value().unwrap_basic();
            let v = self.unbox_tagged_val_to_type(tagged, &fty);
            let owned = matches!(fty, Type::Object { .. }) && Self::sealed_fields(&fty).is_some();
            vals.push((k.clone(), v, fty, owned));
        }
        self.sealed_construct(target_fields, &vals)
    }

    /// Field-wise equality of two sealed scalar records of the SAME type (`fields`). Loads each
    /// field by offset and compares with the scalar equality for that field type, AND-ing. Returns
    /// an i1.
    pub(crate) fn sealed_eq(
        &mut self,
        a: BasicValueEnum<'ctx>,
        b: BasicValueEnum<'ctx>,
        fields: &indexmap::IndexMap<String, Type>,
    ) -> inkwell::values::IntValue<'ctx> {
        let bool_ty = self.context.bool_type();
        let mut acc = bool_ty.const_int(1, false);
        for (k, fty) in fields.iter() {
            let av = self.sealed_field_get(a, k, fields, fty);
            let bv = self.sealed_field_get(b, k, fields, fty);
            let eq = if fty.is_float() {
                self.builder.float_compare(inkwell::FloatPredicate::OEQ, av.into_float_value(), bv.into_float_value(), "sealed_feq")
            } else {
                self.builder.int_compare(IntPredicate::EQ, av.into_int_value(), bv.into_int_value(), "sealed_ieq")
            };
            acc = self.builder.and(acc, eq, "sealed_eq_acc");
        }
        acc
    }

    /// Release a sealed scalar record: `lin_sealed_release(ptr, size)`. No per-field release.
    pub(crate) fn emit_sealed_release(&mut self, val: BasicValueEnum<'ctx>, fields: &indexmap::IndexMap<String, Type>) {
        if !val.is_pointer_value() { return; }
        let total = Self::sealed_struct_size(fields);
        let i64_ty = self.context.i64_type();
        self.builder.call(self.rt.sealed_release, &[val.into(), i64_ty.const_int(total, false).into()], "");
    }

    pub(crate) fn compile_ir_field_get(&mut self, obj: BasicValueEnum<'ctx>, field: &str, obj_ty: &Type, result_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        // Sealed scalar record: constant-offset load (the win).
        if let Some(fields) = Self::sealed_scalar_fields(obj_ty) {
            if obj.is_pointer_value() {
                return self.sealed_field_get(obj, field, fields, result_ty);
            }
            return ptr_ty.const_null().into();
        }
        if obj.is_pointer_value() {
            // A Json/union object arrives as a boxed TaggedVal*; unbox to the raw LinObject*.
            let container = if Self::is_union_type(obj_ty) {
                self.builder.call(self.rt.unbox_ptr, &[obj.into()], "ir_fget_unbox").try_as_basic_value().unwrap_basic()
            } else {
                obj
            };
            let key_str = self.compile_string_lit(field).into_pointer_value();
            let tagged = self.builder.call(self.rt.object_get, &[container.into(), key_str.into()], "ir_fget").try_as_basic_value().unwrap_basic();
            // No string_release: `compile_string_lit` returns an interned, immortal
            // LinString (refcount == IMMORTAL_RC), so the release is a runtime no-op
            // — but still an emitted call, hit on every typed field read. Drop it.
            self.unbox_tagged_val_to_type(tagged, result_ty)
        } else { ptr_ty.const_null().into() }
    }

}