//! Async / await / parallel / worker / threadPool runtime support.
//!
//! Real OS-thread concurrency (ADR-027). `async(thunk)` spawns a `std::thread` that runs the
//! thunk inside a fault-isolation boundary (`fault::with_async_boundary`); a runtime fault
//! becomes an `Error` value surfaced at `await` rather than aborting the process. The thunk's
//! captured env is deep-copied (Option C) so the worker owns a private graph and the parent's
//! non-atomic refcounts are never touched concurrently. The result is a boxed `TaggedVal*`
//! computed by the worker and handed to the parent through the promise (the worker thread has
//! exited by the time `await` reads it — ownership transfer, no shared access).

use crate::memory::lin_alloc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

/// A raw pointer asserted safe to move across threads. We uphold the invariant manually: the
/// only pointers wrapped here are (a) a deep-copied, thread-private closure env, and (b) a
/// transferable result the producing thread no longer touches after publishing it.
#[derive(Clone, Copy)]
struct SendPtr(*mut u8);
unsafe impl Send for SendPtr {}

/// The resolved state of a promise: either a value (boxed `TaggedVal*`, owned by the promise)
/// or an error message captured at the thread boundary (built into an `Error` object lazily at
/// `await`, to keep object construction on the awaiting thread).
enum PromiseState {
    Pending,
    Resolved(SendPtr),
    Failed(String),
}

/// A Promise — a future value computed on another OS thread (spec §24.2). Backed by a
/// mutex+condvar so `await` blocks until the worker publishes a result.
#[repr(C)]
pub struct LinPromise {
    inner: Arc<(Mutex<PromiseState>, Condvar)>,
    handle: Option<JoinHandle<()>>,
}

/// A unit of work for a thread pool: a thunk (fn_ptr + deep-copied private env) plus the
/// promise state to resolve with its result. All pointers are addresses (Send) recast inside
/// the worker; the env is a thread-private copy (Option C) so non-atomic RC is safe.
struct PoolTask {
    fn_addr: usize,
    env_addr: usize,
    desc_addr: usize,
    env_size: u64,
    result: Arc<(Mutex<PromiseState>, Condvar)>,
}
// SAFETY: fn_addr is read-only code; env is a private deep copy not shared with any other
// thread; `result` is an Arc (Send). The addresses are upheld-valid by the spawn path.
unsafe impl Send for PoolTask {}

/// A bounded thread pool (spec §24.5): `n` worker threads draining a shared MPMC task queue.
/// `pool.async` enqueues work rather than spawning. Dropping the pool (program exit) closes the
/// queue; workers finish the in-flight task and exit.
#[repr(C)]
pub struct LinThreadPool {
    pub n: i32,
    queue: Arc<PoolQueue>,
    workers: Vec<JoinHandle<()>>,
}

/// Shared task queue + shutdown flag, guarded by a mutex with a condvar for idle workers.
struct PoolQueue {
    tasks: Mutex<PoolQueueState>,
    cvar: Condvar,
}
struct PoolQueueState {
    queue: std::collections::VecDeque<PoolTask>,
    shutdown: bool,
}

/// A message sent to a worker's mailbox (spec §24.6). `Request` carries a oneshot reply
/// channel; `Message` is fire-and-forget; `Close` triggers `onShutdown` + thread exit.
enum WorkerMsg {
    Request { msg: SendPtr, reply: std::sync::mpsc::Sender<WorkerReply> },
    Message { msg: SendPtr },
    Close,
}
unsafe impl Send for WorkerMsg {}

/// A worker's reply to a `request`: the handler's result, or an error if the handler faulted
/// (the worker survives a faulting message; the in-flight request gets the diagnostic, §24.6.5).
enum WorkerReply {
    Ok(SendPtr),
    Err(String),
}
unsafe impl Send for WorkerReply {}

/// A long-lived worker thread + MPSC mailbox (spec §24.6). The handler closure runs on the
/// worker thread, processing messages sequentially — so the handler MAY close over `var`
/// (§24.6.4): the state is confined to this one thread and never concurrently accessed.
/// `request` blocks for the reply; `message` is fire-and-forget; `close` drains, runs
/// `onShutdown`, and joins. Messages crossing into the worker are deep-copied (transferable).
#[repr(C)]
pub struct LinWorker {
    tx: std::sync::mpsc::Sender<WorkerMsg>,
    handle: Option<JoinHandle<()>>,
    /// True once `close` has been called, so later sends are rejected (§24.6.5).
    closed: bool,
    /// OWNING references to the handler / onClose closure structs (the heap allocations whose
    /// refcount lives at offset 0). The worker thread runs these closures for its whole lifetime,
    /// so the worker must hold a reference that outlives the frame that built it (the factory in
    /// §24.6.4 returns the worker, then its frame releases the closures). Retained on the parent
    /// thread in `lin_worker_new` BEFORE the worker thread is spawned, released in
    /// `lin_worker_close` AFTER the thread is joined (no concurrent refcount access). Cleared to
    /// null after release so a double `close` cannot double-release. Null when the corresponding
    /// closure was absent (e.g. no onClose).
    on_msg_cls: *mut u8,
    on_close_cls: *mut u8,
}

/// Build an `Error` map `{ "type": "error", "message": <msg> }` as a TAG_MAP TaggedVal*.
unsafe fn make_error_tagged(msg: &str) -> *mut u8 {
    crate::fs::make_error_tagged(msg)
}

