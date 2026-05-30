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
        self.builder.build_call(push_fn, &[arr.into(), val.into()], "").unwrap();
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
        self.builder.build_call(get_fn, &[arr.into(), idx.into()], "flat_get")
            .unwrap()
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
        self.builder.build_call(rt_push_tagged, &[arr.into(), val_ptr.into()], "").unwrap();
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
                        let ext = self.builder.build_int_z_extend_or_bit_cast(val.into_int_value(), i64_ty, "arr_cell_ext").unwrap();
                        (ext.into(), i64_ty.as_basic_type_enum())
                    }
                    _ => (val, self.llvm_type(val_ty)),
                };
                let cell = self.builder.build_alloca(store_llvm_ty, "arr_cell").unwrap();
                self.builder.build_store(cell, store_val).unwrap();
                self.builder.build_call(self.rt.array_push, &[arr.into(), cell.into(), tag.into()], "arr_push").unwrap();
            }
        }
    }

    /// Coerce an IR value to a raw heap pointer (LinObject*/LinArray*/LinString*): if the
    /// static type is a union (boxed TaggedVal*) OR the value isn't already a pointer, unbox
    /// it; otherwise pass through. Used by the dynamic object/array helper intrinsics.
    pub(crate) fn ir_as_raw_ptr(&mut self, v: BasicValueEnum<'ctx>, ty: &Type) -> BasicValueEnum<'ctx> {
        if Self::is_union_type(ty) || !v.is_pointer_value() {
            self.builder.build_call(self.rt.unbox_ptr, &[v.into()], "ir_raw_ptr")
                .unwrap().try_as_basic_value().unwrap_basic()
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
            let n_i32 = self.builder.build_call(self.rt.unbox_int32, &[n.into()], "ir_n_unbox")
                .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
            return self.builder.build_int_s_extend(n_i32, i64_ty, "ir_n64").unwrap();
        }
        if n.is_int_value() {
            let _ = n_ty;
            self.builder.build_int_s_extend_or_bit_cast(n.into_int_value(), i64_ty, "ir_n64").unwrap()
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
            self.builder.build_call(self.rt.unbox_ptr, &[obj.into()], "ir_idx_unbox")
                .unwrap().try_as_basic_value().unwrap_basic()
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
            let k_tag = self.builder.build_call(self.rt.get_tag, &[key.into()], "ir_idxk_tag")
                .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
            let i8t = self.context.i8_type();
            let is_i32 = self.builder.build_int_compare(IntPredicate::EQ, k_tag, i8t.const_int(2, false), "ir_k_i32").unwrap();
            let is_i64 = self.builder.build_int_compare(IntPredicate::EQ, k_tag, i8t.const_int(3, false), "ir_k_i64").unwrap();
            let is_int = self.builder.build_or(is_i32, is_i64, "ir_k_int").unwrap();
            let int_b = self.context.append_basic_block(llvm_fn, "ir_idx_intk");
            let str_b = self.context.append_basic_block(llvm_fn, "ir_idx_strk");
            let mrg = self.context.append_basic_block(llvm_fn, "ir_idx_kmrg");
            self.builder.build_conditional_branch(is_int, int_b, str_b).unwrap();
            // int key → array get (always returns a valid TaggedVal*).
            self.builder.position_at_end(int_b);
            let idx = self.unbox_value(key, &Type::Int64).into_int_value();
            let get_tagged_fn = self.get_or_declare_fn("lin_array_get_tagged",
                ptr_ty.fn_type(&[ptr_ty.into(), self.context.i64_type().into()], false));
            let arr_res = self.builder.build_call(get_tagged_fn, &[container.into(), idx.into()], "ir_idx_aget")
                .unwrap().try_as_basic_value().unwrap_basic();
            let int_exit = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(mrg).unwrap();
            // string key → object get, guarded by an object-tag check on the container source.
            self.builder.position_at_end(str_b);
            let key_raw = self.builder.build_call(self.rt.unbox_ptr, &[key.into()], "ir_idxk_str")
                .unwrap().try_as_basic_value().unwrap_basic();
            let obj_tag = self.builder.build_call(self.rt.get_tag, &[obj.into()], "ir_idx_otag")
                .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
            let is_obj = self.builder.build_int_compare(IntPredicate::EQ, obj_tag, i8t.const_int(7, false), "ir_idx_isobj").unwrap();
            let oget_b = self.context.append_basic_block(llvm_fn, "ir_idx_oget");
            let onull_b = self.context.append_basic_block(llvm_fn, "ir_idx_onull");
            let omrg = self.context.append_basic_block(llvm_fn, "ir_idx_omrg");
            self.builder.build_conditional_branch(is_obj, oget_b, onull_b).unwrap();
            self.builder.position_at_end(oget_b);
            let oget = self.builder.build_call(self.rt.object_get, &[container.into(), key_raw.into()], "ir_idx_osget")
                .unwrap().try_as_basic_value().unwrap_basic();
            let oget_exit = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(omrg).unwrap();
            self.builder.position_at_end(onull_b);
            self.builder.build_unconditional_branch(omrg).unwrap();
            self.builder.position_at_end(omrg);
            let ophi = self.builder.build_phi(ptr_ty, "ir_idx_ophi").unwrap();
            ophi.add_incoming(&[(&oget, oget_exit), (&ptr_ty.const_null(), onull_b)]);
            let str_res = ophi.as_basic_value();
            let str_exit = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(mrg).unwrap();
            self.builder.position_at_end(mrg);
            let phi = self.builder.build_phi(ptr_ty, "ir_idx_kphi").unwrap();
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
                self.builder.build_int_s_extend_or_bit_cast(key.into_int_value(), self.context.i64_type(), "ir_idx").unwrap()
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
                return self.builder.build_call(get_tagged_fn, &[container.into(), idx.into()], "ir_aget_tv")
                    .unwrap().try_as_basic_value().unwrap_basic();
            }
            let tagged = self.builder.build_call(self.rt.array_get, &[container.into(), idx.into()], "ir_aget")
                .unwrap().try_as_basic_value().unwrap_basic();
            return self.unbox_tagged_val_to_type(tagged, result_ty);
        }
        // Object key access. lin_object_get expects a raw *LinString key; unbox a boxed key.
        let key_str = if matches!(key_ty, Type::Str) {
            key
        } else if Self::is_union_type(key_ty) && key.is_pointer_value() {
            self.builder.build_call(self.rt.unbox_ptr, &[key.into()], "ir_key_unbox")
                .unwrap().try_as_basic_value().unwrap_basic()
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
            let obj_tag = self.builder.build_call(self.rt.get_tag, &[obj.into()], "ir_idx_tag")
                .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
            let is_obj = self.builder.build_int_compare(
                IntPredicate::EQ, obj_tag, self.context.i8_type().const_int(7, false), "ir_idx_is_obj").unwrap();
            let ok = self.context.append_basic_block(llvm_fn, "ir_idx_obj_ok");
            let no = self.context.append_basic_block(llvm_fn, "ir_idx_obj_no");
            let mrg = self.context.append_basic_block(llvm_fn, "ir_idx_obj_mrg");
            self.builder.build_conditional_branch(is_obj, ok, no).unwrap();
            self.builder.position_at_end(ok);
            let entry = self.builder.build_call(self.rt.object_get, &[container.into(), key_str.into()], "ir_oget")
                .unwrap().try_as_basic_value().unwrap_basic();
            let ok_exit = self.builder.get_insert_block().unwrap();
            self.builder.build_unconditional_branch(mrg).unwrap();
            self.builder.position_at_end(no);
            let null_res = ptr_ty.const_null();
            self.builder.build_unconditional_branch(mrg).unwrap();
            self.builder.position_at_end(mrg);
            let phi = self.builder.build_phi(ptr_ty, "ir_idx_obj_phi").unwrap();
            phi.add_incoming(&[(&entry, ok_exit), (&null_res, no)]);
            let result_ptr = phi.as_basic_value();
            return self.unbox_tagged_val_to_type(result_ptr, result_ty);
        }
        let tagged = self.builder.build_call(self.rt.object_get, &[container.into(), key_str.into()], "ir_oget")
            .unwrap().try_as_basic_value().unwrap_basic();
        self.unbox_tagged_val_to_type(tagged, result_ty)
    }

    /// `object[key] = value` for the IR path. Mirrors the AST `compile_index_set`:
    /// dispatch on the object's static type; for Json/union objects, dispatch at
    /// runtime on the value's LLVM kind (pointer key ⇒ object set, int key ⇒ array set),
    /// unboxing the boxed container first.
    pub(crate) fn compile_ir_index_set(&mut self, obj: BasicValueEnum<'ctx>, key: BasicValueEnum<'ctx>, value: BasicValueEnum<'ctx>, obj_ty: &Type, _key_ty: &Type, val_ty: &Type) {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let void_ty = self.context.void_type();
        let tagged_val = self.build_tagged_val_alloca(&value, val_ty);
        match obj_ty {
            Type::Object(_) | Type::Named(_) => {
                if obj.is_pointer_value() && key.is_pointer_value() {
                    self.builder.build_call(self.rt.object_set,
                        &[obj.into(), key.into(), tagged_val.into()], "").unwrap();
                }
            }
            Type::Array(_) | Type::FixedArray(_) => {
                let idx = self.index_value_to_i64(key);
                let set_fn = self.get_or_declare_fn("lin_array_set",
                    void_ty.fn_type(&[ptr_ty.into(), i64_ty.into(), ptr_ty.into()], false));
                self.builder.build_call(set_fn, &[obj.into(), idx.into(), tagged_val.into()], "").unwrap();
            }
            Type::TypeVar(_) | Type::Union(_) => {
                if !obj.is_pointer_value() { return; }
                // Unbox the boxed container, then dispatch on the key's LLVM kind.
                let container = self.builder.build_call(self.rt.unbox_ptr, &[obj.into()], "iset_unbox")
                    .unwrap().try_as_basic_value().unwrap_basic();
                if key.is_pointer_value() {
                    // String (object) key.
                    self.builder.build_call(self.rt.object_set,
                        &[container.into(), key.into(), tagged_val.into()], "").unwrap();
                } else if key.is_int_value() {
                    let idx = self.index_value_to_i64(key);
                    let set_fn = self.get_or_declare_fn("lin_array_set",
                        void_ty.fn_type(&[ptr_ty.into(), i64_ty.into(), ptr_ty.into()], false));
                    self.builder.build_call(set_fn, &[container.into(), idx.into(), tagged_val.into()], "").unwrap();
                }
            }
            _ => {}
        }
    }

    /// Normalise an index value (raw int or boxed TaggedVal*) to an i64.
    pub(crate) fn index_value_to_i64(&mut self, key: BasicValueEnum<'ctx>) -> inkwell::values::IntValue<'ctx> {
        if key.is_int_value() {
            self.builder.build_int_s_extend_or_bit_cast(key.into_int_value(), self.context.i64_type(), "ir_idx64").unwrap()
        } else if key.is_pointer_value() {
            let i32_key = self.builder.build_call(self.rt.unbox_int32, &[key.into()], "ir_skey_i32")
                .unwrap().try_as_basic_value().unwrap_basic().into_int_value();
            self.builder.build_int_s_extend(i32_key, self.context.i64_type(), "ir_skey_i64").unwrap()
        } else {
            self.context.i64_type().const_zero()
        }
    }

    pub(crate) fn compile_ir_field_get(&mut self, obj: BasicValueEnum<'ctx>, field: &str, obj_ty: &Type, result_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        if obj.is_pointer_value() {
            // A Json/union object arrives as a boxed TaggedVal*; unbox to the raw LinObject*.
            let container = if Self::is_union_type(obj_ty) {
                self.builder.build_call(self.rt.unbox_ptr, &[obj.into()], "ir_fget_unbox")
                    .unwrap().try_as_basic_value().unwrap_basic()
            } else {
                obj
            };
            let key_str = self.compile_string_lit(field).into_pointer_value();
            let tagged = self.builder.build_call(self.rt.object_get, &[container.into(), key_str.into()], "ir_fget")
                .unwrap().try_as_basic_value().unwrap_basic();
            self.builder.build_call(self.rt.string_release, &[key_str.into()], "").unwrap();
            self.unbox_tagged_val_to_type(tagged, result_ty)
        } else { ptr_ty.const_null().into() }
    }

}
