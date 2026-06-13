/// JSON parsing and serialization for Lin runtime.
use crate::string::LinString;
use crate::object::tagged_as_object;
use crate::array::{LinArray, lin_array_alloc};
use crate::tagged::{TaggedVal, TAG_NULL, TAG_BOOL, TAG_INT32, TAG_INT64, TAG_FLOAT64, TAG_STR, TAG_OBJECT, TAG_ARRAY, alloc_tagged};
use crate::fs::{make_string, make_error_tagged, resolve_lin_str};
use crate::sealed::{
    SEALED_HEADER,
    NKIND_INT32, NKIND_INT64, NKIND_FLOAT64, NKIND_BOOL, NKIND_STRING, NKIND_ARRAY, NKIND_SEALED,
    KIND_STRING, KIND_ARRAY, KIND_SEALED,
};
use std::sync::{Mutex, OnceLock};
use std::collections::HashMap;

// --------------------------------------------------------------------------
// Descriptor interning cache
//
// Each distinct combination of (ordered field names, nkind values, and for
// NKIND_SEALED fields the nested named_desc pointer) maps to ONE immortal pair
// of (heap_desc, named_desc) blobs. Both are leaked once (process-lifetime);
// the Mutex guards the HashMap itself. The pointers are stored as usize so the
// raw-pointer value is Send + Sync through the Mutex.
// --------------------------------------------------------------------------

/// Cache key: for each field, (name, nkind, nested_named_desc_ptr_as_usize).
/// For non-SEALED fields, nested_ptr = 0.
type DescCacheKey = Vec<(String, u32, usize)>;
/// Cache value: (heap_desc_ptr as usize, named_desc_ptr as usize).
/// heap_desc_ptr may be 0 (null) when there are no heap fields.
type DescCacheVal = (usize, usize);

static NAMED_DESC_CACHE: OnceLock<Mutex<HashMap<DescCacheKey, DescCacheVal>>> = OnceLock::new();

fn desc_cache() -> &'static Mutex<HashMap<DescCacheKey, DescCacheVal>> {
    NAMED_DESC_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

// --------------------------------------------------------------------------
// Helper: nkind from JSON value
// --------------------------------------------------------------------------

fn json_nkind(val: &serde_json::Value) -> u32 {
    match val {
        serde_json::Value::Bool(_) => NKIND_BOOL,
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if i >= i32::MIN as i64 && i <= i32::MAX as i64 {
                    NKIND_INT32
                } else {
                    NKIND_INT64
                }
            } else {
                NKIND_FLOAT64
            }
        }
        serde_json::Value::String(_) => NKIND_STRING,
        serde_json::Value::Array(_) => NKIND_ARRAY,
        serde_json::Value::Object(_) => NKIND_SEALED,
        // Null → 8-byte ptr slot, null pointer stored.
        // lin_record_get_field returns null for a null NKIND_STRING slot.
        serde_json::Value::Null => NKIND_STRING,
    }
}

// --------------------------------------------------------------------------
// Helper: field slot size in bytes for a given nkind
// --------------------------------------------------------------------------

fn nkind_slot_size(nkind: u32) -> usize {
    match nkind {
        NKIND_BOOL => 1,
        NKIND_INT32 => 4,
        // INT64, FLOAT64, and all heap kinds (STRING, ARRAY, SEALED, MAP) → 8-byte slot
        _ => 8,
    }
}

// --------------------------------------------------------------------------
// Helper: compute per-field byte offsets + total struct size from an ordered
// list of (nkind) values. Fields are packed with natural alignment starting at
// SEALED_HEADER; total size is padded to 8-byte alignment.
// --------------------------------------------------------------------------

