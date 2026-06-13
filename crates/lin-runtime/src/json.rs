/// JSON parsing and serialization for Lin runtime.
use crate::string::LinString;
use crate::object::{lin_object_alloc, lin_object_set, tagged_as_object};
use crate::array::{LinArray, lin_array_alloc};
use crate::tagged::{TaggedVal, TAG_NULL, TAG_BOOL, TAG_INT32, TAG_INT64, TAG_FLOAT64, TAG_STR, TAG_OBJECT, TAG_ARRAY, alloc_tagged};
use crate::fs::{make_string, make_error_tagged, resolve_lin_str};

/// Convert a serde_json Value to a TaggedVal*.
pub unsafe fn json_to_tagged(val: &serde_json::Value) -> *mut u8 {
    match val {
        serde_json::Value::Null => std::ptr::null_mut(),
        serde_json::Value::Bool(b) => alloc_tagged(TAG_BOOL, *b as u64),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if i >= i32::MIN as i64 && i <= i32::MAX as i64 {
                    alloc_tagged(TAG_INT32, i as i64 as u64)
                } else {
                    alloc_tagged(TAG_INT64, i as u64)
                }
            } else if let Some(f) = n.as_f64() {
                alloc_tagged(TAG_FLOAT64, f.to_bits())
            } else {
                std::ptr::null_mut()
            }
        }
        serde_json::Value::String(s) => {
            let ls = make_string(s);
            alloc_tagged(TAG_STR, ls as u64)
        }
        serde_json::Value::Array(arr) => {
            let la = lin_array_alloc(arr.len().max(4) as u64);
            for item in arr {
                let tv_ptr = json_to_tagged(item);
                if tv_ptr.is_null() {
                    let tv: TaggedVal = std::mem::zeroed();
                    crate::array::lin_array_push_tagged(la, &tv as *const TaggedVal as *const u8);
                } else {
                    crate::array::lin_array_push_tagged(la, tv_ptr);
                }
            }
            alloc_tagged(TAG_ARRAY, la as u64)
        }
        serde_json::Value::Object(map) => {
            let obj = lin_object_alloc(map.len().max(4) as u32);
            for (k, v) in map {
                let key = make_string(k);
                let val_ptr = json_to_tagged(v);
                let tv: TaggedVal = if val_ptr.is_null() {
                    std::mem::zeroed()
                } else {
                    std::ptr::read(val_ptr as *const TaggedVal)
                };
                lin_object_set(obj, key, &tv);
                // Note: lin_object_set takes ownership of key pointer; do NOT release key.
            }
            alloc_tagged(TAG_OBJECT, obj as u64)
        }
    }
}

