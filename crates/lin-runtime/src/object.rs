use std::alloc::{alloc, realloc, Layout};
use crate::string::LinString;
use crate::tagged::TaggedVal;

/// Dynamic object (Json-typed) represented as an array of key-value pairs.
/// Layout: refcount (u32) | len (u32) | cap (u32) | flags (u32) | entries (*mut LinObjectEntry)
///
/// SINGLE-ALLOCATION optimization: a freshly-`alloc`'d object places its entries buffer
/// *immediately after the header in the same allocation* (`entries` points just past the
/// header) and sets `FLAG_INLINE`. This halves the per-object allocator traffic (1 malloc/free
/// instead of header + separate entries) and co-locates header + entries on the same cache line
/// — a big win for record-heavy code that builds millions of small objects. The `entries`
/// pointer field is RETAINED (not removed), so every reader/writer is unchanged: they keep
/// loading `(*obj).entries` and indexing it. On the rare GROW, an inline object MIGRATES its
/// entries to a separate heap buffer (clearing `FLAG_INLINE`); the *header* never moves, so
/// shared `LinObject*` holders stay valid (they already re-read `(*obj).entries` each access).
#[repr(C)]
pub struct LinObject {
    pub refcount: u32,
    pub len: u32,
    pub cap: u32,
    flags: u32,
    pub entries: *mut LinObjectEntry,
}

/// `flags` bit: the entries buffer is inline (part of the header allocation), so it must NOT be
/// freed separately and a grow must migrate it to a heap buffer first.
const FLAG_INLINE: u32 = 1;

#[repr(C)]
pub struct LinObjectEntry {
    pub key: *mut LinString,
    pub value: TaggedVal,
}

