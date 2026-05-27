use crate::string::{LinString, lin_string_from_bytes};
use std::path::Path;

#[no_mangle]
pub unsafe extern "C" fn lin_path_basename(p: *const LinString) -> *mut LinString {
    let st = (*p).as_str();
    let result = Path::new(st)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    lin_string_from_bytes(result.as_ptr(), result.len() as u32)
}

#[no_mangle]
pub unsafe extern "C" fn lin_path_dirname(p: *const LinString) -> *mut LinString {
    let st = (*p).as_str();
    let result = Path::new(st)
        .parent()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());
    // Empty parent (e.g. "file.txt" has parent "") should be "."
    let result = if result.is_empty() { ".".to_string() } else { result };
    lin_string_from_bytes(result.as_ptr(), result.len() as u32)
}

#[no_mangle]
pub unsafe extern "C" fn lin_path_extname(p: *const LinString) -> *mut LinString {
    let st = (*p).as_str();
    let result = Path::new(st)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    lin_string_from_bytes(result.as_ptr(), result.len() as u32)
}

#[no_mangle]
pub unsafe extern "C" fn lin_path_stem(p: *const LinString) -> *mut LinString {
    let st = (*p).as_str();
    let result = Path::new(st)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    lin_string_from_bytes(result.as_ptr(), result.len() as u32)
}

#[no_mangle]
pub unsafe extern "C" fn lin_path_is_absolute(p: *const LinString) -> bool {
    let st = (*p).as_str();
    Path::new(st).is_absolute()
}

/// Resolve . and .. components without touching the filesystem.
#[no_mangle]
pub unsafe extern "C" fn lin_path_normalize(p: *const LinString) -> *mut LinString {
    let st = (*p).as_str();
    let is_absolute = st.starts_with('/');
    let mut components: Vec<&str> = Vec::new();
    for part in st.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            other => components.push(other),
        }
    }
    let result = if is_absolute {
        format!("/{}", components.join("/"))
    } else {
        let joined = components.join("/");
        if joined.is_empty() { ".".to_string() } else { joined }
    };
    lin_string_from_bytes(result.as_ptr(), result.len() as u32)
}

/// Join two path segments, normalising redundant slashes.
#[no_mangle]
pub unsafe extern "C" fn lin_path_join2(a: *const LinString, b: *const LinString) -> *mut LinString {
    let a_str = (*a).as_str();
    let b_str = (*b).as_str();
    let joined = if a_str.is_empty() {
        b_str.to_string()
    } else if b_str.is_empty() {
        a_str.to_string()
    } else if a_str.ends_with('/') || b_str.starts_with('/') {
        format!("{}{}", a_str.trim_end_matches('/'), b_str)
    } else {
        format!("{}/{}", a_str, b_str)
    };
    lin_string_from_bytes(joined.as_ptr(), joined.len() as u32)
}

/// Resolve a path to an absolute path using the real filesystem (canonicalise if possible,
/// otherwise join cwd + path).
#[no_mangle]
pub unsafe extern "C" fn lin_path_resolve(p: *const LinString) -> *mut LinString {
    let st = (*p).as_str();
    let result = std::fs::canonicalize(st)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| {
            let cwd = std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            if Path::new(st).is_absolute() {
                st.to_string()
            } else {
                format!("{}/{}", cwd, st)
            }
        });
    lin_string_from_bytes(result.as_ptr(), result.len() as u32)
}