fn compute_layout(nkinds: &[u32]) -> (Vec<usize>, usize) {
    let mut offsets = Vec::with_capacity(nkinds.len());
    let mut offset = SEALED_HEADER;
    for &nkind in nkinds {
        let sz = nkind_slot_size(nkind);
        // Align offset to field size (natural alignment).
        offset = (offset + sz - 1) / sz * sz;
        offsets.push(offset);
        offset += sz;
    }
    // Pad total to 8-byte multiple.
    let total = (offset + 7) & !7;
    (offsets, total)
}

// --------------------------------------------------------------------------
// Build (and intern) descriptor blobs for a given field schema.
//
// `field_schema`: slice of (name, nkind, nested_named_desc_ptr) — one per field.
// Returns (heap_desc_ptr, named_desc_ptr) — both immortal (leaked, process-lifetime).
// heap_desc_ptr may be null if there are no heap fields.
// --------------------------------------------------------------------------

unsafe fn get_or_build_descriptors(
    field_schema: &[(&str, u32, *const u8)],
) -> (*const u8, *const u8) {
    // Build the cache key.
    let key: DescCacheKey = field_schema.iter()
        .map(|(name, nkind, nested)| (name.to_string(), *nkind, *nested as usize))
        .collect();

    {
        let cache = desc_cache().lock().unwrap();
        if let Some(&(hp, np)) = cache.get(&key) {
            return (hp as *const u8, np as *const u8);
        }
    }

    // Not cached — build the blobs.
    let nkinds: Vec<u32> = field_schema.iter().map(|(_, nk, _)| *nk).collect();
    let (offsets, _total_size) = compute_layout(&nkinds);

    // ----- Named descriptor blob -----
    // Layout: [ u32 field_count | NamedField * count ]
    // NamedField = [ u32 byte_offset | u32 nkind | u64 nested_named_desc_ptr |
    //                u16 name_len | name_bytes ]
    let mut named_blob: Vec<u8> = Vec::new();
    named_blob.extend_from_slice(&(field_schema.len() as u32).to_le_bytes());
    for (i, (name, nkind, nested)) in field_schema.iter().enumerate() {
        named_blob.extend_from_slice(&(offsets[i] as u32).to_le_bytes()); // byte_offset
        named_blob.extend_from_slice(&nkind.to_le_bytes());               // nkind
        named_blob.extend_from_slice(&(*nested as u64).to_le_bytes());    // nested_named_desc_ptr
        named_blob.extend_from_slice(&(name.len() as u16).to_le_bytes()); // name_len
        named_blob.extend_from_slice(name.as_bytes());                    // name_bytes
    }
    let named_ptr: *const u8 = Box::leak(named_blob.into_boxed_slice()).as_ptr();

    // ----- Heap descriptor blob -----
    // Layout: [ u32 count | { u32 byte_offset, u32 kind } * count ]
    // Only heap fields are listed (STRING, ARRAY, SEALED — not scalars or BOOL).
    let mut heap_fields: Vec<(u32, u32)> = Vec::new();
    for (i, (_, nkind, _)) in field_schema.iter().enumerate() {
        let kind = match *nkind {
            NKIND_STRING => KIND_STRING,
            NKIND_ARRAY  => KIND_ARRAY,
            NKIND_SEALED => KIND_SEALED,
            // scalars and BOOL need no heap descriptor entry
            _ => continue,
        };
        heap_fields.push((offsets[i] as u32, kind));
    }
    let heap_ptr: *const u8 = if heap_fields.is_empty() {
        std::ptr::null()
    } else {
        let mut heap_blob: Vec<u8> = Vec::new();
        heap_blob.extend_from_slice(&(heap_fields.len() as u32).to_le_bytes());
        for (off, kind) in &heap_fields {
            heap_blob.extend_from_slice(&off.to_le_bytes());
            heap_blob.extend_from_slice(&kind.to_le_bytes());
        }
        Box::leak(heap_blob.into_boxed_slice()).as_ptr()
    };

    // Insert into cache.
    {
        let mut cache = desc_cache().lock().unwrap();
        cache.entry(key).or_insert((heap_ptr as usize, named_ptr as usize));
    }

    (heap_ptr, named_ptr)
}

