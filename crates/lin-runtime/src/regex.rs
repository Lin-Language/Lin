//! `std/regex` runtime support: bind the Rust `regex` crate (RE2-style, linear time, no
//! backtracking, no ReDoS) behind a thin set of `lin_regex_*` intrinsics.
//!
//! ## Handle model
//! A compiled pattern is an opaque, **program-lifetime immortal** handle, exactly like a
//! `Timer` (`std/time`): `compile` `Box::leak`s a `regex::Regex` and returns the raw pointer
//! as an `i64`. The handle is never freed — a regex compiled once and reused for the life of
//! the program is the overwhelmingly common case, and immortality removes the whole class of
//! cross-thread / use-after-free handle bugs (the regex is `Sync`, so the handle is freely
//! shareable across worker boundaries by value). The cost is one leaked `Regex` per distinct
//! pattern compiled, which is bounded and intended. A `0` handle is the "invalid pattern"
//! sentinel; `compile` returns the canonical `{type:error,message}` value in that case and
//! never hands back a `0` handle for a usable regex.
//!
//! ## Offsets
//! All offsets exposed to Lin are **codepoint** offsets (Lin strings are codepoint-aware).
//! The `regex` crate works in UTF-8 byte offsets internally; we translate byte→codepoint on
//! the way out. For pure-ASCII text the two coincide and the translation is effectively free.

use crate::array::{lin_array_alloc, lin_array_push_tagged};
use crate::fs::{make_error_tagged, make_string, resolve_lin_str};
use crate::map::{lin_map_alloc, lin_map_set, lin_map_release};
use crate::object::{lin_object_alloc, lin_object_set, LinObject};
use crate::string::{lin_string_release, LinString};
use crate::tagged::{alloc_tagged, TaggedVal, TAG_ARRAY, TAG_INT32, TAG_MAP, TAG_NULL, TAG_OBJECT, TAG_STR};

use regex::Regex;

/// Recover the leaked `&'static Regex` from a boxed handle. The handle reaches us as a
/// `Json`-typed argument: a `TaggedVal*(Int64)` whose payload is the leaked `regex::Regex`
/// pointer. (`Regex` is aliased to `Json` on the Lin side rather than `Int64`, because an
/// `X | Error` union with a *scalar* `X` hits an `is Error` narrowing/codegen gap — see the
/// module's design notes; a boxed `Json` member narrows correctly.) Returns `None` for a null
/// pointer or a `0` payload (the invalid-pattern sentinel) so callers degenerate gracefully.
unsafe fn handle_to_regex(handle: *const u8) -> Option<&'static Regex> {
    if handle.is_null() {
        return None;
    }
    let tv = handle as *const TaggedVal;
    let payload = (*tv).payload as usize;
    if payload == 0 {
        None
    } else {
        Some(&*(payload as *const Regex))
    }
}

/// Build a tagged `String` value (`TaggedVal*(Str)`) owning a fresh `LinString`.
unsafe fn tagged_str(s: &str) -> TaggedVal {
    let mut tv: TaggedVal = std::mem::zeroed();
    tv.tag = TAG_STR;
    tv.payload = make_string(s) as u64;
    tv
}

/// Map a byte offset within `s` to a codepoint offset. O(byte_off); short-circuits to the
/// byte offset itself for the all-ASCII prefix (each ASCII byte is one codepoint), which is
/// the common case.
fn byte_to_cp(s: &str, byte_off: usize) -> i32 {
    if s.is_ascii() {
        return byte_off as i32;
    }
    s[..byte_off].chars().count() as i32
}