/// Box a raw `*mut LinPromise` into a `TaggedVal*(TAG_PROMISE)` so it round-trips through
/// TypeVar slots and arrays. Null promise → null Json.
#[no_mangle]
pub unsafe extern "C" fn lin_box_promise(p: *mut LinPromise) -> *mut u8 {
    if p.is_null() {
        return std::ptr::null_mut();
    }
    crate::tagged::alloc_tagged(crate::tagged::TAG_PROMISE, p as u64)
}

/// Unbox a `TaggedVal*(TAG_PROMISE)` back to the raw `*mut LinPromise`. Accepts an
/// already-raw pointer defensively (returns it unchanged if its first byte isn't TAG_PROMISE…
/// — but callers always pass a boxed promise). Null → null.
#[no_mangle]
pub unsafe extern "C" fn lin_unbox_promise(p: *mut u8) -> *mut LinPromise {
    if p.is_null() {
        return std::ptr::null_mut();
    }
    let tv = &*(p as *const crate::tagged::TaggedVal);
    if tv.tag == crate::tagged::TAG_PROMISE {
        tv.payload as *mut LinPromise
    } else {
        // Not a boxed promise — treat the pointer itself as the promise (legacy/raw path).
        p as *mut LinPromise
    }
}

/// Box an opaque runtime handle (`*LinThreadPool` / `*LinWorker`) as a TaggedVal*(TAG_HANDLE)
/// so it round-trips through TypeVar slots / Json params. Null → null.
#[no_mangle]
pub unsafe extern "C" fn lin_box_handle(h: *mut u8) -> *mut u8 {
    if h.is_null() {
        return std::ptr::null_mut();
    }
    crate::tagged::alloc_tagged(crate::tagged::TAG_HANDLE, h as u64)
}

/// Unbox a TaggedVal*(TAG_HANDLE) back to the raw handle pointer. Accepts an already-raw
/// pointer defensively. Null → null.
#[no_mangle]
pub unsafe extern "C" fn lin_unbox_handle(p: *mut u8) -> *mut u8 {
    if p.is_null() {
        return std::ptr::null_mut();
    }
    let tv = &*(p as *const crate::tagged::TaggedVal);
    if tv.tag == crate::tagged::TAG_HANDLE {
        tv.payload as *mut u8
    } else {
        p
    }
}

/// Allocate a LinPromise that is already resolved to `value` (no thread). Used for the
/// degenerate inline path and by combinators that need a settled promise.
#[no_mangle]
pub unsafe extern "C" fn lin_make_promise(value: *mut u8) -> *mut LinPromise {
    let inner = Arc::new((Mutex::new(PromiseState::Resolved(SendPtr(value))), Condvar::new()));
    let p = lin_alloc(std::mem::size_of::<LinPromise>()) as *mut LinPromise;
    std::ptr::write(p, LinPromise { inner, handle: None });
    p
}

/// Spawn the thunk closure `thunk` (a `*LinClosure` { rc, _pad, fn_ptr, env_ptr, .. }) on a
/// new OS thread and return a `LinPromise` for its result. The thunk's captured env is
/// deep-copied (Option C, ADR-027) so the worker owns a private graph and the parent's
/// non-atomic refcounts are never touched concurrently.
///
/// If the env is NOT transferable (no capture descriptor, or it captures a function/iterator —
/// `CAP_OPAQUE`), we cannot safely hand it to another thread, so the thunk is run **inline** on
/// the calling thread and the promise resolves immediately. This is the sound fallback: still
/// correct, just without parallelism for that one thunk. (The checker already bans `var`
/// captures and non-transferable *returns*; an opaque *captured function* is the only case that
/// reaches here, and §24.2.1 allows it — running inline keeps it correct.)
#[no_mangle]
pub unsafe extern "C" fn lin_async_spawn(thunk: *mut u8) -> *mut LinPromise {
    crate::fault::install_quiet_fault_hook();
    if thunk.is_null() {
        return lin_make_promise(std::ptr::null_mut());
    }
    // Closure layout: offset 8 = fn_ptr, offset 16 = env_ptr, offset 40 = capture descriptor.
    let fn_ptr = *(thunk.add(8) as *const *mut u8);
    let env_ptr = *(thunk.add(16) as *const *mut u8);
    let cap_desc = *(thunk.add(40) as *const *const u8);

    if !crate::transfer::env_is_transferable(env_ptr, cap_desc) {
        // Run inline (sound fallback). We still want fault isolation even inline, so wrap it.
        let outcome = crate::fault::with_async_boundary(|| {
            let call: unsafe extern "C-unwind" fn(*mut u8) -> *mut u8 = std::mem::transmute(fn_ptr);
            call(env_ptr)
        });
        return match outcome {
            Ok(v) => lin_make_promise(v),
            Err(msg) => lin_make_promise(make_error_tagged(&msg)),
        };
    }

    // Deep-copy the env so the worker owns a private graph (null env → null, no copy needed).
    let env_copy = crate::transfer::transfer_clone_env(env_ptr, cap_desc);
    // env_size for releasing the copy on the worker: 8 + count*8, recovered from the descriptor.
    let env_size: u64 = if env_copy.is_null() {
        0
    } else {
        let count = *(cap_desc as *const u32) as u64;
        8 + count * 8
    };

    let inner = Arc::new((Mutex::new(PromiseState::Pending), Condvar::new()));
    let inner_for_thread = Arc::clone(&inner);
    // Capture the pointers as usize (unconditionally Send) and recast inside; the safety
    // invariant — env_copy is a thread-private deep copy, fn_ptr is read-only code — is
    // upheld manually (ADR-027 Option C).
    let fn_addr = fn_ptr as usize;
    let env_addr = env_copy as usize;
    let desc_addr = cap_desc as usize;

    let handle = std::thread::spawn(move || {
        let fn_ptr = fn_addr as *mut u8;
        let env_ptr = env_addr as *mut u8;
        let outcome = crate::fault::with_async_boundary(|| {
            let call: unsafe extern "C-unwind" fn(*mut u8) -> *mut u8 = std::mem::transmute(fn_ptr);
            call(env_ptr)
        });
        // Free the thread-private env copy now that the thunk has finished with it.
        if !env_ptr.is_null() && env_size > 0 {
            crate::transfer::release_env_copy(env_ptr, desc_addr as *const u8, env_size);
        }
        let (lock, cvar) = &*inner_for_thread;
        let mut state = lock.lock().unwrap();
        *state = match outcome {
            Ok(v) => PromiseState::Resolved(SendPtr(v)),
            Err(msg) => PromiseState::Failed(msg),
        };
        cvar.notify_all();
    });

    let p = lin_alloc(std::mem::size_of::<LinPromise>()) as *mut LinPromise;
    std::ptr::write(p, LinPromise { inner, handle: Some(handle) });
    p
}

