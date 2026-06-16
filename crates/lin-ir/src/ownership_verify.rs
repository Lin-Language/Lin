//! Ownership conventions as a verified IR fact (Path-10/11 Leg 1) — inference + SHADOW-MODE verifier.
//!
//! This module makes ownership an explicit, checkable property of the flat IR rather than an
//! emergent side effect of scattered `Retain`/`Release` emission. It has three parts:
//!
//!   1. [`intrinsic_conventions`] — the **hand-audited intrinsic convention table**. For every
//!      `lin_*` runtime intrinsic it declares the ownership convention of each parameter and the
//!      return. This is the load-bearing table the brief warns about (a wrong entry would be a
//!      miscompile if Wave 2 ever *consumes* it). It lives here, not in `lin-codegen`, because
//!      `lin-ir` cannot depend on `lin-codegen` (the dependency runs the other way) — so the table
//!      must sit in a crate codegen can read. `lin-codegen/src/codegen/runtime.rs` documents and
//!      cross-checks it next to the matching LLVM `declare`s. THIS ROUND it is only cross-checked,
//!      never consumed.
//!
//!   2. [`infer_conventions`] — fills each `LinFunction`'s `param_conventions` / `ret_convention`.
//!      A parameter is `Borrow` when its carry class is read-only and never escapes, `Inout` when
//!      mutated in place but not escaping, and `Own` (today's behaviour) when stored / captured /
//!      returned / passed to a consuming position, or on ANY doubt. Populating these fields is pure
//!      data — codegen ignores them this round — so it is a zero-behaviour-change operation.
//!
//!   3. [`verify_module`] — the SHADOW-MODE verifier. It walks every function's blocks and call
//!      edges and reports invariant violations WITHOUT changing anything. The checks target the
//!      project's recurring RC bug class:
//!        - **RC emitted into an unreachable block** (the `tco_post` class: releases emitted into a
//!          dead continuation after a diverging `TailCall` never run — `b2e6d35`, `9a1a735`).
//!        - **double-release / release-then-use** on a straight-line path (the union-boundary
//!          double-free class).
//!        - **owned value never released and never escaping** (a leak).
//!        - **intrinsic-table consistency** (every `CallIntrinsic` has a table entry; a missing one
//!          is a gap that would block Wave 2).
//!
//! Soundness note: the verifier is REPORT-ONLY. It can have false positives (it is an
//! over-approximation of a hard dataflow question) — every reported violation must be triaged into
//! (a) a real latent bug or (b) a wrong/over-conservative inference. That triage is the whole point
//! of shadow mode.

use std::collections::{HashMap, HashSet, VecDeque};

use lin_check::types::Type;

use crate::carry::{classify_carry_edges, classify_tailcall_carry, UnionFind};
use crate::ir::*;
use crate::liveness::instr_use_def;

// ===========================================================================
// 1. The hand-audited intrinsic convention table
// ===========================================================================

/// The ownership convention of a runtime intrinsic: one convention per declared parameter, plus the
/// return convention. `params` is positional and matches the order the lowerer emits arguments in
/// the `CallIntrinsic { args }` vector (which is the same order codegen passes them to the `lin_*`
/// C-ABI symbol — see `RuntimeFns` in `codegen/runtime.rs`).
#[derive(Debug, Clone)]
pub struct IntrinsicConv {
    pub params: Vec<Convention>,
    pub ret: Convention,
}

impl IntrinsicConv {
    fn new(params: Vec<Convention>, ret: Convention) -> Self {
        IntrinsicConv { params, ret }
    }
}

use Convention::{Borrow, Inout, Own};

