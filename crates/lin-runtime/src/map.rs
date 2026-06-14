//! `LinMap` — the runtime backing for the typed index-signature object type `{ String: T }`
//! (ADR-055). A String-keyed hashed dictionary giving O(1) average lookup/insert, in contrast
//! to `LinObject`'s O(n) association-list scan (which is optimal for record-shaped objects but
//! catastrophic for dictionary-shaped ones — see ADR-055 / spec §5.1.1).
//!
//! Backing representation: a single open-addressing (linear-probing) hash table. Each occupied
//! slot stores `(hash: u64, key: *mut LinString, value: TaggedVal)`. The stored hash acts as a
//! cheap first-comparison filter in `find_slot`: when the probe's hash doesn't match the lookup
//! key's hash, `key_eq` (which dereferences the key pointer — a cache miss) is skipped entirely.
//! Empty slots are identified by `key == null`; their `hash` field is ignored. The hash is
//! FNV-1a over the key bytes; a collision in the lower/upper bits is handled gracefully because
//! `key_eq` is the authoritative check and is always called on a hash match.
//!
//! Values are boxed inside the 16-byte `TaggedVal` exactly like `LinObject` entries, so the
//! refcount discipline (retain on store, release on overwrite/free) is byte-for-byte the proven
//! `object.rs` discipline — only the *lookup* changes from a linear scan to a hash probe.
//!
//! A distinct container (rather than retrofitting a hash side-index onto `LinObject`) deliberately
//! sidesteps the inline `MakeObject` codegen ABI constraint (`LinObject` entries GEP'd at
//! `entries@16`, 24-byte stride). `LinMap` is opaque to codegen — every access goes through these
//! FFI functions.
//!
//! ## LIN_MAP_PROFILE (env-gated, zero overhead when unset)
//! When `LIN_MAP_PROFILE=1`, atomic counters track:
//!   MAP_GETS      — total lin_map_get calls
//!   HASH_SKIPS    — probe steps skipped by hash mismatch (no key_eq call)
//!   KEY_EQ_CALLS  — probe steps where key_eq was called (hash matched or slot empty)
//!   KEY_EQ_MISS   — key_eq called + returned false (key content mismatch on hash collision)
//! These are printed to stderr at process exit.

use std::alloc::{alloc, dealloc, Layout};
use std::sync::atomic::{AtomicU64, Ordering};
use crate::string::{LinString, lin_string_inc_ref, lin_string_release, IMMORTAL_RC};
use crate::tagged::{TaggedVal, TAG_NULL};

// ── Profiling counters (env-gated, zero cost when disabled) ─────────────────────────────────
// State: 0 = uninit, 1 = disabled, 2 = enabled.
// Use SeqCst on the state transitions to prevent the compiler from reordering the init check
// with subsequent counter increments. On the steady-state fast path (state == 1 = disabled)
// the single SeqCst load adds ~1 cycle — invisible against 316M map_get calls.
use std::sync::atomic::AtomicU8;
static MAP_PROFILE_STATE: AtomicU8 = AtomicU8::new(0);
static MAP_GETS: AtomicU64 = AtomicU64::new(0);
static MAP_HASH_SKIPS: AtomicU64 = AtomicU64::new(0);
static MAP_KEY_EQ_CALLS: AtomicU64 = AtomicU64::new(0);
static MAP_KEY_EQ_MISS: AtomicU64 = AtomicU64::new(0);

/// Initialize the profiling state on the very first map operation.
/// After the first call this is a no-op (state != 0). Uses SeqCst to ensure visibility.
#[cold]
fn init_map_profile_state() -> u8 {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let enabled = std::env::var("LIN_MAP_PROFILE").as_deref() == Ok("1");
        let new_state: u8 = if enabled { 2 } else { 1 };
        MAP_PROFILE_STATE.store(new_state, Ordering::SeqCst);
        if enabled {
            unsafe { libc::atexit(map_profile_atexit); }
        }
    });
    MAP_PROFILE_STATE.load(Ordering::SeqCst)
}

/// Return the profiling state: 0=uninit (first call), 1=disabled, 2=enabled.
#[inline(always)]
fn map_profile_state() -> u8 {
    let s = MAP_PROFILE_STATE.load(Ordering::Relaxed);
    if s == 0 { init_map_profile_state() } else { s }
}

