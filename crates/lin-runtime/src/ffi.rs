//! Raw-memory + C-string FFI "island" for the prototype richer-FFI keystone.
//!
//! These functions are the unsafe primitives the `std/ffi` stdlib wrapper re-exports under clean
//! names (`cstr`, `alloc`, `free`, `peekU32`, …). They let pure Lin marshal C-string arguments and
//! read/write fixed-layout structs returned through `void*` out-params, using `Ptr` (an Int64 alias,
//! ABI-identical to a 64-bit pointer) as the handle type.
//!
//! Everything here is deliberately minimal and unsafe — it is the trusted boundary between Lin's
//! managed values and arbitrary C memory. Pointers are passed to/from Lin as `i64`.

use crate::string::LinString;

/// Allocate a NUL-terminated copy of a Lin string's bytes and return a pointer to it.
///
/// `LinString` is NOT NUL-terminated (it carries an explicit length), so C APIs that expect a
/// `const char*` need this conversion. We allocate `len + 1` bytes via libc `malloc`, copy the
/// bytes, append a `\0`, and hand the raw pointer to the caller.
///
/// OWNERSHIP: this function does NOT free the buffer — the caller owns it. The recommended,
/// leak-free idiom from Lin is `std/ffi`'s `withCstr`, which allocates, runs a callback with the
/// pointer, and frees the buffer afterwards (covering the common "C copies the string during the
/// call" case without the programmer having to remember a paired free). The bare `cstr`/`free`
/// pair remains available for C APIs that RETAIN the pointer and require explicit lifetime
/// management. Calling `cstr` without a matching `free` (and without `withCstr`) leaks — building
/// many cstrings in a hot loop that way would leak unboundedly. (`withCstr` itself can still leak
/// only if the callback faults, since Lin has no try/finally; accepted for this prototype.)
#[no_mangle]
pub unsafe extern "C" fn lin_ffi_cstr(s: *const LinString) -> *mut u8 {
    let len = (*s).len as usize;
    let src = (*s).data.as_ptr();
    // malloc(len + 1) for the trailing NUL.
    let buf = libc_malloc(len + 1);
    if buf.is_null() {
        return std::ptr::null_mut();
    }
    std::ptr::copy_nonoverlapping(src, buf, len);
    *buf.add(len) = 0;
    buf
}

/// Allocate `n` bytes of raw, uninitialized scratch memory (libc `malloc`) and return the pointer.
/// Used for out-param struct buffers (e.g. an event struct a C poll function writes into).
/// The buffer is the caller's to manage; release it with `lin_ffi_free`.
#[no_mangle]
pub unsafe extern "C" fn lin_ffi_alloc(n: i64) -> *mut u8 {
    if n <= 0 {
        return std::ptr::null_mut();
    }
    libc_malloc(n as usize)
}

/// Free a buffer previously returned by `lin_ffi_alloc` (or `lin_ffi_cstr`). No-op on null.
#[no_mangle]
pub unsafe extern "C" fn lin_ffi_free(p: *mut u8) {
    if !p.is_null() {
        libc_free(p);
    }
}

// ── peek: read a value at byte offset `off` from pointer `p` ──────────────────────────────────
// Pointers arrive as i64 (the `Ptr` alias). We cast to the appropriate raw pointer, add the byte
// offset, and read unaligned (struct fields may not be naturally aligned relative to a u8 base).
// The caller is responsible for the read being in-bounds — these are raw memory primitives.

#[no_mangle]
pub unsafe extern "C" fn lin_ffi_peek_u8(p: i64, off: i64) -> u8 {
    let addr = (p as usize).wrapping_add(off as usize) as *const u8;
    std::ptr::read_unaligned(addr)
}

#[no_mangle]
pub unsafe extern "C" fn lin_ffi_peek_u16(p: i64, off: i64) -> u16 {
    let addr = (p as usize).wrapping_add(off as usize) as *const u16;
    std::ptr::read_unaligned(addr)
}

#[no_mangle]
pub unsafe extern "C" fn lin_ffi_peek_u32(p: i64, off: i64) -> u32 {
    let addr = (p as usize).wrapping_add(off as usize) as *const u32;
    std::ptr::read_unaligned(addr)
}

#[no_mangle]
pub unsafe extern "C" fn lin_ffi_peek_u64(p: i64, off: i64) -> u64 {
    let addr = (p as usize).wrapping_add(off as usize) as *const u64;
    std::ptr::read_unaligned(addr)
}

