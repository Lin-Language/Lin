use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::cell::RefCell;
use std::collections::HashMap;
use crate::tagged::{TaggedVal, TAG_NULL, TAG_BOOL, TAG_INT32, TAG_INT64, TAG_FLOAT64, TAG_STR, TAG_FLOAT32, TAG_ARRAY, TAG_OBJECT};

/// Runtime string representation: reference-counted, UTF-8.
/// Layout: refcount (u32) | len (u32) | data ([u8; len])
#[repr(C)]
pub struct LinString {
    pub refcount: u32,
    pub len: u32,
    pub data: [u8; 0],
}

/// Refcount sentinel for *immortal* (interned) string literals.
///
/// String literals are compile-time constants; we allocate one shared `LinString` per distinct
/// literal for the whole program run (see `lin_string_literal`) instead of re-allocating on every
/// evaluation. To keep that shared box alive under arbitrary retain/release traffic without a
/// layout change (the `LinString` layout is pinned by codegen — refcount:u32, len:u32, data), we
/// mark it immortal by setting its refcount into the top of the u32 range.
///
/// SAFETY CONTRACT (sound under real OS-thread concurrency — async is genuine threading, ADR-042/043):
///   * `lin_string_release` returns early (before the underflow `debug_assert!` and before
///     decrementing) when `refcount >= IMMORTAL_RC`, so an interned literal is never freed even
///     when a container that holds it is dropped.
///   * Every refcount *increment* path that touches a `LinString` (`lin_rc_retain`,
///     `retain_tagged_payload`, the raw `(*key).refcount += 1` sites in object.rs) is funneled
///     through `lin_string_inc_ref`, which leaves an immortal string's refcount unchanged. So
///     retains can never push it past `u32::MAX`, and a balanced retain/release pair is a double
///     no-op — provably overflow-free regardless of how many containers borrow the literal.
/// The threshold sits halfway up the u32 range: an ordinary heap string would need 2^31 live
/// owners to reach it, which is impossible (each owner is a distinct pointer into a far smaller
/// address space), so a normal string can never be mistaken for immortal.
pub const IMMORTAL_RC: u32 = 0x8000_0000;

thread_local! {
    /// Interning cache for string literals, keyed by the literal's global data pointer.
    ///
    /// Each distinct string literal in the program has a unique, stable `@str_data` global; codegen
    /// passes that pointer to `lin_string_literal`. The first call for a given pointer allocates the
    /// `LinString` once (copying the bytes, refcount = IMMORTAL_RC) and caches it; subsequent calls
    /// return the same pointer. Net: one allocation per distinct literal per run.
    ///
    /// Thread-safe without any locking: because the cache is `thread_local!`, each thread interns
    /// into its OWN map — there is no shared map for concurrent threads to race on, so a plain
    /// `RefCell` is sufficient on the hot path. Interned strings are immortal (refcount =
    /// IMMORTAL_RC) and immutable; both retain (`lin_string_inc_ref`) and release
    /// (`lin_string_release`) no-op on them. So even though an immortal literal POINTER can escape
    /// its originating thread (e.g. `transfer::clone_string` passes immortal strings through by
    /// pointer instead of deep-copying), sharing it across threads is benign: nothing ever mutates
    /// its bytes or refcount — the same safety basis as `Frozen<T>`. Two threads may each intern
    /// distinct boxes for the same literal, which only wastes a little memory.
    /// (The scalar-box cache in tagged.rs is a compile-time `static` because TaggedVals are
    /// plain data; literals need a runtime map because the byte data lives in the compiled module,
    /// keyed by a pointer only known at run time.)
    static LITERAL_CACHE: RefCell<HashMap<(usize, u32), *mut LinString>> = RefCell::new(HashMap::new());
}

/// Increment a string's refcount, leaving immortal (interned) strings untouched.
/// All retain paths that touch a `LinString` go through this so an immortal string can never
/// overflow its refcount. Null-safe. Inlined to keep the ordinary path a single branch + add.
#[inline]
pub unsafe fn lin_string_inc_ref(s: *mut LinString) {
    if s.is_null() {
        return;
    }
    if (*s).refcount >= IMMORTAL_RC {
        return;
    }
    (*s).refcount += 1;
}

