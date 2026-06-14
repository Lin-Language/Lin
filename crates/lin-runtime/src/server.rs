use crate::map::{lin_map_alloc, lin_map_set, LinMap};
use crate::object::{LinObject, tagged_as_object};
use crate::tagged::{TaggedVal, TAG_STR, TAG_MAP, TAG_INT32, alloc_tagged};
use crate::fs::{make_string, resolve_lin_str, make_error_tagged};
use crate::string::{LinString, lin_string_release};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

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

    let map = lin_map_alloc(4, 0);
    for (pp, tp) in pat_parts.iter().zip(path_parts.iter()) {
        if let Some(param_name) = pp.strip_prefix(':') {
            let key = make_string(param_name);
            let val_str = make_string(tp);
            let mut tv: TaggedVal = std::mem::zeroed();
            tv.tag = TAG_STR;
            tv.payload = val_str as u64;
            lin_map_set(map, key, &tv);
            lin_string_release(key);
            lin_string_release(val_str);
        } else if *pp != *tp {
            crate::map::lin_map_release(map);
            return std::ptr::null_mut();
        }
    }

    alloc_tagged(TAG_MAP, map as u64)
}

// ---------------------------------------------------------------------------
// HTTP/1.1 server (`serve`, spec §25.5)
//
// A minimal, dependency-free HTTP/1.1 server. `lin_serve` binds a TCP listener,
// then serves connections SEQUENTIALLY (one request at a time): read + parse the
// request, build an HttpRequest object, invoke the Lin handler closure, read the
// returned HttpResponse object, and write it back on the wire.
//
// The handler closure is invoked through the same raw fn/env ABI the async/worker
// runtime uses (`extern "C-unwind"`, env-or-no-env), inside a fault-isolation
// boundary so a faulting handler yields a 500 rather than killing the server. The
// handler `env_ptr` lives for the whole server (single serving thread, sequential)
// and is NOT deep-copied or freed between requests — unlike async's per-spawn env.
// ---------------------------------------------------------------------------

/// A parsed HTTP request line + headers + body.
pub(crate) struct ParsedRequest {
    pub method: String,
    pub path: String,
    pub query: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// Parse a raw HTTP request (request line + headers + optional body) from `buf`.
/// `header_end` is the index of the `\r\n\r\n` that terminates the header block;
/// bytes after it are the body. Returns `None` on a malformed request line.
pub(crate) fn parse_http_request(buf: &[u8], header_end: usize) -> Option<ParsedRequest> {
    let head = std::str::from_utf8(&buf[..header_end]).ok()?;
    let mut lines = head.split("\r\n");

    // Request line: METHOD SP target SP HTTP/1.1
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    // HTTP version is parts.next(); ignored.

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };

    let mut headers: Vec<(String, String)> = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }

    // Body: whatever was read past the header terminator (the caller has already
    // ensured Content-Length bytes are present).
    let body_start = header_end + 4; // skip the "\r\n\r\n"
    let body = if body_start <= buf.len() {
        String::from_utf8_lossy(&buf[body_start..]).into_owned()
    } else {
        String::new()
    };

    Some(ParsedRequest { method, path, query, headers, body })
}

/// Locate the end of the header block (index of the start of `\r\n\r\n`) within `buf`.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Read a full HTTP request from `stream`: the header block, then the body per
/// `Content-Length`. Returns the raw bytes and the header-block end index, or
/// `None` on EOF/timeout/error before a complete header block arrived.
fn read_request(stream: &mut TcpStream) -> Option<(Vec<u8>, usize)> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];

    // Phase 1: read until we have the full header block.
    let header_end = loop {
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return None, // peer closed before headers completed
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => return None,
        }
        if buf.len() > 1024 * 1024 {
            return None; // header block too large; bail
        }
    };

    // Phase 2: read the body if Content-Length says so.
    let content_length = {
        let head = std::str::from_utf8(&buf[..header_end]).unwrap_or("");
        head.split("\r\n")
            .filter_map(|l| l.split_once(':'))
            .find(|(n, _)| n.trim().eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.trim().parse::<usize>().ok())
            .unwrap_or(0)
    };

    let body_start = header_end + 4;
    let want_total = body_start + content_length;
    while buf.len() < want_total {
        match stream.read(&mut chunk) {
            Ok(0) => break, // peer closed early; serve what we have
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => break,
        }
    }

    Some((buf, header_end))
}

