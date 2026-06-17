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
//!
//! ## Freeze-repack for 0xFD pointer-backed sealed-record arrays
//!
//! When the freeze walk hits a 0xFD (`SEALED_PTR_ARRAY_TAG`) array, it repacks it into a 0xFE
//! (`SEALED_ARRAY_TAG`) inline buffer. The repack is sound ONLY because freeze means the value is
//! transitioning to read-only program-lifetime state — nothing will push/mutate it after this
//! point. The win: each element's 24-byte header + glibc per-object overhead (~16 B) is eliminated.
//! For a `{String: Rec[]}` index with 70M records this is ~48 B × 70M = ~3.36 GB saved.
//!
//! Repack sequence (per array):
//!   1. Read elem payload stride from the first struct's header (offset 4 = `size` field minus
//!      `SEALED_HEADER`). All elements of a typed array share the same size, so the first is
//!      representative.
//!   2. Allocate a new inline buffer: `stride × len` bytes, 8-aligned.
//!   3. For each element struct pointer in the old spine:
//!      a. Copy `stride` bytes from `ptr + SEALED_HEADER` → new inline slot.
//!      b. Freeze any heap fields in the new slot (same walk as freeze_sealed but on the payload).
//!      c. Free the OLD struct shell with `dealloc` — WITHOUT walking heap descriptors, because
//!         the heap-field pointers have moved into the inline buffer (freeing them here would be
//!         a double-free once the new buffer is dropped at process exit… but since freeze = never
//!         freed, there IS no drop. The old struct shell is the only allocation we reclaim now).
//!   4. Free the old pointer spine (`8 × cap` bytes).
//!   5. Update the array header: `elem_tag ← SEALED_ARRAY_TAG`, `data ← new inline buffer`,
//!      `elem_stride ← stride`, `cap ← len` (exact fit, no wasted capacity).
//!
//! The FREEZE_REPACK_COUNT atomic counter (instrumented by `LIN_FREEZE_STATS=1`) tracks how many
//! 0xFD arrays were repacked — used by the POC benchmark to confirm the repack fired.

use std::sync::atomic::{AtomicU64, Ordering};
use crate::tagged::{TaggedVal, TAG_STR, TAG_ARRAY, TAG_MAP, TAG_RECORD, TAG_SUMNODE};
use crate::string::{LinString, IMMORTAL_RC};
use crate::array::{LinArray, LinArrayElem, SEALED_PTR_ARRAY_TAG, SEALED_ARRAY_TAG};
use crate::map::LinMap;

/// Number of 0xFD arrays repacked to 0xFE during a freeze walk. Enabled by `LIN_FREEZE_STATS=1`.
pub static FREEZE_REPACK_COUNT: AtomicU64 = AtomicU64::new(0);
/// Number of struct shells freed during repack (one per element). Enabled by `LIN_FREEZE_STATS=1`.
pub static FREEZE_FREED_SHELLS: AtomicU64 = AtomicU64::new(0);
pub static FREEZE_STATS_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn ensure_freeze_stats_init() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        if std::env::var("LIN_FREEZE_STATS").as_deref() == Ok("1") {
            FREEZE_STATS_ENABLED.store(true, Ordering::Relaxed);
            unsafe { libc::atexit(freeze_stats_atexit) };
        }
    });
}

extern "C" fn freeze_stats_atexit() {
    if FREEZE_STATS_ENABLED.load(Ordering::Relaxed) {
        let repacked = FREEZE_REPACK_COUNT.load(Ordering::Relaxed);
        let freed = FREEZE_FREED_SHELLS.load(Ordering::Relaxed);
        eprintln!("FREEZE_STATS: repacked_0xfd_arrays={repacked} freed_struct_shells={freed}");
    }
}

/// Recursively seal a `LinString` immortal (idempotent). `pub(crate)` so the sum-node freeze walk
/// (`sumnode::lin_sumnode_freeze`) can seal a variant's string fields with the same primitive.
pub(crate) unsafe fn freeze_string(s: *mut LinString) {
    if !s.is_null() {
        (*s).refcount = IMMORTAL_RC;
    }
}

