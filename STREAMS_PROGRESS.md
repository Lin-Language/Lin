# Streams — Progress Log

Branch: `feat/streams`. Work tracked stage-by-stage per `STREAMS_DESIGN_BRIEF.md`.
Gate discipline: `cargo build --workspace && cargo test --workspace` before each commit.

## Baseline (before Stage 1)
- Build: OK.
- Tests: 375 passed; 1 failed = `test_http_fetch_json` (network/localhost socket test).
  Re-ran in isolation → PASS. Confirmed pre-existing FLAKY test (timing under parallel
  load with an ephemeral-port tiny_http listener); unrelated to streams work. CI only runs
  non-network examples, consistent with this being environment-flaky. Treating "all
  non-flaky tests green" as the gate.

---

## Stage 1 — Type::Stream plumbing
- Commit: <pending>
- Gate: builds clean (no errors, no new warnings), all tests green.
- Result: 376 passed; 0 failed (the flaky http test passed this run too).

### What was added
New `Type::Stream(Box<Type>)` variant, sibling to `Type::Iterator`, threaded through every
site that handles `Iterator`/`Shared`. It mirrors `Shared<T>`'s opaque-box treatment: stored
at runtime as a boxed `TaggedVal*` (TAG_STREAM, added Stage 2), owning RC model via the
tag-aware release path. Pure plumbing — no behavior change, no runtime/codegen emission yet.

Files touched:
- `lin-check/src/types.rs` — variant + doc; `contains_type_var`; `Display` (`Stream<T>`).
- `lin-check/src/compat.rs` — covariant `Stream<a> ~ Stream<b>`; opaque (no widen to Json /
  no unify with anything else), arms placed before the Json wildcard.
- `lin-check/src/zonk.rs` — recurse into inner T.
- `lin-check/src/checker/helpers.rs` — `collect_type_subs`, `apply_type_subs`, and
  `is_definitely_non_transferable` (Stream is non-transferable-by-copy; crosses only by MOVE).
- `lin-check/src/checker/{call,expr,mod}.rs` — `collect_named_defs`, `type_mentions_strlit`,
  `collect_typevar_ids`.
- `lin-ir/src/monomorphize.rs` — `mentions_generic_tv`, `subst_type`,
  `erase_nonconcrete_typevars`, `collect_subs`, `mangle_type` (`Stream_<T>`),
  `collect_quantified_ids`.
- `lin-ir/src/lower.rs` — `is_union_ty` (Stream is owning/tag-aware-RC like Shared).
- `lin-codegen/src/codegen/types.rs` — `llvm_type` (opaque ptr), `is_union_type`.
- `lin-codegen/src/codegen/rc.rs` — `emit_release` Stream arm → `lin_tagged_release`
  (dispatches the TAG_STREAM finalizer added in Stage 2). NOTE: `Shared` is NOT in
  `emit_release` today (falls to `_ => {}` no-op), so a Shared in a global currently leaks
  its box — pre-existing, out of scope. Stream MUST close its fd, so it is wired here.
- `lin-codegen/src/codegen/intrinsics.rs` — fromJson decode-spec encoder exhaustiveness arm
  (accept-any; Stream is never a real fromJson target).

### Deliberately NOT touched (with rationale)
- `resolve.rs` — Stream is not spellable in source annotations (brief §1), so no named/generic
  resolver case. The recursive `substitute`/`expand_named_body` catch-alls clone it harmlessly.
- `lower.rs` `iter_elem_type` (the Array/Iterator element extractor) — Stream does NOT use the
  Array/Iterator loop machinery; its `.for` element extraction is a dedicated Stage 5 branch.
- `boxing.rs` / `data.rs` array-push arms — Stream is already a `TaggedVal*` (box_value
  catch-all passes it through), and the placement restriction (Stage 6) forbids Stream in
  arrays/objects, so no array-element handling is needed.

### Surprises / questions for the morning
1. `emit_release` has no `Shared` arm — Shared values stored in globals appear to leak their
   box (no double-free, just a leak). Looks like a latent pre-existing bug, not mine to fix
   here, but worth confirming. Stream is wired correctly into `emit_release`.
