/// HTTP fetch intrinsics for compiled Lin programs.
use crate::fs::{make_string, resolve_lin_str};
use crate::map::{lin_map_alloc, lin_map_set};
use crate::string::lin_string_release;
use crate::tagged::{TAG_INT32, TAG_STR, TAG_MAP, alloc_tagged};
use crate::object::tagged_as_object;
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
    let map = lin_map_alloc(4);

    // status field (Int32)
    let k = make_string("status");
    let mut tv: TaggedVal = std::mem::zeroed();
    tv.tag = TAG_INT32;
    tv.payload = status as i32 as i64 as u64;
    lin_map_set(map, k, &tv);
    lin_string_release(k);

    // headers field (empty LinMap — typed as { String: String })
    let headers_map = lin_map_alloc(1);
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

    // Normalize opts to a LinObject* (handles both TAG_OBJECT and TAG_RECORD).
    let tv = if opts.is_null() { std::ptr::null() } else { opts as *const TaggedVal };
    let (opts_obj, opts_owned) = match tagged_as_object(tv) {
        Some(pair) => (pair.0, pair.1),
        None => (std::ptr::null(), false),
    };

    let method = if opts_obj.is_null() {
        "GET".to_string()
    } else {
        let obj = opts_obj;
        let method_key = "method";
        let mut found = "GET".to_string();
        let len = (*obj).len as usize;
        for i in 0..len {
            let entry = (*obj).entries.add(i);
            let key_s = (*entry).key;
            let slice = std::slice::from_raw_parts((*key_s).data.as_ptr(), (*key_s).len as usize);
            if let Ok(k) = std::str::from_utf8(slice) {
                if k == method_key {
                    let val_tv = &(*entry).value;
                    if val_tv.tag == TAG_STR {
                        let vs = val_tv.payload as *const crate::string::LinString;
                        let vs_slice = std::slice::from_raw_parts((*vs).data.as_ptr(), (*vs).len as usize);
                        if let Ok(s) = std::str::from_utf8(vs_slice) {
                            found = s.to_uppercase();
                        }
                    }
                    break;
                }
            }
        }
        found
    };

    let body_str: Option<String> = if opts_obj.is_null() {
        None
    } else {
        let obj = opts_obj;
        let len = (*obj).len as usize;
        let mut found = None;
        for i in 0..len {
            let entry = (*obj).entries.add(i);
            let key_s = (*entry).key;
            let slice = std::slice::from_raw_parts((*key_s).data.as_ptr(), (*key_s).len as usize);
            if let Ok(k) = std::str::from_utf8(slice) {
                if k == "body" {
                    let val_tv = &(*entry).value;
                    if val_tv.tag == TAG_STR {
                        let vs = val_tv.payload as *const crate::string::LinString;
                        let vs_slice = std::slice::from_raw_parts((*vs).data.as_ptr(), (*vs).len as usize);
                        if let Ok(s) = std::str::from_utf8(vs_slice) {
                            found = Some(s.to_string());
                        }
                    }
                    break;
                }
            }
        }
        found
    };

    let req = ureq::request(&method, &url_str);
    let result = if let Some(b) = body_str {
        req.send_string(&b)
    } else {
        req.call()
    };

    if opts_owned { crate::object::lin_object_release(opts_obj as *mut crate::object::LinObject); }

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
