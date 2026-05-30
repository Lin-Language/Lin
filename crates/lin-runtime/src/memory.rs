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

/// Release a closure struct.
///
/// Closure layout (32 bytes, all fields written by compile_closure in codegen):
///   offset  0: u32  refcount
///   offset  4: u32  _pad
///   offset  8: ptr  fn_ptr   (LLVM function pointer)
///   offset 16: ptr  env_ptr  (heap env struct, or null for non-capturing)
///   offset 24: u64  env_size (byte-size of env allocation; 0 when env_ptr is null)
///
/// The env struct itself begins with a u64 size field (redundant with env_size here,
/// but available for future use). After env_size bytes the env allocation is freed.
#[no_mangle]
pub unsafe extern "C" fn lin_closure_release(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    let rc_ptr = ptr as *mut u32;
    // Zero refcount ⇒ double-release (ownership bug); the decrement below would wrap u32.
    // Debug/ASan-only guard, no release-build cost.
    debug_assert!(*rc_ptr > 0, "lin_closure_release: refcount underflow (double free)");
    *rc_ptr -= 1;
    if *rc_ptr == 0 {
        // Read env_ptr (offset 16) and env_size (offset 24).
        let env_ptr = *(ptr.add(16) as *const *mut u8);
        let env_size = *(ptr.add(24) as *const u64);
        // Free the env allocation if present.
        if !env_ptr.is_null() && env_size > 0 {
            let env_layout = std::alloc::Layout::from_size_align_unchecked(env_size as usize, 8);
            std::alloc::dealloc(env_ptr, env_layout);
        }
        // Free the closure struct itself (32 bytes, align 8).
        let cls_layout = std::alloc::Layout::from_size_align_unchecked(32, 8);
        std::alloc::dealloc(ptr, cls_layout);
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
