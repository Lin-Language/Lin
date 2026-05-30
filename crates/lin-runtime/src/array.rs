use std::alloc::{alloc, alloc_zeroed, dealloc, realloc, Layout};

/// Heap-allocated growable array.
/// Layout: refcount (u32) | elem_tag (u8) | _pad3 ([u8;3]) | len (u64) | cap (u64) | data (*mut LinArrayElem)
/// elem_tag == 0xFF → tagged elements (LinArrayElem 16-byte layout).
/// elem_tag == TAG_INT32/INT64/FLOAT32/FLOAT64 → flat scalar elements (raw T-sized layout).
#[repr(C)]
pub struct LinArray {
    pub refcount: u32,
    pub elem_tag: u8,
    _pad3: [u8; 3],
    pub len: u64,
    pub cap: u64,
    pub data: *mut LinArrayElem,
}

#[repr(C)]
pub struct LinArrayElem {
    pub tag: u8,
    _pad: [u8; 7],
    /// For scalar types this is the value directly (int/float/bool/null).
    /// For pointer types (String, Array, Object, Closure) this is the pointer.
    pub payload: u64,
}

unsafe fn array_elem_layout(cap: u64) -> Layout {
    Layout::from_size_align_unchecked(
        std::mem::size_of::<LinArrayElem>() * cap as usize,
        std::mem::align_of::<LinArrayElem>(),
    )
}

/// (size, align) of one element for a given `elem_tag`. Flat scalar arrays store raw
/// values of the element's natural width; `0xFF` (tagged) stores 16-byte LinArrayElem.
/// The data buffer MUST be freed with the same layout it was allocated with, so this
/// must match each flat family's `$alloc`/`$free` (e.g. lin_flat_array_alloc_u8 uses
/// size_of::<u8>()). Using the tagged 16-byte layout to free a flat array corrupts the heap.
fn flat_elem_size_align(elem_tag: u8) -> (usize, usize) {
    use crate::tagged::*;
    match elem_tag {
        TAG_INT32 | TAG_FLOAT32 => (4, 4),
        TAG_INT64 | TAG_FLOAT64 => (8, 8),
        TAG_UINT8 | TAG_INT8 | TAG_BOOL => (1, 1),
        TAG_UINT16 | TAG_INT16 => (2, 2),
        // 0xFF and anything else: tagged 16-byte elements.
        _ => (
            std::mem::size_of::<LinArrayElem>(),
            std::mem::align_of::<LinArrayElem>(),
        ),
    }
}

unsafe fn array_layout() -> Layout {
    Layout::from_size_align_unchecked(
        std::mem::size_of::<LinArray>(),
        std::mem::align_of::<LinArray>(),
    )
}

