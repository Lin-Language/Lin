//! `LinMap` — the runtime backing for the typed index-signature object type `{ K: V }`
//! (ADR-055 + numeric-key extension). A hashed dictionary giving O(1) average
//! lookup/insert, in contrast to `LinObject`'s O(n) association-list scan.
//!
//! Two key kinds are supported, selected by `LinMap::key_kind`:
//!   - `KEY_KIND_STRING` (0) — String-keyed: arbitrary string keys. Hash = FNV-1a over bytes.
//!   - `KEY_KIND_INT` (1) — Int-keyed: arbitrary i64 integer keys. Hash = fmix64 mixer.
//!
//! ## SwissTable-style control-byte layout (perf spike)
//!
//! The probe structure uses a **separate contiguous control-byte array** in addition to the
//! parallel slots array. Each slot `i` has a corresponding byte `ctrl[i]`:
//!   - `0x00` — empty slot.
//!   - `0x80 | (hash >> 57)` — occupied slot (7-bit h2 + high bit set).
//!
//! A probe scans `ctrl[]` sequentially:
//!   - `0x00` → empty, stop (miss or insertion point).
//!   - `ctrl[i] == h2` → candidate: load key from `slots[i]` and call `key_eq`.
//!   - anything else → skip (no slot load).
//!
//! Because `ctrl` holds one byte per slot, a single 64-byte cache line covers **64 slots**
//! for the probe scan, vs. 2-3 slots per cache line with the old combined layout. This is
//! the primary cache win: collision chains traverse the ctrl array without touching the slots
//! array, which stays cold until a genuine candidate is found.
//!
//! Slot layout (revised — no hash stored in slot):
//!   key:u64 @ +0, value @ +8.
//! Slot stride = 16B (homogeneous) or 24B (mixed), down from 24B / 32B.
//! This is a secondary win: more slots per data cache line once we DO load the slots array.
//!
//! `grow` recomputes the hash from the key (string: cached `LinString.hash`; int: fast fmix64)
//! so it doesn't need to store the full hash per slot.
//!
//! ## Value-unboxed slots (Wave R memory lever)
//! A slot is NOT a fixed struct — it is a byte region of `slot_stride()` bytes:
//!   key:u64 @ +0, value @ +8.
//! The value region is sized by the map's `value_kind`:
//!   - HOMOGENEOUS (the common case): all values share one tag. `value_kind` records that tag,
//!     the value region is **8 bytes** (the `TaggedVal.payload` only — the tag is implicit), and
//!     the slot is 16 bytes.
//!   - MIXED (`value_kind == VKIND_MIXED`): values have differing tags. The value region is a full
//!     16-byte `TaggedVal`, slot = 24 bytes.
//! `lin_map_get` preserves its borrowed-`*const TaggedVal` ABI: for MIXED it returns the slot's
//! interior `TaggedVal`; for the homogeneous case it reconstructs a `TaggedVal{ tag: value_kind,
//! payload }` into a small per-thread scratch ring and returns a pointer to that (valid until the
//! next several gets on the same thread — every codegen consumer reads tag+payload immediately).
//!
//! **Occupancy rule**: `ctrl[i] == 0x00` means empty. Occupied entries have `ctrl[i] >= 0x80`.
//! The key and value fields in an empty slot are uninitialized.
//!
//! String keys: cast `u64` as `*mut LinString`. RC discipline unchanged.
//! Int keys: raw i64 stored as `u64` via bitcast. No RC on keys.
//!
//! ## LIN_MAP_PROFILE (env-gated, zero overhead when unset)
//! When `LIN_MAP_PROFILE=1`, atomic counters track:
//!   MAP_GETS        — total lin_map_get calls
//!   CTRL_SKIPS      — probe steps skipped by ctrl-byte mismatch (no key load)
//!   KEY_EQ_CALLS    — probe steps where key_eq was called (h2 matched or empty)
//!   KEY_EQ_MISS     — key_eq called + returned false (true hash collision on h2)
//!   H2_FALSE_POS    — ctrl byte matched h2 but key did not match (7-bit false positive)

use std::alloc::{alloc, alloc_zeroed, dealloc, Layout};
use std::cell::{Cell, UnsafeCell};
use std::sync::atomic::{AtomicU64, Ordering};
use crate::string::{LinString, lin_string_inc_ref, lin_string_release, IMMORTAL_RC};
use crate::tagged::{TaggedVal, TAG_NULL};

// Key-kind constants.
pub const KEY_KIND_STRING: u32 = 0;
pub const KEY_KIND_INT: u32 = 1;

// ── Value-kind (the per-map value tag selector) ─────────────────────────────────────────────────
// A real tag is a small u8 (widened to u32). Two sentinels live above the u8 range:
/// No value inserted yet — slots not yet allocated, value width unknown.
const VKIND_UNINIT: u32 = u32::MAX;
/// Heterogeneous values — slots store full 16-byte `TaggedVal`s (the old layout).
const VKIND_MIXED: u32 = u32::MAX - 1;

// Slot byte offsets (SwissTable layout — no hash in slot).
const SLOT_KEY_OFF: usize = 0;
const SLOT_VAL_OFF: usize = 8;

// Control-byte sentinels.
const CTRL_EMPTY: u8 = 0x00;

/// Width of the value region (bytes) for a given `value_kind`.
#[inline(always)]
fn value_bytes(vk: u32) -> usize {
    if vk == VKIND_MIXED { 16 } else { 8 }
}

/// Stride (bytes) between consecutive slots for a given `value_kind`.
/// Always a multiple of 8, so every slot's key/value words stay 8-aligned.
#[inline(always)]
fn slot_stride(vk: u32) -> usize {
    SLOT_VAL_OFF + value_bytes(vk)
}

// ── Per-thread scratch ring for the homogeneous `lin_map_get` reconstruction ─────────────────────
thread_local! {
    static GET_SCRATCH: UnsafeCell<[TaggedVal; 8]> = const {
        UnsafeCell::new([TaggedVal { tag: TAG_NULL, _pad: [0; 7], payload: 0 }; 8])
    };
    static GET_SCRATCH_IDX: Cell<usize> = const { Cell::new(0) };
}

// ── Profiling counters (env-gated, zero cost when disabled) ─────────────────────────────────
use std::sync::atomic::AtomicU8;
static MAP_PROFILE_STATE: AtomicU8 = AtomicU8::new(0);
static MAP_GETS: AtomicU64 = AtomicU64::new(0);
static MAP_CTRL_SKIPS: AtomicU64 = AtomicU64::new(0);
static MAP_KEY_EQ_CALLS: AtomicU64 = AtomicU64::new(0);
static MAP_KEY_EQ_MISS: AtomicU64 = AtomicU64::new(0);
static MAP_H2_FALSE_POS: AtomicU64 = AtomicU64::new(0);

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
        let skips = MAP_CTRL_SKIPS.load(Ordering::Relaxed);
        let eq_calls = MAP_KEY_EQ_CALLS.load(Ordering::Relaxed);
        let eq_miss = MAP_KEY_EQ_MISS.load(Ordering::Relaxed);
        let h2_fp = MAP_H2_FALSE_POS.load(Ordering::Relaxed);
        let total_probes = skips + eq_calls;
        let probes_per_get = if gets > 0 { total_probes as f64 / gets as f64 } else { 0.0 };
        eprintln!(
            "MAP_PROFILE: gets={gets} total_probe_steps={total_probes} probes/get={probes_per_get:.2} \
             ctrl_skips={skips} ({:.1}%) key_eq_calls={eq_calls} key_eq_miss={eq_miss} h2_false_pos={h2_fp}",
            if total_probes > 0 { skips as f64 / total_probes as f64 * 100.0 } else { 0.0 },
        );
    }
}

