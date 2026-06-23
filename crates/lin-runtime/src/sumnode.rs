//! Unboxed tagged sum-type runtime support (unboxed-sumtype design, Stage 1).
//!
//! A *sum type* `type T = A | B | …` where every variant is a sealed record sharing a distinct
//! `StrLit` discriminant field and (Stage 1) every OTHER field is an unboxed scalar gets an unboxed
//! heap `SumNode` representation. The node mirrors the sealed-record header (`sealed.rs`) so the
//! existing RC primitive (`lin_rc_retain` bumping the u32 at offset 0) and the IMMORTAL_RC stack
//! sentinel work UNCHANGED:
//!
//! ```text
//! [ u32 refcount | u32 size | u64 desc_ptr | u32 tag | u32 _pad | <payload, max-variant-sized> ]
//!    @0            @4         @8             @16        @20        @24...
//! ```
//!
//!   - offset 0  (refcount) — identical to sealed records → `lin_rc_retain` works verbatim.
//!   - offset 4  (size)     — total byte size = `SUMNODE_HEADER + max_variant_payload`, so a value
//!     can be released without the caller knowing its variant (`lin_sumnode_release_self`).
//!   - offset 8  (desc_ptr) — pointer to a static `SumDesc` (the per-variant heap-field table, for
//!     the recursive drop walk). Stage 1 is SCALAR-ONLY so every variant's heap-field list is empty
//!     and the descriptor pointer may be NULL — drop is a pure refcount decrement + free.
//!   - offset 16 (tag)      — the inline discriminant: a small dense integer (0,1,2…), one per
//!     variant in declaration order. The match/`is` switch key. Stored inline, NOT in a TaggedVal.
//!   - offset 24 (payload)  — the variant's fields packed exactly like a sealed record's field block
//!     (`Codegen::sealed_field_layout`), sized to the MAX over all variants so every variant fits in
//!     one fixed-size node.
//!
//! ## `SumDesc` (static, one per sum type) — Stage 2 soundness mechanism
//! Like `SealedDesc` but per-VARIANT, because heap-field offsets differ by variant:
//! ```text
//! SumDesc     = [ u32 variant_count | VariantDesc * variant_count ]
//! VariantDesc = [ u32 heap_field_count | { u32 byte_offset, u32 kind } * heap_field_count ]
//! ```
//! `kind` extends the sealed `KIND_*` set; a recursive child (Stage 2) uses `KIND_SUMNODE`
//! (= `sealed::KIND_MAP` = 4 — same value, disjoint descriptor namespace).
//! Drop reads `tag@16`, indexes into `SumDesc` to get that variant's heap-field list, releases each,
//! then frees. For Stage 1 (scalar-only) every per-variant list is empty.

use std::alloc::{alloc, dealloc, Layout};

/// Header size in bytes: `u32 rc | u32 size | u64 desc_ptr | u32 tag | u32 _pad`. Payload begins at
/// offset 24. Kept in lockstep with `Codegen::SUMNODE_HEADER`.
pub const SUMNODE_HEADER: usize = 24;

/// Byte offset of the inline discriminant tag (`u32`). Kept in lockstep with
/// `Codegen::SUMNODE_TAG_OFFSET`.
pub const SUMNODE_TAG_OFFSET: usize = 16;

// A recursive-child kind (Stage 2). Reuses the same numeric value as `sealed::KIND_MAP` (4)
// because the two live in DISJOINT descriptor namespaces: sealed descriptors are walked by
// `sealed::release_field`, sumnode descriptors by `sumnode::release_field` — neither function
// is ever called on the other's descriptor. Reusing the constant avoids a redundant definition
// while keeping the single source of truth in `sealed.rs`.
//
// HAZARD: if the namespace invariant is ever violated (a SumNode variant descriptor is walked
// by `sealed::release_field` or vice versa), `KIND_SUMNODE=4` would be misread as `KIND_MAP`
// and the payload treated as a `*LinMap` instead of a `*SumNode`, causing a wild free. The
// `debug_assert` in `release_field` below catches this in debug builds if a MAP kind somehow
// reaches the sumnode walk.
/// A recursive `*SumNode` child field → `lin_sumnode_release_self` on drop. (Stage 2.)
/// Numeric alias of `sealed::KIND_MAP`; safe because the two descriptor namespaces are disjoint.
pub const KIND_SUMNODE: u32 = crate::sealed::KIND_MAP;

