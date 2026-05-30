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

use std::alloc::{Layout, alloc};

pub const TAG_NULL: u8 = 0;
pub const TAG_BOOL: u8 = 1;
pub const TAG_INT32: u8 = 2;
pub const TAG_INT64: u8 = 3;
pub const TAG_FLOAT32: u8 = 4;
pub const TAG_FLOAT64: u8 = 5;
pub const TAG_STR: u8 = 6;
pub const TAG_OBJECT: u8 = 7;
pub const TAG_ARRAY: u8 = 8;
pub const TAG_FUNCTION: u8 = 9;
pub const TAG_UINT8: u8 = 10;
pub const TAG_INT8: u8 = 11;
pub const TAG_UINT16: u8 = 12;
pub const TAG_INT16: u8 = 13;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct TaggedVal {
    pub tag: u8,
    pub _pad: [u8; 7],
    pub payload: u64,
}

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
/// `[-128, 1024)` (1152 entries × 16 B × 2 int caches ≈ 37 KB of static data) covers byte
/// values, common loop bounds, and small arithmetic results; values outside fall back to a
/// fresh heap box. (Measured: widening 256→1024 on the map/filter/reduce benchmark cut mallocs
/// ~24% and runtime ~16%.)
pub const SMALL_INT_MIN: i64 = -128;
/// One past the largest cached integer.
pub const SMALL_INT_MAX: i64 = 1024;
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

pub unsafe fn alloc_tagged(tag: u8, payload: u64) -> *mut u8 {
    let layout = Layout::new::<TaggedVal>();
    let ptr = alloc(layout);
    if ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    let tv = ptr as *mut TaggedVal;
    (*tv).tag = tag;
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
    let n = v as i64;
    if n >= SMALL_INT_MIN && n < SMALL_INT_MAX {
        return &INT32_CACHE[(n - SMALL_INT_MIN) as usize] as *const TaggedVal as *mut u8;
    }
    alloc_tagged(TAG_INT32, v as i64 as u64)
}

