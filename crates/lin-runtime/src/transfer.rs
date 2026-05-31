//! Transfer-by-deep-copy for crossing a thread boundary (ADR-042, Option C).
//!
//! When a value or a thunk's captured environment crosses into another OS thread, Lin
//! copies it so each thread owns a private, disjoint object graph — refcounts stay
//! non-atomic because nothing is shared. The set of values that can cross is exactly the
//! *transferable* types (the checker forbids `Function`/`Iterator`/cyclic graphs at a
//! boundary), so a deep copy is total and bounded.
//!
//! Two entry points:
//!   * `lin_transfer_clone(TaggedVal*)` — deep-copies a transferable value graph (scalars,
//!     strings, arrays, objects, recursively). Used for the closure's captured `val`s and
//!     (defensively) for results. Immortal/interned strings are shared, not copied (never
//!     mutated or freed). `Shared`/`Frozen` boxes (Phases 6-7) will be shared by
//!     atomic-refcount bump, not copied through — handled when those types land.
//!   * `transfer_clone_env(env_ptr, desc)` — deep-copies a closure's env allocation using the
//!     codegen-emitted capture descriptor (passed in from the closure's offset-40 slot, ADR-055)
//!     recording each slot's kind.

use crate::tagged::{TaggedVal, TAG_STR, TAG_ARRAY, TAG_OBJECT};
use crate::string::{LinString, lin_string_alloc, IMMORTAL_RC};
use crate::array::{LinArray, lin_array_alloc};
use crate::object::LinObject;

/// Deep-copy a `LinString`. Immortal (interned literal) strings are shared as-is — they are
/// never mutated or freed, so concurrent reads of their bytes/refcount are race-free.
unsafe fn clone_string(s: *const LinString) -> *mut LinString {
    if s.is_null() {
        return std::ptr::null_mut();
    }
    if (*s).refcount >= IMMORTAL_RC {
        return s as *mut LinString;
    }
    let len = (*s).len;
    let fresh = lin_string_alloc(len);
    if len > 0 {
        std::ptr::copy_nonoverlapping((*s).data.as_ptr(), (*fresh).data.as_mut_ptr(), len as usize);
    }
    fresh
}

/// Deep-copy a `LinArray`, flat or tagged. Flat scalar arrays copy their raw buffer; tagged
/// arrays recursively transfer each element.
pub(crate) unsafe fn clone_array(src: *const LinArray) -> *mut LinArray {
    if src.is_null() {
        return std::ptr::null_mut();
    }
    // Frozen (immortal) arrays are immutable and shared read-only across threads — share by
    // reference (zero-copy), never deep-copy through (Frozen<T>, ADR-045). Safe because their
    // contents and refcount are never written.
    if (*src).refcount >= IMMORTAL_RC {
        return src as *mut LinArray;
    }
    let len = (*src).len;
    let elem_tag = (*src).elem_tag;
    if elem_tag != 0xFF {
        // Flat scalar array: copy the raw element buffer verbatim (no pointers inside).
        return crate::array::lin_array_clone_flat(src);
    }
    // Tagged array: allocate and transfer each element.
    let dst = lin_array_alloc(len.max(4));
    for i in 0..len as usize {
        let se = (*src).data.add(i);
        let de = (*dst).data.add(i);
        (*de).tag = (*se).tag;
        (*de).payload = transfer_payload((*se).tag, (*se).payload);
    }
    (*dst).len = len;
    dst
}

/// Deep-copy a `LinObject` (recursively transfers each value; keys are cloned strings).
unsafe fn clone_object(src: *const LinObject) -> *mut LinObject {
    if src.is_null() {
        return std::ptr::null_mut();
    }
    // Frozen objects: share by reference, zero-copy (see clone_array).
    if (*src).refcount >= IMMORTAL_RC {
        return src as *mut LinObject;
    }
    let len = (*src).len;
    let dst = crate::object::lin_object_alloc(len.max(4));
    for i in 0..len as usize {
        let se = (*src).entries.add(i);
        let key = clone_string((*se).key);
        let mut v: TaggedVal = TaggedVal { tag: (*se).value.tag, _pad: [0; 7], payload: 0 };
        v.payload = transfer_payload((*se).value.tag, (*se).value.payload);
        crate::object::object_push_owned(dst, key, v);
    }
    dst
}

/// Transfer one tagged payload (the 8-byte field) by kind: scalars copy verbatim; heap
/// pointers are deep-copied.
unsafe fn transfer_payload(tag: u8, payload: u64) -> u64 {
    use crate::tagged::TAG_SHARED;
    match tag {
        TAG_STR => clone_string(payload as *const LinString) as u64,
        TAG_ARRAY => clone_array(payload as *const LinArray) as u64,
        TAG_OBJECT => clone_object(payload as *const LinObject) as u64,
        TAG_SHARED => {
            // Nesting/boundary rule (ADR-043 §2.3.1): a Shared box embedded in a transferred
            // value is NOT deep-copied through — bump its atomic refcount and SHARE the box.
            crate::shared::lin_shared_retain_box(payload as *const u8);
            payload
        }
        // Scalars: copy verbatim. (TAG_FUNCTION is not transferable data — the checker
        // prevents it appearing here; pass through as a last resort.)
        _ => payload,
    }
}

/// Deep-copy a transferable value graph rooted at a boxed `TaggedVal*`. Returns a fresh,
/// independently-owned box (or null for the null value). The caller owns the result.
#[no_mangle]
pub unsafe extern "C" fn lin_transfer_clone(p: *const u8) -> *mut u8 {
    if p.is_null() {
        return std::ptr::null_mut();
    }
    let src = &*(p as *const TaggedVal);
    let payload = transfer_payload(src.tag, src.payload);
    crate::tagged::alloc_tagged(src.tag, payload)
}

