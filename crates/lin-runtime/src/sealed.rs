//! Sealed-record runtime support (sealed-records design, Stages 1–2).
//!
//! A *sealed record* is a named record type (`type T = { ... }`) whose fields are ALL either
//! unboxed scalars (Int8/16/32/64, UInt8/16/32/64, Float32/64, Bool) — Stage 1 — and/or a small
//! set of HEAP-pointer field kinds (String, Array, nested sealed record) — Stage 2. Such a value
//! is laid out by codegen as a packed heap struct:
//!
//! ```text
//! [ u32 refcount | u32 size | u64 desc_ptr | field 0 | field 1 | ... ]
//! ```
//!
//! The 16-byte header keeps i64/f64/ptr fields naturally aligned. Layout invariants:
//!   - offset 0:  u32 refcount — so the existing `lin_rc_retain` (which bumps the u32 at offset 0)
//!     works UNCHANGED on a sealed struct.
//!   - offset 4:  u32 size — total byte size, so the struct can be freed WITHOUT the caller passing
//!     the size (`lin_sealed_release_self`), needed by the closure-capture-release walk and the
//!     thread-transfer release, which only have a one-byte kind per capture.
//!   - offset 8:  u64 desc_ptr — pointer to a static, codegen-emitted FIELD DESCRIPTOR (see below),
//!     or NULL when the record has no heap fields (a Stage-1 scalar-only record). The descriptor
//!     reaching EVERY drop site through the header is the entire soundness mechanism for Stage 2:
//!     release/capture-release/transfer all read it from the struct without needing the static
//!     type.
//!   - offset 16: field payload begins. Scalar fields store the scalar; HEAP fields store an
//!     owned (+1) POINTER (a `*LinString` / `*LinArray` / `*sealed-struct`), exactly like a boxed
//!     object's heap payload.
//!
//! ## Field descriptor (`SealedDesc`)
//! A static read-only blob emitted once per sealed type by codegen (mirrors the closure capture
//! descriptor and the fromJson schema descriptor). Layout:
//!
//! ```text
//! [ u32 count | { u32 byte_offset, u32 kind } * count ]
//! ```
//!
//! It lists ONLY the HEAP fields (scalars need no per-field RC, so they are omitted). `kind` is one
//! of the `KIND_*` constants below. A scalar-only record's descriptor pointer is NULL (count == 0).
//!
//! ## Per-field RC contract (the Stage-2 obligation)
//!   - Construct: each heap field is retained exactly once (the struct owns a +1 reference). Codegen
//!     emits the retain (mirrors the boxed-object inline construction `lin_rc_retain` per heap field).
//!   - Projection-copy: the fresh sealed struct retains its copied heap fields; the source is
//!     untouched (it keeps its own ownership). Codegen emits the retain.
//!   - Drop at rc==0: `lin_sealed_release` walks the descriptor and releases each heap field FIRST
//!     (by kind), THEN frees the struct. Nested sealed fields recurse, releasing THEIR heap fields.
//!   - Thread transfer: `clone_sealed` (in transfer.rs) deep-copies each heap field per the
//!     descriptor (share-nothing); release frees them via the same descriptor walk.

use std::alloc::{alloc, dealloc, Layout};
use std::collections::HashMap;
use std::sync::Mutex;

// One heap descriptor is built per distinct sealed type. Named-desc pointers are stable (static
// codegen globals), so the pointer value is a sound key.
static HEAP_DESC_MEMO: Mutex<Option<HashMap<usize, usize>>> = Mutex::new(None);

/// Header size in bytes: `u32 refcount` + `u32 size` + `u64 heap_desc_ptr` + `u64 named_desc_ptr`.
/// Field payload begins at offset 24. Kept in lockstep with `Codegen::SEALED_HEADER`.
/// Stage 6a: extended from 16 to 24 bytes to carry BOTH the heap descriptor (at offset 8, for
/// RC/drop/transfer field walks) AND the named descriptor (at offset 16, for dynamic field access
/// by name — used by TAG_RECORD/lin_record_get_field). The named descriptor is the same static
/// global emitted by `Codegen::sealed_named_descriptor`; storing its pointer in the header makes
/// it recoverable from any sealed struct pointer without a side table.
pub const SEALED_HEADER: usize = 24;

// Field-descriptor kind codes. MUST stay in lockstep with `Codegen::sealed_field_kind`.
/// A scalar field — NEVER appears in a descriptor (scalars need no per-field RC). Reserved.
pub const KIND_SCALAR: u32 = 0;
/// `*LinString` heap field → `lin_string_release` on drop, deep-copied on transfer.
pub const KIND_STRING: u32 = 1;
/// `*LinArray` heap field → `lin_array_release` on drop, deep-copied on transfer.
pub const KIND_ARRAY: u32 = 2;
/// Nested sealed-record `*struct` heap field → `lin_sealed_release_self` on drop (which recurses
/// via the nested struct's OWN descriptor), deep-copied on transfer via `clone_sealed`.
pub const KIND_SEALED: u32 = 3;
/// `*LinMap` (a `{ String: T }` index-signature map) heap field → `lin_map_release` on drop,
/// deep-copied on transfer via `clone_map`. A Map field lives inline in the packed struct as an
/// owned (+1) `*LinMap` pointer slot, exactly like a String/Array heap field.
///
/// NOTE on the numeric value: this collides with `sumnode::KIND_SUMNODE = 4`, but the two live in
/// SEPARATE descriptor namespaces — a SEALED-record field descriptor (walked by `sealed::release_field`)
/// never carries `KIND_SUMNODE`, and a SUM-NODE variant descriptor (walked by `sumnode::release_field`)
/// never carries `KIND_MAP` (a sum-node variant cannot have a Map field today). A Map field nested
/// inside a sealed record reached from a sum node is released through THAT sealed record's own
/// descriptor via `sealed::release_field`, so the value `4` is always interpreted in the right walk.
pub const KIND_MAP: u32 = 4;
/// Unboxed sum-type (`*SumNode`) field stored in a sealed record. Runtime value is an owned `*SumNode`
/// pointer. Drop → `lin_sumnode_release_self`. Transfer → `clone_sumnode`. Freeze → `lin_sumnode_freeze`.
/// Materialize (NKIND_SUMNODE path) → `lin_sumnode_materialize` → TAG_MAP.
pub const KIND_SUMNODE_FIELD: u32 = 5;

/// Read the HEAP descriptor pointer from a sealed struct's header (offset 8). Null = no heap fields.
#[inline]
unsafe fn desc_of(ptr: *const u8) -> *const u8 {
    *((ptr.add(8)) as *const *const u8)
}

/// Allocate a sealed-record struct of `size` total bytes (header + packed fields), zero-initialised,
/// with refcount 1, the byte `size` at offset 4, the heap-field descriptor `heap_desc` at offset 8,
/// and the named-field descriptor `named_desc` at offset 16. Either descriptor may be NULL.
/// `size` is computed by codegen as `SEALED_HEADER + payload`. Aborts on allocation failure.
/// Always 8-aligned. Heap field slots start NULL (a valid, releasable state) until stored.
///
/// Stage 6a: extended to store BOTH descriptors in the header. The heap descriptor (offset 8) is
/// used by drop/transfer; the named descriptor (offset 16) is used by lin_record_get_field for
/// dynamic key lookup on a TAG_RECORD value.
#[no_mangle]
pub extern "C" fn lin_sealed_alloc(size: usize, heap_desc: *const u8, named_desc: *const u8) -> *mut u8 {
    let size = size.max(SEALED_HEADER);
    unsafe {
        let layout = Layout::from_size_align_unchecked(size, 8);
        let ptr = alloc(layout);
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        // Zero the whole block so any unwritten padding/field bytes (incl. heap-field ptr slots
        // that codegen has not yet stored) are deterministic NULL.
        std::ptr::write_bytes(ptr, 0, size);
        let words = ptr as *mut u32;
        *words = 1; // refcount @ 0
        *words.add(1) = size as u32; // size @ 4
        *((ptr.add(8)) as *mut *const u8) = heap_desc; // heap_desc @ 8
        *((ptr.add(16)) as *mut *const u8) = named_desc; // named_desc @ 16
        ptr
    }
}

