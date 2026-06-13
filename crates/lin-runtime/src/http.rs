/// HTTP fetch intrinsics for compiled Lin programs.
use crate::fs::{make_string, resolve_lin_str};
use crate::tagged::{TAG_INT32, TAG_STR, TAG_OBJECT, alloc_tagged};
use crate::object::{lin_object_alloc, lin_object_set, tagged_as_object};
use crate::tagged::TaggedVal;

unsafe fn make_response_object(status: u16, body: &str) -> *mut u8 {
    let obj = lin_object_alloc(4);

    // status field (Int32)
    let status_key = make_string("status");
    let mut status_tv: TaggedVal = std::mem::zeroed();
    status_tv.tag = TAG_INT32;
    status_tv.payload = status as i32 as i64 as u64;
    lin_object_set(obj, status_key, &status_tv);

    // headers field (empty object)
    let headers_obj = lin_object_alloc(1);
    let headers_key = make_string("headers");
    let mut headers_tv: TaggedVal = std::mem::zeroed();
    headers_tv.tag = TAG_OBJECT;
    headers_tv.payload = headers_obj as u64;
    lin_object_set(obj, headers_key, &headers_tv);

    // body field (Str)
    let body_str = make_string(body);
    let body_key = make_string("body");
    let mut body_tv: TaggedVal = std::mem::zeroed();
    body_tv.tag = TAG_STR;
    body_tv.payload = body_str as u64;
    lin_object_set(obj, body_key, &body_tv);

    alloc_tagged(TAG_OBJECT, obj as u64)
}

unsafe fn make_error_object(msg: &str) -> *mut u8 {
    let obj = lin_object_alloc(2);

    let type_key = make_string("type");
    let type_val = make_string("error");
    let mut type_tv: TaggedVal = std::mem::zeroed();
    type_tv.tag = TAG_STR;
    type_tv.payload = type_val as u64;
    lin_object_set(obj, type_key, &type_tv);

    let msg_key = make_string("message");
    let msg_val = make_string(msg);
    let mut msg_tv: TaggedVal = std::mem::zeroed();
    msg_tv.tag = TAG_STR;
    msg_tv.payload = msg_val as u64;
    lin_object_set(obj, msg_key, &msg_tv);

    alloc_tagged(TAG_OBJECT, obj as u64)
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
