//! Sealed scalar-record runtime support (sealed-records design, Stage 1).
//!
//! A *sealed scalar record* is a named record type (`type T = { ... }`) whose fields are ALL
//! unboxed scalars (Int8/16/32/64, UInt8/16/32/64, Float32/64, Bool). Such a value is laid out
//! by codegen as a packed heap struct:
//!
//! ```text
//! [ u32 refcount | u32 size | scalar field 0 | scalar field 1 | ... ]
//! ```
//!
//! The 8-byte header keeps i64/f64 fields naturally aligned. The refcount lives at offset 0, so
//! the existing `lin_rc_retain` (which bumps the u32 at offset 0) works UNCHANGED on a sealed
//! struct. The total byte `size` is stored at offset 4 so the struct can be freed WITHOUT the
//! caller passing the size (see `lin_sealed_release_self`) — needed by the closure-capture-release
//! walk, whose descriptor carries only a one-byte kind per capture. Fields are stored at
//! codegen-computed byte offsets in the TYPE DECLARATION's field order. Because every field is a
//! scalar (never refcounted), there is NO per-field release: the struct's own refcount governs
//! its entire lifetime, and dropping it is a single deallocation.
//!
//! A sealed struct must NEVER be passed to `lin_object_*` (which read a `LinObject` header/entries)
//! nor boxed as `TAG_OBJECT`. Codegen routes every operation that would observe it as a generic
//! Json object through a boundary MATERIALIZATION into a real boxed `LinObject` first (see
//! `compile_ir_coerce` Object->Json). So the only runtime support a sealed struct needs is alloc
//! and release; all richer ops (toString/keys/eq-vs-Json/print/dynamic-index) operate on the
//! materialized `LinObject` and require no new runtime code.

use std::alloc::{alloc, dealloc, Layout};

/// Header size in bytes: `u32 refcount` + `u32 size`. Field payload begins at offset 8.
pub const SEALED_HEADER: usize = 8;

/// Allocate a sealed scalar-record struct of `size` total bytes (header + packed fields),
/// zero-initialised, with refcount 1 and the byte `size` recorded at offset 4. `size` is computed
/// by codegen as `SEALED_HEADER + payload`. Aborts on allocation failure. Always 8-aligned.
#[no_mangle]
pub extern "C" fn lin_sealed_alloc(size: usize) -> *mut u8 {
    let size = size.max(SEALED_HEADER);
    unsafe {
        let layout = Layout::from_size_align_unchecked(size, 8);
        let ptr = alloc(layout);
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        // Zero the whole block so any unwritten padding/field bytes are deterministic.
        std::ptr::write_bytes(ptr, 0, size);
        let words = ptr as *mut u32;
        *words = 1; // refcount
        *words.add(1) = size as u32; // size @ offset 4
        ptr
    }
}

/// Release a sealed scalar-record struct: decrement its refcount and, on reaching zero, free the
/// allocation. No per-field release is needed — every field is an unboxed scalar. `size` is the
/// same total byte size passed to `lin_sealed_alloc` (codegen knows it statically per type). Null-
/// and (degenerate) zero-refcount-safe.
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
        let size = size.max(SEALED_HEADER);
        let layout = Layout::from_size_align_unchecked(size, 8);
        dealloc(ptr, layout);
    }
}

/// Release a sealed scalar-record struct, reading its byte `size` from the header (offset 4)
/// instead of taking it as an argument. Used where the caller does NOT have the size available —
/// specifically the closure-capture-release walk (`release_captures`), which has only a one-byte
/// kind per capture. Equivalent to `lin_sealed_release(ptr, *(u32*)(ptr+4))`. Null/zero-rc-safe.
#[no_mangle]
pub unsafe extern "C" fn lin_sealed_release_self(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    let size = *((ptr as *const u32).add(1)) as usize;
    lin_sealed_release(ptr, size);
}