extern "C" fn map_profile_atexit() {
    lin_map_profile_print();
}

#[no_mangle]
pub extern "C" fn lin_map_profile_print() {
    if MAP_PROFILE_STATE.load(Ordering::Relaxed) == 2 {
        let gets = MAP_GETS.load(Ordering::Relaxed);
        let skips = MAP_HASH_SKIPS.load(Ordering::Relaxed);
        let eq_calls = MAP_KEY_EQ_CALLS.load(Ordering::Relaxed);
        let eq_miss = MAP_KEY_EQ_MISS.load(Ordering::Relaxed);
        let total_probes = skips + eq_calls;
        let probes_per_get = if gets > 0 { total_probes as f64 / gets as f64 } else { 0.0 };
        eprintln!(
            "MAP_PROFILE: gets={gets} total_probe_steps={total_probes} probes/get={probes_per_get:.2} \
             hash_skips={skips} ({:.1}%) key_eq_calls={eq_calls} key_eq_miss={eq_miss}",
            if total_probes > 0 { skips as f64 / total_probes as f64 * 100.0 } else { 0.0 },
        );
    }
}

/// A hashed String-keyed map. `slots` points at `cap` `Slot`s (cap is always a power of two).
#[repr(C)]
pub struct LinMap {
    pub refcount: u32,
    pub len: u32, // number of occupied slots
    pub cap: u32, // table size (power of two), 0 = no table allocated yet
    _pad: u32,
    pub slots: *mut Slot,
}