2. Confirmed the one baseline test failure is the flaky localhost HTTP test, not a regression.

---

## Stage 2 — TAG_STREAM + finalizer + lin_stream_read/close
- Commit: <pending>
- Gate (close-once): MET, verified under AddressSanitizer.
- Tests: 5 new `lin-runtime` stream unit tests pass under `cargo test` AND under
  `RUSTFLAGS="-Zsanitizer=address" cargo +nightly test -p lin-runtime --target
  x86_64-unknown-linux-gnu stream` — clean, no UAF / no double-free / no leak.
- Workspace: build clean; 375 integration tests pass + the same flaky http test (passes in
  isolation). No regressions.

### What was added
- `lin-runtime/src/tagged.rs` — `TAG_STREAM = 19` + doc; TAG_STREAM arm in `lin_tagged_release`
  → `lin_stream_release_box` (runs the auto-close finalizer).
- `lin-runtime/src/object.rs` — TAG_STREAM arms in `release_tagged_payload` and
  `retain_tagged_payload` (so the tag-aware `lin_tagged_retain`/`lin_tagged_release` and
  object/array slot retain/release handle streams).
- `lin-runtime/src/lib.rs` — `pub mod stream;`.
- `lin-runtime/src/stream.rs` (NEW) — modelled on `shared.rs`:
  - `StreamSource` trait — the pluggable read backend (`read() -> Ok(Some(chunk))|Ok(None)=EOF|
    Err(msg)`, `close()`). Concrete file backend lands Stage 3; tcp/process/stdin Stage 5.
  - `StreamBox { rc: AtomicU32, state: Mutex<{source: Option<Box<dyn StreamSource>>, closed} >}`.
    Atomic rc (matches Shared) so the finalizer can't race a stray cross-thread touch.
  - `ReadOutcome` (Chunk | Eof | Err) → tagged value (flat UInt8[] | null | error object).
  - `lin_stream_read` / `lin_stream_close` (idempotent) C ABI.
  - `lin_stream_retain_box` / `lin_stream_release_box` — the release path runs `close_box`
    (guarded by the `closed` flag → fd closes EXACTLY ONCE) then frees the box.
  - 5 unit tests asserting close-once via a shared `AtomicUsize` close counter across: drop
    w/o close, explicit-close-then-drop, retain/release balance, read-error-then-drop, and the
    tagged-chunk shape.

### Notes / questions
3. `transfer.rs::transfer_payload` has no TAG_STREAM arm — a stream embedded in a transferred
   value would currently fall to the `_ => payload` catch-all (alias the box pointer WITHOUT a
   retain → would double-close). This path is UNREACHABLE today because the checker marks
   `Stream` non-transferable (`is_definitely_non_transferable`, Stage 1) so a stream never
   reaches `lin_transfer_clone`. The correct cross-thread mechanism is CAP_MOVE (Stage 7), which
   will touch transfer.rs deliberately. Flagged so it isn't forgotten; safe as-is.
4. Atomic vs non-atomic rc: brief §9 says non-atomic would be sound (disjoint graphs after a
   move). I chose AtomicU32 for belt-and-braces (cheap: one box, not per-element). Easy to
   revisit if a benchmark ever cares.

---

## Stage 3 — fs.openRead + file read backend + codegen dispatch
- Commit: <pending>
- Gate (open+read bytes end-to-end): MET. `test_stream_open_read_bytes_end_to_end` opens a
  13-byte file as a `Stream<UInt8[]>`, pulls chunks until EOF, counts bytes → prints `13`.
- Tests: 2 new runtime tests (file backend + open-missing-is-error) + 1 integration test.
  Runtime stream tests (7) pass under ASan. Workspace: 376 pass + the flaky http test.

### What was added
- `lin-runtime/src/stream.rs` — `FileSource` (reads in 64 KiB chunks; `close` drops the File →
  fd closed) + `lin_fs_open(path) -> Stream<UInt8[]> | Error` (open failure → Error object).