/// A hashed map with SwissTable-style control-byte layout.
///
/// `ctrl` is a `cap`-byte array where `ctrl[i] == 0x00` means slot `i` is empty and
/// `ctrl[i] == 0x80 | (hash >> 57)` means slot `i` is occupied. `slots` is a parallel
/// array holding `(key:u64, value:...)` per slot (no hash — see slot offsets above).
///
/// `key_kind` selects the key type: KEY_KIND_STRING (0) or KEY_KIND_INT (1).
/// `value_kind` selects the value width (a tag, or VKIND_UNINIT / VKIND_MIXED).
/// `order` tracks insertion order (raw key bits).
///
/// **Struct field offsets are LOCKED** — the codegen (`inline_map_get_str` in index.rs)
/// accesses `len` @ +4, `cap` @ +8, `slots` @ +16, and `value_kind` @ +36 directly via GEP.
/// Do NOT reorder existing fields without updating those GEP constants.
/// `ctrl` lives at +40 (after `value_kind`) preserving all existing codegen-visible offsets.
#[repr(C)]
pub struct LinMap {
    pub refcount: u32,   // +0
    pub len: u32,        // +4   number of occupied slots
    pub cap: u32,        // +8   table size (power of two)
    pub key_kind: u32,   // +12  KEY_KIND_STRING (0) or KEY_KIND_INT (1)
    pub slots: *mut u8,  // +16  `cap` slots of `slot_stride(value_kind)` bytes; null before first insert
    pub order: *mut u64, // +24  insertion-order key list, length = len, capacity = cap_order
    pub cap_order: u32,  // +32  allocated capacity of `order` array
    pub value_kind: u32, // +36  VKIND_UNINIT / VKIND_MIXED / a real value tag  ← codegen-visible
    pub ctrl: *mut u8,   // +40  `cap` control bytes (SwissTable); null before first insert
}

const INITIAL_CAP: u32 = 8; // must be power of two; bumped from 4 for SwissTable probe efficiency
#[inline]
fn over_load(len: u32, cap: u32) -> bool {
    // 7/8 load factor (slightly denser than before since probe is now cache-cheap)
    (len as u64) * 8 >= (cap as u64) * 7
}

// ── Raw slot accessors (byte-offset, stride-aware, no hash) ─────────────────────────────────────

#[inline(always)]
unsafe fn slot_at(base: *mut u8, idx: usize, stride: usize) -> *mut u8 {
    base.add(idx * stride)
}
#[inline(always)]
unsafe fn slot_key(s: *mut u8) -> u64 {
    *(s.add(SLOT_KEY_OFF) as *const u64)
}
#[inline(always)]
unsafe fn set_slot_key(s: *mut u8, k: u64) {
    *(s.add(SLOT_KEY_OFF) as *mut u64) = k;
}

/// Reconstruct an owned `TaggedVal` from a slot's value region (a by-value copy; no RC change).
#[inline(always)]
unsafe fn slot_value_owned(s: *mut u8, vk: u32) -> TaggedVal {
    if vk == VKIND_MIXED {
        std::ptr::read(s.add(SLOT_VAL_OFF) as *const TaggedVal)
    } else {
        TaggedVal { tag: vk as u8, _pad: [0; 7], payload: *(s.add(SLOT_VAL_OFF) as *const u64) }
    }
}

/// Write a value into a slot's value region (value bits only; caller handles RC).
#[inline(always)]
unsafe fn store_slot_value(s: *mut u8, vk: u32, v: &TaggedVal) {
    if vk == VKIND_MIXED {
        std::ptr::write(s.add(SLOT_VAL_OFF) as *mut TaggedVal,
            TaggedVal { tag: v.tag, _pad: [0; 7], payload: v.payload });
    } else {
        *(s.add(SLOT_VAL_OFF) as *mut u64) = v.payload;
    }
}

/// Return a borrowed `*const TaggedVal` for a slot's value (the `lin_map_get` ABI).
#[inline]
unsafe fn slot_value_ptr(vk: u32, s: *mut u8) -> *const TaggedVal {
    if vk == VKIND_MIXED {
        s.add(SLOT_VAL_OFF) as *const TaggedVal
    } else {
        let payload = *(s.add(SLOT_VAL_OFF) as *const u64);
        let tag = vk as u8;
        GET_SCRATCH.with(|ring| {
            let idx = GET_SCRATCH_IDX.with(|c| {
                let n = (c.get() + 1) & 7;
                c.set(n);
                n
            });
            let p = (*ring.get()).as_mut_ptr().add(idx);
            (*p).tag = tag;
            (*p).payload = payload;
            p as *const TaggedVal
        })
    }
}

/// Iterate every occupied slot as `(key_bits, owned TaggedVal)`.
pub(crate) unsafe fn map_for_each_slot(map: *const LinMap, mut f: impl FnMut(u64, TaggedVal)) {
    if map.is_null() || (*map).ctrl.is_null() {
        return;
    }
    let cap = (*map).cap as usize;
    let vk = (*map).value_kind;
    let stride = slot_stride(vk);
    let ctrl = (*map).ctrl;
    let base = (*map).slots;
    for i in 0..cap {
        if *ctrl.add(i) == CTRL_EMPTY {
            continue;
        }
        let s = slot_at(base, i, stride);
        f(slot_key(s), slot_value_owned(s, vk));
    }
}

unsafe fn map_header_layout() -> Layout {
    Layout::from_size_align_unchecked(
        std::mem::size_of::<LinMap>(),
        std::mem::align_of::<LinMap>(),
    )
}

unsafe fn slots_layout(cap: u32, stride: usize) -> Layout {
    Layout::from_size_align_unchecked(stride * cap as usize, 8)
}

/// Allocate a zeroed slot table of `cap` slots at `stride` bytes each.
unsafe fn alloc_slots(cap: u32, stride: usize) -> *mut u8 {
    alloc_zeroed(slots_layout(cap, stride))
}

unsafe fn ctrl_layout(cap: u32) -> Layout {
    Layout::from_size_align_unchecked(cap as usize, 1)
}

/// Allocate a zeroed control-byte array (all CTRL_EMPTY = 0x00).
unsafe fn alloc_ctrl(cap: u32) -> *mut u8 {
    alloc_zeroed(ctrl_layout(cap))
}

unsafe fn order_layout(cap: u32) -> Layout {
    Layout::from_size_align_unchecked(
        std::mem::size_of::<u64>() * cap as usize,
        std::mem::align_of::<u64>(),
    )
}

unsafe fn alloc_order(cap: u32) -> *mut u64 {
    alloc(order_layout(cap)) as *mut u64
}

