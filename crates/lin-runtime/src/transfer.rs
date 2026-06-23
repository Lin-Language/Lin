//! Transfer-by-deep-copy for crossing a thread boundary (ADR-027, Option C).
//!
//! When a value or a thunk's captured environment crosses into another OS thread, Lin
//! copies it so each thread owns a private, disjoint object graph — refcounts stay
//! non-atomic because nothing is shared. The set of values that can cross is exactly the
//! *transferable* types (the checker forbids `Function`/`Iterator`/cyclic graphs at a
//! boundary), so a deep copy is total and bounded.
//!
//! Two entry points:
//!   * `lin_transfer_clone(TaggedVal*)` — deep-copies a transferable value graph (scalars,
//!     strings, arrays, objects, recursively). Used for the closure's captured `val`s and
//!     (defensively) for results. Immortal/interned strings are shared, not copied (never
//!     mutated or freed). `Shared`/`Frozen` boxes (Phases 6-7) will be shared by
//!     atomic-refcount bump, not copied through — handled when those types land.
//!   * `transfer_clone_env(env_ptr, desc)` — deep-copies a closure's env allocation using the
//!     codegen-emitted capture descriptor (passed in from the closure's offset-40 slot, ADR-041)
//!     recording each slot's kind.

use crate::tagged::{TaggedVal, TAG_STR, TAG_ARRAY};
use crate::string::{LinString, lin_string_alloc, IMMORTAL_RC};
use crate::array::{LinArray, lin_array_alloc};

/// Deep-copy a `LinString`. Immortal (interned literal) strings are shared as-is — they are
/// never mutated or freed, so concurrent reads of their bytes/refcount are race-free.
unsafe fn clone_string(s: *const LinString) -> *mut LinString {
    if s.is_null() {
        return std::ptr::null_mut();
    }
    if (*s).refcount >= IMMORTAL_RC {
        return s as *mut LinString;
    }
    let len = (*s).len;
    let fresh = lin_string_alloc(len);
    if len > 0 {
        std::ptr::copy_nonoverlapping((*s).data.as_ptr(), (*fresh).data.as_mut_ptr(), len as usize);
    }
    fresh
}

/// Deep-copy a `LinMap` (`{ String: T }` index-signature container) for cross-thread transfer.
/// Share-nothing: allocate a fresh map and re-insert a DEEP CLONE of every live key/value pair, so
/// the worker owns a private, disjoint graph. Keys are cloned strings; each value's heap payload is
/// recursively transferred via `transfer_payload` (scalars copy verbatim; strings/arrays/objects/
/// nested maps deep-copy). The fresh map has refcount 1; `lin_map_set` takes its own +1 on each
/// inserted key + value payload, so we DROP our construction references afterwards (release the
/// cloned key; release the cloned value payload via a temporary box) to leave exactly the map's own
/// +1 per entry. Frozen/immortal maps are shared read-only (zero-copy), mirroring clone_array.
/// Deep-clone ONE tagged value for cross-thread transfer, returning a fully independent
/// `(tag, payload)` that shares NO pointer with `src` (the transfer invariant, ADR-028).
/// TAG_RECORD transfers as a fresh deep-copied sealed struct (TAG_RECORD) via clone_sealed (no
/// intermediate LinMap). TAG_SUMNODE materialises to a fresh LinMap and transfers as TAG_MAP.
/// Every other tag keeps its tag with a deep-cloned payload via `transfer_payload` (scalars
/// verbatim; string/array/map deep-copy; Shared/TarEntry retained). Used by `clone_map`,
/// `clone_array`, and the CAP_OBJECT env clone so they share one correct implementation.
unsafe fn transfer_clone_value(src: &TaggedVal) -> TaggedVal {
    use crate::tagged::{TAG_MAP, TAG_RECORD, TAG_SUMNODE};
    if src.tag == TAG_RECORD {
        // Deep-copy the sealed struct directly via clone_sealed (descriptor-walk, no intermediate LinMap).
        // The clone is a fresh TAG_RECORD owned by the worker thread — no aliasing with the source.
        let cloned = clone_sealed(src.payload as *const u8);
        TaggedVal { tag: TAG_RECORD, _pad: [0; 7], payload: cloned as u64 }
    } else if src.tag == TAG_SUMNODE {
        // KEEP-PACKED-THROUGH-RECORD-FIELDS thread-transfer: a kept-packed `*SumNode` value MUST
        // NOT cross the thread boundary by pointer (it would alias → cross-thread double-free).
        // Materialise it to a fresh +1 LinMap, deep-clone that, and transfer as a uniform TAG_MAP.
        let map = crate::sumnode::lin_sumnode_materialize(src.payload as *mut u8);
        let cloned = clone_map(map as *const crate::map::LinMap);
        if !map.is_null() { crate::map::lin_map_release(map as *mut crate::map::LinMap); }
        TaggedVal { tag: TAG_MAP, _pad: [0; 7], payload: cloned as u64 }
    } else {
        let payload = transfer_payload(src.tag, src.payload);
        TaggedVal { tag: src.tag, _pad: [0; 7], payload }
    }
}

