//! Columnar record-array runtime POC (design spike — not yet wired to codegen).
//!
//! A columnar array (tag `0xFC`) stores an array of `N` records of type `T = { f0: T0, f1: T1, … }`
//! as one *contiguous column buffer per field* rather than interleaved element payloads. For a
//! `Trip = { dep: Int64, arr: Int64, stop: *LinString }` array:
//!
//! ```text
//! col_ptrs[0] → [dep0, dep1, dep2, …]   (Int64 × N)
//! col_ptrs[1] → [arr0, arr1, arr2, …]   (Int64 × N)
//! col_ptrs[2] → [*str0, *str1, *str2, …] (ptr × N)
//! ```
//!
//! Field read `arr[i].dep` → `*(i64*)(col_ptrs[0] + i*8)` — a pure stride-8 sequential load.
//! Cache density: 8 Int64 values per 64-byte line vs ≈1.6 for a 0xFE (stride-40) buffer.
//!
//! # Layout
//!
//! The header is the existing `LinArray` struct with `elem_tag = 0xFC`:
//!   - `data`   (`*mut LinArrayElem @ 24`) repurposed as `col_ptrs: *mut *mut u8`
//!   - `elem_stride` (u64 @ 32)            repurposed as `n_fields: u64`
//!   - `elem_desc` (ptr @ 40)              repurposed as `col_meta: *const ColMeta`
//!   - `elem_named_desc` (ptr @ 48)        same `NamedDesc` pointer as 0xFE
//!
//! # Scope
//!
//! This module provides the runtime primitives only. Codegen integration (repr lattice variant,
//! MakeArray + SealedArrayFieldGet LLVM emission) is the Phase 1 work described in
//! `docs/design-columnar-arrays.md`.

use std::alloc::{alloc, dealloc, realloc, Layout};

use crate::array::{LinArray, LinArrayElem};

/// `elem_tag` for a columnar record array. One step below `0xFE` (inline AoS stride), `0xFD`
/// (pointer-backed AoS). Kept in lockstep with the prospective `Codegen::COLUMNAR_ARRAY_TAG`.
pub const COLUMNAR_ARRAY_TAG: u8 = 0xFC;

// ---------------------------------------------------------------------------
// Column metadata: the static per-type descriptor
// ---------------------------------------------------------------------------

/// Column-level kind code (same numeric values as `sealed::KIND_*` for the matching kinds).
pub const COL_KIND_SCALAR: u8 = 0; // raw value (i8/i16/i32/i64/u*/f32/f64/bool)
pub const COL_KIND_STRING: u8 = 1; // *LinString — retained pointer
pub const COL_KIND_ARRAY: u8 = 2;  // *LinArray  — retained pointer
pub const COL_KIND_SEALED: u8 = 3; // *sealed_T  — retained pointer

/// Per-field metadata for a columnar array's column. Two fields are enough for the spike; a real
/// implementation would also carry the field name (for dynamic access via a NamedDesc) and the
/// declare-order index.
#[repr(C)]
pub struct ColFieldMeta {
    /// `COL_KIND_*` constant above.
    pub kind: u8,
    pub _pad: [u8; 3],
    /// Byte width of one element in this column (1/2/4/8).
    pub elem_size: u32,
}

/// Static per-type metadata: `n_fields` followed by `n_fields` `ColFieldMeta` entries. Emitted
/// once per columnar type by codegen; the pointer is stored in the `LinArray::elem_desc` slot.
#[repr(C)]
pub struct ColMeta {
    pub n_fields: u32,
    pub _pad: u32,
    // Followed by n_fields ColFieldMeta entries in memory (open-ended C array style, accessed via
    // `col_meta_field(meta, i)`).
}

/// Read field `i`'s `ColFieldMeta` from a `ColMeta` blob. Unsafe: `i` must be < `meta.n_fields`.
#[inline]
pub unsafe fn col_meta_field(meta: *const ColMeta, i: u32) -> &'static ColFieldMeta {
    let base = meta.add(1) as *const ColFieldMeta;
    &*base.add(i as usize)
}

// ---------------------------------------------------------------------------
// Helpers to read the repurposed LinArray header fields for a 0xFC array
// ---------------------------------------------------------------------------

