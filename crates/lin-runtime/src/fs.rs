use crate::string::{LinString, lin_string_from_bytes};
use crate::object::{LinObject, lin_object_alloc, lin_object_set};
use crate::array::lin_array_alloc;
use crate::tagged::{TaggedVal, TAG_STR, TAG_OBJECT, TAG_ARRAY, alloc_tagged, lin_unbox_ptr};

pub unsafe fn make_string(s: &str) -> *mut LinString {
    lin_string_from_bytes(s.as_ptr(), s.len() as u32)
}

pub unsafe fn make_error_tagged(msg: &str) -> *mut u8 {
    alloc_tagged(TAG_OBJECT, make_error_obj(msg) as u64)
}

/// C-callable wrapper: take a LinString* message, return TaggedVal*(Object error).
#[no_mangle]
pub unsafe extern "C" fn lin_make_error_tagged(msg: *const LinString) -> *mut u8 {
    let slice = std::slice::from_raw_parts((*msg).data.as_ptr(), (*msg).len as usize);
    let s = std::str::from_utf8_unchecked(slice);
    make_error_tagged(s)
}

unsafe fn make_error_obj(msg: &str) -> *mut LinObject {
    let obj = lin_object_alloc(4);
    let type_key = make_string("type");
    let error_val = make_string("error");
    let msg_key = make_string("message");
    let msg_val = make_string(msg);
    let mut tv: TaggedVal = std::mem::zeroed();
    tv.tag = TAG_STR;
    tv.payload = error_val as u64;
    lin_object_set(obj, type_key, &tv);
    // Note: lin_object_set takes ownership of key pointer; do NOT release type_key.
    let mut tv2: TaggedVal = std::mem::zeroed();
    tv2.tag = TAG_STR;
    tv2.payload = msg_val as u64;
    lin_object_set(obj, msg_key, &tv2);
    // Note: lin_object_set takes ownership of key pointer; do NOT release msg_key.
    obj
}

/// Resolve a path that may be either a bare LinString* or a TaggedVal*(Str).
/// Returns a Rust String on success, None on null/invalid input.
pub unsafe fn resolve_lin_str(ptr: *const u8) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let tag = *ptr;
    let lin_str = if tag == TAG_STR {
        lin_unbox_ptr(ptr) as *const LinString
    } else {
        ptr as *const LinString
    };
    let slice = std::slice::from_raw_parts((*lin_str).data.as_ptr(), (*lin_str).len as usize);
    std::str::from_utf8(slice).ok().map(|s| s.to_owned())
}

/// Read entire file as string. Returns TaggedVal*(Str) or TaggedVal*(Object error) on failure.
#[no_mangle]
pub unsafe extern "C" fn lin_fs_read_file(path: *const u8) -> *mut u8 {
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return make_error_tagged("invalid UTF-8 path"),
    };
    match std::fs::read_to_string(&path_str) {
        Ok(content) => alloc_tagged(TAG_STR, make_string(&content) as u64),
        Err(e) => make_error_tagged(&e.to_string()),
    }
}

/// Write string content to file. Returns null on success, error object on failure.
#[no_mangle]
pub unsafe extern "C" fn lin_fs_write_file(path: *const u8, content: *const u8) -> *mut u8 {
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return make_error_tagged("invalid path"),
    };
    let content_str = match resolve_lin_str(content) {
        Some(s) => s,
        None => return make_error_tagged("invalid content"),
    };
    match std::fs::write(&path_str, content_str.as_bytes()) {
        Ok(_) => std::ptr::null_mut(),
        Err(e) => make_error_tagged(&e.to_string()),
    }
}

/// Append string content to file. Returns null on success, error object on failure.
#[no_mangle]
pub unsafe extern "C" fn lin_fs_append_file(path: *const u8, content: *const u8) -> *mut u8 {
    use std::io::Write;
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return make_error_tagged("invalid path"),
    };
    let content_str = match resolve_lin_str(content) {
        Some(s) => s,
        None => return make_error_tagged("invalid content"),
    };
    let mut file = match std::fs::OpenOptions::new().create(true).append(true).open(&path_str) {
        Ok(f) => f,
        Err(e) => return make_error_tagged(&e.to_string()),
    };
    match file.write_all(content_str.as_bytes()) {
        Ok(_) => std::ptr::null_mut(),
        Err(e) => make_error_tagged(&e.to_string()),
    }
}

/// Check if file exists. Returns u8 bool.
#[no_mangle]
pub unsafe extern "C" fn lin_fs_exists(path: *const u8) -> u8 {
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return 0,
    };
    std::path::Path::new(&path_str).exists() as u8
}

/// Check if path is a regular file. Returns u8 bool.
#[no_mangle]
pub unsafe extern "C" fn lin_fs_is_file(path: *const u8) -> u8 {
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return 0,
    };
    std::path::Path::new(&path_str).is_file() as u8
}

/// Check if path is a directory. Returns u8 bool.
#[no_mangle]
pub unsafe extern "C" fn lin_fs_is_dir(path: *const u8) -> u8 {
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return 0,
    };
    std::path::Path::new(&path_str).is_dir() as u8
}

