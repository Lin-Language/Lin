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
