/// Tagged union representation for Lin Union-typed values.
///
/// Layout: heap-allocated { u8 tag, [8]u8 payload }
/// Tags:
///   0 = Null   (represented as null pointer — no heap alloc needed)
///   1 = Bool   (payload: u8, 0=false, 1=true)
///   2 = Int32  (payload: i32 little-endian)
///   3 = Int64  (payload: i64 little-endian)
///   4 = Float32 (payload: f32)
///   5 = Float64 (payload: f64)
///   6 = Str    (payload: *mut LinString as pointer)
///   7 = Object (payload: opaque pointer)
///   8 = Array  (payload: *mut LinArray)
///   9 = Function (payload: closure pointer)
///
/// SMI (Small Integer Inline) encoding — feature `smi` only:
///   When bit0 of a `*mut u8` / `*const u8` value is SET (= 1), the pointer is NOT a heap
///   address. It encodes an inline integer: `value = (ptr as isize) >> 1` (arithmetic shift).
///   Encoding: `ptr = ((n as u64) << 1) | 1`.
///   An SMI pointer MUST NEVER be dereferenced. All consumers guard with `is_smi_ptr` first.
///   RC operations (retain/release/clone/free) are no-ops on SMI pointers.
///   The integer kind (Int32 vs Int64) is encoded in the high bits:
///     - Int32 SMI: top 33 bits are sign-extension of bit 30 (fits i32 range)
///     - Int64 SMI: full 63-bit range (bit 62 sign-extension of bit 62)
///   For simplicity we encode BOTH Int32 and Int64 as a 63-bit signed immediate and recover
///   the tag via the call site (lin_box_int32 vs lin_box_int64).  The unbox functions
///   truncate/sign-extend as appropriate for their declared return type.
///   NULL (0) always means Tag_NULL — bit0=0, so null is never an SMI.

use std::alloc::{Layout, alloc};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

// ── LIN_SMI_STATS instrumentation ──────────────────────────────────────────────────────────────
// When LIN_SMI_STATS=1, every alloc_tagged call is counted by class:
//   int_allocs   — TAG_INT32 / TAG_INT64 / TAG_UINT64 heap boxes
//   other_allocs — everything else (str, array, map, function, float, …)
// Stats are printed to stderr at process exit via atexit. Zero overhead when env var absent.
//
// Note: with feature `smi` ON, lin_box_int32/int64 emit SMI immediates and NEVER reach
// alloc_tagged, so int_allocs should be ~0 when smi is active (only TAG_UINT64 / out-of-62-bit
// i64 boxes land here). With smi OFF, cached ints [-128, 65536) and bools are intercepted before
// alloc_tagged, so int_allocs counts only out-of-cache-range heap boxes.

static SMI_STATS_STATE: AtomicU8 = AtomicU8::new(0); // 0=uninit 1=disabled 2=enabled
static SMI_INT_ALLOCS: AtomicU64 = AtomicU64::new(0);
static SMI_OTHER_ALLOCS: AtomicU64 = AtomicU64::new(0);
// Count of integer boxes that were encoded as SMI immediates (only meaningful with feature `smi`).
#[cfg(feature = "smi")]
static SMI_INT_BOXES: AtomicU64 = AtomicU64::new(0);

#[cold]
fn init_smi_stats() -> u8 {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let enabled = std::env::var("LIN_SMI_STATS").as_deref() == Ok("1");
        SMI_STATS_STATE.store(if enabled { 2 } else { 1 }, Ordering::SeqCst);
        if enabled {
            unsafe { libc::atexit(smi_stats_atexit); }
        }
    });
    SMI_STATS_STATE.load(Ordering::SeqCst)
}

#[inline(always)]
fn smi_stats_state() -> u8 {
    let s = SMI_STATS_STATE.load(Ordering::Relaxed);
    if s == 0 { init_smi_stats() } else { s }
}

extern "C" fn smi_stats_atexit() {
    let int_allocs = SMI_INT_ALLOCS.load(Ordering::Relaxed);
    let other_allocs = SMI_OTHER_ALLOCS.load(Ordering::Relaxed);
    let total = int_allocs + other_allocs;
    let pct = if total > 0 { int_allocs as f64 / total as f64 * 100.0 } else { 0.0 };
    eprintln!(
        "SMI_STATS: total_alloc_tagged={total} int_boxes={int_allocs} ({pct:.1}%) \
         other_boxes={other_allocs}"
    );
    #[cfg(feature = "smi")]
    {
        let smi_int_boxes = SMI_INT_BOXES.load(Ordering::Relaxed);
        eprintln!(
            "SMI_STATS: smi_int_boxes={smi_int_boxes} (inline immediates, never reach alloc_tagged)"
        );
    }
    #[cfg(not(feature = "smi"))]
    eprintln!(
        "SMI_STATS: (cached ints [-128,65536) and bools never reach alloc_tagged; \
         int_boxes = out-of-cache-range heap boxes only)"
    );
}

// The canonical tag values live in `lin_common::tags` — the SINGLE source of truth shared
// with the compiler backend (`lin-codegen`) so a tag byte can never drift from how the
// runtime reads it. Re-exported here so existing `crate::tagged::TAG_*` references keep
// working. Semantic notes on the non-obvious tags:
//   TAG_UINT64 — payload read as `u64` (unsigned). For *boxed scalars* the other unsigned
//     widths (UInt8/16/32) are zero-extended and boxed as TAG_INT64 (always-positive i64),
//     so for boxed scalars this is the only tag whose payload must be read unsigned. (As a
//     *flat array elem_tag* it likewise marks unsigned-64-bit storage.)
//   TAG_UINT32 — only ever a flat-array elem_tag (raw u32 elements, read unsigned for
//     display/JSON). Boxed UInt32 *scalars* still use TAG_INT64-positive.
//   TAG_FLOAT32 — only ever a flat-array elem_tag (dense f32 storage). Boxed float
//     *scalars* are ALWAYS TAG_FLOAT64 with an f64-bits payload.
//   TAG_PROMISE / TAG_HANDLE — opaque, non-refcounted runtime handles; RC is a no-op.
//   TAG_SHARED — `*const SharedBox`, ATOMIC refcount via lin_shared_retain/release.
//   TAG_STREAM — `*const StreamBox`, refcount inside the box; final drop runs auto-close.
pub use lin_common::tags::{
    TAG_NULL, TAG_BOOL, TAG_INT32, TAG_INT64, TAG_FLOAT32, TAG_FLOAT64, TAG_STR, TAG_OBJECT,
    TAG_ARRAY, TAG_FUNCTION, TAG_UINT8, TAG_INT8, TAG_UINT16, TAG_INT16, TAG_UINT64, TAG_UINT32,
    TAG_PROMISE, TAG_HANDLE, TAG_SHARED, TAG_STREAM, TAG_MAP, TAG_SUMNODE,
    TAG_BIGNUM, TAG_DECIMAL, TAG_TAR_ENTRY, TAG_RECORD,
};

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TaggedVal {
    pub tag: u8,
    pub _pad: [u8; 7],
    pub payload: u64,
}

// Codegen and the runtime hard-code this layout: `lin_box_*`/`build_tagged_val_alloca`
// write `tag` at offset 0 and `payload` at offset 8, and several hot paths
// `copy_nonoverlapping(.., 16)` between a TaggedVal and a LinArrayElem (which must be the
// same shape — see array.rs). A field reorder or size change would silently corrupt every
// boxed value, so pin the layout at compile time.
const _: () = {
    assert!(core::mem::size_of::<TaggedVal>() == 16);
    assert!(core::mem::offset_of!(TaggedVal, tag) == 0);
    assert!(core::mem::offset_of!(TaggedVal, payload) == 8);
};

// ---------------------------------------------------------------------------
// Cached scalar boxes (CPython-style small-value interning).
//
// Boxing a scalar (`lin_box_int32`/`_int64`/`_bool`) is on the hot path of every
// map/filter/reduce callback — `(acc, x) => acc + x` heap-allocates a TaggedVal per
// element. The vast majority of those scalars are small integers (loop indices, counts) and
// booleans. We pre-allocate immutable TaggedVals for those and return pointers into the
// table instead of calling the allocator, eliminating ~one malloc per element.
//
// SAFETY CONTRACT: cached boxes are immutable and must never be freed. The pointer is only
// ever read, copied wholesale (e.g. lin_array_push_tagged copies the 16 bytes), or released
// — and `lin_tagged_release` skips any pointer that lies inside `CACHE` (see is_cached_box).
// Scalar TaggedVals carry no heap payload, so skipping their free leaks nothing.
//
// The table is a compile-time-initialized `static` (TaggedVal is plain data), so it needs no
// runtime/lazy init and is trivially shared across threads (workers/async).

/// Cached integer range `[SMALL_INT_MIN, SMALL_INT_MAX)`. Boxing an int in this range returns
/// an immutable static box instead of allocating — the dominant cost of map/filter/reduce
/// callbacks, whose results (loop indices, counts, byte values, small sums) are usually small.
/// `[-128, 65536)` (65664 entries × 16 B × 2 int caches ≈ 2.0 MB of static data) covers byte
/// values, common loop bounds, UInt16 values, and small arithmetic results; values outside fall
/// back to a fresh heap box. (Measured: widening 256→1024 on the map/filter/reduce benchmark cut
/// mallocs ~24% and runtime ~16%.)
pub const SMALL_INT_MIN: i64 = -128;
/// One past the largest cached integer.
pub const SMALL_INT_MAX: i64 = 65536;
const SMALL_INT_LEN: usize = (SMALL_INT_MAX - SMALL_INT_MIN) as usize;

const fn tv(tag: u8, payload: u64) -> TaggedVal {
    TaggedVal { tag, _pad: [0; 7], payload }
}

const fn build_int_cache() -> [TaggedVal; SMALL_INT_LEN] {
    let mut arr = [tv(TAG_INT32, 0); SMALL_INT_LEN];
    let mut i = 0;
    while i < SMALL_INT_LEN {
        arr[i] = tv(TAG_INT32, (SMALL_INT_MIN + i as i64) as u64);
        i += 1;
    }
    arr
}