/// Build an HttpRequest map `{ method, path, query, headers, body }` as an owned
/// `TaggedVal*(Map)`. Mirrors the `{ status, headers, body }` construction in `http.rs`.
unsafe fn build_request_object(req: &ParsedRequest) -> *mut u8 {
    let map = lin_map_alloc(5, 0);

    let set_str = |m: *mut LinMap, key: &str, val: &str| {
        let k = make_string(key);
        let v = make_string(val);
        let mut tv: TaggedVal = std::mem::zeroed();
        tv.tag = TAG_STR;
        tv.payload = v as u64;
        lin_map_set(m, k, &tv);
        lin_string_release(k);
        lin_string_release(v);
    };

    set_str(map, "method", &req.method);
    set_str(map, "path", &req.path);
    set_str(map, "query", &req.query);

    // headers: nested map of String -> String
    let headers_map = lin_map_alloc(req.headers.len().max(1) as u32, 0);
    for (name, value) in &req.headers {
        set_str(headers_map, name, value);
    }
    let headers_key = make_string("headers");
    let mut headers_tv: TaggedVal = std::mem::zeroed();
    headers_tv.tag = TAG_MAP;
    headers_tv.payload = headers_map as u64;
    lin_map_set(map, headers_key, &headers_tv);
    lin_string_release(headers_key);
    // lin_map_set retains headers_map; drop our local +1
    crate::map::lin_map_release(headers_map);

    set_str(map, "body", &req.body);

    alloc_tagged(TAG_MAP, map as u64)
}

/// Read a String field from an object payload by key, if present and TAG_STR.
unsafe fn obj_get_string(obj: *const LinObject, key: &str) -> Option<String> {
    let k = make_string(key);
    let tv = crate::object::lin_object_get(obj, k);
    lin_string_release(k);
    if tv.is_null() || (*tv).tag != TAG_STR {
        return None;
    }
    let s = (*tv).payload as *const LinString;
    let slice = std::slice::from_raw_parts((*s).data.as_ptr(), (*s).len as usize);
    std::str::from_utf8(slice).ok().map(|x| x.to_string())
}

/// Read an Int32 field from an object payload by key, if present and TAG_INT32.
unsafe fn obj_get_i32(obj: *const LinObject, key: &str) -> Option<i32> {
    let k = make_string(key);
    let tv = crate::object::lin_object_get(obj, k);
    lin_string_release(k);
    if tv.is_null() || (*tv).tag != TAG_INT32 {
        return None;
    }
    Some((*tv).payload as i32)
}

/// Serialize a Lin HttpResponse object (a `TaggedVal*(Object)`) into an HTTP/1.1
/// wire response. Reads `status` (Int32, default 200), `headers` (Object), and
/// `body` (Str). An error-shaped object (`{ "type": "error", ... }`) or a
/// missing/ill-typed value yields a 500.
unsafe fn serialize_response(resp: *const u8) -> Vec<u8> {
    if resp.is_null() {
        return wire_response(500, &[], "Internal Server Error");
    }
    let tv = resp as *const TaggedVal;
    let (obj, resp_owned) = match tagged_as_object(tv) {
        Some(pair) => pair,
        None => return wire_response(500, &[], "Internal Server Error"),
    };

    // Error-shaped result → 500.
    if let Some(t) = obj_get_string(obj, "type") {
        if t == "error" {
            let msg = obj_get_string(obj, "message").unwrap_or_else(|| "error".to_string());
            if resp_owned { crate::object::lin_object_release(obj as *mut LinObject); }
            return wire_response(500, &[], &msg);
        }
    }

    let status = obj_get_i32(obj, "status").unwrap_or(200);
    let body = obj_get_string(obj, "body").unwrap_or_default();

    // headers nested object → Vec<(name, value)>
    let mut headers: Vec<(String, String)> = Vec::new();
    let hkey = make_string("headers");
    let htv = crate::object::lin_object_get(obj, hkey);
    lin_string_release(hkey);
    if !htv.is_null() {
        let htv_typed = htv as *const TaggedVal;
        if let Some((hobj, hobj_owned)) = tagged_as_object(htv_typed) {
            let len = (*hobj).len as usize;
            for i in 0..len {
                let entry = (*hobj).entries.add(i);
                let key_s = (*entry).key;
                let kslice = std::slice::from_raw_parts((*key_s).data.as_ptr(), (*key_s).len as usize);
                let name = match std::str::from_utf8(kslice) {
                    Ok(s) => s.to_string(),
                    Err(_) => continue,
                };
                let vtv = &(*entry).value;
                if vtv.tag == TAG_STR {
                    let vs = vtv.payload as *const LinString;
                    let vslice = std::slice::from_raw_parts((*vs).data.as_ptr(), (*vs).len as usize);
                    if let Ok(v) = std::str::from_utf8(vslice) {
                        headers.push((name, v.to_string()));
                    }
                }
            }
            if hobj_owned { crate::object::lin_object_release(hobj as *mut LinObject); }
        }
    }

    if resp_owned { crate::object::lin_object_release(obj as *mut LinObject); }
    wire_response(status, &headers, &body)
}

