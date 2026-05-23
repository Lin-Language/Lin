use std::alloc::{alloc, dealloc, realloc, Layout};

/// Heap-allocated growable array.
/// Layout: refcount (u32) | len (u64) | cap (u64) | data (*mut LinArrayElem)
/// Each element is a tagged { tag: u8, pad: [u8;7], payload: u64 } cell.
#[repr(C)]
pub struct LinArray {
    pub refcount: u32,
    _pad: u32,
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
    (*ptr).len = 0;
    (*ptr).cap = cap;
    let elem_layout = array_elem_layout(cap);
    (*ptr).data = alloc(elem_layout) as *mut LinArrayElem;
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn lin_array_free(arr: *mut LinArray) {
    let cap = (*arr).cap;
    dealloc((*arr).data as *mut u8, array_elem_layout(cap));
    dealloc(arr as *mut u8, array_layout());
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
/// Avoids double indirection: the element is stored inline, not as a pointer to TaggedVal.
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
    // Copy 16 bytes (full TaggedVal = LinArrayElem) from tagged into slot.
    std::ptr::copy_nonoverlapping(tagged, slot as *mut u8, 16);
    (*arr).len = len + 1;
}

/// Get a pointer to the element payload at index. Panics (exits) on OOB.
#[no_mangle]
pub unsafe extern "C" fn lin_array_get(arr: *const LinArray, idx: i64) -> *mut LinArrayElem {
    let len = (*arr).len as i64;
    if idx < 0 || idx >= len {
        eprintln!("Runtime error: array index {} out of bounds (len {})", idx, len);
        std::process::exit(1);
    }
    (*arr).data.add(idx as usize)
}

#[no_mangle]
pub unsafe extern "C" fn lin_array_length(arr: *const LinArray) -> i64 {
    (*arr).len as i64
}