#[no_mangle]
pub unsafe extern "C" fn lin_array_alloc(initial_cap: u64) -> *mut LinArray {
    let cap = initial_cap.max(4);
    let arr_layout = array_layout();
    let ptr = alloc(arr_layout) as *mut LinArray;
    (*ptr).refcount = 1;
    (*ptr).elem_tag = 0xFF;
    (*ptr)._pad3 = [0; 3];
    (*ptr).len = 0;
    (*ptr).cap = cap;
    let elem_layout = array_elem_layout(cap);
    (*ptr).data = alloc(elem_layout) as *mut LinArrayElem;
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn lin_array_free(arr: *mut LinArray) {
    let cap = (*arr).cap as usize;
    // Free the data buffer with the SAME layout it was allocated with. Flat scalar
    // arrays use their element's natural width, not the 16-byte tagged element size —
    // freeing a flat u8 array with the tagged layout deallocs 16x too much and corrupts
    // the heap (surfaces as a SEGV in a much later, unrelated allocation).
    let (esize, ealign) = flat_elem_size_align((*arr).elem_tag);
    let data_layout = Layout::from_size_align_unchecked(esize * cap, ealign);
    dealloc((*arr).data as *mut u8, data_layout);
    dealloc(arr as *mut u8, array_layout());
}

/// Decrement refcount; when it reaches zero, release all heap-typed elements then free.
#[no_mangle]
pub unsafe extern "C" fn lin_array_release(arr: *mut LinArray) {
    if arr.is_null() {
        return;
    }
    (*arr).refcount -= 1;
    if (*arr).refcount == 0 {
        // For tagged arrays (elem_tag == 0xFF), release any heap-typed elements before
        // freeing the backing buffer.  Flat scalar arrays hold no pointers.
        if (*arr).elem_tag == 0xFF {
            let len = (*arr).len as usize;
            for i in 0..len {
                let elem = (*arr).data.add(i);
                let payload = (*elem).payload;
                match (*elem).tag {
                    crate::tagged::TAG_STR => {
                        crate::string::lin_string_release(payload as *mut crate::string::LinString);
                    }
                    crate::tagged::TAG_ARRAY => {
                        lin_array_release(payload as *mut LinArray);
                    }
                    crate::tagged::TAG_OBJECT => {
                        crate::object::lin_object_release(payload as *mut crate::object::LinObject);
                    }
                    crate::tagged::TAG_FUNCTION => {
                        crate::memory::lin_closure_release(payload as *mut u8);
                    }
                    _ => {} // scalars: no heap payload
                }
            }
        }
        lin_array_free(arr);
    }
}

/// Push an element. `elem_ptr` points to the value; `tag` is the type tag.
#[no_mangle]
pub unsafe extern "C" fn lin_array_push(arr: *mut LinArray, elem_ptr: *const u8, tag: u8) {
    let len = (*arr).len;
    let cap = (*arr).cap;
    if len == cap {
        let new_cap = cap * 2;
        let old_layout = array_elem_layout(cap);
        let new_layout = array_elem_layout(new_cap);
        (*arr).data = realloc((*arr).data as *mut u8, old_layout, new_layout.size()) as *mut LinArrayElem;
        (*arr).cap = new_cap;
    }
    let slot = (*arr).data.add(len as usize);
    (*slot).tag = tag;
    // Copy 8 bytes from elem_ptr into payload (assumes elem fits in 8 bytes).
    std::ptr::copy_nonoverlapping(elem_ptr, &mut (*slot).payload as *mut u64 as *mut u8, 8);
    (*arr).len = len + 1;
}

/// Push an element that is already a TaggedVal* (copies tag+payload inline).
/// Ownership transfer: caller must NOT release the box after this call.
/// The array takes ownership of the inner heap value (no retain performed).
#[no_mangle]
pub unsafe extern "C" fn lin_array_push_tagged(arr: *mut LinArray, tagged: *const u8) {
    let len = (*arr).len;
    let cap = (*arr).cap;
    if len == cap {
        let new_cap = cap * 2;
        let old_layout = array_elem_layout(cap);
        let new_layout = array_elem_layout(new_cap);
        (*arr).data = realloc((*arr).data as *mut u8, old_layout, new_layout.size()) as *mut LinArrayElem;
        (*arr).cap = new_cap;
    }
    let slot = (*arr).data.add(len as usize);
    if tagged.is_null() {
        // A null TaggedVal* IS the Json null value — store a TAG_NULL entry rather than
        // dereferencing the null pointer.
        (*slot).tag = crate::tagged::TAG_NULL;
        (*slot).payload = 0;
    } else {
        // Copy 16 bytes (full TaggedVal = LinArrayElem) from tagged into slot.
        std::ptr::copy_nonoverlapping(tagged, slot as *mut u8, 16);
    }
    (*arr).len = len + 1;
}

/// Dynamic push: push a TaggedVal* element into an array of any format (flat or tagged).
/// Handles flat arrays (elem_tag != 0xFF) by converting the TaggedVal to the flat element type.
/// For tagged arrays (elem_tag == 0xFF), copies the TaggedVal inline and retains inner refcount.
#[no_mangle]
pub unsafe extern "C" fn lin_push_dyn(arr: *mut LinArray, tagged: *const crate::tagged::TaggedVal) {
    use crate::tagged::*;
    if arr.is_null() { return; }
    let elem_tag = (*arr).elem_tag;
    if elem_tag == 0xFF {
        // Tagged array: copy TaggedVal into slot and retain the inner heap value.
        lin_array_push_tagged(arr, tagged as *const u8);
        // Retain the inner payload so the array slot owns a reference.
        if !tagged.is_null() {
            crate::object::retain_tagged_payload_pub(&*tagged);
        }
    } else {
        // Flat array: extract the scalar value and push it.
        let tag = if tagged.is_null() { TAG_NULL } else { (*tagged).tag };
        let payload = if tagged.is_null() { 0u64 } else { (*tagged).payload };
        match elem_tag {
            TAG_INT32 => {
                let v = match tag {
                    TAG_INT32 => payload as i32,
                    TAG_INT64 => payload as i32,
                    TAG_FLOAT64 => f64::from_bits(payload) as i32,
                    _ => 0,
                };
                lin_flat_array_push_i32(arr, v);
            }
            TAG_INT64 => {
                let v = match tag {
                    TAG_INT32 => payload as i32 as i64,
                    TAG_INT64 => payload as i64,
                    TAG_FLOAT64 => f64::from_bits(payload) as i64,
                    _ => 0,
                };
                lin_flat_array_push_i64(arr, v);
            }
            TAG_FLOAT32 => {
                let v = match tag {
                    TAG_FLOAT32 => f32::from_bits(payload as u32),
                    TAG_FLOAT64 => f64::from_bits(payload) as f32,
                    TAG_INT32 => payload as i32 as f32,
                    _ => 0.0,
                };
                lin_flat_array_push_f32(arr, v);
            }
            TAG_FLOAT64 => {
                let v = match tag {
                    TAG_FLOAT64 => f64::from_bits(payload),
                    TAG_FLOAT32 => f32::from_bits(payload as u32) as f64,
                    TAG_INT32 => payload as i32 as f64,
                    _ => 0.0,
                };
                lin_flat_array_push_f64(arr, v);
            }
            TAG_UINT8 | TAG_INT8 => {
                let v = match tag {
                    TAG_INT32 => payload as i32,
                    TAG_INT64 => payload as i32,
                    TAG_FLOAT64 => f64::from_bits(payload) as i32,
                    _ => 0,
                };
                if elem_tag == TAG_UINT8 { lin_flat_array_push_u8(arr, v as u8); }
                else { lin_flat_array_push_i8(arr, v as i8); }
            }
            TAG_UINT16 | TAG_INT16 => {
                let v = match tag {
                    TAG_INT32 => payload as i32,
                    TAG_INT64 => payload as i32,
                    TAG_FLOAT64 => f64::from_bits(payload) as i32,
                    _ => 0,
                };
                if elem_tag == TAG_UINT16 { lin_flat_array_push_u16(arr, v as u16); }
                else { lin_flat_array_push_i16(arr, v as i16); }
            }
            _ => {}
        }
    }
}

/// Convert a flat i32 array to a tagged LinArray (each element tagged as TAG_INT32).
/// Used when passing a flat array into a Json-typed context.
#[no_mangle]
pub unsafe extern "C" fn lin_flat_to_tagged_i32(flat: *const LinArray) -> *mut LinArray {
    let len = (*flat).len;
    let tagged = lin_array_alloc(len.max(4));
    let src = (*flat).data as *const i32;
    for i in 0..len as usize {
        let v = *src.add(i);
        let slot = (*tagged).data.add(i);
        (*slot).tag = crate::tagged::TAG_INT32;
        (*slot).payload = v as i64 as u64;
    }
    (*tagged).len = len;
    tagged
}

/// Convert a flat i64 array to a tagged LinArray (each element tagged as TAG_INT64).
#[no_mangle]
pub unsafe extern "C" fn lin_flat_to_tagged_i64(flat: *const LinArray) -> *mut LinArray {
    let len = (*flat).len;
    let tagged = lin_array_alloc(len.max(4));
    let src = (*flat).data as *const i64;
    for i in 0..len as usize {
        let v = *src.add(i);
        let slot = (*tagged).data.add(i);
        (*slot).tag = crate::tagged::TAG_INT64;
        (*slot).payload = v as u64;
    }
    (*tagged).len = len;
    tagged
}

/// Convert a flat f32 array to a tagged LinArray (each element tagged as TAG_FLOAT32).
#[no_mangle]
pub unsafe extern "C" fn lin_flat_to_tagged_f32(flat: *const LinArray) -> *mut LinArray {
    let len = (*flat).len;
    let tagged = lin_array_alloc(len.max(4));
    let src = (*flat).data as *const f32;
    for i in 0..len as usize {
        let v = *src.add(i);
        let slot = (*tagged).data.add(i);
        (*slot).tag = crate::tagged::TAG_FLOAT32;
        (*slot).payload = v.to_bits() as u64;
    }
    (*tagged).len = len;
    tagged
}

/// Convert a flat f64 array to a tagged LinArray (each element tagged as TAG_FLOAT64).
#[no_mangle]
pub unsafe extern "C" fn lin_flat_to_tagged_f64(flat: *const LinArray) -> *mut LinArray {
    let len = (*flat).len;
    let tagged = lin_array_alloc(len.max(4));
    let src = (*flat).data as *const f64;
    for i in 0..len as usize {
        let v = *src.add(i);
        let slot = (*tagged).data.add(i);
        (*slot).tag = crate::tagged::TAG_FLOAT64;
        (*slot).payload = v.to_bits();
    }
    (*tagged).len = len;
    tagged
}

/// Get a pointer to the element payload at index. Supports negative indices (Python-style).
#[no_mangle]
pub unsafe extern "C" fn lin_array_get(arr: *const LinArray, idx: i64) -> *mut LinArrayElem {
    let len = (*arr).len as i64;
    let actual = if idx < 0 { len + idx } else { idx };
    if actual < 0 || actual >= len {
        eprintln!("Runtime error: array index {} out of bounds (len {})", idx, len);
        std::process::exit(1);
    }
    (*arr).data.add(actual as usize)
}

/// Set the element at index (in-place mutation). Supports negative indices.
/// Handles both flat and tagged arrays. No-op if index is out of bounds.
#[no_mangle]
pub unsafe extern "C" fn lin_array_set(arr: *mut LinArray, idx: i64, tagged: *const crate::tagged::TaggedVal) {
    use crate::tagged::*;
    if arr.is_null() { return; }
    let len = (*arr).len as i64;
    let actual = if idx < 0 { len + idx } else { idx };
    if actual < 0 || actual >= len { return; }
    let elem_tag = (*arr).elem_tag;
    if elem_tag == 0xFF {
        let slot = (*arr).data.add(actual as usize);
        std::ptr::copy_nonoverlapping(tagged as *const u8, slot as *mut u8, std::mem::size_of::<TaggedVal>());
    } else {
        let tag = if tagged.is_null() { TAG_NULL } else { (*tagged).tag };
        let payload = if tagged.is_null() { 0u64 } else { (*tagged).payload };
        match elem_tag {
            TAG_INT32 => {
                let v = match tag { TAG_INT32 => payload as i32, TAG_INT64 => payload as i32, TAG_FLOAT64 => f64::from_bits(payload) as i32, _ => 0 };
                *((*arr).data as *mut i32).add(actual as usize) = v;
            }
            TAG_INT64 => {
                let v = match tag { TAG_INT32 => payload as i32 as i64, TAG_INT64 => payload as i64, TAG_FLOAT64 => f64::from_bits(payload) as i64, _ => 0 };
                *((*arr).data as *mut i64).add(actual as usize) = v;
            }
            TAG_FLOAT32 => {
                let v = match tag { TAG_FLOAT32 => f32::from_bits(payload as u32), TAG_FLOAT64 => f64::from_bits(payload) as f32, TAG_INT32 => payload as i32 as f32, _ => 0.0 };
                *((*arr).data as *mut f32).add(actual as usize) = v;
            }
            TAG_FLOAT64 => {
                let v = match tag { TAG_FLOAT64 => f64::from_bits(payload), TAG_FLOAT32 => f32::from_bits(payload as u32) as f64, TAG_INT32 => payload as i32 as f64, _ => 0.0 };
                *((*arr).data as *mut f64).add(actual as usize) = v;
            }
            TAG_UINT8 | TAG_INT8 => {
                let v = match tag { TAG_INT32 => payload as i32, TAG_INT64 => payload as i32, TAG_FLOAT64 => f64::from_bits(payload) as i32, _ => 0 };
                *((*arr).data as *mut u8).add(actual as usize) = v as u8;
            }
            TAG_UINT16 | TAG_INT16 => {
                let v = match tag { TAG_INT32 => payload as i32, TAG_INT64 => payload as i32, TAG_FLOAT64 => f64::from_bits(payload) as i32, _ => 0 };
                *((*arr).data as *mut u16).add(actual as usize) = v as u16;
            }
            _ => {}
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_array_length(arr: *const LinArray) -> i64 {
    (*arr).len as i64
}

/// Get element at index as a heap-allocated TaggedVal*, handling both flat and tagged arrays.
/// The caller is responsible for eventual deallocation. Returns null on OOB.
#[no_mangle]
pub unsafe extern "C" fn lin_array_get_tagged(arr: *const LinArray, idx: i64) -> *mut crate::tagged::TaggedVal {
    use crate::tagged::*;
    if arr.is_null() { return std::ptr::null_mut(); }
    let len = (*arr).len as i64;
    let actual = if idx < 0 { len + idx } else { idx };
    let idx = actual;
    if idx < 0 || idx >= len {
        eprintln!("Runtime error: array index {} out of bounds (len {})", actual, len);
        std::process::exit(1);
    }
    let tv_layout = Layout::from_size_align_unchecked(
        std::mem::size_of::<TaggedVal>(),
        std::mem::align_of::<TaggedVal>(),
    );
    let tv = alloc(tv_layout) as *mut TaggedVal;
    let tag = (*arr).elem_tag;
    match tag {
        TAG_INT32 => {
            let v = *((*arr).data as *const i32).add(idx as usize);
            (*tv).tag = TAG_INT32;
            (*tv)._pad = [0; 7];
            (*tv).payload = v as i64 as u64;
        }
        TAG_INT64 => {
            let v = *((*arr).data as *const i64).add(idx as usize);
            (*tv).tag = TAG_INT64;
            (*tv)._pad = [0; 7];
            (*tv).payload = v as u64;
        }
        TAG_FLOAT32 => {
            let v = *((*arr).data as *const f32).add(idx as usize);
            (*tv).tag = TAG_FLOAT32;
            (*tv)._pad = [0; 7];
            (*tv).payload = v.to_bits() as u64;
        }
        TAG_FLOAT64 => {
            let v = *((*arr).data as *const f64).add(idx as usize);
            (*tv).tag = TAG_FLOAT64;
            (*tv)._pad = [0; 7];
            (*tv).payload = v.to_bits();
        }
        TAG_UINT8 => {
            let v = *((*arr).data as *const u8).add(idx as usize);
            (*tv).tag = TAG_INT32;
            (*tv)._pad = [0; 7];
            (*tv).payload = v as i64 as u64;
        }
        TAG_INT8 => {
            let v = *((*arr).data as *const i8).add(idx as usize);
            (*tv).tag = TAG_INT32;
            (*tv)._pad = [0; 7];
            (*tv).payload = v as i64 as u64;
        }
        TAG_UINT16 => {
            let v = *((*arr).data as *const u16).add(idx as usize);
            (*tv).tag = TAG_INT32;
            (*tv)._pad = [0; 7];
            (*tv).payload = v as i64 as u64;
        }
        TAG_INT16 => {
            let v = *((*arr).data as *const i16).add(idx as usize);
            (*tv).tag = TAG_INT32;
            (*tv)._pad = [0; 7];
            (*tv).payload = v as i64 as u64;
        }
        _ => {
            // Tagged array: elem is already a LinArrayElem (16 bytes) = TaggedVal layout.
            let elem = (*arr).data.add(idx as usize);
            std::ptr::copy_nonoverlapping(elem as *const u8, tv as *mut u8, std::mem::size_of::<TaggedVal>());
            // Retain the inner payload so the caller owns a reference.
            crate::object::retain_tagged_payload_pub(&*tv);
        }
    }
    tv
}

/// Build a tagged LinArray containing elements from arr[start..end] (for rest patterns).
/// Handles both flat and tagged source arrays.
#[no_mangle]
pub unsafe extern "C" fn lin_array_slice_tagged(arr: *const LinArray, start: i64, end: i64) -> *mut LinArray {
    let len = (*arr).len as i64;
    let start = start.max(0).min(len);
    let end = end.max(start).min(len);
    let count = (end - start) as u64;
    let out = lin_array_alloc(count.max(4));
    for i in 0..count as i64 {
        let tv = lin_array_get_tagged(arr, start + i);
        // Push into tagged output array
        let out_len = (*out).len;
        let out_cap = (*out).cap;
        if out_len == out_cap {
            let new_cap = out_cap * 2;
            let old_layout = array_elem_layout(out_cap);
            let new_layout = array_elem_layout(new_cap);
            (*out).data = std::alloc::realloc((*out).data as *mut u8, old_layout, new_layout.size()) as *mut LinArrayElem;
            (*out).cap = new_cap;
        }
        let slot = (*out).data.add(out_len as usize);
        std::ptr::copy_nonoverlapping(tv as *const u8, slot as *mut u8, std::mem::size_of::<crate::tagged::TaggedVal>());
        (*out).len = out_len + 1;
        // Free the heap TaggedVal since we've copied it.
        dealloc(tv as *mut u8, Layout::from_size_align_unchecked(
            std::mem::size_of::<crate::tagged::TaggedVal>(),
            std::mem::align_of::<crate::tagged::TaggedVal>(),
        ));
    }
    out
}

/// Polymorphic slice: dispatch on the array's runtime `elem_tag` and call the
/// matching typed slice fn. Preserves element type (a UInt8[] yields a UInt8[],
/// an Int32[] yields an Int32[], a tagged Json[] yields a Json[]). Backs the
/// std/array `slice` export and (re-exported) std/bytes `slice`.
#[no_mangle]
pub unsafe extern "C" fn lin_array_slice_dyn(arr: *const u8, start: i64, end: i64) -> *mut u8 {
    use crate::tagged::*;
    if arr.is_null() {
        return alloc_tagged(TAG_ARRAY, lin_array_alloc(4) as u64);
    }
    // `Json` arrays cross the FFI boundary as a TaggedVal*(Array); a raw LinArray*
    // can also arrive from typed array params. Unwrap to the inner LinArray*.
    let tag = *arr;
    let lin_arr = if tag == TAG_ARRAY {
        (*(arr as *const TaggedVal)).payload as *const LinArray
    } else {
        arr as *const LinArray
    };
    let out: *mut LinArray = match (*lin_arr).elem_tag {
        0xFF => lin_array_slice_tagged(lin_arr, start, end),
        TAG_INT32 => lin_flat_array_slice_i32(lin_arr, start, end),
        TAG_INT64 => lin_flat_array_slice_i64(lin_arr, start, end),
        TAG_FLOAT32 => lin_flat_array_slice_f32(lin_arr, start, end),
        TAG_FLOAT64 => lin_flat_array_slice_f64(lin_arr, start, end),
        TAG_UINT8 => lin_flat_array_slice_u8(lin_arr, start, end),
        TAG_INT8 => lin_flat_array_slice_i8(lin_arr, start, end),
        TAG_UINT16 => lin_flat_array_slice_u16(lin_arr, start, end),
        TAG_INT16 => lin_flat_array_slice_i16(lin_arr, start, end),
        // Unknown tag: fall back to a tagged slice (treats elements as 16-byte TaggedVals).
        _ => lin_array_slice_tagged(lin_arr, start, end),
    };
    // Return a Json value: TaggedVal*(Array) wrapping the result.
    alloc_tagged(TAG_ARRAY, out as u64)
}

/// Copy all elements from `src` into `dst` (tagged arrays only).
/// Used by lin concat(a, b) — appends all elements of src to dst.
#[no_mangle]
pub unsafe extern "C" fn lin_array_concat_into(dst: *mut LinArray, src: *const LinArray) {
    if src.is_null() { return; }
    let src_len = (*src).len as usize;
    for i in 0..src_len {
        let elem = (*src).data.add(i);
        lin_array_push_tagged(dst, elem as *const u8);
    }
}

/// Copy all i32 elements from `src` flat array into `dst` flat array.
#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_concat_into_i32(dst: *mut LinArray, src: *const LinArray) {
    if src.is_null() { return; }
    let src_len = (*src).len as usize;
    let src_data = (*src).data as *const i32;
    for i in 0..src_len {
        lin_flat_array_push_i32(dst, *src_data.add(i));
    }
}

/// Copy all i64 elements from `src` flat array into `dst` flat array.
#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_concat_into_i64(dst: *mut LinArray, src: *const LinArray) {
    if src.is_null() { return; }
    let src_len = (*src).len as usize;
    let src_data = (*src).data as *const i64;
    for i in 0..src_len {
        lin_flat_array_push_i64(dst, *src_data.add(i));
    }
}

/// Copy all f32 elements from `src` flat array into `dst` flat array.
#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_concat_into_f32(dst: *mut LinArray, src: *const LinArray) {
    if src.is_null() { return; }
    let src_len = (*src).len as usize;
    let src_data = (*src).data as *const f32;
    for i in 0..src_len {
        lin_flat_array_push_f32(dst, *src_data.add(i));
    }
}

/// Copy all f64 elements from `src` flat array into `dst` flat array.
#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_concat_into_f64(dst: *mut LinArray, src: *const LinArray) {
    if src.is_null() { return; }
    let src_len = (*src).len as usize;
    let src_data = (*src).data as *const f64;
    for i in 0..src_len {
        lin_flat_array_push_f64(dst, *src_data.add(i));
    }
}