/// THE hand-audited table. The match is now EXHAUSTIVE over `Intrinsic` — every runtime intrinsic
/// (including the previously-un-audited async / worker / Shared / Stream families and `FromJson`)
/// has an individually-audited convention. The signature stays `-> Option<…>` so the verifier's gap
/// enumeration and any Wave-2 consumer keep their existing shape, but it never returns `None` today.
/// A NEW intrinsic added to the enum is a compile error here (non-exhaustive match), forcing its
/// convention to be audited up-front rather than silently degrading into an unaudited gap. The
/// verifier's "un-audited intrinsic" gap check therefore now reports ZERO gaps over the corpus.
///
/// Each audited entry cites WHY, grounded in the runtime semantics in `lin-runtime/src/` and the
/// `RuntimeFns` declarations. The recurring question is "does this symbol store/retain the pointer
/// (→ caller transfers ownership: `Own`), only read it (→ `Borrow`), or mutate it in place
/// (→ `Inout`)?" and "does it hand back a fresh +1 (→ ret `Own`) or a borrowed interior pointer
/// (→ ret `Borrow`)?"
pub fn intrinsic_conventions(intr: &Intrinsic) -> Option<IntrinsicConv> {
    use Intrinsic::*;
    let conv = match intr {
        // ---- Pure reads of a heap value, returning a scalar / borrowed interior ----
        // lin_object_get(obj, key) -> *TaggedVal: reads `obj`, returns a BORROWED pointer INTO the
        // container (not a fresh +1 — this is exactly why the lowerer must clone before it escapes;
        // see `own_for_read`). Both operands are read-only. CONFIDENT.
        ObjectGet => IntrinsicConv::new(vec![Borrow, Borrow], Borrow),
        // lin_object_has(obj, key) -> bool: read-only membership test. CONFIDENT.
        ObjectHas => IntrinsicConv::new(vec![Borrow, Borrow], Own),
        // lin_object_eq(a, b) -> i8: deep structural equality, reads both. ret is a scalar bool
        // (Own is the scalar default — a scalar has no ownership, Own is harmless). CONFIDENT.
        ObjectEq => IntrinsicConv::new(vec![Borrow, Borrow], Own),
        // lin_string_eq(a, b) -> bool: reads both strings. CONFIDENT.
        StringEq => IntrinsicConv::new(vec![Borrow, Borrow], Own),
        // lin_string_length(s) -> i32 / lin_array_length, lin_length: read length, scalar out.
        StringLength | ArrayLength | Length => IntrinsicConv::new(vec![Borrow], Own),
        // lin_array_get(arr, i) -> *elem: reads the array, returns a BORROWED element box (the
        // `lin_array_get`, NOT the `_tagged` fresh-+1 variant). i is a scalar. CONFIDENT for the
        // borrowed-get; the lowerer separately handles the fresh-+1 tagged variant via its own RC.
        ArrayGet => IntrinsicConv::new(vec![Borrow, Own], Borrow),
        // lin_get_tag(v) -> u8: reads the tag byte of a boxed value. scalar out. CONFIDENT.
        GetTag => IntrinsicConv::new(vec![Borrow], Own),
        // Unbox family: read the box, return the inner scalar (Int/Float/Bool) or a BORROWED inner
        // pointer (UnboxPtr). For scalar unboxes the inner is a value copy → ret Own (harmless
        // scalar). UnboxPtr returns the inner heap pointer WITHOUT retaining → Borrow. CONFIDENT.
        UnboxInt32 | UnboxInt64 | UnboxFloat64 | UnboxBool => IntrinsicConv::new(vec![Borrow], Own),
        UnboxPtr => IntrinsicConv::new(vec![Borrow], Borrow),
        // is-type / pattern tests: read the (boxed) value, scalar bool out. CONFIDENT.
        // (These appear as their own IR opcodes IsType/HasPattern/etc., not CallIntrinsic — listed
        // here only for completeness if ever routed through the intrinsic path.)

        // ---- to_string family: read input, return a FRESH LinString (+1) ----
        // lin_int_to_string / float / bool / null: take a scalar, return a fresh string the caller
        // owns. lin_tagged_to_string(v): reads the boxed value, returns a fresh string. CONFIDENT.
        IntToString | FloatToString | BoolToString => IntrinsicConv::new(vec![Borrow], Own),
        NullToString => IntrinsicConv::new(vec![], Own),
        ToString | TaggedToString => IntrinsicConv::new(vec![Borrow], Own),
        // lin_print(s) -> void: reads (prints) the string, does NOT store it. CONFIDENT borrow —
        // this is the single most common case where today's lowering over-owns (it currently
        // materializes/releases a temp the runtime only reads).
        Print => IntrinsicConv::new(vec![Borrow], Own),

        // ---- Allocators: take inputs, return a fresh +1 ----
        // lin_string_concat / lin_array concat: read inputs, return a FRESH heap value. The inputs
        // are NOT consumed (the runtime copies). CONFIDENT borrow-in / own-out.
        StringConcat => IntrinsicConv::new(vec![Borrow, Borrow], Own),
        Concat => IntrinsicConv::new(vec![Borrow, Borrow], Own),
        // lin_array_alloc(cap) / lin_object_alloc(cap) / lin_map_alloc(cap): scalar in, fresh +1 out.
        ArrayAlloc | ObjectAlloc => IntrinsicConv::new(vec![Own], Own),
        // FlatArrayAlloc(kind)(cap) -> fresh array.
        FlatArrayAlloc(_) => IntrinsicConv::new(vec![Own], Own),
        // ArrayAllocate / ArrayAllocateFilled: size (+ fill) -> fresh array. CONFIRMED on the fill
        // (was UNSURE): codegen's `ArrayAllocateFilled` arm (intrinsics.rs ~1114-1146) emits one
        // `lin_tagged_retain(fill)` per slot before each `lin_array_set` for a heap-payload fill, and
        // documents verbatim "the caller's original borrowed reference is left intact for its own
        // scope" — so every slot gets its own +1 and the fill is BORROWED, never consumed. A flat
        // scalar fill carries no heap payload → no retain, still Borrow. ret is the fresh +1 array.
        ArrayAllocate => IntrinsicConv::new(vec![Own], Own),
        ArrayAllocateFilled => IntrinsicConv::new(vec![Own, Borrow], Own),
        // lin_alloc(size) -> raw buffer: scalar in, fresh out.
        Alloc => IntrinsicConv::new(vec![Own], Own),

        // ---- In-place container mutation: receiver is INOUT, stored value is OWN ----
        // lin_push(arr, v) / lin_array_push: the array is mutated in place (Inout — NOT consumed,
        // NOT a fresh result), the pushed value's reference is TRANSFERRED into the slot (Own). ret
        // is void/the array. CONFIDENT — this is the canonical `(inout, own)` from the brief.
        Push | ArrayPush => IntrinsicConv::new(vec![Inout, Own], Own),
        FlatArrayPush(_) => IntrinsicConv::new(vec![Inout, Own], Own),
        // lin_object_set(obj, key, val): obj mutated in place (Inout); key + val transferred into
        // the entry (Own — the runtime retains/stores them). CONFIDENT.
        ObjectSet | ObjectSetDyn => IntrinsicConv::new(vec![Inout, Own, Own], Own),
        // lin_map_set(map, key, val): same shape as object_set. CONFIDENT.
        // (Map ops are routed via Index/IndexSet opcodes today, but list for completeness.)
        // lin_array set-by-index: arr inout, value transferred.
        ArraySetDyn => IntrinsicConv::new(vec![Inout, Own, Own], Own),
        // FlatArrayGet(kind)(arr, i) -> scalar: reads, scalar out.
        FlatArrayGet(_) => IntrinsicConv::new(vec![Borrow, Own], Own),

        // ---- keys / values / introspection: read, return fresh +1 ----
        // lin_keys(obj) -> fresh String[]: reads obj, returns a fresh array. CONFIDENT.
        Keys => IntrinsicConv::new(vec![Borrow], Own),
        // ValueKey(v): canonicalize a value to a string key. CONFIRMED (was UNSURE): `lin_value_key`
        // (string.rs:804) only READS the tagged value via `tagged_to_key_string` and builds a FRESH
        // `LinString` with `lin_string_from_bytes` — input borrowed, ret fresh +1 (Own).
        ValueKey => IntrinsicConv::new(vec![Borrow], Own),
        // ToJson(v): structural serialization. CONFIRMED (was UNSURE — the input is NEVER shared):
        // `lin_to_json` (string.rs:1294) walks the value into a NEW `String` (`push_json_value`) and
        // returns a fresh `LinString` (`lin_string_from_bytes`) — input borrowed, ret fresh +1 (Own).
        ToJson => IntrinsicConv::new(vec![Borrow], Own),

        // ---- Box family: wrap a value, return a fresh box ----
        // lin_box_int32(x) etc.: scalar in, fresh box out. The pointer-wrapping boxes
        // (BoxStr/BoxObject/BoxArray/BoxFunction) wrap the inner WITHOUT bumping its rc — the box
        // BORROWS the inner (the lowerer tracks the inner's ownership separately; see the Coerce
        // widen path + `record_escape_alias`). So the arg is Borrow and the box is a fresh shell
        // (+1 shell, borrowed inner). CONFIDENT on the read-only-arg; ret is the fresh shell (Own).
        BoxNull => IntrinsicConv::new(vec![], Own),
        BoxBool | BoxInt32 | BoxInt64 | BoxFloat64 => IntrinsicConv::new(vec![Borrow], Own),
        BoxStr | BoxObject | BoxArray | BoxFunction => IntrinsicConv::new(vec![Borrow], Own),

        // ---- Release/free helpers (lowerer-internal, not user calls) ----
        StringRelease | ArrayRelease => IntrinsicConv::new(vec![Own], Own),

        // ---- Panic / exit ----
        Panic => IntrinsicConv::new(vec![Borrow], Own),
        Exit => IntrinsicConv::new(vec![Own], Own),

        // =====================================================================================
        // Async / concurrency family (std/async — runtime in `async_rt.rs`). RECURRING SHAPE: the
        // runtime READS each closure/promise/array arg (it deep-COPIES the captured env or
        // deep-CLONES values for cross-thread transfer — it never frees the caller's arg), and
        // returns a FRESH +1 result (a `LinPromise`/`LinArray`/handle, boxed by codegen). So the
        // family is overwhelmingly Borrow-in / Own-out. The two exceptions are documented inline:
        // `Worker` (RETAINS its handler closures → Own) and `StreamPromise` (MOVES its stream → Own).
        // =====================================================================================

        // async(thunk) → lin_async_spawn(thunk): reads the thunk's fn/env/cap fields and deep-COPIES
        // the env for the worker (`transfer_clone_env`); the thunk closure itself is NOT freed. The
        // `pool.async(f)` 2-arg form is lin_pool_async_one(pool, thunk): pool read (Borrow), thunk
        // read+env-copied (Borrow). ret = a fresh LinPromise (boxed → Own). CONFIDENT (async_rt.rs
        // :202 lin_async_spawn, :653 lin_pool_async_one). A scalar pad in the 1-arg form is Borrow-safe.
        Async => IntrinsicConv::new(vec![Borrow, Borrow], Own),
        // await(promise) → lin_await_promise(promise): reads the promise, joins its thread, and hands
        // the resolved TaggedVal* OUT (ownership transfers to caller). The promise itself is consumed
        // logically (its join handle is taken) but its box is freed by the caller's scope, not here —
        // so at the pointer level it is read (Borrow); ret = the owned result (Own). CONFIDENT
        // (async_rt.rs:312).
        Await => IntrinsicConv::new(vec![Borrow], Own),
        // parallel(tasks) → lin_parallel(tasks): reads the (unboxed) task array, spawning thunks /
        // awaiting promises and DEEP-COPYING each result into a FRESH result array — the source array
        // and its elements are borrowed (never consumed). ret = fresh +1 array (Own). CONFIDENT
        // (async_rt.rs:511 — "tasks is the raw array … ownership of the result array transfers to the
        // caller").
        Parallel => IntrinsicConv::new(vec![Borrow], Own),
        // race(promises) → lin_race(promises): reads the promise array, DEEP-CLONES the winning value
        // (`lin_transfer_clone`) so the result is independent of the still-live source promises — the
        // array is borrowed. ret = fresh promise (boxed → Own). CONFIDENT (async_rt.rs:377).
        Race => IntrinsicConv::new(vec![Borrow], Own),
        // timeout(promise, ms) → lin_timeout(promise, ms): polls the promise, DEEP-CLONES its value
        // into a fresh settled promise — promise borrowed, ms scalar. ret = fresh promise (Own).
        // CONFIDENT (async_rt.rs:409).
        Timeout => IntrinsicConv::new(vec![Borrow, Own], Own),
        // retry(thunk, n) → lin_retry(thunk, n): re-spawns the SAME thunk up to n times (reading it
        // each attempt — never freeing it), n scalar. ret = fresh settled promise (Own). CONFIDENT
        // (async_rt.rs:438).
        Retry => IntrinsicConv::new(vec![Borrow, Own], Own),
        // threadPool(n) → lin_thread_pool_new(n): scalar in, fresh *LinThreadPool out (boxed handle →
        // Own). CONFIDENT (async_rt.rs:632).
        ThreadPool => IntrinsicConv::new(vec![Own], Own),
        // worker(handler, onClose) → lin_worker_new(…): EXCEPTION — the runtime takes an OWNING
        // reference to BOTH closure structs (`lin_rc_retain` on on_msg_cls / on_close_cls), holding
        // them for the worker thread's whole lifetime and releasing them in `lin_worker_close`. This
        // is the textbook Own (ownership transferred into the worker). ret = fresh *LinWorker (boxed
        // handle → Own). CONFIDENT (async_rt.rs:717 lin_worker_new + :829 lin_worker_close). NOTE: the
        // IR `args` are the 2 closure VALUES (handler, onClose); codegen later explodes each into
        // fn/env/has/cls C-ABI args, but the table is positional over the IR `args` vector.
        Worker => IntrinsicConv::new(vec![Own, Own], Own),
        // serve(handler, port) → lin_serve(…): reads the handler closure (invoked per request; blocks
        // forever, never returns), port scalar. ret = Null. The handler is borrowed (the call never
        // returns to free anything; treat as read). CONFIDENT (intrinsics.rs:512; server.rs lin_serve).
        Serve => IntrinsicConv::new(vec![Borrow, Own], Own),
        // shared(v) → lin_shared_new(v): DEEP-CLONES v into a private box (`lin_transfer_clone`) — v
        // borrowed; ret = fresh boxed Shared (Own). CONFIDENT (shared.rs:61).
        SharedNew => IntrinsicConv::new(vec![Borrow], Own),
        // get(s) → lin_shared_get(s): reads the Shared handle under a read-lock and DEEP-CLONES a
        // snapshot OUT — handle borrowed; ret = fresh snapshot (Own). CONFIDENT (shared.rs:73).
        SharedGet => IntrinsicConv::new(vec![Borrow], Own),
        // set(s, v) → lin_shared_set(s, v): reads the handle, DEEP-CLONES v into the box's slot
        // (releasing the OLD inner internally — not the arg) — handle + v both borrowed; ret = Null.
        // CONFIDENT (shared.rs:86).
        SharedSet => IntrinsicConv::new(vec![Borrow, Borrow], Own),
        // withLock(s, f) → lin_shared_with_lock(s, f): reads the handle + the closure f (called under
        // the write-lock; never freed), DEEP-CLONES f's result OUT — both borrowed; ret fresh (Own).
        // CONFIDENT (shared.rs:112).
        SharedWithLock => IntrinsicConv::new(vec![Borrow, Borrow], Own),
        // frozen(v) → lin_freeze(v): SEALS the graph rooted at v IN PLACE (immortalizes its rc) and
        // returns the SAME pointer v unchanged — not a fresh +1. So v is mutated-in-place (Inout) and
        // the result is the borrowed-through same value (ret Borrow). CONFIDENT (frozen.rs:79 returns
        // `v` as-is; intrinsics.rs:584 returns the original `v`, freeing only a transient box shell).
        Freeze => IntrinsicConv::new(vec![Inout], Borrow),
        // w.request(w, msg) → lin_worker_request(w, msg): reads the worker handle, DEEP-CLONES msg for
        // transfer (`lin_transfer_clone`) — both borrowed; ret = the reply (Own). CONFIDENT
        // (async_rt.rs:799).
        Request => IntrinsicConv::new(vec![Borrow, Borrow], Own),
        // w.message(w, msg) → lin_worker_message(w, msg): reads the worker, DEEP-CLONES msg — both
        // borrowed; ret = Null (void). CONFIDENT (async_rt.rs:818).
        Message => IntrinsicConv::new(vec![Borrow, Borrow], Own),
        // w.close(w) → lin_worker_close(w): reads the worker handle, releases the closures it RETAINED
        // in worker_new (internal balance, not the arg) — handle borrowed; ret = Null. CONFIDENT
        // (async_rt.rs:829).
        Close => IntrinsicConv::new(vec![Borrow], Own),

        // =====================================================================================
        // Stream<T> family (std/stream, std/fs, std/compress, std/archive — runtime in `stream.rs`).
        // RECURRING SHAPE confirmed for EVERY adapter + terminal: the upstream stream is taken via
        // `own_upstream` which RETAINS its own +1 (`lin_stream_retain_box`) — the caller keeps its
        // reference, released at its own scope exit (stream.rs:1686 "the affine check makes the
        // double-use a compile-time error, but the runtime RC stays balanced either way"). Closures
        // are taken via `retain_closure` (own +1, caller keeps its ref). Boxed value args
        // (sep / b / value / arr / init) are taken via `lin_tagged_clone` (own copy, caller keeps its
        // ref). Terminals `unwrap_stream` + `close_box` but do NOT release the caller's outer box.
        // => Streams, closures, and cloned value-args are all BORROW; adapters/terminals return a
        // FRESH +1 (a new Stream box, or a boxed result/error union) = Own. EXCEPTIONS: `StreamReduce`'s
        // INIT accumulator is CONSUMED (Own), and `StreamPromise` MOVES its stream (Own). Both inline.
        // =====================================================================================

        // open/read/close (StreamOpen=lin_fs_open(path), StreamRead=lin_stream_read(s),
        // StreamClose=lin_stream_close(s)): each reads its single arg (path String / stream box) and
        // returns a fresh boxed value (Stream|Error / chunk-or-EOF-or-Error union / Null). The stream
        // box is borrowed; close is idempotent and does not free the caller's box. CONFIDENT
        // (stream.rs:409, :1654, :1677).
        StreamOpen => IntrinsicConv::new(vec![Borrow], Own),
        StreamRead | StreamClose => IntrinsicConv::new(vec![Borrow], Own),
        // Single-stream-arg lazy adapters → fresh Stream. All `own_upstream(s)` (retain → Borrow s).
        // flatten/pairwise/dedup/manifest/files take only the stream; the gzip/inflate/deflate codecs
        // likewise. CONFIDENT (stream.rs:1766/1789/1805/2635/2753/1857/1869/1881/1893).
        StreamFlatten | StreamPairwise | StreamDedup | StreamManifest | StreamFiles
        | StreamGunzip | StreamGzip | StreamInflate | StreamDeflate => IntrinsicConv::new(vec![Borrow], Own),
        // (stream, closure) lazy adapters: map/filter/takeWhile/dropWhile/flatMap. own_upstream(s) +
        // retain_closure(f) → both Borrow; fresh Stream out. CONFIDENT (stream.rs:1712/1720/1742/1750/1758).
        StreamMap | StreamFilter | StreamTakeWhile | StreamDropWhile | StreamFlatMap => {
            IntrinsicConv::new(vec![Borrow, Borrow], Own)
        }
        // (stream, i64) lazy adapters: take/drop/lines/chunks/sliding. own_upstream(s) → Borrow s;
        // count/size scalar (Own-neutral). Fresh Stream out. CONFIDENT (stream.rs:1728/1735/1905/1914/1781).
        StreamTake | StreamDrop | StreamLines | StreamChunks | StreamSliding => {
            IntrinsicConv::new(vec![Borrow, Own], Own)
        }
        // (stream, path) lazy SINKS: writeStream/writeLines. own_upstream(s) → Borrow s; path String
        // is read (resolve_lin_str, Borrow). Fresh Stream out. CONFIDENT (stream.rs:1925/1935).
        StreamWrite | StreamWriteLines => IntrinsicConv::new(vec![Borrow, Borrow], Own),
        // (stream, boxed-value) adapter: intersperse(s, sep). own_upstream(s) → Borrow; sep taken via
        // lin_tagged_clone (own copy) → Borrow. Fresh Stream out. CONFIDENT (stream.rs:1797).
        StreamIntersperse => IntrinsicConv::new(vec![Borrow, Borrow], Own),
        // zipWith(s, b, f): own_upstream(s) → Borrow; b cloned (Borrow); retain_closure(f) → Borrow.
        // Fresh Stream out. CONFIDENT (stream.rs:1813).
        StreamZipWith => IntrinsicConv::new(vec![Borrow, Borrow, Borrow], Own),
        // concat(a, b): BOTH streams own_upstream'd (each retained) → both Borrow. Fresh Stream out.
        // CONFIDENT (stream.rs:1773).
        StreamConcat => IntrinsicConv::new(vec![Borrow, Borrow], Own),
        // SOURCE adapters (no upstream): count(start, step) scalars; repeat(value, n) clones value
        // (Borrow) + scalar n; cycle(arr) clones arr (Borrow). Fresh Stream out. CONFIDENT
        // (stream.rs:1828/1835/1844).
        StreamCount => IntrinsicConv::new(vec![Own, Own], Own),
        StreamRepeat => IntrinsicConv::new(vec![Borrow, Own], Own),
        StreamCycle => IntrinsicConv::new(vec![Borrow], Own),
        // Unified OS sources: tcp(fd)/stdout(handle)/stdin(): scalar/no args → fresh Stream (Own).
        // CONFIDENT (stream.rs:388/394/400).
        StreamTcp | StreamStdout => IntrinsicConv::new(vec![Own], Own),
        StreamStdin => IntrinsicConv::new(vec![], Own),
        // Single-stream-arg TERMINALS: drain/collect/readText. unwrap_stream(s) + close_box(s) — the
        // caller's outer box is NOT released here (it owns its own ref). s Borrow; fresh boxed result
        // (Null|Error / UInt8[]|Error / String|Error) out (Own). CONFIDENT (stream.rs:1946/2229/2255).
        StreamDrain | StreamCollect | StreamReadText => IntrinsicConv::new(vec![Borrow], Own),
        // (stream, closure) TERMINALS: for/find/some/every/while/untar. retain_closure(body) → Borrow;
        // s Borrow (terminal close_box doesn't free the caller's box). Fresh boxed result out (Own).
        // CONFIDENT (stream.rs:1978/2059/2098/2137/2177/2508).
        StreamFor | StreamFind | StreamSome | StreamEvery | StreamWhile | StreamUntar => {
            IntrinsicConv::new(vec![Borrow, Borrow], Own)
        }
        // entries(s) (lin_stream_tar_entries): single-stream-arg lazy adapter, same shape as
        // manifest/files — own_upstream(s) retains → Borrow s; fresh Stream<TarEntry> out (Own).
        // CONFIDENT (stream.rs lin_stream_tar_entries: own_upstream + StreamBox::new_boxed).
        StreamTarEntries => IntrinsicConv::new(vec![Borrow], Own),
        // header(e) (lin_tar_header): reads the TarEntryBox's COPIED metadata (no retain/release of
        // the entry box) → e Borrow; returns a FRESH LinObject* at rc=1 (make_meta_object_unboxed)
        // → Own. body(e) (lin_tar_body): reads the entry's generation/body_taken and clones the
        // internal Arc (no entry-box RC change) → e Borrow; mints a fresh StreamBox (rc=1) → Own.
        // CONFIDENT (stream.rs lin_tar_header/lin_tar_body).
        TarHeader | TarBody => IntrinsicConv::new(vec![Borrow], Own),
        // reduce(s, init, f) TERMINAL: EXCEPTION — `init` arrives as an OWNED +1 and the runtime takes
        // ownership of the running accumulator, releasing the previous one each step and on error
        // (stream.rs:2013 "the caller hands us an owned +1 ref … we own the running accumulator").
        // So init = Own (CONSUMED). s Borrow; retain_closure(f) → Borrow. Fresh boxed U|Error out (Own).
        // NOTE: the lowerer does NOT `unregister_owned` the init temp — it relies on init being a fresh
        // transient box (a scalar boxed by codegen, or a fresh-alloc) so there is no scope-registered
        // owner to double-free. See FINDINGS: a NON-fresh heap init (e.g. an owned `var` passed to
        // reduce) would be released by BOTH the runtime and the caller's scope. CONFIDENT on the
        // convention; the double-free risk is a SEPARATE codegen question, not a table error.
        StreamReduce => IntrinsicConv::new(vec![Borrow, Own, Borrow], Own),
        // .promise() (StreamPromise=lin_stream_promise(s)): EXCEPTION — MOVES the stream onto a worker
        // thread; the lowerer `unregister_owned`s the stream arg (lower.rs:4807) and the worker drives
        // + releases it via `lin_stream_drive_owned` (stream.rs:2222 lin_tagged_release(s)). So the
        // stream is CONSUMED (Own); ret = fresh Promise (boxed → Own). CONFIDENT (async_rt.rs:279).
        StreamPromise => IntrinsicConv::new(vec![Own], Own),

        // fromJson(value) → lin_from_json(value, desc): the runtime BORROWS the input (decode.rs:289
        // "the input is borrowed (never consumed)") and returns either the same pointer cloned (+1) on
        // success or a fresh Error — unconditionally a fresh +1 union (Own). The IR `args` is `[value]`
        // only (the schema descriptor is a codegen-built static global, NOT an IR arg). CONFIDENT
        // (lower.rs:5844 lower_from_json, decode.rs:289).
        FromJson { .. } => IntrinsicConv::new(vec![Borrow], Own),

        // The match is now EXHAUSTIVE over `Intrinsic` — every intrinsic is individually audited, so
        // there is no longer a conservative all-Own blanket / gap fall-through. The return type stays
        // `Option` so the verifier's gap-enumeration (and Wave-2 consumers) keep their existing shape,
        // but in practice this never returns `None`. Deliberately NO `_ =>` arm: a NEW intrinsic added
        // later is then a COMPILE ERROR here (non-exhaustive match), forcing its convention to be
        // audited at the point it is introduced rather than silently degrading to an unaudited gap.
    };
    Some(conv)
}

