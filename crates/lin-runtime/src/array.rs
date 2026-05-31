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

// A tagged array element IS a `TaggedVal`: `lin_array_get_tagged` reinterprets element memory
// as a TaggedVal and codegen `copy_nonoverlapping(.., 16)` between the two. They must stay
// byte-identical, so pin the layout at compile time.
const _: () = {
    assert!(core::mem::size_of::<LinArrayElem>() == core::mem::size_of::<crate::tagged::TaggedVal>());
    assert!(core::mem::offset_of!(LinArrayElem, tag) == 0);
    assert!(core::mem::offset_of!(LinArrayElem, payload) == 8);
};

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
        TAG_INT32 | TAG_UINT32 | TAG_FLOAT32 => (4, 4),
        TAG_INT64 | TAG_UINT64 | TAG_FLOAT64 => (8, 8),
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

/// Deep-copy a FLAT scalar array (elem_tag != 0xFF): allocate a fresh header + raw element
/// buffer of the same width and copy the bytes verbatim. Flat arrays hold no pointers, so a
/// byte copy is a complete deep copy. Used by the thread-transfer path (transfer.rs).
pub unsafe fn lin_array_clone_flat(src: *const LinArray) -> *mut LinArray {
    let len = (*src).len;
    let cap = (*src).cap.max(4);
    let elem_tag = (*src).elem_tag;
    let (esize, ealign) = flat_elem_size_align(elem_tag);
    let ptr = alloc(array_layout()) as *mut LinArray;
    (*ptr).refcount = 1;
    (*ptr).elem_tag = elem_tag;
    (*ptr)._pad3 = [0; 3];
    (*ptr).len = len;
    (*ptr).cap = cap;
    let data_layout = Layout::from_size_align_unchecked(esize * cap as usize, ealign);
    (*ptr).data = alloc(data_layout) as *mut LinArrayElem;
    if len > 0 {
        std::ptr::copy_nonoverlapping(
            (*src).data as *const u8,
            (*ptr).data as *mut u8,
            esize * len as usize,
        );
    }
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
    // Frozen (immortal) arrays carry a saturated refcount and must never be freed or
    // decremented — they are deep-frozen, shared read-only across threads, and program-lifetime
    // (Frozen<T>, ADR-045). The read-only guard makes retain/release no-ops, so concurrent reads
    // of a frozen graph from N threads never write the refcount → race-free with non-atomic RC.
    if (*arr).refcount >= crate::string::IMMORTAL_RC {
        return;
    }
    // Zero refcount ⇒ double-release (ownership bug); the decrement below would wrap u32.
    // Debug/ASan-only guard, no release-build cost.
    debug_assert!((*arr).refcount > 0, "lin_array_release: refcount underflow (double free)");
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
            TAG_UINT32 => {
                // A boxed UInt32 scalar is TAG_INT64-positive; read it back unsigned.
                let v = match tag {
                    TAG_INT32 => payload as i32 as u32,
                    TAG_INT64 => payload as u32,
                    TAG_UINT64 => payload as u32,
                    TAG_FLOAT64 => f64::from_bits(payload) as u32,
                    _ => 0,
                };
                lin_flat_array_push_u32(arr, v);
            }
            TAG_UINT64 => {
                let v = match tag {
                    TAG_INT32 => payload as i32 as i64 as u64,
                    TAG_INT64 => payload,
                    TAG_UINT64 => payload,
                    TAG_FLOAT64 => f64::from_bits(payload) as u64,
                    _ => 0,
                };
                lin_flat_array_push_u64(arr, v);
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
pub unsafe extern "C-unwind" fn lin_array_get(arr: *const LinArray, idx: i64) -> *mut LinArrayElem {
    let len = (*arr).len as i64;
    let actual = if idx < 0 { len + idx } else { idx };
    if actual < 0 || actual >= len {
        crate::fault::runtime_fault(&format!("Runtime error: array index {} out of bounds (len {})", idx, len));
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

/// Iterable length for the `for`/`while`/`map`/`filter`/`reduce` combinator loop bound when the
/// iterable is a UNION/Json-typed value (a boxed `TaggedVal*`). Returns the backing array's length
/// only when the box actually holds an Array (TAG_ARRAY); for any other runtime kind (Object,
/// String, Null, scalar, …) it returns 0, so the combinator iterates ZERO times rather than
/// misreading the non-array payload as a `LinArray` (a `LinObject`/`LinString` read through the
/// flat/tagged array element path is undefined behaviour — the docs-builder crash, ADR-068 follow-up).
///
/// This keeps `for`/`filter` over a statically-`Json` value SOUND when its runtime value isn't an
/// array (e.g. an `ls()` error object that slipped past a misspelled `isFailure` guard): a no-op
/// loop and an empty result, matching Lin's "bracket access is safe by default" stance — never UB.
/// User-facing `length()` is unaffected (it still routes through `lin_length_dyn`, which reports
/// object key counts / string lengths); only the combinator's internal loop bound uses this.
#[no_mangle]
pub unsafe extern "C" fn lin_iterable_length(p: *const u8) -> i64 {
    use crate::tagged::{TaggedVal, TAG_ARRAY};
    if p.is_null() {
        return 0;
    }
    let tv = p as *const TaggedVal;
    if (*tv).tag != TAG_ARRAY {
        return 0;
    }
    let arr = (*tv).payload as *const LinArray;
    if arr.is_null() {
        return 0;
    }
    (*arr).len as i64
}

/// Get element at index as a heap-allocated TaggedVal*, handling both flat and tagged arrays.
/// The caller is responsible for eventual deallocation. Returns null on OOB.
#[no_mangle]
pub unsafe extern "C-unwind" fn lin_array_get_tagged(arr: *const LinArray, idx: i64) -> *mut crate::tagged::TaggedVal {
    use crate::tagged::*;
    if arr.is_null() { return std::ptr::null_mut(); }
    let len = (*arr).len as i64;
    let actual = if idx < 0 { len + idx } else { idx };
    let idx = actual;
    if idx < 0 || idx >= len {
        crate::fault::runtime_fault(&format!("Runtime error: array index {} out of bounds (len {})", actual, len));
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
        TAG_UINT32 => {
            // Zero-extend the u32 into a positive i64 box (matches the scalar boxing of
            // UInt32, which uses TAG_INT64-positive). A raw u32 may exceed i32 range, so
            // TAG_INT32 would render it signed — TAG_INT64 keeps it positive and exact.
            let v = *((*arr).data as *const u32).add(idx as usize);
            (*tv).tag = TAG_INT64;
            (*tv)._pad = [0; 7];
            (*tv).payload = v as u64;
        }
        TAG_UINT64 => {
            let v = *((*arr).data as *const u64).add(idx as usize);
            (*tv).tag = TAG_UINT64;
            (*tv)._pad = [0; 7];
            (*tv).payload = v;
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
        TAG_UINT32 => lin_flat_array_slice_u32(lin_arr, start, end),
        TAG_UINT64 => lin_flat_array_slice_u64(lin_arr, start, end),
        // Unknown tag: fall back to a tagged slice (treats elements as 16-byte TaggedVals).
        _ => lin_array_slice_tagged(lin_arr, start, end),
    };
    // Return a Json value: TaggedVal*(Array) wrapping the result.
    alloc_tagged(TAG_ARRAY, out as u64)
}

/// Copy all elements from `src` into `dst` (tagged arrays only), MOVING each element's
/// 16-byte `TaggedVal` without retaining its payload. The caller must own the moved-from
/// elements and not release them afterwards (e.g. `concat_dyn`'s widened-flat temp, whose
/// boxes are transferred to `dst` and whose array struct is then freed by `lin_array_free`
/// — which frees only the struct + data buffer, never the element payloads). Using this on
/// a borrowed source that stays live double-counts and leads to a use-after-free; use
/// `lin_array_concat_into_retaining` for that case.
#[no_mangle]
pub unsafe extern "C" fn lin_array_concat_into(dst: *mut LinArray, src: *const LinArray) {
    if src.is_null() { return; }
    let src_len = (*src).len as usize;
    for i in 0..src_len {
        let elem = (*src).data.add(i);
        lin_array_push_tagged(dst, elem as *const u8);
    }
}

/// Copy all elements from a BORROWED `src` into `dst` (tagged arrays only), RETAINING each
/// element's heap payload so `dst` and `src` are independent owners. This is the correct
/// primitive when `src` stays alive after the copy (e.g. `concat(a, b)` where `a`/`b` are the
/// caller's still-owned arrays). Without the retain, both arrays reference the same payload at
/// one refcount, and freeing either frees the shared payload out from under the other —
/// observable as heap corruption when concatenating fresh (non-interned) strings/objects.
#[no_mangle]
pub unsafe extern "C" fn lin_array_concat_into_retaining(dst: *mut LinArray, src: *const LinArray) {
    if src.is_null() { return; }
    let src_len = (*src).len as usize;
    for i in 0..src_len {
        let elem = (*src).data.add(i);
        // Retain the payload before the move-copy so dst is an independent owner.
        // LinArrayElem and TaggedVal share the same {tag, payload} layout (push_tagged
        // raw-copies the 16 bytes between them), so the reinterpret is sound.
        crate::object::retain_tagged_payload_pub(&*(elem as *const crate::tagged::TaggedVal));
        lin_array_push_tagged(dst, elem as *const u8);
    }
}

/// Concatenate `a ++ b`, PRESERVING a flat element representation when both inputs are
/// flat arrays of the same element type. Mirrors `lin_array_slice_dyn`: the pure-Lin
/// `concat` allocated a tagged result (lin_array_allocate), so concatenating two flat
/// `UInt8[]` produced a tagged array — `c[i]` read correctly via the elem_tag-aware get,
/// but byte-level consumers (`u32FromBe`, fs write, FFI) that read `(*arr).data as *const
/// u8` saw 16-byte TaggedVal elements instead of packed bytes. When the two arrays share a
/// flat `elem_tag`, build a flat result of that type; otherwise fall back to a tagged
/// concat. Inputs cross the boundary as `Json` (TaggedVal*(Array)) or raw `LinArray*`.
#[no_mangle]
pub unsafe extern "C" fn lin_array_concat_dyn(a: *const u8, b: *const u8) -> *mut u8 {
    use crate::tagged::*;
    unsafe fn unwrap(p: *const u8) -> *const LinArray {
        if p.is_null() { return std::ptr::null(); }
        if *p == TAG_ARRAY { (*(p as *const TaggedVal)).payload as *const LinArray }
        else { p as *const LinArray }
    }
    let la = unwrap(a);
    let lb = unwrap(b);
    let ta = if la.is_null() { 0xFF } else { (*la).elem_tag };
    let tb = if lb.is_null() { 0xFF } else { (*lb).elem_tag };
    let total = (if la.is_null() { 0 } else { (*la).len }) + (if lb.is_null() { 0 } else { (*lb).len });

    // Both flat and same element type → flat result of that type.
    if ta == tb && ta != 0xFF {
        // (alloc_fn, concat_into_fn) for the shared flat element tag.
        macro_rules! flat_cat {
            ($alloc:ident, $cat:ident) => {{
                let out = $alloc(total.max(1));
                if !la.is_null() { $cat(out, la); }
                if !lb.is_null() { $cat(out, lb); }
                return alloc_tagged(TAG_ARRAY, out as u64);
            }};
        }
        match ta {
            TAG_INT32   => flat_cat!(lin_flat_array_alloc_i32, lin_flat_array_concat_into_i32),
            TAG_INT64   => flat_cat!(lin_flat_array_alloc_i64, lin_flat_array_concat_into_i64),
            TAG_FLOAT32 => flat_cat!(lin_flat_array_alloc_f32, lin_flat_array_concat_into_f32),
            TAG_FLOAT64 => flat_cat!(lin_flat_array_alloc_f64, lin_flat_array_concat_into_f64),
            TAG_UINT8   => flat_cat!(lin_flat_array_alloc_u8,  lin_flat_array_concat_into_u8),
            TAG_INT8    => flat_cat!(lin_flat_array_alloc_i8,  lin_flat_array_concat_into_i8),
            TAG_UINT16  => flat_cat!(lin_flat_array_alloc_u16, lin_flat_array_concat_into_u16),
            TAG_INT16   => flat_cat!(lin_flat_array_alloc_i16, lin_flat_array_concat_into_i16),
            TAG_UINT32  => flat_cat!(lin_flat_array_alloc_u32, lin_flat_array_concat_into_u32),
            TAG_UINT64  => flat_cat!(lin_flat_array_alloc_u64, lin_flat_array_concat_into_u64),
            _ => {} // unknown flat tag: fall through to the tagged path
        }
    }

    // Mixed or tagged element types → tagged result. A flat source is first widened to a
    // tagged array (lin_flat_to_tagged_* boxes its raw scalars) so concat_into can copy
    // its elements as TaggedVals; a tagged source is appended directly.
    let out = lin_array_alloc(total.max(4));
    unsafe fn append_tagged(out: *mut LinArray, src: *const LinArray) {
        if src.is_null() { return; }
        let et = (*src).elem_tag;
        if et == 0xFF {
            // Borrowed tagged source — retain each element so `out` owns its own references.
            lin_array_concat_into_retaining(out, src);
            return;
        }
        let widened: *mut LinArray = match et {
            TAG_INT32   => lin_flat_to_tagged_i32(src),
            TAG_INT64   => lin_flat_to_tagged_i64(src),
            TAG_FLOAT32 => lin_flat_to_tagged_f32(src),
            TAG_FLOAT64 => lin_flat_to_tagged_f64(src),
            TAG_UINT8   => lin_flat_to_tagged_u8(src),
            TAG_INT8    => lin_flat_to_tagged_i8(src),
            TAG_UINT16  => lin_flat_to_tagged_u16(src),
            TAG_INT16   => lin_flat_to_tagged_i16(src),
            TAG_UINT32  => lin_flat_to_tagged_u32(src),
            TAG_UINT64  => lin_flat_to_tagged_u64(src),
            _ => { lin_array_concat_into_retaining(out, src); return; }
        };
        // `widened` is a fresh +1 temp whose boxed scalars are MOVED into `out`; freeing it
        // drops only the struct + buffer (never the element payloads), so this stays a move.
        lin_array_concat_into(out, widened);
        lin_array_free(widened);
    }
    append_tagged(out, la);
    append_tagged(out, lb);
    alloc_tagged(TAG_ARRAY, out as u64)
}

/// Append `item` to the end of `arr`, returning a NEW array. Prepend puts it first.
/// Both PRESERVE the input's representation: a flat array of element tag T stays flat
/// (the item is coerced into T via `lin_push_dyn`); a tagged/`Json` array stays tagged
/// (each copied element AND the item are RETAINED into the result, so the result owns its
/// own +1 reference to every heap payload). Inputs are BORROWED: `arr` is not consumed and
/// `item` is not consumed.
///
/// Unlike `lin_array_concat_into`/`lin_array_concat_dyn` (which copy tagged elements WITHOUT
/// retaining and so rely on a steal-the-reference discipline at the call boundary), this path
/// retains every tagged element it copies. That makes append/prepend RC-self-contained: the
/// returned array can be released independently of `arr`/`item` with no over- or under-release,
/// even when the elements are fresh (non-interned) heap values.
unsafe fn array_unwrap(p: *const u8) -> *const LinArray {
    use crate::tagged::TAG_ARRAY;
    if p.is_null() { return std::ptr::null(); }
    if *p == TAG_ARRAY { (*(p as *const crate::tagged::TaggedVal)).payload as *const LinArray }
    else { p as *const LinArray }
}

/// Allocate a fresh result array matching `src`'s element representation, sized for
/// `src.len + 1` (the appended/prepended item). A flat source yields a flat result of the
/// same `elem_tag`; a tagged/null source yields a tagged result.
unsafe fn alloc_like(src: *const LinArray, extra: u64) -> *mut LinArray {
    use crate::tagged::*;
    let et = if src.is_null() { 0xFF } else { (*src).elem_tag };
    let cap = (if src.is_null() { 0 } else { (*src).len }) + extra;
    match et {
        TAG_INT32   => lin_flat_array_alloc_i32(cap.max(1)),
        TAG_INT64   => lin_flat_array_alloc_i64(cap.max(1)),
        TAG_FLOAT32 => lin_flat_array_alloc_f32(cap.max(1)),
        TAG_FLOAT64 => lin_flat_array_alloc_f64(cap.max(1)),
        TAG_UINT8   => lin_flat_array_alloc_u8(cap.max(1)),
        TAG_INT8    => lin_flat_array_alloc_i8(cap.max(1)),
        TAG_UINT16  => lin_flat_array_alloc_u16(cap.max(1)),
        TAG_INT16   => lin_flat_array_alloc_i16(cap.max(1)),
        TAG_UINT32  => lin_flat_array_alloc_u32(cap.max(1)),
        TAG_UINT64  => lin_flat_array_alloc_u64(cap.max(1)),
        _           => lin_array_alloc(cap.max(4)), // 0xFF tagged / unknown
    }
}

/// Copy every element of `src` into `out`. For a flat `src` this is a raw scalar byte copy
/// (no RC); for a tagged `src` each element is pushed via `lin_push_dyn`, which RETAINS the
/// inner heap payload so `out` owns its own reference. `out` must already match `src`'s
/// representation (see `alloc_like`).
unsafe fn copy_all_retaining(out: *mut LinArray, src: *const LinArray) {
    use crate::tagged::*;
    if src.is_null() { return; }
    match (*src).elem_tag {
        TAG_INT32   => lin_flat_array_concat_into_i32(out, src),
        TAG_INT64   => lin_flat_array_concat_into_i64(out, src),
        TAG_FLOAT32 => lin_flat_array_concat_into_f32(out, src),
        TAG_FLOAT64 => lin_flat_array_concat_into_f64(out, src),
        TAG_UINT8   => lin_flat_array_concat_into_u8(out, src),
        TAG_INT8    => lin_flat_array_concat_into_i8(out, src),
        TAG_UINT16  => lin_flat_array_concat_into_u16(out, src),
        TAG_INT16   => lin_flat_array_concat_into_i16(out, src),
        TAG_UINT32  => lin_flat_array_concat_into_u32(out, src),
        TAG_UINT64  => lin_flat_array_concat_into_u64(out, src),
        _ => {
            // Tagged source: push each element via lin_push_dyn (copies + retains payload).
            let len = (*src).len as usize;
            for i in 0..len {
                let elem = (*src).data.add(i) as *const TaggedVal;
                lin_push_dyn(out, elem);
            }
        }
    }
}

/// `arr ++ [item]` — append `item` at the end, preserving representation. See the doc above.
#[no_mangle]
pub unsafe extern "C" fn lin_array_append_dyn(arr: *const u8, item: *const u8) -> *mut u8 {
    use crate::tagged::*;
    let src = array_unwrap(arr);
    let out = alloc_like(src, 1);
    copy_all_retaining(out, src);
    // lin_push_dyn coerces `item` into a flat element (no RC) or copies+retains it for a
    // tagged result — exactly the per-representation handling we want.
    lin_push_dyn(out, item as *const TaggedVal);
    alloc_tagged(TAG_ARRAY, out as u64)
}

/// `[item] ++ arr` — prepend `item` at the front, preserving representation. See the doc above.
#[no_mangle]
pub unsafe extern "C" fn lin_array_prepend_dyn(arr: *const u8, item: *const u8) -> *mut u8 {
    use crate::tagged::*;
    let src = array_unwrap(arr);
    let out = alloc_like(src, 1);
    lin_push_dyn(out, item as *const TaggedVal);
    copy_all_retaining(out, src);
    alloc_tagged(TAG_ARRAY, out as u64)
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

/// Load element `i` of `arr` into a `TaggedVal` (by value, no heap allocation), handling
/// BOTH tagged arrays (`elem_tag == 0xFF`, elements already laid out as TaggedVal) and flat
/// scalar arrays (raw i8/i16/i32/i64/f32/f64/bool elements). Used by `lin_array_eq` so it can
/// compare any array — including flat ones reached by recursion through a nested heap array.
unsafe fn array_elem_as_tagged(arr: *const LinArray, i: usize) -> crate::tagged::TaggedVal {
    use crate::tagged::*;
    let mut tv: TaggedVal = std::mem::zeroed();
    let et = (*arr).elem_tag;
    if et == 0xFF {
        // Tagged element: copy the in-place TaggedVal-layout element directly.
        let elem = (*arr).data.add(i) as *const TaggedVal;
        return *elem;
    }
    // Flat scalar element: read the raw value of the right width and box it inline.
    match et {
        TAG_INT32 => { tv.tag = TAG_INT32; tv.payload = (*((*arr).data as *const i32).add(i)) as i64 as u64; }
        TAG_INT64 => { tv.tag = TAG_INT64; tv.payload = (*((*arr).data as *const i64).add(i)) as u64; }
        TAG_FLOAT32 => { tv.tag = TAG_FLOAT32; tv.payload = (*((*arr).data as *const f32).add(i)).to_bits() as u64; }
        TAG_FLOAT64 => { tv.tag = TAG_FLOAT64; tv.payload = (*((*arr).data as *const f64).add(i)).to_bits(); }
        TAG_UINT8 => { tv.tag = TAG_INT32; tv.payload = (*((*arr).data as *const u8).add(i)) as i64 as u64; }
        TAG_INT8 => { tv.tag = TAG_INT32; tv.payload = (*((*arr).data as *const i8).add(i)) as i64 as u64; }
        TAG_UINT16 => { tv.tag = TAG_INT32; tv.payload = (*((*arr).data as *const u16).add(i)) as i64 as u64; }
        TAG_INT16 => { tv.tag = TAG_INT32; tv.payload = (*((*arr).data as *const i16).add(i)) as i64 as u64; }
        TAG_UINT32 => { tv.tag = TAG_INT64; tv.payload = (*((*arr).data as *const u32).add(i)) as u64; }
        TAG_UINT64 => { tv.tag = TAG_UINT64; tv.payload = *((*arr).data as *const u64).add(i); }
        TAG_BOOL => { tv.tag = TAG_BOOL; tv.payload = (*((*arr).data as *const u8).add(i)) as u64; }
        _ => { tv.tag = TAG_NULL; }
    }
    tv
}

/// Structural array equality (element-by-element, deep). Works for tagged AND flat scalar
/// arrays, and is the recursion target for nested arrays (a TAG_ARRAY element routes here
/// via `lin_tagged_eq`). Each element is compared via `lin_tagged_eq`, so scalars compare by
/// value (incl. cross-numeric) and heap elements (String/Array/Object) recurse deeply. A raw
/// payload `!=` would compare heap elements by POINTER — wrong for distinct-but-equal values.
#[no_mangle]
pub unsafe extern "C" fn lin_array_eq(a: *const LinArray, b: *const LinArray) -> u8 {
    if a == b { return 1; }
    if a.is_null() || b.is_null() { return 0; }
    let len = (*a).len;
    if len != (*b).len { return 0; }
    // Compare element-by-element via `lin_tagged_eq` uniformly (handles flat and tagged,
    // scalars by value, heap elements deeply); the per-element TaggedVal copy is cheap and
    // avoids reading a flat scalar buffer with the 16-byte tagged stride.
    for i in 0..len as usize {
        let ae = array_elem_as_tagged(a, i);
        let be = array_elem_as_tagged(b, i);
        if crate::tagged::lin_tagged_eq(
            &ae as *const crate::tagged::TaggedVal as *const u8,
            &be as *const crate::tagged::TaggedVal as *const u8,
        ) == 0 {
            return 0;
        }
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
pub unsafe extern "C-unwind" fn lin_flat_array_get_i32(arr: *const LinArray, idx: i64) -> i32 {
    let len = (*arr).len as i64;
    let actual = if idx < 0 { len + idx } else { idx };
    if actual < 0 || actual >= len {
        crate::fault::runtime_fault(&format!("Runtime error: array index {} out of bounds (len {})", idx, len));
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
pub unsafe extern "C-unwind" fn lin_flat_array_get_i64(arr: *const LinArray, idx: i64) -> i64 {
    let len = (*arr).len as i64;
    let actual = if idx < 0 { len + idx } else { idx };
    if actual < 0 || actual >= len {
        crate::fault::runtime_fault(&format!("Runtime error: array index {} out of bounds (len {})", idx, len));
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
pub unsafe extern "C-unwind" fn lin_flat_array_get_f32(arr: *const LinArray, idx: i64) -> f32 {
    let len = (*arr).len as i64;
    let actual = if idx < 0 { len + idx } else { idx };
    if actual < 0 || actual >= len {
        crate::fault::runtime_fault(&format!("Runtime error: array index {} out of bounds (len {})", idx, len));
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
pub unsafe extern "C-unwind" fn lin_flat_array_get_f64(arr: *const LinArray, idx: i64) -> f64 {
    let len = (*arr).len as i64;
    let actual = if idx < 0 { len + idx } else { idx };
    if actual < 0 || actual >= len {
        crate::fault::runtime_fault(&format!("Runtime error: array index {} out of bounds (len {})", idx, len));
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
                crate::fault::runtime_fault(&format!("Runtime error: array index {} out of bounds (len {})", idx, len));
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
                crate::fault::runtime_fault(&format!("Runtime error: array index {} out of bounds (len {})", idx, len));
            }
            let data = (*arr).data as *const $t;
            *data.add(actual as usize)
        }

        #[no_mangle]
        pub unsafe extern "C" fn $set(arr: *mut LinArray, idx: i64, val: $t) {
            let len = (*arr).len as i64;
            let actual = if idx < 0 { len + idx } else { idx };
            if actual < 0 || actual >= len {
                crate::fault::runtime_fault(&format!("Runtime error: array index {} out of bounds (len {})", idx, len));
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

// Unsigned 32/64-bit flat families. Same generated shape as the small-int families, but
// elem_tag carries TAG_UINT32/TAG_UINT64 so display/JSON read the elements UNSIGNED.
// (Signed i32/i64 keep their own families with TAG_INT32/TAG_INT64.) The macro generates
// set+slice itself, so there is NO name collision with the flat_set_slice! families (those
// only cover i32/i64/f32/f64 — distinct symbol names).
flat_small_int!(u32, crate::tagged::TAG_UINT32,
    lin_flat_array_alloc_u32, lin_flat_array_push_u32, lin_flat_array_get_u32,
    lin_flat_array_set_u32, lin_flat_array_free_u32, lin_flat_array_alloc_filled_u32,
    lin_flat_array_concat_into_u32, lin_flat_array_eq_u32, lin_flat_array_slice_u32);

flat_small_int!(u64, crate::tagged::TAG_UINT64,
    lin_flat_array_alloc_u64, lin_flat_array_push_u64, lin_flat_array_get_u64,
    lin_flat_array_set_u64, lin_flat_array_free_u64, lin_flat_array_alloc_filled_u64,
    lin_flat_array_concat_into_u64, lin_flat_array_eq_u64, lin_flat_array_slice_u64);

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

/// Convert a flat u32 array to a tagged LinArray. Each element is zero-extended into a
/// positive Int64 box (TAG_INT64), matching how a boxed UInt32 scalar is represented, so
/// values above i32::MAX render unsigned (e.g. 4294967295) instead of negative.
#[no_mangle]
pub unsafe extern "C" fn lin_flat_to_tagged_u32(flat: *const LinArray) -> *mut LinArray {
    let len = (*flat).len;
    let tagged = lin_array_alloc(len.max(4));
    let src = (*flat).data as *const u32;
    for i in 0..len as usize {
        let slot = (*tagged).data.add(i);
        (*slot).tag = crate::tagged::TAG_INT64;
        (*slot).payload = *src.add(i) as u64;
    }
    (*tagged).len = len;
    tagged
}

/// Convert a flat u64 array to a tagged LinArray. Each element is tagged TAG_UINT64 so the
/// payload is read back unsigned (matching the boxed UInt64 scalar representation).
#[no_mangle]
pub unsafe extern "C" fn lin_flat_to_tagged_u64(flat: *const LinArray) -> *mut LinArray {
    let len = (*flat).len;
    let tagged = lin_array_alloc(len.max(4));
    let src = (*flat).data as *const u64;
    for i in 0..len as usize {
        let slot = (*tagged).data.add(i);
        (*slot).tag = crate::tagged::TAG_UINT64;
        (*slot).payload = *src.add(i);
    }
    (*tagged).len = len;
    tagged
}

#[cfg(test)]
mod free_layout_tests {
    use super::*;
    use crate::tagged::*;

    // Regression: lin_array_free must size the data-buffer dealloc from elem_tag, not
    // always assume the 16-byte tagged LinArrayElem. Freeing a flat UInt8[] (1 byte/elem)
    // with the tagged layout deallocs 16x too much and corrupts the heap. This pure-logic
    // check fails instantly if anyone reverts flat_elem_size_align to the 16-byte size,
    // without needing the non-deterministic ASan crash to happen to trigger.
    #[test]
    fn flat_elem_size_align_matches_alloc_width() {
        assert_eq!(flat_elem_size_align(TAG_UINT8), (1, 1));
        assert_eq!(flat_elem_size_align(TAG_INT8), (1, 1));
        assert_eq!(flat_elem_size_align(TAG_UINT16), (2, 2));
        assert_eq!(flat_elem_size_align(TAG_INT16), (2, 2));
        assert_eq!(flat_elem_size_align(TAG_INT32), (4, 4));
        assert_eq!(flat_elem_size_align(TAG_UINT32), (4, 4));
        assert_eq!(flat_elem_size_align(TAG_FLOAT32), (4, 4));
        assert_eq!(flat_elem_size_align(TAG_INT64), (8, 8));
        assert_eq!(flat_elem_size_align(TAG_UINT64), (8, 8));
        assert_eq!(flat_elem_size_align(TAG_FLOAT64), (8, 8));
        // Tagged arrays (0xFF) and anything unknown use the 16-byte element.
        assert_eq!(
            flat_elem_size_align(0xFF),
            (std::mem::size_of::<LinArrayElem>(), std::mem::align_of::<LinArrayElem>())
        );
    }

    // Alloc/grow/release cycles for the flat widths. The release frees the data buffer via
    // lin_array_free; with the wrong layout this is an allocator mismatch that miri and
    // `cargo test` under -Zsanitizer=address catch deterministically.
    #[test]
    fn flat_u8_alloc_push_release_roundtrips() {
        unsafe {
            // Start at cap 4 and push past it to force a realloc, exercising the grow path.
            let arr = lin_flat_array_alloc_u8(4);
            for b in 0u16..=300 {
                lin_flat_array_push_u8(arr, b as u8);
            }
            assert_eq!((*arr).len, 301);
            assert_eq!(lin_flat_array_get_u8(arr, 0), 0);
            assert_eq!(lin_flat_array_get_u8(arr, 255), 255);
            lin_array_release(arr); // refcount starts at 1 -> frees here
        }
    }

    #[test]
    fn flat_widths_release_cleanly() {
        unsafe {
            let a = lin_flat_array_alloc_i16(4);
            for v in 0..50i16 { lin_flat_array_push_i16(a, v); }
            lin_array_release(a);

            let b = lin_flat_array_alloc_u16(4);
            for v in 0..50u16 { lin_flat_array_push_u16(b, v); }
            lin_array_release(b);

            let c = lin_flat_array_alloc_i8(4);
            for v in 0..50i8 { lin_flat_array_push_i8(c, v); }
            lin_array_release(c);
        }
    }

    // append/prepend on a FLAT u8 array must stay flat (elem_tag preserved) and place the new
    // element's raw byte in the right slot — the latent-bug check (a tagged result would store a
    // 16-byte TaggedVal and `data as *const u8` would read a zero/garbage byte at index 2).
    #[test]
    fn append_prepend_preserve_flat_u8_representation() {
        unsafe {
            let b = lin_flat_array_alloc_u8(4);
            lin_flat_array_push_u8(b, 1);
            lin_flat_array_push_u8(b, 2);
            // A u8 value crosses as a boxed scalar: small ints box as TAG_INT32 (lin_box_int32),
            // so the item arrives tagged TAG_INT32. lin_push_dyn coerces it into the flat u8 slot.
            let item = alloc_tagged(TAG_INT32, 3);

            // append -> [1,2,3], still flat u8
            let app_box = lin_array_append_dyn(b as *const u8, item as *const u8);
            let app = (*(app_box as *const TaggedVal)).payload as *mut LinArray;
            assert_eq!((*app).elem_tag, TAG_UINT8, "append result must stay flat u8");
            assert_eq!((*app).len, 3);
            let bytes = std::slice::from_raw_parts((*app).data as *const u8, 3);
            assert_eq!(bytes, &[1u8, 2, 3], "raw packed bytes (NOT zero at idx 2)");

            // prepend -> [3,1,2], still flat u8
            let pre_box = lin_array_prepend_dyn(b as *const u8, item as *const u8);
            let pre = (*(pre_box as *const TaggedVal)).payload as *mut LinArray;
            assert_eq!((*pre).elem_tag, TAG_UINT8, "prepend result must stay flat u8");
            let pbytes = std::slice::from_raw_parts((*pre).data as *const u8, 3);
            assert_eq!(pbytes, &[3u8, 1, 2]);

            // Input `b` is borrowed and unchanged.
            assert_eq!((*b).len, 2);

            // The result arrays (rc=1) are owned by their returned boxes; release via the box
            // exactly once (releasing both the box AND the array directly would double-free).
            lin_tagged_release(app_box);
            lin_tagged_release(pre_box);
            lin_array_release(b);
            lin_tagged_release(item as *mut u8);
        }
    }

    // append/prepend on a TAGGED String array: each copied element AND the item must be
    // RC-retained into the result, so releasing the result frees its own refs without touching
    // the borrowed inputs. Under ASan a missing retain surfaces as a UAF when reading back the
    // strings after the inputs drop; a missing release surfaces as a leak. Loop to amplify.
    #[test]
    fn append_prepend_tagged_strings_rc_balanced() {
        unsafe {
            // Build the growing-accumulator pattern that the pure-Lin/concat path mishandles:
            // acc = append(acc, freshString) in a loop, releasing the previous acc each round.
            let mk = |s: &str| crate::string::lin_string_from_bytes(s.as_ptr(), s.len() as u32);
            let mut acc = lin_array_alloc(4); // empty tagged
            (*acc).len = 0;
            for i in 0..200 {
                let s = mk(&format!("item{i}")); // fresh (non-interned) string, rc=1
                let item_box = alloc_tagged(TAG_STR, s as u64); // owns the +1 via this box
                let next_box = lin_array_append_dyn(
                    acc as *const u8, item_box as *const u8,
                );
                // Release the item box (append retained its own ref into the result).
                lin_tagged_release(item_box);
                // Release the previous acc (its elements were retained into next, so they live).
                lin_array_release(acc);
                acc = (*(next_box as *const TaggedVal)).payload as *mut LinArray;
                // Free only the box shell; the inner array survives in `acc`.
                crate::tagged::lin_tagged_free_box(next_box);
            }
            assert_eq!((*acc).len, 200);
            // Read back every string — a UAF here means a retain was missing.
            for i in 0..200usize {
                let elem = (*acc).data.add(i) as *const TaggedVal;
                assert_eq!((*elem).tag, TAG_STR);
                let sp = (*elem).payload as *const crate::string::LinString;
                let want = format!("item{i}");
                let wp = mk(&want);
                assert!(crate::string::lin_string_eq(sp, wp), "element {i} survived RC and matches");
                crate::string::lin_string_release(wp);
            }
            lin_array_release(acc);
        }
    }
}
