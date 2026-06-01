# std/iter unification — progress + lazy-pending tracker

Branch: feat/streams. Plan: STD_ITER_DESIGN.md (docs/streams). 6 staged steps, each gated
(build + `cargo test --workspace` + `lin test stdlib/` green). ASan on Stages 4/5.

## Module boundary (LOCKED)
**std/iter** (iterable combinators + iterator constructors — moved out of std/array):
  map, filter, reduce, for, while, take, drop, find, some, every, flatMap, takeWhile,
  dropWhile, flatten, concat, range, rangeStep, iter, iterOf
**std/array** (array-shaped — stay):
  push, slice, set, at, length, reverse, sort, sortBy, zip, unique, chunk, compact, indexOf,
  partition, sum, product, min, max, minBy, maxBy, append, prepend, scan, groupBy, countBy,
  arrayAllocate, arrayAllocateFilled
Combinators live in EXACTLY ONE place (std/iter) — NOT dual-exported from std/array.

## Lazy-pending tracker (which std/iter fns still need a stream-lazy backend)
After Stage 1 (eager relocation only), every combinator works on Array/Iterator but NOT yet on
Stream. This table tracks the stream backend status per fn. Update as Stages 3-4 land.

| fn         | kind            | Array/Iter (eager) | Stream backend          | status        |
|------------|-----------------|--------------------|-------------------------|---------------|
| for        | terminal        | ✅ Null             | ✅ StreamFor (Null|Error)| DONE (Stage 5 pre-existing; now the SINGLE `for` — removed from std/stream) |
| map        | lazy adapter    | ✅ U[]              | ✅ Intrinsic::StreamMap (`lin_stream_map`) | DONE (S3) |
| filter     | lazy adapter    | ✅ T[]              | ✅ Intrinsic::StreamFilter (`lin_stream_filter`) | DONE (S3) |
| take       | lazy adapter    | ✅ T[]              | ✅ Intrinsic::StreamTake (`lin_stream_take`) | DONE (S3) |
| drop       | lazy adapter    | ✅ T[]              | ✅ Intrinsic::StreamDrop (`lin_stream_drop`) | DONE (S3) |
| flatMap    | lazy adapter    | ✅ U[]              | ✅ Intrinsic::StreamFlatMap (`lin_stream_flat_map`) | DONE (S3) |
| takeWhile  | lazy adapter    | ✅ T[]              | ✅ Intrinsic::StreamTakeWhile (`lin_stream_take_while`) | DONE (S3) |
| dropWhile  | lazy adapter    | ✅ T[]              | ✅ Intrinsic::StreamDropWhile (`lin_stream_drop_while`) | DONE (S3) |
| flatten    | lazy adapter    | ✅ flat             | ✅ Intrinsic::StreamFlatten (`lin_stream_flatten`) | DONE (S3) |
| concat     | lazy adapter    | ✅ array            | ✅ Intrinsic::StreamConcat (`lin_stream_concat`, TWO streams) | DONE (S3) |
| reduce     | terminal        | ✅ U                | ✅ Intrinsic::StreamReduce (`lin_stream_reduce`, U|Error) | DONE (S4) |
| while      | terminal-ish    | ✅ Null             | ✅ Intrinsic::StreamWhile (`lin_stream_while`, Null|Error) | DONE (S4) |
| find       | terminal        | ✅ T|Null           | ✅ Intrinsic::StreamFind (`lin_stream_find`, T|Null|Error) | DONE (S4) |
| some       | terminal        | ✅ Boolean          | ✅ Intrinsic::StreamSome (`lin_stream_some`, Boolean|Error) | DONE (S4) |
| every      | terminal        | ✅ Boolean          | ✅ Intrinsic::StreamEvery (`lin_stream_every`, Boolean|Error) | DONE (S4) |
| range      | constructor     | ✅ Iterator         | n/a (produces)          | n/a           |
| rangeStep  | constructor     | ✅                  | n/a                     | n/a           |
| iter       | constructor     | ✅ Iterator         | n/a                     | n/a           |
| iterOf     | constructor     | ✅ Iterator         | n/a                     | n/a           |