unsafe fn order_push(map: *mut LinMap, key: u64) {
    let len = (*map).len;
    if len >= (*map).cap_order {
        let old_cap = (*map).cap_order;
        let new_cap = if old_cap == 0 { INITIAL_CAP } else { old_cap * 2 };
        let new_order = alloc_order(new_cap);
        std::ptr::copy_nonoverlapping((*map).order, new_order, len as usize);
        dealloc((*map).order as *mut u8, order_layout(old_cap));
        (*map).order = new_order;
        (*map).cap_order = new_cap;
    }
    *(*map).order.add(len as usize) = key;
}

// ── Hash functions ───────────────────────────────────────────────────────────────────────────

#[inline]
fn fnv1a_bytes(bytes: &[u8]) -> u64 {
    crate::string::fnv1a_bytes_str(bytes)
}

/// FNV-1a hash for String keys. Uses `LinString.hash` cache when available.
#[inline]
unsafe fn hash_string_key(key: *const LinString) -> u64 {
    if key.is_null() {
        return 1;
    }
    let h = (*key).hash;
    if h != 0 {
        return h;
    }
    (*key).get_or_init_hash()
}

/// Murmurhash3 finalizer (fmix64) for Int keys. Guarantees nonzero output.
#[inline]
fn fmix64(mut k: u64) -> u64 {
    k ^= k >> 33;
    k = k.wrapping_mul(0xff51afd7ed558ccd);
    k ^= k >> 33;
    k = k.wrapping_mul(0xc4ceb9fe1a85ec53);
    k ^= k >> 33;
    if k == 0 { 1 } else { k }
}

/// Compute the h2 control byte for a given full hash.
/// Occupied: high bit set + 7 bits of hash. Never equals CTRL_EMPTY (0x00).
#[inline(always)]
fn h2_of(hash: u64) -> u8 {
    0x80 | ((hash >> 57) as u8)
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
    let ha = (*a).hash;
    let hb = (*b).hash;
    if ha != 0 && hb != 0 && ha != hb {
        return false;
    }
    let aa = std::slice::from_raw_parts((*a).data.as_ptr(), al as usize);
    let bb = std::slice::from_raw_parts((*b).data.as_ptr(), bl as usize);
    aa == bb
}

// ── Slot finder (String kind) — SwissTable ctrl-byte probe ──────────────────────────────────

/// Find the slot index for `key` (String map). Returns the matching slot index or the first
/// empty slot (ctrl == 0x00). Scans `ctrl[]` for h2 match or CTRL_EMPTY; loads the slot only
/// on a ctrl-byte hit — drastically fewer data cache misses on large tables.
#[inline]
unsafe fn find_slot_string(map: *const LinMap, key: *const LinString, khash: u64) -> usize {
    let cap = (*map).cap as usize;
    let mask = cap - 1;
    let h2 = h2_of(khash);
    let ctrl = (*map).ctrl;
    let stride = slot_stride((*map).value_kind);
    let base = (*map).slots;
    let mut idx = (khash as usize) & mask;
    for _ in 0..cap {
        let c = *ctrl.add(idx);
        if c == CTRL_EMPTY {
            return idx;
        }
        if c == h2 {
            let slot = slot_at(base, idx, stride);
            if string_key_eq(slot_key(slot) as *const LinString, key) {
                return idx;
            }
        }
        idx = (idx + 1) & mask;
    }
    idx
}

#[cold]
unsafe fn find_slot_string_profiled(map: *const LinMap, key: *const LinString, khash: u64) -> usize {
    let cap = (*map).cap as usize;
    let mask = cap - 1;
    let h2 = h2_of(khash);
    let ctrl = (*map).ctrl;
    let stride = slot_stride((*map).value_kind);
    let base = (*map).slots;
    let mut idx = (khash as usize) & mask;
    for _ in 0..cap {
        let c = *ctrl.add(idx);
        if c == CTRL_EMPTY {
            MAP_KEY_EQ_CALLS.fetch_add(1, Ordering::Relaxed);
            return idx;
        }
        if c != h2 {
            MAP_CTRL_SKIPS.fetch_add(1, Ordering::Relaxed);
            idx = (idx + 1) & mask;
            continue;
        }
        // ctrl byte matches h2 — load slot and compare key
        MAP_KEY_EQ_CALLS.fetch_add(1, Ordering::Relaxed);
        let slot = slot_at(base, idx, stride);
        if string_key_eq(slot_key(slot) as *const LinString, key) {
            return idx;
        }
        MAP_KEY_EQ_MISS.fetch_add(1, Ordering::Relaxed);
        MAP_H2_FALSE_POS.fetch_add(1, Ordering::Relaxed);
        idx = (idx + 1) & mask;
    }
    idx
}

// ── Slot finder (Int kind) ────────────────────────────────────────────────────────────────────

/// Find the slot index for an Int key (raw i64 as u64). Probes via ctrl bytes first.
#[inline]
unsafe fn find_slot_int(map: *const LinMap, key_bits: u64, khash: u64) -> usize {
    let cap = (*map).cap as usize;
    let mask = cap - 1;
    let h2 = h2_of(khash);
    let ctrl = (*map).ctrl;
    let stride = slot_stride((*map).value_kind);
    let base = (*map).slots;
    let mut idx = (khash as usize) & mask;
    for _ in 0..cap {
        let c = *ctrl.add(idx);
        if c == CTRL_EMPTY {
            return idx;
        }
        if c == h2 {
            let slot = slot_at(base, idx, stride);
            if slot_key(slot) == key_bits {
                return idx;
            }
        }
        idx = (idx + 1) & mask;
    }
    idx
}

// ── grow / mixed-upgrade ───────────────────────────────────────────────────────────────────────

/// Double the table size and re-insert all live entries.
/// Recomputes the hash from the key (string: cached hash; int: fmix64) — no hash stored in slot.
unsafe fn grow(map: *mut LinMap) {
    let old_cap = (*map).cap;
    let old_ctrl = (*map).ctrl;
    let old_slots = (*map).slots;
    let vk = (*map).value_kind;
    let stride = slot_stride(vk);
    let val_bytes = value_bytes(vk);
    let new_cap = if old_cap == 0 { INITIAL_CAP } else { old_cap * 2 };
    let new_ctrl = alloc_ctrl(new_cap);
    let new_slots = alloc_slots(new_cap, stride);
    (*map).ctrl = new_ctrl;
    (*map).slots = new_slots;
    (*map).cap = new_cap;
    let mask = (new_cap - 1) as usize;
    let is_int = (*map).key_kind == KEY_KIND_INT;
    for i in 0..old_cap as usize {
        if *old_ctrl.add(i) == CTRL_EMPTY {
            continue;
        }
        let src = slot_at(old_slots, i, stride);
        let key = slot_key(src);
        // Recompute hash from key (strings have it cached; ints are a fast mixer).
        let h = if is_int { fmix64(key) } else { hash_string_key(key as *const LinString) };
        let h2 = h2_of(h);
        let mut dst_idx = (h as usize) & mask;
        loop {
            if *new_ctrl.add(dst_idx) == CTRL_EMPTY {
                *new_ctrl.add(dst_idx) = h2;
                let dst = slot_at(new_slots, dst_idx, stride);
                set_slot_key(dst, key);
                std::ptr::copy_nonoverlapping(
                    src.add(SLOT_VAL_OFF), dst.add(SLOT_VAL_OFF), val_bytes);
                break;
            }
            dst_idx = (dst_idx + 1) & mask;
        }
    }
    if old_cap > 0 {
        if !old_ctrl.is_null() {
            dealloc(old_ctrl, ctrl_layout(old_cap));
        }
        if !old_slots.is_null() {
            dealloc(old_slots, slots_layout(old_cap, stride));
        }
    }
}