/// Release one heap field stored at `ptr` (an owned heap payload pointer) according to its `kind`.
/// A NULL payload is a no-op (an unwritten/cleared heap slot). Used by the descriptor walk on drop.
#[inline]
unsafe fn release_field(payload: *mut u8, kind: u32) {
    if payload.is_null() {
        return;
    }
    match kind {
        KIND_STRING => crate::string::lin_string_release(payload as *mut crate::string::LinString),
        KIND_ARRAY => crate::array::lin_array_release(payload as *mut crate::array::LinArray),
        KIND_SEALED => lin_sealed_release_self(payload),
        KIND_MAP => crate::map::lin_map_release(payload as *mut crate::map::LinMap),
        KIND_SUMNODE_FIELD => crate::sumnode::lin_sumnode_release_self(payload),
        // KIND_SCALAR / unknown: never recorded in a descriptor; defensively a no-op.
        _ => {}
    }
}

/// Walk a sealed struct's field descriptor and release every heap field. Called by
/// `lin_sealed_release` exactly once, when the refcount reaches zero, BEFORE freeing the block.
/// No-op when the descriptor pointer is NULL (a scalar-only record).
#[inline]
unsafe fn release_heap_fields(ptr: *mut u8) {
    let desc = desc_of(ptr);
    if desc.is_null() {
        return;
    }
    let count = *(desc as *const u32);
    // Entries begin after the u32 count; each entry is { u32 offset, u32 kind } = 8 bytes.
    let entries = desc.add(4);
    for i in 0..count as usize {
        let ent = entries.add(i * 8);
        let offset = *(ent as *const u32) as usize;
        let kind = *((ent.add(4)) as *const u32);
        // The field slot holds an owned heap-payload pointer (8 bytes).
        let slot = ptr.add(offset) as *mut *mut u8;
        release_field(*slot, kind);
    }
}

/// Release a sealed-record struct: decrement its refcount and, on reaching zero, release each heap
/// field (per the descriptor) THEN free the allocation. `size` is the same total byte size passed to
/// `lin_sealed_alloc` (codegen knows it statically per type). Null- and (degenerate)
/// zero-refcount-safe. For a scalar-only record the descriptor is NULL and this is just a refcount
/// decrement + free, identical to Stage 1.
#[no_mangle]
pub unsafe extern "C" fn lin_sealed_release(ptr: *mut u8, size: usize) {
    if crate::memory::RC_COUNT_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        if !ptr.is_null() { crate::memory::SEALED_RELEASE_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
    }
    if ptr.is_null() {
        return;
    }
    let rc = ptr as *mut u32;
    if *rc == 0 {
        return;
    }
    // Immortal / stack-allocated sealed records (sealed-records Stage 4) carry a saturated refcount
    // (>= IMMORTAL_RC). They live on the stack (an entry-block alloca reused across a TCO loop) and
    // are NEVER heap-freed — a `dealloc` of a stack pointer is memory corruption. Mirror of the
    // sentinel guard in `lin_rc_retain` / `lin_string_release`: an immortal record is inert to RC,
    // so any Retain/Release the codegen owning model emits on it (RC suppression should remove these
    // in the hot path, but this is defense-in-depth) is a safe no-op. The escape analysis (lin-ir
    // `escape.rs`) is what guarantees such a value never outlives its frame.
    if *rc >= crate::string::IMMORTAL_RC {
        return;
    }
    *rc -= 1;
    if *rc == 0 {
        release_heap_fields(ptr);
        let size = size.max(SEALED_HEADER);
        let layout = Layout::from_size_align_unchecked(size, 8);
        dealloc(ptr, layout);
    }
}

/// Walk a field descriptor over an element PAYLOAD (header-less: a sealed-record array element,
/// which stores only the packed fields, no 16-byte header) and release each heap field. The
/// descriptor records STRUCT-relative offsets (from the standalone struct base, including the
/// header), so each is rebased to the payload by subtracting `SEALED_HEADER`. NULL descriptor (a
/// scalar-only record) → no heap fields → no-op. Used on sealed-record array drop (Stage 3b).
#[inline]
unsafe fn release_payload_fields(payload: *mut u8, desc: *const u8) {
    if desc.is_null() {
        return;
    }
    let count = *(desc as *const u32);
    let entries = desc.add(4);
    for i in 0..count as usize {
        let ent = entries.add(i * 8);
        let offset = *(ent as *const u32) as usize;
        let kind = *((ent.add(4)) as *const u32);
        // Element offsets are payload-relative; the descriptor stores struct-relative offsets.
        let slot = payload.add(offset - SEALED_HEADER) as *mut *mut u8;
        release_field(*slot, kind);
    }
}

/// Public wrapper: release the heap fields of one element payload (for the overwrite-old path in
/// `lin_sealed_array_set`). NULL desc → no-op.
#[no_mangle]
pub unsafe extern "C" fn release_payload_fields_pub(payload: *mut u8, desc: *const u8) {
    release_payload_fields(payload, desc);
}

/// Release every heap field of every element of a sealed-record array on array drop. `data` is the
/// element buffer, `len` the element count, `stride` the per-element byte size, `desc` the field
/// descriptor (NULL for a scalar-only record → no-op). Called by `lin_array_release` for the
/// `SEALED_ARRAY_TAG` case BEFORE the buffer is freed.
pub unsafe fn release_sealed_array_elems(data: *mut u8, len: u64, stride: u64, desc: *const u8) {
    if data.is_null() || desc.is_null() {
        return; // scalar-only record (or empty): nothing per-field to release.
    }
    for i in 0..len {
        let payload = data.add((i * stride) as usize);
        release_payload_fields(payload, desc);
    }
}

/// Retain each heap field of a sealed-record array element PAYLOAD per the descriptor. Used by the
/// BORROWED-source push (`lin_sealed_array_push_struct_retaining`): the array slot becomes an
/// independent +1 owner of each heap field while the source struct keeps its own. NULL descriptor
/// (scalar-only) → no-op.
#[no_mangle]
pub unsafe extern "C" fn retain_sealed_payload_fields(payload: *mut u8, desc: *const u8) {
    if payload.is_null() || desc.is_null() {
        return;
    }
    let count = *(desc as *const u32);
    let entries = desc.add(4);
    for i in 0..count as usize {
        let ent = entries.add(i * 8);
        let offset = *(ent as *const u32) as usize;
        let slot = payload.add(offset - SEALED_HEADER) as *const *mut u8;
        let p = *slot;
        if !p.is_null() {
            // All heap field kinds (String/Array/nested-sealed) carry a u32 refcount at offset 0,
            // so the uniform retain (bump the u32) is correct for every kind.
            crate::memory::lin_rc_retain(p as *mut u32);
        }
    }
}

/// Cold path for the inline sealed-release: walks heap fields and frees the allocation. Called
/// ONLY after the inline LLVM code has already decremented the refcount to exactly zero. The IMMORTAL
/// guard and the null/zero-rc guards all fire in the inline path BEFORE the decrement, so this
/// cold path is only reached for live heap allocations with no immortal flag. NOT null-safe.
#[no_mangle]
pub unsafe extern "C" fn lin_sealed_drop_at_zero(ptr: *mut u8, size: usize) {
    release_heap_fields(ptr);
    let size = size.max(SEALED_HEADER);
    let layout = Layout::from_size_align_unchecked(size, 8);
    dealloc(ptr, layout);
}

/// Release a sealed-record struct, reading its byte `size` from the header (offset 4) instead of
/// taking it as an argument. Used where the caller does NOT have the size available — the
/// closure-capture-release walk (`release_captures`) and the thread-transfer release
/// (`release_env_copy`), which have only a one-byte kind per capture, AND the descriptor walk for a
/// nested sealed field (which knows neither the size nor the type). Equivalent to
/// `lin_sealed_release(ptr, *(u32*)(ptr+4))`. Null/zero-rc-safe.
#[no_mangle]
pub unsafe extern "C" fn lin_sealed_release_self(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    let size = *((ptr as *const u32).add(1)) as usize;
    lin_sealed_release(ptr, size);
}

