use super::super::builder_ext::BuilderExt;
use inkwell::values::BasicValueEnum;
use inkwell::AddressSpace;
use lin_check::types::Type;
use super::super::Codegen;

impl<'ctx> Codegen<'ctx> {
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
    pub(crate) fn sealed_repr_differs(from: &Type, to: &Type) -> bool {
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
                self.builder.int_compare(inkwell::IntPredicate::EQ, av.into_int_value(), bv.into_int_value(), "sealed_ieq")
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
    pub(crate) fn null_value_for(&mut self, result_ty: &Type) -> BasicValueEnum<'ctx> {
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
    pub(crate) fn get_or_build_sumnode_materializer(&mut self, sum_ty: &Type) -> inkwell::values::FunctionValue<'ctx> {
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
    pub(crate) fn get_or_build_sumnode_projector(&mut self, sum_ty: &Type) -> inkwell::values::FunctionValue<'ctx> {
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
}