/// Upgrade a homogeneous map to MIXED: widen every slot's value region from 8 to 16 bytes.
/// Slot positions and ctrl bytes are preserved (same h2 → same probe path); only layout widens.
unsafe fn convert_to_mixed(map: *mut LinMap) {
    let old_vk = (*map).value_kind;
    if old_vk == VKIND_MIXED {
        return;
    }
    if (*map).ctrl.is_null() {
        // Nothing allocated yet — just record MIXED; the first alloc will use the wide stride.
        (*map).value_kind = VKIND_MIXED;
        return;
    }
    let cap = (*map).cap;
    let old_stride = slot_stride(old_vk);
    let new_stride = slot_stride(VKIND_MIXED);
    let old_slots = (*map).slots;
    let new_slots = alloc_slots(cap, new_stride);
    let ctrl = (*map).ctrl;
    for i in 0..cap as usize {
        if *ctrl.add(i) == CTRL_EMPTY {
            continue;
        }
        let src = slot_at(old_slots, i, old_stride);
        let dst = slot_at(new_slots, i, new_stride);
        set_slot_key(dst, slot_key(src));
        // Expand the 8-byte payload into a full TaggedVal with the old (homogeneous) tag.
        let payload = *(src.add(SLOT_VAL_OFF) as *const u64);
        std::ptr::write(dst.add(SLOT_VAL_OFF) as *mut TaggedVal,
            TaggedVal { tag: old_vk as u8, _pad: [0; 7], payload });
    }
    dealloc(old_slots, slots_layout(cap, old_stride));
    (*map).slots = new_slots;
    (*map).value_kind = VKIND_MIXED;
    // ctrl is unchanged — occupancy is the same.
}

// ── Value-kind diagnostics (env-gated LIN_VKIND_STATS=1, zero cost when off) ─────────────────────
static VKIND_STATS_ON: AtomicU8 = AtomicU8::new(0); // 0=uninit 1=off 2=on
static VK_FIRST: [AtomicU64; 40] = {
    const Z: AtomicU64 = AtomicU64::new(0);
    [Z; 40]
};
static VK_MIXED_CONV: AtomicU64 = AtomicU64::new(0);
static VK_SLOT_BYTES_HOMO: AtomicU64 = AtomicU64::new(0);

#[cold]
fn vkind_stats_on() -> bool {
    let s = VKIND_STATS_ON.load(Ordering::Relaxed);
    if s != 0 { return s == 2; }
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let on = std::env::var("LIN_VKIND_STATS").as_deref() == Ok("1");
        VKIND_STATS_ON.store(if on { 2 } else { 1 }, Ordering::SeqCst);
        if on { unsafe { libc::atexit(vkind_stats_atexit); } }
    });
    VKIND_STATS_ON.load(Ordering::SeqCst) == 2
}
extern "C" fn vkind_stats_atexit() {
    let mixed = VK_MIXED_CONV.load(Ordering::Relaxed);
    let mut total = 0u64;
    eprint!("VKIND_STATS: first-insert tag histogram: ");
    for (t, c) in VK_FIRST.iter().enumerate() {
        let n = c.load(Ordering::Relaxed);
        if n > 0 { eprint!("tag{t}={n} "); total += n; }
    }
    eprintln!("\nVKIND_STATS: total_maps_first_inserted={total} mixed_conversions={mixed} \
        homogeneous={}", total.saturating_sub(mixed));
}

/// Establish / upgrade `value_kind` for an incoming value tag, converting to MIXED on a mismatch.
#[inline]
unsafe fn note_value_tag(map: *mut LinMap, vtag: u8) {
    let vk = (*map).value_kind;
    if vk == VKIND_UNINIT {
        (*map).value_kind = vtag as u32;
        if vkind_stats_on() {
            VK_FIRST[(vtag as usize).min(39)].fetch_add(1, Ordering::Relaxed);
        }
    } else if vk != VKIND_MIXED && vk != vtag as u32 {
        if vkind_stats_on() {
            VK_MIXED_CONV.fetch_add(1, Ordering::Relaxed);
        }
        convert_to_mixed(map);
    }
    let _ = &VK_SLOT_BYTES_HOMO;
}

/// Ensure `ctrl` + `slots` are allocated and have room for one more entry.
#[inline]
unsafe fn ensure_capacity(map: *mut LinMap) {
    if (*map).ctrl.is_null() {
        // First insert: allocate ctrl + slots.
        let stride = slot_stride((*map).value_kind);
        (*map).ctrl = alloc_ctrl((*map).cap);
        (*map).slots = alloc_slots((*map).cap, stride);
    } else if over_load((*map).len + 1, (*map).cap) {
        grow(map);
    }
}

// ── Public API ───────────────────────────────────────────────────────────────────────────────

/// Allocate a new `LinMap` with the given `key_kind`.
/// `hint` is the expected initial capacity; sized to the next power-of-two >= max(hint, INITIAL_CAP).
/// Ctrl + slot bytes are allocated lazily on the first insert (once value width is known).
#[no_mangle]
pub unsafe extern "C" fn lin_map_alloc(hint: u32, key_kind: u32) -> *mut LinMap {
    let cap = hint.next_power_of_two().max(INITIAL_CAP);
    let ptr = alloc(map_header_layout()) as *mut LinMap;
    (*ptr).refcount = 1;
    (*ptr).len = 0;
    (*ptr).cap = cap;
    (*ptr).key_kind = key_kind;
    (*ptr).slots = std::ptr::null_mut();
    (*ptr).ctrl = std::ptr::null_mut();
    (*ptr).order = alloc_order(cap);
    (*ptr).cap_order = cap;
    (*ptr).value_kind = VKIND_UNINIT;
    ptr
}

/// Allocate a `LinMap` pre-committed to MIXED value layout (full 16-byte `TaggedVal` slots).
pub(crate) unsafe fn lin_map_alloc_mixed(hint: u32, key_kind: u32) -> *mut LinMap {
    let m = lin_map_alloc(hint, key_kind);
    (*m).value_kind = VKIND_MIXED;
    m
}

/// Insert / overwrite `key -> *val` (String map).
#[no_mangle]
pub unsafe extern "C" fn lin_map_set(map: *mut LinMap, key: *mut LinString, val: *const TaggedVal) {
    if map.is_null() { return; }
    let null_tv = TaggedVal { tag: TAG_NULL, _pad: [0; 7], payload: 0 };
    let val_ref: &TaggedVal = if val.is_null() { &null_tv } else { &*val };
    note_value_tag(map, val_ref.tag);
    ensure_capacity(map);
    let vk = (*map).value_kind;
    let stride = slot_stride(vk);
    let khash = hash_string_key(key);
    let h2 = h2_of(khash);
    let idx = find_slot_string(map, key, khash);
    let ctrl_byte = (*map).ctrl.add(idx);
    let slot = slot_at((*map).slots, idx, stride);
    if *ctrl_byte == CTRL_EMPTY {
        // Fresh insert.
        order_push(map, key as u64);
        *ctrl_byte = h2;
        lin_string_inc_ref(key);
        set_slot_key(slot, key as u64);
        store_slot_value(slot, vk, val_ref);
        crate::tagged::retain_tagged_payload_pub(val_ref);
        (*map).len += 1;
    } else {
        // Overwrite — release old value.
        let old = slot_value_owned(slot, vk);
        crate::tagged::release_tagged_payload_pub(&old);
        store_slot_value(slot, vk, val_ref);
        crate::tagged::retain_tagged_payload_pub(val_ref);
    }
}

