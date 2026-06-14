//! `std/url` runtime support: RFC 3986 URL parsing/splitting and reference resolution.
//!
//! The contract is **non-normalising and non-decoding**: `lin_url_parse` splits the input
//! into its raw component substrings exactly as they appeared (percent-escapes preserved,
//! case preserved except the scheme which RFC 3986 §3.1 defines as case-insensitive and we
//! lowercase), so the pure-Lin `build` can reproduce the input byte-for-byte. The `url` crate
//! is used ONLY for `lin_url_join`, where the WHATWG/RFC 3986 §5 reference-resolution
//! algorithm (dot-segment removal, merge-paths, scheme-relative/absolute-path/relative-path
//! precedence) is the correctness-critical part worth borrowing from a battle-tested crate.
//!
//! `parse` deliberately does NOT route through the `url` crate, because that crate normalises
//! (resolves dot-segments, may percent-encode, fills in nothing but reorders/cleans) and would
//! break the `build(parse(s)) == s` round-trip guarantee.

use crate::fs::{make_error_tagged, make_string, resolve_lin_str};
use crate::map::{lin_map_alloc, lin_map_set, LinMap};
use crate::string::{lin_string_from_bytes, lin_string_release, LinString};
use crate::tagged::{alloc_tagged, TaggedVal, TAG_INT32, TAG_MAP, TAG_STR};

/// The split result: each Option is None when the component was absent, Some(raw substring)
/// when present (which may be the empty string, e.g. an empty query in `http://h/?`).
struct UrlParts {
    scheme: String,        // already lowercased; "" if relative reference
    userinfo: Option<String>,
    host: String,          // "" if no authority
    port: Option<i32>,
    path: String,          // may be ""
    query: Option<String>, // WITHOUT leading '?'
    fragment: Option<String>, // WITHOUT leading '#'
    has_authority: bool,
}

/// Validate that every `%` in `s` is followed by exactly two ASCII hex digits, and that there
/// are no raw control bytes (0x00-0x1F, 0x7F) or spaces. Returns an error message on failure.
fn validate_component(s: &str, what: &str) -> Result<(), String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' {
            if i + 2 >= bytes.len() || !bytes[i + 1].is_ascii_hexdigit() || !bytes[i + 2].is_ascii_hexdigit() {
                return Err(format!("invalid percent-escape in {what}"));
            }
            i += 3;
            continue;
        }
        if b <= 0x1F || b == 0x7F || b == b' ' {
            return Err(format!("invalid control/space byte in {what}"));
        }
        i += 1;
    }
    Ok(())
}

/// Split a raw URL string into RFC 3986 components WITHOUT normalising or decoding.
///
/// Grammar (RFC 3986 Appendix B, applied positionally):
///   URI = scheme ":" hier-part [ "?" query ] [ "#" fragment ]
///   hier-part = "//" authority path-abempty / path-absolute / path-rootless / path-empty
///   authority = [ userinfo "@" ] host [ ":" port ]
fn split_url(input: &str) -> Result<UrlParts, String> {
    // No raw spaces or controls anywhere in a URI.
    // (Percent-escapes are validated per-component below; here we reject the obvious globals
    //  that no component may contain.)
    let mut rest = input;

    // 1. fragment (everything after the FIRST '#')
    let fragment = match rest.find('#') {
        Some(pos) => {
            let frag = rest[pos + 1..].to_string();
            rest = &rest[..pos];
            Some(frag)
        }
        None => None,
    };

    // 2. query (everything after the FIRST '?')
    let query = match rest.find('?') {
        Some(pos) => {
            let q = rest[pos + 1..].to_string();
            rest = &rest[..pos];
            Some(q)
        }
        None => None,
    };

    // 3. scheme: ALPHA *( ALPHA / DIGIT / "+" / "-" / "." ) ":" — only if the ':' precedes
    //    the first '/', '?' or '#'. The scheme must start with a letter. A bare "foo:bar" is
    //    absolute; "/a:b" or "a/b:c" is a relative reference whose path happens to contain ':'.
    let mut scheme = String::new();
    if let Some(colon) = rest.find(':') {
        let candidate = &rest[..colon];
        let valid_scheme = !candidate.is_empty()
            && candidate.as_bytes()[0].is_ascii_alphabetic()
            && candidate
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'-' || b == b'.');
        // Ensure no '/' appears before the colon (otherwise the ':' is inside a path segment).
        let no_slash_before = !candidate.contains('/');
        if valid_scheme && no_slash_before {
            scheme = candidate.to_ascii_lowercase();
            rest = &rest[colon + 1..];
        }
    }

    // 4. authority: present iff what's left starts with "//".
    let mut userinfo = None;
    let mut host = String::new();
    let mut port = None;
    let mut has_authority = false;
    let after_authority;
    if let Some(stripped) = rest.strip_prefix("//") {
        has_authority = true;
        // authority ends at the first '/', and there is no '?'/'#' left (already stripped).
        let (authority, path_part) = match stripped.find('/') {
            Some(pos) => (&stripped[..pos], &stripped[pos..]),
            None => (stripped, ""),
        };
        after_authority = path_part.to_string();

        // userinfo: everything before the LAST '@' in the authority (RFC: userinfo may itself
        // contain ':' but not '@'; the host is what follows the final '@').
        let host_port = match authority.rfind('@') {
            Some(at) => {
                userinfo = Some(authority[..at].to_string());
                &authority[at + 1..]
            }
            None => authority,
        };

        // host + optional port. An IPv6 literal is bracketed "[...]"; the port colon is the one
        // AFTER the closing ']'. For a reg-name/IPv4 the port colon is the last ':'.
        if let Some(rest_hp) = host_port.strip_prefix('[') {
            // IPv6 / IPvFuture literal.
            match rest_hp.find(']') {
                Some(close) => {
                    host = format!("[{}]", &rest_hp[..close]);
                    let tail = &rest_hp[close + 1..];
                    if let Some(p) = tail.strip_prefix(':') {
                        port = Some(parse_port(p)?);
                    } else if !tail.is_empty() {
                        return Err("invalid characters after IPv6 host".to_string());
                    }
                }
                None => return Err("unterminated IPv6 host".to_string()),
            }
        } else {
            match host_port.rfind(':') {
                Some(colon) => {
                    host = host_port[..colon].to_string();
                    port = Some(parse_port(&host_port[colon + 1..])?);
                }
                None => host = host_port.to_string(),
            }
        }

        if let Some(ui) = &userinfo {
            validate_component(ui, "userinfo")?;
        }
        validate_component(&host, "host")?;
    } else {
        after_authority = rest.to_string();
    }

    let path = after_authority;
    validate_component(&path, "path")?;
    if let Some(q) = &query {
        validate_component(q, "query")?;
    }
    if let Some(f) = &fragment {
        validate_component(f, "fragment")?;
    }

    Ok(UrlParts {
        scheme,
        userinfo,
        host,
        port,
        path,
        query,
        fragment,
        has_authority,
    })
}

