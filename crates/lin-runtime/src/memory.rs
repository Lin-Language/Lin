/// Heap allocation and reference counting for Lin values.

/// LIN_RC_COUNT instrumentation (Stage 4 measurement). When `LIN_RC_COUNT=1`, atomic counters
/// track retain/release traffic and are printed at process exit. Zero runtime cost when env var
/// is not set (checked once at startup via `RC_COUNT_ENABLED`).
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
pub static RC_COUNT_ENABLED: AtomicBool = AtomicBool::new(false);
pub static RC_RETAIN_COUNT: AtomicU64 = AtomicU64::new(0);
pub static RC_RELEASE_COUNT: AtomicU64 = AtomicU64::new(0);
pub static SEALED_RETAIN_COUNT: AtomicU64 = AtomicU64::new(0);
pub static SEALED_RELEASE_COUNT: AtomicU64 = AtomicU64::new(0);

/// LIN_RC_COUNT: auto-init on first retain call (lazy). Registers lin_rc_count_print via atexit.
fn ensure_rc_count_init() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        if std::env::var("LIN_RC_COUNT").as_deref() == Ok("1") {
            RC_COUNT_ENABLED.store(true, Ordering::Relaxed);
            // Register print at normal process exit (complements the lin_exit path).
            unsafe { libc::atexit(rc_count_atexit); }
        }
    });
}

extern "C" fn rc_count_atexit() {
    lin_rc_count_print();
}

/// Called at program exit to print RC counts if enabled.
#[no_mangle]
pub extern "C" fn lin_rc_count_print() {
    if RC_COUNT_ENABLED.load(Ordering::Relaxed) {
        let retain = RC_RETAIN_COUNT.load(Ordering::Relaxed);
        let release = RC_RELEASE_COUNT.load(Ordering::Relaxed);
        let sretain = SEALED_RETAIN_COUNT.load(Ordering::Relaxed);
        let srelease = SEALED_RELEASE_COUNT.load(Ordering::Relaxed);
        eprintln!("RC_COUNT: retain={retain} release={release} sealed_retain={sretain} sealed_release={srelease}");
    }
}

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

/// Free a captured-`var` heap cell allocated by codegen via `lin_alloc(size)`.
///
/// A cell is a raw `size`-byte allocation (8 or 16 bytes, align 8) with NO refcount header:
/// it just holds the current value of a mutably-captured `var`, shared by reference among
/// the closures that capture it. The IR-level `FreeCell` instruction releases the cell's
/// owned VALUE first (via the tag-aware / concrete release path), then calls this to reclaim
/// the cell allocation itself. Only emitted for cells the lowerer has PROVEN non-escaping
/// (every capturing closure is a synchronous, non-retained combinator callback argument), so
/// the pointer is unique and dead at this point. Null- and zero-size-safe.
#[no_mangle]
pub unsafe extern "C" fn lin_cell_free(ptr: *mut u8, size: usize) {
    if ptr.is_null() || size == 0 {
        return;
    }
    let layout = std::alloc::Layout::from_size_align_unchecked(size, 8);
    std::alloc::dealloc(ptr, layout);
}

/// Release a closure struct.
///
/// Closure layout (48 bytes, all fields written by codegen `make_closure_struct_desc_caps`):
///   offset  0: u32  refcount
///   offset  4: u32  _pad
///   offset  8: ptr  fn_ptr   (LLVM function pointer)
///   offset 16: ptr  env_ptr  (heap env struct, or null for non-capturing)
///   offset 24: u64  env_size (byte-size of env allocation; 0 when env_ptr is null)
///   offset 32: ptr  default_descriptor (static global, or null; never freed here)
///   offset 40: ptr  capture_descriptor (static global, or null; never freed here)
///
/// The env struct is `{ u64 size, cap0, cap1, ... }` — captures at byte offset `8 + i*8`.
/// The capture descriptor (ADR-041) is `{ u32 count, u8 kinds[count] }`: when non-null, the
/// closure OWNS one reference per owning capture, so each is released here before the env is
/// freed (mirroring `lin_array_release`/`lin_object_release` recursive element release). A null
/// capture descriptor means borrow-only captures (partial applications) — nothing to release.
/// Both descriptors are static read-only globals and are never freed.
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
        // Read env_ptr (offset 16), env_size (offset 24), capture descriptor (offset 40).
        let env_ptr = *(ptr.add(16) as *const *mut u8);
        let env_size = *(ptr.add(24) as *const u64);
        let cap_desc = *(ptr.add(40) as *const *const u8);
        // Release owning captures (the env owns one ref per owning capture). Each capture word
        // lives at env offset 8 + i*8; the descriptor's kind byte says how to release it.
        if !env_ptr.is_null() && !cap_desc.is_null() {
            release_captures(env_ptr, cap_desc);
        }
        // Free the env allocation if present.
        if !env_ptr.is_null() && env_size > 0 {
            let env_layout = std::alloc::Layout::from_size_align_unchecked(env_size as usize, 8);
            std::alloc::dealloc(env_ptr, env_layout);
        }
        // Free the closure struct itself (CLOSURE_SIZE = 48 bytes, align 8). Both descriptors
        // (offsets 32, 40) are static globals and are not freed.
        let cls_layout = std::alloc::Layout::from_size_align_unchecked(48, 8);
        std::alloc::dealloc(ptr, cls_layout);
    }
}

