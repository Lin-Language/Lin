//! `Stream<T>` — an opaque, lazy, effectful, fallible PULL-SOURCE that owns an OS resource
//! (a file descriptor, socket, child-process pipe, …) (streams brief, ADR-047).
//!
//! A `Stream` is the resource-owning sibling of `Iterator`. The iterator protocol's
//! `cond`/`current` must be PURE, but reading from a stream has side effects (it advances an
//! fd), can FAIL (I/O error), and the stream OWNS something the OS must reclaim. So a stream is
//! its own opaque runtime type with its own RC discipline.
//!
//! Modelled closely on `Shared<T>` (`shared.rs`): the heap box is wrapped in a
//! `TaggedVal*(TAG_STREAM)` so it flows through the universal value representation, and its RC
//! dispatches through the tag-aware `lin_tagged_retain`/`lin_tagged_release` (whose TAG_STREAM
//! arms call `lin_stream_retain_box`/`lin_stream_release_box`).
//!
//! Resource discipline (brief §2):
//!   * Each `StreamBox` owns a read backend (`StreamSource`) and a `closed` flag.
//!   * `lin_stream_close` closes the backend; idempotent (closing twice is a no-op).
//!   * When the refcount hits 0, the box's `Drop` closes the backend IF it was not already
//!     closed — so a dropped (never-explicitly-closed) stream still reclaims its fd. This is
//!     the auto-close finalizer; explicit `close()` exists for DETERMINISM only.
//!
//! Refcount: the brief locks the transfer model as "resource values cross by MOVE" — a moved
//! stream yields a disjoint object graph, so non-atomic RC would be sound. We nonetheless use an
//! `AtomicU32` (as `Shared` does) so a stray cross-thread touch can never race the finalizer; the
//! cost is negligible (one stream box, not per-element).

use crate::tagged::{TaggedVal, TAG_STREAM};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

/// A read backend behind a `Stream`. Each concrete OS source (file, TCP socket, process stdout,
/// stdin) implements the byte-yielding `read`; lazy ADAPTERS (map/filter/take/lines/chunks) and
/// the sink override `read_tagged` instead, yielding arbitrary tagged values.
///
/// Byte sources implement `read`:
///   * `Ok(Some(bytes))` — a non-empty chunk of bytes was read.
///   * `Ok(None)`        — end of stream (EOF); no more data.
///   * `Err(msg)`        — an I/O error; the chunk could not be read.
///
/// The default `read_tagged` wraps `read`'s byte chunk into a flat `UInt8[]` tagged array, so a
/// byte source's items are `UInt8[]`. Adapters override `read_tagged` to pull from an upstream
/// `StreamBox` and transform the tagged items in-band (see the `*Source` adapter structs).
pub trait StreamSource: Send {
    /// Pull the next chunk of bytes. `Ok(None)` signals EOF. Byte (OS) sources implement this;
    /// adapters that override `read_tagged` may leave this as the `unreachable!` default.
    fn read(&mut self) -> Result<Option<Vec<u8>>, String> {
        unreachable!("a tagged adapter source must override read_tagged, not read")
    }
    /// Pull the next item as a tagged value. The default wraps `read`'s byte chunk into a flat
    /// `UInt8[]` tagged array. Returns a `TaggedOutcome` (Item / Eof / Err). The returned
    /// pointer is OWNED by the caller (the adapter/driver releases it).
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        match self.read() {
            Ok(Some(bytes)) => TaggedOutcome::Item(bytes_to_u8_array(&bytes)),
            Ok(None) => TaggedOutcome::Eof,
            Err(msg) => TaggedOutcome::Err(msg),
        }
    }
    /// Release the underlying resource (OS fd for a source; the upstream stream for an adapter).
    /// Called at most once (the box guards against double-close via its `closed` flag), either by
    /// explicit `close()` or by the finalizer.
    fn close(&mut self);

    /// Drive this source to completion on the calling thread → `Null | Error` (the `.drain()`
    /// terminal). The default pulls-and-discards every item (forcing a non-sink pipeline for
    /// effects); `WriteSink` overrides this to run its file-write loop. `_self_box` is the
    /// driving stream's own box (already locked by the caller) — passed for sinks that need it.
    unsafe fn drive(&mut self, _self_box: *const StreamBox) -> *mut u8 {
        // Pull-and-discard via the box would re-lock; instead pull directly through read_tagged
        // until EOF/Err. (The caller holds the box lock, so we must not re-lock it here.)
        loop {
            match self.read_tagged() {
                TaggedOutcome::Eof => return std::ptr::null_mut(),
                TaggedOutcome::Err(m) => return crate::fs::make_error_tagged(&m),
                TaggedOutcome::Item(item) => crate::tagged::lin_tagged_release(item),
            }
        }
    }
}

/// A tagged-item read outcome (the adapter-level analogue of `ReadOutcome`).
pub enum TaggedOutcome {
    /// An owned tagged item (`UInt8[]` chunk, `String` line, mapped value, …).
    Item(*mut u8),
    /// End of stream.
    Eof,
    /// An I/O / transform error message (becomes the canonical Error object at the boundary).
    Err(String),
}

/// Wrap a byte slice into a freshly-owned flat `UInt8[]` tagged array (`TaggedVal*(TAG_ARRAY)`).
unsafe fn bytes_to_u8_array(bytes: &[u8]) -> *mut u8 {
    use crate::array::{lin_flat_array_alloc_u8, lin_flat_array_push_u8};
    use crate::tagged::{alloc_tagged, TAG_ARRAY};
    let arr = lin_flat_array_alloc_u8(bytes.len().max(1) as u64);
    for b in bytes {
        lin_flat_array_push_u8(arr, *b);
    }
    alloc_tagged(TAG_ARRAY, arr as u64)
}

/// The heap box behind a `Stream<T>` value. Owns the read backend and tracks close state.
pub struct StreamBox {
    /// Refcount — atomic so the finalizer can never race a stray cross-thread touch.
    rc: AtomicU32,
    /// The read backend + the closed flag, guarded by a mutex. The mutex serializes reads (a
    /// stream is pulled by one driver at a time) and makes the box `Sync`. The `Option` is
    /// taken when closed, so a closed stream's backend is dropped immediately.
    state: Mutex<StreamState>,
}

struct StreamState {
    source: Option<Box<dyn StreamSource>>,
    closed: bool,
}

/// The result of a `lin_stream_read`, distinguishing EOF from an error from a chunk. Returned to
/// codegen / the stdlib adapter layer as a tagged value (see `read_result_to_tagged`).
pub enum ReadOutcome {
    /// A chunk of bytes (becomes a flat UInt8[] tagged array).
    Chunk(Vec<u8>),
    /// End of stream — becomes the Json `null` value (a null TaggedVal*).
    Eof,
    /// An I/O error — becomes a `{ type: "error", message }` tagged object (the canonical
    /// fallible-stdlib error shape).
    Err(String),
}

impl StreamBox {
    /// Allocate a `StreamBox` over `source`, boxed into a `TaggedVal*(TAG_STREAM)` with refcount 1.
    pub unsafe fn new_boxed(source: Box<dyn StreamSource>) -> *mut u8 {
        let b = Box::into_raw(Box::new(StreamBox {
            rc: AtomicU32::new(1),
            state: Mutex::new(StreamState { source: Some(source), closed: false }),
        }));
        crate::tagged::alloc_tagged(TAG_STREAM, b as u64)
    }
}

/// Box a raw `*const StreamBox` into a `TaggedVal*(TAG_STREAM)`.
pub unsafe fn box_stream(b: *const StreamBox) -> *mut u8 {
    crate::tagged::alloc_tagged(TAG_STREAM, b as u64)
}

/// Extract the `*const StreamBox` from a boxed `Stream` value (TAG_STREAM). Null/wrong-tag → null.
unsafe fn unwrap_stream(p: *const u8) -> *const StreamBox {
    if p.is_null() {
        return std::ptr::null();
    }
    let tv = &*(p as *const TaggedVal);
    if tv.tag == TAG_STREAM {
        tv.payload as *const StreamBox
    } else {
        std::ptr::null()
    }
}

/// If `s` is NOT a stream but IS the canonical Error object (`{ "type": "error", "message": … }`),
/// return its message. This is how a failed source (`readStream` of a missing file returns an
/// Error, not a Stream — `lin_fs_open`) is recognised when it flows into a stream adapter or
/// terminal: instead of dereferencing a null box (a process abort) or silently swallowing the
/// fault (returning Null as if the stream were empty), the operation threads the Error in-band,
/// exactly as a mid-pipeline read fault would. Returns `None` for a real stream or any non-error
/// value (which the caller treats as an empty/EOF stream, the prior behaviour).
unsafe fn stream_arg_error(s: *const u8) -> Option<String> {
    use crate::string::LinString;
    if s.is_null() {
        return None;
    }
    let tv = &*(s as *const TaggedVal);
    match tv.tag {
        crate::tagged::TAG_MAP => {
            let map = tv.payload as *const crate::map::LinMap;
            if map.is_null() { return None; }
            let get_str = |key: &str| -> Option<String> {
                let borrowed = crate::map::lin_map_get_bytes(map, key.as_ptr(), key.len() as u32);
                if borrowed.is_null() || (*borrowed).tag != crate::tagged::TAG_STR { return None; }
                let sp = (*borrowed).payload as *const LinString;
                let slice = std::slice::from_raw_parts((*sp).data.as_ptr(), (*sp).len as usize);
                std::str::from_utf8(slice).ok().map(|x| x.to_string())
            };
            match get_str("type") {
                Some(ref t) if t == "error" => Some(get_str("message").unwrap_or_else(|| "stream error".to_string())),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Pull the next BYTE chunk from a `StreamBox` (given a boxed `Stream` value), as a `ReadOutcome`.
/// Used by `lin_stream_read` (the low-level byte read). Closed/null → `Eof`.
pub unsafe fn stream_read_outcome(s: *const u8) -> ReadOutcome {
    let b = unwrap_stream(s);
    if b.is_null() {
        if let Some(m) = stream_arg_error(s) {
            return ReadOutcome::Err(m);
        }
    }
    match pull_tagged(b) {
        TaggedOutcome::Eof => ReadOutcome::Eof,
        TaggedOutcome::Err(m) => ReadOutcome::Err(m),
        TaggedOutcome::Item(item) => {
            // Convert the tagged UInt8[] item back to a Vec<u8> for the byte-level API. For a
            // base byte source the item IS a UInt8[]; the low-level read is only used on byte
            // streams, so this is faithful. (Adapters are pulled via `pull_tagged` directly.)
            use crate::tagged::{lin_get_tag, lin_unbox_ptr, TAG_ARRAY, lin_tagged_release};
            if lin_get_tag(item) == TAG_ARRAY {
                let arr = lin_unbox_ptr(item) as *const crate::array::LinArray;
                let n = crate::array::lin_array_length(arr) as usize;
                let mut v = Vec::with_capacity(n);
                let data = (*arr).data as *const u8;
                let elem_tag = (*arr).elem_tag;
                if elem_tag == crate::tagged::TAG_UINT8 || elem_tag == crate::tagged::TAG_INT8 {
                    for i in 0..n { v.push(*data.add(i)); }
                }
                lin_tagged_release(item);
                ReadOutcome::Chunk(v)
            } else {
                lin_tagged_release(item);
                ReadOutcome::Eof
            }
        }
    }
}

/// Pull the next TAGGED item from a raw `StreamBox*`. The single low-level pull every adapter and
/// terminal driver funnels through: closed/null → `Eof`; otherwise dispatch the backend's
/// `read_tagged`. Holds the box's mutex for the read (one driver pulls at a time).
pub unsafe fn pull_tagged(b: *const StreamBox) -> TaggedOutcome {
    if b.is_null() {
        return TaggedOutcome::Eof;
    }
    let mut guard = (*b).state.lock().unwrap();
    if guard.closed {
        return TaggedOutcome::Eof;
    }
    match guard.source.as_mut() {
        None => TaggedOutcome::Eof,
        Some(src) => src.read_tagged(),
    }
}

/// Close a `StreamBox` (given a boxed `Stream` value). Idempotent: closing an already-closed or
/// null stream is a no-op. Drops the backend (which releases the fd) and sets the closed flag.
pub unsafe fn stream_close(s: *const u8) {
    let b = unwrap_stream(s);
    if b.is_null() {
        return;
    }
    close_box(b);
}

/// Close the backend behind a raw `StreamBox*` exactly once. Used by both `stream_close` and the
/// finalizer; the `closed` flag guarantees the backend's `close` runs at most once.
unsafe fn close_box(b: *const StreamBox) {
    if b.is_null() {
        return;
    }
    let mut guard = (*b).state.lock().unwrap();
    if guard.closed {
        return;
    }
    guard.closed = true;
    if let Some(mut src) = guard.source.take() {
        src.close();
    }
}

// -------------------------------------------------------------------------
// File read backend (Stage 3). Reads a file in fixed-size byte chunks.
// -------------------------------------------------------------------------

/// Default read chunk size (bytes). Each `read` pulls up to this many bytes; a short read at the
/// tail still produces a chunk, and the following read returns EOF.
const FILE_CHUNK: usize = 64 * 1024;

/// Maximum length (bytes) of a single line buffered by `lines()` before its terminator is seen.
/// `LinesSource` accumulates upstream bytes until it finds a `\n`; without a bound, a pathological
/// input (a huge file with no newline) would buffer the entire stream into one allocation — an
/// unbounded-memory hazard that defeats the constant-memory promise of streaming. When a partial
/// line exceeds this cap, `lines()` fails in-band with an `Error` value (surfaced at the terminal,
/// like any other read error) rather than growing without limit. 64 MiB is far above any realistic
/// text line while still bounding the worst case.
const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;

/// A read backend over an open file. Owns the `File` (closing it drops the fd).
struct FileSource {
    file: Option<std::fs::File>,
}

impl StreamSource for FileSource {
    fn read(&mut self) -> Result<Option<Vec<u8>>, String> {
        use std::io::Read;
        let f = match self.file.as_mut() {
            Some(f) => f,
            None => return Ok(None),
        };
        let mut buf = vec![0u8; FILE_CHUNK];
        match f.read(&mut buf) {
            Ok(0) => Ok(None), // EOF
            Ok(n) => {
                buf.truncate(n);
                Ok(Some(buf))
            }
            Err(e) => Err(e.to_string()),
        }
    }
    fn close(&mut self) {
        // Drop the File — its Drop closes the fd. Idempotent (the box guards double-close).
        self.file.take();
    }
}

// Unified OS sources (Stage 5): TCP socket, child-process stdout, and stdin all become
// `Stream<UInt8[]>`, each supplying a different read backend (brief §4). The fd/handle-keyed
// sources delegate to the owning module's registry (net/process); stdin reads the global handle.

/// A TCP socket read backend. Holds the connected socket's fd; reads delegate to the net
/// registry (`net::tcp_stream_read`); close removes the socket from the registry (closing the fd).
struct TcpSource {
    fd: i32,
    open: bool,
}
impl StreamSource for TcpSource {
    fn read(&mut self) -> Result<Option<Vec<u8>>, String> {
        if !self.open {
            return Ok(None);
        }
        crate::net::tcp_stream_read(self.fd, FILE_CHUNK)
    }
    fn close(&mut self) {
        if self.open {
            crate::net::tcp_stream_close(self.fd);
            self.open = false;
        }
    }
}

/// A child-process stdout read backend. Holds the process handle; reads delegate to the process
/// registry (`process::process_stdout_read`). Close is a no-op here (the child's lifecycle —
/// kill/wait — is managed via std/process; the stream just stops reading).
struct ProcessSource {
    handle: i64,
}
impl StreamSource for ProcessSource {
    fn read(&mut self) -> Result<Option<Vec<u8>>, String> {
        crate::process::process_stdout_read(self.handle, FILE_CHUNK)
    }
    fn close(&mut self) {}
}

/// A stdin read backend. Reads from the process's standard input in chunks; `Ok(None)` at EOF.
struct StdinSource {
    done: bool,
}
impl StreamSource for StdinSource {
    fn read(&mut self) -> Result<Option<Vec<u8>>, String> {
        use std::io::Read;
        if self.done {
            return Ok(None);
        }
        let mut buf = vec![0u8; FILE_CHUNK];
        match std::io::stdin().lock().read(&mut buf) {
            Ok(0) => {
                self.done = true;
                Ok(None)
            }
            Ok(n) => {
                buf.truncate(n);
                Ok(Some(buf))
            }
            Err(e) => Err(e.to_string()),
        }
    }
    fn close(&mut self) {
        self.done = true;
    }
}

/// `tcpStream(fd)` → a `Stream<UInt8[]>` over a connected TCP socket.
#[no_mangle]
pub unsafe extern "C" fn lin_net_tcp_stream(fd: i32) -> *mut u8 {
    StreamBox::new_boxed(Box::new(TcpSource { fd, open: true }))
}

/// `stdoutStream(handle)` → a `Stream<UInt8[]>` over a child process's piped stdout.
#[no_mangle]
pub unsafe extern "C" fn lin_process_stdout_stream(handle: i64) -> *mut u8 {
    StreamBox::new_boxed(Box::new(ProcessSource { handle }))
}

/// `stdinStream()` → a `Stream<UInt8[]>` over the process's standard input.
#[no_mangle]
pub unsafe extern "C" fn lin_io_stdin_stream() -> *mut u8 {
    StreamBox::new_boxed(Box::new(StdinSource { done: false }))
}

/// `openRead(path)` → open `path` for reading and return a boxed `Stream<UInt8[]>`
/// (TaggedVal*(TAG_STREAM)). On open failure, returns a `{ type:"error", message }` tagged
/// object instead of a stream — so the result type is `Stream<UInt8[]> | Error` and the caller
/// branches on `is Error` before using the stream (matches the canonical fallible-stdlib shape).
#[no_mangle]
pub unsafe extern "C" fn lin_fs_open(path: *const u8) -> *mut u8 {
    let path_str = match crate::fs::resolve_lin_str(path) {
        Some(s) => s,
        None => return crate::fs::make_error_tagged("invalid UTF-8 path"),
    };
    match std::fs::File::open(&path_str) {
        Ok(file) => StreamBox::new_boxed(Box::new(FileSource { file: Some(file) })),
        Err(e) => crate::fs::make_error_tagged(&e.to_string()),
    }
}

// -------------------------------------------------------------------------
// Lazy adapter sources (Stage 4). Each wraps an upstream StreamBox and transforms its tagged
// items in-band. IN-BAND ERROR THREADING (brief §6): the pull funnel propagates `Err` straight
// through every adapter to the terminal op, so the chain stays fluent (no `is Error` per step).
// -------------------------------------------------------------------------

/// A retained Lin closure callable with one boxed arg → one boxed result. Holds a raw
/// `*mut LinClosure` (offset 8 = fn_ptr, offset 16 = env_ptr) with one owned reference; `Drop`
/// releases it. SAFETY: the closure (and its env) are only ever called/released on the thread
/// that DRIVES the pipeline. For `.drain()` that is the calling thread; when a pipeline is MOVED
/// to a worker (Stage 7/8) the whole graph — closures included — moves with it, so it is still
/// touched by exactly one thread. We assert `Send` on that single-owner-thread invariant.
struct LinFn {
    closure: *mut u8,
}
unsafe impl Send for LinFn {}

impl LinFn {
    /// Take ownership of a closure pointer (already +1 for us; we release it on drop).
    unsafe fn from_owned(closure: *mut u8) -> LinFn {
        LinFn { closure }
    }
    /// Call `closure(arg, index)` — `arg` is a boxed TaggedVal* (consumed/borrowed per the
    /// closure's ABI), `index` the element's 0-based ordinal. Returns the boxed result.
    ///
    /// ABI NOTE (the macOS/arm64 fault, fixed here): every combinator callback is type-checked
    /// against the std/iter signature `(T, Int32) => U`, so codegen emits the closure's boxed
    /// trampoline expecting a TRAILING boxed `Int32` index argument — `(env, item, index)`. The
    /// stream runtime previously called it as `(env, item)`, leaving the trailing `index` register
    /// uninitialised; the trampoline then unboxed that garbage as an `Int32` (`lin_unbox_int32`).
    /// On x86-64 the stale value was usually a benign 8-aligned pointer; on arm64 it was 4-aligned
    /// garbage, tripping Rust's debug `misaligned pointer dereference` abort. We now always pass a
    /// real boxed index so the trampoline's unbox reads a valid TaggedVal.
    unsafe fn call(&self, arg: *mut u8, index: i64) -> *mut u8 {
        if self.closure.is_null() {
            return std::ptr::null_mut();
        }
        let fn_ptr = *(self.closure.add(8) as *const *mut u8);
        let env_ptr = *(self.closure.add(16) as *const *mut u8);
        let idx = crate::tagged::lin_box_int32(index as i32);
        let call: unsafe extern "C-unwind" fn(*mut u8, *mut u8, *mut u8) -> *mut u8 =
            std::mem::transmute(fn_ptr);
        call(env_ptr, arg, idx)
    }

    /// Call a 2-arg closure `closure(arg0, arg1, index)` — the reduce ABI `(env, acc, item, index)
    /// -> acc`. All value args follow the boxed-TaggedVal* ABI; `index` is the boxed 0-based
    /// ordinal (see `call` for why the trailing index is required). Returns the boxed result.
    unsafe fn call2(&self, arg0: *mut u8, arg1: *mut u8, index: i64) -> *mut u8 {
        if self.closure.is_null() {
            return std::ptr::null_mut();
        }
        let fn_ptr = *(self.closure.add(8) as *const *mut u8);
        let env_ptr = *(self.closure.add(16) as *const *mut u8);
        let idx = crate::tagged::lin_box_int32(index as i32);
        let call: unsafe extern "C-unwind" fn(*mut u8, *mut u8, *mut u8, *mut u8) -> *mut u8 =
            std::mem::transmute(fn_ptr);
        call(env_ptr, arg0, arg1, idx)
    }

    /// 2-arg `call2` with the same fault-catching discipline as `call_caught` (see its doc). The
    /// caller releases `arg0`/`arg1` after a catch; we do not re-touch them.
    unsafe fn call2_caught(&self, arg0: *mut u8, arg1: *mut u8, index: i64) -> Result<*mut u8, String> {
        let closure_addr = self.closure as usize;
        let a0 = arg0 as usize;
        let a1 = arg1 as usize;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let f = LinFn { closure: closure_addr as *mut u8 };
            let r = f.call2(a0 as *mut u8, a1 as *mut u8, index);
            std::mem::forget(f); // do not release the borrowed closure here
            r
        }));
        match result {
            Ok(v) => Ok(v),
            Err(_) => Err("stream transform faulted".to_string()),
        }
    }

    /// Call the transform, CATCHING any unwinding fault (array OOB, division-by-zero, an explicit
    /// `panic`, …) and converting it to an in-band `Err(message)` (streams brief §8 fault
    /// boundary). This MUST wrap every transform call, because the surrounding drive path crosses
    /// `extern "C"` (non-unwind) ABI boundaries (`lin_stream_drain`/`_drive_owned`) — an
    /// uncaught panic there would ABORT the process ("panic in a function that cannot unwind")
    /// rather than surface as the awaited `Error`. The closure call itself is `extern "C-unwind"`,
    /// so the panic unwinds INTO this Rust frame, where `catch_unwind` can intercept it.
    unsafe fn call_caught(&self, arg: *mut u8, index: i64) -> Result<*mut u8, String> {
        let this = self.closure;
        let arg_addr = arg as usize;
        // `AssertUnwindSafe`: the closure pointer + arg are raw and not Rust-UnwindSafe, but a
        // caught fault leaves them in a defined (already-released-by-the-caller) state — we do not
        // re-touch `arg` after a catch (the caller releases it), so this is sound here.
        let closure_addr = this as usize;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let f = LinFn { closure: closure_addr as *mut u8 };
            let r = f.call(arg_addr as *mut u8, index);
            std::mem::forget(f); // do not drop (release) the borrowed closure here
            r
        }));
        match result {
            Ok(v) => Ok(v),
            Err(_) => Err("stream transform faulted".to_string()),
        }
    }
}

impl Drop for LinFn {
    fn drop(&mut self) {
        unsafe {
            if !self.closure.is_null() {
                crate::memory::lin_closure_release(self.closure);
            }
        }
    }
}

/// Bump a closure's refcount (offset-0 u32) and return it, taking one owned reference. Null-safe.
unsafe fn retain_closure(closure: *mut u8) -> *mut u8 {
    if !closure.is_null() {
        crate::memory::lin_rc_retain(closure as *mut u32);
    }
    closure
}

/// Shared adapter state: the OWNED upstream stream box (released on close). An adapter holds one
/// reference to its upstream; closing the adapter closes+releases the upstream (which closes the
/// fd when the last reference drops).
struct Upstream {
    /// Raw `*const StreamBox` with one owned reference. Released (via `lin_stream_release_box`,
    /// which runs the upstream's own close+finalizer) when the adapter closes.
    boxptr: *const StreamBox,
    /// A latched in-band error: set when the adapter was built over a non-stream Error value (a
    /// failed source, e.g. `readStream` of a missing file). The first pull surfaces it as an
    /// `Err`, so the fault propagates through the chain to the terminal instead of looking like an
    /// empty stream. Cleared after it is yielded once (then the upstream reads as EOF).
    pending_err: Option<String>,
}
unsafe impl Send for Upstream {}

impl Upstream {
    /// Pull the next item from upstream, surfacing a latched source error first (once).
    unsafe fn pull(&mut self) -> TaggedOutcome {
        if let Some(m) = self.pending_err.take() {
            return TaggedOutcome::Err(m);
        }
        pull_tagged(self.boxptr)
    }
    unsafe fn close(&mut self) {
        if !self.boxptr.is_null() {
            // Closing the adapter eagerly closes the upstream resource (deterministic), then
            // drops our owned reference. lin_stream_release_box handles the refcount + finalizer.
            close_box(self.boxptr);
            lin_stream_release_box(self.boxptr as *const u8);
            self.boxptr = std::ptr::null();
        }
    }
}

/// `map(s, f)`: apply `f` to every item. A transform that itself produces an `Error` value
/// poisons the chain (the Error flows downstream as the item; the terminal op surfaces it).
struct MapSource {
    up: Upstream,
    f: LinFn,
    /// 0-based ordinal of the next item, passed as the callback's trailing `Int32` index arg.
    idx: i64,
}
impl StreamSource for MapSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        match self.up.pull() {
            TaggedOutcome::Eof => TaggedOutcome::Eof,
            TaggedOutcome::Err(m) => TaggedOutcome::Err(m),
            TaggedOutcome::Item(item) => {
                let i = self.idx;
                self.idx += 1;
                let out = self.f.call_caught(item, i);
                // The closure consumed/derived `item`; release our reference to it. (The boxed
                // result `out` is independently owned and returned to the driver.)
                crate::tagged::lin_tagged_release(item);
                match out {
                    Ok(v) => TaggedOutcome::Item(v),
                    // A transform fault becomes an in-band Err that short-circuits to the terminal.
                    Err(m) => TaggedOutcome::Err(m),
                }
            }
        }
    }
    fn close(&mut self) {
        unsafe { self.up.close(); }
    }
}