// ── Named full-field descriptor + materialize-on-read (ADR-063 Stage 3b mechanism (i)) ───────────
//
// The heap-only `SealedDesc` above lists only HEAP fields by (offset, kind) — enough for the RC
// drop/retain walks, but NAMELESS and missing scalars. To present a packed HEADER-LESS sealed-record
// element as a keyed `Json` `LinObject` (the boxed reader path — `lin_array_get_tagged` over a 0xFE
// array, or any generic boxed `for`), we need EVERY field's NAME, byte offset and kind. That is the
// NAMED full-field descriptor: a SEPARATE static blob emitted once per sealed type by codegen
// (`Codegen::sealed_named_descriptor`), reached at runtime via `LinArray::elem_named_desc`. The
// heap-only descriptor is left BYTE-IDENTICAL so the existing release/retain walks never regress.
//
// ```text
// NamedDesc  = [ u32 field_count | NamedField * field_count ]   (little-endian, byte-addressed)
// NamedField = [ u32 byte_offset | u32 nkind | u64 nested_named_desc_ptr |
//                u16 name_len | name_bytes(name_len) ]          (variable length; walked, not strided)
// ```
//
// `byte_offset` is STRUCT-relative (from the standalone struct base, including the 16-byte header),
// matching the heap-only descriptor; the element payload is header-less so it is rebased by
// subtracting `SEALED_HEADER`. `nested_named_desc_ptr` is non-NULL only for `NKIND_SEALED` (it points
// at the nested record's own NamedDesc, so materialize recurses). `nkind` is one of the `NKIND_*`
// codes below — they cover SCALARS too (unlike the heap-only `KIND_*`), and their boxing matches
// `Codegen::type_tag` / `box_value` exactly (UInt8/16/32 → INT64-positive; Float32 → FLOAT64).

/// Named-descriptor field kinds — re-exported from `lin_common::tags` (the single source of truth).
/// Both the runtime and codegen reference `lin_common::tags::NKIND_*` directly; these re-exports
/// keep existing call-sites in this file working without qualification changes.
pub use lin_common::tags::{
    NKIND_INT32, NKIND_INT64, NKIND_UINT64, NKIND_FLOAT64, NKIND_FLOAT32, NKIND_BOOL,
    NKIND_STRING, NKIND_ARRAY, NKIND_SEALED, NKIND_MAP, NKIND_SUMNODE,
    NKIND_UINT32, NKIND_UINT16, NKIND_UINT8, NKIND_INT16, NKIND_INT8,
};

/// Read a NamedField row at byte offset `cur` in the blob. Returns the parsed fields and the offset
/// of the NEXT row (so the caller can walk to the next field).
///
/// Layout per row (matches `Codegen::sealed_named_descriptor`):
///   [ u32 offset | u32 nkind | u64 nested_ptr | u16 name_len | name_bytes | pad-to-8 ]
/// Each row is padded to a multiple of 8 bytes (and the blob header is an 8-byte `[u32 count | u32
/// pad]`) so that every `nested_ptr` lands on an 8-byte boundary — macOS ld64 rejects misaligned
/// pointer relocations. Hence the returned next-offset is rounded UP to the next multiple of 8.
#[inline]
unsafe fn read_named_field(base: *const u8, cur: usize) -> (u32, u32, *const u8, &'static str, usize) {
    let offset = u32::from_le_bytes([*base.add(cur), *base.add(cur + 1), *base.add(cur + 2), *base.add(cur + 3)]);
    let nkind = u32::from_le_bytes([*base.add(cur + 4), *base.add(cur + 5), *base.add(cur + 6), *base.add(cur + 7)]);
    let nested = usize::from_le_bytes([
        *base.add(cur + 8), *base.add(cur + 9), *base.add(cur + 10), *base.add(cur + 11),
        *base.add(cur + 12), *base.add(cur + 13), *base.add(cur + 14), *base.add(cur + 15),
    ]) as *const u8;
    let name_len = u16::from_le_bytes([*base.add(cur + 16), *base.add(cur + 17)]) as usize;
    let name_off = cur + 18;
    let name = std::str::from_utf8_unchecked(std::slice::from_raw_parts(base.add(name_off), name_len));
    let next = (name_off + name_len + 7) & !7; // round the row stride up to a multiple of 8
    (offset, nkind, nested, name, next)
}

/// Allocate a fresh standalone sealed struct from a 0xFE inline payload, copy the payload bytes
/// in, retain each heap field (+1 for the struct's ownership), and return it boxed as TAG_RECORD.
///
/// RC contract: `lin_sealed_alloc` gives rc=1; `retain_sealed_payload_fields` retains each heap
/// field (+1 per field); `alloc_tagged(TAG_RECORD, …)` stores the pointer WITHOUT an additional
/// retain — the struct's sole rc=1 reference is transferred into the box. The box owns exactly
/// +1 on the struct; the struct owns exactly +1 on each heap field.
///
/// This mirrors `Codegen::sealed_array_materialize_elem` 0xFE branch exactly (alloc + memcpy +
/// retain_sealed_payload_fields), but at runtime (no static type info — heap desc is derived
/// from the named desc via the memoised `build_heap_desc_from_named_desc`).
pub unsafe fn sealed_elem_payload_to_record_box(
    payload: *const u8,
    named_desc: *const u8,
    stride: u32,
) -> *mut crate::tagged::TaggedVal {
    use crate::tagged::{TAG_RECORD, alloc_tagged};
    let heap_desc = build_heap_desc_from_named_desc(named_desc);
    let sptr = lin_sealed_alloc(SEALED_HEADER + stride as usize, heap_desc, named_desc);
    let dst_payload = sptr.add(SEALED_HEADER);
    std::ptr::copy_nonoverlapping(payload, dst_payload, stride as usize);
    retain_sealed_payload_fields(dst_payload, heap_desc);
    // alloc_tagged does NOT retain — struct stays at rc=1, box is the sole owner.
    alloc_tagged(TAG_RECORD, sptr as u64) as *mut crate::tagged::TaggedVal
}