// Int32 cache for [SMALL_INT_MIN, SMALL_INT_MAX).
static INT32_CACHE: [TaggedVal; SMALL_INT_LEN] = build_int_cache();
// Int64 cache (separate so the tag is TAG_INT64).
static INT64_CACHE: [TaggedVal; SMALL_INT_LEN] = {
    let mut arr = [tv(TAG_INT64, 0); SMALL_INT_LEN];
    let mut i = 0;
    while i < SMALL_INT_LEN {
        arr[i] = tv(TAG_INT64, (SMALL_INT_MIN + i as i64) as u64);
        i += 1;
    }
    arr
};
// Bool cache: [false, true].
static BOOL_CACHE: [TaggedVal; 2] = [tv(TAG_BOOL, 0), tv(TAG_BOOL, 1)];
// Null is represented as a null pointer, so no cache entry is needed.

/// True if `p` points into one of the immutable cached-box tables and therefore must not be
/// freed. Checked by `lin_tagged_release`.
#[inline]
unsafe fn is_cached_box(p: *const u8) -> bool {
    let in_range = |base: *const TaggedVal, len: usize| {
        let lo = base as usize;
        let hi = lo + len * core::mem::size_of::<TaggedVal>();
        let q = p as usize;
        q >= lo && q < hi
    };
    in_range(INT32_CACHE.as_ptr(), SMALL_INT_LEN)
        || in_range(INT64_CACHE.as_ptr(), SMALL_INT_LEN)
        || in_range(BOOL_CACHE.as_ptr(), 2)
}

/// Public wrapper for `is_cached_box`, used by `lin_tagged_clone` to alias immutable cached
/// scalar boxes instead of allocating a fresh box for them.
#[inline]
pub unsafe fn is_cached_box_pub(p: *const u8) -> bool {
    is_cached_box(p)
}

// ── SMI (Small Integer Inline) helpers — only compiled when feature `smi` is on ──────────────────
//
// Bit0=1 in a `*mut u8`/`*const u8` value means the pointer is a 63-bit signed immediate integer,
// NOT a heap address. All `*mut u8` consumers in this file guard with `is_smi_ptr` before any
// dereference. RC operations are unconditional no-ops for SMI pointers.
//
// Encoding/decoding contract:
//   encode(n: i64) -> *mut u8: ((n as u64) << 1) | 1, cast to *mut u8.
//   decode(p: *mut u8) -> i64: (p as i64) >> 1  (arithmetic, sign-extends).
//   is_smi(p)                : (p as usize) & 1 == 1.
//
// Tag ambiguity: an SMI pointer has bit0=1. We need to know whether it was created by
// lin_box_int32 or lin_box_int64. We use a second sentinel bit (bit1):
//   bit1 = 0 → Int32  (value fits i32, recovered by sign-truncating decode to i32)
//   bit1 = 1 → Int64  (full 63-bit range)
// Encoding for Int32:  ((n as i64 as u64) << 2) | 0b01  (bits[1:0] = 0b01)
// Encoding for Int64:  ((n as u64) << 2) | 0b11  (bits[1:0] = 0b11)
// Decode: shift right 2 (arithmetic), then truncate for Int32 or keep i64 for Int64.
//
// This gives 62-bit range for Int64 ([-2^61, 2^61-1]) and full i32 range for Int32.
// All i32 values fit in 30 bits after the 2-bit tag, so Int32 encoding is lossless.
// Int64 values outside [-2^61, 2^61-1] fall back to heap allocation (the out-of-range path).

/// True if `p` is an inline SMI integer (bit0 = 1). NEVER dereference an SMI pointer.
#[cfg(feature = "smi")]
#[inline(always)]
pub fn is_smi_ptr(p: *const u8) -> bool {
    (p as usize) & 1 == 1
}

/// True if `p` is an inline Int32 SMI (bit0=1, bit1=0).
#[cfg(feature = "smi")]
#[allow(dead_code)]
#[inline(always)]
fn is_smi_int32(p: *const u8) -> bool {
    (p as usize) & 3 == 1
}

/// True if `p` is an inline Int64 SMI (bit0=1, bit1=1).
#[cfg(feature = "smi")]
#[inline(always)]
fn is_smi_int64(p: *const u8) -> bool {
    (p as usize) & 3 == 3
}

/// Public alias for is_smi_int64, used by string.rs's lin_tagged_to_string.
#[cfg(feature = "smi")]
#[inline(always)]
pub fn is_smi_int64_pub(p: *const u8) -> bool {
    is_smi_int64(p)
}

/// Encode an i32 as an SMI pointer. All i32 values fit (30-bit value + 2 tag bits = 32 bits).
/// NOT YET called by lin_box_int32 — infrastructure for Phase 2 (consumer guards required first).
#[cfg(feature = "smi")]
#[allow(dead_code)]
#[inline(always)]
pub(crate) fn smi_encode_i32(v: i32) -> *mut u8 {
    // ((v as i64 as u64) << 2) | 0b01
    (((v as i64 as u64) << 2) | 1) as *mut u8
}

/// Encode an i64 as an SMI pointer, or return None if out of 62-bit range.
/// Range: [-2^61, 2^61 - 1].
/// NOT YET called by lin_box_int64 — infrastructure for Phase 2.
#[cfg(feature = "smi")]
#[allow(dead_code)]
#[inline(always)]
pub(crate) fn smi_encode_i64(v: i64) -> Option<*mut u8> {
    const SMI_I64_MIN: i64 = -(1i64 << 61);
    const SMI_I64_MAX: i64 = (1i64 << 61) - 1;
    if v >= SMI_I64_MIN && v <= SMI_I64_MAX {
        // ((v as u64) << 2) | 0b11
        Some(((v as u64) << 2 | 3) as *mut u8)
    } else {
        None
    }
}

/// Decode the i32 value from an Int32 SMI pointer (bits[1:0] = 0b01).
/// Caller must have verified `is_smi_int32(p)`.
#[cfg(feature = "smi")]
#[allow(dead_code)]
#[inline(always)]
fn smi_decode_i32(p: *const u8) -> i32 {
    ((p as i64) >> 2) as i32
}

/// Decode the i64 value from an Int64 SMI pointer (bits[1:0] = 0b11).
/// Caller must have verified `is_smi_int64(p)`.
#[cfg(feature = "smi")]
#[allow(dead_code)]
#[inline(always)]
fn smi_decode_i64(p: *const u8) -> i64 {
    (p as i64) >> 2
}

/// Get the Lin tag from an SMI pointer (TAG_INT32 or TAG_INT64).
#[cfg(feature = "smi")]
#[allow(dead_code)]
#[inline(always)]
fn smi_tag(p: *const u8) -> u8 {
    if is_smi_int64(p) { TAG_INT64 } else { TAG_INT32 }
}

