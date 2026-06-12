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

/// Header size in bytes: `u32 refcount` + `u32 size` + `u64 desc_ptr`. Field payload begins at
/// offset 16. Kept in lockstep with `Codegen::SEALED_HEADER`.
pub const SEALED_HEADER: usize = 16;

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

/// Read the descriptor pointer from a sealed struct's header (offset 8). Null = no heap fields.
#[inline]
unsafe fn desc_of(ptr: *const u8) -> *const u8 {
    *((ptr.add(8)) as *const *const u8)
}

/// Allocate a sealed-record struct of `size` total bytes (header + packed fields), zero-initialised,
/// with refcount 1, the byte `size` at offset 4, and the field descriptor pointer `desc` at offset 8
/// (NULL for a scalar-only record). `size` is computed by codegen as `SEALED_HEADER + payload`.
/// Aborts on allocation failure. Always 8-aligned. Heap field slots start NULL (a valid, releasable
/// state) until codegen stores the retained payload.
#[no_mangle]
pub extern "C" fn lin_sealed_alloc(size: usize, desc: *const u8) -> *mut u8 {
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
        *((ptr.add(8)) as *mut *const u8) = desc; // desc_ptr @ 8
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

/// Named-descriptor field kinds. MUST stay in lockstep with `Codegen::sealed_named_field_kind`.
pub const NKIND_INT32: u32 = 1; // Int8/Int16/Int32 → lin_box_int32
pub const NKIND_INT64: u32 = 2; // Int64, UInt8/UInt16/UInt32 (zero-extended positive) → lin_box_int64
pub const NKIND_UINT64: u32 = 3; // UInt64 → lin_box_uint64
pub const NKIND_FLOAT64: u32 = 4; // Float32/Float64 → lin_box_float64
pub const NKIND_BOOL: u32 = 5; // Bool → lin_box_bool
pub const NKIND_STRING: u32 = 6; // *LinString heap field → retain + lin_box_str
pub const NKIND_ARRAY: u32 = 7; // *LinArray heap field → retain + lin_box_array
pub const NKIND_SEALED: u32 = 8; // *sealed-struct heap field → recurse via nested NamedDesc
pub const NKIND_MAP: u32 = 9; // *LinMap heap field → retain + lin_box_map

/// Read a NamedField row at byte offset `cur` in the blob. Returns the parsed fields and the offset
/// just past the row (so the caller can walk to the next field).
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
    (offset, nkind, nested, name, name_off + name_len)
}

/// Box one already-loaded heap-field POINTER `p` (non-null) for the named-materialize path. RETAINS
/// the inner payload so the returned box is an independently-owned +1 view — the packed buffer keeps
/// its own +1. `nested` is the nested NamedDesc for `NKIND_SEALED` (else ignored). Returns a fresh
/// heap `TaggedVal*` the caller owns (and must `lin_tagged_release`).
unsafe fn box_named_heap_field(p: *mut u8, nkind: u32, nested: *const u8) -> *mut u8 {
    use crate::tagged::{TAG_OBJECT, lin_box_str, lin_box_array, lin_box_map, alloc_tagged};
    match nkind {
        NKIND_STRING => {
            crate::memory::lin_rc_retain(p as *mut u32);
            lin_box_str(p)
        }
        NKIND_ARRAY => {
            crate::memory::lin_rc_retain(p as *mut u32);
            lin_box_array(p)
        }
        NKIND_MAP => {
            crate::memory::lin_rc_retain(p as *mut u32);
            lin_box_map(p)
        }
        NKIND_SEALED => {
            // A nested sealed record stored as a STANDALONE struct (with header). Recurse to a fresh
            // boxed LinObject so the materialized view is uniform Json. Its heap fields are retained
            // by the recursive materialize; the parent's pointer keeps its own +1, untouched.
            let nested_obj = materialize_sealed_struct(p, nested);
            alloc_tagged(TAG_OBJECT, nested_obj as u64)
        }
        // A scalar kind should never reach here.
        _ => crate::tagged::lin_box_null(),
    }
}