/// Number of columns (stored in `elem_stride`).
#[inline]
pub unsafe fn n_fields(arr: *const LinArray) -> u64 {
    (*arr).elem_stride
}

/// Pointer to the heap-allocated `*mut *mut u8` column-pointer array (stored in `data`).
#[inline]
pub unsafe fn col_ptrs(arr: *const LinArray) -> *mut *mut u8 {
    (*arr).data as *mut *mut u8
}

/// Pointer to the static `ColMeta` blob (stored in `elem_desc`).
#[inline]
pub unsafe fn col_meta(arr: *const LinArray) -> *const ColMeta {
    (*arr).elem_desc as *const ColMeta
}

// ---------------------------------------------------------------------------
// Allocation
// ---------------------------------------------------------------------------

/// Allocate an empty columnar array for a type with `n_fields` columns described by `meta`.
/// `initial_cap` is the element capacity for each column. Returns a `LinArray*` with
/// `elem_tag = 0xFC`. The `col_ptrs` indirection array and all column buffers are allocated.
///
/// # Safety
/// `meta` must point to a `ColMeta` followed by exactly `n_fields` `ColFieldMeta` entries and
/// must remain valid for the lifetime of the array.
#[no_mangle]
pub unsafe extern "C" fn lin_columnar_array_alloc(
    initial_cap: u64,
    n_fields_val: u64,
    meta: *const ColMeta,
    named_desc: *const u8,
) -> *mut LinArray {
    let cap = initial_cap.max(4);

    let arr_layout = Layout::from_size_align_unchecked(
        std::mem::size_of::<LinArray>(),
        std::mem::align_of::<LinArray>(),
    );
    let ptr = alloc(arr_layout) as *mut LinArray;
    if ptr.is_null() {
        std::alloc::handle_alloc_error(arr_layout);
    }
    (*ptr).refcount = 1;
    (*ptr).elem_tag = COLUMNAR_ARRAY_TAG;
    // _pad3 is private; the whole header is zeroed below then fields are set.
    // Zero the entire header first.
    std::ptr::write_bytes(ptr as *mut u8, 0, std::mem::size_of::<LinArray>());
    (*ptr).refcount = 1;
    (*ptr).elem_tag = COLUMNAR_ARRAY_TAG;
    (*ptr).len = 0;
    (*ptr).cap = cap;
    // Repurpose elem_stride as n_fields.
    (*ptr).elem_stride = n_fields_val;
    // Repurpose elem_desc as col_meta.
    (*ptr).elem_desc = meta as *const u8;
    // elem_named_desc: same role as in 0xFE.
    (*ptr).elem_named_desc = named_desc;

    // Allocate the col_ptrs indirection array (one pointer per field).
    let ptrs_layout = Layout::from_size_align_unchecked(
        (n_fields_val as usize) * std::mem::size_of::<*mut u8>(),
        std::mem::align_of::<*mut u8>(),
    );
    let ptrs = alloc(ptrs_layout) as *mut *mut u8;
    if ptrs.is_null() {
        std::alloc::handle_alloc_error(ptrs_layout);
    }
    (*ptr).data = ptrs as *mut LinArrayElem;

    // Allocate each column buffer.
    for i in 0..n_fields_val as u32 {
        let fm = col_meta_field(meta, i);
        let col_layout = Layout::from_size_align_unchecked(
            cap as usize * fm.elem_size as usize,
            fm.elem_size.max(8) as usize,
        );
        let col_buf = alloc(col_layout);
        if col_buf.is_null() {
            std::alloc::handle_alloc_error(col_layout);
        }
        *ptrs.add(i as usize) = col_buf;
    }

    ptr
}

// ---------------------------------------------------------------------------
// Push
// ---------------------------------------------------------------------------

