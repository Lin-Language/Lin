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
/// SAFETY CONTRACT (sound under real OS-thread concurrency — async is genuine threading, ADR-027/043):
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
    // Negative indices count from the end of the string (Python-style).
    let start = if start < 0 { start + len } else { start };
    let end = if end < 0 { end + len } else { end };
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

/// Return the Unicode code point at CHAR index `index`. Returns -1 if OOB.
/// A negative `index` counts from the end (codepoint-wise): -1 is the last codepoint.
/// O(n): walks the UTF-8 string from the start (codepoint-correct). For O(1) ASCII/byte
/// scanning use `lin_string_byte_at` (exposed as `std/string.byteAt`).
#[no_mangle]
pub unsafe extern "C" fn lin_string_char_code(s: *const LinString, index: i32) -> i32 {
    let st = (*s).as_str();
    let idx = if index < 0 { index + st.chars().count() as i32 } else { index };
    if idx < 0 { return -1; }
    st.chars().nth(idx as usize).map(|c| c as i32).unwrap_or(-1)
}

/// Return the raw UTF-8 BYTE at byte index `index` (0..len), or -1 if OOB / negative.
/// O(1) — a direct indexed load from the string's byte buffer (same byte-index space as
/// `lin_string_char_at`). This is the primitive that makes Lin-side string scanning viable:
/// indexing `0..length(s)` with `byteAt` is O(n) total, whereas `charCode` (codepoint-indexed,
/// O(n) per call) makes the same loop O(n²). For pure-ASCII text `byteAt(s,i) == charCode(s,i)`.
/// Inlined in codegen (see codegen/intrinsics.rs) so the per-byte cost is a single load, not a
/// non-inlinable staticlib call — the same lever as the flat-array-read inlining (ADR-044).
#[no_mangle]
pub unsafe extern "C" fn lin_string_byte_at(s: *const LinString, index: i32) -> i32 {
    if s.is_null() || index < 0 || index >= (*s).len as i32 {
        return -1;
    }
    *(*s).data.as_ptr().add(index as usize) as i32
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

/// Capacity of `StackBuf`. Rust's `Display` for `f64` emits the FULL decimal expansion (not
/// scientific notation), so the longest `{}`-formatted `f64` is ~326 bytes (e.g. `f64::MIN` /
/// subnormals near `5e-324` render as `0.000…` with hundreds of fractional digits). 384 bytes
/// covers every finite `f64` and any `i64`/`{:.1}` form with slack; it must NEVER be exceeded
/// or output would silently truncate (a correctness bug, not just a perf miss).
const STACK_BUF_CAP: usize = 384;

/// A fixed-capacity stack buffer that implements `core::fmt::Write`, so `write!(buf, "{}", f)`
/// formats into it with no heap allocation. On the (impossible-for-the-numeric-inputs-here)
/// overflow path it silently stops appending; callers only feed integer/float formats that fit.
struct StackBuf {
    buf: [u8; STACK_BUF_CAP],
    len: usize,
}

impl StackBuf {
    #[inline]
    fn new() -> Self {
        StackBuf { buf: [0; STACK_BUF_CAP], len: 0 }
    }
    #[inline]
    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

impl std::fmt::Write for StackBuf {
    #[inline]
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let bytes = s.as_bytes();
        let end = self.len + bytes.len();
        if end <= self.buf.len() {
            self.buf[self.len..end].copy_from_slice(bytes);
            self.len = end;
        }
        Ok(())
    }
}

#[no_mangle]
pub extern "C" fn lin_float_to_string(f: f64) -> *mut LinString {
    use std::fmt::Write as _;
    let mut buf = StackBuf::new();
    if f.fract() == 0.0 && f.abs() < 1e15 {
        let _ = write!(buf, "{:.1}", f);
    } else {
        let _ = write!(buf, "{}", f);
    }
    let bytes = buf.as_bytes();
    unsafe { lin_string_from_bytes(bytes.as_ptr(), bytes.len() as u32) }
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

/// Append the JSON-escaped, double-quoted representation of `st` to `out`.
/// Single source of truth for JSON string escaping, shared by `lin_json_escape` (the
/// string-only entry point used by std/test) and `lin_to_json` (the recursive value
/// serializer) so the two can never drift. Produces a valid JSON string literal.
fn push_json_escaped(out: &mut String, st: &str) {
    out.push('"');
    for c in st.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Escape a string and wrap it in double quotes, producing a valid JSON string literal.
#[no_mangle]
pub unsafe extern "C" fn lin_json_escape(s: *const LinString) -> *mut LinString {
    let st = (*s).as_str();
    let mut out = String::with_capacity(st.len() + 2);
    push_json_escaped(&mut out, st);
    lin_string_from_bytes(out.as_ptr(), out.len() as u32)
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

/// Index of first occurrence of `needle` at or after byte index `from`. -1 if none.
/// Returns a BYTE offset (consistent with `lin_string_index_of`).
#[no_mangle]
pub unsafe extern "C" fn lin_string_index_of_from(s: *const LinString, needle: *const LinString, from: i32) -> i32 {
    let st = (*s).as_str();
    let nd = (*needle).as_str();
    let from = from.max(0) as usize;
    if from > st.len() { return -1; }
    match st[from..].find(nd) {
        Some(i) => (from + i) as i32,
        None => -1,
    }
}

/// Index of last occurrence of `needle` whose start is at or before byte index `before`. -1 if none.
/// Returns a CODEPOINT count (consistent with `lin_string_last_index_of`).
#[no_mangle]
pub unsafe extern "C" fn lin_string_last_index_of_from(s: *const LinString, needle: *const LinString, before: i32) -> i32 {
    let st = (*s).as_str();
    let nd = (*needle).as_str();
    if before < 0 { return -1; }
    // A match may START at <= before, so search the prefix ending at before + needle.len().
    let end = ((before as usize).saturating_add(nd.len())).min(st.len());
    match st[..end].rfind(nd) {
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

/// Recursively convert a TaggedVal to its (lossy, display) JSON string representation.
/// Used for toString(obj), toString(arr), and string interpolation of complex values.
///
/// This is the DISPLAY stringifier: unlike the strict-JSON `push_json_value`, string values
/// and object keys are emitted UNESCAPED, container separators are `", "` (comma-space), and
/// object entries are `"key": value`. The push_display_* helpers below append into one reused
/// String to avoid the former `Vec<String>`+`format!`-per-element+`join` (N+1 allocs).
pub unsafe fn tagged_to_json_string(tagged: *const TaggedVal) -> String {
    let mut out = String::new();
    push_display_value(&mut out, tagged);
    out
}

/// Display float formatting for a scalar (TAG_FLOAT32/64) box: plain `{}`, no `.1` handling.
/// (Kept distinct from the flat-array float path, which DOES apply `.1` — preserving the
/// pre-existing behavioural split between the two.)
fn push_display_float_plain(out: &mut String, f: f64) {
    use std::fmt::Write as _;
    let _ = write!(out, "{}", f);
}

/// Display float formatting for a flat scalar-array element: integral magnitudes < 1e15 render
/// with a trailing `.1` decimal; everything else plain `{}`.
fn push_display_float_dot1(out: &mut String, f: f64) {
    use std::fmt::Write as _;
    if f.fract() == 0.0 && f.abs() < 1e15 {
        let _ = write!(out, "{:.1}", f);
    } else {
        let _ = write!(out, "{}", f);
    }
}

unsafe fn push_display_value(out: &mut String, tagged: *const TaggedVal) {
    use std::fmt::Write as _;
    if tagged.is_null() {
        out.push_str("null");
        return;
    }
    let tag = (*tagged).tag;
    let payload = (*tagged).payload;
    if tag == TAG_NULL { out.push_str("null"); return; }
    if tag == TAG_BOOL { out.push_str(if payload != 0 { "true" } else { "false" }); return; }
    if tag == TAG_INT32 { let _ = write!(out, "{}", payload as i32); return; }
    if tag == TAG_INT64 { let _ = write!(out, "{}", payload as i64); return; }
    if tag == crate::tagged::TAG_UINT64 { let _ = write!(out, "{}", payload); return; }
    if tag == TAG_FLOAT32 {
        push_display_float_plain(out, f32::from_bits(payload as u32) as f64);
        return;
    }
    if tag == TAG_FLOAT64 {
        push_display_float_plain(out, f64::from_bits(payload));
        return;
    }
    if tag == TAG_STR {
        let s = payload as *const LinString;
        if s.is_null() { out.push_str("null"); return; }
        out.push('"');
        out.push_str((*s).as_str());
        out.push('"');
        return;
    }
    if tag == TAG_ARRAY {
        let arr = payload as *const crate::array::LinArray;
        if arr.is_null() { out.push_str("[]"); return; }
        push_display_array(out, arr);
        return;
    }
    if tag == TAG_OBJECT {
        let obj = payload as *const crate::object::LinObject;
        if obj.is_null() { out.push_str("{}"); return; }
        push_display_object(out, obj);
        return;
    }
    if tag == crate::tagged::TAG_SUMNODE {
        // KEEP-PACKED-THROUGH-RECORD-FIELDS boundary: a kept-packed `*SumNode` field reached the
        // (lossy display) object stringifier — materialize it to a real LinObject and serialize, then
        // release the transient. This makes `toString(record_with_sum_field)` correct (NOT `[object]`).
        let obj = crate::sumnode::lin_sumnode_materialize(payload as *mut u8);
        if obj.is_null() { out.push_str("[object]"); return; }
        push_display_object(out, obj as *const crate::object::LinObject);
        crate::object::lin_object_release(obj as *mut crate::object::LinObject);
        return;
    }
    out.push_str("[object]");
}

unsafe fn array_to_json_string(arr: *const crate::array::LinArray) -> String {
    let mut out = String::new();
    push_display_array(&mut out, arr);
    out
}

unsafe fn push_display_array(out: &mut String, arr: *const crate::array::LinArray) {
    use crate::tagged::*;
    use std::fmt::Write as _;
    let len = (*arr).len as usize;
    let elem_tag = (*arr).elem_tag;
    out.push('[');
    for i in 0..len {
        if i > 0 {
            out.push_str(", ");
        }
        match elem_tag {
            0xFF => {
                // Tagged array: elements are TaggedVal structs (16 bytes each).
                let elem = (*arr).data.add(i);
                push_display_value(out, elem as *const TaggedVal);
            }
            TAG_INT32 => { let _ = write!(out, "{}", *((*arr).data as *const i32).add(i)); }
            TAG_INT64 => { let _ = write!(out, "{}", *((*arr).data as *const i64).add(i)); }
            TAG_FLOAT32 => push_display_float_dot1(out, *((*arr).data as *const f32).add(i) as f64),
            TAG_FLOAT64 => push_display_float_dot1(out, *((*arr).data as *const f64).add(i)),
            TAG_BOOL => out.push_str(if *((*arr).data as *const u8).add(i) != 0 { "true" } else { "false" }),
            TAG_UINT8 => { let _ = write!(out, "{}", *((*arr).data as *const u8).add(i)); }
            TAG_INT8 => { let _ = write!(out, "{}", *((*arr).data as *const i8).add(i)); }
            TAG_UINT16 => { let _ = write!(out, "{}", *((*arr).data as *const u16).add(i)); }
            TAG_INT16 => { let _ = write!(out, "{}", *((*arr).data as *const i16).add(i)); }
            TAG_UINT32 => { let _ = write!(out, "{}", *((*arr).data as *const u32).add(i)); }
            TAG_UINT64 => { let _ = write!(out, "{}", *((*arr).data as *const u64).add(i)); }
            _ => out.push_str("null"),
        }
    }
    out.push(']');
}

unsafe fn object_to_json_string(obj: *const crate::object::LinObject) -> String {
    let mut out = String::new();
    push_display_object(&mut out, obj);
    out
}

unsafe fn push_display_object(out: &mut String, obj: *const crate::object::LinObject) {
    let len = (*obj).len as usize;
    out.push('{');
    for i in 0..len {
        if i > 0 {
            out.push_str(", ");
        }
        let entry = (*obj).entries.add(i);
        let key = (*entry).key;
        out.push('"');
        if key.is_null() { out.push_str("null"); } else { out.push_str((*key).as_str()); }
        out.push_str("\": ");
        push_display_value(out, &(*entry).value as *const TaggedVal);
    }
    out.push('}');
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
    } else if tag == crate::tagged::TAG_SUMNODE {
        // KEEP-PACKED-THROUGH-RECORD-FIELDS boundary: a kept-packed `*SumNode` that escaped a record
        // field into the type-erased dynamic domain. Materialize it to a real LinObject (via the
        // per-type materializer in its descriptor), stringify, and release the transient object.
        let obj = crate::sumnode::lin_sumnode_materialize(payload as *mut u8);
        if obj.is_null() {
            return lin_string_from_bytes(b"[object]".as_ptr(), 8);
        }
        let s = lin_object_to_string(obj as *const crate::object::LinObject);
        crate::object::lin_object_release(obj as *mut crate::object::LinObject);
        s
    } else if tag == crate::tagged::TAG_BIGNUM {
        // Opaque BigInt handle: render its exact base-10 form (so accidental interpolation shows
        // the value rather than `[object]`; the canonical entry point is still std/bignum.toString).
        crate::bignum::bignum_render(payload as *const u8)
    } else if tag == crate::tagged::TAG_DECIMAL {
        // Opaque Decimal handle: render its exact, scale-preserving base-10 form.
        crate::decimal::decimal_render(payload as *const u8)
    } else {
        lin_string_from_bytes(b"[object]".as_ptr(), 8)
    }
}

/// Recursively serialize a TaggedVal to STRICT, valid JSON, appending to `out`.
///
/// Differs from `tagged_to_json_string` (the lossy display stringifier above) in two ways
/// required for valid JSON: string VALUES and object KEYS are escaped via the same
/// `push_json_escaped` helper that backs `lin_json_escape`, and non-finite floats
/// (NaN/±Inf, which are not representable in JSON) are emitted as `null` — matching
/// JavaScript's `JSON.stringify`. This is a READ-ONLY walk: it borrows the value and never
/// retains/releases or frees anything.
unsafe fn push_json_value(out: &mut String, tagged: *const TaggedVal) {
    use crate::tagged::*;
    if tagged.is_null() {
        out.push_str("null");
        return;
    }
    let tag = (*tagged).tag;
    let payload = (*tagged).payload;
    match tag {
        TAG_NULL => out.push_str("null"),
        TAG_BOOL => out.push_str(if payload != 0 { "true" } else { "false" }),
        TAG_INT32 => out.push_str(&(payload as i32).to_string()),
        TAG_INT64 => out.push_str(&(payload as i64).to_string()),
        TAG_UINT64 => out.push_str(&payload.to_string()),
        TAG_FLOAT32 => push_json_float(out, f32::from_bits(payload as u32) as f64),
        TAG_FLOAT64 => push_json_float(out, f64::from_bits(payload)),
        TAG_STR => {
            let s = payload as *const LinString;
            if s.is_null() {
                out.push_str("null");
            } else {
                push_json_escaped(out, (*s).as_str());
            }
        }
        TAG_ARRAY => {
            let arr = payload as *const crate::array::LinArray;
            push_json_array(out, arr);
        }
        TAG_OBJECT => {
            let obj = payload as *const crate::object::LinObject;
            push_json_object(out, obj);
        }
        crate::tagged::TAG_SUMNODE => {
            // KEEP-PACKED-THROUGH-RECORD-FIELDS boundary: materialize the kept-packed `*SumNode` to a
            // real LinObject, serialize, release the transient object.
            let obj = crate::sumnode::lin_sumnode_materialize(payload as *mut u8);
            if obj.is_null() {
                out.push_str("null");
            } else {
                push_json_object(out, obj as *const crate::object::LinObject);
                crate::object::lin_object_release(obj as *mut crate::object::LinObject);
            }
        }
        // Functions/iterators and any unknown tag are not JSON values → null.
        _ => out.push_str("null"),
    }
}

/// Emit a finite float as a JSON number; non-finite (NaN/Inf) becomes `null` (matches
/// JSON.stringify, since JSON has no representation for them).
fn push_json_float(out: &mut String, f: f64) {
    use std::fmt::Write as _;
    if !f.is_finite() {
        out.push_str("null");
    } else if f.fract() == 0.0 && f.abs() < 1e15 {
        // Append directly into `out` — no intermediate String.
        let _ = write!(out, "{:.1}", f);
    } else {
        let _ = write!(out, "{}", f);
    }
}

unsafe fn push_json_array(out: &mut String, arr: *const crate::array::LinArray) {
    use crate::tagged::*;
    if arr.is_null() {
        out.push_str("[]");
        return;
    }
    let len = (*arr).len as usize;
    let elem_tag = (*arr).elem_tag;
    out.push('[');
    for i in 0..len {
        if i > 0 {
            out.push(',');
        }
        match elem_tag {
            0xFF => {
                // Tagged array: elements are TaggedVal structs.
                let elem = (*arr).data.add(i) as *const TaggedVal;
                push_json_value(out, elem);
            }
            TAG_INT32 => out.push_str(&(*((*arr).data as *const i32).add(i)).to_string()),
            TAG_INT64 => out.push_str(&(*((*arr).data as *const i64).add(i)).to_string()),
            TAG_FLOAT32 => push_json_float(out, *((*arr).data as *const f32).add(i) as f64),
            TAG_FLOAT64 => push_json_float(out, *((*arr).data as *const f64).add(i)),
            TAG_BOOL => out.push_str(if *((*arr).data as *const u8).add(i) != 0 { "true" } else { "false" }),
            TAG_UINT8 => out.push_str(&(*((*arr).data as *const u8).add(i)).to_string()),
            TAG_INT8 => out.push_str(&(*((*arr).data as *const i8).add(i)).to_string()),
            TAG_UINT16 => out.push_str(&(*((*arr).data as *const u16).add(i)).to_string()),
            TAG_INT16 => out.push_str(&(*((*arr).data as *const i16).add(i)).to_string()),
            TAG_UINT32 => out.push_str(&(*((*arr).data as *const u32).add(i)).to_string()),
            TAG_UINT64 => out.push_str(&(*((*arr).data as *const u64).add(i)).to_string()),
            _ => out.push_str("null"),
        }
    }
    out.push(']');
}

unsafe fn push_json_object(out: &mut String, obj: *const crate::object::LinObject) {
    if obj.is_null() {
        out.push_str("{}");
        return;
    }
    let len = (*obj).len as usize;
    out.push('{');
    for i in 0..len {
        if i > 0 {
            out.push(',');
        }
        let entry = (*obj).entries.add(i);
        let key = (*entry).key;
        // Keys are escaped+quoted exactly like string values (the lossy stringifier above
        // leaves them unescaped — strict JSON requires escaping).
        if key.is_null() {
            out.push_str("\"\"");
        } else {
            push_json_escaped(out, (*key).as_str());
        }
        out.push(':');
        push_json_value(out, &(*entry).value as *const TaggedVal);
    }
    out.push('}');
}

/// Decode a `UInt8[]` of UTF-8 bytes into a validated `String`. The inverse of `byteAt`:
/// `byteAt` turns a `String` into bytes one at a time; this turns the byte buffer back into a
/// `String`, validating that the bytes are well-formed UTF-8. Returns a boxed `TaggedVal*`:
///   * success → `TAG_STR` box wrapping a fresh +1 `LinString`
///   * invalid UTF-8 → `TAG_OBJECT` box wrapping the standard `{type:"error",message}` shape
/// This is why the foreign declaration is `=> Json` (a boxed tagged value, re-annotated to
/// `String | Error` in the `std/string.fromUtf8` wrapper): a bare `=> UInt8[]`/`=> String`
/// foreign return cannot carry the Error arm. `arr` may be a raw `LinArray*` or a
/// `TaggedVal*(Array)`; flat `UInt8`/`Int8` buffers are read straight from the data buffer,
/// other element shapes fall back to per-element boxing + truncation (mirrors
/// `lin_fs_write_file_bytes`). The returned box is independently owned by the caller (release
/// with `lin_tagged_release`).
#[no_mangle]
pub unsafe extern "C" fn lin_string_from_utf8(arr: *const u8) -> *mut u8 {
    use crate::array::{lin_array_get_tagged, lin_array_length, LinArray};
    use crate::fs::make_error_tagged;
    use crate::tagged::{alloc_tagged, TaggedVal, TAG_ARRAY, TAG_UINT8, TAG_INT8};
    if arr.is_null() {
        return make_error_tagged("fromUtf8: null byte array");
    }
    // arr may be a TaggedVal*(Array) or a raw LinArray* (see resolve patterns in fs.rs).
    let head = (arr as *const u64).read_unaligned();
    let lin_arr = if head == TAG_ARRAY as u64 {
        (*(arr as *const TaggedVal)).payload as *const LinArray
    } else {
        arr as *const LinArray
    };
    if lin_arr.is_null() {
        return make_error_tagged("fromUtf8: null byte array");
    }
    let len = lin_array_length(lin_arr) as usize;
    let mut bytes: Vec<u8> = Vec::with_capacity(len);
    let elem_tag = (*lin_arr).elem_tag;
    if elem_tag == TAG_UINT8 || elem_tag == TAG_INT8 {
        // Flat 1-byte buffer: copy the raw bytes directly.
        let data = (*lin_arr).data as *const u8;
        for i in 0..len {
            bytes.push(*data.add(i));
        }
    } else {
        // Tagged / wider-element arrays: box each element and truncate to a byte.
        for i in 0..len as i64 {
            let tv_ptr = lin_array_get_tagged(lin_arr, i);
            let v = if tv_ptr.is_null() {
                0u8
            } else {
                let payload = (*tv_ptr).payload;
                // Cached-box-safe free: flat-int reads may return an immutable cached static box.
                crate::tagged::lin_tagged_release(tv_ptr as *mut u8);
                payload as u8
            };
            bytes.push(v);
        }
    }
    match std::str::from_utf8(&bytes) {
        Ok(s) => alloc_tagged(TAG_STR, lin_string_from_bytes(s.as_ptr(), s.len() as u32) as u64),
        Err(_) => make_error_tagged("fromUtf8: invalid UTF-8 byte sequence"),
    }
}

/// Serialize ANY Lin value (passed as a boxed TaggedVal*) to a strict, valid JSON string.
///
/// OWNERSHIP: this BORROWS `tagged` (a read-only walk) and returns a freshly-allocated OWNED
/// (+1) LinString the caller must release. It never retains/releases the input and never frees
/// anything it did not allocate — so, unlike `lin_tagged_to_string`'s TAG_STR fast path, there
/// is no aliasing of the input string and thus no double-free risk on the input.
#[no_mangle]
pub unsafe extern "C" fn lin_to_json(tagged: *const TaggedVal) -> *mut LinString {
    let mut out = String::new();
    push_json_value(&mut out, tagged);
    lin_string_from_bytes(out.as_ptr(), out.len() as u32)
}