unsafe fn clone_map(src: *const crate::map::LinMap) -> *mut crate::map::LinMap {
    if src.is_null() {
        return std::ptr::null_mut();
    }
    if (*src).refcount >= IMMORTAL_RC {
        return src as *mut crate::map::LinMap;
    }
    let dst = crate::map::lin_map_alloc((*src).len, (*src).key_kind);
    let len = (*src).len as usize;
    // Iterate the source's INSERTION-ORDER key list (not raw hash slots) so the clone preserves
    // declaration order (option A: insertion order everywhere). Inserting in this sequence rebuilds
    // the destination's own order array identically.
    if len > 0 && !(*src).order.is_null() {
        let is_int = (*src).key_kind == crate::map::KEY_KIND_INT;
        for i in 0..len {
            let key_bits = *(*src).order.add(i);
            // Fetch the value slot for this ordered key.
            let vsrc: *const TaggedVal = if is_int {
                crate::map::lin_map_get_int(src, key_bits as i64)
            } else {
                crate::map::lin_map_get(src, key_bits as *const crate::string::LinString)
            };
            if vsrc.is_null() {
                continue; // key vanished — defensive, should not happen
            }
            let v = transfer_clone_value(&*vsrc);
            // Insert into the destination map. For String maps, deep-clone the key; for Int maps
            // the key is a scalar (no RC).
            if is_int {
                crate::map::lin_map_set_int(dst, key_bits as i64, &v);
                crate::tagged::release_tagged_payload_pub(&v);
            } else {
                // Deep-clone the string key.
                let key = clone_string(key_bits as *mut crate::string::LinString);
                // lin_map_set retains the key (first insert) and retains the value payload.
                crate::map::lin_map_set(dst, key, &v);
                crate::string::lin_string_release(key);
                crate::tagged::release_tagged_payload_pub(&v);
            }
        }
    }
    dst
}

/// Deep-copy a sealed record (sealed-records Stages 1–2). Share-nothing: allocate a fresh struct of
/// the same byte `size` (read from offset 4) carrying the same field descriptor (offset 8), copy the
/// field bytes verbatim (this gets every SCALAR field right in one move), then — for each HEAP field
/// Deep-copy a `*SumNode` for cross-thread transfer. Allocates a fresh SumNode of the same total
/// size (read from header offset 4) and descriptor, byte-copies the header+payload, then recursively
/// deep-clones any heap fields listed in the variant's SumDesc entry. The fresh node has refcount 1.
unsafe fn clone_sumnode(src: *mut u8) -> *mut u8 {
    if src.is_null() {
        return std::ptr::null_mut();
    }
    // Header: [u32 rc | u32 size | u64 desc_ptr | u32 tag | u32 pad | payload...]
    let size = *((src as *const u32).add(1)) as usize;
    let desc = *((src.add(8)) as *const *const u8); // SumDesc ptr @ 8
    let fresh = crate::sumnode::lin_sumnode_alloc(size, desc);
    // Byte-copy the entire node (header + payload). The tag, scalar fields, and heap pointers are
    // all set; rc on fresh is 1 (correct), size and desc were set by alloc. Heap slots temporarily
    // ALIAS the source — fixed below via the variant's SumDesc heap-field table.
    if size > crate::sumnode::SUMNODE_HEADER {
        std::ptr::copy_nonoverlapping(
            src.add(crate::sumnode::SUMNODE_HEADER),
            fresh.add(crate::sumnode::SUMNODE_HEADER),
            size - crate::sumnode::SUMNODE_HEADER,
        );
    }
    // Deep-clone heap fields using the SumDesc variant table. The desc layout:
    //   [u64 matfn_ptr | u32 variant_count | VariantDesc * variant_count]
    // where VariantDesc = [u32 heap_count | {u32 offset, u32 kind} * heap_count].
    if !desc.is_null() {
        use crate::sumnode::SUMDESC_TABLE_OFFSET;
        let table = desc.add(SUMDESC_TABLE_OFFSET);
        let variant_count = *(table as *const u32) as usize;
        let tag = *((src.add(crate::sumnode::SUMNODE_TAG_OFFSET)) as *const u32) as usize;
        if tag < variant_count {
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
                let slot = fresh.add(offset) as *mut *mut u8;
                let payload = *slot;
                if payload.is_null() { continue; }
                // SumNode variant descriptor KIND constants mirror sealed KIND_* but KIND_SUMNODE(=4)
                // maps to "recursive SumNode child" (not a LinMap). Use numeric match to avoid the
                // namespace ambiguity (sealed::KIND_MAP == sumnode::KIND_SUMNODE == 4).
                let cloned: *mut u8 = match kind {
                    crate::sealed::KIND_STRING => clone_string(payload as *const LinString) as *mut u8,
                    crate::sealed::KIND_ARRAY => clone_array(payload as *const LinArray) as *mut u8,
                    crate::sealed::KIND_SEALED => clone_sealed(payload as *const u8),
                    4 /* KIND_SUMNODE — recursive child */ => clone_sumnode(payload),
                    _ => payload,
                };
                *slot = cloned;
            }
        }
    }
    fresh
}