/// `.promise()` (streams brief §5, Stage 8): MOVE the stream pipeline `s` onto a NEW worker
/// thread that drives it to EOF, and return a `Promise<Null | Error>`. The stream pointer is
/// handed off VERBATIM (the IR suppresses the caller's release via the move), so the worker owns
/// the whole pipeline graph — disjoint from the parent's, keeping non-atomic RC sound. A fault
/// inside a transform closure is caught at the async boundary (`with_async_boundary`) and surfaces
/// as the awaited `Error` (ADR-045 / §32.2.2). The worker closes the fd exactly once.
///
/// A null stream resolves immediately to Null. Unlike `lin_async_spawn`, there is no closure env
/// to deep-copy — the single moved resource pointer IS the whole transfer, so this always runs on
/// a real worker (never the inline fallback).
#[no_mangle]
pub unsafe extern "C" fn lin_stream_promise(s: *mut u8) -> *mut LinPromise {
    crate::fault::install_quiet_fault_hook();
    if s.is_null() {
        return lin_make_promise(std::ptr::null_mut());
    }
    let inner = Arc::new((Mutex::new(PromiseState::Pending), Condvar::new()));
    let inner_for_thread = Arc::clone(&inner);
    // The stream box pointer is Send-by-handoff: the parent will not touch it again (the move
    // suppressed its release), so the worker is its sole owner. Carry it as a usize.
    let stream_addr = s as usize;
    let handle = std::thread::spawn(move || {
        let s = stream_addr as *mut u8;
        let outcome = crate::fault::with_async_boundary(|| {
            // Drive + consume the moved stream on this worker; closes the fd exactly once here.
            crate::stream::lin_stream_drive_owned(s)
        });
        let (lock, cvar) = &*inner_for_thread;
        let mut state = lock.lock().unwrap();
        *state = match outcome {
            Ok(v) => PromiseState::Resolved(SendPtr(v)),
            Err(msg) => PromiseState::Failed(msg),
        };
        cvar.notify_all();
    });
    let p = lin_alloc(std::mem::size_of::<LinPromise>()) as *mut LinPromise;
    std::ptr::write(p, LinPromise { inner, handle: Some(handle) });
    p
}

/// Await a promise: block until the worker publishes a result, join the thread, and return
/// the resolved `TaggedVal*` (or a freshly-built `Error` object on fault). Ownership of the
/// result transfers to the caller. Null promise → null (the Json null value).
#[no_mangle]
pub unsafe extern "C" fn lin_await_promise(promise: *mut LinPromise) -> *mut u8 {
    if promise.is_null() {
        return std::ptr::null_mut();
    }
    // Take the join handle so we can join exactly once.
    let handle = (*promise).handle.take();
    let inner = Arc::clone(&(*promise).inner);
    // Wait for resolution.
    let result = {
        let (lock, cvar) = &*inner;
        let mut state = lock.lock().unwrap();
        loop {
            match &*state {
                PromiseState::Pending => {
                    state = cvar.wait(state).unwrap();
                }
                PromiseState::Resolved(p) => break Ok(p.0),
                PromiseState::Failed(msg) => break Err(msg.clone()),
            }
        }
    };
    // Join the worker thread (it has already published; this reaps it).
    if let Some(h) = handle {
        let _ = h.join();
    }
    match result {
        Ok(v) => {
            // Auto-flatten nested promises (spec §24.2.3): if the thunk itself returned a
            // Promise (boxed TAG_PROMISE), resolve through it. The inner promise's result
            // ownership transfers out; we free the now-redundant outer TAG_PROMISE box shell.
            if !v.is_null() && (*(v as *const crate::tagged::TaggedVal)).tag == crate::tagged::TAG_PROMISE {
                let inner_promise = (*(v as *const crate::tagged::TaggedVal)).payload as *mut LinPromise;
                let flattened = lin_await_promise(inner_promise);
                crate::tagged::lin_tagged_free_box(v);
                flattened
            } else {
                v
            }
        }
        Err(msg) => make_error_tagged(&msg),
    }
}