/// `filter(s, p)`: keep items for which `p(item)` is truthy. Pulls until a kept item or EOF/Err.
struct FilterSource {
    up: Upstream,
    p: LinFn,
    /// 0-based ordinal of the next item, passed as the callback's trailing `Int32` index arg.
    idx: i64,
}
impl StreamSource for FilterSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        loop {
            match self.up.pull() {
                TaggedOutcome::Eof => return TaggedOutcome::Eof,
                TaggedOutcome::Err(m) => return TaggedOutcome::Err(m),
                TaggedOutcome::Item(item) => {
                    let i = self.idx;
                    self.idx += 1;
                    let verdict = match self.p.call_caught(item, i) {
                        Ok(v) => v,
                        Err(m) => {
                            crate::tagged::lin_tagged_release(item);
                            return TaggedOutcome::Err(m);
                        }
                    };
                    let keep = crate::tagged::lin_get_tag(verdict) == crate::tagged::TAG_BOOL
                        && crate::tagged::lin_unbox_bool(verdict) != 0;
                    crate::tagged::lin_tagged_release(verdict);
                    if keep {
                        return TaggedOutcome::Item(item);
                    }
                    crate::tagged::lin_tagged_release(item);
                    // else: drop and pull the next.
                }
            }
        }
    }
    fn close(&mut self) {
        unsafe { self.up.close(); }
    }
}

/// `take(s, n)`: yield at most `n` items, then EOF (without pulling further upstream).
struct TakeSource {
    up: Upstream,
    remaining: i64,
}
impl StreamSource for TakeSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        if self.remaining <= 0 {
            return TaggedOutcome::Eof;
        }
        match self.up.pull() {
            TaggedOutcome::Eof => TaggedOutcome::Eof,
            TaggedOutcome::Err(m) => TaggedOutcome::Err(m),
            TaggedOutcome::Item(item) => {
                self.remaining -= 1;
                TaggedOutcome::Item(item)
            }
        }
    }
    fn close(&mut self) {
        unsafe { self.up.close(); }
    }
}

/// `lines(s)`: re-frame a `UInt8[]` byte stream into `String` lines (split on `\n`, `\r\n`
/// tolerated by trimming a trailing `\r`). Buffers partial lines across chunks; flushes a final
/// unterminated line at EOF.
struct LinesSource {
    up: Upstream,
    buf: Vec<u8>,
    upstream_done: bool,
    /// Cap on a single partial line before failing in-band (see `MAX_LINE_BYTES`). Configurable
    /// via `linesMax(s, n)`; `lines(s)` uses the default.
    max_line_bytes: usize,
}
impl LinesSource {
    /// Pop the next complete line from `buf` (up to and including a `\n`), returning the line
    /// bytes WITHOUT the terminator. Returns None if `buf` holds no full line.
    fn pop_line(&mut self) -> Option<Vec<u8>> {
        if let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop(); // drop '\n'
            if line.last() == Some(&b'\r') {
                line.pop(); // drop '\r' (CRLF)
            }
            Some(line)
        } else {
            None
        }
    }
}
impl StreamSource for LinesSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        loop {
            if let Some(line) = self.pop_line() {
                return TaggedOutcome::Item(string_to_tagged(&line));
            }
            if self.upstream_done {
                // Flush a final unterminated line, if any.
                if self.buf.is_empty() {
                    return TaggedOutcome::Eof;
                }
                let line = std::mem::take(&mut self.buf);
                return TaggedOutcome::Item(string_to_tagged(&line));
            }
            match self.up.pull() {
                TaggedOutcome::Eof => self.upstream_done = true,
                TaggedOutcome::Err(m) => return TaggedOutcome::Err(m),
                TaggedOutcome::Item(item) => {
                    append_u8_array_to(&mut self.buf, item);
                    crate::tagged::lin_tagged_release(item);
                    // Bound the partial-line buffer: a stream with no newline must not grow an
                    // unbounded allocation. Fail in-band once a single line exceeds the cap.
                    if self.buf.len() > self.max_line_bytes {
                        let cap = self.max_line_bytes;
                        self.buf.clear();
                        return TaggedOutcome::Err(format!(
                            "lines(): a single line exceeded {} bytes without a newline — refusing to buffer unbounded input",
                            cap
                        ));
                    }
                }
            }
        }
    }
    fn close(&mut self) {
        unsafe { self.up.close(); }
    }
}

/// `chunks(s, n)`: re-chunk a `UInt8[]` byte stream into fixed-size `UInt8[]` pieces of `n` bytes
/// (the final piece may be shorter). Buffers across upstream chunks.
struct ChunksSource {
    up: Upstream,
    size: usize,
    buf: Vec<u8>,
    upstream_done: bool,
}
impl StreamSource for ChunksSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        loop {
            if self.buf.len() >= self.size && self.size > 0 {
                let piece: Vec<u8> = self.buf.drain(..self.size).collect();
                return TaggedOutcome::Item(bytes_to_u8_array(&piece));
            }
            if self.upstream_done {
                if self.buf.is_empty() {
                    return TaggedOutcome::Eof;
                }
                let piece = std::mem::take(&mut self.buf);
                return TaggedOutcome::Item(bytes_to_u8_array(&piece));
            }
            match self.up.pull() {
                TaggedOutcome::Eof => self.upstream_done = true,
                TaggedOutcome::Err(m) => return TaggedOutcome::Err(m),
                TaggedOutcome::Item(item) => {
                    append_u8_array_to(&mut self.buf, item);
                    crate::tagged::lin_tagged_release(item);
                }
            }
        }
    }
    fn close(&mut self) {
        unsafe { self.up.close(); }
    }
}

// -------------------------------------------------------------------------
// Net-new lazy adapter sources (std/iter unification Stage 3). Each mirrors the MapSource/
// FilterSource/TakeSource RC discipline EXACTLY: pull the upstream item, transform/decide,
// release the pulled item after the closure consumes it, return an independently-owned box,
// propagate `Err` straight through, and close the upstream in `close()`.
// -------------------------------------------------------------------------

/// True if a boxed tagged value is "truthy" for predicate purposes: a Bool reads its bit; any
/// other non-null value is truthy; null is falsy. Matches the filter adapter's Bool check but is
/// tolerant of non-Bool predicate results (a predicate returning e.g. an object is truthy).
unsafe fn is_truthy(v: *const u8) -> bool {
    use crate::tagged::{lin_get_tag, lin_unbox_bool, TAG_BOOL, TAG_NULL};
    match lin_get_tag(v) {
        TAG_NULL => false,
        TAG_BOOL => lin_unbox_bool(v) != 0,
        _ => true,
    }
}

/// `drop(s, n)`: discard the first `n` items, then pass through. Pulls-and-discards on the first
/// reads until `n` items have been dropped (propagating Err during the skip), then yields.
struct DropSource {
    up: Upstream,
    remaining: i64,
}
impl StreamSource for DropSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        while self.remaining > 0 {
            match self.up.pull() {
                TaggedOutcome::Eof => return TaggedOutcome::Eof,
                TaggedOutcome::Err(m) => return TaggedOutcome::Err(m),
                TaggedOutcome::Item(item) => {
                    crate::tagged::lin_tagged_release(item);
                    self.remaining -= 1;
                }
            }
        }
        self.up.pull()
    }
    fn close(&mut self) {
        unsafe { self.up.close(); }
    }
}

/// `takeWhile(s, p)`: yield items while `p(item)` is truthy; on the first false, EOF and stop
/// pulling upstream entirely (a `done` latch makes subsequent reads return Eof without a pull).
struct TakeWhileSource {
    up: Upstream,
    p: LinFn,
    done: bool,
    /// 0-based ordinal of the next item, passed as the callback's trailing `Int32` index arg.
    idx: i64,
}
impl StreamSource for TakeWhileSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        if self.done {
            return TaggedOutcome::Eof;
        }
        match self.up.pull() {
            TaggedOutcome::Eof => {
                self.done = true;
                TaggedOutcome::Eof
            }
            TaggedOutcome::Err(m) => TaggedOutcome::Err(m),
            TaggedOutcome::Item(item) => {
                let i = self.idx;
                self.idx += 1;
                let verdict = match self.p.call_caught(item, i) {
                    Ok(v) => v,
                    Err(m) => {
                        crate::tagged::lin_tagged_release(item);
                        return TaggedOutcome::Err(m);
                    }
                };
                let keep = is_truthy(verdict);
                crate::tagged::lin_tagged_release(verdict);
                if keep {
                    TaggedOutcome::Item(item)
                } else {
                    // First false: drop this item, latch done, and stop pulling upstream.
                    crate::tagged::lin_tagged_release(item);
                    self.done = true;
                    TaggedOutcome::Eof
                }
            }
        }
    }
    fn close(&mut self) {
        unsafe { self.up.close(); }
    }
}

/// `dropWhile(s, p)`: skip items while `p(item)` is truthy; once `p` is first false, yield that
/// item and every subsequent item unconditionally (a `dropping` latch records the transition).
struct DropWhileSource {
    up: Upstream,
    p: LinFn,
    dropping: bool,
    /// 0-based ordinal of the next item evaluated by `p` (the dropping phase), passed as the
    /// callback's trailing `Int32` index arg. Frozen once `dropping` ends (p is no longer called).
    idx: i64,
}
impl StreamSource for DropWhileSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        loop {
            match self.up.pull() {
                TaggedOutcome::Eof => return TaggedOutcome::Eof,
                TaggedOutcome::Err(m) => return TaggedOutcome::Err(m),
                TaggedOutcome::Item(item) => {
                    if !self.dropping {
                        return TaggedOutcome::Item(item);
                    }
                    let i = self.idx;
                    self.idx += 1;
                    let verdict = match self.p.call_caught(item, i) {
                        Ok(v) => v,
                        Err(m) => {
                            crate::tagged::lin_tagged_release(item);
                            return TaggedOutcome::Err(m);
                        }
                    };
                    let still_drop = is_truthy(verdict);
                    crate::tagged::lin_tagged_release(verdict);
                    if still_drop {
                        // Still in the dropping phase: discard and pull the next.
                        crate::tagged::lin_tagged_release(item);
                    } else {
                        // Transition: stop dropping and yield this first kept item.
                        self.dropping = false;
                        return TaggedOutcome::Item(item);
                    }
                }
            }
        }
    }
    fn close(&mut self) {
        unsafe { self.up.close(); }
    }
}

/// A lazily-flattened inner collection: a boxed array `arr` (TAG_ARRAY) we hold ONE owned
/// reference to, plus a cursor. `next()` yields each element as a freshly-owned box (via
/// `lin_array_get_tagged`, which retains the element); when exhausted it releases the held array.
struct InnerCursor {
    /// One owned reference to a boxed array; released when the cursor is exhausted or dropped.
    arr_box: *mut u8,
    idx: i64,
    len: i64,
}
// Same single-owner-thread invariant as `LinFn`/`Upstream`: a stream pipeline (cursor included)
// is only ever touched by the thread that drives it.
unsafe impl Send for InnerCursor {}
impl InnerCursor {
    /// Build a cursor over a boxed item that should be an array. Non-array items (or null) yield
    /// an empty cursor (the boxed item is released immediately). Takes ownership of `item`.
    unsafe fn new(item: *mut u8) -> InnerCursor {
        use crate::tagged::{lin_get_tag, lin_unbox_ptr, TAG_ARRAY};
        if lin_get_tag(item) == TAG_ARRAY {
            let arr = lin_unbox_ptr(item) as *const crate::array::LinArray;
            let len = crate::array::lin_array_length(arr);
            InnerCursor { arr_box: item, idx: 0, len }
        } else {
            // Not a flattenable collection — discard and present as empty.
            crate::tagged::lin_tagged_release(item);
            InnerCursor { arr_box: std::ptr::null_mut(), idx: 0, len: 0 }
        }
    }
    /// Yield the next element as a freshly-owned box, or None when exhausted (releasing the array).
    unsafe fn next(&mut self) -> Option<*mut u8> {
        if self.arr_box.is_null() || self.idx >= self.len {
            self.release();
            return None;
        }
        let arr = crate::tagged::lin_unbox_ptr(self.arr_box) as *const crate::array::LinArray;
        let elem = crate::array::lin_array_get_tagged(arr, self.idx) as *mut u8;
        self.idx += 1;
        Some(elem)
    }
    unsafe fn release(&mut self) {
        if !self.arr_box.is_null() {
            crate::tagged::lin_tagged_release(self.arr_box);
            self.arr_box = std::ptr::null_mut();
        }
    }
}

/// `flatMap(s, f)`: `f(item)` returns a collection (array) per item; flatten lazily. Holds an
/// optional CURRENT inner cursor; drains it before pulling the next upstream item and calling `f`.
struct FlatMapSource {
    up: Upstream,
    f: LinFn,
    current: Option<InnerCursor>,
    /// 0-based ordinal of the next UPSTREAM item passed to `f`, as the trailing `Int32` index arg.
    idx: i64,
}
impl StreamSource for FlatMapSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        loop {
            // Drain the current inner collection first.
            if let Some(cur) = self.current.as_mut() {
                if let Some(elem) = cur.next() {
                    return TaggedOutcome::Item(elem);
                }
                // Exhausted (next() released the held array): drop the cursor and pull next.
                self.current = None;
            }
            match self.up.pull() {
                TaggedOutcome::Eof => return TaggedOutcome::Eof,
                TaggedOutcome::Err(m) => return TaggedOutcome::Err(m),
                TaggedOutcome::Item(item) => {
                    let i = self.idx;
                    self.idx += 1;
                    let inner = match self.f.call_caught(item, i) {
                        Ok(v) => v,
                        Err(m) => {
                            crate::tagged::lin_tagged_release(item);
                            return TaggedOutcome::Err(m);
                        }
                    };
                    // The closure consumed/derived `item`; release our pulled reference. The
                    // returned `inner` collection is independently owned — hand it to a cursor.
                    crate::tagged::lin_tagged_release(item);
                    self.current = Some(InnerCursor::new(inner));
                }
            }
        }
    }
    fn close(&mut self) {
        unsafe {
            if let Some(mut cur) = self.current.take() {
                cur.release();
            }
            self.up.close();
        }
    }
}

/// `flatten(s)`: `s` is a stream of collections; flatten lazily (flatMap with the identity
/// transform). Reuses `InnerCursor` directly over each pulled item, no closure call.
struct FlattenSource {
    up: Upstream,
    current: Option<InnerCursor>,
}
impl StreamSource for FlattenSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        loop {
            if let Some(cur) = self.current.as_mut() {
                if let Some(elem) = cur.next() {
                    return TaggedOutcome::Item(elem);
                }
                self.current = None;
            }
            match self.up.pull() {
                TaggedOutcome::Eof => return TaggedOutcome::Eof,
                TaggedOutcome::Err(m) => return TaggedOutcome::Err(m),
                // The pulled item IS the inner collection; the cursor takes ownership of it.
                TaggedOutcome::Item(item) => {
                    self.current = Some(InnerCursor::new(item));
                }
            }
        }
    }
    fn close(&mut self) {
        unsafe {
            if let Some(mut cur) = self.current.take() {
                cur.release();
            }
            self.up.close();
        }
    }
}

/// `concat(a, b)`: yield every item of `a`, then every item of `b`. Holds BOTH upstreams (each
/// retained via `own_upstream`); `close()` closes both. RC: the active upstream's items are
/// returned verbatim to the driver (no extra release — each item is independently owned by its
/// source's `read_tagged`).
struct ConcatSource {
    a: Upstream,
    b: Upstream,
    on_b: bool,
}
impl StreamSource for ConcatSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        if !self.on_b {
            match self.a.pull() {
                TaggedOutcome::Eof => {
                    // First stream exhausted: close it eagerly, switch to the second.
                    self.a.close();
                    self.on_b = true;
                }
                other => return other,
            }
        }
        self.b.pull()
    }
    fn close(&mut self) {
        unsafe {
            self.a.close();
            self.b.close();
        }
    }
}

// -------------------------------------------------------------------------
// Enrichment lazy adapter + infinite-source nodes (iter-combinators proposal). Each mirrors the
// MapSource/FilterSource RC discipline: pull the upstream item, transform/decide, release a pulled
// item once consumed, return an independently-owned box, propagate `Err` straight through, and
// close the upstream in `close()`. The infinite sources (count/repeat/cycle) own NO upstream — they
// generate values forever and MUST be bounded downstream by `take`/`takeWhile`/`find`/`some`.
// -------------------------------------------------------------------------

/// Build a freshly-owned boxed TAGGED array (`TaggedVal*(TAG_ARRAY)`) holding clones of `items`,
/// in order. Each element is `lin_tagged_clone`'d so the array owns its own +1 (the ring buffer /
/// caller keeps its own references). Used to materialise a `sliding`/`pairwise` window.
unsafe fn window_array_from(items: &[*mut u8]) -> *mut u8 {
    use crate::tagged::{alloc_tagged, TAG_ARRAY};
    let arr = crate::array::lin_array_alloc(items.len().max(1) as u64);
    for &it in items {
        let cloned = crate::tagged::lin_tagged_clone(it);
        crate::array::lin_array_push_tagged(arr as *mut crate::array::LinArray, cloned);
    }
    alloc_tagged(TAG_ARRAY, arr as u64)
}

/// `sliding(s, size)`: overlapping fixed-width windows advancing by one. Keeps a bounded ring
/// buffer (`buf`) of the last `size` PULLED items; once full, emits a window per subsequently
/// pulled item. A source shorter than `size` yields no windows. The held items are released on
/// close (or as they age out of the window).
struct SlidingSource {
    up: Upstream,
    size: usize,
    /// Owned references to the last (up to) `size` pulled items, oldest first.
    buf: Vec<*mut u8>,
}
unsafe impl Send for SlidingSource {}
impl StreamSource for SlidingSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        loop {
            match self.up.pull() {
                TaggedOutcome::Eof => return TaggedOutcome::Eof,
                TaggedOutcome::Err(m) => return TaggedOutcome::Err(m),
                TaggedOutcome::Item(item) => {
                    // Append the new item (we own it); drop the oldest if over capacity.
                    self.buf.push(item);
                    if self.buf.len() > self.size {
                        let old = self.buf.remove(0);
                        crate::tagged::lin_tagged_release(old);
                    }
                    if self.buf.len() == self.size {
                        return TaggedOutcome::Item(window_array_from(&self.buf));
                    }
                    // Not enough items yet: pull again.
                }
            }
        }
    }
    fn close(&mut self) {
        unsafe {
            for &it in &self.buf {
                crate::tagged::lin_tagged_release(it);
            }
            self.buf.clear();
            self.up.close();
        }
    }
}

/// `pairwise(s)`: adjacent overlapping pairs as 2-element arrays. Holds the single previous item;
/// emits `[prev, item]` once a second item arrives, then slides. Equivalent to `sliding(s, 2)` with
/// a fixed 2-tuple shape (built inline, not delegated).
struct PairwiseSource {
    up: Upstream,
    /// One owned reference to the previous pulled item, or None before the first pull.
    prev: Option<*mut u8>,
}
unsafe impl Send for PairwiseSource {}
impl StreamSource for PairwiseSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        loop {
            match self.up.pull() {
                TaggedOutcome::Eof => return TaggedOutcome::Eof,
                TaggedOutcome::Err(m) => return TaggedOutcome::Err(m),
                TaggedOutcome::Item(item) => {
                    match self.prev.take() {
                        None => {
                            // First item: stash and pull again.
                            self.prev = Some(item);
                        }
                        Some(prev) => {
                            let pair = window_array_from(&[prev, item]);
                            // The new item becomes the next pair's left; release the old left.
                            crate::tagged::lin_tagged_release(prev);
                            self.prev = Some(item);
                            return TaggedOutcome::Item(pair);
                        }
                    }
                }
            }
        }
    }
    fn close(&mut self) {
        unsafe {
            if let Some(p) = self.prev.take() {
                crate::tagged::lin_tagged_release(p);
            }
            self.up.close();
        }
    }
}

/// `intersperse(s, sep)`: insert `sep` between adjacent items (not before the first, not after the
/// last). Holds one owned reference to `sep` (cloned per emission); a one-bit `emitted_first` latch
/// plus a `pending` item buffer alternate item / separator without an extra upstream pull.
struct IntersperseSource {
    up: Upstream,
    /// One owned reference to the separator value (cloned for each emitted copy).
    sep: *mut u8,
    /// Have we emitted at least one item yet?
    emitted_first: bool,
    /// A pulled item waiting to be emitted AFTER a separator (set when we owe a separator first).
    pending: Option<*mut u8>,
}
unsafe impl Send for IntersperseSource {}
impl StreamSource for IntersperseSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        // If we owe an item that follows a just-emitted separator, emit it now.
        if let Some(item) = self.pending.take() {
            return TaggedOutcome::Item(item);
        }
        match self.up.pull() {
            TaggedOutcome::Eof => TaggedOutcome::Eof,
            TaggedOutcome::Err(m) => TaggedOutcome::Err(m),
            TaggedOutcome::Item(item) => {
                if !self.emitted_first {
                    self.emitted_first = true;
                    TaggedOutcome::Item(item)
                } else {
                    // Emit the separator now; stash the item to emit on the next pull.
                    self.pending = Some(item);
                    TaggedOutcome::Item(crate::tagged::lin_tagged_clone(self.sep))
                }
            }
        }
    }
    fn close(&mut self) {
        unsafe {
            if let Some(p) = self.pending.take() {
                crate::tagged::lin_tagged_release(p);
            }
            if !self.sep.is_null() {
                crate::tagged::lin_tagged_release(self.sep);
                self.sep = std::ptr::null_mut();
            }
            self.up.close();
        }
    }
}

/// `dedup(s)`: collapse CONSECUTIVE runs of equal items (deep structural equality, `lin_tagged_eq`)
/// to a single item. Holds one owned reference to the last EMITTED value for the compare.
struct DedupSource {
    up: Upstream,
    /// One owned reference to the last emitted value, or None before the first emit.
    last: Option<*mut u8>,
}
unsafe impl Send for DedupSource {}
impl StreamSource for DedupSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        loop {
            match self.up.pull() {
                TaggedOutcome::Eof => return TaggedOutcome::Eof,
                TaggedOutcome::Err(m) => return TaggedOutcome::Err(m),
                TaggedOutcome::Item(item) => {
                    let dup = match self.last {
                        Some(prev) => crate::tagged::lin_tagged_eq(prev as *const u8, item as *const u8) != 0,
                        None => false,
                    };
                    if dup {
                        // Same as the last emitted value: drop and pull the next.
                        crate::tagged::lin_tagged_release(item);
                        continue;
                    }
                    // New value: it becomes the last-emitted (we keep our own clone), then emit it.
                    if let Some(old) = self.last.take() {
                        crate::tagged::lin_tagged_release(old);
                    }
                    self.last = Some(crate::tagged::lin_tagged_clone(item));
                    return TaggedOutcome::Item(item);
                }
            }
        }
    }
    fn close(&mut self) {
        unsafe {
            if let Some(p) = self.last.take() {
                crate::tagged::lin_tagged_release(p);
            }
            self.up.close();
        }
    }
}

/// `zipWith(a, b, f)` (stream arm): pull `a` lazily, index into the captured in-memory boxed array
/// `b`, and emit `f(item, b[i])`. Ends when `a` ends or `b` is exhausted. Holds one owned reference
/// to the boxed `b` array and a retained `f`.
struct ZipWithSource {
    up: Upstream,
    /// One owned reference to the boxed `B[]` (TAG_ARRAY) we index into.
    b: *mut u8,
    b_len: i64,
    f: LinFn,
    idx: i64,
}
unsafe impl Send for ZipWithSource {}
impl StreamSource for ZipWithSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        if self.idx >= self.b_len {
            return TaggedOutcome::Eof;
        }
        match self.up.pull() {
            TaggedOutcome::Eof => TaggedOutcome::Eof,
            TaggedOutcome::Err(m) => TaggedOutcome::Err(m),
            TaggedOutcome::Item(item) => {
                let i = self.idx;
                self.idx += 1;
                // b[i] as a freshly-owned box (lin_array_get_tagged retains the inner).
                let barr = crate::tagged::lin_unbox_ptr(self.b) as *const crate::array::LinArray;
                let belem = crate::array::lin_array_get_tagged(barr, i) as *mut u8;
                let out = self.f.call2_caught(item, belem, i);
                crate::tagged::lin_tagged_release(item);
                crate::tagged::lin_tagged_release(belem);
                match out {
                    Ok(v) => TaggedOutcome::Item(v),
                    Err(m) => TaggedOutcome::Err(m),
                }
            }
        }
    }
    fn close(&mut self) {
        unsafe {
            if !self.b.is_null() {
                crate::tagged::lin_tagged_release(self.b);
                self.b = std::ptr::null_mut();
            }
            self.up.close();
        }
    }
}

/// `count(start, step)`: an INFINITE counting source: start, start+step, … . Owns no upstream.
struct CountSource {
    next: i64,
    step: i64,
}
unsafe impl Send for CountSource {}
impl StreamSource for CountSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        let v = self.next;
        self.next = self.next.wrapping_add(self.step);
        TaggedOutcome::Item(crate::tagged::lin_box_int32(v as i32))
    }
    fn close(&mut self) {}
}

/// `repeat(value, n)`: yield `value` `n` times, or infinitely when `n < 0`. Owns one reference to
/// the boxed `value` (cloned per emission).
struct RepeatSource {
    value: *mut u8,
    /// Remaining count, or -1 for infinite.
    remaining: i64,
    infinite: bool,
}
unsafe impl Send for RepeatSource {}
impl StreamSource for RepeatSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        if !self.infinite {
            if self.remaining <= 0 {
                return TaggedOutcome::Eof;
            }
            self.remaining -= 1;
        }
        TaggedOutcome::Item(crate::tagged::lin_tagged_clone(self.value))
    }
    fn close(&mut self) {
        unsafe {
            if !self.value.is_null() {
                crate::tagged::lin_tagged_release(self.value);
                self.value = std::ptr::null_mut();
            }
        }
    }
}

/// `cycle(arr)`: repeat the elements of a finite boxed array `arr` endlessly. An empty `arr` yields
/// EOF immediately (no infinite loop over nothing). Owns one reference to the boxed array.
struct CycleSource {
    arr: *mut u8,
    len: i64,
    idx: i64,
}
unsafe impl Send for CycleSource {}
impl StreamSource for CycleSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        if self.len <= 0 {
            return TaggedOutcome::Eof;
        }
        let i = self.idx % self.len;
        self.idx = (self.idx + 1) % self.len;
        let a = crate::tagged::lin_unbox_ptr(self.arr) as *const crate::array::LinArray;
        let elem = crate::array::lin_array_get_tagged(a, i) as *mut u8;
        TaggedOutcome::Item(elem)
    }
    fn close(&mut self) {
        unsafe {
            if !self.arr.is_null() {
                crate::tagged::lin_tagged_release(self.arr);
                self.arr = std::ptr::null_mut();
            }
        }
    }
}