/// Release each owning capture in `env_ptr` per the capture descriptor `{u32 count, u8 kinds[]}`.
/// Kind codes mirror `lin_ir::ir::CaptureRelease::code()`:
///   0 None (scalar / borrowed var-cell pointer — skip), 1 Str, 2 Array, 3 Object,
///   4 Closure, 5 Tagged (boxed TaggedVal*).
unsafe fn release_captures(env_ptr: *mut u8, cap_desc: *const u8) {
    let count = *(cap_desc as *const u32) as usize;
    let kinds = cap_desc.add(std::mem::size_of::<u32>());
    for i in 0..count {
        let word = *(env_ptr.add(8 + i * 8) as *const *mut u8);
        match *kinds.add(i) {
            1 => crate::string::lin_string_release(word as *mut crate::string::LinString),
            2 => crate::array::lin_array_release(word as *mut crate::array::LinArray),
            // CAP_OBJECT (3): after Phase 3, open objects are LinMap-backed; release via lin_map_release.
            3 => crate::map::lin_map_release(word as *mut crate::map::LinMap),
            4 => lin_closure_release(word),
            5 => crate::tagged::lin_tagged_release(word),
            // 7 Sealed: a captured sealed scalar record (packed struct). Release via its self-sized
            // release (reads the byte size from the struct's offset-4 header); NEVER lin_object_release
            // (that would walk the struct as a LinObject — heap-buffer-overflow, ASan-caught).
            7 => crate::sealed::lin_sealed_release_self(word),
            // 6 Move: a MOVED resource (Stream) was HANDED OFF to a worker — the LOCAL closure
            // must NOT release it (the worker's `release_env_copy` owns + releases it). Falls to
            // the no-op arm deliberately. (If a moved-capture closure is ever torn down WITHOUT a
            // handoff — e.g. an inline-fallback spawn — see the note in async_rt: the inline path
            // does not use a deep env copy, so the move degenerates to the source owning it; that
            // is handled there, not here.)
            _ => {} // 0: scalar or borrowed cell pointer (or 6 Move: handed off) — nothing to release
        }
    }
}

/// Reference counting operations for heap-allocated Lin values.

/// Atomic init-flag: 0=unchecked, 1=checked.
static RC_COUNT_CHECKED: AtomicBool = AtomicBool::new(false);

#[no_mangle]
pub extern "C" fn lin_rc_retain(ptr: *mut u32) {
    // Lazy init: check env var exactly once.
    if !RC_COUNT_CHECKED.load(Ordering::Relaxed) {
        RC_COUNT_CHECKED.store(true, Ordering::Relaxed);
        ensure_rc_count_init();
    }
    if !ptr.is_null() {
        unsafe {
            // Immortal (interned) string literals carry a saturated refcount (>= IMMORTAL_RC).
            // Strings reach this path via the codegen `Retain` instruction for `Type::Str`, so a
            // retain of an interned literal must be a no-op to keep its refcount from climbing past
            // u32::MAX. Arrays/objects/closures (the other users of this offset-0 refcount) can
            // never reach 2^31 live owners, so the guard never affects them. Mirror of the
            // sentinel guard in lin_string_release.
            if *ptr >= crate::string::IMMORTAL_RC {
                return;
            }
            *ptr += 1;
        }
    }
    if RC_COUNT_ENABLED.load(Ordering::Relaxed) {
        RC_RETAIN_COUNT.fetch_add(1, Ordering::Relaxed);
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
