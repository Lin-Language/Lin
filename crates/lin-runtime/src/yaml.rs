//! `std/yaml` runtime support: parse and serialise YAML to/from the tagged Json
//! representation, reusing the existing `serde_json::Value` <-> tagged bridge.

use crate::fs::{make_error_tagged, make_string, resolve_lin_str};
use crate::json::{json_to_tagged, tagged_to_json};
use crate::string::LinString;

/// Split a YAML stream into per-document source slices on `---` document markers (a line whose
/// trimmed content is exactly `---`, the standard YAML document boundary). The leading marker of
/// the first document, if any, is included with that document and tolerated by the parser.
fn split_yaml_documents(src: &str) -> Vec<&str> {
    let mut docs = Vec::new();
    let mut start = 0usize;
    let mut pos = 0usize;
    for line in src.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']).trim();
        if trimmed == "---" && pos > start {
            docs.push(&src[start..pos]);
            start = pos + line.len();
        } else if trimmed == "---" {
            // Leading separator before any content: advance start past it.
            start = pos + line.len();
        }
        pos += line.len();
    }
    if start < src.len() {
        docs.push(&src[start..]);
    }
    docs
}

/// Parse a single YAML document into a tagged Json value, or an error value on failure.
#[no_mangle]
pub unsafe extern "C" fn lin_yaml_parse(s: *const u8) -> *mut u8 {
    let src = match resolve_lin_str(s) {
        Some(s) => s,
        None => return make_error_tagged("invalid UTF-8"),
    };
    match serde_yml::from_str::<serde_json::Value>(&src) {
        Ok(val) => json_to_tagged(&val),
        Err(e) => make_error_tagged(&e.to_string()),
    }
}

/// Parse a multi-document YAML stream (`---`-separated) into a tagged Json array, or an error.
#[no_mangle]
pub unsafe extern "C" fn lin_yaml_parse_all(s: *const u8) -> *mut u8 {
    let src = match resolve_lin_str(s) {
        Some(s) => s,
        None => return make_error_tagged("invalid UTF-8"),
    };
    let mut docs: Vec<serde_json::Value> = Vec::new();
    for chunk in split_yaml_documents(&src) {
        // Skip whitespace-/comment-only chunks (e.g. a trailing `---`).
        if chunk
            .lines()
            .all(|l| l.trim().is_empty() || l.trim_start().starts_with('#'))
        {
            continue;
        }
        match serde_yml::from_str::<serde_json::Value>(chunk) {
            Ok(v) => docs.push(v),
            Err(e) => return make_error_tagged(&e.to_string()),
        }
    }
    json_to_tagged(&serde_json::Value::Array(docs))
}

/// Serialise a tagged Json value to a block-style YAML string.
#[no_mangle]
pub unsafe extern "C" fn lin_yaml_stringify(v: *const u8) -> *mut LinString {
    let val = tagged_to_json(v);
    let s = serde_yml::to_string(&val).unwrap_or_default();
    make_string(&s)
}

/// Serialise a tagged Json *array* into multiple YAML documents joined by `---\n`.
#[no_mangle]
pub unsafe extern "C" fn lin_yaml_stringify_all(v: *const u8) -> *mut LinString {
    let val = tagged_to_json(v);
    let docs: &[serde_json::Value] = match &val {
        serde_json::Value::Array(arr) => arr,
        // A non-array argument is treated as a single document.
        _ => std::slice::from_ref(&val),
    };
    let mut out = String::new();
    for doc in docs {
        let chunk = serde_yml::to_string(doc).unwrap_or_default();
        out.push_str("---\n");
        out.push_str(&chunk);
        // serde_yml does not always terminate a document with a newline; ensure the next
        // `---` marker starts on its own line.
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }
    make_string(&out)
}
