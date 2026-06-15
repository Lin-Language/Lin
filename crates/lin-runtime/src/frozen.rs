//! `Frozen<T>` — opt-in shared **read-only** state (ADR-028 §2.3.2, ADR-030).
//!
//! `frozen(v)` performs a deep, one-time **immortal seal** of a transferable graph: every heap
//! node (string, array, object, recursively) has its refcount saturated to `IMMORTAL_RC`. After
//! that, retain/release on those nodes are guarded no-ops (see the immortal guards in
//! string/array/object RC), so:
//!   * contents are never mutated and never freed → concurrent **reads** are safe;
//!   * the refcount is never written → reads of it from N threads aren't a data race;
//!   * therefore a read-only function compiled with ordinary **non-atomic** RC runs correctly on
//!     a shared frozen value with no recompilation, no lock, and no atomics.
//!
//! This is the interned-string immortality trick generalized from one string to a whole graph.
//! Cost: a frozen graph is **never freed** — `frozen` is for load-once, program-lifetime data.

use crate::tagged::{TaggedVal, TAG_STR, TAG_ARRAY, TAG_MAP};
use crate::string::{LinString, IMMORTAL_RC};
use crate::array::{LinArray, LinArrayElem};
use crate::map::LinMap;

/// Recursively seal a `LinString` immortal (idempotent).
unsafe fn freeze_string(s: *mut LinString) {
    if !s.is_null() {
        (*s).refcount = IMMORTAL_RC;
    }
}

/// Recursively seal a `LinArray` and all its (tagged) elements immortal. Flat scalar arrays have
/// no nested pointers, so only the header is sealed.
unsafe fn freeze_array(arr: *mut LinArray) {
    if arr.is_null() || (*arr).refcount >= IMMORTAL_RC {
        return; // null or already frozen (also breaks any accidental sharing/cycle)
    }
    (*arr).refcount = IMMORTAL_RC;
    if (*arr).elem_tag == 0xFF {
        let len = (*arr).len as usize;
        for i in 0..len {
            let elem = (*arr).data.add(i) as *mut LinArrayElem;
            freeze_payload((*elem).tag, (*elem).payload);
        }
    }
}

/// Recursively seal a `LinMap` (string-keyed open object, TAG_MAP) immortal.
unsafe fn freeze_map(map: *mut LinMap) {
    if map.is_null() || (*map).refcount >= IMMORTAL_RC {
        return;
    }
    (*map).refcount = IMMORTAL_RC;
    let cap = (*map).cap as usize;
    for i in 0..cap {
        let slot = (*map).slots.add(i);
        if (*slot).hash == 0 {
            continue;
        }
        // Freeze the string key.
        let key_ptr = (*slot).key as *mut crate::string::LinString;
        freeze_string(key_ptr);
        // Freeze the value payload.
        freeze_payload((*slot).value.tag, (*slot).value.payload);
    }
}

/// Seal one tagged payload by kind.
unsafe fn freeze_payload(tag: u8, payload: u64) {
    match tag {
        TAG_STR => freeze_string(payload as *mut LinString),
        TAG_ARRAY => freeze_array(payload as *mut LinArray),
        TAG_MAP => freeze_map(payload as *mut LinMap),
        _ => {} // scalars (TAG_OBJECT has no producers after Phase 3)
    }
}

/// `frozen(v)` — deep, transitive immortal+immutable seal of the graph rooted at boxed `v`
/// (a `TaggedVal*`). Returns `v` unchanged (now frozen): the value keeps its ordinary type, so
/// readers use it through the plain type. `v` must be transferable/acyclic (same rule as
/// `shared`). Idempotent and safe to call on an already-frozen graph.
#[no_mangle]
pub unsafe extern "C" fn lin_freeze(v: *mut u8) -> *mut u8 {
    if v.is_null() {
        return v;
    }
    let tv = &*(v as *const TaggedVal);
    freeze_payload(tv.tag, tv.payload);
    // The box shell itself: if it's a heap-allocated TaggedVal (not a cached scalar box), leave
    // it as the caller's owned box — the INNER graph is what's frozen and shared. Return as-is.
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::array::{lin_array_alloc, lin_array_push_tagged};
    use crate::tagged::{alloc_tagged, TAG_INT32};

    #[test]
    fn frozen_array_read_concurrently_is_race_free() {
        // N threads read a frozen array's header (length) and bump/drop its (immortal) refcount
        // via retain/release — which are guarded no-ops — concurrently. Under TSan this proves
        // the immortal-RC read path has no data race (the load-bearing Frozen<T> guarantee).
        unsafe {
            let arr = lin_array_alloc(8);
            for i in 0..5 {
                let e = alloc_tagged(TAG_INT32, i);
                lin_array_push_tagged(arr, e as *const u8);
                crate::tagged::lin_tagged_free_box(e);
            }
            let boxed = alloc_tagged(TAG_ARRAY, arr as u64);
            lin_freeze(boxed);
            let addr = arr as usize;
            let mut handles = Vec::new();
            for _ in 0..8 {
                handles.push(std::thread::spawn(move || {
                    let a = addr as *mut LinArray;
                    for _ in 0..200 {
                        // Read length + retain/release (guarded no-ops on the immortal array).
                        let _len = (*a).len;
                        crate::memory::lin_rc_retain(a as *mut u32);
                        crate::array::lin_array_release(a);
                    }
                }));
            }
            for h in handles {
                h.join().unwrap();
            }
            assert_eq!((*arr).len, 5);
            assert!((*arr).refcount >= IMMORTAL_RC);
        }
    }

    #[test]
    fn freeze_seals_array_immortal() {
        unsafe {
            let arr = lin_array_alloc(4);
            let e = alloc_tagged(TAG_INT32, 1);
            lin_array_push_tagged(arr, e as *const u8);
            crate::tagged::lin_tagged_free_box(e);
            let boxed = alloc_tagged(TAG_ARRAY, arr as u64);
            lin_freeze(boxed);
            assert!((*arr).refcount >= IMMORTAL_RC);
            // Release is now a no-op on the frozen array — it survives.
            crate::array::lin_array_release(arr);
            assert!((*arr).refcount >= IMMORTAL_RC);
        }
    }
}