#[no_mangle]
pub unsafe extern "C" fn lin_box_int64(v: i64) -> *mut u8 {
    if v >= SMALL_INT_MIN && v < SMALL_INT_MAX {
        return &INT64_CACHE[(v - SMALL_INT_MIN) as usize] as *const TaggedVal as *mut u8;
    }
    alloc_tagged(TAG_INT64, v as u64)
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
pub unsafe extern "C" fn lin_box_object(p: *mut u8) -> *mut u8 {
    alloc_tagged(TAG_OBJECT, p as u64)
}

#[no_mangle]
pub unsafe extern "C" fn lin_box_array(p: *mut u8) -> *mut u8 {
    alloc_tagged(TAG_ARRAY, p as u64)
}

#[no_mangle]
pub unsafe extern "C" fn lin_box_function(p: *mut u8) -> *mut u8 {
    alloc_tagged(TAG_FUNCTION, p as u64)
}

/// Get the type tag of a boxed value. Returns TAG_NULL (0) for null pointer.
#[no_mangle]
pub unsafe extern "C" fn lin_get_tag(p: *const u8) -> u8 {
    if p.is_null() {
        TAG_NULL
    } else {
        (*(p as *const TaggedVal)).tag
    }
}

/// Unbox an Int32 value (assumes tag is TAG_INT32).
#[no_mangle]
pub unsafe extern "C" fn lin_unbox_int32(p: *const u8) -> i32 {
    (*(p as *const TaggedVal)).payload as i32
}

/// Unbox an Int64 value (assumes tag is TAG_INT64).
#[no_mangle]
pub unsafe extern "C" fn lin_unbox_int64(p: *const u8) -> i64 {
    (*(p as *const TaggedVal)).payload as i64
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
#[no_mangle]
pub unsafe extern "C" fn lin_unbox_ptr(p: *const u8) -> *mut u8 {
    if p.is_null() { return std::ptr::null_mut(); }
    (*(p as *const TaggedVal)).payload as *mut u8
}

/// Deep equality for two TaggedVal* values. Returns 1 if equal, 0 if not.
/// Handles null (TAG_NULL), scalars (bool/int/float), strings, objects, and arrays.
/// Either pointer may be null (treated as TAG_NULL).
#[no_mangle]
pub unsafe extern "C" fn lin_tagged_eq(a: *const u8, b: *const u8) -> u8 {
    let av = a as *const TaggedVal;
    let bv = b as *const TaggedVal;
    let at = if av.is_null() { TAG_NULL } else { (*av).tag };
    let bt = if bv.is_null() { TAG_NULL } else { (*bv).tag };
    if at == TAG_NULL && bt == TAG_NULL { return 1; }
    if at == TAG_NULL || bt == TAG_NULL { return 0; }
    let ap = (*av).payload;
    let bp = (*bv).payload;
    // Cross-numeric equality: compare numeric types by value (Int32 == Int64 if same numeric value).
    let at_is_num = at >= TAG_INT32 && at <= TAG_FLOAT64;
    let bt_is_num = bt >= TAG_INT32 && bt <= TAG_FLOAT64;
    if at_is_num && bt_is_num && at != bt {
        return (tagged_as_f64(at, ap) == tagged_as_f64(bt, bp)) as u8;
    }
    if at != bt { return 0; }
    match at {
        TAG_BOOL => (ap == bp) as u8,
        TAG_INT32 => ((ap as i32) == (bp as i32)) as u8,
        TAG_INT64 => ((ap as i64) == (bp as i64)) as u8,
        TAG_FLOAT32 => (f32::from_bits(ap as u32) == f32::from_bits(bp as u32)) as u8,
        TAG_FLOAT64 => (f64::from_bits(ap) == f64::from_bits(bp)) as u8,
        TAG_STR => {
            let as_ptr = ap as *const crate::string::LinString;
            let bs_ptr = bp as *const crate::string::LinString;
            crate::string::lin_string_eq(as_ptr, bs_ptr) as u8
        }
        TAG_OBJECT => {
            let ao = ap as *const crate::object::LinObject;
            let bo = bp as *const crate::object::LinObject;
            crate::object::lin_object_eq(ao, bo)
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
#[no_mangle]
pub unsafe extern "C" fn lin_tagged_cmp(a: *const u8, b: *const u8) -> i32 {
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
        (a_tag, b_tag) if a_tag >= TAG_INT32 && a_tag <= TAG_FLOAT64 && b_tag >= TAG_INT32 && b_tag <= TAG_FLOAT64 => {
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
        TAG_FLOAT32 => f32::from_bits(payload as u32) as f64,
        TAG_FLOAT64 => f64::from_bits(payload),
        _ => 0.0,
    }
}

/// Dynamic length dispatch: returns length of string, array, or object TaggedVal*.
/// Returns 0 for null/bool/numeric types (no meaningful length).
#[no_mangle]
pub unsafe extern "C" fn lin_length_dyn(p: *const u8) -> i32 {
    if p.is_null() {
        return 0;
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
        TAG_OBJECT => {
            let n = crate::object::lin_object_length(payload as *const crate::object::LinObject);
            n as i32
        }
        _ => 0,
    }
}

/// Release a TaggedVal*: release the pointed-to heap value (if pointer type), then free the box.
/// Safe to call with null (treated as null — no-op).
#[no_mangle]
pub unsafe extern "C" fn lin_tagged_release(p: *mut u8) {
    if p.is_null() {
        return;
    }
    let tv = p as *mut TaggedVal;
    let tag = (*tv).tag;
    let payload = (*tv).payload;
    // Release the pointed-to value for pointer-typed payloads.
    match tag {
        TAG_STR => crate::string::lin_string_release(payload as *mut crate::string::LinString),
        TAG_ARRAY => crate::array::lin_array_release(payload as *mut crate::array::LinArray),
        TAG_OBJECT => crate::object::lin_object_release(payload as *mut crate::object::LinObject),
        _ => {} // Scalars (null, bool, int, float) have no heap payload.
    }
    // Cached scalar boxes (small ints, bools) are immutable statics — never free them.
    if is_cached_box(p) {
        return;
    }
    // Free the TaggedVal box itself.
    std::alloc::dealloc(p, std::alloc::Layout::new::<TaggedVal>());
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
                assert!(is_cached_box(p), "in-range int {v} should be cached");
                // Boxing the same value twice returns the identical cached pointer.
                assert_eq!(p, lin_box_int32(v));
                // Releasing a cached box must be a harmless no-op (no free).
                lin_tagged_release(p);
                assert_eq!(lin_unbox_int32(p), v, "cached box survived release");
            }
        }
    }

    #[test]
    fn out_of_range_ints_allocate_fresh() {
        unsafe {
            let v = SMALL_INT_MAX as i32; // one past the cache
            let p = lin_box_int32(v);
            assert_eq!(lin_unbox_int32(p), v);
            assert!(!is_cached_box(p), "out-of-range int should be heap-allocated");
            lin_tagged_release(p); // frees the heap box
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
            assert!(is_cached_box(p));
        }
    }
}
