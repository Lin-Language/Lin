//! `std/os` runtime support: read-only host / machine / current-process introspection.
//!
//! Two tiers (cf. the proposal):
//!   * Trivially portable (`std`/`libc` only): `platform`, `arch`, `cpuCount`, `pid`, `ppid`,
//!     `tempDir`, `hostname`, `username`, `homeDir`.
//!   * Platform-specific (backed by the `sysinfo` crate): `uptime`, `loadAverage`, `memInfo`.
//!
//! Total functions return a bare value (String / Int32 / Int64). Fallible functions return the
//! canonical `{ "type":"error", "message":... }` tagged object (discriminated `is Error` in Lin),
//! narrowed to `T | Error` in the `.lin` wrapper.
//!
//! Record/array-returning intrinsics (`memInfo`, `loadAverage`) follow the owned-result RC
//! contract: each returns a fresh `+1` box, with every inner string/value owned by the container
//! so a single `lin_tagged_release` frees the whole graph (verified ASan-clean).

use crate::array::{lin_array_alloc, lin_array_push_tagged};
use crate::fs::{make_error_tagged, make_string};
use crate::object::{lin_object_alloc, lin_object_set, LinObject};
use crate::string::lin_string_release;
use crate::tagged::{alloc_tagged, TaggedVal, TAG_ARRAY, TAG_FLOAT64, TAG_INT64, TAG_OBJECT, TAG_STR};

/// A fresh owned raw `LinString*` (the ABI for a `() => String` intrinsic — codegen reads the
/// result as a bare LinString, NOT a boxed TaggedVal; cf. `lin_process_cwd`).
unsafe fn raw_string(s: &str) -> *mut u8 {
    make_string(s) as *mut u8
}

/// Box a Rust string as a fresh owned `TaggedVal*(Str)` — the ABI for a `() => Json` intrinsic
/// whose `.lin` wrapper narrows to `String | Error` (the Error arm is also a boxed object, so the
/// success arm must be boxed too for `is Error` discrimination to work).
unsafe fn string_tagged(s: &str) -> *mut u8 {
    let ls = make_string(s);
    alloc_tagged(TAG_STR, ls as u64)
}

// --- Tier 1: trivially portable ------------------------------------------------------------

/// Operating-system family as a lowercase string from a closed set
/// (`"linux"`/`"macos"`/`"windows"`/`"freebsd"`/`"openbsd"`/`"netbsd"`/`"unknown"`).
/// Fixed at compile time (the build target) — total, never fails.
#[no_mangle]
pub unsafe extern "C" fn lin_os_platform() -> *mut u8 {
    // std::env::consts::OS already reports "macos" for Darwin and "linux"/"windows"/the BSDs.
    // Normalise the set we document; anything else collapses to "unknown".
    let p = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "macos",
        "windows" => "windows",
        "freebsd" => "freebsd",
        "openbsd" => "openbsd",
        "netbsd" => "netbsd",
        _ => "unknown",
    };
    raw_string(p)
}

/// CPU architecture as a lowercase string from a closed set. Fixed at compile time — total.
#[no_mangle]
pub unsafe extern "C" fn lin_os_arch() -> *mut u8 {
    let a = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "arm" => "arm",
        "x86" => "x86",
        "riscv64" => "riscv64",
        "wasm32" => "wasm32",
        _ => "unknown",
    };
    raw_string(a)
}

/// Number of logical CPUs available to the process (affinity/cgroup-aware), always >= 1.
#[no_mangle]
pub unsafe extern "C" fn lin_os_cpu_count() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(1)
        .max(1)
}

/// OS process id of the current process. Total.
#[no_mangle]
pub unsafe extern "C" fn lin_os_pid() -> i32 {
    std::process::id() as i32
}

/// OS process id of the parent process. On Unix this is `getppid()`. On platforms where the
/// parent pid cannot be determined, returns 0 (not an Error) so callers treat "no parent"
/// uniformly.
#[no_mangle]
pub unsafe extern "C" fn lin_os_ppid() -> i32 {
    #[cfg(unix)]
    {
        libc::getppid()
    }
    #[cfg(not(unix))]
    {
        // sysinfo resolves the parent via a process snapshot on non-Unix targets.
        use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
        let me = Pid::from_u32(std::process::id());
        let mut sys = System::new();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
        sys.process(me)
            .and_then(|p| p.parent())
            .map(|pp| pp.as_u32() as i32)
            .unwrap_or(0)
    }
}

/// The OS temp directory as an absolute path. Falls back to the system default rather than
/// failing — total.
#[no_mangle]
pub unsafe extern "C" fn lin_os_temp_dir() -> *mut u8 {
    let p = std::env::temp_dir();
    raw_string(&p.to_string_lossy())
}

/// Network hostname (short name). `String | Error`.
#[no_mangle]
pub unsafe extern "C" fn lin_os_hostname() -> *mut u8 {
    match read_hostname() {
        Some(h) => string_tagged(&h),
        None => make_error_tagged("hostname: cannot read host name"),
    }
}

#[cfg(unix)]
unsafe fn read_hostname() -> Option<String> {
    // POSIX HOST_NAME_MAX is 255; allocate a generous buffer and let gethostname truncate-and-
    // null-terminate. A non-zero return means failure.
    let mut buf = vec![0u8; 256];
    let rc = libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len());
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let name = String::from_utf8_lossy(&buf[..end]).into_owned();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