/// Tagged-element array equality (structural, element-by-element).
#[no_mangle]
pub unsafe extern "C" fn lin_array_eq(a: *const LinArray, b: *const LinArray) -> u8 {
    if a == b { return 1; }
    if a.is_null() || b.is_null() { return 0; }
    let len = (*a).len;
    if len != (*b).len { return 0; }
    for i in 0..len as usize {
        let ae = (*a).data.add(i);
        let be = (*b).data.add(i);
        if (*ae).tag != (*be).tag { return 0; }
        // Compare payloads — for strings/arrays/objects this is pointer eq (shallow),
        // which matches the spec for the typed-array case where elements are scalars.
        if (*ae).payload != (*be).payload { return 0; }
    }
    1
}

/// Flat i32 array equality.
#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_eq_i32(a: *const LinArray, b: *const LinArray) -> u8 {
    if a == b { return 1; }
    if a.is_null() || b.is_null() { return 0; }
    let len = (*a).len;
    if len != (*b).len { return 0; }
    let da = (*a).data as *const i32;
    let db = (*b).data as *const i32;
    for i in 0..len as usize {
        if *da.add(i) != *db.add(i) { return 0; }
    }
    1
}

/// Flat i64 array equality.
#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_eq_i64(a: *const LinArray, b: *const LinArray) -> u8 {
    if a == b { return 1; }
    if a.is_null() || b.is_null() { return 0; }
    let len = (*a).len;
    if len != (*b).len { return 0; }
    let da = (*a).data as *const i64;
    let db = (*b).data as *const i64;
    for i in 0..len as usize {
        if *da.add(i) != *db.add(i) { return 0; }
    }
    1
}