fn parse_port(s: &str) -> Result<i32, String> {
    if s.is_empty() {
        // An empty port ("host:") — treat as absent rather than error? RFC allows empty port.
        // To keep round-trip fidelity simple we reject it; "host:" is rare and ambiguous.
        return Err("empty port".to_string());
    }
    if !s.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("port is not numeric: '{s}'"));
    }
    s.parse::<i32>().map_err(|_| format!("port out of range: '{s}'"))
}

/// Set a String field on the map (retaining via lin_map_set, then dropping our local +1).
unsafe fn set_str_field(map: *mut LinMap, key: &str, val: &str) {
    let k = make_string(key);
    let v = make_string(val);
    let mut tv: TaggedVal = std::mem::zeroed();
    tv.tag = TAG_STR;
    tv.payload = v as u64;
    lin_map_set(map, k, &tv);
    lin_string_release(k);
    lin_string_release(v);
}

/// Set a field to a Null value (absent component).
unsafe fn set_null_field(map: *mut LinMap, key: &str) {
    let k = make_string(key);
    let tv: TaggedVal = std::mem::zeroed(); // tag 0 == TAG_NULL
    lin_map_set(map, k, &tv);
    lin_string_release(k);
}

/// Set an Int32 field.
unsafe fn set_int_field(map: *mut LinMap, key: &str, val: i32) {
    let k = make_string(key);
    let mut tv: TaggedVal = std::mem::zeroed();
    tv.tag = TAG_INT32;
    tv.payload = val as i64 as u64;
    lin_map_set(map, k, &tv);
    lin_string_release(k);
}

/// Set a `String | Null` field from an Option.
unsafe fn set_opt_str_field(map: *mut LinMap, key: &str, val: &Option<String>) {
    match val {
        Some(s) => set_str_field(map, key, s),
        None => set_null_field(map, key),
    }
}

/// Parse a URL string into a Json object holding the raw `Url` record fields, or the canonical
/// `{ "type":"error", "message": ... }` value on a syntax violation.
///
/// The returned object always has all seven keys (scheme/userinfo/host/port/path/query/fragment)
/// so the pure-Lin wrapper can read each field unconditionally.
#[no_mangle]
pub unsafe extern "C" fn lin_url_parse(s: *const u8) -> *mut u8 {
    let input = match resolve_lin_str(s) {
        Some(v) => v,
        None => return make_error_tagged("invalid URL string"),
    };

    let parts = match split_url(&input) {
        Ok(p) => p,
        Err(e) => return make_error_tagged(&format!("invalid URL: {e}")),
    };

    let map = lin_map_alloc(8, 0);
    set_str_field(map, "scheme", &parts.scheme);
    set_opt_str_field(map, "userinfo", &parts.userinfo);
    // host is always a String (possibly ""), but when there is no authority it must be "".
    set_str_field(map, "host", if parts.has_authority { &parts.host } else { "" });
    match parts.port {
        Some(p) => set_int_field(map, "port", p),
        None => set_null_field(map, "port"),
    }
    set_str_field(map, "path", &parts.path);
    set_opt_str_field(map, "query", &parts.query);
    set_opt_str_field(map, "fragment", &parts.fragment);

    alloc_tagged(TAG_MAP, map as u64)
}

/// Resolve `ref` against `base` using RFC 3986 §5 reference resolution (the `url` crate's
/// WHATWG implementation). Returns the resolved absolute URL string, or "" on error — the Lin
/// wrapper turns "" into an `Error`. `base` must be an absolute URL.
#[no_mangle]
pub unsafe extern "C" fn lin_url_join(base: *const u8, reference: *const u8) -> *mut LinString {
    let empty = || lin_string_from_bytes("".as_ptr(), 0);
    let base_s = match resolve_lin_str(base) {
        Some(v) => v,
        None => return empty(),
    };
    let ref_s = match resolve_lin_str(reference) {
        Some(v) => v,
        None => return empty(),
    };

    let base_url = match url::Url::parse(&base_s) {
        Ok(u) => u,
        Err(_) => return empty(),
    };
    match base_url.join(&ref_s) {
        Ok(resolved) => lin_string_from_bytes(resolved.as_str().as_ptr(), resolved.as_str().len() as u32),
        Err(_) => empty(),
    }
}