/// Deep-copy a sealed struct (`lin_sealed_alloc` allocation) for cross-thread transfer. Allocates a
/// fresh struct of the same size and descriptor, byte-copies the payload, then for each heap field
/// listed in the descriptor — OVERWRITE its pointer slot with a DEEP CLONE of the payload (string →
/// clone_string, array → clone_array, nested sealed → clone_sealed recursively). The verbatim copy
/// would otherwise leave a heap slot ALIASING the source's payload (a cross-thread share, which the
/// transfer model forbids); replacing each heap slot with a fresh +1 clone makes the copy disjoint.
/// The fresh struct has refcount 1 (set by `lin_sealed_alloc`); each cloned heap field is +1 owned.
unsafe fn clone_sealed(src: *const u8) -> *mut u8 {
    if src.is_null() {
        return std::ptr::null_mut();
    }
    let size = *((src as *const u32).add(1)) as usize;
    let desc = *((src.add(8)) as *const *const u8);   // heap descriptor @ 8
    let named_desc = *((src.add(16)) as *const *const u8); // named descriptor @ 16 (SEALED_HEADER=24)
    let fresh = crate::sealed::lin_sealed_alloc(size, desc, named_desc);
    // Copy the field payload (everything past the 24-byte header). Scalars are now correct; heap
    // field slots currently ALIAS the source — fixed below. The header's rc/size/desc on `fresh`
    // are already correct from the alloc.
    if size > crate::sealed::SEALED_HEADER {
        std::ptr::copy_nonoverlapping(
            src.add(crate::sealed::SEALED_HEADER),
            fresh.add(crate::sealed::SEALED_HEADER),
            size - crate::sealed::SEALED_HEADER,
        );
    }
    // Deep-clone each heap field, replacing the aliased pointer with a private +1 copy.
    if !desc.is_null() {
        let count = *(desc as *const u32);
        let entries = desc.add(4);
        for i in 0..count as usize {
            let ent = entries.add(i * 8);
            let offset = *(ent as *const u32) as usize;
            let kind = *((ent.add(4)) as *const u32);
            let slot = fresh.add(offset) as *mut *mut u8;
            let payload = *slot;
            if payload.is_null() {
                continue;
            }
            let cloned: *mut u8 = match kind {
                crate::sealed::KIND_STRING => clone_string(payload as *const LinString) as *mut u8,
                crate::sealed::KIND_ARRAY => clone_array(payload as *const LinArray) as *mut u8,
                crate::sealed::KIND_SEALED => clone_sealed(payload as *const u8),
                crate::sealed::KIND_MAP => clone_map(payload as *const crate::map::LinMap) as *mut u8,
                crate::sealed::KIND_SUMNODE_FIELD => clone_sumnode(payload),
                _ => payload,
            };
            *slot = cloned;
        }
    }
    fresh
}

/// Deep-copy a SEALED-RECORD packed array (`elem_tag == 0xFE`, ADR-063 Stage 3b) for cross-thread
/// transfer. Share-nothing: allocate a fresh `0xFE` array with the SAME `elem_stride`, `elem_desc`
/// (heap-only RC descriptor) and `elem_named_desc` (the boxed-reader materialise descriptor),
/// byte-copy the contiguous packed element buffer (every SCALAR field correct in one move), then —
/// for each element — DEEP-CLONE each heap field listed in the descriptor, overwriting the aliased
/// pointer with a private +1 copy. This mirrors `clone_sealed` (a standalone struct) but over a
/// header-LESS element payload, so field offsets are rebased by `-SEALED_HEADER` exactly like
/// `release_sealed_array_elems` / `release_payload_fields`. A scalar-only record (NULL `elem_desc`)
/// is a pure buffer copy (no inner heap to clone). The fresh array has refcount 1; each cloned heap
/// field is +1 owned by its element slot — so the worker's later `lin_array_release` (which walks
/// the same descriptor via `release_sealed_array_elems`) frees them exactly once, on the worker.
unsafe fn clone_sealed_array(src: *const LinArray) -> *mut LinArray {
    let len = (*src).len;
    let stride = (*src).elem_stride;
    let desc = (*src).elem_desc;
    let named = (*src).elem_named_desc;
    // Allocate a fresh 0xFE array of the same stride/desc, capacity >= len.
    let dst = crate::array::lin_sealed_array_alloc(len.max(4), stride, desc, named);
    (*dst).len = len;
    // Byte-copy the packed element buffer verbatim. Scalars are now correct; heap-field pointer
    // slots currently ALIAS the source's payloads — fixed per element below.
    if len > 0 && stride > 0 {
        std::ptr::copy_nonoverlapping(
            (*src).data as *const u8,
            (*dst).data as *mut u8,
            (len * stride) as usize,
        );
    }
    // Deep-clone each element's heap fields, replacing each aliased pointer with a private +1 copy.
    // NULL desc (scalar-only) -> nothing to do.
    if !desc.is_null() {
        let count = *(desc as *const u32);
        let entries = desc.add(4);
        for ei in 0..len as usize {
            let payload = ((*dst).data as *mut u8).add(ei * stride as usize);
            for fi in 0..count as usize {
                let ent = entries.add(fi * 8);
                let offset = *(ent as *const u32) as usize;
                let kind = *((ent.add(4)) as *const u32);
                // Element offsets are payload-relative; the descriptor stores struct-relative ones.
                let slot = payload.add(offset - crate::sealed::SEALED_HEADER) as *mut *mut u8;
                let p = *slot;
                if p.is_null() {
                    continue;
                }
                let cloned: *mut u8 = match kind {
                    crate::sealed::KIND_STRING => clone_string(p as *const LinString) as *mut u8,
                    crate::sealed::KIND_ARRAY => clone_array(p as *const LinArray) as *mut u8,
                    crate::sealed::KIND_SEALED => clone_sealed(p),
                    crate::sealed::KIND_MAP => clone_map(p as *const crate::map::LinMap) as *mut u8,
                    _ => p,
                };
                *slot = cloned;
            }
        }
    }
    dst
}

