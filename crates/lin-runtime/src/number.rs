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