/// Poll a promise without consuming it. Returns `Some(Ok(value))` / `Some(Err(msg))` if it has
/// settled, or `None` if still pending. The returned value pointer is still owned by the
/// promise — callers that keep it must deep-copy.
unsafe fn poll_promise(promise: *mut LinPromise) -> Option<Result<*mut u8, String>> {
    if promise.is_null() {
        return Some(Ok(std::ptr::null_mut()));
    }
    let (lock, _cvar) = &*(*promise).inner;
    let state = lock.lock().unwrap();
    match &*state {
        PromiseState::Pending => None,
        PromiseState::Resolved(p) => Some(Ok(p.0)),
        PromiseState::Failed(msg) => Some(Err(msg.clone())),
    }
}

/// `race(promises)` (spec §24.4): returns a settled promise carrying the result of the FIRST of
/// `promises` to complete. The others keep running; their results are discarded (abandoned, not
/// cancelled — Lin has no cancellation in v1). `promises` is the raw `LinArray*` whose elements'
/// payloads are `*mut LinPromise`. The winning value is deep-copied into the returned promise so
/// ownership is independent of the still-live source promises.
#[no_mangle]
pub unsafe extern "C" fn lin_race(promises: *mut u8) -> *mut LinPromise {
    use crate::array::{lin_array_length, LinArray};
    let arr = promises as *mut LinArray;
    if arr.is_null() {
        return lin_make_promise(std::ptr::null_mut());
    }
    let len = lin_array_length(arr);
    if len == 0 {
        return lin_make_promise(std::ptr::null_mut());
    }
    loop {
        for i in 0..len as usize {
            let elem = (*arr).data.add(i);
            let p = (*elem).payload as *mut LinPromise;
            if let Some(settled) = poll_promise(p) {
                let v = match settled {
                    Ok(v) => crate::transfer::lin_transfer_clone(v as *const u8),
                    Err(msg) => make_error_tagged(&msg),
                };
                return lin_make_promise(v);
            }
        }
        // Brief backoff to avoid a busy spin while no promise has settled.
        std::thread::sleep(std::time::Duration::from_micros(200));
    }
}

/// `timeout(promise, ms)` (spec §24.4): returns a settled promise carrying the original value if
/// `promise` completes within `ms` milliseconds, else the Json null value (timed out — the slow
/// thread is abandoned, not cancelled). An error result is passed through. The value is
/// deep-copied so it is independent of the source promise.
#[no_mangle]
pub unsafe extern "C" fn lin_timeout(promise: *mut LinPromise, ms: i32) -> *mut LinPromise {
    if promise.is_null() {
        return lin_make_promise(std::ptr::null_mut());
    }
    let deadline_ms = ms.max(0) as u64;
    let mut waited: u64 = 0;
    let step: u64 = 1; // poll every 1ms
    loop {
        if let Some(settled) = poll_promise(promise) {
            let v = match settled {
                Ok(v) => crate::transfer::lin_transfer_clone(v as *const u8),
                Err(msg) => make_error_tagged(&msg),
            };
            return lin_make_promise(v);
        }
        if waited >= deadline_ms {
            // Timed out: resolve with null. The source thread keeps running (abandoned).
            return lin_make_promise(std::ptr::null_mut());
        }
        std::thread::sleep(std::time::Duration::from_millis(step));
        waited += step;
    }
}

/// `retry(thunk, n)` (spec §24.4): spawn `thunk` up to `n` times, returning a settled promise
/// with the first result that is NOT an `Error`; if all `n` attempts error, the last `Error` is
/// the result. `thunk` is the raw closure pointer. Runs attempts sequentially (each is itself a
/// real spawn+await), which is the spec's "spawns the thunk up to n times" semantics.
#[no_mangle]
pub unsafe extern "C" fn lin_retry(thunk: *mut u8, n: i32) -> *mut LinPromise {
    let attempts = n.max(1);
    let mut last: *mut u8 = std::ptr::null_mut();
    for _ in 0..attempts {
        let p = lin_async_spawn(thunk);
        last = lin_await_promise(p);
        if !is_error_value(last) {
            return lin_make_promise(last);
        }
        // Error: free this attempt's box and try again (unless it was the last).
        if !last.is_null() {
            crate::tagged::lin_tagged_release(last);
            last = std::ptr::null_mut();
        }
    }
    // All attempts errored — re-run once more to produce a fresh last Error to return. To avoid
    // an extra spawn, just rebuild a generic error if we cleared the last one.
    if last.is_null() {
        last = make_error_tagged("retry: all attempts failed");
    }
    lin_make_promise(last)
}