/// WRITE-direction inverse of `materialize_named_payload` (ADR-063 mechanism (i), the fail-safe
/// boxed view): pack a keyed `LinObject`'s fields into the HEADER-LESS packed element `slot` of a
/// 0xFE sealed-record array, via the array's NAMED descriptor.
///
/// Why this exists: the `sealed` bit is deliberately NOT part of type identity
/// (`Object{sealed:true} == Object{sealed:false}`, lin-check types.rs), so a statically
/// record-typed array can be EITHER representation at runtime — e.g. a packed `Pt[]` stored as a
/// `{ String: Pt[] }` map value and fetched back through the generic `get<T, D>` seam loses the
/// seal bit, and `push`/`set` lower to the TAGGED write path. The READ side already dispatches on
/// `elem_tag == 0xFE` at the sink (`lin_array_get_tagged` → `sealed_elem_payload_to_record_box`); this
/// is the matching WRITE-side adapter. Without it the tagged sinks blind-wrote 16-byte TaggedVal
/// slots into the stride-sized packed buffer (heap-buffer overflow → `double free or corruption`
/// at drop), and `lin_push_dyn` silently DROPPED the element (`_ => {}`).
///
/// Field semantics mirror `materialize_named_payload` exactly, inverted:
/// - scalar kinds are numerically coerced from the boxed field's runtime tag (the object may carry
///   `TAG_INT64`-boxed values after a Json round-trip), matching `lin_push_dyn`'s flat coercions;
/// - a missing field / `TAG_NULL` writes a zero scalar / NULL heap pointer (defensive, mirroring
///   the read direction's null handling);
/// - heap kinds (String/Array/Map) RETAIN the field pointer into the slot — `obj` keeps its own
///   reference, so the CALLER decides whether to consume `obj`'s ownership afterwards (the move
///   sinks `lin_array_push`/`lin_array_push_tagged`/`lin_array_set` release `obj` once; the
///   retaining sink `lin_push_dyn` leaves it to the caller);
/// - `NKIND_SEALED` (a nested standalone sealed-struct field) faults loudly: re-building a nested
///   sealed STRUCT from a boxed object needs the nested KIND descriptor + alloc size, which the
///   NamedDesc row does not carry. Unreachable while the packing gate
///   (`Type::is_sealed_array_field_packable`) admits scalars + Bool only; the gate-widening work
///   must extend this before admitting nested records.
/// Shared implementation: pack one field per descriptor entry using a caller-supplied lookup
/// function `get_tv(key) -> *const TaggedVal` (borrowed interior pointer or null).
unsafe fn pack_named_payload_impl(
    slot: *mut u8,
    named_desc: *const u8,
    get_tv: impl Fn(*const crate::string::LinString) -> *const crate::tagged::TaggedVal,
) {
    use crate::tagged::{
        TAG_NULL, TAG_INT32, TAG_INT64, TAG_UINT64, TAG_FLOAT32, TAG_FLOAT64, TAG_BOOL,
        TAG_STR, TAG_ARRAY, TAG_MAP, TAG_RECORD,
    };
    if named_desc.is_null() {
        crate::fault::runtime_fault(
            "Runtime error: internal — sealed-record array without a named descriptor cannot accept a boxed element",
        );
    }
    let field_count = u32::from_le_bytes([*named_desc, *named_desc.add(1), *named_desc.add(2), *named_desc.add(3)]) as usize;
    let mut cur = 8usize; // skip the 8-byte header [u32 field_count | u32 pad]
    for _ in 0..field_count {
        let (offset, nkind, nested, name, next) = read_named_field(named_desc, cur);
        cur = next;
        let dst = slot.add(offset as usize - SEALED_HEADER);
        // Intern the field-name string once per type (same rationale as materialize_named_payload_to_map).
        let key = crate::string::lin_string_literal(name.as_ptr(), name.len() as u32);
        let tv = get_tv(key);
        let (tag, payload) = if tv.is_null() { (TAG_NULL, 0u64) } else { ((*tv).tag, (*tv).payload) };
        match nkind {
            NKIND_INT32 => {
                let v: i32 = match tag {
                    TAG_INT32 => payload as i32,
                    TAG_INT64 | TAG_UINT64 => payload as i64 as i32,
                    TAG_FLOAT64 => f64::from_bits(payload) as i32,
                    _ => 0,
                };
                *(dst as *mut i32) = v;
            }
            // Narrow signed ints — sign-truncate from i32 to i16/i8.
            NKIND_INT16 => {
                let v: i16 = match tag {
                    TAG_INT32 => payload as i32 as i16,
                    TAG_INT64 | TAG_UINT64 => payload as i64 as i16,
                    TAG_FLOAT64 => f64::from_bits(payload) as i16,
                    _ => 0,
                };
                *(dst as *mut i16) = v;
            }
            NKIND_INT8 => {
                let v: i8 = match tag {
                    TAG_INT32 => payload as i32 as i8,
                    TAG_INT64 | TAG_UINT64 => payload as i64 as i8,
                    TAG_FLOAT64 => f64::from_bits(payload) as i8,
                    _ => 0,
                };
                *(dst as *mut i8) = v;
            }
            NKIND_INT64 => {
                let v: i64 = match tag {
                    TAG_INT32 => payload as i32 as i64,
                    TAG_INT64 | TAG_UINT64 => payload as i64,
                    TAG_FLOAT64 => f64::from_bits(payload) as i64,
                    _ => 0,
                };
                *(dst as *mut i64) = v;
            }
            // Narrow unsigned ints — zero-truncate from u64 to u32/u16/u8.
            NKIND_UINT32 => {
                let v: u32 = match tag {
                    TAG_INT32 => payload as i32 as u32,
                    TAG_INT64 | TAG_UINT64 => payload as u32,
                    TAG_FLOAT64 => f64::from_bits(payload) as u32,
                    _ => 0,
                };
                *(dst as *mut u32) = v;
            }
            NKIND_UINT16 => {
                let v: u16 = match tag {
                    TAG_INT32 => payload as i32 as u16,
                    TAG_INT64 | TAG_UINT64 => payload as u16,
                    TAG_FLOAT64 => f64::from_bits(payload) as u16,
                    _ => 0,
                };
                *(dst as *mut u16) = v;
            }
            NKIND_UINT8 => {
                let v: u8 = match tag {
                    TAG_INT32 => payload as i32 as u8,
                    TAG_INT64 | TAG_UINT64 => payload as u8,
                    TAG_FLOAT64 => f64::from_bits(payload) as u8,
                    _ => 0,
                };
                *dst = v;
            }
            NKIND_UINT64 => {
                let v: u64 = match tag {
                    TAG_INT32 => payload as i32 as i64 as u64,
                    TAG_INT64 | TAG_UINT64 => payload,
                    TAG_FLOAT64 => f64::from_bits(payload) as u64,
                    _ => 0,
                };
                *(dst as *mut u64) = v;
            }
            NKIND_FLOAT64 => {
                let v: f64 = match tag {
                    TAG_FLOAT64 => f64::from_bits(payload),
                    TAG_FLOAT32 => f32::from_bits(payload as u32) as f64,
                    TAG_INT32 => payload as i32 as f64,
                    TAG_INT64 | TAG_UINT64 => payload as i64 as f64,
                    _ => 0.0,
                };
                *(dst as *mut f64) = v;
            }
            NKIND_FLOAT32 => {
                // Physical slot is 4-byte f32; coerce from the boxed f64/f32 representation.
                let v: f32 = match tag {
                    TAG_FLOAT64 => f64::from_bits(payload) as f32,
                    TAG_FLOAT32 => f32::from_bits(payload as u32),
                    TAG_INT32 => payload as i32 as f32,
                    TAG_INT64 | TAG_UINT64 => payload as i64 as f32,
                    _ => 0.0,
                };
                *(dst as *mut f32) = v;
            }
            NKIND_BOOL => {
                *dst = (tag == TAG_BOOL && payload != 0) as u8;
            }
            NKIND_STRING | NKIND_ARRAY | NKIND_MAP => {
                let expect = match nkind {
                    NKIND_STRING => TAG_STR,
                    NKIND_ARRAY => TAG_ARRAY,
                    _ => TAG_MAP,
                };
                let p = if tag == expect { payload as *mut u8 } else { std::ptr::null_mut() };
                if !p.is_null() {
                    // The slot takes its OWN reference; `obj` keeps its field reference untouched.
                    crate::memory::lin_rc_retain(p as *mut u32);
                }
                *(dst as *mut *mut u8) = p;
            }
            NKIND_SEALED => {
                // Nested sealed record: the source field is either a TAG_MAP (materialized
                // via the boxed round-trip) or a TAG_RECORD (a direct sealed-struct pointer).
                // Either way we produce a fresh +1-owned sealed struct and store its pointer.
                let nested_ptr: *mut u8 = if tag == TAG_MAP {
                    let src_map = payload as *const crate::map::LinMap;
                    let heap_desc = build_heap_desc_from_named_desc(nested);
                    alloc_sealed_struct_from_map(src_map, nested, heap_desc)
                } else if tag == TAG_RECORD {
                    // Source is already a sealed struct; retain it so the slot owns a +1.
                    let p = payload as *mut u8;
                    crate::memory::lin_rc_retain(p as *mut u32);
                    p
                } else {
                    std::ptr::null_mut()
                };
                *(dst as *mut *mut u8) = nested_ptr;
            }
            NKIND_SUMNODE => {
                crate::fault::runtime_fault(
                    "Runtime error: internal — packing a sum-type field from a boxed element is not supported (extend pack_named_payload_impl with NKIND_SUMNODE when the array gate admits sum fields)",
                );
            }
            _ => {
                crate::fault::runtime_fault(
                    "Runtime error: internal — unknown sealed named-descriptor field kind",
                );
            }
        }
    }
}


/// Pack sealed-record payload from a `LinMap` (TAG_MAP) source — Phase 2 open objects.
pub unsafe fn pack_named_payload_from_map(
    slot: *mut u8,
    map: *const crate::map::LinMap,
    named_desc: *const u8,
) {
    pack_named_payload_impl(slot, named_desc, |key| {
        crate::map::lin_map_get(map, key)
    });
}