/// Recursively seal a `LinArray` and all its (tagged) elements immortal. Flat scalar arrays have
/// no nested pointers, so only the header is sealed.
/// For 0xFD pointer-backed sealed-record arrays: REPACK to 0xFE inline before sealing (see
/// module-level doc for rationale and sequence).
pub(crate) unsafe fn freeze_array(arr: *mut LinArray) {
    if arr.is_null() || (*arr).refcount >= IMMORTAL_RC {
        return; // null or already frozen (also breaks any accidental sharing/cycle)
    }
    // Repack 0xFD → 0xFE BEFORE setting IMMORTAL_RC: the repack modifies the array header
    // (elem_tag, data, elem_stride, cap) and must happen while the array is still mutable.
    if (*arr).elem_tag == SEALED_PTR_ARRAY_TAG {
        ensure_freeze_stats_init();
        repack_ptr_array_to_inline(arr);
        // Fall through: now elem_tag == SEALED_ARRAY_TAG; the inline elements' heap fields were
        // frozen by repack_ptr_array_to_inline, so we only need to seal the array header.
        (*arr).refcount = IMMORTAL_RC;
        return;
    }
    (*arr).refcount = IMMORTAL_RC;
    if (*arr).elem_tag == 0xFF {
        let len = (*arr).len as usize;
        for i in 0..len {
            let elem = (*arr).data.add(i) as *mut LinArrayElem;
            freeze_payload((*elem).tag, (*elem).payload);
        }
    }
    // 0xFE (already inline) and flat scalar arrays: no nested heap pointers to freeze.
    // For 0xFE arrays that already exist without going through 0xFD (e.g. direct
    // lin_sealed_array_alloc): walk each element's heap fields via elem_desc.
    if (*arr).elem_tag == SEALED_ARRAY_TAG {
        let len = (*arr).len as usize;
        let stride = (*arr).elem_stride as usize;
        let desc = (*arr).elem_desc;
        if !desc.is_null() {
            for i in 0..len {
                let payload = ((*arr).data as *mut u8).add(i * stride);
                freeze_sealed_payload(payload, desc);
            }
        }
    }
}

