//! `Shared<T>` — opt-in shared *mutable* state across threads (ADR-028 §2.3.1).
//!
//! Copy-by-default (Option C) handles most cases, but a large/mutable structure that many
//! threads must read and update is better shared than copied into every thread. `Shared<T>` is
//! that escape hatch: an **atomic-refcounted** box wrapping an `RwLock` over the inner value.
//!
//! Three accessors, mapping onto the reader-writer lock:
//!   * `get(s)`      — read lock; deep-copies a snapshot OUT (concurrent with other `get`s).
//!   * `set(s, v)`   — write lock; deep-copies `v` IN, replacing the inner value.
//!   * `withLock(s, f)` — write lock held across `f`, which mutates the inner value in place;
//!                        `f`'s result is deep-copied OUT.
//!
//! Safety model (ADR-028): every value entering is copied in, every value leaving is copied
//! out, so no live reference into the inner graph escapes the lock. The box's own refcount is
//! ATOMIC (it is the thing shared across threads); the inner object graph keeps ordinary
//! non-atomic RC because it is only ever reachable while a lock is held (all access serialized;
//! concurrent `get`s only read, copying out).
//!
//! The inner value is stored as a boxed `TaggedVal*` (the universal value representation).

use crate::tagged::{TaggedVal, TAG_SHARED};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::RwLock;

/// The heap box behind a `Shared<T>` value. Shared across threads by atomic-refcount bump.
pub struct SharedBox {
    /// Atomic refcount — the box is the cross-thread-shared object.
    rc: AtomicU32,
    /// The inner value, guarded by a reader-writer lock. Stored as a boxed `TaggedVal*`
    /// (address as usize so the struct is Send/Sync; access is serialized by the lock).
    inner: RwLock<usize>,
}

// SAFETY: the inner TaggedVal* is only ever touched while the RwLock is held, and every value
// crossing the boundary is deep-copied — so no unsynchronized access to the inner graph occurs.
unsafe impl Send for SharedBox {}
unsafe impl Sync for SharedBox {}

/// Box a `SharedBox` into a `TaggedVal*(TAG_SHARED)` so it flows through the value
/// representation like any other tagged value (the payload is the `*const SharedBox`).
unsafe fn box_shared(b: *const SharedBox) -> *mut u8 {
    crate::tagged::alloc_tagged(TAG_SHARED, b as u64)
}

/// Extract the `*const SharedBox` from a boxed `Shared` value (TAG_SHARED). Null/wrong-tag → null.
unsafe fn unwrap_shared(p: *const u8) -> *const SharedBox {
    if p.is_null() {
        return std::ptr::null();
    }
    let tv = &*(p as *const TaggedVal);
    if tv.tag == TAG_SHARED {
        tv.payload as *const SharedBox
    } else {
        std::ptr::null()
    }
}

/// `shared(v)` — create a `Shared<T>` boxing a private deep copy of `v` (transferable). Returns
/// a boxed `TaggedVal*(TAG_SHARED)`. The box starts with atomic refcount 1.
#[no_mangle]
pub unsafe extern "C" fn lin_shared_new(v: *const u8) -> *mut u8 {
    let copy = crate::transfer::lin_transfer_clone(v);
    let b = Box::into_raw(Box::new(SharedBox {
        rc: AtomicU32::new(1),
        inner: RwLock::new(copy as usize),
    }));
    box_shared(b)
}

/// `get(s)` — take the read lock and deep-copy a snapshot of the inner value OUT. Concurrent
/// with other `get`s. Returns a fresh boxed `TaggedVal*` owned by the caller.
#[no_mangle]
pub unsafe extern "C" fn lin_shared_get(s: *const u8) -> *mut u8 {
    let b = unwrap_shared(s);
    if b.is_null() {
        return std::ptr::null_mut();
    }
    let guard = (*b).inner.read().unwrap();
    let inner = *guard as *const u8;
    crate::transfer::lin_transfer_clone(inner)
}

/// `set(s, v)` — take the write lock and replace the inner value with a deep copy of `v`.
/// The old inner value is released. Returns the Json null value.
#[no_mangle]
pub unsafe extern "C" fn lin_shared_set(s: *const u8, v: *const u8) -> *mut u8 {
    let b = unwrap_shared(s);
    if b.is_null() {
        return std::ptr::null_mut();
    }
    let copy = crate::transfer::lin_transfer_clone(v);
    let mut guard = (*b).inner.write().unwrap();
    let old = *guard as *mut u8;
    *guard = copy as usize;
    drop(guard);
    crate::tagged::lin_tagged_release(old);
    std::ptr::null_mut()
}