/// Compute the total struct size (header + payload) from a named descriptor, for dynamic alloc
/// paths that don't have the size statically. Walks the named descriptor to find the maximum
/// `byte_offset + field_byte_size` across all fields, pads to 8-byte alignment. Returns
/// `SEALED_HEADER` (16) minimum (an empty record). NULL descriptor → 16.
unsafe fn struct_size_from_named_desc(named_desc: *const u8) -> usize {
    if named_desc.is_null() {
        return SEALED_HEADER;
    }
    let field_count = u32::from_le_bytes([*named_desc, *named_desc.add(1), *named_desc.add(2), *named_desc.add(3)]) as usize;
    let mut max_end: usize = SEALED_HEADER;
    let mut cur = 8usize; // skip the 8-byte header [u32 field_count | u32 pad]
    for _ in 0..field_count {
        let (offset, nkind, _nested, _name, next) = read_named_field(named_desc, cur);
        cur = next;
        let field_size: usize = lin_common::tags::nkind_size_align(nkind).0 as usize;
        let end = offset as usize + field_size;
        if end > max_end { max_end = end; }
    }
    // Pad to 8-byte alignment.
    (max_end + 7) & !7
}

/// Build a heap-only field descriptor from a named descriptor, for the dynamic alloc path on
/// 0xFD pointer-backed arrays (Stage 2a: heap fields like String/Array/Map). The returned
/// pointer is valid for the lifetime of the process (allocated once per distinct named_desc
/// type, memoised in HEAP_DESC_MEMO keyed on the stable static named_desc pointer). Returns
/// NULL if there are no heap fields (scalar-only type → no descriptor needed).
pub unsafe fn build_heap_desc_from_named_desc(named_desc: *const u8) -> *const u8 {
    if named_desc.is_null() {
        return std::ptr::null();
    }
    let key = named_desc as usize;
    // Fast path: check the memo under lock before doing any work.
    {
        let guard = HEAP_DESC_MEMO.lock().unwrap();
        if let Some(ref map) = *guard {
            if let Some(&cached) = map.get(&key) {
                return cached as *const u8;
            }
        }
    }
    let field_count = u32::from_le_bytes([
        *named_desc, *named_desc.add(1), *named_desc.add(2), *named_desc.add(3),
    ]) as usize;
    // Collect heap fields: (offset, kind) pairs.
    let mut heap_fields: Vec<(u32, u32)> = Vec::new();
    let mut cur = 8usize; // skip the 8-byte header [u32 field_count | u32 pad]
    for _ in 0..field_count {
        let (offset, nkind, _nested, _name, next) = read_named_field(named_desc, cur);
        cur = next;
        let kind = match nkind {
            NKIND_STRING => KIND_STRING,
            NKIND_ARRAY  => KIND_ARRAY,
            NKIND_MAP    => KIND_MAP,
            NKIND_SEALED => KIND_SEALED,
            // scalars and Bool need no heap descriptor entry
            _ => continue,
        };
        heap_fields.push((offset, kind));
    }
    let result_ptr: *const u8 = if heap_fields.is_empty() {
        std::ptr::null()
    } else {
        // Build the heap descriptor blob: [ u32 count | { u32 offset, u32 kind } * count ]
        let byte_len = 4 + heap_fields.len() * 8;
        let layout = std::alloc::Layout::from_size_align_unchecked(byte_len, 4);
        let ptr = std::alloc::alloc(layout);
        if ptr.is_null() { std::alloc::handle_alloc_error(layout); }
        *(ptr as *mut u32) = heap_fields.len() as u32;
        for (i, (off, kind)) in heap_fields.iter().enumerate() {
            let ent = ptr.add(4 + i * 8) as *mut u32;
            *ent = *off;
            *ent.add(1) = *kind;
        }
        ptr as *const u8
    };
    // Store in the memo (initialising the map on first use).
    let mut guard = HEAP_DESC_MEMO.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    // Another thread may have raced and inserted while we built; prefer the winner's allocation
    // to avoid a double-free: if already present, free the blob we just built and return theirs.
    if let Some(&existing) = map.get(&key) {
        if !result_ptr.is_null() {
            let byte_len = 4 + heap_fields.len() * 8;
            let layout = std::alloc::Layout::from_size_align_unchecked(byte_len, 4);
            std::alloc::dealloc(result_ptr as *mut u8, layout);
        }
        return existing as *const u8;
    }
    map.insert(key, result_ptr as usize);
    result_ptr
}


/// Allocate a fresh sealed struct from a `LinMap` (Phase 2 open objects) using the named
/// descriptor. For the dynamic push/set path when the source is a TAG_MAP.
pub unsafe fn alloc_sealed_struct_from_map(
    map: *const crate::map::LinMap,
    named_desc: *const u8,
    heap_desc: *const u8,
) -> *mut u8 {
    let size = struct_size_from_named_desc(named_desc);
    let sptr = lin_sealed_alloc(size, heap_desc, named_desc);
    // Verify that our dynamically-reconstructed size matches what lin_sealed_alloc stored in the
    // header (offset 4). A mismatch means nkind_size_align diverged from codegen's sealed_slot_size.
    debug_assert_eq!(
        *((sptr.add(4)) as *const u32) as usize,
        size,
        "sealed alloc size mismatch: nkind_size_align reconstruction ({size}) != header stored size"
    );
    pack_named_payload_from_map(sptr.add(SEALED_HEADER), map, named_desc);
    sptr
}

/// Box one field of a sealed struct (identified by its struct-relative `offset`, `nkind`, and
/// `nested` named-desc pointer) into a fresh +1-owned TaggedVal*. The caller OWNS the returned
/// box and must `lin_tagged_release` it.
///
/// `sealed` is the struct base pointer (WITH header, struct-relative offsets).
/// This is the same per-nkind boxing that was previously inlined in `lin_record_get_field`, now
/// extracted so both the field-lookup and the descriptor-walk view paths can share it.
///
/// RC contract:
///  - Scalars: value copied into a fresh box; no inner heap payload.
///  - String/Array/Map: slot pointer RETAINED before boxing (+1 for the caller; struct keeps its +1).
///  - Sealed (nested struct): `lin_box_record` retains (+1); Stage-3 lazy (no intermediate LinMap).
///  - SumNode: materialized → fresh LinMap → TAG_MAP.
///  - Null slot (heap kinds): returns null_mut() (caller treats as absent/null field).
pub(crate) unsafe fn box_field_value(
    sealed: *const u8,
    offset: u32,
    nkind: u32,
    nested: *const u8,
) -> *mut u8 {
    use crate::tagged::{TAG_INT32, TAG_INT64, TAG_UINT64, TAG_FLOAT64, TAG_BOOL, TAG_MAP, alloc_tagged};
    let slot = sealed.add(offset as usize);
    let _ = nested; // carried for API symmetry; NKIND_SEALED uses lin_box_record (reads desc from struct header)
    match nkind {
        NKIND_INT32 => {
            let v = *(slot as *const i32);
            alloc_tagged(TAG_INT32, v as i64 as u64)
        }
        NKIND_INT16 => {
            let v = *(slot as *const i16) as i32;
            alloc_tagged(TAG_INT32, v as i64 as u64)
        }
        NKIND_INT8 => {
            let v = *(slot as *const i8) as i32;
            alloc_tagged(TAG_INT32, v as i64 as u64)
        }
        NKIND_INT64 => {
            let v = *(slot as *const i64);
            alloc_tagged(TAG_INT64, v as u64)
        }
        NKIND_UINT32 => {
            let v = *(slot as *const u32) as u64;
            alloc_tagged(TAG_INT64, v)
        }
        NKIND_UINT16 => {
            let v = *(slot as *const u16) as u64;
            alloc_tagged(TAG_INT64, v)
        }
        NKIND_UINT8 => {
            let v = *(slot as *const u8) as u64;
            alloc_tagged(TAG_INT64, v)
        }
        NKIND_UINT64 => {
            let v = *(slot as *const u64);
            alloc_tagged(TAG_UINT64, v)
        }
        NKIND_FLOAT64 => {
            let v = *(slot as *const f64);
            alloc_tagged(TAG_FLOAT64, v.to_bits())
        }
        NKIND_FLOAT32 => {
            let v = *(slot as *const f32) as f64;
            alloc_tagged(TAG_FLOAT64, v.to_bits())
        }
        NKIND_BOOL => {
            let v = *(slot as *const u8);
            alloc_tagged(TAG_BOOL, (v != 0) as u64)
        }
        NKIND_STRING => {
            let p = *(slot as *const *mut u8);
            if p.is_null() { return std::ptr::null_mut(); }
            crate::memory::lin_rc_retain(p as *mut u32);
            crate::tagged::lin_box_str(p)
        }
        NKIND_ARRAY => {
            let p = *(slot as *const *mut u8);
            if p.is_null() { return std::ptr::null_mut(); }
            crate::memory::lin_rc_retain(p as *mut u32);
            crate::tagged::lin_box_array(p)
        }
        NKIND_MAP => {
            let p = *(slot as *const *mut u8);
            if p.is_null() { return std::ptr::null_mut(); }
            crate::memory::lin_rc_retain(p as *mut u32);
            crate::tagged::lin_box_map(p)
        }
        NKIND_SEALED => {
            let p = *(slot as *const *mut u8);
            if p.is_null() { return std::ptr::null_mut(); }
            crate::tagged::lin_box_record(p)
        }
        NKIND_SUMNODE => {
            let p = *(slot as *const *mut u8);
            if p.is_null() { return std::ptr::null_mut(); }
            let sum_map = crate::sumnode::lin_sumnode_materialize(p);
            if sum_map.is_null() { return std::ptr::null_mut(); }
            alloc_tagged(TAG_MAP, sum_map as u64)
        }
        _ => std::ptr::null_mut(),
    }
}

