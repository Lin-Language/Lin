use super::super::builder_ext::BuilderExt;
use inkwell::values::BasicValueEnum;
use inkwell::{AddressSpace, IntPredicate};
use lin_common::tags::{TAG_INT32, TAG_INT64, TAG_MAP, TAG_RECORD};
use lin_check::types::Type;
use super::super::Codegen;

impl<'ctx> Codegen<'ctx> {
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
            // string key → map get: dispatch on the container's tag (TAG_MAP only; no other producers).
            self.builder.position_at_end(str_b);
            let key_raw = self.builder.call(self.rt.unbox_ptr, &[key.into()], "ir_idxk_str").try_as_basic_value().unwrap_basic();
            let obj_tag = self.builder.call(self.rt.get_tag, &[obj.into()], "ir_idx_otag").try_as_basic_value().unwrap_basic().into_int_value();
            let is_map = self.builder.int_compare(IntPredicate::EQ, obj_tag, i8t.const_int(TAG_MAP as u64, false), "ir_idx_ismap");
            let mget_b = self.context.append_basic_block(llvm_fn, "ir_idx_mget");
            let onull_b = self.context.append_basic_block(llvm_fn, "ir_idx_onull");
            let omrg = self.context.append_basic_block(llvm_fn, "ir_idx_omrg");
            self.builder.conditional_branch(is_map, mget_b, onull_b);
            self.builder.position_at_end(mget_b);
            let mget = self.builder.call(self.rt.map_get, &[container.into(), key_raw.into()], "ir_idx_msget").try_as_basic_value().unwrap_basic();
            let mget_exit = self.builder.get_insert_block().unwrap();
            self.builder.unconditional_branch(omrg);
            self.builder.position_at_end(onull_b);
            self.builder.unconditional_branch(omrg);
            self.builder.position_at_end(omrg);
            let ophi = self.builder.phi(ptr_ty, "ir_idx_ophi");
            ophi.add_incoming(&[(&mget, mget_exit), (&ptr_ty.const_null(), onull_b)]);
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
            // a MATERIALIZED TAG_MAP (see `emit_map_set` — bare records materialize before store).
            // `lin_map_get` returns a borrowed TaggedVal*(TAG_MAP) for a hit, or null for a miss.
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
            // `LinMap` (TAG_MAP — see `emit_map_set`); the read-back returns the borrowed box for
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
        // by offset; reading it as a `LinMap` (the generic object path below) misinterprets the
        // packed bytes and crashes the runtime. The literal-key case is fused upstream (IR
        // lowering → constant-offset FieldGet / Null const); only a runtime key reaches here.
        // Materialize the sealed record to a fresh `LinMap` (its EXACTLY-declared fields — extras
        // already stripped) and do the normal dynamic `lin_map_get`, which returns the matching
        // value or Null for an absent key (safe-access §6.1). Clone the (borrowed, interior) result
        // into a fresh owned box and release the temporary map before returning, so nothing
        // dangles once the materialized map is freed.
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
        // Object key access. lin_map_get expects a raw *LinString key; unbox a boxed key.
        let key_str = if key_ty.is_string_ish() {
            key
        } else if Self::is_union_type(key_ty) && key.is_pointer_value() {
            self.builder.call(self.rt.unbox_ptr, &[key.into()], "ir_key_unbox").try_as_basic_value().unwrap_basic()
        } else {
            key
        };
        // When the object is statically Json/union, its runtime value may NOT be an object
        // (e.g. `results["type"]` where results is actually an array). Guard the lookup with
        // a tag check — dispatch on TAG_MAP/TAG_RECORD; otherwise return Null. Without this,
        // lin_map_get would read a LinArray*/scalar as a LinMap* and crash. Mirrors the
        // AST compile_index string-key-on-Json path.
        if Self::is_union_type(obj_ty) {
            // A `{ String: T } | Null` index (e.g. the inner read of a NESTED typed map
            // `outer[a][b]`, where `outer[a]` is `{ String: T } | Null` and is NOT spellable as
            // an `is`-pattern to narrow, ADR-055 §5.1.1) runs through this union path. Its runtime
            // value is a TAG_MAP, so dispatch on the tag: TAG_MAP → `lin_map_get` (O(1) hashed),
            // TAG_RECORD → `lin_record_get_field` (descriptor-driven field read, Stage 6a),
            // otherwise Null.
            //
            // OWNERSHIP CONTRACT:
            //   map_get returns a BORROWED `*const TaggedVal` (interior pointer into
            //   the container); `unbox_tagged_val_to_type` reads it without retaining.
            //   lin_record_get_field returns an OWNED `+1 *mut TaggedVal` (a heap-allocated box
            //   that owns a retain on heap-typed fields). To normalise ownership, the TAG_RECORD
            //   arm unboxes the owned box and then releases it, producing a final result that
            //   matches the borrowed-equivalent semantics of the other arms.
            //
            //   Control-flow structure (two-level merge):
            //   entry → is_map? → map_b → inner_mrg
            //         → is_rec? → rec_b → (unbox, release) → final_mrg
            //         → no → inner_mrg (null result)
            //   inner_mrg → (unbox) → final_mrg
            //   final_mrg holds the final unboxed value of the target result type.
            let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
            let obj_tag = self.builder.call(self.rt.get_tag, &[obj.into()], "ir_idx_tag").try_as_basic_value().unwrap_basic().into_int_value();
            let i8t = self.context.i8_type();
            let is_map = self.builder.int_compare(
                IntPredicate::EQ, obj_tag, i8t.const_int(TAG_MAP as u64, false), "ir_idx_is_map");
            let is_record = self.builder.int_compare(
                IntPredicate::EQ, obj_tag, i8t.const_int(TAG_RECORD as u64, false), "ir_idx_is_rec");
            let map_b = self.context.append_basic_block(llvm_fn, "ir_idx_map");
            let chk_rec = self.context.append_basic_block(llvm_fn, "ir_idx_chk_rec");
            let rec_b = self.context.append_basic_block(llvm_fn, "ir_idx_rec");
            let no = self.context.append_basic_block(llvm_fn, "ir_idx_obj_no");
            let inner_mrg = self.context.append_basic_block(llvm_fn, "ir_idx_inner_mrg"); // map/null
            let final_mrg = self.context.append_basic_block(llvm_fn, "ir_idx_final_mrg"); // all paths
            self.builder.conditional_branch(is_map, map_b, chk_rec);
            self.builder.position_at_end(map_b);
            let map_entry = self.builder.call(self.rt.map_get, &[container.into(), key_str.into()], "ir_mget_u").try_as_basic_value().unwrap_basic();
            let map_exit = self.builder.get_insert_block().unwrap();
            self.builder.unconditional_branch(inner_mrg);
            self.builder.position_at_end(chk_rec);
            self.builder.conditional_branch(is_record, rec_b, no);
            self.builder.position_at_end(no);
            let null_res = ptr_ty.const_null();
            self.builder.unconditional_branch(inner_mrg);
            // inner_mrg: collect borrowed TaggedVal* from map/null paths, unbox, branch to final.
            self.builder.position_at_end(inner_mrg);
            let inner_phi = self.builder.phi(ptr_ty, "ir_idx_inner_phi");
            inner_phi.add_incoming(&[(&map_entry, map_exit), (&null_res, no)]);
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
        // in a 16-byte TaggedVal (`compile_ir_box_keep_packed`, TAG_ARRAY / TAG_RECORD) — O(1), NO
        // `sealed_array_to_tagged` materialize (the O(n) copy that crashed on read-back). `lin_map_set`
        // copies the 16 bytes inline and retains the inner; the shell is freed after. The read-back
        // (`compile_ir_index` Map arm) unboxes it as a packed pointer (`compile_ir_unbox_keep_packed`)
        // feeding SealedArrayFieldGet zero-copy. Always sound: the runtime dispatches release/free on
        // the buffer's `elem_tag` / sealed header, regardless of being wrapped in a TaggedVal slot.
        // (Driven by these helper calls directly; the matching IR opcodes were never emitted and were
        // removed.)
        // UNBOXED SUM TYPE: a SumNode value stored into a `{ String: Expr }` map is MATERIALIZED to a
        // boxed `LinMap` (TAG_MAP) — the universal Json representation the map slot and the boxed
        // `Expr | Null` read-back expect. The materialized map is +1 owned; `lin_map_set` retains
        // the inner into the slot, so we release the transient box after.
        //
        // KEEP-PACKED-BY-POINTER for the Map value slot is DEFERRED (the TAG_SUMNODE runtime substrate
        // + codegen helpers are in place for it): the IR LOWERING `CloneBox`es the union-typed `m[k]`
        // result and the consumer's match-discriminator reads the boxed value via `map_get` / `is`
        // (`compile_ir_index` union arm) assuming a `LinMap` — a keep-packed `TAG_SUMNODE` slot read
        // by that borrowed-interior discriminator path is a type-confusion deref. Enabling it needs the
        // lowering/repr STEP-4 (suppress the project Coerce + teach the discriminator to materialize a
        // TAG_SUMNODE scrutinee), out of this change's scope.
        if value.is_pointer_value() {
            if let Some(sum_ty) = val_repr.sumnode_sum_ty() {
                let sum_ty = sum_ty.clone();
                let llvm_fn = self.builder.get_insert_block().unwrap().get_parent().unwrap();
                let obj = self.sumnode_materialize_to_object(value, &sum_ty, llvm_fn);
                let boxed = self.box_map_of(obj);
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
            // A bare packed sealed RECORD is NOT a `LinMap`: wrapping it in TAG_RECORD type-
            // confuses every boxed read-back (`m[k]` is `T | Null`, a union — the general read path
            // hands the payload to `lin_map_get`, which reads sealed bytes as a LinMap header:
            // the index-cap underflow crash / silent corruption). Bare records fall through to the
            // GENERAL path below: `box_value` MATERIALIZES the sealed struct to a fresh boxed
            // `LinMap` (`sealed_materialize_to_map`), the slot owns the fresh box, and the
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

    /// `object[key] = value` for the IR path. Mirrors the AST `compile_index_set`:
    /// dispatch on the object's static type; for Json/union objects, dispatch at
    /// runtime on the key's tag (int key ⇒ array set, string key ⇒ map set),
    /// unboxing the boxed container first. Stores go through the shared `emit_map_set`/
    /// `emit_array_set` helpers so the boxing/retain/release sequence is IDENTICAL to the
    /// `lin_map_set`/`lin_array_set` intrinsics; the matching IR-level ownership transfer
    /// is emitted in `IndexSet` lowering (`lin-ir`).
    pub(crate) fn compile_ir_index_set(&mut self, obj: BasicValueEnum<'ctx>, key: BasicValueEnum<'ctx>, value: BasicValueEnum<'ctx>, obj_ty: &Type, key_ty: &Type, val_ty: &Type, val_repr: &lin_ir::repr::Repr) {
        // Resolve an object key to a raw `LinString*`. A string key that is a callback param
        // arrives boxed (a `TaggedVal*`); unbox it, or `lin_map_set` reads the box as a
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
            // Cluster D: sealed Object / Named dynamic-key write — route through lin_map_set.
            // Sealed records with a dynamic (non-literal) key are a rare fallthrough path;
            // the lowerer redirects all compile-time-literal keys to FieldSet before this.
            // All dynamic objects are TAG_MAP (LinMap*); route dynamic-key writes through lin_map_set.
            Type::Object { .. } | Type::Named(_) => {
                if obj.is_pointer_value() && key.is_pointer_value() {
                    let key_str = resolve_obj_key(self, key);
                    self.emit_map_set(obj, key_str, value, val_ty, &Type::TypeVar(u32::MAX), &lin_ir::repr::Repr::boxed_opaque());
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

    /// String-keyed store into a union/`T|Null` container that may hold a Json object/map (TAG_MAP)
    /// or sealed record (TAG_RECORD). This is the write analogue of the tag-dispatched read in
    /// `compile_ir_index`: a NESTED typed map's inner write (`outer[a][b] = v`, where `outer[a]`
    /// is `{ String: T } | Null` — not `is`-narrowable, ADR-055 §5.1.1) reaches here with `obj_ty`
    /// a union containing a `Map(elem)` variant. When such a variant is present, dispatch on the
    /// runtime tag: TAG_MAP → `emit_map_set` (O(1) hashed insert). Both branches RETAIN the inner
    /// payload, so the ownership contract is identical on either branch. With no Map variant this is
    /// a plain `emit_map_set` (no extra branch emitted).
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
        // (TAG_MAP) or a sealed record (TAG_RECORD).
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
            // No Map variant and not Json-dynamic: route through lin_map_set.
            self.emit_map_set(container, key_str, value, val_ty, &Type::TypeVar(u32::MAX), &lin_ir::repr::Repr::boxed_opaque());
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
        // Non-TAG_MAP fallback (e.g. TAG_RECORD): route through map_set for uniformity.
        self.builder.position_at_end(obj_b);
        self.emit_map_set(container, key_str, value, val_ty, &elem, &lin_ir::repr::Repr::boxed_opaque());
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

    /// Load `field` from a sealed record at its constant byte offset — THE win: a single typed load,
    /// no `lin_map_get` call / hash lookup / unbox. `obj` is the struct ptr. For a HEAP field the
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
    /// as a boxed `lin_map_set`.
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
        let actual = self.builder.select(is_neg, wrapped, idx, "sarr_idx_actual").into_int_value();
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
    /// heap `LinMap` (the boxed `Token[]` representation — a record with heap fields, NOT a packed
    /// sealed-scalar array). Reads the BORROWED element box via `lin_array_get` (a `*TaggedVal*`
    /// interior pointer — no fresh box alloc, no element release), unboxes to the raw `LinMap*`,
    /// does the SINGLE `lin_map_get` for `field`, then unboxes/coerces to `result_ty`. The
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
        // Array elements of object type are TAG_MAP. Tag-dispatch to distinguish TAG_MAP from
        // other tags (null, scalars) and return Null for non-map elements.
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
        let no_bb = self.context.append_basic_block(llvm_fn, "bafg_no");
        let merge_bb = self.context.append_basic_block(llvm_fn, "bafg_merge");
        self.builder.conditional_branch(is_map_tag, map_bb, no_bb);
        // TAG_MAP branch: use map_get
        self.builder.position_at_end(map_bb);
        let map_tagged = self.builder.call(self.rt.map_get, &[inner_obj.into(), key_str.into()], "bafg_mget")
            .try_as_basic_value().unwrap_basic();
        let map_pred = self.builder.get_insert_block().unwrap();
        self.builder.unconditional_branch(merge_bb);
        // Non-map fallback: return null.
        self.builder.position_at_end(no_bb);
        let no_tagged: BasicValueEnum<'ctx> = ptr_ty.const_null().into();
        let no_pred = self.builder.get_insert_block().unwrap();
        self.builder.unconditional_branch(merge_bb);
        // merge
        self.builder.position_at_end(merge_bb);
        let tagged_phi = self.builder.phi(ptr_ty, "bafg_phi");
        tagged_phi.add_incoming(&[(&map_tagged, map_pred), (&no_tagged, no_pred)]);
        let tagged = tagged_phi.as_basic_value();
        // Coerce the (borrowed interior) field value to the field's declared type. The lowerer
        // registers `dst` owned and retains for an RC result, so this borrowed read is balanced.
        self.unbox_tagged_val_to_type(tagged, result_ty)
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
            // `lin_map_get` missing-key → Null path: produce the Null result for `result_ty`.
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
            // A Json/union object arrives as a boxed TaggedVal* whose runtime value may be a
            // TAG_MAP or TAG_RECORD (e.g. a sealed record laundered through AnyVal, then read by a
            // `match has { "type": ... }` discriminant FieldGet). A blind `lin_map_get` on the
            // unboxed pointer treats a sealed struct as a LinMap and crashes (`find_slot_string`
            // on garbage). Delegate to the Index path, which tag-dispatches map/record exactly like
            // `obj[field]` does. The packed-struct / sumnode / nullable-record reprs were already
            // handled by the early returns above, so `obj_repr` here is the boxed dynamic case
            // those Index branches correctly skip.
            if Self::is_union_type(obj_ty) {
                let key_str = self.compile_string_lit(field);
                return self.compile_ir_index(obj, key_str, obj_ty, &Type::Str, result_ty, obj_repr);
            }
            let key_str = self.compile_string_lit(field).into_pointer_value();
            // Stage 6b Phase 2: concrete (non-union) open-object container is a LinMap*; use map_get.
            let container = obj;
            let tagged = self.builder.call(self.rt.map_get, &[container.into(), key_str.into()], "ir_fget_m").try_as_basic_value().unwrap_basic();
            // No string_release: `compile_string_lit` returns an interned, immortal
            // LinString (refcount == IMMORTAL_RC), so the release is a runtime no-op
            // — but still an emitted call, hit on every typed field read. Drop it.
            //
            // KEEP-PACKED-THROUGH-RECORD-FIELDS read-back: when the field's declared result type is a
            // sum type, the slot MAY hold a keep-packed `TaggedVal(TAG_SUMNODE)` (the BoxKeepSumnode
            // store — the interp cursor `{node,pos}["node"]` zero-copy path) OR a MATERIALIZED
            // `TAG_MAP` (the cross-thread / boundary / fallback path). Dispatch on the RUNTIME TAG so
            // both are correct WITHOUT a static store/read agreement: TAG_SUMNODE → unwrap the still-
            // packed `*SumNode` (+retain), zero copy; otherwise → project the boxed LinMap into a fresh
            // node (the historical path). This runtime-tag dispatch is what makes the optimization sound
            // (no asymmetric keep-packed/materialize decision). A `sum | Null` read is handled the same
            // — a null payload tags as TAG_NULL → the project/unwrap both yield a null node.
            self.unbox_tagged_val_to_type(tagged, result_ty)
        } else { ptr_ty.const_null().into() }
    }

    /// Keep-packed read-back of a sum field into UNION/Json (type-erased) position: the result must
    /// remain a BOXED `TaggedVal*` for the union ABI and be correct for every dynamic consumer
    /// (toString/eq/json). If the slot holds a keep-packed `TAG_SUMNODE`, MATERIALIZE the still-packed
    /// `*SumNode` to a real boxed `LinMap` (TAG_MAP) so the type-erased consumers see an object,
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