// -------------------------------------------------------------------------
// Streaming compression byte-adapters (std/compress). gzip/gunzip use the gzip container;
// deflate/inflate use the raw DEFLATE bitstream. Each wraps a `Stream<UInt8[]>` and runs its bytes
// through the low-level streaming flate2 `Compress`/`Decompress` engine INCREMENTALLY: each
// `read_tagged` pulls ONE upstream chunk, feeds it through the codec, and emits whatever output
// bytes were produced. At upstream EOF the codec is FINISHED (flushed) and the tail emitted. A
// decode/encode fault becomes an in-band `TaggedOutcome::Err`. RC discipline mirrors the other
// adapters EXACTLY: release each pulled item after consuming its bytes; return a freshly-owned
// `UInt8[]` box; propagate upstream `Err` straight through; close the upstream in `close()`.
// -------------------------------------------------------------------------

use flate2::write::{DeflateDecoder, DeflateEncoder, GzEncoder, MultiGzDecoder};
use flate2::Compression;
use std::io::Write;

// The (de)compression engine behind a `CodecSource`. We use flate2's WRITE-based streaming
// wrappers, each writing into an owned `Vec<u8>` sink: feeding a chunk via `write_all` runs it
// through the engine and appends whatever output bytes were produced to the sink — i.e. genuinely
// INCREMENTAL, one upstream chunk at a time (no whole-buffer convenience fns). After each feed we
// drain the sink; at upstream EOF we `try_finish()` and drain the tail.
//
// (Deviation note: the brief asked for the low-level `Compress`/`Decompress` mem API. With the
// pure-Rust `miniz_oxide` backend the gzip CONTAINER framing is only reachable through these
// write wrappers — `Compress::new_gzip`/`Decompress::new_gzip` are gated behind a C-zlib feature
// we deliberately do not enable. The write wrappers give the same incremental, chunk-at-a-time
// behaviour, so all four codecs share this one uniform driver.)
enum Codec {
    GzEnc(GzEncoder<Vec<u8>>),
    GzDec(MultiGzDecoder<Vec<u8>>),
    DfEnc(DeflateEncoder<Vec<u8>>),
    DfDec(DeflateDecoder<Vec<u8>>),
}

impl Codec {
    /// Feed `input` through the engine; the engine appends produced bytes to its internal sink.
    fn feed(&mut self, input: &[u8]) -> std::io::Result<()> {
        match self {
            Codec::GzEnc(w) => w.write_all(input),
            Codec::GzDec(w) => w.write_all(input),
            Codec::DfEnc(w) => w.write_all(input),
            Codec::DfDec(w) => w.write_all(input),
        }
    }
    /// Finish the engine (flush the tail into the sink).
    fn finish(&mut self) -> std::io::Result<()> {
        match self {
            Codec::GzEnc(w) => w.try_finish(),
            Codec::GzDec(w) => w.try_finish(),
            Codec::DfEnc(w) => w.try_finish(),
            Codec::DfDec(w) => w.try_finish(),
        }
    }
    /// Drain (take) the bytes accumulated in the engine's sink so far.
    fn take_sink(&mut self) -> Vec<u8> {
        match self {
            Codec::GzEnc(w) => std::mem::take(w.get_mut()),
            Codec::GzDec(w) => std::mem::take(w.get_mut()),
            Codec::DfEnc(w) => std::mem::take(w.get_mut()),
            Codec::DfDec(w) => std::mem::take(w.get_mut()),
        }
    }
}

/// A streaming (de)compression adapter over an upstream byte stream.
struct CodecSource {
    up: Upstream,
    codec: Codec,
    /// True once the upstream has signalled EOF; the next drive FINISHES the codec.
    upstream_done: bool,
    /// True once the codec has been finished+flushed and its tail emitted — further reads → EOF.
    finished: bool,
}
// Same single-owner-thread invariant as the other adapter sources: a stream pipeline is only ever
// touched by the thread that drives it (Send asserted on that invariant).
unsafe impl Send for CodecSource {}

impl StreamSource for CodecSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        loop {
            if self.finished {
                return TaggedOutcome::Eof;
            }
            // At upstream EOF, finish the codec once and emit the flushed tail.
            if self.upstream_done {
                let fin = self.codec.finish();
                let tail = self.codec.take_sink();
                self.finished = true;
                if let Err(e) = fin {
                    return TaggedOutcome::Err(e.to_string());
                }
                if !tail.is_empty() {
                    return TaggedOutcome::Item(bytes_to_u8_array(&tail));
                }
                return TaggedOutcome::Eof;
            }
            match self.up.pull() {
                TaggedOutcome::Eof => {
                    self.upstream_done = true;
                    // Loop back to run the finish flush.
                }
                TaggedOutcome::Err(m) => return TaggedOutcome::Err(m),
                TaggedOutcome::Item(item) => {
                    // Gather the chunk's bytes, release our pulled reference, feed the engine.
                    let mut chunk = Vec::new();
                    append_u8_array_to(&mut chunk, item);
                    crate::tagged::lin_tagged_release(item);
                    if let Err(e) = self.codec.feed(&chunk) {
                        self.finished = true;
                        return TaggedOutcome::Err(e.to_string());
                    }
                    let out = self.codec.take_sink();
                    if !out.is_empty() {
                        return TaggedOutcome::Item(bytes_to_u8_array(&out));
                    }
                    // Produced nothing this chunk (codec buffering) — pull the next.
                }
            }
        }
    }
    fn close(&mut self) {
        unsafe { self.up.close(); }
    }
}

/// Append the bytes of a tagged `UInt8[]` array `item` to `buf`. Non-array items contribute
/// nothing (defensive; a byte pipeline only ever carries UInt8[] here).
unsafe fn append_u8_array_to(buf: &mut Vec<u8>, item: *mut u8) {
    use crate::tagged::{lin_get_tag, lin_unbox_ptr, TAG_ARRAY, TAG_UINT8, TAG_INT8};
    if lin_get_tag(item) != TAG_ARRAY {
        return;
    }
    let arr = lin_unbox_ptr(item) as *const crate::array::LinArray;
    let n = crate::array::lin_array_length(arr) as usize;
    let elem_tag = (*arr).elem_tag;
    if elem_tag == TAG_UINT8 || elem_tag == TAG_INT8 {
        let data = (*arr).data as *const u8;
        buf.extend_from_slice(std::slice::from_raw_parts(data, n));
    }
}

/// Box a byte slice as a `String` tagged value (lossy UTF-8). Lines/text are UTF-8 strings.
unsafe fn string_to_tagged(bytes: &[u8]) -> *mut u8 {
    use crate::tagged::{alloc_tagged, TAG_STR};
    let s = String::from_utf8_lossy(bytes);
    let lin = crate::fs::make_string(&s);
    alloc_tagged(TAG_STR, lin as u64)
}

// -------------------------------------------------------------------------
// Sink + terminal drivers (Stage 4).
// -------------------------------------------------------------------------

/// A push sink that writes every upstream item to a file. Built lazily by `writeStream`
/// (RAW: each item's bytes verbatim, concatenated, no separator) or `writeLines`
/// (`line_mode = true`: each item's bytes followed by `\n`). Nothing happens until a terminal
/// driver (`.drain()`) pulls it. The sink is itself a `StreamBox` whose `read_tagged` is never
/// meant to be pulled item-by-item by an adapter — instead `lin_stream_drain` recognises it and
/// runs the write loop. We still model it as a source so it composes uniformly and shares the
/// close/finalizer machinery.
struct WriteSink {
    up: Upstream,
    path: String,
    /// When true, append `\n` after every item (line-oriented `writeLines`). When false, write
    /// each item's bytes verbatim with no separator (raw `writeStream`) — required so binary
    /// output (e.g. `gzip(s).writeStream(...)`) is not corrupted by injected newlines.
    line_mode: bool,
}
impl StreamSource for WriteSink {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        // A sink yields nothing when pulled as a source; driving happens in `drive`.
        TaggedOutcome::Eof
    }
    unsafe fn drive(&mut self, _self_box: *const StreamBox) -> *mut u8 {
        drive_sink(self)
    }
    fn close(&mut self) {
        unsafe { self.up.close(); }
    }
}

/// Drive a `WriteSink` to completion on the CALLING thread (brief §5): pull every upstream item
/// and write its bytes to the file, until EOF (success → Null) or the first Err (→ Error object).
/// In raw mode the items' bytes are concatenated verbatim (no separator); in line mode each item
/// is followed by a `\n`. The file is opened once; an open/write failure short-circuits to Error.
unsafe fn drive_sink(sink: &mut WriteSink) -> *mut u8 {
    use std::io::Write;
    let mut file = match std::fs::File::create(&sink.path) {
        Ok(f) => f,
        Err(e) => return crate::fs::make_error_tagged(&e.to_string()),
    };
    let line_mode = sink.line_mode;
    loop {
        match pull_tagged(sink.up.boxptr) {
            TaggedOutcome::Eof => return std::ptr::null_mut(), // Null = success
            TaggedOutcome::Err(m) => return crate::fs::make_error_tagged(&m),
            TaggedOutcome::Item(item) => {
                let bytes = item_to_line_bytes(item);
                crate::tagged::lin_tagged_release(item);
                let write = if line_mode {
                    file.write_all(&bytes).and_then(|_| file.write_all(b"\n"))
                } else {
                    file.write_all(&bytes)
                };
                if let Err(e) = write {
                    return crate::fs::make_error_tagged(&e.to_string());
                }
            }
        }
    }
}

/// Render a tagged item as the bytes to write for one sink item: a String writes its UTF-8
/// bytes; a UInt8[] writes its raw bytes; anything else writes its `toString` rendering.
/// (The line-mode `\n` separator is appended by `drive_sink`, NOT here.)
unsafe fn item_to_line_bytes(item: *mut u8) -> Vec<u8> {
    use crate::tagged::{lin_get_tag, lin_unbox_ptr, TAG_STR, TAG_ARRAY, TAG_UINT8, TAG_INT8};
    match lin_get_tag(item) {
        TAG_STR => {
            let s = lin_unbox_ptr(item) as *const crate::string::LinString;
            let slice = std::slice::from_raw_parts((*s).data.as_ptr(), (*s).len as usize);
            slice.to_vec()
        }
        TAG_ARRAY => {
            let arr = lin_unbox_ptr(item) as *const crate::array::LinArray;
            let elem_tag = (*arr).elem_tag;
            if elem_tag == TAG_UINT8 || elem_tag == TAG_INT8 {
                let n = crate::array::lin_array_length(arr) as usize;
                let data = (*arr).data as *const u8;
                std::slice::from_raw_parts(data, n).to_vec()
            } else {
                render_to_string(item)
            }
        }
        _ => render_to_string(item),
    }
}

unsafe fn render_to_string(item: *mut u8) -> Vec<u8> {
    let s = crate::string::lin_tagged_to_string(item as *const crate::tagged::TaggedVal);
    let slice = std::slice::from_raw_parts((*s).data.as_ptr(), (*s).len as usize);
    let v = slice.to_vec();
    crate::string::lin_string_release(s);
    v
}

// -------------------------------------------------------------------------
// C-callable ABI (codegen dispatch + stdlib wrappers call these).
// -------------------------------------------------------------------------

/// `read(stream)` low-level: pull the next chunk, returning a tagged value:
///   * a flat UInt8[] tagged array for a chunk,
///   * the null value (null TaggedVal*) at EOF,
///   * a `{ type:"error", message }` tagged object on I/O error.
/// The stdlib `readStream` adapter layer interprets these three shapes (Stage 4).
#[no_mangle]
pub unsafe extern "C" fn lin_stream_read(s: *const u8) -> *mut u8 {
    read_outcome_to_tagged(stream_read_outcome(s))
}

/// Convert a `ReadOutcome` to its tagged-value representation.
unsafe fn read_outcome_to_tagged(outcome: ReadOutcome) -> *mut u8 {
    use crate::array::{lin_flat_array_alloc_u8, lin_flat_array_push_u8};
    use crate::tagged::{alloc_tagged, TAG_ARRAY};
    match outcome {
        ReadOutcome::Eof => std::ptr::null_mut(),
        ReadOutcome::Err(msg) => crate::fs::make_error_tagged(&msg),
        ReadOutcome::Chunk(bytes) => {
            let arr = lin_flat_array_alloc_u8(bytes.len().max(1) as u64);
            for byte in &bytes {
                lin_flat_array_push_u8(arr, *byte);
            }
            alloc_tagged(TAG_ARRAY, arr as u64)
        }
    }
}

/// `close(stream)` — close the underlying resource (idempotent). Returns the null value.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_close(s: *const u8) -> *mut u8 {
    stream_close(s);
    std::ptr::null_mut()
}

/// Take an OWNED reference to the upstream `StreamBox*` behind a boxed `Stream` value, for an
/// adapter to hold. Retains it (so the adapter's reference is independent of the caller's `val`
/// binding, which the owning model still releases at its scope exit; the affine check in Stage 6
/// makes the double-use a *compile-time* error, but the runtime RC stays balanced either way).
unsafe fn own_upstream(s: *const u8) -> Upstream {
    let b = unwrap_stream(s);
    // A non-stream Error argument (a failed source, e.g. `readStream` of a missing file) has a
    // null box; latch its message so the first pull surfaces it in-band rather than reading as an
    // empty stream. A real stream has `pending_err == None`.
    let pending_err = if b.is_null() { stream_arg_error(s) } else { None };
    lin_stream_retain_box(b as *const u8);
    Upstream { boxptr: b, pending_err }
}

/// Unbox a stream/closure ARGUMENT that may arrive boxed (TaggedVal*) or raw. For a Function the
/// codegen passes the raw closure ptr already; for safety, unwrap a TAG_FUNCTION box.
unsafe fn as_closure(f: *mut u8) -> *mut u8 {
    if f.is_null() {
        return f;
    }
    if crate::tagged::lin_get_tag(f) == crate::tagged::TAG_FUNCTION {
        crate::tagged::lin_unbox_ptr(f)
    } else {
        f
    }
}

/// `map(s, f)` → a new `Stream` whose items are `f(item)`. Takes an owned ref to `s` (upstream)
/// and a retained ref to `f`. The original `s` value is unchanged for RC purposes.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_map(s: *const u8, f: *mut u8) -> *mut u8 {
    let up = own_upstream(s);
    let closure = retain_closure(as_closure(f));
    StreamBox::new_boxed(Box::new(MapSource { up, f: LinFn::from_owned(closure), idx: 0 }))
}

/// `filter(s, p)` → a new `Stream` keeping items where `p(item)` is truthy.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_filter(s: *const u8, p: *mut u8) -> *mut u8 {
    let up = own_upstream(s);
    let closure = retain_closure(as_closure(p));
    StreamBox::new_boxed(Box::new(FilterSource { up, p: LinFn::from_owned(closure), idx: 0 }))
}

/// `take(s, n)` → a new `Stream` yielding at most `n` items.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_take(s: *const u8, n: i64) -> *mut u8 {
    let up = own_upstream(s);
    StreamBox::new_boxed(Box::new(TakeSource { up, remaining: n }))
}

/// `drop(s, n)` → a new `Stream` that skips the first `n` items then passes through.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_drop(s: *const u8, n: i64) -> *mut u8 {
    let up = own_upstream(s);
    StreamBox::new_boxed(Box::new(DropSource { up, remaining: n }))
}

/// `takeWhile(s, p)` → a new `Stream` yielding items while `p(item)` is truthy.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_take_while(s: *const u8, p: *mut u8) -> *mut u8 {
    let up = own_upstream(s);
    let closure = retain_closure(as_closure(p));
    StreamBox::new_boxed(Box::new(TakeWhileSource { up, p: LinFn::from_owned(closure), done: false, idx: 0 }))
}

/// `dropWhile(s, p)` → a new `Stream` skipping items while `p(item)` is truthy, then passing through.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_drop_while(s: *const u8, p: *mut u8) -> *mut u8 {
    let up = own_upstream(s);
    let closure = retain_closure(as_closure(p));
    StreamBox::new_boxed(Box::new(DropWhileSource { up, p: LinFn::from_owned(closure), dropping: true, idx: 0 }))
}

/// `flatMap(s, f)` → a new `Stream` flattening each `f(item)` collection lazily.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_flat_map(s: *const u8, f: *mut u8) -> *mut u8 {
    let up = own_upstream(s);
    let closure = retain_closure(as_closure(f));
    StreamBox::new_boxed(Box::new(FlatMapSource { up, f: LinFn::from_owned(closure), current: None, idx: 0 }))
}

/// `flatten(s)` → a new `Stream` flattening each item (a collection) lazily.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_flatten(s: *const u8) -> *mut u8 {
    let up = own_upstream(s);
    StreamBox::new_boxed(Box::new(FlattenSource { up, current: None }))
}

/// `concat(a, b)` → a new `Stream` yielding all of `a`, then all of `b`. BOTH are retained.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_concat(a: *const u8, b: *const u8) -> *mut u8 {
    let ua = own_upstream(a);
    let ub = own_upstream(b);
    StreamBox::new_boxed(Box::new(ConcatSource { a: ua, b: ub, on_b: false }))
}

/// `sliding(s, size)` → a new `Stream<T[]>` of overlapping width-`size` windows. `size < 1` → 1.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_sliding(s: *const u8, size: i64) -> *mut u8 {
    let up = own_upstream(s);
    let w = if size < 1 { 1usize } else { size as usize };
    StreamBox::new_boxed(Box::new(SlidingSource { up, size: w, buf: Vec::with_capacity(w) }))
}

/// `pairwise(s)` → a new `Stream<[T, T]>` of adjacent overlapping pairs.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_pairwise(s: *const u8) -> *mut u8 {
    let up = own_upstream(s);
    StreamBox::new_boxed(Box::new(PairwiseSource { up, prev: None }))
}

/// `intersperse(s, sep)` → a new `Stream<T>` inserting `sep` between adjacent items. `sep` arrives
/// boxed (TaggedVal*); we take one owned reference (cloned per emitted copy).
#[no_mangle]
pub unsafe extern "C" fn lin_stream_intersperse(s: *const u8, sep: *mut u8) -> *mut u8 {
    let up = own_upstream(s);
    let sep_owned = crate::tagged::lin_tagged_clone(sep as *const u8);
    StreamBox::new_boxed(Box::new(IntersperseSource { up, sep: sep_owned, emitted_first: false, pending: None }))
}

/// `dedup(s)` → a new `Stream<T>` collapsing consecutive equal items.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_dedup(s: *const u8) -> *mut u8 {
    let up = own_upstream(s);
    StreamBox::new_boxed(Box::new(DedupSource { up, last: None }))
}

/// `zipWith(a, b, f)` (stream arm) → a new `Stream<C>` of `f(a_item, b[i])`. `a` is the upstream
/// stream; `b` is a boxed in-memory `B[]` (one owned reference taken); `f` a retained closure.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_zip_with(s: *const u8, b: *mut u8, f: *mut u8) -> *mut u8 {
    let up = own_upstream(s);
    let b_owned = crate::tagged::lin_tagged_clone(b as *const u8);
    let b_len = if b_owned.is_null() {
        0
    } else {
        let barr = crate::tagged::lin_unbox_ptr(b_owned) as *const crate::array::LinArray;
        if barr.is_null() { 0 } else { crate::array::lin_array_length(barr) }
    };
    let closure = retain_closure(as_closure(f));
    StreamBox::new_boxed(Box::new(ZipWithSource { up, b: b_owned, b_len, f: LinFn::from_owned(closure), idx: 0 }))
}

/// `count(start, step)` → an INFINITE counting `Stream<Int32>`. Must be bounded downstream.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_count(start: i64, step: i64) -> *mut u8 {
    StreamBox::new_boxed(Box::new(CountSource { next: start, step }))
}

/// `repeat(value, n)` → a `Stream<T>` yielding `value` `n` times, or infinitely when `n < 0`.
/// `value` arrives boxed; one owned reference is taken (cloned per emitted copy).
#[no_mangle]
pub unsafe extern "C" fn lin_stream_repeat(value: *mut u8, n: i64) -> *mut u8 {
    let value_owned = crate::tagged::lin_tagged_clone(value as *const u8);
    let infinite = n < 0;
    StreamBox::new_boxed(Box::new(RepeatSource { value: value_owned, remaining: n, infinite }))
}

/// `cycle(arr)` → a `Stream<T>` repeating the elements of the finite boxed array `arr` endlessly.
/// An empty `arr` yields an empty stream. One owned reference to `arr` is taken.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_cycle(arr: *mut u8) -> *mut u8 {
    let arr_owned = crate::tagged::lin_tagged_clone(arr as *const u8);
    let len = if arr_owned.is_null() {
        0
    } else {
        let a = crate::tagged::lin_unbox_ptr(arr_owned) as *const crate::array::LinArray;
        if a.is_null() { 0 } else { crate::array::lin_array_length(a) }
    };
    StreamBox::new_boxed(Box::new(CycleSource { arr: arr_owned, len, idx: 0 }))
}

/// `gunzip(s)` → a `Stream<UInt8[]>` that decompresses a gzip-framed byte stream incrementally.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_gunzip(s: *const u8) -> *mut u8 {
    let up = own_upstream(s);
    StreamBox::new_boxed(Box::new(CodecSource {
        up,
        codec: Codec::GzDec(MultiGzDecoder::new(Vec::new())),
        upstream_done: false,
        finished: false,
    }))
}

/// `gzip(s)` → a `Stream<UInt8[]>` that compresses a byte stream into the gzip container format.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_gzip(s: *const u8) -> *mut u8 {
    let up = own_upstream(s);
    StreamBox::new_boxed(Box::new(CodecSource {
        up,
        codec: Codec::GzEnc(GzEncoder::new(Vec::new(), Compression::default())),
        upstream_done: false,
        finished: false,
    }))
}

/// `inflate(s)` → a `Stream<UInt8[]>` that decompresses a raw DEFLATE byte stream incrementally.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_inflate(s: *const u8) -> *mut u8 {
    let up = own_upstream(s);
    StreamBox::new_boxed(Box::new(CodecSource {
        up,
        codec: Codec::DfDec(DeflateDecoder::new(Vec::new())),
        upstream_done: false,
        finished: false,
    }))
}

/// `deflate(s)` → a `Stream<UInt8[]>` that compresses a byte stream as a raw DEFLATE bitstream.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_deflate(s: *const u8) -> *mut u8 {
    let up = own_upstream(s);
    StreamBox::new_boxed(Box::new(CodecSource {
        up,
        codec: Codec::DfEnc(DeflateEncoder::new(Vec::new(), Compression::default())),
        upstream_done: false,
        finished: false,
    }))
}

/// `lines(s)` → a `Stream<String>` of newline-delimited lines over the byte stream `s`.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_lines(s: *const u8, max_bytes: i64) -> *mut u8 {
    let up = own_upstream(s);
    // `max_bytes <= 0` selects the default cap; a positive value sets an explicit per-line bound.
    let max_line_bytes = if max_bytes > 0 { max_bytes as usize } else { MAX_LINE_BYTES };
    StreamBox::new_boxed(Box::new(LinesSource { up, buf: Vec::new(), upstream_done: false, max_line_bytes }))
}

/// `chunks(s, n)` → a `Stream<UInt8[]>` re-chunked to `n` bytes per item.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_chunks(s: *const u8, n: i64) -> *mut u8 {
    let up = own_upstream(s);
    let size = if n > 0 { n as usize } else { 1 };
    StreamBox::new_boxed(Box::new(ChunksSource { up, size, buf: Vec::new(), upstream_done: false }))
}

/// `writeStream(s, path)` → a RAW sink `Stream` that, when driven, writes each item of `s` to
/// `path` byte-for-byte, concatenated with NO separator (a String item writes its UTF-8 bytes, a
/// UInt8[] item its raw bytes, anything else its `toString`). Lazy: no file is opened until
/// `drain()`/`promise()` drives it. Use `writeLines` for newline-delimited output.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_write(s: *const u8, path: *const u8) -> *mut u8 {
    let path_str = crate::fs::resolve_lin_str(path).unwrap_or_default();
    let up = own_upstream(s);
    StreamBox::new_boxed(Box::new(WriteSink { up, path: path_str, line_mode: false }))
}

/// `writeLines(s, path)` → a LINE-oriented sink `Stream` that, when driven, writes each item of
/// `s` to `path` followed by a `\n` (one item per line). Lazy: no file is opened until
/// `drain()`/`promise()` drives it. Use `writeStream` for raw, verbatim byte output.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_write_lines(s: *const u8, path: *const u8) -> *mut u8 {
    let path_str = crate::fs::resolve_lin_str(path).unwrap_or_default();
    let up = own_upstream(s);
    StreamBox::new_boxed(Box::new(WriteSink { up, path: path_str, line_mode: true }))
}

/// `.drain()` → drive the pipeline to completion on the CALLING thread → `Null | Error`
/// (brief §5). If `s` is a `WriteSink`, run its write loop; otherwise pull-and-discard every item
/// (so a non-sink pipeline can still be forced for effects), surfacing the first Err. Always
/// closes the stream afterwards (deterministic resource release).
#[no_mangle]
pub unsafe extern "C" fn lin_stream_drain(s: *const u8) -> *mut u8 {
    let b = unwrap_stream(s);
    if b.is_null() {
        // A failed source (non-stream Error arg) drained directly: surface the fault in-band
        // instead of silently returning Null (as if an empty stream had drained successfully).
        if let Some(m) = stream_arg_error(s) {
            return crate::fs::make_error_tagged(&m);
        }
        return std::ptr::null_mut();
    }
    let result = {
        let mut guard = (*b).state.lock().unwrap();
        if guard.closed {
            std::ptr::null_mut()
        } else if let Some(src) = guard.source.as_mut() {
            // Downcast-free dispatch: a WriteSink drives its write loop; any other source is
            // drained by pull-and-discard. We detect a sink by attempting the write drive via a
            // trait method would need dyn-downcast; instead we expose `drive` on the source.
            src.drive(b)
        } else {
            std::ptr::null_mut()
        }
    };
    close_box(b);
    result
}

/// `.for(s, body)` over a Stream (Stage 5): pull every item and call `body(item)` on the calling
/// thread, until EOF (→ Null) or the first read Error (→ the Error object becomes the for-expr's
/// value, brief §3). `body` is a 1-arg boxed-ABI closure; its return is discarded. Closes the
/// stream when the loop ends (deterministic release; the RC finalizer would close it anyway).
#[no_mangle]
pub unsafe extern "C" fn lin_stream_for(s: *const u8, body: *mut u8) -> *mut u8 {
    let b = unwrap_stream(s);
    if b.is_null() {
        if let Some(m) = stream_arg_error(s) {
            return crate::fs::make_error_tagged(&m);
        }
    }
    let f = LinFn::from_owned(retain_closure(as_closure(body)));
    let mut idx: i64 = 0;
    let result = loop {
        match pull_tagged(b) {
            TaggedOutcome::Eof => break std::ptr::null_mut(), // normal end → Null
            TaggedOutcome::Err(m) => break crate::fs::make_error_tagged(&m),
            TaggedOutcome::Item(item) => {
                let i = idx;
                idx += 1;
                let ret = f.call_caught(item, i);
                crate::tagged::lin_tagged_release(item);
                match ret {
                    Ok(r) => crate::tagged::lin_tagged_release(r), // body's return is discarded
                    // A body fault ends the for-expr with an Error (consistent with a read Error).
                    Err(m) => break crate::fs::make_error_tagged(&m),
                }
            }
        }
    };
    close_box(b);
    result
}