/// Return a cached, immortal `LinString` for the string literal whose byte data lives at `data`
/// (an `@str_data` global emitted by codegen) with length `len`. First call for a given pointer
/// allocates and caches; subsequent calls return the same pointer. The returned string has refcount
/// `IMMORTAL_RC` and is never freed (see `lin_string_release`) — only true compile-time literals
/// must use this; dynamic strings (concat/interpolation/fs/etc.) keep using `lin_string_from_bytes`.
#[no_mangle]
pub unsafe extern "C" fn lin_string_literal(data: *const u8, len: u32) -> *mut LinString {
    // Key on (pointer, len), not pointer alone. A zero-length global (`""`, empty interpolation
    // segment) has no storage, so the linker can place it at the *same address* as the adjacent
    // non-empty global; keying on the pointer alone would then return the empty string for a
    // distinct non-empty literal. The length disambiguates those aliasing cases. Identical-content
    // literals that LLVM constant-merges share ptr+len and are correctly deduped.
    let key = (data as usize, len);
    LITERAL_CACHE.with(|cache| {
        if let Some(&s) = cache.borrow().get(&key) {
            return s;
        }
        let ptr = lin_string_alloc(len);
        (*ptr).refcount = IMMORTAL_RC;
        if len > 0 {
            std::ptr::copy_nonoverlapping(data, (*ptr).data.as_mut_ptr(), len as usize);
        }
        cache.borrow_mut().insert(key, ptr);
        ptr
    })
}

