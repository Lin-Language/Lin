//! `fromJson` generic type-directed decoder (ADR-031).
//!
//! `lin_from_json(value, descriptor)` validates a Json value (`TaggedVal*`) against a
//! compile-time schema descriptor emitted by codegen (see
//! `crates/lin-codegen/src/codegen/intrinsics.rs` `DescEncoder`). On success it returns an
//! independently-owned clone of the input value (zero structural copy — the inner heap payload
//! is retained, the box is fresh). On the FIRST structural mismatch it returns a fresh decode
//! `Error` object carrying a JSONPath-ish location.
//!
//! The descriptor is a flat little-endian byte blob; offsets are absolute byte indices into the
//! same blob, so recursive/cyclic types are finite back-edges. The byte format MUST match the
//! encoder. See the `KIND_*` constants below.

use std::fmt::Write as _;

use crate::array::{lin_array_length, LinArray};
use crate::fs::make_decode_error;
use crate::tagged::{
    TaggedVal, tagged_as_f64, lin_unbox_ptr, lin_tagged_clone,
    TAG_NULL, TAG_BOOL, TAG_INT8, TAG_INT16, TAG_INT32, TAG_INT64,
    TAG_UINT8, TAG_UINT16, TAG_UINT32, TAG_UINT64, TAG_FLOAT32, TAG_FLOAT64,
    TAG_STR, TAG_ARRAY, TAG_RECORD, TAG_MAP,
};

// Descriptor opcodes — keep in sync with DescEncoder in lin-codegen.
const KIND_JSON: u8 = 0;
const KIND_NULL: u8 = 1;
const KIND_BOOL: u8 = 2;
const KIND_STRING: u8 = 3;
const KIND_INT: u8 = 4;
const KIND_FLOAT: u8 = 5;
const KIND_ARRAY: u8 = 6;
const KIND_FIXED: u8 = 7;
const KIND_OBJECT: u8 = 8;
const KIND_UNION: u8 = 9;
const KIND_STRLIT: u8 = 10;

/// Read-only cursor over the descriptor blob. The blob is a static const global (never freed).
struct Desc {
    base: *const u8,
}

impl Desc {
    #[inline]
    unsafe fn u8_at(&self, off: usize) -> u8 {
        *self.base.add(off)
    }
    #[inline]
    unsafe fn u16_at(&self, off: usize) -> u16 {
        let p = self.base.add(off);
        u16::from_le_bytes([*p, *p.add(1)])
    }
    #[inline]
    unsafe fn u32_at(&self, off: usize) -> u32 {
        let p = self.base.add(off);
        u32::from_le_bytes([*p, *p.add(1), *p.add(2), *p.add(3)])
    }
    #[inline]
    unsafe fn str_at(&self, off: usize, len: usize) -> &str {
        let slice = std::slice::from_raw_parts(self.base.add(off), len);
        std::str::from_utf8_unchecked(slice)
    }
}

/// `(tag, payload)` of a (possibly null) boxed Json value.
unsafe fn tag_payload(v: *const u8) -> (u8, u64) {
    if v.is_null() {
        (TAG_NULL, 0)
    } else {
        let tv = &*(v as *const TaggedVal);
        (tv.tag, tv.payload)
    }
}

/// True if `tag` is any numeric tag.
fn is_numeric_tag(tag: u8) -> bool {
    matches!(
        tag,
        TAG_INT8 | TAG_INT16 | TAG_INT32 | TAG_INT64
            | TAG_UINT8 | TAG_UINT16 | TAG_UINT32 | TAG_UINT64
            | TAG_FLOAT32 | TAG_FLOAT64
    )
}

/// Inclusive integer range for a target int of `width_bytes` bytes and signedness.
fn int_range(width_bytes: u8, signed: bool) -> (i128, i128) {
    let bits = (width_bytes as u32) * 8;
    if signed {
        let lo = -(1i128 << (bits - 1));
        let hi = (1i128 << (bits - 1)) - 1;
        (lo, hi)
    } else {
        (0, (1i128 << bits) - 1)
    }
}

