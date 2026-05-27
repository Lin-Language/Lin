use crate::string::{LinString, lin_string_from_bytes};

// Constants — returned as f64
#[no_mangle] pub extern "C" fn lin_math_pi() -> f64 { std::f64::consts::PI }
#[no_mangle] pub extern "C" fn lin_math_e() -> f64 { std::f64::consts::E }
#[no_mangle] pub extern "C" fn lin_math_infinity() -> f64 { f64::INFINITY }
#[no_mangle] pub extern "C" fn lin_math_nan() -> f64 { f64::NAN }

// Math operations
#[no_mangle] pub extern "C" fn lin_math_floor(x: f64) -> f64 { x.floor() }
#[no_mangle] pub extern "C" fn lin_math_ceil(x: f64) -> f64 { x.ceil() }
#[no_mangle] pub extern "C" fn lin_math_round(x: f64) -> f64 { x.round() }
#[no_mangle] pub extern "C" fn lin_math_trunc(x: f64) -> f64 { x.trunc() }
#[no_mangle] pub extern "C" fn lin_math_sqrt(x: f64) -> f64 { x.sqrt() }
#[no_mangle] pub extern "C" fn lin_math_pow(base: f64, exp: f64) -> f64 { base.powf(exp) }
#[no_mangle] pub extern "C" fn lin_math_exp(x: f64) -> f64 { x.exp() }
#[no_mangle] pub extern "C" fn lin_math_log(x: f64) -> f64 { x.ln() }
#[no_mangle] pub extern "C" fn lin_math_log2(x: f64) -> f64 { x.log2() }
#[no_mangle] pub extern "C" fn lin_math_log10(x: f64) -> f64 { x.log10() }
#[no_mangle] pub extern "C" fn lin_math_sin(x: f64) -> f64 { x.sin() }
#[no_mangle] pub extern "C" fn lin_math_cos(x: f64) -> f64 { x.cos() }
#[no_mangle] pub extern "C" fn lin_math_tan(x: f64) -> f64 { x.tan() }
#[no_mangle] pub extern "C" fn lin_math_asin(x: f64) -> f64 { x.asin() }
#[no_mangle] pub extern "C" fn lin_math_acos(x: f64) -> f64 { x.acos() }
#[no_mangle] pub extern "C" fn lin_math_atan(x: f64) -> f64 { x.atan() }
#[no_mangle] pub extern "C" fn lin_math_atan2(y: f64, x: f64) -> f64 { y.atan2(x) }
#[no_mangle] pub extern "C" fn lin_math_abs_f64(x: f64) -> f64 { x.abs() }
#[no_mangle] pub extern "C" fn lin_math_abs_i64(x: i64) -> i64 { x.abs() }
#[no_mangle] pub extern "C" fn lin_math_min_f64(a: f64, b: f64) -> f64 { a.min(b) }
#[no_mangle] pub extern "C" fn lin_math_max_f64(a: f64, b: f64) -> f64 { a.max(b) }
#[no_mangle] pub extern "C" fn lin_math_clamp(x: f64, lo: f64, hi: f64) -> f64 { x.clamp(lo, hi) }
#[no_mangle] pub extern "C" fn lin_math_sign(x: f64) -> i64 { if x < 0.0 { -1 } else if x > 0.0 { 1 } else { 0 } }
#[no_mangle] pub extern "C" fn lin_math_is_nan(x: f64) -> bool { x.is_nan() }
#[no_mangle] pub extern "C" fn lin_math_is_finite(x: f64) -> bool { x.is_finite() }
#[no_mangle] pub extern "C" fn lin_math_random() -> f64 {
    // Simple LCG random — not cryptographic
    use std::time::SystemTime;
    static SEED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seed = SEED.load(std::sync::atomic::Ordering::Relaxed);
    let new_seed = if seed == 0 {
        SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64).unwrap_or(12345)
    } else {
        seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407)
    };
    SEED.store(new_seed, std::sync::atomic::Ordering::Relaxed);
    (new_seed >> 11) as f64 / (1u64 << 53) as f64
}

#[no_mangle]
pub unsafe extern "C" fn lin_math_to_fixed(x: f64, decimals: i64) -> *mut LinString {
    let s = format!("{:.prec$}", x, prec = decimals as usize);
    lin_string_from_bytes(s.as_ptr(), s.len() as u32)
}