#[no_mangle]
pub unsafe extern "C" fn lin_ffi_peek_i32(p: i64, off: i64) -> i32 {
    let addr = (p as usize).wrapping_add(off as usize) as *const i32;
    std::ptr::read_unaligned(addr)
}

#[no_mangle]
pub unsafe extern "C" fn lin_ffi_peek_i64(p: i64, off: i64) -> i64 {
    let addr = (p as usize).wrapping_add(off as usize) as *const i64;
    std::ptr::read_unaligned(addr)
}

#[no_mangle]
pub unsafe extern "C" fn lin_ffi_peek_f32(p: i64, off: i64) -> f32 {
    let addr = (p as usize).wrapping_add(off as usize) as *const f32;
    std::ptr::read_unaligned(addr)
}

#[no_mangle]
pub unsafe extern "C" fn lin_ffi_peek_f64(p: i64, off: i64) -> f64 {
    let addr = (p as usize).wrapping_add(off as usize) as *const f64;
    std::ptr::read_unaligned(addr)
}

/// Read a pointer-width value (a nested `void*` field) and return it as `i64` (the `Ptr` alias).
#[no_mangle]
pub unsafe extern "C" fn lin_ffi_peek_ptr(p: i64, off: i64) -> i64 {
    let addr = (p as usize).wrapping_add(off as usize) as *const usize;
    std::ptr::read_unaligned(addr) as i64
}

// ── poke: write a value at byte offset `off` to pointer `p` ───────────────────────────────────
// For populating out-param / scratch buffers before handing them to a C function.

#[no_mangle]
pub unsafe extern "C" fn lin_ffi_poke_u8(p: i64, off: i64, v: u8) {
    let addr = (p as usize).wrapping_add(off as usize) as *mut u8;
    std::ptr::write_unaligned(addr, v);
}

#[no_mangle]
pub unsafe extern "C" fn lin_ffi_poke_u16(p: i64, off: i64, v: u16) {
    let addr = (p as usize).wrapping_add(off as usize) as *mut u16;
    std::ptr::write_unaligned(addr, v);
}

#[no_mangle]
pub unsafe extern "C" fn lin_ffi_poke_u32(p: i64, off: i64, v: u32) {
    let addr = (p as usize).wrapping_add(off as usize) as *mut u32;
    std::ptr::write_unaligned(addr, v);
}

#[no_mangle]
pub unsafe extern "C" fn lin_ffi_poke_u64(p: i64, off: i64, v: u64) {
    let addr = (p as usize).wrapping_add(off as usize) as *mut u64;
    std::ptr::write_unaligned(addr, v);
}

#[no_mangle]
pub unsafe extern "C" fn lin_ffi_poke_i32(p: i64, off: i64, v: i32) {
    let addr = (p as usize).wrapping_add(off as usize) as *mut i32;
    std::ptr::write_unaligned(addr, v);
}

#[no_mangle]
pub unsafe extern "C" fn lin_ffi_poke_i64(p: i64, off: i64, v: i64) {
    let addr = (p as usize).wrapping_add(off as usize) as *mut i64;
    std::ptr::write_unaligned(addr, v);
}

#[no_mangle]
pub unsafe extern "C" fn lin_ffi_poke_f32(p: i64, off: i64, v: f32) {
    let addr = (p as usize).wrapping_add(off as usize) as *mut f32;
    std::ptr::write_unaligned(addr, v);
}

#[no_mangle]
pub unsafe extern "C" fn lin_ffi_poke_f64(p: i64, off: i64, v: f64) {
    let addr = (p as usize).wrapping_add(off as usize) as *mut f64;
    std::ptr::write_unaligned(addr, v);
}

/// Write a pointer-width value (a nested `void*` field), received from Lin as `i64` (the `Ptr`
/// alias), at byte offset `off`.
#[no_mangle]
pub unsafe extern "C" fn lin_ffi_poke_ptr(p: i64, off: i64, v: i64) {
    let addr = (p as usize).wrapping_add(off as usize) as *mut usize;
    std::ptr::write_unaligned(addr, v as usize);
}

// ── libc malloc/free shims ────────────────────────────────────────────────────────────────────
// We use libc malloc/free (not Rust's allocator) so the buffers can be freely handed to C code and
// freed symmetrically, and so a leaked cstr is owned by the same allocator a C library would use.
extern "C" {
    #[link_name = "malloc"]
    fn libc_malloc(size: usize) -> *mut u8;
    #[link_name = "free"]
    fn libc_free(ptr: *mut u8);
}