/// True if `v` is an Error-shaped value `{ "type": "error", ... }` (TAG_MAP or TAG_OBJECT).
/// Used by `retry` to decide success vs. failure.
unsafe fn is_error_value(v: *mut u8) -> bool {
    use crate::tagged::{TaggedVal, TAG_MAP, TAG_STR};
    if v.is_null() { return false; }
    let tv = &*(v as *const TaggedVal);
    let got_type: Option<*const TaggedVal> = match tv.tag {
        TAG_MAP => {
            let map = tv.payload as *const crate::map::LinMap;
            if map.is_null() { return false; }
            let g = crate::map::lin_map_get_bytes(map, b"type".as_ptr(), 4);
            if g.is_null() { None } else { Some(g) }
        }
        crate::tagged::TAG_RECORD => {
            // Sealed struct in a dynamic slot: materialize to map and look up "type".
            let sealed = tv.payload as *mut u8;
            if sealed.is_null() { return false; }
            let named_desc = *((sealed.add(16)) as *const *const u8);
            let lmap = crate::sealed::materialize_sealed_to_map_pub(sealed, named_desc);
            if lmap.is_null() { return false; }
            let g = crate::map::lin_map_get_bytes(lmap, b"type".as_ptr(), 4);
            let result = if g.is_null() { None } else { Some(g as *const TaggedVal) };
            // Evaluate before releasing the map since g is a borrow from it.
            let is_err = match result {
                None => false,
                Some(got) => {
                    if (*got).tag != TAG_STR { false } else {
                        let s = (*got).payload as *const crate::string::LinString;
                        !s.is_null() && (*s).as_str() == "error"
                    }
                }
            };
            crate::map::lin_map_release(lmap);
            return is_err;
        }
        _ => return false,
    };
    match got_type {
        None => false,
        Some(got) => {
            if (*got).tag != TAG_STR { return false; }
            let s = (*got).payload as *const crate::string::LinString;
            if s.is_null() { return false; }
            (*s).as_str() == "error"
        }
    }
}

/// Run all tasks in `tasks` (a tagged `LinArray*`) concurrently on OS threads, then join them
/// in order, returning a fresh tagged `LinArray*` of their boxed results — **order-preserving**
/// (result[i] is task[i]'s result), spec §24.3. A faulting task yields an `Error` object in its
/// slot (fault isolation per task). Each thunk's env is deep-copied for transfer (Option C); a
/// non-transferable task runs inline.
///
/// Each element is dispatched on its runtime TAG (the `parallel` signature is `(tasks: Json)`,
/// deliberately untyped, so the checker cannot distinguish thunks from promises — it MUST be
/// resolved at runtime):
///   * `TAG_FUNCTION` — a thunk closure: spawn it via `lin_async_spawn` and await the promise we
///     just created, moving its result out (we own that promise outright).
///   * `TAG_PROMISE`  — an already-spawned promise (the user holds the reference): do NOT
///     re-spawn (its payload is a `*mut LinPromise`, not a closure — reading the closure layout
///     off it dereferences garbage). Await the existing promise and DEEP-COPY its resolved value
///     (mirroring `race`/`timeout`), because the value is still owned by the promise the user may
///     await again; moving it out would leave a dangling pointer behind it.
///   * anything else — pass the value through unchanged (deep-copied into the result slot).
///
/// `tasks` is the raw (unboxed) array. Ownership of the result array transfers to the caller.
#[no_mangle]
pub unsafe extern "C" fn lin_parallel(tasks: *mut u8) -> *mut u8 {
    use crate::array::{lin_array_alloc, lin_array_length, lin_array_push_tagged, LinArray};
    use crate::tagged::{TAG_FUNCTION, TAG_PROMISE};
    crate::fault::install_quiet_fault_hook();
    let arr = tasks as *mut LinArray;
    if arr.is_null() {
        return lin_array_alloc(4) as *mut u8;
    }
    let len = lin_array_length(arr);

    // First pass: kick off all the work so it overlaps. For thunks we spawn a fresh promise we
    // own; for already-spawned promises we just record the existing handle (already running).
    // `tasks` is a tagged array (elem_tag 0xFF); we borrow each element's payload (no retain).
    enum Slot {
        /// A promise we spawned and own outright — move its result out at join.
        Owned(*mut LinPromise),
        /// A user-held promise — await but DEEP-COPY its value (the promise keeps ownership).
        Borrowed(*mut LinPromise),
        /// A non-task element — pass the value through (deep-copied into the result).
        Passthrough(*const u8),
    }
    let mut slots: Vec<Slot> = Vec::with_capacity(len as usize);
    for i in 0..len as usize {
        let elem = (*arr).data.add(i);
        match (*elem).tag {
            TAG_FUNCTION => {
                let cls = (*elem).payload as *mut u8; // closure ptr (TAG_FUNCTION payload)
                slots.push(Slot::Owned(lin_async_spawn(cls)));
            }
            TAG_PROMISE => {
                let p = (*elem).payload as *mut LinPromise; // already-spawned promise
                slots.push(Slot::Borrowed(p));
            }
            _ => slots.push(Slot::Passthrough(elem as *const u8)),
        }
    }

    // Second pass: join in order, preserving result positions. push_tagged copies the 16-byte
    // box and takes ownership of the inner payload.
    let out = lin_array_alloc(len.max(1) as u64);
    for slot in slots {
        match slot {
            Slot::Owned(p) => {
                // We own this promise: lin_await returns an owned result box; after push_tagged
                // takes the inner payload we free only the now-redundant box shell.
                let res = lin_await_promise(p);
                lin_array_push_tagged(out, res as *const u8);
                if !res.is_null() {
                    crate::tagged::lin_tagged_free_box(res);
                }
            }
            Slot::Borrowed(p) => {
                // User-held promise: block until it settles, then deep-copy its value so the
                // result graph is independent of the still-live source promise (same discipline
                // as `race`/`timeout`). poll-loop because we must not consume the promise.
                let v = loop {
                    if let Some(settled) = poll_promise(p) {
                        break match settled {
                            Ok(v) => crate::transfer::lin_transfer_clone(v as *const u8),
                            Err(msg) => make_error_tagged(&msg),
                        };
                    }
                    std::thread::sleep(std::time::Duration::from_micros(200));
                };
                lin_array_push_tagged(out, v as *const u8);
                if !v.is_null() {
                    crate::tagged::lin_tagged_free_box(v);
                }
            }
            Slot::Passthrough(elem) => {
                let copy = crate::transfer::lin_transfer_clone(elem);
                lin_array_push_tagged(out, copy as *const u8);
                if !copy.is_null() {
                    crate::tagged::lin_tagged_free_box(copy);
                }
            }
        }
    }
    out as *mut u8
}