// -------------------------------------------------------------------------
// Flat (unboxed) scalar arrays
// -------------------------------------------------------------------------
//
// When the element type is a known scalar (i32, i64, f32, f64) the codegen
// emits calls to these functions instead of the tagged LinArrayElem variants.
// Layout: same header as LinArray, but `data` points to raw T-sized elements.
// We reuse the LinArray struct — the `data` pointer just stores T* cast to
// *mut LinArrayElem.  A flat i32 array stores 4-byte elements; the tag byte
// is never written.
//
// Flat array: refcount | elem_tag | _pad3 | len | cap | data(*mut T)
// The `data` field is typed as *mut LinArrayElem for layout compatibility but
// treated as *mut T internally — always accessed via the flat functions below.
// elem_tag stores TAG_INT32/TAG_INT64/TAG_FLOAT32/TAG_FLOAT64 so the equality
// function can dispatch to the right comparison without extra type info.

// --- i32 ---

#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_alloc_i32(initial_cap: u64) -> *mut LinArray {
    let cap = initial_cap.max(4);
    let arr_layout = array_layout();
    let ptr = alloc(arr_layout) as *mut LinArray;
    (*ptr).refcount = 1;
    (*ptr).elem_tag = crate::tagged::TAG_INT32;
    (*ptr)._pad3 = [0; 3];
    (*ptr).len = 0;
    (*ptr).cap = cap;
    let data_layout = Layout::from_size_align_unchecked(
        std::mem::size_of::<i32>() * cap as usize,
        std::mem::align_of::<i32>(),
    );
    (*ptr).data = alloc(data_layout) as *mut LinArrayElem;
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_push_i32(arr: *mut LinArray, val: i32) {
    let len = (*arr).len;
    let cap = (*arr).cap;
    if len == cap {
        let new_cap = cap * 2;
        let old_layout = Layout::from_size_align_unchecked(
            std::mem::size_of::<i32>() * cap as usize,
            std::mem::align_of::<i32>(),
        );
        let new_size = std::mem::size_of::<i32>() * new_cap as usize;
        (*arr).data = realloc((*arr).data as *mut u8, old_layout, new_size) as *mut LinArrayElem;
        (*arr).cap = new_cap;
    }
    let data = (*arr).data as *mut i32;
    *data.add(len as usize) = val;
    (*arr).len = len + 1;
}