#[cfg(not(unix))]
unsafe fn read_hostname() -> Option<String> {
    // %COMPUTERNAME% is the conventional Windows hostname source.
    std::env::var("COMPUTERNAME").ok().filter(|s| !s.is_empty())
}

/// Login name of the user the process runs as. `String | Error`.
#[no_mangle]
pub unsafe extern "C" fn lin_os_username() -> *mut u8 {
    match read_username() {
        Some(u) => string_tagged(&u),
        None => make_error_tagged("username: cannot resolve current user"),
    }
}

#[cfg(unix)]
unsafe fn read_username() -> Option<String> {
    // Effective uid's passwd entry, falling back to $USER / $LOGNAME.
    let uid = libc::geteuid();
    let pw = libc::getpwuid(uid);
    if !pw.is_null() && !(*pw).pw_name.is_null() {
        let cstr = std::ffi::CStr::from_ptr((*pw).pw_name);
        if let Ok(s) = cstr.to_str() {
            if !s.is_empty() {
                return Some(s.to_owned());
            }
        }
    }
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .ok()
        .filter(|s| !s.is_empty())
}

#[cfg(not(unix))]
unsafe fn read_username() -> Option<String> {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .ok()
        .filter(|s| !s.is_empty())
}

/// Current user's home directory, absolute path. `String | Error`.
#[no_mangle]
pub unsafe extern "C" fn lin_os_home_dir() -> *mut u8 {
    match read_home_dir() {
        Some(h) => string_tagged(&h),
        None => make_error_tagged("homeDir: no home directory for this process"),
    }
}

#[cfg(unix)]
unsafe fn read_home_dir() -> Option<String> {
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            return Some(h);
        }
    }
    // Fall back to the passwd entry's home directory.
    let uid = libc::geteuid();
    let pw = libc::getpwuid(uid);
    if !pw.is_null() && !(*pw).pw_dir.is_null() {
        let cstr = std::ffi::CStr::from_ptr((*pw).pw_dir);
        if let Ok(s) = cstr.to_str() {
            if !s.is_empty() {
                return Some(s.to_owned());
            }
        }
    }
    None
}

#[cfg(not(unix))]
unsafe fn read_home_dir() -> Option<String> {
    std::env::var("USERPROFILE")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            // %HOMEDRIVE%%HOMEPATH% as a secondary fallback.
            let drive = std::env::var("HOMEDRIVE").ok()?;
            let path = std::env::var("HOMEPATH").ok()?;
            let combined = format!("{drive}{path}");
            if combined.is_empty() {
                None
            } else {
                Some(combined)
            }
        })
}

// --- Tier 2: platform-specific (sysinfo) ----------------------------------------------------

/// System uptime in whole seconds since boot. `Int64 | Error`.
#[no_mangle]
pub unsafe extern "C" fn lin_os_uptime() -> *mut u8 {
    // System::uptime() is an associated function in sysinfo 0.39 (no System instance needed).
    let secs = sysinfo::System::uptime();
    alloc_tagged(TAG_INT64, secs as i64 as u64)
}

/// System load average over 1/5/15 minutes as a fresh 3-element Float64 array. Unix-only;
/// returns an Error on platforms without a load-average concept (Windows). `[Float64;3] | Error`.
#[no_mangle]
pub unsafe extern "C" fn lin_os_load_average() -> *mut u8 {
    #[cfg(unix)]
    {
        let la = sysinfo::System::load_average();
        let arr = lin_array_alloc(3);
        for v in [la.one, la.five, la.fifteen] {
            let boxed = alloc_tagged(TAG_FLOAT64, (v as f64).to_bits());
            // lin_array_push_tagged TRANSFERS ownership of the inner heap value into the array
            // slot (no retain): the array becomes the sole owner of each float box.
            lin_array_push_tagged(arr, boxed);
        }
        alloc_tagged(TAG_ARRAY, arr as u64)
    }
    #[cfg(not(unix))]
    {
        make_error_tagged("loadAverage: not available on this platform (no load average concept)")
    }
}

/// Physical memory snapshot in bytes: `{ "total": Int64, "free": Int64 }`. `MemInfo | Error`.
#[no_mangle]
pub unsafe extern "C" fn lin_os_mem_info() -> *mut u8 {
    use sysinfo::{MemoryRefreshKind, System};
    let mut sys = System::new();
    sys.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
    let total = sys.total_memory();
    let free = sys.available_memory();
    if total == 0 {
        return make_error_tagged("memInfo: cannot read system memory");
    }
    let obj = lin_object_alloc(4);
    set_int64(obj, "total", total as i64);
    set_int64(obj, "free", free as i64);
    alloc_tagged(TAG_OBJECT, obj as u64)
}

/// Set an Int64-valued field on `obj`. The key string's local +1 is released after the set
/// (lin_object_set takes its own reference); the Int64 payload is inline (not heap), so no
/// value release is needed.
unsafe fn set_int64(obj: *mut LinObject, key: &str, val: i64) {
    let k = make_string(key);
    let mut tv: TaggedVal = std::mem::zeroed();
    tv.tag = TAG_INT64;
    tv.payload = val as u64;
    lin_object_set(obj, k, &tv);
    lin_string_release(k);
}