/// Validate `value` against the descriptor node at byte offset `node`. Returns `Ok(())` on
/// match, or `Err(message)` on the first mismatch. `path` is the JSONPath-ish location of
/// `value`, built up as we descend; it is restored to its original length before returning so a
/// caller can reuse the same buffer across sibling fields/elements.
unsafe fn validate(
    value: *const u8,
    desc: &Desc,
    node: usize,
    path: &mut String,
) -> Result<(), String> {
    let kind = desc.u8_at(node);
    let (tag, payload) = tag_payload(value);

    match kind {
        KIND_JSON => Ok(()),
        KIND_NULL => {
            if tag == TAG_NULL {
                Ok(())
            } else {
                Err(format!("expected Null at {}", path))
            }
        }
        KIND_BOOL => {
            if tag == TAG_BOOL {
                Ok(())
            } else {
                Err(format!("expected Boolean at {}", path))
            }
        }
        KIND_STRING => {
            if tag == TAG_STR {
                Ok(())
            } else {
                Err(format!("expected String at {}", path))
            }
        }
        KIND_STRLIT => {
            // Node layout: KIND_STRLIT, u16 lit_len, lit_bytes.
            let lit_len = desc.u16_at(node + 1) as usize;
            let expected = desc.str_at(node + 3, lit_len);
            if tag != TAG_STR {
                return Err(format!("expected \"{}\" at {}", expected, path));
            }
            let s = &*(payload as *const crate::string::LinString);
            if s.as_str() == expected {
                Ok(())
            } else {
                Err(format!("expected \"{}\" at {}, got \"{}\"", expected, path, s.as_str()))
            }
        }
        KIND_INT => {
            let width = desc.u8_at(node + 1);
            let signed = desc.u8_at(node + 2) != 0;
            if !is_numeric_tag(tag) {
                return Err(format!("expected an integer at {}", path));
            }
            // A float-tagged number must be integral to satisfy an integer target.
            let as_int: i128 = match tag {
                TAG_FLOAT32 | TAG_FLOAT64 => {
                    let f = tagged_as_f64(tag, payload);
                    if f.fract() != 0.0 || !f.is_finite() {
                        return Err(format!("expected an integer at {}, got a non-integral number", path));
                    }
                    f as i128
                }
                TAG_UINT64 => payload as u128 as i128,
                _ => tagged_as_f64(tag, payload) as i128,
            };
            let (lo, hi) = int_range(width, signed);
            if as_int < lo || as_int > hi {
                Err(format!("integer at {} is out of range for the target type", path))
            } else {
                Ok(())
            }
        }
        KIND_FLOAT => {
            // Any number satisfies a float target.
            if is_numeric_tag(tag) {
                Ok(())
            } else {
                Err(format!("expected a number at {}", path))
            }
        }
        KIND_ARRAY => {
            if tag != TAG_ARRAY {
                return Err(format!("expected an array at {}", path));
            }
            let elem_off = desc.u32_at(node + 1) as usize;
            let arr = lin_unbox_ptr(value) as *const LinArray;
            let len = if arr.is_null() { 0 } else { lin_array_length(arr) };
            for i in 0..len {
                let elem = crate::array::lin_array_get_tagged(arr, i);
                let base_len = path.len();
                // Append the index directly into `path` (no throwaway String — this is the hot
                // success path); the formatted location is only ever read on error.
                let _ = write!(path, "[{}]", i);
                let r = validate(elem as *const u8, desc, elem_off, path);
                // lin_array_get_tagged allocates a fresh box we own; free it.
                crate::tagged::lin_tagged_release(elem as *mut u8);
                // On error, leave `path` at the failure site for the reported location.
                r?;
                path.truncate(base_len);
            }
            Ok(())
        }
        KIND_FIXED => {
            if tag != TAG_ARRAY {
                return Err(format!("expected a fixed-length array at {}", path));
            }
            let count = desc.u32_at(node + 1) as usize;
            let arr = lin_unbox_ptr(value) as *const LinArray;
            let len = if arr.is_null() { 0 } else { lin_array_length(arr) } as usize;
            if len != count {
                return Err(format!("expected an array of length {} at {}, got length {}", count, path, len));
            }
            for i in 0..count {
                let off = desc.u32_at(node + 5 + i * 4) as usize;
                let elem = crate::array::lin_array_get_tagged(arr, i as i64);
                let base_len = path.len();
                let _ = write!(path, "[{}]", i);
                let r = validate(elem as *const u8, desc, off, path);
                crate::tagged::lin_tagged_release(elem as *mut u8);
                r?;
                path.truncate(base_len);
            }
            Ok(())
        }
        KIND_OBJECT => {
            // Accept TAG_RECORD (sealed struct, Stage 6a) and TAG_MAP (map-backed structural types).
            // Field lookup goes through lin_map_get_bytes for TAG_MAP (no temp LinString alloc).
            if tag == TAG_MAP {
                let map = (*(value as *const TaggedVal)).payload as *const crate::map::LinMap;
                let nfields = desc.u32_at(node + 1) as usize;
                let mut cur = node + 5;
                return (|| -> Result<(), String> {
                    for _ in 0..nfields {
                        let klen = desc.u16_at(cur) as usize;
                        let key = desc.str_at(cur + 2, klen);
                        let nullable = desc.u8_at(cur + 2 + klen) != 0;
                        let val_off = desc.u32_at(cur + 2 + klen + 1) as usize;
                        cur += 2 + klen + 1 + 4;
                        let field = if map.is_null() {
                            std::ptr::null()
                        } else {
                            crate::map::lin_map_get_bytes(map, key.as_ptr(), key.len() as u32)
                        };
                        let field_present = !field.is_null() && (*field).tag != TAG_NULL;
                        if !field_present {
                            if nullable { continue; }
                            return Err(format!("missing required field \"{}\" at {}", key, path));
                        }
                        let base_len = path.len();
                        path.push('.');
                        path.push_str(key);
                        let r = validate(field as *const u8, desc, val_off, path);
                        if r.is_ok() { path.truncate(base_len); }
                        r?;
                    }
                    Ok(())
                })();
            }
            // Normalize any object-shaped value to a LinMap for uniform field lookup.
            let (map, map_owned) = if tag == TAG_MAP {
                let m = (*(value as *const TaggedVal)).payload as *const crate::map::LinMap;
                (m, false)
            } else if tag == TAG_RECORD {
                let sealed = (*(value as *const TaggedVal)).payload as *mut u8;
                if sealed.is_null() {
                    return Err(format!("expected an object at {}", path));
                }
                let named_desc = *((sealed.add(16)) as *const *const u8);
                let m = crate::sealed::materialize_sealed_to_map_pub(sealed, named_desc);
                if m.is_null() {
                    return Err(format!("expected an object at {}", path));
                }
                (m as *const crate::map::LinMap, true)
            } else {
                return Err(format!("expected an object at {}", path));
            };
            let nfields = desc.u32_at(node + 1) as usize;
            // Walk the variable-length field rows.
            let mut cur = node + 5;
            let result = (|| -> Result<(), String> {
                for _ in 0..nfields {
                    let klen = desc.u16_at(cur) as usize;
                    let key = desc.str_at(cur + 2, klen);
                    let nullable = desc.u8_at(cur + 2 + klen) != 0;
                    let val_off = desc.u32_at(cur + 2 + klen + 1) as usize;
                    cur += 2 + klen + 1 + 4;

                    // Look up by raw key bytes — no temp LinString malloc/free per field.
                    let field = if map.is_null() {
                        std::ptr::null()
                    } else {
                        crate::map::lin_map_get_bytes(map, key.as_ptr(), key.len() as u32)
                    };

                    let field_present = !field.is_null() && (*(field as *const TaggedVal)).tag != TAG_NULL;
                    if !field_present {
                        if nullable {
                            continue;
                        }
                        return Err(format!("missing required field \"{}\" at {}", key, path));
                    }
                    let base_len = path.len();
                    path.push('.');
                    path.push_str(key);
                    let r = validate(field as *const u8, desc, val_off, path);
                    if r.is_ok() {
                        path.truncate(base_len);
                    }
                    r?;
                }
                Ok(())
            })();
            if map_owned {
                crate::map::lin_map_release(map as *mut crate::map::LinMap);
            }
            result
        }
        KIND_UNION => {
            let nvariants = desc.u32_at(node + 1) as usize;
            // First structurally-matching variant wins (ADR-031). Probe each in order against the
            // SHARED path, restoring its length after each probe so a failed variant's descent
            // doesn't pollute the reported path — same save-len/truncate pattern used for
            // object/array fields (avoids cloning the whole path per variant).
            let base_len = path.len();
            for i in 0..nvariants {
                let off = desc.u32_at(node + 5 + i * 4) as usize;
                if validate(value, desc, off, path).is_ok() {
                    path.truncate(base_len);
                    return Ok(());
                }
                path.truncate(base_len);
            }
            Err(format!("value at {} matched none of the expected variants", path))
        }
        _ => Ok(()),
    }
}

