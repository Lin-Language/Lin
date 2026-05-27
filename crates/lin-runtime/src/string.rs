use std::alloc::{alloc_zeroed, dealloc, Layout};
use crate::tagged::{TaggedVal, TAG_NULL, TAG_BOOL, TAG_INT32, TAG_INT64, TAG_FLOAT64, TAG_STR, TAG_FLOAT32, TAG_ARRAY, TAG_OBJECT};

/// Runtime string representation: reference-counted, UTF-8.
/// Layout: refcount (u32) | len (u32) | data ([u8; len])
#[repr(C)]
pub struct LinString {
    pub refcount: u32,
    pub len: u32,
    pub data: [u8; 0],
}

impl LinString {
    pub unsafe fn as_str(&self) -> &str {
        let slice = std::slice::from_raw_parts(self.data.as_ptr(), self.len as usize);
        std::str::from_utf8_unchecked(slice)
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_alloc(len: u32) -> *mut LinString {
    let size = std::mem::size_of::<LinString>() + len as usize;
    let layout = Layout::from_size_align_unchecked(size, std::mem::align_of::<u32>());
    let ptr = alloc_zeroed(layout) as *mut LinString;
    (*ptr).refcount = 1;
    (*ptr).len = len;
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_free(s: *mut LinString) {
    let size = std::mem::size_of::<LinString>() + (*s).len as usize;
    let layout = Layout::from_size_align_unchecked(size, std::mem::align_of::<u32>());
    dealloc(s as *mut u8, layout);
}

/// Decrement refcount and free if zero.
#[no_mangle]
pub unsafe extern "C" fn lin_string_release(s: *mut LinString) {
    if s.is_null() {
        return;
    }
    (*s).refcount -= 1;
    if (*s).refcount == 0 {
        lin_string_free(s);
    }
}

/// Create a LinString from a raw byte pointer + length. Copies the bytes.
#[no_mangle]
pub unsafe extern "C" fn lin_string_from_bytes(data: *const u8, len: u32) -> *mut LinString {
    let ptr = lin_string_alloc(len);
    if len > 0 {
        std::ptr::copy_nonoverlapping(data, (*ptr).data.as_mut_ptr(), len as usize);
    }
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_concat(a: *const LinString, b: *const LinString) -> *mut LinString {
    let a_len = (*a).len;
    let b_len = (*b).len;
    let new_len = a_len + b_len;
    let ptr = lin_string_alloc(new_len);
    let dst = (*ptr).data.as_mut_ptr();
    std::ptr::copy_nonoverlapping((*a).data.as_ptr(), dst, a_len as usize);
    std::ptr::copy_nonoverlapping((*b).data.as_ptr(), dst.add(a_len as usize), b_len as usize);
    ptr
}

/// Concatenate `n` strings in a single allocation.
/// `parts` is a pointer to an array of `n` `*const LinString` pointers.
#[no_mangle]
pub unsafe extern "C" fn lin_string_build_n(parts: *const *const LinString, n: u32) -> *mut LinString {
    let parts = std::slice::from_raw_parts(parts, n as usize);
    let total_len: u32 = parts.iter().map(|&s| (*s).len).sum();
    let ptr = lin_string_alloc(total_len);
    let mut dst = (*ptr).data.as_mut_ptr();
    for &s in parts {
        let len = (*s).len as usize;
        std::ptr::copy_nonoverlapping((*s).data.as_ptr(), dst, len);
        dst = dst.add(len);
    }
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_length(s: *const LinString) -> i32 {
    (*s).len as i32
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_eq(a: *const LinString, b: *const LinString) -> bool {
    if (*a).len != (*b).len {
        return false;
    }
    let a_slice = std::slice::from_raw_parts((*a).data.as_ptr(), (*a).len as usize);
    let b_slice = std::slice::from_raw_parts((*b).data.as_ptr(), (*b).len as usize);
    a_slice == b_slice
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_slice(
    s: *const LinString,
    start: i32,
    end: i32,
) -> *mut LinString {
    let len = (*s).len as i32;
    let start = start.clamp(0, len) as usize;
    let end = end.clamp(0, len) as usize;
    let end = end.max(start);
    let slice_len = end - start;
    let ptr = lin_string_alloc(slice_len as u32);
    if slice_len > 0 {
        std::ptr::copy_nonoverlapping(
            (*s).data.as_ptr().add(start),
            (*ptr).data.as_mut_ptr(),
            slice_len,
        );
    }
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_char_at(s: *const LinString, index: i32) -> *mut LinString {
    let len = (*s).len as i32;
    if index < 0 || index >= len {
        return lin_string_alloc(0);
    }
    let byte = *(*s).data.as_ptr().add(index as usize);
    let ptr = lin_string_alloc(1);
    *(*ptr).data.as_mut_ptr() = byte;
    ptr
}

/// Return the Unicode code point at char index `index`. Returns -1 if OOB or negative index.
#[no_mangle]
pub unsafe extern "C" fn lin_string_char_code(s: *const LinString, index: i32) -> i32 {
    if index < 0 { return -1; }
    let st = (*s).as_str();
    st.chars().nth(index as usize).map(|c| c as i32).unwrap_or(-1)
}

/// Create a single-character string from a Unicode code point. Returns "" for invalid code points.
#[no_mangle]
pub unsafe extern "C" fn lin_string_from_char_code(code: i32) -> *mut LinString {
    if code < 0 {
        return lin_string_from_bytes(b"".as_ptr(), 0);
    }
    match char::from_u32(code as u32) {
        None => lin_string_from_bytes(b"".as_ptr(), 0),
        Some(c) => {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            lin_string_from_bytes(s.as_ptr(), s.len() as u32)
        }
    }
}

/// Lexicographic comparison. Returns -1, 0, or 1.
#[no_mangle]
pub unsafe extern "C" fn lin_string_cmp(a: *const LinString, b: *const LinString) -> i32 {
    let a_bytes = std::slice::from_raw_parts((*a).data.as_ptr(), (*a).len as usize);
    let b_bytes = std::slice::from_raw_parts((*b).data.as_ptr(), (*b).len as usize);
    match a_bytes.cmp(b_bytes) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

// Numeric -> string conversions

#[no_mangle]
pub extern "C" fn lin_int_to_string(n: i64) -> *mut LinString {
    let s = n.to_string();
    unsafe { lin_string_from_bytes(s.as_ptr(), s.len() as u32) }
}

#[no_mangle]
pub extern "C" fn lin_float_to_string(f: f64) -> *mut LinString {
    let s = if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{:.1}", f)
    } else {
        format!("{}", f)
    };
    unsafe { lin_string_from_bytes(s.as_ptr(), s.len() as u32) }
}

#[no_mangle]
pub extern "C" fn lin_bool_to_string(b: bool) -> *mut LinString {
    let s = if b { "true" } else { "false" };
    unsafe { lin_string_from_bytes(s.as_ptr(), s.len() as u32) }
}

#[no_mangle]
pub extern "C" fn lin_null_to_string() -> *mut LinString {
    unsafe { lin_string_from_bytes("null".as_ptr(), 4) }
}

// --- String manipulation functions ---

#[no_mangle]
pub unsafe extern "C" fn lin_string_trim(s: *const LinString) -> *mut LinString {
    let st = (*s).as_str();
    let trimmed = st.trim();
    lin_string_from_bytes(trimmed.as_ptr(), trimmed.len() as u32)
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_trim_start(s: *const LinString) -> *mut LinString {
    let st = (*s).as_str();
    let trimmed = st.trim_start();
    lin_string_from_bytes(trimmed.as_ptr(), trimmed.len() as u32)
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_trim_end(s: *const LinString) -> *mut LinString {
    let st = (*s).as_str();
    let trimmed = st.trim_end();
    lin_string_from_bytes(trimmed.as_ptr(), trimmed.len() as u32)
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_to_upper(s: *const LinString) -> *mut LinString {
    let st = (*s).as_str();
    let upper = st.to_uppercase();
    lin_string_from_bytes(upper.as_ptr(), upper.len() as u32)
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_to_lower(s: *const LinString) -> *mut LinString {
    let st = (*s).as_str();
    let lower = st.to_lowercase();
    lin_string_from_bytes(lower.as_ptr(), lower.len() as u32)
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_index_of(s: *const LinString, needle: *const LinString) -> i32 {
    let st = (*s).as_str();
    let nd = (*needle).as_str();
    match st.find(nd) {
        Some(i) => i as i32,
        None => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_last_index_of(s: *const LinString, needle: *const LinString) -> i32 {
    let st = (*s).as_str();
    let nd = (*needle).as_str();
    match st.rfind(nd) {
        Some(byte_pos) => st[..byte_pos].chars().count() as i32,
        None => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_contains(s: *const LinString, needle: *const LinString) -> bool {
    let st = (*s).as_str();
    let nd = (*needle).as_str();
    st.contains(nd)
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_starts_with(s: *const LinString, prefix: *const LinString) -> bool {
    let st = (*s).as_str();
    let pf = (*prefix).as_str();
    st.starts_with(pf)
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_ends_with(s: *const LinString, suffix: *const LinString) -> bool {
    let st = (*s).as_str();
    let sf = (*suffix).as_str();
    st.ends_with(sf)
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_replace(s: *const LinString, pattern: *const LinString, replacement: *const LinString) -> *mut LinString {
    let st = (*s).as_str();
    let pat = (*pattern).as_str();
    let rep = (*replacement).as_str();
    let result = st.replace(pat, rep);
    lin_string_from_bytes(result.as_ptr(), result.len() as u32)
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_repeat(s: *const LinString, count: i32) -> *mut LinString {
    let st = (*s).as_str();
    let n = count.max(0) as usize;
    let result = st.repeat(n);
    lin_string_from_bytes(result.as_ptr(), result.len() as u32)
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_split(s: *const LinString, delimiter: *const LinString) -> *mut crate::array::LinArray {
    use crate::array::{lin_array_alloc, lin_array_push};
    let st = (*s).as_str();
    let delim = (*delimiter).as_str();
    let arr = lin_array_alloc(4);
    for part in st.split(delim) {
        let part_str = lin_string_from_bytes(part.as_ptr(), part.len() as u32);
        let cell = &part_str as *const *mut LinString as *const u8;
        lin_array_push(arr, cell, 0);
    }
    arr
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_join(arr: *const crate::array::LinArray, separator: *const LinString) -> *mut LinString {
    use crate::array::{lin_array_length, lin_array_get};
    let n = lin_array_length(arr) as usize;
    let sep = (*separator).as_str();
    let mut parts: Vec<&str> = Vec::with_capacity(n);
    for i in 0..n {
        let elem = lin_array_get(arr, i as i64);
        // Element payload is a LinString*
        let payload_ptr = (elem as *const u8).add(8) as *const *mut LinString;
        let s_ptr = *payload_ptr;
        parts.push((*s_ptr).as_str());
    }
    let result = parts.join(sep);
    lin_string_from_bytes(result.as_ptr(), result.len() as u32)
}

/// Recursively convert a TaggedVal to its JSON string representation.
/// Used for toString(obj), toString(arr), and string interpolation of complex values.
pub unsafe fn tagged_to_json_string(tagged: *const TaggedVal) -> String {
    if tagged.is_null() {
        return "null".to_string();
    }
    let tag = (*tagged).tag;
    let payload = (*tagged).payload;
    if tag == TAG_NULL { return "null".to_string(); }
    if tag == TAG_BOOL { return if payload != 0 { "true" } else { "false" }.to_string(); }
    if tag == TAG_INT32 { return (payload as i32).to_string(); }
    if tag == TAG_INT64 { return (payload as i64).to_string(); }
    if tag == TAG_FLOAT32 {
        let f = f32::from_bits(payload as u32);
        return format!("{}", f);
    }
    if tag == TAG_FLOAT64 {
        let f = f64::from_bits(payload);
        return format!("{}", f);
    }
    if tag == TAG_STR {
        let s = payload as *const LinString;
        if s.is_null() { return "null".to_string(); }
        return format!("\"{}\"", (*s).as_str());
    }
    if tag == TAG_ARRAY {
        let arr = payload as *const crate::array::LinArray;
        if arr.is_null() { return "[]".to_string(); }
        return array_to_json_string(arr);
    }
    if tag == TAG_OBJECT {
        let obj = payload as *const crate::object::LinObject;
        if obj.is_null() { return "{}".to_string(); }
        return object_to_json_string(obj);
    }
    "[object]".to_string()
}

unsafe fn array_to_json_string(arr: *const crate::array::LinArray) -> String {
    let len = (*arr).len as usize;
    let mut parts = Vec::with_capacity(len);
    for i in 0..len {
        let elem = (*arr).data.add(i);
        parts.push(tagged_to_json_string(elem as *const TaggedVal));
    }
    format!("[{}]", parts.join(", "))
}

unsafe fn object_to_json_string(obj: *const crate::object::LinObject) -> String {
    let len = (*obj).len as usize;
    let mut parts = Vec::with_capacity(len);
    for i in 0..len {
        let entry = (*obj).entries.add(i);
        let key = (*entry).key;
        let key_str = if key.is_null() { "null".to_string() } else { (*key).as_str().to_string() };
        let val_str = tagged_to_json_string(&(*entry).value as *const TaggedVal);
        parts.push(format!("\"{}\": {}", key_str, val_str));
    }
    format!("{{{}}}", parts.join(", "))
}

/// Convert a LinArray* to its JSON string representation.
#[no_mangle]
pub unsafe extern "C" fn lin_array_to_string(arr: *const crate::array::LinArray) -> *mut LinString {
    if arr.is_null() {
        return lin_string_from_bytes(b"null".as_ptr(), 4);
    }
    let s = array_to_json_string(arr);
    lin_string_from_bytes(s.as_ptr(), s.len() as u32)
}

/// Convert a LinObject* to its JSON string representation.
#[no_mangle]
pub unsafe extern "C" fn lin_object_to_string(obj: *const crate::object::LinObject) -> *mut LinString {
    if obj.is_null() {
        return lin_string_from_bytes(b"null".as_ptr(), 4);
    }
    let s = object_to_json_string(obj);
    lin_string_from_bytes(s.as_ptr(), s.len() as u32)
}

/// Convert a TaggedVal* to a string, dispatching on the runtime tag.
/// `tagged` may be null (treated as Null) or a pointer to a TaggedVal.
#[no_mangle]
pub unsafe extern "C" fn lin_tagged_to_string(tagged: *const TaggedVal) -> *mut LinString {
    if tagged.is_null() {
        return lin_null_to_string();
    }
    let tag = (*tagged).tag;
    let payload = (*tagged).payload;
    if tag == TAG_NULL {
        lin_null_to_string()
    } else if tag == TAG_BOOL {
        lin_bool_to_string(payload != 0)
    } else if tag == TAG_INT32 {
        lin_int_to_string(payload as i32 as i64)
    } else if tag == TAG_INT64 {
        lin_int_to_string(payload as i64)
    } else if tag == TAG_FLOAT32 {
        let f = f32::from_bits(payload as u32);
        lin_float_to_string(f as f64)
    } else if tag == TAG_FLOAT64 {
        let f = f64::from_bits(payload);
        lin_float_to_string(f)
    } else if tag == TAG_STR {
        payload as *mut LinString
    } else if tag == TAG_ARRAY {
        let arr = payload as *const crate::array::LinArray;
        lin_array_to_string(arr)
    } else if tag == TAG_OBJECT {
        let obj = payload as *const crate::object::LinObject;
        lin_object_to_string(obj)
    } else {
        lin_string_from_bytes(b"[object]".as_ptr(), 8)
    }
}