/// Build a `Match` object as a +1-owned `*mut LinObject` from a `regex::Captures`.
/// Shape (per the proposal):
///   { text:String, start:Int32, end:Int32, groups:(String|Null)[], named:AnyVal }
/// `groups[0]` is the whole match; a non-participating positional group is a genuine `Null`
/// hole. `named` is a LinMap (TAG_MAP) carrying only the named groups that matched —
/// an absent named group reads back as `Null` via ordinary map indexing. `named` is stored
/// as AnyVal in the Match type so the consumer uses the union tag-dispatch path (TAG_MAP-safe).
///
/// Returns the bare object pointer (NOT a tagged wrapper): `find` boxes it once with
/// `alloc_tagged`, while `find_all` pushes it via a stack `TaggedVal` (transfer semantics),
/// so neither path leaks a 16-byte wrapper shell.
///
/// NOTE: the outer Match is TAG_OBJECT (not TAG_MAP) because the Lin `Match` type is a
/// named record with statically-known fields, and the compiled accessor for a narrowed
/// `m: Match` parameter calls `lin_object_get` directly (no tag dispatch). Only the `named`
/// sub-value is typed `AnyVal` at the Lin level and therefore safely upgraded to TAG_MAP.
unsafe fn build_match(s: &str, re: &Regex, caps: &regex::Captures) -> *mut LinObject {
    let whole = caps.get(0).expect("captures always have group 0");
    let obj = lin_object_alloc(8);

    // text
    let k_text = make_string("text");
    let tv_text = tagged_str(whole.as_str());
    lin_object_set(obj, k_text, &tv_text);
    lin_string_release(k_text);
    lin_string_release(tv_text.payload as *mut LinString);

    // start / end (codepoint offsets)
    let start_cp = byte_to_cp(s, whole.start());
    let end_cp = byte_to_cp(s, whole.end());

    let k_start = make_string("start");
    let mut tv_start: TaggedVal = std::mem::zeroed();
    tv_start.tag = TAG_INT32;
    tv_start.payload = start_cp as i64 as u64;
    lin_object_set(obj, k_start, &tv_start);
    lin_string_release(k_start);

    let k_end = make_string("end");
    let mut tv_end: TaggedVal = std::mem::zeroed();
    tv_end.tag = TAG_INT32;
    tv_end.payload = end_cp as i64 as u64;
    lin_object_set(obj, k_end, &tv_end);
    lin_string_release(k_end);

    // groups: (String | Null)[], one entry per capture group (index 0 = whole match).
    let ngroups = re.captures_len();
    let groups = lin_array_alloc(ngroups.max(1) as u64);
    for i in 0..ngroups {
        match caps.get(i) {
            Some(g) => {
                // lin_array_push_tagged TRANSFERS ownership of the inner heap value into the
                // array slot (no retain) — do NOT release the fresh LinString afterwards.
                let tv = tagged_str(g.as_str());
                lin_array_push_tagged(groups, &tv as *const TaggedVal as *const u8);
            }
            None => {
                let mut tv: TaggedVal = std::mem::zeroed();
                tv.tag = TAG_NULL;
                tv.payload = 0;
                lin_array_push_tagged(groups, &tv as *const TaggedVal as *const u8);
            }
        }
    }
    let k_groups = make_string("groups");
    let mut tv_groups: TaggedVal = std::mem::zeroed();
    tv_groups.tag = TAG_ARRAY;
    tv_groups.payload = groups as u64;
    lin_object_set(obj, k_groups, &tv_groups);
    lin_string_release(k_groups);
    // lin_object_set retains the array payload; drop our local +1 from lin_array_alloc.
    crate::array::lin_array_release(groups);

    // named: { String: String } as LinMap (TAG_MAP) — only the groups that participated.
    // Safe to use TAG_MAP here because `named` is typed `AnyVal` in the Lin Match record,
    // so the consumer uses the full union tag-dispatch path (handles TAG_MAP correctly).
    let named = lin_map_alloc(4);
    for name in re.capture_names().flatten() {
        if let Some(g) = caps.name(name) {
            let k = make_string(name);
            let tv = tagged_str(g.as_str());
            lin_map_set(named, k, &tv);
            lin_string_release(k);
            lin_string_release(tv.payload as *mut LinString);
        }
    }
    let k_named = make_string("named");
    let mut tv_named: TaggedVal = std::mem::zeroed();
    tv_named.tag = TAG_MAP;
    tv_named.payload = named as u64;
    lin_object_set(obj, k_named, &tv_named);
    lin_string_release(k_named);
    // lin_object_set retains the named map; drop our local +1 from lin_map_alloc.
    lin_map_release(named);

    obj
}

/// Compile a pattern. On success returns a boxed `Int64` handle (an i64 pointer to a leaked
/// `regex::Regex`). On an invalid pattern returns the canonical `{type:error,message}` value.
///
/// Declared on the Lin side as `=> Json` and re-annotated to `Regex | Error` in the wrapper:
/// the success value is a boxed Int64 (the handle), the failure value a tagged Object.
#[no_mangle]
pub unsafe extern "C" fn lin_regex_compile(pattern: *const u8) -> *mut u8 {
    let pat = match resolve_lin_str(pattern) {
        Some(s) => s,
        None => return make_error_tagged("regex: invalid pattern string"),
    };
    match Regex::new(&pat) {
        Ok(re) => {
            let leaked: &'static Regex = Box::leak(Box::new(re));
            let handle = leaked as *const Regex as usize as i64;
            crate::tagged::lin_box_int64(handle)
        }
        Err(e) => make_error_tagged(&format!("regex: {e}")),
    }
}

