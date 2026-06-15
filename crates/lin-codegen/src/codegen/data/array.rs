use super::super::builder_ext::BuilderExt;
use inkwell::types::BasicType;
use inkwell::values::BasicValueEnum;
use inkwell::IntPredicate;
use lin_check::types::Type;
use super::super::Codegen;

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
        // array is a packed struct pointer, NOT a boxed LinMap. Storing it raw would type-confuse
        // read-back. Materialize it to a fresh LinMap first, then store that pointer under TAG_MAP —
        // the representation the tagged slot (and toString / index-get) expects.
        // Generic `push$Object` / `set` into `Field[]`.
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
                // type_tag_open: all object types now resolve to TAG_MAP (no TAG_OBJECT producers).
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
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
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
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let i64_ty = self.context.i64_type();
        let i8_ty = self.context.i8_type();
        let void_ty = self.context.void_type();
        // A SEALED-repr record value (`{id:String, dep:Int32, …}`) being set into a TAGGED `Object[]`
        // is a PACKED struct pointer, NOT a boxed LinMap. `build_tagged_val_alloca` would tag it
        // TAG_RECORD with the raw struct pointer as payload — the runtime then reads the packed bytes
        // as a LinMap header on read-back (heap-buffer-overflow / misaligned deref). Materialize it
        // to a fresh LinMap first (heap fields retained into the new map), then store that pointer
        // under TAG_MAP — the SAME representation `tagged_array_push_value` stores for the
        // `push$Object` case. The materialized map is a fresh +1 whose reference moves into the
        // array slot (`lin_array_set` raw-copies the 16-byte TaggedVal without an inner retain for a
        // tagged array), so it is NOT released here; the source struct keeps its own ownership (the IR
        // `ArraySetDyn` transfer leaves it owned, released at scope exit, dropping its heap fields).
        // Without this, `set(boxedSealedArr, i, {…})` crashed (ASan heap-buffer-overflow); the
        // boxed-array set is the index-set analogue of the boxed-array push fix.
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

    pub(crate) fn sealed_array_project_from(&mut self, src: BasicValueEnum<'ctx>, src_ty: &Type, arr_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
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
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
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
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
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
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
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
        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
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
}
