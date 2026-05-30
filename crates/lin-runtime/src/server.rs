use crate::object::{lin_object_alloc, lin_object_set};
use crate::tagged::{TaggedVal, TAG_STR, TAG_OBJECT, alloc_tagged};
use crate::fs::{make_string, resolve_lin_str};

/// Match a URL path against a pattern with `:param` segments.
/// Returns TaggedVal*(Object) with captured params on match, or null on mismatch.
/// pattern and path may be bare LinString* or TaggedVal*(Str).
#[no_mangle]
pub unsafe extern "C" fn lin_server_path_match(
    pattern: *const u8,
    path: *const u8,
) -> *mut u8 {
    let pat_str = match resolve_lin_str(pattern) {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };

    let pat_parts: Vec<&str> = pat_str.split('/').collect();
    let path_parts: Vec<&str> = path_str.split('/').collect();

    if pat_parts.len() != path_parts.len() {
        return std::ptr::null_mut();
    }

    let obj = lin_object_alloc(4);
    for (pp, tp) in pat_parts.iter().zip(path_parts.iter()) {
        if let Some(param_name) = pp.strip_prefix(':') {
            let key = make_string(param_name);
            let val_str = make_string(tp);
            let mut tv: TaggedVal = std::mem::zeroed();
            tv.tag = TAG_STR;
            tv.payload = val_str as u64;
            lin_object_set(obj, key, &tv);
            // Note: lin_object_set takes ownership of key pointer; do NOT release key.
        } else if *pp != *tp {
            crate::object::lin_object_release(obj);
            return std::ptr::null_mut();
        }
    }

    alloc_tagged(TAG_OBJECT, obj as u64)
}