/// Deep-copy a pointer-backed sealed-record array (`elem_tag == 0xFD`, Stage 1) for cross-thread
/// transfer. Share-nothing: allocate a fresh 0xFD array, then for each slot deep-clone the sealed
/// struct pointer via `clone_sealed`. The clone starts at rc=1 (from `clone_sealed`/alloc); we push
/// it to the new array with `lin_sealed_ptr_array_push` (which retains → rc=2), then release our
/// construction reference (rc=1). Worker's later `lin_array_release` will release each slot once.
unsafe fn clone_sealed_ptr_array(src: *const LinArray) -> *mut LinArray {
    let len = (*src).len;
    let named = (*src).elem_named_desc;
    let dst = crate::array::lin_sealed_ptr_array_alloc(len.max(4), named);
    let slots = (*src).data as *const *mut u8;
    for i in 0..len as usize {
        let sptr = *slots.add(i);
        if sptr.is_null() {
            continue; // null slots don't occur in valid 0xFD arrays; skip to preserve length
        }
        let cloned = clone_sealed(sptr);             // rc=1 from alloc
        crate::array::lin_sealed_ptr_array_push(dst, cloned); // retains → rc=2
        crate::sealed::lin_sealed_release_self(cloned);        // release our ref → rc=1
    }
    dst
}

/// Deep-copy a `LinArray`, flat, tagged, or sealed-packed. Flat scalar arrays copy their raw buffer;
/// tagged arrays recursively transfer each element; sealed-packed (`0xFE`) arrays deep-copy the
/// packed buffer + each element's heap fields (`clone_sealed_array`); pointer-backed sealed arrays
/// (`0xFD`) deep-copy each struct pointer slot (`clone_sealed_ptr_array`).
pub(crate) unsafe fn clone_array(src: *const LinArray) -> *mut LinArray {
    if src.is_null() {
        return std::ptr::null_mut();
    }
    // Frozen (immortal) arrays are immutable and shared read-only across threads — share by
    // reference (zero-copy), never deep-copy through (Frozen<T>, ADR-030). Safe because their
    // contents and refcount are never written.
    if (*src).refcount >= IMMORTAL_RC {
        return src as *mut LinArray;
    }
    let len = (*src).len;
    let elem_tag = (*src).elem_tag;
    if elem_tag == crate::array::SEALED_ARRAY_TAG {
        // Sealed-record packed array: deep-copy the packed buffer + each element's heap fields,
        // preserving stride/desc/named_desc. `lin_array_clone_flat` (below) would MIS-SIZE the
        // buffer (it assumes a flat scalar width, not `elem_stride`) and drop the descriptors,
        // corrupting a packed record-array crossing a thread boundary (ADR-063 transfer bug).
        return clone_sealed_array(src);
    }
    if elem_tag == crate::array::SEALED_PTR_ARRAY_TAG {
        // Pointer-backed sealed-record array: each slot is an 8-byte struct pointer.
        // `lin_array_clone_flat` would MIS-SIZE (assumes flat scalar, not 8-byte pointers) and
        // drop the named_desc, corrupting the clone. Deep-copy each struct pointer individually.
        return clone_sealed_ptr_array(src);
    }
    if elem_tag != 0xFF {
        // Flat scalar array: copy the raw element buffer verbatim (no pointers inside).
        return crate::array::lin_array_clone_flat(src);
    }
    // Tagged array: allocate and transfer each element.
    let dst = lin_array_alloc(len.max(4));
    for i in 0..len as usize {
        let se = (*src).data.add(i);
        let de = (*dst).data.add(i);
        let src_tv = TaggedVal { tag: (*se).tag, _pad: [0; 7], payload: (*se).payload };
        let cloned = transfer_clone_value(&src_tv);
        (*de).tag = cloned.tag;
        (*de).payload = cloned.payload;
    }
    (*dst).len = len;
    dst
}


