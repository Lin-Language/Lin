//! `std/crypto` runtime support: security-grade hashing (SHA-256/512/1, MD5), HMAC-SHA-256,
//! the OS CSPRNG (`randomBytes`), UUID v4/v7, constant-time byte comparison, hex/utf8 codecs, and
//! an opaque incremental `Hasher` handle.
//!
//! These primitives cannot be written in Lin (block-compression rotates, OS entropy pool); the
//! whole module lowers here. Buffer arguments and results are the runtime's packed `UInt8[]`
//! representation (a flat `LinArray` with `elem_tag == TAG_UINT8`), declared `Json` on the foreign
//! side and annotated `UInt8[]` in the Lin wrapper. `=> String` returns are a bare `LinString*`
//! (cf. `lin_process_cwd`), NOT a boxed `TaggedVal(Str)`. Errors use the canonical
//! `{ "type":"error", "message":... }` tagged object.
//!
//! The `Hasher` is an opaque handle implemented exactly like `std/process`' `ProcessHandle`:
//! `lin_crypto_hasher_new` returns a boxed `Int64` id (or an error object); the other hasher
//! intrinsics take the raw `i64` id and look it up in a process-global registry. This avoids
//! handing a raw pointer across the FFI boundary and reuses the proven, ASan-clean registry
//! pattern (no `Box::into_raw`/`from_raw` lifetime hazards).

use crate::array::{lin_array_length, lin_flat_array_alloc_u8, lin_flat_array_push_u8, LinArray};
use crate::fs::{make_error_tagged, make_string, resolve_lin_str};
use crate::tagged::{
    alloc_tagged, lin_box_bool, lin_box_int64, TaggedVal, TAG_ARRAY, TAG_INT8, TAG_UINT8,
};

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

use digest::Digest;
use hmac::{Hmac, Mac};
use md5::Md5;
use sha1::Sha1;
use sha2::{Sha256, Sha512};

// ---------------------------------------------------------------------------
// Buffer ABI helpers
// ---------------------------------------------------------------------------

/// Read a `UInt8[]` argument (`TaggedVal*(Array)` or raw `LinArray*`) into a `Vec<u8>`.
/// Mirrors `lin_fs_write_file_bytes`' unpacking: a flat 1-byte array is read straight from its
/// data buffer; any other shape falls back to the element-by-element path (truncating to u8).
/// A null pointer yields an empty vec.
unsafe fn read_byte_buf(arr: *const u8) -> Vec<u8> {
    if arr.is_null() {
        return Vec::new();
    }
    let tag = *arr;
    let lin_arr = if tag == TAG_ARRAY {
        let tv = arr as *const TaggedVal;
        (*tv).payload as *const LinArray
    } else {
        arr as *const LinArray
    };
    if lin_arr.is_null() {
        return Vec::new();
    }
    let len = lin_array_length(lin_arr) as usize;
    let elem_tag = (*lin_arr).elem_tag;
    let mut bytes = Vec::with_capacity(len);
    if elem_tag == TAG_UINT8 || elem_tag == TAG_INT8 {
        let data = (*lin_arr).data as *const u8;
        for i in 0..len {
            bytes.push(*data.add(i));
        }
    } else {
        // Fallback for tagged / other-width arrays (e.g. a Json array of small ints): box each
        // element, take its low byte, and free the transient box (matches fs.rs).
        for i in 0..len as i64 {
            let tv_ptr = crate::array::lin_array_get_tagged(lin_arr, i);
            let v = if tv_ptr.is_null() {
                0u8
            } else {
                let payload = (*tv_ptr).payload;
                std::alloc::dealloc(
                    tv_ptr as *mut u8,
                    std::alloc::Layout::new::<TaggedVal>(),
                );
                payload as u8
            };
            bytes.push(v);
        }
    }
    bytes
}

/// Build an owned `TaggedVal*(Array)` packed `UInt8[]` from a byte slice.
unsafe fn make_byte_buf(bytes: &[u8]) -> *mut u8 {
    let arr = lin_flat_array_alloc_u8(bytes.len().max(4) as u64);
    for b in bytes {
        lin_flat_array_push_u8(arr, *b);
    }
    alloc_tagged(TAG_ARRAY, arr as u64)
}

/// Build an owned raw `LinString*` for a `=> String` foreign return (codegen expects the bare
/// string pointer, not a boxed `TaggedVal(Str)` — cf. `lin_process_cwd` / `lin_time_to_iso`).
unsafe fn make_str_raw(s: &str) -> *mut u8 {
    make_string(s) as *mut u8
}

// ---------------------------------------------------------------------------
// One-shot digests (raw 32/64/20/16-byte buffers)
// ---------------------------------------------------------------------------

// The lowercase-hex digest variants (sha256Hex/…/hmacSha256Hex/digestHex) and the hex/UTF-8 codecs
// (toHex/fromHex/toBytes) live in std/crypto as pure Lin composed over std/encoding's hexEncode /
// hexDecode / utf8Bytes — they are NOT runtime intrinsics, to avoid duplicating that plumbing here.

#[no_mangle]
pub unsafe extern "C" fn lin_crypto_sha256(data: *const u8) -> *mut u8 {
    let d = Sha256::digest(read_byte_buf(data));
    make_byte_buf(&d)
}

#[no_mangle]
pub unsafe extern "C" fn lin_crypto_sha512(data: *const u8) -> *mut u8 {
    let d = Sha512::digest(read_byte_buf(data));
    make_byte_buf(&d)
}

#[no_mangle]
pub unsafe extern "C" fn lin_crypto_sha1(data: *const u8) -> *mut u8 {
    let d = Sha1::digest(read_byte_buf(data));
    make_byte_buf(&d)
}