/// Return file metadata as a tagged object.
/// On success returns TaggedVal*(Object) with fields: size, modified, created, isFile, isDir.
/// On failure returns TaggedVal*(Object error).
#[no_mangle]
pub unsafe extern "C" fn lin_fs_stat(path: *const u8) -> *mut u8 {
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return make_error_tagged("invalid path"),
    };
    match std::fs::metadata(&path_str) {
        Err(e) => make_error_tagged(&e.to_string()),
        Ok(meta) => {
            use std::time::UNIX_EPOCH;
            let modified = meta.modified().ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let created = meta.created().ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            let size = meta.len() as i64;
            let is_file = meta.is_file();
            let is_dir = meta.is_dir();

            let obj = lin_object_alloc(8);

            let k_size = make_string("size");
            let mut tv_size: TaggedVal = std::mem::zeroed();
            tv_size.tag = crate::tagged::TAG_INT64;
            tv_size.payload = size as u64;
            lin_object_set(obj, k_size, &tv_size);

            let k_modified = make_string("modified");
            let mut tv_modified: TaggedVal = std::mem::zeroed();
            tv_modified.tag = crate::tagged::TAG_INT64;
            tv_modified.payload = modified as u64;
            lin_object_set(obj, k_modified, &tv_modified);

            let k_created = make_string("created");
            let mut tv_created: TaggedVal = std::mem::zeroed();
            tv_created.tag = crate::tagged::TAG_INT64;
            tv_created.payload = created as u64;
            lin_object_set(obj, k_created, &tv_created);

            let k_is_file = make_string("isFile");
            let mut tv_is_file: TaggedVal = std::mem::zeroed();
            tv_is_file.tag = crate::tagged::TAG_BOOL;
            tv_is_file.payload = is_file as u64;
            lin_object_set(obj, k_is_file, &tv_is_file);

            let k_is_dir = make_string("isDir");
            let mut tv_is_dir: TaggedVal = std::mem::zeroed();
            tv_is_dir.tag = crate::tagged::TAG_BOOL;
            tv_is_dir.payload = is_dir as u64;
            lin_object_set(obj, k_is_dir, &tv_is_dir);

            alloc_tagged(TAG_OBJECT, obj as u64)
        }
    }
}

/// List directory entries. Returns TaggedVal*(Array of Str) on success, TaggedVal*(Object error) on failure.
#[no_mangle]
pub unsafe extern "C" fn lin_fs_list_dir(path: *const u8) -> *mut u8 {
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return make_error_tagged("invalid UTF-8 path"),
    };
    match std::fs::read_dir(&path_str) {
        Err(e) => make_error_tagged(&e.to_string()),
        Ok(entries) => {
            let arr = lin_array_alloc(8);
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let s = make_string(&name);
                let mut tv: crate::tagged::TaggedVal = std::mem::zeroed();
                tv.tag = TAG_STR;
                tv.payload = s as u64;
                crate::array::lin_array_push_tagged(arr, &tv as *const crate::tagged::TaggedVal as *const u8);
            }
            alloc_tagged(TAG_ARRAY, arr as u64)
        }
    }
}

/// Create a single directory. Returns null on success, TaggedVal*(Object error) on failure.
#[no_mangle]
pub unsafe extern "C" fn lin_fs_mkdir(path: *const u8) -> *mut u8 {
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return make_error_tagged("invalid UTF-8 path"),
    };
    match std::fs::create_dir(&path_str) {
        Ok(_) => std::ptr::null_mut(),
        Err(e) => make_error_tagged(&e.to_string()),
    }
}

/// Create a directory and all parent directories. Returns null on success, TaggedVal*(Object error) on failure.
#[no_mangle]
pub unsafe extern "C" fn lin_fs_mkdir_all(path: *const u8) -> *mut u8 {
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return make_error_tagged("invalid UTF-8 path"),
    };
    match std::fs::create_dir_all(&path_str) {
        Ok(_) => std::ptr::null_mut(),
        Err(e) => make_error_tagged(&e.to_string()),
    }
}

/// Delete a file. Returns null on success, TaggedVal*(Object error) on failure.
#[no_mangle]
pub unsafe extern "C" fn lin_fs_delete_file(path: *const u8) -> *mut u8 {
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return make_error_tagged("invalid UTF-8 path"),
    };
    match std::fs::remove_file(&path_str) {
        Ok(_) => std::ptr::null_mut(),
        Err(e) => make_error_tagged(&e.to_string()),
    }
}

/// Rename (move) a file or directory. Returns null on success, TaggedVal*(Object error) on failure.
#[no_mangle]
pub unsafe extern "C" fn lin_fs_rename(from: *const u8, to: *const u8) -> *mut u8 {
    let from_str = match resolve_lin_str(from) {
        Some(s) => s,
        None => return make_error_tagged("invalid UTF-8 source path"),
    };
    let to_str = match resolve_lin_str(to) {
        Some(s) => s,
        None => return make_error_tagged("invalid UTF-8 destination path"),
    };
    match std::fs::rename(&from_str, &to_str) {
        Ok(_) => std::ptr::null_mut(),
        Err(e) => make_error_tagged(&e.to_string()),
    }
}

/// Read lines from file into a LinArray of LinString*. Returns bare LinArray* or null on error.
/// The returned pointer is a raw LinArray* (not a TaggedVal*) for direct use by Array(Str) slots.
#[no_mangle]
pub unsafe extern "C" fn lin_fs_read_lines(path: *const u8) -> *mut u8 {
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return std::ptr::null_mut(),
    };
    let content = match std::fs::read_to_string(&path_str) {
        Ok(s) => s,
        Err(_) => return std::ptr::null_mut(),
    };
    let lines: Vec<&str> = content.lines().collect();
    let arr = lin_array_alloc(lines.len().max(4) as u64);
    for line in &lines {
        let s = make_string(line);
        let mut tv: TaggedVal = std::mem::zeroed();
        tv.tag = TAG_STR;
        tv.payload = s as u64;
        crate::array::lin_array_push_tagged(arr, &tv as *const TaggedVal as *const u8);
    }
    arr as *mut u8
}