/// The ownership convention of the VALUE PRODUCED by an `Index` (`obj[key]`) operation, decided from
/// the container + key types. This is the ownership authority for the projection-result lifetime — it
/// answers the single question the lowerer's RC insertion needs: does codegen hand back a FRESH +1 box
/// the binding already owns (`Own`), or a BORROWED interior pointer into the (movable) container that
/// must be relocated off its slot before it escapes (`Borrow`)?
///
/// - `Own` (fresh +1): the array / fixed-array path (`lin_array_get_tagged` allocates a standalone
///   `TaggedVal` — for a flat array it boxes the scalar, for a tagged array it copies the element box),
///   and any NUMERIC-keyed container projection (codegen materializes + clones into a fresh box). In
///   both cases `dst` is already an owned, container-independent value, so the union-relocation
///   `CloneBox` (or the sealed/scalar retain) must be SKIPPED — cloning a fresh box leaks the original
///   once per evaluation (the dominant per-scanned-stop leak in `routeScanner.scanBack`'s
///   `routeTrips[i]`).
/// - `Borrow` (interior pointer): the object / `Map` path (`lin_object_get` returns a `*TaggedVal`
///   INTO the container — exactly the `ObjectGet` return `Borrow` in the table above). The value MUST
///   be relocated off the slot (`CloneBox` / retain) before it escapes, or it dangles when the
///   container grows/moves.
///
/// Relocated verbatim (same predicate, expressed in `Convention` terms) from the former
/// `index_result_is_fresh_owned_box` heuristic in `lower.rs`, so the fresh-vs-borrowed projection
/// fact now lives in the ownership authority alongside the intrinsic table it mirrors
/// (`ArrayGet` ret `Borrow` for the plain get, fresh +1 for the `_tagged` variant), rather than being
/// re-derived inline at the RC-insertion site.
pub fn index_result_convention(obj_ty: &Type, key_ty: &Type) -> Convention {
    if matches!(obj_ty, Type::Map { .. }) {
        return Borrow;
    }
    if matches!(obj_ty, Type::Array(_) | Type::FixedArray(_)) || key_ty.is_numeric() {
        Own
    } else {
        Borrow
    }
}