/// Look up `key` (String map). Returns borrowed pointer or null if absent.
#[no_mangle]
pub unsafe extern "C" fn lin_map_get(map: *const LinMap, key: *const LinString) -> *const TaggedVal {
    if map.is_null() || (*map).ctrl.is_null() || (*map).len == 0 {
        return std::ptr::null();
    }
    let khash = hash_string_key(key);
    let idx = if map_profile_state() == 2 {
        MAP_GETS.fetch_add(1, Ordering::Relaxed);
        find_slot_string_profiled(map, key, khash)
    } else {
        find_slot_string(map, key, khash)
    };
    let vk = (*map).value_kind;
    let ctrl_byte = *(*map).ctrl.add(idx);
    if ctrl_byte == CTRL_EMPTY {
        std::ptr::null()
    } else {
        slot_value_ptr(vk, slot_at((*map).slots, idx, slot_stride(vk)))
    }
}

/// Insert / overwrite `key -> *val` (Int map).
#[no_mangle]
pub unsafe extern "C" fn lin_map_set_int(map: *mut LinMap, key: i64, val: *const TaggedVal) {
    if map.is_null() { return; }
    let null_tv = TaggedVal { tag: TAG_NULL, _pad: [0; 7], payload: 0 };
    let val_ref: &TaggedVal = if val.is_null() { &null_tv } else { &*val };
    note_value_tag(map, val_ref.tag);
    ensure_capacity(map);
    let vk = (*map).value_kind;
    let stride = slot_stride(vk);
    let key_bits = key as u64;
    let khash = fmix64(key_bits);
    let h2 = h2_of(khash);
    let idx = find_slot_int(map, key_bits, khash);
    let ctrl_byte = (*map).ctrl.add(idx);
    let slot = slot_at((*map).slots, idx, stride);
    if *ctrl_byte == CTRL_EMPTY {
        order_push(map, key_bits);
        *ctrl_byte = h2;
        set_slot_key(slot, key_bits);
        store_slot_value(slot, vk, val_ref);
        crate::tagged::retain_tagged_payload_pub(val_ref);
        (*map).len += 1;
    } else {
        let old = slot_value_owned(slot, vk);
        crate::tagged::release_tagged_payload_pub(&old);
        store_slot_value(slot, vk, val_ref);
        crate::tagged::retain_tagged_payload_pub(val_ref);
    }
}

/// Look up `key` (Int map). Returns borrowed pointer or null if absent.
#[no_mangle]
pub unsafe extern "C" fn lin_map_get_int(map: *const LinMap, key: i64) -> *const TaggedVal {
    if map.is_null() || (*map).ctrl.is_null() || (*map).len == 0 {
        return std::ptr::null();
    }
    let key_bits = key as u64;
    let khash = fmix64(key_bits);
    let idx = find_slot_int(map, key_bits, khash);
    let vk = (*map).value_kind;
    let ctrl_byte = *(*map).ctrl.add(idx);
    if ctrl_byte == CTRL_EMPTY {
        std::ptr::null()
    } else {
        slot_value_ptr(vk, slot_at((*map).slots, idx, slot_stride(vk)))
    }
}

