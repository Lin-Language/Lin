//! `std/decimal` runtime support: exact base-10 fixed-point arithmetic (`Decimal`) backed by
//! `rust_decimal::Decimal` (a fixed 96-bit coefficient, 28–29 significant digits — far more than
//! money needs, per the proposal's v1 recommendation).
//!
//! A `Decimal` is an **opaque, immutable, refcounted heap handle** in the exact same shape as the
//! `BigInt` handle (`crate::bignum`): a heap `DecimalBox` (`AtomicU32` refcount + the `Decimal`),
//! wrapped in a `TaggedVal*(TAG_DECIMAL)`. RC dispatches through the tag-aware retain/release
//! (`lin_decimal_retain_box`/`lin_decimal_release_box`). Every op borrows its args and returns a
//! fresh +1-owned handle; fallible ops return the canonical `{ type:"error" }` object.
//!
//! `add`/`sub`/`mul` are ALWAYS exact (the result scale is wide enough to hold the true value);
//! only `div`, `round`, and `setScale` round, and they REQUIRE an explicit rounding mode + scale —
//! there is no silent default (the proposal's central money-safety property).
//!
//! Rounding-mode codes (the Lin module exposes these as opaque constants):
//!   0 = RoundHalfUp   (MidpointAwayFromZero)
//!   1 = RoundHalfEven (MidpointNearestEven, banker's)
//!   2 = RoundFloor    (toward -inf)
//!   3 = RoundCeil     (toward +inf)
//!   4 = RoundDown     (toward zero, truncate)
//!
//! Cross-thread transfer is DEFERRED (same rationale as bignum): `TAG_DECIMAL` is not wired into
//! the worker deep-copy path; sending a `Decimal` to a worker is unsupported for v1.

use crate::fs::make_error_tagged;
use crate::string::{LinString, lin_string_from_bytes};
use crate::tagged::{alloc_tagged, TaggedVal, TAG_DECIMAL};

use rust_decimal::{Decimal, RoundingStrategy};
use std::str::FromStr;
use std::sync::atomic::{AtomicU32, Ordering};

/// The heap box behind a `Decimal` value. Owns the `rust_decimal::Decimal` + an atomic refcount.
pub struct DecimalBox {
    rc: AtomicU32,
    val: Decimal,
}

impl DecimalBox {
    unsafe fn new_boxed(val: Decimal) -> *mut u8 {
        let b = Box::into_raw(Box::new(DecimalBox { rc: AtomicU32::new(1), val }));
        alloc_tagged(TAG_DECIMAL, b as u64)
    }
}

unsafe fn borrow(p: *const u8) -> Option<Decimal> {
    if p.is_null() {
        return None;
    }
    let tv = &*(p as *const TaggedVal);
    if tv.tag != TAG_DECIMAL {
        return None;
    }
    let b = tv.payload as *const DecimalBox;
    if b.is_null() {
        return None;
    }
    Some((*b).val)
}

/// Map a Lin rounding-mode code to a `rust_decimal::RoundingStrategy`. Unknown codes default to
/// banker's rounding (the money-safe default).
fn strategy(mode: i32) -> RoundingStrategy {
    match mode {
        0 => RoundingStrategy::MidpointAwayFromZero,
        1 => RoundingStrategy::MidpointNearestEven,
        2 => RoundingStrategy::ToNegativeInfinity,
        3 => RoundingStrategy::ToPositiveInfinity,
        4 => RoundingStrategy::ToZero,
        _ => RoundingStrategy::MidpointNearestEven,
    }
}

/// Render the value behind a raw `*const DecimalBox` payload as exact, scale-preserving base-10.
/// Used by the universal display path so accidental interpolation is not `[object]`.
pub unsafe fn decimal_render(payload: *const u8) -> *mut LinString {
    let b = payload as *const DecimalBox;
    if b.is_null() {
        return lin_string_from_bytes(b"0".as_ptr(), 1);
    }
    let s = (*b).val.to_string();
    lin_string_from_bytes(s.as_ptr(), s.len() as u32)
}

