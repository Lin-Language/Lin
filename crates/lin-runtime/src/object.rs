use std::alloc::{alloc, realloc, Layout};
use crate::string::LinString;
use crate::tagged::TaggedVal;

/// Dynamic object (Json-typed) represented as an array of key-value pairs.
/// Layout: refcount (u32) | len (u32) | cap (u32) | _pad (u32) | entries (*mut LinObjectEntry)
#[repr(C)]
pub struct LinObject {
    pub refcount: u32,
    pub len: u32,
    pub cap: u32,
    _pad: u32,
    pub entries: *mut LinObjectEntry,
}

#[repr(C)]
pub struct LinObjectEntry {
    pub key: *mut LinString,
    pub value: TaggedVal,
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

#[no_mangle]
pub unsafe extern "C" fn lin_object_alloc(initial_cap: u32) -> *mut LinObject {
    let cap = initial_cap.max(4);
    let ptr = alloc(object_layout()) as *mut LinObject;
    (*ptr).refcount = 1;
    (*ptr).len = 0;
    (*ptr).cap = cap;
    (*ptr)._pad = 0;
    (*ptr).entries = alloc(entries_layout(cap)) as *mut LinObjectEntry;
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
            let s = payload as *mut crate::string::LinString;
            if !s.is_null() { (*s).refcount += 1; }
        }
        TAG_ARRAY => {
            let a = payload as *mut crate::array::LinArray;
            if !a.is_null() { (*a).refcount += 1; }
        }
        TAG_OBJECT => {
            let o = payload as *mut LinObject;
            if !o.is_null() { (*o).refcount += 1; }
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
        let old_layout = entries_layout(cap);
        let new_layout = entries_layout(new_cap);
        (*obj).entries = realloc((*obj).entries as *mut u8, old_layout, new_layout.size()) as *mut LinObjectEntry;
        (*obj).cap = new_cap;
    }
    let slot = (*obj).entries.add(len as usize);
    // Retain the key: the object owns one reference.
    // Caller retains their own reference and must release it separately.
    (*key).refcount += 1;
    (*slot).key = key;
    std::ptr::copy_nonoverlapping(val_ref, &mut (*slot).value, 1);
    // Retain the value's inner payload — the object now owns a reference.
    retain_tagged_payload(val_ref);
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

/// Copy all fields from `src` into `dst`, overwriting existing keys.
/// Used to implement object spread: `{ ...src, ... }`.
#[no_mangle]
pub unsafe extern "C" fn lin_object_merge(dst: *mut LinObject, src: *const LinObject) {
    if src.is_null() {
        eprintln!("Runtime error: cannot spread null into object");
        std::process::exit(1);
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
        // Retain so the array owns a reference to each key string.
        (*key).refcount += 1;
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
        (*(*entry).key).refcount += 1; // array slot owns a reference to the key string
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
    let at_is_num = at >= TAG_INT32 && at <= TAG_FLOAT64;
    let bt_is_num = bt >= TAG_INT32 && bt <= TAG_FLOAT64;
    if at_is_num && bt_is_num && at != bt {
        return tagged_as_f64(at, ap) == tagged_as_f64(bt, bp);
    }
    if at != bt { return false; }
    if at == TAG_BOOL { return ap == bp; }
    if at == TAG_INT32 { return (ap as i32) == (bp as i32); }
    if at == TAG_INT64 { return (ap as i64) == (bp as i64); }
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
        std::alloc::dealloc((*obj).entries as *mut u8, entries_layout(cap));
        std::alloc::dealloc(obj as *mut u8, object_layout());
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
