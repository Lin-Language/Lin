//! `std/proc` runtime intrinsics — subprocess management.
//!
//! Subprocesses are exposed to Lin as opaque integer handles (spec §35.4, §35.6).
//! Every fallible call returns the `T | Error` result shape.
//!
//! ## handle / registry design
//!
//! A `std::process::Child` cannot be reconstructed from an OS pid, and reusing the
//! pid as the handle would be unsafe (pid reuse after the child reaps). Instead we
//! keep the owning `Child` alive in a global registry keyed by a **monotonic i64 id**
//! allocated from an `AtomicI64`. The id is the opaque handle returned to user code;
//! it is never an OS pid.
//!
//! Each registry value is a `ProcEntry { child, stdout }`. We `.take()` the child's
//! piped stdout *once* at spawn time and store the `ChildStdout` alongside the `Child`,
//! so repeated `readStdout` calls keep reading incrementally from the same pipe.
//!
//! `wait` consumes the child's exit status and then **removes the entry** from the
//! registry (the `Child` has been reaped; the stdout pipe, if any, is dropped). After
//! a successful `wait`, subsequent operations on the same handle return an error.
//!
//! The registry lives for the program's lifetime, so leak detection must be disabled
//! for ASan runs (the harness passes `ASAN_OPTIONS=detect_leaks=0`).

use std::collections::HashMap;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

use crate::array::{lin_array_length, LinArray};
use crate::fs::make_error_tagged;
use crate::string::LinString;
use crate::tagged::{lin_box_int32, lin_box_int64, TaggedVal, TAG_ARRAY};

struct ProcEntry {
    child: Child,
    stdout: Option<ChildStdout>,
}

static PROC_REGISTRY: Mutex<Option<HashMap<i64, ProcEntry>>> = Mutex::new(None);
static NEXT_ID: AtomicI64 = AtomicI64::new(1);

fn with_registry<R>(f: impl FnOnce(&mut HashMap<i64, ProcEntry>) -> R) -> R {
    let mut guard = PROC_REGISTRY.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    f(map)
}

/// Resolve the caller's UInt8[] buffer to (data ptr, capacity in bytes).
/// `buf` may be a TaggedVal*(Array) or a raw LinArray*; the inner array is a flat
/// UInt8 buffer whose element count bounds the read.
unsafe fn buf_parts(buf: *const u8) -> Option<(*mut u8, usize)> {
    if buf.is_null() {
        return None;
    }
    let tag = *buf;
    let lin_arr = if tag == TAG_ARRAY {
        (*(buf as *const TaggedVal)).payload as *const LinArray
    } else {
        buf as *const LinArray
    };
    if lin_arr.is_null() {
        return None;
    }
    let len = lin_array_length(lin_arr) as usize;
    let data = (*lin_arr).data as *mut u8;
    Some((data, len))
}

/// Read the elements of a `String[]` (Json) into a Vec<String>.
/// `argv` may be a TaggedVal*(Array) or a raw LinArray* of TAG_STR elements; each
/// element's `payload` field is a `LinString*`.
unsafe fn read_string_array(argv: *const u8) -> Option<Vec<String>> {
    if argv.is_null() {
        return None;
    }
    let tag = *argv;
    let lin_arr = if tag == TAG_ARRAY {
        (*(argv as *const TaggedVal)).payload as *const LinArray
    } else {
        argv as *const LinArray
    };
    if lin_arr.is_null() {
        return None;
    }
    let n = lin_array_length(lin_arr) as usize;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let elem = (*lin_arr).data.add(i);
        let s = (*elem).payload as *const LinString;
        if s.is_null() {
            out.push(String::new());
        } else {
            out.push((*s).as_str().to_string());
        }
    }
    Some(out)
}

/// spawn: (argv: String[]) => Int64 | Error. argv[0] = program, rest = args.
/// stdout is piped (so readStdout works); stdin is null; stderr is inherited.
#[no_mangle]
pub unsafe extern "C" fn lin_proc_spawn(argv: *const u8) -> *mut u8 {
    let args = match read_string_array(argv) {
        Some(a) => a,
        None => return make_error_tagged("invalid argv"),
    };
    if args.is_empty() {
        return make_error_tagged("spawn: empty argv");
    }
    let mut cmd = Command::new(&args[0]);
    cmd.args(&args[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    match cmd.spawn() {
        Ok(mut child) => {
            let stdout = child.stdout.take();
            let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
            with_registry(|m| m.insert(id, ProcEntry { child, stdout }));
            lin_box_int64(id)
        }
        Err(e) => make_error_tagged(&e.to_string()),
    }
}

/// readStdout: (handle, buf) => Int32 | Error. Returns bytes read (0 = EOF).
/// Reads incrementally from the child's captured stdout pipe across calls.
#[no_mangle]
pub unsafe extern "C" fn lin_proc_read_stdout(handle: i64, buf: *const u8) -> *mut u8 {
    use std::io::Read;
    let (data, cap) = match buf_parts(buf) {
        Some(p) => p,
        None => return make_error_tagged("invalid buffer"),
    };
    let result = with_registry(|m| match m.get_mut(&handle) {
        Some(entry) => match entry.stdout.as_mut() {
            Some(out) => {
                let slice = std::slice::from_raw_parts_mut(data, cap);
                Some(out.read(slice))
            }
            // stdout was not piped / already taken: treat as EOF.
            None => Some(Ok(0usize)),
        },
        None => None,
    });
    match result {
        None => make_error_tagged("no such process handle"),
        Some(Ok(n)) => lin_box_int32(n as i32),
        Some(Err(e)) => make_error_tagged(&e.to_string()),
    }
}

/// kill: (handle) => Null | Error. Killing an already-exited child is tolerated.
#[no_mangle]
pub unsafe extern "C" fn lin_proc_kill(handle: i64) -> *mut u8 {
    let result = with_registry(|m| m.get_mut(&handle).map(|e| e.child.kill()));
    match result {
        None => make_error_tagged("no such process handle"),
        Some(Ok(())) => std::ptr::null_mut(),
        // The child has already exited; std reports InvalidInput. Treat as success.
        Some(Err(e)) if e.kind() == std::io::ErrorKind::InvalidInput => std::ptr::null_mut(),
        Some(Err(e)) => make_error_tagged(&e.to_string()),
    }
}

/// wait: (handle) => Int32 | Error. Returns the exit code (-1 if killed by signal /
/// no code). Consumes the child and REMOVES the entry from the registry.
#[no_mangle]
pub unsafe extern "C" fn lin_proc_wait(handle: i64) -> *mut u8 {
    let result = with_registry(|m| m.get_mut(&handle).map(|e| e.child.wait()));
    match result {
        None => make_error_tagged("no such process handle"),
        Some(Ok(status)) => {
            with_registry(|m| m.remove(&handle));
            lin_box_int32(status.code().unwrap_or(-1))
        }
        Some(Err(e)) => make_error_tagged(&e.to_string()),
    }
}
