/// HTTP fetch intrinsics for compiled Lin programs.
use crate::fs::{make_string, resolve_lin_str};
use crate::map::{lin_map_alloc, lin_map_set};
use crate::string::lin_string_release;
use crate::tagged::{TAG_INT32, TAG_STR, TAG_MAP, TAG_RECORD, alloc_tagged};
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

    // Read opts fields from the options object. Phase 3: an open object is TAG_MAP; a typed
    // (sealed) options record is TAG_RECORD and must be materialised to a LinMap first, otherwise
    // method/body would be silently dropped. `opts_map_owned` = we allocated the map and must
    // release it before returning.
    let (opts_map, opts_map_owned): (*const crate::map::LinMap, bool) = if opts.is_null() {
        (std::ptr::null(), false)
    } else {
        let tv = opts as *const TaggedVal;
        match (*tv).tag {
            TAG_MAP => ((*tv).payload as *const crate::map::LinMap, false),
            TAG_RECORD => {
                let sealed = (*tv).payload as *mut u8;
                if sealed.is_null() {
                    (std::ptr::null(), false)
                } else {
                    let named_desc = *((sealed.add(16)) as *const *const u8);
                    let map = crate::sealed::materialize_sealed_to_map_pub(sealed, named_desc);
                    (map as *const crate::map::LinMap, !map.is_null())
                }
            }
            _ => (std::ptr::null(), false),
        }
    };

    let method = if opts_map.is_null() {
        "GET".to_string()
    } else {
        let tv = crate::map::lin_map_get_bytes(opts_map, b"method".as_ptr(), 6);
        if tv.is_null() || (*tv).tag != TAG_STR {
            "GET".to_string()
        } else {
            let vs = (*tv).payload as *const crate::string::LinString;
            let vs_slice = std::slice::from_raw_parts((*vs).data.as_ptr(), (*vs).len as usize);
            std::str::from_utf8(vs_slice).unwrap_or("GET").to_uppercase()
        }
    };

    let body_str: Option<String> = if opts_map.is_null() {
        None
    } else {
        let tv = crate::map::lin_map_get_bytes(opts_map, b"body".as_ptr(), 4);
        if tv.is_null() || (*tv).tag != TAG_STR {
            None
        } else {
            let vs = (*tv).payload as *const crate::string::LinString;
            let vs_slice = std::slice::from_raw_parts((*vs).data.as_ptr(), (*vs).len as usize);
            std::str::from_utf8(vs_slice).ok().map(|s| s.to_string())
        }
    };

    // All fields read out; release the materialised options map if we own it.
    if opts_map_owned {
        crate::map::lin_map_release(opts_map as *mut crate::map::LinMap);
    }

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