#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_get_i32(arr: *const LinArray, idx: i64) -> i32 {
    let len = (*arr).len as i64;
    let actual = if idx < 0 { len + idx } else { idx };
    if actual < 0 || actual >= len {
        eprintln!("Runtime error: array index {} out of bounds (len {})", idx, len);
        std::process::exit(1);
    }
    let data = (*arr).data as *const i32;
    *data.add(actual as usize)
}

#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_free_i32(arr: *mut LinArray) {
    let layout = Layout::from_size_align_unchecked(
        std::mem::size_of::<i32>() * (*arr).cap as usize,
        std::mem::align_of::<i32>(),
    );
    dealloc((*arr).data as *mut u8, layout);
    dealloc(arr as *mut u8, array_layout());
}

// --- i64 ---

#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_alloc_i64(initial_cap: u64) -> *mut LinArray {
    let cap = initial_cap.max(4);
    let arr_layout = array_layout();
    let ptr = alloc(arr_layout) as *mut LinArray;
    (*ptr).refcount = 1;
    (*ptr).elem_tag = crate::tagged::TAG_INT64;
    (*ptr)._pad3 = [0; 3];
    (*ptr).len = 0;
    (*ptr).cap = cap;
    let data_layout = Layout::from_size_align_unchecked(
        std::mem::size_of::<i64>() * cap as usize,
        std::mem::align_of::<i64>(),
    );
    (*ptr).data = alloc(data_layout) as *mut LinArrayElem;
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_push_i64(arr: *mut LinArray, val: i64) {
    let len = (*arr).len;
    let cap = (*arr).cap;
    if len == cap {
        let new_cap = cap * 2;
        let old_layout = Layout::from_size_align_unchecked(
            std::mem::size_of::<i64>() * cap as usize,
            std::mem::align_of::<i64>(),
        );
        let new_size = std::mem::size_of::<i64>() * new_cap as usize;
        (*arr).data = realloc((*arr).data as *mut u8, old_layout, new_size) as *mut LinArrayElem;
        (*arr).cap = new_cap;
    }
    let data = (*arr).data as *mut i64;
    *data.add(len as usize) = val;
    (*arr).len = len + 1;
}

#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_get_i64(arr: *const LinArray, idx: i64) -> i64 {
    let len = (*arr).len as i64;
    let actual = if idx < 0 { len + idx } else { idx };
    if actual < 0 || actual >= len {
        eprintln!("Runtime error: array index {} out of bounds (len {})", idx, len);
        std::process::exit(1);
    }
    let data = (*arr).data as *const i64;
    *data.add(actual as usize)
}

#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_free_i64(arr: *mut LinArray) {
    let layout = Layout::from_size_align_unchecked(
        std::mem::size_of::<i64>() * (*arr).cap as usize,
        std::mem::align_of::<i64>(),
    );
    dealloc((*arr).data as *mut u8, layout);
    dealloc(arr as *mut u8, array_layout());
}

// --- f32 ---