/// One pool worker: drain the queue, running each task in a fault boundary and resolving its
/// promise. Exits when the queue is shut down and empty.
unsafe fn pool_worker_loop(queue: Arc<PoolQueue>) {
    crate::fault::install_quiet_fault_hook();
    loop {
        let task = {
            let mut state = queue.tasks.lock().unwrap();
            loop {
                if let Some(t) = state.queue.pop_front() {
                    break Some(t);
                }
                if state.shutdown {
                    break None;
                }
                state = queue.cvar.wait(state).unwrap();
            }
        };
        let Some(task) = task else { return };
        let fn_ptr = task.fn_addr as *mut u8;
        let env_ptr = task.env_addr as *mut u8;
        let outcome = crate::fault::with_async_boundary(|| {
            let call: unsafe extern "C-unwind" fn(*mut u8) -> *mut u8 = std::mem::transmute(fn_ptr);
            call(env_ptr)
        });
        if !env_ptr.is_null() && task.env_size > 0 {
            crate::transfer::release_env_copy(env_ptr, task.desc_addr as *const u8, task.env_size);
        }
        let (lock, cvar) = &*task.result;
        let mut st = lock.lock().unwrap();
        *st = match outcome {
            Ok(v) => PromiseState::Resolved(SendPtr(v)),
            Err(msg) => PromiseState::Failed(msg),
        };
        cvar.notify_all();
    }
}

/// Allocate a bounded LinThreadPool with `n` worker threads draining a shared task queue.
/// `n` is clamped to at least 1.
#[no_mangle]
pub unsafe extern "C" fn lin_thread_pool_new(n: i32) -> *mut LinThreadPool {
    let count = n.max(1) as usize;
    let queue = Arc::new(PoolQueue {
        tasks: Mutex::new(PoolQueueState { queue: std::collections::VecDeque::new(), shutdown: false }),
        cvar: Condvar::new(),
    });
    let mut workers = Vec::with_capacity(count);
    for _ in 0..count {
        let q = Arc::clone(&queue);
        workers.push(std::thread::spawn(move || pool_worker_loop(q)));
    }
    let ptr = lin_alloc(std::mem::size_of::<LinThreadPool>()) as *mut LinThreadPool;
    std::ptr::write(ptr, LinThreadPool { n, queue, workers });
    ptr
}

/// Enqueue `thunk` (a closure pointer) on `pool` and return a `LinPromise` for its result. The
/// thunk's env is deep-copied (Option C) so the worker owns a private graph; a non-transferable
/// env (CAP_OPAQUE) falls back to running inline on the calling thread. Mirror of
/// `lin_async_spawn` but enqueues instead of spawning a fresh thread.
#[no_mangle]
pub unsafe extern "C" fn lin_pool_async_one(pool: *mut LinThreadPool, thunk: *mut u8) -> *mut LinPromise {
    if pool.is_null() || thunk.is_null() {
        return lin_async_spawn(thunk);
    }
    let fn_ptr = *(thunk.add(8) as *const *mut u8);
    let env_ptr = *(thunk.add(16) as *const *mut u8);
    let cap_desc = *(thunk.add(40) as *const *const u8);

    if !crate::transfer::env_is_transferable(env_ptr, cap_desc) {
        let outcome = crate::fault::with_async_boundary(|| {
            let call: unsafe extern "C-unwind" fn(*mut u8) -> *mut u8 = std::mem::transmute(fn_ptr);
            call(env_ptr)
        });
        return match outcome {
            Ok(v) => lin_make_promise(v),
            Err(msg) => lin_make_promise(make_error_tagged(&msg)),
        };
    }

    let env_copy = crate::transfer::transfer_clone_env(env_ptr, cap_desc);
    let env_size: u64 = if env_copy.is_null() {
        0
    } else {
        8 + (*(cap_desc as *const u32) as u64) * 8
    };

    let result = Arc::new((Mutex::new(PromiseState::Pending), Condvar::new()));
    let task = PoolTask {
        fn_addr: fn_ptr as usize,
        env_addr: env_copy as usize,
        desc_addr: cap_desc as usize,
        env_size,
        result: Arc::clone(&result),
    };
    {
        let q = &(*pool).queue;
        let mut state = q.tasks.lock().unwrap();
        state.queue.push_back(task);
        q.cvar.notify_one();
    }
    let p = lin_alloc(std::mem::size_of::<LinPromise>()) as *mut LinPromise;
    std::ptr::write(p, LinPromise { inner: result, handle: None });
    p
}

