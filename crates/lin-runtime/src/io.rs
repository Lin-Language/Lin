use crate::string::{LinString, lin_string_from_bytes};
use crate::tagged::{TaggedVal, alloc_tagged, TAG_STR, TAG_ARRAY};
use std::io::{BufRead, Read, Write};

/// Read one line from stdin. Returns TaggedVal*(Str) or null if EOF.
/// Return type in Lin is Union(Str, Null), so we return a tagged pointer.
#[no_mangle]
pub unsafe extern "C" fn lin_io_read_line() -> *mut u8 {
    let stdin = std::io::stdin();
    let mut line = String::new();
    match stdin.lock().read_line(&mut line) {
        Ok(0) => std::ptr::null_mut(),
        Ok(_) => {
            if line.ends_with('\n') { line.pop(); if line.ends_with('\r') { line.pop(); } }
            let s = lin_string_from_bytes(line.as_ptr(), line.len() as u32);
            alloc_tagged(TAG_STR, s as u64)
        }
        Err(_) => std::ptr::null_mut(),
    }
}

/// Read all of stdin as a string. Returns bare LinString* (result type is Str).
#[no_mangle]
pub unsafe extern "C" fn lin_io_read_all() -> *mut LinString {
    let mut buf = String::new();
    let _ = std::io::stdin().lock().read_to_string(&mut buf);
    lin_string_from_bytes(buf.as_ptr(), buf.len() as u32)
}

/// Read all stdin lines into a LinArray of TaggedVal*(Str). Returns bare LinArray*.
/// Result type is Array(Str), so codegen expects a raw LinArray*.
#[no_mangle]
pub unsafe extern "C" fn lin_io_lines() -> *mut u8 {
    let stdin = std::io::stdin();
    let mut lines: Vec<String> = Vec::new();
    for line in stdin.lock().lines() {
        match line {
            Ok(l) => lines.push(l),
            Err(_) => break,
        }
    }
    let arr = crate::array::lin_array_alloc(lines.len().max(4) as u64);
    for line in &lines {
        let s = lin_string_from_bytes(line.as_ptr(), line.len() as u32);
        let mut tv: TaggedVal = std::mem::zeroed();
        tv.tag = TAG_STR;
        tv.payload = s as u64;
        crate::array::lin_array_push_tagged(arr, &tv as *const TaggedVal as *const u8);
    }
    arr as *mut u8
}

#[no_mangle]
pub unsafe extern "C" fn lin_print(s: *const LinString) {
    let slice = std::slice::from_raw_parts((*s).data.as_ptr(), (*s).len as usize);
    let string = std::str::from_utf8_unchecked(slice);
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    writeln!(handle, "{}", string).unwrap();
}

#[no_mangle]
pub unsafe extern "C" fn lin_io_print_err(s: *const LinString) {
    let slice = std::slice::from_raw_parts((*s).data.as_ptr(), (*s).len as usize);
    let string = std::str::from_utf8_unchecked(slice);
    eprintln!("{}", string);
}

#[no_mangle]
pub unsafe extern "C" fn lin_io_args() -> *mut u8 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arr = crate::array::lin_array_alloc(args.len().max(4) as u64);
    for arg in &args {
        let s = crate::string::lin_string_from_bytes(arg.as_ptr(), arg.len() as u32);
        let mut tv: crate::tagged::TaggedVal = std::mem::zeroed();
        tv.tag = crate::tagged::TAG_STR;
        tv.payload = s as u64;
        crate::array::lin_array_push_tagged(arr, &tv as *const crate::tagged::TaggedVal as *const u8);
    }
    arr as *mut u8
}

#[no_mangle]
pub unsafe extern "C" fn lin_exit(code: i32) -> ! {
    std::process::exit(code);
}

#[no_mangle]
pub unsafe extern "C" fn lin_panic(msg: *const LinString, file_id: i32, offset: i32) {
    let slice = std::slice::from_raw_parts((*msg).data.as_ptr(), (*msg).len as usize);
    let string = std::str::from_utf8_unchecked(slice);
    eprintln!("Runtime error at {}:{}: {}", file_id, offset, string);
    std::process::exit(1);
}