#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_alloc_f32(initial_cap: u64) -> *mut LinArray {
    let cap = initial_cap.max(4);
    let arr_layout = array_layout();
    let ptr = alloc(arr_layout) as *mut LinArray;
    (*ptr).refcount = 1;
    (*ptr).elem_tag = crate::tagged::TAG_FLOAT32;
    (*ptr)._pad3 = [0; 3];
    (*ptr).len = 0;
    (*ptr).cap = cap;
    let data_layout = Layout::from_size_align_unchecked(
        std::mem::size_of::<f32>() * cap as usize,
        std::mem::align_of::<f32>(),
    );
    (*ptr).data = alloc(data_layout) as *mut LinArrayElem;
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_push_f32(arr: *mut LinArray, val: f32) {
    let len = (*arr).len;
    let cap = (*arr).cap;
    if len == cap {
        let new_cap = cap * 2;
        let old_layout = Layout::from_size_align_unchecked(
            std::mem::size_of::<f32>() * cap as usize,
            std::mem::align_of::<f32>(),
        );
        let new_size = std::mem::size_of::<f32>() * new_cap as usize;
        (*arr).data = realloc((*arr).data as *mut u8, old_layout, new_size) as *mut LinArrayElem;
        (*arr).cap = new_cap;
    }
    let data = (*arr).data as *mut f32;
    *data.add(len as usize) = val;
    (*arr).len = len + 1;
}

#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_get_f32(arr: *const LinArray, idx: i64) -> f32 {
    let len = (*arr).len as i64;
    let actual = if idx < 0 { len + idx } else { idx };
    if actual < 0 || actual >= len {
        eprintln!("Runtime error: array index {} out of bounds (len {})", idx, len);
        std::process::exit(1);
    }
    let data = (*arr).data as *const f32;
    *data.add(actual as usize)
}

#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_free_f32(arr: *mut LinArray) {
    let layout = Layout::from_size_align_unchecked(
        std::mem::size_of::<f32>() * (*arr).cap as usize,
        std::mem::align_of::<f32>(),
    );
    dealloc((*arr).data as *mut u8, layout);
    dealloc(arr as *mut u8, array_layout());
}

// --- f64 ---

#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_alloc_f64(initial_cap: u64) -> *mut LinArray {
    let cap = initial_cap.max(4);
    let arr_layout = array_layout();
    let ptr = alloc(arr_layout) as *mut LinArray;
    (*ptr).refcount = 1;
    (*ptr).elem_tag = crate::tagged::TAG_FLOAT64;
    (*ptr)._pad3 = [0; 3];
    (*ptr).len = 0;
    (*ptr).cap = cap;
    let data_layout = Layout::from_size_align_unchecked(
        std::mem::size_of::<f64>() * cap as usize,
        std::mem::align_of::<f64>(),
    );
    (*ptr).data = alloc(data_layout) as *mut LinArrayElem;
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_push_f64(arr: *mut LinArray, val: f64) {
    let len = (*arr).len;
    let cap = (*arr).cap;
    if len == cap {
        let new_cap = cap * 2;
        let old_layout = Layout::from_size_align_unchecked(
            std::mem::size_of::<f64>() * cap as usize,
            std::mem::align_of::<f64>(),
        );
        let new_size = std::mem::size_of::<f64>() * new_cap as usize;
        (*arr).data = realloc((*arr).data as *mut u8, old_layout, new_size) as *mut LinArrayElem;
        (*arr).cap = new_cap;
    }
    let data = (*arr).data as *mut f64;
    *data.add(len as usize) = val;
    (*arr).len = len + 1;
}

#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_get_f64(arr: *const LinArray, idx: i64) -> f64 {
    let len = (*arr).len as i64;
    let actual = if idx < 0 { len + idx } else { idx };
    if actual < 0 || actual >= len {
        eprintln!("Runtime error: array index {} out of bounds (len {})", idx, len);
        std::process::exit(1);
    }
    let data = (*arr).data as *const f64;
    *data.add(actual as usize)
}

#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_free_f64(arr: *mut LinArray) {
    let layout = Layout::from_size_align_unchecked(
        std::mem::size_of::<f64>() * (*arr).cap as usize,
        std::mem::align_of::<f64>(),
    );
    dealloc((*arr).data as *mut u8, layout);
    dealloc(arr as *mut u8, array_layout());
}

// --- Sized allocation helpers ---
// These allocate an array of exactly `len` elements with len==cap and populate
// it immediately, avoiding all push/realloc overhead.

/// Allocate a tagged array of `len` null elements (TAG_NULL, payload=0).
/// All slots are pre-filled; no push calls needed. len is also the capacity.
#[no_mangle]
pub unsafe extern "C" fn lin_array_alloc_null(len: u64) -> *mut LinArray {
    let cap = len.max(1);
    let arr_layout = array_layout();
    let ptr = alloc(arr_layout) as *mut LinArray;
    (*ptr).refcount = 1;
    (*ptr).elem_tag = 0xFF;
    (*ptr)._pad3 = [0; 3];
    (*ptr).len = len;
    (*ptr).cap = cap;
    let elem_layout = array_elem_layout(cap);
    let data = alloc_zeroed(elem_layout) as *mut LinArrayElem;
    (*ptr).data = data;
    // alloc_zeroed fills with 0; tag=0 is TAG_NULL and payload=0 — already correct.
    ptr
}

/// Allocate a flat i32 array of `len` elements all set to `val`.
#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_alloc_filled_i32(len: u64, val: i32) -> *mut LinArray {
    let cap = len.max(1);
    let arr_layout = array_layout();
    let ptr = alloc(arr_layout) as *mut LinArray;
    (*ptr).refcount = 1;
    (*ptr).elem_tag = crate::tagged::TAG_INT32;
    (*ptr)._pad3 = [0; 3];
    (*ptr).len = len;
    (*ptr).cap = cap;
    let data_layout = Layout::from_size_align_unchecked(
        std::mem::size_of::<i32>() * cap as usize,
        std::mem::align_of::<i32>(),
    );
    let data = alloc(data_layout) as *mut i32;
    for i in 0..len as usize { *data.add(i) = val; }
    (*ptr).data = data as *mut LinArrayElem;
    ptr
}

/// Allocate a flat i64 array of `len` elements all set to `val`.
#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_alloc_filled_i64(len: u64, val: i64) -> *mut LinArray {
    let cap = len.max(1);
    let arr_layout = array_layout();
    let ptr = alloc(arr_layout) as *mut LinArray;
    (*ptr).refcount = 1;
    (*ptr).elem_tag = crate::tagged::TAG_INT64;
    (*ptr)._pad3 = [0; 3];
    (*ptr).len = len;
    (*ptr).cap = cap;
    let data_layout = Layout::from_size_align_unchecked(
        std::mem::size_of::<i64>() * cap as usize,
        std::mem::align_of::<i64>(),
    );
    let data = alloc(data_layout) as *mut i64;
    for i in 0..len as usize { *data.add(i) = val; }
    (*ptr).data = data as *mut LinArrayElem;
    ptr
}