/// Materialize a HEADER-LESS packed element payload `payload` (the `data + idx*stride` interior
/// pointer) into a fresh +1-owned keyed `LinObject` via the NAMED descriptor `named_desc`. Each
/// scalar field is boxed by value; each heap field is RETAINED and boxed (the returned object owns a
/// +1, the packed buffer keeps its own). Returns NULL if `named_desc` is NULL (defensive — a 0xFE
/// array always carries one once codegen emits it).
unsafe fn materialize_named_payload(payload: *const u8, named_desc: *const u8) -> *mut crate::object::LinObject {
    use crate::tagged::{
        TaggedVal, TAG_INT32, TAG_INT64, TAG_UINT64, TAG_FLOAT64, TAG_BOOL,
    };
    if named_desc.is_null() {
        return std::ptr::null_mut();
    }
    let field_count = u32::from_le_bytes([*named_desc, *named_desc.add(1), *named_desc.add(2), *named_desc.add(3)]) as usize;
    let obj = crate::object::lin_object_alloc(field_count as u32);
    let mut cur = 4usize;
    for _ in 0..field_count {
        let (offset, nkind, nested, name, next) = read_named_field(named_desc, cur);
        cur = next;
        let slot = payload.add(offset as usize - SEALED_HEADER);
        let key = crate::string::lin_string_from_bytes(name.as_ptr(), name.len() as u32);
        // Build the field's TaggedVal. For SCALAR kinds we read the raw value and stack-build a
        // TaggedVal (lin_object_set_fresh copies it by value, no inner RC). For HEAP kinds we box a
        // fresh +1-retained view, then free our temporary box AFTER set_fresh (which took its own +1).
        match nkind {
            NKIND_INT32 => {
                let v = *(slot as *const i32);
                let tv = TaggedVal { tag: TAG_INT32, _pad: [0; 7], payload: v as i64 as u64 };
                crate::object::lin_object_set_fresh(obj, key, &tv);
            }
            NKIND_INT64 => {
                let v = *(slot as *const i64);
                let tv = TaggedVal { tag: TAG_INT64, _pad: [0; 7], payload: v as u64 };
                crate::object::lin_object_set_fresh(obj, key, &tv);
            }
            NKIND_UINT64 => {
                let v = *(slot as *const u64);
                let tv = TaggedVal { tag: TAG_UINT64, _pad: [0; 7], payload: v };
                crate::object::lin_object_set_fresh(obj, key, &tv);
            }
            NKIND_FLOAT64 => {
                let v = *(slot as *const f64);
                let tv = TaggedVal { tag: TAG_FLOAT64, _pad: [0; 7], payload: v.to_bits() };
                crate::object::lin_object_set_fresh(obj, key, &tv);
            }
            NKIND_BOOL => {
                let v = *(slot as *const u8);
                let tv = TaggedVal { tag: TAG_BOOL, _pad: [0; 7], payload: (v != 0) as u64 };
                crate::object::lin_object_set_fresh(obj, key, &tv);
            }
            NKIND_STRING | NKIND_ARRAY | NKIND_SEALED | NKIND_MAP => {
                // Heap field: the slot holds an 8-byte owned pointer.
                let p = *(slot as *const *mut u8);
                if p.is_null() {
                    use crate::tagged::TAG_NULL;
                    let tv = TaggedVal { tag: TAG_NULL, _pad: [0; 7], payload: 0 };
                    crate::object::lin_object_set_fresh(obj, key, &tv);
                } else {
                    let boxed = box_named_heap_field(p, nkind, nested);
                    // set_fresh retains the inner payload (+1 for the object). `boxed` is our fresh
                    // construction +1; release it to drop construction back to the object's owned +1
                    // AND free the box shell.
                    crate::object::lin_object_set_fresh(obj, key, boxed as *const TaggedVal);
                    crate::tagged::lin_tagged_release(boxed);
                }
            }
            _ => {}
        }
        crate::string::lin_string_release(key);
    }
    obj
}

