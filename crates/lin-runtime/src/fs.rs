use crate::string::{LinString, lin_string_from_bytes};
use crate::object::{LinObject, lin_object_alloc, lin_object_set};
use crate::array::{lin_array_alloc, LinArray, lin_array_length, lin_array_get_tagged,
                   lin_flat_array_alloc_u8, lin_flat_array_push_u8};
use crate::tagged::{TaggedVal, TAG_STR, TAG_INT32, TAG_OBJECT, TAG_ARRAY, alloc_tagged, lin_unbox_ptr};

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
    use crate::string::lin_string_release;
    let obj = lin_object_alloc(4);
    let type_key = make_string("type");
    let error_val = make_string("error");
    let msg_key = make_string("message");
    let msg_val = make_string(msg);
    let mut tv: TaggedVal = std::mem::zeroed();
    tv.tag = TAG_STR;
    tv.payload = error_val as u64;
    lin_object_set(obj, type_key, &tv); // inc_refs type_key, retains error_val
    let mut tv2: TaggedVal = std::mem::zeroed();
    tv2.tag = TAG_STR;
    tv2.payload = msg_val as u64;
    lin_object_set(obj, msg_key, &tv2); // inc_refs msg_key, retains msg_val
    // lin_object_set takes its OWN reference to each key and value; release the local +1
    // from each make_string so the object is the sole owner (freeing the returned error
    // object then frees everything — no leak; verified under LeakSanitizer).
    lin_string_release(type_key);
    lin_string_release(error_val);
    lin_string_release(msg_key);
    lin_string_release(msg_val);
    obj
}

/// Build a `fromJson` decode error as an owned `TaggedVal*(Object)` (ADR-047). Shape:
/// `{ "type": "error", "message": <msg>, "path": <path> }`. The `type`/`message` fields keep
/// the existing error convention; `path` is a JSONPath-ish location (e.g. `$.address.city`).
/// Returned value is independently owned by the caller (release with `lin_tagged_release`).
///
/// Unlike the legacy `make_error_obj`, this builds the object leak-cleanly: `lin_object_set`
/// retains the key and the value's inner payload, so the local +1 reference created here for
/// each freshly-allocated key/value string is released afterwards. The net owner of every
/// inner string is the object; releasing the returned box frees the object and, transitively,
/// every string — no orphaned +1 references (verified under ASan).
pub unsafe fn make_decode_error(msg: &str, path: &str) -> *mut u8 {
    use crate::string::lin_string_release;
    let obj = lin_object_alloc(4);
    let set_str = |obj: *mut LinObject, key: &str, val: &str| {
        let k = make_string(key);
        let v = make_string(val);
        let mut tv: TaggedVal = std::mem::zeroed();
        tv.tag = TAG_STR;
        tv.payload = v as u64;
        lin_object_set(obj, k, &tv); // retains both k and v
        lin_string_release(k); // drop our local +1 (object now owns its own ref)
        lin_string_release(v);
    };
    set_str(obj, "type", "error");
    set_str(obj, "message", msg);
    set_str(obj, "path", path);
    alloc_tagged(TAG_OBJECT, obj as u64)
}

/// Resolve a path that may be either a bare LinString* or a TaggedVal*(Str).
/// Returns a Rust String on success, None on null/invalid input.
///
/// Discriminating boxed-vs-raw by the first byte is unsound: a boxed `TaggedVal{tag,pad,..}`
/// has `tag` at offset 0, but a raw `LinString{refcount:u32,len:u32,..}` has its refcount
/// there — so a string whose refcount's low byte equals TAG_STR(6) would be mis-detected as
/// boxed and its char data read as a pointer. Compare the FULL first 8 bytes instead: a
/// boxed string's leading u64 is exactly TAG_STR (tag=6 with zeroed pad), whereas a raw
/// LinString's leading u64 is `(len << 32) | refcount`, which only equals 6 for an empty
/// string with refcount exactly 6 — a collision narrow enough not to arise in practice.
pub unsafe fn resolve_lin_str(ptr: *const u8) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let head = (ptr as *const u64).read_unaligned();
    let lin_str = if head == TAG_STR as u64 {
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

            #[cfg(unix)]
            let mode: i32 = {
                use std::os::unix::fs::MetadataExt;
                meta.mode() as i32
            };
            #[cfg(not(unix))]
            let mode: i32 = 0i32;

            let k_mode = make_string("mode");
            let mut tv_mode: TaggedVal = std::mem::zeroed();
            tv_mode.tag = crate::tagged::TAG_INT32;
            tv_mode.payload = mode as i64 as u64;
            lin_object_set(obj, k_mode, &tv_mode);

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

/// Read lines from file. Returns TaggedVal*(Array of Str) on success, TaggedVal*(Object error) on failure.
#[no_mangle]
pub unsafe extern "C" fn lin_fs_read_lines(path: *const u8) -> *mut u8 {
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return make_error_tagged("invalid UTF-8 path"),
    };
    let content = match std::fs::read_to_string(&path_str) {
        Ok(s) => s,
        Err(e) => return make_error_tagged(&e.to_string()),
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
    alloc_tagged(TAG_ARRAY, arr as u64)
}

/// Copy a file from src to dst. Returns null on success, TaggedVal*(Object error) on failure.
#[no_mangle]
pub unsafe extern "C" fn lin_fs_cp(src: *const u8, dst: *const u8) -> *mut u8 {
    let src_str = match resolve_lin_str(src) {
        Some(s) => s,
        None => return make_error_tagged("invalid UTF-8 source path"),
    };
    let dst_str = match resolve_lin_str(dst) {
        Some(s) => s,
        None => return make_error_tagged("invalid UTF-8 destination path"),
    };
    match std::fs::copy(&src_str, &dst_str) {
        Ok(_) => std::ptr::null_mut(),
        Err(e) => make_error_tagged(&e.to_string()),
    }
}

/// Remove a file or directory. recursive!=0 allows removing directories recursively.
/// Returns null on success, TaggedVal*(Object error) on failure.
#[no_mangle]
pub unsafe extern "C" fn lin_fs_rm(path: *const u8, recursive: u8) -> *mut u8 {
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return make_error_tagged("invalid UTF-8 path"),
    };
    let p = std::path::Path::new(&path_str);
    let result = if recursive != 0 {
        if p.is_dir() {
            std::fs::remove_dir_all(p)
        } else {
            std::fs::remove_file(p)
        }
    } else {
        std::fs::remove_file(p)
    };
    match result {
        Ok(_) => std::ptr::null_mut(),
        Err(e) => make_error_tagged(&e.to_string()),
    }
}