pub unsafe fn alloc_tagged(tag: u8, payload: u64) -> *mut u8 {
    if smi_stats_state() == 2 {
        let is_int = tag == TAG_INT32 || tag == TAG_INT64 || tag == TAG_UINT64;
        let total = if is_int {
            let prev = SMI_INT_ALLOCS.fetch_add(1, Ordering::Relaxed);
            let other = SMI_OTHER_ALLOCS.load(Ordering::Relaxed);
            prev + 1 + other
        } else {
            let int_allocs = SMI_INT_ALLOCS.load(Ordering::Relaxed);
            let prev = SMI_OTHER_ALLOCS.fetch_add(1, Ordering::Relaxed);
            int_allocs + prev + 1
        };
        // Print a running snapshot every 50M alloc_tagged calls so long-running programs
        // (killed before natural exit) still produce useful numbers.
        if total % 5_000_000 == 0 {
            let int_allocs = SMI_INT_ALLOCS.load(Ordering::Relaxed);
            let other_allocs = SMI_OTHER_ALLOCS.load(Ordering::Relaxed);
            let t = int_allocs + other_allocs;
            let pct = if t > 0 { int_allocs as f64 / t as f64 * 100.0 } else { 0.0 };
            eprintln!("[SMI_STATS @{t}M] int_boxes={int_allocs} ({pct:.1}%) other_boxes={other_allocs}");
        }
    }
    let layout = Layout::new::<TaggedVal>();
    let ptr = alloc(layout);
    if ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    let tv = ptr as *mut TaggedVal;
    (*tv).tag = tag;
    // Zero the padding so the full leading u64 equals `tag` with no garbage in the pad
    // bytes. resolve_lin_str (and similar) discriminate boxed-vs-raw by reading the first
    // 8 bytes as a u64 and comparing to a tag constant; uninitialised pad made that check
    // unreliable (it only worked when the allocator happened to return zeroed memory).
    (*tv)._pad = [0; 7];
    (*tv).payload = payload;
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn lin_box_null() -> *mut u8 {
    std::ptr::null_mut()
}

#[no_mangle]
pub unsafe extern "C" fn lin_box_bool(v: u8) -> *mut u8 {
    // Always cached: only two possible values.
    &BOOL_CACHE[(v != 0) as usize] as *const TaggedVal as *mut u8
}

#[no_mangle]
pub unsafe extern "C" fn lin_box_int32(v: i32) -> *mut u8 {
    // SMI: all i32 values fit in the 30-bit signed field — always emit inline, no heap.
    #[cfg(feature = "smi")]
    {
        if smi_stats_state() == 2 {
            SMI_INT_BOXES.fetch_add(1, Ordering::Relaxed);
        }
        return smi_encode_i32(v) as *mut u8;
    }
    // Without SMI: use cache for common range, heap otherwise.
    #[cfg(not(feature = "smi"))]
    {
        let n = v as i64;
        if n >= SMALL_INT_MIN && n < SMALL_INT_MAX {
            return &INT32_CACHE[(n - SMALL_INT_MIN) as usize] as *const TaggedVal as *mut u8;
        }
        alloc_tagged(TAG_INT32, v as i64 as u64)
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_box_int64(v: i64) -> *mut u8 {
    // SMI: encode inline if in 62-bit range; fall back to heap for rare large values.
    #[cfg(feature = "smi")]
    if let Some(p) = smi_encode_i64(v) {
        if smi_stats_state() == 2 {
            SMI_INT_BOXES.fetch_add(1, Ordering::Relaxed);
        }
        return p as *mut u8;
    }
    // Without SMI (or out-of-range): use cache for common range, heap otherwise.
    if v >= SMALL_INT_MIN && v < SMALL_INT_MAX {
        return &INT64_CACHE[(v - SMALL_INT_MIN) as usize] as *const TaggedVal as *mut u8;
    }
    alloc_tagged(TAG_INT64, v as u64)
}

/// Box a UInt64 value. Tagged TAG_UINT64 so the payload is read back unsigned.
/// Always heap-allocates: the small-int caches are tagged TAG_INT64, so reusing them
/// would lose the unsigned tag. (UInt64 values are rare on the hot path.)
#[no_mangle]
pub unsafe extern "C" fn lin_box_uint64(v: u64) -> *mut u8 {
    alloc_tagged(TAG_UINT64, v)
}

#[no_mangle]
pub unsafe extern "C" fn lin_box_float64(v: f64) -> *mut u8 {
    alloc_tagged(TAG_FLOAT64, v.to_bits())
}

#[no_mangle]
pub unsafe extern "C" fn lin_box_str(p: *mut u8) -> *mut u8 {
    alloc_tagged(TAG_STR, p as u64)
}

#[no_mangle]
pub unsafe extern "C" fn lin_box_array(p: *mut u8) -> *mut u8 {
    alloc_tagged(TAG_ARRAY, p as u64)
}

#[no_mangle]
pub unsafe extern "C" fn lin_box_function(p: *mut u8) -> *mut u8 {
    alloc_tagged(TAG_FUNCTION, p as u64)
}

/// Box a `LinMap*` (the typed index-signature container, ADR-055) as a TaggedVal(TAG_MAP).
#[no_mangle]
pub unsafe extern "C" fn lin_box_map(p: *mut u8) -> *mut u8 {
    alloc_tagged(TAG_MAP, p as u64)
}

/// Box a `*SumNode` (unboxed sum value) by-pointer as a TaggedVal(TAG_SUMNODE) — the
/// keep-packed-through-record-fields store. The SumNode is BORROWED here (the shell is the only
/// fresh +1); the slot's owning reference is supplied by the surrounding container transfer
/// (identical contract to `lin_box_object` for a sealed record). The distinct tag routes the slot's
/// release to `lin_sumnode_release_self`, NOT `lin_object_release` (which would type-confuse the
/// SumNode's offset-4 size as a LinObject len).
#[no_mangle]
pub unsafe extern "C" fn lin_box_sumnode(p: *mut u8) -> *mut u8 {
    alloc_tagged(TAG_SUMNODE, p as u64)
}

/// Box a `*sealed-struct` by-pointer as a TaggedVal(TAG_RECORD) — Stage 6a dynamic-slot widening.
/// The sealed struct carries its own descriptor at offset 8 (`[u32 rc | u32 size | u64 desc_ptr | ...]`),
/// so no separate descriptor argument is needed. The distinct TAG_RECORD tag routes the slot's release
/// to `lin_sealed_release_self` (reads the size from offset 4), NOT `lin_object_release` (which would
/// misinterpret the sealed header).
///
/// RC contract — this RETAINS (+1), and that is DELIBERATELY DIFFERENT from `lin_box_sumnode` (which
/// does NOT retain). The two boxers are used in different ownership contexts:
///   * `lin_box_record` is called only on the COERCE/escape paths (`val j: AnyVal = rec`, NullableRecord
///     coercion — codegen `match.rs`), where the SOURCE struct stays a live owner in its own scope and
///     the box is an ADDITIONAL independent owner → it must take its own +1.
///   * `lin_box_sumnode` is called only on the keep-packed MOVE-into-a-container path
///     (`compile_ir_box_keep_sumnode`), where the IR's `transfer_into_container` MOVES the single +1
///     into the slot and suppresses the source release → the box must NOT add a second +1.
/// (The keep-packed record-into-container MOVE goes through `lin_box_map`/`lin_box_array`, not this
/// function — see `compile_ir_box_keep_packed` — so `lin_box_record` never participates in a move.)
#[no_mangle]
pub unsafe extern "C" fn lin_box_record(p: *mut u8) -> *mut u8 {
    // Retain the sealed struct: the new TaggedVal shell is an additional owner (+1). See the contract
    // note above for why this differs from lin_box_sumnode.
    if !p.is_null() {
        crate::memory::lin_rc_retain(p as *mut u32);
    }
    alloc_tagged(TAG_RECORD, p as u64)
}

/// Stage 6a: read one field from a union-typed TaggedVal box by name, returning an OWNED +1
/// `TaggedVal*` (null = field missing or null source). Handles TAG_MAP and TAG_RECORD:
///   - TAG_MAP → `lin_map_get` (borrowed interior) → `lin_tagged_clone` → owned +1.
///   - TAG_RECORD → `lin_record_get_field` (already owned +1).
///   - anything else (null, scalar, array, SMI, …) → null.
///
/// Used by `sealed_project_from` in codegen when the source is a union-typed box — the
/// generated code calls this once per field and then unboxes + releases the returned owned box.
/// Ownership contract: the returned box is always OWNED by the caller (call `lin_tagged_release`
/// when done). The source box `tv` is BORROWED and untouched (the caller releases it via its own
/// scope).
#[no_mangle]
pub unsafe extern "C" fn lin_union_get_field(tv: *const u8, key: *const crate::string::LinString) -> *mut u8 {
    if tv.is_null() || key.is_null() {
        return std::ptr::null_mut();
    }
    #[cfg(feature = "smi")]
    if is_smi_ptr(tv) {
        return std::ptr::null_mut(); // Integers have no fields.
    }
    let tag = (*(tv as *const TaggedVal)).tag;
    let payload = (*(tv as *const TaggedVal)).payload;
    match tag {
        TAG_MAP => {
            let map = payload as *const crate::map::LinMap;
            if map.is_null() {
                return std::ptr::null_mut();
            }
            // lin_map_get returns a BORROWED interior pointer; clone it into an OWNED box.
            let borrowed = crate::map::lin_map_get(map, key);
            lin_tagged_clone(borrowed as *const u8)
        }
        TAG_RECORD => {
            let sealed = payload as *const u8;
            if sealed.is_null() {
                return std::ptr::null_mut();
            }
            // lin_record_get_field returns OWNED +1 directly.
            crate::sealed::lin_record_get_field(sealed, key)
        }
        _ => std::ptr::null_mut(),
    }
}

/// Get the type tag of a boxed value. Returns TAG_NULL (0) for null pointer.
#[no_mangle]
pub unsafe extern "C" fn lin_get_tag(p: *const u8) -> u8 {
    if p.is_null() {
        return TAG_NULL;
    }
    #[cfg(feature = "smi")]
    if is_smi_ptr(p) {
        return smi_tag(p);
    }
    (*(p as *const TaggedVal)).tag
}

/// Unbox an Int32 value (assumes tag is TAG_INT32).
/// SMI-safe: decode inline integer without dereferencing.
#[no_mangle]
pub unsafe extern "C" fn lin_unbox_int32(p: *const u8) -> i32 {
    #[cfg(feature = "smi")]
    if is_smi_ptr(p) {
        // Both Int32 and Int64 SMIs can be unboxed as i32 (truncating).
        // lin_box_int32 always creates Int32 SMIs; an Int64 SMI here would be a type error.
        return smi_decode_i32(p);
    }
    (*(p as *const TaggedVal)).payload as i32
}

/// Unbox an Int64 value (assumes tag is TAG_INT64).
#[no_mangle]
pub unsafe extern "C" fn lin_unbox_int64(p: *const u8) -> i64 {
    #[cfg(feature = "smi")]
    if is_smi_ptr(p) {
        // An Int32 SMI: sign-extend to i64. An Int64 SMI: full decode.
        return if is_smi_int64(p) { smi_decode_i64(p) } else { smi_decode_i32(p) as i64 };
    }
    (*(p as *const TaggedVal)).payload as i64
}

/// Unbox a UInt64 value (assumes tag is TAG_UINT64).
#[no_mangle]
pub unsafe extern "C" fn lin_unbox_uint64(p: *const u8) -> u64 {
    (*(p as *const TaggedVal)).payload
}

/// Unbox a Float64 value (assumes tag is TAG_FLOAT64).
#[no_mangle]
pub unsafe extern "C" fn lin_unbox_float64(p: *const u8) -> f64 {
    f64::from_bits((*(p as *const TaggedVal)).payload)
}

/// Unbox a Bool value (assumes tag is TAG_BOOL). Returns i8 (0=false, 1=true).
#[no_mangle]
pub unsafe extern "C" fn lin_unbox_bool(p: *const u8) -> u8 {
    (*(p as *const TaggedVal)).payload as u8
}

/// Unbox a pointer payload (Str, Object, Array, Function). A null TaggedVal* is the Json
/// null value — unboxing it yields a null container pointer (safe-access: indexing null
/// propagates null rather than dereferencing).
/// SMI-safe: an SMI pointer is not a container — returns null (defensive; callers should
/// not reach here with an SMI pointer for pointer-typed fields).
#[no_mangle]
pub unsafe extern "C" fn lin_unbox_ptr(p: *const u8) -> *mut u8 {
    if p.is_null() { return std::ptr::null_mut(); }
    #[cfg(feature = "smi")]
    if is_smi_ptr(p) { return std::ptr::null_mut(); }
    (*(p as *const TaggedVal)).payload as *mut u8
}

/// Deep equality for two TaggedVal* values. Returns 1 if equal, 0 if not.
/// Handles null (TAG_NULL), scalars (bool/int/float), strings, objects, and arrays.
/// Either pointer may be null (treated as TAG_NULL).
/// SMI-safe: SMI pointers are decoded before comparison.
#[no_mangle]
pub unsafe extern "C" fn lin_tagged_eq(a: *const u8, b: *const u8) -> u8 {
    // Fast path: two SMI pointers — decode and compare numerically.
    #[cfg(feature = "smi")]
    {
        let a_smi = !a.is_null() && is_smi_ptr(a);
        let b_smi = !b.is_null() && is_smi_ptr(b);
        if a_smi && b_smi {
            // Both inline ints: decode both to i64 and compare (covers Int32==Int64 cross-numeric).
            let av64 = if is_smi_int64(a) { smi_decode_i64(a) } else { smi_decode_i32(a) as i64 };
            let bv64 = if is_smi_int64(b) { smi_decode_i64(b) } else { smi_decode_i32(b) as i64 };
            return (av64 == bv64) as u8;
        }
        if a_smi || b_smi {
            // One SMI, one heap/null — cross-type numeric comparison via f64 widening.
            let (at, ap) = if a_smi {
                (smi_tag(a), if is_smi_int64(a) { smi_decode_i64(a) as u64 } else { smi_decode_i32(a) as i64 as u64 })
            } else if a.is_null() {
                (TAG_NULL, 0)
            } else { ((*( a as *const TaggedVal)).tag, (*(a as *const TaggedVal)).payload) };
            let (bt, bp) = if b_smi {
                (smi_tag(b), if is_smi_int64(b) { smi_decode_i64(b) as u64 } else { smi_decode_i32(b) as i64 as u64 })
            } else if b.is_null() {
                (TAG_NULL, 0)
            } else { ((*(b as *const TaggedVal)).tag, (*(b as *const TaggedVal)).payload) };
            if at == TAG_NULL && bt == TAG_NULL { return 1; }
            if at == TAG_NULL || bt == TAG_NULL { return 0; }
            let at_is_num = (at >= TAG_INT32 && at <= TAG_FLOAT64) || at == TAG_UINT64;
            let bt_is_num = (bt >= TAG_INT32 && bt <= TAG_FLOAT64) || bt == TAG_UINT64;
            if at_is_num && bt_is_num {
                return (tagged_as_f64(at, ap) == tagged_as_f64(bt, bp)) as u8;
            }
            return 0;
        }
    }
    let av = a as *const TaggedVal;
    let bv = b as *const TaggedVal;
    let at = if av.is_null() { TAG_NULL } else { (*av).tag };
    let bt = if bv.is_null() { TAG_NULL } else { (*bv).tag };
    if at == TAG_NULL && bt == TAG_NULL { return 1; }
    if at == TAG_NULL || bt == TAG_NULL { return 0; }
    // Dynamic-object equality: if EITHER side is a map and both sides are dynamic-object-shaped,
    // normalize both to a `LinMap` and compare structurally (order-independent). Covers map==map
    // and the kept-packed cases map==record / map==sumnode.
    let a_dynobj = at == TAG_MAP || at == TAG_RECORD || at == TAG_SUMNODE;
    let b_dynobj = bt == TAG_MAP || bt == TAG_RECORD || bt == TAG_SUMNODE;
    if (at == TAG_MAP || bt == TAG_MAP) && a_dynobj && b_dynobj {
        let am = crate::map::dynamic_to_map(av);
        let bm = crate::map::dynamic_to_map(bv);
        let eq = crate::map::lin_map_eq(am, bm);
        crate::map::lin_map_release(am);
        crate::map::lin_map_release(bm);
        return eq;
    }
    // KEEP-PACKED-THROUGH-RECORD-FIELDS boundary: a kept-packed `*SumNode` (TAG_SUMNODE) or a
    // sealed-record pointer (TAG_RECORD) escaped into a dynamic equality. Normalize both operands
    // to LinMap and compare structurally (order-independent). Transient maps released.
    if at == TAG_SUMNODE || bt == TAG_SUMNODE || at == TAG_RECORD || bt == TAG_RECORD {
        let a_dynobj = at == TAG_MAP || at == TAG_RECORD || at == TAG_SUMNODE;
        let b_dynobj = bt == TAG_MAP || bt == TAG_RECORD || bt == TAG_SUMNODE;
        if !a_dynobj || !b_dynobj { return 0; }
        let am = crate::map::dynamic_to_map(av);
        let bm = crate::map::dynamic_to_map(bv);
        let eq = crate::map::lin_map_eq(am, bm);
        crate::map::lin_map_release(am);
        crate::map::lin_map_release(bm);
        return eq;
    }
    let ap = (*av).payload;
    let bp = (*bv).payload;
    // Cross-numeric equality: compare numeric types by value (Int32 == Int64 if same numeric value).
    let at_is_num = (at >= TAG_INT32 && at <= TAG_FLOAT64) || at == TAG_UINT64;
    let bt_is_num = (bt >= TAG_INT32 && bt <= TAG_FLOAT64) || bt == TAG_UINT64;
    if at_is_num && bt_is_num && at != bt {
        return (tagged_as_f64(at, ap) == tagged_as_f64(bt, bp)) as u8;
    }
    if at != bt { return 0; }
    match at {
        TAG_BOOL => (ap == bp) as u8,
        TAG_INT32 => ((ap as i32) == (bp as i32)) as u8,
        TAG_INT64 => ((ap as i64) == (bp as i64)) as u8,
        TAG_UINT64 => (ap == bp) as u8,
        TAG_FLOAT32 => (f32::from_bits(ap as u32) == f32::from_bits(bp as u32)) as u8,
        TAG_FLOAT64 => (f64::from_bits(ap) == f64::from_bits(bp)) as u8,
        TAG_STR => {
            let as_ptr = ap as *const crate::string::LinString;
            let bs_ptr = bp as *const crate::string::LinString;
            crate::string::lin_string_eq(as_ptr, bs_ptr) as u8
        }
        TAG_ARRAY => {
            let aa = ap as *const crate::array::LinArray;
            let ba = bp as *const crate::array::LinArray;
            crate::array::lin_array_eq(aa, ba)
        }
        _ => (ap == bp) as u8,
    }
}

/// Ordering comparison for two TaggedVal* values. Returns -1, 0, or 1.
/// Compares strings lexicographically; numeric types by value; other types by tag then payload.
/// Either pointer may be null (treated as TAG_NULL, which compares less than everything else).
/// SMI-safe: SMI pointers are decoded before comparison.
#[no_mangle]
pub unsafe extern "C" fn lin_tagged_cmp(a: *const u8, b: *const u8) -> i32 {
    // SMI fast path: decode both inline integers to i64 and compare.
    #[cfg(feature = "smi")]
    {
        let a_smi = !a.is_null() && is_smi_ptr(a);
        let b_smi = !b.is_null() && is_smi_ptr(b);
        if a_smi && b_smi {
            let av64 = if is_smi_int64(a) { smi_decode_i64(a) } else { smi_decode_i32(a) as i64 };
            let bv64 = if is_smi_int64(b) { smi_decode_i64(b) } else { smi_decode_i32(b) as i64 };
            return av64.cmp(&bv64) as i32;
        }
        if a_smi || b_smi {
            // One SMI, one heap/null — decode both to (tag, payload) for the general path.
            let (at, ap) = if a_smi {
                (smi_tag(a), if is_smi_int64(a) { smi_decode_i64(a) as u64 } else { smi_decode_i32(a) as i64 as u64 })
            } else if a.is_null() {
                (TAG_NULL, 0u64)
            } else { ((*(a as *const TaggedVal)).tag, (*(a as *const TaggedVal)).payload) };
            let (bt, bp) = if b_smi {
                (smi_tag(b), if is_smi_int64(b) { smi_decode_i64(b) as u64 } else { smi_decode_i32(b) as i64 as u64 })
            } else if b.is_null() {
                (TAG_NULL, 0u64)
            } else { ((*(b as *const TaggedVal)).tag, (*(b as *const TaggedVal)).payload) };
            // Delegate to the same logic the non-SMI path uses below (numeric widening).
            let at_is_num = (at >= TAG_INT32 && at <= TAG_FLOAT64) || at == TAG_UINT64;
            let bt_is_num = (bt >= TAG_INT32 && bt <= TAG_FLOAT64) || bt == TAG_UINT64;
            if at_is_num && bt_is_num {
                let af = tagged_as_f64(at, ap);
                let bf = tagged_as_f64(bt, bp);
                return af.partial_cmp(&bf).map(|o| o as i32).unwrap_or(0);
            }
            return at.cmp(&bt) as i32;
        }
    }
    let av = a as *const TaggedVal;
    let bv = b as *const TaggedVal;
    let at = if av.is_null() { TAG_NULL } else { (*av).tag };
    let bt = if bv.is_null() { TAG_NULL } else { (*bv).tag };
    let ap = if av.is_null() { 0u64 } else { (*av).payload };
    let bp = if bv.is_null() { 0u64 } else { (*bv).payload };
    match (at, bt) {
        (TAG_STR, TAG_STR) => {
            let as_ptr = ap as *const crate::string::LinString;
            let bs_ptr = bp as *const crate::string::LinString;
            crate::string::lin_string_cmp(as_ptr, bs_ptr)
        }
        (TAG_INT32, TAG_INT32) => (ap as i32).cmp(&(bp as i32)) as i32,
        (TAG_INT64, TAG_INT64) => (ap as i64).cmp(&(bp as i64)) as i32,
        (TAG_UINT64, TAG_UINT64) => ap.cmp(&bp) as i32,
        (TAG_FLOAT32, TAG_FLOAT32) => {
            let af = f32::from_bits(ap as u32);
            let bf = f32::from_bits(bp as u32);
            af.partial_cmp(&bf).map(|o| o as i32).unwrap_or(0)
        }
        (TAG_FLOAT64, TAG_FLOAT64) => {
            let af = f64::from_bits(ap);
            let bf = f64::from_bits(bp);
            af.partial_cmp(&bf).map(|o| o as i32).unwrap_or(0)
        }
        // Mixed numeric: widen to f64
        (a_tag, b_tag)
            if ((a_tag >= TAG_INT32 && a_tag <= TAG_FLOAT64) || a_tag == TAG_UINT64)
                && ((b_tag >= TAG_INT32 && b_tag <= TAG_FLOAT64) || b_tag == TAG_UINT64) =>
        {
            let af = tagged_as_f64(at, ap);
            let bf = tagged_as_f64(bt, bp);
            af.partial_cmp(&bf).map(|o| o as i32).unwrap_or(0)
        }
        _ => at.cmp(&bt) as i32,
    }
}

pub(crate) unsafe fn tagged_as_f64(tag: u8, payload: u64) -> f64 {
    match tag {
        TAG_INT32 => (payload as i32) as f64,
        TAG_INT64 => (payload as i64) as f64,
        TAG_UINT64 => payload as f64,
        TAG_FLOAT32 => f32::from_bits(payload as u32) as f64,
        TAG_FLOAT64 => f64::from_bits(payload),
        _ => 0.0,
    }
}

/// Arithmetic on two boxed numeric TaggedVal*, dispatching on the runtime tags.
/// `op`: 0=Add 1=Sub 2=Mul 3=Div 4=Mod. Returns a freshly boxed numeric TaggedVal*.
///
/// Codegen uses this for `a OP b` when both operands are boxed union/Json values
/// (e.g. fields destructured from an object by a `has` pattern): their concrete
/// numeric type is only known at runtime, so unboxing to a fixed type in codegen
/// would reinterpret a float's bits as an integer. If either operand is a float,
/// the result is Float64; otherwise the widest integer tag present is preserved.
// `extern "C-unwind"`: a non-numeric operand raises `runtime_fault`, which PANICS inside an
// async boundary and must unwind THROUGH this C-ABI frame to the boundary's catch_unwind (a
// plain `extern "C"` fn aborts the process on unwind since Rust 1.81). Outside a boundary it
// `process::exit`s and never unwinds, so the ABI change is invisible there. Mirrors `lin_panic`
// and the array-bounds accessors (`lin_array_get`).
#[no_mangle]
pub unsafe extern "C-unwind" fn lin_tagged_arith(a: *const u8, b: *const u8, op: i32) -> *mut u8 {
    // SMI fast path: decode both inline integers, compute, re-encode.
    #[cfg(feature = "smi")]
    {
        let a_smi = !a.is_null() && is_smi_ptr(a);
        let b_smi = !b.is_null() && is_smi_ptr(b);
        if a_smi || b_smi {
            // Decode both operands to (tag, payload_u64).
            let (at, ap) = if a_smi {
                (smi_tag(a), if is_smi_int64(a) { smi_decode_i64(a) as u64 } else { smi_decode_i32(a) as i64 as u64 })
            } else if a.is_null() {
                (TAG_NULL, 0u64)
            } else { ((*(a as *const TaggedVal)).tag, (*(a as *const TaggedVal)).payload) };
            let (bt, bp) = if b_smi {
                (smi_tag(b), if is_smi_int64(b) { smi_decode_i64(b) as u64 } else { smi_decode_i32(b) as i64 as u64 })
            } else if b.is_null() {
                (TAG_NULL, 0u64)
            } else { ((*(b as *const TaggedVal)).tag, (*(b as *const TaggedVal)).payload) };
            // Non-numeric fault check (same as non-SMI path).
            let tag_is_numeric = |t: u8| (t >= TAG_INT32 && t <= TAG_FLOAT64) || t == TAG_UINT64;
            if !tag_is_numeric(at) || !tag_is_numeric(bt) {
                // Use already-decoded tags (at/bt) directly — av2/bv2 may be SMI pointers,
                // so skip the raw deref and rely on the already-computed at/bt.
                let at2 = at;
                let bt2 = bt;
                let describe = |t: u8| match t {
                    TAG_NULL => "Null", TAG_BOOL => "Bool",
                    TAG_INT32 | TAG_INT64 | TAG_UINT64 => "Int",
                    TAG_FLOAT32 | TAG_FLOAT64 => "Float",
                    TAG_STR => "String", TAG_OBJECT => "Object",
                    TAG_ARRAY => "Array", TAG_FUNCTION => "Function",
                    _ => "non-numeric value",
                };
                let op_name = match op { 0 => "+", 1 => "-", 2 => "*", 3 => "/", 4 => "%", _ => "arithmetic" };
                crate::fault::runtime_fault(&format!(
                    "Runtime error: cannot apply operator '{}' to dynamic AnyVal operands of kind {} and {} \
                     (a missing object key reads as Null — guard with `is`/`!= null` or `has` before arithmetic)",
                    op_name, describe(at2), describe(bt2),
                ));
            }
            let a_is_float = at == TAG_FLOAT32 || at == TAG_FLOAT64;
            let b_is_float = bt == TAG_FLOAT32 || bt == TAG_FLOAT64;
            if a_is_float || b_is_float {
                let af = tagged_as_f64(at, ap);
                let bf = tagged_as_f64(bt, bp);
                let r = match op { 0 => af + bf, 1 => af - bf, 2 => af * bf, 3 => af / bf, 4 => af % bf, _ => 0.0 };
                return lin_box_float64(r);
            }
            let ai = ap as i64;
            let bi = bp as i64;
            if (op == 3 || op == 4) && bi == 0 {
                let op_name = if op == 3 { "division" } else { "modulo" };
                crate::fault::runtime_fault(&format!("Runtime error: {} by zero", op_name));
            }
            let r = match op {
                0 => ai.wrapping_add(bi), 1 => ai.wrapping_sub(bi), 2 => ai.wrapping_mul(bi),
                3 => ai.wrapping_div(bi), 4 => ai.wrapping_rem(bi), _ => 0,
            };
            return if at == TAG_UINT64 || bt == TAG_UINT64 {
                lin_box_uint64(r as u64)
            } else if at == TAG_INT64 || bt == TAG_INT64 {
                lin_box_int64(r) // Returns SMI if in range.
            } else {
                lin_box_int32(r as i32) // Always returns SMI.
            };
        }
    }
    let av = a as *const TaggedVal;
    let bv = b as *const TaggedVal;
    let at = if av.is_null() { TAG_NULL } else { (*av).tag };
    let bt = if bv.is_null() { TAG_NULL } else { (*bv).tag };
    let ap = if av.is_null() { 0u64 } else { (*av).payload };
    let bp = if bv.is_null() { 0u64 } else { (*bv).payload };

    // Dynamic `Json`/union arithmetic must FAULT on a non-numeric operand instead of silently
    // coercing it to 0. The motivating case is a missing object key: `obj["absent"]` reads as
    // `Null` (TAG_NULL), and `obj["present"] + obj["absent"]` previously read the null payload
    // as `0`, so `5 + null` produced `5` and `5 * null` produced `0` — silent wrong results.
    // The static path already rejects `Int32 + Null`; this brings the dynamic-Json path in line
    // (a clear runtime error, NOT JS-style NaN), mirroring array OOB faulting (spec §6.1 / §20.1).
    // (#5: dynamic Json arithmetic with a missing key.)
    let tag_is_numeric =
        |t: u8| (t >= TAG_INT32 && t <= TAG_FLOAT64) || t == TAG_UINT64;
    if !tag_is_numeric(at) || !tag_is_numeric(bt) {
        let describe = |t: u8| match t {
            TAG_NULL => "Null",
            TAG_BOOL => "Bool",
            TAG_INT32 | TAG_INT64 | TAG_UINT64 => "Int",
            TAG_FLOAT32 | TAG_FLOAT64 => "Float",
            TAG_STR => "String",
            TAG_OBJECT => "Object",
            TAG_ARRAY => "Array",
            TAG_FUNCTION => "Function",
            _ => "non-numeric value",
        };
        let op_name = match op {
            0 => "+", 1 => "-", 2 => "*", 3 => "/", 4 => "%", _ => "arithmetic",
        };
        crate::fault::runtime_fault(&format!(
            "Runtime error: cannot apply operator '{}' to dynamic AnyVal operands of kind {} and {} \
             (a missing object key reads as Null — guard with `is`/`!= null` or `has` before arithmetic)",
            op_name, describe(at), describe(bt),
        ));
    }

    let a_is_float = at == TAG_FLOAT32 || at == TAG_FLOAT64;
    let b_is_float = bt == TAG_FLOAT32 || bt == TAG_FLOAT64;

    if a_is_float || b_is_float {
        let af = tagged_as_f64(at, ap);
        let bf = tagged_as_f64(bt, bp);
        let r = match op {
            0 => af + bf,
            1 => af - bf,
            2 => af * bf,
            3 => af / bf,
            4 => af % bf,
            _ => 0.0,
        };
        return lin_box_float64(r);
    }

    // Integer path. Read both payloads as i64 (a UInt64 read as i64 matches the
    // existing two's-complement wrap behaviour) and preserve the widest tag seen.
    let ai = ap as i64;
    let bi = bp as i64;
    // Integer division/modulo by zero must FAULT, exactly like the native (statically-typed) int
    // path (codegen's `emit_int_zero_check`). Routing dynamic-`Json` arithmetic through this helper
    // (#5) must not silently turn `n / 0` into `0` — that would, e.g., swallow a worker-handler
    // div-by-zero that the boundary is supposed to surface as an Error (§24.6.5). Float div-by-zero
    // is left to IEEE (inf/NaN), matching the native float path which has no zero check.
    if (op == 3 || op == 4) && bi == 0 {
        let op_name = if op == 3 { "division" } else { "modulo" };
        crate::fault::runtime_fault(&format!("Runtime error: {} by zero", op_name));
    }
    let r = match op {
        0 => ai.wrapping_add(bi),
        1 => ai.wrapping_sub(bi),
        2 => ai.wrapping_mul(bi),
        3 => ai.wrapping_div(bi),
        4 => ai.wrapping_rem(bi),
        _ => 0,
    };
    if at == TAG_UINT64 || bt == TAG_UINT64 {
        lin_box_uint64(r as u64)
    } else if at == TAG_INT64 || bt == TAG_INT64 {
        lin_box_int64(r)
    } else {
        lin_box_int32(r as i32)
    }
}

/// Dynamic length dispatch: returns length of string, array, or object TaggedVal*.
/// Returns 0 for null/bool/numeric types (no meaningful length).
/// SMI-safe: inline integers have no meaningful length — returns 0.
#[no_mangle]
pub unsafe extern "C" fn lin_length_dyn(p: *const u8) -> i32 {
    if p.is_null() {
        return 0;
    }
    #[cfg(feature = "smi")]
    if is_smi_ptr(p) {
        return 0; // Inline integers have no length.
    }
    let tv = p as *const TaggedVal;
    let tag = (*tv).tag;
    let payload = (*tv).payload as *mut u8;
    match tag {
        TAG_STR => crate::string::lin_string_length(payload as *const crate::string::LinString),
        TAG_ARRAY => {
            let n = crate::array::lin_array_length(payload as *const crate::array::LinArray);
            n as i32
        }
        TAG_MAP => {
            let n = crate::map::lin_map_length(payload as *const crate::map::LinMap);
            n as i32
        }
        _ => 0,
    }
}

/// Free ONLY the TaggedVal box allocation, WITHOUT touching its inner heap payload.
///
/// Used by the owning var-cell/global model when a transient box (e.g. the result of boxing
/// a freshly-allocated array/object via Coerce on the way into a union cell) has had its
/// inner payload's ownership transferred elsewhere (the cell clones the box, retaining the
/// inner; the inner's original +1 is released separately via the raw value's scope-exit
/// release). Calling `lin_tagged_release` on such a box would double-free the inner, so we
/// reclaim only the 16-byte box shell here.
///
/// Null-safe, cached-box-safe (immutable static scalar boxes are never freed), and
/// SMI-safe (inline integer pointers are never heap-allocated).
#[no_mangle]
pub unsafe extern "C" fn lin_tagged_free_box(p: *mut u8) {
    if p.is_null() {
        return;
    }
    #[cfg(feature = "smi")]
    if is_smi_ptr(p) {
        return; // SMI integers are not heap-allocated — nothing to free.
    }
    if is_cached_box(p) {
        return;
    }
    std::alloc::dealloc(p, std::alloc::Layout::new::<TaggedVal>());
}

/// Free the `TaggedVal*` box shell of `p`, but ONLY if `p` is a DIFFERENT pointer than `other`.
/// Used by `for`/`while` to reclaim a per-iteration element box shell while avoiding a
/// double-free when the callback returned (an alias of) that very box: in that case the loop's
/// separate full release of the return box already reclaimed it, so freeing the shell again here
/// would double-free. Frees only the shell (never the inner payload); null/cached-box safe.
#[no_mangle]
pub unsafe extern "C" fn lin_tagged_free_box_if_distinct(p: *mut u8, other: *mut u8) {
    if p == other {
        return;
    }
    lin_tagged_free_box(p);
}

/// FULLY release a `TaggedVal*` box (inner heap payload + shell), but ONLY when `p` is a DISTINCT
/// pointer from `other`. The full-release counterpart of `lin_tagged_free_box_if_distinct`: used by
/// `for`/`while` to reclaim a per-iteration element box that `lin_array_get_tagged` returned as a
/// fresh +1 (with the inner heap payload RETAINED), while avoiding a double-free when the callback
/// returned (an alias of) that very box — in which case the loop's separate full release of the
/// return box already reclaimed it. Releasing only the shell here (the old behaviour) leaked the
/// retained inner heap value of every heap-bearing element (Object/String/Array) — the
/// String-packed-sealed `for` leak AND the pre-existing genuine `Json[]`-of-objects `for` leak.
/// Null/cached-box safe (via `lin_tagged_release`).
#[no_mangle]
pub unsafe extern "C" fn lin_tagged_release_if_distinct(p: *mut u8, other: *mut u8) {
    if p == other {
        return;
    }
    lin_tagged_release(p);
}

/// Release a TaggedVal*: release the pointed-to heap value (if pointer type), then free the box.
/// Safe to call with null (treated as null — no-op).
/// SMI-safe: inline integer pointers are never heap-allocated — release is a no-op.
#[no_mangle]
pub unsafe extern "C" fn lin_tagged_release(p: *mut u8) {
    if p.is_null() {
        return;
    }
    #[cfg(feature = "smi")]
    if is_smi_ptr(p) {
        return; // Inline integer — no heap allocation to free.
    }
    let tv = p as *mut TaggedVal;
    let tag = (*tv).tag;
    let payload = (*tv).payload;
    // Release the pointed-to value for pointer-typed payloads.
    match tag {
        TAG_STR => crate::string::lin_string_release(payload as *mut crate::string::LinString),
        TAG_ARRAY => crate::array::lin_array_release(payload as *mut crate::array::LinArray),
        TAG_MAP => crate::map::lin_map_release(payload as *mut crate::map::LinMap),
        // KEEP-PACKED sum node in a record-field slot: dispatch to the SumNode self-release (reads
        // its own size from the header), NOT lin_object_release (which would read the SumNode's
        // offset-4 size as a LinObject len → type-confusion). The matching retain bumps offset-0 RC.
        TAG_SUMNODE => crate::sumnode::lin_sumnode_release_self(payload as *mut u8),
        // Stage 6a: TAG_RECORD wraps a sealed-struct by-pointer. Release via sealed_release_self
        // (reads size from offset 4), exactly mirroring TAG_SUMNODE. The sealed struct's header
        // shape ([u32 rc | u32 size | u64 desc_ptr | payload]) is the same as a SumNode's header
        // so the generic `lin_rc_retain` works for the retain side too.
        TAG_RECORD => crate::sealed::lin_sealed_release_self(payload as *mut u8),
        TAG_SHARED => crate::shared::lin_shared_release_box(payload as *const u8),
        TAG_STREAM => crate::stream::lin_stream_release_box(payload as *const u8),
        // Opaque arbitrary-precision/decimal handles (std/bignum, std/decimal): refcounted Rust
        // boxes whose final drop frees the wrapped num value. Mirror of the TAG_STREAM arm.
        TAG_BIGNUM => crate::bignum::lin_bignum_release_box(payload as *const u8),
        TAG_DECIMAL => crate::decimal::lin_decimal_release_box(payload as *const u8),
        TAG_TAR_ENTRY => crate::stream::lin_tar_entry_release_box(payload as *const u8),
        _ => {} // Scalars (null, bool, int, float) have no heap payload.
    }
    // Cached scalar boxes (small ints, bools) are immutable statics — never free them.
    if is_cached_box(p) {
        return;
    }
    // Free the TaggedVal box itself.
    std::alloc::dealloc(p, std::alloc::Layout::new::<TaggedVal>());
}

/// Retain the heap-allocated payload of a TaggedVal (increment refcount). Used when copying a
/// TaggedVal into a map/array slot so the new owner has a reference. Moved from object.rs
/// in Cluster D: TAG_OBJECT arm dropped (no producers after Phase 3).
pub(crate) unsafe fn retain_tagged_payload(tv: &TaggedVal) {
    let payload = tv.payload;
    match tv.tag {
        TAG_STR => {
            crate::string::lin_string_inc_ref(payload as *mut crate::string::LinString);
        }
        TAG_ARRAY => {
            let a = payload as *mut crate::array::LinArray;
            if !a.is_null() && (*a).refcount < crate::string::IMMORTAL_RC { (*a).refcount += 1; }
        }
        TAG_MAP => {
            let m = payload as *mut crate::map::LinMap;
            if !m.is_null() && (*m).refcount < crate::string::IMMORTAL_RC { (*m).refcount += 1; }
        }
        TAG_SUMNODE => {
            let s = payload as *mut u32;
            if !s.is_null() && *s < crate::string::IMMORTAL_RC { *s += 1; }
        }
        TAG_RECORD => {
            let s = payload as *mut u32;
            if !s.is_null() && *s < crate::string::IMMORTAL_RC { *s += 1; }
        }
        TAG_FUNCTION => {
            let c = payload as *mut u32;
            if !c.is_null() {
                crate::memory::lin_rc_retain(c);
            }
        }
        TAG_SHARED => {
            crate::shared::lin_shared_retain_box(payload as *const u8);
        }
        TAG_STREAM => {
            crate::stream::lin_stream_retain_box(payload as *const u8);
        }
        TAG_BIGNUM => {
            crate::bignum::lin_bignum_retain_box(payload as *const u8);
        }
        TAG_DECIMAL => {
            crate::decimal::lin_decimal_retain_box(payload as *const u8);
        }
        TAG_TAR_ENTRY => {
            crate::stream::lin_tar_entry_retain_box(payload as *const u8);
        }
        _ => {} // scalars (and retired TAG_OBJECT = 7): no heap payload to retain
    }
}

/// Public wrapper for retain_tagged_payload, used by array.rs and map.rs.
pub unsafe fn retain_tagged_payload_pub(tv: &TaggedVal) {
    retain_tagged_payload(tv);
}

/// Public wrapper for release_tagged_payload, used by map.rs (the typed-map container reuses the
/// exact object value RC discipline; see ADR-055).
pub unsafe fn release_tagged_payload_pub(tv: &TaggedVal) {
    // release_tagged_payload is the body of lin_tagged_release (without the shell free).
    // For simplicity, box the value, release it (which frees the payload), then rebuild — but
    // this would double-free the shell. Instead: inline the payload-only release here.
    let payload = tv.payload;
    match tv.tag {
        TAG_STR => crate::string::lin_string_release(payload as *mut crate::string::LinString),
        TAG_ARRAY => crate::array::lin_array_release(payload as *mut crate::array::LinArray),
        TAG_MAP => crate::map::lin_map_release(payload as *mut crate::map::LinMap),
        TAG_SUMNODE => crate::sumnode::lin_sumnode_release_self(payload as *mut u8),
        TAG_RECORD => crate::sealed::lin_sealed_release_self(payload as *mut u8),
        TAG_FUNCTION => crate::memory::lin_closure_release(payload as *mut u8),
        TAG_SHARED => crate::shared::lin_shared_release_box(payload as *const u8),
        TAG_STREAM => crate::stream::lin_stream_release_box(payload as *const u8),
        TAG_BIGNUM => crate::bignum::lin_bignum_release_box(payload as *const u8),
        TAG_DECIMAL => crate::decimal::lin_decimal_release_box(payload as *const u8),
        TAG_TAR_ENTRY => crate::stream::lin_tar_entry_release_box(payload as *const u8),
        _ => {} // scalars (and retired TAG_OBJECT = 7): no heap payload
    }
}

/// Retain the heap payload of a boxed TaggedVal* (tag-aware). Null-safe.
/// SMI-safe: inline integer pointers carry no heap payload — retain is a no-op.
#[no_mangle]
pub unsafe extern "C" fn lin_tagged_retain(p: *const u8) {
    if p.is_null() {
        return;
    }
    #[cfg(feature = "smi")]
    if is_smi_ptr(p) {
        return; // Inline integer — no heap payload to retain.
    }
    retain_tagged_payload(&*(p as *const TaggedVal));
}

/// Clone a boxed TaggedVal*: allocate a FRESH TaggedVal box copying the tag+payload and retain
/// the inner heap payload (if any). Returns an independently-owned box.
/// Null-safe. Cached scalar boxes are returned as-is (immutable statics).
/// SMI-safe: inline integer pointers are returned as-is (they are their own value — no heap to clone).
#[no_mangle]
pub unsafe extern "C" fn lin_tagged_clone(p: *const u8) -> *mut u8 {
    if p.is_null() {
        return std::ptr::null_mut();
    }
    #[cfg(feature = "smi")]
    if is_smi_ptr(p) {
        return p as *mut u8; // SMI integers are immediate values — return as-is.
    }
    if is_cached_box_pub(p) {
        return p as *mut u8;
    }
    let src = &*(p as *const TaggedVal);
    retain_tagged_payload(src);
    alloc_tagged(src.tag, src.payload)
}

// ── Cluster D: dispatch helpers moved from object.rs ─────────────────────────────────────────────
// TAG_OBJECT (= 7) has no producers after Phase 3; all arms below dispatch TAG_MAP/TAG_RECORD.

/// Return a `String[]` of the keys of a boxed object/map/record value.
/// Dispatches on the runtime tag: TAG_MAP → `lin_map_keys` (O(1)), TAG_RECORD → materialize to
/// LinMap then return its keys, else return an empty array.
/// SMI-safe: inline integers have no keys — returns empty array.
#[no_mangle]
pub unsafe extern "C" fn lin_tagged_keys(tv: *const u8) -> *mut crate::array::LinArray {
    if tv.is_null() {
        return crate::array::lin_array_alloc(0);
    }
    #[cfg(feature = "smi")]
    if is_smi_ptr(tv) {
        return crate::array::lin_array_alloc(0); // Integers have no keys.
    }
    let src = &*(tv as *const TaggedVal);
    match src.tag {
        TAG_MAP => {
            crate::map::lin_map_keys(src.payload as *const crate::map::LinMap)
        }
        TAG_RECORD => {
            let sealed = src.payload as *mut u8;
            if sealed.is_null() {
                return crate::array::lin_array_alloc(0);
            }
            let named_desc = *((sealed.add(16)) as *const *const u8);
            let mat = crate::sealed::materialize_sealed_to_map_pub(sealed, named_desc);
            let arr = crate::map::lin_map_keys(mat as *const crate::map::LinMap);
            crate::map::lin_map_release(mat);
            arr
        }
        _ => crate::array::lin_array_alloc(0),
    }
}

/// Check if a boxed value (TaggedVal*) has a given string key. Returns 0/1.
/// Dispatches: TAG_MAP → `lin_map_has`, TAG_RECORD → materialize + check, else 0.
/// SMI-safe: inline integers have no fields — returns 0.
#[no_mangle]
pub unsafe extern "C" fn lin_value_has_field(tagged: *const u8, key: *const crate::string::LinString) -> u8 {
    if tagged.is_null() { return 0; }
    #[cfg(feature = "smi")]
    if is_smi_ptr(tagged) { return 0; }
    let tv = tagged as *const TaggedVal;
    match (*tv).tag {
        TAG_MAP => {
            let map = (*tv).payload as *const crate::map::LinMap;
            if map.is_null() { 0 } else { crate::map::lin_map_has(map, key) }
        }
        TAG_RECORD => {
            let sealed = (*tv).payload as *mut u8;
            if sealed.is_null() { return 0; }
            let named_desc = *((sealed.add(16)) as *const *const u8);
            let mat = crate::sealed::materialize_sealed_to_map_pub(sealed, named_desc);
            let result = if mat.is_null() { 0 } else { crate::map::lin_map_has(mat, key) };
            crate::map::lin_map_release(mat);
            result
        }
        _ => 0,
    }
}

/// Check if a boxed value (TaggedVal*) is an array of length `n` (exact) or `>= n` when
/// `at_least != 0`. Returns 0 for null/non-array values. Branchless helper for the IR
/// array-pattern lowering.
/// SMI-safe: inline integers are not arrays — returns 0.
#[no_mangle]
pub unsafe extern "C" fn lin_value_array_len_check(tagged: *const u8, n: u64, at_least: u8) -> u8 {
    if tagged.is_null() { return 0; }
    #[cfg(feature = "smi")]
    if is_smi_ptr(tagged) { return 0; }
    let tv = &*(tagged as *const TaggedVal);
    if tv.tag != TAG_ARRAY { return 0; }
    let arr = tv.payload as *const crate::array::LinArray;
    if arr.is_null() { return 0; }
    let len = (*arr).len as u64;
    let ok = if at_least != 0 { len >= n } else { len == n };
    ok as u8
}

#[cfg(test)]
mod smi_tests {
    use super::*;

    /// Verify SMI encoding/decoding round-trip for Int32 values.
    #[cfg(feature = "smi")]
    #[test]
    fn smi_int32_encode_decode_roundtrip() {
        for v in [i32::MIN, -1000, -128, -1, 0, 1, 42, 127, 1023, 1024, i32::MAX] {
            let p = smi_encode_i32(v);
            // Must be detected as SMI.
            assert!(is_smi_ptr(p as *const u8), "encoded i32 {v} must be SMI");
            // Must NOT be detected as Int64 SMI.
            assert!(!is_smi_int64(p as *const u8), "encoded i32 {v} must not be Int64 SMI");
            // Decode must round-trip.
            let got = smi_decode_i32(p as *const u8);
            assert_eq!(got, v, "i32 round-trip failed for {v}");
        }
    }

    /// Verify SMI encoding/decoding round-trip for Int64 values (in-range).
    #[cfg(feature = "smi")]
    #[test]
    fn smi_int64_encode_decode_roundtrip() {
        const SMI_I64_MIN: i64 = -(1i64 << 61);
        const SMI_I64_MAX: i64 = (1i64 << 61) - 1;
        for v in [SMI_I64_MIN, -1_000_000i64, -1, 0, 1, 42, 1_000_000, SMI_I64_MAX] {
            let p = smi_encode_i64(v).expect("in-range i64 must encode as SMI");
            assert!(is_smi_ptr(p as *const u8), "encoded i64 {v} must be SMI");
            assert!(is_smi_int64(p as *const u8), "encoded i64 {v} must be Int64 SMI");
            let got = smi_decode_i64(p as *const u8);
            assert_eq!(got, v, "i64 round-trip failed for {v}");
        }
    }

    /// Verify that out-of-range Int64 values return None (fall back to heap).
    #[cfg(feature = "smi")]
    #[test]
    fn smi_int64_out_of_range_is_none() {
        const SMI_I64_MIN: i64 = -(1i64 << 61);
        const SMI_I64_MAX: i64 = (1i64 << 61) - 1;
        assert!(smi_encode_i64(SMI_I64_MAX + 1).is_none(), "one past max must not encode");
        assert!(smi_encode_i64(SMI_I64_MIN - 1).is_none(), "one before min must not encode");
        assert!(smi_encode_i64(i64::MAX).is_none(), "i64::MAX must not encode as SMI");
        assert!(smi_encode_i64(i64::MIN).is_none(), "i64::MIN must not encode as SMI");
    }

    /// SMI retain/release/free_box must be no-ops on SMI pointers (no crash, no double-free).
    /// Uses smi_encode_i32 directly since lin_box_int32 still returns heap pointers in this
    /// "infrastructure-only" slice — the box functions are not yet wired to emit SMI.
    #[cfg(feature = "smi")]
    #[test]
    fn smi_retain_release_are_noops() {
        unsafe {
            let p = smi_encode_i32(42) as *mut u8;
            assert!(is_smi_ptr(p as *const u8));
            lin_tagged_retain(p as *const u8); // must not crash
            lin_tagged_release(p);             // must not crash/free
            lin_tagged_free_box(p);            // must not crash/free
            // Same encoding for same value.
            let p2 = smi_encode_i32(42) as *mut u8;
            assert_eq!(p, p2, "same i32 must encode to same SMI pointer");
        }
    }

    /// SMI clone must return the same pointer (no heap alloc).
    #[cfg(feature = "smi")]
    #[test]
    fn smi_clone_returns_same_pointer() {
        unsafe {
            let p = smi_encode_i32(100) as *mut u8;
            assert!(is_smi_ptr(p as *const u8));
            let q = lin_tagged_clone(p as *const u8);
            assert_eq!(p, q, "SMI clone must return the identical pointer");
        }
    }

    /// lin_get_tag returns the correct tag for SMI integer pointers.
    #[cfg(feature = "smi")]
    #[test]
    fn smi_get_tag() {
        unsafe {
            let p32 = smi_encode_i32(5) as *const u8;
            assert_eq!(lin_get_tag(p32), TAG_INT32, "Int32 SMI must carry TAG_INT32");
            let p64 = smi_encode_i64(5i64).unwrap() as *const u8;
            assert_eq!(lin_get_tag(p64), TAG_INT64, "Int64 SMI must carry TAG_INT64");
        }
    }

    /// lin_unbox_int32 / lin_unbox_int64 must round-trip SMI values.
    #[cfg(feature = "smi")]
    #[test]
    fn smi_unbox_roundtrip() {
        unsafe {
            for v in [-1000i32, -128, -1, 0, 1, 42, 1023, 1024, i32::MAX, i32::MIN] {
                let p = smi_encode_i32(v) as *const u8;
                assert_eq!(lin_unbox_int32(p), v, "i32 unbox round-trip for {v}");
            }
            for v in [-1_000_000i64, -1, 0, 1, 42, 1_000_000] {
                let p = smi_encode_i64(v).unwrap() as *const u8;
                assert_eq!(lin_unbox_int64(p), v, "i64 unbox round-trip for {v}");
            }
        }
    }

    /// SMI equality comparisons must work correctly.
    #[cfg(feature = "smi")]
    #[test]
    fn smi_eq_comparisons() {
        unsafe {
            let a = smi_encode_i32(42) as *const u8;
            let b = smi_encode_i32(42) as *const u8;
            let c = smi_encode_i32(43) as *const u8;
            assert_eq!(lin_tagged_eq(a, b), 1, "42 == 42 must be 1");
            assert_eq!(lin_tagged_eq(a, c), 0, "42 == 43 must be 0");
            assert_eq!(lin_tagged_eq(a, std::ptr::null()), 0, "42 == null must be 0");
            // Cross-type: Int32 SMI == Int64 SMI with same value.
            let d = smi_encode_i64(42i64).unwrap() as *const u8;
            assert_eq!(lin_tagged_eq(a, d), 1, "Int32(42) == Int64(42) must be 1");
        }
    }

    /// SMI ordering comparisons must work correctly.
    #[cfg(feature = "smi")]
    #[test]
    fn smi_cmp_comparisons() {
        unsafe {
            let a = smi_encode_i32(10) as *const u8;
            let b = smi_encode_i32(20) as *const u8;
            assert_eq!(lin_tagged_cmp(a, b), -1, "10 < 20");
            assert_eq!(lin_tagged_cmp(b, a), 1, "20 > 10");
            assert_eq!(lin_tagged_cmp(a, a), 0, "10 == 10");
        }
    }

    /// SMI zero must not be confused with NULL.
    #[cfg(feature = "smi")]
    #[test]
    fn smi_zero_is_not_null() {
        unsafe {
            let p = smi_encode_i32(0) as *mut u8;
            assert!(!p.is_null(), "SMI(0) must not be a null pointer");
            assert!(is_smi_ptr(p as *const u8), "SMI(0) must be detected as SMI");
            assert_eq!(lin_get_tag(p as *const u8), TAG_INT32, "SMI(0) must carry TAG_INT32");
            assert_eq!(lin_unbox_int32(p as *const u8), 0);
        }
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[test]
    fn small_ints_are_cached_and_roundtrip() {
        unsafe {
            for v in [SMALL_INT_MIN as i32, -1, 0, 1, 42, (SMALL_INT_MAX - 1) as i32] {
                let p = lin_box_int32(v);
                assert_eq!(lin_get_tag(p), TAG_INT32);
                assert_eq!(lin_unbox_int32(p), v);
                #[cfg(not(feature = "smi"))]
                {
                    assert!(is_cached_box(p), "in-range int {v} should be cached (non-SMI)");
                    assert_eq!(p, lin_box_int32(v), "same value should return same cached pointer");
                }
                #[cfg(feature = "smi")]
                {
                    assert!(is_smi_ptr(p), "in-range int {v} should be SMI");
                    assert_eq!(p, lin_box_int32(v), "same i32 value encodes to same SMI pointer");
                }
                lin_tagged_release(p);
                // After release, boxing the same value again must still work.
                let p2 = lin_box_int32(v);
                assert_eq!(lin_unbox_int32(p2), v, "int {v} must still unbox correctly after release");
                lin_tagged_release(p2);
            }
        }
    }

    #[test]
    fn out_of_range_ints_allocate_fresh() {
        unsafe {
            let v = SMALL_INT_MAX as i32; // one past the cache
            let p = lin_box_int32(v);
            assert_eq!(lin_unbox_int32(p), v);
            #[cfg(feature = "smi")]
            assert!(is_smi_ptr(p), "out-of-cache i32 must still be SMI (full i32 range is inline)");
            #[cfg(not(feature = "smi"))]
            {
                assert!(!is_cached_box(p), "out-of-range int should be heap-allocated");
                lin_tagged_release(p); // frees the heap box
            }
            #[cfg(feature = "smi")]
            lin_tagged_release(p); // no-op for SMI
        }
    }

    // Regression: the combinator element-reclaim leak (ADR-063 Stage 3b read path). `for`/`while`/
    // `reduce` over a heap-bearing tagged array read each element via `lin_array_get_tagged`, which
    // returns a fresh box WITH its inner heap payload RETAINED (+1). The old reclaim freed only the
    // box SHELL (`lin_tagged_free_box_if_distinct`), leaking the retained inner of every heap element
    // — the String-packed-sealed `for` leak AND the pre-existing genuine `Json[]`-of-objects leak.
    // `lin_tagged_release_if_distinct` is the full-release fix: it releases inner + shell, but only
    // when the box is DISTINCT from the loop's discarded callback-return box (else that box's own
    // release already reclaimed it — guarding the double-free).
    #[test]
    fn release_if_distinct_reclaims_inner_when_distinct() {
        unsafe {
            // A heap String with rc bumped to 2 so we can observe the inner decrement without freeing.
            let s = crate::string::lin_string_from_bytes(b"hello".as_ptr(), 5);
            assert_eq!((*s).refcount, 1);
            crate::memory::lin_rc_retain(s as *mut u32); // rc = 2
            assert_eq!((*s).refcount, 2);
            // Box it TAG_STR (the box does not retain — it owns the existing +1).
            let elem = lin_box_str(s as *mut u8);
            // Some OTHER distinct pointer (a separate box / SMI) standing in for the callback-return box.
            // Use lin_box_float64 to get a guaranteed heap box that is NEVER an SMI pointer.
            let other = lin_box_float64(3.14);
            assert_ne!(elem, other);
            // Full release of the DISTINCT element box: frees the shell AND releases the inner String
            // (rc 2 → 1). A shell-only free would have left rc at 2 (the leak).
            lin_tagged_release_if_distinct(elem, other);
            assert_eq!((*s).refcount, 1, "inner String must be released (was the shell-only leak)");
            // Clean up the standins.
            lin_tagged_release(other);
            crate::string::lin_string_release(s); // rc 1 → 0, frees
        }
    }

    #[test]
    fn release_if_distinct_is_noop_when_aliased() {
        unsafe {
            // When the element box ALIASES the (already-released) return box, releasing it again would
            // double-free. The guard makes it a no-op.
            let s = crate::string::lin_string_from_bytes(b"world".as_ptr(), 5);
            let elem = lin_box_str(s as *mut u8);
            // p == other → no-op: neither the shell nor the inner are touched.
            lin_tagged_release_if_distinct(elem, elem);
            assert_eq!((*s).refcount, 1, "aliased release must be a no-op (no double-free)");
            // Now reclaim for real.
            lin_tagged_release(elem); // releases inner (rc 1 → 0) + shell
        }
    }

    #[test]
    fn release_if_distinct_scalar_box_degrades_to_shell_free() {
        unsafe {
            // A flat-scalar element box has no heap inner, so a full release just frees the shell.
            // With SMI off: use out-of-range ints that heap-allocate. With SMI on: use floats (always heap).
            #[cfg(not(feature = "smi"))]
            {
                let elem = lin_box_int64(SMALL_INT_MAX);
                let other = lin_box_int64(SMALL_INT_MAX + 1);
                assert!(!is_cached_box(elem) && !is_cached_box(other));
                lin_tagged_release_if_distinct(elem, other); // frees elem's shell (no inner)
                lin_tagged_release(other);
            }
            #[cfg(feature = "smi")]
            {
                // SMI path: use float boxes (always heap-allocated, never SMI).
                let elem = lin_box_float64(1.0);
                let other = lin_box_float64(2.0);
                lin_tagged_release_if_distinct(elem, other); // frees elem's shell (no inner)
                lin_tagged_release(other);
            }
        }
    }

    #[test]
    fn bools_are_cached() {
        unsafe {
            let t = lin_box_bool(1);
            let f = lin_box_bool(0);
            assert!(is_cached_box(t) && is_cached_box(f));
            assert_ne!(t, f);
            assert_eq!(lin_get_tag(t), TAG_BOOL);
            assert_eq!(t, lin_box_bool(7)); // any non-zero → the `true` cache entry
            lin_tagged_release(t);
            lin_tagged_release(f);
        }
    }

    #[test]
    fn int64_cache_uses_int64_tag() {
        unsafe {
            let p = lin_box_int64(5);
            assert_eq!(lin_get_tag(p), TAG_INT64);
            assert_eq!(lin_unbox_int64(p), 5);
            #[cfg(feature = "smi")]
            assert!(is_smi_ptr(p), "int64(5) must be SMI when feature is on");
            #[cfg(not(feature = "smi"))]
            assert!(is_cached_box(p));
            lin_tagged_release(p);
        }
    }

    #[test]
    fn uint64_box_tag_and_roundtrip() {
        unsafe {
            for v in [0u64, 1, 42, i64::MAX as u64, (i64::MAX as u64) + 1, u64::MAX] {
                let p = lin_box_uint64(v);
                assert_eq!(lin_get_tag(p), TAG_UINT64, "uint64 box must carry TAG_UINT64");
                assert_eq!(lin_unbox_uint64(p), v, "uint64 unbox round-trip");
                // Never cached — always heap-allocated; release frees it (no double-free panic).
                assert!(!is_cached_box(p));
                lin_tagged_release(p);
            }
        }
    }

    #[test]
    fn uint64_max_displays_unsigned() {
        unsafe {
            let p = lin_box_uint64(u64::MAX) as *const crate::tagged::TaggedVal;
            let s = crate::string::lin_tagged_to_string(p);
            assert_eq!((*s).as_str(), "18446744073709551615");
            crate::string::lin_string_release(s);
            // Release the box.
            lin_tagged_release(lin_box_uint64(u64::MAX));
        }
    }

    #[test]
    fn uint64_eq_and_cmp_high_bit() {
        unsafe {
            // u64 >= 2^63 must compare as a large positive number, not negative.
            let big = lin_box_uint64(u64::MAX);
            let small = lin_box_uint64(1);
            assert_eq!(lin_tagged_eq(big, big), 1);
            assert_eq!(lin_tagged_eq(big, small), 0);
            assert_eq!(lin_tagged_cmp(big, small), 1, "u64::MAX > 1");
            assert_eq!(lin_tagged_cmp(small, big), -1, "1 < u64::MAX");
            // Cross-type: a UInt64 small value equals an Int32 of the same value.
            let i = lin_box_int32(1);
            assert_eq!(lin_tagged_eq(small, i), 1, "UInt64(1) == Int32(1)");
            lin_tagged_release(big);
            lin_tagged_release(small);
        }
    }
}
