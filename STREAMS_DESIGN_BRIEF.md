# Streams — Design Brief (locked decisions)

This is the single source of truth for the `Stream<T>` feature. Branch: `feat/streams`.
Do NOT merge to master. Do NOT work on master. Build-before-test always:
`cargo build --workspace && cargo test --workspace`.

## What we're building
An opaque runtime `Stream<T>` (lazy pull-source) + a push-driven sink, for reading files
and other byte sources, with a fluent lazy-graph API and two terminal drivers.

## Locked decisions (do not relitigate)
1. **New opaque `Type::Stream(Box<Type>)`** — sibling to `Type::Iterator`. NOT JSON,
   non-transferable, covariant in T. Distinct from Iterator because a stream is effectful,
   fallible, and owns an OS resource (iterator protocol's cond/current must be pure).
2. **RC-drop auto-close + explicit close.** A `TAG_STREAM` finalizer closes the fd when
   refcount hits 0 if not already closed. Explicit `close()` is idempotent, for determinism.
3. **`for` over a stream**: consumed via `.for(fn)` dot-application (NOT `for…in` — that
   syntax does not exist in Lin). EOF ends the loop normally; a read Error makes the
   `for`-expression evaluate to `Error`. So a stream `for` has return type `Null | Error`.
4. **Unified sources**: file / TCP / process-stdout / stdin all become `Stream<UInt8[]>`,
   each supplying a different read backend. Byte streams are fundamental; lines/text are
   adapters.
5. **Two terminal drivers**:
   - `.drain()` → drives on calling thread → `Null | Error`. Sound today, no new machinery.
   - `.promise()` → MOVES the pipeline onto a worker thread → `Promise<Null | Error>`.
     Real concurrency + fault isolation (ADR-070 / §32.2.2 fault boundary).
6. **In-band error threading**: the pipeline is a lazy graph; a poisoned upstream makes
   every adapter a passthrough, so the first error short-circuits to the terminal op.
   This keeps the chain fluent (no `is Error` at every step).
7. **Affine resources** (use-at-most-once; dropping is FINE — finalizer closes the fd).
   NOT strict-linear. Double-use is the only error. Must-use is a WARNING, not an error.
8. **Placement restriction (v1)**: a `Stream` may live only in a `val` binding, function
   arg, or return value — NOT in object/array fields, NOT in `var`. Confines the
   move-checker to local bindings (no container-linearity). Relaxable later.
9. **Transfer model = ADR-042 completed**: transferable values cross threads by deep COPY
   (existing); resource values cross by MOVE (new). Both yield disjoint object graphs ⇒
   non-atomic RC stays sound. Move = handoff: pointer moves, no clone, source must not
   touch it again (enforced by the affine check), worker releases.

## Surface API (std/stream)
```
readStream(path) / net.tcpStream(fd) / process.stdoutStream(h) / io.stdinStream()  -> Stream<UInt8[]>
lines(s)   : Stream<UInt8[]> -> Stream<String>
chunks(s,n): Stream<UInt8[]> -> Stream<UInt8[]>
map(s,f) / filter(s,p) / take(s,n) : Stream<T> -> Stream<U>     (lazy adapters)
readText(s) / collect(s)           : terminal sync reads -> String|Error / UInt8[]|Error
writeStream(s, path)               : builds a sink node
.drain()                           : sink -> Null | Error          (sync, calling thread)
.promise()                         : sink -> Promise<Null | Error>  (async, worker thread)
.for(fn)                           : consume each item
```

Worked example (must run end-to-end):
```lin
val transform = (line: String): String =>
  line.split(",").map(f => "\"${f}\"").join("|")   // a,b,c -> "a"|"b"|"c"

val run = (): Null | Error =>
  readStream("in.csv").lines().map(transform).filter(removeEmptyLines)
    .writeStream("out.csv").drain()
```

## Concrete anchors in the codebase
- `Type` enum: `crates/lin-check/src/types.rs:5`; `Iterator(Box<Type>)` at :37, `Shared` at :44.
  `Type::Iterator` is threaded through ~16 files — use as the checklist for `Type::Stream`:
  types.rs, compat.rs, zonk.rs, resolve.rs, checker/{expr,mod,call,intrinsics,helpers}.rs,
  lin-ir/{monomorphize,lower}.rs, codegen/{types,boxing,data,rc,intrinsics}.rs.
- `is_definitely_non_transferable`: `crates/lin-check/src/checker/helpers.rs:149` (add Stream).
- Tags: `crates/lin-runtime/src/tagged.rs` — last is `TAG_SHARED=18`. Use **TAG_STREAM=19**.
- **`Shared<T>` is the precedent to copy** (ADR-044, recently added opaque box):
  - runtime: `crates/lin-runtime/src/shared.rs` (model `stream.rs` on it)
  - codegen dispatch: `crates/lin-codegen/src/codegen/intrinsics.rs:422+` (lin_shared_new/get/set/with_lock)
- Capture kinds (TWO mirrored places, both stop at 5 = Tagged):
  - `crates/lin-runtime/src/transfer.rs:132` (CAP_NONE..CAP_TAGGED). Use **CAP_MOVE=6**.
    `transfer_clone_env` (:144), `release_env_copy` (:184), `env_is_transferable` (:201).
  - `crates/lin-ir/src/ir.rs:139` `enum CaptureRelease` + `:153` code()  → add `Move=6`.
    Emitted in `lin-ir/src/lower.rs:4517+` from capture `.ty`.
- async runtime: `crates/lin-runtime/src/async_rt.rs:180` `lin_async_spawn` (inline fallback
  at :202 for non-transferable env; `with_async_boundary` fault isolation).
- `lin_for` lowering: `crates/lin-ir/src/lower.rs` (`lower_for`) — add stream branch (Stage 5).
- fs (whole-file only today): `crates/lin-runtime/src/fs.rs`. Add lin_fs_open/read/close.
- stdlib loaded via include_str! in `crates/lin-compile`; new `stdlib/stream.lin` + test
  `stdlib/stream.test.lin`.

## ASan is MANDATORY for Stages 2 and 7
`cargo test` does NOT catch the UAF/double-free class. The fd-closing finalizer (Stage 2)
and the CAP_MOVE no-clone/no-source-release path (Stage 7) MUST be verified under ASan.
Build runtime with `RUSTFLAGS="-Zsanitizer=address"` (nightly) or the project's ASan harness;
check fd closes exactly once. Poison-and-leak technique if a double-free is suspected
(see how project_test_failpath_uaf was diagnosed).

## Stage gates (each must pass before the next starts)
1. Type::Stream plumbing — workspace builds, all existing tests green, no behavior change.
2. TAG_STREAM + finalizer + lin_stream_read/close — ASan: fd closes once.
3. fs.openRead + file backend — open+read bytes end-to-end.
4. adapters + sink + .drain() — worked example produces correct out.csv; stream.test.lin green; ASan over drain.
5. unify sources + .for(fn) + must-use warning — per-source integration tests + example fixture. ← MERGEABLE SYNC-CORE CHECKPOINT
6. affine use-after-move check — positive/negative type-check tests; placement restriction enforced.
7. CAP_MOVE across ABI — ASan: moved stream's fd closes once, by worker.
8. .promise() true-threaded + fault isolation — concurrent pipelines + fault-injection test.

Write progress to STREAMS_PROGRESS.md as each stage completes (stage, commit sha, gate result).
```
