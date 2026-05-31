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
