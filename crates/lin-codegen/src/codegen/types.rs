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
            Type::Map(_) => self.context.ptr_type(AddressSpace::default()).into(),
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
                | Type::Map(_)
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
            Type::Int8 | Type::Int16 | Type::Int32 => TAG_INT32,
            // UInt8/16/32 are zero-extended and boxed as TAG_INT64 (always-positive i64) so
            // a u32 >= 2^31 reads back correctly. Must match box_value / build_tagged_val_alloca.
            Type::UInt8 | Type::UInt16 | Type::UInt32 => TAG_INT64,
            Type::Int64 => TAG_INT64,
            // UInt64 — read back unsigned.
            Type::UInt64 => TAG_UINT64,
            // Both float widths box as f64 bits (see doc above).
            Type::Float32 | Type::Float64 => TAG_FLOAT64,
            Type::Str | Type::StrLit(_) => TAG_STR,
            Type::Object { .. } => TAG_OBJECT,
            Type::Map(_) => TAG_MAP,
            Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_) => TAG_ARRAY,
            Type::Function { .. } => TAG_FUNCTION,
            _ => TAG_NULL,
        }
    }

    /// Byte size of `SEALED_HEADER` (refcount u32 + size u32 + desc_ptr u64). Kept in lockstep with
    /// `lin_runtime::sealed::SEALED_HEADER` (16). Sealed-record field payload begins here.
    pub(crate) const SEALED_HEADER: u64 = 16;

    /// Immortal-refcount sentinel for STACK-allocated sealed records (sealed-records Stage 4). A
    /// record whose header rc is `>= IMMORTAL_RC` is inert to refcounting: `lin_rc_retain` and
    /// `lin_sealed_release` are no-ops on it (it lives on the stack and is never heap-freed). This
    /// is defense-in-depth — with RC-emission suppression the lowerer omits Retain/Release on a
    /// stack value entirely. MUST stay in lockstep with `lin_runtime::string::IMMORTAL_RC`
    /// (0x8000_0000).
    pub(crate) const SEALED_IMMORTAL_RC: u32 = 0x8000_0000;

    /// True when `ty` is an unboxed scalar field of a sealed record: a fixed-width numeric (mirrors
    /// `is_flat_scalar`) OR `Bool`. Scalar fields are stored inline at their natural-aligned offset
    /// and need NO per-field RC.
    pub(crate) fn is_sealed_scalar_field(ty: &Type) -> bool {
        Self::is_flat_scalar(ty) || matches!(ty, Type::Bool)
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
            Type::Map(_) => Some(Self::KIND_MAP),
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

    /// NAMED full-field descriptor kind codes (ADR-063 Stage 3b mechanism (i)). Unlike the heap-only
    /// `KIND_*`, these cover SCALARS too, since the named descriptor lists EVERY field for the boxed
    /// materialize-on-read path. MUST stay in lockstep with `lin_runtime::sealed::NKIND_*`. The boxing
    /// each code implies matches `type_tag` / `box_value` exactly (so a materialized field reads back
    /// identically to a directly-boxed value).
    pub(crate) const NKIND_INT32: u32 = 1; // Int8/Int16/Int32
    pub(crate) const NKIND_INT64: u32 = 2; // Int64, UInt8/UInt16/UInt32 (zero-extended positive)
    pub(crate) const NKIND_UINT64: u32 = 3; // UInt64
    pub(crate) const NKIND_FLOAT64: u32 = 4; // Float32/Float64
    pub(crate) const NKIND_BOOL: u32 = 5; // Bool
    pub(crate) const NKIND_STRING: u32 = 6; // String/StrLit
    pub(crate) const NKIND_ARRAY: u32 = 7; // Array/FixedArray
    pub(crate) const NKIND_SEALED: u32 = 8; // nested sealed record
    pub(crate) const NKIND_MAP: u32 = 9; // { String: T } index-signature map (*LinMap)

    /// The NAMED-descriptor kind for `ty` (a sealed-record field). Covers every permissible sealed
    /// field — scalar OR heap. Returns `None` only for a type that is not a valid sealed field (which
    /// `sealed_fields` already rules out upstream). Mirrors `type_tag`'s boxing choices.
    pub(crate) fn sealed_named_field_kind(ty: &Type) -> Option<u32> {
        match ty {
            Type::Bool => Some(Self::NKIND_BOOL),
            Type::Int8 | Type::Int16 | Type::Int32 => Some(Self::NKIND_INT32),
            Type::UInt8 | Type::UInt16 | Type::UInt32 | Type::Int64 => Some(Self::NKIND_INT64),
            Type::UInt64 => Some(Self::NKIND_UINT64),
            Type::Float32 | Type::Float64 => Some(Self::NKIND_FLOAT64),
            Type::Str | Type::StrLit(_) => Some(Self::NKIND_STRING),
            Type::Array(_) | Type::FixedArray(_) => Some(Self::NKIND_ARRAY),
            Type::Map(_) => Some(Self::NKIND_MAP),
            Type::Object { .. } if Self::sealed_fields(ty).is_some() => Some(Self::NKIND_SEALED),
            _ => None,
        }
    }

    /// True when `ty` is a permissible field of a sealed record: a scalar OR an eligible heap field.
    pub(crate) fn is_sealed_field(ty: &Type) -> bool {
        Self::is_sealed_scalar_field(ty) || Self::sealed_field_kind(ty).is_some()
    }

    /// THE sealed-record gate (sealed-records Stages 1–2). Returns `Some(fields)` iff `ty` is a
    /// `Type::Object { sealed: true }` whose fields are ALL either unboxed scalars OR eligible heap
    /// fields (String/Array/nested-sealed). Returns `None` (→ keep the boxed `LinObject` path) for:
    /// an unsealed object (anonymous literal/inferred shape), any object with an INELIGIBLE field
    /// (union/Json/Iterator/Stream/Shared/Function/unsealed-object), and every non-object type.
    /// FAIL SAFE: when unsure, `None` (boxed). The field order is the TYPE DECLARATION's `IndexMap`
    /// order, preserved by Stage 0.5 resolution — this fixes a single canonical physical layout.
    ///
    /// Note on recursion termination: a nested-sealed field calls back into `sealed_fields`; a
    /// directly self-recursive record (a field of its own type) survives resolution as `Type::Named`
    /// (not an inlined `Type::Object`), so `sealed_field_kind` sees `Named` → `None` → that record
    /// is kept boxed. Hence `sealed_fields` cannot recurse infinitely on a cyclic type.
    pub(crate) fn sealed_fields(ty: &Type) -> Option<&indexmap::IndexMap<String, Type>> {
        match ty {
            Type::Object { fields, sealed: true } if !fields.is_empty()
                && fields.values().all(Self::is_sealed_field) =>
            {
                Some(fields)
            }
            _ => None,
        }
    }

    /// Backwards-compatible alias retained for the (now generalized) gate. Stage 1 call sites used
    /// `sealed_scalar_fields`; it now accepts heap fields too via `sealed_fields`.
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

    /// Per-element byte STRIDE of a sealed-record array: the struct payload WITHOUT the 16-byte
    /// header (the array owns the elements, so no per-element header), padded to 8 so successive
    /// elements stay 8-aligned. Equals `sealed_struct_size - SEALED_HEADER`.
    pub(crate) fn sealed_array_stride(fields: &indexmap::IndexMap<String, Type>) -> u64 {
        Self::sealed_struct_size(fields) - Self::SEALED_HEADER
    }

    /// Does `box_value(v, ty)` produce a FRESH +1-owned heap value (which the boxing caller must
    /// later `tagged_release` to reclaim), as opposed to wrapping a BORROWED inner pointer in a box
    /// shell (which the caller frees with `lin_tagged_free_box`, leaving the borrowed inner alone)?
    ///
    /// `box_value` MATERIALIZES — i.e. allocates a fresh +1 — for exactly two sealed-field cases:
    ///   - a nested SEALED record (`Type::Object` with sealed fields) → fresh boxed `LinObject`
    ///     (`sealed_materialize_to_object`); and
    ///   - a sealed-record ARRAY (`T[]` with sealed elements) → fresh tagged `Object[]`
    ///     (`sealed_array_to_tagged`).
    /// For every other heap field (plain String, plain/flat Array, Map) `box_value` boxes the
    /// borrowed pointer with no retain.
    ///
    /// This is the single source of truth for the sealed→Json materializers' post-`object_set_fresh`
    /// cleanup (`sealed_materialize_to_object` / `sealed_array_elem_materializer`): a fresh-owned
    /// inner needs a full `tagged_release`; a borrowed inner needs only the box shell freed. Getting
    /// this wrong leaks the whole materialized inner (record-with-record-array-field, the RAPTOR
    /// `Trip { stopTimes: StopTime[] }` shape).
    pub(crate) fn box_value_yields_fresh_owned(ty: &Type) -> bool {
        // Nested sealed record: materialized to a fresh boxed object.
        if matches!(ty, Type::Object { .. }) && Self::sealed_fields(ty).is_some() {
            return true;
        }
        // Sealed-record array: materialized to a fresh tagged Object[].
        Self::sealed_array_elem(ty).is_some()
    }

    /// THE sealed-record-ARRAY gate (sealed-records Stage 3). Returns `Some(fields)` iff `ty` is an
    /// `Array(elem)` (or `FixedArray`) whose element is an ALL-SCALAR sealed record — the high-value,
    /// lowest-RC-risk case (no per-element heap fields, so array drop is a single free). FAIL SAFE:
    /// arrays of heap-field sealed records, anonymous-record arrays, union/Json/opaque-element
    /// arrays, and non-arrays all return `None` (→ keep the boxed/flat path). Stage 3b (heap-field
    /// element records) is intentionally NOT yet accepted here.
    pub(crate) fn sealed_array_elem(ty: &Type) -> Option<&indexmap::IndexMap<String, Type>> {
        let elem = match ty {
            Type::Array(e) => e.as_ref(),
            _ => return None,
        };
        let fields = Self::sealed_fields(elem)?;
        // sealed-records Stage 3 (scalar) + Stage 3b (heap-field): a record-array element is laid out
        // contiguously and header-less iff EVERY field is eligible for the packed representation —
        // either an unboxed scalar (Stage 3a) or a packed-eligible HEAP field (Stage 3b: String /
        // nested-sealed / Array, decided by `sealed_array_elem_field_packable`). Any field that is
        // NOT packable (Union/Json/TypeVar/Iterator/non-sealed Object/Function) → fail-safe to the
        // BOXED `Object[]` path for the whole array.
        if fields.values().all(Self::sealed_array_elem_field_packable) {
            Some(fields)
        } else {
            None
        }
    }

    /// THE Stage-3b heap-field eligibility predicate — the SINGLE source of truth for which sealed
    /// record fields may live inline in a contiguous element buffer. MUST be mirrored EXACTLY by
    /// `lin_ir::lower::is_sealed_array_elem_field_packable`, `lin_ir::monomorphize::field_packed_scalar`,
    /// and `lin_ir::repr::sealed_array_elem_field_packable` (the gate is multi-site; any disagreement
    /// makes the lowerer's ownership/Coerce insertion diverge from the physical layout → UAF / mis-read).
    ///
    /// CURRENTLY: SCALARS ONLY (Stage 3a). The per-element-per-field RC machinery, the materializers
    /// (`sealed_array_elem_materializer` / `sealed_array_materialize_elem`, both heap-field-aware), and
    /// the dynamic-consumer boundaries are all COMPLETE and ASan-clean on hand-written heap-field
    /// fixtures (construct / push / field-read / index-set / drop / transfer / `==` / toString /
    /// filter / map / sortBy — single-module). Two of the three historical blockers are now CLOSED:
    ///   1. FIELD OMISSION — structurally omitting a declared sealed field is a COMPILE error
    ///      (`omits_required_field`), so a packed element can never store a NULL heap pointer.
    ///   2. PRODUCER/CONSUMER LITERAL DRIFT — an inferred array literal (`[]`, `Array(Never)`) now
    ///      ADOPTS the concrete param's resolved element representation in BOTH `infer_call` AND
    ///      `infer_dot_call` (the latter previously bypassed it — the calc-lexer `scan(.., [])` UAF),
    ///      so a producer and its consumer agree. `repr::verify` (now covering every repr-consuming
    ///      opcode) makes any residual mismatch a debug-build compile panic, not a silent runtime UAF.
    ///
    /// THE REMAINING BLOCKER (why heap fields stay scalar-only): WHOLE-PROGRAM RECORD REPRESENTATION
    /// CONSISTENCY for records that reach a `{ String: T[] }` MAP-VALUE position (the dijkstra
    /// `{String: Neighbor[]}` shape). A `{String: T[]}` map is pervasively read into a `T[] | Null`
    /// UNION (`match adj[u] is Null => [] else => …`) and then BOTH mutated in place (`push(it, x)`)
    /// AND read by the generic boxed `for`. In-place mutation REQUIRES keep-packed-by-pointer (a
    /// shared 0xFE buffer); the boxed `for`/`lin_array_get_tagged` reader REQUIRES a boxed `Object[]`
    /// (it reads a 0xFE buffer's packed structs as TaggedVals → the `0x07` heap-field deref crash; for
    /// SCALARS it silently misreads → garbage, a latent bug that exists on master but no corpus test
    /// hits). The two are irreconcilable at one map-value representation UNLESS `lin_array_get_tagged`
    /// materializes a packed element using a NAMED full-field descriptor (a runtime-layout change) OR
    /// the record `Neighbor` is boxed CONSISTENTLY everywhere it is reachable from the map (a
    /// cross-module record-taint pass — a record is packed everywhere or boxed everywhere, never
    /// per-occurrence). Either is a larger change than a local gate; until then heap-field element
    /// arrays stay boxed (fail-safe). Re-enable by returning `Self::is_sealed_field(ty)` here AND in
    /// the three mirrors, AND landing one of those two whole-program mechanisms, then re-run corpus +
    /// ASan (the `repr::verify` debug_assert is the structural guard that the swap is consistent).
    pub(crate) fn sealed_array_elem_field_packable(ty: &Type) -> bool {
        // Delegates to the SINGLE source of truth (ADR-063 gate consolidation). Stage 3b widens the
        // gate by editing `Type::is_sealed_array_field_packable` alone; this and the three lin-ir
        // mirrors all defer to it, so they cannot drift.
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

    /// True when `ty` is a permissible SCALAR field of a Stage-1 sum-type variant (a fixed-width
    /// numeric or Bool — no per-field RC). The discriminant field is a StrLit; it is laid out as a
    /// scalar slot is NOT — it is carried inline by the tag, never stored, so it is excluded from the
    /// payload (see `sumnode_variant_payload_fields`).
    pub(crate) fn is_sum_scalar_field(ty: &Type) -> bool {
        Self::is_sealed_scalar_field(ty)
    }

    /// The UNIQUE recursive self-reference name of a candidate sum union (unboxed-sumtype Stage 2),
    /// or `None` if the union has no recursive child or more than one distinct self-name.
    ///
    /// A self-recursive sum type (`type Ast = Num | BinOp` with `BinOp.left/right : Ast`) survives
    /// type resolution with its recursive child fields as `Type::Named(n)` (the checker leaves the
    /// cyclic back-reference unexpanded — `lin-check::resolve::resolve_named_cycle`). At every real
    /// codegen/repr site the recursive child is `Type::Named(n)` for the SINGLE alias name `n` of the
    /// sum type itself. We detect recursion ENV-FREE by collecting the set of `Named` names appearing
    /// directly as a variant field value; a well-formed direct-self-recursive sum type has exactly one
    /// such name. Mutual recursion (two distinct names) is OUT OF SCOPE this stage → `None` (the gate
    /// then falls back to boxed, fail-safe). This is the SINGLE source of truth, mirrored in
    /// `lin_ir::repr::sum_recursive_self_name`.
    pub(crate) fn sum_recursive_self_name(ty: &Type) -> Option<String> {
        let variants = match ty {
            Type::Union(vs) => vs,
            _ => return None,
        };
        let mut names: std::collections::HashSet<String> = std::collections::HashSet::new();
        for v in variants {
            if let Type::Object { fields, .. } = v {
                for fty in fields.values() {
                    if let Type::Named(n) = fty {
                        names.insert(n.clone());
                    }
                }
            }
        }
        if names.len() == 1 {
            names.into_iter().next()
        } else {
            None
        }
    }

    /// True when `fty` is a RECURSIVE child field of the sum type whose self-name is `self_name`
    /// (unboxed-sumtype Stage 2): a `Type::Named(self_name)` slot, stored as an 8-byte owned
    /// `*SumNode` pointer. (The inlined-`Union` form does not appear at real sites — see
    /// `sum_recursive_self_name`.)
    pub(crate) fn is_sum_recursive_child(fty: &Type, self_name: &str) -> bool {
        matches!(fty, Type::Named(n) if n == self_name)
    }

    /// THE Stage-1 sum-type gate (SINGLE source of truth, mirrored in `lin_ir::repr::sum_type_eligible`).
    /// Returns the discriminant key iff `ty` is a `Type::Union` of 2+ variants where:
    ///   (1) every variant is a `Type::Object` (sealed or not — the union itself is the seal);
    ///   (2) a SHARED key exists whose value is a distinct `StrLit` on every variant (the
    ///       discriminant — same soundness rule as the shipped union-discrimination);
    ///   (3) every OTHER field of every variant is EITHER an unboxed scalar OR (Stage 2) a RECURSIVE
    ///       self-child (`Type::Named(self_name)`, stored as an owned `*SumNode` pointer). NO heap
    ///       (String/Array)/union/nested-non-recursive-record/foreign-Named field. Any violation →
    ///       `None` → fall back to the boxed union (fail-safe).
    /// A `Null` member disqualifies (a nullable sum stays boxed — fail-safe, strict scope).
    pub(crate) fn sum_type_discriminant(ty: &Type) -> Option<String> {
        let variants = match ty {
            Type::Union(vs) => vs,
            _ => return None,
        };
        if variants.len() < 2 {
            return None;
        }
        // Stage 2: the unique recursive self-name (if any). A field equal to `Named(self_name)` is a
        // legal recursive child (`*SumNode` slot). `None` when the type is non-recursive (Stage 1) OR
        // when it has >1 distinct Named name (mutual recursion — out of scope → those Named fields
        // then fail `is_sum_scalar_field` → the gate rejects, fail-safe to boxed).
        let self_name = Self::sum_recursive_self_name(ty);
        // All variants must be concrete records (no Null/Named/scalar member).
        let mut recs: Vec<&indexmap::IndexMap<String, Type>> = Vec::with_capacity(variants.len());
        for v in variants {
            match v {
                Type::Object { fields, .. } if !fields.is_empty() => recs.push(fields),
                _ => return None,
            }
        }
        // Find a shared key that is a distinct StrLit on every variant.
        let first = recs[0];
        'keys: for (key, kty) in first.iter() {
            if !matches!(kty, Type::StrLit(_)) {
                continue;
            }
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            for rec in &recs {
                match rec.get(key) {
                    Some(Type::StrLit(s)) => {
                        if !seen.insert(s.clone()) {
                            continue 'keys; // not distinct
                        }
                    }
                    _ => continue 'keys, // missing/non-StrLit on some variant
                }
            }
            // Every OTHER field of every variant must be an unboxed scalar OR a recursive self-child.
            for rec in &recs {
                for (fk, fty) in rec.iter() {
                    if fk == key {
                        continue; // the discriminant (a StrLit) is carried by the tag, not stored
                    }
                    if matches!(fty, Type::StrLit(_)) {
                        // A second StrLit field is not a scalar slot — out of scope.
                        return None;
                    }
                    // Stage 2: a recursive self-child (`Named(self_name)`) is an 8-byte `*SumNode`
                    // slot. Any OTHER `Named` (foreign type) / heap / union field → not packable.
                    let is_recursive_child = self_name
                        .as_deref()
                        .is_some_and(|n| Self::is_sum_recursive_child(fty, n));
                    if !Self::is_sum_scalar_field(fty) && !is_recursive_child {
                        return None;
                    }
                }
            }
            return Some(key.clone());
        }
        None
    }

    /// `Some(())` shorthand: is `ty` a Stage-1-eligible unboxed sum type? (Foundation helper —
    /// consumed once the repr seed + call ABI are wired; see `repr::type_seed`.)
    #[allow(dead_code)]
    pub(crate) fn is_sum_type(ty: &Type) -> bool {
        Self::sum_type_discriminant(ty).is_some()
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
    /// RECURSIVE child (`Type::Named`, Stage 2) occupies an 8-byte `*SumNode` pointer slot. (Any other
    /// non-scalar would never reach here — the gate rejects it.)
    fn sumnode_slot_size(fty: &Type) -> u64 {
        if matches!(fty, Type::Named(_)) {
            8 // recursive child: owned *SumNode pointer slot
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

    /// Return the i8 constant for the runtime tag a value of `ty` is boxed under (used by
    /// `is`-checks in match.rs). Delegates to `type_tag` so the two can never disagree — the
    /// previous hand-copied table is what let Float32/Float64 drift to TAG_FLOAT32 (4) here
    /// while box_value wrote TAG_FLOAT64 (5).
    pub(crate) fn type_tag_const(&self, ty: &Type) -> inkwell::values::IntValue<'ctx> {
        let i8_ty = self.context.i8_type();
        // Types that are never boxed as a recognised scalar tag (Union/etc.) map to a sentinel
        // 0xFF so a stray tag comparison never spuriously matches. `compile_ir_is_type` handles
        // TypeVar (Json-erased ⇒ always true) and Union (match-any-member) BEFORE reaching here,
        // so this fallback is only hit for genuinely-untaggable targets where "never match" is
        // the safe answer.
        let tag: u8 = match ty {
            Type::Null | Type::Bool | Type::Int8 | Type::Int16 | Type::Int32
            | Type::UInt8 | Type::UInt16 | Type::UInt32 | Type::Int64 | Type::UInt64
            | Type::Float32 | Type::Float64 | Type::Str | Type::StrLit(_) | Type::Object { .. }
            | Type::Map(_)
            | Type::Array(_) | Type::FixedArray(_) | Type::Iterator(_) | Type::Function { .. } => {
                Self::type_tag(ty)
            }
            _ => 0xFF,
        };
        i8_ty.const_int(tag as u64, false)
    }

}