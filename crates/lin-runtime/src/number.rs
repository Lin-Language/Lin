use crate::string::LinString;

#[no_mangle]
pub extern "C" fn lin_parse_int32(s: *const LinString) -> i32 {
    unsafe {
        let st = (*s).as_str();
        st.trim().parse::<i32>().unwrap_or(0)
    }
}

#[no_mangle]
pub extern "C" fn lin_parse_float64(s: *const LinString) -> f64 {
    unsafe {
        let st = (*s).as_str();
        st.trim().parse::<f64>().unwrap_or(0.0)
    }
}

#[no_mangle]
pub extern "C" fn lin_to_int32(v: f64) -> i32 {
    v as i32
}

#[no_mangle]
pub extern "C" fn lin_to_float64(v: i32) -> f64 {
    v as f64
}

/// Explicit Float64 -> Float32 narrowing cast (spec §21). There is no implicit
/// float narrowing, so this is the only way to obtain a Float32 from a computed
/// Float64 (e.g. for big-endian f32 serialization via std/bytes).
#[no_mangle]
pub extern "C" fn lin_to_float32(v: f64) -> f32 {
    v as f32
}

// -------------------------------------------------------------------------
// Explicit narrowing integer casts (spec §21). Each takes the widest
// practical integer input (i64) and truncates to the target width using
// `as`-cast (sign/zero handling per Rust `as` semantics). These back the
// std/number `toUInt8`/`toInt8`/`toUInt16`/`toInt16`/`toUInt32`/`toInt64`/
// `toUInt64` exports and the byte-extraction in std/bytes. The input is taken
// as u64 (the widest unsigned) so any narrower unsigned integer (UInt8/16/32)
// or a masked UInt64 widens into it at the call site without losing range.
// -------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn lin_to_uint8(v: u64) -> u8 {
    v as u8
}

#[no_mangle]
pub extern "C" fn lin_to_int8(v: u64) -> i8 {
    v as i8
}

#[no_mangle]
pub extern "C" fn lin_to_uint16(v: u64) -> u16 {
    v as u16
}

#[no_mangle]
pub extern "C" fn lin_to_int16(v: u64) -> i16 {
    v as i16
}

#[no_mangle]
pub extern "C" fn lin_to_uint32(v: u64) -> u32 {
    v as u32
}

#[no_mangle]
pub extern "C" fn lin_to_int64(v: u64) -> i64 {
    v as i64
}

#[no_mangle]
pub extern "C" fn lin_to_uint64(v: u64) -> u64 {
    v
}

// -------------------------------------------------------------------------
// Float bit-reinterpret intrinsics (spec §27.3). A float's bit pattern
// cannot be obtained by shift-and-mask, so std/bytes needs these to
// (de)serialize floats through UInt8[] buffers.
// -------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn lin_f32_to_bits(f: f32) -> u32 {
    f.to_bits()
}

#[no_mangle]
pub extern "C" fn lin_f32_from_bits(u: u32) -> f32 {
    f32::from_bits(u)
}

#[no_mangle]
pub extern "C" fn lin_f64_to_bits(f: f64) -> u64 {
    f.to_bits()
}

#[no_mangle]
pub extern "C" fn lin_f64_from_bits(u: u64) -> f64 {
    f64::from_bits(u)
}

/// Single-pass `tryParseInt32`: validate + parse `s` in ONE scan, returning a boxed `Int32 | Null`
/// TaggedVal (a null pointer is the `Null` arm; a small int routes through the immortal box cache).
/// Replaces the `if lin_is_int32(s) then lin_parse_int32(s)` two-scan idiom in std/number.
#[no_mangle]
pub unsafe extern "C" fn lin_try_parse_int32(s: *const LinString) -> *mut u8 {
    let st = (*s).as_str();
    match st.trim().parse::<i32>() {
        Ok(n) => crate::tagged::lin_box_int32(n),
        Err(_) => std::ptr::null_mut(),
    }
}

/// Single-pass `tryParseFloat64`: validate + parse in ONE scan, returning a boxed `Float64 | Null`
/// (null pointer = `Null`). Replaces the `lin_is_float64` + `lin_parse_float64` two-scan idiom.
#[no_mangle]
pub unsafe extern "C" fn lin_try_parse_float64(s: *const LinString) -> *mut u8 {
    let st = (*s).as_str();
    match st.trim().parse::<f64>() {
        Ok(f) => crate::tagged::lin_box_float64(f),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub extern "C" fn lin_is_int32(s: *const LinString) -> bool {
    unsafe {
        let st = (*s).as_str();
        st.trim().parse::<i32>().is_ok()
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_is_float64(s: *const LinString) -> bool {
    let st = (*s).as_str();
    st.trim().parse::<f64>().is_ok()
}

// -------------------------------------------------------------------------
// Unsigned-integer parse functions.
// -------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn lin_parse_uint8(s: *const LinString) -> u8 {
    unsafe { (*s).as_str().trim().parse::<u8>().unwrap_or(0) }
}

#[no_mangle]
pub extern "C" fn lin_parse_uint16(s: *const LinString) -> u16 {
    unsafe { (*s).as_str().trim().parse::<u16>().unwrap_or(0) }
}

#[no_mangle]
pub extern "C" fn lin_parse_uint32(s: *const LinString) -> u32 {
    unsafe { (*s).as_str().trim().parse::<u32>().unwrap_or(0) }
}

#[no_mangle]
pub extern "C" fn lin_parse_uint64(s: *const LinString) -> u64 {
    unsafe { (*s).as_str().trim().parse::<u64>().unwrap_or(0) }
}

#[no_mangle]
pub extern "C" fn lin_parse_int64(s: *const LinString) -> i64 {
    unsafe { (*s).as_str().trim().parse::<i64>().unwrap_or(0) }
}

#[no_mangle]
pub extern "C" fn lin_parse_float32(s: *const LinString) -> f32 {
    unsafe { (*s).as_str().trim().parse::<f32>().unwrap_or(0.0) }
}

#[no_mangle]
pub unsafe extern "C" fn lin_try_parse_uint8(s: *const LinString) -> *mut u8 {
    let st = (*s).as_str();
    match st.trim().parse::<u8>() {
        Ok(n) => crate::tagged::lin_box_int32(n as i32),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_try_parse_uint16(s: *const LinString) -> *mut u8 {
    let st = (*s).as_str();
    match st.trim().parse::<u16>() {
        Ok(n) => crate::tagged::lin_box_int32(n as i32),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_try_parse_uint32(s: *const LinString) -> *mut u8 {
    let st = (*s).as_str();
    match st.trim().parse::<u32>() {
        // UInt32 values that fit in i32 range use the int32 box; larger values use int64.
        Ok(n) => crate::tagged::lin_box_int64(n as i64),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_try_parse_uint64(s: *const LinString) -> *mut u8 {
    let st = (*s).as_str();
    match st.trim().parse::<u64>() {
        Ok(n) => crate::tagged::lin_box_uint64(n),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_try_parse_int64(s: *const LinString) -> *mut u8 {
    let st = (*s).as_str();
    match st.trim().parse::<i64>() {
        Ok(n) => crate::tagged::lin_box_int64(n),
        Err(_) => std::ptr::null_mut(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn lin_try_parse_float32(s: *const LinString) -> *mut u8 {
    let st = (*s).as_str();
    match st.trim().parse::<f32>() {
        Ok(f) => crate::tagged::lin_box_float64(f as f64),
        Err(_) => std::ptr::null_mut(),
    }
}