/// Convert a TaggedVal* to a serde_json Value.
pub unsafe fn tagged_to_json(tv: *const u8) -> serde_json::Value {
    if tv.is_null() {
        return serde_json::Value::Null;
    }
    let t = tv as *const TaggedVal;
    let tag = (*t).tag;
    let payload = (*t).payload;
    match tag {
        TAG_NULL => serde_json::Value::Null,
        TAG_BOOL => serde_json::Value::Bool(payload != 0),
        TAG_INT32 => serde_json::json!(payload as i32),
        TAG_INT64 => serde_json::json!(payload as i64),
        crate::tagged::TAG_UINT64 => serde_json::json!(payload),
        TAG_FLOAT64 => serde_json::json!(f64::from_bits(payload)),
        TAG_STR => {
            let s = payload as *const LinString;
            let slice = std::slice::from_raw_parts((*s).data.as_ptr(), (*s).len as usize);
            let str_val = std::str::from_utf8_unchecked(slice);
            serde_json::Value::String(str_val.to_owned())
        }
        TAG_ARRAY => {
            let arr = payload as *const LinArray;
            let len = (*arr).len as usize;
            let mut vec = Vec::with_capacity(len);
            for i in 0..len as i64 {
                let elem = crate::array::lin_array_get_tagged(arr, i);
                vec.push(tagged_to_json(elem as *const u8));
            }
            serde_json::Value::Array(vec)
        }
        TAG_OBJECT => {
            let obj = payload as *const crate::object::LinObject;
            let len = (*obj).len as usize;
            let mut map = serde_json::Map::new();
            for i in 0..len {
                let entry = (*obj).entries.add(i);
                let key_s = (*entry).key;
                let slice = std::slice::from_raw_parts((*key_s).data.as_ptr(), (*key_s).len as usize);
                let key_str = std::str::from_utf8_unchecked(slice).to_owned();
                let val_tv = &(*entry).value as *const TaggedVal as *const u8;
                map.insert(key_str, tagged_to_json(val_tv));
            }
            serde_json::Value::Object(map)
        }
        crate::tagged::TAG_RECORD => {
            // Stage 6a: sealed-struct pointer in a dynamic slot. Materialize to a LinObject
            // and serialize as an object, then release the transient.
            match tagged_as_object(t) {
                Some((obj, owned)) => {
                    let len = (*obj).len as usize;
                    let mut map = serde_json::Map::new();
                    for i in 0..len {
                        let entry = (*obj).entries.add(i);
                        let key_s = (*entry).key;
                        let slice = std::slice::from_raw_parts((*key_s).data.as_ptr(), (*key_s).len as usize);
                        let key_str = std::str::from_utf8_unchecked(slice).to_owned();
                        let val_tv = &(*entry).value as *const TaggedVal as *const u8;
                        map.insert(key_str, tagged_to_json(val_tv));
                    }
                    if owned { crate::object::lin_object_release(obj as *mut crate::object::LinObject); }
                    serde_json::Value::Object(map)
                }
                None => serde_json::Value::Null,
            }
        }
        _ => serde_json::Value::Null,
    }
}

/// Parse a JSON string into a TaggedVal*. Returns error object on failure.
/// s may be a bare LinString* or a TaggedVal*(Str).
#[no_mangle]
pub unsafe extern "C" fn lin_parse_json(s: *const u8) -> *mut u8 {
    let src = match resolve_lin_str(s) {
        Some(s) => s,
        None => return make_error_tagged("invalid UTF-8"),
    };
    match serde_json::from_str::<serde_json::Value>(&src) {
        Ok(val) => json_to_tagged(&val),
        Err(e) => make_error_tagged(&e.to_string()),
    }
}

/// Write a TaggedVal* as JSON to a file. path may be LinString* or TaggedVal*(Str).
#[no_mangle]
pub unsafe extern "C" fn lin_fs_write_json(path: *const u8, val: *const u8) -> *mut u8 {
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return make_error_tagged("invalid path"),
    };
    let json_val = tagged_to_json(val);
    let serialized = serde_json::to_string_pretty(&json_val).unwrap_or_default();
    match std::fs::write(&path_str, &serialized) {
        Ok(_) => std::ptr::null_mut(),
        Err(e) => make_error_tagged(&e.to_string()),
    }
}

/// Write a TaggedVal* as compact (single-line) JSON to a file.
#[no_mangle]
pub unsafe extern "C" fn lin_fs_write_json_compact(path: *const u8, val: *const u8) -> *mut u8 {
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return make_error_tagged("invalid path"),
    };
    let json_val = tagged_to_json(val);
    let serialized = serde_json::to_string(&json_val).unwrap_or_default();
    match std::fs::write(&path_str, &serialized) {
        Ok(_) => std::ptr::null_mut(),
        Err(e) => make_error_tagged(&e.to_string()),
    }
}

/// Read a file and parse it as JSON. path may be LinString* or TaggedVal*(Str).
#[no_mangle]
pub unsafe extern "C" fn lin_fs_read_json(path: *const u8) -> *mut u8 {
    let path_str = match resolve_lin_str(path) {
        Some(s) => s,
        None => return make_error_tagged("invalid path"),
    };
    match std::fs::read_to_string(&path_str) {
        Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
            Ok(val) => json_to_tagged(&val),
            Err(e) => make_error_tagged(&e.to_string()),
        },
        Err(e) => make_error_tagged(&e.to_string()),
    }
}
