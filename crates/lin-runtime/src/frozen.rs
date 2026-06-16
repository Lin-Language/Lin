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

use crate::tagged::{TaggedVal, TAG_STR, TAG_ARRAY, TAG_MAP, TAG_RECORD, TAG_SUMNODE};
use crate::string::{LinString, IMMORTAL_RC};
use crate::array::{LinArray, LinArrayElem};
use crate::map::LinMap;

/// Recursively seal a `LinString` immortal (idempotent). `pub(crate)` so the sum-node freeze walk
/// (`sumnode::lin_sumnode_freeze`) can seal a variant's string fields with the same primitive.
pub(crate) unsafe fn freeze_string(s: *mut LinString) {
    if !s.is_null() {
        (*s).refcount = IMMORTAL_RC;
    }
}

/// Recursively seal a `LinArray` and all its (tagged) elements immortal. Flat scalar arrays have
/// no nested pointers, so only the header is sealed.
pub(crate) unsafe fn freeze_array(arr: *mut LinArray) {
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
pub(crate) unsafe fn freeze_map(map: *mut LinMap) {
    if map.is_null() || (*map).refcount >= IMMORTAL_RC {
        return;
    }
    (*map).refcount = IMMORTAL_RC;
    // Value-unboxed slots: iterate via the map's encapsulated helper (reconstructs each value).
    crate::map::map_for_each_slot(map, |key_bits, val| {
        freeze_string(key_bits as *mut crate::string::LinString);
        freeze_payload(val.tag, val.payload);
    });
}

/// Seal one tagged payload by kind.
unsafe fn freeze_payload(tag: u8, payload: u64) {
    match tag {
        TAG_STR => freeze_string(payload as *mut LinString),
        TAG_ARRAY => freeze_array(payload as *mut LinArray),
        TAG_MAP => freeze_map(payload as *mut LinMap),
        // A packed record / sum node boxed into a dynamic (AnyVal/union) slot — e.g. produced by
        // lin_box_record / lin_box_sumnode on the sealed→AnyVal coerce path — must ALSO be sealed,
        // else a frozen graph holding one leaves that node MORTAL: shared read-only across threads
        // under non-atomic RC, a stray release frees it → cross-thread UAF (the exact thing frozen()
        // exists to prevent). Recurse into its heap fields via the type's descriptor.
        TAG_RECORD => freeze_sealed(payload as *mut u8),
        TAG_SUMNODE => crate::sumnode::lin_sumnode_freeze(payload as *mut u8),
        _ => {} // scalars; opaque atomic handles (Shared/BigInt/Decimal) are already immortal-safe
    }
}

/// Recursively seal a packed sealed-record struct (TAG_RECORD) immortal: saturate its own offset-0
/// refcount and freeze every heap field per the descriptor at header offset 8. Mirrors
/// `sealed::release_heap_fields`' descriptor walk but immortal-seals instead of releasing.
/// (Kept here, not in sealed.rs, so the freeze walk and the drop walk don't share a file during the
/// reset; the planned TagClass walker unification — docs/TODO.md B2 — will merge these.)
pub(crate) unsafe fn freeze_sealed(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    let rc = ptr as *mut u32;
    if *rc >= IMMORTAL_RC {
        return; // already frozen — also breaks any accidental sharing/cycle
    }
    *rc = IMMORTAL_RC;
    // heap_desc @ offset 8; NULL for a scalar-only record.
    let desc = *((ptr.add(8)) as *const *const u8);
    if desc.is_null() {
        return;
    }
    let count = *(desc as *const u32);
    let entries = desc.add(4); // each entry = { u32 offset, u32 kind }
    for i in 0..count as usize {
        let ent = entries.add(i * 8);
        let offset = *(ent as *const u32) as usize;
        let kind = *((ent.add(4)) as *const u32);
        let payload = *(ptr.add(offset) as *const *mut u8);
        if payload.is_null() {
            continue;
        }
        match kind {
            crate::sealed::KIND_STRING => freeze_string(payload as *mut LinString),
            crate::sealed::KIND_ARRAY => freeze_array(payload as *mut LinArray),
            crate::sealed::KIND_SEALED => freeze_sealed(payload),
            crate::sealed::KIND_MAP => freeze_map(payload as *mut LinMap),
            crate::sealed::KIND_SUMNODE_FIELD => crate::sumnode::lin_sumnode_freeze(payload),
            _ => {}
        }
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
