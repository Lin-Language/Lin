//! `Stream<T>` — an opaque, lazy, effectful, fallible PULL-SOURCE that owns an OS resource
//! (a file descriptor, socket, child-process pipe, …) (streams brief, ADR-072).
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

/// Pull the next BYTE chunk from a `StreamBox` (given a boxed `Stream` value), as a `ReadOutcome`.
/// Used by `lin_stream_read` (the low-level byte read). Closed/null → `Eof`.
pub unsafe fn stream_read_outcome(s: *const u8) -> ReadOutcome {
    match pull_tagged(unwrap_stream(s)) {
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
    /// Call `closure(arg)` — arg is a boxed TaggedVal* (consumed/borrowed per the closure's ABI);
    /// returns the boxed result. The closure ABI is `(env, arg) -> ret`.
    unsafe fn call(&self, arg: *mut u8) -> *mut u8 {
        if self.closure.is_null() {
            return std::ptr::null_mut();
        }
        let fn_ptr = *(self.closure.add(8) as *const *mut u8);
        let env_ptr = *(self.closure.add(16) as *const *mut u8);
        let call: unsafe extern "C-unwind" fn(*mut u8, *mut u8) -> *mut u8 =
            std::mem::transmute(fn_ptr);
        call(env_ptr, arg)
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
}
unsafe impl Send for Upstream {}

impl Upstream {
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
}
impl StreamSource for MapSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        match pull_tagged(self.up.boxptr) {
            TaggedOutcome::Eof => TaggedOutcome::Eof,
            TaggedOutcome::Err(m) => TaggedOutcome::Err(m),
            TaggedOutcome::Item(item) => {
                let out = self.f.call(item);
                // The closure consumed/derived `item`; release our reference to it. (The boxed
                // result `out` is independently owned and returned to the driver.)
                crate::tagged::lin_tagged_release(item);
                TaggedOutcome::Item(out)
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
}
impl StreamSource for FilterSource {
    unsafe fn read_tagged(&mut self) -> TaggedOutcome {
        loop {
            match pull_tagged(self.up.boxptr) {
                TaggedOutcome::Eof => return TaggedOutcome::Eof,
                TaggedOutcome::Err(m) => return TaggedOutcome::Err(m),
                TaggedOutcome::Item(item) => {
                    let verdict = self.p.call(item);
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
        match pull_tagged(self.up.boxptr) {
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
            match pull_tagged(self.up.boxptr) {
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
            match pull_tagged(self.up.boxptr) {
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

/// A push sink that writes every upstream item to a file, one item per line. Built lazily by
/// `writeStream`; nothing happens until a terminal driver (`.drain()`) pulls it. The sink is
/// itself a `StreamBox` whose `read_tagged` is never meant to be pulled item-by-item by an
/// adapter — instead `lin_stream_drain` recognises it and runs the write loop. We still model it
/// as a source so it composes uniformly and shares the close/finalizer machinery.
struct WriteSink {
    up: Upstream,
    path: String,
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

/// Drive a `WriteSink` to completion on the CALLING thread (brief §5): pull every upstream item,
/// write each as a UTF-8 line, until EOF (success → Null) or the first Err (→ Error object). The
/// file is opened once; an open/write failure short-circuits to an Error.
unsafe fn drive_sink(sink: &mut WriteSink) -> *mut u8 {
    use std::io::Write;
    let mut file = match std::fs::File::create(&sink.path) {
        Ok(f) => f,
        Err(e) => return crate::fs::make_error_tagged(&e.to_string()),
    };
    loop {
        match pull_tagged(sink.up.boxptr) {
            TaggedOutcome::Eof => return std::ptr::null_mut(), // Null = success
            TaggedOutcome::Err(m) => return crate::fs::make_error_tagged(&m),
            TaggedOutcome::Item(item) => {
                let line = item_to_line_bytes(item);
                crate::tagged::lin_tagged_release(item);
                if let Err(e) = file.write_all(&line).and_then(|_| file.write_all(b"\n")) {
                    return crate::fs::make_error_tagged(&e.to_string());
                }
            }
        }
    }
}

/// Render a tagged item as the bytes to write for one sink line: a String writes its UTF-8
/// bytes; a UInt8[] writes its raw bytes; anything else writes its `toString` rendering.
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
    lin_stream_retain_box(b as *const u8);
    Upstream { boxptr: b }
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
    StreamBox::new_boxed(Box::new(MapSource { up, f: LinFn::from_owned(closure) }))
}

/// `filter(s, p)` → a new `Stream` keeping items where `p(item)` is truthy.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_filter(s: *const u8, p: *mut u8) -> *mut u8 {
    let up = own_upstream(s);
    let closure = retain_closure(as_closure(p));
    StreamBox::new_boxed(Box::new(FilterSource { up, p: LinFn::from_owned(closure) }))
}

/// `take(s, n)` → a new `Stream` yielding at most `n` items.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_take(s: *const u8, n: i64) -> *mut u8 {
    let up = own_upstream(s);
    StreamBox::new_boxed(Box::new(TakeSource { up, remaining: n }))
}

/// `lines(s)` → a `Stream<String>` of newline-delimited lines over the byte stream `s`.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_lines(s: *const u8) -> *mut u8 {
    let up = own_upstream(s);
    StreamBox::new_boxed(Box::new(LinesSource { up, buf: Vec::new(), upstream_done: false }))
}

/// `chunks(s, n)` → a `Stream<UInt8[]>` re-chunked to `n` bytes per item.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_chunks(s: *const u8, n: i64) -> *mut u8 {
    let up = own_upstream(s);
    let size = if n > 0 { n as usize } else { 1 };
    StreamBox::new_boxed(Box::new(ChunksSource { up, size, buf: Vec::new(), upstream_done: false }))
}

/// `writeStream(s, path)` → a sink `Stream` that, when driven, writes each item of `s` to `path`
/// (one item per line). Lazy: no file is opened until `drain()`/`promise()` drives it.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_write(s: *const u8, path: *const u8) -> *mut u8 {
    let path_str = crate::fs::resolve_lin_str(path).unwrap_or_default();
    let up = own_upstream(s);
    StreamBox::new_boxed(Box::new(WriteSink { up, path: path_str }))
}

/// `.drain()` → drive the pipeline to completion on the CALLING thread → `Null | Error`
/// (brief §5). If `s` is a `WriteSink`, run its write loop; otherwise pull-and-discard every item
/// (so a non-sink pipeline can still be forced for effects), surfacing the first Err. Always
/// closes the stream afterwards (deterministic resource release).
#[no_mangle]
pub unsafe extern "C" fn lin_stream_drain(s: *const u8) -> *mut u8 {
    let b = unwrap_stream(s);
    if b.is_null() {
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

/// `collect(s)` → pull all items, concatenating their bytes into a single `UInt8[]` → `UInt8[] |
/// Error`. Closes the stream afterwards.
#[no_mangle]
pub unsafe extern "C" fn lin_stream_collect(s: *const u8) -> *mut u8 {
    let b = unwrap_stream(s);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

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
            // An open failure is an Error object, NOT a stream.
            assert_eq!(crate::tagged::lin_get_tag(r), crate::tagged::TAG_OBJECT);
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
            let lines = lin_stream_lines(src as *const u8);
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
            let lines = lin_stream_lines(src as *const u8);
            crate::tagged::lin_tagged_release(src);
            let r = lin_stream_collect(lines as *const u8);
            assert_eq!(crate::tagged::lin_get_tag(r), crate::tagged::TAG_OBJECT, "error object");
            crate::tagged::lin_tagged_release(r);
            crate::tagged::lin_tagged_release(lines);
            assert_eq!(cc.load(Ordering::SeqCst), 1, "errored stream still closed once");
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
}