/// Walk every field in a named descriptor, invoking `f(name_bytes, offset, nkind, nested_desc)`
/// once per field in descriptor order. `named_desc` may be NULL (→ no iterations).
///
/// The descriptor blob layout (see SEALED_HEADER docs):
///   `[u32 count | u32 pad | {u32 offset, u32 nkind, u64 nested_ptr, u16 name_len, name_bytes,
///     pad-to-8} * count]`
///
/// `offset` is struct-relative (includes the SEALED_HEADER), matching the convention of
/// `lin_record_get_field` and `sealed_elem_payload_to_record_box`.
pub(crate) unsafe fn record_walk_fields(
    named_desc: *const u8,
    mut f: impl FnMut(&[u8], u32, u32, *const u8),
) {
    if named_desc.is_null() {
        return;
    }
    let field_count = u32::from_le_bytes([
        *named_desc, *named_desc.add(1), *named_desc.add(2), *named_desc.add(3),
    ]) as usize;
    let mut cur = 8usize;
    for _ in 0..field_count {
        let (offset, nkind, nested, name, next) = read_named_field(named_desc, cur);
        cur = next;
        f(name.as_bytes(), offset, nkind, nested);
    }
}

/// Stage 6a: look up one field by name in a TAG_RECORD sealed struct and return an OWNED +1
/// TaggedVal* for that field (or null for a missing/null field).
#[no_mangle]
pub unsafe extern "C" fn lin_record_get_field(sealed: *const u8, key: *const crate::string::LinString) -> *mut u8 {
    if sealed.is_null() || key.is_null() {
        return std::ptr::null_mut();
    }
    let named_desc = *((sealed.add(16)) as *const *const u8);
    if named_desc.is_null() {
        return std::ptr::null_mut();
    }
    let key_bytes = std::slice::from_raw_parts((*key).data.as_ptr(), (*key).len as usize);
    let mut result: *mut u8 = std::ptr::null_mut();
    let mut found = false;
    record_walk_fields(named_desc, |name, offset, nkind, nested| {
        if found || name != key_bytes {
            return;
        }
        found = true;
        result = box_field_value(sealed, offset, nkind, nested);
    });
    result
}

#[cfg(test)]
mod named_desc_tests {
    //! ADR-063 Stage 3b mechanism (i): exercise `lin_array_get_tagged`'s 0xFE materialize-on-read
    //! branch. The gate is scalar-only, so no corpus `.lin` program drives a 0xFE array through the
    //! DYNAMIC boxed reader yet — these tests hand-build a 0xFE `LinArray` + a NAMED descriptor (the
    //! exact byte layout `Codegen::sealed_named_descriptor` emits) and assert get_tagged returns a
    //! correct keyed object that is RC-balanced (run under ASan to judge UAF/leak).

    use super::*;
    use crate::tagged::{TAG_MAP, TAG_INT32, TAG_STR, TAG_FLOAT64, TAG_RECORD};