ALL 14 stream-backend combinators DONE (Stages 3+4). The net-new backends added in stream.rs:
drop/takeWhile/dropWhile/flatMap/flatten/concat (adapters) + reduce/find/some/every/while
(terminals). map/filter/take/for re-used the pre-existing intrinsics.

## Stage status
1. Create std/iter, relocate eager combinators, migrate imports — DONE (see git log; this entry
   committed in the same relocation commit on feat/streams)
   - New module `stdlib/iter.lin` (`std/iter`): map, filter, reduce, for, while, take, drop, find,
     some, every, flatMap, takeWhile, dropWhile, flatten, concat, range, rangeStep, iter, iterOf —
     bodies verbatim from std/array, eager, unchanged. iter.lin is the LOWER-LEVEL module: it does
     NOT import from std/array (that would form an array->iter->array cycle, a hard compile error).
     The two array primitives the combinators use internally (`length`, `push`) are duplicated in
     iter.lin as PRIVATE (non-exported) byte-identical lin_length/lin_push wrappers, keeping the
     moved bodies verbatim and the module self-contained.
   - CYCLE RESOLUTION: array.lin (higher-level) imports `{ for, take, concat } from "std/iter"` for
     its staying functions (reverse/partition/zip/unique/chunk/sort/scan/groupBy/countBy). iter.lin
     never imports array. No cycle.
   - Registered `std/iter` in `crates/lin-compile/src/lib.rs` (stdlib_source match) and mirrored in
     `crates/lin-lsp/src/main.rs` (stdlib_source + completion table re-homed moved names to category
     "iter").
   - Migrated 50 stdlib/examples .lin files + the embedded Lin source strings in
     `crates/lin/tests/integration.rs` (split MOVE names to std/iter, kept STAY on std/array,
     preserved `as` aliases, dropped now-empty std/array import lines).
   - FLAT-PRODUCER LISTS (lin-ir/src/lower.rs): NO CHANGE NEEDED. `is_flat_producer_name` matches
     `lin_*` intrinsic names (lin_range/lin_map/lin_filter/lin_array_allocate*) — these are unchanged
     in Stage 1 (only the stdlib WRAPPER moved, it still calls lin_map etc.). `is_flat_producer_export`
     matches the trailing wrapper name (range/map/filter/arrayAllocate/arrayAllocateFilled) regardless
     of module, so the relocation is transparent to it. Verified: both lists untouched.
   - GATE: cargo test --workspace = 392 integration + all crate suites green (test_http_fetch_json
     passed in this run); `lin test stdlib/` = 22 files pass; `lin test examples/` = 42 files pass;
     `lin check` over all 13 example main.lin + 42 example source .lin = zero failures. ZERO behaviour
     change confirmed.
   - VERIFIED BY ME (clean build, stale cache cleared): cargo test 502 passed / 0 failed;
     lin test stdlib/ = 22 files; lin test examples/ = 42 files; worked stream example builds+runs.
   - KNOWN MINOR WART (accepted, not a blocker): iter.lin duplicates `length`/`push` as private
     thin wrappers over lin_length/lin_push to break the array↔iter cycle. They're one-line
     forwarders over compiler builtins — can't drift in behaviour. Alternative (a 3rd lower
     module for two one-liners) would be over-engineering. Revisit only if more sharing is needed.
   - VERIFIED BY ME (clean build, cache cleared): cargo 506/0, stdlib 22, examples 42. By-hand:
     stream.map().filter().writeStream().drain() checks; [1,2,3].map().length() checks (array
     preserved); stream.reduce → Int|Error (match-narrow required). NEGATIVES reject correctly:
     length(stream.map(..)) → "Stream ≠ array"; stream.reduce result + without narrowing →
     rejected. Typing is discriminating, not permissive. Stray-fmt incident left NO residue
     (working tree clean, commit = exactly 5 intended files).
   - RECONCILIATION TODO for Stage 3: `take` is DOUBLE-IMPLEMENTED — std/stream exports a lazy
     `take` (lin_stream_take, already exists) AND std/iter has the eager array `take` (pure-Lin).
     This is exactly the dual-impl confusion we're eliminating. Stage 3: std/iter.take must
     dispatch to lin_stream_take for a stream receiver; std/stream must STOP exporting take (and
     likewise map/filter/lines/chunks that std/iter will own). The std/stream module should end
     up with only stream-SPECIFIC ops (readStream/writeStream/drain/collect/readText/promise/
     close/lines/linesMax/chunks) — the generic combinators (map/filter/take/reduce/…) come from
     std/iter via receiver dispatch. CONFIRM this consolidation in Stage 3.