/// Lookup by raw UTF-8 key bytes — avoids allocating a temporary `LinString`.
pub(crate) unsafe fn lin_map_get_bytes(
    map: *const LinMap,
    key_ptr: *const u8,
    key_len: u32,
) -> *const TaggedVal {
    if map.is_null() || (*map).ctrl.is_null() || (*map).len == 0 {
        return std::ptr::null();
    }
    let bytes = std::slice::from_raw_parts(key_ptr, key_len as usize);
    let h = fnv1a_bytes(bytes);
    let h2 = h2_of(h);
    let cap = (*map).cap as usize;
    let mask = cap - 1;
    let vk = (*map).value_kind;
    let stride = slot_stride(vk);
    let ctrl = (*map).ctrl;
    let base = (*map).slots;
    let mut idx = (h as usize) & mask;
    for _ in 0..cap {
        let c = *ctrl.add(idx);
        if c == CTRL_EMPTY {
            return std::ptr::null();
        }
        if c == h2 {
            let slot = slot_at(base, idx, stride);
            let slot_key_ptr = slot_key(slot) as *const LinString;
            let kl = (*slot_key_ptr).len as usize;
            if kl == bytes.len() {
                let kb = std::slice::from_raw_parts((*slot_key_ptr).data.as_ptr(), kl);
                if kb == bytes {
                    return slot_value_ptr(vk, slot);
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

/// Return a `LinArray*` of all values in insertion order.
#[no_mangle]
pub unsafe extern "C" fn lin_map_values(map: *const LinMap) -> *mut crate::array::LinArray {
    let len = if map.is_null() { 0 } else { (*map).len };
    let arr = crate::array::lin_array_alloc(len as u64);
    if !map.is_null() && len > 0 && !(*map).order.is_null() {
        let is_int = (*map).key_kind == KEY_KIND_INT;
        for i in 0..len as usize {
            let key = *(*map).order.add(i);
            let v = if is_int {
                let p = lin_map_get_int(map, key as i64);
                if p.is_null() { TaggedVal { tag: TAG_NULL, _pad: [0; 7], payload: 0 } } else { *p }
            } else {
                let p = lin_map_get(map, key as *const LinString);
                if p.is_null() { TaggedVal { tag: TAG_NULL, _pad: [0; 7], payload: 0 } } else { *p }
            };
            let dst = (*arr).data.add(i) as *mut TaggedVal;
            (*dst).tag = v.tag;
            (*dst).payload = v.payload;
            crate::tagged::retain_tagged_payload_pub(&v);
        }
    }
    (*arr).len = len as u64;
    arr
}

/// Return a `LinArray*` of `[key, value]` pair arrays in insertion order.
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
            let v = if is_int {
                let p = lin_map_get_int(map, key as i64);
                if p.is_null() { TaggedVal { tag: TAG_NULL, _pad: [0; 7], payload: 0 } } else { *p }
            } else {
                let p = lin_map_get(map, key as *const LinString);
                if p.is_null() { TaggedVal { tag: TAG_NULL, _pad: [0; 7], payload: 0 } } else { *p }
            };
            let val_dst = (*pair).data.add(1) as *mut TaggedVal;
            (*val_dst).tag = v.tag;
            (*val_dst).payload = v.payload;
            crate::tagged::retain_tagged_payload_pub(&v);
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
    if cap > 0 && !(*map).ctrl.is_null() {
        let is_int = (*map).key_kind == KEY_KIND_INT;
        let vk = (*map).value_kind;
        let stride = slot_stride(vk);
        let ctrl = (*map).ctrl;
        let base = (*map).slots;
        for i in 0..cap as usize {
            if *ctrl.add(i) == CTRL_EMPTY {
                continue;
            }
            let slot = slot_at(base, i, stride);
            if !is_int {
                lin_string_release(slot_key(slot) as *mut LinString);
            }
            let v = slot_value_owned(slot, vk);
            crate::tagged::release_tagged_payload_pub(&v);
        }
        dealloc(ctrl, ctrl_layout(cap));
        if !base.is_null() {
            dealloc(base, slots_layout(cap, stride));
        }
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

unsafe fn stringify_int_key_slot(dst: *mut crate::array::LinArrayElem) {
    let dst = dst as *mut TaggedVal;
    if (*dst).tag == crate::tagged::TAG_INT64 {
        let n = (*dst).payload as i64;
        let s = n.to_string();
        let ls = crate::string::lin_string_from_bytes(s.as_ptr(), s.len() as u32);
        (*dst).tag = crate::tagged::TAG_STR;
        (*dst).payload = ls as u64;
    }
}

unsafe fn stringify_int_keys(arr: *mut crate::array::LinArray) {
    if arr.is_null() { return; }
    let len = (*arr).len as usize;
    for i in 0..len {
        stringify_int_key_slot((*arr).data.add(i));
    }
}

unsafe fn stringify_int_entry_keys(arr: *mut crate::array::LinArray) {
    if arr.is_null() { return; }
    let len = (*arr).len as usize;
    for i in 0..len {
        let elem = (*arr).data.add(i);
        if (*elem).tag == crate::tagged::TAG_ARRAY {
            let pair = (*elem).payload as *mut crate::array::LinArray;
            if !pair.is_null() && (*pair).len >= 1 {
                stringify_int_key_slot((*pair).data.add(0));
            }
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_keys_any(p: *const u8) -> *mut crate::array::LinArray {
    if p.is_null() {
        return crate::array::lin_array_alloc(0);
    }
    let tv = &*(p as *const TaggedVal);
    match tv.tag {
        TAG_MAP => {
            let arr = lin_map_keys(tv.payload as *const LinMap);
            stringify_int_keys(arr);
            arr
        }
        TAG_RECORD => {
            let sealed = tv.payload as *mut u8;
            if sealed.is_null() { return crate::array::lin_array_alloc(0); }
            let named_desc = *((sealed.add(16)) as *const *const u8);
            if named_desc.is_null() { return crate::array::lin_array_alloc(0); }
            let field_count = u32::from_le_bytes([
                *named_desc, *named_desc.add(1), *named_desc.add(2), *named_desc.add(3),
            ]) as u64;
            let arr = crate::array::lin_array_alloc(field_count);
            (*arr).len = field_count;
            let mut i = 0usize;
            crate::sealed::record_walk_fields(named_desc, |name_bytes, _offset, _nkind, _nested| {
                let ls = crate::string::lin_string_from_bytes(name_bytes.as_ptr(), name_bytes.len() as u32);
                let dst = (*arr).data.add(i) as *mut TaggedVal;
                (*dst).tag = crate::tagged::TAG_STR;
                (*dst).payload = ls as u64;
                i += 1;
            });
            arr
        }
        _ => crate::array::lin_array_alloc(0),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_keys_flat(map: *const LinMap, elem_tag: u8) -> *mut crate::array::LinArray {
    let len: u64 = if map.is_null() { 0 } else { (*map).len as u64 };
    if len == 0 || (*map).order.is_null() {
        return crate::array::lin_flat_array_from_i64_keys(std::ptr::null(), 0, elem_tag);
    }
    crate::array::lin_flat_array_from_i64_keys((*map).order as *const i64, len, elem_tag)
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
            if named_desc.is_null() { return crate::array::lin_array_alloc(0); }
            let field_count = u32::from_le_bytes([
                *named_desc, *named_desc.add(1), *named_desc.add(2), *named_desc.add(3),
            ]) as u64;
            let arr = crate::array::lin_array_alloc(field_count);
            (*arr).len = field_count;
            let mut i = 0usize;
            crate::sealed::record_walk_fields(named_desc, |_name_bytes, offset, nkind, nested| {
                let boxed = crate::sealed::box_field_value(sealed, offset, nkind, nested);
                let dst = (*arr).data.add(i) as *mut TaggedVal;
                if boxed.is_null() {
                    (*dst).tag = crate::tagged::TAG_NULL;
                    (*dst).payload = 0;
                } else {
                    let tv_box = boxed as *const TaggedVal;
                    (*dst).tag = (*tv_box).tag;
                    (*dst).payload = (*tv_box).payload;
                    // Retain: the box's payload is +1; we copy tag+payload into the array slot
                    // (which owns a +1 via retain_tagged_payload_pub), then release the box shell.
                    crate::tagged::retain_tagged_payload_pub(&*tv_box);
                    crate::tagged::lin_tagged_release(boxed);
                }
                i += 1;
            });
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
        TAG_MAP => {
            let arr = lin_map_entries(tv.payload as *const LinMap);
            stringify_int_entry_keys(arr);
            arr
        }
        TAG_RECORD => {
            let sealed = tv.payload as *mut u8;
            if sealed.is_null() { return crate::array::lin_array_alloc(0); }
            let named_desc = *((sealed.add(16)) as *const *const u8);
            if named_desc.is_null() { return crate::array::lin_array_alloc(0); }
            let field_count = u32::from_le_bytes([
                *named_desc, *named_desc.add(1), *named_desc.add(2), *named_desc.add(3),
            ]) as u64;
            let out = crate::array::lin_array_alloc(field_count);
            (*out).len = field_count;
            let mut i = 0usize;
            crate::sealed::record_walk_fields(named_desc, |name_bytes, offset, nkind, nested| {
                let pair = crate::array::lin_array_alloc(2);
                (*pair).len = 2;
                let key_str = crate::string::lin_string_from_bytes(name_bytes.as_ptr(), name_bytes.len() as u32);
                let k_slot = (*pair).data.add(0) as *mut TaggedVal;
                (*k_slot).tag = crate::tagged::TAG_STR;
                (*k_slot).payload = key_str as u64;
                let boxed = crate::sealed::box_field_value(sealed, offset, nkind, nested);
                let v_slot = (*pair).data.add(1) as *mut TaggedVal;
                if boxed.is_null() {
                    (*v_slot).tag = crate::tagged::TAG_NULL;
                    (*v_slot).payload = 0;
                } else {
                    let tv_box = boxed as *const TaggedVal;
                    (*v_slot).tag = (*tv_box).tag;
                    (*v_slot).payload = (*tv_box).payload;
                    crate::tagged::retain_tagged_payload_pub(&*tv_box);
                    crate::tagged::lin_tagged_release(boxed);
                }
                let dst = (*out).data.add(i) as *mut TaggedVal;
                (*dst).tag = crate::tagged::TAG_ARRAY;
                (*dst).payload = pair as u64;
                i += 1;
            });
            out
        }
        _ => crate::array::lin_array_alloc(0),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_map_raw_len(map: *const LinMap) -> i64 {
    if map.is_null() { 0 } else { (*map).len as i64 }
}

#[no_mangle]
pub unsafe extern "C" fn lin_map_raw_key_at(map: *const LinMap, i: i64) -> *mut u8 {
    if map.is_null() || i < 0 || i >= (*map).len as i64 || (*map).order.is_null() {
        return crate::tagged::alloc_tagged(crate::tagged::TAG_NULL, 0);
    }
    let key = *(*map).order.add(i as usize);
    if (*map).key_kind == KEY_KIND_INT {
        crate::tagged::alloc_tagged(crate::tagged::TAG_INT64, key)
    } else {
        lin_string_inc_ref(key as *mut LinString);
        crate::tagged::alloc_tagged(crate::tagged::TAG_STR, key)
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_map_raw_value_at(map: *const LinMap, i: i64) -> *mut u8 {
    if map.is_null() || i < 0 || i >= (*map).len as i64 || (*map).order.is_null() {
        return crate::tagged::alloc_tagged(crate::tagged::TAG_NULL, 0);
    }
    let key = *(*map).order.add(i as usize);
    let v = if (*map).key_kind == KEY_KIND_INT {
        let p = lin_map_get_int(map, key as i64);
        if p.is_null() { crate::tagged::TaggedVal { tag: crate::tagged::TAG_NULL, _pad: [0; 7], payload: 0 } } else { *p }
    } else {
        let p = lin_map_get(map, key as *const LinString);
        if p.is_null() { crate::tagged::TaggedVal { tag: crate::tagged::TAG_NULL, _pad: [0; 7], payload: 0 } } else { *p }
    };
    crate::tagged::retain_tagged_payload_pub(&v);
    crate::tagged::alloc_tagged(v.tag, v.payload)
}

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

// ── object.rs-parity ops ─────────────────────────────────────────────────────────────────────

/// Structural, order-independent equality of two maps.
#[no_mangle]
pub unsafe extern "C" fn lin_map_eq(a: *const LinMap, b: *const LinMap) -> u8 {
    if a == b { return 1; }
    if a.is_null() || b.is_null() { return 0; }
    if (*a).key_kind != (*b).key_kind { return 0; }
    if (*a).len != (*b).len { return 0; }
    if (*a).ctrl.is_null() { return 1; } // len matched and a is empty → equal
    let cap = (*a).cap as usize;
    let is_int = (*a).key_kind == KEY_KIND_INT;
    let vk = (*a).value_kind;
    let stride = slot_stride(vk);
    let ctrl = (*a).ctrl;
    let base = (*a).slots;
    for i in 0..cap {
        if *ctrl.add(i) == CTRL_EMPTY { continue; }
        let slot = slot_at(base, i, stride);
        let av = slot_value_owned(slot, vk);
        let bval = if is_int {
            lin_map_get_int(b, slot_key(slot) as i64)
        } else {
            lin_map_get(b, slot_key(slot) as *const LinString)
        };
        if bval.is_null() { return 0; }
        let av_ptr = &av as *const TaggedVal as *const u8;
        if crate::tagged::lin_tagged_eq(av_ptr, bval as *const u8) == 0 {
            return 0;
        }
    }
    1
}

/// Merge `src` into `dst` (object spread). Iterates in insertion order.
#[no_mangle]
pub unsafe extern "C-unwind" fn lin_map_merge(dst: *mut LinMap, src: *const LinMap) {
    if src.is_null() || dst.is_null() { return; }
    let len = (*src).len as usize;
    let is_int = (*src).key_kind == KEY_KIND_INT;
    if (*src).order.is_null() || len == 0 { return; }
    for i in 0..len {
        let key = *(*src).order.add(i);
        if is_int {
            let p = lin_map_get_int(src, key as i64);
            let v = if p.is_null() { TaggedVal { tag: TAG_NULL, _pad: [0; 7], payload: 0 } } else { *p };
            lin_map_set_int(dst, key as i64, &v);
        } else {
            let p = lin_map_get(src, key as *const LinString);
            let v = if p.is_null() { TaggedVal { tag: TAG_NULL, _pad: [0; 7], payload: 0 } } else { *p };
            lin_map_set(dst, key as *mut LinString, &v);
        }
    }
}

/// Copy every entry of `src` into `dst` except those whose key is in `excluded`.
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
        let p = lin_map_get(src, key);
        let v = if p.is_null() { TaggedVal { tag: TAG_NULL, _pad: [0; 7], payload: 0 } } else { *p };
        lin_map_set(dst, key as *mut LinString, &v);
    }
}

/// Normalize ANY dynamic-object representation to a fresh owned `LinMap` (+1).
pub(crate) unsafe fn dynamic_to_map(tv: *const TaggedVal) -> *mut LinMap {
    crate::repr_verify::repr_note("dynamic_to_map");
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
            if named_desc.is_null() { return lin_map_alloc(0, KEY_KIND_STRING); }
            let field_count = u32::from_le_bytes([
                *named_desc, *named_desc.add(1), *named_desc.add(2), *named_desc.add(3),
            ]);
            let m = lin_map_alloc(field_count, KEY_KIND_STRING);
            crate::sealed::record_walk_fields(named_desc, |name_bytes, offset, nkind, nested| {
                // RC: lin_string_from_bytes → rc=1 (ours). lin_map_set inc_refs key → map owns +1.
                let key_str = crate::string::lin_string_from_bytes(name_bytes.as_ptr(), name_bytes.len() as u32);
                // RC: box_field_value → OWNED +1 TaggedVal*. lin_map_set retains payload → +2.
                // lin_tagged_release → releases payload -1 + frees box shell → map owns +1.
                let boxed = crate::sealed::box_field_value(sealed, offset, nkind, nested);
                let null_tv = TaggedVal { tag: crate::tagged::TAG_NULL, _pad: [0; 7], payload: 0 };
                let tv_val: &TaggedVal = if boxed.is_null() { &null_tv } else { &*(boxed as *const TaggedVal) };
                lin_map_set(m, key_str as *mut crate::string::LinString, tv_val);
                if !boxed.is_null() { crate::tagged::lin_tagged_release(boxed); }
                crate::string::lin_string_release(key_str);
            });
            m
        }
        t if t == crate::tagged::TAG_SUMNODE => {
            let m = crate::sumnode::lin_sumnode_materialize((*tv).payload as *mut u8);
            if m.is_null() { return lin_map_alloc(0, KEY_KIND_STRING); }
            m as *mut LinMap
        }
        _ => lin_map_alloc(0, KEY_KIND_STRING),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_union_force_to_map(tv: *const u8) -> *mut LinMap {
    crate::repr_verify::repr_note("lin_union_force_to_map");
    dynamic_to_map(tv as *const TaggedVal)
}

/// Cluster D: Get or insert a `LinArray` at `key` inside a LinMap.
#[no_mangle]
pub unsafe extern "C" fn lin_map_get_or_insert_array(obj: *const u8, key: *const u8) -> *mut u8 {
    use crate::tagged::{TaggedVal, TAG_ARRAY, TAG_MAP, TAG_STR, alloc_tagged};
    use crate::string::LinString;
    if obj.is_null() {
        return alloc_tagged(TAG_ARRAY, crate::array::lin_array_alloc(4) as u64);
    }
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
        let arr = crate::array::lin_array_alloc(4);
        let val = TaggedVal { tag: TAG_ARRAY, _pad: [0; 7], payload: arr as u64 };
        lin_map_set(lin_map, key_str as *mut LinString, &val);
        crate::array::lin_array_release(arr);
        if (*arr).refcount < crate::string::IMMORTAL_RC {
            (*arr).refcount += 1;
        }
        return alloc_tagged(TAG_ARRAY, arr as u64);
    }
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
            assert_eq!((*m).value_kind, TAG_INT32 as u32);
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
        unsafe {
            let m = lin_map_alloc(0, KEY_KIND_INT);
            assert_eq!((*m).key_kind, KEY_KIND_INT);

            let v0 = int_val(100);
            lin_map_set_int(m, 0, &v0);
            let vm1 = int_val(200);
            lin_map_set_int(m, -1, &vm1);
            let v42 = int_val(300);
            lin_map_set_int(m, 42, &v42);
            let v1m = int_val(400);
            lin_map_set_int(m, 1_000_000, &v1m);

            assert_eq!(lin_map_length(m), 4);

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

            assert!(lin_map_get_int(m, 7).is_null(), "key 7 should be absent");

            let v42b = int_val(999);
            lin_map_set_int(m, 42, &v42b);
            assert_eq!(lin_map_length(m), 4);
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
            for i in 0..2usize {
                let tv = &*(*keys).data.add(i);
                assert_eq!(tv.tag, TAG_INT64);
            }
            crate::array::lin_array_release(keys);
            lin_map_release(m);
        }
    }

    #[test]
    fn test_keys_flat_narrows_to_element_width() {
        unsafe {
            let m = lin_map_alloc(0, KEY_KIND_INT);
            lin_map_set_int(m, 3, &int_val(30));
            lin_map_set_int(m, 10, &int_val(100));
            let keys = lin_keys_flat(m, crate::tagged::TAG_UINT8);
            assert_eq!((*keys).len, 2);
            assert_eq!((*keys).elem_tag, crate::tagged::TAG_UINT8);
            assert_eq!(crate::array::lin_flat_array_get_u8(keys, 0), 3);
            assert_eq!(crate::array::lin_flat_array_get_u8(keys, 1), 10);
            crate::array::lin_array_release(keys);
            lin_map_release(m);
        }
    }

    #[test]
    fn test_int_map_grows() {
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
            assert_eq!((*m).value_kind, TAG_STR as u32);
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
    fn heterogeneous_values_upgrade_to_mixed() {
        unsafe {
            let m = lin_map_alloc(0, KEY_KIND_STRING);
            let ka = str_key("a");
            let va = int_val(7);
            lin_map_set(m, ka, &va);
            lin_string_release(ka);
            assert_eq!((*m).value_kind, TAG_INT32 as u32, "first insert sets homogeneous kind");

            let kb = str_key("b");
            let (sv, vb) = str_tagged_val("hello");
            lin_map_set(m, kb, &vb);
            lin_string_release(kb);
            lin_string_release(sv);
            assert_eq!((*m).value_kind, VKIND_MIXED, "tag mismatch upgrades to MIXED");

            let ka2 = str_key("a");
            let ga = lin_map_get(m, ka2);
            assert!(!ga.is_null());
            assert_eq!((*ga).tag, TAG_INT32);
            assert_eq!((*ga).payload as u32 as i32, 7);
            lin_string_release(ka2);

            let kb2 = str_key("b");
            let gb = lin_map_get(m, kb2);
            assert!(!gb.is_null());
            assert_eq!((*gb).tag, TAG_STR);
            let s = (*gb).payload as *const LinString;
            assert_eq!((*s).as_str(), "hello");
            lin_string_release(kb2);

            assert_eq!(lin_map_length(m), 2);
            lin_map_release(m);
        }
    }

    #[test]
    fn many_string_values_no_leak_after_release() {
        unsafe {
            let m = lin_map_alloc(0, KEY_KIND_STRING);
            let mut handles: Vec<*mut LinString> = Vec::new();
            for i in 0..30i32 {
                let k = str_key(&format!("k{i}"));
                let (sv, v) = str_tagged_val(&format!("v{i}"));
                lin_map_set(m, k, &v);
                lin_string_release(k);
                assert_eq!((*sv).refcount, 2, "value held by map + our handle");
                handles.push(sv);
            }
            lin_map_release(m);
            for sv in handles {
                assert_eq!((*sv).refcount, 1, "map over-retained or leaked a value");
                lin_string_release(sv);
            }
        }
    }

    #[test]
    fn map_eq_structural_order_independent() {
        unsafe {
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

    /// Stress test: insert 10,000 string keys and verify all round-trip correctly after multiple grows.
    #[test]
    fn large_string_map_stress() {
        unsafe {
            let m = lin_map_alloc(0, KEY_KIND_STRING);
            let n = 10_000i32;
            for i in 0..n {
                let k = str_key(&format!("stop_{i:05}"));
                let v = int_val(i * 7);
                lin_map_set(m, k, &v);
                lin_string_release(k);
            }
            assert_eq!(lin_map_length(m), n as i64);
            for i in 0..n {
                let k = str_key(&format!("stop_{i:05}"));
                let got = lin_map_get(m, k);
                assert!(!got.is_null(), "stop_{i:05} missing");
                assert_eq!((*got).payload as u32 as i32, i * 7);
                lin_string_release(k);
            }
            // Miss
            let k = str_key("stop_99999");
            assert!(lin_map_get(m, k).is_null());
            lin_string_release(k);
            lin_map_release(m);
        }
    }

    /// Verify insertion order is preserved across grows.
    #[test]
    fn insertion_order_preserved() {
        unsafe {
            let m = lin_map_alloc(0, KEY_KIND_STRING);
            let keys = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta",
                        "iota", "kappa", "lambda", "mu", "nu", "xi", "omicron", "pi"];
            for (i, &kname) in keys.iter().enumerate() {
                let k = str_key(kname);
                let v = int_val(i as i32);
                lin_map_set(m, k, &v);
                lin_string_release(k);
            }
            let karr = lin_map_keys(m);
            assert_eq!((*karr).len, keys.len() as u64);
            for (i, &expected) in keys.iter().enumerate() {
                let elem = (*karr).data.add(i);
                assert_eq!((*elem).tag, TAG_STR);
                let s = (*elem).payload as *const LinString;
                assert_eq!((*s).as_str(), expected, "order mismatch at position {i}");
            }
            crate::array::lin_array_release(karr);
            lin_map_release(m);
        }
    }

    /// Collision stress: keys that map to the same h2 (7-bit hash) should still round-trip.
    #[test]
    fn h2_collision_stress() {
        unsafe {
            // Insert enough keys that h2 collisions are statistically guaranteed (pigeonhole).
            let m = lin_map_alloc(0, KEY_KIND_STRING);
            let n = 500i32; // 128 h2 values → guaranteed collisions
            for i in 0..n {
                let k = str_key(&format!("route_{i}"));
                let v = int_val(i);
                lin_map_set(m, k, &v);
                lin_string_release(k);
            }
            assert_eq!(lin_map_length(m), n as i64);
            for i in 0..n {
                let k = str_key(&format!("route_{i}"));
                let got = lin_map_get(m, k);
                assert!(!got.is_null(), "route_{i} missing");
                assert_eq!((*got).payload as u32 as i32, i);
                lin_string_release(k);
            }
            lin_map_release(m);
        }
    }
}