/// Decode/validate `value` (a Json `TaggedVal*`) against the schema `desc`. Returns an owned
/// clone of `value` on success, or a fresh `Error` object on the first mismatch (ADR-031). The
/// input is borrowed (never consumed).
#[no_mangle]
pub unsafe extern "C" fn lin_from_json(value: *const u8, desc: *const u8) -> *mut u8 {
    let d = Desc { base: desc };
    let mut path = String::from("$");
    match validate(value, &d, 0, &mut path) {
        Ok(()) => lin_tagged_clone(value),
        Err(msg) => make_decode_error(&msg, &path),
    }
}

/// Deep structural type test for `is <ObjectType>` (ADR-036). Runs the SAME validator the
/// `fromJson` decoder uses (`validate`) against the schema descriptor `desc` and returns
/// `1` when `value` fully conforms to the target type (recursively, with fromJson's number
/// policy), `0` otherwise. The input is borrowed (never cloned/consumed); the descriptor is a
/// static const global. The mismatch error string is discarded (cold path).
#[no_mangle]
pub unsafe extern "C" fn lin_matches_schema(value: *const u8, desc: *const u8) -> u8 {
    let d = Desc { base: desc };
    let mut p = String::new();
    validate(value, &d, 0, &mut p).is_ok() as u8
}