2. Union params + receiver-dependent return typing (checker) — DONE (checker-only; no Stage-3 codegen)
   - INTRINSIC-BACKED combinators (typed this stage to accept a Stream receiver): `map`→lin_map,
     `filter`→lin_filter, `reduce`→lin_reduce, `while`→lin_while (`for`→lin_for was already done).
     Each intrinsic's iterable param union was extended `Array | Iterator` → `Array | Iterator |
     Stream` (crates/lin-check/src/checker/intrinsics.rs).
   - PURE-LIN combinators (loops over `for`/`while`/`push` in stdlib/iter.lin — NOT typed for
     streams this stage, they stay array/iterator-only and need a real Stage-3 stream backend):
     `find`, `some`, `every`, `flatMap`, `take`, `drop`, `takeWhile`, `dropWhile`, `flatten`. They
     build eagerly on `push`-to-array, so typing alone cannot make them lazy-over-stream; a stream
     passed to them remains a type error this stage (their wrapper params are unchanged `Json`).
   - INJECTION POINT for receiver-dependent return typing: `Checker::streamish_combinator_ret`
     (crates/lin-check/src/checker/call.rs), called as a post-processing step at the END of both
     `infer_call` and `infer_dot_call` (just before the `TypedExpr::Call` is built). It is keyed on
     the callee's IMPORT ORIGIN `(module_path, export_name)` via `self.import_origins` — so ONLY the
     genuine `std/iter` exports (`map`/`filter`/`reduce`/`while`) are re-typed; a user-defined
     same-named function is never affected. When arg0 is DEFINITELY a stream it rewrites the eager
     array-shaped result to: map/filter → `Stream<elem>`, reduce → `U | Error`, while → `Null |
     Error`. `for` needs no entry (its wrapper is always `Null`; std/stream's `for` handles the
     error arm).
   - CRITICAL "definitely a stream" distinction: the override uses a NEW stricter predicate
     `is_definitely_stream` (Stream, or a union whose only non-`Error` members are Streams), NOT the
     looser `type_is_streamish` (true for ANY union that merely includes a Stream). The mixed
     `Array | Iterator | Stream` union — both the stdlib wrapper's own param while its body is
     checked AND a user generic's `Iterable<T>` param — is streamish but NOT definitely-stream, so
     its eager ARRAY return is preserved. Using the loose check leaked `Stream<…>` into the array
     call sites of any generic over the union (caught empirically; fixed).
   - WRAPPER changes (stdlib/iter.lin): `map`/`filter`/`reduce`/`while` widened their iterable param
     to `T[] | Iterator | Stream` (a Stream does NOT flow into a bare `Json` param, so the union is
     required) and DROPPED their `: U[]`/`: T[]`/`: U`/`: Null` return annotation so the wrapper
     INFERS the array return from the intrinsic body; the call-site override then supplies the
     stream form. NOTE: `Iterator`/`Stream` are written WITHOUT a type argument (= `<Json>`): the
     array `T[]` arm carries the callback element-type inference (and the ADR-069 flat path), AND
     the source formatter cannot round-trip a parametric `Iterator<T>`/`Stream<T>` annotation
     (renders `Iterator<T>` → `Iterator[T]` → garbled, breaking the corpus idempotency test). Bare
     `Iterator | Stream` is formatter-idempotent. Array/iterator behaviour is UNCHANGED.
   - VERIFICATION #3 (generic-Iterable mixed call sites) — RESOLVED EMPIRICALLY: a user generic
     `<T>(xs: T[] | Iterator | Stream)` forwarding to `.map` has its OWN return monomorphized ONCE
     to the eager ARRAY shape (its param is not definitely-stream, so the override is suppressed
     inside the body — this is exactly what stops the stream return leaking into the generic's array
     call sites). Consequence/limitation: an ARRAY call site of such a generic correctly yields an
     array; a STREAM call site of the SAME generic ALSO yields the array shape (it does NOT become
     `Stream`). Receiver-dependent stream typing applies only at a DIRECT std/iter combinator call
     with a concrete stream receiver, not when routed through a user generic. This is the SAFE
     resolution (the HARD GATE — never regress arrays — takes priority); a user wanting stream
     passthrough calls the combinator directly or annotates `Stream` explicitly. Tested in
     `test_iter_generic_iterable_mixed_call_sites`.
   - NEW type-check tests (crates/lin/tests/integration.rs, `lin check`-level, no run):
     `test_iter_stream_map_yields_stream_not_array`, `test_iter_stream_reduce_and_while_widen_to_error`,
     `test_iter_array_map_still_yields_array_unchanged`, `test_iter_generic_iterable_mixed_call_sites`.
   - GATE (verified by me, clean build, caches cleared): `cargo test --workspace` = 506 passed / 0
     failed (was 502 + 4 new), incl. the formatter corpus idempotency test; `lin test stdlib/` = 22
     files; `lin test examples/` = 42 files; `lin check` over all example main.lin = zero failures.
     ZERO array/iterator behaviour change confirmed.
3. Stream branches for lazy adapters — DONE
   - NET-NEW runtime backends in `crates/lin-runtime/src/stream.rs`, each mirroring MapSource/
     TakeSource RC discipline EXACTLY (release pulled item after the closure consumes it; close
     upstream in close(); return independently-owned boxes; propagate Err in-band):
     DropSource, TakeWhileSource (done-latch), DropWhileSource (dropping-latch), FlatMapSource +
     FlattenSource (both via a shared `InnerCursor` holding ONE owned ref to the inner array +
     cursor; release on exhaust/close), ConcatSource (TWO retained upstreams, close BOTH; first
     stream closed eagerly on its EOF). Constructors: lin_stream_drop/_take_while/_drop_while/
     _flat_map/_flatten/_concat.
   - IR: `Intrinsic::Stream{Drop,TakeWhile,DropWhile,FlatMap,Flatten,Concat}` (ir.rs); name→intr
     in lower.rs; codegen dispatch in lin-codegen/src/codegen/intrinsics.rs; checker sigs in
     lin-check/src/checker/intrinsics.rs.
   - DISPATCH WIRING (the key Stage-3 mechanism): a genuine `std/iter` combinator called with a
     DEFINITELY-stream arg0 is redirected to the `lin_stream_*` backend at `lower_call`
     (`stream_combinator_intrinsic_name`, keyed on the `std_iter_` symbol prefix) — delegating to
     `lower_intrinsic_call` so stream-arg RC matches the proven std/stream wrapper path. To make
     the redirect reachable: (a) `try_inline_combinator_wrapper` (monomorphize.rs) BAILS when arg0
     is a Stream (so map/filter/reduce don't inline to the eager `lin_map` loop); (b) the generic
     specialization path is SKIPPED for a stream arg0 (so map/filter/reduce/while stay Named import
     calls). The checker's `streamish_combinator_ret` (call.rs) was EXTENDED to re-type the result
     of drop/take/flatMap/takeWhile/dropWhile/flatten/concat → `Stream<elem>`, find → T|Null|Error,
     some/every → Boolean|Error, for → Null|Error — so a chain stays typed Stream and each step
     re-dispatches. The pure-Lin wrapper PARAMS stay `Json` (a definite/narrowed Stream coerces to
     a Json param — verified), so NO wrapper body/param changes were needed for the pure-Lin set.
4. New stream terminals (reduce/find/some/every) + while — DONE
   - Runtime terminals in stream.rs: lin_stream_reduce (fold, releases prev acc each step, adopts
     closure result; init owned +1), lin_stream_find (returns the found item owned, or Null),
     lin_stream_some/_every (short-circuit, return a fresh Bool box), lin_stream_while (drive until
     predicate falsy or EOF → Null). All close the stream. reduce uses a NEW `LinFn::call2_caught`
     (3-arg env,acc,item ABI) added to stream.rs.
   - ASan: `RUSTFLAGS=-Zsanitizer=address ASAN_OPTIONS=detect_leaks=0 cargo +nightly test -p
     lin-runtime --target x86_64-unknown-linux-gnu stream` = 20/20 CLEAN, incl. 6 net-new focused
     unit tests (drop, concat two-upstream close-once, flatten, find, reduce, flatMap) asserting
     close-once via shared counters.
   - std/stream CONSOLIDATION: removed `map`, `filter`, `take`, `for` exports from stdlib/stream.lin
     (they now come from std/iter via receiver dispatch). std/stream keeps only stream-SPECIFIC
     ops: readStream/writeStream/drain/collect/readText/promise/close/lines/linesMax/chunks. The
     `for` ambiguity (std/iter AND std/stream both exporting `for`) is RESOLVED by making std/iter's
     `for` the single one (removed from std/stream); it dispatches to `lin_stream_for` on a stream
     receiver via the `lower_call` redirect (verified: `stdinStream().lines().for(...)` works).
   - Migrated imports: stdlib/stream.test.lin (+11 new combinator tests), examples/streams/main.lin,
     and crates/lin/tests/integration.rs (csv pipeline, promise fault-isolation, stdin source) now
     import map/filter/take/for from std/iter.
   - GATE (verified by me): cargo test --workspace = 402 integration + 60 lin-runtime (was 54) +
     37 type_check + … all green; test_http_fetch_json passes in isolation; `lin test stdlib/` = 22;
     `lin test examples/` = 42; 6 NEW end-to-end integration tests (drop/take/reduce, flatMap,
     takeWhile/dropWhile, concat, find/some/every, array non-regression) all pass.
5. Re-key affine consume-set off dispatch; delete name allowlist; 5 attacks as tests — DONE
   - THE BUG (confirmed reproduced before): the affine use-after-move check keyed consumption on a
     hardcoded NAME allowlist `is_stream_consuming_op` (call.rs) =
     lines|map|filter|take|chunks|writeStream|drain|collect|readText|for. Several ownership-taking
     ops were ABSENT, so the checker permitted a use-after-move that the IR's `move_streamish_arg`
     (lin-ir/src/lower.rs — moves ANY streamish arg) actually performed. Confirmed holes (all 3
     type-checked OK before, MUST reject): `linesMax`-then-reuse, `promise`-then-reuse (WORST:
     promise moves the pipeline onto a WORKER thread → cross-thread UAF), `close`-then-reuse.
   - THE FIX (re-keyed off the DISPATCH FACT, not a name list): deleted `is_stream_consuming_op`.
     Consumption now derives from the SAME machinery as the result re-typing and the IR redirect:
       * `Checker::callee_routes_to_stream_op(local_name)` resolves the callee's IMPORT ORIGIN via
         `self.import_origins` (so a user-defined same-named fn is NEVER affected) and returns true
         for a genuine `std/iter` stream combinator (`is_std_iter_stream_combinator` — the SAME set
         as lin-ir's `stream_combinator_intrinsic_name` and call.rs's `streamish_combinator_ret`:
         map/filter/take/drop/flatMap/takeWhile/dropWhile/flatten/concat/reduce/find/some/every/
         while/for) OR a genuine `std/stream` op (`is_std_stream_consuming_export`: lines/linesMax/
         chunks/writeStream/drain/collect/readText/close/promise).
       * `Checker::consume_definite_stream_args` then marks CONSUMED every argument whose inferred
         type `is_definitely_stream` — the affine MIRROR of `move_streamish_arg`. Applied
         per-argument at all three call sites (free `infer_call`, dot-call typed-method path,
         dot-call fallback path) AFTER args are inferred.
   - CURATED SET KEPT (not pure dispatch-derived) + WHY: the two `matches!` sets (std/iter combinators,
     std/stream exports) are tiny and live in ONE place in call.rs, each with a comment that they MUST
     stay in sync with `move_streamish_arg`. They exist only to GATE on "the callee is a stream op",
     avoiding consuming a stream handed to a hypothetical non-stream callee. In practice Stream is
     opaque (rejected everywhere else), so the gate is belt-and-braces; the actual move decision is
     the dispatch-derived per-argument `is_definitely_stream` test, which cannot diverge from the IR.
   - CONCAT (two stream args): handled by PER-ARGUMENT consumption — both `a` and `b` in
     `a.concat(b)` are definitely-stream, so BOTH are marked consumed (not arity-special-cased).
     Test reuses each arg independently; both rejected.
   - BORROW DECISION: there is NO `read` export (verified — std/stream exports only readStream/lines/
     linesMax/chunks/writeStream/drain/collect/readText/close/promise). `close` CONSUMES (it ends the
     stream — the box is released; reuse is a UAF). `promise` CONSUMES (moves onto a worker thread).
     So there are NO borrow ops among the exports — every std/stream/std/iter op taking a Stream
     consumes it. Simplest sound rule, exactly mirroring `move_streamish_arg`.
   - REGRESSION TESTS (crates/lin/tests/integration.rs, `lin check`-level), all 5 attacks now REJECT:
     `test_stream_affine_lines_then_reuse_rejected` (control — was already caught),
     `test_stream_affine_linesmax_then_reuse_rejected` (HOLE #1),
     `test_stream_affine_promise_then_reuse_rejected` (HOLE #2, cross-thread),
     `test_stream_affine_close_then_reuse_rejected` (HOLE #3),
     `test_stream_affine_concat_then_reuse_of_either_arg_rejected` (BOTH args). PLUS positive
     `test_stream_affine_single_use_chain_and_arrays_unaffected` (single-use chain passes; array/
     iterator combinator chains — incl. reusing an array across map/filter/reduce/concat — totally
     unaffected). Updated `test_iter_stream_reduce_and_while_widen_to_error` to use two separate
     streams (reduce+while on ONE stream is now correctly a use-after-move — the test was written
     under the old unsound allowlist where reduce/while weren't consuming).
   - GATE (verified by me, clean build): `cargo test --workspace` = 408 integration + 60 lin-runtime
     + 37 type_check + … ALL green (test_http_fetch_json passed in this run); `lin test stdlib/` = 22;
     `lin test examples/` = 42; all example `lin check` clean; examples/streams builds+runs (pipeline
     ok). ASan: `cargo +nightly test -p lin-runtime stream` = 20/20 CLEAN, no AddressSanitizer errors.
     Arrays/iterators COMPLETELY unaffected (only STREAM args are marked consumed).
6. Flat-producer recognition + docs (ADR-075) + final sweep — PENDING
6. Flat-producer recognition + docs (ADR-075) + final sweep — PENDING