/// Invoke a worker closure `(env?, msg) -> reply` by raw fn/env pointers on the worker thread.
unsafe fn call_worker_handler(fn_ptr: *mut u8, env_ptr: *mut u8, has_env: u8, msg: *mut u8) -> *mut u8 {
    if fn_ptr.is_null() {
        return std::ptr::null_mut();
    }
    if has_env != 0 {
        let call: unsafe extern "C-unwind" fn(*mut u8, *mut u8) -> *mut u8 = std::mem::transmute(fn_ptr);
        call(env_ptr, msg)
    } else {
        let call: unsafe extern "C-unwind" fn(*mut u8) -> *mut u8 = std::mem::transmute(fn_ptr);
        call(msg)
    }
}

/// Spawn a long-lived worker thread with an MPSC mailbox. `on_msg_*` is the message handler
/// closure (`(Msg) => Reply`); `on_close_*` is the optional `onShutdown` closure (`() => Null`,
/// invoked once at `close`). The handler env stays on the worker thread (thread-confined state,
/// so the handler may close over `var`, §24.6.4). Returns a `*LinWorker` handle.
#[no_mangle]
pub unsafe extern "C" fn lin_worker_new(
    on_msg_fn: *mut u8,
    on_msg_env: *mut u8,
    on_msg_has_env: u8,
    on_close_fn: *mut u8,
    on_close_env: *mut u8,
    on_close_has_env: u8,
    on_msg_cls: *mut u8,
    on_close_cls: *mut u8,
) -> *mut LinWorker {
    crate::fault::install_quiet_fault_hook();
    // Take an OWNING reference to the handler / onClose closures (their fn_ptr + env live for the
    // worker thread's whole lifetime, §24.6.4). The refcount is the u32 at offset 0 of the closure
    // struct — exactly the same pointer codegen read fn_ptr/env_ptr from — so this bumps the live
    // closure (NOT a derived/garbage pointer, which was the prior unsound attempt). Done HERE on
    // the parent thread before `thread::spawn`, so there is no concurrent refcount write.
    if !on_msg_cls.is_null() {
        crate::memory::lin_rc_retain(on_msg_cls as *mut u32);
    }
    if !on_close_cls.is_null() {
        crate::memory::lin_rc_retain(on_close_cls as *mut u32);
    }
    let (tx, rx) = std::sync::mpsc::channel::<WorkerMsg>();

    // Capture handler/onClose pointers as addresses (Send); they are read-only code + a
    // thread-confined env that lives on the worker thread for the worker's lifetime.
    let msg_fn = on_msg_fn as usize;
    let msg_env = on_msg_env as usize;
    let close_fn = on_close_fn as usize;
    let close_env = on_close_env as usize;

    let handle = std::thread::spawn(move || {
        // Process messages sequentially until Close (or the sender is dropped).
        while let Ok(m) = rx.recv() {
            match m {
                WorkerMsg::Request { msg, reply } => {
                    let msg_ptr = msg.0;
                    let outcome = crate::fault::with_async_boundary(|| {
                        call_worker_handler(msg_fn as *mut u8, msg_env as *mut u8, on_msg_has_env, msg_ptr)
                    });
                    let r = match outcome {
                        Ok(v) => WorkerReply::Ok(SendPtr(v)),
                        Err(e) => WorkerReply::Err(e),
                    };
                    // If the requester has gone away, drop the reply silently.
                    let _ = reply.send(r);
                }
                WorkerMsg::Message { msg } => {
                    let msg_ptr = msg.0;
                    // Fire-and-forget: a fault here is isolated (worker survives), result dropped.
                    let _ = crate::fault::with_async_boundary(|| {
                        call_worker_handler(msg_fn as *mut u8, msg_env as *mut u8, on_msg_has_env, msg_ptr)
                    });
                }
                WorkerMsg::Close => {
                    // Run onShutdown (if any), then exit the loop. Fault in onShutdown is isolated.
                    if close_fn != 0 {
                        let _ = crate::fault::with_async_boundary(|| {
                            call_worker_handler(close_fn as *mut u8, close_env as *mut u8, on_close_has_env, std::ptr::null_mut())
                        });
                    }
                    break;
                }
            }
        }
    });

    let ptr = lin_alloc(std::mem::size_of::<LinWorker>()) as *mut LinWorker;
    std::ptr::write(ptr, LinWorker {
        tx,
        handle: Some(handle),
        closed: false,
        on_msg_cls,
        on_close_cls,
    });
    ptr
}

/// Send a message to a worker and BLOCK for its reply (spec §24.6: `request`). The message is
/// deep-copied for transfer (Option C). On handler fault, returns an `Error` object. Sending to
/// a closed/dead worker returns an `Error` (§24.6.5).
#[no_mangle]
pub unsafe extern "C" fn lin_worker_request(worker: *mut LinWorker, msg: *mut u8) -> *mut u8 {
    if worker.is_null() || (*worker).closed {
        return make_error_tagged("worker is closed");
    }
    let msg_copy = crate::transfer::lin_transfer_clone(msg as *const u8);
    let (reply_tx, reply_rx) = std::sync::mpsc::channel::<WorkerReply>();
    if (*worker).tx.send(WorkerMsg::Request { msg: SendPtr(msg_copy), reply: reply_tx }).is_err() {
        return make_error_tagged("worker is dead");
    }
    match reply_rx.recv() {
        Ok(WorkerReply::Ok(v)) => v.0,
        Ok(WorkerReply::Err(e)) => make_error_tagged(&e),
        Err(_) => make_error_tagged("worker died before replying"),
    }
}