/// `reduce(s, init, f)` → fold every item into an accumulator via `f(acc, item)` → `U | Error`.
/// Drives on the calling thread; a read or transform fault short-circuits to an Error. The
/// accumulator starts at `init` (the caller hands us an owned +1 ref) and is replaced each step
/// (the old acc is released, the new one — the closure's result — is owned). Closes the stream.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_reduce(s: *const u8, init: *mut u8, f: *mut u8) -> *mut u8 {
    let b = unwrap_stream(s);
    if b.is_null() {
        if let Some(m) = stream_arg_error(s) {
            crate::tagged::lin_tagged_release(init); // we own the +1 init; drop it before the Error
            return crate::fs::make_error_tagged(&m);
        }
    }
    let func = LinFn::from_owned(retain_closure(as_closure(f)));
    // `init` arrives as an owned +1 reference (the caller suppressed its own release / it is a
    // fresh box); we own the running accumulator and release the previous on each replacement.
    let mut acc = init;
    let mut idx: i64 = 0;
    let result = loop {
        match pull_tagged(b) {
            TaggedOutcome::Eof => break acc, // success → the final accumulator
            TaggedOutcome::Err(m) => {
                crate::tagged::lin_tagged_release(acc);
                break crate::fs::make_error_tagged(&m);
            }
            TaggedOutcome::Item(item) => {
                let i = idx;
                idx += 1;
                let next = func.call2_caught(acc, item, i);
                crate::tagged::lin_tagged_release(item);
                match next {
                    Ok(v) => {
                        // Release the previous accumulator; adopt the closure's result.
                        crate::tagged::lin_tagged_release(acc);
                        acc = v;
                    }
                    Err(m) => {
                        crate::tagged::lin_tagged_release(acc);
                        break crate::fs::make_error_tagged(&m);
                    }
                }
            }
        }
    };
    close_box(b);
    result
}

/// `find(s, p)` → the first item where `p(item)` is truthy → `T | Null | Error` (Null if none).
/// Closes the stream when it ends (match, EOF, or error).
#[no_mangle]
pub unsafe extern "C" fn lin_stream_find(s: *const u8, p: *mut u8) -> *mut u8 {
    let b = unwrap_stream(s);
    if b.is_null() {
        if let Some(m) = stream_arg_error(s) {
            return crate::fs::make_error_tagged(&m);
        }
    }
    let pred = LinFn::from_owned(retain_closure(as_closure(p)));
    let mut idx: i64 = 0;
    let result = loop {
        match pull_tagged(b) {
            TaggedOutcome::Eof => break std::ptr::null_mut(), // none found → Null
            TaggedOutcome::Err(m) => break crate::fs::make_error_tagged(&m),
            TaggedOutcome::Item(item) => {
                let i = idx;
                idx += 1;
                let verdict = match pred.call_caught(item, i) {
                    Ok(v) => v,
                    Err(m) => {
                        crate::tagged::lin_tagged_release(item);
                        break crate::fs::make_error_tagged(&m);
                    }
                };
                let hit = is_truthy(verdict);
                crate::tagged::lin_tagged_release(verdict);
                if hit {
                    break item; // the found item is returned (owned)
                }
                crate::tagged::lin_tagged_release(item);
            }
        }
    };
    close_box(b);
    result
}

/// `some(s, p)` → `Boolean | Error`: true on the first truthy `p(item)` (short-circuit), false if
/// none. A read/transform fault → Error. Closes the stream.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_some(s: *const u8, p: *mut u8) -> *mut u8 {
    let b = unwrap_stream(s);
    if b.is_null() {
        if let Some(m) = stream_arg_error(s) {
            return crate::fs::make_error_tagged(&m);
        }
    }
    let pred = LinFn::from_owned(retain_closure(as_closure(p)));
    let mut idx: i64 = 0;
    let result = loop {
        match pull_tagged(b) {
            TaggedOutcome::Eof => break crate::tagged::lin_box_bool(0),
            TaggedOutcome::Err(m) => break crate::fs::make_error_tagged(&m),
            TaggedOutcome::Item(item) => {
                let i = idx;
                idx += 1;
                let verdict = match pred.call_caught(item, i) {
                    Ok(v) => v,
                    Err(m) => {
                        crate::tagged::lin_tagged_release(item);
                        break crate::fs::make_error_tagged(&m);
                    }
                };
                let hit = is_truthy(verdict);
                crate::tagged::lin_tagged_release(verdict);
                crate::tagged::lin_tagged_release(item);
                if hit {
                    break crate::tagged::lin_box_bool(1);
                }
            }
        }
    };
    close_box(b);
    result
}

/// `every(s, p)` → `Boolean | Error`: false on the first falsy `p(item)` (short-circuit), true if
/// all pass (or empty). A read/transform fault → Error. Closes the stream.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_every(s: *const u8, p: *mut u8) -> *mut u8 {
    let b = unwrap_stream(s);
    if b.is_null() {
        if let Some(m) = stream_arg_error(s) {
            return crate::fs::make_error_tagged(&m);
        }
    }
    let pred = LinFn::from_owned(retain_closure(as_closure(p)));
    let mut idx: i64 = 0;
    let result = loop {
        match pull_tagged(b) {
            TaggedOutcome::Eof => break crate::tagged::lin_box_bool(1),
            TaggedOutcome::Err(m) => break crate::fs::make_error_tagged(&m),
            TaggedOutcome::Item(item) => {
                let i = idx;
                idx += 1;
                let verdict = match pred.call_caught(item, i) {
                    Ok(v) => v,
                    Err(m) => {
                        crate::tagged::lin_tagged_release(item);
                        break crate::fs::make_error_tagged(&m);
                    }
                };
                let pass = is_truthy(verdict);
                crate::tagged::lin_tagged_release(verdict);
                crate::tagged::lin_tagged_release(item);
                if !pass {
                    break crate::tagged::lin_box_bool(0);
                }
            }
        }
    };
    close_box(b);
    result
}

/// `while(s, f)` → drive each item through `f(item)` until it returns a falsy value or EOF →
/// `Null | Error`. Like `for` but with early stop on a falsy predicate result. A read/transform
/// fault → Error. Closes the stream.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_while(s: *const u8, f: *mut u8) -> *mut u8 {
    let b = unwrap_stream(s);
    if b.is_null() {
        if let Some(m) = stream_arg_error(s) {
            return crate::fs::make_error_tagged(&m);
        }
    }
    let func = LinFn::from_owned(retain_closure(as_closure(f)));
    let mut idx: i64 = 0;
    let result = loop {
        match pull_tagged(b) {
            TaggedOutcome::Eof => break std::ptr::null_mut(), // normal end → Null
            TaggedOutcome::Err(m) => break crate::fs::make_error_tagged(&m),
            TaggedOutcome::Item(item) => {
                let i = idx;
                idx += 1;
                let verdict = match func.call_caught(item, i) {
                    Ok(v) => v,
                    Err(m) => {
                        crate::tagged::lin_tagged_release(item);
                        break crate::fs::make_error_tagged(&m);
                    }
                };
                let keep = is_truthy(verdict);
                crate::tagged::lin_tagged_release(verdict);
                crate::tagged::lin_tagged_release(item);
                if !keep {
                    break std::ptr::null_mut(); // predicate false → stop, Null
                }
            }
        }
    };
    close_box(b);
    result
}

/// Drive a stream to completion and CONSUME it (close + release the caller's owned reference),
/// returning `Null | Error`. This is the worker-thread body behind `.promise()` (Stage 8): it
/// takes ownership of `s` (a boxed `TaggedVal*(TAG_STREAM)`, moved onto the worker), drives the
/// sink (or pulls-and-discards a non-sink), then releases the stream so the fd closes exactly
/// once on the worker. A read/transform fault surfaces as the returned Error object (in-band).
#[no_mangle]
pub unsafe extern "C" fn lin_stream_drive_owned(s: *mut u8) -> *mut u8 {
    let result = lin_stream_drain(s);
    // `drain` closed the backend; release the worker's owned reference (frees the box).
    crate::tagged::lin_tagged_release(s);
    result
}

/// `collect(s)` → pull all items, concatenating their bytes into a single `UInt8[]` → `UInt8[] |
/// Error`. Closes the stream afterwards.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_collect(s: *const u8) -> *mut u8 {
    let b = unwrap_stream(s);
    if b.is_null() {
        if let Some(m) = stream_arg_error(s) {
            return crate::fs::make_error_tagged(&m);
        }
    }
    let mut bytes: Vec<u8> = Vec::new();
    let result = loop {
        match pull_tagged(b) {
            TaggedOutcome::Eof => break bytes_to_u8_array(&bytes),
            TaggedOutcome::Err(m) => break crate::fs::make_error_tagged(&m),
            TaggedOutcome::Item(item) => {
                let line = item_to_line_bytes(item);
                crate::tagged::lin_tagged_release(item);
                bytes.extend_from_slice(&line);
            }
        }
    };
    close_box(b);
    result
}

/// `readText(s)` → pull all items, concatenating into a single UTF-8 `String` → `String | Error`.
/// Closes the stream afterwards.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_read_text(s: *const u8) -> *mut u8 {
    let b = unwrap_stream(s);
    if b.is_null() {
        if let Some(m) = stream_arg_error(s) {
            return crate::fs::make_error_tagged(&m);
        }
    }
    let mut bytes: Vec<u8> = Vec::new();
    let result = loop {
        match pull_tagged(b) {
            TaggedOutcome::Eof => break string_to_tagged(&bytes),
            TaggedOutcome::Err(m) => break crate::fs::make_error_tagged(&m),
            TaggedOutcome::Item(item) => {
                let line = item_to_line_bytes(item);
                crate::tagged::lin_tagged_release(item);
                bytes.extend_from_slice(&line);
            }
        }
    };
    close_box(b);
    result
}

/// Atomic retain given the RAW `*const StreamBox` payload (not a boxed TaggedVal*). Called from
/// the tag-aware retain path (`retain_tagged_payload`'s TAG_STREAM arm). Null-safe.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_retain_box(b: *const u8) {
    let b = b as *const StreamBox;
    if !b.is_null() {
        (*b).rc.fetch_add(1, Ordering::Relaxed);
    }
}

/// Atomic release given the RAW `*const StreamBox` payload. When the last reference drops, the
/// FINALIZER runs: it closes the backend IF not already closed (auto-close, brief §2), then frees
/// the box. Acquire/Release fences make the final drop see all prior writes. Null-safe.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_release_box(b: *const u8) {
    let b = b as *const StreamBox;
    if b.is_null() {
        return;
    }
    if (*b).rc.fetch_sub(1, Ordering::Release) == 1 {
        std::sync::atomic::fence(Ordering::Acquire);
        // Last reference: auto-close the backend (no-op if already explicitly closed), then
        // reclaim the box. close_box's `closed` flag guarantees the fd closes EXACTLY ONCE.
        close_box(b);
        drop(Box::from_raw(b as *mut StreamBox));
    }
}

// =====================================================================================
// `std/archive` — tar splitting over a `Stream<UInt8[]>` (ADR-047 streams; std/archive surface).
//
// A tar archive is a flat concatenation of 512-byte-aligned (header + body) records. This module
// turns a byte stream into archive entries WITHOUT buffering the whole archive — the parent stream
// is pulled one chunk at a time and the active entry's body is yielded as a bounded SUB-STREAM.
//
// Three surfaces (all CONSUME the parent stream by move):
//   * `lin_stream_untar(s, body)` — TERMINAL. Drives the whole archive on the calling thread,
//     calling `body(meta, data)` per entry where `data` is a `Stream<UInt8[]>` sub-stream over that
//     entry's body. Returns `Null | Error`. The constant-memory primitive.
//   * `lin_stream_manifest(s)` — ADAPTER → `Stream<Object>`: yields each entry's meta object and
//     SKIPS its body (meta-only listing).
//   * `lin_stream_files(s)` — ADAPTER → `Stream<Object>`: yields `{name, data, size, …}` where
//     `data` is the entry's full body buffered into a `UInt8[]`.
//
// SUB-STREAM RC CONTRACT (untar): `untar` owns the parent via an `Arc<Mutex<TarReaderState>>`. Per
// entry it MINTS a `BoundedSource` boxed as TAG_STREAM (rc=1), passes it BORROWED to `body` (the
// 2-arg `call2_caught` ABI; the caller owns it), then releases the body's return value, reads back
// `current_entry_remaining` from the shared state, skips that many bytes + tar padding, and finally
// releases its OWN ref to the sub-stream box (rc→0; the no-op `close` leaves the parent untouched).
// SYNC-ONLY: the sub-stream is valid only DURING the body's synchronous execution (the driver is
// paused). Handing `data` to a worker (`.promise()`) would race the shared cursor and is UNSUPPORTED
// (bounded by the ADR-049 placement restriction — `data` cannot be stored in a field/var/array).
// =====================================================================================

/// Shared cursor over the parent byte stream + the active entry's remaining body length. `untar`
/// owns the parent (one ref); a per-entry `BoundedSource` holds a clone of the `Arc`.
struct TarReaderState {
    /// The parent upstream (one owned ref). Closed exactly once when the reader finishes.
    up: Upstream,
    /// Bytes pulled from upstream but not yet consumed (header bytes, body bytes, padding).
    buf: Vec<u8>,
    /// True once the parent has signalled EOF (or errored).
    upstream_done: bool,
    /// True once the parent has produced an in-band read Error (short-circuits the driver).
    upstream_err: Option<String>,
    /// Bytes of the CURRENT entry's body not yet handed out by the BoundedSource.
    current_entry_remaining: usize,
}
unsafe impl Send for TarReaderState {}

impl TarReaderState {
    /// Pull from the parent until `buf` holds at least `n` bytes or the parent is exhausted.
    /// An in-band read Error sets `upstream_err` and stops. Returns true if `buf.len() >= n`.
    unsafe fn fill_at_least(&mut self, n: usize) -> bool {
        while self.buf.len() < n && !self.upstream_done {
            match self.up.pull() {
                TaggedOutcome::Item(item) => {
                    append_u8_array_to(&mut self.buf, item);
                    crate::tagged::lin_tagged_release(item);
                }
                TaggedOutcome::Eof => self.upstream_done = true,
                TaggedOutcome::Err(m) => {
                    self.upstream_done = true;
                    if self.upstream_err.is_none() {
                        self.upstream_err = Some(m);
                    }
                }
            }
        }
        self.buf.len() >= n
    }

    /// Take up to `n` bytes off the front of `buf` (filling from upstream as needed).
    unsafe fn take_front(&mut self, n: usize) -> Vec<u8> {
        self.fill_at_least(n);
        let take = n.min(self.buf.len());
        self.buf.drain(..take).collect()
    }
}

/// Parsed tar header fields we care about.
struct TarEntryMeta {
    name: String,
    size: usize,
    typeflag: u8,
}

/// Parse a 512-byte tar (ustar) header. An all-zero block (end-of-archive) returns None. Tolerant:
/// a truncated/short block returns None; an empty ustar `prefix` field degrades to the bare name
/// (no full GNU/pax long-name support in v1, but never crashes on it).
fn parse_tar_header(block: &[u8]) -> Option<TarEntryMeta> {
    if block.len() < 512 || block[..512].iter().all(|&b| b == 0) {
        return None;
    }
    // name: 100 bytes at offset 0, NUL-trimmed.
    let name_end = block[0..100].iter().position(|&b| b == 0).unwrap_or(100);
    let mut name = String::from_utf8_lossy(&block[0..name_end]).into_owned();
    // ustar `prefix`: 155 bytes at offset 345 (a path prefix joined with '/'). Tolerate an empty
    // prefix (the common case) — degrade gracefully if present but malformed.
    if block.len() >= 500 {
        let prefix_end = block[345..345 + 155].iter().position(|&b| b == 0).unwrap_or(155);
        if prefix_end > 0 {
            let prefix = String::from_utf8_lossy(&block[345..345 + prefix_end]);
            name = format!("{}/{}", prefix, name);
        }
    }
    // size: 12 bytes at offset 124, octal ASCII (NUL/space-trimmed).
    let size_field = &block[124..136];
    let size_str: String = size_field
        .iter()
        .take_while(|&&b| b != 0 && b != b' ')
        .map(|&b| b as char)
        .collect();
    let size = usize::from_str_radix(size_str.trim(), 8).unwrap_or(0);
    // typeflag: 1 byte at offset 156.
    let typeflag = block[156];
    Some(TarEntryMeta { name, size, typeflag })
}

/// Round a body size up to the next 512-byte boundary (tar pads each body with NULs).
fn padded_len(size: usize) -> usize {
    size.div_ceil(512) * 512
}

/// Build the pure-JSON `meta` object for a tar entry:
///   `{ name: String, size: Int64, typeflag: String, isDir: Boolean }`.
/// Returned value is independently owned (release with `lin_tagged_release`). Leak-clean: each
/// `lin_object_set` retains its own key/value, so the local +1 from each `make_string` is released.
/// Build a `TarHeader` map and return the raw `LinMap*` (not wrapped in `alloc_tagged`).
/// Used by `lin_tar_header` — the stdlib wrapper's codegen calls `lin_map_get` directly on
/// the return value (Phase 2: non-sealed objects are now backed by LinMap).
unsafe fn make_meta_object_unboxed(meta: &TarEntryMeta) -> *mut u8 {
    use crate::map::{lin_map_alloc, lin_map_set};
    use crate::string::lin_string_release;
    use crate::tagged::{TAG_STR, TAG_INT64, TAG_BOOL, TaggedVal};

    let map = lin_map_alloc(4, 0);

    // name
    let k = crate::fs::make_string("name");
    let v = crate::fs::make_string(&meta.name);
    let mut tv: TaggedVal = std::mem::zeroed();
    tv.tag = TAG_STR;
    tv.payload = v as u64;
    lin_map_set(map, k, &tv);
    lin_string_release(k);
    lin_string_release(v);

    // size (Int64)
    let k = crate::fs::make_string("size");
    let mut tv: TaggedVal = std::mem::zeroed();
    tv.tag = TAG_INT64;
    tv.payload = meta.size as u64;
    lin_map_set(map, k, &tv);
    lin_string_release(k);

    // typeflag
    let tf = if meta.typeflag == 0 { '0' } else { meta.typeflag as char };
    let tf_str = tf.to_string();
    let k = crate::fs::make_string("typeflag");
    let v = crate::fs::make_string(&tf_str);
    let mut tv: TaggedVal = std::mem::zeroed();
    tv.tag = TAG_STR;
    tv.payload = v as u64;
    lin_map_set(map, k, &tv);
    lin_string_release(k);
    lin_string_release(v);

    // isDir
    let is_dir = meta.typeflag == b'5';
    let k = crate::fs::make_string("isDir");
    let mut tv: TaggedVal = std::mem::zeroed();
    tv.tag = TAG_BOOL;
    tv.payload = is_dir as u64;
    lin_map_set(map, k, &tv);
    lin_string_release(k);

    map as *mut u8
}

unsafe fn make_meta_object(meta: &TarEntryMeta) -> *mut u8 {
    use crate::map::{lin_map_alloc, lin_map_set};
    use crate::string::lin_string_release;
    use crate::tagged::{alloc_tagged, TAG_MAP, TAG_STR, TAG_INT64, TAG_BOOL, TaggedVal};

    let map = lin_map_alloc(4, 0);

    // name
    let k = crate::fs::make_string("name");
    let v = crate::fs::make_string(&meta.name);
    let mut tv: TaggedVal = std::mem::zeroed();
    tv.tag = TAG_STR;
    tv.payload = v as u64;
    lin_map_set(map, k, &tv);
    lin_string_release(k);
    lin_string_release(v);

    // size (Int64)
    let k = crate::fs::make_string("size");
    let mut tv: TaggedVal = std::mem::zeroed();
    tv.tag = TAG_INT64;
    tv.payload = meta.size as u64;
    lin_map_set(map, k, &tv);
    lin_string_release(k);

    // typeflag (the raw single byte as a 1-char String; '0'/'\0' = file, '5' = directory)
    let tf = if meta.typeflag == 0 { '0' } else { meta.typeflag as char };
    let tf_str = tf.to_string();
    let k = crate::fs::make_string("typeflag");
    let v = crate::fs::make_string(&tf_str);
    let mut tv: TaggedVal = std::mem::zeroed();
    tv.tag = TAG_STR;
    tv.payload = v as u64;
    lin_map_set(map, k, &tv);
    lin_string_release(k);
    lin_string_release(v);

    // isDir (Boolean)
    let is_dir = meta.typeflag == b'5';
    let k = crate::fs::make_string("isDir");
    let mut tv: TaggedVal = std::mem::zeroed();
    tv.tag = TAG_BOOL;
    tv.payload = is_dir as u64;
    lin_map_set(map, k, &tv);
    lin_string_release(k);

    alloc_tagged(TAG_MAP, map as u64)
}

/// The per-entry `data` sub-stream (untar): yields at most `current_entry_remaining` bytes from the
/// shared buffer, then EOF for that entry. Holds a CLONE of the shared `Arc` — the shared cursor.
struct BoundedSource {
    state: std::sync::Arc<std::sync::Mutex<TarReaderState>>,
}
unsafe impl Send for BoundedSource {}
impl StreamSource for BoundedSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        let mut st = self.state.lock().unwrap();
        if st.current_entry_remaining == 0 {
            return TaggedOutcome::Eof;
        }
        // Yield up to 64 KiB of the entry's remaining body per read.
        let want = st.current_entry_remaining.min(64 * 1024);
        let bytes = st.take_front(want);
        if bytes.is_empty() {
            // Parent ended mid-body — present as EOF for this entry.
            st.current_entry_remaining = 0;
            return TaggedOutcome::Eof;
        }
        st.current_entry_remaining -= bytes.len();
        TaggedOutcome::Item(bytes_to_u8_array(&bytes))
    }
    // The sub-stream does NOT own the parent — closing it must NOT close the shared upstream (the
    // driver still needs it for the next entry). No-op close (sub-stream RC contract).
    fn close(&mut self) {}
}

/// `untar(s, body)` — drive the whole archive on the calling thread, calling `body(meta, data)` per
/// entry. Returns `Null | Error`. Constant-memory: the parent is pulled one chunk at a time and the
/// active entry's body is exposed as a bounded sub-stream. A body fault → in-band Error (like
/// `lin_stream_for`); a parent read Error short-circuits to that Error. Closes the parent at the end.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_untar(s: *const u8, body: *mut u8) -> *mut u8 {
    // Own the parent (one ref) inside the shared state; release the caller-suppressed boxed value.
    let state = std::sync::Arc::new(std::sync::Mutex::new(TarReaderState {
        up: own_upstream(s),
        buf: Vec::new(),
        upstream_done: false,
        upstream_err: None,
        current_entry_remaining: 0,
    }));
    let f = LinFn::from_owned(retain_closure(as_closure(body)));
    // Entry ordinal, passed as call2's trailing index arg. The untar body is `(meta, data)` (2
    // params, no index), so its wrapper ignores this trailing arg — but call2 always supplies one,
    // keeping the ABI uniform across all stream callbacks.
    let mut idx: i64 = 0;

    let result: *mut u8 = loop {
        // Parse the next 512-byte header.
        let header = { state.lock().unwrap().take_front(512) };
        // A parent read Error surfaces as the driver's Error.
        if let Some(m) = { state.lock().unwrap().upstream_err.take() } {
            break crate::fs::make_error_tagged(&m);
        }
        let meta = match parse_tar_header(&header) {
            Some(m) => m,
            None => break std::ptr::null_mut(), // end-of-archive (or no more data) → Null
        };
        { state.lock().unwrap().current_entry_remaining = meta.size; }

        // Build the per-entry meta object + the bounded `data` sub-stream box (rc=1).
        let meta_obj = make_meta_object(&meta);
        let data_box = StreamBox::new_boxed(Box::new(BoundedSource { state: state.clone() }));

        // Call body(meta, data) — args are BORROWED (caller owns them, per the call2 ABI).
        let i = idx;
        idx += 1;
        let ret = f.call2_caught(meta_obj, data_box, i);

        // Release our refs to the args we minted (the meta object + the sub-stream box). For the
        // sub-stream this drops untar's OWN ref → rc 0 → frees the box; the no-op close means the
        // shared parent is untouched.
        crate::tagged::lin_tagged_release(meta_obj);
        crate::tagged::lin_tagged_release(data_box);

        match ret {
            Ok(r) => crate::tagged::lin_tagged_release(r), // the body's return is discarded
            Err(m) => break crate::fs::make_error_tagged(&m),
        }

        // Read back how many body bytes remain undrained; skip them + the body padding to land on
        // the next header. Whether the body drained the sub-stream or ignored it, this is correct.
        {
            let mut st = state.lock().unwrap();
            let undrained = st.current_entry_remaining;
            let body_padding = padded_len(meta.size) - meta.size;
            let skip = undrained + body_padding;
            let _ = st.take_front(skip);
            st.current_entry_remaining = 0;
        }
        // A parent read Error during the skip also surfaces.
        if let Some(m) = { state.lock().unwrap().upstream_err.take() } {
            break crate::fs::make_error_tagged(&m);
        }
    };

    // Close the parent exactly once. The `Arc` may still be alive in a leaked sub-stream box only if
    // the body stored it (forbidden by ADR-049) — under the supported contract this is the last ref.
    {
        let mut st = state.lock().unwrap();
        st.up.close();
    }
    result
}

/// `manifest(s)` adapter source: each `read_tagged` parses the next header, builds the meta object,
/// SKIPS the entry's body + padding, and returns the meta Item. Owns + closes the parent like other
/// adapters. Zero-block (end-of-archive) → Eof.
struct ManifestSource {
    up: Upstream,
    buf: Vec<u8>,
    upstream_done: bool,
}
unsafe impl Send for ManifestSource {}
impl ManifestSource {
    unsafe fn fill_at_least(&mut self, n: usize) -> Result<bool, String> {
        while self.buf.len() < n && !self.upstream_done {
            match self.up.pull() {
                TaggedOutcome::Item(item) => {
                    append_u8_array_to(&mut self.buf, item);
                    crate::tagged::lin_tagged_release(item);
                }
                TaggedOutcome::Eof => self.upstream_done = true,
                TaggedOutcome::Err(m) => return Err(m),
            }
        }
        Ok(self.buf.len() >= n)
    }
    unsafe fn take_front(&mut self, n: usize) -> Result<Vec<u8>, String> {
        self.fill_at_least(n)?;
        let take = n.min(self.buf.len());
        Ok(self.buf.drain(..take).collect())
    }
}
impl StreamSource for ManifestSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        let header = match self.take_front(512) {
            Ok(h) => h,
            Err(m) => return TaggedOutcome::Err(m),
        };
        let meta = match parse_tar_header(&header) {
            Some(m) => m,
            None => return TaggedOutcome::Eof, // end-of-archive
        };
        let obj = make_meta_object(&meta);
        // Skip the body + padding so the next read lands on the next header.
        if let Err(m) = self.take_front(padded_len(meta.size)) {
            crate::tagged::lin_tagged_release(obj);
            return TaggedOutcome::Err(m);
        }
        TaggedOutcome::Item(obj)
    }
    fn close(&mut self) {
        unsafe { self.up.close(); }
    }
}

