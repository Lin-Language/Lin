//! `LinMap` — the runtime backing for the typed index-signature object type `{ String: T }`
//! (ADR-055). A String-keyed hashed dictionary giving O(1) average lookup/insert, in contrast
//! to `LinObject`'s O(n) association-list scan (which is optimal for record-shaped objects but
//! catastrophic for dictionary-shaped ones — see ADR-055 / spec §5.1.1).
//!
//! Backing representation: a single open-addressing (linear-probing) hash table. Each occupied
//! slot stores `(key: *mut LinString, value: TaggedVal)`. Values are boxed inside the 16-byte
//! `TaggedVal` exactly like `LinObject` entries, so the refcount discipline (retain on store,
//! release on overwrite/free) is byte-for-byte the proven `object.rs` discipline — only the
//! *lookup* changes from a linear scan to a hash probe. (Unboxing scalar values is a documented
//! follow-up; boxed-but-hashed already delivers the O(1) headline win safely.)
//!
//! A distinct container (rather than retrofitting a hash side-index onto `LinObject`) deliberately
//! sidesteps the inline `MakeObject` codegen ABI constraint (`LinObject` entries GEP'd at
//! `entries@16`, 24-byte stride). `LinMap` is opaque to codegen — every access goes through these
//! FFI functions.

use std::alloc::{alloc, dealloc, Layout};
use crate::string::{LinString, lin_string_inc_ref, lin_string_release, IMMORTAL_RC};
use crate::tagged::{TaggedVal, TAG_NULL};

/// A hashed String-keyed map. `slots` points at `cap` `Slot`s (cap is always a power of two).
#[repr(C)]
pub struct LinMap {
    pub refcount: u32,
    pub len: u32, // number of occupied slots
    pub cap: u32, // table size (power of two), 0 = no table allocated yet
    _pad: u32,
    pub slots: *mut Slot,
}

#[repr(C)]
pub struct Slot {
    /// null = empty slot. (No tombstones: this map has no delete operation.)
    pub key: *mut LinString,
    pub value: TaggedVal,
}

const INITIAL_CAP: u32 = 8;
/// Grow when occupancy exceeds 7/8 of capacity (keeps probe chains short).
#[inline]
fn over_load(len: u32, cap: u32) -> bool {
    (len as u64) * 8 >= (cap as u64) * 7
}

unsafe fn map_header_layout() -> Layout {
    Layout::from_size_align_unchecked(
        std::mem::size_of::<LinMap>(),
        std::mem::align_of::<LinMap>(),
    )
}

unsafe fn slots_layout(cap: u32) -> Layout {
    Layout::from_size_align_unchecked(
        std::mem::size_of::<Slot>() * cap as usize,
        std::mem::align_of::<Slot>(),
    )
}

/// Allocate a zeroed slot table of `cap` slots (every slot empty: key = null).
unsafe fn alloc_slots(cap: u32) -> *mut Slot {
    let buf = alloc(slots_layout(cap)) as *mut Slot;
    // Zero the whole buffer: key = null (empty), value = {tag 0, payload 0}.
    std::ptr::write_bytes(buf as *mut u8, 0, slots_layout(cap).size());
    buf
}