// ----------------------------------------------------------------------------------------------
// Construction
// ----------------------------------------------------------------------------------------------

/// `decimal(s)` — exact base-10 parse, preserving the WRITTEN scale (`"1.50"` keeps scale 2).
/// `{ type:"error" }` on malformed input. The preferred, exact constructor for fractional values.
#[no_mangle]
pub unsafe extern "C" fn lin_decimal_parse(s: *const u8) -> *mut u8 {
    let txt = match crate::fs::resolve_lin_str(s) {
        Some(t) => t,
        None => return make_error_tagged("decimal: invalid string"),
    };
    let trimmed = txt.trim();
    // `from_str_exact` preserves the written scale (trailing zeros) and supports e-notation.
    match Decimal::from_str_exact(trimmed).or_else(|_| Decimal::from_str(trimmed)) {
        Ok(v) => DecimalBox::new_boxed(v),
        Err(_) => make_error_tagged(&format!("decimal: not a base-10 numeral: \"{txt}\"")),
    }
}

/// `fromInt(n)` — exact; scale 0.
#[no_mangle]
pub unsafe extern "C" fn lin_decimal_from_int64(n: i64) -> *mut u8 {
    DecimalBox::new_boxed(Decimal::from(n))
}

/// `fromFloat(f)` — LOSSY (a Float64 already carries binary rounding error). For money, never use.
#[no_mangle]
pub unsafe extern "C" fn lin_decimal_from_float64(f: f64) -> *mut u8 {
    match Decimal::from_f64_retain(f) {
        Some(v) => DecimalBox::new_boxed(v),
        None => make_error_tagged("fromFloat: value not representable as Decimal"),
    }
}

/// `zero` / `one` constants.
#[no_mangle]
pub unsafe extern "C" fn lin_decimal_zero() -> *mut u8 {
    DecimalBox::new_boxed(Decimal::ZERO)
}

#[no_mangle]
pub unsafe extern "C" fn lin_decimal_one() -> *mut u8 {
    DecimalBox::new_boxed(Decimal::ONE)
}

// ----------------------------------------------------------------------------------------------
// Arithmetic (exact)
// ----------------------------------------------------------------------------------------------