/// List all files recursively under path. Returns TaggedVal*(Array of Str) on success, error on failure.
/// Paths are relative to the given root directory.
#[no_mangle]
pub unsafe extern "C" fn lin_fs_list_dir_all(path: *const u8) -> *mut u8 {
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return make_error_tagged("invalid UTF-8 path"),
    };
    let arr = lin_array_alloc(8);
    match collect_dir_recursive(&path_str, "", arr) {
        Ok(_) => alloc_tagged(TAG_ARRAY, arr as u64),
        Err(e) => {
            // arr will leak but this is an error path
            make_error_tagged(&e)
        }
    }
}

unsafe fn collect_dir_recursive(base: &str, prefix: &str, arr: *mut LinArray) -> Result<(), String> {
    let read_path = if prefix.is_empty() {
        base.to_string()
    } else {
        format!("{}/{}", base, prefix)
    };
    let entries = std::fs::read_dir(&read_path)
        .map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", prefix, name)
        };
        let s = make_string(&rel);
        let mut tv: TaggedVal = std::mem::zeroed();
        tv.tag = TAG_STR;
        tv.payload = s as u64;
        crate::array::lin_array_push_tagged(arr, &tv as *const TaggedVal as *const u8);
        let ft = entry.file_type().map_err(|e| e.to_string())?;
        if ft.is_dir() {
            collect_dir_recursive(base, &rel, arr)?;
        }
    }
    Ok(())
}

/// Read a file as raw bytes. Returns TaggedVal*(flat UInt8 array) on success, error on failure.
/// The result is a packed UInt8[] byte buffer (§27.1): one byte per element.
#[no_mangle]
pub unsafe extern "C" fn lin_fs_read_file_bytes(path: *const u8) -> *mut u8 {
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return make_error_tagged("invalid UTF-8 path"),
    };
    let bytes = match std::fs::read(&path_str) {
        Ok(b) => b,
        Err(e) => return make_error_tagged(&e.to_string()),
    };
    let arr = lin_flat_array_alloc_u8(bytes.len().max(4) as u64);
    for b in &bytes {
        lin_flat_array_push_u8(arr, *b);
    }
    alloc_tagged(TAG_ARRAY, arr as u64)
}

/// Write a UInt8[] byte buffer as raw bytes to a file.
/// arr is a TaggedVal*(Array) or raw LinArray*; the inner array is normally a
/// flat UInt8 array (read directly from its u8 data) but a tagged array is also
/// tolerated (each element read via lin_array_get_tagged).
/// Returns null on success, error on failure.
#[no_mangle]
pub unsafe extern "C" fn lin_fs_write_file_bytes(path: *const u8, arr: *const u8) -> *mut u8 {
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return make_error_tagged("invalid UTF-8 path"),
    };
    if arr.is_null() {
        return make_error_tagged("null array");
    }
    // arr may be a TaggedVal*(Array) or a raw LinArray*
    let tag = *arr;
    let lin_arr = if tag == TAG_ARRAY {
        let tv = arr as *const TaggedVal;
        (*tv).payload as *const LinArray
    } else {
        arr as *const LinArray
    };
    let len = lin_array_length(lin_arr) as usize;
    let mut bytes: Vec<u8> = Vec::with_capacity(len);
    let elem_tag = (*lin_arr).elem_tag;
    if elem_tag == crate::tagged::TAG_UINT8 || elem_tag == crate::tagged::TAG_INT8 {
        // Flat 1-byte array: read raw bytes straight from the data buffer.
        let data = (*lin_arr).data as *const u8;
        for i in 0..len {
            bytes.push(*data.add(i));
        }
    } else {
        // Fallback for tagged or other-width arrays: box each element and truncate.
        for i in 0..len as i64 {
            let tv_ptr = lin_array_get_tagged(lin_arr, i);
            let val = if tv_ptr.is_null() {
                0u8
            } else {
                let etag = (*tv_ptr).tag;
                let payload = (*tv_ptr).payload;
                let v = match etag {
                    TAG_INT32 => payload as i32,
                    _ => payload as i32,
                };
                // Free the allocated TaggedVal returned by lin_array_get_tagged.
                std::alloc::dealloc(tv_ptr as *mut u8, std::alloc::Layout::new::<TaggedVal>());
                v as u8
            };
            bytes.push(val);
        }
    }
    match std::fs::write(&path_str, &bytes) {
        Ok(_) => std::ptr::null_mut(),
        Err(e) => make_error_tagged(&e.to_string()),
    }
}

