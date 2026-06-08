//! `std/bignum` runtime support: arbitrary-precision signed integers (`BigInt`) backed by
//! `num_bigint::BigInt`.
//!
//! A `BigInt` is an **opaque, immutable, refcounted heap handle** in the `Timer`/`Stream`/`Shared`
//! family (bignum-decimal proposal §"Opaque-handle representation"). The Rust value lives in a
//! heap-allocated `BigNumBox` (an `AtomicU32` refcount + the `BigInt`), wrapped in a
//! `TaggedVal*(TAG_BIGNUM)` so it flows through the universal boxed-value representation. Its RC
//! dispatches through the tag-aware `lin_tagged_retain`/`lin_tagged_release` (whose `TAG_BIGNUM`
//! arms call `lin_bignum_retain_box`/`lin_bignum_release_box`); the final drop frees the box.
//!
//! Ownership contract (the recurring UAF/double-free class — verify under ASan):
//!   * Every operation borrows its `BigInt` argument(s) (no retain) and returns a FRESH +1-owned
//!     handle, exactly the standard owned-call-result contract. The arithmetic never mutates in
//!     place (values are immutable).
//!   * Fallible operations (`parse`, `div`, `mod`, `pow`, `modPow`, `toInt64`) return the canonical
//!     `{ "type": "error", "message": … }` tagged object on failure, discriminated by `is Error`.
//!
//! Cross-thread transfer is DEFERRED (proposal §"Opaque-handle"): a `BigInt` is a raw pointer that
//! cannot be shared across a worker boundary, so the deep-copy thread-transfer path does not wire
//! `TAG_BIGNUM`. Sending one to a worker is unsupported for v1 (money/bignum math is main-thread).

use crate::fs::make_error_tagged;
use crate::string::{LinString, lin_string_from_bytes};
use crate::tagged::{alloc_tagged, TaggedVal, TAG_BIGNUM};

use num_bigint::BigInt;
use num_traits::{Signed, ToPrimitive, Zero, One};
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};

/// The heap box behind a `BigInt` value. Owns the `num_bigint::BigInt` and an atomic refcount.
/// Atomic (like `StreamBox`/`SharedBox`) purely as defence against a stray cross-thread touch —
/// the cost is one box, not per-op.
pub struct BigNumBox {
    rc: AtomicU32,
    val: BigInt,
}

impl BigNumBox {
    /// Allocate a `BigNumBox` over `val`, boxed into a `TaggedVal*(TAG_BIGNUM)` with refcount 1.
    /// The returned pointer is a fresh +1-owned handle.
    unsafe fn new_boxed(val: BigInt) -> *mut u8 {
        let b = Box::into_raw(Box::new(BigNumBox { rc: AtomicU32::new(1), val }));
        alloc_tagged(TAG_BIGNUM, b as u64)
    }
}

/// Extract a `&BigInt` from a boxed `BigInt` value (TAG_BIGNUM). Returns `None` for null / wrong tag.
unsafe fn borrow(p: *const u8) -> Option<&'static BigInt> {
    if p.is_null() {
        return None;
    }
    let tv = &*(p as *const TaggedVal);
    if tv.tag != TAG_BIGNUM {
        return None;
    }
    let b = tv.payload as *const BigNumBox;
    if b.is_null() {
        return None;
    }
    Some(&(*b).val)
}

/// Render the value behind a raw `*const BigNumBox` payload as a base-10 `String`. Used by the
/// universal display path (`lin_tagged_to_string`) so accidental interpolation is not `[object]`.
pub unsafe fn bignum_render(payload: *const u8) -> *mut LinString {
    let b = payload as *const BigNumBox;
    if b.is_null() {
        return lin_string_from_bytes(b"0".as_ptr(), 1);
    }
    let s = (*b).val.to_string();
    lin_string_from_bytes(s.as_ptr(), s.len() as u32)
}

// ----------------------------------------------------------------------------------------------
// Construction
// ----------------------------------------------------------------------------------------------

/// `bigInt(n)` — exact widen from an `Int64`.
#[no_mangle]
pub unsafe extern "C" fn lin_bignum_from_int64(n: i64) -> *mut u8 {
    BigNumBox::new_boxed(BigInt::from(n))
}

/// `parseBigInt(s)` — base-10 parse; `{ type:"error" }` on malformed input.
#[no_mangle]
pub unsafe extern "C" fn lin_bignum_parse(s: *const u8) -> *mut u8 {
    let txt = match crate::fs::resolve_lin_str(s) {
        Some(t) => t,
        None => return make_error_tagged("parseBigInt: invalid string"),
    };
    let trimmed = txt.trim();
    match BigInt::from_str(trimmed) {
        Ok(v) => BigNumBox::new_boxed(v),
        Err(_) => make_error_tagged(&format!("parseBigInt: not a base-10 integer: \"{txt}\"")),
    }
}

/// `zero` — the constant 0.
#[no_mangle]
pub unsafe extern "C" fn lin_bignum_zero() -> *mut u8 {
    BigNumBox::new_boxed(BigInt::zero())
}

