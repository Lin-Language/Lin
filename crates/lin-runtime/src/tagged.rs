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
pub struct TaggedVal {
    pub tag: u8,
    pub _pad: [u8; 7],
    pub payload: u64,
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
    alloc_tagged(TAG_BOOL, v as u64)
}

#[no_mangle]
pub unsafe extern "C" fn lin_box_int32(v: i32) -> *mut u8 {
    alloc_tagged(TAG_INT32, v as i64 as u64)
}

#[no_mangle]
pub unsafe extern "C" fn lin_box_int64(v: i64) -> *mut u8 {
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
    // Free the TaggedVal box itself.
    std::alloc::dealloc(p, std::alloc::Layout::new::<TaggedVal>());
}