/// How codegen takes an INDEPENDENT owning reference to a value of a given slot/cell type — the
/// single decision the lowerer's owning-read (`own_for_read`) and owning-store (`own_for_store`)
/// model needs, and which today each re-derives inline from the type shape. A `var` cell / module
/// global / scope-exit register owns ONE reference to its value; producing that reference depends
/// only on the value's repr kind:
///
/// - `Clone` — a boxed Json/union value (`is_union_ty`: `Union`/`TypeVar`/`Named`/`Shared`/`Stream`/
///   `Promise`). The owner must own its OWN `TaggedVal*` box, not an alias of a borrowed caller box,
///   so codegen emits `CloneBox` (→ `lin_tagged_clone`); release-old can then free it safely.
/// - `Retain` — a concrete refcounted heap value (`is_rc_type`: `Str`/`StrLit`/`Array`/`FixedArray`/
///   `Object`/`Map`/`Iterator`/`Function`). The owner shares the same heap pointer with one extra
///   reference, so codegen emits `Retain` (rc + 1) in place.
/// - `Trivial` — a scalar (`Int`/`Bool`/`Float`/`Null`/sum-node/…): no heap reference exists, so
///   owning is a no-op and the value is used unchanged.
///
/// Relocated verbatim (same `is_union_ty` / `is_rc_type` trichotomy, in the same priority order)
/// from the inline classification in `own_for_read` / `own_for_store` in `lower.rs`, so the
/// "how do I own a value of this type" fact lives in the ownership authority instead of being
/// re-derived at each RC-insertion site. The lowerer reads this and emits the matching op, so the
/// produced IR — and therefore the RC — is byte-identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwningStrategy {
    /// Boxed union/Json value: own via `CloneBox`.
    Clone,
    /// Concrete refcounted heap value: own via `Retain` (rc + 1) in place.
    Retain,
    /// Scalar: no heap reference; owning is a no-op.
    Trivial,
}

/// THE authority for `OwningStrategy` — see the enum doc. Mirrors `lower::own_for_read` /
/// `lower::own_for_store`: union check FIRST (the two type sets are disjoint, so priority is not
/// load-bearing, but it is preserved verbatim), then concrete-rc, else trivial.
pub fn owning_strategy(ty: &Type) -> OwningStrategy {
    if is_union_owning_ty(ty) {
        OwningStrategy::Clone
    } else if is_concrete_rc_ty(ty) {
        OwningStrategy::Retain
    } else {
        OwningStrategy::Trivial
    }
}

/// Boxed Json/union value types (the `CloneBox` owning set). Kept in the authority alongside
/// `owning_strategy`; mirrors `lower::is_union_ty`. Stage 3: NullableRecord unions are EXCLUDED
/// (they use Retain, not Clone — a raw nullable ptr, not a TaggedVal shell).
fn is_union_owning_ty(ty: &Type) -> bool {
    if crate::repr::nullable_sealed_record(ty).is_some() { return false; }
    matches!(
        ty,
        Type::Union(_) | Type::TypeVar(_) | Type::Named(_) | Type::Shared(_) | Type::Stream(_) | Type::Promise(_) | Type::TarEntry
    )
}

// Delegates to the shared `ir::is_concrete_rc_ty`; the alias keeps call-site names stable.
fn is_concrete_rc_ty(ty: &Type) -> bool {
    crate::ir::is_concrete_rc_ty(ty)
}

/// Whether the box/unbox `Coerce` emitted across the union boundary produces a `dst` that ALIASES the
/// source `arg`'s inner heap payload — i.e. whether `lower::record_escape_alias(dst, arg)` must fire.
/// This is the single ownership fact the lowerer's union-boundary `Coerce` arm needs: when the coerced
/// box and the source share the SAME inner heap pointer, that source must be treated as KEPT whenever
/// the box is threaded into a self-tail-call param slot (releasing it on the back-edge would free the
/// inner pointer the slot now holds → double-free); when they do NOT share an inner pointer, the
/// source is a genuine orphan after the `Coerce` and must be released before the back-edge (aliasing it
/// would leak it every iteration).
///
/// Two directions, exactly one of which applies inside the `is_union_ty(param) != is_union_ty(arg)`
/// arm (the boolean inequality guarantees exactly one operand is a union):
///
/// - **UNBOX** (union `arg` → concrete `param`): alias iff the unboxed payload is a HEAP pointer
///   (`is_rc_type(param)`) — that pointer is what lands in the param slot, so the source box is kept.
///   A SCALAR unbox (`param` not rc) reads the value OUT of the box, leaving the box a genuine orphan
///   → do NOT alias (else a 16-byte `TaggedVal` leaks per loop iteration).
/// - **WIDEN** (concrete `arg` → union `param`): the `box_*` wraps the inner WITHOUT bumping its rc, so
///   the box `dst` aliases `arg`'s inner — alias it so the inner survives a back-edge. EXCEPTION: a
///   SEALED scalar record arg is materialized to a FRESH independent `LinObject` (its heap fields
///   retained), so the box does NOT alias the source struct; the source is a genuine orphan and must be
///   released → do NOT alias (else the whole packed struct + its String/array fields leak per iteration,
///   the RAPTOR `Trip | Null` RANGE-phase scaling leak).
///
/// Relocated VERBATIM from the two inline `record_escape_alias` gates in `lower::lower_coerce_arg`
/// (the box/unbox arm). `is_union_ty` / `is_rc_type` are mirrored here as `is_union_owning_ty` /
/// `is_concrete_rc_ty`; the one predicate that lives only in `lower` — `is_sealed_scalar_repr`, a
/// recursive sealed-field tree that must stay codegen's single source of truth — is passed in by the
/// caller as `arg_is_sealed_scalar_repr` rather than re-derived here (re-deriving it would fork that
/// gate). The lowerer reads this and emits (or skips) the same `record_escape_alias`, so the resulting
/// IR — and therefore the RC — is byte-identical.
pub fn escape_alias_convention(arg_ty: &Type, param_ty: &Type, arg_is_sealed_scalar_repr: bool) -> bool {
    let arg_union = is_union_owning_ty(arg_ty);
    let param_union = is_union_owning_ty(param_ty);
    // UNBOX: union arg → concrete heap param.
    if arg_union && !param_union && is_concrete_rc_ty(param_ty) {
        return true;
    }
    // WIDEN: concrete heap arg → union param, excluding the sealed-record materialize case.
    if !arg_union && param_union && is_concrete_rc_ty(arg_ty) && !arg_is_sealed_scalar_repr {
        return true;
    }
    false
}

/// How the lowerer must balance the refcount of a value being STORED into a container that takes
/// ownership of one reference (an array element, an object field, a `push` / `set`). This is the
/// single ownership fact the lowerer's `transfer_into_container` site needs: when a value flows into
/// a slot, does it flow in BY MOVE (the source already holds the only +1 — drop it from the owning
/// scope so scope-exit does not also free it, the container's drop accounts for it), does it need a
/// fresh +1 RETAIN (a shared/borrowed heap value — so the slot's copy and the original owner each
/// release independently), or is there NOTHING to balance (a scalar, or a retain-semantics union op
/// whose runtime already took its own inner reference)?
///
/// Three outcomes, decided in this priority order (preserved verbatim from the inline gate):
///
/// - **`Nothing`** — either the value is not refcounted (`!needs_owning`: a scalar / sum node),
///   OR it is a UNION element flowing into a RETAIN-semantics op (`is_union && !op_consumes`):
///   `Push` (`lin_push_dyn`) / `object_set` RETAIN the boxed value's inner payload, so the slot
///   gets its own reference and the source box stays owned by its current owner — there is nothing
///   to balance at this site.
/// - **`Transfer`** — the source is a FRESH allocation (`source_is_fresh_alloc`) that already holds
///   the only +1: MOVE it into the slot by un-registering the temp from the owning scope so
///   scope-exit will not double-free it (the container's drop now accounts for that reference).
/// - **`Retain`** — a BORROWED / shared heap value (e.g. a `LocalGet`): take an independent +1 so
///   the container's copy and the original owner can each release exactly once.
///
/// `op_consumes_union` records whether the container op, for a UNION element, MOVES the box into the
/// slot (raw struct copy, no inner retain — `lin_array_set`) rather than retaining the inner
/// (`Push` / `object_set`). For a CONCRETE rc element every op consumes (codegen never retains a
/// concrete element on insert), so `op_consumes_union` is irrelevant there and the fresh-vs-borrowed
/// split applies regardless.
///
/// Relocated VERBATIM from the inline three-way branch in `lower::transfer_into_container`.
/// `needs_owning` / `is_union_ty` are mirrored here as `needs_owning_insert` (= `is_concrete_rc_ty`
/// ∨ `is_union_owning_ty`) / `is_union_owning_ty`; the one predicate that lives only in `lower` —
/// `expr_is_fresh_alloc`, a recursive walk over the SOURCE AST that has no business in `lin-ir`'s
/// ownership authority — is computed by the caller and passed in as `source_is_fresh_alloc` (exactly
/// as `escape_alias_convention` takes `is_sealed_scalar_repr` by argument). The lowerer reads this
/// and performs the matching action (un-register / `Retain` / nothing), so the resulting IR — and
/// therefore the RC — is byte-identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerInsert {
    /// Nothing to balance: a scalar, or a union element a retain-semantics op already +1'd.
    Nothing,
    /// Fresh +1 source: MOVE it in by un-registering the temp from the owning scope.
    Transfer,
    /// Borrowed/shared heap value: take an independent `Retain` (+1) so each owner frees once.
    Retain,
}

/// THE authority for `ContainerInsert` — see the enum doc. Mirrors `lower::transfer_into_container`
/// branch-for-branch: not-owning OR retain-semantics-union ⇒ `Nothing`; else fresh ⇒ `Transfer`,
/// else borrowed ⇒ `Retain`.
pub fn container_insert_convention(elem_ty: &Type, op_consumes_union: bool, source_is_fresh_alloc: bool) -> ContainerInsert {
    if !needs_owning_insert(elem_ty) {
        return ContainerInsert::Nothing;
    }
    if is_union_owning_ty(elem_ty) && !op_consumes_union {
        // Retain-semantics op (Push / object_set): the runtime took its own inner reference; the
        // source box stays owned by its current owner. Nothing to balance here.
        return ContainerInsert::Nothing;
    }
    if source_is_fresh_alloc {
        ContainerInsert::Transfer
    } else {
        ContainerInsert::Retain
    }
}