/// `one` — the constant 1.
#[no_mangle]
pub unsafe extern "C" fn lin_bignum_one() -> *mut u8 {
    BigNumBox::new_boxed(BigInt::one())
}

// ----------------------------------------------------------------------------------------------
// Arithmetic (each borrows its args, returns a fresh +1 handle)
// ----------------------------------------------------------------------------------------------

/// Apply a binary op over two borrowed BigInts; non-bignum args fault to an Error.
unsafe fn binop(a: *const u8, b: *const u8, name: &str, f: impl FnOnce(&BigInt, &BigInt) -> BigInt) -> *mut u8 {
    match (borrow(a), borrow(b)) {
        (Some(x), Some(y)) => BigNumBox::new_boxed(f(x, y)),
        _ => make_error_tagged(&format!("{name}: argument is not a BigInt")),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_bignum_add(a: *const u8, b: *const u8) -> *mut u8 {
    binop(a, b, "add", |x, y| x + y)
}

#[no_mangle]
pub unsafe extern "C" fn lin_bignum_sub(a: *const u8, b: *const u8) -> *mut u8 {
    binop(a, b, "sub", |x, y| x - y)
}

#[no_mangle]
pub unsafe extern "C" fn lin_bignum_mul(a: *const u8, b: *const u8) -> *mut u8 {
    binop(a, b, "mul", |x, y| x * y)
}

/// `div(a, b)` — truncating integer division. `Error` on divide-by-zero.
#[no_mangle]
pub unsafe extern "C" fn lin_bignum_div(a: *const u8, b: *const u8) -> *mut u8 {
    match (borrow(a), borrow(b)) {
        (Some(x), Some(y)) => {
            if y.is_zero() {
                return make_error_tagged("div: division by zero");
            }
            BigNumBox::new_boxed(x / y)
        }
        _ => make_error_tagged("div: argument is not a BigInt"),
    }
}

/// `mod(a, b)` — remainder matching truncating division. `Error` on divide-by-zero.
#[no_mangle]
pub unsafe extern "C" fn lin_bignum_mod(a: *const u8, b: *const u8) -> *mut u8 {
    match (borrow(a), borrow(b)) {
        (Some(x), Some(y)) => {
            if y.is_zero() {
                return make_error_tagged("mod: division by zero");
            }
            BigNumBox::new_boxed(x % y)
        }
        _ => make_error_tagged("mod: argument is not a BigInt"),
    }
}

/// `pow(a, e)` — raise to an `Int64` exponent. `Error` if `e < 0`.
#[no_mangle]
pub unsafe extern "C" fn lin_bignum_pow(a: *const u8, e: i64) -> *mut u8 {
    let x = match borrow(a) {
        Some(x) => x,
        None => return make_error_tagged("pow: argument is not a BigInt"),
    };
    if e < 0 {
        return make_error_tagged("pow: negative exponent");
    }
    BigNumBox::new_boxed(x.pow(e as u32))
}

/// `modPow(base, exp, modulus)` — `base^exp mod modulus` without materialising `base^exp`.
/// `Error` if `modulus` is zero or `exp` is negative.
#[no_mangle]
pub unsafe extern "C" fn lin_bignum_modpow(base: *const u8, exp: *const u8, modulus: *const u8) -> *mut u8 {
    match (borrow(base), borrow(exp), borrow(modulus)) {
        (Some(b), Some(e), Some(m)) => {
            if m.is_zero() {
                return make_error_tagged("modPow: modulus is zero");
            }
            if e.is_negative() {
                return make_error_tagged("modPow: negative exponent");
            }
            BigNumBox::new_boxed(b.modpow(e, m))
        }
        _ => make_error_tagged("modPow: argument is not a BigInt"),
    }
}

/// `neg(a)` — additive inverse.
#[no_mangle]
pub unsafe extern "C" fn lin_bignum_neg(a: *const u8) -> *mut u8 {
    match borrow(a) {
        Some(x) => BigNumBox::new_boxed(-x),
        None => make_error_tagged("neg: argument is not a BigInt"),
    }
}

/// `abs(a)` — absolute value.
#[no_mangle]
pub unsafe extern "C" fn lin_bignum_abs(a: *const u8) -> *mut u8 {
    match borrow(a) {
        Some(x) => BigNumBox::new_boxed(x.abs()),
        None => make_error_tagged("abs: argument is not a BigInt"),
    }
}

// ----------------------------------------------------------------------------------------------
// Comparison (scalar returns: raw i32 / i32-bool)
// ----------------------------------------------------------------------------------------------

/// `cmp(a, b)` — -1 / 0 / 1. (Conveniences eq/lt/… are pure-Lin over this.)
#[no_mangle]
pub unsafe extern "C" fn lin_bignum_cmp(a: *const u8, b: *const u8) -> i32 {
    match (borrow(a), borrow(b)) {
        (Some(x), Some(y)) => match x.cmp(y) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        },
        _ => 0,
    }
}

/// `sign(a)` — -1, 0, or 1.
#[no_mangle]
pub unsafe extern "C" fn lin_bignum_sign(a: *const u8) -> i32 {
    match borrow(a) {
        Some(x) => {
            if x.is_zero() {
                0
            } else if x.is_negative() {
                -1
            } else {
                1
            }
        }
        None => 0,
    }
}

// ----------------------------------------------------------------------------------------------
// Conversion
// ----------------------------------------------------------------------------------------------

/// `toString(a)` — base-10 with a leading `-` if negative. Returns a bare `LinString*` (the
/// `=> String` foreign ABI: NOT a boxed TaggedVal).
#[no_mangle]
pub unsafe extern "C" fn lin_bignum_to_string(a: *const u8) -> *mut LinString {
    match borrow(a) {
        Some(x) => {
            let s = x.to_string();
            lin_string_from_bytes(s.as_ptr(), s.len() as u32)
        }
        None => lin_string_from_bytes(b"0".as_ptr(), 1),
    }
}

/// `toInt64(a)` — `Int64 | Error` (Error if out of `Int64` range). Returns a boxed TaggedVal so the
/// success arm is a boxed Int64 and the failure arm is the Error object (the union is `Json`).
#[no_mangle]
pub unsafe extern "C" fn lin_bignum_to_int64(a: *const u8) -> *mut u8 {
    match borrow(a) {
        Some(x) => match x.to_i64() {
            Some(n) => crate::tagged::lin_box_int64(n),
            None => make_error_tagged("toInt64: value out of Int64 range"),
        },
        None => make_error_tagged("toInt64: argument is not a BigInt"),
    }
}

/// `toFloat64(a)` — LOSSY past 2^53; always succeeds. Returns a raw `f64` (the `=> Float64` ABI).
#[no_mangle]
pub unsafe extern "C" fn lin_bignum_to_float64(a: *const u8) -> f64 {
    match borrow(a) {
        Some(x) => x.to_f64().unwrap_or(f64::INFINITY),
        None => 0.0,
    }
}

// ----------------------------------------------------------------------------------------------
// RC box primitives (tag-aware retain/release dispatch lands here)
// ----------------------------------------------------------------------------------------------

/// Atomic retain given the RAW `*const BigNumBox` payload. Called from the `TAG_BIGNUM` arm of the
/// tag-aware retain path. Null-safe.
#[no_mangle]
pub unsafe extern "C" fn lin_bignum_retain_box(b: *const u8) {
    let b = b as *const BigNumBox;
    if !b.is_null() {
        (*b).rc.fetch_add(1, Ordering::Relaxed);
    }
}

/// Atomic release given the RAW `*const BigNumBox` payload. The last reference frees the box (and
/// its `BigInt`). Acquire/Release fences make the final drop see all prior writes. Null-safe.
#[no_mangle]
pub unsafe extern "C" fn lin_bignum_release_box(b: *const u8) {
    let b = b as *const BigNumBox;
    if b.is_null() {
        return;
    }
    if (*b).rc.fetch_sub(1, Ordering::Release) == 1 {
        std::sync::atomic::fence(Ordering::Acquire);
        drop(Box::from_raw(b as *mut BigNumBox));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factorial_and_roundtrip() {
        unsafe {
            // 5! = 120 via repeated mul; then parse/toString round-trip.
            let mut acc = lin_bignum_one();
            for i in 1..=5i64 {
                let bi = lin_bignum_from_int64(i);
                let next = lin_bignum_mul(acc, bi);
                crate::tagged::lin_tagged_release(acc);
                crate::tagged::lin_tagged_release(bi);
                acc = next;
            }
            let s = lin_bignum_to_string(acc);
            assert_eq!((*s).as_str(), "120");
            crate::string::lin_string_release(s);
            crate::tagged::lin_tagged_release(acc);
        }
    }

    #[test]
    fn div_by_zero_is_error() {
        unsafe {
            let a = lin_bignum_from_int64(7);
            let z = lin_bignum_zero();
            let r = lin_bignum_div(a, z);
            // An Error object is TAG_OBJECT, not TAG_BIGNUM.
            assert_eq!(crate::tagged::lin_get_tag(r), crate::tagged::TAG_OBJECT);
            crate::tagged::lin_tagged_release(a);
            crate::tagged::lin_tagged_release(z);
            crate::tagged::lin_tagged_release(r);
        }
    }

    #[test]
    fn modpow_matches() {
        unsafe {
            // 4^13 mod 497 == 445 (classic RSA worked example).
            let b = lin_bignum_from_int64(4);
            let e = lin_bignum_from_int64(13);
            let m = lin_bignum_from_int64(497);
            let r = lin_bignum_modpow(b, e, m);
            let expected = lin_bignum_from_int64(445);
            assert_eq!(lin_bignum_cmp(r, expected), 0);
            crate::tagged::lin_tagged_release(b);
            crate::tagged::lin_tagged_release(e);
            crate::tagged::lin_tagged_release(m);
            crate::tagged::lin_tagged_release(r);
            crate::tagged::lin_tagged_release(expected);
        }
    }
}