impl LinString {
    pub unsafe fn as_str(&self) -> &str {
        let slice = std::slice::from_raw_parts(self.data.as_ptr(), self.len as usize);
        std::str::from_utf8_unchecked(slice)
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_alloc(len: u32) -> *mut LinString {
    let size = std::mem::size_of::<LinString>() + len as usize;
    let layout = Layout::from_size_align_unchecked(size, std::mem::align_of::<u32>());
    let ptr = alloc_zeroed(layout) as *mut LinString;
    (*ptr).refcount = 1;
    (*ptr).len = len;
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_free(s: *mut LinString) {
    let size = std::mem::size_of::<LinString>() + (*s).len as usize;
    let layout = Layout::from_size_align_unchecked(size, std::mem::align_of::<u32>());
    dealloc(s as *mut u8, layout);
}

/// Decrement refcount and free if zero.
#[no_mangle]
pub unsafe extern "C" fn lin_string_release(s: *mut LinString) {
    if s.is_null() {
        return;
    }
    // Immortal (interned) string literals carry a saturated refcount and must never be freed or
    // decremented — they are shared, allocated once, and outlive every container that borrows
    // them. Return before the underflow assert and the decrement. Single predictable branch on
    // the hot release path (the sentinel comparison); ordinary strings fall through unchanged.
    if (*s).refcount >= IMMORTAL_RC {
        return;
    }
    // A zero refcount here means a double-release (ownership bug in codegen/lowering): the
    // next decrement would wrap u32 and leak instead of freeing. Catch it in debug/ASan
    // builds; release builds keep the original (silent) behaviour to avoid a runtime cost.
    debug_assert!((*s).refcount > 0, "lin_string_release: refcount underflow (double free)");
    (*s).refcount -= 1;
    if (*s).refcount == 0 {
        lin_string_free(s);
    }
}

/// Create a LinString from a raw byte pointer + length. Copies the bytes.
#[no_mangle]
pub unsafe extern "C" fn lin_string_from_bytes(data: *const u8, len: u32) -> *mut LinString {
    let ptr = lin_string_alloc(len);
    if len > 0 {
        std::ptr::copy_nonoverlapping(data, (*ptr).data.as_mut_ptr(), len as usize);
    }
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_concat(a: *const LinString, b: *const LinString) -> *mut LinString {
    let a_len = (*a).len;
    let b_len = (*b).len;
    let new_len = a_len + b_len;
    let ptr = lin_string_alloc(new_len);
    let dst = (*ptr).data.as_mut_ptr();
    std::ptr::copy_nonoverlapping((*a).data.as_ptr(), dst, a_len as usize);
    std::ptr::copy_nonoverlapping((*b).data.as_ptr(), dst.add(a_len as usize), b_len as usize);
    ptr
}

/// Concatenate `n` strings in a single allocation.
/// `parts` is a pointer to an array of `n` `*const LinString` pointers.
#[no_mangle]
pub unsafe extern "C" fn lin_string_build_n(parts: *const *const LinString, n: u32) -> *mut LinString {
    let parts = std::slice::from_raw_parts(parts, n as usize);
    let total_len: u32 = parts.iter().map(|&s| (*s).len).sum();
    let ptr = lin_string_alloc(total_len);
    let mut dst = (*ptr).data.as_mut_ptr();
    for &s in parts {
        let len = (*s).len as usize;
        std::ptr::copy_nonoverlapping((*s).data.as_ptr(), dst, len);
        dst = dst.add(len);
    }
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_length(s: *const LinString) -> i32 {
    (*s).len as i32
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_eq(a: *const LinString, b: *const LinString) -> bool {
    // Null-safe, matching lin_object_eq / lin_array_eq: a Lin `null` is a null pointer,
    // and `"s" == null` / `s != null` must be a plain false — not a deref crash. Two nulls
    // are equal (both the absent value); a string vs null is unequal.
    if a == b { return true; }
    if a.is_null() || b.is_null() { return false; }
    if (*a).len != (*b).len {
        return false;
    }
    let a_slice = std::slice::from_raw_parts((*a).data.as_ptr(), (*a).len as usize);
    let b_slice = std::slice::from_raw_parts((*b).data.as_ptr(), (*b).len as usize);
    a_slice == b_slice
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_slice(
    s: *const LinString,
    start: i32,
    end: i32,
) -> *mut LinString {
    let len = (*s).len as i32;
    let start = start.clamp(0, len) as usize;
    let end = end.clamp(0, len) as usize;
    let end = end.max(start);
    let slice_len = end - start;
    let ptr = lin_string_alloc(slice_len as u32);
    if slice_len > 0 {
        std::ptr::copy_nonoverlapping(
            (*s).data.as_ptr().add(start),
            (*ptr).data.as_mut_ptr(),
            slice_len,
        );
    }
    ptr
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_char_at(s: *const LinString, index: i32) -> *mut LinString {
    let len = (*s).len as i32;
    if index < 0 || index >= len {
        return lin_string_alloc(0);
    }
    let byte = *(*s).data.as_ptr().add(index as usize);
    let ptr = lin_string_alloc(1);
    *(*ptr).data.as_mut_ptr() = byte;
    ptr
}

/// Return the Unicode code point at char index `index`. Returns -1 if OOB or negative index.
#[no_mangle]
pub unsafe extern "C" fn lin_string_char_code(s: *const LinString, index: i32) -> i32 {
    if index < 0 { return -1; }
    let st = (*s).as_str();
    st.chars().nth(index as usize).map(|c| c as i32).unwrap_or(-1)
}

/// Create a single-character string from a Unicode code point. Returns "" for invalid code points.
#[no_mangle]
pub unsafe extern "C" fn lin_string_from_char_code(code: i32) -> *mut LinString {
    if code < 0 {
        return lin_string_from_bytes(b"".as_ptr(), 0);
    }
    match char::from_u32(code as u32) {
        None => lin_string_from_bytes(b"".as_ptr(), 0),
        Some(c) => {
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            lin_string_from_bytes(s.as_ptr(), s.len() as u32)
        }
    }
}

/// Lexicographic comparison. Returns -1, 0, or 1.
#[no_mangle]
pub unsafe extern "C" fn lin_string_cmp(a: *const LinString, b: *const LinString) -> i32 {
    let a_bytes = std::slice::from_raw_parts((*a).data.as_ptr(), (*a).len as usize);
    let b_bytes = std::slice::from_raw_parts((*b).data.as_ptr(), (*b).len as usize);
    match a_bytes.cmp(b_bytes) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

// Numeric -> string conversions

#[no_mangle]
pub extern "C" fn lin_int_to_string(n: i64) -> *mut LinString {
    let s = n.to_string();
    unsafe { lin_string_from_bytes(s.as_ptr(), s.len() as u32) }
}

#[no_mangle]
pub extern "C" fn lin_uint_to_string(n: u64) -> *mut LinString {
    let s = n.to_string();
    unsafe { lin_string_from_bytes(s.as_ptr(), s.len() as u32) }
}

#[no_mangle]
pub extern "C" fn lin_float_to_string(f: f64) -> *mut LinString {
    let s = if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{:.1}", f)
    } else {
        format!("{}", f)
    };
    unsafe { lin_string_from_bytes(s.as_ptr(), s.len() as u32) }
}

#[no_mangle]
pub extern "C" fn lin_bool_to_string(b: bool) -> *mut LinString {
    let s = if b { "true" } else { "false" };
    unsafe { lin_string_from_bytes(s.as_ptr(), s.len() as u32) }
}

#[no_mangle]
pub extern "C" fn lin_null_to_string() -> *mut LinString {
    unsafe { lin_string_from_bytes("null".as_ptr(), 4) }
}

// --- String manipulation functions ---

#[no_mangle]
pub unsafe extern "C" fn lin_string_trim(s: *const LinString) -> *mut LinString {
    let st = (*s).as_str();
    let trimmed = st.trim();
    lin_string_from_bytes(trimmed.as_ptr(), trimmed.len() as u32)
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_trim_start(s: *const LinString) -> *mut LinString {
    let st = (*s).as_str();
    let trimmed = st.trim_start();
    lin_string_from_bytes(trimmed.as_ptr(), trimmed.len() as u32)
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_trim_end(s: *const LinString) -> *mut LinString {
    let st = (*s).as_str();
    let trimmed = st.trim_end();
    lin_string_from_bytes(trimmed.as_ptr(), trimmed.len() as u32)
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_to_upper(s: *const LinString) -> *mut LinString {
    let st = (*s).as_str();
    let upper = st.to_uppercase();
    lin_string_from_bytes(upper.as_ptr(), upper.len() as u32)
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_to_lower(s: *const LinString) -> *mut LinString {
    let st = (*s).as_str();
    let lower = st.to_lowercase();
    lin_string_from_bytes(lower.as_ptr(), lower.len() as u32)
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_index_of(s: *const LinString, needle: *const LinString) -> i32 {
    let st = (*s).as_str();
    let nd = (*needle).as_str();
    match st.find(nd) {
        Some(i) => i as i32,
        None => -1,
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_last_index_of(s: *const LinString, needle: *const LinString) -> i32 {
    let st = (*s).as_str();
    let nd = (*needle).as_str();
    match st.rfind(nd) {
        Some(byte_pos) => st[..byte_pos].chars().count() as i32,
        None => -1,
    }
}

/// Returns true if the string is empty or contains only whitespace. No allocation.
#[no_mangle]
pub unsafe extern "C" fn lin_string_is_blank(s: *const LinString) -> bool {
    (*s).as_str().chars().all(|c| c.is_whitespace())
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_contains(s: *const LinString, needle: *const LinString) -> bool {
    let st = (*s).as_str();
    let nd = (*needle).as_str();
    st.contains(nd)
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_starts_with(s: *const LinString, prefix: *const LinString) -> bool {
    let st = (*s).as_str();
    let pf = (*prefix).as_str();
    st.starts_with(pf)
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_ends_with(s: *const LinString, suffix: *const LinString) -> bool {
    let st = (*s).as_str();
    let sf = (*suffix).as_str();
    st.ends_with(sf)
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_replace(s: *const LinString, pattern: *const LinString, replacement: *const LinString) -> *mut LinString {
    let st = (*s).as_str();
    let pat = (*pattern).as_str();
    let rep = (*replacement).as_str();
    let result = st.replace(pat, rep);
    lin_string_from_bytes(result.as_ptr(), result.len() as u32)
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_repeat(s: *const LinString, count: i32) -> *mut LinString {
    let st = (*s).as_str();
    let n = count.max(0) as usize;
    let result = st.repeat(n);
    lin_string_from_bytes(result.as_ptr(), result.len() as u32)
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_split(s: *const LinString, delimiter: *const LinString) -> *mut crate::array::LinArray {
    use crate::array::{lin_array_alloc, lin_array_push};
    use crate::tagged::TAG_STR;
    let st = (*s).as_str();
    let delim = (*delimiter).as_str();
    let arr = lin_array_alloc(4);
    for part in st.split(delim) {
        let part_str = lin_string_from_bytes(part.as_ptr(), part.len() as u32);
        let cell = &part_str as *const *mut LinString as *const u8;
        // Tag each element TAG_STR (not 0/TAG_NULL): the array owns the fresh string.
        // A wrong tag makes generic iteration (for/map) read the element as null and
        // leaks the string on array release.
        lin_array_push(arr, cell, TAG_STR);
    }
    arr
}

#[no_mangle]
pub unsafe extern "C" fn lin_string_join(arr: *const crate::array::LinArray, separator: *const LinString) -> *mut LinString {
    use crate::array::{lin_array_length, lin_array_get};
    let n = lin_array_length(arr) as usize;
    let sep = (*separator).as_str();
    let mut parts: Vec<&str> = Vec::with_capacity(n);
    for i in 0..n {
        let elem = lin_array_get(arr, i as i64);
        // Element payload is a LinString*
        let payload_ptr = (elem as *const u8).add(8) as *const *mut LinString;
        let s_ptr = *payload_ptr;
        parts.push((*s_ptr).as_str());
    }
    let result = parts.join(sep);
    lin_string_from_bytes(result.as_ptr(), result.len() as u32)
}

/// Recursively convert a TaggedVal to its JSON string representation.
/// Used for toString(obj), toString(arr), and string interpolation of complex values.
pub unsafe fn tagged_to_json_string(tagged: *const TaggedVal) -> String {
    if tagged.is_null() {
        return "null".to_string();
    }
    let tag = (*tagged).tag;
    let payload = (*tagged).payload;
    if tag == TAG_NULL { return "null".to_string(); }
    if tag == TAG_BOOL { return if payload != 0 { "true" } else { "false" }.to_string(); }
    if tag == TAG_INT32 { return (payload as i32).to_string(); }
    if tag == TAG_INT64 { return (payload as i64).to_string(); }
    if tag == crate::tagged::TAG_UINT64 { return payload.to_string(); }
    if tag == TAG_FLOAT32 {
        let f = f32::from_bits(payload as u32);
        return format!("{}", f);
    }
    if tag == TAG_FLOAT64 {
        let f = f64::from_bits(payload);
        return format!("{}", f);
    }
    if tag == TAG_STR {
        let s = payload as *const LinString;
        if s.is_null() { return "null".to_string(); }
        return format!("\"{}\"", (*s).as_str());
    }
    if tag == TAG_ARRAY {
        let arr = payload as *const crate::array::LinArray;
        if arr.is_null() { return "[]".to_string(); }
        return array_to_json_string(arr);
    }
    if tag == TAG_OBJECT {
        let obj = payload as *const crate::object::LinObject;
        if obj.is_null() { return "{}".to_string(); }
        return object_to_json_string(obj);
    }
    "[object]".to_string()
}

unsafe fn array_to_json_string(arr: *const crate::array::LinArray) -> String {
    use crate::tagged::*;
    let len = (*arr).len as usize;
    let mut parts = Vec::with_capacity(len);
    let elem_tag = (*arr).elem_tag;
    for i in 0..len {
        let s = match elem_tag {
            0xFF => {
                // Tagged array: elements are TaggedVal structs (16 bytes each).
                let elem = (*arr).data.add(i);
                tagged_to_json_string(elem as *const TaggedVal)
            }
            TAG_INT32 => {
                let v = *((*arr).data as *const i32).add(i);
                format!("{}", v)
            }
            TAG_INT64 => {
                let v = *((*arr).data as *const i64).add(i);
                format!("{}", v)
            }
            TAG_FLOAT32 => {
                let v = *((*arr).data as *const f32).add(i) as f64;
                if v.fract() == 0.0 && v.abs() < 1e15 { format!("{:.1}", v) } else { format!("{}", v) }
            }
            TAG_FLOAT64 => {
                let v = *((*arr).data as *const f64).add(i);
                if v.fract() == 0.0 && v.abs() < 1e15 { format!("{:.1}", v) } else { format!("{}", v) }
            }
            TAG_BOOL => {
                let v = *((*arr).data as *const u8).add(i);
                if v != 0 { "true".to_string() } else { "false".to_string() }
            }
            TAG_UINT8 => {
                let v = *((*arr).data as *const u8).add(i);
                format!("{}", v)
            }
            TAG_INT8 => {
                let v = *((*arr).data as *const i8).add(i);
                format!("{}", v)
            }
            TAG_UINT16 => {
                let v = *((*arr).data as *const u16).add(i);
                format!("{}", v)
            }
            TAG_INT16 => {
                let v = *((*arr).data as *const i16).add(i);
                format!("{}", v)
            }
            TAG_UINT32 => {
                let v = *((*arr).data as *const u32).add(i);
                format!("{}", v)
            }
            TAG_UINT64 => {
                let v = *((*arr).data as *const u64).add(i);
                format!("{}", v)
            }
            _ => "null".to_string(),
        };
        parts.push(s);
    }
    format!("[{}]", parts.join(", "))
}

unsafe fn object_to_json_string(obj: *const crate::object::LinObject) -> String {
    let len = (*obj).len as usize;
    let mut parts = Vec::with_capacity(len);
    for i in 0..len {
        let entry = (*obj).entries.add(i);
        let key = (*entry).key;
        let key_str = if key.is_null() { "null".to_string() } else { (*key).as_str().to_string() };
        let val_str = tagged_to_json_string(&(*entry).value as *const TaggedVal);
        parts.push(format!("\"{}\": {}", key_str, val_str));
    }
    format!("{{{}}}", parts.join(", "))
}

/// Convert a LinArray* to its JSON string representation.
#[no_mangle]
pub unsafe extern "C" fn lin_array_to_string(arr: *const crate::array::LinArray) -> *mut LinString {
    if arr.is_null() {
        return lin_string_from_bytes(b"null".as_ptr(), 4);
    }
    let s = array_to_json_string(arr);
    lin_string_from_bytes(s.as_ptr(), s.len() as u32)
}

/// Convert a LinObject* to its JSON string representation.
#[no_mangle]
pub unsafe extern "C" fn lin_object_to_string(obj: *const crate::object::LinObject) -> *mut LinString {
    if obj.is_null() {
        return lin_string_from_bytes(b"null".as_ptr(), 4);
    }
    let s = object_to_json_string(obj);
    lin_string_from_bytes(s.as_ptr(), s.len() as u32)
}

/// Produce a canonical, type-tagged key string for any value, suitable for use as an object key.
/// This allows using a plain {} object as a hash set keyed on arbitrary Lin values.
///
/// Format (type-prefixed to avoid cross-type collisions):
///   null      → "N"
///   bool      → "b:true" / "b:false"
///   int32     → "i:<n>"
///   int64     → "I:<n>"
///   float32   → "f:<n>"
///   float64   → "F:<n>"
///   string    → "s:<content>"
///   array     → "a:[<elem>,...]"  (elements recursively keyed)
///   object    → "o:{<k1>:<v1>,...}" (keys sorted for order-independence)
///   function  → "fn:<pointer>"  (identity — consistent with no function equality)
///   iterator  → "it:<pointer>"
#[no_mangle]
pub unsafe extern "C" fn lin_value_key(tagged: *const TaggedVal) -> *mut LinString {
    let s = tagged_to_key_string(tagged);
    lin_string_from_bytes(s.as_ptr(), s.len() as u32)
}

unsafe fn tagged_to_key_string(tagged: *const TaggedVal) -> String {
    use crate::tagged::*;
    if tagged.is_null() {
        return "N".to_string();
    }
    let tag = (*tagged).tag;
    let payload = (*tagged).payload;
    match tag {
        TAG_NULL => "N".to_string(),
        TAG_BOOL => format!("b:{}", if payload != 0 { "true" } else { "false" }),
        TAG_INT32 => format!("i:{}", payload as i32),
        TAG_INT64 => format!("I:{}", payload as i64),
        TAG_UINT64 => format!("U:{}", payload),
        TAG_FLOAT32 => format!("f:{}", f32::from_bits(payload as u32)),
        TAG_FLOAT64 => format!("F:{}", f64::from_bits(payload)),
        TAG_STR => {
            let s = payload as *const LinString;
            if s.is_null() { return "s:".to_string(); }
            format!("s:{}", (*s).as_str())
        }
        TAG_ARRAY => {
            let arr = payload as *const crate::array::LinArray;
            if arr.is_null() { return "a:[]".to_string(); }
            let len = (*arr).len as usize;
            let elem_tag = (*arr).elem_tag;
            let mut parts = Vec::with_capacity(len);
            for i in 0..len {
                // Flat (unboxed) scalar arrays store raw values, not TaggedVal structs,
                // so decode per elem_tag exactly like array_to_json_string does. A tagged
                // array (elem_tag == 0xFF) recurses element-by-element.
                let part = match elem_tag {
                    0xFF => {
                        let elem = (*arr).data.add(i) as *const TaggedVal;
                        tagged_to_key_string(elem)
                    }
                    TAG_INT32 => format!("i:{}", *((*arr).data as *const i32).add(i)),
                    TAG_INT64 => format!("I:{}", *((*arr).data as *const i64).add(i)),
                    TAG_FLOAT32 => format!("f:{}", *((*arr).data as *const f32).add(i)),
                    TAG_FLOAT64 => format!("F:{}", *((*arr).data as *const f64).add(i)),
                    TAG_BOOL => format!("b:{}", if *((*arr).data as *const u8).add(i) != 0 { "true" } else { "false" }),
                    TAG_UINT8 => format!("i:{}", *((*arr).data as *const u8).add(i)),
                    TAG_INT8 => format!("i:{}", *((*arr).data as *const i8).add(i)),
                    TAG_UINT16 => format!("i:{}", *((*arr).data as *const u16).add(i)),
                    TAG_INT16 => format!("i:{}", *((*arr).data as *const i16).add(i)),
                    // u32 zero-extends to a positive Int64 (matches flat→tagged boxing).
                    TAG_UINT32 => format!("I:{}", *((*arr).data as *const u32).add(i) as u64),
                    TAG_UINT64 => format!("U:{}", *((*arr).data as *const u64).add(i)),
                    _ => "N".to_string(),
                };
                parts.push(part);
            }
            format!("a:[{}]", parts.join(","))
        }
        TAG_OBJECT => {
            let obj = payload as *const crate::object::LinObject;
            if obj.is_null() { return "o:{}".to_string(); }
            let len = (*obj).len as usize;
            let mut pairs: Vec<(String, String)> = Vec::with_capacity(len);
            for i in 0..len {
                let entry = (*obj).entries.add(i);
                let key_str = if (*entry).key.is_null() {
                    String::new()
                } else {
                    (*(*entry).key).as_str().to_string()
                };
                let val_str = tagged_to_key_string(&(*entry).value as *const TaggedVal);
                pairs.push((key_str, val_str));
            }
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            let inner: Vec<String> = pairs.into_iter().map(|(k, v)| format!("{}:{}", k, v)).collect();
            format!("o:{{{}}}", inner.join(","))
        }
        crate::tagged::TAG_FUNCTION => format!("fn:{:#x}", payload),
        _ => format!("?:{}", payload),
    }
}

/// Join an array of strings with a separator in a single allocation.
/// `arr` must be a LinArray* of LinString* elements (TAG_STR).
#[no_mangle]
pub unsafe extern "C" fn lin_string_join_arr(arr: *const crate::array::LinArray, separator: *const LinString) -> *mut LinString {
    use crate::array::lin_array_length;
    let n = lin_array_length(arr) as usize;
    if n == 0 {
        return lin_string_from_bytes(b"".as_ptr(), 0);
    }
    let sep = (*separator).as_str();
    let sep_len = sep.len();

    // Collect element string slices.
    // Elements in a String[] array have the LinString* stored in the payload field
    // (8 bytes after the tag byte). Read payload directly regardless of tag.
    let mut strs: Vec<&str> = Vec::with_capacity(n);
    for i in 0..n {
        let elem = (*arr).data.add(i);
        let s = (*elem).payload as *const LinString;
        if !s.is_null() {
            strs.push((*s).as_str());
        } else {
            strs.push("");
        }
    }

    // Compute total length in one pass.
    let total_len: usize = strs.iter().map(|s| s.len()).sum::<usize>()
        + sep_len * (n - 1);
    let result = lin_string_alloc(total_len as u32);
    let mut dst = (*result).data.as_mut_ptr();
    for (idx, s) in strs.iter().enumerate() {
        std::ptr::copy_nonoverlapping(s.as_ptr(), dst, s.len());
        dst = dst.add(s.len());
        if idx + 1 < n {
            std::ptr::copy_nonoverlapping(sep.as_ptr(), dst, sep_len);
            dst = dst.add(sep_len);
        }
    }
    result
}

/// Replace all occurrences of `pattern` in `s` with `replacement` in a single allocation.
#[no_mangle]
pub unsafe extern "C" fn lin_string_replace_all(
    s: *const LinString,
    pattern: *const LinString,
    replacement: *const LinString,
) -> *mut LinString {
    let src = (*s).as_str();
    let pat = (*pattern).as_str();
    let rep = (*replacement).as_str();
    if pat.is_empty() {
        return lin_string_from_bytes(src.as_ptr(), src.len() as u32);
    }
    let result = src.replace(pat, rep);
    lin_string_from_bytes(result.as_ptr(), result.len() as u32)
}

/// Convert a TaggedVal* to a string, dispatching on the runtime tag.
/// `tagged` may be null (treated as Null) or a pointer to a TaggedVal.
///
/// OWNERSHIP: returns an OWNED (+1) string the caller must release. Every numeric/null/bool/
/// array/object branch allocates a fresh +1 string, and codegen's `ToString` lowering registers
/// the result as owned and releases it at scope exit. The `TAG_STR` case must therefore RETAIN the
/// borrowed input string before returning it (rather than handing back a +0 alias), or the
/// caller's release over-decrements and double-frees the underlying buffer — the bug behind the
/// intermittent heap corruption when a fresh heap string (e.g. a `join` result) was stringified on
/// an assertion's fail path (`toString(value)` in std/test).
#[no_mangle]
pub unsafe extern "C" fn lin_tagged_to_string(tagged: *const TaggedVal) -> *mut LinString {
    if tagged.is_null() {
        return lin_null_to_string();
    }
    let tag = (*tagged).tag;
    let payload = (*tagged).payload;
    if tag == TAG_NULL {
        lin_null_to_string()
    } else if tag == TAG_BOOL {
        lin_bool_to_string(payload != 0)
    } else if tag == TAG_INT32 {
        lin_int_to_string(payload as i32 as i64)
    } else if tag == TAG_INT64 {
        lin_int_to_string(payload as i64)
    } else if tag == crate::tagged::TAG_UINT64 {
        lin_uint_to_string(payload)
    } else if tag == TAG_FLOAT32 {
        let f = f32::from_bits(payload as u32);
        lin_float_to_string(f as f64)
    } else if tag == TAG_FLOAT64 {
        let f = f64::from_bits(payload);
        lin_float_to_string(f)
    } else if tag == TAG_STR {
        // Return an OWNED reference: retain the borrowed input so the caller's release is
        // balanced (no-op for immortal literals). See the OWNERSHIP note above.
        let s = payload as *mut LinString;
        lin_string_inc_ref(s);
        s
    } else if tag == TAG_ARRAY {
        let arr = payload as *const crate::array::LinArray;
        lin_array_to_string(arr)
    } else if tag == TAG_OBJECT {
        let obj = payload as *const crate::object::LinObject;
        lin_object_to_string(obj)
    } else {
        lin_string_from_bytes(b"[object]".as_ptr(), 8)
    }
}