/// Transfer one tagged payload (the 8-byte field) by kind: scalars copy verbatim; heap
/// pointers are deep-copied.
/// NOTE: TAG_RECORD and TAG_SUMNODE are NOT handled here — `transfer_clone_value` handles them
/// directly (TAG_RECORD via clone_sealed → fresh TAG_RECORD; TAG_SUMNODE via materialize → TAG_MAP).
unsafe fn transfer_payload(tag: u8, payload: u64) -> u64 {
    use crate::tagged::{TAG_SHARED, TAG_MAP, TAG_TAR_ENTRY, TAG_BIGNUM, TAG_DECIMAL};
    match tag {
        TAG_STR => clone_string(payload as *const LinString) as u64,
        TAG_ARRAY => clone_array(payload as *const LinArray) as u64,
        TAG_MAP => clone_map(payload as *const crate::map::LinMap) as u64,
        TAG_SHARED => {
            // Nesting/boundary rule (ADR-028 §2.3.1): a Shared box embedded in a transferred
            // value is NOT deep-copied through — bump its atomic refcount and SHARE the box.
            crate::shared::lin_shared_retain_box(payload as *const u8);
            payload
        }
        // BigInt / Decimal are opaque, IMMUTABLE, ATOMICALLY-refcounted heap handles (same shape as
        // Shared). They surface to the checker as `AnyVal` (`export type BigInt = AnyVal`), so the
        // async capture/return gate cannot reject them by type — they CAN reach this boundary inside
        // a transferred graph. Previously they fell through to `_ => payload`, aliasing the box across
        // the thread with NO refcount bump → freed when one side released while the other still held
        // it (cross-thread double-free). Retain the box (atomic +1) and share it, exactly like
        // TAG_SHARED: immutable + atomic RC makes sharing sound; the worker's release goes through the
        // matching `lin_bignum_release_box` / `lin_decimal_release_box` arm.
        TAG_BIGNUM => {
            crate::bignum::lin_bignum_retain_box(payload as *const u8);
            payload
        }
        TAG_DECIMAL => {
            crate::decimal::lin_decimal_retain_box(payload as *const u8);
            payload
        }
        // Scalars: copy verbatim. (TAG_FUNCTION is not transferable data — the checker
        // prevents it appearing here; pass through as a last resort.)
        // TarEntry is NOT transferable (holds a shared cursor into a live stream). If one
        // somehow reaches this path despite the checker gate, retain and copy to avoid a dangling
        // pointer in the worker (the worker will then hold an aliased entry, which is incorrect
        // but at least not a UAF — the checker is the primary guard). Ideally this is unreachable.
        TAG_TAR_ENTRY => {
            crate::stream::lin_tar_entry_retain_box(payload as *const u8);
            payload
        }
        _ => payload,
    }
}

/// Deep-copy a transferable value graph rooted at a boxed `TaggedVal*`. Returns a fresh,
/// independently-owned box (or null for the null value). The caller owns the result.
#[no_mangle]
pub unsafe extern "C" fn lin_transfer_clone(p: *const u8) -> *mut u8 {
    if p.is_null() {
        return std::ptr::null_mut();
    }
    let src = &*(p as *const TaggedVal);
    // Deep-clone the value graph. transfer_clone_value handles the record/sum-node → LinMap
    // materialisation (Phase 3 dynamic-object unification) and deep-copies every other tag.
    let cloned = transfer_clone_value(src);
    crate::tagged::alloc_tagged(cloned.tag, cloned.payload)
}

// -------------------------------------------------------------------------
// Closure environment transfer
// -------------------------------------------------------------------------

// Capture descriptor kind codes (one byte per captured env slot, env slot `i` at byte offset
// `8 + i*8`). These mirror `lin_ir::ir::CaptureRelease::code()` — the SAME descriptor drives
// both closure-release and this thread-transfer path. The descriptor pointer lives in the
// CLOSURE at offset 40 (ADR-041); the async caller passes it in explicitly.
pub const CAP_NONE: u8 = 0; // scalar (copy verbatim) or a borrowed var-cell pointer
pub const CAP_STR: u8 = 1; // *mut LinString
pub const CAP_ARRAY: u8 = 2; // *mut LinArray
pub const CAP_OBJECT: u8 = 3; // *mut LinObject
pub const CAP_CLOSURE: u8 = 4; // *mut LinClosure — NOT deep-copyable across a thread boundary
pub const CAP_TAGGED: u8 = 5; // *mut TaggedVal (boxed Json/union) — deep-copy via lin_transfer_clone
/// MOVED resource capture (streams brief §9): a `Stream` crosses by MOVE, not copy. The env-clone
/// path hands the pointer off VERBATIM (no clone, no retain) — the source env will not be released
/// for this slot (its scope release is suppressed by the IR), and the worker's `release_env_copy`
/// releases it via `lin_tagged_release` (TAG_STREAM finalizer). Mirrors `ir::CaptureRelease::Move`.
pub const CAP_MOVE: u8 = 6;
/// SEALED scalar record (sealed-records Stage 1): a packed `[u32 rc | u32 size | scalars]` struct
/// (NOT a `LinObject`). Deep-copied across a thread boundary by a flat byte copy of `size` bytes
/// (all fields are scalars, no inner heap to clone) and released via `lin_sealed_release_self`
/// (reads the size from offset 4). Mirrors `ir::CaptureRelease::Sealed`.
pub const CAP_SEALED: u8 = 7;