/// Byte offset of the heap-field drop table within a SumDesc. The descriptor begins with an 8-byte
/// MATERIALIZER fn-ptr (`*SumNode -> *LinObject`, the keep-packed-through-record-fields boundary
/// materializer); the `[u32 variant_count | VariantDesc*]` drop table follows. Kept in lockstep with
/// `Codegen::sumnode_descriptor` (which emits `{ ptr matfn, [N x i32] table }`).
pub const SUMDESC_TABLE_OFFSET: usize = 8;

/// The materializer function pointer signature: `*SumNode -> *LinMap` (the per-type
/// `lin_summat_<key>` codegen emits). Used to materialize a kept-packed `TAG_SUMNODE` slot that
/// escaped a record field into the type-erased dynamic domain (toString/eq/json).
type SumMatFn = unsafe extern "C" fn(*mut u8) -> *mut u8;

/// Materialize an unboxed `*SumNode` to a fresh +1 `LinMap*`, via the per-type materializer
/// fn-ptr stored at the head of the node's SumDesc. Returns a `*mut LinMap` cast as `*mut u8`.
/// Null/desc-less safe (returns null — a scalar-only sum type still carries a descriptor under
/// this scheme, so the desc is non-null for every keep-packed-eligible node). Used by the runtime
/// dynamic-boundary walkers (`lin_tagged_to_string` / `push_json_value` / `lin_tagged_eq`) for a
/// TAG_SUMNODE payload.
#[no_mangle]
pub unsafe extern "C" fn lin_sumnode_materialize(node: *mut u8) -> *mut u8 {
    if node.is_null() {
        return std::ptr::null_mut();
    }
    let desc = desc_of(node);
    if desc.is_null() {
        return std::ptr::null_mut();
    }
    // The materializer fn-ptr is the first 8 bytes of the descriptor.
    let matfn_slot = desc as *const SumMatFn;
    let matfn = *matfn_slot;
    matfn(node)
}

/// Read the descriptor pointer from a sum node's header (offset 8). Null = no heap fields anywhere.
#[inline]
unsafe fn desc_of(ptr: *const u8) -> *const u8 {
    *((ptr.add(8)) as *const *const u8)
}

/// Read the inline discriminant tag (offset 16).
#[inline]
unsafe fn tag_of(ptr: *const u8) -> u32 {
    *((ptr.add(SUMNODE_TAG_OFFSET)) as *const u32)
}

/// Allocate a `SumNode` of `size` total bytes (header + max-variant payload), zero-initialised, with
/// refcount 1, the byte `size` at offset 4, and the `SumDesc` pointer `desc` at offset 8 (NULL when
/// no variant has a heap field — every Stage-1 sum type). The inline tag (offset 16) is left 0;
/// codegen stores the variant's actual tag immediately after. `size` is computed by codegen as
/// `SUMNODE_HEADER + max_variant_payload`, padded to 8. Aborts on allocation failure. Always
/// 8-aligned. Scalar payload slots start zeroed (a valid, releasable state).
#[no_mangle]
pub extern "C" fn lin_sumnode_alloc(size: usize, desc: *const u8) -> *mut u8 {
    let size = size.max(SUMNODE_HEADER);
    unsafe {
        let layout = Layout::from_size_align_unchecked(size, 8);
        let ptr = alloc(layout);
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        // Zero the whole block so any unwritten padding/payload bytes are deterministic NULL.
        std::ptr::write_bytes(ptr, 0, size);
        let words = ptr as *mut u32;
        *words = 1; // refcount @ 0
        *words.add(1) = size as u32; // size @ 4
        *((ptr.add(8)) as *mut *const u8) = desc; // desc_ptr @ 8
        ptr
    }
}