/// True if `re` matches anywhere in `s`. Returns a `u8` boolean.
#[no_mangle]
pub unsafe extern "C" fn lin_regex_is_match(handle: *const u8, s: *const u8) -> u8 {
    let re = match handle_to_regex(handle) {
        Some(r) => r,
        None => return 0,
    };
    let hay = match resolve_lin_str(s) {
        Some(s) => s,
        None => return 0,
    };
    re.is_match(&hay) as u8
}

/// First (leftmost) match as a `Match` object, or `Null` if none. Declared `=> Json`,
/// re-annotated `Match | Null` in the wrapper.
#[no_mangle]
pub unsafe extern "C" fn lin_regex_find(handle: *const u8, s: *const u8) -> *mut u8 {
    let re = match handle_to_regex(handle) {
        Some(r) => r,
        None => return std::ptr::null_mut(),
    };
    let hay = match resolve_lin_str(s) {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    match re.captures(&hay) {
        Some(caps) => alloc_tagged(TAG_OBJECT, build_match(&hay, re, &caps) as u64),
        None => std::ptr::null_mut(),
    }
}

/// All non-overlapping matches left-to-right as a `Match[]` (empty array if none).
#[no_mangle]
pub unsafe extern "C" fn lin_regex_find_all(handle: *const u8, s: *const u8) -> *mut u8 {
    let arr = lin_array_alloc(8);
    let re = match handle_to_regex(handle) {
        Some(r) => r,
        None => return alloc_tagged(TAG_ARRAY, arr as u64),
    };
    let hay = match resolve_lin_str(s) {
        Some(s) => s,
        None => return alloc_tagged(TAG_ARRAY, arr as u64),
    };
    for caps in re.captures_iter(&hay) {
        // build_match returns a +1-owned object. Push it via a STACK TaggedVal:
        // lin_array_push_tagged copies the 16 bytes inline and TRANSFERS ownership of the
        // object into the array slot (no retain, no wrapper to free) — the array_release at
        // end of life will release the object.
        let obj = build_match(&hay, re, &caps);
        let mut tv: TaggedVal = std::mem::zeroed();
        tv.tag = TAG_OBJECT;
        tv.payload = obj as u64;
        lin_array_push_tagged(arr, &tv as *const TaggedVal as *const u8);
    }
    alloc_tagged(TAG_ARRAY, arr as u64)
}

/// Replace the first match (`all == 0`) or every match (`all != 0`) of `re` in `s` with
/// `replacement`, applying `$1`/`${name}`/`$$` substitution (the `regex` crate's native
/// `Replacer` syntax). Returns a bare `LinString*` (the `=> String` foreign ABI contract).
#[no_mangle]
pub unsafe extern "C" fn lin_regex_replace(
    handle: *const u8,
    s: *const u8,
    replacement: *const u8,
    all: u8,
) -> *mut LinString {
    let hay = resolve_lin_str(s).unwrap_or_default();
    let rep = resolve_lin_str(replacement).unwrap_or_default();
    let re = match handle_to_regex(handle) {
        Some(r) => r,
        None => return make_string(&hay),
    };
    let out: std::borrow::Cow<str> = if all != 0 {
        re.replace_all(&hay, rep.as_str())
    } else {
        re.replace(&hay, rep.as_str())
    };
    make_string(&out)
}

/// Split `s` around every non-overlapping match of `re`. Returns a `String[]`; if the pattern
/// never matches the result is the single-element array `[s]`.
#[no_mangle]
pub unsafe extern "C" fn lin_regex_split(handle: *const u8, s: *const u8) -> *mut u8 {
    let hay = resolve_lin_str(s).unwrap_or_default();
    let arr = lin_array_alloc(8);
    let re = match handle_to_regex(handle) {
        Some(r) => r,
        None => {
            // lin_array_push_tagged TRANSFERS the fresh LinString into the slot — no release.
            let tv = tagged_str(&hay);
            lin_array_push_tagged(arr, &tv as *const TaggedVal as *const u8);
            return alloc_tagged(TAG_ARRAY, arr as u64);
        }
    };
    for piece in re.split(&hay) {
        let tv = tagged_str(piece);
        lin_array_push_tagged(arr, &tv as *const TaggedVal as *const u8);
    }
    alloc_tagged(TAG_ARRAY, arr as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::lin_object_get;
    use crate::string::lin_string_from_bytes;
    use crate::tagged::{lin_tagged_release, TAG_INT32};

    unsafe fn lin_str(s: &str) -> *mut u8 {
        alloc_tagged(
            TAG_STR,
            lin_string_from_bytes(s.as_ptr(), s.len() as u32) as u64,
        )
    }

    unsafe fn tv(p: *mut u8) -> *const TaggedVal {
        p as *const TaggedVal
    }

    unsafe fn obj_str(obj: *const LinObject, key: &str) -> String {
        let k = make_string(key);
        let tv = lin_object_get(obj, k);
        lin_string_release(k);
        assert!(!tv.is_null(), "field {key} missing");
        assert_eq!((*tv).tag, TAG_STR, "field {key} not a string");
        let ls = (*tv).payload as *const LinString;
        let slice = std::slice::from_raw_parts((*ls).data.as_ptr(), (*ls).len as usize);
        std::str::from_utf8(slice).unwrap().to_owned()
    }

    unsafe fn obj_i32(obj: *const LinObject, key: &str) -> i32 {
        let k = make_string(key);
        let tv = lin_object_get(obj, k);
        lin_string_release(k);
        assert!(!tv.is_null(), "field {key} missing");
        assert_eq!((*tv).tag, TAG_INT32);
        (*tv).payload as i32
    }

    #[test]
    fn compile_ok_and_bad() {
        unsafe {
            let h = lin_regex_compile(lin_str("[0-9]+"));
            assert_eq!((*tv(h)).tag, crate::tagged::TAG_INT64);
            assert_ne!((*tv(h)).payload, 0);

            let bad = lin_regex_compile(lin_str("(unbalanced"));
            assert_eq!((*tv(bad)).tag, TAG_MAP);
            // backreferences rejected
            let br = lin_regex_compile(lin_str(r"(\w+)\1"));
            assert_eq!((*tv(br)).tag, TAG_MAP);
        }
    }

    #[test]
    fn is_match_and_find_offsets() {
        unsafe {
            let h = lin_regex_compile(lin_str("[a-z]+"));
            assert_eq!(lin_regex_is_match(h, lin_str("  hello world")), 1);
            assert_eq!(lin_regex_is_match(h, lin_str("123")), 0);

            let m = lin_regex_find(h, lin_str("  hello world"));
            assert_eq!((*tv(m)).tag, TAG_OBJECT);
            let obj = (*tv(m)).payload as *const LinObject;
            assert_eq!(obj_str(obj, "text"), "hello");
            assert_eq!(obj_i32(obj, "start"), 2);
            assert_eq!(obj_i32(obj, "end"), 7);
            lin_tagged_release(m);
        }
    }

    #[test]
    fn codepoint_offsets_multibyte() {
        unsafe {
            let h = lin_regex_compile(lin_str("café"));
            // "a café here": 'café' starts at codepoint 2 (bytes 2), ends at codepoint 6.
            let m = lin_regex_find(h, lin_str("a café here"));
            let obj = (*tv(m)).payload as *const LinObject;
            assert_eq!(obj_str(obj, "text"), "café");
            assert_eq!(obj_i32(obj, "start"), 2);
            assert_eq!(obj_i32(obj, "end"), 6);
            lin_tagged_release(m);

            // An emoji before the match shifts codepoint vs byte offsets apart.
            let h2 = lin_regex_compile(lin_str("end"));
            let m2 = lin_regex_find(h2, lin_str("🎉 the end"));
            let obj2 = (*tv(m2)).payload as *const LinObject;
            // "🎉 the " = codepoints: 🎉(1) space(2) t(3)h(4)e(5) space(6) -> "end" at cp 6.
            assert_eq!(obj_i32(obj2, "start"), 6);
            assert_eq!(obj_i32(obj2, "end"), 9);
            lin_tagged_release(m2);
        }
    }

    #[test]
    fn replace_and_split() {
        unsafe {
            let h = lin_regex_compile(lin_str(r"\s+"));
            let r1 = lin_regex_replace(h, lin_str("a   b   c"), lin_str("_"), 0);
            let slice = std::slice::from_raw_parts((*r1).data.as_ptr(), (*r1).len as usize);
            assert_eq!(std::str::from_utf8(slice).unwrap(), "a_b   c");
            lin_string_release(r1);

            let split = lin_regex_split(h, lin_str("the   quick\tbrown"));
            assert_eq!((*tv(split)).tag, TAG_ARRAY);
            lin_tagged_release(split);

            // named-capture substitution
            let hd = lin_regex_compile(lin_str(r"(?P<y>\d{4})-(?P<m>\d{2})-(?P<d>\d{2})"));
            let rd = lin_regex_replace(hd, lin_str("2026-06-07"), lin_str("${d}/${m}/${y}"), 1);
            let sl = std::slice::from_raw_parts((*rd).data.as_ptr(), (*rd).len as usize);
            assert_eq!(std::str::from_utf8(sl).unwrap(), "07/06/2026");
            lin_string_release(rd);
        }
    }
}
