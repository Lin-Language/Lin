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
| for        | terminal        | ✅ Null             | ✅ StreamFor (Null|Error)| DONE (Stage 5 pre-existing) |
| map        | lazy adapter    | ✅ U[]              | StreamMap (Stream<U>)   | TYPED (S2, intrinsic); backend lazy-pending (S3) |
| filter     | lazy adapter    | ✅ T[]              | StreamFilter            | TYPED (S2, intrinsic); backend lazy-pending (S3) |
| take       | lazy adapter    | ✅ T[]              | StreamTake              | lazy-pending (PURE-LIN; needs S3 backend) |
| drop       | lazy adapter    | ✅ T[]              | StreamDrop (NEW)        | lazy-pending (PURE-LIN; needs S3 backend) |
| flatMap    | lazy adapter    | ✅ U[]              | StreamFlatMap (NEW)     | lazy-pending (PURE-LIN; needs S3 backend) |
| takeWhile  | lazy adapter    | ✅ T[]              | StreamTakeWhile (NEW)   | lazy-pending (PURE-LIN; needs S3 backend) |
| dropWhile  | lazy adapter    | ✅ T[]              | StreamDropWhile (NEW)   | lazy-pending (PURE-LIN; needs S3 backend) |
| flatten    | lazy adapter    | ✅ flat             | StreamFlatten (NEW)     | lazy-pending (PURE-LIN; needs S3 backend) |
| concat     | lazy adapter    | ✅ array            | StreamConcat (NEW)      | lazy-pending (PURE-LIN; needs S3 backend) |
| reduce     | terminal        | ✅ U                | StreamReduce (NEW, U|Error) | TYPED (S2, intrinsic, U|Error); backend lazy-pending (S3) |
| while      | terminal-ish    | ✅ Null             | StreamWhile (NEW)       | TYPED (S2, intrinsic, Null|Error); backend lazy-pending (S3) |
| find       | terminal        | ✅ T|Null           | StreamFind (NEW, T|Null|Error) | lazy-pending (PURE-LIN; needs S3 backend) |
| some       | terminal        | ✅ Boolean          | StreamSome (NEW, Boolean|Error) | lazy-pending (PURE-LIN; needs S3 backend) |
| every      | terminal        | ✅ Boolean          | StreamEvery (NEW, Boolean|Error) | lazy-pending (PURE-LIN; needs S3 backend) |
| range      | constructor     | ✅ Iterator         | n/a (produces)          | n/a           |
| rangeStep  | constructor     | ✅                  | n/a                     | n/a           |
| iter       | constructor     | ✅ Iterator         | n/a                     | n/a           |
| iterOf     | constructor     | ✅ Iterator         | n/a                     | n/a           |

NOTE: lin_stream_map/filter/take/lines/chunks ALREADY exist (Stage 4 of streams). So map/
filter/take stream backends are mostly wiring the unified name to the existing intrinsic;
drop/flatMap/takeWhile/dropWhile/flatten/concat/reduce/while/find/some/every are NET-NEW
stream backends.

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
3. Stream branches for lazy adapters — PENDING
4. New stream terminals (reduce/find/some/every) + while — PENDING
5. Re-key affine consume-set off dispatch; delete name allowlist; 5 attacks as tests — PENDING
6. Flat-producer recognition + docs (ADR-075) + final sweep — PENDING