/// FNV-1a hash over the key bytes.
unsafe fn hash_key(key: *const LinString) -> u64 {
    if key.is_null() {
        return 0;
    }
    let len = (*key).len as usize;
    let data = (*key).data.as_ptr();
    let bytes = std::slice::from_raw_parts(data, len);
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

unsafe fn key_eq(a: *const LinString, b: *const LinString) -> bool {
    if a == b {
        return true;
    }
    if a.is_null() || b.is_null() {
        return false;
    }
    let (al, bl) = ((*a).len, (*b).len);
    if al != bl {
        return false;
    }
    let aa = std::slice::from_raw_parts((*a).data.as_ptr(), al as usize);
    let bb = std::slice::from_raw_parts((*b).data.as_ptr(), bl as usize);
    aa == bb
}

#[no_mangle]
pub unsafe extern "C" fn lin_map_alloc(_hint: u32) -> *mut LinMap {
    let ptr = alloc(map_header_layout()) as *mut LinMap;
    (*ptr).refcount = 1;
    (*ptr).len = 0;
    (*ptr).cap = INITIAL_CAP;
    (*ptr)._pad = 0;
    (*ptr).slots = alloc_slots(INITIAL_CAP);
    ptr
}

/// Probe for `key`, returning the index of the matching slot or, if absent, the first empty slot
/// where it would be inserted. Requires cap > 0 and at least one empty slot (load factor < 1).
unsafe fn find_slot(map: *const LinMap, key: *const LinString) -> usize {
    let cap = (*map).cap as usize;
    let mask = cap - 1;
    let mut idx = (hash_key(key) as usize) & mask;
    loop {
        let slot = (*map).slots.add(idx);
        if (*slot).key.is_null() || key_eq((*slot).key, key) {
            return idx;
        }
        idx = (idx + 1) & mask;
    }
}

/// Double the table size and re-insert all live entries (keys/values move by raw bytes — no RC
/// change, ownership is preserved). The header never moves.
unsafe fn grow(map: *mut LinMap) {
    let old_cap = (*map).cap;
    let old_slots = (*map).slots;
    let new_cap = if old_cap == 0 { INITIAL_CAP } else { old_cap * 2 };
    let new_slots = alloc_slots(new_cap);
    (*map).slots = new_slots;
    (*map).cap = new_cap;
    let mask = (new_cap - 1) as usize;
    for i in 0..old_cap as usize {
        let src = old_slots.add(i);
        if (*src).key.is_null() {
            continue;
        }
        let mut idx = (hash_key((*src).key) as usize) & mask;
        loop {
            let dst = new_slots.add(idx);
            if (*dst).key.is_null() {
                (*dst).key = (*src).key;
                std::ptr::copy_nonoverlapping(&(*src).value, &mut (*dst).value, 1);
                break;
            }
            idx = (idx + 1) & mask;
        }
    }
    if old_cap > 0 {
        dealloc(old_slots as *mut u8, slots_layout(old_cap));
    }
}

/// Insert / overwrite `key -> *val`. Retains the value's inner payload (the map owns a reference);
/// retains the key on first insert. A null `val` pointer means the null Json value. The caller
/// keeps and releases its own key/value references (identical contract to `lin_object_set`).
#[no_mangle]
pub unsafe extern "C" fn lin_map_set(map: *mut LinMap, key: *mut LinString, val: *const TaggedVal) {
    let null_tv = TaggedVal { tag: TAG_NULL, _pad: [0; 7], payload: 0 };
    let val_ref: &TaggedVal = if val.is_null() { &null_tv } else { &*val };
    if (*map).cap == 0 || over_load((*map).len + 1, (*map).cap) {
        grow(map);
    }
    let idx = find_slot(map, key);
    let slot = (*map).slots.add(idx);
    if (*slot).key.is_null() {
        // Fresh insert.
        lin_string_inc_ref(key);
        (*slot).key = key;
        std::ptr::copy_nonoverlapping(val_ref, &mut (*slot).value, 1);
        crate::object::retain_tagged_payload_pub(val_ref);
        (*map).len += 1;
    } else {
        // Overwrite: release the old value, store the new, retain it.
        crate::object::release_tagged_payload_pub(&(*slot).value);
        std::ptr::copy_nonoverlapping(val_ref, &mut (*slot).value, 1);
        crate::object::retain_tagged_payload_pub(val_ref);
    }
}

/// Look up `key`; returns a borrowed pointer to the stored TaggedVal, or null if absent.
#[no_mangle]
pub unsafe extern "C" fn lin_map_get(map: *const LinMap, key: *const LinString) -> *const TaggedVal {
    if map.is_null() || (*map).cap == 0 || (*map).len == 0 {
        return std::ptr::null();
    }
    let idx = find_slot(map, key);
    let slot = (*map).slots.add(idx);
    if (*slot).key.is_null() {
        std::ptr::null()
    } else {
        &(*slot).value
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_map_has(map: *const LinMap, key: *const LinString) -> u8 {
    if lin_map_get(map, key).is_null() {
        0
    } else {
        1
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_map_length(map: *const LinMap) -> i64 {
    if map.is_null() {
        0
    } else {
        (*map).len as i64
    }
}

/// Return a `LinArray*` of all keys (as TAG_STR), insertion order NOT preserved (hash order).
#[no_mangle]
pub unsafe extern "C" fn lin_map_keys(map: *const LinMap) -> *mut crate::array::LinArray {
    let len = if map.is_null() { 0 } else { (*map).len };
    let arr = crate::array::lin_array_alloc(len as u64);
    if !map.is_null() {
        let cap = (*map).cap as usize;
        let mut out = 0usize;
        for i in 0..cap {
            let slot = (*map).slots.add(i);
            if (*slot).key.is_null() {
                continue;
            }
            lin_string_inc_ref((*slot).key);
            let dst = (*arr).data.add(out);
            (*dst).tag = crate::tagged::TAG_STR;
            (*dst).payload = (*slot).key as u64;
            out += 1;
        }
    }
    (*arr).len = len as u64;
    arr
}

/// Return a `LinArray*` of all values (each a TaggedVal copied + payload-retained).
#[no_mangle]
pub unsafe extern "C" fn lin_map_values(map: *const LinMap) -> *mut crate::array::LinArray {
    let len = if map.is_null() { 0 } else { (*map).len };
    let arr = crate::array::lin_array_alloc(len as u64);
    if !map.is_null() {
        let cap = (*map).cap as usize;
        let mut out = 0usize;
        for i in 0..cap {
            let slot = (*map).slots.add(i);
            if (*slot).key.is_null() {
                continue;
            }
            let src = &(*slot).value;
            let dst = (*arr).data.add(out) as *mut TaggedVal;
            std::ptr::copy_nonoverlapping(src as *const TaggedVal, dst, 1);
            crate::object::retain_tagged_payload_pub(src);
            out += 1;
        }
    }
    (*arr).len = len as u64;
    arr
}

/// Return a `LinArray*` of `[key, value]` pair arrays (hash order).
#[no_mangle]
pub unsafe extern "C" fn lin_map_entries(map: *const LinMap) -> *mut crate::array::LinArray {
    let len = if map.is_null() { 0 } else { (*map).len };
    let out = crate::array::lin_array_alloc(len as u64);
    if !map.is_null() {
        let cap = (*map).cap as usize;
        let mut o = 0usize;
        for i in 0..cap {
            let slot = (*map).slots.add(i);
            if (*slot).key.is_null() {
                continue;
            }
            let pair = crate::array::lin_array_alloc(2);
            (*(*pair).data.add(0)).tag = crate::tagged::TAG_STR;
            (*(*pair).data.add(0)).payload = (*slot).key as u64;
            lin_string_inc_ref((*slot).key);
            let src = &(*slot).value;
            std::ptr::copy_nonoverlapping(src as *const TaggedVal, (*pair).data.add(1) as *mut TaggedVal, 1);
            crate::object::retain_tagged_payload_pub(src);
            (*pair).len = 2;
            let dst = (*out).data.add(o);
            (*dst).tag = crate::tagged::TAG_ARRAY;
            (*dst).payload = pair as u64;
            o += 1;
        }
    }
    (*out).len = len as u64;
    out
}

/// Decrement the refcount; on reaching zero, release every key + value payload and free the table.
#[no_mangle]
pub unsafe extern "C" fn lin_map_release(map: *mut LinMap) {
    if map.is_null() {
        return;
    }
    if (*map).refcount >= IMMORTAL_RC {
        return;
    }
    (*map).refcount -= 1;
    if (*map).refcount != 0 {
        return;
    }
    let cap = (*map).cap;
    if cap > 0 && !(*map).slots.is_null() {
        for i in 0..cap as usize {
            let slot = (*map).slots.add(i);
            if !(*slot).key.is_null() {
                lin_string_release((*slot).key);
                crate::object::release_tagged_payload_pub(&(*slot).value);
            }
        }
        dealloc((*map).slots as *mut u8, slots_layout(cap));
    }
    dealloc(map as *mut u8, map_header_layout());
}

#[no_mangle]
pub unsafe extern "C" fn lin_map_retain(map: *mut LinMap) {
    if !map.is_null() && (*map).refcount < IMMORTAL_RC {
        (*map).refcount += 1;
    }
}

// ── Tag-aware bridges: keys/values/entries over a BOXED value (`TaggedVal*`) ──────────────
// These back the `std/object` wrappers (ADR-055) so the SAME `keys`/`values`/`entries` work on
// both a `Json`/`{}` record (TAG_OBJECT → `LinObject`) and a typed index-signature map (TAG_MAP →
// `LinMap`). The arg is a boxed `TaggedVal*`; we dispatch on its tag. A null/other tag yields an
// empty array. Each returns a freshly-owned `LinArray*` with its elements payload-retained.

use crate::tagged::{TAG_OBJECT, TAG_MAP};

#[no_mangle]
pub unsafe extern "C" fn lin_keys_any(p: *const u8) -> *mut crate::array::LinArray {
    if p.is_null() {
        return crate::array::lin_array_alloc(0);
    }
    let tv = &*(p as *const TaggedVal);
    match tv.tag {
        TAG_OBJECT => crate::object::lin_object_keys(tv.payload as *const crate::object::LinObject),
        TAG_MAP => lin_map_keys(tv.payload as *const LinMap),
        _ => crate::array::lin_array_alloc(0),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_values_any(p: *const u8) -> *mut crate::array::LinArray {
    if p.is_null() {
        return crate::array::lin_array_alloc(0);
    }
    let tv = &*(p as *const TaggedVal);
    match tv.tag {
        TAG_OBJECT => crate::object::lin_object_values(tv.payload as *const crate::object::LinObject),
        TAG_MAP => lin_map_values(tv.payload as *const LinMap),
        _ => crate::array::lin_array_alloc(0),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_entries_any(p: *const u8) -> *mut crate::array::LinArray {
    if p.is_null() {
        return crate::array::lin_array_alloc(0);
    }
    let tv = &*(p as *const TaggedVal);
    match tv.tag {
        TAG_OBJECT => crate::object::lin_object_entries(tv.payload as *const crate::object::LinObject),
        TAG_MAP => lin_map_entries(tv.payload as *const LinMap),
        _ => crate::array::lin_array_alloc(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::string::lin_string_alloc;
    use crate::tagged::{TaggedVal, TAG_INT32, TAG_STR};

    unsafe fn key(s: &str) -> *mut LinString {
        // Build a fresh HEAP (non-immortal, refcount 1) string so RC bugs on keys/values surface
        // under ASan — `lin_string_literal` returns immortal cached strings keyed by pointer, which
        // is unsafe to call with a temporary's address.
        let bytes = s.as_bytes();
        let ptr = lin_string_alloc(bytes.len() as u32);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), (*ptr).data.as_mut_ptr(), bytes.len());
        ptr
    }

    unsafe fn int_val(n: i32) -> TaggedVal {
        TaggedVal { tag: TAG_INT32, _pad: [0; 7], payload: n as u32 as u64 }
    }

    #[test]
    fn insert_lookup_overwrite_grow() {
        unsafe {
            let m = lin_map_alloc(0);
            // Insert many distinct keys (forces several grows past the initial 8 slots).
            for i in 0..200i32 {
                let k = key(&format!("k{i}"));
                let v = int_val(i * 10);
                lin_map_set(m, k, &v);
                lin_string_release(k); // caller drops its own ref; the map kept its own
            }
            assert_eq!(lin_map_length(m), 200);
            // Look every key back up.
            for i in 0..200i32 {
                let k = key(&format!("k{i}"));
                let got = lin_map_get(m, k);
                assert!(!got.is_null(), "k{i} missing");
                assert_eq!((*got).tag, TAG_INT32);
                assert_eq!((*got).payload as u32 as i32, i * 10);
                lin_string_release(k);
            }
            // Overwrite a key — length unchanged, value updated, old value released.
            let k = key("k5");
            let v = int_val(999);
            lin_map_set(m, k, &v);
            lin_string_release(k);
            assert_eq!(lin_map_length(m), 200);
            let k = key("k5");
            assert_eq!((*lin_map_get(m, k)).payload as u32 as i32, 999);
            lin_string_release(k);
            // Missing key → null.
            let k = key("nope");
            assert!(lin_map_get(m, k).is_null());
            lin_string_release(k);
            lin_map_release(m);
        }
    }

    #[test]
    fn string_values_rc_balanced() {
        unsafe {
            let m = lin_map_alloc(0);
            for i in 0..50i32 {
                let k = key(&format!("s{i}"));
                let sv = key(&format!("val{i}"));
                let v = TaggedVal { tag: TAG_STR, _pad: [0; 7], payload: sv as u64 };
                lin_map_set(m, k, &v);
                lin_string_release(k);
                lin_string_release(sv); // map keeps its own ref to the string value
            }
            // keys()/values()/entries() each retain their outputs; release them.
            let keys = lin_map_keys(m);
            let vals = lin_map_values(m);
            let ents = lin_map_entries(m);
            assert_eq!((*keys).len, 50);
            assert_eq!((*vals).len, 50);
            assert_eq!((*ents).len, 50);
            crate::array::lin_array_release(keys);
            crate::array::lin_array_release(vals);
            crate::array::lin_array_release(ents);
            lin_map_release(m); // frees every key + string value
        }
    }
}
