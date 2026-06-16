//! `LinMap` — the runtime backing for the typed index-signature object type `{ K: V }`
//! (ADR-055 + numeric-key extension). A hashed dictionary giving O(1) average
//! lookup/insert, in contrast to `LinObject`'s O(n) association-list scan.
//!
//! Two key kinds are supported, selected by `LinMap::key_kind`:
//!   - `KEY_KIND_STRING` (0) — String-keyed: arbitrary string keys. Hash = FNV-1a over bytes.
//!   - `KEY_KIND_INT` (1) — Int-keyed: arbitrary i64 integer keys. Hash = fmix64 mixer.
//!
//! Backing representation: a single open-addressing (linear-probing) hash table.
//! Each occupied slot stores `(hash: u64, key: u64, value: TaggedVal)`.
//!
//! **Occupancy rule (changed from old key==null)**: empty slots are identified by `hash == 0`.
//! For both key kinds we guarantee every real key's hash is nonzero, so `hash==0` unambiguously
//! marks an empty slot without needing to store a null sentinel pointer. This allows integer key 0
//! to be stored (its hash is fmix64(0) != 0).
//!
//! String keys: cast `u64` as `*mut LinString`. RC discipline unchanged.
//! Int keys: raw i64 stored as `u64` via bitcast. No RC on keys.
//!
//! ## LIN_MAP_PROFILE (env-gated, zero overhead when unset)
//! When `LIN_MAP_PROFILE=1`, atomic counters track:
//!   MAP_GETS      — total lin_map_get calls
//!   HASH_SKIPS    — probe steps skipped by hash mismatch (no key_eq call)
//!   KEY_EQ_CALLS  — probe steps where key_eq was called (hash matched or slot empty)
//!   KEY_EQ_MISS   — key_eq called + returned false (key content mismatch on hash collision)
//! These are printed to stderr at process exit.

use std::alloc::{alloc, alloc_zeroed, dealloc, Layout};
use std::sync::atomic::{AtomicU64, Ordering};
use crate::string::{LinString, lin_string_inc_ref, lin_string_release, IMMORTAL_RC};
use crate::tagged::{TaggedVal, TAG_NULL};

// Key-kind constants.
pub const KEY_KIND_STRING: u32 = 0;
pub const KEY_KIND_INT: u32 = 1;

// ── Profiling counters (env-gated, zero cost when disabled) ─────────────────────────────────
use std::sync::atomic::AtomicU8;
static MAP_PROFILE_STATE: AtomicU8 = AtomicU8::new(0);
static MAP_GETS: AtomicU64 = AtomicU64::new(0);
static MAP_HASH_SKIPS: AtomicU64 = AtomicU64::new(0);
static MAP_KEY_EQ_CALLS: AtomicU64 = AtomicU64::new(0);
static MAP_KEY_EQ_MISS: AtomicU64 = AtomicU64::new(0);

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

/// A hashed map. `slots` points at `cap` `Slot`s (cap is always a power of two).
/// `key_kind` selects the key type: KEY_KIND_STRING (0) or KEY_KIND_INT (1).
/// `order` is a heap-allocated `*mut u64` array of length `len` tracking insertion order:
/// each entry is a raw key (pointer for String maps, i64 bits for Int maps) in the order
/// the key was FIRST inserted. This lets `lin_map_keys` return keys in insertion order,
/// matching `lin_object_keys` behavior. The order array is grown by doubling (cap_order
/// tracks its allocation size; always >= len).
#[repr(C)]
pub struct LinMap {
    pub refcount: u32,
    pub len: u32,       // number of occupied slots
    pub cap: u32,       // table size (power of two), 0 = no table allocated yet
    pub key_kind: u32,  // KEY_KIND_STRING (0) or KEY_KIND_INT (1)
    pub slots: *mut Slot,
    pub order: *mut u64, // insertion-order key list, length = len, capacity = cap_order
    pub cap_order: u32,  // allocated capacity of `order` array
    pub _pad: u32,       // padding for 8-byte alignment
}

/// Each slot stores the full 64-bit hash inline so probe steps can skip `key_eq`
/// whenever hashes differ.
///
/// **Occupancy rule**: an empty slot has `hash == 0`. We ensure every real key's
/// hash is nonzero so this is unambiguous (no null pointer sentinel needed).
/// The `key` field is a raw `u64`: for String maps it holds a `*mut LinString` cast to u64;
/// for Int maps it holds the raw i64 key bits. When `hash == 0` the `key` field is ignored.
#[repr(C)]
pub struct Slot {
    /// Nonzero hash = occupied; zero = empty.
    pub hash: u64,
    /// String map: *mut LinString (cast to u64). Int map: i64 key (cast to u64).
    pub key: u64,
    pub value: TaggedVal,
}

