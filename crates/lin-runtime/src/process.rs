//! `std/process` runtime intrinsics — external process execution.
//!
//! Two styles, on one module:
//!
//! * **Batch** — `exec`/`shell` run a command to completion and collect its full
//!   stdout/stderr into an `ExecResult` object `{ status, stdout, stderr }`. `cwd`/`chdir`
//!   query/change the working directory. These need no handle.
//! * **Streaming** — `spawn` starts a child and returns an opaque `Int64` handle;
//!   `readStdout` reads its piped stdout incrementally; `kill` signals it; `wait` blocks
//!   for the exit code. This is the long-running / incremental-output path.
//!
//! Every fallible call returns the `T | Error` result shape (spec §27.6).
//!
//! ## handle / registry design (streaming path)
//!
//! A `std::process::Child` cannot be reconstructed from an OS pid, and reusing the pid as
//! the handle would be unsafe (pid reuse after the child reaps). Instead the owning `Child`
//! is kept alive in a global registry keyed by a **monotonic i64 id** from an `AtomicI64`.
//! The id is the opaque handle; it is never an OS pid.
//!
//! Each entry is a `ProcEntry { child, stdout }`. The child's piped stdout is `.take()`n
//! once at spawn and stored alongside, so repeated `readStdout` calls keep reading from the
//! same pipe. `wait` reaps the child and **removes the entry**; later ops on that handle err.
//!
//! The registry lives for the program's lifetime, so leak detection must be disabled for
//! ASan runs (the harness passes `ASAN_OPTIONS=detect_leaks=0`).

use std::collections::HashMap;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

use crate::array::{lin_array_length, LinArray};
use crate::fs::{make_error_tagged, make_string};
use crate::object::{lin_object_alloc, lin_object_set};
use crate::string::LinString;
use crate::tagged::{alloc_tagged, lin_box_int32, lin_box_int64, TaggedVal, TAG_ARRAY, TAG_INT32, TAG_OBJECT, TAG_STR};

struct ProcEntry {
    child: Child,
    stdout: Option<ChildStdout>,
}

static PROC_REGISTRY: Mutex<Option<HashMap<i64, ProcEntry>>> = Mutex::new(None);
static NEXT_ID: AtomicI64 = AtomicI64::new(1);

