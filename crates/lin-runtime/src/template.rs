use crate::string::{LinString, lin_string_from_bytes, lin_tagged_to_string};
use crate::object::{LinObject, lin_object_get};
use crate::tagged::{TaggedVal, TAG_OBJECT};

/// Render a template string against a data object.
///
/// Template syntax: `${key}` or `${key.nested.path}` holes; everything else is
/// literal text.  Missing keys render as the string `"null"`.
///
/// Signature (C-ABI): (template: *const LinString, data: *const u8) -> *mut LinString
/// `data` may be a raw `LinObject*` or a `TaggedVal*(Object)`.
#[no_mangle]
pub unsafe extern "C" fn lin_template_render(
    template: *const LinString,
    data: *const u8,
) -> *mut LinString {
    // Unwrap data: accept either a raw LinObject* or a TaggedVal*(TAG_OBJECT).
    let obj: *const LinObject = if data.is_null() {
        std::ptr::null()
    } else {
        let tag = *data;
        if tag == TAG_OBJECT {
            let tv = data as *const TaggedVal;
            (*tv).payload as *const LinObject
        } else {
            data as *const LinObject
        }
    };

    let src = (*template).as_str();
    let mut result: Vec<u8> = Vec::with_capacity(src.len());
    let bytes = src.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        // Look for '${'
        if i + 1 < bytes.len() && bytes[i] == b'$' && bytes[i + 1] == b'{' {
            i += 2;
            let start = i;
            while i < bytes.len() && bytes[i] != b'}' {
                i += 1;
            }
            let path = &src[start..i];
            if i < bytes.len() {
                i += 1; // skip '}'
            }

            // Walk dot-separated path into the object.
            let val_str = resolve_path(obj, path);
            result.extend_from_slice(val_str.as_bytes());
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }

    lin_string_from_bytes(result.as_ptr(), result.len() as u32)
}

unsafe fn resolve_path(obj: *const LinObject, path: &str) -> String {
    let mut cur_obj = obj;
    let segments: Vec<&str> = path.split('.').collect();
    let last = segments.len() - 1;

    for (idx, seg) in segments.iter().enumerate() {
        if cur_obj.is_null() {
            return "null".to_owned();
        }
        let key = lin_string_from_bytes(seg.as_ptr(), seg.len() as u32);
        let tv = lin_object_get(cur_obj, key);
        // key is a temporary; drop it now (no refcount here, alloc is just for lookup)
        std::alloc::dealloc(
            key as *mut u8,
            std::alloc::Layout::from_size_align_unchecked(
                std::mem::size_of::<crate::string::LinString>() + seg.len(),
                std::mem::align_of::<u32>(),
            ),
        );

        if tv.is_null() {
            return "null".to_owned();
        }

        if idx == last {
            // Convert the final value to its display string. `lin_tagged_to_string` returns an
            // OWNED (+1) string; copy its bytes into an owned Rust String, then release our
            // reference so the heap string is not leaked (no-op for immortal literals).
            let s = lin_tagged_to_string(tv);
            let owned = (*s).as_str().to_owned();
            crate::string::lin_string_release(s);
            return owned;
        }

        // Descend into nested object.
        use crate::tagged::TAG_OBJECT;
        let tag = (*tv).tag;
        if tag == TAG_OBJECT {
            cur_obj = (*tv).payload as *const LinObject;
        } else {
            return "null".to_owned();
        }
    }
    "null".to_owned()
}
