use super::builder_ext::BuilderExt;
use inkwell::values::BasicValueEnum;
use inkwell::{AddressSpace, IntPredicate};

use lin_check::types::Type;
use lin_ir::ir as lir;
use super::Codegen;

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
                let expected = self.type_tag_const(m);
                let eq = self.builder.int_compare(IntPredicate::EQ, tag, expected, "ir_is_member");
                acc = self.builder.or(acc, eq, "ir_is_union_acc");
            }
            return acc;
        }
        let expected = self.type_tag_const(ty);
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

    /// `is <ObjectType>` deep type validation (ADR-054). Emits the SAME schema descriptor the
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

    pub(crate) fn compile_ir_coerce(&mut self, val: BasicValueEnum<'ctx>, from_ty: &Type, to_ty: &Type) -> BasicValueEnum<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
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
            // MATERIALIZATION: a sealed struct → boxed LinObject (the Json/unsealed representation),
            // then box to a union/Json TaggedVal if the target is union/Json.
            if let Some(src_fields) = Self::sealed_scalar_fields(from_ty) {
                let sf = src_fields.clone();
                let obj = self.sealed_materialize_to_object(val, &sf);
                if Self::is_union_type(to_ty) {
                    // Box the fresh LinObject* as TAG_OBJECT. The +1 of the materialized object
                    // transfers into the box's inner; the box itself is +1 owned.
                    return self.box_value(obj, &Type::object(sf));
                }
                return obj;
            }
        }
        // ── Sealed-record ARRAY boundaries (sealed-records Stage 3) ───────────────────────
        // A `MyType[]` is a contiguous unboxed buffer (elem_tag 0xFE); a Json/Object[] is a tagged
        // array of boxed LinObjects. Crossing between them is a per-element PROJECTION /
        // MATERIALIZATION, not a pointer reinterpret. Handle BEFORE the generic union arms.
        let to_sealed_arr = Self::sealed_array_elem(to_ty).is_some();
        let from_sealed_arr = Self::sealed_array_elem(from_ty).is_some();
        if to_sealed_arr && !from_sealed_arr {
            // Wider/Json/Object[] source → fresh sealed-record array (each element projected).
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