/// `manifest(s)` → a `Stream<Object>` of entry meta objects (body skipped).
#[no_mangle]
pub unsafe extern "C" fn lin_stream_manifest(s: *const u8) -> *mut u8 {
    StreamBox::new_boxed(Box::new(ManifestSource {
        up: own_upstream(s),
        buf: Vec::new(),
        upstream_done: false,
    }))
}

/// `files(s)` adapter source: each `read_tagged` parses the header, reads the FULL body into a
/// `UInt8[]`, builds `{name, data, size, typeflag, isDir}`, and returns it. Owns + closes the
/// parent. Zero-block → Eof.
struct FilesSource {
    up: Upstream,
    buf: Vec<u8>,
    upstream_done: bool,
}
unsafe impl Send for FilesSource {}
impl FilesSource {
    unsafe fn fill_at_least(&mut self, n: usize) -> Result<bool, String> {
        while self.buf.len() < n && !self.upstream_done {
            match self.up.pull() {
                TaggedOutcome::Item(item) => {
                    append_u8_array_to(&mut self.buf, item);
                    crate::tagged::lin_tagged_release(item);
                }
                TaggedOutcome::Eof => self.upstream_done = true,
                TaggedOutcome::Err(m) => return Err(m),
            }
        }
        Ok(self.buf.len() >= n)
    }
    unsafe fn take_front(&mut self, n: usize) -> Result<Vec<u8>, String> {
        self.fill_at_least(n)?;
        let take = n.min(self.buf.len());
        Ok(self.buf.drain(..take).collect())
    }
}
impl StreamSource for FilesSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        use crate::map::{lin_map_alloc, lin_map_set};
        use crate::string::lin_string_release;
        use crate::tagged::{alloc_tagged, TAG_MAP, TAG_STR, TAG_INT64, TAG_BOOL, TAG_ARRAY, TaggedVal};

        let header = match self.take_front(512) {
            Ok(h) => h,
            Err(m) => return TaggedOutcome::Err(m),
        };
        let meta = match parse_tar_header(&header) {
            Some(m) => m,
            None => return TaggedOutcome::Eof,
        };
        // Read the FULL body, then skip the padding to the next header.
        let body = match self.take_front(meta.size) {
            Ok(b) => b,
            Err(m) => return TaggedOutcome::Err(m),
        };
        let pad = padded_len(meta.size) - meta.size;
        if let Err(m) = self.take_front(pad) {
            return TaggedOutcome::Err(m);
        }

        // Build { name, data, size, typeflag, isDir } as LinMap.
        let map = lin_map_alloc(8, 0);

        let k = crate::fs::make_string("name");
        let v = crate::fs::make_string(&meta.name);
        let mut tv: TaggedVal = std::mem::zeroed();
        tv.tag = TAG_STR;
        tv.payload = v as u64;
        lin_map_set(map, k, &tv);
        lin_string_release(k);
        lin_string_release(v);

        // data: the entry's body as a fresh UInt8[] box.
        let data_box = bytes_to_u8_array(&body);
        let k = crate::fs::make_string("data");
        let mut tv: TaggedVal = std::mem::zeroed();
        tv.tag = TAG_ARRAY;
        tv.payload = crate::tagged::lin_unbox_ptr(data_box) as u64;
        lin_map_set(map, k, &tv);
        lin_string_release(k);
        crate::tagged::lin_tagged_release(data_box); // map now owns its own ref to the array

        let k = crate::fs::make_string("size");
        let mut tv: TaggedVal = std::mem::zeroed();
        tv.tag = TAG_INT64;
        tv.payload = meta.size as u64;
        lin_map_set(map, k, &tv);
        lin_string_release(k);

        let tf = if meta.typeflag == 0 { '0' } else { meta.typeflag as char };
        let tf_str = tf.to_string();
        let k = crate::fs::make_string("typeflag");
        let v = crate::fs::make_string(&tf_str);
        let mut tv: TaggedVal = std::mem::zeroed();
        tv.tag = TAG_STR;
        tv.payload = v as u64;
        lin_map_set(map, k, &tv);
        lin_string_release(k);
        lin_string_release(v);

        let is_dir = meta.typeflag == b'5';
        let k = crate::fs::make_string("isDir");
        let mut tv: TaggedVal = std::mem::zeroed();
        tv.tag = TAG_BOOL;
        tv.payload = is_dir as u64;
        lin_map_set(map, k, &tv);
        lin_string_release(k);

        TaggedOutcome::Item(alloc_tagged(TAG_MAP, map as u64))
    }
    fn close(&mut self) {
        unsafe { self.up.close(); }
    }
}

/// `files(s)` → a `Stream<Object>` of `{name, data, size, typeflag, isDir}` (each body buffered).
#[no_mangle]
pub unsafe extern "C" fn lin_stream_files(s: *const u8) -> *mut u8 {
    StreamBox::new_boxed(Box::new(FilesSource {
        up: own_upstream(s),
        buf: Vec::new(),
        upstream_done: false,
    }))
}

// =====================================================================================
// `std/archive` — `entries` adapter: composable tar splitting with streaming bodies.
//
// Design: generation-stamped TarEntry handles (ADR-0XX). A shared `TarEntriesState` (behind
// `Arc<Mutex>`) owns the parent upstream. Each `TarEntry` is stamped with the generation it was
// minted under; advancing the archive bumps the generation, expiring the previous entry's body.
// The body is a `TarBodySource` that re-checks the generation on each read and yields an in-band
// Error if the archive has advanced. `header()` reads from the copied metadata (always valid).
//
// Close/drop discipline: the parent upstream is closed when the shared state drops (its last
// `Arc` ref is released), NOT when the entries stream closes. This lets a `find`-style early
// stop return a live entry whose body can still be read: the entries `StreamBox` drops (last ref
// gone → `Arc` drops), the shared state drops, and that Drop closes the parent.
// =====================================================================================

/// Shared state for the entries adapter + all TarEntry/TarBody handles derived from it.
struct TarEntriesState {
    up: Upstream,
    buf: Vec<u8>,
    upstream_done: bool,
    upstream_err: Option<String>,
    /// Bytes of the CURRENT entry's body not yet consumed (by either a TarBodySource or the
    /// entries driver skipping to the next header).
    current_remaining: usize,
    /// Current entry's declared body size (for computing the tail padding after the body).
    current_size: usize,
    /// Generation counter: bumped each time the entries source advances to the next entry.
    generation: u64,
}
unsafe impl Send for TarEntriesState {}

impl TarEntriesState {
    unsafe fn fill_at_least(&mut self, n: usize) -> bool {
        while self.buf.len() < n && !self.upstream_done {
            match self.up.pull() {
                TaggedOutcome::Item(item) => {
                    append_u8_array_to(&mut self.buf, item);
                    crate::tagged::lin_tagged_release(item);
                }
                TaggedOutcome::Eof => self.upstream_done = true,
                TaggedOutcome::Err(m) => {
                    self.upstream_done = true;
                    if self.upstream_err.is_none() {
                        self.upstream_err = Some(m);
                    }
                }
            }
        }
        self.buf.len() >= n
    }

    unsafe fn take_front(&mut self, n: usize) -> Vec<u8> {
        self.fill_at_least(n);
        let take = n.min(self.buf.len());
        self.buf.drain(..take).collect()
    }
}

impl Drop for TarEntriesState {
    fn drop(&mut self) {
        // Close the parent upstream exactly once when the shared state drops (last Arc ref gone).
        unsafe { self.up.close(); }
    }
}

/// The `TarEntry` handle: an opaque, generation-stamped handle to one archive entry.
/// Lives in a `TaggedVal*(TAG_TAR_ENTRY)` box, RC-managed.
/// Holds a clone of the `Arc<Mutex<TarEntriesState>>` + a copy of the header metadata
/// (always valid even after expiry) + a one-shot body-taken flag.
pub struct TarEntryBox {
    rc: std::sync::atomic::AtomicU32,
    state: std::sync::Arc<std::sync::Mutex<TarEntriesState>>,
    generation: u64,
    name: String,
    size: usize,
    typeflag: u8,
    body_taken: std::sync::atomic::AtomicBool,
}
unsafe impl Send for TarEntryBox {}
unsafe impl Sync for TarEntryBox {}

/// Allocate a `TarEntryBox` and box it as a `TaggedVal*(TAG_TAR_ENTRY)` with refcount 1.
unsafe fn new_tar_entry_box(
    state: std::sync::Arc<std::sync::Mutex<TarEntriesState>>,
    generation: u64,
    meta: &TarEntryMeta,
) -> *mut u8 {
    use crate::tagged::{alloc_tagged, TAG_TAR_ENTRY};
    let b = Box::into_raw(Box::new(TarEntryBox {
        rc: std::sync::atomic::AtomicU32::new(1),
        state,
        generation,
        name: meta.name.clone(),
        size: meta.size,
        typeflag: meta.typeflag,
        body_taken: std::sync::atomic::AtomicBool::new(false),
    }));
    alloc_tagged(TAG_TAR_ENTRY, b as u64)
}

/// Retain a `TarEntryBox` by pointer (bump atomic RC).
#[no_mangle]
pub unsafe extern "C" fn lin_tar_entry_retain_box(p: *const u8) {
    if p.is_null() { return; }
    let b = p as *const TarEntryBox;
    (*b).rc.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
}

/// Release a `TarEntryBox` by pointer (decrement RC; free on zero).
#[no_mangle]
pub unsafe extern "C" fn lin_tar_entry_release_box(p: *const u8) {
    if p.is_null() { return; }
    let b = p as *const TarEntryBox;
    if (*b).rc.fetch_sub(1, std::sync::atomic::Ordering::AcqRel) == 1 {
        // Last reference — drop the box (releases the Arc clone).
        drop(Box::from_raw(p as *mut TarEntryBox));
    }
}

/// The body sub-stream for a `TarEntry`. Yields up to `remaining` bytes from the shared cursor.
/// Each read re-checks the generation; if the archive has advanced mid-read, returns an in-band
/// Error. `close` is a no-op (the parent is owned by the shared Arc, not this stream).
struct TarBodySource {
    state: std::sync::Arc<std::sync::Mutex<TarEntriesState>>,
    generation: u64,
}
unsafe impl Send for TarBodySource {}

impl StreamSource for TarBodySource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        let mut st = self.state.lock().unwrap();
        // Generation check: if the archive has advanced, this body is expired.
        if st.generation != self.generation {
            return TaggedOutcome::Err(
                "tar entry expired: archive advanced past this entry".to_string()
            );
        }
        if st.current_remaining == 0 {
            return TaggedOutcome::Eof;
        }
        let want = st.current_remaining.min(64 * 1024);
        let bytes = st.take_front(want);
        if bytes.is_empty() {
            st.current_remaining = 0;
            return TaggedOutcome::Eof;
        }
        st.current_remaining -= bytes.len();
        TaggedOutcome::Item(bytes_to_u8_array(&bytes))
    }
    /// No-op: the parent upstream is owned by the shared state's Arc, not this sub-stream.
    fn close(&mut self) {}
}

/// A stream that always yields a single in-band Error on the first read (used for expired /
/// double-taken bodies, where we still need to return a Stream but the first read must fail).
struct ErrorOnceSource {
    message: String,
    done: bool,
}
impl StreamSource for ErrorOnceSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        if self.done {
            return TaggedOutcome::Eof;
        }
        self.done = true;
        TaggedOutcome::Err(self.message.clone())
    }
    fn close(&mut self) {}
}

/// The entries adapter source: each `read_tagged` skips the remainder of the previous entry's
/// body + padding, bumps the generation, parses the next 512-byte header, and returns a
/// `TarEntry` handle (`TaggedVal*(TAG_TAR_ENTRY)`). Zero-block or EOF → Eof.
struct TarEntriesSource {
    state: std::sync::Arc<std::sync::Mutex<TarEntriesState>>,
}
unsafe impl Send for TarEntriesSource {}

impl StreamSource for TarEntriesSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        let mut st = self.state.lock().unwrap();
        // Skip any unread bytes of the previous entry's body + tar padding.
        let undrained = st.current_remaining;
        let padding = padded_len(st.current_size) - st.current_size;
        let skip = undrained + padding;
        if skip > 0 {
            let _ = st.take_front(skip);
            st.current_remaining = 0;
        }
        // Propagate any upstream error that occurred during the skip.
        if let Some(m) = st.upstream_err.take() {
            return TaggedOutcome::Err(m);
        }
        // Bump the generation — the previous entry (and its body) is now expired.
        st.generation += 1;
        let gen = st.generation;

        // Parse the next 512-byte header.
        let header = st.take_front(512);
        if let Some(m) = st.upstream_err.take() {
            return TaggedOutcome::Err(m);
        }
        let meta = match parse_tar_header(&header) {
            Some(m) => m,
            None => return TaggedOutcome::Eof, // end-of-archive
        };

        st.current_size = meta.size;
        st.current_remaining = meta.size;

        // Mint the entry handle — holds an Arc clone.
        let state_clone = self.state.clone();
        drop(st); // release lock before cloning Arc (Arc::clone does its own locking)
        let handle = new_tar_entry_box(state_clone, gen, &meta);
        TaggedOutcome::Item(handle)
    }

    /// The entries stream closing does NOT close the parent; it just marks this source as done.
    /// The parent is closed when the shared state drops (last Arc gone: entries source gone AND
    /// all TarEntry handles gone). No-op is correct here.
    fn close(&mut self) {}
}

/// `entries(s)` → `Stream<TarEntry>`. Adapter: splits the upstream byte stream into a stream
/// of generation-stamped `TarEntry` handles. The parent is moved in; it is closed when the last
/// live reference to the shared state drops (the last TarEntry handle and the entries stream
/// itself are both gone).
#[no_mangle]
pub unsafe extern "C" fn lin_stream_tar_entries(s: *const u8) -> *mut u8 {
    let state = std::sync::Arc::new(std::sync::Mutex::new(TarEntriesState {
        up: own_upstream(s),
        buf: Vec::new(),
        upstream_done: false,
        upstream_err: None,
        current_remaining: 0,
        current_size: 0,
        generation: 0,
    }));
    StreamBox::new_boxed(Box::new(TarEntriesSource { state }))
}

/// `header(e)` → `TarHeader` object: `{ name, size, typeflag, isDir }`. Always valid; built
/// from the copied metadata in the TarEntryBox (does NOT lock the shared state).
///
/// Returns a raw `LinObject*` (NOT a tagged box) because the stdlib wrapper `header(e): TarHeader`
/// generates code that calls `lin_object_get(lin_tar_header(e), key)` directly — the codegen
/// treats the return as an unboxed object, not a `TaggedVal*`. The caller (std_archive_header)
/// then builds a sealed record from the fields and manages RC from there.
#[no_mangle]
pub unsafe extern "C" fn lin_tar_header(e: *const u8) -> *mut u8 {
    use crate::tagged::TAG_TAR_ENTRY;
    if e.is_null() {
        // Return a minimal empty map rather than an error-tagged value, since the caller
        // will immediately call lin_map_get on the result (Phase 2: non-sealed objects = LinMap).
        let map = crate::map::lin_map_alloc(0, 0);
        return map as *mut u8;
    }
    let tv = &*(e as *const crate::tagged::TaggedVal);
    if tv.tag != TAG_TAR_ENTRY {
        let map = crate::map::lin_map_alloc(0, 0);
        return map as *mut u8;
    }
    let entry = tv.payload as *const TarEntryBox;
    let meta = TarEntryMeta {
        name: (*entry).name.clone(),
        size: (*entry).size,
        typeflag: (*entry).typeflag,
    };
    // Return the raw LinObject* — the tagged wrapper (alloc_tagged) is NOT used here because
    // std_archive_header calls lin_object_get directly on this return value.
    make_meta_object_unboxed(&meta)
}

/// `body(e)` → `Stream<UInt8[]>`. Always returns a Stream (failures are in-band on first read):
///   - If the entry is expired (archive advanced past it): first read yields Error.
///   - If the body was already taken: first read yields Error.
///   - Otherwise: mint a `TarBodySource` over the shared cursor for this entry.
#[no_mangle]
pub unsafe extern "C" fn lin_tar_body(e: *const u8) -> *mut u8 {
    use crate::tagged::TAG_TAR_ENTRY;
    if e.is_null() {
        return StreamBox::new_boxed(Box::new(ErrorOnceSource {
            message: "lin_tar_body: null entry".to_string(),
            done: false,
        }));
    }
    let tv = &*(e as *const crate::tagged::TaggedVal);
    if tv.tag != TAG_TAR_ENTRY {
        return StreamBox::new_boxed(Box::new(ErrorOnceSource {
            message: "lin_tar_body: not a TarEntry".to_string(),
            done: false,
        }));
    }
    let entry = tv.payload as *const TarEntryBox;

    // Check generation (without holding the TarEntriesState lock yet — if the entry generation
    // already mismatches the state's generation, the entry is expired).
    let entry_gen = (*entry).generation;
    {
        let st = (*entry).state.lock().unwrap();
        if st.generation != entry_gen {
            return StreamBox::new_boxed(Box::new(ErrorOnceSource {
                message: "tar entry expired: archive advanced past this entry".to_string(),
                done: false,
            }));
        }
    }

    // Atomically take the body-taken flag.
    use std::sync::atomic::Ordering;
    if (*entry).body_taken.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
        return StreamBox::new_boxed(Box::new(ErrorOnceSource {
            message: "tar entry body already read".to_string(),
            done: false,
        }));
    }

    // Mint the body sub-stream. It holds an Arc clone of the shared state.
    StreamBox::new_boxed(Box::new(TarBodySource {
        state: (*entry).state.clone(),
        generation: entry_gen,
    }))
}

// =====================================================================================
// `std/csv` — quote-aware CSV row assembler over a `Stream<UInt8[]>` (csv proposal).
//
// A naive `readStream(...).lines().map(parseLine)` is WRONG for CSV: a single quoted field may
// contain a `\n` and span several physical lines, so a line-stream is not row-aligned. `rows`/
// `recordRows` therefore run a small stateful FSM over the BYTE stream directly — it tracks whether
// the scanner is currently INSIDE a quoted field and only completes a record on a record-
// terminating newline seen OUTSIDE quotes; a newline inside quotes is buffered as field content.
// This keeps memory bounded to a single in-flight record. An unterminated quote at EOF surfaces as
// an in-band Error; the in-flight buffer is capped (à la `linesMax`) so adversarial input fails
// rather than OOMing.
//
// The byte FSM mirrors `stdlib/csv.lin`'s eager scanner RFC 4180 rules EXACTLY (quoted fields,
// embedded delimiter/newline/quote, `""` escape, CRLF or LF terminators). It is a small, separate
// Rust implementation because a Stream node with carry-over state cannot be expressed in pure Lin
// (the eager parser stays pure Lin). The two share no code but share the same five-state spec.

/// Cap on the in-flight record buffer before failing in-band (default 64 MiB, à la `MAX_LINE_BYTES`).
const MAX_CSV_RECORD_BYTES: usize = 64 * 1024 * 1024;

/// The stateful byte-level CSV row assembler. Fed bytes (in chunks), it emits complete records as
/// `Vec<Vec<u8>>` (fields are raw UTF-8 byte vectors). State carries across chunk boundaries.
struct CsvAssembler {
    delim: u8,
    /// The field currently being built.
    field: Vec<u8>,
    /// Fields accumulated so far for the in-flight record.
    record: Vec<Vec<u8>>,
    /// True while the scanner is INSIDE a quoted field (a newline here is field content).
    in_quotes: bool,
    /// True just after a closing quote of a quoted field — a following `"` is an escaped quote;
    /// any other byte (delimiter / newline / EOF) terminates the field.
    after_quote: bool,
    /// True if the current field began with a `"` (a quoted field).
    field_quoted: bool,
    /// True if anything (any field/byte) has been seen for the current record — distinguishes a
    /// genuine empty final record (drop a spurious trailing newline) from a one-empty-field row.
    record_started: bool,
    /// True if the byte just consumed was a CR that ended a record: a single following LF (CRLF) is
    /// then ignored rather than starting a spurious empty record.
    skip_next_lf: bool,
    /// Total bytes buffered in the in-flight record for the cap check.
    buffered: usize,
    /// Latched malformed-input flag (stray quote in an unquoted field). Surfaced at the next emit.
    malformed: bool,
}

impl CsvAssembler {
    fn new(delim: u8) -> CsvAssembler {
        let d = if delim == 0 { b',' } else { delim };
        CsvAssembler {
            delim: d,
            field: Vec::new(),
            record: Vec::new(),
            in_quotes: false,
            after_quote: false,
            field_quoted: false,
            record_started: false,
            skip_next_lf: false,
            buffered: 0,
            malformed: false,
        }
    }

    /// Finish the current field, pushing it onto the in-flight record.
    fn end_field(&mut self) {
        self.record.push(std::mem::take(&mut self.field));
        self.field_quoted = false;
        self.after_quote = false;
    }

    /// Finish the current record, returning its fields. Resets per-record state.
    fn end_record(&mut self) -> Vec<Vec<u8>> {
        self.end_field();
        self.record_started = false;
        self.buffered = 0;
        std::mem::take(&mut self.record)
    }

    /// Feed one byte. Returns `Some(record)` when a record completes (newline outside quotes).
    /// Sets `self.malformed` on a stray quote in an unquoted field.
    fn push_byte(&mut self, b: u8) -> Option<Vec<Vec<u8>>> {
        // CRLF: a LF immediately after a record-ending CR is swallowed (not a new empty record).
        if self.skip_next_lf {
            self.skip_next_lf = false;
            if b == b'\n' {
                return None;
            }
        }
        if self.in_quotes {
            self.record_started = true;
            if b == b'"' {
                // Closing quote OR first of an escaped pair: defer the decision to after_quote.
                self.in_quotes = false;
                self.after_quote = true;
            } else {
                self.field.push(b);
                self.buffered += 1;
            }
            return None;
        }
        if self.after_quote {
            // We just saw a `"` that closed a quoted run.
            self.after_quote = false;
            if b == b'"' {
                // Escaped quote: emit a literal `"` and re-enter the quoted field.
                self.field.push(b'"');
                self.buffered += 1;
                self.in_quotes = true;
                return None;
            }
            // Otherwise the quoted field is closed; fall through to handle `b` structurally.
        }
        if b == b'"' {
            self.record_started = true;
            if self.field.is_empty() && !self.field_quoted {
                // Opening quote of a quoted field.
                self.field_quoted = true;
                self.in_quotes = true;
            } else {
                // A `"` in the middle of an unquoted field is a stray-quote error (RFC 4180).
                self.malformed = true;
            }
            return None;
        }
        if b == self.delim {
            self.record_started = true;
            self.end_field();
            return None;
        }
        if b == b'\n' {
            self.record_started = true;
            return Some(self.end_record());
        }
        if b == b'\r' {
            self.record_started = true;
            self.skip_next_lf = true;
            return Some(self.end_record());
        }
        // Ordinary field byte.
        self.record_started = true;
        self.field.push(b);
        self.buffered += 1;
        None
    }

    /// At EOF, flush a final unterminated record (if any bytes were started). Returns the last
    /// record, or None if the buffer is clean (a spurious trailing newline left nothing). The
    /// caller must check `in_quotes` FIRST — an EOF inside quotes is the unterminated-quote Error.
    fn finish(&mut self) -> Option<Vec<Vec<u8>>> {
        if self.after_quote {
            // A closed quoted field with nothing after it: a valid final field.
            self.after_quote = false;
        }
        if self.record_started || !self.field.is_empty() || !self.record.is_empty() {
            Some(self.end_record())
        } else {
            None
        }
    }
}

/// Build a boxed `String[]` (TAG_ARRAY of TAG_STR) from a record's byte-vector fields. Each field
/// becomes a fresh LinString that the array owns (push_tagged copies the stack TaggedVal inline and
/// takes ownership of the inner value — the io.rs/fs.rs idiom, no box shell to free).
unsafe fn record_to_string_array(fields: &[Vec<u8>]) -> *mut u8 {
    use crate::array::{lin_array_alloc, lin_array_push_tagged};
    use crate::string::lin_string_from_bytes;
    use crate::tagged::{alloc_tagged, TAG_ARRAY, TAG_STR};
    let arr = lin_array_alloc(fields.len().max(1) as u64);
    for f in fields {
        let s = lin_string_from_bytes(f.as_ptr(), f.len() as u32);
        let mut tv: TaggedVal = std::mem::zeroed();
        tv.tag = TAG_STR;
        tv.payload = s as u64;
        lin_array_push_tagged(arr, &tv as *const TaggedVal as *const u8);
    }
    alloc_tagged(TAG_ARRAY, arr as u64)
}

/// Raw record outcome from `CsvRowsSource::next_raw` — bypasses the boxed-String[] round-trip
/// used by `rows()`, letting `CsvRecordsSource` work directly with byte vectors.
enum RawRecord {
    Eof,
    Err(String),
    Item(Vec<Vec<u8>>),
}

/// `rows(s)`: a `Stream<String[]>` over a byte stream, quote-aware (see CsvAssembler). Buffers only
/// the in-flight record; emits one row at a time. Unterminated quote at EOF -> in-band Error.
struct CsvRowsSource {
    up: Upstream,
    asm: CsvAssembler,
    /// Completed rows decoded from the current buffer, awaiting emission (one per pull).
    queue: std::collections::VecDeque<Vec<Vec<u8>>>,
    upstream_done: bool,
    /// True once the final EOF flush has run.
    flushed: bool,
}

impl CsvRowsSource {
    /// Feed a chunk's bytes through the assembler, enqueueing every completed record. Returns an
    /// Err message on a stray-quote fault or a buffer-cap overflow.
    fn feed(&mut self, chunk: &[u8]) -> Result<(), String> {
        for &b in chunk {
            if let Some(rec) = self.asm.push_byte(b) {
                self.queue.push_back(rec);
            }
            if self.asm.malformed {
                return Err("csv: malformed input (stray quote in an unquoted field)".to_string());
            }
            if self.asm.buffered > MAX_CSV_RECORD_BYTES {
                return Err(format!(
                    "csv: a single record exceeded {} bytes without a terminator — refusing to buffer unbounded input",
                    MAX_CSV_RECORD_BYTES
                ));
            }
        }
        Ok(())
    }