/// A type that participates in container-insert ownership balancing — concrete rc OR boxed union.
/// Mirrors `lower::needs_owning` (= `is_rc_type` ∨ `is_union_ty`).
fn needs_owning_insert(ty: &Type) -> bool {
    is_concrete_rc_ty(ty) || is_union_owning_ty(ty)
}

/// Whether widening a value of `value_ty` into a `slot_ty` cell/slot (a `var` init, a captured-heap
/// `CellSet`, a module-global `GlobalValSet`) produces a FRESH, DISTINCT 16-byte `TaggedVal*` shell
/// whose inner payload's ownership lives ELSEWHERE — so the shell must be reclaimed (`FreeBoxShell`
/// → `lin_tagged_free_box`) once its bytes have been move-copied/cloned into the owned store(s).
///
/// This is the single ownership fact the lowerer's box-shell-reclaim sites need. When a CONCRETE
/// value is stored into a UNION/Json slot whose representation differs, `coerce_to_slot_type`
/// allocates a fresh transient box wrapping the raw value; the raw value keeps its OWN +1 (released
/// at scope exit), and the owning-store / owning-read clones produce the slot's and the result's
/// independent references. The transient box is then an ORPHAN: its 16-byte shell is unreferenced,
/// its inner is owned by the raw value's scope-exit release — so freeing ONLY the shell
/// (`lin_tagged_free_box` never touches the inner) exactly balances it, and without it the shell
/// leaks per store (`var x: Json = 1_000_000` / a `var`/global reassignment to a concrete value).
///
/// True iff the slot is a boxed union (`is_union_owning_ty`), the value is NOT already a union (so a
/// new box really is allocated rather than the existing box forwarded), AND their representations
/// differ (`repr_differs`). When the slot is concrete (no union box made) or the value is already a
/// boxed union (the box is a clone/forward owned elsewhere) the result is `false` — nothing to free.
///
/// Relocated VERBATIM from the three identical inline `made_fresh_box` gates that drive the
/// `FreeBoxShell` reclaim in `lower.rs` (`coerce_and_own_store`, the captured-cell `LocalSet`, and
/// the module-global `LocalSet`): `is_union_ty(slot) && !is_union_ty(value) && type_repr_differs(value, slot)`.
/// `is_union_ty` is mirrored here as `is_union_owning_ty`; the one predicate that lives only in
/// `lower` — `type_repr_differs`, a deep recursive walk entangled with `is_sealed_scalar_repr` /
/// `repr::sum_type_eligible` / `param_elem_is_boxed_repr` that must stay codegen's single source of
/// truth — is computed by the caller and passed in as `repr_differs` (exactly as
/// `escape_alias_convention` takes `is_sealed_scalar_repr` and `container_insert_convention` takes
/// `source_is_fresh_alloc` by argument). The lowerer reads this and emits (or skips) the same
/// `FreeBoxShell`, so the resulting IR — and therefore the RC — is byte-identical.
pub fn box_shell_reclaim(value_ty: &Type, slot_ty: &Type, repr_differs: bool) -> bool {
    is_union_owning_ty(slot_ty) && !is_union_owning_ty(value_ty) && repr_differs
}

/// Whether widening a value of `value_ty` into a UNION/Json `slot_ty` slot that a BINDING owns
/// directly (the box IS the value the slot holds — no clone) produces a fresh union box that takes
/// over the source's +1 inner reference, so the lowerer must MOVE that reference: `unregister_owned`
/// the raw source (its single +1 transfers INTO the box) and `register_owned` the box for the
/// binding's scope-exit release.
///
/// This is the BINDING sibling of `box_shell_reclaim` (the CELL/global case). There the slot owns a
/// CLONE of the box, so the transient shell is reclaimed (`FreeBoxShell`) and the raw keeps its own
/// reference. Here the slot owns the box ITSELF, so there is no shell to reclaim — instead the box
/// becomes the union representation and the inner's single +1 is moved from the raw into the box
/// (`coerce_to_slot_type_owning_bind`'s `made_fresh_box` arm: `unregister_owned(raw)` +
/// `register_owned(box)`). Without the move the inner is owned twice (raw scope-exit AND the box's
/// `lin_tagged_release`) → double-free; without the register the box's shell + inner leak.
///
/// True iff the slot is a boxed union (`is_union_owning_ty`), the value is a CONCRETE refcounted heap
/// value (`is_concrete_rc_ty` — this is the load-bearing difference from `box_shell_reclaim`, which
/// also fires on a SCALAR value whose box carries no inner heap +1 to move), the value is NOT already
/// a union (so a new box really is allocated rather than the existing box forwarded), AND their
/// representations differ (`repr_differs`).
///
/// Relocated VERBATIM from the inline `made_fresh_box` gate in `coerce_to_slot_type_owning_bind`:
/// `is_union_ty(slot) && !is_union_ty(value) && is_rc_type(value) && type_repr_differs(value, slot)`.
/// `is_union_ty` / `is_rc_type` are mirrored here as `is_union_owning_ty` / `is_concrete_rc_ty`; the
/// lower-only `type_repr_differs` is computed by the caller and passed in as `repr_differs` (exactly
/// as `box_shell_reclaim` and `escape_alias_convention` take their lower-only predicates by argument).
/// The lowerer reads this and performs the same `unregister_owned`/`register_owned` move, so the
/// resulting IR — and therefore the RC — is byte-identical.
pub fn bound_box_moves_inner(value_ty: &Type, slot_ty: &Type, repr_differs: bool) -> bool {
    is_union_owning_ty(slot_ty) && !is_union_owning_ty(value_ty) && is_concrete_rc_ty(value_ty) && repr_differs
}

// ===========================================================================
// 2. Convention inference at lowering
// ===========================================================================

/// Populate `param_conventions` and `ret_convention` for every function in the module.
/// Pure data: codegen ignores these fields this round, so this changes no output.
pub fn infer_conventions(module: &mut LinModule) {
    for func in &mut module.functions {
        infer_fn(func);
    }
}

/// How one temp is *used* at a single site, for convention inference.
#[derive(Clone, Copy, PartialEq)]
enum UseKind {
    /// Read-only (does not extend the value's lifetime, does not mutate it).
    Read,
    /// Mutated in place (the value's owner still owns it; the callee needs `&mut`).
    InPlace,
    /// Escapes: stored, captured, returned, retained, or passed to a consuming position.
    Escape,
}

fn infer_fn(func: &mut LinFunction) {
    let n = func.temp_count;
    let np = func.params.len();
    if np == 0 {
        func.param_conventions = Vec::new();
        func.ret_convention = Convention::Own;
        return;
    }
    if n == 0 {
        func.param_conventions = vec![Convention::Own; np];
        func.ret_convention = Convention::Own;
        return;
    }

    // Build carry classes (Copy/Bind/Phi/no-op-Coerce + self-TailCall arg↔param), exactly as the
    // escape and repr passes do, so a fact about a class is the join over its members.
    let mut uf = UnionFind::new(n);
    for block in &func.blocks {
        for instr in &block.instructions {
            classify_carry_edges(instr, &mut uf);
        }
        if let Terminator::TailCall { args } = &block.terminator {
            let _ = classify_tailcall_carry(args, &func.params, &mut uf);
        }
    }

    // Per-temp use-kind marks. A temp's strongest mark wins per class: Escape > InPlace > Read.
    let mut escapes = vec![false; n as usize];
    let mut inplace = vec![false; n as usize];
    let mark = |v: &mut Vec<bool>, t: Temp| {
        if (t.0 as usize) < v.len() {
            v[t.0 as usize] = true;
        }
    };

    for block in &func.blocks {
        for instr in &block.instructions {
            classify_uses(instr, &mut |t, kind| match kind {
                UseKind::Escape => mark(&mut escapes, t),
                UseKind::InPlace => mark(&mut inplace, t),
                UseKind::Read => {}
            });
        }
        match &block.terminator {
            Terminator::Return(Some(t)) => mark(&mut escapes, *t),
            Terminator::Switch { val, .. } => mark(&mut escapes, *val),
            // CondJump cond is a scalar read; TailCall args are carry edges (handled above) — a
            // tail-arg neither escapes nor mutates *this* frame's value, the carry union folds the
            // next iteration's uses in. Jump/Return(None)/Unreachable: nothing.
            _ => {}
        }
    }

    // Fold per-temp marks into per-class flags.
    let mut class_escapes: HashMap<u32, bool> = HashMap::new();
    let mut class_inplace: HashMap<u32, bool> = HashMap::new();
    for t in 0..n {
        let r = uf.find_raw(t);
        if escapes[t as usize] {
            class_escapes.insert(r, true);
        }
        if inplace[t as usize] {
            *class_inplace.entry(r).or_insert(false) = true;
        }
    }

    // Assign a convention per parameter. The closure env pointer (param 0 of a closure) is read
    // through `EnvCapture` (a read-only position) and never escapes, so it falls out as Borrow
    // naturally — which is correct (the closure struct owns the env; the body borrows it).
    let mut convs = Vec::with_capacity(np);
    for (pt, pty) in &func.params {
        // A scalar param carries no ownership — its convention is irrelevant; default Own (the
        // neutral choice) to avoid claiming a meaningful "borrow" where there is nothing to borrow.
        if !needs_owning_conv(pty) {
            convs.push(Convention::Own);
            continue;
        }
        let r = uf.find_raw(pt.0);
        let conv = if class_escapes.get(&r).copied().unwrap_or(false) {
            Convention::Own
        } else if class_inplace.get(&r).copied().unwrap_or(false) {
            Convention::Inout
        } else {
            Convention::Borrow
        };
        convs.push(conv);
    }
    func.param_conventions = convs;

    // Return convention: today's lowering always normalizes an escaping result to an owned +1
    // (interior pointers are cloned via `own_for_read`/`CloneBox` before they escape; fresh allocs
    // are +1). So the honest inference is `Own` for every function — and that is also the
    // conservative default that keeps behaviour unchanged. We KEEP it Own and document (FINDINGS)
    // that genuine borrow-returns do not arise in the current IR because the lowerer pre-clones.
    func.ret_convention = Convention::Own;
}

/// A type that participates in ownership (heap rc or boxed union) — only these have a meaningful
/// borrow/own distinction. Mirrors the lowerer's `needs_owning` (kept local to avoid coupling).
fn needs_owning_conv(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Str
            | Type::StrLit(_)
            | Type::Array(_)
            | Type::FixedArray(_)
            | Type::Object { .. }
            | Type::Map { .. }
            | Type::Iterator(_)
            | Type::Function { .. }
            | Type::Union(_)
    ) || matches!(ty, Type::TypeVar(_)) // includes Json = TypeVar(u32::MAX)
}

