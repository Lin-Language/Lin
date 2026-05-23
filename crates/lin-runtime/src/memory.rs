/// Heap allocation and reference counting for Lin values.

/// Allocate `size` bytes on the heap, returning a raw pointer.
/// Aborts on allocation failure.
#[no_mangle]
pub extern "C" fn lin_alloc(size: usize) -> *mut u8 {
    if size == 0 {
        return std::ptr::NonNull::dangling().as_ptr();
    }
    unsafe {
        let layout = std::alloc::Layout::from_size_align_unchecked(size, 8);
        let ptr = std::alloc::alloc(layout);
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        ptr
    }
}

/// Reference counting operations for heap-allocated Lin values.

#[no_mangle]
pub extern "C" fn lin_rc_retain(ptr: *mut u32) {
    if !ptr.is_null() {
        unsafe {
            *ptr += 1;
        }
    }
}

#[no_mangle]
pub extern "C" fn lin_rc_release(ptr: *mut u32, size: usize, align: usize) {
    if ptr.is_null() {
        return;
    }
    unsafe {
        *ptr -= 1;
        if *ptr == 0 {
            let layout = std::alloc::Layout::from_size_align_unchecked(size, align);
            std::alloc::dealloc(ptr as *mut u8, layout);
        }
    }
}