/// Deep-copy a captured closure value (`CAP_CLOSURE`) for cross-thread transfer. A closure is a
/// 48-byte struct (`lin_closure_release` documents the layout): rc @0, fn_ptr @8, env_ptr @16,
/// env_size @24, default_descriptor @32, capture_descriptor @40. We allocate a fresh struct with
/// refcount 1 (owned solely by the worker's env copy), copy the two code/descriptor pointers
/// verbatim (they are static read-only globals / LLVM function pointers), and RECURSIVELY deep-copy
/// the captured closure's own env via `transfer_clone_env` using ITS capture descriptor (offset 40)
/// — so a function value that itself captures heap data still produces a fully private graph on the
/// worker. A null source yields a null copy.
///
/// SAFETY: the caller must have established (via `closure_is_transferable`) that this closure's env
/// contains no non-transferable capture; otherwise the recursive clone would alias an
/// un-copyable resource.
unsafe fn clone_closure(src: *const u8) -> *mut u8 {
    if src.is_null() {
        return std::ptr::null_mut();
    }
    const CLOSURE_SIZE: usize = 48;
    let fresh = crate::memory::lin_alloc(CLOSURE_SIZE);
    // Copy the whole struct verbatim first (gets fn_ptr, env_size, both descriptors right), then
    // fix up the refcount and env pointer.
    std::ptr::copy_nonoverlapping(src, fresh, CLOSURE_SIZE);
    *(fresh as *mut u32) = 1; // sole owner = the worker's env copy
    let src_env = *(src.add(16) as *const *const u8);
    let inner_desc = *(src.add(40) as *const *const u8);
    let env_copy = transfer_clone_env(src_env, inner_desc);
    *(fresh.add(16) as *mut *mut u8) = env_copy;
    fresh
}

/// True if a captured closure value can be safely deep-copied for cross-thread transfer: its env
/// (offset 16) is either null, or every capture in it is itself transferable (recursing into nested
/// `CAP_CLOSURE` captures). A captured `var`-cell (lowered as `CAP_NONE`, a borrowed pointer) is
/// already banned from async thunks by the checker (ADR-022), so the recursion only ever sees
/// owning, deep-copyable captures.
unsafe fn closure_is_transferable(closure: *const u8) -> bool {
    if closure.is_null() {
        return true;
    }
    let env_ptr = *(closure.add(16) as *const *const u8);
    let desc = *(closure.add(40) as *const *const u8);
    env_is_transferable(env_ptr, desc)
}

/// Deep-copy a closure env allocation given its capture descriptor `desc` (a static read-only
/// `{u32 count, u8 kinds[]}` global from the closure's offset-40 slot). `env_ptr` layout:
/// `{ u64 size @0, cap0 @8, cap1 @16, ... }`. Returns a fresh env whose heap captures are
/// private copies, or null if `env_ptr`/`desc` is null. The new env's offset-0 word is its
/// size (the descriptor is NOT stored in the env — it stays on the closure).
pub unsafe fn transfer_clone_env(env_ptr: *const u8, desc: *const u8) -> *mut u8 {
    if env_ptr.is_null() || desc.is_null() {
        return std::ptr::null_mut();
    }
    let count = *(desc as *const u32) as usize;
    let kinds = desc.add(std::mem::size_of::<u32>());
    let env_size = 8 + count * 8;
    let new_env = crate::memory::lin_alloc(env_size);
    *(new_env as *mut u64) = env_size as u64; // size header at offset 0
    for i in 0..count {
        let off = 8 + i * 8;
        let src_word = *(env_ptr.add(off) as *const u64);
        let new_word = match *kinds.add(i) {
            CAP_NONE => src_word,
            // A captured FUNCTION value: recursively deep-copy the whole closure (its own env
            // included) so the worker owns a private graph. (Previously copied verbatim, which
            // would have shared the parent's closure across threads — instead the spawn path ran
            // such thunks inline, defeating `timeout`.)
            CAP_CLOSURE => clone_closure(src_word as *const u8) as u64,
            CAP_STR => clone_string(src_word as *const LinString) as u64,
            CAP_ARRAY => clone_array(src_word as *const LinArray) as u64,
            // CAP_OBJECT: env slot holds a raw *mut LinMap (Phase 3: open objects are LinMap-backed).
            // Deep-clone the map so the worker owns a private copy.
            CAP_OBJECT => {
                let src_map = src_word as *const crate::map::LinMap;
                if src_map.is_null() {
                    0u64
                } else {
                    clone_map(src_map) as u64
                }
            },
            CAP_TAGGED => lin_transfer_clone(src_word as *const u8) as u64,
            CAP_SEALED => clone_sealed(src_word as *const u8) as u64,
            // CAP_MOVE: hand the resource pointer off VERBATIM — no clone, no retain. The source
            // env will not release this slot (the IR suppresses its scope release); the worker's
            // `release_env_copy` releases it, so the fd closes exactly once, on the worker.
            CAP_MOVE => src_word,
            _ => src_word,
        };
        *(new_env.add(off) as *mut u64) = new_word;
    }
    new_env
}