// --------------------------------------------------------------------------
// Build a fresh sealed struct for a JSON object map.
// Returns an owned (*mut u8) sealed struct pointer with rc = 1.
// Returns null_mut() for errors (defensive; shouldn't happen for valid inputs).
// --------------------------------------------------------------------------

unsafe fn json_object_to_sealed(map: &serde_json::Map<String, serde_json::Value>) -> *mut u8 {
    // First pass: determine nkind for each field and recursively build nested objects.
    // We need to do this BEFORE building descriptors, because for NKIND_SEALED fields
    // we need the nested struct's named_desc pointer to include in the cache key.

    struct FieldInfo {
        name: String,
        nkind: u32,
        // For NKIND_SEALED: the inner struct pointer (rc=1, owned by us temporarily).
        // For other kinds: null.
        nested_sealed_ptr: *mut u8,
        // For NKIND_SEALED: the inner struct's named_desc (read from the inner header).
        // For other kinds: null.
        nested_named_desc: *const u8,
    }

    let mut fields: Vec<FieldInfo> = Vec::with_capacity(map.len());

    for (k, v) in map.iter() {
        let nkind = json_nkind(v);
        let (nested_sealed_ptr, nested_named_desc) = if nkind == NKIND_SEALED {
            if let serde_json::Value::Object(inner_map) = v {
                let inner_ptr = json_object_to_sealed(inner_map);
                if inner_ptr.is_null() {
                    (std::ptr::null_mut(), std::ptr::null())
                } else {
                    // Read the inner struct's named_desc from header offset 16.
                    let nd = *((inner_ptr.add(16)) as *const *const u8);
                    (inner_ptr, nd)
                }
            } else {
                (std::ptr::null_mut(), std::ptr::null())
            }
        } else {
            (std::ptr::null_mut(), std::ptr::null())
        };

        fields.push(FieldInfo {
            name: k.clone(),
            nkind,
            nested_sealed_ptr,
            nested_named_desc,
        });
    }

    // Build the field_schema for descriptor lookup.
    let field_schema: Vec<(&str, u32, *const u8)> = fields.iter()
        .map(|f| (f.name.as_str(), f.nkind, f.nested_named_desc))
        .collect();

    // Get or build the descriptor pair.
    let (heap_desc, named_desc) = get_or_build_descriptors(&field_schema);

    // Compute the layout to know per-field offsets and total struct size.
    let nkinds: Vec<u32> = fields.iter().map(|f| f.nkind).collect();
    let (offsets, total_size) = compute_layout(&nkinds);

    // Allocate the sealed struct (zero-initialised, rc=1).
    let sptr = crate::sealed::lin_sealed_alloc(total_size, heap_desc, named_desc);

    // Populate each field.
    for (i, ((_k, v), field)) in map.iter().zip(fields.iter()).enumerate() {
        let offset = offsets[i];
        let slot = sptr.add(offset);

        match field.nkind {
            NKIND_BOOL => {
                let b: u8 = if let serde_json::Value::Bool(b) = v { *b as u8 } else { 0 };
                *slot = b;
            }
            NKIND_INT32 => {
                let iv = if let serde_json::Value::Number(n) = v {
                    n.as_i64().unwrap_or(0) as i32
                } else {
                    0
                };
                *(slot as *mut i32) = iv;
            }
            NKIND_INT64 => {
                let iv = if let serde_json::Value::Number(n) = v {
                    n.as_i64().unwrap_or(0)
                } else {
                    0
                };
                *(slot as *mut i64) = iv;
            }
            NKIND_FLOAT64 => {
                let fv = if let serde_json::Value::Number(n) = v {
                    n.as_f64().unwrap_or(0.0)
                } else {
                    0.0
                };
                *(slot as *mut f64) = fv;
            }
            NKIND_STRING => {
                // String field or Null field (Null maps to NKIND_STRING with null ptr).
                let ls: *mut LinString = if let serde_json::Value::String(s) = v {
                    make_string(s) as *mut LinString
                } else {
                    // Null or unexpected: null pointer in the slot.
                    std::ptr::null_mut()
                };
                // The struct slot takes ownership of the +1 from make_string.
                // (A null pointer is valid: lin_record_get_field returns null for null NKIND_STRING.)
                *(slot as *mut *mut LinString) = ls;
            }
            NKIND_ARRAY => {
                // Build a LinArray containing the JSON array elements.
                let la: *mut LinArray = if let serde_json::Value::Array(arr) = v {
                    let la = lin_array_alloc(arr.len().max(4) as u64);
                    for item in arr {
                        let tv_ptr = json_to_tagged(item);
                        if tv_ptr.is_null() {
                            let tv: TaggedVal = std::mem::zeroed();
                            crate::array::lin_array_push_tagged(la, &tv as *const TaggedVal as *const u8);
                        } else {
                            crate::array::lin_array_push_tagged(la, tv_ptr);
                            // Pre-existing: tv_ptr box is NOT freed here (same pattern as the
                            // outer Array arm of json_to_tagged). The inner payload's +1 is now
                            // owned by the array slot; the box shell is intentionally leaked
                            // (matches the historical Object arm's val_ptr leak).
                        }
                    }
                    la
                } else {
                    std::ptr::null_mut()
                };
                // The struct slot takes ownership of the array's +1 reference.
                *(slot as *mut *mut LinArray) = la;
            }
            NKIND_SEALED => {
                // Transfer ownership of the pre-built nested sealed struct into the slot.
                // The nested struct was built with rc=1; that +1 is now owned by this slot.
                // On drop, lin_sealed_release_self will be called via the heap descriptor.
                *(slot as *mut *mut u8) = field.nested_sealed_ptr;
            }
            _ => {
                // Unexpected nkind — slot is already zeroed from lin_sealed_alloc.
            }
        }
    }

    sptr
}

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
            // Stage 6a leg-3: JSON objects produce TAG_RECORD (sealed struct by-pointer),
            // not TAG_OBJECT (LinObject).
            //
            // RC contract: json_object_to_sealed returns an OWNED pointer (rc=1). We transfer
            // that +1 directly into the TaggedVal box via alloc_tagged — we do NOT call
            // lin_box_record here because lin_box_record adds an extra retain (it is designed
            // for the BORROWED-pointer case: the caller keeps its own +1 and the box gets an
            // additional +1). Using lin_box_record on an already-owned pointer would leave
            // rc=2 with only one owner, causing a leak on release.
            let sptr = json_object_to_sealed(map);
            alloc_tagged(crate::tagged::TAG_RECORD, sptr as u64)
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