unsafe fn binop(a: *const u8, b: *const u8, name: &str, f: impl FnOnce(Decimal, Decimal) -> Decimal) -> *mut u8 {
    match (borrow(a), borrow(b)) {
        (Some(x), Some(y)) => DecimalBox::new_boxed(f(x, y)),
        _ => make_error_tagged(&format!("{name}: argument is not a Decimal")),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_decimal_add(a: *const u8, b: *const u8) -> *mut u8 {
    binop(a, b, "add", |x, y| x + y)
}

#[no_mangle]
pub unsafe extern "C" fn lin_decimal_sub(a: *const u8, b: *const u8) -> *mut u8 {
    binop(a, b, "sub", |x, y| x - y)
}

#[no_mangle]
pub unsafe extern "C" fn lin_decimal_mul(a: *const u8, b: *const u8) -> *mut u8 {
    binop(a, b, "mul", |x, y| x * y)
}

/// `div(a, b, scale, mode)` — the ONLY rounding arithmetic. `Error` on divide-by-zero.
#[no_mangle]
pub unsafe extern "C" fn lin_decimal_div(a: *const u8, b: *const u8, scale: i32, mode: i32) -> *mut u8 {
    match (borrow(a), borrow(b)) {
        (Some(x), Some(y)) => {
            if y.is_zero() {
                return make_error_tagged("div: division by zero");
            }
            let q = x / y;
            let scale = scale.max(0) as u32;
            DecimalBox::new_boxed(q.round_dp_with_strategy(scale, strategy(mode)))
        }
        _ => make_error_tagged("div: argument is not a Decimal"),
    }
}

/// `pow(a, e)` — non-negative `Int32` power, exact (scale multiplies out). `Error` if `e < 0` or
/// the result overflows the 96-bit coefficient.
#[no_mangle]
pub unsafe extern "C" fn lin_decimal_pow(a: *const u8, e: i32) -> *mut u8 {
    let x = match borrow(a) {
        Some(x) => x,
        None => return make_error_tagged("pow: argument is not a Decimal"),
    };
    if e < 0 {
        return make_error_tagged("pow: negative exponent");
    }
    let mut acc = Decimal::ONE;
    for _ in 0..e {
        acc = match acc.checked_mul(x) {
            Some(v) => v,
            None => return make_error_tagged("pow: result overflows Decimal precision"),
        };
    }
    DecimalBox::new_boxed(acc)
}

/// `neg(a)` / `abs(a)`.
#[no_mangle]
pub unsafe extern "C" fn lin_decimal_neg(a: *const u8) -> *mut u8 {
    match borrow(a) {
        Some(x) => DecimalBox::new_boxed(-x),
        None => make_error_tagged("neg: argument is not a Decimal"),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_decimal_abs(a: *const u8) -> *mut u8 {
    match borrow(a) {
        Some(x) => DecimalBox::new_boxed(x.abs()),
        None => make_error_tagged("abs: argument is not a Decimal"),
    }
}

// ----------------------------------------------------------------------------------------------
// Rounding and scale
// ----------------------------------------------------------------------------------------------

/// `round(d, scale, mode)` — round to `scale` fractional digits using `mode`.
#[no_mangle]
pub unsafe extern "C" fn lin_decimal_round(d: *const u8, scale: i32, mode: i32) -> *mut u8 {
    match borrow(d) {
        Some(x) => {
            let scale = scale.max(0) as u32;
            DecimalBox::new_boxed(x.round_dp_with_strategy(scale, strategy(mode)))
        }
        None => make_error_tagged("round: argument is not a Decimal"),
    }
}

/// `setScale(d, scale, mode)` — force exactly `scale` decimal places: padding with zeros when
/// increasing scale (exact), rounding with `mode` when decreasing.
#[no_mangle]
pub unsafe extern "C" fn lin_decimal_set_scale(d: *const u8, scale: i32, mode: i32) -> *mut u8 {
    match borrow(d) {
        Some(x) => {
            let target = scale.max(0) as u32;
            // Round to the target scale first (handles a DECREASE), then rescale to PAD trailing
            // zeros when the value's scale is below the target (an INCREASE) so the written scale
            // is exactly `target` — `1.5` setScale 2 → `1.50`.
            let mut v = x.round_dp_with_strategy(target, strategy(mode));
            v.rescale(target);
            DecimalBox::new_boxed(v)
        }
        None => make_error_tagged("setScale: argument is not a Decimal"),
    }
}

/// `scale(d)` — current number of fractional digits (raw `Int32`).
#[no_mangle]
pub unsafe extern "C" fn lin_decimal_scale(d: *const u8) -> i32 {
    match borrow(d) {
        Some(x) => x.scale() as i32,
        None => 0,
    }
}

// ----------------------------------------------------------------------------------------------
// Comparison (by VALUE, ignoring scale)
// ----------------------------------------------------------------------------------------------

/// `cmp(a, b)` — -1 / 0 / 1 by numeric value (scale-insensitive).
#[no_mangle]
pub unsafe extern "C" fn lin_decimal_cmp(a: *const u8, b: *const u8) -> i32 {
    match (borrow(a), borrow(b)) {
        (Some(x), Some(y)) => match x.cmp(&y) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        },
        _ => 0,
    }
}

// ----------------------------------------------------------------------------------------------
// Conversion
// ----------------------------------------------------------------------------------------------

/// `toString(d)` — exact, scale-preserving base-10. Bare `LinString*` (the `=> String` ABI).
#[no_mangle]
pub unsafe extern "C" fn lin_decimal_to_string(d: *const u8) -> *mut LinString {
    match borrow(d) {
        Some(x) => {
            let s = x.to_string();
            lin_string_from_bytes(s.as_ptr(), s.len() as u32)
        }
        None => lin_string_from_bytes(b"0".as_ptr(), 1),
    }
}

/// `toInt64(d)` — `Int64 | Error` (Error if non-integer OR out of range). Boxed TaggedVal.
#[no_mangle]
pub unsafe extern "C" fn lin_decimal_to_int64(d: *const u8) -> *mut u8 {
    use rust_decimal::prelude::ToPrimitive;
    match borrow(d) {
        Some(x) => {
            if x.fract() != Decimal::ZERO {
                return make_error_tagged("toInt64: value has a fractional part");
            }
            match x.to_i64() {
                Some(n) => crate::tagged::lin_box_int64(n),
                None => make_error_tagged("toInt64: value out of Int64 range"),
            }
        }
        None => make_error_tagged("toInt64: argument is not a Decimal"),
    }
}

/// `toFloat64(d)` — LOSSY; always succeeds. Raw `f64` (the `=> Float64` ABI).
#[no_mangle]
pub unsafe extern "C" fn lin_decimal_to_float64(d: *const u8) -> f64 {
    use rust_decimal::prelude::ToPrimitive;
    match borrow(d) {
        Some(x) => x.to_f64().unwrap_or(f64::NAN),
        None => 0.0,
    }
}

// ----------------------------------------------------------------------------------------------
// RC box primitives
// ----------------------------------------------------------------------------------------------

#[no_mangle]
pub unsafe extern "C" fn lin_decimal_retain_box(b: *const u8) {
    let b = b as *const DecimalBox;
    if !b.is_null() {
        (*b).rc.fetch_add(1, Ordering::Relaxed);
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_decimal_release_box(b: *const u8) {
    let b = b as *const DecimalBox;
    if b.is_null() {
        return;
    }
    if (*b).rc.fetch_sub(1, Ordering::Release) == 1 {
        std::sync::atomic::fence(Ordering::Acquire);
        drop(Box::from_raw(b as *mut DecimalBox));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe fn mk(s: &str) -> *mut u8 {
        let ls = crate::fs::make_string(s);
        let p = lin_decimal_parse(ls as *const u8);
        crate::string::lin_string_release(ls);
        p
    }
    unsafe fn str_of(p: *const u8) -> String {
        let s = lin_decimal_to_string(p);
        let r = (*s).as_str().to_string();
        crate::string::lin_string_release(s);
        r
    }

    #[test]
    fn exact_add_no_binary_drift() {
        unsafe {
            let a = mk("0.1");
            let b = mk("0.2");
            let sum = lin_decimal_add(a, b);
            let three = mk("0.3");
            assert_eq!(lin_decimal_cmp(sum, three), 0);
            for p in [a, b, sum, three] { crate::tagged::lin_tagged_release(p); }
        }
    }

    #[test]
    fn round_half_even() {
        unsafe {
            let x = mk("2.345");
            let r = lin_decimal_round(x, 2, 1); // banker's
            assert_eq!(str_of(r), "2.34");
            let y = mk("2.355");
            let r2 = lin_decimal_round(y, 2, 1);
            assert_eq!(str_of(r2), "2.36");
            for p in [x, r, y, r2] { crate::tagged::lin_tagged_release(p); }
        }
    }

    #[test]
    fn set_scale_pads() {
        unsafe {
            let x = mk("1.5");
            let r = lin_decimal_set_scale(x, 2, 1);
            assert_eq!(str_of(r), "1.50");
            assert_eq!(lin_decimal_scale(r), 2);
            crate::tagged::lin_tagged_release(x);
            crate::tagged::lin_tagged_release(r);
        }
    }

    #[test]
    fn div_rounds_and_errors() {
        unsafe {
            let a = mk("1");
            let b = mk("3");
            let q = lin_decimal_div(a, b, 4, 1);
            assert_eq!(str_of(q), "0.3333");
            let z = mk("0");
            let err = lin_decimal_div(a, z, 2, 1);
            assert_eq!(crate::tagged::lin_get_tag(err), crate::tagged::TAG_OBJECT);
            for p in [a, b, q, z, err] { crate::tagged::lin_tagged_release(p); }
        }
    }
}