fn with_registry<R>(f: impl FnOnce(&mut HashMap<i64, ProcEntry>) -> R) -> R {
    // Recover from a poisoned lock instead of `.unwrap()`-panicking: these registry ops run
    // from `extern "C"` functions, where unwinding is UB / process abort. Poisoning only means a
    // prior thread panicked while holding the guard (plausible: spawned children are managed
    // across worker/async paths). The HashMap itself stays consistent, so we take the inner guard
    // and carry on.
    let mut guard = PROC_REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
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
/// `arr` may be a TaggedVal*(Array) or a raw LinArray* of TAG_STR elements; each
/// element's `payload` field is a `LinString*`.
unsafe fn read_string_array(arr: *const u8) -> Option<Vec<String>> {
    if arr.is_null() {
        return None;
    }
    let tag = *arr;
    let lin_arr = if tag == TAG_ARRAY {
        (*(arr as *const TaggedVal)).payload as *const LinArray
    } else {
        arr as *const LinArray
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

/// Build an `ExecResult` object `{ "status": Int32, "stdout": String, "stderr": String }`
/// as an owned `TaggedVal*(Object)`. `lin_object_set` retains both the key and the value's
/// inner string, so the local `+1` from each `make_string` is released afterward — the object
/// becomes the sole owner and freeing the returned box frees everything (no leak; see
/// `fs::make_decode_error` for the same pattern, verified under ASan).
unsafe fn make_exec_result(status: i32, stdout: &str, stderr: &str) -> *mut u8 {
    use crate::string::lin_string_release;
    let obj = lin_object_alloc(3);

    let status_key = make_string("status");
    let mut status_tv: TaggedVal = std::mem::zeroed();
    status_tv.tag = TAG_INT32;
    status_tv.payload = status as i64 as u64;
    lin_object_set(obj, status_key, &status_tv); // retains status_key
    lin_string_release(status_key);

    let set_str = |key: &str, val: &str| {
        let k = make_string(key);
        let v = make_string(val);
        let mut tv: TaggedVal = std::mem::zeroed();
        tv.tag = TAG_STR;
        tv.payload = v as u64;
        lin_object_set(obj, k, &tv); // retains both k and v
        lin_string_release(k); // drop our local +1 (object owns its own ref)
        lin_string_release(v);
    };
    set_str("stdout", stdout);
    set_str("stderr", stderr);

    alloc_tagged(TAG_OBJECT, obj as u64)
}

/// Run a fully-built `Command` to completion, capturing stdout+stderr; return an
/// `ExecResult` object or an `Error`.
unsafe fn run_to_completion(mut cmd: Command) -> *mut u8 {
    cmd.stdin(Stdio::null());
    match cmd.output() {
        Ok(out) => {
            let code = out.status.code().unwrap_or(-1);
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            make_exec_result(code, &stdout, &stderr)
        }
        Err(e) => make_error_tagged(&e.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Batch API: exec / shell / cwd / chdir
// ---------------------------------------------------------------------------

/// exec: (command: String, args: String[]) => ExecResult | Error.
/// Runs `command` with `args`, waits, and returns its status + captured stdout/stderr.
#[no_mangle]
pub unsafe extern "C" fn lin_process_exec(command: *const u8, args: *const u8) -> *mut u8 {
    if command.is_null() {
        return make_error_tagged("exec: null command");
    }
    let cmd_str = (*(command as *const LinString)).as_str().to_string();
    let arg_vec = read_string_array(args).unwrap_or_default();
    let mut cmd = Command::new(&cmd_str);
    cmd.args(&arg_vec);
    run_to_completion(cmd)
}

/// shell: (command: String) => ExecResult | Error.
/// Runs `command` through `/bin/sh -c` (POSIX), capturing stdout/stderr.
#[no_mangle]
pub unsafe extern "C" fn lin_process_shell(command: *const u8) -> *mut u8 {
    if command.is_null() {
        return make_error_tagged("shell: null command");
    }
    let cmd_str = (*(command as *const LinString)).as_str().to_string();
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(&cmd_str);
    run_to_completion(cmd)
}

/// cwd: () => String. Returns the absolute current working directory (or "" on error).
#[no_mangle]
pub unsafe extern "C" fn lin_process_cwd() -> *mut u8 {
    let dir = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    make_string(&dir) as *mut u8
}

/// chdir: (path: String) => Null | Error.
#[no_mangle]
pub unsafe extern "C" fn lin_process_chdir(path: *const u8) -> *mut u8 {
    if path.is_null() {
        return make_error_tagged("chdir: null path");
    }
    let p = (*(path as *const LinString)).as_str().to_string();
    match std::env::set_current_dir(&p) {
        Ok(()) => std::ptr::null_mut(),
        Err(e) => make_error_tagged(&format!("chdir '{}': {}", p, e)),
    }
}

// ---------------------------------------------------------------------------
// Streaming API: spawn / readStdout / kill / wait
// ---------------------------------------------------------------------------

/// spawn: (command: String, args: String[]) => Int64 | Error.
/// stdout is piped (so readStdout works); stdin is null; stderr is inherited.
#[no_mangle]
pub unsafe extern "C" fn lin_process_spawn(command: *const u8, args: *const u8) -> *mut u8 {
    if command.is_null() {
        return make_error_tagged("spawn: null command");
    }
    let cmd_str = (*(command as *const LinString)).as_str().to_string();
    let arg_vec = read_string_array(args).unwrap_or_default();
    let mut cmd = Command::new(&cmd_str);
    cmd.args(&arg_vec)
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

/// Stream-source read: pull up to `max` bytes from the child `handle`'s piped stdout. Returns
/// `Ok(None)` at EOF (0 bytes), `Ok(Some(bytes))` for a chunk, or `Err` on an I/O error / unknown
/// handle. Used by `std/stream`'s `stdoutStream` backend (the registry stays encapsulated here).
pub fn process_stdout_read(handle: i64, max: usize) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read;
    let mut buf = vec![0u8; max];
    let result = with_registry(|m| match m.get_mut(&handle) {
        Some(entry) => match entry.stdout.as_mut() {
            Some(out) => Some(out.read(&mut buf)),
            None => Some(Ok(0usize)), // not piped / already taken → EOF
        },
        None => None,
    });
    match result {
        None => Err("no such process handle".to_string()),
        Some(Ok(0)) => Ok(None),
        Some(Ok(n)) => {
            buf.truncate(n);
            Ok(Some(buf))
        }
        Some(Err(e)) => Err(e.to_string()),
    }
}

/// readStdout: (handle, buf) => Int32 | Error. Returns bytes read (0 = EOF).
/// Reads incrementally from the child's captured stdout pipe across calls.
#[no_mangle]
pub unsafe extern "C" fn lin_process_read_stdout(handle: i64, buf: *const u8) -> *mut u8 {
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
pub unsafe extern "C" fn lin_process_kill(handle: i64) -> *mut u8 {
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
pub unsafe extern "C" fn lin_process_wait(handle: i64) -> *mut u8 {
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