/// `withLock(s, f)` — take the write lock for the whole of `f`, call `f(inner)` passing the
/// inner value **mutable, in place**, then deep-copy `f`'s result OUT to the caller (spec
/// §2.3.1 table). `f` is a closure pointer (boxed ABI: `(env, arg) -> ret`); we pass the inner
/// `TaggedVal*` as its argument, so an in-place mutation (`a => push(a, 7)`) hits the box's
/// value directly, while a read (`a => length(a)`) returns a derived value without changing it.
/// The inner value is NOT replaced by `f`'s return — the return is only copied out.
///
/// Caveat: scalars cannot be mutated in place, so `withLock(counter, n => n + 1)` does NOT
/// persist (it returns `n+1` but leaves the box unchanged). For a scalar accumulator use a
/// single-element array (`withLock(c, a => set/push ...)`) or `get`/`set`. Documented, not
/// silently surprising.
#[no_mangle]
pub unsafe extern "C" fn lin_shared_with_lock(s: *const u8, f: *mut u8) -> *mut u8 {
    let b = unwrap_shared(s);
    if b.is_null() || f.is_null() {
        return std::ptr::null_mut();
    }
    let fn_ptr = *(f.add(8) as *const *mut u8);
    let env_ptr = *(f.add(16) as *const *mut u8);
    let guard = (*b).inner.write().unwrap();
    let inner = *guard as *mut u8;
    // Call f(env, inner) under the write lock; f may mutate `inner` in place.
    let result = {
        let call: unsafe extern "C-unwind" fn(*mut u8, *mut u8) -> *mut u8 = std::mem::transmute(fn_ptr);
        call(env_ptr, inner)
    };
    drop(guard);
    // Deep-copy f's result OUT (independent of the box). If f returned the inner value itself
    // (an alias), the copy makes mutating the returned value harmless to the box.
    crate::transfer::lin_transfer_clone(result as *const u8)
}

/// Atomic retain of a `Shared` box (the cross-thread-shared refcount). Called when a boxed
/// Shared value is copied across a thread boundary (the nesting rule: the copy path bumps the
/// box's atomic refcount and shares it, rather than deep-copying through it).
#[no_mangle]
pub unsafe extern "C" fn lin_shared_retain(s: *const u8) {
    let b = unwrap_shared(s);
    if !b.is_null() {
        (*b).rc.fetch_add(1, Ordering::Relaxed);
    }
}

/// Atomic release of a `Shared` box. When the last reference drops, release the inner value and
/// free the box. Uses Acquire/Release fences so the final drop sees all prior writes.
#[no_mangle]
pub unsafe extern "C" fn lin_shared_release(s: *const u8) {
    let b = unwrap_shared(s);
    if b.is_null() {
        return;
    }
    if (*b).rc.fetch_sub(1, Ordering::Release) == 1 {
        std::sync::atomic::fence(Ordering::Acquire);
        // Last reference: release the inner value and reclaim the box.
        let inner = *(*b).inner.read().unwrap() as *mut u8;
        crate::tagged::lin_tagged_release(inner);
        drop(Box::from_raw(b as *mut SharedBox));
    }
}

/// Atomic retain given the RAW `*const SharedBox` payload (not a boxed TaggedVal*). Used by the
/// tag-aware `retain_tagged_payload` path. Null-safe.
#[no_mangle]
pub unsafe extern "C" fn lin_shared_retain_box(b: *const u8) {
    let b = b as *const SharedBox;
    if !b.is_null() {
        (*b).rc.fetch_add(1, Ordering::Relaxed);
    }
}

/// Atomic release given the RAW `*const SharedBox` payload. Frees the box (and releases its
/// inner value) when the last reference drops. Null-safe.
#[no_mangle]
pub unsafe extern "C" fn lin_shared_release_box(b: *const u8) {
    let b = b as *const SharedBox;
    if b.is_null() {
        return;
    }
    if (*b).rc.fetch_sub(1, Ordering::Release) == 1 {
        std::sync::atomic::fence(Ordering::Acquire);
        let inner = *(*b).inner.read().unwrap() as *mut u8;
        crate::tagged::lin_tagged_release(inner);
        drop(Box::from_raw(b as *mut SharedBox));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tagged::{alloc_tagged, TAG_INT32, TaggedVal};

    #[test]
    fn shared_get_set_roundtrip() {
        unsafe {
            let v = alloc_tagged(TAG_INT32, 7);
            let s = lin_shared_new(v);
            let got = lin_shared_get(s);
            assert_eq!((*(got as *const TaggedVal)).payload, 7);
            crate::tagged::lin_tagged_release(got);
            let nv = alloc_tagged(TAG_INT32, 99);
            lin_shared_set(s, nv);
            let got2 = lin_shared_get(s);
            assert_eq!((*(got2 as *const TaggedVal)).payload, 99);
            crate::tagged::lin_tagged_release(got2);
            crate::tagged::lin_tagged_release(nv);
            crate::tagged::lin_tagged_release(v);
            lin_shared_release(s);
        }
    }

    #[test]
    fn shared_concurrent_get_set() {
        unsafe {
            let v = alloc_tagged(TAG_INT32, 0);
            let s = lin_shared_new(v) as usize;
            crate::tagged::lin_tagged_release(v);
            let mut handles = Vec::new();
            for _ in 0..8 {
                let sp = s;
                handles.push(std::thread::spawn(move || {
                    for _ in 0..100 {
                        let g = lin_shared_get(sp as *const u8);
                        crate::tagged::lin_tagged_release(g);
                        let nv = alloc_tagged(TAG_INT32, 1);
                        lin_shared_set(sp as *const u8, nv);
                        crate::tagged::lin_tagged_release(nv);
                    }
                }));
            }
            for h in handles {
                h.join().unwrap();
            }
            lin_shared_release(s as *const u8);
        }
    }
}
