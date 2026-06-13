use super::builder_ext::BuilderExt;
use inkwell::values::BasicValueEnum;
use inkwell::{AddressSpace, IntPredicate};

use lin_check::types::Type;
use lin_ir::ir as lir;
use super::Codegen;
use lin_common::tags::{TAG_OBJECT, TAG_RECORD};

impl<'ctx> Codegen<'ctx> {
    pub(crate) fn compile_ir_is_type(&mut self, val: BasicValueEnum<'ctx>, ty: &Type) -> inkwell::values::IntValue<'ctx> {
        let bool_ty = self.context.bool_type();
        // `is <Json wildcard or unresolved generic TypeVar>`: after monomorphization an `is T`
        // whose `T` resolved to the Json wildcard (or stayed a generically-erased TypeVar) means
        // "the type is unknown / Json-erased". A boxed value of any tag conforms to Json, so the
        // sound answer is "always true" — NOT the historical 0xFF sentinel that matched nothing
        // (the silent-wrong-result bug for `is <type-variable>`). A non-pointer (unboxed scalar)
        // still cannot be a Json box here, so it stays false.
        if matches!(ty, Type::TypeVar(_)) {
            return if val.is_pointer_value() {
                bool_ty.const_int(1, false)
            } else {
                bool_ty.const_zero()
            };
        }
        if !val.is_pointer_value() {
            return bool_ty.const_zero();
        }
        let tag = self
            .builder
            .call(self.rt.get_tag, &[val.into()], "ir_tag")
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        // `is <Union>` (e.g. a generic `is T` specialized to `Int32 | String`): match if the
        // runtime tag equals ANY member's tag. Members that are not a recognised scalar tag
        // (nested unions / TypeVars) are skipped here — a nested union is flattened by the
        // checker, and a Json-erased member is the wildcard handled above.
        if let Type::Union(members) = ty {
            let mut acc = bool_ty.const_zero();
            for m in members {
                if matches!(m, Type::TypeVar(_) | Type::Union(_)) {
                    continue;
                }
                let eq = self.compile_ir_is_type_single(tag, m);
                acc = self.builder.or(acc, eq, "ir_is_union_acc");
            }
            return acc;
        }
        self.compile_ir_is_type_single(tag, ty)
    }

    /// Test a single concrete type `ty` against an already-extracted tag integer. Returns an i1.
    /// Handles the Stage-6a dual-tag case: sealed Object types can appear as TAG_OBJECT (when
    /// materialized as a LinObject) OR TAG_RECORD (when boxed via `lin_box_record` in a union
    /// slot) — both are valid runtime representations of the same Lin type.
    fn compile_ir_is_type_single(&mut self, tag: inkwell::values::IntValue<'ctx>, ty: &Type) -> inkwell::values::IntValue<'ctx> {
        let bool_ty = self.context.bool_type();
        let i8_ty = self.context.i8_type();
        // Stage 6a: a sealed record type can be TAG_OBJECT (materialized LinObject, the pre-6a
        // path) OR TAG_RECORD (wrapped sealed struct, the 6a path). Accept both.
        if Self::sealed_fields(ty).is_some() {
            let eq_obj = self.builder.int_compare(
                IntPredicate::EQ, tag, i8_ty.const_int(TAG_OBJECT as u64, false), "ir_is_obj");
            let eq_rec = self.builder.int_compare(
                IntPredicate::EQ, tag, i8_ty.const_int(TAG_RECORD as u64, false), "ir_is_rec");
            return self.builder.or(eq_obj, eq_rec, "ir_is_seal");
        }
        let expected = i8_ty.const_int(Self::type_tag(ty) as u64, false);
        self.builder.int_compare(IntPredicate::EQ, tag, expected, "ir_is")
    }

    pub(crate) fn compile_ir_has_pattern(&mut self, val: BasicValueEnum<'ctx>, pattern: &lir::HasDesc) -> inkwell::values::IntValue<'ctx> {
        let bool_ty = self.context.bool_type();
        if !val.is_pointer_value() { return bool_ty.const_zero(); }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i8_ty = self.context.i8_type();
        // BRANCHLESS: lin_value_has_field does the tag check + unbox + presence test in the
        // runtime, returning 0 for null/non-object values. Emitting no LLVM branches keeps
        // this IR instruction within a single basic block (avoids out-of-order block
        // creation that breaks SSA dominance when used inside match arms).
        let has_fn = self.get_or_declare_fn("lin_value_has_field",
            i8_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
        let mut all_present = bool_ty.const_int(1, false);
        for field in &pattern.required_fields {
            let key_str = self.compile_string_lit(field).into_pointer_value();
            let has_i8 = self.builder.call(has_fn, &[val.into(), key_str.into()], "ir_has").try_as_basic_value().unwrap_basic().into_int_value();
            // No string_release: compile_string_lit returns an immortal interned key
            // (refcount == IMMORTAL_RC), so releasing it is a runtime no-op call. Drop it.
            let has_bool = self.builder.int_truncate_or_bit_cast(has_i8, bool_ty, "has_b");
            all_present = self.builder.and(all_present, has_bool, "has_acc");
        }
        all_present
    }

    /// `is <ObjectType>` deep type validation (ADR-036). Emits the SAME schema descriptor the
    /// `fromJson` path builds (`emit_from_json_descriptor`) and calls `lin_matches_schema(value,
    /// descriptor)`, which runs the `fromJson` structural walker and returns an `i8` bool (`1` iff
    /// `val` recursively conforms to `target`). `val` is a boxed `TaggedVal*`, borrowed (no
    /// ownership change). Branchless — one runtime call, single basic block — so it composes
    /// inside match-arm test blocks just like `compile_ir_has_pattern`.
    pub(crate) fn compile_ir_matches_schema(
        &mut self,
        val: BasicValueEnum<'ctx>,
        target: &Type,
        named_defs: &[(String, Type)],
    ) -> inkwell::values::IntValue<'ctx> {
        let bool_ty = self.context.bool_type();
        if !val.is_pointer_value() {
            return bool_ty.const_zero();
        }
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i8_ty = self.context.i8_type();
        let desc_ptr = self.emit_from_json_descriptor(target, named_defs);
        let matches_fn = self.get_or_declare_fn(
            "lin_matches_schema",
            i8_ty.fn_type(&[ptr_ty.into(), ptr_ty.into()], false),
        );
        let r_i8 = self
            .builder
            .call(matches_fn, &[val.into(), desc_ptr.into()], "ir_matches_schema")
            .try_as_basic_value()
            .unwrap_basic()
            .into_int_value();
        self.builder.int_truncate_or_bit_cast(r_i8, bool_ty, "matches_b")
    }

    /// Repr-aware Coerce entry (unboxed-sumtype Stage 1 — the CALL ABI boundary machinery). When the
    /// SOURCE operand is physically an unboxed `SumNode` (its repr is `Packed(SumNode)`, proven by the
    /// repr pass + verify), a Coerce out of the sum type must use the `sumnode_*` helpers, NOT the
    /// type-driven `compile_ir_coerce` boxed path (which would read the SumNode pointer as a boxed
    /// `TaggedVal` → UAF). Three directions:
    ///   - sum → SAME sum type: a no-op carry (same SumNode pointer).
    ///   - sum → a concrete VARIANT record (the `match`-arm narrowing): project to a sealed struct.
    ///   - sum → Json / union / generic / anything else: materialize the node to a boxed `LinObject`,
    ///     then box to a union/Json `TaggedVal` if the target is union/Json.
    /// All other coercions (numeric, sealed-record, array, boxed sources) delegate to the existing
    /// type-driven `compile_ir_coerce`.
    pub(crate) fn compile_ir_coerce_with_repr(
        &mut self,
        val: BasicValueEnum<'ctx>,
        from_ty: &Type,
        to_ty: &Type,
        src_repr: &lin_ir::repr::Repr,
        llvm_fn: inkwell::values::FunctionValue<'ctx>,
    ) -> BasicValueEnum<'ctx> {
        if let Some(sum_ty) = src_repr.sumnode_sum_ty() {
            let sum_ty = sum_ty.clone();
            // sum → SAME sum type (e.g. an identity widening, or a union-to-union no-op): carry the
            // SumNode pointer verbatim. The repr pass keeps both sides Packed(SumNode), so codegen
            // must not convert. (Identity by Type equality — Type ignores `sealed`.)
            if to_ty == &sum_ty {
                return val;
            }
            // sum → a concrete sealed VARIANT record (the match-arm narrowing Coerce, from_ty=sum,
            // to_ty=Circle). Project the node's scalar payload into a fresh packed sealed struct.
            if let Some(target_fields) = Self::sealed_scalar_fields(to_ty) {
                let tf = target_fields.clone();
                return self.sumnode_project_to_sealed(val, &sum_ty, &tf);
            }
            // sum → Json / union / generic / unsealed object: materialize to a boxed LinObject, then
            // box as TAG_OBJECT if the target is a union/Json wildcard.
            let obj = self.sumnode_materialize_to_object(val, &sum_ty, llvm_fn);
            if Self::is_union_type(to_ty) || matches!(to_ty, Type::TypeVar(_)) {
                return self.box_value(obj, &Self::sumnode_first_variant_obj_ty(&sum_ty));
            }
            return obj;
        }
        // REVERSE boundary: a BOXED / Json / unsealed-object / concrete-variant-record source coerced
        // INTO a sum type (`to_ty` is Stage-1-eligible) must build a fresh `SumNode` — the source is
        // NOT physically a SumNode (its repr is Boxed / a sealed variant struct). This is the
        // construction edge for `val c: Shape = { "kind": "circle", … }` (the literal is built boxed
        // then coerced to the sum slot) and for a Json value flowing into a sum-typed slot/param.
        // Reconstruct by reading the discriminant + scalar payload fields from the source.
        if Self::is_sum_type(to_ty) {
            // A source that is a concrete sealed VARIANT record (a packed struct) materializes to a
            // boxed object first so `sumnode_project_from_boxed` can read its fields uniformly.
            if Self::sealed_scalar_fields(from_ty).is_some() {
                if let Some(sf) = Self::sealed_scalar_fields(from_ty) {
                    let sf = sf.clone();
                    let obj = self.sealed_materialize_to_object(val, &sf);
                    let boxed = self.box_value(obj, &Type::object(sf));
                    let node = self.sumnode_project_from_boxed(boxed, &Type::TypeVar(u32::MAX), to_ty, llvm_fn);
                    // Release the transient boxed materialization (its inner object + shell).
                    if boxed.is_pointer_value() {
                        self.builder.call(self.rt.tagged_release, &[boxed.into()], "");
                    }
                    return node;
                }
            }
            // KEEP-PACKED-THROUGH-RECORD-FIELDS read-back is centralized in `sumnode_project_from_boxed`:
            // for a BOXED union/Json source it tag-dispatches (TAG_SUMNODE → unwrap the kept-packed
            // `*SumNode` zero-copy; TAG_OBJECT → project a fresh node), and for a raw `LinObject*` (a
            // match-narrowed, already-unboxed scrutinee — `src_ty` not a union) it projects directly.
            // So this single call is correct for BOTH the cursor read (`parsed["node"]` boxed, the
            // interp keep-packed fast path) AND the match-arm narrowing alike — with no static asymmetry.
            return self.sumnode_project_from_boxed(val, from_ty, to_ty, llvm_fn);
        }
        self.compile_ir_coerce(val, from_ty, to_ty)
    }

    pub(crate) fn compile_ir_coerce(&mut self, val: BasicValueEnum<'ctx>, from_ty: &Type, to_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        // NOTE (keep-packed-through-record-fields): a kept-packed `TAG_SUMNODE` value that escapes a
        // record field into the type-erased dynamic domain (toString/eq/json) is materialized in the
        // RUNTIME boundary walkers (`lin_tagged_to_string`/`push_json_value`/`lin_tagged_eq`, via the
        // per-type materializer in the SumNode's descriptor). No codegen-side guard is emitted here —
        // a sum-typed `from_ty` value reaching `compile_ir_coerce` may be a RAW `*SumNode` (not a box),
        // so a `get_tag` probe would misread it; the repr-aware `compile_ir_coerce_with_repr` already
        // handles a Packed(SumNode) source's sum→Json materialize before delegating here.
        // Numeric widening.
        if from_ty.is_numeric() && to_ty.is_numeric() {
            if val.is_int_value() && to_ty.is_float() {
                let iv = val.into_int_value();
                let ft = if matches!(to_ty, Type::Float32) { self.context.f32_type().into() } else { self.context.f64_type() };
                return self.builder.signed_int_to_float(iv, ft, "ir_i2f").into();
            }
            if val.is_float_value() && to_ty.is_integer() {
                let fv = val.into_float_value();
                let it = self.llvm_type(to_ty).into_int_type();
                return self.builder.float_to_signed_int(fv, it, "ir_f2i").into();
            }
            if val.is_float_value() && to_ty.is_float() {
                // Float ↔ float width change: fpext (Float32→Float64) or fptrunc
                // (Float64→Float32). Without this arm the value stayed at its source
                // width and the downstream call/store saw the wrong float type.
                let fv = val.into_float_value();
                let ft = self.llvm_type(to_ty).into_float_type();
                let from_bits = fv.get_type().get_bit_width();
                let to_bits = ft.get_bit_width();
                return if to_bits > from_bits {
                    self.builder.float_ext(fv, ft, "ir_fpext").into()
                } else if to_bits < from_bits {
                    self.builder.float_trunc(fv, ft, "ir_fptrunc").into()
                } else {
                    val
                };
            }
            if val.is_int_value() && to_ty.is_integer() {
                let iv = val.into_int_value();
                let it = self.llvm_type(to_ty).into_int_type();
                let from_bits = iv.get_type().get_bit_width();
                let to_bits = it.get_bit_width();
                return if to_bits > from_bits {
                    // Widen by the SOURCE type's signedness: a signed Int32 -1 (0xFFFFFFFF)
                    // must sign-extend to Int64 -1, not zero-extend to 4294967295. Using
                    // zero-extend unconditionally corrupted `val x: Int64 = 0 - 1`.
                    if from_ty.is_signed() {
                        self.builder.int_s_extend_or_bit_cast(iv, it, "ir_sext").into()
                    } else {
                        self.builder.int_z_extend_or_bit_cast(iv, it, "ir_zext").into()
                    }
                } else {
                    self.builder.int_truncate_or_bit_cast(iv, it, "ir_trunc").into()
                };
            }
            return val;
        }
        // NOTE (unboxed-sumtype Stage 1): the sum-type Coerce boundaries (sum→variant projection,
        // sum→Json materialize, Json→sum reconstruction) are implemented as codegen helpers
        // (`sumnode_project_to_sealed` / `sumnode_materialize_to_object` / `sumnode_project_from_boxed`)
        // but are NOT yet wired in here, because they must dispatch on the operand's REPR (proof the
        // value is physically a SumNode), not its TYPE — a tagged union's `Type` is `is_sum_type` true
        // even while its runtime repr is still boxed (the seed is inert pending the ABI; see
        // `repr::type_seed`). Gating on type here would route an existing boxed union through the
        // SumNode reader → UAF. Re-enable together with the repr seed + call ABI.
        // ── Sealed scalar-record boundaries (sealed-records Stage 1) ──────────────────────
        // Order matters: handle sealed→X and X→sealed BEFORE the generic union arms, because a
        // sealed Object is not `is_union_type` but DOES need a representation conversion.
        let from_sealed = Self::sealed_scalar_fields(from_ty).is_some();
        let to_sealed = Self::sealed_scalar_fields(to_ty).is_some();
        if to_sealed {
            // PROJECTION: a wider/Json/unsealed/other-sealed source → a fresh sealed struct.
            // Non-mutating; the source keeps its own ownership (released by its own scope).
            if let Some(target_fields) = Self::sealed_scalar_fields(to_ty) {
                // Clone so the borrow checker is happy (target_fields borrows self via to_ty).
                let tf = target_fields.clone();
                return self.sealed_project_from(val, from_ty, &tf);
            }
        }
        if from_sealed {
            // MATERIALIZATION: a sealed struct → dynamic/unsealed representation.
            if let Some(src_fields) = Self::sealed_scalar_fields(from_ty) {
                let sf = src_fields.clone();
                // Stage 6a: DIRECT O(1) wrap as TAG_RECORD for the sealed→Json/AnyVal Coerce.
                // Only applied when the target is EXACTLY the JSON/AnyVal type (TypeVar(u32::MAX)
                // or TypeVar in general), NOT for multi-variant Union types (which are synthetic
                // merge unions for if-branches and expect TAG_OBJECT from lin_unbox_ptr).
                //
                // The restriction is necessary because: multi-variant unions created by if-merge
                // phi nodes are immediately unboxed via `lin_unbox_ptr` + `Coerce(union → Object)`,
                // and `lin_unbox_ptr` for TAG_RECORD returns the sealed struct ptr (not LinObject*)
                // → crashes. Only genuine Json/AnyVal slots are fully TAG_RECORD-aware via tag dispatch.
                //
                // `lin_box_record` retains the sealed struct (bumps its RC at offset 0) and
                // returns a fresh +1 TaggedVal*(TAG_RECORD) wrapping the sealed ptr.
                // Runtime consumers (lin_tagged_eq/to_string/push_json_value/transfer/field-access)
                // all have TAG_RECORD arms that dispatch correctly via the named_desc at header offset 16.
                // This is the ONE flow the BRIEF targets: `val j: Json = p` → O(1) wrap, no copy.
                // Pre-6a was: sealed_materialize_to_object + box_value → TAG_OBJECT (O(n) copy).
                if matches!(to_ty, Type::TypeVar(_)) && val.is_pointer_value() {
                    return self.builder.call(self.rt.box_record, &[val.into()], "boxrec")
                        .try_as_basic_value().unwrap_basic();
                }
                if Self::is_union_type(to_ty) {
                    // sealed → multi-variant union (if-merge, named union type): materialize to
                    // LinObject for TAG_OBJECT. TAG_RECORD would be unboxed by lin_unbox_ptr on
                    // the phi result, returning a sealed ptr instead of LinObject* → crash.
                    let obj = self.sealed_materialize_to_object(val, &sf);
                    return self.box_value(obj, &Type::object(sf));
                }
                // sealed → unsealed object (non-union target): materialize to LinObject.
                let obj = self.sealed_materialize_to_object(val, &sf);
                return obj;
            }
        }
        // ── FLAT scalar ARRAY width/kind change (e.g. UInt8[] → Int32[]) ──────────────────
        // Two flat scalar arrays with DIFFERENT element types are physically different buffers:
        // each is stored at its element's native stride and tagged with that element kind. Binding
        // a `UInt8[]` value to an `Int32[]` slot reinterpreting the same pointer would read 4 source
        // bytes as one i32 on every indexed access. MATERIALIZE a fresh dest-strided buffer, widening
        // each element (sext/zext/sitofp/fpext via the numeric arm above). Same precedent as the
        // mixed-numeric array literal `[0, 3.14]` coercion (lower's `scalar_numeric_repr_differs`),
        // extended to whole arrays. Handle BEFORE the sealed-array / generic arms.
        if let (Type::Array(from_e), Type::Array(to_e)) = (from_ty, to_ty) {
            if Self::is_flat_scalar(from_e)
                && Self::is_flat_scalar(to_e)
                && from_e != to_e
                && val.is_pointer_value()
            {
                return self.flat_array_widen(val, from_e, to_e);
            }
        }
        // ── Sealed-record ARRAY boundaries (sealed-records Stage 3) ───────────────────────
        // A `MyType[]` is a contiguous unboxed buffer (elem_tag 0xFE); a Json/Object[] is a tagged
        // array of boxed LinObjects. Crossing between them is a per-element PROJECTION /
        // MATERIALIZATION, not a pointer reinterpret. Handle BEFORE the generic union arms.
        let to_sealed_arr = Self::sealed_array_elem(to_ty).is_some();
        let from_sealed_arr = Self::sealed_array_elem(from_ty).is_some();
        if to_sealed_arr && !from_sealed_arr {
            // Wider/Json/Object[] or union source → fresh sealed-record array (each element projected,
            // or a keep-packed O(1) retain when the union's inner is already 0xFD/0xFE).
            // `sealed_array_project_from` unboxes a union source to the raw LinArray*, then checks the
            // runtime elem_tag: 0xFD/0xFE → keep-packed (O(1) retain); otherwise → O(n) element
            // rebuild. This handles both `Trip[]|Null → Trip[]` (O(1)) and `Json → Item[]` (O(n)).
            return self.sealed_array_project_from(val, from_ty, to_ty);
        }
        if from_sealed_arr && !to_sealed_arr {
            // Sealed-record array → tagged Object[] (Json) view, then box if the target is union.
            let tagged = self.sealed_array_to_tagged(val, from_ty);
            if Self::is_union_type(to_ty) {
                return self.box_value(tagged, &Type::Array(Box::new(Type::object(Default::default()))));
            }
            return tagged;
        }
        // ── NESTED sealed-record array (Problem A / Stage 3b) ────────────────────────────────
        // A combinator returning a NESTED sealed structure — `partition: T[][]`, `groupBy: {String:
        // T[]}` (its map values), `chunk: T[][]` — routes through the type-erased boxed fallback,
        // so the boxed `Json` result must be re-projected into the sealed `to_ty`. The one-level
        // sealed-array arms above only fire when `to_ty` IS the sealed array; for an OUTER array
        // whose ELEMENTS contain a sealed array (or sealed record), recurse element-wise: rebuild a
        // tagged outer array, coercing each element from its boxed view into the inner sealed
        // representation. Without this the boxed inner `Json[]` is read as a packed `Pt[]` →
        // misaligned deref / double-free (the `partition`/`groupBy` crash).
        // Gate strictly on a REPRESENTATION CHANGE: only when the source's inner element is a BOXED
        // view (a Json/union/TypeVar — the type-erased boxed-fallback result) while `inner_to` is a
        // concrete sealed-containing structure. A verbatim same-representation array (e.g. building a
        // `Neighbor[]` literal, both sides boxed `Object[]`) must NOT be rebuilt — that would re-project
        // every element through the Json view and corrupt counts (observed: dijkstra `buildAdj`).
        if let (Type::Array(inner_to), Type::Array(inner_from)) = (to_ty, from_ty) {
            // Fire only when `inner_to` contains a sealed (packed) structure AND `inner_from` is NOT
            // that SAME packed representation — i.e. a genuine boxed→packed re-projection. A verbatim
            // same-representation array (a `Neighbor[]` literal: `inner_from == inner_to`) is a pointer
            // pass-through and must NOT be rebuilt (would corrupt counts — dijkstra `buildAdj`). The
            // boxed-fallback result is `Array(Array(TypeVar))`-shaped, whose inner differs from `Pt[]`.
            if Self::ty_contains_sealed(inner_to)
                && inner_from.as_ref() != inner_to.as_ref()
                && val.is_pointer_value()
            {
                return self.array_coerce_elementwise(val, from_ty, inner_to);
            }
        }
        // Json/Object → typed map `{ String: T }` (ADR-055). A value reaching a map-typed context
        // through the Json supertype — an empty object literal `{}`, a `Json` field read, a `Json`
        // parameter — is physically a `LinObject` (TAG_OBJECT), but the map accessors require a
        // `LinMap`. Reinterpreting the pointer corrupts the heap (the `lin_map_get`/`_set` crash:
        // `find_slot` probes a LinObject's bytes as a hash table → infinite-loop / invalid free).
        // MATERIALIZE a real map from the object's entries instead. Skip when the source is already
        // a map (`from_ty` is `Type::Map`): that is a verbatim pointer pass-through. The source may
        // arrive boxed (union/Json TaggedVal*) — unbox to the raw `LinObject*` first. The fresh map
        // is +1 owned (matching the `register_owned` the lowerer applies to a Coerce result).
        if matches!(to_ty, Type::Map(_)) && !matches!(from_ty, Type::Map(_)) && val.is_pointer_value() {
            // The runtime value may be EITHER a real `LinMap` (a `{ String: T }` value flowing
            // through the Json supertype — e.g. a nested map read as `T|Null`) or a `LinObject` (an
            // empty object literal, a Json object field). The two have incompatible layouts and the
            // raw pointer carries no tag, so dispatch on the BOXED value's tag at runtime via
            // `lin_to_map`: TAG_MAP → retain + return as-is (preserve identity, no copy); TAG_OBJECT
            // → materialize a fresh `LinMap`. A union source is already a boxed `TaggedVal*`; box a
            // concrete object source first so `lin_to_map` always has a tag to read.
            let boxed = if Self::is_union_type(from_ty) {
                val
            } else {
                self.box_value(val, from_ty)
            };
            let f = self.get_or_declare_fn("lin_to_map", ptr_ty.fn_type(&[ptr_ty.into()], false));
            let m = self.builder.call(f, &[boxed.into()], "to_map").try_as_basic_value().unwrap_basic();
            // If we boxed a concrete source just to read its tag, free that transient box shell
            // (its inner was not consumed by lin_to_map — the map took its own retains).
            if !Self::is_union_type(from_ty) && boxed.is_pointer_value() {
                let free_box = self.get_or_declare_fn("lin_tagged_free_box_if_distinct",
                    self.context.void_type().fn_type(&[ptr_ty.into(), ptr_ty.into()], false));
                self.builder.call(free_box, &[boxed.into(), m.into()], "");
            }
            return m;
        }
        // D3b: Unsealed Object widening into a NARROWER unsealed Object slot. Both are physically
        // LinObject*; the source has extra or different fields the slot does not. Project-copy into
        // a fresh LinObject with exactly to_ty's fields, severing sharing.
        if let (Type::Object { sealed: false, fields: to_fields }, Type::Object { sealed: false, .. }) = (to_ty, from_ty) {
            let tf = to_fields.clone();
            return self.boxed_object_project(val, &tf);
        }
        // Box to union. Use heap boxing (lin_box_*) rather than a stack alloca, because
        // a coerced value may escape its defining function (returned, stored in an array,
        // captured) — a stack TaggedVal would dangle.
        if Self::is_union_type(to_ty) {
            return self.box_value(val, from_ty);
        }
        // Unbox from union.
        if Self::is_union_type(from_ty) && val.is_pointer_value() {
            return self.unbox_tagged_val_to_type(val, to_ty);
        }
        let _ = (from_ty, to_ty);
        if val.get_type() == self.llvm_type(to_ty) { val } else { ptr_ty.const_null().into() }
    }

}