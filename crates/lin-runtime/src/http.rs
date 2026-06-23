/// HTTP fetch intrinsics for compiled Lin programs.
use crate::fs::{make_string, resolve_lin_str};
use crate::map::{lin_map_alloc, lin_map_set};
use crate::string::lin_string_release;
use crate::tagged::{TAG_INT32, TAG_STR, TAG_MAP, alloc_tagged};
use crate::tagged::TaggedVal;

unsafe fn map_set_str(map: *mut crate::map::LinMap, key: &str, val: &str) {
    let k = make_string(key);
    let v = make_string(val);
    let mut tv: TaggedVal = std::mem::zeroed();
    tv.tag = TAG_STR;
    tv.payload = v as u64;
    lin_map_set(map, k, &tv);
    lin_string_release(k);
    lin_string_release(v);
}

unsafe fn make_response_object(status: u16, body: &str) -> *mut u8 {
    let map = lin_map_alloc(4, 0);

    // status field (Int32)
    let k = make_string("status");
    let mut tv: TaggedVal = std::mem::zeroed();
    tv.tag = TAG_INT32;
    tv.payload = status as i32 as i64 as u64;
    lin_map_set(map, k, &tv);
    lin_string_release(k);

    // headers field (empty LinMap — typed as { String: String })
    let headers_map = lin_map_alloc(1, 0);
    let k = make_string("headers");
    let mut tv: TaggedVal = std::mem::zeroed();
    tv.tag = TAG_MAP;
    tv.payload = headers_map as u64;
    lin_map_set(map, k, &tv);
    lin_string_release(k);

    // body field (Str)
    map_set_str(map, "body", body);

    alloc_tagged(TAG_MAP, map as u64)
}

unsafe fn make_error_object(msg: &str) -> *mut u8 {
    crate::fs::make_error_tagged(msg)
}

/// HTTP GET fetch. url is a LinString* or TaggedVal*(Str).
/// Returns a TaggedVal*(Object) with { status: Int32, headers: Object, body: Str }.
/// On network error returns { type: "error", message: Str }.
#[no_mangle]
pub unsafe extern "C" fn lin_http_fetch(url: *const u8) -> *mut u8 {
    let url_str = match resolve_lin_str(url) {
        Some(s) => s,
        None => return make_error_object("invalid URL"),
    };
    match ureq::get(&url_str).call() {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.into_string().unwrap_or_default();
            make_response_object(status, &body)
        }
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            make_response_object(code, &body)
        }
        Err(e) => make_error_object(&e.to_string()),
    }
}

/// HTTP fetch with options. url is LinString* or TaggedVal*(Str).
/// opts is a TaggedVal*(Object) with optional fields: method (Str), body (Str), headers (Object).
/// Returns same as lin_http_fetch.
#[no_mangle]
pub unsafe extern "C" fn lin_http_fetch_with(url: *const u8, opts: *const u8) -> *mut u8 {
    let url_str = match resolve_lin_str(url) {
        Some(s) => s,
        None => return make_error_object("invalid URL"),
    };

    // Read opts fields from the options object via descriptor-walk (no intermediate LinMap).
    // TAG_MAP: use lin_map_get_bytes. TAG_RECORD: use lin_record_get_field (box_field_value walk).
    // Helper: read a TAG_STR field from opts by name. Returns owned box (must lin_tagged_release)
    // or null.
    unsafe fn opts_get_str_field(
        tv: *const TaggedVal,
        key_bytes: &[u8],
    ) -> *mut u8 {
        use crate::tagged::{TAG_MAP, TAG_RECORD, TAG_STR};
        if tv.is_null() { return std::ptr::null_mut(); }
        match (*tv).tag {
            TAG_MAP => {
                let map = (*tv).payload as *const crate::map::LinMap;
                if map.is_null() { return std::ptr::null_mut(); }
                let got = crate::map::lin_map_get_bytes(map, key_bytes.as_ptr(), key_bytes.len() as u32);
                if got.is_null() || (*got).tag != TAG_STR { return std::ptr::null_mut(); }
                // Borrow-intern: retain the string and build an owned box so caller can release uniformly.
                let s = (*got).payload as *mut u8;
                crate::memory::lin_rc_retain(s as *mut u32);
                crate::tagged::alloc_tagged(TAG_STR, s as u64)
            }
            TAG_RECORD => {
                let sealed = (*tv).payload as *const u8;
                if sealed.is_null() { return std::ptr::null_mut(); }
                let k = crate::string::lin_string_literal(key_bytes.as_ptr(), key_bytes.len() as u32);
                let boxed = crate::sealed::lin_record_get_field(sealed, k);
                if boxed.is_null() { return std::ptr::null_mut(); }
                if (*(boxed as *const TaggedVal)).tag != TAG_STR {
                    crate::tagged::lin_tagged_release(boxed);
                    return std::ptr::null_mut();
                }
                boxed
            }
            _ => std::ptr::null_mut(),
        }
    }

    let opts_tv: *const TaggedVal = if opts.is_null() { std::ptr::null() } else { opts as *const TaggedVal };

    let method = {
        let boxed = opts_get_str_field(opts_tv, b"method");
        if boxed.is_null() {
            "GET".to_string()
        } else {
            let vs = (*(boxed as *const TaggedVal)).payload as *const crate::string::LinString;
            let vs_slice = std::slice::from_raw_parts((*vs).data.as_ptr(), (*vs).len as usize);
            let s = std::str::from_utf8(vs_slice).unwrap_or("GET").to_uppercase();
            crate::tagged::lin_tagged_release(boxed);
            s
        }
    };

    let body_str: Option<String> = {
        let boxed = opts_get_str_field(opts_tv, b"body");
        if boxed.is_null() {
            None
        } else {
            let vs = (*(boxed as *const TaggedVal)).payload as *const crate::string::LinString;
            let vs_slice = std::slice::from_raw_parts((*vs).data.as_ptr(), (*vs).len as usize);
            let s = std::str::from_utf8(vs_slice).ok().map(|x| x.to_string());
            crate::tagged::lin_tagged_release(boxed);
            s
        }
    };

    let req = ureq::request(&method, &url_str);
    let result = if let Some(b) = body_str {
        req.send_string(&b)
    } else {
        req.call()
    };

    match result {
        Ok(resp) => {
            let status = resp.status();
            let body = resp.into_string().unwrap_or_default();
            make_response_object(status, &body)
        }
        Err(ureq::Error::Status(code, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            make_response_object(code, &body)
        }
        Err(e) => make_error_object(&e.to_string()),
    }
}