// -------------------------------------------------------------------------
// Closure environment transfer
// -------------------------------------------------------------------------

// Capture descriptor kind codes (one byte per captured env slot, env slot `i` at byte offset
// `8 + i*8`). These mirror `lin_ir::ir::CaptureRelease::code()` — the SAME descriptor drives
// both closure-release and this thread-transfer path. The descriptor pointer lives in the
// CLOSURE at X; the async caller passes it in explicitly.
pub const CAP_NONE: u8 = 0; // scalar (copy verbatim) or a borrowed var-cell pointer
pub const CAP_STR: u8 = 1; // *mut LinString
pub const CAP_ARRAY: u8 = 2; // *mut LinArray
pub const CAP_OBJECT: u8 = 3; // *mut LinObject
pub const CAP_CLOSURE: u8 = 4; // *mut LinClosure — NOT deep-copyable across a thread boundary
pub const CAP_TAGGED: u8 = 5; // *mut TaggedVal (boxed Json/union) — deep-copy via lin_transfer_clone

/// Deep-copy a closure env allocation given its capture descriptor `desc` (a static read-only
/// `{u32 count, u8 kinds[]}` global from the closure's offset-40 slot). `env_ptr` layout:
/// `{ u64 size @0, cap0 @8, cap1 @16, ... }`. Returns a fresh env whose heap captures are
/// private copies, or null if `env_ptr`/`desc` is null. The new env's offset-0 word is its
/// size (the descriptor is NOT stored in the env — it stays on the closure).
pub unsafe fn transfer_clone_env(env_ptr: *const u8, desc: *const u8) -> *mut u8 {
    if env_ptr.is_null() || desc.is_null() {
        return std::ptr::null_mut();
    }
    let count = *(desc as *const u32) as usize;
    let kinds = desc.add(std::mem::size_of::<u32>());
    let env_size = 8 + count * 8;
    let new_env = crate::memory::lin_alloc(env_size);
    *(new_env as *mut u64) = env_size as u64; // size header at offset 0
    for i in 0..count {
        let off = 8 + i * 8;
        let src_word = *(env_ptr.add(off) as *const u64);
        let new_word = match *kinds.add(i) {
            CAP_NONE | CAP_CLOSURE => src_word,
            CAP_STR => clone_string(src_word as *const LinString) as u64,
            CAP_ARRAY => clone_array(src_word as *const LinArray) as u64,
            CAP_OBJECT => clone_object(src_word as *const LinObject) as u64,
            CAP_TAGGED => lin_transfer_clone(src_word as *const u8) as u64,
            _ => src_word,
        };
        *(new_env.add(off) as *mut u64) = new_word;
    }
    new_env
}

/// Release a deep-copied env produced by `transfer_clone_env`: drop the owned reference to each
/// heap capture (the copies were created with refcount 1, owned by no Lin binding — the worker
/// holds the sole reference), then free the env allocation. `desc` is the capture descriptor;
/// `env_size` is `8 + count*8`.
pub unsafe fn release_env_copy(env_ptr: *mut u8, desc: *const u8, env_size: u64) {
    if env_ptr.is_null() {
        return;
    }
    if !desc.is_null() {
        let count = *(desc as *const u32) as usize;
        let kinds = desc.add(std::mem::size_of::<u32>());
        for i in 0..count {
            let off = 8 + i * 8;
            let word = *(env_ptr.add(off) as *const u64);
            match *kinds.add(i) {
                CAP_STR => crate::string::lin_string_release(word as *mut LinString),
                CAP_ARRAY => crate::array::lin_array_release(word as *mut LinArray),
                CAP_OBJECT => crate::object::lin_object_release(word as *mut LinObject),
                CAP_CLOSURE => crate::memory::lin_closure_release(word as *mut u8),
                CAP_TAGGED => crate::tagged::lin_tagged_release(word as *mut u8),
                _ => {} // CAP_NONE: no owned heap payload to release
            }
        }
    }
    let layout = std::alloc::Layout::from_size_align_unchecked(env_size as usize, 8);
    std::alloc::dealloc(env_ptr, layout);
}

/// True if a closure with env `env_ptr` and capture descriptor `desc` can be safely deep-copied
/// for transfer: a null env (no captures) is trivially transferable; otherwise `desc` must be
/// present and contain no `CAP_CLOSURE` slot (a captured closure can't be deep-copied across a
/// thread boundary). When false, the spawn path must run the thunk inline.
pub unsafe fn env_is_transferable(env_ptr: *const u8, desc: *const u8) -> bool {
    if env_ptr.is_null() {
        return true;
    }
    if desc.is_null() {
        return false;
    }
    let count = *(desc as *const u32) as usize;
    let kinds = desc.add(std::mem::size_of::<u32>());
    for i in 0..count {
        if *kinds.add(i) == CAP_CLOSURE {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tagged::{alloc_tagged, TAG_INT32};

    #[test]
    fn transfer_scalar_box_is_independent() {
        unsafe {
            let a = alloc_tagged(TAG_INT32, 5);
            let b = lin_transfer_clone(a);
            assert!(!b.is_null());
            assert_ne!(a, b);
            assert_eq!((*(b as *const TaggedVal)).payload, 5);
        }
    }

    #[test]
    fn transfer_null_is_null() {
        unsafe {
            assert!(lin_transfer_clone(std::ptr::null()).is_null());
        }
    }
}
