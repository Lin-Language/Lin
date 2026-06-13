use crate::string::{LinString, lin_string_from_bytes};
use crate::tagged::{TaggedVal, alloc_tagged, TAG_STR};
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
    // A bare `.unwrap()` here would PANIC across this `extern "C"` boundary on any write
    // error (UB / process abort). The common case is a closed downstream pipe
    // (`lin-prog | head` — the reader exits early): Rust installs SIG_IGN for SIGPIPE at
    // startup, so the EPIPE surfaces as a `BrokenPipe` error rather than a signal. Match the
    // conventional Unix-tool behaviour and exit cleanly. Any other write error (e.g. disk
    // full) is non-recoverable for a print call, so exit with a failure code rather than
    // unwinding through C.
    if let Err(e) = writeln!(handle, "{}", string) {
        match e.kind() {
            std::io::ErrorKind::BrokenPipe => std::process::exit(0),
            _ => std::process::exit(1),
        }
    }
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
    crate::memory::lin_rc_count_print();
    std::process::exit(code);
}

// `extern "C-unwind"`: inside an async boundary `runtime_fault` panics and must unwind THROUGH
// this C-ABI frame back to the thread boundary's catch_unwind (a plain `extern "C"` fn aborts
// the process on unwind since Rust 1.81). Outside a boundary it still process::exit's, never
// unwinding, so the ABI change is invisible there.
#[no_mangle]
pub unsafe extern "C-unwind" fn lin_panic(msg: *const LinString, file_id: i32, offset: i32) {
    let slice = std::slice::from_raw_parts((*msg).data.as_ptr(), (*msg).len as usize);
    let string = std::str::from_utf8_unchecked(slice);
    // Inside an async boundary this unwinds to the thread boundary (caught → Error);
    // at the top level it prints and exits (uncatchable, spec §20.1).
    crate::fault::runtime_fault(&format!("Runtime error at {}:{}: {}", file_id, offset, string));
}