/// Release a deep-copied env produced by `transfer_clone_env`: drop the owned reference to each
/// heap capture (the copies were created with refcount 1, owned by no Lin binding — the worker
/// holds the sole reference), then free the env allocation. `desc` is the capture descriptor;
/// `env_size` is `8 + count*8`.
pub unsafe fn release_env_copy(env_ptr: *mut u8, desc: *const u8, env_size: u64) {
    if env_ptr.is_null() {
        return;
    }
    if !desc.is_null() {
        let count = *(desc as *const u32) as usize;
        let kinds = desc.add(std::mem::size_of::<u32>());
        for i in 0..count {
            let off = 8 + i * 8;
            let word = *(env_ptr.add(off) as *const u64);
            match *kinds.add(i) {
                CAP_STR => crate::string::lin_string_release(word as *mut LinString),
                CAP_ARRAY => crate::array::lin_array_release(word as *mut LinArray),
                // CAP_OBJECT: after Phase 3, open objects are LinMap-backed.
                CAP_OBJECT => crate::map::lin_map_release(word as *mut crate::map::LinMap),
                CAP_CLOSURE => crate::memory::lin_closure_release(word as *mut u8),
                CAP_TAGGED => crate::tagged::lin_tagged_release(word as *mut u8),
                CAP_SEALED => crate::sealed::lin_sealed_release_self(word as *mut u8),
                // CAP_MOVE: the worker OWNS the moved resource — release it here (TAG_STREAM
                // finalizer closes the fd exactly once, on the worker thread).
                CAP_MOVE => crate::tagged::lin_tagged_release(word as *mut u8),
                _ => {} // CAP_NONE: no owned heap payload to release
            }
        }
    }
    let layout = std::alloc::Layout::from_size_align_unchecked(env_size as usize, 8);
    std::alloc::dealloc(env_ptr, layout);
}

