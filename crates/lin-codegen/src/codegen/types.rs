use super::builder_ext::BuilderExt;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::BasicValueEnum;
use inkwell::AddressSpace;

use lin_check::types::Type;
use lin_common::tags::*;
use super::Codegen;

impl<'ctx> Codegen<'ctx> {
    pub(crate) fn llvm_type(&self, ty: &Type) -> BasicTypeEnum<'ctx> {
        match ty {
            Type::Bool => self.context.bool_type().into(),
            Type::Int8 => self.context.i8_type().into(),
            Type::Int16 => self.context.i16_type().into(),
            Type::Int32 => self.context.i32_type().into(),
            Type::Int64 => self.context.i64_type().into(),
            Type::UInt8 => self.context.i8_type().into(),
            Type::UInt16 => self.context.i16_type().into(),
            Type::UInt32 => self.context.i32_type().into(),
            Type::UInt64 => self.context.i64_type().into(),
            Type::Float32 => self.context.f32_type().into(),
            Type::Float64 => self.context.f64_type().into(),
            Type::Str | Type::StrLit(_) => self.string_ptr_type.into(),
            // IntLit is Int32 at runtime — no boxing, same LLVM integer type.
            Type::IntLit(_) => self.context.i32_type().into(),
            Type::Null => {
                // Null is represented as a pointer (null ptr), same as Union/TypeVar.
                // This ensures Null-typed vars can hold tagged values assigned later.
                self.context.ptr_type(AddressSpace::default()).into()
            }
            Type::Array(_) | Type::FixedArray(_) => self.array_ptr_type.into(),
            // Stage 0.5: codegen IGNORES the `sealed` marker — every object, sealed or not, is the
            // boxed string-keyed `LinObject` pointer, exactly as before. Stage 1 will branch here.
            Type::Object { .. } => self.context.ptr_type(AddressSpace::default()).into(),
            // A typed index-signature map (`{ String: T }`, ADR-055) is a `LinMap*` — an opaque
            // pointer to the hashed container.
            Type::Map { .. } => self.context.ptr_type(AddressSpace::default()).into(),
            Type::Union(_) => {
                // Tagged union: { i8 tag, [8 x i8] payload } — 9 bytes total.
                // We use an opaque pointer to a heap-allocated tagged value.
                self.context.ptr_type(AddressSpace::default()).into()
            }
            Type::Function { .. } => {
                // Closures are represented as { fn_ptr, env_ptr } pairs.
                // Returns a pointer to the closure struct.
                self.context.ptr_type(AddressSpace::default()).into()
            }
            Type::Iterator(_) => self.context.ptr_type(AddressSpace::default()).into(),
            // Shared<T> is a boxed TaggedVal*(TAG_SHARED) at runtime — an opaque pointer.
            Type::Shared(_) => self.context.ptr_type(AddressSpace::default()).into(),
            // Stream<T> is a boxed TaggedVal*(TAG_STREAM) at runtime — an opaque pointer.
            Type::Stream(_) => self.context.ptr_type(AddressSpace::default()).into(),
            // Promise<T> is a boxed TaggedVal*(TAG_PROMISE) at runtime — an opaque pointer.
            Type::Promise(_) => self.context.ptr_type(AddressSpace::default()).into(),
            // TarEntry is a boxed TaggedVal*(TAG_TAR_ENTRY) at runtime — an opaque pointer.
            Type::TarEntry => self.context.ptr_type(AddressSpace::default()).into(),
            Type::Never => self.context.i8_type().into(), // unreachable
            Type::TypeVar(_) => {
                // Unresolved type var — use opaque pointer (Json/"any" type at runtime)
                self.context.ptr_type(AddressSpace::default()).into()
            }
            Type::Named(_) => {
                // Named recursive type reference — use opaque pointer (heap-allocated object)
                self.context.ptr_type(AddressSpace::default()).into()
            }
        }
    }

    pub(crate) fn llvm_param_type(&self, ty: &Type) -> BasicMetadataTypeEnum<'ctx> {
        self.llvm_type(ty).into()
    }

    /// True if `ty` is a union or TypeVar (i.e., needs tagged representation). `Shared<T>` is
    /// included: its runtime value is a boxed `TaggedVal*(TAG_SHARED)`, so box/unbox sites must
    /// treat it as an already-boxed tagged value (never re-box or reinterpret it as a scalar).
    pub(crate) fn is_union_type(ty: &Type) -> bool {
        matches!(ty, Type::Union(_) | Type::TypeVar(_) | Type::Named(_) | Type::Shared(_) | Type::Stream(_) | Type::Promise(_) | Type::TarEntry)
    }

    /// Stage 6a: Returns true if `ty` represents a value whose tagged-box payload is a HEAP POINTER
    /// (LinString*, LinArray*, LinObject*, LinMap*) — as opposed to a scalar (Int, Float, Bool,
    /// Null) whose payload is an integer or zero. Used to decide whether to retain the inner before
    /// releasing an owned box (e.g. a TAG_RECORD field-lookup result from `lin_record_get_field`).
    pub(crate) fn result_is_heap_pointer(ty: &Type) -> bool {
        matches!(ty, Type::Str | Type::StrLit(_) | Type::Array(_) | Type::FixedArray(_)
            | Type::Object { .. } | Type::Map { .. } | Type::Function { .. }
            | Type::Shared(_) | Type::Stream(_) | Type::Promise(_) | Type::TarEntry)
    }

    /// Returns the LLVM struct type for a closure header.
    ///
    /// Layout (32 bytes):
    ///   field 0: i32  refcount
    ///   field 1: i32  _pad
    ///   field 2: ptr  fn_ptr
    ///   field 3: ptr  env_ptr
    ///
    /// A trailing u64 env_size lives at offset 24 and is written directly via GEP on the
    /// raw allocation rather than as a struct field, because the closure struct type is
    /// referenced in many places and keeping it to 4 fields keeps all the call-site GEPs
    /// consistent.  The env_size write is done once at closure creation.
    pub(crate) fn closure_struct_type(&self) -> inkwell::types::StructType<'ctx> {
        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        let i32_ty = self.context.i32_type();
        self.context.struct_type(&[i32_ty.into(), i32_ty.into(), ptr_ty.into(), ptr_ty.into()], false)
    }

    /// Concrete (non-boxed) reference-counted heap types: the IR lowerer tracks these for
    /// scope-exit release (mirrors `lin_ir`'s `is_rc_type`), and a cell/global holding one
    /// owns an independent reference (the lowerer retains on store). Boxed Json/union/Named
    /// values are excluded: they follow the legacy borrow model where the value's true owner
    /// frees it, so a cell/global must NOT release them on reassignment (double-free).
    ///
    /// This MUST stay in lockstep with `lin_ir::lower::is_rc_type`: codegen releases the old
    /// value on reassignment only for types the lowerer also retained on store. A type present
    /// here but absent there would be released without a matching retain — a refcount underflow.
    /// (`Iterator` IS included: an `Iterator<T>` value is a freshly-materialised heap `LinArray`
    /// — `lin_range`/`lin_iter`/`iterOf` all allocate one with no borrowed alias — so it follows
    /// the same owning model as `Array`. Omitting it leaked every `range(...)`/combinator-iterator
    /// result, since the lowerer then never released it at scope exit.)
    pub(crate) fn ty_is_concrete_rc(ty: &Type) -> bool {
        matches!(
            ty,
            Type::Str
                | Type::StrLit(_)
                | Type::Array(_)
                | Type::FixedArray(_)
                | Type::Object { .. }
                | Type::Map { .. }
                | Type::Iterator(_)
                | Type::Function { .. }
        )
    }

    /// True when `ty` is a sealed (packed) record carrying at least one HEAP field (String / Array /
    /// Map / nested-sealed — anything `sealed_field_kind` recognises). Such a record is a heap-
    /// allocated `lin_sealed_*` struct whose header refcount + per-field heap pointers must be
    /// released (`lin_sealed_release` walks the heap fields). A PURELY-scalar sealed record, by
    /// contrast, holds no owned heap references — it is (or may be) a stack-resident immortal-rc
    /// value, so releasing it is at best a no-op (and may defeat SROA). This distinction is the
    /// basis of the TCO-param carve-out narrowing below.
    pub(crate) fn sealed_record_is_heap_bearing(ty: &Type) -> bool {
        match Self::sealed_fields(ty) {
            Some(fields) => fields.values().any(|fty| Self::sealed_field_kind(fty).is_some()),
            None => false,
        }
    }

    /// True if a TCO-loop param of type `ty` holds an owned, heap-refcounted value whose PRIOR
    /// slot value must be released before a tail-call back-edge overwrites it (the per-iteration
    /// TCO leak fix). This is the owning set (`ty_is_concrete_rc` ∪ `is_union_type`) plus
    /// HEAP-BEARING sealed records, but MINUS purely-scalar sealed records.
    ///
    /// Path 9 packs heap-field records (e.g. `Trip { id: String, stops: StopTime[] }`) as
    /// `lin_sealed_*` structs. When such a record is threaded through a self-tail-recursive
    /// param slot, each iteration overwrites the slot with a fresh packed struct WITHOUT releasing
    /// the prior one — the old struct + ALL its heap fields (stopTimes buffer, maps, strings) leak
    /// once per iteration (linear scaling). Including heap-bearing sealed records here arms the
    /// back-edge `emit_tco_release_old` + loop-exit `emit_tco_release_final` for them (both route
    /// through `emit_release_repr` → `emit_sealed_release`, which decrements the header rc and walks
    /// the heap fields). The alias guards in those helpers prevent double-freeing a struct still
    /// referenced by a new arg / the returned value / a borrowed entry param.
    ///
    /// A PURELY-scalar sealed record (`sealed_record_is_heap_bearing` is false) is RC-SUPPRESSED and
    /// often stack-resident (an immortal-rc `sealed_stack` alloca): it holds no owned heap reference,
    /// so emitting `lin_sealed_release` on it is at best a no-op and at worst defeats SROA promotion
    /// of the stack value (the sealed-records Stage-4 RC-suppression milestone). It therefore stays
    /// OUT and keeps its pre-existing (no per-iteration release) behavior.
    pub(crate) fn tco_param_needs_release(ty: &Type) -> bool {
        if Self::sealed_fields(ty).is_some() {
            // A sealed record participates in TCO param-slot release ONLY when it carries heap
            // fields (those are what leak). Purely-scalar sealed records stay suppressed.
            return Self::sealed_record_is_heap_bearing(ty);
        }
        Self::ty_is_concrete_rc(ty) || Self::is_union_type(ty)
    }

    /// Tag for how a value of `ty` is BOXED as a scalar TaggedVal — i.e. the byte stored in
    /// the tag field and matched by `is`-checks. This must EXACTLY mirror `box_value` /
    /// `tagged_payload_i64` so the runtime reads the payload back the same way it was written.
    ///