    /// Pull one raw record (no boxing) from the assembler/queue, threading EOF/Err.
    /// `CsvRowsSource::read_tagged` wraps this into a boxed String[]; `CsvRecordsSource` calls
    /// this directly to avoid building and immediately tearing down that box.
    unsafe fn next_raw(&mut self) -> RawRecord {
        loop {
            if let Some(rec) = self.queue.pop_front() {
                return RawRecord::Item(rec);
            }
            if self.upstream_done {
                if self.flushed {
                    return RawRecord::Eof;
                }
                self.flushed = true;
                // EOF inside a quoted field (or a dangling `""` open) is the unterminated-quote Error.
                if self.asm.in_quotes {
                    return RawRecord::Err(
                        "csv: unterminated quoted field at end of input".to_string(),
                    );
                }
                if let Some(rec) = self.asm.finish() {
                    return RawRecord::Item(rec);
                }
                return RawRecord::Eof;
            }
            match self.up.pull() {
                TaggedOutcome::Eof => self.upstream_done = true,
                TaggedOutcome::Err(m) => return RawRecord::Err(m),
                TaggedOutcome::Item(item) => {
                    let mut bytes: Vec<u8> = Vec::new();
                    append_u8_array_to(&mut bytes, item);
                    crate::tagged::lin_tagged_release(item);
                    if let Err(m) = self.feed(&bytes) {
                        return RawRecord::Err(m);
                    }
                }
            }
        }
    }
}

impl StreamSource for CsvRowsSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        match self.next_raw() {
            RawRecord::Eof => TaggedOutcome::Eof,
            RawRecord::Err(m) => TaggedOutcome::Err(m),
            RawRecord::Item(rec) => TaggedOutcome::Item(record_to_string_array(&rec)),
        }
    }
    fn close(&mut self) {
        unsafe { self.up.close(); }
    }
}

/// `recordRows(s)`: like `rows`, but consumes the first record as the header and yields one
/// `{ String: String }` per subsequent data row (last-wins dup headers, lenient ragged rows).
/// Header key LinStrings are interned once and reused across all rows to avoid per-row reallocation.
///
/// Optional projection: when `wanted` is `Some(names)`, only columns whose header name is in
/// `names` are interned and materialized per row. All other columns are never allocated. The
/// projected slot list (`proj`) maps each kept column to its original row-field index.
struct CsvRecordsSource {
    inner: CsvRowsSource,
    header: Option<Vec<Vec<u8>>>,
    /// Pre-built LinString pointers for the header keys, one per column (full path).
    /// Each pointer holds a persistent ref (refcount 1); `lin_map_set` takes its own additional
    /// ref on each fresh insert, so we must NOT release these until the source drops.
    /// In the projected path this vec is EMPTY — `proj` holds the kept keys instead.
    header_keys: Vec<*mut crate::string::LinString>,
    /// Projected path: each entry is (row_field_index, interned_key). Built once from the header
    /// when `wanted` is `Some`. Empty when running the full (non-projected) path.
    proj: Vec<(usize, *mut crate::string::LinString)>,
    /// Requested column names for projection. `None` = all columns (default, full path).
    wanted: Option<Vec<Vec<u8>>>,
    /// True once the header has been pulled (it may legitimately be absent for an empty stream).
    header_done: bool,
}

impl CsvRecordsSource {
    /// Pull one raw record from the inner rows source, threading EOF/Err.
    unsafe fn pull_record(&mut self) -> Result<Option<Vec<Vec<u8>>>, String> {
        match self.inner.next_raw() {
            RawRecord::Eof => Ok(None),
            RawRecord::Err(m) => Err(m),
            RawRecord::Item(rec) => Ok(Some(rec)),
        }
    }
}

impl Drop for CsvRecordsSource {
    fn drop(&mut self) {
        // Release the persistent ref on each cached header-key LinString (full path).
        for &k in &self.header_keys {
            if !k.is_null() {
                unsafe { crate::string::lin_string_release(k); }
            }
        }
        self.header_keys.clear();
        // Release the persistent ref on each projected key (projected path).
        for &(_, k) in &self.proj {
            if !k.is_null() {
                unsafe { crate::string::lin_string_release(k); }
            }
        }
        self.proj.clear();
    }
}

// SAFETY: CsvRecordsSource is only accessed from a single stream-driver thread; the raw
// LinString pointers in header_keys/proj are owned by this struct and not shared.
unsafe impl Send for CsvRecordsSource {}

/// Build a `{ String: String }` LinMap using pre-interned key LinStrings. Each key pointer already
/// has a persistent ref owned by `CsvRecordsSource`; `lin_map_set` takes an additional ref per
/// fresh insert, so we do NOT release keys here. Values are allocated fresh per row and released
/// after the map takes its own ref via `lin_map_set`.
unsafe fn record_to_object_cached(keys: &[*mut crate::string::LinString], row: &[Vec<u8>]) -> *mut u8 {
    use crate::map::{lin_map_alloc, lin_map_set};
    use crate::string::{lin_string_from_bytes, lin_string_release};
    use crate::tagged::{alloc_tagged, TAG_MAP, TAG_STR, TaggedVal};
    let map = lin_map_alloc(keys.len().max(1) as u32, 0);
    for (i, &k) in keys.iter().enumerate() {
        if i >= row.len() {
            break; // short row: omit the trailing keys
        }
        let v = lin_string_from_bytes(row[i].as_ptr(), row[i].len() as u32);
        let mut tv: TaggedVal = std::mem::zeroed();
        tv.tag = TAG_STR;
        tv.payload = v as u64;
        lin_map_set(map, k, &tv); // last-wins on dup header names; map takes its own key ref
        lin_string_release(v);    // map has retained v; drop our ref
        // key NOT released here — source's persistent ref outlives all rows
    }
    alloc_tagged(TAG_MAP, map as u64)
}

/// Build a projected `{ String: String }` LinMap from `(row_index, interned_key)` pairs.
/// Only the selected columns are materialized; others are never allocated.
unsafe fn record_to_object_projected(proj: &[(usize, *mut crate::string::LinString)], row: &[Vec<u8>]) -> *mut u8 {
    use crate::map::{lin_map_alloc, lin_map_set};
    use crate::string::{lin_string_from_bytes, lin_string_release};
    use crate::tagged::{alloc_tagged, TAG_MAP, TAG_STR, TaggedVal};
    let map = lin_map_alloc(proj.len().max(1) as u32, 0);
    for &(row_idx, k) in proj {
        if row_idx >= row.len() {
            continue; // ragged row: this column is absent, skip
        }
        let v = lin_string_from_bytes(row[row_idx].as_ptr(), row[row_idx].len() as u32);
        let mut tv: TaggedVal = std::mem::zeroed();
        tv.tag = TAG_STR;
        tv.payload = v as u64;
        lin_map_set(map, k, &tv);
        lin_string_release(v);
        // key NOT released here — source's persistent ref outlives all rows
    }
    alloc_tagged(TAG_MAP, map as u64)
}

impl StreamSource for CsvRecordsSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        if !self.header_done {
            self.header_done = true;
            match self.pull_record() {
                Err(m) => return TaggedOutcome::Err(m),
                Ok(None) => return TaggedOutcome::Eof, // empty stream: no header, no rows
                Ok(Some(h)) => {
                    match &self.wanted {
                        None => {
                            // Full path: intern every header column.
                            self.header_keys = h
                                .iter()
                                .map(|col| {
                                    crate::string::lin_string_from_bytes(col.as_ptr(), col.len() as u32)
                                })
                                .collect();
                        }
                        Some(names) => {
                            // Projected path: only intern and remember columns in `names`.
                            // A wanted name absent from the header simply contributes no slot (lenient).
                            let mut proj = Vec::with_capacity(names.len());
                            for (col_idx, col_bytes) in h.iter().enumerate() {
                                if names.iter().any(|n| n == col_bytes) {
                                    let k = crate::string::lin_string_from_bytes(
                                        col_bytes.as_ptr(), col_bytes.len() as u32,
                                    );
                                    proj.push((col_idx, k));
                                }
                            }
                            self.proj = proj;
                        }
                    }
                    self.header = Some(h);
                }
            }
        }
        // No header (e.g. an empty stream that ended before yielding one) => Eof.
        if self.header.is_none() {
            return TaggedOutcome::Eof;
        }
        match self.pull_record() {
            Err(m) => TaggedOutcome::Err(m),
            Ok(None) => TaggedOutcome::Eof,
            Ok(Some(row)) => {
                if self.wanted.is_none() {
                    TaggedOutcome::Item(record_to_object_cached(&self.header_keys, &row))
                } else {
                    TaggedOutcome::Item(record_to_object_projected(&self.proj, &row))
                }
            }
        }
    }
    fn close(&mut self) {
        self.inner.close();
    }
}

/// `rows(s, delim)` → a `Stream<String[]>` of parsed CSV rows over the byte stream `s`.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_csv_rows(s: *const u8, delim: i32) -> *mut u8 {
    let up = own_upstream(s);
    StreamBox::new_boxed(Box::new(CsvRowsSource {
        up,
        asm: CsvAssembler::new(delim as u8),
        queue: std::collections::VecDeque::new(),
        upstream_done: false,
        flushed: false,
    }))
}

/// `recordRows(s, delim)` → a `Stream<{ String: String }>` keyed by the first (header) record.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_csv_records(s: *const u8, delim: i32) -> *mut u8 {
    let up = own_upstream(s);
    let inner = CsvRowsSource {
        up,
        asm: CsvAssembler::new(delim as u8),
        queue: std::collections::VecDeque::new(),
        upstream_done: false,
        flushed: false,
    };
    StreamBox::new_boxed(Box::new(CsvRecordsSource {
        inner,
        header: None,
        header_keys: Vec::new(),
        proj: Vec::new(),
        wanted: None,
        header_done: false,
    }))
}

