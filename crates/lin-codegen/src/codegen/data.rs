use super::builder_ext::BuilderExt;
use inkwell::types::BasicType;
use inkwell::values::BasicValueEnum;
use lin_common::tags::{TAG_INT32, TAG_INT64, TAG_OBJECT, TAG_MAP, TAG_RECORD};
use inkwell::{AddressSpace, IntPredicate};

use lin_check::types::Type;
use super::Codegen;

impl<'ctx> Codegen<'ctx> {
    /// Allocate a SCRATCH stack slot in the function's ENTRY block (the standard LLVM idiom; see
    /// `sealed_construct_stack` for the same pattern). Use this for short-lived scratch the value is
    /// COPIED OUT of immediately at its use site (e.g. an `arr_cell` whose bytes `lin_array_push`
    /// copies into the array before the next statement) — emitting the `alloca` at the (loop-body)
    /// use site instead makes the stack GROW one slot per iteration, a stack overflow at high N (the
    /// inline-heap-array-literal-in-a-fused-loop overflow: `xs.flatMap(s => [s, s, s])` spliced into a
    /// 100k iteration loop). An entry-block alloca is allocated ONCE per call and the slot is reused
    /// every iteration — sound here precisely because the cell is dead the instant `lin_array_push`
    /// returns, so the next iteration's overwrite races nothing.
    pub(crate) fn entry_alloca<T: BasicType<'ctx>>(&self, ty: T, name: &str) -> inkwell::values::PointerValue<'ctx> {
        let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let saved = self.builder.get_insert_block();
        let entry = llvm_fn.get_first_basic_block().expect("function has an entry block");
        match entry.get_first_instruction() {
            Some(first) => self.builder.position_before(&first),
            None => self.builder.position_at_end(entry),
        }
        let cell = self.builder.alloca(ty, name);
        if let Some(bb) = saved {
            self.builder.position_at_end(bb);
        }
        cell
    }

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
                let obj = self.sealed_materialize_to_map(val, &fields);
                let tag = i8_ty.const_int(Self::type_tag_open(val_ty) as u64, false);
                let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
                let cell = self.entry_alloca(ptr_ty, "arr_cell");
                self.builder.store(cell, obj);
                self.builder.call(self.rt.array_push, &[arr.into(), cell.into(), tag.into()], "arr_push");
                return;
            }
        }
        match val_ty {
            // Boxed opaque handles (Promise/Shared/Stream) are `TaggedVal*` with their own tag, so
            // they push into a tagged array exactly like a union/TypeVar value: copy the 16-byte
            // TaggedVal so the element carries `(tag, payload)`. `lin_race` and friends read the
            // element's payload as the inner handle pointer — a flat raw-pointer push (the `_` arm,
            // tag 0) would store the box pointer in the payload slot AND mis-tag it, so the runtime
            // would deref the box header as the inner handle.
            Type::TypeVar(_) | Type::Union(_) | Type::Promise(_) | Type::Shared(_) | Type::Stream(_) | Type::TarEntry =>
                self.push_tagged_val(arr, val, val_ty),
            _ => {
                // Phase 2: use type_tag_open so non-sealed open objects get TAG_MAP, not TAG_OBJECT.
                let tag_val = Self::type_tag_open(val_ty);
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
                        let cell = self.entry_alloca(ptr_ty, "arr_cell");
                        self.builder.store(cell, val);
                        cell
                    }
                    _ => {
                        let i64_ty = self.context.i64_type();
                        let payload = self.tagged_payload_i64(&val, val_ty);
                        let cell = self.entry_alloca(i64_ty, "arr_cell");
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

    pub(crate) fn compile_ir_index(&mut self, obj: BasicValueEnum<'ctx>, key: BasicValueEnum<'ctx>, obj_ty: &Type, key_ty: &Type, result_ty: &Type, obj_repr: &lin_ir::repr::Repr) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        if !obj.is_pointer_value() {
            return ptr_ty.const_null().into();
        }
        // UNBOXED SUM TYPE (unboxed-sumtype Stage 1): when the object operand's repr is a SumNode, an
        // `obj[key]` index is served by materializing the node (the discriminant read the shipped
        // match-dispatch `scrut["kind"] == "circle"` lowers to), NOT `lin_object_get` on a non-LinObject.
        // NOTE: currently INERT — no temp carries `Packed(SumNode)` yet (the repr seed is gated off
        // pending the call ABI; see `repr::type_seed`), so this branch is never taken on the present
        // corpus. It is the wired index/dispatch site the ABI follow-up activates.
        if let Some(sum_ty) = obj_repr.sumnode_sum_ty() {
            let sum_ty = sum_ty.clone();
            let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
            // Runtime key: materialize the whole node to a fresh LinMap* once, then map_get.
            let obj_box = self.sumnode_materialize_to_object(obj, &sum_ty, llvm_fn).into_pointer_value();
            let key_raw = if Self::is_union_type(key_ty) && key.is_pointer_value() {
                self.builder.call(self.rt.unbox_ptr, &[key.into()], "sumnode_idx_kstr").try_as_basic_value().unwrap_basic()
            } else {
                key
            };
            let got = self.builder.call(self.rt.map_get, &[obj_box.into(), key_raw.into()], "sumnode_idx_get").try_as_basic_value().unwrap_basic();
            let cloned = if got.is_pointer_value() {
                let clone_fn = self.get_or_declare_fn("lin_tagged_clone", ptr_ty.fn_type(&[ptr_ty.into()], false));
                self.builder.call(clone_fn, &[got.into()], "sumnode_idx_clone").try_as_basic_value().unwrap_basic()
            } else {
                got
            };
            self.builder.call(self.rt.map_release, &[obj_box.into()], "");
            return cloned;
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
            // string key → object/map get: dispatch on the container's tag (TAG_MAP or TAG_OBJECT).
            self.builder.position_at_end(str_b);
            let key_raw = self.builder.call(self.rt.unbox_ptr, &[key.into()], "ir_idxk_str").try_as_basic_value().unwrap_basic();
            let obj_tag = self.builder.call(self.rt.get_tag, &[obj.into()], "ir_idx_otag").try_as_basic_value().unwrap_basic().into_int_value();
            let is_map = self.builder.int_compare(IntPredicate::EQ, obj_tag, i8t.const_int(TAG_MAP as u64, false), "ir_idx_ismap");
            let is_obj = self.builder.int_compare(IntPredicate::EQ, obj_tag, i8t.const_int(TAG_OBJECT as u64, false), "ir_idx_isobj");
            let mget_b = self.context.append_basic_block(llvm_fn, "ir_idx_mget");
            let oget_b = self.context.append_basic_block(llvm_fn, "ir_idx_oget");
            let chk_obj = self.context.append_basic_block(llvm_fn, "ir_idx_chkobj");
            let onull_b = self.context.append_basic_block(llvm_fn, "ir_idx_onull");
            let omrg = self.context.append_basic_block(llvm_fn, "ir_idx_omrg");
            self.builder.conditional_branch(is_map, mget_b, chk_obj);
            self.builder.position_at_end(mget_b);
            let mget = self.builder.call(self.rt.map_get, &[container.into(), key_raw.into()], "ir_idx_msget").try_as_basic_value().unwrap_basic();
            let mget_exit = self.builder.get_insert_block().unwrap();
            self.builder.unconditional_branch(omrg);
            self.builder.position_at_end(chk_obj);
            self.builder.conditional_branch(is_obj, oget_b, onull_b);
            self.builder.position_at_end(oget_b);
            let oget = self.builder.call(self.rt.object_get, &[container.into(), key_raw.into()], "ir_idx_osget").try_as_basic_value().unwrap_basic();
            let oget_exit = self.builder.get_insert_block().unwrap();
            self.builder.unconditional_branch(omrg);
            self.builder.position_at_end(onull_b);
            self.builder.unconditional_branch(omrg);
            self.builder.position_at_end(omrg);
            let ophi = self.builder.phi(ptr_ty, "ir_idx_ophi");
            ophi.add_incoming(&[(&mget, mget_exit), (&oget, oget_exit), (&ptr_ty.const_null(), onull_b)]);
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
        if let Type::Map { key: map_key_ty, value: map_elem } = obj_ty {
            let _ = map_elem;
            // Int-keyed map: coerce the key to i64 and call lin_map_get_int.
            if map_key_ty.is_integer() {
                let i64_key = if key.is_int_value() {
                    self.builder.int_s_extend_or_bit_cast(key.into_int_value(), self.context.i64_type(), "ir_mkey_i64")
                } else if key.is_pointer_value() {
                    let unboxed = self.unbox_value(key, &Type::Int64);
                    unboxed.into_int_value()
                } else {
                    self.context.i64_type().const_zero()
                };
                let tagged = self.builder.call(self.rt.map_get_int, &[container.into(), i64_key.into()], "ir_mget_int").try_as_basic_value().unwrap_basic();
                return if Self::is_union_type(result_ty) {
                    tagged
                } else {
                    self.unbox_tagged_val_to_type(tagged, result_ty)
                };
            }
            // String-keyed map path (existing logic).
            // unboxed-sumtype Stage 3: a `{ String: Expr }` map slot holds a KEEP-PACKED `TAG_SUMNODE`
            // (the `emit_map_set` keep-packed store). Decide the keep-packed read-back from the map's
            // VALUE type (`obj_ty = Map{value:elem}`) — not `result_ty`, which is the wider `Expr | Null`
            // safe-access view whose `Named`/`| Null` shape the codegen sum predicate may not match.
            // When the value type is a sum type, unwrap the slot's TAG_SUMNODE to the still-packed
            // `*SumNode` (+retain) — the read-back twin of the keep-packed store. A missing key unwraps
            // to a null pointer (Null). The downstream `sum|Null` consumer materializes via `box_value`.
            let key_str = if key_ty.is_string_ish() {
                key
            } else if Self::is_union_type(key_ty) && key.is_pointer_value() {
                self.builder.call(self.rt.unbox_ptr, &[key.into()], "ir_mkey_unbox").try_as_basic_value().unwrap_basic()
            } else {
                key
            };
            // KEEP-PACKED read-back: when the map value type is a PACKED sealed array / sealed record,
            // the slot holds a keep-packed handle (a TaggedVal wrapping the still-packed buffer, stored
            // by `emit_map_set`'s `compile_ir_box_keep_packed`). Unbox it as a packed pointer + retain
            // (`compile_ir_unbox_keep_packed`) — a fresh +1 owner matching what the old materialize path
            // produced (so the projection's scheduled Release balances). Zero copy: the inner buffer
            // never materializes. `lin_map_get` returns a BORROWED interior TaggedVal*, so the retain on
            // the unboxed payload is what gives the result its own reference. A packed sealed value can
            // ONLY have been stored via that keep-packed store into a real `LinMap`, so the container is
            // guaranteed TAG_MAP here — the direct `map_get` is sound. (These helpers are called
            // directly; the never-emitted `Box/UnboxKeepPacked` IR opcodes were removed.)
            if Self::sealed_array_elem(result_ty).is_some() {
                let tagged = self.builder.call(self.rt.map_get, &[container.into(), key_str.into()], "ir_mget").try_as_basic_value().unwrap_basic();
                return self.compile_ir_unbox_keep_packed(tagged, /*arr=*/true);
            }
            // Bare sealed-record result (a NARROWED `m[k]` read — index-place narrowing can give the
            // Index a bare `T` result type): the slot holds a MATERIALIZED boxed `LinObject` (see
            // `emit_map_set` — bare records are never keep-packed in map slots, a sealed struct is
            // not a `LinObject` and cannot be tag-dispatched). PROJECT the boxed object into a fresh
            // +1 sealed struct — the same ownership the old unbox-keep-packed read produced, so the
            // projection's scheduled Release balances identically.
            if let Some(fields) = Self::sealed_fields(result_ty) {
                let fields = fields.clone();
                let tagged = self.builder.call(self.rt.map_get, &[container.into(), key_str.into()], "ir_mget").try_as_basic_value().unwrap_basic();
                return self.sealed_project_from(tagged, &Type::TypeVar(u32::MAX), &fields);
            }
            // Stage 3 NullableRecord: `T | Null` result where T is a sealed record. The slot holds
            // a MATERIALIZED TAG_OBJECT (see `emit_map_set` — bare records materialize before store).
            // `lin_map_get` returns a borrowed TaggedVal*(TAG_OBJECT) for a hit, or null for a miss.
            // Project the hit into a fresh +1 sealed struct; return null ptr for a miss.
            // NullableRecord repr = raw `*T` (non-null) or null — no TaggedVal wrapper.
            if let Some(fields) = Self::nullable_sealed_record_type(result_ty) {
                let fields = fields.clone();
                let ptr_ty_local = self.context.ptr_type(AddressSpace::default());
                let i64_ty = self.context.i64_type();
                let tagged = self.builder.call(self.rt.map_get, &[container.into(), key_str.into()], "ir_mget_nr").try_as_basic_value().unwrap_basic();
                // Null-guard: miss → return null ptr (no projection on null).
                let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                let pi = self.builder.ptr_to_int(tagged.into_pointer_value(), i64_ty, "nr_mget_p2i");
                let is_null = self.builder.int_compare(
                    inkwell::IntPredicate::EQ, pi, i64_ty.const_zero(), "nr_mget_isnull");
                let hit_bb = self.context.append_basic_block(llvm_fn, "nr_mget_hit");
                let merge_bb = self.context.append_basic_block(llvm_fn, "nr_mget_merge");
                self.builder.conditional_branch(is_null, merge_bb, hit_bb);
                let miss_pred = self.builder.get_insert_block().unwrap();
                self.builder.position_at_end(hit_bb);
                let projected = self.sealed_project_from(tagged, &Type::TypeVar(u32::MAX), &fields);
                let hit_pred = self.builder.get_insert_block().unwrap();
                self.builder.unconditional_branch(merge_bb);
                self.builder.position_at_end(merge_bb);
                let phi = self.builder.phi(ptr_ty_local, "nr_mget_phi");
                phi.add_incoming(&[(&ptr_ty_local.const_null(), miss_pred), (&projected, hit_pred)]);
                return phi.as_basic_value();
            }
            // GENERAL read. `container` is a RAW pointer (the `{ String: T }` ABI passes the unboxed
            // container, not a boxed TaggedVal), so its tag is NOT readable here — we rely on the
            // Json→Map coercion boundary (`compile_ir_coerce`) having already materialized any
            // object-shaped source into a real `LinMap`, so a `Type::Map` value is always a `LinMap`
            // at runtime. `lin_map_get` returns null for a missing key.
            let tagged = self.builder.call(self.rt.map_get, &[container.into(), key_str.into()], "ir_mget").try_as_basic_value().unwrap_basic();
            // UNBOXED SUM TYPE: a `{ String: Expr }` map value is stored MATERIALIZED as a boxed
            // `LinObject` (TAG_OBJECT — see `emit_map_set`); the read-back returns the borrowed box for
            // the `Expr | Null` union result, which the consumer's `box_value`/match boundary handles.
            // (Keep-packed-by-pointer for the Map value slot is DEFERRED: the IR lowering wraps a
            // union-typed Index result in a `CloneBox` that assumes a `TaggedVal*`, so returning a raw
            // `*SumNode` here would clone a non-box. The RECORD-field slot — read via `FieldGet`, no
            // union CloneBox — IS keep-packed (see `compile_ir_field_get`).)
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
            // STAGE 3: packed-sealed-array ASSUME read from the object operand's repr (proven by the
            // pass + verifier to match where the old `sealed_array_elem(obj_ty)` gate fired).
            if obj_repr.packed_sealed_array_layout().is_some() {
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
            // UNBOXED SUM TYPE: a `Shape[]` element was stored MATERIALIZED (a boxed LinObject). Read
            // it back and PROJECT into a fresh SumNode so the consumer sees the packed repr the type
            // implies (the result is genuinely a SumNode — repr-consistent for verify).
            if Self::is_sum_type(result_ty) {
                let get_tagged_fn = self.get_or_declare_fn("lin_array_get_tagged",
                    ptr_ty.fn_type(&[ptr_ty.into(), self.context.i64_type().into()], false));
                let tagged = self.builder.call(get_tagged_fn, &[container.into(), idx.into()], "ir_aget_sum").try_as_basic_value().unwrap_basic();
                let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                return self.sumnode_project_from_boxed(tagged, result_ty, result_ty, llvm_fn);
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
        // Sealed record indexed by a NON-LITERAL key (`p[k]`). A sealed record is a packed,
        // header-prefixed struct with NO runtime key table, so a dynamic key can't resolve a slot
        // by offset; reading it as a `LinObject` (the generic object path below) misinterprets the
        // packed bytes and crashes the runtime. The literal-key case is fused upstream (IR
        // lowering → constant-offset FieldGet / Null const); only a runtime key reaches here.
        // Materialize the sealed record to a boxed `LinObject` (its EXACTLY-declared fields — extras
        // already stripped) and do the normal dynamic `lin_object_get`, which returns the matching
        // value or Null for an absent key (safe-access §6.1). Clone the (borrowed, interior) result
        // into a fresh owned box and release the temporary object before returning, so nothing
        // dangles once the materialized object is freed.
        // STAGE 3: a sealed record indexed by a non-literal key — packed-struct ASSUME from repr.
        if let Some(fields) = obj_repr.packed_struct_fields().cloned() {
            if obj.is_pointer_value() {
                // Stage 6b Phase 2: materialize sealed record to a fresh LinMap* then map_get.
                let mat = self.sealed_materialize_to_map(obj, &fields).into_pointer_value();
                let key_raw = if Self::is_union_type(key_ty) && key.is_pointer_value() {
                    self.builder.call(self.rt.unbox_ptr, &[key.into()], "sealed_dynk_unbox").try_as_basic_value().unwrap_basic()
                } else {
                    key
                };
                let entry = self.builder.call(self.rt.map_get, &[mat.into(), key_raw.into()], "sealed_dynk_get").try_as_basic_value().unwrap_basic();
                // `entry` is a borrowed interior `*TaggedVal` (or null) into `mat`; clone it into an
                // independent owned box, then free `mat` (the clone keeps the inner alive).
                let clone_fn = self.get_or_declare_fn("lin_tagged_clone", ptr_ty.fn_type(&[ptr_ty.into()], false));
                let owned = self.builder.call(clone_fn, &[entry.into()], "sealed_dynk_clone").try_as_basic_value().unwrap_basic();
                self.builder.call(self.rt.map_release, &[mat.into()], "");
                // `owned` is a +1 box; the IR lowering's projection CloneBox (union result) clones it
                // again into the binding's owned box — balanced. Match the surrounding repr.
                return if Self::is_union_type(result_ty) { owned } else { self.unbox_tagged_val_to_type(owned, result_ty) };
            }
            return ptr_ty.const_null().into();
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
            // TAG_OBJECT → `lin_object_get` (the Json association-list path),
            // TAG_RECORD → `lin_record_get_field` (descriptor-driven field read, Stage 6a),
            // otherwise Null.
            //
            // OWNERSHIP CONTRACT:
            //   map_get / object_get return a BORROWED `*const TaggedVal` (interior pointer into
            //   the container); `unbox_tagged_val_to_type` reads it without retaining.
            //   lin_record_get_field returns an OWNED `+1 *mut TaggedVal` (a heap-allocated box
            //   that owns a retain on heap-typed fields). To normalise ownership, the TAG_RECORD
            //   arm unboxes the owned box and then releases it, producing a final result that
            //   matches the borrowed-equivalent semantics of the other arms.
            //
            //   Control-flow structure (two-level merge):
            //   entry → is_map? → map_b → inner_mrg
            //         → is_obj? → obj_ok → inner_mrg
            //         → is_rec? → rec_b → (unbox, release) → final_mrg
            //         → no → final_mrg (null result)
            //   inner_mrg → (unbox) → final_mrg
            //   final_mrg holds the final unboxed value of the target result type.
            let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
            let obj_tag = self.builder.call(self.rt.get_tag, &[obj.into()], "ir_idx_tag").try_as_basic_value().unwrap_basic().into_int_value();
            let i8t = self.context.i8_type();
            let is_map = self.builder.int_compare(
                IntPredicate::EQ, obj_tag, i8t.const_int(TAG_MAP as u64, false), "ir_idx_is_map");
            let is_obj = self.builder.int_compare(
                IntPredicate::EQ, obj_tag, i8t.const_int(TAG_OBJECT as u64, false), "ir_idx_is_obj");
            let is_record = self.builder.int_compare(
                IntPredicate::EQ, obj_tag, i8t.const_int(TAG_RECORD as u64, false), "ir_idx_is_rec");
            let map_b = self.context.append_basic_block(llvm_fn, "ir_idx_map");
            let chk_obj = self.context.append_basic_block(llvm_fn, "ir_idx_chk_obj");
            let ok = self.context.append_basic_block(llvm_fn, "ir_idx_obj_ok");
            let chk_rec = self.context.append_basic_block(llvm_fn, "ir_idx_chk_rec");
            let rec_b = self.context.append_basic_block(llvm_fn, "ir_idx_rec");
            let no = self.context.append_basic_block(llvm_fn, "ir_idx_obj_no");
            let inner_mrg = self.context.append_basic_block(llvm_fn, "ir_idx_inner_mrg"); // map/obj/null
            let final_mrg = self.context.append_basic_block(llvm_fn, "ir_idx_final_mrg"); // all paths
            self.builder.conditional_branch(is_map, map_b, chk_obj);
            self.builder.position_at_end(map_b);
            let map_entry = self.builder.call(self.rt.map_get, &[container.into(), key_str.into()], "ir_mget_u").try_as_basic_value().unwrap_basic();
            let map_exit = self.builder.get_insert_block().unwrap();
            self.builder.unconditional_branch(inner_mrg);
            self.builder.position_at_end(chk_obj);
            self.builder.conditional_branch(is_obj, ok, chk_rec);
            self.builder.position_at_end(ok);
            let entry = self.builder.call(self.rt.object_get, &[container.into(), key_str.into()], "ir_oget").try_as_basic_value().unwrap_basic();
            let ok_exit = self.builder.get_insert_block().unwrap();
            self.builder.unconditional_branch(inner_mrg);
            self.builder.position_at_end(chk_rec);
            self.builder.conditional_branch(is_record, rec_b, no);
            self.builder.position_at_end(no);
            let null_res = ptr_ty.const_null();
            self.builder.unconditional_branch(inner_mrg);
            // inner_mrg: collect borrowed TaggedVal* from map/obj/null paths, unbox, branch to final.
            self.builder.position_at_end(inner_mrg);
            let inner_phi = self.builder.phi(ptr_ty, "ir_idx_inner_phi");
            inner_phi.add_incoming(&[(&map_entry, map_exit), (&entry, ok_exit), (&null_res, no)]);
            let inner_result_ptr = inner_phi.as_basic_value();
            let inner_unboxed = self.unbox_tagged_val_to_type(inner_result_ptr, result_ty);
            // inner_unboxed may be a pointer or a scalar. For the phi we need a uniform type; we use
            // ptr_ty (all LLVM values can be carried as i64 via ptrtoint/inttoptr or just ptr).
            // Actually: we must carry the exact LLVM basic type of the result. Use the LLVM result type.
            let inner_mrg_exit = self.builder.get_insert_block().unwrap();
            self.builder.unconditional_branch(final_mrg);
            // Stage 6a: TAG_RECORD arm — call lin_record_get_field, get owned box, unbox, release.
            self.builder.position_at_end(rec_b);
            let sealed_ptr = self.builder.call(self.rt.unbox_ptr, &[obj.into()], "ir_rec_sptr").try_as_basic_value().unwrap_basic();
            let rec_get_fn = self.get_or_declare_fn("lin_record_get_field",
                ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
            let rec_box = self.builder.call(rec_get_fn, &[sealed_ptr.into(), key_str.into()], "ir_recget").try_as_basic_value().unwrap_basic();
            // rec_box is null (field not found) or an owned +1 TaggedVal*.
            //
            // TWO ownership paths depending on result_ty:
            //
            // (A) result_ty is Union/Json: the TaggedVal* IS the typed result (already a box).
            //     `unbox_tagged_val_to_type` passes it through unchanged.  We must NOT release
            //     the box — the caller now owns the +1.  (No retain needed either.)
            //
            // (B) result_ty is a concrete type (Int32, Str, Object, …): unbox the box payload
            //     to extract the raw value, retain if it's a heap pointer (so the retain
            //     outlives the box-release below), then release the box shell.
            //     lin_rc_retain / lin_tagged_release on null are both no-ops.
            let rec_unboxed = self.unbox_tagged_val_to_type(rec_box, result_ty);
            if !Self::is_union_type(result_ty) {
                // (B) Retain if this is a heap-pointer result type.
                if Self::result_is_heap_pointer(result_ty) {
                    if rec_unboxed.is_pointer_value() {
                        self.builder.call(self.rt.rc_retain, &[rec_unboxed.into()], "");
                    }
                }
                // Release the owned box (after the retain, the inner's RC is +2 if non-null; release
                // drops one via the box's release action → final +1 owned by the caller).
                self.builder.call(self.rt.tagged_release, &[rec_box.into()], "");
            }
            // (A) Union result: rec_box IS rec_unboxed; owned +1 handed to caller as-is.
            let rec_exit = self.builder.get_insert_block().unwrap();
            self.builder.unconditional_branch(final_mrg);
            // final_mrg: all paths have produced the same LLVM-type result. Phi to collect.
            self.builder.position_at_end(final_mrg);
            let result_llvm_ty = inner_unboxed.get_type();
            let final_phi = self.builder.phi(result_llvm_ty, "ir_idx_final");
            final_phi.add_incoming(&[(&inner_unboxed, inner_mrg_exit), (&rec_unboxed, rec_exit)]);
            return final_phi.as_basic_value();
        }
        // Stage 6b Phase 2: concrete open-object container is now a LinMap*; use map_get.
        let tagged = self.builder.call(self.rt.map_get, &[container.into(), key_str.into()], "ir_oget").try_as_basic_value().unwrap_basic();
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
    pub(crate) fn emit_map_set(&mut self, map_ptr: BasicValueEnum<'ctx>, key_ptr: BasicValueEnum<'ctx>, value: BasicValueEnum<'ctx>, val_ty: &Type, elem_ty: &Type, val_repr: &lin_ir::repr::Repr) {
        // KEEP-PACKED: when the value is a PACKED sealed array / sealed record (proven by the pass:
        // `val_repr` is `Packed(L)`), store it into the map slot by WRAPPING the still-packed pointer
        // in a 16-byte TaggedVal (`compile_ir_box_keep_packed`, TAG_ARRAY / TAG_OBJECT) — O(1), NO
        // `sealed_array_to_tagged` materialize (the O(n) copy that crashed on read-back). `lin_map_set`
        // copies the 16 bytes inline and retains the inner; the shell is freed after. The read-back
        // (`compile_ir_index` Map arm) unboxes it as a packed pointer (`compile_ir_unbox_keep_packed`)
        // feeding SealedArrayFieldGet zero-copy. Always sound: the runtime dispatches release/free on
        // the buffer's `elem_tag` / sealed header, regardless of being wrapped in a TaggedVal slot.
        // (Driven by these helper calls directly; the matching IR opcodes were never emitted and were
        // removed.)
        // UNBOXED SUM TYPE: a SumNode value stored into a `{ String: Expr }` map is MATERIALIZED to a
        // boxed `LinObject` (TAG_OBJECT) — the universal Json representation the map slot and the boxed
        // `Expr | Null` read-back expect. The materialized object is +1 owned; `lin_map_set` retains
        // the inner into the slot, so we release the transient box after.
        //
        // KEEP-PACKED-BY-POINTER for the Map value slot is DEFERRED (the TAG_SUMNODE runtime substrate
        // + codegen helpers are in place for it): the IR LOWERING `CloneBox`es the union-typed `m[k]`
        // result and the consumer's match-discriminator reads the boxed value via `object_get` / `is`
        // (`compile_ir_index` union arm) assuming a `LinObject` — a keep-packed `TAG_SUMNODE` slot read
        // by that borrowed-interior discriminator path is a type-confusion deref. Enabling it needs the
        // lowering/repr STEP-4 (suppress the project Coerce + teach the discriminator to materialize a
        // TAG_SUMNODE scrutinee), out of this change's scope.
        if value.is_pointer_value() {
            if let Some(sum_ty) = val_repr.sumnode_sum_ty() {
                let sum_ty = sum_ty.clone();
                let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                let obj = self.sumnode_materialize_to_object(value, &sum_ty, llvm_fn);
                let boxed = self.box_value(obj, &Self::sumnode_first_variant_obj_ty(&sum_ty));
                self.builder.call(self.rt.map_set, &[map_ptr.into(), key_ptr.into(), boxed.into()], "");
                if boxed.is_pointer_value() {
                    self.builder.call(self.rt.tagged_release, &[boxed.into()], "");
                }
                return;
            }
        }
        if value.is_pointer_value() {
            let packed_arr = val_repr.packed_sealed_array_layout().is_some();
            // ARRAYS ONLY. A packed sealed ARRAY is a real `LinArray` (elem_tag 0xFE) — every
            // dynamic consumer tag-dispatches on it, so wrapping the still-packed pointer is sound.
            // A bare packed sealed RECORD is NOT a `LinObject`: wrapping it in TAG_OBJECT type-
            // confuses every boxed read-back (`m[k]` is `T | Null`, a union — the general read path
            // hands the payload to `lin_object_get`, which reads sealed bytes as a LinObject header:
            // the index-cap underflow crash / silent corruption). Bare records fall through to the
            // GENERAL path below: `box_value` MATERIALIZES the sealed struct to a fresh boxed
            // `LinObject` (`sealed_materialize_to_object`), the slot owns the fresh box, and the
            // source struct stays owned by its scope — the IndexSet lowering's
            // `set_sealed_elem_into_tagged` carve-out already skips `transfer_into_container` for
            // exactly this materialize contract (same as the sealed-elem-into-tagged-array store).
            if packed_arr {
                // keep-packed store: wrap the still-packed pointer (O(1) — `lin_box_array`
                // stores the pointer verbatim, NO inner retain, NO `sealed_array_to_tagged` copy).
                // OWNERSHIP: the slot's single owning reference is supplied by the IR
                // `transfer_into_container` retain emitted in `IndexSet` lowering (identical to the
                // materialize path's contract). `lin_map_set` ALSO retains the inner into the slot, so
                // that DUPLICATE retain is undone by releasing the inner when we free the shell
                // (`lin_tagged_release` = drop inner + free shell). Net codegen effect on the inner is
                // ZERO (retain then release), leaving exactly the IR transfer's +1 as the slot's
                // reference — so the map drop's per-slot release frees it exactly once (ASan
                // detect_leaks verified). Mirrors `emit_object_set`'s fresh-box contract.
                let boxed = self.compile_ir_box_keep_packed(value, /*arr=*/true);
                self.builder.call(self.rt.map_set,
                    &[map_ptr.into(), key_ptr.into(), boxed.into()], "");
                if boxed.is_pointer_value() {
                    self.builder.call(self.rt.tagged_release, &[boxed.into()], "");
                }
                return;
            }
        }
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

    /// Store `value` into an Int-keyed map slot via `lin_map_set_int`.
    /// `map_ptr` is the raw `LinMap*`; `int_key` is an i64 LLVM value; `elem_ty` is the map's value type.
    /// Mirrors `emit_map_set` but calls the int-key runtime entry point.
    pub(crate) fn emit_map_set_int(&mut self, map_ptr: BasicValueEnum<'ctx>, int_key: BasicValueEnum<'ctx>, value: BasicValueEnum<'ctx>, val_ty: &Type, elem_ty: &Type, val_repr: &lin_ir::repr::Repr) {
        // Reuse the same value-boxing logic from emit_map_set, then call lin_map_set_int.
        let _ = val_repr; // repr handling same as String-map
        if Self::is_flat_scalar(elem_ty) {
            let coerced = if val_ty == elem_ty {
                value
            } else {
                self.compile_ir_coerce(value, val_ty, elem_ty)
            };
            let stack_tagged = self.build_tagged_val_alloca(&coerced, elem_ty);
            self.builder.call(self.rt.map_set_int,
                &[map_ptr.into(), int_key.into(), stack_tagged.into()], "");
            return;
        }
        let val_is_fresh_box = !Self::is_union_type(val_ty);
        let val_tagged = if val_is_fresh_box {
            self.box_value(value, val_ty)
        } else { value };
        self.builder.call(self.rt.map_set_int,
            &[map_ptr.into(), int_key.into(), val_tagged.into()], "");
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
        let i8_ty = self.context.i8_type();
        let void_ty = self.context.void_type();
        // A SEALED-repr record value (`{id:String, dep:Int32, …}`) being set into a TAGGED `Object[]`
        // is a PACKED struct pointer, NOT a boxed LinObject. `build_tagged_val_alloca` would tag it
        // TAG_OBJECT with the raw struct pointer as payload — the runtime then reads the packed bytes
        // as a LinObject header on read-back (heap-buffer-overflow / misaligned deref). Materialize it
        // to a fresh boxed LinObject first (its heap fields retained into the new object), then store
        // that pointer under TAG_OBJECT — the SAME representation `tagged_array_push_value` stores for
        // the `push$Object` case. The materialized object is a fresh +1 whose reference moves into the
        // array slot (`lin_array_set` raw-copies the 16-byte TaggedVal without an inner retain for a
        // tagged array), so it is NOT released here; the source struct keeps its own ownership (the IR
        // `ArraySetDyn` transfer leaves it owned, released at scope exit, dropping its heap fields).
        // Without this, `set(boxedSealedArr, i, {…})` crashed (ASan heap-buffer-overflow in
        // `lin_object`); the boxed-array set is the index-set analogue of the boxed-array push fix.
        if let Type::Object { .. } = val_ty {
            if let Some(fields) = Self::sealed_fields(val_ty).cloned() {
                let obj = self.sealed_materialize_to_map(value, &fields);
                let tag = i8_ty.const_int(Self::type_tag_open(val_ty) as u64, false);
                let cell = self.builder.alloca(self.context.struct_type(
                    &[i8_ty.into(), i8_ty.array_type(7).into(), i64_ty.into()], false), "set_tv");
                let tag_ptr = self.builder.struct_gep(
                    self.context.struct_type(&[i8_ty.into(), i8_ty.array_type(7).into(), i64_ty.into()], false),
                    cell, 0, "set_tv_tag");
                self.builder.store(tag_ptr, tag);
                let pay_ptr = self.builder.struct_gep(
                    self.context.struct_type(&[i8_ty.into(), i8_ty.array_type(7).into(), i64_ty.into()], false),
                    cell, 2, "set_tv_pay");
                let pay = self.builder.ptr_to_int(obj.into_pointer_value(), i64_ty, "set_tv_payi");
                self.builder.store(pay_ptr, pay);
                let set_fn = self.get_or_declare_fn("lin_array_set",
                    void_ty.fn_type(&[ptr_ty.into(), i64_ty.into(), ptr_ty.into()], false));
                self.builder.call(set_fn, &[arr_ptr.into(), idx_i64.into(), cell.into()], "");
                return;
            }
        }
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
    pub(crate) fn compile_ir_index_set(&mut self, obj: BasicValueEnum<'ctx>, key: BasicValueEnum<'ctx>, value: BasicValueEnum<'ctx>, obj_ty: &Type, key_ty: &Type, val_ty: &Type, val_repr: &lin_ir::repr::Repr) {
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
            // Stage 6b Phase 2: concrete open objects are LinMap* — use map_set. Named types
            // and sealed records come through here too but are handled below; unsealed Object
            // is the only case reaching this with a raw LinMap*.
            Type::Object { sealed: false, .. } => {
                if obj.is_pointer_value() && key.is_pointer_value() {
                    let key_str = resolve_obj_key(self, key);
                    // Use emit_map_set with a boxed-opaque repr; the concrete open object holds
                    // any-typed tagged values like a Json map.
                    self.emit_map_set(obj, key_str, value, val_ty, &Type::TypeVar(u32::MAX), &lin_ir::repr::Repr::boxed_opaque());
                }
            }
            Type::Object { .. } | Type::Named(_) => {
                if obj.is_pointer_value() && key.is_pointer_value() {
                    let key_str = resolve_obj_key(self, key);
                    self.emit_object_set(obj, key_str, value, val_ty);
                }
            }
            // Typed index-signature map `{ K: V }` (ADR-055 + numeric-key): O(1) hashed insert/overwrite.
            // Pass the map's value type `V` so a flat-scalar `V` is stored UNBOXED (inline in the
            // slot's TaggedVal, no heap box) and a narrower source value is widened to `V`.
            Type::Map { key: map_key_ty, value: elem } => {
                if map_key_ty.is_integer() {
                    // Int-keyed map: coerce key to i64 and call lin_map_set_int.
                    let i64_key = self.index_value_to_i64(key);
                    self.emit_map_set_int(obj, i64_key.into(), value, val_ty, elem, val_repr);
                } else if obj.is_pointer_value() && key.is_pointer_value() {
                    let key_str = resolve_obj_key(self, key);
                    self.emit_map_set(obj, key_str, value, val_ty, elem, val_repr);
                }
            }
            Type::Array(elem) => {
                let idx = self.index_value_to_i64(key);
                // Sealed-record array element (Stage 3): copy the element struct's payload into the
                // slot (`lin_sealed_array_set` releases the old element's heap fields and retains the
                // new ones; a scalar-only record is a straight overwrite). `value` is a sealed
                // struct ptr; it stays owned by its caller (released at scope exit).
                if let Some(elem_fields) = Self::sealed_array_elem(obj_ty) {
                    let elem_fields = elem_fields.clone();
                    let ptr_ty = self.context.ptr_type(AddressSpace::default());
                    let i64_ty = self.context.i64_type();
                    // Stage 1 pointer-backed array (0xFD): use `lin_sealed_ptr_array_set` which retains
                    // the new struct pointer and releases the old one. The RHS value must be a sealed
                    // struct pointer; project it if it isn't yet (e.g. a structural `{...}` literal in
                    // a callee context is an unsealed LinObject — project to a fresh sealed struct first).
                    let _ = val_ty;
                    // PART C (single-owner): the projection decision is read from the pass-computed
                    // representation of the RHS temp (`val_repr`), NOT a Type comparison. A verbatim
                    // pointer store is sound iff the RHS is ALREADY a packed sealed struct of the
                    // element's exact layout; anything else (boxed LinObject / unsealed `{...}`) is
                    // projected into a fresh sealed struct first. This replaces
                    // `sealed_repr_differs(val_ty, elem_ty)` with the dataflow fact.
                    let needs_proj = val_repr.packed_struct_fields() != Some(&elem_fields);
                    let (sealed_val, owned_here) = if needs_proj {
                        (self.sealed_project_from(value, val_ty, &elem_fields), true)
                    } else {
                        (value, false)
                    };
                    // `lin_sealed_ptr_array_set(arr, idx, sptr)` retains the new struct and releases
                    // the old slot. Borrowing semantics: the caller's ref is NOT consumed.
                    let set_fn = self.get_or_declare_fn("lin_sealed_ptr_array_set",
                        self.context.void_type().fn_type(&[ptr_ty.into(), i64_ty.into(), ptr_ty.into()], false));
                    self.builder.call(set_fn, &[obj.into(), idx.into(), sealed_val.into()], "");
                    if owned_here {
                        // We own the projected struct — release our ref (the set already retained its own).
                        self.emit_sealed_release(sealed_val, &elem_fields);
                    }
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
        // Find a Map{value:elem} variant in the union, if any.
        // For TypeVar (Json), ALWAYS emit a tag-dispatch because the container may be a LinMap
        // (TAG_MAP, from Phase 2 concrete open-object flip) or a LinObject (TAG_OBJECT).
        let is_json_dynamic = matches!(obj_ty, Type::TypeVar(_));
        let map_elem: Option<Type> = match obj_ty {
            Type::Union(vs) => vs.iter().find_map(|v| match v {
                Type::Map { value: e, .. } => Some((**e).clone()),
                _ => None,
            }),
            Type::Map { value: e, .. } => Some((**e).clone()),
            // For TypeVar (Json), use a dynamic-fallback elem type so the dispatch is emitted.
            Type::TypeVar(_) => Some(Type::TypeVar(u32::MAX)),
            _ => None,
        };
        let Some(elem) = map_elem else {
            self.emit_object_set(container, key_str, value, val_ty);
            return;
        };
        // For pure JSON (TypeVar), use the value's type as-is (opaque boxed).
        let elem = if is_json_dynamic { val_ty.clone() } else { elem };
        let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let i8t = self.context.i8_type();
        let tag = self.builder.call(self.rt.get_tag, &[boxed_obj.into()], "set_objtag").try_as_basic_value().unwrap_basic().into_int_value();
        let is_map = self.builder.int_compare(IntPredicate::EQ, tag, i8t.const_int(TAG_MAP as u64, false), "set_is_map");
        let map_b = self.context.append_basic_block(llvm_fn, "set_map");
        let obj_b = self.context.append_basic_block(llvm_fn, "set_obj");
        let mrg = self.context.append_basic_block(llvm_fn, "set_mrg");
        self.builder.conditional_branch(is_map, map_b, obj_b);
        self.builder.position_at_end(map_b);
        // In the union/nested-map dispatch the value arrives already boxed (a union TaggedVal*), so
        // there is no packed buffer to keep-pack — pass the fail-safe boxed repr.
        self.emit_map_set(container, key_str, value, val_ty, &elem, &lin_ir::repr::Repr::boxed_opaque());
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

    /// Emit (and cache) the static NAMED full-field descriptor global for a sealed record (ADR-063
    /// Stage 3b mechanism (i)) and return a pointer to it. UNLIKE `sealed_descriptor` (heap-only,
    /// nameless), this lists EVERY field — scalar and heap — with its NAME, struct-relative byte
    /// offset and `NKIND_*` code, so the runtime boxed reader (`lin_array_get_tagged`'s 0xFE branch)
    /// can materialize a keyed `LinObject` on demand. Format (PACKED, little-endian, byte-addressed —
    /// must match `lin_runtime::sealed::read_named_field`):
    /// ```text
    /// NamedDesc  = [ u32 field_count | NamedField * field_count ]
    /// NamedField = [ u32 offset | u32 nkind | u64 nested_named_desc_ptr | u16 name_len | name_bytes ]
    /// ```
    /// `nested_named_desc_ptr` is the nested record's NamedDesc (only for `NKIND_SEALED`; NULL else),
    /// making materialize recurse. Cached by the field (name, offset, kind) sequence so identical
    /// record types share one descriptor. Never NULL — a sealed array always carries a named desc.
    pub(crate) fn sealed_named_descriptor(&mut self, fields: &indexmap::IndexMap<String, Type>) -> inkwell::values::PointerValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i8_ty = self.context.i8_type();
        let i16_ty = self.context.i16_type();
        let i32_ty = self.context.i32_type();
        // Cache key: the per-field (name, offset, nkind) sequence.
        let key: String = format!(
            "__sealednameddesc_{}",
            fields.iter().map(|(k, fty)| {
                let (off, _) = Self::sealed_field_layout(fields, k);
                let nk = Self::sealed_named_field_kind(fty).unwrap_or(0);
                format!("{}@{}#{}", k, off, nk)
            }).collect::<Vec<_>>().join("__")
        );
        if let Some(g) = self.module.get_global(&key) {
            return g.as_pointer_value();
        }
        // Resolve nested named-desc pointers BEFORE adding this global (recursion). A directly
        // self-recursive record survives resolution as `Type::Named` (never an inlined sealed
        // `Object`), so `sealed_named_field_kind` returns None/Sealed only for an inlined sealed
        // field — the recursion terminates exactly as `sealed_fields` does.
        let mut nested_ptrs: Vec<inkwell::values::PointerValue<'ctx>> = Vec::with_capacity(fields.len());
        for fty in fields.values() {
            if Self::sealed_named_field_kind(fty) == Some(Self::NKIND_SEALED) {
                if let Some(nf) = Self::sealed_fields(fty) {
                    let nf = nf.clone();
                    nested_ptrs.push(self.sealed_named_descriptor(&nf));
                    continue;
                }
            }
            nested_ptrs.push(ptr_ty.const_null());
        }
        // Assemble the packed struct field-by-field so byte offsets are exact (no struct padding).
        let mut members: Vec<inkwell::values::BasicValueEnum<'ctx>> = Vec::new();
        let mut member_tys: Vec<inkwell::types::BasicTypeEnum<'ctx>> = Vec::new();
        // field_count (u32)
        members.push(i32_ty.const_int(fields.len() as u64, false).into());
        member_tys.push(i32_ty.into());
        for ((k, fty), nested) in fields.iter().zip(nested_ptrs.iter()) {
            let (off, _) = Self::sealed_field_layout(fields, k);
            let nkind = Self::sealed_named_field_kind(fty).unwrap_or(0);
            // u32 offset
            members.push(i32_ty.const_int(off, false).into());
            member_tys.push(i32_ty.into());
            // u32 nkind
            members.push(i32_ty.const_int(nkind as u64, false).into());
            member_tys.push(i32_ty.into());
            // u64 nested_named_desc_ptr (a real pointer global, or NULL)
            members.push((*nested).into());
            member_tys.push(ptr_ty.into());
            // u16 name_len
            members.push(i16_ty.const_int(k.len() as u64, false).into());
            member_tys.push(i16_ty.into());
            // name bytes as an [N x i8] array
            let bytes: Vec<inkwell::values::IntValue<'ctx>> =
                k.bytes().map(|b| i8_ty.const_int(b as u64, false)).collect();
            let name_arr = i8_ty.const_array(&bytes);
            member_tys.push(name_arr.get_type().into());
            members.push(name_arr.into());
        }
        let desc_ty = self.context.struct_type(&member_tys, true); // PACKED
        let desc_val = self.context.const_struct(&members, true);
        let global = self.module.add_global(desc_ty, None, &key);
        global.set_initializer(&desc_val);
        global.set_constant(true);
        global.as_pointer_value()
    }

    /// Emit (and cache) the static `SumDesc` global for a sum type and return a pointer to it (NULL
    /// pointer constant when NO variant has a heap/recursive field — a Stage-1 scalar-only sum type,
    /// whose drop is a pure refcount decrement + free). The descriptor is the variant-indexed
    /// heap-field table the runtime drop walk (`lin_sumnode_release`) reads:
    /// ```text
    /// SumDesc     = [ u32 variant_count | VariantDesc * variant_count ]
    /// VariantDesc = [ u32 heap_field_count | { u32 byte_offset, u32 kind } * heap_field_count ]
    /// ```
    /// Stage 2: the only heap fields are RECURSIVE children (`KIND_SUMNODE` = a `*SumNode` slot the
    /// drop walk recurses into). Variant order matches the union's declaration order (the inline tag
    /// indexes it). Cached by the sum type's `Debug` shape so identical sum types share one descriptor.
    pub(crate) fn sumnode_descriptor(&mut self, sum_ty: &Type) -> inkwell::values::PointerValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i32_ty = self.context.i32_type();
        let disc_key = match Self::sum_type_discriminant(sum_ty) {
            Some(k) => k,
            None => return ptr_ty.const_null(),
        };
        let self_name = Self::sum_recursive_self_name(sum_ty);
        let variants = match sum_ty {
            Type::Union(vs) => vs,
            _ => return ptr_ty.const_null(),
        };
        // Per-variant heap-field lists, in declaration (tag) order.
        let mut per_variant: Vec<Vec<(u64, u32)>> = Vec::with_capacity(variants.len());
        let mut any_heap = false;
        for v in variants {
            let mut heap: Vec<(u64, u32)> = Vec::new();
            if let Type::Object { fields, .. } = v {
                let payload = Self::sumnode_variant_payload_fields(fields, &disc_key);
                for (k, fty) in payload.iter() {
                    let is_recursive = self_name
                        .as_deref()
                        .is_some_and(|n| Self::is_sum_recursive_child(fty, n));
                    if is_recursive {
                        let offset = Self::sumnode_field_offset(&payload, k);
                        heap.push((offset, Self::KIND_SUMNODE));
                        any_heap = true;
                    } else if let Some(kind) = Self::sealed_field_kind(fty) {
                        // Heap-field SumNode Stage 3: String/Array/nested-sealed fields need a
                        // descriptor entry so the runtime drop walk releases them. Uses the same
                        // KIND_STRING/KIND_ARRAY/KIND_SEALED constants as sealed records.
                        let offset = Self::sumnode_field_offset(&payload, k);
                        heap.push((offset, kind));
                        any_heap = true;
                    }
                }
            }
            per_variant.push(heap);
        }
        let _ = any_heap;
        // SumDesc layout (keep-packed-through-record-fields extension): the descriptor now ALWAYS
        // begins with an 8-byte MATERIALIZER fn-ptr (`*SumNode -> *LinObject`, the per-type
        // `lin_summat_<key>`), so the runtime can materialize a kept-packed `TAG_SUMNODE` slot that
        // escaped a record field into the type-erased dynamic domain (toString/eq/json — where codegen
        // has lost the sum type). The heap-field drop table follows, its `variant_count` now read at
        // BYTE OFFSET 8 (after the ptr). The descriptor is ALWAYS emitted (non-null) — even a
        // scalar-only sum type needs the materializer ptr; its heap-field table is just all-empty
        // (every per-variant heap_count = 0), so the drop walk is still a no-op for it.
        //   SumDesc = [ u64 matfn_ptr | u32 variant_count | VariantDesc * variant_count ]
        //   VariantDesc = [ u32 heap_field_count | { u32 byte_offset, u32 kind } * heap_field_count ]
        // Build the trailing i32 table.
        let mut words: Vec<inkwell::values::IntValue<'ctx>> = Vec::new();
        words.push(i32_ty.const_int(per_variant.len() as u64, false)); // variant_count
        for heap in &per_variant {
            words.push(i32_ty.const_int(heap.len() as u64, false)); // heap_field_count
            for (off, kind) in heap {
                words.push(i32_ty.const_int(*off, false));
                words.push(i32_ty.const_int(*kind as u64, false));
            }
        }
        // Cache key: the full word sequence + the sum type's shape (the matfn is a deterministic
        // function of the shape, so two identical sum types share one descriptor + materializer).
        let key: String = format!(
            "__sumdesc_{:x}_{}",
            {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                format!("{sum_ty:?}").hash(&mut h);
                h.finish()
            },
            words.iter().map(|w| w.get_zero_extended_constant().unwrap_or(0).to_string()).collect::<Vec<_>>().join("_")
        );
        if let Some(g) = self.module.get_global(&key) {
            return g.as_pointer_value();
        }
        // Build the materializer fn-ptr FIRST (it positions the builder elsewhere; the descriptor is a
        // const global so insertion point does not matter, but the materializer must exist).
        let matfn = self.get_or_build_sumnode_materializer(sum_ty);
        let matfn_ptr = matfn.as_global_value().as_pointer_value();
        let arr_ty = i32_ty.array_type(words.len() as u32);
        let struct_ty = self.context.struct_type(&[ptr_ty.into(), arr_ty.into()], false);
        let arr = i32_ty.const_array(&words);
        let init = struct_ty.const_named_struct(&[matfn_ptr.into(), arr.into()]);
        let global = self.module.add_global(struct_ty, None, &key);
        global.set_initializer(&init);
        global.set_constant(true);
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

    /// `rec["field"] = value` over a PACKED SEALED RECORD: a constant-offset packed-struct store, the
    /// write counterpart of `sealed_field_get`. The object operand is a sealed-struct pointer (proven
    /// by the repr pass / verifier). For a SCALAR field this is a direct store (coercing a narrower/
    /// wider source to the field's width); for a HEAP field (String / Array / nested sealed) the old
    /// pointer is released and the new one retained, so the struct keeps exactly one +1 reference and
    /// the source value stays owned by its caller (released at scope exit) — the same retain semantics
    /// as a boxed `lin_object_set`.
    pub(crate) fn compile_ir_field_set(
        &mut self,
        obj: BasicValueEnum<'ctx>,
        field: &str,
        value: BasicValueEnum<'ctx>,
        obj_ty: &Type,
        val_ty: &Type,
        obj_repr: &lin_ir::repr::Repr,
    ) {
        // The fields come from the repr (the proven packed layout); fall back to the static type's
        // sealed fields if the repr did not carry them (should not happen — verifier asserts packed).
        let fields = obj_repr
            .packed_struct_fields()
            .cloned()
            .or_else(|| Self::sealed_scalar_fields(obj_ty).cloned());
        let Some(fields) = fields else { return; };
        if !obj.is_pointer_value() || !fields.contains_key(field) {
            return;
        }
        let (offset, _total) = Self::sealed_field_layout(&fields, field);
        let i64_ty = self.context.i64_type();
        let base = obj.into_pointer_value();
        let fld_ty = fields.get(field).cloned().unwrap_or(Type::Null);
        let p = unsafe {
            self.builder.gep(self.context.i8_type(), base, &[i64_ty.const_int(offset, false)], "sealed_set_fld_p")
        };
        let is_heap = Self::sealed_field_kind(&fld_ty).is_some();
        // Coerce a representation-mismatched source into the field's layout (a narrower/wider scalar,
        // or an unsealed `{...}` / Json projected into a nested sealed field). A repr-changing coerce
        // yields a FRESH +1 we then own (and must NOT additionally retain below).
        let repr_change = Self::sealed_repr_differs(val_ty, &fld_ty);
        let stored = if repr_change { self.compile_ir_coerce(value, val_ty, &fld_ty) } else { value };
        if is_heap {
            // Release the OLD heap pointer the slot held (balanced against its construction/prior-set
            // +1), then store and take a fresh +1 for the struct. A repr-changing coerce already
            // produced an owned +1, so only retain when the source was stored verbatim (borrowed).
            let old = self.builder.load(self.context.ptr_type(AddressSpace::default()), p, "sealed_set_old");
            self.emit_release(old, &fld_ty);
            self.builder.store(p, stored);
            if !repr_change && stored.is_pointer_value() {
                self.builder.call(self.rt.rc_retain, &[stored.into_pointer_value().into()], "sealed_set_retain");
            }
        } else {
            // Scalar field: a plain store (coerced to the field width above). No RC.
            self.builder.store(p, stored);
        }
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
        arr_repr: &lin_ir::repr::Repr,
    ) -> BasicValueEnum<'ctx> {
        let _ = arr_ty;
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        // STAGE 3: the packed-sealed-array ASSUME is read from the array operand's repr (proven by
        // the pass + verifier to carry a real `elem_tag==0xFE` packed buffer exactly where the old
        // `sealed_array_elem(arr_ty)` gate fired). Oracle-proven byte identical.
        let Some(fields) = arr_repr.packed_sealed_array_layout() else {
            return ptr_ty.const_null().into();
        };
        let fields = fields.clone();
        // A field NOT in the sealed element's shape is statically absent — `sealed_field_layout`
        // would assert. Follow safe-access (§6.1: missing key → Null) instead of panicking.
        if !fields.contains_key(field) {
            return self.null_value_for(result_ty);
        }
        let (field_off, _total) = Self::sealed_field_layout(&fields, field);
        // Stage 1 pointer-backed: use full struct-relative offset (includes SEALED_HEADER) —
        // the GEP goes into sptr which points at the struct base, not the payload.
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

        // len = *(u64*)(arr + 8); data = *(ptr*)(arr + 24)
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

        // Cold OOB path: defer to the runtime accessor (bounds-checked, faults) with the ORIGINAL
        // index for the correct fault message; it does not return. For pointer-backed arrays,
        // lin_sealed_ptr_array_get_ptr has the same signature and faulting behaviour.
        self.builder.position_at_end(oob_b);
        let elem_fn = self.get_or_declare_fn("lin_sealed_ptr_array_get_ptr",
            ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false));
        self.builder.call(elem_fn, &[arr_ptr.into(), idx.into()], "sarr_oob_call");
        self.builder.unreachable();

        // Fast path (Stage 1 pointer-backed): data = *(ptr*)(arr+24);
        //   sptr = *(ptr*)(data + actual*8);   // load the struct pointer from the 8-byte slot
        //   field_ptr = sptr + field_off;       // full struct-relative offset (includes SEALED_HEADER)
        self.builder.position_at_end(ok_b);
        let data_pp = unsafe { self.builder.gep(self.context.i8_type(), arr_ptr, &[i64_ty.const_int(24, false)], "sarr_data_pp") };
        let data = self.builder.load(ptr_ty, data_pp, "sarr_data").into_pointer_value();
        // Each slot is 8 bytes (pointer size); byte offset = actual * 8.
        let slot_off = self.builder.int_mul(actual, i64_ty.const_int(8, false), "sarr_slot_off");
        let slot_p = unsafe { self.builder.gep(self.context.i8_type(), data, &[slot_off], "sarr_slot_p") };
        let sptr = self.builder.load(ptr_ty, slot_p, "sarr_sptr").into_pointer_value();
        // field_off is the full struct-relative offset (SEALED_HEADER + payload offset).
        let fld_p = unsafe { self.builder.gep(self.context.i8_type(), sptr, &[i64_ty.const_int(field_off, false)], "sarr_fld_p") };
        let loaded = self.builder.load(llvm_fld, fld_p, "sarr_fld");
        if &fld_ty == result_ty { loaded } else { self.compile_ir_coerce(loaded, &fld_ty, result_ty) }
    }

    /// `arr[index][field]` for a BOXED `Object[]` whose element is a sealed/typed record stored as a
    /// heap `LinObject` (the boxed `Token[]` representation — a record with heap fields, NOT a packed
    /// sealed-scalar array). Reads the BORROWED element box via `lin_array_get` (a `*TaggedVal*`
    /// interior pointer — no fresh box alloc, no element release), unboxes to the raw `LinObject`,
    /// does the SINGLE `lin_object_get` for `field`, then unboxes/coerces to `result_ty`. The
    /// returned value is BORROWED interior storage; the lowerer's `BoxedArrayFieldGet` registers `dst`
    /// owned and emits the `Retain` for an RC `result_ty`, matching the materialize-then-read path
    /// this replaces. This skips the generic `arr[i]` sealed PROJECTION (alloc + read every field +
    /// per-field retain + reload + release) paid per access in a hot parser loop.
    pub(crate) fn compile_ir_boxed_array_field_get(
        &mut self,
        arr: BasicValueEnum<'ctx>,
        idx: BasicValueEnum<'ctx>,
        field: &str,
        arr_ty: &Type,
        result_ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let _ = arr_ty;
        if !arr.is_pointer_value() {
            return ptr_ty.const_null().into();
        }
        // The array operand is the boxed array. It may be a raw `LinArray*` (a typed array local) or
        // a boxed `Json` (TaggedVal*); array_get expects the raw `LinArray*`, so unbox a boxed array
        // first the same way the generic Index path does.
        let arr_raw = if Self::is_union_type(arr_ty) {
            self.builder.call(self.rt.unbox_ptr, &[arr.into()], "bafg_arr_unbox").try_as_basic_value().unwrap_basic()
        } else {
            arr
        };
        // idx → i64.
        let idx_i64 = if idx.is_int_value() {
            self.builder.int_s_extend_or_bit_cast(idx.into_int_value(), i64_ty, "bafg_idx")
        } else {
            self.unbox_value(idx, &Type::Int64).into_int_value()
        };
        // Borrowed element box (interior `*TaggedVal`); no fresh allocation, no release.
        let elem_box = self.builder.call(self.rt.array_get, &[arr_raw.into(), idx_i64.into()], "bafg_elem")
            .try_as_basic_value().unwrap_basic();
        // Phase 2: array elements may be TAG_MAP (codegen-produced open objects, Phase 2) or
        // TAG_OBJECT (runtime/legacy producers, Track B not yet done). Tag-dispatch to handle both.
        let inner_obj = self.builder.call(self.rt.unbox_ptr, &[elem_box.into()], "bafg_obj")
            .try_as_basic_value().unwrap_basic();
        let key_str = self.compile_string_lit(field).into_pointer_value();
        let i8_ty = self.context.i8_type();
        let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let elem_tag = self.builder.call(self.rt.get_tag, &[elem_box.into()], "bafg_tag")
            .try_as_basic_value().unwrap_basic().into_int_value();
        let is_map_tag = self.builder.int_compare(
            inkwell::IntPredicate::EQ, elem_tag,
            i8_ty.const_int(lin_common::tags::TAG_MAP as u64, false), "bafg_is_map");
        let map_bb = self.context.append_basic_block(llvm_fn, "bafg_map");
        let obj_bb = self.context.append_basic_block(llvm_fn, "bafg_obj");
        let merge_bb = self.context.append_basic_block(llvm_fn, "bafg_merge");
        self.builder.conditional_branch(is_map_tag, map_bb, obj_bb);
        // TAG_MAP branch: use map_get
        self.builder.position_at_end(map_bb);
        let map_tagged = self.builder.call(self.rt.map_get, &[inner_obj.into(), key_str.into()], "bafg_mget")
            .try_as_basic_value().unwrap_basic();
        let map_pred = self.builder.get_insert_block().unwrap();
        self.builder.unconditional_branch(merge_bb);
        // TAG_OBJECT branch: use object_get (legacy runtime producers)
        self.builder.position_at_end(obj_bb);
        let obj_tagged = self.builder.call(self.rt.object_get, &[inner_obj.into(), key_str.into()], "bafg_oget")
            .try_as_basic_value().unwrap_basic();
        let obj_pred = self.builder.get_insert_block().unwrap();
        self.builder.unconditional_branch(merge_bb);
        // merge
        self.builder.position_at_end(merge_bb);
        let tagged_phi = self.builder.phi(ptr_ty, "bafg_phi");
        tagged_phi.add_incoming(&[(&map_tagged, map_pred), (&obj_tagged, obj_pred)]);
        let tagged = tagged_phi.as_basic_value();
        // Coerce the (borrowed interior) field value to the field's declared type. The lowerer
        // registers `dst` owned and retains for an RC result, so this borrowed read is balanced.
        self.unbox_tagged_val_to_type(tagged, result_ty)
    }

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
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
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
        let idx_slot = self.builder.alloca(i64_ty, "nestarr_i");
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
        // wildcard makes `sealed_array_project_from` / `sealed_project_from` unbox the element first.
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
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
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
        let idx_slot = self.builder.alloca(i64_ty, "fwiden_i");
        self.builder.store(idx_slot, i64_ty.const_zero());
        self.builder.unconditional_branch(head);
        self.builder.position_at_end(head);
        let i = self.builder.load(i64_ty, idx_slot, "fwiden_iv").into_int_value();
        let cond = self.builder.int_compare(IntPredicate::SLT, i, len, "fwiden_cond");
        self.builder.conditional_branch(cond, body, done);
        self.builder.position_at_end(body);
        // Read the element at the SOURCE element type (its native stride), convert to the DEST
        // scalar (numeric widen/convert), and push at the DEST stride.
        let elem = self.flat_array_get(src_raw, i, from_elem);
        let conv = self.compile_ir_coerce(elem, from_elem, to_elem);
        self.flat_array_push(out, conv, to_elem);
        let next = self.builder.int_add(i, i64_ty.const_int(1, false), "fwiden_next");
        self.builder.store(idx_slot, next);
        self.builder.unconditional_branch(head);
        self.builder.position_at_end(done);
        out
    }

    pub(crate) fn sealed_array_project_from(&mut self, src: BasicValueEnum<'ctx>, src_ty: &Type, arr_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        if Self::sealed_array_elem(arr_ty).is_none() {
            return ptr_ty.const_null().into();
        }
        // Unbox the source to a raw LinArray* if it is a boxed Json/union value.
        let src_raw = if Self::is_union_type(src_ty) {
            self.builder.call(self.rt.unbox_ptr, &[src.into()], "sarrp_unbox").try_as_basic_value().unwrap_basic()
        } else { src };
        // KEEP-PACKED fast path (repr pass, Stage 4): if the unboxed source is ALREADY a packed
        // 0xFE buffer (a boxed sealed array stored keep-packed, e.g. a Map slot read-back or a
        // narrowing of `T[]|Null`), there is NO representation change — clone it BY POINTER (retain
        // the existing 0xFE buffer, O(1)) instead of rebuilding element-wise through the boxed
        // `Object[]` machinery (which would mis-read the inline scalar bytes → UAF). Dispatch on the
        // runtime `elem_tag` (byte 4): 0xFE ⇒ keep-packed; otherwise (a genuinely-boxed `Object[]`,
        // e.g. a `fromJson` result) fall through to the element rebuild below.
        {
            let i8_ty = self.context.i8_type();
            let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
            let tag_ptr = unsafe {
                self.builder.gep(i8_ty, src_raw.into_pointer_value(), &[i64_ty.const_int(4, false)], "sarrp_tagp")
            };
            let etag = self.builder.load(i8_ty, tag_ptr, "sarrp_etag").into_int_value();
            // Accept both 0xFE (inline packed) and 0xFD (pointer-backed sealed) as already-packed.
            let is_fe = self.builder.int_compare(IntPredicate::EQ, etag, i8_ty.const_int(0xFE, false), "sarrp_isfe");
            let is_fd = self.builder.int_compare(IntPredicate::EQ, etag, i8_ty.const_int(0xFD, false), "sarrp_isfd");
            let is_packed = self.builder.or(is_fe, is_fd, "sarrp_ispk");
            let kp_b = self.context.append_basic_block(llvm_fn, "sarrp_kp");
            let rebuild_b = self.context.append_basic_block(llvm_fn, "sarrp_rebuild");
            let merge_b = self.context.append_basic_block(llvm_fn, "sarrp_merge");
            self.builder.conditional_branch(is_packed, kp_b, rebuild_b);
            // Keep-packed: the unboxed 0xFE buffer is the SAME object the boxed source (`src`) holds a
            // reference to. The projection here is a non-mutating BORROW that aliases the source's
            // existing reference (the caller releases `src`/the cloned union box at the projection's
            // scope, which drops the inner). So return the buffer VERBATIM with NO extra retain —
            // adding one would out-balance the single source release and leak the buffer (the map
            // value would never reach rc 0). This mirrors the union/Json `obj[k]` projection borrow.
            self.builder.position_at_end(kp_b);
            let kp_exit = self.builder.get_insert_block().unwrap();
            self.builder.unconditional_branch(merge_b);
            self.builder.position_at_end(rebuild_b);
            // Fall through to the element-rebuild path; capture its result and join at merge.
            let rebuilt = self.sealed_array_rebuild_from_boxed(src_raw, arr_ty);
            let rebuild_exit = self.builder.get_insert_block().unwrap();
            self.builder.unconditional_branch(merge_b);
            self.builder.position_at_end(merge_b);
            let phi = self.builder.phi(ptr_ty, "sarrp_phi");
            phi.add_incoming(&[(&src_raw, kp_exit), (&rebuilt, rebuild_exit)]);
            return phi.as_basic_value();
        }
    }

    /// Like `sealed_array_project_from`, but ALWAYS returns a FRESH +1-OWNED packed buffer (the caller
    /// transfers ownership, e.g. into a sealed struct slot it owns). The difference is the keep-packed
    /// branch: `sealed_array_project_from` BORROWS the source's existing reference (no retain), which
    /// is correct for a non-owning consumer (a match/coerce that releases the source itself). But when
    /// a SEALED-RECORD field stores a nested packed `T[]` (`Trip { stopTimes: StopTime[] }`), the
    /// struct OWNS its field and releases it on drop — so the value MUST be +1. Here the keep-packed
    /// branch RETAINS the aliased buffer; the rebuild branch is already +1. Either way the result is a
    /// fresh +1 the struct construction can store verbatim (`already_owned = true`).
    pub(crate) fn sealed_array_project_owned(&mut self, src: BasicValueEnum<'ctx>, src_ty: &Type, arr_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        if Self::sealed_array_elem(arr_ty).is_none() {
            return ptr_ty.const_null().into();
        }
        let src_raw = if Self::is_union_type(src_ty) {
            self.builder.call(self.rt.unbox_ptr, &[src.into()], "sarrpo_unbox").try_as_basic_value().unwrap_basic()
        } else { src };
        let i8_ty = self.context.i8_type();
        let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let tag_ptr = unsafe {
            self.builder.gep(i8_ty, src_raw.into_pointer_value(), &[i64_ty.const_int(4, false)], "sarrpo_tagp")
        };
        let etag = self.builder.load(i8_ty, tag_ptr, "sarrpo_etag").into_int_value();
        // Accept BOTH 0xFE (old inline-packed) and 0xFD (Stage-1 pointer-backed) as "already sealed".
        // A 0xFD array is shared-ownership: retain transfers the caller's +1 into the struct slot.
        // A 0xFE array is similarly: retain the buffer pointer for the struct slot.
        // Only a genuinely-tagged 0xFF array (or other) needs a rebuild.
        let is_fe = self.builder.int_compare(IntPredicate::EQ, etag, i8_ty.const_int(0xFE, false), "sarrpo_isfe");
        let is_fd = self.builder.int_compare(IntPredicate::EQ, etag, i8_ty.const_int(0xFD, false), "sarrpo_isfd");
        let is_packed = self.builder.or(is_fe, is_fd, "sarrpo_ispk");
        let kp_b = self.context.append_basic_block(llvm_fn, "sarrpo_kp");
        let rebuild_b = self.context.append_basic_block(llvm_fn, "sarrpo_rebuild");
        let merge_b = self.context.append_basic_block(llvm_fn, "sarrpo_merge");
        self.builder.conditional_branch(is_packed, kp_b, rebuild_b);
        // Keep-packed: the unboxed 0xFE/0xFD buffer is shared with the source. To TRANSFER a +1 into
        // the owning struct, RETAIN it (so the struct's later release is balanced against the source's
        // own release). This is the ownership difference from `sealed_array_project_from`.
        self.builder.position_at_end(kp_b);
        self.builder.call(self.rt.rc_retain, &[src_raw.into_pointer_value().into()], "sarrpo_kp_retain");
        let kp_exit = self.builder.get_insert_block().unwrap();
        self.builder.unconditional_branch(merge_b);
        // Rebuild: a genuinely-boxed `Object[]` (e.g. a Json literal field) → element-wise rebuild into
        // a fresh +1 pointer-backed (0xFD) buffer.
        self.builder.position_at_end(rebuild_b);
        let rebuilt = self.sealed_array_rebuild_from_boxed(src_raw, arr_ty);
        let rebuild_exit = self.builder.get_insert_block().unwrap();
        self.builder.unconditional_branch(merge_b);
        self.builder.position_at_end(merge_b);
        let phi = self.builder.phi(ptr_ty, "sarrpo_phi");
        phi.add_incoming(&[(&src_raw, kp_exit), (&rebuilt, rebuild_exit)]);
        phi.as_basic_value()
    }

    /// Element-by-element rebuild of a sealed-record array from a genuinely-boxed `Object[]` source
    /// (each element a boxed `LinObject` projected into the sealed element layout). The cold path of
    /// `sealed_array_project_from` — used only when the source is NOT already a sealed 0xFE/0xFD
    /// buffer (e.g. a `fromJson` result or a tagged literal). Split out so the keep-packed fast path
    /// is the common case. Stage 1: output is a 0xFD pointer-backed array.
    pub(crate) fn sealed_array_rebuild_from_boxed(&mut self, src_raw: BasicValueEnum<'ctx>, arr_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let fields = match Self::sealed_array_elem(arr_ty) {
            Some(f) => f.clone(),
            None => return ptr_ty.const_null().into(),
        };
        let named_desc = self.sealed_named_descriptor(&fields);
        // len = lin_array_length(src_raw)
        let len_fn = self.get_or_declare_fn("lin_array_length", i64_ty.fn_type(&[ptr_ty.into()], false));
        let len = self.builder.call(len_fn, &[src_raw.into()], "sarrp_len").try_as_basic_value().unwrap_basic().into_int_value();
        // Stage 1: out = lin_sealed_ptr_array_alloc(len, named_desc) — produces a 0xFD pointer-backed array.
        let alloc_fn = self.get_or_declare_fn("lin_sealed_ptr_array_alloc",
            ptr_ty.fn_type(&[i64_ty.into(), ptr_ty.into()], false));
        let out = self.builder.call(alloc_fn, &[len.into(), named_desc.into()], "sarrp_out")
            .try_as_basic_value().unwrap_basic();
        // Push each projected struct via lin_sealed_ptr_array_push (retains the struct pointer).
        let push_fn = self.get_or_declare_fn("lin_sealed_ptr_array_push",
            self.context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
        let get_tagged = self.get_or_declare_fn("lin_array_get_tagged", ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false));
        // Loop i in [0, len): proj = project(boxed src[i]); push proj (retaining the struct pointer).
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
        // push retains the struct (+1), so struct rc: 1 (from sealed_project_from) → 2 after push.
        self.builder.call(push_fn, &[out.into(), proj.into()], "");
        // Release our alloc ref to the projected struct (push holds it now).
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
    /// Convert a sealed-record array to a tagged `Object[]` at the Json boundary. For Stage 1
    /// pointer-backed arrays (0xFD), calls `lin_sealed_ptr_array_to_tagged` which materializes each
    /// struct pointer via the named descriptor. Used where the dynamic reader can't process struct
    /// pointers directly. The returned tagged array is fresh +1 owned.
    pub(crate) fn sealed_array_to_tagged(&mut self, arr: BasicValueEnum<'ctx>, arr_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        if Self::sealed_array_elem(arr_ty).is_none() {
            return arr;
        }
        // Stage 1 pointer-backed: use lin_sealed_ptr_array_to_tagged (materializes via named desc).
        let to_tagged = self.get_or_declare_fn("lin_sealed_ptr_array_to_tagged",
            ptr_ty.fn_type(&[ptr_ty.into()], false));
        self.builder.call(to_tagged, &[arr.into()], "sarr_tagged")
            .try_as_basic_value().unwrap_basic()
    }

    /// Load element `idx` of a pointer-backed sealed-record array as an owned (+1) sealed struct
    /// pointer. For Stage 1 pointer-backed arrays (0xFD), `arr[i]` loads the 8-byte struct pointer
    /// from the data buffer and retains it (+1 rc). The caller receives an independent +1 ownership
    /// of the same struct the array holds. Used for `arr[i]` as a whole value (the fused
    /// `arr[i].field` read goes through `compile_ir_sealed_array_field_get` instead).
    pub(crate) fn sealed_array_materialize_elem(
        &mut self,
        arr: BasicValueEnum<'ctx>,
        idx: inkwell::values::IntValue<'ctx>,
        _fields: &indexmap::IndexMap<String, Type>,
    ) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        // Load the struct pointer from `data + idx*8` (bounds-checked via lin_sealed_ptr_array_get_ptr).
        let get_fn = self.get_or_declare_fn("lin_sealed_ptr_array_get_ptr",
            ptr_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false));
        let sptr = self.builder.call(get_fn, &[arr.into(), idx.into()], "sarr_mat")
            .try_as_basic_value().unwrap_basic();
        // Retain: the caller takes +1 ownership (the array keeps its own +1).
        self.builder.call(self.rt.rc_retain, &[sptr.into()], "");
        sptr
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

        // Header: rc @ 0 = IMMORTAL_RC, size @ 4 = total, desc_ptr @ 8 = NULL,
        // named_desc_ptr @ 16 = static named descriptor (Stage 6a: needed so lin_box_record on
        // this stack-alloc produces a TAG_RECORD that lin_record_get_field can service).
        let rc_p = unsafe { self.builder.gep(i8_ty, obj, &[i64_ty.const_int(0, false)], "sealed_stk_rc") };
        self.builder.store(rc_p, i32_ty.const_int(Self::SEALED_IMMORTAL_RC as u64, false));
        let sz_p = unsafe { self.builder.gep(i8_ty, obj, &[i64_ty.const_int(4, false)], "sealed_stk_sz") };
        self.builder.store(sz_p, i32_ty.const_int(total, false));
        let desc_p = unsafe { self.builder.gep(i8_ty, obj, &[i64_ty.const_int(8, false)], "sealed_stk_desc") };
        self.builder.store(desc_p, self.context.ptr_type(AddressSpace::default()).const_null());
        // Stage 6a: write the named descriptor at header offset 16. Without this write,
        // `lin_box_record` called on this stack frame produces a TAG_RECORD box whose
        // `lin_record_get_field` reads garbage at offset 16 → UAF/crash.
        let named_desc = self.sealed_named_descriptor(fields);
        let named_desc_p = unsafe { self.builder.gep(i8_ty, obj, &[i64_ty.const_int(16, false)], "sealed_stk_nmd") };
        self.builder.store(named_desc_p, named_desc);

        // Fields: all scalars, stored inline by offset. No coerce (scalar field type == value type
        // under this gate), no retain. Skip any width-subtyping EXTRA field not in the declared
        // sealed shape (see `sealed_construct`).
        for (name, val, _val_ty, _already_owned) in field_vals {
            if !fields.contains_key(name) {
                continue;
            }
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
        // Stage 6a: pass the NAMED descriptor as the 3rd arg so the sealed struct stores it at
        // header offset 16 (`SEALED_HEADER` = 24). This enables TAG_RECORD field access via
        // `lin_record_get_field` — it reads named_desc from offset 16 to look up field names + offsets.
        let named_desc = self.sealed_named_descriptor(fields);
        let obj = self.builder.call(self.rt.sealed_alloc, &[i64_ty.const_int(total, false).into(), desc.into(), named_desc.into()], "sealed_obj")
            .try_as_basic_value().unwrap_basic().into_pointer_value();
        for (name, val, val_ty, already_owned) in field_vals {
            // A literal may carry EXTRA fields beyond the declared sealed shape (width subtyping:
            // `val p: Pt = { x, y, extra }` is well-typed; extras are dropped on assignment). The
            // packed sealed layout has slots ONLY for the declared fields, so skip any field_vals
            // entry not in `fields` — otherwise `sealed_field_layout` asserts "field not in record".
            // (The value is still lowered by the caller; here we simply don't store it.)
            if !fields.contains_key(name) {
                continue;
            }
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
            // A representation-changing coerce USUALLY produces a FRESH +1-owned value — a sealed
            // record/array PROJECTION (allocates), a flat-array WIDEN (fresh buffer), or a Json→Map
            // MATERIALIZE (fresh LinMap) — so the struct can store it verbatim and own it. BUT the
            // union/Json → concrete-HEAP unbox (a `String`, a BOXED `Object[]` — e.g. `StopTime[]`,
            // whose String fields keep it boxed — or a non-sealed object field) routes through
            // `unbox_tagged_val_to_type` → `lin_unbox_ptr`, which returns the source box's interior
            // pointer BORROWED, with NO new reference. Treating that as owned (the old `owned =
            // repr_change`) skipped the retain, so when the lowerer releases the source box at scope
            // exit the field's buffer is freed and the struct dangles — the `Trip { stopTimes:
            // StopTime[] }`-built-from-a-`Json`-array use-after-free (a packed `Trip[]` then read
            // garbage lengths / corrupted data). Detect that borrowing path and fall through to the
            // retain below so the struct takes its own +1. Sealed-record / packed-sealed-array / Map
            // fields are EXCLUDED (their coerce genuinely allocates a fresh +1) — unchanged, so those
            // paths stay byte-identical.
            let coerce_borrowed = repr_change
                && Self::is_union_type(val_ty)
                && Self::sealed_fields(&fld_ty).is_none()
                && Self::sealed_array_elem(&fld_ty).is_none()
                && !matches!(fld_ty, Type::Map { .. });
            let owned = (*already_owned || repr_change) && !coerce_borrowed;
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
    /// pointer is BORROWED (the struct still owns its original +1); after `map_set` retains
    /// the inner (map +1), only the box SHELL is freed (`lin_tagged_free_box`) — NOT
    /// `lin_tagged_release`, which would also drop the inner and leave the map holding a pointer
    /// it never accounted for (a use-after-free once the struct releases). The struct keeps its
    /// reference; the materialized map owns an independent +1. Both balanced.
    /// Stage 6b Phase 2: builds a `LinMap*` (was `LinObject*`).
    pub(crate) fn sealed_materialize_to_map(
        &mut self,
        obj: BasicValueEnum<'ctx>,
        fields: &indexmap::IndexMap<String, Type>,
    ) -> BasicValueEnum<'ctx> {
        let i32_ty = self.context.i32_type();
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let new_map = self.builder.call(self.rt.map_alloc,
            &[i32_ty.const_int(fields.len() as u64, false).into(), i32_ty.const_zero().into()],
            "sealed_mat")
            .try_as_basic_value().unwrap_basic().into_pointer_value();
        let keys: Vec<String> = fields.keys().cloned().collect();
        let free_box_shell = self.get_or_declare_fn("lin_tagged_free_box", self.context.void_type().fn_type(&[ptr_ty.into()], false));
        for k in &keys {
            let fld_ty = fields.get(k).cloned().unwrap_or(Type::Null);
            let is_heap = Self::sealed_field_kind(&fld_ty).is_some();
            let v = self.sealed_field_get(obj, k, fields, &fld_ty);
            // box_value(heap) wraps the BORROWED pointer (no retain); box_value(scalar) wraps the
            // scalar (cached/heap box). For a nested sealed field, box_value materializes it to its
            // own boxed LinMap (a fresh +1), which map_set then retains — handled below.
            let boxed = self.box_value(v, &fld_ty);
            let key_str = self.compile_string_lit(k).into_pointer_value();
            self.builder.call(self.rt.map_set, &[new_map.into(), key_str.into(), boxed.into()], "");
            if boxed.is_pointer_value() {
                if is_heap && Self::box_value_yields_fresh_owned(&fld_ty) {
                    // box_value produced a FRESH +1 value (a nested SEALED record materialized to a
                    // boxed LinMap, OR a sealed-record ARRAY materialized to a fresh tagged
                    // `Object[]` via `sealed_array_to_tagged`). map_set retained it (+2 on
                    // the fresh inner); full tagged_release drops the construction +1 back to the
                    // map's owned +1 AND frees the box shell. Using `free_box_shell` here would
                    // leak the entire materialized inner (its header + nested elements) — the
                    // record-with-record-array-field leak (the RAPTOR `Trip { stopTimes: StopTime[] }`
                    // shape) every build/push/index-set/map dropped ~176 B/element.
                    self.builder.call(self.rt.tagged_release, &[boxed.into()], "");
                } else if is_heap {
                    // A plain String / plain (non-sealed-elem) Array field's box wraps a BORROWED
                    // inner pointer (the struct still owns its original +1) that map_set
                    // retained — free ONLY the shell so the borrowed inner is not dropped.
                    self.builder.call(free_box_shell, &[boxed.into()], "");
                } else {
                    // Scalar: no inner heap — full release reclaims the (cache-safe) box shell.
                    self.builder.call(self.rt.tagged_release, &[boxed.into()], "");
                }
            }
        }
        new_map.into()
    }

    /// Project a source value (`src`, statically `src_ty`) into a FRESH sealed scalar record of
    /// `target_fields`. THE central boundary op. Non-mutating: `src` is untouched (its own owner
    /// releases it), extras are ignored, and the result is an independent +1 struct. The source
    /// is read by whatever representation it has:
    ///   - another sealed scalar record → field copy by offset;
    ///   - a boxed `LinObject` / Json TaggedVal → `lin_object_get` per target field, unbox, store.
    /// Emit the sealed-projection field loop reading from a raw `container` (`LinObject*` or
    /// `LinMap*`) via `getter` (`lin_object_get` / `lin_map_get`, both `(container, key) -> BORROWED
    /// TaggedVal*`). Per-field RC is the borrowed-interior model (`already_owned=false` ⇒
    /// `sealed_construct` retains heap fields); returns the fresh +1 sealed struct. The CALLER owns
    /// `container` and releases it after (the borrowed reads are consumed by `sealed_construct` first).
    fn emit_sealed_proj_loop(
        &mut self,
        container: BasicValueEnum<'ctx>,
        getter: inkwell::values::FunctionValue<'ctx>,
        target_fields: &indexmap::IndexMap<String, Type>,
    ) -> BasicValueEnum<'ctx> {
        let mut vals: Vec<(String, BasicValueEnum<'ctx>, Type, bool)> =
            Vec::with_capacity(target_fields.len());
        for (k, fty0) in target_fields.iter() {
            let fty = fty0.clone();
            let key_str = self.compile_string_lit(k).into_pointer_value();
            let tagged = self.builder.call(getter, &[container.into(), key_str.into()], "sealed_proj_get")
                .try_as_basic_value().unwrap_basic();
            if Self::sealed_array_elem(&fty).is_some() {
                let packed = self.sealed_array_project_owned(tagged, &Type::TypeVar(u32::MAX), &fty);
                vals.push((k.clone(), packed, fty, true));
                continue;
            }
            let v = self.unbox_tagged_val_to_type(tagged, &fty);
            let owned = matches!(fty, Type::Object { .. }) && Self::sealed_fields(&fty).is_some();
            vals.push((k.clone(), v, fty, owned));
        }
        self.sealed_construct(target_fields, &vals)
    }

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
        // Source is a boxed object / Json.
        let target_keys: Vec<String> = target_fields.keys().cloned().collect();
        let mut vals: Vec<(String, BasicValueEnum<'ctx>, Type, bool)> = Vec::with_capacity(target_keys.len());

        if Self::is_union_type(src_ty) {
            // All union sources (TAG_MAP / TAG_RECORD / TAG_OBJECT) are normalised to a LinMap*
            // by lin_union_force_to_map: TAG_MAP → O(1) retain; TAG_RECORD → materialise; TAG_OBJECT → copy.
            // Phase 3: TAG_RECORD materialises to TAG_MAP, so the old TAG_OBJECT branch is gone.
            let cmap = self.builder.call(self.rt.map_force, &[src.into()], "sproj_cmap")
                .try_as_basic_value().unwrap_basic();
            let result = self.emit_sealed_proj_loop(cmap, self.rt.map_get, target_fields);
            self.builder.call(self.rt.map_release, &[cmap.into()], "");
            return result;
        }

        // Non-union source: a concrete raw `LinMap*`. Borrowed per-field reads, no copy, no release.
        let container = src;
        for k in &target_keys {
            let fty = target_fields.get(k).cloned().unwrap_or(Type::Null);
            let key_str = self.compile_string_lit(k).into_pointer_value();
            let tagged = self.builder.call(self.rt.map_get, &[container.into(), key_str.into()], "sealed_proj_get").try_as_basic_value().unwrap_basic();
            if Self::sealed_array_elem(&fty).is_some() {
                let packed = self.sealed_array_project_owned(tagged, &Type::TypeVar(u32::MAX), &fty);
                vals.push((k.clone(), packed, fty, true));
                continue;
            }
            let v = self.unbox_tagged_val_to_type(tagged, &fty);
            let owned = matches!(fty, Type::Object { .. }) && Self::sealed_fields(&fty).is_some();
            vals.push((k.clone(), v, fty, owned));
        }
        self.sealed_construct(target_fields, &vals)
    }

    /// Project a WIDER `LinMap*` into a FRESH `LinMap*` with exactly `target_fields`.
    /// D3b: the boxed→boxed-narrower slot boundary (anon-struct widening severs sharing).
    /// Non-mutating: `src` is untouched (its owner releases it); extra fields are
    /// ignored; the result is a fresh +1 independent owned `LinMap*`.
    ///
    /// RC contract per field:
    /// - `lin_map_get` returns a BORROWED interior `TaggedVal*` pointer (do NOT release).
    /// - `lin_map_set` copies the 16-byte `TaggedVal` and RETAINS the inner payload (+1).
    /// - No extra per-field cleanup is needed; `map_set` handles retention.
    /// Stage 6b Phase 2: was `boxed_object_project` building a `LinObject*`.
    pub(crate) fn boxed_object_project(
        &mut self,
        src: BasicValueEnum<'ctx>,
        target_fields: &indexmap::IndexMap<String, Type>,
    ) -> BasicValueEnum<'ctx> {
        let i32_ty = self.context.i32_type();
        let new_map = self.builder.call(
            self.rt.map_alloc,
            &[i32_ty.const_int(target_fields.len() as u64, false).into(), i32_ty.const_zero().into()],
            "anon_proj_map",
        ).try_as_basic_value().unwrap_basic().into_pointer_value();
        for k in target_fields.keys() {
            let key_str = self.compile_string_lit(k).into_pointer_value();
            // Borrow the entry from the source map (interior ptr, never release).
            let entry = self.builder.call(
                self.rt.map_get,
                &[src.into(), key_str.into()],
                "anon_proj_get",
            ).try_as_basic_value().unwrap_basic();
            // map_set copies the TaggedVal and retains the inner.
            self.builder.call(
                self.rt.map_set,
                &[new_map.into(), key_str.into(), entry.into()],
                "",
            );
        }
        new_map.into()
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

    /// The Null value in the representation `result_ty` expects. A union/Json slot holds a boxed
    /// TaggedVal*, so emit a boxed null (`lin_box_null`); any other (concrete, incl. `Type::Null`)
    /// slot is a raw null pointer — identical to how a `Const::Null` literal is materialized. Used
    /// by the sealed-record field-access paths to yield the safe-access missing-key → Null result
    /// without panicking, mirroring the boxed `lin_object_get` missing-key path.
    fn null_value_for(&mut self, result_ty: &Type) -> BasicValueEnum<'ctx> {
        if Self::is_union_type(result_ty) {
            self.builder.call(self.rt.box_null, &[], "sealed_absent_null").try_as_basic_value().unwrap_basic()
        } else {
            self.context.ptr_type(AddressSpace::default()).const_null().into()
        }
    }

    // ── Unboxed tagged sum type (`SumNode`) — unboxed-sumtype Stage 1 ─────────────────────────────

    /// Construct a `SumNode` for the variant whose discriminant value is `disc`. Allocates a
    /// max-variant-sized node (`lin_sumnode_alloc`, desc NULL — Stage 1 scalar-only), stores the
    /// dense variant tag inline at offset 16, then stores each scalar payload field by offset. The
    /// discriminant field itself is NOT stored (it is the inline tag). `field_vals` are the literal's
    /// (name, value, value_ty) — including the discriminant, which is skipped. Returns a +1 node.
    pub(crate) fn sumnode_construct(
        &mut self,
        sum_ty: &Type,
        disc: &str,
        field_vals: &[(String, BasicValueEnum<'ctx>, Type)],
    ) -> BasicValueEnum<'ctx> {
        let total = Self::sumnode_total_size(sum_ty);
        let tag = Self::sumnode_variant_tag(sum_ty, disc).expect("sumnode_construct: unknown variant");
        let payload_fields = Self::sumnode_variant_by_disc(sum_ty, disc)
            .expect("sumnode_construct: unknown variant payload");
        let disc_key = Self::sum_type_discriminant(sum_ty).expect("sumnode_construct: not a sum type");
        let self_name = Self::sum_recursive_self_name(sum_ty);
        let i64_ty = self.context.i64_type();
        let i32_ty = self.context.i32_type();
        let i8_ty = self.context.i8_type();
        // The SumDesc (NULL for a scalar-only sum type — the runtime drop is then a pure rc dec+free;
        // non-NULL for a recursive sum type so the drop walk recurses into `*SumNode` children).
        let desc = self.sumnode_descriptor(sum_ty);
        let node = self
            .builder
            .call(self.rt.sumnode_alloc, &[i64_ty.const_int(total, false).into(), desc.into()], "sumnode")
            .try_as_basic_value()
            .unwrap_basic()
            .into_pointer_value();
        // Inline tag @ 16.
        let tag_p = unsafe { self.builder.gep(i8_ty, node, &[i64_ty.const_int(Self::SUMNODE_TAG_OFFSET, false)], "sumnode_tag_p") };
        self.builder.store(tag_p, i32_ty.const_int(tag as u64, false));
        // Payload fields by offset (skip the discriminant — carried by the tag).
        for (name, val, val_ty) in field_vals {
            if name == &disc_key {
                continue;
            }
            let Some(fld_ty) = payload_fields.get(name).cloned() else { continue };
            let offset = Self::sumnode_field_offset(&payload_fields, name);
            let p = unsafe { self.builder.gep(i8_ty, node, &[i64_ty.const_int(offset, false)], "sumnode_set_p") };
            let is_recursive = self_name
                .as_deref()
                .is_some_and(|n| Self::is_sum_recursive_child(&fld_ty, n));
            if is_recursive {
                // A recursive child is an owned `*SumNode` slot: the node owns +1 of the child. The
                // lowerer TRANSFERS the child's existing +1 into the node (it `unregister_owned`s a
                // fresh-alloc child literal, or `Retain`s a borrowed child) — exactly the
                // `transfer_into_container` discipline — so codegen must NOT add another retain here
                // (that would double-count the child, never balanced by the single drop-walk release).
                self.builder.store(p, *val);
            } else if Self::sealed_field_kind(&fld_ty).is_some() && !Self::is_sum_scalar_field(&fld_ty) {
                // Heap field (String/Array/nested-sealed) Stage 3: store the heap pointer and RETAIN
                // so the node owns an independent +1. The caller's temp (the field expression's result)
                // retains its own +1 which scope-exit releases; the node takes a second +1 here that
                // the descriptor's drop walk releases when the node is freed. This mirrors
                // sealed_construct's unconditional retain for non-owned fields. We always retain (no
                // `already_owned` flag in the SumNode construct API) — the heap-field temp is NEVER
                // transferred into the SumNode by the lowerer (only recursive children are).
                let stored = if val_ty == &fld_ty { *val } else { self.compile_ir_coerce(*val, val_ty, &fld_ty) };
                self.builder.store(p, stored);
                if stored.is_pointer_value() {
                    self.builder.call(self.rt.rc_retain, &[stored.into_pointer_value().into()], "sumnode_heap_fld_retain");
                }
            } else {
                // Scalar field: reconcile a wider/narrower numeric literal into the stored width.
                let stored = if val_ty == &fld_ty { *val } else { self.compile_ir_coerce(*val, val_ty, &fld_ty) };
                self.builder.store(p, stored);
            }
        }
        node.into()
    }

    /// Load the inline discriminant tag (u32 @ offset 16) of a `SumNode`.
    pub(crate) fn sumnode_tag_load(&mut self, node: BasicValueEnum<'ctx>) -> inkwell::values::IntValue<'ctx> {
        let i64_ty = self.context.i64_type();
        let i32_ty = self.context.i32_type();
        let i8_ty = self.context.i8_type();
        let base = node.into_pointer_value();
        let p = unsafe { self.builder.gep(i8_ty, base, &[i64_ty.const_int(Self::SUMNODE_TAG_OFFSET, false)], "sumnode_tag_p") };
        self.builder.load(i32_ty, p, "sumnode_tag").into_int_value()
    }

    /// Read a RECURSIVE CHILD field of a `SumNode` by constant offset (unboxed-sumtype Stage 2),
    /// yielding the BORROWED interior `*SumNode` (the parent still owns it). The offset is resolved by
    /// field NAME: the recursive child field appears in exactly the variant(s) that declare it, all at
    /// the same payload offset (the access is only reachable when the tag selects such a variant). The
    /// field is loaded as a raw pointer; its repr is Packed(SumNode) of the child sum type, so a
    /// chained `evalNode(node["left"])` re-enters the tag switch. The lowerer applies retain-on-escape.
    /// Read a field of a `SumNode` by NAME, dispatching on whether the field is a recursive child
    /// (`*SumNode` pointer slot → borrowed interior pointer) or a scalar payload (const-offset value
    /// load). The variant carrying `field` is resolved by name (consistent offset across the
    /// variant(s) declaring it). Used by `compile_ir_field_get` for a narrowed-variant SumNode whose
    /// recursive children block the sealed-projection path, so EVERY field read goes direct to the
    /// node. A field absent from every variant → the Null value for `result_ty` (safe-access rule).
    pub(crate) fn sumnode_field_get_by_name(
        &mut self,
        node: BasicValueEnum<'ctx>,
        field: &str,
        sum_ty: &Type,
        result_ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        let disc_key = match Self::sum_type_discriminant(sum_ty) {
            Some(k) => k,
            None => return self.null_value_for(result_ty),
        };
        let self_name = Self::sum_recursive_self_name(sum_ty);
        let variants = match sum_ty {
            Type::Union(vs) => vs,
            _ => return self.null_value_for(result_ty),
        };
        // Find the variant payload that declares `field`, and whether it is a recursive child.
        for v in variants {
            if let Type::Object { fields, .. } = v {
                let payload = Self::sumnode_variant_payload_fields(fields, &disc_key);
                if let Some(fty) = payload.get(field) {
                    let is_recursive = self_name
                        .as_deref()
                        .is_some_and(|n| Self::is_sum_recursive_child(fty, n));
                    if is_recursive {
                        return self.sumnode_recursive_child_get(node, field, sum_ty);
                    }
                    // Scalar payload field: const-offset value load via the variant payload layout.
                    return self.sumnode_field_get(node, field, &payload, result_ty);
                }
            }
        }
        // Field not in any variant payload (e.g. the discriminant, or a statically-absent key) → Null.
        self.null_value_for(result_ty)
    }

    pub(crate) fn sumnode_recursive_child_get(
        &mut self,
        node: BasicValueEnum<'ctx>,
        field: &str,
        sum_ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let disc_key = match Self::sum_type_discriminant(sum_ty) {
            Some(k) => k,
            None => return ptr_ty.const_null().into(),
        };
        let self_name = Self::sum_recursive_self_name(sum_ty);
        let variants = match sum_ty {
            Type::Union(vs) => vs,
            _ => return ptr_ty.const_null().into(),
        };
        // Find the (unique) offset of `field` as a recursive child across variants that declare it.
        let mut offset: Option<u64> = None;
        for v in variants {
            if let Type::Object { fields, .. } = v {
                let payload = Self::sumnode_variant_payload_fields(fields, &disc_key);
                if let Some(fty) = payload.get(field) {
                    let is_recursive = self_name
                        .as_deref()
                        .is_some_and(|n| Self::is_sum_recursive_child(fty, n));
                    if is_recursive {
                        let off = Self::sumnode_field_offset(&payload, field);
                        match offset {
                            Some(prev) if prev != off => {
                                // Ambiguous (the field is a recursive child at DIFFERENT offsets in
                                // different variants). Defensive: not reachable for the in-scope
                                // direct-self-recursive sum types (children share a consistent layout
                                // position). Return Null rather than read a wrong offset.
                                return ptr_ty.const_null().into();
                            }
                            _ => offset = Some(off),
                        }
                    }
                }
            }
        }
        let Some(offset) = offset else {
            return ptr_ty.const_null().into();
        };
        let i64_ty = self.context.i64_type();
        let i8_ty = self.context.i8_type();
        let base = node.into_pointer_value();
        let p = unsafe { self.builder.gep(i8_ty, base, &[i64_ty.const_int(offset, false)], "sumnode_child_p") };
        self.builder.load(ptr_ty, p, "sumnode_child")
    }

    /// Read a SCALAR payload field of a `SumNode` by constant offset (the value's variant is known —
    /// from a narrowed match arm — so the payload field offset is statically resolvable). For the
    /// discriminant field, materialize the variant's StrLit (the tag identifies it). `variant_payload`
    /// is the narrowed variant's payload field map.
    pub(crate) fn sumnode_field_get(
        &mut self,
        node: BasicValueEnum<'ctx>,
        field: &str,
        variant_payload: &indexmap::IndexMap<String, Type>,
        result_ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        let Some(fld_ty) = variant_payload.get(field).cloned() else {
            // Field not in this variant's payload (e.g. the discriminant, or an absent key) → Null.
            return self.null_value_for(result_ty);
        };
        let offset = Self::sumnode_field_offset(variant_payload, field);
        let i64_ty = self.context.i64_type();
        let i8_ty = self.context.i8_type();
        let base = node.into_pointer_value();
        let llvm_fld = self.llvm_type(&fld_ty);
        let p = unsafe { self.builder.gep(i8_ty, base, &[i64_ty.const_int(offset, false)], "sumnode_fld_p") };
        let loaded = self.builder.load(llvm_fld, p, "sumnode_fld");
        if &fld_ty == result_ty { loaded } else { self.compile_ir_coerce(loaded, &fld_ty, result_ty) }
    }

    /// Materialize a `SumNode` into a fresh `LinMap*` (Phase 2: open-object/TAG_MAP backing) for
    /// a dynamic edge (toString / Json-serialize / keys / spread / `==` vs a non-sum value / FFI /
    /// transfer). Returns a +1 `LinMap*`. `sum_ty` is the static sum type.
    ///
    /// Emits a per-variant switch (each variant materialises its own concrete shape), merging the
    /// resulting `LinMap*` at a phi.
    pub(crate) fn sumnode_materialize_to_map(
        &mut self,
        node: BasicValueEnum<'ctx>,
        sum_ty: &Type,
        _llvm_fn: inkwell::values::FunctionValue<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        // A recursive sum type can NOT be materialized by inlining the per-variant switch (a recursive
        // child would inline another full switch → infinite codegen). Build (once) a memoized
        // per-sum-type materializer FUNCTION that calls ITSELF for recursive children (runtime
        // recursion, terminating), and call it. A non-recursive sum type uses the same function (no
        // self-call) — uniform and still O(1)-ish per node.
        let func = self.get_or_build_sumnode_materializer(sum_ty);
        self.builder
            .call(func, &[node.into()], "sumnode_mat")
            .try_as_basic_value()
            .unwrap_basic()
    }

    /// Backwards-compat alias.
    #[allow(dead_code)]
    pub(crate) fn sumnode_materialize_to_object(
        &mut self,
        node: BasicValueEnum<'ctx>,
        sum_ty: &Type,
        llvm_fn: inkwell::values::FunctionValue<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        self.sumnode_materialize_to_map(node, sum_ty, llvm_fn)
    }

    /// Build (and memoize) the per-sum-type materializer `lin_summat_<key>(node: ptr) -> ptr`: it
    /// reads the node's inline tag, switches to the matching variant, and builds a fresh `LinMap`
    /// with the discriminant StrLit + each payload field. A SCALAR field is boxed directly; a
    /// RECURSIVE CHILD (`*SumNode`) is materialized by a RECURSIVE CALL to this same function
    /// (so the whole tree serialises). Returns a +1 `LinMap*`. Children are BORROWED (read by
    /// const offset, never released here); the per-field box shells are reclaimed after `map_set`
    /// retains. Memoized by the sum type's shape so it is emitted once.
    fn get_or_build_sumnode_materializer(&mut self, sum_ty: &Type) -> inkwell::values::FunctionValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i32_ty = self.context.i32_type();
        let disc_key = Self::sum_type_discriminant(sum_ty).expect("sumnode_materialize: not a sum type");
        let self_name = Self::sum_recursive_self_name(sum_ty);
        let key = format!("lin_summat_{:x}", {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            format!("{sum_ty:?}").hash(&mut h);
            h.finish()
        });
        if let Some(f) = self.module.get_function(&key) {
            return f;
        }
        let variants = match sum_ty {
            Type::Union(vs) => vs.clone(),
            _ => unreachable!(),
        };
        let saved_block = self.builder.get_insert_block();
        let fn_ty = ptr_ty.fn_type(&[ptr_ty.into()], false);
        let func = self.module.add_function(&key, fn_ty, None);
        let entry = self.context.append_basic_block(func, "entry");
        self.builder.position_at_end(entry);
        let node: BasicValueEnum<'ctx> = func.get_nth_param(0).unwrap();
        let free_box_shell = self.get_or_declare_fn("lin_tagged_free_box", self.context.void_type().fn_type(&[ptr_ty.into()], false));
        let mut variant_bodies: Vec<(u32, indexmap::IndexMap<String, Type>, String)> = Vec::new();
        for (i, v) in variants.iter().enumerate() {
            if let Type::Object { fields, .. } = v {
                let disc_val = match fields.get(&disc_key) {
                    Some(Type::StrLit(s)) => s.clone(),
                    _ => continue,
                };
                let payload = Self::sumnode_variant_payload_fields(fields, &disc_key);
                variant_bodies.push((i as u32, payload, disc_val));
            }
        }
        let merge_bb = self.context.append_basic_block(func, "sumnode_mat_merge");
        let default_bb = self.context.append_basic_block(func, "sumnode_mat_default");
        let blocks: Vec<inkwell::basic_block::BasicBlock<'ctx>> = variant_bodies
            .iter()
            .map(|_| self.context.append_basic_block(func, "sumnode_mat_arm"))
            .collect();
        let tag = self.sumnode_tag_load(node);
        let cases: Vec<(inkwell::values::IntValue<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> = variant_bodies
            .iter()
            .enumerate()
            .map(|(idx, (tagv, _, _))| (i32_ty.const_int(*tagv as u64, false), blocks[idx]))
            .collect();
        self.builder.switch(tag, default_bb, &cases);
        let mut incoming: Vec<(inkwell::values::PointerValue<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> = Vec::new();
        // Default arm: defensive empty map (the tag is always valid).
        self.builder.position_at_end(default_bb);
        let def_obj = self.builder.call(self.rt.map_alloc, &[i32_ty.const_int(0, false).into(), i32_ty.const_zero().into()], "sumnode_mat_def").try_as_basic_value().unwrap_basic().into_pointer_value();
        let def_pred = self.builder.get_insert_block().unwrap();
        self.builder.unconditional_branch(merge_bb);
        incoming.push((def_obj, def_pred));
        for (idx, (_tagv, payload, disc_val)) in variant_bodies.iter().enumerate() {
            self.builder.position_at_end(blocks[idx]);
            let nfields = (payload.len() + 1) as u64;
            let obj = self.builder.call(self.rt.map_alloc, &[i32_ty.const_int(nfields, false).into(), i32_ty.const_zero().into()], "sumnode_mat_obj").try_as_basic_value().unwrap_basic().into_pointer_value();
            let dk = self.compile_string_lit(&disc_key).into_pointer_value();
            let dv_raw = self.compile_string_lit(disc_val);
            let dv_box = self.box_value(dv_raw, &Type::Str);
            self.builder.call(self.rt.map_set, &[obj.into(), dk.into(), dv_box.into()], "");
            if dv_box.is_pointer_value() {
                self.builder.call(free_box_shell, &[dv_box.into()], "");
            }
            for (k, fty) in payload.iter() {
                let key_str = self.compile_string_lit(k).into_pointer_value();
                let is_recursive = self_name
                    .as_deref()
                    .is_some_and(|n| Self::is_sum_recursive_child(fty, n));
                if is_recursive {
                    // Recursive child: read the borrowed `*SumNode`, materialize it via a SELF-CALL,
                    // box as TAG_MAP. Child is borrowed — not released; the materialized map is
                    // a fresh +1 that `map_set` retains, so we release our copy after.
                    let child = self.sumnode_recursive_child_get(node, k, sum_ty);
                    let child_obj = self.builder.call(func, &[child.into()], "sumnode_mat_child").try_as_basic_value().unwrap_basic();
                    let boxed = self.box_value(child_obj, &Self::sumnode_first_variant_obj_ty(sum_ty));
                    self.builder.call(self.rt.map_set, &[obj.into(), key_str.into(), boxed.into()], "");
                    if boxed.is_pointer_value() {
                        self.builder.call(self.rt.tagged_release, &[boxed.into()], "");
                    }
                    continue;
                }
                // Heap-field SumNode Stage 3: a heap field (String/Array/nested-sealed) is stored
                // as an owned interior pointer in the node. Read it directly (BORROWED — the node
                // still owns it), box it, set it in the map (map_set retains), then
                // release our fresh-box shell. The node remains the owner; the map takes its own
                // reference via map_set's retain.
                if Self::sealed_field_kind(fty).is_some() && !Self::is_sum_scalar_field(fty) {
                    let v = self.sumnode_field_get(node, k, payload, fty);
                    let boxed = self.box_value(v, fty);
                    self.builder.call(self.rt.map_set, &[obj.into(), key_str.into(), boxed.into()], "");
                    if boxed.is_pointer_value() {
                        self.builder.call(self.rt.tagged_release, &[boxed.into()], "");
                    }
                    continue;
                }
                let v = self.sumnode_field_get(node, k, payload, fty);
                let boxed = self.box_value(v, fty);
                self.builder.call(self.rt.map_set, &[obj.into(), key_str.into(), boxed.into()], "");
                if boxed.is_pointer_value() {
                    self.builder.call(self.rt.tagged_release, &[boxed.into()], "");
                }
            }
            let pred = self.builder.get_insert_block().unwrap();
            self.builder.unconditional_branch(merge_bb);
            incoming.push((obj, pred));
        }
        self.builder.position_at_end(merge_bb);
        let phi = self.builder.phi(ptr_ty, "sumnode_mat_phi");
        let refs: Vec<(&dyn inkwell::values::BasicValue<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> =
            incoming.iter().map(|(v, b)| (v as &dyn inkwell::values::BasicValue<'ctx>, *b)).collect();
        phi.add_incoming(&refs);
        self.builder.r#return(Some(&phi.as_basic_value()));
        // Restore the caller's insertion point.
        if let Some(b) = saved_block {
            self.builder.position_at_end(b);
        }
        func
    }

    /// Project a `SumNode` source into a FRESH sealed-record struct of `target_fields` (the matched
    /// variant record, inside a narrowed `match` arm). The variant is known statically from
    /// `target_fields`' discriminant value. Each scalar payload field is copied from the node by
    /// const offset; the discriminant field (a StrLit) is materialized as the interned literal. Used
    /// by `compile_ir_coerce` for a sum→variant-record Coerce (the arm-entry narrowing). Non-mutating:
    /// the source SumNode keeps its own ownership. Returns a +1 sealed struct.
    pub(crate) fn sumnode_project_to_sealed(
        &mut self,
        node: BasicValueEnum<'ctx>,
        sum_ty: &Type,
        target_fields: &indexmap::IndexMap<String, Type>,
    ) -> BasicValueEnum<'ctx> {
        let disc_key = Self::sum_type_discriminant(sum_ty).expect("sumnode_project: not a sum type");
        // The target variant's discriminant value (its StrLit in target_fields).
        let disc_val = match target_fields.get(&disc_key) {
            Some(Type::StrLit(s)) => s.clone(),
            _ => {
                // Target is not a single concrete variant — cannot project; return null (defensive).
                return self.context.ptr_type(AddressSpace::default()).const_null().into();
            }
        };
        let payload = Self::sumnode_variant_by_disc(sum_ty, &disc_val).unwrap_or_default();
        let vals: Vec<(String, BasicValueEnum<'ctx>, Type, bool)> = target_fields
            .iter()
            .map(|(k, fty)| {
                if k == &disc_key {
                    // discriminant StrLit → interned immortal LinString (already-owned, no retain).
                    let s = self.compile_string_lit(&disc_val);
                    (k.clone(), s, fty.clone(), true)
                } else {
                    let v = self.sumnode_field_get(node, k, &payload, fty);
                    (k.clone(), v, fty.clone(), false)
                }
            })
            .collect();
        self.sealed_construct(target_fields, &vals)
    }

    /// Reconstruct a fresh `SumNode` from a BOXED object / Json source (`src`, statically `src_ty`):
    /// the reverse boundary (a Json value coerced into a sum type, e.g. `fromJson`). Reads the
    /// discriminant key, switches on its string value to the matching variant, and builds that
    /// variant's node by reading each scalar payload field with `lin_object_get`+unbox. Returns a +1
    /// SumNode. Defensive default arm builds the first variant's node (the checker guarantees a valid
    /// discriminant for a well-typed coercion).
    pub(crate) fn sumnode_project_from_boxed(
        &mut self,
        src: BasicValueEnum<'ctx>,
        src_ty: &Type,
        sum_ty: &Type,
        _llvm_fn: inkwell::values::FunctionValue<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        // KEEP-PACKED-THROUGH-RECORD-FIELDS fast path: when `src` is a BOXED union/Json value (so it
        // is a `TaggedVal*`), its payload MAY be a keep-packed `*SumNode` (TAG_SUMNODE — the cursor
        // zero-copy store) rather than a materialized `LinObject` (TAG_OBJECT). Tag-dispatch: a
        // TAG_SUMNODE box's payload IS already the projected node, so just unwrap it + retain (zero
        // copy); any other tag → unbox to the LinObject and run the per-type projector (rebuild). This
        // centralizes the keep-packed read-back so EVERY caller (the arg/slot Coerce, `unbox_tagged_
        // val_to_type`, Index) gets it. Only when `src_ty` is a union is `src` a box we may probe; a
        // non-union `src` is a raw `LinObject*` (e.g. a match-narrowed, already-unboxed scrutinee) whose
        // first bytes are NOT a tag — never probe it, project directly. The runtime-tag dispatch is the
        // soundness mechanism: no static store/read agreement is required.
        if Self::is_union_type(src_ty) && src.is_pointer_value() {
            let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
            let i8_ty = self.context.i8_type();
            let tag = self.builder.call(self.rt.get_tag, &[src.into()], "pfb_tag").try_as_basic_value().unwrap_basic().into_int_value();
            let is_kp = self.builder.int_compare(
                inkwell::IntPredicate::EQ, tag, i8_ty.const_int(lin_common::tags::TAG_SUMNODE as u64, false), "pfb_is_kp");
            let kp_bb = self.context.append_basic_block(llvm_fn, "pfb_kp");
            let proj_bb = self.context.append_basic_block(llvm_fn, "pfb_proj");
            let merge_bb = self.context.append_basic_block(llvm_fn, "pfb_merge");
            self.builder.conditional_branch(is_kp, kp_bb, proj_bb);
            // KEEP-PACKED: payload IS the *SumNode — unwrap + retain (fresh +1 owner).
            self.builder.position_at_end(kp_bb);
            let node = self.builder.call(self.rt.unbox_ptr, &[src.into()], "pfb_kp_node").try_as_basic_value().unwrap_basic();
            if node.is_pointer_value() {
                self.builder.call(self.rt.rc_retain, &[node.into()], "");
            }
            let kp_pred = self.builder.get_insert_block().unwrap();
            self.builder.unconditional_branch(merge_bb);
            // MATERIALIZED (TAG_OBJECT) box: unbox to the LinObject and run the projector.
            self.builder.position_at_end(proj_bb);
            let container = self.builder.call(self.rt.unbox_ptr, &[src.into()], "sumnode_pfb_unbox").try_as_basic_value().unwrap_basic();
            let func = self.get_or_build_sumnode_projector(sum_ty);
            let proj = self.builder.call(func, &[container.into()], "sumnode_pfb").try_as_basic_value().unwrap_basic();
            let proj_pred = self.builder.get_insert_block().unwrap();
            self.builder.unconditional_branch(merge_bb);
            self.builder.position_at_end(merge_bb);
            let phi = self.builder.phi(ptr_ty, "pfb_phi");
            phi.add_incoming(&[(&node, kp_pred), (&proj, proj_pred)]);
            return phi.as_basic_value();
        }
        // Non-union `src`: a raw `LinObject*` (or already-unboxed) — project directly, no tag probe.
        let func = self.get_or_build_sumnode_projector(sum_ty);
        self.builder
            .call(func, &[src.into()], "sumnode_pfb")
            .try_as_basic_value()
            .unwrap_basic()
    }

    /// Build (and memoize) the per-sum-type projector `lin_sumproj_<key>(boxed_obj: ptr) -> *SumNode`:
    /// reads the boxed `LinObject`'s discriminant string, switches to the matching variant, reads each
    /// scalar payload field (+unbox) and each RECURSIVE child (via a SELF-CALL on the child's boxed
    /// object), and `sumnode_construct`s the +1 node. The reverse of the materializer. Memoized by the
    /// sum type's shape so it is emitted once and the recursion terminates at runtime.
    fn get_or_build_sumnode_projector(&mut self, sum_ty: &Type) -> inkwell::values::FunctionValue<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let disc_key = Self::sum_type_discriminant(sum_ty).expect("project_from_boxed: not a sum type");
        let self_name = Self::sum_recursive_self_name(sum_ty);
        let key = format!("lin_sumproj_{:x}", {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            format!("{sum_ty:?}").hash(&mut h);
            h.finish()
        });
        if let Some(f) = self.module.get_function(&key) {
            return f;
        }
        let variants = match sum_ty {
            Type::Union(vs) => vs.clone(),
            _ => unreachable!(),
        };
        let saved_block = self.builder.get_insert_block();
        let fn_ty = ptr_ty.fn_type(&[ptr_ty.into()], false);
        let func = self.module.add_function(&key, fn_ty, None);
        let entry = self.context.append_basic_block(func, "entry");
        self.builder.position_at_end(entry);
        let container: BasicValueEnum<'ctx> = func.get_nth_param(0).unwrap();
        // Read the discriminant string value (boxed) for the switch.
        let dk = self.compile_string_lit(&disc_key).into_pointer_value();
        // Phase 2: container is now LinMap* (materializer returns LinMap*). Use map_get.
        let disc_box = self.builder.call(self.rt.map_get, &[container.into(), dk.into()], "sumnode_pfb_disc").try_as_basic_value().unwrap_basic();
        let mut variant_info: Vec<(String, indexmap::IndexMap<String, Type>)> = Vec::new();
        for v in &variants {
            if let Type::Object { fields, .. } = v {
                if let Some(Type::StrLit(s)) = fields.get(&disc_key) {
                    variant_info.push((s.clone(), Self::sumnode_variant_payload_fields(fields, &disc_key)));
                }
            }
        }
        let merge_bb = self.context.append_basic_block(func, "sumnode_pfb_merge");
        let mut incoming: Vec<(inkwell::values::PointerValue<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> = Vec::new();
        let n = variant_info.len();
        for (idx, (disc_val, payload)) in variant_info.iter().enumerate() {
            let arm_bb = self.context.append_basic_block(func, "sumnode_pfb_arm");
            let next_bb = if idx + 1 < n {
                self.context.append_basic_block(func, "sumnode_pfb_next")
            } else {
                arm_bb
            };
            if idx + 1 < n {
                let lit_raw = self.compile_string_lit(disc_val);
                let lit = self.box_value(lit_raw, &Type::Str);
                let eq_fn = self.get_or_declare_fn("lin_tagged_eq", self.context.i8_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                let eq_i8 = self.builder.call(eq_fn, &[disc_box.into(), lit.into()], "sumnode_pfb_eq").try_as_basic_value().unwrap_basic().into_int_value();
                let eq = self.builder.int_truncate_or_bit_cast(eq_i8, self.context.bool_type(), "sumnode_pfb_eqb");
                if lit.is_pointer_value() {
                    let free_box_shell = self.get_or_declare_fn("lin_tagged_free_box", self.context.void_type().fn_type(&[ptr_ty.into()], false));
                    self.builder.call(free_box_shell, &[lit.into()], "");
                }
                self.builder.conditional_branch(eq, arm_bb, next_bb);
            } else {
                self.builder.unconditional_branch(arm_bb);
            }
            self.builder.position_at_end(arm_bb);
            let field_vals: Vec<(String, BasicValueEnum<'ctx>, Type)> = {
                let mut v = Vec::new();
                v.push((disc_key.clone(), self.compile_string_lit(disc_val), Type::StrLit(disc_val.clone())));
                for (k, fty) in payload.iter() {
                    let key_str = self.compile_string_lit(k).into_pointer_value();
                    // Phase 2: container is LinMap*. Use map_get.
                    let tagged = self.builder.call(self.rt.map_get, &[container.into(), key_str.into()], "sumnode_pfb_get").try_as_basic_value().unwrap_basic();
                    // A RECURSIVE CHILD field (typed `Named(self)`) is projected into a fresh `*SumNode`
                    // of the SAME sum type via a SELF-CALL on the child's boxed object; `sumnode_construct`
                    // stores it as the owned recursive child pointer. A scalar field is unboxed.
                    let is_recursive = self_name
                        .as_deref()
                        .is_some_and(|n| Self::is_sum_recursive_child(fty, n));
                    let val = if is_recursive {
                        // The child slot in the boxed object is a nested boxed LinObject (TAG_OBJECT).
                        let child_obj = self.builder.call(self.rt.unbox_ptr, &[tagged.into()], "sumnode_pfb_child_unbox").try_as_basic_value().unwrap_basic();
                        self.builder.call(func, &[child_obj.into()], "sumnode_pfb_child").try_as_basic_value().unwrap_basic()
                    } else {
                        self.unbox_tagged_val_to_type(tagged, fty)
                    };
                    v.push((k.clone(), val, fty.clone()));
                }
                v
            };
            let node = self.sumnode_construct(sum_ty, disc_val, &field_vals);
            let pred = self.builder.get_insert_block().unwrap();
            self.builder.unconditional_branch(merge_bb);
            incoming.push((node.into_pointer_value(), pred));
            if idx + 1 < n {
                self.builder.position_at_end(next_bb);
            }
        }
        self.builder.position_at_end(merge_bb);
        let phi = self.builder.phi(ptr_ty, "sumnode_pfb_phi");
        let refs: Vec<(&dyn inkwell::values::BasicValue<'ctx>, inkwell::basic_block::BasicBlock<'ctx>)> =
            incoming.iter().map(|(v, b)| (v as &dyn inkwell::values::BasicValue<'ctx>, *b)).collect();
        phi.add_incoming(&refs);
        self.builder.r#return(Some(&phi.as_basic_value()));
        if let Some(b) = saved_block {
            self.builder.position_at_end(b);
        }
        func
    }

    pub(crate) fn compile_ir_field_get(&mut self, obj: BasicValueEnum<'ctx>, field: &str, obj_ty: &Type, result_ty: &Type, obj_repr: &lin_ir::repr::Repr) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        // UNBOXED SUM TYPE (Stage 2/3): a field read on a value that is physically a `SumNode`
        // (repr Packed(SumNode)) — emitted by the lowerer for a narrowed-variant scrutinee.
        // A RECURSIVE CHILD field is a const-offset `*SumNode` pointer load (borrowed interior);
        // a SCALAR field is a const-offset value load; a HEAP field (Stage 3) is a const-offset
        // pointer load + Retain (the lowerer already emits the Retain above). The variant offset
        // is resolved by field NAME — for recursive-child/scalar fields this is unambiguous (each
        // field has a unique name or consistent offset across variants), so `sum_ty` is used.
        // For HEAP fields, the lowerer passes the NARROWED variant type as `obj_ty` (an unsealed
        // Object) so we use that variant's payload for the exact offset rather than scanning all
        // variants — correct when the same field name appears in multiple variants at different
        // offsets (e.g. after heap-field slots shift the position of a shared trailing scalar).
        // Guard on the repr so a mis-seeded boxed value never reads at a raw payload offset.
        if let Some(sum_ty) = obj_repr.sumnode_sum_ty() {
            if obj.is_pointer_value() {
                // Heap-field direct read (Stage 3): obj_ty is the NARROWED variant Object
                // (sealed or not, passed by the lowerer to carry the correct variant payload
                // layout). Use its payload for the field offset so we get the variant-specific
                // position rather than scanning all variants (which gives the wrong offset when
                // the same field name appears in multiple variants at different positions due to
                // preceding heap-field slots). Fall back to the full-sum scan for non-narrowed
                // FieldGets (scalar/recursive-child reads where sum_ty is passed directly).
                if let Type::Object { fields: variant_fields, .. } = obj_ty {
                    if variant_fields.contains_key(field) && !variant_fields.is_empty() {
                        let disc_key = Self::sum_type_discriminant(sum_ty).unwrap_or_default();
                        let payload = Self::sumnode_variant_payload_fields(variant_fields, &disc_key);
                        return self.sumnode_field_get(obj, field, &payload, result_ty);
                    }
                }
                let sum_ty = sum_ty.clone();
                return self.sumnode_field_get_by_name(obj, field, &sum_ty, result_ty);
            }
            return ptr_ty.const_null().into();
        }
        // NULLABLE RECORD (Stage 3 Avenue B): the object is a raw sealed ptr that is guaranteed
        // non-null in the branch we are in (the IsType/null check guards the outer `if`). The
        // physical layout is identical to PackedStruct — delegate to sealed_field_get directly.
        if let Some(fields) = obj_repr.nullable_record_fields() {
            if !fields.contains_key(field) {
                return self.null_value_for(result_ty);
            }
            if obj.is_pointer_value() {
                return self.sealed_field_get(obj, field, fields, result_ty);
            }
            return ptr_ty.const_null().into();
        }
        // STAGE 3: the packed-struct ASSUME is read from the object operand's repr (`func.repr`),
        // not re-derived from `obj_ty`. The representation-inference pass + verifier prove this
        // operand carries a real packed struct exactly where the old `sealed_scalar_fields(obj_ty)`
        // gate fired (oracle-proven byte identical). Constant-offset load is the win.
        if let Some(fields) = obj_repr.packed_struct_fields() {
            // A sealed record has EXACTLY its declared fields. A field NOT in the shape is
            // statically absent — `sealed_field_layout` would assert on it (compiler panic). Follow
            // the safe-access rule (§6.1: missing object key → Null), mirroring the boxed
            // `lin_object_get` missing-key → Null path: produce the Null result for `result_ty`.
            // (The IR lowerer already routes a statically-absent literal key to a Null const; this is
            // the codegen-side safety net for any other FieldGet that reaches here, e.g. destructure.)
            if !fields.contains_key(field) {
                return self.null_value_for(result_ty);
            }
            if obj.is_pointer_value() {
                return self.sealed_field_get(obj, field, fields, result_ty);
            }
            return ptr_ty.const_null().into();
        }
        if obj.is_pointer_value() {
            // A Json/union object arrives as a boxed TaggedVal*; unbox to the raw LinMap*.
            let container = if Self::is_union_type(obj_ty) {
                self.builder.call(self.rt.unbox_ptr, &[obj.into()], "ir_fget_unbox").try_as_basic_value().unwrap_basic()
            } else {
                obj
            };
            let key_str = self.compile_string_lit(field).into_pointer_value();
            // Phase 2: concrete open-object container is now a LinMap*; use map_get.
            let tagged = self.builder.call(self.rt.map_get, &[container.into(), key_str.into()], "ir_fget_m").try_as_basic_value().unwrap_basic();
            // No string_release: `compile_string_lit` returns an interned, immortal
            // LinString (refcount == IMMORTAL_RC), so the release is a runtime no-op
            // — but still an emitted call, hit on every typed field read. Drop it.
            //
            // KEEP-PACKED-THROUGH-RECORD-FIELDS read-back: when the field's declared result type is a
            // sum type, the slot MAY hold a keep-packed `TaggedVal(TAG_SUMNODE)` (the BoxKeepSumnode
            // store — the interp cursor `{node,pos}["node"]` zero-copy path) OR a MATERIALIZED
            // `TAG_OBJECT` (the cross-thread / boundary / fallback path). Dispatch on the RUNTIME TAG so
            // both are correct WITHOUT a static store/read agreement: TAG_SUMNODE → unwrap the still-
            // packed `*SumNode` (+retain), zero copy; otherwise → project the boxed object into a fresh
            // node (the historical path). This runtime-tag dispatch is what makes the optimization sound
            // (no asymmetric keep-packed/materialize decision). A `sum | Null` read is handled the same
            // — a null payload tags as TAG_NULL → the project/unwrap both yield a null node.
            self.unbox_tagged_val_to_type(tagged, result_ty)
        } else { ptr_ty.const_null().into() }
    }

    /// Keep-packed read-back of a sum field into UNION/Json (type-erased) position: the result must
    /// remain a BOXED `TaggedVal*` for the union ABI and be correct for every dynamic consumer
    /// (toString/eq/json). If the slot holds a keep-packed `TAG_SUMNODE`, MATERIALIZE the still-packed
    /// `*SumNode` to a real boxed `LinObject` (TAG_OBJECT) so the type-erased consumers see an object,
    /// not a SumNode they cannot interpret. If it already holds a materialized box (or a null), pass it
    /// through unchanged. Tag-dispatched (sound: zero static asymmetry). The boundary counterpart of
    /// `sumnode_project_from_boxed`'s keep-packed fast path (which targets sum-CONSUMING position and
    /// unwraps the node zero-copy); here the consumer is dynamic so the node is materialized. `sum_ty`
    /// is the non-null sum member. Returns a borrowed-or-fresh `TaggedVal*` (the materialized box is a
    /// fresh +1 the union owning model releases; the pass-through stays the borrowed interior box).
    pub(crate) fn sumnode_box_readback_to_object_box(
        &mut self,
        tagged: BasicValueEnum<'ctx>,
        sum_ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i8_ty = self.context.i8_type();
        if !tagged.is_pointer_value() {
            return tagged;
        }
        let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
        let tag = self.builder.call(self.rt.get_tag, &[tagged.into()], "fgb_sum_tag")
            .try_as_basic_value().unwrap_basic().into_int_value();
        let is_kp = self.builder.int_compare(
            inkwell::IntPredicate::EQ, tag,
            i8_ty.const_int(lin_common::tags::TAG_SUMNODE as u64, false), "fgb_is_kp");
        let kp_bb = self.context.append_basic_block(llvm_fn, "fgb_sum_kp");
        let pass_bb = self.context.append_basic_block(llvm_fn, "fgb_sum_pass");
        let merge_bb = self.context.append_basic_block(llvm_fn, "fgb_sum_merge");
        self.builder.conditional_branch(is_kp, kp_bb, pass_bb);
        // KEEP-PACKED: unwrap the raw *SumNode (borrowed), materialize to a fresh LinMap*, box it.
        self.builder.position_at_end(kp_bb);
        let node = self.builder.call(self.rt.unbox_ptr, &[tagged.into()], "fgb_kp_node").try_as_basic_value().unwrap_basic();
        let obj = self.sumnode_materialize_to_object(node, sum_ty, llvm_fn);
        let kp_box = self.builder.call(self.rt.box_map, &[obj.into()], "fgb_kp_box").try_as_basic_value().unwrap_basic();
        let kp_pred = self.builder.get_insert_block().unwrap();
        self.builder.unconditional_branch(merge_bb);
        // OTHER tag (materialized TAG_MAP box / null): pass through unchanged.
        self.builder.position_at_end(pass_bb);
        self.builder.unconditional_branch(merge_bb);
        self.builder.position_at_end(merge_bb);
        let phi = self.builder.phi(ptr_ty, "fgb_sum_phi");
        phi.add_incoming(&[(&kp_box, kp_pred), (&tagged, pass_bb)]);
        phi.as_basic_value()
    }

}