/// Floats: both Float32 and Float64 box as TAG_FLOAT64 with an f64-bits payload (codegen
    /// fpext's a Float32 to f64 before boxing), so TAG_FLOAT32 (a flat-array elem_tag only)
    /// must NEVER be emitted for a boxed scalar — doing so made the runtime read an f64-bits
    /// payload as `f32::from_bits(payload as u32)` → garbage, and made `x is Float64` compare
    /// 5 against a value tagged 4 → dead arm.
    pub(crate) fn type_tag(ty: &Type) -> u8 {
        match ty {
            Type::Null => TAG_NULL,
            Type::Bool => TAG_BOOL,
            Type::Int8 | Type::Int16 | Type::Int32 | Type::IntLit(_) => TAG_INT32,
            // UInt8/16/32 are zero-extended and boxed as TAG_INT64 (always-positive i64) so
            // a u32 >= 2^31 reads back correctly. Must match box_value / build_tagged_val_alloca.
            Type::UInt8 | Type::UInt16 | Type::UInt32 => TAG_INT64,
            Type::Int64 => TAG_INT64,
            // UInt64 — read back unsigned.
            Type::UInt64 => TAG_UINT64,
            // Both float widths box as f64 bits (see doc above).
            Type::Float32 | Type::Float64 => TAG_FLOAT64,
            Type::Str | Type::StrLit(_) => TAG_STR,
            // After Phase 3: non-sealed open objects are TAG_MAP (LinMap); sealed objects are
            // packed structs (TAG_RECORD in union slots). type_tag returns TAG_MAP for both since
            // sealed records boxed into union slots use TAG_RECORD (handled by sealed arm in
            // compile_ir_is_type_single), and open objects are always TAG_MAP.
            Type::Object { .. } => TAG_MAP,
            Type::Map { .. } => TAG_MAP,
            Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_) => TAG_ARRAY,
            Type::Function { .. } => TAG_FUNCTION,
            _ => TAG_NULL,
        }
    }

    /// Like `type_tag` but historically returned `TAG_MAP` for non-sealed and `TAG_OBJECT` for
    /// sealed `Type::Object`. After Cluster D (TAG_OBJECT producers removed), `type_tag` already
    /// returns `TAG_MAP` for all `Type::Object` variants, so this is now identical to `type_tag`.
    /// Kept as a separate entry point for call-site clarity (indicates the site produces a
    /// concrete value, not a boxed union tag check).
    pub(crate) fn type_tag_open(ty: &Type) -> u8 {
        Self::type_tag(ty)
    }

    /// Byte size of `SEALED_HEADER` (refcount u32 + size u32 + heap_desc_ptr u64 + named_desc_ptr u64).
    /// Kept in lockstep with `lin_runtime::sealed::SEALED_HEADER` (24). Sealed-record field payload begins here.
    /// Stage 6a: the 3rd slot (offset 16) is the named descriptor pointer (all fields + names) for TAG_RECORD
    /// field access via `lin_record_get_field`.
    pub(crate) const SEALED_HEADER: u64 = 24;

    /// Immortal-refcount sentinel for STACK-allocated sealed records (sealed-records Stage 4). A
    /// record whose header rc is `>= IMMORTAL_RC` is inert to refcounting: `lin_rc_retain` and
    /// `lin_sealed_release` are no-ops on it (it lives on the stack and is never heap-freed). This
    /// is defense-in-depth — with RC-emission suppression the lowerer omits Retain/Release on a
    /// stack value entirely. MUST stay in lockstep with `lin_runtime::string::IMMORTAL_RC`
    /// (0x8000_0000).
    pub(crate) const SEALED_IMMORTAL_RC: u32 = 0x8000_0000;

    /// True when `ty` is an unboxed scalar field of a sealed record: a fixed-width numeric,
    /// `Bool`, or `IntLit` (Int32 at runtime). Delegates to the canonical definition in
    /// `lin_check::types::Type::is_sealed_scalar_field`.
    pub(crate) fn is_sealed_scalar_field(ty: &Type) -> bool {
        ty.is_sealed_scalar_field()
    }

    /// The descriptor kind code for a HEAP field of a sealed record, or `None` if `ty` is not an
    /// eligible heap field. Heap fields are stored as an 8-byte owned pointer slot and need per-field
    /// retain-on-construct / release-on-drop. MUST stay in lockstep with the `lin_runtime::sealed`
    /// `KIND_*` constants. Eligible (Stage 2): `String`/`StrLit` → KIND_STRING, `Array`/`FixedArray`
    /// → KIND_ARRAY, a NESTED SEALED RECORD → KIND_SEALED. Everything else (`Object` that is NOT a
    /// sealed record, `Union`/`Json`/`TypeVar`, `Iterator`/`Stream`/`Shared`/`Function`, bare
    /// recursive `Named`) → `None` → the whole record stays BOXED (fail-safe).
    pub(crate) fn sealed_field_kind(ty: &Type) -> Option<u32> {
        match ty {
            Type::Str | Type::StrLit(_) => Some(Self::KIND_STRING),
            Type::Array(_) | Type::FixedArray(_) => Some(Self::KIND_ARRAY),
            // A `{ String: T }` index-signature map is a `*LinMap` owned-pointer heap field.
            Type::Map { .. } => Some(Self::KIND_MAP),
            // A nested sealed record (a field whose type is itself sealed-eligible). The recursion
            // bottoms out: a field's eligibility is decided by `sealed_fields` on that field type.
            Type::Object { .. } if Self::sealed_fields(ty).is_some() => Some(Self::KIND_SEALED),
            _ => None,
        }
    }

    /// Descriptor heap-field kind codes. MUST stay in lockstep with `lin_runtime::sealed::KIND_*`.
    pub(crate) const KIND_STRING: u32 = 1;
    pub(crate) const KIND_ARRAY: u32 = 2;
    pub(crate) const KIND_SEALED: u32 = 3;
    /// `{ String: T }` index-signature map heap field (`*LinMap` owned pointer). MUST stay in
    /// lockstep with `lin_runtime::sealed::KIND_MAP`. (Numerically equals `KIND_SUMNODE = 4`, but the
    /// two are in DISJOINT descriptor namespaces — see the runtime const's doc comment.)
    pub(crate) const KIND_MAP: u32 = 4;

    /// NAMED full-field descriptor kind codes — canonical definitions live in `lin_common::tags`.
    /// Re-exported here as associated constants for call-site clarity. The boxing each code implies
    /// matches `type_tag` / `box_value` exactly (so a materialized field reads back identically to a
    /// directly-boxed value).
    pub(crate) const NKIND_INT32: u32 = lin_common::tags::NKIND_INT32;
    pub(crate) const NKIND_INT64: u32 = lin_common::tags::NKIND_INT64;
    pub(crate) const NKIND_UINT64: u32 = lin_common::tags::NKIND_UINT64;
    pub(crate) const NKIND_FLOAT64: u32 = lin_common::tags::NKIND_FLOAT64;
    pub(crate) const NKIND_BOOL: u32 = lin_common::tags::NKIND_BOOL;
    pub(crate) const NKIND_STRING: u32 = lin_common::tags::NKIND_STRING;
    pub(crate) const NKIND_ARRAY: u32 = lin_common::tags::NKIND_ARRAY;
    pub(crate) const NKIND_SEALED: u32 = lin_common::tags::NKIND_SEALED;
    pub(crate) const NKIND_MAP: u32 = lin_common::tags::NKIND_MAP;

    /// The NAMED-descriptor kind for `ty` (a sealed-record field). Covers every permissible sealed
    /// field — scalar OR heap. Returns `None` only for a type that is not a valid sealed field (which
    /// `sealed_fields` already rules out upstream). Mirrors `type_tag`'s boxing choices.
    pub(crate) fn sealed_named_field_kind(ty: &Type) -> Option<u32> {
        match ty {
            Type::Bool => Some(Self::NKIND_BOOL),
            Type::Int8 | Type::Int16 | Type::Int32 | Type::IntLit(_) => Some(Self::NKIND_INT32),
            Type::UInt8 | Type::UInt16 | Type::UInt32 | Type::Int64 => Some(Self::NKIND_INT64),
            Type::UInt64 => Some(Self::NKIND_UINT64),
            Type::Float32 | Type::Float64 => Some(Self::NKIND_FLOAT64),
            Type::Str | Type::StrLit(_) => Some(Self::NKIND_STRING),
            Type::Array(_) | Type::FixedArray(_) => Some(Self::NKIND_ARRAY),
            Type::Map { .. } => Some(Self::NKIND_MAP),
            Type::Object { .. } if Self::sealed_fields(ty).is_some() => Some(Self::NKIND_SEALED),
            _ => None,
        }
    }

    /// True when `ty` is a permissible field of a sealed record. Delegates to the canonical
    /// definition in `lin_check::types::Type::is_sealed_field`.
    pub(crate) fn is_sealed_field(ty: &Type) -> bool {
        ty.is_sealed_field()
    }

    /// THE sealed-record gate. Delegates to the canonical `Type::sealed_fields`
    /// (`lin_check::types`). See that function for the full contract and fail-safe semantics.
    pub(crate) fn sealed_fields(ty: &Type) -> Option<&indexmap::IndexMap<String, Type>> {
        Type::sealed_fields(ty)
    }

    /// Backwards-compatible alias; delegates to `sealed_fields`.
    pub(crate) fn sealed_scalar_fields(ty: &Type) -> Option<&indexmap::IndexMap<String, Type>> {
        Self::sealed_fields(ty)
    }

    /// Byte width of a field SLOT in a sealed record. A scalar occupies its natural width (1/2/4/8);
    /// a HEAP field occupies an 8-byte pointer slot. Used by both layout and size.
    fn sealed_slot_size(fty: &Type) -> u64 {
        if Self::sealed_field_kind(fty).is_some() {
            8 // heap field: pointer slot
        } else {
            fty.bit_width().map(|b| (b as u64) / 8).unwrap_or(1).max(1)
        }
    }

    /// Byte offset (from the struct base, including the header) of `field` within a sealed record,
    /// and the total struct byte size. Fields are packed in declaration order with NATURAL alignment
    /// (each slot aligned to its own width — heap pointer slots to 8); the struct is padded to an
    /// 8-byte multiple so arrays/successive allocs stay aligned. Returns `(offset_of_field,
    /// total_size)`. Panics if `field` is not in the record (a compile error rules this out upstream).
    pub(crate) fn sealed_field_layout(fields: &indexmap::IndexMap<String, Type>, field: &str) -> (u64, u64) {
        let mut offset = Self::SEALED_HEADER;
        let mut found: Option<u64> = None;
        for (k, fty) in fields.iter() {
            let sz = Self::sealed_slot_size(fty);
            let align = sz; // natural alignment = slot size (1/2/4/8)
            offset = (offset + align - 1) / align * align;
            if k == field {
                found = Some(offset);
            }
            offset += sz;
        }
        let total = (offset + 7) / 8 * 8;
        (found.unwrap_or_else(|| panic!("sealed_field_layout: field {field:?} not in record")), total)
    }

    /// Total byte size of a sealed record (header + packed fields, padded to 8).
    pub(crate) fn sealed_struct_size(fields: &indexmap::IndexMap<String, Type>) -> u64 {
        let mut offset = Self::SEALED_HEADER;
        for fty in fields.values() {
            let sz = Self::sealed_slot_size(fty);
            offset = (offset + sz - 1) / sz * sz;
            offset += sz;
        }
        (offset + 7) / 8 * 8
    }

    // Note: the sealed-record-array `elem_tag` sentinel (0xFE) is set by the runtime
    // (`lin_runtime::array::SEALED_ARRAY_TAG`) inside `lin_sealed_array_alloc`; codegen never reads
    // it (all sealed-array ops dispatch on the STATIC element type via `sealed_array_elem`), so no
    // codegen-side constant is needed.

    /// Does `box_value(v, ty)` produce a FRESH +1-owned heap value (which the boxing caller must
    /// later `tagged_release` to reclaim), as opposed to wrapping a BORROWED inner pointer in a box
    /// shell (which the caller frees with `lin_tagged_free_box`, leaving the borrowed inner alone)?
    ///
    /// `box_value` MATERIALIZES — i.e. allocates a fresh +1 — only for nested SEALED records
    /// (`Type::Object` with sealed fields) → fresh boxed `LinObject` (`sealed_materialize_to_object`).
    ///
    /// For sealed-record ARRAYS (Stage-2a: 0xFD pointer-backed), `box_value` calls `lin_box_array`
    /// which wraps the raw `LinArray*` pointer directly (BORROWED, no retain and no fresh
    /// materialization). The `else if is_heap` branch (free box shell only via `lin_tagged_free_box`)
    /// is the correct cleanup path for sealed-array heap fields — NOT `tagged_release`.
    ///
    /// For every other heap field (plain String, plain/flat Array, Map) `box_value` also boxes the
    /// borrowed pointer with no retain.
    ///
    /// This is the single source of truth for the sealed→Json materializers' post-`object_set_fresh`
    /// cleanup (`sealed_materialize_to_object`): a fresh-owned inner needs a full `tagged_release`;
    /// a borrowed inner needs only the box shell freed. Getting this wrong UAF-s the sealed array
    /// (the RAPTOR `Trip { stopTimes: StopTime[] }` shape and the `unionproj3.lin` TCO crash).
    pub(crate) fn box_value_yields_fresh_owned(ty: &Type) -> bool {
        // Nested sealed record: materialized to a fresh boxed object. NOT sealed-record arrays:
        // Stage-2a changed box_value for those to lin_box_array (borrowed pointer, no fresh alloc).
        matches!(ty, Type::Object { .. }) && Self::sealed_fields(ty).is_some()
    }

    /// THE sealed-record-ARRAY gate. Delegates to the canonical `Type::sealed_array_elem`
    /// (`lin_check::types`). See that function for the full contract and fail-safe semantics.
    pub(crate) fn sealed_array_elem(ty: &Type) -> Option<&indexmap::IndexMap<String, Type>> {
        Type::sealed_array_elem(ty)
    }

    /// Element-field packability gate. Delegates to `Type::is_sealed_array_field_packable`.
    pub(crate) fn sealed_array_elem_field_packable(ty: &Type) -> bool {
        ty.is_sealed_array_field_packable()
    }

    // ── Unboxed tagged sum type (`SumNode`) — unboxed-sumtype Stage 1 ─────────────────────────────
    //
    // A sum type `type T = A | B | …` where every variant is a sealed record sharing a distinct
    // StrLit discriminant and (Stage 1) every OTHER field is an unboxed scalar gets the `SumNode`
    // representation (`lin_runtime::sumnode`). NON-RECURSIVE, SCALAR-ONLY this stage: any variant
    // with a heap/Named/union/nested-record field → fall back to the BOXED union (fail-safe).

    /// SumNode header bytes: `u32 rc | u32 size | u64 desc_ptr | u32 tag | u32 _pad`. Payload begins
    /// at offset 24. Kept in lockstep with `lin_runtime::sumnode::SUMNODE_HEADER`.
    pub(crate) const SUMNODE_HEADER: u64 = 24;
    /// Byte offset of the inline discriminant tag. Lockstep with `sumnode::SUMNODE_TAG_OFFSET`.
    pub(crate) const SUMNODE_TAG_OFFSET: u64 = 16;

    /// Descriptor kind code for a RECURSIVE child (`*SumNode`) slot of a sum-type variant
    /// (unboxed-sumtype Stage 2). MUST stay in lockstep with `lin_runtime::sumnode::KIND_SUMNODE`.
    pub(crate) const KIND_SUMNODE: u32 = 4;

    /// True when `ty` is a permissible scalar field of a Stage-1 sum-type variant. Delegates to
    /// `Type::is_sealed_scalar_field` (the canonical definition in `lin_check::types`).
    pub(crate) fn is_sum_scalar_field(ty: &Type) -> bool {
        ty.is_sealed_scalar_field()
    }

    /// The UNIQUE recursive self-reference name of a candidate sum union. Delegates to the
    /// canonical `Type::sum_recursive_self_name` (`lin_check::types`).
    pub(crate) fn sum_recursive_self_name(ty: &Type) -> Option<String> {
        Type::sum_recursive_self_name(ty)
    }

    /// True when `fty` is a recursive self-child field (a `Type::Named(self_name)` slot).
    pub(crate) fn is_sum_recursive_child(fty: &Type, self_name: &str) -> bool {
        matches!(fty, Type::Named(n) if n == self_name)
    }

    /// THE Stage-1 sum-type gate. Delegates to the canonical `Type::sum_type_discriminant`
    /// (`lin_check::types`). See that function for the full contract.
    pub(crate) fn sum_type_discriminant(ty: &Type) -> Option<String> {
        Type::sum_type_discriminant(ty)
    }

    /// True when `ty` is a Stage-1-eligible unboxed sum type. Delegates to `Type::sum_type_eligible`.
    pub(crate) fn is_sum_type(ty: &Type) -> bool {
        Type::sum_type_eligible(ty)
    }

    /// True when `ty` is `Union([Named(n), Null])` — a self-recursive Named alias union where the
    /// sealed fields cannot be resolved (Named isn't an inlined Object). Used to treat a
    /// PackedStruct → Named-nullable coerce as a pass-through identity.
    pub(crate) fn is_named_nullable_union(ty: &Type) -> bool {
        let Type::Union(members) = ty else { return false };
        let mut has_named = false;
        for m in members {
            match m {
                Type::Null => {}
                Type::Named(_) => { has_named = true; }
                _ => return false,
            }
        }
        has_named
    }

    /// Stage-3 NullableRecord gate. Delegates to the canonical `Type::nullable_sealed_record`
    /// (`lin_check::types`). See that function for the full contract.
    pub(crate) fn nullable_sealed_record_type(ty: &Type) -> Option<&indexmap::IndexMap<String, Type>> {
        Type::nullable_sealed_record(ty)
    }

    /// unboxed-sumtype Stage 3: if `ty` is a union of EXACTLY a Stage-eligible sum type plus `Null`
    /// (e.g. `Expr | Null` — the static type of a `{ String: Expr }` map read, ADR-055 safe-access),
    /// return that inner sum type. Such a value at runtime is either a `*SumNode` (a real node) or a
    /// null pointer, so the dynamic-boundary boxing must materialize the SumNode when non-null. The
    /// flattened union shape is the canonical sum union with a `Type::Null` member appended.
    pub(crate) fn sum_member_of_nullable_union(ty: &Type) -> Option<Type> {
        let Type::Union(members) = ty else { return None };
        let mut sum: Option<Type> = None;
        for m in members {
            if matches!(m, Type::Null) {
                continue;
            }
            if Self::is_sum_type(m) && sum.is_none() {
                sum = Some(m.clone());
            } else {
                return None; // any other member → not a bare `sum | Null`
            }
        }
        sum
    }

    /// The (unsealed) object Type of the FIRST variant of a sum type — used purely to give
    /// `box_value` a TAG_OBJECT box tag when boxing a materialized SumNode for a dynamic edge. Any
    /// variant's object type yields the same box tag (TAG_OBJECT); the first is a convenient
    /// representative. `ty` must be a sum type.
    pub(crate) fn sumnode_first_variant_obj_ty(ty: &Type) -> Type {
        let fields = match ty {
            Type::Union(vs) => match vs.first() {
                Some(Type::Object { fields, .. }) => fields.clone(),
                _ => Default::default(),
            },
            _ => Default::default(),
        };
        Type::object(fields)
    }

    /// The PAYLOAD field map of one variant (the discriminant key removed — it is the inline tag).
    /// Only the scalar fields remain. Declaration order is preserved (the layout key).
    pub(crate) fn sumnode_variant_payload_fields(
        variant: &indexmap::IndexMap<String, Type>,
        disc_key: &str,
    ) -> indexmap::IndexMap<String, Type> {
        variant
            .iter()
            .filter(|(k, _)| k.as_str() != disc_key)
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// Byte width of one variant-payload field SLOT. A scalar occupies its natural width (1/2/4/8); a
    /// RECURSIVE child (`Type::Named`, Stage 2) occupies an 8-byte `*SumNode` pointer slot. A HEAP
    /// field (String/Array/nested-sealed, Stage 3) occupies an 8-byte owned pointer slot (same as the
    /// recursive child — both are RC heap pointers). Scalars use their natural width.
    fn sumnode_slot_size(fty: &Type) -> u64 {
        if matches!(fty, Type::Named(_)) {
            8 // recursive child: owned *SumNode pointer slot
        } else if Self::sealed_field_kind(fty).is_some() && !Self::is_sum_scalar_field(fty) {
            8 // heap field (String/Array/nested-sealed): owned heap pointer slot (Stage 3)
        } else {
            fty.bit_width().map(|b| (b as u64) / 8).unwrap_or(1).max(1)
        }
    }

    /// Byte offset (from the node base, INCLUDING the 24-byte header) of `field` within a variant's
    /// payload, packed in declaration order with natural alignment. The header end (offset 24) is the
    /// payload base. Returns the field offset. Panics if `field` is not in the payload.
    pub(crate) fn sumnode_field_offset(
        payload_fields: &indexmap::IndexMap<String, Type>,
        field: &str,
    ) -> u64 {
        let mut offset = Self::SUMNODE_HEADER;
        for (k, fty) in payload_fields.iter() {
            let sz = Self::sumnode_slot_size(fty);
            offset = (offset + sz - 1) / sz * sz;
            if k == field {
                return offset;
            }
            offset += sz;
        }
        panic!("sumnode_field_offset: field {field:?} not in variant payload")
    }

    /// Byte size of one variant's node (header + that variant's packed payload, padded to 8).
    pub(crate) fn sumnode_variant_size(payload_fields: &indexmap::IndexMap<String, Type>) -> u64 {
        let mut offset = Self::SUMNODE_HEADER;
        for fty in payload_fields.values() {
            let sz = Self::sumnode_slot_size(fty);
            offset = (offset + sz - 1) / sz * sz;
            offset += sz;
        }
        (offset + 7) / 8 * 8
    }

    /// The total (max-variant-sized) node byte size for a whole sum type. Every variant fits in one
    /// fixed-size node. `ty` must be a sum type (`is_sum_type`).
    pub(crate) fn sumnode_total_size(ty: &Type) -> u64 {
        let key = Self::sum_type_discriminant(ty).expect("sumnode_total_size on non-sum type");
        let variants = match ty {
            Type::Union(vs) => vs,
            _ => unreachable!(),
        };
        let mut max = Self::SUMNODE_HEADER;
        for v in variants {
            if let Type::Object { fields, .. } = v {
                let payload = Self::sumnode_variant_payload_fields(fields, &key);
                max = max.max(Self::sumnode_variant_size(&payload));
            }
        }
        max
    }

    /// The dense variant tag (declaration order) for the variant whose discriminant value is `disc`.
    pub(crate) fn sumnode_variant_tag(ty: &Type, disc: &str) -> Option<u32> {
        let key = Self::sum_type_discriminant(ty)?;
        let variants = match ty {
            Type::Union(vs) => vs,
            _ => return None,
        };
        for (i, v) in variants.iter().enumerate() {
            if let Type::Object { fields, .. } = v {
                if let Some(Type::StrLit(s)) = fields.get(&key) {
                    if s == disc {
                        return Some(i as u32);
                    }
                }
            }
        }
        None
    }

    /// The payload field map of the variant whose discriminant value is `disc`.
    pub(crate) fn sumnode_variant_by_disc(
        ty: &Type,
        disc: &str,
    ) -> Option<indexmap::IndexMap<String, Type>> {
        let key = Self::sum_type_discriminant(ty)?;
        let variants = match ty {
            Type::Union(vs) => vs,
            _ => return None,
        };
        for v in variants {
            if let Type::Object { fields, .. } = v {
                if let Some(Type::StrLit(s)) = fields.get(&key) {
                    if s == disc {
                        return Some(Self::sumnode_variant_payload_fields(fields, &key));
                    }
                }
            }
        }
        None
    }

    /// Returns true when the element type maps to a flat unboxed scalar array.
    /// Only concrete fixed-width numeric scalars qualify — not Bool (stored as i1,
    /// awkward to pack densely), not pointers, not unions.
    pub(crate) fn is_flat_scalar(ty: &Type) -> bool {
        matches!(ty,
            Type::Int8 | Type::UInt8 |
            Type::Int16 | Type::UInt16 |
            Type::Int32 | Type::UInt32 |
            Type::Int64 | Type::UInt64 |
            Type::Float32 | Type::Float64
        )
    }

    /// Suffix used in runtime function names for flat array variants.
    pub(crate) fn flat_suffix(ty: &Type) -> &'static str {
        match ty {
            Type::Int8 => "i8",
            Type::UInt8 => "u8",
            Type::Int16 => "i16",
            Type::UInt16 => "u16",
            Type::Int32 => "i32",
            Type::UInt32 => "u32",
            Type::Int64 => "i64",
            Type::UInt64 => "u64",
            Type::Float32 => "f32",
            Type::Float64 => "f64",
            _ => unreachable!("flat_suffix called with non-scalar type"),
        }
    }

    /// Narrow/widen an integer value to the integer width of `target_ty`. Non-integer
    /// values and non-integer targets are returned unchanged. Used to reconcile a runtime
    /// intrinsic that returns a fixed width (e.g. lin_array_length → i64) with a declared
    /// result type of a different width (e.g. Int32).
    pub(crate) fn coerce_int_width(&self, val: BasicValueEnum<'ctx>, target_ty: &Type) -> BasicValueEnum<'ctx> {
        if !val.is_int_value() || !target_ty.is_integer() {
            return val;
        }
        let iv = val.into_int_value();
        let target_llvm = self.llvm_type(target_ty).into_int_type();
        let iv_bits = iv.get_type().get_bit_width();
        let tgt_bits = target_llvm.get_bit_width();
        if tgt_bits == iv_bits {
            val
        } else if tgt_bits > iv_bits {
            if target_ty.is_signed() {
                self.builder.int_s_extend(iv, target_llvm, "ir_len_sext").into()
            } else {
                self.builder.int_z_extend(iv, target_llvm, "ir_len_zext").into()
            }
        } else {
            self.builder.int_truncate(iv, target_llvm, "ir_len_trunc").into()
        }
    }


}