/// Format an HTTP/1.1 response. Always sets Content-Length; adds Connection: close
/// so the sequential server can close the socket after each response.
fn wire_response(status: i32, headers: &[(String, String)], body: &str) -> Vec<u8> {
    let reason = reason_phrase(status);
    let mut out = format!("HTTP/1.1 {} {}\r\n", status, reason);
    let mut saw_content_length = false;
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("content-length") {
            saw_content_length = true;
        }
        out.push_str(&format!("{}: {}\r\n", name, value));
    }
    if !saw_content_length {
        out.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    out.push_str("Connection: close\r\n");
    out.push_str("\r\n");
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(body.as_bytes());
    bytes
}

fn reason_phrase(status: i32) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "OK",
    }
}

/// Invoke a serve handler closure `(env?, req) -> resp` by raw fn/env pointers.
/// Mirrors `call_worker_handler` in `async_rt.rs` (same `extern "C-unwind"` ABI).
unsafe fn call_serve_handler(fn_ptr: *mut u8, env_ptr: *mut u8, has_env: u8, req: *mut u8) -> *mut u8 {
    if fn_ptr.is_null() {
        return std::ptr::null_mut();
    }
    if has_env != 0 {
        let call: unsafe extern "C-unwind" fn(*mut u8, *mut u8) -> *mut u8 = std::mem::transmute(fn_ptr);
        call(env_ptr, req)
    } else {
        let call: unsafe extern "C-unwind" fn(*mut u8) -> *mut u8 = std::mem::transmute(fn_ptr);
        call(req)
    }
}

/// Start an HTTP/1.1 server on `port` and serve requests sequentially, calling
/// `handler` for each. `handler_*` is the Lin handler closure `(HttpRequest) -> HttpResponse`.
/// Blocks forever; returns only on bind failure (as an `Error` object).
#[no_mangle]
pub unsafe extern "C" fn lin_serve(
    handler_fn: *mut u8,
    handler_env: *mut u8,
    has_env: u8,
    port: i32,
) -> *mut u8 {
    crate::fault::install_quiet_fault_hook();

    let listener = match TcpListener::bind(("0.0.0.0", port as u16)) {
        Ok(l) => l,
        Err(e) => return make_error_tagged(&format!("failed to bind port {}: {}", port, e)),
    };

    for incoming in listener.incoming() {
        let mut stream = match incoming {
            Ok(s) => s,
            Err(_) => continue,
        };
        // A slow/half-open client must not wedge the sequential loop forever.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));

        let parsed = match read_request(&mut stream) {
            Some((buf, header_end)) => parse_http_request(&buf, header_end),
            None => None,
        };

        let wire = match parsed {
            Some(req) => {
                // Build the request object (owned), hand it to the handler. The handler
                // consumes its argument per Lin's arg-passing convention (same as the
                // worker message path), so we do NOT release `req_obj` here.
                let req_obj = build_request_object(&req);
                let outcome = crate::fault::with_async_boundary(|| {
                    call_serve_handler(handler_fn, handler_env, has_env, req_obj)
                });
                match outcome {
                    Ok(resp) => {
                        let wire = serialize_response(resp);
                        // The handler returns its result owned; release it after serializing.
                        if !resp.is_null() {
                            crate::tagged::lin_tagged_release(resp);
                        }
                        wire
                    }
                    Err(_) => wire_response(500, &[], "Internal Server Error"),
                }
            }
            None => wire_response(400, &[], "Bad Request"),
        };

        let _ = stream.write_all(&wire);
        let _ = stream.flush();
        // Connection: close — drop the stream to close the socket.
    }

    std::ptr::null_mut()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &[u8]) -> Option<ParsedRequest> {
        let header_end = find_header_end(raw)?;
        parse_http_request(raw, header_end)
    }

    #[test]
    fn parses_simple_get() {
        let r = parse(b"GET /hello HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        assert_eq!(r.method, "GET");
        assert_eq!(r.path, "/hello");
        assert_eq!(r.query, "");
        assert!(r.headers.iter().any(|(n, v)| n == "Host" && v == "x"));
        assert_eq!(r.body, "");
    }

    #[test]
    fn parses_path_and_query() {
        let r = parse(b"GET /users/1?verbose=true HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(r.path, "/users/1");
        assert_eq!(r.query, "verbose=true");
    }

    #[test]
    fn parses_body() {
        let raw = b"POST /api HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello";
        let r = parse(raw).unwrap();
        assert_eq!(r.method, "POST");
        assert_eq!(r.body, "hello");
    }

    #[test]
    fn malformed_request_line_is_none() {
        // Empty request line yields no method/target.
        assert!(parse(b"\r\n\r\n").is_none());
    }

    #[test]
    fn wire_response_has_content_length() {
        let bytes = wire_response(200, &[], "ok");
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(s.contains("Content-Length: 2\r\n"));
        assert!(s.ends_with("\r\nok"));
    }
}
