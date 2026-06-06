use super::builder_ext::BuilderExt;
use inkwell::types::BasicType;
use inkwell::values::BasicValueEnum;
use lin_common::tags::{TAG_INT32, TAG_INT64, TAG_OBJECT, TAG_MAP};
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
        // A SEALED-repr record element (`{tag:Int32, bytes:Int32[]}` etc.) flowing into a TAGGED
        // array is a packed struct pointer, NOT a boxed LinObject. Storing it raw under TAG_OBJECT
        // makes the runtime read the struct as a LinObject header → a misaligned-pointer deref of a
        // scalar field (`0x5`) on read-back. Materialize it to a fresh boxed LinObject first, then
        // store that pointer under TAG_OBJECT — the representation the tagged slot (and toString /
        // index-get) expects. This is the generic `push$Object` / `set` into a `Field[]` case.
        if let Type::Object { .. } = val_ty {
            if let Some(fields) = Self::sealed_fields(val_ty).cloned() {
                let obj = self.sealed_materialize_to_object(val, &fields);
                let tag = i8_ty.const_int(Self::type_tag(val_ty) as u64, false);
                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let cell = self.builder.alloca(ptr_ty, "arr_cell");
                self.builder.store(cell, obj);
                self.builder.call(self.rt.array_push, &[arr.into(), cell.into(), tag.into()], "arr_push");
                return;
            }
        }
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
        // Typed index-signature map `{ String: T }` (ADR-055): `m[k]` is an O(1) hashed lookup.
        // The key is a String (raw LinString*, or unbox a Json/union-boxed key); the result is
        // `T | Null` — `lin_map_get` returns null for a missing key, which `unbox_tagged_val_to_type`
        // maps to the language Null.
        if let Type::Map(_) = obj_ty {
            let key_str = if key_ty.is_string_ish() {
                key
            } else if Self::is_union_type(key_ty) && key.is_pointer_value() {
                self.builder.call(self.rt.unbox_ptr, &[key.into()], "ir_mkey_unbox").try_as_basic_value().unwrap_basic()
            } else {
                key
            };
            let tagged = self.builder.call(self.rt.map_get, &[container.into(), key_str.into()], "ir_mget").try_as_basic_value().unwrap_basic();
            return if Self::is_union_type(result_ty) {
                tagged
            } else {
                self.unbox_tagged_val_to_type(tagged, result_ty)
            };
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
            // Sealed-record array element (Stage 3): `arr[i]` yields a whole sealed-record value.
            // Materialize a FRESH standalone sealed struct (header + payload copied) so the result
            // is an ownable +1 value the standard retain/release/field-read machinery handles. The
            // hot `arr[i].field` access never reaches here (it is fused upstream to a direct scalar
            // load); this path covers `val p = arr[i]` / passing an element as a whole value.
            if Self::sealed_array_elem(obj_ty).is_some() {
                if let Some(fields) = Self::sealed_fields(result_ty) {
                    let fields = fields.clone();
                    return self.sealed_array_materialize_elem(container, idx, &fields);
                }
            }
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
            // A `{ String: T } | Null` index (e.g. the inner read of a NESTED typed map
            // `outer[a][b]`, where `outer[a]` is `{ String: T } | Null` and is NOT spellable as
            // an `is`-pattern to narrow, ADR-055 §5.1.1) runs through this union path. Its runtime
            // value is a TAG_MAP, so dispatch on the tag: TAG_MAP → `lin_map_get` (O(1) hashed),
            // TAG_OBJECT → `lin_object_get` (the Json association-list path), otherwise Null. Both
            // getters return a borrowed `*const TaggedVal`, so the ownership contract is identical.
            let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
            let obj_tag = self.builder.call(self.rt.get_tag, &[obj.into()], "ir_idx_tag").try_as_basic_value().unwrap_basic().into_int_value();
            let i8t = self.context.i8_type();
            let is_map = self.builder.int_compare(
                IntPredicate::EQ, obj_tag, i8t.const_int(TAG_MAP as u64, false), "ir_idx_is_map");
            let is_obj = self.builder.int_compare(
                IntPredicate::EQ, obj_tag, i8t.const_int(TAG_OBJECT as u64, false), "ir_idx_is_obj");
            let map_b = self.context.append_basic_block(llvm_fn, "ir_idx_map");
            let chk_obj = self.context.append_basic_block(llvm_fn, "ir_idx_chk_obj");
            let ok = self.context.append_basic_block(llvm_fn, "ir_idx_obj_ok");
            let no = self.context.append_basic_block(llvm_fn, "ir_idx_obj_no");
            let mrg = self.context.append_basic_block(llvm_fn, "ir_idx_obj_mrg");
            self.builder.conditional_branch(is_map, map_b, chk_obj);
            self.builder.position_at_end(map_b);
            let map_entry = self.builder.call(self.rt.map_get, &[container.into(), key_str.into()], "ir_mget_u").try_as_basic_value().unwrap_basic();
            let map_exit = self.builder.get_insert_block().unwrap();
            self.builder.unconditional_branch(mrg);
            self.builder.position_at_end(chk_obj);
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
            phi.add_incoming(&[(&map_entry, map_exit), (&entry, ok_exit), (&null_res, no)]);
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

    /// Store `value` into a typed index-signature map (`lin_map_set`, ADR-055).
    ///
    /// `elem_ty` is the map's value type `T` (from `Type::Map(T)`); `val_ty` is the source
    /// expression's static type, which may be a NARROWER numeric (e.g. an `Int32` variable stored
    /// into a `{ String: Int64 }` map). The value is normalised to `T`'s representation before
    /// storage so the slot reads back `T`-correct regardless of the source width (ADR-055).
    ///
    /// FLAT-SCALAR UNBOXING (ADR-055 follow-up): when `T` is a flat scalar (`is_flat_scalar` —
    /// Int8/16/32/64, UInt8/16/32/64, Float32/64), the scalar is marshalled through a STACK
    /// `TaggedVal` (tag+payload = `T`'s boxed-scalar convention, identical to what an array slot
    /// stores) rather than `box_value`'s HEAP box. `lin_map_set` copies the 16 bytes INLINE into the
    /// slot and `retain_tagged_payload` is a no-op for a scalar tag, so the value lives unboxed in
    /// the slot with NO per-value heap allocation, NO RC, and NO box-shell to free — the analogue of
    /// the flat scalar array store. (The stack TaggedVal is reclaimed automatically; `lin_map_set`
    /// never takes ownership of the passed pointer, it copies from it.)
    ///
    /// Otherwise (a heap value `T`, or a union/Json value): identical ownership contract to
    /// `emit_object_set` — a concrete heap value is freshly heap-boxed and the box shell released
    /// after the set (net zero on the inner; the slot's reference comes from the IR
    /// `transfer_into_container`), a union value (already a `TaggedVal*`) passes straight through.
    pub(crate) fn emit_map_set(&mut self, map_ptr: BasicValueEnum<'ctx>, key_ptr: BasicValueEnum<'ctx>, value: BasicValueEnum<'ctx>, val_ty: &Type, elem_ty: &Type) {
        // Flat-scalar value: store unboxed via a stack TaggedVal carrying T's tag/payload. Coerce
        // the (possibly narrower) source value to T's numeric representation first so the stored
        // payload is T-correct (e.g. a signed Int32 -1 sign-extends to Int64 -1, not 4294967295).
        if Self::is_flat_scalar(elem_ty) {
            let coerced = if val_ty == elem_ty {
                value
            } else {
                self.compile_ir_coerce(value, val_ty, elem_ty)
            };
            let stack_tagged = self.build_tagged_val_alloca(&coerced, elem_ty);
            self.builder.call(self.rt.map_set,
                &[map_ptr.into(), key_ptr.into(), stack_tagged.into()], "");
            return;
        }
        let val_is_fresh_box = !Self::is_union_type(val_ty);
        let val_tagged = if val_is_fresh_box {
            self.box_value(value, val_ty)
        } else { value };
        self.builder.call(self.rt.map_set,
            &[map_ptr.into(), key_ptr.into(), val_tagged.into()], "");
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
            // Typed index-signature map `{ String: T }` (ADR-055): O(1) hashed insert/overwrite.
            // Pass the map's value type `T` so a flat-scalar `T` is stored UNBOXED (inline in the
            // slot's TaggedVal, no heap box) and a narrower source value is widened to `T`.
            Type::Map(elem) => {
                if obj.is_pointer_value() && key.is_pointer_value() {
                    let key_str = resolve_obj_key(self, key);
                    self.emit_map_set(obj, key_str, value, val_ty, elem);
                }
            }
            Type::Array(elem) => {
                let idx = self.index_value_to_i64(key);
                // Sealed-record array element (Stage 3): copy the element struct's payload into the
                // slot (`lin_sealed_array_set` releases the old element's heap fields and retains the
                // new ones; a scalar-only record is a straight overwrite). `value` is a sealed
                // struct ptr; it stays owned by its caller (released at scope exit).
                if Self::sealed_array_elem(obj_ty).is_some() {
                    let ptr_ty = self.context.ptr_type(AddressSpace::default());
                    let i64_ty = self.context.i64_type();
                    let set_fn = self.get_or_declare_fn("lin_sealed_array_set",
                        self.context.void_type().fn_type(&[ptr_ty.into(), i64_ty.into(), ptr_ty.into()], false));
                    self.builder.call(set_fn, &[obj.into(), idx.into(), value.into()], "");
                }
                // Flat scalar element AND the value already has the matching scalar type ⇒ inline a
                // bounds-checked raw store (no box + cross-staticlib `lin_array_set` round-trip).
                // A differing scalar width/kind would need the runtime's value conversion, so fall
                // back. A FixedArray is always stored tagged (heterogeneous slots) — handled below.
                else if Self::is_flat_scalar(elem) && elem.as_ref() == val_ty {
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
                    self.emit_obj_or_map_set(obj, container, key_str, value, val_ty, obj_ty);
                    self.builder.unconditional_branch(mrg);
                    self.builder.position_at_end(mrg);
                } else if key.is_pointer_value() {
                    // Statically a string (object) key.
                    self.emit_obj_or_map_set(obj, container, key, value, val_ty, obj_ty);
                } else if key.is_int_value() {
                    let idx = self.index_value_to_i64(key);
                    self.emit_array_set(container, idx, value, val_ty);
                }
            }
            _ => {}
        }
    }

    /// String-keyed store into a union/`T|Null` container that may hold EITHER a Json object
    /// (TAG_OBJECT) OR a typed index-signature map (TAG_MAP). This is the write analogue of the
    /// tag-dispatched read in `compile_ir_index`: a NESTED typed map's inner write
    /// (`outer[a][b] = v`, where `outer[a]` is `{ String: T } | Null` — not `is`-narrowable,
    /// ADR-055 §5.1.1) reaches here with `obj_ty` a union containing a `Map(elem)` variant.
    /// When such a variant is present, dispatch on the runtime tag: TAG_MAP → `emit_map_set`
    /// (O(1) hashed insert), otherwise `emit_object_set`. Both helpers RETAIN the inner payload,
    /// so the ownership contract is identical on either branch. With no Map variant this is a
    /// plain `emit_object_set` (no extra branch emitted).
    pub(crate) fn emit_obj_or_map_set(
        &mut self,
        boxed_obj: BasicValueEnum<'ctx>,
        container: BasicValueEnum<'ctx>,
        key_str: BasicValueEnum<'ctx>,
        value: BasicValueEnum<'ctx>,
        val_ty: &Type,
        obj_ty: &Type,
    ) {
        // Find a Map(elem) variant in the union, if any.
        let map_elem: Option<Type> = match obj_ty {
            Type::Union(vs) => vs.iter().find_map(|v| match v {
                Type::Map(e) => Some((**e).clone()),
                _ => None,
            }),
            Type::Map(e) => Some((**e).clone()),
            _ => None,
        };
        let Some(elem) = map_elem else {
            self.emit_object_set(container, key_str, value, val_ty);
            return;
        };
        let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let i8t = self.context.i8_type();
        let tag = self.builder.call(self.rt.get_tag, &[boxed_obj.into()], "set_objtag").try_as_basic_value().unwrap_basic().into_int_value();
        let is_map = self.builder.int_compare(IntPredicate::EQ, tag, i8t.const_int(TAG_MAP as u64, false), "set_is_map");
        let map_b = self.context.append_basic_block(llvm_fn, "set_map");
        let obj_b = self.context.append_basic_block(llvm_fn, "set_obj");
        let mrg = self.context.append_basic_block(llvm_fn, "set_mrg");
        self.builder.conditional_branch(is_map, map_b, obj_b);
        self.builder.position_at_end(map_b);
        self.emit_map_set(container, key_str, value, val_ty, &elem);
        self.builder.unconditional_branch(mrg);
        self.builder.position_at_end(obj_b);
        self.emit_object_set(container, key_str, value, val_ty);
        self.builder.unconditional_branch(mrg);
        self.builder.position_at_end(mrg);
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

    /// FUSED `arr[idx].field` over a SEALED-RECORD ARRAY (Stage 3): a single constant-offset scalar
    /// load directly from the contiguous, header-less element — no per-element struct
    /// materialization. The element payload begins at `data + idx*stride`; the field lives at the
    /// struct-relative `sealed_field_layout` offset MINUS `SEALED_HEADER` (array elements are
    /// header-less). Bounds-checked inline (Python-style negative index; OOB defers to the runtime
    /// `lin_sealed_array_elem_ptr` so the fault message is byte-identical). `arr_ty` is `Array(elem)`.
    /// LinArray layout: len u64 @ byte 8, data ptr @ byte 24, elem_stride u64 @ byte 32.
    pub(crate) fn compile_ir_sealed_array_field_get(
        &mut self,
        arr: BasicValueEnum<'ctx>,
        idx: BasicValueEnum<'ctx>,
        field: &str,
        arr_ty: &Type,
        result_ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let Some(fields) = Self::sealed_array_elem(arr_ty) else {
            return ptr_ty.const_null().into();
        };
        let fields = fields.clone();
        let (field_off, _total) = Self::sealed_field_layout(&fields, field);
        let payload_off = field_off - Self::SEALED_HEADER;
        let fld_ty = fields.get(field).cloned().unwrap_or(Type::Null);
        let llvm_fld = self.llvm_type(&fld_ty);
        let arr_ptr = arr.into_pointer_value();

        // Normalise idx to i64.
        let idx = if idx.is_int_value() {
            let iv = idx.into_int_value();
            if iv.get_type().get_bit_width() == 64 { iv } else { self.builder.int_s_extend(iv, i64_ty, "sarr_idx64") }
        } else {
            self.unbox_value(idx, &Type::Int64).into_int_value()
        };

        // len = *(u64*)(arr + 8); stride = *(u64*)(arr + 32); data = *(ptr*)(arr + 24)
        let len_ptr = unsafe { self.builder.gep(self.context.i8_type(), arr_ptr, &[i64_ty.const_int(8, false)], "sarr_len_p") };
        let len = self.builder.load(i64_ty, len_ptr, "sarr_len").into_int_value();
        let zero = i64_ty.const_zero();
        let is_neg = self.builder.int_compare(IntPredicate::SLT, idx, zero, "sarr_idx_neg");
        let wrapped = self.builder.int_add(len, idx, "sarr_idx_wrap");
        let actual = self.builder.build_select(is_neg, wrapped, idx, "sarr_idx_actual").unwrap().into_int_value();
        let oob = self.builder.int_compare(IntPredicate::UGE, actual, len, "sarr_oob");

        let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let ok_b = self.context.append_basic_block(llvm_fn, "sarr_ok");
        let oob_b = self.context.append_basic_block(llvm_fn, "sarr_oob");
        self.builder.conditional_branch(oob, oob_b, ok_b);

        // Cold OOB path: defer to the runtime accessor with the ORIGINAL index for the identical
        // fault message; it does not return.
        self.builder.position_at_end(oob_b);
        let elem_fn = self.get_or_declare_fn("lin_sealed_array_elem_ptr",
            ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false));
        self.builder.call(elem_fn, &[arr_ptr.into(), idx.into()], "sarr_oob_call");
        self.builder.unreachable();

        // Fast path: stride = *(u64*)(arr+32); data = *(ptr*)(arr+24);
        //            field_ptr = data + actual*stride + payload_off; load.
        self.builder.position_at_end(ok_b);
        let stride_ptr = unsafe { self.builder.gep(self.context.i8_type(), arr_ptr, &[i64_ty.const_int(32, false)], "sarr_stride_p") };
        let stride = self.builder.load(i64_ty, stride_ptr, "sarr_stride").into_int_value();
        let data_pp = unsafe { self.builder.gep(self.context.i8_type(), arr_ptr, &[i64_ty.const_int(24, false)], "sarr_data_pp") };
        let data = self.builder.load(ptr_ty, data_pp, "sarr_data").into_pointer_value();
        let elem_off = self.builder.int_mul(actual, stride, "sarr_elem_off");
        let total_off = self.builder.int_add(elem_off, i64_ty.const_int(payload_off, false), "sarr_fld_off");
        let fld_p = unsafe { self.builder.gep(self.context.i8_type(), data, &[total_off], "sarr_fld_p") };
        let loaded = self.builder.load(llvm_fld, fld_p, "sarr_fld");
        if &fld_ty == result_ty { loaded } else { self.compile_ir_coerce(loaded, &fld_ty, result_ty) }
    }

    /// Project a wider/Json/`Object[]` source array (`src`, statically `src_ty`) into a FRESH
    /// SEALED-RECORD ARRAY of `arr_ty`'s element type (sealed-records Stage 3 boundary, §3.2). Builds
    /// a new contiguous sealed array and, per element, reads the source element as a boxed value,
    /// projects it into the element record's struct layout, and copies the projected payload into the
    /// sealed slot. Non-mutating; `src` keeps its own ownership. Rare path; correctness over speed.
    pub(crate) fn sealed_array_project_from(&mut self, src: BasicValueEnum<'ctx>, src_ty: &Type, arr_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let fields = match Self::sealed_array_elem(arr_ty) {
            Some(f) => f.clone(),
            None => return ptr_ty.const_null().into(),
        };
        let stride = Self::sealed_array_stride(&fields);
        let desc = self.sealed_descriptor(&fields);
        // Unbox the source to a raw LinArray* if it is a boxed Json/union value.
        let src_raw = if Self::is_union_type(src_ty) {
            self.builder.call(self.rt.unbox_ptr, &[src.into()], "sarrp_unbox").try_as_basic_value().unwrap_basic()
        } else { src };
        // len = lin_array_length(src_raw)
        let len_fn = self.get_or_declare_fn("lin_array_length", i64_ty.fn_type(&[ptr_ty.into()], false));
        let len = self.builder.call(len_fn, &[src_raw.into()], "sarrp_len").try_as_basic_value().unwrap_basic().into_int_value();
        // out = lin_sealed_array_alloc(len, stride, desc)
        let alloc_fn = self.get_or_declare_fn("lin_sealed_array_alloc",
            ptr_ty.fn_type(&[i64_ty.into(), i64_ty.into(), ptr_ty.into()], false));
        let out = self.builder.call(alloc_fn, &[len.into(), i64_ty.const_int(stride, false).into(), desc.into()], "sarrp_out")
            .try_as_basic_value().unwrap_basic();
        let push_fn = self.get_or_declare_fn("lin_sealed_array_push_struct_retaining",
            self.context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
        let get_tagged = self.get_or_declare_fn("lin_array_get_tagged", ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false));
        // Loop i in [0, len): proj = project(boxed src[i]); push proj payload (retaining heap fields).
        let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let head = self.context.append_basic_block(llvm_fn, "sarrp_head");
        let body = self.context.append_basic_block(llvm_fn, "sarrp_body");
        let done = self.context.append_basic_block(llvm_fn, "sarrp_done");
        let idx_slot = self.builder.alloca(i64_ty, "sarrp_i");
        self.builder.store(idx_slot, i64_ty.const_zero());
        self.builder.unconditional_branch(head);
        self.builder.position_at_end(head);
        let i = self.builder.load(i64_ty, idx_slot, "sarrp_iv").into_int_value();
        let cond = self.builder.int_compare(IntPredicate::SLT, i, len, "sarrp_cond");
        self.builder.conditional_branch(cond, body, done);
        self.builder.position_at_end(body);
        let elem_box = self.builder.call(get_tagged, &[src_raw.into(), i.into()], "sarrp_get").try_as_basic_value().unwrap_basic();
        // Project the boxed element (a Json TaggedVal*) into the element record's struct layout.
        let proj = self.sealed_project_from(elem_box, &Type::TypeVar(u32::MAX), &fields);
        self.builder.call(push_fn, &[out.into(), proj.into()], "");
        // Release the projected struct (its heap fields were retained into the slot) and the boxed
        // element (lin_array_get_tagged returns a fresh +1 box).
        self.emit_sealed_release(proj, &fields);
        if elem_box.is_pointer_value() {
            self.builder.call(self.rt.tagged_release, &[elem_box.into()], "");
        }
        let next = self.builder.int_add(i, i64_ty.const_int(1, false), "sarrp_next");
        self.builder.store(idx_slot, next);
        self.builder.unconditional_branch(head);
        self.builder.position_at_end(done);
        out
    }

    /// Materialize a whole SEALED-RECORD ARRAY into a tagged `Object[]` `LinArray*` (the Json view).
    /// Emits a per-element-type materializer thunk and calls `lin_sealed_array_to_tagged`. The
    /// thunk reads each element's header-less payload and builds a fresh boxed `LinObject`. Used at
    /// the Json boundary (boxing / dynamic ops) where the contiguous unboxed buffer can't be read by
    /// the dynamic machinery.
    pub(crate) fn sealed_array_to_tagged(&mut self, arr: BasicValueEnum<'ctx>, arr_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let fields = match Self::sealed_array_elem(arr_ty) {
            Some(f) => f.clone(),
            None => return arr,
        };
        let mat = self.sealed_array_elem_materializer(&fields);
        let to_tagged = self.get_or_declare_fn("lin_sealed_array_to_tagged",
            ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
        self.builder.call(to_tagged, &[arr.into(), mat.as_global_value().as_pointer_value().into()], "sarr_tagged")
            .try_as_basic_value().unwrap_basic()
    }

    /// Emit (and cache) a per-sealed-type element materializer thunk `(payload_ptr) -> *LinObject`:
    /// reads each field from the HEADER-LESS element payload (struct offset minus `SEALED_HEADER`),
    /// boxes it, and `object_set_fresh`'s it under the interned key — producing a fresh +1 boxed
    /// object. Mirrors `sealed_materialize_to_object` but the source is a payload pointer, not a
    /// standalone struct. Scalar-only (Stage 3): no per-field RC to balance (the boxed scalar shell
    /// is reclaimed by object_set_fresh's retain + a tagged_release).
    fn sealed_array_elem_materializer(&mut self, fields: &indexmap::IndexMap<String, Type>) -> inkwell::values::FunctionValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let i32_ty = self.context.i32_type();
        // Cache key by the field layout (offsets + types reflected via stride + names).
        let key = format!("__sealedarrmat_{}_{}",
            Self::sealed_array_stride(fields),
            fields.iter().map(|(k, t)| format!("{}_{:?}", k, Self::type_tag(t))).collect::<Vec<_>>().join("_"));
        if let Some(f) = self.module.get_function(&key) {
            return f;
        }
        // Save the current insertion point; the thunk is a separate function.
        let saved_block = self.builder.get_insert_block();
        let fn_ty = ptr_ty.fn_type(&[ptr_ty.into()], false);
        let func = self.module.add_function(&key, fn_ty, None);
        let entry = self.context.append_basic_block(func, "entry");
        self.builder.position_at_end(entry);
        let payload = func.get_nth_param(0).unwrap().into_pointer_value();
        let new_obj = self.builder.call(self.rt.object_alloc, &[i32_ty.const_int(fields.len() as u64, false).into()], "smat_obj")
            .try_as_basic_value().unwrap_basic().into_pointer_value();
        for (k, fty) in fields.iter() {
            let (off, _) = Self::sealed_field_layout(fields, k);
            let payload_off = off - Self::SEALED_HEADER;
            let llvm_fld = self.llvm_type(fty);
            let p = unsafe { self.builder.gep(self.context.i8_type(), payload, &[i64_ty.const_int(payload_off, false)], "smat_fld_p") };
            let loaded = self.builder.load(llvm_fld, p, "smat_fld");
            let boxed = self.box_value(loaded, fty);
            let key_str = self.compile_string_lit(k).into_pointer_value();
            self.builder.call(self.rt.object_set_fresh, &[new_obj.into(), key_str.into(), boxed.into()], "");
            // Scalar field: reclaim the cache-safe box shell (no inner heap).
            if boxed.is_pointer_value() {
                self.builder.call(self.rt.tagged_release, &[boxed.into()], "");
            }
        }
        self.builder.build_return(Some(&new_obj)).unwrap();
        if let Some(b) = saved_block {
            self.builder.position_at_end(b);
        }
        func
    }

    /// Materialize element `idx` of a sealed-record array as a FRESH standalone sealed struct (+1
    /// owned): allocate the struct, then byte-copy the element's `stride`-byte payload (from
    /// `lin_sealed_array_elem_ptr`) into the struct payload region (offset `SEALED_HEADER`). For a
    /// SCALAR-only record this is a complete copy with no per-field RC. Used for `arr[i]` as a whole
    /// value (the fused `arr[i].field` read never reaches here).
    pub(crate) fn sealed_array_materialize_elem(
        &mut self,
        arr: BasicValueEnum<'ctx>,
        idx: inkwell::values::IntValue<'ctx>,
        fields: &indexmap::IndexMap<String, Type>,
    ) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let total = Self::sealed_struct_size(fields);
        let stride = Self::sealed_array_stride(fields);
        let desc = self.sealed_descriptor(fields);
        // Fresh +1 sealed struct.
        let obj = self.builder.call(self.rt.sealed_alloc,
            &[i64_ty.const_int(total, false).into(), desc.into()], "sarr_mat")
            .try_as_basic_value().unwrap_basic().into_pointer_value();
        // Element payload source pointer (bounds-checked in the runtime).
        let elem_fn = self.get_or_declare_fn("lin_sealed_array_elem_ptr",
            ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false));
        let src = self.builder.call(elem_fn, &[arr.into(), idx.into()], "sarr_mat_src")
            .try_as_basic_value().unwrap_basic().into_pointer_value();
        // dst payload begins at obj + SEALED_HEADER.
        let dst = unsafe {
            self.builder.gep(self.context.i8_type(), obj, &[i64_ty.const_int(Self::SEALED_HEADER, false)], "sarr_mat_dst")
        };
        self.builder.build_memcpy(dst, 8, src, 8, i64_ty.const_int(stride, false))
            .expect("sealed_array_materialize_elem memcpy");
        obj.into()
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
    /// Sealed-records Stage 4: construct an ALL-SCALAR sealed record on the STACK. The escape
    /// analysis (`lin_ir::escape`) proved this construction never escapes its frame, so instead of
    /// `lin_sealed_alloc` (heap + refcount) we use a function-ENTRY-BLOCK `alloca`.
    ///
    /// Two soundness-critical properties:
    ///  1. **Entry-block alloca, reused per iteration.** The `alloca` is emitted at the START of the
    ///     function's entry block, NOT at the (loop-body) construction site. So in a TCO loop the
    ///     SAME stack slot is reused every iteration — the stack does NOT grow per iteration (a fresh
    ///     in-loop alloca would be a stack overflow at high N). All of the record's field values are
    ///     already computed into SSA temps before this instruction (MakeObject takes field temps), so
    ///     overwriting the slot with the new iteration's values cannot corrupt the reads that
    ///     produced them.
    ///  2. **Immortal refcount → RC is inert (defense-in-depth).** With RC-emission suppression the
    ///     lowerer omits Retain/Release on this value entirely, so the alloca can SROA-promote to
    ///     registers. As a belt-and-braces guard the header refcount is still initialised to the
    ///     immortal sentinel (`>= IMMORTAL_RC`), so any `lin_rc_retain` / `lin_sealed_release` that
    ///     DOES slip through (e.g. via a path the suppression missed) is a runtime no-op — a stack
    ///     pointer is NEVER passed to `dealloc`.
    ///
    /// Scalar-only: every field is stored inline by offset; no heap field, no descriptor, no
    /// per-field retain. `desc_ptr` (header offset 8) is left NULL (no heap fields to walk on drop).
    pub(crate) fn sealed_construct_stack(
        &mut self,
        fields: &indexmap::IndexMap<String, Type>,
        field_vals: &[(String, BasicValueEnum<'ctx>, Type, bool)],
        llvm_fn: inkwell::values::FunctionValue<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let total = Self::sealed_struct_size(fields);
        let i32_ty = self.context.i32_type();
        let i64_ty = self.context.i64_type();
        let i8_ty = self.context.i8_type();

        // Emit the alloca at the start of the function's entry block (the standard LLVM idiom) so it
        // is allocated ONCE per call and reused across every TCO loop iteration. Save/restore the
        // builder position around it.
        let saved = self.builder.get_insert_block();
        let entry = llvm_fn.get_first_basic_block().expect("function has an entry block");
        match entry.get_first_instruction() {
            Some(first) => self.builder.position_before(&first),
            None => self.builder.position_at_end(entry),
        }
        // Allocate as `[N x i64]` (NOT `[total x i8]`) so the slot is 8-ALIGNED — the i64/f64 field
        // loads/stores at 8-aligned offsets must not be under-aligned (a misaligned 8-byte access is
        // LLVM UB and faults on stricter arches, e.g. arm64). `total` is always an 8-byte multiple
        // (`sealed_struct_size` pads to 8), so the division is exact.
        debug_assert_eq!(total % 8, 0, "sealed struct size must be 8-padded");
        let slot_ty = i64_ty.array_type((total / 8) as u32);
        let obj = self.builder.alloca(slot_ty, "sealed_stack");
        // Restore to the construction site for the header/field stores.
        if let Some(bb) = saved {
            self.builder.position_at_end(bb);
        }

        // Header: rc @ 0 = IMMORTAL_RC, size @ 4 = total, desc_ptr @ 8 = NULL.
        let rc_p = unsafe { self.builder.gep(i8_ty, obj, &[i64_ty.const_int(0, false)], "sealed_stk_rc") };
        self.builder.store(rc_p, i32_ty.const_int(Self::SEALED_IMMORTAL_RC as u64, false));
        let sz_p = unsafe { self.builder.gep(i8_ty, obj, &[i64_ty.const_int(4, false)], "sealed_stk_sz") };
        self.builder.store(sz_p, i32_ty.const_int(total, false));
        let desc_p = unsafe { self.builder.gep(i8_ty, obj, &[i64_ty.const_int(8, false)], "sealed_stk_desc") };
        self.builder.store(desc_p, self.context.ptr_type(AddressSpace::default()).const_null());

        // Fields: all scalars, stored inline by offset. No coerce (scalar field type == value type
        // under this gate), no retain.
        for (name, val, _val_ty, _already_owned) in field_vals {
            let (offset, _) = Self::sealed_field_layout(fields, name);
            let p = unsafe {
                self.builder.gep(i8_ty, obj, &[i64_ty.const_int(offset, false)], "sealed_stk_set_p")
            };
            self.builder.store(p, *val);
        }
        obj.into()
    }

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