/// Release one heap field stored at `payload` (an owned heap-payload pointer) according to its
/// `kind`. A NULL payload is a no-op. Used by the per-variant descriptor walk on drop. Mirrors
/// `sealed::release_field`, plus the recursive-child (`KIND_SUMNODE`) kind for Stage 2.
#[inline]
unsafe fn release_field(payload: *mut u8, kind: u32) {
    if payload.is_null() {
        return;
    }
    // HAZARD guard: `KIND_SUMNODE == sealed::KIND_MAP == 4`. A sumnode descriptor must NEVER
    // carry `KIND_SUMNODE_FIELD(5)` or `KIND_MAP` — those only appear in sealed descriptors.
    // If a sealed descriptor were ever walked here by mistake, kind=4 would call
    // `lin_sumnode_release_self` on a `*LinMap` → wild free. The disjoint-namespace invariant
    // prevents this at construction time; this assert fires in debug builds if it breaks.
    debug_assert!(
        kind == crate::sealed::KIND_STRING
            || kind == crate::sealed::KIND_ARRAY
            || kind == crate::sealed::KIND_SEALED
            || kind == KIND_SUMNODE,
        "sumnode::release_field: unknown kind {kind} — possible namespace violation"
    );
    match kind {
        crate::sealed::KIND_STRING => {
            crate::string::lin_string_release(payload as *mut crate::string::LinString)
        }
        crate::sealed::KIND_ARRAY => {
            crate::array::lin_array_release(payload as *mut crate::array::LinArray)
        }
        crate::sealed::KIND_SEALED => crate::sealed::lin_sealed_release_self(payload),
        KIND_SUMNODE => lin_sumnode_release_self(payload),
        _ => {}
    }
}

/// Walk the variant-indexed `SumDesc` for the node's current tag and release every heap field. Called
/// by `lin_sumnode_release` exactly once when the refcount reaches zero, BEFORE freeing the block.
/// No-op when the descriptor pointer is NULL (no variant has a heap field — every Stage-1 sum type).
#[inline]
unsafe fn release_heap_fields(ptr: *mut u8) {
    let desc = desc_of(ptr);
    if desc.is_null() {
        return;
    }
    // SumDesc layout (keep-packed-through-record-fields extension): an 8-byte materializer fn-ptr
    // PRECEDES the heap-field drop table, so `variant_count` and the VariantDesc blocks start at byte
    // offset 8 (`SUMDESC_TABLE_OFFSET`). The fn-ptr is read separately by `lin_sumnode_materialize`.
    let table = desc.add(SUMDESC_TABLE_OFFSET);
    let variant_count = *(table as *const u32);
    let tag = tag_of(ptr);
    if tag >= variant_count {
        return; // defensively: an out-of-range tag indexes nothing.
    }
    // Locate this variant's VariantDesc. The table is a u32 count followed by `variant_count`
    // VariantDescs; each VariantDesc is a variable-length `[u32 heap_count | {u32 off, u32 kind}*]`.
    // Walk forward to the target variant (variant blocks are not fixed-size, so we must skip).
    let mut cur = table.add(4);
    for _ in 0..tag {
        let hc = *(cur as *const u32) as usize;
        cur = cur.add(4 + hc * 8);
    }
    let heap_count = *(cur as *const u32) as usize;
    let entries = cur.add(4);
    for i in 0..heap_count {
        let ent = entries.add(i * 8);
        let offset = *(ent as *const u32) as usize;
        let kind = *((ent.add(4)) as *const u32);
        // The field slot holds an owned heap-payload pointer (8 bytes), at a node-relative offset.
        let slot = ptr.add(offset) as *mut *mut u8;
        release_field(*slot, kind);
    }
}