/// `recordRows(s, delim, cols)` → like `lin_stream_csv_records` but only materializes the columns
/// named in `cols` (a boxed `String[]`). Columns not in `cols` are never allocated per row.
/// `cols` is borrowed (caller retains ownership); pass NULL for no projection (all columns).
#[no_mangle]
pub unsafe extern "C" fn lin_stream_csv_records_projected(
    s: *const u8, delim: i32, cols: *const u8,
) -> *mut u8 {
    // Decode the `cols` String[] argument into a Vec<Vec<u8>> of wanted column names.
    // We read from the boxed array but do NOT free it (caller owns it).
    let wanted: Option<Vec<Vec<u8>>> = if cols.is_null() {
        None
    } else {
        use crate::tagged::{TAG_ARRAY, TAG_STR};
        use crate::string::LinString;
        let tv = &*(cols as *const crate::tagged::TaggedVal);
        if tv.tag != TAG_ARRAY {
            None
        } else {
            let arr = tv.payload as *const crate::array::LinArray;
            if arr.is_null() {
                None
            } else {
                let n = crate::array::lin_array_length(arr) as usize;
                let mut names: Vec<Vec<u8>> = Vec::with_capacity(n);
                for i in 0..n {
                    let elem = crate::array::lin_array_get_tagged(arr, i as i64);
                    if !elem.is_null() && (*elem).tag == TAG_STR {
                        let sp = (*elem).payload as *const LinString;
                        let bytes = std::slice::from_raw_parts((*sp).data.as_ptr(), (*sp).len as usize);
                        names.push(bytes.to_vec());
                    }
                    // Release the +1 ref returned by lin_array_get_tagged.
                    if !elem.is_null() {
                        crate::tagged::lin_tagged_release(elem as *mut u8);
                    }
                }
                if names.is_empty() { None } else { Some(names) }
            }
        }
    };
    let up = own_upstream(s);
    let inner = CsvRowsSource {
        up,
        asm: CsvAssembler::new(delim as u8),
        queue: std::collections::VecDeque::new(),
        upstream_done: false,
        flushed: false,
    };
    StreamBox::new_boxed(Box::new(CsvRecordsSource {
        inner,
        header: None,
        header_keys: Vec::new(),
        proj: Vec::new(),
        wanted,
        header_done: false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Read the `message` field of a canonical Error object (`{ type:"error", message }`).
    unsafe fn error_message_of(v: *const u8) -> Option<String> {
        // `stream_arg_error` returns Some(msg) iff `v` is an error-shaped object.
        stream_arg_error(v)
    }

    /// Regression: a failed source (`readStream` of a missing file) returns an Error OBJECT, not a
    /// Stream. Flowing that Error into a stream adapter/terminal must thread it IN-BAND — neither
    /// abort on a null-box deref (the old `readText`/`collect` crash) nor silently swallow it as an
    /// empty stream (the old `drain`/`for` behaviour). We simulate the failed source with the same
    /// error object `lin_fs_open` produces and drive each entry point.
    #[test]
    fn error_arg_threads_in_band_not_abort_or_swallow() {
        unsafe {
            // 1. A terminal called DIRECTLY on the error arg surfaces the error (no abort).
            let err = || crate::fs::make_error_tagged("no such file");
            let e = err();
            let r = lin_stream_read_text(e);
            assert_eq!(error_message_of(r), Some("no such file".to_string()),
                "readText on a failed source must surface the Error, not abort");
            crate::tagged::lin_tagged_release(r);
            crate::tagged::lin_tagged_release(e);

            // 2. drain() on the error arg surfaces the error (was: silent Null).
            let e = err();
            let r = lin_stream_drain(e);
            assert_eq!(error_message_of(r), Some("no such file".to_string()),
                "drain on a failed source must surface the Error, not return Null");
            crate::tagged::lin_tagged_release(r);
            crate::tagged::lin_tagged_release(e);

            // 3. An ADAPTER over the error arg latches it; the downstream terminal surfaces it.
            //    `lines(err).drain()` mirrors the report pipeline's `readStream(p).lines()...`.
            let e = err();
            let piped = lin_stream_lines(e, 0);
            crate::tagged::lin_tagged_release(e); // the adapter took its own ref / latched the error
            let r = lin_stream_drain(piped);
            assert_eq!(error_message_of(r), Some("no such file".to_string()),
                "an adapter over a failed source must propagate the Error to the terminal");
            crate::tagged::lin_tagged_release(r);
            crate::tagged::lin_tagged_release(piped);
        }
    }

    /// A fake backend that hands out a fixed sequence of chunks then EOF, and counts how many
    /// times `close` is invoked. The counter is shared (Arc) so the test can assert close-once
    /// AFTER the box is fully released — this is the close-once invariant the brief mandates be
    /// verified (cargo test can't see fd leaks, but it CAN assert the finalizer fires exactly
    /// once via this counter; the ASan leg confirms no double-free of the box itself).
    struct CountingSource {
        chunks: Vec<Vec<u8>>,
        idx: usize,
        close_count: Arc<AtomicUsize>,
        fail_after: Option<usize>,
    }

    impl StreamSource for CountingSource {
        fn read(&mut self) -> Result<Option<Vec<u8>>, String> {
            if let Some(n) = self.fail_after {
                if self.idx == n {
                    return Err("injected read error".to_string());
                }
            }
            if self.idx >= self.chunks.len() {
                return Ok(None);
            }
            let c = self.chunks[self.idx].clone();
            self.idx += 1;
            Ok(Some(c))
        }
        fn close(&mut self) {
            self.close_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn make(chunks: Vec<Vec<u8>>, close_count: Arc<AtomicUsize>, fail_after: Option<usize>) -> *mut u8 {
        unsafe {
            StreamBox::new_boxed(Box::new(CountingSource {
                chunks,
                idx: 0,
                close_count,
                fail_after,
            }))
        }
    }

    #[test]
    fn dropped_stream_closes_exactly_once() {
        unsafe {
            let cc = Arc::new(AtomicUsize::new(0));
            let s = make(vec![b"ab".to_vec(), b"cd".to_vec()], cc.clone(), None);
            // Read both chunks, then EOF.
            let c1 = stream_read_outcome(s);
            assert!(matches!(c1, ReadOutcome::Chunk(ref b) if b == b"ab"));
            let c2 = stream_read_outcome(s);
            assert!(matches!(c2, ReadOutcome::Chunk(ref b) if b == b"cd"));
            assert!(matches!(stream_read_outcome(s), ReadOutcome::Eof));
            // Never explicitly closed — the finalizer must close it on the last release.
            assert_eq!(cc.load(Ordering::SeqCst), 0, "not closed before release");
            // Release the boxed TaggedVal*; this dispatches the TAG_STREAM finalizer.
            crate::tagged::lin_tagged_release(s);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "finalizer closed fd exactly once");
        }
    }

    #[test]
    fn explicit_close_then_drop_closes_exactly_once() {
        unsafe {
            let cc = Arc::new(AtomicUsize::new(0));
            let s = make(vec![b"x".to_vec()], cc.clone(), None);
            stream_close(s);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "explicit close fires once");
            // Reading a closed stream yields EOF.
            assert!(matches!(stream_read_outcome(s), ReadOutcome::Eof));
            // Closing again is a no-op (idempotent).
            stream_close(s);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "second close is a no-op");
            // The finalizer must NOT close again (already closed).
            crate::tagged::lin_tagged_release(s);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "finalizer skips already-closed stream");
        }
    }

    #[test]
    fn retain_release_balanced_close_once() {
        unsafe {
            let cc = Arc::new(AtomicUsize::new(0));
            let s = make(vec![b"q".to_vec()], cc.clone(), None);
            let b = unwrap_stream(s);
            // Simulate a second owner (e.g. a clone / a captured copy): retain then release.
            lin_stream_retain_box(b as *const u8);
            crate::tagged::lin_tagged_release(s); // drops one ref — NOT the last, box survives.
            assert_eq!(cc.load(Ordering::SeqCst), 0, "not closed while a ref remains");
            // Drop the second ref via the raw release path (mirrors a moved/worker release).
            lin_stream_release_box(b as *const u8);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "closed once when last ref drops");
        }
    }

    #[test]
    fn read_error_then_close_once() {
        unsafe {
            let cc = Arc::new(AtomicUsize::new(0));
            let s = make(vec![b"a".to_vec()], cc.clone(), Some(1)); // fail on the 2nd read
            assert!(matches!(stream_read_outcome(s), ReadOutcome::Chunk(_)));
            assert!(matches!(stream_read_outcome(s), ReadOutcome::Err(_)));
            crate::tagged::lin_tagged_release(s);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "errored stream still closes once");
        }
    }

    #[test]
    fn file_source_reads_bytes_then_eof_and_closes() {
        unsafe {
            let path = "/tmp/lin_stream_file_source_test.bin";
            std::fs::write(path, b"hello stream").unwrap();
            let path_str = crate::string::lin_string_from_bytes(path.as_ptr(), path.len() as u32);
            let path_tagged = crate::tagged::alloc_tagged(crate::tagged::TAG_STR, path_str as u64);

            let s = lin_fs_open(path_tagged as *const u8);
            assert_eq!(crate::tagged::lin_get_tag(s), TAG_STREAM, "openRead yields a Stream");

            // First read: the whole file (< chunk size).
            match stream_read_outcome(s) {
                ReadOutcome::Chunk(b) => assert_eq!(&b, b"hello stream"),
                _ => panic!("expected a chunk"),
            }
            // Next read: EOF.
            assert!(matches!(stream_read_outcome(s), ReadOutcome::Eof));

            crate::tagged::lin_tagged_release(s); // finalizer closes the fd (no double-close)
            crate::tagged::lin_tagged_release(path_tagged);
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn open_missing_file_is_error() {
        unsafe {
            let path = "/tmp/lin_stream_definitely_missing_file_xyz.bin";
            let _ = std::fs::remove_file(path);
            let path_str = crate::string::lin_string_from_bytes(path.as_ptr(), path.len() as u32);
            let path_tagged = crate::tagged::alloc_tagged(crate::tagged::TAG_STR, path_str as u64);
            let r = lin_fs_open(path_tagged as *const u8);
            // An open failure is an Error map (TAG_MAP), NOT a stream.
            assert_eq!(crate::tagged::lin_get_tag(r), crate::tagged::TAG_MAP);
            crate::tagged::lin_tagged_release(r);
            crate::tagged::lin_tagged_release(path_tagged);
        }
    }

    #[test]
    fn read_tagged_chunk_is_flat_u8_array() {
        unsafe {
            let cc = Arc::new(AtomicUsize::new(0));
            let s = make(vec![vec![1u8, 2, 3]], cc.clone(), None);
            let tagged = lin_stream_read(s);
            assert_eq!(crate::tagged::lin_get_tag(tagged), crate::tagged::TAG_ARRAY);
            let arr = crate::tagged::lin_unbox_ptr(tagged) as *const crate::array::LinArray;
            assert_eq!(crate::array::lin_array_length(arr), 3);
            crate::tagged::lin_tagged_release(tagged);
            // EOF reads back as the null value.
            let eof = lin_stream_read(s);
            assert!(eof.is_null());
            crate::tagged::lin_tagged_release(s);
            assert_eq!(cc.load(Ordering::SeqCst), 1);
        }
    }

    // Stage 4 adapter tests. These exercise the lazy graph + terminals WITHOUT a Lin closure
    // (lines/take/chunks/collect/lines+drain are closure-free); map/filter's closure path is
    // covered end-to-end by the integration + stdlib stream tests. Run under ASan they verify
    // the adapter→upstream close propagation and per-item box release have no UAF/double-free.

    #[test]
    fn lines_adapter_splits_and_closes_upstream_once() {
        unsafe {
            let cc = Arc::new(AtomicUsize::new(0));
            let src = make(vec![b"one\ntwo\n".to_vec(), b"three".to_vec()], cc.clone(), None);
            let lines = lin_stream_lines(src as *const u8, 0);
            // Pull lines: "one", "two", "three", then EOF.
            for expected in ["one", "two", "three"] {
                match pull_tagged(unwrap_stream(lines)) {
                    TaggedOutcome::Item(item) => {
                        let s = crate::tagged::lin_unbox_ptr(item) as *const crate::string::LinString;
                        let slice = std::slice::from_raw_parts((*s).data.as_ptr(), (*s).len as usize);
                        assert_eq!(std::str::from_utf8(slice).unwrap(), expected);
                        crate::tagged::lin_tagged_release(item);
                    }
                    _ => panic!("expected line {expected}"),
                }
            }
            assert!(matches!(pull_tagged(unwrap_stream(lines)), TaggedOutcome::Eof));
            // Releasing the adapter closes the upstream exactly once (and frees both boxes).
            crate::tagged::lin_tagged_release(lines);
            crate::tagged::lin_tagged_release(src); // drop our local ref to the upstream box
            assert_eq!(cc.load(Ordering::SeqCst), 1, "upstream closed exactly once via adapter");
        }
    }

    // Backpressure guard: a newline-less stream must NOT buffer unbounded. `lines()` caps the
    // partial-line buffer at MAX_LINE_BYTES and fails in-band once exceeded. We feed 1 MiB chunks
    // with no '\n' and assert the adapter returns Err (not OOM) once the cap is crossed, and that
    // the upstream still closes exactly once afterwards.
    #[test]
    fn lines_adapter_caps_unbounded_line() {
        unsafe {
            let cc = Arc::new(AtomicUsize::new(0));
            // Use a small EXPLICIT cap (1 KiB) so the test is fast and also exercises the
            // configurable `linesMax`/`lin_stream_lines(s, n)` path, not just the 64 MiB default.
            let cap = 1024i64;
            let chunk = vec![b'x'; 512]; // 512 B, no newline
            let n_chunks = (cap as usize / chunk.len()) + 4; // enough to cross the cap
            let chunks: Vec<Vec<u8>> = (0..n_chunks).map(|_| chunk.clone()).collect();
            let src = make(chunks, cc.clone(), None);
            let lines = lin_stream_lines(src as *const u8, cap);
            // Driving the adapter must surface an Err once the line buffer exceeds the cap,
            // rather than buffering the whole stream.
            let mut saw_err = false;
            for _ in 0..(n_chunks + 4) {
                match pull_tagged(unwrap_stream(lines)) {
                    TaggedOutcome::Err(m) => {
                        assert!(m.contains("without a newline"), "unexpected err: {m}");
                        saw_err = true;
                        break;
                    }
                    TaggedOutcome::Eof => panic!("hit EOF before the cap fired — buffer was unbounded"),
                    TaggedOutcome::Item(item) => {
                        crate::tagged::lin_tagged_release(item);
                        panic!("emitted a line for newline-less input before the cap");
                    }
                }
            }
            assert!(saw_err, "lines() never enforced MAX_LINE_BYTES");
            crate::tagged::lin_tagged_release(lines);
            crate::tagged::lin_tagged_release(src);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "upstream closed exactly once after cap error");
        }
    }

    #[test]
    fn take_adapter_limits_and_collect_concatenates() {
        unsafe {
            let cc = Arc::new(AtomicUsize::new(0));
            let src = make(vec![b"ab".to_vec(), b"cd".to_vec(), b"ef".to_vec()], cc.clone(), None);
            // take(2) over the byte chunks, then collect → "abcd" (4 bytes).
            let taken = lin_stream_take(src as *const u8, 2);
            crate::tagged::lin_tagged_release(src); // adapter holds its own ref
            let collected = lin_stream_collect(taken as *const u8);
            let arr = crate::tagged::lin_unbox_ptr(collected) as *const crate::array::LinArray;
            assert_eq!(crate::array::lin_array_length(arr), 4);
            crate::tagged::lin_tagged_release(collected);
            crate::tagged::lin_tagged_release(taken);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "collect closed the stream once");
        }
    }

    #[test]
    fn chunks_adapter_reframes_bytes() {
        unsafe {
            let cc = Arc::new(AtomicUsize::new(0));
            let src = make(vec![b"abcdefg".to_vec()], cc.clone(), None);
            let chunked = lin_stream_chunks(src as *const u8, 3);
            crate::tagged::lin_tagged_release(src);
            // 3,3,1 byte pieces.
            let mut lens = Vec::new();
            loop {
                match pull_tagged(unwrap_stream(chunked)) {
                    TaggedOutcome::Item(item) => {
                        let a = crate::tagged::lin_unbox_ptr(item) as *const crate::array::LinArray;
                        lens.push(crate::array::lin_array_length(a));
                        crate::tagged::lin_tagged_release(item);
                    }
                    TaggedOutcome::Eof => break,
                    TaggedOutcome::Err(_) => panic!("unexpected err"),
                }
            }
            assert_eq!(lens, vec![3, 3, 1]);
            crate::tagged::lin_tagged_release(chunked); // closes the adapter → upstream once
            assert_eq!(cc.load(Ordering::SeqCst), 1, "upstream closed once when adapter released");
        }
    }

    #[test]
    fn collect_propagates_upstream_error() {
        unsafe {
            let cc = Arc::new(AtomicUsize::new(0));
            // Fail on the 2nd read; collect must short-circuit to an Error object.
            let src = make(vec![b"x".to_vec()], cc.clone(), Some(1));
            let lines = lin_stream_lines(src as *const u8, 0);
            crate::tagged::lin_tagged_release(src);
            let r = lin_stream_collect(lines as *const u8);
            assert_eq!(crate::tagged::lin_get_tag(r), crate::tagged::TAG_MAP, "error object");
            crate::tagged::lin_tagged_release(r);
            crate::tagged::lin_tagged_release(lines);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "errored stream still closed once");
        }
    }

    // Stage 7: CAP_MOVE across the closure-env transfer ABI. Simulates moving a Stream capture
    // onto a worker: build an env holding the boxed stream at a CAP_MOVE slot, deep-copy the env
    // for the worker (`transfer_clone_env` — CAP_MOVE hands the pointer off VERBATIM, no clone),
    // then release BOTH the worker's env copy and the source closure's captures. The fd must close
    // EXACTLY ONCE — by the worker — and the source must NOT release it (handoff). Run under ASan
    // this proves the no-clone / no-source-release / worker-releases path has no double-free.
    #[test]
    fn cap_move_stream_closes_once_by_worker() {
        unsafe {
            use crate::transfer::{transfer_clone_env, release_env_copy, CAP_MOVE};
            let cc = Arc::new(AtomicUsize::new(0));
            let stream = make(vec![b"data".to_vec()], cc.clone(), None);

            // Build a source env: { u64 size, cap0 = stream_ptr } (one capture, 16 bytes).
            let env_size = 8 + 8usize;
            let src_env = crate::memory::lin_alloc(env_size);
            *(src_env as *mut u64) = env_size as u64;
            *(src_env.add(8) as *mut *mut u8) = stream;

            // Capture descriptor: { u32 count=1, u8 kinds[1] = CAP_MOVE }.
            let desc_layout = std::alloc::Layout::from_size_align_unchecked(8, 4);
            let desc = std::alloc::alloc(desc_layout);
            *(desc as *mut u32) = 1;
            *desc.add(4) = CAP_MOVE;

            // Worker side: deep-copy the env. CAP_MOVE → the SAME stream pointer is handed off.
            let worker_env = transfer_clone_env(src_env as *const u8, desc as *const u8);
            assert_eq!(*(worker_env.add(8) as *const *mut u8), stream, "move hands off verbatim");
            assert_eq!(cc.load(Ordering::SeqCst), 0, "no close on transfer");

            // Source closure teardown: `release_captures` skips CAP_MOVE (handed off) — model it
            // by simply NOT releasing the source slot, then free the source env shell.
            crate::memory::lin_cell_free(src_env, env_size);
            assert_eq!(cc.load(Ordering::SeqCst), 0, "source handoff does not close");

            // Worker teardown: release the env COPY — CAP_MOVE releases the stream (close once).
            release_env_copy(worker_env, desc as *const u8, env_size as u64);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "worker closes the moved stream exactly once");

            std::alloc::dealloc(desc, desc_layout);
        }
    }

    // Stage 3 net-new backend tests. These exercise the closure-free paths (drop/flatten/concat)
    // directly, plus a Rust-level closure shim for the closure-bearing ones (flatMap/reduce/find)
    // so ASan can verify the RC discipline without a full Lin pipeline. The closure shim builds a
    // minimal LinClosure-shaped object: [rc:u32 | _pad | fn_ptr | env_ptr]. We never release it
    // through `lin_closure_release` here (no real descriptor), so we free the raw allocation
    // ourselves after the terminal/adapter has dropped its `LinFn` (which calls lin_closure_release
    // — guarded to be a no-op for our shim by giving it a zeroed env so the release walk is inert).

    /// Build a single-element tagged array box holding `v` (an owned int box). Returns an owned
    /// TAG_ARRAY box. Used to fabricate flatMap inner collections.
    unsafe fn arr_of_one_int(v: i32) -> *mut u8 {
        let arr = crate::array::lin_array_alloc(1);
        let elem = crate::tagged::lin_box_int32(v);
        crate::array::lin_array_push_tagged(arr, elem);
        crate::tagged::lin_tagged_release(elem);
        crate::tagged::alloc_tagged(crate::tagged::TAG_ARRAY, arr as u64)
    }

    #[test]
    fn drop_adapter_skips_then_passes_through_and_closes_once() {
        unsafe {
            let cc = Arc::new(AtomicUsize::new(0));
            let src = make(vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec(), b"d".to_vec()], cc.clone(), None);
            let dropped = lin_stream_drop(src as *const u8, 2);
            crate::tagged::lin_tagged_release(src);
            // First two ("a","b") skipped; "c","d" pass through.
            let mut got: Vec<Vec<u8>> = Vec::new();
            loop {
                match pull_tagged(unwrap_stream(dropped)) {
                    TaggedOutcome::Item(item) => {
                        let a = crate::tagged::lin_unbox_ptr(item) as *const crate::array::LinArray;
                        let n = crate::array::lin_array_length(a) as usize;
                        let data = (*a).data as *const u8;
                        got.push(std::slice::from_raw_parts(data, n).to_vec());
                        crate::tagged::lin_tagged_release(item);
                    }
                    TaggedOutcome::Eof => break,
                    TaggedOutcome::Err(_) => panic!("unexpected err"),
                }
            }
            assert_eq!(got, vec![b"c".to_vec(), b"d".to_vec()]);
            crate::tagged::lin_tagged_release(dropped);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "drop closed upstream once");
        }
    }

    #[test]
    fn concat_two_upstreams_close_once_each() {
        unsafe {
            let cca = Arc::new(AtomicUsize::new(0));
            let ccb = Arc::new(AtomicUsize::new(0));
            let a = make(vec![b"a1".to_vec(), b"a2".to_vec()], cca.clone(), None);
            let b = make(vec![b"b1".to_vec()], ccb.clone(), None);
            let cat = lin_stream_concat(a as *const u8, b as *const u8);
            crate::tagged::lin_tagged_release(a);
            crate::tagged::lin_tagged_release(b);
            let mut count = 0;
            loop {
                match pull_tagged(unwrap_stream(cat)) {
                    TaggedOutcome::Item(item) => { count += 1; crate::tagged::lin_tagged_release(item); }
                    TaggedOutcome::Eof => break,
                    TaggedOutcome::Err(_) => panic!("unexpected err"),
                }
            }
            assert_eq!(count, 3, "yields all of a then b");
            crate::tagged::lin_tagged_release(cat);
            assert_eq!(cca.load(Ordering::SeqCst), 1, "first upstream closed exactly once");
            assert_eq!(ccb.load(Ordering::SeqCst), 1, "second upstream closed exactly once");
        }
    }

    #[test]
    fn flatten_adapter_flattens_arrays_and_closes_once() {
        unsafe {
            let cc = Arc::new(AtomicUsize::new(0));
            // Build a stream of two arrays: [10], [20]. We can't use the byte `make` helper for
            // tagged-array items, so build a custom source.
            struct ArrSource { items: Vec<*mut u8>, idx: usize, close_count: Arc<AtomicUsize> }
            unsafe impl Send for ArrSource {}
            impl StreamSource for ArrSource {
                unsafe fn read_tagged(&mut self) -> TaggedOutcome {
                    if self.idx >= self.items.len() { return TaggedOutcome::Eof; }
                    let it = self.items[self.idx];
                    self.idx += 1;
                    TaggedOutcome::Item(it)
                }
                fn close(&mut self) { self.close_count.fetch_add(1, Ordering::SeqCst); }
            }
            let src = StreamBox::new_boxed(Box::new(ArrSource {
                items: vec![arr_of_one_int(10), arr_of_one_int(20)],
                idx: 0,
                close_count: cc.clone(),
            }));
            let flat = lin_stream_flatten(src as *const u8);
            crate::tagged::lin_tagged_release(src);
            let mut vals: Vec<i32> = Vec::new();
            loop {
                match pull_tagged(unwrap_stream(flat)) {
                    TaggedOutcome::Item(item) => {
                        vals.push(crate::tagged::lin_unbox_int32(item) as i32);
                        crate::tagged::lin_tagged_release(item);
                    }
                    TaggedOutcome::Eof => break,
                    TaggedOutcome::Err(_) => panic!("unexpected err"),
                }
            }
            assert_eq!(vals, vec![10, 20]);
            crate::tagged::lin_tagged_release(flat);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "flatten closed upstream once");
        }
    }

    #[test]
    fn find_returns_first_match_and_closes_once() {
        unsafe {
            // A predicate closure that returns true iff the item's byte-array length == 2. We build
            // a minimal closure that ignores its env and checks the arg. Implement via a real
            // extern fn matching the (env, arg) -> ret ABI.
            unsafe extern "C-unwind" fn pred_len2(_env: *mut u8, arg: *mut u8) -> *mut u8 {
                let a = crate::tagged::lin_unbox_ptr(arg) as *const crate::array::LinArray;
                let n = crate::array::lin_array_length(a);
                crate::tagged::lin_box_bool((n == 2) as u8)
            }
            let cc = Arc::new(AtomicUsize::new(0));
            let src = make(vec![b"a".to_vec(), b"bb".to_vec(), b"c".to_vec()], cc.clone(), None);
            let closure = make_test_closure(pred_len2 as *mut u8);
            let found = lin_stream_find(src as *const u8, closure);
            // "bb" has length 2 → the found item is a 2-byte UInt8[].
            let a = crate::tagged::lin_unbox_ptr(found) as *const crate::array::LinArray;
            assert_eq!(crate::array::lin_array_length(a), 2);
            crate::tagged::lin_tagged_release(found);
            crate::tagged::lin_tagged_release(src);
            free_test_closure(closure);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "find closed the stream once");
        }
    }

    #[test]
    fn reduce_folds_and_closes_once() {
        unsafe {
            // acc + item.length  (acc is an int box, item a UInt8[]).
            unsafe extern "C-unwind" fn add_len(_env: *mut u8, acc: *mut u8, item: *mut u8) -> *mut u8 {
                let a = crate::tagged::lin_unbox_int32(acc);
                let arr = crate::tagged::lin_unbox_ptr(item) as *const crate::array::LinArray;
                let n = crate::array::lin_array_length(arr);
                crate::tagged::lin_box_int32((a as i64 + n) as i32)
            }
            let cc = Arc::new(AtomicUsize::new(0));
            let src = make(vec![b"a".to_vec(), b"bb".to_vec(), b"ccc".to_vec()], cc.clone(), None);
            let closure = make_test_closure(add_len as *mut u8);
            let init = crate::tagged::lin_box_int32(0);
            let total = lin_stream_reduce(src as *const u8, init, closure);
            assert_eq!(crate::tagged::lin_unbox_int32(total), 6, "1+2+3 byte lengths");
            crate::tagged::lin_tagged_release(total);
            crate::tagged::lin_tagged_release(src);
            free_test_closure(closure);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "reduce closed the stream once");
        }
    }

    #[test]
    fn flat_map_flattens_and_closes_once() {
        unsafe {
            // f(item) → a single-element array [item.length].
            unsafe extern "C-unwind" fn to_len_arr(_env: *mut u8, item: *mut u8) -> *mut u8 {
                let arr = crate::tagged::lin_unbox_ptr(item) as *const crate::array::LinArray;
                let n = crate::array::lin_array_length(arr) as i32;
                // build [n]
                let out = crate::array::lin_array_alloc(1);
                let e = crate::tagged::lin_box_int32(n);
                crate::array::lin_array_push_tagged(out, e);
                crate::tagged::lin_tagged_release(e);
                crate::tagged::alloc_tagged(crate::tagged::TAG_ARRAY, out as u64)
            }
            let cc = Arc::new(AtomicUsize::new(0));
            let src = make(vec![b"a".to_vec(), b"bb".to_vec()], cc.clone(), None);
            let closure = make_test_closure(to_len_arr as *mut u8);
            let fm = lin_stream_flat_map(src as *const u8, closure);
            crate::tagged::lin_tagged_release(src);
            let mut vals: Vec<i32> = Vec::new();
            loop {
                match pull_tagged(unwrap_stream(fm)) {
                    TaggedOutcome::Item(item) => {
                        vals.push(crate::tagged::lin_unbox_int32(item) as i32);
                        crate::tagged::lin_tagged_release(item);
                    }
                    TaggedOutcome::Eof => break,
                    TaggedOutcome::Err(_) => panic!("unexpected err"),
                }
            }
            assert_eq!(vals, vec![1, 2], "byte lengths flattened");
            crate::tagged::lin_tagged_release(fm);
            free_test_closure(closure);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "flatMap closed upstream once");
        }
    }

    /// Build a minimal LinClosure-shaped object [rc:u32 | _pad | fn_ptr | env_ptr] with rc=1 and a
    /// null env. The layout matches `LinFn`'s field offsets (8 = fn_ptr, 16 = env_ptr). We bump rc
    /// to a high value so the adapter's `LinFn::Drop` → `lin_closure_release` never reaches 0 and
    /// thus never walks a (nonexistent) capture descriptor. The test frees it manually afterwards.
    unsafe fn make_test_closure(fn_ptr: *mut u8) -> *mut u8 {
        let size = 24usize;
        let p = crate::memory::lin_alloc(size);
        // rc = large so the runtime release never frees/walks it.
        *(p as *mut u32) = 1000;
        *(p.add(8) as *mut *mut u8) = fn_ptr;
        *(p.add(16) as *mut *mut u8) = std::ptr::null_mut();
        p
    }
    unsafe fn free_test_closure(p: *mut u8) {
        crate::memory::lin_cell_free(p, 24);
    }

    // std/compress adapter tests. Round-trip through the streaming codecs (deflate→inflate,
    // gzip→gunzip) and assert the codec adapter closes its upstream EXACTLY ONCE — the
    // ASan-relevant close-propagation check the brief mandates.

    /// Drain every item of a codec adapter stream into one Vec<u8>, asserting no in-band error.
    unsafe fn drain_codec_bytes(s: *mut u8) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            match pull_tagged(unwrap_stream(s)) {
                TaggedOutcome::Item(item) => {
                    let a = crate::tagged::lin_unbox_ptr(item) as *const crate::array::LinArray;
                    let n = crate::array::lin_array_length(a) as usize;
                    let data = (*a).data as *const u8;
                    out.extend_from_slice(std::slice::from_raw_parts(data, n));
                    crate::tagged::lin_tagged_release(item);
                }
                TaggedOutcome::Eof => break,
                TaggedOutcome::Err(m) => panic!("codec adapter erred: {m}"),
            }
        }
        out
    }

    #[test]
    fn deflate_then_inflate_round_trips_and_closes_once() {
        unsafe {
            // A payload spread across several upstream chunks (and large enough to exercise the
            // multi-pass scratch loop) round-trips byte-for-byte through deflate→inflate.
            let mut payload = Vec::new();
            for i in 0..50_000u32 { payload.push((i % 251) as u8); }
            let chunks: Vec<Vec<u8>> = payload.chunks(4096).map(|c| c.to_vec()).collect();

            // deflate the source.
            let cc_src = Arc::new(AtomicUsize::new(0));
            let src = make(chunks, cc_src.clone(), None);
            let deflated_stream = lin_stream_deflate(src as *const u8);
            crate::tagged::lin_tagged_release(src); // adapter holds its own ref
            let compressed = drain_codec_bytes(deflated_stream);
            crate::tagged::lin_tagged_release(deflated_stream);
            assert_eq!(cc_src.load(Ordering::SeqCst), 1, "deflate closed upstream exactly once");
            assert!(compressed.len() < payload.len(), "compressed smaller than input");

            // inflate the compressed bytes (fed in small chunks to exercise carry-over).
            let cc_cmp = Arc::new(AtomicUsize::new(0));
            let cmp_chunks: Vec<Vec<u8>> = compressed.chunks(100).map(|c| c.to_vec()).collect();
            let cmp_src = make(cmp_chunks, cc_cmp.clone(), None);
            let inflated_stream = lin_stream_inflate(cmp_src as *const u8);
            crate::tagged::lin_tagged_release(cmp_src);
            let recovered = drain_codec_bytes(inflated_stream);
            crate::tagged::lin_tagged_release(inflated_stream);
            assert_eq!(cc_cmp.load(Ordering::SeqCst), 1, "inflate closed upstream exactly once");
            assert_eq!(recovered, payload, "deflate→inflate recovers the input");
        }
    }

    #[test]
    fn gzip_then_gunzip_round_trips_and_closes_once() {
        unsafe {
            let payload = b"the quick brown fox jumps over the lazy dog\n".repeat(500);
            let chunks: Vec<Vec<u8>> = payload.chunks(1000).map(|c| c.to_vec()).collect();

            let cc_src = Arc::new(AtomicUsize::new(0));
            let src = make(chunks, cc_src.clone(), None);
            let gz_stream = lin_stream_gzip(src as *const u8);
            crate::tagged::lin_tagged_release(src);
            let compressed = drain_codec_bytes(gz_stream);
            crate::tagged::lin_tagged_release(gz_stream);
            assert_eq!(cc_src.load(Ordering::SeqCst), 1, "gzip closed upstream exactly once");
            // gzip magic bytes 0x1f 0x8b.
            assert_eq!(&compressed[..2], &[0x1f, 0x8b], "gzip container magic");

            let cc_cmp = Arc::new(AtomicUsize::new(0));
            let cmp_chunks: Vec<Vec<u8>> = compressed.chunks(64).map(|c| c.to_vec()).collect();
            let cmp_src = make(cmp_chunks, cc_cmp.clone(), None);
            let gunzip_stream = lin_stream_gunzip(cmp_src as *const u8);
            crate::tagged::lin_tagged_release(cmp_src);
            let recovered = drain_codec_bytes(gunzip_stream);
            crate::tagged::lin_tagged_release(gunzip_stream);
            assert_eq!(cc_cmp.load(Ordering::SeqCst), 1, "gunzip closed upstream exactly once");
            assert_eq!(recovered, payload, "gzip→gunzip recovers the input");
        }
    }

    #[test]
    fn gunzip_of_garbage_errors_in_band_and_closes_once() {
        unsafe {
            // Feeding non-gzip bytes into gunzip must surface an in-band Err (not a panic/abort),
            // and the upstream must still close exactly once.
            let cc = Arc::new(AtomicUsize::new(0));
            let src = make(vec![vec![0u8; 64], vec![1u8; 64]], cc.clone(), None);
            let gunzip_stream = lin_stream_gunzip(src as *const u8);
            crate::tagged::lin_tagged_release(src);
            let mut saw_err = false;
            loop {
                match pull_tagged(unwrap_stream(gunzip_stream)) {
                    TaggedOutcome::Err(_) => { saw_err = true; break; }
                    TaggedOutcome::Eof => break,
                    TaggedOutcome::Item(item) => crate::tagged::lin_tagged_release(item),
                }
            }
            assert!(saw_err, "gunzip of garbage surfaces an in-band Err");
            crate::tagged::lin_tagged_release(gunzip_stream);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "errored codec still closed upstream once");
        }
    }

    #[test]
    fn drain_default_forces_and_closes() {
        unsafe {
            let cc = Arc::new(AtomicUsize::new(0));
            let src = make(vec![b"aa".to_vec(), b"bb".to_vec()], cc.clone(), None);
            // A non-sink stream drained → pull-and-discard → Null, closed once.
            let r = lin_stream_drain(src as *const u8);
            assert!(r.is_null(), "drain of a successful non-sink stream → Null");
            crate::tagged::lin_tagged_release(src);
            assert_eq!(cc.load(Ordering::SeqCst), 1);
        }
    }

    /// Build a boxed `path` String tagged value for the sink entry points.
    unsafe fn path_tagged(path: &str) -> *mut u8 {
        let s = crate::string::lin_string_from_bytes(path.as_ptr(), path.len() as u32);
        crate::tagged::alloc_tagged(crate::tagged::TAG_STR, s as u64)
    }

    #[test]
    fn write_stream_raw_concatenates_bytes_verbatim_and_closes_once() {
        unsafe {
            // The RAW sink must write each UInt8[] item's bytes back-to-back with NO separator —
            // the property that makes binary output (e.g. gzip chunks) round-trip uncorrupted.
            let path = "/tmp/lin_stream_write_raw_test.bin";
            let _ = std::fs::remove_file(path);
            let cc = Arc::new(AtomicUsize::new(0));
            let src = make(vec![vec![0u8, 1, 2, 3], vec![4u8, 5, 6, 7]], cc.clone(), None);
            let p = path_tagged(path);
            let sink = lin_stream_write(src as *const u8, p as *const u8);
            crate::tagged::lin_tagged_release(src); // sink holds its own ref
            // Drive the sink → Null on success.
            let r = lin_stream_drain(sink as *const u8);
            assert!(r.is_null(), "raw write of a clean stream → Null");
            crate::tagged::lin_tagged_release(sink);
            crate::tagged::lin_tagged_release(p);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "raw sink closed upstream exactly once");

            let written = std::fs::read(path).unwrap();
            assert_eq!(written, vec![0u8, 1, 2, 3, 4, 5, 6, 7],
                "raw sink writes the exact concatenation with no \\n separators");
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn write_lines_separates_items_with_newlines_and_closes_once() {
        unsafe {
            // The line-oriented sink writes each item's bytes followed by a single '\n'.
            let path = "/tmp/lin_stream_write_lines_test.txt";
            let _ = std::fs::remove_file(path);
            let cc = Arc::new(AtomicUsize::new(0));
            let src = make(vec![b"one".to_vec(), b"two".to_vec(), b"three".to_vec()], cc.clone(), None);
            let p = path_tagged(path);
            let sink = lin_stream_write_lines(src as *const u8, p as *const u8);
            crate::tagged::lin_tagged_release(src);
            let r = lin_stream_drain(sink as *const u8);
            assert!(r.is_null(), "line write of a clean stream → Null");
            crate::tagged::lin_tagged_release(sink);
            crate::tagged::lin_tagged_release(p);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "line sink closed upstream exactly once");

            let written = std::fs::read(path).unwrap();
            assert_eq!(written, b"one\ntwo\nthree\n",
                "line sink writes each item followed by a newline");
            let _ = std::fs::remove_file(path);
        }
    }

    // =====================================================================================
    // `std/archive` — tar splitting tests. These drive the REAL `lin_stream_untar` /
    // `lin_stream_manifest` / `lin_stream_files` entry points (the spike's struct/parse/BoundedSource
    // are now production code above, not redefined here). Each test asserts the parent stream closes
    // EXACTLY ONCE and that an undrained body is skipped so the next header parses correctly.
    // =====================================================================================

    /// Build a tiny in-memory tar: a 512-byte header per entry (name/size/typeflag) + the padded
    /// body. ustar checksum is not validated by our parser, so we leave it blank (NUL).
    fn tar_header(name: &str, size: usize, typeflag: u8) -> Vec<u8> {
        let mut h = vec![0u8; 512];
        let nb = name.as_bytes();
        h[0..nb.len()].copy_from_slice(nb);
        // size as octal ASCII in 11 chars + NUL at offset 124.
        let octal = format!("{:011o}", size);
        h[124..124 + 11].copy_from_slice(octal.as_bytes());
        h[135] = 0;
        h[156] = typeflag;
        h
    }
    fn tar_entry(name: &str, body: &[u8]) -> Vec<u8> {
        let mut e = tar_header(name, body.len(), b'0');
        e.extend_from_slice(body);
        let pad = padded_len(body.len()) - body.len();
        e.extend(std::iter::repeat(0u8).take(pad));
        e
    }

    /// A 2-arg test closure shim `(env, arg0, arg1) -> ret` over `make_test_closure`'s layout.
    unsafe fn make_test_closure2(fn_ptr: *mut u8) -> *mut u8 {
        make_test_closure(fn_ptr)
    }

    #[test]
    fn untar_drains_skips_and_closes_once() {
        unsafe {
            // A 2-arg body: drains entry 0 fully (recording its bytes via a global), IGNORES entry 1.
            // We can't capture state in an extern fn, so use a thread-local accumulator.
            thread_local! {
                static DRAINED: std::cell::RefCell<Vec<u8>> = std::cell::RefCell::new(Vec::new());
                static SEEN_NAMES: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
            }
            unsafe extern "C-unwind" fn body(_env: *mut u8, meta: *mut u8, data: *mut u8) -> *mut u8 {
                // Record the entry name from the meta map.
                let map = crate::tagged::lin_unbox_ptr(meta) as *const crate::map::LinMap;
                let v = crate::map::lin_map_get_bytes(map, b"name".as_ptr(), 4);
                if !v.is_null() {
                    let name_s = crate::fs::resolve_lin_str(v as *const u8).unwrap_or_default();
                    SEEN_NAMES.with(|s| s.borrow_mut().push(name_s.clone()));
                    // Drain entry "big.bin" fully; ignore everything else (proves skip-undrained).
                    if name_s == "big.bin" {
                        loop {
                            match pull_tagged(unwrap_stream(data)) {
                                TaggedOutcome::Item(item) => {
                                    let a = crate::tagged::lin_unbox_ptr(item) as *const crate::array::LinArray;
                                    let n = crate::array::lin_array_length(a) as usize;
                                    let d = (*a).data as *const u8;
                                    let slice = std::slice::from_raw_parts(d, n).to_vec();
                                    DRAINED.with(|x| x.borrow_mut().extend_from_slice(&slice));
                                    crate::tagged::lin_tagged_release(item);
                                }
                                TaggedOutcome::Eof => break,
                                TaggedOutcome::Err(_) => break,
                            }
                        }
                    }
                }
                std::ptr::null_mut()
            }

            let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
            let small = b"hello, small entry".to_vec();
            let mut archive = Vec::new();
            archive.extend(tar_entry("big.bin", &big));
            archive.extend(tar_entry("small.txt", &small));
            archive.extend(vec![0u8; 1024]); // two zero blocks = end-of-archive

            let cc = Arc::new(AtomicUsize::new(0));
            let chunks: Vec<Vec<u8>> = archive.chunks(3000).map(|c| c.to_vec()).collect();
            let parent = make(chunks, cc.clone(), None);

            let closure = make_test_closure2(body as *mut u8);
            let r = lin_stream_untar(parent as *const u8, closure);
            assert!(r.is_null(), "clean archive → Null");
            crate::tagged::lin_tagged_release(parent); // drop the caller's ref (untar held its own)
            free_test_closure(closure);

            DRAINED.with(|d| assert_eq!(*d.borrow(), big,
                "(1) fully-drained sub-stream yields exactly the entry's bytes"));
            SEEN_NAMES.with(|s| assert_eq!(*s.borrow(), vec!["big.bin".to_string(), "small.txt".to_string()],
                "(2) the IGNORED entry's body was skipped so the next header parsed correctly"));
            assert_eq!(cc.load(Ordering::SeqCst), 1, "(3) parent upstream closed exactly once");
        }
    }

    #[test]
    fn manifest_lists_entries_and_closes_once() {
        unsafe {
            let mut archive = Vec::new();
            archive.extend(tar_entry("a.txt", b"alpha"));
            archive.extend(tar_entry("b.txt", b"bravo bravo"));
            archive.extend(vec![0u8; 1024]);

            let cc = Arc::new(AtomicUsize::new(0));
            let chunks: Vec<Vec<u8>> = archive.chunks(700).map(|c| c.to_vec()).collect();
            let parent = make(chunks, cc.clone(), None);

            let m = lin_stream_manifest(parent as *const u8);
            crate::tagged::lin_tagged_release(parent);

            let mut names = Vec::new();
            let mut sizes = Vec::new();
            loop {
                match pull_tagged(unwrap_stream(m)) {
                    TaggedOutcome::Item(item) => {
                        let map = crate::tagged::lin_unbox_ptr(item) as *const crate::map::LinMap;
                        let vn = crate::map::lin_map_get_bytes(map, b"name".as_ptr(), 4);
                        names.push(crate::fs::resolve_lin_str(vn as *const u8).unwrap());
                        let vs = crate::map::lin_map_get_bytes(map, b"size".as_ptr(), 4);
                        sizes.push((*vs).payload as i64);
                        crate::tagged::lin_tagged_release(item);
                    }
                    TaggedOutcome::Eof => break,
                    TaggedOutcome::Err(m) => panic!("manifest erred: {m}"),
                }
            }
            assert_eq!(names, vec!["a.txt".to_string(), "b.txt".to_string()]);
            assert_eq!(sizes, vec![5, 11]);
            crate::tagged::lin_tagged_release(m);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "manifest closed the parent once");
        }
    }

    #[test]
    fn files_recovers_bodies_and_closes_once() {
        unsafe {
            let mut archive = Vec::new();
            archive.extend(tar_entry("a.txt", b"alpha"));
            archive.extend(tar_entry("b.txt", b"bravo bravo"));
            archive.extend(vec![0u8; 1024]);

            let cc = Arc::new(AtomicUsize::new(0));
            let chunks: Vec<Vec<u8>> = archive.chunks(64).map(|c| c.to_vec()).collect();
            let parent = make(chunks, cc.clone(), None);

            let fs = lin_stream_files(parent as *const u8);
            crate::tagged::lin_tagged_release(parent);

            let mut bodies: Vec<Vec<u8>> = Vec::new();
            loop {
                match pull_tagged(unwrap_stream(fs)) {
                    TaggedOutcome::Item(item) => {
                        let map = crate::tagged::lin_unbox_ptr(item) as *const crate::map::LinMap;
                        let vd = crate::map::lin_map_get_bytes(map, b"data".as_ptr(), 4);
                        let a = (*vd).payload as *const crate::array::LinArray;
                        let n = crate::array::lin_array_length(a) as usize;
                        let d = (*a).data as *const u8;
                        bodies.push(std::slice::from_raw_parts(d, n).to_vec());
                        crate::tagged::lin_tagged_release(item);
                    }
                    TaggedOutcome::Eof => break,
                    TaggedOutcome::Err(m) => panic!("files erred: {m}"),
                }
            }
            assert_eq!(bodies, vec![b"alpha".to_vec(), b"bravo bravo".to_vec()]);
            crate::tagged::lin_tagged_release(fs);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "files closed the parent once");
        }
    }

    // -------------------------------------------------------------------------
    // TarEntry (entries / header / body) unit tests
    // -------------------------------------------------------------------------

    /// Read all bytes from a body stream (a `Stream<UInt8[]>` box), returning them collected.
    /// Also returns the first error message if any read yielded Err.
    unsafe fn drain_body(body_box: *mut u8) -> (Vec<u8>, Option<String>) {
        let mut bytes = Vec::new();
        let mut err_msg: Option<String> = None;
        loop {
            match pull_tagged(unwrap_stream(body_box)) {
                TaggedOutcome::Item(item) => {
                    use crate::tagged::{lin_get_tag, lin_unbox_ptr, TAG_ARRAY, lin_tagged_release};
                    if lin_get_tag(item) == TAG_ARRAY {
                        let arr = lin_unbox_ptr(item) as *const crate::array::LinArray;
                        let n = crate::array::lin_array_length(arr) as usize;
                        let data = (*arr).data as *const u8;
                        let elem_tag = (*arr).elem_tag;
                        if elem_tag == crate::tagged::TAG_UINT8 || elem_tag == crate::tagged::TAG_INT8 {
                            bytes.extend_from_slice(std::slice::from_raw_parts(data, n));
                        }
                        lin_tagged_release(item);
                    } else {
                        crate::tagged::lin_tagged_release(item);
                    }
                }
                TaggedOutcome::Eof => break,
                TaggedOutcome::Err(m) => {
                    err_msg = Some(m);
                    break;
                }
            }
        }
        (bytes, err_msg)
    }

    /// Read the `name` field from a `lin_tar_header` result map.
    /// `lin_tar_header` returns a raw `LinMap*` (Phase 2: non-sealed objects are LinMap), NOT a tagged box — use directly.
    unsafe fn header_name(h: *mut u8) -> String {
        let map = h as *const crate::map::LinMap;
        let k = crate::fs::make_string("name");
        let v = crate::map::lin_map_get(map, k);
        crate::string::lin_string_release(k);
        if v.is_null() { return String::new(); }
        // v is a *const TaggedVal (TAG_STR, LinString*); resolve_lin_str accepts TaggedVal*.
        crate::fs::resolve_lin_str(v as *const u8).unwrap_or_default()
    }

    /// Read the `size` field from a `lin_tar_header` result map as i64.
    /// `lin_tar_header` returns a raw `LinMap*` (Phase 2: non-sealed objects are LinMap), NOT a tagged box — use directly.
    unsafe fn header_size(h: *mut u8) -> i64 {
        let map = h as *const crate::map::LinMap;
        let k = crate::fs::make_string("size");
        let v = crate::map::lin_map_get(map, k);
        crate::string::lin_string_release(k);
        if v.is_null() { return 0; }
        (*v).payload as i64
    }

    /// Release a `lin_tar_header` result (a raw `LinMap*`, not a TaggedVal box).
    unsafe fn release_header(h: *mut u8) {
        crate::map::lin_map_release(h as *mut crate::map::LinMap);
    }

    // (a) entries() yields all handles with correct headers.
    #[test]
    fn entries_yields_all_handles_with_correct_headers() {
        unsafe {
            let mut archive = Vec::new();
            archive.extend(tar_entry("a.txt", b"alpha"));
            archive.extend(tar_entry("b.txt", b"bravo bravo"));
            archive.extend(vec![0u8; 1024]);

            let cc = Arc::new(AtomicUsize::new(0));
            let chunks: Vec<Vec<u8>> = archive.chunks(256).map(|c| c.to_vec()).collect();
            let parent = make(chunks, cc.clone(), None);

            let entries_stream = lin_stream_tar_entries(parent as *const u8);
            crate::tagged::lin_tagged_release(parent);

            let mut names = Vec::new();
            let mut sizes = Vec::new();
            loop {
                match pull_tagged(unwrap_stream(entries_stream)) {
                    TaggedOutcome::Item(handle) => {
                        let h = lin_tar_header(handle);
                        names.push(header_name(h));
                        sizes.push(header_size(h));
                        release_header(h);
                        // Drain the body so the next entry can be parsed.
                        let body = lin_tar_body(handle);
                        let _ = drain_body(body);
                        crate::tagged::lin_tagged_release(body);
                        crate::tagged::lin_tagged_release(handle);
                    }
                    TaggedOutcome::Eof => break,
                    TaggedOutcome::Err(m) => panic!("entries erred: {m}"),
                }
            }
            crate::tagged::lin_tagged_release(entries_stream);

            assert_eq!(names, vec!["a.txt".to_string(), "b.txt".to_string()],
                "(a) entry names match");
            assert_eq!(sizes, vec![5i64, 11i64], "(a) entry sizes match");
        }
    }

    // (b) body() streams correct bytes in bounded chunks.
    #[test]
    fn body_streams_correct_bytes() {
        unsafe {
            let content: Vec<u8> = (0u8..200).collect();
            let mut archive = Vec::new();
            archive.extend(tar_entry("data.bin", &content));
            archive.extend(vec![0u8; 1024]);

            let cc = Arc::new(AtomicUsize::new(0));
            let chunks: Vec<Vec<u8>> = archive.chunks(64).map(|c| c.to_vec()).collect();
            let parent = make(chunks, cc.clone(), None);

            let entries_stream = lin_stream_tar_entries(parent as *const u8);
            crate::tagged::lin_tagged_release(parent);

            // Pull first (and only) entry.
            let handle = match pull_tagged(unwrap_stream(entries_stream)) {
                TaggedOutcome::Item(h) => h,
                other => panic!("expected entry, got {:?}", std::mem::discriminant(&other)),
            };

            let body = lin_tar_body(handle);
            let (got_bytes, err) = drain_body(body);
            assert!(err.is_none(), "(b) no error draining body");
            assert_eq!(got_bytes, content, "(b) body bytes match");
            crate::tagged::lin_tagged_release(body);
            crate::tagged::lin_tagged_release(handle);
            crate::tagged::lin_tagged_release(entries_stream);
        }
    }

    // (c) Advancing the archive expires the previous entry's body — first read returns in-band Error.
    #[test]
    fn advancing_expires_previous_entry_body() {
        unsafe {
            let mut archive = Vec::new();
            archive.extend(tar_entry("a.txt", b"hello"));
            archive.extend(tar_entry("b.txt", b"world"));
            archive.extend(vec![0u8; 1024]);

            let cc = Arc::new(AtomicUsize::new(0));
            let chunks: Vec<Vec<u8>> = archive.chunks(2048).map(|c| c.to_vec()).collect();
            let parent = make(chunks, cc.clone(), None);

            let entries_stream = lin_stream_tar_entries(parent as *const u8);
            crate::tagged::lin_tagged_release(parent);

            // Pull entry 0 but do NOT drain its body yet.
            let h0 = match pull_tagged(unwrap_stream(entries_stream)) {
                TaggedOutcome::Item(h) => h,
                other => panic!("expected entry 0, got {:?}", std::mem::discriminant(&other)),
            };

            // Pull entry 1 — this advances the archive, expiring entry 0's body.
            let h1 = match pull_tagged(unwrap_stream(entries_stream)) {
                TaggedOutcome::Item(h) => h,
                other => panic!("expected entry 1, got {:?}", std::mem::discriminant(&other)),
            };

            // Now call body() on the expired h0.
            let body0 = lin_tar_body(h0);
            let (_bytes, err) = drain_body(body0);
            assert!(err.is_some(), "(c) expired body must yield in-band Error");
            let msg = err.unwrap();
            assert!(msg.contains("expired"), "(c) error message mentions 'expired': {msg}");
            crate::tagged::lin_tagged_release(body0);

            // h1's body is still valid; drain it cleanly.
            let body1 = lin_tar_body(h1);
            let (got, err1) = drain_body(body1);
            assert!(err1.is_none(), "(c) entry 1 body is still valid");
            assert_eq!(got, b"world".to_vec(), "(c) entry 1 body correct");
            crate::tagged::lin_tagged_release(body1);

            crate::tagged::lin_tagged_release(h0);
            crate::tagged::lin_tagged_release(h1);
            crate::tagged::lin_tagged_release(entries_stream);
        }
    }

    // (d) Second body() call → in-band Error on first read.
    #[test]
    fn second_body_call_yields_error() {
        unsafe {
            let mut archive = Vec::new();
            archive.extend(tar_entry("once.txt", b"one-shot"));
            archive.extend(vec![0u8; 1024]);

            let cc = Arc::new(AtomicUsize::new(0));
            let parent = make(vec![archive], cc.clone(), None);

            let entries_stream = lin_stream_tar_entries(parent as *const u8);
            crate::tagged::lin_tagged_release(parent);

            let handle = match pull_tagged(unwrap_stream(entries_stream)) {
                TaggedOutcome::Item(h) => h,
                other => panic!("expected entry, got {:?}", std::mem::discriminant(&other)),
            };

            // First body() call — drain it fully.
            let body1 = lin_tar_body(handle);
            let (got, err) = drain_body(body1);
            assert!(err.is_none(), "(d) first body drain ok");
            assert_eq!(got, b"one-shot".to_vec(), "(d) first body bytes ok");
            crate::tagged::lin_tagged_release(body1);

            // Second body() call — must yield an in-band Error.
            let body2 = lin_tar_body(handle);
            let (_bytes, err2) = drain_body(body2);
            assert!(err2.is_some(), "(d) second body() call must yield Error");
            assert!(err2.unwrap().contains("already"), "(d) error mentions 'already'");
            crate::tagged::lin_tagged_release(body2);

            crate::tagged::lin_tagged_release(handle);
            crate::tagged::lin_tagged_release(entries_stream);
        }
    }

    // (e) Header is valid after the entry has been expired (archive advanced past it).
    #[test]
    fn header_valid_after_expiry() {
        unsafe {
            let mut archive = Vec::new();
            archive.extend(tar_entry("expired.txt", b"data"));
            archive.extend(tar_entry("next.txt", b"x"));
            archive.extend(vec![0u8; 1024]);

            let cc = Arc::new(AtomicUsize::new(0));
            let parent = make(vec![archive], cc.clone(), None);

            let entries_stream = lin_stream_tar_entries(parent as *const u8);
            crate::tagged::lin_tagged_release(parent);

            // Pull entry 0, retain the handle.
            let h0 = match pull_tagged(unwrap_stream(entries_stream)) {
                TaggedOutcome::Item(h) => h,
                other => panic!("expected entry 0, got {:?}", std::mem::discriminant(&other)),
            };

            // Pull entry 1 — expires h0's body.
            let h1 = match pull_tagged(unwrap_stream(entries_stream)) {
                TaggedOutcome::Item(h) => h,
                other => panic!("expected entry 1, got {:?}", std::mem::discriminant(&other)),
            };

            // Header of h0 must still be valid even though it is expired.
            let header0 = lin_tar_header(h0);
            assert_eq!(header_name(header0), "expired.txt", "(e) expired header name ok");
            assert_eq!(header_size(header0), 4i64, "(e) expired header size ok");
            release_header(header0);

            // h1's header is also valid.
            let header1 = lin_tar_header(h1);
            assert_eq!(header_name(header1), "next.txt", "(e) live header name ok");
            release_header(header1);

            // Drain h1 body so the archive closes cleanly.
            let body1 = lin_tar_body(h1);
            let _ = drain_body(body1);
            crate::tagged::lin_tagged_release(body1);

            crate::tagged::lin_tagged_release(h0);
            crate::tagged::lin_tagged_release(h1);
            crate::tagged::lin_tagged_release(entries_stream);
        }
    }

    // (f) Unread bodies are auto-skipped: entry N+1 parses correctly even when entry N's body
    // was never read.
    #[test]
    fn unread_body_is_auto_skipped() {
        unsafe {
            let mut archive = Vec::new();
            archive.extend(tar_entry("skip.txt", b"this body is never read"));
            archive.extend(tar_entry("next.txt", b"read me"));
            archive.extend(vec![0u8; 1024]);

            let cc = Arc::new(AtomicUsize::new(0));
            let chunks: Vec<Vec<u8>> = archive.chunks(512).map(|c| c.to_vec()).collect();
            let parent = make(chunks, cc.clone(), None);

            let entries_stream = lin_stream_tar_entries(parent as *const u8);
            crate::tagged::lin_tagged_release(parent);

            // Pull entry 0 but do NOT call body() at all.
            let h0 = match pull_tagged(unwrap_stream(entries_stream)) {
                TaggedOutcome::Item(h) => h,
                other => panic!("expected entry 0, got {:?}", std::mem::discriminant(&other)),
            };
            crate::tagged::lin_tagged_release(h0); // drop without reading body

            // The entries source must auto-skip to find entry 1.
            let h1 = match pull_tagged(unwrap_stream(entries_stream)) {
                TaggedOutcome::Item(h) => h,
                other => panic!("expected entry 1 after skip, got {:?}", std::mem::discriminant(&other)),
            };

            let body1 = lin_tar_body(h1);
            let (got, err) = drain_body(body1);
            assert!(err.is_none(), "(f) second entry body ok after skip");
            assert_eq!(got, b"read me".to_vec(), "(f) second entry bytes correct");
            crate::tagged::lin_tagged_release(body1);
            crate::tagged::lin_tagged_release(h1);
            crate::tagged::lin_tagged_release(entries_stream);
        }
    }

    // (g) Find-style early stop: close the entries stream after entry 0, the body of entry 0
    // is still readable (the shared state is kept alive by the retained handle).
    #[test]
    fn find_style_early_stop_body_still_readable() {
        unsafe {
            let mut archive = Vec::new();
            archive.extend(tar_entry("target.txt", b"found it!"));
            archive.extend(tar_entry("other.txt", b"ignored"));
            archive.extend(vec![0u8; 1024]);

            let cc = Arc::new(AtomicUsize::new(0));
            let chunks: Vec<Vec<u8>> = archive.chunks(2048).map(|c| c.to_vec()).collect();
            let parent = make(chunks, cc.clone(), None);

            let entries_stream = lin_stream_tar_entries(parent as *const u8);
            crate::tagged::lin_tagged_release(parent);

            // Pull the first entry.
            let h0 = match pull_tagged(unwrap_stream(entries_stream)) {
                TaggedOutcome::Item(h) => h,
                other => panic!("expected entry, got {:?}", std::mem::discriminant(&other)),
            };

            // "Stop" the stream: release the entries_stream box without draining it.
            // The shared state is still alive because h0 holds an Arc clone.
            crate::tagged::lin_tagged_release(entries_stream);

            // h0's body must still be readable.
            let body0 = lin_tar_body(h0);
            let (got, err) = drain_body(body0);
            assert!(err.is_none(), "(g) find-stop body still readable: no error");
            assert_eq!(got, b"found it!".to_vec(), "(g) find-stop body bytes correct");
            crate::tagged::lin_tagged_release(body0);

            // Dropping h0 now releases the last Arc → parent upstream is closed.
            let before = cc.load(Ordering::SeqCst);
            crate::tagged::lin_tagged_release(h0);
            let after = cc.load(Ordering::SeqCst);
            assert_eq!(after - before, 1, "(g) parent closed exactly once after last handle drop");
        }
    }

    // (h) Upstream in-band error propagates through entries stream.
    #[test]
    fn upstream_error_propagates_through_entries() {
        unsafe {
            let mut archive = Vec::new();
            // Build a 2-entry archive, then tell the parent to inject an error on the 3rd chunk.
            archive.extend(tar_entry("a.txt", b"ok"));
            archive.extend(tar_entry("b.txt", b"also ok"));
            archive.extend(vec![0u8; 1024]);

            let cc = Arc::new(AtomicUsize::new(0));
            // Slice archive into very small chunks; inject an error after just 1 chunk read so
            // the archive will be interrupted part-way through parsing entry 0.
            let partial: Vec<Vec<u8>> = archive.chunks(64).map(|c| c.to_vec()).collect();
            let parent = make(partial, cc.clone(), Some(2)); // fail after 2 successful chunks

            let entries_stream = lin_stream_tar_entries(parent as *const u8);
            crate::tagged::lin_tagged_release(parent);

            // Try to pull entries until we see Err or Eof.
            let mut saw_err = false;
            for _ in 0..10 {
                match pull_tagged(unwrap_stream(entries_stream)) {
                    TaggedOutcome::Item(h) => {
                        // Try to drain body — might also hit the injected error.
                        let body = lin_tar_body(h);
                        let (_bytes, err) = drain_body(body);
                        crate::tagged::lin_tagged_release(body);
                        crate::tagged::lin_tagged_release(h);
                        if err.is_some() {
                            saw_err = true;
                            break;
                        }
                    }
                    TaggedOutcome::Eof => break,
                    TaggedOutcome::Err(_m) => { saw_err = true; break; }
                }
            }
            assert!(saw_err, "(h) upstream injected error must propagate in-band");
            crate::tagged::lin_tagged_release(entries_stream);
        }
    }

    // (i) No double-close of parent: parent upstream is closed exactly once across entry drain +
    // entries stream release + all handles released.
    #[test]
    fn no_double_close_of_parent() {
        unsafe {
            let mut archive = Vec::new();
            archive.extend(tar_entry("x.txt", b"xyz"));
            archive.extend(vec![0u8; 1024]);

            let cc = Arc::new(AtomicUsize::new(0));
            let parent = make(vec![archive], cc.clone(), None);

            let entries_stream = lin_stream_tar_entries(parent as *const u8);
            crate::tagged::lin_tagged_release(parent);

            let handle = match pull_tagged(unwrap_stream(entries_stream)) {
                TaggedOutcome::Item(h) => h,
                other => panic!("expected entry, got {:?}", std::mem::discriminant(&other)),
            };

            let body = lin_tar_body(handle);
            let _ = drain_body(body);
            crate::tagged::lin_tagged_release(body);

            // Release entries stream first, then the handle.
            crate::tagged::lin_tagged_release(entries_stream);
            assert_eq!(cc.load(Ordering::SeqCst), 0, "(i) parent not yet closed (handle still live)");
            crate::tagged::lin_tagged_release(handle);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "(i) parent closed exactly once after all refs dropped");
        }
    }

    // ── CSV recordRows: differential correctness ────────────────────────────────────────────────

    /// Drive `lin_stream_csv_records` over `csv_bytes` and return every row as a
    /// `Vec<(String,String)>` of (key,value) pairs (in insertion order of the header).
    unsafe fn csv_records_to_vec(csv_bytes: &[u8]) -> Vec<Vec<(String, String)>> {
        use crate::map::lin_map_get_bytes;
        use crate::tagged::{lin_get_tag, lin_unbox_ptr, TAG_MAP, TAG_STR};

        // Build a byte-stream source backed by the slice.
        let src = make(vec![csv_bytes.to_vec()], Arc::new(AtomicUsize::new(0)), None);
        let stream = lin_stream_csv_records(src as *const u8, b',' as i32);
        crate::tagged::lin_tagged_release(src);

        // Peek at the header by peeking the first row's keys — we need the header to extract
        // values in order. Re-extract the header each row from the keys we already know.
        // Actually: parse headers from the first data record's map keys. We need them sorted
        // consistently; use the fixed header set derived from the CSV first row instead.
        // Simpler: collect all rows into TaggedVal*, then read each map by known keys.

        let mut rows: Vec<*mut u8> = Vec::new();
        loop {
            match pull_tagged(unwrap_stream(stream)) {
                TaggedOutcome::Item(item) => rows.push(item),
                TaggedOutcome::Eof => break,
                TaggedOutcome::Err(e) => panic!("csv error: {e}"),
            }
        }
        crate::tagged::lin_tagged_release(stream);

        if rows.is_empty() {
            return Vec::new();
        }

        // Extract key names from the first row's map.
        let first = rows[0];
        assert_eq!(lin_get_tag(first), TAG_MAP, "row must be a MAP");
        let map0 = lin_unbox_ptr(first) as *const crate::map::LinMap;
        let keys_arr = crate::map::lin_map_keys(map0);
        let n_keys = crate::array::lin_array_length(keys_arr) as usize;

        // Collect key strings.
        let mut header: Vec<String> = Vec::new();
        for ki in 0..n_keys {
            let kt = crate::array::lin_array_get_tagged(keys_arr, ki as i64);
            assert!(!kt.is_null() && (*kt).tag == TAG_STR);
            let ks = (*kt).payload as *const crate::string::LinString;
            let kb = std::slice::from_raw_parts((*ks).data.as_ptr(), (*ks).len as usize);
            header.push(String::from_utf8_lossy(kb).into_owned());
        }
        // Release the keys array (lin_map_keys returns a fresh LinArray* owning retained key refs).
        let keys_tagged = crate::tagged::alloc_tagged(crate::tagged::TAG_ARRAY, keys_arr as u64);
        crate::tagged::lin_tagged_release(keys_tagged);

        // For each row, look up every header key.
        let mut result: Vec<Vec<(String, String)>> = Vec::new();
        for row_ptr in &rows {
            let map = lin_unbox_ptr(*row_ptr) as *const crate::map::LinMap;
            let mut pairs: Vec<(String, String)> = Vec::new();
            for key in &header {
                let tv = lin_map_get_bytes(map, key.as_ptr(), key.len() as u32);
                let val = if tv.is_null() || (*tv).tag != TAG_STR {
                    String::new()
                } else {
                    let s = (*tv).payload as *const crate::string::LinString;
                    let b = std::slice::from_raw_parts((*s).data.as_ptr(), (*s).len as usize);
                    String::from_utf8_lossy(b).into_owned()
                };
                pairs.push((key.clone(), val));
            }
            result.push(pairs);
        }

        for row_ptr in rows {
            crate::tagged::lin_tagged_release(row_ptr);
        }
        result
    }

    /// Correctness gate for the UTF8-FAST lane: recordRows correctly handles
    /// multi-byte UTF-8 fields, empty fields, and quoted fields.
    #[test]
    fn csv_record_rows_multibyte_empty_quoted() {
        unsafe {
            // Header: multi-byte UTF-8 key, ascii key, ascii key
            // Row 1: multi-byte UTF-8 value, empty value, RFC-4180 quoted value with comma inside
            // Row 2: ascii round-trip
            let csv = "résumé,desc,note\ncafé,,\"hello, world\"\nsimple,plain,ok\n";
            let rows = csv_records_to_vec(csv.as_bytes());
            assert_eq!(rows.len(), 2, "expected 2 data rows");

            // Row 0
            let r0: std::collections::HashMap<_, _> = rows[0].iter().cloned().collect();
            assert_eq!(r0["résumé"], "café", "multi-byte UTF-8 value under multi-byte UTF-8 key");
            assert_eq!(r0["desc"], "", "empty field");
            // RFC-4180 quoted field: outer quotes stripped, inner content preserved
            assert_eq!(r0["note"], "hello, world", "quoted field with comma inside");

            // Row 1
            let r1: std::collections::HashMap<_, _> = rows[1].iter().cloned().collect();
            assert_eq!(r1["résumé"], "simple");
            assert_eq!(r1["desc"], "plain");
            assert_eq!(r1["note"], "ok");
        }
    }

    /// Wall-time smoke bench: parse a 10 k-row ASCII CSV 5 times; just confirms the fast path
    /// doesn't regress throughput vs a timing floor. Not a hard assertion — just prints wall ms.
    #[test]
    fn csv_record_rows_wall_time_smoke() {
        unsafe {
            // Build a 10 000-row CSV (~700 KB) in memory.
            let mut csv = String::from("stop_id,stop_name,stop_lat,stop_lon\n");
            for i in 0..10_000u32 {
                csv.push_str(&format!("{i},Station {i},51.{i},0.{i}\n"));
            }
            let csv_bytes = csv.into_bytes();

            let t0 = std::time::Instant::now();
            let reps = 5u32;
            for _ in 0..reps {
                let rows = csv_records_to_vec(&csv_bytes);
                assert_eq!(rows.len(), 10_000);
            }
            let elapsed = t0.elapsed();
            eprintln!(
                "csv_record_rows wall: {}ms total, {}ms/rep ({} rows × {} reps)",
                elapsed.as_millis(),
                elapsed.as_millis() / reps as u128,
                10_000,
                reps
            );
        }
    }
}