/// Each slot stores the full 64-bit FNV hash inline so probe steps can skip `key_eq`
/// (a pointer dereference — cache miss) whenever hashes differ.
/// Empty slot: `key == null` (hash is ignored for empty slots, left as zero).
#[repr(C)]
pub struct Slot {
    /// FNV-1a hash of the key's bytes. Zero means empty when `key` is also null (unambiguous
    /// because we check `key.is_null()` first). For an occupied slot this is the precomputed hash,
    /// so a probe step reads this u64 (hot — same cache line as the slot itself) before deciding
    /// whether to dereference `key` for `key_eq`.
    pub hash: u64,
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

/// Allocate a zeroed slot table of `cap` slots (every slot empty: key = null, hash = 0).
unsafe fn alloc_slots(cap: u32) -> *mut Slot {
    let buf = alloc(slots_layout(cap)) as *mut Slot;
    std::ptr::write_bytes(buf as *mut u8, 0, slots_layout(cap).size());
    buf
}

/// FNV-1a hash over the key bytes.
#[inline]
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

#[inline]
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

/// Probe for `key` with precomputed `khash`, returning the index of the matching slot or, if
/// absent, the first empty slot where it would be inserted. Requires cap > 0 and at least one
/// empty slot (load factor < 1).
///
/// Probe step logic (inner-loop fast path):
///   1. Read `slot.key` (always a hot load — same cache line as `slot.hash` in a 32-byte slot).
///   2. If `key == null` → empty slot: return (insertion point or miss).
///   3. If `slot.hash != khash` → different key by hash: skip `key_eq` entirely (saves the
///      dereference of the stored key pointer, a probable cache miss for a scattered map).
///   4. `key_eq` only on hash match: the full byte compare, which may deref both key pointers.
#[inline]
unsafe fn find_slot(map: *const LinMap, key: *const LinString, khash: u64) -> usize {
    let cap = (*map).cap as usize;
    let mask = cap - 1;
    let mut idx = (khash as usize) & mask;
    // Bound: defensive against a degenerate full table (same as before).
    for _ in 0..cap {
        let slot = (*map).slots.add(idx);
        let slot_key = (*slot).key;
        if slot_key.is_null() {
            return idx; // empty → insertion point or miss
        }
        if (*slot).hash == khash && key_eq(slot_key, key) {
            return idx; // found
        }
        idx = (idx + 1) & mask;
    }
    idx
}

/// Instrumented variant of find_slot — identical logic but bumps the profiling counters.
#[cold]
unsafe fn find_slot_profiled(map: *const LinMap, key: *const LinString, khash: u64) -> usize {
    let cap = (*map).cap as usize;
    let mask = cap - 1;
    let mut idx = (khash as usize) & mask;
    for _ in 0..cap {
        let slot = (*map).slots.add(idx);
        let slot_key = (*slot).key;
        if slot_key.is_null() {
            MAP_KEY_EQ_CALLS.fetch_add(1, Ordering::Relaxed); // null check counts as key_eq work
            return idx;
        }
        if (*slot).hash != khash {
            MAP_HASH_SKIPS.fetch_add(1, Ordering::Relaxed);
            idx = (idx + 1) & mask;
            continue;
        }
        MAP_KEY_EQ_CALLS.fetch_add(1, Ordering::Relaxed);
        if key_eq(slot_key, key) {
            return idx;
        }
        MAP_KEY_EQ_MISS.fetch_add(1, Ordering::Relaxed);
        idx = (idx + 1) & mask;
    }
    idx
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
        // Re-use the stored hash — no need to recompute FNV on grow.
        let mut idx = ((*src).hash as usize) & mask;
        loop {
            let dst = new_slots.add(idx);
            if (*dst).key.is_null() {
                (*dst).hash = (*src).hash;
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
    let khash = hash_key(key);
    let idx = find_slot(map, key, khash);
    let slot = (*map).slots.add(idx);
    if (*slot).key.is_null() {
        // Fresh insert: store hash + key + value.
        (*slot).hash = khash;
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
    let khash = hash_key(key);
    let idx = if map_profile_state() == 2 {
        MAP_GETS.fetch_add(1, Ordering::Relaxed);
        find_slot_profiled(map, key, khash)
    } else {
        find_slot(map, key, khash)
    };
    let slot = (*map).slots.add(idx);
    if (*slot).key.is_null() {
        std::ptr::null()
    } else {
        &(*slot).value
    }
}

/// Lookup by raw UTF-8 key bytes — the map analogue of `lin_object_get_bytes`. Avoids
/// allocating a temporary `LinString` for cold consumers (e.g. the fromJson validator).
/// Returns a borrowed `*const TaggedVal` into the map slot, or null if absent.
pub(crate) unsafe fn lin_map_get_bytes(
    map: *const LinMap,
    key_ptr: *const u8,
    key_len: u32,
) -> *const TaggedVal {
    if map.is_null() || (*map).cap == 0 || (*map).len == 0 {
        return std::ptr::null();
    }
    let bytes = std::slice::from_raw_parts(key_ptr, key_len as usize);
    // FNV-1a over the raw bytes — same algorithm as hash_key.
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let cap = (*map).cap as usize;
    let mask = cap - 1;
    let mut idx = (h as usize) & mask;
    for _ in 0..cap {
        let slot = (*map).slots.add(idx);
        let slot_key = (*slot).key;
        if slot_key.is_null() {
            return std::ptr::null();
        }
        if (*slot).hash == h {
            let kl = (*slot_key).len as usize;
            if kl == bytes.len() {
                let kb = std::slice::from_raw_parts((*slot_key).data.as_ptr(), kl);
                if kb == bytes {
                    return &(*slot).value;
                }
            }
        }
        idx = (idx + 1) & mask;
    }
    std::ptr::null()
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

use crate::tagged::{TAG_OBJECT, TAG_MAP, TAG_RECORD};

#[no_mangle]
pub unsafe extern "C" fn lin_keys_any(p: *const u8) -> *mut crate::array::LinArray {
    if p.is_null() {
        return crate::array::lin_array_alloc(0);
    }
    let tv = &*(p as *const TaggedVal);
    match tv.tag {
        TAG_OBJECT => crate::object::lin_object_keys(tv.payload as *const crate::object::LinObject),
        TAG_MAP => lin_map_keys(tv.payload as *const LinMap),
        // Stage 6a: TAG_RECORD — sealed struct pointer; materialize to a transient LinObject to
        // enumerate its keys, then release the temporary.
        TAG_RECORD => {
            let sealed = tv.payload as *mut u8;
            if sealed.is_null() { return crate::array::lin_array_alloc(0); }
            let named_desc = *((sealed.add(16)) as *const *const u8);
            let mat = crate::sealed::materialize_sealed_struct_pub(sealed, named_desc);
            if mat.is_null() { return crate::array::lin_array_alloc(0); }
            let arr = crate::object::lin_object_keys(mat as *const crate::object::LinObject);
            crate::object::lin_object_release(mat);
            arr
        }
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
        // Stage 6a: TAG_RECORD — materialize sealed struct to extract values, then release.
        TAG_RECORD => {
            let sealed = tv.payload as *mut u8;
            if sealed.is_null() { return crate::array::lin_array_alloc(0); }
            let named_desc = *((sealed.add(16)) as *const *const u8);
            let mat = crate::sealed::materialize_sealed_struct_pub(sealed, named_desc);
            if mat.is_null() { return crate::array::lin_array_alloc(0); }
            let arr = crate::object::lin_object_values(mat as *const crate::object::LinObject);
            crate::object::lin_object_release(mat);
            arr
        }
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
        // Stage 6a: TAG_RECORD — materialize sealed struct to extract entries, then release.
        TAG_RECORD => {
            let sealed = tv.payload as *mut u8;
            if sealed.is_null() { return crate::array::lin_array_alloc(0); }
            let named_desc = *((sealed.add(16)) as *const *const u8);
            let mat = crate::sealed::materialize_sealed_struct_pub(sealed, named_desc);
            if mat.is_null() { return crate::array::lin_array_alloc(0); }
            let arr = crate::object::lin_object_entries(mat as *const crate::object::LinObject);
            crate::object::lin_object_release(mat);
            arr
        }
        _ => crate::array::lin_array_alloc(0),
    }
}

/// Coerce a (possibly boxed) value to a raw `LinMap*` for the Json/Object → `{ String: T }`
/// boundary, returning a value with ONE owned reference (rc +1 the caller owns).
///
/// `p` is a `*TaggedVal` (the boxed value at the coercion site). Dispatch on its tag:
///   * TAG_MAP — the value is ALREADY a map (e.g. a real `{ String: T }` flowing through the Json
///     supertype, or a nested map value): retain it and return the same pointer. NO copy — keeps
///     identity so a later mutation is observed (the `test_typed_map_through_function_value`
///     nested case) and avoids an O(n) rebuild.
///   * TAG_OBJECT — the value is a `LinObject` (an empty object literal `{}`, a `Json` object
///     field): MATERIALIZE a fresh `LinMap` from its entries, because the map accessors
///     (`lin_map_get`/`_set`) would otherwise read a `LinObject`'s bytes as a hash table and
///     corrupt the heap. `lin_map_set` retains each key/value, so the new map owns its references;
///     the source object is untouched.
///   * anything else (null / non-container) — an empty map.
///
/// Returning a +1-owned map in every arm matches the `register_owned` the lowerer applies to a
/// Coerce result, so the scheduled release balances regardless of which arm ran.
#[no_mangle]
pub unsafe extern "C" fn lin_to_map(p: *const u8) -> *mut LinMap {
    if p.is_null() {
        return lin_map_alloc(0);
    }
    let tv = &*(p as *const TaggedVal);
    match tv.tag {
        crate::tagged::TAG_MAP => {
            let m = tv.payload as *mut LinMap;
            if !m.is_null() && (*m).refcount < IMMORTAL_RC {
                (*m).refcount += 1;
            }
            m
        }
        crate::tagged::TAG_OBJECT => lin_object_to_map(tv.payload as *const crate::object::LinObject),
        _ => lin_map_alloc(0),
    }
}

/// Build a fresh `LinMap` (rc = 1) from a `LinObject`'s entries. The materialization half of
/// `lin_to_map` (which dispatches the already-a-map case). Each value's inner payload is retained
/// by `lin_map_set`; the source object is unchanged.
#[no_mangle]
pub unsafe extern "C" fn lin_object_to_map(obj: *const crate::object::LinObject) -> *mut LinMap {
    let map = lin_map_alloc(0);
    if obj.is_null() {
        return map;
    }
    let len = (*obj).len;
    for i in 0..len {
        let entry = (*obj).entries.add(i as usize);
        let key = (*entry).key;
        if !key.is_null() {
            lin_map_set(map, key, &(*entry).value as *const TaggedVal);
        }
    }
    map
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