// --------------------------------------------------------------------------
// Unit tests: exercise the TAG_RECORD path from json_to_tagged.
// --------------------------------------------------------------------------

#[cfg(test)]
mod fromjson_record_tests {
    use super::*;
    use crate::tagged::{TAG_RECORD, TAG_STR, TAG_INT32, TAG_ARRAY};

    // Helper: create a LinString key and look up a field in a TAG_RECORD box.
    // Returns an OWNED TaggedVal* box (caller must lin_tagged_release).
    unsafe fn record_get(tv_ptr: *mut u8, key: &str) -> *mut u8 {
        let k = crate::string::lin_string_from_bytes(key.as_ptr(), key.len() as u32);
        let sealed = (*(tv_ptr as *const TaggedVal)).payload as *const u8;
        let result = crate::sealed::lin_record_get_field(sealed, k);
        crate::string::lin_string_release(k);
        result
    }

    // ── TAG_RECORD tag ──────────────────────────────────────────────────────

    #[test]
    fn parse_simple_object_is_tag_record() {
        unsafe {
            let v: serde_json::Value = serde_json::from_str(r#"{"x":1,"name":"a"}"#).unwrap();
            let tv_ptr = json_to_tagged(&v);
            assert!(!tv_ptr.is_null(), "json_to_tagged returned null for an object");
            let tag = (*(tv_ptr as *const TaggedVal)).tag;
            assert_eq!(tag, TAG_RECORD, "object must produce TAG_RECORD (not TAG_OBJECT)");
            // RC contract: json_object_to_sealed returns rc=1 (the TaggedVal's owned +1).
            let sptr = (*(tv_ptr as *const TaggedVal)).payload as *const u8;
            assert_eq!(*(sptr as *const u32), 1, "sealed struct rc must be 1 (owned by the box)");
            crate::tagged::lin_tagged_release(tv_ptr);
        }
    }

    // ── Scalar field reads ──────────────────────────────────────────────────

    #[test]
    fn parse_object_int_field() {
        unsafe {
            let v: serde_json::Value = serde_json::from_str(r#"{"x":42}"#).unwrap();
            let tv_ptr = json_to_tagged(&v);
            let field = record_get(tv_ptr, "x");
            assert!(!field.is_null(), "field 'x' must be found");
            let ftv = field as *const TaggedVal;
            assert_eq!((*ftv).tag, TAG_INT32);
            assert_eq!((*ftv).payload as i32, 42);
            crate::tagged::lin_tagged_release(field);
            crate::tagged::lin_tagged_release(tv_ptr);
        }
    }

    #[test]
    fn parse_object_string_field() {
        unsafe {
            let v: serde_json::Value = serde_json::from_str(r#"{"name":"hello"}"#).unwrap();
            let tv_ptr = json_to_tagged(&v);
            let field = record_get(tv_ptr, "name");
            assert!(!field.is_null());
            let ftv = field as *const TaggedVal;
            assert_eq!((*ftv).tag, TAG_STR);
            let s = (*ftv).payload as *const crate::string::LinString;
            assert_eq!((*s).as_str(), "hello");
            crate::tagged::lin_tagged_release(field);
            crate::tagged::lin_tagged_release(tv_ptr);
        }
    }

    // ── Nested object ───────────────────────────────────────────────────────

    #[test]
    fn parse_nested_object_accessible() {
        unsafe {
            let v: serde_json::Value = serde_json::from_str(r#"{"p":{"q":2}}"#).unwrap();
            let tv_ptr = json_to_tagged(&v);

            // Get field "p" — lin_record_get_field for NKIND_SEALED materialises the nested
            // struct to a LinObject tagged TAG_OBJECT.
            let p_box = record_get(tv_ptr, "p");
            assert!(!p_box.is_null(), "nested field 'p' must be accessible");
            let p_tag = (*(p_box as *const TaggedVal)).tag;
            assert_eq!(p_tag, TAG_OBJECT, "nested sealed field must materialise to TAG_OBJECT");

            // Access ["q"] from the nested materialised object.
            let obj = (*(p_box as *const TaggedVal)).payload as *const crate::object::LinObject;
            let k = crate::string::lin_string_from_bytes(b"q".as_ptr(), 1);
            let q_tv = crate::object::lin_object_get(obj, k);
            assert!(!q_tv.is_null());
            assert_eq!((*q_tv).tag, TAG_INT32);
            assert_eq!((*q_tv).payload as i32, 2);
            crate::string::lin_string_release(k);

            crate::tagged::lin_tagged_release(p_box);
            crate::tagged::lin_tagged_release(tv_ptr);
        }
    }

    // ── Array field ──────────────────────────────────────────────────────────

    #[test]
    fn parse_object_with_array_field() {
        unsafe {
            let v: serde_json::Value = serde_json::from_str(r#"{"nums":[10,20,30]}"#).unwrap();
            let tv_ptr = json_to_tagged(&v);
            let field = record_get(tv_ptr, "nums");
            assert!(!field.is_null());
            let ftv = field as *const TaggedVal;
            assert_eq!((*ftv).tag, TAG_ARRAY);
            let arr = (*ftv).payload as *const LinArray;
            assert_eq!((*arr).len, 3);
            crate::tagged::lin_tagged_release(field);
            crate::tagged::lin_tagged_release(tv_ptr);
        }
    }

    // ── tagged_to_json round-trip ────────────────────────────────────────────

    #[test]
    fn tagged_to_json_roundtrip_object() {
        unsafe {
            let v: serde_json::Value = serde_json::from_str(r#"{"x":1,"name":"a"}"#).unwrap();
            let tv_ptr = json_to_tagged(&v);
            let back = tagged_to_json(tv_ptr as *const u8);
            match &back {
                serde_json::Value::Object(m) => {
                    assert_eq!(m.get("x"), Some(&serde_json::Value::Number(1.into())));
                    assert_eq!(m.get("name"), Some(&serde_json::Value::String("a".to_owned())));
                }
                other => panic!("expected JSON object, got {:?}", other),
            }
            crate::tagged::lin_tagged_release(tv_ptr);
        }
    }

    // ── Descriptor interning: same shape → same descriptor pointer ───────────

    #[test]
    fn same_shape_reuses_descriptors() {
        unsafe {
            let v1: serde_json::Value = serde_json::from_str(r#"{"x":1}"#).unwrap();
            let v2: serde_json::Value = serde_json::from_str(r#"{"x":99}"#).unwrap();
            let tv1 = json_to_tagged(&v1);
            let tv2 = json_to_tagged(&v2);
            let s1 = (*(tv1 as *const TaggedVal)).payload as *const u8;
            let s2 = (*(tv2 as *const TaggedVal)).payload as *const u8;
            // Both structs must share the same named_desc pointer (descriptor interning).
            let nd1 = *((s1.add(16)) as *const *const u8);
            let nd2 = *((s2.add(16)) as *const *const u8);
            assert_eq!(nd1, nd2, "same field shape must share a named_desc pointer");
            crate::tagged::lin_tagged_release(tv1);
            crate::tagged::lin_tagged_release(tv2);
        }
    }

    // ── RC balance: TAG_RECORD release walks heap fields without double-free ──

    #[test]
    fn tagged_release_heap_field_rc_balanced() {
        unsafe {
            // Parse an object with a string field.
            // After json_object_to_sealed: struct rc=1 (TaggedVal owns it via alloc_tagged direct).
            // After lin_record_get_field(NKIND_STRING): the struct's string slot is RETAINED (+1),
            // so the returned box + the struct slot both own the string.
            // After lin_tagged_release(field): string rc back to 1 (only struct owns it).
            // After lin_tagged_release(tv_ptr): struct rc→0 → heap_desc walk releases string (rc→0).
            let v: serde_json::Value = serde_json::from_str(r#"{"s":"world"}"#).unwrap();
            let tv_ptr = json_to_tagged(&v);
            assert_eq!((*(tv_ptr as *const TaggedVal)).tag, TAG_RECORD);
            let sptr = (*(tv_ptr as *const TaggedVal)).payload as *const u8;
            // rc=1: the box is the sole owner.
            assert_eq!(*(sptr as *const u32), 1);

            let field = record_get(tv_ptr, "s");
            assert!(!field.is_null());
            // lin_record_get_field retained the string: struct slot keeps its +1, field box adds +1.
            let str_ptr = (*(field as *const TaggedVal)).payload as *const crate::string::LinString;
            assert_eq!((*str_ptr).refcount, 2, "string must be retained by lin_record_get_field");

            crate::tagged::lin_tagged_release(field);
            assert_eq!((*str_ptr).refcount, 1, "after field release, string rc must be 1");

            // Release the outer box: rc=1→0 → heap_desc walk releases the string (rc→0, freed).
            crate::tagged::lin_tagged_release(tv_ptr);
            // No assertion here — just "didn't crash" proves the walk fired without double-free.
        }
    }
}
