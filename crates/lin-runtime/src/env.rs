use crate::string::{LinString, lin_string_from_bytes, lin_string_release};
use crate::map::{lin_map_alloc, lin_map_set};
use crate::tagged::{TaggedVal, TAG_STR, TAG_MAP, alloc_tagged};

unsafe fn make_lin_string(s: &str) -> *mut LinString {
    lin_string_from_bytes(s.as_ptr(), s.len() as u32)
}

/// Get an environment variable by name.
/// Returns a TaggedVal*(Str) if the variable is set, or null pointer if not found.
#[no_mangle]
pub unsafe extern "C" fn lin_env_get(name: *const LinString) -> *mut u8 {
    let slice = std::slice::from_raw_parts((*name).data.as_ptr(), (*name).len as usize);
    let st = match std::str::from_utf8(slice) {
        Ok(s) => s,
        Err(_) => return std::ptr::null_mut(),
    };
    match std::env::var(st) {
        Ok(val) => {
            let s = make_lin_string(&val);
            alloc_tagged(TAG_STR, s as u64)
        }
        Err(_) => std::ptr::null_mut(),
    }
}

/// Set an environment variable.
/// Returns null.
#[no_mangle]
pub unsafe extern "C" fn lin_env_set(name: *const LinString, value: *const LinString) {
    let name_slice = std::slice::from_raw_parts((*name).data.as_ptr(), (*name).len as usize);
    let val_slice = std::slice::from_raw_parts((*value).data.as_ptr(), (*value).len as usize);
    if let (Ok(n), Ok(v)) = (std::str::from_utf8(name_slice), std::str::from_utf8(val_slice)) {
        std::env::set_var(n, v);
    }
}

/// Unset (remove) an environment variable.
/// Returns null.
#[no_mangle]
pub unsafe extern "C" fn lin_env_unset(name: *const LinString) {
    let slice = std::slice::from_raw_parts((*name).data.as_ptr(), (*name).len as usize);
    if let Ok(st) = std::str::from_utf8(slice) {
        std::env::remove_var(st);
    }
}

/// Return all environment variables as a TaggedVal*(LinMap) (key→string value, { String: String }).
#[no_mangle]
pub unsafe extern "C" fn lin_env_environ() -> *mut u8 {
    let vars: Vec<(String, String)> = std::env::vars().collect();
    let cap = (vars.len().max(4)) as u32;
    let map = lin_map_alloc(cap);
    for (key, val) in &vars {
        let k = make_lin_string(key);
        let v = make_lin_string(val);
        let mut tv: TaggedVal = std::mem::zeroed();
        tv.tag = TAG_STR;
        tv.payload = v as u64;
        lin_map_set(map, k, &tv);
        lin_string_release(k);
        lin_string_release(v);
    }
    alloc_tagged(TAG_MAP, map as u64)
}