/// True if a closure with env `env_ptr` and capture descriptor `desc` can be safely deep-copied
/// for transfer: a null env (no captures) is trivially transferable; otherwise `desc` must be
/// present and every capture must be deep-copyable. A captured FUNCTION value (`CAP_CLOSURE`) is
/// transferable as long as it is itself recursively transferable (its own env is deep-copyable) —
/// `clone_closure` handles the deep copy. When false, the spawn path must run the thunk inline.
pub unsafe fn env_is_transferable(env_ptr: *const u8, desc: *const u8) -> bool {
    if env_ptr.is_null() {
        return true;
    }
    if desc.is_null() {
        return false;
    }
    let count = *(desc as *const u32) as usize;
    let kinds = desc.add(std::mem::size_of::<u32>());
    for i in 0..count {
        if *kinds.add(i) == CAP_CLOSURE {
            // Recurse into the captured closure: transferable iff its own env is.
            let inner = *(env_ptr.add(8 + i * 8) as *const *const u8);
            if !closure_is_transferable(inner) {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tagged::{alloc_tagged, TAG_INT32};

    // ADR-063 KIND_MAP: `clone_map` deep-copies a `LinMap` share-nothing for cross-thread transfer.
    // Asserts: (1) the clone is a DISTINCT map with the same key/value content, (2) a STRING value is
    // a PRIVATE copy (distinct pointer, share-nothing), (3) RC is balanced — dropping the source does
    // NOT free the clone's string, and dropping the clone frees only its own. A by-pointer share
    // would be a cross-thread UAF (caught under ASan). Also exercises a SCALAR value (verbatim copy).
    #[test]
    fn clone_map_is_private_and_rc_balanced() {
        use crate::map::{lin_map_alloc, lin_map_set, lin_map_get, lin_map_length, lin_map_release};
        use crate::string::{lin_string_from_bytes, lin_string_release, lin_string_eq, LinString};
        use crate::tagged::{TaggedVal, TAG_STR, TAG_INT64};
        unsafe {
            let src = lin_map_alloc(4, 0);
            // key "name" -> string "hello" (a fresh, non-interned +1)
            let kname = lin_string_from_bytes(b"name".as_ptr(), 4);
            let sval = lin_string_from_bytes(b"hello".as_ptr(), 5);
            assert_eq!((*sval).refcount, 1);
            let tv_s = TaggedVal { tag: TAG_STR, _pad: [0; 7], payload: sval as u64 };
            lin_map_set(src, kname, &tv_s); // map retains the string -> rc 2
            assert_eq!((*sval).refcount, 2);
            // key "n" -> Int64 42 (scalar, no inner heap)
            let kn = lin_string_from_bytes(b"n".as_ptr(), 1);
            let tv_n = TaggedVal { tag: TAG_INT64, _pad: [0; 7], payload: 42 };
            lin_map_set(src, kn, &tv_n);
            // Drop our local +1 on the string and keys (map keeps its own refs).
            lin_string_release(sval); // rc -> 1, owned only by the source map
            assert_eq!((*sval).refcount, 1);

            // Deep-copy for transfer.
            let dst = clone_map(src);
            assert!(!dst.is_null() && dst != src);
            assert_eq!(lin_map_length(dst), 2);
            // The cloned string must be a PRIVATE copy: distinct pointer, same bytes, its own rc 1.
            let got = lin_map_get(dst, kname);
            assert!(!got.is_null());
            assert_eq!((*got).tag, TAG_STR);
            let ds = (*got).payload as *const LinString;
            assert_ne!(ds, sval as *const LinString, "value string must be a private copy");
            assert!(lin_string_eq(ds, sval));
            assert_eq!((*ds).refcount, 1, "clone owns the sole +1 of its string");
            // The scalar value copies verbatim.
            let gotn = lin_map_get(dst, kn);
            assert_eq!((*gotn).tag, TAG_INT64);
            assert_eq!((*gotn).payload, 42);

            // Drop the SOURCE: frees src's string. The clone's string is independent.
            lin_map_release(src);
            assert_eq!((*ds).refcount, 1, "source drop must not touch the clone's string");
            // Drop the clone: frees its private string exactly once (ASan verifies no leak/UAF).
            lin_map_release(dst);
            lin_string_release(kname);
            lin_string_release(kn);
        }
    }

    #[test]
    fn transfer_scalar_box_is_independent() {
        unsafe {
            let a = alloc_tagged(TAG_INT32, 5);
            let b = lin_transfer_clone(a);
            assert!(!b.is_null());
            assert_ne!(a, b);
            assert_eq!((*(b as *const TaggedVal)).payload, 5);
        }
    }

    #[test]
    fn transfer_null_is_null() {
        unsafe {
            assert!(lin_transfer_clone(std::ptr::null()).is_null());
        }
    }

    // ADR-063: a SEALED-RECORD packed array (elem_tag 0xFE) with a STRING heap field, deep-copied
    // for cross-thread transfer via `clone_array` -> `clone_sealed_array`. Asserts: (1) the clone is a
    // distinct 0xFE array preserving stride/desc/named_desc, (2) its String field is a PRIVATE copy
    // (a distinct pointer, share-nothing), (3) RC is balanced — dropping the source array does NOT
    // free the clone's string, and dropping the clone frees only its own. The PRE-FIX path
    // (`lin_array_clone_flat`) mis-sized the buffer (16 B/elem, not the real stride) and dropped the
    // descriptors, so the clone aliased the source's String (a cross-thread share -> UAF) — this test
    // would corrupt/UAF under ASan on the old path. Run under `cargo test`'s asan CI leg.
    #[test]
    fn clone_sealed_array_string_field_is_private_and_rc_balanced() {
        use crate::sealed::{lin_sealed_alloc, lin_sealed_release_self, SEALED_HEADER, KIND_STRING};
        unsafe {
            // Record R { name: String @24, n: Int32 @32 }, stride 16. (SEALED_HEADER = 24)
            let mut heap_desc = Vec::new();
            heap_desc.extend_from_slice(&1u32.to_le_bytes()); // 1 heap field
            heap_desc.extend_from_slice(&24u32.to_le_bytes()); // offset
            heap_desc.extend_from_slice(&KIND_STRING.to_le_bytes());
            let stride = 16u64;
            let src = crate::array::lin_sealed_array_alloc(4, stride, heap_desc.as_ptr(), std::ptr::null());
            // Push two elements, each owning a +1 (non-interned) String.
            for txt in ["alpha", "beta"] {
                let st = lin_sealed_alloc(SEALED_HEADER + stride as usize, heap_desc.as_ptr(), std::ptr::null());
                let s = crate::string::lin_string_from_bytes(txt.as_ptr(), txt.len() as u32);
                *((st.add(24)) as *mut *mut u8) = s as *mut u8; // struct owns the +1
                *((st.add(32)) as *mut i32) = txt.len() as i32;
                // Borrowed-source push: array retains each heap field (string rc -> 2).
                crate::array::lin_sealed_array_push_struct_retaining(src, st);
                lin_sealed_release_self(st); // string rc -> 1, owned only by the array
            }
            assert_eq!((*src).len, 2);
            // Deep-copy for transfer.
            let dst = clone_array(src);
            assert!(!dst.is_null() && dst != src);
            assert_eq!((*dst).elem_tag, crate::array::SEALED_ARRAY_TAG);
            assert_eq!((*dst).elem_stride, stride);
            assert_eq!((*dst).len, 2);
            assert_eq!((*dst).elem_desc, heap_desc.as_ptr()); // descriptor preserved
            // Each element's String must be a PRIVATE copy (distinct pointer, same bytes, rc 1).
            for i in 0..2u64 {
                let sp = ((*src).data as *const u8).add((i * stride) as usize);
                let dp = ((*dst).data as *const u8).add((i * stride) as usize);
                let ss = *(sp as *const *const crate::string::LinString);
                let ds = *(dp as *const *const crate::string::LinString);
                assert_ne!(ss, ds, "elem {i} string must be a private copy, not aliased");
                assert!(crate::string::lin_string_eq(ss, ds), "elem {i} bytes must match");
                assert_eq!((*ds).refcount, 1, "clone owns the sole +1 of its string");
            }
            // Drop the SOURCE first: frees src's strings. The clone's strings are independent.
            crate::array::lin_array_release(src);
            // The clone's strings must still be readable (no UAF) and still rc 1.
            for i in 0..2u64 {
                let dp = ((*dst).data as *const u8).add((i * stride) as usize);
                let ds = *(dp as *const *const crate::string::LinString);
                assert_eq!((*ds).refcount, 1);
                let want = if i == 0 { "alpha" } else { "beta" };
                let wp = crate::string::lin_string_from_bytes(want.as_ptr(), want.len() as u32);
                assert!(crate::string::lin_string_eq(ds, wp));
                crate::string::lin_string_release(wp);
            }
            // Drop the clone: frees its private strings exactly once (ASan verifies no leak/double-free).
            crate::array::lin_array_release(dst);
        }
    }
}