/// Grow all column buffers to `new_cap` when the array is full.
unsafe fn columnar_grow(arr: *mut LinArray) {
    let old_cap = (*arr).cap;
    let new_cap = old_cap * 2;
    let nf = n_fields(arr) as u32;
    let meta = col_meta(arr);
    let ptrs = col_ptrs(arr);

    for i in 0..nf {
        let fm = col_meta_field(meta, i);
        let old_layout = Layout::from_size_align_unchecked(
            old_cap as usize * fm.elem_size as usize,
            fm.elem_size.max(8) as usize,
        );
        let new_size = new_cap as usize * fm.elem_size as usize;
        let new_buf = realloc(*ptrs.add(i as usize), old_layout, new_size);
        if new_buf.is_null() {
            std::alloc::handle_alloc_error(Layout::from_size_align_unchecked(new_size, fm.elem_size.max(8) as usize));
        }
        *ptrs.add(i as usize) = new_buf;
    }
    (*arr).cap = new_cap;
}

/// Append one record to the columnar array from a packed sealed struct (0xFE-style
/// `[rc|size|desc|named_desc|field0|field1|…]` pointer). Field values are scattered
/// column-by-column; scalar fields are copied by value, pointer fields are retained.
///
/// The field order in the struct must match the column order in the `ColMeta`. For the spike,
/// the struct payload begins at `SEALED_HEADER` (24 bytes). Scalar fields are stored at packed
/// offsets matching `sealed_struct_size` arithmetic; pointer fields are 8-byte slots.
///
/// In the full codegen integration, the push-scatter path will emit column stores directly
/// without constructing the intermediate sealed struct (the "push-scatter optimisation").
#[no_mangle]
pub unsafe extern "C" fn lin_columnar_push_from_sealed(
    arr: *mut LinArray,
    sealed_ptr: *const u8,
    // field_offsets: &[u32] — for the spike we accept a static slice via pointer + count
    field_offsets: *const u32,
) {
    if (*arr).len == (*arr).cap {
        columnar_grow(arr);
    }
    let slot = (*arr).len as usize;
    (*arr).len += 1;

    let nf = n_fields(arr) as u32;
    let meta = col_meta(arr);
    let ptrs = col_ptrs(arr);

    for i in 0..nf {
        let fm = col_meta_field(meta, i);
        let src_offset = *field_offsets.add(i as usize);
        let src = sealed_ptr.add(src_offset as usize);
        let dst = (*ptrs.add(i as usize)).add(slot * fm.elem_size as usize);

        if fm.kind == COL_KIND_SCALAR {
            // Copy elem_size bytes from the sealed field slot to the column buffer slot.
            std::ptr::copy_nonoverlapping(src, dst, fm.elem_size as usize);
        } else {
            // Pointer field: read the pointer, retain it, write to column slot.
            let heap_ptr = *(src as *const *mut u8);
            if !heap_ptr.is_null() {
                crate::memory::lin_rc_retain(heap_ptr as *mut u32);
            }
            *(dst as *mut *mut u8) = heap_ptr;
        }
    }
}

// ---------------------------------------------------------------------------
// Field reads (the hot path)
// ---------------------------------------------------------------------------

/// Read an `Int64` value from column `col_idx` at element index `i`.
/// This is the inlined form of `arr[i].field` for an Int64 column.
///
/// # Safety
/// `arr` must be a valid 0xFC columnar array. `col_idx < n_fields(arr)`. `i < arr.len`.
#[inline]
pub unsafe fn lin_columnar_field_get_i64(arr: *const LinArray, i: u64, col_idx: usize) -> i64 {
    let ptrs = col_ptrs(arr);
    let col_ptr = *ptrs.add(col_idx) as *const i64;
    *col_ptr.add(i as usize)
}

/// Read an `Int32` value from column `col_idx` at element index `i`.
#[inline]
pub unsafe fn lin_columnar_field_get_i32(arr: *const LinArray, i: u64, col_idx: usize) -> i32 {
    let ptrs = col_ptrs(arr);
    let col_ptr = *ptrs.add(col_idx) as *const i32;
    *col_ptr.add(i as usize)
}

/// Read a pointer field from column `col_idx` at element index `i`, returning an OWNED (+1) ref.
#[inline]
pub unsafe fn lin_columnar_field_get_ptr(arr: *const LinArray, i: u64, col_idx: usize) -> *mut u8 {
    let ptrs = col_ptrs(arr);
    let col_ptr = *ptrs.add(col_idx) as *const *mut u8;
    let p = *col_ptr.add(i as usize);
    if !p.is_null() {
        crate::memory::lin_rc_retain(p as *mut u32);
    }
    p
}