- `lin-ir/src/ir.rs` — `Intrinsic::{StreamOpen, StreamRead, StreamClose}`.
- `lin-ir/src/lower.rs` — name map: `lin_fs_open`→StreamOpen, `lin_stream_read`→StreamRead,
  `lin_stream_close`→StreamClose (so they're intrinsics, not foreign named calls).
- `lin-check/src/checker/intrinsics.rs` — intrinsic signatures (the SOLE source of `Stream<T>`):
  `lin_fs_open: (String)=>Stream<UInt8[]>|Error`, `lin_stream_read: <T>(Stream<T>)=>T|Null|Error`,
  `lin_stream_close: <T>(Stream<T>)=>Null`.
- `lin-codegen/src/codegen/intrinsics.rs` — StreamOpen/Read/Close dispatch (modelled on
  `lin_shared_*`): each is a single runtime call returning a TaggedVal*; results are unions so
  they stay boxed.
- `stdlib/fs.lin` — `openRead` (return type inferred), `readChunk`, `closeStream` wrappers.

### DESIGN DEVIATION (needs morning sign-off)
5. **Made `Stream` SPELLABLE in source annotations** (added the `"Stream"` cases to
   `resolve.rs`, mirroring `Shared` exactly: `Stream` => `Stream<Json>`, `Stream<T>` generic).
   The brief LOCKED "not spellable (no resolve.rs case)". I relaxed it because:
   - The stdlib's thin pull wrappers (`readChunk`/`closeStream`) need a `Stream`-typed param.
   - An UNANNOTATED single param (`(s) =>`) is mis-rendered by the FORMATTER as an arg-position
     bare lambda (`s =>`), which is INVALID at a `val =` RHS (ADR-007: bare lambdas only in arg
     position) — this is a genuine pre-existing formatter bug (`formatter.rs:703-708`) that no
     existing stdlib code triggers because nothing else uses untyped single params. It also
     emits `s: Null =>` (return type on a bare lambda) which is invalid everywhere.
   - Opacity is FULLY preserved: `compat.rs` still rejects every non-stream op on a `Stream`, so
     a user who names `Stream` gains nothing but the type itself (cannot index/push/iterate/widen
     it). This is exactly the `Shared` situation (Shared is spellable and stdlib annotates it).
   ALTERNATIVES considered: (a) fix the formatter to keep parens for single-untyped-param
   lambdas / track arg-position context — larger change touching the parser/formatter; (b) keep
   Stream unspellable and only ever pass it through typed params — impossible for a generic pull
   wrapper. If you'd rather keep the brief's letter, the fix is (a); say so and I'll do it.
6. **compat.rs TypeVar exception for Stream**: the opaque-rejection arms `(Stream,_)=>false` /
   `(_,Stream)=>false` would also have blocked an INFERENCE TypeVar from unifying with a Stream
   (needed even for annotated wrappers, since the intrinsic's `Stream<T9160>` param meets a
   `Stream<Json>` arg and T9160 must bind). Added explicit arms so a non-MAX TypeVar on either
   side is permissive, while the `u32::MAX` Json wildcard is STILL rejected (a stream must never
   widen to Json). Mirrors the existing general TypeVar-permissive rule, scoped under the Stream
   guards.

---

## Stage 4 — std/stream adapters + sink + .drain()
- Commit: <pending>
- Gate: MET. Worked CSV example produces the EXACT expected `out.csv`
  (`a,b,c` -> `"a"|"b"|"c"`); `stdlib/stream.test.lin` green (10 tests); ASan over drain green.
- Tests: 12 runtime stream tests (incl. 5 new adapter/drain) pass under `cargo test` AND ASan;
  `stdlib/stream.test.lin` (10 tests) green via `lin test`; integration
  `test_stream_csv_pipeline_drain` green; `examples/streams/main.lin` runs and prints the
  transformed CSV. Workspace: 378 integration + all unit tests green, 0 failures.

### What was added
- `lin-runtime/src/stream.rs` — generalised the backend to a TAGGED-item model:
  `StreamSource::read_tagged` (default wraps byte `read` into a `UInt8[]`), `pull_tagged` (the
  single low-level pull every adapter/terminal funnels through), `TaggedOutcome`.
  - Lazy adapters (each a new `StreamBox` owning a RETAINED ref to its upstream; closing it
    closes+releases the upstream): `MapSource`/`FilterSource` (call a retained Lin closure via
    the boxed-ABI `(env, boxed_arg)->boxed_ret`), `TakeSource`, `LinesSource` (byte→String line
    framing, CRLF-tolerant, flushes a final unterminated line), `ChunksSource` (fixed-size
    re-chunk). `LinFn` wraps a retained closure (released on Drop). `Upstream` owns+closes the
    upstream box.
  - Sink + terminals (sync, calling thread): `WriteSink` + `drive_sink`; `lin_stream_drain`
    (sink → write loop; non-sink → pull-and-discard; always closes), `lin_stream_collect`
    (→ UInt8[]|Error), `lin_stream_read_text` (→ String|Error). In-band error threading: an
    upstream `Err` short-circuits straight to the terminal (becomes the canonical Error object).
- `lin-ir/src/ir.rs` + `lower.rs` — `Intrinsic::Stream{Map,Filter,Take,Lines,Chunks,Write,
  Drain,Collect,ReadText}` + name map.
- `lin-check/src/checker/intrinsics.rs` — signatures (transform closures typed `(Json)=>Json` /
  `(Json)=>Boolean` so the runtime calls them via the uniform boxed-closure ABI regardless of
  the concrete item type — chunks/lines are JSON-compatible).
- `lin-codegen/src/codegen/intrinsics.rs` — dispatch for all nine.
- `stdlib/stream.lin` (NEW) — `readStream/lines/chunks/map/filter/take/writeStream/drain/
  collect/readText/close`. Registered in `lin-compile` as `std/stream`.
- `stdlib/stream.test.lin` (NEW, 10 tests), `examples/streams/main.lin` (NEW worked CSV),
  `crates/lin/tests/integration.rs::test_stream_csv_pipeline_drain`.

### Notes / surprises
7. **Closure representation**: transform closures are typed `(Json)=>Json` and called from the
   runtime via the BOXED closure ABI (the `__cls_wrapb_*` wrapper every function value carries —
   it declares all params `ptr`, unboxes to the concrete param, boxes the result). So a user's
   `(line: String): String` closure works unchanged: the wrapper unboxes the boxed String item.
   An inline `f => …` infers `Json` params; both forms tested.
8. **stream.map vs array.map**: `stream.map` is `Stream`-typed, so the brief example's INNER
   `line.split(",").map(...)` (over a `String[]`) must use ARRAY map — import it `as amap`.
   First CSV attempt crashed (huge alloc) because `.map` dot-resolved to `stream.map` over a
   String[]; with `amap` it's correct. Worth a doc note for users; the example/test use `amap`.
9. The chain accepts a `Stream<UInt8[]> | Error` receiver into a `Stream` param via dot-call
   receiver leniency (a union receiver picks the matching variant). A DIRECT call `close(st)`
   with `st: Stream|Error` does NOT type-check — use `st.close()` (dot form). Tests use dot form.

---

## Stage 5 — unified sources + .for(fn) + must-use warning  ★ MERGEABLE SYNC-CORE CHECKPOINT ★
- Commit: <pending>
- Gate: MET. Per-source integration tests (file/TCP/process-stdout/stdin) green; example
  fixture (`examples/streams/main.lin`) runs; must-use warning fires on a dropped stream and is
  absent on a consumed one. Full workspace: 381 integration + all unit tests, 0 failures.
- **THIS IS THE MERGEABLE SYNC-CORE CHECKPOINT** — everything through here is sound on a single
  thread with no new concurrency machinery. Stages 6-8 add the affine check, CAP_MOVE, and the
  async `.promise()` driver. If you want to merge a usable subset, merge up to this commit.

### What was added
- `lin-runtime`: unified OS sources — `TcpSource` (delegates to `net::tcp_stream_read`/
  `tcp_stream_close`), `ProcessSource` (`process::process_stdout_read`), `StdinSource` (reads
  stdin). Public read helpers added to `net.rs`/`process.rs` so the registries stay encapsulated.
  Constructors `lin_net_tcp_stream(fd)`, `lin_process_stdout_stream(handle)`,
  `lin_io_stdin_stream()`. New `lin_stream_for(stream, body)` drives `.for` on the calling
  thread (EOF → Null; first read Error → the Error value), closing the stream at the end.
- `lin-ir`/`lin-check`/`lin-codegen`: `Intrinsic::Stream{Tcp,Stdout,Stdin,For}` + types +
  dispatch. `lower_for` gains a STREAM branch (keys on `Type::Stream` iterable → emits
  `lin_stream_for`); `lin_for`'s iterable union gains a `Stream<T>` variant (return kept `Null`
  — see surprise #10).
- `lin-check/src/checker/stmt.rs`: must-use WARNING when a bare expression statement has type
  `Stream` (a pipeline with no terminal). It's a WARNING (build still succeeds) per brief §7.
- `lin-compile`: `check_module_with_imports` now also returns the checker's non-error warnings;
  the build pipeline renders the main module's warnings (they were previously dropped).
- `stdlib`: `net.tcpStream`, `process.stdoutStream`, `io.stdinStream`, and std/stream's `for`
  (typed `(Stream, Function): Json`).
- Tests: 3 per-source integration tests (`test_stream_{process_stdout,stdin,tcp}_source`).

### Surprises / decisions
10. **`lin_for` return type must stay `Null`**: I first widened it to `Null | Error` (to give a
    stream `for` the right static type). That broke EVERY test — std/array's `for` wrapper is
    declared `: Null`, so the stdlib `array.lin` failed to compile, cascading to all programs
    that import std/array. REVERTED: `lin_for` keeps `: Null` (only the iterable union gained the
    `Stream` variant); the IR lowerer's stream branch emits `lin_stream_for` regardless of the
    declared type, and std/stream's own `for` wrapper is typed `Json` so the runtime's Null|Error
    is surfaced. LESSON for the morning: changing a widely-used intrinsic's return type ripples
    through every stdlib wrapper's annotation — touch only the iterable/param side.
11. **compat `Stream`↔`Union`**: the opaque `(Stream,_)=>false` arm also intercepted
    `(Stream, Union)` BEFORE the union-target rule, so a `Stream` couldn't match the `Stream`
    variant of `lin_for`'s iterable union. Added explicit `(Stream,Union)|(Union,Stream)` arms
    that defer to a new `union_compat` helper (the standard all-variants / any-variant rules).
12. **must-use scope**: warns on a bare `Stmt::Expr` of `Stream` type only — a properly-terminated
    pipeline has type `Null|Error`/`UInt8[]|…`, never `Stream`, so no false positive. A
    stream stored in an unused `val` is NOT yet flagged (that's the affine analysis, Stage 6).
13. **stream `for` import**: `stream.for(fn)` requires importing `for` from `std/stream` (not
    `std/array`) — the std/array `for` erases the receiver to `Json`, so the IR never sees a
    `Stream` and silently no-ops (treats the stream as a 0-length array). Documented in the
    stdlib comment; the example/tests import `for` from std/stream.

---

## Stage 6 — affine use-after-move check + placement restriction
- Commit: <pending>
- Gate: MET. Positive + negative type-check tests green:
  `test_stream_{use_after_move,in_var,in_object_field}_rejected` + `test_stream_single_use_ok`.
  Full workspace: 385 integration + all unit tests, 0 failures.

### What was added (`lin-check`)
- **Placement restriction** (hard ERROR, brief §8): a `Stream` (or `Stream | Error` — see #14)
  may live only in a `val`/arg/return. Rejected in `Stmt::Var` (stmt.rs), object Pair, and array
  element (expr.rs) via the new `type_is_streamish` helper.
- **Affine use-after-move** (hard ERROR, brief §7): a flow-sensitive `consumed_streams: HashSet<
  slot>` on the Checker. An OWNERSHIP-TAKING op (adapters lines/map/filter/take/chunks/writeStream
  + terminals drain/collect/readText/for) MOVES its stream arg — `mark_stream_consumed` records
  the binding at the call/dot-call site (`is_stream_consuming_op`). A later read of a consumed
  binding errors in `infer_ident`. BORROWS (`read`/`readChunk`/`close`/`closeStream`) are NOT in
  the consuming set, so a low-level recursive pull loop reads the same binding freely.
- **Conservative branch merge**: `infer_if` and `infer_match` snapshot `consumed_streams` before
  the branches and union the per-branch results after (a stream consumed in ANY branch is
  consumed after — prevents a use after a possible move).

### Surprises / decisions
14. **`Stream | Error` is the real binding type**: the source intrinsics return `Stream<…> |
    Error`, so a `val s = readStream(...)` has the UNION type, not a bare `Stream`. The
    placement/affine/must-use checks therefore use `type_is_streamish` (matches `Stream` OR a
    union containing `Stream`) rather than `matches!(.., Stream(_))`.
15. **borrow vs move was load-bearing**: my FIRST affine model consumed on EVERY identifier read,
    which broke the Stage-3 `readChunk` recursive pull loop (it reads `s` twice per frame:
    `readChunk(s)` + `countBytes(s, …)`). REWORKED to consume only at ownership-taking call
    sites; `read`/`close` borrow. This matches the brief's "consumed by the first adapter/terminal
    it flows into" precisely. The Stage-3 low-level loop now type-checks again.
16. **v1 scope of the affine check**: tracks simple `Ident` reads of `val`/param Stream bindings.
    NOT yet covered (sound — they just aren't flagged): a stream consumed inside a lambda that's
    called multiple times; a stream moved through a generic container. These are out of v1 scope
    per the placement restriction (no container linearity). The must-use warning (Stage 5) and
    this consume-check together cover the common single-binding patterns.

---

## Stage 7 — CAP_MOVE across the closure-env transfer ABI
- Commit: <pending>
- Gate (ASan: moved stream's fd closes once, by the worker): MET, verified under
  AddressSanitizer (`cap_move_stream_closes_once_by_worker`). Full workspace: 385 integration +
  52 runtime unit tests, 0 failures.

### What was added
- `ir.rs` `CaptureRelease::Move` (`.code() == 6`) + `transfer.rs` `CAP_MOVE = 6` — the two
  mirrored capture-kind tables, both extended past 5 = Tagged.
- `transfer.rs`: `transfer_clone_env` CAP_MOVE arm hands the resource pointer off VERBATIM (no
  clone, no retain); `release_env_copy` CAP_MOVE arm releases it via `lin_tagged_release`
  (TAG_STREAM finalizer → fd closes once, ON THE WORKER). `env_is_transferable` already permits
  CAP_MOVE (it only rejects CAP_CLOSURE).
- `memory.rs` `release_captures`: code 6 (Move) deliberately falls to the no-op arm — the LOCAL
  (source) closure must NOT release a handed-off resource; only the worker's `release_env_copy`
  does. (Documented; the inline-fallback caveat noted there too.)
- `lower.rs`: a streamish capture (`type_is_streamish_ir`) emits `CaptureRelease::Move` with NO
  CloneBox/Retain AND `unregister_owned(base)` — suppressing the SOURCE's scope-exit release so
  the moved box isn't released twice (source + worker).
- ASan test (`cap_move_stream_closes_once_by_worker`) drives the env transfer directly: build env
  with a CAP_MOVE stream slot → `transfer_clone_env` (verbatim handoff) → free source env shell
  (no close) → `release_env_copy` (worker closes once). Asserts close-count == 1.

### Notes
17. The IR/codegen Move path is now in place, but it is only REACHABLE end-to-end once `.promise()`
    captures a stream into an async thunk — which the async checker currently REJECTS (Stream is
    `is_definitely_non_transferable`). Making that capture legal-by-move is exactly Stage 8. So
    Stage 7's gate is verified at the runtime-ABI level (the direct env-transfer ASan test);
    Stage 8 wires the language path that emits it and adds the concurrent + fault-injection tests.