const INITIAL_CAP: u32 = 4;
#[inline]
fn over_load(len: u32, cap: u32) -> bool {
    (len as u64) * 10 >= (cap as u64) * 7
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

/// Allocate a zeroed slot table of `cap` slots (every slot empty: hash = 0).
unsafe fn alloc_slots(cap: u32) -> *mut Slot {
    alloc_zeroed(slots_layout(cap)) as *mut Slot
}

unsafe fn order_layout(cap: u32) -> Layout {
    Layout::from_size_align_unchecked(
        std::mem::size_of::<u64>() * cap as usize,
        std::mem::align_of::<u64>(),
    )
}

/// Allocate an uninitialised order array of `cap` u64 slots.
unsafe fn alloc_order(cap: u32) -> *mut u64 {
    alloc(order_layout(cap)) as *mut u64
}

/// Append `key` to the order list, growing if necessary.
unsafe fn order_push(map: *mut LinMap, key: u64) {
    let len = (*map).len; // called before len is incremented
    if len >= (*map).cap_order {
        let old_cap = (*map).cap_order;
        let new_cap = if old_cap == 0 { INITIAL_CAP } else { old_cap * 2 };
        let new_order = alloc_order(new_cap) as *mut u64;
        std::ptr::copy_nonoverlapping((*map).order, new_order, len as usize);
        dealloc((*map).order as *mut u8, order_layout(old_cap));
        (*map).order = new_order;
        (*map).cap_order = new_cap;
    }
    *(*map).order.add(len as usize) = key;
}

// ── Hash functions ───────────────────────────────────────────────────────────────────────────

/// FNV-1a hash over a raw byte slice. Returns nonzero (maps 0 → 1).
#[inline]
fn fnv1a_bytes(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    if h == 0 { 1 } else { h }
}

/// FNV-1a hash over the key bytes (String kind).
/// Returns a nonzero value (maps 0 → 1 to avoid the empty-slot sentinel).
#[inline]
unsafe fn hash_string_key(key: *const LinString) -> u64 {
    if key.is_null() {
        return 1; // degenerate — shouldn't happen for a valid string key
    }
    let len = (*key).len as usize;
    let data = (*key).data.as_ptr();
    let bytes = std::slice::from_raw_parts(data, len);
    fnv1a_bytes(bytes)
}

/// Murmurhash3 finalizer (fmix64) for Int keys.
/// Guarantees nonzero output (maps 0 → 1).
#[inline]
fn fmix64(mut k: u64) -> u64 {
    k ^= k >> 33;
    k = k.wrapping_mul(0xff51afd7ed558ccd);
    k ^= k >> 33;
    k = k.wrapping_mul(0xc4ceb9fe1a85ec53);
    k ^= k >> 33;
    if k == 0 { 1 } else { k }
}

// ── Key equality ─────────────────────────────────────────────────────────────────────────────

#[inline]
unsafe fn string_key_eq(a: *const LinString, b: *const LinString) -> bool {
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

// ── Slot finder (String kind) ────────────────────────────────────────────────────────────────

/// Find the slot index for `key` (String map). Returns the index of the matching slot or,
/// if absent, the first empty slot (hash == 0).
#[inline]
unsafe fn find_slot_string(map: *const LinMap, key: *const LinString, khash: u64) -> usize {
    let cap = (*map).cap as usize;
    let mask = cap - 1;
    let mut idx = (khash as usize) & mask;
    for _ in 0..cap {
        let slot = (*map).slots.add(idx);
        if (*slot).hash == 0 {
            return idx; // empty → insertion point or miss
        }
        if (*slot).hash == khash && string_key_eq((*slot).key as *const LinString, key) {
            return idx; // found
        }
        idx = (idx + 1) & mask;
    }
    idx
}

#[cold]
unsafe fn find_slot_string_profiled(map: *const LinMap, key: *const LinString, khash: u64) -> usize {
    let cap = (*map).cap as usize;
    let mask = cap - 1;
    let mut idx = (khash as usize) & mask;
    for _ in 0..cap {
        let slot = (*map).slots.add(idx);
        if (*slot).hash == 0 {
            MAP_KEY_EQ_CALLS.fetch_add(1, Ordering::Relaxed);
            return idx;
        }
        if (*slot).hash != khash {
            MAP_HASH_SKIPS.fetch_add(1, Ordering::Relaxed);
            idx = (idx + 1) & mask;
            continue;
        }
        MAP_KEY_EQ_CALLS.fetch_add(1, Ordering::Relaxed);
        if string_key_eq((*slot).key as *const LinString, key) {
            return idx;
        }
        MAP_KEY_EQ_MISS.fetch_add(1, Ordering::Relaxed);
        idx = (idx + 1) & mask;
    }
    idx
}

// ── Slot finder (Int kind) ────────────────────────────────────────────────────────────────────

/// Find the slot index for an Int key (raw i64 as u64). Empty = hash==0.
#[inline]
unsafe fn find_slot_int(map: *const LinMap, key_bits: u64, khash: u64) -> usize {
    let cap = (*map).cap as usize;
    let mask = cap - 1;
    let mut idx = (khash as usize) & mask;
    for _ in 0..cap {
        let slot = (*map).slots.add(idx);
        if (*slot).hash == 0 {
            return idx; // empty → miss or insertion point
        }
        if (*slot).hash == khash && (*slot).key == key_bits {
            return idx; // found
        }
        idx = (idx + 1) & mask;
    }
    idx
}

// ── grow ─────────────────────────────────────────────────────────────────────────────────────

/// Double the table size and re-insert all live entries. Works for both key kinds:
/// empty detection via `hash == 0`, re-hash via stored hash.
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
        if (*src).hash == 0 {
            continue; // empty slot — skip
        }
        // Re-use the stored hash — no need to recompute.
        let mut dst_idx = ((*src).hash as usize) & mask;
        loop {
            let dst = new_slots.add(dst_idx);
            if (*dst).hash == 0 {
                (*dst).hash = (*src).hash;
                (*dst).key = (*src).key;
                std::ptr::copy_nonoverlapping(&(*src).value, &mut (*dst).value, 1);
                break;
            }
            dst_idx = (dst_idx + 1) & mask;
        }
    }
    if old_cap > 0 {
        dealloc(old_slots as *mut u8, slots_layout(old_cap));
    }
}