/// Immortal-seal a `*SumNode` and (recursively) every heap field of its current variant — the
/// sum-node arm of `frozen()`. Mirrors `release_heap_fields`' variant-indexed descriptor walk but
/// saturates the refcount instead of decrementing, and seals each heap field via the shared
/// `frozen::freeze_*` primitives. Needed because a kept-packed sum node can be boxed into a dynamic
/// (AnyVal/union) slot (`lin_box_sumnode`); when such a graph is `frozen()`, the node must be sealed
/// too or it stays mortal and is freeable while shared read-only across threads (cross-thread UAF).
/// Idempotent (inert once IMMORTAL_RC). Null-safe.
#[no_mangle]
pub unsafe extern "C" fn lin_sumnode_freeze(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    let rc = ptr as *mut u32;
    if *rc >= crate::string::IMMORTAL_RC {
        return; // already frozen — also breaks any accidental sharing/cycle
    }
    *rc = crate::string::IMMORTAL_RC;
    let desc = desc_of(ptr);
    if desc.is_null() {
        return; // scalar-only sum type: no heap fields to seal
    }
    let table = desc.add(SUMDESC_TABLE_OFFSET);
    let variant_count = *(table as *const u32);
    let tag = tag_of(ptr);
    if tag >= variant_count {
        return;
    }
    // Skip to this variant's VariantDesc (variable-length blocks, exactly as release_heap_fields).
    let mut cur = table.add(4);
    for _ in 0..tag {
        let hc = *(cur as *const u32) as usize;
        cur = cur.add(4 + hc * 8);
    }
    let heap_count = *(cur as *const u32) as usize;
    let entries = cur.add(4);
    for i in 0..heap_count {
        let ent = entries.add(i * 8);
        let offset = *(ent as *const u32) as usize;
        let kind = *((ent.add(4)) as *const u32);
        let payload = *(ptr.add(offset) as *const *mut u8);
        if payload.is_null() {
            continue;
        }
        match kind {
            crate::sealed::KIND_STRING => {
                crate::frozen::freeze_string(payload as *mut crate::string::LinString)
            }
            crate::sealed::KIND_ARRAY => {
                crate::frozen::freeze_array(payload as *mut crate::array::LinArray)
            }
            crate::sealed::KIND_SEALED => crate::frozen::freeze_sealed(payload),
            KIND_SUMNODE => lin_sumnode_freeze(payload), // recursive child
            _ => {}
        }
    }
}

/// Release a `SumNode`: decrement its refcount and, on reaching zero, release each heap field of the
/// node's variant (per the descriptor) THEN free the allocation. `size` is the same total byte size
/// passed to `lin_sumnode_alloc` (codegen knows it statically per sum type). Null- and
/// zero-refcount-safe, and inert on an IMMORTAL_RC (stack/immortal) node. For a Stage-1 scalar-only
/// sum type the descriptor is NULL and this is just a refcount decrement + free.
#[no_mangle]
pub unsafe extern "C" fn lin_sumnode_release(ptr: *mut u8, size: usize) {
    if ptr.is_null() {
        return;
    }
    let rc = ptr as *mut u32;
    if *rc == 0 {
        return;
    }
    // Mirror the sealed/string IMMORTAL_RC stack sentinel: an immortal node is inert to RC.
    if *rc >= crate::string::IMMORTAL_RC {
        return;
    }
    *rc -= 1;
    if *rc == 0 {
        release_heap_fields(ptr);
        let size = size.max(SUMNODE_HEADER);
        let layout = Layout::from_size_align_unchecked(size, 8);
        dealloc(ptr, layout);
    }
}

