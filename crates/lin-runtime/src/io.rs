use crate::string::LinString;
use std::io::Write;

#[no_mangle]
pub unsafe extern "C" fn lin_print(s: *const LinString) {
    let slice = std::slice::from_raw_parts((*s).data.as_ptr(), (*s).len as usize);
    let string = std::str::from_utf8_unchecked(slice);
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    writeln!(handle, "{}", string).unwrap();
}

#[no_mangle]
pub unsafe extern "C" fn lin_panic(msg: *const LinString, file_id: i32, offset: i32) {
    let slice = std::slice::from_raw_parts((*msg).data.as_ptr(), (*msg).len as usize);
    let string = std::str::from_utf8_unchecked(slice);
    eprintln!("Runtime error at {}:{}: {}", file_id, offset, string);
    std::process::exit(1);
}