/// Materialize a STANDALONE sealed struct `ptr` (header + payload) into a fresh boxed LinObject via
/// its NAMED descriptor — used for a nested `NKIND_SEALED` field. The struct's payload begins at
/// `SEALED_HEADER`; the named descriptor stores struct-relative offsets, so we pass the struct base
/// adjusted: `materialize_named_payload` expects a header-LESS payload and rebases by
/// `-SEALED_HEADER`, so we hand it `ptr + SEALED_HEADER`.
unsafe fn materialize_sealed_struct(ptr: *const u8, named_desc: *const u8) -> *mut crate::object::LinObject {
    materialize_named_payload(ptr.add(SEALED_HEADER), named_desc)
}

/// Public wrapper around `materialize_sealed_struct` for use by the pointer-backed array path in
/// `lin_array_get_tagged`. Takes the struct base pointer (WITH 16-byte header) + named descriptor;
/// returns a fresh +1-owned `LinObject` for the caller to box and return. Non-null assumption: the
/// caller checks for null before calling.
pub unsafe fn materialize_sealed_struct_pub(ptr: *mut u8, named_desc: *const u8) -> *mut crate::object::LinObject {
    materialize_sealed_struct(ptr as *const u8, named_desc)
}

/// Materialize element `idx`'s packed payload of a 0xFE sealed-record array into a FRESH +1-owned
/// keyed `LinObject`, wrapped in a fresh `TaggedVal*` tagged `TAG_OBJECT`. The caller OWNS the
/// returned box and must `lin_tagged_release` it (matching `lin_array_get_tagged`'s contract). Heap
/// fields are RETAINED into the materialized object (the packed buffer keeps its own reference). The
/// boxed reader path of ADR-063 mechanism (i). `payload` is `data + idx*stride`; `named_desc` is the
/// array's `elem_named_desc`.
pub unsafe fn materialize_sealed_elem_boxed(payload: *const u8, named_desc: *const u8) -> *mut crate::tagged::TaggedVal {
    use crate::tagged::{TaggedVal, TAG_OBJECT, alloc_tagged};
    let obj = materialize_named_payload(payload, named_desc);
    alloc_tagged(TAG_OBJECT, obj as u64) as *mut TaggedVal
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
/// `elem_tag == 0xFE` at the sink (`lin_array_get_tagged` → `materialize_sealed_elem_boxed`); this
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
pub unsafe fn pack_named_payload_from_object(
    slot: *mut u8,
    obj: *const crate::object::LinObject,
    named_desc: *const u8,
) {
    use crate::tagged::{
        TAG_NULL, TAG_INT32, TAG_INT64, TAG_UINT64, TAG_FLOAT32, TAG_FLOAT64, TAG_BOOL,
        TAG_STR, TAG_ARRAY, TAG_MAP,
    };
    if named_desc.is_null() {
        crate::fault::runtime_fault(
            "Runtime error: internal — sealed-record array without a named descriptor cannot accept a boxed element",
        );
    }
    let field_count = u32::from_le_bytes([*named_desc, *named_desc.add(1), *named_desc.add(2), *named_desc.add(3)]) as usize;
    let mut cur = 4usize;
    for _ in 0..field_count {
        let (offset, nkind, _nested, name, next) = read_named_field(named_desc, cur);
        cur = next;
        let dst = slot.add(offset as usize - SEALED_HEADER);
        let key = crate::string::lin_string_from_bytes(name.as_ptr(), name.len() as u32);
        // Borrowed interior pointer (null if the key is absent).
        let tv = crate::object::lin_object_get(obj, key);
        crate::string::lin_string_release(key);
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
            NKIND_INT64 => {
                let v: i64 = match tag {
                    TAG_INT32 => payload as i32 as i64,
                    TAG_INT64 | TAG_UINT64 => payload as i64,
                    TAG_FLOAT64 => f64::from_bits(payload) as i64,
                    _ => 0,
                };
                *(dst as *mut i64) = v;
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
                crate::fault::runtime_fault(
                    "Runtime error: internal — packing a nested sealed-record field from a boxed element is not supported (widen pack_named_payload_from_object with the packing gate)",
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
    let mut cur = 4usize;
    for _ in 0..field_count {
        let (offset, nkind, _nested, _name, next) = read_named_field(named_desc, cur);
        cur = next;
        let field_size: usize = match nkind {
            NKIND_INT32 => 4,
            NKIND_INT64 | NKIND_UINT64 | NKIND_FLOAT64 => 8,
            NKIND_BOOL => 1,
            // heap fields and anything else: pointer = 8 bytes
            _ => 8,
        };
        let end = offset as usize + field_size;
        if end > max_end { max_end = end; }
    }
    // Pad to 8-byte alignment.
    (max_end + 7) & !7
}

/// Allocate a fresh sealed struct from a `LinObject` using the named descriptor, for the
/// dynamic push path on a 0xFD pointer-backed array. Returns a +1-owned struct pointer.
/// Caller is responsible for releasing it (or transferring its ownership to the array).
pub unsafe fn alloc_sealed_struct_from_object(
    obj: *const crate::object::LinObject,
    named_desc: *const u8,
) -> *mut u8 {
    let size = struct_size_from_named_desc(named_desc);
    // Alloc with NULL heap-only desc (scalar-only for Stage 1). rc=1.
    let sptr = lin_sealed_alloc(size, std::ptr::null());
    // Pack the object's fields into the struct payload.
    pack_named_payload_from_object(sptr.add(SEALED_HEADER), obj, named_desc);
    sptr
}

#[cfg(test)]
mod named_desc_tests {
    //! ADR-063 Stage 3b mechanism (i): exercise `lin_array_get_tagged`'s 0xFE materialize-on-read
    //! branch. The gate is scalar-only, so no corpus `.lin` program drives a 0xFE array through the
    //! DYNAMIC boxed reader yet — these tests hand-build a 0xFE `LinArray` + a NAMED descriptor (the
    //! exact byte layout `Codegen::sealed_named_descriptor` emits) and assert get_tagged returns a
    //! correct keyed object that is RC-balanced (run under ASan to judge UAF/leak).

    use super::*;
    use crate::tagged::{TAG_OBJECT, TAG_INT32, TAG_STR};

    /// Build a NamedDesc byte blob matching `Codegen::sealed_named_descriptor`:
    /// `[u32 count | { u32 offset, u32 nkind, u64 nested_ptr, u16 name_len, name_bytes } * count]`.
    fn build_named_desc(fields: &[(&str, u32, u32, *const u8)]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&(fields.len() as u32).to_le_bytes());
        for (name, offset, nkind, nested) in fields {
            b.extend_from_slice(&offset.to_le_bytes());
            b.extend_from_slice(&nkind.to_le_bytes());
            b.extend_from_slice(&(*nested as u64).to_le_bytes());
            b.extend_from_slice(&(name.len() as u16).to_le_bytes());
            b.extend_from_slice(name.as_bytes());
        }
        b
    }

    // SCALAR record P { x: Int32, y: Int32 }. Header 16, x@16, y@20, stride = 8.
    #[test]
    fn get_tagged_materializes_scalar_record() {
        unsafe {
            let named = build_named_desc(&[
                ("x", 16, NKIND_INT32, std::ptr::null()),
                ("y", 20, NKIND_INT32, std::ptr::null()),
            ]);
            let stride = 8u64;
            // Heap-only descriptor is NULL for a scalar-only record.
            let arr = crate::array::lin_sealed_array_alloc(4, stride, std::ptr::null(), named.as_ptr());
            // Push two elements by writing standalone structs and copying their payloads.
            for (xv, yv) in [(11i32, 22i32), (-3i32, 7i32)] {
                let st = lin_sealed_alloc(SEALED_HEADER + stride as usize, std::ptr::null());
                *((st.add(16)) as *mut i32) = xv;
                *((st.add(20)) as *mut i32) = yv;
                crate::array::lin_sealed_array_push_struct(arr, st);
                lin_sealed_release_self(st);
            }
            // Read element 1 via the DYNAMIC boxed reader (the new 0xFE branch).
            let tv = crate::array::lin_array_get_tagged(arr, 1);
            assert!(!tv.is_null());
            assert_eq!((*tv).tag, TAG_OBJECT);
            let obj = (*tv).payload as *const crate::object::LinObject;
            let kx = crate::string::lin_string_from_bytes(b"x".as_ptr(), 1);
            let ky = crate::string::lin_string_from_bytes(b"y".as_ptr(), 1);
            let fx = crate::object::lin_object_get(obj, kx);
            let fy = crate::object::lin_object_get(obj, ky);
            assert_eq!((*fx).tag, TAG_INT32);
            assert_eq!((*fx).payload as i32, -3);
            assert_eq!((*fy).tag, TAG_INT32);
            assert_eq!((*fy).payload as i32, 7);
            crate::string::lin_string_release(kx);
            crate::string::lin_string_release(ky);
            // Caller owns the box: release it (frees the materialized object — scalar, no heap fields).
            crate::tagged::lin_tagged_release(tv as *mut u8);
            // Array drop (scalar-only: just frees the buffer).
            crate::array::lin_array_release(arr);
        }
    }

    // HEAP-FIELD record R { name: String, n: Int32 }. Header 16, name@16 (8-byte ptr), n@24, stride 16.
    // Proves the materialized object takes its OWN +1 on the shared String (RC balance), and that
    // releasing both the box and the array frees the string exactly once.
    #[test]
    fn get_tagged_materializes_heap_field_record_rc_balanced() {
        unsafe {
            let named = build_named_desc(&[
                ("name", 16, NKIND_STRING, std::ptr::null()),
                ("n", 24, NKIND_INT32, std::ptr::null()),
            ]);
            // Heap-only descriptor: one heap field (name @ offset 16, KIND_STRING).
            let mut heap_desc = Vec::new();
            heap_desc.extend_from_slice(&1u32.to_le_bytes()); // count
            heap_desc.extend_from_slice(&16u32.to_le_bytes()); // offset
            heap_desc.extend_from_slice(&KIND_STRING.to_le_bytes()); // kind
            let stride = 16u64;
            let arr = crate::array::lin_sealed_array_alloc(4, stride, heap_desc.as_ptr(), named.as_ptr());
            // Construct one element: a standalone struct owning a +1 String.
            let s = crate::string::lin_string_from_bytes(b"hello".as_ptr(), 5);
            assert_eq!((*s).refcount, 1);
            let st = lin_sealed_alloc(SEALED_HEADER + stride as usize, heap_desc.as_ptr());
            *((st.add(16)) as *mut *mut u8) = s as *mut u8; // struct owns the +1
            *((st.add(24)) as *mut i32) = 42;
            // BORROWED-source push: array retains each heap field (string rc -> 2).
            crate::array::lin_sealed_array_push_struct_retaining(arr, st);
            assert_eq!((*s).refcount, 2); // struct + array
            // Drop the standalone struct (releases its +1; string rc -> 1, owned only by the array).
            lin_sealed_release_self(st);
            assert_eq!((*s).refcount, 1);

            // DYNAMIC boxed read: materialize the element. The materialized object must take its OWN
            // +1 on the string (rc -> 2) so the packed buffer's reference is independent.
            let tv = crate::array::lin_array_get_tagged(arr, 0);
            assert_eq!((*tv).tag, TAG_OBJECT);
            assert_eq!((*s).refcount, 2); // array + materialized object
            let obj = (*tv).payload as *const crate::object::LinObject;
            let kname = crate::string::lin_string_from_bytes(b"name".as_ptr(), 4);
            let kn = crate::string::lin_string_from_bytes(b"n".as_ptr(), 1);
            let fname = crate::object::lin_object_get(obj, kname);
            let fn_ = crate::object::lin_object_get(obj, kn);
            assert_eq!((*fname).tag, TAG_STR);
            let sptr = (*fname).payload as *const crate::string::LinString;
            assert_eq!((*sptr).as_str(), "hello");
            assert_eq!((*fn_).tag, TAG_INT32);
            assert_eq!((*fn_).payload as i32, 42);
            crate::string::lin_string_release(kname);
            crate::string::lin_string_release(kn);

            // Release the box -> frees the materialized object -> releases its string +1 (rc -> 1).
            crate::tagged::lin_tagged_release(tv as *mut u8);
            assert_eq!((*s).refcount, 1); // only the array now
            // Array drop -> release_sealed_array_elems walks the heap desc -> string rc -> 0, freed.
            crate::array::lin_array_release(arr);
            // (s is now freed; ASan verifies no leak/double-free.)
        }
    }

    // ----- WRITE direction: `pack_named_payload_from_object` through the dynamic/tagged sinks -----
    // (the inverse of the materialize tests above; guards the map-value-fetch/push corruption fix —
    // before it, lin_array_push blind-wrote 16-byte TaggedVal slots into the stride-sized packed
    // buffer (heap overflow at the 3rd element) and lin_push_dyn silently dropped the element.)

    /// Build a fresh keyed LinObject `{ x: Int32(xv), y: Int32(yv) }` (rc = 1).
    unsafe fn make_obj_xy(xv: i32, yv: i32) -> *mut crate::object::LinObject {
        use crate::tagged::TaggedVal;
        let obj = crate::object::lin_object_alloc(2);
        for (name, v) in [("x", xv), ("y", yv)] {
            let key = crate::string::lin_string_from_bytes(name.as_ptr(), name.len() as u32);
            let tv = TaggedVal { tag: TAG_INT32, _pad: [0; 7], payload: v as i64 as u64 };
            crate::object::lin_object_set_fresh(obj, key, &tv);
            crate::string::lin_string_release(key);
        }
        obj
    }

    // Scalar record P { x: Int32, y: Int32 } pushed through BOTH dynamic write sinks, crossing the
    // initial-capacity growth boundary (the original crash fired on push #3 of a cap-4/stride-8
    // buffer because tagged 16-byte writes filled it after 2). Reads back via the packed elem ptr
    // AND the materializing boxed reader.
    #[test]
    fn dynamic_push_packs_into_sealed_array() {
        unsafe {
            use crate::tagged::TaggedVal;
            let named = build_named_desc(&[
                ("x", 16, NKIND_INT32, std::ptr::null()),
                ("y", 20, NKIND_INT32, std::ptr::null()),
            ]);
            let arr = crate::array::lin_sealed_array_alloc(4, 8, std::ptr::null(), named.as_ptr());
            // 5 pushes via lin_push_dyn (RETAINING contract: the caller keeps its object ref).
            for i in 0..5i32 {
                let obj = make_obj_xy(i, i * 10);
                let tv = TaggedVal { tag: TAG_OBJECT, _pad: [0; 7], payload: obj as u64 };
                crate::array::lin_push_dyn(arr, &tv);
                assert_eq!((*obj).refcount, 1, "lin_push_dyn must not consume the caller's ref");
                crate::object::lin_object_release(obj);
            }
            assert_eq!((*arr).len, 5);
            // Packed bytes are real field values at the element stride (not TaggedVal slots).
            let p = crate::array::lin_sealed_array_elem_ptr(arr, 4);
            assert_eq!(*(p as *const i32), 4);
            assert_eq!(*(p.add(4) as *const i32), 40);
            // lin_array_push (MOVE contract: consumes one transferred object ref).
            let obj = make_obj_xy(7, 8);
            crate::memory::lin_rc_retain(obj as *mut u32); // the codegen-transferred +1 (rc -> 2)
            let cell: *mut crate::object::LinObject = obj;
            crate::array::lin_array_push(arr, &cell as *const _ as *const u8, TAG_OBJECT);
            assert_eq!((*obj).refcount, 1, "lin_array_push must consume exactly the transferred ref");
            crate::object::lin_object_release(obj);
            // Roundtrip element 5 through the materializing boxed reader.
            let tv = crate::array::lin_array_get_tagged(arr, 5);
            assert_eq!((*tv).tag, TAG_OBJECT);
            let mat = (*tv).payload as *const crate::object::LinObject;
            let kx = crate::string::lin_string_from_bytes(b"x".as_ptr(), 1);
            let fx = crate::object::lin_object_get(mat, kx);
            assert_eq!((*fx).payload as i32, 7);
            crate::string::lin_string_release(kx);
            crate::tagged::lin_tagged_release(tv as *mut u8);
            crate::array::lin_array_release(arr);
        }
    }

    // Heap-field record R { name: String, n: Int32 }: the pack retains the String into the slot
    // (the source object keeps its own ref), and the materialize/drop chain releases it exactly
    // once each — RC-balanced end to end (ASan judges leak/double-free).
    #[test]
    fn dynamic_push_packs_heap_field_rc_balanced() {
        unsafe {
            use crate::tagged::TaggedVal;
            let named = build_named_desc(&[
                ("name", 16, NKIND_STRING, std::ptr::null()),
                ("n", 24, NKIND_INT32, std::ptr::null()),
            ]);
            let mut heap_desc = Vec::new();
            heap_desc.extend_from_slice(&1u32.to_le_bytes());
            heap_desc.extend_from_slice(&16u32.to_le_bytes());
            heap_desc.extend_from_slice(&KIND_STRING.to_le_bytes());
            let arr = crate::array::lin_sealed_array_alloc(4, 16, heap_desc.as_ptr(), named.as_ptr());
            // Source object { name: "hello", n: 42 } — it owns its own +1 on the string.
            let s = crate::string::lin_string_from_bytes(b"hello".as_ptr(), 5);
            let obj = crate::object::lin_object_alloc(2);
            let kname = crate::string::lin_string_from_bytes(b"name".as_ptr(), 4);
            let tv_name = TaggedVal { tag: TAG_STR, _pad: [0; 7], payload: s as u64 };
            crate::object::lin_object_set_fresh(obj, kname, &tv_name); // retains: s rc -> 2
            crate::string::lin_string_release(kname);
            let kn = crate::string::lin_string_from_bytes(b"n".as_ptr(), 1);
            let tv_n = TaggedVal { tag: TAG_INT32, _pad: [0; 7], payload: 42u64 };
            crate::object::lin_object_set_fresh(obj, kn, &tv_n);
            crate::string::lin_string_release(kn);
            crate::string::lin_string_release(s); // drop our construction ref: obj is sole owner (rc 1)
            assert_eq!((*s).refcount, 1);

            let tv = TaggedVal { tag: TAG_OBJECT, _pad: [0; 7], payload: obj as u64 };
            crate::array::lin_push_dyn(arr, &tv); // pack retains the string into the slot (rc -> 2)
            assert_eq!((*s).refcount, 2, "the packed slot must take its OWN string ref");
            crate::object::lin_object_release(obj); // source object drops its ref (rc -> 1, array owns)
            assert_eq!((*s).refcount, 1);

            // Materialize-on-read takes another independent +1.
            let out = crate::array::lin_array_get_tagged(arr, 0);
            assert_eq!((*s).refcount, 2);
            let mat = (*out).payload as *const crate::object::LinObject;
            let k = crate::string::lin_string_from_bytes(b"name".as_ptr(), 4);
            let f = crate::object::lin_object_get(mat, k);
            assert_eq!((*f).tag, TAG_STR);
            assert_eq!((*((*f).payload as *const crate::string::LinString)).as_str(), "hello");
            crate::string::lin_string_release(k);
            crate::tagged::lin_tagged_release(out as *mut u8);
            assert_eq!((*s).refcount, 1);
            // Array drop walks the heap desc: string rc -> 0, freed exactly once.
            crate::array::lin_array_release(arr);
        }
    }
}