/// Layout for a one-block object: the `LinObject` header followed by `cap` inline entries.
unsafe fn inline_object_layout(cap: u32) -> Layout {
    let size = std::mem::size_of::<LinObject>()
        + std::mem::size_of::<LinObjectEntry>() * cap as usize;
    // Both types are 8-aligned; the header size is a multiple of the entry alignment so the
    // inline entries start correctly aligned right after it.
    let align = std::mem::align_of::<LinObject>().max(std::mem::align_of::<LinObjectEntry>());
    Layout::from_size_align_unchecked(size, align)
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

/// Pointer to the inline entries region (immediately past the header).
#[inline]
unsafe fn inline_entries_ptr(obj: *mut LinObject) -> *mut LinObjectEntry {
    (obj as *mut u8).add(std::mem::size_of::<LinObject>()) as *mut LinObjectEntry
}

/// Migrate an inline-entries object to a separately-heap-allocated entries buffer of `new_cap`
/// (copying the existing `len` entries by raw bytes — ownership of keys/values moves with them).
/// Clears `FLAG_INLINE`. Used on the first grow of an inline object. The header does not move.
#[inline]
unsafe fn migrate_inline_to_heap(obj: *mut LinObject, new_cap: u32) {
    let len = (*obj).len as usize;
    let buf = alloc(entries_layout(new_cap)) as *mut LinObjectEntry;
    if len > 0 {
        std::ptr::copy_nonoverlapping(inline_entries_ptr(obj), buf, len);
    }
    (*obj).entries = buf;
    (*obj).cap = new_cap;
    (*obj).flags &= !FLAG_INLINE;
}

#[no_mangle]
pub unsafe extern "C" fn lin_object_alloc(initial_cap: u32) -> *mut LinObject {
    // Honor the caller's exact capacity hint (codegen passes the literal's field count for the
    // no-spread case, so a 3-field literal allocates 3 entries). Keep a minimum of 1 so the
    // entries region is never zero-size and the grow path's `cap * 2` always makes progress.
    //
    // SINGLE ALLOCATION: header + `cap` entries in one block; `entries` points just past the
    // header; FLAG_INLINE set. One malloc instead of two, header+entries on one cache line.
    let cap = initial_cap.max(1);
    let ptr = alloc(inline_object_layout(cap)) as *mut LinObject;
    (*ptr).refcount = 1;
    (*ptr).len = 0;
    (*ptr).cap = cap;
    (*ptr).flags = FLAG_INLINE;
    (*ptr).entries = inline_entries_ptr(ptr);
    ptr
}

/// Release the heap-allocated payload of a TaggedVal (decrement refcount / free).
/// Does NOT free the TaggedVal box itself (used for inline-stored entries).
unsafe fn release_tagged_payload(tv: &TaggedVal) {
    use crate::tagged::*;
    let payload = tv.payload;
    match tv.tag {
        TAG_STR => crate::string::lin_string_release(payload as *mut crate::string::LinString),
        TAG_ARRAY => crate::array::lin_array_release(payload as *mut crate::array::LinArray),
        TAG_OBJECT => lin_object_release(payload as *mut LinObject),
        TAG_FUNCTION => crate::memory::lin_closure_release(payload as *mut u8),
        TAG_SHARED => crate::shared::lin_shared_release_box(payload as *const u8),
        TAG_STREAM => crate::stream::lin_stream_release_box(payload as *const u8),
        _ => {}
    }
}

/// Retain the heap-allocated payload of a TaggedVal (increment refcount).
/// Used when copying a TaggedVal into an object/array slot so the new owner has a reference.
unsafe fn retain_tagged_payload(tv: &TaggedVal) {
    use crate::tagged::*;
    let payload = tv.payload;
    match tv.tag {
        TAG_STR => {
            // inc_ref leaves interned literals (saturated refcount) untouched; ordinary strings
            // are bumped as before.
            crate::string::lin_string_inc_ref(payload as *mut crate::string::LinString);
        }
        TAG_ARRAY => {
            let a = payload as *mut crate::array::LinArray;
            // Skip frozen (immortal) arrays — their refcount must never be written (read-only,
            // shared across threads). Mirror of the string immortal guard.
            if !a.is_null() && (*a).refcount < crate::string::IMMORTAL_RC { (*a).refcount += 1; }
        }
        TAG_OBJECT => {
            let o = payload as *mut LinObject;
            if !o.is_null() && (*o).refcount < crate::string::IMMORTAL_RC { (*o).refcount += 1; }
        }
        TAG_SHARED => {
            // Atomic refcount on the Shared box (cross-thread-shared).
            crate::shared::lin_shared_retain_box(payload as *const u8);
        }
        TAG_STREAM => {
            // Refcount on the Stream box; the matching release runs the auto-close finalizer.
            crate::stream::lin_stream_retain_box(payload as *const u8);
        }
        _ => {} // scalars: no heap payload
    }
}

/// Public wrapper for retain_tagged_payload, used by array.rs when pushing tagged values.
pub unsafe fn retain_tagged_payload_pub(tv: &TaggedVal) {
    retain_tagged_payload(tv);
}

/// Retain the heap payload of a boxed TaggedVal* (tag-aware). The codegen `Retain`
/// instruction can't use `lin_rc_retain` on a TaggedVal* — offset 0 is the tag byte, not a
/// refcount, so that would corrupt the tag. This reads the tag and bumps the inner value's
/// refcount, the mirror of the tag-aware `lin_tagged_release`. Null-safe.
#[no_mangle]
pub unsafe extern "C" fn lin_tagged_retain(p: *const u8) {
    if p.is_null() {
        return;
    }
    retain_tagged_payload(&*(p as *const TaggedVal));
}

/// Clone a boxed TaggedVal*: allocate a FRESH TaggedVal box copying the tag+payload and
/// retain the inner heap payload (if any). Returns an independently-owned box that can be
/// released with `lin_tagged_release` without affecting the source box.
///
/// This is the union analogue of `lin_rc_retain` for the OWNING var-cell/global model: a
/// cell/global holding a Json/union value owns its OWN box (not an alias of a borrowed
/// caller box). Storing clones the incoming box; reading clones the cell's box; the cell's
/// release-old and the read's scope-exit release each free a box they exclusively own. This
/// keeps store/read/release-old/teardown perfectly symmetric (mirroring the concrete-rc
/// retain/release pairs) and never frees a box owned by someone else.
///
/// Null-safe (null Json → null box). Cached scalar boxes (small ints, bools) are returned
/// as-is: they are immutable statics, carry no heap payload, and `lin_tagged_release`
/// no-ops on them, so an alias is safe and avoids a needless allocation.
#[no_mangle]
pub unsafe extern "C" fn lin_tagged_clone(p: *const u8) -> *mut u8 {
    if p.is_null() {
        return std::ptr::null_mut();
    }
    if crate::tagged::is_cached_box_pub(p) {
        return p as *mut u8;
    }
    let src = &*(p as *const TaggedVal);
    retain_tagged_payload(src);
    crate::tagged::alloc_tagged(src.tag, src.payload)
}

/// Set a field. Key must be a LinString*. Value is a TaggedVal* (pointer to tagged payload).
/// Copies the 16-byte TaggedVal struct and retains the inner value (the object owns a reference).
/// Takes ownership of the key reference (caller must not release it — use lin_object_keys'
/// retained references or freshly-allocated strings).
#[no_mangle]
pub unsafe extern "C" fn lin_object_set(obj: *mut LinObject, key: *mut LinString, val: *const TaggedVal) {
    use crate::tagged::TAG_NULL;
    let null_tv = TaggedVal { tag: TAG_NULL, _pad: [0; 7], payload: 0 };
    // Null pointer represents the null Json value — use a local null TaggedVal instead.
    let val_ref: &TaggedVal = if val.is_null() { &null_tv } else { &*val };
    let len = (*obj).len;
    // Check if key already exists.
    for i in 0..len {
        let entry = (*obj).entries.add(i as usize);
        if lin_string_key_eq((*entry).key, key) {
            // Update existing entry: release old value, copy new, retain new.
            // The caller is responsible for releasing the new key they passed in.
            release_tagged_payload(&(*entry).value);
            std::ptr::copy_nonoverlapping(val_ref, &mut (*entry).value, 1);
            retain_tagged_payload(val_ref);
            return;
        }
    }
    // New key — grow if needed.
    let cap = (*obj).cap;
    if len == cap {
        let new_cap = cap * 2;
        if (*obj).flags & FLAG_INLINE != 0 {
            // Inline entries can't be realloc'd in place (they're part of the header block, and
            // the header must not move). Migrate to a separate heap buffer of the new capacity.
            migrate_inline_to_heap(obj, new_cap);
        } else {
            let old_layout = entries_layout(cap);
            let new_layout = entries_layout(new_cap);
            (*obj).entries = realloc((*obj).entries as *mut u8, old_layout, new_layout.size()) as *mut LinObjectEntry;
            (*obj).cap = new_cap;
        }
    }
    let slot = (*obj).entries.add(len as usize);
    // Retain the key: the object owns one reference.
    // Caller retains their own reference and must release it separately.
    // inc_ref is a no-op for interned literal keys (saturated refcount).
    crate::string::lin_string_inc_ref(key);
    (*slot).key = key;
    std::ptr::copy_nonoverlapping(val_ref, &mut (*slot).value, 1);
    // Retain the value's inner payload — the object now owns a reference.
    retain_tagged_payload(val_ref);
    (*obj).len = len + 1;
}

/// Append a field for a statically-known-distinct key, with the SAME ownership semantics as
/// `lin_object_set`'s append branch but WITHOUT the linear dup-check scan.
///
/// Used only for object-literal construction (codegen `MakeObject`), where the keys appended
/// through this path are guaranteed distinct: the codegen de-duplicates literal keys (last
/// wins) before emitting, and this path is only used when the literal has no spreads (so a
/// field cannot collide with a spread-provided key). For genuine dynamic `obj[k]=v`, spreads,
/// and merges, `lin_object_set` (which dup-checks/overwrites) is still used.
///
/// Ownership (must stay identical to `lin_object_set`'s append branch so RC stays balanced —
/// the IR lowering accounts for object_set RETAINING the value's inner payload):
///   - the key is `inc_ref`'d (no-op for interned literal keys with a saturated refcount); the
///     object owns one reference, the caller keeps and releases their own;
///   - the 16-byte TaggedVal is copied into the slot and its inner heap payload retained, so the
///     object owns its own reference to the value (the source box keeps its own).
/// A null `val` pointer is treated as the null Json value (matching `lin_object_set`).
#[no_mangle]
pub unsafe extern "C" fn lin_object_set_fresh(obj: *mut LinObject, key: *mut LinString, val: *const TaggedVal) {
    use crate::tagged::TAG_NULL;
    let null_tv = TaggedVal { tag: TAG_NULL, _pad: [0; 7], payload: 0 };
    let val_ref: &TaggedVal = if val.is_null() { &null_tv } else { &*val };
    let len = (*obj).len;
    let cap = (*obj).cap;
    if len == cap {
        let new_cap = if cap == 0 { 1 } else { cap * 2 };
        if (*obj).flags & FLAG_INLINE != 0 {
            migrate_inline_to_heap(obj, new_cap);
        } else {
            let old_layout = entries_layout(cap);
            let new_layout = entries_layout(new_cap);
            (*obj).entries = realloc((*obj).entries as *mut u8, old_layout, new_layout.size()) as *mut LinObjectEntry;
            (*obj).cap = new_cap;
        }
    }
    let slot = (*obj).entries.add(len as usize);
    // inc_ref is a no-op for interned literal keys (saturated refcount).
    crate::string::lin_string_inc_ref(key);
    (*slot).key = key;
    std::ptr::copy_nonoverlapping(val_ref, &mut (*slot).value, 1);
    // Retain the value's inner payload — the object now owns a reference.
    retain_tagged_payload(val_ref);
    (*obj).len = len + 1;
}

/// Append an entry taking OWNERSHIP of an already-owned `key` and `value` (no retain). The
/// caller must not release either afterwards. Assumes `key` does not already exist (used by
/// the thread-transfer deep-copy path, which builds a fresh object from distinct keys). Grows
/// the entry buffer as needed.
pub unsafe fn object_push_owned(obj: *mut LinObject, key: *mut LinString, value: TaggedVal) {
    let len = (*obj).len;
    let cap = (*obj).cap;
    if len == cap {
        let new_cap = cap * 2;
        if (*obj).flags & FLAG_INLINE != 0 {
            migrate_inline_to_heap(obj, new_cap);
        } else {
            let old_layout = entries_layout(cap);
            let new_layout = entries_layout(new_cap);
            (*obj).entries = realloc((*obj).entries as *mut u8, old_layout, new_layout.size()) as *mut LinObjectEntry;
            (*obj).cap = new_cap;
        }
    }
    let slot = (*obj).entries.add(len as usize);
    (*slot).key = key;
    (*slot).value = value;
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

/// Look up `key` in `obj` ONCE. If it maps to an array, return that array (the live interior
/// one, so a subsequent `push` mutates it in place); otherwise insert a fresh empty array under
/// `key` and return that. The result is a `Json` value (`TaggedVal*(Array)`) so the caller can
/// pass it straight to `push`. Backs `std/array.groupBy`, replacing the get-then-set double
/// lookup with one lookup + push.
///
/// `obj` crosses the FFI boundary as a `Json` (`TaggedVal*(Object)`) or raw `LinObject*`; `key`
/// is a `String` (`LinString*`). Both are BORROWED.
///
/// RC: the array always lives INSIDE the object (object owns it). The returned `Json` box is an
/// owned +1 like every other foreign `Json` result, so we RETAIN the array before boxing it —
/// the box's eventual scope-exit release brings the count back down, leaving the object's own
/// reference intact. `push` into the returned box mutates the interior array IN PLACE (push
/// borrows its array arg; it neither retains nor replaces it), so the mutation is visible
/// through the object. On the insert path the object takes its own +1 via `object_push_owned`
/// over a freshly-allocated (rc=1) array, and we bump once more for the returned box.
#[no_mangle]
pub unsafe extern "C" fn lin_object_get_or_insert_array(obj: *const u8, key: *const u8) -> *mut u8 {
    use crate::tagged::*;
    if obj.is_null() {
        // No object to mutate; hand back a fresh empty array so `push` is still well-defined.
        return alloc_tagged(TAG_ARRAY, crate::array::lin_array_alloc(4) as u64);
    }
    // Unwrap a boxed Json object to the raw LinObject*; a raw LinObject* is used as-is.
    let lin_obj = if *obj == TAG_OBJECT {
        (*(obj as *const TaggedVal)).payload as *mut LinObject
    } else {
        obj as *mut LinObject
    };
    // `key` arrives as a LinString* (possibly boxed as Json(Str) — unwrap if so).
    let key_str = if !key.is_null() && *key == TAG_STR {
        (*(key as *const TaggedVal)).payload as *const LinString
    } else {
        key as *const LinString
    };

    // Single lookup.
    let existing = lin_object_get(lin_obj, key_str);
    if !existing.is_null() && (*existing).tag == TAG_ARRAY {
        let arr = (*existing).payload as *mut crate::array::LinArray;
        // Retain: the returned Json box owns a +1; the object keeps its own reference.
        if !arr.is_null() && (*arr).refcount < crate::string::IMMORTAL_RC {
            (*arr).refcount += 1;
        }
        return alloc_tagged(TAG_ARRAY, arr as u64);
    }

    // Absent (or present-but-not-an-array): create a fresh empty array and insert it.
    let arr = crate::array::lin_array_alloc(4); // rc = 1
    // Build a TaggedVal(Array) and store it via the dup-checking setter so a present-but-
    // non-array value is overwritten (release its old payload) rather than duplicated.
    let val = TaggedVal { tag: TAG_ARRAY, _pad: [0; 7], payload: arr as u64 };
    // lin_object_set RETAINS the value's inner payload (arr -> rc 2) and keeps its own key ref.
    lin_object_set(lin_obj, key_str as *mut LinString, &val);
    // Drop our construction +1: the object now owns its retained reference (rc back to 1 in the
    // object). Then bump once for the returned box's owned +1 (rc 2: object + returned box).
    // Net since alloc (rc1): set retained (+1 = 2), we release our build ref (-1 = 1), box (+1 = 2).
    crate::array::lin_array_release(arr);
    if (*arr).refcount < crate::string::IMMORTAL_RC {
        (*arr).refcount += 1;
    }
    alloc_tagged(TAG_ARRAY, arr as u64)
}

/// Copy all fields from `src` into `dst`, overwriting existing keys.
/// Used to implement object spread: `{ ...src, ... }`.
#[no_mangle]
pub unsafe extern "C-unwind" fn lin_object_merge(dst: *mut LinObject, src: *const LinObject) {
    if src.is_null() {
        return; // spreading null contributes nothing
    }
    let src_len = (*src).len;
    for i in 0..src_len {
        let entry = (*src).entries.add(i as usize);
        lin_object_set(dst, (*entry).key, &(*entry).value);
    }
}

/// Return a LinArray* containing all keys as LinString* (tagged TAG_STR).
/// Each key string's refcount is incremented so the array owns a reference.
#[no_mangle]
pub unsafe extern "C" fn lin_object_keys(obj: *const LinObject) -> *mut crate::array::LinArray {
    let len = if obj.is_null() { 0 } else { (*obj).len };
    let arr = crate::array::lin_array_alloc(len as u64);
    for i in 0..len {
        let entry = (*obj).entries.add(i as usize);
        let key = (*entry).key;
        // Retain so the array owns a reference to each key string (no-op for interned literals).
        crate::string::lin_string_inc_ref(key);
        let slot = (*arr).data.add(i as usize);
        (*slot).tag = crate::tagged::TAG_STR;
        (*slot).payload = key as u64;
    }
    (*arr).len = len as u64;
    arr
}

/// Return a LinArray* containing all values as TaggedVal (each stored inline).
#[no_mangle]
pub unsafe extern "C" fn lin_object_values(obj: *const LinObject) -> *mut crate::array::LinArray {
    let len = if obj.is_null() { 0 } else { (*obj).len };
    let arr = crate::array::lin_array_alloc(len as u64);
    for i in 0..len {
        let entry = (*obj).entries.add(i as usize);
        let src = &(*entry).value;
        let slot = (*arr).data.add(i as usize) as *mut TaggedVal;
        std::ptr::copy_nonoverlapping(src as *const TaggedVal, slot, 1);
        retain_tagged_payload(src);
    }
    (*arr).len = len as u64;
    arr
}

/// Return a LinArray* of pairs (each pair is a LinArray* with [key, value]).
#[no_mangle]
pub unsafe extern "C" fn lin_object_entries(obj: *const LinObject) -> *mut crate::array::LinArray {
    let len = if obj.is_null() { 0 } else { (*obj).len };
    let out = crate::array::lin_array_alloc(len as u64);
    for i in 0..len {
        let entry = (*obj).entries.add(i as usize);
        // Build pair array [key, value]
        let pair = crate::array::lin_array_alloc(2);
        (*(*pair).data.add(0)).tag = crate::tagged::TAG_STR;
        (*(*pair).data.add(0)).payload = (*entry).key as u64;
        crate::string::lin_string_inc_ref((*entry).key); // array slot owns a ref to the key string (no-op for interned literals)
        let val_src = &(*entry).value;
        std::ptr::copy_nonoverlapping(val_src as *const TaggedVal, (*pair).data.add(1) as *mut TaggedVal, 1);
        retain_tagged_payload(val_src);
        (*pair).len = 2;
        // Store pair pointer in output array as TAG_ARRAY
        let slot = (*out).data.add(i as usize);
        (*slot).tag = crate::tagged::TAG_ARRAY;
        (*slot).payload = pair as u64;
    }
    (*out).len = len as u64;
    out
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

/// Check if a boxed value (TaggedVal*) is an object that has `key`. Returns 0 for null
/// or non-object values. Does the tag check + unbox internally so callers need no
/// branching (used by the IR `has`-pattern lowering).
#[no_mangle]
pub unsafe extern "C" fn lin_value_has_field(tagged: *const u8, key: *const LinString) -> u8 {
    use crate::tagged::{TaggedVal, TAG_OBJECT};
    if tagged.is_null() { return 0; }
    let tv = &*(tagged as *const TaggedVal);
    if tv.tag != TAG_OBJECT { return 0; }
    let obj = tv.payload as *const LinObject;
    lin_object_has(obj, key)
}

/// Check if a boxed value (TaggedVal*) is an array of length `n` (exact) or `>= n` when
/// `at_least != 0`. Returns 0 for null/non-array values. Branchless helper for the IR
/// array-pattern lowering.
#[no_mangle]
pub unsafe extern "C" fn lin_value_array_len_check(tagged: *const u8, n: u64, at_least: u8) -> u8 {
    use crate::tagged::{TaggedVal, TAG_ARRAY};
    if tagged.is_null() { return 0; }
    let tv = &*(tagged as *const TaggedVal);
    if tv.tag != TAG_ARRAY { return 0; }
    let arr = tv.payload as *const crate::array::LinArray;
    if arr.is_null() { return 0; }
    let len = (*arr).len as u64;
    let ok = if at_least != 0 { len >= n } else { len == n };
    ok as u8
}

/// Deep structural equality for two objects: same keys and values, order-independent.
/// Returns 1 if equal, 0 if not.
#[no_mangle]
pub unsafe extern "C" fn lin_object_eq(a: *const LinObject, b: *const LinObject) -> u8 {

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
    let ap = (*a).payload;
    let bp = (*b).payload;
    if at == TAG_NULL && bt == TAG_NULL { return true; }
    if at == TAG_NULL || bt == TAG_NULL { return false; }
    // Cross-numeric: widen to f64 so Int32(1) == Int64(1).
    let at_is_num = (at >= TAG_INT32 && at <= TAG_FLOAT64) || at == crate::tagged::TAG_UINT64;
    let bt_is_num = (bt >= TAG_INT32 && bt <= TAG_FLOAT64) || bt == crate::tagged::TAG_UINT64;
    if at_is_num && bt_is_num && at != bt {
        return tagged_as_f64(at, ap) == tagged_as_f64(bt, bp);
    }
    if at != bt { return false; }
    if at == TAG_BOOL { return ap == bp; }
    if at == TAG_INT32 { return (ap as i32) == (bp as i32); }
    if at == TAG_INT64 { return (ap as i64) == (bp as i64); }
    if at == crate::tagged::TAG_UINT64 { return ap == bp; }
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
    if at == TAG_OBJECT {
        let ao = ap as *const LinObject;
        let bo = bp as *const LinObject;
        return lin_object_eq(ao, bo) != 0;
    }
    if at == TAG_ARRAY {
        let aa = ap as *const crate::array::LinArray;
        let ba = bp as *const crate::array::LinArray;
        return lin_array_eq_deep(aa, ba);
    }
    // For other types (closures, iterators): pointer equality.
    ap == bp
}

/// Deep equality for arrays: dispatches on elem_tag to handle flat vs tagged layouts.
unsafe fn lin_array_eq_deep(a: *const crate::array::LinArray, b: *const crate::array::LinArray) -> bool {
    use crate::tagged::*;
    if a == b { return true; }
    if a.is_null() || b.is_null() { return false; }
    let len = (*a).len;
    if len != (*b).len { return false; }
    let tag_a = (*a).elem_tag;
    let tag_b = (*b).elem_tag;
    if tag_a != tag_b { return false; }
    match tag_a {
        TAG_INT32 => {
            let da = (*a).data as *const i32;
            let db = (*b).data as *const i32;
            for i in 0..len as usize {
                if *da.add(i) != *db.add(i) { return false; }
            }
        }
        TAG_INT64 => {
            let da = (*a).data as *const i64;
            let db = (*b).data as *const i64;
            for i in 0..len as usize {
                if *da.add(i) != *db.add(i) { return false; }
            }
        }
        TAG_FLOAT32 => {
            let da = (*a).data as *const f32;
            let db = (*b).data as *const f32;
            for i in 0..len as usize {
                if *da.add(i) != *db.add(i) { return false; }
            }
        }
        TAG_FLOAT64 => {
            let da = (*a).data as *const f64;
            let db = (*b).data as *const f64;
            for i in 0..len as usize {
                if *da.add(i) != *db.add(i) { return false; }
            }
        }
        _ => {
            // Tagged array (elem_tag == 0xFF or any other): elements are LinArrayElem.
            for i in 0..len as usize {
                let ae = (*a).data.add(i);
                let be = (*b).data.add(i);
                let av = ae as *const crate::tagged::TaggedVal;
                let bv = be as *const crate::tagged::TaggedVal;
                if !tagged_val_eq(av, bv) { return false; }
            }
        }
    }
    true
}

/// Decrement refcount; when it reaches zero, release all keys and heap-typed values then free.
#[no_mangle]
pub unsafe extern "C" fn lin_object_release(obj: *mut LinObject) {
    if obj.is_null() {
        return;
    }
    // Frozen (immortal) objects: saturated refcount, never freed/decremented (Frozen<T>,
    // ADR-045). Guard makes retain/release no-ops so concurrent reads are race-free.
    if (*obj).refcount >= crate::string::IMMORTAL_RC {
        return;
    }
    // Zero refcount ⇒ double-release (ownership bug); the decrement below would wrap u32.
    // Debug/ASan-only guard, no release-build cost.
    debug_assert!((*obj).refcount > 0, "lin_object_release: refcount underflow (double free)");
    (*obj).refcount -= 1;
    if (*obj).refcount == 0 {
        use crate::tagged::*;
        let len = (*obj).len as usize;
        for i in 0..len {
            let entry = (*obj).entries.add(i);
            // Keys are always owned LinString*.
            crate::string::lin_string_release((*entry).key);
            // Values: release heap-typed payloads.
            let tag = (*entry).value.tag;
            let payload = (*entry).value.payload;
            match tag {
                TAG_STR => {
                    crate::string::lin_string_release(payload as *mut crate::string::LinString);
                }
                TAG_ARRAY => {
                    crate::array::lin_array_release(payload as *mut crate::array::LinArray);
                }
                TAG_OBJECT => {
                    lin_object_release(payload as *mut LinObject);
                }
                TAG_FUNCTION => {
                    crate::memory::lin_closure_release(payload as *mut u8);
                }
                _ => {} // scalars: no heap payload
            }
        }
        let cap = (*obj).cap;
        if (*obj).flags & FLAG_INLINE != 0 {
            // Entries live inside the header allocation — one dealloc frees both.
            std::alloc::dealloc(obj as *mut u8, inline_object_layout(cap));
        } else {
            // Entries were migrated to a separate heap buffer (object grew).
            std::alloc::dealloc((*obj).entries as *mut u8, entries_layout(cap));
            std::alloc::dealloc(obj as *mut u8, object_layout());
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_object_length(obj: *const LinObject) -> i64 {
    if obj.is_null() { return 0; }
    (*obj).len as i64
}

/// Copy all fields from `src` into `dst` except those whose keys are in `excluded`.
/// `excluded` is a pointer to `n_excluded` LinString* values.
/// Used to implement object rest destructuring: `val { a, b, ...rest } = obj`.
#[no_mangle]
pub unsafe extern "C" fn lin_object_copy_except(
    dst: *mut LinObject,
    src: *const LinObject,
    excluded: *const *const LinString,
    n_excluded: u32,
) {
    if src.is_null() { return; }
    let len = (*src).len;
    'outer: for i in 0..len {
        let entry = (*src).entries.add(i as usize);
        let key = (*entry).key;
        for j in 0..n_excluded {
            if lin_string_key_eq(key, *excluded.add(j as usize)) {
                continue 'outer;
            }
        }
        lin_object_set(dst, key, &(*entry).value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tagged::{alloc_tagged, lin_tagged_release, TAG_STR, TAG_ARRAY, TAG_OBJECT, TAG_INT32};

    unsafe fn mk_string(s: &str) -> *mut LinString {
        crate::string::lin_string_from_bytes(s.as_ptr(), s.len() as u32)
    }

    unsafe fn get_tag(obj: *const LinObject, key: *const LinString) -> u8 {
        let tv = lin_object_get(obj, key);
        if tv.is_null() { crate::tagged::TAG_NULL } else { (*tv).tag }
    }

    // Build an object literal-style (fresh append) whose VALUES are heap types, then drop
    // it. Under ASan this proves the new no-dup-check append retains the inner payloads
    // exactly like lin_object_set (one balanced reference per slot) — no leak, no UAF.
    #[test]
    fn fresh_append_heap_values_balanced_rc() {
        unsafe {
            // Distinct literal keys (caller owns one ref each, as codegen does).
            let kx = mk_string("x");
            let ky = mk_string("y");
            let kz = mk_string("z");

            // Heap-typed values: a string, an array, a nested object. The caller box owns
            // the +1; lin_object_set_fresh retains the inner so the object owns its own ref.
            let sval = mk_string("hello");
            let s_box = alloc_tagged(TAG_STR, sval as u64) as *const TaggedVal;

            let arr = crate::array::lin_array_alloc(2);
            (*arr).len = 0;
            let a_box = alloc_tagged(TAG_ARRAY, arr as u64) as *const TaggedVal;

            let inner = lin_object_alloc(0); // empty {} -> min cap honored
            let o_box = alloc_tagged(TAG_OBJECT, inner as u64) as *const TaggedVal;

            // cap exactly 3 (right-sized, no over-alloc / no min-4).
            let obj = lin_object_alloc(3);
            assert_eq!((*obj).cap, 3, "right-sized cap honored");

            lin_object_set_fresh(obj, kx, s_box);
            lin_object_set_fresh(obj, ky, a_box);
            lin_object_set_fresh(obj, kz, o_box);

            assert_eq!((*obj).len, 3);
            assert_eq!((*obj).cap, 3, "no growth for exactly-sized literal");
            assert_eq!(get_tag(obj, kx), TAG_STR);
            assert_eq!(get_tag(obj, ky), TAG_ARRAY);
            assert_eq!(get_tag(obj, kz), TAG_OBJECT);

            // Drop the object: releases each key (no-op for any saturated) and each inner
            // payload's owned reference.
            lin_object_release(obj);

            // The caller's own boxes still hold a live reference; release them now. If the
            // object's release had over-released, this would be a UAF/double-free under ASan.
            crate::tagged::lin_tagged_release(s_box as *mut u8);
            crate::tagged::lin_tagged_release(a_box as *mut u8);
            crate::tagged::lin_tagged_release(o_box as *mut u8);

            // Free the caller's key references (object held its own inc_ref'd copies).
            crate::string::lin_string_release(kx);
            crate::string::lin_string_release(ky);
            crate::string::lin_string_release(kz);
        }
    }

    // Growth path of fresh append: starting from a 0-hint (min cap 1) object, append past
    // capacity and confirm the realloc keeps every entry intact and RC stays balanced.
    #[test]
    fn fresh_append_growth() {
        unsafe {
            let obj = lin_object_alloc(0);
            assert_eq!((*obj).cap, 1, "min cap 1 for empty hint (no zero-size alloc)");
            let keys: Vec<*mut LinString> =
                (0..5).map(|i| mk_string(&format!("k{i}"))).collect();
            for (i, &k) in keys.iter().enumerate() {
                let v = alloc_tagged(TAG_INT32, i as u64) as *const TaggedVal;
                lin_object_set_fresh(obj, k, v);
                // scalar box: lin_tagged_release no-ops, but call it for symmetry safety.
                crate::tagged::lin_tagged_release(v as *mut u8);
            }
            assert_eq!((*obj).len, 5);
            assert!((*obj).cap >= 5);
            for (i, &k) in keys.iter().enumerate() {
                let tv = lin_object_get(obj, k);
                assert!(!tv.is_null());
                assert_eq!((*tv).payload as i32, i as i32);
            }
            lin_object_release(obj);
            for k in keys { crate::string::lin_string_release(k); }
        }
    }

    // Mirror std/array.groupBy: for each item, get-or-insert the group array under its key and
    // push the item into the returned (boxed) array IN PLACE. The returned Json box is an owned
    // +1; releasing it after each push must leave the object's own reference intact. Under ASan
    // an over-release (returning a +0 alias) frees the group while the object still points at it
    // (UAF on the next push/read); an under-release leaks every group box. Loop to amplify.
    #[test]
    fn get_or_insert_array_groupby_rc_balanced() {
        unsafe {
            use crate::array::{lin_array_length, lin_array_get_tagged};
            let obj = lin_object_alloc(2); // the `var result = {}`
            let obj_box = alloc_tagged(TAG_OBJECT, obj as u64); // object crosses as Json
            // Two interned-ish keys reused every iteration (caller owns one ref each).
            let even = crate::string::lin_string_from_bytes("even".as_ptr(), 4);
            let odd = crate::string::lin_string_from_bytes("odd".as_ptr(), 3);

            for i in 0..100i64 {
                let key = if i % 2 == 0 { even } else { odd };
                let key_box = alloc_tagged(TAG_STR, key as u64);
                // get-or-insert returns a boxed Json(Array), owned +1.
                let group_box = lin_object_get_or_insert_array(
                    obj_box as *const u8, key_box as *const u8,
                );
                let group = (*(group_box as *const TaggedVal)).payload as *mut crate::array::LinArray;
                // push the integer item into the group in place (mirrors `push(group, item)`).
                let item = alloc_tagged(TAG_INT32, i as u64);
                crate::array::lin_push_dyn(group, item as *const TaggedVal);
                // scalar box: release no-ops on cached small ints, frees otherwise.
                lin_tagged_release(item as *mut u8);
                // Release the returned group box (its +1 was for this binding only).
                lin_tagged_release(group_box);
                // key_box is a fresh box aliasing `even`/`odd`; free the shell, the key lives on.
                crate::tagged::lin_tagged_free_box(key_box);
            }

            // Two groups, 50 each, all items intact.
            let g_even = lin_object_get(obj, even);
            let g_odd = lin_object_get(obj, odd);
            assert!(!g_even.is_null() && (*g_even).tag == TAG_ARRAY);
            assert!(!g_odd.is_null() && (*g_odd).tag == TAG_ARRAY);
            let ea = (*g_even).payload as *const crate::array::LinArray;
            let oa = (*g_odd).payload as *const crate::array::LinArray;
            assert_eq!(lin_array_length(ea), 50);
            assert_eq!(lin_array_length(oa), 50);
            // Read back the first/last of each (UAF check).
            let first_even = lin_array_get_tagged(ea, 0);
            assert_eq!((*first_even).payload as i32, 0);
            std::alloc::dealloc(first_even as *mut u8, std::alloc::Layout::new::<TaggedVal>());

            // Teardown: releasing the object frees both group arrays exactly once.
            lin_object_release(obj);
            crate::tagged::lin_tagged_free_box(obj_box);
            crate::string::lin_string_release(even);
            crate::string::lin_string_release(odd);
        }
    }
}