/// Release a `SumNode`, reading its byte `size` from the header (offset 4) instead of taking it as an
/// argument. Used where the caller does NOT have the size — a `KIND_SUMNODE` recursive child (Stage
/// 2) and the closure-capture / thread-transfer release walks (which carry only a one-byte kind).
/// Equivalent to `lin_sumnode_release(ptr, *(u32*)(ptr+4))`. Null/zero-rc-safe.
#[no_mangle]
pub unsafe extern "C" fn lin_sumnode_release_self(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    let size = *((ptr as *const u32).add(1)) as usize;
    lin_sumnode_release(ptr, size);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_sets_header_and_release_frees_once() {
        // A scalar-only sum node (desc NULL): alloc, check header, store tag+payload, release once.
        let size = SUMNODE_HEADER + 8; // header + one i32 payload slot (padded)
        let p = lin_sumnode_alloc(size, std::ptr::null());
        assert!(!p.is_null());
        unsafe {
            let words = p as *const u32;
            assert_eq!(*words, 1, "refcount initialised to 1");
            assert_eq!(*words.add(1) as usize, size, "size @ 4");
            // tag @ 16 starts zeroed; store variant tag 1 + payload r=42.
            *((p.add(SUMNODE_TAG_OFFSET)) as *mut u32) = 1;
            *((p.add(SUMNODE_HEADER)) as *mut i32) = 42;
            assert_eq!(tag_of(p), 1);
            assert_eq!(*((p.add(SUMNODE_HEADER)) as *const i32), 42);
            // Single owner → release frees (refcount 1 → 0). No double-free (scalar-only, NULL desc).
            lin_sumnode_release(p, size);
        }
    }

    #[test]
    fn retain_then_release_twice() {
        let size = SUMNODE_HEADER + 8;
        let p = lin_sumnode_alloc(size, std::ptr::null());
        unsafe {
            crate::memory::lin_rc_retain(p as *mut u32); // rc 1 -> 2
            lin_sumnode_release(p, size); // rc 2 -> 1, no free
            assert_eq!(*(p as *const u32), 1);
            lin_sumnode_release(p, size); // rc 1 -> 0, free
        }
    }

    #[test]
    fn release_self_reads_size_from_header() {
        let size = SUMNODE_HEADER + 16;
        let p = lin_sumnode_alloc(size, std::ptr::null());
        unsafe {
            // release_self must read size@4 and free without the caller passing it.
            lin_sumnode_release_self(p);
        }
    }

    #[test]
    fn recursive_child_drop_walks_and_frees_subtree() {
        // unboxed-sumtype Stage 2: a parent node holds a recursive child at a KIND_SUMNODE slot. The
        // parent's drop walk must recurse into the child (releasing it) before freeing the parent.
        // Build a SumDesc with ONE variant (tag 0) whose single heap field is a KIND_SUMNODE child at
        // payload offset 24 (the first payload slot). Lay it out exactly as codegen emits (keep-packed
        // -through-record-fields extension): an 8-byte MATERIALIZER fn-ptr PRECEDES the drop table, so
        // the table (variant_count + VariantDesc) starts at `SUMDESC_TABLE_OFFSET` (8). This test
        // exercises only the drop walk, so the fn-ptr is null (never invoked here).
        //   SumDesc = [ u64 matfn_ptr=0 | u32 variant_count=1 | VariantDesc ]
        //   VariantDesc = [ u32 heap_count=1 | { u32 offset=24, u32 kind=KIND_SUMNODE } ]
        #[repr(C)]
        struct TestDesc {
            matfn: *const u8,
            table: [u32; 4],
        }
        let desc = TestDesc { matfn: std::ptr::null(), table: [1, 1, 24, KIND_SUMNODE] };
        let desc_ptr = &desc as *const TestDesc as *const u8;
        let size = SUMNODE_HEADER + 8; // header + one pointer slot
        unsafe {
            let child = lin_sumnode_alloc(size, std::ptr::null()); // leaf: NULL desc, scalar-only
            let parent = lin_sumnode_alloc(size, desc_ptr);
            // parent.tag = 0 (the only variant); store the OWNED child pointer at payload offset 24.
            *((parent.add(SUMNODE_TAG_OFFSET)) as *mut u32) = 0;
            *((parent.add(SUMNODE_HEADER)) as *mut *mut u8) = child;
            // The child rc stays 1 (the parent owns it). Releasing the parent (rc 1 -> 0) must run the
            // drop walk: read tag 0 -> variant 0 -> the KIND_SUMNODE field at offset 24 ->
            // lin_sumnode_release_self(child) -> child rc 1 -> 0 -> free, THEN free the parent.
            assert_eq!(*(child as *const u32), 1, "child rc starts at 1 (parent-owned)");
            lin_sumnode_release(parent, size);
            // If the walk did NOT recurse, the child would leak (caught by ASan/Miri). If it
            // double-freed, this test would abort under the allocator. Reaching here = freed once.
        }
    }

    #[test]
    fn null_and_immortal_are_inert() {
        unsafe {
            lin_sumnode_release(std::ptr::null_mut(), 32); // null-safe
            let size = SUMNODE_HEADER + 8;
            let p = lin_sumnode_alloc(size, std::ptr::null());
            *(p as *mut u32) = crate::string::IMMORTAL_RC; // immortal sentinel
            lin_sumnode_release(p, size); // inert: must NOT free (would be UB to free below)
            assert_eq!(*(p as *const u32), crate::string::IMMORTAL_RC, "immortal rc untouched");
            // Reset to mortal and free to avoid leaking the test allocation.
            *(p as *mut u32) = 1;
            lin_sumnode_release(p, size);
        }
    }
}
