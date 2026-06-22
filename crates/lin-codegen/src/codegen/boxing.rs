use super::builder_ext::BuilderExt;
use inkwell::values::{
    BasicMetadataValueEnum, BasicValueEnum, PointerValue,
};
use inkwell::AddressSpace;

use lin_check::types::Type;
use super::Codegen;

impl<'ctx> Codegen<'ctx> {
    /// Box one argument into the uniform boxed closure-call ABI representation: a TaggedVal*
    /// (ptr). EVERY indirect/closure call passes its args this way, because every function
    /// value is stored as a boxed-ABI wrapper (`__cls_wrapb_*` / `__papp_*`) that declares all
    /// params `ptr` and unboxes each to its concrete type.
    ///
    /// An argument whose Lin type is already a union/Json is itself a boxed `ptr` — passed
    /// through unchanged to avoid double-boxing. A concrete scalar / raw String*/Array*/Object*
    /// value is boxed. This keeps both ends of every indirect call agreeing on the all-ptr ABI
    /// regardless of which args the IR pre-boxed (the IR only boxes up to the value's *declared*
    /// param arity, e.g. one for an opaque `Function`, so extra args reach here unboxed — the
    /// wrapper-ABI bug).
    pub(crate) fn box_arg_for_closure_abi(
        &mut self,
        val: BasicMetadataValueEnum<'ctx>,
        arg_ty: &Type,
    ) -> BasicValueEnum<'ctx> {
        let basic: BasicValueEnum<'ctx> = match val {
            BasicMetadataValueEnum::IntValue(v) => v.into(),
            BasicMetadataValueEnum::FloatValue(v) => v.into(),
            BasicMetadataValueEnum::PointerValue(v) => v.into(),
            BasicMetadataValueEnum::ArrayValue(v) => v.into(),
            BasicMetadataValueEnum::StructValue(v) => v.into(),
            BasicMetadataValueEnum::VectorValue(v) => v.into(),
            _ => self.context.ptr_type(AddressSpace::default()).const_null().into(),
        };
        // Already a boxed Json/union value (a ptr) — pass through.
        if Self::is_union_type(arg_ty) {
            return basic;
        }
        self.box_value(basic, arg_ty)
    }

    /// Box a value into a tagged union pointer (TaggedVal*).
    /// For concrete types, allocates and fills a TaggedVal with the appropriate tag.
    /// For TypeVar, dispatches on the actual LLVM type (int/float/pointer) to pick the right box call.
    pub(crate) fn box_value(&mut self, val: BasicValueEnum<'ctx>, val_ty: &Type) -> BasicValueEnum<'ctx> {
        #[allow(unreachable_patterns)] // _ arm is a future-proof guard; currently exhaustive
        let ptr = match val_ty {
            Type::Null => self.builder.call(self.rt.box_null, &[], "boxnull")
                .try_as_basic_value().unwrap_basic(),
            Type::Bool => {
                let i8v = if val.is_int_value() {
                    // Bool is i1; zero-extend to i8 for lin_box_bool(i8).
                    self.builder.int_z_extend_or_bit_cast(val.into_int_value(), self.context.i8_type(), "btoi8").into()
                } else { val };
                self.builder.call(self.rt.box_bool, &[i8v.into()], "boxbool")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Int8 | Type::Int16 | Type::Int32 | Type::IntLit(_) => {
                let i32v = self.builder.int_s_extend_or_bit_cast(val.into_int_value(), self.context.i32_type(), "toi32");
                self.builder.call(self.rt.box_int32, &[i32v.into()], "boxi32")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::UInt8 | Type::UInt16 | Type::UInt32 => {
                // Zero-extend to a (always-positive) i64 and box as TAG_INT64 so the value
                // reads back correctly: a u32 >= 2^31 would be a negative i32 if boxed as
                // TAG_INT32, breaking display/JSON/eq/cmp. The zero-extended i64 is positive.
                let i64v = self.builder.int_z_extend_or_bit_cast(val.into_int_value(), self.context.i64_type(), "tou64");
                self.builder.call(self.rt.box_int64, &[i64v.into()], "boxu_as_i64")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Int64 => {
                let i64v = val.into_int_value();
                self.builder.call(self.rt.box_int64, &[i64v.into()], "boxi64")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::UInt64 => {
                // Box as TAG_UINT64 so the payload is read back unsigned (a u64 >= 2^63 would
                // be negative if read as i64).
                let i64v = val.into_int_value();
                self.builder.call(self.rt.box_uint64, &[i64v.into()], "boxu64")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Float32 => {
                let f64v = self.builder.float_ext(val.into_float_value(), self.context.f64_type(), "f32tof64");
                self.builder.call(self.rt.box_float64, &[f64v.into()], "boxf64")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Float64 => {
                self.builder.call(self.rt.box_float64, &[val.into()], "boxf64")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Str | Type::StrLit(_) => {
                self.builder.call(self.rt.box_str, &[val.into()], "boxstr")
                    .try_as_basic_value().unwrap_basic()
            }
            // A SEALED SCALAR RECORD is a packed struct, NOT a LinMap/LinObject — box_map would
            // treat its ptr as a LinMap header and corrupt. Materialize to a fresh LinMap first,
            // then box that as TAG_MAP. (This is the same conversion the Coerce(sealed → Json)
            // boundary performs; it is the safety net for any path that boxes a sealed value
            // directly, e.g. heterogeneous array elements or closure args.)
            // NOTE (Stage 6a): the sealed→Json Coerce path in compile_ir_coerce emits lin_box_record
            // DIRECTLY (bypassing this materialize path) when the target is a union/Json slot.
            Type::Object { .. } if Self::sealed_scalar_fields(val_ty).is_some() => {
                let fields = Self::sealed_scalar_fields(val_ty).unwrap().clone();
                let obj = self.sealed_materialize_to_map(val, &fields);
                self.builder.call(self.rt.box_map, &[obj.into()], "boxsealed_map")
                    .try_as_basic_value().unwrap_basic()
            }
            // Phase 2: non-sealed open objects are now LinMap* — box with TAG_MAP.
            Type::Object { .. } => {
                self.builder.call(self.rt.box_map, &[val.into()], "boxmap_obj")
                    .try_as_basic_value().unwrap_basic()
            }
            // Typed index-signature map (`{ K: V }`, ADR-055 + numeric-key): box the LinMap* as TAG_MAP.
            Type::Map { .. } => {
                self.builder.call(self.rt.box_map, &[val.into()], "boxmap")
                    .try_as_basic_value().unwrap_basic()
            }
            // A SEALED-RECORD ARRAY (0xFD pointer-backed or 0xFE inline-packed) is boxed as a
            // TAG_ARRAY by storing the raw LinArray* pointer directly — the elem_tag field lets
            // runtime consumers (lin_array_get_tagged, lin_array_release, lin_tagged_release, etc.)
            // dispatch correctly without a prior O(n) materialise to a tagged Object[] copy.
            // Explicit materialise-before-box callers (intrinsics.rs ToString, arith.rs equality)
            // call sealed_array_to_tagged themselves and do NOT route through box_value.
            // The keep-packed container-store path (emit_map_set / compile_ir_box_keep_packed) also
            // bypasses box_value.
            Type::Array(_) if val.is_pointer_value() && Self::sealed_array_elem(val_ty).is_some() => {
                self.builder.call(self.rt.box_array, &[val.into()], "boxsarr")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Array(_) if val.is_pointer_value() => {
                // Box the LinArray* directly (flat or tagged). The elem_tag field in LinArray
                // lets runtime functions (lin_array_get_tagged, lin_push_dyn, etc.) dispatch
                // correctly without needing a separate conversion copy.
                self.builder.call(self.rt.box_array, &[val.into()], "boxarr")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_) => {
                // Iterator values have already been converted to tagged arrays by the intrinsic.
                self.builder.call(self.rt.box_array, &[val.into()], "boxarr")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Function { .. } => {
                self.builder.call(self.rt.box_function, &[val.into()], "boxfn")
                    .try_as_basic_value().unwrap_basic()
            }
            // UNBOXED SUM TYPE (unboxed-sumtype Stage 3): a Stage-eligible sum type's value is
            // physically a `*SumNode`, NOT a `LinMap`. `box_value` is the GENERICALLY-DYNAMIC
            // boxing entry (toString / `==` / match-discriminator `map_get` / spread / closure
            // arg / a `sum|Null` param) — those consumers read the boxed value as a LinMap, so the
            // node MUST be MATERIALIZED to a real boxed `LinMap` here (Phase 2: sumnode materializes
            // to LinMap* — box as TAG_MAP; formerly TAG_OBJECT, which was a latent type-confusion bug:
            // the consumer walked a SumNode header as a LinObject → garbage discriminant / crash).
            // The keep-packed container-store boundary (Map value slot) does NOT route through
            // `box_value`; it stores a TAG_SUMNODE node directly so only the genuinely-dynamic
            // consumers pay the materialize.
            // Handle BEFORE the generic Union arm (a sum type IS a `Type::Union`).
            Type::Union(_) if Self::is_sum_type(val_ty) && val.is_pointer_value() => {
                let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                let map = self.sumnode_materialize_to_object(val, val_ty, llvm_fn);
                self.builder.call(self.rt.box_map, &[map.into()], "boxsummap")
                    .try_as_basic_value().unwrap_basic()
            }
            // A `sum | Null` value (e.g. a `{ String: Expr }` map read): physically EITHER a `*SumNode`
            // or a null pointer. Materialize the node to a boxed LinMap when non-null, else box
            // Null. Without this the generic Union arm (below) returned the raw `*SumNode` verbatim
            // (a non-`all_objects` union), so a downstream `map_get`/match read it as a LinMap →
            // garbage discriminant ("non-exhaustive match"). Runtime-branch on null.
            Type::Union(_) if Self::sum_member_of_nullable_union(val_ty).is_some() && val.is_pointer_value() => {
                let sum_ty = Self::sum_member_of_nullable_union(val_ty).unwrap();
                let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                let p = val.into_pointer_value();
                let pi = self.builder.ptr_to_int(p, self.context.i64_type(), "sumnull_p2i");
                let is_null = self.builder.int_compare(inkwell::IntPredicate::EQ, pi, self.context.i64_type().const_zero(), "sumnull_isnull");
                let null_bb = self.context.append_basic_block(llvm_fn, "sum_box_null");
                let node_bb = self.context.append_basic_block(llvm_fn, "sum_box_node");
                let merge_bb = self.context.append_basic_block(llvm_fn, "sum_box_merge");
                self.builder.conditional_branch(is_null, null_bb, node_bb);
                self.builder.position_at_end(null_bb);
                let null_box = self.builder.call(self.rt.box_null, &[], "sumnull_box").try_as_basic_value().unwrap_basic();
                let null_pred = self.builder.get_insert_block().unwrap();
                self.builder.unconditional_branch(merge_bb);
                self.builder.position_at_end(node_bb);
                let map = self.sumnode_materialize_to_object(val, &sum_ty, llvm_fn);
                let node_box = self.builder.call(self.rt.box_map, &[map.into()], "sumnode_mapbox").try_as_basic_value().unwrap_basic();
                let node_pred = self.builder.get_insert_block().unwrap();
                self.builder.unconditional_branch(merge_bb);
                self.builder.position_at_end(merge_bb);
                let phi = self.builder.phi(self.context.ptr_type(AddressSpace::default()), "sum_box_phi");
                phi.add_incoming(&[(&null_box, null_pred), (&node_box, node_pred)]);
                phi.as_basic_value()
            }
            // Union type — if value is a pointer, box as object (most common case).
            // If it's already a tagged pointer, return as-is.
            Type::Union(variants) => {
                if val.is_pointer_value() {
                    // If all variants are Object types, they are now LinMap* (Phase 2).
                    let all_objects = variants.iter().all(|v| matches!(v, Type::Object { .. }));
                    if all_objects {
                        self.builder.call(self.rt.box_map, &[val.into()], "boxobj_map")
                            .try_as_basic_value().unwrap_basic()
                    } else {
                        // Already tagged (or unknown) — return as-is.
                        val
                    }
                } else {
                    val
                }
            }
            Type::TypeVar(_) => {
                // TypeVar value — box by actual LLVM type.
                if val.is_int_value() {
                    let iv = val.into_int_value();
                    let i32_ty = self.context.i32_type();
                    let i64_ty = self.context.i64_type();
                    let bit_width = iv.get_type().get_bit_width();
                    if bit_width <= 32 {
                        let i32v = self.builder.int_s_extend_or_bit_cast(iv, i32_ty, "tvi32");
                        self.builder.call(self.rt.box_int32, &[i32v.into()], "tvboxi32")
                            .try_as_basic_value().unwrap_basic()
                    } else {
                        let i64v = self.builder.int_s_extend_or_bit_cast(iv, i64_ty, "tvi64");
                        self.builder.call(self.rt.box_int64, &[i64v.into()], "tvboxi64")
                            .try_as_basic_value().unwrap_basic()
                    }
                } else if val.is_float_value() {
                    let fv = val.into_float_value();
                    let f64_ty = self.context.f64_type();
                    let f64v = self.builder.float_ext(fv, f64_ty, "tvf64");
                    self.builder.call(self.rt.box_float64, &[f64v.into()], "tvboxf64")
                        .try_as_basic_value().unwrap_basic()
                } else {
                    val
                }
            }
            // Opaque handle types whose runtime value is already a boxed TaggedVal* — pass through
            // unchanged. is_union_type() returns true for all of these, so call sites that guard
            // with is_union_type() never reach here; the pass-through is the safety net for any
            // site that doesn't guard.
            Type::Shared(_) | Type::Stream(_) | Type::Promise(_) | Type::Opaque(_)
            | Type::Named(_) | Type::Never => val,
            // Any Type variant that reaches here was not expected to be boxed. In a release build
            // the old fall-through behaviour is preserved (return val unchanged) so existing
            // behaviour is not regressed; in debug/test builds this fires as a panic so the corpus
            // gate catches the unhandled case immediately rather than silently miscompiling.
            _ => {
                debug_assert!(false, "box_value: unhandled type {val_ty:?} — add an explicit arm");
                val
            }
        };
        ptr
    }

    /// Unbox a tagged union pointer to the concrete type `target_ty`.
    /// Handles the same type set as `unbox_tagged_val_to_type`; call sites in the closure-wrapper
    /// ABI (call.rs) and index key paths use this entry point with concrete scalar/ptr types.
    pub(crate) fn unbox_value(&mut self, ptr: BasicValueEnum<'ctx>, target_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_val = ptr.into_pointer_value();
        #[allow(unreachable_patterns)] // _ arm is a future-proof guard; currently exhaustive
        match target_ty {
            Type::Null => self.context.ptr_type(AddressSpace::default()).const_null().into(),
            Type::Bool => {
                let v = self.builder.call(self.rt.unbox_bool, &[ptr_val.into()], "ubool")
                    .try_as_basic_value().unwrap_basic();
                self.builder.int_truncate(v.into_int_value(), self.context.bool_type(), "utobool").into()
            }
            Type::Int8 | Type::Int16 | Type::Int32 | Type::IntLit(_) => {
                let v = self.builder.call(self.rt.unbox_int32, &[ptr_val.into()], "ui32")
                    .try_as_basic_value().unwrap_basic();
                let ity = self.llvm_type(target_ty).into_int_type();
                self.builder.int_truncate_or_bit_cast(v.into_int_value(), ity, "toi").into()
            }
            Type::UInt8 | Type::UInt16 | Type::UInt32 => {
                let v = self.builder.call(self.rt.unbox_int64, &[ptr_val.into()], "uu64")
                    .try_as_basic_value().unwrap_basic();
                let ity = self.llvm_type(target_ty).into_int_type();
                self.builder.int_truncate_or_bit_cast(v.into_int_value(), ity, "toui").into()
            }
            Type::Int64 => {
                self.builder.call(self.rt.unbox_int64, &[ptr_val.into()], "ui64")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::UInt64 => {
                self.builder.call(self.rt.unbox_uint64, &[ptr_val.into()], "uu64v")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Float32 | Type::Float64 => {
                let v = self.builder.call(self.rt.unbox_float64, &[ptr_val.into()], "uf64")
                    .try_as_basic_value().unwrap_basic();
                if matches!(target_ty, Type::Float32) {
                    self.builder.float_trunc(v.into_float_value(), self.context.f32_type(), "tof32").into()
                } else {
                    v
                }
            }
            Type::Str | Type::StrLit(_) => {
                self.builder.call(self.rt.unbox_ptr, &[ptr_val.into()], "ustr")
                    .try_as_basic_value().unwrap_basic()
            }
            Type::Object { .. } if Self::sealed_scalar_fields(target_ty).is_some() => {
                let fields = Self::sealed_scalar_fields(target_ty).unwrap().clone();
                self.sealed_project_from(ptr, &Type::TypeVar(u32::MAX), &fields)
            }
            Type::Object { .. } | Type::Array(_) | Type::FixedArray(_) | Type::Function { .. } | Type::Map { .. } => {
                self.builder.call(self.rt.unbox_ptr, &[ptr_val.into()], "uptr")
                    .try_as_basic_value().unwrap_basic()
            }
            // KEEP-PACKED-THROUGH-RECORD-FIELDS: `sum | Null` union box — materialize TAG_SUMNODE
            // slots to a real TAG_MAP so dynamic consumers see a valid LinMap*.
            Type::Union(_) if Self::sum_member_of_nullable_union(target_ty).is_some() => {
                let sum_ty = Self::sum_member_of_nullable_union(target_ty).unwrap();
                self.sumnode_box_readback_to_object_box(ptr, &sum_ty)
            }
            // Sum-type-eligible Union: the body compiled with Packed(SumNode) repr, so the
            // incoming TaggedVal* (from the stdlib closure-call ABI) must be projected to
            // a *SumNode before forwarding. sumnode_project_from_boxed tag-dispatches:
            // TAG_SUMNODE → unwrap+retain (zero copy); TAG_MAP → run the per-type projector.
            Type::Union(_) if Self::is_sum_type(target_ty) => {
                let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                self.sumnode_project_from_boxed(ptr, target_ty, target_ty, llvm_fn)
            }
            // Already tagged — return as-is.
            Type::Union(_) | Type::TypeVar(_) => ptr,
            // Opaque handle types: their runtime value IS the tagged box pointer.
            Type::Shared(_) | Type::Stream(_) | Type::Promise(_) | Type::Opaque(_)
            | Type::Named(_) | Type::Never | Type::Iterator(_) => ptr,
            // Sum type: project from the boxed LinMap back to a fresh *SumNode.
            _ if Self::is_sum_type(target_ty) => {
                let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                self.sumnode_project_from_boxed(ptr, target_ty, target_ty, llvm_fn)
            }
            _ => {
                debug_assert!(false, "unbox_value: unhandled type {target_ty:?} — add an explicit arm");
                ptr
            }
        }
    }

    /// Compute the 64-bit TaggedVal payload bits for `val` of type `val_ty`. This is the value
    /// half of a TaggedVal (scalars zero/sign-extended or float-bitcast to i64; pointers
    /// `ptrtoint`-ed). Shared by `build_tagged_val_alloca` and the inline object-construction
    /// fast path, so the two never drift on how a payload is encoded.
    pub(crate) fn tagged_payload_i64(&mut self, val: &BasicValueEnum<'ctx>, val_ty: &Type) -> inkwell::values::IntValue<'ctx> {
        let i64_ty = self.context.i64_type();
        match val_ty {
            Type::Null => i64_ty.const_zero(),
            Type::Bool => {
                if val.is_int_value() {
                    self.builder.int_z_extend(val.into_int_value(), i64_ty, "bext")
                } else { i64_ty.const_zero() }
            }
            Type::Int8 | Type::Int16 | Type::Int32 | Type::IntLit(_) | Type::UInt8 | Type::UInt16 | Type::UInt32 => {
                if val.is_int_value() {
                    self.builder.int_z_extend_or_bit_cast(val.into_int_value(), i64_ty, "iext")
                } else { i64_ty.const_zero() }
            }
            Type::Int64 | Type::UInt64 => {
                if val.is_int_value() { val.into_int_value() } else { i64_ty.const_zero() }
            }
            Type::Float32 => {
                let fv = if val.is_float_value() { val.into_float_value() }
                    else { self.context.f32_type().const_float(0.0) };
                // Extend to f64 then bitcast bits to i64
                let fv64 = self.builder.float_ext(fv, self.context.f64_type(), "f32ext");
                self.builder.bit_cast(fv64, i64_ty, "fbits").into_int_value()
            }
            Type::Float64 => {
                let fv = if val.is_float_value() { val.into_float_value() }
                    else { self.context.f64_type().const_float(0.0) };
                // Bitcast f64 bits to i64 (reinterpret, not convert)
                self.builder.bit_cast(fv, i64_ty, "fbits").into_int_value()
            }
            _ => {
                // Pointer types: str, array, object, function — store pointer as u64
                if val.is_pointer_value() {
                    self.builder.ptr_to_int(val.into_pointer_value(), i64_ty, "pti")
                } else { i64_ty.const_zero() }
            }
        }
    }

    /// Build a stack-allocated TaggedVal from a value + type, return its alloca ptr.
    pub(crate) fn build_tagged_val_alloca(&mut self, val: &BasicValueEnum<'ctx>, val_ty: &Type) -> PointerValue<'ctx> {
        // TaggedVal layout: { tag: u8, pad: [u8;7], payload: u64 } = 16 bytes total
        let i8_ty = self.context.i8_type();
        let i64_ty = self.context.i64_type();
        let tagged_ty = self.context.struct_type(&[i8_ty.into(), i8_ty.array_type(7).into(), i64_ty.into()], false);
        // Place the scratch slot at the TOP of the function entry block, NOT at the current insert
        // point. This TaggedVal is a fixed-size, immediately-consumed temporary (lin_map_set/… copy
        // the value out), so one entry-block slot can be safely reused every iteration. Emitting it
        // at the current position would, on an inlined-combinator loop body (the Layer-1 capturing-
        // lambda inline path), allocate one stack slot PER iteration — `alloca` outside the entry
        // block is not reclaimed until function return — and overflow the stack at scale (the
        // map_flat_scalar segfault). Mirrors the home-alloca hoist in mod.rs.
        let alloca = self.entry_block_alloca(tagged_ty, "tv");

        let tag = Self::type_tag(val_ty);
        let tag_val = i8_ty.const_int(tag as u64, false);
        let tag_ptr = self.builder.struct_gep(tagged_ty, alloca, 0, "tv_tag");
        self.builder.store(tag_ptr, tag_val);

        // Write payload as u64.
        let payload_ptr = self.builder.struct_gep(tagged_ty, alloca, 2, "tv_payload");
        let payload = self.tagged_payload_i64(val, val_ty);
        self.builder.store(payload_ptr, payload);
        alloca
    }

    /// Emit an `alloca` at the TOP of the current function's entry block (not the current insert
    /// point), then restore the builder to its previous position. Use for fixed-size scratch slots
    /// that are written/consumed immediately each time, so a SINGLE entry-block slot is reused for
    /// the whole function rather than leaking one stack slot per loop iteration (an `alloca` in a
    /// loop body is never reclaimed until the function returns — at scale that overflows the stack).
    /// Mirrors the home-alloca hoist in `mod.rs`.
    pub(crate) fn entry_block_alloca<T: inkwell::types::BasicType<'ctx>>(
        &self,
        ty: T,
        name: &str,
    ) -> PointerValue<'ctx> {
        let cur_block = self.builder.get_insert_block().unwrap();
        let llvm_fn = cur_block.get_parent().unwrap();
        let entry_bb = llvm_fn.get_first_basic_block().unwrap();
        match entry_bb.get_first_instruction() {
            Some(first) => self.builder.position_before(&first),
            None => self.builder.position_at_end(entry_bb),
        }
        let slot = self.builder.alloca(ty, name);
        self.builder.position_at_end(cur_block);
        slot
    }

    /// KEEP-PACKED box (repr pass Stage 4): wrap a still-packed `LinArray*` (elem_tag 0xFE) / packed
    /// sealed struct* into a 16-byte `TaggedVal` WITHOUT materializing. O(1), borrows the inner. A
    /// sealed ARRAY uses `lin_box_array` (TAG_ARRAY) and a sealed RECORD uses `lin_box_record`
    /// (TAG_RECORD) — both store the payload pointer verbatim; the runtime dispatches release/free on
    /// the header (`elem_tag` for arrays, the sealed offset-4 size for structs). The box shell is a
    /// fresh +1; the inner's owning reference is supplied by the surrounding container transfer.
    pub(crate) fn compile_ir_box_keep_packed(&mut self, val: BasicValueEnum<'ctx>, arr: bool) -> BasicValueEnum<'ctx> {
        if !val.is_pointer_value() {
            return val;
        }
        if arr {
            self.builder.call(self.rt.box_array, &[val.into()], "kp_boxarr")
                .try_as_basic_value().unwrap_basic()
        } else {
            // Phase 2: non-array keep-packed values are LinMap* (TAG_MAP).
            self.builder.call(self.rt.box_map, &[val.into()], "kp_boxmap")
                .try_as_basic_value().unwrap_basic()
        }
    }

    /// KEEP-PACKED unbox: tag-check + load the payload pointer as the still-packed `LinArray*` /
    /// packed struct*, then retain it (one shell +1). O(1), zero copy. Called DIRECTLY from
    /// `compile_ir_index` for a `{String: Sealed[]}` map-value read, where the slot is known to hold a
    /// still-packed buffer wrapped in a `TaggedVal` (written by `emit_map_set`'s keep-packed store), so
    /// reading the payload as a packed pointer is sound. The retain balances the `Release` the pass
    /// schedules at the read-back temp's last drop. (Not driven by any IR opcode — the never-emitted
    /// `UnboxKeepPacked` instruction was removed.)
    pub(crate) fn compile_ir_unbox_keep_packed(&mut self, val: BasicValueEnum<'ctx>, _arr: bool) -> BasicValueEnum<'ctx> {
        if !val.is_pointer_value() {
            return val;
        }
        let raw = self.builder.call(self.rt.unbox_ptr, &[val.into()], "kp_unbox")
            .try_as_basic_value().unwrap_basic();
        // Retain the packed buffer: the read-back temp is a fresh owner (+1) whose Release the pass
        // schedules. The inline retain increments the offset-0 refcount shared by LinArray and packed
        // sealed structs (both carry it at offset 0), so this is correct for either kind.
        if raw.is_pointer_value() {
            self.emit_rc_retain_inline(raw.into_pointer_value());
        }
        raw
    }

    /// KEEP-PACKED box of an unboxed sum value (`*SumNode`) into a record/object FIELD slot
    /// (keep-packed-through-record-fields). Wraps the still-packed node by-pointer in a 16-byte
    /// `TaggedVal(TAG_SUMNODE)` — O(1), no `lin_summat` materialize. The DISTINCT tag is what makes
    /// this sound: the slot's release routes to `lin_sumnode_release_self` (the node's own size), not
    /// `lin_map_release`. Borrows the inner (shell is +1); the slot's owning reference comes from
    /// the IR `transfer_into_container`. The read-back twin is `compile_ir_unbox_keep_sumnode`, which
    /// tag-checks before unwrapping — so a slot that was instead MATERIALIZED (TAG_MAP, the
    /// fallback / cross-thread / boundary path) reads back correctly too (runtime-tag dispatch removes
    /// the store/read static asymmetry entirely).
    pub(crate) fn compile_ir_box_keep_sumnode(&mut self, val: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        if !val.is_pointer_value() {
            return val;
        }
        self.builder.call(self.rt.box_sumnode, &[val.into()], "kp_boxsum")
            .try_as_basic_value().unwrap_basic()
    }

    /// Box a raw `LinMap*` pointer directly as TAG_MAP. Replaces the pattern
    /// `box_value(obj, sumnode_first_variant_obj_ty(&sum_ty))` at dynamic-boundary sites
    /// where a materialized SumNode `LinMap*` needs wrapping into a TaggedVal.
    pub(crate) fn box_map_of(&mut self, obj_ptr: BasicValueEnum<'ctx>) -> BasicValueEnum<'ctx> {
        self.builder.call(self.rt.box_map, &[obj_ptr.into()], "boxmap_obj")
            .try_as_basic_value().unwrap_basic()
    }

    pub(crate) fn compile_ir_box(&mut self, val: BasicValueEnum<'ctx>, ty: &Type) -> BasicValueEnum<'ctx> {
        // Heap-box (see compile_ir_coerce) so the boxed value can safely escape.
        self.box_value(val, ty)
    }

    pub(crate) fn compile_ir_unbox(&mut self, val: BasicValueEnum<'ctx>, result_ty: &Type) -> BasicValueEnum<'ctx> {
        self.unbox_tagged_val_to_type(val, result_ty)
    }

    /// Unbox a tagged union value to a concrete type.
    pub(crate) fn unbox_tagged_val_to_type(&mut self, tagged: BasicValueEnum<'ctx>, ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        if !tagged.is_pointer_value() { return tagged; }
        let ptr = tagged.into_pointer_value();
        #[allow(unreachable_patterns)] // _ arm is a future-proof guard; currently exhaustive
        match ty {
            Type::Int8 | Type::Int16 | Type::Int32 | Type::IntLit(_) => {
                // Boxed as TAG_INT32 (sign-extended at box time). Read i32 and truncate to the
                // target width (a no-op bitcast for Int32). Without the Int8/Int16 arms these fell
                // through to `_ => tagged`, leaking the raw box pointer where a narrow scalar was
                // expected — the gate-divergence inline path's `for(x => push(buf, x))` over a
                // `UInt8[]`/`Int8[]` with a Json-typed lambda param (codegen signature mismatch).
                // IntLit(_) is Int32 at runtime; unbox the same way.
                let v = self.builder.call(self.rt.unbox_int32, &[ptr.into()], "ir_i32").try_as_basic_value().unwrap_basic().into_int_value();
                let ity = self.llvm_type(ty).into_int_type();
                self.builder.int_truncate_or_bit_cast(v, ity, "ir_inarrow").into()
            }
            Type::UInt8 | Type::UInt16 | Type::UInt32 => {
                // Boxed as TAG_INT64 (zero-extended); read i64 and truncate to the target width.
                let v = self.builder.call(self.rt.unbox_int64, &[ptr.into()], "ir_u_64").try_as_basic_value().unwrap_basic().into_int_value();
                let ity = self.llvm_type(ty).into_int_type();
                self.builder.int_truncate_or_bit_cast(v, ity, "ir_unarrow").into()
            }
            Type::Int64 => {
                self.builder.call(self.rt.unbox_int64, &[ptr.into()], "ir_i64").try_as_basic_value().unwrap_basic()
            }
            Type::UInt64 => {
                self.builder.call(self.rt.unbox_uint64, &[ptr.into()], "ir_u64").try_as_basic_value().unwrap_basic()
            }
            Type::Float64 | Type::Float32 => {
                let v = self.builder.call(self.rt.unbox_float64, &[ptr.into()], "ir_uf64").try_as_basic_value().unwrap_basic();
                if matches!(ty, Type::Float32) {
                    self.builder.float_trunc(v.into_float_value(), self.context.f32_type(), "tof32").into()
                } else {
                    v
                }
            }
            Type::Bool => {
                let i8v = self.builder.call(self.rt.unbox_bool, &[ptr.into()], "ir_ubool").try_as_basic_value().unwrap_basic().into_int_value();
                self.builder.int_truncate_or_bit_cast(i8v, self.context.bool_type(), "ub_bool").into()
            }
            Type::Str | Type::StrLit(_) => {
                self.builder.call(self.rt.unbox_ptr, &[ptr.into()], "ir_ustr").try_as_basic_value().unwrap_basic()
            }
            // Unboxing a boxed Json/object into a SEALED scalar record target = a PROJECTION:
            // the boxed value is a TaggedVal* (or raw LinMap*); project it into a fresh sealed
            // struct. Routed through the central projection helper so the source representation is
            // handled correctly (it unboxes a union box to the raw LinMap internally).
            Type::Object { .. } if Self::sealed_scalar_fields(ty).is_some() => {
                let fields = Self::sealed_scalar_fields(ty).unwrap().clone();
                // The incoming `tagged` is a boxed value (Json). Use the union-typed projection
                // path: sealed_project_from unboxes a union source to the raw LinMap itself.
                self.sealed_project_from(tagged, &Type::TypeVar(u32::MAX), &fields)
            }
            // Typed index-signature map (`{ String: T }`, ADR-055): a `m[k]` whose value type is
            // itself a Map is boxed as TAG_MAP. Unbox the payload back to the raw `LinMap*` so a
            // nested store (`m[a][b] = v`) and a chained read both operate on the SHARED inner
            // container, not on the TaggedVal box (which a missing arm here would leak through,
            // making nested-map mutation a no-op).
            Type::Array(_) | Type::FixedArray(_) | Type::Object { .. } | Type::Function { .. } | Type::Map { .. } => {
                self.builder.call(self.rt.unbox_ptr, &[ptr.into()], "ir_uptr").try_as_basic_value().unwrap_basic()
            }
            // KEEP-PACKED-THROUGH-RECORD-FIELDS read into UNION/Json position: a `sum | Null` result
            // (the safe-access `cur["node"] : Expr | Null` shape) stays a BOXED union value. If the slot
            // holds a keep-packed `TAG_SUMNODE`, MATERIALIZE it to a real TAG_MAP box so the dynamic
            // consumers (toString/eq/json, or a later `is`-narrowing match's `map_get`) see a real
            // LinMap — NOT a SumNode pointer they cannot interpret. A non-keep-packed box / null passes
            // through. (A subsequent narrow into a sum PARAM re-projects the materialized box → correct.)
            // This keeps the keep-packed STORE win while making EVERY read boundary correct + sound.
            Type::Union(_) if Self::sum_member_of_nullable_union(ty).is_some() => {
                let sum_ty = Self::sum_member_of_nullable_union(ty).unwrap();
                self.sumnode_box_readback_to_object_box(tagged, &sum_ty)
            }
            // UNBOXED SUM TYPE (unboxed-sumtype Stage 3): unboxing a boxed Json/object into a sum-typed
            // target is a PROJECTION back into a fresh `*SumNode` — the consumer (a SumNode param / a
            // `match` over the packed scrutinee / a recursive eval) requires the packed repr the type
            // implies, NOT the boxed LinMap. Without this arm the sum union fell to `_ => tagged`,
            // returning the boxed value where a SumNode was expected → garbage tag read. Reads the
            // discriminant + scalar/recursive-child fields from the boxed map (recursing for
            // children). This is the read-back twin of the `box_value` sum-materialize boundary above.
            _ if Self::is_sum_type(ty) => {
                // KEEP-PACKED-THROUGH-RECORD-FIELDS read-back: `sumnode_project_from_boxed` tag-dispatches
                // on the boxed value — a keep-packed `TAG_SUMNODE` (cursor zero-copy store) is unwrapped
                // to the still-packed `*SumNode` (+retain, zero copy); a materialized `TAG_MAP` is
                // projected into a fresh node. Sound with NO static store/read agreement. (`ty` is a
                // union here, so the tag probe is on a genuine box.)
                let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                self.sumnode_project_from_boxed(tagged, ty, ty, llvm_fn)
            }
            Type::Null => ptr_ty.const_null().into(),
            // Already-tagged values: the caller has a boxed TaggedVal* and the target type is
            // itself tagged — return the box unchanged. Includes generic unions, TypeVar (unknown
            // concrete type at compile time), opaque handle types (Shared/Stream/Promise/Opaque
            // whose runtime rep IS a tagged box), Named aliases, and Never (unreachable in
            // practice).
            Type::Union(_) | Type::TypeVar(_)
            | Type::Shared(_) | Type::Stream(_) | Type::Promise(_) | Type::Opaque(_)
            | Type::Named(_) | Type::Never => tagged,
            // Iterator<T> values materialise as a LinArray* (TAG_ARRAY) at the IR boundary;
            // unboxing to the raw pointer falls through here when Iterator is the stated target
            // type. Pass through unchanged — the pointer IS the value.
            Type::Iterator(_) => tagged,
            // Any Type variant that reaches here was not expected to be unboxed via this entry
            // point. Preserve old fall-through in release builds; panic in debug/test so the
            // corpus gate catches the gap immediately.
            _ => {
                debug_assert!(false, "unbox_tagged_val_to_type: unhandled type {ty:?} — add an explicit arm");
                tagged
            }
        }
    }

}