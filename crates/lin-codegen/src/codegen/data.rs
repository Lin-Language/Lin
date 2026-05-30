use super::builder_ext::BuilderExt;
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::BasicValueEnum;
use inkwell::{AddressSpace, IntPredicate};

use lin_check::types::Type;
use super::Codegen;

impl<'ctx> Codegen<'ctx> {
    /// Push a scalar into a flat unboxed array (lin_flat_array_push_<suffix>).
    pub(crate) fn flat_array_push(&mut self, arr: BasicValueEnum<'ctx>, val: BasicValueEnum<'ctx>, elem_ty: &Type) {
        let suffix = Self::flat_suffix(elem_ty);
        let push_name = format!("lin_flat_array_push_{}", suffix);
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let llvm_elem_ty = self.llvm_type(elem_ty);
        let push_fn = self.get_or_declare_fn(&push_name,
            self.context.void_type().fn_type(&[ptr_ty.into(), llvm_elem_ty.into()], false));
        self.builder.call(push_fn, &[arr.into(), val.into()], "");
    }

    /// Load a scalar element from a flat unboxed array (lin_flat_array_get_<suffix>).
    pub(crate) fn flat_array_get(&mut self, arr: BasicValueEnum<'ctx>, idx: inkwell::values::IntValue<'ctx>, elem_ty: &Type) -> BasicValueEnum<'ctx> {
        let suffix = Self::flat_suffix(elem_ty);
        let get_name = format!("lin_flat_array_get_{}", suffix);
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let llvm_elem_ty = self.llvm_type(elem_ty);
        let get_fn = self.get_or_declare_fn(&get_name,
            llvm_elem_ty.fn_type(&[ptr_ty.into(), i64_ty.into()], false));
        self.builder.call(get_fn, &[arr.into(), idx.into()], "flat_get")
            .try_as_basic_value()
            .unwrap_basic()
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
                // cell must hold 8 defined bytes. Pointers are 8 bytes; small integers/bools
                // are zero-extended to i64; f32 is bit-widened via an i64 cell.
                let (store_val, store_llvm_ty): (BasicValueEnum<'ctx>, BasicTypeEnum<'ctx>) = match val_ty {
                    Type::Str | Type::Array(_) | Type::Object(_) | Type::Iterator(_) | Type::Function { .. } => {
                        (val, self.context.ptr_type(inkwell::AddressSpace::default()).as_basic_type_enum())
                    }
                    _ if val.is_int_value() => {
                        let i64_ty = self.context.i64_type();
                        let ext = self.builder.int_z_extend_or_bit_cast(val.into_int_value(), i64_ty, "arr_cell_ext");
                        (ext.into(), i64_ty.as_basic_type_enum())
                    }
                    _ => (val, self.llvm_type(val_ty)),
                };
                let cell = self.builder.alloca(store_llvm_ty, "arr_cell");
                self.builder.store(cell, store_val);
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
            let is_i32 = self.builder.int_compare(IntPredicate::EQ, k_tag, i8t.const_int(2, false), "ir_k_i32");
            let is_i64 = self.builder.int_compare(IntPredicate::EQ, k_tag, i8t.const_int(3, false), "ir_k_i64");
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
            let is_obj = self.builder.int_compare(IntPredicate::EQ, obj_tag, i8t.const_int(7, false), "ir_idx_isobj");
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
            if Self::is_flat_scalar(result_ty) {
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
        let key_str = if matches!(key_ty, Type::Str) {
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
                IntPredicate::EQ, obj_tag, self.context.i8_type().const_int(7, false), "ir_idx_is_obj");
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
            Type::Object(_) | Type::Named(_) => {
                if obj.is_pointer_value() && key.is_pointer_value() {
                    let key_str = resolve_obj_key(self, key);
                    self.emit_object_set(obj, key_str, value, val_ty);
                }
            }
            Type::Array(_) | Type::FixedArray(_) => {
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
                    let is_i32 = self.builder.int_compare(IntPredicate::EQ, k_tag, i8t.const_int(2, false), "iset_k_i32");
                    let is_i64 = self.builder.int_compare(IntPredicate::EQ, k_tag, i8t.const_int(3, false), "iset_k_i64");
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

    pub(crate) fn compile_ir_field_get(&mut self, obj: BasicValueEnum<'ctx>, field: &str, obj_ty: &Type, result_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        if obj.is_pointer_value() {
            // A Json/union object arrives as a boxed TaggedVal*; unbox to the raw LinObject*.
            let container = if Self::is_union_type(obj_ty) {
                self.builder.call(self.rt.unbox_ptr, &[obj.into()], "ir_fget_unbox").try_as_basic_value().unwrap_basic()
            } else {
                obj
            };
            let key_str = self.compile_string_lit(field).into_pointer_value();
            let tagged = self.builder.call(self.rt.object_get, &[container.into(), key_str.into()], "ir_fget").try_as_basic_value().unwrap_basic();
            self.builder.call(self.rt.string_release, &[key_str.into()], "");
            self.unbox_tagged_val_to_type(tagged, result_ty)
        } else { ptr_ty.const_null().into() }
    }

}