    /// Build a NamedDesc byte blob matching `Codegen::sealed_named_descriptor`:
    /// `[u32 count | u32 pad | { u32 offset, u32 nkind, u64 nested_ptr, u16 name_len, name_bytes,
    ///   pad-to-8 } * count]`. The 8-byte header + per-row pad-to-8 keep every `nested_ptr` 8-aligned
    /// (macOS ld64 requires it); the runtime walks the blob byte-by-byte, rounding each row up to 8.
    fn build_named_desc(fields: &[(&str, u32, u32, *const u8)]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&(fields.len() as u32).to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes()); // header pad → 8-byte header
        for (name, offset, nkind, nested) in fields {
            b.extend_from_slice(&offset.to_le_bytes());
            b.extend_from_slice(&nkind.to_le_bytes());
            b.extend_from_slice(&(*nested as u64).to_le_bytes());
            b.extend_from_slice(&(name.len() as u16).to_le_bytes());
            b.extend_from_slice(name.as_bytes());
            let row_len = 18 + name.len();
            let pad_len = (8 - (row_len % 8)) % 8;
            b.extend(std::iter::repeat(0u8).take(pad_len)); // pad row to a multiple of 8
        }
        b
    }

    // SCALAR record P { x: Int32, y: Int32 }. Header 24, x@24, y@28, stride = 8.
    #[test]
    fn get_tagged_materializes_scalar_record() {
        unsafe {
            let named = build_named_desc(&[
                ("x", 24, NKIND_INT32, std::ptr::null()),
                ("y", 28, NKIND_INT32, std::ptr::null()),
            ]);
            let stride = 8u64;
            // Heap-only descriptor is NULL for a scalar-only record.
            let arr = crate::array::lin_sealed_array_alloc(4, stride, std::ptr::null(), named.as_ptr());
            // Push two elements by writing standalone structs and copying their payloads.
            for (xv, yv) in [(11i32, 22i32), (-3i32, 7i32)] {
                let st = lin_sealed_alloc(SEALED_HEADER + stride as usize, std::ptr::null(), std::ptr::null());
                *((st.add(24)) as *mut i32) = xv;
                *((st.add(28)) as *mut i32) = yv;
                crate::array::lin_sealed_array_push_struct(arr, st);
                lin_sealed_release_self(st);
            }
            // Read element 1 via the DYNAMIC boxed reader (the new 0xFE branch).
            let tv = crate::array::lin_array_get_tagged(arr, 1);
            assert!(!tv.is_null());
            // Stage 4c: 0xFE elements are now TAG_RECORD (lazy sealed struct), not TAG_MAP.
            assert_eq!((*tv).tag, TAG_RECORD);
            let sealed = (*tv).payload as *const u8;
            let kx = crate::string::lin_string_from_bytes(b"x".as_ptr(), 1);
            let ky = crate::string::lin_string_from_bytes(b"y".as_ptr(), 1);
            // lin_record_get_field returns an owned +1 TaggedVal* — must be released.
            let fx = lin_record_get_field(sealed, kx) as *const crate::tagged::TaggedVal;
            let fy = lin_record_get_field(sealed, ky) as *const crate::tagged::TaggedVal;
            assert!(!fx.is_null());
            assert!(!fy.is_null());
            assert_eq!((*fx).tag, TAG_INT32);
            assert_eq!((*fx).payload as i32, -3);
            assert_eq!((*fy).tag, TAG_INT32);
            assert_eq!((*fy).payload as i32, 7);
            crate::tagged::lin_tagged_release(fx as *mut u8);
            crate::tagged::lin_tagged_release(fy as *mut u8);
            crate::string::lin_string_release(kx);
            crate::string::lin_string_release(ky);
            // Caller owns the box: release it (frees the sealed struct — scalar, no heap fields).
            crate::tagged::lin_tagged_release(tv as *mut u8);
            // Array drop (scalar-only: just frees the buffer).
            crate::array::lin_array_release(arr);
        }
    }

    // HEAP-FIELD record R { name: String, n: Int32 }. Header 24, name@24 (8-byte ptr), n@32, stride 16.
    // Proves the materialized object takes its OWN +1 on the shared String (RC balance), and that
    // releasing both the box and the array frees the string exactly once.
    #[test]
    fn get_tagged_materializes_heap_field_record_rc_balanced() {
        unsafe {
            let named = build_named_desc(&[
                ("name", 24, NKIND_STRING, std::ptr::null()),
                ("n", 32, NKIND_INT32, std::ptr::null()),
            ]);
            // Heap-only descriptor: one heap field (name @ offset 24, KIND_STRING).
            let mut heap_desc = Vec::new();
            heap_desc.extend_from_slice(&1u32.to_le_bytes()); // count
            heap_desc.extend_from_slice(&24u32.to_le_bytes()); // offset
            heap_desc.extend_from_slice(&KIND_STRING.to_le_bytes()); // kind
            let stride = 16u64;
            let arr = crate::array::lin_sealed_array_alloc(4, stride, heap_desc.as_ptr(), named.as_ptr());
            // Construct one element: a standalone struct owning a +1 String.
            let s = crate::string::lin_string_from_bytes(b"hello".as_ptr(), 5);
            assert_eq!((*s).refcount, 1);
            let st = lin_sealed_alloc(SEALED_HEADER + stride as usize, heap_desc.as_ptr(), std::ptr::null());
            *((st.add(24)) as *mut *mut u8) = s as *mut u8; // struct owns the +1
            *((st.add(32)) as *mut i32) = 42;
            // BORROWED-source push: array retains each heap field (string rc -> 2).
            crate::array::lin_sealed_array_push_struct_retaining(arr, st);
            assert_eq!((*s).refcount, 2); // struct + array
            // Drop the standalone struct (releases its +1; string rc -> 1, owned only by the array).
            lin_sealed_release_self(st);
            assert_eq!((*s).refcount, 1);

            // DYNAMIC boxed read: Stage 4c returns TAG_RECORD (lazy sealed struct). The struct
            // takes its OWN +1 on the string (rc -> 2) so the packed buffer's reference is independent.
            let tv = crate::array::lin_array_get_tagged(arr, 0);
            // Stage 4c: 0xFE elements are now TAG_RECORD, not TAG_MAP.
            assert_eq!((*tv).tag, TAG_RECORD);
            assert_eq!((*s).refcount, 2); // array + materialized struct
            let sealed = (*tv).payload as *const u8;
            let kname = crate::string::lin_string_from_bytes(b"name".as_ptr(), 4);
            let kn = crate::string::lin_string_from_bytes(b"n".as_ptr(), 1);
            // lin_record_get_field returns owned +1 — retains the string (rc -> 3).
            let fname = lin_record_get_field(sealed, kname) as *const crate::tagged::TaggedVal;
            let fn_ = lin_record_get_field(sealed, kn) as *const crate::tagged::TaggedVal;
            assert!(!fname.is_null());
            assert!(!fn_.is_null());
            assert_eq!((*fname).tag, TAG_STR);
            let sptr = (*fname).payload as *const crate::string::LinString;
            assert_eq!((*sptr).as_str(), "hello");
            assert_eq!((*fn_).tag, TAG_INT32);
            assert_eq!((*fn_).payload as i32, 42);
            // Release field boxes: string rc -> 2.
            crate::tagged::lin_tagged_release(fname as *mut u8);
            crate::tagged::lin_tagged_release(fn_ as *mut u8);
            crate::string::lin_string_release(kname);
            crate::string::lin_string_release(kn);

            // Release the TAG_RECORD box -> frees the struct -> releases its string +1 (rc -> 1).
            crate::tagged::lin_tagged_release(tv as *mut u8);
            assert_eq!((*s).refcount, 1); // only the array now
            // Array drop -> release_sealed_array_elems walks the heap desc -> string rc -> 0, freed.
            crate::array::lin_array_release(arr);
            // (s is now freed; ASan verifies no leak/double-free.)
        }
    }

    // ----- WRITE direction: `pack_named_payload_from_map` through the dynamic/tagged sinks -----
    // Guards the map-value-fetch/push corruption fix — before it, lin_array_push blind-wrote
    // 16-byte TaggedVal slots into the stride-sized packed buffer (heap overflow at the 3rd
    // element) and lin_push_dyn silently dropped the element.

    /// Build a fresh LinMap `{ x: Int32(xv), y: Int32(yv) }` (rc = 1).
    unsafe fn make_map_xy(xv: i32, yv: i32) -> *mut crate::map::LinMap {
        use crate::tagged::TaggedVal;
        let map = crate::map::lin_map_alloc(2, crate::map::KEY_KIND_STRING);
        for (name, v) in [("x", xv), ("y", yv)] {
            let key = crate::string::lin_string_from_bytes(name.as_ptr(), name.len() as u32);
            let tv = TaggedVal { tag: TAG_INT32, _pad: [0; 7], payload: v as i64 as u64 };
            crate::map::lin_map_set(map, key, &tv);
            crate::string::lin_string_release(key);
        }
        map
    }

    // Scalar record P { x: Int32, y: Int32 } pushed through BOTH dynamic write sinks, crossing the
    // initial-capacity growth boundary (the original crash fired on push #3 of a cap-4/stride-8
    // buffer because tagged 16-byte writes filled it after 2). Reads back via the packed elem ptr
    // AND the materializing boxed reader.
    #[test]
    fn dynamic_push_packs_into_sealed_array() {
        unsafe {
            use crate::tagged::TaggedVal;
            // Struct-relative offsets (24-byte SEALED_HEADER): x@payload0 → 24, y@payload4 → 28.
            let named = build_named_desc(&[
                ("x", 24, NKIND_INT32, std::ptr::null()),
                ("y", 28, NKIND_INT32, std::ptr::null()),
            ]);
            let arr = crate::array::lin_sealed_array_alloc(4, 8, std::ptr::null(), named.as_ptr());
            // 5 pushes via lin_push_dyn (RETAINING contract: the caller keeps its map ref).
            for i in 0..5i32 {
                let map = make_map_xy(i, i * 10);
                let tv = TaggedVal { tag: TAG_MAP, _pad: [0; 7], payload: map as u64 };
                crate::array::lin_push_dyn(arr, &tv);
                assert_eq!((*map).refcount, 1, "lin_push_dyn must not consume the caller's ref");
                crate::map::lin_map_release(map);
            }
            assert_eq!((*arr).len, 5);
            // Packed bytes are real field values at the element stride (not TaggedVal slots).
            let p = crate::array::lin_sealed_array_elem_ptr(arr, 4);
            assert_eq!(*(p as *const i32), 4);
            assert_eq!(*(p.add(4) as *const i32), 40);
            // lin_array_push (MOVE contract: consumes one transferred map ref).
            let map = make_map_xy(7, 8);
            crate::memory::lin_rc_retain(map as *mut u32); // the codegen-transferred +1 (rc -> 2)
            let cell: *mut crate::map::LinMap = map;
            crate::array::lin_array_push(arr, &cell as *const _ as *const u8, TAG_MAP);
            assert_eq!((*map).refcount, 1, "lin_array_push must consume exactly the transferred ref");
            crate::map::lin_map_release(map);
            // Roundtrip element 5 through the materializing boxed reader (Stage 4c: TAG_RECORD).
            let tv = crate::array::lin_array_get_tagged(arr, 5);
            assert_eq!((*tv).tag, TAG_RECORD);
            let sealed = (*tv).payload as *const u8;
            let kx = crate::string::lin_string_from_bytes(b"x".as_ptr(), 1);
            let fx = lin_record_get_field(sealed, kx) as *const crate::tagged::TaggedVal;
            assert!(!fx.is_null());
            assert_eq!((*fx).payload as i32, 7);
            crate::tagged::lin_tagged_release(fx as *mut u8);
            crate::string::lin_string_release(kx);
            crate::tagged::lin_tagged_release(tv as *mut u8);
            crate::array::lin_array_release(arr);
        }
    }

    // Two-Float32-field record F { a: Float32, b: Float32 }.
    // Physical layout (24-byte header): a@24 (4 bytes, align 4), b@28 (4 bytes, align 4), total=32.
    // The dynamic boundary must reconstruct total=32 (not 40 as NKIND_FLOAT64 would) and box both
    // fields as TAG_FLOAT64 (fpext). Tests both the materialize path (read) and the
    // alloc_sealed_struct_from_map path (write via struct_size_from_named_desc).
    #[test]
    fn two_float32_fields_round_trip_dynamic_boundary() {
        unsafe {
            use crate::tagged::TaggedVal;
            // Named descriptor: a@24 NKIND_FLOAT32, b@28 NKIND_FLOAT32 (stride=8, total=32).
            let named = build_named_desc(&[
                ("a", 24, NKIND_FLOAT32, std::ptr::null()),
                ("b", 28, NKIND_FLOAT32, std::ptr::null()),
            ]);
            // Verify struct_size_from_named_desc returns 32, not 40 (the old NKIND_FLOAT64 over-size).
            let computed_size = struct_size_from_named_desc(named.as_ptr());
            assert_eq!(computed_size, 32, "struct_size should be 32 for two Float32 fields, got {computed_size}");

            // Build a standalone sealed struct manually (header + two f32 fields).
            let sptr = lin_sealed_alloc(32, std::ptr::null(), named.as_ptr());
            *(sptr.add(24) as *mut f32) = 1.5f32;
            *(sptr.add(28) as *mut f32) = 2.25f32;

            // Verify Float32 fields are boxed as TAG_FLOAT64 via lin_record_get_field.
            let ka = crate::string::lin_string_from_bytes(b"a".as_ptr(), 1);
            let kb = crate::string::lin_string_from_bytes(b"b".as_ptr(), 1);
            let fa = lin_record_get_field(sptr as *const u8, ka) as *const crate::tagged::TaggedVal;
            let fb = lin_record_get_field(sptr as *const u8, kb) as *const crate::tagged::TaggedVal;
            assert!(!fa.is_null(), "field 'a' missing");
            assert!(!fb.is_null(), "field 'b' missing");
            assert_eq!((*fa).tag, TAG_FLOAT64, "Float32 field must box as TAG_FLOAT64");
            assert_eq!((*fb).tag, TAG_FLOAT64, "Float32 field must box as TAG_FLOAT64");
            let va = f64::from_bits((*fa).payload);
            let vb = f64::from_bits((*fb).payload);
            assert!((va - 1.5f64).abs() < 1e-9, "a should be 1.5, got {va}");
            assert!((vb - 2.25f64).abs() < 1e-9, "b should be 2.25, got {vb}");
            crate::tagged::lin_tagged_release(fa as *mut u8);
            crate::tagged::lin_tagged_release(fb as *mut u8);
            crate::string::lin_string_release(ka);
            crate::string::lin_string_release(kb);

            // alloc_sealed_struct_from_map: reconstruct a struct from a map with Float64 values
            // (mirrors what the dynamic boundary sees after fpext boxing).
            let src_map = crate::map::lin_map_alloc(2, crate::map::KEY_KIND_STRING);
            for (name, val) in [("a", 3.0f64), ("b", 4.5f64)] {
                let key = crate::string::lin_string_from_bytes(name.as_ptr(), name.len() as u32);
                let tv = TaggedVal { tag: TAG_FLOAT64, _pad: [0; 7], payload: val.to_bits() };
                crate::map::lin_map_set(src_map, key, &tv);
                crate::string::lin_string_release(key);
            }
            let heap_desc = build_heap_desc_from_named_desc(named.as_ptr());
            let rebuilt = alloc_sealed_struct_from_map(src_map, named.as_ptr(), heap_desc);
            assert!(!rebuilt.is_null());
            // Verify the header size is 32.
            let stored_size = *((rebuilt.add(4)) as *const u32) as usize;
            assert_eq!(stored_size, 32, "rebuilt struct header size should be 32, got {stored_size}");
            // Read back the f32 field values directly.
            let ra = *(rebuilt.add(24) as *const f32);
            let rb = *(rebuilt.add(28) as *const f32);
            assert!((ra - 3.0f32).abs() < 1e-6, "rebuilt a should be 3.0, got {ra}");
            assert!((rb - 4.5f32).abs() < 1e-6, "rebuilt b should be 4.5, got {rb}");

            lin_sealed_release_self(sptr);
            lin_sealed_release_self(rebuilt);
            crate::map::lin_map_release(src_map);
        }
    }

    // Heap-field record R { name: String, n: Int32 }: the pack retains the String into the slot
    // (the source map keeps its own ref), and the materialize/drop chain releases it exactly
    // once each — RC-balanced end to end (ASan judges leak/double-free).
    #[test]
    fn dynamic_push_packs_heap_field_rc_balanced() {
        unsafe {
            use crate::tagged::TaggedVal;
            // Offsets are struct-relative (include the 24-byte SEALED_HEADER): name@payload0 → 24,
            // n@payload8 → 32.
            let named = build_named_desc(&[
                ("name", 24, NKIND_STRING, std::ptr::null()),
                ("n", 32, NKIND_INT32, std::ptr::null()),
            ]);
            let mut heap_desc = Vec::new();
            heap_desc.extend_from_slice(&1u32.to_le_bytes());
            heap_desc.extend_from_slice(&24u32.to_le_bytes());
            heap_desc.extend_from_slice(&KIND_STRING.to_le_bytes());
            let arr = crate::array::lin_sealed_array_alloc(4, 16, heap_desc.as_ptr(), named.as_ptr());
            // Source map { name: "hello", n: 42 } — it owns its own +1 on the string.
            let s = crate::string::lin_string_from_bytes(b"hello".as_ptr(), 5);
            let map = crate::map::lin_map_alloc(2, crate::map::KEY_KIND_STRING);
            let kname = crate::string::lin_string_from_bytes(b"name".as_ptr(), 4);
            let tv_name = TaggedVal { tag: TAG_STR, _pad: [0; 7], payload: s as u64 };
            crate::map::lin_map_set(map, kname, &tv_name); // retains: s rc -> 2
            crate::string::lin_string_release(kname);
            let kn = crate::string::lin_string_from_bytes(b"n".as_ptr(), 1);
            let tv_n = TaggedVal { tag: TAG_INT32, _pad: [0; 7], payload: 42u64 };
            crate::map::lin_map_set(map, kn, &tv_n);
            crate::string::lin_string_release(kn);
            crate::string::lin_string_release(s); // drop our construction ref: map is sole owner (rc 1)
            assert_eq!((*s).refcount, 1);

            let tv = TaggedVal { tag: TAG_MAP, _pad: [0; 7], payload: map as u64 };
            crate::array::lin_push_dyn(arr, &tv); // pack retains the string into the slot (rc -> 2)
            assert_eq!((*s).refcount, 2, "the packed slot must take its OWN string ref");
            crate::map::lin_map_release(map); // source map drops its ref (rc -> 1, array owns)
            assert_eq!((*s).refcount, 1);

            // Stage 4c: get_tagged returns TAG_RECORD; the struct takes its OWN +1 on the string.
            let out = crate::array::lin_array_get_tagged(arr, 0);
            assert_eq!((*s).refcount, 2); // array slot + the fresh struct's +1
            assert_eq!((*out).tag, TAG_RECORD);
            let sealed = (*out).payload as *const u8;
            let k = crate::string::lin_string_from_bytes(b"name".as_ptr(), 4);
            // lin_record_get_field retains the string (rc -> 3); we release the field box after.
            let f = lin_record_get_field(sealed, k) as *const crate::tagged::TaggedVal;
            assert!(!f.is_null());
            assert_eq!((*f).tag, TAG_STR);
            assert_eq!((*((*f).payload as *const crate::string::LinString)).as_str(), "hello");
            crate::tagged::lin_tagged_release(f as *mut u8); // string rc -> 2
            crate::string::lin_string_release(k);
            crate::tagged::lin_tagged_release(out as *mut u8); // struct released → string rc -> 1
            assert_eq!((*s).refcount, 1);
            // Array drop walks the heap desc: string rc -> 0, freed exactly once.
            crate::array::lin_array_release(arr);
        }
    }
}
