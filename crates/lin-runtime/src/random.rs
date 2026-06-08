//! std/random OS-entropy intrinsic.
//!
//! The PRNG itself (a PCG generator seeded through SplitMix64) lives in pure Lin in
//! `stdlib/random.lin`; the ONLY thing that cannot be expressed there is a source of
//! operating-system entropy for `fromEntropy()`. This module provides that single
//! intrinsic and nothing else.
//!
//! NOT cryptographically secure on its own: it merely seeds a non-cryptographic PRNG.
//! For key material / tokens use `std/crypto`.

/// Return 64 bits of operating-system entropy as an `Int64` (the value is an opaque bit
/// pattern; the Lin side treats it as a `UInt64` seed). On the astronomically unlikely
/// event the OS entropy source fails, fall back to a time/address-derived value so that
/// `fromEntropy()` never aborts the program.
#[no_mangle]
pub extern "C" fn lin_random_entropy() -> i64 {
    let mut buf = [0u8; 8];
    match getrandom::getrandom(&mut buf) {
        Ok(()) => i64::from_le_bytes(buf),
        Err(_) => {
            // Defensive fallback: mix a high-resolution timestamp with a stack address so
            // the seed is at least not constant. This path should essentially never run.
            use std::time::{SystemTime, UNIX_EPOCH};
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x9E37_79B9_7F4A_7C15);
            let addr = &buf as *const _ as u64;
            (nanos ^ addr.rotate_left(32)) as i64
        }
    }
}

// ── Width conversions used by stdlib/random.lin ───────────────────────────────────────────────
// Lin has no implicit wide↔narrow integer conversion, and the existing `std/number` casts route
// through `Float64` (lossy for full-width 32-bit values) or only narrow. These three total,
// well-defined conversions keep the PRNG's UInt64↔Int32↔Float64 plumbing exact. They mirror the
// `lin_to_*` convention (a thin `extern "C"` over a Rust `as`-cast).

/// Truncate a `UInt64` to its low 32 bits, reinterpreted as a signed `Int32` (two's-complement).
#[no_mangle]
pub extern "C" fn lin_uint64_to_int32(v: u64) -> i32 {
    v as u32 as i32
}

/// Widen a (non-negative) `Int32` to `UInt64` by zero-extending its 32-bit pattern.
#[no_mangle]
pub extern "C" fn lin_int32_to_uint64(v: i32) -> u64 {
    v as u32 as u64
}

/// Convert a `UInt64` to the nearest `Float64`. For the masked-to-32-bit values the PRNG passes in
/// this is exact; for full-width inputs it rounds as usual.
#[no_mangle]
pub extern "C" fn lin_uint64_to_float64(v: u64) -> f64 {
    v as f64
}