/// Write an array of strings to a file, one per line with a trailing newline.
/// arr is a TaggedVal*(Array of Str).
/// Returns null on success, error on failure.
#[no_mangle]
pub unsafe extern "C" fn lin_fs_write_lines(path: *const u8, arr: *const u8) -> *mut u8 {
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return make_error_tagged("invalid UTF-8 path"),
    };
    if arr.is_null() {
        return make_error_tagged("null array");
    }
    // arr may be a TaggedVal*(Array) or a raw LinArray*
    let tag = *arr;
    let lin_arr = if tag == TAG_ARRAY {
        let tv = arr as *const TaggedVal;
        (*tv).payload as *const LinArray
    } else {
        arr as *const LinArray
    };
    let len = lin_array_length(lin_arr) as usize;
    let mut lines: Vec<String> = Vec::with_capacity(len);
    // Read each String element's LinString* directly from the array's data buffer
    // (payload field of the 16-byte LinArrayElem), the same way lin_string_join_arr
    // does. This avoids lin_array_get_tagged, which allocates a fresh TaggedVal and
    // retains the payload — the previous `resolve_lin_str(tv as *const u8)` approach
    // misread that wrapper as a LinString and leaked the retain, causing intermittent
    // wild-pointer reads under load.
    for i in 0..len {
        let elem = (*lin_arr).data.add(i);
        let s = (*elem).payload as *const crate::string::LinString;
        if s.is_null() {
            lines.push(String::new());
        } else {
            let slice = std::slice::from_raw_parts((*s).data.as_ptr(), (*s).len as usize);
            lines.push(std::str::from_utf8(slice).unwrap_or_default().to_owned());
        }
    }
    let content = lines.join("\n") + "\n";
    match std::fs::write(&path_str, content.as_bytes()) {
        Ok(_) => std::ptr::null_mut(),
        Err(e) => make_error_tagged(&e.to_string()),
    }
}

#[cfg(test)]
mod write_lines_tests {
    use super::*;
    use crate::array::{lin_array_alloc, lin_array_push};
    use crate::string::lin_string_from_bytes;
    use crate::tagged::{alloc_tagged, TAG_STR, TAG_ARRAY};

    // Regression: lin_fs_write_lines previously read each String element with
    // resolve_lin_str(tv as *const u8) on a freshly lin_array_get_tagged'd TaggedVal,
    // which misread the wrapper and leaked a retain — producing intermittent wild-pointer
    // SEGVs under allocation load. This test drives write_lines directly with a real
    // tagged String[] and verifies the file contents, exercising the element-read path.
    // Run under `cargo test` (and the -Zsanitizer=address CI leg) it is deterministic.
    #[test]
    fn write_lines_reads_string_elements_correctly() {
        unsafe {
            // Build a tagged String[] = ["foo", "bar", "baz"].
            let arr = lin_array_alloc(4);
            for word in ["foo", "bar", "baz"] {
                let s = lin_string_from_bytes(word.as_ptr(), word.len() as u32);
                let payload = s as u64;
                lin_array_push(arr, &payload as *const u64 as *const u8, TAG_STR);
            }
            let arr_tagged = alloc_tagged(TAG_ARRAY, arr as u64);

            // Path string as a TaggedVal*(Str), matching how Lin passes a String arg.
            let path = "/tmp/lin_write_lines_unit_test.txt";
            let path_str = lin_string_from_bytes(path.as_ptr(), path.len() as u32);
            let path_tagged = alloc_tagged(TAG_STR, path_str as u64);

            let res = lin_fs_write_lines(path_tagged as *const u8, arr_tagged as *const u8);
            assert!(res.is_null(), "write_lines should succeed (null = ok)");

            let written = std::fs::read_to_string(path).expect("file should exist");
            assert_eq!(written, "foo\nbar\nbaz\n");
            let _ = std::fs::remove_file(path);
        }
    }
}