// ── Public API ───────────────────────────────────────────────────────────────────────────────

/// Allocate a new `LinMap` with the given `key_kind` (KEY_KIND_STRING or KEY_KIND_INT).
/// `hint` is the expected initial capacity; the table is sized to the next power-of-two
/// >= max(hint, INITIAL_CAP) so that maps built with a known count avoid an early grow.
#[no_mangle]
pub unsafe extern "C" fn lin_map_alloc(hint: u32, key_kind: u32) -> *mut LinMap {
    let cap = hint.next_power_of_two().max(INITIAL_CAP);
    let ptr = alloc(map_header_layout()) as *mut LinMap;
    (*ptr).refcount = 1;
    (*ptr).len = 0;
    (*ptr).cap = cap;
    (*ptr).key_kind = key_kind;
    (*ptr).slots = alloc_slots(cap);
    (*ptr).order = alloc_order(cap);
    (*ptr).cap_order = cap;
    (*ptr)._pad = 0;
    ptr
}

/// Insert / overwrite `key -> *val` (String map). Retains value and key on insert.
/// A null `val` pointer means the null value.
#[no_mangle]
pub unsafe extern "C" fn lin_map_set(map: *mut LinMap, key: *mut LinString, val: *const TaggedVal) {
    let null_tv = TaggedVal { tag: TAG_NULL, _pad: [0; 7], payload: 0 };
    let val_ref: &TaggedVal = if val.is_null() { &null_tv } else { &*val };
    if (*map).cap == 0 || over_load((*map).len + 1, (*map).cap) {
        grow(map);
    }
    let khash = hash_string_key(key);
    let idx = find_slot_string(map, key, khash);
    let slot = (*map).slots.add(idx);
    if (*slot).hash == 0 {
        // Fresh insert — record insertion order before incrementing len.
        order_push(map, key as u64);
        (*slot).hash = khash;
        lin_string_inc_ref(key);
        (*slot).key = key as u64;
        std::ptr::copy_nonoverlapping(val_ref, &mut (*slot).value, 1);
        crate::tagged::retain_tagged_payload_pub(val_ref);
        (*map).len += 1;
    } else {
        // Overwrite — order unchanged (key already present).
        crate::tagged::release_tagged_payload_pub(&(*slot).value);
        std::ptr::copy_nonoverlapping(val_ref, &mut (*slot).value, 1);
        crate::tagged::retain_tagged_payload_pub(val_ref);
    }
}

/// Look up `key` (String map). Returns borrowed pointer or null if absent.
#[no_mangle]
pub unsafe extern "C" fn lin_map_get(map: *const LinMap, key: *const LinString) -> *const TaggedVal {
    if map.is_null() || (*map).cap == 0 || (*map).len == 0 {
        return std::ptr::null();
    }
    let khash = hash_string_key(key);
    let idx = if map_profile_state() == 2 {
        MAP_GETS.fetch_add(1, Ordering::Relaxed);
        find_slot_string_profiled(map, key, khash)
    } else {
        find_slot_string(map, key, khash)
    };
    let slot = (*map).slots.add(idx);
    if (*slot).hash == 0 {
        std::ptr::null()
    } else {
        &(*slot).value
    }
}

/// Insert / overwrite `key -> *val` (Int map). `key` is a raw i64.
#[no_mangle]
pub unsafe extern "C" fn lin_map_set_int(map: *mut LinMap, key: i64, val: *const TaggedVal) {
    let null_tv = TaggedVal { tag: TAG_NULL, _pad: [0; 7], payload: 0 };
    let val_ref: &TaggedVal = if val.is_null() { &null_tv } else { &*val };
    if (*map).cap == 0 || over_load((*map).len + 1, (*map).cap) {
        grow(map);
    }
    let key_bits = key as u64;
    let khash = fmix64(key_bits);
    let idx = find_slot_int(map, key_bits, khash);
    let slot = (*map).slots.add(idx);
    if (*slot).hash == 0 {
        // Fresh insert: no key RC (scalar) — record insertion order.
        order_push(map, key_bits);
        (*slot).hash = khash;
        (*slot).key = key_bits;
        std::ptr::copy_nonoverlapping(val_ref, &mut (*slot).value, 1);
        crate::tagged::retain_tagged_payload_pub(val_ref);
        (*map).len += 1;
    } else {
        // Overwrite — order unchanged.
        crate::tagged::release_tagged_payload_pub(&(*slot).value);
        std::ptr::copy_nonoverlapping(val_ref, &mut (*slot).value, 1);
        crate::tagged::retain_tagged_payload_pub(val_ref);
    }
}

/// Look up `key` (Int map). Returns borrowed pointer or null if absent.
#[no_mangle]
pub unsafe extern "C" fn lin_map_get_int(map: *const LinMap, key: i64) -> *const TaggedVal {
    if map.is_null() || (*map).cap == 0 || (*map).len == 0 {
        return std::ptr::null();
    }
    let key_bits = key as u64;
    let khash = fmix64(key_bits);
    let idx = find_slot_int(map, key_bits, khash);
    let slot = (*map).slots.add(idx);
    if (*slot).hash == 0 {
        std::ptr::null()
    } else {
        &(*slot).value
    }
}