// ---------------------------------------------------------------------------
// Release
// ---------------------------------------------------------------------------

/// Free the column buffers and the col_ptrs indirection array for a columnar array whose refcount
/// has already reached zero. Called by `lin_array_release` (array.rs) BEFORE `lin_array_free`.
/// Does NOT free the LinArray header (that is done by `lin_array_free`).
/// Does NOT decrement refcount (caller already did that).
///
/// # Safety
/// `arr` must be a valid 0xFC columnar array with refcount already at zero.
pub unsafe fn free_columnar_array_cols(arr: *mut LinArray) {
    let nf = n_fields(arr) as u32;
    let meta = col_meta(arr);
    let ptrs = col_ptrs(arr);
    let len = (*arr).len as usize;
    let cap = (*arr).cap as usize;

    for i in 0..nf {
        let fm = col_meta_field(meta, i);
        if fm.kind != COL_KIND_SCALAR {
            // Release pointer-typed elements (String/Array/Sealed) in this column.
            let col_ptr = *ptrs.add(i as usize) as *mut *mut u8;
            for j in 0..len {
                let p = *col_ptr.add(j);
                if !p.is_null() {
                    match fm.kind {
                        COL_KIND_STRING => crate::string::lin_string_release(p as *mut crate::string::LinString),
                        COL_KIND_ARRAY  => crate::array::lin_array_release(p as *mut LinArray),
                        COL_KIND_SEALED => crate::sealed::lin_sealed_release_self(p),
                        _ => {}
                    }
                }
            }
        }
        // Free the column buffer itself.
        let col_layout = std::alloc::Layout::from_size_align_unchecked(
            cap * fm.elem_size as usize,
            fm.elem_size.max(8) as usize,
        );
        dealloc(*ptrs.add(i as usize), col_layout);
    }

    // Free the col_ptrs indirection array.
    let ptrs_layout = std::alloc::Layout::from_size_align_unchecked(
        nf as usize * std::mem::size_of::<*mut u8>(),
        std::mem::align_of::<*mut u8>(),
    );
    dealloc(ptrs as *mut u8, ptrs_layout);
}

/// Release a columnar array. Decrements refcount; at zero frees all column buffers, the
/// col_ptrs array, and the LinArray header. This is the standalone (direct) release path
/// for columnar arrays not going through `lin_array_release`.
#[no_mangle]
pub unsafe extern "C" fn lin_columnar_array_release(arr: *mut LinArray) {
    if arr.is_null() {
        return;
    }
    let rc = (*arr).refcount;
    if rc == 0 {
        // Already freed — caller bug, but don't double-free.
        return;
    }
    (*arr).refcount = rc - 1;
    if (*arr).refcount > 0 {
        return;
    }

    // Free all column buffers + the col_ptrs array.
    free_columnar_array_cols(arr);

    // Free the LinArray header.
    let arr_layout = Layout::from_size_align_unchecked(
        std::mem::size_of::<LinArray>(),
        std::mem::align_of::<LinArray>(),
    );
    dealloc(arr as *mut u8, arr_layout);
}

