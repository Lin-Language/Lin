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
    TAG_BIGNUM, TAG_DECIMAL,
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

/// Public wrapper for `is_cached_box`, used by `lin_tagged_clone` to alias immutable cached
/// scalar boxes instead of allocating a fresh box for them.
#[inline]
pub unsafe fn is_cached_box_pub(p: *const u8) -> bool {
    is_cached_box(p)
}

pub unsafe fn alloc_tagged(tag: u8, payload: u64) -> *mut u8 {
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
    // KEEP-PACKED-THROUGH-RECORD-FIELDS boundary: a kept-packed `*SumNode` (TAG_SUMNODE) escaped into
    // a dynamic equality. Materialize either operand to a real LinObject and compare as objects
    // (order-independent structural equality). Transient materializations released after.
    if at == TAG_SUMNODE || bt == TAG_SUMNODE {
        let mat = |tv: *const TaggedVal, t: u8| -> (*mut u8, bool) {
            if t == TAG_SUMNODE {
                (crate::sumnode::lin_sumnode_materialize((*tv).payload as *mut u8), true)
            } else {
                ((*tv).payload as *mut u8, false)
            }
        };
        let (ao, a_owned) = mat(av, at);
        let (bo, b_owned) = mat(bv, bt);
        let eq = if (at == TAG_SUMNODE || at == TAG_OBJECT) && (bt == TAG_SUMNODE || bt == TAG_OBJECT) {
            crate::object::lin_object_eq(ao as *const crate::object::LinObject, bo as *const crate::object::LinObject)
        } else {
            0
        };
        if a_owned { crate::object::lin_object_release(ao as *mut crate::object::LinObject); }
        if b_owned { crate::object::lin_object_release(bo as *mut crate::object::LinObject); }
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
            "Runtime error: cannot apply operator '{}' to dynamic Json operands of kind {} and {} \
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
/// Null-safe and cached-box-safe (immutable static scalar boxes are never freed).
#[no_mangle]
pub unsafe extern "C" fn lin_tagged_free_box(p: *mut u8) {
    if p.is_null() || is_cached_box(p) {
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
        TAG_MAP => crate::map::lin_map_release(payload as *mut crate::map::LinMap),
        // KEEP-PACKED sum node in a record-field slot: dispatch to the SumNode self-release (reads
        // its own size from the header), NOT lin_object_release (which would read the SumNode's
        // offset-4 size as a LinObject len → type-confusion). The matching retain bumps offset-0 RC.
        TAG_SUMNODE => crate::sumnode::lin_sumnode_release_self(payload as *mut u8),
        TAG_SHARED => crate::shared::lin_shared_release_box(payload as *const u8),
        TAG_STREAM => crate::stream::lin_stream_release_box(payload as *const u8),
        // Opaque arbitrary-precision/decimal handles (std/bignum, std/decimal): refcounted Rust
        // boxes whose final drop frees the wrapped num value. Mirror of the TAG_STREAM arm.
        TAG_BIGNUM => crate::bignum::lin_bignum_release_box(payload as *const u8),
        TAG_DECIMAL => crate::decimal::lin_decimal_release_box(payload as *const u8),
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