#[no_mangle]
pub unsafe extern "C" fn lin_crypto_md5(data: *const u8) -> *mut u8 {
    let d = Md5::digest(read_byte_buf(data));
    make_byte_buf(&d)
}

// ---------------------------------------------------------------------------
// HMAC-SHA-256
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn lin_crypto_hmac_sha256(key: *const u8, msg: *const u8) -> *mut u8 {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&read_byte_buf(key))
        .expect("HMAC accepts any key length");
    mac.update(&read_byte_buf(msg));
    make_byte_buf(&mac.finalize().into_bytes())
}

// ---------------------------------------------------------------------------
// CSPRNG / UUID
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn lin_crypto_random_bytes(n: i32) -> *mut u8 {
    let n = n.max(0) as usize;
    let mut buf = vec![0u8; n];
    if n > 0 {
        // getrandom never fails on a healthy OS; on the (theoretical) failure path return an empty
        // buffer rather than abort, so a misbehaving sandbox cannot crash the program.
        if getrandom::getrandom(&mut buf).is_err() {
            return make_byte_buf(&[]);
        }
    }
    make_byte_buf(&buf)
}

#[no_mangle]
pub unsafe extern "C" fn lin_crypto_uuid_v4() -> *mut u8 {
    make_str_raw(&uuid::Uuid::new_v4().to_string())
}

#[no_mangle]
pub unsafe extern "C" fn lin_crypto_uuid_v7() -> *mut u8 {
    make_str_raw(&uuid::Uuid::now_v7().to_string())
}

// ---------------------------------------------------------------------------
// Constant-time compare
// ---------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn lin_crypto_ct_eq(a: *const u8, b: *const u8) -> *mut u8 {
    let av = read_byte_buf(a);
    let bv = read_byte_buf(b);
    // Differing lengths are not secret in these protocols → return false immediately.
    if av.len() != bv.len() {
        return lin_box_bool(false as u8);
    }
    use subtle::ConstantTimeEq;
    let eq: bool = av.ct_eq(&bv).into();
    lin_box_bool(eq as u8)
}

// (hex/UTF-8 codecs — toHex/fromHex/toBytes — are pure Lin in std/crypto over std/encoding.)

// ---------------------------------------------------------------------------
// Incremental Hasher (opaque Int64 handle backed by a global registry)
// ---------------------------------------------------------------------------

enum HasherState {
    Sha256(Box<Sha256>),
    Sha512(Box<Sha512>),
    Sha1(Box<Sha1>),
    Md5(Box<Md5>),
}

impl HasherState {
    fn update(&mut self, data: &[u8]) {
        match self {
            HasherState::Sha256(h) => h.update(data),
            HasherState::Sha512(h) => h.update(data),
            HasherState::Sha1(h) => h.update(data),
            HasherState::Md5(h) => h.update(data),
        }
    }
    fn finalize(self) -> Vec<u8> {
        match self {
            HasherState::Sha256(h) => h.finalize().to_vec(),
            HasherState::Sha512(h) => h.finalize().to_vec(),
            HasherState::Sha1(h) => h.finalize().to_vec(),
            HasherState::Md5(h) => h.finalize().to_vec(),
        }
    }
}

fn registry() -> &'static Mutex<HashMap<i64, HasherState>> {
    use std::sync::OnceLock;
    static REG: OnceLock<Mutex<HashMap<i64, HasherState>>> = OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

static NEXT_HASHER_ID: AtomicI64 = AtomicI64::new(1);

fn make_hasher_state(algorithm: &str) -> Option<HasherState> {
    match algorithm.to_ascii_lowercase().as_str() {
        "sha256" => Some(HasherState::Sha256(Box::new(Sha256::new()))),
        "sha512" => Some(HasherState::Sha512(Box::new(Sha512::new()))),
        "sha1" => Some(HasherState::Sha1(Box::new(Sha1::new()))),
        "md5" => Some(HasherState::Md5(Box::new(Md5::new()))),
        _ => None,
    }
}

/// newHasher(algorithm) -> Int64 handle (boxed) | Error.
#[no_mangle]
pub unsafe extern "C" fn lin_crypto_hasher_new(algorithm: *const u8) -> *mut u8 {
    let algo = match resolve_lin_str(algorithm) {
        Some(s) => s,
        None => return make_error_tagged("invalid algorithm name"),
    };
    let state = match make_hasher_state(&algo) {
        Some(s) => s,
        None => return make_error_tagged(&format!("unknown hash algorithm: {algo}")),
    };
    let id = NEXT_HASHER_ID.fetch_add(1, Ordering::SeqCst);
    registry().lock().unwrap().insert(id, state);
    lin_box_int64(id)
}

/// update(handle, data) -> handle. Feeds bytes into the running digest; a finalised/unknown
/// handle is a no-op (digest already consumed the state).
#[no_mangle]
pub unsafe extern "C" fn lin_crypto_hasher_update(handle: i64, data: *const u8) -> i64 {
    let bytes = read_byte_buf(data);
    if let Some(state) = registry().lock().unwrap().get_mut(&handle) {
        state.update(&bytes);
    }
    handle
}

/// digest(handle) -> UInt8[]. Finalises (removes the state from the registry); a finalised /
/// unknown handle yields an empty buffer.
#[no_mangle]
pub unsafe extern "C" fn lin_crypto_hasher_digest(handle: i64) -> *mut u8 {
    let state = registry().lock().unwrap().remove(&handle);
    match state {
        Some(s) => make_byte_buf(&s.finalize()),
        None => make_byte_buf(&[]),
    }
}
// (digestHex is pure Lin in std/crypto: hexEncode(digest(h)).)
