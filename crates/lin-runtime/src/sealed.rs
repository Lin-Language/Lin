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
    *rc -= 1;
    if *rc == 0 {
        release_heap_fields(ptr);
        let size = size.max(SEALED_HEADER);
        let layout = Layout::from_size_align_unchecked(size, 8);
        dealloc(ptr, layout);
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