/// Fire-and-forget message to a worker (spec §24.6: `message`); the reply is discarded. The
/// message is deep-copied for transfer. Sending to a closed worker is a no-op.
#[no_mangle]
pub unsafe extern "C" fn lin_worker_message(worker: *mut LinWorker, msg: *mut u8) {
    if worker.is_null() || (*worker).closed {
        return;
    }
    let msg_copy = crate::transfer::lin_transfer_clone(msg as *const u8);
    let _ = (*worker).tx.send(WorkerMsg::Message { msg: SendPtr(msg_copy) });
}

/// Close a worker (spec §24.6: `close`): send the shutdown sentinel (the worker drains queued
/// messages first, then runs `onShutdown`), and join the thread. Idempotent.
#[no_mangle]
pub unsafe extern "C" fn lin_worker_close(worker: *mut LinWorker) {
    if worker.is_null() || (*worker).closed {
        return;
    }
    (*worker).closed = true;
    let _ = (*worker).tx.send(WorkerMsg::Close);
    if let Some(h) = (*worker).handle.take() {
        let _ = h.join();
    }
    // The worker thread has now exited and will never touch the handler/onClose closures again,
    // so it is safe (and single-threaded) to drop the owning reference taken in `lin_worker_new`.
    // Clear the slots so a second `close` (idempotent, §24.6.5) cannot double-release.
    let on_msg_cls = std::mem::replace(&mut (*worker).on_msg_cls, std::ptr::null_mut());
    if !on_msg_cls.is_null() {
        crate::memory::lin_closure_release(on_msg_cls);
    }
    let on_close_cls = std::mem::replace(&mut (*worker).on_close_cls, std::ptr::null_mut());
    if !on_close_cls.is_null() {
        crate::memory::lin_closure_release(on_close_cls);
    }
}

#[cfg(test)]
mod parallel_tests {
    use super::*;
    use crate::array::{lin_array_alloc, lin_array_get, lin_array_length, lin_array_push_tagged};
    use crate::tagged::{
        alloc_tagged, lin_get_tag, lin_tagged_release, lin_unbox_int64, TaggedVal, TAG_INT64,
        TAG_PROMISE,
    };

    /// Regression: `lin_parallel` over an array whose elements are ALREADY-SPAWNED promise boxes
    /// (TAG_PROMISE) — the case that used to misread the closure layout and abort. Verifies it
    /// awaits the existing promises (not re-spawning them), preserves order, and — crucially under
    /// ASan — that the user-held source promises are NOT consumed/freed (the result is a deep copy)
    /// so a subsequent independent release of each source value is not a double-free.
    #[test]
    fn parallel_awaits_already_spawned_promises() {
        unsafe {
            // Two resolved promises (no worker thread; degenerate settled state) holding Int64s,
            // plus a passthrough scalar element, to exercise both new dispatch branches.
            let v1 = alloc_tagged(TAG_INT64, 1u64);
            let v2 = alloc_tagged(TAG_INT64, 2u64);
            let p1 = lin_make_promise(v1); // promise owns v1
            let p2 = lin_make_promise(v2); // promise owns v2
            let boxed_p1 = lin_box_promise(p1); // TAG_PROMISE box (the value the "user" holds)
            let boxed_p2 = lin_box_promise(p2);
            assert_eq!(lin_get_tag(boxed_p1), TAG_PROMISE);

            let tasks = lin_array_alloc(4);
            lin_array_push_tagged(tasks, boxed_p1 as *const u8);
            lin_array_push_tagged(tasks, boxed_p2 as *const u8);
            // Passthrough: a plain scalar element flows through unchanged.
            let v3 = alloc_tagged(TAG_INT64, 3u64);
            lin_array_push_tagged(tasks, v3 as *const u8);

            let out = lin_parallel(tasks as *mut u8) as *mut crate::array::LinArray;
            assert_eq!(lin_array_length(out), 3);
            let e0 = lin_array_get(out, 0) as *const TaggedVal;
            let e1 = lin_array_get(out, 1) as *const TaggedVal;
            let e2 = lin_array_get(out, 2) as *const TaggedVal;
            assert_eq!(lin_unbox_int64(e0 as *const u8), 1);
            assert_eq!(lin_unbox_int64(e1 as *const u8), 2);
            assert_eq!(lin_unbox_int64(e2 as *const u8), 3);

            // The source promises must still own their original resolved values (NOT consumed by
            // parallel). Awaiting them again must yield the same values with no double-free — this
            // is the ASan-sensitive assertion.
            let again1 = lin_await_promise(p1);
            let again2 = lin_await_promise(p2);
            assert_eq!(lin_unbox_int64(again1), 1);
            assert_eq!(lin_unbox_int64(again2), 2);

            // Clean up everything; under ASan a double-free here would abort.
            lin_tagged_release(again1);
            lin_tagged_release(again2);
            crate::array::lin_array_release(out);
            crate::array::lin_array_release(tasks);
            crate::tagged::lin_tagged_free_box(boxed_p1);
            crate::tagged::lin_tagged_free_box(boxed_p2);
        }
    }
}