/// Lookup by raw UTF-8 key bytes — the map analogue of `lin_object_get_bytes`. Avoids
/// allocating a temporary `LinString` for cold consumers (e.g. the fromJson validator).
/// Uses the same FNV-1a hash and probe structure as `find_slot_string` to stay in lockstep.
pub(crate) unsafe fn lin_map_get_bytes(
    map: *const LinMap,
    key_ptr: *const u8,
    key_len: u32,
) -> *const TaggedVal {
    if map.is_null() || (*map).cap == 0 || (*map).len == 0 {
        return std::ptr::null();
    }
    let bytes = std::slice::from_raw_parts(key_ptr, key_len as usize);
    let h = fnv1a_bytes(bytes);
    let cap = (*map).cap as usize;
    let mask = cap - 1;
    let mut idx = (h as usize) & mask;
    for _ in 0..cap {
        let slot = (*map).slots.add(idx);
        if (*slot).hash == 0 {
            return std::ptr::null();
        }
        if (*slot).hash == h {
            let slot_key = (*slot).key as *const LinString;
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
    if lin_map_get(map, key).is_null() { 0 } else { 1 }
}

#[no_mangle]
pub unsafe extern "C" fn lin_map_length(map: *const LinMap) -> i64 {
    if map.is_null() { 0 } else { (*map).len as i64 }
}

/// Return a `LinArray*` of all keys. For String maps: TAG_STR strings. For Int maps: TAG_INT64.
#[no_mangle]
pub unsafe extern "C" fn lin_map_keys(map: *const LinMap) -> *mut crate::array::LinArray {
    let len = if map.is_null() { 0 } else { (*map).len };
    let arr = crate::array::lin_array_alloc(len as u64);
    if !map.is_null() && len > 0 && !(*map).order.is_null() {
        let is_int = (*map).key_kind == KEY_KIND_INT;
        for i in 0..len as usize {
            let key = *(*map).order.add(i);
            let dst = (*arr).data.add(i);
            if is_int {
                (*dst).tag = crate::tagged::TAG_INT64;
                (*dst).payload = key;
            } else {
                let key_ptr = key as *mut LinString;
                lin_string_inc_ref(key_ptr);
                (*dst).tag = crate::tagged::TAG_STR;
                (*dst).payload = key;
            }
        }
    }
    (*arr).len = len as u64;
    arr
}

/// Return a `LinArray*` of all values in insertion order (matches `lin_map_keys` order).
#[no_mangle]
pub unsafe extern "C" fn lin_map_values(map: *const LinMap) -> *mut crate::array::LinArray {
    let len = if map.is_null() { 0 } else { (*map).len };
    let arr = crate::array::lin_array_alloc(len as u64);
    if !map.is_null() && len > 0 && !(*map).order.is_null() {
        let is_int = (*map).key_kind == KEY_KIND_INT;
        for i in 0..len as usize {
            let key = *(*map).order.add(i);
            let src = if is_int {
                lin_map_get_int(map, key as i64)
            } else {
                lin_map_get(map, key as *const LinString)
            };
            let dst = (*arr).data.add(i) as *mut TaggedVal;
            if src.is_null() {
                (*dst).tag = TAG_NULL;
                (*dst).payload = 0;
            } else {
                std::ptr::copy_nonoverlapping(src, dst, 1);
                crate::tagged::retain_tagged_payload_pub(&*src);
            }
        }
    }
    (*arr).len = len as u64;
    arr
}

/// Return a `LinArray*` of `[key, value]` pair arrays in insertion order (matches `lin_map_keys`).
#[no_mangle]
pub unsafe extern "C" fn lin_map_entries(map: *const LinMap) -> *mut crate::array::LinArray {
    let len = if map.is_null() { 0 } else { (*map).len };
    let out = crate::array::lin_array_alloc(len as u64);
    if !map.is_null() && len > 0 && !(*map).order.is_null() {
        let is_int = (*map).key_kind == KEY_KIND_INT;
        for i in 0..len as usize {
            let key = *(*map).order.add(i);
            let pair = crate::array::lin_array_alloc(2);
            if is_int {
                (*(*pair).data.add(0)).tag = crate::tagged::TAG_INT64;
                (*(*pair).data.add(0)).payload = key;
            } else {
                let key_ptr = key as *mut LinString;
                lin_string_inc_ref(key_ptr);
                (*(*pair).data.add(0)).tag = crate::tagged::TAG_STR;
                (*(*pair).data.add(0)).payload = key;
            }
            let src = if is_int {
                lin_map_get_int(map, key as i64)
            } else {
                lin_map_get(map, key as *const LinString)
            };
            let val_dst = (*pair).data.add(1) as *mut TaggedVal;
            if src.is_null() {
                (*val_dst).tag = TAG_NULL;
                (*val_dst).payload = 0;
            } else {
                std::ptr::copy_nonoverlapping(src, val_dst, 1);
                crate::tagged::retain_tagged_payload_pub(&*src);
            }
            (*pair).len = 2;
            let dst = (*out).data.add(i);
            (*dst).tag = crate::tagged::TAG_ARRAY;
            (*dst).payload = pair as u64;
        }
    }
    (*out).len = len as u64;
    out
}

/// Decrement the refcount; on reaching zero, release every key + value payload and free.
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
        let is_int = (*map).key_kind == KEY_KIND_INT;
        for i in 0..cap as usize {
            let slot = (*map).slots.add(i);
            if (*slot).hash == 0 {
                continue;
            }
            if !is_int {
                // Release string key.
                lin_string_release((*slot).key as *mut LinString);
            }
            // Int keys are scalar — no release needed.
            crate::tagged::release_tagged_payload_pub(&(*slot).value);
        }
        dealloc((*map).slots as *mut u8, slots_layout(cap));
    }
    let cap_order = (*map).cap_order;
    if cap_order > 0 && !(*map).order.is_null() {
        dealloc((*map).order as *mut u8, order_layout(cap_order));
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

use crate::tagged::{TAG_MAP, TAG_RECORD};

#[no_mangle]
pub unsafe extern "C" fn lin_keys_any(p: *const u8) -> *mut crate::array::LinArray {
    if p.is_null() {
        return crate::array::lin_array_alloc(0);
    }
    let tv = &*(p as *const TaggedVal);
    match tv.tag {
        TAG_MAP => lin_map_keys(tv.payload as *const LinMap),
        TAG_RECORD => {
            let sealed = tv.payload as *mut u8;
            if sealed.is_null() { return crate::array::lin_array_alloc(0); }
            let named_desc = *((sealed.add(16)) as *const *const u8);
            let mat = crate::sealed::materialize_sealed_to_map_pub(sealed, named_desc);
            if mat.is_null() { return crate::array::lin_array_alloc(0); }
            let arr = lin_map_keys(mat as *const LinMap);
            lin_map_release(mat);
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
        TAG_MAP => lin_map_values(tv.payload as *const LinMap),
        TAG_RECORD => {
            let sealed = tv.payload as *mut u8;
            if sealed.is_null() { return crate::array::lin_array_alloc(0); }
            let named_desc = *((sealed.add(16)) as *const *const u8);
            let mat = crate::sealed::materialize_sealed_to_map_pub(sealed, named_desc);
            if mat.is_null() { return crate::array::lin_array_alloc(0); }
            let arr = lin_map_values(mat as *const LinMap);
            lin_map_release(mat);
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
        TAG_MAP => lin_map_entries(tv.payload as *const LinMap),
        TAG_RECORD => {
            let sealed = tv.payload as *mut u8;
            if sealed.is_null() { return crate::array::lin_array_alloc(0); }
            let named_desc = *((sealed.add(16)) as *const *const u8);
            let mat = crate::sealed::materialize_sealed_to_map_pub(sealed, named_desc);
            if mat.is_null() { return crate::array::lin_array_alloc(0); }
            let arr = lin_map_entries(mat as *const LinMap);
            lin_map_release(mat);
            arr
        }
        _ => crate::array::lin_array_alloc(0),
    }
}

/// Coerce a (possibly boxed) value to a raw `LinMap*` for the Json/Object → `{ String: T }`
/// boundary. See original docs — this is for String-keyed maps only.
/// For Int-keyed maps, `{}` is the only allowed literal (handled directly in codegen).
#[no_mangle]
pub unsafe extern "C" fn lin_to_map(p: *const u8) -> *mut LinMap {
    if p.is_null() {
        return lin_map_alloc(0, KEY_KIND_STRING);
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
        _ => lin_map_alloc(0, KEY_KIND_STRING),
    }
}

// ── object.rs-parity ops (LinObject → LinMap migration, Stage 6b deletion) ─────────────────
//
// These mirror the corresponding `lin_object_*` functions so the dynamic-object representation
// can move from `LinObject` onto `LinMap`. Structural equality is order-independent (§5.7);
// merge/copy_except back object spread / rest.

/// Structural, order-independent equality of two maps (mirrors `lin_object_eq`). Maps of
/// different key kinds (String vs Int) are never equal. Values compared via `lin_tagged_eq`.
#[no_mangle]
pub unsafe extern "C" fn lin_map_eq(a: *const LinMap, b: *const LinMap) -> u8 {
    if a == b { return 1; }
    if a.is_null() || b.is_null() { return 0; }
    if (*a).key_kind != (*b).key_kind { return 0; }
    if (*a).len != (*b).len { return 0; }
    let cap = (*a).cap as usize;
    let is_int = (*a).key_kind == KEY_KIND_INT;
    for i in 0..cap {
        let slot = (*a).slots.add(i);
        if (*slot).hash == 0 { continue; }
        let bval = if is_int {
            lin_map_get_int(b, (*slot).key as i64)
        } else {
            lin_map_get(b, (*slot).key as *const LinString)
        };
        if bval.is_null() { return 0; } // key absent in b → unequal
        let av = &(*slot).value as *const TaggedVal as *const u8;
        if crate::tagged::lin_tagged_eq(av, bval as *const u8) == 0 {
            return 0;
        }
    }
    1
}

/// Merge `src` into `dst` (mirrors `lin_object_merge` — object spread). Both maps must share a
/// key kind; a null `src` contributes nothing. Each value is retained by `lin_map_set`.
/// Iterates in insertion order to preserve key ordering semantics.
#[no_mangle]
pub unsafe extern "C-unwind" fn lin_map_merge(dst: *mut LinMap, src: *const LinMap) {
    if src.is_null() || dst.is_null() { return; }
    let len = (*src).len as usize;
    let is_int = (*src).key_kind == KEY_KIND_INT;
    if (*src).order.is_null() || len == 0 { return; }
    for i in 0..len {
        let key = *(*src).order.add(i);
        // Look up the value by key to pass to lin_map_set.
        if is_int {
            let val = lin_map_get_int(src, key as i64);
            let null_tv = TaggedVal { tag: TAG_NULL, _pad: [0; 7], payload: 0 };
            let val_ref = if val.is_null() { &null_tv } else { &*val };
            lin_map_set_int(dst, key as i64, val_ref);
        } else {
            let val = lin_map_get(src, key as *const LinString);
            let null_tv = TaggedVal { tag: TAG_NULL, _pad: [0; 7], payload: 0 };
            let val_ref = if val.is_null() { &null_tv } else { &*val };
            lin_map_set(dst, key as *mut LinString, val_ref);
        }
    }
}

/// Copy every entry of `src` into `dst` except those whose key is in `excluded` (mirrors
/// `lin_object_copy_except` — object rest pattern). String-keyed only (rest is string-keyed).
/// Iterates in insertion order to preserve key ordering semantics.
#[no_mangle]
pub unsafe extern "C" fn lin_map_copy_except(
    dst: *mut LinMap,
    src: *const LinMap,
    excluded: *const *const LinString,
    n_excluded: u32,
) {
    if src.is_null() || dst.is_null() { return; }
    let len = (*src).len as usize;
    if (*src).order.is_null() || len == 0 { return; }
    'outer: for i in 0..len {
        let key = *(*src).order.add(i) as *const LinString;
        for j in 0..n_excluded {
            if string_key_eq(key, *excluded.add(j as usize)) {
                continue 'outer;
            }
        }
        let val = lin_map_get(src, key);
        let null_tv = TaggedVal { tag: TAG_NULL, _pad: [0; 7], payload: 0 };
        let val_ref = if val.is_null() { &null_tv } else { &*val };
        lin_map_set(dst, key as *mut LinString, val_ref);
    }
}

/// Normalize ANY dynamic-object representation (Map/Object/Record/SumNode boxed in a TaggedVal)
/// to a fresh owned `LinMap` (+1). Used by the equality path during the LinObject→LinMap
/// migration so a value produced as a map compares structurally-equal to one still produced as
/// a LinObject (or a kept-packed record/sumnode). Caller releases the result.
pub(crate) unsafe fn dynamic_to_map(tv: *const TaggedVal) -> *mut LinMap {
    if tv.is_null() { return lin_map_alloc(0, KEY_KIND_STRING); }
    match (*tv).tag {
        TAG_MAP => {
            let m = (*tv).payload as *mut LinMap;
            lin_map_retain(m);
            m
        }
        TAG_RECORD => {
            let sealed = (*tv).payload as *mut u8;
            if sealed.is_null() { return lin_map_alloc(0, KEY_KIND_STRING); }
            let named_desc = *((sealed.add(16)) as *const *const u8);
            let m = crate::sealed::materialize_sealed_to_map_pub(sealed, named_desc);
            if m.is_null() { return lin_map_alloc(0, KEY_KIND_STRING); }
            m
        }
        t if t == crate::tagged::TAG_SUMNODE => {
            // lin_sumnode_materialize now returns a *LinMap directly (Phase 3/Cluster B).
            let m = crate::sumnode::lin_sumnode_materialize((*tv).payload as *mut u8);
            if m.is_null() { return lin_map_alloc(0, KEY_KIND_STRING); }
            m as *mut LinMap
        }
        _ => lin_map_alloc(0, KEY_KIND_STRING),
    }
}

/// Public C entry point for `dynamic_to_map`. Used by codegen's `sealed_project_from` to
/// normalise a union/Json boxed source into a fresh owned `LinMap` (+1). Caller releases.
#[no_mangle]
pub unsafe extern "C" fn lin_union_force_to_map(tv: *const u8) -> *mut LinMap {
    dynamic_to_map(tv as *const TaggedVal)
}

/// Cluster D: moved from object.rs. Get or insert a `LinArray` at `key` inside a LinMap.
/// All dynamic objects are TAG_MAP (LinMap*) — no TAG_OBJECT producers remain after Phase 3.
/// Used by the stdlib `groupBy` implementation.
#[no_mangle]
pub unsafe extern "C" fn lin_map_get_or_insert_array(obj: *const u8, key: *const u8) -> *mut u8 {
    use crate::tagged::{TaggedVal, TAG_ARRAY, TAG_MAP, TAG_STR, alloc_tagged};
    use crate::string::LinString;
    if obj.is_null() {
        return alloc_tagged(TAG_ARRAY, crate::array::lin_array_alloc(4) as u64);
    }
    // After Phase 3: dynamic objects are always TAG_MAP.
    if *obj == TAG_MAP {
        let lin_map = (*(obj as *const TaggedVal)).payload as *mut LinMap;
        let key_str = if !key.is_null() && *key == TAG_STR {
            (*(key as *const TaggedVal)).payload as *const LinString
        } else {
            key as *const LinString
        };
        let existing = lin_map_get(lin_map, key_str);
        if !existing.is_null() && (*existing).tag == TAG_ARRAY {
            let arr = (*existing).payload as *mut crate::array::LinArray;
            if !arr.is_null() && (*arr).refcount < crate::string::IMMORTAL_RC {
                (*arr).refcount += 1;
            }
            return alloc_tagged(TAG_ARRAY, arr as u64);
        }
        // Absent (or present-but-not-an-array): create a fresh array and insert it.
        let arr = crate::array::lin_array_alloc(4); // rc = 1
        let val = TaggedVal { tag: TAG_ARRAY, _pad: [0; 7], payload: arr as u64 };
        lin_map_set(lin_map, key_str as *mut LinString, &val);
        // `lin_map_set` retains the inner (arr rc → 2). Drop our construction +1 (rc → 1),
        // then bump once for the returned box's +1 (rc → 2).
        crate::array::lin_array_release(arr);
        if (*arr).refcount < crate::string::IMMORTAL_RC {
            (*arr).refcount += 1;
        }
        return alloc_tagged(TAG_ARRAY, arr as u64);
    }
    // Not a map — return a fresh empty array.
    alloc_tagged(TAG_ARRAY, crate::array::lin_array_alloc(4) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::string::lin_string_alloc;
    use crate::tagged::{TaggedVal, TAG_INT32, TAG_STR, TAG_INT64};

    unsafe fn str_key(s: &str) -> *mut LinString {
        let bytes = s.as_bytes();
        let ptr = lin_string_alloc(bytes.len() as u32);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), (*ptr).data.as_mut_ptr(), bytes.len());
        ptr
    }

    unsafe fn int_val(n: i32) -> TaggedVal {
        TaggedVal { tag: TAG_INT32, _pad: [0; 7], payload: n as u32 as u64 }
    }

    unsafe fn str_tagged_val(s: &str) -> (*mut LinString, TaggedVal) {
        let ptr = str_key(s);
        let tv = TaggedVal { tag: TAG_STR, _pad: [0; 7], payload: ptr as u64 };
        (ptr, tv)
    }

    #[test]
    fn test_string_map_regression() {
        // Existing string map behaviour still works after the slot layout change.
        unsafe {
            let m = lin_map_alloc(0, KEY_KIND_STRING);
            assert_eq!((*m).key_kind, KEY_KIND_STRING);
            for i in 0..200i32 {
                let k = str_key(&format!("k{i}"));
                let v = int_val(i * 10);
                lin_map_set(m, k, &v);
                lin_string_release(k);
            }
            assert_eq!(lin_map_length(m), 200);
            for i in 0..200i32 {
                let k = str_key(&format!("k{i}"));
                let got = lin_map_get(m, k);
                assert!(!got.is_null(), "k{i} missing");
                assert_eq!((*got).tag, TAG_INT32);
                assert_eq!((*got).payload as u32 as i32, i * 10);
                lin_string_release(k);
            }
            // Overwrite.
            let k = str_key("k5");
            let v = int_val(999);
            lin_map_set(m, k, &v);
            lin_string_release(k);
            assert_eq!(lin_map_length(m), 200);
            let k = str_key("k5");
            assert_eq!((*lin_map_get(m, k)).payload as u32 as i32, 999);
            lin_string_release(k);
            // Miss.
            let k = str_key("nope");
            assert!(lin_map_get(m, k).is_null());
            lin_string_release(k);
            lin_map_release(m);
        }
    }

    #[test]
    fn test_int_map_basic() {
        // Int-keyed map: 0, -1, 42, 1_000_000.
        unsafe {
            let m = lin_map_alloc(0, KEY_KIND_INT);
            assert_eq!((*m).key_kind, KEY_KIND_INT);

            // Store key 0 — this is the tricky case (hash must be nonzero).
            let v0 = int_val(100);
            lin_map_set_int(m, 0, &v0);
            // Store -1, 42, 1_000_000.
            let vm1 = int_val(200);
            lin_map_set_int(m, -1, &vm1);
            let v42 = int_val(300);
            lin_map_set_int(m, 42, &v42);
            let v1m = int_val(400);
            lin_map_set_int(m, 1_000_000, &v1m);

            assert_eq!(lin_map_length(m), 4);

            // Hits.
            let r0 = lin_map_get_int(m, 0);
            assert!(!r0.is_null(), "key 0 missing");
            assert_eq!((*r0).payload as u32 as i32, 100);

            let rm1 = lin_map_get_int(m, -1);
            assert!(!rm1.is_null(), "key -1 missing");
            assert_eq!((*rm1).payload as u32 as i32, 200);

            let r42 = lin_map_get_int(m, 42);
            assert!(!r42.is_null(), "key 42 missing");
            assert_eq!((*r42).payload as u32 as i32, 300);

            let r1m = lin_map_get_int(m, 1_000_000);
            assert!(!r1m.is_null(), "key 1_000_000 missing");
            assert_eq!((*r1m).payload as u32 as i32, 400);

            // Miss.
            assert!(lin_map_get_int(m, 7).is_null(), "key 7 should be absent");

            // Overwrite key 42.
            let v42b = int_val(999);
            lin_map_set_int(m, 42, &v42b);
            assert_eq!(lin_map_length(m), 4); // length unchanged
            let r42b = lin_map_get_int(m, 42);
            assert!(!r42b.is_null());
            assert_eq!((*r42b).payload as u32 as i32, 999);

            lin_map_release(m);
        }
    }

    #[test]
    fn test_int_map_keys_returns_int64() {
        unsafe {
            let m = lin_map_alloc(0, KEY_KIND_INT);
            lin_map_set_int(m, 10, &int_val(1));
            lin_map_set_int(m, 20, &int_val(2));
            let keys = lin_map_keys(m);
            assert_eq!((*keys).len, 2);
            // All keys should be TAG_INT64.
            for i in 0..2usize {
                let tv = &*(*keys).data.add(i);
                assert_eq!(tv.tag, TAG_INT64);
            }
            crate::array::lin_array_release(keys);
            lin_map_release(m);
        }
    }

    #[test]
    fn test_int_map_grows() {
        // Force several grows to verify the grow() path with hash==0 occupancy rule.
        unsafe {
            let m = lin_map_alloc(0, KEY_KIND_INT);
            for i in 0..300i64 {
                let v = int_val(i as i32);
                lin_map_set_int(m, i, &v);
            }
            assert_eq!(lin_map_length(m), 300);
            for i in 0..300i64 {
                let r = lin_map_get_int(m, i);
                assert!(!r.is_null(), "key {i} missing after grow");
                assert_eq!((*r).payload as u32 as i32, i as i32);
            }
            lin_map_release(m);
        }
    }

    #[test]
    fn string_values_rc_balanced() {
        unsafe {
            let m = lin_map_alloc(0, KEY_KIND_STRING);
            for i in 0..50i32 {
                let k = str_key(&format!("s{i}"));
                let (sv, v) = str_tagged_val(&format!("val{i}"));
                lin_map_set(m, k, &v);
                lin_string_release(k);
                lin_string_release(sv);
            }
            let keys = lin_map_keys(m);
            let vals = lin_map_values(m);
            let ents = lin_map_entries(m);
            assert_eq!((*keys).len, 50);
            assert_eq!((*vals).len, 50);
            assert_eq!((*ents).len, 50);
            crate::array::lin_array_release(keys);
            crate::array::lin_array_release(vals);
            crate::array::lin_array_release(ents);
            lin_map_release(m);
        }
    }

    #[test]
    fn map_eq_structural_order_independent() {
        unsafe {
            // a and b have the same content, inserted in DIFFERENT order.
            let a = lin_map_alloc(0, KEY_KIND_STRING);
            let b = lin_map_alloc(0, KEY_KIND_STRING);
            for i in 0..30i32 {
                let k = str_key(&format!("k{i}"));
                let v = int_val(i);
                lin_map_set(a, k, &v);
                lin_string_release(k);
            }
            for i in (0..30i32).rev() {
                let k = str_key(&format!("k{i}"));
                let v = int_val(i);
                lin_map_set(b, k, &v);
                lin_string_release(k);
            }
            assert_eq!(lin_map_eq(a, b), 1, "same content diff order should be equal");
            // Mutate one value in b → unequal.
            let k = str_key("k7");
            let v = int_val(999);
            lin_map_set(b, k, &v);
            lin_string_release(k);
            assert_eq!(lin_map_eq(a, b), 0, "differing value should be unequal");
            lin_map_release(a);
            lin_map_release(b);
        }
    }

    #[test]
    fn map_eq_len_and_keykind() {
        unsafe {
            let a = lin_map_alloc(0, KEY_KIND_STRING);
            let b = lin_map_alloc(0, KEY_KIND_STRING);
            let k = str_key("x");
            let v = int_val(1);
            lin_map_set(a, k, &v);
            lin_string_release(k);
            assert_eq!(lin_map_eq(a, b), 0, "different lengths unequal");
            // Different key kind never equal.
            let c = lin_map_alloc(0, KEY_KIND_INT);
            assert_eq!(lin_map_eq(a, c), 0, "string vs int map unequal");
            lin_map_release(a);
            lin_map_release(b);
            lin_map_release(c);
        }
    }

    #[test]
    fn map_merge_overwrites() {
        unsafe {
            let dst = lin_map_alloc(0, KEY_KIND_STRING);
            let src = lin_map_alloc(0, KEY_KIND_STRING);
            let ka = str_key("a"); let va = int_val(1); lin_map_set(dst, ka, &va); lin_string_release(ka);
            let kb = str_key("b"); let vb = int_val(2); lin_map_set(dst, kb, &vb); lin_string_release(kb);
            // src overwrites b, adds c.
            let kb2 = str_key("b"); let vb2 = int_val(20); lin_map_set(src, kb2, &vb2); lin_string_release(kb2);
            let kc = str_key("c"); let vc = int_val(3); lin_map_set(src, kc, &vc); lin_string_release(kc);
            lin_map_merge(dst, src);
            assert_eq!(lin_map_length(dst), 3);
            let kb3 = str_key("b");
            assert_eq!((*lin_map_get(dst, kb3)).payload as u32 as i32, 20, "b overwritten");
            lin_string_release(kb3);
            let kc2 = str_key("c");
            assert_eq!((*lin_map_get(dst, kc2)).payload as u32 as i32, 3, "c added");
            lin_string_release(kc2);
            lin_map_release(dst);
            lin_map_release(src);
        }
    }

    #[test]
    fn map_copy_except_excludes() {
        unsafe {
            let src = lin_map_alloc(0, KEY_KIND_STRING);
            for n in ["a", "b", "c", "d"] {
                let k = str_key(n); let v = int_val(1); lin_map_set(src, k, &v); lin_string_release(k);
            }
            let dst = lin_map_alloc(0, KEY_KIND_STRING);
            let ex_b = str_key("b");
            let ex_d = str_key("d");
            let excluded: [*const LinString; 2] = [ex_b as *const LinString, ex_d as *const LinString];
            lin_map_copy_except(dst, src, excluded.as_ptr(), 2);
            assert_eq!(lin_map_length(dst), 2, "b and d excluded");
            let ka = str_key("a"); assert!(!lin_map_get(dst, ka).is_null()); lin_string_release(ka);
            let kb = str_key("b"); assert!(lin_map_get(dst, kb).is_null(), "b excluded"); lin_string_release(kb);
            lin_string_release(ex_b);
            lin_string_release(ex_d);
            lin_map_release(src);
            lin_map_release(dst);
        }
    }
}
