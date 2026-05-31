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

/// A read backend behind a `Stream`. Each concrete source (file, TCP socket, process stdout,
/// stdin) implements this. `read` pulls the next chunk; `close` releases the OS resource.
///
/// `read` returns:
///   * `Ok(Some(bytes))` — a non-empty chunk of bytes was read.
///   * `Ok(None)`        — end of stream (EOF); no more data.
///   * `Err(msg)`        — an I/O error; the chunk could not be read.
pub trait StreamSource: Send {
    /// Pull the next chunk of bytes. `Ok(None)` signals EOF.
    fn read(&mut self) -> Result<Option<Vec<u8>>, String>;
    /// Release the underlying OS resource. Called at most once (the box guards against
    /// double-close via its `closed` flag), either by explicit `close()` or by the finalizer.
    fn close(&mut self);
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

/// Pull the next chunk from a `StreamBox` (given a boxed `Stream` value). The core read step:
///   * closed/null stream  → `Eof`
///   * backend `Ok(None)`  → `Eof`
///   * backend `Ok(chunk)` → `Chunk`
///   * backend `Err(msg)`  → `Err`
/// An empty (zero-length) chunk is treated as a valid chunk (the adapter layer decides EOF only
/// on `Ok(None)`); concrete backends signal EOF explicitly with `Ok(None)`.
pub unsafe fn stream_read_outcome(s: *const u8) -> ReadOutcome {
    let b = unwrap_stream(s);
    if b.is_null() {
        return ReadOutcome::Eof;
    }
    let mut guard = (*b).state.lock().unwrap();
    if guard.closed {
        return ReadOutcome::Eof;
    }
    match guard.source.as_mut() {
        None => ReadOutcome::Eof,
        Some(src) => match src.read() {
            Ok(Some(bytes)) => ReadOutcome::Chunk(bytes),
            Ok(None) => ReadOutcome::Eof,
            Err(msg) => ReadOutcome::Err(msg),
        },
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
}