// ---------------------------------------------------------------------------
// Tests (prove the layout math and scan)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // A 2-Int64-field + 1-ptr-field record: { dep: Int64, arr: Int64, stop: *u8 }.
    // Static ColMeta blob for this type.
    #[repr(C)]
    struct TripMeta {
        header: ColMeta,
        fields: [ColFieldMeta; 3],
    }

    static TRIP_META: TripMeta = TripMeta {
        header: ColMeta { n_fields: 3, _pad: 0 },
        fields: [
            ColFieldMeta { kind: COL_KIND_SCALAR, _pad: [0;3], elem_size: 8 }, // dep: Int64
            ColFieldMeta { kind: COL_KIND_SCALAR, _pad: [0;3], elem_size: 8 }, // arr: Int64
            ColFieldMeta { kind: COL_KIND_STRING, _pad: [0;3], elem_size: 8 }, // stop: *LinString
        ],
    };

    /// Helper: write an (dep, arr) pair into a columnar array at index `slot` without going
    /// through push_from_sealed (simulates the push-scatter codegen path directly).
    unsafe fn write_elem(arr: *mut LinArray, slot: usize, dep: i64, arr_val: i64) {
        let ptrs = col_ptrs(arr);
        let dep_col = *ptrs.add(0) as *mut i64;
        let arr_col = *ptrs.add(1) as *mut i64;
        let str_col = *ptrs.add(2) as *mut *mut u8;
        *dep_col.add(slot) = dep;
        *arr_col.add(slot) = arr_val;
        *str_col.add(slot) = std::ptr::null_mut();
        (*arr).len += 1;
    }

    #[test]
    fn columnar_field_read() {
        const N: u64 = 1_000_000;
        unsafe {
            let arr = lin_columnar_array_alloc(N, 3, &TRIP_META.header, std::ptr::null());
            assert_eq!((*arr).elem_tag, COLUMNAR_ARRAY_TAG);

            // Populate: dep[i] = i, arr[i] = i * 3.
            for i in 0..N as usize {
                write_elem(arr, i, i as i64, (i * 3) as i64);
            }
            assert_eq!((*arr).len, N);

            // Verify field reads.
            assert_eq!(lin_columnar_field_get_i64(arr, 0, 0), 0);
            assert_eq!(lin_columnar_field_get_i64(arr, 1, 0), 1);
            assert_eq!(lin_columnar_field_get_i64(arr, 999_999, 0), 999_999);
            assert_eq!(lin_columnar_field_get_i64(arr, 0, 1), 0);
            assert_eq!(lin_columnar_field_get_i64(arr, 1, 1), 3);
            assert_eq!(lin_columnar_field_get_i64(arr, 999_999, 1), 2_999_997);

            // Sequential dep-column scan with a running sum (proves cache locality is exercised).
            let mut sum: i64 = 0;
            for i in 0..N {
                sum += lin_columnar_field_get_i64(arr, i, 0);
            }
            // sum = 0 + 1 + … + (N-1) = N*(N-1)/2
            let expected = (N * (N - 1) / 2) as i64;
            assert_eq!(sum, expected);

            lin_columnar_array_release(arr);
        }
    }

    #[test]
    fn columnar_alloc_free_no_elements() {
        unsafe {
            let arr = lin_columnar_array_alloc(4, 3, &TRIP_META.header, std::ptr::null());
            assert_eq!((*arr).len, 0);
            assert_eq!((*arr).cap, 4);
            lin_columnar_array_release(arr);
        }
    }

    #[test]
    fn columnar_grow_on_push() {
        unsafe {
            // Start with cap=4; push 32 elements to force multiple doublings.
            let arr = lin_columnar_array_alloc(4, 3, &TRIP_META.header, std::ptr::null());
            for i in 0..32usize {
                if (*arr).len == (*arr).cap {
                    columnar_grow(arr);
                }
                write_elem(arr, (*arr).len as usize, i as i64, (i * 2) as i64);
            }
            assert_eq!((*arr).len, 32);
            assert!((*arr).cap >= 32);
            // Verify element 31.
            assert_eq!(lin_columnar_field_get_i64(arr, 31, 0), 31);
            assert_eq!(lin_columnar_field_get_i64(arr, 31, 1), 62);
            lin_columnar_array_release(arr);
        }
    }

    /// Lay out math sanity: the dep column pointer must differ from the arr column pointer by
    /// exactly cap*8 bytes — they're in separate allocations, so this just confirms we get two
    /// distinct non-null pointers.
    #[test]
    fn column_pointers_distinct() {
        unsafe {
            let arr = lin_columnar_array_alloc(8, 3, &TRIP_META.header, std::ptr::null());
            let ptrs = col_ptrs(arr);
            let dep_ptr = *ptrs.add(0);
            let arr_ptr = *ptrs.add(1);
            let str_ptr = *ptrs.add(2);
            assert!(!dep_ptr.is_null());
            assert!(!arr_ptr.is_null());
            assert!(!str_ptr.is_null());
            // All three must be distinct allocations.
            assert_ne!(dep_ptr, arr_ptr);
            assert_ne!(dep_ptr, str_ptr);
            assert_ne!(arr_ptr, str_ptr);
            lin_columnar_array_release(arr);
        }
    }
}