/// Classify each temp USED by `instr` into a [`UseKind`]. Carry edges (Copy/Bind/Phi/no-op-Coerce)
/// are handled by the union-find and contribute NO use here. Everything not provably read-only or
/// in-place is `Escape` (fail-safe).
fn classify_uses(instr: &Instruction, mark: &mut impl FnMut(Temp, UseKind)) {
    use Instruction::*;
    match instr {
        // Carry edges — no direct use (union-find joins the classes).
        Copy { .. } | Bind { .. } | Phi { .. } => {}
        Coerce { src, from_ty, to_ty, .. } => {
            // A no-op coerce is a carry edge (joined elsewhere). A repr-changing coerce produces a
            // distinct value; its source is read but the result is independent — conservatively
            // treat the source as escaping (it may be consumed by the coercion, e.g. boxing).
            if !crate::carry::coerce_is_carry(from_ty, to_ty) {
                mark(*src, UseKind::Escape);
            }
        }

        // ---- Read-only positions ----
        FieldGet { object, .. } => mark(*object, UseKind::Read),
        Index { object, key, .. } => {
            mark(*object, UseKind::Read);
            mark(*key, UseKind::Read);
        }
        SealedArrayFieldGet { array, index, .. } | BoxedArrayFieldGet { array, index, .. } => {
            mark(*array, UseKind::Read);
            mark(*index, UseKind::Read);
        }
        EnvCapture { env, .. } => mark(*env, UseKind::Read),
        CellGet { cell, .. } => mark(*cell, UseKind::Read),
        ArrayLenCheck { val, .. } => mark(*val, UseKind::Read),
        ObjectRest { src, .. } => mark(*src, UseKind::Read),
        IsType { val, .. } | SumTagEq { val, .. } | HasPattern { val, .. } | MatchesSchema { val, .. } => {
            mark(*val, UseKind::Read)
        }
        Unary { operand, .. } => mark(*operand, UseKind::Read),
        Binary { lhs, rhs, .. } => {
            mark(*lhs, UseKind::Read);
            mark(*rhs, UseKind::Read);
        }
        Unbox { val, .. } => mark(*val, UseKind::Read),
        // A Release is the value's death (a drop) — not an escape and not a mutation.
        Release { .. } | FreeBoxShell { .. } | FreeBoxShellIfDistinct { .. }
        | ReleaseIfDistinct { .. } | FreeCell { .. } => {}

        // ---- In-place mutation: receiver is Inout, stored value escapes ----
        IndexSet { object, value, .. } => {
            mark(*object, UseKind::InPlace);
            mark(*value, UseKind::Escape);
        }
        FieldSet { object, value, .. } => {
            mark(*object, UseKind::InPlace);
            mark(*value, UseKind::Escape);
        }
        CellSet { cell, value, .. } => {
            mark(*cell, UseKind::InPlace);
            mark(*value, UseKind::Escape);
        }

        // ---- Escape positions (own) ----
        Call { args, callee, .. } => {
            for a in args {
                mark(*a, UseKind::Escape);
            }
            if let CallTarget::Indirect(t) = callee {
                mark(*t, UseKind::Read);
            }
        }
        CallIntrinsic { intrinsic, args, .. } => {
            // Use the hand-audited table when available; else conservative all-Escape.
            match intrinsic_conventions(intrinsic) {
                Some(conv) => {
                    for (i, a) in args.iter().enumerate() {
                        let c = conv.params.get(i).copied().unwrap_or(Convention::Own);
                        let kind = match c {
                            Convention::Borrow => UseKind::Read,
                            Convention::Inout => UseKind::InPlace,
                            Convention::Own => UseKind::Escape,
                        };
                        mark(*a, kind);
                    }
                }
                None => {
                    for a in args {
                        mark(*a, UseKind::Escape);
                    }
                }
            }
        }
        MakeClosure { captures, .. } => {
            for c in captures {
                mark(*c, UseKind::Escape);
            }
        }
        MakeObject { fields, spreads, .. } => {
            for (_, t) in fields {
                mark(*t, UseKind::Escape);
            }
            for s in spreads {
                mark(*s, UseKind::Escape);
            }
        }
        MakeArray { elements, .. } => {
            for e in elements {
                mark(*e, UseKind::Escape);
            }
        }
        MakeCell { init, .. } => mark(*init, UseKind::Escape),
        GlobalValSet { value, .. } => mark(*value, UseKind::Escape),
        Retain { val, .. } => mark(*val, UseKind::Escape),
        CloneBox { src, .. } => mark(*src, UseKind::Escape),
        Box { val, .. } => mark(*val, UseKind::Escape),
        Panic { msg } => mark(*msg, UseKind::Read),

        // Pure producers / no heap uses.
        Const { .. } => {}
        MakeNamedClosure { .. } | GlobalValGet { .. } => {}
        DebugDeclare { .. } => {}
    }
}

// ===========================================================================
// 3. The shadow-mode verifier
// ===========================================================================

/// A single ownership-invariant violation. Report-only — collected, never acted on.
#[derive(Debug, Clone)]
pub struct Violation {
    pub func: String,
    pub block: u32,
    pub kind: ViolationKind,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViolationKind {
    /// An RC op (Retain/Release/CloneBox/Free*) sits in a block unreachable from entry — the
    /// `tco_post` class: it can never run, so its balancing reference op is missing → a leak (or,
    /// for a release-old, an overwrite-without-release). The headline check.
    RcInUnreachableBlock,
    /// A temp is Released more than once on a straight-line (single-block) path → double-free.
    DoubleRelease,
    /// A temp is USED by a non-release instruction AFTER it was Released on the same straight-line
    /// path → use-after-free.
    UseAfterRelease,
    /// A `CallIntrinsic` uses an intrinsic with no hand-audited table entry — a gap to close before
    /// Wave 2 can consume the table for that intrinsic.
    UnauditedIntrinsic,
}

impl ViolationKind {
    pub fn label(self) -> &'static str {
        match self {
            ViolationKind::RcInUnreachableBlock => "rc-in-unreachable-block",
            ViolationKind::DoubleRelease => "double-release",
            ViolationKind::UseAfterRelease => "use-after-release",
            ViolationKind::UnauditedIntrinsic => "unaudited-intrinsic",
        }
    }
}

/// Run the shadow verifier over an entire module. Returns every violation found. Never mutates.
pub fn verify_module(module: &LinModule) -> Vec<Violation> {
    let mut out = Vec::new();
    for func in &module.functions {
        verify_fn(func, &mut out);
    }
    out
}

fn fname(func: &LinFunction) -> String {
    func.name.clone().unwrap_or_else(|| format!("fn#{}", func.id.0))
}

/// If `instr` produces a value with a KNOWN fresh refcount of exactly 1 (an owning producer), return
/// its destination temp. Used by the over-release balance check to seed `balance[dst] = 1`. We list
/// only producers whose result is unambiguously a fresh +1 the current scope owns: a heap literal,
/// a clone, a box, a call result (the callee returns +1 under today's Own convention). Borrowed
/// producers (`FieldGet`/`Index`/`EnvCapture`/`CellGet` — interior pointers the scope does NOT own
/// until an explicit Retain) are deliberately EXCLUDED, so a release on them is not balance-checked
/// (their incoming count is unknown). Conservative: a producer we are unsure about is omitted.
fn owning_producer_def(instr: &Instruction) -> Option<Temp> {
    use Instruction::*;
    match instr {
        MakeObject { dst, .. }
        | MakeArray { dst, .. }
        | MakeClosure { dst, .. }
        | CloneBox { dst, .. }
        | Box { dst, .. }
        | Call { dst, .. } => Some(*dst),
        _ => None,
    }
}