/// Allocate a flat f32 array of `len` elements all set to `val`.
#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_alloc_filled_f32(len: u64, val: f32) -> *mut LinArray {
    let cap = len.max(1);
    let arr_layout = array_layout();
    let ptr = alloc(arr_layout) as *mut LinArray;
    (*ptr).refcount = 1;
    (*ptr).elem_tag = crate::tagged::TAG_FLOAT32;
    (*ptr)._pad3 = [0; 3];
    (*ptr).len = len;
    (*ptr).cap = cap;
    let data_layout = Layout::from_size_align_unchecked(
        std::mem::size_of::<f32>() * cap as usize,
        std::mem::align_of::<f32>(),
    );
    let data = alloc(data_layout) as *mut f32;
    for i in 0..len as usize { *data.add(i) = val; }
    (*ptr).data = data as *mut LinArrayElem;
    ptr
}

/// Allocate a flat f64 array of `len` elements all set to `val`.
#[no_mangle]
pub unsafe extern "C" fn lin_flat_array_alloc_filled_f64(len: u64, val: f64) -> *mut LinArray {
    let cap = len.max(1);
    let arr_layout = array_layout();
    let ptr = alloc(arr_layout) as *mut LinArray;
    (*ptr).refcount = 1;
    (*ptr).elem_tag = crate::tagged::TAG_FLOAT64;
    (*ptr)._pad3 = [0; 3];
    (*ptr).len = len;
    (*ptr).cap = cap;
    let data_layout = Layout::from_size_align_unchecked(
        std::mem::size_of::<f64>() * cap as usize,
        std::mem::align_of::<f64>(),
    );
    let data = alloc(data_layout) as *mut f64;
    for i in 0..len as usize { *data.add(i) = val; }
    (*ptr).data = data as *mut LinArrayElem;
    ptr
}

// -------------------------------------------------------------------------
// In-place flat setter + slice for all flat scalar element types.
// -------------------------------------------------------------------------
//
// `lin_flat_array_set_<sfx>` writes a raw scalar at `idx` (Python-style negative
// indices supported; OOB exits like get). `lin_flat_array_slice_<sfx>` copies the
// raw scalar elements arr[start..end] into a freshly allocated flat array of the
// same element type. Bounds semantics mirror `lin_array_slice_tagged`.

macro_rules! flat_set_slice {
    ($t:ty, $set:ident, $slice:ident, $alloc:ident) => {
        #[no_mangle]
        pub unsafe extern "C" fn $set(arr: *mut LinArray, idx: i64, val: $t) {
            let len = (*arr).len as i64;
            let actual = if idx < 0 { len + idx } else { idx };
            if actual < 0 || actual >= len {
                eprintln!("Runtime error: array index {} out of bounds (len {})", idx, len);
                std::process::exit(1);
            }
            let data = (*arr).data as *mut $t;
            *data.add(actual as usize) = val;
        }

        #[no_mangle]
        pub unsafe extern "C" fn $slice(arr: *const LinArray, start: i64, end: i64) -> *mut LinArray {
            let len = (*arr).len as i64;
            let start = start.max(0).min(len);
            let end = end.max(start).min(len);
            let count = (end - start) as u64;
            let out = $alloc(count.max(1));
            let src = (*arr).data as *const $t;
            let dst = (*out).data as *mut $t;
            for i in 0..count as usize {
                *dst.add(i) = *src.add(start as usize + i);
            }
            (*out).len = count;
            out
        }
    };
}

flat_set_slice!(i32, lin_flat_array_set_i32, lin_flat_array_slice_i32, lin_flat_array_alloc_i32);
flat_set_slice!(i64, lin_flat_array_set_i64, lin_flat_array_slice_i64, lin_flat_array_alloc_i64);
flat_set_slice!(f32, lin_flat_array_set_f32, lin_flat_array_slice_f32, lin_flat_array_alloc_f32);
flat_set_slice!(f64, lin_flat_array_set_f64, lin_flat_array_slice_f64, lin_flat_array_alloc_f64);

// -------------------------------------------------------------------------
// Small-integer flat array families: u8 / i8 / u16 / i16.
// -------------------------------------------------------------------------
//
// Identical to the i32 family but with the correct element stride (1 byte for
// u8/i8, 2 bytes for u16/i16). `elem_tag` is set to the matching small-int tag so
// dispatch (e.g. lin_array_set, to_string) can find the right comparison/width.

macro_rules! flat_small_int {
    ($t:ty, $tag:expr, $alloc:ident, $push:ident, $get:ident, $set:ident,
     $free:ident, $filled:ident, $concat:ident, $eq:ident, $slice:ident) => {
        #[no_mangle]
        pub unsafe extern "C" fn $alloc(initial_cap: u64) -> *mut LinArray {
            let cap = initial_cap.max(4);
            let arr_layout = array_layout();
            let ptr = alloc(arr_layout) as *mut LinArray;
            (*ptr).refcount = 1;
            (*ptr).elem_tag = $tag;
            (*ptr)._pad3 = [0; 3];
            (*ptr).len = 0;
            (*ptr).cap = cap;
            let data_layout = Layout::from_size_align_unchecked(
                std::mem::size_of::<$t>() * cap as usize,
                std::mem::align_of::<$t>(),
            );
            (*ptr).data = alloc(data_layout) as *mut LinArrayElem;
            ptr
        }

        #[no_mangle]
        pub unsafe extern "C" fn $push(arr: *mut LinArray, val: $t) {
            let len = (*arr).len;
            let cap = (*arr).cap;
            if len == cap {
                let new_cap = cap * 2;
                let old_layout = Layout::from_size_align_unchecked(
                    std::mem::size_of::<$t>() * cap as usize,
                    std::mem::align_of::<$t>(),
                );
                let new_size = std::mem::size_of::<$t>() * new_cap as usize;
                (*arr).data = realloc((*arr).data as *mut u8, old_layout, new_size) as *mut LinArrayElem;
                (*arr).cap = new_cap;
            }
            let data = (*arr).data as *mut $t;
            *data.add(len as usize) = val;
            (*arr).len = len + 1;
        }

        #[no_mangle]
        pub unsafe extern "C" fn $get(arr: *const LinArray, idx: i64) -> $t {
            let len = (*arr).len as i64;
            let actual = if idx < 0 { len + idx } else { idx };
            if actual < 0 || actual >= len {
                eprintln!("Runtime error: array index {} out of bounds (len {})", idx, len);
                std::process::exit(1);
            }
            let data = (*arr).data as *const $t;
            *data.add(actual as usize)
        }

        #[no_mangle]
        pub unsafe extern "C" fn $set(arr: *mut LinArray, idx: i64, val: $t) {
            let len = (*arr).len as i64;
            let actual = if idx < 0 { len + idx } else { idx };
            if actual < 0 || actual >= len {
                eprintln!("Runtime error: array index {} out of bounds (len {})", idx, len);
                std::process::exit(1);
            }
            let data = (*arr).data as *mut $t;
            *data.add(actual as usize) = val;
        }

        #[no_mangle]
        pub unsafe extern "C" fn $free(arr: *mut LinArray) {
            let layout = Layout::from_size_align_unchecked(
                std::mem::size_of::<$t>() * (*arr).cap as usize,
                std::mem::align_of::<$t>(),
            );
            dealloc((*arr).data as *mut u8, layout);
            dealloc(arr as *mut u8, array_layout());
        }

        #[no_mangle]
        pub unsafe extern "C" fn $filled(len: u64, val: $t) -> *mut LinArray {
            let cap = len.max(1);
            let arr_layout = array_layout();
            let ptr = alloc(arr_layout) as *mut LinArray;
            (*ptr).refcount = 1;
            (*ptr).elem_tag = $tag;
            (*ptr)._pad3 = [0; 3];
            (*ptr).len = len;
            (*ptr).cap = cap;
            let data_layout = Layout::from_size_align_unchecked(
                std::mem::size_of::<$t>() * cap as usize,
                std::mem::align_of::<$t>(),
            );
            let data = alloc(data_layout) as *mut $t;
            for i in 0..len as usize { *data.add(i) = val; }
            (*ptr).data = data as *mut LinArrayElem;
            ptr
        }

        #[no_mangle]
        pub unsafe extern "C" fn $concat(dst: *mut LinArray, src: *const LinArray) {
            if src.is_null() { return; }
            let src_len = (*src).len as usize;
            let src_data = (*src).data as *const $t;
            for i in 0..src_len {
                $push(dst, *src_data.add(i));
            }
        }

        #[no_mangle]
        pub unsafe extern "C" fn $eq(a: *const LinArray, b: *const LinArray) -> u8 {
            if a == b { return 1; }
            if a.is_null() || b.is_null() { return 0; }
            let len = (*a).len;
            if len != (*b).len { return 0; }
            let da = (*a).data as *const $t;
            let db = (*b).data as *const $t;
            for i in 0..len as usize {
                if *da.add(i) != *db.add(i) { return 0; }
            }
            1
        }

        #[no_mangle]
        pub unsafe extern "C" fn $slice(arr: *const LinArray, start: i64, end: i64) -> *mut LinArray {
            let len = (*arr).len as i64;
            let start = start.max(0).min(len);
            let end = end.max(start).min(len);
            let count = (end - start) as u64;
            let out = $alloc(count.max(1));
            let src = (*arr).data as *const $t;
            let dst = (*out).data as *mut $t;
            for i in 0..count as usize {
                *dst.add(i) = *src.add(start as usize + i);
            }
            (*out).len = count;
            out
        }
    };
}

