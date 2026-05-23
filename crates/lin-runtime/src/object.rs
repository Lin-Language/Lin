use std::alloc::{alloc, dealloc, realloc, Layout};
use crate::string::LinString;
use crate::tagged::TaggedVal;

/// Dynamic object (Json-typed) represented as an array of key-value pairs.
/// Layout: refcount (u32) | len (u32) | cap (u32) | _pad (u32) | entries (*mut LinObjectEntry)
#[repr(C)]
pub struct LinObject {
    pub refcount: u32,
    pub len: u32,
    pub cap: u32,
    _pad: u32,
    pub entries: *mut LinObjectEntry,
}

#[repr(C)]
pub struct LinObjectEntry {
    pub key: *mut LinString,
    pub value: TaggedVal,
}

unsafe fn object_layout() -> Layout {
    Layout::from_size_align_unchecked(
        std::mem::size_of::<LinObject>(),
        std::mem::align_of::<LinObject>(),
    )
}

unsafe fn entries_layout(cap: u32) -> Layout {
    Layout::from_size_align_unchecked(
        std::mem::size_of::<LinObjectEntry>() * cap as usize,
        std::mem::align_of::<LinObjectEntry>(),
    )
}

#[no_mangle]
pub unsafe extern "C" fn lin_object_alloc(initial_cap: u32) -> *mut LinObject {
    let cap = initial_cap.max(4);
    let ptr = alloc(object_layout()) as *mut LinObject;
    (*ptr).refcount = 1;
    (*ptr).len = 0;
    (*ptr).cap = cap;
    (*ptr)._pad = 0;
    (*ptr).entries = alloc(entries_layout(cap)) as *mut LinObjectEntry;
    ptr
}

/// Set a field. Key must be a LinString*. Value is a TaggedVal* (pointer to tagged payload).
/// Copies the 16-byte TaggedVal struct.
#[no_mangle]
pub unsafe extern "C" fn lin_object_set(obj: *mut LinObject, key: *mut LinString, val: *const TaggedVal) {
    let len = (*obj).len;
    // Check if key already exists.
    for i in 0..len {
        let entry = (*obj).entries.add(i as usize);
        if lin_string_key_eq((*entry).key, key) {
            // Update existing entry.
            std::ptr::copy_nonoverlapping(val, &mut (*entry).value, 1);
            return;
        }
    }
    // New key — grow if needed.
    let cap = (*obj).cap;
    if len == cap {
        let new_cap = cap * 2;
        let old_layout = entries_layout(cap);
        let new_layout = entries_layout(new_cap);
        (*obj).entries = realloc((*obj).entries as *mut u8, old_layout, new_layout.size()) as *mut LinObjectEntry;
        (*obj).cap = new_cap;
    }
    let slot = (*obj).entries.add(len as usize);
    (*slot).key = key;
    std::ptr::copy_nonoverlapping(val, &mut (*slot).value, 1);
    (*obj).len = len + 1;
}

/// Get a field value as a pointer to TaggedVal. Returns null if key not found.
#[no_mangle]
pub unsafe extern "C" fn lin_object_get(obj: *const LinObject, key: *const LinString) -> *const TaggedVal {
    if obj.is_null() {
        return std::ptr::null();
    }
    let len = (*obj).len;
    for i in 0..len {
        let entry = (*obj).entries.add(i as usize);
        if lin_string_key_eq((*entry).key, key) {
            return &(*entry).value;
        }
    }
    std::ptr::null()
}

/// Check if two LinString keys are equal.
unsafe fn lin_string_key_eq(a: *const LinString, b: *const LinString) -> bool {
    if a == b { return true; }
    if a.is_null() || b.is_null() { return false; }
    let a_len = (*a).len;
    let b_len = (*b).len;
    if a_len != b_len { return false; }
    let a_data = (*a).data.as_ptr();
    let b_data = (*b).data.as_ptr();
    let a_slice = std::slice::from_raw_parts(a_data, a_len as usize);
    let b_slice = std::slice::from_raw_parts(b_data, b_len as usize);
    a_slice == b_slice
}

/// Check if an object has a given key. Returns 1 if present, 0 if not.
#[no_mangle]
pub unsafe extern "C" fn lin_object_has(obj: *const LinObject, key: *const LinString) -> u8 {
    if obj.is_null() { return 0; }
    let len = (*obj).len;
    for i in 0..len {
        let entry = (*obj).entries.add(i as usize);
        if lin_string_key_eq((*entry).key, key) {
            return 1;
        }
    }
    0
}

/// Deep structural equality for two objects: same keys and values, order-independent.
/// Returns 1 if equal, 0 if not.
#[no_mangle]
pub unsafe extern "C" fn lin_object_eq(a: *const LinObject, b: *const LinObject) -> u8 {
    use crate::tagged::{TaggedVal, TAG_NULL, TAG_BOOL, TAG_INT32, TAG_INT64, TAG_STR};
    use crate::string::{LinString, lin_string_eq};

    if a == b { return 1; }
    if a.is_null() || b.is_null() { return 0; }
    let a_len = (*a).len;
    let b_len = (*b).len;
    if a_len != b_len { return 0; }
    // For each entry in a, find matching entry in b with equal value.
    for i in 0..a_len {
        let ae = (*a).entries.add(i as usize);
        let a_key = (*ae).key;
        // Find this key in b.
        let mut found = false;
        for j in 0..b_len {
            let be = (*b).entries.add(j as usize);
            let b_key = (*be).key;
            if lin_string_key_eq(a_key, b_key) {
                // Compare values.
                let av = &(*ae).value as *const TaggedVal;
                let bv = &(*be).value as *const TaggedVal;
                if !tagged_val_eq(av, bv) { return 0; }
                found = true;
                break;
            }
        }
        if !found { return 0; }
    }
    1
}

unsafe fn tagged_val_eq(a: *const crate::tagged::TaggedVal, b: *const crate::tagged::TaggedVal) -> bool {
    use crate::tagged::*;
    if a.is_null() && b.is_null() { return true; }
    if a.is_null() || b.is_null() { return false; }
    let at = (*a).tag;
    let bt = (*b).tag;
    if at != bt { return false; }
    let ap = (*a).payload;
    let bp = (*b).payload;
    if at == TAG_NULL { return true; }
    if at == TAG_BOOL { return ap == bp; }
    if at == TAG_INT32 { return (ap as i32) == (bp as i32); }
    if at == TAG_INT64 { return (ap as i64) == (bp as i64); }
    if at == TAG_FLOAT32 {
        let af = f32::from_bits(ap as u32);
        let bf = f32::from_bits(bp as u32);
        return af == bf;
    }
    if at == TAG_FLOAT64 {
        let af = f64::from_bits(ap);
        let bf = f64::from_bits(bp);
        return af == bf;
    }
    if at == TAG_STR {
        let as_ptr = ap as *const crate::string::LinString;
        let bs_ptr = bp as *const crate::string::LinString;
        return crate::string::lin_string_eq(as_ptr, bs_ptr);
    }
    // For objects, arrays, etc.: compare by pointer (structural eq not yet recursive).
    ap == bp
}