fn verify_fn(func: &LinFunction, out: &mut Vec<Violation>) {
    // --- Reachability from entry over terminator successors ---
    let mut reachable: HashSet<BlockId> = HashSet::new();
    let mut q: VecDeque<BlockId> = VecDeque::new();
    if let Some(b0) = func.blocks.first() {
        reachable.insert(b0.id);
        q.push_back(b0.id);
    }
    let succ = |term: &Terminator| -> Vec<BlockId> {
        match term {
            Terminator::Jump(b) => vec![*b],
            Terminator::CondJump { then_block, else_block, .. } => vec![*then_block, *else_block],
            Terminator::Switch { cases, default, .. } => {
                let mut v: Vec<BlockId> = cases.iter().map(|(_, b)| *b).collect();
                v.push(*default);
                v
            }
            Terminator::Return(_) | Terminator::TailCall { .. } | Terminator::Unreachable => vec![],
        }
    };
    while let Some(bid) = q.pop_front() {
        if let Some(b) = func.block(bid) {
            for s in succ(&b.terminator) {
                if reachable.insert(s) {
                    q.push_back(s);
                }
            }
        }
    }

    // --- Check 1: the tco_post class — a RELEASE in an unreachable block whose target is NEVER
    // released on any reachable path. This is the precise shape of the historical leaks
    // (`b2e6d35`, `9a1a735`): the lowerer emitted the only release of a per-iteration allocation
    // into the dead `tco_post` continuation after a diverging TailCall, so it never ran → leak.
    //
    // CRUCIAL refinement: post-fix, the lowerer STILL pops scopes into the dead post block (those
    // releases are now harmless DUPLICATES because `release_owned_for_tail_call` emits the real
    // release on the LIVE back-edge block first). A naive "any RC in a dead block" check would flag
    // every tail-recursive function. So we only flag a dead-block release whose temp has NO
    // reachable release — exactly the unbalanced-allocation leak, not the harmless duplicate.
    // A Retain in a dead block is pure dead code (never runs ⇒ never over-retains) and is ignored.
    let mut reachable_released: HashSet<Temp> = HashSet::new();
    for block in &func.blocks {
        if !reachable.contains(&block.id) {
            continue;
        }
        for instr in &block.instructions {
            match instr {
                Instruction::Release { val, .. } | Instruction::ReleaseIfDistinct { val, .. } => {
                    reachable_released.insert(*val);
                }
                Instruction::FreeCell { cell, .. } => {
                    reachable_released.insert(*cell);
                }
                _ => {}
            }
        }
    }
    // A temp passed as a self-`TailCall` argument has its ownership TRANSFERRED into the next
    // iteration's parameter slot. Codegen's TCO release-old machinery (the `tco_owns` alias-compare)
    // frees the PRIOR slot value before the back-edge store, and the final iteration's value is
    // freed at teardown — so there is intentionally NO reachable `Release` instruction for it, and
    // the lowerer's scope-exit Release lands (harmlessly) in the dead `tco_post` block. Treating a
    // tail-call arg as "released via slot transfer" is what makes Check 1 precise: without this it
    // false-positives on EVERY tail-recursive function that threads an owned value (e.g. RAPTOR's
    // `scanBack` lastFound, `scanRounds` markedStops). These are NOT leaks; the transfer + release-old
    // balances them. See `lower::release_owned_for_tail_call`.
    for block in &func.blocks {
        if let Terminator::TailCall { args } = &block.terminator {
            for a in args {
                reachable_released.insert(*a);
            }
        }
    }
    for block in &func.blocks {
        if reachable.contains(&block.id) {
            continue;
        }
        for instr in &block.instructions {
            let released_temp = match instr {
                Instruction::Release { val, .. } | Instruction::ReleaseIfDistinct { val, .. } => Some(*val),
                Instruction::FreeCell { cell, .. } => Some(*cell),
                _ => None,
            };
            if let Some(t) = released_temp {
                if !reachable_released.contains(&t) {
                    out.push(Violation {
                        func: fname(func),
                        block: block.id.0,
                        kind: ViolationKind::RcInUnreachableBlock,
                        detail: format!(
                            "owned temp t{} released ONLY in unreachable block {} (no reachable release) — leak: {instr:?}",
                            t.0, block.id.0
                        ),
                    });
                }
            }
        }
    }

    // --- Check 2: over-release (per reachable block, refcount-BALANCE aware) ---
    //
    // The IR is reference-counted, so a temp may be released several times legitimately when it was
    // retained the same number of times (a `retain;retain;release;release` nets to zero). A naive
    // "released twice ⇒ double-free" check drowns in those balanced pairs. Instead we track a NET
    // ownership balance, but ONLY for temps DEFINED in this block by an owning producer (so we know
    // their starting count is exactly 1). Live-in temps (params, predecessor values) arrive with an
    // unknown incoming refcount, so we cannot reason about them within one block — we skip them
    // (a deliberate under-approximation; cross-block balance is out of scope for shadow mode).
    //
    //   balance[t] starts at 1 when t is defined by an owning producer in this block;
    //   Retain(t)/CloneBox{src:t}/Box{val:t} → +1;  Release(t)/ReleaseIfDistinct{val:t} → −1.
    // A release that drives an IN-BLOCK temp's balance NEGATIVE is releasing more references than
    // exist → a genuine over-release on this straight line. This cannot false-positive on balanced
    // retain/release pairs, which is what made the earlier set-based check unusable.
    for block in &func.blocks {
        if !reachable.contains(&block.id) {
            continue;
        }
        let mut balance: HashMap<Temp, i32> = HashMap::new();
        for instr in &block.instructions {
            // Owning producers establish a known starting count of 1 for the defined temp.
            if let Some(d) = owning_producer_def(instr) {
                balance.insert(d, 1);
            }
            match instr {
                Instruction::Retain { val, .. } => {
                    if let Some(b) = balance.get_mut(val) {
                        *b += 1;
                    }
                }
                Instruction::CloneBox { src, .. } | Instruction::Box { val: src, .. } => {
                    if let Some(b) = balance.get_mut(src) {
                        *b += 1;
                    }
                }
                Instruction::Release { val, .. } | Instruction::ReleaseIfDistinct { val, .. } => {
                    if let Some(b) = balance.get_mut(val) {
                        *b -= 1;
                        if *b < 0 {
                            out.push(Violation {
                                func: fname(func),
                                block: block.id.0,
                                kind: ViolationKind::DoubleRelease,
                                detail: format!(
                                    "in-block temp t{} released more times than owned (balance {}) — over-release",
                                    val.0, *b
                                ),
                            });
                        }
                    }
                }
                // A genuine dereference of an in-block owning temp whose balance has reached 0 (fully
                // released) is a use-after-free. We only check temps with a known in-block balance,
                // and skip the *IfDistinct `other` guard + the box-shell frees (whose operand is a
                // guard/shell pointer, not a live deref), so this cannot false-positive on the
                // deliberate distinct-guard pattern. (Retain/CloneBox/Box are matched above.)
                Instruction::FreeBoxShell { .. } | Instruction::FreeBoxShellIfDistinct { .. }
                | Instruction::FreeCell { .. } => {}
                other => {
                    let (uses, _defs) = instr_use_def(other);
                    for u in &uses {
                        if balance.get(u).copied().unwrap_or(1) <= 0 {
                            out.push(Violation {
                                func: fname(func),
                                block: block.id.0,
                                kind: ViolationKind::UseAfterRelease,
                                detail: format!("in-block temp t{} used after full release in {other:?}", u.0),
                            });
                        }
                    }
                }
            }
        }
    }

    // --- Check 4: un-audited intrinsics (table-completeness gap) ---
    let mut seen_gap: HashSet<String> = HashSet::new();
    for block in &func.blocks {
        for instr in &block.instructions {
            if let Instruction::CallIntrinsic { intrinsic, .. } = instr {
                if intrinsic_conventions(intrinsic).is_none() {
                    let key = format!("{intrinsic:?}");
                    let short = key.split(['{', '(']).next().unwrap_or(&key).trim().to_string();
                    if seen_gap.insert(short.clone()) {
                        out.push(Violation {
                            func: fname(func),
                            block: block.id.0,
                            kind: ViolationKind::UnauditedIntrinsic,
                            detail: short,
                        });
                    }
                }
            }
        }
    }
}

/// Aggregate counts for a one-line shadow summary (printed by the pipeline under the shadow env).
#[derive(Debug, Default, Clone)]
pub struct ShadowSummary {
    pub functions: usize,
    pub params_total: usize,
    pub params_borrow: usize,
    pub params_own: usize,
    pub params_inout: usize,
    pub violations: Vec<Violation>,
}