flat_small_int!(u8, crate::tagged::TAG_UINT8,
    lin_flat_array_alloc_u8, lin_flat_array_push_u8, lin_flat_array_get_u8,
    lin_flat_array_set_u8, lin_flat_array_free_u8, lin_flat_array_alloc_filled_u8,
    lin_flat_array_concat_into_u8, lin_flat_array_eq_u8, lin_flat_array_slice_u8);

flat_small_int!(i8, crate::tagged::TAG_INT8,
    lin_flat_array_alloc_i8, lin_flat_array_push_i8, lin_flat_array_get_i8,
    lin_flat_array_set_i8, lin_flat_array_free_i8, lin_flat_array_alloc_filled_i8,
    lin_flat_array_concat_into_i8, lin_flat_array_eq_i8, lin_flat_array_slice_i8);

flat_small_int!(u16, crate::tagged::TAG_UINT16,
    lin_flat_array_alloc_u16, lin_flat_array_push_u16, lin_flat_array_get_u16,
    lin_flat_array_set_u16, lin_flat_array_free_u16, lin_flat_array_alloc_filled_u16,
    lin_flat_array_concat_into_u16, lin_flat_array_eq_u16, lin_flat_array_slice_u16);

flat_small_int!(i16, crate::tagged::TAG_INT16,
    lin_flat_array_alloc_i16, lin_flat_array_push_i16, lin_flat_array_get_i16,
    lin_flat_array_set_i16, lin_flat_array_free_i16, lin_flat_array_alloc_filled_i16,
    lin_flat_array_concat_into_i16, lin_flat_array_eq_i16, lin_flat_array_slice_i16);

/// Convert a flat u8 array to a tagged LinArray (each element tagged as TAG_INT32).
/// Small integers widen to Int32 in the tagged (Json) representation.
#[no_mangle]
pub unsafe extern "C" fn lin_flat_to_tagged_u8(flat: *const LinArray) -> *mut LinArray {
    let len = (*flat).len;
    let tagged = lin_array_alloc(len.max(4));
    let src = (*flat).data as *const u8;
    for i in 0..len as usize {
        let slot = (*tagged).data.add(i);
        (*slot).tag = crate::tagged::TAG_INT32;
        (*slot).payload = *src.add(i) as i64 as u64;
    }
    (*tagged).len = len;
    tagged
}

/// Convert a flat i8 array to a tagged LinArray (each element tagged as TAG_INT32).
#[no_mangle]
pub unsafe extern "C" fn lin_flat_to_tagged_i8(flat: *const LinArray) -> *mut LinArray {
    let len = (*flat).len;
    let tagged = lin_array_alloc(len.max(4));
    let src = (*flat).data as *const i8;
    for i in 0..len as usize {
        let slot = (*tagged).data.add(i);
        (*slot).tag = crate::tagged::TAG_INT32;
        (*slot).payload = *src.add(i) as i64 as u64;
    }
    (*tagged).len = len;
    tagged
}

/// Convert a flat u16 array to a tagged LinArray (each element tagged as TAG_INT32).
#[no_mangle]
pub unsafe extern "C" fn lin_flat_to_tagged_u16(flat: *const LinArray) -> *mut LinArray {
    let len = (*flat).len;
    let tagged = lin_array_alloc(len.max(4));
    let src = (*flat).data as *const u16;
    for i in 0..len as usize {
        let slot = (*tagged).data.add(i);
        (*slot).tag = crate::tagged::TAG_INT32;
        (*slot).payload = *src.add(i) as i64 as u64;
    }
    (*tagged).len = len;
    tagged
}

/// Convert a flat i16 array to a tagged LinArray (each element tagged as TAG_INT32).
#[no_mangle]
pub unsafe extern "C" fn lin_flat_to_tagged_i16(flat: *const LinArray) -> *mut LinArray {
    let len = (*flat).len;
    let tagged = lin_array_alloc(len.max(4));
    let src = (*flat).data as *const i16;
    for i in 0..len as usize {
        let slot = (*tagged).data.add(i);
        (*slot).tag = crate::tagged::TAG_INT32;
        (*slot).payload = *src.add(i) as i64 as u64;
    }
    (*tagged).len = len;
    tagged
}