/// Repack a 0xFD pointer-backed sealed-record array into 0xFE inline layout.
/// SAFETY: `arr` must be a valid, non-null 0xFD array that is not yet frozen (RC < IMMORTAL_RC).
/// After this returns, `arr.elem_tag == SEALED_ARRAY_TAG` and `arr.data` points to the new
/// inline buffer. The old pointer spine and per-element struct shells have been freed.
unsafe fn repack_ptr_array_to_inline(arr: *mut LinArray) {
    use std::alloc::{alloc, dealloc, Layout};
    use crate::sealed::SEALED_HEADER;

    let len = (*arr).len as usize;
    let cap = (*arr).cap as usize;
    let named_desc = (*arr).elem_named_desc;
    let heap_desc = (*arr).elem_desc;

    // If the array is empty, no work beyond tag swap and freeing the empty spine.
    if len == 0 {
        let old_spine_layout = Layout::from_size_align_unchecked(8 * cap.max(1), 8);
        dealloc((*arr).data as *mut u8, old_spine_layout);
        (*arr).elem_tag = SEALED_ARRAY_TAG;
        (*arr).elem_stride = 0;
        (*arr).cap = 0;
        (*arr).data = std::ptr::null_mut::<LinArrayElem>();
        if FREEZE_STATS_ENABLED.load(Ordering::Relaxed) {
            FREEZE_REPACK_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        return;
    }

    // Derive stride from the first element struct's header (offset 4 = size field).
    let slots = (*arr).data as *const *mut u8;
    let first_sptr = *slots;
    if first_sptr.is_null() {
        // Pathological: first element is null. Treat as empty.
        let old_spine_layout = Layout::from_size_align_unchecked(8 * cap.max(1), 8);
        dealloc((*arr).data as *mut u8, old_spine_layout);
        (*arr).elem_tag = SEALED_ARRAY_TAG;
        (*arr).elem_stride = 0;
        (*arr).len = 0;
        (*arr).cap = 0;
        (*arr).data = std::ptr::null_mut::<LinArrayElem>();
        if FREEZE_STATS_ENABLED.load(Ordering::Relaxed) {
            FREEZE_REPACK_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        return;
    }
    let struct_size = *((first_sptr.add(4)) as *const u32) as usize; // header offset 4 = total size
    let stride = struct_size.saturating_sub(SEALED_HEADER);

    // Allocate the new inline buffer (stride * len bytes, 8-aligned, exact fit).
    let inline_layout = Layout::from_size_align_unchecked(stride.max(1) * len, 8);
    let inline_buf = alloc(inline_layout);
    if inline_buf.is_null() {
        std::alloc::handle_alloc_error(inline_layout);
    }

    // Copy payload + freeze heap fields + free old struct shells.
    let mut freed_count: u64 = 0;
    for i in 0..len {
        let sptr = *slots.add(i);
        let dst = inline_buf.add(i * stride);
        if sptr.is_null() {
            // NULL element slot: zero-fill (safe default for any scalar/pointer payload).
            std::ptr::write_bytes(dst, 0, stride);
            continue;
        }
        // Copy the payload bytes (src = sptr + SEALED_HEADER, stride bytes).
        std::ptr::copy_nonoverlapping(sptr.add(SEALED_HEADER), dst, stride);
        // Freeze the heap fields within the new inline slot. The heap pointers are now
        // IN the inline buffer (we just copied them), so freeze from the new location.
        if !heap_desc.is_null() {
            freeze_sealed_payload(dst, heap_desc);
        }
        // Free the old struct shell. We MUST NOT call lin_sealed_release_self / release_heap_fields
        // because the heap-field pointers have moved into `dst` — releasing them here would be a
        // use-after-move. We dealloc only the struct allocation itself.
        let shell_layout = Layout::from_size_align_unchecked(struct_size, 8);
        dealloc(sptr, shell_layout);
        freed_count += 1;
    }

    // Free the old pointer spine.
    let old_spine_layout = Layout::from_size_align_unchecked(8 * cap.max(1), 8);
    dealloc((*arr).data as *mut u8, old_spine_layout);

    // Swap the array header to 0xFE inline layout.
    (*arr).elem_tag = SEALED_ARRAY_TAG;
    (*arr).data = inline_buf as *mut LinArrayElem;
    (*arr).elem_stride = stride as u64;
    (*arr).cap = len as u64; // exact fit; frozen arrays never grow
    (*arr).elem_desc = heap_desc; // keep the heap-only descriptor for completeness
    (*arr).elem_named_desc = named_desc;

    if FREEZE_STATS_ENABLED.load(Ordering::Relaxed) {
        FREEZE_REPACK_COUNT.fetch_add(1, Ordering::Relaxed);
        FREEZE_FREED_SHELLS.fetch_add(freed_count, Ordering::Relaxed);
    }
}

/// Freeze the heap fields of a HEADER-LESS sealed-record payload (for 0xFE inline elements).
/// Mirrors `freeze_sealed` but operates on a payload pointer (no refcount field, no size field).
/// `desc` is the heap-only field descriptor (the same format walked by `release_payload_fields`
/// in sealed.rs). NULL descriptor → no heap fields → no-op.
unsafe fn freeze_sealed_payload(payload: *mut u8, desc: *const u8) {
    if payload.is_null() || desc.is_null() {
        return;
    }
    use crate::sealed::SEALED_HEADER;
    let count = *(desc as *const u32);
    let entries = desc.add(4);
    for i in 0..count as usize {
        let ent = entries.add(i * 8);
        let offset = *(ent as *const u32) as usize;
        let kind = *((ent.add(4)) as *const u32);
        // Payload-relative: descriptor stores struct-relative offsets, subtract SEALED_HEADER.
        let field_ptr = *(payload.add(offset - SEALED_HEADER) as *const *mut u8);
        if field_ptr.is_null() {
            continue;
        }
        match kind {
            crate::sealed::KIND_STRING => freeze_string(field_ptr as *mut LinString),
            crate::sealed::KIND_ARRAY => freeze_array(field_ptr as *mut LinArray),
            crate::sealed::KIND_SEALED => freeze_sealed(field_ptr),
            crate::sealed::KIND_MAP => freeze_map(field_ptr as *mut LinMap),
            crate::sealed::KIND_SUMNODE_FIELD => crate::sumnode::lin_sumnode_freeze(field_ptr),
            _ => {}
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
    use crate::array::{lin_array_alloc, lin_array_push_tagged, lin_sealed_ptr_array_alloc, lin_sealed_ptr_array_push, lin_sealed_array_elem_ptr, SEALED_ARRAY_TAG, SEALED_PTR_ARRAY_TAG};
    use crate::tagged::{alloc_tagged, TAG_INT32, TAG_ARRAY};
    use crate::sealed::{lin_sealed_alloc, lin_sealed_release_self, SEALED_HEADER, NKIND_INT32, NKIND_STRING, KIND_STRING};

    // ── helpers ──────────────────────────────────────────────────────────────────────────────

    /// Build a NamedDesc for a scalar record P { x: Int32, y: Int32 }.
    /// Struct layout (24-byte SEALED_HEADER): x@24 (i32), y@28 (i32). Stride = 8 bytes.
    fn scalar_named_desc() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&2u32.to_le_bytes()); // field count
        b.extend_from_slice(&0u32.to_le_bytes()); // header pad
        for (name, off) in [("x", 24u32), ("y", 28u32)] {
            b.extend_from_slice(&off.to_le_bytes());
            b.extend_from_slice(&NKIND_INT32.to_le_bytes());
            b.extend_from_slice(&0u64.to_le_bytes()); // nested_ptr = NULL
            b.extend_from_slice(&(name.len() as u16).to_le_bytes());
            b.extend_from_slice(name.as_bytes());
            let row_len = 18 + name.len();
            let pad = (8 - (row_len % 8)) % 8;
            b.extend(std::iter::repeat(0u8).take(pad));
        }
        b
    }

    /// Build a NamedDesc for a heap-field record R { name: String, n: Int32 }.
    /// Struct layout (24-byte SEALED_HEADER): name@24 (8-byte ptr), n@32 (i32). Stride = 16 bytes.
    fn heap_named_desc() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&2u32.to_le_bytes()); // field count
        b.extend_from_slice(&0u32.to_le_bytes()); // header pad
        for (name, off, nkind) in [("name", 24u32, NKIND_STRING), ("n", 32u32, NKIND_INT32)] {
            b.extend_from_slice(&off.to_le_bytes());
            b.extend_from_slice(&nkind.to_le_bytes());
            b.extend_from_slice(&0u64.to_le_bytes()); // nested_ptr = NULL
            b.extend_from_slice(&(name.len() as u16).to_le_bytes());
            b.extend_from_slice(name.as_bytes());
            let row_len = 18 + name.len();
            let pad = (8 - (row_len % 8)) % 8;
            b.extend(std::iter::repeat(0u8).take(pad));
        }
        b
    }

    /// Build a heap-only descriptor for R { name: String } (one heap field: name@24, KIND_STRING).
    fn string_heap_desc() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&1u32.to_le_bytes()); // count
        b.extend_from_slice(&24u32.to_le_bytes()); // offset
        b.extend_from_slice(&KIND_STRING.to_le_bytes()); // kind
        b
    }

    // ── tests ────────────────────────────────────────────────────────────────────────────────

    /// Freeze a 0xFD array of scalar sealed records: after freeze, elem_tag must be 0xFE and
    /// field reads through the inline element pointer must return the original values.
    #[test]
    fn freeze_repacks_0xfd_scalar_to_0xfe_and_reads_correctly() {
        unsafe {
            let named = scalar_named_desc();
            let arr = lin_sealed_ptr_array_alloc(4, named.as_ptr());
            assert_eq!((*arr).elem_tag, SEALED_PTR_ARRAY_TAG);

            // Push 3 elements.
            for i in 0i32..3 {
                let sptr = lin_sealed_alloc(SEALED_HEADER + 8, std::ptr::null(), named.as_ptr());
                *(sptr.add(24) as *mut i32) = i;
                *(sptr.add(28) as *mut i32) = i * 10;
                lin_sealed_ptr_array_push(arr, sptr);
                lin_sealed_release_self(sptr); // drop our ref (array retains)
            }
            assert_eq!((*arr).len, 3);
            assert_eq!((*arr).elem_tag, SEALED_PTR_ARRAY_TAG);

            // Freeze: this should repack 0xFD → 0xFE.
            let boxed = alloc_tagged(TAG_ARRAY, arr as u64);
            lin_freeze(boxed);

            // Array must now be 0xFE.
            assert_eq!((*arr).elem_tag, SEALED_ARRAY_TAG, "elem_tag must be 0xFE after freeze");
            assert_eq!((*arr).len, 3);
            assert_eq!((*arr).elem_stride, 8, "stride = struct_size - SEALED_HEADER = 32 - 24 = 8");
            assert!((*arr).refcount >= IMMORTAL_RC, "array must be immortal after freeze");

            // Read fields through the 0xFE element pointer.
            for i in 0i32..3 {
                let elem = lin_sealed_array_elem_ptr(arr, i as i64);
                let x = *(elem as *const i32);
                let y = *(elem.add(4) as *const i32);
                assert_eq!(x, i, "elem[{i}].x should be {i}, got {x}");
                assert_eq!(y, i * 10, "elem[{i}].y should be {}, got {y}", i * 10);
            }

            crate::tagged::lin_tagged_free_box(boxed);
        }
    }

    /// Freeze a 0xFD array with a heap-field (String) element: after freeze, the inline slot must
    /// hold the string pointer with IMMORTAL_RC, and the original struct shells must be freed.
    #[test]
    fn freeze_repacks_0xfd_heap_field_seals_inner_string() {
        unsafe {
            let named = heap_named_desc();
            let heap_desc = string_heap_desc();
            // Build the array with the heap descriptor so repack can freeze the string field.
            let arr = lin_sealed_ptr_array_alloc(4, named.as_ptr());
            // Patch elem_desc to our heap_desc (lin_sealed_ptr_array_alloc derives it from named_desc;
            // verify it matches ours or override for test isolation).
            let arr_heap_desc = (*arr).elem_desc;
            // Note: if build_heap_desc_from_named_desc is correct, arr_heap_desc will already point
            // to a descriptor with KIND_STRING@24 — our test heap_desc. We verify through reads.
            let _ = arr_heap_desc;

            // Build 3 elements each with a unique string.
            let strs: Vec<*mut crate::string::LinString> = (0..3).map(|i| {
                crate::string::lin_string_from_bytes(
                    format!("str{i}").as_ptr(), format!("str{i}").len() as u32
                )
            }).collect();
            for (i, &s) in strs.iter().enumerate() {
                let sptr = lin_sealed_alloc(SEALED_HEADER + 16, (*arr).elem_desc, named.as_ptr());
                // Write string pointer into the heap-field slot at struct-relative offset 24.
                *(sptr.add(24) as *mut *mut crate::string::LinString) = s;
                *(sptr.add(32) as *mut i32) = (i as i32) * 7;
                // Retain the string for the struct (lin_sealed_alloc doesn't auto-retain).
                crate::memory::lin_rc_retain(s as *mut u32); // now rc=2 (our + struct)
                lin_sealed_ptr_array_push(arr, sptr); // array retains: rc=3
                lin_sealed_release_self(sptr); // drop struct ref: rc=2 (our + array)
            }
            // Drop our construction refs — array now owns 1 per string.
            for &s in &strs { crate::string::lin_string_release(s); }
            // Each string should now have rc=1 (owned by the array).
            for &s in &strs {
                assert_eq!((*s).refcount, 1, "array must be the sole owner before freeze");
            }

            // Freeze repacks 0xFD → 0xFE.
            let boxed = alloc_tagged(TAG_ARRAY, arr as u64);
            lin_freeze(boxed);

            assert_eq!((*arr).elem_tag, SEALED_ARRAY_TAG, "elem_tag must be 0xFE after freeze");
            assert_eq!((*arr).len, 3);
            assert_eq!((*arr).elem_stride, 16, "stride = 40 - 24 = 16");
            assert!((*arr).refcount >= IMMORTAL_RC);

            // All strings must now be immortal (freeze_sealed_payload walked the heap desc).
            for &s in &strs {
                assert!((*s).refcount >= IMMORTAL_RC,
                    "string must be immortal after freeze; got rc={}", (*s).refcount);
            }

            // Read field values through the inline element pointer.
            for (i, &s) in strs.iter().enumerate() {
                let elem = lin_sealed_array_elem_ptr(arr, i as i64);
                let s_from_slot = *(elem as *const *mut crate::string::LinString);
                assert_eq!(s_from_slot, s, "inline slot must hold the original string pointer");
                let n = *(elem.add(8) as *const i32);
                assert_eq!(n, (i as i32) * 7, "n field wrong for elem {i}");
            }

            crate::tagged::lin_tagged_free_box(boxed);
        }
    }

    /// Freeze stats counter increments for a repacked array when LIN_FREEZE_STATS is not set
    /// (the counter is always written, just not printed). Verify FREEZE_REPACK_COUNT increments.
    #[test]
    fn freeze_repack_count_increments() {
        unsafe {
            // Enable stats counters for this test.
            FREEZE_STATS_ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);
            let before = FREEZE_REPACK_COUNT.load(std::sync::atomic::Ordering::Relaxed);

            let named = scalar_named_desc();
            let arr = lin_sealed_ptr_array_alloc(4, named.as_ptr());
            let sptr = lin_sealed_alloc(SEALED_HEADER + 8, std::ptr::null(), named.as_ptr());
            *(sptr.add(24) as *mut i32) = 99;
            *(sptr.add(28) as *mut i32) = 88;
            lin_sealed_ptr_array_push(arr, sptr);
            lin_sealed_release_self(sptr);

            let boxed = alloc_tagged(TAG_ARRAY, arr as u64);
            lin_freeze(boxed);

            let after = FREEZE_REPACK_COUNT.load(std::sync::atomic::Ordering::Relaxed);
            assert!(after > before, "FREEZE_REPACK_COUNT must increment after a 0xFD repack");
            let freed = FREEZE_FREED_SHELLS.load(std::sync::atomic::Ordering::Relaxed);
            assert!(freed >= 1, "FREEZE_FREED_SHELLS must count the freed struct shell");

            FREEZE_STATS_ENABLED.store(false, std::sync::atomic::Ordering::Relaxed);
            crate::tagged::lin_tagged_free_box(boxed);
        }
    }

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