/// Infer conventions (on a clone-free borrow of an already-inferred module) and verify, returning
/// the summary. Expects `infer_conventions` to have already populated the convention fields.
pub fn shadow_summary(module: &LinModule) -> ShadowSummary {
    let mut s = ShadowSummary::default();
    for func in &module.functions {
        s.functions += 1;
        for (i, _) in func.params.iter().enumerate() {
            s.params_total += 1;
            match func.param_convention(i) {
                Convention::Borrow => s.params_borrow += 1,
                Convention::Own => s.params_own += 1,
                Convention::Inout => s.params_inout += 1,
            }
        }
    }
    s.violations = verify_module(module);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn func(blocks: Vec<BasicBlock>, params: Vec<(Temp, Type)>, temp_count: u32) -> LinFunction {
        let mut temp_types = HashMap::new();
        for (t, ty) in &params {
            temp_types.insert(*t, ty.clone());
        }
        LinFunction {
            id: FuncId(0),
            name: Some("t".into()),
            params,
            is_closure: false,
            ret_ty: Type::Null,
            param_conventions: Vec::new(),
            ret_convention: Convention::Own,
            blocks,
            temp_types,
            temp_count,
            intrinsic_slots: HashMap::new(),
            repr: Vec::new(),
            coverage_origin: None,
        }
    }

    fn blk(id: u32, instrs: Vec<Instruction>, term: Terminator) -> BasicBlock {
        BasicBlock {
            id: BlockId(id),
            label: None,
            instructions: instrs,
            terminator: term,
            span: None,
            instr_spans: Vec::new(),
        }
    }

    /// The headline `tco_post` leak: a function tail-calls (block 0 ends with TailCall, diverges),
    /// and the ONLY release of an owned per-iteration temp lands in the dead continuation block 1.
    /// The verifier must flag it as `rc-in-unreachable-block` with no reachable release.
    #[test]
    fn catches_release_in_unreachable_tco_post() {
        let f = func(
            vec![
                blk(
                    0,
                    vec![Instruction::MakeArray { dst: Temp(0), elements: vec![], elem_ty: Type::Int32, inline: false, columnar: false }],
                    Terminator::TailCall { args: vec![] },
                ),
                // Dead block: never reached (TailCall has no successors).
                blk(1, vec![Instruction::Release { val: Temp(0), ty: Type::Array(Box::new(Type::Int32)) }], Terminator::Return(None)),
            ],
            vec![],
            1,
        );
        let mut out = Vec::new();
        verify_fn(&f, &mut out);
        assert!(
            out.iter().any(|v| v.kind == ViolationKind::RcInUnreachableBlock),
            "expected rc-in-unreachable-block, got {out:?}"
        );
    }

    /// The harmless DUPLICATE case: the temp is released on the LIVE block before the TailCall AND
    /// (redundantly) in the dead post block. Because there IS a reachable release, this must NOT be
    /// flagged — exactly the post-`9a1a735`/`b2e6d35` shape the lowerer now emits.
    #[test]
    fn ignores_duplicate_release_when_reachable_one_exists() {
        let arr = Type::Array(Box::new(Type::Int32));
        let f = func(
            vec![
                blk(
                    0,
                    vec![
                        Instruction::MakeArray { dst: Temp(0), elements: vec![], elem_ty: Type::Int32, inline: false, columnar: false },
                        Instruction::Release { val: Temp(0), ty: arr.clone() },
                    ],
                    Terminator::TailCall { args: vec![] },
                ),
                blk(1, vec![Instruction::Release { val: Temp(0), ty: arr.clone() }], Terminator::Return(None)),
            ],
            vec![],
            1,
        );
        let mut out = Vec::new();
        verify_fn(&f, &mut out);
        assert!(
            !out.iter().any(|v| v.kind == ViolationKind::RcInUnreachableBlock),
            "duplicate dead release with a reachable release must not flag: {out:?}"
        );
    }

    /// Over-release: a fresh +1 array released twice with no intervening retain → balance goes < 0.
    #[test]
    fn catches_over_release_of_fresh_value() {
        let arr = Type::Array(Box::new(Type::Int32));
        let f = func(
            vec![blk(
                0,
                vec![
                    Instruction::MakeArray { dst: Temp(0), elements: vec![], elem_ty: Type::Int32, inline: false, columnar: false },
                    Instruction::Release { val: Temp(0), ty: arr.clone() },
                    Instruction::Release { val: Temp(0), ty: arr.clone() },
                ],
                Terminator::Return(None),
            )],
            vec![],
            1,
        );
        let mut out = Vec::new();
        verify_fn(&f, &mut out);
        assert!(out.iter().any(|v| v.kind == ViolationKind::DoubleRelease), "expected over-release: {out:?}");
    }

    /// Balanced retain/release pairs on a fresh value must NOT flag (refcount +2 / −2 nets to zero).
    #[test]
    fn ignores_balanced_retain_release() {
        let arr = Type::Array(Box::new(Type::Int32));
        let f = func(
            vec![blk(
                0,
                vec![
                    Instruction::MakeArray { dst: Temp(0), elements: vec![], elem_ty: Type::Int32, inline: false, columnar: false },
                    Instruction::Retain { val: Temp(0), ty: arr.clone() },
                    Instruction::Release { val: Temp(0), ty: arr.clone() },
                    Instruction::Release { val: Temp(0), ty: arr.clone() },
                ],
                Terminator::Return(None),
            )],
            vec![],
            1,
        );
        let mut out = Vec::new();
        verify_fn(&f, &mut out);
        assert!(!out.iter().any(|v| v.kind == ViolationKind::DoubleRelease), "balanced pairs must not flag: {out:?}");
    }

    /// Convention inference: a param only read (FieldGet) and never escaping infers `Borrow`; a
    /// param returned infers `Own`.
    #[test]
    fn infers_borrow_for_readonly_param_and_own_for_returned() {
        use indexmap::IndexMap;
        let mut fields = IndexMap::new();
        fields.insert("x".into(), Type::Int32);
        let rec = Type::Object { fields, sealed: true };
        // fn(p: rec, q: rec) -> Int32 { read p.x; return q }  — but ret is Int32 here; q escapes via
        // being the returned temp through a Copy. Simpler: p read-only, q returned.
        let mut f = func(
            vec![blk(
                0,
                vec![Instruction::FieldGet {
                    dst: Temp(2),
                    object: Temp(0),
                    field: "x".into(),
                    obj_ty: rec.clone(),
                    result_ty: Type::Int32,
                }],
                Terminator::Return(Some(Temp(1))),
            )],
            vec![(Temp(0), rec.clone()), (Temp(1), rec.clone())],
            3,
        );
        infer_fn(&mut f);
        assert_eq!(f.param_convention(0), Convention::Borrow, "read-only param should be Borrow");
        assert_eq!(f.param_convention(1), Convention::Own, "returned param should be Own");
    }

    #[test]
    fn intrinsic_table_object_get_is_borrow_borrow_borrow() {
        let c = intrinsic_conventions(&Intrinsic::ObjectGet).unwrap();
        assert_eq!(c.params, vec![Convention::Borrow, Convention::Borrow]);
        assert_eq!(c.ret, Convention::Borrow);
    }

    #[test]
    fn intrinsic_table_push_is_inout_own() {
        let c = intrinsic_conventions(&Intrinsic::Push).unwrap();
        assert_eq!(c.params, vec![Convention::Inout, Convention::Own]);
    }

    /// Async/worker/shared/stream/FromJson families are now audited (no longer table GAPs). Pin the
    /// load-bearing decisions — including the two consuming EXCEPTIONS (`Worker`, `StreamPromise`,
    /// `StreamReduce` init) and the borrowed-through `Freeze` — so a future edit cannot drift them.
    #[test]
    fn intrinsic_table_async_stream_families_audited() {
        // Borrow-in / Own-out is the dominant async/stream shape.
        let map = intrinsic_conventions(&Intrinsic::StreamMap).unwrap();
        assert_eq!(map.params, vec![Convention::Borrow, Convention::Borrow]);
        assert_eq!(map.ret, Convention::Own);
        assert_eq!(intrinsic_conventions(&Intrinsic::Async).unwrap().ret, Convention::Own);
        assert_eq!(intrinsic_conventions(&Intrinsic::SharedNew).unwrap().params, vec![Convention::Borrow]);

        // EXCEPTION 1 — Worker RETAINS both handler closures → Own (ownership into the worker).
        assert_eq!(
            intrinsic_conventions(&Intrinsic::Worker).unwrap().params,
            vec![Convention::Own, Convention::Own]
        );
        // EXCEPTION 2 — StreamPromise MOVES (consumes) its stream → Own.
        assert_eq!(intrinsic_conventions(&Intrinsic::StreamPromise).unwrap().params, vec![Convention::Own]);
        // EXCEPTION 3 — StreamReduce CONSUMES its init accumulator (arg 1 = Own), borrows s + f.
        assert_eq!(
            intrinsic_conventions(&Intrinsic::StreamReduce).unwrap().params,
            vec![Convention::Borrow, Convention::Own, Convention::Borrow]
        );
        // Freeze SEALS in place and returns the SAME pointer (not a fresh +1): Inout arg, Borrow ret.
        let fr = intrinsic_conventions(&Intrinsic::Freeze).unwrap();
        assert_eq!(fr.params, vec![Convention::Inout]);
        assert_eq!(fr.ret, Convention::Borrow);

        // FromJson borrows the input, returns a fresh +1 union.
        let fj = intrinsic_conventions(&Intrinsic::FromJson {
            target: Box::new(Type::Null),
            named_defs: Vec::new(),
        })
        .unwrap();
        assert_eq!(fj.params, vec![Convention::Borrow]);
        assert_eq!(fj.ret, Convention::Own);

        // The previously-UNSURE entries are now confirmed Borrow-fill / Borrow-in.
        assert_eq!(
            intrinsic_conventions(&Intrinsic::ArrayAllocateFilled).unwrap().params,
            vec![Convention::Own, Convention::Borrow]
        );
        assert_eq!(intrinsic_conventions(&Intrinsic::ValueKey).unwrap().params, vec![Convention::Borrow]);
        assert_eq!(intrinsic_conventions(&Intrinsic::ToJson).unwrap().params, vec![Convention::Borrow]);
    }

    /// `escape_alias_convention` mirrors the two former inline `record_escape_alias` gates in
    /// `lower::lower_coerce_arg`'s box/unbox arm. Pin each decision so a future edit cannot drift it.
    #[test]
    fn escape_alias_unbox_and_widen_gates() {
        let arr = Type::Array(Box::new(Type::Int32));
        let json = Type::TypeVar(u32::MAX); // a union/boxed view
        // UNBOX union arg → concrete HEAP param: alias.
        assert!(escape_alias_convention(&json, &arr, false));
        // UNBOX union arg → concrete SCALAR param: do NOT alias (box is orphaned).
        assert!(!escape_alias_convention(&json, &Type::Int32, false));
        // WIDEN concrete heap arg → union param, non-sealed: alias.
        assert!(escape_alias_convention(&arr, &json, false));
        // WIDEN concrete heap arg → union param, SEALED record source: do NOT alias (materialized).
        assert!(!escape_alias_convention(&arr, &json, true));
        // Same-side (both concrete / both union): not in the box/unbox arm → never alias.
        assert!(!escape_alias_convention(&arr, &arr, false));
        assert!(!escape_alias_convention(&json, &json, false));
    }

    /// `container_insert_convention` mirrors the three-way branch in `lower::transfer_into_container`.
    /// Pin each outcome so a future edit cannot drift the fresh-vs-retain (move-vs-+1) decision.
    #[test]
    fn container_insert_three_way() {
        let arr = Type::Array(Box::new(Type::Int32));
        let json = Type::TypeVar(u32::MAX); // a union/boxed view
        // Non-owning scalar: nothing to balance, regardless of op/fresh.
        assert_eq!(container_insert_convention(&Type::Int32, true, true), ContainerInsert::Nothing);
        assert_eq!(container_insert_convention(&Type::Int32, false, false), ContainerInsert::Nothing);
        // Union element into a RETAIN-semantics op (op_consumes=false): nothing — runtime +1'd inner.
        assert_eq!(container_insert_convention(&json, false, true), ContainerInsert::Nothing);
        assert_eq!(container_insert_convention(&json, false, false), ContainerInsert::Nothing);
        // Union element into a CONSUMING op: fresh ⇒ transfer, borrowed ⇒ retain.
        assert_eq!(container_insert_convention(&json, true, true), ContainerInsert::Transfer);
        assert_eq!(container_insert_convention(&json, true, false), ContainerInsert::Retain);
        // Concrete rc element: op_consumes irrelevant; fresh ⇒ transfer, borrowed ⇒ retain.
        assert_eq!(container_insert_convention(&arr, false, true), ContainerInsert::Transfer);
        assert_eq!(container_insert_convention(&arr, false, false), ContainerInsert::Retain);
        assert_eq!(container_insert_convention(&arr, true, true), ContainerInsert::Transfer);
        assert_eq!(container_insert_convention(&arr, true, false), ContainerInsert::Retain);
    }

    /// `box_shell_reclaim` mirrors the three identical inline `made_fresh_box` gates that drive the
    /// `FreeBoxShell` reclaim in `lower.rs` (`coerce_and_own_store`, the captured-cell + global
    /// `LocalSet`): `is_union_ty(slot) && !is_union_ty(value) && type_repr_differs`. Pin each outcome
    /// so a future edit cannot drift the fresh-distinct-shell-must-be-reclaimed decision.
    #[test]
    fn box_shell_reclaim_widen_gate() {
        let arr = Type::Array(Box::new(Type::Int32));
        let json = Type::TypeVar(u32::MAX); // a union/boxed slot view
        // Concrete value → union slot, repr differs (a fresh box really made): reclaim the shell.
        assert!(box_shell_reclaim(&arr, &json, true));
        assert!(box_shell_reclaim(&Type::Int32, &json, true));
        // Same case but repr does NOT differ (no box made — caller's type_repr_differs said so):
        // nothing to free.
        assert!(!box_shell_reclaim(&arr, &json, false));
        // Slot is concrete (not a union): no union box is ever made → never reclaim.
        assert!(!box_shell_reclaim(&arr, &arr, true));
        // Value is ALREADY a union (the box is a clone/forward owned elsewhere): never reclaim here.
        assert!(!box_shell_reclaim(&json, &json, true));
    }

    /// `bound_box_moves_inner` mirrors the inline `made_fresh_box` gate in
    /// `lower::coerce_to_slot_type_owning_bind`:
    /// `is_union_ty(slot) && !is_union_ty(value) && is_rc_type(value) && type_repr_differs`. Pin each
    /// outcome — in particular the SCALAR difference from `box_shell_reclaim` (the load-bearing
    /// `is_rc_type` conjunct) — so a future edit cannot drift the inner-+1-move decision.
    #[test]
    fn bound_box_moves_inner_gate() {
        let arr = Type::Array(Box::new(Type::Int32));
        let json = Type::TypeVar(u32::MAX); // a union/boxed slot view
        // Concrete RC value → union slot, repr differs: the box takes the inner +1 → move it.
        assert!(bound_box_moves_inner(&arr, &json, true));
        // SCALAR value → union slot: box_shell_reclaim fires here, but bound_box_moves_inner does NOT
        // (no inner heap +1 to move — the box wraps copied scalar bytes). THE load-bearing difference.
        assert!(!bound_box_moves_inner(&Type::Int32, &json, true));
        assert!(box_shell_reclaim(&Type::Int32, &json, true)); // contrast
        // repr does NOT differ (no box made): nothing to move.
        assert!(!bound_box_moves_inner(&arr, &json, false));
        // Slot is concrete (not a union): no union box ever made → never move.
        assert!(!bound_box_moves_inner(&arr, &arr, true));
        // Value is ALREADY a union (box is a clone/forward owned elsewhere): never move here.
        assert!(!bound_box_moves_inner(&json, &json, true));
    }
}
