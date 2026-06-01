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
| map        | lazy adapter    | ✅ U[]              | StreamMap (Stream<U>)   | lazy-pending  |
| filter     | lazy adapter    | ✅ T[]              | StreamFilter            | lazy-pending  |
| take       | lazy adapter    | ✅ T[]              | StreamTake              | lazy-pending  |
| drop       | lazy adapter    | ✅ T[]              | StreamDrop (NEW)        | lazy-pending  |
| flatMap    | lazy adapter    | ✅ U[]              | StreamFlatMap (NEW)     | lazy-pending  |
| takeWhile  | lazy adapter    | ✅ T[]              | StreamTakeWhile (NEW)   | lazy-pending  |
| dropWhile  | lazy adapter    | ✅ T[]              | StreamDropWhile (NEW)   | lazy-pending  |
| flatten    | lazy adapter    | ✅ flat             | StreamFlatten (NEW)     | lazy-pending  |
| concat     | lazy adapter    | ✅ array            | StreamConcat (NEW)      | lazy-pending  |
| reduce     | terminal        | ✅ U                | StreamReduce (NEW, U|Error) | lazy-pending |
| while      | terminal-ish    | ✅ Null             | StreamWhile (NEW)       | lazy-pending  |
| find       | terminal        | ✅ T|Null           | StreamFind (NEW, T|Null|Error) | lazy-pending |
| some       | terminal        | ✅ Boolean          | StreamSome (NEW, Boolean|Error) | lazy-pending |
| every      | terminal        | ✅ Boolean          | StreamEvery (NEW, Boolean|Error) | lazy-pending |
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
2. Union params + receiver-dependent return typing (checker) — PENDING
3. Stream branches for lazy adapters — PENDING
4. New stream terminals (reduce/find/some/every) + while — PENDING
5. Re-key affine consume-set off dispatch; delete name allowlist; 5 attacks as tests — PENDING
6. Flat-producer recognition + docs (ADR-075) + final sweep — PENDING
